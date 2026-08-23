use crate::{
    Side, Worktree,
    platform::{CommandSpec, Interrupt, Wait, Workload},
};
use anyhow::{Context, Result, bail, ensure};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub(crate) struct RunCommand {
    program: OsString,
    args: Vec<OsString>,
    working_directory: Option<PathBuf>,
    env: Vec<(String, String)>,
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
pub(crate) struct RunOutput {
    exit_status: ExitStatus,
    elapsed_time: Duration,
    peak_memory: Option<Bytes>,
}

impl RunCommand {
    pub(crate) fn new(
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
        }
    }

    pub(crate) fn run_once_in(&self, worktree: &Worktree, label: &str) -> Result<()> {
        self.run_once_at(worktree.path(), label)
    }

    pub(crate) fn run_once_at(&self, directory: &Path, label: &str) -> Result<()> {
        let cwd = self
            .working_directory
            .as_ref()
            .map_or_else(|| directory.to_owned(), |path| directory.join(path));
        let output = Command::new(&self.program)
            .args(&self.args)
            .current_dir(cwd)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("Failed to run {:?}.", self.program))?;
        ensure!(
            output.status.success(),
            "{:?} failed with {}.{}",
            self.program,
            output.status,
            display_output(label, &output.stdout, &output.stderr)
        );
        Ok(())
    }
}

impl RunOutput {
    pub(crate) fn measurement(elapsed_time: Duration) -> Self {
        Self {
            exit_status: ExitStatus::default(),
            elapsed_time,
            peak_memory: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        exit_status: ExitStatus,
        elapsed_time: Duration,
        peak_memory: Option<Bytes>,
    ) -> Self {
        Self {
            exit_status,
            elapsed_time,
            peak_memory,
        }
    }

    #[cfg(test)]
    pub(crate) fn exit_status(&self) -> ExitStatus {
        self.exit_status
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed_time
    }

    pub(crate) fn peak_memory(&self) -> Option<Bytes> {
        self.peak_memory
    }
}

fn display_output(label: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = String::new();
    for (stream, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        if !bytes.is_empty() {
            output.push_str(&format!(
                "\n[{label} {stream}]\n{}",
                String::from_utf8_lossy(bytes).trim_end()
            ));
        }
    }
    output
}

pub(crate) struct BenchmarkLog<W> {
    writer: W,
    run: usize,
    runs: usize,
    started: Instant,
    progress: ProgressBar,
    interactive: bool,
}

#[derive(Serialize)]
struct BenchmarkEntry<'a> {
    run: usize,
    side: &'a str,
    elapsed_seconds: f64,
    exit_code: Option<i32>,
    peak_memory_bytes: Option<u64>,
    timed_out: bool,
    interrupted: bool,
}

impl<W: Write> BenchmarkLog<W> {
    pub(crate) fn new(writer: W, runs: usize) -> Self {
        let interactive = io::stderr().is_terminal();
        let progress = if interactive {
            ProgressBar::new(runs as u64).with_style(
                ProgressStyle::with_template("{prefix:.cyan} {msg:.dim}")
                    .expect("The progress template is valid."),
            )
        } else {
            ProgressBar::hidden()
        };
        Self {
            writer,
            run: 0,
            runs,
            started: Instant::now(),
            progress,
            interactive,
        }
    }

