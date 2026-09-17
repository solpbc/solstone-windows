// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump, sampleMarkSpec, unknownJournalsDump } from "./fixtures";

const ids = automationContract.automation_ids;

const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function resetRoot(rootId = ids["settings.window.root"]): HTMLDivElement {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  rootEl.setAttribute("data-automation-id", rootId);
  document.body.append(rootEl);
  app.__test__.reset();
  app.__test__.setRoot(rootEl);
  return rootEl;
}

function present(id: string): HTMLElement {
  const el = byId(id);
  expect(el).not.toBeNull();
  return el as HTMLElement;
}

describe("unknown journal settings UI", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("does not render summary node when sightings are absent or empty", () => {
    app.__test__.setRoute("journal");
    const dump = observingDump();
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.summary"]}"]`).length).toBe(0);
    expect(byId(ids["settings.unknown-journal.list"])).toBeNull();
  });

  it("renders two sightings with exact copy, locked geometry, rotation, and mark differences", () => {
    const dump = unknownJournalsDump([
      {
        address: "192.168.1.120:443",
        expected_mark: sampleMarkSpec("liquefy", "smock", "#3b82f6"),
        responding_mark: sampleMarkSpec("distrust", "chokehold", "#ec4899"),
      },
      {
        address: null,
        expected_mark: sampleMarkSpec("anchor", "banana", "#10b981"),
        responding_mark: null,
      },
    ]);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const summaries = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.summary"]}"]`);
    expect(summaries.length).toBe(2);
    expect(summaries[0].textContent).toBe("unknown journal seen at 192.168.1.120:443 — view details");
    expect(summaries[1].textContent).toBe("unknown journal seen through the relay — view details");

    const details = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.detail"]}"]`);
    expect(details.length).toBe(2);
    expect(details[0].textContent).toContain("something other than your journal answered. compare its mark with your journal's own.");
    expect(details[1].textContent).toContain("something other than your journal answered. compare its mark with your journal's own.");

    const whatAnswereds = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.what-answered"]}"]`);
    const yourJournals = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.your-journal"]}"]`);
    expect(whatAnswereds.length).toBe(2);
    expect(yourJournals.length).toBe(2);
    expect(whatAnswereds[0].textContent).toContain("what answered");
    expect(yourJournals[0].textContent).toContain("your journal");

    const captions = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.responding-caption"]}"]`);
    expect(captions.length).toBe(2);
    expect(captions[0].textContent).toBe("claimed, not verified");
    expect(captions[1].textContent).toBe("no identity presented");

    expect(whatAnswereds[1].textContent).toContain("not");
    expect(whatAnswereds[1].textContent).toContain("presented");

    // Marks different (hex/words)
    expect(whatAnswereds[0].textContent).toContain("distrust · chokehold");
    expect(yourJournals[0].textContent).toContain("liquefy · smock");

    // Tile geometry on first sighting
    const tiles = whatAnswereds[0].querySelectorAll(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
    const tile0 = tiles[0] as HTMLElement;
    expect(tile0.style.width).toBe("32px");
    expect(tile0.style.height).toBe("32px");
    expect(tile0.style.borderRadius).toBe("8px");
    expect(tile0.style.border).toContain("2px");
    expect(tile0.style.background).toMatch(/(1f|0\.12)/);
    expect(tile0.style.transform).toBe("rotate(45deg)");

    const svg = tile0.querySelector("svg");
    expect(svg).not.toBeNull();
    expect(svg?.getAttribute("viewBox")).toBe("0 0 24 24");

    // Empty tile geometry on second sighting
    const emptyTiles = whatAnswereds[1].querySelectorAll(".unknown-journal-mark-tile");
    expect(emptyTiles.length).toBe(2);
    const emptyTile0 = emptyTiles[0] as HTMLElement;
    expect(emptyTile0.style.width).toBe("32px");
    expect(emptyTile0.style.height).toBe("32px");
    expect(emptyTile0.style.borderRadius).toBe("8px");
    expect(emptyTile0.style.border).toContain("2px dashed");

    // Middots present
    expect(whatAnswereds[0].textContent).toContain("·");
    expect(yourJournals[0].textContent).toContain("·");
    expect(whatAnswereds[1].textContent).toContain("·");
  });

  it("leaves pairing labels untouched on journal route", () => {
    const dump = unknownJournalsDump([
      {
        address: "192.168.1.120:443",
        expected_mark: sampleMarkSpec("liquefy", "smock", "#3b82f6"),
        responding_mark: sampleMarkSpec("distrust", "chokehold", "#ec4899"),
      },
    ]);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const pairedEl = present(ids["settings.pairing.state"]);
    expect(pairedEl.textContent).toContain("paired with");

    const syncSummaryEl = present(ids["settings.status.upload.state"]);
    expect(syncSummaryEl.textContent).toContain("delivered");
  });

  it("ensures owner-visible strings contain no observer", () => {
    const dump = unknownJournalsDump([
      {
        address: "192.168.1.120:443",
        expected_mark: sampleMarkSpec("liquefy", "smock", "#3b82f6"),
        responding_mark: sampleMarkSpec("distrust", "chokehold", "#ec4899"),
      },
      {
        address: null,
        expected_mark: sampleMarkSpec("anchor", "banana", "#10b981"),
        responding_mark: null,
      },
    ]);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const list = present(ids["settings.unknown-journal.list"]);
    expect(list.textContent?.toLowerCase()).not.toContain("observer");
  });
});
