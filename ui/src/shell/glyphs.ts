// Shelf and tile marks. Stroke icons, one accent, no system glyphs.

export type GlyphName =
  | "audio"
  | "screen"
  | "browsers"
  | "obsidian"
  | "plus"
  | "home"
  | "journal"
  | "device"
  | "updates"
  | "help"
  | "side";

const PATHS: Record<GlyphName, string> = {
  audio: '<path d="M3 11v2M7 8v8M11 4v16M15 8v8M19 11v2"/>',
  screen: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
  browsers:
    '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.6 2.4 4 5.6 4 9s-1.4 6.6-4 9c-2.6-2.4-4-5.6-4-9s1.4-6.6 4-9z"/>',
  obsidian: '<path d="M12 3l7 6-2.5 11h-9L5 9z"/><path d="M12 3v9l4.5 8M12 12l-7-3"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  home: '<path d="M4 11l8-7 8 7v8a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 19z"/><path d="M10 20.5V14h4v6.5"/>',
  journal:
    '<path d="M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5z"/><path d="M5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3"/>',
  device: '<rect x="4" y="5" width="16" height="11" rx="1.5"/><path d="M2.5 19.5h19"/>',
  updates: '<path d="M12 4v10M8 10.5l4 4 4-4"/><path d="M4 12a8 8 0 1 0 16 0"/>',
  help: '<circle cx="12" cy="12" r="9"/><path d="M9.5 9.5a2.5 2.5 0 1 1 3.6 2.3c-.8.4-1.1 1-1.1 1.7M12 17h.01"/>',
  side: '<rect x="3" y="5" width="18" height="14" rx="2.5"/><path d="M9 5v14"/>',
};

export function glyphForTile(id: string): GlyphName {
  if (id === "audio" || id === "screen" || id === "browsers" || id === "obsidian") return id;
  return "plus";
}

export function glyph(name: GlyphName): SVGElement {
  const parsed = new DOMParser().parseFromString(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" class="desk-glyph">${PATHS[name]}</svg>`,
    "image/svg+xml",
  );
  return document.importNode(parsed.documentElement, true) as unknown as SVGElement;
}
