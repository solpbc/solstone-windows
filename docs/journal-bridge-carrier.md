# Journal Bridge Persistent Carrier Design

Status: implemented (2026-07-02).

This note documents the local journal bridge rework: each bridge instance owns
at most one live upstream PL/TLS carrier, and every authorized loopback browser
request opens its own odd-numbered mux stream on that carrier. The observer
cadence paths - pairing, register, ingest, heartbeat, and reconcile - stay on
the existing request-per-carrier transport.

## Goals

- One live upstream carrier per journal WebView2 bridge instance.
- One mux stream per authorized loopback HTTP request.
- `/sse/events` can stay open without blocking normal dashboard requests.
- Concurrent first-load requests coalesce behind one carrier dial.
- Local authorization, bootstrap, header transforms, redacted logs, and local
  HTTP wire behavior stay byte-identical.
- Pure mux state stays in `observer-pl`; socket, TLS, relay, and task topology
  stay in `pl-transport-win`.

## Non-goals

- No changes to observer cadence request-per-carrier behavior.
- No in-flight replay after a carrier dies; the browser owns retry.
- No global or static bridge carrier shared across windows or app instances.
- No platform dependencies, `windows`, or `unsafe` in `observer-pl`.

## Type and Module Plan

### Pure crate: `crates/observer-pl/src/mux.rs`

Keep `WindowedUpload`, `ResponseAssembler`, `StreamItem`, `StreamEnd`, and
`HttpHead`. Add the central carrier demux here so all mux-side state remains in
one pure, host-testable module.

New types:

- `CarrierDemux`
  - Fields: one `FrameDecoder`; per-stream `HttpStreamAssembler` and
    receive-window state for active response streams.
  - Methods: `new`, `open_stream(stream_id)`, `remove_stream(stream_id)`,
    `consume(stream_id, bytes)`, and
    `feed(bytes) -> Result<DemuxOutput, MuxError>`.
  - Behavior: decode frames once per carrier; answer stream-0 PINGs; surface
    stream-0 PONG nonces; route DATA/CLOSE/RESET/WINDOW by stream id; drop
    frames for unknown or already-removed streams.
- `DemuxOutput`
  - Fields: `pongs: Vec<Vec<u8>>`, `inbound_pongs: Vec<[u8; 8]>`,
    `stream_events: Vec<(u32, StreamEvent)>` with per-Body wire costs,
    `window_grants: Vec<(u32, u32)>`, and `emit_frames: Vec<Vec<u8>>` for
    originated WINDOW and RESET frames.
- `HttpStreamAssembler`
  - Decoder-free, stream-id-agnostic helper extracted from current
    `StreamingResponseAssembler::feed_data` / `feed_body`.
  - Fields: `head_buf`, `head_emitted`, `chunked`, `ChunkedDecoder`, deferred
    body wire cost, and `closed`.
  - Methods: `feed_data(payload) -> Result<AssemblerOutput, MuxError>`,
    `close() -> StreamItem`, `reset() -> StreamItem`, `finish_eof()`.

Frame helper additions in `crates/observer-pl/src/frame.rs`:

- `Frame::control_ping(nonce: [u8; 8]) -> Frame`.
- `Frame::control_pong_nonce(&self) -> Option<[u8; 8]>`.
- `Frame::window(stream_id: u32, credit: u32) -> Frame`.
- `Frame::reset(stream_id: u32, reason: u8) -> Frame`.

Deletion after replacement:

- `StreamingResponseAssembler`.
- `StreamingFeed`.
- Streaming assembler unit tests that only pin the deleted type. Equivalent
  coverage moves to `HttpStreamAssembler` and `CarrierDemux` tests.

### Transport crate: `crates/pl-transport-win/src/client.rs`

Add carrier dialing and expose proxy auth composition without changing cadence
send paths.

New or changed items:

- `pub(crate) trait CarrierIo: AsyncRead + AsyncWrite + Send + Unpin`.
  - Blanket-implemented for matching stream types so `Box<dyn CarrierIo>` is
    legal and avoids duplicating generic task code.
- `pub(crate) struct DialedCarrier`
  - Fields: `stream: Box<dyn CarrierIo>`, `kind: CarrierKind`.
- `pub(crate) enum CarrierKind`
  - `Lan`
  - `Relay { termination: RelayTerminationHandle, token_at_dial: String }`
