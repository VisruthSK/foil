use crate::analysis::analyze_checked;
use crate::config::{
    Benchmark, BenchmarkLifecycle, Cli, ResolvedSuiteConfig, RunConfig, Suite, SuiteLifecycle,
    WorktreeLifecycle,
};
use crate::platform::{CommandSpec, Interrupt, Session};
use crate::{
    BenchmarkLifecycleConfig, BenchmarkLog, CommandTemplate, Config, Measurement, MeasurementsCsv,
    Pair, Repetition, Repetitions, Revision, RunOrder, Side, SuiteLifecycleConfig, Summary, Time,
    Worktree, WorktreeLifecycleConfig, run_unmeasured, working_tree_has_modified_tracked_files,
    write_config_json, write_posterior_csv,
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
    teardown: Option<Pair<CommandSpec>>,
}

struct EachRunCommands {
    startup: Option<Pair<CommandSpec>>,
    teardown: Option<Pair<CommandSpec>>,
}

struct Interrupts {
    work: Interrupt,
    cleanup: Interrupt,
}

impl Interrupts {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            work: Interrupt::new()?,
            cleanup: Interrupt::new()?,
        })
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
    let interrupts = Interrupts::new().context("Failed to create the interrupt handles.")?;
    let signal = interrupts.work.clone();
    ctrlc::set_handler(move || handle_interrupt(&signal))
        .context("Failed to set the Ctrl+C handler.")?;

    let Suite {
        config: suite,
        lifecycle,
        worktree_lifecycle,
        runs,
    } = Cli::suite()?;

    clear_outputs(&runs)?;
    write_configs(&suite, &lifecycle, &worktree_lifecycle, &runs)?;
    let mut session = Session::new().context("Failed to create the platform session.")?;

    let startup = build_command(&lifecycle.startup, None, &[]);
    let teardown = build_command(&lifecycle.teardown, None, &[]);
    let mut result = scoped(
        &mut session,
        |session| {
            run_at(
                session,
                startup.as_ref(),
                Path::new("."),
                &interrupts.work,
                "suite startup",
            )
        },
        |session| execute_suite(&suite, &worktree_lifecycle, runs, &interrupts, session),
        |session| {
            run_at(
                session,
                teardown.as_ref(),
                Path::new("."),
                &interrupts.cleanup,
                "suite teardown",
            )
        },
    );
    if result.is_ok() && interrupted() {
        result = Err(anyhow::anyhow!("Interrupted."));
    }
    combine(
        result,
        session
            .shutdown()
            .context("Failed to shut down the platform session."),
    )
}

fn execute_suite(
    suite: &ResolvedSuiteConfig,
    worktree_lifecycle: &WorktreeLifecycle,
    runs: Vec<(Option<String>, Benchmark)>,
    interrupts: &Interrupts,
    session: &mut Session,
) -> Result<()> {
    if working_tree_has_modified_tracked_files()? {
        eprintln!(
            "Warning: the working tree has modified tracked files, which are never benchmarked."
        );
    }
    let revisions = Pair {
        baseline: suite.baseline.clone(),
        candidate: suite.candidate.clone(),
    };
    let mut shared = runs
        .iter()
        .any(|(_, benchmark)| !benchmark.config.isolate)
        .then(|| create_worktrees(&revisions, worktree_lifecycle, interrupts, session))
        .transpose()?;

    let result = (|| {
        for (name, benchmark) in runs {
            ensure!(!interrupted(), "Interrupted.");
            let heading = name.as_deref();
            if benchmark.config.isolate {
                let worktrees =
                    create_worktrees(&revisions, worktree_lifecycle, interrupts, session)?;
                let result = compare(
                    suite,
                    benchmark,
                    &worktrees.pair,
                    heading,
                    interrupts,
                    session,
                );
                combine(result, worktrees.shutdown(session, &interrupts.cleanup))?;
            } else {
                compare(
                    suite,
                    benchmark,
                    &shared.as_ref().expect("shared worktrees were created").pair,
                    heading,
                    interrupts,
                    session,
                )?;
            }
        }
        Ok(())
    })();

    let cleanup = shared.take().map_or(Ok(()), |worktrees| {
        worktrees.shutdown(session, &interrupts.cleanup)
    });
    combine(result, cleanup)
}

