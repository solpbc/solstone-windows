// The desktop shell's document. Decisions live in derive.ts. This only paints.

import { glyph, glyphForTile, type GlyphName } from "./glyphs";

export interface DeskTile {
  id: string;
  name: string;
  word: string;
  sub: string;
  switchOn: boolean | null;
  switchId: string | null;
  switchLabel: string;
  selected: boolean;
}

export interface DeskStatusRow {
  id: string;
  name: string;
  nothingYet: boolean;
  /** Empty when the counter read has no rows. That is not `nothing yet`. */
  total: string;
  relative: string;
  hasFacts: boolean;
}

export type DeskWidth = "full" | "narrower" | "phone";
export type DeskRow = "home" | "journal" | "thisPc" | "updates" | "help";

export interface DeskPageVerdict {
  id: string;
  word: string;
  switchOn: boolean | null;
  switchId: string | null;
  switchLabel: string;
}

export interface DeskView {
  width: DeskWidth;
  row: DeskRow;
  shelfOpen: boolean;
  phoneShowsPane: boolean;
  greeting: string;
  pill: string;
  /** Foot of the deck. Unpaired is the placeholder pair. A pair uses its mark. */
  journalName: string;
  pausedLine: string | null;
  badges: readonly string[];
  tiles: readonly DeskTile[];
  rows: readonly DeskStatusRow[];
  paneTitle: string;
  titleGlyph: GlyphName | null;
  paneIsStatus: boolean;
  /** Caught up: a green dot beside the word. No other pill is green. */
  connected: boolean;
  leadHeadline: string;
  leadLines: readonly string[];
  /** Unpaired storage, when a real size is known. Not a journal fact. */
  kept: { label: string; value: string } | null;
  facts: readonly { label: string; value: string }[];
  /** Unpaired status offers the existing connect action. */
  showConnect: boolean;
  /** Status offers one closed row. The strip itself is a deeper page. */
  showTechnical: boolean;
  /** Shelf pages replace the deck. Home keeps the deck beside the pane. */
  shelfPage: boolean;
  pageVerdict: DeskPageVerdict | null;
}

export interface DeskParts {
  notice: HTMLElement | null;
  unread: HTMLElement | null;
  saveRetry: HTMLElement | null;
  paneExtra: HTMLElement | null;
  shelfBody: HTMLElement | null;
}

export interface DeskActions {
  onShelf: (row: DeskRow) => void;
  onShelfToggle: () => void;
  onBackHome: () => void;
  onPill: () => void;
  onResume: () => void;
  onClosePane: () => void;
  onTechnical: () => void;
  onTile: (id: string) => void;
  onSwitch: (id: string, on: boolean) => void;
  onStatus: (id: string) => void;
}

const SHELF: readonly { row: DeskRow; label: string; mark: GlyphName }[] = [
  { row: "home", label: "home", mark: "home" },
  { row: "journal", label: "your journal", mark: "journal" },
  { row: "thisPc", label: "this PC", mark: "device" },
  { row: "updates", label: "updates", mark: "updates" },
  { row: "help", label: "help", mark: "help" },
];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function renderDesk(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const shell = el("div", "desk-shell");
  shell.dataset.width = view.width;
  shell.dataset.row = view.row;
  shell.dataset.shelf = view.shelfOpen ? "open" : "closed";
  shell.dataset.page = view.phoneShowsPane ? "pane" : "deck";

  shell.append(renderShelf(view, actions));

  if (view.shelfPage && parts.shelfBody) {
    const page = el("div", "desk-deck");
    page.setAttribute("data-automation-id", "settings.deck");
    if (view.width === "phone") page.append(backHome(actions));
    page.append(parts.shelfBody);
    shell.append(page);
    return shell;
  }

  shell.append(renderDeck(view, parts, actions));
  shell.append(renderPane(view, parts, actions));
  return shell;
}

