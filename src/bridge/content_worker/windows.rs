use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use std::ptr::null;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

/// Owns a non-inheritable job that terminates its processes when dropped.
pub(super) struct Job {
    handle: OwnedHandle,
}

impl Job {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: Null attributes create a non-inheritable handle. A null name
        // creates a new unnamed job owned by this process.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new, valid owned handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: The handle and the limits structure remain valid for the
        // call, and the size matches the selected information class.
        let success = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Assign a worker before releasing its gate to start Git.
    pub(super) fn attach(&self, child: &Child) -> io::Result<()> {
        // SAFETY: Both handles are valid and remain owned by their callers.
        let success =
            unsafe { AssignProcessToJobObject(self.handle.as_raw_handle(), child.as_raw_handle()) };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
