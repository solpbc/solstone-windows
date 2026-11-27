// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Import handoff, version 2 of the journal web host contract. The journal's
//! import page asks the app to pick files; the app holds them in its own queue
//! and reports their progress. The page never receives a path, the bytes or the
//! pairing credential.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::auth_host::{self, AuthContract, AuthMessage};
use crate::export_host::{self, ExportContract, ExportMessage};
use crate::{HostContractError, JournalOrigin};

/// The constants the journal publishes for this contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportContract {
    pub version: u64,
    pub capability: String,
    pub pick: String,
    pub reply: String,
    pub status: String,
    pub event: String,
    pub drop: String,
    pub unsupported: String,
    pub context_max_bytes: usize,
}

/// The address on the journal's own origin the page asks the app through. The
/// app answers it before the request leaves the window, so it never reaches
/// the journal, and it is not a Tauri page call.
pub const HOST_PATH: &str = "/_solstone/import-host";
/// The request header carrying the page's message: its JSON, percent-encoded.
pub const HOST_HEADER: &str = "x-solstone-host";
/// The response header carrying the app's answer: its JSON, form-encoded.
pub const REPLY_HEADER: &str = "x-solstone-reply";

pub fn bundled_import_contract() -> &'static [u8] {
    include_bytes!("../../../contracts/journal-web-host/import-host-contract.json")
}

pub fn import_contract(bytes: &[u8]) -> Result<ImportContract, HostContractError> {
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
    Ok(ImportContract {
        version,
        capability: text("capability")?,
        pick: text("pick")?,
        reply: text("reply")?,
        status: text("status")?,
        event: text("event")?,
        drop: text("drop")?,
        unsupported: text("unsupported")?,
        context_max_bytes: value
            .get("contextMaxBytes")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(HostContractError::Malformed)?,
    })
}

