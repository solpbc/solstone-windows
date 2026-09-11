// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { notPairedDump } from "./fixtures";

const ids = automationContract.automation_ids;

const PAIR_LINK =
  "that pairing link isn't one the solstone app can read. show a new pairing code on your journal and try again.";
const WINDOW_CLOSED =
  "the pairing window closed. show a new pairing code on your journal, then try again.";
const NO_NETWORK =
  "this device isn't on a network. pairing needs to reach your journal directly, so join the same wi-fi as your journal and try again. everything the solstone app has taken in is on this device and syncs once you reconnect.";
const GENERIC =
  "pairing didn't go through. show a new pairing code on your journal and try again.";
const PRIVATE_NETWORK_UNAVAILABLE =
  "couldn't reach your journal over your private network. join the same wi-fi as your journal, then try again with a new pairing code on your journal.";

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

function failedDump(detail?: string | null) {
  const base = notPairedDump();
  return {
    ...base,
    sync: {
      ...base.sync,
      pairing: {
        ...base.sync.pairing,
        phase: "failed" as const,
        detail: detail !== undefined ? detail : null,
      },
    },
  };
}

function homeJournalCardGlance(): string {
  const cards = Array.from(document.querySelectorAll<HTMLElement>(".settings-card"));
  const card = cards.find(
    (c) => c.querySelector(".settings-card-title")?.textContent === "journal",
  );
  expect(card).toBeDefined();
  const glance = card?.querySelector<HTMLElement>(".settings-card-glance");
  expect(glance).not.toBeNull();
  return glance?.textContent ?? "";
}

function homeStatusStripJournalValue(): string {
  const buttons = Array.from(
    document.querySelectorAll<HTMLButtonElement>(".settings-status-button"),
  );
  const button = buttons.find(
    (b) => b.querySelector(".settings-status-label")?.textContent === "journal",
  );
  expect(button).toBeDefined();
  const value = button?.querySelector<HTMLElement>(".settings-status-value");
  expect(value).not.toBeNull();
  return value?.textContent ?? "";
}

function assertOldPrimaryFormGone(text: string, detail?: string | null): void {
  expect(text).not.toBe("pairing failed");
  if (detail !== undefined && detail !== null && detail !== "") {
    expect(text).not.toBe(`pairing failed: ${detail}`);
    expect(text).not.toBe(detail);
  } else if (detail === "") {
    expect(text).not.toBe("pairing failed: ");
  }
}