- `ObserverClient::dial_carrier(&self) -> Result<DialedCarrier, TransportError>`
  - Uses buffered `send` dial semantics: five LAN attempts over all endpoints,
    `Tls`/`Io` linear backoff, `lan_unreachable` relay fallback, relay proactive
    refresh, reactive unauthorized refresh/redial once, single-flight token
    mutex, and persisted token write-back.
  - Returns the already-handshaken inner mTLS byte stream for both LAN and relay.
- `ObserverClient::proxy_headers(&self, browser_headers)`.
  - `pub(crate)` replacement for private `compose_proxy_headers`.
  - Keeps auth injection exactly where it is today: observer handle,
    `Authorization: Bearer`, and protocol version.

The existing `send`, `send_over_relay`, `request_once`, and
`request_once_relay` paths stay intact for cadence and pairing.

### Transport crate: `crates/pl-transport-win/src/connection.rs`

Factor only the LAN dial prologue if it is clean:

- `dial_tls(config, host, port) -> Result<TlsStream<TcpStream>, TransportError>`
  performs the existing TCP connect timeout, `set_nodelay`, and pinned TLS
  handshake.

If factoring makes the one-shot path harder to read, keep a small duplication
inside `ObserverClient::dial_carrier`. Do not move retry/token logic out of
`ObserverClient`.

Deletion after replacement:

- `connection::request_stream`.
- `run_request_stream_over_stream`.
- `run_request_stream_loop`.
- `end_or_error`.

`connection::request_once` and `run_request_over_stream` stay.

### Transport crate: `crates/pl-transport-win/src/relay.rs`

Factor the relay carrier prologue:

- `dial_relay_carrier(inner_config, relay_origin, instance_id, device_token)
  -> Result<RelayCarrier, TransportError>`.
- `RelayCarrier` carries the inner mTLS stream plus a `RelayTerminationHandle`
  that maps later WS close/abnormal termination to the existing redacted
  `RelayError`/`transport_error_code` vocabulary.

This helper is the common part of the existing `request_once_over_ws_inner` and
the new persistent carrier. It uses `dial_relay_ws`, `WsByteDuplex::new`, and
the inner `TlsConnector` handshake. It does not own refresh policy.

Deletion after replacement:

- `request_stream_relay`.
- `request_stream_over_ws`.

`request_once_relay`, `request_once_over_ws`, and relay pairing helpers stay.

### Transport crate: `crates/pl-transport-win/src/journal_bridge_carrier.rs`

New private module owned by the journal bridge.

Types:

- `MuxCarrier`
  - Fields: `client: Arc<ObserverClient>`,
    `slot: tokio::sync::Mutex<Option<Arc<CarrierHandle>>>`,
    `keepalive: KeepaliveConfig`.
  - Methods: `new`, `open_stream`, `shutdown`.
- `CarrierHandle`
  - Fields: `commands: mpsc::Sender<CarrierCommand>`,
    `alive: Arc<AtomicBool>`, `writer_alive: Arc<AtomicBool>`,
    task joins for cleanup.
- `StreamRx`
  - Fields: `stream_id`, bounded `mpsc::Receiver<StreamItem>`,
    coordinator command sender, cancel-on-drop flag.
  - Methods: `recv`, `cancel`.
  - Drop behavior: best-effort `CancelStream { stream_id }`.
- `CarrierCommand`
  - `OpenStream { method, target, headers, body, reply }`
  - `CancelStream { stream_id }`
  - `Shutdown`
- `KeepaliveConfig`
  - Fields: `interval`, `deadline`, `max_missed`, `write_queue_frames`,
    `stream_queue_items`.
  - Defaulted for production; injectable in tests.

`open_stream(method, target, upstream_headers, body)` locks the slot. If the
slot holds an alive handle, it clones it. Otherwise, the caller holding the
async mutex dials one new carrier via `ObserverClient::dial_carrier`, spawns its
coordinator/writer pair, stores it, and releases the lock. Concurrent first-load
requests therefore queue behind exactly one dial and then share the resulting
carrier. If command send races a dead coordinator, `open_stream` clears the slot
and redials once.

### Bridge module: `crates/pl-transport-win/src/journal_bridge.rs`

`start()` still builds one `Arc<ObserverClient>`, then wraps it in one
`Arc<MuxCarrier>`. The bridge handle owns that carrier and shuts it down when the
window closes.

