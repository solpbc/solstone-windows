// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[path = "support/transparency.rs"]
mod transparency_support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use transparency_support::{DirectoryTransparencyTransport, RecordedTransparencyCall};
use xtask::release_clock::FixedClock;
use xtask::release_exec::{CommandOutput, CommandRunner, CommandRunnerError};
use xtask::release_receipt::{
    render_windows_native_proof_receipt, CompanionManifestReceipt, WindowsNativeProofReceipt,
    WINDOWS_NATIVE_PROOF_SCHEMA,
};
use xtask::rust_release_manifest::{
    self, CheckoutFacts, Manifest, PRODUCT, TARGET_FEATURES, TARGET_PROFILE, TARGET_TRIPLE,
};
use xtask::transparency_format::{
    canonicalize_transparency_json, format_entry_trusted_comment, format_latest_trusted_comment,
    render_transparency_entry, render_transparency_latest, TransparencyHeadLogRow,
    TransparencyLatestV1, TransparencyLedgerEntryV1,
};
use xtask::transparency_publisher::{
    parse_archive_receipt, publish_transparency, resign_transparency_pointer,
    resolve_transparency_environment_with, TransparencyPublishError, TransparencyPublishRequest,
    TransparencyResignRequest, TRANSPARENCY_ENV_NAMES,
};
use xtask::transparency_stage::render_staging_manifest_v1;
use xtask::transparency_transport::{
    ObservedHttpResponse, TransparencyCachePolicy, TransparencyFetchPolicy,
    TransparencyListDestination, TransparencyObjectDestination, TransparencyObjectTransport,
    TransparencyPlane, TransparencyTransportError,
};

struct FakePublisherRunner {
    archive_program: PathBuf,
    archive_behavior: ArchiveBehavior,
    verification_behavior: VerificationBehavior,
    mutate_candidate_at_snapshot: Option<PathBuf>,
    phases: Mutex<Vec<&'static str>>,
}

#[derive(Clone, Copy)]
enum ArchiveBehavior {
    Valid,
    WrongDigest,
    Failure,
}

#[derive(Clone, Copy)]
enum VerificationBehavior {
    Valid,
    RejectLedgerEntry,
}

impl CommandRunner for FakePublisherRunner {
    fn record_phase(&self, phase: &'static str) -> Result<(), CommandRunnerError> {
        self.phases
            .lock()
            .expect("record publisher phase")
            .push(phase);
        if phase == xtask::transparency_publisher::STEP_4_SNAPSHOT_STAGE {
            if let Some(path) = &self.mutate_candidate_at_snapshot {
                fs::write(path, b"candidate changed after preflight\n")
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
            }
        }
        Ok(())
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        _stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        assert!(env.is_none());
        if program == self.archive_program {
            let manifest = render_staging_manifest_v1(Path::new(&args[0]))
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
            return Ok(match self.archive_behavior {
                ArchiveBehavior::Valid => output(format!("ARCHIVED {}\n", manifest.sha256)),
                ArchiveBehavior::WrongDigest => output(format!("ARCHIVED {}\n", "0".repeat(64))),
                ArchiveBehavior::Failure => CommandOutput {
                    status: 17,
                    stdout: b"archive rejected".to_vec(),
                    stderr: b"private child detail".to_vec(),
                },
            });
        }
        match args.first().map(String::as_str) {
            Some("-v") => Ok(output("minisign 0.11\n")),
            Some("--version") => Ok(output("curl 8.5.0\n")),
            Some("-S") => {
                let signature_path = argument_value(args, "-x");
                let comment = argument_value(args, "-t");
                fs::write(
                    signature_path,
                    format!(
                        "untrusted comment: fake signature\nFAKE\ntrusted comment: {comment}\nFAKE\n"
                    ),
                )
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                Ok(output(Vec::new()))
            }
            Some("-V") => {
                let reject = match self.verification_behavior {
                    VerificationBehavior::Valid => false,
                    VerificationBehavior::RejectLedgerEntry => fs::read(argument_value(args, "-m"))
                        .map(|body| {
                            body.windows(b"transparency-ledger-entry".len())
                                .any(|window| window == b"transparency-ledger-entry")
                        })
                        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?,
                };
                if reject {
                    Ok(CommandOutput {
                        status: 1,
                        stdout: Vec::new(),
                        stderr: b"invalid signature".to_vec(),
                    })
                } else {
                    Ok(output("Signature and comment signature verified\n"))
                }
            }
            _ => Err(CommandRunnerError::UnexpectedInvocation),
        }
    }
}

struct OneVerificationWaitFailure<'a> {
    inner: &'a FakePublisherRunner,
    failed: AtomicBool,
}

impl CommandRunner for OneVerificationWaitFailure<'_> {
    fn record_phase(&self, phase: &'static str) -> Result<(), CommandRunnerError> {
        self.inner.record_phase(phase)
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        if args.first().map(String::as_str) == Some("-V")
            && !self.failed.swap(true, Ordering::SeqCst)
        {
            return Err(CommandRunnerError::WaitFailed);
        }
        self.inner.run(program, args, stdin, env)
    }

    fn run_interactive(
        &self,
        program: &Path,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        self.inner.run_interactive(program, args, env)
    }
}

