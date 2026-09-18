// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { notPairedDump, observingDump, pairedWithMarkDump, sampleMarkSpec } from "./fixtures";

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

describe("paired journal mark in the ordinary pairing pane", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("shows the paired journal's own mark on the pairing pane", () => {
    const dump = pairedWithMarkDump(sampleMarkSpec("liquefy", "smock", "#3b82f6"));
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const markEl = present(ids["settings.pairing.mark"]);
    expect(markEl.textContent).toContain("liquefy · smock");

    const tiles = markEl.querySelectorAll(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
  });

  it("does not render a mark row when paired but no mark is known yet", () => {
    const dump = observingDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(byId(ids["settings.pairing.mark"])).toBeNull();
  });

  it("does not render a mark row when not paired", () => {
    const dump = notPairedDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(byId(ids["settings.pairing.mark"])).toBeNull();
  });

  it("carries one role=img aria-label announcement, with every inner node hidden from the tree", () => {
    const dump = pairedWithMarkDump(sampleMarkSpec("liquefy", "smock", "#3b82f6"));
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    // The mark row's automation id is stamped directly on the chip container
    // (renderMarkChip's return value), so `markEl` here IS the role="img" node.
    const markEl = present(ids["settings.pairing.mark"]);
    expect(markEl.classList.contains("unknown-journal-chip")).toBe(true);
    expect(markEl.getAttribute("role")).toBe("img");
    expect(markEl.getAttribute("aria-label")).toBe("mark: blue, purple, liquefy, smock");

    const tiles = markEl.querySelectorAll(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
    for (const tile of Array.from(tiles)) {
      expect(tile.getAttribute("aria-hidden")).toBe("true");
    }

    const words = markEl.querySelector("span");
    expect(words?.getAttribute("aria-hidden")).toBe("true");
  });
});
