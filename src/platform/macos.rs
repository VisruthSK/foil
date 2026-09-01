use super::{CommandSpec, Finished, Wait, combine_errors, drain_interrupt, reap_after_kill};
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    process::{Child, Command},
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

use rustix::buffer::spare_capacity;
use rustix::event::kqueue::{Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue};
use rustix::fs::{OFlags, fcntl_setfl};
use rustix::io::{Errno, FdFlags, fcntl_setfd, write};
use rustix::pipe::pipe;
use rustix::process::{Pid, Signal, kill_process_group};

#[derive(Clone)]
pub(crate) struct Interrupt {
    read: Arc<OwnedFd>,
    write: Arc<OwnedFd>,
}

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        let (read, write) = pipe()?;
        fcntl_setfl(&read, OFlags::NONBLOCK)?;
        fcntl_setfl(&write, OFlags::NONBLOCK)?;
        fcntl_setfd(&read, FdFlags::CLOEXEC)?;
        fcntl_setfd(&write, FdFlags::CLOEXEC)?;
        Ok(Self {
            read: Arc::new(read),
            write: Arc::new(write),
        })
    }

    pub(crate) fn signal(&self) {
        let _ = write(&*self.write, &[1u8]);
    }
}

pub(crate) struct Workload {
    child: Child,
    kqueue: OwnedFd,
    events: Vec<Event>,
    // Process groups do not contain descendants that call setsid() or setpgid().
    pgid: Pid,
    cleaned: bool,
}

pub(crate) struct Prepared {
    kqueue: OwnedFd,
    events: Vec<Event>,
    command: Command,
}

pub(crate) struct Session;

impl Session {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) fn prepare(&mut self, spec: &CommandSpec) -> io::Result<Prepared> {
        use std::os::unix::process::CommandExt;
        let mut command = spec.command();
        command.process_group(0);
        Ok(Prepared {
            kqueue: kqueue()?,
            events: Vec::with_capacity(2),
            command,
        })
    }

    pub(crate) fn shutdown(self) -> io::Result<()> {
        Ok(())
    }
}

impl Workload {
    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        let changes = [
            Event::new(
                EventFilter::Proc {
                    pid: Pid::from_child(&self.child),
                    flags: ProcessEvents::EXIT,
                },
                EventFlags::ADD | EventFlags::ONESHOT,
                ptr::null_mut(),
            ),
            Event::new(
                EventFilter::Read(interrupt.read.as_raw_fd()),
                EventFlags::ADD,
                ptr::null_mut(),
            ),
        ];

