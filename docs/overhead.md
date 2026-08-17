# Runner overhead

Runner overhead is measured separately from posterior calibration. An ignored release-mode test runs the same synthetic child through both `std::process::Command::output()` and B3's process runner. The child reports its own controlled work; subtracting each internal time and then taking the paired difference isolates B3's incremental process-group, reader-thread, timing, and wait machinery. Both paths pay executable startup and pipe capture.

Run the diagnostic with:

```sh
cargo test --release run::overhead::process_runner_overhead -- --ignored --nocapture
```

It uses 200 paired repetitions at each of 0, 1, 10, and 100 milliseconds of controlled internal work by default. Pair order uses seed-0 blocks of four with two raw-first and two B3-first pairs. `B3_OVERHEAD_RUNS` changes the repetitions. The report includes mean, median, p90, p99, minimum, maximum, and the number of negative paired differences.

This is intentionally diagnostic rather than a pass/fail test. Process startup and scheduler behavior are machine- and operating-system-dependent. Inference calibration remains fully synthetic and deterministic, so runner overhead cannot be mistaken for posterior error.

## Reference Windows result

The 2026-08-17 Windows x86-64 run used 200 paired repetitions per workload:

| Controlled work | Mean | Median | p90 | p99 | Minimum | Maximum |
|---:|---:|---:|---:|---:|---:|---:|
| 0 ms | 48.889 ms | 45.659 ms | 66.132 ms | 71.038 ms | 33.568 ms | 72.969 ms |
| 1 ms | 53.081 ms | 53.906 ms | 66.124 ms | 74.134 ms | 38.635 ms | 76.720 ms |
| 10 ms | 60.286 ms | 62.740 ms | 69.375 ms | 76.281 ms | 39.263 ms | 82.164 ms |
| 100 ms | 48.087 ms | 45.118 ms | 64.180 ms | 71.900 ms | 38.031 ms | 77.582 ms |

No paired difference was negative. These values describe this machine and runner backend, not a portable performance guarantee. They show that short commands can be dominated by process-group runner overhead on Windows; benchmarks should do enough work per invocation for that fixed cost to be negligible or should measure repeated work inside one command.
