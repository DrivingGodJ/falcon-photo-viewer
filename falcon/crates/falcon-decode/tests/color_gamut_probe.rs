//! THE COLOR ROUND (v0.8.140) — source-gamut resolution reads the profile's COLORIMETRY, not its name.
//!
//! THE DEFECT, as the verify-first probe measured it on 220830c: `shot_source_gamut` resolved a
//! file's source gamut by NAME-MATCHING the embedded ICC profile's `desc` string
//! (`Gamut::from_description`) and fell back to sRGB when the name was unrecognised. A macOS screen
//! capture embeds the DISPLAY's own profile, whose description on the tester's machine is literally
//! "Display" — no name match — so genuine Display-P3 pixels were converted FROM sRGB and came out
//! uniformly desaturated in EVERY output mode, silently. The bytes were in the file the whole time.
//!
//! THE FIXTURE: one profile, two names. Both are produced from
//! `falcon_color::icc_bytes_for_gamut(Gamut::DisplayP3)` — the tree's OWN serializer, so the
//! colorimetry is exact by construction — and then differ ONLY in the bytes of the `desc` string.
//! `fixtures_differ_only_in_the_description_string` proves that byte-for-byte, so the name is the
//! sole experimental variable and any difference in outcome is attributable to it alone.
//!
//! The two tests this file was BORN red on — `..._from_the_bytes_alone` (the falcon-color layer) and
//! `macos_screenshot_png_resolves_to_display_p3` (end to end through falcon-decode) — are green
//! here. Their pre-fix red is recorded in `scratchpad\color_probe\probe_run_output.txt`.
//!
//! ONE DELIBERATE CHANGE from the probe's wording. The probe expressed its falcon-color-layer claim
//! as `Gamut::from_description("Display") == Some(DisplayP3)`, because a name matcher was the only
//! API there was. Making THAT assertion pass would mean adding a bare "display" arm to the name
//! matcher — which the round forbids outright, because it would swallow "Generic RGB Display" and
//! every vendor "…Display" profile that is NOT P3 (probe finding 5). So the claim is re-pointed at
//! the resolver that now owns it, and the forbidden arm is pinned SHUT by
//! `the_name_matcher_still_refuses_to_guess_at_display`.

use falcon_color::Gamut;
use falcon_decode::{frame_source_gamut, load_watermark_png_srgb, shot_source_gamut, FrameSource, Shot, SrcKind};

// ─────────────────────────────────────────────────────────────────────────────
// Fixture: a real Display-P3 matrix/TRC ICC, renamed.
// ─────────────────────────────────────────────────────────────────────────────

/// Locate the `desc` tag: `(tag offset, string byte offset, string byte length)`.
fn desc_window(icc: &[u8]) -> (usize, usize, usize) {
    let be32 = |o: usize| u32::from_be_bytes([icc[o], icc[o + 1], icc[o + 2], icc[o + 3]]) as usize;
    let n = be32(128);
    for k in 0..n {
        let e = 132 + k * 12;
        if &icc[e..e + 4] == b"desc" {
            let off = be32(e + 4);
            assert_eq!(&icc[off..off + 4], b"mluc", "icc_bytes_for_gamut writes a v4 mluc desc");
            assert_eq!(be32(off + 8), 1, "one record");
            let len = be32(off + 20); // record: lang2 country2 LEN4 OFF4
            let str_off = off + be32(off + 24);
            return (off, str_off, len);
        }
    }
    panic!("no desc tag");
}

