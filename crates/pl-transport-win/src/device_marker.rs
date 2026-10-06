// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Typed, privacy-preserving local device marker evaluation.

use serde::{Deserialize, Serialize};

const DIGEST_DOMAIN: &[u8] = b"solstone.windows.owner-device-marker.v1\0publisher:solpbc\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerSource {
    PublisherSystemId,
    RegistryMachineGuid,
}

impl MarkerSource {
    fn wire(self) -> &'static [u8] {
        match self {
            Self::PublisherSystemId => b"system-identification-publisher",
            Self::RegistryMachineGuid => b"registry-machine-guid-fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerConfidence {
    Primary,
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceMarker {
    pub digest: String,
    pub source: MarkerSource,
    pub confidence: MarkerConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Available,
    Empty,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum MarkerResult {
    Available(DeviceMarker),
    Missing,
    ProbeFailure {
        system_identification: ProbeStatus,
        registry_fallback: ProbeStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerProbeError;

pub trait MarkerProvider {
    fn system_id_for_publisher(&self) -> Result<Option<Vec<u8>>, MarkerProbeError>;
    fn registry_fallback(&self) -> Result<Option<Vec<u8>>, MarkerProbeError>;
}

pub fn probe(provider: &impl MarkerProvider) -> MarkerResult {
    let system_status = match provider.system_id_for_publisher() {
        Ok(Some(value)) if !value.is_empty() => {
            return MarkerResult::Available(marker(value, MarkerSource::PublisherSystemId));
        }
        Ok(Some(_)) | Ok(None) => ProbeStatus::Empty,
        Err(MarkerProbeError) => ProbeStatus::Failed,
    };
    match provider.registry_fallback() {
        Ok(Some(value)) if !value.is_empty() => {
            MarkerResult::Available(marker(value, MarkerSource::RegistryMachineGuid))
        }
        Ok(Some(_)) | Ok(None) if system_status == ProbeStatus::Empty => MarkerResult::Missing,
        Ok(Some(_)) | Ok(None) => MarkerResult::ProbeFailure {
            system_identification: system_status,
            registry_fallback: ProbeStatus::Empty,
        },
        Err(MarkerProbeError) => MarkerResult::ProbeFailure {
            system_identification: system_status,
            registry_fallback: ProbeStatus::Failed,
        },
    }
}

fn marker(value: Vec<u8>, source: MarkerSource) -> DeviceMarker {
    let mut bytes = Vec::with_capacity(DIGEST_DOMAIN.len() + source.wire().len() + value.len() + 1);
    bytes.extend_from_slice(DIGEST_DOMAIN);
    bytes.extend_from_slice(source.wire());
    bytes.push(0);
    bytes.extend_from_slice(&value);
    let digest = spl_core::ca::sha256_hex(&bytes);
    DeviceMarker {
        digest,
        source,
        confidence: match source {
            MarkerSource::PublisherSystemId => MarkerConfidence::Primary,
            MarkerSource::RegistryMachineGuid => MarkerConfidence::Fallback,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerDecision {
    EstablishBaseline,
    UnchangedPrimary,
    SameFallback,
    Changed,
    Missing,
    ProbeFailure,
}

pub fn evaluate(baseline: Option<&DeviceMarker>, current: &MarkerResult) -> MarkerDecision {
    match (baseline, current) {
        (None, MarkerResult::Available(_)) => MarkerDecision::EstablishBaseline,
        (None, MarkerResult::Missing) => MarkerDecision::Missing,
        (None, MarkerResult::ProbeFailure { .. }) => MarkerDecision::ProbeFailure,
        (Some(_), MarkerResult::Missing) => MarkerDecision::Missing,
        (Some(_), MarkerResult::ProbeFailure { .. }) => MarkerDecision::ProbeFailure,
        (Some(old), MarkerResult::Available(new)) if old.digest != new.digest => {
            MarkerDecision::Changed
        }
        (
            Some(_),
            MarkerResult::Available(DeviceMarker {
                confidence: MarkerConfidence::Fallback,
                ..
            }),
        ) => MarkerDecision::SameFallback,
        (Some(_), MarkerResult::Available(_)) => MarkerDecision::UnchangedPrimary,
    }
}

#[cfg(windows)]
struct WindowsMarkerProvider;

#[cfg(windows)]
impl MarkerProvider for WindowsMarkerProvider {
    fn system_id_for_publisher(&self) -> Result<Option<Vec<u8>>, MarkerProbeError> {
        use windows::Security::Cryptography::CryptographicBuffer;
        use windows::System::Profile::SystemIdentification;

        // The activation factory is cached process-wide by the bindings, which
        // initialize the multithreaded apartment themselves when a thread has
        // none. Pairing a manual RoInitialize with RoUninitialize here tore
        // that apartment down behind the cached factory, so a second probe in
        // the same process on another thread faulted.
        let info = SystemIdentification::GetSystemIdForPublisher().map_err(|_| MarkerProbeError)?;
        if info.Source().map_err(|_| MarkerProbeError)?.0 == 0 {
            return Ok(None);
        }
        let buffer = info.Id().map_err(|_| MarkerProbeError)?;
        let mut bytes = windows::core::Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(&buffer, &mut bytes).map_err(|_| MarkerProbeError)?;
        Ok((!bytes.is_empty()).then(|| bytes.as_ref().to_vec()))
    }

    fn registry_fallback(&self) -> Result<Option<Vec<u8>>, MarkerProbeError> {
        use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_64KEY};
        use winreg::RegKey;

        let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = machine
            .open_subkey_with_flags(
                "SOFTWARE\\Microsoft\\Cryptography",
                KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            )
            .map_err(|_| MarkerProbeError)?;
        let marker: String = key.get_value("MachineGuid").map_err(|_| MarkerProbeError)?;
        Ok((!marker.is_empty()).then(|| marker.into_bytes()))
    }
}

/// Probe the Windows publisher identifier and use MachineGuid only when that
/// API cannot provide a nonempty identifier. Non-Windows builds return a
/// typed failure and never fabricate a marker.
pub fn probe_platform() -> MarkerResult {
    #[cfg(windows)]
    {
        probe(&WindowsMarkerProvider)
    }
    #[cfg(not(windows))]
    {
        MarkerResult::ProbeFailure {
            system_identification: ProbeStatus::Failed,
            registry_fallback: ProbeStatus::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        system: Result<Option<Vec<u8>>, MarkerProbeError>,
        registry: Result<Option<Vec<u8>>, MarkerProbeError>,
    }

    impl MarkerProvider for FakeProvider {
        fn system_id_for_publisher(&self) -> Result<Option<Vec<u8>>, MarkerProbeError> {
            self.system.clone()
        }

        fn registry_fallback(&self) -> Result<Option<Vec<u8>>, MarkerProbeError> {
            self.registry.clone()
        }
    }

    #[test]
    fn marker_probe_prefers_publisher_system_id_and_persists_only_digest() {
        let result = probe(&FakeProvider {
            system: Ok(Some(b"primary-marker".to_vec())),
            registry: Ok(Some(b"fallback-marker".to_vec())),
        });
        let MarkerResult::Available(marker) = result else {
            panic!("primary marker expected")
        };
        assert_eq!(marker.source, MarkerSource::PublisherSystemId);
        assert_eq!(marker.confidence, MarkerConfidence::Primary);
        assert!(!marker.digest.contains("primary-marker"));
        assert_eq!(
            evaluate(None, &MarkerResult::Available(marker.clone())),
            MarkerDecision::EstablishBaseline
        );
        assert_eq!(
            evaluate(Some(&marker), &MarkerResult::Available(marker.clone())),
            MarkerDecision::UnchangedPrimary
        );
    }

    #[test]
    fn fallback_is_weaker_and_marker_changes_fail_closed() {
        let result = probe(&FakeProvider {
            system: Err(MarkerProbeError),
            registry: Ok(Some(b"registry-id".to_vec())),
        });
        let MarkerResult::Available(fallback) = result else {
            panic!("fallback expected")
        };
        assert_eq!(fallback.source, MarkerSource::RegistryMachineGuid);
        assert_eq!(fallback.confidence, MarkerConfidence::Fallback);
        assert_eq!(
            evaluate(Some(&fallback), &MarkerResult::Available(fallback.clone())),
            MarkerDecision::SameFallback
        );
        let changed = MarkerResult::Available(marker(
            b"different".to_vec(),
            MarkerSource::PublisherSystemId,
        ));
        assert_eq!(evaluate(Some(&fallback), &changed), MarkerDecision::Changed);
        assert_eq!(
            evaluate(Some(&fallback), &MarkerResult::Missing),
            MarkerDecision::Missing
        );
        assert_eq!(
            evaluate(
                Some(&fallback),
                &MarkerResult::ProbeFailure {
                    system_identification: ProbeStatus::Failed,
                    registry_fallback: ProbeStatus::Failed
                }
            ),
            MarkerDecision::ProbeFailure
        );
    }
}
