"""Keep the hosted-view patch confined to opt-in Mac code, with auditable upstream bytes."""
import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "falcon/vendor/winit"
ALLOWED = {"Cargo.toml", "src/platform/macos.rs", "src/platform_impl/macos/view.rs",
           "src/platform_impl/macos/window_delegate.rs"}


class VendorTest(unittest.TestCase):
    def test_only_declared_mac_patch_files_differ_from_upstream(self):
        original = json.loads((ROOT / "scripts/winit-upstream-sha256.json").read_text())
        current = {p.relative_to(VENDOR).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                   for p in VENDOR.rglob("*") if p.is_file()}
        self.assertEqual(current.keys(), original.keys(), "unexpected added/missing vendor source")
        changed = {name for name in original if original[name] != current[name]}
        self.assertEqual(changed, ALLOWED)
        # This covers Windows/shared implementation, version, license and build script.
        self.assertGreater(len(original) - len(changed), 200)


if __name__ == "__main__":
    unittest.main()
