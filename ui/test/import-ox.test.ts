// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import {
  counterFacts,
  importTileView,
  isValidImportId,
  journalPill,
  journalPillVisual,
  sortImportRows,
  statusLead,
  type CounterDocument,
  type JournalPillInput,
} from "../src/shell/derive";
import { observingDump, sampleMarkSpec } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

type ImportDoc = NonNullable<Parameters<typeof app.__test__.setImport>[0]>;
type ImportRow = ImportDoc["rows"][number];

const caughtUp: JournalPillInput = {
  paired: true,
  needsNewLink: false,
  refused: false,
  syncPaused: false,
  held: false,
  offline: false,
  connecting: false,
  waitingToSync: false,
  waiting: 0,
  sourceNeedsAttention: false,
  importsNeedOwner: false,
  importsReadyToReview: 0,
};

function deskDump(overrides: {
  phase?: "paired" | "not_paired" | "awaiting_confirmation" | "failed" | "pairing";
  link?: "connected" | "connecting" | "offline" | null;
  imports_waiting?: number;
  imports_ready_to_review?: number;
  imports_need_owner?: boolean;
} = {}) {
  const base = observingDump();
  const phase = overrides.phase ?? "paired";
  return {
    ...base,
    app_state: "idle",
    sync: {
      ...base.sync,
      pairing: {
        ...base.sync.pairing,
        phase,
        mark: phase === "not_paired" ? null : sampleMarkSpec("amber", "pine", "#d97706"),
      },
      journal_link: overrides.link !== undefined ? overrides.link : phase === "paired" ? "connected" : null,
      imports_waiting: overrides.imports_waiting ?? 0,
      imports_ready_to_review: overrides.imports_ready_to_review ?? 0,
      imports_need_owner: overrides.imports_need_owner ?? false,
    },
    setup: {
      sequence_enabled: true,
      mic: false,
      system_audio: false,
      screen: false,
      start_at_login: false,
      wizard: "done",
      notice_dismissed: true,
      desktop_activity: false,
      desktop_activity_allowed: true,
      desktop_activity_through_pause: false,
      desktop_activity_window_titles: true,
      desktop_activity_page_addresses: true,
    },
  } as typeof base;
}

function row(overrides: Partial<ImportRow> & { id: string; name: string; state: ImportRow["state"] }): ImportRow {
  return {
    reason: null,
    retryable: false,
    may_have_reached: false,
    missing_copy: false,
    import_id: null,
    reason_code: null,
    review_kind: null,
    review_expired: false,
    added: 1_700_000_000_000,
    size: 1024,
    ...overrides,
  };
}

function importDoc(overrides: Partial<ImportDoc> = {}): ImportDoc {
  return {
    recent: "nothing yet.",
    recent_receipts: [],
    hold_reason: null,
    rows: [],
    intake: null,
    interrupted: false,
    waiting: 0,
    ready_to_review: 0,
    needs_owner: false,
    available: true,
    ...overrides,
  };
}

function byId(id: string): HTMLElement | null {
  return document.querySelector(`[data-automation-id="${id}"]`);
}

