use crate::metric::{MeasuredMetric, Metric};
use crate::repetition::Repetitions;
use crate::summary::{Interval, Summary};

use anyhow::{Context, Result, ensure};
use rand::Rng;
use rand_distr::{Distribution, Exp1, Gamma};
use std::{num::NonZeroUsize, str::FromStr};

struct RegressionRow {
    midpoint: f64,
    difference: f64,
    run: f64,
    order: f64,
}

impl RegressionRow {
    fn all<M: MeasuredMetric>(repetitions: &Repetitions) -> Result<Vec<Self>> {
        let center = repetitions.center();

        repetitions
            .iter()
            .enumerate()
            .map(|(position, repetition)| {
                let baseline = M::read(&repetition.outputs.baseline)?.base();
                let candidate = M::read(&repetition.outputs.candidate)?.base();

                Ok(Self {
                    midpoint: 0.5 * (baseline + candidate),
                    difference: candidate - baseline,
                    run: (position as f64 - center) / center,
                    order: repetition.order.effect_code(),
                })
            })
            .collect()
    }
}

/// Weight-summed products of the predictors: scaled run position and order
/// contrast. Together with the intercept column these form the Gram matrix
/// $X^\top W X$ shared by both regressions.
///
/// Throughout, $w_i$ is the bootstrap weight of repetition $i$, $r_i$ its scaled run
/// position, $o_i$ its order contrast, and $y_i$ its response.
#[derive(Default)]
struct WeightedDesign {
    /// $\sum_i w_i$.
    sum_weight: f64,
    /// $\sum_i w_i r_i$.
    sum_run: f64,
    /// $\sum_i w_i o_i$.
    sum_order: f64,
    /// $\sum_i w_i r_i^2$.
    sum_run_run: f64,
    /// $\sum_i w_i r_i o_i$.
    sum_run_order: f64,
}

impl WeightedDesign {
    fn add_observation(&mut self, weight: f64, row: &RegressionRow) {
        self.sum_weight += weight;
        self.sum_run += weight * row.run;
        self.sum_order += weight * row.order;
        self.sum_run_run += weight * row.run * row.run;
        self.sum_run_order += weight * row.run * row.order;
    }

    /// Fitted value of `response` at run = order = 0, optionally adding a (0, 0, 0) pseudo-observation.
    fn intercept(&self, response: &WeightedResponse, prior_weight: f64) -> Result<f64> {
        let sum_weight = self.sum_weight + prior_weight;

        let run_run = self.sum_run_run - self.sum_run * self.sum_run / sum_weight;
        let run_order = self.sum_run_order - self.sum_run * self.sum_order / sum_weight;
        let order_order = self.sum_weight - self.sum_order * self.sum_order / sum_weight;
        let run_response =
            response.sum_run_response - self.sum_run * response.sum_response / sum_weight;
        let order_response =
            response.sum_order_response - self.sum_order * response.sum_response / sum_weight;

        let determinant = run_run * order_order - run_order * run_order;
        ensure!(determinant > 0.0, "Regression model is singular.");

        let run_slope = (run_response * order_order - order_response * run_order) / determinant;
        let order_slope = (order_response * run_run - run_response * run_order) / determinant;

        Ok(
            (response.sum_response - run_slope * self.sum_run - order_slope * self.sum_order)
                / sum_weight,
        )
    }
}

/// Weight-summed products of one response with the shared design, forming that regression's right-hand side $X^\top W y$.
#[derive(Default)]
struct WeightedResponse {
    /// $\sum_i w_i y_i$.
    sum_response: f64,
    /// $\sum_i w_i r_i y_i$.
    sum_run_response: f64,
    /// $\sum_i w_i o_i y_i$.
    sum_order_response: f64,
}

impl WeightedResponse {
    fn add_observation(&mut self, weight: f64, row: &RegressionRow, response: f64) {
        self.sum_response += weight * response;
        self.sum_run_response += weight * row.run * response;
        self.sum_order_response += weight * row.order * response;
    }
}