describe("failed pairing status sentences", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("discriminates named classes and generic on the pairing pane", () => {
    expect(
      new Set([PAIR_LINK, WINDOW_CLOSED, NO_NETWORK, PRIVATE_NETWORK_UNAVAILABLE, GENERIC])
        .size,
    ).toBe(5);

    const cases = [
      { detail: "pair_link", expected: PAIR_LINK },
      { detail: "relay_pair_window_closed", expected: WINDOW_CLOSED },
      { detail: "io", expected: NO_NETWORK },
      { detail: "relay_unpaid", expected: PRIVATE_NETWORK_UNAVAILABLE },
      { detail: "http_403", expected: GENERIC },
    ];

    for (const { detail, expected } of cases) {
      resetRoot();
      const dump = failedDump(detail);
      app.__test__.setRoute("journal");
      app.__test__.setHealth(dump);
      app.__test__.renderSettings(dump);

      const text = present(ids["settings.pairing.state"]).textContent ?? "";
      expect(text).toBe(expected);
      assertOldPrimaryFormGone(text, detail);
    }
  });

  it("maps catch-all codes, extra tokens, missing, empty, and unknown details to generic", () => {
    const frozenCodes = [
      "tls",
      "crypto",
      "mux",
      "http",
      "json",
      "pairing",
      "ingest",
      "http_503",
      "relay_home_offline",
      "relay_unauthorized",
      "relay_unknown_instance",
      "relay_overflow",
      "relay_abnormal",
      "relay_upgrade_rejected",
      "relay_stalled",
      "relay_enroll_device_http_409",
      "relay_refresh_http_404",
      "no_endpoint",
      "not_paired",
      "local_offset",
    ];

    const extraCodes = [
      "http_404",
      "relay_enroll_device_http_500",
      null,
      "",
      "something_new",
    ];

    const allCatchAll = [...frozenCodes, ...extraCodes];

    for (const detail of allCatchAll) {
      resetRoot();
      const dump = failedDump(detail);
      app.__test__.setRoute("journal");
      app.__test__.setHealth(dump);
      app.__test__.renderSettings(dump);

      const text = present(ids["settings.pairing.state"]).textContent ?? "";
      expect(text).toBe(GENERIC);
      if (
        detail === "tls" ||
        detail === "no_endpoint" ||
        detail === "relay_home_offline"
      ) {
        expect(text).not.toBe(NO_NETWORK);
      }
      assertOldPrimaryFormGone(text, detail);
    }
  });

  it("paints named classes on all four pairing-status sites", () => {
    const namedCases = [
      { detail: "io", expected: NO_NETWORK },
      { detail: "pair_link", expected: PAIR_LINK },
      { detail: "relay_unpaid", expected: PRIVATE_NETWORK_UNAVAILABLE },
    ];

    for (const { detail, expected } of namedCases) {
      // Home route
      resetRoot();
      const dumpHome = failedDump(detail);
      app.__test__.setRoute("home");
      app.__test__.setHealth(dumpHome);
      app.__test__.renderSettings(dumpHome);

      const glance = homeJournalCardGlance();
      expect(glance).toBe(expected);
      expect(glance).not.toBe(GENERIC);
      assertOldPrimaryFormGone(glance, detail);

      const stripVal = homeStatusStripJournalValue();
      expect(stripVal).toBe(expected);
      expect(stripVal).not.toBe(GENERIC);
      assertOldPrimaryFormGone(stripVal, detail);

      // Journal route
      resetRoot();
      const dumpJournal = failedDump(detail);
      app.__test__.setRoute("journal");
      app.__test__.setHealth(dumpJournal);
      app.__test__.renderSettings(dumpJournal);

      const paneStatus = present(ids["settings.pairing.state"]).textContent ?? "";
      expect(paneStatus).toBe(expected);
      expect(paneStatus).not.toBe(GENERIC);
      assertOldPrimaryFormGone(paneStatus, detail);

      const syncStatus = present(ids["settings.status.upload.state"]).textContent ?? "";
      expect(syncStatus).toBe(expected);
      expect(syncStatus).not.toBe(GENERIC);
      assertOldPrimaryFormGone(syncStatus, detail);
    }
  });

  it("paints the generic sentence on all four pairing-status sites", () => {
    const genericCases: Array<string | null> = ["tls", null, "http_403"];

    for (const detail of genericCases) {
      // Home route
      resetRoot();
      const dumpHome = failedDump(detail);
      app.__test__.setRoute("home");
      app.__test__.setHealth(dumpHome);
      app.__test__.renderSettings(dumpHome);

      const glance = homeJournalCardGlance();
      expect(glance).toBe(GENERIC);
      expect(glance).not.toBe(NO_NETWORK);
      assertOldPrimaryFormGone(glance, detail);

      const stripVal = homeStatusStripJournalValue();
      expect(stripVal).toBe(GENERIC);
      expect(stripVal).not.toBe(NO_NETWORK);
      assertOldPrimaryFormGone(stripVal, detail);

      // Journal route
      resetRoot();
      const dumpJournal = failedDump(detail);
      app.__test__.setRoute("journal");
      app.__test__.setHealth(dumpJournal);
      app.__test__.renderSettings(dumpJournal);

      const paneStatus = present(ids["settings.pairing.state"]).textContent ?? "";
      expect(paneStatus).toBe(GENERIC);
      expect(paneStatus).not.toBe(NO_NETWORK);
      assertOldPrimaryFormGone(paneStatus, detail);

      const syncStatus = present(ids["settings.status.upload.state"]).textContent ?? "";
      expect(syncStatus).toBe(GENERIC);
      expect(syncStatus).not.toBe(NO_NETWORK);
      assertOldPrimaryFormGone(syncStatus, detail);
    }
  });

  it("adjacent pairing copy is not a register sentence", () => {
    const dump = failedDump("http_403");
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const lockedSentences = [
      PAIR_LINK,
      WINDOW_CLOSED,
      NO_NETWORK,
      PRIVATE_NETWORK_UNAVAILABLE,
      GENERIC,
    ];

    const journalLabelText = present(ids["settings.pairing.journal"]).textContent ?? "";
    for (const s of lockedSentences) {
      expect(journalLabelText).not.toBe(s);
    }
    expect(journalLabelText).not.toBe("pairing failed");
    expect(journalLabelText).not.toBe("pairing failed: http_403");

    const unavailableText = present(ids["settings.journal.unavailable"]).textContent ?? "";
    for (const s of lockedSentences) {
      expect(unavailableText).not.toBe(s);
    }
    expect(unavailableText).not.toBe("pairing failed");
    expect(unavailableText).not.toBe("pairing failed: http_403");
  });

  it.each(["http_403", "relay_unpaid"] as const)(
    "failed pairing keeps one pair control per route for %s",
    (detail) => {
      resetRoot();
      const dumpHome = failedDump(detail);
      app.__test__.setRoute("home");
      app.__test__.setHealth(dumpHome);
      app.__test__.renderSettings(dumpHome);

      const homeButtons = Array.from(document.querySelectorAll("button"));
      const homePairButtons = homeButtons.filter((b) => b.textContent === "pair");
      expect(homePairButtons.length).toBe(1);

      resetRoot();
      const dumpJournal = failedDump(detail);
      app.__test__.setRoute("journal");
      app.__test__.setHealth(dumpJournal);
      app.__test__.renderSettings(dumpJournal);

      const journalButtons = Array.from(document.querySelectorAll("button"));
      const journalPairButtons = journalButtons.filter((b) => b.textContent === "pair");
      expect(journalPairButtons.length).toBe(1);
      expect(present(ids["settings.pairing.submit"])).not.toBeNull();
      expect(present(ids["settings.pairing.input"])).not.toBeNull();
    },
  );
});
