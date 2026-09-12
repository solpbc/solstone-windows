#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
"""Generate lock-bound Rust dependency notices for the shipped Windows app.

Population: non-dev (normal + build) dependency closure of solstone-windows-app
for x86_64-pc-windows-msvc from locked Cargo metadata. Workspace crates are
AGPL-3.0-only and are not reproduced here.

Crate-shipped LICENSE/COPYING/NOTICE files are preferred. packaging/rust-notices/overrides
covers crates that publish no license file. Regeneration is offline after `cargo fetch
--locked --target x86_64-pc-windows-msvc`.

This script does not determine the license of vendored prebuilt binaries
(.lib, .a, .dll, .so, .dylib, .o, .obj, or weight files). It fails if the
shipped closure gains an unlisted prebuilt; packaging/rust-notices/prebuilt-artifacts.json
is detection only.
"""
from __future__ import annotations

import datetime
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
ROOT_PACKAGE = "solstone-windows-app"
NOTICES_PATH = ROOT / "RUST_DEPENDENCY_NOTICES.txt"
INDEX_PATH = ROOT / "packaging/rust-notices/index.json"
PREBUILT_PATH = ROOT / "packaging/rust-notices/prebuilt-artifacts.json"
OVERRIDES = ROOT / "packaging/rust-notices/overrides"
PREBUILT_SUFFIXES = {
    ".lib",
    ".a",
    ".dll",
    ".so",
    ".dylib",
    ".o",
    ".obj",
    ".onnx",
    ".pt",
    ".pth",
    ".safetensors",
    ".gguf",
    ".npz",
    ".tflite",
    ".weights",
}
SKIP_PREBUILT_PREFIXES = ("tests/", "benches/", "examples/")
LICENSE_NAME = re.compile(
    r"^(licen[cs]e|copying|copyright|notice)([._-].*)?$",
    re.I,
)
HEADER = (
    "Rust dependency notices\n"
    "\n"
    "this file reproduces the license texts of the crates statically linked into\n"
    "the solstone app for windows. solstone's own crates are agpl-3.0-only; see LICENSE.\n"
    "Microsoft runtime and WebView2 loader terms are in THIRD_PARTY_NOTICES.md.\n"
    "\n"
)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def cargo_metadata() -> dict:
    proc = subprocess.run(
        [
            "cargo",
            "metadata",
            "--manifest-path",
            str(ROOT / "Cargo.toml"),
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            TARGET,
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(proc.stdout)


def closure(meta: dict, root_name: str) -> set[str]:
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [pid for pid, p in packages.items() if p["name"] == root_name]
    if len(roots) != 1:
        raise SystemExit(f"expected one {root_name} package, got {roots}")
    pending = list(roots)
    seen: set[str] = set()
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        node = nodes.get(pid)
        if node is None:
            continue
        for dep in node.get("deps", []):
            kinds = dep.get("dep_kinds") or []
            if any(k.get("kind") != "dev" for k in kinds):
                pending.append(dep["pkg"])
    return seen


def crate_license_files(pkg: dict) -> list[tuple[str, bytes]]:
    crate_root = Path(pkg["manifest_path"]).parent
    found: list[tuple[str, bytes]] = []
    if not crate_root.is_dir():
        return found
    for path in crate_root.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(crate_root).as_posix()
        if any(part.startswith(".") for part in Path(rel).parts):
            continue
        parent = Path(rel).parts[0] if Path(rel).parts else ""
        if LICENSE_NAME.match(path.name) or parent.upper() in {"LICENSE", "LICENSES", "LICENCE"}:
            found.append((rel, path.read_bytes()))
    found.sort(key=lambda item: item[0])
    return found


def crate_prebuilt_members(pkg: dict) -> list[str]:
    crate_root = Path(pkg["manifest_path"]).parent
    found: list[str] = []
    if not crate_root.is_dir():
        return found
    for path in crate_root.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(crate_root).as_posix()
        if any(part.startswith(".") for part in Path(rel).parts):
            continue
        if rel.startswith(SKIP_PREBUILT_PREFIXES) or "/tests/" in rel:
            continue
        if path.suffix.lower() in PREBUILT_SUFFIXES:
            found.append(rel)
    found.sort()
    return found


def load_prebuilt_allowlist() -> dict[tuple[str, str], set[str]]:
    allowed = json.loads(PREBUILT_PATH.read_text())
    if allowed.get("schema") != "solstone.windows-app-prebuilt-artifacts.v1":
        raise SystemExit("prebuilt-artifacts.json has an unexpected schema")
    mapping: dict[tuple[str, str], set[str]] = {}
    for row in allowed["packages"]:
        mapping[(row["name"], row["version"])] = {
            suffix.lower() for suffix in row["suffixes"]
        }
    return mapping


def override_files(name: str, version: str) -> list[tuple[str, bytes]]:
    directory = OVERRIDES / f"{name}-{version}"
    if not directory.is_dir():
        return []
    found: list[tuple[str, bytes]] = []
    for path in sorted(directory.rglob("*")):
        if path.is_file():
            found.append((path.relative_to(directory).as_posix(), path.read_bytes()))
    return found


def main() -> int:
    meta = cargo_metadata()
    seen = closure(meta, ROOT_PACKAGE)
    packages = {p["id"]: p for p in meta["packages"]}
    lock = (ROOT / "Cargo.lock").read_bytes()
    lock_hash = sha256(lock)
    texts: dict[str, bytes] = {}
    rows = []
    missing = []
    workspace = []
    detected_prebuilts: dict[tuple[str, str], set[str]] = {}
    for pid in sorted(seen):
        pkg = packages[pid]
        if pkg.get("source") is None:
            workspace.append(pkg["name"])
            continue
        prebuilt = crate_prebuilt_members(pkg)
        if prebuilt:
            detected_prebuilts[(pkg["name"], pkg["version"])] = {
                Path(member).suffix.lower() for member in prebuilt
            }
        files = crate_license_files(pkg)
        source_kind = "crate-archive"
        if not files:
            files = override_files(pkg["name"], pkg["version"])
            source_kind = "committed-override"
        refs = []
        for member, data in files:
            digest = sha256(data)
            texts[digest] = data
            refs.append(
                {
                    "bytes": len(data),
                    "sha256": digest,
                    "member": member,
                    "source_kind": source_kind,
                }
            )
        identity = f"{pkg['name']}@{pkg['version']} ({pkg['source']})"
        row = {
            "identity": identity,
            "name": pkg["name"],
            "version": pkg["version"],
            "source": pkg["source"],
            "license_expression": pkg.get("license") or "",
            "notice_references": refs,
        }
        rows.append(row)
        if not refs:
            missing.append(identity)
    if missing:
        sys.stderr.write("missing licence bodies:\n")
        for identity in missing:
            sys.stderr.write(f"  {identity}\n")
        return 1
    allowed_prebuilts = load_prebuilt_allowlist()
    unexpected = sorted(set(detected_prebuilts) - set(allowed_prebuilts))
    stale = sorted(set(allowed_prebuilts) - set(detected_prebuilts))
    suffix_mismatch = []
    for key in sorted(set(detected_prebuilts) & set(allowed_prebuilts)):
        if detected_prebuilts[key] != allowed_prebuilts[key]:
            suffix_mismatch.append(
                f"{key[0]}@{key[1]} detected={sorted(detected_prebuilts[key])} "
                f"allowlist={sorted(allowed_prebuilts[key])}"
            )
    if unexpected or stale or suffix_mismatch:
        sys.stderr.write(
            "prebuilt artifacts changed; this generator cannot determine their license.\n"
            "update packaging/rust-notices/prebuilt-artifacts.json after a legal read "
            "of each artifact's own primary license text.\n"
        )
        for name, version in unexpected:
            sys.stderr.write(
                f"  unexpected {name}@{version} suffixes={sorted(detected_prebuilts[(name, version)])}\n"
            )
        for name, version in stale:
            sys.stderr.write(f"  stale allowlist row {name}@{version}\n")
        for line in suffix_mismatch:
            sys.stderr.write(f"  {line}\n")
        return 1
    notice = bytearray(HEADER.encode())
    text_records = []
    for digest, data in sorted(texts.items()):
        notice.extend(f"SHA-256: {digest}\n\n".encode())
        start = len(notice)
        notice.extend(data)
        if not data.endswith(b"\n"):
            notice.extend(b"\n")
        notice.extend(b"\n")
        text_records.append(
            {
                "sha256": digest,
                "bytes": len(data),
                "byte_start_inclusive": start,
                "byte_end_exclusive": start + len(data),
            }
        )
    if len(notice) >= 16 * 1024 * 1024:
        raise SystemExit("notices file exceeds 16 MiB bound")
    index = {
        "schema": "solstone.windows-app-rust-notices.v1",
        "cargo_lock_sha256": lock_hash,
        "notices_sha256": sha256(bytes(notice)),
        "population": {
            "root_package": ROOT_PACKAGE,
            "filter_platform": TARGET,
            "source": "non-dev dependency closure; conservative inclusion, not exact PE link graph",
            "query": (
                "cargo metadata --locked --offline --format-version 1 "
                f"--filter-platform {TARGET}; follow non-dev edges from {ROOT_PACKAGE}"
            ),
            "query_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "workspace_package_count": len(workspace),
            "external_package_count": len(rows),
            "notice_text_count": len(text_records),
        },
        "packages": rows,
        "notice_texts": text_records,
    }
    encoded = (json.dumps(index, indent=2, sort_keys=True) + "\n").encode()
    NOTICES_PATH.write_bytes(notice)
    INDEX_PATH.parent.mkdir(parents=True, exist_ok=True)
    INDEX_PATH.write_bytes(encoded)
    print(
        json.dumps(
            {
                "notices": str(NOTICES_PATH.relative_to(ROOT)),
                "index": str(INDEX_PATH.relative_to(ROOT)),
                "cargo_lock_sha256": lock_hash,
                "notices_sha256": index["notices_sha256"],
                "notices_bytes": len(notice),
                "external_package_count": len(rows),
                "notice_text_count": len(text_records),
                "override_packages": sum(
                    1
                    for row in rows
                    if any(ref["source_kind"] == "committed-override" for ref in row["notice_references"])
                ),
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
