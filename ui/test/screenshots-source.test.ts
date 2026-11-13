// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import {
  counterFacts,
  deckTiles,
  screenshotsConfirm,
  screenshotsPage,
  screenshotsTile,
  type ScreenshotsStateName,
} from "../src/shell/derive";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

type Status = NonNullable<Parameters<typeof app.__test__.setScreenshots>[0]>;

const STATES: ScreenshotsStateName[] = ["off", "no_folder", "folder_missing", "choosing", "bringing_in", "on", "paused"];

function status(overrides: Partial<Status> = {}): Status {
  return {
    state: "on",
    folder: "C:\\Users\\ana\\Pictures\\Screenshots",
    folder_line: "Pictures \u203A Screenshots",
    folder_name: "Screenshots",
    windows_folder: true,
    counting: false,
    count_failed: false,
    counts: null,
    span: "thirty_days",
    turned_back_on: false,
    backlog: null,
    waiting_for_journal: false,
    waiting: 0,
    cant_come_in: 0,
    unreadable: 0,
    last_checked_ms: null,
    ...overrides,
  };
}

const byId = (id: string): HTMLElement | null => document.querySelector(`[data-automation-id="${id}"]`);
const pane = (): HTMLElement => document.querySelector(".desk-pane") as HTMLElement;
const tile = (): HTMLElement => document.querySelector('[data-tile="screenshots"]') as HTMLElement;
const reasons = (): number => pane().querySelectorAll(".desk-reason").length;
const breaths = (): number => pane().querySelectorAll(".desk-breath").length;
const radios = (): HTMLInputElement[] => Array.from(pane().querySelectorAll('input[type="radio"]'));
const confirmButton = (): HTMLButtonElement => byId(ids["settings.sources.screenshots.confirm"]) as HTMLButtonElement;

