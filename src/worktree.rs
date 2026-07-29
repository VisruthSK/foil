use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub struct Worktree {
    path: PathBuf,
}

impl Worktree {
    pub fn create(path: PathBuf, revision: &str) -> Result<Self> {
        let status = Command::new("git")
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(&path)
            .arg(revision)
            .status()
            .context("Failed to run git.")?;

        anyhow::ensure!(
            status.success(),
            "git worktree add failed for {revision} with {status}."
        );

        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let result = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .status();

        match result {
            Ok(status) if status.success() => {}

            Ok(status) => {
                eprintln!(
                    "Failed to remove worktree {} with {status}.",
                    self.path.display()
                );
            }

            Err(error) => {
                eprintln!(
                    "Failed to run git while removing {}: {error}.",
                    self.path.display()
                );
            }
        }
    }
}