/// The adapter installed in every journal document. It acts only in the top
/// document on the paired origin: WebView2 runs initialization scripts in
/// frames too, whatever the main-frame-only option says.
const PAGE_ADAPTER: &str = r#"((contract, auth, exportContract, origin, nonce, path, ask, answer) => {
  if (window !== window.top || location.origin !== origin) return;
  if (window[contract.capability] !== undefined) return;
  const fetch = window.fetch.bind(window);
  const encode = text => encodeURIComponent(text);
  const decode = text => decodeURIComponent(text.replace(/\+/g, ' '));
  const post = async message => {
    const response = await fetch(path, {method:'POST', cache:'no-store', credentials:'same-origin',
      headers:{[ask]: encode(JSON.stringify({nonce, message}))}});
    const reply = response.headers.get(answer);
    if (!response.ok || !reply) throw new Error('host unavailable');
    return JSON.parse(decode(reply));
  };
  const authTypes = auth && auth.version === contract.version && auth.capability === contract.capability
    ? [auth.prepare, auth.authorize, auth.cancel]
    : [];
  const exportTypes = exportContract && exportContract.version === contract.version && exportContract.capability === contract.capability
    ? [exportContract.start, exportContract.status, exportContract.pause, exportContract.resume, exportContract.cancel, exportContract.reveal, exportContract.deviceSpace]
    : [];
  const allTypes = Object.freeze([contract.pick, ...authTypes, ...exportTypes]);
  const items = new Map();
  let timer;
  let exportTimer;
  let activeExportId = null;
  let lastExportSerial = null;
  function emitExport(reply) {
    if (!exportContract || !reply || typeof reply.phase !== 'string') return;
    const serial = JSON.stringify({
      export_id: reply.export_id ?? null, phase: reply.phase,
      bytes_written: reply.bytes_written ?? 0, total_bytes: reply.total_bytes ?? 0,
      destination_display: reply.destination_display ?? null,
      pause_reason: reply.pause_reason ?? null, error_code: reply.error_code ?? null,
      can_reveal: Boolean(reply.can_reveal), verified: Boolean(reply.verified),
    });
    if (serial !== lastExportSerial) {
      lastExportSerial = serial;
      window.dispatchEvent(new CustomEvent(exportContract.event, {detail:{
        ...reply, type:exportContract.status,
      }}));
    }
    if (reply.phase === 'transferring' || reply.phase === 'verifying' || reply.phase === 'paused') {
      activeExportId = reply.export_id || activeExportId;
      if (!exportTimer) exportTimer = setTimeout(pollExport, reply.phase === 'paused' ? 1000 : 250);
    } else {
      activeExportId = null;
      if (exportTimer) { clearTimeout(exportTimer); exportTimer = undefined; }
    }
  }
  async function pollExport() {
    exportTimer = undefined;
    if (!exportContract || !activeExportId) return;
    try {
      const reply = await post({type:exportContract.status, id:crypto.randomUUID(), export_id:activeExportId});
      emitExport(reply);
    } catch (_) {
      exportTimer = setTimeout(pollExport, 500);
    }
  }
  async function update() {
    let allReview = true;
    for (const [item, record] of items) {
      try {
        const status = await post({type:'host.status', id:record.id, item});
        const serial = JSON.stringify(status);
        if (record.last !== serial) {
          record.last = serial;
          window.dispatchEvent(new CustomEvent(contract.event, {detail:{
            ...status, type:contract.status, item, name:record.name, id:record.id,
          }}));
        }
        const awaitingReview = status.state === 'attention' && status.review_required === true;
        if (status.state === 'delivered' || status.state === 'gone' ||
            (status.state === 'attention' && !awaitingReview)) {
          items.delete(item);
        } else if (!awaitingReview) {
          allReview = false;
        }
      } catch (_) { allReview = false; /* Read the status again while the document is open. */ }
    }
    timer = items.size ? setTimeout(update, allReview ? 2000 : 400) : undefined;
  }
  Object.defineProperty(window, contract.capability, {value:Object.freeze({
    version:contract.version, types:allTypes,
    async send(message) {
      if (!message || typeof message.id !== 'string' || typeof message.type !== 'string') throw new TypeError();
      if (authTypes.includes(message.type)) return await post(message);
      if (exportTypes.includes(message.type)) {
        const reply = await post(message);
        if (message.type !== exportContract.reveal && message.type !== exportContract.deviceSpace) emitExport(reply);
        return reply;
      }
      if (message.type !== contract.pick) return {type:contract.unsupported, id:message.id};
      const reply = await post(message);
      for (const item of reply.items || []) items.set(item.item, {id:message.id, name:item.name});
      if (items.size && !timer) timer = setTimeout(update, 0);
      return reply;
    }
  })});
  window.addEventListener(contract.drop, event => {
    // A card can take a drop with its own choices. A drop elsewhere is a quick import.
    queueMicrotask(() => {
      if (!event.defaultPrevented && event.detail?.token)
        window[contract.capability].send({type:contract.pick, id:crypto.randomUUID(), area:'quick',
          accept:[], multiple:true, context:{}, drop:event.detail.token}).catch(() => {});
    });
  });
  document.addEventListener('DOMContentLoaded', () => {
    if (location.pathname !== '/_solstone/offline' &&
        document.contentType === 'text/plain' &&
        (document.body?.textContent || '').trim() === 'journal unreachable') {
      location.replace('/_solstone/offline?next=' + encodeURIComponent(location.pathname + location.search + location.hash));
      return;
    }
    if (window.solstoneImportContract?.version === contract.version)
      post({type:'host.ready'}).catch(() => {});
  }, {once:true});
})"#;

/// The page adapter, bound to one window's origin and secret.
pub fn page_script(contract: &[u8], origin: &JournalOrigin, nonce: &str) -> String {
    page_script_with_contracts(
        contract,
        Some(auth_host::bundled_auth_contract()),
        Some(export_host::bundled_export_contract()),
        origin,
        nonce,
    )
}

/// The page adapter, bound to one window's origin, secret, and optional OAuth
/// brokerage contract.
pub fn page_script_with_auth(
    contract: &[u8],
    auth_contract: Option<&[u8]>,
    origin: &JournalOrigin,
    nonce: &str,
) -> String {
    page_script_with_contracts(
        contract,
        auth_contract,
        Some(export_host::bundled_export_contract()),
        origin,
        nonce,
    )
}