`handle_conn` preserves the exact ordering:

1. Read one local HTTP request.
2. Parse request head.
3. If bootstrap route, run bootstrap handling and return.
4. Run `observer_pl::bridge::authorize` before any carrier open or dial.
5. Build upstream headers with `bridge::upstream_request_headers`.
6. Build proxy headers with `ObserverClient::proxy_headers`.
7. Open a mux stream on the carrier.
8. Write the local response.

Bootstrap and authorization rejections remain pre-dial and byte-identical.

## Task Topology

Each live carrier has exactly two tasks.

### Coordinator task

Owns:

- TLS read half.
- `CarrierDemux`.
- Per-stream map.
- Per-stream `WindowedUpload`s.
- Per-stream local delivery channels.
- `FrameDialer` for monotonic odd stream ids starting at 1.
- Keepalive timer and outstanding probe state.
- `mpsc` command receiver from `MuxCarrier` and `StreamRx`.
- Sender to the writer task.

The coordinator never touches the TLS write half. On every event it emits
encoded frame bytes to the writer channel. It pumps each `WindowedUpload` after
stream open and after matching WINDOW grants. WINDOW grants are routed only to
the owning stream id. `StreamRx::recv` sends a lossless `Consume` command after
it drains a costed Body item; the coordinator returns any resulting receive
credit as an encoded WINDOW frame through the writer task.

### Writer task

Owns:

- TLS write half.
- A bounded `mpsc::Receiver<Vec<u8>>`.

Loop:

- Receive one encoded frame buffer.
- `write_all`.
- `flush`.
- On write error or closed channel, exit and mark the handle dead.

### Why two tasks instead of one actor

A single actor owning both halves gives one serialization point but also makes a
slow or blocked socket write block reads, including PING/PONG, WINDOW grants,
RESETs, and responses for unrelated streams. The split keeps one serialized
writer while allowing the coordinator to keep reading and demuxing as long as
the writer queue has capacity. This is the cleaner invariant: there is exactly
one writer, and read progress does not depend on write syscall latency.

## Writer Back-pressure Decision

Use a bounded writer channel, not unbounded.

Reasoning:

- `WindowedUpload` already bounds outstanding request bytes per stream by the
  peer window.
- A bounded frame queue bounds memory if the socket stalls anyway.
- The coordinator must not await indefinitely while holding the read loop.

Coordinator uses `try_send` into the writer queue. If the writer queue is full
or closed, the carrier is considered write-stalled/dead: mark `alive=false`,
fail active streams, drop the writer channel, and let future `open_stream` dial
a fresh carrier. This avoids unbounded memory and avoids read-loop head-of-line
blocking.

Default queue size: 256 frame buffers. Keep it configurable through
`KeepaliveConfig` for stress tests.

## Stream Lifecycle

Open:

- Coordinator allocates the next odd id from its per-carrier `FrameDialer`.
- It creates a bounded per-stream delivery channel, `WindowedUpload`, and
  `CarrierDemux` stream state.
- It builds request bytes with `observer_pl::http::build_request`.
- It pumps upload frames until the stream is blocked by credit or done.
- It replies to `open_stream` with `StreamRx`.

Inbound frame:

- DATA debits the stream receive window and emits `Head` and/or costed `Body`
  events through `HttpStreamAssembler`.
- WINDOW grants credit only to that stream's `WindowedUpload`.
- CLOSE emits `End(Close)` and frees stream state.
- RESET emits `End(Reset)` and frees stream state.
- DATA beyond the available receive credit emits RESET(FLOW_CONTROL_ERROR),
  ends only that local stream, and leaves the carrier and siblings alive.
- Unknown/closed stream frames are dropped defensively.

Local consumer back-pressure:

- Per-stream delivery is bounded, default `channel(16)`.
- Coordinator uses `try_send`.
- Only response-HEAD bytes are auto-consumed at decode time. After the head, all
  chunked wire bytes, including framing-only and partial-chunk frames, accumulate
  in `pending_body_wire` and attach to the next emitted Body event; terminal
  residue is dropped at End, so a single chunk larger than the receive window
  stalls at zero credit. Body-attributed DATA bytes return credit only when
  `StreamRx::recv` drains the Body item, with a WINDOW grant for all consumed
  bytes once the accumulated amount reaches half the initial window.
