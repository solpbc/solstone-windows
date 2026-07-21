// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Shared path and filesystem safety for offline artifact verifiers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsafePathReason {
    Absolute,
    Empty,
    EmptyComponent,
    ReservedName,
    TrailingDotOrSpace,
    NonPortableName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnixModePolicy {
    StrictNoExecute,
    AllowExecute,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ArtifactFsError {
    Io {
        path: String,
        message: String,
    },
    UnsafePath {
        path: String,
        reason: UnsafePathReason,
    },
    Traversal {
        path: String,
    },
    Backslash {
        path: String,
    },
    ControlChar {
        path: String,
    },
    CaseCollision {
        first: String,
        second: String,
    },
    NonRegularFile {
        path: String,
        kind: &'static str,
    },
    ReparsePoint {
        path: String,
    },
    UnsafeResolution {
        path: String,
    },
    InvalidFileMode {
        path: String,
        mode: u32,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct WalkInventory {
    pub files: BTreeSet<String>,
    pub directories: BTreeSet<String>,
}

/// Canonical containment authority for all release-artifact reads.
#[derive(Clone, Debug)]
pub struct ContainedRoot {
    path: PathBuf,
    canonical: PathBuf,
    label: String,
    mode_policy: UnixModePolicy,
}

/// A file proven contained before a read and re-verified after it.
#[derive(Clone, Debug)]
pub struct ResolvedFile {
    root: ContainedRoot,
    relative: String,
    label: String,
    canonical: PathBuf,
}

impl ContainedRoot {
    pub fn new(
        path: &Path,
        label: &str,
        mode_policy: UnixModePolicy,
    ) -> Result<Self, ArtifactFsError> {
        verify_root_components(path, label)?;
        let metadata = fs::symlink_metadata(path).map_err(|error| io_error(label, error))?;
        reject_reparse_point(label, &metadata)?;
        if metadata.file_type().is_symlink() {
            return Err(ArtifactFsError::UnsafeResolution {
                path: label.to_owned(),
            });
        }
        if !metadata.file_type().is_dir() {
            return Err(ArtifactFsError::NonRegularFile {
                path: label.to_owned(),
                kind: file_kind(&metadata),
            });
        }
        verify_mode(label, &metadata, true, mode_policy)?;
        let canonical = fs::canonicalize(path).map_err(|_| ArtifactFsError::UnsafeResolution {
            path: label.to_owned(),
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            canonical,
            label: label.to_owned(),
            mode_policy,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn resolve(&self, relative: &str, label: &str) -> Result<ResolvedFile, ArtifactFsError> {
        verify_root_components(&self.path, &self.label)?;
        let canonical_root =
            fs::canonicalize(&self.path).map_err(|_| ArtifactFsError::UnsafeResolution {
                path: label.to_owned(),
            })?;
        if canonical_root != self.canonical {
            return Err(ArtifactFsError::UnsafeResolution {
                path: label.to_owned(),
            });
        }
        validate_relative_path(relative).map_err(|_| ArtifactFsError::UnsafeResolution {
            path: label.to_owned(),
        })?;
        let leaf = self.path.join(relative);
        verify_leaf_components(&self.path, relative, label)?;
        let metadata = fs::symlink_metadata(&leaf).map_err(|error| io_error(label, error))?;
        reject_reparse_point(label, &metadata)?;
        if !metadata.file_type().is_file() {
            return Err(ArtifactFsError::NonRegularFile {
                path: label.to_owned(),
                kind: file_kind(&metadata),
            });
        }
        verify_mode(label, &metadata, false, self.mode_policy)?;
        let canonical = fs::canonicalize(&leaf).map_err(|_| ArtifactFsError::UnsafeResolution {
            path: label.to_owned(),
        })?;
        if canonical == self.canonical || !canonical.starts_with(&self.canonical) {
            return Err(ArtifactFsError::UnsafeResolution {
                path: label.to_owned(),
            });
        }
        Ok(ResolvedFile {
            root: self.clone(),
            relative: relative.to_owned(),
            label: label.to_owned(),
            canonical,
        })
    }

    pub fn read(&self, relative: &str, label: &str) -> Result<Vec<u8>, ArtifactFsError> {
        self.resolve(relative, label)?.read()
    }
}

impl ResolvedFile {
    pub fn read(self) -> Result<Vec<u8>, ArtifactFsError> {
        self.read_stable(|| {})
    }

    fn read_stable(self, after_first_read: impl FnOnce()) -> Result<Vec<u8>, ArtifactFsError> {
        let first = fs::read(&self.canonical).map_err(|error| io_error(&self.label, error))?;
        after_first_read();
        let after = self
            .root
            .resolve(&self.relative, &self.label)
            .map_err(|_| ArtifactFsError::UnsafeResolution {
                path: self.label.clone(),
            })?;
        let second = fs::read(&after.canonical).map_err(|_| ArtifactFsError::UnsafeResolution {
            path: self.label.clone(),
        })?;
        if first != second {
            return Err(ArtifactFsError::UnsafeResolution { path: self.label });
        }
        Ok(first)
    }
}

pub fn validate_relative_path(path: &str) -> Result<(), ArtifactFsError> {
    if path.is_empty() {
        return Err(ArtifactFsError::UnsafePath {
            path: path.to_owned(),
            reason: UnsafePathReason::Empty,
        });
    }
    if path.starts_with('/') || Path::new(path).is_absolute() {
        return Err(ArtifactFsError::UnsafePath {
            path: path.to_owned(),
            reason: UnsafePathReason::Absolute,
        });
    }
    if path.contains('\\') {
        return Err(ArtifactFsError::Backslash {
            path: path.to_owned(),
        });
    }
    if path.chars().any(char::is_control) {
        return Err(ArtifactFsError::ControlChar {
            path: path.to_owned(),
        });
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(ArtifactFsError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::EmptyComponent,
            });
        }
        if component == "." || component == ".." {
            return Err(ArtifactFsError::Traversal {
                path: path.to_owned(),
            });
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(ArtifactFsError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::TrailingDotOrSpace,
            });
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                });
        if reserved {
            return Err(ArtifactFsError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::ReservedName,
            });
        }
        if !component
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '{' | '}'))
        {
            return Err(ArtifactFsError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::NonPortableName,
            });
        }
    }
    Ok(())
}

