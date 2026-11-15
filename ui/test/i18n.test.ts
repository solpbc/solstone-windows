// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
import { afterEach, expect, it, vi } from "vitest";

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });

it("keeps existing English count formatting when Windows regional format differs", async () => {
  vi.resetModules();
  vi.stubGlobal("__SOLSTONE_LOCALE__", {locale:"en-US", formatLocale:"de-DE"});
  const {number} = await import("../src/i18n");
  expect(number(12345)).toBe("12,345");
  expect(number(12345, {useGrouping:false})).toBe("12345");
});
