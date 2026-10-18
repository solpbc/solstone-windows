// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::{Cursor, Write};

use velopack::bundle::load_bundle_from_memory;
use zip::{write::SimpleFileOptions, ZipWriter};

fn inert_package() -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("Solstone.nuspec", options).unwrap();
    writer
        .write_all(
            br"<package><metadata><id>Solstone</id><version>2.0.28</version><mainExe>solstone-windows-app.exe</mainExe></metadata></package>",
        )
        .unwrap();
    writer.start_file("lib/app/Squirrel.exe", options).unwrap();
    writer.write_all(b"inert updater fixture").unwrap();
    writer.finish().unwrap().into_inner()
}

#[test]
fn patched_velopack_reads_manifest_and_copies_only_the_selected_entry() {
    let bytes = inert_package();
    let mut bundle = load_bundle_from_memory(&bytes).unwrap();
    let manifest = bundle.read_manifest().unwrap();
    assert_eq!(manifest.id, "Solstone");
    assert_eq!(manifest.version.to_string(), "2.0.28");
    assert_eq!(
        bundle.get_file_names().unwrap(),
        ["Solstone.nuspec", "lib/app/Squirrel.exe"]
    );
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("Update.exe");
    bundle
        .extract_zip_predicate_to_path(|name| name.ends_with("Squirrel.exe"), &output)
        .unwrap();
    assert_eq!(std::fs::read(output).unwrap(), b"inert updater fixture");
    let missing = directory.path().join("missing.exe");
    assert!(bundle
        .extract_zip_predicate_to_path(|name| name == "missing", &missing)
        .is_err());
    assert!(!missing.exists());
}

#[test]
fn malformed_package_is_rejected_without_an_output_file() {
    let mut bytes = inert_package();
    bytes.truncate(bytes.len() / 2);
    assert!(load_bundle_from_memory(&bytes).is_err());
}

#[test]
fn legacy_codepage_names_still_read_and_copy_to_the_explicit_destination() {
    let mut bytes = inert_package();
    let mut changed = 0;
    for index in 0..bytes.len().saturating_sub(46) {
        let (flags, name) = match bytes.get(index..index + 4) {
            Some(b"PK\x03\x04") => (index + 6, index + 30),
            Some(b"PK\x01\x02") => (index + 8, index + 46),
            _ => continue,
        };
        bytes[flags + 1] &= !8; // Legacy codepage name.
        bytes[name] = 0x82; // CP437 e-acute.
        changed += 1;
    }
    assert_eq!(changed, 4); // Two entries, each with local and central names.
    let mut bundle = load_bundle_from_memory(&bytes).unwrap();
    assert_eq!(
        bundle.get_file_names().unwrap(),
        ["éolstone.nuspec", "éib/app/Squirrel.exe"]
    );
    assert_eq!(bundle.read_manifest().unwrap().id, "Solstone");
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("Update.exe");
    bundle
        .extract_zip_predicate_to_path(|name| name.ends_with("Squirrel.exe"), &output)
        .unwrap();
    assert_eq!(std::fs::read(output).unwrap(), b"inert updater fixture");
}
