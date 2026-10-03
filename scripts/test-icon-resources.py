"""Production resource/container and Mac wiring checks (stdlib only, no photo fixtures)."""
import importlib.util
import base64
import json
import pathlib
import plistlib
import re
import shutil
import struct
import tempfile
import unittest
import zlib
import sys

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location('icons', pathlib.Path(__file__).with_name('check-icon-resources.py'))
icons = importlib.util.module_from_spec(spec); spec.loader.exec_module(icons)


png_pixels = icons.png_pixels


def native_fixture(source):
    captured = json.loads((icons.ROOT/'scripts/fixtures/mac-icon-argb.json').read_text())['icons'][source]
    entries = {}
    for kind, name in icons.ICNS_FRAMES.items():
        entries[kind] = (base64.b64decode(captured[kind.decode()]) if kind in (b'ic04', b'ic05')
                         else (icons.ASSETS/(source+'.iconset')/name).read_bytes())
    return entries


def container(entries):
    body = b''.join(struct.pack('>4sI', key, len(value)+8)+value for key,value in entries.items())
    return b'icns'+struct.pack('>I', len(body)+8)+body


class IconsTest(unittest.TestCase):
    # Falsifier: compare iconutil's re-exported small PNGs instead of stored ARGB,
    # or ignore the small/Retina representations. Payloads came from the failed native run.
    def test_native_stored_frames_match_and_reject_changed_artwork(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp)/'test.icns'
            for source in ('app','document'):
                entries = native_fixture(source)
                path.write_bytes(container(entries))
                icons.verify_icns(path, source)
                for kind in icons.ICNS_FRAMES:
                    missing = dict(entries); del missing[kind]
                    path.write_bytes(container(missing))
                    with self.assertRaises(ValueError): icons.verify_icns(path, source)
                # This is the actual problematic native sample. Stored RGB is near
                # source33; iconutil's exported PNG instead contains193 at alpha41.
                stored = icons.argb_pixels(entries[b'ic04'],16)
                self.assertEqual(list(stored[2][0][4:6]), [31,31])
                self.assertEqual(stored[2][0][7],41)
                expected = png_pixels((icons.ASSETS/(source+'.iconset')/'icon_16x16.png').read_bytes())
                stored[2][0][4] = 193
                with self.assertRaisesRegex(ValueError,'colours'):
                    icons.verify_pixels(expected,stored,'exported small frame')
                # Write a changed native payload through the production parser.
                for channel in (0,1):  # alpha change and opaque RGB change
                    planes = [bytearray(entries_value) for entries_value in (
                        b''.join(expected[2])[3::4], b''.join(expected[2])[0::4],
                        b''.join(expected[2])[1::4], b''.join(expected[2])[2::4])]
                    center = 8*16+8
                    planes[channel][center] ^= 32
                    # Literal runs make a simple independent encoder for test mutations.
                    changed = b'ARGB'+b''.join(bytes([127])+plane[i:i+128]
                        for plane in planes for i in range(0,len(plane),128))
                    bad = dict(entries); bad[b'ic04'] = changed
                    path.write_bytes(container(bad))
                    with self.assertRaises(ValueError): icons.verify_icns(path,source)

    def test_argb_rejects_broken_planes(self):
        payload = native_fixture('app')[b'ic04']
        for bad in (b'PNG '+payload[4:], payload[:-1], payload+b'X', b'ARGB'+bytes([255,0])*2):
            with self.assertRaises(ValueError): icons.argb_pixels(bad,16)

    def test_native_icon_check_runs_before_compilation(self):
        workflow = (icons.ROOT/'.github/workflows/mac-proto.yml').read_text()
        self.assertLess(workflow.index('check-icon-resources.py --native'), workflow.index('cargo test'))

    # Falsifier: replace resources with the opaque preview images or omit a standard size.
    def test_ico_entries_and_alpha_are_real_production_pixels(self):
        manifest = icons.verify_assets()
        for entry in manifest['resources']:
            data = (icons.ASSETS / entry['file']).read_bytes()
            reserved, kind, count = struct.unpack('<HHH', data[:6]); self.assertEqual((reserved,kind),(0,1))
            sizes = []
            for i in range(count):
                w,h,_,_,planes,bpp,n,offset = struct.unpack('<BBBBHHII',data[6+16*i:22+16*i])
                w,h = w or 256,h or 256; self.assertEqual(w,h); sizes.append(w)
                iw,ih,rows = png_pixels(data[offset:offset+n]); self.assertEqual((iw,ih),(w,h))
                self.assertEqual([rows[y][x*4+3] for x,y in [(0,0),(w-1,0),(0,h-1),(w-1,h-1)]],[0]*4)
                self.assertEqual(rows[h//2][w//2*4+3],255)
            self.assertEqual(sorted(sizes),manifest['ico_sizes'])

    # Falsifier: use the old 256 master, omit a face, or give documents the app v3 artwork.
    def test_high_resolution_iconsets_and_generic_artwork_are_distinct(self):
        for name in ['app','document']:
            frames = icons.ASSETS / (name + '.iconset')
            icons.verify_iconset(frames, name)
            for size,density in [(16,1),(16,2),(32,1),(32,2),(128,1),(128,2),(256,1),(256,2),(512,1),(512,2)]:
                suffix = '@2x' if density == 2 else ''
                w,h,rows = png_pixels((frames / f'icon_{size}x{size}{suffix}.png').read_bytes())
                self.assertEqual((w,h),(size*density,size*density))
                self.assertEqual(rows[0][3],0); self.assertEqual(rows[h//2][w//2*4+3],255)
        self.assertNotEqual((icons.ASSETS/'app-1024.png').read_bytes(),(icons.ASSETS/'document-1024.png').read_bytes())

    # Falsifier: remove a plist reference, change handler rank, or omit the document resource copy.
    def test_actual_bundle_script_plist_and_copied_resources_agree(self):
        shell = (icons.ROOT/'scripts/mac-bundle.sh').read_text(encoding='utf-8')
        xml = shell.split('<<PLIST\n',1)[1].split('\nPLIST',1)[0]
        xml = xml.replace('$BUNDLE_VERSION','1.0.8').replace('$VERSION','1.0.8').replace('$SOURCE_REVISION','0'*40)
        info = plistlib.loads(xml.encode())
        with tempfile.TemporaryDirectory() as tmp:
            app = pathlib.Path(tmp)/'Falcon.app'; resources = app/'Contents/Resources'; resources.mkdir(parents=True)
            plist = app/'Contents/Info.plist'; plist.write_bytes(plistlib.dumps(info))
            # Native small-frame payloads plus unchanged approved PNG representations.
            for source,dest in re.findall(r'^package_icon (\S+) (\S+)$', shell, re.M):
                (resources/(dest+'.icns')).write_bytes(container(native_fixture(source)))
            icons.verify_bundle(app)
            self.assertIn('check-icon-resources.py" "$APP"',shell)
            self.assertIn('iconutil -c icns',shell)
            self.assertIn('--icns "$CONTENTS/Resources/$destination.icns" "$source"',shell)
            info['CFBundleDocumentTypes'][0]['CFBundleTypeIconFile']='falcon'
            plist.write_bytes(plistlib.dumps(info))
            with self.assertRaises(ValueError): icons.verify_bundle(app)


if __name__ == '__main__': unittest.main()