#[test]
fn transparency_publisher_archives_artifacts_but_never_addresses_them_publicly() {
    let checkout = temporary_root("publisher");
    fs::write(checkout.join("transparency-head-log.jsonl"), b"").expect("create empty head log");
    let release_dir = fixture_release_dir();
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(release_dir.join(rust_release_manifest::companion_basename()))
            .expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");
    let facts = facts_for(&manifest);
    let archive_program = absolute_program("archive");
    let environment = environment(&archive_program);
    let runner = FakePublisherRunner {
        archive_program,
        archive_behavior: ArchiveBehavior::Valid,
        verification_behavior: VerificationBehavior::Valid,
        mutate_candidate_at_snapshot: None,
        phases: Mutex::new(Vec::new()),
    };
    let transport = DirectoryTransparencyTransport::new(checkout.join("fake-objects"));
    let clock = FixedClock::new("2026-07-22T00:00:00Z").expect("fixed clock");
    let evidence_dir = checkout.join("evidence");
    let minisign_program = absolute_program("minisign");
    let curl_program = absolute_program("curl");
    let request = TransparencyPublishRequest {
        checkout_root: &checkout,
        release_dir: &release_dir,
        evidence_dir: &evidence_dir,
        checkout_facts: &facts,
        environment: &environment,
        minisign_program: &minisign_program,
        curl_program: &curl_program,
    };
    let publication = publish_transparency(&request, &transport, &runner, &clock)
        .expect("publish fixture through fakes");
    assert_eq!(publication.version, "0.2.11");
    assert_eq!(publication.seq, 1);
    assert_eq!(
        *runner.phases.lock().expect("publisher phases"),
        [
            xtask::transparency_publisher::STEP_1_PREFLIGHT,
            xtask::transparency_publisher::STEP_2_FETCH_CHAIN,
            xtask::transparency_publisher::STEP_4_SNAPSHOT_STAGE,
            xtask::transparency_publisher::STEP_3_BUILD_SIGN,
            xtask::transparency_publisher::STEP_5_ARCHIVE,
            xtask::transparency_publisher::STEP_6_IMMUTABLE_UPLOAD,
            xtask::transparency_publisher::STEP_7_PUBLIC_VERIFY,
            xtask::transparency_publisher::STEP_8_MUTABLE_COMMIT,
            xtask::transparency_publisher::STEP_9_HEAD_LOG,
            xtask::transparency_publisher::STEP_10_SUMMARY,
        ]
    );
    let staged_entry_path = checkout
        .join("target/release-transparency-stage")
        .join(PRODUCT)
        .join("0.2.11/archive/releases")
        .join(PRODUCT)
        .join("v/0.2.11/ledger-entry.json");
    let first_entry = fs::read(&staged_entry_path).expect("read first staged entry");
    let calls_before_retry = transport.calls();

    let later = FixedClock::new("2026-08-07T00:00:00Z").expect("later clock");
    let retry = publish_transparency(&request, &transport, &runner, &later)
        .expect("recognize already published retry");
    assert!(retry.already_published);
    assert!(retry.pointer_requires_resign);
    assert_eq!(retry.archive_sha256, publication.archive_sha256);
    assert_eq!(
        fs::read(&staged_entry_path).expect("read retried staged entry"),
        first_entry
    );

    assert_eq!(
        calls_before_retry
            .iter()
            .map(recorded_call_name)
            .collect::<Vec<_>>(),
        [
            format!("get:s3:releases/{PRODUCT}/latest.json"),
            format!("get:s3:releases/{PRODUCT}/latest.json.minisig"),
            format!("list:releases/{PRODUCT}/v/"),
            format!("get:s3:releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
            format!("get:s3:releases/{PRODUCT}/v/0.2.11/ledger-entry.json.minisig"),
            format!("list:releases/{PRODUCT}/v/0.2.11/"),
            format!("create:s3:releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
            format!("create:s3:releases/{PRODUCT}/v/0.2.11/ledger-entry.json.minisig"),
            format!("create:s3:releases/{PRODUCT}/v/0.2.11/solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json"),
            format!("get:public:releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
            format!("get:public:releases/{PRODUCT}/v/0.2.11/ledger-entry.json.minisig"),
            format!("get:public:releases/{PRODUCT}/v/0.2.11/solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json"),
            format!("get:s3:releases/{PRODUCT}/latest.json"),
            format!("mutable:s3:releases/{PRODUCT}/ledger.jsonl"),
            format!("get:public:releases/{PRODUCT}/ledger.jsonl"),
            format!("mutable:s3:releases/{PRODUCT}/latest.json.minisig"),
            format!("get:public:releases/{PRODUCT}/latest.json.minisig"),
            format!("mutable:s3:releases/{PRODUCT}/latest.json"),
            format!("get:public:releases/{PRODUCT}/latest.json"),
            format!("get:public:releases/{PRODUCT}/latest.json"),
            format!("get:public:releases/{PRODUCT}/latest.json.minisig"),
        ]
    );

    let resign = resign_transparency_pointer(
        &TransparencyResignRequest {
            checkout_root: &checkout,
            environment: &environment,
            minisign_program: &minisign_program,
            curl_program: &curl_program,
        },
        &transport,
        &runner,
        &later,
    )
    .expect("re-sign pointer through fakes");
    assert_eq!(resign.chain_length, publication.seq);
    assert_eq!(resign.tip_sha256, publication.entry_sha256);

    let archive_version = checkout
        .join("target/release-transparency-stage")
        .join(PRODUCT)
        .join("0.2.11/archive/releases")
        .join(PRODUCT)
        .join("v/0.2.11");
    let stage_root = archive_version
        .ancestors()
        .nth(5)
        .expect("version staging root");
    let mut stage_names: Vec<_> = fs::read_dir(stage_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    stage_names.sort();
    assert_eq!(
        stage_names,
        [
            std::ffi::OsString::from("archive"),
            std::ffi::OsString::from("archive-ack.v1"),
            std::ffi::OsString::from("stage-manifest.v1")
        ]
    );
    assert_eq!(
        fs::read(stage_root.join("archive-ack.v1")).expect("durable archive acknowledgment"),
        format!(
            "ARCHIVED {}\n",
            publication
                .archive_sha256
                .as_deref()
                .expect("archive digest after channel acknowledgment")
        )
        .as_bytes()
    );
    let artifact_names: BTreeSet<_> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect();
    for artifact in &artifact_names {
        assert_eq!(
            fs::read(archive_version.join(artifact)).expect("archived artifact bytes"),
            fs::read(release_dir.join(artifact)).expect("candidate artifact bytes")
        );
    }
    let public_destinations: Vec<_> = transport
        .destinations()
        .into_iter()
        .filter(|destination| destination.plane == TransparencyPlane::Public)
        .map(|destination| destination.key)
        .collect();
    for artifact in artifact_names {
        assert!(
            public_destinations
                .iter()
                .all(|destination| !destination.ends_with(artifact)),
            "artifact reached public destination: {artifact}"
        );
    }
}

#[test]
fn transparency_archive_failure_or_wrong_receipt_reaches_no_public_write() {
    for (label, behavior) in [
        ("archive-failure", ArchiveBehavior::Failure),
        ("archive-digest", ArchiveBehavior::WrongDigest),
    ] {
        assert_archive_fault_fails_before_publication(label, behavior);
    }
}

#[test]
fn transparency_crash_table_keeps_the_pointer_body_at_the_commit_boundary() {
    for immutable_index in 0..3 {
        run_crash_case(
            &format!("immutable-{immutable_index}"),
            Some(immutable_index),
            None,
            false,
            false,
        );
    }
    for mutable_index in 0..3 {
        run_crash_case(
            &format!("mutable-{mutable_index}"),
            None,
            Some(mutable_index),
            false,
            false,
        );
    }
    run_crash_case("pointer-moved", None, None, true, false);
    run_crash_case("committed", None, None, false, true);
}

#[test]
fn transparency_pointer_resigning_recovers_after_each_mutable_failure() {
    for mutable_offset in 0..2 {
        let fixture = OwnedPublisherFixture::new(&format!("resign-crash-{mutable_offset}"));
        let transport = CrashTransport {
            inner: DirectoryTransparencyTransport::new(
                fixture.checkout.join("resign-crash-objects"),
            ),
            fail_public_at: None,
            fail_mutable_at: AtomicUsize::new(usize::MAX),
            public_calls: AtomicUsize::new(0),
            mutable_calls: AtomicUsize::new(0),
            move_pointer_on_recheck: false,
            pointer_reads: AtomicUsize::new(0),
        };
        let request = TransparencyPublishRequest {
            checkout_root: &fixture.checkout,
            release_dir: &fixture.release_dir,
            evidence_dir: &fixture.evidence_dir,
            checkout_facts: &fixture.facts,
            environment: &fixture.environment,
            minisign_program: &fixture.minisign_program,
            curl_program: &fixture.curl_program,
        };
        publish_transparency(
            &request,
            &transport,
            &fixture.runner,
            &FixedClock::new("2026-07-22T00:00:00Z").unwrap(),
        )
        .expect("publish initial pointer");
        let pointer_destination = TransparencyObjectDestination {
            plane: TransparencyPlane::S3,
            key: format!("releases/{PRODUCT}/latest.json"),
        };
        let old_pointer = transport
            .inner
            .object_bytes(&pointer_destination)
            .expect("old pointer body");

        transport.arm_mutable_failure(mutable_offset);
        let resign_request = TransparencyResignRequest {
            checkout_root: &fixture.checkout,
            environment: &fixture.environment,
            minisign_program: &fixture.minisign_program,
            curl_program: &fixture.curl_program,
        };
        assert_eq!(
            resign_transparency_pointer(
                &resign_request,
                &transport,
                &fixture.runner,
                &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
            ),
            Err(TransparencyPublishError::MutableWrite {
                observed: 503,
                expected: "HTTP 200..299".to_owned(),
            })
        );
        assert_eq!(
            transport
                .inner
                .object_bytes(&pointer_destination)
                .expect("body remains readable at commit boundary"),
            old_pointer
        );

        let completed = resign_transparency_pointer(
            &resign_request,
            &transport,
            &fixture.runner,
            &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
        )
        .expect("retry resumes persisted resign pair");
        assert_eq!(completed.chain_length, 1);
        assert_ne!(
            transport
                .inner
                .object_bytes(&pointer_destination)
                .expect("new pointer body"),
            old_pointer
        );
    }
}

#[test]
fn transparency_signature_verification_failure_leaves_the_next_retry_usable() {
    let fixture = OwnedPublisherFixture::new("signature-scratch-retry");
    fixture
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let failing_runner = OneVerificationWaitFailure {
        inner: &fixture.runner,
        failed: AtomicBool::new(false),
    };
    let request = TransparencyPublishRequest {
        checkout_root: &fixture.checkout,
        release_dir: &fixture.release_dir,
        evidence_dir: &fixture.evidence_dir,
        checkout_facts: &fixture.facts,
        environment: &fixture.environment,
        minisign_program: &fixture.minisign_program,
        curl_program: &fixture.curl_program,
    };
    assert_eq!(
        publish_transparency(
            &request,
            &fixture.transport,
            &failing_runner,
            &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
        ),
        Err(TransparencyPublishError::Process)
    );
    let stage_root = fixture.checkout.join("target/release-transparency-stage");
    assert!(fs::read_dir(&stage_root).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("fetch-pointer-verify")));
    assert!(
        fixture
            .publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap())
            .expect("retry after wait failure")
            .already_published
    );
}

#[test]
fn transparency_contract_faults_reject_genesis_rollback_signatures_and_snapshot_drift() {
    let genesis = OwnedPublisherFixture::new("genesis-disabled");
    let mut environment = environment(&genesis.archive_program);
    environment.genesis = false;
    assert_eq!(
        genesis.publish_with(
            &environment,
            &genesis.runner,
            &FixedClock::new("2026-07-22T00:00:00Z").unwrap()
        ),
        Err(TransparencyPublishError::GenesisNotAuthorized)
    );

    let existing = OwnedPublisherFixture::new("genesis-existing");
    existing
        .transport
        .create_only_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("releases/{PRODUCT}/v/orphan/evidence"),
            },
            b"existing",
            TransparencyCachePolicy::Immutable,
        )
        .unwrap();
    assert_eq!(
        existing.publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::GenesisNotEmpty)
    );

    let rollback = OwnedPublisherFixture::new("head-rollback");
    rollback
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let row = TransparencyHeadLogRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.to_owned(),
        published_utc: "2026-07-23T00:00:00Z".to_owned(),
        seq: 2,
        version: "0.2.12".to_owned(),
    };
    fs::write(
        rollback.checkout.join("transparency-head-log.jsonl"),
        canonicalize_transparency_json(&row).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rollback.publish(&FixedClock::new("2026-07-24T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::Rollback {
            observed: 1,
            expected: 2
        })
    );

    let invalid_signature = OwnedPublisherFixture::new("tip-signature");
    invalid_signature
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let rejecting_runner = FakePublisherRunner {
        archive_program: invalid_signature.archive_program.clone(),
        archive_behavior: ArchiveBehavior::Valid,
        verification_behavior: VerificationBehavior::RejectLedgerEntry,
        mutate_candidate_at_snapshot: None,
        phases: Mutex::new(Vec::new()),
    };
    assert_eq!(
        invalid_signature.publish_with(
            &invalid_signature.environment,
            &rejecting_runner,
            &FixedClock::new("2026-07-23T00:00:00Z").unwrap()
        ),
        Err(TransparencyPublishError::ChainInvalid {
            observed: "locked entry signature verification failed".to_owned(),
            expected: "a valid signature over immutable entry bytes".to_owned(),
        })
    );

    let mut drift = OwnedPublisherFixture::new("snapshot-drift");
    let copied_release = drift.checkout.join("release-dir");
    copy_directory(&drift.release_dir, &copied_release);
    drift.release_dir = copied_release.clone();
    let mutating_runner = FakePublisherRunner {
        archive_program: drift.archive_program.clone(),
        archive_behavior: ArchiveBehavior::Valid,
        verification_behavior: VerificationBehavior::Valid,
        mutate_candidate_at_snapshot: Some(copied_release.join("assets.win.json")),
        phases: Mutex::new(Vec::new()),
    };
    assert_eq!(
        drift.publish_with(
            &drift.environment,
            &mutating_runner,
            &FixedClock::new("2026-07-22T00:00:00Z").unwrap()
        ),
        Err(TransparencyPublishError::CandidateInvalid)
    );
}

