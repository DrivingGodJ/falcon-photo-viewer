# Regenerates every fixture in this directory. Deterministic: the same Pillow build produces
# byte-identical output, and nothing here reads the clock, the filesystem or a random source.
#
# Why the fixtures are COMMITTED rather than synthesized in the test: three of them
# (the CMYK family, the truncated pair, the 320 KB-APP2 one) are byte-surgery on an encoder's
# output, and a test that re-does that surgery every run is asserting against its own arithmetic
# rather than against a fixed artefact. The generator is committed beside them so the artefact is
# reproducible and reviewable, not magic.
#
#   python make_mismatch_fixtures.py
#
# Written for the v1.0.0-rc "BYTES OVER NAMES" round (FIX_2026-09-02_bytes_over_names.md).
import io
import os
import struct

from PIL import Image

OUT = os.path.dirname(os.path.abspath(__file__))
W = H = 16


def write(name, data):
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)
    print(f"{name:28s} {len(data):>8d} B")


def rgb16():
    """A 16x16 image with a deterministic, non-uniform pattern (so a decode that silently
    returns a flat buffer cannot pass a pixel assertion)."""
    im = Image.new("RGB", (W, H))
    px = im.load()
    for y in range(H):
        for x in range(W):
            px[x, y] = (x * 16, y * 16, (x * y) % 256)
    return im


def encode(im, **kw):
    buf = io.BytesIO()
    im.save(buf, **kw)
    return buf.getvalue()


def tiny_icc(name):
    """The smallest ICC blob whose `desc` tag reads back as `name` — the same layout the crate's
    own `tiny_icc` test helper builds (falcon-decode/src/lib.rs)."""
    ascii_name = name.encode("ascii")
    count = len(ascii_name) + 1  # the ASCII count includes the trailing NUL
    tag_off = 132 + 12  # header(128) + tag count(4) + one 12-byte tag entry
    v = bytearray(tag_off)
    v[128:132] = struct.pack(">I", 1)  # one tag
    v[132:136] = b"desc"
    v[136:140] = struct.pack(">I", tag_off)
    v[140:144] = struct.pack(">I", 12 + count)
    v += b"desc" + b"\0" * 4 + struct.pack(">I", count) + ascii_name + b"\0"
    return bytes(v)


def patch_adobe_transform(jpg, value):
    """Flip the Adobe APP14 colour-transform byte (0 = CMYK, 2 = YCCK)."""
    i = 2
    while i < len(jpg) - 1:
        assert jpg[i] == 0xFF, f"not at a marker at {i}"
        m = jpg[i + 1]
        if m in (0xD8, 0xD9) or 0xD0 <= m <= 0xD7:
            i += 2
            continue
        seg_len = struct.unpack(">H", jpg[i + 2 : i + 4])[0]
        if m == 0xEE and jpg[i + 4 : i + 9] == b"Adobe":
            out = bytearray(jpg)
            out[i + 2 + seg_len - 1] = value
            return bytes(out)
        if m == 0xDA:
            break
        i += 2 + seg_len
    raise SystemExit("no Adobe APP14 segment found")


def strip_app14(jpg):
    i, out = 2, bytearray(jpg[:2])
    while i < len(jpg) - 1:
        assert jpg[i] == 0xFF
        m = jpg[i + 1]
        if m in (0xD8, 0xD9) or 0xD0 <= m <= 0xD7:
            out += jpg[i : i + 2]
            i += 2
            continue
        seg_len = struct.unpack(">H", jpg[i + 2 : i + 4])[0]
        if m == 0xDA:
            out += jpg[i:]
            return bytes(out)
        if m != 0xEE:
            out += jpg[i : i + 2 + seg_len]
        i += 2 + seg_len
    return bytes(out)


def insert_after_soi(jpg, blob):
    return jpg[:2] + blob + jpg[2:]


rgb = rgb16()

