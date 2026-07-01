// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { vi } from "vitest";

window.matchMedia = ((query: string) =>
  ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent() {
      return false;
    },
  }) as unknown as MediaQueryList);

const rafCbs: FrameRequestCallback[] = [];

globalThis.requestAnimationFrame = (cb: FrameRequestCallback): number => {
  rafCbs.push(cb);
  return rafCbs.length;
};

globalThis.cancelAnimationFrame = () => {};

export function flushAnimationFrames(): void {
  const cbs = rafCbs.splice(0);
  const now = typeof performance !== "undefined" ? performance.now() : 0;
  for (const cb of cbs) {
    cb(now);
  }
}

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: vi.fn(() => ({ label: "settings" })),
}));