#[test]
fn transparency_derived_ledger_repairs_missing_malformed_and_superset_bytes() {
    let recoverable = OwnedPublisherFixture::new("ledger-rederive");
    recoverable
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let ledger_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/ledger.jsonl"),
    };
    recoverable
        .transport
        .mutable_put(
            &ledger_destination,
            b"malformed derived bytes\n",
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    let expected_ledger = recoverable
        .transport
        .object_bytes(&TransparencyObjectDestination {
            plane: TransparencyPlane::S3,
            key: format!("releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
        })
        .expect("locked entry bytes");
    assert!(
        recoverable
            .publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap())
            .unwrap()
            .already_published
    );
    assert_eq!(
        recoverable
            .transport
            .object_bytes(&ledger_destination)
            .expect("repaired malformed ledger"),
        expected_ledger
    );

    recoverable.transport.remove_object(&ledger_destination);
    assert!(
        recoverable
            .publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap())
            .unwrap()
            .already_published
    );
    assert_eq!(
        recoverable
            .transport
            .object_bytes(&ledger_destination)
            .expect("repaired missing ledger"),
        expected_ledger
    );

    let mut extra: TransparencyLedgerEntryV1 =
        serde_json::from_slice(&expected_ledger).expect("parse current tip");
    extra.seq = 2;
    extra.version = "0.2.12".to_owned();
    extra.prev_version = "0.2.11".to_owned();
    extra.prev_sha256 = xtask::transparency_format::transparency_sha256_hex(&expected_ledger);
    extra.published_utc = "2026-07-23T00:00:00Z".to_owned();
    let mut superset = expected_ledger.clone();
    superset.extend_from_slice(&render_transparency_entry(&extra).unwrap());
    recoverable
        .transport
        .mutable_put(
            &ledger_destination,
            &superset,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    assert!(
        recoverable
            .publish(&FixedClock::new("2026-07-24T00:00:00Z").unwrap())
            .unwrap()
            .already_published
    );
    assert_eq!(
        recoverable
            .transport
            .object_bytes(&ledger_destination)
            .expect("repaired ledger superset"),
        expected_ledger
    );
}

#[test]
fn transparency_derived_ledger_rejects_a_locked_contradiction() {
    let contradictory = OwnedPublisherFixture::new("ledger-contradiction");
    contradictory
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let entry_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
    };
    let original = contradictory
        .transport
        .object_bytes(&entry_destination)
        .expect("locked entry bytes");
    let mut changed: TransparencyLedgerEntryV1 = serde_json::from_slice(&original).unwrap();
    changed.published_utc = "2026-07-22T00:00:01Z".to_owned();
    let changed = render_transparency_entry(&changed).unwrap();
    let ledger_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/ledger.jsonl"),
    };
    contradictory
        .transport
        .mutable_put(
            &ledger_destination,
            &changed,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    assert_eq!(
        contradictory.publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::ChainInvalid {
            observed: "derived ledger contradicts a locked entry".to_owned(),
            expected: "byte identity for every overlapping version".to_owned(),
        })
    );
}

#[derive(Clone, Copy)]
enum ChainLinkFault {
    BrokenPreviousDigest,
    GappedSequence,
    NonMonotonicSequence,
}

#[test]
fn transparency_locked_chain_rejects_each_linkage_fault_at_its_guard() {
    for (label, fault, expected) in [
        (
            "broken-prev-sha256",
            ChainLinkFault::BrokenPreviousDigest,
            TransparencyPublishError::ChainInvalid {
                observed: "broken previous entry linkage".to_owned(),
                expected: "contiguous sequence digest and increasing time".to_owned(),
            },
        ),
        (
            "gapped-sequence",
            ChainLinkFault::GappedSequence,
            TransparencyPublishError::ChainInvalid {
                observed: "broken previous entry linkage".to_owned(),
                expected: "contiguous sequence digest and increasing time".to_owned(),
            },
        ),
        (
            "non-monotonic-sequence",
            ChainLinkFault::NonMonotonicSequence,
            TransparencyPublishError::ChainInvalid {
                observed: "invalid genesis linkage".to_owned(),
                expected: "sequence 1 with empty previous version and zero digest".to_owned(),
            },
        ),
    ] {
        assert_chain_link_fault(label, fault, expected);
    }
}

