# Screen H.264 encoder design

## Purpose

Add a Windows screen H.264 encoder that writes one MP4 screen file per clock-aligned segment while preserving the existing engine-owned rotation model, honest health, and pure/platform DAG.

This is a design-gate document only. It proposes no implementation code in this stage.

## Confirmed Facts

- `windows-capture` v2.0.0 `Frame` exposes GPU accessors: `as_raw_surface() -> &IDirect3DSurface` and `as_raw_texture() -> &ID3D11Texture2D`; it also exposes the current CPU path via `buffer()`. `width()` and `height()` return `D3D11_TEXTURE2D_DESC.Width/Height`. Source: `docs/notes/encoder-prep.md`, Unknown 1.
- WGC currently configures `ColorFormat::Rgba8` at `crates/capture-wgc/src/lib.rs:151-164`, and `on_frame_arrived` currently discards frame width, height, and color format after converting to a CPU buffer at `crates/capture-wgc/src/lib.rs:88-100`.
- The current engine has one rotation owner: `observer-segment::should_rotate` and `CaptureEngine::rotate_if_needed` (`crates/observer-segment/src/lib.rs:44-49`, `crates/capture-engine/src/lib.rs:397-422`).
- Local `make ci` excludes Windows-only crates through `REMOTE_CRATES` (`Makefile:15-18`) and runs clippy/tests with that exclude list (`Makefile:63-68`). Therefore pure conversion tests run locally only if conversion stays in a non-excluded pure crate.
- `scripts/win-ci.cmd` builds and tests the whole workspace except `solstone-windows-app` (`scripts/win-ci.cmd:32-35`). A new workspace crate is included automatically unless it is the app.
- The contract generator derives token vocabulary from `AppPhase`, `SourceKind`, `SourceState` status tokens, `PauseReason`, `ErrorReason`, and `PairingPhase` only (`crates/observer-contract/src/lib.rs:107-117`, `:211-221`). Adding a non-contract pixel-format enum and a new `HealthDump` field does not change `automation-contract.json`.
- The coordinator is filename-agnostic: it iterates `segment.files`, reads each name, and uploads/reconciles by filename and sha (`crates/pl-transport-win/src/coordinator.rs:68-78`, `:86-99`). `docs/notes/encoder-prep.md` confirmed no legacy screen-file literal in the coordinator.

## D1 - Engine-Owned Encoder Seam

Use the senior ruling: engine-owned encoder via a new pure trait seam. No pushback on ownership: keeping the encoder in the engine is cleaner than folding it into `capture-wgc` because the engine already owns the only segment clock and the only seal/rename decision. Putting finalization inside the WGC source would create a second rotation owner.

New pure types in `observer-model`:

- `ScreenPixelFormat`: plain non-contract enum with `Rgba8` and `Bgra8`. It must not derive `EnumIter` or `IntoStaticStr`, and `observer-contract::state_tokens()` must not import it.
- `ScreenFrame`: owned screen frame data with `seq: u64`, `width: u32`, `height: u32`, `pixel_format: ScreenPixelFormat`, and `pixels: Arc<[u8]>`.
- `EncoderErrorKind`: plain non-contract enum with at least `OpenFailed`, `EncodeFailed`, `FinalizeFailed`, `InvalidFrameDimensions`, `DeviceLost`, `Unavailable`, and `WorkerStopped`.
- `EncoderError`: plain error object with `kind: EncoderErrorKind` and `detail: String`. The engine always maps it to `ErrorReason::WriteFailed` in `SourceState::Faulted`.
- `EncoderHealth`: serde model with `frames_consumed: u64`, `samples_written: u64`, and `last_error: Option<String>`.
- `ScreenEncoder: Send`: trait with these public methods:
  - `open(&mut self, dir: &str, width: u32, height: u32) -> Result<(), EncoderError>`
  - `encode_frame(&mut self, frame: &ScreenFrame) -> Result<(), EncoderError>`
  - `finalize(&mut self) -> Result<(), EncoderError>`
  - `frames_consumed(&self) -> u64`
  - `samples_written(&self) -> u64`
  - `last_error(&self) -> Option<String>`
  - `health(&self) -> EncoderHealth`

