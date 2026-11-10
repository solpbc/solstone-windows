// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformConnectionCostKind {
    Unrestricted,
    Fixed,
    Variable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformConnectionCost {
    pub kind: PlatformConnectionCostKind,
    pub roaming: bool,
    pub background_restricted: bool,
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub fn connection_cost() -> PlatformConnectionCost {
    use windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::CoIncrementMTAUsage;
    use windows::Win32::System::WinRT::{
        RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED,
    };

    let unknown = || PlatformConnectionCost {
        kind: PlatformConnectionCostKind::Unknown,
        roaming: false,
        background_restricted: false,
    };
    // windows-rs caches agile activation factories for the process. Keep one
    // MTA lease for that same lifetime, including between short-lived workers.
    // Releasing the last apartment per probe can invalidate the cached factory.
    static MTA_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*MTA_AVAILABLE.get_or_init(|| unsafe { CoIncrementMTAUsage() }.is_ok()) {
        return unknown();
    }
    let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.or_else(|error| {
        if error.code() == RPC_E_CHANGED_MODE {
            unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
        } else {
            Err(error)
        }
    });
    if initialized.is_err() {
        return unknown();
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    let _apartment = Apartment;

    let profile = match NetworkInformation::GetInternetConnectionProfile() {
        Ok(p) => p,
        Err(_) => {
            return PlatformConnectionCost {
                kind: PlatformConnectionCostKind::Unknown,
                roaming: false,
                background_restricted: false,
            };
        }
    };

    let cost = match profile.GetConnectionCost() {
        Ok(c) => c,
        Err(_) => {
            return PlatformConnectionCost {
                kind: PlatformConnectionCostKind::Unknown,
                roaming: false,
                background_restricted: false,
            };
        }
    };

    let cost_type = match cost.NetworkCostType() {
        Ok(ct) => ct,
        Err(_) => {
            return PlatformConnectionCost {
                kind: PlatformConnectionCostKind::Unknown,
                roaming: false,
                background_restricted: false,
            };
        }
    };

    let kind = match cost_type {
        NetworkCostType::Unrestricted => PlatformConnectionCostKind::Unrestricted,
        NetworkCostType::Fixed => PlatformConnectionCostKind::Fixed,
        NetworkCostType::Variable => PlatformConnectionCostKind::Variable,
        _ => PlatformConnectionCostKind::Unknown,
    };

    let roaming = match cost.Roaming() {
        Ok(r) => r,
        Err(_) => {
            return PlatformConnectionCost {
                kind: PlatformConnectionCostKind::Unknown,
                roaming: false,
                background_restricted: false,
            };
        }
    };
    let background_restricted = match cost.BackgroundDataUsageRestricted() {
        Ok(b) => b,
        Err(_) => {
            return PlatformConnectionCost {
                kind: PlatformConnectionCostKind::Unknown,
                roaming: false,
                background_restricted: false,
            };
        }
    };

    PlatformConnectionCost {
        kind,
        roaming,
        background_restricted,
    }
}

#[cfg(not(windows))]
pub fn connection_cost() -> PlatformConnectionCost {
    PlatformConnectionCost {
        kind: PlatformConnectionCostKind::Unknown,
        roaming: false,
        background_restricted: false,
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn cost_probe_survives_successive_and_concurrent_worker_exits() {
        let probe_twice = || {
            let first = super::connection_cost();
            let second = super::connection_cost();
            eprintln!("native connection costs: {first:?}; {second:?}");
        };
        for _ in 0..3 {
            std::thread::spawn(probe_twice).join().unwrap();
        }
        let workers: Vec<_> = (0..4).map(|_| std::thread::spawn(probe_twice)).collect();
        for worker in workers {
            worker.join().unwrap();
        }
        probe_twice();
    }
}