fn assert_chain_link_fault(label: &str, fault: ChainLinkFault, expected: TransparencyPublishError) {
    let fixture = OwnedPublisherFixture::new(label);
    fixture
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let (release_dir, facts) = versioned_release_fixture(&fixture.checkout, "0.2.12");
    fixture
        .publish_candidate(
            &release_dir,
            &facts,
            &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
        )
        .expect("publish second chain entry");
    let entry_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/v/0.2.12/ledger-entry.json"),
    };
    let mut entry: TransparencyLedgerEntryV1 = serde_json::from_slice(
        &fixture
            .transport
            .object_bytes(&entry_destination)
            .expect("second locked entry"),
    )
    .unwrap();
    match fault {
        ChainLinkFault::BrokenPreviousDigest => entry.prev_sha256 = "f".repeat(64),
        ChainLinkFault::GappedSequence => entry.seq = 3,
        ChainLinkFault::NonMonotonicSequence => entry.seq = 1,
    }
    overwrite_signed_entry_and_pointer(&fixture.transport, &entry);
    assert_eq!(
        fixture.publish_candidate(
            &release_dir,
            &facts,
            &FixedClock::new("2026-07-24T00:00:00Z").unwrap(),
        ),
        Err(expected),
        "{label}"
    );
}

#[test]
fn transparency_create_only_race_rejects_foreign_bytes_and_adopts_valid_own_bytes() {
    run_race_case("race-invalid", RaceMode::Invalid);
    run_race_case("race-valid-own", RaceMode::ValidOwn);
}

#[test]
fn transparency_adoption_rejects_a_timestamp_not_later_than_the_tip() {
    let fixture = OwnedPublisherFixture::new("race-non-monotonic");
    let transport =
        DirectoryTransparencyTransport::new(fixture.checkout.join("race-non-monotonic-objects"));
    publish_transparency(
        &TransparencyPublishRequest {
            checkout_root: &fixture.checkout,
            release_dir: &fixture.release_dir,
            evidence_dir: &fixture.evidence_dir,
            checkout_facts: &fixture.facts,
            environment: &fixture.environment,
            minisign_program: &fixture.minisign_program,
            curl_program: &fixture.curl_program,
        },
        &transport,
        &fixture.runner,
        &FixedClock::new("2026-07-22T00:00:00Z").unwrap(),
    )
    .expect("publish chain tip");
    let mutable_before = transport
        .calls()
        .into_iter()
        .filter(|call| matches!(call, RecordedTransparencyCall::MutablePut(_, _, _, _)))
        .count();
    let transport = RaceTransport {
        inner: transport,
        mode: RaceMode::ValidOwn,
        triggered: AtomicBool::new(false),
    };
    let (release_dir, facts) = versioned_release_fixture(&fixture.checkout, "0.2.12");
    let error = publish_transparency(
        &TransparencyPublishRequest {
            checkout_root: &fixture.checkout,
            release_dir: &release_dir,
            evidence_dir: &fixture.evidence_dir,
            checkout_facts: &facts,
            environment: &fixture.environment,
            minisign_program: &fixture.minisign_program,
            curl_program: &fixture.curl_program,
        },
        &transport,
        &fixture.runner,
        &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
    )
    .expect_err("non-monotonic racing entry must be terminal");
    assert_eq!(
        error,
        TransparencyPublishError::VersionPrefixPoisoned {
            version: "0.2.12".to_owned(),
            observed: "a racing entry with different release or chain semantics",
        }
    );
    assert_eq!(
        transport
            .inner
            .calls()
            .into_iter()
            .filter(|call| matches!(call, RecordedTransparencyCall::MutablePut(_, _, _, _)))
            .count(),
        mutable_before
    );
}

#[test]
fn transparency_proof_product_and_head_log_faults_fail_closed() {
    let stale_proof = OwnedPublisherFixture::new("stale-proof");
    fs::create_dir_all(&stale_proof.evidence_dir).unwrap();
    let companion_bytes = fs::read(
        stale_proof
            .release_dir
            .join(rust_release_manifest::companion_basename()),
    )
    .unwrap();
    let manifest: Manifest = serde_json::from_slice(&companion_bytes).unwrap();
    let setup_sha256 = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.path.ends_with(".exe"))
        .expect("setup artifact")
        .sha256
        .clone();
    let prior_version_receipt = WindowsNativeProofReceipt {
        schema: WINDOWS_NATIVE_PROOF_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        version: "0.2.10".to_owned(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: manifest.source_commit.clone(),
        companion_manifest: CompanionManifestReceipt {
            filename: rust_release_manifest::companion_basename(),
            sha256: xtask::transparency_format::transparency_sha256_hex(&companion_bytes),
        },
        setup_sha256,
        packaged_executable_sha256: manifest.packaged_executable.sha256.clone(),
        installed_executable_sha256: manifest.packaged_executable.sha256.clone(),
        install_mode: "isolated-clean".to_owned(),
        installer_success: true,
        smoke_success: true,
        proved_at: "2026-07-21T00:00:00Z".to_owned(),
    };
    fs::write(
        stale_proof.evidence_dir.join("windows-native-proof.json"),
        render_windows_native_proof_receipt(&prior_version_receipt).unwrap(),
    )
    .unwrap();
    assert_eq!(
        stale_proof.publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::ProofInvalid)
    );

    let foreign = OwnedPublisherFixture::new("foreign-product");
    foreign
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let pointer_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/latest.json"),
    };
    let mut pointer: TransparencyLatestV1 = serde_json::from_slice(
        &foreign
            .transport
            .object_bytes(&pointer_destination)
            .expect("current pointer"),
    )
    .unwrap();
    pointer.product = "foreign-product".to_owned();
    foreign
        .transport
        .mutable_put(
            &pointer_destination,
            &render_transparency_latest(&pointer).unwrap(),
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    assert_eq!(
        foreign.publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::ChainInvalid {
            observed: "foreign pointer product".to_owned(),
            expected: PRODUCT.to_owned(),
        })
    );

    let fork = OwnedPublisherFixture::new("head-fork");
    fork.publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let head_path = fork.checkout.join("transparency-head-log.jsonl");
    let mut row: TransparencyHeadLogRow =
        serde_json::from_slice(&fs::read(&head_path).unwrap()).unwrap();
    row.entry_sha256 = "f".repeat(64);
    fs::write(&head_path, canonicalize_transparency_json(&row).unwrap()).unwrap();
    assert_eq!(
        fork.publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::HeadLogFork)
    );
}

