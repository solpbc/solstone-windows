// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable local custody for files chosen for import.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::import_client::{ImportFile, SavedImport, MAX_IMPORT_FILE_BYTES, REQUEST_BODY_LIMIT};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedProgress {
    pub journal: String,
    pub saved: SavedImport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refusal {
    pub journal: String,
    pub version: String,
    pub sentence: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub added: u64,
    pub saved: Option<SavedProgress>,
    pub refusal: Option<Refusal>,
    #[serde(default)]
    pub may_have_reached: bool,
    #[serde(default)]
    pub missing_copy: bool,
}

impl Item {
    pub fn refused_here(&self, journal: &str, version: &str) -> bool {
        self.refusal
            .as_ref()
            .is_some_and(|r| r.journal == journal && r.version == version)
    }

    pub fn progress_for(&self, journal: &str) -> Option<&SavedImport> {
        self.saved
            .as_ref()
            .filter(|p| p.journal == journal)
            .map(|p| &p.saved)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct IntakeRefusal {
    pub name: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Intake {
    pub copying: bool,
    pub done: usize,
    pub total: usize,
    pub accepted: usize,
    pub refused: Vec<IntakeRefusal>,
}

pub struct Store {
    root: PathBuf,
    pub items: BTreeMap<String, Item>,
    pub active: Option<String>,
    pub confirming_remove: Option<String>,
    pub intake: Option<Intake>,
    pub interrupted: bool,
    pub delivered_to: Option<String>,
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn new_id() -> io::Result<String> {
    let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
    Ok(spl_core::ca::sha256_hex(&key.public_key_der()))
}

fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

impl Store {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut items = BTreeMap::new();
        let mut interrupted = root.join("interrupted.json").exists();
        let mut partials = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".staging-") && valid_id(&name[9..]) {
                interrupted = true;
                partials.push(entry.path());
                continue;
            }
            if !valid_id(&name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("item.json");
            let bytes = fs::read(path)?;
            let item: Item = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if item.id != name {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "import item identity differs",
                ));
            }
            items.insert(name, item);
        }
        let delivered_to = match fs::read(root.join("recent.json")) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(io::Error::other)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        // Validate every published record before removing an unpublished copy.
        // An open failure leaves all material in place. Cleanup trouble is an
        // interrupted intake, with its durable notice, rather than an open failure.
        if !partials.is_empty() {
            atomic_json(&root.join("interrupted.json"), &true)?;
            for partial in partials {
                if let Err(error) = fs::remove_dir_all(partial) {
                    tracing::warn!(target: "import", %error, "partial import copy retained");
                }
            }
        }
        Ok(Self {
            root,
            items,
            active: None,
            confirming_remove: None,
            intake: None,
            interrupted,
            delivered_to,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn payload(&self, id: &str) -> PathBuf {
        self.root.join(id).join("payload")
    }

    pub fn put(&mut self, item: Item) -> io::Result<()> {
        if !valid_id(&item.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid import item",
            ));
        }
        atomic_json(&self.root.join(&item.id).join("item.json"), &item)?;
        self.items.insert(item.id.clone(), item);
        Ok(())
    }

    pub fn prepare_remove(&mut self, id: &str) -> Option<Item> {
        if self.active.as_deref() == Some(id) {
            return None;
        }
        let item = self.items.get(id)?.clone();
        self.confirming_remove = Some(id.to_string());
        Some(item)
    }

    pub fn cancel_remove(&mut self, id: Option<&str>) {
        if id.is_none() || self.confirming_remove.as_deref() == id {
            self.confirming_remove = None;
        }
    }

    pub fn remove(&mut self, id: &str) -> io::Result<bool> {
        if self.active.as_deref() == Some(id)
            || self.confirming_remove.as_deref() != Some(id)
            || !self.items.contains_key(id)
        {
            return Ok(false);
        }
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.items.remove(id);
        self.confirming_remove = None;
        Ok(true)
    }

    /// Called only after the journal proved terminal custody. A failed cleanup
    /// keeps the same id and progress so replay can finish it on a later pass.
    pub fn delivered(&mut self, id: &str, journal: &str) -> io::Result<()> {
        if !self.items.contains_key(id) {
            return Ok(());
        }
        atomic_json(&self.root.join("recent.json"), &journal)?;
        self.delivered_to = Some(journal.to_string());
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.items.remove(id);
        Ok(())
    }

    pub fn dismiss_interrupted(&mut self) -> io::Result<()> {
        match fs::remove_file(self.root.join("interrupted.json")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.interrupted = false;
        Ok(())
    }

    pub fn retry(&mut self, id: &str) -> io::Result<()> {
        if self.active.as_deref() == Some(id) {
            return Ok(());
        }
        if let Some(mut item) = self.items.get(id).cloned() {
            if item.refusal.as_ref().is_some_and(|r| r.retryable) {
                item.refusal = None;
                self.put(item)?;
            }
        }
        Ok(())
    }
}

/// Copy once at the owner's gesture. A staging directory is never deliverable;
/// only the final rename publishes both the complete bytes and their identity.
pub fn stage(root: &Path, source: &Path, id: String, now: u64) -> Result<Item, IntakeRefusal> {
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let refuse = |reason| IntakeRefusal {
        name: name.clone(),
        reason,
    };
    if !valid_id(&id) {
        return Err(refuse("this couldn't be read. add it again"));
    }
    let mime =
        mime_for(source).ok_or_else(|| refuse("your journal can't take this kind of file"))?;
    let mut input =
        File::open(source).map_err(|_| refuse("this couldn't be read. add it again"))?;
    let info = input
        .metadata()
        .map_err(|_| refuse("this couldn't be read. add it again"))?;
    if !info.is_file() {
        return Err(refuse("your journal can't take this kind of file"));
    }
    let size = info.len();
    let item = Item {
        id,
        name: name.clone(),
        mime: mime.into(),
        size,
        added: now,
        saved: None,
        refusal: None,
        may_have_reached: false,
        missing_copy: false,
    };
    let framing = ImportFile {
        id: &item.id,
        name: &item.name,
        content_type: &item.mime,
        path: source,
    };
    if size > MAX_IMPORT_FILE_BYTES || framing.request_length(size) > REQUEST_BODY_LIMIT {
        return Err(refuse("it's larger than your journal takes"));
    }
    let staging = root.join(format!(".staging-{}", item.id));
    let result = (|| -> io::Result<()> {
        fs::create_dir(&staging)?;
        atomic_json(&staging.join("item.json"), &item)?;
        let mut output = File::create(staging.join("payload"))?;
        let copied = io::copy(&mut Read::by_ref(&mut input).take(size), &mut output)?;
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "import source changed",
            ));
        }
        output.sync_all()?;
        drop(output);
        drop(input);
        fs::rename(&staging, root.join(&item.id))
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(staging);
        return Err(refuse(if error.kind() == io::ErrorKind::StorageFull {
            "there isn't enough room on this device"
        } else {
            "this couldn't be read. add it again"
        }));
    }
    Ok(item)
}

