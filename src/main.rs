mod run;
mod worktree;

use anyhow::{Context, Result};
use clap::Parser;
use std::ffi::OsString;
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

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `b3 -- Rscript benchmark.R`.
    #[arg(long, required = true)]
    command: Vec<OsString>,

    /// Control shrinkage of mean log ratios towards 0 by specifying a (prior) number of no-change pseudo-observations.
    #[arg(long, default_value_t = 0)]
    shrinkage: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (program, args) = cli.command.split_first().context("No program provided.")?;
    let benchmark = RunCommand::new(program.clone(), args.to_vec());

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    let baseline = Worktree::create(worktree_dir.path().join("baseline"), &cli.baseline)?;
    let candidate = Worktree::create(worktree_dir.path().join("candidate"), &cli.candidate)?;

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
