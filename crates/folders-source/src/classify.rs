// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Deterministic file sorting, partial-download / junk filtering, symlink and
//! junction confinement, Windows-safe path normalization, and overlap detection.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::types::{DeliveryMode, FileKind, FolderKinds, OverlapTarget, SupportedList};

const PARTIAL_SUFFIXES: [&str; 6] = [
    ".crdownload",
    ".part",
    ".download",
    ".opdownload",
    ".filepart",
    ".tmp",
];

const JUNK_NAMES: [&str; 4] = ["thumbs.db", "desktop.ini", "ehthumbs.db", ".ds_store"];

/// Whether a filename looks like an in-progress download or temporary save
/// that must wait until finished.
#[must_use]
pub fn is_partial_download(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    PARTIAL_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// Whether a directory entry is a hidden dot entry, OS metadata junk, or an
/// office temporary lock file that is always skipped.
#[must_use]
pub fn is_junk_entry(file_name: &str) -> bool {
    if file_name.starts_with('.') || file_name.starts_with("~$") || file_name.ends_with('~') {
        return true;
    }
    let lower = file_name.to_ascii_lowercase();
    JUNK_NAMES.contains(&lower.as_str())
}

/// Strip Windows extended-length verbatim prefixes (`\\?\` and `\\?\UNC\`)
/// for clean display, folder naming, and consistent comparison.
#[must_use]
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path.to_path_buf()
}

/// Canonicalize a candidate path and strip any Windows verbatim prefix,
/// preserving the original path if canonicalize fails.
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim_prefix(&canonical)
}

/// Join a forward-slash relative path onto a root path using native path
/// separators (safe with Windows verbatim paths).
#[must_use]
pub fn join_rel_path(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for segment in rel.split('/') {
        if !segment.is_empty() {
            out.push(segment);
        }
    }
    out
}

/// Whether a symlink or NTFS junction resolves inside `canonical_root`.
/// Broken targets or targets escaping the chosen folder return `false`.
#[must_use]
pub fn symlink_stays_inside(canonical_root: &Path, entry_path: &Path) -> bool {
    let Ok(target) = std::fs::canonicalize(entry_path) else {
        return false;
    };
    path_starts_with_ci(
        &strip_verbatim_prefix(&target),
        &strip_verbatim_prefix(canonical_root),
    )
}

fn path_segments(path: &Path, case_insensitive: bool) -> Vec<String> {
    let clean = strip_verbatim_prefix(path);
    let mut out = Vec::new();
    if is_windows_style_path(&clean) {
        let raw = clean.to_string_lossy();
        for seg in raw.split(['\\', '/']) {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if case_insensitive {
                out.push(seg.to_lowercase());
            } else {
                out.push(seg.to_owned());
            }
        }
        return out;
    }
    for comp in clean.components() {
        let s = match comp {
            Component::Prefix(p) => p.as_os_str().to_string_lossy().into_owned(),
            Component::RootDir => "/".to_owned(),
            Component::CurDir => continue,
            Component::ParentDir => "..".to_owned(),
            Component::Normal(n) => n.to_string_lossy().into_owned(),
        };
        if case_insensitive {
            out.push(s.to_lowercase());
        } else {
            out.push(s);
        }
    }
    out
}

fn is_windows_style_path(path: &Path) -> bool {
    if cfg!(windows) {
        return true;
    }
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':') || s.starts_with(r"\\")
}

/// Compare two paths for equality (case-insensitive on Windows or Windows-style paths).
#[must_use]
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    let ci = is_windows_style_path(a) || is_windows_style_path(b);
    path_segments(a, ci) == path_segments(b, ci)
}

/// Check whether `child` is equal to or inside `parent` (case-insensitive on Windows).
#[must_use]
pub fn path_starts_with_ci(child: &Path, parent: &Path) -> bool {
    let ci = is_windows_style_path(child) || is_windows_style_path(parent);
    let c = path_segments(child, ci);
    let p = path_segments(parent, ci);
    !p.is_empty() && c.len() >= p.len() && c[..p.len()] == p[..]
}

