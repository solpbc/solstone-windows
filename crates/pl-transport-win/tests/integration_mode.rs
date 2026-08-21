// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! End-to-end contract tests for the operator integration mode.
//!
//! These live here rather than in `src-tauri/` because no repository gate
//! compiles the app crate — a test placed there would never run.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use observer_pl::frame::{Frame, FLAG_CLOSE, FLAG_DATA};
use pl_transport_win::credential::{Credential, PairedState};
use pl_transport_win::integration::report::{
    Outcome, EXIT_ASSERTION_FAILED, EXIT_ERROR, EXIT_PASS, SCHEMA_VERSION,
};
use pl_transport_win::integration::{self, Carrier, Environment};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{direct_credential, read_framed_request, self_signed, server_config};

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "plw-mode-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn environment(root: &Path) -> Environment {
    Environment {
        state_path: root.join("pairing.json"),
        segments_root: root.join("segments"),
        device_label: "build-box".into(),
        platform: "windows".into(),
        stream_type: "desktop".into(),
        app_version: "9.9.9".into(),
        period_secs: 300,
        executable: None,
        source_commit: None,
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Run the mode the way the binary does and return its terminal outcome.
fn outcome_for(argv: &[&str], root: &Path) -> Outcome {
    let selection = integration::selected(&args(argv)).expect("the mode was selected");
    integration::report_for(selection, &environment(root))
}

fn outcome_value(outcome: &Outcome) -> serde_json::Value {
    let json = outcome.json();
    // Exactly one object: the whole string parses as a single JSON value.
    let value: serde_json::Value =
        serde_json::from_str(&json).expect("stdout carries one parseable JSON object");
    assert!(!json.trim_end().contains('\n'), "one line, one object");
    value
}

/// Run the mode the way the binary does and return the parsed stdout object plus
/// the exit status.
fn run(argv: &[&str], root: &Path) -> (serde_json::Value, u8) {
    let outcome = outcome_for(argv, root);
    let value = outcome_value(&outcome);
    (value, outcome.exit_code)
}

fn start_direct_upload_journal(
    confirmed: bool,
    payload: &[u8],
) -> (Credential, std::thread::JoinHandle<()>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let sha256 = observer_pl::ca::sha256_hex(payload);
    let size = payload.len() as u64;
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            serve_direct_upload_journal(listener, acceptor, confirmed, size, &sha256).await;
        });
    });
    (direct_credential(pin, port), server)
}

