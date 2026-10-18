// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { notPairedDump, observingDump, pairedWithMarkDump, sampleMarkSpec } from "./fixtures";

const invokeMock = vi.mocked(invoke);
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

  it("renders an unavailable mark row when paired but no mark is known yet", () => {
    const dump = observingDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const markEl = present(ids["settings.pairing.mark"]);
    expect(markEl.textContent).toContain("?");

    const tiles = markEl.querySelectorAll<HTMLElement>(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
    for (const tile of Array.from(tiles)) {
      expect(tile.style.border).not.toContain("dashed");
      expect(tile.style.background).toMatch(/(#6e6453|rgb\(110, 100, 83\))/i);
    }
  });

  it("renders a generic mark row when not paired", () => {
    const dump = notPairedDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const markEl = present(ids["settings.pairing.mark"]);
    expect(markEl.textContent).not.toContain("?");

    const tiles = markEl.querySelectorAll<HTMLElement>(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
    for (const tile of Array.from(tiles)) {
      expect(tile.style.border).toContain("2px dashed");
    }
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
    expect(markEl.getAttribute("aria-label")).toBe("blue, purple, liquefy, smock");

    const tiles = markEl.querySelectorAll(".unknown-journal-mark-tile");
    expect(tiles.length).toBe(2);
    for (const tile of Array.from(tiles)) {
      expect(tile.getAttribute("aria-hidden")).toBe("true");
    }

    const words = markEl.querySelector("span");
    expect(words?.getAttribute("aria-hidden")).toBe("true");
  });
});

describe("setup handoff and finish connecting in pairing", () => {
  beforeEach(() => {
    resetRoot();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined as never);
  });

  it("finish-connecting shows settings.pairing.finishConnecting and not the mark card", () => {
    const dump = {
      ...notPairedDump(),
      setup: { sequence_enabled: true, mic: false, screen: false, system_audio: false, start_at_login: false, wizard: "done" as const, notice_dismissed: true },
      sync: {
        ...notPairedDump().sync,
        pairing: {
          phase: "awaiting_confirmation" as const,
          journal_label: "my-journal",
          detail: null,
          finish_connecting: true,
          binding: "abc",
        },
      },
    };
    app.__test__.setRoute("journal");
    app.__test__.renderSettings(dump);

    expect(present(ids["settings.pairing.finishConnecting"])).not.toBeNull();
    expect(byId(ids["settings.pairing.markCard"])).toBeNull();
  });

  it("ordinary awaiting confirmation still shows the mark card", () => {
    const dump = {
      ...notPairedDump(),
      sync: {
        ...notPairedDump().sync,
        pairing: {
          phase: "awaiting_confirmation" as const,
          journal_label: "my-journal",
          detail: null,
          finish_connecting: false,
          binding: "abc",
        },
      },
    };
    app.__test__.setRoute("journal");
    app.__test__.renderSettings(dump);

    expect(present(ids["settings.pairing.markCard"])).not.toBeNull();
    expect(byId(ids["settings.pairing.finishConnecting"])).toBeNull();
  });

  it("fresh-link click does not invoke pair or open_setup_journal", () => {
    app.__test__.setRoute("journal");
    app.__test__.renderSettings(notPairedDump());

    const freshLink = present(ids["settings.setup.freshLink"]);
    expect(freshLink.tagName).not.toBe("BUTTON");
    expect(byId(ids["settings.pairing.submit"])).not.toBeNull();

    freshLink.click();
    expect(invokeMock).not.toHaveBeenCalledWith("pair", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("open_setup_journal");
  });

  it("link-retry invokes retry_setup_link", () => {
    const dump = {
      ...notPairedDump(),
      setup: {
          sequence_enabled: true,
        ...notPairedDump().setup,
        mailbox_busy: true,
      },
    };
    app.__test__.setRoute("journal");
    app.__test__.renderSettings(dump);

    const retryBtn = present(ids["settings.setup.linkRetry"]);
    retryBtn.click();
    expect(invokeMock).toHaveBeenCalledWith("retry_setup_link");
  });

  it("browser control invokes open_setup_journal", () => {
    const dump = {
      ...notPairedDump(),
      setup: { sequence_enabled: true, mic: false, screen: false, system_audio: false, start_at_login: false, wizard: "done" as const, notice_dismissed: true },
      sync: {
        ...notPairedDump().sync,
        pairing: {
          phase: "paired" as const,
          journal_label: "my-journal",
          detail: null,
          setup_browser_ready: true,
          binding: "displayed-binding",
        },
      },
    };
    app.__test__.setRoute("journal");
    app.__test__.renderSettings(dump);

    const browserBtn = present(ids["settings.setup.browserJournal"]);
    browserBtn.click();
    expect(invokeMock).toHaveBeenCalledWith("open_setup_journal", { binding: "displayed-binding" });
  });
});