`Sources` in `capture-engine/src/lib.rs` gets `screen_encoder: Box<dyn ScreenEncoder>`. `src-tauri/src/app.rs` injects `capture_screen_encode::MfScreenEncoder::new()` next to `WgcScreenSource::new()` (`src-tauri/src/app.rs:103-107`).

Engine behavior changes:

- `CaptureEngine` receives a screen event as `ScreenFrame`; it no longer sends screen bytes to `SegmentFs::write_chunk`.
- `SegmentFs::write_chunk` remains the audio-only path for `SystemAudio` and `Mic`.
- `OpenSegment` gains screen encoder state: pinned screen meta, whether the encoder has opened for this segment, and the existing `screen_chunks` counter.
- `open_current_segment` still opens only the `.incomplete` dir and records the dir path. It does not call `ScreenEncoder::open` because the first frame supplies the native dimensions.
- On the first screen frame for a segment, the engine calls `screen_encoder.open(segment.dir, frame.width, frame.height)` and then `screen_encoder.encode_frame(&frame)`.
- On later screen frames, the encoder accepts only frames matching the dimensions pinned at `open`.
- `rotate_if_needed` finalizes the screen encoder before `segment_fs.finalize`.
- `stop` stops sources, drains queued events, calls `screen_encoder.finalize`, and only then calls `segment_fs.finalize`.

## D2 - Frame Metadata Channel

Do not add `Option<ScreenFrameMeta>` to `CaptureChunk`. That would force every existing audio struct literal to add `screen_meta: None`, contradicting the requirement to keep audio call sites unchanged (`crates/capture-wasapi/src/lib.rs:186`, `crates/platform-win/src/lib.rs:454-459`, `crates/capture-engine/src/lib.rs:925-929`).

Use a channel-message split:

- Leave `CaptureChunk` unchanged for audio: `source`, `seq`, `data`.
- Extend `CaptureSink` with a screen-specific method `emit_screen_frame(&self, frame: ScreenFrame)`. The existing `emit(CaptureChunk)` method remains unchanged, so WASAPI call sites stay as they are.
- `EngineSink` internally sends an enum, conceptually `Audio(CaptureChunk) | Screen(ScreenFrame)`, to the engine queue.
- `capture-wgc` changes only its screen call site: after `frame.buffer()`, it records `frame.width()`, `frame.height()`, and maps `windows_capture::settings::ColorFormat::{Rgba8,Bgra8}` to `ScreenPixelFormat`; then it emits `ScreenFrame`.

This keeps WGC honest about RGBA/BGRA without adding a contract-token enum. Because `ScreenPixelFormat` is not in `observer-contract::state_tokens()`, the contract drift gate remains untouched.

Decided: this is the capture channel seam. Any `CaptureSink` implementation or test fake must implement the new `emit_screen_frame` method. Current repo search found only `EngineSink` in `crates/capture-engine/src/lib.rs`; capture-engine test helpers that fabricate screen delivery must call the new method, and no other test `CaptureSink` implementation exists today.

## D3 - Pure `observer-nv12` Crate

Add `crates/observer-nv12` as a pure crate:

- `#![forbid(unsafe_code)]`.
- Workspace member and workspace dependency.
- Add to `xtask/src/main.rs` `PURE_CRATES` so `cargo xtask purity-check` covers it (`xtask/src/main.rs:119-131`).
- Do not add it to `Makefile` `REMOTE_CRATES`; it must run in local fast CI.

It owns RGBA8/BGRA8 to NV12 conversion. Use BT.601 limited range:

- `Y = 16 + 0.257R + 0.504G + 0.098B`
- `U = 128 - 0.148R - 0.291G + 0.439B`
- `V = 128 + 0.439R - 0.368G - 0.071B`
- Clamp Y to 16..235 and U/V to 16..240.
- NV12 output is full Y plane followed by interleaved UV plane, one U/V pair per 2x2 block. Chroma is computed from the average RGB of each 2x2 block.

Public pure API shape:

