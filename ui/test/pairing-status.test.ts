// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { notPairedDump, sampleMarkSpec } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

const PAIR_LINK =
  "that pairing link isn't one the solstone app can read. show a new pairing code on your journal and try again.";
const WINDOW_CLOSED =
  "the pairing window closed. show a new pairing code on your journal, then try again.";
const NO_NETWORK =
  "this device isn't on a network. pairing needs to reach your journal directly, so join the same wi-fi as your journal and try again. everything the solstone app has taken in is on this device and syncs once you reconnect.";
const GENERIC =
  "pairing didn't go through. show a new pairing code on your journal and try again.";
const JOURNAL_REFUSED =
  "this PC can't connect to your journal with its saved pairing. show a new pairing code on your journal and pair again.";
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
      { detail: "journal_refused", expected: JOURNAL_REFUSED },
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

describe("awaiting mark confirmation", () => {
  beforeEach(() => {
    resetRoot();
  });

  function awaitingConfirmationDump(mark?: ReturnType<typeof sampleMarkSpec> | null) {
    const base = notPairedDump();
    return {
      ...base,
      sync: {
        ...base.sync,
        pairing: {
          ...base.sync.pairing,
          phase: "awaiting_confirmation" as const,
          journal_label: "my-journal",
          detail: null,
          mark: mark !== undefined ? mark : null,
          binding: "abc123binding",
        },
      },
    };
  }

  it("paints waiting for you to confirm your journal's mark on all four pairing-status sites", () => {
    const dump = awaitingConfirmationDump(null);

    // Home route
    resetRoot();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const homeGlance = homeJournalCardGlance();
    expect(homeGlance).toBe("waiting for you to confirm your journal's mark");
    expect(homeGlance).not.toContain("paired with");

    const homeStrip = homeStatusStripJournalValue();
    expect(homeStrip).toBe("waiting for you to confirm your journal's mark");
    expect(homeStrip).not.toContain("paired with");

    // Journal route
    resetRoot();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const paneStatus = present(ids["settings.pairing.state"]).textContent ?? "";
    expect(paneStatus).toBe("waiting for you to confirm your journal's mark");
    expect(paneStatus).not.toContain("paired with");

    const syncStatus = present(ids["settings.status.upload.state"]).textContent ?? "";
    expect(syncStatus).toBe("waiting for you to confirm your journal's mark");
    expect(syncStatus).not.toContain("paired with");
  });

  it("renders mark confirmation card when mark is present with accessible name, focus, and non-default buttons", () => {
    const mark = sampleMarkSpec("liquefy", "smock", "#3b82f6");
    mark.icon1.color.name = "Blue";
    mark.icon2.color.name = "Amber";
    const dump = awaitingConfirmationDump(mark);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const card = present(ids["settings.pairing.markCard"]);
    expect(card.getAttribute("role")).toBe("img");
    expect(card.getAttribute("tabindex")).toBe("-1");
    expect(card.getAttribute("aria-label")).toBe("blue, amber · liquefy smock");
    expect(document.activeElement).toBe(card);

    expect(card.parentElement?.textContent).toContain("does this match your journal?");
    expect(card.parentElement?.textContent).toContain(
      "your journal shows this same mark in its network app. it should match, exactly.",
    );

    const rejectBtn = present(ids["settings.pairing.confirmReject"]) as HTMLButtonElement;
    expect(rejectBtn.textContent).toBe("that doesn't match");
    expect(rejectBtn.getAttribute("type")).toBe("button");
    expect(rejectBtn.type).toBe("button");
    expect(rejectBtn.classList.contains("fluent-control")).toBe(true);

    const yesBtn = present(ids["settings.pairing.confirmYes"]) as HTMLButtonElement;
    expect(yesBtn.textContent).toBe("yes, this is my journal");
    expect(yesBtn.getAttribute("type")).toBe("button");
    expect(yesBtn.type).toBe("button");
    expect(yesBtn.classList.contains("fluent-accent")).toBe(true);

    expect(card.contains(rejectBtn)).toBe(false);
    expect(card.contains(yesBtn)).toBe(false);
    expect(rejectBtn.getAttribute("aria-hidden")).toBeNull();
    expect(yesBtn.getAttribute("aria-hidden")).toBeNull();

    expect(byId(ids["settings.pairing.confirmContinue"])).toBeNull();
    expect(byId(ids["settings.pairing.confirmCancel"])).toBeNull();
  });

  it("renders unreadable mark card when mark is absent with accessible name and non-default buttons", () => {
    const dump = awaitingConfirmationDump(null);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const card = present(ids["settings.pairing.markCard"]);
    expect(card.getAttribute("role")).toBe("img");
    expect(card.getAttribute("tabindex")).toBe("-1");
    expect(card.getAttribute("aria-label")).toBe("your journal's mark, unavailable right now");
    expect(document.activeElement).toBe(card);

    expect(card.parentElement?.textContent).toContain("couldn't verify");
    expect(card.parentElement?.textContent).toContain(
      "this PC couldn't work out your journal's mark, so there's nothing to compare. continue only if you're sure the link came from your journal.",
    );

    const cancelBtn = present(ids["settings.pairing.confirmCancel"]) as HTMLButtonElement;
    expect(cancelBtn.textContent).toBe("cancel pairing");
    expect(cancelBtn.getAttribute("type")).toBe("button");
    expect(cancelBtn.type).toBe("button");
    expect(cancelBtn.classList.contains("fluent-control")).toBe(true);

    const continueBtn = present(ids["settings.pairing.confirmContinue"]) as HTMLButtonElement;
    expect(continueBtn.textContent).toBe("continue anyway");
    expect(continueBtn.getAttribute("type")).toBe("button");
    expect(continueBtn.type).toBe("button");
    expect(continueBtn.classList.contains("fluent-accent")).toBe(true);

    expect(card.contains(cancelBtn)).toBe(false);
    expect(card.contains(continueBtn)).toBe(false);
    expect(cancelBtn.getAttribute("aria-hidden")).toBeNull();
    expect(continueBtn.getAttribute("aria-hidden")).toBeNull();

    expect(byId(ids["settings.pairing.confirmYes"])).toBeNull();
    expect(byId(ids["settings.pairing.confirmReject"])).toBeNull();
  });

  it("does not render pairing input while awaiting and enter key does not invoke pair or answer", () => {
    invokeMock.mockReset();
    const dump = awaitingConfirmationDump(null);
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(byId(ids["settings.pairing.input"])).toBeNull();

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    const pairCalls = invokeMock.mock.calls.filter(
      ([cmd]) => cmd === "pair" || cmd === "answer_pairing",
    );
    expect(pairCalls.length).toBe(0);
  });

  it("retains focus on subsequent renders when focus was moved away from mark card", () => {
    const mark = sampleMarkSpec("liquefy", "smock", "#3b82f6");
    const dump = awaitingConfirmationDump(mark);

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const card = present(ids["settings.pairing.markCard"]);
    expect(document.activeElement).toBe(card);

    document.body.tabIndex = -1;
    document.body.focus();
    expect(document.activeElement).toBe(document.body);

    app.__test__.renderSettings(dump);
    expect(document.activeElement).toBe(document.body);
  });

  it("renders not paired for not_paired phase even with pair_link detail", () => {
    const base = notPairedDump();
    const dump = {
      ...base,
      sync: {
        ...base.sync,
        pairing: {
          ...base.sync.pairing,
          phase: "not_paired" as const,
          detail: "pair_link",
        },
      },
    };

    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const text = present(ids["settings.pairing.state"]).textContent ?? "";
    expect(text).toBe("not paired");
  });
});