describe("import OX pure derivations", () => {
  it("status pill visual and label honor waiting > attention > ready_to_review > connected", () => {
    expect(journalPill(caughtUp)).toBe("connected");
    expect(journalPillVisual(caughtUp)).toBe("connected");

    const oneReview = { ...caughtUp, importsReadyToReview: 1 };
    expect(journalPill(oneReview)).toBe("1 ready to review");
    expect(journalPillVisual(oneReview)).toBe("review");

    const threeReview = { ...caughtUp, importsReadyToReview: 3 };
    expect(journalPill(threeReview)).toBe("3 ready to review");
    expect(journalPillVisual(threeReview)).toBe("review");

    // Fault with waiting == 0: never green connected, no fault text rollup on pill.
    const faultOnly = { ...caughtUp, importsNeedOwner: true, waiting: 0 };
    expect(journalPill(faultOnly)).toBe("connected");
    expect(journalPillVisual(faultOnly)).toBe("neutral");

    // Fault beats ready_to_review.
    const faultAndReview = { ...caughtUp, importsNeedOwner: true, importsReadyToReview: 2 };
    expect(journalPill(faultAndReview)).toBe("connected");
    expect(journalPillVisual(faultAndReview)).toBe("neutral");

    // Waiting to sync beats ready_to_review.
    const syncingAndReview = { ...caughtUp, waitingToSync: true, waiting: 1, importsReadyToReview: 2 };
    expect(journalPill(syncingAndReview)).toBe("waiting to sync");
    expect(journalPillVisual(syncingAndReview)).toBe("neutral");
  });

  it("status pane lead distinguishes caught up, ready to review, and imports need you", () => {
    expect(statusLead(caughtUp)).toEqual({
      headline: "all caught up",
      lines: ["everything is in your journal"],
    });
    expect(statusLead({ ...caughtUp, importsReadyToReview: 1 })).toEqual({
      headline: "1 import ready to review",
      lines: ["open import to review and start."],
    });
    expect(statusLead({ ...caughtUp, importsReadyToReview: 3 })).toEqual({
      headline: "3 imports ready to review",
      lines: ["open import to review and start."],
    });
    expect(statusLead({ ...caughtUp, importsNeedOwner: true, importsReadyToReview: 2 })).toEqual({
      headline: "imports need you",
      lines: ["open import to see why."],
    });
  });

  it("home import tile stays quiet when idle and applies worst-signal-wins when active", () => {
    expect(
      importTileView({ rows: [], waitingForJournal: false, needsAttention: false }),
    ).toEqual({ word: "", sub: "files" });

    expect(
      importTileView({
        rows: [{ name: "notes.pdf", state: "waiting" }],
        waitingForJournal: false,
        needsAttention: false,
      }),
    ).toEqual({
      word: "waiting",
      sub: "1 waiting",
      ariaLabel: "import, waiting. 1 waiting.",
    });

    expect(
      importTileView({
        rows: [{ name: "notes.pdf", state: "waiting" }],
        waitingForJournal: true,
        needsAttention: false,
      }),
    ).toEqual({
      word: "waiting",
      sub: "waiting for your journal",
      ariaLabel: "import, waiting. waiting for your journal.",
    });

    expect(
      importTileView({
        rows: [{ name: "chatgpt-export.zip", state: "sending" }],
        waitingForJournal: false,
        needsAttention: false,
      }),
    ).toEqual({
      word: "sending",
      sub: "chatgpt-export.zip",
      ariaLabel: "import, sending. chatgpt-export.zip.",
    });

    expect(
      importTileView({
        rows: [
          { name: "chatgpt-export.zip", state: "sending" },
          { name: "a.pdf", state: "waiting" },
          { name: "b.pdf", state: "waiting" },
        ],
        waitingForJournal: false,
        needsAttention: false,
      }),
    ).toEqual({
      word: "sending",
      sub: "3 files",
      ariaLabel: "import, sending. 3 files.",
    });

    expect(
      importTileView({
        rows: [
          { name: "export.zip", state: "ready_to_review" },
          { name: "a.pdf", state: "sending" },
        ],
        waitingForJournal: false,
        needsAttention: false,
      }),
    ).toEqual({
      word: "ready to review",
      sub: "export.zip",
      ariaLabel: "import, ready to review. export.zip.",
    });

    expect(
      importTileView({
        rows: [
          { name: "export.zip", state: "ready_to_review" },
          { name: "evernote.enex", state: "ready_to_review" },
        ],
        waitingForJournal: false,
        needsAttention: false,
      }),
    ).toEqual({
      word: "ready to review",
      sub: "2 ready in your journal",
      ariaLabel: "import, ready to review. 2 ready in your journal.",
    });

    expect(
      importTileView({
        rows: [
          { name: "holiday.mov", state: "refused" },
          { name: "export.zip", state: "ready_to_review" },
          { name: "a.pdf", state: "sending" },
        ],
        waitingForJournal: false,
        needsAttention: true,
      }),
    ).toEqual({
      word: "needs attention",
      sub: "holiday.mov",
      ariaLabel: "import, needs attention. holiday.mov.",
    });

    expect(
      importTileView({
        rows: [
          { name: "holiday.mov", state: "refused" },
          { name: "export.zip", state: "review_expired" },
        ],
        waitingForJournal: false,
        needsAttention: true,
      }),
    ).toEqual({
      word: "needs attention",
      sub: "2 items",
      ariaLabel: "import, needs attention. 2 items.",
    });
  });

  it("validates strict ASCII import ids and rejects traversal or Unicode digits", () => {
    expect(isValidImportId("20261122_143000")).toBe(true);
    expect(isValidImportId("20261122_14300")).toBe(false);
    expect(isValidImportId("20261122-143000")).toBe(false);
    expect(isValidImportId("../20261122_143000")).toBe(false);
    expect(isValidImportId("٢٠٢٦١١٢٢_١٤٣٠٠٠")).toBe(false);
    expect(isValidImportId(null)).toBe(false);
    expect(isValidImportId(undefined)).toBe(false);
  });

  it("sorts import rows deterministically by state rank and newest-first timestamp", () => {
    const sorted = sortImportRows([
      { id: "0001", state: "waiting", added: 100 },
      { id: "ffff", state: "waiting", added: 500 },
      { id: "aaaa", state: "sending", added: 200 },
      { id: "0002", state: "refused", added: 300 },
      { id: "bbbb", state: "refused", added: 900 },
      { id: "cccc", state: "review_expired", added: 400 },
      { id: "dddd", state: "ready_to_review", added: 600 },
    ]);
    expect(sorted.map((r) => r.id)).toEqual([
      "dddd", // ready_to_review
      "cccc", // review_expired
      "bbbb", // refused (newest)
      "0002", // refused (older)
      "aaaa", // sending
      "ffff", // waiting (newest)
      "0001", // waiting (older)
    ]);
  });

  it("includes upgrade-safe import counter facts", () => {
    const legacy: CounterDocument = {
      started: "2026-11-09",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
    };
    const legacyMap = Object.fromEntries(counterFacts(legacy)!.map((f) => [f.source, f]));
    expect(legacyMap.import.nothingYet).toBe(true);

    const withImports: CounterDocument = {
      ...legacy,
      imports: { total: 4, last: 1_700_000_000 },
    };
    const nextMap = Object.fromEntries(counterFacts(withImports)!.map((f) => [f.source, f]));
    expect(nextMap.import).toMatchObject({
      nothingYet: false,
      total: 4,
      last: 1_700_000_000,
    });
  });
});

