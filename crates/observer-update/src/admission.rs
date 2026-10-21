// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Package admission policy, identity checks, and held file guards for Windows update packages.

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use xml::reader::{EventReader, XmlEvent};

/// Stock signatures are a few hundred bytes; this is the hard read cap.
pub const SIDECAR_MAX_BYTES: usize = 4096;

/// Helper trait for readable and seekable streams.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// Pluggable package signature verifier seam.
pub trait PackageVerifier {
    fn verify(&self, package: &mut (dyn ReadSeek + '_), sidecar_text: &str) -> Result<(), String>;
}

/// Package admission outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Fully verified package and sidecar, ready to stage or install.
    Ready,
    /// Final sidecar is absent (e.g. interrupted download); not ready, not deleted.
    NotReady,
    /// Corrupt, tampered, unauthenticated, or malformed package; must be retired.
    Refuse,
}

/// Checks whether an asset type represents a full update package (ASCII case-insensitive).
pub fn is_full_asset_type(asset_type: &str) -> bool {
    asset_type.eq_ignore_ascii_case("Full")
}

/// Validates that a package or sidecar filename consists of exactly one plain path segment.
pub fn check_plain_filename(file_name: &str) -> Result<(), &'static str> {
    if file_name.is_empty()
        || file_name.starts_with('.')
        || file_name.contains("..")
        || !file_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("filename must be one plain ASCII filename");
    }
    Ok(())
}

/// Builds a query-free sibling URL on the exact same origin and path prefix.
pub fn sibling_url(feed_base: &str, file_name: &str) -> Result<String, String> {
    check_plain_filename(file_name)?;

    let base_trimmed = feed_base.trim_end_matches('/');
    let base_with_slash = format!("{base_trimmed}/");

    // Simple robust URI parsing without heavy dependencies
    let (scheme, rest) = base_with_slash
        .split_once("://")
        .ok_or_else(|| "missing scheme in feed base URL".to_string())?;

    if scheme != "https" && scheme != "http" {
        return Err(format!("unsupported scheme: {scheme}"));
    }

    let (host_port, path_part) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (rest, "/".to_string()),
    };

    if host_port.is_empty() {
        return Err("empty host in feed base URL".to_string());
    }

    let path_prefix = path_part.trim_end_matches('/');
    let resolved_path = if path_prefix.is_empty() {
        format!("/{file_name}")
    } else {
        format!("{path_prefix}/{file_name}")
    };

    // Ensure resolved path starts strictly with the base path
    if !resolved_path.starts_with(path_prefix) {
        return Err("resolved path escapes base path".to_string());
    }

    Ok(format!("{scheme}://{host_port}{resolved_path}"))
}

/// Reads a sidecar signature file bounded by SIDECAR_MAX_BYTES.
/// Reads at most `SIDECAR_MAX_BYTES + 1` bytes; returns an error if oversize.
pub fn read_sidecar_bounded<R: Read>(reader: &mut R) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut take = reader.take((SIDECAR_MAX_BYTES + 1) as u64);
    take.read_to_end(&mut buf)
        .map_err(|e| format!("failed to read sidecar: {e}"))?;

    if buf.len() > SIDECAR_MAX_BYTES {
        return Err(format!(
            "sidecar signature exceeds maximum allowed size of {SIDECAR_MAX_BYTES} bytes"
        ));
    }

    String::from_utf8(buf).map_err(|e| format!("invalid UTF-8 in sidecar signature: {e}"))
}

/// Verified identity extracted from nuspec manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NuspecIdentity {
    pub id: String,
    pub version: semver::Version,
    pub channel: String,
    pub os: String,
    pub rid: String,
    pub machine_architecture: String,
    pub main_exe: String,
}

