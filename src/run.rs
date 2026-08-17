use crate::repetition::Side;
use crate::worktree::Worktree;

use anyhow::{Context, Result, bail, ensure};
use command_group::{CommandGroup, GroupChild};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

pub struct RunCommand {
    program: OsString,
    args: Vec<OsString>,
    working_directory: Option<PathBuf>,
    env: Vec<(String, String)>,
    timeout: Option<Duration>,
}

struct ProcessTree(GroupChild);

impl ProcessTree {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command.group_spawn().map(Self)
    }

    fn child(&mut self) -> &mut std::process::Child {
        self.0.inner()
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait()
    }

    fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        self.child().wait_timeout(timeout)
    }

    fn kill(&mut self) -> io::Result<()> {
        self.0.kill()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bytes(u64);

impl Bytes {
    pub const ZERO: Self = Self(0);

    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunOutput {
    exit_status: ExitStatus,
    elapsed_time: Duration,
    peak_sampled_memory: Option<Bytes>,
}

impl RunCommand {
    pub fn new(
        program: OsString,
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            program,
            args,
            working_directory,
            env,
            timeout: None,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    pub(crate) fn run_in(&self, worktree: &Path, label: &str) -> Result<Run> {
        let working_dir = match &self.working_directory {
            Some(directory) => worktree.join(directory),
            None => worktree.to_path_buf(),
        };

        let start = Instant::now();
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(working_dir)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = ProcessTree::spawn(&mut command)
            .with_context(|| format!("Failed to run {:?}.", self.program))?;
        let stdout = drain(child.child().stdout.take().expect("Stdout is piped."));
        let stderr = drain(child.child().stderr.take().expect("Stderr is piped."));

        let failed_waiting = || format!("Failed to wait for {:?}.", self.program);
        let mut timed_out = None;
        let exit_status = match self.timeout {
            None => child.wait().with_context(failed_waiting)?,
            Some(limit) => match child.wait_timeout(limit).with_context(failed_waiting)? {
                Some(status) => status,
                None => {
                    child.kill().with_context(|| {
                        format!("Failed to kill {:?} and its children.", self.program)
                    })?;
                    timed_out = Some(limit);
                    child.wait().with_context(failed_waiting)?
                }
            },
        };
        let elapsed_time = start.elapsed();
        let stdout = stdout.join().expect("The stdout reader does not panic.")?;
        let stderr = stderr.join().expect("The stderr reader does not panic.")?;

        let run = Run {
            output: RunOutput {
                exit_status,
                elapsed_time,
                peak_sampled_memory: None,
            },
            stdout,
            stderr,
        };

        if let Some(limit) = timed_out {
            bail!(
                "{:?} timed out after {limit:?}.{}",
                self.program,
                display_output(label, &run)
            );
        }

        Ok(run)
    }

    pub fn run_once_in(&self, worktree: &Worktree, label: &str) -> Result<()> {
        self.run_once_at(worktree.path(), label)
    }

    pub fn run_once_at(&self, directory: &Path, label: &str) -> Result<()> {
        let run = self.run_in(directory, label)?;

        ensure!(
            run.output.exit_status.success(),
            "{:?} failed with {}.{}",
            self.program,
            run.output.exit_status,
            display_output(label, &run)
        );

        Ok(())
    }
}

impl RunOutput {
    /// Assembles an output directly, for fixtures. Real runs come from [`RunCommand::run_in`].
    #[cfg(test)]
    pub(crate) fn new(
        exit_status: ExitStatus,
        elapsed_time: Duration,
        peak_sampled_memory: Option<Bytes>,
    ) -> Self {
        Self {
            exit_status,
            elapsed_time,
            peak_sampled_memory,
        }
    }

    pub fn exit_status(&self) -> ExitStatus {
        self.exit_status
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed_time
    }

    pub fn peak_memory(&self) -> Option<Bytes> {
        self.peak_sampled_memory
    }
}

fn drain(mut stream: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;

        Ok(bytes)
    })
}

fn display_output(label: &str, run: &Run) -> String {
    let mut output = String::new();
    for (stream, bytes) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        if !bytes.is_empty() {
            output.push_str(&format!(
                "\n[{label} {stream}]\n{}",
                String::from_utf8_lossy(bytes).trim_end()
            ));
        }
    }
    output
}

