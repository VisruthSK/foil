mod config;

use crate::config::{Cli, ResolvedSuiteConfig, RunConfig, Suite};
use b3::{
    BenchmarkLog, Config, MeasurementsCsv, Pair, Posterior, Repetition, Repetitions, Revision,
    RunCommand, RunOrder, RunOutput, Side, Summary, Time, Worktree,
    working_tree_has_modified_tracked_files, write_config_json, write_posterior_csv,
};

use anyhow::{Context, Result, ensure};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{
    ffi::OsString,
    fs,
    path::Path,
    process,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tempfile::{TempDir, tempdir};

struct Worktrees {
    pair: Pair<Worktree>,
    _directory: TempDir,
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

fn main() -> Result<()> {
    ctrlc::set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::Relaxed) {
            process::exit(130);
        }
        eprintln!("\nInterrupted; cleaning up. Press Ctrl+C again to exit immediately.");
    })
    .context("Failed to set the Ctrl+C handler.")?;

    let Suite {
        config: suite,
        output_dir: suite_output_dir,
        runs,
    } = Cli::suite()?;

    if working_tree_has_modified_tracked_files() {
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
        let output_dir = match &name {
            Some(name) => config.output_dir.join(name),
            _ => config.output_dir.clone(),
        };
        let heading = if multiple { name.as_deref() } else { None };
        let isolated = config
            .isolate
            .then(|| create_worktrees(&revisions))
            .transpose()?;
        let worktrees = isolated.as_ref().unwrap_or(&shared);

        let summary = compare(&suite, config, &worktrees.pair, &output_dir, heading)?;

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

fn compare(
    suite: &ResolvedSuiteConfig,
    config: RunConfig,
    worktrees: &Pair<Worktree>,
    output_dir: &Path,
    heading: Option<&str>,
) -> Result<Summary<Time>> {
    let RunConfig {
        isolate: _,
        shrinkage,
        output_dir: _,
        repetitions: repetition_count,
        draws,
        timeout,
        intervals,
        working_directory,
        envs,
        setup,
        prepare,
        teardown,
        command,
    } = config;

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create output directory {}.",
            output_dir.display()
        )
    })?;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(suite.seed);

    let run_command = |command: &[OsString]| -> Option<RunCommand> {
        let (program, arguments) = command.split_first()?;

        Some(RunCommand::new(
            program.clone(),
            arguments.to_vec(),
            working_directory.clone(),
            envs.clone(),
        ))
    };
    let benchmark = run_command(&command)
        .expect("Clap requires at least one command argument.")
        .with_timeout(timeout.map(|seconds| Duration::from_secs(seconds.get())));

    let repetition_count = repetition_count.get();

    let config_path = output_dir.join("config.json");
    write_config_json(
        &config_path,
        &Config {
            seed: suite.seed,
            repetitions: repetition_count,
            draws: draws.get(),
            timeout_seconds: timeout.map(|seconds| seconds.get()),
            shrinkage,
            baseline: worktrees.baseline.revision(),
            candidate: worktrees.candidate.revision(),
            setup: &setup,
            prepare: &prepare,
            command: &command,
            teardown: &teardown,
        },
    )
    .with_context(|| format!("Failed to write {}.", config_path.display()))?;

    run_in_both(run_command(&setup), worktrees, "setup")?;
    ensure!(!interrupted(), "Interrupted.");

    let measured = measure_all(
        &benchmark,
        run_command(&prepare).as_ref(),
        worktrees,
        repetition_count,
        output_dir,
        &mut rng,
    );
    let torn_down = run_in_both(run_command(&teardown), worktrees, "teardown");
    if let (Err(_), Err(error)) = (&measured, &torn_down) {
        eprintln!("{error:#}");
    }
    let repetitions = measured?;
    torn_down?;

    let posterior =
        Posterior::<Time>::bootstrap_checked(&repetitions, draws, shrinkage, &mut rng, || {
            ensure!(!interrupted(), "Interrupted.");
            Ok(())
        })?;
    ensure!(!interrupted(), "Interrupted.");

    let posterior_path = output_dir.join("posterior.csv");
    write_posterior_csv(&posterior_path, &posterior)
        .with_context(|| format!("Failed to write {}.", posterior_path.display()))?;

    let summary = posterior.summarize(&intervals)?;

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

fn measure_all(
    benchmark: &RunCommand,
    prepare: Option<&RunCommand>,
    worktrees: &Pair<Worktree>,
    repetition_count: usize,
    output_dir: &Path,
    rng: &mut Xoshiro256PlusPlus,
) -> Result<Repetitions> {
    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        fs::File::create(&log_path)
            .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        repetition_count * 2,
    );

    let measurements_path = output_dir.join("measurements.csv");
    let mut measurements = MeasurementsCsv::create(&measurements_path)
        .with_context(|| format!("Failed to create {}.", measurements_path.display()))?;

    for order in RunOrder::schedule(repetition_count, rng) {
        ensure!(!interrupted(), "Interrupted.");
        let [first, second] = order.sides();

        // TODO: Better handling of failing runs to find systematic errors. Should record and write out?
        let mut measure = |side: Side| -> Result<RunOutput> {
            if let Some(prepare) = prepare {
                prepare
                    .run_once_in(worktrees.get(side))
                    .with_context(|| format!("The {side} preparation failed."))?;
                ensure!(!interrupted(), "Interrupted.");
            }
            let output = log.measure(benchmark, side, worktrees.get(side))?;
            ensure!(!interrupted(), "Interrupted.");
            ensure!(
                output.exit_status().success(),
                "The {side} benchmark failed with {}.",
                output.exit_status()
            );

            Ok(output)
        };
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

fn run_in_both(command: Option<RunCommand>, worktrees: &Pair<Worktree>, phase: &str) -> Result<()> {
    let Some(command) = command else {
        return Ok(());
    };

    let mut first_error = None;
    for side in [Side::Baseline, Side::Candidate] {
        if let Err(error) = command
            .run_once_in(worktrees.get(side))
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
