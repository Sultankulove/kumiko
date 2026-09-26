//! Windows Job Objects (ADR-020): a process plugin and everything it starts live in one job.
//! A forced stop terminates the whole job, and the job kills what is left once its handle is
//! closed: when the plugin's main process has exited, and when the IDE itself dies.
//!
//! Processes started by the plugin between its spawn and [`Job::assign`] escape the job; a
//! plugin does not start helpers before its `initialize` handshake, so the window is harmless.

#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

pub(crate) struct Job(OwnedHandle);

impl Job {
    /// A new job that kills its processes when closed, with `process` assigned to it.
    pub(crate) fn assign(process: RawHandle) -> io::Result<Self> {
        // SAFETY: no security attributes and no name; the result is checked before use.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh job handle nothing else owns.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job handle is valid; the pointer and size describe `limits`, which
        // outlives the call.
        let set = unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles are valid; the caller keeps the process handle open.
        if unsafe { AssignProcessToJobObject(job.handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Kills every process in the job.
    pub(crate) fn terminate(&self) {
        // SAFETY: the job handle is valid for the lifetime of `self`.
        unsafe { TerminateJobObject(self.handle(), 1) };
    }

    fn handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}
