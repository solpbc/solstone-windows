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

// Capture-exclusion rules + the running-app picker list. Held in module vars so a
// 1s health re-render repaints the section without losing edits; `titleDraft`
// preserves a half-typed title keyword across re-renders (like pairingDraft).
let latestExclusions: ExclusionRules | null = null;
let latestHotkey: HotkeyView | null = null;
let latestMic: MicView | null = null;
let micDevices: MicDeviceRef[] = [];
let runningApps: RunningApp[] = [];
let titleDraft = "";

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

// Keyframes for the indeterminate update-progress sweep (checking / installing).
const updateProgressStyle = document.createElement("style");
updateProgressStyle.textContent =
  "@keyframes update-indeterminate{0%{left:-35%}100%{left:100%}}";
document.head.append(updateProgressStyle);

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
  const parts = [
    `${health.frames_redacted} frame${health.frames_redacted === 1 ? "" : "s"} redacted`,
  ];
  if (health.frames_dropped > 0) {
    parts.push(`${health.frames_dropped} dropped`);
  }
  return `${parts.join(" · ")} this session`;
}

// A removable list of string values (excluded exes / title keywords).
function removableList(
  values: string[],
  listAutomationId: string,
  onRemove: (value: string) => void,
): HTMLElement {
  const list = automation(document.createElement("div"), listAutomationId);
  list.style.display = "flex";
  list.style.flexWrap = "wrap";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (values.length === 0) {
    const empty = text("div", "none yet");
    empty.style.color = "#9aa49c";
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
    chip.style.border = "1px solid #c4ccc0";
    chip.style.borderRadius = "6px";
    chip.style.background = "#eef1ec";
    chip.append(text("span", value));

    const remove = document.createElement("button");
    remove.textContent = "×";
    remove.setAttribute("aria-label", `remove ${value}`);
    remove.style.border = "none";
    remove.style.background = "transparent";
    remove.style.color = "#5f6b63";
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

  // Private browsing — title-heuristic auto-exclude, on by default.
  pane.append(
    checkboxRow(
      "private browsing",
      ids["settings.exclusions.privateBrowsing"],
      rules.exclude_private_browsing,
      true,
      (on) => {
        void applyExclusions({ ...rules, exclude_private_browsing: on });
      },
    ),
  );
  const privateHelp = text(
    "div",
    "automatically keep private and incognito browser windows out of the journal",
  );
  privateHelp.style.color = "#5f6b63";
  privateHelp.style.fontSize = "12px";
  privateHelp.style.padding = "0 0 6px";
  pane.append(privateHelp);

  // Excluded apps — pick from the live running-app list (robust process identity).
  const appPickRow = document.createElement("div");
  appPickRow.style.display = "grid";
  appPickRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  appPickRow.style.gap = "8px";
  appPickRow.style.padding = "7px 0";

  const appSelect = document.createElement("select");
  appSelect.dataset.automationId = ids["settings.exclusions.appInput"];
  appSelect.style.fontSize = "13px";
  appSelect.style.padding = "7px 9px";
  appSelect.style.border = "1px solid #c4ccc0";
  appSelect.style.borderRadius = "6px";
  appSelect.style.minWidth = "0";
  const choices = runningApps.filter((app) => !rules.excluded_exes.includes(app.exe_name));
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = choices.length > 0 ? "choose a running app…" : "no other apps running";
  appSelect.append(placeholder);
  for (const app of choices) {
    const opt = document.createElement("option");
    opt.value = app.exe_name;
    opt.textContent = `${app.display_name} (${app.exe_name})`;
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
  pane.append(valueRow("excluded apps", document.createElement("div")));
  pane.append(appPickRow);
  pane.append(
    removableList(rules.excluded_exes, ids["settings.exclusions.appsList"], (exe) => {
      void applyExclusions({
        ...rules,
        excluded_exes: rules.excluded_exes.filter((e) => e !== exe),
      });
    }),
  );

  // Title keywords — case-insensitive substring of a window title.
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
  titleInput.style.fontSize = "13px";
  titleInput.style.padding = "7px 9px";
  titleInput.style.border = "1px solid #c4ccc0";
  titleInput.style.borderRadius = "6px";
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
  pane.append(valueRow("title keywords", document.createElement("div")));
  pane.append(titleRow);
  pane.append(
    removableList(rules.title_patterns, ids["settings.exclusions.titlesList"], (keyword) => {
      void applyExclusions({
        ...rules,
        title_patterns: rules.title_patterns.filter((k) => k !== keyword),
      });
    }),
  );

  // Exclusion activity — the never-silent surface: redacted/dropped this session.
  pane.append(
    valueRow(
      "exclusion activity",
      automation(
        text("div", exclusionActivityLabel(dump.exclusions)),
        ids["settings.exclusions.activity"],
      ),
    ),
  );

  return pane;
}

function resetRoot(rootId: string): void {
  root.replaceChildren();
  root.dataset.automationId = rootId;
  root.style.minHeight = "100vh";
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

  pane.append(
    checkboxRow(
      "pause/resume shortcut",
      ids["settings.hotkey.enabled"],
      cfg.enabled,
      true,
      (on) => {
        void applyHotkey({ ...cfg, enabled: on });
      },
    ),
  );

  // Modifier checkboxes — the functional default (VPX owns a nicer "press your
  // shortcut" capture in the experience pass).
  const mods = document.createElement("div");
  mods.style.display = "flex";
  mods.style.flexWrap = "wrap";
  mods.style.gap = "12px";
  const modDefs: ReadonlyArray<readonly ["ctrl" | "alt" | "shift" | "win", string]> = [
    ["ctrl", "Ctrl"],
    ["alt", "Alt"],
    ["shift", "Shift"],
    ["win", "Win"],
  ];
  for (const [key, lbl] of modDefs) {
    const wrap = document.createElement("label");
    wrap.style.display = "inline-flex";
    wrap.style.alignItems = "center";
    wrap.style.gap = "4px";
    wrap.style.fontSize = "13px";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = cfg[key];
    box.onchange = () => {
      void applyHotkey({ ...cfg, [key]: box.checked });
    };
    wrap.append(box, text("span", lbl));
    mods.append(wrap);
  }
  pane.append(valueRow("modifiers", mods));

  const keySel = document.createElement("select");
  keySel.dataset.automationId = ids["settings.hotkey.combo"];
  keySel.style.fontSize = "13px";
  const none = document.createElement("option");
  none.value = "0";
  none.textContent = "(choose a key)";
  keySel.append(none);
  for (const [vk, lbl] of HOTKEY_KEYS) {
    const opt = document.createElement("option");
    opt.value = String(vk);
    opt.textContent = lbl;
    if (vk === cfg.vk) {
      opt.selected = true;
    }
    keySel.append(opt);
  }
  keySel.onchange = () => {
    void applyHotkey({ ...cfg, vk: Number(keySel.value) });
  };
  pane.append(valueRow("key", keySel));

  pane.append(
    valueRow(
      "status",
      automation(text("div", hotkeyStatusLabel(view)), ids["settings.hotkey.status"]),
    ),
  );

  const clearRow = document.createElement("div");
  clearRow.style.padding = "7px 0";
  clearRow.append(
    actionButton("clear shortcut", ids["settings.hotkey.clear"], hotkeyHasCombo(cfg), () => {
      void applyHotkey({
        enabled: false,
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        vk: 0,
      });
    }),
  );
  pane.append(clearRow);

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

function reorderButton(glyph: string, enabled: boolean, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = glyph;
  b.disabled = !enabled;
  b.style.fontSize = "13px";
  b.style.lineHeight = "1";
  b.style.padding = "3px 7px";
  b.style.border = "1px solid #c4ccc0";
  b.style.borderRadius = "6px";
  b.style.background = enabled ? "#eef1ec" : "#f3f5f1";
  b.style.color = enabled ? "#415146" : "#c4ccc0";
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

  // Input gain — segmented 1× / 2× / 4× / 8×.
  const gainRow = automation(document.createElement("div"), ids["settings.mic.gain"]);
  gainRow.style.display = "flex";
  gainRow.style.gap = "6px";
  for (const level of MIC_GAIN_LEVELS) {
    const on = cfg.gain === level;
    const b = document.createElement("button");
    b.textContent = `${level}×`;
    b.style.fontSize = "13px";
    b.style.padding = "5px 12px";
    b.style.border = on ? "1px solid #2f6f4f" : "1px solid #c4ccc0";
    b.style.borderRadius = "6px";
    b.style.background = on ? "#2f6f4f" : "#eef1ec";
    b.style.color = on ? "#fff" : "#415146";
    b.style.cursor = "pointer";
    b.onclick = () => {
      void applyMic({ ...cfg, gain: level });
    };
    gainRow.append(b);
  }
  pane.append(valueRow("input gain", gainRow));

  // Active device — earned from what the loop actually opened.
  const activeName =
    ordered.find((d) => d.id === view.active_id)?.name ??
    (view.active_id ? "selected device" : "none");
  pane.append(
    valueRow(
      "active microphone",
      automation(text("div", activeName), ids["settings.mic.active"]),
    ),
  );

  // Device priority list — reorder + enable/disable. Highest priority first.
  const list = automation(document.createElement("div"), ids["settings.mic.devices"]);
  list.style.display = "flex";
  list.style.flexDirection = "column";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (ordered.length === 0) {
    const empty = text("div", "no microphone input devices");
    empty.style.color = "#9aa49c";
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
      reorderButton("↑", idx > 0, () => {
        const o = [...orderIds];
        [o[idx - 1], o[idx]] = [o[idx], o[idx - 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );
    row.append(
      reorderButton("↓", idx < ordered.length - 1, () => {
        const o = [...orderIds];
        [o[idx + 1], o[idx]] = [o[idx], o[idx + 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );

    const label = text("span", d.name);
    label.style.flex = "1";
    if (disabled) {
      label.style.textDecoration = "line-through";
      label.style.color = "#9aa49c";
    }
    if (d.id === view.active_id) {
      const badge = text("span", " · active");
      badge.style.color = "#2f6f4f";
      label.append(badge);
    }
    row.append(label);

    const toggle = document.createElement("button");
    toggle.textContent = disabled ? "enable" : "disable";
    toggle.style.fontSize = "12px";
    toggle.style.padding = "4px 10px";
    toggle.style.border = "1px solid #c4ccc0";
    toggle.style.borderRadius = "6px";
    toggle.style.background = "#eef1ec";
    toggle.style.color = "#415146";
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
  pane.append(valueRow("devices", document.createElement("div")));
  pane.append(list);

  return pane;
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
      automation(text("div", statusStateLabel(dump)), ids["settings.status.appState.state"]),
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

  root.append(status, sources);
  if (latestExclusions) {
    root.append(renderExclusionsSection(latestExclusions, dump));
  }
  if (latestHotkey) {
    root.append(renderHotkeySection(latestHotkey));
  }
  if (latestMic) {
    root.append(renderMicSection(latestMic));
  }
  root.append(renderPairingSection(dump));
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

function frequencyRow(interval: CheckIntervalKind, enabled: boolean): HTMLElement {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.alignItems = "center";
  row.style.gap = "10px";
  row.style.padding = "6px 0";
  row.style.marginLeft = "26px";

  const labelNode = text("div", "how often");
  labelNode.style.fontSize = "13px";
  labelNode.style.color = enabled ? "#5f6b63" : "#9aa49c";

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
  sel.style.padding = "3px 6px";
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
  lab.style.color = "#17201b";
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
  track.style.background = "#e7ebe5";
  track.style.overflow = "hidden";

  const fill = document.createElement("div");
  fill.style.position = "absolute";
  fill.style.top = "0";
  fill.style.bottom = "0";
  fill.style.background = "#2f6f4f";
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
    lbl.style.color = "#5f6b63";
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
  cap.style.color = "#5f6b63";
  cap.style.margin = "0 0 6px";

  const card = automation(document.createElement("div"), ids["settings.updates.notes"]);
  card.style.background = "#ffffff";
  card.style.border = "1px solid #e2e7dd";
  card.style.borderRadius = "6px";
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
      h.style.color = "#17201b";
      h.style.margin = "6px 0 3px";
      card.append(h);
    } else if (bullet) {
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.gap = "7px";
      row.style.fontSize = "12.5px";
      row.style.color = "#415146";
      row.style.lineHeight = "1.45";
      row.style.margin = "2px 0";
      const dot = text("div", "•");
      dot.style.color = "#2f6f4f";
      row.append(dot, text("div", bullet[1]));
      card.append(row);
    } else {
      const p = text("div", line);
      p.style.fontSize = "12.5px";
      p.style.color = "#415146";
      p.style.lineHeight = "1.45";
      p.style.margin = "2px 0";
      card.append(p);
    }
  }
  wrap.append(cap, card);
  return wrap;
}

function renderUpdatesSection(view: UpdateView): HTMLElement {
  const pane = section("Updates");
  const a = view.actions;

  // Focal state headline + a quiet subtitle (the macOS Updates header hierarchy),
  // not another label : value row. The headline carries the contract state id.
  const headline = automation(text("div", updateHeadline(view)), ids["settings.updates.state"]);
  headline.style.fontSize = "16px";
  headline.style.fontWeight = "650";
  headline.style.color = "#17201b";
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
    sub.style.color = "#5f6b63";
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
  prefs.style.borderTop = "1px solid #e2e7dd";

  const prefsLabel = text("div", "automatic updates");
  prefsLabel.style.fontSize = "12px";
  prefsLabel.style.fontWeight = "600";
  prefsLabel.style.color = "#5f6b63";
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
  foot.style.borderTop = "1px solid #e2e7dd";
  const footText = text(
    "div",
    "solstone never sends usage data. update checks only fetch the version manifest.",
  );
  footText.style.fontSize = "12px";
  footText.style.color = "#5f6b63";
  footText.style.lineHeight = "1.45";
  foot.append(footText);
  pane.append(foot);

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
  const [health, update, exclusions, apps, hotkey, micCfg, mics] = await Promise.all([
    invoke<HealthDump>("get_health"),
    invoke<UpdateView>("update_get").catch(() => null),
    invoke<ExclusionRules>("get_exclusions").catch(() => null),
    invoke<RunningApp[]>("list_running_apps").catch(() => [] as RunningApp[]),
    invoke<HotkeyView>("get_hotkey").catch(() => null),
    invoke<MicView>("get_mic_config").catch(() => null),
    invoke<MicDeviceRef[]>("list_mic_devices").catch(() => [] as MicDeviceRef[]),
  ]);
  latestHealth = health;
  latestUpdate = update;
  latestExclusions = exclusions;
  runningApps = apps;
  latestHotkey = hotkey;
  latestMic = micCfg;
  micDevices = mics;
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