/// Whether two canonical paths overlap (equal, ancestor, or descendant).
#[must_use]
pub fn paths_overlap(a: &Path, b: &Path) -> bool {
    paths_equal(a, b) || path_starts_with_ci(a, b) || path_starts_with_ci(b, a)
}

/// Whether a `.md` or `.txt` file crosses triage's document threshold
/// (`long_prose`: >= 5,000 words with >= 70% prose; `structured_prose`: >= 1,500
/// words with >= 70% prose and >= 2 heading levels) or exceeds the note size cap.
#[must_use]
pub fn classify_text_file(bytes: &[u8], supported: &SupportedList) -> FileKind {
    if bytes.len() as u64 > supported.max_note_bytes {
        return FileKind::Document;
    }
    let Ok(raw_text) = std::str::from_utf8(bytes) else {
        return FileKind::Other;
    };
    let text = raw_text.strip_prefix('\u{feff}').unwrap_or(raw_text);
    let mut total_words = 0usize;
    let mut prose_lines = 0usize;
    let mut non_empty_lines = 0usize;
    let mut heading_levels: BTreeSet<usize> = BTreeSet::new();
    let mut in_fence = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            non_empty_lines += 1;
            continue;
        }
        non_empty_lines += 1;
        if in_fence {
            continue;
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
            heading_levels.insert(hashes);
            let rest = line[hashes + 1..].trim();
            total_words += rest.split_whitespace().count();
            prose_lines += 1;
            continue;
        }
        let words = line.split_whitespace().count();
        total_words += words;
        let is_list_or_table = line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with('|')
            || line.starts_with("> ");
        if !is_list_or_table || words >= 6 {
            prose_lines += 1;
        }
    }

    let prose_ok = non_empty_lines > 0 && prose_lines * 100 >= non_empty_lines * 70;
    let long_prose = prose_ok && total_words >= supported.note_long_words;
    let structured_prose = prose_ok
        && total_words >= supported.note_structured_words
        && heading_levels.len() >= supported.note_heading_levels;

    if long_prose || structured_prose {
        FileKind::Document
    } else {
        FileKind::Note
    }
}

/// Deterministic sorting table: assigns a file kind and decides whether file
/// bytes are sent (`Content`), only metadata goes (`MetadataOnly`), or the
/// file is ignored (`Ignore`).
#[must_use]
pub fn classify_file(
    extension: Option<&str>,
    size_bytes: u64,
    is_screenshot: bool,
    note_bytes: Option<&[u8]>,
    kinds: FolderKinds,
    supported: &SupportedList,
) -> (FileKind, DeliveryMode) {
    let ext = extension.map(str::to_ascii_lowercase);
    let ext_str = ext.as_deref().unwrap_or("");

    let is_image = supported.accepted_image_extensions.contains(ext_str)
        || supported.convertible_image_extensions.contains(ext_str);

    let file_kind = if is_screenshot && is_image {
        FileKind::Screenshot
    } else if supported.note_extensions.contains(ext_str) {
        if size_bytes > supported.max_note_bytes {
            FileKind::Document
        } else if let Some(bytes) = note_bytes {
            classify_text_file(bytes, supported)
        } else {
            FileKind::Note
        }
    } else if supported.document_extensions.contains(ext_str) {
        FileKind::Document
    } else if supported.transcript_extensions.contains(ext_str) {
        FileKind::Transcript
    } else if is_image {
        FileKind::Photo
    } else {
        FileKind::Other
    };

    let within_size = size_bytes <= supported.max_content_bytes;
    let delivery = if file_kind == FileKind::Other {
        if kinds.other {
            DeliveryMode::MetadataOnly
        } else {
            DeliveryMode::Ignore
        }
    } else if kinds.enabled_for(file_kind) && within_size {
        DeliveryMode::Content
    } else if kinds.other {
        DeliveryMode::MetadataOnly
    } else {
        DeliveryMode::Ignore
    };

    (file_kind, delivery)
}

