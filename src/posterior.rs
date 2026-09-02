use crate::metric::{MeasuredMetric, Metric};
use crate::repetition::Repetitions;
use crate::run::Measurement;
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
    fn all<M: MeasuredMetric>(repetitions: &Repetitions<Measurement>) -> Result<Vec<Self>> {
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
    /// Draws Bayesian-bootstrap samples while checking for cancellation before each draw.
    pub(crate) fn bootstrap_checked(
        repetitions: &Repetitions<Measurement>,
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
    use crate::{Pair, Repetition, RunOrder, Time};
    use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
    use std::time::Duration;

    fn fixture() -> Result<Repetitions<Measurement>> {
        (0..10)
            .map(|position| Repetition {
                outputs: Pair {
                    baseline: Measurement {
                        elapsed: Duration::from_secs_f64(1.0 + position as f64 / 10.0),
                        peak_memory: None,
                    },
                    candidate: Measurement {
                        elapsed: Duration::from_secs_f64(1.1 + position as f64 / 10.0),
                        peak_memory: None,
                    },
                },
                order: if position % 2 == 0 {
                    RunOrder::BaselineFirst
                } else {
                    RunOrder::CandidateFirst
                },
            })
            .collect::<Vec<_>>()
            .try_into()
    }

    #[test]
    fn bootstrapping_can_be_cancelled_between_draws() -> Result<()> {
        let mut checks = 0;
        let error = Posterior::<Time>::bootstrap_checked(
            &fixture()?,
            NonZeroUsize::new(1_000).unwrap(),
            Shrinkage::NONE,
            &mut Xoshiro256PlusPlus::seed_from_u64(0),
            || {
                checks += 1;
                anyhow::ensure!(checks < 4, "Interrupted.");
                Ok(())
            },
        )
        .err()
        .context("bootstrapping should stop when cancelled")?;

        assert_eq!(error.to_string(), "Interrupted.");
        assert_eq!(checks, 4);
        Ok(())
    }
}
