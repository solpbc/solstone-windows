// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use pl_transport_win::integration::report::{EXIT_PASS, SCHEMA_VERSION};
use pl_transport_win::integration::{self, Environment};

use support::relay_pairing::{relay_form_link, spawn_mock_relay, MockState, PAIR_SECRET};

const CHILD_ENV: &str = "SOLSTONE_INTEGRATION_STACK_CHILD";
const APP_MAIN_STACK_BYTES: usize = 1 << 20;
const TEST_NAME: &str = "integration_pair_emits_one_envelope_from_one_mib_caller_stack";

fn temp_root(process_id: u32) -> PathBuf {
    std::env::temp_dir().join(format!("plw-integration-stack-{process_id}"))
}

fn environment(root: &Path) -> Environment {
    Environment {
        state_path: root.join("pairing.json"),
        segments_root: root.join("segments"),
        device_label: "stack-test".into(),
        app_version: "0.0.0".into(),
        period_secs: 300,
        executable: None,
        source_commit: None,
    }
}

fn run_child() -> ! {
    let root = temp_root(std::process::id());
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let args = [
        "--integration".to_string(),
        "pair".to_string(),
        "--deadline-secs".to_string(),
        "5".to_string(),
        "--carrier".to_string(),
        "relay".to_string(),
    ];
    let selection = integration::selected(&args).unwrap();
    let environment = environment(&root);
    let exit_code = std::thread::Builder::new()
        .stack_size(APP_MAIN_STACK_BYTES)
        .spawn(move || integration::run_to_stdout(selection, &environment))
        .unwrap()
        .join()
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
    std::process::exit(i32::from(exit_code));
}

fn assert_not_reflected(channel: &str, output: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(
            !output.contains(secret),
            "{channel} reflected private pairing input"
        );
    }
}

/// The fixture serves the complete pairing ceremony and credential persistence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integration_pair_emits_one_envelope_from_one_mib_caller_stack() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_child();
    }

    let state = Arc::new(MockState::normal().with_same_tls_ca());
    let origin = spawn_mock_relay(state.clone()).await;
    let origin_host = origin
        .split_once("://")
        .and_then(|(_, authority)| authority.split(':').next())
        .unwrap()
        .to_string();
    let link = relay_form_link(&origin, &PAIR_SECRET, &state.json_ca.spki_pin());
    let fragment = link.split_once('#').unwrap().1.to_string();
    let child_link = link.clone();

    let (output, child_id) = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let child_id = child.id();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(child_link.as_bytes())
            .unwrap();
        (child.wait_with_output().unwrap(), child_id)
    })
    .await
    .unwrap();
    let _ = std::fs::remove_dir_all(temp_root(child_id));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let private_inputs = [
        link.as_str(),
        fragment.as_str(),
        origin.as_str(),
        origin_host.as_str(),
    ];
    assert_not_reflected("stdout", &stdout, &private_inputs);
    assert_not_reflected("stderr", &stderr, &private_inputs);

    let envelopes: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        envelopes.len(),
        1,
        "child status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );

    let exit_code = output.status.code().unwrap_or_else(|| {
        panic!(
            "child terminated without an exit code: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        )
    });
    assert_eq!(exit_code, i32::from(EXIT_PASS));

    let envelope = &envelopes[0];
    assert_eq!(envelope["schema_version"], SCHEMA_VERSION);
    assert_eq!(envelope["operation"], "pair");
    assert_eq!(envelope["verdict"], "PASS");
    assert_eq!(envelope["phase"], "complete");
    assert_eq!(envelope["evidence"]["state_written"], true);

    println!(
        "child_status={} envelope={}",
        output.status,
        serde_json::to_string(envelope).unwrap()
    );
}
