// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The source list belongs to one confirmed pairing, including on offline starts.
use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use crate::credential::{owner_state_write_guard, publish_staged_file, PairedState};
use crate::TransportError;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    pub cid: String,
    pub display_label: String,
    pub device_type: Option<String>,
    pub platform: Option<String>,
    pub device_last_seen_at: Option<String>,
    pub sources: Vec<Source>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    pub source: String,
    pub detail: Option<String>,
    pub state: Option<String>,
    pub origin: String,
    pub declared_at: Option<String>,
    pub last_synced_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub sources_rev: String,
    pub fetched_at: u64,
}

impl Snapshot {
    pub fn from_reply(body: &[u8], fetched_at: u64) -> Option<Self> {
        #[derive(Deserialize)]
        struct Reply {
            self_cid: String,
            sources_rev: String,
            devices: Vec<Device>,
        }
        let mut reply: Reply = serde_json::from_slice(body).ok()?;
        if reply.self_cid.is_empty() || reply.sources_rev.is_empty() {
            return None;
        }
        reply.devices.retain(|device| device.cid != reply.self_cid);
        Some(Self {
            devices: reply.devices,
            sources_rev: reply.sources_rev,
            fetched_at,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Cached {
    Known(Snapshot),
    Revoked,
}

#[derive(Deserialize, Serialize)]
struct Stored {
    binding: String,
    value: Cached,
}

pub fn read(path: &Path, binding: &str) -> Option<Cached> {
    let data = fs::read(path.with_extension("sources.json")).ok()?;
    let stored: Stored = serde_json::from_slice(&data).ok()?;
    (stored.binding == binding).then_some(stored.value)
}

/// Called under the credential writer's lock on retirement or replacement,
/// including a pairing committed by the CLI while the app is stopped.
pub(crate) fn forget(path: &Path) -> io::Result<()> {
    for extension in ["sources.tmp", "sources.json"] {
        match fs::remove_file(path.with_extension(extension)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            result => result?,
        }
    }
    Ok(())
}

pub fn store(path: &Path, binding: &str, value: Option<Cached>) -> Result<bool, TransportError> {
    let _guard = owner_state_write_guard();
    let Some(state) = PairedState::read_setup_metadata(path)? else {
        return Ok(false);
    };
    let Some(cred) = state.credential else {
        return Ok(false);
    };
    if state.retirement_intent.is_some()
        || crate::ack::JournalIdentity::from_credential(&cred).client_cert_sha256 != binding
    {
        return Ok(false);
    }
    if !matches!(value, Some(Cached::Revoked))
        && matches!(read(path, binding), Some(Cached::Revoked))
    {
        return Ok(false);
    }
    if let Some(value) = value {
        let staged = path.with_extension("sources.tmp");
        let bytes = serde_json::to_vec(&Stored {
            binding: binding.into(),
            value,
        })?;
        if let Err(error) = fs::write(&staged, bytes) {
            let _ = fs::remove_file(&staged);
            return Err(error.into());
        }
        let result = publish_staged_file(&staged, &path.with_extension("sources.json"));
        if result.is_err() {
            let _ = fs::remove_file(staged);
        }
        result?;
    } else {
        forget(path)?;
    }
    Ok(true)
}
