// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The webview is a pure renderer. It subscribes to `health://changed` and paints
// the honest state it receives; it has no other input and cannot mint status.
// AutomationIds are stamped from the generated contract (see ./lib/contract.ts).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { automationContract } from "./lib/contract";

installFrontendErrorHandlers();

type FrontendOrigin = "settings" | "about" | "none";
type FrontendErrorKind = "error" | "unhandled_rejection";

interface FrontendErrorRecord {
  kind: FrontendErrorKind;
  level: "error";
  origin: FrontendOrigin;
  line: number;
  column: number;
}

function installFrontendErrorHandlers(): void {
  try {
    window.addEventListener("error", (event: ErrorEvent) => {
      try {
        forwardFrontendError({
          kind: "error",
          level: "error",
          origin: resolveFrontendOrigin(),
          line: event.lineno || 0,
          column: event.colno || 0,
        });
      } catch {
        // Never let the error handler recurse.
      }
    });
    window.addEventListener("unhandledrejection", (_event: PromiseRejectionEvent) => {
      try {
        forwardFrontendError({
          kind: "unhandled_rejection",
          level: "error",
          origin: resolveFrontendOrigin(),
          line: 0,
          column: 0,
        });
      } catch {
        // Never let the error handler recurse.
      }
    });
  } catch {
    // Frontend error capture must be best-effort.
  }
}

function resolveFrontendOrigin(): FrontendOrigin {
  try {
    const label = getCurrentWindow().label;
    if (label === "settings" || label === "about") {
      return label;
    }
  } catch {
    // Tauri API unavailable during early module evaluation.
  }
  return "none";
}

function forwardFrontendError(record: FrontendErrorRecord): void {
  try {
    void invoke("log_frontend_error", { record }).catch(() => {});
  } catch {
    // Fire-and-forget only.
  }
}

type AppPhase = "idle" | "starting" | "observing" | "paused" | "error";
type SourceKind = "screen" | "system_audio" | "mic";
type SourceStatus = "active" | "inactive" | "no_input_device" | "faulted";
type Severity = "ok" | "neutral" | "attention";
type Route = "home" | "sources" | "privacy" | "journal" | "shortcut" | "storage" | "updates";

interface SourceReport {
  kind: SourceKind;
  status: SourceStatus;
  reason?: string;
  detail?: string;
  device?: string | null;
}

type PairingPhase = "not_paired" | "pairing" | "paired" | "failed";

interface PairingState {
  phase: PairingPhase;
  journal_label: string | null;
  observer_name: string | null;
  detail: string | null;
}

interface UploadStatus {
  pending_segments: number;
  uploaded_segments: number;
  failed_segments: number;
  last_uploaded_segment: string | null;
  last_error: string | null;
  heartbeat_ok: boolean;
}

interface SyncSnapshot {
  pairing: PairingState;
  upload: UploadStatus;
}

interface EncoderHealth {
  frames_consumed: number;
  samples_written: number;
  last_error: string | null;
}

interface ExclusionHealth {
  rules_active: boolean;
  frames_redacted: number;
  frames_dropped: number;
}

interface PauseSnapshot {
  reason: "operator" | "session_locked" | "system_suspending";
  seconds_remaining: number | null;
}

interface HealthDump {
  app_state: AppPhase;
  sources: SourceReport[];
  frame_rate: number | null;
  segment_dir: string | null;
  segment_seconds_remaining: number | null;
  engine_ready: boolean;
  version: string;
  sync: SyncSnapshot;
  screen_encoder: EncoderHealth | null;
  exclusions: ExclusionHealth | null;
  pause: PauseSnapshot | null;
  views?: Record<string, string>;
}

interface StorageInfo {
  root: string;
  bytes: number | null;
}

// ── Capture exclusions (observer-exclusion) ──────────────────────────────────
// The owner's privacy controls. Rules are loaded once and edited in place; each
// edit calls `set_exclusions` (effective on the next captured frame, persisted)
// and re-renders. Exclusion *activity* is read from the health dump so the
// owner can see exclusions working — it is never silent.

interface ExclusionRules {
  excluded_exes: string[];
  title_patterns: string[];
  exclude_private_browsing: boolean;
}

interface RunningApp {
  exe_name: string;
  display_name: string;
}

const SELF_EXE = "solstone-windows-app.exe";

// ── Updater (observer-update) ────────────────────────────────────────────────
// Update state arrives on its own `update://changed` event, separate from the
// health stream, and is rendered into the Updates section honestly: every control
// is enabled only when its `actions` flag is true (no dead buttons), and the
// state text never claims "up to date" without an earned up-to-date result.

type UpdateDisplayKind =
  | "never_checked"
  | "up_to_date"
  | "checking"
  | "available"
  | "downloading"
  | "staged"
  | "failed"
  | "failed_with_available"
  | "unavailable";
type UpdateActivityKind = "idle" | "checking" | "downloading" | "installing";
type CheckIntervalKind = "day" | "week" | "month";

interface UpdatePrefs {
  auto_check: boolean;
  interval: CheckIntervalKind;
  auto_download: boolean;
}

interface UpdateActions {
  can_check_now: boolean;
  can_cancel: boolean;
  can_download: boolean;
  can_install: boolean;
  can_retry: boolean;
  can_dismiss: boolean;
  frequency_enabled: boolean;
}

interface UpdateView {
  display: UpdateDisplayKind;
  activity: UpdateActivityKind;
  last_checked_at: number | null;
  available_version: string | null;
  notes: string | null;
  download_pct: number | null;
  prefs: UpdatePrefs;
  actions: UpdateActions;
}

// Latest snapshots; the settings view renders from both. Held in module vars so a
// re-render from either stream never loses the other's state.
let latestHealth: HealthDump | null = null;
let latestStorage: StorageInfo | null = null;
let latestUpdate: UpdateView | null = null;
let activeRoute: Route = "home";
let renderBeaconFired = false;
let focusPaneTitleOnRender = false;
// Set when a background event (health/update stream) wants a full rerender but an
// interactive control is active; the next flush or any direct rerender consumes it,
// coalescing many deferred events into a single repaint.
let pendingRerender = false;
// The last-checked line node, refreshed each render; a 1s interval rewrites its
// text so the relative clock ticks live — the JS analog of the macOS TimelineView.
let lastCheckedEl: HTMLElement | null = null;

// The pair-link the owner typed/pasted. Held in a module var (never read back
// from the DOM) so a health re-render never loses it.
let pairingDraft = "";
let pairingBusy = false;

// Capture-exclusion rules + the running-app picker list. Held in module vars so a
// 1s health re-render repaints the section without losing edits; `titleDraft`
// preserves a half-typed title keyword across re-renders (like pairingDraft).
let latestExclusions: ExclusionRules | null = null;
let latestHotkey: HotkeyView | null = null;
// True while the press-to-capture field is listening for the owner's next combo.
// Held in a module var so a 1s health re-render keeps the capturing state; the
// keydown listener lives on the window, added once when capture starts.
let hotkeyCapturing = false;
let latestMic: MicView | null = null;
let micDevices: MicDeviceRef[] = [];
let latestRetention: RetentionConfig | null = null;
let runningApps: RunningApp[] = [];
let titleDraft = "";

const ids = automationContract.automation_ids;
const queriedRoot = document.querySelector<HTMLDivElement>("#app");

if (!queriedRoot) {
  throw new Error("missing app root");
}

const root: HTMLDivElement = queriedRoot;

const ROUTES: ReadonlyArray<{ route: Route; label: string; glyph: string }> = [
  { route: "home", label: "home", glyph: "\uE80F" },
  { route: "sources", label: "sources", glyph: "\uE8B3" },
  { route: "privacy", label: "privacy", glyph: "\uEA18" },
  { route: "journal", label: "journal", glyph: "\uE753" },
  { route: "shortcut", label: "shortcut", glyph: "\uE765" },
  { route: "storage", label: "storage", glyph: "\uE8B7" },
  { route: "updates", label: "updates", glyph: "\uE777" },
];

