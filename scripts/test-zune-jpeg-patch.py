"""Keep the new JPEG transpose isolated and the remaining dependency source unchanged."""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    'zune_patch', ROOT / 'scripts/check-zune-jpeg-patch.py'
)
patch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patch)


class ZunePatch(unittest.TestCase):
    def test_complete_production_tree_and_connection(self):
        original, current = patch.check(ROOT)
        self.assertEqual(len(original), 39)
        self.assertEqual(len(current), 41)

    def test_missing_or_undeclared_source_is_refused(self):
        # Falsifiers: ignore missing/additional files or permit unrelated decoder changes.
        original, actual = patch.check(ROOT)
        for case in ('missing-original', 'missing-new', 'extra', 'changed'):
            current = actual.copy()
            if case == 'missing-original':
                current.pop('src/decoder.rs')
            elif case == 'missing-new':
                current.pop('src/falcon_transpose.rs')
            elif case == 'extra':
                current['extra.rs'] = b'not reviewed'
            else:
                current['src/decoder.rs'] += b'\nchanged'
            with self.subTest(case=case), self.assertRaises(ValueError):
                patch.validate(original, current)

    def test_version_licence_and_include_are_checked(self):
        # Falsifier: check only which paths changed, not the actual version and call connection.
        original, actual = patch.check(ROOT)
        for name, old, new in [
            ('Cargo.toml', b'license = "Apache-2.0"', b'license = "MIT"'),
            ('Cargo.toml', b'version = "0.5.15"', b'version = "0.5.14"'),
            (
                'src/unsafe_utils_avx2.rs',
                b'include!("falcon_transpose.rs")',
                b'include!("other.rs")',
            ),
        ]:
            current = actual.copy()
            current[name] = current[name].replace(old, new)
            with self.subTest(field=old), self.assertRaises(ValueError):
                patch.validate(original, current)


if __name__ == '__main__':
    unittest.main()
