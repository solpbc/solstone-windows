// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;

use observer_model::LocalZone;
use observer_pl::civil;
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
            segment: None,
            stream: None,
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
fn envelope_meta_reports_the_injected_zone_and_offset() {
    let cases = [
        (1_768_503_600_u64, -25_200_i64, Some("America/Denver")),
        (1_784_138_400, -21_600, Some("America/Denver")),
        (1_768_458_600, 19_800, Some("Asia/Kolkata")),
    ];
    for (boundary, offset, tz) in cases {
        let value = zoned_envelope(boundary, offset, tz.map(str::to_owned));
        assert_eq!(value["meta"]["tz"], tz.unwrap());
        assert_eq!(value["meta"]["utc_offset_seconds"].as_i64(), Some(offset));
        assert_zone_inverse(&value, boundary, offset);
        let raw = serde_json::to_string(&value).unwrap();
        assert!(raw.contains(&format!(
            r#""meta":{{"tz":"{tz}","utc_offset_seconds":{offset}}}"#,
            tz = tz.unwrap()
        )));
        assert!(!raw.contains("name=\"meta\""));
    }

    let missed = zoned_envelope(1_768_458_600, 19_800, None);
    assert!(missed["meta"].get("tz").is_none());
    assert_eq!(missed["meta"]["utc_offset_seconds"].as_i64(), Some(19_800));
    assert_zone_inverse(&missed, 1_768_458_600, 19_800);
    let missed_raw = serde_json::to_string(&missed).unwrap();
    assert!(missed_raw.contains(r#""meta":{"utc_offset_seconds":19800}"#));
    assert!(!missed_raw.contains("\"tz\""));

    for rejected in ["Mountain Standard Time", "MST", ""] {
        let value = zoned_envelope(1_768_458_600, 19_800, Some(rejected.to_owned()));
        assert!(value["meta"].get("tz").is_none());
        assert_eq!(value["meta"]["utc_offset_seconds"].as_i64(), Some(19_800));
        assert_eq!(value["day"], "20260115");
        assert_eq!(value["segment"], "120000_300");
    }
}

fn sample_files() -> Vec<FilePart> {
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
    ]
}

fn zoned_envelope(boundary: u64, offset: i64, tz: Option<String>) -> serde_json::Value {
    let day = civil::day_string_local(boundary, offset);
    let segment = civil::segment_key_string_local(boundary, offset, 300);
    let plain = IngestMultipart::new("v3-boundary", &day, &segment, sample_files()).unwrap();
    let zoned = IngestMultipart::new("v3-boundary", &day, &segment, sample_files())
        .unwrap()
        .with_zone(&LocalZone {
            utc_offset_seconds: offset,
            tz,
        });
    let plain_value = envelope_json(&plain.serialize().unwrap());
    let zoned_value = envelope_json(&zoned.serialize().unwrap());
    assert_eq!(zoned_value["day"], plain_value["day"]);
    assert_eq!(zoned_value["segment"], plain_value["segment"]);
    assert_eq!(zoned_value["files"], plain_value["files"]);
    assert!(plain_value.get("meta").is_none());
    let body = String::from_utf8(zoned.serialize().unwrap()).unwrap();
    assert!(!body.contains("name=\"meta\""));
    zoned_value
}

fn envelope_json(body: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(body).unwrap();
    let marker = "name=\"envelope\"\r\nContent-Type: application/json\r\n\r\n";
    let start = text.find(marker).unwrap() + marker.len();
    let rest = &text[start..];
    let end = rest.find("\r\n--").unwrap();
    serde_json::from_str(&rest[..end]).unwrap()
}

fn assert_zone_inverse(value: &serde_json::Value, boundary: u64, offset: i64) {
    let day = value["day"].as_str().unwrap();
    let segment = value["segment"].as_str().unwrap();
    let hhmmss = segment.split('_').next().unwrap();
    let year: i64 = day[0..4].parse().unwrap();
    let month: u32 = day[4..6].parse().unwrap();
    let dom: u32 = day[6..8].parse().unwrap();
    let hour: u32 = hhmmss[0..2].parse().unwrap();
    let minute: u32 = hhmmss[2..4].parse().unwrap();
    let second: u32 = hhmmss[4..6].parse().unwrap();
    assert_eq!(
        civil::epoch_from_local_parts(year, month, dom, hour, minute, second, offset),
        Some(boundary)
    );
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
fn listing_reconciliation_rejects_duplicate_keys_and_incomplete_physical_coordinates() {
    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260820", "120000_10~browser_a", valid_files());
    segments.items[0].segment = Some("120000_10".into());
    segments.items[0].stream = Some("browser_a".into());
    let mut duplicate = segments.items[0].clone();
    duplicate.stream = Some("browser_b".into());
    segments.items.push(duplicate);
    segments.total = 2;
    assert!(matches!(
        prove_custody(
            &manifest,
            &day_manifest,
            &segments,
            "20260820",
            "120000_10~browser_a",
            &local
        ),
        CustodyProof::Unconfirmed(CustodyFailure::DuplicateListingKey { .. })
    ));

    segments.items.truncate(1);
    segments.total = 1;
    for (segment, stream) in [
        (Some("120000_10"), None),
        (Some("120000_10"), Some("")),
        (Some(""), Some("browser_a")),
    ] {
        segments.items[0].segment = segment.map(str::to_owned);
        segments.items[0].stream = stream.map(str::to_owned);
        assert!(
            matches!(
                prove_custody(
                    &manifest,
                    &day_manifest,
                    &segments,
                    "20260820",
                    "120000_10~browser_a",
                    &local
                ),
                CustodyProof::Unconfirmed(CustodyFailure::PhysicalCoordinatesMalformed { .. })
            ),
            "{segment:?}/{stream:?}"
        );
    }
}

#[test]
fn listing_collision_alias_keeps_physical_stream_and_segment_distinct() {
    let (manifest, day_manifest, mut segments, local) =
        proof_input("20260820", "120000_10~browser_a", valid_files());
    segments.items[0].segment = Some("120000_10".into());
    segments.items[0].stream = Some("browser_a".into());
    let proof = prove_custody(
        &manifest,
        &day_manifest,
        &segments,
        "20260820",
        "120000_10~browser_a",
        &local,
    );
    let CustodyProof::Confirmed(witness) = proof else {
        panic!("physical alias should prove custody");
    };
    assert_eq!(witness.server_segment(), "120000_10~browser_a");
    assert_eq!(segments.items[0].segment.as_deref(), Some("120000_10"));
    assert_eq!(segments.items[0].stream.as_deref(), Some("browser_a"));
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

#[test]
fn receipt_validation_fails_on_sha256_mismatch() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".into(),
            disposition: "written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::Sha256Mismatch { .. })
    ));
}

