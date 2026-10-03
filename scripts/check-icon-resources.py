"""Verify the checked-in icons and the actual Mac bundle; Python stdlib only."""
import hashlib
import json
import pathlib
import plistlib
import struct
import subprocess
import sys
import tempfile
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'falcon/native/assets/icons'

# Apple's ic04/ic05 are channel-compressed ARGB, not PNG. The remaining
# standard representations contain PNG data on the supported macOS versions.
ICNS_FRAMES = {
    b'ic04': 'icon_16x16.png', b'ic11': 'icon_16x16@2x.png',
    b'ic05': 'icon_32x32.png', b'ic12': 'icon_32x32@2x.png',
    b'ic07': 'icon_128x128.png', b'ic13': 'icon_128x128@2x.png',
    b'ic08': 'icon_256x256.png', b'ic14': 'icon_256x256@2x.png',
    b'ic09': 'icon_512x512.png', b'ic10': 'icon_512x512@2x.png',
}


def icns_entries(data):
    if data[:4] != b'icns' or struct.unpack('>I', data[4:8])[0] != len(data):
        raise ValueError('Invalid ICNS container')
    entries = {}
    pos = 8
    while pos < len(data):
        kind, length = struct.unpack('>4sI', data[pos:pos + 8])
        if length < 8 or pos + length > len(data): raise ValueError('Invalid ICNS entry')
        if kind in entries: raise ValueError('Duplicate ICNS entry')
        entries[kind] = data[pos + 8:pos + length]; pos += length
    if not {b'ic07', b'ic08', b'ic09', b'ic10'}.issubset(entries):
        raise ValueError('ICNS must contain 128 through 1024 pixel representations')
    for kind in (b'icp4', b'icp5', b'icp6'):
        if entries.get(kind, b'').startswith(b'\x89PNG'):
            raise ValueError('Do not put PNG data in legacy small-icon slots')
    return entries


def verify_assets():
    manifest = json.loads((ASSETS / 'manifest.json').read_text(encoding='utf-8'))
    for entry in manifest['resources'] + manifest['mac_resources']:
        data = (ASSETS / entry['file']).read_bytes()
        if hashlib.sha256(data).hexdigest() != entry['sha256']:
            raise ValueError('Icon differs from manifest: ' + entry['file'])
        if entry['file'].endswith('.icns'): icns_entries(data)
    if (ROOT / 'falcon/native/icon.ico').read_bytes() != (ASSETS / 'app.ico').read_bytes():
        raise ValueError('Windows resource 1 does not match the approved app icon')
    for name, entries in manifest['mac_iconsets'].items():
        for file, expected in entries.items():
            if hashlib.sha256((ASSETS / (name + '.iconset') / file).read_bytes()).hexdigest() != expected:
                raise ValueError('Iconset frame differs: ' + file)
    return manifest


def verify_bundle(app):
    manifest = verify_assets()
    info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
    resources = app / 'Contents/Resources'
    for stem, source in [('falcon', 'app'), ('falcon-document', 'document')]:
        data = (resources / (stem + '.icns')).read_bytes()
        icns_entries(data)
    if (resources / 'falcon.icns').read_bytes() == (resources / 'falcon-document.icns').read_bytes():
        raise ValueError('App and generic document icons must differ')
    if info.get('CFBundleIconFile') != 'falcon': raise ValueError('Wrong app icon reference')
    if 'FalconExperimentMode' not in info:
        docs = info.get('CFBundleDocumentTypes', [])
        if len(docs) != 2: raise ValueError('Expected existing image and RAW document groups')
        for doc in docs:
            if (doc.get('CFBundleTypeIconFile'), doc.get('CFBundleTypeRole'), doc.get('LSHandlerRank')) != ('falcon-document', 'Viewer', 'Alternate'):
                raise ValueError('Wrong document icon/handler declaration')
    return manifest


def png_pixels(data, reject_metadata=True):
    """Decode RGBA icon PNGs independently, including PNG's five row filters."""
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    w, h, depth, colour = struct.unpack('>IIBB', data[16:26])
    assert (depth, colour) == (8, 6), (depth, colour)
    compressed = bytearray(); pos = 8
    while pos < len(data):
        n = struct.unpack('>I', data[pos:pos+4])[0]; kind = data[pos+4:pos+8]
        if kind == b'IDAT': compressed.extend(data[pos+8:pos+8+n])
        if reject_metadata: assert kind not in (b'eXIf', b'tEXt', b'iTXt', b'caBX'), kind
        pos += n + 12
    raw = zlib.decompress(compressed); rows = []; previous = bytes(w*4)
    for y in range(h):
        filt = raw[y*(w*4+1)]; row = bytearray(raw[y*(w*4+1)+1:(y+1)*(w*4+1)])
        for x in range(w*4):
            a = row[x-4] if x >= 4 else 0; b = previous[x]; c = previous[x-4] if x >= 4 else 0
            if filt == 0: p = 0
            elif filt == 1: p = a
            elif filt == 2: p = b
            elif filt == 3: p = (a+b)//2
            elif filt == 4:
                p = a+b-c; pa,pb,pc = abs(p-a),abs(p-b),abs(p-c)
                p = a if pa <= pb and pa <= pc else b if pb <= pc else c
            else: raise ValueError('Bad PNG filter')
            row[x] = (row[x]+p) & 255
        rows.append(row); previous = row
    return w, h, rows



