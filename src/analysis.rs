use crate::run::Measurement;
use crate::{
    Interval, Pair, Posterior, Repetition, Repetitions, RunOrder, Shrinkage, Summary, Time,
};
use anyhow::{Context, Result, ensure};
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
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
    analyze_checked(
        &read_measurements(path)?,
        seed,
        draws,
        shrinkage,
        intervals,
        || Ok(()),
    )
}

/// Analyzes validated repetitions, checking for cancellation between draws.
pub(crate) fn analyze_checked(
    repetitions: &Repetitions<Measurement>,
    seed: u64,
    draws: NonZeroUsize,
    shrinkage: Shrinkage,
    intervals: &[Interval],
    check: impl FnMut() -> Result<()>,
) -> Result<Analysis> {
    ensure!(!intervals.is_empty(), "At least one interval is required.");
    let posterior = Posterior::bootstrap_checked(
        repetitions,
        draws,
        shrinkage,
        &mut Xoshiro256PlusPlus::seed_from_u64(crate::seed::posterior(seed)),
        check,
    )?;
    let summary = posterior.summarize(intervals)?;
    Ok(Analysis { posterior, summary })
}

fn read_measurements(path: &Path) -> Result<Repetitions<Measurement>> {
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
            let elapsed = |field: &str| -> Result<Measurement> {
                let seconds: f64 = field.parse()?;
                ensure!(
                    seconds.is_finite() && seconds >= 0.0,
                    "Measurement must be finite and nonnegative."
                );
                Ok(Measurement {
                    elapsed: Duration::try_from_secs_f64(seconds)
                        .context("Measurement is too large to represent.")?,
                    peak_memory: None,
                })
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