        loop {
            self.events.clear();
            // SAFETY: The child pid and interrupt fd stay valid for this Workload.
            match unsafe {
                kevent(
                    &self.kqueue,
                    &changes,
                    spare_capacity(&mut self.events),
                    Some(Duration::ZERO),
                )
            } {
                Ok(_) => break,
                Err(Errno::SRCH) => {
                    drain_interrupt(&interrupt.read)?;
                    return Ok(Wait::Exited);
                }
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(outcome) = ready(&self.events, interrupt)? {
            return Ok(outcome);
        }

        let started = Instant::now();
        loop {
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            self.events.clear();
            // SAFETY: No changelist entries, so no fd validity requirements to uphold.
            let n = unsafe {
                match kevent(
                    &self.kqueue,
                    &[],
                    spare_capacity(&mut self.events),
                    remaining,
                ) {
                    Ok(n) => n,
                    Err(Errno::INTR) => continue,
                    Err(e) => return Err(e.into()),
                }
            };
            if n == 0 {
                return Ok(Wait::TimedOut);
            }
            if let Some(outcome) = ready(&self.events, interrupt)? {
                return Ok(outcome);
            }
        }
    }

    pub(crate) fn finish(mut self) -> Finished {
        let (status, cleanup) = finish_process_group(&mut self.child, self.pgid);
        self.cleaned = cleanup.is_ok();
        Finished {
            status,
            peak_memory: None,
            cleanup,
        }
    }
}

impl Prepared {
    pub(crate) fn spawn(mut self) -> io::Result<Workload> {
        let child = self.command.spawn()?;
        let pgid = Pid::from_child(&child);
        Ok(Workload {
            child,
            kqueue: self.kqueue,
            events: self.events,
            pgid,
            cleaned: false,
        })
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let _ = finish_process_group(&mut self.child, self.pgid);
    }
}

fn finish_process_group(
    child: &mut Child,
    pgid: Pid,
) -> (io::Result<std::process::ExitStatus>, io::Result<()>) {
    if let Ok(Some(status)) = child.try_wait() {
        return (Ok(status), terminate_process_group(pgid));
    }

    let terminated = match kill_process_group(pgid, Signal::KILL) {
        Ok(()) => Ok(()),
        Err(Errno::PERM) => match child.try_wait() {
            Ok(Some(status)) => return (Ok(status), terminate_process_group(pgid)),
            _ => Err(io::Error::from(Errno::PERM)),
        },
        Err(Errno::SRCH) => match child.try_wait() {
            Ok(Some(status)) => return (Ok(status), Ok(())),
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "process group disappeared while its child was still running",
            )),
        },
        Err(error) => Err(error.into()),
    };
    let fallback = terminated.as_ref().err().map(|_| child.kill());
    let status = reap_after_kill(child, &terminated, fallback.as_ref());
    let cleanup = match fallback {
        Some(fallback) => combine_errors(terminated, fallback),
        None => terminated,
    };
    (status, cleanup)
}

fn terminate_process_group(pgid: Pid) -> io::Result<()> {
    match kill_process_group(pgid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn ready(events: &[Event], interrupt: &Interrupt) -> io::Result<Option<Wait>> {
    let exited = events
        .iter()
        .any(|event| matches!(event.filter(), EventFilter::Proc { .. }));
    let interrupted = events
        .iter()
        .any(|event| matches!(event.filter(), EventFilter::Read(_)));
    if interrupted {
        drain_interrupt(&interrupt.read)?;
    }
    Ok(if exited {
        Some(Wait::Exited)
    } else if interrupted {
        Some(Wait::Interrupted)
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, ensure};
    use std::{ffi::OsString, time::Duration};

    fn spec(test: &str) -> Result<CommandSpec> {
        Ok(CommandSpec::new(
            std::env::current_exe()?.into_os_string(),
            ["--exact", test, "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            std::env::current_dir()?,
            Vec::new(),
        ))
    }

    #[test]
    fn esrch_exit_consumes_a_pending_interrupt() -> Result<()> {
        let mut session = Session::new()?;
        let interrupt = Interrupt::new()?;
        let mut exited = session
            .prepare(&spec("platform::macos::tests::fast_child")?)?
            .spawn()?;
        ensure!(exited.child.wait()?.success());
        interrupt.signal();
        ensure!(matches!(exited.wait(&interrupt, None)?, Wait::Exited));
        ensure!(exited.finish().cleanup.is_ok());

        let mut next = session
            .prepare(&spec("platform::macos::tests::slow_child")?)?
            .spawn()?;
        ensure!(matches!(
            next.wait(&interrupt, Some(Duration::ZERO))?,
            Wait::TimedOut
        ));
        ensure!(next.finish().cleanup.is_ok());
        session.shutdown()?;
        Ok(())
    }

    #[test]
    fn fast_natural_exits_do_not_fail_group_cleanup() -> Result<()> {
        let mut session = Session::new()?;
        let interrupt = Interrupt::new()?;

        for _ in 0..100 {
            let mut workload = session
                .prepare(&spec("platform::macos::tests::fast_child")?)?
                .spawn()?;
            ensure!(matches!(workload.wait(&interrupt, None)?, Wait::Exited));
            let finished = workload.finish();
            ensure!(finished.status?.success());
            finished.cleanup?;
        }

        session.shutdown()?;
        Ok(())
    }

    #[test]
    #[ignore]
    fn fast_child() {}

    #[test]
    #[ignore]
    fn slow_child() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
