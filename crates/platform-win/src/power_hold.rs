// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! RAII Windows power request guard for active import delivery.
//!
//! Uses handle-based `PowerCreateRequest` / `PowerSetRequest` rather than
//! thread-local `SetThreadExecutionState`, so a hold can safely live across
//! async worker threads and is released on every exit path via `Drop`.
//!
//! Holds both `PowerRequestExecutionRequired` and `PowerRequestSystemRequired`:
//! - `PowerRequestExecutionRequired` keeps Windows process lifetime management
//!   and the Modern Standby Desktop Activity Moderator (DAM) from suspending
//!   the app, and implies system-required on Traditional Sleep (S3) PCs.
//! - `PowerRequestSystemRequired` blocks idle sleep transitions (including the
//!   Modern Standby No-CS idle phase on AC) where `PowerRequestExecutionRequired`
//!   does not imply system-required on Modern Standby hardware.
//!
//! Intentionally omits `PowerRequestDisplayRequired` (the screen may still turn
//! off normally) and `PowerRequestAwayModeRequired` (owner-initiated sleep is
//! honored). On Modern Standby PCs running on battery (DC), Windows enforces a
//! 5-minute post-sleep-timeout guardrail on both request types.

#[cfg(windows)]
pub use imp::PowerHold;

/// Off-Windows no-op guard so the crate stays host-compilable.
#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct PowerHold {
    active: bool,
}

#[cfg(not(windows))]
impl PowerHold {
    pub fn acquire(_reason: &str) -> Self {
        Self { active: false }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn release(&mut self) {
        self.active = false;
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerRequestExecutionRequired,
        PowerRequestSystemRequired, PowerSetRequest,
    };
    use windows::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
    use windows::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };

    /// Active Windows kernel power request guard. Releases every set request
    /// and closes the handle on [`Drop`].
    #[derive(Debug)]
    pub struct PowerHold {
        // Stored as raw pointer value so `PowerHold` can be `Send`.
        handle: usize,
        execution_set: bool,
        system_set: bool,
    }

    // SAFETY: Win32 power request handles created by `PowerCreateRequest` belong
    // to the process kernel handle table with no thread affinity, so calling
    // `PowerClearRequest` and `CloseHandle` from another thread is sound.
    unsafe impl Send for PowerHold {}

    impl PowerHold {
        /// Acquire an awake hold with a diagnostic reason string shown in
        /// `powercfg /requests`. Failures log a warning and return an inactive
        /// guard rather than blocking delivery.
        pub fn acquire(reason: &str) -> Self {
            const FALLBACK_REASON: &str = "solstone import delivery";
            let cleaned = reason.replace('\0', "");
            // Windows rejects an empty simple reason string with ERROR_INVALID_PARAMETER.
            let reason_text = if cleaned.trim().is_empty() {
                FALLBACK_REASON
            } else {
                cleaned.as_str()
            };
            let mut wide: Vec<u16> = reason_text
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: PWSTR(wide.as_mut_ptr()),
                },
            };
            // SAFETY: `context` points to a NUL-terminated UTF-16 buffer that
            // outlives the `PowerCreateRequest` call.
            let handle = match unsafe { PowerCreateRequest(&context) } {
                Ok(handle) => handle,
                Err(error) => {
                    tracing::warn!(target: "import", %error, "power request creation failed");
                    return Self::inactive();
                }
            };
            drop(wide);

            // SAFETY: `handle` is a live power request handle owned exclusively by `Self`.
            let execution_set =
                match unsafe { PowerSetRequest(handle, PowerRequestExecutionRequired) } {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(target: "import", %error, "execution power request failed");
                        false
                    }
                };
            let system_set = match unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) } {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(target: "import", %error, "system power request failed");
                    false
                }
            };

            if !execution_set && !system_set {
                // SAFETY: `handle` is valid and no longer referenced after close.
                let _ = unsafe { CloseHandle(handle) };
                return Self::inactive();
            }

            Self {
                handle: handle.0 as usize,
                execution_set,
                system_set,
            }
        }

        fn inactive() -> Self {
            Self {
                handle: 0,
                execution_set: false,
                system_set: false,
            }
        }

        pub fn is_active(&self) -> bool {
            self.handle != 0 && (self.execution_set || self.system_set)
        }

        pub fn holds_execution(&self) -> bool {
            self.handle != 0 && self.execution_set
        }

        pub fn holds_system(&self) -> bool {
            self.handle != 0 && self.system_set
        }

        /// Explicitly clear any active requests and close the kernel handle.
        /// Idempotent; also called automatically by [`Drop`].
        pub fn release(&mut self) {
            let raw = std::mem::replace(&mut self.handle, 0);
            if raw == 0 {
                return;
            }
            let handle = HANDLE(raw as *mut core::ffi::c_void);
            if std::mem::take(&mut self.system_set) {
                // SAFETY: `handle` is live and `PowerRequestSystemRequired` was set once.
                if let Err(error) = unsafe { PowerClearRequest(handle, PowerRequestSystemRequired) }
                {
                    tracing::warn!(target: "import", %error, "system power request clear failed");
                }
            }
            if std::mem::take(&mut self.execution_set) {
                // SAFETY: `handle` is live and `PowerRequestExecutionRequired` was set once.
                if let Err(error) =
                    unsafe { PowerClearRequest(handle, PowerRequestExecutionRequired) }
                {
                    tracing::warn!(target: "import", %error, "execution power request clear failed");
                }
            }
            // SAFETY: `handle` is owned exclusively by `Self` and never used again.
            if let Err(error) = unsafe { CloseHandle(handle) } {
                tracing::warn!(target: "import", %error, "power request handle close failed");
            }
        }
    }

    impl Drop for PowerHold {
        fn drop(&mut self) {
            self.release();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn power_hold_acquires_system_and_execution_and_releases_across_threads() {
            let hold = PowerHold::acquire("solstone import delivery test");
            assert!(hold.is_active());
            assert!(hold.holds_execution());
            assert!(hold.holds_system());

            // Prove cross-thread Send + Drop works cleanly (matching Tokio worker migration).
            std::thread::spawn(move || {
                assert!(hold.is_active());
                drop(hold);
            })
            .join()
            .unwrap();
        }

        #[test]
        fn power_hold_release_is_idempotent_and_tolerates_nul_and_empty_reasons() {
            let mut hold = PowerHold::acquire("sending\0import");
            assert!(hold.is_active());
            hold.release();
            assert!(!hold.is_active());
            assert!(!hold.holds_execution());
            assert!(!hold.holds_system());
            // Second release and Drop are safe no-ops.
            hold.release();
            drop(hold);

            let empty = PowerHold::acquire("");
            assert!(empty.is_active());
            drop(empty);

            let nul_only = PowerHold::acquire("\0");
            assert!(nul_only.is_active());
            drop(nul_only);
        }
    }
}
