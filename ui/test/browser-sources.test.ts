// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump, pausedDump } from "./fixtures";
import type { CounterDocument } from "../src/shell/derive";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

type BrowserStatusFixture = {
  capture: "unavailable" | "not_paired" | "permitted" | "paused" | "intake_off";
  delivery: "unknown" | "kept_locally" | "delivered" | "idle" | "failed";
  failure: string | null;
  connected: { brand: string; connected_at_ms: number }[];
  custody: { held_bytes: number; waiting: boolean; full: boolean };
  stores: string[];
};

function resetRoot(): HTMLDivElement {
  document.body.replaceChildren();
  const root = document.createElement("div");
  root.id = "app";
  root.setAttribute("data-automation-id", ids["settings.window.root"]);
  document.body.append(root);
  app.__test__.reset();
  app.__test__.setRoot(root);
  invokeMock.mockReset();
  return root;
}

function browserStatus(overrides: Partial<BrowserStatusFixture> = {}): BrowserStatusFixture {
  return {
    capture: "permitted",
    delivery: "idle",
    failure: null,
    connected: [],
    custody: { held_bytes: 0, waiting: false, full: false },
    stores: ["chrome", "edge"],
    ...overrides,
  };
}

function renderSources(status: BrowserStatusFixture): void {
  const dump = observingDump();
  app.__test__.setRoute("sources");
  app.__test__.setHealth(dump);
  app.__test__.setBrowser(status);
  app.__test__.renderSettings(dump);
}

