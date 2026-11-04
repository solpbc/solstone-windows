// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Start the app again after Setup stopped it.
//!
//! Setup, run over an install, stops the app before it replaces the folder, and
//! a silent run (`winget upgrade`, `Setup --silent`) never starts it again. The
//! install hook can't start it either: when the hook exits, Setup stops anything
//! still running from the install folder. So the hook hands the start to Windows
//! PowerShell, which runs from outside that folder, waits for Setup to exit, and
//! then starts the app the way sign-in does, into the tray.
//!
//! Whether the app was running comes from a mark it keeps in the data root while
//! it runs. Setup stops the app without letting it clear the mark; the owner's
//! Quit clears it. The mark names the sign-in it was written in, so a mark left
//! from an earlier sign-in (the PC restarted with the app open and the owner
//! hasn't opened it since) starts nothing, and neither does a Setup run in
//! another sign-in, such as an elevated one.

use std::fs;
use std::io;
use std::path::Path;

/// The mark's file name in the data root. Setup's state restore carries it across
/// with everything else the owner has.
pub const RUNNING_MARK: &str = "running";

/// Record that the app is running in this sign-in. Called once by the instance
/// that holds the single-instance gate.
pub fn mark_running(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    fs::write(root.join(RUNNING_MARK), sign_in_id()?)
}

/// The owner quit: a later Setup leaves the app stopped.
pub fn clear_running(root: &Path) -> io::Result<()> {
    match fs::remove_file(root.join(RUNNING_MARK)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Whether the app was running in this sign-in when Setup stopped it.
pub fn was_running_in_this_sign_in(root: &Path) -> bool {
    match (fs::read_to_string(root.join(RUNNING_MARK)), sign_in_id()) {
        (Ok(mark), Ok(current)) => mark_matches(&mark, &current),
        _ => false,
    }
}

fn mark_matches(mark: &str, current: &str) -> bool {
    !current.is_empty() && mark.trim() == current
}

/// Hand the start to Windows PowerShell. It waits for Setup, this hook's parent,
/// to exit, then starts `exe` with `start_args`. Returns once PowerShell has been
/// started; nothing reads its outcome.
#[cfg(windows)]
pub fn start_after_setup(exe: &Path, start_args: &[&str]) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let setup = imp::parent_pid()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("app path is not Unicode"))?;
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    // Setup waits on this hook; a child holding inherited copies of its standard
    // handles would keep them open after the hook exits.
    imp::stop_standard_handle_inheritance();
    let mut command = Command::new(powershell);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &start_after_setup_script(setup, exe, start_args),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    // Velopack's launch markers belong to the launch they were set for.
    for name in ["VELOPACK_FIRSTRUN", "VELOPACK_RESTART"] {
        command.env_remove(name);
    }
    command.spawn().map(|_| ())
}

/// The script PowerShell runs. Values travel as single-quoted literals (a quote
/// doubled), which PowerShell never expands.
///
/// Setup is alive while this hook runs, so it started before PowerShell did. A
/// process with Setup's id that started later is a reused id, not Setup.
#[cfg_attr(not(windows), allow(dead_code))]
fn start_after_setup_script(setup_pid: u32, exe: &str, start_args: &[&str]) -> String {
    let literal = |value: &str| format!("'{}'", value.replace('\'', "''"));
    let args = start_args
        .iter()
        .map(|arg| literal(arg))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "$exe={exe}; $self=[Diagnostics.Process]::GetCurrentProcess().StartTime; \
         $setup=Get-Process -Id {setup_pid} -ErrorAction SilentlyContinue; \
         if ($setup -and $setup.StartTime -lt $self -and -not $setup.WaitForExit(300000)) {{ exit 1 }}; \
         Start-Process -FilePath $exe -ArgumentList @({args}) -WorkingDirectory (Split-Path -Parent $exe)",
        exe = literal(exe),
    )
}

#[cfg(windows)]
fn sign_in_id() -> io::Result<String> {
    imp::logon_session()
}

/// Off Windows there is no sign-in to match, so no mark ever starts the app.
#[cfg(not(windows))]
fn sign_in_id() -> io::Result<String> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::c_void;
    use std::io;

    use windows::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenStatistics, TOKEN_QUERY, TOKEN_STATISTICS,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The logon session this process belongs to. Each sign-in gets a new one,
    /// and an elevated token has its own.
    pub(super) fn logon_session() -> io::Result<String> {
        let mut token = HANDLE::default();
        // SAFETY: queries this process's own token into a local handle.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(io::Error::other)?;
        let mut stats = TOKEN_STATISTICS::default();
        let mut needed = 0u32;
        // SAFETY: the buffer is a TOKEN_STATISTICS of the size passed.
        let result = unsafe {
            GetTokenInformation(
                token,
                TokenStatistics,
                Some(&mut stats as *mut TOKEN_STATISTICS as *mut c_void),
                std::mem::size_of::<TOKEN_STATISTICS>() as u32,
                &mut needed,
            )
        };
        // SAFETY: closes the token opened above.
        unsafe {
            let _ = CloseHandle(token);
        }
        result.map_err(io::Error::other)?;
        let id = stats.AuthenticationId;
        Ok(format!("{:08x}{:08x}", id.HighPart as u32, id.LowPart))
    }

    /// This process's parent: Setup, for the install hook.
    pub(super) fn parent_pid() -> io::Result<u32> {
        let own = std::process::id();
        // SAFETY: a process snapshot walked with a correctly sized entry, then closed.
        unsafe {
            let snapshot =
                CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(io::Error::other)?;
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = None;
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                if entry.th32ProcessID == own {
                    found = Some(entry.th32ParentProcessID);
                    break;
                }
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
            found.ok_or_else(|| io::Error::other("this process is not in the snapshot"))
        }
    }

    pub(super) fn stop_standard_handle_inheritance() {
        use std::os::windows::io::AsRawHandle;
        for handle in [
            std::io::stdin().as_raw_handle(),
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            // SAFETY: clears one flag on this process's own standard handle.
            unsafe {
                let _ =
                    SetHandleInformation(HANDLE(handle), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_matches_only_the_same_sign_in() {
        assert!(mark_matches("000000000004a2f1", "000000000004a2f1"));
        assert!(mark_matches("000000000004a2f1\r\n", "000000000004a2f1"));
        assert!(!mark_matches("000000000004a2f1", "00000000000b7c20"));
        assert!(!mark_matches("", ""));
    }

    #[test]
    fn script_keeps_paths_literal() {
        let script = start_after_setup_script(
            4242,
            r"C:\Users\o'neil\AppData\Local\Solstone\current\solstone-windows-app.exe",
            &["--from-autostart"],
        );
        assert!(script.contains(
            r"$exe='C:\Users\o''neil\AppData\Local\Solstone\current\solstone-windows-app.exe'"
        ));
        assert!(script.contains("-ArgumentList @('--from-autostart')"));
    }
}
