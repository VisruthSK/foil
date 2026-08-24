use crate::run::RunOutput;
use crate::{
    Interval, Pair, Posterior, Repetition, Repetitions, RunOrder, Shrinkage, Summary, Time,
};
use anyhow::{Context, Result, ensure};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{
    fs::File,
    io::{BufRead, BufReader},
    num::NonZeroUsize,
    path::Path,
    time::Duration,
};

/// A sampled posterior and its requested credible-interval summary.
pub struct Analysis {
    pub posterior: Posterior<Time>,
    pub summary: Summary<Time>,
}

/// Repeats the production analysis from a `measurements.csv` artifact.
pub fn analyze_measurements(
    path: &Path,
    seed: u64,
    draws: NonZeroUsize,
    shrinkage: Shrinkage,
    intervals: &[Interval],
) -> Result<Analysis> {
    analyze(&read_measurements(path)?, seed, draws, shrinkage, intervals)
}

pub(crate) fn analyze(
    repetitions: &Repetitions,
    seed: u64,
    draws: NonZeroUsize,
    shrinkage: Shrinkage,
    intervals: &[Interval],
) -> Result<Analysis> {
    analyze_checked(repetitions, seed, draws, shrinkage, intervals, || Ok(()))
}

/// Analyzes validated repetitions, checking for cancellation between draws.
pub(crate) fn analyze_checked(
    repetitions: &Repetitions,
    seed: u64,
    draws: NonZeroUsize,
    shrinkage: Shrinkage,
    intervals: &[Interval],
    check: impl FnMut() -> Result<()>,
) -> Result<Analysis> {
    let posterior = Posterior::bootstrap_checked(
        repetitions,
        draws,
        shrinkage,
        &mut Xoshiro256PlusPlus::seed_from_u64(seed),
        check,
    )?;
    let summary = posterior.summarize(intervals)?;
    Ok(Analysis { posterior, summary })
}

fn read_measurements(path: &Path) -> Result<Repetitions> {
    let mut lines = BufReader::new(
        File::open(path).with_context(|| format!("Failed to read {}.", path.display()))?,
    )
    .lines();
    ensure!(
        lines.next().transpose()?.as_deref()
            == Some("repetition,order,baseline_seconds,candidate_seconds"),
        "{} has an invalid measurements header.",
        path.display()
    );

    lines
        .enumerate()
        .map(|(index, line)| {
            let line_number = index + 2;
            let line = line.with_context(|| format!("Failed to read line {line_number}."))?;
            let mut fields = line.split(',');
            let repetition = fields.next().unwrap_or_default();
            let order = fields.next().unwrap_or_default();
            let baseline = fields.next().unwrap_or_default();
            let candidate = fields.next().unwrap_or_default();
            ensure!(
                fields.next().is_none() && !candidate.is_empty(),
                "Invalid measurements line {line_number}."
            );
            ensure!(
                repetition.parse::<usize>()? == index + 1,
                "Measurements repetitions must be sequential."
            );
            let order = match order {
                "baseline_first" => RunOrder::BaselineFirst,
                "candidate_first" => RunOrder::CandidateFirst,
                order => anyhow::bail!("Unknown run order `{order}`."),
            };
            let elapsed = |field: &str| -> Result<RunOutput> {
                let seconds: f64 = field.parse()?;
                ensure!(
                    seconds.is_finite() && seconds >= 0.0,
                    "Measurement must be finite and nonnegative."
                );
                Ok(RunOutput::measurement(
                    Duration::try_from_secs_f64(seconds)
                        .context("Measurement is too large to represent.")?,
                ))
            };
            Ok(Repetition {
                outputs: Pair {
                    baseline: elapsed(baseline)?,
                    candidate: elapsed(candidate)?,
                },
                order,
            })
        })
        .collect::<Result<Vec<_>>>()?
        .try_into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MeasurementsCsv;
    use std::fs;
    use tempfile::tempdir;

    fn repetitions() -> Result<Repetitions> {
        (0..10)
            .map(|index| Repetition {
                outputs: Pair {
                    baseline: RunOutput::measurement(Duration::from_secs_f64(1.0 + index as f64)),
                    candidate: RunOutput::measurement(Duration::from_secs_f64(1.1 + index as f64)),
                },
                order: if index % 2 == 0 {
                    RunOrder::BaselineFirst
                } else {
                    RunOrder::CandidateFirst
                },
            })
            .collect::<Vec<_>>()
            .try_into()
    }

    #[test]
    fn csv_analysis_matches_in_memory_analysis_at_the_same_seed() -> Result<()> {
        let repetitions = repetitions()?;
        let directory = tempdir()?;
        let path = directory.path().join("measurements.csv");
        let mut csv = MeasurementsCsv::create(&path)?;
        for repetition in repetitions.iter() {
            csv.append(repetition)?;
        }
        drop(csv);
        let intervals = [
            Interval::new(0.5)?,
            Interval::new(0.8)?,
            Interval::new(0.98)?,
        ];
        let draws = NonZeroUsize::new(1_000).unwrap();

        let from_memory = analyze(&repetitions, 0, draws, Shrinkage::NONE, &intervals)?;
        let from_csv = analyze_measurements(&path, 0, draws, Shrinkage::NONE, &intervals)?;

        assert_eq!(from_csv.posterior.draws(), from_memory.posterior.draws());
        assert_eq!(from_csv.summary, from_memory.summary);
        Ok(())
    }

    #[test]
    fn an_unrepresentable_duration_is_an_error() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("measurements.csv");
        let mut csv = String::from("repetition,order,baseline_seconds,candidate_seconds\n");
        for repetition in 1..=10 {
            let order = if repetition % 2 == 0 {
                "baseline_first"
            } else {
                "candidate_first"
            };
            csv.push_str(&format!("{repetition},{order},1e300,1\n"));
        }
        fs::write(&path, csv)?;

        assert!(read_measurements(&path).is_err());
        Ok(())
    }
}
