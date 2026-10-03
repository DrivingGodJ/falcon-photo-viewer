"""Capture Apple's ICNS round trip without compiling or launching Falcon."""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import shutil
import struct
import subprocess
import sys
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('icons', ROOT/'scripts/check-icon-resources.py')
icons = importlib.util.module_from_spec(spec)
spec.loader.exec_module(icons)


def png_chunks(data):
    chunks = []; pos = 8
    while pos < len(data):
        size = struct.unpack_from('>I', data, pos)[0]
        kind = data[pos+4:pos+8].decode('ascii')
        body = data[pos+8:pos+8+size]
        item = {'kind': kind, 'bytes': size}
        if kind in ('iCCP', 'gAMA', 'sRGB', 'cHRM', 'CgBI'):
            item['sha256'] = hashlib.sha256(body).hexdigest()
            if kind == 'iCCP': item['profile_name'] = body.split(b'\0')[0].decode('latin1')
            if kind in ('gAMA', 'sRGB'): item['value_hex'] = body.hex()
        chunks.append(item); pos += size + 12
    return {'bit_depth': data[24], 'colour_type': data[25], 'interlace': data[28], 'chunks': chunks}


def compare_pixels(expected, actual):
    w, h, rows = expected
    aw, ah, other = actual
    result = {'dimensions': [w, h], 'actual_dimensions': [aw, ah],
              'alpha_changes': 0, 'colour_failures': 0, 'max_coverage_delta': 0,
              'max_opaque_rgb_delta': 0, 'examples': []}
    if (w, h) != (aw, ah): return result
    for y, (a, b) in enumerate(zip(rows, other)):
        for pos in range(0, len(a), 4):
            delta = max(abs(a[pos+i]*a[pos+3] - b[pos+i]*b[pos+3]) for i in range(3))
            alpha_changed = a[pos+3] != b[pos+3]
            result['alpha_changes'] += alpha_changed
            result['colour_failures'] += delta > 255
            result['max_coverage_delta'] = max(result['max_coverage_delta'], delta)
            if a[pos+3] == b[pos+3] == 255:
                result['max_opaque_rgb_delta'] = max(result['max_opaque_rgb_delta'],
                    max(abs(a[pos+i]-b[pos+i]) for i in range(3)))
            if (alpha_changed or delta > 255) and len(result['examples']) < 8:
                result['examples'].append({'xy': [pos//4, y], 'expected': list(a[pos:pos+4]),
                                           'actual': list(b[pos:pos+4]), 'coverage_delta': delta})
    return result


def tag_srgb(data):
    """Annotate the untagged approved pixels; never resample or recompress them."""
    if any(c['kind'] in ('iCCP', 'sRGB', 'gAMA', 'cHRM') for c in png_chunks(data)['chunks']):
        raise ValueError('Profile-tagging probe requires an untagged source')
    assert data[12:16] == b'IHDR' and struct.unpack_from('>I', data, 8)[0] == 13
    chunk = b'sRGB\0'
    return data[:33] + struct.pack('>I', 1) + chunk + struct.pack('>I', zlib.crc32(chunk)) + data[33:]


def probe(output):
    if sys.platform != 'darwin': raise RuntimeError('Apple iconutil must run on macOS')
    output = output.resolve(); output.mkdir(parents=True, exist_ok=False)
    manifest = icons.verify_assets()
    report = {'system': subprocess.check_output(['sw_vers'], text=True).strip(), 'icons': {}}
    failed = False
    for name, variant in ((name, variant) for name in ('app', 'document')
                          for variant in ('original', 'srgb-tagged')):
        label = name+'-'+variant
        item = {'frames': {}}; report['icons'][label] = item
        original = icons.ASSETS/(name+'.iconset')
        input_set = output/(label+'-source.iconset')
        encoded = output/(label+'.icns'); decoded = output/(label+'-decoded.iconset')
        shutil.copytree(original, input_set)
        try:
            if variant == 'srgb-tagged':
                for frame in input_set.glob('*.png'):
                    data = frame.read_bytes(); tagged = tag_srgb(data)
                    assert icons.png_pixels(data) == icons.png_pixels(tagged)
                    frame.write_bytes(tagged)
            subprocess.run(['iconutil', '-c', 'icns', str(input_set), '-o', str(encoded)], check=True)
            subprocess.run(['iconutil', '-c', 'iconset', str(encoded), '-o', str(decoded)], check=True)
            try:
                entries = icons.icns_entries(encoded.read_bytes())
                item['representations'] = {kind.decode(): {'bytes': len(data), 'prefix_hex': data[:16].hex()}
                                           for kind, data in entries.items()}
            except Exception as error:
                item['container_verification_error'] = str(error)
            for frame in manifest['mac_iconsets'][name]:
                source_data = (original/frame).read_bytes(); native_data = (decoded/frame).read_bytes()
                metrics = {'source_png': png_chunks(source_data), 'native_png': png_chunks(native_data)}
                if metrics['source_png']['interlace'] or metrics['native_png']['interlace']:
                    metrics['error'] = 'Interlaced PNG needs native decoding; pixel comparison skipped'
                else:
                    try:
                        metrics.update(compare_pixels(icons.png_pixels(source_data),
                                                       icons.png_pixels(native_data, reject_metadata=False)))
                    except Exception as error:
                        metrics['error'] = repr(error)
                item['frames'][frame] = metrics
            try:
                icons.verify_iconset(decoded, name)
                item['verification'] = 'passed'
            except Exception as error:
                item['verification'] = str(error); failed = True
        except Exception as error:
            item['error'] = str(error); failed = True
        (output/'report.json').write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(report, indent=2))
    return 1 if failed else 0


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=pathlib.Path)
    sys.exit(probe(parser.parse_args().output))
