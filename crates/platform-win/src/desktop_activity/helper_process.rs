// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Atomic helper containment. The job is attached by CreateProcessW itself.
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    FILETIME, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::*;

#[derive(Debug)]
pub(super) struct Child {
    process: OwnedHandle,
    thread: Option<OwnedHandle>,
    pub stdout: Option<File>,
    started_filetime: u64,
    job: std::sync::Arc<super::JobHandle>,
}

fn handle(value: &OwnedHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

// Windows CRT command-line quoting, including trailing backslashes.
fn quote(value: &std::ffi::OsStr) -> Vec<u16> {
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for ch in value.encode_wide() {
        if ch == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if ch == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

impl Child {
    pub(super) fn spawn(
        program: &Path,
        args: &[&str],
        job: std::sync::Arc<super::JobHandle>,
    ) -> io::Result<Self> {
        use windows::Win32::Foundation::SetHandleInformation;
        use windows::Win32::System::Pipes::CreatePipe;
        unsafe {
            let started_filetime =
                filetime(windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime());
            let mut read = HANDLE::default();
            let mut write = HANDLE::default();
            CreatePipe(&mut read, &mut write, None, 0)?;
            let stdout = File::from_raw_handle(read.0);
            let write = OwnedHandle::from_raw_handle(write.0);
            SetHandleInformation(handle(&write), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)?;
            let null = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("NUL")?;
            let null_handle = HANDLE(null.as_raw_handle());
            SetHandleInformation(null_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)?;

            let mut size = 0;
            let _ = InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                2,
                0,
                &mut size,
            );
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
            InitializeProcThreadAttributeList(list, 2, 0, &mut size)?;
            let attributes = Attributes {
                _storage: storage,
                list,
            };
            let jobs = [job.0];
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_ptr().cast()),
                std::mem::size_of_val(&jobs),
                None,
                None,
            )?;
            let inherited = [handle(&write), null_handle];
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(inherited.as_ptr().cast()),
                std::mem::size_of_val(&inherited),
                None,
                None,
            )?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdOutput = handle(&write);
            startup.StartupInfo.hStdError = null_handle;
            startup.StartupInfo.hStdInput = null_handle;
            startup.lpAttributeList = attributes.list;
            let application: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut command = quote(program.as_os_str());
            for arg in args {
                command.push(b' ' as u16);
                command.extend(quote(std::ffi::OsStr::new(arg)));
            }
            command.push(0);
            // Copy the environment only into the child; the parent pump guard is never bypassed.
            let mut environment: Vec<(std::ffi::OsString, std::ffi::OsString)> =
                std::env::vars_os()
                    .filter(|(key, _)| !key.eq_ignore_ascii_case("SOLSTONE_PUMP_READY"))
                    .collect();
            environment.push(("SOLSTONE_PUMP_READY".into(), "1".into()));
            environment.sort_by_key(|(key, _)| key.to_string_lossy().to_ascii_uppercase());
            let mut block = Vec::<u16>::new();
            for (key, value) in environment {
                block.extend(key.encode_wide());
                block.push(b'=' as u16);
                block.extend(value.encode_wide());
                block.push(0);
            }
            block.push(0);
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                PWSTR(command.as_mut_ptr()),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT,
                Some(block.as_ptr().cast()),
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut info,
            )?;
            Ok(Self {
                process: OwnedHandle::from_raw_handle(info.hProcess.0),
                thread: Some(OwnedHandle::from_raw_handle(info.hThread.0)),
                stdout: Some(stdout),
                started_filetime,
                job,
            })
        }
    }

    pub(super) fn resume(&mut self) -> io::Result<()> {
        let thread = self
            .thread
            .take()
            .ok_or_else(|| io::Error::other("helper already resumed"))?;
        if unsafe { ResumeThread(handle(&thread)) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        unsafe {
            match WaitForSingleObject(handle(&self.process), 0) {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    GetExitCodeProcess(handle(&self.process), &mut code)?;
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    pub(super) fn finished_before_deadline(&self) -> bool {
        unsafe {
            let (mut creation, mut exit, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            GetProcessTimes(
                handle(&self.process),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
            .is_ok()
                && filetime(exit) >= filetime(creation)
                && filetime(exit) >= self.started_filetime
                && filetime(exit).saturating_sub(self.started_filetime) <= 20_000_000
        }
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        // Kill the whole job, including descendants. A helper that exited between
        // ticks is already reaped successfully, not a permanent worker fault.
        let result =
            unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.job.0, 1) };
        match result {
            Ok(()) => Ok(()),
            Err(_) if self.try_wait()?.is_some() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
