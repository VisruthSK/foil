use super::{Posterior, RegressionRow, Shrinkage, WeightedRegressionMoments};
use crate::RunOutput;
use crate::{Interval, Metric, Pair, Repetition, Repetitions, RunOrder, Time};
use anyhow::{Context, Result, ensure};
use rand::{RngExt, SeedableRng, rngs::Xoshiro256PlusPlus};
use rand_distr::{Distribution, Exp1, StandardNormal};
use std::{num::NonZeroUsize, process::ExitStatus, time::Duration};

const SEED: u64 = 0;
const LEVELS: [f64; 3] = [0.5, 0.8, 0.98];
const DEFAULT_OUTER_DATASETS: usize = 10_000;
const DEFAULT_POSTERIOR_DRAWS: usize = 20_000;

#[derive(Clone, Copy)]
struct Model {
    effect: f64,
    midpoint: f64,
    midpoint_drift: f64,
    differential_drift: f64,
    midpoint_order: f64,
    differential_order: f64,
    midpoint_noise: f64,
    differential_noise: f64,
    shape: Shape,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            effect: 0.0,
            midpoint: 10.0,
            midpoint_drift: 0.0,
            differential_drift: 0.0,
            midpoint_order: 0.0,
            differential_order: 0.0,
            midpoint_noise: 0.04,
            differential_noise: 0.08,
            shape: Shape::Normal,
        }
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Normal,
    HeavyTailed,
    Heteroskedastic,
    StepDrift,
    Autocorrelated,
    ConfoundedOrder,
}

fn rng() -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(SEED)
}

fn measurement(seconds: f64) -> RunOutput {
    RunOutput::new(
        ExitStatus::default(),
        Duration::from_secs_f64(seconds),
        None,
    )
}

fn synthetic(model: Model, n: usize, generator: &mut Xoshiro256PlusPlus) -> Result<Repetitions> {
    let orders = if matches!(model.shape, Shape::ConfoundedOrder) {
        (0..n)
            .map(|position| {
                if position < n / 2 {
                    RunOrder::CandidateFirst
                } else {
                    RunOrder::BaselineFirst
                }
            })
            .collect()
    } else {
        RunOrder::schedule(n, NonZeroUsize::new(4).unwrap(), generator)
    };
    synthetic_with_orders(model, &orders, generator)
}

fn synthetic_with_orders(
    model: Model,
    orders: &[RunOrder],
    generator: &mut Xoshiro256PlusPlus,
) -> Result<Repetitions> {
    let center = (orders.len() - 1) as f64 / 2.0;
    let mut midpoint_error = 0.0;
    let mut difference_error = 0.0;

    orders
        .iter()
        .enumerate()
        .map(|(position, &order)| {
            let run = position as f64 - center;
            let order = order.effect_code();
            let midpoint_z: f64 = StandardNormal.sample(generator);
            let difference_z: f64 = StandardNormal.sample(generator);
            let scale = match model.shape {
                Shape::Heteroskedastic => 0.5 + position as f64 / center.max(1.0),
                Shape::HeavyTailed if generator.random::<f64>() < 0.05 => 8.0,
                _ => 1.0,
            };

            if matches!(model.shape, Shape::Autocorrelated) {
                const RHO: f64 = 0.7;
                let innovation = (1.0 - RHO * RHO).sqrt();
                midpoint_error = RHO * midpoint_error + innovation * midpoint_z;
                difference_error = RHO * difference_error + innovation * difference_z;
            } else {
                midpoint_error = scale * midpoint_z;
                difference_error = scale * difference_z;
            }

            let misspecified_drift = match model.shape {
                Shape::StepDrift => 0.08 * run.signum(),
                _ => 0.0,
            };
            let midpoint = model.midpoint
                + model.midpoint_drift * run
                + model.midpoint_order * order
                + model.midpoint_noise * midpoint_error;
            let difference = model.effect
                + model.differential_drift * run
                + model.differential_order * order
                + misspecified_drift
                + model.differential_noise * difference_error;

            Ok(Repetition {
                outputs: Pair {
                    baseline: measurement(midpoint - 0.5 * difference),
                    candidate: measurement(midpoint + 0.5 * difference),
                },
                order: if order < 0.0 {
                    RunOrder::CandidateFirst
                } else {
                    RunOrder::BaselineFirst
                },
            })
        })
        .collect::<Result<Vec<_>>>()?
        .try_into()
}

