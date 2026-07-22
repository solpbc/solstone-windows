// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Byte-exact archive staging manifest for transparency publication.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagingManifestV1 {
    pub bytes: Vec<u8>,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransparencyStageError {
    RootUnavailable,
    LinkRejected,
    SpecialFileRejected,
    InvalidRelativePath,
    FileUnavailable,
    ByteCountOverflow,
    RetryRecordMismatch,
}

impl fmt::Display for TransparencyStageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RootUnavailable => "transparency archive root is unavailable",
            Self::LinkRejected => "transparency archive contains a symbolic link",
            Self::SpecialFileRejected => "transparency archive contains a non-regular file",
            Self::InvalidRelativePath => {
                "transparency archive contains a non-ASCII or control-character path"
            }
            Self::FileUnavailable => "transparency archive file bytes are unavailable",
            Self::ByteCountOverflow => "transparency archive file size is out of range",
            Self::RetryRecordMismatch => {
                "transparency staged bytes differ from the persisted retry record"
            }
        })
    }
}

impl std::error::Error for TransparencyStageError {}

pub fn render_staging_manifest_v1(
    archive_root: &Path,
) -> Result<StagingManifestV1, TransparencyStageError> {
    let metadata =
        fs::symlink_metadata(archive_root).map_err(|_| TransparencyStageError::RootUnavailable)?;
    if metadata.file_type().is_symlink() {
        return Err(TransparencyStageError::LinkRejected);
    }
    if !metadata.is_dir() {
        return Err(TransparencyStageError::RootUnavailable);
    }

    let mut files = Vec::new();
    enumerate_regular_files(archive_root, archive_root, &mut files)?;
    files.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    let mut rendered = Vec::new();
    for (relative, path) in files {
        let bytes = fs::read(&path).map_err(|_| TransparencyStageError::FileUnavailable)?;
        let byte_count =
            u64::try_from(bytes.len()).map_err(|_| TransparencyStageError::ByteCountOverflow)?;
        let sha256 = hex_lower(&Sha256::digest(&bytes));
        rendered.extend_from_slice(b"sha256=");
        rendered.extend_from_slice(sha256.as_bytes());
        rendered.extend_from_slice(b"\tbytes=");
        rendered.extend_from_slice(byte_count.to_string().as_bytes());
        rendered.extend_from_slice(b"\tpath=");
        rendered.extend_from_slice(relative.as_bytes());
        rendered.push(b'\n');
    }
    let sha256 = hex_lower(&Sha256::digest(&rendered));
    Ok(StagingManifestV1 {
        bytes: rendered,
        sha256,
    })
}

pub fn verify_staging_manifest_v1(
    archive_root: &Path,
    retry_record: &[u8],
) -> Result<StagingManifestV1, TransparencyStageError> {
    let observed = render_staging_manifest_v1(archive_root)?;
    if observed.bytes != retry_record {
        return Err(TransparencyStageError::RetryRecordMismatch);
    }
    Ok(observed)
}

fn enumerate_regular_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), TransparencyStageError> {
    let entries = fs::read_dir(directory).map_err(|_| TransparencyStageError::RootUnavailable)?;
    for entry in entries {
        let entry = entry.map_err(|_| TransparencyStageError::RootUnavailable)?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| TransparencyStageError::FileUnavailable)?;
        if metadata.file_type().is_symlink() {
            return Err(TransparencyStageError::LinkRejected);
        }
        if metadata.is_dir() {
            validate_relative(root, &path)?;
            enumerate_regular_files(root, &path, output)?;
        } else if metadata.is_file() {
            output.push((validate_relative(root, &path)?, path));
        } else {
            return Err(TransparencyStageError::SpecialFileRejected);
        }
    }
    Ok(())
}

fn validate_relative(root: &Path, path: &Path) -> Result<String, TransparencyStageError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| TransparencyStageError::InvalidRelativePath)?;
    let mut components = Vec::new();
    for component in relative.components() {
        let text = component
            .as_os_str()
            .to_str()
            .ok_or(TransparencyStageError::InvalidRelativePath)?;
        if text.is_empty()
            || !text.is_ascii()
            || text.bytes().any(|byte| byte.is_ascii_control())
            || matches!(text, "." | "..")
        {
            return Err(TransparencyStageError::InvalidRelativePath);
        }
        components.push(text);
    }
    if components.is_empty() {
        return Err(TransparencyStageError::InvalidRelativePath);
    }
    Ok(components.join("/"))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
