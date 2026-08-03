use b3::{
    posterior::RunOrder, posterior::bootstrap_paired_means, posterior::report_posterior,
    run::RunCommand, worktree::Worktree,
};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use rand::{RngExt, SeedableRng, rngs::StdRng, seq::SliceRandom};
use std::ffi::OsString;
use std::fs::{File, create_dir};
use std::io::{BufWriter, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[derive(Parser)]
#[command(name = "b3")]
#[command(version = "0.1.0")]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    // TODO: reorder args into cogent order.
    /// Git revision used as the baseline.
    #[arg(short, long, default_value = "main")]
    baseline: String,

    /// Git revision containing the candidate changes.
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,

    /// Control shrinkage of the adjusted mean runtime difference toward 0 by specifying a prior number of no-change pseudo-observations.
    #[arg(long, default_value_t = 0.0)]
    shrinkage: f64,

    /// Skip repetition pairs where either benchmark exits unsuccessfully.
    #[arg(long)]
    skip_failing: bool,

    /// Directory where generated output files are written.
    #[arg(long, value_name = "DIR", required = true)]
    output_dir: PathBuf,

    /// Number of benchmark runs per branch.
    ///
    /// Each repetition runs both branches, for `repetitions * 2` total runs.
    #[arg(short, long, required = true)]
    repetitions: NonZeroUsize,

    /// Number of Bayesian bootstrap draws.
    #[arg(long, default_value = "10000")]
    draws: NonZeroUsize,

    /// Central credible interval widths.
    #[arg(long = "interval", default_values = ["0.5", "0.8", "0.98"])]
    intervals: Vec<f64>,

    /// Set a seed for reproducible benchmarking.
    #[arg(long)]
    seed: Option<u64>,

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `b3 -- Rscript benchmark.R`.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<OsString>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // TODO: move this to type somehow?
    ensure!(
        cli.intervals
            .iter()
            .all(|&width| 0.0 < width && width < 1.0),
        "Intervals must be between 0 and 1."
    );

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    create_dir(&cli.output_dir)
        .with_context(|| format!("Failed to create output directory {:?}", cli.output_dir))?;
    let seed = cli.seed.unwrap_or_else(rand::random);
    let mut rng = StdRng::seed_from_u64(seed);
    eprintln!("Seed: {seed}");

    // Setting up the benchmark call
    let (program, args) = cli.command.split_first().context("No program provided.")?;
    let benchmark = RunCommand::new(program.clone(), args.to_vec());

    // Worktree setup
    let baseline = Worktree::create(worktree_dir.path().join("baseline"), &cli.baseline)?;
    let candidate = Worktree::create(worktree_dir.path().join("candidate"), &cli.candidate)?;

    // Allocating vectors for the runs
    let repetitions = cli.repetitions.get();
    let mut baseline_times = Vec::with_capacity(repetitions);
    let mut candidate_times = Vec::with_capacity(repetitions);
    let mut orders = Vec::with_capacity(repetitions);
    let mut run_index = Vec::with_capacity(repetitions);

    // Random order for baseline/candidate
    let mut baseline_firsts = [true, false].repeat(repetitions / 2);
    if repetitions % 2 == 1 {
        baseline_firsts.push(rng.random());
    }
    baseline_firsts.shuffle(&mut rng);

    for (pair_index, baseline_first) in baseline_firsts.into_iter().enumerate() {
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

        // TODO: better handling of failing runs to find systematic errors. Should record and write out?
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
        orders.push(if baseline_first {
            RunOrder::BaselineFirst
        } else {
            RunOrder::CandidateFirst
        });
        run_index.push(pair_index as f64);
    }

    ensure!(!baseline_times.is_empty(), "No successful benchmark pairs.");

    let posterior = bootstrap_paired_means(
        &baseline_times,
        &candidate_times,
        &orders,
        &run_index,
        cli.draws.get(),
        cli.shrinkage,
        &mut rng,
    )?;

    let posterior_path = cli.output_dir.join("posterior.csv");
    write_posterior_csv(&posterior_path, &posterior)
        .with_context(|| format!("Failed to write {}", posterior_path.display()))?;

    let report = report_posterior(&posterior, &cli.intervals);
    print!("{report}");
    let report_path = cli.output_dir.join("report.txt");
    std::fs::write(&report_path, report)
        .with_context(|| format!("Failed to write {}", report_path.display()))?;

    Ok(())
}

// NOTE: could swap to CSV crate if this gets annoying
fn write_posterior_csv(path: &Path, posterior: &[(f64, f64)]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "baseline,candidate")?;

    for &(baseline, candidate) in posterior {
        writeln!(writer, "{baseline},{candidate}")?;
    }

    writer.flush()?;
    Ok(())
}
