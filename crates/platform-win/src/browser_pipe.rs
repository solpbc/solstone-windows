// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The browser native host's same-user local endpoint: a named pipe.
//!
//! The trust boundary is the OS user. The pipe's name carries the user's SID
//! and the session id, its DACL grants only that user, it rejects remote
//! clients, and the app creates it as the first instance, so a pipe someone
//! else created under the name is a collision the app refuses rather than
//! shares. The host, before sending anything, checks the server end belongs to
//! a process of the same user in the same session. There is no code-signing or
//! image-path check: any same-user process can drive the genuine host, so such
//! a check would prove nothing.

use std::io;

#[cfg(windows)]
pub use imp::{connect, create_server, pipe_name};

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
    };
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;

    fn win_err(e: windows::core::Error) -> io::Error {
        io::Error::other(e)
    }

    /// The string SID of the user `token` belongs to.
    fn token_user_sid(token: HANDLE) -> io::Result<String> {
        let mut needed = 0u32;
        // First call sizes the buffer; it fails by design.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; needed as usize];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr() as *mut c_void),
                needed,
                &mut needed,
            )
        }
        .map_err(win_err)?;
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let mut text = PWSTR::null();
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.map_err(win_err)?;
        let sid = unsafe { text.to_string() }.map_err(|e| io::Error::other(e.to_string()));
        unsafe {
            let _ = LocalFree(HLOCAL(text.0 as *mut c_void));
        }
        sid
    }

    fn process_user_sid(process: HANDLE) -> io::Result<String> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(win_err)?;
        let sid = token_user_sid(token);
        unsafe {
            let _ = CloseHandle(token);
        }
        sid
    }

    fn current_user_sid() -> io::Result<String> {
        process_user_sid(unsafe { GetCurrentProcess() })
    }

    fn current_session() -> io::Result<u32> {
        let mut session = 0u32;
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }.map_err(win_err)?;
        Ok(session)
    }

    /// `\\.\pipe\solstone-browser-host-<user SID>-<session>`.
    pub fn pipe_name() -> io::Result<String> {
        Ok(format!(
            r"\\.\pipe\solstone-browser-host-{}-{}",
            current_user_sid()?,
            current_session()?
        ))
    }

    /// Create one server instance with a DACL granting only the current user.
    /// `first` makes creation fail if any instance of the name already exists.
    pub fn create_server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", current_user_sid()?)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(win_err)?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .pipe_mode(PipeMode::Byte)
                .create_with_security_attributes_raw(
                    name,
                    &mut attributes as *mut SECURITY_ATTRIBUTES as *mut c_void,
                )
        };
        unsafe {
            let _ = LocalFree(HLOCAL(descriptor.0));
        }
        server
    }

    /// Connect to the app's endpoint. `Ok(None)` when no app is listening.
    /// Refuses an endpoint whose server is not this user in this session.
    pub async fn connect(name: &str) -> io::Result<Option<NamedPipeClient>> {
        let mut attempts = 0;
        let client = loop {
            match ClientOptions::new().open(name) {
                Ok(client) => break client,
                Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => return Ok(None),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let handle = HANDLE(client.as_raw_handle());
        let mut pid = 0u32;
        unsafe { GetNamedPipeServerProcessId(handle, &mut pid) }.map_err(win_err)?;
        let mut session = 0u32;
        unsafe { GetNamedPipeServerSessionId(handle, &mut session) }.map_err(win_err)?;
        if session != current_session()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint_other_session",
            ));
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "endpoint_other_user"))?;
        let owner = process_user_sid(process);
        unsafe {
            let _ = CloseHandle(process);
        }
        if owner? != current_user_sid()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint_other_user",
            ));
        }
        Ok(Some(client))
    }
}

/// Whether `error` from [`create_server`] with `first` means another
/// instance already holds the name (an app already running, or a squatter).
pub fn is_collision(error: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED (5) for a first-instance clash; ERROR_PIPE_BUSY (231).
    matches!(error.raw_os_error(), Some(5) | Some(231))
        || error.kind() == io::ErrorKind::PermissionDenied
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_name(tag: &str) -> String {
        format!("{}-test-{tag}-{}", pipe_name().unwrap(), std::process::id())
    }

    #[tokio::test]
    async fn a_same_user_client_reaches_the_server_both_ways() {
        let name = test_name("roundtrip");
        let server = create_server(&name, true).unwrap();
        let accept = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut server = server;
            let mut buf = [0u8; 5];
            server.read_exact(&mut buf).await.unwrap();
            server.write_all(b"pong!").await.unwrap();
            buf
        });
        let mut client = connect(&name).await.unwrap().expect("endpoint present");
        client.write_all(b"ping!").await.unwrap();
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"pong!");
        assert_eq!(&accept.await.unwrap(), b"ping!");
    }

    #[tokio::test]
    async fn a_second_first_instance_is_a_collision() {
        let name = test_name("collision");
        let _held = create_server(&name, true).unwrap();
        let err = create_server(&name, true).unwrap_err();
        assert!(is_collision(&err), "{err:?}");
    }

    #[tokio::test]
    async fn no_endpoint_is_absent_not_an_error() {
        let name = test_name("absent");
        assert!(connect(&name).await.unwrap().is_none());
    }
}
