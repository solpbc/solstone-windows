// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, expect, it, vi } from "vitest";
import { createLogHelp, type LogPreview } from "../src/logs";
import { automationContract } from "../src/lib/contract";

const calls = vi.mocked(invoke);
const ids = automationContract.automation_ids;
const first: LogPreview = { id: 7, document: "logs\n<img src=x onerror=bad()>\n", state: "ready", entries: 1 };
let pane: HTMLElement;
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
function button(name: "read" | "save"): HTMLButtonElement {
  return pane.querySelector(`[data-automation-id="${ids[`settings.logs.${name}`]}"]`)!;
}

beforeEach(() => {
  calls.mockReset();
  pane = createLogHelp();
  document.body.replaceChildren(pane);
});

it("does not read or save until the owner acts, then saves the displayed snapshot id", async () => {
  expect(calls).not.toHaveBeenCalled();
  expect(button("save").disabled).toBe(true);
  calls.mockResolvedValueOnce(first).mockResolvedValueOnce(true);
  button("read").click();
  await flush();
  expect(pane.querySelector("textarea")?.value).toBe(first.document);
  expect(pane.querySelector("img")).toBeNull();
  button("save").click();
  await flush();
  expect(calls.mock.calls).toEqual([
    ["read_recent_logs"],
    ["save_recent_logs", { previewId: first.id }],
  ]);
  expect(pane.querySelector("textarea")?.value).toBe(first.document);
});

it("clears the old snapshot when a refresh fails", async () => {
  calls.mockResolvedValueOnce(first).mockRejectedValueOnce(new Error("unreadable"));
  button("read").click(); await flush();
  button("read").click(); await flush();
  expect(button("save").disabled).toBe(true);
  expect(pane.querySelector("textarea")?.value).toBe("");
  expect(pane.querySelector("textarea")?.hidden).toBe(true);
});

it("permits a partial document and retains it after save failure or cancellation", async () => {
  calls.mockResolvedValueOnce({ ...first, state: "partial" })
    .mockRejectedValueOnce(new Error("write failed")).mockResolvedValueOnce(false);
  button("read").click(); await flush();
  expect(button("save").disabled).toBe(false);
  button("save").click(); await flush();
  expect(button("save").disabled).toBe(false);
  expect(pane.querySelector("textarea")?.value).toBe(first.document);
  button("save").click(); await flush();
  expect(pane.querySelector("textarea")?.value).toBe(first.document);
  expect(calls.mock.calls.filter(([name]) => name === "read_recent_logs")).toHaveLength(1);
});

it.each(["empty", "missing", "unreadable"] as const)("does not offer an export for %s logs", async (state) => {
  calls.mockResolvedValueOnce({ ...first, state, entries: 0 });
  button("read").click(); await flush();
  expect(button("save").hidden).toBe(true);
  expect(button("save").disabled).toBe(true);
});


it("keeps the displayed document and selection through health renders", async () => {
  const app = await import("../src/main");
  const { observingDump } = await import("./fixtures");
  app.__test__.reset();
  const root = document.createElement("div");
  root.id = "app";
  document.body.replaceChildren(root);
  app.__test__.setRoot(root);
  app.__test__.setRoute("help");
  const health = observingDump();
  app.__test__.renderSettings(health);
  calls.mockResolvedValueOnce(first);
  root.querySelector<HTMLButtonElement>(`[data-automation-id="${ids["settings.logs.read"]}"]`)!.click();
  await flush();
  const preview = root.querySelector("textarea")!;
  preview.setSelectionRange(2, 8);
  app.__test__.renderSettings(health);
  expect(root.querySelector("textarea")).toBe(preview);
  expect(preview.value).toBe(first.document);
  expect([preview.selectionStart, preview.selectionEnd]).toEqual([2, 8]);
  expect(calls.mock.calls.filter(([name]) => name === "read_recent_logs")).toHaveLength(1);
});
