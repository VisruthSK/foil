# Bayesian-bootstrap calibration results

These confirmatory results use seed 0, 10,000 independently generated datasets per scenario, and 20,000 production Bayesian-bootstrap draws per dataset. The run completed on 2026-08-17. Coverage is compared with the four-standard-error binomial range around its nominal level.

| Scenario | Bias | Sign accuracy | 50% coverage / width | 80% coverage / width | 98% coverage / width |
|---|---:|---:|---:|---:|---:|
| n=10, null, no drift | -0.000453 | n/a | 0.3856 / 0.026067 | 0.6573 / 0.049268 | 0.8742 / 0.086972 |
| n=20, small effect, common drift | +0.000317 | 0.9195 | 0.4350 / 0.020776 | 0.7197 / 0.039580 | 0.9445 / 0.072172 |
| n=30, clear effect, differential drift | +0.000089 | 1.0000 | 0.4557 / 0.017737 | 0.7511 / 0.033812 | 0.9552 / 0.061934 |
| n=50, null, run-order bias | +0.000117 | n/a | 0.4820 / 0.014266 | 0.7653 / 0.027189 | 0.9666 / 0.049791 |
| n=75, small effect, combined drift and order | +0.000063 | 0.9968 | 0.4752 / 0.011897 | 0.7796 / 0.022660 | 0.9714 / 0.041437 |
| n=100, clear effect, paired common-mode noise | +0.000020 | 1.0000 | 0.4799 / 0.003907 | 0.7822 / 0.007437 | 0.9729 / 0.013584 |
| n=30, small effect, substantial difference noise | +0.000201 | 0.7782 | 0.4557 / 0.039909 | 0.7511 / 0.076076 | 0.9552 / 0.139352 |

The nominal four-standard-error coverage ranges are `[0.4800, 0.5200]`, `[0.7840, 0.8160]`, and `[0.9744, 0.9856]` for the 50%, 80%, and 98% intervals. The results are descriptive validation of the nonparametric posterior; no parametric correction is applied.
