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
  foldersTile,
  foldersTileLines,
  folderWord,
  settingsNavigate,
  type FolderSummary,
} from "../src/shell/derive";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

type Doc = NonNullable<Parameters<typeof app.__test__.setFolders>[0]>;
type Folder = Doc["folders"][number];

const DEFAULT_KINDS = {
  notes: true,
  documents: true,
  transcripts: true,
  screenshots: true,
  photos: true,
  other: true,
};

function makeFolder(overrides: Partial<Folder> = {}): Folder {
  return {
    id: "fld_work",
    path: "C:\\Users\\ana\\Documents\\Work",
    name: "Work",
    label: "Documents \u203A Work",
    mode: "on",
    attention: null,
    kinds: { ...DEFAULT_KINDS },
    facet: null,
    overlap: null,
    returning: false,
    progress: null,
    waiting: 0,
    cant_read: 0,
    fallback_to_details: 0,
    last_checked_ms: null,
    paused_by_computer: false,
    preview: null,
    ...overrides,
  };
}

function makeDoc(overrides: Partial<Doc> = {}): Doc {
  return {
    v: 1,
    folders: [],
    facets: [
      { name: "work", title: "Work" },
      { name: "home", title: "Home" },
    ],
    facets_state: "ready",
    computer_paused: false,
    paired: true,
    offline: false,
    journal_lists: true,
    ...overrides,
  };
}

const byId = (id: string): HTMLElement | null => document.querySelector(`[data-automation-id="${id}"]`);
const pane = (): HTMLElement => document.querySelector(".desk-pane") as HTMLElement;
const tile = (): HTMLElement => document.querySelector('[data-tile="folders"]') as HTMLElement;
const radios = (): HTMLInputElement[] => Array.from(pane().querySelectorAll('input[type="radio"]'));
const kindChecks = (): HTMLInputElement[] =>
  Array.from(pane().querySelectorAll('input[type="checkbox"][data-folder-kind]'));
const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

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

function renderDesk(
  doc: Doc | null,
  options: { open?: boolean; selected?: string | null; appState?: "idle" | "paused" } = {},
): void {
  const { open = true, selected = null, appState = "idle" } = options;
  const dump = deskDump(appState);
  app.__test__.setRoute("home");
  app.__test__.setHealth(dump);
  app.__test__.setCounters(null);
  app.__test__.setFolders(doc);
  if (selected) app.__test__.selectFolder(selected);
  if (open) app.__test__.openDeskSource("folders");
  app.__test__.renderSettings(dump);
}

