// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::{Arc, Mutex};

use browser_host::custody::{BatchInput, BatchResult, OutboxEntry, Policy, Store};
use browser_host::hub::{Hub, HubConfig};
use browser_host::upload::{deliver_pending, Journal, UploadOutcome};
use observer_model::about::NativeAboutSnapshot;
use observer_model::{LocalOffset, LocalOffsetError, LocalZone, SyncSnapshot};
use pl_transport_win::ack::JournalIdentity;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::service::SyncConfig;
use pl_transport_win::{CredentialAccess, JournalVersionController, ObserverClient};
use rcgen::{CertificateParams, KeyPair};
use serde_json::json;
use tokio::sync::Notify;

const T0: u64 = 1_790_000_100_000;

#[derive(Debug)]
struct UtcOffset;

impl LocalOffset for UtcOffset {
    fn local_zone(&self, _: u64) -> Result<LocalZone, LocalOffsetError> {
        Ok(LocalZone {
            utc_offset_seconds: 0,
            tz: None,
        })
    }
}

fn credential(instance: &str) -> Credential {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["observer.test".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: vec![0; 16],
        instance_id: instance.into(),
        home_label: instance.into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 1,
        }],
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    }
}

type Received = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

struct SelectedJournal {
    client: Arc<ObserverClient>,
    old: Arc<ObserverClient>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    old_outcome: UploadOutcome,
    received: Received,
    hub: Arc<Hub>,
}

impl Journal for SelectedJournal {
    fn same_connection(&self, current: &Self) -> bool {
        Arc::ptr_eq(&self.client, &current.client)
    }

    async fn upload(&self, _: &OutboxEntry, body: Vec<u8>) -> UploadOutcome {
        if Arc::ptr_eq(&self.client, &self.old) {
            self.entered.notify_one();
            self.release.notified().await;
            if self.old_outcome == UploadOutcome::Delivered {
                self.received.lock().unwrap().push(("old".into(), body));
            }
            return self.old_outcome;
        }
        if !self.client.gate_open() {
            assert_eq!(self.hub.status().failure, Some("journal_rejected"));
            return UploadOutcome::Held;
        }
        self.received
            .lock()
            .unwrap()
            .push(("replacement".into(), body));
        UploadOutcome::Delivered
    }
}

async fn replacement_during_send(same_journal: bool, old_outcome: UploadOutcome) {
    let dir = tempfile::tempdir().unwrap();
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: dir.path().join("pairing.json"),
        local_offset: Arc::new(UtcOffset),
        journal_version: Arc::new(JournalVersionController::new(
            dir.path().join("version.json"),
        )),
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        awaiting_hold: None,
    };
    let a = credential("journal-a");
    *confirmation.lock().unwrap() = JournalIdentity::from_credential(&a).client_cert_sha256;
    let old_access = CredentialAccess::bind(
        &PairedState {
            credential: Some(a.clone()),
            ..Default::default()
        },
        &cfg,
        sync.clone(),
        None,
    )
    .unwrap();
    let access = Arc::new(tokio::sync::Mutex::new(Some(old_access)));
    let old = pl_transport_win::access::load_current_client(&access)
        .await
        .unwrap();
    assert!(old.gate_open());

    let mut store = Store::open(
        dir.path().join("browser"),
        Policy::default(),
        Box::new(|s, l| (format!("d{s}"), format!("s{s}_{l}"))),
        T0,
    );
    for (i, text) in ["first", "second"].into_iter().enumerate() {
        let now = T0 + i as u64 * 300_000;
        let records = [
            json!({"t":"segment_start","ts":1,"ctx":text,"site":"example.com","blocks":[{"id":"b1","text":text}]}),
        ];
        let id = format!("{:032x}", i + 1);
        assert!(matches!(
            store.offer(
                &BatchInput {
                    generation: "connection",
                    inst: "browser",
                    batch_id: &id,
                    queued_at_ms: now,
                    records: &records,
                },
                now
            ),
            BatchResult::Accepted { .. }
        ));
        store.finalize_now(now + 1);
    }
    let hub = Hub::new(
        HubConfig {
            development: false,
            app_version: "test".into(),
            about: NativeAboutSnapshot::unknown("windows", "", ""),
        },
        store,
        Box::new(|| T0 + 600_000),
    );
    assert_eq!(hub.outbox().len(), 2);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let received = Arc::new(Mutex::new(Vec::new()));
    let run = || {
        let (access, hub, old, entered, release, received) = (
            access.clone(),
            hub.clone(),
            old.clone(),
            entered.clone(),
            release.clone(),
            received.clone(),
        );
        async move {
            deliver_pending(&hub, || {
                let (access, hub, old, entered, release, received) = (
                    access.clone(),
                    hub.clone(),
                    old.clone(),
                    entered.clone(),
                    release.clone(),
                    received.clone(),
                );
                async move {
                    pl_transport_win::access::load_current_client(&access)
                        .await
                        .map(|client| SelectedJournal {
                            client,
                            old,
                            entered,
                            release,
                            old_outcome,
                            received,
                            hub,
                        })
                }
            })
            .await
        }
    };
    let task = tokio::spawn(run());
    entered.notified().await;
    confirmation.lock().unwrap().clear();
    let b = if same_journal {
        a
    } else {
        credential("journal-b")
    };
    let replacement_access = CredentialAccess::bind(
        &PairedState {
            credential: Some(b),
            ..Default::default()
        },
        &cfg,
        sync,
        None,
    )
    .unwrap();
    access.lock().await.as_ref().unwrap().retire();
    *access.lock().await = Some(replacement_access);
    let replacement = pl_transport_win::access::load_current_client(&access)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&old, &replacement));
    assert!(!replacement.gate_open());
    hub.set_delivery_failure(Some("journal_rejected"));
    release.notify_one();
    task.await.unwrap();
    assert!(received
        .lock()
        .unwrap()
        .iter()
        .all(|(target, _)| target == "old"));
    if old_outcome != UploadOutcome::Delivered {
        assert_eq!(hub.status().failure, Some("journal_rejected"));
    }
    replacement.open_gate();
    let pass = run().await;
    assert_eq!(pass.remaining, 0);
    let received = received.lock().unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received.last().unwrap().0, "replacement");
    assert!(String::from_utf8_lossy(&received.last().unwrap().1).contains("second"));
}

#[tokio::test]
async fn app_client_selection_changes_between_sends_and_preserves_the_mark_gate() {
    for same_journal in [false, true] {
        for outcome in [
            UploadOutcome::Delivered,
            UploadOutcome::Failed("relay_unavailable"),
        ] {
            replacement_during_send(same_journal, outcome).await;
        }
    }
}
