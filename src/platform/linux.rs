use super::{CommandSpec, Finished, Wait, combine_errors, drain_interrupt, reap_after_kill};
use std::{
    env, fs,
    fs::OpenOptions,
    io::{self, Read, Seek, Write},
    os::{fd::OwnedFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, write};
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::{Pid, PidfdFlags, pidfd_open};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

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

pub(crate) struct Session {
    parent: PathBuf,
    ordinary: PathBuf,
    closed: bool,
}

impl Session {
    pub(crate) fn new() -> io::Result<Self> {
        Self::create(&cgroup_root()?, &NEXT_SESSION)
    }

    fn create(root: &Path, counter: &AtomicU64) -> io::Result<Self> {
        let parent = loop {
            let id = counter.fetch_add(1, Ordering::Relaxed);
            let candidate = root.join(format!("foil-{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        // Processes enter this leaf; the parent stays empty so controllers can be delegated.
        let ordinary = parent.join("ordinary");
        if let Err(error) = fs::create_dir(&ordinary) {
            return match fs::remove_dir(&parent) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::new(
                    error.kind(),
                    format!("{error}; parent cleanup also failed: {cleanup}"),
                )),
            };
        }
        Ok(Self {
            parent,
            ordinary,
            closed: false,
        })
    }

    pub(crate) fn prepare(&mut self, spec: &CommandSpec) -> io::Result<Prepared> {
        ensure_empty(&self.ordinary)?;
        let procs = OpenOptions::new()
            .write(true)
            .open(self.ordinary.join("cgroup.procs"))?;
        let mut command = spec.command();
        unsafe {
            // SAFETY: After fork this closure only invokes async-signal-safe write(2)
            // on an inherited fd and constructs allocation-free raw OS errors.
            command.pre_exec(move || match write(&procs, b"0") {
                Ok(1) => Ok(()),
                Ok(_) => Err(io::Error::from_raw_os_error(Errno::IO.raw_os_error())),
                Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
            });
        }
        Ok(Prepared {
            command,
            ordinary: self.ordinary.clone(),
        })
    }

    pub(crate) fn shutdown(mut self) -> io::Result<()> {
        let (cleanup, empty) = clean_leaf(&self.ordinary);
        let removal = if empty {
            remove_tree(&self.ordinary, &self.parent)
        } else {
            Ok(())
        };
        if removal.is_ok() && empty {
            self.closed = true;
        }
        combine_errors(cleanup, removal)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let _ = kill(&self.ordinary);
        let _ = fs::remove_dir(&self.ordinary);
        let _ = fs::remove_dir(&self.parent);
    }
}

pub(crate) struct Workload {
    child: Child,
    ordinary: PathBuf,
    pidfd: OwnedFd,
    cleaned: bool,
}

pub(crate) struct Prepared {
    command: Command,
    ordinary: PathBuf,
}

impl Workload {
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

    pub(crate) fn finish(mut self) -> Finished {
        let killed = kill(&self.ordinary);
        let fallback = killed.as_ref().err().map(|_| self.child.kill());
        let terminated = killed.is_ok() || fallback.as_ref().is_some_and(Result::is_ok);
        let status = reap_after_kill(&mut self.child, &killed, fallback.as_ref());
        let emptied = wait_after_kill(&self.ordinary, terminated);
        let cleanup = combine_cleanup(killed, fallback, emptied);
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
        let mut child = self.command.spawn()?;
        let pid = Pid::from_child(&child);
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                let killed = kill(&self.ordinary);
                let fallback = killed.as_ref().err().map(|_| child.kill());
                let terminated = killed.is_ok() || fallback.as_ref().is_some_and(Result::is_ok);
                let reaped = reap_after_kill(&mut child, &killed, fallback.as_ref()).map(drop);
                let emptied = wait_after_kill(&self.ordinary, terminated);
                report_secondary(
                    combine_cleanup(killed, fallback, combine_errors(reaped, emptied)),
                    "cgroup cleanup",
                );
                return Err(io::Error::from(error));
            }
        };
        Ok(Workload {
            child,
            ordinary: self.ordinary,
            pidfd,
            cleaned: false,
        })
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let killed = kill(&self.ordinary);
        let fallback = killed.as_ref().err().map(|_| self.child.kill());
        let _ = reap_after_kill(&mut self.child, &killed, fallback.as_ref());
    }
}

fn clean_leaf(path: &Path) -> (io::Result<()>, bool) {
    let killed = kill(path);
    let emptied = wait_after_kill(path, killed.is_ok());
    let empty = emptied.is_ok();
    (combine_errors(killed, emptied), empty)
}

fn kill(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path.join("cgroup.kill"))?
        .write_all(b"1")
}

fn populated(path: &Path) -> io::Result<bool> {
    let state = fs::read_to_string(path.join("cgroup.events"))?;
    let value = state
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
        .ok_or_else(|| io::Error::other("cgroup.events has no populated field"))?;
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(io::Error::other(
            "cgroup.events has an invalid populated field",
        )),
    }
}

fn ensure_empty(path: &Path) -> io::Result<()> {
    if populated(path)? {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "ordinary cgroup is still populated",
        ))
    } else {
        Ok(())
    }
}

fn wait_after_kill(path: &Path, terminated: bool) -> io::Result<()> {
    if terminated {
        wait_empty(path)
    } else {
        ensure_empty(path)
    }
}