const nativeFeelStyle = document.createElement("style");
nativeFeelStyle.textContent = `
:root {
  color-scheme: light dark;
  --fg: rgba(0,0,0,0.886);
  --fg-subtle: rgba(0,0,0,0.60);
  --muted: rgba(0,0,0,0.45);
  --bg: rgba(249,249,249,0.80);
  --bg-input: #ffffff;
  --border: rgba(0,0,0,0.0803);
  --border-subtle: rgba(0,0,0,0.06);
  --accent: #0067c0;
  --accent-fg: #ffffff;
  --accent-busy: rgba(0,103,192,0.55);
  --accent-subtle: rgba(0,103,192,0.10);
  --danger: #c42b1c;
  --fill: rgba(0,0,0,0.045);
  --radius-control: 4px;
  --radius-card: 8px;
  --dur-fast: 83ms;
  --ease-standard: cubic-bezier(0,0,0,1);
  --overlay-hover: rgba(0,0,0,0.037);
	  --overlay-pressed: rgba(0,0,0,0.024);
	  --accent-overlay-hover: rgba(255,255,255,0.10);
	  --accent-overlay-pressed: rgba(0,0,0,0.06);
	  --rail-w: 200px;
	}

@media (prefers-color-scheme: dark) {
  :root {
    --fg: #ffffff;
    --fg-subtle: rgba(255,255,255,0.606);
    --muted: rgba(255,255,255,0.50);
    --bg: rgba(32,32,32,0.80);
    --bg-input: #2b2b2b;
    --border: rgba(255,255,255,0.10);
    --border-subtle: rgba(255,255,255,0.07);
    --accent: #60cdff;
    --accent-fg: #000000;
    --accent-busy: rgba(96,205,255,0.45);
    --accent-subtle: rgba(96,205,255,0.14);
    --danger: #ff99a4;
    --fill: rgba(255,255,255,0.065);
    --radius-control: 4px;
    --radius-card: 8px;
    --overlay-hover: rgba(255,255,255,0.045);
    --overlay-pressed: rgba(255,255,255,0.030);
  }
}

@supports (color: AccentColor) {
  :root {
    --accent: AccentColor;
    --accent-fg: AccentColorText;
  }
}

html,
body {
  margin: 0;
  height: 100%;
  overflow: hidden;
  overscroll-behavior: none;
  background: transparent;
}

body {
  font-family: "Segoe UI Variable Text", "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
  color: var(--fg);
  user-select: none;
  -webkit-user-select: none;
  cursor: default;
}

#app {
  height: 100vh;
  min-height: 100vh;
  overflow-y: auto;
  overscroll-behavior: none;
  background: var(--bg);
  color: var(--fg);
  scrollbar-width: thin;
}

button,
input,
textarea,
select {
  font: inherit;
}

input[type="checkbox"] {
  accent-color: var(--accent);
}

input,
textarea {
  user-select: text;
  -webkit-user-select: text;
  cursor: text;
}

.fluent-control:not(:disabled):hover {
  box-shadow: inset 0 0 0 999px var(--overlay-hover);
}

.fluent-control:not(:disabled):active {
  box-shadow: inset 0 0 0 999px var(--overlay-pressed);
}

.fluent-accent:not(:disabled):hover {
  box-shadow: inset 0 0 0 999px var(--accent-overlay-hover);
}

.fluent-accent:not(:disabled):active {
  box-shadow: inset 0 0 0 999px var(--accent-overlay-pressed);
}

button:focus-visible,
input:focus-visible,
select:focus-visible,
a:focus-visible,
[tabindex]:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

::selection {
  background: var(--accent);
  color: var(--accent-fg);
}

.selectable {
  user-select: text;
  -webkit-user-select: text;
  cursor: text;
}

	.scroll-surface {
	  scrollbar-width: thin;
	}

	.settings-shell {
	  position: relative;
	  display: grid;
	  grid-template-columns: var(--rail-w) minmax(0, 1fr);
	  height: 100%;
	  min-height: 0;
	  overflow: hidden;
	}

	.settings-scrim {
	  display: none;
	}

	.settings-rail {
	  min-height: 0;
	  overflow-y: auto;
	  border-right: 1px solid var(--border);
	  background: color-mix(in srgb, var(--bg) 92%, transparent);
	  padding: 14px 10px;
	  box-sizing: border-box;
	  scrollbar-width: thin;
	}

	.settings-rail-title {
	  padding: 4px 10px 14px;
	  font-size: 18px;
	  font-weight: 700;
	}

	.settings-nav-item {
	  position: relative;
	  display: flex;
	  align-items: center;
	  gap: 10px;
	  width: 100%;
	  min-height: 36px;
	  padding: 8px 10px;
	  border: 1px solid transparent;
	  border-radius: var(--radius-control);
	  background: transparent;
	  color: var(--fg);
	  font-size: 13px;
	  text-align: left;
	  cursor: pointer;
	}

	.settings-nav-item[aria-current="page"] {
	  background: var(--accent-subtle);
	}

	.settings-nav-item[aria-current="page"]::before {
	  content: "";
	  position: absolute;
	  left: 0;
	  top: 7px;
	  bottom: 7px;
	  width: 3px;
	  border-radius: 0 2px 2px 0;
	  background: var(--accent);
	}

	.settings-nav-glyph,
	.settings-hamburger-glyph {
	  font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets", sans-serif;
	  font-size: 16px;
	  line-height: 1;
	}

	.settings-nav-glyph {
	  width: 20px;
	  flex: 0 0 20px;
	  text-align: center;
	}

	.settings-pane {
	  min-width: 0;
	  height: 100%;
	  overflow-y: auto;
	  scrollbar-width: thin;
	}

	.settings-pane-frame {
	  box-sizing: border-box;
	  width: 100%;
	  max-width: 720px;
	  margin: 0 auto;
	  padding: 24px;
	}

	.settings-pane-topbar {
	  display: flex;
	  align-items: center;
	  gap: 10px;
	  margin: 0 0 14px;
	}

	.settings-pane-title {
	  margin: 0;
	  font-size: 22px;
	  font-weight: 700;
	  line-height: 1.25;
	}

	.settings-hamburger {
	  display: none;
	  align-items: center;
	  justify-content: center;
	  width: 34px;
	  height: 34px;
	  padding: 0;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-control);
	  background: var(--fill);
	  color: var(--fg);
	  cursor: pointer;
	}

	.settings-route-content {
	  min-width: 0;
	}

	.settings-home {
	  display: grid;
	  gap: 16px;
	}

	.settings-status-strip {
	  display: grid;
	  gap: 8px;
	  padding: 14px 16px;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-card);
	  background: var(--bg-input);
	}

	.settings-status-line,
	.settings-status-button {
	  display: grid;
	  grid-template-columns: 116px minmax(0, 1fr);
	  gap: 12px;
	  align-items: center;
	  min-height: 30px;
	  font-size: 13px;
	}

	.settings-status-button {
	  width: 100%;
	  border: 1px solid transparent;
	  border-radius: var(--radius-control);
	  background: transparent;
	  color: var(--fg);
	  text-align: left;
	  cursor: pointer;
	}

	.settings-status-label {
	  color: var(--fg-subtle);
	}

	.settings-status-value {
	  min-width: 0;
	  overflow-wrap: anywhere;
	}

	.settings-card-grid {
	  display: grid;
	  grid-template-columns: repeat(2, minmax(0, 1fr));
	  gap: 12px;
	}

	.settings-card {
	  display: grid;
	  gap: 12px;
	  align-content: start;
	  min-width: 0;
	  padding: 15px;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-card);
	  background: var(--fill);
	}

	.settings-card-title {
	  margin: 0;
	  font-size: 12px;
	  font-weight: 600;
	  color: var(--fg-subtle);
	}

	.settings-card-glance {
	  min-width: 0;
	  font-size: 13px;
	  overflow-wrap: anywhere;
	}

	.settings-card-action {
	  align-self: end;
	}

	@media (max-width: 719px) {
	  .settings-shell {
	    grid-template-columns: minmax(0, 1fr);
	  }

	  .settings-pane-frame {
	    padding: 12px;
	  }

	  .settings-hamburger {
	    display: inline-flex;
	    flex: 0 0 auto;
	  }

	  .settings-rail {
	    position: absolute;
	    inset: 0 auto 0 0;
	    z-index: 3;
	    width: min(280px, calc(100% - 48px));
	    visibility: hidden;
	    transform: translateX(-100%);
	    transition: transform var(--dur-fast) var(--ease-standard), visibility var(--dur-fast) var(--ease-standard);
	  }

	  .settings-shell[data-pane-open="true"] .settings-rail {
	    visibility: visible;
	    transform: translateX(0);
	  }

	  .settings-scrim {
	    display: block;
	    position: absolute;
	    inset: 0;
	    z-index: 2;
	    background: rgba(0,0,0,0.32);
	    opacity: 0;
	    visibility: hidden;
	    transition: opacity var(--dur-fast) var(--ease-standard), visibility var(--dur-fast) var(--ease-standard);
	  }

	  .settings-shell[data-pane-open="true"] .settings-scrim {
	    opacity: 1;
	    visibility: visible;
	  }

	  .settings-card-grid {
	    grid-template-columns: minmax(0, 1fr);
	  }

	  .settings-status-line,
	  .settings-status-button {
	    grid-template-columns: 92px minmax(0, 1fr);
	  }
	}

	@media (prefers-reduced-motion: no-preference) {
	  .fluent-control,
	  .fluent-accent,
	  .settings-nav-item,
	  .settings-status-button,
	  .settings-hamburger {
	    transition: box-shadow var(--dur-fast) var(--ease-standard);
	  }

  @keyframes update-indeterminate {
    0% { left: -35%; }
    100% { left: 100%; }
  }
}
`;
document.head.append(nativeFeelStyle);

function isTextEntryTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
}

// True while the owner is mid-interaction with a control a full rerender would
// disrupt: an open native <select> popup (the picker / retention / frequency
// dropdowns), in-progress text entry, or hotkey capture. Distinct from
// isTextEntryTarget — it reads document.activeElement and includes <select>.
function isInteractiveControlActive(): boolean {
  const active = document.activeElement;
  return (
    active instanceof HTMLSelectElement ||
    active instanceof HTMLInputElement ||
    active instanceof HTMLTextAreaElement ||
    hotkeyCapturing
  );
}

document.addEventListener("contextmenu", (event) => {
  if (isTextEntryTarget(event.target)) {
    return;
  }
  event.preventDefault();
});

document.addEventListener("keydown", (event) => {
  const key = event.key;
  const lowerKey = key.toLowerCase();
  const ctrlOrMeta = event.ctrlKey || event.metaKey;
  const zoomKey = key === "+" || key === "=" || key === "-" || key === "_" || key === "0";
  const blocked =
    key === "F5" ||
    key === "F3" ||
    key === "F7" ||
    (ctrlOrMeta &&
      (lowerKey === "r" ||
        lowerKey === "p" ||
        lowerKey === "f" ||
        zoomKey));

  if (blocked) {
    event.preventDefault();
  }
});

document.addEventListener(
  "wheel",
  (event) => {
    if (event.ctrlKey) {
      event.preventDefault();
    }
  },
  { passive: false },
);

// Flush a deferred background rerender once interaction ends. focusout fires before
// the new focus settles, so recheck on a microtask (the blur->focusout->focus->focusin
// sequence dispatches synchronously, so document.activeElement is settled by then) —
// focus moving control->control doesn't flush; only settling onto a non-control does.
document.addEventListener(
  "focusout",
  () => {
    queueMicrotask(() => {
      if (pendingRerender && !isInteractiveControlActive()) {
        rerender();
      }
    });
  },
  true,
);

const narrowNav = window.matchMedia("(max-width: 719px)");

function settingsShell(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".settings-shell");
}

function openNavOverlay(): void {
  const shell = settingsShell();
  if (!shell) {
    return;
  }
  shell.dataset.paneOpen = "true";
  const hamburger = document.querySelector<HTMLElement>(".settings-hamburger");
  if (hamburger) {
    hamburger.setAttribute("aria-expanded", "true");
    hamburger.setAttribute("aria-label", "close navigation");
  }
  document.querySelector<HTMLElement>(".settings-rail .settings-nav-item")?.focus();
}

function closeNavOverlay(returnFocusToHamburger = false): void {
  const shell = settingsShell();
  if (!shell) {
    return;
  }
  const wasOpen = navOverlayOpen(shell);
  shell.dataset.paneOpen = "false";
  const hamburger = document.querySelector<HTMLElement>(".settings-hamburger");
  if (hamburger) {
    hamburger.setAttribute("aria-expanded", "false");
    hamburger.setAttribute("aria-label", "open navigation");
  }
  if (returnFocusToHamburger && wasOpen) {
    hamburger?.focus();
  }
}

function navOverlayOpen(shell: HTMLElement): boolean {
  return shell.dataset.paneOpen === "true";
}

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || !narrowNav.matches) {
    return;
  }
  const shell = settingsShell();
  if (shell && navOverlayOpen(shell)) {
    closeNavOverlay(true);
  }
});

document.addEventListener("click", (event) => {
  if (!narrowNav.matches) {
    return;
  }
  const shell = settingsShell();
  if (!shell || !navOverlayOpen(shell)) {
    return;
  }
  const target = event.target;
  if (!(target instanceof Node)) {
    return;
  }
  const element = target instanceof Element ? target : target.parentNode;
  if (!(element instanceof Element)) {
    return;
  }
  if (element.closest(".settings-rail") || element.closest(".settings-hamburger")) {
    return;
  }
  closeNavOverlay(true);
});

const label = getCurrentWindow().label;

function text(tag: keyof HTMLElementTagNameMap, value: string): HTMLElement {
  const node = document.createElement(tag);
  node.textContent = value;
  return node;
}

function automation(node: HTMLElement, id: string): HTMLElement {
  node.dataset.automationId = id;
  return node;
}

function selectable(node: HTMLElement): HTMLElement {
  node.classList.add("selectable");
  return node;
}

function section(title: string): HTMLElement {
  const node = document.createElement("section");
  node.style.padding = "18px 20px";
  node.style.borderBottom = "1px solid var(--border)";

  const heading = text("h2", title);
  heading.style.margin = "0 0 12px";
  heading.style.fontSize = "13px";
  heading.style.fontWeight = "700";
  heading.style.textTransform = "uppercase";
  heading.style.letterSpacing = "0";
  heading.style.color = "var(--fg-subtle)";
  node.append(heading);

  return node;
}

function valueRow(labelText: string, value: HTMLElement): HTMLDivElement {
  const row = document.createElement("div");
  row.style.display = "grid";
  row.style.gridTemplateColumns = "132px minmax(0, 1fr)";
  row.style.gap = "12px";
  row.style.alignItems = "start";
  row.style.minHeight = "30px";
  row.style.padding = "7px 0";

  const labelNode = text("div", labelText);
  labelNode.style.color = "var(--fg-subtle)";
  labelNode.style.fontSize = "13px";
  value.style.fontSize = "13px";
  value.style.overflowWrap = "anywhere";
  row.append(labelNode, value);
  return row;
}

