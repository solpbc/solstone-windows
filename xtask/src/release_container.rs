// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Read-only executable extraction and cross-container release evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Cursor, Read};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::artifact_fs::{check_case_collision, validate_relative_path};
use crate::rust_release_manifest::PackagedExecutableEvidence;

const NUPKG_EXECUTABLE: &str = "lib/app/solstone-windows-app.exe";
const PORTABLE_EXECUTABLE: &str = "current/solstone-windows-app.exe";
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;
const CENTRAL_HEADER_LEN: usize = 46;
const UNIX_TYPE_MASK: u32 = 0o170_000;
const UNIX_REGULAR: u32 = 0o100_000;
const UNIX_DIRECTORY: u32 = 0o040_000;
const UNIX_SPECIAL_PERMISSIONS: u32 = 0o007_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerKind {
    Nupkg,
    Portable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaselineSource {
    Nupkg,
    Portable,
    Staged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseContainerError {
    InvalidArchive { container: ContainerKind },
    InvalidEntryName { container: ContainerKind },
    DuplicateEntryName { container: ContainerKind },
    EntryCaseCollision { container: ContainerKind },
    EncryptedEntry { container: ContainerKind },
    UnsafeUnixMode { container: ContainerKind },
    CanonicalMemberIsDirectory { container: ContainerKind },
    MissingCanonicalMember { container: ContainerKind },
    DuplicateCanonicalMember { container: ContainerKind },
    EmptyCanonicalMember { container: ContainerKind },
    CanonicalMemberSizeMismatch { container: ContainerKind },
    CanonicalMemberReadFailed { container: ContainerKind },
    BaselineDiverged { source: BaselineSource },
    BaselineHasNoAgreement,
}

impl fmt::Display for ReleaseContainerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArchive { container } => write!(
                formatter,
                "{} is not a valid supported ZIP archive; rebuild that container in this transaction",
                container.label()
            ),
            Self::InvalidEntryName { container } => write!(
                formatter,
                "{} contains an unsafe or non-portable entry name; rebuild it with portable relative entry names",
                container.label()
            ),
            Self::DuplicateEntryName { container } => write!(
                formatter,
                "{} contains a duplicate entry name; rebuild it with one unique name per entry",
                container.label()
            ),
            Self::EntryCaseCollision { container } => write!(
                formatter,
                "{} contains ASCII case-folding entry collisions; rebuild it with case-unique entry names",
                container.label()
            ),
            Self::EncryptedEntry { container } => write!(
                formatter,
                "{} contains an encrypted entry; rebuild it without archive encryption",
                container.label()
            ),
            Self::UnsafeUnixMode { container } => write!(
                formatter,
                "{} contains a link, special file, or unsafe Unix permission mode; rebuild it with regular files and directories",
                container.label()
            ),
            Self::CanonicalMemberIsDirectory { container } => write!(
                formatter,
                "{} stores the canonical app member as a directory; rebuild it with the executable as one regular file",
                container.label()
            ),
            Self::MissingCanonicalMember { container } => write!(
                formatter,
                "{} is missing the exact canonical app member; rebuild that container in this transaction",
                container.label()
            ),
            Self::DuplicateCanonicalMember { container } => write!(
                formatter,
                "{} contains the canonical app member more than once; rebuild it with exactly one executable member",
                container.label()
            ),
            Self::EmptyCanonicalMember { container } => write!(
                formatter,
                "{} contains an empty canonical app member; rebuild that container in this transaction",
                container.label()
            ),
            Self::CanonicalMemberSizeMismatch { container } => write!(
                formatter,
                "{} canonical app member size disagrees with its streamed bytes; rebuild that container in this transaction",
                container.label()
            ),
            Self::CanonicalMemberReadFailed { container } => write!(
                formatter,
                "{} canonical app member could not be streamed and verified; rebuild that container in this transaction",
                container.label()
            ),
            Self::BaselineDiverged { source } => write!(
                formatter,
                "{} executable diverged from the other two release sources; rebuild both containers in this transaction",
                source.label()
            ),
            Self::BaselineHasNoAgreement => write!(
                formatter,
                "nupkg, portable, and staged executables all disagree; rebuild both containers in this transaction"
            ),
        }
    }
}

impl std::error::Error for ReleaseContainerError {}

