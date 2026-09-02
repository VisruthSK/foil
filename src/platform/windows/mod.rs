use super::{CommandSpec, Finished, Wait, combine_errors};
use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

mod raw;
use raw::{
    AttributeList, Child, Event, Job, compare_ordinal, null_stdio_handles, spawn_process, wait_for,
};
use std::os::windows::io::OwnedHandle;

#[derive(Clone)]
pub(crate) struct Interrupt(Arc<Event>);

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        Event::new().map(Arc::new).map(Self)
    }

    pub(crate) fn signal(&self) {
        self.0.signal();
    }
}

/// Everything computable before the timed interval begins.
pub(crate) struct Prepared {
    job: Job,
    attribute_list: AttributeList,
    input: OwnedHandle,
    output: OwnedHandle,
    application: Vec<u16>,
    command_line: Vec<u16>,
    environment: Option<Vec<u16>>,
    cwd: Vec<u16>,
}

pub(crate) struct Workload {
    child: Child,
    job: Job,
}

pub(crate) struct Session;

impl Session {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) fn prepare(&mut self, spec: &CommandSpec) -> io::Result<Prepared> {
        reject_embedded_nuls(spec)?;

        // Preparation follows startup, which may create the executable.
        let application = wide(resolve_application(spec)?.as_os_str());
        let command_line = command_line(spec);
        let environment = environment_block(&spec.env);
        let cwd = wide(spec.cwd.as_os_str());

        let job = Job::new()?;
        let (input, output) = null_stdio_handles()?;
        let attribute_list = AttributeList::new()?
            .with_job(&job)?
            .with_inherited_handles(&[&input, &output])?;
        Ok(Prepared {
            job,
            attribute_list,
            input,
            output,
            application,
            command_line,
            environment,
            cwd,
        })
    }

    pub(crate) fn shutdown(self) -> io::Result<()> {
        Ok(())
    }
}

impl Workload {
    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        wait_for(&self.child, &interrupt.0, timeout)
    }

    pub(crate) fn finish(self) -> Finished {
        let terminated = self.job.terminate();
        let fallback = terminated.as_ref().err().map(|_| self.child.terminate());
        let status = if terminated.is_ok() || fallback.as_ref().is_some_and(Result::is_ok) {
            self.child.wait()
        } else {
            self.child.try_wait().and_then(|status| {
                status.ok_or_else(|| {
                    io::Error::other("workload could not be terminated safely; refusing to wait")
                })
            })
        };
        let cleanup = match fallback {
            Some(fallback) => combine_errors(terminated, fallback),
            None => terminated,
        };
        Finished {
            status,
            peak_memory: None,
            cleanup,
        }
    }
}

impl Prepared {
    pub(crate) fn spawn(mut self) -> io::Result<Workload> {
        let child = spawn_process(
            &self.application,
            &mut self.command_line,
            self.environment.as_deref_mut(),
            &self.cwd,
            &self.attribute_list,
            (&self.input, &self.output),
        )?;
        Ok(Workload {
            child,
            job: self.job,
        })
    }
}

fn reject_embedded_nuls(spec: &CommandSpec) -> io::Result<()> {
    fn reject(value: &OsStr, what: &str) -> io::Result<()> {
        if value.encode_wide().any(|unit| unit == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{what} contains an embedded NUL"),
            ));
        }
        Ok(())
    }

    reject(&spec.program, "program")?;
    for argument in &spec.args {
        reject(argument, "argument")?;
    }
    reject(spec.cwd.as_os_str(), "working directory")?;
    for (key, value) in &spec.env {
        reject(key, "environment key")?;
        reject(value, "environment value")?;
    }
    Ok(())
}

enum Kind {
    Binary,
    Batch,
    Missing,
}

fn kind(path: &Path) -> Kind {
    if !path.is_file() {
        return Kind::Missing;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd") => Kind::Batch,
        _ => Kind::Binary,
    }
}

/// Candidate paths for an explicit program path. Like `CreateProcessW`, a name
/// without an extension prefers the `.exe` form before the literal file.
fn executable_forms(path: &Path) -> Vec<PathBuf> {
    if path.extension().is_some() {
        return vec![path.to_owned()];
    }
    let mut with_exe = path.to_owned();
    with_exe.set_extension("exe");
    vec![with_exe, path.to_owned()]
}

/// Resolves the logical program to an executable at prepare time.
///
/// An explicit relative path resolves against the effective child working directory.
/// A bare name searches the effective child environment's `PATH`, then the foil
/// directory, then the system directories, then the parent `PATH`. Batch files are
/// rejected outright rather than run with ordinary-executable quoting.
fn resolve_application(spec: &CommandSpec) -> io::Result<PathBuf> {
    let path = Path::new(&spec.program);
    if spec.program.is_empty() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path has no file name",
        ));
    }
    if path.components().count() > 1 {
        let rooted = if path.is_absolute() {
            path.to_owned()
        } else {
            spec.cwd.join(path)
        };
        return resolve_explicit(&spec.program, &rooted);
    }
    resolve_bare(spec)
}

