// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! OAuth browser-callback brokerage, version 2 of the journal web host contract.
//!
//! The desktop app binds an ephemeral or requested `127.0.0.1` loopback HTTP/1.1
//! listener, opens the provider's HTTPS `authorize_url` in the system browser,
//! verifies the callback request's `Host`, path, unique query keys, and
//! constant-time `state`, and hands the raw `callback_url` back to the journal
//! page. The app holds zero OAuth tokens, refresh tokens, or PKCE verifiers and
//! never logs `callback_url`.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use url::Url;

use crate::HostContractError;

pub const MAX_ID_BYTES: usize = 128;
pub const MAX_PREFERENCES: usize = 16;
pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_AUTHORIZE_URL_BYTES: usize = 8_192;
pub const MAX_STATE_BYTES: usize = 1_024;
pub const MAX_REQUEST_HEADER_BYTES: usize = 8_192;
pub const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(5);
pub const MIN_TIMEOUT_SECONDS: f64 = 1.0;
pub const MAX_TIMEOUT_SECONDS: f64 = 600.0;
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 600.0;
pub const SUCCESS_BODY: &str = "signed in. you can close this tab and go back to your journal";
const MAX_CONCURRENT_CALLBACK_HANDLERS: usize = 16;

/// The constants the journal publishes for the v2 OAuth brokerage contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContract {
    pub version: u64,
    pub capability: String,
    pub prepare: String,
    pub prepared: String,
    pub authorize: String,
    pub cancel: String,
    pub result: String,
    pub unsupported: String,
}

pub fn bundled_auth_contract() -> &'static [u8] {
    include_bytes!("../../../contracts/journal-web-host/auth-host-contract.json")
}

pub fn auth_contract(bytes: &[u8]) -> Result<AuthContract, HostContractError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| HostContractError::Malformed)?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or(HostContractError::Malformed)
    };
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(HostContractError::Malformed)?;
    if version != 2 {
        return Err(HostContractError::Unsupported);
    }
    Ok(AuthContract {
        version,
        capability: text("capability")?,
        prepare: text("prepare")?,
        prepared: text("prepared")?,
        authorize: text("authorize")?,
        cancel: text("cancel")?,
        result: text("result")?,
        unsupported: text("unsupported")?,
    })
}

/// One callback preference from an `auth.prepare` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthPreference {
    Loopback {
        host: String,
        path: String,
        port: u16,
    },
    CustomScheme {
        uri: String,
    },
    Other {
        mode: String,
    },
}

/// A parsed OAuth host message from the journal page.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthMessage {
    Prepare { id: String, raw: Map<String, Value> },
    Authorize { id: String, raw: Map<String, Value> },
    Cancel { id: String, raw: Map<String, Value> },
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

pub fn is_valid_loopback_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.contains('?')
        && !path.contains('#')
        && !path.contains("..")
        && path.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

pub fn parse_preferences(raw: Option<&Value>) -> Result<Vec<AuthPreference>, &'static str> {
    let Some(list) = raw.and_then(Value::as_array) else {
        return Err("invalid_preferences");
    };
    if list.is_empty() || list.len() > MAX_PREFERENCES {
        return Err("invalid_preferences");
    }
    let mut parsed = Vec::with_capacity(list.len());
    for item in list {
        let Some(object) = item.as_object() else {
            return Err("invalid_preferences");
        };
        let Some(mode) = object
            .get("mode")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Err("invalid_preferences");
        };
        match mode {
            "loopback" => {
                let Some(host) = object.get("host").and_then(Value::as_str) else {
                    return Err("invalid_loopback_host");
                };
                if host != "127.0.0.1" && host != "localhost" {
                    return Err("invalid_loopback_host");
                }
                let Some(path) = object.get("path").and_then(Value::as_str) else {
                    return Err("invalid_loopback_path");
                };
                if !is_valid_loopback_path(path) {
                    return Err("invalid_loopback_path");
                }
                let port = match object.get("port") {
                    None | Some(Value::Null) => 0_u16,
                    Some(Value::Number(n)) => {
                        let Some(val) = n.as_u64() else {
                            return Err("invalid_loopback_port");
                        };
                        if val == 0 {
                            0
                        } else if (1024..=65535).contains(&val) {
                            val as u16
                        } else {
                            return Err("invalid_loopback_port");
                        }
                    }
                    _ => return Err("invalid_loopback_port"),
                };
                parsed.push(AuthPreference::Loopback {
                    host: host.to_owned(),
                    path: path.to_owned(),
                    port,
                });
            }
            "custom_scheme" => {
                let Some(uri) = object
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Err("invalid_custom_scheme");
                };
                parsed.push(AuthPreference::CustomScheme {
                    uri: uri.to_owned(),
                });
            }
            other => parsed.push(AuthPreference::Other {
                mode: other.to_owned(),
            }),
        }
    }
    Ok(parsed)
}