/// A valid display ICC for `g` whose description reads exactly `desc`.
///
/// The rename is IN PLACE: the `mluc` record's length field governs the read (this tree's
/// `icc_description` and ColorSync work that way), so shortening the string leaves every other tag
/// offset untouched. The tail of the old string is zero-filled so that two profiles built here
/// differ in nothing but the string bytes themselves.
fn icc_named(g: Gamut, desc: &str) -> Vec<u8> {
    let mut icc = falcon_color::icc_bytes_for_gamut(g).expect("the tree serializes its own encoding");
    let (tag_off, str_off, old_len) = desc_window(&icc);
    let utf16: Vec<u8> = desc.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    assert!(utf16.len() <= old_len, "rename must fit the original string slot");
    let new_len = utf16.len();
    icc[tag_off + 20..tag_off + 24].copy_from_slice(&(new_len as u32).to_be_bytes());
    icc[str_off..str_off + new_len].copy_from_slice(&utf16);
    for b in &mut icc[str_off + new_len..str_off + old_len] {
        *b = 0;
    }
    icc
}

fn p3_icc_named(desc: &str) -> Vec<u8> {
    icc_named(Gamut::DisplayP3, desc)
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixture: a 4x4 saturated-red PNG carrying that profile in an iCCP chunk.
// Hand-rolled (STORED deflate blocks + adler32) so the probe needs no new dependency.
// ─────────────────────────────────────────────────────────────────────────────

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// zlib stream using only STORED (uncompressed) deflate blocks — trivially correct, and every
/// inflater accepts it.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78u8, 0x01]; // CM=8 CINFO=7, FCHECK making the header %31==0
    let mut i = 0usize;
    loop {
        let n = (data.len() - i).min(65_535);
        let last = i + n >= data.len();
        out.push(if last { 1 } else { 0 });
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + n]);
        i += n;
        if last {
            break;
        }
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&(((b << 16) | a) as u32).to_be_bytes());
    out
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = (body.len() as u32).to_be_bytes().to_vec();
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(body);
    v.extend_from_slice(&crc_input);
    v.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    v
}

/// A 4x4 flat-colour PNG carrying `icc` as a zlib-compressed `iCCP` chunk — the shape a macOS
/// screen capture has. `alpha` picks RGB vs RGBA (the watermark door wants RGBA); `icc = None`
/// writes the untagged control.
fn flat_png(px: [u8; 4], icc: Option<&[u8]>, alpha: bool) -> Vec<u8> {
    const W: u32 = 4;
    const H: u32 = 4;
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&W.to_be_bytes());
    ihdr.extend_from_slice(&H.to_be_bytes());
    // 8-bit, truecolour RGB(A), deflate, adaptive filtering, non-interlaced
    ihdr.extend_from_slice(&[8, if alpha { 6 } else { 2 }, 0, 0, 0]);
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));

    if let Some(icc) = icc {
        let mut iccp = b"ICC".to_vec(); // profile name (Latin-1)
        iccp.push(0); // NUL terminator
        iccp.push(0); // compression method 0 = zlib/deflate
        iccp.extend_from_slice(&zlib_stored(icc));
        png.extend_from_slice(&chunk(b"iCCP", &iccp));
    }

    let mut raw = Vec::new();
    for _ in 0..H {
        raw.push(0u8); // filter type 0 (None)
        for _ in 0..W {
            raw.extend_from_slice(if alpha { &px[..] } else { &px[..3] });
        }
    }
    png.extend_from_slice(&chunk(b"IDAT", &zlib_stored(&raw)));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