- `Nv12Error` for invalid dimensions, unsupported pixel format, and buffer length mismatch.
- `Nv12Frame` with `width`, `height`, and `bytes`.
- `rgba_or_bgra_to_nv12(frame: &ScreenFrame) -> Result<Nv12Frame, Nv12Error>`.

Justification for a separate pure crate: if conversion lived inside `capture-screen-encode`, it would be excluded by the local `make ci` fast checks once that platform crate is added to `REMOTE_CRATES`. Keeping conversion pure gives AC2 Linux coverage.

AC2 tests in `observer-nv12`:

- `solid_colors_match_bt601_limited_range`
- `checker_gradient_sets_expected_luma_and_chroma`
- `rgba_and_bgra_byte_order_guard_changes_uv`
- `uv_interleave_order_guard_rejects_swapped_planes`

Use exact expected values for solid colors and tolerance of +/- 1 for gradient/checker rounding.

## D4 - Platform `capture-screen-encode` Crate

Add `crates/capture-screen-encode` as a platform-tier crate mirroring `capture-wgc`:

- `#[cfg(windows)] mod imp` contains the Media Foundation and COM glue.
- `#[cfg(not(windows))] mod imp` exposes an honest inert stub that compiles off Windows and returns `EncoderErrorKind::Unavailable` from encode/open paths if used.
- Implements `observer_model::ScreenEncoder`.
- Depends on `observer-model` and `observer-nv12` only, plus target-gated `windows-rs` on Windows.
- Does not depend on `platform-win`; the final screen filename comes from the pure `observer_model::SCREEN_FILE_NAME` constant.

Cargo edits:

- Root `Cargo.toml` workspace members: add `crates/observer-nv12` and `crates/capture-screen-encode`.
- Root `Cargo.toml` workspace dependencies: add `observer-nv12` and `capture-screen-encode`.
- `src-tauri/Cargo.toml`: add `capture-screen-encode.workspace = true`.
- `capture-engine/Cargo.toml`: no platform dependency; it depends only on `observer-model` as the trait seam.
- `Makefile`: append `--exclude capture-screen-encode` to `REMOTE_CRATES` at `Makefile:18`.
- `scripts/win-ci.cmd`: no explicit crate-name edit. It builds/tests `--workspace --exclude solstone-windows-app`, so the new crate is included automatically once it is a workspace member (`scripts/win-ci.cmd:32-35`).

Windows dependencies:

- Use `windows = { version = "0.58", features = ["Win32_Media_MediaFoundation", "Win32_System_Com"] }` under `[target.'cfg(windows)'.dependencies]`.
- Do not add `Win32_System_Variant` for GOP if using `windows_core::VARIANT::from(90u32)`; prep confirmed that avoids it.
- No version bump: prep confirmed all requested symbols exist in `windows` 0.58.

Media Foundation sequence:

- Dedicated encoder thread initializes COM with `CoInitializeEx`, then Media Foundation with `MFStartup(MF_VERSION, ...)`; teardown calls `MFShutdown` and `CoUninitialize`. Prep URLs: `docs/notes/encoder-prep.md`, Unknown 2 lifecycle.
- Plain MP4 construction uses `MFCreateSinkWriterFromURL` on the `.partial` path.
- Sink-writer attributes include `MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS=TRUE` and `MF_SINK_WRITER_DISABLE_THROTTLING=TRUE`.
- Do not set `MF_READWRITE_USE_ONLY_HARDWARE_TRANSFORMS`; software fallback remains available when no hardware MFT exists.
- Do not set `MF_SINK_WRITER_D3D_MANAGER`; this is the CPU-fed path.
- Output type: `MF_MT_MAJOR_TYPE=MFMediaType_Video`, `MF_MT_SUBTYPE=MFVideoFormat_H264`, `MF_MT_AVG_BITRATE=1_000_000`, `MF_MT_FRAME_SIZE=width x height`, `MF_MT_FRAME_RATE=1/1`, `MF_MT_PIXEL_ASPECT_RATIO=1/1`, `MF_MT_INTERLACE_MODE=MFVideoInterlace_Progressive`, `MF_MT_MPEG2_PROFILE=eAVEncH264VProfile_High`.
- Input type: `MF_MT_MAJOR_TYPE=MFMediaType_Video`, `MF_MT_SUBTYPE=MFVideoFormat_NV12`, same frame size, frame rate, pixel aspect ratio, and progressive interlace mode.
- GOP: after the encoder is available through the sink writer, call `IMFSinkWriter::GetServiceForStream(stream, GUID_NULL, ICodecAPI::IID)` and `ICodecAPI::SetValue(CODECAPI_AVEncMPVGOPSize, 90)`.
- Sample feed: convert to NV12, create memory buffer/sample, set sample time and duration in 100 ns units, then `WriteSample`.