/// Strict validation of `authorize_url`: `https://` only, <= 8192 bytes, no
/// userinfo (`@`), no whitespace or ASCII control characters, non-empty host.
pub fn validate_authorize_url(raw: Option<&Value>) -> Option<String> {
    let text = raw.and_then(Value::as_str)?;
    if text.is_empty() || text.len() > MAX_AUTHORIZE_URL_BYTES {
        return None;
    }
    if text
        .chars()
        .any(|c| c.is_whitespace() || (c as u32) <= 0x20 || (c as u32) == 0x7F)
    {
        return None;
    }
    if text.len() < 8 || !text[..8].eq_ignore_ascii_case("https://") {
        return None;
    }
    let after_scheme = &text[8..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let parsed = Url::parse(text).ok()?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return None;
    }
    Some(text.to_owned())
}

pub fn validate_expected_state(raw: Option<&Value>) -> Option<String> {
    let state = raw.and_then(Value::as_str)?;
    if state.is_empty()
        || state.len() > MAX_STATE_BYTES
        || !state.bytes().all(|b| (0x21..=0x7E).contains(&b))
    {
        return None;
    }
    Some(state.to_owned())
}

pub fn parse_timeout_seconds(raw: Option<&Value>) -> Result<Duration, &'static str> {
    match raw {
        None | Some(Value::Null) => Ok(Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS)),
        Some(Value::Number(n)) => {
            let Some(value) = n.as_f64() else {
                return Err("invalid_timeout");
            };
            if !value.is_finite() {
                return Err("invalid_timeout");
            }
            let clamped = value.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);
            Ok(Duration::from_secs_f64(clamped))
        }
        _ => Err("invalid_timeout"),
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Strict percent decoding matching RFC 3986 + `application/x-www-form-urlencoded`
/// (`+` -> space, `%XX` strict hex -> strict UTF-8).
pub fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = hex_nibble(bytes[i + 1])?;
                let lo = hex_nibble(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

pub fn parse_query_refusing_duplicates(query: &str) -> Option<HashMap<String, String>> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = percent_decode(raw_key)?;
        let value = percent_decode(raw_value)?;
        if params.insert(key, value).is_some() {
            return None;
        }
    }
    Some(params)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallbackValidation {
    Accepted { callback_url: String },
    Rejected { status: u16 },
}

