// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import { flushAnimationFrames } from "./setup";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

async function mockedTauri() {
  const core = await import("@tauri-apps/api/core");
  const event = await import("@tauri-apps/api/event");
  const win = await import("@tauri-apps/api/window");
  const invoke = vi.mocked(core.invoke);
  const listen = vi.mocked(event.listen);
  const getCurrentWindow = vi.mocked(win.getCurrentWindow);
  invoke.mockReset();
  listen.mockReset();
  getCurrentWindow.mockReset();
  listen.mockResolvedValue(() => {});
  getCurrentWindow.mockReturnValue({ label: "settings" } as ReturnType<typeof win.getCurrentWindow>);
  return { invoke, listen, getCurrentWindow };
}

function resetDom(): void {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  rootEl.setAttribute("data-automation-id", ids["settings.window.root"]);
  document.body.append(rootEl);
}

async function flushAsync(): Promise<void> {
  await vi.runAllTimersAsync();
  await Promise.resolve();
}

describe("entry behavior", () => {
  let setIntervalSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers({
      toFake: ["setInterval", "setTimeout", "clearInterval", "clearTimeout", "Date"],
    });
    setIntervalSpy = vi
      .spyOn(globalThis, "setInterval")
      .mockImplementation(() => 0 as unknown as ReturnType<typeof setInterval>);
    resetDom();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("imports main without side effects", async () => {
    const addEventListenerSpy = vi.spyOn(document, "addEventListener");
    const windowAddEventListenerSpy = vi.spyOn(window, "addEventListener");
    const headAppendSpy = vi.spyOn(document.head, "append");
    const { invoke, listen, getCurrentWindow } = await mockedTauri();

    await import("../src/main");

    expect(addEventListenerSpy).not.toHaveBeenCalled();
    expect(windowAddEventListenerSpy).not.toHaveBeenCalled();
    expect(headAppendSpy).not.toHaveBeenCalled();
    expect(setIntervalSpy).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
    expect(getCurrentWindow).not.toHaveBeenCalled();
  });

  it("fires view_rendered once after the contract root is present", async () => {
    const { invoke } = await mockedTauri();
    invoke.mockImplementation((cmd) => {
      switch (cmd) {
        case "get_health":
          return Promise.resolve(observingDump());
        case "list_running_apps":
        case "list_mic_devices":
          return Promise.resolve([]);
        default:
          return Promise.resolve(null);
      }
    });
    const app = await import("../src/main");

    app.start();
    await flushAsync();

    expect(byId(ids["settings.window.root"])).not.toBeNull();
    flushAnimationFrames();
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "view_rendered")).toHaveLength(1);

    app.__test__.rerender();
    flushAnimationFrames();
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "view_rendered")).toHaveLength(1);
  });

  it("registers the health listener before get_health resolves", async () => {
    const { invoke, listen } = await mockedTauri();
    invoke.mockImplementation((cmd) => {
      if (cmd === "get_health") {
        return new Promise((resolve) => setTimeout(() => resolve(observingDump()), 0));
      }
      if (cmd === "list_running_apps" || cmd === "list_mic_devices") {
        return Promise.resolve([]);
      }
      return Promise.resolve(null);
    });
    const app = await import("../src/main");

    app.start();

    expect(listen).toHaveBeenCalledWith("health://changed", expect.any(Function));
    await flushAsync();
  });

  it("boots the about entrypoint through get_health only", async () => {
    const { invoke, getCurrentWindow } = await mockedTauri();
    getCurrentWindow.mockReturnValue({ label: "about" } as ReturnType<typeof getCurrentWindow>);
    invoke.mockImplementation((cmd) => {
      if (cmd === "get_health") {
        return Promise.resolve(observingDump());
      }
      return Promise.resolve(null);
    });
    const app = await import("../src/main");

    app.start();
    await flushAsync();
    flushAnimationFrames();

    expect(byId(ids["about.window.root"])).not.toBeNull();
    const commands = invoke.mock.calls.map(([cmd]) => cmd);
    expect(commands).toContain("get_health");
    expect(commands).toContain("view_rendered");
    expect(commands).not.toContain("storage_info");
    expect(commands).not.toContain("update_get");
    expect(commands).not.toContain("get_exclusions");
    expect(commands).not.toContain("get_hotkey");
    expect(commands).not.toContain("get_mic_config");
    expect(commands).not.toContain("get_retention");
  });
});
