// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Bounded, restartable adoption of the old owner tree.
//!
//! Only known owner state is copied from `%LocalAppData%\\Solstone`. The
//! Velopack installation tree and native-host manifests have separate owners.

use std::collections::BTreeSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
std::thread_local! {
    static CLEANUP_FAIL_AFTER_REMOVALS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

trait OwnerFileSystem {
    fn open_source_file(&self, root: &Path, relative: &Path) -> io::Result<File>;
    fn remove_source_file(
        &self,
        root: &Path,
        relative: &Path,
        verified_destination_root: &Path,
    ) -> io::Result<()>;
    fn remove_empty_source_dir(&self, root: &Path, relative: &Path) -> io::Result<()>;
}

struct SystemOwnerFileSystem;

impl OwnerFileSystem for SystemOwnerFileSystem {
    fn open_source_file(&self, root: &Path, relative: &Path) -> io::Result<File> {
        open_source_file(root, relative)
    }

    fn remove_source_file(
        &self,
        root: &Path,
        relative: &Path,
        verified_destination_root: &Path,
    ) -> io::Result<()> {
        remove_source_file(root, relative, verified_destination_root)
    }

    fn remove_empty_source_dir(&self, root: &Path, relative: &Path) -> io::Result<()> {
        remove_empty_source_dir(root, relative)
    }
}

const COMPLETE_FILE: &str = "owner-adoption-v1.json";
const COMPLETE_SCHEMA: &str = "solstone.owner-adoption.v1";
const COMPLETE_SOURCE: &str = "legacy-localappdata-solstone";
/// Every file a pre-separation release could have written at the legacy root.
/// Migration, key-recovery and refused-pairing records only ever exist under
/// the owner root, so they are not adoption inventory.
const ROOT_FILES: &[&str] = &[
    "pairing.json",
    "pairing.json.tmp",
    "pairing-answer.json",
    "pairing-answer.json.tmp",
    "journal-version.json",
    "journal-version.json.tmp",
    "exclusions.json",
    "mic.json",
    "pause.txt",
    "pause.tmp",
    "hotkey.json",
    "update.json",
];

/// The new owner root. On non-Windows hosts this remains constructible for
/// deterministic filesystem tests.
pub fn owner_root() -> PathBuf {
    local_app_data_root().join("SolstoneOwner")
}

/// The previous root, retained as an adoption source.
pub fn legacy_root() -> PathBuf {
    local_app_data_root().join("Solstone")
}

fn local_app_data_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Return the currently existing root for a read-only diagnostic. This never
/// seeds the destination or runs adoption.
pub fn existing_root() -> PathBuf {
    let owner = owner_root();
    existing_root_between(&legacy_root(), &owner)
}

fn existing_root_between(legacy: &Path, owner: &Path) -> PathBuf {
    if completion_marker_is_regular(owner) {
        owner.to_path_buf()
    } else if legacy.exists() {
        legacy.to_path_buf()
    } else {
        owner.to_path_buf()
    }
}

/// Adopt the bounded legacy inventory. Call only while holding the app's
/// per-session owner mutex. Legacy bytes are retained so every failure and
/// every conflict leaves a complete recovery source.
pub fn adopt_legacy_state() -> io::Result<PathBuf> {
    adopt_between(&legacy_root(), &owner_root())
}

fn adopt_between(source: &Path, destination: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(destination)?;
    if !is_directory_non_reparse(&fs::symlink_metadata(destination)?) {
        return Err(inventory_error(destination));
    }
    let marker = destination.join(COMPLETE_FILE);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if is_regular_non_reparse(&metadata) => {
            let inventory = parse_completion_marker(&fs::read(&marker)?)?;
            // The durable marker makes the destination authoritative. Cleanup
            // is retryable and best-effort so a cleanup error cannot make a
            // complete destination unavailable or restore legacy precedence.
            let _ = cleanup_completed(source, destination, &inventory);
            return Ok(destination.to_path_buf());
        }
        Ok(_) => return Err(inventory_error(&marker)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let files = if read_known_dir(source)?.is_some() {
        known_files(source)?
    } else {
        Vec::new()
    };
    for relative in &files {
        copy_exact_file(source, destination, relative)?;
    }

    let marker_bytes = completion_marker_bytes(&files)?;
    publish_completion_marker(&marker, &marker_bytes)?;
    let inventory = CompletionInventory::Exact(files);
    let _ = cleanup_completed(source, destination, &inventory);
    Ok(destination.to_path_buf())
}

#[derive(Debug, PartialEq, Eq)]
enum CompletionInventory {
    Exact(Vec<PathBuf>),
}

fn completion_marker_is_regular(owner: &Path) -> bool {
    fs::symlink_metadata(owner.join(COMPLETE_FILE))
        .is_ok_and(|metadata| is_regular_non_reparse(&metadata))
}

fn completion_marker_bytes(files: &[PathBuf]) -> io::Result<Vec<u8>> {
    let files = files
        .iter()
        .map(|path| {
            validate_relative_path(path)?;
            path.to_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "owner adoption inventory path is not Unicode",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    serde_json::to_vec(&serde_json::json!({
        "schema": COMPLETE_SCHEMA,
        "source": COMPLETE_SOURCE,
        "files": files,
    }))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn parse_completion_marker(bytes: &[u8]) -> io::Result<CompletionInventory> {
    let marker: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if marker.get("schema").and_then(serde_json::Value::as_str) != Some(COMPLETE_SCHEMA)
        || marker.get("source").and_then(serde_json::Value::as_str) != Some(COMPLETE_SOURCE)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner adoption marker is invalid",
        ));
    }
    let entries = marker
        .get("files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "owner adoption inventory is missing",
            )
        })?;
    let mut files = Vec::with_capacity(entries.len());
    let mut unique = BTreeSet::new();
    for entry in entries {
        let raw = entry.as_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "owner adoption inventory path is invalid",
            )
        })?;
        let path = PathBuf::from(raw);
        validate_relative_path(&path)?;
        if path.to_str() != Some(raw) || !unique.insert(path.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owner adoption inventory path is not canonical",
            ));
        }
        files.push(path);
    }
    Ok(CompletionInventory::Exact(files))
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    validate_relative_components(path)?;
    if !known_inventory_path(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner adoption path is outside the known inventory",
        ));
    }
    Ok(())
}