/// Validates one raw HTTP/1.1 loopback request header block (up to `\r\n\r\n`)
/// against the expected `Host`, exact `path`, unique query keys, and
/// constant-time `state`.
pub fn validate_http_callback(
    raw_headers: &[u8],
    expected_host: &str,
    bound_port: u16,
    expected_path: &str,
    expected_state: Option<&str>,
) -> CallbackValidation {
    if raw_headers.is_empty() || raw_headers.len() > MAX_REQUEST_HEADER_BYTES {
        return CallbackValidation::Rejected { status: 400 };
    }
    let Some(header_end) = find_header_end(raw_headers) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Ok(header_text) = std::str::from_utf8(&raw_headers[..header_end]) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let parts: Vec<&str> = request_line.split(' ').collect();
    if parts.len() != 3 {
        return CallbackValidation::Rejected { status: 400 };
    }
    let (method, target, version) = (parts[0], parts[1], parts[2]);
    if method != "GET" || version != "HTTP/1.1" {
        return CallbackValidation::Rejected { status: 400 };
    }

    let expected_host_header = format!("{expected_host}:{bound_port}");
    let mut saw_host: Option<&str> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return CallbackValidation::Rejected { status: 400 };
        }
        let Some((name, raw_val)) = line.split_once(':') else {
            return CallbackValidation::Rejected { status: 400 };
        };
        if name.is_empty() || name.bytes().any(|b| b <= 0x20 || b >= 0x7F) {
            return CallbackValidation::Rejected { status: 400 };
        }
        if name.eq_ignore_ascii_case("host") {
            if saw_host.is_some() {
                return CallbackValidation::Rejected { status: 400 };
            }
            saw_host = Some(raw_val.trim_matches(|c| c == ' ' || c == '\t'));
        }
    }
    let Some(host_val) = saw_host else {
        return CallbackValidation::Rejected { status: 400 };
    };
    if !host_val.eq_ignore_ascii_case(&expected_host_header) {
        return CallbackValidation::Rejected { status: 400 };
    }

    if !target.starts_with('/')
        || target.contains('#')
        || target.bytes().any(|b| b <= 0x20 || b == 0x7F)
    {
        return CallbackValidation::Rejected { status: 400 };
    }
    let (req_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    if req_path != expected_path {
        return CallbackValidation::Rejected { status: 404 };
    }
    let Some(expected_state) = expected_state else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(raw_query) = raw_query.filter(|q| !q.is_empty()) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(params) = parse_query_refusing_duplicates(raw_query) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(state) = params.get("state") else {
        return CallbackValidation::Rejected { status: 400 };
    };
    if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
        return CallbackValidation::Rejected { status: 400 };
    }
    if !params.contains_key("code") && !params.contains_key("error") {
        return CallbackValidation::Rejected { status: 400 };
    }

    CallbackValidation::Accepted {
        callback_url: format!("http://{expected_host}:{bound_port}{req_path}?{raw_query}"),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionOutcome {
    Callback(String),
    Cancelled,
    Timeout,
    Error(&'static str),
}

struct SessionState {
    authorizing: bool,
    expected_state: Option<String>,
    outcome: Option<SessionOutcome>,
}

struct Session {
    session_id: String,
    host: String,
    path: String,
    bound_port: u16,
    stopped: AtomicBool,
    in_flight_handlers: AtomicUsize,
    state: Mutex<SessionState>,
    condvar: Condvar,
    accept_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Session {
    fn stop_listener(&self) {
        self.stopped.store(true, Ordering::Release);
        {
            let _guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            self.condvar.notify_all();
        }
        let handle = self
            .accept_thread
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(handle) = handle {
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }

    fn settle(&self, outcome: SessionOutcome) -> bool {
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if guard.outcome.is_some() {
            return false;
        }
        guard.outcome = Some(outcome);
        self.condvar.notify_all();
        true
    }
}

type OpenUrlFn = Arc<dyn Fn(&str) -> Result<(), ()> + Send + Sync>;
type NewSessionIdFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Manages the single active OAuth browser-callback brokerage session for one
/// journal window.
pub struct AuthBroker {
    contract: AuthContract,
    open_url: OpenUrlFn,
    new_session_id: NewSessionIdFn,
    idle_timeout: Duration,
    active: Mutex<Option<Arc<Session>>>,
}

impl AuthBroker {
    pub fn new(
        contract: AuthContract,
        open_url: impl Fn(&str) -> Result<(), ()> + Send + Sync + 'static,
        new_session_id: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self::with_idle_timeout(
            contract,
            open_url,
            new_session_id,
            Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS),
        )
    }

    pub fn with_idle_timeout(
        contract: AuthContract,
        open_url: impl Fn(&str) -> Result<(), ()> + Send + Sync + 'static,
        new_session_id: impl Fn() -> Option<String> + Send + Sync + 'static,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            contract,
            open_url: Arc::new(open_url),
            new_session_id: Arc::new(new_session_id),
            idle_timeout,
            active: Mutex::new(None),
        }
    }

    pub fn contract(&self) -> &AuthContract {
        &self.contract
    }

    pub fn handle(self: &Arc<Self>, message: AuthMessage) -> Value {
        match message {
            AuthMessage::Prepare { id, raw } => self.prepare(&id, &raw),
            AuthMessage::Authorize { id, raw } => self.authorize(&id, &raw),
            AuthMessage::Cancel { id, raw } => self.cancel(&id, &raw),
        }
    }

    /// Cancels any active session, settling any pending `auth.authorize` with
    /// `outcome: "cancelled"` and closing the loopback listener immediately.
    pub fn tear_down(&self) {
        let prior = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(session) = prior {
            session.settle(SessionOutcome::Cancelled);
            session.stop_listener();
        }
    }

    fn clear_if_same(&self, session: &Arc<Session>) {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            *active = None;
        }
    }

    fn prepare(self: &Arc<Self>, id: &str, raw: &Map<String, Value>) -> Value {
        if !is_valid_id(id) {
            return json!({"type": self.contract.unsupported, "id": ""});
        }
        let preferences = match parse_preferences(raw.get("preferences")) {
            Ok(prefs) => prefs,
            Err(reason) => {
                return json!({
                    "type": self.contract.prepared,
                    "id": id,
                    "outcome": "refused",
                    "reason": reason,
                });
            }
        };

        // Single active session per window: tear down any prior idle or
        // in-flight session before binding so an earlier `auth.authorize`
        // settles as `cancelled` and any fixed port is freed.
        self.tear_down();

        let mut saw_unavailable = false;
        for pref in preferences {
            let AuthPreference::Loopback { host, path, port } = pref else {
                continue;
            };
            // Bind strictly on 127.0.0.1 with default socket flags (never
            // 0.0.0.0, never SO_REUSEADDR on Windows).
            let listener = match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => listener,
                Err(_) => {
                    saw_unavailable = true;
                    continue;
                }
            };
            if listener.set_nonblocking(true).is_err() {
                saw_unavailable = true;
                continue;
            }
            let Ok(bound_addr) = listener.local_addr() else {
                saw_unavailable = true;
                continue;
            };
            let bound_port = bound_addr.port();
            let Some(session_id) = (self.new_session_id)() else {
                return json!({
                    "type": self.contract.prepared,
                    "id": id,
                    "outcome": "unavailable",
                    "reason": "session_id_unavailable",
                });
            };
            let redirect_uri = format!("http://{host}:{bound_port}{path}");
            let session = Arc::new(Session {
                session_id: session_id.clone(),
                host,
                path,
                bound_port,
                stopped: AtomicBool::new(false),
                in_flight_handlers: AtomicUsize::new(0),
                state: Mutex::new(SessionState {
                    authorizing: false,
                    expected_state: None,
                    outcome: None,
                }),
                condvar: Condvar::new(),
                accept_thread: Mutex::new(None),
            });

            let accept_session = Arc::clone(&session);
            let accept_handle = thread::spawn(move || {
                run_accept_loop(listener, accept_session);
            });
            *session
                .accept_thread
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(accept_handle);

            *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&session));

            // Idle watchdog: if the page prepares a listener and never calls
            // `auth.authorize`, tear it down after `idle_timeout`. Wakes
            // immediately once `auth.authorize` starts or the session ends.
            let weak_broker = Arc::downgrade(self);
            let idle_session = Arc::clone(&session);
            let idle_timeout = self.idle_timeout;
            thread::spawn(move || {
                let deadline = Instant::now() + idle_timeout;
                let mut guard = idle_session.state.lock().unwrap_or_else(|p| p.into_inner());
                while !guard.authorizing && guard.outcome.is_none() {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        guard.outcome = Some(SessionOutcome::Timeout);
                        idle_session.condvar.notify_all();
                        drop(guard);
                        if let Some(broker) = weak_broker.upgrade() {
                            broker.clear_if_same(&idle_session);
                        }
                        idle_session.stop_listener();
                        return;
                    };
                    let (next_guard, _) = idle_session
                        .condvar
                        .wait_timeout(guard, remaining)
                        .unwrap_or_else(|p| p.into_inner());
                    guard = next_guard;
                }
            });

            return json!({
                "type": self.contract.prepared,
                "id": id,
                "outcome": "ready",
                "session_id": session_id,
                "mode": "loopback",
                "redirect_uri": redirect_uri,
            });
        }

        if saw_unavailable {
            json!({
                "type": self.contract.prepared,
                "id": id,
                "outcome": "unavailable",
                "reason": "port_unavailable",
            })
        } else {
            json!({
                "type": self.contract.prepared,
                "id": id,
                "outcome": "no_compatible_mode",
            })
        }
    }

    fn authorize(&self, id: &str, raw: &Map<String, Value>) -> Value {
        if !is_valid_id(id) {
            return json!({"type": self.contract.unsupported, "id": ""});
        }
        let refused = |reason: &str| {
            json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "refused",
                "reason": reason,
            })
        };
        let Some(session_id) = raw
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return refused("invalid_session");
        };
        let Some(authorize_url) = validate_authorize_url(raw.get("authorize_url")) else {
            return refused("invalid_authorize_url");
        };
        let Some(expected_state) = validate_expected_state(raw.get("expected_state")) else {
            return refused("invalid_state");
        };
        let timeout = match parse_timeout_seconds(raw.get("timeout_seconds")) {
            Ok(d) => d,
            Err(reason) => return refused(reason),
        };

        let session = {
            let active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            let Some(current) = active.as_ref() else {
                return refused("invalid_session");
            };
            if current.session_id != session_id {
                return refused("invalid_session");
            }
            let mut state = current.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.authorizing || state.outcome.is_some() {
                return refused("invalid_session");
            }
            state.authorizing = true;
            state.expected_state = Some(expected_state);
            current.condvar.notify_all();
            Arc::clone(current)
        };

        if (self.open_url)(&authorize_url).is_err() {
            session.settle(SessionOutcome::Error("browser_open_failed"));
            self.clear_if_same(&session);
            session.stop_listener();
            return json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "error",
                "reason": "browser_open_failed",
            });
        }

        let deadline = Instant::now() + timeout;
        let outcome = {
            let mut guard = session.state.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(outcome) = guard.outcome.clone() {
                    break outcome;
                }
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    guard.outcome = Some(SessionOutcome::Timeout);
                    session.condvar.notify_all();
                    break SessionOutcome::Timeout;
                };
                let (next_guard, _) = session
                    .condvar
                    .wait_timeout(guard, remaining)
                    .unwrap_or_else(|p| p.into_inner());
                guard = next_guard;
            }
        };

        self.clear_if_same(&session);
        session.stop_listener();

        match outcome {
            SessionOutcome::Callback(callback_url) => json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "callback",
                "callback_url": callback_url,
            }),
            SessionOutcome::Cancelled => json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "cancelled",
            }),
            SessionOutcome::Timeout => json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "timeout",
            }),
            SessionOutcome::Error(reason) => json!({
                "type": self.contract.result,
                "id": id,
                "outcome": "error",
                "reason": reason,
            }),
        }
    }

    fn cancel(&self, id: &str, raw: &Map<String, Value>) -> Value {
        if !is_valid_id(id) {
            return json!({"type": self.contract.unsupported, "id": ""});
        }
        let target_session = match raw.get("session_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => {
                return json!({
                    "type": self.contract.result,
                    "id": id,
                    "outcome": "cancelled",
                });
            }
        };

        let cancelled = {
            let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            let should_cancel = match (active.as_ref(), target_session) {
                (Some(_), None) => true,
                (Some(current), Some(target)) => current.session_id == target,
                (None, _) => false,
            };
            if should_cancel {
                active.take()
            } else {
                None
            }
        };

        if let Some(session) = cancelled {
            session.settle(SessionOutcome::Cancelled);
            session.stop_listener();
        }

        json!({
            "type": self.contract.result,
            "id": id,
            "outcome": "cancelled",
        })
    }
}

