# `foil`

NB: `foil` is currently experimental, the API may change without warning.

---

`foil` helps you compare two Git revisions, typically `main` and a branch, to understand how your changes affect performance. It is designed for CI but also works during local development. `foil` runs the revisions in pairs and uses a Bayesian bootstrap with a small regression to estimate the branch's effect, account for drift and run order, and report the uncertainty around the results.

## Usage

```sh
foil --baseline main --candidate HEAD --repetitions 30 --interval 0.5 0.8 0.9 --output-dir benchmark/ -- cargo bench
```

Run `foil --help` for the full set of options.

Each revision runs in its own clean worktree, so local changes are never part of a measurement; `foil` warns when tracked files have been modified.

On Linux, `foil` requires Linux 5.14 or newer and a writable delegated cgroup v2. Run `foil` from inside the delegated subtree; `FOIL_CGROUP_ROOT` only selects an existing delegation.

## Configuration

The flags above can also be set in a TOML file, keyed by their long names. `foil` reads `foil.toml` from the working directory when present, or the file given by `--config`.

```toml
baseline = "main"
candidate = "HEAD"
repetitions = 30
block-size = 4
interval = [0.5, 0.8, 0.9]
output-dir = "benchmark/"
```

With that file, the run above is `foil -- cargo bench`. Arguments override the file, which overrides the built-in defaults. `foil --help` always shows only the built-in CLI defaults. The command may also live in the file as a `command` list; one passed after `--` overrides it.

Run order uses small-block randomization. The default `block-size = 4` gives each full block two baseline-first and two candidate-first pairs; `block-size = 1` is the minimum.

Lifecycle hooks are configured only in TOML. `suite-startup` runs once in the original checkout before revision worktrees are created; `suite-teardown` runs there after every worktree has been removed. `worktree-startup` and `worktree-teardown` run once in each newly created baseline or candidate worktree. Top-level `startup-each-run` and `teardown-each-run` surround every measured command, while benchmark-local lifecycle hooks apply only to that benchmark.

```toml
suite-startup = ["docker", "compose", "up", "-d"]
suite-teardown = ["docker", "compose", "down"]
worktree-startup = ["git", "submodule", "update", "--init"]
worktree-teardown = ["git", "clean", "-fdx"]
startup-each-run = ["reset-global-state"]
teardown-each-run = ["collect-global-state"]

[benchmarks.parse]
startup = ["cargo", "build", "--release"]
startup-each-run = ["reset-database"]
teardown-each-run = ["collect-logs"]
command = ["./target/release/parse", "corpus/"]
```

Benchmark lifecycle commands share the benchmark's `working-directory` and `env`. Their stdout and stderr are discarded, like measured commands'; redirect explicitly if the output matters. The first Ctrl-C interrupts active startup or benchmark work, then teardown unwinds on a protected cleanup wait. A second Ctrl-C exits immediately. Teardown is also attempted after startup, benchmark, or timeout failures; the original error remains primary and additional cleanup errors are reported alongside it. On macOS, containment uses a process group, which a descendant can deliberately escape with `setsid` or `setpgid`.

A `[benchmarks]` table is where a command belongs in TOML. Each entry names a benchmark for `--benchmark` to select and typically sets its own `command`; it may override ordinary options, and anything it leaves unset, including `command`, is inherited from the top level. Benchmark lifecycle commands are local to that benchmark. Its `env` table is merged with the top-level one, variable by variable, with the benchmark's values winning on conflicts:

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

`foil --benchmark render` runs with 50 repetitions in `benchmarks/render`; `foil --benchmark parse` runs with the top-level 10. An explicit argument still overrides a benchmark's setting, except for `command`, `working-directory`, and `env`. Those define what a benchmark is, so one argument cannot sensibly stand in for all of the selected benchmarks, and passing one alongside a benchmark is an error. Lifecycle hooks are TOML-only.
`working-directory` must be a relative path within the worktree; absolute paths and `..` are rejected.

With no `--benchmark`, every benchmark in the table runs in declaration order, each in its own `--output-dir` subdirectory named after it. Pass `--benchmark render parse` to run only some of them in the order given. A configuration with no `[benchmarks]` table always runs a single, unnamed command, exactly as with no configuration file at all.

One seed is drawn for the suite when `seed` is omitted and recorded in every benchmark's `config.json`. Fixed domain constants derive separate schedule and posterior streams from it. Benchmarks share the same baseline and candidate worktrees by default. Set `isolate = true` on a benchmark that needs a fresh pair, or pass `--isolate` to isolate every selected benchmark.

The intended workflow is a `foil.toml` with named benchmarks, run with a plain `foil`. For an unnamed run, a trailing `-- <COMMAND>...` replaces the configured top-level command while keeping the other configured options. Named benchmarks keep their configured commands.

## Output

Each run writes to its output directory:

- `config.json`: run's parameters and resolved revisions.
- `benchmark.log`: one JSON line per individual run.
- `measurements.csv`: paired baseline/candidate timings.
- `posterior.csv`: Bayesian bootstrap draws.
- `report.txt`: human-readable summary.

Library callers can pass `measurements.csv` to `analyze_measurements`; the same seed, draw count, shrinkage, and intervals reproduce the CLI posterior exactly.

Named benchmark reports are prefixed with the benchmark name when printed.

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
