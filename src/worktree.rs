use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub struct Worktree {
    path: PathBuf,
    revision: String,
}

impl Worktree {
    pub fn create(path: PathBuf, revision: String) -> Result<Self> {
        let status = Command::new("git")
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(&path)
            .arg(&revision)
            .status()
            .context("Failed to run git.")?;

        anyhow::ensure!(
            status.success(),
            "Git worktree add failed for {revision} with {status}."
        );

        Ok(Self { path, revision })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn revision(&self) -> &str {
        &self.revision
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
