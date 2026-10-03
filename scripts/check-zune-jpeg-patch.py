"""Check the complete source boundary of Falcon's independently replaced JPEG transpose."""

import hashlib
import json
import tomllib
from pathlib import Path

CHANGED = {'Cargo.toml', 'src/unsafe_utils_avx2.rs', 'src/idct.rs', 'src/bitstream.rs'}
ADDED = {'src/falcon_transpose.rs', 'FALCON-CHANGES.md'}


def validate(original, current):
    if current.keys() != original.keys() | ADDED:
        raise ValueError('Missing or unexpected zune-jpeg source files')
    changed = {
        name for name in original if hashlib.sha256(current[name]).hexdigest() != original[name]
    }
    if changed != CHANGED:
        raise ValueError('Undeclared zune-jpeg source changes')
    package = tomllib.loads(current['Cargo.toml'].decode('utf-8'))['package']
    if package['license'] != 'Apache-2.0' or package['version'] != '0.5.15':
        raise ValueError('The reviewed zune-jpeg version/licence selection changed')
    if b'include!("falcon_transpose.rs")' not in current['src/unsafe_utils_avx2.rs']:
        raise ValueError('The replacement transpose is not connected')


def check(root):
    root = Path(root)
    original = json.loads(
        (root / 'scripts/zune-jpeg-upstream-sha256.json').read_text(encoding='utf-8')
    )
    base = root / 'falcon/vendor/zune-jpeg'
    current = {
        p.relative_to(base).as_posix(): p.read_bytes() for p in base.rglob('*') if p.is_file()
    }
    validate(original, current)
    return original, current


if __name__ == '__main__':
    check(Path(__file__).resolve().parents[1])
    print('zune-jpeg source differs only at the declared Falcon changes')
