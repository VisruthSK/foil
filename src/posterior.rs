use anyhow::{Result, ensure};
use rand::Rng;
use rand_distr::{Distribution, Exp1, Gamma};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOrder {
    CandidateFirst,
    BaselineFirst,
}

impl RunOrder {
    /// Effect coding of the order contrast used by the regressions.
    pub const fn effect_code(self) -> f64 {
        match self {
            Self::CandidateFirst => -1.0,
            Self::BaselineFirst => 1.0,
        }
    }
}

/// Weight-summed products of the predictors: centered run position and order
/// contrast. Together with the intercept column these form the Gram matrix
/// `Xᵀ W X` shared by both regressions.
#[derive(Default)]
struct WeightedDesign {
    /// `Σ wᵢ`
    sum_weight: f64,
    /// `Σ wᵢ runᵢ`
    sum_run: f64,
    /// `Σ wᵢ orderᵢ`
    sum_order: f64,
    /// `Σ wᵢ runᵢ²`
    sum_run_run: f64,
    /// `Σ wᵢ runᵢ orderᵢ`
    sum_run_order: f64,
    /// `Σ wᵢ orderᵢ²`
    sum_order_order: f64,
}

impl WeightedDesign {
    fn add_observation(&mut self, weight: f64, run: f64, order: f64) {
        self.sum_weight += weight;
        self.sum_run += weight * run;
        self.sum_order += weight * order;
        self.sum_run_run += weight * run * run;
        self.sum_run_order += weight * run * order;
        self.sum_order_order += weight * order * order;
    }

