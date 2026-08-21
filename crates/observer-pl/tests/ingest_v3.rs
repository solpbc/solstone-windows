// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;

use observer_pl::ingest::{
    prove_custody, CustodyFailure, CustodyProof, CustodySource, DayManifest, DayManifestSegment,
    FilePart, IngestManifest, IngestMultipart, IngestMultipartError, IngestResponse, IngestStatus,
    LocalFile, ManifestDay, SegmentFile, SegmentFileStatus, SegmentItem, SegmentsEnvelope,
};

fn file(
    name: &str,
    submitted_name: Option<&str>,
    sha256: &str,
    size: u64,
    status: SegmentFileStatus,
) -> SegmentFile {
    SegmentFile {
        name: name.to_owned(),
        submitted_name: submitted_name.map(ToOwned::to_owned),
        sha256: sha256.to_owned(),
        size,
        status,
    }
}

fn proof_input(
    day: &str,
    key: &str,
    files: Vec<SegmentFile>,
) -> (
    IngestManifest,
    DayManifest,
    SegmentsEnvelope,
    Vec<LocalFile<'static>>,
) {
    let local = vec![
        LocalFile {
            name: "screen-unique.mp4",
            sha256: "screen-sha-001",
            size: 101,
        },
        LocalFile {
            name: "audio-unique.flac",
            sha256: "audio-sha-002",
            size: 202,
        },
    ];
    let manifest = IngestManifest {
        days: BTreeMap::from([(day.to_owned(), ManifestDay { segments: 17 })]),
    };
    let day_manifest = DayManifest {
        version: 1,
        day: day.to_owned(),
        segments: BTreeMap::from([(
            key.to_owned(),
            DayManifestSegment {
                files: files.clone(),
            },
        )]),
    };
    let segments = SegmentsEnvelope {
        total: 1,
        protocol_version: 3,
        items: vec![SegmentItem {
            key: key.to_owned(),
            observed: false,
            files,
            original_key: None,
        }],
    };
    (manifest, day_manifest, segments, local)
}

fn valid_files() -> Vec<SegmentFile> {
    vec![
        file(
            "stored-screen.mp4",
            Some("screen-unique.mp4"),
            "screen-sha-001",
            101,
            SegmentFileStatus::Present,
        ),
        file(
            "audio-unique.flac",
            None,
            "audio-sha-002",
            202,
            SegmentFileStatus::Processed,
        ),
    ]
}

#[test]
fn multipart_derives_the_envelope_file_list_and_uses_a_text_envelope_part() {
    let multipart = IngestMultipart::new(
        "v3-boundary",
        "20260820",
        "080000_600",
        vec![
            FilePart {
                filename: "screen-unique.mp4".into(),
                content_type: "video/mp4".into(),
                bytes: b"screen".to_vec(),
            },
            FilePart {
                filename: "audio-unique.flac".into(),
                content_type: "audio/flac".into(),
                bytes: b"audio".to_vec(),
            },
        ],
    )
    .unwrap();

    let body = String::from_utf8(multipart.serialize().unwrap()).unwrap();
    assert_eq!(
        multipart.content_type(),
        "multipart/form-data; boundary=v3-boundary"
    );
    assert!(body.contains(
        "name=\"envelope\"\r\nContent-Type: application/json\r\n\r\n{\"day\":\"20260820\",\"segment\":\"080000_600\",\"files\":[{\"submitted\":\"screen-unique.mp4\"},{\"submitted\":\"audio-unique.flac\"}]}"
    ));
    assert!(!body.contains("name=\"envelope\"; filename="));
    assert!(!body.contains("platform"));
    assert_eq!(body.matches("name=\"files\"").count(), 2);
}

#[test]
fn multipart_rejects_a_duplicate_filename_before_serialization() {
    let result = IngestMultipart::new(
        "b",
        "20260821",
        "081000_600",
        vec![
            FilePart {
                filename: "duplicate.wav".into(),
                content_type: "audio/wav".into(),
                bytes: vec![1],
            },
            FilePart {
                filename: "duplicate.wav".into(),
                content_type: "audio/wav".into(),
                bytes: vec![2],
            },
        ],
    );
    assert_eq!(
        result.unwrap_err(),
        IngestMultipartError::DuplicateFilename("duplicate.wav".into())
    );
}

#[test]
fn multipart_rejects_an_empty_file_list_before_serialization() {
    assert_eq!(
        IngestMultipart::new("b", "20260821", "081000_600", Vec::new()).unwrap_err(),
        IngestMultipartError::EmptyFiles
    );
}

