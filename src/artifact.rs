use crate::{Metric, Pair, Posterior, Repetitions, Revision, RunOrder};

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
    pub baseline: &'a Revision,
    pub candidate: &'a Revision,
    pub command: &'a [OsString],
}

pub fn write_config_json(path: &Path, config: &Config<'_>) -> Result<()> {
    let command: Vec<_> = config
        .command
        .iter()
        .map(|part| {
            part.to_str()
                .context("Benchmark command contains non-UTF-8 text.")
        })
        .collect::<Result<_>>()?;
    let value = json!({
        "seed": config.seed,
        "repetitions": config.repetitions,
        "b3_version": env!("CARGO_PKG_VERSION"),
        "baseline": {
            "revision": config.baseline.name(),
            "hash": config.baseline.hash(),
        },
        "candidate": {
            "revision": config.candidate.name(),
            "hash": config.candidate.hash(),
        },
        "command": command,
    });

    let mut writer = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, &value)?;
    writeln!(writer)?;
    writer.flush()?;

    Ok(())
}

pub fn write_measurements_csv(path: &Path, repetitions: &Repetitions) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);

    writeln!(
        writer,
        "repetition,order,baseline_seconds,candidate_seconds"
    )?;

    for (index, repetition) in repetitions.iter().enumerate() {
        let Pair {
            baseline,
            candidate,
        } = repetition.outputs;
        let order = match repetition.order {
            RunOrder::BaselineFirst => "baseline_first",
            RunOrder::CandidateFirst => "candidate_first",
        };

        writeln!(
            writer,
            "{},{},{},{}",
            index + 1,
            order,
            baseline.elapsed().as_secs_f64(),
            candidate.elapsed().as_secs_f64(),
        )?;
    }

    writer.flush()?;

    Ok(())
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

    #[test]
    fn measurements_csv_contains_complete_pairs() -> Result<()> {
        let repetitions: Repetitions = (0..10)
            .map(|index| Repetition {
                outputs: Pair {
                    baseline: output(1.0, 1_000),
                    candidate: output(0.5, 2_000),
                },
                order: if index % 2 == 0 {
                    RunOrder::BaselineFirst
                } else {
                    RunOrder::CandidateFirst
                },
            })
            .collect::<Vec<_>>()
            .try_into()?;
        let directory = tempdir()?;
        let path = directory.path().join("measurements.csv");

        write_measurements_csv(&path, &repetitions)?;

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
}
