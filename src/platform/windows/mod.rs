use super::{CommandSpec, Wait};
use std::{
    ffi::OsString,
    io,
    os::windows::io::OwnedHandle,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

mod raw;

#[derive(Clone)]
pub(crate) struct Interrupt(Arc<raw::Event>);

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        raw::Event::new().map(Arc::new).map(Self)
    }

    pub(crate) fn signal(&self) {
        self.0.signal();
    }
}

pub(crate) struct Workload {
    child: raw::Child,
    job: raw::Job,
}

impl Workload {
    pub(crate) fn prepare() -> io::Result<Prepared> {
        let job = raw::Job::new()?;
        let (input, output) = raw::null_stdio_handles()?;
        let attribute_list = raw::AttributeList::new()?
            .with_job(&job)?
            .with_inherited_handles(&[&input, &output])?;
        Ok(Prepared {
            job,
            attribute_list,
            input,
            output,
            error_output: None,
        })
    }

    /// A workload whose stdout and stderr are spooled to files instead of
    /// discarded, for lifecycle commands that need failure diagnostics.
    pub(crate) fn prepare_spooled(stdout: &Path, stderr: &Path) -> io::Result<Prepared> {
        let job = raw::Job::new()?;
        let input = raw::null_stdio_handles()?.0;
        let stdout_file = raw::create_inheritable_file(stdout)?;
        let stderr_file = raw::create_inheritable_file(stderr)?;
        let attribute_list = raw::AttributeList::new()?
            .with_job(&job)?
            .with_inherited_handles(&[&input, &stdout_file, &stderr_file])?;
        Ok(Prepared {
            job,
            attribute_list,
            input,
            output: stdout_file,
            error_output: Some(stderr_file),
        })
    }

    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        raw::wait_for(&self.child, &interrupt.0, timeout)
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let terminate = self.job.terminate();
        if terminate.is_err() {
            // The job could not be terminated; kill the child directly as a fallback.
            let _ = self.child.terminate();
        }
        let reap = self.child.reap();
        terminate.and(reap)
    }
}

pub(crate) struct Prepared {
    job: raw::Job,
    attribute_list: raw::AttributeList,
    input: OwnedHandle,
    output: OwnedHandle,
    // Only set when spooling; otherwise stderr shares `output` (the NUL device).
    error_output: Option<OwnedHandle>,
}

impl Prepared {
    pub(crate) fn spawn(self, spec: &CommandSpec) -> io::Result<Workload> {
        // Resolution happens here rather than when the benchmark is configured, so a
        // `startup` command can create the executable this command names.
        let application = resolve_application(spec)?;
        let application = wide(application.as_os_str());
        let mut command_line = command_line(spec);
        let environment = environment_block(&spec.env);
        let cwd = wide(spec.cwd.as_os_str());
        let child = raw::spawn_process(
            &application,
            &mut command_line,
            &environment,
            &cwd,
            &self.attribute_list,
            raw::StdioHandles {
                stdin: &self.input,
                stdout: &self.output,
                stderr: self.error_output.as_ref().unwrap_or(&self.output),
            },
        )?;
        Ok(Workload {
            child,
            job: self.job,
        })
    }
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
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        Some(extension) if extension == "bat" || extension == "cmd" => Kind::Batch,
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
            "Could not find the benchmark program `{}`. A relative program resolves \
             against the benchmark's working directory. Tried: {tried}.",
            program.to_string_lossy()
        ),
    )
}

/// Resolves the logical program to an executable at spawn time.
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
    if let Ok(parent_path) = std::env::var("PATH") {
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

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}

fn command_line(spec: &CommandSpec) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
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

fn environment_block(overrides: &[(OsString, OsString)]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut variables: Vec<_> = std::env::vars_os().collect();
    for (key, value) in overrides {
        variables.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        variables.push((key.clone(), value.clone()));
    }
    variables.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_lowercase());
    let mut block = Vec::new();
    for (key, value) in variables {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}
