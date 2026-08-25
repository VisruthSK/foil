use super::{CommandSpec, Wait};
use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, BorrowedFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Child,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, write};
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::{Pid, PidfdFlags, pidfd_open};

static CGROUP_ID: AtomicU64 = AtomicU64::new(0);
static CGROUP_ROOT: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct Interrupt {
    read: Arc<OwnedFd>,
    write: Arc<OwnedFd>,
}

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
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
            PollFd::new(&self.pidfd, PollFlags::IN),
            PollFd::new(&*interrupt.read, PollFlags::IN),
        ];
        let started = Instant::now();
        loop {
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            let ts = remaining.map(|r| Timespec::try_from(r).expect("duration fits in Timespec"));
            match poll(&mut fds, ts.as_ref()) {
                Ok(0) => return Ok(Wait::TimedOut),
                Ok(_) => {
                    if fds[0].revents().contains(PollFlags::IN) {
                        return self.child.wait().map(Wait::Exited);
                    }
                    if fds[1].revents().contains(PollFlags::IN) {
                        return Ok(Wait::Interrupted);
                    }
                }
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
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
                // SAFETY: procs_fd is a valid open file descriptor for the lifetime of this closure.
                let fd = BorrowedFd::borrow_raw(procs_fd);
                match write(fd, b"0") {
                    Ok(1) => Ok(()),
                    Ok(_) => Err(io::Error::other(
                        "write to cgroup.procs did not write all bytes",
                    )),
                    Err(error) => Err(error.into()),
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
        let pid = Pid::from_child(&child);
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                let error = io::Error::from(error);
                let _ = kill_cgroup(&cgroup);
                let _ = child.wait();
                let _ = remove_when_empty(&cgroup);
                return Err(io::Error::new(
                    error.kind(),
                    format!("pidfd_open failed; foil requires Linux 5.14 or newer: {error}"),
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
        let mut fds = [PollFd::new(&events, PollFlags::PRI | PollFlags::ERR)];
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn create_cgroup() -> io::Result<PathBuf> {
    let root = cgroup_root()?;
    for _ in 0..16 {
        let id = CGROUP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("foil-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to create a workload cgroup under {}. Run foil inside a writable delegated cgroup v2 subtree: {error}",
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
    let root = match env::var_os("FOIL_CGROUP_ROOT") {
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
