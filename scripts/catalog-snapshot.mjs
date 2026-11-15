// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const ROOTS = ["i18n/compiler.json", "i18n/catalog", "i18n/generated", "i18n/arguments.ts", "crates/observer-model/i18n"];
const SKIP = new Set(["i18n/generated/snapshot.json"]);

export function catalogFiles(root) {
  const found = [];
  function walk(rel) {
    const abs = path.join(root, rel);
    if (fs.statSync(abs).isDirectory()) {
      for (const name of fs.readdirSync(abs).sort((a, b) => a.localeCompare(b, "en"))) {
        walk(`${rel}/${name}`);
      }
      return;
    }
    if (!SKIP.has(rel)) found.push(rel);
  }
  for (const rel of ROOTS) walk(rel);
  found.sort((a, b) => a.localeCompare(b, "en"));
  return found;
}

export function catalogHashes(root) {
  const hashes = {};
  for (const rel of catalogFiles(root)) {
    hashes[rel] = crypto.createHash("sha256").update(fs.readFileSync(path.join(root, rel))).digest("hex");
  }
  return hashes;
}

export function writeSnapshot(root) {
  const body = `${JSON.stringify(catalogHashes(root), null, 2)}\n`;
  fs.writeFileSync(path.join(root, "i18n/generated/snapshot.json"), body);
}