function renderShelf(view: DeskView, actions: DeskActions): HTMLElement {
  const rail = el("nav", "settings-rail");
  rail.id = "settings-nav";
  rail.setAttribute("aria-label", "settings");
  for (const item of SHELF) {
    const button = el("button", "settings-nav-item");
    button.type = "button";
    button.append(glyph(item.mark), el("span", "desk-shelf-name", item.label));
    if (item.row === view.row) button.setAttribute("aria-current", "page");
    if (view.badges.includes(item.row)) {
      const dot = el("span", "desk-badge");
      dot.setAttribute("aria-hidden", "true");
      button.append(dot);
    }
    button.onclick = () => actions.onShelf(item.row);
    rail.append(button);
  }
  return rail;
}

function backHome(actions: DeskActions): HTMLButtonElement {
  const back = el("button", "desk-back", "‹ home");
  back.type = "button";
  back.setAttribute("aria-label", "home");
  back.onclick = () => actions.onBackHome();
  return back;
}

function renderDeck(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const deck = el("div", "desk-deck");
  deck.setAttribute("data-automation-id", "settings.deck");
  if (parts.unread) deck.append(parts.unread);

  const head = el("div", "desk-head");
  if (view.width !== "full") {
    const opener = el("button", "desk-shelf-button");
    opener.type = "button";
    opener.setAttribute("aria-label", "home");
    opener.append(glyph("side"));
    opener.onclick = () => actions.onShelfToggle();
    head.append(opener);
  }
  const greeting = el("h1", "desk-greeting", view.greeting);
  greeting.tabIndex = -1;
  head.append(greeting);

  const pill = el("button", "desk-pill", view.pill);
  pill.type = "button";
  if (view.connected) pill.dataset.connected = "true";
  pill.onclick = () => actions.onPill();
  head.append(pill);
  deck.append(head);

  if (view.pausedLine) {
    deck.append(el("p", "desk-paused", view.pausedLine));
    const resume = el("button", "desk-resume", "resume");
    resume.type = "button";
    resume.onclick = () => actions.onResume();
    deck.append(resume);
  }

  if (parts.notice) deck.append(parts.notice);
  if (parts.saveRetry) deck.append(parts.saveRetry);

  const grid = el("div", "desk-grid");
  for (const tile of view.tiles) {
    grid.append(renderTile(tile, actions));
  }
  deck.append(grid, journalFoot(view, actions));
  return deck;
}

function journalFoot(view: DeskView, actions: DeskActions): HTMLButtonElement {
  const foot = el("button", "desk-journal");
  foot.type = "button";
  // Placeholders are the unpaired pill. A pair already has its name.
  if (view.journalName === "your \u00B7 journal") {
    const first = el("span", "desk-mark-slot");
    const second = el("span", "desk-mark-slot");
    first.setAttribute("aria-hidden", "true");
    second.setAttribute("aria-hidden", "true");
    foot.append(first, second);
  }
  foot.append(el("span", "desk-journal-name", view.journalName));
  foot.onclick = () => actions.onShelf("journal");
  return foot;
}

function renderTile(tile: DeskTile, actions: DeskActions): HTMLElement {
  const card = el("div", "desk-tile");
  card.dataset.tile = tile.id;
  if (tile.selected) card.setAttribute("aria-current", "true");
  const open = el("button", "desk-tile-open");
  open.type = "button";
  open.append(glyph(glyphForTile(tile.id)));
  open.append(el("span", "desk-tile-name", tile.name));
  if (tile.word) {
    const word = el("span", "desk-tile-word", tile.word);
    word.dataset.word = tile.word;
    open.append(word);
  }
  if (tile.sub) open.append(el("span", "desk-tile-sub", tile.sub));
  open.onclick = () => actions.onTile(tile.id);
  card.append(open);
  if (tile.switchOn !== null) {
    card.append(switchControl(tile.id, tile.switchOn, tile.switchId, tile.switchLabel, "desk-switch desk-tile-switch", actions));
  }
  return card;
}

function switchControl(
  id: string,
  switchOn: boolean,
  switchId: string | null,
  switchLabel: string,
  className: string,
  actions: DeskActions,
): HTMLLabelElement {
  const label = el("label", className);
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = switchOn;
  if (switchId) input.setAttribute("data-automation-id", switchId);
  input.setAttribute("aria-label", switchLabel);
  input.onchange = () => actions.onSwitch(id, input.checked);
  label.append(input);
  return label;
}