/// Sufficient statistics for the two regressions, which share a design.
#[derive(Default)]
struct WeightedRegressionMoments {
    design: WeightedDesign,
    midpoint: WeightedResponse,
    difference: WeightedResponse,
}

impl WeightedRegressionMoments {
    /// Folds one weighted row into the sums.
    fn add(&mut self, weight: f64, row: &RegressionRow) {
        self.design.add_observation(weight, row);
        self.midpoint.add_observation(weight, row, row.midpoint);
        self.difference.add_observation(weight, row, row.difference);
    }
}

/// A prior count of no-change pseudo-observations, finite and nonnegative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shrinkage(f64);

impl Shrinkage {
    pub const NONE: Self = Self(0.0);

    pub fn new(value: f64) -> Result<Self> {
        ensure!(
            value.is_finite() && value >= 0.0,
            "Shrinkage must be finite and nonnegative, got {value}."
        );

        Ok(Self(value))
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl FromStr for Shrinkage {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::new(
            text.parse()
                .with_context(|| format!("`{text}` is not a number."))?,
        )
    }
}

/// One Bayesian-bootstrap draw of the drift and order adjusted mean baseline and candidate value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Draw<M> {
    pub baseline: M,
    pub candidate: M,
}

impl<M: Metric> Draw<M> {
    pub fn absolute(self) -> M {
        M::from_base(self.candidate.base() - self.baseline.base())
    }

    pub fn relative(self) -> Option<f64> {
        let baseline = self.baseline.base();

        (baseline != 0.0).then(|| 100.0 * (self.candidate.base() / baseline - 1.0))
    }
}

/// Posterior draws of one metric's adjusted means.
pub struct Posterior<M> {
    draws: Vec<Draw<M>>,
}

impl<M: Metric> Posterior<M> {
    /// Draws Bayesian-bootstrap samples of the adjusted mean baseline and candidate value.
    #[cfg(test)]
    pub(crate) fn bootstrap(
        repetitions: &Repetitions,
        draws: NonZeroUsize,
        shrinkage: Shrinkage,
        rng: &mut impl Rng,
    ) -> Result<Self>
    where
        M: MeasuredMetric,
    {
        Self::bootstrap_checked(repetitions, draws, shrinkage, rng, || Ok(()))
    }

    /// Draws Bayesian-bootstrap samples while checking for cancellation before each draw.
    pub(crate) fn bootstrap_checked(
        repetitions: &Repetitions,
        draws: NonZeroUsize,
        shrinkage: Shrinkage,
        rng: &mut impl Rng,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self>
    where
        M: MeasuredMetric,
    {
        let shrinkage_distribution = if shrinkage.0 == 0.0 {
            None
        } else {
            Some(Gamma::new(shrinkage.0, 1.0)?)
        };
        let rows = RegressionRow::all::<M>(repetitions)?;

        (0..draws.get())
            .map(|_| {
                check()?;
                let mut moments = WeightedRegressionMoments::default();

                for row in &rows {
                    moments.add(Exp1.sample(rng), row);
                }

                let midpoint = moments.design.intercept(&moments.midpoint, 0.0)?;

                let prior_weight = match &shrinkage_distribution {
                    Some(distribution) => distribution.sample(rng),
                    None => 0.0,
                };

                let difference = moments
                    .design
                    .intercept(&moments.difference, prior_weight)?;

                Ok(Draw {
                    baseline: M::from_base(midpoint - 0.5 * difference),
                    candidate: M::from_base(midpoint + 0.5 * difference),
                })
            })
            .collect::<Result<_>>()
            .map(|draws| Self { draws })
    }
}

impl<M: Metric> Posterior<M> {
    /// Never empty.
    pub fn draws(&self) -> &[Draw<M>] {
        &self.draws
    }