describe("folders decisions", () => {
  it("rolls up multiple folders by worst signal with no tile switch", () => {
    expect(foldersTile(null)).toMatchObject({ word: "ready to set up", switchOn: null });
    expect(foldersTile(makeDoc({ folders: [] }))).toMatchObject({ word: "ready to set up", switchOn: null });
    expect(foldersTile(makeDoc({ folders: [makeFolder({ mode: "off" })] }))).toMatchObject({
      word: "off",
      switchOn: null,
    });
    expect(
      foldersTile(makeDoc({ folders: [makeFolder({ mode: "paused" }), makeFolder({ id: "b", mode: "off" })] })),
    ).toMatchObject({
      word: "paused",
      switchOn: null,
    });
    expect(
      foldersTile(
        makeDoc({
          folders: [
            makeFolder({ mode: "on" }),
            makeFolder({ id: "b", mode: "paused" }),
            makeFolder({ id: "c", mode: "off" }),
          ],
        }),
      ),
    ).toMatchObject({ word: "on", switchOn: null });
    expect(
      foldersTile(
        makeDoc({
          folders: [
            makeFolder({ mode: "on" }),
            makeFolder({ id: "b", mode: "setting_up" }),
          ],
        }),
      ),
    ).toMatchObject({ word: "setting up", switchOn: null });
    expect(
      foldersTile(
        makeDoc({
          folders: [
            makeFolder({ mode: "setting_up" }),
            makeFolder({ id: "b", mode: "on", attention: "missing" }),
          ],
        }),
      ),
    ).toMatchObject({ word: "needs attention", switchOn: null });
  });

  it("maps each individual folder state to its canonical word", () => {
    const cases: Array<[Partial<FolderSummary>, ReturnType<typeof folderWord>]> = [
      [{ mode: "setting_up" }, "setting up"],
      [{ mode: "on" }, "on"],
      [{ mode: "on", paused_by_computer: true }, "paused"],
      [{ mode: "paused" }, "paused"],
      [{ mode: "off" }, "off"],
      [{ mode: "on", attention: "missing" }, "needs attention"],
      [{ mode: "on", attention: "unreadable" }, "needs attention"],
    ];
    for (const [override, expected] of cases) {
      expect(folderWord(makeFolder(override))).toBe(expected);
    }
  });

  it("summarizes multiple active folders and suppresses companion text under computer pause", () => {
    const two = [
      makeFolder({ id: "a", name: "Work", label: "Documents \u203A Work" }),
      makeFolder({ id: "b", name: "Notes", label: "Documents \u203A Notes" }),
    ];
    const twoLines = foldersTileLines(makeDoc({ folders: two }));
    expect(twoLines.companion).toContain("Work");
    expect(twoLines.companion).toContain("Notes");
    const three = [...two, makeFolder({ id: "c", name: "Tax", label: "Documents \u203A Tax" })];
    expect(foldersTileLines(makeDoc({ folders: three })).companion).toContain("3");
    const paused = three.map((f) => ({ ...f, paused_by_computer: true }));
    const pausedLines = foldersTileLines(makeDoc({ folders: paused, computer_paused: true }));
    expect(pausedLines.companion).not.toContain("Work");
    expect(pausedLines.companion).not.toContain("Notes");
  });

  it("places folders after screenshots in the deck, navigates directly, and reads legacy counters", () => {
    const order = deckTiles();
    expect(order.indexOf("folders")).toBe(order.indexOf("screenshots") + 1);
    expect(settingsNavigate("folders")).toEqual({ row: "home", pane: "folders" });
    const legacy = counterFacts({
      started: "2026-11-01",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
    });
    expect(legacy?.find((fact) => fact.source === "folders")?.nothingYet).toBe(true);
    const active = counterFacts({
      started: "2026-11-01",
      audio: { seconds: 0, last: null },
      screen: { seconds: 0, last: null },
      browsers: { total: 0, last: null },
      obsidian: { total: 0, last: null },
      folders: { total: 27, last: Math.floor(Date.now() / 1000) - 10 },
    });
    expect(active?.find((fact) => fact.source === "folders")?.nothingYet).toBe(false);
  });
});

