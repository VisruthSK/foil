use crate::analysis::analyze_checked;
use crate::config::{Cli, Lifecycle, ResolvedSuiteConfig, RunConfig, Suite};
use crate::platform::{CommandSpec, Interrupt};
use crate::{
    BenchmarkLog, CommandTemplate, Config, LifecycleConfig, MeasurementsCsv, Pair, Repetition,
    Repetitions, Revision, RunOrder, RunOutput, Side, Summary, Time, Worktree, run_unmeasured,
    working_tree_has_modified_tracked_files, write_config_json, write_posterior_csv,
};

use anyhow::{Context, Result, ensure};
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use std::{
    ffi::OsString,
    fs,
    io::{BufWriter, ErrorKind},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use tempfile::{TempDir, tempdir};

struct Worktrees {
    pair: Pair<Worktree>,
    _directory: TempDir,
}

struct LifecycleCommands {
    startup: Option<CommandTemplate>,
    startup_each_run: Option<CommandTemplate>,
    teardown_each_run: Option<CommandTemplate>,
    teardown: Option<CommandTemplate>,
}

impl LifecycleCommands {
    fn new(
        lifecycle: &Lifecycle,
        working_directory: Option<PathBuf>,
        envs: &[(String, String)],
    ) -> Self {
        Self {
            startup: build_command(&lifecycle.startup, working_directory.clone(), envs),
            startup_each_run: build_command(
                &lifecycle.startup_each_run,
                working_directory.clone(),
                envs,
            ),
            teardown_each_run: build_command(
                &lifecycle.teardown_each_run,
                working_directory.clone(),
                envs,
            ),
            teardown: build_command(&lifecycle.teardown, working_directory, envs),
        }
    }
}

static INTERRUPTS: AtomicU8 = AtomicU8::new(0);

fn interrupted() -> bool {
    INTERRUPTS.load(Ordering::Relaxed) != 0
}

fn repeated_interrupt() -> bool {
    INTERRUPTS.fetch_add(1, Ordering::Relaxed) != 0
}

fn handle_interrupt(interrupt: &Interrupt) {
    if repeated_interrupt() {
        process::exit(130);
    }
    interrupt.signal();
}

pub(crate) fn run() -> Result<()> {
    let interrupt = Interrupt::new().context("Failed to create the interrupt handle.")?;
    let signal = interrupt.clone();
    ctrlc::set_handler(move || handle_interrupt(&signal))
        .context("Failed to set the Ctrl+C handler.")?;

    let Suite {
        config: suite,
        lifecycle,
        output_dir: suite_output_dir,
        runs,
    } = Cli::suite()?;

    clear_outputs(&suite_output_dir, &runs)?;
    write_configs(&suite, &lifecycle, &runs)?;

    let startup = build_command(&lifecycle.startup, None, &[]);
    let teardown = build_command(&lifecycle.teardown, None, &[]);
    scoped(
        run_at(
            startup.as_ref(),
            Path::new("."),
            &interrupt,
            "suite startup",
        ),
        || execute_suite(&suite, &lifecycle, &suite_output_dir, runs, &interrupt),
        || {
            run_at(
                teardown.as_ref(),
                Path::new("."),
                &interrupt,
                "suite teardown",
            )
        },
    )
}

fn execute_suite(
    suite: &ResolvedSuiteConfig,
    lifecycle: &Lifecycle,
    suite_output_dir: &Path,
    runs: Vec<(Option<String>, RunConfig)>,
    interrupt: &Interrupt,
) -> Result<()> {
    if working_tree_has_modified_tracked_files()? {
        eprintln!(
            "Warning: the working tree has modified tracked files, which are never benchmarked."
        );
    }
    let multiple = runs.len() > 1;
    let revisions = Pair {
        baseline: suite.baseline.clone(),
        candidate: suite.candidate.clone(),
    };
    let shared = create_worktrees(&revisions)?;

    let mut compact = Vec::with_capacity(runs.len());

    for (name, config) in runs {
        ensure!(!interrupted(), "Interrupted.");
        let output_dir = output_directory(name.as_deref(), &config);
        let heading = if multiple { name.as_deref() } else { None };
        let isolated = config
            .isolate
            .then(|| create_worktrees(&revisions))
            .transpose()?;
        let worktrees = isolated.as_ref().unwrap_or(&shared);

        let summary = compare(
            suite,
            lifecycle,
            config,
            &worktrees.pair,
            &output_dir,
            heading,
            interrupt,
        )?;

        if let Some(name) = name {
            compact.push(format!("{name}: {}", summary.compact()));
        }
    }

    if multiple {
        let report = format!("{}\n", compact.join("\n"));
        print!("{report}");

        let report_path = suite_output_dir.join("report_short.txt");
        fs::write(&report_path, &report)
            .with_context(|| format!("Failed to write {}.", report_path.display()))?;
    }

    Ok(())
}

fn create_worktrees(revisions: &Pair<Revision>) -> Result<Worktrees> {
    let directory = tempdir().context("Failed to create temporary directory.")?;
    let pair = Pair {
        baseline: Worktree::create(
            directory.path().join("baseline"),
            revisions.baseline.clone(),
        )?,
        candidate: Worktree::create(
            directory.path().join("candidate"),
            revisions.candidate.clone(),
        )?,
    };

    Ok(Worktrees {
        _directory: directory,
        pair,
    })
}

fn output_directory(name: Option<&str>, config: &RunConfig) -> std::path::PathBuf {
    name.map_or_else(
        || config.output_dir.clone(),
        |name| config.output_dir.join(name),
    )
}

fn clear_outputs(suite_output_dir: &Path, runs: &[(Option<String>, RunConfig)]) -> Result<()> {
    let mut result = Ok(());
    for (name, config) in runs {
        let output_dir = output_directory(name.as_deref(), config);
        for artifact in [
            "config.json",
            "benchmark.log",
            "measurements.csv",
            "posterior.csv",
            "report.txt",
        ] {
            result = combine(result, remove_generated(&output_dir.join(artifact)));
        }
    }
    combine(
        result,
        remove_generated(&suite_output_dir.join("report_short.txt")),
    )
}

fn remove_generated(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to remove {}.", path.display())),
    }
}