fn create_worktrees(
    revisions: &Pair<Revision>,
    lifecycle: &WorktreeLifecycle,
    interrupts: &Interrupts,
    session: &mut Session,
) -> Result<Worktrees> {
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
    let startup = bind_command(&lifecycle.startup, None, &[], &pair);
    let teardown = bind_command(&lifecycle.teardown, None, &[], &pair);
    let worktrees = Worktrees {
        _directory: directory,
        pair,
        teardown,
    };
    let started = run_startup_both(
        session,
        startup.as_ref(),
        &interrupts.work,
        "worktree startup",
    );
    match started {
        Ok(()) => Ok(worktrees),
        Err(error) => Err(combine::<()>(
            Err(error),
            worktrees.shutdown(session, &interrupts.cleanup),
        )
        .unwrap_err()),
    }
}

impl Worktrees {
    fn shutdown(self, session: &mut Session, interrupt: &Interrupt) -> Result<()> {
        run_teardown_both(
            session,
            self.teardown.as_ref(),
            interrupt,
            "worktree teardown",
        )
    }
}

fn output_directory(name: Option<&str>, benchmark: &Benchmark) -> PathBuf {
    let config = &benchmark.config;
    name.map_or_else(
        || config.output_dir.clone(),
        |name| config.output_dir.join(name),
    )
}

fn clear_outputs(runs: &[(Option<String>, Benchmark)]) -> Result<()> {
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
    result
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
    suite_lifecycle: &SuiteLifecycle,
    worktree_lifecycle: &WorktreeLifecycle,
    runs: &[(Option<String>, Benchmark)],
) -> Result<()> {
    for (name, benchmark) in runs {
        let config = &benchmark.config;
        let output_dir = output_directory(name.as_deref(), benchmark);
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
                suite_lifecycle: suite_lifecycle_config(suite_lifecycle),
                worktree_lifecycle: worktree_lifecycle_config(worktree_lifecycle),
                benchmark_lifecycle: benchmark_lifecycle_config(&benchmark.lifecycle),
                command: &config.command,
            },
        )
        .with_context(|| format!("Failed to write {}.", path.display()))?;
    }
    Ok(())
}

fn compare(
    suite: &ResolvedSuiteConfig,
    benchmark_config: Benchmark,
    worktrees: &Pair<Worktree>,
    heading: Option<&str>,
    interrupts: &Interrupts,
    session: &mut Session,
) -> Result<Summary<Time>> {
    let output_dir = output_directory(heading, &benchmark_config);
    let Benchmark { config, lifecycle } = benchmark_config;
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
        command,
    } = config;

    let mut schedule_rng = Xoshiro256PlusPlus::seed_from_u64(crate::seed::schedule(suite.seed));

    let benchmark_commands = EachRunCommands {
        startup: bind_command(
            &lifecycle.startup_each_run,
            working_directory.clone(),
            &envs,
            worktrees,
        ),
        teardown: bind_command(
            &lifecycle.teardown_each_run,
            working_directory.clone(),
            &envs,
            worktrees,
        ),
    };
    let benchmark_startup = bind_command(
        &lifecycle.startup,
        working_directory.clone(),
        &envs,
        worktrees,
    );
    let benchmark_teardown = bind_command(
        &lifecycle.teardown,
        working_directory.clone(),
        &envs,
        worktrees,
    );
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
        session,
        |session| {
            run_startup_both(
                session,
                benchmark_startup.as_ref(),
                &interrupts.work,
                "startup",
            )
        },
        |session| {
            ensure!(!interrupted(), "Interrupted.");
            let context = MeasurementContext {
                benchmark: &benchmark,
                benchmark_lifecycle: &benchmark_commands,
                interrupts,
                session,
                timeout,
            };
            measure_all(context, &schedule, &output_dir, heading)
        },
        |session| {
            run_teardown_both(
                session,
                benchmark_teardown.as_ref(),
                &interrupts.cleanup,
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

struct MeasurementContext<'a> {
    benchmark: &'a Pair<CommandSpec>,
    benchmark_lifecycle: &'a EachRunCommands,
    interrupts: &'a Interrupts,
    session: &'a mut Session,
    timeout: Option<Duration>,
}

fn measure_all(
    mut context: MeasurementContext<'_>,
    schedule: &[RunOrder],
    output_dir: &Path,
    benchmark_name: Option<&str>,
) -> Result<Repetitions<Measurement>> {
    let repetition_count = schedule.len();
    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        BufWriter::new(
            fs::File::create(&log_path)
                .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        ),
        repetition_count * 2,
        benchmark_name.map(str::to_owned),
    );

    let measurements_path = output_dir.join("measurements.csv");
    let mut measurements = MeasurementsCsv::create(&measurements_path)
        .with_context(|| format!("Failed to create {}.", measurements_path.display()))?;

    for &order in schedule {
        ensure!(!interrupted(), "Interrupted.");
        let [first, second] = order.sides();

        let mut measure =
            |side: Side| -> Result<Measurement> { measure_one(&mut context, &mut log, side) };
        let first_output = measure(first)?;
        let second_output = measure(second)?;

        let outputs = Pair::from_execution_order([first_output, second_output], order);
        let repetition = Repetition { outputs, order };
        measurements
            .append(&repetition)
            .with_context(|| format!("Failed to write {}.", measurements_path.display()))?;
        measured_repetitions.push(repetition);
    }

    drop(log);

    Repetitions::try_from(measured_repetitions)
}

