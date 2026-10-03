// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { JSDOM } from "jsdom";
import { afterEach, describe, expect, it, vi } from "vitest";

const script = readFileSync(resolve(process.cwd(), "../src-tauri/src/journal-new-window-links.js"), "utf8");
const opened: JSDOM[] = [];
afterEach(() => { for (const dom of opened.splice(0)) dom.window.close(); });

function fixture(html: string, options: { child?: boolean; outside?: boolean } = {}) {
  const dom = new JSDOM(html, { url: "http://journal.test:8080/" });
  opened.push(dom);
  const assign = vi.fn();
  const host = {
    top: undefined as unknown,
    addEventListener: dom.window.addEventListener.bind(dom.window),
  };
  host.top = options.child ? {} : host;
  const location = {
    origin: options.outside ? "http://outside.test" : "http://journal.test:8080",
    href: "http://journal.test:8080/",
    assign,
  };
  new Function("window", "document", "location", "journalOrigin", script)(
    host, dom.window.document, location, "http://journal.test:8080");
  // Suppress jsdom's own navigation after the production listener runs.
  dom.window.addEventListener("click", (event) => event.preventDefault());
  dom.window.addEventListener("auxclick", (event) => event.preventDefault());
  function click(selector = "a", type = "click", button = 0, modifiers: MouseEventInit = {}) {
    const event = new dom.window.MouseEvent(type, { bubbles: true, cancelable: true, button, ...modifiers });
    dom.window.document.querySelector(selector)!.dispatchEvent(event);
  }
  return { dom, assign, click };
}

describe("Journal anchor navigation", () => {
  it.each(["/second", "https://outside.test/sign-in"])("routes a new-window link through main-frame policy: %s", (href) => {
    const f = fixture(`<a target="_blank" href="${href}"><span>open</span></a>`);
    f.click("span");
    expect(f.assign).toHaveBeenCalledExactlyOnceWith(new URL(href, "http://journal.test:8080/").href);
  });

  it("handles dynamically inserted links, named targets and a base target", () => {
    const f = fixture('<base target="details"><div></div>');
    f.dom.window.document.body.insertAdjacentHTML("beforeend", '<a href="/second">open</a>');
    f.click();
    expect(f.assign).toHaveBeenCalledExactlyOnceWith("http://journal.test:8080/second");
  });

  it("lets an explicit empty link target override the base target", () => {
    const f = fixture('<base target="_blank"><a target="" href="/second">open</a>');
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });

  it("resolves a link against the document base, including an SVG anchor", () => {
    const f = fixture('<base href="https://outside.test/auth/"><svg><a target="_blank" href="sign-in"><text>open</text></a></svg>');
    f.click("text");
    expect(f.assign).toHaveBeenCalledExactlyOnceWith("https://outside.test/auth/sign-in");
  });

  it("handles middle-click activation without taking right clicks", () => {
    const f = fixture('<a href="/second">open</a>');
    f.click("a", "auxclick", 2);
    expect(f.assign).not.toHaveBeenCalled();
    f.click("a", "auxclick", 1);
    expect(f.assign).toHaveBeenCalledExactlyOnceWith("http://journal.test:8080/second");
  });

  it.each([{ ctrlKey: true }, { metaKey: true }, { shiftKey: true }])("routes a modified untargeted link: %j", (modifiers) => {
    const f = fixture('<a href="https://outside.test/sign-in">open</a>');
    f.click("a", "click", 0, modifiers);
    expect(f.assign).toHaveBeenCalledExactlyOnceWith("https://outside.test/sign-in");
  });

  it("leaves download shortcuts alone", () => {
    const f = fixture('<a target="_blank" href="/export">save</a>');
    f.click("a", "click", 0, { altKey: true });
    expect(f.assign).not.toHaveBeenCalled();
  });

  it("preserves page cancellation", () => {
    const f = fixture('<a target="_blank" href="/second">open</a>');
    f.dom.window.document.querySelector("a")!.addEventListener("click", (event) => event.preventDefault());
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });

  it.each(["", "_self", "_parent", "_top"])("leaves existing-window links to normal navigation: %s", (target) => {
    const f = fixture(`<a target="${target}" href="/second">open</a>`);
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });

  it.each(["javascript:alert(1)", "mailto:hello@example.test", "https://user@outside.test/", "http://["])("denies an unsupported new-window destination: %s", (href) => {
    const f = fixture(`<a target="_blank" href="${href}">open</a>`);
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });

  it("leaves download links alone", () => {
    const f = fixture('<a target="_blank" href="/export" download>save</a>');
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });

  it.each([{ child: true }, { outside: true }])("does not route a child or outside document: %j", (options) => {
    const f = fixture('<a target="_blank" href="/second">open</a>', options);
    f.click();
    expect(f.assign).not.toHaveBeenCalled();
  });
});