fn write_configs(
    suite: &ResolvedSuiteConfig,
    suite_lifecycle: &Lifecycle,
    runs: &[(Option<String>, RunConfig)],
) -> Result<()> {
    for (name, config) in runs {
        let output_dir = output_directory(name.as_deref(), config);
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create {}.", output_dir.display()))?;
        let path = output_dir.join("config.json");
        write_config_json(
            &path,
            &Config {
                seed: suite.seed,
                repetitions: config.repetitions.get(),
                block_size: config.block_size.get(),
                draws: config.draws.get(),
                timeout_seconds: config.timeout.map(|seconds| seconds.get()),
                isolate: config.isolate,
                shrinkage: config.shrinkage,
                intervals: &config.intervals,
                working_directory: config.working_directory.as_deref(),
                baseline: &suite.baseline,
                candidate: &suite.candidate,
                suite_lifecycle: lifecycle_config(suite_lifecycle),
                benchmark_lifecycle: lifecycle_config(&config.lifecycle),
                command: &config.command,
            },
        )
        .with_context(|| format!("Failed to write {}.", path.display()))?;
    }
    Ok(())
}

fn compare(
    suite: &ResolvedSuiteConfig,
    suite_lifecycle: &Lifecycle,
    config: RunConfig,
    worktrees: &Pair<Worktree>,
    output_dir: &Path,
    heading: Option<&str>,
    interrupt: &Interrupt,
) -> Result<Summary<Time>> {
    let RunConfig {
        isolate: _,
        shrinkage,
        output_dir: _,
        repetitions: repetition_count,
        block_size,
        draws,
        timeout,
        intervals,
        working_directory,
        envs,
        lifecycle,
        command,
    } = config;

    let mut schedule_rng = Xoshiro256PlusPlus::seed_from_u64(crate::seed::schedule(suite.seed));

    let suite_commands = LifecycleCommands::new(suite_lifecycle, working_directory.clone(), &envs);
    let benchmark_commands = LifecycleCommands::new(&lifecycle, working_directory.clone(), &envs);
    // Clap requires at least one command argument.
    let benchmark_template = build_command(&command, working_directory.clone(), &envs)
        .expect("the benchmark command is non-empty");
    let benchmark = Pair {
        baseline: benchmark_template.at(worktrees.baseline.path()),
        candidate: benchmark_template.at(worktrees.candidate.path()),
    };
    let timeout = timeout.map(|seconds| Duration::from_secs(seconds.get()));

    let repetition_count = repetition_count.get();
    let schedule = RunOrder::schedule(repetition_count, block_size, &mut schedule_rng);

    let repetitions = scoped(
        run_startup_both(
            benchmark_commands.startup.as_ref(),
            worktrees,
            interrupt,
            "startup",
        ),
        || {
            ensure!(!interrupted(), "Interrupted.");
            measure_all(
                &benchmark,
                &suite_commands,
                &benchmark_commands,
                worktrees,
                &schedule,
                output_dir,
                interrupt,
                timeout,
            )
        },
        || {
            run_teardown_both(
                benchmark_commands.teardown.as_ref(),
                worktrees,
                interrupt,
                "teardown",
            )
        },
    )?;

    let analysis = analyze_checked(
        &repetitions,
        suite.seed,
        draws,
        shrinkage,
        &intervals,
        || {
            ensure!(!interrupted(), "Interrupted.");
            Ok(())
        },
    )?;
    ensure!(!interrupted(), "Interrupted.");

    let posterior_path = output_dir.join("posterior.csv");
    write_posterior_csv(&posterior_path, &analysis.posterior)
        .with_context(|| format!("Failed to write {}.", posterior_path.display()))?;

    let summary = analysis.summary;

    let prefix = heading.map_or_else(String::new, |name| format!("{name}: "));
    let report = format!(
        "{prefix}Comparing candidate ({}) to baseline ({}) with {repetition_count} paired repetitions and {} Bayesian bootstrap draws.\n\n{summary}",
        worktrees.candidate.revision().name(),
        worktrees.baseline.revision().name(),
        draws.get(),
    );
    print!("{report}");

    let report_path = output_dir.join("report.txt");
    fs::write(&report_path, &report)
        .with_context(|| format!("Failed to write {}.", report_path.display()))?;

    Ok(summary)
}