fn measure_one<W: std::io::Write>(
    context: &mut MeasurementContext<'_>,
    log: &mut BenchmarkLog<W>,
    side: Side,
) -> Result<Measurement> {
    log.phase(side, "benchmark startup");
    let benchmark_start = run_in(
        context.session,
        context
            .benchmark_lifecycle
            .startup
            .as_ref()
            .map(|commands| commands.get(side)),
        &context.interrupts.work,
        side,
        "benchmark startup-each-run",
    );
    let measured = benchmark_start.and_then(|()| {
        ensure!(!interrupted(), "Interrupted.");
        let output = log.measure(
            context.session,
            context.benchmark.get(side),
            &context.interrupts.work,
            context.timeout,
            side,
        )?;
        ensure!(!interrupted(), "Interrupted.");
        Ok(output)
    });
    log.phase(side, "benchmark teardown");
    combine(
        measured,
        run_in(
            context.session,
            context
                .benchmark_lifecycle
                .teardown
                .as_ref()
                .map(|commands| commands.get(side)),
            &context.interrupts.cleanup,
            side,
            "benchmark teardown-each-run",
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

fn bind_command(
    parts: &[OsString],
    working_directory: Option<PathBuf>,
    envs: &[(String, String)],
    worktrees: &Pair<Worktree>,
) -> Option<Pair<CommandSpec>> {
    let (program, arguments) = parts.split_first()?;
    let environment = envs
        .iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect::<Vec<_>>();
    let at = |root: &Path| {
        working_directory
            .as_ref()
            .map_or_else(|| root.to_owned(), |path| root.join(path))
    };
    Some(Pair {
        baseline: CommandSpec::new(
            program.clone(),
            arguments.to_vec(),
            at(worktrees.baseline.path()),
            environment.clone(),
        ),
        candidate: CommandSpec::new(
            program.clone(),
            arguments.to_vec(),
            at(worktrees.candidate.path()),
            environment,
        ),
    })
}

fn suite_lifecycle_config(lifecycle: &SuiteLifecycle) -> SuiteLifecycleConfig<'_> {
    SuiteLifecycleConfig {
        startup: &lifecycle.startup,
        teardown: &lifecycle.teardown,
    }
}

fn worktree_lifecycle_config(lifecycle: &WorktreeLifecycle) -> WorktreeLifecycleConfig<'_> {
    WorktreeLifecycleConfig {
        startup: &lifecycle.startup,
        teardown: &lifecycle.teardown,
    }
}

fn benchmark_lifecycle_config(lifecycle: &BenchmarkLifecycle) -> BenchmarkLifecycleConfig<'_> {
    BenchmarkLifecycleConfig {
        startup: &lifecycle.startup,
        startup_each_run: &lifecycle.startup_each_run,
        teardown_each_run: &lifecycle.teardown_each_run,
        teardown: &lifecycle.teardown,
    }
}

fn run_in(
    session: &mut Session,
    command: Option<&CommandSpec>,
    interrupt: &Interrupt,
    side: Side,
    phase: &str,
) -> Result<()> {
    command.map_or(Ok(()), |command| {
        run_unmeasured(session, command, interrupt)
            .with_context(|| format!("The {side} {phase} failed."))
    })
}