describe("browser extension sources offer", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("renders offer and stores in order when permitted and not connected", () => {
    renderSources(
      browserStatus({
        connected: [],
        capture: "permitted",
        stores: ["chrome", "edge"],
      }),
    );
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.offer"]}"]`,
      ),
    ).not.toBeNull();
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.stores"]}"]`,
      ),
    ).not.toBeNull();
    const buttons = Array.from(
      document.querySelectorAll<HTMLElement>("[data-brand]"),
    );
    expect(buttons.map((b) => b.dataset.brand)).toEqual(["chrome", "edge"]);
  });

  it("hides offer and stores when a browser is connected", () => {
    renderSources(
      browserStatus({
        connected: [{ brand: "chrome", connected_at_ms: 1000 }],
        capture: "permitted",
        stores: ["chrome", "edge"],
      }),
    );
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.offer"]}"]`,
      ),
    ).toBeNull();
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.stores"]}"]`,
      ),
    ).toBeNull();
  });

  it("hides offer and stores when not paired", () => {
    renderSources(
      browserStatus({
        connected: [],
        capture: "not_paired",
        stores: ["chrome", "edge"],
      }),
    );
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.offer"]}"]`,
      ),
    ).toBeNull();
    expect(
      document.querySelector(
        `[data-automation-id="${ids["settings.sources.browser.stores"]}"]`,
      ),
    ).toBeNull();
  });

  it("invokes open_browser_store with brand argument object on button click", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_browser_store") {
        return true;
      }
      return null;
    });

    renderSources(
      browserStatus({
        connected: [],
        capture: "permitted",
        stores: ["chrome", "edge"],
      }),
    );

    const chromeButton =
      document.querySelector<HTMLButtonElement>('[data-brand="chrome"]');
    expect(chromeButton).not.toBeNull();
    chromeButton!.click();

    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(
        ([cmd]) => cmd === "open_browser_store",
      );
      expect(calls.length).toBeGreaterThan(0);
      expect(calls[0][1]).toEqual({ brand: "chrome" });
    });
  });

  it("renders store error when open_browser_store resolves false", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_browser_store") {
        return false;
      }
      return null;
    });

    renderSources(
      browserStatus({
        connected: [],
        capture: "permitted",
        stores: ["chrome", "edge"],
      }),
    );

    const chromeButton =
      document.querySelector<HTMLButtonElement>('[data-brand="chrome"]');
    expect(chromeButton).not.toBeNull();
    chromeButton!.click();

    await vi.waitFor(() => {
      expect(
        document.querySelector(
          `[data-automation-id="${ids["settings.sources.browser.storeError"]}"]`,
        ),
      ).not.toBeNull();
    });
  });

  it("does not render element for unlisted store", () => {
    renderSources(
      browserStatus({
        connected: [],
        capture: "permitted",
        stores: ["edge"],
      }),
    );
    expect(document.querySelector('[data-brand="chrome"]')).toBeNull();
    expect(document.querySelector('[data-brand="edge"]')).not.toBeNull();
  });
});

function deskPairedDump(paused = false) {
  const base = paused ? pausedDump(900) : observingDump();
  return {
    ...base,
    sync: {
      ...base.sync,
      journal_link: "connected" as const,
    },
    setup: {
      sequence_enabled: true,
      mic: true,
      system_audio: true,
      screen: true,
      start_at_login: false,
      wizard: "done" as const,
      notice_dismissed: true,
    },
  };
}

function counterDoc(browsersTotal = 0, browsersLast: number | null = null): CounterDocument {
  return {
    started: "2026-11-25",
    audio: { seconds: 0, last: null },
    screen: { seconds: 0, last: null },
    browsers: { total: browsersTotal, last: browsersLast },
    obsidian: { total: 0, last: null },
    screenshots: { total: 0, last: null },
    imports: { total: 0, last: null },
  };
}

describe("desktop browsers tile, pane, cold boot, and live poll", () => {
  beforeEach(() => {
    resetRoot();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  });

  it("shows ready to set up and pane store actions when paired with no extension, custody, or counters", () => {
    const dump = deskPairedDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setCounters(counterDoc(0, null));
    app.__test__.setBrowser(browserStatus({ connected: [], capture: "permitted" }));
    app.__test__.renderSettings(dump);

    const tile = document.querySelector('[data-tile="browsers"]');
    expect(tile?.querySelector(".desk-tile-word")?.textContent).toBe("ready to set up");
    expect(tile?.querySelector(".desk-tile-sub")?.textContent).toBe("add the solstone extension.");

    app.__test__.openDeskSource("browsers");
    const pane = document.querySelector(".desk-pane");
    expect(pane?.querySelector(".desk-reason")?.textContent).toContain("solstone extension");
    const actions = Array.from(pane?.querySelectorAll(".desk-actions button") ?? []).map(
      (btn) => btn.textContent,
    );
    expect(actions).toEqual(["add to Chrome", "add to Edge"]);
  });

  it("loads counters before initial boot render so closed configured browsers never flash ready to set up", async () => {
    vi.resetModules();
    const core = await import("@tauri-apps/api/core");
    const event = await import("@tauri-apps/api/event");
    const win = await import("@tauri-apps/api/window");
    const freshInvoke = vi.mocked(core.invoke);
    const listen = vi.mocked(event.listen);
    const getCurrentWindow = vi.mocked(win.getCurrentWindow);
    freshInvoke.mockReset();
    listen.mockReset();
    getCurrentWindow.mockReset();
    listen.mockResolvedValue(() => {});
    getCurrentWindow.mockReturnValue({ label: "settings" } as ReturnType<typeof win.getCurrentWindow>);

    document.body.replaceChildren();
    const rootEl = document.createElement("div");
    rootEl.id = "app";
    rootEl.setAttribute("data-automation-id", ids["settings.window.root"]);
    document.body.append(rootEl);

    const observedWords: string[] = [];
    const observer = new MutationObserver(() => {
      const word = document.querySelector('[data-tile="browsers"] .desk-tile-word')?.textContent;
      if (word) observedWords.push(word);
    });
    observer.observe(rootEl, { childList: true, subtree: true, characterData: true });

    const setIntervalSpy = vi
      .spyOn(globalThis, "setInterval")
      .mockImplementation(() => 0 as unknown as ReturnType<typeof setInterval>);

    try {
      freshInvoke.mockImplementation(async (cmd: string) => {
        if (cmd === "get_health") return deskPairedDump(false);
        if (cmd === "get_source_counters") return counterDoc(3, 1_700_000_000);
        if (cmd === "browser_status") {
          return browserStatus({ connected: [], capture: "permitted" });
        }
        if (cmd === "list_running_apps" || cmd === "list_mic_devices") return [];
        return null;
      });

      const freshApp = await import("../src/main");
      freshApp.start();
      await vi.waitFor(() => {
        expect(document.querySelector('[data-tile="browsers"] .desk-tile-word')?.textContent).toBe("on");
      });
      expect(observedWords).not.toContain("ready to set up");
      expect(observedWords).toContain("on");
    } finally {
      observer.disconnect();
      setIntervalSpy.mockRestore();
    }
  });

  it("polls browser_status and transitions from ready to set up to on when an extension connects", async () => {
    let current = browserStatus({ connected: [], capture: "permitted" });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "browser_status") return current;
      if (cmd === "get_source_counters") return counterDoc(0, null);
      return null;
    });

    const dump = deskPairedDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setCounters(counterDoc(0, null));
    app.__test__.setBrowser(current);
    app.__test__.renderSettings(dump);
    app.__test__.openDeskSource("browsers");

    expect(document.querySelector('[data-tile="browsers"] .desk-tile-word')?.textContent).toBe(
      "ready to set up",
    );
    expect(document.querySelector(".desk-pane .desk-actions")).not.toBeNull();

    current = browserStatus({
      connected: [{ brand: "chrome", connected_at_ms: 1_000 }],
      capture: "permitted",
    });
    await app.__test__.pollBrowserStatus();

    await vi.waitFor(() => {
      expect(document.querySelector('[data-tile="browsers"] .desk-tile-word')?.textContent).toBe("on");
    });
    expect(document.querySelector('[data-tile="browsers"] .desk-tile-sub')?.textContent).toBe("Chrome");
    expect(document.querySelector(".desk-pane .desk-actions")).toBeNull();
    expect(document.querySelector(".desk-pane")?.textContent).toContain("Chrome");
  });

  it("keeps needs attention on tile and pane for full custody or failed delivery even while paused with no connected browsers", () => {
    const paused = deskPairedDump(true);
    app.__test__.setRoute("home");
    app.__test__.setHealth(paused);
    app.__test__.setCounters(counterDoc(0, null));

    for (const faulted of [
      browserStatus({
        capture: "paused",
        delivery: "kept_locally",
        connected: [],
        custody: { held_bytes: 4096, waiting: true, full: true },
      }),
      browserStatus({
        capture: "paused",
        delivery: "failed",
        connected: [],
        custody: { held_bytes: 512, waiting: true, full: false },
      }),
    ]) {
      app.__test__.setBrowser(faulted);
      app.__test__.renderSettings(paused);
      expect(document.querySelector('[data-tile="browsers"] .desk-tile-word')?.textContent).toBe(
        "needs attention",
      );
      app.__test__.openDeskSource("browsers");
      expect(document.querySelector(".desk-pane .desk-reason")).not.toBeNull();
      expect(document.querySelector(".desk-pane .desk-actions")).toBeNull();
    }
  });
});
