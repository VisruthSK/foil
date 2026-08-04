use crate::repetition::Side;
use crate::worktree::Worktree;

use anyhow::{Context, Result};
use serde_json::json;
use std::{
    ffi::OsString,
    io::Write,
    path::Path,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

pub struct RunCommand {
    program: OsString,
    args: Vec<OsString>,
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
    peak_sampled_memory: Bytes,
}

impl RunCommand {
    pub fn new(program: OsString, args: Vec<OsString>) -> Self {
        Self { program, args }
    }

    pub(crate) fn run_in(&self, working_dir: &Path) -> Result<Run> {
        let start = Instant::now();
        let captured = Command::new(&self.program)
            .args(&self.args)
            .current_dir(working_dir)
            .output()
            .with_context(|| format!("Failed to run {:?}.", self.program))?;
        let elapsed_time = start.elapsed();

        Ok(Run {
            output: RunOutput {
                exit_status: captured.status,
                elapsed_time,
                peak_sampled_memory: Bytes::ZERO,
            },
            stdout: captured.stdout,
            stderr: captured.stderr,
        })
    }
}

impl RunOutput {
    /// Assembles an output directly, for fixtures. Real runs come from [`RunCommand::run_in`].
    #[cfg(test)]
    pub(crate) fn new(
        exit_status: ExitStatus,
        elapsed_time: Duration,
        peak_sampled_memory: Bytes,
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

    pub fn peak_memory(&self) -> Bytes {
        self.peak_sampled_memory
    }
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
            eprintln!();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1.5 and 4096 are both exactly representable, so these round-trip cleanly.
    fn output() -> RunOutput {
        RunOutput::new(
            ExitStatus::default(),
            Duration::from_secs_f64(1.5),
            Bytes::new(4096),
        )
    }

    #[test]
    fn new_preserves_what_it_is_given() {
        let output = output();

        assert!(output.exit_status().success());
        assert_eq!(output.elapsed(), Duration::from_secs_f64(1.5));
        assert_eq!(output.peak_memory(), Bytes::new(4096));
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
                    "stdout": "first\n",
                    "stderr": "",
                }),
                json!({
                    "run": 2,
                    "side": "candidate",
                    "elapsed_seconds": 1.5,
                    "exit_code": 0,
                    "stdout": "second",
                    "stderr": "warning",
                }),
            ]
        );

        Ok(())
    }
}
