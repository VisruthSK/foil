mod run;
mod worktree;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use std::ffi::OsString;
use std::num::NonZeroUsize;
use tempfile::tempdir;

use run::RunCommand;
use worktree::Worktree;

#[derive(Parser)]
#[command(name = "b3")]
#[command(version = "0.1.0")]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    /// Git revision used as the baseline.
    #[arg(short, long, default_value = "main")]
    baseline: String,

    /// Git revision containing the candidate changes.
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,

    /// Control shrinkage of mean log ratios towards 0 by specifying a (prior) number of no-change pseudo-observations.
    #[arg(long, default_value_t = 0)]
    shrinkage: usize,

    /// Skip repetition pairs where either benchmark exits unsuccessfully.
    #[arg(long)]
    skip_failing: bool,

    /// Number of benchmark runs per branch.
    ///
    /// Each repetition runs both branches, for `repetitions * 2` total runs.
    #[arg(short, long, required = true)]
    repetitions: NonZeroUsize,

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `b3 -- Rscript benchmark.R`.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<OsString>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (program, args) = cli.command.split_first().context("No program provided.")?;
    let benchmark = RunCommand::new(program.clone(), args.to_vec());

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    let baseline = Worktree::create(worktree_dir.path().join("baseline"), &cli.baseline)?;
    let candidate = Worktree::create(worktree_dir.path().join("candidate"), &cli.candidate)?;

    let repetitions = cli.repetitions.get();
    let mut baseline_times = Vec::with_capacity(repetitions);
    let mut candidate_times = Vec::with_capacity(repetitions);

    for _ in 0..repetitions {
        let (status, baseline_duration) = benchmark.run_in(baseline.path())?;
        if !status.success() {
            if cli.skip_failing {
                continue;
            }
            bail!("Baseline benchmark failed with {status}.");
        }

        let (status, candidate_duration) = benchmark.run_in(candidate.path())?;
        if !status.success() {
            if cli.skip_failing {
                continue;
            }
            bail!("Candidate benchmark failed with {status}.");
        }

        baseline_times.push(baseline_duration.as_secs_f64());
        candidate_times.push(candidate_duration.as_secs_f64());
    }

    ensure!(!baseline_times.is_empty(), "No successful benchmark pairs.");

    let (baseline_status, baseline_duration) = benchmark.run_in(baseline.path())?;
    let (candidate_status, candidate_duration) = benchmark.run_in(candidate.path())?;

    println!(
        "Baseline: {baseline_status}, {:.3} s",
        baseline_duration.as_secs_f64()
    );
    println!(
        "Candidate: {candidate_status}, {:.3} s",
        candidate_duration.as_secs_f64()
    );

    Ok(())
}
