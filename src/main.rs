mod config;

use crate::config::{Cli, ResolvedSuiteConfig, RunConfig, Suite};
use b3::{
    BenchmarkLog, Config, Pair, Posterior, Repetition, Repetitions, Revision, RunCommand, RunOrder,
    Side, Summary, Time, Worktree, write_config_json, write_measurements_csv, write_posterior_csv,
};

use anyhow::{Context, Result, ensure};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{ffi::OsString, fs, path::Path};
use tempfile::{TempDir, tempdir};

struct Worktrees {
    pair: Pair<Worktree>,
    _directory: TempDir,
}

fn main() -> Result<()> {
    let Suite {
        config: suite,
        output_dir: suite_output_dir,
        runs,
    } = Cli::suite()?;
    let multiple = runs.len() > 1;
    let revisions = Pair {
        baseline: suite.baseline.clone(),
        candidate: suite.candidate.clone(),
    };
    let shared = create_worktrees(&revisions)?;

    let mut compact = Vec::with_capacity(runs.len());

    for (name, config) in runs {
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

        if multiple {
            let name = name.expect("Multi-benchmark runs are always named.");
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
        intervals,
        working_directory,
        envs,
        setup,
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
    let benchmark = run_command(&command).expect("Clap requires at least one command argument.");

    let repetition_count = repetition_count.get();

    let config_path = output_dir.join("config.json");
    write_config_json(
        &config_path,
        &Config {
            seed: suite.seed,
            repetitions: repetition_count,
            draws: draws.get(),
            shrinkage,
            baseline: worktrees.baseline.revision(),
            candidate: worktrees.candidate.revision(),
            setup: &setup,
            command: &command,
            teardown: &teardown,
        },
    )
    .with_context(|| format!("Failed to write {}.", config_path.display()))?;

    run_in_both(run_command(&setup), worktrees, "setup")?;

    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        fs::File::create(&log_path)
            .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        repetition_count * 2,
    );

    for order in RunOrder::schedule(repetition_count, &mut rng) {
        let [first, second] = order.sides();

        let first_output = log.measure(&benchmark, first, worktrees.get(first))?;
        ensure!(
            first_output.exit_status().success(),
            "The {first} benchmark failed with {}.",
            first_output.exit_status()
        );

        let second_output = log.measure(&benchmark, second, worktrees.get(second))?;
        ensure!(
            second_output.exit_status().success(),
            "The {second} benchmark failed with {}.",
            second_output.exit_status()
        );

        let outputs = Pair::from_execution_order([first_output, second_output], order);
        measured_repetitions.push(Repetition { outputs, order });
    }

    drop(log);

    run_in_both(run_command(&teardown), worktrees, "teardown")?;

    let repetitions = Repetitions::try_from(measured_repetitions)?;

    let measurements_path = output_dir.join("measurements.csv");
    write_measurements_csv(&measurements_path, &repetitions)
        .with_context(|| format!("Failed to write {}.", measurements_path.display()))?;

    let posterior = Posterior::<Time>::bootstrap(&repetitions, draws, shrinkage, &mut rng)?;

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

fn run_in_both(command: Option<RunCommand>, worktrees: &Pair<Worktree>, phase: &str) -> Result<()> {
    let Some(command) = command else {
        return Ok(());
    };

    for side in [Side::Baseline, Side::Candidate] {
        command
            .run_once_in(worktrees.get(side))
            .with_context(|| format!("The {side} {phase} failed."))?;
    }

    Ok(())
}