pub fn file_kind(metadata: &fs::Metadata) -> &'static str {
    let kind = metadata.file_type();
    if kind.is_symlink() {
        "symlink"
    } else if kind.is_dir() {
        "directory"
    } else if kind.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

pub fn check_case_collision(
    folded: &mut BTreeMap<String, String>,
    path: &str,
) -> Result<(), ArtifactFsError> {
    if let Some(first) = folded.insert(path.to_ascii_lowercase(), path.to_owned()) {
        return Err(ArtifactFsError::CaseCollision {
            first,
            second: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(any(windows, test))]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

#[cfg(any(windows, test))]
fn reject_windows_reparse_attributes(label: &str, attributes: u32) -> Result<(), ArtifactFsError> {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ArtifactFsError::ReparsePoint {
            path: label.to_owned(),
        });
    }
    Ok(())
}

#[cfg(windows)]
pub fn reject_reparse_point(label: &str, metadata: &fs::Metadata) -> Result<(), ArtifactFsError> {
    use std::os::windows::fs::MetadataExt;

    reject_windows_reparse_attributes(label, metadata.file_attributes())
}

#[cfg(not(windows))]
pub fn reject_reparse_point(_label: &str, _metadata: &fs::Metadata) -> Result<(), ArtifactFsError> {
    Ok(())
}

pub fn verify_regular_file(
    path: &Path,
    label: &str,
    mode_policy: UnixModePolicy,
) -> Result<fs::Metadata, ArtifactFsError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(label, error))?;
    reject_reparse_point(label, &metadata)?;
    if !metadata.file_type().is_file() {
        return Err(ArtifactFsError::NonRegularFile {
            path: label.to_owned(),
            kind: file_kind(&metadata),
        });
    }
    verify_mode(label, &metadata, false, mode_policy)?;
    Ok(metadata)
}

pub fn walk_directory(
    root: &Path,
    root_label: &str,
    mode_policy: UnixModePolicy,
) -> Result<WalkInventory, ArtifactFsError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| io_error(root_label, error))?;
    reject_reparse_point(root_label, &metadata)?;
    if !metadata.file_type().is_dir() {
        return Err(ArtifactFsError::NonRegularFile {
            path: root_label.to_owned(),
            kind: file_kind(&metadata),
        });
    }
    verify_mode(root_label, &metadata, true, mode_policy)?;

    let mut inventory = WalkInventory {
        files: BTreeSet::new(),
        directories: BTreeSet::new(),
    };
    let mut folded = BTreeMap::new();
    walk_recursive(
        root,
        "",
        root_label,
        mode_policy,
        &mut inventory,
        &mut folded,
    )?;
    Ok(inventory)
}