#[test]
fn transparency_fetched_pointer_requires_exactly_fourteen_days_of_validity() {
    let fixture = OwnedPublisherFixture::new("pointer-validity");
    fixture
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let pointer_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/latest.json"),
    };
    let signature_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/latest.json.minisig"),
    };
    let mut pointer: TransparencyLatestV1 = serde_json::from_slice(
        &fixture
            .transport
            .object_bytes(&pointer_destination)
            .expect("current pointer"),
    )
    .unwrap();
    pointer.valid_until = "2026-08-06T23:59:59Z".to_owned();
    let pointer_bytes = render_transparency_latest(&pointer).unwrap();
    fixture
        .transport
        .mutable_put(
            &pointer_destination,
            &pointer_bytes,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    fixture
        .transport
        .mutable_put(
            &signature_destination,
            &fake_signature(&format_latest_trusted_comment(&pointer)),
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    assert_eq!(
        fixture.publish(&FixedClock::new("2026-07-23T00:00:00Z").unwrap()),
        Err(TransparencyPublishError::ChainInvalid {
            observed: "pointer validity interval differs from fourteen days".to_owned(),
            expected: "valid_until equal to signed_at plus fourteen days".to_owned(),
        })
    );
}

#[test]
fn transparency_pointer_resigning_rejects_chain_identity_changes() {
    for change in ["chain-length", "tip-sha256"] {
        let fixture = OwnedPublisherFixture::new(&format!("resign-{change}"));
        fixture
            .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
            .unwrap();
        let pointer_destination = TransparencyObjectDestination {
            plane: TransparencyPlane::S3,
            key: format!("releases/{PRODUCT}/latest.json"),
        };
        let mut pointer: TransparencyLatestV1 = serde_json::from_slice(
            &fixture
                .transport
                .object_bytes(&pointer_destination)
                .expect("current pointer"),
        )
        .unwrap();
        match change {
            "chain-length" => pointer.chain_length += 1,
            "tip-sha256" => pointer.tip_sha256 = "f".repeat(64),
            _ => unreachable!(),
        }
        let pointer_bytes = render_transparency_latest(&pointer).unwrap();
        fixture
            .transport
            .mutable_put(
                &pointer_destination,
                &pointer_bytes,
                TransparencyCachePolicy::NoCache,
                None,
            )
            .unwrap();
        fixture
            .transport
            .mutable_put(
                &TransparencyObjectDestination {
                    plane: TransparencyPlane::S3,
                    key: format!("releases/{PRODUCT}/latest.json.minisig"),
                },
                &fake_signature(&format_latest_trusted_comment(&pointer)),
                TransparencyCachePolicy::NoCache,
                None,
            )
            .unwrap();
        assert_eq!(
            resign_transparency_pointer(
                &TransparencyResignRequest {
                    checkout_root: &fixture.checkout,
                    environment: &fixture.environment,
                    minisign_program: &fixture.minisign_program,
                    curl_program: &fixture.curl_program,
                },
                &fixture.transport,
                &fixture.runner,
                &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
            ),
            Err(TransparencyPublishError::ChainInvalid {
                observed: "pointer identity differs from the locked tip".to_owned(),
                expected: "tip digest and chain length equality".to_owned(),
            }),
            "{change}"
        );
    }
}

#[test]
fn transparency_empty_entry_pair_rejects_every_object_in_the_version_prefix() {
    let fixture = OwnedPublisherFixture::new("version-prefix-poison");
    fixture
        .publish(&FixedClock::new("2026-07-22T00:00:00Z").unwrap())
        .unwrap();
    let (release_dir, facts) = versioned_release_fixture(&fixture.checkout, "0.2.12");
    fixture
        .transport
        .create_only_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("releases/{PRODUCT}/v/0.2.12/stray-object"),
            },
            b"stray",
            TransparencyCachePolicy::Immutable,
        )
        .unwrap();
    assert_eq!(
        fixture.publish_candidate(
            &release_dir,
            &facts,
            &FixedClock::new("2026-07-23T00:00:00Z").unwrap(),
        ),
        Err(TransparencyPublishError::VersionPrefixPoisoned {
            version: "0.2.12".to_owned(),
            observed: "objects without a complete signed entry pair",
        })
    );
}

#[derive(Clone, Copy)]
enum RaceMode {
    Invalid,
    ValidOwn,
}

struct RaceTransport {
    inner: DirectoryTransparencyTransport,
    mode: RaceMode,
    triggered: AtomicBool,
}

impl TransparencyObjectTransport for RaceTransport {
    fn create_only_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        if destination.key.ends_with("/ledger-entry.json")
            && !self.triggered.swap(true, Ordering::SeqCst)
        {
            let (entry_bytes, signature) = match self.mode {
                RaceMode::Invalid => (
                    b"foreign conflicting bytes\n".to_vec(),
                    b"invalid\n".to_vec(),
                ),
                RaceMode::ValidOwn => {
                    let mut entry: TransparencyLedgerEntryV1 =
                        serde_json::from_slice(body).expect("parse attempted entry");
                    entry.published_utc = "2026-07-21T23:59:59Z".to_owned();
                    let entry_bytes =
                        render_transparency_entry(&entry).expect("render racing entry");
                    let comment = format_entry_trusted_comment(&entry, &entry_bytes);
                    let signature = format!(
                        "untrusted comment: fake signature\nFAKE\ntrusted comment: {comment}\nFAKE\n"
                    )
                    .into_bytes();
                    (entry_bytes, signature)
                }
            };
            self.inner
                .create_only_put(destination, &entry_bytes, cache)?;
            self.inner.create_only_put(
                &TransparencyObjectDestination {
                    plane: TransparencyPlane::S3,
                    key: format!("{}.minisig", destination.key),
                },
                &signature,
                cache,
            )?;
        }
        self.inner.create_only_put(destination, body, cache)
    }

    fn mutable_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
        if_match: Option<&str>,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.inner.mutable_put(destination, body, cache, if_match)
    }

    fn get(
        &self,
        destination: &TransparencyObjectDestination,
        cache: TransparencyFetchPolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.inner.get(destination, cache)
    }

    fn list(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.inner.list(destination)
    }
}

struct CrashTransport {
    inner: DirectoryTransparencyTransport,
    fail_public_at: Option<usize>,
    fail_mutable_at: AtomicUsize,
    public_calls: AtomicUsize,
    mutable_calls: AtomicUsize,
    move_pointer_on_recheck: bool,
    pointer_reads: AtomicUsize,
}

impl CrashTransport {
    fn arm_mutable_failure(&self, offset: usize) {
        self.fail_mutable_at.store(
            self.mutable_calls.load(Ordering::SeqCst) + offset,
            Ordering::SeqCst,
        );
    }
}

impl TransparencyObjectTransport for CrashTransport {
    fn create_only_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.inner.create_only_put(destination, body, cache)
    }

    fn mutable_put(
        &self,
        destination: &TransparencyObjectDestination,
        body: &[u8],
        cache: TransparencyCachePolicy,
        if_match: Option<&str>,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        let index = self.mutable_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_mutable_at.load(Ordering::SeqCst) == index {
            return Ok(ObservedHttpResponse {
                status: 503,
                body: b"injected mutable failure".to_vec(),
                etag: None,
            });
        }
        self.inner.mutable_put(destination, body, cache, if_match)
    }

    fn get(
        &self,
        destination: &TransparencyObjectDestination,
        cache: TransparencyFetchPolicy,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        if self.move_pointer_on_recheck
            && destination.plane == TransparencyPlane::S3
            && destination.key.ends_with("/latest.json")
            && self.pointer_reads.fetch_add(1, Ordering::SeqCst) == 1
        {
            return Ok(ObservedHttpResponse {
                status: 200,
                body: b"different concurrent pointer".to_vec(),
                etag: None,
            });
        }
        if destination.plane == TransparencyPlane::Public && destination.key.contains("/v/") {
            let index = self.public_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_public_at == Some(index) {
                return Ok(ObservedHttpResponse {
                    status: 200,
                    body: b"injected public mismatch".to_vec(),
                    etag: None,
                });
            }
        }
        self.inner.get(destination, cache)
    }

    fn list(
        &self,
        destination: &TransparencyListDestination,
    ) -> Result<ObservedHttpResponse, TransparencyTransportError> {
        self.inner.list(destination)
    }
}

#[test]
fn transparency_environment_resolution_reads_exactly_the_nine_contract_names() {
    let mut observed = Vec::new();
    let values = environment_values(&absolute_program("archive"));
    let resolved = resolve_transparency_environment_with(|name| {
        observed.push(name.to_owned());
        values.get(name).cloned()
    })
    .expect("resolve complete environment");
    assert_eq!(resolved.base_url, "https://transparency.solstone.app");
    let observed: BTreeSet<_> = observed.into_iter().collect();
    let expected: BTreeSet<_> = TRANSPARENCY_ENV_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(observed, expected);
    for delivery_surface_name in ["GH_TOKEN", "CLOUDFLARE_API_TOKEN", "WRANGLER_SEND_METRICS"] {
        assert!(!observed.contains(delivery_surface_name));
    }
}

#[test]
fn transparency_archive_receipt_parser_requires_an_exact_final_line() {
    let digest = "a".repeat(64);
    assert_eq!(
        parse_archive_receipt(format!("progress\nARCHIVED {digest}\n").as_bytes()),
        Ok(digest.clone())
    );
    for (label, bytes, expected) in [
        (
            "missing-line",
            b"progress\n".to_vec(),
            TransparencyPublishError::ArchiveReceiptInvalid {
                observed: "missing or malformed final receipt".to_owned(),
                expected: "ARCHIVED followed by one lowercase SHA-256".to_owned(),
            },
        ),
        (
            "wrong-digest-shape",
            format!("ARCHIVED {}\n", "b".repeat(63)).into_bytes(),
            TransparencyPublishError::ArchiveReceiptInvalid {
                observed: "missing or malformed final receipt".to_owned(),
                expected: "ARCHIVED followed by one lowercase SHA-256".to_owned(),
            },
        ),
        (
            "trailing-garbage",
            format!("ARCHIVED {digest}\ntrailing").into_bytes(),
            TransparencyPublishError::ArchiveReceiptInvalid {
                observed: "missing or malformed final receipt".to_owned(),
                expected: "ARCHIVED followed by one lowercase SHA-256".to_owned(),
            },
        ),
        (
            "trailing-blank",
            format!("ARCHIVED {digest}\n\n").into_bytes(),
            TransparencyPublishError::ArchiveReceiptInvalid {
                observed: "trailing blank or carriage-return data".to_owned(),
                expected: "one final ARCHIVED digest line".to_owned(),
            },
        ),
    ] {
        assert_eq!(parse_archive_receipt(&bytes), Err(expected), "{label}");
    }
}