## D5 - Container, Finalization, and Recovery

Recommended v1 ruling to present to Jer: plain MP4 with finalize-at-rotation and orphan quarantine.

Use plain MP4, not fragmented MP4:

- Plain MP4 via `MFCreateSinkWriterFromURL` writes the `moov` metadata at `Finalize`.
- We quarantine unfinalized orphans in v1, so fMP4 partial-playability does not buy operational value yet.
- Prep could not confirm from MS Learn that periodic `Flush` makes a partial fMP4 already playable or crash-recoverable.
- fMP4 parity should wait for a follow-up gate with live validation on the Windows box.

Bold invariant:

**A segment dir is sealed from `<index>.incomplete` to `<index>` only after `ScreenEncoder::finalize()` succeeds. `.incomplete` means the screen MP4 may not have finalized and the segment must not upload as complete.**

Finalization marker:

- While a segment is open, the encoder targets `display_1_screen.mp4.partial`.
- After `IMFSinkWriter::Finalize()` succeeds, the encoder renames that file to `display_1_screen.mp4`.
- If finalize fails or the process crashes before finalize completes, `.partial` remains.
- If finalize succeeds but the process crashes before the dir rename, no `.partial` remains and recovery can seal the orphan.

Recovery predicate:

- Existing recovery semantics quarantine empty/corrupt dirs (`observer-recovery::StaleSegment.has_usable_data` means usable/sealable at `crates/observer-recovery/src/lib.rs:21-24`, `:50-67`).
- An orphan `.incomplete` dir is sealable iff it contains no `*.partial` file and contains at least one usable final media file with nonzero length.
- A `*.partial` file always vetoes sealing and sends the whole dir to quarantine.

`platform-win` changes:

- Rename or retarget `has_usable_media` (`crates/platform-win/src/lib.rs:70-78`) to a `.partial`-aware `has_sealable_media`.
- `LocalRecoveryFs::scan_incomplete` continues to populate `StaleSegment.has_usable_data`; no pure `observer-recovery` change is required.
- Add platform tests:
  - `recovery_quarantines_orphan_with_partial_screen_mp4`
  - `recovery_seals_orphan_with_final_screen_mp4_and_no_partial`
  - `recovery_quarantines_empty_orphan_even_without_partial`

Rotation sequence:

1. Engine observes a clock-boundary crossing through existing `should_rotate`.
2. Engine calls `screen_encoder.finalize()` for the current segment.
3. If finalize succeeds, engine calls `segment_fs.finalize(old_key)` to rename the dir.
4. Engine opens the next `.incomplete` dir.
5. Encoder `open` for the next segment is deferred until the first screen frame supplies dimensions.
6. If finalize fails, engine does not call `segment_fs.finalize`, reports Screen as `Faulted { reason: WriteFailed, detail }`, returns an engine error following the existing write-failure precedent, and leaves the dir `.incomplete`.

Audio in orphan note: quarantine moves the whole `.incomplete` dir aside, so any audio in that in-flight orphan is preserved but set aside. That is acceptable for v1 and affects at most the segment active at crash. Do not build per-file recovery.

Alternative for Jer: fMP4 plus periodic `Flush`, with recovery sealing flushed orphans as already playable. This is closer to macOS movie-fragment parity but adds COM/container surface and remains unverifiable in-lode until a post-ship/live Windows validation gate.

## D6 - Finalize Failure, Accounting, and Contract

AC4 behavior:

