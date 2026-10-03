// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Raw Windows facts used by the pure About renderer.

use observer_model::about::WindowsObservation;

/// Read the machine build string and native machine code. Failures are kept as
/// missing facts so the pure renderer can omit them without guessing.
pub fn windows_about_observation() -> WindowsObservation {
    WindowsObservation {
        build_number: current_build_number(),
        native_machine: native_machine_code(),
    }
}

#[cfg(windows)]
fn current_build_number() -> Option<String> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
    let current_version = machine
        .open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion")
        .ok()?;
    current_version.get_value("CurrentBuildNumber").ok()
}

#[cfg(not(windows))]
fn current_build_number() -> Option<String> {
    None
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn native_machine_code() -> Option<u16> {
    use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE;
    use windows::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};

    let mut process_machine = IMAGE_FILE_MACHINE(0);
    let mut native_machine = IMAGE_FILE_MACHINE(0);
    // Only the native machine output is reported. The process machine is
    // intentionally ignored so emulation does not change the machine fact.
    unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &mut process_machine,
            Some(&mut native_machine),
        )
        .ok()?;
    }
    Some(native_machine.0)
}

#[cfg(not(windows))]
fn native_machine_code() -> Option<u16> {
    None
}
