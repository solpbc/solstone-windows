// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump } from "./fixtures";

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

function pairedDump(upload: {
  uploaded_segments?: number;
  pending_segments?: number;
  failed_segments?: number;
  quarantined_segments?: number;
  last_error?: string | null;
}) {
  const base = observingDump();
  return {
    ...base,
    sync: {
      ...base.sync,
      upload: {
        ...base.sync.upload,
        uploaded_segments: 3,
        pending_segments: 1,
        failed_segments: 0,
        quarantined_segments: 0,
        last_error: null,
        ...upload,
      },
    },
  };
}

describe("paired send-state", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("does not alarm on a retryable last_error with nothing quarantined", () => {
    const dump = pairedDump({ last_error: "relay_unpaid", failed_segments: 0 });
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const glance = homeStatusStripJournalValue();
    expect(glance).toBe("3 delivered · 1 pending");
    expect(glance).not.toBe("sync needs attention");
    expect(glance).not.toContain("last error:");
    expect(glance).not.toContain("relay_unpaid");
  });

  it("presents a retrying backlog as waiting to sync, not needs attention", () => {
    const dump = pairedDump({ failed_segments: 2, last_error: "io" });
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(homeStatusStripJournalValue()).toBe("waiting to sync");
  });

  it("reserves needs attention for quarantined segments", () => {
    const dump = pairedDump({
      quarantined_segments: 1,
      failed_segments: 2,
      last_error: "http_400",
    });
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(homeStatusStripJournalValue()).toBe("1 needs attention");
  });

  it("keeps raw transport codes off the journal sync row", () => {
    const dump = pairedDump({
      failed_segments: 2,
      quarantined_segments: 4,
      last_error: "http_400",
    });
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    const row = present(ids["settings.status.upload.state"]).textContent ?? "";
    expect(row).toBe("3 delivered · 1 pending · 2 retrying · 4 need attention");
    expect(row).not.toContain("last error:");
    expect(row).not.toContain("http_400");
  });
});