/// Overlap check against Obsidian, screenshots, and already-added folders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlapOutcome {
    None,
    AlreadyAdded { id: String, name: String },
    Overlap(OverlapTarget),
}

#[must_use]
pub fn detect_overlap(
    candidate: &Path,
    obsidian: Option<(&Path, &str)>,
    screenshots: Option<(&Path, &str)>,
    existing_folders: &[(&str, &Path, &str)],
) -> OverlapOutcome {
    let canonical = normalize_path(candidate);

    for (id, path, name) in existing_folders {
        let existing = normalize_path(path);
        if paths_equal(&canonical, &existing) {
            return OverlapOutcome::AlreadyAdded {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            };
        }
        if paths_overlap(&canonical, &existing) {
            return OverlapOutcome::Overlap(OverlapTarget::Folder {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            });
        }
    }
    if let Some((path, name)) = obsidian {
        if paths_overlap(&canonical, &normalize_path(path)) {
            return OverlapOutcome::Overlap(OverlapTarget::Obsidian {
                name: name.to_owned(),
            });
        }
    }
    if let Some((path, name)) = screenshots {
        if paths_overlap(&canonical, &normalize_path(path)) {
            return OverlapOutcome::Overlap(OverlapTarget::Screenshots {
                name: name.to_owned(),
            });
        }
    }
    OverlapOutcome::None
}

/// Friendly folder label relative to `home` using ` › ` separators.
#[must_use]
pub fn folder_label(folder: &Path, home: &Path) -> String {
    let clean_folder = strip_verbatim_prefix(folder);
    let clean_home = strip_verbatim_prefix(home);
    let ci = is_windows_style_path(&clean_folder) || is_windows_style_path(&clean_home);
    let home_segs = path_segments(&clean_home, ci);
    let folder_segs_ci = path_segments(&clean_folder, ci);
    let raw_parts: Vec<String> = clean_folder
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    if !home_segs.is_empty()
        && folder_segs_ci.len() > home_segs.len()
        && folder_segs_ci[..home_segs.len()] == home_segs[..]
    {
        let home_normal_count = clean_home
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .count();
        if raw_parts.len() > home_normal_count {
            return raw_parts[home_normal_count..].join(" \u{203a} ");
        }
    }
    if raw_parts.is_empty() {
        clean_folder.display().to_string()
    } else {
        raw_parts.join(" \u{203a} ")
    }
}

