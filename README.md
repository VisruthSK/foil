# `foil`

_`git diff` for performance._

> **Experimental:** `foil`'s API may change without warning.

`foil` compares the performance of two Git revisions, typically `main` and a branch. It is language and benchmark agnostic: it measures the execution time of arbitrary commands, without requiring a particular benchmarking framework.

Designed for CI, especially pull requests, `foil` also works locally. It runs both revisions in randomized pairs and uses a Bayesian bootstrap with a small regression to estimate the candidate's effect, account for drift and run order, and report uncertainty.

## Usage

```sh
foil --baseline main --candidate HEAD --repetitions 60 -- COMMAND [ARGS...]
```

`foil` measures the elapsed time of the entire command. Each revision runs in its own clean Git worktree, excluding uncommitted source changes from the measurements. Note that `repetitions` counts pairs: 60 repetitions means 60 baseline runs and 60 candidate runs. Run `foil --help` for all available options.

## Configuration

For repeated use, we recommend a `foil.toml` configuration file:

```toml
baseline = "main"
candidate = "HEAD"
repetitions = 60
block-size = 4
interval = [0.5, 0.8, 0.9]
output-dir = "benchmark/"
command = ["./benchmarks/run"]
```

With this file, simply run `foil`. The command can also be passed after `--` for an unnamed benchmark, overriding the configured command.

`foil` reads `foil.toml` from the working directory, or a file specified by `--config`. CLI options take precedence over the configuration file, which takes precedence over built-in defaults.

### Multiple benchmarks

Define multiple benchmarks using `[benchmarks]`. For example, `foil` uses the following configuration to benchmark its own release builds and tests:

```toml
repetitions = 30
block-size = 4
output-dir = "benchmark/results"
timeout = 1800
worktree-startup = ["cargo", "fetch", "--locked"]

[benchmarks.release-compile]
startup-each-run = ["cargo", "clean", "--release"]
command = ["cargo", "build", "--release", "--locked", "--offline"]

[benchmarks.release-test]
startup = ["cargo", "test", "--release", "--locked", "--offline", "--no-run"]
command = ["cargo", "test", "--release", "--locked", "--offline"]
```

Here, `release-compile` cleans before every measured build, while `release-test` compiles the tests once before measuring them. Lifecycle hooks are excluded from the measured execution time.

Running `foil` executes all benchmarks in declaration order, each in its own output subdirectory. Use `foil --benchmark release-test` to run only the test benchmark.

Benchmarks inherit top-level options unless overridden and share worktrees by default. Set `isolate = true` on a benchmark, or use `--isolate` for all selected benchmarks, to give them fresh worktrees.

## Output

`foil` reports the estimated performance difference and its uncertainty. For example, benchmarking `foil`'s own release compilation produced:

```text
release-compile: Comparing candidate (HEAD) to baseline (main) with 30 paired repetitions and 10000 Bayesian bootstrap draws.

Baseline:  25.7s
Candidate: 33.3s

Change: +7.7s (+29.90%)
  50% CrI: [+7.4s, +7.9s] (+28.82%, +30.89%)
  80% CrI: [+7.2s, +8.1s] (+27.79%, +31.74%)
  90% CrI: [+7.0s, +8.2s] (+27.12%, +32.25%)

P(candidate faster): 0.0% (0 of 10,000 draws)
```

Positive changes indicate slower candidate runs. `foil` reports the estimated effect and its uncertainty, leaving its practical importance to the user.

Each benchmark also writes the following files to its output directory:

- `config.json`: resolved configuration and revisions.
- `benchmark.log`: one JSON line per individual run.
- `measurements.csv`: paired baseline and candidate timings.
- `posterior.csv`: Bayesian bootstrap draws.
- `report.txt`: human-readable summary.

## Future Work and Details

Planned work includes process-tree peak memory measurement on Linux and Windows, a documentation site, and a `crates.io` release.

For more information on `foil`'s design, statistical methodology, and planned development, see the [2026 R Consortium ISC grant proposal](https://visruthsk.github.io/foil-ISC-2026/).

## License

Dual licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
