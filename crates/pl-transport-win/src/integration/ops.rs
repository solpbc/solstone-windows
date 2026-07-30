// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The four operations.
//!
//! Every network step goes through a production entry point —
//! [`pairing::pair_from_link_observed`], [`ObserverClient`],
//! [`journal_bridge::start_observed`], [`UploadCoordinator`]. Nothing here
//! re-implements pairing, framing, retry, or credential handling, and nothing
//! here can turn a mock or a direct-LAN route into a passing relay result.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::{SyncSnapshot, TransportPath};
use observer_pl::civil;
use observer_pl::pairlink::{self, ParsedPairLink};
use observer_pl::wire::HeartbeatEvent;
use observer_pl::{bridge, ca};
use observer_retention::RetentionConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::args::{Command, OperationArgs};
use super::report::{Evidence, Failure, Phase, RemoteResidue, Residue};
use super::{
    progress, relay_path_failure, runtime, shared_observer, Environment, FixedOffset,
    OperationBudget,
};
use crate::client::ObserverClient;
use crate::coordinator::UploadCoordinator;
use crate::credential::PairedState;
use crate::journal_bridge;
use crate::observe::{ObserverHandle, OperationObserver};
use crate::pairing;
use crate::sealed::{SealedSegment, SealedStore};
use crate::TransportError;

/// The result of one operation: how it failed (if it did) and what it earned.
type OpResult = (Option<Failure>, Evidence);

// The shipped binary's main thread reserves 1 MiB (measured as 0x100000, the
// MSVC default, with no /STACK override), and the full relay pairing path
// aborts at that size. Native MSVC debug measurements established 2 MiB as the
// smallest reservation proven sufficient. Reserve 4 MiB for 2x headroom; Windows
// reserves this range as virtual address space and commits pages lazily, so the
// headroom costs address space rather than 4 MiB of memory up front.
const OPERATION_WORKER_STACK_BYTES: usize = 4 * 1024 * 1024;

/// Run the operation on a dedicated worker because the shipped binary's main
/// thread cannot hold the relay pairing path. Worker panics resume into
/// `report_for`'s `catch_unwind`, the single producer of `internal_panic`; the
/// borrowed scope lets the worker take `&Command` and `&Environment`.
pub(crate) fn execute(
    command: &Command,
    environment: &Environment,
    observer: Arc<OperationObserver>,
) -> OpResult {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(OPERATION_WORKER_STACK_BYTES)
            .spawn_scoped(scope, move || execute_inner(command, environment, observer));
        match worker {
            Ok(worker) => match worker.join() {
                Ok(result) => result,
                Err(payload) => std::panic::resume_unwind(payload),
            },
            Err(_) => (
                Some(Failure::error(
                    Phase::Validate,
                    "operation_worker_unavailable",
                    "could not start the dedicated worker thread for this operation",
                )),
                Evidence::default(),
            ),
        }
    })
}

fn execute_inner(
    command: &Command,
    environment: &Environment,
    observer: Arc<OperationObserver>,
) -> OpResult {
    // Read the pair link before the runtime exists, so a blocking stdin read can
    // never stall a scheduler that is also driving the bridge's spawned tasks.
    let link = match command.args {
        OperationArgs::Pair => match read_link_from_stdin() {
            Ok(link) => Some(link),
            Err(failure) => return (Some(failure), Evidence::default()),
        },
        _ => None,
    };

    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(_) => {
            return (
                Some(Failure::error(
                    Phase::Validate,
                    "runtime_unavailable",
                    "could not start the async runtime for this operation",
                )),
                Evidence::default(),
            )
        }
    };

    let handle = shared_observer(&observer);
    runtime.block_on(async move {
        match &command.args {
            OperationArgs::Pair => {
                pair(command, environment, handle, link.unwrap_or_default()).await
            }
            OperationArgs::Roundtrip => roundtrip(command, environment, handle, &observer).await,
            OperationArgs::Fetch {
                journal_path,
                expected_bytes,
                expected_sha256,
                expected_status,
            } => {
                fetch(
                    command,
                    environment,
                    handle,
                    &observer,
                    journal_path,
                    *expected_bytes,
                    expected_sha256,
                    *expected_status,
                )
                .await
            }
            OperationArgs::Upload {
                payload,
                day,
                segment,
            } => {
                upload(
                    command,
                    environment,
                    handle,
                    &observer,
                    payload,
                    day,
                    segment,
                )
                .await
            }
        }
    })
}

// ── pair ─────────────────────────────────────────────────────────────────────

/// One relay-form link, read from stdin. The link is a secret: it is never
/// echoed, logged, or reflected into the envelope.
fn read_link_from_stdin() -> Result<String, Failure> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).map_err(|_| {
        Failure::error(
            Phase::Validate,
            "stdin_unreadable",
            "could not read the pair link from stdin",
        )
    })?;
    let link = raw.trim().to_string();
    if link.is_empty() {
        return Err(Failure::error(
            Phase::Validate,
            "link_missing",
            "pair reads one relay-form pair link from stdin; nothing was supplied",
        ));
    }
    Ok(link)
}

/// An empty profile is the primary state path absent **and** its `.tmp` sibling
/// absent.
///
/// Anything else is partial: a file with `credential: null` is loadable state, a
/// malformed file is an error rather than an unpaired default, and a stray `.tmp`
/// is a half-written credential from an interrupted save. All three fail closed
/// before any network work happens.
fn empty_profile_failure(environment: &Environment) -> Option<Failure> {
    let primary = environment.state_path.exists();
    let temporary = environment.state_tmp_path().exists();
    if !primary && !temporary {
        return None;
    }
    Some(Failure::error(
        Phase::Precondition,
        if primary {
            "profile_not_empty"
        } else {
            "profile_partial"
        },
        "pair requires an empty profile: both the pairing state file and its .tmp sibling must be absent",
    ))
}