/// Short display name for a folder path (its final path component).
#[must_use]
pub fn folder_name(folder: &Path) -> String {
    let clean = strip_verbatim_prefix(folder);
    clean
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map_or_else(|| clean.display().to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partials_and_junk_are_filtered_deterministically() {
        assert!(is_partial_download("edge.exe.crdownload"));
        assert!(is_partial_download("firefox.msi.part"));
        assert!(is_partial_download("movie.mp4.download"));
        assert!(!is_partial_download("report.pdf"));

        assert!(is_junk_entry(".DS_Store"));
        assert!(is_junk_entry(".git"));
        assert!(is_junk_entry("Thumbs.db"));
        assert!(is_junk_entry("desktop.ini"));
        assert!(is_junk_entry("~$contract.docx"));
        assert!(!is_junk_entry("todo.md"));
    }

    #[test]
    fn sorting_table_routes_each_kind_and_respects_64_mib_and_switches() {
        let supported = SupportedList::default();
        let all_on = FolderKinds::default();

        assert_eq!(
            classify_file(
                Some("md"),
                120,
                false,
                Some(b"\xEF\xBB\xBF# short note\nbody"),
                all_on,
                &supported
            ),
            (FileKind::Note, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("pdf"), 400_000, false, None, all_on, &supported),
            (FileKind::Document, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("vtt"), 8_000, false, None, all_on, &supported),
            (FileKind::Transcript, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("png"), 50_000, true, None, all_on, &supported),
            (FileKind::Screenshot, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("heic"), 50_000, false, None, all_on, &supported),
            (FileKind::Photo, DeliveryMode::Content)
        );
        assert_eq!(
            classify_file(Some("docx"), 20_000, false, None, all_on, &supported),
            (FileKind::Other, DeliveryMode::MetadataOnly)
        );
        assert_eq!(
            classify_file(Some("exe"), 58_000_000, false, None, all_on, &supported),
            (FileKind::Other, DeliveryMode::MetadataOnly)
        );
        assert_eq!(
            classify_file(
                Some("pdf"),
                supported.max_content_bytes + 1,
                false,
                None,
                all_on,
                &supported
            ),
            (FileKind::Document, DeliveryMode::MetadataOnly)
        );
        let mut no_docs = all_on;
        no_docs.documents = false;
        assert_eq!(
            classify_file(Some("pdf"), 10_000, false, None, no_docs, &supported),
            (FileKind::Document, DeliveryMode::MetadataOnly)
        );
        no_docs.other = false;
        assert_eq!(
            classify_file(Some("pdf"), 10_000, false, None, no_docs, &supported),
            (FileKind::Document, DeliveryMode::Ignore)
        );
    }

    #[test]
    fn structured_and_long_markdown_with_utf8_bom_promote_to_document() {
        let supported = SupportedList::default();
        let mut structured = String::from("\u{feff}# Chapter One\n\n## Section A\n\n");
        for _ in 0..1_500 {
            structured.push_str("prose ");
        }
        assert_eq!(
            classify_text_file(structured.as_bytes(), &supported),
            FileKind::Document
        );
    }

    #[test]
    fn windows_verbatim_and_case_insensitive_paths_overlap_and_label_cleanly() {
        let verbatim = Path::new(r"\\?\C:\Users\Alice\Downloads");
        assert_eq!(
            strip_verbatim_prefix(verbatim),
            PathBuf::from(r"C:\Users\Alice\Downloads")
        );
        assert!(paths_equal(
            Path::new(r"\\?\C:\Users\Alice\Downloads"),
            Path::new(r"c:\users\alice\downloads")
        ));
        assert!(paths_overlap(
            Path::new(r"C:\Users\Alice\OneDrive\Notes"),
            Path::new(r"c:\users\alice\onedrive")
        ));
    }

    #[test]
    fn overlap_detects_obsidian_screenshots_parent_child_and_exact_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("Vault");
        let sub = vault.join("notes");
        let shots = dir.path().join("Pictures").join("Screenshots");
        let dl = dir.path().join("Downloads");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&shots).unwrap();
        std::fs::create_dir_all(&dl).unwrap();

        assert_eq!(
            detect_overlap(&sub, Some((&vault, "field notes")), None, &[]),
            OverlapOutcome::Overlap(OverlapTarget::Obsidian {
                name: "field notes".into()
            })
        );
        assert_eq!(
            detect_overlap(
                &dir.path().join("Pictures"),
                None,
                Some((&shots, "Screenshots")),
                &[]
            ),
            OverlapOutcome::Overlap(OverlapTarget::Screenshots {
                name: "Screenshots".into()
            })
        );
        assert_eq!(
            detect_overlap(&dl, None, None, &[("f1", &dl, "Downloads")]),
            OverlapOutcome::AlreadyAdded {
                id: "f1".into(),
                name: "Downloads".into()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("chosen");
        let outside = dir.path().join("secret.txt");
        let inside = root.join("ok.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "secret").unwrap();
        std::fs::write(&inside, "ok").unwrap();
        let esc_link = root.join("escape.txt");
        let ok_link = root.join("local.txt");
        std::os::unix::fs::symlink(&outside, &esc_link).unwrap();
        std::os::unix::fs::symlink(&inside, &ok_link).unwrap();

        let canon_root = std::fs::canonicalize(&root).unwrap();
        assert!(!symlink_stays_inside(&canon_root, &esc_link));
        assert!(symlink_stays_inside(&canon_root, &ok_link));
    }
}