- Stream receive-window state is removed immediately on CLOSE or RESET, so
  queued Body drains after End do not emit late WINDOW frames.
- `Full` or `Closed` means the local browser side is slow or gone. Coordinator
  sends RESET for that stream only, frees that stream, and siblings continue.

Cancel:

- `StreamRx::cancel()` or Drop sends `CancelStream`.
- Coordinator sends upstream RESET for that stream only and removes the stream.

Carrier death:

- Read EOF, read error, write failure, writer-queue full, keepalive miss, or
  shutdown marks `alive=false`.
- Coordinator fans out `End(Eof)` or closes channels for active streams, clears
  the map, drops the writer sender, and exits.
- The slot keeps a dead handle only until the next `open_stream` checks
  `alive=false`; then it dials a fresh carrier.
- There is no in-flight replay.

## Local HTTP Response Modes

Both response modes consume `StreamRx`.

Buffered:

- Wait for one `Head`.
- Accumulate all `Body` items until `End`.
- Before local head: stream open/dial failure, EOF, RESET, parse error, or no
  head maps to local `502 journal unreachable`.
- After head: write status, `bridge::response_headers`, `content-length`,
  `connection: close`, then body.
- Preserve current `HEAD` behavior: no local body; content length comes from the
  upstream `content-length` header when parseable, otherwise zero.
- Upstream `401` or `403` is forwarded unmasked and logs
  `category=upstream_credential status=<status>`.
- Local socket write failure calls `StreamRx::cancel()`.

SSE:

- Same `StreamRx`, but write head immediately.
- Head writer uses `bridge::response_headers` plus `connection: close`; it never
  writes local `content-length` or `transfer-encoding`.
- Body items are written and flushed as they arrive.
- Error before local head maps to `502 journal unreachable`.
- End/error after local head closes the local socket.
- Local socket write failure cancels only that stream.

## Keepalive and Liveness

Coordinator keeps:

- `last_inbound_at`.
- Optional outstanding probe `{ nonce, sent_at, missed_count }`.
- Interval timer.
- Deadline timer derived from `KeepaliveConfig`.

On each decoded inbound frame, update `last_inbound_at`. A matching inbound PONG
nonce clears the outstanding probe. PING/PONG recognition stays pure in
`CarrierDemux`.

On interval tick:

- If there is no outstanding probe and no inbound activity since the previous
  interval, send stream-0 PING via the writer and record the nonce.
- If a probe is outstanding and its deadline elapsed, increment missed count.
- If missed count reaches `max_missed`, mark the carrier dead and fan out stream
  termination.
- Otherwise send a new PING with a new nonce.

Tests can inject short intervals/deadlines or use Tokio paused time after adding
Tokio `test-util`.

## Dial and Re-dial Semantics

`ObserverClient::dial_carrier` is the only place that decides LAN vs relay for
the persistent bridge carrier.

LAN:

- Try every credential endpoint in order.
- Repeat for five attempts.
- Retry only `TransportError::Tls` and `TransportError::Io`.
- Sleep `250ms * (attempt + 1)` between failed attempts.
- On success, return `CarrierKind::Lan`.

Relay:

- Enter relay only when final LAN error is `Tls`, `Io`, or `NoEndpoint` and
  `relay_eligible()` is true.
- Clone current live token under the existing mutex.
- If `token_should_refresh`, call `refresh_if_current`.
  - `Terminal` returns relay unauthorized.
  - `Redial` means use the refreshed current token.
  - `Transient` preserves current behavior and attempts with the current token.
- Dial outer WS, wrap `WsByteDuplex`, complete inner mTLS handshake, then return
  the inner stream and `CarrierKind::Relay`.
- If relay dial/handshake reports unauthorized, refresh/redial once using
  `refresh_if_current(origin, token_at_dial)`.
- Relay transient faults use the buffered `send_over_relay` retry policy:
  bounded by `RELAY_MAX_TRANSIENT_ATTEMPTS`.

This deliberately adopts buffered semantics for bridge carrier dials. That is a
small behavior change for the old SSE-only fresh carrier path, which previously
did not retry relay transient faults. The persistent carrier serves both
buffered and SSE requests, and using the stronger existing buffered policy gives
one deterministic dial policy without changing cadence paths.

## Logging and Error Tokens

Reuse existing categories where possible:

- Local authorization rejects: `local_capability_reject` plus stable
  `RejectReason` token.
