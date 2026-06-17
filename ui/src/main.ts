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

interface HealthDump {
  app_state: AppPhase;
  sources: SourceReport[];
  frame_rate: number | null;
  segment_dir: string | null;
  segment_seconds_remaining: number | null;
  engine_ready: boolean;
  version: string;
  sync: SyncSnapshot;
}

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

function render(dump: HealthDump): void {
  if (label === "about") {
    renderAbout(dump);
  } else {
    renderSettings(dump);
  }
}

void invoke<HealthDump>("get_health").then(render);
void listen<HealthDump>("health://changed", (event) => render(event.payload));
