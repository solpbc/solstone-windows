// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Operator-direct relay-form pairing gate (not part of `make ci`).
//!
//! Runs the real relay-form (`0x06`) pairing ceremony against a live relay +
//! home: parse a relay pair-link, pair over the relay (pair-dial → CSR over the
//! tunnel → live-peer SPKI pin → enroll/device), register the
//! observer, then persist the resulting [`PairedState`] (credential + observer
//! handle) to a JSON file and print the relay env (`relay_origin`, `instance_id`,
//! `device_token`) that [`relay_live_gate`](relay_live_gate.rs) consumes for a
//! direct-relay ingest. Together the two gates prove off-LAN relay reach on real
//! hardware without forcing any network topology — `relay_live_gate` dials the
//! relay directly, never the LAN.
//!
//! Usage: `SOLSTONE_PAIR_LINK='https://go.solstone.app/p#…' (a 0x06 relay link)
//! SOLSTONE_CREDENTIAL_FILE=/path/pairing.json cargo run -p pl-transport-win
//! --example relay_pair_gate`. Because rustls + tokio-tungstenite are
//! cross-platform this runs on Linux or Windows alike.

use pl_transport_win::client::ObserverClient;
use pl_transport_win::credential::PairedState;
use pl_transport_win::pairing;

const PLATFORM: &str = "windows";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let link = std::env::var("SOLSTONE_PAIR_LINK")
        .expect("set SOLSTONE_PAIR_LINK to a fresh 0x06 relay pair-link from `sol call link pair`");
    let credential_file = std::env::var("SOLSTONE_CREDENTIAL_FILE")
        .expect("set SOLSTONE_CREDENTIAL_FILE to the output pairing.json path");
    let device_label = std::env::var("SOLSTONE_DEVICE_LABEL")
        .unwrap_or_else(|_| "win-relay-pair-gate".to_string());

    // 1. Pair relay-form over the live relay.
    let credential = pairing::pair_from_link(&link, &device_label)
        .await
        .expect("relay-form pairing failed");

    // The whole point: a relay-form pairing must carry relay coords.
    let relay_origin = credential
        .relay_origin
        .clone()
        .expect("paired credential has no relay_origin — was this a 0x06 relay link?");
    let device_token = credential
        .device_token
        .clone()
        .expect("paired credential has no device_token — relay enrollment did not complete");
    println!(
        "PAIRED (relay-form): journal='{}' instance={} relay_origin={} device_token={}… endpoints={:?}",
        credential.home_label,
        credential.instance_id,
        relay_origin,
        &device_token[..device_token.len().min(8)],
        credential.endpoints,
    );

    // 2. Register the observer (over LAN-first/relay-fallback — either path is fine here).
    let mut client = ObserverClient::new(credential.clone()).expect("client build failed");
    let registration = client
        .register(
            PLATFORM,
            &device_label,
            "desktop",
            env!("CARGO_PKG_VERSION"),
            None,
        )
        .await
        .expect("register failed");
    println!(
        "REGISTERED: stream='{}' key_prefix={}",
        registration.name,
        &registration.key[..registration.key.len().min(8)],
    );

    // 3. Persist the credential + handle for relay_live_gate to consume.
    let state = PairedState {
        credential: Some(credential),
        observer_key: Some(registration.key.clone()),
        observer_name: Some(registration.name.clone()),
    };
    state
        .save(std::path::Path::new(&credential_file))
        .expect("save credential file failed");
    println!("SAVED: {credential_file}");

    // 4. Print the relay env relay_live_gate needs.
    let instance_id = state
        .credential
        .as_ref()
        .map(|c| c.instance_id.clone())
        .unwrap_or_default();
    println!("RELAY_PAIR_GATE_PASS");
    println!("  SOLSTONE_RELAY_ORIGIN={relay_origin}");
    println!("  SOLSTONE_INSTANCE_ID={instance_id}");
    println!("  SOLSTONE_DEVICE_TOKEN={device_token}");
    println!("  SOLSTONE_CREDENTIAL_FILE={credential_file}");
}
