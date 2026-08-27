use super::{CommandSpec, Wait, drain_interrupt};
use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, BorrowedFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, write};
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::{Pid, PidfdFlags, pidfd_open};

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
    command: Command,
    cgroup: Option<PathBuf>,
}

impl Workload {
    pub(crate) fn prepare(spec: &CommandSpec) -> io::Result<Prepared> {
        let cgroup = create_cgroup()?;
        let procs = OpenOptions::new()
            .write(true)
            .open(cgroup.join("cgroup.procs"))
            .map_err(|error| {
                let _ = fs::remove_dir(&cgroup);
                error
            })?;

        // The closure owns the cgroup.procs handle; it stays open until the
        // command is dropped, which happens after spawn has forked and exec'd.
        let mut command = spec.command();
        unsafe {
            command.pre_exec(move || {
                let fd = BorrowedFd::borrow_raw(procs.as_raw_fd());
                match write(fd, b"0") {
                    Ok(1) => Ok(()),
                    Ok(_) => Err(io::Error::other(
                        "write to cgroup.procs did not write all bytes",
                    )),
                    Err(error) => Err(error.into()),
                }
            });
        }

        Ok(Prepared {
            command,
            cgroup: Some(cgroup),
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
            let ts = remaining
                .map(Timespec::try_from)
                .transpose()
                .map_err(|_| io::Error::other("timeout exceeds the Timespec range"))?;
            match poll(&mut fds, ts.as_ref()) {
                Ok(0) => return Ok(Wait::TimedOut),
                Ok(_) => {
                    if fds[0].revents().contains(PollFlags::IN) {
                        return self.child.wait().map(Wait::Exited);
                    }
                    if fds[1].revents().contains(PollFlags::IN) {
                        drain_interrupt(&interrupt.read);
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
        let reaped = self.child.wait().map(drop);
        let remove = terminate
            .as_ref()
            .map_or(Ok(()), |_| remove_when_empty(&self.cgroup));
        terminate.and(reaped).and(remove)
    }
}

impl Prepared {
    pub(crate) fn spawn(mut self) -> io::Result<Workload> {
        let cgroup = self.cgroup.take().expect("prepared cgroup is available");
        let mut child = match self.command.spawn() {
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
                let _ = kill_cgroup(&cgroup);
                let _ = child.wait();
                let _ = remove_when_empty(&cgroup);
                return Err(io::Error::from(error));
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

/// Creates a cgroup named `foil-{pid}`; the deterministic name means only one
/// workload may be live at a time — a stale cgroup surfaces as an error.
/// Creates a cgroup named `foil-{pid}`; the deterministic name means only one
/// workload may be live at a time — a stale cgroup surfaces as an error.
fn create_cgroup() -> io::Result<PathBuf> {
    let root = cgroup_root()?;
    let path = root.join(format!("foil-{}", std::process::id()));
    fs::create_dir(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                error.kind(),
                format!(
                    "stale foil cgroup exists: {}; remove it or set FOIL_CGROUP_ROOT",
                    path.display()
                ),
            )
        } else {
            error
        }
    })?;
    Ok(path)
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
