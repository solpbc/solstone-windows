// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The journal's mark: two tinted glyph chips above two words, in a card. Every
// mark in this app comes from here, drawn to the same geometry as the journal's
// own pages, because the owner compares the two by eye. The card stays
// warm-bright in a dark window too, so its inks never switch.

export interface MarkColor {
  name: string;
  hex: string;
}

export interface MarkIconSpec {
  name: string;
  svg: string;
  color: MarkColor;
  rot: number;
}

export interface MarkRenderSpec {
  icon1: MarkIconSpec;
  icon2: MarkIconSpec;
  words: [string, string];
}

/** What the card shows when there is no mark it can draw. */
export type MarkFallback =
  // The owner's own journal has no identity yet.
  | "generic"
  // The owner's journal has an identity that can't be read right now.
  | "unavailable"
  // A stranger offered no identity. Neutral ink, because it isn't ours.
  | "not-presented";

const SVG_NS = "http://www.w3.org/2000/svg";
const GENERIC_CHIPS = ["#B06A1A", "#A57F0E"];
const ALLOWED_SHAPES = new Set(["path", "circle", "rect", "line", "polyline", "polygon", "ellipse"]);
const SHAPE_ATTRIBUTES = [
  "d", "cx", "cy", "r", "rx", "ry", "x", "y", "width", "height", "x1", "y1", "x2", "y2", "points",
];

export const MARK_CSS = `
.mark-card {
  --mark-neutral-border: #ECE3D0;
  --mark-neutral-fill: #FEFCF8;
  --mark-neutral-ink: #6E6453;
  display: inline-flex;
  flex-direction: column;
  align-items: center;
  justify-self: start;
  gap: 10px;
  box-sizing: border-box;
  max-width: 100%;
  padding: 18px 22px;
  background: #fffdf9;
  border: 1px solid #e7d8c6;
  border-radius: 12px;
}
.mark-chips { display: flex; gap: 13px; align-items: center; height: 76px; }
.mark-chip {
  box-sizing: border-box;
  width: 56px;
  height: 56px;
  border: 2px solid;
  border-radius: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
}
/* Paper fills the chip and runs under the dashes, so every gap sits on paper. */
.mark-chip.dashed {
  border-style: dashed;
  background: #FEFCF8;
  box-shadow: 0 0 0 2px #FEFCF8;
}
.mark-chip.rotated { transform: rotate(45deg); }
.mark-chip svg { width: 58%; height: 58%; }
.mark-chip-unknown { font-size: 24px; font-weight: 700; color: var(--mark-neutral-ink); }
.mark-words {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  font-family: "Comfortaa", "Segoe UI Variable Display", "Segoe UI", system-ui, sans-serif;
  font-weight: 700;
  font-size: 19px;
  line-height: 1.3;
  color: #1A1A1A;
  white-space: nowrap;
  overflow-wrap: normal;
}
.mark-sep { color: var(--mark-neutral-ink); margin: 0 0.3em; }
`;

function validMark(mark: MarkRenderSpec | null | undefined): mark is MarkRenderSpec {
  if (!mark || !Array.isArray(mark.words) || mark.words.length !== 2) return false;
  if (!mark.words.every((word) => typeof word === "string" && word.trim())) return false;
  return [mark.icon1, mark.icon2].every(
    (icon) =>
      icon &&
      typeof icon.svg === "string" &&
      icon.svg.trim() &&
      icon.color &&
      typeof icon.color.name === "string" &&
      icon.color.name.trim() &&
      /^#[0-9a-fA-F]{6}$/.test(icon.color.hex) &&
      (icon.rot === 0 || icon.rot === 45),
  );
}

/** A mark with everything it needs to be drawn whole. Never a half-drawn one. */
export function displayableMark(mark: MarkRenderSpec | null | undefined): mark is MarkRenderSpec {
  return validMark(mark) && glyphShapes(mark.icon1.svg).length > 0 && glyphShapes(mark.icon2.svg).length > 0;
}

// The glyph markup, read as shapes rather than inserted as markup: only the
// drawing elements a glyph uses are kept.
function glyphShapes(svg: string): Element[] {
  const doc = new DOMParser().parseFromString(`<svg xmlns="${SVG_NS}">${svg}</svg>`, "image/svg+xml");
  if (doc.querySelector("parsererror")) return [];
  return [...doc.documentElement.children].filter((el) => ALLOWED_SHAPES.has(el.localName) && hasGeometry(el));
}