#[test]
fn transparency_diagnostics_classify_observed_expected_and_one_remediation() {
    for error in [
        TransparencyPublishError::EnvironmentMissing { name: "REQUIRED" },
        TransparencyPublishError::GenesisValueInvalid,
        TransparencyPublishError::EnvironmentPathInvalid,
        TransparencyPublishError::ToolUnavailable {
            tool: "required-tool",
        },
        TransparencyPublishError::CandidateInvalid,
        TransparencyPublishError::CandidateChanged,
        TransparencyPublishError::ProofMissing,
        TransparencyPublishError::ProofInvalid,
        TransparencyPublishError::ChainFetch {
            observed: "HTTP 503".to_owned(),
            expected: "HTTP 200".to_owned(),
        },
        TransparencyPublishError::ChainInvalid {
            observed: "invalid chain".to_owned(),
            expected: "valid chain".to_owned(),
        },
        TransparencyPublishError::GenesisNotAuthorized,
        TransparencyPublishError::GenesisNotEmpty,
        TransparencyPublishError::Rollback {
            observed: 1,
            expected: 2,
        },
        TransparencyPublishError::VersionPoisoned {
            version: "0.2.11".to_owned(),
            source_commit: "1".repeat(40),
            seq: 1,
            sha256: "2".repeat(64),
        },
        TransparencyPublishError::VersionPrefixPoisoned {
            version: "0.2.11".to_owned(),
            observed: "an incomplete signed entry pair",
        },
        TransparencyPublishError::StageInvalid,
        TransparencyPublishError::StageConflict,
        TransparencyPublishError::SignatureFailed,
        TransparencyPublishError::ArchiveFailed {
            observed: "exit 1".to_owned(),
            expected: "exit 0".to_owned(),
        },
        TransparencyPublishError::ImmutableVerification,
        TransparencyPublishError::ArchiveReceiptInvalid {
            observed: "wrong digest".to_owned(),
            expected: "staged digest".to_owned(),
        },
        TransparencyPublishError::ImmutableWrite {
            observed: 403,
            expected: "HTTP 201".to_owned(),
        },
        TransparencyPublishError::ImmutableConflict,
        TransparencyPublishError::AdoptedRemoteEntry,
        TransparencyPublishError::ConcurrentPublish,
        TransparencyPublishError::MutableWrite {
            observed: 412,
            expected: "HTTP 200".to_owned(),
        },
        TransparencyPublishError::MutableVerification,
        TransparencyPublishError::HeadLogInvalid,
        TransparencyPublishError::HeadLogFork,
        TransparencyPublishError::HeadLogWrite,
        TransparencyPublishError::Process,
    ] {
        let diagnostic = error.to_string();
        assert_eq!(diagnostic.matches(';').count(), 1);
        assert!(diagnostic.contains("observed"));
        assert!(diagnostic.contains("expected"));
        assert!(diagnostic.contains("terminal") || diagnostic.contains("retryable"));
        assert!([
            "retry", "restore", "install", "rebuild", "prove", "approve", "audit", "cut",
            "discard", "correct",
        ]
        .iter()
        .any(|verb| diagnostic.contains(verb)));
        for private_marker in [
            "example.invalid",
            "fixture-bucket",
            "fixture-secret",
            "/tmp/",
        ] {
            assert!(!diagnostic.contains(private_marker));
        }
    }
}

fn assert_archive_fault_fails_before_publication(label: &str, archive_behavior: ArchiveBehavior) {
    let checkout = temporary_root(label);
    fs::write(checkout.join("transparency-head-log.jsonl"), b"").expect("create empty head log");
    let release_dir = fixture_release_dir();
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(release_dir.join(rust_release_manifest::companion_basename()))
            .expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");
    let facts = facts_for(&manifest);
    let archive_program = absolute_program("archive");
    let environment = environment(&archive_program);
    let runner = FakePublisherRunner {
        archive_program,
        archive_behavior,
        verification_behavior: VerificationBehavior::Valid,
        mutate_candidate_at_snapshot: None,
        phases: Mutex::new(Vec::new()),
    };
    let transport = DirectoryTransparencyTransport::new(checkout.join("fake-objects"));
    let clock = FixedClock::new("2026-07-22T00:00:00Z").expect("fixed clock");
    let evidence_dir = checkout.join("evidence");
    let minisign_program = absolute_program("minisign");
    let curl_program = absolute_program("curl");
    let error = publish_transparency(
        &TransparencyPublishRequest {
            checkout_root: &checkout,
            release_dir: &release_dir,
            evidence_dir: &evidence_dir,
            checkout_facts: &facts,
            environment: &environment,
            minisign_program: &minisign_program,
            curl_program: &curl_program,
        },
        &transport,
        &runner,
        &clock,
    )
    .expect_err("archive falsification must fail");
    let stage_root = checkout
        .join("target/release-transparency-stage")
        .join(PRODUCT)
        .join("0.2.11");
    let digest = xtask::transparency_format::transparency_sha256_hex(
        &fs::read(stage_root.join("stage-manifest.v1")).expect("staging manifest record"),
    );
    let expected = match archive_behavior {
        ArchiveBehavior::Failure => TransparencyPublishError::ArchiveFailed {
            observed: "exit status 17".to_owned(),
            expected: format!("exit 0 and ARCHIVED {digest}"),
        },
        ArchiveBehavior::WrongDigest => TransparencyPublishError::ArchiveReceiptInvalid {
            observed: "0".repeat(64),
            expected: digest,
        },
        ArchiveBehavior::Valid => unreachable!("valid archive is not a fault case"),
    };
    assert_eq!(error, expected);
    assert!(!stage_root.join("archive-ack.v1").exists());
    assert!(transport.calls().iter().all(|call| !matches!(
        call,
        RecordedTransparencyCall::CreateOnlyPut(_, _, _)
            | RecordedTransparencyCall::MutablePut(_, _, _, _)
    )));
}

