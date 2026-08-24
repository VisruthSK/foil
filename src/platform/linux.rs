use super::{CommandSpec, Wait};
use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Child,
    ptr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static CGROUP_ID: AtomicU64 = AtomicU64::new(0);
static CGROUP_ROOT: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct Interrupt {
    read: Arc<OwnedFd>,
    write: Arc<OwnedFd>,
}

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        cvt(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) })?;
        Ok(Self {
            read: Arc::new(unsafe { OwnedFd::from_raw_fd(fds[0]) }),
            write: Arc::new(unsafe { OwnedFd::from_raw_fd(fds[1]) }),
        })
    }

    pub(crate) fn signal(&self) {
        unsafe { libc::write(self.write.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    }
}

pub(crate) struct Workload {
    child: Child,
    cgroup: PathBuf,
    pidfd: OwnedFd,
}

pub(crate) struct Prepared {
    cgroup: Option<PathBuf>,
    procs: File,
}

impl Workload {
    pub(crate) fn prepare() -> io::Result<Prepared> {
        let cgroup = create_cgroup()?;
        let procs = match OpenOptions::new()
            .write(true)
            .open(cgroup.join("cgroup.procs"))
        {
            Ok(procs) => procs,
            Err(error) => {
                let _ = fs::remove_dir(&cgroup);
                return Err(error);
            }
        };
        Ok(Prepared {
            cgroup: Some(cgroup),
            procs,
        })
    }

    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        let mut fds = [
            libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: interrupt.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let started = Instant::now();
        loop {
            let timeout =
                timeout.map(|timeout| timespec(timeout.saturating_sub(started.elapsed())));
            let result = unsafe {
                libc::ppoll(
                    fds.as_mut_ptr(),
                    fds.len() as libc::nfds_t,
                    timeout.as_ref().map_or(ptr::null(), |value| value),
                    ptr::null(),
                )
            };
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds[0].revents != 0 {
                return self.child.wait().map(Wait::Exited);
            }
            if fds[1].revents != 0 {
                return Ok(Wait::Interrupted);
            }
            if result == 0 {
                return Ok(Wait::TimedOut);
            }
        }
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let terminate = kill_cgroup(&self.cgroup);
        if terminate.is_err() {
            let _ = self.child.kill();
        }
        let reap = self.child.wait().map(drop);
        let remove = terminate
            .as_ref()
            .map_or(Ok(()), |_| remove_when_empty(&self.cgroup));
        terminate.and(reap).and(remove)
    }
}

impl Prepared {
    pub(crate) fn spawn(mut self, spec: &CommandSpec) -> io::Result<Workload> {
        let procs_fd = self.procs.as_raw_fd();
        let cgroup = self.cgroup.take().expect("prepared cgroup is available");
        let mut command = spec.command();
        unsafe {
            command.pre_exec(move || {
                let result = libc::write(procs_fd, b"0".as_ptr().cast(), 1);
                if result == 1 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir(&cgroup);
                return Err(error);
            }
        };
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) } as i32;
        let pidfd = match fd(raw) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                let _ = kill_cgroup(&cgroup);
                let _ = child.wait();
                let _ = remove_when_empty(&cgroup);
                return Err(io::Error::new(
                    error.kind(),
                    format!("pidfd_open failed; b3 requires Linux 5.14 or newer: {error}"),
                ));
            }
        };
        Ok(Workload {
            child,
            cgroup,
            pidfd,
        })
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        if let Some(cgroup) = &self.cgroup {
            let _ = fs::remove_dir(cgroup);
        }
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        let _ = kill_cgroup(&self.cgroup);
        let _ = fs::remove_dir(&self.cgroup);
    }
}

fn kill_cgroup(cgroup: &std::path::Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(cgroup.join("cgroup.kill"))?
        .write_all(b"1")
}

fn remove_when_empty(cgroup: &std::path::Path) -> io::Result<()> {
    let mut events = OpenOptions::new()
        .read(true)
        .open(cgroup.join("cgroup.events"))?;
    let mut state = String::with_capacity(64);
    loop {
        events.rewind()?;
        state.clear();
        events.read_to_string(&mut state)?;
        if state.lines().any(|line| line == "populated 0") {
            return fs::remove_dir(cgroup);
        }
        let mut event = libc::pollfd {
            fd: events.as_raw_fd(),
            events: libc::POLLPRI | libc::POLLERR,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut event, 1, -1) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn create_cgroup() -> io::Result<PathBuf> {
    let root = cgroup_root()?;
    for _ in 0..16 {
        let id = CGROUP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("b3-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to create a workload cgroup under {}. Run b3 inside a writable delegated cgroup v2 subtree: {error}",
                        root.display()
                    ),
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not allocate a cgroup.",
    ))
}

fn cgroup_root() -> io::Result<&'static PathBuf> {
    if let Some(root) = CGROUP_ROOT.get() {
        return Ok(root);
    }
    let root = match env::var_os("B3_CGROUP_ROOT") {
        Some(root) => PathBuf::from(root),
        None => current_cgroup()?,
    };
    let _ = CGROUP_ROOT.set(root);
    Ok(CGROUP_ROOT.get().expect("the cgroup root was initialized"))
}

fn current_cgroup() -> io::Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let path = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| io::Error::other("cgroup v2 is unavailable"))?;
    Ok(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
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
