use crate::repetition::Side;
use crate::worktree::Worktree;

use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    io::{self, Write},
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

        self.append(side, worktree.revision(), &run)?;

        Ok(run.output)
    }

    fn starting(&self) {
        eprint!("\rBenchmarking run {}/{}", self.run + 1, self.runs);
    }

    fn append(&mut self, side: Side, revision: &str, run: &Run) -> io::Result<()> {
        self.run += 1;

        writeln!(
            self.writer,
            "run {}/{}  {side} ({revision})",
            self.run, self.runs
        )?;

        self.append_stream(&run.stdout)?;

        if !run.stderr.is_empty() {
            writeln!(self.writer, "--- stderr")?;
            self.append_stream(&run.stderr)?;
        }

        writeln!(self.writer, "{:.3?}", run.output.elapsed_time)?;
        writeln!(self.writer, "{}", run.output.exit_status)?;
        writeln!(self.writer)?;

        self.writer.flush()
    }

    fn append_stream(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;

        if bytes.last().is_some_and(|&byte| byte != b'\n') {
            writeln!(self.writer)?;
        }

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

    /// Two runs' worth of log, pinned exactly. The second prints to stderr and ends
    /// without a newline, which is what would otherwise run its output into the
    /// closing line.
    #[test]
    fn the_log_matches_golden() -> io::Result<()> {
        const EXPECTED: &str = concat!(
            "run 1/2  baseline (main)\n",
            "first\n",
            "1.500s\n",
            "exit code: 0\n",
            "\n",
            "run 2/2  candidate (HEAD)\n",
            "second\n",
            "--- stderr\n",
            "warning\n",
            "1.500s\n",
            "exit code: 0\n",
            "\n",
        );

        let mut buffer = Vec::new();

        {
            let mut log = BenchmarkLog::new(&mut buffer, 2);

            log.append(Side::Baseline, "main", &run("first\n", ""))?;
            log.append(Side::Candidate, "HEAD", &run("second", "warning"))?;
        }

        assert_eq!(
            String::from_utf8(buffer).expect("The test writes UTF-8."),
            EXPECTED
        );

        Ok(())
    }
}
