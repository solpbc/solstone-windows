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
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    // Balance both successful HRESULTs. An existing STA apartment remains usable,
    // but RPC_E_CHANGED_MODE did not acquire an initialization to release.
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    let _apartment = Apartment(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());

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
