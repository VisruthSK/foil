# Runner overhead

Runner overhead is measured separately from posterior calibration. An ignored release-mode test runs the same synthetic child through `std::process::Command::output()` and both B3 runner modes. The default mode uses `Command::output()` directly. The timeout mode adds a process group, concurrent pipe readers, bounded waiting, and process-tree termination. The child reports its own controlled work, so subtracting internal time before taking each paired difference removes the requested workload from the comparison.

Run the diagnostic with:

```sh
cargo test --release run::overhead::process_runner_overhead -- --ignored --nocapture
```

It uses 200 paired repetitions for each runner mode at 0, 1, 10, and 100 milliseconds of controlled internal work. Pair order uses seed-0 blocks of four with two raw-first and two B3-first pairs. `B3_OVERHEAD_RUNS` changes the repetitions. The report includes mean, median, p90, p99, minimum, maximum, and the number of negative paired differences.

This is intentionally diagnostic rather than a pass/fail test. Process startup and scheduler behavior are machine- and operating-system-dependent. Inference calibration remains fully synthetic and deterministic, so runner overhead cannot be mistaken for posterior error.

## Reference Windows result

The 2026-08-18 Windows x86-64 run used 50 paired repetitions per workload. Times are incremental overhead relative to `Command::output()`.

| Mode | Controlled work | Mean | Median | p99 | Minimum | Maximum |
|:---|---:|---:|---:|---:|---:|---:|
| default | 0 ms | 0.270 ms | 0.295 ms | 4.039 ms | -3.804 ms | 4.039 ms |
| default | 1 ms | 0.072 ms | 0.066 ms | 2.442 ms | -3.922 ms | 2.442 ms |
| default | 10 ms | 0.351 ms | 0.530 ms | 3.744 ms | -2.820 ms | 3.744 ms |
| default | 100 ms | 0.151 ms | -0.147 ms | 5.044 ms | -9.163 ms | 5.044 ms |
| timeout | 0 ms | 53.594 ms | 56.017 ms | 66.978 ms | 36.711 ms | 66.978 ms |
| timeout | 1 ms | 48.739 ms | 48.936 ms | 68.240 ms | 31.497 ms | 68.240 ms |
| timeout | 10 ms | 44.644 ms | 48.598 ms | 57.023 ms | 30.880 ms | 57.023 ms |
| timeout | 100 ms | 39.892 ms | 36.796 ms | 61.165 ms | 26.662 ms | 61.165 ms |

The default differences fluctuate around zero, so its overhead is small relative to process-launch noise. The timeout machinery remains expensive on this machine. Users who enable `--timeout` should measure enough work per command for that fixed cost to be negligible or batch repeated work inside one command. These results are diagnostic, not portable guarantees.