fn validate_relative_components(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner adoption path is not a bounded relative path",
        ));
    }
    Ok(())
}

fn known_inventory_path(path: &Path) -> bool {
    let Some(parts) = path
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    match parts.as_slice() {
        [name] => ROOT_FILES.contains(name),
        ["logs", name] => {
            *name == "solstone.log" || (1..=4).any(|n| *name == format!("solstone.{n}.log"))
        }
        ["segments", segment, _file] => segment_dir_name(segment),
        ["segments", "quarantine", quarantine, _file] => quarantine_dir_name(quarantine),
        ["browser-intake", name] => {
            ["status.json", "status.json.partial", "update-quiesce.txt"].contains(name)
        }
        ["browser-intake", "receipts", _file] => true,
        ["browser-intake", subdir, _owner, _file] => *subdir == "open" || *subdir == "outbox",
        _ => false,
    }
}

fn known_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for name in ROOT_FILES {
        let path = root.join(name);
        if regular_file_exists(&path)? {
            files.push(PathBuf::from(name));
        }
    }

    collect_log_files(root, &mut files)?;
    collect_segment_files(root, &mut files)?;
    collect_browser_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn regular_file_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if is_regular_non_reparse(&meta) => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("known owner path is not a regular file: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn collect_log_files(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let dir = root.join("logs");
    let entries = match read_known_dir(&dir)? {
        Some(entries) => entries,
        None => return Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "solstone.log" || (1..=4).any(|n| name == format!("solstone.{n}.log")) {
            if !is_regular_non_reparse(&fs::symlink_metadata(entry.path())?) {
                return Err(inventory_error(&entry.path()));
            }
            files.push(PathBuf::from("logs").join(name));
        }
    }
    Ok(())
}

fn read_known_dir(path: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_directory_non_reparse(&metadata) => fs::read_dir(path).map(Some),
        Ok(_) => Err(inventory_error(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn collect_segment_files(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let dir = root.join("segments");
    let Some(entries) = read_known_dir(&dir)? else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let entry_metadata = fs::symlink_metadata(entry.path())?;
        if is_reparse_point(&entry_metadata) {
            return Err(inventory_error(&entry.path()));
        }
        if name == "quarantine" {
            if !entry_metadata.is_dir() {
                return Err(inventory_error(&entry.path()));
            }
            collect_segment_children(Path::new("segments/quarantine"), &entry.path(), true, files)?;
        } else if segment_dir_name(name) {
            if !entry_metadata.is_dir() {
                return Err(inventory_error(&entry.path()));
            }
            collect_direct_files(&Path::new("segments").join(name), &entry.path(), files)?;
        } else if entry_metadata.is_dir() {
            // An unrecognized directory beneath the owner segment root may hold
            // custody bytes. Refuse adoption rather than publish a partial tree.
            return Err(inventory_error(&entry.path()));
        }
    }
    Ok(())
}

fn segment_dir_name(name: &str) -> bool {
    let bare = name.strip_suffix(".incomplete").unwrap_or(name);
    !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit())
}

fn collect_segment_children(
    relative: &Path,
    dir: &Path,
    quarantined: bool,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let Some(entries) = read_known_dir(dir)? else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let valid = if quarantined {
            quarantine_dir_name(name)
        } else {
            segment_dir_name(name)
        };
        let entry_metadata = fs::symlink_metadata(entry.path())?;
        if is_reparse_point(&entry_metadata) {
            return Err(inventory_error(&entry.path()));
        }
        if valid {
            if !entry_metadata.is_dir() {
                return Err(inventory_error(&entry.path()));
            }
            collect_direct_files(&relative.join(name), &entry.path(), files)?;
        } else if entry_metadata.is_dir() {
            return Err(inventory_error(&entry.path()));
        }
    }
    Ok(())
}

fn quarantine_dir_name(name: &str) -> bool {
    let mut parts = name.split('-');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    segment_dir_name(first)
        && second
            .is_none_or(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}

fn collect_direct_files(relative: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = read_known_dir(dir)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "known owner directory disappeared")
    })?;
    for entry in entries {
        let entry = entry?;
        if !is_regular_non_reparse(&fs::symlink_metadata(entry.path())?) {
            return Err(inventory_error(&entry.path()));
        }
        files.push(relative.join(entry.file_name()));
    }
    Ok(())
}

fn collect_browser_files(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let dir = root.join("browser-intake");
    match fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) if !is_directory_non_reparse(&metadata) => return Err(inventory_error(&dir)),
        Ok(_) => {}
    }
    for name in ["status.json", "status.json.partial", "update-quiesce.txt"] {
        let path = dir.join(name);
        if regular_file_exists(&path)? {
            files.push(PathBuf::from("browser-intake").join(name));
        }
    }
    for subdir in ["open", "receipts", "outbox"] {
        let path = dir.join(subdir);
        let Some(entries) = read_known_dir(&path)? else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if is_reparse_point(&metadata) {
                return Err(inventory_error(&entry.path()));
            }
            if subdir == "receipts" {
                if !is_regular_non_reparse(&metadata) {
                    return Err(inventory_error(&entry.path()));
                }
                files.push(
                    PathBuf::from("browser-intake")
                        .join(subdir)
                        .join(entry.file_name()),
                );
            } else {
                if !metadata.is_dir() {
                    return Err(inventory_error(&entry.path()));
                }
                let child_dir = entry.path();
                let children = read_known_dir(&child_dir)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "browser owner directory disappeared",
                    )
                })?;
                for child in children {
                    let child = child?;
                    if !is_regular_non_reparse(&fs::symlink_metadata(child.path())?) {
                        return Err(inventory_error(&child.path()));
                    }
                    files.push(
                        PathBuf::from("browser-intake")
                            .join(subdir)
                            .join(entry.file_name())
                            .join(child.file_name()),
                    );
                }
            }
        }
    }
    Ok(())
}

