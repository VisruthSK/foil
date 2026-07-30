mod bootstrap;
mod run;
mod worktree;

use bootstrap::bootstrap_mean_log_ratios;
use run::RunCommand;
use worktree::Worktree;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use rand::seq::SliceRandom;
use std::ffi::OsString;
use std::num::NonZeroUsize;
use tempfile::tempdir;

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

    let mut rng = rand::rng();
    let mut orders: Vec<bool> = (0..repetitions).map(|i| i % 2 == 0).collect();
    orders.shuffle(&mut rng);

    for baseline_first in orders {
        let (first, second) = if baseline_first {
            (&baseline, &candidate)
        } else {
            (&candidate, &baseline)
        };

        let first = benchmark.run_in(first.path())?;
        let second = benchmark.run_in(second.path())?;

        let (baseline_run, candidate_run) = if baseline_first {
            (first, second)
        } else {
            (second, first)
        };

        if let Some((name, status)) = [
            ("Baseline", &baseline_run.0),
            ("Candidate", &candidate_run.0),
        ]
        .into_iter()
        .find(|(_, status)| !status.success())
        {
            if cli.skip_failing {
                continue;
            }

            bail!("{name} benchmark failed with {status}.");
        }

        baseline_times.push(baseline_run.1.as_secs_f64());
        candidate_times.push(candidate_run.1.as_secs_f64());
    }

    ensure!(!baseline_times.is_empty(), "No successful benchmark pairs.");

    let mut posterior = bootstrap_mean_log_ratios(
        &baseline_times,
        &candidate_times,
        // TODO: surface this as optional argument, defaults to 10k? 20k?
        10_000,
        cli.shrinkage,
        &mut rng,
    )?;

    posterior.sort_by(f64::total_cmp);

    let quantile = |p: f64| posterior[((posterior.len() - 1) as f64 * p).round() as usize];

    println!(
        "Mean log ratio: {:.4} [{:.4}, {:.4}]",
        quantile(0.5),
        // TODO: surface as optional argument (vector of interval widths: 0-1)
        quantile(0.05),
        quantile(0.95),
    );

    Ok(())
}