    pub(crate) fn measure(
        &mut self,
        command: &CommandSpec,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
        side: Side,
    ) -> Result<RunOutput> {
        self.starting(side);
        let prepared = Workload::prepare().context("Failed to prepare workload containment.")?;
        let started = Instant::now();
        let mut workload = prepared
            .spawn(command)
            .with_context(|| format!("Failed to run {:?}.", command.program))?;
        let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
        let outcome = match workload.wait(interrupt, remaining) {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Err(cleanup) = workload.terminate() {
                    eprintln!("Cleanup also failed: {cleanup}");
                }
                return Err(error)
                    .with_context(|| format!("Failed to wait for {:?}.", command.program));
            }
        };
        let elapsed = started.elapsed();
        match outcome {
            Wait::Exited(status) => {
                let output = RunOutput {
                    exit_status: status,
                    elapsed_time: elapsed,
                    peak_memory: None,
                };
                let cleanup = workload.terminate();
                let logging = self.append(side, elapsed, Some(status), None, false, false);
                if !status.success() {
                    report_secondary(cleanup, "Cleanup");
                    report_secondary(logging, "Logging");
                    bail!("The {side} benchmark failed with {status}.");
                }
                if let Err(error) = cleanup {
                    report_secondary(logging, "Logging");
                    return Err(error).context("Failed to clean up the workload.");
                }
                logging?;
                Ok(output)
            }
            outcome @ (Wait::Interrupted | Wait::TimedOut) => {
                let cleanup = workload.terminate();
                let timed_out = matches!(outcome, Wait::TimedOut);
                let logging = self.append(side, elapsed, None, None, timed_out, !timed_out);
                report_secondary(cleanup, "Cleanup");
                report_secondary(logging, "Logging");
                if !timed_out {
                    bail!("Interrupted.");
                }
                bail!(
                    "{:?} timed out after {:?}.",
                    command.program,
                    timeout.unwrap()
                );
            }
        }
    }

    fn starting(&self, side: Side) {
        if self.interactive {
            self.set_phase(side, self.run + 1, &format!("benchmark{}", self.eta()));
        } else {
            eprintln!("{side} {}/{} benchmark", self.run + 1, self.runs);
        }
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

    fn append(
        &mut self,
        side: Side,
        elapsed: Duration,
        exit_status: Option<ExitStatus>,
        peak_memory: Option<Bytes>,
        timed_out: bool,
        interrupted: bool,
    ) -> Result<()> {
        self.run += 1;
        self.progress.inc(1);
        let side = match side {
            Side::Baseline => "baseline",
            Side::Candidate => "candidate",
        };
        serde_json::to_writer(
            &mut self.writer,
            &BenchmarkEntry {
                run: self.run,
                side,
                elapsed_seconds: elapsed.as_secs_f64(),
                exit_code: exit_status.and_then(|status| status.code()),
                peak_memory_bytes: peak_memory.map(Bytes::get),
                timed_out,
                interrupted,
            },
        )?;
        writeln!(self.writer)?;
        self.writer.flush()?;
        Ok(())
    }

    pub(crate) fn phase(&self, side: Side, phase: &'static str) {
        if self.interactive {
            self.set_phase(side, self.run + 1, phase);
        }
    }

    fn set_phase(&self, side: Side, run: usize, phase: &str) {
        self.progress
            .set_prefix(format!("{side} {run}/{}", self.runs));
        self.progress.set_message(phase.to_owned());
    }
}

fn report_secondary<E: std::fmt::Display>(result: std::result::Result<(), E>, label: &str) {
    if let Err(error) = result {
        eprintln!("{label} also failed: {error}");
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
    use std::process::ExitStatus;

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

    #[test]
    fn the_log_matches_golden() -> Result<()> {
        let mut buffer = Vec::new();
        {
            let mut log = BenchmarkLog::new(&mut buffer, 2);
            log.append(
                Side::Baseline,
                output().elapsed(),
                Some(output().exit_status()),
                None,
                false,
                false,
            )?;
            log.append(
                Side::Candidate,
                output().elapsed(),
                Some(output().exit_status()),
                None,
                false,
                false,
            )?;
        }
        let entries: Vec<serde_json::Value> = String::from_utf8(buffer)?
            .lines()
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["exit_code"], 0);
        assert_eq!(entries[0]["timed_out"], false);
        assert_eq!(entries[0]["interrupted"], false);
        assert!(entries[0].get("stdout").is_none());
        Ok(())
    }
}