#[test]
fn strict_v3_models_reject_missing_or_unknown_custody_fields() {
    let unknown = serde_json::from_str::<SegmentsEnvelope>(
        r#"{"items":[{"key":"082000_600","observed":false,"files":[{"name":"f","size":1,"sha256":"a","status":"quarantined"}]}],"total":1,"protocol_version":3}"#,
    );
    assert!(unknown.is_err());
    let missing = serde_json::from_str::<SegmentsEnvelope>(
        r#"{"items":[{"key":"082000_600","observed":false,"files":[{"name":"f","size":1,"sha256":"a"}]}],"total":1,"protocol_version":3}"#,
    );
    assert!(missing.is_err());
    assert!(serde_json::from_str::<SegmentsEnvelope>("[]").is_err());
}

#[test]
fn ingest_status_acceptance_is_closed() {
    assert!(IngestStatus::Ok.is_accepted());
    assert!(IngestStatus::Duplicate.is_accepted());
    assert!(IngestStatus::Collision.is_accepted());
    assert!(!IngestStatus::Conflict.is_accepted());
    assert!(!IngestStatus::Failed.is_accepted());
}

#[test]
fn ingest_response_rejects_an_unknown_status() {
    assert!(serde_json::from_str::<IngestResponse>(r#"{"status":"quarantined"}"#).is_err());
}

#[test]
fn root_and_day_manifest_require_their_v3_fields() {
    assert!(serde_json::from_str::<IngestManifest>(r#"{}"#).is_err());
    assert!(serde_json::from_str::<DayManifest>(r#"{"day":"20260902"}"#).is_err());
    assert!(serde_json::from_str::<DayManifest>(r#"{"version":1,"segments":{}}"#).is_err());
}

#[test]
fn submitted_or_name_uses_the_written_name_only_when_not_renamed() {
    assert_eq!(
        file(
            "written.flac",
            None,
            "sha-written",
            401,
            SegmentFileStatus::Present
        )
        .submitted_or_name(),
        "written.flac"
    );
    assert_eq!(
        file(
            "written.flac",
            Some("submitted.flac"),
            "sha-submitted",
            402,
            SegmentFileStatus::Processed,
        )
        .submitted_or_name(),
        "submitted.flac"
    );
}

#[test]
fn proof_rejects_a_missing_file_from_the_segments_listing() {
    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260903", "094000_600", valid_files());
    segments.items[0].files[0] = file(
        "wrong-screen.mp4",
        None,
        "wrong-sha-404",
        404,
        SegmentFileStatus::Present,
    );
    assert_eq!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260903",
            "094000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::FileMissing {
            source: CustodySource::Segments,
            name: "screen-unique.mp4".into(),
        })
    );
}

#[test]
fn proof_accepts_processed_custody_in_both_documents() {
    let (manifest, day_manifest, segments, local) =
        proof_input("20260904", "095000_600", valid_files());
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260904",
            "095000_600",
            &local
        ),
        CustodyProof::Confirmed(_)
    ));
}

#[test]
fn proof_returns_the_server_key_only_after_all_documents_agree() {
    let (manifest, day_manifest, segments, local) =
        proof_input("20260822", "080000_600", valid_files());
    let proof = prove_custody(
        &manifest,
        &day_manifest,
        &segments,
        "20260822",
        "080000_600",
        &local,
    );
    let CustodyProof::Confirmed(witness) = proof else {
        panic!("expected complete custody witness");
    };
    assert_eq!(witness.server_segment(), "080000_600");
}

#[test]
fn proof_fails_for_each_document_invariant() {
    let (mut manifest, day_manifest, segments, local) =
        proof_input("20260823", "081000_600", valid_files());
    manifest.days.clear();
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260823",
            "081000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::DayAbsentFromRootManifest { .. })
    ));

    let (manifest, mut day_manifest, segments, local) =
        proof_input("20260824", "082000_600", valid_files());
    day_manifest.day = "20260825".into();
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260824",
            "082000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::DayManifestDayMismatch { .. })
    ));

    let (manifest, mut day_manifest, segments, local) =
        proof_input("20260826", "083000_600", valid_files());
    day_manifest.segments.clear();
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260826",
            "083000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::SegmentAbsentFromDayManifest { .. })
    ));

    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260827", "084000_600", valid_files());
    segments.protocol_version = 4;
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260827",
            "084000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::ProtocolVersionMismatch { actual: 4 })
    ));

    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260828", "085000_600", valid_files());
    segments.total = 2;
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260828",
            "085000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::SegmentsTotalMismatch {
            total: 2,
            item_count: 1
        })
    ));

    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260829", "090000_600", valid_files());
    segments.items.clear();
    segments.total = 0;
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260829",
            "090000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::SegmentAbsentFromSegments { .. })
    ));
}

