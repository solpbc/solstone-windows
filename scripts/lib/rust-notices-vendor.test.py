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


if __name__ == "__main__":
    unittest.main()