/// The page adapter, bound to one window's origin, secret, optional OAuth
/// brokerage contract, and optional export host contract.
pub fn page_script_with_contracts(
    contract: &[u8],
    auth_contract: Option<&[u8]>,
    export_contract: Option<&[u8]>,
    origin: &JournalOrigin,
    nonce: &str,
) -> String {
    let contract: Value = serde_json::from_slice(contract).unwrap_or(Value::Null);
    let auth: Value = auth_contract
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or(Value::Null);
    let export: Value = export_contract
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or(Value::Null);
    format!(
        "{PAGE_ADAPTER}({contract}, {auth}, {export}, {}, {}, {}, {}, {});",
        Value::from(origin.ascii()),
        Value::from(nonce),
        Value::from(HOST_PATH),
        Value::from(HOST_HEADER),
        Value::from(REPLY_HEADER)
    )
}

/// The full address the app answers for this window.
pub fn endpoint(origin: &JournalOrigin) -> String {
    format!("{}{HOST_PATH}", origin.ascii())
}

/// The event the app raises in the page for a file dropped on the window.
pub fn drop_script(contract: &ImportContract, token: &str) -> String {
    format!(
        "window.dispatchEvent(new CustomEvent({}, {{cancelable:true, detail:{{token:{}}}}}));",
        Value::from(contract.drop.as_str()),
        Value::from(token)
    )
}

/// A file pick the page asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub id: String,
    pub area: String,
    pub accept: Vec<String>,
    pub multiple: bool,
    pub context: Map<String, Value>,
    pub drop: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// The page carries the v2 contract, so a drop can go to it.
    Ready,
    /// Where one held item stands. `None` is an item this app never issued.
    Status(Option<String>),
    Pick(Pick),
    /// An OAuth brokerage message (`auth.prepare`, `auth.authorize`, `auth.cancel`).
    Auth(AuthMessage),
    /// A journal export host message (`export.*`).
    Export(ExportMessage),
    /// An export message whose fields failed validation.
    ExportInvalid(String),
    /// A pick this app cannot act on as asked.
    Refused(String),
    Unsupported(String),
}

const MAX_MESSAGE_BYTES: usize = 65_536;

fn token(value: &str, max: usize, allowed: impl Fn(char) -> bool) -> bool {
    !value.is_empty() && value.len() <= max && value.chars().all(allowed)
}

fn default_auth_contract() -> Option<&'static AuthContract> {
    static BUNDLED: OnceLock<Option<AuthContract>> = OnceLock::new();
    BUNDLED
        .get_or_init(|| auth_host::auth_contract(auth_host::bundled_auth_contract()).ok())
        .as_ref()
}

fn default_export_contract() -> Option<&'static ExportContract> {
    static BUNDLED: OnceLock<Option<ExportContract>> = OnceLock::new();
    BUNDLED
        .get_or_init(|| export_host::export_contract(export_host::bundled_export_contract()).ok())
        .as_ref()
}

/// Check one request header from the page. `None` did not come from this
/// window's adapter, and gets no answer but a refusal.
pub fn parse(contract: &ImportContract, header: &str, nonce: &str) -> Option<Request> {
    parse_with_contracts(
        contract,
        default_auth_contract(),
        default_export_contract(),
        header,
        nonce,
    )
}

/// Check one request header from the page against the import contract and
/// optional OAuth brokerage contract.
pub fn parse_with_auth(
    contract: &ImportContract,
    auth: Option<&AuthContract>,
    header: &str,
    nonce: &str,
) -> Option<Request> {
    parse_with_contracts(contract, auth, default_export_contract(), header, nonce)
}

