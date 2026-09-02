#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{ffi::OsString, path::PathBuf, process::ExitStatus};

#[cfg(unix)]
use std::io;

pub(crate) enum Wait {
    Exited,
    Interrupted,
    TimedOut,
}

pub(crate) struct Finished {
    pub(crate) status: std::io::Result<ExitStatus>,
    pub(crate) peak_memory: Option<crate::Bytes>,
    pub(crate) cleanup: std::io::Result<()>,
}

fn combine_errors(
    primary: std::io::Result<()>,
    secondary: std::io::Result<()>,
) -> std::io::Result<()> {
    match (primary, secondary) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(primary), Err(secondary)) => Err(std::io::Error::new(
            primary.kind(),
            format!("{primary}; additionally: {secondary}"),
        )),
    }
}

#[cfg(unix)]
fn reap_after_kill(
    child: &mut std::process::Child,
    containment: &std::io::Result<()>,
    fallback: Option<&std::io::Result<()>>,
) -> std::io::Result<ExitStatus> {
    if containment.is_ok() || fallback.is_some_and(Result::is_ok) {
        child.wait()
    } else {
        child.try_wait()?.ok_or_else(|| {
            std::io::Error::other("workload could not be terminated safely; refusing to wait")
        })
    }
}

/// A logical benchmark command: what to run, with which arguments, working
/// directory, and effective child environment.
///
/// The executable is resolved late enough that a benchmark `startup` command
/// may create the program this spec names: on Windows at prepare time, on
/// Unix by the OS at exec time.
pub(crate) struct CommandSpec {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
}

impl CommandSpec {
    pub(crate) fn new(
        program: OsString,
        args: Vec<OsString>,
        cwd: PathBuf,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            program,
            args,
            cwd,
            env,
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) fn drain_interrupt(fd: &std::os::fd::OwnedFd) -> io::Result<()> {
    use rustix::io::{Errno, read};
    loop {
        match read(fd, &mut [0u8; 64]) {
            Ok(0) => return Ok(()),
            Ok(_) => continue,
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return Ok(()),
            Err(error) => return Err(io::Error::from(error)),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) use linux::Workload;
#[cfg(target_os = "linux")]
pub(crate) use linux::{Interrupt, Session};
#[cfg(all(test, target_os = "macos"))]
pub(crate) use macos::Workload;
#[cfg(target_os = "macos")]
pub(crate) use macos::{Interrupt, Session};
#[cfg(all(test, windows))]
pub(crate) use windows::Workload;
#[cfg(windows)]
pub(crate) use windows::{Interrupt, Session};

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result, ensure};
    use std::{
        env, fs, thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    const MARKER: &str = "FOIL_PROCESS_TEST_MARKER";

    fn spec(test: &str, env: Vec<(OsString, OsString)>) -> Result<CommandSpec> {
        Ok(CommandSpec::new(
            env::current_exe()?.into_os_string(),
            ["--exact", test, "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            env::current_dir()?,
            env,
        ))
    }

    struct TestWorkload {
        session: Session,
        workload: Workload,
    }

    impl TestWorkload {
        fn wait(
            &mut self,
            interrupt: &Interrupt,
            timeout: Option<Duration>,
        ) -> std::io::Result<Wait> {
            self.workload.wait(interrupt, timeout)
        }
    }

    fn spawn(spec: &CommandSpec) -> Result<TestWorkload> {
        let mut session = Session::new().context("session")?;
        let workload = session
            .prepare(spec)
            .context("prepare")?
            .spawn()
            .context("spawn")?;
        Ok(TestWorkload { session, workload })
    }

    fn finish(workload: TestWorkload) -> Result<ExitStatus> {
        let finished = workload.workload.finish();
        let status = finished.status?;
        finished.cleanup?;
        workload.session.shutdown()?;
        Ok(status)
    }

    #[test]
    fn timeout_wakes_near_its_deadline_and_reaps_the_child() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&spec("platform::tests::slow_child", Vec::new())?)?;
        let started = Instant::now();
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_millis(100)))?,
            Wait::TimedOut
        ));
        let elapsed = started.elapsed();
        ensure!(
            elapsed >= Duration::from_millis(80),
            "woke after {elapsed:?}"
        );
        // Coarse polling would overshoot by whole ticks; native waits do not.
        ensure!(
            elapsed < Duration::from_millis(600),
            "woke after {elapsed:?}"
        );
        let _ = finish(workload)?;
        Ok(())
    }

    #[test]
    fn interrupt_wakes_a_blocked_wait_promptly() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let signal = interrupt.clone();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal.signal();
        });
        let mut workload = spawn(&spec("platform::tests::slow_child", Vec::new())?)?;
        let started = Instant::now();
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(30)))?,
            Wait::Interrupted
        ));
        ensure!(started.elapsed() < Duration::from_secs(2));
        let _ = finish(workload)?;
        sender.join().expect("the signal thread does not panic");
        Ok(())
    }
    #[test]
    fn interruption_terminates_the_complete_workload() -> Result<()> {
        let directory = tempdir()?;
        let marker = directory.path().join("descendant-finished");
        let command = spec(
            "platform::tests::parent_child",
            vec![(MARKER.into(), marker.as_os_str().to_owned())],
        )?;
        let interrupt = Interrupt::new()?;
        let signal = interrupt.clone();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal.signal();
        });
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(30)))?,
            Wait::Interrupted
        ));
        let _ = finish(workload)?;
        sender.join().expect("the signal thread does not panic");
        thread::sleep(Duration::from_millis(800));
        ensure!(!marker.exists(), "the descendant survived interruption");
        Ok(())
    }
    /// The first interrupt is consumed by the wait that observed it, so
    /// teardown and later workloads start clean; only a second Ctrl+C exits.
    #[test]
    fn an_interrupt_does_not_poison_later_workloads() -> Result<()> {
        let interrupt = Interrupt::new()?;
        interrupt.signal();
        let mut first = spawn(&spec("platform::tests::slow_child", Vec::new())?)?;
        ensure!(matches!(
            first.wait(&interrupt, Some(Duration::from_secs(5)))?,
            Wait::Interrupted
        ));
        let _ = finish(first)?;

        let mut second = spawn(&spec("platform::tests::noop_child", Vec::new())?)?;
        ensure!(matches!(
            second.wait(&interrupt, Some(Duration::from_secs(5)))?,
            Wait::Exited
        ));
        ensure!(finish(second)?.success());
        Ok(())
    }
    #[test]
    #[ignore]
    fn slow_child() {
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore]
    fn noop_child() {}

    #[test]
    #[ignore]
    fn parent_child() {
        let status = child().status().expect("the descendant starts");
        assert!(status.success(), "the descendant failed with {status}");
    }

    #[test]
    #[ignore]
    fn descendant() {
        thread::sleep(Duration::from_millis(600));
        fs::write(
            env::var_os(MARKER).expect("a marker path is set"),
            "finished",
        )
        .expect("the marker is writable");
    }

    fn child() -> std::process::Command {
        let mut child =
            std::process::Command::new(env::current_exe().expect("the test executable exists"));
        child
            .args(["--exact", "platform::tests::descendant", "--ignored"])
            .env(MARKER, env::var_os(MARKER).expect("a marker path is set"));
        child
    }
}
