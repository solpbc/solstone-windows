// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";

import * as app from "../src/main";
import { observingDump } from "./fixtures";

const APP_LINE = "windows app 2.0.16 · windows 11 26100 · x86_64";
const JOURNAL_LINE = "journal 1.2.3 · ubuntu 24.04 · x86_64 · last seen 2 minutes ago";
const ABOUT_BLOCK = `${APP_LINE}\n${JOURNAL_LINE}`;

function prepareAbout(): ReturnType<typeof observingDump> {
  document.body.replaceChildren();
  const root = document.createElement("div");
  root.id = "app";
  document.body.append(root);
  app.__test__.reset();
  app.__test__.setRoot(root);
  const dump = observingDump();
  dump.sync.about_app_line = APP_LINE;
  dump.sync.journal_display_line = JOURNAL_LINE;
  dump.sync.about_block = ABOUT_BLOCK;
  app.__test__.setHealth(dump);
  app.__test__.renderAbout(dump);
  return dump;
}

function copyButton(): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) =>
      candidate.textContent === "copy" ||
      candidate.textContent === "copied" ||
      candidate.textContent === "couldn't copy. select the text and copy it.",
  );
  expect(button).toBeInstanceOf(HTMLButtonElement);
  return button as HTMLButtonElement;
}

describe("About displayed copy", () => {
  beforeEach(() => {
    vi.useRealTimers();
    prepareAbout();
  });

  it("shows both frozen lines and copies their exact LF-separated bytes", async () => {
    const appLine = document.querySelector('[data-automation-id="about.version"]');
    const journalLine = document.querySelector('[data-automation-id="about.journalVersion"]');
    expect(appLine?.textContent).toBe(APP_LINE);
    expect(journalLine?.textContent).toBe(JOURNAL_LINE);
    expect(ABOUT_BLOCK.split("\n")).toHaveLength(2);
    expect(ABOUT_BLOCK).toContain("·");

    let finishWrite: () => void = () => {};
    const writer = vi.fn(
      (_value: string) =>
        new Promise<void>((resolve) => {
          finishWrite = resolve;
        }),
    );
    app.__test__.setClipboardWriter(writer);
    copyButton().click();
    await Promise.resolve();
    expect(copyButton().textContent).toBe("copy");
    finishWrite();
    await vi.waitFor(() => expect(copyButton().textContent).toBe("copied"));
    expect(writer).toHaveBeenCalledOnce();
    expect(writer).toHaveBeenCalledWith(ABOUT_BLOCK);
  });

  it("reports clipboard refusal with the exact selection instruction", async () => {
    app.__test__.setClipboardWriter(async () => {
      throw new Error("clipboard refused");
    });
    copyButton().click();
    await vi.waitFor(() =>
      expect(document.querySelector("button")?.textContent).toBe(
        "couldn't copy. select the text and copy it.",
      ),
    );
    expect(document.querySelector('[data-automation-id="about.version"]')?.textContent).toBe(
      APP_LINE,
    );
    expect(
      document
        .querySelector('[data-automation-id="about.version"]')
        ?.classList.contains("selectable"),
    ).toBe(true);
    expect(
      document.querySelector('[data-automation-id="about.journalVersion"]')?.textContent,
    ).toBe(JOURNAL_LINE);
    expect(
      document
        .querySelector('[data-automation-id="about.journalVersion"]')
        ?.classList.contains("selectable"),
    ).toBe(true);
  });

  it("keeps copied bytes stable after a clock advance", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    const writes: string[] = [];
    app.__test__.setClipboardWriter(async (value) => {
      writes.push(value);
    });
    copyButton().click();
    await vi.waitFor(() => expect(copyButton().textContent).toBe("copied"));

    vi.setSystemTime(new Date("2026-01-04T00:00:00Z"));
    const dump = observingDump();
    dump.sync.about_app_line = APP_LINE;
    dump.sync.journal_display_line = JOURNAL_LINE;
    dump.sync.about_block = ABOUT_BLOCK;
    app.__test__.renderAbout(dump);
    copyButton().click();
    await vi.waitFor(() => expect(copyButton().textContent).toBe("copied"));

    expect(writes).toEqual([ABOUT_BLOCK, ABOUT_BLOCK]);
    expect(dump.sync.about_block).toBe(ABOUT_BLOCK);
  });
});