- `encoder.finalize()` returns `Err`.
- Engine reports Screen as `SourceState::Faulted { reason: ErrorReason::WriteFailed, detail }`.
- Engine does not call `segment_fs.finalize`.
- Current dir remains `<index>.incomplete`.
- Engine returns an error just like current `SegmentFs::write_chunk` failures do after faulting the source (`crates/capture-engine/src/lib.rs:378-389`).
- `stop` uses the same ordering and failure behavior.

Named test: `finalize_error_leaves_segment_incomplete_and_faults_screen_write_failed`.

AC5 health/accounting:

- Add `screen_encoder: Option<EncoderHealth>` to `HealthDump`.
- Running engine dumps use `Some(screen_encoder.health())`.
- `not_running_snapshot` and terminal app-error snapshots use `None`.
- A visible problem is either `last_error.is_some()` or `frames_consumed > samples_written`.
- `encode_frame` errors map to `ErrorReason::WriteFailed` using the same fault path as write failures.

Fixtures and consumers to update:

- `crates/observer-model/src/lib.rs:230-247` `HealthDump`
- `crates/observer-health/src/lib.rs:32-42` sample fixture
- `crates/capture-engine/src/lib.rs:232-241` `health_dump`
- `crates/capture-engine/src/lib.rs:585-595` `empty_health`
- `crates/capture-engine/src/lib.rs:1193-1202` `loopback_health_serves_fixed_dump`
- `src-tauri/src/health.rs:26-36` `not_running_snapshot`
- `src-tauri/src/app.rs:214-223` terminal health dump
- `ui/src/main.ts:49-58` `HealthDump` TypeScript interface

Contract impact:

- Reuse `ErrorReason::WriteFailed`.
- Add no new `ErrorReason` variant.
- Add no new contract-token enum.
- Do not regenerate `automation-contract.json` or `ui/src/lib/contract.ts`.
- The totality test at `crates/observer-contract/src/lib.rs:211-221` stays unchanged.

## D7 - Thread Model, Timestamps, and Degradation

Thread model:

- The platform encoder owns one dedicated OS thread.
- `Box<dyn ScreenEncoder>` in the engine is `Send` because `engine.run()` is spawned through `tauri::async_runtime::spawn` (`src-tauri/src/app.rs:162-164`).
- Media Foundation COM objects and `IMFSinkWriter` stay on the dedicated thread; the trait object held by the engine is only a sendable channel handle plus shared health/error state.
- `open` blocks for an ack.
- `finalize` blocks for an ack and drains all pending frame work before returning.
- `encode_frame` is fire-and-forget for real frame writes so WGC arrival stays non-blocking.
- The send handle caches the dimensions accepted by `open` and synchronously returns `Err(InvalidFrameDimensions)` for mismatched frame dimensions without a worker round trip. The fake encoder used by engine tests must implement the same handle-side check.
- Worker-side `WriteSample` failures, device-removed HRESULTs, and other real encode failures are recorded as sticky `last_error` in shared state. A known sticky worker error may also cause later `encode_frame` calls to return `Err` before posting new frames.

Decided async error reporting: the engine reads `screen_encoder.last_error()`/`screen_encoder.health()` every pump/tick and folds sticky errors into Screen `Faulted { reason: WriteFailed, detail }` alongside the existing `fold_source_states`/`refresh_health` path. In `pump()`, this read/fold happens after event draining, rotation checks, and due-source retries, then before the refreshed health dump is published. In `run()`, the interval tick performs the same read/fold after `rotate_if_needed`/`retry_due_sources` and before `refresh_health`. A real write/device failure therefore surfaces within <=1 engine tick, not from the original fire-and-forget `encode_frame` call.

Timestamp policy:

- Deterministic per-segment uniform clock.
- `FRAME_DURATION_100NS = 10_000_000`.
- Per-segment encoded frame index starts at 0 on `open`.
- `sample_time = frame_index * FRAME_DURATION_100NS`.
- `sample_duration = FRAME_DURATION_100NS`.
- Do not use wall-clock arrival time; this avoids jitter and gappy timelines.

AC7 behaviors:

