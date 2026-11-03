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

  it("renders two sightings, each with its own body, marks and captions", () => {
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
    expect(summaries[0].textContent).toContain("192.168.1.120:443");
    expect(summaries[1].textContent).not.toContain("192.168.1.120:443");

    const details = document.querySelectorAll(`[data-automation-id="${ids["settings.unknown-journal.detail"]}"]`);
    expect(details.length).toBe(2);
    expect(details[0].querySelector(".unknown-journal-heading")).not.toBeNull();
    expect(details[1].querySelector(".unknown-journal-heading")).not.toBeNull();

    // A direct sighting names its address. The relay has none, so its body is its own.
    const directBody = details[0].querySelector(".unknown-journal-body")?.textContent ?? "";
    const relayBody = details[1].querySelector(".unknown-journal-body")?.textContent ?? "";
    expect(directBody).toContain("192.168.1.120:443");
    expect(relayBody).not.toContain("192.168.1.120:443");
    expect(relayBody).not.toBe("");
    expect(relayBody).not.toBe(directBody);

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

    // Marks different (words)
    expect(whatAnswereds[0].textContent).toContain("distrust");
    expect(whatAnswereds[0].textContent).toContain("chokehold");
    expect(yourJournals[0].textContent).toContain("liquefy");
    expect(yourJournals[0].textContent).toContain("smock");

    // A claimed mark draws both chips whole, the rotated one rotated as one piece.
    const claimedChips = whatAnswereds[0].querySelectorAll(".mark-chip");
    expect(claimedChips.length).toBe(2);
    expect(claimedChips[0].classList.contains("rotated")).toBe(true);
    expect(claimedChips[0].querySelector("svg")?.getAttribute("viewBox")).toBe("0 0 24 24");

    // A stranger that presented nothing is never announced as the owner's journal.
    const noIdentity = whatAnswereds[1].querySelector(".mark-card");
    expect(noIdentity?.getAttribute("role")).toBe("img");
    expect(noIdentity?.getAttribute("aria-label")).toBe(captions[1].textContent);
    expect(whatAnswereds[1].textContent).not.toContain("?");
    const noIdentityChips = whatAnswereds[1].querySelectorAll(".mark-chip");
    expect(noIdentityChips.length).toBe(2);
    for (const chip of Array.from(noIdentityChips)) {
      expect(chip.classList.contains("dashed")).toBe(true);
      expect(chip.classList.contains("rotated")).toBe(false);
      expect(chip.querySelector("svg")).toBeNull();
    }
    // Its own form, not the owner's "no identity yet" one.
    expect(noIdentity?.getAttribute("aria-label")).not.toContain("your journal");
  });

  it("leaves pairing labels untouched on journal route", () => {
    const dump = unknownJournalsDump([
      {
        address: "192.168.1.120:443",
        expected_mark: sampleMarkSpec("liquefy", "smock", "#3b82f6"),
        responding_mark: sampleMarkSpec("distrust", "chokehold", "#ec4899"),
      },
    ]);
    dump.sync.pairing.journal_label = "label-sent-by-the-journal";

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const pairedEl = present(ids["settings.pairing.state"]);
    expect(pairedEl.textContent).not.toContain("connected");

    const syncSummaryEl = present(ids["settings.status.upload.state"]);
    expect(syncSummaryEl.textContent).toContain("delivered");

    expect(document.body.textContent).not.toContain("label-sent-by-the-journal");
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
