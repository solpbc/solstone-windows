// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The settings window opens with `?route=` from every native entry point: first-run
// auto-open, the tray, `--open-view settings` and a source destination. These tests run
// the real start() path with that query, so the route is read the way the app reads it.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import { flushAnimationFrames } from "./setup";
import { notPairedDump, observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

type Wizard = "mic" | "done";

function withSetup<T extends object>(dump: T, wizard: Wizard) {
  return {
    ...dump,
    setup: {
      sequence_enabled: true,
      mic: false,
      system_audio: false,
      screen: false,
      start_at_login: false,
      wizard,
      notice_dismissed: true,
      desktop_activity: false,
      desktop_activity_allowed: true,
      desktop_activity_through_pause: false,
      desktop_activity_window_titles: true,
      desktop_activity_page_addresses: true,
    },
  };
}

const firstRun = () => withSetup(notPairedDump(), "mic");
const finished = () => withSetup(observingDump(), "done");

type Listener = (event: { payload: unknown }) => void;

async function openWindow(route: string | null, health: () => object) {
  window.history.replaceState({}, "", route ? `/?route=${route}` : "/");
  const core = await import("@tauri-apps/api/core");
  const event = await import("@tauri-apps/api/event");
  const win = await import("@tauri-apps/api/window");
  const invoke = vi.mocked(core.invoke);
  const listen = vi.mocked(event.listen);
  invoke.mockReset();
  listen.mockReset();
  const listeners = new Map<string, Listener>();
  listen.mockImplementation(async (name: string, handler: unknown) => {
    listeners.set(name, handler as Listener);
    return () => {};
  });
  vi.mocked(win.getCurrentWindow).mockReturnValue({ label: "settings" } as ReturnType<typeof win.getCurrentWindow>);
  invoke.mockImplementation((cmd) => {
    switch (cmd) {
      case "get_health":
        return Promise.resolve(health());
      case "list_running_apps":
      case "list_mic_devices":
        return Promise.resolve([]);
      default:
        return Promise.resolve(null);
    }
  });
  const app = await import("../src/main");
  app.start();
  await vi.runAllTimersAsync();
  await Promise.resolve();
  flushAnimationFrames();
  return { app, listeners };
}

describe("where the settings window opens", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers({ toFake: ["setInterval", "setTimeout", "clearInterval", "clearTimeout", "Date"] });
    vi.spyOn(globalThis, "setInterval").mockImplementation(() => 0 as unknown as ReturnType<typeof setInterval>);
    document.body.replaceChildren();
    const rootEl = document.createElement("div");
    rootEl.id = "app";
    document.body.append(rootEl);
  });

  afterEach(() => {
    window.history.replaceState({}, "", "/");
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each([null, "sources", "audio", "screen", "browsers", "obsidian"])(
    "shows setup to a first-time owner when opened with route %s",
    async (route) => {
      await openWindow(route, firstRun);
      expect(byId(ids["settings.setup.step"])).not.toBeNull();
      expect(document.querySelector(".desk-shell")).toBeNull();
    },
  );

  it("keeps setup on screen when the tray or --open-view asks an open window for the deck", async () => {
    const { listeners } = await openWindow("sources", firstRun);
    const navigate = listeners.get("settings://navigate");
    expect(navigate).toBeDefined();
    navigate!({ payload: "sources" });
    navigate!({ payload: "audio" });
    flushAnimationFrames();
    expect(byId(ids["settings.setup.step"])).not.toBeNull();
  });

  it("opens a finished owner on the deck with the status pane", async () => {
    const { app } = await openWindow("sources", finished);
    expect(document.querySelector(".desk-shell")).not.toBeNull();
    expect(byId(ids["settings.setup.step"])).toBeNull();
    expect(app.__test__.getDesk()).toEqual({ row: "home", pane: "status" });
  });

  it.each(["audio", "screen", "browsers", "obsidian"])(
    "opens a finished owner on the %s page when that source is the destination",
    async (source) => {
      const { app } = await openWindow(source, finished);
      expect(document.querySelector(".desk-shell")).not.toBeNull();
      expect(app.__test__.getDesk()).toEqual({ row: "home", pane: source });
    },
  );
});
