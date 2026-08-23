#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{ffi::OsString, path::PathBuf, process::ExitStatus};

pub(crate) enum Wait {
    Exited(ExitStatus),
    Interrupted,
    TimedOut,
}

pub(crate) struct CommandSpec {
    pub(crate) program: OsString,
    #[cfg(not(windows))]
    pub(crate) args: Vec<OsString>,
    #[cfg(not(windows))]
    pub(crate) cwd: PathBuf,
    #[cfg(not(windows))]
    pub(crate) env: Vec<(OsString, OsString)>,

    #[cfg(windows)]
    pub(crate) command_line: Vec<u16>,
    #[cfg(windows)]
    pub(crate) environment: Vec<u16>,
    #[cfg(windows)]
    pub(crate) cwd_wide: Vec<u16>,
}

impl CommandSpec {
    pub(crate) fn new(
        program: OsString,
        args: Vec<OsString>,
        cwd: PathBuf,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        #[cfg(windows)]
        {
            let command_line = windows_command_line(&program, &args);
            let environment = windows_environment(&env);
            let cwd_wide = wide(cwd.as_os_str());
            Self {
                program,
                command_line,
                environment,
                cwd_wide,
            }
        }
        #[cfg(not(windows))]
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

#[cfg(windows)]
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn windows_command_line(program: &std::ffi::OsStr, args: &[OsString]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut line = Vec::new();
    for argument in std::iter::once(program).chain(args.iter().map(OsString::as_os_str)) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        let argument: Vec<_> = argument.encode_wide().collect();
        let quoted = argument.is_empty()
            || argument
                .iter()
                .any(|&unit| [b' ' as u16, b'\t' as u16, b'"' as u16].contains(&unit));
        if quoted {
            line.push(b'"' as u16);
        }
        let mut backslashes = 0;
        for unit in argument {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else {
                if unit == b'"' as u16 {
                    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
                } else {
                    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
                }
                backslashes = 0;
                line.push(unit);
            }
        }
        line.extend(std::iter::repeat_n(
            b'\\' as u16,
            if quoted { backslashes * 2 } else { backslashes },
        ));
        if quoted {
            line.push(b'"' as u16);
        }
    }
    line.push(0);
    line
}

#[cfg(windows)]
fn windows_environment(overrides: &[(OsString, OsString)]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut variables: Vec<_> = std::env::vars_os().collect();
    for (key, value) in overrides {
        variables.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        variables.push((key.clone(), value.clone()));
    }
    variables.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_lowercase());
    let mut block = Vec::new();
    for (key, value) in variables {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result, ensure};
    use std::{
        env, fs, thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    const MARKER: &str = "B3_PROCESS_TEST_MARKER";

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
    fn short_processes_are_not_quantized_to_a_poll_interval() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let mut fastest = Duration::MAX;
        for _ in 0..5 {
            let mut workload = spawn(&spec("platform::tests::brief_child", Vec::new())?)?;
            let started = Instant::now();
            ensure!(matches!(workload.wait(&interrupt, None)?, Wait::Exited(_)));
            fastest = fastest.min(started.elapsed());
            workload.terminate()?;
        }
        ensure!(
            fastest < Duration::from_millis(45),
            "fastest wait was {fastest:?}"
        );
        Ok(())
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
        ensure!(elapsed < Duration::from_secs(2), "woke after {elapsed:?}");
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

    #[test]
    fn process_exit_precedes_simultaneous_interrupt_and_timeout() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&spec("platform::tests::noop_child", Vec::new())?)?;
        thread::sleep(Duration::from_millis(100));
        interrupt.signal();
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::ZERO))?,
            Wait::Exited(status) if status.success()
        ));
        workload.terminate()?;
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
    fn brief_child() {
        thread::sleep(Duration::from_millis(25));
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
