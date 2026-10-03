// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

if (window.top === window) {
  const routeLink = (event) => {
    if (location.origin !== journalOrigin || event.defaultPrevented || event.altKey) return;
    if (event.type === "click" ? event.button !== 0 : event.button !== 1) return;
    const anchor = event.composedPath()
      .map((node) => node.closest?.("a[href]"))
      .find(Boolean);
    if (!anchor || anchor.hasAttribute("download")) return;
    const target = (anchor.getAttribute("target") ??
      document.querySelector("base[target]")?.getAttribute("target") ?? "").toLowerCase();
    const newContext = event.type === "auxclick" || event.ctrlKey || event.metaKey || event.shiftKey ||
      (target && !["_self", "_parent", "_top"].includes(target));
    if (!newContext) return;
    event.preventDefault();
    let destination;
    try {
      destination = new URL(anchor.getAttribute("href"), document.baseURI);
    } catch {
      return;
    }
    if (!["http:", "https:"].includes(destination.protocol) ||
        destination.username || destination.password) return;
    location.assign(destination.href);
  };
  window.addEventListener("click", routeLink);
  window.addEventListener("auxclick", routeLink);
}