// ── Settings experience vocabulary ───────────────────────────────────────────
// The within-section building blocks the Updates pass established (orienting
// caption → subhead → helper caption → trust footnote) so every Settings
// section reads as a status/control surface, not a flat label:value table.

// An orienting line under a section heading or a subhead (the macOS GroupBox caption).
function helpCaption(value: string): HTMLElement {
  const d = text("div", value);
  d.style.color = "var(--fg-subtle)";
  d.style.fontSize = "12px";
  d.style.lineHeight = "1.45";
  d.style.margin = "0 0 8px";
  return d;
}

// A group label within a section (e.g. "device priority", "your shortcut").
function subheadLabel(value: string): HTMLElement {
  const d = text("div", value);
  d.style.fontSize = "12px";
  d.style.fontWeight = "600";
  d.style.color = "var(--fg-subtle)";
  d.style.margin = "14px 0 4px";
  return d;
}

// A trailing caption beneath a control (the macOS "changes take effect…" line).
function microCaption(value: string): HTMLElement {
  const d = text("div", value);
  d.style.color = "var(--fg-subtle)";
  d.style.fontSize = "12px";
  d.style.lineHeight = "1.4";
  d.style.margin = "4px 0 0";
  return d;
}

// A bordered-top trust line at the foot of a section (the Updates privacy-footnote
// register), for the load-bearing covenant copy a trust surface must land.
function trustFootnote(value: string): HTMLElement {
  const foot = document.createElement("div");
  foot.style.marginTop = "16px";
  foot.style.paddingTop = "12px";
  foot.style.borderTop = "1px solid var(--border-subtle)";
  const ft = text("div", value);
  ft.style.fontSize = "12px";
  ft.style.color = "var(--fg-subtle)";
  ft.style.lineHeight = "1.45";
  foot.append(ft);
  return foot;
}

function pill(label: string, severity: Severity): HTMLElement {
  const colors: Record<Severity, { text: string; bg: string; border: string }> = {
    ok: {
      text: "var(--accent)",
      bg: "var(--accent-subtle)",
      border: "1px solid var(--accent)",
    },
    neutral: {
      text: "var(--fg-subtle)",
      bg: "var(--fill)",
      border: "1px solid var(--border)",
    },
    attention: {
      text: "var(--danger)",
      bg: "var(--fill)",
      border: "1px solid var(--danger)",
    },
  };

  const chip = document.createElement("span");
  const color = colors[severity];
  chip.style.display = "inline-flex";
  chip.style.alignItems = "center";
  chip.style.width = "fit-content";
  chip.style.maxWidth = "100%";
  chip.style.boxSizing = "border-box";
  chip.style.fontSize = "12px";
  chip.style.fontWeight = "500";
  chip.style.lineHeight = "1.35";
  chip.style.padding = "2px 9px";
  chip.style.borderRadius = "999px";
  chip.style.color = color.text;
  chip.style.background = color.bg;
  chip.style.border = color.border;

  const value = text("span", label);
  value.style.minWidth = "0";
  value.style.overflowWrap = "anywhere";
  chip.append(value);
  return chip;
}

function phaseLabel(phase: AppPhase): string {
  switch (phase) {
    case "idle":
      return "idle";
    case "starting":
      return "starting";
    case "observing":
      return "observing";
    case "paused":
      return "paused";
    case "error":
      return "attention needed";
  }
}

function sourceStatusLabel(source: SourceReport | undefined): string {
  if (!source) {
    return "not reported";
  }

  switch (source.status) {
    case "active":
      return "active";
    case "inactive":
      return "inactive";
    case "no_input_device":
      return source.kind === "mic" ? "no microphone input device" : "no input device";
    case "faulted":
      return source.detail ? `attention needed: ${source.detail}` : "attention needed";
  }
}

function severityForSource(source: SourceReport | undefined): Severity {
  if (!source) {
    return "neutral";
  }

  switch (source.status) {
    case "active":
      return "ok";
    case "faulted":
      return "attention";
    case "inactive":
    case "no_input_device":
      return "neutral";
  }
}

function sourcePill(source: SourceReport | undefined): HTMLElement {
  return pill(sourceStatusLabel(source), severityForSource(source));
}

function sourceByKind(dump: HealthDump, kind: SourceKind): SourceReport | undefined {
  return dump.sources.find((source) => source.kind === kind);
}

function pairingPhaseLabel(pairing: PairingState): string {
  switch (pairing.phase) {
    case "not_paired":
      return "not paired";
    case "pairing":
      return "pairing…";
    case "paired":
      return pairing.journal_label ? `paired with ${pairing.journal_label}` : "paired";
    case "failed":
      return pairing.detail ? `pairing failed: ${pairing.detail}` : "pairing failed";
  }
}

function uploadLabel(upload: UploadStatus): string {
  const parts = [
    `${upload.uploaded_segments} delivered`,
    `${upload.pending_segments} pending`,
  ];
  if (upload.failed_segments > 0) {
    parts.push(`${upload.failed_segments} retrying`);
  }
  if (upload.last_error) {
    parts.push(`last error: ${upload.last_error}`);
  }
  return parts.join(" · ");
}

function formatStorageBytes(bytes: number): string {
  const units = ["bytes", "kb", "mb", "gb", "tb"];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rendered = unit === 0 ? String(Math.floor(value)) : value.toFixed(1);
  return `${rendered} ${units[unit]}`;
}

function storageRow(storage: StorageInfo | null): HTMLElement {
  const value = document.createElement("div");
  value.style.display = "grid";
  value.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  value.style.gap = "8px";
  value.style.alignItems = "start";

  const pathWrap = document.createElement("div");
  pathWrap.style.minWidth = "0";
  pathWrap.append(
    selectable(
      automation(
        text("div", storage ? storage.root : "not available"),
        ids["settings.status.segmentDir"],
      ),
    ),
  );
  if (storage && storage.bytes !== null) {
    pathWrap.append(microCaption(`${formatStorageBytes(storage.bytes)} stored locally`));
  }

  value.append(
    pathWrap,
    actionButton("open folder", undefined, true, () => void invoke("open_storage_folder")),
  );
  return valueRow("stored on this pc", value);
}

function syncRow(sync: SyncSnapshot): HTMLElement {
  let label: string;
  switch (sync.pairing.phase) {
    case "not_paired":
      label = "not paired — pair to sync your journal";
      break;
    case "pairing":
    case "failed":
      label = pairingPhaseLabel(sync.pairing);
      break;
    case "paired":
      label = uploadLabel(sync.upload);
      break;
  }

  return valueRow(
    "journal sync",
    selectable(automation(text("div", label), ids["settings.status.upload.state"])),
  );
}

function renderPairingSection(dump: HealthDump): HTMLElement {
  const pairing = dump.sync.pairing;
  const pane = section("Pairing");
  pane.append(
    valueRow(
      "status",
      selectable(automation(text("div", pairingPhaseLabel(pairing)), ids["settings.pairing.state"])),
    ),
    valueRow(
      "journal",
      automation(
        text("div", pairing.journal_label ?? "not paired"),
        ids["settings.pairing.journal"],
      ),
    ),
  );

  const inputRow = document.createElement("div");
  inputRow.style.display = "grid";
  inputRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  inputRow.style.gap = "8px";
  inputRow.style.padding = "7px 0";

  const input = document.createElement("input");
  input.type = "text";
  input.placeholder = "paste a pair-link from your journal";
  input.value = pairingDraft;
  input.dataset.automationId = ids["settings.pairing.input"];
  input.setAttribute("aria-label", "pair-link from your journal");
  input.classList.add("fluent-control");
  input.style.fontSize = "13px";
  input.style.padding = "7px 9px";
  input.style.border = "1px solid var(--border)";
  input.style.borderRadius = "var(--radius-control)";
  input.style.background = "var(--bg-input)";
  input.style.color = "var(--fg)";
  input.style.minWidth = "0";
  input.oninput = () => {
    pairingDraft = input.value;
  };

  const button = document.createElement("button");
  const busy = pairingBusy || pairing.phase === "pairing";
  button.textContent = busy ? "pairing…" : "pair";
  button.disabled = busy;
  button.dataset.automationId = ids["settings.pairing.submit"];
  button.classList.add("fluent-accent");
  button.style.fontSize = "13px";
  button.style.padding = "7px 14px";
  button.style.border = "1px solid var(--accent)";
  button.style.borderRadius = "var(--radius-control)";
  button.style.background = busy ? "var(--accent-busy)" : "var(--accent)";
  button.style.color = "var(--accent-fg)";
  button.style.cursor = busy ? "default" : "pointer";
  button.onclick = async () => {
    const link = pairingDraft.trim();
    if (!link || pairingBusy) {
      return;
    }
    pairingBusy = true;
    button.disabled = true;
    button.textContent = "pairing…";
    try {
      // The result is reflected through the health dump's pairing phase; we
      // ignore the return and let the next render paint the outcome.
      await invoke("pair", { link });
    } catch {
      // Failure surfaces as pairing.phase = "failed" with a detail.
    } finally {
      pairingBusy = false;
    }
  };

  inputRow.append(input, button);
  pane.append(inputRow);
  return pane;
}

async function applyExclusions(next: ExclusionRules): Promise<void> {
  latestExclusions = next;
  rerender();
  try {
    await invoke("set_exclusions", { rules: next });
  } catch {
    // Persistence failures are logged backend-side; the in-memory rules already
    // took effect. The next get on restart reflects the last persisted state.
  }
}

function exclusionActivityLabel(health: ExclusionHealth | null): string {
  if (!health || !health.rules_active) {
    return "no exclusions active";
  }
  const kept = `${health.frames_redacted} frame${health.frames_redacted === 1 ? "" : "s"} kept out of your journal this session`;
  if (health.frames_dropped > 0) {
    return `${kept} · ${health.frames_dropped} dropped`;
  }
  return kept;
}

// A removable list of string values (excluded exes / title keywords).
function removableList(
  values: string[],
  listAutomationId: string,
  onRemove: (value: string) => void,
  emptyText = "none yet",
): HTMLElement {
  const list = automation(document.createElement("div"), listAutomationId);
  list.style.display = "flex";
  list.style.flexWrap = "wrap";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (values.length === 0) {
    const empty = text("div", emptyText);
    empty.style.color = "var(--muted)";
    empty.style.fontSize = "13px";
    list.append(empty);
    return list;
  }
  for (const value of values) {
    const chip = document.createElement("span");
    chip.style.display = "inline-flex";
    chip.style.alignItems = "center";
    chip.style.gap = "6px";
    chip.style.fontSize = "13px";
    chip.style.padding = "3px 4px 3px 9px";
    chip.style.border = "1px solid var(--border)";
    chip.style.borderRadius = "var(--radius-control)";
    chip.style.background = "var(--fill)";
    chip.append(text("span", value));

    const remove = document.createElement("button");
    remove.textContent = "×";
    remove.setAttribute("aria-label", `remove ${value}`);
    remove.style.border = "none";
    remove.style.background = "transparent";
    remove.style.color = "var(--fg-subtle)";
    remove.style.cursor = "pointer";
    remove.style.fontSize = "15px";
    remove.style.lineHeight = "1";
    remove.style.padding = "0 2px";
    remove.onclick = () => onRemove(value);
    chip.append(remove);
    list.append(chip);
  }
  return list;
}

