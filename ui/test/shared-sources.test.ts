// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
import { expect, it } from "vitest";
import { otherDevices, summary, wrappingSummary } from "../src/shell/shared-sources";

it("ranks distinct peers by source evidence and ages times without refreshing", () => {
  const stamp = (hour: number) => `2026-11-20T${hour.toString().padStart(2, "0")}:00:00Z`;
  const entry = (origin: string, detail: string, last_synced_at: string | null) => ({
    origin, detail, last_synced_at, source: "obsidian", declared_at: stamp(10), state: origin === "declared" ? "paused" : null,
  });
  const peer = (cid: string, time: number, ...sources: ReturnType<typeof entry>[]) => ({
    cid, display_label: "Desk", device_type: null, platform: null, device_last_seen_at: stamp(time), sources,
  });
  const peers = otherDevices({binding: "current", devices: [
    peer("a", 14, entry("observed","Old",stamp(8))),
    peer("c", 12, entry("declared","Two",stamp(7)), entry("declared","One",stamp(9))),
    peer("d", 15, entry("observed","Undated",null)),
    peer("b", 11, entry("declared","Synced",stamp(12))),
  ]});
  expect(peers.map(p => p.cid)).toEqual(["b", "c", "a", "d"]);
  expect(peers[1].vaults.map(v => v.detail)).toEqual(["One", "Two"]);
  const v = peers[2].vaults[0];
  const at = Date.parse(stamp(8)) / 1000;
  expect(summary("Desk", v, at + 120)).toBe("Desk · 2 min ago");
  expect(summary("Desk", v, at + 3600)).toBe("Desk · 1 hour ago");
  expect(summary("Desk", peers[3].vaults[0], at)).toBe("Desk");
});

it("lets a line break only after a separator, and shortens a long name on the tile", () => {
  const name = "A device name that runs well past the tile";
  const vault = {detail: "Notes", state: "needs attention", at: 1_000};
  const breaks = (line: string) => [...line.matchAll(/ /g)].map(m => line[m.index - 1]);
  const tile = wrappingSummary(name, vault, 1_000 + 240, true);
  expect(breaks(tile).every(before => before === "\u00b7")).toBe(true);
  expect(tile).not.toMatch(/^\u00b7| \u00b7/);
  const shown = tile.split(/[\u00a0 ]\u00b7/)[0];
  expect([...shown].length).toBeLessThanOrEqual(20);
  expect(shown.endsWith("\u2026")).toBe(true);
  expect(wrappingSummary("Desk", vault, 1_000 + 240, true).startsWith("Desk")).toBe(true);
  const full = wrappingSummary(name, vault, 1_000 + 240);
  expect(full.replace(/\u00a0/g, " ")).toBe(summary(name, vault, 1_000 + 240));
  const tail = full.slice(name.length);
  expect(breaks(tail).every(before => before === "\u00b7")).toBe(true);
});