/// Check one request header from the page against the import, OAuth brokerage,
/// and journal export host contracts.
pub fn parse_with_contracts(
    contract: &ImportContract,
    auth: Option<&AuthContract>,
    export: Option<&ExportContract>,
    header: &str,
    nonce: &str,
) -> Option<Request> {
    if header.len() > MAX_MESSAGE_BYTES * 2 {
        return None;
    }
    let text = decode(header.trim())?;
    if text.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(&text).ok()?;
    if value.get("nonce")?.as_str()? != nonce {
        return None;
    }
    let message = value.get("message")?.as_object()?;
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .map(str::to_owned);
    if let Some(export_contract) = export {
        if let Some(req_id) = id.clone() {
            if let Some(parsed) = export_host::parse_message(export_contract, kind, req_id, message)
            {
                return Some(match parsed {
                    Ok(msg) => Request::Export(msg),
                    Err(bad_id) => Request::ExportInvalid(bad_id),
                });
            }
        } else if kind == export_contract.start
            || kind == export_contract.status
            || kind == export_contract.pause
            || kind == export_contract.resume
            || kind == export_contract.cancel
            || kind == export_contract.reveal
            || kind == export_contract.device_space
        {
            return Some(Request::Unsupported(String::new()));
        }
    }
    let request = match kind {
        "host.ready" => Request::Ready,
        "host.status" => Request::Status(
            message
                .get("item")
                .and_then(Value::as_str)
                .filter(|item| {
                    item.len() == 64
                        && token(item, 64, |c| c.is_ascii_digit() || ('a'..='f').contains(&c))
                })
                .map(str::to_owned),
        ),
        kind if kind == contract.pick => match id {
            None => Request::Unsupported(String::new()),
            Some(id) => pick(contract, id, message),
        },
        kind if auth.is_some_and(|a| kind == a.prepare) => match id {
            None => Request::Unsupported(String::new()),
            Some(id) => Request::Auth(AuthMessage::Prepare {
                id,
                raw: message.clone(),
            }),
        },
        kind if auth.is_some_and(|a| kind == a.authorize) => match id {
            None => Request::Unsupported(String::new()),
            Some(id) => Request::Auth(AuthMessage::Authorize {
                id,
                raw: message.clone(),
            }),
        },
        kind if auth.is_some_and(|a| kind == a.cancel) => match id {
            None => Request::Unsupported(String::new()),
            Some(id) => Request::Auth(AuthMessage::Cancel {
                id,
                raw: message.clone(),
            }),
        },
        _ => Request::Unsupported(id.unwrap_or_default()),
    };
    Some(request)
}

fn pick(contract: &ImportContract, id: String, message: &Map<String, Value>) -> Request {
    let area = message.get("area").and_then(Value::as_str).filter(|area| {
        token(area, 96, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
        })
    });
    let context = message
        .get("context")
        .and_then(Value::as_object)
        .filter(|context| {
            serde_json::to_string(context)
                .is_ok_and(|text| text.len() <= contract.context_max_bytes)
        });
    let multiple = message.get("multiple").and_then(Value::as_bool);
    let accept = message.get("accept").and_then(Value::as_array);
    let drop = match message.get("drop") {
        None => Some(None),
        Some(Value::String(token)) if !token.is_empty() && token.len() <= 128 => {
            Some(Some(token.clone()))
        }
        Some(_) => None,
    };
    match (area, context, multiple, accept, drop) {
        (Some(area), Some(context), Some(multiple), Some(accept), Some(drop)) => {
            Request::Pick(Pick {
                id,
                area: area.to_owned(),
                accept: accept
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                multiple,
                context: context.clone(),
                drop,
            })
        }
        _ => Request::Refused(id),
    }
}

/// The picker's first view: the extensions the page asked for, as a Windows
/// filter name and pattern. Media types have no Windows pattern and are left
/// out. The owner can always switch to every file.
pub fn picker_filter(accept: &[String]) -> Option<(String, String)> {
    let patterns: Vec<String> = accept
        .iter()
        .filter_map(|entry| entry.strip_prefix('.'))
        .filter(|ext| {
            token(ext, 32, |c| {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            })
        })
        .take(64)
        .map(|ext| format!("*.{}", ext.to_ascii_lowercase()))
        .collect();
    if patterns.is_empty() {
        return None;
    }
    Some((patterns.join(", "), patterns.join(";")))
}

/// What the app tells the page about a pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Queued,
    Refused,
    Busy,
    Cancelled,
}

