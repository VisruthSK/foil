use super::{CommandSpec, Wait};
use std::{
    io,
    mem::zeroed,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::Child,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Interrupt {
    read: Arc<OwnedFd>,
    write: Arc<OwnedFd>,
}

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        cvt(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        set_flags(&read, libc::O_NONBLOCK)?;
        set_flags(&write, libc::O_NONBLOCK)?;
        Ok(Self {
            read: Arc::new(read),
            write: Arc::new(write),
        })
    }

    pub(crate) fn signal(&self) {
        unsafe { libc::write(self.write.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    }
}

pub(crate) struct Workload {
    child: Child,
    kqueue: OwnedFd,
    // Process groups do not contain descendants that call setsid() or setpgid().
    pgid: libc::pid_t,
}

pub(crate) struct Prepared {
    kqueue: OwnedFd,
}

impl Workload {
    pub(crate) fn prepare() -> io::Result<Prepared> {
        Ok(Prepared {
            kqueue: fd(unsafe { libc::kqueue() })?,
        })
    }

    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        let changes = [
            event(
                self.child.id() as usize,
                libc::EVFILT_PROC,
                libc::EV_ADD | libc::EV_ONESHOT,
                libc::NOTE_EXIT,
            ),
            event(
                interrupt.read.as_raw_fd() as usize,
                libc::EVFILT_READ,
                libc::EV_ADD,
                0,
            ),
        ];
        let registered = cvt(unsafe {
            libc::kevent(
                self.kqueue.as_raw_fd(),
                changes.as_ptr(),
                changes.len() as i32,
                ptr::null_mut(),
                0,
                ptr::null(),
            )
        });
        if let Err(error) = registered {
            if error.raw_os_error() == Some(libc::ESRCH) {
                return self.child.wait().map(Wait::Exited);
            }
            return Err(error);
        }
        let started = Instant::now();
        let mut events = [unsafe { zeroed() }; 2];
        loop {
            let remaining =
                timeout.map(|timeout| timespec(timeout.saturating_sub(started.elapsed())));
            let count = unsafe {
                libc::kevent(
                    self.kqueue.as_raw_fd(),
                    ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    remaining.as_ref().map_or(ptr::null(), |value| value),
                )
            };
            if count == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                return Ok(Wait::TimedOut);
            }
            if events[..count as usize]
                .iter()
                .any(|event| event.filter == libc::EVFILT_PROC)
            {
                return self.child.wait().map(Wait::Exited);
            }
            return Ok(Wait::Interrupted);
        }
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let terminate = killpg(self.pgid);
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
        let pgid = child.id() as libc::pid_t;
        Ok(Workload {
            child,
            kqueue: self.kqueue,
            pgid,
        })
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        let _ = killpg(self.pgid);
    }
}

fn event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: ptr::null_mut(),
    }
}

fn killpg(pgid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn set_flags(fd: &OwnedFd, flags: i32) -> io::Result<()> {
    cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags) }).map(drop)?;
    cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }).map(drop)
}

fn timespec(duration: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: duration.as_secs().min(i64::MAX as u64) as i64,
        tv_nsec: duration.subsec_nanos().into(),
    }
}

fn fd(raw: i32) -> io::Result<OwnedFd> {
    cvt(raw).map(|raw| unsafe { OwnedFd::from_raw_fd(raw) })
}

fn cvt(result: i32) -> io::Result<i32> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}