- Upstream credential status: `upstream_credential`.
- Dial, read, mux, relay, keepalive, or write failure before local head:
  `upstream_unreachable` plus `transport_error_code`.

Add a new failure category only if implementation needs to distinguish local
writer-queue saturation from upstream failure. Default plan: do not add one;
report it as `upstream_unreachable code=io` to keep the redacted vocabulary
stable.

Logs must never include cap values, cookie names, paths, queries, request bodies,
tokens, host endpoints, or relay origins.

## Clean Break Deletions

Deleted in the clean break:

- `ObserverClient::request`.
- `ObserverClient::request_stream`.
- `ObserverClient::send_stream`.
- `ObserverClient::send_stream_over_relay`.
- `connection::request_stream`.
- `connection::run_request_stream_over_stream`.
- `connection::run_request_stream_loop`.
- `connection::end_or_error`.
- `relay::request_stream_relay`.
- `relay::request_stream_over_ws`.
- `observer_pl::mux::StreamingResponseAssembler`.
- `observer_pl::mux::StreamingFeed`.
- Client-level tests that only exercised deleted bridge helper methods:
  `buffered_proxy_request_forwards_auth_accept_and_body`,
  `buffered_proxy_request_preserves_401_as_response`,
  `buffered_proxy_request_strips_caller_auth_headers`,
  `sse_streams_head_first_body_items_then_close`,
  `sse_chunked_body_is_decoded_incrementally`,
  `sse_reset_after_head_returns_ok_with_reset_end`,
  `sse_eof_before_head_returns_error_without_head_item`.
- Streaming assembler unit tests tied to `StreamingResponseAssembler`.

Replacement coverage lands in pure `CarrierDemux`/`HttpStreamAssembler` tests
and bridge-level persistent-carrier tests. `ResponseAssembler`,
`WindowedUpload`, `request_once`, `run_request_over_stream`,
`request_once_relay`, pairing, register, ingest, heartbeat, and reconcile stay.

## Test Scaffolding Plan

Add a persistent multi-stream TLS server helper under
`crates/pl-transport-win/tests/transport_round_trip.rs` or a test support module.

Harness capabilities:

- Accept one TCP/TLS carrier and keep it open across multiple local bridge
  requests.
- Use one server-side `FrameDecoder`.
- Demux concurrent odd stream ids.
- Assemble request bytes per stream until CLOSE.
- Hold, interleave, close, or reset individual streams.
- Inject stream-0 PING and assert one matching PONG.
- Consume client PING and return PONG.
- Enforce per-stream WINDOW credit for two concurrent large uploads; RESET only
  the stream that overruns credit.
- Count accepted upstream carriers so first-load coalescing can assert one dial.

Add Tokio `test-util` to `pl-transport-win` dev-dependencies so tests can use
`#[tokio::test(start_paused = true)]`, `tokio::time::pause`, and
`tokio::time::advance` for keepalive.

## Test Map

Because the external scope's section 7 is not in this file, these are the 16
tests this design expects to carry the requested coverage.

Pure `observer-pl`:

1. `frame_control_ping_and_pong_nonce_round_trip`: `control_ping`,
   `control_pong`, and `control_pong_nonce` are byte-identical.
2. `http_stream_assembler_emits_head_body_and_split_head`: replacement coverage
   for head-first streaming.
3. `http_stream_assembler_dechunks_incrementally`: replacement coverage for
   chunked SSE bodies.
4. `carrier_demux_routes_interleaved_streams`: two stream ids receive only their
   own head/body/end events.
5. `carrier_demux_control_and_window_outputs_are_tagged`: PING returns one PONG,
   inbound PONG nonce is surfaced, and WINDOW grants are tagged by stream id.
6. `carrier_demux_drops_unknown_or_closed_stream_frames`: defensive drops do not
   panic or leak events.

`pl-transport-win` bridge/carrier integration:

7. `journal_bridge_bootstrap_cookie_contract_unchanged`: current bootstrap 302
   and reject behavior stays byte-identical.
8. `journal_bridge_rejects_before_carrier_dial`: bad host/cap/method/auth all
   return 403/405 and upstream accept count is zero, including POST bootstrap.
9. `journal_bridge_first_load_coalesces_one_carrier`: concurrent authorized
   requests result in one upstream TLS accept and distinct odd stream ids.