function renderPane(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const pane = el("aside", "desk-pane");
  const head = el("div", "desk-pane-head");
  if (view.width === "phone") head.append(backHome(actions));
  const titleRow = el("div", "desk-pane-title-row");
  if (view.titleGlyph) titleRow.append(glyph(view.titleGlyph));
  const title = el("h2", "desk-pane-title", view.paneTitle);
  title.tabIndex = -1;
  titleRow.append(title);
  if (!view.paneIsStatus && view.width !== "phone") {
    const close = el("button", "desk-close", "×");
    close.type = "button";
    close.setAttribute("aria-label", "status");
    close.onclick = () => actions.onClosePane();
    titleRow.append(close);
  }
  head.append(titleRow);
  pane.append(head);

  if (view.pageVerdict) {
    const verdict = el("div", "desk-verdict");
    const word = el("span", "desk-tile-word", view.pageVerdict.word);
    word.dataset.word = view.pageVerdict.word;
    verdict.append(word);
    if (view.pageVerdict.switchOn !== null) {
      verdict.append(
        switchControl(
          view.pageVerdict.id,
          view.pageVerdict.switchOn,
          view.pageVerdict.switchId,
          view.pageVerdict.switchLabel,
          "desk-switch",
          actions,
        ),
      );
    }
    pane.append(verdict);
  }

  if (view.paneIsStatus) {
    if (view.leadHeadline || view.leadLines.length > 0) {
      const lead = el("div", "desk-lead");
      if (view.leadHeadline) {
        const headline = el("p", "desk-lead-headline", view.leadHeadline);
        if (/^\d+$/.test(view.leadHeadline)) headline.dataset.count = "true";
        lead.append(headline);
      }
      for (const line of view.leadLines) lead.append(el("p", "desk-lead-line", line));
      pane.append(lead);
    }
    if (view.showConnect) {
      const connect = el("button", "desk-connect", "connect a journal");
      connect.type = "button";
      connect.onclick = () => actions.onShelf("journal");
      pane.append(connect);
    }
    const labels = el("div", "desk-status-labels");
    labels.append(
      el("span", "desk-status-glyph"),
      el("span", "", "sources"),
      el("span", "", "so far"),
      el("span", "", "last taken in"),
    );
    pane.append(labels);
    for (const row of view.rows) {
      pane.append(renderStatusRow(row, actions));
    }
    const journal = el("div", "desk-journal-facts");
    journal.append(el("h3", "desk-section", "your journal"));
    if (view.kept) journal.append(factRow(view.kept.label, view.kept.value));
    for (const fact of view.facts) journal.append(factRow(fact.label, fact.value));
    pane.append(journal);
    if (view.showTechnical) {
      const technical = el("button", "desk-technical");
      technical.type = "button";
      technical.append(el("span", "", "technical details"), el("span", "desk-chevron", "\u203A"));
      technical.onclick = () => actions.onTechnical();
      pane.append(technical);
    }
  }
  if (parts.paneExtra) pane.append(parts.paneExtra);
  return pane;
}

function factRow(label: string, value: string): HTMLElement {
  const row = el("div", "desk-fact");
  row.append(el("span", "desk-fact-label", label), el("span", "desk-fact-value", value));
  return row;
}

function renderStatusRow(row: DeskStatusRow, actions: DeskActions): HTMLElement {
  const button = el("button", "desk-status-row");
  button.type = "button";
  if (!row.hasFacts || row.nothingYet) button.classList.add("is-empty");
  button.append(glyph(glyphForTile(row.id)));
  button.append(el("span", "desk-status-name", row.name));
  if (!row.hasFacts) {
    // A missing read is not a fact. The row still opens the page.
  } else if (row.nothingYet) {
    button.append(el("span", "desk-status-empty", "nothing yet"));
  } else {
    button.append(el("span", "desk-status-total", row.total));
    button.append(el("span", "desk-status-relative", row.relative));
  }
  button.onclick = () => actions.onStatus(row.id);
  return button;
}