struct MeasurementContext<'a, W: std::io::Write> {
    benchmark: &'a Pair<CommandSpec>,
    suite: &'a LifecycleCommands,
    benchmark_lifecycle: &'a LifecycleCommands,
    worktrees: &'a Pair<Worktree>,
    log: &'a mut BenchmarkLog<W>,
    interrupt: &'a Interrupt,
    timeout: Option<Duration>,
}

#[allow(clippy::too_many_arguments)]
fn measure_all(
    benchmark: &Pair<CommandSpec>,
    suite: &LifecycleCommands,
    benchmark_lifecycle: &LifecycleCommands,
    worktrees: &Pair<Worktree>,
    schedule: &[RunOrder],
    output_dir: &Path,
    interrupt: &Interrupt,
    timeout: Option<Duration>,
) -> Result<Repetitions> {
    let repetition_count = schedule.len();
    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        BufWriter::new(
            fs::File::create(&log_path)
                .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        ),
        repetition_count * 2,
    );

    let measurements_path = output_dir.join("measurements.csv");
    let mut measurements = MeasurementsCsv::create(&measurements_path)
        .with_context(|| format!("Failed to create {}.", measurements_path.display()))?;

    let mut ctx = MeasurementContext {
        benchmark,
        suite,
        benchmark_lifecycle,
        worktrees,
        log: &mut log,
        interrupt,
        timeout,
    };

    for &order in schedule {
        ensure!(!interrupted(), "Interrupted.");
        let [first, second] = order.sides();

        let mut measure = |side: Side| -> Result<RunOutput> { measure_one(&mut ctx, side) };
        let first_output = measure(first)?;
        let second_output = measure(second)?;

        let outputs = Pair::from_execution_order([first_output, second_output], order);
        let repetition = Repetition { outputs, order };
        measurements
            .append(&repetition)
            .with_context(|| format!("Failed to write {}.", measurements_path.display()))?;
        measured_repetitions.push(repetition);
    }

    // Clears the progress line before the report starts printing.
    drop(log);

    Repetitions::try_from(measured_repetitions)
}

fn measure_one<W: std::io::Write>(
    ctx: &mut MeasurementContext<'_, W>,
    side: Side,
) -> Result<RunOutput> {
    let worktree = ctx.worktrees.get(side);
    ctx.log.phase(side, "suite startup");
    let suite_start = run_in(
        ctx.suite.startup_each_run.as_ref(),
        worktree,
        ctx.interrupt,
        side,
        "suite startup-each-run",
    );
    let body = suite_start.and_then(|()| {
        ctx.log.phase(side, "benchmark startup");
        let benchmark_start = run_in(
            ctx.benchmark_lifecycle.startup_each_run.as_ref(),
            worktree,
            ctx.interrupt,
            side,
            "benchmark startup-each-run",
        );
        let measured = benchmark_start.and_then(|()| {
            ensure!(!interrupted(), "Interrupted.");
            let output =
                ctx.log
                    .measure(ctx.benchmark.get(side), ctx.interrupt, ctx.timeout, side)?;
            ensure!(!interrupted(), "Interrupted.");
            Ok(output)
        });
        ctx.log.phase(side, "benchmark teardown");
        combine(
            measured,
            run_in(
                ctx.benchmark_lifecycle.teardown_each_run.as_ref(),
                worktree,
                ctx.interrupt,
                side,
                "benchmark teardown-each-run",
            ),
        )
    });
    ctx.log.phase(side, "suite teardown");
    combine(
        body,
        run_in(
            ctx.suite.teardown_each_run.as_ref(),
            worktree,
            ctx.interrupt,
            side,
            "suite teardown-each-run",
        ),
    )
}

