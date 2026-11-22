// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The native shell selects once per process. Never infer language from WebView2.
import { render, integer, cardinal } from "../../i18n/generated/esm/windows/api.mjs";
import { argumentsById } from "../../i18n/arguments";

type Bootstrap = { locale: string; formatLocale: string };
const native = (globalThis as typeof globalThis & { __SOLSTONE_LOCALE__?: Bootstrap }).__SOLSTONE_LOCALE__;
export const locale = native?.locale ?? "en-US";
export const formatLocale = native?.formatLocale ?? locale;

export function t(id: string, values: Record<string, string | number | boolean> = {}): string {
  const args = (argumentsById[id] ?? []).map(([name, type]) => {
    const value = values[name];
    if (value === undefined) throw new Error(`Missing message argument: ${id}.${name}`);
    return [name, type === "cardinal" ? cardinal(value) : type === "integer" ? integer(value) : value];
  });
  return render(locale, id, args, { useIsolating: locale !== "en-US" });
}

export function stateWord(word: string): string {
  const ids: Record<string, string> = {
    sending: "windows.ui.sendingword",
    waiting: "windows.ui.waitingword",
    off: "windows.desktop.shared.app.microphonemodeoff",
    on: "windows.desktop.shared.app.settingsdiagnosticson",
    paused: "windows.ui.pausedword",
    "setting up": "windows.ui.settingupword",
    "ready to set up": "windows.ui.readytosetupword",
    "ready to review": "windows.import.readytoreviewword",
    expired: "windows.import.expiredword",
    "checking…": "windows.ui.checkingword",
    "needs attention": "windows.ui.needsattentionword",
    unavailable: "windows.ui.unavailableword",
    error: "windows.ui.errorword",
  };
  return ids[word] ? t(ids[word]) : word;
}

export function number(value: number, options: Intl.NumberFormatOptions = {}): string {
  // Source English used explicit en-US number formatting before catalog adoption.
  // Preserve those bytes even when the owner's regional format differs.
  return new Intl.NumberFormat(locale === "en-US" ? "en-US" : formatLocale, options).format(value);
}

export function applyDocumentLocale(): void {
  document.documentElement.lang = locale;
  // Intl.Locale's direction data is not available on every supported WebView2.
  const info = new Intl.Locale(locale).maximize() as Intl.Locale & {getTextInfo?: () => {direction: string}};
  const direction = info.getTextInfo?.().direction;
  document.documentElement.dir = direction ?? (["Arab", "Hebr", "Syrc", "Thaa", "Nkoo", "Adlm", "Rohg", "Mand", "Samr"].includes(info.script ?? "") ? "rtl" : "ltr");
}
