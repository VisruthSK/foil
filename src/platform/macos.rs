use super::{CommandSpec, Wait};
use std::{
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::Child,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

use rustix::buffer::spare_capacity;
use rustix::event::kqueue::{
    Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue,
};
use rustix::io::{Errno, FdFlags, fcntl_setfd, write};
use rustix::fs::{OFlags, fcntl_setfl};
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
    pgid: i32,
}

pub(crate) struct Prepared {
    kqueue: OwnedFd,
}

impl Workload {
    pub(crate) fn prepare() -> io::Result<Prepared> {
        Ok(Prepared {
            kqueue: kqueue()?,
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

        let mut dummy = Vec::with_capacity(2);
        // SAFETY: The child process and interrupt pipe fd are valid for the lifetime of this Workload.
        let registered = unsafe {
            kevent(&self.kqueue, &changes, spare_capacity(&mut dummy), Some(Duration::ZERO))
        };
        match registered {
            Ok(_) => {}
            Err(Errno::SRCH) => return self.child.wait().map(Wait::Exited),
            Err(error) => return Err(error.into()),
        }

        let started = Instant::now();
        let mut eventlist = Vec::with_capacity(2);
        loop {
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            eventlist.clear();
            // SAFETY: No changelist entries, so no fd validity requirements to uphold.
            let n = unsafe {
                kevent(&self.kqueue, &[], spare_capacity(&mut eventlist), remaining)?
            };
            if n == 0 {
                return Ok(Wait::TimedOut);
            }
            if eventlist
                .iter()
                .any(|ev| matches!(ev.filter(), EventFilter::Proc { .. }))
            {
                return self.child.wait().map(Wait::Exited);
            }
            return Ok(Wait::Interrupted);
        }
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let pgid = Pid::from_raw(self.pgid).expect("child pgid is positive");
        let terminate = match kill_process_group(pgid, Signal::KILL) {
            Ok(()) => Ok(()),
            Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        };
        if terminate.is_err() {
            let _ = self.child.kill();
        }
        let reap = self.child.wait().map(drop);
        terminate.and(reap)
    }
}

impl Prepared {
    pub(crate) fn spawn(self, spec: &CommandSpec) -> io::Result<Workload> {
        let mut command = spec.command();
        command.process_group(0);
        let child = command.spawn()?;
        let pgid = child.id() as i32;
        Ok(Workload {
            child,
            kqueue: self.kqueue,
            pgid,
        })
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        let pgid = Pid::from_raw(self.pgid).expect("child pgid is positive");
        let _ = kill_process_group(pgid, Signal::KILL);
    }
}