pub(crate) struct Run {
    output: RunOutput,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub struct BenchmarkLog<W> {
    writer: W,
    run: usize,
    runs: usize,
    started: Instant,
    progress: ProgressBar,
}

impl<W: Write> BenchmarkLog<W> {
    pub fn new(writer: W, runs: usize) -> Self {
        let progress = ProgressBar::new(runs as u64).with_style(
            ProgressStyle::with_template("{prefix:.cyan} {msg:.dim}")
                .expect("The progress template is valid."),
        );
        Self {
            writer,
            run: 0,
            runs,
            started: Instant::now(),
            progress,
        }
    }

    /// Runs the benchmark in one side's worktree and records what it printed.
    pub fn measure(
        &mut self,
        benchmark: &RunCommand,
        side: Side,
        worktree: &Worktree,
    ) -> Result<RunOutput> {
        self.starting(side);

        let label = format!("{side} benchmark");
        let run = benchmark.run_in(worktree.path(), &label)?;

        self.append(side, &run)?;
        ensure!(
            run.output.exit_status.success(),
            "The {side} benchmark failed with {}.{}",
            run.output.exit_status,
            display_output(&label, &run)
        );

        Ok(run.output)
    }

    fn starting(&self, side: Side) {
        self.set_phase(side, self.run + 1, &format!("benchmark{}", self.eta()));
    }

    fn eta(&self) -> String {
        if self.run == 0 {
            return String::new();
        }

        let per_run = self.started.elapsed().as_secs_f64() / self.run as f64;
        let seconds = (per_run * (self.runs - self.run) as f64).round() as u64;

        if seconds >= 60 {
            format!(" (ETA {}m{:02}s)", seconds / 60, seconds % 60)
        } else {
            format!(" (ETA {seconds}s)")
        }
    }

    fn append(&mut self, side: Side, run: &Run) -> Result<()> {
        self.run += 1;
        self.progress.inc(1);

        let entry = json!({
            "run": self.run,
            "side": side.to_string(),
            "elapsed_seconds": run.output.elapsed_time.as_secs_f64(),
            "exit_code": run.output.exit_status.code(),
            "peak_memory_bytes": run.output.peak_sampled_memory.map(Bytes::get),
            "stdout": String::from_utf8_lossy(&run.stdout),
            "stderr": String::from_utf8_lossy(&run.stderr),
        });
        serde_json::to_writer(&mut self.writer, &entry)?;
        writeln!(self.writer)?;

        self.writer.flush()?;

        Ok(())
    }

    pub fn progress(&self) -> ProgressBar {
        self.progress.clone()
    }

    pub fn next_run(&self) -> usize {
        self.run + 1
    }

    fn set_phase(&self, side: Side, run: usize, phase: &str) {
        self.progress
            .set_prefix(format!("{side} {run}/{}", self.runs));
        self.progress.set_message(phase.to_owned());
    }
}

impl<W> Drop for BenchmarkLog<W> {
    fn drop(&mut self) {
        self.progress.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};
    use tempfile::tempdir;

    const TIMEOUT_MARKER: &str = "B3_TIMEOUT_TEST_MARKER";

    fn command(program: &str, args: &[&str]) -> RunCommand {
        RunCommand::new(
            program.into(),
            args.iter().map(Into::into).collect(),
            None,
            Vec::new(),
        )
    }

    #[test]
    fn a_run_that_exceeds_the_timeout_fails() -> Result<()> {
        let directory = tempdir()?;
        let slow = if cfg!(windows) {
            command("ping", &["-n", "31", "127.0.0.1"])
        } else {
            command("sleep", &["30"])
        };

        let error = slow
            .with_timeout(Some(Duration::from_millis(100)))
            .run_in(directory.path(), "test benchmark")
            .err()
            .context("The run should time out.")?;

        assert!(error.to_string().contains("timed out"), "{error}");

        Ok(())
    }

