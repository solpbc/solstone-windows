// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use xtask::release_clock::UtcTimestamp;
use xtask::rust_release_manifest::{companion_basename, Manifest};
use xtask::transparency_format::{
    build_transparency_entry, build_transparency_pointer, format_entry_trusted_comment,
    format_latest_trusted_comment, render_transparency_entry, render_transparency_latest,
    require_entry_trusted_comment_matches_body, require_latest_trusted_comment_matches_body,
    transparency_sha256_hex, TransparencyFormatError, TransparencyNamedDigest,
    TransparencyTipIdentity, TrustedCommentError,
};

const ENTRY_SHA256: &str = "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f";

#[test]
fn transparency_entry_inventory_equals_the_validated_manifest() {
    let manifest = fixture_manifest();
    let companion = TransparencyNamedDigest {
        name: companion_basename(),
        sha256: "e".repeat(64),
    };
    let now = UtcTimestamp::parse("2026-07-22T00:00:00Z").unwrap();
    let entry = build_transparency_entry(&manifest, &companion, &[], None, &now).unwrap();
    let actual: Vec<_> = entry
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.name.as_str(),
                artifact.sha256.as_str(),
                artifact.bytes,
            )
        })
        .collect();
    let mut expected: Vec<_> = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.path.as_str(),
                artifact.sha256.as_str(),
                artifact.bytes,
            )
        })
        .collect();
    expected.sort_by_key(|record| record.0.as_bytes());
    assert_eq!(actual, expected);
    assert_eq!(entry.artifacts.len(), 6);
    assert_eq!(entry.seq, 1);
    assert_eq!(entry.prev_version, "");
    assert_eq!(entry.prev_sha256, "0".repeat(64));
}

#[test]
fn transparency_entry_and_pointer_trusted_comments_match_the_shared_fixtures() {
    let entry: xtask::transparency_format::TransparencyLedgerEntryV1 = serde_json::from_slice(
        include_bytes!("fixtures/transparency/entry-vector.canonical.json"),
    )
    .unwrap();
    let entry_bytes = render_transparency_entry(&entry).unwrap();
    assert_eq!(transparency_sha256_hex(&entry_bytes), ENTRY_SHA256);
    let entry_comment = format_entry_trusted_comment(&entry, &entry_bytes);
    assert_eq!(
        format!("{entry_comment}\n").as_bytes(),
        include_bytes!("fixtures/transparency/entry-trusted-comment.txt")
    );
    require_entry_trusted_comment_matches_body(&entry, &entry_bytes, &entry_comment).unwrap();

    let pointer: xtask::transparency_format::TransparencyLatestV1 = serde_json::from_slice(
        include_bytes!("fixtures/transparency/latest-vector.canonical.json"),
    )
    .unwrap();
    let pointer_bytes = render_transparency_latest(&pointer).unwrap();
    assert_eq!(
        pointer_bytes,
        include_bytes!("fixtures/transparency/latest-vector.canonical.json")
    );
    let latest_comment = format_latest_trusted_comment(&pointer);
    assert_eq!(
        format!("{latest_comment}\n").as_bytes(),
        include_bytes!("fixtures/transparency/latest-trusted-comment.txt")
    );
    require_latest_trusted_comment_matches_body(&pointer, &latest_comment).unwrap();
}

#[test]
fn transparency_trusted_comment_body_disagreement_fails_semantically() {
    let entry: xtask::transparency_format::TransparencyLedgerEntryV1 = serde_json::from_slice(
        include_bytes!("fixtures/transparency/entry-vector.canonical.json"),
    )
    .unwrap();
    let body = render_transparency_entry(&entry).unwrap();
    let comment = format_entry_trusted_comment(&entry, &body).replace("seq=1", "seq=5");
    assert_eq!(
        require_entry_trusted_comment_matches_body(&entry, &body, &comment),
        Err(TrustedCommentError::SequenceMismatch)
    );
}

#[test]
fn transparency_pointer_builder_preserves_the_tip_identity() {
    let tip = TransparencyTipIdentity {
        seq: 8,
        version: "0.2.11".to_owned(),
        sha256: "a".repeat(64),
        published_utc: "2026-07-21T00:00:00Z".to_owned(),
    };
    let signed_at = UtcTimestamp::parse("2026-07-22T00:00:00Z").unwrap();
    let pointer = build_transparency_pointer(&tip, &signed_at).unwrap();
    assert_eq!(pointer.chain_length, tip.seq);
    assert_eq!(pointer.tip_sha256, tip.sha256);
    assert_eq!(pointer.version, tip.version);
    assert_eq!(pointer.valid_until, "2026-08-05T00:00:00Z");
}

#[test]
fn transparency_entry_builder_rejects_dirty_sources_non_basename_artifacts_and_old_time() {
    let companion = TransparencyNamedDigest {
        name: companion_basename(),
        sha256: "e".repeat(64),
    };
    let now = UtcTimestamp::parse("2026-07-22T00:00:00Z").unwrap();

    let mut dirty = fixture_manifest();
    dirty.source_dirty = true;
    assert_eq!(
        build_transparency_entry(&dirty, &companion, &[], None, &now),
        Err(TransparencyFormatError::DirtySource)
    );

    let mut nested = fixture_manifest();
    nested.artifacts[0].path = "nested/artifact".to_owned();
    assert_eq!(
        build_transparency_entry(&nested, &companion, &[], None, &now),
        Err(TransparencyFormatError::InvalidArtifactName)
    );

    let previous = TransparencyTipIdentity {
        seq: 1,
        version: "0.2.10".to_owned(),
        sha256: "a".repeat(64),
        published_utc: now.as_str().to_owned(),
    };
    assert_eq!(
        build_transparency_entry(&fixture_manifest(), &companion, &[], Some(&previous), &now),
        Err(TransparencyFormatError::PublicationTimeNotLater)
    );
}

fn fixture_manifest() -> Manifest {
    serde_json::from_slice(include_bytes!(
        "fixtures/rust-release-manifest/release-dir/solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json"
    ))
    .unwrap()
}
