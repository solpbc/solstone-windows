# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
import sys
from pathlib import Path
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "rust_notices", Path(__file__).resolve().parents[1] / "generate-rust-notices.py"
)
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class VendoredNotices(unittest.TestCase):
    def test_local_mit_dependency_keeps_its_license_in_the_shipped_notices(self):
        self.assert_vendor_license_kept("MIT")

    def test_a_local_agpl_declaration_does_not_make_a_vendor_first_party(self):
        self.assert_vendor_license_kept("AGPL-3.0-only")

    def assert_vendor_license_kept(self, license_expression):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            vendor = root / "vendor" / "velopack"
            vendor.mkdir(parents=True)
            license_body = b"MIT license\nCopyright upstream\n"
            (vendor / "LICENSE").write_bytes(license_body)
            (root / "Cargo.lock").write_text("test locked graph\n")
            prebuilt = root / "prebuilt.json"
            prebuilt.write_text(json.dumps({
                "schema": "solstone.windows-app-prebuilt-artifacts.v1",
                "packages": [],
            }))
            metadata = {
                "packages": [
                    {"id": "app", "name": "solstone-windows-app", "source": None,
                     "license": "AGPL-3.0-only"},
                    {"id": "vendor", "name": "velopack", "version": "1.2.0",
                     "source": None, "license": license_expression,
                     "manifest_path": str(vendor / "Cargo.toml")},
                ],
                "workspace_members": ["app"],
                "resolve": {"nodes": [
                    {"id": "app", "deps": [{"pkg": "vendor", "dep_kinds": [{"kind": None}]}]},
                    {"id": "vendor", "deps": []},
                ]},
            }
            output = root / "notices.txt"
            index = root / "index.json"
            with patch.multiple(notices, ROOT=root, NOTICES_PATH=output,
                                INDEX_PATH=index, PREBUILT_PATH=prebuilt), \
                    patch.object(notices, "cargo_metadata", return_value=metadata), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(notices.main(), 0)
            self.assertIn(license_body, output.read_bytes())
            packages = json.loads(index.read_text())["packages"]
            self.assertEqual([p["name"] for p in packages], ["velopack"])
            self.assertEqual(packages[0]["source"], "vendored:vendor/velopack")
            self.assertEqual(packages[0]["notice_references"][0]["source_kind"], "vendored-source")


class LicenseElectionsAndMplPointer(unittest.TestCase):
    def generate(self, crates):
        """Run the generator over crates.io crates given as (name, version, license)."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.lock").write_text("test locked graph\n")
            prebuilt = root / "prebuilt.json"
            prebuilt.write_text(json.dumps({
                "schema": "solstone.windows-app-prebuilt-artifacts.v1",
                "packages": [],
            }))
            packages = [{"id": "app", "name": "solstone-windows-app", "source": None,
                         "license": "AGPL-3.0-only"}]
            deps = []
            for name, version, license_expression in crates:
                crate = root / "registry" / f"{name}-{version}"
                crate.mkdir(parents=True)
                (crate / "LICENSE").write_bytes(f"{license_expression} text\n".encode())
                packages.append({"id": name, "name": name, "version": version,
                                 "source": notices.CRATES_IO_SOURCE,
                                 "license": license_expression,
                                 "manifest_path": str(crate / "Cargo.toml")})
                deps.append({"pkg": name, "dep_kinds": [{"kind": None}]})
            metadata = {
                "packages": packages,
                "workspace_members": ["app"],
                "resolve": {"nodes": [{"id": "app", "deps": deps}]},
            }
            output = root / "notices.txt"
            index = root / "index.json"
            with patch.multiple(notices, ROOT=root, NOTICES_PATH=output,
                                INDEX_PATH=index, PREBUILT_PATH=prebuilt), \
                    patch.object(notices, "cargo_metadata", return_value=metadata), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(notices.main(), 0)
            rows = {p["name"]: p for p in json.loads(index.read_text())["packages"]}
            return output.read_text(), rows

    def test_index_records_the_permissive_option_elected(self):
        _, rows = self.generate([
            ("self_cell", "1.3.0", "Apache-2.0 OR GPL-2.0-only"),
            ("legacy", "1.0.0", "MIT/Apache-2.0"),
            ("compound", "1.0.0", "(GPL-3.0-only OR Apache-2.0) AND Unicode-3.0"),
            ("plain", "1.0.0", "MIT"),
            ("copyleft-only", "1.0.0", "LGPL-2.1-only OR GPL-2.0-only"),
        ])
        self.assertEqual(rows["self_cell"]["license_election"], "Apache-2.0")
        self.assertEqual(rows["legacy"]["license_election"], "MIT")
        self.assertEqual(rows["compound"]["license_election"], "Apache-2.0 AND Unicode-3.0")
        self.assertIsNone(rows["plain"]["license_election"])
        self.assertIsNone(rows["copyleft-only"]["license_election"])

    def test_notices_name_each_mpl_crate_with_its_source_location(self):
        text, _ = self.generate([
            ("selectors", "0.36.1", "MPL-2.0"),
            ("cssparser", "0.36.0", "MPL-2.0"),
            ("either-way", "1.0.0", "MPL-2.0 OR MIT"),
        ])
        self.assertIn("MPL-2.0 source code", text)
        self.assertIn("- cssparser 0.36.0: https://crates.io/crates/cssparser/0.36.0\n"
                      "- selectors 0.36.1: https://crates.io/crates/selectors/0.36.1\n", text)
        self.assertNotIn("either-way", text)
        self.assertIn(notices.SOURCE_URL, text)

    def test_notices_without_mpl_crates_carry_no_pointer(self):
        text, _ = self.generate([("plain", "1.0.0", "MIT")])
        self.assertNotIn("MPL-2.0 source code", text)


if __name__ == "__main__":
    unittest.main()