#[test]
fn receipt_validation_fails_on_size_mismatch() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 200,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::SizeMismatch { .. })
    ));
}

#[test]
fn receipt_validation_fails_on_received_not_written() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "received_not_written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::ReceivedNotWritten { .. })
    ));
}

#[test]
fn receipt_validation_fails_on_missing_descriptor() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [
        LocalFile {
            name: "screen.mp4",
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            size: 100,
        },
        LocalFile {
            name: "audio.flac",
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            size: 50,
        },
    ];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::FileCountMismatch { .. })
    ));
}

#[test]
fn receipt_validation_fails_on_extra_descriptor() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![
            FileDescriptor {
                submitted: "screen.mp4".into(),
                written: "screen.mp4".into(),
                size: 100,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                disposition: "written".into(),
            },
            FileDescriptor {
                submitted: "extra.bin".into(),
                written: "extra.bin".into(),
                size: 50,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                disposition: "written".into(),
            },
        ]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::FileCountMismatch { .. })
    ));
}

#[test]
fn receipt_validation_fails_on_duplicate_submitted_name() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![
            FileDescriptor {
                submitted: "screen.mp4".into(),
                written: "screen.mp4".into(),
                size: 100,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                disposition: "written".into(),
            },
            FileDescriptor {
                submitted: "screen.mp4".into(),
                written: "screen.mp4".into(),
                size: 100,
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                disposition: "written".into(),
            },
        ]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::DuplicateSubmittedName(_))
    ));
}

#[test]
fn receipt_validation_fails_on_non_hex_or_uppercase_sha256() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp_upper = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF".into(),
            disposition: "written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp_upper, &local),
        Err(ReceiptFault::Sha256InvalidHex(..))
    ));

    let resp_short = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "invalid-hex".into(),
            disposition: "written".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp_short, &local),
        Err(ReceiptFault::Sha256InvalidHex(..))
    ));
}

#[test]
fn receipt_validation_fails_on_absent_file_descriptors() {
    use observer_pl::ingest::{validate_receipt, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Absent,
    };
    assert_eq!(validate_receipt(&resp, &local), Err(ReceiptFault::Absent));
}