- Resolution/display change mid-segment: encoder pins dimensions at `open`. If a later frame's dimensions differ, the send handle synchronously returns `Err(InvalidFrameDimensions)` before posting to the writer, increments consumed but not written, records `last_error`, and the engine folds that into a transient Screen `WriteFailed` fault. The next clock rotation's first frame opens with the new dimensions.
- Runtime hardware-MFT loss or `DXGI_ERROR_DEVICE_REMOVED`: worker records the HRESULT as sticky `DeviceLost`; the engine folds it into a Screen `WriteFailed` fault on the next tick, current finalize fails, dir remains `.incomplete`, and the next segment's `open` re-resolves the MFT. No in-place mid-segment reopen.
- Zero-frame window: lazy writer construction. `open` records target path and dimensions and builds inspectable config, but the real sink writer/file is constructed only on the first frame. If no frames arrive, no `.partial` and no `display_1_screen.mp4` exist. Empty sealed dirs remain ingest-safe because the coordinator drops empty sealed dirs (`crates/pl-transport-win/src/coordinator.rs:80-83`).

Named AC7 tests:

- `drops_mismatched_resolution_until_next_rotation_and_reports_delta`
- `device_removed_finalize_failure_leaves_incomplete_and_next_open_retries_mft`
- `zero_frame_window_produces_no_screen_file_and_empty_upload_is_dropped`

## D8 - Filename, Config, and Upload

Filename:

- Final screen file: `display_1_screen.mp4`.
- Partial file while open: `display_1_screen.mp4.partial`.
- Base before extension is `display_1_screen`, matching journal regex `^([a-z-]+)_([A-Za-z0-9-]+)_screen$` after extension/prefix handling: `display` and `1`.
- V1 is primary-monitor only, so `1` is fixed.

Single source of truth:

- Add `pub const SCREEN_FILE_NAME: &str = "display_1_screen.mp4";` to `observer-model`.
- `platform_win::source_file_name(SourceKind::Screen)` returns `observer_model::SCREEN_FILE_NAME`; it no longer owns the literal.
- `capture-screen-encode` references `observer_model::SCREEN_FILE_NAME` to build the final target and the temporary `.partial` path.
- `ScreenEncoder::open(dir, width, height)` keeps no filename parameter.
- `.partial` remains an encode/recovery lifecycle detail, not a public file contract. Use a local `PARTIAL_SUFFIX = ".partial"` in the encoder crate, and the platform recovery predicate checks the same suffix pattern to veto any `*.partial` orphan.

Migration list from the legacy raw screen filename to `display_1_screen.mp4`:

- `crates/observer-model/src/lib.rs` new `SCREEN_FILE_NAME` pure constant carrying `display_1_screen.mp4`
- `docs/design-1A-capture-core.md:109` doc/comment
- `docs/design-1A-capture-core.md:117` doc/comment
- `crates/platform-win/src/lib.rs:64` product filename contract; replace hardcoded literal with `observer_model::SCREEN_FILE_NAME`
- `crates/platform-win/src/lib.rs:499` test expected screen file
- `crates/platform-win/src/lib.rs:527` recovery fixture
- `crates/platform-win/src/lib.rs:535` live-segment recovery fixture
- `crates/platform-win/src/lib.rs:566` just-crossed recovery fixture
- `crates/pl-transport-win/src/sealed.rs:7` module comment
- `crates/pl-transport-win/src/sealed.rs:146` sealed-store fixture
- `crates/pl-transport-win/src/sealed.rs:158` expected file list
- `crates/pl-transport-win/src/sealed.rs:159` read-file assertion
- `crates/pl-transport-win/examples/live_gate.rs:63` fabricated sealed segment
- `crates/observer-pl/src/wire.rs:217` segment-list fixture
- `crates/observer-pl/src/multipart.rs:68` multipart fixture filename
- `crates/observer-pl/src/multipart.rs:81` multipart expected body

Scope note: prep confirmed `platform-win/src/lib.rs:503` and `:506` are audio filename assertions in this checkout, not legacy screen-file hits.

AC1 inspectable config:

