// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::{Cursor, Write};

use xtask::release_container::{
    compare_executable_baseline, BaselineSource, ContainerKind, ExecutableContainerReader,
    ReleaseContainerError,
};
use xtask::rust_release_manifest::PackagedExecutableEvidence;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const NUPKG_MEMBER: &str = "lib/app/solstone-windows-app.exe";
const PORTABLE_MEMBER: &str = "current/solstone-windows-app.exe";
const UPPER_NUPKG_MEMBER: &str = "LIB/APP/SOLSTONE-WINDOWS-APP.EXE";
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;

#[test]
fn exact_nupkg_and_portable_members_produce_the_expected_evidence() {
    let nupkg = build_zip(|writer| {
        add_file(
            writer,
            "metadata/info.json",
            b"{}",
            CompressionMethod::Stored,
        );
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
    });
    let portable = build_zip(|writer| {
        add_file(writer, PORTABLE_MEMBER, b"abc", CompressionMethod::Deflated);
    });

    let expected = evidence(
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        3,
    );
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&nupkg).expect("read exact nupkg member"),
        expected
    );
    assert_eq!(
        ExecutableContainerReader::read_portable(&portable).expect("read exact portable member"),
        expected
    );
}

#[test]
fn missing_and_exact_duplicate_canonical_members_are_distinct_errors() {
    let missing = build_zip(|writer| {
        add_file(
            writer,
            "metadata/info.json",
            b"{}",
            CompressionMethod::Stored,
        );
    });
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&missing).expect_err("missing member must fail"),
        ReleaseContainerError::MissingCanonicalMember {
            container: ContainerKind::Nupkg,
        }
    );

    let mut duplicate = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
        add_file(
            writer,
            UPPER_NUPKG_MEMBER,
            b"abc",
            CompressionMethod::Stored,
        );
    });
    rename_entry(&mut duplicate, UPPER_NUPKG_MEMBER, NUPKG_MEMBER);
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&duplicate)
            .expect_err("duplicate canonical member must fail"),
        ReleaseContainerError::DuplicateCanonicalMember {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn case_fold_duplicate_member_is_rejected() {
    let archive = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
        add_file(
            writer,
            UPPER_NUPKG_MEMBER,
            b"abc",
            CompressionMethod::Stored,
        );
    });

    assert_eq!(
        ExecutableContainerReader::read_nupkg(&archive).expect_err("case-fold duplicate must fail"),
        ReleaseContainerError::EntryCaseCollision {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn directory_at_the_canonical_member_is_rejected() {
    let archive = build_zip(|writer| {
        writer
            .add_directory(NUPKG_MEMBER, safe_options(CompressionMethod::Stored))
            .expect("add target directory");
    });

    assert_eq!(
        ExecutableContainerReader::read_nupkg(&archive).expect_err("target directory must fail"),
        ReleaseContainerError::CanonicalMemberIsDirectory {
            container: ContainerKind::Nupkg,
        }
    );

    let mut mode_directory = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
    });
    set_external_attributes(&mut mode_directory, NUPKG_MEMBER, 0o040_755 << 16);
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&mode_directory)
            .expect_err("directory mode at target must fail"),
        ReleaseContainerError::CanonicalMemberIsDirectory {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn encrypted_entry_is_rejected() {
    let mut archive = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
    });
    set_encrypted_flag(&mut archive, NUPKG_MEMBER);

    assert_eq!(
        ExecutableContainerReader::read_nupkg(&archive).expect_err("encrypted entry must fail"),
        ReleaseContainerError::EncryptedEntry {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn symlink_and_unsafe_permission_modes_are_rejected() {
    let symlink = build_zip(|writer| {
        writer
            .add_symlink(
                NUPKG_MEMBER,
                "elsewhere.exe",
                safe_options(CompressionMethod::Stored),
            )
            .expect("add symlink entry");
    });
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&symlink).expect_err("symlink member must fail"),
        ReleaseContainerError::UnsafeUnixMode {
            container: ContainerKind::Nupkg,
        }
    );

    let mut unsafe_mode = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
    });
    set_external_attributes(&mut unsafe_mode, NUPKG_MEMBER, 0o104_755 << 16);
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&unsafe_mode).expect_err("set-id mode must fail"),
        ReleaseContainerError::UnsafeUnixMode {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn backslash_and_traversal_names_are_rejected_even_when_the_target_is_present() {
    for unsafe_name in ["unsafe\\entry.bin", "../escape.bin"] {
        let archive = build_zip(|writer| {
            add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
            add_file(writer, unsafe_name, b"unsafe", CompressionMethod::Stored);
        });
        assert_eq!(
            ExecutableContainerReader::read_nupkg(&archive)
                .expect_err("unsafe entry name must fail"),
            ReleaseContainerError::InvalidEntryName {
                container: ContainerKind::Nupkg,
            }
        );
    }
}

#[test]
fn zero_byte_member_and_reported_size_mismatch_are_distinct_errors() {
    let empty = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"", CompressionMethod::Stored);
    });
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&empty).expect_err("empty member must fail"),
        ReleaseContainerError::EmptyCanonicalMember {
            container: ContainerKind::Nupkg,
        }
    );

    let mut wrong_size = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
    });
    set_reported_size(&mut wrong_size, NUPKG_MEMBER, 4);
    assert_eq!(
        ExecutableContainerReader::read_nupkg(&wrong_size)
            .expect_err("reported size mismatch must fail"),
        ReleaseContainerError::CanonicalMemberSizeMismatch {
            container: ContainerKind::Nupkg,
        }
    );
}