/// The photo fixtures' pixel: saturated red, the channel P3 most exceeds sRGB on.
const RED: [u8; 4] = [255, 0, 0, 255];
/// The watermark fixture's pixel. Saturated red would be USELESS here: P3 red is outside sRGB, so
/// converting it CLIPS straight back to 255,0,0 and a broken conversion would look identical to a
/// working one. This one is inside both gamuts, so the conversion has somewhere to move it.
const BRAND: [u8; 4] = [200, 90, 40, 255];

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("falcon_color_round_{}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Write a fixture PNG into a per-test scratch dir and wrap it in the `Shot` the decoder takes.
fn png_shot(icc: &[u8], tag: &str) -> (Shot, std::path::PathBuf) {
    let path = scratch(tag).join(format!("{tag}.png"));
    std::fs::write(&path, flat_png(RED, Some(icc), false)).expect("write fixture");
    (shot_at(&path, SrcKind::Png, tag), path)
}

fn shot_at(path: &std::path::Path, kind: SrcKind, name: &str) -> Shot {
    Shot {
        id: 0,
        name: name.to_string(),
        has_raw: false,
        has_jpg: true,
        raw: None,
        jpg: Some(path.to_path_buf()),
        kind,
        cloud_placeholder: false,
        sniffed: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Anti-vacuity: the fixture really is P3, and the name really is the only variable.
// ─────────────────────────────────────────────────────────────────────────────

/// The published Display-P3 (D65) linear-RGB → XYZ matrix, typed here as an INDEPENDENT oracle
/// (falcon-color's own table is private to that crate).
const P3_D65_TO_XYZ: [[f32; 3]; 3] = [
    [0.486_570_9, 0.265_667_7, 0.198_217_3],
    [0.228_974_8, 0.691_738_8, 0.079_286_5],
    [0.000_000_0, 0.045_113_4, 1.043_944_4],
];

#[test]
fn fixture_really_carries_display_p3_primaries_under_the_name_display() {
    let icc = p3_icc_named("Display");
    let p = falcon_color::parse_display_icc(&icc, "probe")
        .expect("the fixture is a well-formed matrix/TRC display profile");
    for r in 0..3 {
        for c in 0..3 {
            let (got, want) = (p.rgb_to_xyz[r][c], P3_D65_TO_XYZ[r][c]);
            assert!(
                (got - want).abs() < 2e-3,
                "colorant [{r}][{c}] = {got} but Display P3 is {want} — the fixture is not P3"
            );
        }
    }
    eprintln!(
        "fixture colorants (D50 PCS un-adapted back to D65 by parse_display_icc): {:?}",
        p.rgb_to_xyz
    );
}

#[test]
fn fixtures_differ_only_in_the_description_string() {
    let red = p3_icc_named("Display");
    let green = p3_icc_named("Display P3");
    assert_eq!(red.len(), green.len(), "same profile, same length");
    // The permitted window is the ORIGINAL string SLOT (the un-renamed profile's 25-char desc), not
    // either fixture's shortened record length — the two names occupy different amounts of it.
    let (tag_off, str_off, str_len) =
        desc_window(&falcon_color::icc_bytes_for_gamut(Gamut::DisplayP3).unwrap());
    // the mluc record's length field (4 bytes) + that string slot are the ONLY permitted differences
    let tag_len_field = tag_off + 20..tag_off + 24;
    let diffs: Vec<usize> = (0..red.len())
        .filter(|&i| red[i] != green[i])
        .filter(|i| !tag_len_field.contains(i))
        .collect();
    assert!(
        diffs.iter().all(|&i| i >= str_off && i < str_off + str_len),
        "the two fixtures differ OUTSIDE the desc string (at {diffs:?}) — the name is not the only \
         variable and the experiment is invalid"
    );
    assert!(!diffs.is_empty(), "the two fixtures are identical — the rename did nothing");
    eprintln!("fixtures differ at {} bytes, all inside the desc string window", diffs.len());
}

// ─────────────────────────────────────────────────────────────────────────────
// THE ROUND'S TWO FALSIFIERS. Both were RED on 220830c; both must stay green.
// ─────────────────────────────────────────────────────────────────────────────

/// WAS RED (falcon-color layer): the macOS display profile is resolvable — from its BYTES.
///
/// The probe wrote this as `from_description("Display") == Some(DisplayP3)` because a name matcher
/// was all there was. The fix does not teach the name matcher that word (see the test below); it
/// stops asking the name at all when the file carries a profile.
#[test]
fn the_macos_display_profile_resolves_from_the_bytes_alone() {
    let icc = p3_icc_named("Display");
    let got = Gamut::from_icc_bytes(&icc);
    eprintln!("Gamut::from_icc_bytes(<P3 colorants, desc \"Display\">) = {got:?}");
    assert_eq!(
        got,
        Some(Gamut::DisplayP3),
        "a macOS screenshot's profile is named \"Display\" but carries P3 colorants; the resolver \
         returned {got:?}, so the caller falls back to sRGB and P3 pixels render dull"
    );
    // …and it is measured, not guessed: the match is essentially exact.
    let (near, d) = Gamut::nearest_from_icc_bytes(&icc).expect("a matrix/TRC profile");
    eprintln!("nearest modeled gamut {near:?} at d = {d:.8} (τ = {})", falcon_color::GAMUT_MATCH_TOL);
    assert!(d < falcon_color::GAMUT_MATCH_TOL / 50.0, "an exact P3 profile must match far inside τ");
}

/// THE ARM THE ROUND FORBIDS, pinned SHUT (probe finding 5). A bare "display" name arm would
/// swallow "Generic RGB Display" and every vendor "…Display" profile that is not P3 — colorimetry,
/// not another name. If someone ever adds it, this fails.
#[test]
fn the_name_matcher_still_refuses_to_guess_at_display() {
    for name in ["Display", "Generic RGB Display", "LCD Display", "Color LCD"] {
        assert_eq!(
            Gamut::from_description(name),
            None,
            "{name:?} names no colour space — a name arm here would mislabel every vendor profile \
             that merely contains the word"
        );
    }
}

/// WAS RED (end to end, falcon-decode): a P3-primaries PNG named "Display" must not resolve to sRGB.
/// This is the tester's macOS screen capture, in miniature.
#[test]
fn macos_screenshot_png_resolves_to_display_p3() {
    let (shot, path) = png_shot(&p3_icc_named("Display"), "named_display");
    let got = shot_source_gamut(&shot);
    eprintln!("shot_source_gamut({}) = {got:?}  (fixture desc = \"Display\")", path.display());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(
        got,
        Gamut::DisplayP3,
        "the PNG carries genuine Display-P3 colorants but its profile is named \"Display\"; \
         shot_source_gamut answered {got:?} — every output mode then converts FROM sRGB and the \
         photograph is uniformly desaturated, silently"
    );
}

/// CONTROL: same bytes, name "Display P3" → resolved correctly BEFORE this round and still does.
/// Proves the harness, the PNG writer, the iCCP chunk, the `mluc` reader and the `Shot` seam are
/// all sound, and isolates the name as the single discriminator.
#[test]
fn control_same_png_named_display_p3_resolves_to_display_p3() {
    let (shot, path) = png_shot(&p3_icc_named("Display P3"), "named_display_p3");
    let got = shot_source_gamut(&shot);
    eprintln!("shot_source_gamut({}) = {got:?}  (fixture desc = \"Display P3\")", path.display());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(got, Gamut::DisplayP3, "the control fixture must resolve — else the harness is broken");
}

// ─────────────────────────────────────────────────────────────────────────────
// C4 — the rest of the matrix, through the real doors.
// ─────────────────────────────────────────────────────────────────────────────

/// C4 (a)(b)(d): a standard-NAMED profile of each modeled gamut resolves to that gamut through the
/// real PNG door — the strictly-better invariant, end to end rather than in falcon-color alone.
#[test]
fn every_standard_named_profile_still_resolves_to_its_own_gamut() {
    for (g, name) in [
        (Gamut::Srgb, "sRGB"),
        (Gamut::DisplayP3, "Display P3"),
        (Gamut::AdobeRgb, "Adobe RGB (1998)"),
        (Gamut::Rec2020, "Rec. 2020"),
    ] {
        let tag = format!("std_{}", g.label().replace([' ', '.'], "_"));
        let (shot, path) = png_shot(&icc_named(g, name), &tag);
        let got = shot_source_gamut(&shot);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(got, g, "{name:?} must still resolve {g:?} — it did before this round");
    }
}

/// C4 (g): THE DESIGNED EXCEPTION, through the real door. A PNG whose profile is NAMED "Display P3"
/// over plain sRGB colorants now resolves sRGB — the bytes win. This is the one input on which the
/// new path may disagree with the old, and disagreeing is the point.
#[test]
fn a_mislabeled_png_follows_its_bytes_not_its_name() {
    assert_eq!(Gamut::from_description("Display P3"), Some(Gamut::DisplayP3), "the name says P3");
    let (shot, path) = png_shot(&icc_named(Gamut::Srgb, "Display P3"), "mislabeled");
    let got = shot_source_gamut(&shot);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(got, Gamut::Srgb, "sRGB colorants under a P3 name must resolve sRGB");
}

/// C4 (h) — THE WATERMARK DOOR. A logo PNG whose profile is named "Display" over P3 colorants: the
/// door must report Display P3 and CONVERT the pixels to sRGB, not ship them untouched. Before this
/// round the name matched nothing, the door reported sRGB, and a brand red shipped desaturated into
/// every ./Web deliverable — the exact failure v0.8.104's own doc comment worried about.
#[test]
fn the_watermark_door_reads_a_display_named_profile() {
    let dir = scratch("wm");
    let named = dir.join("display_named.png");
    std::fs::write(&named, flat_png(BRAND, Some(&p3_icc_named("Display")), true)).expect("write");
    let plain = dir.join("untagged.png"); // the same pixels, NO profile — "nothing to convert"
    std::fs::write(&plain, flat_png(BRAND, None, true)).expect("write");

    let (tagged_px, _, _, src) = load_watermark_png_srgb(&named).expect("load the Display-named logo");
    let (plain_px, _, _, psrc) = load_watermark_png_srgb(&plain).expect("load the untagged logo");
    eprintln!(
        "watermark: Display-named → {src:?} px {:?}; untagged → {psrc:?} px {:?}",
        &tagged_px[0..3],
        &plain_px[0..3]
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(src, Gamut::DisplayP3, "the logo's own colorants are P3 — the door must say so");
    assert_eq!(psrc, Gamut::Srgb, "an untagged logo keeps the sRGB assumption");
    // The pixels were really converted, and onto the INDEPENDENT oracle — not merely moved.
    let want = falcon_color::transform_rgb8([BRAND[0], BRAND[1], BRAND[2]], Gamut::DisplayP3, Gamut::Srgb);
    assert_eq!(&tagged_px[0..3], &want[..], "the mark must be converted to sRGB at load");
    assert_eq!(&plain_px[0..3], &BRAND[0..3], "an untagged mark must ship as authored");
    assert_ne!(&tagged_px[0..3], &plain_px[0..3], "…and that conversion must be visible");
}

/// C4 (h) — THE `frame_source_gamut` PREVIEW DOOR, on a synthetic HEIF container (no codec, no
/// testkit, runs on every machine). The MASTER declares sRGB by `nclx`; the THUMBNAIL item carries a
/// `prof` box holding P3 colorants described "Display". Before this round the preview's name matched
/// nothing, the door fell back to the master, and a preview-sourced thumb tile was converted from
/// sRGB. The preview's own bytes must now decide, and be distinguishable from the master's answer.
#[test]
fn the_preview_door_reads_a_display_named_profile() {
    let bx = |t: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(body);
        v
    };
    let full = |t: &[u8; 4], ver: u8, flags: u32, body: &[u8]| -> Vec<u8> {
        let mut b = vec![ver, (flags >> 16) as u8, (flags >> 8) as u8, flags as u8];
        b.extend_from_slice(body);
        bx(t, &b)
    };
    // property 1 — the master: `colr` nclx, primaries 1 = sRGB.
    let master = {
        let mut b = b"nclx".to_vec();
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&13u16.to_be_bytes());
        b.extend_from_slice(&6u16.to_be_bytes());
        b.push(0x80);
        bx(b"colr", &b)
    };
    // property 2 — the preview: `colr` prof, a REAL P3 profile described "Display".
    let preview = {
        let mut b = b"prof".to_vec();
        b.extend_from_slice(&p3_icc_named("Display"));
        bx(b"colr", &b)
    };
    let ipco = bx(b"ipco", &[master, preview].concat());
    let ipma = full(b"ipma", 0, 0, &[
        0, 0, 0, 2, // entry_count
        0, 1, 1, 1, // item 1 (master)    → [property 1]
        0, 2, 1, 2, // item 2 (thumbnail) → [property 2]
    ]);
    let iprp = bx(b"iprp", &[ipco, ipma].concat());
    let pitm = full(b"pitm", 0, 0, &[0, 1]);
    let thmb = bx(b"thmb", &[2u16.to_be_bytes(), 1u16.to_be_bytes(), 1u16.to_be_bytes()].concat());
    let meta = full(b"meta", 0, 0, &[pitm, full(b"iref", 0, 0, &thmb), iprp].concat());
    let mut bytes = bx(b"ftyp", b"heic\0\0\0\0heic");
    bytes.extend_from_slice(&meta);

    let dir = scratch("heif_preview");
    let path = dir.join("preview_display.heic");
    std::fs::write(&path, &bytes).expect("write the synthetic container");
    let shot = shot_at(&path, SrcKind::Heic, "preview_display");

    let master_g = frame_source_gamut(&shot, FrameSource::MainImage);
    let preview_g = frame_source_gamut(&shot, FrameSource::EmbeddedPreview);
    eprintln!("synthetic HEIF: master → {master_g:?}, preview → {preview_g:?}");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(master_g, Gamut::Srgb, "the master declares sRGB by nclx — the fixture's control");
    assert_eq!(
        preview_g,
        Gamut::DisplayP3,
        "the preview item carries P3 colorants under the name \"Display\"; the preview door must \
         read its BYTES, not fall back to the master because the name matched nothing"
    );
}

/// C5 — THE LOG LINE, and its silence. A resolution the NAME would have got right says nothing (an
/// ordinary folder of sRGB photographs must not write a line per file); a resolution the name would
/// have missed says exactly one line, naming the raw description, the method, the result AND the
/// distance — and repeating the question about the same file does not repeat the line.
///
/// That last number is the point: the round fixes files whose colorimetry lands INSIDE τ, and a
/// measured-panel profile may not. When one does not, this line is what says so from the field,
/// with the distance, instead of the miss being invisible all over again.
///
/// v0.8.141 (R1) — AND THE SECOND ARM, which exists because that last paragraph was only true of
/// SOME misses. Arm 1's gate is "the answer changed", so a measured-panel profile whose NAME
/// happens to carry a token the matcher knows (a Dell "…, sRGB" sitting 0.03 outside τ) resolves
/// sRGB by BOTH routes and is silent — three of the seven measured display profiles on the owner's
/// machine are in that class. Rows (3) and (4) below are that case: arm 1 stays silent, and the new
/// per-PROFILE arm reports the miss once no matter how many files carry the profile.
#[test]
fn a_resolution_the_name_would_have_missed_parks_exactly_one_line() {
    let _ = falcon_decode::drain_decode_notes(); // start from a known state

    // (1) SILENT: a standard sRGB profile — colorimetry and name agree, so nothing is worth saying.
    let (quiet, qpath) = png_shot(&icc_named(Gamut::Srgb, "sRGB IEC61966-2.1"), "note_quiet");
    assert_eq!(shot_source_gamut(&quiet), Gamut::Srgb);

    // (2) SPEAKS: P3 colorants under the name "Display" — and asked three times, as the tiers do.
    let (loud, lpath) = png_shot(&p3_icc_named("Display"), "note_loud");
    for _ in 0..3 {
        assert_eq!(shot_source_gamut(&loud), Gamut::DisplayP3);
    }

    // (3)+(4) THE CLASS ARM 1 CANNOT SEE. A profile whose colorants are real and measurable but sit
    // outside τ — true cinema DCI-P3 stands in for a measured panel at 0.08045 out — carrying a
    // name the matcher DOES recognise, so name and fallback agree and arm 1 has nothing to report.
    // TWO files carry the identical profile: the per-PROFILE key must make that ONE line, which is
    // the whole reason arm 1 was not simply widened (a folder of 400 such exports would otherwise
    // spend the entire per-file budget saying the same sentence).
    let missing = icc_named(Gamut::DciP3, "Dell S27 sRGB");
    let (miss_a, apath) = png_shot(&missing, "note_miss_a");
    let (miss_b, bpath) = png_shot(&missing, "note_miss_b");
    assert_eq!(shot_source_gamut(&miss_a), Gamut::Srgb, "the NAME answers — that is what makes arm 1 silent");
    assert_eq!(shot_source_gamut(&miss_b), Gamut::Srgb);

    let notes = falcon_decode::drain_decode_notes();
    for p in [&qpath, &lpath, &apath, &bpath] {
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
    for n in &notes {
        eprintln!("note: {n}");
    }
    assert!(
        !notes.iter().any(|n| n.contains("note_quiet")),
        "a profile whose name already said sRGB must not write a line: {notes:?}"
    );
    let mine: Vec<&String> = notes.iter().filter(|n| n.contains("note_loud")).collect();
    assert_eq!(mine.len(), 1, "three tiers asking about one file must cost ONE line, got {mine:?}");
    let line = mine[0];
    for want in ["\"Display\"", "Display P3", "colorimetry", "nearest Display P3", "the name alone said nothing"] {
        assert!(line.contains(want), "the line must carry {want:?} — got {line:?}");
    }

    // ── R1 arm 2 ──
    assert!(
        !notes.iter().any(|n| n.contains("note_miss_a") || n.contains("note_miss_b")),
        "arm 1 keys on the ANSWER CHANGING and these files' names agreed — no per-file line: {notes:?}"
    );
    let misses: Vec<&String> = notes.iter().filter(|n| n.contains("unplaceable")).collect();
    assert_eq!(
        misses.len(),
        1,
        "two files sharing one unplaceable profile must cost exactly ONE per-profile line, got {misses:?}"
    );
    let miss = misses[0];
    for want in ["\"Dell S27 sRGB\"", "0.08045", "nearest modeled gamut (sRGB", "resolve by name"] {
        assert!(miss.contains(want), "the miss line must carry {want:?} — got {miss:?}");
    }
    assert!(
        !miss.contains("note_miss"),
        "the miss line is about the PROFILE, not about a file — naming one would defeat the key: {miss:?}"
    );
}

/// C4 (e) — a LUT-class profile (an `A2B0`, no colorant tags) cannot be measured, so the file falls
/// back honestly: the NAME if it says anything, else sRGB. Nothing panics on the unreadable bytes.
#[test]
fn a_lut_class_profile_falls_back_without_panicking() {
    let mut icc = vec![0u8; 128];
    icc[8..12].copy_from_slice(&0x0420_0000u32.to_be_bytes());
    icc[12..16].copy_from_slice(b"mntr");
    icc[16..20].copy_from_slice(b"RGB ");
    icc[20..24].copy_from_slice(b"XYZ ");
    icc[36..40].copy_from_slice(b"acsp");
    let a2b0 = {
        let mut v = b"mft1".to_vec();
        v.extend_from_slice(&[0u8; 44]);
        v
    };
    icc.extend_from_slice(&1u32.to_be_bytes());
    let off = 128 + 4 + 12;
    icc.extend_from_slice(b"A2B0");
    icc.extend_from_slice(&(off as u32).to_be_bytes());
    icc.extend_from_slice(&(a2b0.len() as u32).to_be_bytes());
    icc.extend_from_slice(&a2b0);
    let size = icc.len() as u32;
    icc[0..4].copy_from_slice(&size.to_be_bytes());

    assert_eq!(Gamut::from_icc_bytes(&icc), None, "a cLUT profile has no colorants to measure");
    let (shot, path) = png_shot(&icc, "lut_class");
    let got = shot_source_gamut(&shot);
    eprintln!("LUT-class profile, no description → {got:?}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert_eq!(got, Gamut::Srgb, "unmeasurable and unnamed → the safe default, not a crash");
}
