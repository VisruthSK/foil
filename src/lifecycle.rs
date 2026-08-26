//! Lifecycle command execution.
//!
//! Setup and teardown commands run outside the measured interval, so they use
//! a lighter executor than benchmarks: no timing, no measurement records, no
//! Linux cgroup per invocation. What they do get is bounded output capture,
//! first-Ctrl+C handling, whole-workload termination, direct-child reaping,
//! and timeout support.
//!
//! Output is spooled to temporary files rather than piped, so a chatty command
//! can never fill a buffer and stall, and diagnostics only pay for reading the
//! final tail of each stream.

use crate::platform::{CommandSpec, Interrupt};
use anyhow::{Context, Result, anyhow};
use std::{
    fs::File,
    io::{Read, Seek},
    path::PathBuf,
    process::ExitStatus,
    time::Duration,
};

/// Per-stream output cap. A failing command's diagnostics keep this much of
/// each stream; anything beyond it is dropped and noted as truncated.
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// How often the Unix wait loop re-checks child exit, interrupts, and timeouts.
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) fn execute(
    spec: &CommandSpec,
    interrupt: &Interrupt,
    timeout: Option<Duration>,
    label: &str,
) -> Result<()> {
    let spool = Spool::new()?;

    let failure = match platform_run(spec, interrupt, timeout, &spool)
        .with_context(|| format!("Failed to run {:?}.", spec.program))?
    {
        None => return Ok(()),
        Some(failure) => failure,
    };

    // Successful output is suppressed anyway, so only failures pay for reading
    // the spooled tails.
    let stdout = tail_from_file(&spool.stdout)?;
    let stderr = tail_from_file(&spool.stderr)?;

    Err(match failure {
        Failure::Exited(status) => anyhow!(
            "{:?} failed with {}.{}",
            spec.program,
            status,
            display_tails(label, &stdout, &stderr)
        ),
        Failure::Interrupted => anyhow!("Interrupted."),
        Failure::TimedOut(limit) => anyhow!("{:?} timed out after {limit:?}.", spec.program),
    })
}

/// Why a lifecycle command did not succeed, when it did not.
enum Failure {
    Exited(ExitStatus),
    Interrupted,
    TimedOut(Duration),
}

/// Temporary files that capture a command's output streams. Deleting them when
/// this is dropped also discards everything not kept by a tail.
struct Spool {
    _directory: tempfile::TempDir,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Spool {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir().context("Failed to create a spool directory.")?;
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        Ok(Self {
            _directory: directory,
            stdout,
            stderr,
        })
    }
}

/// The tail of one output stream, keeping only what fits in `OUTPUT_LIMIT`.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    dropped: bool,
}

fn tail_from_file(path: &std::path::Path) -> Result<Tail> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to read {}.", path.display()))?;
    let length = file.metadata()?.len();
    let mut tail = Tail {
        dropped: length > OUTPUT_LIMIT,
        ..Default::default()
    };
    if tail.dropped {
        file.seek(std::io::SeekFrom::Start(length - OUTPUT_LIMIT))?;
    }
    file.read_to_end(&mut tail.bytes)?;
    Ok(tail)
}

fn display_tails(label: &str, stdout: &Tail, stderr: &Tail) -> String {
    fn stream(label: &str, name: &'static str, tail: &Tail) -> String {
        if tail.bytes.is_empty() && !tail.dropped {
            return String::new();
        }
        let truncation = if tail.dropped {
            "\n[...truncated...]"
        } else {
            ""
        };
        format!(
            "\n[{label} {name}]\n{}{truncation}",
            String::from_utf8_lossy(&tail.bytes).trim_end()
        )
    }
    format!(
        "{}{}",
        stream(label, "stdout", stdout),
        stream(label, "stderr", stderr)
    )
}

