mod worktree;

use anyhow::{Context, Result};
use clap::Parser;
use tempfile::tempdir;

use worktree::Worktree;

#[derive(Parser)]
#[command(name = "b3")]
#[command(version = "0.1.0")]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    #[arg(short, long, default_value = "main")]
    baseline: String,
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!("Base git ref: {:?}", cli.baseline);
    println!("Candidate git ref: {:?}", cli.candidate);

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    let baseline = Worktree::create(worktree_dir.path().join("baseline"), &cli.baseline)?;
    let candidate = Worktree::create(worktree_dir.path().join("candidate"), &cli.candidate)?;

    println!("Baseline:  {}", baseline.path().display());
    println!("Candidate: {}", candidate.path().display());

    Ok(())
}
