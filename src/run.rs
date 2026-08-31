use crate::{
    Side,
    platform::{CommandSpec, Finished, Interrupt, Wait, Workload},
};
use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    time::{Duration, Instant},
};

/// A configured command that has not been bound to a working directory yet.
pub(crate) struct CommandTemplate {
    program: OsString,
    args: Vec<OsString>,
    working_directory: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl CommandTemplate {
    pub(crate) fn new(
        program: OsString,
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            program,
            args,
            working_directory,
            env,
        }
    }

    /// Resolves against a root directory into an executable spec.
    pub(crate) fn at(&self, root: &Path) -> CommandSpec {
        let cwd = self
            .working_directory
            .as_ref()
            .map_or_else(|| root.to_owned(), |path| root.join(path));
        CommandSpec::new(
            self.program.clone(),
            self.args.clone(),
            cwd,
            self.env.clone(),
        )
    }
}

/// Runs a command outside any measured interval: no timeout, no records.
/// The first Ctrl+C interrupts it like any other workload; cleanup still runs.
pub(crate) fn run_unmeasured(spec: &CommandSpec, interrupt: &Interrupt) -> Result<()> {
    let mut workload = Workload::prepare(spec)
        .context("Failed to prepare workload containment.")?
        .spawn()?;
    let outcome = workload.wait(interrupt, None);
    let finished = workload.finish();
    match outcome {
        Ok(Wait::Exited) => {
            let status = match finished.status {
                Ok(status) => status,
                Err(error) => {
                    report_secondary(finished.cleanup, "Cleanup");
                    return Err(error).context("Failed to reap the workload.");
                }
            };
            if status.success() {
                finished
                    .cleanup
                    .context("Failed to clean up workload containment.")
            } else {
                report_secondary(finished.cleanup, "Cleanup");
                bail!("{:?} failed with {status}.", spec.program);
            }
        }
        Ok(Wait::Interrupted) => {
            report_finished(finished);
            bail!("Interrupted.");
        }
        Ok(Wait::TimedOut) => {
            report_finished(finished);
            Err(anyhow::anyhow!(
                "the platform reported a timeout without a timeout"
            ))
        }
        Err(error) => {
            report_finished(finished);
            Err(error).with_context(|| format!("Failed to wait for {:?}.", spec.program))
        }
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
pub(crate) struct Measurement {
    pub(crate) elapsed: Duration,
    pub(crate) peak_memory: Option<Bytes>,
}

impl Measurement {
    pub(crate) fn elapsed(&self) -> Duration {
        self.elapsed
    }

    pub(crate) fn peak_memory(&self) -> Option<Bytes> {
        self.peak_memory
    }
}

pub(crate) struct BenchmarkLog<W> {
    writer: W,
    run: usize,
    runs: usize,
    started: Instant,
    progress: ProgressBar,
    interactive: bool,
    benchmark_name: Option<String>,
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
    pub(crate) fn new(writer: W, runs: usize, benchmark_name: Option<String>) -> Self {
        let interactive = io::stderr().is_terminal();
        let progress = if interactive {
            ProgressBar::new(runs as u64).with_style(
                ProgressStyle::with_template("{prefix:.cyan} {msg:.dim}")
                    .expect("The progress template is valid."),
            )
        } else {
            ProgressBar::hidden()
        };
        if let Some(name) = &benchmark_name {
            progress.set_prefix(name.clone());
        }
        Self {
            writer,
            run: 0,
            runs,
            started: Instant::now(),
            progress,
            interactive,
            benchmark_name,
        }
    }

    pub(crate) fn measure(
        &mut self,
        command: &CommandSpec,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
        side: Side,
    ) -> Result<Measurement> {
        self.starting(side);
        let prepared =
            Workload::prepare(command).context("Failed to prepare workload containment.")?;
        let started = Instant::now();
        let mut workload = prepared
            .spawn()
            .with_context(|| format!("Failed to run {:?}.", command.program))?;
        let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
        let outcome = match workload.wait(interrupt, remaining) {
            Ok(outcome) => outcome,
            Err(error) => {
                report_finished(workload.finish());
                return Err(error)
                    .with_context(|| format!("Failed to wait for {:?}.", command.program));
            }
        };
        let elapsed = started.elapsed();
        match outcome {
            Wait::Exited => {
                let finished = workload.finish();
                let status = match finished.status {
                    Ok(status) => status,
                    Err(error) => {
                        report_secondary(finished.cleanup, "Cleanup");
                        return Err(error).context("Failed to reap the benchmark workload.");
                    }
                };
                let logging = self.append(
                    side,
                    elapsed,
                    Some(status),
                    finished.peak_memory,
                    false,
                    false,
                );
                if !status.success() {
                    report_secondary(finished.cleanup, "Cleanup");
                    report_secondary(logging, "Logging");
                    bail!("The {side} benchmark failed with {status}.");
                }
                if let Err(error) = finished.cleanup {
                    report_secondary(logging, "Logging");
                    return Err(error).context("Failed to clean up benchmark containment.");
                }
                logging?;
                Ok(Measurement {
                    elapsed,
                    peak_memory: finished.peak_memory,
                })
            }
            outcome @ (Wait::Interrupted | Wait::TimedOut) => {
                let finished = workload.finish();
                let timed_out = matches!(outcome, Wait::TimedOut);
                let logging = self.append(side, elapsed, None, None, timed_out, !timed_out);
                report_finished(finished);
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
        } else if let Some(name) = &self.benchmark_name {
            eprintln!("{name}: {side} {}/{} benchmark", self.run + 1, self.runs);
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
        } else if let Some(name) = &self.benchmark_name {
            eprintln!("{name}: {side} {}/{} {}", self.run + 1, self.runs, phase);
        }
    }

    fn set_phase(&self, side: Side, run: usize, phase: &str) {
        let prefix = if let Some(name) = &self.benchmark_name {
            format!("{name} {side} {run}/{}", self.runs)
        } else {
            format!("{side} {run}/{}", self.runs)
        };
        self.progress.set_prefix(prefix);
        self.progress.set_message(phase.to_owned());
    }
}

fn report_secondary<T, E: std::fmt::Display>(result: std::result::Result<T, E>, label: &str) {
    if let Err(error) = result {
        eprintln!("{label} also failed: {error}");
    }
}

fn report_finished(finished: Finished) {
    report_secondary(finished.status, "Reaping");
    report_secondary(finished.cleanup, "Cleanup");
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

    #[test]
    fn the_log_matches_golden() -> Result<()> {
        let mut buffer = Vec::new();
        {
            let mut log = BenchmarkLog::new(&mut buffer, 2, None);
            log.append(
                Side::Baseline,
                Duration::from_secs_f64(1.5),
                Some(ExitStatus::default()),
                None,
                false,
                false,
            )?;
            log.append(
                Side::Candidate,
                Duration::from_secs_f64(1.5),
                Some(ExitStatus::default()),
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
