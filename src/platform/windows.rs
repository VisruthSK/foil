use super::{CommandSpec, Wait};
use std::{
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    os::windows::process::ExitStatusExt,
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
        let mut command_line = spec.command_line.clone();
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
                spec.application.as_ptr(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                TRUE,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                spec.environment.as_ptr().cast(),
                spec.cwd_wide.as_ptr(),
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
