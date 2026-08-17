# Statistical validation

The posterior has two complementary synthetic checks. Ordinary tests generate noise-free data exactly from the fitted midpoint and candidate-minus-baseline regressions. They verify that adjusted intercepts are invariant to arbitrary positive Bayesian-bootstrap weights and that unshrunk posterior draws recover the known values to floating-point tolerance.

**`foil` uses the uncorrected Bayesian bootstrap; its intervals are materially anti-conservative at small sample sizes and approach nominal coverage gradually with increasing repetitions.** The adjusted-effect estimator is nearly unbiased in the model-correct simulations, but that does not imply calibrated uncertainty: at `n = 30`, all three interval levels still undercover materially.

The ignored calibration tests repeatedly generate independent datasets, run the production posterior, and summarize its production 50%, 80%, and 98% central credible intervals. All generator and posterior RNG streams are separate `Xoshiro256PlusPlus` instances initialized from seed 0, so failures reproduce exactly.

Run the confirmatory model-correct suite in release mode:

```sh
cargo test --release posterior::calibration::confirmatory_coverage -- --ignored --nocapture
```

It uses 10,000 outer datasets per scenario and 20,000 Bayesian-bootstrap draws per dataset by default. The report gives bias, sign accuracy, interval width, observed coverage, and the four-standard-error binomial range around nominal coverage. The confirmatory matrix reports `n = 10`, `20`, `30`, `50`, `75`, and `100` while covering null, small, and clear effects; common and differential drift; order bias; combined adjustment; paired common-mode noise; and substantial pairwise-difference noise. Coverage is reported rather than repaired by changing the nonparametric posterior.

Shrinkage and model misspecification are diagnostic rather than nominal-coverage gates:

```sh
cargo test --release posterior::calibration::shrinkage_diagnostics -- --ignored --nocapture
cargo test --release posterior::calibration::misspecification_diagnostics -- --ignored --nocapture
```

They report bias, coverage, mean interval width, and sign accuracy. Misspecification cases cover heavy tails and outliers, heteroskedasticity, step drift, autocorrelation, and run-order/run-position confounding. Shrinkage is evaluated separately at zero, small, and larger true effects because intentional pull toward zero need not have nominal frequentist coverage.

For a quick deterministic smoke run, override the simulation sizes:

```sh
FOIL_CALIBRATION_DATASETS=200 FOIL_CALIBRATION_DRAWS=2000 cargo test --release posterior::calibration::confirmatory_coverage -- --ignored --nocapture
```

Small smoke runs are useful for exercising the harness, not for judging calibration. The reported coverage uncertainty is governed mainly by the number of outer datasets; posterior draws should remain numerous enough that quantile Monte Carlo error is smaller than that uncertainty.

The checked-in [seed-0 confirmatory results](calibration-results.md) contain the full 1.4-billion-draw report.

One additional family holds the complete data-generating process fixed while varying only `n = 10`, `20`, `30`, `50`, `75`, `100`, `150`, and `200`. It reports coverage error for log-log plotting and estimates the log-log slope directly:

```sh
cargo test --release posterior::calibration::coverage_convergence -- --ignored --nocapture
```
