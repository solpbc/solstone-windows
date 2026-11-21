// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { number, stateWord, t } from "../i18n";
import { relativeTaken } from "./derive";
import { glyph, type GlyphName } from "./glyphs";

interface Source {
  source: string;
  state: string | null;
  origin: string;
  detail: string | null;
  declared_at: string | null;
  last_synced_at: string | null;
}
interface Device {
  cid: string;
  display_label: string;
  device_type: string | null;
  platform: string | null;
  device_last_seen_at: string | null;
  sources: Source[];
}
export interface KnownSources { binding: string; devices: Device[] }
export interface Vault { detail: string; state: string; at: number | null }
export interface OtherDevice { cid: string; name: string; mark: GlyphName; vaults: Vault[] }

function epoch(raw: string | null): number | null {
  const ms = raw ? Date.parse(raw) : NaN;
  return Number.isFinite(ms) ? ms / 1000 : null;
}
function newest(a: Vault, b: Vault): number {
  if (a.at === b.at) return 0;
  if (a.at === null) return 1;
  if (b.at === null) return -1;
  return b.at - a.at;
}
function byText(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}
export function otherDevices(view: KnownSources): OtherDevice[] {
  const devices = new Map<string, OtherDevice>();
  for (const d of view.devices) {
    for (const s of d.sources) {
      if (s.source !== "obsidian") continue;
      const observed = s.origin === "observed";
      if (!observed && (s.origin !== "declared" || !["on", "paused", "needs_attention"].includes(s.state ?? ""))) continue;
      const times = (observed ? [s.last_synced_at] : [s.last_synced_at, s.declared_at, d.device_last_seen_at])
        .map(epoch).filter((at): at is number => at !== null);
      const state = observed ? "" : s.state === "paused" ? stateWord("paused") :
        s.state === "needs_attention" ? stateWord("needs attention") : "";
      let device = devices.get(d.cid);
      if (!device) {
        const mark = d.device_type === "tablet" ? "tablet" : d.device_type === "phone" ? "phone" : "device";
        device = { cid: d.cid, name: d.display_label, mark, vaults: [] };
        devices.set(d.cid, device);
      }
      device.vaults.push({detail: s.detail ?? "", state, at: times.length ? Math.max(...times) : null});
    }
  }
  for (const d of devices.values()) d.vaults.sort((a,b) => newest(a,b) || byText(a.detail,b.detail));
  return [...devices.values()].sort((a,b) => newest(a.vaults[0],b.vaults[0]) || byText(a.name,b.name) || byText(a.cid,b.cid));
}

export function heading(count: number): string {
  return count === 1 ? t("windows.sharedsources.one") : t("windows.sharedsources.many", {count: number(count)});
}

function compose(label: string, state: string, relative: string): string {
  const values = {label, state, relative};
  if (label && state && relative) return t("windows.sharedsources.labelstateage", values);
  if (label && relative) return t("windows.sharedsources.labelage", values);
  if (label && state) return t("windows.sharedsources.labelstate", values);
  if (state && relative) return t("windows.sharedsources.stateage", values);
  return label || state || relative;
}

export function summary(label: string, vault: Vault, now: number): string {
  return compose(label, vault.state, vault.at === null ? "" : relativeTaken(vault.at, now));
}

const NAME_CHARS = 20;
const whole = (part: string) => part.replace(/ /g, "\u00a0");

/**
 * The separator wrap rule: a line breaks only after a separator, so the state and the
 * time are never split and no line starts with one. On the tile the name stays whole
 * too, shortened first when it is long; elsewhere the full name may wrap at its spaces.
 */
export function wrappingSummary(label: string, vault: Vault, now: number, compact = false): string {
  const chars = [...label];
  const name = !compact ? label : whole(chars.length > NAME_CHARS ? `${chars.slice(0, NAME_CHARS - 1).join("").trimEnd()}\u2026` : label);
  const relative = vault.at === null ? "" : relativeTaken(vault.at, now);
  return compose(name, whole(vault.state), whole(relative)).replace(/ \u00b7/g, "\u00a0\u00b7");
}

const clocks = new WeakMap<HTMLElement, () => void>();
function line(label: string, vault: Vault, compact = false): HTMLElement {
  const node = document.createElement("span");
  node.className = "desk-remote-line";
  let previous: string | undefined;
  const paint = () => {
    const value = wrappingSummary(label, vault, Math.floor(Date.now()/1000), compact);
    if (value === previous) return;
    previous = value;
    node.textContent = value;
  };
  paint();
  clocks.set(node, paint);
  return node;
}
export function updateSharedClocks(): void {
  document.querySelectorAll<HTMLElement>(".desk-remote-line").forEach(node => clocks.get(node)?.());
}
export function deviceLine(device: OtherDevice, compact = false): HTMLElement {
  return line(device.name, device.vaults[0], compact);
}
export function remoteHeading(count: number): HTMLElement {
  const node = document.createElement("span");
  node.className = "desk-tile-word desk-remote-word";
  node.textContent = heading(count);
  return node;
}
export function remoteCard(devices: OtherDevice[]): HTMLElement {
  const card = document.createElement("section");
  card.className = "desk-remote-card";
  card.append(remoteHeading(devices.length));
  for (const d of devices) {
    const row = document.createElement("div");
    row.className = "desk-remote-device";
    const body = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = d.name;
    body.append(name, ...d.vaults.map(vault => line(vault.detail, vault)));
    row.append(glyph(d.mark), body);
    card.append(row);
  }
  return card;
}