fn inventory_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("known owner path is not a regular file: {}", path.display()),
    )
}

fn copy_exact_file(source: &Path, destination: &Path, relative: &Path) -> io::Result<()> {
    copy_exact_file_with(&SystemOwnerFileSystem, source, destination, relative)
}

fn copy_exact_file_with(
    source_fs: &dyn OwnerFileSystem,
    source: &Path,
    destination: &Path,
    relative: &Path,
) -> io::Result<()> {
    let from = source.join(relative);
    let to = destination.join(relative);
    let metadata = fs::symlink_metadata(&from)?;
    if !metadata.file_type().is_file() {
        return Err(inventory_error(&from));
    }
    ensure_destination_parent(destination, relative)?;
    match fs::symlink_metadata(&to) {
        Ok(metadata) if metadata.file_type().is_file() => {
            return if same_source_bytes(source_fs, source, relative, destination, relative)? {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("owner adoption conflict at {}", relative.display()),
                ))
            };
        }
        Ok(_) => return Err(inventory_error(&to)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let temp_name = format!(".owner-adopt-{}.tmp", relative_key(relative));
    let temp_relative = relative.with_file_name(&temp_name);
    let tmp = to.with_file_name(temp_name);
    match fs::symlink_metadata(&tmp) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if !same_source_bytes(source_fs, source, relative, destination, &temp_relative)? {
                fs::remove_file(&tmp)?;
                copy_source_to_temp(source_fs, source, relative, &tmp)?;
            }
        }
        Ok(_) => return Err(inventory_error(&tmp)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            copy_source_to_temp(source_fs, source, relative, &tmp)?;
        }
        Err(error) => return Err(error),
    }
    if !same_source_bytes(source_fs, source, relative, destination, &temp_relative)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("owner adoption readback mismatch at {}", relative.display()),
        ));
    }
    match publish_exact_file(&tmp, &to) {
        Ok(()) => {}
        Err(error) => {
            if !regular_file_exists(&to)?
                || !same_source_bytes(source_fs, source, relative, destination, relative)?
            {
                return Err(error);
            }
        }
    }
    if tmp.exists() {
        fs::remove_file(&tmp)?;
        sync_parent(&to)?;
    }
    if !same_source_bytes(source_fs, source, relative, destination, relative)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "owner adoption publication mismatch at {}",
                relative.display()
            ),
        ));
    }
    Ok(())
}

fn ensure_destination_parent(destination: &Path, relative: &Path) -> io::Result<()> {
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    let mut current = destination.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owner adoption path is not relative",
            ));
        };
        current.push(name);
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !is_directory_non_reparse(&fs::symlink_metadata(&current)?) {
            return Err(inventory_error(&current));
        }
    }
    Ok(())
}

fn copy_source_to_temp(
    source_fs: &dyn OwnerFileSystem,
    root: &Path,
    relative: &Path,
    temp: &Path,
) -> io::Result<()> {
    let mut input = source_fs.open_source_file(root, relative)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(temp)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

#[allow(unsafe_code)]
fn publish_exact_file(staged: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

        let source: Vec<u16> = staged.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: both paths are NUL-terminated and remain live through the call.
        // The sibling stage keeps the move on one volume; WRITE_THROUGH makes
        // publication durable, and no replace flag means conflicts are refused.
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(target.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| io::Error::other(error.to_string()))?;
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(staged, destination)?;
        sync_parent(destination)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn sync_parent(_path: &Path) -> io::Result<()> {
    // The only publication primitive here is MoveFileExW with WRITE_THROUGH.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable owner adoption is unsupported on this platform",
    ))
}

fn relative_key(path: &Path) -> String {
    path.as_os_str()
        .to_string_lossy()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn same_source_bytes(
    source_fs: &dyn OwnerFileSystem,
    root: &Path,
    source_relative: &Path,
    destination_root: &Path,
    destination_relative: &Path,
) -> io::Result<bool> {
    let mut source = source_fs.open_source_file(root, source_relative)?;
    let mut destination = source_fs.open_source_file(destination_root, destination_relative)?;
    same_file_bytes(&mut source, &mut destination)
}

fn same_file_bytes(source: &mut File, destination: &mut File) -> io::Result<bool> {
    let mut source_buffer = [0; 64 * 1024];
    let mut destination_buffer = [0; 64 * 1024];
    loop {
        // A short read is not a mismatch: fill each buffer to the same length.
        let source_len = fill_buffer(source, &mut source_buffer)?;
        let destination_len = fill_buffer(destination, &mut destination_buffer)?;
        if source_len != destination_len
            || source_buffer[..source_len] != destination_buffer[..destination_len]
        {
            return Ok(false);
        }
        if source_len == 0 {
            return Ok(true);
        }
    }
}

fn fill_buffer(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn is_reparse_point(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

fn is_regular_non_reparse(metadata: &Metadata) -> bool {
    metadata.is_file() && !is_reparse_point(metadata)
}

fn is_directory_non_reparse(metadata: &Metadata) -> bool {
    metadata.is_dir() && !is_reparse_point(metadata)
}

#[cfg(not(windows))]
fn open_source_file(root: &Path, relative: &Path) -> io::Result<File> {
    let metadata = safe_metadata(root, relative)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "owner source file disappeared"))?;
    if !is_regular_non_reparse(&metadata) {
        return Err(inventory_error(&root.join(relative)));
    }
    File::open(root.join(relative))
}

#[cfg(windows)]
fn open_source_file(root: &Path, relative: &Path) -> io::Result<File> {
    open_windows_source_file(root, relative)
}

#[cfg(not(windows))]
fn remove_source_file(
    root: &Path,
    relative: &Path,
    verified_destination_root: &Path,
) -> io::Result<()> {
    let metadata = safe_metadata(root, relative)?;
    let Some(metadata) = metadata else {
        return Ok(());
    };
    if !is_regular_non_reparse(&metadata) {
        return Err(inventory_error(&root.join(relative)));
    }
    let mut source = open_source_file(root, relative)?;
    let mut destination = open_source_file(verified_destination_root, relative)?;
    if !same_file_bytes(&mut source, &mut destination)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner source changed after verification",
        ));
    }
    fs::remove_file(root.join(relative))
}

