#![allow(dead_code)]

use anyhow::{Result, ensure};
use std::{fs, process::Command};
use tempfile::{TempDir, tempdir};

pub const REQUIRED: &str =
    "output-dir = 'bench'\nrepetitions = 12\ninterval = [0.5, 0.8]\ncommand = ['benchmark']\n";

pub const PREAMBLE: &str = "baseline = 'HEAD'\n\
    candidate = 'HEAD'\n\
    output-dir = 'bench'\n\
    repetitions = 10\n\
    draws = 1000\n\
    interval = [0.5, 0.8]\n\
    seed = 0\n";

pub const CONFIG: &str = "\
    baseline = 'baseline-branch'\n\
    candidate = 'candidate-branch'\n\
    shrinkage = 2.5\n\
    output-dir = 'configured'\n\
    repetitions = 12\n\
    draws = 2000\n\
    interval = [0.5, 0.8]\n\
    seed = 0\n\
    command = ['Rscript', 'benchmark.R']\n";

pub const BUILTIN_USAGE: &str = "--output-dir <DIR> --repetitions <REPETITIONS> -- <COMMAND>...";

pub fn project(files: &[(&str, &str)]) -> Result<TempDir> {
    let directory = tempdir()?;

    for (name, contents) in files {
        fs::write(directory.path().join(name), contents)?;
    }

    Ok(directory)
}

pub fn run(project: &TempDir, arguments: &[&str]) -> Result<(bool, String, String)> {
    let output = Command::new(env!("CARGO_BIN_EXE_foil"))
        .args(arguments)
        .current_dir(project.path())
        .output()?;

    Ok((
        output.status.success(),
        String::from_utf8(output.stdout)?,
        String::from_utf8(output.stderr)?,
    ))
}

pub fn help(project: &TempDir, arguments: &[&str]) -> Result<String> {
    let (succeeded, stdout, stderr) = run(project, &[arguments, &["--help"]].concat())?;
    ensure!(succeeded, "foil --help failed with {stderr}");

    Ok(stdout)
}

pub fn failure(project: &TempDir, arguments: &[&str]) -> Result<String> {
    let (succeeded, stdout, stderr) = run(project, arguments)?;
    ensure!(!succeeded, "foil unexpectedly succeeded with {stdout}");

    Ok(stderr)
}

pub fn repository(config: &str) -> Result<TempDir> {
    let project = project(&[("foil.toml", config)])?;

    git(&project, &["init", "--quiet"])?;
    git(
        &project,
        &["commit", "--quiet", "--allow-empty", "--message", "root"],
    )?;

    Ok(project)
}

pub fn git(project: &TempDir, arguments: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=foil",
            "-c",
            "user.email=foil@example.invalid",
        ])
        .args(arguments)
        .current_dir(project.path())
        .status()?;
    ensure!(status.success(), "git {arguments:?} failed with {status}");

    Ok(())
}
