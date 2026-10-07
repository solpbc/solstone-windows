// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump, sampleMarkSpec } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);
const firstCid = `sha256:${"a".repeat(64)}`;
const secondCid = `sha256:${"b".repeat(64)}`;

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

function migration(overrides: Record<string, unknown> = {}) {
  return {
    phase: "offered",
    revision: 4,
    pairing_generation: Array.from({ length: 32 }, (_, index) => index),
    state: null,
    replaced_cid: null,
    decision_choice: null,
    decision_result: null,
    offer_available: true,
    offer_binding: "certificate-binding",
    ...overrides,
  };
}

function pairedDump() {
  const dump = observingDump();
  return {
    ...dump,
    sync: {
      ...dump.sync,
      pairing: {
        ...dump.sync.pairing,
        phase: "paired" as const,
        mark: sampleMarkSpec("one", "two", "#3b82f6"),
      },
    },
  };
}

function paint() {
  const dump = pairedDump();
  app.__test__.setRoute("journal");
  app.__test__.setHealth(dump);
  app.__test__.renderSettings(dump);
}

const action = (key: string): HTMLButtonElement => {
  const node = document.querySelector<HTMLButtonElement>(
    `[data-migration-action="${key}"]`,
  );
  expect(node).not.toBeNull();
  return node as HTMLButtonElement;
};

describe("fresh-pair replacement offer settings flow", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("renders the one-time offer from saved state and defers without a decision", async () => {
    app.__test__.setMigration({ migration: migration(), issue: null });
    paint();

    expect(
      document.querySelector('[data-migration-key="migration.replace_offer.title"]'),
    ).not.toBeNull();
    action("migration.replace_offer.defer").click();

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("pairing_migration_offer_dismiss", {
        expectedBinding: "certificate-binding",
        expectedGeneration: Array.from({ length: 32 }, (_, index) => index),
        expectedRevision: 4,
      });
    });
    expect(invokeMock).not.toHaveBeenCalledWith(
      "pairing_migration_decide",
      expect.anything(),
    );
  });

  it("keeps both devices with a new-device decision", async () => {
    app.__test__.setMigration({ migration: migration(), issue: null });
    invokeMock.mockResolvedValueOnce({ migration: null, issue: null } as never);
    paint();

    action("migration.replace_offer.keep").click();

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("pairing_migration_decide", {
        choice: "new_device",
        replacesCid: null,
      });
    });
  });

  it("keeps duplicate display labels distinct and submits the selected CID", async () => {
    app.__test__.setMigration({ migration: migration(), issue: null });
    app.__test__.setMigrationDevices([
      { cid: firstCid, display_label: "<em>shared label</em>" },
      { cid: secondCid, display_label: "<em>shared label</em>" },
    ]);
    app.__test__.setMigrationFlow("picker");
    paint();

    const rows = document.querySelectorAll<HTMLButtonElement>(
      '[data-migration-action="select_device"]',
    );
    expect(rows).toHaveLength(2);
    expect(rows[1].textContent).toBe("<em>shared label</em>");
    expect(rows[1].querySelector("em")).toBeNull();
    rows[1].click();
    paint();
    action("migration.replace_confirm.replace").click();

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("pairing_migration_decide", {
        choice: "replace_device",
        replacesCid: secondCid,
      });
    });
  });

  it("renders and retries an unanswered decision without changing its choice", async () => {
    app.__test__.setMigration({
      migration: migration({ phase: "decision_unknown", decision_result: "unknown", offer_available: false }),
      issue: null,
    });
    paint();

    expect(
      document.querySelector('[data-migration-key="migration.decision_unknown.title"]'),
    ).not.toBeNull();
    expect(document.querySelector('[data-migration-key="migration.pending.value"]')).not.toBeNull();
    action("migration.decision_unknown.action").click();

    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("pairing_migration_state");
    });
    expect(invokeMock).not.toHaveBeenCalledWith(
      "pairing_migration_decide",
      expect.anything(),
    );
  });

  it("renders an issue action by its migration key", () => {
    app.__test__.setMigration({ migration: null, issue: "unsupported" });
    const dump = pairedDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);
    app.__test__.renderSettings(dump);

    expect(
      document.querySelector('[data-migration-key="migration.unsupported.title"]'),
    ).not.toBeNull();
    expect(action("migration.unsupported.action")).not.toBeNull();
  });

  it("opens the replacement chooser after a target is no longer available", async () => {
    app.__test__.setMigration({
      migration: migration({ offer_available: false }),
      issue: "target_missing",
    });
    invokeMock.mockResolvedValueOnce([
      { cid: firstCid, display_label: "first device" },
    ] as never);
    paint();

    action("migration.target_missing.action").click();

    await vi.waitFor(() => {
      expect(document.querySelector('[data-migration-key="migration.picker.title"]')).not.toBeNull();
    });
    expect(invokeMock).toHaveBeenCalledWith("pairing_migration_devices");
    expect(invokeMock).not.toHaveBeenCalledWith(
      "pairing_migration_decide",
      expect.anything(),
    );
  });

  it("offers only keep-both or a chosen replacement, never a same-device choice", () => {
    app.__test__.setMigration({ migration: migration(), issue: null });
    paint();

    expect(action("migration.replace_offer.pick")).not.toBeNull();
    expect(document.querySelector('[data-migration-key^="migration.choice."]')).toBeNull();
  });

  it("stays hidden once the offer is no longer available", () => {
    app.__test__.setMigration({ migration: migration({ offer_available: false }), issue: null });
    paint();

    expect(document.querySelector("[data-migration-key]")).toBeNull();
  });
});