describe("import OX WebView2 UI, focus, and accessibility", () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="app"></div>';
    vi.clearAllMocks();
    app.__test__.reset();
    app.__test__.setRoot(document.querySelector("#app") as HTMLDivElement);
    app.__test__.setLabel("settings");
    Object.defineProperty(window, "innerWidth", { value: 1180, writable: true, configurable: true });
    invokeMock.mockResolvedValue(null);
  });

  it("renders main landmark, semantic lists, ready-to-review / expired / device / recent sections, and counters", async () => {
    const dump = deskDump({ imports_ready_to_review: 1, imports_need_owner: false });
    const nowMs = Date.now();
    const doc = importDoc({
      ready_to_review: 1,
      waiting: 2,
      needs_owner: true,
      rows: [
        row({
          id: "rev1",
          name: "export.zip",
          state: "ready_to_review",
          import_id: "20261122_143000",
          review_kind: "body_confirmation_required",
          reason: "saved in your journal. review within an hour before your journal removes the waiting copy.",
          added: 600,
        }),
        row({
          id: "exp1",
          name: "old-export.zip",
          state: "review_expired",
          import_id: "20261122_120000",
          review_expired: true,
          retryable: true,
          reason: "the waiting copy in your journal expired. this device still holds the file.",
          added: 500,
        }),
        row({
          id: "ref-retry",
          name: "retryable.pdf",
          state: "refused",
          retryable: true,
          reason: "your journal is busy. try again.",
          added: 400,
        }),
        row({
          id: "ref-plan",
          name: "broken.zip",
          state: "refused",
          retryable: false,
          reason_code: "import_preview_required",
          review_kind: "plan_failed",
          reason: "your journal couldn't prepare a preview for this file.",
          added: 300,
        }),
        row({
          id: "send1",
          name: "sending.pdf",
          state: "sending",
          added: 200,
        }),
        row({
          id: "wait1",
          name: "queued.txt",
          state: "waiting",
          added: 100,
        }),
      ],
      recent_receipts: [
        {
          filename: "journal-note.md",
          import_id: "20261122_140000",
          instance_id: "j-1",
          delivered_at_epoch_ms: nowMs - 240_000,
        },
        {
          filename: "unlinked-legacy.txt",
          import_id: null,
          instance_id: "j-1",
          delivered_at_epoch_ms: nowMs - 600_000,
        },
      ],
    });
    app.__test__.setHealth(dump);
    app.__test__.setImport(doc);
    app.__test__.setCounters({
      started: "2026-11-01",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
      imports: { total: 7, last: Math.floor(nowMs / 1000) - 240 },
    });
    app.__test__.openDeskSource("import");

    // Semantic <main> landmark for deck.
    expect(document.querySelector("main.desk-deck")).not.toBeNull();

    // Intro copy verbatim.
    expect(document.querySelector(".desk-import-page .desk-breath")?.textContent).toBe(
      "adds any file you choose here or drop on this window. your journal works out what it is.",
    );

    // Paired & confirmed shows more imports, helper, and all import history.
    expect(byId(ids["settings.import.more"])).not.toBeNull();
    expect(document.querySelector(".desk-import-helper")?.textContent).toBe(
      "guides, options, and import history in your journal",
    );
    expect(byId(ids["settings.import.allHistory"])?.textContent).toContain(
      "all import history in your journal",
    );

    // Review section uses <ul>/<li> and has NO `try now` button.
    const reviewSection = byId(ids["settings.import.reviewSection"])!;
    expect(reviewSection).not.toBeNull();
    expect(reviewSection.querySelector("ul.desk-import-list")).not.toBeNull();
    const reviewItems = reviewSection.querySelectorAll("li.desk-import-item");
    expect(reviewItems.length).toBe(2);
    expect(reviewSection.textContent).not.toContain("try now");

    // Click review in your journal -> invokes import_open_journal_item with validated id.
    const reviewBtn = byId("settings.import.row.rev1.review") as HTMLButtonElement;
    expect(reviewBtn.textContent).toBe("review in your journal");
    expect(reviewBtn.getAttribute("aria-label")).toBe("review export.zip in your journal");
    reviewBtn.click();
    expect(invokeMock).toHaveBeenCalledWith("import_open_journal_item", { id: "20261122_143000" });

    // Click send again to review on expired row -> invokes import_send_again.
    const sendAgainBtn = byId("settings.import.row.exp1.sendAgain") as HTMLButtonElement;
    expect(sendAgainBtn.textContent).toBe("send again to review");
    expect(sendAgainBtn.getAttribute("aria-label")).toBe("send old-export.zip again to review");
    sendAgainBtn.click();
    expect(invokeMock).toHaveBeenCalledWith("import_send_again", { id: "exp1" });

    // Device section: retryable refused gets try now; non-retryable refused gets remove only; sending gets no buttons.
    const deviceSection = byId(ids["settings.import.deviceSection"])!;
    expect(deviceSection.querySelector("ul.desk-import-list")).not.toBeNull();
    expect(byId("settings.import.row.ref-retry.retry")).not.toBeNull();
    expect(byId("settings.import.row.ref-plan.retry")).toBeNull();
    expect(byId("settings.import.row.ref-plan.remove")).not.toBeNull();
    const sendingLi = deviceSection.querySelector('li[data-import-id="send1"]')!;
    expect(sendingLi.querySelectorAll("button").length).toBe(0);

    // Recent section: linked receipt opens detail; missing import_id renders calmly as non-link.
    const recentSection = byId(ids["settings.import.recentSection"])!;
    expect(recentSection.querySelector("ul.desk-import-recent-list")).not.toBeNull();
    const linkedReceipt = byId("settings.import.recent.20261122_140000") as HTMLButtonElement;
    expect(linkedReceipt).not.toBeNull();
    linkedReceipt.click();
    expect(invokeMock).toHaveBeenCalledWith("import_open_journal_item", { id: "20261122_140000" });
    expect(recentSection.querySelector(".desk-import-recent-row")?.textContent).toContain("unlinked-legacy.txt");

    // Counter facts at bottom render items unit.
    expect(document.querySelector(".desk-import-page .desk-facts")?.textContent).toContain("7 items");
  });

  it("preserves exact DOM node identity (toBe) for persistent aria-live region and focused row buttons across import://changed and health://changed", async () => {
    const listeners = new Map<string, (event: { payload: unknown }) => void>();
    listenMock.mockImplementation(async (eventName, cb) => {
      listeners.set(eventName, cb as (event: { payload: unknown }) => void);
      return () => {};
    });

    const initialDump = deskDump({ imports_ready_to_review: 1, imports_need_owner: false });
    const initialDoc = importDoc({
      ready_to_review: 1,
      rows: [
        row({
          id: "rev1",
          name: "export.zip",
          state: "ready_to_review",
          import_id: "20261122_143000",
          added: 200,
        }),
      ],
    });

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_health") return initialDump;
      if (cmd === "import_status") return initialDoc;
      return null;
    });

    app.start();
    // Flush boot promises.
    await new Promise((resolve) => setTimeout(resolve, 0));
    app.__test__.openDeskSource("import");

    const appRoot = document.querySelector("#app")!;
    const liveBefore = document.getElementById("solstone-live-region")!;
    expect(liveBefore).not.toBeNull();
    expect(appRoot.contains(liveBefore)).toBe(false);
    expect(liveBefore.getAttribute("aria-live")).toBe("polite");

    const reviewBtnBefore = byId("settings.import.row.rev1.review") as HTMLButtonElement;
    expect(reviewBtnBefore).not.toBeNull();
    reviewBtnBefore.focus();
    expect(document.activeElement).toBe(reviewBtnBefore);

    // Emit import://changed adding another item while rev1 stays focused.
    const onImportChanged = listeners.get("import://changed");
    expect(onImportChanged).toBeDefined();
    onImportChanged!({
      payload: importDoc({
        ready_to_review: 1,
        waiting: 1,
        rows: [
          row({
            id: "rev1",
            name: "export.zip",
            state: "ready_to_review",
            import_id: "20261122_143000",
            added: 200,
          }),
          row({
            id: "send2",
            name: "notes.pdf",
            state: "sending",
            added: 300,
          }),
        ],
      }),
    });

    expect(byId("settings.import.row.rev1.review")).toBe(reviewBtnBefore);
    expect(document.activeElement).toBe(reviewBtnBefore);
    expect(document.getElementById("solstone-live-region")).toBe(liveBefore);
    expect(byId(ids["settings.import.deviceSection"])?.textContent).toContain("notes.pdf");

    // Emit health://changed (heartbeat + link change) while rev1 stays focused.
    const onHealthChanged = listeners.get("health://changed");
    expect(onHealthChanged).toBeDefined();
    onHealthChanged!({
      payload: deskDump({ link: "offline", imports_ready_to_review: 1, imports_waiting: 1 }),
    });

    expect(byId("settings.import.row.rev1.review")).toBe(reviewBtnBefore);
    expect(document.activeElement).toBe(reviewBtnBefore);
    expect(document.getElementById("solstone-live-region")).toBe(liveBefore);
    expect(document.querySelector(".desk-pill")?.textContent).toContain("offline");
  });

  it("status pill never shows green connected when review or attention items exist, and status pane leads accurately", () => {
    // Ready to review -> orange review pill, never connected=true.
    const reviewDump = deskDump({ imports_ready_to_review: 2, imports_need_owner: false });
    app.__test__.setHealth(reviewDump);
    app.__test__.setImport(
      importDoc({
        ready_to_review: 2,
        rows: [
          row({ id: "r1", name: "a.zip", state: "ready_to_review", import_id: "20261122_143000" }),
          row({ id: "r2", name: "b.zip", state: "ready_to_review", import_id: "20261122_143001" }),
        ],
      }),
    );
    app.__test__.renderSettings(reviewDump);

    const pill = document.querySelector<HTMLButtonElement>(".desk-pill")!;
    expect(pill.textContent).toBe("2 ready to review");
    expect(pill.dataset.pillKind).toBe("review");
    expect(pill.dataset.connected).toBeUndefined();
    expect(document.querySelector(".desk-lead-headline")?.textContent).toBe("2 imports ready to review");
    expect(document.querySelector(".desk-lead-line")?.textContent).toBe("open import to review and start.");

    // Attention with waiting == 0 -> never connected=true.
    const attnDump = deskDump({ imports_need_owner: true, imports_waiting: 0 });
    app.__test__.setHealth(attnDump);
    app.__test__.setImport(
      importDoc({
        needs_owner: true,
        rows: [row({ id: "f1", name: "bad.mov", state: "refused", reason: "too large" })],
      }),
    );
    app.__test__.renderSettings(attnDump);

    const attnPill = document.querySelector<HTMLButtonElement>(".desk-pill")!;
    expect(attnPill.dataset.connected).toBeUndefined();
    expect(attnPill.dataset.pillKind).toBeUndefined();
    expect(document.querySelector(".desk-lead-headline")?.textContent).toBe("imports need you");
    expect(document.querySelector(".desk-lead-line")?.textContent).toBe("open import to see why.");
  });

  it("hides more imports and all import history when unpaired or awaiting confirmation while keeping choose files enabled", () => {
    for (const phase of ["not_paired", "awaiting_confirmation"] as const) {
      const dump = deskDump({ phase, link: null });
      app.__test__.setHealth(dump);
      app.__test__.setImport(
        importDoc({
          waiting: 1,
          rows: [row({ id: "w1", name: "queued.pdf", state: "waiting" })],
        }),
      );
      app.__test__.openDeskSource("import");

      const choose = byId(ids["settings.import.choose"]) as HTMLButtonElement;
      expect(choose).not.toBeNull();
      expect(choose.disabled).toBe(false);
      expect(byId(ids["settings.import.more"])).toBeNull();
      expect(document.querySelector(".desk-import-helper")).toBeNull();
      expect(byId(ids["settings.import.allHistory"])).toBeNull();

      const expectedHold =
        phase === "not_paired"
          ? "imports wait on this device until you connect a journal."
          : "imports wait until you confirm your journal's mark.";
      expect(document.querySelector(".desk-import-hold")?.textContent).toBe(expectedHold);
    }
  });

  it("remove dialog uses ready-to-review copy and restores focus to triggering button on cancel", async () => {
    const dump = deskDump({ imports_ready_to_review: 1 });
    app.__test__.setHealth(dump);
    app.__test__.setImport(
      importDoc({
        ready_to_review: 1,
        rows: [
          row({
            id: "rev1",
            name: "export.zip",
            state: "ready_to_review",
            import_id: "20261122_143000",
          }),
        ],
      }),
    );
    app.__test__.openDeskSource("import");

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "import_prepare_remove") {
        return {
          name: "export.zip",
          may_have_reached: true,
          missing_copy: false,
          ready_to_review: true,
        };
      }
      return null;
    });

    const removeBtn = byId("settings.import.row.rev1.remove") as HTMLButtonElement;
    removeBtn.focus();
    removeBtn.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const dialog = document.querySelector("dialog.desk-import-confirm") as HTMLDialogElement;
    expect(dialog).not.toBeNull();
    expect(dialog.querySelector("p")?.textContent).toBe(
      "this removes the copy on this device. the waiting copy in your journal stays there until you discard it or it expires.",
    );

    const cancelBtn = dialog.querySelector("button") as HTMLButtonElement;
    cancelBtn.click();
    expect(document.querySelector("dialog.desk-import-confirm")).toBeNull();
    expect(document.activeElement).toBe(removeBtn);
  });

  it("moves focus to pane title at narrow <720px and closes back to home/status on Escape restoring tile focus", () => {
    const dump = deskDump();
    app.__test__.setHealth(dump);
    app.__test__.setImport(importDoc());

    // Narrow <720px width moves focus to .desk-pane-title.
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true, configurable: true });
    app.__test__.renderSettings(dump);
    app.__test__.openDeskSource("import");
    const paneTitle = document.querySelector<HTMLElement>(".desk-pane-title")!;
    expect(document.activeElement).toBe(paneTitle);

    // Escape closes pane back to status and restores focus to the import tile.
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(app.__test__.getDesk()).toEqual({ row: "home", pane: "status" });
    const importTileBtn = document.querySelector<HTMLButtonElement>('[data-tile="import"] .desk-tile-open');
    expect(document.activeElement).toBe(importTileBtn);
  });

  it("triggers import_reconcile when the settings window receives focus", async () => {
    const dump = deskDump();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_health") return dump;
      return null;
    });

    app.start();
    await new Promise((resolve) => setTimeout(resolve, 0));
    invokeMock.mockClear();

    window.dispatchEvent(new Event("focus"));
    expect(invokeMock).toHaveBeenCalledWith("import_reconcile");
  });
});