describe("the folders page, detail view, and tile", () => {
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

  it("renders a tile with no toggle switch and opens the empty add state", () => {
    renderDesk(makeDoc());
    expect(tile().querySelector('input[type="checkbox"]')).toBeNull();
    expect(byId(ids["settings.sources.folders.add"])).not.toBeNull();
    expect(byId(ids["settings.sources.folders.list"])).toBeNull();
  });

  it("walks add -> overlap warning -> override -> begin_setup", async () => {
    const added = makeFolder({ id: "fld_shots", path: "C:\\Users\\ana\\Pictures\\Screenshots", mode: "setting_up" });
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === "get_source_counters") return null;
      if (cmd === "folders_pick_folder") return added.path;
      if (cmd === "folders_check_path") {
        return { status: "overlaps", path: added.path, target: { kind: "screenshots", name: "Screenshots" } };
      }
      if (cmd === "folders_action") {
        const req = (args as { request: { op: string } }).request;
        if (req.op === "begin_setup" || req.op === "refresh_facets" || req.op === "preview") {
          return makeDoc({ folders: [added] });
        }
      }
      return null;
    });

    renderDesk(makeDoc());
    byId(ids["settings.sources.folders.add"])?.click();
    await flush();

    const overlapCard = byId(ids["settings.sources.folders.overlap"]);
    expect(overlapCard).not.toBeNull();
    const buttons = Array.from(overlapCard!.querySelectorAll("button"));
    expect(buttons).toHaveLength(2);
    // Second button is the explicit override action.
    buttons[1].click();
    await flush();

    expect(invokeMock).toHaveBeenCalledWith("folders_action", {
      request: {
        op: "begin_setup",
        path: added.path,
        overlap: { kind: "screenshots", name: "Screenshots" },
      },
    });
  });

  it("handles already_added and refused checks inline without starting setup", async () => {
    const existing = makeFolder({ id: "fld_work", name: "Work" });
    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "get_source_counters") return null;
      if (cmd === "folders_pick_folder") return existing.path;
      if (cmd === "folders_check_path") {
        return { status: "already_added", folder_id: existing.id, name: existing.name };
      }
      return null;
    });

    renderDesk(makeDoc({ folders: [existing] }));
    byId(ids["settings.sources.folders.add"])?.click();
    await flush();
    expect(pane().querySelectorAll(".desk-reason")).toHaveLength(1);
    expect(invokeMock).not.toHaveBeenCalledWith("folders_action", expect.objectContaining({
      request: expect.objectContaining({ op: "begin_setup" }),
    }));

    invokeMock.mockImplementation(async (cmd) => {
      if (cmd === "get_source_counters") return null;
      if (cmd === "folders_pick_folder") return "C:\\Windows";
      if (cmd === "folders_check_path") return { status: "refused", path: "C:\\Windows" };
      return null;
    });
    byId(ids["settings.sources.folders.add"])?.click();
    await flush();
    expect(pane().querySelectorAll(".desk-reason")).toHaveLength(1);
  });

  it("renders setup kinds, facet picker, 5 spans, non-money estimate, and confirm action", async () => {
    const settingUp = makeFolder({
      mode: "setting_up",
      preview: {
        span: "thirty_days",
        total_items: 14,
        content_items: 12,
        metadata_only_items: 2,
        content_bytes: 2_621_440,
        first_date: "2024-02-29",
        oldest_ms: Date.UTC(2026, 10, 1),
        newest_ms: Date.UTC(2026, 10, 27),
        estimate: { segments: 4, days: 3, thoughts: 9, from_journal: true },
        failed: false,
      },
    });
    renderDesk(makeDoc({ folders: [settingUp] }), { selected: settingUp.id });
    invokeMock.mockClear();

    expect(kindChecks()).toHaveLength(6);
    expect(kindChecks().every((c) => c.checked)).toBe(true);
    expect(radios()).toHaveLength(5);
    expect(radios()[0].checked).toBe(true);

    expect(document.body.textContent?.toLowerCase()).toMatch(/from february 29, 2024/);
    const est = byId(ids["settings.sources.folders.estimate"]);
    expect(est).not.toBeNull();
    const estText = est?.textContent ?? "";
    expect(estText).toContain("4");
    expect(estText).toContain("3");
    expect(estText).toContain("9");
    // Hard invariant: never show currency or money symbols in estimates.
    expect(estText).not.toMatch(/[$€£¥]/);

    const facetSelect = byId(ids["settings.sources.folders.facet"]) as HTMLSelectElement;
    expect(facetSelect).not.toBeNull();
    facetSelect.value = "work";
    facetSelect.dispatchEvent(new Event("change"));
    expect(invokeMock).toHaveBeenCalledWith("folders_action", {
      request: { op: "set_facet", id: settingUp.id, facet: { name: "work", title: "Work" } },
    });
    await flush();

    const confirm = byId(ids["settings.sources.folders.confirm"]) as HTMLButtonElement;
    expect(confirm.disabled).toBe(false);
    confirm.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", {
      request: { op: "confirm", id: settingUp.id, span: "thirty_days" },
    });
  });

  it("requires a selected kind and accepts a local preview while unpaired", () => {
    const basePreview = {
      span: "thirty_days" as const,
      total_items: 0,
      content_items: 0,
      metadata_only_items: 0,
      content_bytes: 0,
      oldest_ms: null,
      newest_ms: null,
      estimate: { segments: 0, days: 0, thoughts: 0, from_journal: true },
      failed: false,
    };
    const noKinds = makeFolder({
      mode: "setting_up",
      kinds: { notes: false, documents: false, transcripts: false, screenshots: false, photos: false, other: false },
      preview: basePreview,
    });
    renderDesk(makeDoc({ folders: [noKinds] }), { selected: noKinds.id });
    expect((byId(ids["settings.sources.folders.confirm"]) as HTMLButtonElement).disabled).toBe(true);
    expect(pane().querySelectorAll(".desk-reason").length).toBeGreaterThanOrEqual(1);

    const unpaired = makeFolder({ mode: "setting_up", preview: basePreview });
    renderDesk(makeDoc({ paired: false, facets_state: "unpaired", folders: [unpaired] }), { selected: unpaired.id });
    const confirm = byId(ids["settings.sources.folders.confirm"]) as HTMLButtonElement;
    expect(confirm.disabled).toBe(false);
    confirm.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "confirm", id: unpaired.id, span: "thirty_days" } });
  });

  it("exposes pause, resume, turn off, turn on, and inline remove confirmation", async () => {
    const onFolder = makeFolder({ mode: "on" });
    renderDesk(makeDoc({ folders: [onFolder] }), { selected: onFolder.id });
    invokeMock.mockClear();
    byId(ids["settings.sources.folders.pause"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "pause", id: onFolder.id } });
    await flush();
    byId(ids["settings.sources.folders.enabled"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "turn_off", id: onFolder.id } });
    await flush();

    const pausedFolder = makeFolder({ mode: "paused" });
    renderDesk(makeDoc({ folders: [pausedFolder] }), { selected: pausedFolder.id });
    invokeMock.mockClear();
    byId(ids["settings.sources.folders.pause"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "resume", id: pausedFolder.id } });

    const offFolder = makeFolder({ mode: "off" });
    renderDesk(makeDoc({ folders: [offFolder] }), { selected: offFolder.id });
    invokeMock.mockClear();
    byId(ids["settings.sources.folders.enabled"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "turn_on", id: offFolder.id } });
    await flush();

    // Remove requires an explicit inline second step before invoking.
    invokeMock.mockClear();
    byId(ids["settings.sources.folders.remove"])?.click();
    expect(invokeMock).not.toHaveBeenCalledWith("folders_action", expect.objectContaining({
      request: expect.objectContaining({ op: "remove" }),
    }));
    byId(ids["settings.sources.folders.remove"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "remove", id: offFolder.id } });
  });

  it("shows collapsed Windows OneDrive note, attention controls without duplicates, and partial error counts", () => {
    const missing = makeFolder({ mode: "on", attention: "missing" });
    renderDesk(makeDoc({ folders: [missing] }), { selected: missing.id });
    expect(document.querySelectorAll(`[data-automation-id="${ids["settings.sources.folders.repoint"]}"]`)).toHaveLength(1);
    expect(document.querySelectorAll(`[data-automation-id="${ids["settings.sources.folders.tryAgain"]}"]`)).toHaveLength(1);

    const more = pane().querySelector("details");
    expect(more).not.toBeNull();
    expect(more?.open).toBe(false);

    const degraded = makeFolder({ mode: "on", cant_read: 2, fallback_to_details: 1 });
    renderDesk(makeDoc({ folders: [degraded] }), { selected: degraded.id });
    invokeMock.mockClear();
    expect(pane().querySelectorAll(".desk-reason")).toHaveLength(2);
    byId(ids["settings.sources.folders.tryAgain"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("folders_action", { request: { op: "retry", id: degraded.id } });
  });

  it("lets an owner using only folders pause from home and polls only while folders exist", async () => {
    const dump = { ...observingDump(), app_state: "idle" as const };
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setCounters(null);
    app.__test__.setFolders(makeDoc({ folders: [makeFolder({ mode: "on" })] }));
    app.__test__.renderSettings(dump);
    const pause = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "pause");
    expect(pause?.disabled).toBe(false);

    renderDesk(makeDoc({ folders: [] }));
    invokeMock.mockClear();
    await app.__test__.pollFoldersStatus();
    expect(invokeMock).not.toHaveBeenCalled();

    renderDesk(makeDoc({ folders: [makeFolder({ mode: "on" })] }));
    invokeMock.mockClear();
    invokeMock.mockResolvedValue(makeDoc({ folders: [makeFolder({ mode: "on" })] }));
    await app.__test__.pollFoldersStatus();
    expect(invokeMock).toHaveBeenCalledWith("folders_status");
  });

  it("uses no forbidden owner words across list and detail views", () => {
    const busy = makeFolder({
      mode: "setting_up",
      cant_read: 1,
      fallback_to_details: 1,
      preview: {
        span: "thirty_days",
        total_items: 5,
        content_items: 4,
        metadata_only_items: 1,
        content_bytes: 4096,
        oldest_ms: Date.UTC(2026, 10, 10),
        newest_ms: Date.UTC(2026, 10, 27),
        estimate: { segments: 2, days: 2, thoughts: 4, from_journal: true },
        failed: false,
      },
    });
    renderDesk(makeDoc({ folders: [busy] }), { selected: busy.id });
    const words = (document.body.textContent ?? "").toLowerCase();
    for (const word of ["capture", "record", "watch", "monitor", "track", "collect", "observer"]) {
      expect(words.includes(word), word).toBe(false);
    }
  });
});