function deskDump(appState: "idle" | "paused" = "idle") {
  const base = observingDump();
  return {
    ...base,
    app_state: appState,
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

function renderDesk(value: Status | null, open = true, appState: "idle" | "paused" = "idle"): void {
  const dump = deskDump(appState);
  app.__test__.setRoute("home");
  app.__test__.setHealth(dump);
  app.__test__.setScreenshots(value);
  if (open) app.__test__.openDeskSource("screenshots");
  app.__test__.renderSettings(dump);
}

describe("screenshots decisions", () => {
  it("names each state with a locked word and a switch that says whether it is on", () => {
    expect(screenshotsTile(null)).toMatchObject({ word: "off", switchOn: false });
    expect(screenshotsTile("off")).toMatchObject({ word: "off", switchOn: false });
    expect(screenshotsTile("no_folder")).toMatchObject({ word: "ready to set up", switchOn: false });
    expect(screenshotsTile("choosing")).toMatchObject({ word: "ready to set up", switchOn: true });
    expect(screenshotsTile("bringing_in")).toMatchObject({ word: "on", switchOn: true });
    expect(screenshotsTile("on")).toMatchObject({ word: "on", switchOn: true });
    expect(screenshotsTile("paused")).toMatchObject({ word: "paused", switchOn: true });
    expect(screenshotsTile("folder_missing")).toMatchObject({ word: "needs attention", switchOn: true });
  });

  it("offers one action per state at most, and the folder is always a fact", () => {
    for (const state of STATES) {
      for (const countFailed of [false, true]) {
        const page = screenshotsPage({
          state,
          countFailed,
          waitingForJournal: false,
          windowsFolder: true,
          hasFolder: state !== "no_folder",
          confirmed: state === "on" || state === "bringing_in" || state === "paused",
        });
        expect(page.folderFact).toBe(true);
        expect(["tryAgain", "resume", null]).toContain(page.action);
      }
    }
    const failed = screenshotsPage({
      state: "choosing",
      countFailed: true,
      waitingForJournal: false,
      windowsFolder: true,
      hasFolder: true,
      confirmed: false,
    });
    expect(failed).toMatchObject({ reason: "countFailed", action: "tryAgain", chooser: true });
  });

  it("says other files stay only for a folder the owner chose", () => {
    const base = { state: "on" as const, countFailed: false, waitingForJournal: false, hasFolder: true, confirmed: true };
    expect(screenshotsPage({ ...base, windowsFolder: true })).toMatchObject({ otherFilesStay: false, everyPicture: true });
    expect(screenshotsPage({ ...base, windowsFolder: false })).toMatchObject({ otherFilesStay: true, everyPicture: false });
  });

  it("keeps the confirm waiting for the count, then carries it, or today", () => {
    expect(screenshotsConfirm({ span: "thirty_days", count: null, counting: true })).toEqual({ kind: "checking" });
    expect(screenshotsConfirm({ span: "thirty_days", count: 12, counting: false })).toEqual({ kind: "count", count: 12 });
    expect(screenshotsConfirm({ span: "thirty_days", count: 0, counting: false })).toEqual({ kind: "today" });
    expect(screenshotsConfirm({ span: "nothing", count: 3, counting: false })).toEqual({ kind: "today" });
  });

  it("puts the tile after Obsidian and reads a counter document from before screenshots", () => {
    const order = deckTiles();
    expect(order.indexOf("screenshots")).toBe(order.indexOf("obsidian") + 1);
    const facts = counterFacts({
      started: "2026-11-01",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
    });
    expect(facts?.find((fact) => fact.source === "screenshots")?.nothingYet).toBe(true);
  });
});

describe("the screenshots page and tile", () => {
  beforeEach(() => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "app";
    root.setAttribute("data-automation-id", ids["settings.window.root"]);
    document.body.append(root);
    app.__test__.reset();
    app.__test__.setRoot(root);
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(null);
  });

  it("shows the folder on the tile while on, and not under the computer's pause", () => {
    renderDesk(status({ state: "on" }), false);
    expect(tile().textContent).toContain("Pictures \u203A Screenshots");
    renderDesk(status({ state: "paused" }), false, "paused");
    expect(tile().querySelector(".desk-tile-word")?.textContent).toBe("paused");
    expect(tile().textContent).not.toContain("Pictures \u203A Screenshots");
  });

  it("shows progress on the tile while bringing in", () => {
    renderDesk(status({ state: "bringing_in", backlog: { done: 3, total: 40 } }), false);
    const sub = tile().querySelector(".desk-tile-sub")?.textContent ?? "";
    expect(sub).toContain("3");
    expect(sub).toContain("40");
  });

  it("offers five choices with the first preselected, and waits for the count", () => {
    renderDesk(status({ state: "choosing", counting: true }));
    expect(radios()).toHaveLength(5);
    expect(radios()[0].checked).toBe(true);
    expect(radios().filter((r) => r.checked)).toHaveLength(1);
    expect(confirmButton().disabled).toBe(true);
  });

  it("brings in what was counted for the chosen span", () => {
    renderDesk(status({ state: "choosing", counts: { thirty_days: 12, six_months: 40, year: 41, all: 99, nothing: 0 } }));
    expect(confirmButton().disabled).toBe(false);
    expect(confirmButton().textContent).toContain("12");
    radios()[3].click();
    expect(confirmButton().textContent).toContain("99");
    confirmButton().click();
    expect(invokeMock).toHaveBeenCalledWith("screenshots_confirm", { span: "all" });
  });

  it("says once more what's in already when turned back on", () => {
    renderDesk(status({ state: "choosing", counts: { thirty_days: 1 } }));
    const quiet = breaths();
    renderDesk(status({ state: "choosing", counts: { thirty_days: 1 }, turned_back_on: true }));
    expect(breaths()).toBe(quiet + 1);
  });

  it("lets a failed count try again, and keeps the confirm waiting", () => {
    renderDesk(status({ state: "choosing", count_failed: true }));
    expect(confirmButton().disabled).toBe(true);
    byId(ids["settings.sources.screenshots.tryAgain"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("screenshots_try_again");
  });

  it("keeps choosing a folder beside the folder fact in every state", () => {
    for (const state of STATES) {
      renderDesk(status({ state, folder: state === "no_folder" ? null : "C:\\Shots", windows_folder: false }));
      const fact = byId(ids["settings.sources.screenshots.folder"]);
      expect(fact, state).not.toBeNull();
      const choose = byId(ids["settings.sources.screenshots.chooseFolder"]);
      expect(fact?.contains(choose), state).toBe(true);
    }
    byId(ids["settings.sources.screenshots.chooseFolder"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("screenshots_choose_folder");
  });

  it("gives a missing folder and no folder a reason, with no second action", () => {
    for (const state of ["no_folder", "folder_missing"] as const) {
      renderDesk(status({ state, folder: state === "no_folder" ? null : "C:\\Shots" }));
      expect(byId(ids["settings.sources.screenshots.reason"]), state).not.toBeNull();
      expect(byId(ids["settings.sources.screenshots.tryAgain"]), state).toBeNull();
    }
  });

  it("says other files stay where they are only for a chosen folder", () => {
    renderDesk(status({ state: "on", windows_folder: true }));
    const own = breaths();
    renderDesk(status({ state: "on", windows_folder: false, folder_name: "Desktop" }));
    expect(breaths()).toBe(own + 1);
  });

  it("explains a journal that doesn't take screenshots yet, without a fault", () => {
    renderDesk(status({ state: "on" }));
    expect(reasons()).toBe(0);
    renderDesk(status({ state: "on", waiting_for_journal: true, waiting: 4 }));
    expect(reasons()).toBe(1);
    expect(tile().querySelector(".desk-tile-word")?.textContent).toBe("on");
  });

  it("says what can't come in once confirmed, and lets what couldn't be read try again", () => {
    renderDesk(status({ state: "on" }));
    const quiet = reasons();
    renderDesk(status({ state: "on", cant_come_in: 2, unreadable: 1 }));
    expect(reasons()).toBe(quiet + 2);
    const again = Array.from(pane().querySelectorAll("button")).filter((b) => b.textContent === "try again");
    expect(again).toHaveLength(1);
    again[0].click();
    expect(invokeMock).toHaveBeenCalledWith("screenshots_try_again");
  });

  it("keeps the clipboard note behind more about this", () => {
    renderDesk(status({ state: "on" }));
    const more = pane().querySelector("details");
    expect(more).not.toBeNull();
    expect(more?.open).toBe(false);
  });

  it("turns on from the switch and opens the page, and turns off", () => {
    renderDesk(status({ state: "off" }), false);
    const input = tile().querySelector('input[type="checkbox"]') as HTMLInputElement;
    input.checked = true;
    input.dispatchEvent(new Event("change"));
    expect(invokeMock).toHaveBeenCalledWith("screenshots_turn_on");
    renderDesk(status({ state: "on" }), false);
    const on = tile().querySelector('input[type="checkbox"]') as HTMLInputElement;
    on.checked = false;
    on.dispatchEvent(new Event("change"));
    expect(invokeMock).toHaveBeenCalledWith("screenshots_turn_off");
  });

  it("asks for a folder instead of turning on when none resolves", () => {
    renderDesk(status({ state: "no_folder", folder: null }), false);
    const input = tile().querySelector('input[type="checkbox"]') as HTMLInputElement;
    input.checked = true;
    input.dispatchEvent(new Event("change"));
    expect(invokeMock).not.toHaveBeenCalledWith("screenshots_turn_on");
    expect(document.querySelector(".desk-pane-title")?.textContent).toBe("screenshots");
  });

  it("lists screenshots in the status pane with its own total", () => {
    const dump = deskDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setScreenshots(status({ state: "on" }));
    app.__test__.setCounters({
      started: "2026-11-01",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
      screenshots: { total: 412, last: Math.floor(Date.now() / 1000) - 30 },
    });
    app.__test__.renderSettings(dump);
    expect(pane().textContent).toContain("412");
  });

  it("lets an owner using only screenshots pause from home", () => {
    const dump = { ...observingDump(), app_state: "idle" as const };
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setScreenshots(status({ state: "on" }));
    app.__test__.renderSettings(dump);
    const pause = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "pause");
    expect(pause?.disabled).toBe(false);
    pause?.click();
    expect(invokeMock).toHaveBeenCalledWith("pause", { reason: "operator", durationSecs: null });
  });

  it("reads the source again while it's set up, and not while it's off", async () => {
    renderDesk(status({ state: "off" }));
    invokeMock.mockClear();
    await app.__test__.pollScreenshotsStatus();
    expect(invokeMock).not.toHaveBeenCalled();
    renderDesk(status({ state: "on" }));
    invokeMock.mockResolvedValue({ v: 1, status: status({ state: "folder_missing" }) });
    await app.__test__.pollScreenshotsStatus();
    expect(invokeMock).toHaveBeenCalledWith("screenshots_status");
  });

  it("uses no forbidden owner words", () => {
    renderDesk(status({ state: "choosing", counts: { thirty_days: 2 }, turned_back_on: true, waiting: 1, unreadable: 1 }));
    const words = (document.body.textContent ?? "").toLowerCase();
    for (const word of ["capture", "record", "watch", "monitor", "track", "collect", "observer"]) {
      expect(words.includes(word), word).toBe(false);
    }
  });
});