/// Parses and validates the nuspec manifest inside a package archive.
/// Requires exactly one *.nuspec entry and exact match of all identity fields.
/// Parses and validates all invariant fields of the single `.nuspec` inside the package.
pub fn parse_nuspec_identity<R: Read + Seek>(package: &mut R) -> Result<NuspecIdentity, String> {
    package
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("failed to seek package start: {e}"))?;

    let mut archive =
        zip::ZipArchive::new(package).map_err(|e| format!("invalid zip package: {e}"))?;

    let mut nuspec_index = None;
    for i in 0..archive.len() {
        let file_entry = archive
            .by_index(i)
            .map_err(|e| format!("invalid zip entry at index {i}: {e}"))?;
        let entry_name = file_entry
            .name()
            .map_err(|e| format!("invalid entry name at index {i}: {e}"))?;
        let lower = entry_name.to_ascii_lowercase();
        if lower.ends_with(".nuspec") {
            if nuspec_index.is_some() {
                return Err("package contains multiple .nuspec entries".to_string());
            }
            nuspec_index = Some(i);
        }
    }

    let nuspec_idx = nuspec_index.ok_or_else(|| "package contains no .nuspec entry".to_string())?;

    let mut nuspec_file = archive
        .by_index(nuspec_idx)
        .map_err(|e| format!("failed to read .nuspec entry: {e}"))?;

    const MAX_NUSPEC_BYTES: u64 = 65536;
    if nuspec_file.size() > MAX_NUSPEC_BYTES {
        return Err("nuspec exceeds the size limit".to_string());
    }
    let mut xml_bytes = Vec::new();
    nuspec_file
        .by_ref()
        .take(MAX_NUSPEC_BYTES + 1)
        .read_to_end(&mut xml_bytes)
        .map_err(|e| format!("failed to read .nuspec contents: {e}"))?;

    if xml_bytes.len() as u64 > MAX_NUSPEC_BYTES {
        return Err("nuspec exceeds the size limit".to_string());
    }
    let parser = EventReader::new(Cursor::new(xml_bytes));
    let mut current_element = Vec::new();

    let mut id: Option<String> = None;
    let mut version: Option<String> = None;
    let mut channel: Option<String> = None;
    let mut os: Option<String> = None;
    let mut rid: Option<String> = None;
    let mut machine_architecture: Option<String> = None;
    let mut main_exe: Option<String> = None;

    let mut seen = std::collections::BTreeSet::new();
    let identity_tag = |tag: &str| {
        matches!(
            tag,
            "id" | "version" | "channel" | "os" | "rid" | "machineArchitecture" | "mainExe"
        )
    };
    for event in parser {
        match event {
            Ok(XmlEvent::StartElement { name, .. }) => {
                if current_element
                    .last()
                    .is_some_and(|tag: &String| identity_tag(tag))
                {
                    return Err("nested identity element".to_string());
                }
                let tag = name.local_name;
                current_element.push(tag.clone());
                if identity_tag(&tag) {
                    if current_element.len() != 3
                        || current_element[0] != "package"
                        || current_element[1] != "metadata"
                    {
                        return Err("identity element outside package metadata".to_string());
                    }
                    if !seen.insert(tag) {
                        return Err("duplicate identity element".to_string());
                    }
                }
            }
            Ok(XmlEvent::EndElement { .. }) => {
                current_element.pop();
            }
            Ok(XmlEvent::Characters(text)) | Ok(XmlEvent::CData(text)) => {
                let Some(el) = current_element.last() else {
                    continue;
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match el.as_str() {
                    "id" => {
                        id.get_or_insert_with(String::new).push_str(&text);
                    }
                    "version" => {
                        version.get_or_insert_with(String::new).push_str(&text);
                    }
                    "channel" => {
                        channel.get_or_insert_with(String::new).push_str(&text);
                    }
                    "os" => {
                        os.get_or_insert_with(String::new).push_str(&text);
                    }
                    "rid" => {
                        rid.get_or_insert_with(String::new).push_str(&text);
                    }
                    "machineArchitecture" => {
                        machine_architecture
                            .get_or_insert_with(String::new)
                            .push_str(&text);
                    }
                    "mainExe" => {
                        main_exe.get_or_insert_with(String::new).push_str(&text);
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(e) => return Err(format!("malformed XML in .nuspec: {e}")),
        }
    }

    let id = id
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing id in .nuspec".to_string())?;
    let version_raw = version
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing version in .nuspec".to_string())?;
    let channel = channel
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing channel in .nuspec".to_string())?;
    let os = os
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing os in .nuspec".to_string())?;
    let rid = rid
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing rid in .nuspec".to_string())?;
    let machine_architecture = machine_architecture
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing machineArchitecture in .nuspec".to_string())?;
    let main_exe = main_exe
        .map(|value| value.trim().to_string())
        .ok_or_else(|| "missing mainExe in .nuspec".to_string())?;

    if id != "Solstone" {
        return Err(format!("unexpected package id: {id}"));
    }
    if channel != "win" {
        return Err(format!("unexpected package channel: {channel}"));
    }
    if os != "win" {
        return Err(format!("unexpected package os: {os}"));
    }
    if rid != "win-x64" {
        return Err(format!("unexpected package rid: {rid}"));
    }
    if machine_architecture != "x64" {
        return Err(format!(
            "unexpected machineArchitecture: {machine_architecture}"
        ));
    }
    if main_exe != "solstone-windows-app.exe" {
        return Err(format!("unexpected mainExe: {main_exe}"));
    }

    let parsed_ver = semver::Version::parse(&version_raw)
        .map_err(|e| format!("invalid semver in .nuspec version '{version_raw}': {e}"))?;

    Ok(NuspecIdentity {
        id,
        version: parsed_ver,
        channel,
        os,
        rid,
        machine_architecture,
        main_exe,
    })
}

/// Verifies package nuspec against required Solstone identity elements and expected version.
pub fn verify_nuspec<R: Read + Seek>(
    package: &mut R,
    expected_version: &str,
) -> Result<NuspecIdentity, String> {
    let identity = parse_nuspec_identity(package)?;
    let expected_semver = semver::Version::parse(expected_version)
        .map_err(|e| format!("invalid expected semver '{expected_version}': {e}"))?;

    if identity.version != expected_semver {
        return Err(format!(
            "manifest version '{}' does not match expected version '{expected_semver}'",
            identity.version
        ));
    }

    Ok(identity)
}

/// Evaluates package admission against signature and nuspec identity.
pub fn admit_package_reader<R: Read + Seek>(
    package: &mut R,
    sidecar_text: Option<&str>,
    expected_version: &str,
    verifier: &impl PackageVerifier,
) -> Admission {
    let Some(sidecar) = sidecar_text else {
        return Admission::NotReady;
    };

    if let Err(_e) = verifier.verify(package, sidecar) {
        return Admission::Refuse;
    }

    if let Err(_e) = verify_nuspec(package, expected_version) {
        return Admission::Refuse;
    }

    Admission::Ready
}

/// Retires an asset by deleting only its package, partial, and sidecar names.
/// Never globs other packages.
pub fn retire_asset(dir: &Path, file_name: &str) {
    if check_plain_filename(file_name).is_err() {
        return;
    }
    let target = dir.join(file_name);
    let partial = dir.join(Path::new(file_name).with_extension("partial"));
    let sig = dir.join(format!("{file_name}.minisig"));
    let sig_partial = dir.join(format!("{file_name}.minisig.partial"));

    let _ = std::fs::remove_file(target);
    let _ = std::fs::remove_file(partial);
    let _ = std::fs::remove_file(sig);
    let _ = std::fs::remove_file(sig_partial);
}

/// An authenticated cached archive, held stable through the installer handoff.
pub struct AuthenticatedPackage {
    pub path: PathBuf,
    pub identity: NuspecIdentity,
    pub held: HeldPackage,
}

/// Authenticate before interpreting cached archive metadata.
pub fn admit_cached_package(
    path: &Path,
    verifier: &impl PackageVerifier,
) -> Result<AuthenticatedPackage, Admission> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Admission::Refuse)?;
    check_plain_filename(name).map_err(|_| Admission::Refuse)?;
    let mut held = open_regular_package(path).map_err(|_| Admission::Refuse)?;
    let sidecar = path.with_file_name(format!("{name}.minisig"));
    if !sidecar.exists() {
        return Err(Admission::NotReady);
    }
    let mut signature = open_regular_package(&sidecar).map_err(|_| Admission::Refuse)?;
    let text = read_sidecar_bounded(&mut signature.file).map_err(|_| Admission::Refuse)?;
    verifier
        .verify(&mut held.file, &text)
        .map_err(|_| Admission::Refuse)?;
    let identity = parse_nuspec_identity(&mut held.file).map_err(|_| Admission::Refuse)?;
    held.file.rewind().map_err(|_| Admission::Refuse)?;
    if !held.identity_still_at(path) {
        return Err(Admission::Refuse);
    }
    Ok(AuthenticatedPackage {
        path: path.to_path_buf(),
        identity,
        held,
    })
}

pub struct CacheScan {
    pub selected: Option<AuthenticatedPackage>,
    pub rejected: bool,
}

/// Select by signed version, retaining the admitted handle and refusal state.
pub fn scan_authenticated_packages(
    dir: &Path,
    installed: &semver::Version,
    verifier: &impl PackageVerifier,
) -> CacheScan {
    let mut scan = CacheScan {
        selected: None,
        rejected: false,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            scan.rejected = error.kind() != std::io::ErrorKind::NotFound;
            return scan;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            scan.rejected = true;
            continue;
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with("-full.nupkg") || check_plain_filename(name).is_err() {
            continue;
        }
        // Setup and older clients can leave the installed package without a sidecar.
        // A canonical old filename only excludes a non-candidate; it never admits one.
        if name
            .strip_prefix("Solstone-")
            .and_then(|name| name.strip_suffix("-full.nupkg"))
            .and_then(|version| semver::Version::parse(version).ok())
            .is_some_and(|version| version <= *installed)
        {
            continue;
        }
        match admit_cached_package(&path, verifier) {
            Ok(package) if package.identity.version > *installed => {
                if scan
                    .selected
                    .as_ref()
                    .is_none_or(|current| package.identity.version > current.identity.version)
                {
                    scan.selected = Some(package);
                }
            }
            Ok(_) => {}
            Err(_) => {
                scan.rejected = true;
            }
        }
    }
    scan
}

/// File identity descriptor for held regular file verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    // Stable MetadataExt has no file index; the index compare is pl_transport_win::release_signature::windows_path_matches_held.
    #[cfg(not(any(unix, windows)))]
    len: u64,
}