- Add pure `EncoderConfig`/`ScreenEncoderConfig` in `observer-model`, not in the platform crate, so AC1 runs in local CI.
- Logical fields: bitrate `1_000_000`, frame size native width x height, frame rate `1/1`, pixel aspect `1/1`, progressive interlace, H.264 High profile, GOP size `90`, hardware transforms enabled, use-only-hardware disabled, D3D manager disabled, throttling disabled.
- Windows `imp` consumes this config and maps it to Media Foundation attributes.
- Test name: `encoder_config_matches_ac1_media_foundation_defaults`.

AC6 upload:

- Coordinator ships the renamed file unchanged because it is filename-agnostic.
- Add `.mp4 -> video/mp4` to `content_type_for` (`crates/pl-transport-win/src/sealed.rs:36-40`). It is advisory, but useful and low risk.
- Test name: `mp4_content_type_is_video_mp4`.

## Cargo And CI Change List

Add files/crates in implementation stage:

- `crates/observer-nv12`
- `crates/capture-screen-encode`

Update existing files in implementation stage:

- `Cargo.toml` workspace members/dependencies
- `Makefile` `REMOTE_CRATES`
- `xtask/src/main.rs` `PURE_CRATES`
- `src-tauri/Cargo.toml`
- `src-tauri/src/app.rs`
- `crates/observer-model/src/lib.rs`
- `crates/capture-engine/src/lib.rs`
- `crates/capture-wgc/src/lib.rs`
- `crates/platform-win/src/lib.rs`
- `crates/pl-transport-win/src/sealed.rs`
- `crates/pl-transport-win/examples/live_gate.rs`
- `crates/observer-pl/src/multipart.rs`
- `crates/observer-pl/src/wire.rs`
- `crates/observer-health/src/lib.rs`
- `src-tauri/src/health.rs`
- `ui/src/main.ts`
- `docs/design-1A-capture-core.md`

No implementation-stage edit expected:

- `scripts/win-ci.cmd` unless future changes stop using whole-workspace build/test.
- `automation-contract.json`
- `ui/src/lib/contract.ts`

## Decisions For Jer To Confirm

1. D5 product call: approve plain MP4 plus finalize-at-rotation plus orphan quarantine (recommended), or choose fMP4 plus periodic `Flush` plus seal-flushed orphan for macOS parity (deferred; partial-playability is unverifiable in-lode).

Senior-decided (flag if you disagree): D2 channel-split, D7 sticky-async errors plus sync dim-reject, D8 pure `SCREEN_FILE_NAME` constant.

## AC To Test Map

| AC | Coverage |
|---|---|
| AC1 H.264 config and fallback attributes | `encoder_config_matches_ac1_media_foundation_defaults` in pure local CI; Windows compile in `scripts/win-ci.cmd` verifies `windows` 0.58 symbol wiring |
| AC2 RGBA/BGRA to NV12 conversion | `solid_colors_match_bt601_limited_range`; `checker_gradient_sets_expected_luma_and_chroma`; `rgba_and_bgra_byte_order_guard_changes_uv`; `uv_interleave_order_guard_rejects_swapped_planes` in `observer-nv12` |
| AC3 clock rotation/finalize/recovery policy | `rotation_finalizes_encoder_before_sealing_segment`; `recovery_quarantines_orphan_with_partial_screen_mp4`; `recovery_seals_orphan_with_final_screen_mp4_and_no_partial`; `recovery_quarantines_empty_orphan_even_without_partial` |
| AC4 finalize failure leaves dir incomplete and faults screen | `finalize_error_leaves_segment_incomplete_and_faults_screen_write_failed` |
| AC5 honest encoder accounting in health | `encoder_health_is_folded_into_health_dump`; `encode_error_faults_screen_with_write_failed`; existing `observer-health` round-trip updated for `screen_encoder` |
| AC6 renamed MP4 upload compatibility | `coordinator_uploads_renamed_mp4_filename_unchanged`; `mp4_content_type_is_video_mp4` |
| AC7 degradation behaviors | `drops_mismatched_resolution_until_next_rotation_and_reports_delta`; `device_removed_finalize_failure_leaves_incomplete_and_next_open_retries_mft`; `zero_frame_window_produces_no_screen_file_and_empty_upload_is_dropped` |