#[test]
fn comparator_returns_one_baseline_for_three_identical_sources() {
    let baseline = evidence(&"a".repeat(64), 123);

    assert_eq!(
        compare_executable_baseline(&baseline, &baseline, &baseline)
            .expect("identical sources agree"),
        baseline
    );
}

#[test]
fn comparator_names_each_single_diverging_source() {
    let common = evidence(&"a".repeat(64), 123);
    let divergent_hash = evidence(&"b".repeat(64), 123);
    let divergent_size = evidence(&"a".repeat(64), 124);
    let cases = [
        (
            &divergent_hash,
            &common,
            &common,
            BaselineSource::Nupkg,
            "nupkg",
        ),
        (
            &common,
            &divergent_hash,
            &common,
            BaselineSource::Portable,
            "portable",
        ),
        (
            &common,
            &common,
            &divergent_size,
            BaselineSource::Staged,
            "staged",
        ),
    ];

    for (nupkg, portable, staged, source, label) in cases {
        let error = compare_executable_baseline(nupkg, portable, staged)
            .expect_err("one-source divergence must fail");
        assert_eq!(error, ReleaseContainerError::BaselineDiverged { source });
        let diagnostic = error.to_string();
        assert!(diagnostic.contains(label));
        assert!(diagnostic.contains("rebuild both containers in this transaction"));
    }
}

#[test]
fn diagnostics_do_not_echo_private_archive_names_or_machine_paths() {
    let private_canary = "/home/private-account/private-build/app.exe";
    let archive = build_zip(|writer| {
        add_file(writer, NUPKG_MEMBER, b"abc", CompressionMethod::Stored);
        add_file(
            writer,
            private_canary,
            b"private",
            CompressionMethod::Stored,
        );
    });

    let diagnostic = ExecutableContainerReader::read_nupkg(&archive)
        .expect_err("absolute private path must fail")
        .to_string();
    assert!(!diagnostic.contains(private_canary));
    assert!(!diagnostic.contains("private-account"));
    assert!(diagnostic.contains("portable relative entry names"));
}

fn evidence(sha256: &str, bytes: u64) -> PackagedExecutableEvidence {
    PackagedExecutableEvidence {
        sha256: sha256.to_owned(),
        bytes,
    }
}

fn safe_options(compression: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(compression)
        .unix_permissions(0o644)
}

fn add_file(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    bytes: &[u8],
    compression: CompressionMethod,
) {
    writer
        .start_file(name, safe_options(compression))
        .expect("start inert archive entry");
    writer.write_all(bytes).expect("write inert archive entry");
}

fn build_zip(build: impl FnOnce(&mut ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    build(&mut writer);
    writer.finish().expect("finish inert archive").into_inner()
}

fn rename_entry(bytes: &mut [u8], old_name: &str, new_name: &str) {
    assert_eq!(old_name.len(), new_name.len());
    let central = central_entry_offset(bytes, old_name);
    let local = usize::try_from(read_u32(bytes, central + 42)).expect("local offset fits usize");
    assert_eq!(read_u32(bytes, local), 0x0403_4b50);
    bytes[central + 46..central + 46 + old_name.len()].copy_from_slice(new_name.as_bytes());
    bytes[local + 30..local + 30 + old_name.len()].copy_from_slice(new_name.as_bytes());
}

fn set_encrypted_flag(bytes: &mut [u8], name: &str) {
    let central = central_entry_offset(bytes, name);
    let local = usize::try_from(read_u32(bytes, central + 42)).expect("local offset fits usize");
    let central_flags = read_u16(bytes, central + 8) | 1;
    let local_flags = read_u16(bytes, local + 6) | 1;
    bytes[central + 8..central + 10].copy_from_slice(&central_flags.to_le_bytes());
    bytes[local + 6..local + 8].copy_from_slice(&local_flags.to_le_bytes());
}

fn set_external_attributes(bytes: &mut [u8], name: &str, attributes: u32) {
    let central = central_entry_offset(bytes, name);
    bytes[central + 38..central + 42].copy_from_slice(&attributes.to_le_bytes());
}

fn set_reported_size(bytes: &mut [u8], name: &str, size: u32) {
    let central = central_entry_offset(bytes, name);
    bytes[central + 24..central + 28].copy_from_slice(&size.to_le_bytes());
}

fn central_entry_offset(bytes: &[u8], target_name: &str) -> usize {
    let archive = ZipArchive::new(Cursor::new(bytes)).expect("open inert archive");
    let mut position = usize::try_from(archive.central_directory_start())
        .expect("central directory offset fits usize");
    while read_u32(bytes, position) == CENTRAL_HEADER_SIGNATURE {
        let name_len = usize::from(read_u16(bytes, position + 28));
        let extra_len = usize::from(read_u16(bytes, position + 30));
        let comment_len = usize::from(read_u16(bytes, position + 32));
        let name = std::str::from_utf8(&bytes[position + 46..position + 46 + name_len])
            .expect("inert entry name is UTF-8");
        if name == target_name {
            return position;
        }
        position += 46 + name_len + extra_len + comment_len;
    }
    panic!("target central entry not found");
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("two bytes"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}
