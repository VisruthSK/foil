#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::{ffi::OsString, io, path::PathBuf, process::ExitStatus};

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
    pub(crate) application: Vec<u16>,
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
    ) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let application = windows_application(&program, &env)?;
            let command_line = windows_command_line(&program, &args);
            let environment = windows_environment(&env);
            let cwd_wide = wide(cwd.as_os_str());
            Ok(Self {
                program,
                application,
                command_line,
                environment,
                cwd_wide,
            })
        }
        #[cfg(not(windows))]
        Ok(Self {
            program,
            args,
            cwd,
            env,
        })
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
fn windows_application(
    program: &std::ffi::OsStr,
    overrides: &[(OsString, OsString)],
) -> io::Result<Vec<u16>> {
    use std::path::Path;
    if program.is_empty() || Path::new(program).file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path has no file name",
        ));
    }
    let path = Path::new(program);
    if path.components().count() > 1 {
        return Ok(wide(with_exe(path).as_os_str()));
    }
    let child_path = overrides
        .iter()
        .rev()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.as_os_str());
    let mut directories = Vec::new();
    if let Some(path) = child_path {
        directories.extend(std::env::split_paths(path).filter(|path| !path.as_os_str().is_empty()));
    }
    if let Ok(mut path) = std::env::current_exe() {
        path.pop();
        directories.push(path);
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        directories.push(PathBuf::from(&root).join("System32"));
        directories.push(root.into());
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories
            .extend(std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()));
    }
    directories
        .into_iter()
        .map(|directory| directory.join(program))
        .find_map(|path| searched_executable(&path))
        .map(|path| wide(path.as_os_str()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found"))
}

#[cfg(windows)]
fn searched_executable(path: &std::path::Path) -> Option<PathBuf> {
    let mut path = path.to_owned();
    if path.extension().is_none() {
        path.set_extension("exe");
    }
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn with_exe(path: &std::path::Path) -> PathBuf {
    if path.extension().is_none() {
        let mut executable = path.to_owned();
        executable.set_extension("exe");
        if executable.is_file() {
            return executable;
        }
    }
    path.to_owned()
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
        )?)
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
        )?;
        let interrupt = Interrupt::new()?;
        let mut workload = spawn(&command)?;
        ensure!(matches!(
            workload.wait(&interrupt, Some(Duration::from_secs(2)))?,
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