10. `journal_bridge_buffered_pass_through_on_shared_carrier`: status/body,
   auth injection, local header stripping, response headers, content-length, and
   connection close match current behavior.
11. `journal_bridge_head_preserves_content_length_on_shared_carrier`: HEAD keeps
   upstream content length and sends no body.
12. `journal_bridge_forwards_401_403_and_warns`: both statuses pass through
   unmasked with `upstream_credential`.
13. `journal_bridge_sse_does_not_block_buffered_request`: long-lived SSE stream
   and normal GET complete concurrently on one carrier.
14. `journal_bridge_sse_before_head_502_after_head_close`: before-head EOF/RESET
   maps to 502; after-head close/reset just closes local stream.
15. `journal_bridge_local_write_failure_resets_only_that_stream`: abandoned or
   full local consumer sends RESET for one stream and siblings continue.
16. `journal_bridge_carrier_death_redials_without_replay`: carrier EOF/error
   ends active streams, marks slot dead, next request dials a fresh carrier, and
   no old request is replayed.

Additional transport stress:

- `journal_bridge_two_concurrent_uploads_keep_window_credit_isolated`: two POSTs
  over the initial window complete only if WINDOW grants are routed per stream.
- `journal_bridge_keepalive_missed_probe_tears_down_carrier`: paused-time test
  proving PING/PONG liveness and dead-slot replacement.

Receive-window flow control:

- A1: `window_and_reset_builders_encode_protocol_payloads`,
  `recv_window_grants_all_consumed_bytes_at_half_window`, and
  `recv_window_rejects_over_credit_without_mutation` pin the pure frame and
  receive-window primitives.
- B1: `one_shot_response_over_initial_window_replenishes_peer_credit` and
  `carrier_response_over_initial_window_replenishes_credit_on_consumer_drain`
  reproduce the former 1 MiB stall at both transport seams.
- B2: `carrier_without_body_drain_depletes_window_then_flow_control_resets`
  proves a full delivery queue cannot manufacture grants or trigger
  RESET(CANCEL).
- B3: `response_assembler_grants_exact_wire_bytes_at_half_window` and
  `response_assembler_subthreshold_response_emits_no_window` cover one-shot;
  `carrier_grants_exact_wire_bytes_after_body_drain` and
  `carrier_subthreshold_response_emits_no_window` cover the carrier; and
  `carrier_chunked_window_counts_framing_wire_bytes_on_drain` pins chunk-framing
  wire-byte attribution.
- B4: `response_assembler_over_credit_emits_one_flow_control_reset` and
  `carrier_demux_over_credit_resets_only_offending_stream` provide pure-tier
  coverage; `one_shot_over_window_writes_one_flow_control_reset_before_error`
  and `carrier_over_window_resets_one_stream_and_keeps_sibling_alive` cover both
  transport seams.
- End-time accounting: `response_assembler_close_suppresses_terminal_window`,
  `carrier_consume_after_close_emits_no_late_window`, and
  `carrier_close_suppresses_decode_time_window` pin immediate state removal and
  the absence of late grants.
- B5: `response_assembler_cap_remains_exactly_four_mib` keeps the one-shot 4 MiB
  assembled-response ceiling independent of replenished receive credit.

## Implementation Order

1. Add pure frame helpers, `HttpStreamAssembler`, and `CarrierDemux` with tests.
2. Add LAN/relay carrier dial helpers and `ObserverClient::dial_carrier`.
3. Add `ObserverClient::proxy_headers`.
4. Add `journal_bridge_carrier.rs` with coordinator/writer tasks and focused
   unit-level transport tests where possible.
5. Rewire `journal_bridge.rs` to consume `MuxCarrier`.
6. Add persistent multi-stream integration harness and bridge tests.
7. Delete orphaned streaming request APIs and old tests.
8. Run `make test`; run `make ci` if the code changes touch generated contract
   or dependency policy.

## Open Decisions

- Production keepalive values: this design needs concrete defaults for interval,
  deadline, and missed count. Proposed starting point: 30s interval, 10s
  deadline, 3 missed probes.
- Writer queue size: proposed default is 256 frame buffers; stress tests should
  validate this is enough for first-load fanout without hiding write stalls.
- Whether to keep `journal_bridge_carrier.rs` private at crate root or convert
  `journal_bridge.rs` into a module directory. Root-private is the smaller
  change.