fn run_at(
    session: &mut Session,
    command: Option<&CommandTemplate>,
    directory: &Path,
    interrupt: &Interrupt,
    label: &str,
) -> Result<()> {
    command.map_or(Ok(()), |command| {
        run_unmeasured(session, &command.at(directory), interrupt)
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
    session: &mut Session,
    startup: impl FnOnce(&mut Session) -> Result<()>,
    body: impl FnOnce(&mut Session) -> Result<T>,
    teardown: impl FnOnce(&mut Session) -> Result<()>,
) -> Result<T> {
    let primary = startup(session).and_then(|()| body(session));
    combine(primary, teardown(session))
}

/// Runs a lifecycle command on both sides with fail-fast semantics: if the
/// baseline side fails, the candidate side is not run.
fn run_startup_both(
    session: &mut Session,
    commands: Option<&Pair<CommandSpec>>,
    interrupt: &Interrupt,
    phase: &str,
) -> Result<()> {
    let Some(commands) = commands else {
        return Ok(());
    };
    for side in [Side::Baseline, Side::Candidate] {
        run_unmeasured(session, commands.get(side), interrupt)
            .with_context(|| format!("The {side} {phase} failed."))?;
    }
    Ok(())
}

/// Runs a lifecycle command on both sides with best-effort semantics: both
/// sides run regardless of failures; the first error is returned, subsequent
/// errors are printed to stderr.
fn run_teardown_both(
    session: &mut Session,
    commands: Option<&Pair<CommandSpec>>,
    interrupt: &Interrupt,
    phase: &str,
) -> Result<()> {
    let Some(commands) = commands else {
        return Ok(());
    };

    let mut first_error = None;
    for side in [Side::Baseline, Side::Candidate] {
        if let Err(error) = run_unmeasured(session, commands.get(side), interrupt)
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
    use std::{env, fs, num::NonZeroUsize};
    use tempfile::tempdir;

    const CLEANUP_MARKER: &str = "FOIL_CLEANUP_MARKER";

    fn helper(test: &str, env: Vec<(OsString, OsString)>) -> Result<CommandSpec> {
        Ok(CommandSpec::new(
            env::current_exe()?.into_os_string(),
            ["--exact", test, "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            env::current_dir()?,
            env,
        ))
    }

    #[test]
    fn the_second_interrupt_exits_with_status_130() -> Result<()> {
        let status = process::Command::new(std::env::current_exe()?)
            .args(["--exact", "app::tests::second_interrupt_child", "--ignored"])
            .status()?;
        assert_eq!(status.code(), Some(130));
        Ok(())
    }

    #[test]
    fn cancellation_outside_a_wait_does_not_poison_cleanup() -> Result<()> {
        let status = process::Command::new(env::current_exe()?)
            .args([
                "--exact",
                "app::tests::analysis_interrupt_cleanup_child",
                "--ignored",
            ])
            .status()?;
        assert!(
            status.success(),
            "cleanup regression child failed: {status}"
        );
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

    #[test]
    #[ignore]
    fn analysis_interrupt_cleanup_child() -> Result<()> {
        INTERRUPTS.store(0, Ordering::Relaxed);
        let interrupts = Interrupts::new()?;
        let mut session = Session::new()?;
        let directory = tempdir()?;
        let marker = directory.path().join("cleanup-ran");
        let cleanup = helper(
            "app::tests::cleanup_marker_child",
            vec![(CLEANUP_MARKER.into(), marker.as_os_str().to_owned())],
        )?;
        let repetitions = (0..10)
            .map(|position| Repetition {
                outputs: Pair {
                    baseline: Measurement {
                        elapsed: Duration::from_secs(1),
                        peak_memory: None,
                    },
                    candidate: Measurement {
                        elapsed: Duration::from_secs(2),
                        peak_memory: None,
                    },
                },
                order: if position % 2 == 0 {
                    RunOrder::BaselineFirst
                } else {
                    RunOrder::CandidateFirst
                },
            })
            .collect::<Vec<_>>()
            .try_into()?;
        let mut signaled = false;

        let result = scoped(
            &mut session,
            |_| Ok(()),
            |_| {
                analyze_checked(
                    &repetitions,
                    0,
                    NonZeroUsize::new(1_000).unwrap(),
                    crate::Shrinkage::NONE,
                    &[crate::Interval::new(0.5)?],
                    || {
                        if !signaled {
                            signaled = true;
                            handle_interrupt(&interrupts.work);
                        }
                        ensure!(!interrupted(), "Interrupted.");
                        Ok(())
                    },
                )
                .map(drop)
            },
            |session| run_unmeasured(session, &cleanup, &interrupts.cleanup),
        );

        assert_eq!(result.unwrap_err().to_string(), "Interrupted.");
        assert!(interrupted());
        assert!(marker.exists(), "cleanup did not execute");
        session.shutdown()?;
        Ok(())
    }

    #[test]
    #[ignore]
    fn cleanup_marker_child() -> Result<()> {
        fs::write(
            env::var_os(CLEANUP_MARKER).expect("cleanup marker is configured"),
            "ran",
        )?;
        Ok(())
    }
}
