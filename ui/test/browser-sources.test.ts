// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

type BrowserStatusFixture = {
  capture: "unavailable" | "not_paired" | "permitted" | "paused" | "intake_off";
  delivery: "unknown" | "kept_locally" | "delivered" | "idle" | "failed";
  failure: string | null;
  connected: { brand: string; connected_at_ms: number }[];
  custody: { held_bytes: number; waiting: boolean };
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
    custody: { held_bytes: 0, waiting: false },
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