/// The phones' four kinds. Video containers are not inferred as audio.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "pdf" => "application/pdf",
            "txt" | "md" | "markdown" | "log" => "text/plain",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            "aac" => "audio/aac",
            "ogg" | "oga" => "audio/ogg",
            "opus" => "audio/opus",
            "flac" => "audio/flac",
            "wav" => "audio/wav",
            "amr" => "audio/amr",
            "aif" | "aiff" => "audio/aiff",
            _ => return None,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionGate {
    Unknown,
    Old,
    Supported,
}

pub fn version_gate(version: Option<&str>, fresh: bool) -> VersionGate {
    if !fresh {
        return VersionGate::Unknown;
    }
    let Some(version) = version else {
        return VersionGate::Unknown;
    };
    let version = version.trim_start_matches('v');
    let parts: Vec<_> = version.split('.').map(str::parse::<u64>).collect();
    match parts.as_slice() {
        [Ok(major), Ok(minor), Ok(patch)] if (*major, *minor, *patch) >= (2, 0, 52) => {
            VersionGate::Supported
        }
        [Ok(_), Ok(_), Ok(_)] => VersionGate::Old,
        _ => VersionGate::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_supported_version_opens_delivery() {
        assert_eq!(version_gate(Some("2.0.51"), true), VersionGate::Old);
        assert_eq!(version_gate(Some("2.0.52"), true), VersionGate::Supported);
        assert_eq!(version_gate(Some("2.0.75"), false), VersionGate::Unknown);
        assert_eq!(version_gate(Some("2.0.75x"), true), VersionGate::Unknown);
        assert_eq!(version_gate(None, true), VersionGate::Unknown);
    }

    #[test]
    fn saved_progress_and_refusal_are_not_a_pairing_filter_on_held_material() {
        let item = Item {
            id: "a".repeat(64),
            name: "file.txt".into(),
            mime: "text/plain".into(),
            size: 7,
            added: 1,
            saved: Some(SavedProgress {
                journal: "old".into(),
                saved: SavedImport {
                    path: "p".into(),
                    timestamp: "t".into(),
                },
            }),
            refusal: Some(Refusal {
                journal: "old".into(),
                version: "2.0.52".into(),
                sentence: "refusal".into(),
                retryable: false,
            }),
            may_have_reached: true,
            missing_copy: false,
        };
        assert!(item.progress_for("new").is_none());
        assert!(!item.refused_here("new", "2.0.52"));
        assert!(!item.refused_here("old", "2.0.53"));
        assert!(item.refused_here("old", "2.0.52"));
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn copying_and_reopening_preserve_bytes_and_saved_progress() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("note.txt");
        fs::write(&source, b"chosen bytes").unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let mut item = stage(store.root(), &source, new_id().unwrap(), 1).unwrap();
        fs::write(&source, b"later edit").unwrap();
        assert_eq!(fs::read(store.payload(&item.id)).unwrap(), b"chosen bytes");
        item.saved = Some(SavedProgress {
            journal: "one".into(),
            saved: SavedImport {
                path: "saved".into(),
                timestamp: "time".into(),
            },
        });
        item.may_have_reached = true;
        store.put(item.clone()).unwrap();
        let mut reopened = Store::open(store.root().into()).unwrap();
        assert_eq!(
            reopened.items[&item.id].progress_for("one"),
            item.progress_for("one")
        );
        reopened.active = Some(item.id.clone());
        assert!(!reopened.remove(&item.id).unwrap());
        reopened.delivered(&item.id, "one").unwrap();
        assert!(!reopened.payload(&item.id).exists());
        assert_eq!(
            Store::open(reopened.root().into())
                .unwrap()
                .delivered_to
                .as_deref(),
            Some("one")
        );
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn confirmation_reserves_a_copy_and_partial_intake_recovers_once() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("note.txt");
        fs::write(&source, b"held").unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let item = stage(store.root(), &source, new_id().unwrap(), 1).unwrap();
        store.put(item.clone()).unwrap();
        assert!(!store.remove(&item.id).unwrap());
        store.prepare_remove(&item.id).unwrap();
        assert_eq!(store.confirming_remove.as_deref(), Some(item.id.as_str()));
        store.cancel_remove(Some("another id"));
        assert!(store.confirming_remove.is_some());
        store.cancel_remove(Some(&item.id));
        assert!(!store.remove(&item.id).unwrap());
        store.active = Some(item.id.clone());
        assert!(store.prepare_remove(&item.id).is_none());
        store.active = None;
        store.prepare_remove(&item.id).unwrap();
        assert!(store.remove(&item.id).unwrap());
        // The owner may already have removed the whole directory externally.
        // Confirmation still clears its in-memory row without recreating it.
        let gone = stage(store.root(), &source, new_id().unwrap(), 2).unwrap();
        store.put(gone.clone()).unwrap();
        fs::remove_dir_all(store.root().join(&gone.id)).unwrap();
        store.prepare_remove(&gone.id).unwrap();
        assert!(store.remove(&gone.id).unwrap());
        assert!(!store.items.contains_key(&gone.id));
        let delivered = stage(store.root(), &source, new_id().unwrap(), 3).unwrap();
        store.put(delivered.clone()).unwrap();
        fs::remove_dir_all(store.root().join(&delivered.id)).unwrap();
        store.delivered(&delivered.id, "one").unwrap();
        assert!(!store.items.contains_key(&delivered.id));
        let partial = store.root().join(format!(".staging-{}", new_id().unwrap()));
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("payload"), b"unfinished").unwrap();
        let mut recovered = Store::open(store.root().into()).unwrap();
        assert!(recovered.interrupted);
        assert!(!partial.exists());
        assert!(Store::open(store.root().into()).unwrap().interrupted);
        recovered.dismiss_interrupted().unwrap();
        assert!(!Store::open(store.root().into()).unwrap().interrupted);
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn failed_open_never_cleans_up_material() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("a".repeat(64));
        fs::create_dir(&ready).unwrap();
        fs::write(ready.join("item.json"), b"unreadable metadata").unwrap();
        let partial = root.path().join(format!(".staging-{}", "b".repeat(64)));
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("payload"), b"partial").unwrap();
        assert!(Store::open(root.path().into()).is_err());
        assert_eq!(fs::read(partial.join("payload")).unwrap(), b"partial");
    }
}
