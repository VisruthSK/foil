use crate::{Metric, Pair, Posterior, Repetition, Revision, RunOrder, Shrinkage};

use anyhow::{Context, Result};
use serde_json::json;
use std::{
    ffi::OsString,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

pub struct Config<'a> {
    pub seed: u64,
    pub repetitions: usize,
    pub draws: usize,
    pub shrinkage: Shrinkage,
    pub baseline: &'a Revision,
    pub candidate: &'a Revision,
    pub setup: &'a [OsString],
    pub command: &'a [OsString],
    pub teardown: &'a [OsString],
}

fn utf8<'a>(name: &str, command: &'a [OsString]) -> Result<Vec<&'a str>> {
    command
        .iter()
        .map(|part| {
            part.to_str()
                .with_context(|| format!("The {name} command contains non-UTF-8 text."))
        })
        .collect()
}

pub fn write_config_json(path: &Path, config: &Config<'_>) -> Result<()> {
    let value = json!({
        "seed": config.seed,
        "repetitions": config.repetitions,
        "draws": config.draws,
        "shrinkage": config.shrinkage.get(),
        "b3_version": env!("CARGO_PKG_VERSION"),
        "baseline": {
            "revision": config.baseline.name(),
            "hash": config.baseline.hash(),
        },
        "candidate": {
            "revision": config.candidate.name(),
            "hash": config.candidate.hash(),
        },
        "setup": utf8("setup", config.setup)?,
        "command": utf8("benchmark", config.command)?,
        "teardown": utf8("teardown", config.teardown)?,
    });

    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, &value)?;
    writeln!(writer)?;
    writer.flush()?;

    Ok(())
}

pub struct MeasurementsCsv {
    writer: BufWriter<File>,
    rows: usize,
}

impl MeasurementsCsv {
    pub fn create(path: &Path) -> Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);

        writeln!(
            writer,
            "repetition,order,baseline_seconds,candidate_seconds"
        )?;
        writer.flush()?;

        Ok(Self { writer, rows: 0 })
    }

    pub fn append(&mut self, repetition: &Repetition) -> Result<()> {
        let Pair {
            baseline,
            candidate,
        } = repetition.outputs;
        let order = match repetition.order {
            RunOrder::BaselineFirst => "baseline_first",
            RunOrder::CandidateFirst => "candidate_first",
        };

        self.rows += 1;
        writeln!(
            self.writer,
            "{},{},{},{}",
            self.rows,
            order,
            baseline.elapsed().as_secs_f64(),
            candidate.elapsed().as_secs_f64(),
        )?;
        self.writer.flush()?;

        Ok(())
    }
}

pub fn write_posterior_csv<M: Metric>(path: &Path, posterior: &Posterior<M>) -> Result<()> {
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
    use crate::{Bytes, Repetition, RunOutput};
    use std::{fs::read_to_string, process::ExitStatus, time::Duration};
    use tempfile::tempdir;

    fn output(seconds: f64, bytes: u64) -> RunOutput {
        RunOutput::new(
            ExitStatus::default(),
            Duration::from_secs_f64(seconds),
            Some(Bytes::new(bytes)),
        )
    }

    fn repetition(index: usize) -> Repetition {
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
    fn measurements_csv_contains_complete_pairs() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("measurements.csv");

        let mut csv = MeasurementsCsv::create(&path)?;
        for index in 0..10 {
            csv.append(&repetition(index))?;
        }
        drop(csv);

        const EXPECTED: &str = concat!(
            "repetition,order,baseline_seconds,candidate_seconds\n",
            "1,baseline_first,1,0.5\n",
            "2,candidate_first,1,0.5\n",
            "3,baseline_first,1,0.5\n",
            "4,candidate_first,1,0.5\n",
            "5,baseline_first,1,0.5\n",
            "6,candidate_first,1,0.5\n",
            "7,baseline_first,1,0.5\n",
            "8,candidate_first,1,0.5\n",
            "9,baseline_first,1,0.5\n",
            "10,candidate_first,1,0.5\n",
        );

        assert_eq!(read_to_string(path)?, EXPECTED);

        Ok(())
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