#[cfg(unix)]
fn platform_run(
    spec: &CommandSpec,
    interrupt: &Interrupt,
    timeout: Option<Duration>,
    spool: &Spool,
) -> Result<Option<Failure>> {
    use rustix::process::Pid;
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        time::Instant,
    };

    let stdout = File::create(&spool.stdout)?;
    let stderr = File::create(&spool.stderr)?;

    // The child leads its own process group, so the whole workload can be
    // signaled without touching foil's own group.
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0);

    let mut child = command.spawn()?;
    // A pgid of 0 makes the child its own group leader, so the pgid is its pid.
    let group = Pid::from_raw(child.id() as i32).expect("child pid is positive");

    let deadline = timeout.map(|limit| Instant::now() + limit);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(if status.success() {
                None
            } else {
                Some(Failure::Exited(status))
            });
        }
        if interrupt.poll_read()? {
            kill_tree(&mut child, group);
            return Ok(Some(Failure::Interrupted));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            kill_tree(&mut child, group);
            return Ok(Some(Failure::TimedOut(
                timeout.expect("the deadline exists"),
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child, group: rustix::process::Pid) {
    use rustix::process::Signal;

    let terminated = match rustix::process::kill_process_group(group, Signal::KILL) {
        Ok(()) => Ok(()),
        // The group already exited on its own.
        Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(std::io::Error::from(error)),
    };
    if terminated.is_err() {
        let _ = child.kill();
    }
    let reaped = child.wait().map(drop);
    report_secondary(terminated.and(reaped), "Cleanup");
}

#[cfg(windows)]
fn platform_run(
    spec: &CommandSpec,
    interrupt: &Interrupt,
    timeout: Option<Duration>,
    spool: &Spool,
) -> Result<Option<Failure>> {
    use crate::platform::{Wait, Workload};

    let mut workload = Workload::prepare_spooled(&spool.stdout, &spool.stderr)
        .context("Failed to prepare workload containment.")?
        .spawn(spec)?;

    Ok(match workload.wait(interrupt, timeout)? {
        Wait::Exited(status) if status.success() => None,
        Wait::Exited(status) => Some(Failure::Exited(status)),
        Wait::Interrupted => {
            report_secondary(workload.terminate(), "Cleanup");
            Some(Failure::Interrupted)
        }
        Wait::TimedOut => {
            report_secondary(workload.terminate(), "Cleanup");
            Some(Failure::TimedOut(
                timeout.expect("a timed-out wait has a timeout"),
            ))
        }
    })
}

fn report_secondary<E: std::fmt::Display>(result: std::result::Result<(), E>, label: &str) {
    if let Err(error) = result {
        eprintln!("{label} also failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Interrupt;
    use anyhow::ensure;
    use std::{
        env,
        ffi::OsString,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    const MARKER: &str = "FOIL_PROCESS_TEST_MARKER";

    /// A shell one-liner is the only reliable chatty child: the test harness
    /// itself merges and reprints captured streams, so this binary cannot serve.
    #[cfg(windows)]
    fn shell(script: &str) -> CommandSpec {
        CommandSpec::new(
            "cmd".into(),
            vec!["/c".into(), script.into()],
            env::current_dir().expect("a working directory"),
            Vec::new(),
        )
    }

    #[cfg(not(windows))]
    fn shell(script: &str) -> CommandSpec {
        CommandSpec::new(
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
            env::current_dir().expect("a working directory"),
            Vec::new(),
        )
    }

    #[cfg(windows)]
    const FAILING_STREAMS: &str = "echo OUT-LINE & echo ERR-LINE 1>&2 & exit 7";

    #[cfg(not(windows))]
    const FAILING_STREAMS: &str = "echo OUT-LINE; echo ERR-LINE 1>&2; exit 7";

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

    #[test]
    fn a_failing_command_reports_output_from_both_streams() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let spec = shell(FAILING_STREAMS);
        let error = execute(&spec, &interrupt, None, "probe").expect_err("fails");
        let text = format!("{error:#}");
        ensure!(text.contains("failed with exit code: 7"), "{text}");
        ensure!(text.contains("[probe stdout]\nOUT-LINE"), "{text}");
        ensure!(text.contains("[probe stderr]\nERR-LINE"), "{text}");
        Ok(())
    }

    #[test]
    fn successful_lifecycle_output_is_suppressed() -> Result<()> {
        let interrupt = Interrupt::new()?;
        execute(&shell("echo noisy"), &interrupt, None, "quiet phase")?;
        Ok(())
    }

    #[test]
    fn the_first_interrupt_wakes_a_running_lifecycle_command_promptly() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let signal = interrupt.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            signal.signal();
        });

        let started = Instant::now();
        let error = execute(
            &spec("platform::tests::slow_child", Vec::new())?,
            &interrupt,
            None,
            "slow phase",
        )
        .expect_err("the interrupted command fails");
        ensure!(started.elapsed() < Duration::from_secs(5), "{error:#}");
        ensure!(format!("{error:#}").contains("Interrupted."), "{error:#}");
        Ok(())
    }

    #[test]
    fn an_internal_timeout_terminates_a_hung_lifecycle_command() -> Result<()> {
        let interrupt = Interrupt::new()?;
        let started = Instant::now();
        let error = execute(
            &spec("platform::tests::slow_child", Vec::new())?,
            &interrupt,
            Some(Duration::from_millis(200)),
            "hung phase",
        )
        .expect_err("the hung command fails");

        ensure!(started.elapsed() < Duration::from_secs(5), "{error:#}");
        ensure!(format!("{error:#}").contains("timed out"), "{error:#}");
        Ok(())
    }

    #[test]
    fn descendants_of_a_timed_out_startup_do_not_survive() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("descendant-finished");
        let command = spec(
            "lifecycle::tests::spawning_hang",
            vec![(MARKER.into(), marker.as_os_str().to_owned())],
        )?;

        let interrupt = Interrupt::new()?;
        // The command hangs after spawning a descendant; the timeout must end
        // the whole workload, descendant included, before it writes its marker.
        let error = execute(
            &command,
            &interrupt,
            Some(Duration::from_millis(200)),
            "spawning phase",
        )
        .expect_err("the hung spawning command fails");
        ensure!(format!("{error:#}").contains("timed out"), "{error:#}");

        thread::sleep(Duration::from_millis(800));
        ensure!(!marker.exists(), "the descendant survived termination");
        Ok(())
    }

    #[test]
    #[ignore]
    fn slow_child() {
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore]
    fn spawning_hang() {
        let marker = env::var_os(MARKER).expect("a marker path is set");
        Command::new(env::current_exe().expect("the test executable exists"))
            .args(["--exact", "lifecycle::tests::marker_writer", "--ignored"])
            .env(MARKER, marker)
            .status()
            .expect("the descendant starts");
        thread::sleep(Duration::from_secs(30));
    }

    #[test]
    #[ignore]
    fn marker_writer() {
        // Only writes after surviving its sleep, so a killed workload never
        // produces the marker.
        thread::sleep(Duration::from_millis(600));
        let marker = env::var_os(MARKER).expect("a marker path is set");
        std::fs::write(marker, "finished").expect("the marker is writable");
    }

    #[test]
    fn tails_keep_only_the_last_output_limit_bytes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("spool");

        // Exactly at the limit: nothing is dropped.
        std::fs::write(&path, vec![b'a'; OUTPUT_LIMIT as usize])?;
        let tail = tail_from_file(&path)?;
        ensure!(tail.bytes.len() == OUTPUT_LIMIT as usize);
        ensure!(!tail.dropped);

        // One byte over: the tail starts mid-stream and notes the drop.
        std::fs::write(&path, vec![b'a'; OUTPUT_LIMIT as usize + 1])?;
        let tail = tail_from_file(&path)?;
        ensure!(tail.bytes.len() == OUTPUT_LIMIT as usize);
        ensure!(tail.dropped);
        Ok(())
    }
}
