//! The crate's only unsafe module: every Win32 call, behind safe RAII types
//! that own their handles through `OwnedHandle`.

#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    os::windows::process::ExitStatusExt,
    process::ExitStatus,
    ptr,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        FALSE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_FAILED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
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

use crate::platform::Wait;

/// A kill-on-close job object covering the whole workload.
pub(crate) struct Job(OwnedHandle);

impl Job {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Both parameters are null, which requests an unnamed job object.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        owned(raw).map(Self)?.with_kill_on_close()
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: `self.0` is a valid job object handle for the lifetime of `self`.
        bool_result(unsafe { TerminateJobObject(self.as_handle(), 1) })
    }

    fn as_handle(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    fn with_kill_on_close(self) -> io::Result<Self> {
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `self.0` is a valid job object handle; `info` outlives the call.
        bool_result(unsafe {
            SetInformationJobObject(
                self.as_handle(),
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(self)
    }
}

/// A manual-reset event that wakes a blocked wait on interrupt.
pub(crate) struct Event(OwnedHandle);

impl Event {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Name is null (unnamed event); manual reset, initially clear.
        let raw = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        owned(raw).map(Self)
    }

    pub(crate) fn signal(&self) {
        // SAFETY: `self.0` is a valid event handle for the lifetime of `self`.
        unsafe { SetEvent(self.as_handle()) };
    }

    fn as_handle(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

pub(crate) struct Child(OwnedHandle);

impl Child {
    pub(crate) fn exit_status(&self) -> io::Result<ExitStatus> {
        let mut code = 0;
        // SAFETY: `self.0` is a valid process handle; `code` outlives the call.
        bool_result(unsafe { GetExitCodeProcess(self.as_handle(), &mut code) })?;
        Ok(ExitStatus::from_raw(code))
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: `self.0` is a valid process handle.
        bool_result(unsafe { TerminateProcess(self.as_handle(), 1) })
    }

    /// Blocks until the child exits, whatever caused it to exit.
    pub(crate) fn reap(&self) -> io::Result<()> {
        // SAFETY: `self.0` is a valid process handle.
        match unsafe { WaitForSingleObject(self.as_handle(), INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("Unexpected process wait result.")),
        }
    }

    fn as_handle(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

/// An initialized attribute list. It points into `_storage`, which must never
/// reallocate, and keeps the data its attributes reference alive.
pub(crate) struct AttributeList {
    _storage: Box<[usize]>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    jobs: Option<Box<[HANDLE; 1]>>,
    handles: Option<Box<[HANDLE]>>,
}

impl AttributeList {
    pub(crate) fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: A null list with a valid byte count queries the required size;
        // the failure of this query call is expected and its result is ignored.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes) };
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())].into_boxed_slice();
        let list = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `list` points into `storage`, which was sized by the query
        // above for exactly 2 attributes; `bytes` remains writable for the call.
        bool_result(unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut bytes) })?;
        Ok(Self {
            _storage: storage,
            list,
            jobs: None,
            handles: None,
        })
    }

    pub(crate) fn with_job(mut self, job: &Job) -> io::Result<Self> {
        let jobs = Box::new([job.as_handle()]);
        // SAFETY: `self.list` is initialized; `jobs` outlives the CreateProcessW
        // call via `self.jobs`.
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_ptr().cast(),
                size_of::<HANDLE>(),
                ptr::null_mut(),
                ptr::null(),
            )
        })?;
        self.jobs = Some(jobs);
        Ok(self)
    }

    pub(crate) fn with_inherited_handles(mut self, handles: &[&OwnedHandle]) -> io::Result<Self> {
        let handles: Box<[HANDLE]> = handles
            .iter()
            .map(|handle| handle.as_raw_handle() as HANDLE)
            .collect();
        // SAFETY: `self.list` is initialized; `handles` outlives the
        // CreateProcessW call via `self.handles`.
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of::<HANDLE>() * handles.len(),
                ptr::null_mut(),
                ptr::null(),
            )
        })?;
        self.handles = Some(handles);
        Ok(self)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: `self.list` was successfully initialized and not yet deleted.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

