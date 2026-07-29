use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

pub struct RunCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl RunCommand {
    pub fn new(program: OsString, args: Vec<OsString>) -> Self {
        Self { program, args }
    }

    pub fn run_in(&self, working_dir: &Path) -> Result<(ExitStatus, Duration)> {
        let start = Instant::now();

        let status = Command::new(&self.program)
            .args(&self.args)
            .current_dir(working_dir)
            .status()
            .with_context(|| format!("Failed to run {:?}.", self.program))?;

        Ok((status, start.elapsed()))
    }
}
