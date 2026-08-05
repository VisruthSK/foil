# b3

NB: `b3` is currently experimental, the API may change without warning.

---

`b3` helps you compare two Git revisions, typically `main` and a branch, to understand how your changes affect performance. It is designed for CI but also works during local development. `b3` runs the revisions in pairs and uses a Bayesian bootstrap with a small regression to estimate the branch's effect, account for drift and run order, and report the uncertainty around the results.

## Usage

```sh
b3 --baseline main --candidate HEAD --repetitions 30 --interval 0.5 0.8 0.98 --output-dir benchmark/ -- cargo bench
```

Run `b3 --help` for the full set of options.

## Configuration

The flags above can also be set in a TOML file, keyed by their long names. `b3` reads `b3.toml` from the working directory when present, or the file given by `--config`.

```toml
baseline = "main"
candidate = "HEAD"
repetitions = 30
interval = [0.5, 0.8, 0.98]
output-dir = "benchmark/"
```

With that file, the run above is `b3 -- cargo bench`. Arguments override the file, which overrides the built-in defaults. `b3 --help` always shows only the built-in CLI defaults. The command may also live in the file as a `command` list; one passed after `--` overrides it.

`setup` and `teardown` are commands `b3` runs once in each worktree, before the first and after the last measured run, and never times. A `setup` is where a build belongs, so that compilation stays out of the measurements. Both share the benchmark's `working-directory` and `env`.

```toml
setup = ["cargo", "build", "--release"]
command = ["./target/release/parse", "corpus/"]
```

Each runs once per revision, so a side effect reaching outside the worktree happens twice, once for the baseline and once for the candidate. A failing `setup` or `teardown` stops the run and reports what the command printed, and `teardown` still runs when a benchmark fails. On the command line these take a bare command, as in `--setup make`; one carrying flags of its own, like `cargo build --release`, belongs in the configuration file as a list.

A `[benchmarks]` table is where commands belong in TOML. Each entry names a benchmark for `--benchmark` to select and must set its own `command`; it may override run options such as `repetitions`, `working-directory`, `isolate`, `setup`, and `teardown`. An empty list such as `setup = []` clears an inherited setup or teardown. `baseline`, `candidate`, and `seed` apply to the whole suite. A benchmark's `env` table is merged with the top-level one variable by variable, with the benchmark's values winning on conflicts:

```toml
repetitions = 10
draws = 20000

[benchmarks.parse]
command = ["cargo", "run", "--release", "--", "parse"]

[benchmarks.render]
repetitions = 50
working-directory = "benchmarks/render"
command = ["cargo", "run", "--release", "--", "render"]

[benchmarks.render.env]
RAYON_NUM_THREADS = "1"
```

`b3 --benchmark render` runs with 50 repetitions in `benchmarks/render`; `b3 --benchmark parse` runs with the top-level 10. An explicit argument still overrides a benchmark's setting.
`working-directory` must be a relative path within the worktree; absolute paths and `..` are rejected.

With no `--benchmark`, every benchmark in the table runs in declaration order, each in its own `--output-dir` subdirectory named after it. Pass `--benchmark render parse` to run only some of them in the order given. A configuration with no `[benchmarks]` table always runs a single, unnamed command, exactly as with no configuration file at all.

One seed is drawn for the suite when `seed` is omitted, recorded in every benchmark's `config.json`, and used for every benchmark's run schedule and bootstrap. Benchmarks share the same baseline and candidate worktrees by default. Set `isolate = true` on a benchmark that needs a fresh pair, or pass `--isolate` to isolate every selected benchmark.

The intended workflow is a `b3.toml` with named benchmarks, run with a plain `b3`; the trailing `-- <COMMAND>...` is for one-off runs that skip configuration entirely.

## Output

Each run writes to its output directory:

- `config.json`: run's parameters and resolved revisions.
- `benchmark.log`: one JSON line per individual run.
- `measurements.csv`: paired baseline/candidate timings.
- `posterior.csv`: Bayesian bootstrap draws.
- `report.txt`: human-readable summary.

Running more than one benchmark also prints a one-line-per-benchmark summary and writes it to `report_short.txt` in `--output-dir`:

```
parse: 1.2s -> 554.0ms [-52.41%, -51.31%]
render: 3.1s -> 3.0s [-4.02%, +1.15%]
```

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
