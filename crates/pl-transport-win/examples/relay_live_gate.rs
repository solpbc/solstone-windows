// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Live relay ingest gate (operator-direct, not part of `make ci`).
//!
//! Drives one established observer request through the SPL relay carrier. The
//! inner mTLS identity comes from an existing paired-state JSON file; relay
//! pairing/enrollment is still external to this example, so the relay token and
//! instance are supplied by env.
//!
//! Required env:
//! - `SOLSTONE_CREDENTIAL_FILE` — path to a `PairedState` JSON with credential
//!   and observer handle.
//! - `SOLSTONE_RELAY_ORIGIN` — e.g. `https://link.solstone.app`.
//! - `SOLSTONE_INSTANCE_ID` — relay instance id to place in the dial URL.
//! - `SOLSTONE_DEVICE_TOKEN` — enrolled relay device token for outer WS auth.
//!
//! Optional env:
//! - `SOLSTONE_REAL_SCREEN_FILE` — upload a real screen MP4 payload. If absent,
//!   the example sends a tiny deterministic marker body.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::SCREEN_FILE_NAME;
use observer_pl::multipart::{self, FilePart};
use observer_pl::{
    civil, paths, OBSERVER_HANDLE_HEADER, OBSERVER_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use pl_transport_win::credential::PairedState;
use pl_transport_win::relay::request_once_relay;
use pl_transport_win::tls;

const PERIOD_SECS: u64 = 300;
const PLATFORM: &str = "windows";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let credential_file =
        std::env::var("SOLSTONE_CREDENTIAL_FILE").expect("set SOLSTONE_CREDENTIAL_FILE");
    let relay_origin = std::env::var("SOLSTONE_RELAY_ORIGIN").expect("set SOLSTONE_RELAY_ORIGIN");
    let instance_id = std::env::var("SOLSTONE_INSTANCE_ID").expect("set SOLSTONE_INSTANCE_ID");
    let device_token = std::env::var("SOLSTONE_DEVICE_TOKEN").expect("set SOLSTONE_DEVICE_TOKEN");

    let state = PairedState::load(std::path::Path::new(&credential_file))
        .expect("load SOLSTONE_CREDENTIAL_FILE");
    let credential = state
        .credential
        .expect("SOLSTONE_CREDENTIAL_FILE must contain a paired credential");
    let observer_key = state
        .observer_key
        .expect("SOLSTONE_CREDENTIAL_FILE must contain an observer handle");

    let chain = tls::parse_certs(&credential.client_cert_pem).expect("parse client cert");
    let key = tls::parse_private_key(&credential.client_key_pem).expect("parse client key");
    let inner_config = Arc::new(
        tls::mtls_config(&credential.ca_fp_prefix, chain, key).expect("build inner mTLS config"),
    );

    let screen_payload = if let Ok(path) = std::env::var("SOLSTONE_REAL_SCREEN_FILE") {
        std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read SOLSTONE_REAL_SCREEN_FILE {path}: {e}"))
    } else {
        b"solstone-windows-w1-relay-live-screen".to_vec()
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let boundary_start = (now / PERIOD_SECS) * PERIOD_SECS;
    let day = civil::day_string(boundary_start);
    let segment = civil::segment_key_string(boundary_start, PERIOD_SECS);
    let boundary = format!("----solstonewindowsrelaylive{}", std::process::id());
    let fields = [
        ("segment", segment.as_str()),
        ("day", day.as_str()),
        ("platform", PLATFORM),
    ];
    let files = [FilePart {
        filename: SCREEN_FILE_NAME.to_string(),
        content_type: "video/mp4".to_string(),
        bytes: screen_payload,
    }];
    let body = multipart::build(&boundary, &fields, &files);
    let headers = vec![
        (OBSERVER_HANDLE_HEADER.to_string(), observer_key.clone()),
        (
            "Authorization".to_string(),
            format!("Bearer {observer_key}"),
        ),
        (
            PROTOCOL_VERSION_HEADER.to_string(),
            OBSERVER_PROTOCOL_VERSION.to_string(),
        ),
        (
            "Content-Type".to_string(),
            multipart::content_type(&boundary),
        ),
    ];

    let response = request_once_relay(
        inner_config,
        &relay_origin,
        &instance_id,
        &device_token,
        "POST",
        paths::INGEST,
        &headers,
        &body,
    )
    .await
    .expect("relay ingest request failed");

    println!(
        "RELAY: status={} body={}",
        response.status,
        response.body_text()
    );
    assert!(
        response.is_success(),
        "relay ingest returned HTTP {}",
        response.status
    );
    println!("RELAY_LIVE_GATE_PASS");
}
