use anyhow::Result;
use b3::{Config, LifecycleConfig, Revision, Shrinkage, write_config_json};
use serde_json::json;
use std::{
    env::{current_dir, set_current_dir},
    ffi::OsString,
    fs::read_to_string,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::tempdir;

struct RestoreCwd(PathBuf);

impl Drop for RestoreCwd {
    fn drop(&mut self) {
        let _ = set_current_dir(&self.0);
    }
}

fn init_repo(path: &Path) -> Result<()> {
    let git = |args: &[&str]| -> Result<()> {
        anyhow::ensure!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()?
                .success(),
            "git {args:?} failed."
        );
        Ok(())
    };

    git(&["init", "--quiet", "--initial-branch=main"])?;
    git(&["config", "user.email", "b3@example.com"])?;
    git(&["config", "user.name", "b3"])?;
    git(&["commit", "--quiet", "--allow-empty", "-m", "root"])
}

#[test]
fn config_json_contains_reproduction_metadata() -> Result<()> {
    let repo = tempdir()?;
    init_repo(repo.path())?;

    let _restore = RestoreCwd(current_dir()?);
    set_current_dir(&repo)?;

    let directory = tempdir()?;
    let path = directory.path().join("config.json");
    let command = ["cargo", "test --workspace"].map(OsString::from);
    let startup = ["cargo", "fetch --locked"].map(OsString::from);
    let startup_each_run = ["cargo", "clean --release"].map(OsString::from);
    let teardown_each_run = ["git", "status --short"].map(OsString::from);
    let teardown = ["git", "clean --force"].map(OsString::from);
    let baseline = Revision::resolve("main".to_owned())?;
    let candidate = Revision::resolve("HEAD".to_owned())?;
    let config = Config {
        seed: 0,
        repetitions: 10,
        block_size: 4,
        draws: 1000,
        timeout_seconds: Some(30),
        shrinkage: Shrinkage::new(5.0)?,
        baseline: &baseline,
        candidate: &candidate,
        suite_lifecycle: LifecycleConfig {
            startup: &startup,
            startup_each_run: &startup_each_run,
            teardown_each_run: &teardown_each_run,
            teardown: &teardown,
        },
        benchmark_lifecycle: LifecycleConfig {
            startup: &startup,
            startup_each_run: &startup_each_run,
            teardown_each_run: &teardown_each_run,
            teardown: &teardown,
        },
        command: &command,
    };

    write_config_json(&path, &config)?;

    let actual: serde_json::Value = serde_json::from_str(&read_to_string(path)?)?;
    let expected = json!({
        "seed": 0,
        "repetitions": 10,
        "block_size": 4,
        "draws": 1000,
        "timeout_seconds": 30,
        "shrinkage": 5.0,
        "b3_version": env!("CARGO_PKG_VERSION"),
        "baseline": { "revision": "main", "hash": baseline.hash() },
        "candidate": { "revision": "HEAD", "hash": candidate.hash() },
        "suite_lifecycle": {
            "startup": ["cargo", "fetch --locked"],
            "startup_each_run": ["cargo", "clean --release"],
            "teardown_each_run": ["git", "status --short"],
            "teardown": ["git", "clean --force"],
        },
        "benchmark_lifecycle": {
            "startup": ["cargo", "fetch --locked"],
            "startup_each_run": ["cargo", "clean --release"],
            "teardown_each_run": ["git", "status --short"],
            "teardown": ["git", "clean --force"],
        },
        "command": ["cargo", "test --workspace"],
    });

    assert_eq!(actual, expected);

    Ok(())
}
