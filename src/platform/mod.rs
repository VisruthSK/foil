#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{ffi::OsString, path::PathBuf, process::ExitStatus};

pub(crate) enum Wait {
    Exited(ExitStatus),
    Interrupted,
    TimedOut,
}

/// A logical benchmark command: what to run, with which arguments, working
/// directory, and effective child environment.
///
/// The executable itself is resolved by each platform backend at spawn time,
/// so a benchmark `startup` command may create the program this spec names.
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

#[cfg(target_os = "linux")]
pub(crate) use linux::{Interrupt, Workload};
#[cfg(target_os = "macos")]
pub(crate) use macos::{Interrupt, Workload};
#[cfg(windows)]
pub(crate) use windows::{Interrupt, Workload};

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result, ensure};
    use std::{
        env, fs, io, thread,
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

    fn spawn(spec: &CommandSpec) -> Result<Workload> {
        Workload::prepare()
            .context("prepare")?
            .spawn(spec)
            .context("spawn")
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
        // A coarse polling loop would overshoot the deadline by whole ticks;
        // native waiting lands within a small multiple of the deadline.
        ensure!(
            elapsed < Duration::from_millis(600),
            "woke after {elapsed:?}"
        );
        workload.terminate()?;
        Ok(())
    }

    /// Direct process exit wins when exit, interrupt, and timeout readiness
    /// become observable in the same wait. This pins runner semantics that each
    /// backend implements natively.
    #[test]
    fn direct_exit_wins_over_simultaneous_interrupt_and_timeout() -> Result<()> {
        let interrupt = Interrupt::new()?;
        // The child exits before the interrupt fires, so by the time wait runs,
        // the exited-child handle and the event are ready at the same instant.
        let mut workload = spawn(&spec("platform::tests::noop_child", Vec::new())?)?;
        thread::sleep(Duration::from_millis(200));
        interrupt.signal();
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(5)))?,
            Wait::Exited(status) if status.success()
        ));
        workload.terminate()?;
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
        workload.terminate()?;
        sender.join().expect("the signal thread does not panic");
        Ok(())
    }

    #[test]
    fn timeout_terminates_the_complete_workload() -> Result<()> {
        let directory = tempdir()?;
        let marker = directory.path().join("descendant-finished");
        let command = spec(
            "platform::tests::parent_child",
            vec![(MARKER.into(), marker.as_os_str().to_owned())],
        )?;
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_millis(100)))?,
            Wait::TimedOut
        ));
        workload.terminate()?;
        thread::sleep(Duration::from_millis(800));
        ensure!(!marker.exists(), "the descendant survived termination");
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
        workload.terminate()?;
        sender.join().expect("the signal thread does not panic");
        thread::sleep(Duration::from_millis(800));
        ensure!(!marker.exists(), "the descendant survived interruption");
        Ok(())
    }

    #[test]
    fn descendant_inheritance_cannot_delay_direct_child_exit() -> Result<()> {
        let directory = tempdir()?;
        let marker = directory.path().join("orphan-finished");
        let command = spec(
            "platform::tests::orphan_parent",
            vec![(MARKER.into(), marker.as_os_str().to_owned())],
        )?;
        let interrupt = Interrupt::new()?;
        let started = Instant::now();
        {
            let mut workload = spawn(&command)?;
            ensure!(matches!(
                workload.wait(&interrupt, Some(Duration::from_secs(2)))?,
                Wait::Exited(status) if status.success()
            ));
            workload.terminate()?;
        }
        ensure!(started.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(800));
        ensure!(!marker.exists(), "the orphan escaped containment");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolves_the_executable_from_the_child_path() -> Result<()> {
        let directory = tempdir()?;
        let executable = directory.path().join("path-only-tool.exe");
        fs::copy(env::current_exe()?, &executable)?;
        let command = CommandSpec::new(
            "path-only-tool".into(),
            ["--exact", "platform::tests::noop_child", "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            env::current_dir()?,
            vec![("PATH".into(), directory.path().as_os_str().to_owned())],
        );
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(2)))?,
            Wait::Exited(status) if status.success()
        ));
        workload.terminate()?;
        Ok(())
    }

    /// A benchmark `startup` may create the executable the benchmark command names,
    /// so the spec must stay logical until spawn time. Spawning before the executable
    /// exists fails; after startup "creates" it, the same spec runs.
    #[cfg(windows)]
    #[test]
    fn an_executable_created_after_configuration_still_runs() -> Result<()> {
        let directory = tempdir()?;
        let executable = directory.path().join("late-tool.exe");
        let command = CommandSpec::new(
            executable.clone().into_os_string(),
            ["--exact", "platform::tests::noop_child", "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            env::current_dir()?,
            Vec::new(),
        );
        let interrupt = Interrupt::new()?;

        let missing = Workload::prepare()
            .context("prepare")?
            .spawn(&command)
            .err()
            .context("spawning a missing program should fail")?;
        ensure!(missing.kind() == io::ErrorKind::NotFound, "{missing}");

        fs::copy(env::current_exe()?, &executable)?;
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(2)))?,
            Wait::Exited(status) if status.success()
        ));
        workload.terminate()?;
        Ok(())
    }

    /// A relative path-qualified program resolves against the benchmark's effective
    /// child working directory, including the implicit `.exe` form.
    #[cfg(windows)]
    #[test]
    fn a_relative_program_resolves_against_the_child_working_directory() -> Result<()> {
        let directory = tempdir()?;
        let nested = directory.path().join("bin");
        fs::create_dir(&nested)?;
        fs::copy(env::current_exe()?, nested.join("nested-tool.exe"))?;
        let command = CommandSpec::new(
            r"bin\nested-tool".into(),
            ["--exact", "platform::tests::noop_child", "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            directory.path().to_owned(),
            Vec::new(),
        );
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(2)))?,
            Wait::Exited(status) if status.success()
        ));
        workload.terminate()?;
        Ok(())
    }

    /// Batch files are rejected outright rather than run with ordinary-executable
    /// argument quoting, whether named by explicit path or found through the PATH.
    #[cfg(windows)]
    #[test]
    fn batch_files_are_rejected_with_a_clear_error() -> Result<()> {
        let directory = tempdir()?;
        let script = directory.path().join("scripty.cmd");
        fs::write(&script, "@echo off\r\nexit /b 0\r\n")?;
        let env = vec![("PATH".into(), directory.path().as_os_str().to_owned())];

        let explicit = CommandSpec::new(
            script.clone().into_os_string(),
            Vec::new(),
            env::current_dir()?,
            Vec::new(),
        );
        let error = Workload::prepare()
            .context("prepare")?
            .spawn(&explicit)
            .err()
            .context("an explicit batch file should be rejected")?;
        ensure!(error.to_string().contains("batch"), "{error}");

        let bare = CommandSpec::new("scripty".into(), Vec::new(), env::current_dir()?, env);
        let error = Workload::prepare()
            .context("prepare")?
            .spawn(&bare)
            .err()
            .context("a batch file found through PATH should be rejected")?;
        ensure!(error.to_string().contains("batch"), "{error}");

        Ok(())
    }

    #[test]
    #[ignore = "release-only native-runner diagnostic"]
    fn native_runner_overhead() -> Result<()> {
        ensure!(
            !cfg!(debug_assertions),
            "run this diagnostic with --release"
        );
        const RUNS: usize = 100;
        let command = spec("platform::tests::noop_child", Vec::new())?;
        let interrupt = Interrupt::new()?;
        let executable = env::current_exe()?;
        let mut raw = Duration::ZERO;
        let mut native = Duration::ZERO;
        for _ in 0..RUNS {
            let started = Instant::now();
            let status = std::process::Command::new(&executable)
                .args(["--exact", "platform::tests::noop_child", "--ignored"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            raw += started.elapsed();
            ensure!(status.success());

            let prepared = Workload::prepare()?;
            let started = Instant::now();
            let mut workload = prepared.spawn(&command)?;
            ensure!(matches!(
                workload.wait(&interrupt, None)?,
                Wait::Exited(status) if status.success()
            ));
            native += started.elapsed();
            workload.terminate()?;
        }
        let overhead = (native.as_secs_f64() - raw.as_secs_f64()) * 1e6 / RUNS as f64;
        eprintln!("native runner overhead: {overhead:.1} us/run");
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
    #[allow(clippy::zombie_processes)]
    fn orphan_parent() {
        child().spawn().expect("the descendant starts");
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
