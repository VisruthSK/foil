# b3

NB: `b3` is currently experimental, the API may change without warning.

---

`b3` helps you compare two Git revisions, typically `main` and a branch, to understand how your changes affect performance. It is designed for CI but also works during local development. `b3` runs the revisions in pairs and uses a Bayesian bootstrap with a small regression to estimate the branch's effect, account for drift and run order, and report the uncertainty around the results.

## Usage

```sh
b3 --baseline main --candidate HEAD --repetitions 30 --output-dir benchmark/ -- Rscript benchmark.R
```

Run `b3 --help` for the full set of options.

## Configuration

Options can also be set in a TOML file, keyed by the long name of the option, plus `command` for the benchmark itself. `b3` reads `b3.toml` from the working directory when it is present, or the file given by `--config`.

```toml
baseline = "main"
candidate = "HEAD"
repetitions = 30
interval = [0.5, 0.8, 0.98]
output-dir = "benchmark/"
command = ["Rscript", "benchmark.R"]
```

With that file, the run above is just `b3`. Arguments override the file, which overrides the builtin defaults, and `b3 --help` reports the defaults the file leaves in place.

A `[benchmarks]` table names benchmarks for `--benchmark` to select. Each one sets `command` and may override any option above, plus set `working-directory` and `env`:

```toml
repetitions = 10
draws = 20000

[benchmarks.parse]
command = ["cargo", "run", "--release", "--", "parse"]

[benchmarks.render]
command = ["cargo", "run", "--release", "--", "render"]
working-directory = "benchmarks/render"
repetitions = 50

[benchmarks.render.env]
RAYON_NUM_THREADS = "1"
```

`b3 --benchmark render` runs with 50 repetitions in `benchmarks/render`; `b3 --benchmark parse` runs with the top-level 10. An explicit argument still overrides a benchmark's setting.

## Output

Each run writes to `--output-dir`:

- `config.json`: run's parameters and resolved revisions.
- `benchmark.log`: one JSON line per individual run.
- `measurements.csv`: paired baseline/candidate timings.
- `posterior.csv`: Bayesian bootstrap draws.
- `report.txt`: human-readable summary.

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