fn run_crash_case(
    label: &str,
    fail_public_at: Option<usize>,
    fail_mutable_at: Option<usize>,
    move_pointer_on_recheck: bool,
    expect_committed_body: bool,
) {
    let checkout = temporary_root(label);
    fs::write(checkout.join("transparency-head-log.jsonl"), b"").expect("create empty head log");
    let release_dir = fixture_release_dir();
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(release_dir.join(rust_release_manifest::companion_basename()))
            .expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");
    let facts = facts_for(&manifest);
    let archive_program = absolute_program("archive");
    let environment = environment(&archive_program);
    let runner = FakePublisherRunner {
        archive_program,
        archive_behavior: ArchiveBehavior::Valid,
        verification_behavior: VerificationBehavior::Valid,
        mutate_candidate_at_snapshot: None,
        phases: Mutex::new(Vec::new()),
    };
    let transport = CrashTransport {
        inner: DirectoryTransparencyTransport::new(checkout.join("fake-objects")),
        fail_public_at,
        fail_mutable_at: AtomicUsize::new(fail_mutable_at.unwrap_or(usize::MAX)),
        public_calls: AtomicUsize::new(0),
        mutable_calls: AtomicUsize::new(0),
        move_pointer_on_recheck,
        pointer_reads: AtomicUsize::new(0),
    };
    let evidence_dir = checkout.join("evidence");
    let minisign_program = absolute_program("minisign");
    let curl_program = absolute_program("curl");
    let result = publish_transparency(
        &TransparencyPublishRequest {
            checkout_root: &checkout,
            release_dir: &release_dir,
            evidence_dir: &evidence_dir,
            checkout_facts: &facts,
            environment: &environment,
            minisign_program: &minisign_program,
            curl_program: &curl_program,
        },
        &transport,
        &runner,
        &FixedClock::new("2026-07-22T00:00:00Z").expect("fixed clock"),
    );
    assert_eq!(result.is_ok(), expect_committed_body);
    let pointer = transport
        .inner
        .object_bytes(&TransparencyObjectDestination {
            plane: TransparencyPlane::S3,
            key: format!("releases/{PRODUCT}/latest.json"),
        });
    assert_eq!(pointer.is_some(), expect_committed_body);
    if fail_public_at.is_some() {
        assert_eq!(result, Err(TransparencyPublishError::ImmutableVerification));
        assert!(transport
            .inner
            .calls()
            .iter()
            .all(|call| !matches!(call, RecordedTransparencyCall::MutablePut(_, _, _, _))));
    }
    if move_pointer_on_recheck {
        assert_eq!(result, Err(TransparencyPublishError::ConcurrentPublish));
    }
    if fail_mutable_at.is_some() {
        assert_eq!(
            result,
            Err(TransparencyPublishError::MutableWrite {
                observed: 503,
                expected: "HTTP 200..299".to_owned(),
            })
        );
        let retry = publish_transparency(
            &TransparencyPublishRequest {
                checkout_root: &checkout,
                release_dir: &release_dir,
                evidence_dir: &evidence_dir,
                checkout_facts: &facts,
                environment: &environment,
                minisign_program: &minisign_program,
                curl_program: &curl_program,
            },
            &transport,
            &runner,
            &FixedClock::new("2026-07-22T00:00:00Z").expect("retry clock"),
        )
        .expect("retry resumes the exact staged mutable pair");
        assert_eq!(retry.version, "0.2.11");
        assert!(transport
            .inner
            .object_bytes(&TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("releases/{PRODUCT}/latest.json"),
            })
            .is_some());
    }
}

fn run_race_case(label: &str, mode: RaceMode) {
    let checkout = temporary_root(label);
    fs::write(checkout.join("transparency-head-log.jsonl"), b"").expect("create empty head log");
    let release_dir = fixture_release_dir();
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(release_dir.join(rust_release_manifest::companion_basename())).unwrap(),
    )
    .unwrap();
    let facts = facts_for(&manifest);
    let archive_program = absolute_program("archive");
    let environment = environment(&archive_program);
    let runner = FakePublisherRunner {
        archive_program,
        archive_behavior: ArchiveBehavior::Valid,
        verification_behavior: VerificationBehavior::Valid,
        mutate_candidate_at_snapshot: None,
        phases: Mutex::new(Vec::new()),
    };
    let transport = RaceTransport {
        inner: DirectoryTransparencyTransport::new(checkout.join("fake-objects")),
        mode,
        triggered: AtomicBool::new(false),
    };
    let evidence_dir = checkout.join("evidence");
    let minisign_program = absolute_program("minisign");
    let curl_program = absolute_program("curl");
    let error = publish_transparency(
        &TransparencyPublishRequest {
            checkout_root: &checkout,
            release_dir: &release_dir,
            evidence_dir: &evidence_dir,
            checkout_facts: &facts,
            environment: &environment,
            minisign_program: &minisign_program,
            curl_program: &curl_program,
        },
        &transport,
        &runner,
        &FixedClock::new("2026-07-22T00:00:00Z").unwrap(),
    )
    .expect_err("racing create must stop before mutable writes");
    assert!(transport
        .inner
        .calls()
        .iter()
        .all(|call| !matches!(call, RecordedTransparencyCall::MutablePut(_, _, _, _))));
    match mode {
        RaceMode::Invalid => assert_eq!(
            error,
            TransparencyPublishError::VersionPrefixPoisoned {
                version: "0.2.11".to_owned(),
                observed: "an unparseable racing entry body",
            }
        ),
        RaceMode::ValidOwn => {
            assert_eq!(error, TransparencyPublishError::AdoptedRemoteEntry);
            let staged = fs::read(
                checkout
                    .join("target/release-transparency-stage")
                    .join(PRODUCT)
                    .join("0.2.11/archive/releases")
                    .join(PRODUCT)
                    .join("v/0.2.11/ledger-entry.json"),
            )
            .expect("persist adopted entry");
            let remote = transport
                .inner
                .object_bytes(&TransparencyObjectDestination {
                    plane: TransparencyPlane::S3,
                    key: format!("releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
                })
                .expect("read racing entry");
            assert_eq!(staged, remote);
            let completed = publish_transparency(
                &TransparencyPublishRequest {
                    checkout_root: &checkout,
                    release_dir: &release_dir,
                    evidence_dir: &evidence_dir,
                    checkout_facts: &facts,
                    environment: &environment,
                    minisign_program: &minisign_program,
                    curl_program: &curl_program,
                },
                &transport,
                &runner,
                &FixedClock::new("2026-07-22T00:00:00Z").unwrap(),
            )
            .expect("retry archives adopted bytes and resumes");
            assert_eq!(
                completed.entry_sha256,
                xtask::transparency_format::transparency_sha256_hex(&remote)
            );
        }
    }
}

struct OwnedPublisherFixture {
    checkout: PathBuf,
    release_dir: PathBuf,
    evidence_dir: PathBuf,
    facts: CheckoutFacts,
    archive_program: PathBuf,
    environment: xtask::transparency_publisher::TransparencyEnvironment,
    runner: FakePublisherRunner,
    transport: DirectoryTransparencyTransport,
    minisign_program: PathBuf,
    curl_program: PathBuf,
}

impl OwnedPublisherFixture {
    fn new(label: &str) -> Self {
        let checkout = temporary_root(label);
        fs::write(checkout.join("transparency-head-log.jsonl"), b"")
            .expect("create empty head log");
        let release_dir = fixture_release_dir();
        let manifest: Manifest = serde_json::from_slice(
            &fs::read(release_dir.join(rust_release_manifest::companion_basename()))
                .expect("read fixture manifest"),
        )
        .expect("parse fixture manifest");
        let facts = facts_for(&manifest);
        let archive_program = absolute_program("archive");
        Self {
            evidence_dir: checkout.join("evidence"),
            environment: environment(&archive_program),
            runner: FakePublisherRunner {
                archive_program: archive_program.clone(),
                archive_behavior: ArchiveBehavior::Valid,
                verification_behavior: VerificationBehavior::Valid,
                mutate_candidate_at_snapshot: None,
                phases: Mutex::new(Vec::new()),
            },
            transport: DirectoryTransparencyTransport::new(checkout.join("fake-objects")),
            minisign_program: absolute_program("minisign"),
            curl_program: absolute_program("curl"),
            checkout,
            release_dir,
            facts,
            archive_program,
        }
    }

    fn publish(
        &self,
        clock: &FixedClock,
    ) -> Result<xtask::transparency_publisher::TransparencyPublication, TransparencyPublishError>
    {
        self.publish_with(&self.environment, &self.runner, clock)
    }

    fn publish_with(
        &self,
        environment: &xtask::transparency_publisher::TransparencyEnvironment,
        runner: &FakePublisherRunner,
        clock: &FixedClock,
    ) -> Result<xtask::transparency_publisher::TransparencyPublication, TransparencyPublishError>
    {
        publish_transparency(
            &TransparencyPublishRequest {
                checkout_root: &self.checkout,
                release_dir: &self.release_dir,
                evidence_dir: &self.evidence_dir,
                checkout_facts: &self.facts,
                environment,
                minisign_program: &self.minisign_program,
                curl_program: &self.curl_program,
            },
            &self.transport,
            runner,
            clock,
        )
    }

    fn publish_candidate(
        &self,
        release_dir: &Path,
        facts: &CheckoutFacts,
        clock: &FixedClock,
    ) -> Result<xtask::transparency_publisher::TransparencyPublication, TransparencyPublishError>
    {
        publish_transparency(
            &TransparencyPublishRequest {
                checkout_root: &self.checkout,
                release_dir,
                evidence_dir: &self.evidence_dir,
                checkout_facts: facts,
                environment: &self.environment,
                minisign_program: &self.minisign_program,
                curl_program: &self.curl_program,
            },
            &self.transport,
            &self.runner,
            clock,
        )
    }
}