impl Drop for AuthBroker {
    fn drop(&mut self) {
        self.tear_down();
    }
}

fn run_accept_loop(listener: TcpListener, session: Arc<Session>) {
    while !session.stopped.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                if session.stopped.load(Ordering::Acquire) {
                    drop(stream);
                    break;
                }
                let _ = stream.set_nonblocking(false);
                // Bound concurrent slow connections so a rogue local process holding a
                // socket open cannot starve the legitimate callback.
                let prev = session.in_flight_handlers.fetch_add(1, Ordering::AcqRel);
                if prev >= MAX_CONCURRENT_CALLBACK_HANDLERS {
                    session.in_flight_handlers.fetch_sub(1, Ordering::AcqRel);
                    let mut stream = stream;
                    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));
                    write_http_response(&mut stream, 400, "Bad Request", "invalid request");
                    continue;
                }
                let handler_session = Arc::clone(&session);
                thread::spawn(move || {
                    handle_connection(stream, &handler_session);
                    handler_session
                        .in_flight_handlers
                        .fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                let guard = session.state.lock().unwrap_or_else(|p| p.into_inner());
                if session.stopped.load(Ordering::Acquire) {
                    break;
                }
                let _ = session
                    .condvar
                    .wait_timeout(guard, Duration::from_millis(10))
                    .unwrap_or_else(|p| p.into_inner());
            }
            Err(_) => {
                if session.stopped.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    drop(listener);
}

fn handle_connection(mut stream: TcpStream, session: &Session) {
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

    let Some(raw_headers) = read_request_headers(&mut stream, MAX_REQUEST_HEADER_BYTES) else {
        write_http_response(&mut stream, 400, "Bad Request", "invalid request");
        return;
    };

    let expected_state = {
        let guard = session.state.lock().unwrap_or_else(|p| p.into_inner());
        if guard.outcome.is_some() {
            None
        } else {
            guard.expected_state.clone()
        }
    };

    match validate_http_callback(
        &raw_headers,
        &session.host,
        session.bound_port,
        &session.path,
        expected_state.as_deref(),
    ) {
        CallbackValidation::Accepted { callback_url } => {
            if session.settle(SessionOutcome::Callback(callback_url)) {
                write_http_response(&mut stream, 200, "OK", SUCCESS_BODY);
                // Close the listener promptly after the single valid callback.
                session.stopped.store(true, Ordering::Release);
                let _guard = session.state.lock().unwrap_or_else(|p| p.into_inner());
                session.condvar.notify_all();
            } else {
                write_http_response(&mut stream, 400, "Bad Request", "session closed");
            }
        }
        CallbackValidation::Rejected { status: 404 } => {
            write_http_response(&mut stream, 404, "Not Found", "not found");
        }
        CallbackValidation::Rejected { .. } => {
            write_http_response(&mut stream, 400, "Bad Request", "invalid request");
        }
    }
}

fn read_request_headers(stream: &mut TcpStream, limit: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while buf.len() < limit {
        let to_read = chunk.len().min(limit - buf.len());
        match stream.read(&mut chunk[..to_read]) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if find_header_end(&buf).is_some() {
                    return Some(buf);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    find_header_end(&buf).map(|_| buf)
}

fn write_http_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn contract() -> AuthContract {
        auth_contract(bundled_auth_contract()).expect("bundled auth contract")
    }

    fn make_broker(
        open_url: impl Fn(&str) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Arc<AuthBroker> {
        let seq = AtomicU64::new(1);
        Arc::new(AuthBroker::new(contract(), open_url, move || {
            let n = seq.fetch_add(1, Ordering::Relaxed);
            Some(format!("sess_{n:032x}"))
        }))
    }

    fn send_raw_http(port: u16, request: &str) -> String {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback listener");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("json object")
    }

    #[test]
    fn bundled_auth_contract_matches_frozen_v2_schema() {
        let c = contract();
        assert_eq!(c.version, 2);
        assert_eq!(c.capability, "solstoneJournalHost");
        assert_eq!(c.prepare, "auth.prepare");
        assert_eq!(c.prepared, "auth.prepared");
        assert_eq!(c.authorize, "auth.authorize");
        assert_eq!(c.cancel, "auth.cancel");
        assert_eq!(c.result, "auth.result");
        assert_eq!(c.unsupported, "unsupported");
        assert_eq!(
            auth_contract(br#"{"version":3}"#),
            Err(HostContractError::Unsupported)
        );
        assert_eq!(auth_contract(b"{"), Err(HostContractError::Malformed));
    }

    #[test]
    fn authorize_url_validation_allows_only_strict_https() {
        assert!(
            validate_authorize_url(Some(&json!("https://provider.example/oauth?a=1&b=2")))
                .is_some()
        );
        for bad in [
            json!("http://provider.example/oauth"),
            json!("javascript:alert(1)"),
            json!("file:///etc/passwd"),
            json!("data:text/html,<script>alert(1)</script>"),
            json!("https://user:pass@provider.example/oauth"),
            json!("https://user@provider.example/oauth"),
            json!("https://provider.example/oauth with space"),
            json!("https://provider.example/oauth\r\nX-Injected: 1"),
            json!("https://"),
            json!(""),
            json!(format!("https://provider.example/{}", "a".repeat(8200))),
            json!(42),
        ] {
            assert!(
                validate_authorize_url(Some(&bad)).is_none(),
                "must reject {bad}"
            );
        }
    }

    #[test]
    fn http_callback_validation_rejects_bad_host_path_method_duplicates_and_state() {
        let valid = b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n";
        assert_eq!(
            validate_http_callback(valid, "127.0.0.1", 43210, "/cb", Some("st-1")),
            CallbackValidation::Accepted {
                callback_url: "http://127.0.0.1:43210/cb?code=abc&state=st-1".to_owned(),
            }
        );

        let cases: &[(&[u8], u16)] = &[
            (
                b"POST /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: evil.example:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /other?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                404,
            ),
            (
                b"GET /cb?code=abc&state=wrong HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&state=st-1&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&code=def&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=%ZZ&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n",
                400,
            ),
        ];
        for (req, expected_status) in cases {
            assert_eq!(
                validate_http_callback(req, "127.0.0.1", 43210, "/cb", Some("st-1")),
                CallbackValidation::Rejected {
                    status: *expected_status,
                },
                "request {:?}",
                String::from_utf8_lossy(req)
            );
        }
    }

    #[test]
    fn loopback_happy_path_rejects_wrong_state_without_consuming_then_accepts_valid() {
        let opened = Arc::new((Mutex::new(Vec::<String>::new()), Condvar::new()));
        let opened_cb = Arc::clone(&opened);
        let broker = make_broker(move |url| {
            let (lock, cv) = &*opened_cb;
            lock.lock().unwrap().push(url.to_owned());
            cv.notify_all();
            Ok(())
        });

        let prep = broker.handle(AuthMessage::Prepare {
            id: "prep-1".into(),
            raw: object(json!({
                "preferences": [
                    {"mode": "custom_scheme", "uri": "solstone://oauth/callback"},
                    {"mode": "loopback", "host": "localhost", "path": "/oauth/cb", "port": 0}
                ]
            })),
        });
        assert_eq!(prep["type"], "auth.prepared");
        assert_eq!(prep["id"], "prep-1");
        assert_eq!(prep["outcome"], "ready");
        assert_eq!(prep["mode"], "loopback");
        let session_id = prep["session_id"].as_str().unwrap().to_owned();
        let redirect_uri = prep["redirect_uri"].as_str().unwrap().to_owned();
        let parsed = Url::parse(&redirect_uri).unwrap();
        assert_eq!(parsed.host_str(), Some("localhost"));
        assert_eq!(parsed.path(), "/oauth/cb");
        let port = parsed.port().unwrap();

        let auth_broker = Arc::clone(&broker);
        let sid_for_auth = session_id.clone();
        let auth_thread = thread::spawn(move || {
            auth_broker.handle(AuthMessage::Authorize {
                id: "auth-1".into(),
                raw: object(json!({
                    "session_id": sid_for_auth,
                    "authorize_url": "https://provider.example/authorize?client_id=test",
                    "expected_state": "state_secret_123",
                    "timeout_seconds": 30
                })),
            })
        });

        // Wait for browser opener to fire so expected_state is armed.
        {
            let (lock, cv) = &*opened;
            let mut guard = lock.lock().unwrap();
            while guard.is_empty() {
                guard = cv.wait(guard).unwrap();
            }
            assert_eq!(
                guard[0],
                "https://provider.example/authorize?client_id=test"
            );
        }

        // Wrong state, wrong host, wrong path, duplicate keys must return 400/404 without closing listener.
        let wrong_state = send_raw_http(
            port,
            &format!(
                "GET /oauth/cb?code=c1&state=wrong HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
            ),
        );
        assert!(wrong_state.starts_with("HTTP/1.1 400 "));

        let wrong_host = send_raw_http(
            port,
            "GET /oauth/cb?code=c1&state=state_secret_123 HTTP/1.1\r\nHost: evil.example:80\r\n\r\n",
        );
        assert!(wrong_host.starts_with("HTTP/1.1 400 "));

        let wrong_path = send_raw_http(
            port,
            &format!("GET /nope?code=c1&state=state_secret_123 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
        );
        assert!(wrong_path.starts_with("HTTP/1.1 404 "));

        let dup_keys = send_raw_http(
            port,
            &format!(
                "GET /oauth/cb?code=c1&code=c2&state=state_secret_123 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
            ),
        );
        assert!(dup_keys.starts_with("HTTP/1.1 400 "));

        // Now deliver the valid callback: must return 200 OK with exact body and Cache-Control: no-store.
        let ok = send_raw_http(
            port,
            &format!(
                "GET /oauth/cb?code=real_code&state=state_secret_123 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
            ),
        );
        assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
        assert!(ok.contains("Cache-Control: no-store\r\n"), "{ok}");
        assert!(ok.ends_with(SUCCESS_BODY), "{ok}");

        let auth_reply = auth_thread.join().unwrap();
        assert_eq!(auth_reply["type"], "auth.result");
        assert_eq!(auth_reply["id"], "auth-1");
        assert_eq!(auth_reply["outcome"], "callback");
        assert_eq!(
            auth_reply["callback_url"],
            format!("http://localhost:{port}/oauth/cb?code=real_code&state=state_secret_123")
        );

        // Listener must be closed immediately after single valid callback.
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn cancel_superseding_prepare_teardown_and_timeout_settle_pending_authorize() {
        // 1. Explicit auth.cancel settles in-flight auth.authorize as cancelled.
        {
            let opened = Arc::new((Mutex::new(false), Condvar::new()));
            let opened_cb = Arc::clone(&opened);
            let broker = make_broker(move |_| {
                let (lock, cv) = &*opened_cb;
                *lock.lock().unwrap() = true;
                cv.notify_all();
                Ok(())
            });
            let prep = broker.handle(AuthMessage::Prepare {
                id: "p1".into(),
                raw: object(json!({
                    "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                })),
            });
            let sid = prep["session_id"].as_str().unwrap().to_owned();
            let b = Arc::clone(&broker);
            let sid_c = sid.clone();
            let t = thread::spawn(move || {
                b.handle(AuthMessage::Authorize {
                    id: "a1".into(),
                    raw: object(json!({
                        "session_id": sid_c,
                        "authorize_url": "https://provider.example/auth",
                        "expected_state": "st1"
                    })),
                })
            });
            let (lock, cv) = &*opened;
            let mut g = lock.lock().unwrap();
            while !*g {
                g = cv.wait(g).unwrap();
            }
            let cancel_reply = broker.handle(AuthMessage::Cancel {
                id: "c1".into(),
                raw: object(json!({"session_id": sid})),
            });
            assert_eq!(
                cancel_reply,
                json!({"type": "auth.result", "id": "c1", "outcome": "cancelled"})
            );
            let auth_reply = t.join().unwrap();
            assert_eq!(
                auth_reply,
                json!({"type": "auth.result", "id": "a1", "outcome": "cancelled"})
            );
        }

        // 2. Superseding auth.prepare settles prior in-flight auth.authorize and frees its port.
        {
            let opened = Arc::new((Mutex::new(false), Condvar::new()));
            let opened_cb = Arc::clone(&opened);
            let broker = make_broker(move |_| {
                let (lock, cv) = &*opened_cb;
                *lock.lock().unwrap() = true;
                cv.notify_all();
                Ok(())
            });
            let prep1 = broker.handle(AuthMessage::Prepare {
                id: "p2".into(),
                raw: object(json!({
                    "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                })),
            });
            let sid1 = prep1["session_id"].as_str().unwrap().to_owned();
            let port1 = Url::parse(prep1["redirect_uri"].as_str().unwrap())
                .unwrap()
                .port()
                .unwrap();
            let b = Arc::clone(&broker);
            let t = thread::spawn(move || {
                b.handle(AuthMessage::Authorize {
                    id: "a2".into(),
                    raw: object(json!({
                        "session_id": sid1,
                        "authorize_url": "https://provider.example/auth",
                        "expected_state": "st2"
                    })),
                })
            });
            let (lock, cv) = &*opened;
            let mut g = lock.lock().unwrap();
            while !*g {
                g = cv.wait(g).unwrap();
            }
            let prep2 = broker.handle(AuthMessage::Prepare {
                id: "p3".into(),
                raw: object(json!({
                    "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb", "port": port1}]
                })),
            });
            assert_eq!(prep2["outcome"], "ready");
            assert_eq!(
                prep2["redirect_uri"],
                format!("http://127.0.0.1:{port1}/cb")
            );
            let prior = t.join().unwrap();
            assert_eq!(
                prior,
                json!({"type": "auth.result", "id": "a2", "outcome": "cancelled"})
            );
            broker.tear_down();
        }

        // 3. Window teardown settles in-flight auth.authorize as cancelled.
        {
            let opened = Arc::new((Mutex::new(false), Condvar::new()));
            let opened_cb = Arc::clone(&opened);
            let broker = make_broker(move |_| {
                let (lock, cv) = &*opened_cb;
                *lock.lock().unwrap() = true;
                cv.notify_all();
                Ok(())
            });
            let prep = broker.handle(AuthMessage::Prepare {
                id: "p4".into(),
                raw: object(json!({
                    "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                })),
            });
            let sid = prep["session_id"].as_str().unwrap().to_owned();
            let b = Arc::clone(&broker);
            let t = thread::spawn(move || {
                b.handle(AuthMessage::Authorize {
                    id: "a4".into(),
                    raw: object(json!({
                        "session_id": sid,
                        "authorize_url": "https://provider.example/auth",
                        "expected_state": "st4"
                    })),
                })
            });
            let (lock, cv) = &*opened;
            let mut g = lock.lock().unwrap();
            while !*g {
                g = cv.wait(g).unwrap();
            }
            broker.tear_down();
            let reply = t.join().unwrap();
            assert_eq!(
                reply,
                json!({"type": "auth.result", "id": "a4", "outcome": "cancelled"})
            );
        }

        // 4. Timeout settles in-flight auth.authorize with outcome "timeout".
        {
            let broker = make_broker(|_| Ok(()));
            let prep = broker.handle(AuthMessage::Prepare {
                id: "p5".into(),
                raw: object(json!({
                    "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                })),
            });
            let sid = prep["session_id"].as_str().unwrap().to_owned();
            let reply = broker.handle(AuthMessage::Authorize {
                id: "a5".into(),
                raw: object(json!({
                    "session_id": sid,
                    "authorize_url": "https://provider.example/auth",
                    "expected_state": "st5",
                    "timeout_seconds": 1
                })),
            });
            assert_eq!(
                reply,
                json!({"type": "auth.result", "id": "a5", "outcome": "timeout"})
            );
        }
    }

    #[test]
    fn custom_scheme_only_returns_no_compatible_mode_and_invalid_authorize_url_is_refused() {
        let broker = make_broker(|_| Ok(()));
        let prep = broker.handle(AuthMessage::Prepare {
            id: "p-cs".into(),
            raw: object(json!({
                "preferences": [{"mode": "custom_scheme", "uri": "solstone://oauth/callback"}]
            })),
        });
        assert_eq!(
            prep,
            json!({"type": "auth.prepared", "id": "p-cs", "outcome": "no_compatible_mode"})
        );

        let prep_ok = broker.handle(AuthMessage::Prepare {
            id: "p-ok".into(),
            raw: object(json!({
                "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
            })),
        });
        let sid = prep_ok["session_id"].as_str().unwrap();
        for bad_url in [
            "http://provider.example/auth",
            "javascript:alert(1)",
            "file:///C:/Windows/win.ini",
            "data:text/html,hi",
            "https://user:pass@provider.example/auth",
        ] {
            let reply = broker.handle(AuthMessage::Authorize {
                id: "a-bad".into(),
                raw: object(json!({
                    "session_id": sid,
                    "authorize_url": bad_url,
                    "expected_state": "st"
                })),
            });
            assert_eq!(
                reply,
                json!({
                    "type": "auth.result",
                    "id": "a-bad",
                    "outcome": "refused",
                    "reason": "invalid_authorize_url"
                }),
                "must refuse {bad_url}"
            );
        }
        broker.tear_down();
    }
}