/// Remove any local credential material and verify it is gone.
///
/// `PairedState::load` reads only the primary path, so the `.tmp` sibling is not
/// loadable through the API — but it still holds credential bytes and would make
/// the next run's precondition fail, so both go.
fn clear_local_credential(environment: &Environment) -> bool {
    for path in [environment.state_path.clone(), environment.state_tmp_path()] {
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }
    !environment.state_path.exists() && !environment.state_tmp_path().exists()
}

async fn pair(
    command: &Command,
    environment: &Environment,
    observer: ObserverHandle,
    link: String,
) -> OpResult {
    let mut evidence = Evidence {
        registered: Some(false),
        state_written: Some(false),
        remote_residue: Some(RemoteResidue::default()),
        ..Default::default()
    };

    if let Some(failure) = empty_profile_failure(environment) {
        return (Some(failure), evidence);
    }

    // The authoritative parser decides relay-form; never a hand-decode of byte 0,
    // which would skip tag, length, origin, and address-policy validation.
    match pairlink::parse(&link) {
        Ok(ParsedPairLink::Relay(_)) => {}
        Ok(_) => {
            return (
                Some(Failure::error(
                    Phase::Validate,
                    "link_not_relay_form",
                    "pair requires a relay-form pair link; the supplied link is a direct-form link",
                )),
                evidence,
            )
        }
        Err(_) => {
            return (
                Some(Failure::error(
                    Phase::Validate,
                    "pair_link",
                    "the supplied pair link did not parse",
                )),
                evidence,
            )
        }
    }

    progress("pairing over the production relay ceremony");
    let budget = OperationBudget::start(command.deadline);
    let ceremony = budget
        .run(
            Phase::Pair,
            pairing::pair_from_link_observed(&link, &environment.device_label, observer.clone()),
        )
        .await;

    // From here on the ceremony has touched the relay and the journal, so any
    // failure owes both a local cleanup and an honest statement of remote residue.
    let credential = match ceremony {
        Err(deadline) => {
            evidence.remote_residue = Some(RemoteResidue {
                journal_pairing_identity: Residue::Possible,
                relay_device_enrollment: Residue::Possible,
            });
            evidence.local_residue_cleared = Some(clear_local_credential(environment));
            return (Some(deadline), evidence);
        }
        Ok(Err(error)) => {
            evidence.remote_residue = Some(RemoteResidue {
                journal_pairing_identity: Residue::Possible,
                relay_device_enrollment: Residue::Possible,
            });
            evidence.local_residue_cleared = Some(clear_local_credential(environment));
            return (
                Some(Failure::transport(
                    Phase::Pair,
                    &error,
                    "the relay pairing ceremony failed; regenerate the link on the journal and retry",
                )),
                evidence,
            );
        }
        Ok(Ok(credential)) => credential,
    };

    // The ceremony returns only after the journal signed an identity and the relay
    // enrolled the device, so both definitely exist now.
    let residue = RemoteResidue {
        journal_pairing_identity: Residue::Present,
        relay_device_enrollment: Residue::Present,
    };
    evidence.remote_residue = Some(residue);

    progress("registering the observer stream");
    let mut client = match ObserverClient::new(credential.clone()) {
        Ok(client) => client
            .with_state_path(environment.state_path.clone())
            .with_observer(observer.clone()),
        Err(error) => {
            evidence.local_residue_cleared = Some(clear_local_credential(environment));
            return (
                Some(Failure::transport(
                    Phase::Register,
                    &error,
                    "the signed credential could not be loaded into a client",
                )),
                evidence,
            );
        }
    };

    let registration = budget
        .run(
            Phase::Register,
            client.register(
                &environment.platform,
                &environment.device_label,
                &environment.stream_type,
                &environment.app_version,
                None,
            ),
        )
        .await;

    let registration = match registration {
        Err(deadline) => {
            evidence.local_residue_cleared = Some(clear_local_credential(environment));
            return (Some(deadline), evidence);
        }
        Ok(Err(error)) => {
            evidence.local_residue_cleared = Some(clear_local_credential(environment));
            return (
                Some(Failure::transport(
                    Phase::Register,
                    &error,
                    "the journal refused the observer registration",
                )),
                evidence,
            );
        }
        Ok(Ok(registration)) => registration,
    };
    evidence.registered = Some(true);

    let paired = PairedState {
        credential: Some(credential),
        observer_key: Some(registration.key.clone()),
        observer_name: Some(registration.name.clone()),
    };
    if let Some(deadline) = budget.checkpoint(Phase::Persist) {
        evidence.local_residue_cleared = Some(clear_local_credential(environment));
        return (Some(deadline), evidence);
    }
    if let Err(error) = paired.save(&environment.state_path) {
        evidence.local_residue_cleared = Some(clear_local_credential(environment));
        return (
            Some(Failure::transport(
                Phase::Persist,
                &error,
                "the paired state could not be written to the profile",
            )),
            evidence,
        );
    }
    evidence.state_written = Some(true);
    (None, evidence)
}

// ── shared ───────────────────────────────────────────────────────────────────

fn load_paired(environment: &Environment) -> Result<PairedState, Failure> {
    let paired = PairedState::load(&environment.state_path).map_err(|error| {
        Failure::transport(
            Phase::Precondition,
            &error,
            "the profile's pairing state could not be read; it is absent, malformed, or unreadable",
        )
    })?;
    if !paired.is_paired() || paired.observer_key.is_none() {
        return Err(Failure::error(
            Phase::Precondition,
            "not_paired",
            "this operation needs a paired profile; run --integration pair first",
        ));
    }
    Ok(paired)
}

