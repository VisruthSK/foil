use anyhow::{Result, ensure};
use rand::Rng;
use rand_distr::{Distribution, Exp1, Gamma};

/// Draws Bayesian bootstrap samples of the mean paired log ratio.
pub fn bootstrap_mean_log_ratios(
    baseline: &[f64],
    candidate: &[f64],
    draws: usize,
    shrinkage: usize,
    rng: &mut impl Rng,
) -> Result<Vec<f64>> {
    ensure!(!baseline.is_empty(), "No samples provided.");
    ensure!(baseline.len() == candidate.len(), "Sample counts differ.");

    let ratios: Vec<f64> = baseline
        .iter()
        .zip(candidate)
        .map(|(&baseline, &candidate)| candidate.ln() - baseline.ln())
        .collect();

    let shrinkage_weight = (shrinkage > 0)
        .then(|| Gamma::new(shrinkage as f64, 1.0))
        .transpose()?;

    let posterior = (0..draws)
        .map(|_| {
            let (sum, weight_sum) = ratios.iter().fold((0.0, 0.0), |(sum, weight_sum), &ratio| {
                let weight: f64 = Exp1.sample(rng);
                (sum + weight * ratio, weight_sum + weight)
            });
            let shrinkage_weight = shrinkage_weight
                .as_ref()
                .map_or(0.0, |distribution| distribution.sample(rng));

            sum / (weight_sum + shrinkage_weight)
        })
        .collect();

    Ok(posterior)
}
