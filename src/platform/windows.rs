use super::{CommandSpec, Wait};
use std::{
    ffi::OsString,
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    os::windows::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_FAILED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, SetEvent, TerminateProcess,
            UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};

#[derive(Clone)]
pub(crate) struct Interrupt(Arc<OwnedHandle>);

impl Interrupt {
    pub(crate) fn new() -> io::Result<Self> {
        let handle = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        owned(handle).map(Arc::new).map(Self)
    }

    pub(crate) fn signal(&self) {
        unsafe { SetEvent(self.0.as_raw_handle() as HANDLE) };
    }
}

pub(crate) struct Workload {
    child: OwnedHandle,
    job: OwnedHandle,
}

pub(crate) struct Prepared {
    job: OwnedHandle,
    attributes: Attributes,
    input: OwnedHandle,
    output: OwnedHandle,
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    jobs: Box<[HANDLE; 1]>,
    handles: Box<[HANDLE; 2]>,
}

impl Workload {
    pub(crate) fn prepare() -> io::Result<Prepared> {
        let job = create_job()?;
        let input = null_handle(GENERIC_READ)?;
        let output = null_handle(GENERIC_WRITE)?;
        let attributes = Attributes::new(&job, &input, &output)?;
        Ok(Prepared {
            job,
            attributes,
            input,
            output,
        })
    }

    pub(crate) fn wait(
        &mut self,
        interrupt: &Interrupt,
        timeout: Option<Duration>,
    ) -> io::Result<Wait> {
        let handles = [
            self.child.as_raw_handle() as HANDLE,
            interrupt.0.as_raw_handle() as HANDLE,
        ];
        let started = Instant::now();
        loop {
            let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
            let result = unsafe {
                WaitForMultipleObjects(2, handles.as_ptr(), FALSE, milliseconds(remaining))
            };
            match result {
                WAIT_OBJECT_0 => return self.exit_status().map(Wait::Exited),
                value if value == WAIT_OBJECT_0 + 1 => return Ok(Wait::Interrupted),
                WAIT_TIMEOUT if timeout.is_some_and(|limit| started.elapsed() >= limit) => {
                    return Ok(Wait::TimedOut);
                }
                WAIT_TIMEOUT => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Err(io::Error::other("Unexpected wait result.")),
            }
        }
    }

    pub(crate) fn terminate(&mut self) -> io::Result<()> {
        let terminate =
            bool_result(unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) });
        if terminate.is_err() {
            unsafe { TerminateProcess(self.child.as_raw_handle() as HANDLE, 1) };
        }
        let reap = self.reap();
        terminate.and(reap)
    }

    fn reap(&self) -> io::Result<()> {
        match unsafe { WaitForSingleObject(self.child.as_raw_handle() as HANDLE, INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("Unexpected process wait result.")),
        }
    }

    fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code = 0;
        bool_result(unsafe {
            GetExitCodeProcess(self.child.as_raw_handle() as HANDLE, &mut code)
        })?;
        Ok(ExitStatus::from_raw(code))
    }
}

impl Prepared {
    pub(crate) fn spawn(self, spec: &CommandSpec) -> io::Result<Workload> {
        // Resolution happens here rather than when the benchmark is configured, so a
        // `startup` command can create the executable this command names.
        let application = resolve_application(spec)?;
        let mut application = wide(application.as_os_str());
        let mut command_line = command_line(spec);
        let environment = environment_block(&spec.env);
        let cwd_wide = wide(spec.cwd.as_os_str());
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = self.input.as_raw_handle() as HANDLE;
        startup.StartupInfo.hStdOutput = self.output.as_raw_handle() as HANDLE;
        startup.StartupInfo.hStdError = self.output.as_raw_handle() as HANDLE;
        startup.lpAttributeList = self.attributes.list;
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        bool_result(unsafe {
            CreateProcessW(
                application.as_mut_ptr(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                TRUE,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                cwd_wide.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        })?;
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread as _) };
        drop(thread);
        let child = unsafe { OwnedHandle::from_raw_handle(info.hProcess as _) };
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
    for argument in std::iter::once(spec.program.as_os_str()).chain(spec.args.iter().map(OsString::as_os_str)) {
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

impl Attributes {
    fn new(job: &OwnedHandle, input: &OwnedHandle, output: &OwnedHandle) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes) };
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr().cast();
        bool_result(unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut bytes) })?;
        let jobs = Box::new([job.as_raw_handle() as HANDLE]);
        let handles = Box::new([
            input.as_raw_handle() as HANDLE,
            output.as_raw_handle() as HANDLE,
        ]);
        let attributes = Self {
            _storage: storage,
            list,
            jobs,
            handles,
        };
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                attributes.jobs.as_ptr().cast(),
                size_of::<HANDLE>(),
                ptr::null_mut(),
                ptr::null(),
            )
        })?;
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                attributes.handles.as_ptr().cast(),
                size_of::<[HANDLE; 2]>(),
                ptr::null_mut(),
                ptr::null(),
            )
        })?;
        Ok(attributes)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

fn create_job() -> io::Result<OwnedHandle> {
    let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    bool_result(unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            &info as *const _ as _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

fn null_handle(access: u32) -> io::Result<OwnedHandle> {
    const NUL: [u16; 4] = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    let handle = unsafe {
        CreateFileW(
            NUL.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
    }
}

fn milliseconds(timeout: Option<Duration>) -> u32 {
    timeout.map_or(u32::MAX, |timeout| {
        let millis = timeout.as_millis() + u128::from(timeout.subsec_nanos() % 1_000_000 != 0);
        millis.min(u128::from(u32::MAX - 1)) as u32
    })
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
    }
}

fn bool_result(result: i32) -> io::Result<()> {
    if result == FALSE {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
