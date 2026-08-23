# b3

NB: `b3` is currently experimental, the API may change without warning.

---

`b3` helps you compare two Git revisions, typically `main` and a branch, to understand how your changes affect performance. It is designed for CI but also works during local development. `b3` runs the revisions in pairs and uses a Bayesian bootstrap with a small regression to estimate the branch's effect, account for drift and run order, and report the uncertainty around the results.

## Usage

```sh
b3 --baseline main --candidate HEAD --repetitions 30 --interval 0.5 0.8 0.98 --output-dir benchmark/ -- cargo bench
```

Run `b3 --help` for the full set of options.

Each revision runs in its own clean worktree, so local changes are never part of a measurement; `b3` warns when tracked files have been modified.

On Linux, `b3` requires pidfd support (Linux 5.3 or newer) and a writable delegated cgroup v2. Run `b3` from inside the delegated subtree; `B3_CGROUP_ROOT` only selects an existing delegation.

## Configuration

The flags above can also be set in a TOML file, keyed by their long names. `b3` reads `b3.toml` from the working directory when present, or the file given by `--config`.

```toml
baseline = "main"
candidate = "HEAD"
repetitions = 30
block-size = 4
interval = [0.5, 0.8, 0.98]
output-dir = "benchmark/"
```

With that file, the run above is `b3 -- cargo bench`. Arguments override the file, which overrides the built-in defaults. `b3 --help` always shows only the built-in CLI defaults. The command may also live in the file as a `command` list; one passed after `--` overrides it.

Run order uses small-block randomization. The default `block-size = 4` gives each full block two baseline-first and two candidate-first pairs; `block-size = 1` is the minimum.

Lifecycle commands surround the suite, each benchmark, or every measured run. Top-level `startup` and `teardown` run once in the original checkout around the whole suite. The same keys in a benchmark run once in each revision worktree around that benchmark. `startup-each-run` and `teardown-each-run` run outside every timed interval; suite and benchmark commands compose, with teardown unwinding in reverse order.

```toml
startup = ["docker", "compose", "up", "-d"]
startup-each-run = ["reset-database"]
teardown-each-run = ["collect-logs"]
teardown = ["docker", "compose", "down"]

[benchmarks.parse]
startup = ["cargo", "build", "--release"]
command = ["./target/release/parse", "corpus/"]
```

Benchmark lifecycle commands share the benchmark's `working-directory` and `env`. Successful lifecycle output is suppressed; failures report both nonempty streams under explicit labels. `b3` discards stdout and stderr from measured commands. If output is part of the workload, redirect it explicitly in the benchmark command. Teardown is still attempted after startup, benchmark, timeout, or interruption failures; the original error remains primary and additional cleanup errors are also reported.

A `[benchmarks]` table is where a command belongs in TOML. Each entry names a benchmark for `--benchmark` to select and typically sets its own `command`; it may override ordinary options, and anything it leaves unset, including `command`, is inherited from the top level. Lifecycle commands are not inherited: suite and benchmark lifecycles remain distinct and compose. Its `env` table is merged with the top-level one, variable by variable, with the benchmark's values winning on conflicts:

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

`b3 --benchmark render` runs with 50 repetitions in `benchmarks/render`; `b3 --benchmark parse` runs with the top-level 10. An explicit argument still overrides a benchmark's setting, except for `command`, `working-directory`, and `env`. Those define what a benchmark is, so one argument cannot sensibly stand in for all of the selected benchmarks, and passing one alongside a benchmark is an error. Lifecycle arguments apply to the suite.
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

Library callers can pass `measurements.csv` to `analyze_measurements`; the same seed, draw count, shrinkage, and intervals reproduce the CLI posterior exactly.

Running more than one benchmark also prints a one-line-per-benchmark summary and writes it to `report_short.txt` in `--output-dir`:

```
parse: 1.2s -> 554.0ms [-52.41%, -51.31%]
render: 3.1s -> 3.0s [-4.02%, +1.15%]
```

## Statistical validation

The ignored release-mode [statistical validation suite](docs/statistical-validation.md) reports synthetic exact recovery, credible-interval coverage, shrinkage behavior, and model misspecification.

Simulation shows that the reported intervals materially under-cover at small repetition counts, including the default 30.

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