impl Outcome {
    fn token(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Refused => "refused",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One held file in a reply: the queue's id and the original display name.
pub struct Accepted {
    pub item: String,
    pub name: String,
}

pub struct Declined {
    pub name: String,
    pub reason: String,
}

pub fn pick_reply(
    contract: &ImportContract,
    id: &str,
    outcome: Outcome,
    accepted: &[Accepted],
    declined: &[Declined],
    reason: Option<&str>,
) -> Value {
    let mut reply = json!({"type": contract.reply, "id": id, "outcome": outcome.token()});
    if !accepted.is_empty() {
        reply["items"] = accepted
            .iter()
            .map(|a| json!({"item": a.item, "name": a.name}))
            .collect();
    }
    if !declined.is_empty() {
        reply["refused"] = declined
            .iter()
            .map(|d| json!({"name": d.name, "reason": d.reason}))
            .collect();
    }
    if let Some(reason) = reason {
        reply["reason"] = reason.into();
    }
    reply
}

pub fn unsupported_reply(contract: &ImportContract, id: &str) -> Value {
    json!({"type": contract.unsupported, "id": id})
}

/// The answer header the app sends back to the request that asked.
pub fn reply_header(reply: &Value) -> String {
    url::form_urlencoded::byte_serialize(reply.to_string().as_bytes()).collect()
}

/// Percent-decode the page's header. A malformed escape or invalid UTF-8 is
/// not from the adapter.
fn decode(header: &str) -> Option<String> {
    let bytes = header.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn contract() -> ImportContract {
        import_contract(bundled_import_contract()).expect("bundled contract")
    }

    // What the adapter sends: encodeURIComponent of the JSON.
    fn header(value: Value) -> String {
        value
            .to_string()
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }

    fn envelope(message: Value) -> String {
        header(json!({"nonce": "secret", "message": message}))
    }

    fn request(message: Value) -> Request {
        parse(&contract(), &envelope(message), "secret").expect("answered")
    }

    #[test]
    fn the_bundled_contract_is_version_two() {
        let c = contract();
        assert_eq!(c.version, 2);
        assert_eq!(c.capability, "solstoneJournalHost");
        assert_eq!(c.pick, "import.pick");
        assert_eq!(c.context_max_bytes, 16_384);
        assert_eq!(
            import_contract(br#"{"version":3}"#),
            Err(HostContractError::Unsupported)
        );
        assert_eq!(import_contract(b"{"), Err(HostContractError::Malformed));
    }

    #[test]
    fn only_this_windows_adapter_is_answered() {
        let c = contract();
        let message = json!({"type": "host.ready"});
        assert_eq!(
            parse(&c, &envelope(message.clone()), "secret"),
            Some(Request::Ready)
        );
        assert!(parse(&c, &envelope(message.clone()), "other").is_none());
        for broken in [
            header(json!({"message": message})),
            header(json!({"nonce": "secret", "message": "pick"})),
            header(json!("a string")),
            "%zz".to_owned(),
            "%FF%FE".to_owned(),
            "has space".to_owned(),
            header(json!({"nonce": "secret", "pad": "x".repeat(70_000)})),
        ] {
            assert!(parse(&c, &broken, "secret").is_none(), "{broken:.80}");
        }
    }

    #[test]
    fn the_kits_cases_get_the_contract_answers() {
        // contracts/journal-web-host/conformance/cases.json in the journal.
        assert_eq!(
            request(json!({"type": "future.example", "id": "unknown-1", "future": 1})),
            Request::Unsupported("unknown-1".into())
        );
        assert_eq!(
            request(
                json!({"type": "import.pick", "id": "invalid-1", "area": "quick",
                           "multiple": true, "accept": [], "context": []})
            ),
            Request::Refused("invalid-1".into())
        );
        let Request::Pick(pick) = request(json!({"type": "import.pick", "id": "pick-1",
            "area": "quick", "accept": [".txt", "text/plain"], "multiple": true,
            "future": {"ignore": true},
            "context": {"setting": "host conformance", "future": {"preserve": true}}}))
        else {
            panic!("pick expected");
        };
        assert_eq!(pick.id, "pick-1");
        assert_eq!(pick.area, "quick");
        assert!(pick.multiple);
        assert_eq!(pick.drop, None);
        assert_eq!(
            Value::Object(pick.context),
            json!({"setting": "host conformance", "future": {"preserve": true}})
        );
        assert_eq!(
            picker_filter(&pick.accept),
            Some(("*.txt".into(), "*.txt".into()))
        );
    }

    #[test]
    fn a_pick_is_refused_unless_every_field_is_usable() {
        let base = json!({"type": "import.pick", "id": "p", "area": "apple_health",
                          "accept": [".zip"], "multiple": false, "context": {}});
        assert!(matches!(request(base.clone()), Request::Pick(_)));
        let mut cases = Vec::new();
        for (key, value) in [
            ("area", json!("")),
            ("area", json!("line\r\nbreak")),
            ("area", json!("x".repeat(97))),
            ("context", json!(null)),
            ("context", json!({"big": "x".repeat(16_400)})),
            ("multiple", json!("yes")),
            ("accept", json!(".zip")),
            ("drop", json!(7)),
            ("drop", json!("")),
        ] {
            let mut message = base.clone();
            message[key] = value;
            cases.push(message);
        }
        for message in cases {
            assert_eq!(
                request(message.clone()),
                Request::Refused("p".into()),
                "{message}"
            );
        }
        let mut dropped = base.clone();
        dropped["drop"] = json!("token");
        let Request::Pick(pick) = request(dropped) else {
            panic!("pick expected")
        };
        assert_eq!(pick.drop.as_deref(), Some("token"));
        let mut no_id = base;
        no_id["id"] = json!(5);
        assert_eq!(request(no_id), Request::Unsupported(String::new()));
    }

    #[test]
    fn status_reads_name_only_an_item_this_app_could_have_issued() {
        let item = "a".repeat(64);
        assert_eq!(
            request(json!({"type": "host.status", "item": item})),
            Request::Status(Some(item))
        );
        for bad in [
            json!("A".repeat(64)),
            json!("a".repeat(63)),
            json!("../x"),
            json!(1),
        ] {
            assert_eq!(
                request(json!({"type": "host.status", "item": bad})),
                Request::Status(None)
            );
        }
    }

    #[test]
    fn the_picker_filter_takes_extensions_and_never_a_pattern() {
        let accept: Vec<String> = [".ics", ".ZIP", "application/zip", ".*", "*.exe", ".a;b", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            picker_filter(&accept),
            Some(("*.ics, *.zip".into(), "*.ics;*.zip".into()))
        );
        assert_eq!(picker_filter(&["text/plain".into()]), None);
        assert_eq!(picker_filter(&[]), None);
    }

    #[test]
    fn replies_carry_the_request_id_and_only_what_happened() {
        let c = contract();
        let queued = pick_reply(
            &c,
            "pick-1",
            Outcome::Queued,
            &[Accepted {
                item: "b".repeat(64),
                name: "sample.txt".into(),
            }],
            &[Declined {
                name: "folder".into(),
                reason: "why".into(),
            }],
            None,
        );
        assert_eq!(
            queued,
            json!({"type": "import.reply", "id": "pick-1", "outcome": "queued",
                   "items": [{"item": "b".repeat(64), "name": "sample.txt"}],
                   "refused": [{"name": "folder", "reason": "why"}]})
        );
        assert_eq!(
            pick_reply(&c, "x", Outcome::Busy, &[], &[], None),
            json!({"type": "import.reply", "id": "x", "outcome": "busy"})
        );
        assert_eq!(
            unsupported_reply(&c, "u"),
            json!({"type": "unsupported", "id": "u"})
        );
        let reply = json!({"state": "waiting", "name": "r\u{e9}sum\u{e9} + notes.txt"});
        let sent = reply_header(&reply);
        assert!(sent.bytes().all(|b| b.is_ascii_graphic()));
        // The adapter reads it with decodeURIComponent after turning + into a space.
        let back: Value =
            serde_json::from_str(&decode(&sent.replace('+', " ").replace(' ', "%20")).unwrap())
                .unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn the_adapter_is_bound_to_the_top_document_on_the_paired_origin() {
        let origin =
            JournalOrigin::from_url(&Url::parse("http://127.0.0.1:61234/x").unwrap()).unwrap();
        let script = page_script(bundled_import_contract(), &origin, "n\"once");
        assert!(script.contains("window !== window.top || location.origin !== origin"));
        assert!(script.ends_with(
            r#", "http://127.0.0.1:61234", "n\"once", "/_solstone/import-host", "x-solstone-host", "x-solstone-reply");"#
        ));
        assert_eq!(
            endpoint(&origin),
            "http://127.0.0.1:61234/_solstone/import-host"
        );
        assert!(script.contains(r#""capability":"solstoneJournalHost""#));
        let https =
            JournalOrigin::from_url(&Url::parse("https://journal.example/").unwrap()).unwrap();
        assert_eq!(https.ascii(), "https://journal.example");
        let c = contract();
        assert_eq!(
            drop_script(&c, "t"),
            r#"window.dispatchEvent(new CustomEvent("solstone-journal-host-drop", {cancelable:true, detail:{token:"t"}}));"#
        );
        assert!(script.contains(r#""prepare":"auth.prepare""#));
        assert!(script.contains(r#""authorize":"auth.authorize""#));
        assert!(script.contains(r#""cancel":"auth.cancel""#));
        assert!(script.contains("status.review_required === true"));
        assert!(script.contains("allReview ? 2000 : 400"));
        assert!(script.contains("/_solstone/offline?next="));
    }

    #[test]
    fn auth_messages_route_only_with_valid_nonce_and_id() {
        let c = contract();
        let auth = auth_host::auth_contract(auth_host::bundled_auth_contract()).unwrap();
        let prep_hdr = envelope(json!({
            "type": "auth.prepare",
            "id": "p-1",
            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
        }));
        assert!(matches!(
            parse_with_auth(&c, Some(&auth), &prep_hdr, "secret"),
            Some(Request::Auth(AuthMessage::Prepare { id, .. })) if id == "p-1"
        ));
        assert!(parse_with_auth(&c, Some(&auth), &prep_hdr, "wrong-nonce").is_none());

        let auth_hdr = envelope(json!({
            "type": "auth.authorize",
            "id": "a-1",
            "session_id": "s1",
            "authorize_url": "https://provider.example/auth",
            "expected_state": "st"
        }));
        assert!(matches!(
            parse_with_auth(&c, Some(&auth), &auth_hdr, "secret"),
            Some(Request::Auth(AuthMessage::Authorize { id, .. })) if id == "a-1"
        ));

        let cancel_hdr = envelope(json!({
            "type": "auth.cancel",
            "id": "c-1"
        }));
        assert!(matches!(
            parse_with_auth(&c, Some(&auth), &cancel_hdr, "secret"),
            Some(Request::Auth(AuthMessage::Cancel { id, .. })) if id == "c-1"
        ));

        let no_id = envelope(json!({"type": "auth.prepare"}));
        assert_eq!(
            parse_with_auth(&c, Some(&auth), &no_id, "secret"),
            Some(Request::Unsupported(String::new()))
        );
    }

    #[test]
    fn export_messages_route_with_valid_nonce_and_id() {
        let c = contract();
        let sha = "b".repeat(64);
        let start_hdr = envelope(json!({
            "type": "export.start",
            "id": "x-1",
            "export_id": "exp_win_1",
            "download_path": "/app/export/api/ready/exp_win_1/download",
            "filename": "solstone-journal-2026-11-26.zip",
            "size_bytes": 4096,
            "sha256": sha,
            "etag": "\"sha256-win\""
        }));
        assert!(matches!(
            parse(&c, &start_hdr, "secret"),
            Some(Request::Export(ExportMessage::Start(req))) if req.id == "x-1" && req.export_id == "exp_win_1"
        ));
        assert!(parse(&c, &start_hdr, "wrong").is_none());

        let space_hdr = envelope(json!({"type": "export.device_space", "id": "sp-1"}));
        assert!(matches!(
            parse(&c, &space_hdr, "secret"),
            Some(Request::Export(ExportMessage::DeviceSpace { id })) if id == "sp-1"
        ));

        let origin =
            JournalOrigin::from_url(&Url::parse("http://127.0.0.1:61234/x").unwrap()).unwrap();
        let script = page_script(bundled_import_contract(), &origin, "nonce");
        assert!(script.contains(r#""start":"export.start""#));
        assert!(script.contains(r#""deviceSpace":"export.device_space""#));
    }
}