async fn serve_direct_upload_journal(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    confirmed: bool,
    size: u64,
    sha256: &str,
) {
    let day = "20260617";
    let segment = "143000_300";
    let file =
        format!(r#"{{"name":"payload.bin","size":{size},"sha256":"{sha256}","status":"present"}}"#);
    let day_manifest = if confirmed {
        format!(r#"{{"version":1,"day":"{day}","segments":{{"{segment}":{{"files":[{file}]}}}}}}"#)
    } else {
        format!(r#"{{"version":1,"day":"{day}","segments":{{}}}}"#)
    };
    let segments = if confirmed {
        format!(
            r#"{{"items":[{{"key":"{segment}","observed":true,"files":[{file}]}}],"total":1,"protocol_version":3}}"#
        )
    } else {
        r#"{"items":[],"total":0,"protocol_version":3}"#.to_string()
    };
    let responses = [
        (
            "POST /app/devices/ingest HTTP/1.1\r\n",
            format!(r#"{{"status":"ok","segment":"{segment}"}}"#),
        ),
        (
            "GET /app/devices/ingest/manifest HTTP/1.1\r\n",
            format!(r#"{{"days":{{"{day}":{{"segments":1}}}}}}"#),
        ),
        (
            "GET /app/devices/ingest/manifest/20260617 HTTP/1.1\r\n",
            day_manifest,
        ),
        (
            "GET /app/devices/ingest/segments/20260617 HTTP/1.1\r\n",
            segments,
        ),
    ];

    for (route, body) in responses {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let (stream_id, request) = read_framed_request(&mut tls).await;
        assert!(
            request.starts_with(route.as_bytes()),
            "expected route {route:?}, got {:?}",
            String::from_utf8_lossy(&request)
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        tls.flush().await.unwrap();
        let _ = tls.shutdown().await;
    }
}

fn save_direct_pairing(root: &Path, credential: Credential) {
    PairedState {
        credential: Some(credential),
        observer_key: None,
        observer_name: None,
    }
    .save(&environment(root).state_path)
    .unwrap();
}

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn the_mode_is_selected_only_by_its_own_flag() {
    assert!(integration::selected(&args(&["--integration", "pair"])).is_some());
    assert!(integration::selected(&args(&["--dump-state"])).is_none());
    assert!(integration::selected(&args(&[])).is_none());
    // Normal GUI startup is untouched: a bare launch still surfaces.
    assert!(observer_model::launch_should_surface::<&str>(&[]));
    assert!(!observer_model::launch_should_surface(&["--integration"]));
}

#[test]
fn every_attempted_operation_emits_one_schema_versioned_object() {
    let root = temp_root("one-object");
    // A missing operation, an unknown operation, and a valid-but-unpaired
    // operation all still owe exactly one envelope.
    for argv in [
        vec!["--integration"],
        vec!["--integration", "nonsense"],
        vec!["--integration", "roundtrip", "--deadline-secs", "5"],
    ] {
        let (value, code) = run(&argv, &root);
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert!(value["operation"].is_string());
        assert!(value["reason"].is_string(), "a failure names a reason");
        assert!(value["elapsed_ms"].is_number());
        assert!(value["retryable"].is_boolean());
        assert!(value["guidance"].is_string());
        assert!(value["artifact"]["app_version"] == "9.9.9");
        assert!(value["dials"]["total"].is_number());
        assert_ne!(code, EXIT_PASS, "none of these can pass");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rejected_invocation_is_an_error_verdict_with_the_error_exit_status() {
    let root = temp_root("rejected");
    for argv in [
        vec!["--integration"],
        vec!["--integration", "nope"],
        vec!["--integration", "pair"], // missing --deadline-secs
        vec!["--integration", "roundtrip", "--deadline-secs", "0"],
        vec!["--integration", "roundtrip", "--deadline-secs", "abc"],
    ] {
        let (value, code) = run(&argv, &root);
        assert_eq!(value["verdict"], "ERROR", "{argv:?}");
        assert_eq!(value["phase"], "validate", "{argv:?}");
        assert_eq!(code, EXIT_ERROR, "{argv:?}");
        assert_eq!(value["dials"]["total"], 0, "a rejection dials nothing");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_malformed_flag_value_is_never_echoed_back_into_the_envelope() {
    let root = temp_root("no-echo");
    let secret = "S3CRET-token-value";
    let (value, code) = run(
        &[
            "--integration",
            "fetch",
            "--deadline-secs",
            "5",
            "--journal-path",
            "/day/20260617",
            "--expected-bytes",
            "2097152",
            "--expected-sha256",
            secret,
            "--expected-status",
            "200",
        ],
        &root,
    );
    assert_eq!(code, EXIT_ERROR);
    let rendered = serde_json::to_string(&value).unwrap();
    assert!(!rendered.contains(secret));
    assert!(!rendered.contains("S3CRET"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_large_fetch_precondition_is_enforced_before_any_network_work() {
    let root = temp_root("fetch-window");
    let window = observer_pl::mux::INITIAL_WINDOW as u64;
    for bytes in [0, 1, window] {
        let (value, code) = run(
            &[
                "--integration",
                "fetch",
                "--deadline-secs",
                "5",
                "--journal-path",
                "/day/20260617",
                "--expected-bytes",
                &bytes.to_string(),
                "--expected-sha256",
                SHA,
                "--expected-status",
                "200",
            ],
            &root,
        );
        assert_eq!(value["reason"], "expected_bytes_not_over_window");
        assert_eq!(code, EXIT_ERROR);
        assert_eq!(value["dials"]["total"], 0);
    }

    // One byte over the window clears the precondition and fails later, on the
    // profile — proving the gate is the byte count, not the flag's presence.
    let (value, _) = run(
        &[
            "--integration",
            "fetch",
            "--deadline-secs",
            "5",
            "--journal-path",
            "/day/20260617",
            "--expected-bytes",
            &(window + 1).to_string(),
            "--expected-sha256",
            SHA,
            "--expected-status",
            "200",
        ],
        &root,
    );
    assert_eq!(value["phase"], "precondition");
    assert_eq!(value["reason"], "paired_credential_missing");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn operations_needing_a_pairing_refuse_an_unpaired_profile() {
    let root = temp_root("unpaired");
    let payload = root.join("payload.bin");
    std::fs::write(&payload, vec![7u8; 1024]).unwrap();

    let cases: Vec<Vec<String>> = vec![
        args(&["--integration", "roundtrip", "--deadline-secs", "5"]),
        args(&[
            "--integration",
            "upload",
            "--deadline-secs",
            "5",
            "--payload",
            payload.to_str().unwrap(),
            "--day",
            "20260617",
            "--segment",
            "143000_300",
            "--carrier",
            "direct",
        ]),
    ];
    for argv in cases {
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (value, code) = run(&borrowed, &root);
        assert_eq!(value["phase"], "precondition", "{argv:?}");
        assert_eq!(value["reason"], "paired_credential_missing", "{argv:?}");
        assert_eq!(code, EXIT_ERROR, "{argv:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_malformed_pairing_file_is_an_error_not_a_silent_unpaired_default() {
    let root = temp_root("malformed-state");
    let environment = environment(&root);
    std::fs::write(&environment.state_path, b"{ this is not json").unwrap();

    let (value, code) = run(
        &["--integration", "roundtrip", "--deadline-secs", "5"],
        &root,
    );
    assert_eq!(value["phase"], "precondition");
    assert_eq!(value["reason"], "pairing_state_unavailable");
    assert_eq!(code, EXIT_ERROR);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn upload_refuses_an_unrepresentable_caller_named_segment() {
    let root = temp_root("bad-segment");
    let payload = root.join("payload.bin");
    std::fs::write(&payload, vec![7u8; 1024]).unwrap();

    // Shape-valid but not a real instant: rejected by the parser's shape check or
    // by the round-trip through production key derivation.
    let (value, code) = run(
        &[
            "--integration",
            "upload",
            "--deadline-secs",
            "5",
            "--payload",
            payload.to_str().unwrap(),
            "--day",
            "20260231",
            "--segment",
            "143000_300",
        ],
        &root,
    );
    assert_ne!(code, EXIT_PASS);
    assert_eq!(value["verdict"], "ERROR");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn artifact_identity_is_reported_or_honestly_absent() {
    let root = temp_root("artifact");
    let executable = root.join("fake-app.exe");
    std::fs::write(&executable, b"executable bytes").unwrap();
    let commit = "0123456789abcdef0123456789abcdef01234567";

    let mut environment = environment(&root);
    environment.executable = Some(executable.clone());
    environment.source_commit = Some(commit.to_string());

    let selection = integration::selected(&args(&["--integration"])).unwrap();
    let outcome = integration::report_for(selection, &environment);
    let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();

    assert_eq!(value["artifact"]["source_commit"], commit);
    assert_eq!(
        value["artifact"]["executable_sha256"],
        observer_pl::ca::sha256_hex(b"executable bytes")
    );

    // An unstamped build reports null rather than a placeholder.
    let mut unstamped = environment.clone();
    unstamped.source_commit = Some("not-a-commit".into());
    unstamped.executable = Some(root.join("missing.exe"));
    let selection = integration::selected(&args(&["--integration"])).unwrap();
    let outcome = integration::report_for(selection, &unstamped);
    let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
    assert!(value["artifact"]["source_commit"].is_null());
    assert!(value["artifact"]["executable_sha256"].is_null());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_dial_ceiling_is_reported_even_when_it_is_not_exceeded() {
    let root = temp_root("max-dials");
    let (value, _) = run(
        &[
            "--integration",
            "roundtrip",
            "--deadline-secs",
            "5",
            "--max-dials",
            "3",
        ],
        &root,
    );
    assert_eq!(value["dials"]["max_allowed"], 3);
    assert_eq!(value["dials"]["total"], 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A full upload can earn a custody witness over direct PL but must still fail
/// when the operator required the relay carrier.
#[test]
fn an_upload_carrier_mismatch_is_a_nonzero_terminal_outcome() {
    let root = temp_root("carrier-mismatch");
    let payload = b"carrier-mismatch-payload";
    let payload_path = root.join("payload.bin");
    std::fs::write(&payload_path, payload).unwrap();
    let (credential, server) = start_direct_upload_journal(true, payload);
    save_direct_pairing(&root, credential);

    let argv = vec![
        "--integration".to_string(),
        "upload".to_string(),
        "--deadline-secs".to_string(),
        "5".to_string(),
        "--payload".to_string(),
        payload_path.display().to_string(),
        "--day".to_string(),
        "20260617".to_string(),
        "--segment".to_string(),
        "143000_300".to_string(),
        "--carrier".to_string(),
        "relay".to_string(),
    ];
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let outcome = outcome_for(&borrowed, &root);
    let value = outcome_value(&outcome);
    server.join().unwrap();

    assert_eq!(outcome.exit_code, EXIT_ASSERTION_FAILED);
    assert_eq!(value["verdict"], "FAIL");
    assert_eq!(value["reason"], "direct_path_success");
    assert_eq!(value["evidence"]["requested_carrier"], "relay");
    assert_eq!(value["evidence"]["observed_carrier"], "direct");
    assert_eq!(value["evidence"]["confirmed"], true);
    assert_eq!(value["evidence"]["server_submitted_name"], "payload.bin");
    assert_eq!(value["evidence"]["server_size"], payload.len() as u64);
    assert_eq!(
        value["evidence"]["server_custody_status"], "present",
        "custody succeeded before the carrier assertion"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn requested_carrier_uses_the_operator_cli_vocabulary() {
    assert_eq!(Carrier::Relay.as_str(), "relay");
    assert_eq!(Carrier::Direct.as_str(), "direct");
}
