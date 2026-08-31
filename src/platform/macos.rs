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
    // Process groups do not contain descendants that call setsid() or setpgid().
    pgid: Pid,
}

pub(crate) struct Prepared {
    kqueue: OwnedFd,
    command: Command,
}

impl Workload {
    pub(crate) fn prepare(spec: &CommandSpec) -> io::Result<Prepared> {
        use std::os::unix::process::CommandExt;
        let mut command = spec.command();
        command.process_group(0);
        Ok(Prepared {
            kqueue: kqueue()?,
            command,
        })
    }

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

        let mut events = Vec::with_capacity(2);
        loop {
            events.clear();
            // SAFETY: The child pid and interrupt fd stay valid for this Workload.
            match unsafe {
                kevent(
                    &self.kqueue,
                    &changes,
                    spare_capacity(&mut events),
                    Some(Duration::ZERO),
                )
            } {
                Ok(_) => break,
                Err(Errno::SRCH) => return Ok(Wait::Exited),
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(outcome) = ready(&events, interrupt)? {
            return Ok(outcome);
        }

        let started = Instant::now();
        loop {
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            events.clear();
            // SAFETY: No changelist entries, so no fd validity requirements to uphold.
            let n = unsafe {
                match kevent(&self.kqueue, &[], spare_capacity(&mut events), remaining) {
                    Ok(n) => n,
                    Err(Errno::INTR) => continue,
                    Err(e) => return Err(e.into()),
                }
            };
            if n == 0 {
                return Ok(Wait::TimedOut);
            }
            if let Some(outcome) = ready(&events, interrupt)? {
                return Ok(outcome);
            }
        }
    }

    pub(crate) fn finish(self) -> Finished {
        let Workload {
            mut child,
            kqueue: _,
            pgid,
        } = self;
        let terminated = match kill_process_group(pgid, Signal::KILL) {
            Ok(()) => Ok(()),
            Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        };
        let fallback = terminated.as_ref().err().map(|_| child.kill());
        let status = reap_after_kill(&mut child, &terminated, fallback.as_ref());
        let cleanup = match fallback {
            Some(fallback) => combine_errors(terminated, fallback),
            None => terminated,
        };
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
            pgid,
        })
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
