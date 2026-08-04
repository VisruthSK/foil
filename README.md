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

A `[benchmarks]` table names benchmarks for `--benchmark` to select. Each one sets `command` and may override any option above, including `working-directory` and `env`:

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

With no `--benchmark`, every benchmark in the table runs, each in its own `--output-dir` subdirectory named after it. Pass `--benchmark render parse` to run only some of them. A configuration with no `[benchmarks]` table always runs the single command above, unnamed, exactly as without one.

The intended workflow is a `b3.toml` with named benchmarks, run with a plain `b3`; the trailing `-- <COMMAND>...` is there for one-off, unconfigured runs.

## Output

Each run writes to its output directory:

- `config.json`: run's parameters and resolved revisions.
- `benchmark.log`: one JSON line per individual run.
- `measurements.csv`: paired baseline/candidate timings.
- `posterior.csv`: Bayesian bootstrap draws.
- `report.txt`: human-readable summary.

Running more than one benchmark also prints a one-line-per-benchmark summary and writes it to `report_short.txt` in `--output-dir`:

```
ggplot2: 1.2s -> 554.0ms [-52.41%, -51.31%]
dplyr: 3.1s -> 3.0s [-4.02%, +1.15%]
```

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