/// Held package open file descriptor guarding against symlink/directory replacement races.
pub struct HeldPackage {
    pub file: File,
    pub identity: FileIdentity,
}

impl HeldPackage {
    /// Re-stats the directory entry to verify it is still the identical regular file.
    pub fn identity_still_at(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        if !meta.file_type().is_file() {
            return false;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.dev() != self.identity.dev || meta.ino() != self.identity.ino {
                return false;
            }
        }

        #[cfg(windows)]
        {
            // Stable MetadataExt has no file index; the index compare is pl_transport_win::release_signature::windows_path_matches_held.
        }

        #[cfg(not(any(unix, windows)))]
        {
            if meta.len() != self.identity.len {
                return false;
            }
        }

        true
    }
}

/// Opens a regular file read-only and binds its file identity.
/// Refuses symlinks, directories, or identity mismatches on opening.
pub fn open_regular_package(path: &Path) -> Result<HeldPackage, String> {
    let sym_meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("failed to read metadata for {}: {e}", path.display()))?;

    if !sym_meta.file_type().is_file() {
        return Err(format!(
            "package at {} is not a regular file (symlinks/directories forbidden)",
            path.display()
        ));
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ: keep the admitted bytes stable through helper spawn.
        options.share_mode(1).custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .map_err(|e| format!("failed to open package at {}: {e}", path.display()))?;

    let file_meta = file.metadata().map_err(|e| {
        format!(
            "failed to read opened file metadata for {}: {e}",
            path.display()
        )
    })?;

    if !file_meta.file_type().is_file() {
        return Err(format!(
            "opened package at {} is not a regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if sym_meta.dev() != file_meta.dev() || sym_meta.ino() != file_meta.ino() {
            return Err(
                "file identity mismatch between symlink metadata and opened file descriptor"
                    .to_string(),
            );
        }
    }

    #[cfg(windows)]
    {
        // Stable MetadataExt has no file index; the index compare is pl_transport_win::release_signature::windows_path_matches_held.
    }

    let identity = FileIdentity {
        #[cfg(unix)]
        dev: {
            use std::os::unix::fs::MetadataExt;
            sym_meta.dev()
        },
        #[cfg(unix)]
        ino: {
            use std::os::unix::fs::MetadataExt;
            sym_meta.ino()
        },
        #[cfg(not(any(unix, windows)))]
        len: sym_meta.len(),
    };

    // The held descriptor is not a claim about the helper's re-open after this process exits.
    Ok(HeldPackage { file, identity })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_nuspec_xml(
        id: &str,
        version: &str,
        channel: &str,
        os: &str,
        rid: Option<&str>,
        arch: &str,
        exe: &str,
    ) -> String {
        let rid_tag = rid.map(|r| format!("<rid>{r}</rid>")).unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2010/07/nuspec.xsd">
<metadata>
<id>{id}</id>
<version>{version}</version>
<channel>{channel}</channel>
<os>{os}</os>
{rid_tag}
<machineArchitecture>{arch}</machineArchitecture>
<mainExe>{exe}</mainExe>
</metadata>
</package>"#
        )
    }

    fn create_test_nupkg(nuspec_xml: &str, second_nuspec: bool) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Solstone.nuspec", options).unwrap();
        zip.write_all(nuspec_xml.as_bytes()).unwrap();
        if second_nuspec {
            zip.start_file("extra.nuspec", options).unwrap();
            zip.write_all(b"<package></package>").unwrap();
        }
        zip.start_file("lib/net45/solstone-windows-app.exe", options)
            .unwrap();
        zip.write_all(b"fake binary bytes").unwrap();
        zip.finish().unwrap();
        buf.into_inner()
    }

    struct MockVerifier {
        read_counter: AtomicUsize,
        should_succeed: bool,
    }

    impl MockVerifier {
        fn new(should_succeed: bool) -> Self {
            Self {
                read_counter: AtomicUsize::new(0),
                should_succeed,
            }
        }
    }

    impl PackageVerifier for MockVerifier {
        fn verify(
            &self,
            package: &mut (dyn ReadSeek + '_),
            _sidecar_text: &str,
        ) -> Result<(), String> {
            let mut buf = [0u8; 32];
            if let Ok(n) = package.read(&mut buf) {
                self.read_counter.fetch_add(n, Ordering::SeqCst);
            }
            if self.should_succeed {
                Ok(())
            } else {
                Err("mock signature verification failed".to_string())
            }
        }
    }

    #[test]
    fn unsigned_installed_packages_are_not_pending_candidates() {
        let dir = tempfile::tempdir().unwrap();
        for version in ["2.0.32", "2.0.33"] {
            std::fs::write(
                dir.path().join(format!("Solstone-{version}-full.nupkg")),
                b"unsigned Setup cache",
            )
            .unwrap();
        }
        let installed = semver::Version::parse("2.0.33").unwrap();
        let verifier = MockVerifier::new(false);
        let scan = scan_authenticated_packages(dir.path(), &installed, &verifier);
        assert!(scan.selected.is_none());
        assert!(!scan.rejected);
        assert_eq!(verifier.read_counter.load(Ordering::SeqCst), 0);
        std::fs::write(
            dir.path().join("Solstone-2.0.34-full.nupkg"),
            b"unsigned pending",
        )
        .unwrap();
        let scan = scan_authenticated_packages(dir.path(), &installed, &verifier);
        assert!(scan.selected.is_none());
        assert!(scan.rejected);
    }

    #[test]
    fn test_plain_name_and_sibling_url_positive_and_negative() {
        assert!(check_plain_filename("Solstone-1.2.3-full.nupkg").is_ok());
        assert!(check_plain_filename("Solstone-1.2.3-1.2.2-delta.nupkg").is_ok());

        assert!(check_plain_filename("").is_err());
        assert!(check_plain_filename("   ").is_err());
        assert!(check_plain_filename("Solstone%20App-1.2.3-full.nupkg").is_err());
        assert!(check_plain_filename("subdir/Solstone-1.2.3-full.nupkg").is_err());
        assert!(check_plain_filename("subdir\\Solstone-1.2.3-full.nupkg").is_err());
        assert!(check_plain_filename("../Solstone-1.2.3-full.nupkg").is_err());
        assert!(check_plain_filename("/Solstone-1.2.3-full.nupkg").is_err());
        assert!(check_plain_filename("//evil.example/pkg.nupkg").is_err());
        assert!(check_plain_filename("C:\\pkg.nupkg").is_err());
        assert!(check_plain_filename("pkg.nupkg?query=1").is_err());
        assert!(check_plain_filename("pkg.nupkg#frag").is_err());

        let base = "https://updates.solstone.app/solstone-windows";
        let url = sibling_url(base, "Solstone-1.2.3-full.nupkg").unwrap();
        assert_eq!(
            url,
            "https://updates.solstone.app/solstone-windows/Solstone-1.2.3-full.nupkg"
        );

        let sig_name = format!("{}.minisig", "Solstone-1.2.3-full.nupkg");
        let sig_url = sibling_url(base, &sig_name).unwrap();
        assert_eq!(
            sig_url,
            "https://updates.solstone.app/solstone-windows/Solstone-1.2.3-full.nupkg.minisig"
        );

        assert!(sibling_url(base, "Solstone%20App.nupkg").is_err());
        assert!(sibling_url(base, "../escape.nupkg").is_err());
        assert!(sibling_url(base, "/escape.nupkg").is_err());
        assert!(sibling_url(base, "pkg.nupkg?query=1").is_err());
    }

    #[test]
    fn test_nuspec_parser_positive_and_negative() {
        let valid_xml = make_nuspec_xml(
            "Solstone",
            "2.0.34",
            "win",
            "win",
            Some("win-x64"),
            "x64",
            "solstone-windows-app.exe",
        );
        let valid_bytes = create_test_nupkg(&valid_xml, false);

        let mut reader = Cursor::new(&valid_bytes);
        let id = verify_nuspec(&mut reader, "2.0.34").unwrap();
        assert_eq!(id.id, "Solstone");
        assert_eq!(id.version.to_string(), "2.0.34");
        assert_eq!(id.rid, "win-x64");

        // Two nuspec entries refuse
        let two_nuspec = create_test_nupkg(&valid_xml, true);
        assert!(verify_nuspec(&mut Cursor::new(&two_nuspec), "2.0.34").is_err());

        // Duplicate id refuses
        let dup_id_xml = r#"<package><metadata><id>Solstone</id><id>Solstone2</id><version>2.0.34</version><channel>win</channel><os>win</os><rid>win-x64</rid><machineArchitecture>x64</machineArchitecture><mainExe>solstone-windows-app.exe</mainExe></metadata></package>"#.to_string();
        assert!(verify_nuspec(
            &mut Cursor::new(&create_test_nupkg(&dup_id_xml, false)),
            "2.0.34"
        )
        .is_err());

        // Wrong version string refuses
        assert!(verify_nuspec(&mut Cursor::new(&valid_bytes), "2.0.35").is_err());

        // Missing rid refuses
        let no_rid_xml = make_nuspec_xml(
            "Solstone",
            "2.0.34",
            "win",
            "win",
            None,
            "x64",
            "solstone-windows-app.exe",
        );
        assert!(verify_nuspec(
            &mut Cursor::new(&create_test_nupkg(&no_rid_xml, false)),
            "2.0.34"
        )
        .is_err());

        // Wrong id refuses
        let wrong_id_xml = make_nuspec_xml(
            "WrongApp",
            "2.0.34",
            "win",
            "win",
            Some("win-x64"),
            "x64",
            "solstone-windows-app.exe",
        );
        assert!(verify_nuspec(
            &mut Cursor::new(&create_test_nupkg(&wrong_id_xml, false)),
            "2.0.34"
        )
        .is_err());
    }

    #[test]
    fn test_sidecar_cap_and_bounded_read() {
        let small_sidecar = "untrusted comment: test\nRU...\n";
        let read = read_sidecar_bounded(&mut small_sidecar.as_bytes()).unwrap();
        assert_eq!(read, small_sidecar);

        let oversize = vec![b'a'; SIDECAR_MAX_BYTES + 1];
        let err = read_sidecar_bounded(&mut oversize.as_slice());
        assert!(err.is_err());

        // If sidecar read fails before verify, package read counter stays 0
        let verifier = MockVerifier::new(true);
        let mut pkg = Cursor::new(b"package bytes");
        let admission = admit_package_reader(&mut pkg, None, "2.0.34", &verifier);
        assert_eq!(admission, Admission::NotReady);
        assert_eq!(verifier.read_counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_incomplete_sidecar_admission_and_retire() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_path = dir.path().join("Solstone-2.0.34-full.nupkg");
        let partial_path = dir.path().join("Solstone-2.0.34-full.partial");
        let sig_path = dir.path().join("Solstone-2.0.34-full.nupkg.minisig");
        let sig_partial_path = dir
            .path()
            .join("Solstone-2.0.34-full.nupkg.minisig.partial");

        let second_pkg = dir.path().join("Solstone-2.0.33-full.nupkg");
        File::create(&second_pkg).unwrap();

        let valid_xml = make_nuspec_xml(
            "Solstone",
            "2.0.34",
            "win",
            "win",
            Some("win-x64"),
            "x64",
            "solstone-windows-app.exe",
        );
        let bytes = create_test_nupkg(&valid_xml, false);
        std::fs::write(&pkg_path, &bytes).unwrap();
        std::fs::write(&partial_path, b"partial").unwrap();
        std::fs::write(&sig_partial_path, b"temp sig").unwrap();

        let verifier = MockVerifier::new(true);

        // When final sidecar is absent: NotReady
        let mut f = File::open(&pkg_path).unwrap();
        let admission = admit_package_reader(&mut f, None, "2.0.34", &verifier);
        assert_eq!(admission, Admission::NotReady);

        // Write final sidecar: Ready
        std::fs::write(&sig_path, "valid signature").unwrap();
        let mut f2 = File::open(&pkg_path).unwrap();
        let admission2 =
            admit_package_reader(&mut f2, Some("valid signature"), "2.0.34", &verifier);
        assert_eq!(admission2, Admission::Ready);

        // Retire asset deletes only the target's four filenames
        retire_asset(dir.path(), "Solstone-2.0.34-full.nupkg");
        assert!(!pkg_path.exists());
        assert!(!partial_path.exists());
        assert!(!sig_path.exists());
        assert!(!sig_partial_path.exists());

        // Second nupkg stays untouched
        assert!(second_pkg.exists());
    }

    #[test]
    fn unsafe_cleanup_names_leave_outside_files_untouched() {
        let root = tempfile::tempdir().unwrap();
        let packages = root.path().join("packages");
        std::fs::create_dir(&packages).unwrap();
        let sentinel = root.path().join("pairing.json");
        std::fs::write(&sentinel, b"keep pairing").unwrap();
        for name in [
            "../pairing.json",
            r"..\pairing.json",
            "x:stream",
            " file.nupkg",
            ".hidden",
        ] {
            assert!(check_plain_filename(name).is_err());
            retire_asset(&packages, name);
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep pairing");
        }
    }

    #[cfg(windows)]
    #[test]
    fn held_package_denies_mutation_and_removal_before_spawn() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("pending.nupkg");
        std::fs::write(&file, b"admitted bytes").unwrap();
        let held = open_regular_package(&file).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&file).is_err());
        assert!(std::fs::remove_file(&file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"admitted bytes");
        drop(held);
        std::fs::OpenOptions::new().write(true).open(&file).unwrap();
    }

    #[test]
    fn test_held_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let real_file = dir.path().join("test.nupkg");
        std::fs::write(&real_file, b"test content").unwrap();

        let held = open_regular_package(&real_file).unwrap();
        assert!(held.identity_still_at(&real_file));

        // After replacing the directory entry with new content/inode
        std::fs::remove_file(&real_file).unwrap();
        std::fs::write(&real_file, b"different content").unwrap();

        #[cfg(unix)]
        {
            // On Unix, a newly created file gets a different inode
            assert!(!held.identity_still_at(&real_file));
        }

        // Symlink refused
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = dir.path().join("target.txt");
            let link = dir.path().join("link.txt");
            std::fs::write(&target, b"target").unwrap();
            symlink(&target, &link).unwrap();
            assert!(open_regular_package(&link).is_err());
        }
    }
}