function renderExclusionsSection(rules: ExclusionRules, dump: HealthDump): HTMLElement {
  const pane = section("Privacy");
  pane.append(
    helpCaption("choose what solstone keeps out of your journal. changes take effect right away."),
  );

  // Private browsing — title-heuristic auto-exclude, on by default. The honest
  // caveat (a title heuristic, not a structural exclude) is stated, not implied.
  pane.append(
    toggleRow(
      "keep private browsing windows out",
      ids["settings.exclusions.privateBrowsing"],
      rules.exclude_private_browsing,
      (on) => {
        void applyExclusions({ ...rules, exclude_private_browsing: on });
      },
    ),
  );
  pane.append(
    microCaption(
      "solstone recognizes private and incognito windows by their title — it catches the major browsers in their default private mode.",
    ),
  );

  // Excluded apps — pick from the live running-app list (robust process identity).
  pane.append(subheadLabel("excluded apps"));
  pane.append(helpCaption("windows from these apps never reach your journal."));
  const appPickRow = document.createElement("div");
  appPickRow.style.display = "grid";
  appPickRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  appPickRow.style.gap = "8px";
  appPickRow.style.padding = "7px 0";

  const appSelect = document.createElement("select");
  appSelect.dataset.automationId = ids["settings.exclusions.appInput"];
  appSelect.setAttribute("aria-label", "choose an app to exclude");
  appSelect.classList.add("fluent-control");
  appSelect.style.fontSize = "13px";
  appSelect.style.padding = "7px 9px";
  appSelect.style.border = "1px solid var(--border)";
  appSelect.style.borderRadius = "var(--radius-control)";
  appSelect.style.minWidth = "0";
  const choices = runningApps.filter(
    (app) =>
      app.exe_name.toLowerCase() !== SELF_EXE &&
      !rules.excluded_exes.includes(app.exe_name),
  );
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = choices.length > 0 ? "choose a running app…" : "no other apps running";
  appSelect.append(placeholder);
  for (const app of choices) {
    const opt = document.createElement("option");
    opt.value = app.exe_name;
    opt.textContent = app.display_name || app.exe_name;
    opt.title = app.exe_name;
    appSelect.append(opt);
  }

  const appAdd = actionButton(
    "exclude",
    ids["settings.exclusions.appAdd"],
    choices.length > 0,
    () => {
      const exe = appSelect.value.trim().toLowerCase();
      if (!exe || rules.excluded_exes.includes(exe)) {
        return;
      }
      void applyExclusions({ ...rules, excluded_exes: [...rules.excluded_exes, exe] });
    },
  );
  appPickRow.append(appSelect, appAdd);
  pane.append(appPickRow);
  pane.append(
    removableList(
      rules.excluded_exes,
      ids["settings.exclusions.appsList"],
      (exe) => {
        void applyExclusions({
          ...rules,
          excluded_exes: rules.excluded_exes.filter((e) => e !== exe),
        });
      },
      "nothing excluded yet",
    ),
  );

  // Title keywords — case-insensitive substring of a window title.
  pane.append(subheadLabel("title keywords"));
  pane.append(helpCaption("hide any window whose title contains a word you choose."));
  const titleRow = document.createElement("div");
  titleRow.style.display = "grid";
  titleRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  titleRow.style.gap = "8px";
  titleRow.style.padding = "7px 0";

  const titleInput = document.createElement("input");
  titleInput.type = "text";
  titleInput.placeholder = "a keyword, e.g. banking";
  titleInput.value = titleDraft;
  titleInput.dataset.automationId = ids["settings.exclusions.titleInput"];
  titleInput.setAttribute("aria-label", "title keyword to exclude");
  titleInput.classList.add("fluent-control");
  titleInput.style.fontSize = "13px";
  titleInput.style.padding = "7px 9px";
  titleInput.style.border = "1px solid var(--border)";
  titleInput.style.borderRadius = "var(--radius-control)";
  titleInput.style.background = "var(--bg-input)";
  titleInput.style.color = "var(--fg)";
  titleInput.style.minWidth = "0";
  titleInput.oninput = () => {
    titleDraft = titleInput.value;
  };
  const addTitle = (): void => {
    const keyword = titleDraft.trim().toLowerCase();
    if (!keyword || rules.title_patterns.includes(keyword)) {
      return;
    }
    titleDraft = "";
    void applyExclusions({ ...rules, title_patterns: [...rules.title_patterns, keyword] });
  };
  titleInput.onkeydown = (event) => {
    if (event.key === "Enter") {
      addTitle();
    }
  };

  const titleAdd = actionButton("add", ids["settings.exclusions.titleAdd"], true, addTitle);
  titleRow.append(titleInput, titleAdd);
  pane.append(titleRow);
  pane.append(
    removableList(
      rules.title_patterns,
      ids["settings.exclusions.titlesList"],
      (keyword) => {
        void applyExclusions({
          ...rules,
          title_patterns: rules.title_patterns.filter((k) => k !== keyword),
        });
      },
      "no keywords yet",
    ),
  );

  // Exclusion activity — the never-silent surface (kept out/dropped this session),
  // landed as a labeled trust footnote: the proof the exclusions are working.
  const activityFoot = document.createElement("div");
  activityFoot.style.marginTop = "16px";
  activityFoot.style.paddingTop = "12px";
  activityFoot.style.borderTop = "1px solid var(--border-subtle)";
  const activityCap = text("div", "exclusion activity");
  activityCap.style.fontSize = "12px";
  activityCap.style.fontWeight = "600";
  activityCap.style.color = "var(--fg-subtle)";
  activityCap.style.margin = "0 0 3px";
  const activityVal = automation(
    text("div", exclusionActivityLabel(dump.exclusions)),
    ids["settings.exclusions.activity"],
  );
  activityVal.style.fontSize = "12px";
  activityVal.style.color = "var(--fg-subtle)";
  activityVal.style.lineHeight = "1.45";
  activityFoot.append(activityCap, activityVal);
  pane.append(activityFoot);

  return pane;
}

function resetRoot(rootId: string): void {
  root.replaceChildren();
  root.dataset.automationId = rootId;
  root.style.minHeight = "100vh";
  root.style.padding = "";
  root.style.overflow = "";
  root.style.boxSizing = "";
}

// Pause detail for the status line: "paused — 14 min left" while a bounded pause
// counts down, plain "paused" otherwise (indefinite, or no detail yet).
function pauseRemainingLabel(secs: number): string {
  const mins = Math.floor(secs / 60);
  if (mins <= 0) {
    return "less than a minute left";
  }
  if (mins < 60) {
    return `${mins} min left`;
  }
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  return m === 0 ? `${h} hr left` : `${h} hr ${m} min left`;
}

function statusStateLabel(dump: HealthDump): string {
  if (dump.app_state === "paused") {
    const secs = dump.pause?.seconds_remaining;
    return secs != null ? `paused — ${pauseRemainingLabel(secs)}` : "paused";
  }
  return phaseLabel(dump.app_state);
}

// ── Global pause/resume hotkey (observer-hotkey) ──────────────────────────────
// A configurable global shortcut that toggles pause/resume. The owner picks
// modifiers + a key; the backend registers it with the OS and reports the honest
// outcome — a combo another app already owns shows as taken, never a silent no-op.
interface HotkeyConfig {
  enabled: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
  vk: number;
}
type HotkeyRegistration = "inactive" | "registered" | "combo_taken" | "failed";
interface HotkeyView {
  config: HotkeyConfig;
  registration: HotkeyRegistration;
}

// ── Microphone controls (observer-mic) ───────────────────────────────────────
// Device priority + per-device disable + input gain. The owner reorders/disables
// devices and sets gain; the capture loop opens the selected device and reports
// the actually-open id back as active_id, so "active" is earned, not guessed.
interface MicDeviceRef {
  id: string;
  name: string;
}
interface MicConfig {
  priority: string[];
  disabled: string[];
  gain: number;
}
interface MicView {
  config: MicConfig;
  active_id: string | null;
}
const MIC_GAIN_LEVELS = [1, 2, 4, 8];

// ── Cache retention (observer-retention) ──────────────────────────────────────
// How long confirmed-synced local segments are kept. keep_days: 0 = don't keep
// (delete once synced), -1 = keep forever, N = keep N days then prune.
interface RetentionConfig {
  keep_days: number;
}
const RETENTION_CHOICES: ReadonlyArray<readonly [number, string]> = [
  [0, "don't keep (delete once synced)"],
  [7, "7 days"],
  [14, "14 days"],
  [30, "30 days"],
  [60, "60 days"],
  [-1, "keep forever"],
];

// VK options the owner can pick as the main key (the backend validates the combo).
const HOTKEY_KEYS: ReadonlyArray<readonly [number, string]> = (() => {
  const out: Array<[number, string]> = [];
  for (let v = 0x41; v <= 0x5a; v++) {
    out.push([v, String.fromCharCode(v)]); // A-Z
  }
  for (let v = 0x30; v <= 0x39; v++) {
    out.push([v, String.fromCharCode(v)]); // 0-9
  }
  for (let n = 1; n <= 12; n++) {
    out.push([0x70 + n - 1, `F${n}`]); // F1-F12
  }
  out.push([0x20, "Space"]);
  return out;
})();

function hotkeyHasCombo(c: HotkeyConfig): boolean {
  return c.vk !== 0 && (c.ctrl || c.alt || c.shift || c.win);
}

function hotkeyStatusLabel(view: HotkeyView): string {
  switch (view.registration) {
    case "registered":
      return "active";
    case "combo_taken":
      return "that combo is in use by another app — pick another";
    case "failed":
      return "couldn't register that shortcut";
    case "inactive":
      if (!hotkeyHasCombo(view.config)) {
        return "no shortcut set";
      }
      return view.config.enabled ? "starting…" : "shortcut set but turned off";
  }
}

// Honest registration status as [text, color]: semantic color where the job is
// distinguishing states (registered = active, taken/failed = a problem), AA on
// the light ground. The text is the same source of truth as hotkeyStatusLabel.
function hotkeyStatusDisplay(view: HotkeyView): [string, string] {
  const labelText = hotkeyStatusLabel(view);
  switch (view.registration) {
    case "registered":
      return [labelText, "var(--accent)"];
    case "combo_taken":
    case "failed":
      return [labelText, "var(--danger)"];
    case "inactive":
      return [labelText, "var(--fg-subtle)"];
  }
}

function vkLabel(vk: number): string {
  const found = HOTKEY_KEYS.find(([v]) => v === vk);
  return found ? found[1] : "";
}

// "Ctrl + Alt + P" from a config; "" when there's no real combo to show.
function hotkeyComboString(cfg: HotkeyConfig): string {
  const parts: string[] = [];
  if (cfg.ctrl) parts.push("Ctrl");
  if (cfg.alt) parts.push("Alt");
  if (cfg.shift) parts.push("Shift");
  if (cfg.win) parts.push("Win");
  const key = vkLabel(cfg.vk);
  if (key) parts.push(key);
  return parts.join(" + ");
}

// Map a keydown to one of the allowed virtual-key codes (A–Z, 0–9, F1–F12,
// Space) — exactly the HOTKEY_KEYS set. Returns 0 for a modifier-only or
// unsupported key, so the capture keeps waiting for a real key.
function vkFromKeydown(event: KeyboardEvent): number {
  const code = event.code;
  if (code.length === 4 && code.startsWith("Key")) {
    return code.charCodeAt(3); // "KeyA" -> 'A' = 0x41
  }
  if (code.length === 6 && code.startsWith("Digit")) {
    return 0x30 + Number(code.slice(5));
  }
  if (/^F([1-9]|1[0-2])$/.test(code)) {
    return 0x70 + Number(code.slice(1)) - 1;
  }
  if (code === "Space") {
    return 0x20;
  }
  return 0;
}