#[test]
fn exact_recovery_is_independent_of_bootstrap_weights() -> Result<()> {
    let model = Model {
        effect: 0.24,
        midpoint: 8.0,
        midpoint_drift: 0.07,
        differential_drift: -0.03,
        midpoint_order: 0.11,
        differential_order: -0.09,
        midpoint_noise: 0.0,
        differential_noise: 0.0,
        shape: Shape::Normal,
    };
    let expected = Pair {
        baseline: model.midpoint - 0.5 * model.effect,
        candidate: model.midpoint + 0.5 * model.effect,
    };

    for n in [10, 30] {
        let repetitions = synthetic(model, n, &mut rng())?;
        let rows = RegressionRow::all::<Time>(&repetitions)?;
        let mut weight_rng = rng();
        for _ in 0..512 {
            let mut moments = WeightedRegressionMoments::default();
            for row in &rows {
                moments.add(Exp1.sample(&mut weight_rng), row);
            }
            let midpoint = moments.design.intercept(&moments.midpoint, 0.0)?;
            let difference = moments.design.intercept(&moments.difference, 0.0)?;
            assert!((midpoint - model.midpoint).abs() < 1e-10);
            assert!((difference - model.effect).abs() < 1e-10);
        }
        let posterior = Posterior::<Time>::bootstrap(
            &repetitions,
            NonZeroUsize::new(512).unwrap(),
            Shrinkage::NONE,
            &mut rng(),
        )?;

        for draw in posterior.draws() {
            assert!((draw.baseline.base() - expected.baseline).abs() < 1e-10);
            assert!((draw.candidate.base() - expected.candidate).abs() < 1e-10);
            assert!((draw.absolute().base() - model.effect).abs() < 1e-10);
        }
    }
    Ok(())
}

#[test]
fn an_unidentifiable_order_design_is_rejected() {
    let repetitions = (0..10)
        .map(|_| Repetition {
            outputs: Pair {
                baseline: measurement(10.0),
                candidate: measurement(10.1),
            },
            order: RunOrder::BaselineFirst,
        })
        .collect::<Vec<_>>();

    let error = Repetitions::try_from(repetitions)
        .err()
        .expect("A design with no order contrast must fail.");
    assert_eq!(error.to_string(), "Both run orders are required.");
}

struct Scenario {
    name: &'static str,
    n: usize,
    model: Model,
}

struct Calibration {
    coverage: [usize; 3],
    bias: f64,
    width: [f64; 3],
    sign_correct: usize,
    datasets: usize,
    effect: f64,
}

impl Calibration {
    fn report(&self, name: &str, shrinkage: Shrinkage) {
        let n = self.datasets as f64;
        let sign = (self.effect != 0.0).then(|| self.sign_correct as f64 / n);
        eprintln!(
            "{name}: datasets={}, shrinkage={}, bias={:+.6}, sign_accuracy={}",
            self.datasets,
            shrinkage.get(),
            self.bias / n,
            sign.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.4}"))
        );
        for (index, level) in LEVELS.into_iter().enumerate() {
            let standard_error = (level * (1.0 - level) / n).sqrt();
            eprintln!(
                "  {:>2.0}% CrI: coverage={:.4}, binomial_4se=[{:.4}, {:.4}], mean_width={:.6}",
                100.0 * level,
                self.coverage[index] as f64 / n,
                (level - 4.0 * standard_error).max(0.0),
                (level + 4.0 * standard_error).min(1.0),
                self.width[index] / n
            );
        }
    }
}

fn calibrate(
    scenario: &Scenario,
    datasets: usize,
    draws: usize,
    shrinkage: Shrinkage,
) -> Result<Calibration> {
    let intervals = LEVELS.map(|level| Interval::new(level).expect("Valid interval."));
    let mut generator = rng();
    let mut posterior_rng = rng();
    let mut result = Calibration {
        coverage: [0; 3],
        bias: 0.0,
        width: [0.0; 3],
        sign_correct: 0,
        datasets,
        effect: scenario.model.effect,
    };

    for dataset in 0..datasets {
        let repetitions = synthetic(scenario.model, scenario.n, &mut generator)
            .with_context(|| format!("{} dataset {dataset}", scenario.name))?;
        let posterior = Posterior::<Time>::bootstrap(
            &repetitions,
            NonZeroUsize::new(draws).context("Posterior draws must be positive.")?,
            shrinkage,
            &mut posterior_rng,
        )
        .with_context(|| format!("{} dataset {dataset}", scenario.name))?;
        let summary = posterior.summarize(&intervals)?;
        let estimate = summary.change.absolute_median.base();
        result.bias += estimate - scenario.model.effect;
        result.sign_correct += usize::from(
            scenario.model.effect != 0.0 && estimate.signum() == scenario.model.effect.signum(),
        );

        for (index, bounds) in summary.change.intervals.iter().enumerate() {
            let lower = bounds.absolute.lower.base();
            let upper = bounds.absolute.upper.base();
            result.coverage[index] +=
                usize::from(lower <= scenario.model.effect && scenario.model.effect <= upper);
            result.width[index] += upper - lower;
        }
    }
    Ok(result)
}