fn walk_recursive(
    root: &Path,
    relative: &str,
    root_label: &str,
    mode_policy: UnixModePolicy,
    inventory: &mut WalkInventory,
    folded: &mut BTreeMap<String, String>,
) -> Result<(), ArtifactFsError> {
    let current = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let directory_label = if relative.is_empty() {
        root_label
    } else {
        relative
    };
    let entries = fs::read_dir(&current).map_err(|error| io_error(directory_label, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(directory_label, error))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ArtifactFsError::UnsafePath {
                path: directory_label.to_owned(),
                reason: UnsafePathReason::NonPortableName,
            })?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        validate_relative_path(&child)?;
        check_case_collision(folded, &child)?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|error| io_error(&child, error))?;
        reject_reparse_point(&child, &metadata)?;
        if metadata.file_type().is_dir() {
            verify_mode(&child, &metadata, true, mode_policy)?;
            inventory.directories.insert(child.clone());
            walk_recursive(root, &child, root_label, mode_policy, inventory, folded)?;
        } else if metadata.file_type().is_file() {
            verify_mode(&child, &metadata, false, mode_policy)?;
            inventory.files.insert(child);
        } else {
            return Err(ArtifactFsError::NonRegularFile {
                path: child,
                kind: file_kind(&metadata),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn verify_mode(
    path: &str,
    metadata: &fs::Metadata,
    directory: bool,
    policy: UnixModePolicy,
) -> Result<(), ArtifactFsError> {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o7777;
    let forbidden = if directory || policy == UnixModePolicy::AllowExecute {
        mode & 0o7000
    } else {
        mode & 0o7111
    };
    if forbidden != 0 {
        return Err(ArtifactFsError::InvalidFileMode {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_mode(
    _path: &str,
    _metadata: &fs::Metadata,
    _directory: bool,
    _policy: UnixModePolicy,
) -> Result<(), ArtifactFsError> {
    Ok(())
}

fn io_error(path: &str, error: std::io::Error) -> ArtifactFsError {
    ArtifactFsError::Io {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

fn verify_root_components(path: &Path, label: &str) -> Result<(), ArtifactFsError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| io_error(label, error))?
            .join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(ArtifactFsError::UnsafeResolution {
                    path: label.to_owned(),
                });
            }
            Component::Normal(value) => current.push(value),
        }
        let metadata = fs::symlink_metadata(&current).map_err(|error| io_error(label, error))?;
        reject_parent_metadata(label, &metadata, false)?;
    }
    Ok(())
}

fn verify_leaf_components(root: &Path, relative: &str, label: &str) -> Result<(), ArtifactFsError> {
    let components: Vec<&str> = relative.split('/').collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| io_error(label, error))?;
        let is_leaf = index + 1 == components.len();
        if is_leaf {
            reject_reparse_point(label, &metadata)?;
        } else {
            reject_parent_metadata(label, &metadata, false)?;
            if !metadata.file_type().is_dir() {
                return Err(ArtifactFsError::UnsafeResolution {
                    path: label.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn reject_parent_metadata(
    label: &str,
    metadata: &fs::Metadata,
    synthetic_reparse: bool,
) -> Result<(), ArtifactFsError> {
    if synthetic_reparse
        || metadata.file_type().is_symlink()
        || reject_reparse_point(label, metadata).is_err()
    {
        return Err(ArtifactFsError::UnsafeResolution {
            path: label.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn synthetic_parent_reparse_is_an_unsafe_resolution() {
        let metadata = fs::symlink_metadata(".").expect("current directory metadata");
        assert!(matches!(
            reject_parent_metadata("candidate", &metadata, true),
            Err(ArtifactFsError::UnsafeResolution { .. })
        ));
    }

    #[test]
    fn windows_reparse_attribute_has_the_dedicated_identity() {
        assert_eq!(
            reject_windows_reparse_attributes("candidate", FILE_ATTRIBUTE_REPARSE_POINT),
            Err(ArtifactFsError::ReparsePoint {
                path: "candidate".to_owned(),
            })
        );
    }

    #[test]
    fn stable_read_rejects_same_size_different_bytes() {
        let root_path = std::env::temp_dir().join(format!(
            "solstone-artifact-fs-stable-read-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root_path).expect("create isolated root");
        let file = root_path.join("artifact.bin");
        fs::write(&file, b"first").expect("write first bytes");
        let root = ContainedRoot::new(&root_path, "candidate", UnixModePolicy::AllowExecute)
            .expect("contain root");
        let resolved = root
            .resolve("artifact.bin", "artifact.bin")
            .expect("resolve file");
        let error = resolved
            .read_stable(|| fs::write(&file, b"other").expect("replace with same-size bytes"))
            .expect_err("changed bytes must fail");
        assert_eq!(
            error,
            ArtifactFsError::UnsafeResolution {
                path: "artifact.bin".to_owned(),
            }
        );
        fs::remove_dir_all(root_path).expect("remove isolated root");
    }
}