#[cfg(windows)]
fn remove_source_file(
    root: &Path,
    relative: &Path,
    verified_destination_root: &Path,
) -> io::Result<()> {
    remove_windows_source_file(root, relative, verified_destination_root)
}

#[cfg(not(windows))]
fn remove_empty_source_dir(root: &Path, relative: &Path) -> io::Result<()> {
    let Some(metadata) = safe_metadata(root, relative)? else {
        return Ok(());
    };
    if !is_directory_non_reparse(&metadata) {
        return Err(inventory_error(&root.join(relative)));
    }
    if fs::read_dir(root.join(relative))?.next().is_some() {
        return Ok(());
    }
    match fs::remove_dir(root.join(relative)) {
        Ok(()) => sync_parent(&root.join(relative)),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::DirectoryNotEmpty =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn remove_empty_source_dir(root: &Path, relative: &Path) -> io::Result<()> {
    remove_windows_empty_source_dir(root, relative)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn open_windows_source_file(root: &Path, relative: &Path) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    validate_relative_components(relative)?;
    let mut guards = windows_parent_guards(root, relative)?;
    let path = root.join(relative);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: path is NUL-terminated and all handles remain live through the
    // open. OPEN_REPARSE_POINT prevents following a final reparse point.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OPEN_REPARSE_POINT.0),
            HANDLE::default(),
        )
    }
    .map_err(windows_error)?;
    let file = unsafe { File::from_raw_handle(handle.0 as *mut _) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns the valid handle and `info` is writable for the call.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(windows_error)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
    {
        return Err(inventory_error(&path));
    }
    guards.clear();
    Ok(file)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn remove_windows_source_file(
    root: &Path,
    relative: &Path,
    verified_destination_root: &Path,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOLEAN, GENERIC_READ, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, OPEN_EXISTING,
    };

    validate_relative_components(relative)?;
    let mut guards = windows_parent_guards(root, relative)?;
    let path = root.join(relative);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the validated root and ancestor handles stay open and the final
    // path is opened as a reparse object rather than followed.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            DELETE.0 | GENERIC_READ.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OPEN_REPARSE_POINT.0),
            HANDLE::default(),
        )
    }
    .map_err(windows_error)?;
    let mut file = unsafe { File::from_raw_handle(handle.0 as *mut _) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a valid handle and `info` is writable for the call.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(windows_error)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
    {
        return Err(inventory_error(&path));
    }
    let mut destination = open_windows_source_file(verified_destination_root, relative)?;
    if !same_file_bytes(&mut file, &mut destination)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner source changed after verification",
        ));
    }
    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(1),
    };
    // SAFETY: the disposition structure has the exact Win32 layout for the
    // FileDispositionInfo class and remains live through the call.
    unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(windows_error)?;
    guards.clear();
    drop(file);
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn remove_windows_empty_source_dir(root: &Path, relative: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_MODE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    validate_relative_components(relative)?;
    let mut guards = windows_parent_guards(root, relative)?;
    let path = root.join(relative);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: the validated ancestor handles remain live and the final entry
    // is opened without following a reparse point.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            DELETE.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(
                FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0,
            ),
            HANDLE::default(),
        )
    }
    .map_err(windows_error)?;
    let directory = unsafe { File::from_raw_handle(handle.0 as *mut _) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `directory` owns a valid handle and `info` is writable.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(windows_error)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0
    {
        return Err(inventory_error(&path));
    }
    match fs::read_dir(&path)?.next() {
        Some(_) => return Ok(()),
        None => {}
    }
    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(1),
    };
    // SAFETY: the disposition structure is valid for FileDispositionInfo.
    unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(windows_error)?;
    guards.clear();
    drop(directory);
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_parent_guards(root: &Path, relative: &Path) -> io::Result<Vec<File>> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let absolute_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut directories = Vec::new();
    let mut current = PathBuf::new();
    for component in absolute_root.components() {
        current.push(component.as_os_str());
        if !current.is_absolute() || current == Path::new("\\") {
            continue;
        }
        if matches!(component, std::path::Component::Normal(_))
            || matches!(component, std::path::Component::RootDir)
        {
            let wide: Vec<u16> = current.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: each absolute ancestor is NUL-terminated and is opened
            // without following a final reparse point. Omitting FILE_SHARE_DELETE
            // pins its name until the protected child file handle is opened.
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    FILE_READ_ATTRIBUTES.0,
                    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(
                        FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0,
                    ),
                    HANDLE::default(),
                )
            }
            .map_err(windows_error)?;
            let guard = unsafe { File::from_raw_handle(handle.0 as *mut _) };
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: `guard` owns a valid directory handle and info is writable.
            unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(windows_error)?;
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
                || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0
            {
                return Err(inventory_error(&current));
            }
            directories.push(guard);
        }
    }
    let components: Vec<_> = relative.components().collect();
    let mut parent = absolute_root;
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let std::path::Component::Normal(name) = component else {
            return Err(inventory_error(relative));
        };
        parent.push(name);
        let wide: Vec<u16> = parent.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: parent was built from validated normal components. Opening
        // with OPEN_REPARSE_POINT plus the attribute check refuses a swapped link.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(
                    FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0,
                ),
                HANDLE::default(),
            )
        }
        .map_err(windows_error)?;
        let guard = unsafe { File::from_raw_handle(handle.0 as *mut _) };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `guard` owns a valid handle and info is writable.
        unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(windows_error)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0
        {
            return Err(inventory_error(&parent));
        }
        directories.push(guard);
    }
    Ok(directories)
}