impl ContainerKind {
    fn label(self) -> &'static str {
        match self {
            Self::Nupkg => "full nupkg",
            Self::Portable => "portable ZIP",
        }
    }

    fn canonical_member(self) -> &'static str {
        match self {
            Self::Nupkg => NUPKG_EXECUTABLE,
            Self::Portable => PORTABLE_EXECUTABLE,
        }
    }
}

impl BaselineSource {
    fn label(self) -> &'static str {
        match self {
            Self::Nupkg => "nupkg",
            Self::Portable => "portable",
            Self::Staged => "staged",
        }
    }
}

pub struct ExecutableContainerReader;

impl ExecutableContainerReader {
    pub fn read_nupkg(
        archive_bytes: &[u8],
    ) -> Result<PackagedExecutableEvidence, ReleaseContainerError> {
        Self::read(archive_bytes, ContainerKind::Nupkg)
    }

    pub fn read_portable(
        archive_bytes: &[u8],
    ) -> Result<PackagedExecutableEvidence, ReleaseContainerError> {
        Self::read(archive_bytes, ContainerKind::Portable)
    }

    fn read(
        archive_bytes: &[u8],
        container: ContainerKind,
    ) -> Result<PackagedExecutableEvidence, ReleaseContainerError> {
        let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
            .map_err(|_| ReleaseContainerError::InvalidArchive { container })?;
        let names =
            central_directory_names(archive_bytes, archive.central_directory_start(), container)?;
        let canonical = container.canonical_member();
        let canonical_count = names
            .iter()
            .filter(|name| name.as_str() == canonical)
            .count();
        if canonical_count > 1 {
            return Err(ReleaseContainerError::DuplicateCanonicalMember { container });
        }

        validate_entry_names(&names, container)?;
        if names.len() != archive.len() {
            return Err(ReleaseContainerError::InvalidArchive { container });
        }

        let mut target_index = None;
        let mut target_is_directory = false;
        for (index, central_name) in names.iter().enumerate() {
            let entry = archive
                .by_index_raw(index)
                .map_err(|_| ReleaseContainerError::InvalidArchive { container })?;
            if entry.name() != central_name {
                return Err(ReleaseContainerError::InvalidArchive { container });
            }
            if entry.encrypted() {
                return Err(ReleaseContainerError::EncryptedEntry { container });
            }

            let mode = entry.unix_mode();
            let directory_by_mode =
                mode.is_some_and(|mode| mode & UNIX_TYPE_MASK == UNIX_DIRECTORY);
            let is_directory = entry.is_dir() || directory_by_mode;
            let is_target_directory =
                central_name.trim_end_matches('/') == canonical && is_directory;
            if is_target_directory {
                target_is_directory = true;
            }
            if unsafe_unix_mode(mode, entry.is_dir()) {
                if is_target_directory {
                    continue;
                }
                return Err(ReleaseContainerError::UnsafeUnixMode { container });
            }
            if central_name == canonical {
                target_index = Some(index);
            }
        }

        if target_is_directory {
            return Err(ReleaseContainerError::CanonicalMemberIsDirectory { container });
        }
        let target_index =
            target_index.ok_or(ReleaseContainerError::MissingCanonicalMember { container })?;
        let mut member = archive
            .by_index(target_index)
            .map_err(|_| ReleaseContainerError::CanonicalMemberReadFailed { container })?;
        let declared_size = member.size();
        if declared_size == 0 {
            return Err(ReleaseContainerError::EmptyCanonicalMember { container });
        }

        let mut hasher = Sha256::new();
        let mut actual_size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = member
                .read(&mut buffer)
                .map_err(|_| ReleaseContainerError::CanonicalMemberReadFailed { container })?;
            if read == 0 {
                break;
            }
            actual_size = actual_size
                .checked_add(u64::try_from(read).map_err(|_| {
                    ReleaseContainerError::CanonicalMemberSizeMismatch { container }
                })?)
                .ok_or(ReleaseContainerError::CanonicalMemberSizeMismatch { container })?;
            hasher.update(&buffer[..read]);
        }
        if actual_size != declared_size {
            return Err(ReleaseContainerError::CanonicalMemberSizeMismatch { container });
        }

        Ok(PackagedExecutableEvidence {
            sha256: format!("{:x}", hasher.finalize()),
            bytes: actual_size,
        })
    }
}

