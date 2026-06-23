// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The webview is a pure renderer. It subscribes to `health://changed` and paints
// the honest state it receives; it has no other input and cannot mint status.
// AutomationIds are stamped from the generated contract (see ./lib/contract.ts).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { automationContract } from "./lib/contract";

type AppPhase = "idle" | "starting" | "observing" | "paused" | "error";
type SourceKind = "screen" | "system_audio" | "mic";
type SourceStatus = "active" | "inactive" | "no_input_device" | "faulted";

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
}

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
let latestUpdate: UpdateView | null = null;
// The last-checked line node, refreshed each render; a 1s interval rewrites its
// text so the relative clock ticks live — the JS analog of the macOS TimelineView.
let lastCheckedEl: HTMLElement | null = null;

// The pair-link the owner typed/pasted. Held in a module var (never read back
// from the DOM) so a health re-render never loses it.
let pairingDraft = "";
let pairingBusy = false;

const ids = automationContract.automation_ids;
const queriedRoot = document.querySelector<HTMLDivElement>("#app");

if (!queriedRoot) {
  throw new Error("missing app root");
}

const root: HTMLDivElement = queriedRoot;

document.body.style.margin = "0";
document.body.style.fontFamily =
  'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
document.body.style.color = "#17201b";
document.body.style.background = "#f6f7f4";

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

