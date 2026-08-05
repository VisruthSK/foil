use crate::repetition::Side;
use crate::worktree::Worktree;

use anyhow::{Context, Result, bail, ensure};
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

    pub(crate) fn run_in(&self, worktree: &Path) -> Result<Run> {
        let working_dir = match &self.working_directory {
            Some(directory) => worktree.join(directory),
            None => worktree.to_path_buf(),
        };

        let start = Instant::now();
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .current_dir(working_dir)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to run {:?}.", self.program))?;
        let stdout = drain(child.stdout.take().expect("Stdout is piped."));
        let stderr = drain(child.stderr.take().expect("Stderr is piped."));

        let exit_status = match self.timeout {
            None => child.wait()?,
            Some(limit) => match child.wait_timeout(limit)? {
                Some(status) => status,
                None => {
                    child.kill()?;
                    child.wait()?;
                    bail!("{:?} timed out after {limit:?}.", self.program);
                }
            },
        };
        let elapsed_time = start.elapsed();

        Ok(Run {
            output: RunOutput {
                exit_status,
                elapsed_time,
                peak_sampled_memory: None,
            },
            stdout: stdout.join().expect("The stdout reader does not panic.")?,
            stderr: stderr.join().expect("The stderr reader does not panic.")?,
        })
    }

    pub fn run_once_in(&self, worktree: &Worktree) -> Result<()> {
        let run = self.run_in(worktree.path())?;

        ensure!(
            run.output.exit_status.success(),
            "{:?} failed with {}.\n{}",
            self.program,
            run.output.exit_status,
            String::from_utf8_lossy(&run.stderr).trim_end()
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

pub(crate) struct Run {
    output: RunOutput,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub struct BenchmarkLog<W> {
    writer: W,
    run: usize,
    runs: usize,
}

impl<W: Write> BenchmarkLog<W> {
    pub fn new(writer: W, runs: usize) -> Self {
        Self {
            writer,
            run: 0,
            runs,
        }
    }

    /// Runs the benchmark in one side's worktree and records what it printed.
    pub fn measure(
        &mut self,
        benchmark: &RunCommand,
        side: Side,
        worktree: &Worktree,
    ) -> Result<RunOutput> {
        self.starting();

        let run = benchmark.run_in(worktree.path())?;

        self.append(side, &run)?;

        Ok(run.output)
    }

    fn starting(&self) {
        eprint!("\rBenchmarking run {}/{}", self.run + 1, self.runs);
    }

    fn append(&mut self, side: Side, run: &Run) -> Result<()> {
        self.run += 1;

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
}

impl<W> Drop for BenchmarkLog<W> {
    fn drop(&mut self) {
        if self.run > 0 {
            let width = format!("Benchmarking run {0}/{0}", self.runs).len();
            eprint!("\r{}\r", " ".repeat(width));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
            .run_in(directory.path())
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
            .run_in(directory.path())?;

        assert!(run.output.exit_status.success());
        assert!(!run.stdout.is_empty());

        Ok(())
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
