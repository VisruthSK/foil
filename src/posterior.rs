use anyhow::{Result, ensure};
use rand::Rng;
use rand_distr::{Distribution, Exp1, Gamma};

/// Draws Bayesian bootstrap samples of paired mean baseline and candidate runtimes.
pub fn bootstrap_paired_means(
    baseline: &[f64],
    candidate: &[f64],
    baseline_firsts: &[f64],
    draws: usize,
    shrinkage: f64,
    rng: &mut impl Rng,
) -> Result<Vec<(f64, f64)>> {
    ensure!(!baseline.is_empty(), "No samples provided.");
    ensure!(baseline.len() == candidate.len(), "Sample counts differ.");

    let shrinkage_weight = (shrinkage > 0.0)
        .then(|| Gamma::new(shrinkage, 1.0))
        .transpose()?;

    let posterior = (0..draws)
        .map(|_| {
            let (baseline_sum, candidate_sum, weight_sum) = baseline.iter().zip(candidate).fold(
                (0.0, 0.0, 0.0),
                |(baseline_sum, candidate_sum, weight_sum), (&baseline, &candidate)| {
                    let weight: f64 = Exp1.sample(rng);
                    (
                        baseline_sum + weight * baseline,
                        candidate_sum + weight * candidate,
                        weight_sum + weight,
                    )
                },
            );

            let baseline = baseline_sum / weight_sum;
            let prior_weight = shrinkage_weight
                .as_ref()
                .map_or(0.0, |distribution| distribution.sample(rng));
            let candidate = (candidate_sum + prior_weight * baseline) / (weight_sum + prior_weight);

            (baseline, candidate)
        })
        .collect();

    Ok(posterior)
}

// TODO: make internal somehow?
pub fn report_posterior(posterior: &[(f64, f64)], intervals: &[f64]) {
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

    println!("Baseline:  {:.1}{unit}", scale * quantile(&baseline, 0.5));
    println!("Candidate: {:.1}{unit}", scale * quantile(&candidate, 0.5));
    println!();

    println!(
        "Change: {:+.1}{unit} ({:+.2}%)",
        scale * quantile(&absolute, 0.5),
        quantile(&relative, 0.5),
    );

    for &width in intervals {
        let tail = (1.0 - width) / 2.0;
        println!(
            "  {:.0}% CrI: [{:+.1}, {:+.1}]{unit} ({:+.2}%, {:+.2}%)",
            100.0 * width,
            scale * quantile(&absolute, tail),
            scale * quantile(&absolute, 1.0 - tail),
            quantile(&relative, tail),
            quantile(&relative, 1.0 - tail),
        );
    }

    let probability_faster =
        absolute.partition_point(|&change| change < 0.0) as f64 / absolute.len() as f64;

    println!();
    println!("P(candidate faster): {:.1}%", 100.0 * probability_faster);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn bootstrap_paired_means_is_deterministic() -> Result<()> {
        let baseline = [1.0, 2.0, 3.0, 4.0];
        let candidate = [2.0, 3.0, 4.0, 5.0];
        let baseline_firsts = [0.0, 0.0, 1.0, 1.0];

        let mut rng_a = StdRng::seed_from_u64(0);
        let mut rng_b = StdRng::seed_from_u64(0);

        let posterior_a = bootstrap_paired_means(
            &baseline,
            &candidate,
            &baseline_firsts,
            1_000,
            0.0,
            &mut rng_a,
        )?;
        let posterior_b = bootstrap_paired_means(
            &baseline,
            &candidate,
            &baseline_firsts,
            1_000,
            0.0,
            &mut rng_b,
        )?;

        assert_eq!(posterior_a, posterior_b);

        Ok(())
    }
}
