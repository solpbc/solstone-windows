// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { describe, expect, it, vi } from "vitest";

import { renderDesk, type DeskActions, type DeskView } from "../src/shell/view";

function shelfPageView(width: DeskView["width"]): DeskView {
  return {
    width,
    row: "help",
    shelfOpen: false,
    phoneShowsPane: false,
    greeting: "",
    pill: "",
    journalName: "",
    journalChips: null,
    pausedLine: null,
    badges: [],
    tiles: [],
    rows: [],
    paneTitle: "",
    titleGlyph: null,
    paneIsStatus: true,
    connected: false,
    leadHeadline: "",
    leadLines: [],
    kept: null,
    facts: [],
    showConnect: false,
    showTechnical: false,
    shelfPage: true,
    pageVerdict: null,
  };
}

function actions(onBackHome = vi.fn()): DeskActions {
  return {
    onShelf: vi.fn(),
    onShelfToggle: vi.fn(),
    onBackHome,
    onPill: vi.fn(),
    onResume: vi.fn(),
    onClosePane: vi.fn(),
    onTechnical: vi.fn(),
    onTile: vi.fn(),
    onSwitch: vi.fn(),
    onStatus: vi.fn(),
  };
}

const parts = () => ({
  notice: null,
  unread: null,
  saveRetry: null,
  paneExtra: null,
  shelfBody: document.createElement("section"),
});

describe("a shelf page", () => {
  it.each(["narrower", "phone"] as const)("has a way back home at %s width, where the rail is hidden", (width) => {
    const onBackHome = vi.fn();
    const shell = renderDesk(shelfPageView(width), parts(), actions(onBackHome));
    const back = shell.querySelector<HTMLButtonElement>(".desk-deck .desk-back");
    expect(back).not.toBeNull();
    back!.click();
    expect(onBackHome).toHaveBeenCalledTimes(1);
  });

  it("leaves home to the rail at full width", () => {
    const shell = renderDesk(shelfPageView("full"), parts(), actions());
    expect(shell.querySelector(".desk-deck .desk-back")).toBeNull();
  });
});