function onHotkeyCaptureKey(event: KeyboardEvent): void {
  if (!hotkeyCapturing) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  if (event.key === "Escape") {
    stopHotkeyCapture();
    return;
  }
  const vk = vkFromKeydown(event);
  if (vk === 0) {
    return; // modifier-only or unsupported — wait for a real key.
  }
  const ctrl = event.ctrlKey;
  const alt = event.altKey;
  const shift = event.shiftKey;
  const win = event.metaKey;
  if (!(ctrl || alt || shift || win)) {
    return; // a global shortcut needs at least one modifier — keep waiting.
  }
  hotkeyCapturing = false;
  window.removeEventListener("keydown", onHotkeyCaptureKey, true);
  // The backend reports the honest registration outcome (registered / taken /
  // failed); the capture only records the owner's intended combo.
  void applyHotkey({ enabled: true, ctrl, alt, shift, win, vk });
}

function startHotkeyCapture(): void {
  if (hotkeyCapturing) {
    return;
  }
  hotkeyCapturing = true;
  window.addEventListener("keydown", onHotkeyCaptureKey, true);
  rerender();
}

function stopHotkeyCapture(): void {
  if (!hotkeyCapturing) {
    return;
  }
  hotkeyCapturing = false;
  window.removeEventListener("keydown", onHotkeyCaptureKey, true);
  rerender();
}

async function applyHotkey(next: HotkeyConfig): Promise<void> {
  latestHotkey = { config: next, registration: latestHotkey?.registration ?? "inactive" };
  rerender();
  try {
    await invoke("set_hotkey", { config: next });
  } catch {
    // Persistence failures are logged backend-side; the desired config still took.
  }
  // The pump reconciles registration within a poll (~250ms); refetch the honest
  // outcome shortly after so the status reflects registered / combo-taken.
  setTimeout(() => {
    void invoke<HotkeyView>("get_hotkey")
      .then((v) => {
        latestHotkey = v;
        rerender();
      })
      .catch(() => {});
  }, 450);
}

function renderHotkeySection(view: HotkeyView): HTMLElement {
  const pane = section("Global shortcut");
  const cfg = view.config;

  pane.append(helpCaption("a global shortcut to pause and resume solstone from anywhere."));

  pane.append(
    toggleRow(
      "enable the pause / resume shortcut",
      ids["settings.hotkey.enabled"],
      cfg.enabled,
      (on) => {
        void applyHotkey({ ...cfg, enabled: on });
      },
    ),
  );

  // Press-to-capture — the native global-shortcut pattern. The owner clicks the
  // field and presses their combo; the field carries the settings.hotkey.combo
  // contract id (the combo control). Writes the same HotkeyConfig fields.
  pane.append(subheadLabel("your shortcut"));
  const combo = hotkeyComboString(cfg);
  const field = document.createElement("button");
  field.dataset.automationId = ids["settings.hotkey.combo"];
  field.classList.add("fluent-control");
  field.style.display = "flex";
  field.style.alignItems = "center";
  field.style.justifyContent = "space-between";
  field.style.gap = "10px";
  field.style.width = "100%";
  field.style.textAlign = "left";
  field.style.fontSize = "13px";
  field.style.padding = "9px 12px";
  field.style.borderRadius = "var(--radius-control)";
  field.style.border = `1px dashed ${hotkeyCapturing ? "var(--accent)" : "var(--border)"}`;
  field.style.background = hotkeyCapturing ? "var(--accent-subtle)" : "var(--bg-input)";
  field.style.color = "var(--fg)";
  field.style.cursor = "pointer";
  if (hotkeyCapturing) {
    const lead = text("span", "press your shortcut…");
    lead.style.color = "var(--accent)";
    lead.style.fontWeight = "600";
    const hint = text("span", "esc to cancel");
    hint.style.color = "var(--muted)";
    hint.style.fontSize = "12px";
    field.append(lead, hint);
    field.onclick = () => stopHotkeyCapture();
  } else if (combo) {
    const kbd = text("span", combo);
    kbd.style.fontFamily = 'ui-monospace, "Cascadia Code", Consolas, monospace';
    const hint = text("span", "change");
    hint.style.color = "var(--fg-subtle)";
    hint.style.fontSize = "12px";
    field.append(kbd, hint);
    field.onclick = () => startHotkeyCapture();
  } else {
    const lead = text("span", "click, then press your shortcut");
    lead.style.color = "var(--fg-subtle)";
    const hint = text("span", "set");
    hint.style.color = "var(--accent)";
    hint.style.fontSize = "12px";
    hint.style.fontWeight = "600";
    field.append(lead, hint);
    field.onclick = () => startHotkeyCapture();
  }
  pane.append(field);
  pane.append(microCaption("hold one or more of Ctrl, Alt, Shift, or Win and press a key."));

  const [statusText, statusColor] = hotkeyStatusDisplay(view);
  const statusEl = automation(text("div", statusText), ids["settings.hotkey.status"]);
  statusEl.style.color = statusColor;
  pane.append(valueRow("status", statusEl));

  // Clear — a quiet text button, present only when a combo is set.
  if (hotkeyHasCombo(cfg)) {
    const clear = document.createElement("button");
    clear.textContent = "clear shortcut";
    clear.dataset.automationId = ids["settings.hotkey.clear"];
    clear.style.border = "none";
    clear.style.background = "transparent";
    clear.style.color = "var(--fg-subtle)";
    clear.style.cursor = "pointer";
    clear.style.fontSize = "12px";
    clear.style.padding = "6px 0";
    clear.style.textDecoration = "underline";
    clear.onclick = () => {
      stopHotkeyCapture();
      void applyHotkey({
        enabled: false,
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        vk: 0,
      });
    };
    const clearWrap = document.createElement("div");
    clearWrap.append(clear);
    pane.append(clearWrap);
  }

  return pane;
}

// Display order: priority ids first (those present), then remaining present
// devices in enumeration order. Reorder/disable operate on this list.
function orderedMicDevices(cfg: MicConfig, devices: MicDeviceRef[]): MicDeviceRef[] {
  const byId = new Map(devices.map((d) => [d.id, d]));
  const out: MicDeviceRef[] = [];
  const seen = new Set<string>();
  for (const id of cfg.priority) {
    const d = byId.get(id);
    if (d && !seen.has(id)) {
      out.push(d);
      seen.add(id);
    }
  }
  for (const d of devices) {
    if (!seen.has(d.id)) {
      out.push(d);
      seen.add(d.id);
    }
  }
  return out;
}

async function applyMic(next: MicConfig): Promise<void> {
  latestMic = { config: next, active_id: latestMic?.active_id ?? null };
  rerender();
  try {
    await invoke("set_mic_config", { config: next });
  } catch {
    // Persistence failures are logged backend-side; the desired config still took.
  }
  // The capture loop reconciles selection within ~1s; refetch the active device.
  setTimeout(() => {
    void invoke<MicView>("get_mic_config")
      .then((v) => {
        latestMic = v;
        rerender();
      })
      .catch(() => {});
  }, 1200);
}

function reorderButton(
  glyph: string,
  enabled: boolean,
  ariaLabel: string,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = glyph;
  b.disabled = !enabled;
  b.setAttribute("aria-label", ariaLabel);
  b.classList.add("fluent-control");
  b.style.fontSize = "13px";
  b.style.lineHeight = "1";
  b.style.padding = "3px 7px";
  b.style.border = "1px solid var(--border)";
  b.style.borderRadius = "var(--radius-control)";
  b.style.background = "var(--fill)";
  b.style.color = enabled ? "var(--fg-subtle)" : "var(--muted)";
  b.style.cursor = enabled ? "pointer" : "default";
  if (enabled) {
    b.onclick = onClick;
  }
  return b;
}