function hasGeometry(el: Element): boolean {
  // Read from the attributes. These elements sit in a parsed document that is
  // never rendered, where the browser's geometry calls throw for everything but
  // a path, so they would drop every circle and rect from a glyph.
  const value = (name: string): string => el.getAttribute(name)?.trim() || "";
  const number = (name: string): number => Number(value(name));
  const positive = (name: string): boolean => Number.isFinite(number(name)) && number(name) > 0;
  switch (el.localName) {
    case "path": {
      const path = value("d");
      const numbers = path.match(/[+-]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][+-]?\d+)?/g) || [];
      return (
        /^[Mm]/.test(path) &&
        numbers.every((n) => Number.isFinite(Number(n))) &&
        (/[LlHhVvCcSsQqTtAa]/.test(path) || numbers.length >= 4)
      );
    }
    case "circle":
      return positive("r");
    case "ellipse":
      return positive("rx") && positive("ry");
    case "rect":
      return positive("width") && positive("height");
    case "line":
      return number("x1") !== number("x2") || number("y1") !== number("y2");
    case "polyline":
    case "polygon": {
      const points = value("points").split(/[\s,]+/).filter(Boolean).map(Number);
      return points.length >= 4 && points.length % 2 === 0 && points.every(Number.isFinite);
    }
    default:
      return false;
  }
}

function glyphSvgElement(svg: string, hex: string): SVGSVGElement {
  const el = document.createElementNS(SVG_NS, "svg");
  el.setAttribute("viewBox", "0 0 24 24");
  el.setAttribute("fill", "none");
  el.setAttribute("stroke", hex);
  el.setAttribute("stroke-width", "2");
  el.setAttribute("stroke-linecap", "round");
  el.setAttribute("stroke-linejoin", "round");
  el.setAttribute("aria-hidden", "true");
  for (const shape of glyphShapes(svg)) {
    const copy = document.createElementNS(SVG_NS, shape.localName);
    for (const name of SHAPE_ATTRIBUTES) {
      const attribute = shape.getAttribute(name);
      if (attribute !== null) copy.setAttribute(name, attribute);
    }
    el.appendChild(copy);
  }
  return el;
}

function chip(classes: string[] = []): HTMLDivElement {
  const el = document.createElement("div");
  el.classList.add("mark-chip", ...classes);
  return el;
}

function identityChip(icon: MarkIconSpec): HTMLDivElement {
  const el = chip(icon.rot === 45 ? ["rotated"] : []);
  el.style.borderColor = icon.color.hex;
  el.style.background = `${icon.color.hex}1f`;
  el.append(glyphSvgElement(icon.svg, icon.color.hex));
  return el;
}

function unavailableChip(): HTMLDivElement {
  const el = chip();
  el.style.borderColor = "var(--mark-neutral-border)";
  el.style.background = "var(--mark-neutral-fill)";
  const unknown = document.createElement("span");
  unknown.classList.add("mark-chip-unknown");
  unknown.textContent = "?";
  el.append(unknown);
  return el;
}

function genericChip(hex: string, rotated: boolean): HTMLDivElement {
  const el = chip(rotated ? ["dashed", "rotated"] : ["dashed"]);
  el.style.borderColor = hex;
  return el;
}

function notPresentedChip(): HTMLDivElement {
  const el = chip(["dashed"]);
  el.style.borderColor = "var(--mark-neutral-ink)";
  return el;
}

function wordsElement(first: string, second: string): HTMLDivElement {
  const words = document.createElement("div");
  words.classList.add("mark-words");
  const word = (value: string): HTMLSpanElement => {
    const span = document.createElement("span");
    span.textContent = value;
    return span;
  };
  const sep = word("·");
  sep.classList.add("mark-sep");
  words.append(word(first), sep, word(second));
  return words;
}

/**
 * The one mark card: `mark` when it can be drawn whole, otherwise `fallback`.
 * One accessible element, with one spoken name in the same order everywhere.
 */
export function renderMarkCard(
  mark: MarkRenderSpec | null | undefined,
  fallback: MarkFallback,
): HTMLDivElement {
  const card = document.createElement("div");
  card.classList.add("mark-card");
  card.setAttribute("role", "img");

  const chips = document.createElement("div");
  chips.classList.add("mark-chips");
  chips.setAttribute("aria-hidden", "true");

  let words: HTMLDivElement;
  let name: string;
  if (displayableMark(mark)) {
    const [first, second] = [mark.words[0].toLowerCase(), mark.words[1].toLowerCase()];
    chips.append(identityChip(mark.icon1), identityChip(mark.icon2));
    words = wordsElement(first, second);
    name = `${mark.icon1.color.name}, ${mark.icon2.color.name} · ${first} ${second}`;
  } else if (fallback === "unavailable") {
    chips.append(unavailableChip(), unavailableChip());
    words = wordsElement("mark", "unavailable");
    name = "your journal's mark, unavailable right now";
  } else if (fallback === "not-presented") {
    chips.append(notPresentedChip(), notPresentedChip());
    words = wordsElement("not", "presented");
    name = "no identity presented";
  } else {
    chips.append(genericChip(GENERIC_CHIPS[0], false), genericChip(GENERIC_CHIPS[1], true));
    words = wordsElement("your", "journal");
    name = "your journal, not set up yet";
  }
  words.setAttribute("aria-hidden", "true");
  card.setAttribute("aria-label", name);
  card.append(chips, words);
  return card;
}