fn wait_empty(path: &Path) -> io::Result<()> {
    let mut events = OpenOptions::new()
        .read(true)
        .open(path.join("cgroup.events"))?;
    let mut state = String::with_capacity(64);
    loop {
        events.rewind()?;
        state.clear();
        events.read_to_string(&mut state)?;
        if state.lines().any(|line| line == "populated 0") {
            return Ok(());
        }
        let mut fds = [PollFd::new(&events, PollFlags::PRI | PollFlags::ERR)];
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn remove_tree(ordinary: &Path, parent: &Path) -> io::Result<()> {
    fs::remove_dir(ordinary)?;
    fs::remove_dir(parent)
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

fn cgroup_root() -> io::Result<PathBuf> {
    match env::var_os("FOIL_CGROUP_ROOT") {
        Some(root) => Ok(PathBuf::from(root)),
        None => current_cgroup(),
    }
}

fn current_cgroup() -> io::Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let path = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| io::Error::other("cgroup v2 is unavailable"))?;
    Ok(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result, ensure};
    use std::{ffi::OsString, thread};

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

    fn run(session: &mut Session) -> Result<()> {
        let interrupt = Interrupt::new()?;
        let mut workload = session
            .prepare(&spec("platform::tests::noop_child")?)?
            .spawn()?;
        ensure!(
            fs::read_to_string(session.parent.join("cgroup.procs"))?
                .trim()
                .is_empty()
        );
        ensure!(matches!(workload.wait(&interrupt, None)?, Wait::Exited));
        let finished = workload.finish();
        ensure!(finished.status?.success());
        finished.cleanup?;
        ensure!(
            fs::read_to_string(session.parent.join("cgroup.procs"))?
                .trim()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn sessions_are_unique_and_parents_remain_process_free() -> Result<()> {
        let first = Session::new()?;
        let second = Session::new()?;

        ensure!(first.parent != second.parent);
        ensure!(first.ordinary == first.parent.join("ordinary"));
        ensure!(second.ordinary == second.parent.join("ordinary"));
        ensure!(
            fs::read_to_string(first.parent.join("cgroup.procs"))?
                .trim()
                .is_empty()
        );
        ensure!(
            fs::read_to_string(second.parent.join("cgroup.procs"))?
                .trim()
                .is_empty()
        );

        first.shutdown()?;
        second.shutdown()?;
        Ok(())
    }

    #[test]
    fn sequential_workloads_reuse_the_ordinary_leaf() -> Result<()> {
        let mut session = Session::new()?;
        let parent = session.parent.clone();
        let ordinary = session.ordinary.clone();

        run(&mut session)?;
        ensure!(ordinary.is_dir());
        run(&mut session)?;
        ensure!(session.ordinary == ordinary);

        session.shutdown()?;
        ensure!(!ordinary.exists());
        ensure!(!parent.exists());
        Ok(())
    }

    #[test]
    fn direct_exit_wins_and_consumes_a_pending_interrupt() -> Result<()> {
        let mut session = Session::new()?;
        let interrupt = Interrupt::new()?;
        let mut exited = session
            .prepare(&spec("platform::tests::noop_child")?)?
            .spawn()?;
        ensure!(exited.child.wait()?.success());
        interrupt.signal();
        ensure!(matches!(exited.wait(&interrupt, None)?, Wait::Exited));
        ensure!(exited.finish().cleanup.is_ok());

        let mut next = session
            .prepare(&spec("platform::tests::slow_child")?)?
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
    fn stale_candidate_names_are_skipped() -> Result<()> {
        let root = cgroup_root()?;
        let first = u64::MAX - 1;
        let counter = AtomicU64::new(first);
        let stale = root.join(format!("foil-{}-{first}", std::process::id()));
        fs::create_dir(&stale)?;

        let session = Session::create(&root, &counter)?;
        ensure!(session.parent != stale);
        ensure!(
            session
                .parent
                .ends_with(format!("foil-{}-{}", std::process::id(), u64::MAX))
        );

        session.shutdown()?;
        fs::remove_dir(stale)?;
        Ok(())
    }

    #[test]
    fn concurrent_sessions_do_not_collide() -> Result<()> {
        let sessions = (0..8)
            .map(|_| thread::spawn(Session::new))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().expect("session thread does not panic"))
            .collect::<io::Result<Vec<_>>>()?;
        let mut paths = sessions
            .iter()
            .map(|session| session.parent.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        ensure!(paths.len() == sessions.len());

        for session in sessions {
            session.shutdown()?;
        }
        Ok(())
    }

    #[test]
    fn shutdown_failures_are_returned() -> Result<()> {
        let root = tempfile::tempdir()?;
        let counter = AtomicU64::new(0);
        let session = Session::create(root.path(), &counter)?;

        let error = session
            .shutdown()
            .err()
            .context("a non-cgroup hierarchy should fail cleanup")?;
        ensure!(
            matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::Other),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn workload_cleanup_failure_propagates_without_poisoning_the_session() -> Result<()> {
        let mut session = Session::new()?;
        let mut workload = session
            .prepare(&spec("platform::tests::slow_child")?)?
            .spawn()?;
        workload.ordinary = session.parent.join("missing");

        let finished = workload.finish();
        ensure!(finished.status.is_ok());
        ensure!(finished.cleanup.is_err());

        run(&mut session)?;
        session.shutdown()?;
        Ok(())
    }
}
