use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::{path::Path, process::Command};
use tempfile::tempdir;

#[derive(Parser)]
#[command(name = "b3")]
#[command(version = "0.1.0")]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    #[arg(long)]
    baseline: String,
    #[arg(long)]
    candidate: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!("Base git ref: {:?}", cli.baseline);
    println!("Candidate git ref: {:?}", cli.candidate);

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    let baseline_path = worktree_dir.path().join("baseline");
    let candidate_path = worktree_dir.path().join("candidate");

    create_worktree(&baseline_path, &cli.baseline)?;
    create_worktree(&candidate_path, &cli.candidate)?;

    println!("Baseline:  {}", baseline_path.display());
    println!("Candidate: {}", candidate_path.display());

    remove_worktree(&baseline_path)?;
    remove_worktree(&candidate_path)?;

    Ok(())
}

fn create_worktree(path: &Path, revision: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(path)
        .arg(revision)
        .status()
        .context("Failed to run git.")?;

    anyhow::ensure!(
        status.success(),
        "git worktree add failed for {revision} with {status}."
    );

    Ok(())
}

fn remove_worktree(path: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .status()
        .context("failed to run git")?;

    anyhow::ensure!(
        status.success(),
        "git worktree remove failed for {} with {status}.",
        path.display()
    );

    Ok(())
}