def verify_pixels(expected, actual, label):
    if expected[:2] != actual[:2]: raise ValueError('Native icon dimensions changed: ' + label)
    for a, b in zip(expected[2], actual[2]):
        for x in range(0, len(a), 4):
            if a[x+3] != b[x+3]: raise ValueError('Native icon alpha changed: ' + label)
            # Unpremultiplication of small alpha can round RGB; compare displayed coverage.
            if any(abs(a[x+i]*a[x+3] - b[x+i]*b[x+3]) > 255 for i in range(3)):
                raise ValueError('Native icon colours changed: ' + label)


def argb_pixels(data, size):
    """Decode ic04/ic05's four planes without iconutil's extra unpremultiplication."""
    if not data.startswith(b'ARGB'): raise ValueError('Expected native ARGB icon')
    pos = 4; planes = []; count = size * size
    for _ in range(4):
        plane = bytearray()
        while len(plane) < count:
            if pos >= len(data): raise ValueError('Truncated ARGB plane')
            control = data[pos]; pos += 1
            length = control + 1 if control < 128 else control - 125
            consumed = length if control < 128 else 1
            if len(plane) + length > count or pos + consumed > len(data):
                raise ValueError('Invalid ARGB run')
            if control < 128: plane.extend(data[pos:pos+length])
            else: plane.extend(data[pos:pos+1] * length)
            pos += consumed
        planes.append(plane)
    if pos != len(data): raise ValueError('Unexpected trailing ARGB data')
    rgba = bytearray(count * 4)
    for channel, plane in enumerate((planes[1], planes[2], planes[3], planes[0])):
        rgba[channel::4] = plane
    return size, size, [rgba[y*size*4:(y+1)*size*4] for y in range(size)]


def verify_icns(path, source):
    """Compare the stored pixels, not iconutil's lossy small-icon PNG export.

    On macOS 15, iconutil exports ic04/ic05 with another unpremultiplication,
    brightening partially transparent pixels. The stored ARGB has the correct
    straight colours within the existing coverage tolerance. All ten frames,
    including alpha and opaque RGB, still go through the same strict check.
    """
    if source not in ('app', 'document'): raise ValueError('Unknown iconset')
    entries = icns_entries(path.read_bytes())
    for kind, name in ICNS_FRAMES.items():
        if kind not in entries: raise ValueError('Missing native icon frame: ' + name)
        expected = png_pixels((ASSETS / (source + '.iconset') / name).read_bytes())
        actual = (argb_pixels(entries[kind], 16 if kind == b'ic04' else 32)
                  if kind in (b'ic04', b'ic05') else png_pixels(entries[kind], reject_metadata=False))
        verify_pixels(expected, actual, source + '/' + name)


def verify_native_icons():
    """Run the packaging check before the expensive app compilation on Mac."""
    verify_assets()
    with tempfile.TemporaryDirectory(prefix='falcon-icon-check-') as tmp:
        for source in ('app', 'document'):
            path = pathlib.Path(tmp) / (source + '.icns')
            subprocess.run(['iconutil', '-c', 'icns', str(ASSETS / (source + '.iconset')),
                            '-o', str(path)], check=True)
            verify_icns(path, source)


def verify_iconset(directory, source):
    if source not in ('app', 'document'): raise ValueError('Unknown iconset')
    frames = (ASSETS / (source + '.iconset'))
    manifest = json.loads((ASSETS / 'manifest.json').read_text(encoding='utf-8'))
    for name in manifest['mac_iconsets'][source]:
        original = frames / name
        expected = png_pixels(original.read_bytes())
        actual = png_pixels((directory / original.name).read_bytes(), reject_metadata=False)
        verify_pixels(expected, actual, source + '/' + name)


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--iconset': verify_iconset(pathlib.Path(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) > 1 and sys.argv[1] == '--icns': verify_icns(pathlib.Path(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) > 1 and sys.argv[1] == '--native': verify_native_icons()
    elif len(sys.argv) > 1: verify_bundle(pathlib.Path(sys.argv[1]))
    else: verify_assets()
    print('Icon resources verified')