fn copy_directory(source: &Path, target: &Path) {
    fs::create_dir(target).expect("create candidate copy");
    for entry in fs::read_dir(source).expect("read fixture candidate") {
        let entry = entry.expect("read fixture candidate entry");
        assert!(entry.file_type().expect("fixture entry type").is_file());
        fs::copy(entry.path(), target.join(entry.file_name()))
            .expect("copy fixture candidate entry");
    }
}

fn versioned_release_fixture(checkout: &Path, version: &str) -> (PathBuf, CheckoutFacts) {
    let target = checkout.join(format!("release-dir-{version}"));
    fs::create_dir(&target).expect("create versioned candidate");
    for entry in fs::read_dir(fixture_release_dir()).expect("read base candidate") {
        let entry = entry.expect("read base candidate entry");
        assert!(entry.file_type().expect("base candidate type").is_file());
        let source_name = entry.file_name().to_string_lossy().into_owned();
        let target_name = source_name.replace("0.2.11", version);
        let bytes = fs::read(entry.path()).expect("read base candidate bytes");
        let text = String::from_utf8_lossy(&bytes).replace("0.2.11", version);
        fs::write(target.join(target_name), text.as_bytes()).expect("write versioned candidate");
    }
    let companion_path = target.join(rust_release_manifest::companion_basename());
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(&companion_path).unwrap()).unwrap();
    for artifact in &mut manifest.artifacts {
        let bytes = fs::read(target.join(&artifact.path)).expect("versioned artifact bytes");
        artifact.bytes = u64::try_from(bytes.len()).expect("artifact byte count");
        artifact.sha256 = xtask::transparency_format::transparency_sha256_hex(&bytes);
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    fs::write(&companion_path, manifest_bytes).expect("write versioned companion");
    let facts = facts_for(&manifest);
    (target, facts)
}

fn facts_for(manifest: &Manifest) -> CheckoutFacts {
    let root = repo_root();
    let projection = rust_release_manifest::project_release_toolchain(&root).unwrap();
    CheckoutFacts {
        product: PRODUCT.to_owned(),
        version: manifest.version.clone(),
        source_commit: manifest.source_commit.clone(),
        source_dirty: false,
        cargo_lock_sha256: manifest.cargo_lock_sha256.clone(),
        rustc_verbose: projection.rustc_verbose,
        cargo_version: projection.cargo_version,
        target_triple: TARGET_TRIPLE.to_owned(),
        target_profile: TARGET_PROFILE.to_owned(),
        target_features: TARGET_FEATURES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        cargo_deny_version: projection.cargo_deny_version,
        active_exceptions: rust_release_manifest::read_active_exceptions(&root).unwrap(),
        unsigned_native_tools: projection.unsigned_native_tools,
        signed_native_tools: projection.signed_native_tools,
    }
}

fn environment(archive_program: &Path) -> xtask::transparency_publisher::TransparencyEnvironment {
    let values = environment_values(archive_program);
    resolve_transparency_environment_with(|name| values.get(name).cloned())
        .expect("test environment")
}

fn environment_values(archive_program: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "TRANSPARENCY_S3_ENDPOINT".to_owned(),
            "https://objects.example.invalid".to_owned(),
        ),
        (
            "TRANSPARENCY_BUCKET".to_owned(),
            "fixture-bucket".to_owned(),
        ),
        (
            "TRANSPARENCY_S3_ACCESS_KEY_ID".to_owned(),
            "fixture-access".to_owned(),
        ),
        (
            "TRANSPARENCY_S3_SECRET_ACCESS_KEY".to_owned(),
            "fixture-secret".to_owned(),
        ),
        (
            "TRANSPARENCY_MINISIGN_KEY".to_owned(),
            absolute_program("secret-key").display().to_string(),
        ),
        (
            "TRANSPARENCY_MINISIGN_PUB".to_owned(),
            absolute_program("public-key").display().to_string(),
        ),
        (
            "TRANSPARENCY_ARCHIVE_CHANNEL".to_owned(),
            archive_program.display().to_string(),
        ),
        ("TRANSPARENCY_GENESIS".to_owned(), "1".to_owned()),
    ])
}

fn output(bytes: impl Into<Vec<u8>>) -> CommandOutput {
    CommandOutput {
        status: 0,
        stdout: bytes.into(),
        stderr: Vec::new(),
    }
}

fn fake_signature(trusted_comment: &str) -> Vec<u8> {
    format!("untrusted comment: fake signature\nFAKE\ntrusted comment: {trusted_comment}\nFAKE\n")
        .into_bytes()
}

fn recorded_call_name(call: &RecordedTransparencyCall) -> String {
    let plane = |plane| match plane {
        TransparencyPlane::S3 => "s3",
        TransparencyPlane::Public => "public",
    };
    match call {
        RecordedTransparencyCall::CreateOnlyPut(destination, _, _) => {
            format!("create:{}:{}", plane(destination.plane), destination.key)
        }
        RecordedTransparencyCall::MutablePut(destination, _, _, _) => {
            format!("mutable:{}:{}", plane(destination.plane), destination.key)
        }
        RecordedTransparencyCall::Get(destination, _) => {
            format!("get:{}:{}", plane(destination.plane), destination.key)
        }
        RecordedTransparencyCall::List(destination) => format!("list:{}", destination.prefix),
    }
}

fn overwrite_signed_entry_and_pointer(
    transport: &DirectoryTransparencyTransport,
    entry: &TransparencyLedgerEntryV1,
) {
    let entry_bytes = render_transparency_entry(entry).expect("render mutated entry");
    let entry_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/v/{}/ledger-entry.json", entry.version),
    };
    transport
        .mutable_put(
            &entry_destination,
            &entry_bytes,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    transport
        .mutable_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("{}.minisig", entry_destination.key),
            },
            &fake_signature(&format_entry_trusted_comment(entry, &entry_bytes)),
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();

    let pointer_destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: format!("releases/{PRODUCT}/latest.json"),
    };
    let mut pointer: TransparencyLatestV1 = serde_json::from_slice(
        &transport
            .object_bytes(&pointer_destination)
            .expect("current pointer"),
    )
    .unwrap();
    pointer.chain_length = entry.seq;
    pointer.tip_sha256 = xtask::transparency_format::transparency_sha256_hex(&entry_bytes);
    pointer.version = entry.version.clone();
    let pointer_bytes = render_transparency_latest(&pointer).unwrap();
    transport
        .mutable_put(
            &pointer_destination,
            &pointer_bytes,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    transport
        .mutable_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("releases/{PRODUCT}/latest.json.minisig"),
            },
            &fake_signature(&format_latest_trusted_comment(&pointer)),
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
    let first = transport
        .object_bytes(&TransparencyObjectDestination {
            plane: TransparencyPlane::S3,
            key: format!("releases/{PRODUCT}/v/0.2.11/ledger-entry.json"),
        })
        .expect("first locked entry");
    let mut ledger = first;
    ledger.extend_from_slice(&entry_bytes);
    transport
        .mutable_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: format!("releases/{PRODUCT}/ledger.jsonl"),
            },
            &ledger,
            TransparencyCachePolicy::NoCache,
            None,
        )
        .unwrap();
}

fn argument_value<'a>(args: &'a [String], name: &str) -> &'a str {
    let index = args
        .iter()
        .position(|argument| argument == name)
        .expect("fake argument present");
    &args[index + 1]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask workspace parent")
        .to_path_buf()
}

fn fixture_release_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-release-manifest/release-dir")
}

fn temporary_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "solstone-transparency-publisher-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temporary root");
    root
}

#[cfg(not(windows))]
fn absolute_program(name: &str) -> PathBuf {
    PathBuf::from(format!("/fake-tools/{name}"))
}

#[cfg(windows)]
fn absolute_program(name: &str) -> PathBuf {
    PathBuf::from(format!(r"C:\fake-tools\{name}.exe"))
}