function section(title: string): HTMLElement {
  const node = document.createElement("section");
  node.style.padding = "18px 20px";
  node.style.borderBottom = "1px solid #d8ddd4";

  const heading = text("h2", title);
  heading.style.margin = "0 0 12px";
  heading.style.fontSize = "13px";
  heading.style.fontWeight = "700";
  heading.style.textTransform = "uppercase";
  heading.style.letterSpacing = "0";
  heading.style.color = "#415146";
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
  labelNode.style.color = "#5f6b63";
  labelNode.style.fontSize = "13px";
  value.style.fontSize = "13px";
  value.style.overflowWrap = "anywhere";
  row.append(labelNode, value);
  return row;
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

function renderPairingSection(dump: HealthDump): HTMLElement {
  const pairing = dump.sync.pairing;
  const pane = section("Pairing");
  pane.append(
    valueRow(
      "status",
      automation(text("div", pairingPhaseLabel(pairing)), ids["settings.pairing.state"]),
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
  input.style.fontSize = "13px";
  input.style.padding = "7px 9px";
  input.style.border = "1px solid #c4ccc0";
  input.style.borderRadius = "6px";
  input.style.minWidth = "0";
  input.oninput = () => {
    pairingDraft = input.value;
  };

  const button = document.createElement("button");
  const busy = pairingBusy || pairing.phase === "pairing";
  button.textContent = busy ? "pairing…" : "pair";
  button.disabled = busy;
  button.dataset.automationId = ids["settings.pairing.submit"];
  button.style.fontSize = "13px";
  button.style.padding = "7px 14px";
  button.style.border = "1px solid #2f6f4f";
  button.style.borderRadius = "6px";
  button.style.background = busy ? "#9bb6a6" : "#2f6f4f";
  button.style.color = "#fff";
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

function resetRoot(rootId: string): void {
  root.replaceChildren();
  root.dataset.automationId = rootId;
  root.style.minHeight = "100vh";
}

function renderSettings(dump: HealthDump): void {
  resetRoot(ids["settings.window.root"]);

  const title = text("h1", "solstone");
  title.style.margin = "0";
  title.style.padding = "18px 20px 8px";
  title.style.fontSize = "22px";
  title.style.fontWeight = "700";
  root.append(title);

  const status = section("Status");
  status.append(
    valueRow(
      "state",
      automation(text("div", phaseLabel(dump.app_state)), ids["settings.status.appState.state"]),
    ),
    valueRow(
      "segment directory",
      automation(
        text("div", dump.segment_dir ?? "not available"),
        ids["settings.status.segmentDir"],
      ),
    ),
    valueRow(
      "journal sync",
      automation(text("div", uploadLabel(dump.sync.upload)), ids["settings.status.upload.state"]),
    ),
  );

  const sources = section("Sources");
  const screen = sourceByKind(dump, "screen");
  const systemAudio = sourceByKind(dump, "system_audio");
  const mic = sourceByKind(dump, "mic");
  sources.append(
    valueRow(
      "screen",
      automation(text("div", sourceStatusLabel(screen)), ids["settings.sources.screen.state"]),
    ),
    valueRow(
      "system audio",
      automation(
        text("div", sourceStatusLabel(systemAudio)),
        ids["settings.sources.systemAudio.state"],
      ),
    ),
    valueRow(
      "microphone",
      automation(text("div", sourceStatusLabel(mic)), ids["settings.sources.mic.state"]),
    ),
  );

  root.append(status, sources, renderPairingSection(dump));
  if (latestUpdate) {
    root.append(renderUpdatesSection(latestUpdate));
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
  body.style.color = "#415146";

  const version = automation(text("div", dump.version), ids["about.version"]);
  version.style.fontSize = "13px";
  version.style.color = "#5f6b63";

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
  switch (view.display) {
    case "never_checked":
      return "not checked yet";
    case "up_to_date":
      return "solstone is up to date";
    case "checking":
      return "checking for updates…";
    case "available":
      return `solstone ${v} is available`;
    case "downloading":
      return `downloading ${v} — ${view.download_pct ?? 0}%`;
    case "staged":
      return `solstone ${v} is ready — it installs when solstone relaunches`;
    case "failed":
      return "couldn't check for updates right now";
    case "failed_with_available":
      return `couldn't check right now — solstone ${v} was found earlier`;
    case "unavailable":
      return "this build can't check for updates on its own";
  }
}

function actionButton(
  labelText: string,
  automationId: string,
  enabled: boolean,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = labelText;
  b.disabled = !enabled;
  b.dataset.automationId = automationId;
  b.style.fontSize = "13px";
  b.style.padding = "6px 12px";
  b.style.border = enabled ? "1px solid #2f6f4f" : "1px solid #c4ccc0";
  b.style.borderRadius = "6px";
  b.style.background = enabled ? "#2f6f4f" : "#e7ebe5";
  b.style.color = enabled ? "#fff" : "#9aa49c";
  b.style.cursor = enabled ? "pointer" : "default";
  if (enabled) {
    b.onclick = onClick;
  }
  return b;
}

function checkboxRow(
  labelText: string,
  automationId: string,
  checked: boolean,
  enabled: boolean,
  onChange: (on: boolean) => void,
): HTMLDivElement {
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = checked;
  box.disabled = !enabled;
  box.dataset.automationId = automationId;
  box.onchange = () => onChange(box.checked);
  const wrap = document.createElement("div");
  wrap.append(box);
  return valueRow(labelText, wrap);
}

function frequencyRow(interval: CheckIntervalKind, enabled: boolean): HTMLDivElement {
  const sel = document.createElement("select");
  sel.disabled = !enabled;
  sel.dataset.automationId = ids["settings.updates.frequency"];
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
  sel.onchange = () => {
    void invoke("update_set_interval", { interval: sel.value });
  };
  const wrap = document.createElement("div");
  wrap.append(sel);
  return valueRow("how often", wrap);
}

function renderUpdatesSection(view: UpdateView): HTMLElement {
  const pane = section("Updates");
  const a = view.actions;

  pane.append(
    valueRow(
      "status",
      automation(text("div", updateHeadline(view)), ids["settings.updates.state"]),
    ),
  );

  lastCheckedEl = automation(
    text("div", lastCheckedRelative(view.last_checked_at, nowSecs())),
    ids["settings.updates.lastChecked"],
  );
  pane.append(valueRow("last checked", lastCheckedEl));

  // Action buttons — each shown only when relevant, each disabled from real
  // actionability (no dead buttons). "check" is always present; there is no
  // cancel control (Velopack 1.2.0 has no cancellation API).
  const actions = document.createElement("div");
  actions.style.display = "flex";
  actions.style.flexWrap = "wrap";
  actions.style.gap = "8px";
  actions.style.padding = "7px 0";

  const checkLabel = view.display === "never_checked" ? "check for updates" : "check again";
  actions.append(
    actionButton(checkLabel, ids["settings.updates.checkNow"], a.can_check_now, () => {
      void invoke("update_check_now");
    }),
  );
  if (view.display === "available") {
    actions.append(
      actionButton("download", ids["settings.updates.download"], a.can_download, () => {
        void invoke("update_download");
      }),
    );
  }
  if (view.display === "staged") {
    actions.append(
      actionButton("relaunch to install", ids["settings.updates.install"], a.can_install, () => {
        void invoke("update_install");
      }),
    );
  }
  if (view.display === "failed" || view.display === "failed_with_available") {
    actions.append(
      actionButton("retry", ids["settings.updates.retry"], a.can_retry, () => {
        void invoke("update_check_now");
      }),
    );
  }
  if (a.can_dismiss) {
    actions.append(
      actionButton("dismiss", ids["settings.updates.dismiss"], true, () => {
        void invoke("update_dismiss");
      }),
    );
  }
  pane.append(actions);

  // Release notes (functional default; VPX owns the experience-layer pass).
  if (view.notes) {
    const notes = automation(document.createElement("div"), ids["settings.updates.notes"]);
    notes.textContent = view.notes;
    notes.style.whiteSpace = "pre-wrap";
    notes.style.fontSize = "12px";
    notes.style.color = "#415146";
    notes.style.maxHeight = "120px";
    notes.style.overflowY = "auto";
    pane.append(valueRow("what's new", notes));
  }

  // Preferences.
  pane.append(
    checkboxRow(
      "check automatically",
      ids["settings.updates.autoCheck"],
      view.prefs.auto_check,
      true,
      (on) => {
        void invoke("update_set_auto_check", { on });
      },
    ),
  );
  pane.append(frequencyRow(view.prefs.interval, a.frequency_enabled));
  pane.append(
    checkboxRow(
      "download in the background",
      ids["settings.updates.autoDownload"],
      view.prefs.auto_download,
      true,
      (on) => {
        void invoke("update_set_auto_download", { on });
      },
    ),
  );

  return pane;
}

function rerender(): void {
  if (!latestHealth) {
    return;
  }
  if (label === "about") {
    renderAbout(latestHealth);
  } else {
    renderSettings(latestHealth);
  }
}

async function boot(): Promise<void> {
  if (label === "about") {
    latestHealth = await invoke<HealthDump>("get_health");
    rerender();
    return;
  }
  const [health, update] = await Promise.all([
    invoke<HealthDump>("get_health"),
    invoke<UpdateView>("update_get").catch(() => null),
  ]);
  latestHealth = health;
  latestUpdate = update;
  rerender();
}

void boot();
void listen<HealthDump>("health://changed", (event) => {
  latestHealth = event.payload;
  rerender();
});
void listen<UpdateView>("update://changed", (event) => {
  latestUpdate = event.payload;
  rerender();
});

// Live last-checked clock: tick the relative string once a second without a full
// re-render (the JS analog of the macOS TimelineView; <60s -> "just now").
setInterval(() => {
  if (lastCheckedEl && latestUpdate) {
    lastCheckedEl.textContent = lastCheckedRelative(latestUpdate.last_checked_at, nowSecs());
  }
}, 1000);