fn resolve_explicit(program: &OsString, rooted: &Path) -> io::Result<PathBuf> {
    let forms = executable_forms(rooted);
    for candidate in &forms {
        match kind(candidate) {
            Kind::Binary => return Ok(candidate.clone()),
            Kind::Batch => return Err(batch_rejected(candidate)),
            Kind::Missing => {}
        }
    }
    Err(not_found(program, &forms))
}

fn resolve_bare(spec: &CommandSpec) -> io::Result<PathBuf> {
    let mut directories = Vec::new();
    if let Some(child_path) = spec
        .env
        .iter()
        .rev()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value)
    {
        directories.extend(std::env::split_paths(child_path).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Ok(mut directory) = std::env::current_exe() {
        directory.pop();
        directories.push(directory);
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        let root = PathBuf::from(&root);
        directories.push(root.join("System32"));
        directories.push(root);
    }
    if let Some(parent_path) = std::env::var_os("PATH") {
        directories.extend(
            std::env::split_paths(&parent_path).filter(|path| !path.as_os_str().is_empty()),
        );
    }

    let mut batch_alternative = None;
    for directory in &directories {
        let base = directory.join(&spec.program);
        for candidate in executable_forms(&base) {
            match kind(&candidate) {
                Kind::Binary => return Ok(candidate),
                Kind::Batch => {}
                Kind::Missing => {}
            }
        }
        for extension in ["bat", "cmd"] {
            let mut script = base.clone();
            script.set_extension(extension);
            if script.is_file() && batch_alternative.is_none() {
                batch_alternative = Some(script);
            }
        }
    }

    match batch_alternative {
        Some(script) => Err(batch_rejected(&script)),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Could not find the benchmark program `{}` on the search path.",
                spec.program.to_string_lossy()
            ),
        )),
    }
}

fn batch_rejected(script: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "`{}` is a batch file; foil cannot run `.bat` or `.cmd` programs directly \
             because their argument quoting differs from ordinary executables. \
             Invoke it through the shell instead, e.g. `-- cmd /c {} ...`.",
            script.display(),
            script.display()
        ),
    )
}

fn not_found(program: &OsString, attempted: &[PathBuf]) -> io::Error {
    let tried = attempted
        .iter()
        .map(|path| format!("`{}`", path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "Could not find the benchmark program `{}`. Tried: {tried}.",
            program.to_string_lossy()
        ),
    )
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn command_line(spec: &CommandSpec) -> Vec<u16> {
    let mut line = Vec::new();
    for argument in
        std::iter::once(spec.program.as_os_str()).chain(spec.args.iter().map(OsString::as_os_str))
    {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        let argument: Vec<_> = argument.encode_wide().collect();
        let quoted = argument.is_empty()
            || argument
                .iter()
                .any(|&unit| [b' ' as u16, b'\t' as u16, b'"' as u16].contains(&unit));
        if quoted {
            line.push(b'"' as u16);
        }
        let mut backslashes = 0;
        for unit in argument {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else {
                if unit == b'"' as u16 {
                    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
                } else {
                    line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
                }
                backslashes = 0;
                line.push(unit);
            }
        }
        line.extend(std::iter::repeat_n(
            b'\\' as u16,
            if quoted { backslashes * 2 } else { backslashes },
        ));
        if quoted {
            line.push(b'"' as u16);
        }
    }
    line.push(0);
    line
}

/// `None` inherits the parent environment untouched. Windows matches and sorts
/// names case-insensitively using CompareStringOrdinal.
fn environment_block(overrides: &[(OsString, OsString)]) -> Option<Vec<u16>> {
    if overrides.is_empty() {
        return None;
    }

    let mut variables: Vec<_> = std::env::vars_os().collect();
    for (key, value) in overrides {
        variables
            .retain(|(existing, _)| compare_ordinal(existing, key) != std::cmp::Ordering::Equal);
        variables.push((key.clone(), value.clone()));
    }
    variables.sort_by(|(a, _), (b, _)| compare_ordinal(a, b));

    let mut block = Vec::new();
    for (key, value) in variables {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, ensure};

    fn spec(test: &str) -> Result<CommandSpec> {
        Ok(CommandSpec::new(
            std::env::current_exe()?.into_os_string(),
            ["--exact", test, "--ignored"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            std::env::current_dir()?,
            Vec::new(),
        ))
    }

    #[test]
    fn direct_exit_wins_and_consumes_a_pending_interrupt() -> Result<()> {
        let mut session = Session::new()?;
        let interrupt = Interrupt::new()?;
        let mut exited = session
            .prepare(&spec("platform::windows::tests::fast_child")?)?
            .spawn()?;
        ensure!(exited.child.wait()?.success());
        interrupt.signal();
        ensure!(matches!(exited.wait(&interrupt, None)?, Wait::Exited));
        ensure!(exited.finish().cleanup.is_ok());

        let mut next = session
            .prepare(&spec("platform::windows::tests::slow_child")?)?
            .spawn()?;
        ensure!(matches!(
            next.wait(&interrupt, Some(Duration::ZERO))?,
            Wait::TimedOut
        ));
        ensure!(next.finish().cleanup.is_ok());
        session.shutdown()?;
        Ok(())
    }

    #[test]
    #[ignore]
    fn fast_child() {}

    #[test]
    #[ignore]
    fn slow_child() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