#[test]
fn proof_fails_with_distinct_file_reasons() {
    let cases = [
        (
            "missing",
            vec![
                file(
                    "other.bin",
                    None,
                    "other-sha",
                    303,
                    SegmentFileStatus::Present,
                ),
                file(
                    "another.bin",
                    None,
                    "another-sha",
                    404,
                    SegmentFileStatus::Processed,
                ),
            ],
            CustodyFailure::FileMissing {
                source: CustodySource::DayManifest,
                name: "screen-unique.mp4".into(),
            },
        ),
        (
            "renamed",
            vec![
                file(
                    "screen-unique.mp4",
                    Some("renamed-screen.mp4"),
                    "screen-sha-001",
                    101,
                    SegmentFileStatus::Present,
                ),
                file(
                    "audio-unique.flac",
                    None,
                    "audio-sha-002",
                    202,
                    SegmentFileStatus::Processed,
                ),
            ],
            CustodyFailure::FileRenamed {
                source: CustodySource::DayManifest,
                expected: "screen-unique.mp4".into(),
                actual: "renamed-screen.mp4".into(),
            },
        ),
        (
            "hash",
            vec![
                file(
                    "stored-screen.mp4",
                    Some("screen-unique.mp4"),
                    "screen-sha-wrong",
                    101,
                    SegmentFileStatus::Present,
                ),
                file(
                    "audio-unique.flac",
                    None,
                    "audio-sha-002",
                    202,
                    SegmentFileStatus::Processed,
                ),
            ],
            CustodyFailure::FileSha256Mismatch {
                source: CustodySource::DayManifest,
                name: "screen-unique.mp4".into(),
                expected: "screen-sha-001".into(),
                actual: "screen-sha-wrong".into(),
            },
        ),
        (
            "size",
            vec![
                file(
                    "stored-screen.mp4",
                    Some("screen-unique.mp4"),
                    "screen-sha-001",
                    111,
                    SegmentFileStatus::Present,
                ),
                file(
                    "audio-unique.flac",
                    None,
                    "audio-sha-002",
                    202,
                    SegmentFileStatus::Processed,
                ),
            ],
            CustodyFailure::FileSizeMismatch {
                source: CustodySource::DayManifest,
                name: "screen-unique.mp4".into(),
                expected: 101,
                actual: 111,
            },
        ),
        (
            "nonterminal",
            vec![
                file(
                    "stored-screen.mp4",
                    Some("screen-unique.mp4"),
                    "screen-sha-001",
                    101,
                    SegmentFileStatus::Missing,
                ),
                file(
                    "audio-unique.flac",
                    None,
                    "audio-sha-002",
                    202,
                    SegmentFileStatus::Processed,
                ),
            ],
            CustodyFailure::FileCustodyNotTerminal {
                source: CustodySource::DayManifest,
                name: "screen-unique.mp4".into(),
                status: SegmentFileStatus::Missing,
            },
        ),
    ];

    for (case, files, expected) in cases {
        let (manifest, day_manifest, segments, local) =
            proof_input("20260830", "091000_600", files);
        assert_eq!(
            prove_custody(
                &manifest,
                &day_manifest,
                &segments,
                "20260830",
                "091000_600",
                &local
            ),
            CustodyProof::Unconfirmed(expected),
            "{case}"
        );
    }
}

#[test]
fn proof_rejects_an_extra_file_with_a_count_mismatch() {
    let mut files = valid_files();
    files.push(file(
        "extra.bin",
        None,
        "extra-sha-303",
        303,
        SegmentFileStatus::Present,
    ));
    let (manifest, day_manifest, segments, local) = proof_input("20260901", "093000_600", files);
    assert_eq!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260901",
            "093000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::FileCountMismatch {
            source: CustodySource::DayManifest,
            expected: 2,
            actual: 3,
        })
    );
}

#[test]
fn proof_checks_segments_files_after_the_day_manifest() {
    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260831", "092000_600", valid_files());
    segments.items[0].files[1].size = 212;
    assert_eq!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260831",
            "092000_600",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::FileSizeMismatch {
            source: CustodySource::Segments,
            name: "audio-unique.flac".into(),
            expected: 202,
            actual: 212,
        })
    );
}