pub fn compare_executable_baseline(
    nupkg: &PackagedExecutableEvidence,
    portable: &PackagedExecutableEvidence,
    staged: &PackagedExecutableEvidence,
) -> Result<PackagedExecutableEvidence, ReleaseContainerError> {
    if nupkg == portable && portable == staged {
        return Ok(nupkg.clone());
    }
    if portable == staged {
        return Err(ReleaseContainerError::BaselineDiverged {
            source: BaselineSource::Nupkg,
        });
    }
    if nupkg == staged {
        return Err(ReleaseContainerError::BaselineDiverged {
            source: BaselineSource::Portable,
        });
    }
    if nupkg == portable {
        return Err(ReleaseContainerError::BaselineDiverged {
            source: BaselineSource::Staged,
        });
    }
    Err(ReleaseContainerError::BaselineHasNoAgreement)
}

fn central_directory_names(
    archive_bytes: &[u8],
    start: u64,
    container: ContainerKind,
) -> Result<Vec<String>, ReleaseContainerError> {
    let mut position =
        usize::try_from(start).map_err(|_| ReleaseContainerError::InvalidArchive { container })?;
    let mut names = Vec::new();
    while read_u32(archive_bytes, position) == Some(CENTRAL_HEADER_SIGNATURE) {
        let header_end = position
            .checked_add(CENTRAL_HEADER_LEN)
            .ok_or(ReleaseContainerError::InvalidArchive { container })?;
        if header_end > archive_bytes.len() {
            return Err(ReleaseContainerError::InvalidArchive { container });
        }
        let name_len = usize::from(
            read_u16(archive_bytes, position + 28)
                .ok_or(ReleaseContainerError::InvalidArchive { container })?,
        );
        let extra_len = usize::from(
            read_u16(archive_bytes, position + 30)
                .ok_or(ReleaseContainerError::InvalidArchive { container })?,
        );
        let comment_len = usize::from(
            read_u16(archive_bytes, position + 32)
                .ok_or(ReleaseContainerError::InvalidArchive { container })?,
        );
        let name_end = header_end
            .checked_add(name_len)
            .ok_or(ReleaseContainerError::InvalidArchive { container })?;
        let entry_end = name_end
            .checked_add(extra_len)
            .and_then(|end| end.checked_add(comment_len))
            .ok_or(ReleaseContainerError::InvalidArchive { container })?;
        let raw_name = archive_bytes
            .get(header_end..name_end)
            .ok_or(ReleaseContainerError::InvalidArchive { container })?;
        let name = std::str::from_utf8(raw_name)
            .map_err(|_| ReleaseContainerError::InvalidEntryName { container })?;
        names.push(name.to_owned());
        if entry_end > archive_bytes.len() {
            return Err(ReleaseContainerError::InvalidArchive { container });
        }
        position = entry_end;
    }
    Ok(names)
}

fn validate_entry_names(
    names: &[String],
    container: ContainerKind,
) -> Result<(), ReleaseContainerError> {
    let mut exact = BTreeSet::new();
    let mut folded = BTreeMap::new();
    for name in names {
        let spelling = if let Some(directory) = name.strip_suffix('/') {
            if directory.is_empty() || directory.ends_with('/') {
                return Err(ReleaseContainerError::InvalidEntryName { container });
            }
            directory
        } else {
            name.as_str()
        };
        validate_relative_path(spelling)
            .map_err(|_| ReleaseContainerError::InvalidEntryName { container })?;
        if !exact.insert(name.as_str()) {
            return Err(ReleaseContainerError::DuplicateEntryName { container });
        }
        check_case_collision(&mut folded, name)
            .map_err(|_| ReleaseContainerError::EntryCaseCollision { container })?;
    }
    Ok(())
}

fn unsafe_unix_mode(mode: Option<u32>, name_is_directory: bool) -> bool {
    let Some(mode) = mode else {
        return false;
    };
    let kind = mode & UNIX_TYPE_MASK;
    let kind_is_safe = matches!(kind, 0 | UNIX_REGULAR | UNIX_DIRECTORY);
    let kind_matches_name = if name_is_directory {
        matches!(kind, 0 | UNIX_DIRECTORY)
    } else {
        matches!(kind, 0 | UNIX_REGULAR)
    };
    !kind_is_safe || !kind_matches_name || mode & UNIX_SPECIAL_PERMISSIONS != 0
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(value))
}
