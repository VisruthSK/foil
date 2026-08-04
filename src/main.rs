use b3::{
    BenchmarkLog, Interval, Pair, Posterior, Repetition, Repetitions, RunCommand, RunOrder,
    Shrinkage, Side, Time, Worktree, write_posterior_csv,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{ffi::OsString, fs, num::NonZeroUsize, path::PathBuf};
use tempfile::tempdir;

const MIN_DRAWS: usize = 1_000;

#[derive(Parser)]
#[command(name = "b3")]
#[command(version)]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    // TODO: Reorder args into cogent order.
    /// Git revision used as the baseline.
    #[arg(short, long, default_value = "main")]
    baseline: String,

    /// Git revision containing the candidate changes.
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,

    /// Control shrinkage of the adjusted mean runtime difference toward 0 by specifying a prior number of no-change pseudo-observations.
    #[arg(long, default_value = "0")]
    shrinkage: Shrinkage,

    /// Directory where generated output files are written.
    #[arg(long, value_name = "DIR", required = true)]
    output_dir: PathBuf,

    /// Number of benchmark runs per branch.
    ///
    /// Each repetition runs both branches, for `repetitions * 2` total runs.
    #[arg(short, long, required = true, value_parser = parse_repetitions)]
    repetitions: NonZeroUsize,

    /// Number of Bayesian bootstrap draws.
    #[arg(long, value_parser = parse_draws, default_value = "10000")]
    draws: NonZeroUsize,

    /// Central credible interval widths.
    #[arg(long = "interval", default_values = ["0.5", "0.8", "0.98"])]
    intervals: Vec<Interval>,

    /// Set a seed for reproducible benchmarking.
    #[arg(long)]
    seed: Option<u64>,

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `b3 -- Rscript benchmark.R`.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<OsString>,
}

fn parse_repetitions(text: &str) -> Result<NonZeroUsize> {
    let repetitions: NonZeroUsize = text
        .parse()
        .with_context(|| format!("`{text}` is not a positive integer."))?;

    ensure!(
        repetitions.get() >= Repetitions::MINIMUM,
        "At least {} repetitions are required.",
        Repetitions::MINIMUM
    );

    Ok(repetitions)
}

fn parse_draws(text: &str) -> Result<NonZeroUsize> {
    let draws: NonZeroUsize = text
        .parse()
        .with_context(|| format!("`{text}` is not a positive integer."))?;

    ensure!(
        draws.get() >= MIN_DRAWS,
        "At least {MIN_DRAWS} draws are required."
    );

    Ok(draws)
}

fn main() -> Result<()> {
    let Cli {
        baseline,
        candidate,
        shrinkage,
        output_dir,
        repetitions: repetition_count,
        draws,
        intervals,
        seed,
        command,
    } = Cli::parse();

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "Failed to create output directory {}.",
            output_dir.display()
        )
    })?;
    let seed = seed.unwrap_or_else(rand::random);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    eprintln!("Seed: {seed}");

    let mut command = command.into_iter();
    let program = command
        .next()
        .expect("Clap requires at least one command argument.");
    let benchmark = RunCommand::new(program, command.collect());

    let worktrees = Pair {
        baseline: Worktree::create(worktree_dir.path().join("baseline"), baseline)?,
        candidate: Worktree::create(worktree_dir.path().join("candidate"), candidate)?,
    };

    let repetition_count = repetition_count.get();
    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        fs::File::create(&log_path)
            .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        repetition_count * 2,
    );

    for order in RunOrder::schedule(repetition_count, &mut rng) {
        let [first, second] = order.sides();

        let outputs = Pair::from_execution_order(
            [
                log.measure(&benchmark, first, worktrees.get(first))?,
                log.measure(&benchmark, second, worktrees.get(second))?,
            ],
            order,
        );

        // TODO: Better handling of failing runs to find systematic errors. Should record and write out?
        for side in [Side::Baseline, Side::Candidate] {
            let status = outputs.get(side).exit_status();

            ensure!(
                status.success(),
                "The {side} benchmark failed with {status}."
            );
        }

        measured_repetitions.push(Repetition { outputs, order });
    }

    // Ends the progress line before the report starts printing.
    drop(log);

    let repetitions = Repetitions::try_from(measured_repetitions)?;

    // TODO: Add memory report. Needs one output path per metric.
    let posterior = Posterior::<Time>::bootstrap(&repetitions, draws, shrinkage, &mut rng)?;

    let posterior_path = output_dir.join("posterior.csv");
    write_posterior_csv(&posterior_path, &posterior)
        .with_context(|| format!("Failed to write {}.", posterior_path.display()))?;

    let report = posterior.summarize(&intervals)?.to_string();
    print!("{report}");

    let report_path = output_dir.join("report.txt");
    fs::write(&report_path, &report)
        .with_context(|| format!("Failed to write {}.", report_path.display()))?;

    Ok(())
}
