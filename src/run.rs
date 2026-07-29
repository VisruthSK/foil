use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, ExitStatus},
};

pub struct RunCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl RunCommand {
    pub fn new(program: OsString, args: Vec<OsString>) -> Self {
        Self { program, args }
    }

    pub fn run_in(&self, working_dir: &Path) -> Result<ExitStatus> {
        Command::new(&self.program)
            .args(&self.args)
            .current_dir(working_dir)
            .status()
            .with_context(|| format!("Failed to run {:?}.", self.program))
    }
}