    pub fn summarize(&self, intervals: &[Interval]) -> Result<Summary<M>> {
        ensure!(!intervals.is_empty(), "At least one interval is required.");
        Summary::from_draws(&self.draws, intervals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::write_posterior_csv;
    use crate::metric::{PeakMemory, Time};
    use crate::repetition::{Pair, Repetition, RunOrder};
    use crate::run::{Bytes, RunOutput};
    use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
    use std::fs::read_to_string;
    use std::process::ExitStatus;
    use std::time::Duration;
    use tempfile::tempdir;

    /// Ten paired repetitions carrying a mild upward drift, with both run orders
    /// represented so `Repetitions` accepts them.
    ///
    /// Memory is left at zero throughout, which is what lets a test detect a metric
    /// that reads the wrong field.
    fn fixture() -> Result<Repetitions> {
        let baseline = [1.00, 1.08, 1.13, 1.18, 1.27, 1.31, 1.39, 1.44, 1.53, 1.59];
        let candidate = [1.04, 1.06, 1.19, 1.17, 1.31, 1.30, 1.46, 1.41, 1.58, 1.61];
        let orders = [
            RunOrder::CandidateFirst,
            RunOrder::BaselineFirst,
            RunOrder::BaselineFirst,
            RunOrder::CandidateFirst,
            RunOrder::CandidateFirst,
            RunOrder::BaselineFirst,
            RunOrder::CandidateFirst,
            RunOrder::BaselineFirst,
            RunOrder::BaselineFirst,
            RunOrder::CandidateFirst,
        ];

        // `ExitStatus::default()` is exit 0, so these read as successful runs.
        let measure = |seconds: f64| {
            RunOutput::new(
                ExitStatus::default(),
                Duration::from_secs_f64(seconds),
                Some(Bytes::ZERO),
            )
        };

        (0..baseline.len())
            .map(|position| Repetition {
                outputs: Pair {
                    baseline: measure(baseline[position]),
                    candidate: measure(candidate[position]),
                },
                order: orders[position],
            })
            .collect::<Vec<_>>()
            .try_into()
    }

    /// The generator every golden is pinned to.
    ///
    /// `Xoshiro256PlusPlus` rather than `StdRng`, whose stream `rand` is free to
    /// change between releases. That would invalidate the goldens without any change
    /// to this crate, and it is also the generator `main` uses.
    fn rng() -> Xoshiro256PlusPlus {
        Xoshiro256PlusPlus::seed_from_u64(0)
    }

    /// The fixture's posterior for `M` at seed 0.
    fn posterior<M: Metric + MeasuredMetric>(draws: usize, shrinkage: f64) -> Result<Posterior<M>> {
        let draws = NonZeroUsize::new(draws).expect("Test draw counts are positive.");

        Posterior::bootstrap(&fixture()?, draws, Shrinkage::new(shrinkage)?, &mut rng())
    }

    /// Eight time draws, for golden comparison.
    fn golden(shrinkage: f64) -> Result<Vec<(f64, f64)>> {
        Ok(posterior::<Time>(8, shrinkage)?
            .draws()
            .iter()
            .map(|draw| (draw.baseline.base(), draw.candidate.base()))
            .collect())
    }

    /// Pins the adjusted means bit for bit. Any change to the arithmetic, the order of
    /// RNG consumption, or the fixture moves these numbers, so a refactor meant to be
    /// inert fails here.
    ///
    /// These say nothing about whether the model is correct. They were generated from
    /// the code they check, so they catch drift, not error.
    #[test]
    fn unshrunk_posterior_matches_golden() -> Result<()> {
        #[rustfmt::skip]
        const EXPECTED: [(f64, f64); 8] = [
            (1.2947094189758073, 1.3181596849182757),
            (1.295794106123628,  1.3142782690813486),
            (1.2901681962200746, 1.3153286953279186),
            (1.2948331312861074, 1.3383861101432366),
            (1.2943604503099144, 1.3140897265174494),
            (1.287205483379233,  1.295401275903725),
            (1.2873420629313597, 1.2950401423013826),
            (1.2917392395536966, 1.3134285293933505),
        ];

        assert_eq!(golden(0.0)?, EXPECTED);

        Ok(())
    }

    /// Same, through the `Gamma` path, which a shrinkage of zero skips entirely.
    #[test]
    fn shrunk_posterior_matches_golden() -> Result<()> {
        #[rustfmt::skip]
        const EXPECTED: [(f64, f64); 8] = [
            (1.2987470681009876, 1.3141220357930954),
            (1.3034419507117767, 1.3148530015498219),
            (1.3034869031980645, 1.3229031219464489),
            (1.2963181289876267, 1.3077913512511117),
            (1.2963354841261892, 1.3072501069680238),
            (1.2888327854737045, 1.2935494197590378),
            (1.2974459759699728, 1.311355238500543),
            (1.2885189566400443, 1.2988151609969738),
        ];

        assert_eq!(golden(5.0)?, EXPECTED);

        Ok(())
    }

    /// Typical magnitude of the adjusted difference across a posterior.
    ///
    /// Compared distributionally rather than draw by draw: a shrunk run pulls one extra
    /// `Gamma` sample per draw, so its weight stream diverges from an unshrunk run's and
    /// the two are not paired.
    fn median_absolute_difference(shrinkage: f64) -> Result<f64> {
        let mut differences: Vec<f64> = posterior::<Time>(4_000, shrinkage)?
            .draws()
            .iter()
            .map(|draw| draw.absolute().base().abs())
            .collect();

        differences.sort_by(f64::total_cmp);

        Ok(differences[differences.len() / 2])
    }

    /// More shrinkage has to pull the adjusted difference further toward zero.
    #[test]
    fn shrinkage_narrows_the_difference() -> Result<()> {
        let none = median_absolute_difference(0.0)?;
        let some = median_absolute_difference(5.0)?;
        let lots = median_absolute_difference(50.0)?;

        assert!(
            lots < some && some < none,
            "Expected monotone narrowing, got none={none}, five={some}, fifty={lots}."
        );

        Ok(())
    }

    /// Pins the machine-readable side: header, column order, and the full precision a
    /// reader needs to reproduce the summary.
    #[test]
    fn posterior_csv_matches_golden() -> Result<()> {
        const EXPECTED: &str = concat!(
            "baseline_seconds,candidate_seconds\n",
            "1.2947094189758073,1.3181596849182757\n",
            "1.295794106123628,1.3142782690813486\n",
            "1.2901681962200746,1.3153286953279186\n",
        );

        let directory = tempdir()?;
        let path = directory.path().join("posterior.csv");

        write_posterior_csv(&path, &posterior::<Time>(3, 0.0)?)?;

        assert_eq!(read_to_string(&path)?, EXPECTED);

        Ok(())
    }

    /// `Metric` has to select the response, not just ride along. The fixture records
    /// no memory, so reading the wrong field yields runtimes near 1.3 instead of zeros.
    #[test]
    fn memory_metric_reads_the_memory_field() -> Result<()> {
        let zero = PeakMemory::from_base(0.0);

        assert_eq!(
            posterior::<PeakMemory>(8, 0.0)?.draws(),
            [Draw {
                baseline: zero,
                candidate: zero
            }; 8]
        );

        Ok(())
    }

    #[test]
    fn bootstrapping_is_deterministic() -> Result<()> {
        let draws = || posterior::<Time>(1_000, 0.0).map(|it| it.draws().to_vec());

        assert_eq!(draws()?, draws()?);

        Ok(())
    }

    #[test]
    fn bootstrapping_can_be_cancelled_between_draws() -> Result<()> {
        let repetitions = fixture()?;
        let mut rng = rng();
        let mut checks = 0;

        let error = Posterior::<Time>::bootstrap_checked(
            &repetitions,
            NonZeroUsize::new(1_000).unwrap(),
            Shrinkage::new(0.0)?,
            &mut rng,
            || {
                checks += 1;
                anyhow::ensure!(checks < 4, "Interrupted.");
                Ok(())
            },
        )
        .err()
        .context("Bootstrapping should stop when cancelled.")?;

        assert_eq!(error.to_string(), "Interrupted.");
        assert_eq!(checks, 4);

        Ok(())
    }
}