    /// Fitted value of `response` at run = order = 0, optionally adding a
    /// (0, 0, 0) pseudo-observation.
    fn intercept(&self, response: &WeightedResponse, prior_weight: f64) -> Result<f64> {
        // The pseudo-observation contributes weight alone. Sweeping out the
        // intercept leaves the moments below centered on their weighted means;
        // `sum_`-prefixed values are the raw accumulations.
        let sum_weight = self.sum_weight + prior_weight;

        let run_run = self.sum_run_run - self.sum_run * self.sum_run / sum_weight;
        let run_order = self.sum_run_order - self.sum_run * self.sum_order / sum_weight;
        let order_order = self.sum_order_order - self.sum_order * self.sum_order / sum_weight;
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

/// Weight-summed products of one response with the shared design, forming that
/// regression's right-hand side `Xᵀ W y`.
#[derive(Default)]
struct WeightedResponse {
    /// `Σ wᵢ yᵢ`
    sum_response: f64,
    /// `Σ wᵢ runᵢ yᵢ`
    sum_run_response: f64,
    /// `Σ wᵢ orderᵢ yᵢ`
    sum_order_response: f64,
}

impl WeightedResponse {
    fn add_observation(&mut self, weight: f64, run: f64, order: f64, response: f64) {
        self.sum_response += weight * response;
        self.sum_run_response += weight * run * response;
        self.sum_order_response += weight * order * response;
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
    fn add_pair(&mut self, weight: f64, run: f64, order: f64, midpoint: f64, difference: f64) {
        self.design.add_observation(weight, run, order);
        self.midpoint.add_observation(weight, run, order, midpoint);
        self.difference
            .add_observation(weight, run, order, difference);
    }
}

/// Draws Bayesian-bootstrap samples of drift- and order-adjusted mean baseline and candidate runtimes.
pub fn bootstrap_paired_means(
    baseline: &[f64],
    candidate: &[f64],
    orders: &[RunOrder],
    run_index: &[f64],
    draws: usize,
    shrinkage: f64,
    rng: &mut impl Rng,
) -> Result<Vec<(f64, f64)>> {
    // TODO: move some checks to types
    // TODO: move some checks to CLI
    ensure!(
        baseline.len() >= 10,
        "At least ten paired samples are required."
    );
    ensure!(baseline.len() == candidate.len(), "Sample counts differ.");
    ensure!(
        baseline.len() == orders.len(),
        "Order count differs from sample count."
    );
    ensure!(
        baseline.len() == run_index.len(),
        "Run-position count differs from sample count."
    );
    ensure!(
        orders.contains(&RunOrder::CandidateFirst) && orders.contains(&RunOrder::BaselineFirst),
        "Both run orders are required."
    );
    ensure!(
        run_index.iter().all(|position| position.is_finite()),
        "Run positions must be finite."
    );
    ensure!(draws > 0, "No posterior draws requested.");
    ensure!(
        shrinkage.is_finite() && shrinkage >= 0.0,
        "Shrinkage must be finite and nonnegative."
    );

    let shrinkage_weight = (shrinkage > 0.0)
        .then(|| Gamma::new(shrinkage, 1.0))
        .transpose()?;
    let run_center = run_index.iter().sum::<f64>() / run_index.len() as f64;

    (0..draws)
        .map(|_| {
            let mut moments = WeightedRegressionMoments::default();

            for (((&baseline, &candidate), &order), &run_position) in
                baseline.iter().zip(candidate).zip(orders).zip(run_index)
            {
                let weight: f64 = Exp1.sample(rng);
                let run = run_position - run_center;

                moments.add_pair(
                    weight,
                    run,
                    order.effect_code(),
                    0.5 * (baseline + candidate),
                    candidate - baseline,
                );
            }

            let midpoint = moments.design.intercept(&moments.midpoint, 0.0)?;
            let difference = moments.design.intercept(
                &moments.difference,
                shrinkage_weight
                    .as_ref()
                    .map_or(0.0, |distribution| distribution.sample(rng)),
            )?;

            Ok((midpoint - 0.5 * difference, midpoint + 0.5 * difference))
        })
        .collect()
}

// TODO: make internal somehow?
use std::fmt::Write as _;

pub fn report_posterior(posterior: &[(f64, f64)], intervals: &[f64]) -> String {
    let (mut baseline, mut candidate): (Vec<f64>, Vec<f64>) = posterior.iter().copied().unzip();
    let mut absolute = Vec::with_capacity(posterior.len());
    let mut relative = Vec::with_capacity(posterior.len());

    for &(baseline, candidate) in posterior {
        absolute.push(candidate - baseline);
        relative.push(100.0 * (candidate / baseline - 1.0));
    }

    baseline.sort_by(f64::total_cmp);
    candidate.sort_by(f64::total_cmp);
    absolute.sort_by(f64::total_cmp);
    relative.sort_by(f64::total_cmp);

    let quantile =
        |posterior: &[f64], p: f64| posterior[((posterior.len() - 1) as f64 * p).round() as usize];
    // TODO: move to custom duration derived type?
    let (scale, unit) = match quantile(&baseline, 0.5).max(quantile(&candidate, 0.5)) {
        x if x >= 1.0 => (1.0, "s"),
        x if x >= 1e-3 => (1e3, "ms"),
        x if x >= 1e-6 => (1e6, "µs"),
        _ => (1e9, "ns"),
    };
    let mut report = String::new();

    writeln!(
        report,
        "Baseline:  {:.1}{unit}",
        scale * quantile(&baseline, 0.5)
    )
    .unwrap();

    writeln!(
        report,
        "Candidate: {:.1}{unit}",
        scale * quantile(&candidate, 0.5)
    )
    .unwrap();

    writeln!(report).unwrap();

    writeln!(
        report,
        "Change: {:+.1}{unit} ({:+.2}%)",
        scale * quantile(&absolute, 0.5),
        quantile(&relative, 0.5),
    )
    .unwrap();

    for &width in intervals {
        let tail = (1.0 - width) / 2.0;

        writeln!(
            report,
            "  {:.0}% CrI: [{:+.1}, {:+.1}]{unit} ({:+.2}%, {:+.2}%)",
            100.0 * width,
            scale * quantile(&absolute, tail),
            scale * quantile(&absolute, 1.0 - tail),
            quantile(&relative, tail),
            quantile(&relative, 1.0 - tail),
        )
        .unwrap();
    }

    let probability_faster =
        absolute.partition_point(|&change| change < 0.0) as f64 / absolute.len() as f64;

    writeln!(report).unwrap();

    writeln!(
        report,
        "P(candidate faster): {:.1}%",
        100.0 * probability_faster
    )
    .unwrap();

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn bootstrap_paired_means_is_deterministic() -> Result<()> {
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
        let run_positions = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];

        let mut rng_a = StdRng::seed_from_u64(0);
        let mut rng_b = StdRng::seed_from_u64(0);

        let posterior_a = bootstrap_paired_means(
            &baseline,
            &candidate,
            &orders,
            &run_positions,
            1_000,
            0.0,
            &mut rng_a,
        )?;
        let posterior_b = bootstrap_paired_means(
            &baseline,
            &candidate,
            &orders,
            &run_positions,
            1_000,
            0.0,
            &mut rng_b,
        )?;

        assert_eq!(posterior_a, posterior_b);

        Ok(())
    }
}