function renderMicSection(view: MicView): HTMLElement {
  const pane = section("Microphones");
  const cfg = view.config;
  const ordered = orderedMicDevices(cfg, micDevices);

  pane.append(
    helpCaption(
      "solstone observes through one microphone at a time. set which one, and how much to boost it.",
    ),
  );

  // Device priority first (macOS-sibling order) — reorder + enable/disable.
  pane.append(subheadLabel("device priority"));
  pane.append(
    helpCaption(
      "the top enabled microphone is used. use ↑ ↓ to set the order; solstone falls back to the next if one is unavailable.",
    ),
  );
  const list = automation(document.createElement("div"), ids["settings.mic.devices"]);
  list.style.display = "flex";
  list.style.flexDirection = "column";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (ordered.length === 0) {
    const empty = text("div", "no microphone input devices");
    empty.style.color = "var(--muted)";
    empty.style.fontSize = "13px";
    list.append(empty);
  }
  ordered.forEach((d, idx) => {
    const disabled = cfg.disabled.includes(d.id);
    const row = document.createElement("div");
    row.style.display = "flex";
    row.style.alignItems = "center";
    row.style.gap = "8px";
    row.style.fontSize = "13px";

    const orderIds = ordered.map((x) => x.id);
    row.append(
      reorderButton("↑", idx > 0, `move ${d.name} up`, () => {
        const o = [...orderIds];
        [o[idx - 1], o[idx]] = [o[idx], o[idx - 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );
    row.append(
      reorderButton("↓", idx < ordered.length - 1, `move ${d.name} down`, () => {
        const o = [...orderIds];
        [o[idx + 1], o[idx]] = [o[idx], o[idx + 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );

    const label = text("span", d.name);
    label.style.flex = "1";
    if (disabled) {
      label.style.textDecoration = "line-through";
      label.style.color = "var(--muted)";
    }
    if (d.id === view.active_id) {
      const badge = text("span", " · active");
      badge.style.color = "var(--accent)";
      label.append(badge);
    }
    row.append(label);

    const toggle = document.createElement("button");
    toggle.textContent = disabled ? "enable" : "disable";
    toggle.classList.add("fluent-control");
    toggle.style.fontSize = "12px";
    toggle.style.padding = "4px 10px";
    toggle.style.border = "1px solid var(--border)";
    toggle.style.borderRadius = "var(--radius-control)";
    toggle.style.background = "var(--fill)";
    toggle.style.color = "var(--fg-subtle)";
    toggle.style.cursor = "pointer";
    toggle.onclick = () => {
      const next = disabled
        ? cfg.disabled.filter((x) => x !== d.id)
        : [...cfg.disabled, d.id];
      void applyMic({ ...cfg, disabled: next });
    };
    row.append(toggle);

    list.append(row);
  });
  pane.append(list);

  // Active device — earned from what the loop actually opened (never guessed).
  // Carries the settings.mic.active contract id.
  const activeName =
    ordered.find((d) => d.id === view.active_id)?.name ??
    (view.active_id ? "selected device" : "none");
  const activeRow = document.createElement("div");
  activeRow.style.display = "flex";
  activeRow.style.gap = "6px";
  activeRow.style.fontSize = "12px";
  activeRow.style.color = "var(--fg-subtle)";
  activeRow.style.margin = "6px 0 0";
  const activeVal = automation(text("span", activeName), ids["settings.mic.active"]);
  activeVal.style.color = "var(--fg)";
  activeRow.append(text("span", "active:"), activeVal);
  pane.append(activeRow);

  // Input gain — segmented 1× / 2× / 4× / 8×.
  pane.append(subheadLabel("input gain"));
  const gainRow = automation(document.createElement("div"), ids["settings.mic.gain"]);
  gainRow.setAttribute("role", "group");
  gainRow.setAttribute("aria-label", "input gain");
  gainRow.style.display = "flex";
  gainRow.style.gap = "6px";
  for (const level of MIC_GAIN_LEVELS) {
    const on = cfg.gain === level;
    const b = document.createElement("button");
    b.textContent = `${level}×`;
    b.classList.add(on ? "fluent-accent" : "fluent-control");
    b.style.fontSize = "13px";
    b.style.padding = "5px 12px";
    b.style.border = on ? "1px solid var(--accent)" : "1px solid var(--border)";
    b.style.borderRadius = "var(--radius-control)";
    b.style.background = on ? "var(--accent)" : "var(--fill)";
    b.style.color = on ? "var(--accent-fg)" : "var(--fg-subtle)";
    b.style.cursor = "pointer";
    b.onclick = () => {
      void applyMic({ ...cfg, gain: level });
    };
    gainRow.append(b);
  }
  pane.append(gainRow);
  pane.append(
    microCaption(
      "a louder input for quiet microphones — changes apply right away. a stronger boost can pick up more background noise in a quiet room.",
    ),
  );

  return pane;
}

function renderRetentionSection(cfg: RetentionConfig): HTMLElement {
  const pane = section("Local storage");
  pane.append(
    helpCaption(
      "after a segment safely reaches your journal, how long should solstone keep its local copy on this computer?",
    ),
  );
  pane.append(
    helpCaption("a segment is a 5-minute local bundle that stays here until your journal receives it."),
  );

  const sel = document.createElement("select");
  sel.dataset.automationId = ids["settings.retention"];
  sel.setAttribute("aria-label", "how long to keep local segments");
  sel.classList.add("fluent-control");
  sel.style.fontSize = "13px";
  sel.style.padding = "7px 9px";
  sel.style.border = "1px solid var(--border)";
  sel.style.borderRadius = "var(--radius-control)";
  // If the persisted value isn't one of the presets, show it as a custom option
  // so the picker reflects the real state rather than silently snapping.
  const known = RETENTION_CHOICES.some(([days]) => days === cfg.keep_days);
  const choices: ReadonlyArray<readonly [number, string]> = known
    ? RETENTION_CHOICES
    : [...RETENTION_CHOICES, [cfg.keep_days, `${cfg.keep_days} days`] as const];
  for (const [days, label] of choices) {
    const opt = document.createElement("option");
    opt.value = String(days);
    opt.textContent = label;
    if (days === cfg.keep_days) {
      opt.selected = true;
    }
    sel.append(opt);
  }
  sel.onchange = () => {
    const next: RetentionConfig = { keep_days: Number(sel.value) };
    latestRetention = next;
    void invoke("set_retention", { config: next }).catch(() => {});
  };
  pane.append(valueRow("keep segments", sel));
  pane.append(
    trustFootnote(
      "your unsynced segments are never deleted — solstone only clears local copies of segments already saved to your journal.",
    ),
  );

  return pane;
}

function routeLabel(route: Route): string {
  return ROUTES.find((item) => item.route === route)?.label ?? route;
}

function navigateTo(route: Route): void {
  activeRoute = route;
  focusPaneTitleOnRender = true;
  closeNavOverlay();
  rerender();
}

function loadingCaption(): HTMLElement {
  const cap = helpCaption("not available right now.");
  cap.style.padding = "18px 20px";
  return cap;
}

function clearLastSectionDivider(container: HTMLElement): void {
  const sections = Array.from(container.children).filter(
    (child): child is HTMLElement =>
      child instanceof HTMLElement && child.tagName.toLowerCase() === "section",
  );
  if (sections.length === 0) {
    return;
  }
  sections[sections.length - 1].style.borderBottom = "none";
}

function renderSourcesSection(dump: HealthDump): HTMLElement {
  const sources = section("Sources");
  const screen = sourceByKind(dump, "screen");
  const systemAudio = sourceByKind(dump, "system_audio");
  const mic = sourceByKind(dump, "mic");
  sources.append(
    valueRow(
      "screen",
      selectable(automation(sourcePill(screen), ids["settings.sources.screen.state"])),
    ),
    valueRow(
      "system audio",
      selectable(automation(sourcePill(systemAudio), ids["settings.sources.systemAudio.state"])),
    ),
    valueRow(
      "microphone",
      selectable(automation(sourcePill(mic), ids["settings.sources.mic.state"])),
    ),
  );
  return sources;
}

function statusSeverity(phase: AppPhase): Severity {
  switch (phase) {
    case "observing":
      return "ok";
    case "error":
      return "attention";
    case "idle":
    case "starting":
    case "paused":
      return "neutral";
  }
}

function syncSummary(sync: SyncSnapshot): string {
  switch (sync.pairing.phase) {
    case "not_paired":
      return "pair to deliver to your journal";
    case "pairing":
    case "failed":
      return pairingPhaseLabel(sync.pairing);
    case "paired": {
      const upload = sync.upload;
      if (upload.last_error || upload.failed_segments > 0) {
        return upload.failed_segments > 0
          ? `${upload.failed_segments} retrying`
          : "sync needs attention";
      }
      return `${upload.uploaded_segments} delivered · ${upload.pending_segments} pending`;
    }
  }
}

function sourceGlanceToken(source: SourceReport | undefined, isMic: boolean): string {
  if (!source) {
    return "not reported";
  }

  switch (source.status) {
    case "active":
      return "active";
    case "inactive":
      return "inactive";
    case "no_input_device":
      return isMic ? "none" : "no input device";
    case "faulted":
      return "attention needed";
  }
}

function sourcesGlance(dump: HealthDump): string {
  const screen = sourceGlanceToken(sourceByKind(dump, "screen"), false);
  const systemAudio = sourceGlanceToken(sourceByKind(dump, "system_audio"), false);
  const mic = sourceGlanceToken(sourceByKind(dump, "mic"), true);
  if (screen === "active" && systemAudio === "active") {
    return mic === "active" ? "all sources active" : `screen + system audio active · mic: ${mic}`;
  }
  return `screen: ${screen} · system audio: ${systemAudio} · mic: ${mic}`;
}

function statusLabelNode(labelText: string): HTMLElement {
  const node = text("span", labelText);
  node.classList.add("settings-status-label");
  return node;
}

function statusValueNode(value: HTMLElement): HTMLElement {
  const node = document.createElement("span");
  node.classList.add("settings-status-value");
  node.append(value);
  return node;
}

function statusTextValue(value: string): HTMLElement {
  return statusValueNode(text("span", value));
}

function statusLine(labelText: string, value: HTMLElement): HTMLElement {
  const row = document.createElement("div");
  row.classList.add("settings-status-line");
  row.append(statusLabelNode(labelText), statusValueNode(value));
  return row;
}

function statusButton(labelText: string, value: string, route: Route): HTMLButtonElement {
  const row = document.createElement("button");
  row.type = "button";
  row.classList.add("settings-status-button", "fluent-control");
  row.onclick = () => navigateTo(route);
  row.append(statusLabelNode(labelText), statusTextValue(value));
  return row;
}

function renderStatusStrip(dump: HealthDump): HTMLElement {
  const strip = document.createElement("div");
  strip.classList.add("settings-status-strip");
  strip.append(
    statusLine(
      "state",
      automation(
        pill(statusStateLabel(dump), statusSeverity(dump.app_state)),
        ids["settings.status.appState.state"],
      ),
    ),
    statusButton("journal", syncSummary(dump.sync), "journal"),
    statusButton("sources", sourcesGlance(dump), "sources"),
  );
  return strip;
}

function homeCard(titleText: string, glanceNode: HTMLElement, actionNode: HTMLElement): HTMLElement {
  const card = document.createElement("div");
  card.classList.add("settings-card");

  const title = text("h2", titleText);
  title.classList.add("settings-card-title");

  const glance = document.createElement("div");
  glance.classList.add("settings-card-glance");
  glance.append(glanceNode);

  const action = document.createElement("div");
  action.classList.add("settings-card-action");
  action.append(actionNode);

  card.append(title, glance, action);
  return card;
}

function storageGlance(): HTMLElement {
  return text("div", "stored on this pc");
}

function renderPauseCard(dump: HealthDump): HTMLElement {
  const phase = latestHealth?.app_state ?? dump.app_state;
  let action: HTMLButtonElement;
  if (phase === "observing") {
    action = actionButton("pause", undefined, true, () => {
      void invoke("pause", { reason: "operator", durationSecs: null }).then(() => retryHealth());
    });
  } else if (phase === "paused") {
    action = actionButton("resume", undefined, true, () => {
      void invoke("resume").then(() => retryHealth());
    });
  } else {
    action = actionButton("pause", undefined, false, () => {});
  }

  return homeCard(
    "pause / resume",
    pill(statusStateLabel(dump), statusSeverity(dump.app_state)),
    action,
  );
}

function renderJournalCard(dump: HealthDump): HTMLElement {
  const phase = dump.sync.pairing.phase;
  const labelText = phase === "not_paired" || phase === "failed" ? "pair" : "journal details";
  return homeCard(
    "journal",
    text("div", pairingPhaseLabel(dump.sync.pairing)),
    actionButton(labelText, undefined, true, () => navigateTo("journal")),
  );
}

function renderStorageCard(): HTMLElement {
  return homeCard(
    "storage",
    storageGlance(),
    actionButton("open folder", undefined, true, () => void invoke("open_storage_folder")),
  );
}

function renderUpdatesCard(): HTMLElement {
  return homeCard(
    "updates",
    text("div", latestUpdate ? updateHeadline(latestUpdate) : "not available right now."),
    actionButton("check now", undefined, latestUpdate !== null, () => void invoke("update_check_now")),
  );
}

function renderHome(dump: HealthDump): HTMLElement {
  const home = document.createElement("div");
  home.classList.add("settings-home");

  const cards = document.createElement("div");
  cards.classList.add("settings-card-grid");
  cards.append(
    renderPauseCard(dump),
    renderJournalCard(dump),
    renderStorageCard(),
    renderUpdatesCard(),
  );

  home.append(renderStatusStrip(dump), cards);
  return home;
}

function renderRouteContent(route: Route, dump: HealthDump): HTMLElement {
  const content = document.createElement("div");
  content.classList.add("settings-route-content");

  switch (route) {
    case "home":
      content.append(renderHome(dump));
      break;
    case "sources":
      content.append(renderSourcesSection(dump), latestMic ? renderMicSection(latestMic) : loadingCaption());
      break;
    case "privacy":
      content.append(latestExclusions ? renderExclusionsSection(latestExclusions, dump) : loadingCaption());
      break;
    case "journal": {
      const sync = section("sync");
      sync.append(syncRow(dump.sync));
      content.append(renderPairingSection(dump), sync);
      break;
    }
    case "shortcut":
      content.append(latestHotkey ? renderHotkeySection(latestHotkey) : loadingCaption());
      break;
    case "storage": {
      const location = section("storage location");
      location.append(storageRow(latestStorage));
      content.append(location, latestRetention ? renderRetentionSection(latestRetention) : loadingCaption());
      break;
    }
    case "updates":
      content.append(latestUpdate ? renderUpdatesSection(latestUpdate) : loadingCaption());
      break;
  }

  clearLastSectionDivider(content);
  return content;
}

function navButton(route: Route, labelText: string, glyph: string): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.classList.add("settings-nav-item", "fluent-control");
  if (route === activeRoute) {
    button.setAttribute("aria-current", "page");
  }

  const icon = text("span", glyph);
  icon.classList.add("settings-nav-glyph");
  icon.setAttribute("aria-hidden", "true");

  const labelNode = text("span", labelText);
  button.append(icon, labelNode);
  return button;
}

function renderRail(): HTMLElement {
  const rail = document.createElement("nav");
  rail.id = "settings-nav";
  rail.classList.add("settings-rail");
  rail.setAttribute("aria-label", "settings");

  const title = text("div", "solstone");
  title.classList.add("settings-rail-title");
  rail.append(title);

  for (const item of ROUTES) {
    const button = navButton(item.route, item.label, item.glyph);
    button.onclick = () => navigateTo(item.route);
    rail.append(button);
  }

  return rail;
}

function renderPane(dump: HealthDump): HTMLElement {
  const pane = document.createElement("main");
  pane.classList.add("settings-pane");

  const frame = document.createElement("div");
  frame.classList.add("settings-pane-frame");

  const topbar = document.createElement("div");
  topbar.classList.add("settings-pane-topbar");

  const hamburger = document.createElement("button");
  hamburger.type = "button";
  hamburger.classList.add("settings-hamburger", "fluent-control");
  hamburger.setAttribute("aria-label", "open navigation");
  hamburger.setAttribute("aria-expanded", "false");
  hamburger.setAttribute("aria-controls", "settings-nav");
  hamburger.onclick = () => {
    const shell = settingsShell();
    if (shell && navOverlayOpen(shell)) {
      closeNavOverlay();
    } else {
      openNavOverlay();
    }
  };

  const hamburgerGlyph = text("span", "\uE700");
  hamburgerGlyph.classList.add("settings-hamburger-glyph");
  hamburgerGlyph.setAttribute("aria-hidden", "true");
  hamburger.append(hamburgerGlyph);

  const title = text("h1", routeLabel(activeRoute));
  title.classList.add("settings-pane-title");
  title.tabIndex = -1;

  topbar.append(hamburger, title);
  frame.append(topbar, renderRouteContent(activeRoute, dump));
  pane.append(frame);
  return pane;
}

function renderSettingsShell(dump: HealthDump): HTMLElement {
  const shell = document.createElement("div");
  shell.classList.add("settings-shell");
  shell.dataset.paneOpen = "false";

  const scrim = document.createElement("div");
  scrim.classList.add("settings-scrim");

  shell.append(scrim, renderRail(), renderPane(dump));
  return shell;
}

function renderSettings(dump: HealthDump): void {
  resetRoot(ids["settings.window.root"]);
  root.style.padding = "0";
  root.style.overflow = "hidden";
  root.style.boxSizing = "border-box";
  root.append(renderSettingsShell(dump));
  if (focusPaneTitleOnRender) {
    focusPaneTitleOnRender = false;
    root.querySelector<HTMLElement>(".settings-pane-title")?.focus();
  }
}

function renderAbout(dump: HealthDump): void {
  resetRoot(ids["about.window.root"]);
  root.style.padding = "22px";
  root.style.boxSizing = "border-box";

  const title = text("h1", "solstone");
  title.style.margin = "0 0 12px";
  title.style.fontSize = "24px";

  const body = text("p", "observers and the owner's journal, with sol the keeper");
  body.style.margin = "0 0 18px";
  body.style.lineHeight = "1.5";
  body.style.color = "var(--fg-subtle)";

  const version = selectable(automation(text("div", dump.version), ids["about.version"]));
  version.style.fontSize = "13px";
  version.style.color = "var(--fg-subtle)";

  root.append(title, body, version);
}

function nowSecs(): number {
  return Math.floor(Date.now() / 1000);
}

// Mirror of observer_update::last_checked_relative (the Rust-tested canonical
// spec, where the <60s "just now" threshold is pinned). This ticks the live clock.
function lastCheckedRelative(checkedAt: number | null, secsNow: number): string {
  if (checkedAt == null) {
    return "never checked for updates";
  }
  const secs = Math.max(0, secsNow - checkedAt);
  if (secs < 60) {
    return "checked just now";
  }
  if (secs < 3600) {
    const m = Math.floor(secs / 60);
    return `checked ${m} minute${m === 1 ? "" : "s"} ago`;
  }
  if (secs < 86400) {
    const h = Math.floor(secs / 3600);
    return `checked ${h} hour${h === 1 ? "" : "s"} ago`;
  }
  const d = Math.floor(secs / 86400);
  return `checked ${d} day${d === 1 ? "" : "s"} ago`;
}

function updateHeadline(view: UpdateView): string {
  const v = view.available_version ?? "";
  if (view.activity === "installing") {
    return v ? `installing solstone ${v}…` : "installing…";
  }
  switch (view.display) {
    case "never_checked":
      return "not checked for updates yet";
    case "up_to_date":
      return "solstone is up to date";
    case "checking":
      return "checking for updates…";
    case "available":
      return `solstone ${v} is available`;
    case "downloading":
      return `downloading solstone ${v}`;
    case "staged":
      return `solstone ${v} is ready to install`;
    case "failed":
      return "couldn't check for updates";
    case "failed_with_available":
      return `couldn't check — solstone ${v} found earlier`;
    case "unavailable":
      return "this build can't update itself";
  }
}

// The quiet subtitle under the headline. `live` marks the last-checked clock so
// the 1s tick only rewrites those states; static subtitles never tick. Returns
// null where the headline + progress bar already say everything (checking,
// downloading).
function updateSubtitle(
  view: UpdateView,
  secsNow: number,
): { text: string; live: boolean } | null {
  if (view.activity === "installing") {
    return { text: "this only takes a moment", live: false };
  }
  switch (view.display) {
    case "never_checked":
      return {
        text: view.prefs.auto_check
          ? "automatic checks are on — solstone will check on its own"
          : "automatic checks are off",
        live: false,
      };
    case "checking":
    case "downloading":
      return null;
    case "staged":
      return { text: "it installs the next time solstone restarts", live: false };
    case "unavailable":
      return {
        text: "download the latest from solstone.app/download/windows",
        live: false,
      };
    case "up_to_date":
    case "available":
    case "failed":
    case "failed_with_available":
      return { text: lastCheckedRelative(view.last_checked_at, secsNow), live: true };
  }
}

function actionButton(
  labelText: string,
  automationId: string | undefined,
  enabled: boolean,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = labelText;
  b.disabled = !enabled;
  if (automationId) {
    b.dataset.automationId = automationId;
  }
  b.classList.add(enabled ? "fluent-accent" : "fluent-control");
  b.style.fontSize = "13px";
  b.style.padding = "6px 12px";
  b.style.border = enabled ? "1px solid var(--accent)" : "1px solid var(--border)";
  b.style.borderRadius = "var(--radius-control)";
  b.style.background = enabled ? "var(--accent)" : "var(--fill)";
  b.style.color = enabled ? "var(--accent-fg)" : "var(--muted)";
  b.style.cursor = enabled ? "pointer" : "default";
  if (enabled) {
    b.onclick = onClick;
  }
  return b;
}

function frequencyRow(interval: CheckIntervalKind, enabled: boolean): HTMLElement {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.alignItems = "center";
  row.style.gap = "10px";
  row.style.padding = "6px 0";
  row.style.marginLeft = "26px";

  const labelNode = text("div", "how often");
  labelNode.style.fontSize = "13px";
  labelNode.style.color = enabled ? "var(--fg-subtle)" : "var(--muted)";

  const sel = document.createElement("select");
  sel.disabled = !enabled;
  sel.dataset.automationId = ids["settings.updates.frequency"];
  sel.setAttribute("aria-label", "how often to check for updates");
  sel.classList.add("fluent-control");
  const options: ReadonlyArray<readonly [CheckIntervalKind, string]> = [
    ["day", "every day"],
    ["week", "every week"],
    ["month", "every month"],
  ];
  for (const [val, lbl] of options) {
    const opt = document.createElement("option");
    opt.value = val;
    opt.textContent = lbl;
    if (val === interval) {
      opt.selected = true;
    }
    sel.append(opt);
  }
  sel.style.fontSize = "13px";
  sel.style.padding = "3px 6px";
  sel.style.borderRadius = "var(--radius-control)";
  sel.onchange = () => {
    void invoke("update_set_interval", { interval: sel.value });
  };

  row.append(labelNode, sel);
  return row;
}

// A settings toggle row: checkbox adjacent to its label (the macOS Toggle idiom),
// distinct from valueRow's split label : control grid.
function toggleRow(
  labelText: string,
  automationId: string,
  checked: boolean,
  onChange: (on: boolean) => void,
): HTMLElement {
  const lab = document.createElement("label");
  lab.style.display = "flex";
  lab.style.alignItems = "center";
  lab.style.gap = "9px";
  lab.style.padding = "6px 0";
  lab.style.fontSize = "13px";
  lab.style.color = "var(--fg)";
  lab.style.cursor = "pointer";

  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = checked;
  box.dataset.automationId = automationId;
  box.style.margin = "0";
  box.onchange = () => onChange(box.checked);

  lab.append(box, text("span", labelText));
  return lab;
}

// A thin update-progress bar: determinate (download percent) or an indeterminate
// sweep while a check/install is in flight, so a wait visibly breathes.
function updateProgressBar(determinate: boolean, pct: number | null): HTMLElement {
  const wrap = document.createElement("div");
  wrap.style.display = "flex";
  wrap.style.alignItems = "center";
  wrap.style.gap = "10px";
  wrap.style.margin = "12px 0 2px";

  const track = document.createElement("div");
  track.style.position = "relative";
  track.style.flex = "1";
  track.style.height = "6px";
  track.style.borderRadius = "3px";
  track.style.background = "var(--fill)";
  track.style.overflow = "hidden";

  const fill = document.createElement("div");
  fill.style.position = "absolute";
  fill.style.top = "0";
  fill.style.bottom = "0";
  fill.style.background = "var(--accent)";
  fill.style.borderRadius = "3px";
  if (determinate) {
    fill.style.left = "0";
    fill.style.width = `${pct ?? 0}%`;
    fill.style.transition = "width .2s";
  } else {
    fill.style.width = "35%";
    fill.style.animation = "update-indeterminate 1.1s ease-in-out infinite";
  }
  track.append(fill);
  wrap.append(track);

  if (determinate) {
    const lbl = text("div", `${pct ?? 0}%`);
    lbl.style.fontSize = "12px";
    lbl.style.color = "var(--fg-subtle)";
    lbl.style.minWidth = "34px";
    lbl.style.textAlign = "right";
    wrap.append(lbl);
  }
  return wrap;
}

// Release notes from the feed, rendered as light markdown (headings + bullets)
// in a quiet card. Carries the settings.updates.notes automation id.
function updateNotesBlock(notes: string): HTMLElement {
  const wrap = document.createElement("div");
  wrap.style.margin = "14px 0 2px";

  const cap = text("div", "what's new");
  cap.style.fontSize = "12px";
  cap.style.fontWeight = "600";
  cap.style.color = "var(--fg-subtle)";
  cap.style.margin = "0 0 6px";

  const card = automation(document.createElement("div"), ids["settings.updates.notes"]);
  card.classList.add("scroll-surface");
  card.style.background = "var(--bg-input)";
  card.style.border = "1px solid var(--border-subtle)";
  card.style.borderRadius = "var(--radius-card)";
  card.style.padding = "10px 12px";
  card.style.maxHeight = "150px";
  card.style.overflowY = "auto";

  for (const raw of notes.split("\n")) {
    const line = raw.trim();
    if (!line) {
      const spacer = document.createElement("div");
      spacer.style.height = "5px";
      card.append(spacer);
      continue;
    }
    const heading = /^#{1,3}\s+(.*)$/.exec(line);
    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (heading) {
      const h = text("div", heading[1]);
      h.style.fontSize = "12.5px";
      h.style.fontWeight = "600";
      h.style.color = "var(--fg)";
      h.style.margin = "6px 0 3px";
      card.append(h);
    } else if (bullet) {
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.gap = "7px";
      row.style.fontSize = "12.5px";
      row.style.color = "var(--fg-subtle)";
      row.style.lineHeight = "1.45";
      row.style.margin = "2px 0";
      const dot = text("div", "•");
      dot.style.color = "var(--accent)";
      row.append(dot, text("div", bullet[1]));
      card.append(row);
    } else {
      const p = text("div", line);
      p.style.fontSize = "12.5px";
      p.style.color = "var(--fg-subtle)";
      p.style.lineHeight = "1.45";
      p.style.margin = "2px 0";
      card.append(p);
    }
  }
  wrap.append(cap, card, updateNotesOnlineLink());
  return wrap;
}

// The macOS "read the full notes online" affordance (UpdatesCopy.releaseNotesOnlineURL).
// Lives inside the notes block, so it renders only when the feed actually carried
// notes — the same NotesMarkdown the web page renders from the one releases.win.json
// feed — and can therefore never point at an empty release-history page. The webview
// is a sealed renderer with no navigation power: the href mirrors the destination for
// hover/a11y semantics but is never followed (a raw nav would replace the Settings
// shell); the click is handed to the backend, which opens the system browser.
function updateNotesOnlineLink(): HTMLAnchorElement {
  const link = document.createElement("a");
  link.textContent = "read the full notes online";
  link.href = "https://solstone.app/releases/windows";
  link.style.display = "inline-block";
  link.style.margin = "8px 0 0";
  link.style.fontSize = "12.5px";
  link.style.color = "var(--accent)";
  link.style.textDecoration = "none";
  link.style.cursor = "pointer";
  link.onmouseenter = () => {
    link.style.textDecoration = "underline";
  };
  link.onmouseleave = () => {
    link.style.textDecoration = "none";
  };
  link.onclick = (e) => {
    e.preventDefault();
    void invoke("open_release_notes");
  };
  return link;
}

function renderUpdatesSection(view: UpdateView): HTMLElement {
  const pane = section("Updates");
  const a = view.actions;

  // Focal state headline + a quiet subtitle (the macOS Updates header hierarchy),
  // not another label : value row. The headline carries the contract state id.
  const headline = automation(text("div", updateHeadline(view)), ids["settings.updates.state"]);
  headline.style.fontSize = "16px";
  headline.style.fontWeight = "650";
  headline.style.color = "var(--fg)";
  headline.style.lineHeight = "1.3";
  headline.style.margin = "0";
  pane.append(headline);

  // The last-checked clock lives in the subtitle and ticks live (<60s "just
  // now") only where the subtitle *is* the clock; static subtitles (staged,
  // unavailable, never-checked) never tick, so lastCheckedEl stays null there.
  lastCheckedEl = null;
  const subtitle = updateSubtitle(view, nowSecs());
  if (subtitle) {
    const sub = text("div", subtitle.text);
    sub.style.fontSize = "13px";
    sub.style.color = "var(--fg-subtle)";
    sub.style.margin = "3px 0 0";
    if (subtitle.live) {
      automation(sub, ids["settings.updates.lastChecked"]);
      lastCheckedEl = sub;
    }
    pane.append(sub);
  }

  // Progress — determinate for a download, an indeterminate sweep while a check
  // or install is in flight.
  if (view.display === "checking" || view.activity === "installing") {
    pane.append(updateProgressBar(false, null));
  } else if (view.display === "downloading") {
    pane.append(updateProgressBar(true, view.download_pct));
  }

  // Release notes — only present in the available state (the reducer clears them
  // once a version downloads); rendered as light markdown.
  if (view.display === "available" && view.notes) {
    pane.append(updateNotesBlock(view.notes));
  }

  // Action buttons — each shown only when relevant, each disabled from real
  // actionability (no dead buttons). There is no cancel control (Velopack 1.2.0
  // has no cancellation API); an in-flight download/install shows no buttons.
  const actions = document.createElement("div");
  actions.style.display = "flex";
  actions.style.flexWrap = "wrap";
  actions.style.gap = "8px";
  actions.style.margin = "14px 0 2px";
  let anyAction = false;

  const showCheck =
    view.display !== "downloading" &&
    view.display !== "unavailable" &&
    view.activity !== "installing";
  if (showCheck) {
    const checkLabel = view.display === "never_checked" ? "check for updates" : "check again";
    actions.append(
      actionButton(checkLabel, ids["settings.updates.checkNow"], a.can_check_now, () => {
        void invoke("update_check_now");
      }),
    );
    anyAction = true;
  }
  if (view.display === "available") {
    actions.append(
      actionButton("download", ids["settings.updates.download"], a.can_download, () => {
        void invoke("update_download");
      }),
    );
    anyAction = true;
  }
  if (view.display === "staged" && view.activity !== "installing") {
    actions.append(
      actionButton("relaunch to install", ids["settings.updates.install"], a.can_install, () => {
        void invoke("update_install");
      }),
    );
    anyAction = true;
  }
  if (view.display === "failed" || view.display === "failed_with_available") {
    actions.append(
      actionButton("retry", ids["settings.updates.retry"], a.can_retry, () => {
        void invoke("update_check_now");
      }),
    );
    anyAction = true;
  }
  if (a.can_dismiss) {
    actions.append(
      actionButton("dismiss", ids["settings.updates.dismiss"], true, () => {
        void invoke("update_dismiss");
      }),
    );
    anyAction = true;
  }
  if (anyAction) {
    pane.append(actions);
  }

  // Automatic-update preferences, grouped (the macOS "automatic updates" box):
  // the auto-check toggle, the frequency picker indented beneath it (disabled
  // when auto-check is off), and the background-download toggle.
  const prefs = document.createElement("div");
  prefs.style.marginTop = "16px";
  prefs.style.paddingTop = "14px";
  prefs.style.borderTop = "1px solid var(--border-subtle)";

  const prefsLabel = text("div", "automatic updates");
  prefsLabel.style.fontSize = "12px";
  prefsLabel.style.fontWeight = "600";
  prefsLabel.style.color = "var(--fg-subtle)";
  prefsLabel.style.margin = "0 0 4px";
  prefs.append(prefsLabel);

  prefs.append(
    toggleRow(
      "check for updates automatically",
      ids["settings.updates.autoCheck"],
      view.prefs.auto_check,
      (on) => {
        void invoke("update_set_auto_check", { on });
      },
    ),
  );
  prefs.append(frequencyRow(view.prefs.interval, a.frequency_enabled));
  prefs.append(
    toggleRow(
      "download updates in the background",
      ids["settings.updates.autoDownload"],
      view.prefs.auto_download,
      (on) => {
        void invoke("update_set_auto_download", { on });
      },
    ),
  );
  pane.append(prefs);

  // Privacy footnote — the trust line for an update surface, ported from the
  // macOS pane. Honest by construction: the check is a first-party manifest GET
  // with no per-user identifier (the stagingId is neutralized — Article 8).
  const foot = document.createElement("div");
  foot.style.marginTop = "16px";
  foot.style.paddingTop = "12px";
  foot.style.borderTop = "1px solid var(--border-subtle)";
  const footText = text(
    "div",
    "solstone never sends usage data. update checks only fetch the version manifest.",
  );
  footText.style.fontSize = "12px";
  footText.style.color = "var(--fg-subtle)";
  footText.style.lineHeight = "1.45";
  foot.append(footText);
  pane.append(foot);

  return pane;
}

function rerender(): void {
  // Any full render satisfies a deferred background request, so direct callers
  // (commits, hotkey-capture end, boot) auto-flush a pending coalesced rerender.
  pendingRerender = false;
  if (!latestHealth) {
    renderUnavailable();
  } else if (label === "about") {
    renderAbout(latestHealth);
  } else {
    renderSettings(latestHealth);
  }
  scheduleRenderBeacon();
}

// Background event streams call this instead of rerender() directly: defer + coalesce
// the full rerender while a control is active so an open native popup / in-progress
// edit / hotkey capture isn't torn down; otherwise render immediately (no added latency).
function requestRerender(): void {
  if (isInteractiveControlActive()) {
    pendingRerender = true;
    return;
  }
  rerender();
}

function renderUnavailable(): void {
  const rootId = label === "about" ? ids["about.window.root"] : ids["settings.window.root"];
  resetRoot(rootId);

  const title = text("h1", "solstone");
  title.style.margin = "0";
  title.style.padding = "18px 20px 8px";
  title.style.fontSize = "22px";
  title.style.fontWeight = "700";

  const msg = text("p", "couldn't load the observer status just now.");
  msg.style.padding = "0 20px";
  msg.style.color = "var(--fg-subtle)";

  const retry = document.createElement("button");
  retry.textContent = "retry";
  retry.classList.add("fluent-accent");
  retry.style.margin = "8px 20px";
  retry.style.fontSize = "13px";
  retry.style.padding = "7px 14px";
  retry.style.border = "1px solid var(--accent)";
  retry.style.borderRadius = "var(--radius-control)";
  retry.style.background = "var(--accent)";
  retry.style.color = "var(--accent-fg)";
  retry.style.cursor = "pointer";
  retry.addEventListener("click", () => {
    void retryHealth();
  });

  root.append(title, msg, retry);
}

async function retryHealth(): Promise<void> {
  const health = await invoke<HealthDump>("get_health").catch(() => null);
  if (health) {
    latestHealth = health;
  }
  requestRerender();
}

function scheduleRenderBeacon(): void {
  if (renderBeaconFired) {
    return;
  }
  const rootId = label === "about" ? ids["about.window.root"] : ids["settings.window.root"];
  requestAnimationFrame(() => {
    // Gate on OUR contract window-root being present — proves our renderer painted,
    // not a foreign error page. Fires for both the real UI and the error state
    // (both call resetRoot with the contract root id); never if our JS never runs.
    if (document.querySelector(`[data-automation-id="${rootId}"]`)) {
      renderBeaconFired = true;
      try {
        void invoke("view_rendered").catch(() => {});
      } catch {
        // Fire-and-forget only.
      }
    }
  });
}

async function boot(): Promise<void> {
  if (label === "about") {
    latestHealth = await invoke<HealthDump>("get_health").catch(() => null);
    rerender();
    return;
  }
  const [health, storage, update, exclusions, apps, hotkey, micCfg, mics, retention] = await Promise.all([
    invoke<HealthDump>("get_health").catch(() => null),
    invoke<StorageInfo>("storage_info").catch(() => null),
    invoke<UpdateView>("update_get").catch(() => null),
    invoke<ExclusionRules>("get_exclusions").catch(() => null),
    invoke<RunningApp[]>("list_running_apps").catch(() => [] as RunningApp[]),
    invoke<HotkeyView>("get_hotkey").catch(() => null),
    invoke<MicView>("get_mic_config").catch(() => null),
    invoke<MicDeviceRef[]>("list_mic_devices").catch(() => [] as MicDeviceRef[]),
    invoke<RetentionConfig>("get_retention").catch(() => null),
  ]);
  latestHealth = health;
  latestStorage = storage;
  latestUpdate = update;
  latestExclusions = exclusions;
  runningApps = apps;
  latestHotkey = hotkey;
  latestMic = micCfg;
  micDevices = mics;
  latestRetention = retention;
  rerender();
}

void boot();
void listen<HealthDump>("health://changed", (event) => {
  latestHealth = event.payload;
  requestRerender();
});
void listen<UpdateView>("update://changed", (event) => {
  latestUpdate = event.payload;
  requestRerender();
});

// Live last-checked clock: tick the relative string once a second without a full
// re-render (the JS analog of the macOS TimelineView; <60s -> "just now").
setInterval(() => {
  if (lastCheckedEl && latestUpdate) {
    lastCheckedEl.textContent = lastCheckedRelative(latestUpdate.last_checked_at, nowSecs());
  }
}, 1000);
