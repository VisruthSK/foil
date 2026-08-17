use crate::run::RunOutput;
use crate::{
    Interval, Pair, Posterior, Repetition, Repetitions, RunOrder, Shrinkage, Summary, Time,
};
use anyhow::{Context, Result, ensure};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{fs, num::NonZeroUsize, path::Path, time::Duration};

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
pub fn analyze_checked(
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
    let text =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}.", path.display()))?;
    let mut lines = text.lines();
    ensure!(
        lines.next() == Some("repetition,order,baseline_seconds,candidate_seconds"),
        "{} has an invalid measurements header.",
        path.display()
    );

    lines
        .enumerate()
        .map(|(index, line)| {
            let fields: Vec<_> = line.split(',').collect();
            ensure!(fields.len() == 4, "Invalid measurements row {}.", index + 1);
            ensure!(
                fields[0].parse::<usize>()? == index + 1,
                "Measurements repetitions must be sequential."
            );
            let order = match fields[1] {
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
                Ok(RunOutput::measurement(Duration::from_secs_f64(seconds)))
            };
            Ok(Repetition {
                outputs: Pair {
                    baseline: elapsed(fields[2])?,
                    candidate: elapsed(fields[3])?,
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
}
