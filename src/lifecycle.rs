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
use anyhow::{Context, Result, bail};
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
    let directory = tempfile::tempdir().context("Failed to create a spool directory.")?;
    let spool = Spool {
        stdout: directory.path().join("stdout"),
        stderr: directory.path().join("stderr"),
    };

    let outcome = platform_run(spec, interrupt, timeout, &spool)
        .with_context(|| format!("Failed to run {:?}.", spec.program))?;

    // Successful output is suppressed anyway, so only failures and aborts pay
    // for reading the spooled tails. The TempDir cleans up either way.
    if matches!(outcome, Outcome::Succeeded) {
        return Ok(());
    }
    let stdout = tail_from_file(&spool.stdout)?;
    let stderr = tail_from_file(&spool.stderr)?;

    match outcome {
        Outcome::Failed(status) => bail!(
            "{:?} failed with {}.{}",
            spec.program,
            status,
            display_tails(label, &stdout, &stderr)
        ),
        Outcome::Interrupted => bail!("Interrupted."),
        Outcome::TimedOut(limit) => bail!("{:?} timed out after {limit:?}.", spec.program),
        Outcome::Succeeded => unreachable!("handled above"),
    }
}

enum Outcome {
    Succeeded,
    Failed(ExitStatus),
    Interrupted,
    TimedOut(Duration),
}

struct Spool {
    stdout: PathBuf,
    stderr: PathBuf,
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
) -> Result<Outcome> {
    use rustix::process::{Pid, Signal};
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        time::Instant,
    };

    let stdout = File::create(&spool.stdout)?;
    let stderr = File::create(&spool.stderr)?;

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(target_os = "macos")]
    command.process_group(0);

    #[cfg(target_os = "linux")]
    unsafe {
        command.pre_exec(|| {
            // Runs between fork and exec in the child; setpgid(0, 0) makes the
            // child lead its own process group so the whole workload can be
            // signaled. Failures here would leave the group unkillable, but
            // setpgid on a fresh child does not fail in practice.
            let _ = rustix::process::setpgid(None::<Pid>, None::<Pid>);
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run {:?}.", spec.program))?;
    // Both spawn paths above make the child a group leader, so the pgid is its pid.
    let group = Pid::from_raw(child.id() as i32).expect("child pid is positive");

    let deadline = timeout.map(|limit| Instant::now() + limit);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(Outcome::Succeeded);
            }
            return Ok(Outcome::Failed(status));
        }
        if interrupt.poll_read()? {
            kill_tree(&mut child, group);
            return Ok(Outcome::Interrupted);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            kill_tree(&mut child, group);
            return Ok(Outcome::TimedOut(timeout.expect("the deadline exists")));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child, group: rustix::process::Pid) {
    use rustix::process::Signal;

    let killed = match rustix::process::kill_process_group(group, Signal::KILL) {
        Ok(()) => Ok(()),
        // The group already exited on its own.
        Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error),
    };
    if killed.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(windows)]
fn platform_run(
    spec: &CommandSpec,
    interrupt: &Interrupt,
    timeout: Option<Duration>,
    spool: &Spool,
) -> Result<Outcome> {
    use crate::platform::{Wait, Workload};

    let mut workload = Workload::prepare_spooled(&spool.stdout, &spool.stderr)
        .context("Failed to prepare workload containment.")?
        .spawn(spec)?;

    match workload.wait(interrupt, timeout)? {
        Wait::Exited(status) if status.success() => Ok(Outcome::Succeeded),
        Wait::Exited(status) => Ok(Outcome::Failed(status)),
        Wait::Interrupted => {
            report_secondary(workload.terminate(), "Cleanup");
            Ok(Outcome::Interrupted)
        }
        Wait::TimedOut => {
            report_secondary(workload.terminate(), "Cleanup");
            Ok(Outcome::TimedOut(
                timeout.expect("a timed-out wait has a timeout"),
            ))
        }
    }
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
        let spec = CommandSpec::new(
            "cmd".into(),
            ["/c", "echo OUT-LINE & echo ERR-LINE 1>&2 & exit 7"]
                .iter()
                .map(std::convert::Into::into)
                .collect(),
            env::current_dir()?,
            Vec::new(),
        );
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
        let spec = CommandSpec::new(
            "cmd".into(),
            ["/c", "echo noisy & exit 0"]
                .iter()
                .map(std::convert::Into::into)
                .collect(),
            env::current_dir()?,
            Vec::new(),
        );
        execute(&spec, &interrupt, None, "quiet phase")?;
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
