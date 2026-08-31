use crate::run::Measurement;
use crate::{Interval, Metric, Pair, Posterior, Repetition, Revision, RunOrder, Shrinkage};

use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

pub(crate) struct Config<'a> {
    pub(crate) seed: u64,
    pub(crate) repetitions: usize,
    pub(crate) block_size: usize,
    pub(crate) draws: usize,
    pub(crate) timeout_seconds: Option<u64>,
    pub(crate) isolate: bool,
    pub(crate) shrinkage: Shrinkage,
    pub(crate) intervals: &'a [Interval],
    pub(crate) working_directory: Option<&'a Path>,
    pub(crate) baseline: &'a Revision,
    pub(crate) candidate: &'a Revision,
    pub(crate) suite_lifecycle: LifecycleConfig<'a>,
    pub(crate) benchmark_lifecycle: LifecycleConfig<'a>,
    pub(crate) command: &'a [OsString],
}

pub(crate) struct LifecycleConfig<'a> {
    pub(crate) startup: &'a [OsString],
    pub(crate) startup_each_run: &'a [OsString],
    pub(crate) teardown_each_run: &'a [OsString],
    pub(crate) teardown: &'a [OsString],
}

fn utf8(name: &str, command: &[OsString]) -> Result<Vec<String>> {
    command
        .iter()
        .map(|part| {
            part.to_str()
                .map(|s| s.to_owned())
                .with_context(|| format!("The {name} command contains non-UTF-8 text."))
        })
        .collect()
}

#[derive(Serialize)]
struct RevisionDto {
    revision: String,
    hash: String,
}

#[derive(Serialize)]
struct LifecycleDto {
    startup: Vec<String>,
    startup_each_run: Vec<String>,
    teardown_each_run: Vec<String>,
    teardown: Vec<String>,
}

#[derive(Serialize)]
struct ConfigDto<'a> {
    seed: u64,
    repetitions: usize,
    block_size: usize,
    draws: usize,
    timeout_seconds: Option<u64>,
    isolate: bool,
    shrinkage: f64,
    intervals: Vec<f64>,
    working_directory: Option<&'a Path>,
    foil_version: &'static str,
    baseline: RevisionDto,
    candidate: RevisionDto,
    suite_lifecycle: LifecycleDto,
    benchmark_lifecycle: LifecycleDto,
    command: Vec<String>,
}

pub(crate) fn write_config_json(path: &Path, config: &Config<'_>) -> Result<()> {
    let dto = ConfigDto {
        seed: config.seed,
        repetitions: config.repetitions,
        block_size: config.block_size,
        draws: config.draws,
        timeout_seconds: config.timeout_seconds,
        isolate: config.isolate,
        shrinkage: config.shrinkage.get(),
        intervals: config
            .intervals
            .iter()
            .map(|interval| interval.percent() / 100.0)
            .collect(),
        working_directory: config.working_directory,
        foil_version: env!("CARGO_PKG_VERSION"),
        baseline: RevisionDto {
            revision: config.baseline.name().to_owned(),
            hash: config.baseline.hash().to_owned(),
        },
        candidate: RevisionDto {
            revision: config.candidate.name().to_owned(),
            hash: config.candidate.hash().to_owned(),
        },
        suite_lifecycle: LifecycleDto {
            startup: utf8("suite startup", config.suite_lifecycle.startup)?,
            startup_each_run: utf8(
                "suite startup-each-run",
                config.suite_lifecycle.startup_each_run,
            )?,
            teardown_each_run: utf8(
                "suite teardown-each-run",
                config.suite_lifecycle.teardown_each_run,
            )?,
            teardown: utf8("suite teardown", config.suite_lifecycle.teardown)?,
        },
        benchmark_lifecycle: LifecycleDto {
            startup: utf8("benchmark startup", config.benchmark_lifecycle.startup)?,
            startup_each_run: utf8(
                "benchmark startup-each-run",
                config.benchmark_lifecycle.startup_each_run,
            )?,
            teardown_each_run: utf8(
                "benchmark teardown-each-run",
                config.benchmark_lifecycle.teardown_each_run,
            )?,
            teardown: utf8("benchmark teardown", config.benchmark_lifecycle.teardown)?,
        },
        command: utf8("benchmark", config.command)?,
    };

    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, &dto)?;
    writeln!(writer)?;
    writer.flush()?;

    Ok(())
}

pub(crate) struct MeasurementsCsv {
    writer: BufWriter<File>,
    rows: usize,
}

impl MeasurementsCsv {
    pub(crate) fn create(path: &Path) -> Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);

        writeln!(
            writer,
            "repetition,order,baseline_seconds,candidate_seconds"
        )?;
        writer.flush()?;

        Ok(Self { writer, rows: 0 })
    }

    pub(crate) fn append(&mut self, repetition: &Repetition<Measurement>) -> Result<()> {
        let Pair {
            baseline,
            candidate,
        } = repetition.outputs;
        let order = match repetition.order {
            RunOrder::BaselineFirst => "baseline_first",
            RunOrder::CandidateFirst => "candidate_first",
        };

        let row = self.rows + 1;
        writeln!(
            self.writer,
            "{},{},{},{}",
            row,
            order,
            baseline.elapsed.as_secs_f64(),
            candidate.elapsed.as_secs_f64(),
        )?;
        self.writer.flush()?;
        self.rows = row;

        Ok(())
    }
}

pub(crate) fn write_posterior_csv<M: Metric>(path: &Path, posterior: &Posterior<M>) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    let unit = M::BASE_UNIT;

    writeln!(writer, "baseline_{unit},candidate_{unit}")?;

    for draw in posterior.draws() {
        writeln!(writer, "{},{}", draw.baseline.base(), draw.candidate.base())?;
    }

    writer.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bytes, Measurement, Repetition};
    use std::{fs::read_to_string, time::Duration};
    use tempfile::tempdir;

    fn output(seconds: f64, bytes: u64) -> Measurement {
        Measurement {
            elapsed: Duration::from_secs_f64(seconds),
            peak_memory: Some(Bytes::new(bytes)),
        }
    }

    fn repetition(index: usize) -> Repetition<Measurement> {
        Repetition {
            outputs: Pair {
                baseline: output(1.0, 1_000),
                candidate: output(0.5, 2_000),
            },
            order: if index % 2 == 0 {
                RunOrder::BaselineFirst
            } else {
                RunOrder::CandidateFirst
            },
        }
    }

    #[test]
    fn each_appended_repetition_reaches_disk_immediately() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("measurements.csv");

        let mut csv = MeasurementsCsv::create(&path)?;
        assert_eq!(
            read_to_string(&path)?,
            "repetition,order,baseline_seconds,candidate_seconds\n"
        );

        csv.append(&repetition(0))?;
        assert_eq!(read_to_string(&path)?.lines().count(), 2);

        Ok(())
    }
}