#[test]
fn receipt_validation_fails_on_duplicate_without_existing_segment() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Duplicate,
        segment: None,
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "already_held".into(),
        }]),
    };
    assert_eq!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::DuplicateMissingExistingSegment)
    );
}

#[test]
fn receipt_validation_fails_on_ok_without_segment() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: None,
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "written".into(),
        }]),
    };
    assert_eq!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::MissingServerSegment)
    );
}

#[test]
fn receipt_validation_fails_on_unknown_disposition() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors, ReceiptFault};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];
    let resp = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "quarantined".into(),
        }]),
    };
    assert!(matches!(
        validate_receipt(&resp, &local),
        Err(ReceiptFault::UnknownDisposition { .. })
    ));
}

#[test]
fn receipt_validation_passes_on_valid_ok_duplicate_and_collision_responses() {
    use observer_pl::ingest::{validate_receipt, FileDescriptor, FileDescriptors};

    let local = [LocalFile {
        name: "screen.mp4",
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        size: 100,
    }];

    // Ok response
    let resp_ok = IngestResponse {
        status: IngestStatus::Ok,
        segment: Some("143000_300".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "written".into(),
        }]),
    };
    let receipt = validate_receipt(&resp_ok, &local).unwrap();
    assert_eq!(receipt.server_segment(), "143000_300");

    // Collision response with written != submitted
    let resp_collision = IngestResponse {
        status: IngestStatus::Collision,
        segment: Some("143000_300_1".into()),
        existing_segment: None,
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "remapped_screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "written".into(),
        }]),
    };
    let receipt_collision = validate_receipt(&resp_collision, &local).unwrap();
    assert_eq!(receipt_collision.server_segment(), "143000_300_1");
    assert_eq!(receipt_collision.files()[0].written, "remapped_screen.mp4");

    // Duplicate response with already_held
    let resp_duplicate = IngestResponse {
        status: IngestStatus::Duplicate,
        segment: None,
        existing_segment: Some("143000_300".into()),
        reason_code: None,
        file_descriptors: FileDescriptors::Decoded(vec![FileDescriptor {
            submitted: "screen.mp4".into(),
            written: "screen.mp4".into(),
            size: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            disposition: "already_held".into(),
        }]),
    };
    let receipt_dup = validate_receipt(&resp_duplicate, &local).unwrap();
    assert_eq!(receipt_dup.server_segment(), "143000_300");
}

#[test]
fn ingest_response_decodes_unknown_disposition_and_extra_descriptor_properties() {
    use observer_pl::ingest::FileDescriptors;

    let json = r#"{
        "status": "ok",
        "segment": "143000_300",
        "file_descriptors": [
            {
                "submitted": "screen.mp4",
                "written": "screen.mp4",
                "size": 100,
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "disposition": "quarantined_by_server",
                "extra_server_metadata": "ignored_value"
            }
        ]
    }"#;
    let resp: IngestResponse = serde_json::from_str(json).unwrap();
    match resp.file_descriptors {
        FileDescriptors::Decoded(descriptors) => {
            assert_eq!(descriptors.len(), 1);
            assert_eq!(descriptors[0].disposition, "quarantined_by_server");
        }
        _ => panic!("expected decoded descriptors"),
    }
}

#[test]
fn segment_item_decodes_without_observed_field() {
    let json = r#"{
        "key": "143000_300",
        "files": [
            {
                "name": "screen.mp4",
                "size": 100,
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "status": "present"
            }
        ]
    }"#;
    let item: SegmentItem = serde_json::from_str(json).unwrap();
    assert_eq!(item.key, "143000_300");
    assert!(!item.observed);
}

#[test]
fn a_browser_period_names_its_source_in_the_envelope() {
    let multipart = IngestMultipart::new(
        "b",
        "20261002",
        "100000_300",
        vec![FilePart {
            filename: "browser_pages.jsonl".into(),
            content_type: "application/jsonl".into(),
            bytes: b"{\"t\":\"segment_start\"}\n".to_vec(),
        }],
    )
    .unwrap()
    .with_source("browser");
    let body = multipart.serialize().unwrap();
    let envelope = envelope_json(&body);
    assert_eq!(envelope["source"], "browser");
    assert_eq!(envelope["files"][0]["submitted"], "browser_pages.jsonl");
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("filename=\"browser_pages.jsonl\"\r\nContent-Type: application/jsonl\r\n"));
}