# ── the sniff family ────────────────────────────────────────────────────────────────────────
png_bytes = encode(rgb, format="PNG", optimize=False, compress_level=9)
jpg_bytes = encode(rgb, format="JPEG", quality=90, subsampling=0)

write("png_named_jpg.jpg", png_bytes)  # rows 1/5/6/7/10 — the reduced twin of the field file
write("jpeg_named_png.png", jpg_bytes)  # row 2 — the mirror
write("junk_named_jpg.jpg", b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog >>\nendobj\n" * 8)

# row 9 — a Display-P3 PNG wearing a .jpg name. The ICC's `desc` is the whole point: the PNG
# colour door reads it, the JPEG one never gets past the missing SOI.
write("p3_png_named_jpg.jpg", encode(rgb, format="PNG", icc_profile=tiny_icc("Display P3")))

# ── R1 — the 4-component family ─────────────────────────────────────────────────────────────
cmyk_bytes = encode(rgb.convert("CMYK"), format="JPEG", quality=90, subsampling=0)
write("cmyk_adobe.jpg", cmyk_bytes)  # Adobe APP14, transform 0
write("cmyk_no_app14.jpg", strip_app14(cmyk_bytes))  # 4 components, no APP14 at all

# A GENUINE YCCK file, not a CMYK one with its APP14 byte flipped. In Adobe's YCCK the stored
# chroma triple is the RGB->YCbCr transform of the CMY INK channels, and K is stored inverted
# exactly as in a CMYK file. libjpeg writes every CMYK-mode channel inverted, so handing it the
# values (255 - stored) makes the file carry precisely the bytes we want; the APP14 transform byte
# is then set to 2 to declare what those bytes are.
#   stored = (Y, Cb, Cr, 255 - K_ink)  with (Y,Cb,Cr) = RGB->YCbCr(C_ink, M_ink, Y_ink)
# Verified round-trip: libjpeg (Pillow) and WIC both decode this back to the source image.
cmyk_im = rgb.convert("CMYK")
ink = cmyk_im.split()
ycc = Image.merge("RGB", (ink[0], ink[1], ink[2])).convert("YCbCr").split()
invert = lambda ch: ch.point(lambda v: 255 - v)
ycck_src = Image.merge(
    "CMYK", (invert(ycc[0]), invert(ycc[1]), invert(ycc[2]), Image.new("L", (W, H), 0))
)
write("cmyk_ycck.jpg", patch_adobe_transform(encode(ycck_src, format="JPEG", quality=95, subsampling=0), 2))

# ── R2 — the truncation family ──────────────────────────────────────────────────────────────
# A 128x128 source so the scan is long enough that cutting 15% of it lands mid-MCU rather than
# in the header.
big = Image.new("RGB", (128, 128))
bpx = big.load()
for y in range(128):
    for x in range(128):
        bpx[x, y] = (x * 2, y * 2, (x + y) % 256)
big_jpg = encode(big, format="JPEG", quality=90, subsampling=0)
assert big_jpg[-2:] == b"\xff\xd9"
write("no_eoi.jpg", big_jpg[:-2])  # only the 2-byte EOI is missing
write("truncated.jpg", big_jpg[: int(len(big_jpg) * 0.85)])  # cut mid-scan

# ── R3 — the SOF past 256 KB ────────────────────────────────────────────────────────────────
# Five maximal APP2 segments (65 533 payload bytes each) ahead of everything else: 327 675 bytes
# before the SOF, which is how a real 500 KB embedded ICC reaches a file (APP2 is length-capped at
# 65 535, so a big profile is ALWAYS chunked).
app2 = b""
for chunk in range(5):
    payload = b"ICC_PROFILE\0" + bytes([chunk + 1, 5]) + bytes(65533 - 2 - 14)
    app2 += b"\xff\xe2" + struct.pack(">H", len(payload) + 2) + payload
write("big_app2.jpg", insert_after_soi(jpg_bytes, app2))
