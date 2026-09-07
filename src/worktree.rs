use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) struct Worktree {
    path: PathBuf,
    revision: Revision,
    removed: bool,
}

#[derive(Clone)]
pub(crate) struct Revision {
    name: String,
    hash: String,
}

impl Revision {
    pub(crate) fn resolve(name: String) -> Result<Self> {
        let commit = format!("{name}^{{commit}}");
        anyhow::ensure!(!name.is_empty(), "Git revision must not be empty.");
        let output = Command::new("git")
            .args(["rev-parse", "--verify"])
            .arg(commit)
            .output()
            .context("Failed to run git.")?;
        anyhow::ensure!(
            output.status.success(),
            "Git could not resolve {name} to a commit: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let hash = String::from_utf8(output.stdout)
            .context("Git returned a non-UTF-8 commit hash.")?
            .trim()
            .to_owned();

        Ok(Self { name, hash })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn hash(&self) -> &str {
        &self.hash
    }
}

pub(crate) fn working_tree_has_modified_tracked_files() -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .context("Failed to inspect the working tree.")?;
    anyhow::ensure!(output.status.success(), "Git status failed.");
    Ok(!output.stdout.is_empty())
}

impl Worktree {
    pub(crate) fn create(path: PathBuf, revision: Revision) -> Result<Self> {
        let status = Command::new("git")
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(&path)
            .arg(revision.hash())
            .status()
            .context("Failed to run git.")?;

        anyhow::ensure!(
            status.success(),
            "Git worktree add failed for {} ({}) with {status}.",
            revision.name(),
            revision.hash()
        );

        Ok(Self {
            path,
            revision,
            removed: false,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn revision(&self) -> &Revision {
        &self.revision
    }

    pub(crate) fn remove(mut self) -> Result<()> {
        remove_worktree(&self.path)?;
        self.removed = true;
        Ok(())
    }
}

fn remove_worktree(path: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .status()
        .context("Failed to run git worktree remove.")?;
    anyhow::ensure!(
        status.success(),
        "Git worktree remove failed for {} with {status}.",
        path.display()
    );
    Ok(())
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        if let Err(error) = remove_worktree(&self.path) {
            eprintln!("{error:#}");
        }
    }
}