#[cfg(windows)]
fn windows_error(error: windows::core::Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn safe_metadata(root: &Path, relative: &Path) -> io::Result<Option<Metadata>> {
    validate_relative_components(relative)?;
    match fs::symlink_metadata(root) {
        Ok(metadata) if is_directory_non_reparse(&metadata) => {}
        Ok(_) => return Err(inventory_error(root)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let components: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err(inventory_error(relative));
        };
        current.push(name);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if is_reparse_point(&metadata) {
            return Err(inventory_error(&current));
        }
        if index + 1 != components.len() && !metadata.is_dir() {
            return Err(inventory_error(&current));
        }
        if index + 1 == components.len() {
            return Ok(Some(metadata));
        }
    }
    Ok(None)
}

fn cleanup_completed(
    source: &Path,
    destination: &Path,
    inventory: &CompletionInventory,
) -> io::Result<()> {
    cleanup_completed_with(&SystemOwnerFileSystem, source, destination, inventory)
}

fn cleanup_completed_with(
    source_fs: &dyn OwnerFileSystem,
    source: &Path,
    destination: &Path,
    inventory: &CompletionInventory,
) -> io::Result<()> {
    let marker_path = destination.join(COMPLETE_FILE);
    let marker_metadata = fs::symlink_metadata(&marker_path)?;
    if !is_regular_non_reparse(&marker_metadata)
        || parse_completion_marker(&fs::read(&marker_path)?)? != *inventory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner cleanup requires its exact durable completion marker",
        ));
    }
    let CompletionInventory::Exact(files) = inventory;

    let mut removed = 0;
    let mut first_error = None;
    for relative in files {
        let source_path = source.join(relative);
        let destination_path = destination.join(relative);
        let Some(source_metadata) = safe_metadata(source, relative)? else {
            continue;
        };
        if !is_regular_non_reparse(&source_metadata) {
            return Err(inventory_error(&source_path));
        }
        let Some(destination_metadata) = safe_metadata(destination, relative)? else {
            if first_error.is_none() {
                first_error = Some(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("adopted owner file is missing: {}", relative.display()),
                ));
            }
            continue;
        };
        if !is_regular_non_reparse(&destination_metadata) {
            return Err(inventory_error(&destination_path));
        }
        if !same_source_bytes(source_fs, source, relative, destination, relative)? {
            if first_error.is_none() {
                first_error = Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("adopted owner file differs: {}", relative.display()),
                ));
            }
            continue;
        }
        maybe_inject_cleanup_failure(removed)?;
        source_fs.remove_source_file(source, relative, destination)?;
        sync_parent(&source_path)?;
        removed += 1;
    }
    maybe_inject_cleanup_failure(removed)?;

    if let Err(error) = remove_empty_inventory_dirs(source_fs, source, files) {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn maybe_inject_cleanup_failure(removed: usize) -> io::Result<()> {
    #[cfg(test)]
    if CLEANUP_FAIL_AFTER_REMOVALS.with(|fail| fail.get() == Some(removed)) {
        CLEANUP_FAIL_AFTER_REMOVALS.with(|fail| fail.set(None));
        return Err(io::Error::other("injected owner cleanup failure"));
    }
    let _ = removed;
    Ok(())
}

fn remove_empty_inventory_dirs(
    source_fs: &dyn OwnerFileSystem,
    source: &Path,
    files: &[PathBuf],
) -> io::Result<()> {
    let mut directories = BTreeSet::new();
    for file in files {
        let mut parent = file.parent();
        while let Some(relative) = parent {
            if relative.as_os_str().is_empty() {
                break;
            }
            validate_relative_components(relative)?;
            directories.insert(relative.to_path_buf());
            parent = relative.parent();
        }
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for relative in directories {
        let Some(metadata) = safe_metadata(source, &relative)? else {
            continue;
        };
        if !is_directory_non_reparse(&metadata) {
            return Err(inventory_error(&source.join(&relative)));
        }
        source_fs.remove_empty_source_dir(source, &relative)?;
    }
    Ok(())
}

fn publish_completion_marker(path: &Path, complete_bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    match fs::symlink_metadata(&tmp) {
        Ok(metadata) if is_regular_non_reparse(&metadata) => {
            if fs::read(&tmp)? != complete_bytes {
                fs::remove_file(&tmp)?;
                let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
                file.write_all(complete_bytes)?;
                file.sync_all()?;
            }
        }
        Ok(_) => return Err(inventory_error(&tmp)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(complete_bytes)?;
            file.sync_all()?;
        }
        Err(error) => return Err(error),
    }
    if fs::read(&tmp)? != complete_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner adoption marker readback mismatch",
        ));
    }
    match publish_exact_file(&tmp, path) {
        Ok(()) => {}
        Err(error) => {
            if !regular_file_exists(path)? || fs::read(path)? != complete_bytes {
                return Err(error);
            }
        }
    }
    if tmp.exists() {
        fs::remove_file(tmp)?;
        sync_parent(path)?;
    }
    if fs::read(path)? != complete_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owner adoption completion verification failed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum SwapPoint {
        Open,
        Delete,
    }

    #[cfg(unix)]
    struct SwapAncestorFs {
        source: PathBuf,
        external_segments: PathBuf,
        point: SwapPoint,
        swapped: std::cell::Cell<bool>,
    }

    #[cfg(unix)]
    impl SwapAncestorFs {
        fn swap(&self) -> io::Result<()> {
            if self.swapped.replace(true) {
                return Ok(());
            }
            fs::remove_dir_all(self.source.join("segments"))?;
            std::os::unix::fs::symlink(&self.external_segments, self.source.join("segments"))
        }
    }

    #[cfg(unix)]
    impl OwnerFileSystem for SwapAncestorFs {
        fn open_source_file(&self, root: &Path, relative: &Path) -> io::Result<File> {
            if matches!(self.point, SwapPoint::Open) {
                self.swap()?;
            }
            SystemOwnerFileSystem.open_source_file(root, relative)
        }

        fn remove_source_file(
            &self,
            root: &Path,
            relative: &Path,
            verified_destination: &Path,
        ) -> io::Result<()> {
            if matches!(self.point, SwapPoint::Delete) {
                self.swap()?;
            }
            SystemOwnerFileSystem.remove_source_file(root, relative, verified_destination)
        }

        fn remove_empty_source_dir(&self, root: &Path, relative: &Path) -> io::Result<()> {
            SystemOwnerFileSystem.remove_empty_source_dir(root, relative)
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        #[cfg(unix)]
        let temp = PathBuf::from("/var/tmp");
        #[cfg(not(unix))]
        let temp = std::env::temp_dir();
        temp.join(format!(
            "owner-adoption-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn setup_ancestor_swap(label: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, Vec<PathBuf>) {
        let source = temp_root(&format!("{label}-source"));
        let destination = temp_root(&format!("{label}-destination"));
        let external = temp_root(&format!("{label}-external"));
        let external_segments = external.join("segments-data");
        let relative = PathBuf::from("segments/7/audio.flac");
        fs::create_dir_all(source.join("segments/7")).unwrap();
        fs::create_dir_all(destination.join("segments/7")).unwrap();
        fs::create_dir_all(external_segments.join("7")).unwrap();
        fs::write(source.join(&relative), b"source bytes").unwrap();
        fs::write(destination.join(&relative), b"source bytes").unwrap();
        fs::write(external_segments.join("7/audio.flac"), b"outside bytes").unwrap();
        (
            source,
            destination,
            external_segments,
            external,
            vec![relative],
        )
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_swap_before_source_open_is_refused() {
        let (source, destination, external_segments, external, files) =
            setup_ancestor_swap("open-swap");
        let filesystem = SwapAncestorFs {
            source: source.clone(),
            external_segments: external_segments.clone(),
            point: SwapPoint::Open,
            swapped: std::cell::Cell::new(false),
        };
        let error =
            copy_exact_file_with(&filesystem, &source, &destination, &files[0]).unwrap_err();
        assert!(error
            .to_string()
            .contains("known owner path is not a regular file"));
        assert_eq!(
            fs::read(external_segments.join("7/audio.flac")).unwrap(),
            b"outside bytes"
        );
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_symlink_swap_before_source_delete_is_refused() {
        let (source, destination, external_segments, external, files) =
            setup_ancestor_swap("delete-swap");
        let marker = destination.join(COMPLETE_FILE);
        publish_completion_marker(&marker, &completion_marker_bytes(&files).unwrap()).unwrap();
        let filesystem = SwapAncestorFs {
            source: source.clone(),
            external_segments: external_segments.clone(),
            point: SwapPoint::Delete,
            swapped: std::cell::Cell::new(false),
        };
        let error = cleanup_completed_with(
            &filesystem,
            &source,
            &destination,
            &CompletionInventory::Exact(files),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("known owner path is not a regular file"));
        assert_eq!(
            fs::read(external_segments.join("7/audio.flac")).unwrap(),
            b"outside bytes"
        );
        assert_eq!(
            fs::read(destination.join("segments/7/audio.flac")).unwrap(),
            b"source bytes"
        );
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
        let _ = fs::remove_dir_all(external);
    }

    #[test]
    fn adoption_copies_only_known_state_and_publishes_completion_last() {
        let source = temp_root("source");
        let destination = temp_root("destination");
        fs::create_dir_all(source.join("segments/7")).unwrap();
        fs::create_dir_all(source.join("segments/quarantine/3.incomplete")).unwrap();
        fs::create_dir_all(source.join("segments/quarantine/5.incomplete-2")).unwrap();
        fs::create_dir_all(source.join("packages/current")).unwrap();
        fs::create_dir_all(source.join("logs")).unwrap();
        fs::create_dir_all(source.join("unknown/nested")).unwrap();
        fs::create_dir_all(source.join("browser-intake/open/held-1")).unwrap();
        fs::create_dir_all(source.join("browser-intake/receipts")).unwrap();
        fs::create_dir_all(source.join("browser-intake/outbox/pending-1")).unwrap();
        fs::write(source.join("pairing.json"), b"paired").unwrap();
        fs::write(source.join("segments/7/video.mp4"), b"segment").unwrap();
        fs::write(source.join("segments/7/.uploaded"), b"ack").unwrap();
        fs::write(
            source.join("segments/quarantine/3.incomplete/mic.pcm.partial"),
            b"recovery bytes",
        )
        .unwrap();
        fs::write(
            source.join("segments/quarantine/5.incomplete-2/.uploaded"),
            b"recovery ack",
        )
        .unwrap();
        fs::write(source.join("logs/solstone.log"), b"log").unwrap();
        fs::write(source.join("logs/foreign.log"), b"unknown log").unwrap();
        fs::write(source.join("browser-intake/status.json"), b"status").unwrap();
        fs::write(
            source.join("browser-intake/status.json.partial"),
            b"partial status",
        )
        .unwrap();
        fs::write(
            source.join("browser-intake/update-quiesce.txt"),
            b"quiesced",
        )
        .unwrap();
        fs::write(
            source.join("browser-intake/open/held-1/pages.txt"),
            b"held text",
        )
        .unwrap();
        fs::write(
            source.join("browser-intake/receipts/held-1.json"),
            b"receipt",
        )
        .unwrap();
        fs::write(
            source.join("browser-intake/outbox/pending-1/pages.txt"),
            b"outbox text",
        )
        .unwrap();
        fs::write(source.join("packages/.betaId"), b"install-id").unwrap();
        fs::write(
            source.join("packages/current/app.nupkg"),
            b"installer package",
        )
        .unwrap();
        fs::write(source.join("unknown.txt"), b"unknown").unwrap();
        fs::write(source.join("unknown/nested/foreign.bin"), b"unknown nested").unwrap();

        adopt_between(&source, &destination).unwrap();

        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"paired"
        );
        assert_eq!(
            fs::read(destination.join("segments/7/.uploaded")).unwrap(),
            b"ack"
        );
        assert_eq!(
            fs::read(destination.join("segments/quarantine/3.incomplete/mic.pcm.partial")).unwrap(),
            b"recovery bytes"
        );
        assert_eq!(
            fs::read(destination.join("segments/quarantine/5.incomplete-2/.uploaded")).unwrap(),
            b"recovery ack"
        );
        assert_eq!(
            fs::read(destination.join("logs/solstone.log")).unwrap(),
            b"log"
        );
        assert_eq!(
            fs::read(destination.join("browser-intake/status.json.partial")).unwrap(),
            b"partial status"
        );
        assert_eq!(
            fs::read(destination.join("browser-intake/open/held-1/pages.txt")).unwrap(),
            b"held text"
        );
        assert_eq!(
            fs::read(destination.join("browser-intake/receipts/held-1.json")).unwrap(),
            b"receipt"
        );
        assert_eq!(
            fs::read(destination.join("browser-intake/outbox/pending-1/pages.txt")).unwrap(),
            b"outbox text"
        );
        assert!(!destination.join("packages").exists());
        assert!(!destination.join("unknown.txt").exists());
        assert!(!source.join("pairing.json").exists());
        assert!(!source.join("segments/7/video.mp4").exists());
        assert!(!source.join("segments").exists());
        assert!(!source.join("browser-intake/open/held-1/pages.txt").exists());
        assert_eq!(
            fs::read(source.join("packages/.betaId")).unwrap(),
            b"install-id"
        );
        assert_eq!(
            fs::read(source.join("packages/current/app.nupkg")).unwrap(),
            b"installer package"
        );
        assert_eq!(fs::read(source.join("unknown.txt")).unwrap(), b"unknown");
        assert_eq!(
            fs::read(source.join("unknown/nested/foreign.bin")).unwrap(),
            b"unknown nested"
        );
        assert_eq!(
            fs::read(source.join("logs/foreign.log")).unwrap(),
            b"unknown log"
        );
        assert!(source.join("logs").is_dir());
        let marker = fs::read(destination.join(COMPLETE_FILE)).unwrap();
        let CompletionInventory::Exact(adopted) = parse_completion_marker(&marker).unwrap();
        assert!(adopted.contains(&PathBuf::from("pairing.json")));
        assert!(adopted.contains(&PathBuf::from("segments/7/video.mp4")));
        assert!(!adopted.contains(&PathBuf::from("packages/.betaId")));

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn cleanup_failure_before_removal_keeps_destination_usable_and_retry_never_reseeds() {
        let source = temp_root("cleanup-before-source");
        let destination = temp_root("cleanup-before-destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("pairing.json"), b"old pairing").unwrap();
        fs::write(source.join("pause.txt"), b"paused").unwrap();

        CLEANUP_FAIL_AFTER_REMOVALS.with(|fail| fail.set(Some(0)));
        assert_eq!(adopt_between(&source, &destination).unwrap(), destination);

        assert_eq!(
            fs::read(source.join("pairing.json")).unwrap(),
            b"old pairing"
        );
        assert_eq!(fs::read(source.join("pause.txt")).unwrap(), b"paused");
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"old pairing"
        );
        assert_eq!(fs::read(destination.join("pause.txt")).unwrap(), b"paused");
        assert!(destination.join(COMPLETE_FILE).is_file());

        // A later owner write must not be overwritten from the completed
        // adoption's leftover source. Other verified leftovers still clean up.
        fs::write(destination.join("pairing.json"), b"new owner state").unwrap();
        assert_eq!(adopt_between(&source, &destination).unwrap(), destination);
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"new owner state"
        );
        assert_eq!(
            fs::read(source.join("pairing.json")).unwrap(),
            b"old pairing"
        );
        assert!(!source.join("pause.txt").exists());
        assert_eq!(existing_root_between(&source, &destination), destination);

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn cleanup_failure_after_partial_removal_retries_from_the_completion_marker() {
        let source = temp_root("cleanup-partial-source");
        let destination = temp_root("cleanup-partial-destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("pairing.json"), b"pairing").unwrap();
        fs::write(source.join("pause.txt"), b"pause").unwrap();

        CLEANUP_FAIL_AFTER_REMOVALS.with(|fail| fail.set(Some(1)));
        assert_eq!(adopt_between(&source, &destination).unwrap(), destination);

        assert!(!source.join("pairing.json").exists());
        assert_eq!(fs::read(source.join("pause.txt")).unwrap(), b"pause");
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"pairing"
        );
        assert_eq!(fs::read(destination.join("pause.txt")).unwrap(), b"pause");
        let marker_before_retry = fs::read(destination.join(COMPLETE_FILE)).unwrap();

        assert_eq!(adopt_between(&source, &destination).unwrap(), destination);
        assert!(!source.join("pause.txt").exists());
        assert_eq!(fs::read(destination.join("pause.txt")).unwrap(), b"pause");
        assert_eq!(
            fs::read(destination.join(COMPLETE_FILE)).unwrap(),
            marker_before_retry
        );
        assert_eq!(existing_root_between(&source, &destination), destination);

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn cleanup_failure_after_last_file_keeps_empty_legacy_root_non_authoritative() {
        let source = temp_root("cleanup-empty-source");
        let destination = temp_root("cleanup-empty-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let relative = PathBuf::from("pairing.json");
        fs::write(source.join(&relative), b"paired state").unwrap();
        fs::write(destination.join(&relative), b"paired state").unwrap();
        let inventory = CompletionInventory::Exact(vec![relative]);
        let marker_bytes = completion_marker_bytes(&[PathBuf::from("pairing.json")]).unwrap();
        publish_completion_marker(&destination.join(COMPLETE_FILE), &marker_bytes).unwrap();

        CLEANUP_FAIL_AFTER_REMOVALS.with(|fail| fail.set(Some(1)));
        assert!(cleanup_completed(&source, &destination, &inventory).is_err());
        assert!(fs::read_dir(&source).unwrap().next().is_none());
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"paired state"
        );
        assert_eq!(existing_root_between(&source, &destination), destination);

        // Startup retry sees the completion marker and empty source; it never
        // treats the legacy directory as an owner root or recreates pairing.
        assert!(cleanup_completed(&source, &destination, &inventory).is_ok());
        assert!(fs::read_dir(&source).unwrap().next().is_none());
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"paired state"
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn cleanup_refuses_to_remove_any_source_file_before_marker_publication() {
        let source = temp_root("cleanup-no-marker-source");
        let destination = temp_root("cleanup-no-marker-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("pairing.json"), b"source").unwrap();
        fs::write(destination.join("pairing.json"), b"source").unwrap();
        let inventory = CompletionInventory::Exact(vec![PathBuf::from("pairing.json")]);

        assert!(cleanup_completed(&source, &destination, &inventory).is_err());
        assert_eq!(fs::read(source.join("pairing.json")).unwrap(), b"source");

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn completion_inventory_refuses_unknown_paths_without_fallback_or_deletion() {
        let source = temp_root("unknown-inventory-source");
        let destination = temp_root("unknown-inventory-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("unknown.bin"), b"source bytes").unwrap();
        fs::write(destination.join("unknown.bin"), b"source bytes").unwrap();
        let marker = serde_json::json!({
            "schema": COMPLETE_SCHEMA,
            "source": COMPLETE_SOURCE,
            "files": ["unknown.bin"],
        });
        fs::write(
            destination.join(COMPLETE_FILE),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        assert_eq!(existing_root_between(&source, &destination), destination);
        assert_eq!(
            adopt_between(&source, &destination).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fs::read(source.join("unknown.bin")).unwrap(),
            b"source bytes"
        );
        assert_eq!(
            fs::read(destination.join("unknown.bin")).unwrap(),
            b"source bytes"
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn adoption_conflict_preserves_source_and_does_not_complete() {
        let source = temp_root("conflict-source");
        let destination = temp_root("conflict-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("pairing.json"), b"source").unwrap();
        fs::write(destination.join("pairing.json"), b"different").unwrap();

        let error = adopt_between(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(source.join("pairing.json")).unwrap(), b"source");
        assert!(!destination.join(COMPLETE_FILE).exists());

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn interrupted_copy_temp_is_replaced_from_the_preserved_source() {
        let source = temp_root("resume-source");
        let destination = temp_root("resume-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("pairing.json"), b"complete pairing bytes").unwrap();
        let temp = destination.join(format!(
            ".owner-adopt-{}.tmp",
            relative_key(Path::new("pairing.json"))
        ));
        fs::write(&temp, b"partial").unwrap();

        adopt_between(&source, &destination).unwrap();

        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"complete pairing bytes"
        );
        assert!(destination.join(COMPLETE_FILE).is_file());
        assert!(!temp.exists());
        assert!(!source.join("pairing.json").exists());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[cfg(unix)]
    #[test]
    fn completed_cleanup_refuses_reparse_source_and_never_follows_it() {
        use std::os::unix::fs::symlink;

        let source = temp_root("cleanup-symlink-source");
        let destination = temp_root("cleanup-symlink-destination");
        let external = temp_root("cleanup-symlink-external");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        // Match the adopted destination bytes so this test proves the cleanup
        // rejects the reparse point itself instead of merely detecting a
        // source/destination byte mismatch.
        fs::write(&external, b"paired state").unwrap();
        fs::write(destination.join("pairing.json"), b"paired state").unwrap();
        symlink(&external, source.join("pairing.json")).unwrap();
        let files = vec![PathBuf::from("pairing.json")];
        let marker_bytes = completion_marker_bytes(&files).unwrap();
        publish_completion_marker(&destination.join(COMPLETE_FILE), &marker_bytes).unwrap();

        assert!(
            cleanup_completed(&source, &destination, &CompletionInventory::Exact(files)).is_err()
        );
        assert!(fs::symlink_metadata(source.join("pairing.json"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&external).unwrap(), b"paired state");
        assert_eq!(
            fs::read(destination.join("pairing.json")).unwrap(),
            b"paired state"
        );

        let _ = fs::remove_file(source.join("pairing.json"));
        let _ = fs::remove_file(external);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[cfg(unix)]
    #[test]
    fn recognized_custody_symlink_refuses_adoption_without_completion() {
        use std::os::unix::fs::symlink;

        let source = temp_root("symlink-source");
        let destination = temp_root("symlink-destination");
        fs::create_dir_all(source.join("segments/7")).unwrap();
        let external = temp_root("symlink-payload");
        fs::write(&external, b"custody bytes").unwrap();
        symlink(&external, source.join("segments/7/audio.flac")).unwrap();

        let result = adopt_between(&source, &destination);

        assert!(result.is_err());
        assert!(!destination.join(COMPLETE_FILE).exists());
        assert!(source.join("segments/7/audio.flac").exists());
        let _ = fs::remove_file(source.join("segments/7/audio.flac"));
        let _ = fs::remove_file(external);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }
}