fn client_for(
    environment: &Environment,
    paired: &PairedState,
    observer: ObserverHandle,
) -> Result<ObserverClient, Failure> {
    let credential = paired
        .credential
        .clone()
        .ok_or_else(|| Failure::error(Phase::Precondition, "not_paired", "no credential"))?;
    ObserverClient::new(credential)
        .map(|client| {
            client
                .with_observer_key(paired.observer_key.clone())
                .with_state_path(environment.state_path.clone())
                .with_observer(observer)
        })
        .map_err(|error| {
            Failure::transport(
                Phase::Precondition,
                &error,
                "the stored credential could not be loaded into a client",
            )
        })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── roundtrip ────────────────────────────────────────────────────────────────

async fn roundtrip(
    command: &Command,
    environment: &Environment,
    observer: ObserverHandle,
    counts_source: &Arc<OperationObserver>,
) -> OpResult {
    let mut evidence = Evidence {
        heartbeat_ok: Some(false),
        segments_listed: Some(false),
        ..Default::default()
    };

    let paired = match load_paired(environment) {
        Ok(paired) => paired,
        Err(failure) => return (Some(failure), evidence),
    };
    let client = match client_for(environment, &paired, observer) {
        Ok(client) => client,
        Err(failure) => return (Some(failure), evidence),
    };

    progress("posting an authenticated heartbeat");
    let event = HeartbeatEvent::status(false);
    let budget = OperationBudget::start(command.deadline);
    match budget.run(Phase::Heartbeat, client.heartbeat(&event)).await {
        Err(deadline) => return (Some(deadline), evidence),
        Ok(Err(error)) => {
            return (
                Some(Failure::transport(
                    Phase::Heartbeat,
                    &error,
                    "the authenticated heartbeat did not complete",
                )),
                evidence,
            )
        }
        Ok(Ok(())) => evidence.heartbeat_ok = Some(true),
    }

    // The day is named explicitly rather than inferred from a device timezone;
    // the request only has to be authenticated and answered.
    let day = civil::day_string_local(now_secs(), 0);
    progress("listing the journal's segments for the day");
    match budget
        .run(Phase::ListSegments, client.list_segments(&day))
        .await
    {
        Err(deadline) => return (Some(deadline), evidence),
        Ok(Err(error)) => {
            return (
                Some(Failure::transport(
                    Phase::ListSegments,
                    &error,
                    "the authenticated segment-list request did not complete",
                )),
                evidence,
            )
        }
        Ok(Ok(listed)) => {
            evidence.segments_listed = Some(true);
            evidence.segment_count = Some(listed.items.len() as u64);
        }
    }

    let counts = counts_source.counts();
    // The single post-await boundary for roundtrip, fetch, and upload sits
    // immediately before relay-path finalization. Assertions drawn from earned
    // data keep precedence; the budget gates the sole remaining route to PASS.
    // Both failures are FAIL, so this ordering can never fabricate a pass.
    if let Some(deadline) = budget.checkpoint(Phase::Assert) {
        return (Some(deadline), evidence);
    }
    evidence.observed_path = Some(TransportPath::Relay.as_str());
    (relay_path_failure(counts), evidence)
}

// ── fetch ────────────────────────────────────────────────────────────────────

/// Cap on buffered loopback response bytes, derived from what the caller says to
/// expect so a runaway peer cannot grow memory without bound.
fn response_limit(expected_bytes: u64) -> usize {
    const HEAD_SLACK: u64 = 64 * 1024;
    usize::try_from(expected_bytes.saturating_add(HEAD_SLACK).saturating_mul(2))
        .unwrap_or(usize::MAX)
}

/// One HTTP/1.1 GET over the loopback bridge listener.
///
/// The request is built by `observer_pl::http` and the response parsed by it too
/// — the same codec the journal leg uses, so this adds no second HTTP stack. The
/// host is explicit because `bridge::authorize` rejects anything but
/// `127.0.0.1:<port>`.
async fn loopback_get(
    port: u16,
    target: &str,
    cookie: Option<&str>,
    limit: usize,
) -> Result<observer_pl::http::HttpResponse, TransportError> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut headers = Vec::new();
    if let Some(cookie) = cookie {
        headers.push((
            "cookie".to_string(),
            format!("{}={}", bridge::CAP_COOKIE_NAME, cookie),
        ));
    }
    let request = observer_pl::http::build_request_with_host(
        "GET",
        target,
        &format!("127.0.0.1:{port}"),
        &headers,
        b"",
    );
    stream.write_all(&request).await?;
    stream.flush().await?;

    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if let Ok(response) = observer_pl::http::parse_response(&buffer) {
            return Ok(response);
        }
        if buffer.len() > limit {
            return Err(TransportError::Mux(observer_pl::mux::MuxError::CapExceeded));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            // Final attempt on what arrived before the peer closed.
            return observer_pl::http::parse_response(&buffer).map_err(TransportError::Http);
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// The capability the bridge handed back, read from its bootstrap `Set-Cookie`.
fn capability_from_bootstrap(response: &observer_pl::http::HttpResponse) -> Option<String> {
    response
        .headers
        .iter()
        .filter(|(name, _)| name == "set-cookie")
        .find_map(|(_, value)| {
            value.split(';').find_map(|part| {
                let (name, cookie) = part.trim().split_once('=')?;
                (name == bridge::CAP_COOKIE_NAME).then(|| cookie.to_string())
            })
        })
}

#[allow(clippy::too_many_arguments)]
async fn fetch(
    command: &Command,
    environment: &Environment,
    observer: ObserverHandle,
    counts_source: &Arc<OperationObserver>,
    journal_path: &str,
    expected_bytes: u64,
    expected_sha256: &str,
    expected_status: u16,
) -> OpResult {
    let mut evidence = Evidence {
        bridge_contacted: Some(false),
        ..Default::default()
    };

    let paired = match load_paired(environment) {
        Ok(paired) => paired,
        Err(failure) => return (Some(failure), evidence),
    };

    progress("starting the production journal bridge");
    let budget = OperationBudget::start(command.deadline);
    let handle = match budget
        .run(
            Phase::BridgeStart,
            journal_bridge::start_observed(&paired, environment.state_path.clone(), observer),
        )
        .await
    {
        Err(deadline) => return (Some(deadline), evidence),
        Ok(Ok(handle)) => handle,
        Ok(Err(error)) => {
            let failure = match error {
                journal_bridge::BridgeStartError::Client(error) => Failure::transport(
                    Phase::BridgeStart,
                    &error,
                    "the journal bridge could not build its client from the stored credential",
                ),
                journal_bridge::BridgeStartError::Bind(_) => Failure::error(
                    Phase::BridgeStart,
                    "bridge_bind_failed",
                    "the journal bridge could not bind its loopback listener",
                ),
                journal_bridge::BridgeStartError::NotReady => Failure::error(
                    Phase::BridgeStart,
                    "not_paired",
                    "the profile has no credential and observer handle for the bridge",
                ),
            };
            return (Some(failure), evidence);
        }
    };

    let outcome = fetch_through_bridge(
        &budget,
        &handle,
        journal_path,
        expected_bytes,
        expected_sha256,
        expected_status,
        &mut evidence,
    )
    .await;

    evidence.bridge_contacted = Some(handle.contacted());
    // At zero remainder `run` drops the unpolled future, which drops its
    // oneshot sender. The accept loop's shutdown arm resolves on sender closure,
    // and the current-thread runtime is dropped at execute_inner's tail, so no
    // special begin_shutdown branch is needed.
    let _ = budget.run(Phase::Assert, handle.shutdown_and_wait()).await;

    if let Some(failure) = outcome {
        return (Some(failure), evidence);
    }

    let counts = counts_source.counts();
    if let Some(deadline) = budget.checkpoint(Phase::Assert) {
        return (Some(deadline), evidence);
    }
    evidence.observed_path = Some(TransportPath::Relay.as_str());
    (relay_path_failure(counts), evidence)
}

async fn fetch_through_bridge(
    budget: &OperationBudget,
    handle: &journal_bridge::JournalBridgeHandle,
    journal_path: &str,
    expected_bytes: u64,
    expected_sha256: &str,
    expected_status: u16,
    evidence: &mut Evidence,
) -> Option<Failure> {
    let port = handle.port();
    let limit = response_limit(expected_bytes);

    // Bootstrap first, exactly as a browser does, and take the capability from
    // the cookie the bridge sets rather than assuming one.
    let bootstrap_target = handle
        .bootstrap_url()
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| bridge::BOOTSTRAP_ROUTE.to_string());

    progress("bootstrapping the bridge capability");
    let bootstrap = match budget
        .run(
            Phase::BridgeStart,
            loopback_get(port, &bootstrap_target, None, limit),
        )
        .await
    {
        Err(deadline) => return Some(deadline),
        Ok(Err(error)) => {
            return Some(Failure::transport(
                Phase::BridgeStart,
                &error,
                "the bridge bootstrap request failed",
            ))
        }
        Ok(Ok(response)) => response,
    };

    let capability = match capability_from_bootstrap(&bootstrap) {
        Some(capability) => capability,
        None => {
            return Some(Failure::error(
                Phase::BridgeStart,
                "capability_missing",
                "the bridge bootstrap response carried no capability cookie",
            ))
        }
    };

    progress("retrieving the journal path through the bridge");
    let response = match budget
        .run(
            Phase::BridgeFetch,
            loopback_get(port, journal_path, Some(&capability), limit),
        )
        .await
    {
        Err(deadline) => return Some(deadline),
        Ok(Err(error)) => {
            return Some(Failure::transport(
                Phase::BridgeFetch,
                &error,
                "the bridged journal request failed",
            ))
        }
        Ok(Ok(response)) => response,
    };

    let bytes = response.body.len() as u64;
    let digest = ca::sha256_hex(&response.body);
    evidence.http_status = Some(response.status);
    evidence.response_bytes = Some(bytes);
    evidence.response_sha256 = Some(digest.clone());

    fetch_expectation_failure(
        response.status,
        bytes,
        &digest,
        expected_status,
        expected_bytes,
        expected_sha256,
    )
}

/// Compare what came back against every expectation the caller set.
///
/// Pure, so each way the operation turns red is testable without a live bridge.
/// Nothing passes by omission: status, size, and digest are all checked, and the
/// observed values are already recorded in the evidence before this runs.
pub(crate) fn fetch_expectation_failure(
    status: u16,
    bytes: u64,
    digest: &str,
    expected_status: u16,
    expected_bytes: u64,
    expected_sha256: &str,
) -> Option<Failure> {
    if status != expected_status {
        return Some(Failure::assertion(
            Phase::Assert,
            "status_mismatch",
            "the bridged response status did not match --expected-status",
        ));
    }
    if bytes != expected_bytes {
        return Some(Failure::assertion(
            Phase::Assert,
            "size_mismatch",
            "the bridged response byte count did not match --expected-bytes",
        ));
    }
    if digest != expected_sha256 {
        return Some(Failure::assertion(
            Phase::Assert,
            "digest_mismatch",
            "the bridged response SHA-256 did not match --expected-sha256",
        ));
    }
    None
}

// ── upload ───────────────────────────────────────────────────────────────────

/// A one-segment, in-memory [`SealedStore`].
///
/// This is a production seam, not a bypass: the coordinator still hashes the
/// file, builds the multipart ingest, reconciles against `list_segments`, and
/// proves journal custody. Using it avoids fabricating a decimal segment
/// directory purely to make `index * period` land on the caller's instant.
struct SingleSegmentStore {
    segment: SealedSegment,
    file_name: String,
    bytes: Vec<u8>,
    consumed: AtomicBool,
}

impl SealedStore for SingleSegmentStore {
    fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
        if self.consumed.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        Ok(vec![self.segment.clone()])
    }

    fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>> {
        if index == self.segment.index && name == self.file_name {
            return Ok(self.bytes.clone());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such segment file",
        ))
    }

    fn remove(&self, _index: u64) -> std::io::Result<()> {
        self.consumed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn quarantine(&self, _index: u64) -> std::io::Result<()> {
        self.consumed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn mark_confirmed(&self, _index: u64) -> std::io::Result<()> {
        self.consumed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
        Ok(Vec::new())
    }
}

/// Turn the caller's `YYYYMMDD` + `HHMMSS_LEN` into a boundary instant, then let
/// production re-derive both strings from it and require an exact match.
///
/// The identity is *earned*, not injected: a value production would not itself
/// produce can never appear in the result.
pub(crate) fn earned_boundary(day: &str, segment: &str, offset: i64) -> Option<(u64, u64)> {
    let year = day.get(0..4)?.parse::<i64>().ok()?;
    let month = day.get(4..6)?.parse::<u32>().ok()?;
    let date = day.get(6..8)?.parse::<u32>().ok()?;
    let (time, len) = segment.split_once('_')?;
    let hour = time.get(0..2)?.parse::<u32>().ok()?;
    let minute = time.get(2..4)?.parse::<u32>().ok()?;
    let second = time.get(4..6)?.parse::<u32>().ok()?;
    let len_secs = len.parse::<u64>().ok()?;

    let boundary = civil::epoch_from_local_parts(year, month, date, hour, minute, second, offset)?;
    if civil::day_string_local(boundary, offset) != day
        || civil::segment_key_string_local(boundary, offset, len_secs) != segment
    {
        return None;
    }
    Some((boundary, len_secs))
}

#[allow(clippy::too_many_arguments)]
async fn upload(
    command: &Command,
    environment: &Environment,
    observer: ObserverHandle,
    counts_source: &Arc<OperationObserver>,
    payload: &Path,
    day: &str,
    segment: &str,
) -> OpResult {
    let mut evidence = Evidence {
        day: Some(day.to_string()),
        segment: Some(segment.to_string()),
        confirmed: Some(false),
        ..Default::default()
    };

    let paired = match load_paired(environment) {
        Ok(paired) => paired,
        Err(failure) => return (Some(failure), evidence),
    };

    let file_name = match payload.file_name().and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            return (
                Some(Failure::error(
                    Phase::Precondition,
                    "payload_name_invalid",
                    "--payload must name a file",
                )),
                evidence,
            )
        }
    };
    let bytes = match std::fs::read(payload) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => {
            return (
                Some(Failure::error(
                    Phase::Precondition,
                    "payload_empty",
                    "--payload is empty; there would be nothing to upload",
                )),
                evidence,
            )
        }
        Err(_) => {
            return (
                Some(Failure::error(
                    Phase::Precondition,
                    "payload_unreadable",
                    "--payload could not be read",
                )),
                evidence,
            )
        }
    };
    evidence.payload_sha256 = Some(ca::sha256_hex(&bytes));

    // Fixed, caller-scoped offset: the same --day/--segment must mean the same
    // instant on every machine.
    let offset = 0;
    let Some((boundary, len_secs)) = earned_boundary(day, segment, offset) else {
        return (
            Some(Failure::error(
                Phase::Precondition,
                "segment_not_representable",
                "--day and --segment do not round-trip through the journal key derivation; the named instant is not representable",
            )),
            evidence,
        );
    };

    let client = match client_for(environment, &paired, observer) {
        Ok(client) => client,
        Err(failure) => return (Some(failure), evidence),
    };

    let store = SingleSegmentStore {
        segment: SealedSegment {
            index: boundary / environment.period_secs.max(1),
            boundary_epoch_secs: boundary,
            len_secs: Some(len_secs),
            files: vec![file_name.clone()],
        },
        file_name,
        bytes,
        consumed: AtomicBool::new(false),
    };

    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let coordinator = UploadCoordinator::new(
        Arc::new(client),
        Box::new(store),
        sync.clone(),
        environment.platform.clone(),
        environment.period_secs.max(1),
        Arc::new(RwLock::new(RetentionConfig::default())),
        Arc::new(FixedOffset(offset)),
    );

    progress("uploading through the production coordinator");
    let budget = OperationBudget::start(command.deadline);
    let ticked = budget.run(Phase::Ingest, coordinator.tick()).await;

    // Progress evidence is read whether or not the tick succeeded: a bounded
    // interruption must be visible as bytes sent without a completed close.
    let counts = counts_source.counts();
    evidence.bytes_sent_before_close = Some(counts.request_bytes_sent);
    evidence.close_completed = Some(counts.close_completed);
    if let Ok(snapshot) = sync.lock() {
        evidence.server_segment = snapshot.upload.last_uploaded_server_segment.clone();
        evidence.observed_path = snapshot.upload.last_upload_path.map(|path| path.as_str());
    }

    let confirmed = match ticked {
        Err(deadline) => return (Some(deadline), evidence),
        Ok(Err(error)) => {
            return (
                Some(Failure::transport(
                    Phase::Ingest,
                    &error,
                    "the production uploader could not deliver the segment",
                )),
                evidence,
            )
        }
        Ok(Ok(confirmed)) => confirmed,
    };

    evidence.confirmed = Some(confirmed > 0);
    if confirmed == 0 {
        return (
            Some(Failure::assertion(
                Phase::Reconcile,
                "custody_not_proven",
                "the journal accepted the segment but custody of every submitted file was not proven by the segment listing",
            )),
            evidence,
        );
    }

    if let Some(deadline) = budget.checkpoint(Phase::Assert) {
        return (Some(deadline), evidence);
    }
    evidence.observed_path = Some(TransportPath::Relay.as_str());
    (relay_path_failure(counts), evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    fn environment(root: &Path) -> Environment {
        Environment {
            state_path: root.join("pairing.json"),
            segments_root: root.join("segments"),
            device_label: "box".into(),
            platform: "windows".into(),
            stream_type: "desktop".into(),
            app_version: "1.0.0".into(),
            period_secs: 300,
            executable: None,
            source_commit: None,
        }
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "plw-integration-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // Deliberate build-unit-local overlap with tests/support/journal_fake.rs:
    // in-crate tests cannot import an integration-test module, and this fixture
    // needs only two requests decoded through FLAG_CLOSE.
    fn roundtrip_server_config() -> (rustls::ServerConfig, Vec<u8>) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let pin = ca::sha256(cert_der.as_ref())[..16].to_vec();
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
        (config, pin)
    }

    fn roundtrip_credential(pin: Vec<u8>, port: u16) -> crate::credential::Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        crate::credential::Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: pin,
            instance_id: "test-instance".into(),
            home_label: "Home".into(),
            endpoints: vec![crate::credential::EndpointAddr {
                host: "127.0.0.1".into(),
                port,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    async fn read_closed_stream_id(
        tls: &mut tokio_rustls::server::TlsStream<TcpStream>,
    ) -> Option<u32> {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        loop {
            let read = tls.read(&mut buf).await.ok()?;
            if read == 0 {
                return None;
            }
            decoder.feed(&buf[..read]);
            for frame in decoder.drain().ok()? {
                if frame.flags & FLAG_CLOSE != 0 {
                    return Some(frame.stream_id);
                }
            }
        }
    }

    async fn serve_delayed_roundtrip(listener: TcpListener, acceptor: TlsAcceptor) {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let stream_id = read_closed_stream_id(&mut tls).await.unwrap();
        tokio::time::advance(Duration::from_secs(6)).await;
        let heartbeat = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, heartbeat.to_vec());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        tls.flush().await.unwrap();
        let _ = tls.shutdown().await;

        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut tls) = acceptor.accept(tcp).await else {
            return;
        };
        let Some(stream_id) = read_closed_stream_id(&mut tls).await else {
            return;
        };
        // Split the unconditional 4s delay so the first advance reaches the
        // operation's 8s deadline exactly, then let it observe exhaustion
        // before advancing again. This keeps the pre-fix/post-fix difference
        // solely in production budget arithmetic. The response remains
        // unconditional and tolerates a peer that has already gone away,
        // because post-fix the operation returns while this task is mid-flight.
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        let body = br#"{"items":[],"total":0,"protocol_version":2}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
        if let Ok(encoded) = frame.encode() {
            let _ = tls.write_all(&encoded).await;
            let _ = tls.flush().await;
        }
        let _ = tls.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn one_operation_budget_is_shared_across_roundtrip_phases() {
        let root = temp_root("roundtrip-budget");
        let environment = environment(&root);
        let (server_config, pin) = roundtrip_server_config();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let paired = PairedState {
            credential: Some(roundtrip_credential(pin, port)),
            observer_key: Some("observer-key".into()),
            observer_name: Some("Test observer".into()),
        };
        paired.save(&environment.state_path).unwrap();
        let server = tokio::spawn(serve_delayed_roundtrip(
            listener,
            TlsAcceptor::from(Arc::new(server_config)),
        ));

        // Tokio cannot auto-advance paused time while a blocking task is
        // running. Holding one for the whole test makes every clock movement
        // explicit and prevents readiness windows from being skipped. Await
        // its started signal before the operation so no auto-advance window
        // remains before the budget is established.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let inhibitor = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        started_rx.await.unwrap();

        let command = Command {
            operation: super::super::args::Operation::Roundtrip,
            deadline: Duration::from_secs(8),
            max_dials: None,
            args: OperationArgs::Roundtrip,
        };
        let counts_source = OperationObserver::new();
        let started = tokio::time::Instant::now();
        let (failure, evidence) = roundtrip(
            &command,
            &environment,
            Some(counts_source.clone()),
            &counts_source,
        )
        .await;
        let elapsed = started.elapsed();

        drop(release_tx);
        inhibitor.await.unwrap();
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&root);

        let failure = failure.expect("the shared budget must expire");
        assert!(
            matches!(
                failure,
                Failure::Deadline {
                    phase: Phase::ListSegments
                }
            ),
            "expected list_segments deadline, got {failure:?}; virtual elapsed {elapsed:?}"
        );
        assert_eq!(elapsed, Duration::from_secs(8));
        assert_eq!(evidence.heartbeat_ok, Some(true));
    }

    #[test]
    fn an_absent_profile_is_empty() {
        let root = temp_root("empty");
        assert!(empty_profile_failure(&environment(&root)).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_present_state_file_is_not_empty_even_with_a_null_credential() {
        let root = temp_root("null-cred");
        let environment = environment(&root);
        std::fs::write(&environment.state_path, br#"{"credential":null}"#).unwrap();
        let failure = empty_profile_failure(&environment).unwrap();
        assert_eq!(failure.exit_code(), super::super::report::EXIT_ERROR);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stray_tmp_sibling_alone_is_partial_and_fails_closed() {
        let root = temp_root("stray-tmp");
        let environment = environment(&root);
        std::fs::write(environment.state_tmp_path(), b"{}").unwrap();
        assert!(empty_profile_failure(&environment).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_removes_both_files_and_reports_absence() {
        let root = temp_root("clear");
        let environment = environment(&root);
        std::fs::write(&environment.state_path, b"{}").unwrap();
        std::fs::write(environment.state_tmp_path(), b"{}").unwrap();

        assert!(clear_local_credential(&environment));
        assert!(!environment.state_path.exists());
        assert!(!environment.state_tmp_path().exists());
        // No credential is loadable through the production API afterwards.
        assert!(!PairedState::load(&environment.state_path)
            .unwrap()
            .is_paired());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_an_already_empty_profile_is_a_no_op_that_still_reports_true() {
        let root = temp_root("clear-empty");
        assert!(clear_local_credential(&environment(&root)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_caller_named_segment_must_round_trip_through_production_derivation() {
        let (boundary, len) = earned_boundary("20260617", "143000_300", 0).unwrap();
        assert_eq!(civil::day_string_local(boundary, 0), "20260617");
        assert_eq!(
            civil::segment_key_string_local(boundary, 0, len),
            "143000_300"
        );
        assert_eq!(len, 300);
    }

    #[test]
    fn an_unrepresentable_caller_name_is_refused_rather_than_injected() {
        for (day, segment) in [
            ("20260231", "143000_300"), // no such date
            ("20261317", "143000_300"), // no such month
            ("20260617", "253000_300"), // no such hour
            ("20260617", "146000_300"), // no such minute
            ("19690101", "000000_300"), // before the epoch
        ] {
            assert!(
                earned_boundary(day, segment, 0).is_none(),
                "{day} {segment} must not be representable"
            );
        }
    }

    #[test]
    fn the_synthetic_store_serves_exactly_one_segment_then_nothing() {
        let store = SingleSegmentStore {
            segment: SealedSegment {
                index: 4,
                boundary_epoch_secs: 1_200,
                len_secs: Some(300),
                files: vec!["screen.mp4".to_string()],
            },
            file_name: "screen.mp4".to_string(),
            bytes: b"MP4".to_vec(),
            consumed: AtomicBool::new(false),
        };

        assert_eq!(store.scan().unwrap().len(), 1);
        assert_eq!(store.read_file(4, "screen.mp4").unwrap(), b"MP4");
        assert!(store.read_file(4, "other.bin").is_err());
        assert!(store.read_file(5, "screen.mp4").is_err());
        assert!(store.confirmed().unwrap().is_empty());

        store.mark_confirmed(4).unwrap();
        assert!(
            store.scan().unwrap().is_empty(),
            "a consumed segment is never offered again"
        );
    }

    #[test]
    fn response_limit_bounds_a_runaway_peer() {
        assert!(response_limit(1) > 1);
        assert_eq!(response_limit(u64::MAX), usize::MAX);
    }

    /// A syntactically valid `0x06` relay link whose relay origin is unreachable,
    /// so the ceremony genuinely starts and genuinely fails.
    fn unreachable_relay_link() -> String {
        // [version][8-byte secret][spki tag][16-byte spki][origin len][origin]
        let origin = "http://127.0.0.1:1";
        let mut blob = vec![0x06u8];
        blob.extend_from_slice(&[0xA1; 8]);
        blob.push(0x01);
        blob.extend_from_slice(&[0xB2; 16]);
        blob.push(u8::try_from(origin.len()).unwrap());
        blob.extend_from_slice(origin.as_bytes());
        format!(
            "https://go.solstone.app/p#{}",
            observer_pl::crockford::encode(&blob)
        )
    }

    /// A well-formed `0x05` multi-direct IPv4 link.
    ///
    /// Layout: `[version][addr type][candidate count][port][4 bytes per
    /// candidate][16-byte nonce][16-byte CA fp]`. The address must be inside the
    /// protocol's allowed direct ranges or the parser refuses it before form ever
    /// comes up.
    fn valid_direct_link() -> String {
        let mut blob = vec![0x05u8, 0x01, 0x01];
        blob.extend_from_slice(&7657u16.to_be_bytes());
        blob.extend_from_slice(&[192, 168, 1, 10]);
        blob.extend_from_slice(&[0xC3; 16]);
        blob.extend_from_slice(&[0xD4; 16]);
        format!(
            "https://go.solstone.app/p#{}",
            observer_pl::crockford::encode(&blob)
        )
    }

    fn pair_command(deadline_secs: u64) -> Command {
        Command {
            operation: super::super::args::Operation::Pair,
            deadline: Duration::from_secs(deadline_secs),
            max_dials: None,
            args: OperationArgs::Pair,
        }
    }

    #[test]
    fn the_test_relay_link_really_is_relay_form() {
        assert!(matches!(
            pairlink::parse(&unreachable_relay_link()).unwrap(),
            ParsedPairLink::Relay(_)
        ));
    }

    #[test]
    fn a_failed_pair_leaves_no_loadable_credential_and_names_remote_residue() {
        let root = temp_root("failed-pair");
        let environment = environment(&root);
        let link = unreachable_relay_link();

        let runtime = super::super::runtime().unwrap();
        let (failure, evidence) = runtime.block_on(pair(
            &pair_command(20),
            &environment,
            Some(OperationObserver::new()),
            link.clone(),
        ));

        let failure = failure.expect("an unreachable relay cannot pair");
        assert_eq!(failure.phase(), Phase::Pair);

        // Nothing loadable, and nothing left behind on disk either.
        assert!(!environment.state_path.exists());
        assert!(!environment.state_tmp_path().exists());
        assert!(!PairedState::load(&environment.state_path)
            .unwrap()
            .is_paired());
        assert_eq!(evidence.local_residue_cleared, Some(true));
        assert_eq!(evidence.registered, Some(false));
        assert_eq!(evidence.state_written, Some(false));

        // The ceremony began, so remote residue is honestly "possible" rather
        // than a fabricated "none".
        let residue = evidence.remote_residue.expect("residue is reported");
        assert_eq!(residue.journal_pairing_identity, Residue::Possible);
        assert_eq!(residue.relay_device_enrollment, Residue::Possible);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_pair_never_reflects_the_link_into_its_result() {
        let root = temp_root("pair-redaction");
        let environment = environment(&root);
        let link = unreachable_relay_link();
        let fragment = link.rsplit('#').next().unwrap().to_string();

        let runtime = super::super::runtime().unwrap();
        let (failure, evidence) = runtime.block_on(pair(
            &pair_command(20),
            &environment,
            Some(OperationObserver::new()),
            link.clone(),
        ));

        let rendered = format!("{failure:?}{evidence:?}");
        assert!(!rendered.contains(&link));
        assert!(!rendered.contains(&fragment));
        assert!(!rendered.contains("127.0.0.1"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_direct_form_link_is_refused_before_any_network_work() {
        let root = temp_root("direct-refused");
        let environment = environment(&root);

        let link = valid_direct_link();
        // The link really is a well-formed direct link, so this exercises the
        // relay-form refusal and not an incidental parse failure.
        assert!(matches!(
            pairlink::parse(&link).unwrap(),
            ParsedPairLink::Direct(_)
        ));

        let runtime = super::super::runtime().unwrap();
        let observer = OperationObserver::new();
        let (failure, _) = runtime.block_on(pair(
            &pair_command(20),
            &environment,
            Some(observer.clone()),
            link,
        ));

        let failure = failure.expect("a direct link is not a relay link");
        assert_eq!(failure.phase(), Phase::Validate);
        assert!(
            matches!(&failure, Failure::Error { reason, .. } if reason == "link_not_relay_form"),
            "expected the relay-form refusal, got {failure:?}"
        );
        assert_eq!(
            observer.counts().dial_attempts,
            0,
            "refusal must precede every dial"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unparseable_link_is_refused_with_the_sanitizer_s_own_token() {
        let root = temp_root("bad-link");
        let environment = environment(&root);
        let runtime = super::super::runtime().unwrap();

        for link in [
            "not-a-link".to_string(),
            "https://go.solstone.app/p#!!!!".to_string(),
            // Valid crockford, unsupported version byte.
            format!(
                "https://go.solstone.app/p#{}",
                observer_pl::crockford::encode(&[0x09u8; 40])
            ),
        ] {
            let observer = OperationObserver::new();
            let (failure, _) = runtime.block_on(pair(
                &pair_command(20),
                &environment,
                Some(observer.clone()),
                link.clone(),
            ));
            let failure = failure.expect("an unparseable link cannot pair");
            assert_eq!(failure.phase(), Phase::Validate);
            assert!(
                matches!(&failure, Failure::Error { reason, .. } if reason == "pair_link"),
                "expected the pair_link token for {link:?}, got {failure:?}"
            );
            assert_eq!(observer.counts().dial_attempts, 0);
            // The rejected link is never reflected back.
            assert!(!format!("{failure:?}").contains(&link));
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pair_refuses_a_non_empty_profile_before_dialing() {
        let root = temp_root("pair-nonempty");
        let environment = environment(&root);
        std::fs::write(&environment.state_path, br#"{"credential":null}"#).unwrap();

        let runtime = super::super::runtime().unwrap();
        let observer = OperationObserver::new();
        let (failure, _) = runtime.block_on(pair(
            &pair_command(20),
            &environment,
            Some(observer.clone()),
            unreachable_relay_link(),
        ));

        let failure = failure.expect("a non-empty profile fails closed");
        assert_eq!(failure.phase(), Phase::Precondition);
        assert_eq!(observer.counts().dial_attempts, 0);
        // The precondition must not delete the operator's existing profile.
        assert!(environment.state_path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_fetch_expectation_can_turn_the_operation_red() {
        let digest = "a".repeat(64);
        assert!(fetch_expectation_failure(200, 10, &digest, 200, 10, &digest).is_none());

        let status = fetch_expectation_failure(404, 10, &digest, 200, 10, &digest).unwrap();
        assert!(
            matches!(&status, Failure::Assertion { reason, .. } if reason == "status_mismatch")
        );

        let size = fetch_expectation_failure(200, 9, &digest, 200, 10, &digest).unwrap();
        assert!(matches!(&size, Failure::Assertion { reason, .. } if reason == "size_mismatch"));

        let other = "b".repeat(64);
        let sha = fetch_expectation_failure(200, 10, &other, 200, 10, &digest).unwrap();
        assert!(matches!(&sha, Failure::Assertion { reason, .. } if reason == "digest_mismatch"));

        // Every mismatch is an assertion failure, never a dependency error.
        for failure in [status, size, sha] {
            assert_eq!(
                failure.exit_code(),
                super::super::report::EXIT_ASSERTION_FAILED
            );
        }
    }

    #[test]
    fn capability_is_read_from_the_bootstrap_cookie() {
        let response = observer_pl::http::HttpResponse {
            status: 200,
            headers: vec![(
                "set-cookie".to_string(),
                format!("{}=abc123; Path=/; HttpOnly", bridge::CAP_COOKIE_NAME),
            )],
            body: Vec::new(),
        };
        assert_eq!(
            capability_from_bootstrap(&response),
            Some("abc123".to_string())
        );

        let without = observer_pl::http::HttpResponse {
            status: 200,
            headers: vec![("set-cookie".to_string(), "sid=journal".to_string())],
            body: Vec::new(),
        };
        assert_eq!(capability_from_bootstrap(&without), None);
    }
}