fn setting(name: &str, default: usize) -> Result<usize> {
    std::env::var(name).map_or(Ok(default), |value| {
        value
            .parse()
            .with_context(|| format!("{name} must be a positive integer"))
            .and_then(|value| {
                ensure!(value > 0, "{name} must be a positive integer");
                Ok(value)
            })
    })
}

fn release_settings(default_datasets: usize) -> Result<(usize, usize)> {
    ensure!(
        !cfg!(debug_assertions),
        "Calibration is release-only; run `cargo test --release calibration -- --ignored --nocapture`."
    );
    Ok((
        setting("B3_CALIBRATION_DATASETS", default_datasets)?,
        setting("B3_CALIBRATION_DRAWS", DEFAULT_POSTERIOR_DRAWS)?,
    ))
}

fn confirmatory_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "n=10, null, no drift",
            n: 10,
            model: Model::default(),
        },
        Scenario {
            name: "n=20, small effect, common drift",
            n: 20,
            model: Model {
                effect: 0.025,
                midpoint_drift: 0.03,
                ..Model::default()
            },
        },
        Scenario {
            name: "n=30, clear effect, differential drift",
            n: 30,
            model: Model {
                effect: 0.15,
                differential_drift: 0.012,
                ..Model::default()
            },
        },
        Scenario {
            name: "n=50, null, run-order bias",
            n: 50,
            model: Model {
                midpoint_order: 0.08,
                differential_order: -0.06,
                ..Model::default()
            },
        },
        Scenario {
            name: "n=75, small effect, combined drift and order",
            n: 75,
            model: Model {
                effect: -0.025,
                midpoint_drift: 0.03,
                differential_drift: -0.009,
                midpoint_order: 0.08,
                differential_order: 0.05,
                ..Model::default()
            },
        },
        Scenario {
            name: "n=100, clear effect, paired common-mode noise",
            n: 100,
            model: Model {
                effect: 0.15,
                midpoint_noise: 0.30,
                differential_noise: 0.03,
                ..Model::default()
            },
        },
        Scenario {
            name: "n=30, small effect, substantial difference noise",
            n: 30,
            model: Model {
                effect: 0.025,
                midpoint_noise: 0.02,
                differential_noise: 0.18,
                ..Model::default()
            },
        },
    ]
}

#[test]
#[ignore = "release-only Monte Carlo calibration"]
fn confirmatory_coverage() -> Result<()> {
    let (datasets, draws) = release_settings(DEFAULT_OUTER_DATASETS)?;
    for scenario in confirmatory_scenarios() {
        let result = calibrate(&scenario, datasets, draws, Shrinkage::NONE)?;
        result.report(scenario.name, Shrinkage::NONE);
    }
    Ok(())
}

#[test]
#[ignore = "release-only Monte Carlo shrinkage diagnostics"]
fn shrinkage_diagnostics() -> Result<()> {
    let (datasets, draws) = release_settings(5_000)?;
    for effect in [0.0, 0.025, 0.15] {
        let scenario = Scenario {
            name: "shrinkage",
            n: 30,
            model: Model {
                effect,
                ..Model::default()
            },
        };
        for amount in [0.0, 5.0, 20.0] {
            let shrinkage = Shrinkage::new(amount)?;
            let result = calibrate(&scenario, datasets, draws, shrinkage)?;
            result.report(&format!("shrinkage effect={effect:+.3}"), shrinkage);
        }
    }
    Ok(())
}

#[test]
#[ignore = "release-only Monte Carlo misspecification diagnostics"]
fn misspecification_diagnostics() -> Result<()> {
    let (datasets, draws) = release_settings(2_000)?;
    for (name, shape) in [
        ("heavy tails and outliers", Shape::HeavyTailed),
        ("heteroskedasticity", Shape::Heteroskedastic),
        ("nonlinear step drift", Shape::StepDrift),
        ("autocorrelation", Shape::Autocorrelated),
        ("run-order and position confounding", Shape::ConfoundedOrder),
    ] {
        let scenario = Scenario {
            name,
            n: 30,
            model: Model {
                effect: 0.025,
                differential_drift: 0.006,
                differential_order: 0.04,
                shape,
                ..Model::default()
            },
        };
        calibrate(&scenario, datasets, draws, Shrinkage::NONE)?.report(name, Shrinkage::NONE);
    }
    Ok(())
}
