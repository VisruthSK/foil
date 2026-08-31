use super::{CommandSpec, Finished, Wait, combine_errors, drain_interrupt, reap_after_kill};
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

struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    fn new() -> io::Result<Self> {
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
        Ok(Self { path })
    }

    fn open_procs(&self) -> io::Result<fs::File> {
        OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))
    }

    fn kill(&self) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.kill"))?
            .write_all(b"1")
    }

    fn wait_empty_and_remove(self) -> io::Result<()> {
        let mut events = OpenOptions::new()
            .read(true)
            .open(self.path.join("cgroup.events"))?;
        let mut state = String::with_capacity(64);
        loop {
            events.rewind()?;
            state.clear();
            events.read_to_string(&mut state)?;
            if state.lines().any(|line| line == "populated 0") {
                return fs::remove_dir(&self.path);
            }
            let mut fds = [PollFd::new(&events, PollFlags::PRI | PollFlags::ERR)];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = fs::remove_dir(&self.path);
    }
}

pub(crate) struct Workload {
    child: Child,
    cgroup: Cgroup,
    pidfd: OwnedFd,
}

pub(crate) struct Prepared {
    command: Command,
    cgroup: Option<Cgroup>,
}

impl Workload {
    pub(crate) fn prepare(spec: &CommandSpec) -> io::Result<Prepared> {
        let cgroup = Cgroup::new()?;
        let procs = cgroup.open_procs()?;

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
                        if fds[1].revents().contains(PollFlags::IN) {
                            drain_interrupt(&interrupt.read)?;
                        }
                        return Ok(Wait::Exited);
                    }
                    if fds[1].revents().contains(PollFlags::IN) {
                        drain_interrupt(&interrupt.read)?;
                        return Ok(Wait::Interrupted);
                    }
                }
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(crate) fn finish(self) -> Finished {
        let Workload {
            mut child,
            cgroup,
            pidfd: _,
        } = self;
        let killed = cgroup.kill();
        let fallback = killed.as_ref().err().map(|_| child.kill());
        let status = reap_after_kill(&mut child, &killed, fallback.as_ref());
        let removed = if killed.is_ok() {
            cgroup.wait_empty_and_remove()
        } else {
            fs::remove_dir(&cgroup.path)
        };
        let cleanup = combine_cleanup(killed, fallback, removed);

        Finished {
            status,
            peak_memory: None,
            cleanup,
        }
    }
}

impl Prepared {
    pub(crate) fn spawn(mut self) -> io::Result<Workload> {
        let cgroup = self.cgroup.take().expect("prepared cgroup is available");
        let mut child = match self.command.spawn() {
            Ok(child) => child,
            Err(error) => return Err(error),
        };
        let pid = Pid::from_child(&child);
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                let killed = cgroup.kill();
                let fallback = killed.as_ref().err().map(|_| child.kill());
                let reaped = reap_after_kill(&mut child, &killed, fallback.as_ref()).map(drop);
                let removed = if killed.is_ok() {
                    cgroup.wait_empty_and_remove()
                } else {
                    fs::remove_dir(&cgroup.path)
                };
                report_secondary(combine_cleanup(killed, fallback, reaped), "cgroup cleanup");
                report_secondary(removed, "cgroup removal");
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

fn combine_cleanup(
    primary: io::Result<()>,
    fallback: Option<io::Result<()>>,
    final_step: io::Result<()>,
) -> io::Result<()> {
    let mut result = primary;
    if let Some(fallback) = fallback {
        result = combine_errors(result, fallback);
    }
    combine_errors(result, final_step)
}

fn report_secondary(result: io::Result<()>, label: &str) {
    if let Err(error) = result {
        eprintln!("{label} also failed: {error}");
    }
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
