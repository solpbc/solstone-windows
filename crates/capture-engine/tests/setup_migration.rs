// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use capture_engine::{FilePauseStore, HeldPause, PauseStore};
use observer_mic::MicConfig;
use observer_model::setup::{load_or_classify, LoadOutcome, SetupConfig};
use pl_transport_win::credential::PairedState;

fn temp_root(label: &str) -> PathBuf {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from("/var/tmp").join(format!(
        "solstone-setup-migration-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

// Independent plaintext pairing json with distinct pem, endpoint, and label.
// Plaintext client key (base64) accepted by PairedState::load without unprotect.
const DISTINCT_PAIRING_JSON: &str = r#"{
  "credential": {
    "client_key_pem": "S0tLSw==",
    "client_cert_pem": "-----BEGIN CERTIFICATE-----\nMIIB_DISTINCT_CLIENT_CERT_PEM\n-----END CERTIFICATE-----",
    "ca_chain_pem": [
      "-----BEGIN CERTIFICATE-----\nMIIB_DISTINCT_CA_CHAIN_PEM\n-----END CERTIFICATE-----"
    ],
    "ca_fp_prefix": [12, 34, 56, 78],
    "instance_id": "inst-distinct-migration-999",
    "home_label": "Distinct Migration Journal",
    "endpoints": [
      {
        "host": "192.168.42.42",
        "port": 9443
      }
    ],
    "relay_origin": null,
    "device_token": "ZGlzdGluY3QtdG9rZW4=",
    "device_token_expires_at": null
  }
}
"#;

#[test]
fn missing_file_with_paired_fixture_migrates_to_legacy_and_preserves_bytes() {
    let root = temp_root("paired");
    let pairing_path = root.join("pairing.json");
    let mic_path = root.join("mic.json");
    let pause_path = root.join("pause.txt");

    // Write pairing.json (distinct, unprotect-free plaintext base64)
    fs::write(&pairing_path, DISTINCT_PAIRING_JSON.as_bytes()).unwrap();

    // Write mic.json with priority, disabled: ["mic-device-independent-1"], gain: 4
    let mic_cfg = MicConfig {
        priority: vec!["mic-preferred-input-1".to_string()],
        disabled: vec!["mic-device-independent-1".to_string()],
        gain: 4,
    };
    let mic_bytes = serde_json::to_vec_pretty(&mic_cfg).unwrap();
    fs::write(&mic_path, &mic_bytes).unwrap();

    // Write pause.txt with FilePauseStore as HeldPause::UntilResumed
    let pause_store = FilePauseStore::new(pause_path.clone());
    pause_store.save(HeldPause::UntilResumed).unwrap();

    let initial_pairing_bytes = fs::read(&pairing_path).unwrap();
    let initial_mic_bytes = fs::read(&mic_path).unwrap();
    let initial_pause_bytes = fs::read(&pause_path).unwrap();

    // No setup.json exists yet
    assert!(!root.join("setup.json").exists());

    // load_or_classify is legacy
    let first = load_or_classify(&root);
    assert_eq!(first, LoadOutcome::Legacy(SetupConfig::legacy()));

    // The three files' bytes are unchanged
    assert_eq!(fs::read(&pairing_path).unwrap(), initial_pairing_bytes);
    assert_eq!(fs::read(&mic_path).unwrap(), initial_mic_bytes);
    assert_eq!(fs::read(&pause_path).unwrap(), initial_pause_bytes);

    // PairedState::load, MicConfig, and FilePauseStore::load return those values
    let paired = PairedState::load(&pairing_path).unwrap();
    assert!(paired.is_paired());
    let cred = paired.credential.unwrap();
    assert_eq!(cred.home_label, "Distinct Migration Journal");
    assert_eq!(cred.instance_id, "inst-distinct-migration-999");
    assert_eq!(cred.endpoints[0].host, "192.168.42.42");
    assert_eq!(cred.endpoints[0].port, 9443);
    assert_eq!(cred.client_key_pem, "S0tLSw==");
    assert_eq!(cred.device_token.as_deref(), Some("ZGlzdGluY3QtdG9rZW4="));

    let loaded_mic: MicConfig = serde_json::from_slice(&fs::read(&mic_path).unwrap()).unwrap();
    assert_eq!(loaded_mic, mic_cfg);

    let loaded_pause = pause_store.load();
    assert_eq!(loaded_pause, Some(HeldPause::UntilResumed));

    // Second load_or_classify is Valid, not legacy
    let second = load_or_classify(&root);
    assert_eq!(second, LoadOutcome::Valid(SetupConfig::legacy()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_file_with_unpaired_prior_use_migrates_to_legacy_and_preserves_bytes() {
    let root = temp_root("unpaired");
    let mic_path = root.join("mic.json");
    let pause_path = root.join("pause.txt");

    // Write mic.json with priority, disabled: ["mic-device-independent-1"], gain: 4
    let mic_cfg = MicConfig {
        priority: vec!["mic-preferred-input-2".to_string()],
        disabled: vec!["mic-device-independent-1".to_string()],
        gain: 4,
    };
    let mic_bytes = serde_json::to_vec_pretty(&mic_cfg).unwrap();
    fs::write(&mic_path, &mic_bytes).unwrap();

    // Write pause.txt with FilePauseStore as HeldPause::Until(1_735_000_000) and no pairing file
    let pause_store = FilePauseStore::new(pause_path.clone());
    pause_store.save(HeldPause::Until(1_735_000_000)).unwrap();

    let initial_mic_bytes = fs::read(&mic_path).unwrap();
    let initial_pause_bytes = fs::read(&pause_path).unwrap();

    // No setup.json and no pairing.json
    assert!(!root.join("setup.json").exists());
    assert!(!root.join("pairing.json").exists());

    // load_or_classify is legacy
    let first = load_or_classify(&root);
    assert_eq!(first, LoadOutcome::Legacy(SetupConfig::legacy()));

    // Bytes unchanged
    assert_eq!(fs::read(&mic_path).unwrap(), initial_mic_bytes);
    assert_eq!(fs::read(&pause_path).unwrap(), initial_pause_bytes);

    // MicConfig and FilePauseStore::load return those values
    let loaded_mic: MicConfig = serde_json::from_slice(&fs::read(&mic_path).unwrap()).unwrap();
    assert_eq!(loaded_mic, mic_cfg);

    let loaded_pause = pause_store.load();
    assert_eq!(loaded_pause, Some(HeldPause::Until(1_735_000_000)));

    // Second load_or_classify is Valid, not legacy
    let second = load_or_classify(&root);
    assert_eq!(second, LoadOutcome::Valid(SetupConfig::legacy()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn dark_startup_creates_nothing_and_retains_existing_off_and_corrupt_choices() {
    use observer_model::setup::{atomic_write_setup, load_for_startup};
    let root = std::env::temp_dir().join(format!("solstone-dark-startup-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let missing = load_for_startup(&root, false);
    assert!(
        matches!(missing, LoadOutcome::LegacyUnsaved(ref cfg) if cfg.screen && cfg.mic && cfg.system_audio && cfg.start_at_login.choice() == Some(true) && cfg.notice_dismissed)
    );
    assert!(!root.join("setup.json").exists());
    let off = SetupConfig::fresh();
    atomic_write_setup(&root, &off).unwrap();
    let before = fs::read(root.join("setup.json")).unwrap();
    assert!(matches!(load_for_startup(&root, false), LoadOutcome::Valid(ref cfg) if cfg == &off));
    assert_eq!(fs::read(root.join("setup.json")).unwrap(), before);
    fs::write(root.join("setup.json"), b"corrupt").unwrap();
    assert!(
        matches!(load_for_startup(&root, false), LoadOutcome::FailedClosed(ref cfg) if !cfg.screen && !cfg.mic && !cfg.system_audio && cfg.start_at_login.choice().is_none())
    );
    assert_eq!(fs::read(root.join("setup.json")).unwrap(), b"corrupt");
    fs::remove_dir_all(root).unwrap();
}