/// Handles to the NUL device, one readable and one writable, for silent stdio.
pub(crate) fn null_stdio_handles() -> io::Result<(OwnedHandle, OwnedHandle)> {
    const NUL: [u16; 4] = [b'N' as u16, b'U' as u16, b'L' as u16, 0];
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    let open = |access: u32| -> io::Result<OwnedHandle> {
        // SAFETY: `NUL` is NUL-terminated for the duration of the call; the
        // security attributes pointer stays valid for the call.
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
            // SAFETY: `handle` is a valid file handle just returned by
            // CreateFileW; ownership transfers to the OwnedHandle here.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
        }
    };
    Ok((open(GENERIC_READ)?, open(GENERIC_WRITE)?))
}

/// Opens a fresh, writable, inheritable handle at `path` for spooling one of a
/// child's output streams.
pub(crate) fn create_inheritable_file(path: &std::path::Path) -> io::Result<OwnedHandle> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide_path.push(0);

    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    // SAFETY: `wide_path` is NUL-terminated and outlives the call; the security
    // attributes pointer stays valid for the call.
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a valid file handle just returned by CreateFileW;
    // ownership transfers to the OwnedHandle here.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
}

/// The three stdio handles a spawned child inherits.
pub(crate) struct StdioHandles<'a> {
    pub(crate) stdin: &'a OwnedHandle,
    pub(crate) stdout: &'a OwnedHandle,
    pub(crate) stderr: &'a OwnedHandle,
}

/// Spawns `command_line` with `application` as the resolved executable image,
/// attached to `attribute_list`'s job, with stdio wired to the inherited handles.
pub(crate) fn spawn_process(
    application: &[u16],
    command_line: &mut [u16],
    environment: &[u16],
    cwd: &[u16],
    attribute_list: &AttributeList,
    stdio: StdioHandles<'_>,
) -> io::Result<Child> {
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio.stdin.as_raw_handle() as HANDLE;
    startup.StartupInfo.hStdOutput = stdio.stdout.as_raw_handle() as HANDLE;
    startup.StartupInfo.hStdError = stdio.stderr.as_raw_handle() as HANDLE;
    startup.lpAttributeList = attribute_list.as_ptr();

    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };

    // SAFETY:
    // - `application` is NUL-terminated and stays alive for the call; the API
    //   treats lpApplicationName as read-only.
    // - `command_line` is writable and NUL-terminated, as the API requires.
    // - `environment` is a double-NUL-terminated block sorted for case-insensitive lookup.
    // - `cwd` is NUL-terminated.
    // - `startup` and its attribute list (and the data its attributes reference)
    //   all remain alive and unmodified for the duration of the call.
    // - The inherited stdio handles remain valid for the duration of the call.
    bool_result(unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            TRUE,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        )
    })?;

    // SAFETY: On success CreateProcessW returns owned handles; the thread
    // handle is never needed here, so it closes immediately.
    drop(unsafe { OwnedHandle::from_raw_handle(info.hThread as _) });
    // SAFETY: The process handle stays alive inside Child.
    let child = Child(unsafe { OwnedHandle::from_raw_handle(info.hProcess as _) });

    Ok(child)
}

/// Waits for `child` to exit, `interrupt` to fire, or `timeout` to elapse.
pub(crate) fn wait_for(
    child: &Child,
    interrupt: &Event,
    timeout: Option<Duration>,
) -> io::Result<Wait> {
    let handles = [child.as_handle(), interrupt.as_handle()];
    let started = Instant::now();
    loop {
        let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
        // SAFETY: Both array entries are valid handles for the whole loop;
        // the count matches the array length.
        let result =
            unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, milliseconds(remaining)) };
        match result {
            WAIT_OBJECT_0 => return child.exit_status().map(Wait::Exited),
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

fn milliseconds(timeout: Option<Duration>) -> u32 {
    timeout.map_or(u32::MAX, |timeout| {
        let millis = timeout.as_millis() + u128::from(timeout.subsec_nanos() % 1_000_000 != 0);
        millis.min(u128::from(u32::MAX - 1)) as u32
    })
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is non-null and owned by us; the conversion transfers
    // ownership into OwnedHandle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as _) })
}

fn bool_result(result: i32) -> io::Result<()> {
    if result == FALSE {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