    #[test]
    fn a_run_within_the_timeout_succeeds() -> Result<()> {
        let directory = tempdir()?;

        let run = command("git", &["--version"])
            .with_timeout(Some(Duration::from_secs(60)))
            .run_in(directory.path(), "test benchmark")?;

        assert!(run.output.exit_status.success());
        assert!(!run.stdout.is_empty());

        Ok(())
    }

    #[test]
    fn a_timeout_kills_descendant_processes() -> Result<()> {
        let directory = tempdir()?;
        let marker = directory.path().join("descendant-finished");
        let parent = RunCommand::new(
            env::current_exe()?.into_os_string(),
            [
                "--exact",
                "run::tests::timeout_parent",
                "--ignored",
                "--nocapture",
            ]
            .map(OsString::from)
            .to_vec(),
            None,
            vec![(
                TIMEOUT_MARKER.to_owned(),
                marker.to_string_lossy().into_owned(),
            )],
        );

        let error = parent
            .with_timeout(Some(Duration::from_millis(100)))
            .run_in(directory.path(), "test benchmark")
            .err()
            .context("The parent should time out.")?;
        assert!(error.to_string().contains("timed out"), "{error}");

        thread::sleep(Duration::from_secs(1));
        assert!(
            !marker.exists(),
            "The descendant survived its parent's timeout."
        );

        Ok(())
    }

    #[test]
    #[ignore]
    fn timeout_parent() {
        let status = Command::new(env::current_exe().expect("The test executable exists."))
            .args([
                "--exact",
                "run::tests::timeout_descendant",
                "--ignored",
                "--nocapture",
            ])
            .env(
                TIMEOUT_MARKER,
                env::var_os(TIMEOUT_MARKER).expect("The marker path is configured."),
            )
            .status()
            .expect("The descendant starts.");
        assert!(status.success(), "The descendant failed with {status}.");
    }

    #[test]
    #[ignore]
    fn timeout_descendant() {
        thread::sleep(Duration::from_millis(750));
        fs::write(
            env::var_os(TIMEOUT_MARKER).expect("The marker path is configured."),
            "finished",
        )
        .expect("The descendant writes its marker.");
    }

    /// 1.5 is exactly representable, so it round-trips cleanly.
    fn output() -> RunOutput {
        RunOutput::new(ExitStatus::default(), Duration::from_secs_f64(1.5), None)
    }

    #[test]
    fn new_preserves_what_it_is_given() {
        let output = output();

        assert!(output.exit_status().success());
        assert_eq!(output.elapsed(), Duration::from_secs_f64(1.5));
        assert_eq!(output.peak_memory(), None);
    }

    fn run(stdout: &str, stderr: &str) -> Run {
        Run {
            output: output(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Every line is a complete JSON object, including embedded newlines.
    #[test]
    fn the_log_matches_golden() -> Result<()> {
        let mut buffer = Vec::new();

        {
            let mut log = BenchmarkLog::new(&mut buffer, 2);

            log.append(Side::Baseline, &run("first\n", ""))?;
            log.append(Side::Candidate, &run("second", "warning"))?;
        }

        let entries: Vec<serde_json::Value> = String::from_utf8(buffer)
            .expect("The test writes UTF-8.")
            .lines()
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
        assert_eq!(
            entries,
            [
                json!({
                    "run": 1,
                    "side": "baseline",
                    "elapsed_seconds": 1.5,
                    "exit_code": 0,
                    "peak_memory_bytes": null,
                    "stdout": "first\n",
                    "stderr": "",
                }),
                json!({
                    "run": 2,
                    "side": "candidate",
                    "elapsed_seconds": 1.5,
                    "exit_code": 0,
                    "peak_memory_bytes": null,
                    "stdout": "second",
                    "stderr": "warning",
                }),
            ]
        );

        Ok(())
    }
}