fn build_command(
    parts: &[OsString],
    working_directory: Option<std::path::PathBuf>,
    envs: &[(String, String)],
) -> Option<CommandTemplate> {
    let (program, arguments) = parts.split_first()?;
    Some(CommandTemplate::new(
        program.clone(),
        arguments.to_vec(),
        working_directory,
        envs.iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
    ))
}

fn lifecycle_config(lifecycle: &Lifecycle) -> LifecycleConfig<'_> {
    LifecycleConfig {
        startup: &lifecycle.startup,
        startup_each_run: &lifecycle.startup_each_run,
        teardown_each_run: &lifecycle.teardown_each_run,
        teardown: &lifecycle.teardown,
    }
}

fn run_in(
    command: Option<&CommandTemplate>,
    worktree: &Worktree,
    interrupt: &Interrupt,
    side: Side,
    phase: &str,
) -> Result<()> {
    run_at(
        command,
        worktree.path(),
        interrupt,
        &format!("{side} {phase}"),
    )
}

fn run_at(
    command: Option<&CommandTemplate>,
    directory: &Path,
    interrupt: &Interrupt,
    label: &str,
) -> Result<()> {
    command.map_or(Ok(()), |command| {
        run_unmeasured(&command.at(directory), interrupt)
            .with_context(|| format!("The {label} failed."))
    })
}

fn combine<T>(primary: Result<T>, cleanup: Result<()>) -> Result<T> {
    if let (Err(_), Err(error)) = (&primary, &cleanup) {
        eprintln!("{error:#}");
    }
    primary.and_then(|value| cleanup.map(|()| value))
}

fn scoped<T>(
    startup: Result<()>,
    body: impl FnOnce() -> Result<T>,
    teardown: impl FnOnce() -> Result<()>,
) -> Result<T> {
    combine(startup.and_then(|()| body()), teardown())
}

/// Runs a lifecycle command on both sides with fail-fast semantics: if the
/// baseline side fails, the candidate side is not run.
fn run_startup_both(
    command: Option<&CommandTemplate>,
    worktrees: &Pair<Worktree>,
    interrupt: &Interrupt,
    phase: &str,
) -> Result<()> {
    let Some(command) = command else {
        return Ok(());
    };
    for side in [Side::Baseline, Side::Candidate] {
        run_unmeasured(&command.at(worktrees.get(side).path()), interrupt)
            .with_context(|| format!("The {side} {phase} failed."))?;
    }
    Ok(())
}

/// Runs a lifecycle command on both sides with best-effort semantics: both
/// sides run regardless of failures; the first error is returned, subsequent
/// errors are printed to stderr.
fn run_teardown_both(
    command: Option<&CommandTemplate>,
    worktrees: &Pair<Worktree>,
    interrupt: &Interrupt,
    phase: &str,
) -> Result<()> {
    let Some(command) = command else {
        return Ok(());
    };

    let mut first_error = None;
    for side in [Side::Baseline, Side::Candidate] {
        if let Err(error) = run_unmeasured(&command.at(worktrees.get(side).path()), interrupt)
            .with_context(|| format!("The {side} {phase} failed."))
        {
            if first_error.is_some() {
                eprintln!("{error:#}");
            } else {
                first_error = Some(error);
            }
        }
    }

    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_interrupt_exits_with_status_130() -> Result<()> {
        let status = process::Command::new(std::env::current_exe()?)
            .args(["--exact", "app::tests::second_interrupt_child", "--ignored"])
            .status()?;
        assert_eq!(status.code(), Some(130));
        Ok(())
    }

    #[test]
    #[ignore]
    fn second_interrupt_child() -> Result<()> {
        INTERRUPTS.store(0, Ordering::Relaxed);
        let interrupt = Interrupt::new()?;
        handle_interrupt(&interrupt);
        handle_interrupt(&interrupt);
        unreachable!("the second interrupt exits")
    }
}
