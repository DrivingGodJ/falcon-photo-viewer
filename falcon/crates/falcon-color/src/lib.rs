//! Falcon colour — SDR gamut transforms (matrix + tone curve) for colour-managed display.
//!
//! Falcon's render pipeline is otherwise sRGB-assumed: it uploads decoded pixels as a texture that
//! Slint composites and presents as-is. So to show a wide-gamut file (Adobe RGB / Display P3 /
//! Rec.2020) correctly — or to match a calibrated WCG monitor running with OS colour-management OFF —
//! we transform the decoded pixels from the file's source gamut into the user's chosen OUTPUT gamut
//! BEFORE upload. The monitor (set to that gamut) then reproduces them faithfully, with no swapchain
//! colour-space control and no OS involvement needed.
//!
//! The transform is the textbook one for matrix/TRC RGB profiles (which is every photographic gamut):
//! `src RGB → linearise (src TRC) → src→XYZ (3×3, D65) → XYZ→dst (3×3) → dst TRC → dst RGB`, with
//! out-of-gamut values clipped (relative-colorimetric-with-clipping — perceptual mapping is overkill
//! for SDR viewing). The XYZ interchange space is ABSOLUTE (D65-anchored): every gamut's matrix is
//! referenced to that gamut's NATIVE white, so converting between gamuts is a physical XYZ match with
//! NO white re-mapping. The original four gamuts are all D65-native (their whites coincide, so no
//! adaptation ever appears); a `Custom` display ICC's colorants are UN-adapted from the D50 PCS back to
//! the device's native white (see `parse_display_icc`); and v0.8.67's true cinema `DciP3` carries its
//! native DCI white the same way — D65-white content therefore encodes slightly NON-neutral on a
//! DCI-white display, which is exactly what reproduces physical D65 on that screen. This is fast and
//! GPU-portable; CM-2 (per-monitor ICC) / CM-3 (HDR/FP16) build on top later.
//!
//! v0.8.47 — TRC fidelity: for a user-loaded display ICC (`Gamut::Custom`) the DESTINATION tone curve is
//! now applied FAITHFULLY. A measured display profile carries its TRC as a sampled `curv` table (the
//! owner's Dell_S2725QS carries 1024 measured points per channel) or a `para` parametric curve; earlier
//! rounds collapsed it to ONE effective gamma fitted from the curve's midpoint, which lifted shadows
//! (#101010 → #181818, +8/channel, vs the reference #101010). We now keep the real per-channel curve and
//! ENCODE through its inverse via a 4096-entry linear→device LUT (`CUSTOM_LUT_N`), so the CPU tiers and
//! the GPU fast tier (which binds the SAME LUT as a texture) reproduce the display's curve to reference
//! (littleCMS) accuracy. Black-point compensation stays out of scope — a no-op for a black≈0 display.

use std::sync::{Arc, RwLock};

/// A working/display RGB gamut. The original four named gamuts are all D65; the differences are the
/// primaries (gamut size) and the tone response curve. `DisplayP3` covers the common "DCI-P3" monitor
/// mode with a D65 white (P3-D65); `DciP3` (v0.8.67) is TRUE cinema DCI-P3 — the same primaries but the
/// DCI white point (x 0.3140, y 0.3510) and a pure gamma-2.6 TRC — its matrix is referenced to that
/// native white in the shared absolute XYZ space (the same native-white referencing the `Custom` ICC
/// path performs), so no Bradford step appears anywhere for it. `Custom` is a user-loaded
/// monitor/display ICC — its primaries + TRC live in a process-global [`CustomProfile`] (set via
/// [`set_custom_profile`]), so the variant stays data-less and `Copy/Eq` — `Custom` AND `DciP3` are
/// DESTINATION-only targets ([`Gamut::from_description`] deliberately never emits `DciP3`, because
/// consumer files/panels tagged "DCI-P3" are near-always P3-D65 content).
///
/// v0.8.177 — **`SourceIcc` is the mirror image of `Custom`, on the source side.** A file's embedded
/// profile that parses as matrix-TRC but sits outside [`GAMUT_MATCH_TOL`] of everything we model
/// (ProPhoto RGB, an L\*-companded working space, a measured-panel profile) used to be answered by
/// its NAME — for the owner's ProPhoto TIFF that meant Rec.2020, measured at ~28% chroma loss against
/// a reference CM. It is now rendered through its OWN colorants and its OWN per-channel curves. The
/// variant carries an INDEX into a process registry rather than the data itself, so `Gamut` stays
/// `Copy + Eq` and every `src_to_dst_matrix` / `transform_*` call site is unchanged; the registry is
/// append-only, so an index is valid for the life of the process. Source-only in exactly the way
/// `Custom` is destination-only: [`Gamut::from_i32`] never emits it (pinned by
/// `settings_indices_never_emit_a_source_profile`), so it can never become an output target.
///
/// v0.8.177 also RETIRES a stale claim this comment carried: `Hash` was never derived and no
/// `HashMap<Gamut, _>` exists anywhere in the workspace — the source-side caches key on `usize` file
/// indices and compare gamuts with `==`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Gamut {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    Rec2020,
    Custom,
    DciP3,
    /// A SOURCE profile rendered FAITHFULLY from its own bytes — the payload indexes
    /// [`source_profile`]'s process registry. Never a destination.
    SourceIcc(u16),
}

impl Gamut {
    /// Settings index ↔ gamut (0 sRGB / 1 Display P3 / 2 Adobe RGB / 3 Rec.2020 / 4 Custom /
    /// 5 DCI-P3 — appended in v0.8.67 so the persisted indices 0..4 stay stable). Unknown → sRGB
    /// (an old build reading a newer settings.json degrades gracefully to sRGB).
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Gamut::DisplayP3,
            2 => Gamut::AdobeRgb,
            3 => Gamut::Rec2020,
            4 => Gamut::Custom,
            5 => Gamut::DciP3,
            _ => Gamut::Srgb,
        }
    }
    /// The settings index for a gamut — the exact inverse of [`from_i32`] for every value that
    /// function can produce.
    ///
    /// v0.8.177: this exists because `Gamut::Srgb as u32` used to be written directly at five call
    /// sites, which silently assumed "discriminant == settings index". That assumption was true and
    /// undocumented, and a payload-carrying variant makes the cast a compile error rather than a
    /// wrong number — so the assumption is now a named function with a round-trip test
    /// (`settings_index_round_trips`). A source-profile gamut is not a settings value at all and
    /// answers `0` (sRGB), matching `from_i32`'s degrade-to-sRGB rule for anything it cannot name.
    pub fn to_i32(self) -> i32 {
        match self {
            Gamut::Srgb => 0,
            Gamut::DisplayP3 => 1,
            Gamut::AdobeRgb => 2,
            Gamut::Rec2020 => 3,
            Gamut::Custom => 4,
            Gamut::DciP3 => 5,
            Gamut::SourceIcc(_) => 0,
        }
    }

    /// Static display name (the four named gamuts). `Custom` has no static name — the caller that
    /// installs a profile (`install_custom_icc` in the app) tracks its own label; here it reads "Custom".
    ///
    /// v0.8.177: a `SourceIcc` gamut's real name is the PROFILE'S OWN description, which is a
    /// `String` read from the file — it cannot be a `&'static str`. Every user-visible site should
    /// call [`display_name`](Gamut::display_name) instead; this static answer stays deliberately
    /// generic ("Profile") so a site that has not been moved reads as vague rather than as a lie.
    pub fn label(self) -> &'static str {
        match self {
            Gamut::Srgb => "sRGB",
            Gamut::DisplayP3 => "Display P3",
            Gamut::AdobeRgb => "Adobe RGB",
            Gamut::Rec2020 => "Rec. 2020",
            Gamut::Custom => "Custom",
            Gamut::DciP3 => "DCI-P3",
            Gamut::SourceIcc(_) => "Profile",
        }
    }

    /// THE HONEST NAME (v0.8.177) — what to show a user for this gamut. Identical to [`label`] for
    /// every modeled gamut; for a faithful source it is the embedded profile's OWN description
    /// ("ProPhoto RGB", "LStar-RGB-v2.icc"), sanitized and length-bounded by
    /// [`register_source_profile`] at parse time, because that string is untrusted file content.
    ///
    /// The rule this enforces: a faithful-route source is NEVER labelled with a modeled gamut name
    /// it is not. Showing "Rec. 2020" for a ProPhoto file was the visible half of the defect this
    /// round closes.
    pub fn display_name(self) -> String {
        match self {
            Gamut::SourceIcc(_) => match source_profile(self) {
                Some(p) => p.desc.clone(),
                // Unreachable in practice (the registry is append-only and the index came from it);
                // an honest generic beats a panic on a colour path.
                None => "Profile".to_string(),
            },
            g => g.label().to_string(),
        }
    }

    /// Is this the faithful-source route's gamut? The one predicate the GPU doors and the log
    /// formats ask, so no site has to match on the variant shape.
    pub fn is_source_profile(self) -> bool {
        matches!(self, Gamut::SourceIcc(_))
    }

    /// Tone-response-curve FAMILY for an external transform (the GPU fast-tier shaders) that must
    /// exactly match [`Trc::linearize`]/[`Trc::encode`]: `0` = the sRGB piecewise curve (sRGB /
    /// Display P3 / Rec.2020 here), `1` = the Adobe RGB pure gamma ([`ADOBE_GAMMA`]), `2` = the
    /// loaded Custom profile's FAITHFUL per-channel tone curve, `3` = the DCI-P3 pure gamma
    /// ([`DCI_GAMMA`], v0.8.67). For kind 2 the shader ENCODES by sampling the inverse-tone-curve
    /// LUT texture (built from the same data [`custom_encode_lut`] returns; `flags.w` carries the
    /// LUT width [`CUSTOM_LUT_N`]). Kept in lock-step with those fns.
    /// v0.8.177 adds `4` = a FAITHFUL SOURCE's per-channel tone curve, LINEARISED by sampling the
    /// forward (device→linear) LUT texture — rows 3..5 of the same `lut_tex` whose rows 0..2 carry
    /// the kind-2 destination encode LUT. Kind 4 is a LINEARIZE-side kind only (a source profile is
    /// never a destination), exactly as kind 2 is an ENCODE-side kind only.
    ///
    /// The `_ => 0` arm below is deliberately gone. It was the silent trap: a new variant would have
    /// reported "sRGB piecewise" to all three shaders and rendered wrong colour with no error and no
    /// failing test. Every variant now answers explicitly, so the next one is a compile error.
    pub fn trc_kind(self) -> u32 {
        match self {
            Gamut::Srgb | Gamut::DisplayP3 | Gamut::Rec2020 => 0,
            Gamut::AdobeRgb => 1,
            Gamut::Custom => 2,
            Gamut::DciP3 => 3,
            Gamut::SourceIcc(_) => 4,
        }
    }

    /// Map an ICC-profile description / EXIF colour-space name to a gamut (the source side). `None`
    /// when unrecognised — the caller then assumes sRGB (the safe default for the render pipeline).
    /// DELIBERATE (v0.8.67): "dci"/"p3" descriptions keep mapping to `DisplayP3`, NOT the new
    /// `DciP3` — consumer files/panels labelled "DCI-P3" are near-always P3-D65 content, and
    /// misreading them as DCI-white would tint every neutral. `DciP3` stays destination-only.
    pub fn from_description(desc: &str) -> Option<Gamut> {
        let d = desc.to_ascii_lowercase();
        if d.contains("srgb") {
            Some(Gamut::Srgb)
        } else if d.contains("adobe rgb") || d.contains("adobergb") {
            Some(Gamut::AdobeRgb)
        } else if d.contains("display p3") || d.contains("display-p3") || d.contains("dci") || d.contains("p3") {
            Some(Gamut::DisplayP3)
        } else if d.contains("2020") {
            Some(Gamut::Rec2020)
        } else if d.contains("prophoto") {
            // ProPhoto is wider than Rec.2020; Rec.2020 is the closest target we model. Better than sRGB.
            //
            // v0.8.177 — THIS ARM IS NOW THIRD IN LINE AND DEAD FOR REAL PROPHOTO FILES. A genuine
            // ProPhoto profile is matrix-TRC, so `resolve_source_gamut` renders it faithfully at step
            // (2) and never asks here. The arm STAYS because it is still the best available answer
            // for the one case that cannot reach step (2): a LUT/cLUT-class profile — no colorants to
            // measure — whose description says prophoto. The owner's measurement of what this
            // approximation costs when it DOES run: chroma 26 → 18.7, ~28% desaturated.
            Some(Gamut::Rec2020)
        } else {
            None
        }
    }

    /// Map an embedded ICC profile's own COLORANTS to a modeled source gamut — the v0.8.140 answer
    /// to the question [`from_description`] could only guess at. `None` when the profile is not a
    /// matrix/TRC one (LUT/cLUT-class), is malformed, or when no modeled gamut sits within
    /// [`GAMUT_MATCH_TOL`] of it (a ProPhoto or a true-DCI profile, say — nothing we model is close).
    ///
    /// THE ROUND THIS FIXES: a macOS screen capture embeds the DISPLAY's own profile, whose
    /// description on the tester's machine is literally `"Display"`. `from_description` matched
    /// nothing, the caller fell back to sRGB, and genuine Display-P3 pixels were converted FROM
    /// sRGB — uniformly desaturated in every output mode, silently. The bytes were there the whole
    /// time. Names are now the fallback, not the authority.
    ///
    /// Deliberately NEVER answers `Custom` or `DciP3` — see `SOURCE_GAMUTS` in this module (both
    /// are DESTINATION-only; a source resolved to the DCI white would tint every neutral).
    pub fn from_icc_bytes(icc: &[u8]) -> Option<Gamut> {
        let (g, d) = Gamut::nearest_from_icc_bytes(icc)?;
        (d <= GAMUT_MATCH_TOL).then_some(g)
    }

    /// The nearest modeled source gamut to a profile's colorants AND its distance — the same match
    /// [`from_icc_bytes`] makes, WITHOUT the tolerance gate, so a caller can log how close the miss
    /// was. `None` only when the bytes do not parse as a matrix/TRC profile at all.
    pub fn nearest_from_icc_bytes(icc: &[u8]) -> Option<(Gamut, f32)> {
        Some(nearest_source_gamut(icc_colorants_d65(icc)?))
    }

    /// Linear-RGB → CIE XYZ (D65) for this gamut's primaries. Standard published matrices.
    fn rgb_to_xyz(self) -> [[f32; 3]; 3] {
        match self {
            Gamut::Srgb => [
                [0.412_390_8, 0.357_584_3, 0.180_480_8],
                [0.212_639_0, 0.715_168_7, 0.072_192_3],
                [0.019_330_8, 0.119_194_8, 0.950_532_2],
            ],
            Gamut::DisplayP3 => [
                [0.486_570_9, 0.265_667_7, 0.198_217_3],
                [0.228_974_8, 0.691_738_8, 0.079_286_5],
                [0.000_000_0, 0.045_113_4, 1.043_944_4],
            ],
            Gamut::AdobeRgb => [
                [0.576_669_0, 0.185_558_0, 0.188_229_3],
                [0.297_344_9, 0.627_363_6, 0.075_291_5],
                [0.027_031_4, 0.070_688_8, 0.991_337_8],
            ],
            Gamut::Rec2020 => [
                [0.636_958_0, 0.144_616_9, 0.168_881_0],
                [0.262_700_2, 0.677_998_0, 0.059_301_7],
                [0.000_000_0, 0.028_072_6, 1.060_985_1],
            ],
            // True cinema DCI-P3 (SMPTE RP 431-2): primaries R(0.680,0.320) G(0.265,0.690)
            // B(0.150,0.060) referenced to the NATIVE DCI white (x 0.3140, y 0.3510) — the standard
            // published matrix, whose columns sum to the DCI white's XYZ (0.894587, 1, 0.954416).
            // NO Bradford step, mirroring the architecture: the pipeline's XYZ space is absolute, so a
            // DCI-white display's matrix carries its real white exactly like a `Custom` ICC's colorants
            // are un-adapted to the device white. D65 content therefore encodes slightly non-neutral
            // here — the compensation that physically reproduces D65 on the greenish DCI white
            // (`dcip3_grey_compensates_dci_white` pins the direction; derivation test pins the values).
            Gamut::DciP3 => [
                [0.445_169_8, 0.277_134_4, 0.172_282_7],
                [0.209_491_7, 0.721_595_3, 0.068_913_1],
                [0.000_000_0, 0.047_060_6, 0.907_355_4],
            ],
            // The loaded custom profile's colorants, already chad-adapted to D65 by the ICC parser so
            // they drop into this all-D65 pipeline. Reads the process-global profile once per call.
            Gamut::Custom => custom_matrix_gamma().0,
            // v0.8.177: the FILE's own colorants, adapted to D65 by the identical `icc_colorants_d65`
            // the colorimetry match uses — so the faithful route and the modeled route are reading
            // the same number, and a profile that WOULD have matched cannot get a different matrix
            // by taking a different route. Registry read (one RwLock read per transform, not per
            // pixel — `src_to_dst_matrix` is called once).
            Gamut::SourceIcc(_) => match source_profile(self) {
                Some(p) => p.rgb_to_xyz,
                None => Gamut::Srgb.rgb_to_xyz(), // unreachable: the index came from the registry
            },
        }
    }

}

// ───────────── the FAITHFUL SOURCE route (v0.8.177) — a profile we cannot name, honoured ─────────────
//
// THE DEFECT, measured on the owner's weic2212a ESA pair (2026-08-08, littleCMS as the reference CM):
// the sRGB JPG and the ProPhoto-RGB TIFF are the same image — mean|Δ| 0.7/255 under correct
// management. Falcon rendered the TIFF at mean|Δ| ≈ 7/255 over 72% of its pixels, chroma 26 → 18.7
// (~28% desaturated) with a green/blue midtone lift, because ProPhoto's colorants miss every modeled
// gamut and the NAME fallback mapped "prophoto" → Rec.2020 (the documented approximation, and better
// than sRGB — just not right).
//
// THE FIX IS A CLASS FIX, not a ProPhoto special case: a profile that parses as matrix-TRC carries
// everything the engine's transform needs (colorants + per-channel forward TRC). If we can measure
// it, we can render it — being unable to NAME it is irrelevant. LUT/cLUT-class profiles have no
// colorants and still fall through to the name and then to sRGB, unchanged.

/// A SOURCE profile Falcon renders from its own bytes. Built once per distinct embedded profile and
/// kept in the process registry; `Gamut::SourceIcc(i)` is an index into it.
///
/// The tone curves are kept as a `3 × CUSTOM_LUT_N` u16 FORWARD (device→linear) LUT rather than as
/// the parsed [`ToneCurve`]s, for the same reason the destination side keeps an inverse LUT: the GPU
/// fast tier binds these exact bytes as a texture and the CPU tiers sample the exact same table, so
/// scrub (GPU) and on-stop (CPU) cannot disagree about a curve. Sampling matches [`lut_encode`].
#[derive(Clone, Debug)]
pub struct SourceProfile {
    /// Linear device RGB → CIE XYZ, D65-referenced — [`icc_colorants_d65`]'s answer, unmodified.
    pub rgb_to_xyz: [[f32; 3]; 3],
    /// The profile's OWN description, sanitized + length-bounded (untrusted file text). The only
    /// name a faithful source is ever shown under.
    pub desc: String,
    /// Human summary of the parsed curves for the diagnostics log (`curv[1024]`, `gamma 1.80`, …).
    pub trc_summary: String,
    /// Per-channel `device → linear` FORWARD LUT, `3 × CUSTOM_LUT_N` u16 (rows R,G,B). `Arc` so the
    /// per-transform snapshot and the GPU texture upload are cheap clones.
    lin_lut: Arc<[u16]>,
}

impl SourceProfile {
    /// The per-channel forward LUT (`3 × CUSTOM_LUT_N` u16, device→linear, rows R,G,B) — what the
    /// GPU uploads and what [`Trc::linearize`] samples.
    pub fn linearize_lut(&self) -> &[u16] {
        &self.lin_lut
    }
}

/// How many DISTINCT faithful source profiles one process will hold.
///
/// The registry is append-only (an index must stay valid for the life of a `Gamut` value, and those
/// values live in frame stamps and RAM-cache entries), so it needs a ceiling or a hostile folder of
/// 50 000 files each carrying a slightly different crafted profile would grow it without bound —
/// ~32 KB apiece, which is the difference between a bounded cost and a memory exhaustion. 64 is far
/// past any real library: a photographer's folder carries one or two working-space profiles, and the
/// v0.8.141 survey found 37 profiles INSTALLED on the owner's whole machine. Past the cap a profile
/// simply resolves by the older routes (name, then sRGB) — the pre-v0.8.177 behaviour, never a
/// wrong-but-confident one. `the_registry_cap_degrades_to_the_name` pins it.
pub const SOURCE_PROFILE_CAP: usize = 64;

/// The faithful-source registry: `(fingerprint, profile)` in insertion order, so the position IS the
/// `Gamut::SourceIcc` payload. Append-only; entries are never replaced or removed.
static SOURCE_REGISTRY: RwLock<Vec<(u64, Arc<SourceProfile>)>> = RwLock::new(Vec::new());

/// FNV-1a 64 over the profile bytes, salted with the length. The registry key: identical embedded
/// profiles (a folder of exports from one application) must collapse to ONE entry, or the cap would
/// be spent on one photographer's afternoon. Not a security hash — a collision would mean two
/// profiles sharing a transform, so the byte length is folded in and the fingerprint is only ever
/// consulted for profiles that already reached this far.
fn icc_fingerprint(icc: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (icc.len() as u64);
    for &b in icc {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The registered profile behind a [`Gamut::SourceIcc`], or `None` for any other gamut.
pub fn source_profile(g: Gamut) -> Option<Arc<SourceProfile>> {
    let Gamut::SourceIcc(i) = g else { return None };
    // v0.8.152 (R3-L10) house idiom: a poisoned lock is a report of a panic elsewhere; the data
    // behind it (a `Vec` push and some `Arc` clones) cannot be half-written.
    let reg = SOURCE_REGISTRY.read().unwrap_or_else(|e| e.into_inner());
    reg.get(i as usize).map(|(_, p)| p.clone())
}

/// The faithful source's forward LUT for the GPU fast tier: `(profile index, 3 × CUSTOM_LUT_N u16
/// device→linear)`. The index is the texture cache key — a new source profile means a new texture,
/// exactly as a new [`custom_profile_gen`] does for the destination rows.
pub fn source_linearize_lut(g: Gamut) -> Option<(u16, Arc<[u16]>)> {
    let Gamut::SourceIcc(i) = g else { return None };
    source_profile(g).map(|p| (i, p.lin_lut.clone()))
}

/// Why the faithful route DECLINED a profile it was offered (v0.8.177, G-O4). Carried on
/// [`GamutResolution`] so the one line a miss writes can name WHICH of the causes applies —
/// before this, a cap exhaustion, an unadaptable white and a missing tone curve all produced the
/// same sentence, and a tester reading the log could not tell "your library is past 64 profiles"
/// from "this profile has no curve we can render through".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaithfulRefusal {
    /// The registry is at [`SOURCE_PROFILE_CAP`] and this profile is not already in it.
    RegistryFull,
    /// The profile's native white is neither D65 nor D50 within [`SOURCE_WHITE_TOL`], so there is no
    /// adaptation this crate may apply without an owner-facing white-mapping ruling.
    NonAdaptableWhite,
    /// Colorants read, but no channel carries a TRC tag [`parse_tone_curve`] accepts.
    NoUsableTrc,
    /// Not a matrix profile at all — no readable colorants (the LUT/cLUT class, or malformed bytes).
    /// Never reaches the per-profile miss line, which only fires when the colorants DID parse.
    NotMeasurable,
}

impl FaithfulRefusal {
    /// The stable one-word cause token the log line carries. The tester greps these.
    pub fn token(self) -> &'static str {
        match self {
            FaithfulRefusal::RegistryFull => "cap-full",
            FaithfulRefusal::NonAdaptableWhite => "non-adaptable-white",
            FaithfulRefusal::NoUsableTrc => "no-usable-TRC",
            FaithfulRefusal::NotMeasurable => "not-matrix-TRC",
        }
    }
}

/// Parse `icc` as a matrix-TRC SOURCE and return its registry gamut — route (2) of the resolution
/// chain. `Err` when the profile is not matrix-TRC (no colorants, or no channel whose TRC tag
/// parses), when its native white is one no adaptation here may move, or when the registry is at
/// [`SOURCE_PROFILE_CAP`] — and the [`FaithfulRefusal`] says WHICH.
///
/// THE ENTRY BAR IS DELIBERATELY THE SAME ONE `parse_display_icc` USES on the destination side:
/// colorants that survive [`icc_colorants_d65`]'s `chad`/determinant/magnitude guards, plus at least
/// one of `rTRC`/`gTRC`/`bTRC` that [`parse_tone_curve`] accepts, with green (≈ luminance) as the
/// anchor a missing channel falls back to. A profile that fails either half is not "rendered
/// approximately" here — it is refused, and the chain moves on to the name.
pub fn register_source_profile(icc: &[u8]) -> Result<Gamut, FaithfulRefusal> {
    let fp = icc_fingerprint(icc);
    {
        let reg = SOURCE_REGISTRY.read().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = reg.iter().position(|(k, _)| *k == fp) {
            return Ok(Gamut::SourceIcc(i as u16));
        }
        if reg.len() >= SOURCE_PROFILE_CAP {
            return Err(FaithfulRefusal::RegistryFull); // → the caller falls through to the name, then to sRGB
        }
    }
    let profile = parse_source_icc(icc)?;
    let mut reg = SOURCE_REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    // Re-check under the write lock: two decode workers can reach a brand-new profile at once, and
    // registering it twice would burn two cap slots and hand the same file two different `Gamut`
    // values (which compare UNEQUAL, so a cache keyed on the gamut would thrash).
    if let Some(i) = reg.iter().position(|(k, _)| *k == fp) {
        return Ok(Gamut::SourceIcc(i as u16));
    }
    if reg.len() >= SOURCE_PROFILE_CAP {
        return Err(FaithfulRefusal::RegistryFull);
    }
    let i = reg.len() as u16;
    reg.push((fp, Arc::new(profile)));
    Ok(Gamut::SourceIcc(i))
}

/// Build a [`SourceProfile`] from profile bytes — the parse half of [`register_source_profile`],
/// split out so it runs OUTSIDE the registry write lock (three 4096-entry LUT builds).
fn parse_source_icc(icc: &[u8]) -> Result<SourceProfile, FaithfulRefusal> {
    let m = icc_colorants_d65(icc).ok_or(FaithfulRefusal::NotMeasurable)?;
    // see `SOURCE_WHITE_TOL` — D65 as it stands, D50 Bradford-adapted, anything else refused
    let m = source_colorants_d65(m).ok_or(FaithfulRefusal::NonAdaptableWhite)?;
    let tag_count = icc_tag_count(icc).ok_or(FaithfulRefusal::NotMeasurable)?;
    let find = |sig: &[u8; 4]| icc_find_tag(icc, tag_count, sig);
    let parse_ch = |sig: &[u8; 4]| find(sig).and_then(|(o, s)| parse_tone_curve(icc, o, s));
    let rc = parse_ch(b"rTRC");
    let gc = parse_ch(b"gTRC");
    let bc = parse_ch(b"bTRC");
    // Green ≈ luminance is the anchor; a missing channel falls back to it (then to red/blue). A
    // profile with NO usable curve on any channel is not matrix-TRC for our purposes → refused.
    let gc = gc.or_else(|| rc.clone()).or_else(|| bc.clone()).ok_or(FaithfulRefusal::NoUsableTrc)?;
    let rc = rc.unwrap_or_else(|| gc.clone());
    let bc = bc.unwrap_or_else(|| gc.clone());
    let mut flat = Vec::with_capacity(3 * CUSTOM_LUT_N);
    for c in [&rc, &gc, &bc] {
        flat.extend_from_slice(&forward_lut_for(c));
    }
    let per_channel = flat[..CUSTOM_LUT_N] != flat[CUSTOM_LUT_N..2 * CUSTOM_LUT_N]
        || flat[CUSTOM_LUT_N..2 * CUSTOM_LUT_N] != flat[2 * CUSTOM_LUT_N..];
    Ok(SourceProfile {
        rgb_to_xyz: m,
        desc: bounded_profile_desc(icc_description(icc).as_deref()),
        trc_summary: format!(
            "{}{} -> fwd-lut[{}]",
            if per_channel { "per-ch " } else { "" },
            gc.kind_summary(),
            CUSTOM_LUT_N
        ),
        lin_lut: flat.into(),
    })
}

/// Read a profile's OWN human description — the v2 `desc` (textDescriptionType, ASCII) or the v4
/// `desc` holding an `mluc` (multiLocalizedUnicodeType, UTF-16BE first record).
///
/// v0.8.177 — WHY THIS LIVES HERE AND TAKES NO CALLER STRING. The faithful route shows a file under
/// its profile's name instead of a modeled gamut's, which makes that string load-bearing: it is the
/// only thing distinguishing "ProPhoto RGB" from "Rec. 2020" on the panel. The v0.8.141 (R7)
/// discipline says only a name the PROFILE ITSELF carries may be printed as the profile's — and a
/// caller-supplied description can legitimately be the CONTAINER's (a PNG `sRGB` chunk standing in
/// for an unreadable `desc`, an `nclx`/CICP code). Reading it from the same bytes the colorants and
/// the curves came from makes that impossible by construction rather than by convention.
///
/// Bounds every read through this module's shared [`icc_tag_count`] / [`icc_find_tag`] discipline,
/// and — like [`parse_tone_curve`] since this round — against the TAG's declared extent, not the
/// buffer's. `None` for a profile with no readable description.
///
/// DECLARED DUPLICATION (named so a reader does not think it is an accident): `falcon-decode` has
/// its own `icc_description` serving the panel/log name for ALL profiles, matrix-TRC or not. The two
/// are not merged in this round because unifying them would move a parser under six existing
/// description tests for no benefit to this fix; the honest label needs an authority inside the
/// crate that owns the transform, and that is what this is.
pub fn icc_description(icc: &[u8]) -> Option<String> {
    let tag_count = icc_tag_count(icc)?;
    let (off, size) = icc_find_tag(icc, tag_count, b"desc")?;
    let end = off.checked_add(size)?; // `icc_find_tag` already proved `end <= icc.len()`
    if size < 12 {
        return None;
    }
    let be32 = |o: usize| u32::from_be_bytes([icc[o], icc[o + 1], icc[o + 2], icc[o + 3]]) as usize;
    let text = match &icc[off..off + 4] {
        // v2 textDescriptionType: ['desc'][reserved 4][ASCII count 4][NUL-terminated ASCII…]
        b"desc" => {
            let count = be32(off + 8);
            let s = off + 12;
            let e = s.checked_add(count)?.min(end);
            if s >= e {
                return None;
            }
            icc[s..e].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect::<String>()
        }
        // v4 multiLocalizedUnicodeType: [...][rec count 4][rec size 4][lang2 country2 len4 off4]…
        b"mluc" => {
            if be32(off + 8) == 0 || off + 28 > end {
                return None;
            }
            let r = off + 16; // first record
            let len = be32(r + 4);
            let str_off = off.checked_add(be32(r + 8))?; // offset is relative to the tag start
            let e = str_off.checked_add(len)?.min(end);
            if str_off >= e || str_off < off {
                return None;
            }
            let u16s: Vec<u16> =
                icc[str_off..e].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u16s)
        }
        _ => return None,
    };
    let t = text.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// How far a faithful source's native white may sit from a white this crate knows how to adapt FROM
/// (D65, or D50), as a CIE xy chromaticity distance.
///
/// **WHY A SOURCE'S WHITE IS GATED AT ALL, and why this is a colorimetric rule and not a special
/// case.** Every gamut this pipeline will resolve a SOURCE to is D65-native — that is not an
/// accident of the four modeled ones, it is the reason `SOURCE_GAMUTS` deliberately excludes
/// `DciP3` (see its doc: "resolving a source TO the DCI white would tint every neutral", the
/// v0.8.67 ruling). The XYZ interchange space is ABSOLUTE, so a source whose white is NOT D65
/// renders its neutrals as a coloured cast on a D65 destination — physically correct, and wrong for
/// every photograph anyone has ever taken. Honouring such a profile therefore requires a
/// white-MAPPING policy, and for an ARBITRARY white that is an owner-facing colour ruling. So an
/// arbitrary white is refused, the file falls through to exactly the routes it used before this
/// round — including the deliberate `DCI-P3 → Display P3` name mapping, which this guard is what
/// preserves (`colorimetry_never_loses_a_name_the_old_path_knew` pins it).
///
/// **D50 IS NOT ARBITRARY — OWNER RULING, 2026-08-09.** The v0.8.177 text of this block claimed the
/// gate "costs the faithful route NOTHING in practice", on the grounds that a real working-space
/// profile is ICC v2 with no `chad` and is therefore already Bradford-adapted to D65 by
/// [`icc_colorants_d65`]. Skeptic G refuted it: ColorMatchRGB is the counterexample, and it is not
/// alone — ColorMatchRGB, ECI RGB v2 and the v4 spelling of ProPhoto are ordinary D50-NATIVE working
/// spaces, and the v4 ones carry a `chad` that RECOVERS that D50 white (0.044 from D65), so they
/// were refused while their own chad-less v2 twins passed. Two spellings of one colour space cannot
/// have two answers. The ruling: a D50 device white is adapted EXACTLY like the chad-less case —
/// the same [`BRADFORD_D50_TO_D65`] multiplication, reused rather than forked, which is what makes
/// the two spellings land on the same colorants. What stays refused is the white that is neither:
/// true-DCI's ~6300 K (0.022 from D65 and 0.033 from D50 — outside BOTH), where the cast is real and
/// the mapping policy is genuinely unresolved.
///
/// 0.010 sits ~2× past a calibrated panel's spread and ~2× inside the DCI white's offset.
/// **A HAIR MARGIN, ADMITTED (v0.8.177, G-O1):** the owner's own Dell HDR profile measures **0.00888**
/// against this 0.010 — it passes with 11% of head-room, not with comfort. A panel profiled a little
/// further off D65 than that one is refused the faithful route and falls back to its name, silently
/// as far as this constant is concerned (the log's per-profile miss line is where it becomes
/// visible, now carrying `non-adaptable-white`). Recorded here so the next reader knows the number
/// is tight and measured, not chosen with slack to spare.
const SOURCE_WHITE_TOL: f32 = 0.010;

/// The colorants a faithful source renders through, D65-referenced — or `None` when its native white
/// is one this crate may not move (see [`SOURCE_WHITE_TOL`]).
///
/// The columns of a colorant matrix are the colorants and they sum to the white point, so the white
/// is read from the matrix itself — no `wtpt` tag is consulted (an untrusted profile's `wtpt` can
/// disagree with its own colorants; the colorants are what render). D65 comes from the crate's own
/// sRGB matrix and D50 from [`D50_XYZ`], the PCS illuminant this crate already serializes and
/// un-adapts against, rather than from literals, so neither can drift.
///
/// The D50 arm applies [`BRADFORD_D50_TO_D65`] — THE SAME multiplication [`icc_colorants_d65`]
/// performs for a chad-LESS v2 profile, reused rather than reimplemented, which is precisely what
/// makes a chad-carrying D50 profile land on its chad-less twin's colorants
/// (`a_d50_device_white_is_adapted_exactly_like_its_chad_less_twin` pins the pair).
///
/// ONE CONSEQUENCE, STATED RATHER THAN DISCOVERED: this adaptation happens INSIDE route (2), after
/// route (1) has already measured its τ distance on the UNadapted colorants. So a D50-native profile
/// whose adapted primaries would have landed within τ of a modeled gamut takes the faithful route
/// instead of being named as that gamut. It renders correctly either way — through its own
/// colorants and curves — it simply does not collapse onto the modeled entry, and `src == dst`
/// cannot fire for it. Route (1) is deliberately left byte-identical to what it was, because
/// re-measuring τ after an adaptation would change the answer for files this round is not about.
fn source_colorants_d65(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let xy = |w: [f32; 3]| {
        let s = w[0] + w[1] + w[2];
        (s.abs() >= 1e-6).then(|| (w[0] / s, w[1] / s))
    };
    let white = |m: [[f32; 3]; 3]| [m[0][0] + m[0][1] + m[0][2], m[1][0] + m[1][1] + m[1][2], m[2][0] + m[2][1] + m[2][2]];
    let (x, y) = xy(white(m))?;
    let within = |target: [f32; 3]| {
        xy(target).is_some_and(|(tx, ty)| ((x - tx).powi(2) + (y - ty).powi(2)).sqrt() <= SOURCE_WHITE_TOL)
    };
    if within(white(Gamut::Srgb.rgb_to_xyz())) {
        Some(m) // already D65-referenced — the chad-less v2 case arrives here too
    } else if within(D50_XYZ) {
        Some(mat3_mul(BRADFORD_D50_TO_D65, m)) // the owner's 08-09 ruling, by the crate's own adaptation
    } else {
        None
    }
}

/// The longest a faithful source's own description may be when it stands in for a gamut name.
/// The panel row and the log line are both bounded surfaces; an ICC description is free text from an
/// untrusted file and real ones already run long ("Dell S2725QS Native, D6500, 2.2, MHC2 calibrated
/// 2026-01-14"). 40 chars is generous for the real names in this class ("ProPhoto RGB",
/// "LStar-RGB-v2.icc") and short enough that no row is pushed off screen.
const SOURCE_DESC_MAX: usize = 40;

/// The INVISIBLE formatting characters — zero-width and directional marks. They render as nothing,
/// so unlike a control character they cannot be neutralised by turning them into a space: `Adobe`
/// ZWSP `RGB` would become the visible string "Adobe RGB", a name this profile does not carry.
/// They are DROPPED, which leaves the visible text exactly as it reads (v0.8.177, H-Y1).
///
/// U+200E/U+200F left-to-right and right-to-left mark, U+061C Arabic letter mark — one-character
/// direction switches that reorder the rest of the row without any of the U+202x override bracketing
/// the sibling arm already catches. U+200B zero-width space, U+FEFF zero-width no-break space (a BOM
/// that survived a decode), U+2060 word joiner — invisible, but each one splits or fuses what a
/// reader sees as one word, and a name is compared by eye against the file's.
fn is_invisible_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2060}' | '\u{FEFF}')
}

/// The generic COMBINING-MARK blocks: a mark here composes onto the character BEFORE it. Dep-free by
/// construction — these are the five contiguous ranges Unicode reserves for script-independent
/// combining marks, not the whole `Mn`/`Me` category (which needs a table this crate will not carry
/// for a label). That is enough for the case this guards: the stacking marks a hostile name uses to
/// smear across a row, of which only the ones left DANGLING by truncation matter here.
fn is_combining_mark(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'   // combining diacritical marks
        | '\u{1AB0}'..='\u{1AFF}' // …extended
        | '\u{1DC0}'..='\u{1DFF}' // …supplement
        | '\u{20D0}'..='\u{20FF}' // …for symbols
        | '\u{FE20}'..='\u{FE2F}' // half marks
    )
}

/// A profile description reduced to ONE bounded, control-character-free line — the label a faithful
/// source is shown under. An empty or absent description becomes a stated stand-in rather than an
/// empty string, so the panel never shows a blank where a colour space belongs.
fn bounded_profile_desc(desc: Option<&str>) -> String {
    let raw = desc.unwrap_or("").trim();
    // Control characters (a newline would forge a second log line; a bidi override would reorder the
    // whole panel row) become spaces, then runs of whitespace collapse. The INVISIBLE formatting
    // characters are dropped instead — see `is_invisible_format` for why a space is the wrong
    // neutralisation for a character that occupies no space.
    let cleaned: String = raw
        .chars()
        .filter(|c| !is_invisible_format(*c))
        .map(|c| if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { ' ' } else { c })
        .collect();
    let one_line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return "embedded profile".to_string();
    }
    if one_line.chars().count() <= SOURCE_DESC_MAX {
        return one_line;
    }
    // v0.8.177 (H-Y2): a cut at exactly `SOURCE_DESC_MAX` can land between a base character and the
    // marks that compose onto it, leaving marks with nothing to sit on — which then stack onto the
    // '…' we are about to append, decorating a character the profile never wrote. Drop any trailing
    // marks first; the base they belonged to went with them or stays undecorated, and the ellipsis
    // says the same thing either way.
    let mut out: String = one_line.chars().take(SOURCE_DESC_MAX).collect();
    while out.chars().next_back().is_some_and(is_combining_mark) {
        out.pop();
    }
    out.push('…');
    out
}

/// Build the `device → linear` FORWARD LUT (`CUSTOM_LUT_N` u16, uniform in DEVICE value) for a
/// parsed tone curve — the source-side twin of [`inverse_lut_for`]. Linear light is clamped to
/// `0..1`: the pipeline's XYZ stage and every destination encode clamp there anyway, and a crafted
/// `para` curve that returns 40.0 must not be allowed to scale a whole image.
fn forward_lut_for(curve: &ToneCurve) -> Vec<u16> {
    (0..CUSTOM_LUT_N)
        .map(|k| {
            let d = k as f32 / (CUSTOM_LUT_N - 1) as f32;
            let l = curve.eval(d);
            let l = if l.is_finite() { l } else { 0.0 };
            (l.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
        })
        .collect()
}

// ───────────────── source-gamut resolution BY COLORIMETRY (v0.8.140, THE COLOR ROUND) ─────────────
// The defect: `shot_source_gamut` asked `Gamut::from_description` — a NAME-STRING match — what a
// file's pixels were encoded in, and fell back to sRGB when the name was unfamiliar. Real-world
// profiles are not named after their colour space: a macOS screen capture carries the DISPLAY's own
// profile, described "Display" on the tester's machine, so P3 pixels rendered as sRGB in every
// output mode. The bytes were embedded in the file the whole time. This section resolves from them.

/// The gamuts a SOURCE file can be resolved to. `Custom` is excluded because it is a DESTINATION
/// slot — one process-global monitor profile ([`set_custom_profile`]), which has nothing to do with
/// what any given file was encoded in — and `DciP3` because of the v0.8.67 ruling that files and
/// panels labelled "DCI-P3" are near-always P3-D65 content (see [`Gamut::from_description`]);
/// resolving a source TO the DCI white would tint every neutral. A genuine true-DCI profile
/// therefore misses every candidate here and falls through to the name, which answers `DisplayP3`
/// exactly as it did before this round.
///
/// v0.8.141 (R14) — THE MARGIN, CORRECTED. The v0.8.140 text of this comment (and its commit
/// message) said the true-DCI miss was "~0.14, twice the nearest-pair distance". Both halves were
/// wrong. `dci_p3_misses_by_its_measured_margin` measures it: the nearest candidate is **sRGB**
/// (not Display P3, whose big blue-Z difference dominates) at **0.08045**, which is **1.16×** the
/// nearest modeled pair (0.06965) — a comfortable miss against τ = 0.01, but nowhere near "twice"
/// and not the gamut the old sentence implied.
///
/// v0.8.141 (R12): `pub(crate)`. Nothing outside this crate consumed it — the source-side answer is
/// [`Gamut::from_icc_bytes`] / [`resolve_source_gamut`], and a caller that wanted the raw candidate
/// list would be re-implementing the match.
pub(crate) const SOURCE_GAMUTS: [Gamut; 4] =
    [Gamut::Srgb, Gamut::DisplayP3, Gamut::AdobeRgb, Gamut::Rec2020];

/// τ — how close a profile's colorant matrix must sit to a modeled gamut's to BE that gamut,
/// as a max-abs element difference on the D65-referenced linear-RGB → XYZ matrix. (The columns are
/// the colorants and they sum to the white point, so this one metric covers primaries AND white —
/// no separate `wtpt` test is needed.)
///
/// DERIVED FROM MEASUREMENT, both sides pinned by `tau_is_discriminating` below:
/// - the closest pair of modeled gamuts (Adobe RGB ↔ Rec.2020) sits **0.06965** apart, so τ = 0.01
///   gives k = 6.96 — every modeled pair is more than 5 τ apart and no profile can be ambiguous;
/// - the s15Fixed16 round-trip through a real ICC (serialize → chad into the D50 PCS → parse → un-adapt
///   → compare) costs at most **1.34e-5**, i.e. τ = 744× the encoding noise, so an exactly-standard
///   profile can never miss its own gamut.
///
/// Measured against the real profiles shipped with Windows, τ = 0.01 has room to spare in the
/// direction that matters: `sRGB Color Space Profile.icm` lands **3.3e-4** from modeled sRGB and
/// `AdobeRGB1998.icc` **5.9e-4** from modeled Adobe RGB (both ICC v2, no `chad`, so the Bradford
/// adaptation runs), each with its nearest WRONG gamut ~0.07-0.09 away — 100-200× the hit distance.
///
/// WHAT τ DELIBERATELY DOES NOT CATCH, measured on the same machine: a profile MEASURED off a real
/// panel. `Dell_S2725QS_sRGB.icm` sits 0.0334 from sRGB, `Dell_S2725QS_Native.icm` 0.0639 from
/// Display P3, `CalibratedDisplayProfile-2.icc` 0.0123 from sRGB — and normalising the white point
/// away barely moves those numbers (0.0115 / 0.0640 / 0.0126), so it is the PRIMARIES that differ,
/// not the white. No τ that keeps k ≥ 5 can reach them, and snapping them to a nearest neighbour
/// 0.064 away when the runner-up is 0.068 would be a coin toss. Such a profile is what the spec's
/// Custom-from-profile source path exists for; until that lands, they miss here, fall through to
/// the name, and say so in the log with their distance (see `GamutResolution::why`).
///
/// Between those two bounds τ is a JUDGEMENT: 0.01 is deliberately loose enough to absorb a real
/// display profile's manufacturing spread (a measured panel targeting P3 is not the published
/// matrix) and still nowhere near the 0.07 that would let one gamut be mistaken for another.
pub const GAMUT_MATCH_TOL: f32 = 0.01;

/// Distance between two colorant matrices: the largest absolute element difference. Simple,
/// symmetric, and in the same units as the matrix entries, so a number in a log line is readable
/// against [`GAMUT_MATCH_TOL`] without any further scaling.
///
/// v0.8.141 (R12): `pub(crate)` — the distance a caller actually wants is the one attached to an
/// answer ([`Gamut::nearest_from_icc_bytes`], [`GamutResolution::nearest`]), not a bare metric.
pub(crate) fn colorant_distance(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> f32 {
    let mut worst = 0.0f32;
    for r in 0..3 {
        for c in 0..3 {
            worst = worst.max((a[r][c] - b[r][c]).abs());
        }
    }
    worst
}

/// The nearest of [`SOURCE_GAMUTS`] to a D65-referenced colorant matrix, with its distance. Total —
/// there is always a nearest; whether it is near ENOUGH is [`GAMUT_MATCH_TOL`]'s question.
///
/// v0.8.141 (R12): `pub(crate)` — [`Gamut::nearest_from_icc_bytes`] is the public spelling, and it
/// takes the BYTES, so no caller outside this crate has to know how a colorant matrix is obtained.
pub(crate) fn nearest_source_gamut(m: [[f32; 3]; 3]) -> (Gamut, f32) {
    let mut best = (SOURCE_GAMUTS[0], f32::INFINITY);
    for g in SOURCE_GAMUTS {
        let d = colorant_distance(m, g.rgb_to_xyz());
        if d < best.1 {
            best = (g, d);
        }
    }
    best
}

/// Which route decided a source gamut — the provenance one honest log line needs (C5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GamutRoute {
    /// The file's embedded profile COLORANTS matched a modeled gamut within [`GAMUT_MATCH_TOL`].
    /// The authoritative answer: it is measured from the bytes the file actually carries.
    Colorimetry,
    /// v0.8.177 — no modeled gamut was within τ, but the profile IS matrix-TRC, so the file is
    /// rendered through its own colorants and its own per-channel curves. Also measured from the
    /// file's bytes; the difference from [`Colorimetry`](GamutRoute::Colorimetry) is that the answer
    /// has no name we model, not that it is less certain.
    Faithful,
    /// The colorants did not answer (no profile, not matrix/TRC, malformed, or the faithful registry
    /// was full) and the profile DESCRIPTION named a gamut instead.
    Description,
    /// Nothing answered: sRGB, the render pipeline's safe default.
    Fallback,
}

impl GamutRoute {
    /// The one-word route token the log line carries (C5 / U15 honesty). Stable strings — the
    /// tester greps them.
    pub fn token(self) -> &'static str {
        match self {
            GamutRoute::Colorimetry => "modeled-colorants",
            GamutRoute::Faithful => "faithful",
            GamutRoute::Description => "name-fallback",
            GamutRoute::Fallback => "default",
        }
    }
}

/// [`resolve_source_gamut`]'s full answer: the gamut, and enough provenance to say in one line why.
#[derive(Clone, Copy, Debug)]
pub struct GamutResolution {
    /// The gamut to convert the file's pixels FROM.
    pub gamut: Gamut,
    /// Which route decided it.
    pub route: GamutRoute,
    /// The nearest modeled gamut and its colorant distance — `Some` exactly when the profile bytes
    /// PARSED as matrix/TRC, so `None` also reads as "no bytes, or bytes we cannot measure".
    pub nearest: Option<(Gamut, f32)>,
    /// The file carried profile bytes that would not parse as matrix/TRC (a LUT/cLUT-class profile,
    /// or a malformed one). Distinguishes "no profile" from "a profile we cannot measure".
    pub unreadable_profile: bool,
    /// v0.8.177 (G-O4) — why the FAITHFUL route declined, when it was offered this profile and did.
    /// `None` means it was never reached (the colorimetry route answered first, or there were no
    /// colorants to offer it) or that it succeeded.
    pub faithful_refusal: Option<FaithfulRefusal>,
}

impl GamutResolution {
    /// One short phrase naming the route AND, when the bytes did not decide, why they did not.
    ///
    /// v0.8.177 (G-O4): the "measured but not placed" arms name WHICH refusal sent the file to its
    /// name — cap-full, an unadaptable white, or no usable TRC — rather than one sentence covering
    /// all three. Stable strings; the tester greps them.
    pub fn why(&self) -> &'static str {
        let no_bytes = self.nearest.is_none() && !self.unreadable_profile;
        match (self.route, self.unreadable_profile, no_bytes) {
            (GamutRoute::Colorimetry, _, _) => "colorimetry",
            (GamutRoute::Faithful, _, _) => "the profile's own colorants and curves",
            (GamutRoute::Description, true, _) => "name — the profile is not matrix/TRC",
            (GamutRoute::Description, false, true) => "name — no embedded profile",
            (GamutRoute::Description, false, false) => match self.faithful_refusal {
                Some(FaithfulRefusal::RegistryFull) => "name — measured, unplaceable, faithful registry full",
                Some(FaithfulRefusal::NonAdaptableWhite) => "name — measured, white neither D65 nor D50",
                Some(FaithfulRefusal::NoUsableTrc) => "name — measured, no usable TRC to render through",
                _ => "name — no modeled gamut within tolerance",
            },
            (GamutRoute::Fallback, true, _) => "sRGB default — profile not matrix/TRC, name unrecognised",
            (GamutRoute::Fallback, false, true) => "sRGB default — no profile and no usable name",
            (GamutRoute::Fallback, false, false) => match self.faithful_refusal {
                Some(FaithfulRefusal::RegistryFull) => "sRGB default — faithful registry full, name unrecognised",
                Some(FaithfulRefusal::NonAdaptableWhite) => {
                    "sRGB default — white neither D65 nor D50, name unrecognised"
                }
                Some(FaithfulRefusal::NoUsableTrc) => "sRGB default — no usable TRC, name unrecognised",
                _ => "sRGB default — no modeled gamut within tolerance, name unrecognised",
            },
        }
    }
}

/// THE SOURCE-SIDE RULE, in one place: a file's own profile BYTES decide what its pixels are
/// encoded in; its DESCRIPTION is the fallback for what the bytes cannot supply; sRGB is the floor.
///
/// `icc` is the raw embedded profile when the file has one (PNG `iCCP`, JPEG APP2, TIFF 34675, a
/// HEIF `colr` box of type `prof`/`rICC`, a WebP `ICCP`, a JXL original ICC). `desc` is the
/// human description — read out of those same bytes, or SYNTHESISED from an enumerated tag that
/// carries no bytes at all (a HEIF `nclx` box or a JXL CICP triple name their primaries by code,
/// so those doors legitimately pass `icc = None` with a name).
///
/// **The strictly-better invariant** (pinned by `colorimetry_never_loses_a_name_the_old_path_knew`):
/// every input the old name-only path resolved to a named gamut still resolves to that same gamut,
/// because a colorimetric MISS falls through to the identical [`Gamut::from_description`] call.
/// The one designed exception is a MISLABELED profile — a name saying one space over colorants
/// saying another — where the bytes now win. That is the point.
///
/// **v0.8.177 — THE CHAIN GAINS A SECOND BYTES-FIRST STEP, ahead of the name:**
///
/// 1. the colorants match a modeled gamut within [`GAMUT_MATCH_TOL`] → that gamut (unchanged, and
///    still FIRST — which is what guarantees every sRGB / P3 / Adobe RGB / Rec.2020-tagged file in
///    the world resolves exactly as it did before this round, and why the faithful route can never
///    steal a file the colorimetry route can place);
/// 2. NEW — the profile parses as matrix-TRC → render it FAITHFULLY from its own colorants and
///    curves ([`register_source_profile`]);
/// 3. the DESCRIPTION names a gamut ([`Gamut::from_description`]) — now third. Its "prophoto" arm is
///    dead in practice for real ProPhoto files (they reach step 2) and stays alive for the
///    LUT/cLUT-class profile whose name says prophoto and whose bytes cannot be measured at all;
/// 4. sRGB, the floor.
///
/// Steps 3 and 4 are byte-identical to what they were; a file only reaches them if it could not be
/// measured, which is exactly when a name is the best evidence available.
pub fn resolve_source_gamut(icc: Option<&[u8]>, desc: Option<&str>) -> GamutResolution {
    let colorants = icc.and_then(icc_colorants_d65);
    let nearest = colorants.map(nearest_source_gamut);
    let unreadable_profile = icc.is_some() && colorants.is_none();
    let mut faithful_refusal = None;
    if let Some((g, d)) = nearest {
        if d <= GAMUT_MATCH_TOL {
            return GamutResolution {
                gamut: g,
                route: GamutRoute::Colorimetry,
                nearest,
                unreadable_profile,
                faithful_refusal,
            };
        }
    }
    // (2) Measurable but unplaceable → honour it. Guarded by `nearest.is_some()`, i.e. by the
    // colorants having parsed, so this can only ever be reached for a profile step (1) already
    // measured and could not place — never as an alternative FIRST opinion.
    if nearest.is_some() {
        match icc.map(register_source_profile) {
            Some(Ok(g)) => {
                return GamutResolution {
                    gamut: g,
                    route: GamutRoute::Faithful,
                    nearest,
                    unreadable_profile,
                    faithful_refusal,
                }
            }
            // The refusal travels WITH the answer: the log line that reports this file's miss is the
            // only place a tester learns which of the three causes sent it back to its name.
            Some(Err(e)) => faithful_refusal = Some(e),
            None => {}
        }
    }
    match desc.and_then(Gamut::from_description) {
        Some(g) => {
            GamutResolution { gamut: g, route: GamutRoute::Description, nearest, unreadable_profile, faithful_refusal }
        }
        None => GamutResolution {
            gamut: Gamut::Srgb,
            route: GamutRoute::Fallback,
            nearest,
            unreadable_profile,
            faithful_refusal,
        },
    }
}

/// Encoded sRGB value (0..1) → linear light (the sRGB piecewise EOTF). Shared by sRGB / Display P3 /
/// Rec.2020 (their primaries are what matter for gamut; the small SDR TRC difference is sub-perceptual).
#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light → encoded sRGB value (0..1). Inverse of [`srgb_to_linear`].
#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// The Adobe RGB (1998) transfer gamma — exactly 563/256 per the spec (deliberately NOT 2.2). The
/// GPU YUV shader (falcon-gpu `YUV_WGSL`) hardcodes the same literal in its self-contained WGSL
/// source at 2 sites — keep them in lock-step with this constant.
pub const ADOBE_GAMMA: f32 = 2.199_218_75; // 563/256 — the Adobe RGB (1998) transfer gamma

/// The DCI-P3 (cinema) transfer gamma — a pure 2.6 per SMPTE RP 431-2 (v0.8.67). The GPU fast-tier
/// shaders hardcode the same literal in the shared `rot_uv_cm_core!` WGSL (falcon-gpu — 2 sites,
/// `linearize` + `encode`, kind 3) — keep them in lock-step with this constant.
pub const DCI_GAMMA: f32 = 2.6;

/// Inverse-tone-curve LUT resolution for a loaded custom display profile: the `linear → device` encode
/// LUT has this many samples, uniform in linear light. The GPU binds a `CUSTOM_LUT_N × 3` (R/G/B rows)
/// texture built from the SAME data, so CPU and GPU encode the identical curve. 4096 matches littleCMS's
/// reverse-LUT resolution and reproduces the reference to ±0 codes at the shadow probes (verified).
pub const CUSTOM_LUT_N: usize = 4096;

/// A per-transform tone-curve descriptor. Built ONCE per [`transform_rgba`]/[`transform_rgb`] call and
/// carried by value into the per-pixel loop. `Srgb` selects the piecewise sRGB curve; `Gamma` a pure
/// gamma; `Lut` the loaded Custom profile's faithful per-channel inverse-tone-curve LUT (a borrow of the
/// per-transform snapshot, so the profile's `RwLock` is read once, not once per pixel).
#[derive(Clone, Copy)]
enum Trc<'a> {
    Srgb,
    Gamma(f32),
    /// Custom destination: `flat` is `3 × CUSTOM_LUT_N` u16 (linear→device, rows R,G,B). `encode`
    /// samples the per-channel LUT; `linearize` is never used (Custom is DESTINATION-only) but falls
    /// back to `gamma` for completeness.
    Lut { flat: &'a [u16], gamma: f32 },
    /// v0.8.177 — faithful SOURCE: `flat` is `3 × CUSTOM_LUT_N` u16 (device→linear, rows R,G,B), the
    /// exact bytes the GPU binds as rows 3..5 of `lut_tex`. `linearize` samples the per-channel
    /// curve; `encode` is never used (a source profile is never a destination) and falls back to the
    /// sRGB curve for completeness, mirroring `Lut`'s unused-direction arm.
    SrcLut { flat: &'a [u16] },
}

impl<'a> Trc<'a> {
    /// Encoded device value → linear light for INPUT channel `ch` (0=R,1=G,2=B). `ch` matters only
    /// for the `SrcLut` variant (per-channel curves); the analytic variants ignore it.
    #[inline]
    fn linearize(self, ch: usize, c: f32) -> f32 {
        match self {
            Trc::Srgb => srgb_to_linear(c),
            Trc::Gamma(g) => c.max(0.0).powf(g),
            Trc::Lut { gamma, .. } => c.max(0.0).powf(gamma),
            Trc::SrcLut { flat } => lut_encode(&flat[ch * CUSTOM_LUT_N..(ch + 1) * CUSTOM_LUT_N], c),
        }
    }
    /// Linear light → encoded device value for output channel `ch` (0=R,1=G,2=B). `ch` matters only
    /// for the `Lut` variant (per-channel curves); the analytic variants ignore it.
    #[inline]
    fn encode(self, ch: usize, c: f32) -> f32 {
        match self {
            Trc::Srgb => linear_to_srgb(c.clamp(0.0, 1.0)),
            Trc::Gamma(g) => c.clamp(0.0, 1.0).powf(1.0 / g),
            Trc::Lut { flat, .. } => lut_encode(&flat[ch * CUSTOM_LUT_N..(ch + 1) * CUSTOM_LUT_N], c),
            Trc::SrcLut { .. } => linear_to_srgb(c.clamp(0.0, 1.0)),
        }
    }
}

/// Sample a `CUSTOM_LUT_N`-entry u16 LUT (uniform in its INPUT domain, values `v/65535`) at `x ∈
/// [0,1]` with linear interpolation — the CPU twin of the GPU shader's `textureLoad`+`mix` fetch, so
/// the two stay curve-identical.
///
/// v0.8.177: ONE sampler for BOTH directions, and the NAME IS HISTORICAL — it was written when the
/// destination's `linear → device` inverse LUT was its only caller. The faithful source's `device →
/// linear` FORWARD LUT is the same table shape sampled the same way, so it calls this rather than
/// carrying a second copy of an interpolation the GPU mirrors once. Deliberately not renamed: the
/// v0.8.47 destination tests call it by this name and this round's invariant is that every existing
/// colour test stays green untouched.
#[inline]
fn lut_encode(lut: &[u16], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let pos = x * (CUSTOM_LUT_N - 1) as f32;
    let i0 = pos as usize; // x ∈ [0,1] ⇒ i0 ∈ [0, CUSTOM_LUT_N-1]
    let i1 = (i0 + 1).min(CUSTOM_LUT_N - 1);
    let frac = pos - i0 as f32;
    let v0 = lut[i0] as f32;
    let v1 = lut[i1] as f32;
    (v0 + (v1 - v0) * frac) / 65535.0
}

/// Row-major 3×3 multiply: `(a·b)[i][j] = Σ_k a[i][k]·b[k][j]` (vectors are columns; `M·v` dots each
/// row of `M` with `v`). Shared workspace-wide — falcon-decode imports this (v0.7.4 C8 dedup).
pub fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// Row-major 3×3 inverse (adjugate / determinant); identity when singular. Shared workspace-wide —
/// falcon-decode imports this (v0.7.4 C8 dedup).
pub fn mat3_inv(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let det = mat3_det(m);
    // 1e-12 is a NUMERIC-STABILITY floor for the 1/det division only — deliberately tighter than
    // parse_display_icc's 1e-9 profile-sanity threshold, which rejects a near-degenerate colorant
    // matrix as a bad PROFILE long before inversion would wobble.
    if det.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]; // singular → identity (never expected)
    }
    let inv = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ]
}

/// The combined linear-light 3×3 taking `src` RGB → `dst` RGB (i.e. `inv(dst→XYZ) · (src→XYZ)`).
/// Public so the GPU fast-tier transform (main.rs) can upload the SAME matrix the CPU tiers use,
/// keeping the scrub (GPU) and on-stop (CPU) colours identical (no seam / no pop).
pub fn src_to_dst_matrix(src: Gamut, dst: Gamut) -> [[f32; 3]; 3] {
    mat3_mul(mat3_inv(dst.rgb_to_xyz()), src.rgb_to_xyz())
}

#[inline]
fn apply_pixel(m: &[[f32; 3]; 3], st: Trc, dt: Trc, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let lr = st.linearize(0, r as f32 / 255.0);
    let lg = st.linearize(1, g as f32 / 255.0);
    let lb = st.linearize(2, b as f32 / 255.0);
    let or = dt.encode(0, m[0][0] * lr + m[0][1] * lg + m[0][2] * lb);
    let og = dt.encode(1, m[1][0] * lr + m[1][1] * lg + m[1][2] * lb);
    let ob = dt.encode(2, m[2][0] * lr + m[2][1] * lg + m[2][2] * lb);
    (
        (or * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
        (og * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
        (ob * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
    )
}

/// The active custom inverse-LUT snapshot `(flat 3×CUSTOM_LUT_N u16, effective gamma)` if a profile is
/// installed — read ONCE per transform (Arc-cloned, no data copy) so the per-pixel loop borrows a stable
/// slice without re-locking. `None` (→ analytic gamma-2.2 fallback) when no profile is loaded.
fn custom_enc_lut_snapshot() -> Option<(Arc<[u16]>, f32)> {
    // v0.8.152 (R3-L10): the house idiom, `unwrap_or_else(|e| e.into_inner())` — see
    // `falcon-decode`'s `DECODE_NOTES`. See [`CUSTOM`] for why the four sites moved together.
    let g = CUSTOM.read().unwrap_or_else(|e| e.into_inner());
    g.as_ref().map(|p| (p.enc_lut.clone(), p.gamma))
}

/// Build the DESTINATION [`Trc`] for a gamut. `dst` may be `Custom`, in which case it borrows the
/// per-transform inverse-LUT `snapshot`.
#[inline]
fn trc_for<'a>(g: Gamut, snapshot: Option<&'a (Arc<[u16]>, f32)>) -> Trc<'a> {
    match g {
        Gamut::AdobeRgb => Trc::Gamma(ADOBE_GAMMA),
        Gamut::DciP3 => Trc::Gamma(DCI_GAMMA),
        Gamut::Custom => match snapshot {
            Some((lut, gamma)) => Trc::Lut { flat: lut, gamma: *gamma },
            None => Trc::Gamma(2.2), // no profile loaded → sRGB-equivalent fallback (matches Default)
        },
        // A source profile is never a destination (`from_i32` cannot emit one). If one somehow
        // arrived here, sRGB is the same floor every unknown destination has always had.
        // Spelled out rather than left to a `_` arm for the reason `trc_kind` lost its: a catch-all
        // here is how a future variant silently acquires the sRGB curve.
        Gamut::SourceIcc(_) => Trc::Srgb,
        Gamut::Srgb | Gamut::DisplayP3 | Gamut::Rec2020 => Trc::Srgb,
    }
}

/// Build the SOURCE [`Trc`] for a gamut (v0.8.177). Every modeled gamut is analytic and answers
/// exactly as [`trc_for`] does; a faithful source borrows the per-transform FORWARD LUT `snapshot`.
///
/// Split from [`trc_for`] rather than folded into it because the two sides now take DIFFERENT
/// snapshots (destination: `linear → device`; source: `device → linear`), and one function taking
/// both would let a call site pass the wrong one and still compile.
#[inline]
fn trc_for_src<'a>(g: Gamut, snapshot: Option<&'a Arc<[u16]>>) -> Trc<'a> {
    match g {
        Gamut::SourceIcc(_) => match snapshot {
            Some(lut) => Trc::SrcLut { flat: lut },
            // Unreachable: the snapshot is taken from the same gamut value. sRGB is the floor.
            None => Trc::Srgb,
        },
        other => trc_for(other, None),
    }
}

/// The faithful source's forward-LUT snapshot for one transform — read ONCE per call (an `Arc`
/// clone, no data copy) so the per-pixel loop borrows a stable slice without re-locking the registry.
#[inline]
fn source_lin_snapshot(src: Gamut) -> Option<Arc<[u16]>> {
    source_linearize_lut(src).map(|(_, lut)| lut)
}

/// Transform packed RGBA8 pixels in place from `src` gamut to `dst` gamut. A no-op (no allocation,
/// no work) when `src == dst` — so sRGB files on an sRGB output cost nothing. Alpha is untouched.
/// Parallel across the pixel rows (a full-frame convert runs on the on-stop detail tier).
pub fn transform_rgba(px: &mut [u8], src: Gamut, dst: Gamut) {
    transform_rgba_impl(px, src, dst, wants_parallel(px.len() / 4));
}

/// v0.8.150 (J4) — **THE PARALLEL FLOOR**, in pixels. Below it a transform runs on the calling
/// thread; at or above it, on the shared rayon pool.
///
/// # Why this number exists at all
///
/// This crate has exactly two rayon call sites, and they are the process's ONLY ones outside the ROI
/// worker. They are shared by two populations that could not be more different:
///
///   * **the decode pool** — 18 workers, each converting a whole 48 MP frame (48.8 M px, ~195 MB).
///     Measured at 120 ms per photo even parallel; serial it would be seconds. It must stay parallel.
///   * **the UI thread** — the frosted backdrop's 160 px frost mip (19 k px), a 256 px thumb tile
///     (65 k px), the watermark logo. Work measured in microseconds.
///
/// Rayon's pool is process-wide and its size is `num_cpus`. When 18 decode workers are each mid-way
/// through splitting a 195 MB buffer into leaf tasks, a UI-thread `par_chunks_exact_mut` over 19 k
/// pixels does not "also run in parallel" — it **queues**, and the calling thread blocks until its own
/// leaves are stolen back from behind millions of other people's. The 2026-08-05 settings-jank
/// investigation measured that block at 50–110 ms and could not see inside it; the v0.8.150 split
/// timers put 60.9–109.6 ms of a 61.6–110.3 ms `step_blur_backdrop` on ONE line — this call, on a
/// 76 KB buffer whose own arithmetic is ~0.1 ms.
///
/// 262,144 px (a 1 MB RGBA buffer) sits about an order of magnitude above everything the UI thread
/// ever hands this crate and two orders below a browse frame, so neither population is near the edge.
/// Serial cost AT the floor is ~1–2 ms, which is the most this gate can ever add to a caller that
/// would otherwise have parallelised — and it is paid on a worker, never on the tick.
///
/// The floor is a THROUGHPUT decision, never a colour decision: both arms run the identical
/// [`apply_pixel`] per pixel, in the identical order, with the identical `Trc` snapshot, so their
/// output is byte-for-byte the same buffer. `the_parallel_floor_never_changes_a_pixel` pins that.
pub const PAR_MIN_PX: usize = 1 << 18;

/// Does a transform of `px_count` pixels earn the shared rayon pool? See [`PAR_MIN_PX`].
#[inline]
pub fn wants_parallel(px_count: usize) -> bool {
    px_count >= PAR_MIN_PX
}

/// The body of [`transform_rgba`] with the parallel/serial choice made by the caller — so a test can
/// run BOTH arms over the same bytes and compare them (the floor must never change a pixel).
fn transform_rgba_impl(px: &mut [u8], src: Gamut, dst: Gamut, parallel: bool) {
    use rayon::prelude::*;
    if src == dst {
        return;
    }
    let m = src_to_dst_matrix(src, dst);
    let snap = if dst == Gamut::Custom { custom_enc_lut_snapshot() } else { None };
    // v0.8.177: the SOURCE may now carry its own per-channel curve too — one registry read per
    // transform, `None` for every modeled gamut (so nothing changes for them).
    let src_snap = source_lin_snapshot(src);
    let (st, dt) = (trc_for_src(src, src_snap.as_ref()), trc_for(dst, snap.as_ref()));
    let one = |c: &mut [u8]| {
        let (r, g, b) = apply_pixel(&m, st, dt, c[0], c[1], c[2]);
        c[0] = r;
        c[1] = g;
        c[2] = b;
    };
    if parallel {
        px.par_chunks_exact_mut(4).for_each(one);
    } else {
        px.chunks_exact_mut(4).for_each(one);
    }
}

/// Transform ONE encoded 8-bit RGB triple from `src` to `dst` — the UI-CHROME entry point (v0.8.48).
/// The app's design tokens are authored in sRGB; routing each one through here (the SAME
/// [`apply_pixel`] maths as [`transform_rgba`]/[`transform_rgb`] — matrix + faithful TRC/inverse-LUT)
/// puts the chrome in the identical output space as the photos. Identity (returns `rgb` unchanged,
/// no work) when `src == dst` — so on an sRGB output the tokens equal the design values byte-exactly.
pub fn transform_rgb8(rgb: [u8; 3], src: Gamut, dst: Gamut) -> [u8; 3] {
    if src == dst {
        return rgb;
    }
    let m = src_to_dst_matrix(src, dst);
    let snap = if dst == Gamut::Custom { custom_enc_lut_snapshot() } else { None };
    // v0.8.177: the SOURCE may now carry its own per-channel curve too — one registry read per
    // transform, `None` for every modeled gamut (so nothing changes for them).
    let src_snap = source_lin_snapshot(src);
    let (st, dt) = (trc_for_src(src, src_snap.as_ref()), trc_for(dst, snap.as_ref()));
    let (r, g, b) = apply_pixel(&m, st, dt, rgb[0], rgb[1], rgb[2]);
    [r, g, b]
}

/// Transform packed RGB8 pixels in place from `src` gamut to `dst` gamut. No-op when `src == dst`.
///
/// v0.8.150 (J4): carries the same [`PAR_MIN_PX`] floor as [`transform_rgba`] — the two functions are
/// this crate's only rayon call sites and a floor on one of them would just move the stall.
pub fn transform_rgb(px: &mut [u8], src: Gamut, dst: Gamut) {
    transform_rgb_impl(px, src, dst, wants_parallel(px.len() / 3));
}

/// The body of [`transform_rgb`], parallel/serial chosen by the caller (see [`transform_rgba_impl`]).
fn transform_rgb_impl(px: &mut [u8], src: Gamut, dst: Gamut, parallel: bool) {
    use rayon::prelude::*;
    if src == dst {
        return;
    }
    let m = src_to_dst_matrix(src, dst);
    let snap = if dst == Gamut::Custom { custom_enc_lut_snapshot() } else { None };
    // v0.8.177: the SOURCE may now carry its own per-channel curve too — one registry read per
    // transform, `None` for every modeled gamut (so nothing changes for them).
    let src_snap = source_lin_snapshot(src);
    let (st, dt) = (trc_for_src(src, src_snap.as_ref()), trc_for(dst, snap.as_ref()));
    let one = |c: &mut [u8]| {
        let (r, g, b) = apply_pixel(&m, st, dt, c[0], c[1], c[2]);
        c[0] = r;
        c[1] = g;
        c[2] = b;
    };
    if parallel {
        px.par_chunks_exact_mut(3).for_each(one);
    } else {
        px.chunks_exact_mut(3).for_each(one);
    }
}

/// v1.0.0-rc PNG EXPORT (queue item 35): the SIXTEEN-BIT twin of [`apply_pixel`].
///
/// The interior is not a twin, it is the SAME arithmetic: the same [`src_to_dst_matrix`], the same
/// source and destination [`Trc`] pair, the same linear-light multiply, in the same f32. Only the
/// two ENDS move -- `/ 65535.0` going in, `* 65535.0 + 0.5` coming out -- because the ./Web PNG
/// stop now carries a 16-bit source at 16 bits from the decoder to the encoder, and an 8-bit round
/// trip in the middle of it would throw away exactly the depth the file was kept for.
///
/// The rounding is the 8-bit function's, written at the 16-bit scale: `+ 0.5`, clamp, truncate,
/// which is round-half-up on a non-negative value. `the_16_bit_transform_agrees_with_the_8_bit_one`
/// pins the two against each other on the same colours.
#[inline]
fn apply_pixel16(m: &[[f32; 3]; 3], st: Trc, dt: Trc, r: u16, g: u16, b: u16) -> (u16, u16, u16) {
    let lr = st.linearize(0, r as f32 / 65535.0);
    let lg = st.linearize(1, g as f32 / 65535.0);
    let lb = st.linearize(2, b as f32 / 65535.0);
    let or = dt.encode(0, m[0][0] * lr + m[0][1] * lg + m[0][2] * lb);
    let og = dt.encode(1, m[1][0] * lr + m[1][1] * lg + m[1][2] * lb);
    let ob = dt.encode(2, m[2][0] * lr + m[2][1] * lg + m[2][2] * lb);
    (
        (or * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16,
        (og * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16,
        (ob * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16,
    )
}

/// Transform packed RGB16 pixels in place from `src` gamut to `dst` gamut. No-op when `src == dst`.
///
/// **THE PARALLEL FLOOR IS COUNTED IN PIXELS**, as [`PAR_MIN_PX`]'s own doc says it is -- and `px`
/// here is a SAMPLE slice (`[u16]`), not a byte slice, so the divisor below is the CHANNEL count.
/// [`transform_rgb`] divides its BYTE slice by the byte stride and reaches the same number for a
/// different reason; the reason is worth stating, because the two spellings look identical and stop
/// being identical the moment a sample stops being a byte.
pub fn transform_rgb16(px: &mut [u16], src: Gamut, dst: Gamut) {
    transform16_impl(px, src, dst, 3, wants_parallel(px.len() / 3));
}

/// Transform packed RGBA16 pixels in place from `src` gamut to `dst` gamut. No-op when `src == dst`.
/// **Alpha is untouched** -- the contract [`transform_rgba`] states, for the same reason: alpha is a
/// coverage, not a colour, and no gamut has an opinion about it. See [`transform_rgb16`] for the
/// parallel floor's unit.
pub fn transform_rgba16(px: &mut [u16], src: Gamut, dst: Gamut) {
    transform16_impl(px, src, dst, 4, wants_parallel(px.len() / 4));
}

/// The ONE body of [`transform_rgb16`] and [`transform_rgba16`], parallel/serial chosen by the
/// caller exactly as [`transform_rgba_impl`] does it. `ch` is the channel count (3 or 4) and the
/// closure writes only `c[0..3]`, so the four-channel arm's alpha lane is unreachable by
/// construction rather than by care.
fn transform16_impl(px: &mut [u16], src: Gamut, dst: Gamut, ch: usize, parallel: bool) {
    use rayon::prelude::*;
    if src == dst {
        return;
    }
    let m = src_to_dst_matrix(src, dst);
    let snap = if dst == Gamut::Custom { custom_enc_lut_snapshot() } else { None };
    let src_snap = source_lin_snapshot(src);
    let (st, dt) = (trc_for_src(src, src_snap.as_ref()), trc_for(dst, snap.as_ref()));
    let one = |c: &mut [u16]| {
        let (r, g, b) = apply_pixel16(&m, st, dt, c[0], c[1], c[2]);
        c[0] = r;
        c[1] = g;
        c[2] = b;
    };
    if parallel {
        px.par_chunks_exact_mut(ch).for_each(one);
    } else {
        px.chunks_exact_mut(ch).for_each(one);
    }
}

// ───────────────────────────── custom output profile (ICC) ─────────────────────────────

/// A user-loaded custom OUTPUT profile — a monitor/display ICC. Its RGB→XYZ primaries are already
/// chromatic-adapted to D65 by [`parse_display_icc`] (so they drop into the all-D65 transform pipeline).
/// v0.8.47: its per-channel tone curve is kept FAITHFULLY — the `enc_lut` holds the profile's real
/// `linear → device` encode curve (the inverse of the measured `curv`/`para` TRC) as three
/// [`CUSTOM_LUT_N`]-entry u16 LUTs (rows R,G,B). `gamma` survives only as a summary (logging + the
/// no-profile fallback); the transform NEVER reduces the curve to it.
#[derive(Clone, Debug)]
pub struct CustomProfile {
    /// Linear device RGB → CIE XYZ, D65-referenced (chad-adapted). Columns are the R/G/B colorants.
    pub rgb_to_xyz: [[f32; 3]; 3],
    /// Effective/summary display gamma — for the diagnostics log line and the no-profile fallback ONLY.
    pub gamma: f32,
    /// Human label for the UI chip (the caller's filename stem, or the profile's own description).
    pub label: String,
    /// Human summary of the parsed tone curve for the diagnostics log line
    /// (e.g. `curv[1024] -> inv-lut[4096]`, `per-ch para type3 -> inv-lut[4096]`).
    pub trc_summary: String,
    /// Per-channel `linear → device` inverse-tone-curve LUTs, `3 × CUSTOM_LUT_N` u16 (rows R,G,B).
    /// Arc so a per-transform snapshot (and the GPU LUT-texture fetch) is a cheap clone, not a copy.
    enc_lut: Arc<[u16]>,
}

impl CustomProfile {
    /// Build a profile from primaries + a single pure display gamma. The `enc_lut` is the analytic
    /// inverse `x^(1/gamma)` for all three channels. Used as the no-profile fallback and by parity
    /// tests; REAL profiles go through [`parse_display_icc`], which keeps the measured per-channel curve.
    pub fn from_gamma(rgb_to_xyz: [[f32; 3]; 3], gamma: f32, label: impl Into<String>) -> Self {
        let lut = inverse_lut_for(&ToneCurve::Gamma(gamma));
        let mut flat = Vec::with_capacity(3 * CUSTOM_LUT_N);
        for _ in 0..3 {
            flat.extend_from_slice(&lut);
        }
        Self {
            rgb_to_xyz,
            gamma,
            label: label.into(),
            trc_summary: format!("gamma {gamma:.2} -> inv-lut[{CUSTOM_LUT_N}]"),
            enc_lut: flat.into(),
        }
    }

    /// The per-channel inverse-tone-curve LUTs (`3 × CUSTOM_LUT_N` u16, linear→device, rows R,G,B).
    pub fn encode_lut(&self) -> &[u16] {
        &self.enc_lut
    }
}

impl Default for CustomProfile {
    fn default() -> Self {
        // Before any profile is loaded, `Gamut::Custom` behaves as sRGB primaries + ~2.2 gamma so a stray
        // Custom selection is harmless rather than producing garbage colour.
        CustomProfile::from_gamma(Gamut::Srgb.rgb_to_xyz(), 2.2, "Custom")
    }
}

/// The installed custom output profile.
///
/// v0.8.152 (R3-L10) — **poisoning recovers to the house idiom at all four sites.** Every reader
/// used to map `Err(_)` to the sRGB default and `set_custom_profile` did NOTHING AT ALL on a
/// poisoned lock (`if let Ok(mut w)`), so a single poisoning flipped the whole app to sRGB with no
/// log line, no user-visible reason, and — because the writer was mute too — no way ever to heal:
/// exactly the one-way-latch class the rubric names. The idiom everywhere else in the tree is
/// `unwrap_or_else(|e| e.into_inner())` (see `falcon-decode`'s `DECODE_NOTES`), and it is right here
/// for the same reason it is right there: the only code that runs inside this lock is an
/// assignment, an `Arc::clone` and three field reads, none of which can panic, so a poisoned lock
/// would be a report of a panic somewhere else entirely and the profile behind it is still intact.
static CUSTOM: RwLock<Option<CustomProfile>> = RwLock::new(None);
/// Bumped on every [`set_custom_profile`]. `Gamut::Custom` is a data-less enum value, so any cache
/// keyed by `Gamut` cannot tell "Custom with profile A" from "Custom with profile B" — a reloaded
/// profile (e.g. re-running monitor auto-detect after changing it in Windows Settings) would hit
/// stale converted pixels. Such caches watch this generation and invalidate when it changes.
static CUSTOM_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Install the active custom output profile (from an imported / auto-detected ICC). Read by every colour
/// transform when the output gamut is [`Gamut::Custom`]. Overwrites any previously-loaded profile and
/// bumps [`custom_profile_gen`] so `Gamut`-keyed caches drop pixels converted with the old profile.
pub fn set_custom_profile(p: CustomProfile) {
    // v0.8.152 (R3-L10): this site is the one that made the degradation PERMANENT — `if let Ok(..)`
    // meant a poisoned lock silently dropped the install, so the state could never heal.
    let mut w = CUSTOM.write().unwrap_or_else(|e| e.into_inner());
    *w = Some(p);
    CUSTOM_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
}

/// The custom-profile content generation — changes whenever a (new) profile is installed.
pub fn custom_profile_gen() -> u64 {
    CUSTOM_GEN.load(std::sync::atomic::Ordering::Acquire)
}

/// The active custom inverse-tone-curve LUTs for the GPU fast tier (and the parity mirror):
/// `(generation, 3 × CUSTOM_LUT_N u16 linear→device, rows R,G,B)`, or `None` when no profile is loaded.
/// The generation + LUT are read together under the profile lock, so they can never be observed
/// half-swapped (the GPU caches the texture keyed by this generation — rebuilt only on a profile change).
pub fn custom_encode_lut() -> Option<(u64, Arc<[u16]>)> {
    // v0.8.152 (R3-L10): `.ok()?` mapped a poisoned lock to "no profile", i.e. to sRGB.
    let g = CUSTOM.read().unwrap_or_else(|e| e.into_inner());
    let p = g.as_ref()?;
    Some((CUSTOM_GEN.load(std::sync::atomic::Ordering::Acquire), p.enc_lut.clone()))
}

/// The analytic `linear → device` inverse LUT for a pure gamma (`CUSTOM_LUT_N` u16, one channel). The
/// GPU builds a 3-row stand-in from this when no custom profile is installed, matching the CPU's
/// `Trc::Gamma` fallback in [`trc_for`], so the (unreachable) dst==Custom-with-no-profile state stays
/// CPU/GPU-consistent.
pub fn gamma_encode_lut(gamma: f32) -> Vec<u16> {
    inverse_lut_for(&ToneCurve::Gamma(gamma))
}

/// The active custom `(rgb_to_xyz, gamma)`, or the sRGB-equivalent default when none is loaded. Read
/// ONCE per transform call (not per pixel) — see [`Gamut::rgb_to_xyz`]. `Gamut::Srgb`'s `rgb_to_xyz`
/// arm is hardcoded (no global access), so there is no re-entrancy here.
fn custom_matrix_gamma() -> ([[f32; 3]; 3], f32) {
    // v0.8.152 (R3-L10): the sRGB fallback now means exactly one thing — "no profile is loaded" —
    // and no longer doubles as the answer to "the lock is poisoned".
    let g = CUSTOM.read().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(p) => (p.rgb_to_xyz, p.gamma),
        None => (Gamut::Srgb.rgb_to_xyz(), 2.2),
    }
}

/// Determinant of a 3×3 (a degenerate colorant matrix → reject the profile).
fn mat3_det(m: [[f32; 3]; 3]) -> f32 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Read an ICC `s15Fixed16Number` (signed 16.16 fixed point) at byte offset `o`.
fn s15fixed(b: &[u8], o: usize) -> f32 {
    (i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f32) / 65536.0
}

/// Bradford chromatic adaptation D50 → D65 (Lindbloom's standard matrix; the inverse of the D65→D50
/// used to build the `chad` tag). Applied to the colorants of a profile that has NO `chad` tag —
/// typically an ICC v2 working-space profile (AdobeRGB1998.icc, sRGB, …) whose colorants are stored in
/// the D50 PCS. Without this their white renders as ~D50 (a warm cast) in this D65 pipeline. Display
/// profiles targeted at a non-D65 white instead carry a `chad`, which recovers their exact white point.
const BRADFORD_D50_TO_D65: [[f32; 3]; 3] = [
    [0.955_576_6, -0.023_039_3, 0.063_163_6],
    [-0.028_289_5, 1.009_941_6, 0.021_007_7],
    [0.012_298_2, -0.020_483_0, 1.329_909_8],
];

/// The ICC tag count, validated so the whole 12-bytes-per-entry tag table fits the buffer. `None`
/// for a header-truncated or over-declared profile (untrusted file input — fail closed).
fn icc_tag_count(icc: &[u8]) -> Option<usize> {
    if icc.len() < 132 {
        return None;
    }
    let n = u32::from_be_bytes([icc[128], icc[129], icc[130], icc[131]]) as usize;
    (132 + n.checked_mul(12)? <= icc.len()).then_some(n)
}

/// Locate a tag's `(offset, size)`, validated to lie within the buffer. Shared by every reader
/// below so there is ONE bounds discipline, not one per caller.
fn icc_find_tag(icc: &[u8], tag_count: usize, sig: &[u8; 4]) -> Option<(usize, usize)> {
    for k in 0..tag_count {
        let e = 132 + k * 12;
        if &icc[e..e + 4] == sig {
            let be32 = |o: usize| u32::from_be_bytes([icc[o], icc[o + 1], icc[o + 2], icc[o + 3]]) as usize;
            let off = be32(e + 4);
            let size = be32(e + 8);
            if off >= 128 && off.checked_add(size)? <= icc.len() {
                return Some((off, size));
            }
        }
    }
    None
}

/// The COLORIMETRY half of [`parse_display_icc`]: a matrix/TRC display ICC's `rXYZ`/`gXYZ`/`bXYZ`
/// colorants, re-referenced from the D50 PCS to the device white (≈ D65) exactly as that function
/// does — but WITHOUT building the tone-curve LUTs.
///
/// v0.8.140 (THE COLOR ROUND, C1): the source side asks only "what primaries is this file encoded
/// in?", and it asks once per file load. Building three 4096-entry inverse LUTs to answer that
/// would be ~12k `powf` calls thrown away, so the two questions are split — and `parse_display_icc`
/// is now built ON this function, so the colorant/`chad` maths cannot drift between them.
///
/// Deliberately does NOT require a tone curve: a profile's TRC is irrelevant to which gamut its
/// primaries name, and the source transform linearises with the MODELED gamut's analytic curve
/// either way (see `trc_for(src, None)` — a source is always a named gamut). `None` for a
/// non-matrix profile (LUT/cLUT-class has no colorant tags), a malformed one, or a degenerate
/// colorant matrix. Bounds every read against the (untrusted) buffer.
///
/// v0.8.141 (R9) — THE `chad` HOLE, CLOSED FROM BOTH ENDS. A `chad` tag is attacker-supplied like
/// every other byte here, and it was the one number this function inverted without checking:
/// - a SINGULAR chad (all zeros, say) fell into [`mat3_inv`]'s `det < 1e-12` arm, which returns the
///   IDENTITY — so the profile silently kept its D50-PCS colorants, an undocumented third
///   adaptation arm that fails OPEN with a plausible-looking matrix and a plausible-looking
///   distance. `CHAD_MIN_DET` refuses it instead.
/// - a chad that is merely NEAR-singular (`1e-3·I`, det 1e-9 — comfortably past `mat3_inv`'s floor)
///   inverts to `1e3·I` and multiplies the colorants by a thousand: the crafted rows below measure
///   nearest-gamut distances around **953** from such a profile today. A determinant threshold
///   alone cannot catch the asymmetric form of that (a chad degenerate on ONE axis has a perfectly
///   healthy determinant), so the RESULT is bounded too — `COLORANT_MAX_ABS`.
///
/// DECLARED COUPLING (accepted, and named in the v0.8.141 commit): [`parse_display_icc`] is built
/// on this function, so a MONITOR profile with a broken `chad` now fails to load and the user sees
/// the "profile type isn't supported" refusal instead of the previous silent load with a colour
/// cast. An honest refusal beats silently wrong colour.
pub fn icc_colorants_d65(icc: &[u8]) -> Option<[[f32; 3]; 3]> {
    let tag_count = icc_tag_count(icc)?;
    let find = |sig: &[u8; 4]| icc_find_tag(icc, tag_count, sig);
    // XYZType colorant: 'XYZ ' + 4 reserved + one XYZ triplet (3× s15Fixed16).
    let read_xyz = |sig: &[u8; 4]| -> Option<[f32; 3]> {
        let (off, size) = find(sig)?;
        if size < 20 || &icc[off..off + 4] != b"XYZ " {
            return None;
        }
        Some([s15fixed(icc, off + 8), s15fixed(icc, off + 12), s15fixed(icc, off + 16)])
    };
    let r = read_xyz(b"rXYZ")?;
    let g = read_xyz(b"gXYZ")?;
    let b = read_xyz(b"bXYZ")?;
    // Columns are the colorants → M maps linear device RGB to XYZ in the D50 PCS.
    let m_d50 = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    // 'chad' (sf32Type, 9× s15Fixed16 row-major): the Bradford matrix mapping the DEVICE white → D50.
    // `inv(chad)·M_D50` re-references the colorants to the device white (≈ D65 for a display), which is
    // what this D65 pipeline expects (white then maps to the monitor's native white — relative-colorimetric).
    let m = match find(b"chad") {
        Some((off, size)) if size >= 44 && &icc[off..off + 4] == b"sf32" => {
            let mut chad = [[0.0f32; 3]; 3];
            for (i, row) in chad.iter_mut().enumerate() {
                for (j, cell) in row.iter_mut().enumerate() {
                    *cell = s15fixed(icc, off + 8 + (i * 3 + j) * 4);
                }
            }
            // v0.8.141 (R9) THE DET GUARD. Refuse a chad we cannot honestly invert BEFORE inverting
            // it — `mat3_inv` would hand back the identity and the profile would keep its D50
            // colorants under a D65 label, wrong and silent.
            if !chad.iter().flatten().all(|v| v.is_finite()) || mat3_det(chad).abs() < CHAD_MIN_DET {
                return None;
            }
            mat3_mul(mat3_inv(chad), m_d50)
        }
        // No chad → the colorants are D50-PCS-referenced (ICC v2). Adapt them to D65 so the white point
        // matches this pipeline; a bare `m_d50` would render ~D50 (a warm cast) — proven on AdobeRGB1998.icc.
        _ => mat3_mul(BRADFORD_D50_TO_D65, m_d50),
    };
    // 1e-9 here is a PROFILE-SANITY threshold (reject a degenerate colorant matrix outright) —
    // deliberately looser than mat3_inv's 1e-12, which only floors the 1/det division's stability.
    // v0.8.141 (R9) THE RESULT BOUND joins it: a chad degenerate on ONE axis keeps a healthy
    // determinant and still blows one row of the product up by orders of magnitude, which the det
    // test cannot see. Nothing physical lives past COLORANT_MAX_ABS.
    if !m.iter().flatten().all(|v| v.is_finite())
        || m.iter().flatten().any(|v| v.abs() > COLORANT_MAX_ABS)
        || mat3_det(m).abs() < 1e-9
    {
        return None;
    }
    Some(m)
}

/// v0.8.141 (R9): the smallest |det| a `chad` tag may carry and still be treated as a real
/// chromatic-adaptation matrix. A genuine one is a Bradford adaptation between two illuminants —
/// `BRADFORD_D65_TO_D50`'s determinant is ≈ 0.78 and an identity chad's is 1, so every real profile
/// clears this by six orders of magnitude, while `mat3_inv`'s silent identity arm (`det < 1e-12`)
/// becomes unreachable from here.
const CHAD_MIN_DET: f32 = 1e-6;

/// v0.8.141 (R9): the largest absolute element a recovered D65 colorant matrix may carry. The
/// biggest entry in ANY gamut we model is Rec.2020's blue-Z at 1.061 (ProPhoto's, the widest real
/// space, is 1.098), so 4.0 is ~3.6× the widest legitimate value — no real profile can reach it,
/// and the crafted near-degenerate chads that produce entries in the hundreds cannot pass it.
const COLORANT_MAX_ABS: f32 = 4.0;

/// Parse a matrix/TRC display ICC (v2 or v4) into a [`CustomProfile`]: extract the RGB colorants
/// (`rXYZ`/`gXYZ`/`bXYZ`, in the D50 PCS), re-reference them to the device white (≈ D65) via the `chad`
/// chromatic-adaptation tag so they match this all-D65 pipeline, and keep the per-channel tone curves
/// (`rTRC`/`gTRC`/`bTRC`) FAITHFULLY as inverse LUTs (v0.8.47 — no longer reduced to one gamma). Returns
/// `None` for a non-matrix profile (a LUT/cLUT-only profile has no colorant tags) or a malformed/degenerate
/// one — the caller then keeps the previous output gamut and tells the user the profile type isn't
/// supported. Bounds every tag read against the buffer (untrusted file input), so a truncated/crafted
/// profile fails closed rather than panicking.
pub fn parse_display_icc(icc: &[u8], label: impl Into<String>) -> Option<CustomProfile> {
    let m = icc_colorants_d65(icc)?;
    let tag_count = icc_tag_count(icc)?;
    let find = |sig: &[u8; 4]| icc_find_tag(icc, tag_count, sig);
    // TRC: keep the real per-channel curve. Green ≈ luminance is the anchor; a missing channel falls back
    // to green (else red/blue). A profile with NO usable curve on any channel is rejected.
    let parse_ch = |sig: &[u8; 4]| find(sig).and_then(|(o, s)| parse_tone_curve(icc, o, s));
    let rc = parse_ch(b"rTRC");
    let gc = parse_ch(b"gTRC");
    let bc = parse_ch(b"bTRC");
    let gc = gc.or_else(|| rc.clone()).or_else(|| bc.clone())?;
    let rc = rc.unwrap_or_else(|| gc.clone());
    let bc = bc.unwrap_or_else(|| gc.clone());
    // Build the per-channel inverse LUTs (linear→device) — ALWAYS three rows (identical rows when the
    // curves match; `per_channel` only feeds the diagnostics summary, it never changes the layout).
    let rl = inverse_lut_for(&rc);
    let gl = inverse_lut_for(&gc);
    let bl = inverse_lut_for(&bc);
    let per_channel = rl != gl || gl != bl;
    let mut flat = Vec::with_capacity(3 * CUSTOM_LUT_N);
    flat.extend_from_slice(&rl);
    flat.extend_from_slice(&gl);
    flat.extend_from_slice(&bl);
    let gamma = gc.effective_gamma().clamp(1.0, 3.5);
    let trc_summary = format!(
        "{}{} -> inv-lut[{}]",
        if per_channel { "per-ch " } else { "" },
        gc.kind_summary(),
        CUSTOM_LUT_N
    );
    Some(CustomProfile { rgb_to_xyz: m, gamma, label: label.into(), trc_summary, enc_lut: flat.into() })
}

/// A parsed ICC tone curve, forward (device → linear). The three curve encodings a matrix/TRC display
/// profile can carry: a pure gamma (`curv` N=0/1), a sampled table (`curv` N≥2 — the measured case), or
/// a `para` parametric function. [`inverse_lut_for`] turns any of these into the `linear → device`
/// encode LUT the transform needs.
#[derive(Clone, Debug)]
enum ToneCurve {
    /// Pure gamma: `linear = device^g`.
    Gamma(f32),
    /// Sampled table (N≥2): `linear[i]` at `device = i/(N-1)`, linear interpolation between.
    Table(Vec<f32>),
    /// parametricCurveType (funcTypes 0..4), params in ICC order `g,a,b,c,d,e,f`.
    Para { func: u16, p: [f32; 7] },
}

impl ToneCurve {
    /// A single representative gamma — for the diagnostics summary + the no-profile fallback ONLY (the
    /// transform never uses it for a real curve). Table: the classic midpoint estimate.
    fn effective_gamma(&self) -> f32 {
        match self {
            ToneCurve::Gamma(g) => *g,
            ToneCurve::Para { p, .. } => p[0],
            ToneCurve::Table(t) => {
                let n = t.len();
                let mid = n / 2;
                let x = mid as f32 / (n - 1) as f32;
                let v = t[mid];
                if x > 0.0 && x < 1.0 && v > 0.0 {
                    v.ln() / x.ln()
                } else {
                    2.2
                }
            }
        }
    }
    fn kind_summary(&self) -> String {
        match self {
            ToneCurve::Gamma(g) => format!("gamma {g:.2}"),
            ToneCurve::Para { func, .. } => format!("para type{func}"),
            ToneCurve::Table(t) => format!("curv[{}]", t.len()),
        }
    }

    /// FORWARD evaluation (v0.8.177): encoded device value `0..1` → linear light. The destination
    /// side only ever needed this curve INVERTED (see [`inverse_lut_for`], which for a table walks
    /// the raw samples); the faithful SOURCE route needs it in its native direction, which is the
    /// direction an ICC `curv`/`para` tag is defined in. A `Table` interpolates between the two
    /// bracketing samples exactly as [`inverse_lut_for`]'s inverse walk assumes.
    fn eval(&self, device: f32) -> f32 {
        let x = device.clamp(0.0, 1.0);
        match self {
            ToneCurve::Gamma(g) => x.powf(g.max(1e-3)),
            ToneCurve::Para { func, p } => para_eval(*func, p, x),
            ToneCurve::Table(t) => {
                // `parse_tone_curve` only ever builds a Table with n ≥ 2, so `n - 1 ≥ 1`.
                let n = t.len();
                let pos = x * (n - 1) as f32;
                let i0 = (pos as usize).min(n - 1);
                let i1 = (i0 + 1).min(n - 1);
                let frac = pos - i0 as f32;
                t[i0] + (t[i1] - t[i0]) * frac
            }
        }
    }
}

/// ICC parametricCurveType forward evaluation (device → linear). Params `g,a,b,c,d,e,f` (ICC order).
fn para_eval(func: u16, p: &[f32; 7], x: f32) -> f32 {
    let (g, a, b, c, d, e, f) = (p[0], p[1], p[2], p[3], p[4], p[5], p[6]);
    let pw = |base: f32, ex: f32| base.max(0.0).powf(ex);
    // Threshold `-b/a` for the split funcs; guard a≈0 (degenerate) → threshold 0.
    let split = if a.abs() > 1e-9 { -b / a } else { 0.0 };
    match func {
        0 => pw(x, g),
        1 => if x >= split { pw(a * x + b, g) } else { 0.0 },
        2 => if x >= split { pw(a * x + b, g) + c } else { c },
        3 => if x >= d { pw(a * x + b, g) } else { c * x },
        4 => if x >= d { pw(a * x + b, g) + e } else { c * x + f },
        _ => x,
    }
}

/// Parse a `curv`/`para` tone-curve tag into a forward [`ToneCurve`]. Returns `None` for an unknown
/// type or a truncated tag.
///
/// **v0.8.177 (QUEUE §5c(iii)) — THE BOUND IS THE TAG'S, NOT THE BUFFER'S.** Every read used to be
/// checked against `icc.len()`, the length of the WHOLE profile. A tag's `size` field says how many
/// bytes the tag actually occupies, and the three checks ignored it — so a `curv` tag declaring
/// `size = 12` (a header and nothing else) whose count field said 1024 read 2048 bytes of WHATEVER
/// FOLLOWED IT in the profile — the next tag's colorants, a copyright string, padding — and returned
/// them as a tone curve. The read never left the buffer, so nothing crashed and nothing was refused:
/// the profile got a confident, entirely fabricated curve, which on the destination side is a
/// miscalibrated monitor and on the new source side would be a miscoloured photograph.
///
/// The rule now: `end = off + size` (already validated to lie inside the buffer by [`icc_find_tag`])
/// bounds every read, and a tag whose declared extent cannot hold what its own header promises is a
/// REFUSAL — the caller falls to the next route, which is a curve from somewhere honest or no
/// faithful route at all. `truncated_curv_tag_is_refused_not_fabricated` is the falsifier; it was
/// written against the pre-fix code and observed to produce a curve.
fn parse_tone_curve(icc: &[u8], off: usize, size: usize) -> Option<ToneCurve> {
    // The tag's own declared extent, clamped to the buffer. `icc_find_tag` has already checked
    // `off + size <= icc.len()`, but this function is also reachable with a caller-supplied
    // `(off, size)` and must fail closed rather than trust one.
    let end = off.checked_add(size)?;
    if size < 12 || end > icc.len() {
        return None;
    }
    match &icc[off..off + 4] {
        b"curv" => {
            let n = u32::from_be_bytes([icc[off + 8], icc[off + 9], icc[off + 10], icc[off + 11]]) as usize;
            if n == 0 {
                return Some(ToneCurve::Gamma(1.0)); // empty curve = identity (linear)
            }
            if n == 1 {
                // A single u8Fixed8Number gamma at off+12 — 2 bytes that must be INSIDE this tag.
                if off + 14 > end {
                    return None;
                }
                return Some(ToneCurve::Gamma(u16::from_be_bytes([icc[off + 12], icc[off + 13]]) as f32 / 256.0));
            }
            let table = off + 12;
            if table.checked_add(n.checked_mul(2)?)? > end {
                return None;
            }
            let t: Vec<f32> = (0..n)
                .map(|i| u16::from_be_bytes([icc[table + i * 2], icc[table + i * 2 + 1]]) as f32 / 65535.0)
                .collect();
            Some(ToneCurve::Table(t))
        }
        b"para" => {
            // funcType (u16) at off+8, then s15Fixed16 params from off+12.
            let func = u16::from_be_bytes([icc[off + 8], icc[off + 9]]);
            let nparams = match func {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            if off + 12 + nparams * 4 > end {
                return None;
            }
            let mut p = [0.0f32; 7];
            for (i, cell) in p.iter_mut().enumerate().take(nparams) {
                *cell = s15fixed(icc, off + 12 + i * 4);
            }
            Some(ToneCurve::Para { func, p })
        }
        _ => None,
    }
}

/// Build the `linear → device` inverse-tone-curve LUT (`CUSTOM_LUT_N` u16, uniform in linear) for a
/// forward tone curve. Gamma is inverted analytically; a table/parametric curve is inverted by a
/// monotone forward-scan + linear interpolation (matches littleCMS's reverse LUT to ±0 codes — verified).
fn inverse_lut_for(curve: &ToneCurve) -> Vec<u16> {
    match curve {
        ToneCurve::Gamma(g) => {
            let inv = 1.0 / g.max(1e-3);
            (0..CUSTOM_LUT_N)
                .map(|k| {
                    let l = k as f32 / (CUSTOM_LUT_N - 1) as f32;
                    (l.powf(inv) * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16
                })
                .collect()
        }
        // Invert the RAW forward samples (device j/(n-1) → fwd[j]). The forward curve is monotone
        // non-decreasing; walk it once (linear target increases with k) and interpolate within each bucket.
        ToneCurve::Table(fwd) => invert_forward(fwd),
        // Sample the parametric forward at CUSTOM_LUT_N device points, then invert as a table.
        ToneCurve::Para { func, p } => {
            let fwd: Vec<f32> = (0..CUSTOM_LUT_N)
                .map(|j| para_eval(*func, p, j as f32 / (CUSTOM_LUT_N - 1) as f32))
                .collect();
            invert_forward(&fwd)
        }
    }
}

/// Monotone inversion: given `fwd[j]` = linear light at device `j/(n-1)` (non-decreasing), produce the
/// `linear → device` LUT (`CUSTOM_LUT_N` u16, uniform in linear).
fn invert_forward(fwd: &[f32]) -> Vec<u16> {
    let n = fwd.len();
    let mut lut = Vec::with_capacity(CUSTOM_LUT_N);
    let mut j = 0usize;
    for k in 0..CUSTOM_LUT_N {
        let target = k as f32 / (CUSTOM_LUT_N - 1) as f32;
        while j + 1 < n && fwd[j + 1] < target {
            j += 1;
        }
        let d = if target <= fwd[0] {
            0.0
        } else if target >= fwd[n - 1] {
            1.0
        } else {
            let (lo, hi) = (fwd[j], fwd[j + 1]);
            let t = if hi > lo { (target - lo) / (hi - lo) } else { 0.0 };
            (j as f32 + t) / (n - 1) as f32
        };
        lut.push((d * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16);
    }
    lut
}

// ───────────────────────────── ICC serialization (v0.9.21) ─────────────────────────────
// WHY: macOS tags Slint's CAMetalLayer with a CGColorSpace matching Falcon's OUTPUT gamut so
// ColorSync interprets our frames correctly (the tester's double-transform fix: the layer was
// kCGColorSpaceSRGB while we wrote output-gamut pixels). CoreGraphics' NAMED spaces cover
// sRGB / Display P3 / Adobe RGB (1998) / DCI-P3 exactly, but there is NO named space matching
// falcon-color's Rec.2020 ENCODING — Rec.2020 primaries + the sRGB piecewise transfer (see
// `trc_kind`: Srgb/DisplayP3/Rec2020 all share the sRGB curve; kCGColorSpaceITUR_2020 uses the
// BT.2020 camera OETF, a materially different curve in the shadows). So the app serializes the
// EXACT encoding as a minimal matrix/TRC display ICC and hands CoreGraphics the bytes
// (CGColorSpaceCreateWithICCData) — exact by construction, and kept honest by round-trip tests
// (serialize → `parse_display_icc` → recover this module's own matrix + TRC).
//
// PCS mechanics: an ICC stores colorants in the D50 PCS with a `chad` (Bradford) tag mapping the
// DEVICE white → D50. Falcon's pipeline matrices are ABSOLUTE, referenced to each gamut's NATIVE
// white (D65 for four of them, the DCI white for DciP3 — the matrix columns SUM to that white),
// so serialization computes `chad = bradford(native_white → D50)` and stores `chad · M`. A
// consumer that un-adapts via the chad (as `parse_display_icc` and ColorSync both do) recovers
// the native-white matrix exactly — no white re-mapping is invented anywhere.

/// The ICC PCS illuminant (D50), the exact spec bytes 0x0000F6D6 / 0x00010000 / 0x0000D32D.
const D50_XYZ: [f32; 3] = [0.964_202_9, 1.0, 0.824_905_4];

/// Bradford chromatic-adaptation matrix taking XYZ referenced to `white` → the D50 PCS. The
/// standard CAT: `inv(B) · diag(coneD50 / coneWhite) · B` over the Bradford cone matrix.
fn bradford_to_d50(white: [f32; 3]) -> [[f32; 3]; 3] {
    const BRADFORD: [[f32; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let cone = |w: [f32; 3]| -> [f32; 3] {
        [
            BRADFORD[0][0] * w[0] + BRADFORD[0][1] * w[1] + BRADFORD[0][2] * w[2],
            BRADFORD[1][0] * w[0] + BRADFORD[1][1] * w[1] + BRADFORD[1][2] * w[2],
            BRADFORD[2][0] * w[0] + BRADFORD[2][1] * w[1] + BRADFORD[2][2] * w[2],
        ]
    };
    let cs = cone(white);
    let cd = cone(D50_XYZ);
    let diag = [[cd[0] / cs[0], 0.0, 0.0], [0.0, cd[1] / cs[1], 0.0], [0.0, 0.0, cd[2] / cs[2]]];
    mat3_mul(mat3_inv(BRADFORD), mat3_mul(diag, BRADFORD))
}

/// An ICC `s15Fixed16Number` (big-endian) from an f32.
fn s15_be(v: f32) -> [u8; 4] {
    (((v as f64) * 65536.0).round() as i32).to_be_bytes()
}

/// A `para` parametricCurveType tag: sig + 4 reserved + u16 funcType + 2 reserved + s15 params.
fn para_tag(func: u16, params: &[f32]) -> Vec<u8> {
    let mut v = b"para".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&func.to_be_bytes());
    v.extend_from_slice(&[0u8; 2]);
    for p in params {
        v.extend_from_slice(&s15_be(*p));
    }
    v
}

/// An `XYZ ` XYZType tag holding one XYZ triplet.
fn xyz_tag(xyz: [f32; 3]) -> Vec<u8> {
    let mut v = b"XYZ ".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    for c in xyz {
        v.extend_from_slice(&s15_be(c));
    }
    v
}

/// A v4 `mluc` (one en-US record) — the v4-required type for desc/cprt.
fn mluc_tag(text: &str) -> Vec<u8> {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    let mut v = b"mluc".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&1u32.to_be_bytes()); // record count
    v.extend_from_slice(&12u32.to_be_bytes()); // record size
    v.extend_from_slice(b"enUS");
    v.extend_from_slice(&(utf16.len() as u32).to_be_bytes());
    v.extend_from_slice(&28u32.to_be_bytes()); // string offset from tag start (16 header + 12 record)
    v.extend_from_slice(&utf16);
    v
}

/// A v2 `desc` (textDescriptionType): ASCII count + NUL-terminated ASCII, then the zeroed Unicode
/// and ScriptCode records the type requires. v4's `mluc` is NOT legal in a v2 profile, and the v2
/// profile is what a web deliverable is expected to carry.
fn desc_tag_v2(text: &str) -> Vec<u8> {
    // ASCII-only by construction at the one call site; any stray non-ASCII byte would break the
    // count, so map it rather than emit a malformed tag.
    let ascii: Vec<u8> = text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect();
    let mut v = b"desc".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&((ascii.len() + 1) as u32).to_be_bytes()); // count INCLUDES the NUL
    v.extend_from_slice(&ascii);
    v.push(0);
    v.extend_from_slice(&[0u8; 4]); // Unicode language code
    v.extend_from_slice(&[0u8; 4]); // Unicode count (0 = no Unicode record)
    v.extend_from_slice(&[0u8; 2]); // ScriptCode code
    v.push(0); // ScriptCode count
    v.extend_from_slice(&[0u8; 67]); // the fixed 67-byte ScriptCode buffer
    v
}

/// A v2 `text` tag (7-bit ASCII, NUL-terminated) — the type v2 requires for `cprt`.
fn text_tag(text: &str) -> Vec<u8> {
    let mut v = b"text".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend(text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }));
    v.push(0);
    v
}

/// A `curv` curveType holding `n` uniformly-spaced u16 samples of `f` over 0..=1 — the v2-legal
/// tone-curve encoding (`para` is a v4 type). 1024 points is the canonical sRGB profile's own
/// resolution, and [`parse_display_icc`] reads exactly this shape back.
fn curv_tag(n: usize, f: impl Fn(f32) -> f32) -> Vec<u8> {
    let mut v = b"curv".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&(n as u32).to_be_bytes());
    for i in 0..n {
        let x = i as f32 / (n - 1) as f32;
        let y = (f(x).clamp(0.0, 1.0) * 65535.0).round() as u16;
        v.extend_from_slice(&y.to_be_bytes());
    }
    v
}

/// The ICC profile a SHIPPED file gets tagged with when its pixels are sRGB — the `./Web` export.
///
/// v0.8.104 (Round-A C3): [`icc_bytes_for_gamut`] exists for an ON-SCREEN surface (the macOS
/// CAMetalLayer colour-space tag), where nothing ever reads the description and "Falcon sRGB
/// (exact)" is an honest label for bytes we generated. A ./Web JPEG is the opposite case: it is the
/// CLIENT-FACING deliverable, it leaves the machine, and the artefact every consumer expects there
/// is the v2 **sRGB IEC61966-2.1** profile. Shipping a vendor-branded v4 profile made Photoshop
/// announce "Falcon sRGB (exact)" on every open (and prompt under the common Ask-When-Opening
/// setting), and put a `cprt` reading "No copyright — generated colour-space description" on a
/// photographer's licensed work. So the two consumers get two profiles, and this is the shipping one.
///
/// What is in it, and what is honestly claimed:
/// * **v2.4.0**, `mntr`/`RGB `/`XYZ `, tag table in ascending signature order (ICC §7.3.1).
/// * `desc` = **"sRGB IEC61966-2.1"** — the name of the colour space these bytes describe. The
///   COLORIMETRY is the standard's: sRGB's published primaries Bradford-adapted to the D50 PCS
///   (`chad` + D50 `wtpt`, the same pair [`parse_display_icc`] un-adapts), and the exact sRGB
///   piecewise TRC sampled to a 1024-point `curv`. It is not a byte-copy of HP's 1998 file — no
///   third-party profile is vendored into this tree — and it does not pretend to be one.
/// * `cprt` = **"Public Domain"** — the ordinary value for a colour-space description, and the one
///   claim we can actually make about bytes this crate generates. It says nothing about the image.
/// * The creation date is a FIXED constant, not "now": two exports of the same photo must be
///   byte-identical, and a wall-clock stamp would make the deliverable non-reproducible. The
///   profile ID stays zero — that field is OPTIONAL in v2 (v4 is where it is required).
///
/// The three TRC tags share ONE `curv` data block (the canonical profiles do this too), so the whole
/// profile is ~2.4 KB against a multi-hundred-KB JPEG.
pub fn srgb_icc_for_export() -> Vec<u8> {
    let m = Gamut::Srgb.rgb_to_xyz();
    let white = [
        m[0][0] + m[0][1] + m[0][2],
        m[1][0] + m[1][1] + m[1][2],
        m[2][0] + m[2][1] + m[2][2],
    ];
    let chad = bradford_to_d50(white);
    let m_d50 = mat3_mul(chad, m);
    let col = |c: usize| [m_d50[0][c], m_d50[1][c], m_d50[2][c]];
    let mut chad_body = b"sf32".to_vec();
    chad_body.extend_from_slice(&[0u8; 4]);
    for row in chad {
        for v in row {
            chad_body.extend_from_slice(&s15_be(v));
        }
    }
    // An ICC TRC maps DEVICE value → linear, so the sampled function is sRGB's LINEARISATION
    // (`srgb_to_linear`), not its encoding — same direction the `para` type-3 tag encodes, which is
    // why `parse_display_icc` recovers the same inverse LUT from either. (v0.8.105 / W7: the old
    // first clause said "linear → device, i.e. sRGB's ENCODING function" and then contradicted
    // itself in the same sentence; an edit that "fixed" the code to match it would have inverted the
    // gamma of every client-facing JPEG. The direction now has its own oracle in the test below,
    // read straight out of the tag bytes rather than through our own parser.)
    let trc = curv_tag(1024, srgb_to_linear);
    // Ascending signature order, so the tag table is spec-sorted without a later sort step.
    let entries: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"bTRC", Vec::new()), // placeholder — shares `rTRC`'s data block (patched below)
        (*b"bXYZ", xyz_tag(col(2))),
        (*b"chad", chad_body),
        (*b"cprt", text_tag("Public Domain")),
        (*b"desc", desc_tag_v2(SRGB_EXPORT_DESC)),
        (*b"gTRC", Vec::new()), // ditto
        (*b"gXYZ", xyz_tag(col(1))),
        (*b"rTRC", trc),
        (*b"rXYZ", xyz_tag(col(0))),
        (*b"wtpt", xyz_tag(D50_XYZ)),
    ];
    let mut icc = vec![0u8; 128];
    icc[8..12].copy_from_slice(&0x0240_0000u32.to_be_bytes()); // version 2.4.0
    icc[12..16].copy_from_slice(b"mntr");
    icc[16..20].copy_from_slice(b"RGB ");
    icc[20..24].copy_from_slice(b"XYZ ");
    // Creation date (24..36): a FIXED 2026-01-01T00:00:00Z, six big-endian u16s — see the doc above.
    for (i, v) in [2026u16, 1, 1, 0, 0, 0].iter().enumerate() {
        icc[24 + i * 2..26 + i * 2].copy_from_slice(&v.to_be_bytes());
    }
    icc[36..40].copy_from_slice(b"acsp");
    // rendering intent (64..68) stays 0 = perceptual, as shipped display profiles do.
    for (i, c) in D50_XYZ.iter().enumerate() {
        icc[68 + i * 4..72 + i * 4].copy_from_slice(&s15_be(*c));
    }
    let table_len = 4 + entries.len() * 12;
    let data_base = 128 + table_len;
    let mut data = Vec::new();
    let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(entries.len()); // (offset, size)
    let mut trc_slot: Option<(usize, usize)> = None;
    for (sig, d) in &entries {
        if d.is_empty() {
            offsets.push((0, 0)); // patched after rTRC lands
            continue;
        }
        let at = (data_base + data.len(), d.len());
        if sig == b"rTRC" {
            trc_slot = Some(at);
        }
        offsets.push(at);
        data.extend_from_slice(d);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    let trc_slot = trc_slot.expect("rTRC is in the entry table");
    for (i, (sig, _)) in entries.iter().enumerate() {
        if sig == b"gTRC" || sig == b"bTRC" {
            offsets[i] = trc_slot; // ONE curve block, three tags pointing at it
        }
    }
    icc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (sig, _)) in entries.iter().enumerate() {
        icc.extend_from_slice(sig);
        icc.extend_from_slice(&(offsets[i].0 as u32).to_be_bytes());
        icc.extend_from_slice(&(offsets[i].1 as u32).to_be_bytes());
    }
    icc.extend_from_slice(&data);
    let size = icc.len() as u32;
    icc[0..4].copy_from_slice(&size.to_be_bytes());
    icc
}

/// The `desc` string [`srgb_icc_for_export`] writes — the colour space's STANDARD name. Exported so
/// the export test can assert the shipped identity without re-typing the literal.
pub const SRGB_EXPORT_DESC: &str = "sRGB IEC61966-2.1";

/// Serialize falcon-color's EXACT encoding of a NAMED gamut as a matrix/TRC display ICC (v4).
/// `None` for [`Gamut::Custom`] — a custom profile already HAS its own bytes (the loaded file),
/// which are exact by construction; re-serializing our parse of them would only lose fidelity.
/// Round-trip-tested below: `parse_display_icc(icc_bytes_for_gamut(g))` recovers `g.rgb_to_xyz()`
/// and the gamut's tone curve.
///
/// v0.8.104 (Round-A C3): this is the ON-SCREEN serializer — the macOS CAMetalLayer colour-space
/// tag, where the description is never read by anything and "Falcon <gamut> (exact)" is the honest
/// label. A file that LEAVES the machine gets [`srgb_icc_for_export`] instead; do not reuse this one
/// for shipped bytes.
pub fn icc_bytes_for_gamut(g: Gamut) -> Option<Vec<u8>> {
    let m = match g {
        // Both of these ALREADY have their own bytes — the loaded display ICC, and (v0.8.177) the
        // file's embedded source profile. Re-serializing our parse of them could only lose fidelity.
        Gamut::Custom | Gamut::SourceIcc(_) => return None,
        _ => g.rgb_to_xyz(),
    };
    // The gamut's native white IS the matrix's column sums (absolute-XYZ architecture).
    let white = [
        m[0][0] + m[0][1] + m[0][2],
        m[1][0] + m[1][1] + m[1][2],
        m[2][0] + m[2][1] + m[2][2],
    ];
    let chad = bradford_to_d50(white);
    let m_d50 = mat3_mul(chad, m);
    // The tone curve, EXACTLY as `Trc` encodes it: the sRGB piecewise curve is para type 3 with the
    // spec constants; Adobe RGB and DCI-P3 are pure-gamma para type 0 from the shared constants.
    let trc = match g.trc_kind() {
        0 => para_tag(3, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.040_45]),
        1 => para_tag(0, &[ADOBE_GAMMA]),
        3 => para_tag(0, &[DCI_GAMMA]),
        _ => return None, // kind 2 = Custom, already returned above
    };
    let col = |c: usize| [m_d50[0][c], m_d50[1][c], m_d50[2][c]];
    let mut chad_body = b"sf32".to_vec();
    chad_body.extend_from_slice(&[0u8; 4]);
    for row in chad {
        for v in row {
            chad_body.extend_from_slice(&s15_be(v));
        }
    }
    let entries: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"desc", mluc_tag(&format!("Falcon {} (exact)", g.label()))),
        (*b"cprt", mluc_tag("No copyright — generated colour-space description")),
        (*b"wtpt", xyz_tag(D50_XYZ)), // v4: media white after chad-adaptation = the PCS illuminant
        (*b"chad", chad_body),
        (*b"rXYZ", xyz_tag(col(0))),
        (*b"gXYZ", xyz_tag(col(1))),
        (*b"bXYZ", xyz_tag(col(2))),
        (*b"rTRC", trc.clone()),
        (*b"gTRC", trc.clone()),
        (*b"bTRC", trc),
    ];
    // ── header (128 bytes) ──
    let mut icc = vec![0u8; 128];
    icc[8..12].copy_from_slice(&0x0420_0000u32.to_be_bytes()); // version 4.2
    icc[12..16].copy_from_slice(b"mntr"); // display device class
    icc[16..20].copy_from_slice(b"RGB ");
    icc[20..24].copy_from_slice(b"XYZ ");
    icc[36..40].copy_from_slice(b"acsp");
    // rendering intent (64..68) stays 0 (perceptual — matches shipped display profiles).
    for (i, c) in D50_XYZ.iter().enumerate() {
        icc[68 + i * 4..72 + i * 4].copy_from_slice(&s15_be(*c)); // PCS illuminant
    }
    // ── tag table + data (each tag 4-byte aligned) ──
    let table_len = 4 + entries.len() * 12;
    let data_base = 128 + table_len;
    let mut data = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for (_sig, d) in &entries {
        offsets.push(data_base + data.len());
        data.extend_from_slice(d);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    icc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (sig, d)) in entries.iter().enumerate() {
        icc.extend_from_slice(sig);
        icc.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
        icc.extend_from_slice(&(d.len() as u32).to_be_bytes());
    }
    icc.extend_from_slice(&data);
    let size = icc.len() as u32;
    icc[0..4].copy_from_slice(&size.to_be_bytes());
    Some(icc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_noop() {
        let mut px = vec![10u8, 128, 240, 255, 0, 64, 200, 128];
        let before = px.clone();
        transform_rgba(&mut px, Gamut::Srgb, Gamut::Srgb);
        assert_eq!(px, before);
    }

    /// v0.8.150 (J4) — **THE PARALLEL FLOOR IS A THROUGHPUT DECISION, NEVER A COLOUR ONE.**
    ///
    /// [`PAR_MIN_PX`] exists because a UI-thread transform of a 19 k-pixel frost mip was queueing
    /// behind eighteen decode workers' 48 MP buffers in the shared rayon pool — measured at 60.9 to
    /// 109.6 ms inside a step whose own arithmetic is ~0.1 ms. The gate that fixes it changes WHERE
    /// the per-pixel loop runs and nothing else: same matrix, same `Trc` snapshot, same
    /// [`apply_pixel`], same order, no accumulation across pixels. So the two arms must produce the
    /// SAME BYTES — not similar ones, the same ones — and a routing change that silently altered the
    /// colour of every frosted panel and every browse frame would be far worse than the stall it cured.
    ///
    /// Run over both a buffer BELOW the floor and one ABOVE it, in three gamut pairs (two analytic
    /// TRCs and the piecewise sRGB one), so the assertion covers both sides of the switch and every
    /// arm of `Trc` that a test can reach without a loaded ICC.
    ///
    /// FALSIFIER (L28): make either arm skip a pixel (e.g. `chunks_exact_mut(3)` on the RGBA path)
    /// and this fails on the first mismatched byte; make `PAR_MIN_PX` the decision-maker for the
    /// MATHS rather than the routing and it fails everywhere.
    #[test]
    fn the_parallel_floor_never_changes_a_pixel() {
        // A deterministic non-uniform ramp — a flat fill would pass even a badly broken split.
        let make = |px_count: usize, bpp: usize| -> Vec<u8> {
            (0..px_count * bpp).map(|i| ((i * 37 + i / 7) % 251) as u8).collect()
        };
        let pairs = [
            (Gamut::Srgb, Gamut::AdobeRgb),
            (Gamut::DisplayP3, Gamut::Srgb),
            (Gamut::AdobeRgb, Gamut::DciP3),
        ];
        // One below the floor (the UI thread's population) and one above it (the decode pool's).
        for &px_count in &[1024usize, PAR_MIN_PX + 512] {
            for (src, dst) in pairs {
                let base4 = make(px_count, 4);
                let (mut a, mut b) = (base4.clone(), base4.clone());
                transform_rgba_impl(&mut a, src, dst, false);
                transform_rgba_impl(&mut b, src, dst, true);
                assert_eq!(a, b, "rgba {src:?}->{dst:?} at {px_count} px: serial != parallel");
                assert_ne!(a, base4, "rgba {src:?}->{dst:?}: the transform did nothing at all");

                let base3 = make(px_count, 3);
                let (mut a, mut b) = (base3.clone(), base3.clone());
                transform_rgb_impl(&mut a, src, dst, false);
                transform_rgb_impl(&mut b, src, dst, true);
                assert_eq!(a, b, "rgb {src:?}->{dst:?} at {px_count} px: serial != parallel");
            }
        }
        // …and the public entry points route by pixel count, not by byte count: an RGB buffer of
        // `PAR_MIN_PX` pixels is 3/4 the bytes of an RGBA one and must still land on the same side.
        assert!(!wants_parallel(PAR_MIN_PX - 1));
        assert!(wants_parallel(PAR_MIN_PX));
        assert!(!wants_parallel(160 * 120), "the frosted backdrop's mip is below the floor");
        assert!(wants_parallel(8064 * 6048), "a 48 MP browse frame is above it");
    }

    #[test]
    fn srgb_to_p3_round_trips() {
        // sRGB → Display P3 → sRGB returns close to the original (validates the matrix INVERSE + TRC).
        // A wrong matrix drifts by tens; ≤4 here is pure 8-bit quantisation through the wide-gamut
        // intermediate at gamut-boundary primaries (real use is ONE-WAY, so this drift never accumulates).
        for &(r, g, b) in &[(255u8, 0, 0), (0, 255, 0), (0, 0, 255), (128, 64, 200), (250, 250, 250)] {
            let mut px = [r, g, b, 255u8];
            transform_rgba(&mut px, Gamut::Srgb, Gamut::DisplayP3);
            transform_rgba(&mut px, Gamut::DisplayP3, Gamut::Srgb);
            for (i, &orig) in [r, g, b].iter().enumerate() {
                let diff = (px[i] as i32 - orig as i32).abs();
                assert!(diff <= 4, "round-trip drift {diff} for {:?} channel {i}", (r, g, b));
            }
        }
    }

    #[test]
    fn transform_rgb8_matches_bulk_and_is_identity_on_srgb() {
        // v0.8.48: the single-triple UI-chrome entry point. Identity when src == dst (byte-exact —
        // the sRGB-output guarantee for the design tokens), and byte-identical to the bulk
        // transform_rgba path for named gamuts (same apply_pixel, proven not asserted-by-construction).
        for &rgb in &[[0x3fu8, 0x8c, 0xff], [0x00, 0xd7, 0xc8], [0x17, 0x17, 0x17], [0xff, 0xff, 0xff], [0x00, 0x00, 0x00]] {
            assert_eq!(transform_rgb8(rgb, Gamut::Srgb, Gamut::Srgb), rgb, "sRGB→sRGB must be identity");
            for dst in [Gamut::DisplayP3, Gamut::AdobeRgb, Gamut::Rec2020] {
                let mut px = [rgb[0], rgb[1], rgb[2], 0x77];
                transform_rgba(&mut px, Gamut::Srgb, dst);
                assert_eq!(
                    transform_rgb8(rgb, Gamut::Srgb, dst),
                    [px[0], px[1], px[2]],
                    "single-triple vs bulk mismatch for {rgb:?} → {dst:?}"
                );
            }
        }
    }

    #[test]
    fn srgb_red_into_p3_desaturates() {
        // Pure sRGB red sits INSIDE the P3 gamut, so encoded in P3 it must be a less-saturated red
        // (green/blue rise above 0; red stays high). Sanity-checks direction, not just round-trip.
        let mut px = [255u8, 0, 0, 255];
        transform_rgba(&mut px, Gamut::Srgb, Gamut::DisplayP3);
        assert!(px[0] > 200, "red channel should stay high, got {}", px[0]);
        assert!(px[1] > 5, "green should lift above zero, got {}", px[1]);
    }

    // ─── DCI-P3 (v0.8.67) — the one non-D65 named gamut; pins the native-white-referenced arm ───

    /// The hardcoded published matrix must equal the textbook derivation from the spec primaries +
    /// the DCI white: `W = (x/y, 1, (1-x-y)/y)`, solve `P·S = W`, `M = P·diag(S)` — NO adaptation
    /// (the architecture's native-white referencing; a Bradford-to-D65 matrix would differ in row 1
    /// by ~2% and fail this).
    #[test]
    fn dcip3_matrix_matches_primaries_plus_dci_white_derivation() {
        let (xr, yr) = (0.680f64, 0.320f64);
        let (xg, yg) = (0.265f64, 0.690f64);
        let (xb, yb) = (0.150f64, 0.060f64);
        let (xw, yw) = (0.3140f64, 0.3510f64);
        let col = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
        let (r, g, b) = (col(xr, yr), col(xg, yg), col(xb, yb));
        let w = col(xw, yw);
        // Solve the 3×3 system P·s = w by hand (f64 Cramer via the adjugate of P's columns r,g,b).
        let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let det = p[0][0] * (p[1][1] * p[2][2] - p[1][2] * p[2][1])
            - p[0][1] * (p[1][0] * p[2][2] - p[1][2] * p[2][0])
            + p[0][2] * (p[1][0] * p[2][1] - p[1][1] * p[2][0]);
        let rep = |c: usize| {
            let mut m = p;
            for (row, mr) in m.iter_mut().enumerate() {
                mr[c] = w[row];
            }
            m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
        };
        let s = [rep(0) / det, rep(1) / det, rep(2) / det];
        let m = Gamut::DciP3.rgb_to_xyz();
        for row in 0..3 {
            for cidx in 0..3 {
                let want = p[row][cidx] * s[cidx];
                let got = m[row][cidx] as f64;
                assert!(
                    (got - want).abs() < 2e-4,
                    "DciP3 matrix [{row}][{cidx}]: got {got} want {want}"
                );
            }
        }
    }

    /// White (1,1,1) in DciP3 must map to the DCI white's XYZ — column sums = (0.894587, 1, 0.954416).
    /// This IS the architecture's white handling: the matrix is native-white-referenced (unadapted),
    /// exactly like a Custom ICC's colorants after the chad un-adaptation.
    #[test]
    fn dcip3_white_maps_to_native_dci_white_xyz() {
        let m = Gamut::DciP3.rgb_to_xyz();
        let sums: Vec<f32> = (0..3).map(|i| m[i][0] + m[i][1] + m[i][2]).collect();
        for (got, want) in sums.iter().zip([0.894_587, 1.0, 0.954_416]) {
            assert!((got - want).abs() < 1e-4, "DCI white XYZ: got {got} want {want}");
        }
    }

    #[test]
    fn dcip3_trc_is_pure_gamma_26() {
        assert_eq!(Gamut::DciP3.trc_kind(), 3, "GPU shader kind 3 = the DCI gamma arm");
        assert!((DCI_GAMMA - 2.6).abs() < 1e-6);
        // from_i32 round-trip at the new settings index (0..4 unchanged).
        assert_eq!(Gamut::from_i32(5), Gamut::DciP3);
        assert_eq!(Gamut::DciP3.label(), "DCI-P3");
    }

    /// The architecture does NOT adapt to a common white — it preserves absolute chromaticity. So a
    /// D65 (sRGB) grey encodes NON-neutral in DciP3: green suppressed vs red/blue (compensating the
    /// greenish DCI white so the SCREEN shows physical D65 grey), while the same grey through the
    /// D65-white DisplayP3 stays exactly neutral in the same harness. Values cross-checked by hand
    /// against inv(M_dci)·XYZ(D65 grey) + gamma-2.6 encode: #808080 → (146, 139, 149).
    #[test]
    fn dcip3_grey_compensates_dci_white_p3_stays_neutral() {
        // DisplayP3 (D65): neutral in, neutral out — the common-white behaviour, for contrast.
        let mut p3 = [128u8, 128, 128, 255];
        transform_rgba(&mut p3, Gamut::Srgb, Gamut::DisplayP3);
        assert_eq!([p3[0], p3[1], p3[2]], [128, 128, 128], "P3 (D65) must keep grey neutral");
        // DciP3 (DCI white): green dips below red/blue — the absolute-colorimetric compensation.
        let mut px = [128u8, 128, 128, 255];
        transform_rgba(&mut px, Gamut::Srgb, Gamut::DciP3);
        for (c, want) in px[..3].iter().zip([146u8, 139, 149]) {
            assert!((*c as i32 - want as i32).abs() <= 1, "DciP3 grey: got {px:?} want ~(146,139,149)");
        }
        assert!(px[1] < px[0] && px[1] < px[2], "green must be suppressed vs red/blue");
        // Pure D65 WHITE is outside the DCI-white display's full-drive gamut on R/B → clips to 255
        // with green pulled just below (relative-colorimetric-with-clipping, the pipeline's intent).
        let mut w = [255u8, 255, 255, 255];
        transform_rgba(&mut w, Gamut::Srgb, Gamut::DciP3);
        assert_eq!(w[0], 255);
        assert_eq!(w[2], 255);
        assert!((249..=253).contains(&w[1]), "white's green ≈ 251, got {}", w[1]);
    }

    /// sRGB → DCI-P3 → sRGB round-trips within quantisation for IN-GAMUT (non-clipping) colours —
    /// the matrix-inverse + gamma-2.6 TRC mirror of `srgb_to_p3_round_trips`. Near-white greys are
    /// deliberately absent: they CLIP on R/B under the DCI white (see the grey/white test above),
    /// so a round-trip there measures the clip, not the matrix.
    #[test]
    fn srgb_to_dcip3_round_trips_in_gamut() {
        for &(r, g, b) in &[(200u8, 100, 50), (128, 64, 200), (100, 150, 90), (60, 60, 60)] {
            let mut px = [r, g, b, 255u8];
            transform_rgba(&mut px, Gamut::Srgb, Gamut::DciP3);
            transform_rgba(&mut px, Gamut::DciP3, Gamut::Srgb);
            for (i, &orig) in [r, g, b].iter().enumerate() {
                let diff = (px[i] as i32 - orig as i32).abs();
                assert!(diff <= 4, "round-trip drift {diff} for {:?} channel {i}", (r, g, b));
            }
        }
        // And the single-triple UI-chrome entry point rides the same maths (the transform_rgb8
        // bulk-parity loop above doesn't enumerate DciP3 — cover it here).
        let mut px = [0x3f, 0x8c, 0xff, 0x77];
        transform_rgba(&mut px, Gamut::Srgb, Gamut::DciP3);
        assert_eq!(
            transform_rgb8([0x3f, 0x8c, 0xff], Gamut::Srgb, Gamut::DciP3),
            [px[0], px[1], px[2]],
            "single-triple vs bulk mismatch for the accent → DciP3"
        );
    }

    #[test]
    fn inverse_lut_inverts_gamma() {
        // The analytic gamma LUT must invert the forward gamma: encode(linearize(x)) ≈ x.
        let lut = inverse_lut_for(&ToneCurve::Gamma(2.2));
        for code in [8u8, 16, 32, 64, 128, 200, 255] {
            let x = code as f32 / 255.0;
            let lin = x.powf(2.2);
            let back = lut_encode(&lut, lin) * 255.0;
            assert!((back - code as f32).abs() <= 1.0, "gamma LUT round-trip {code} -> {back}");
        }
    }

    #[test]
    fn para_type3_matches_srgb_curve() {
        // The sRGB EOTF is exactly a `para` type-3 curve — parse it and confirm eval reproduces the
        // analytic sRGB linearise across the range (proves the parametric evaluator + its inversion).
        let p = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045, 0.0, 0.0];
        for code in 0..=255u32 {
            let x = code as f32 / 255.0;
            assert!((para_eval(3, &p, x) - srgb_to_linear(x)).abs() < 1e-3, "para type3 vs sRGB at {code}");
        }
    }

    // ─── custom-ICC parsing ───

    /// Bradford chromatic adaptation XYZ_D65 → XYZ_D50 (the well-known matrix). Used to synthesise a
    /// realistic `chad` tag so the parser's D50→device-white re-referencing is exercised.
    const BRADFORD_D65_TO_D50: [[f32; 3]; 3] = [
        [1.047_811_2, 0.022_886_6, -0.050_127_0],
        [0.029_542_4, 0.990_484_4, -0.017_049_1],
        [-0.009_234_5, 0.015_043_6, 0.752_131_6],
    ];

    fn s15_bytes(v: f32) -> [u8; 4] {
        ((v * 65536.0).round() as i32).to_be_bytes()
    }

    /// Build a minimal-but-valid matrix/TRC ICC. `curv` is the raw curv-tag body (type sig + reserved +
    /// count + u16 samples) used verbatim for r/g/bTRC; `chad` (sf32) is optional. Enough for
    /// `parse_display_icc`.
    fn build_matrix_icc_raw(m: [[f32; 3]; 3], chad: Option<[[f32; 3]; 3]>, curv: Vec<u8>) -> Vec<u8> {
        let xyz_tag = |col: usize| -> Vec<u8> {
            let mut v = b"XYZ ".to_vec();
            v.extend_from_slice(&[0u8; 4]);
            for row in 0..3 {
                v.extend_from_slice(&s15_bytes(m[row][col]));
            }
            v
        };
        let mut entries: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"rXYZ", xyz_tag(0)),
            (*b"gXYZ", xyz_tag(1)),
            (*b"bXYZ", xyz_tag(2)),
            (*b"rTRC", curv.clone()),
            (*b"gTRC", curv.clone()),
            (*b"bTRC", curv),
        ];
        if let Some(c) = chad {
            let mut v = b"sf32".to_vec();
            v.extend_from_slice(&[0u8; 4]);
            for row in c {
                for val in row {
                    v.extend_from_slice(&s15_bytes(val));
                }
            }
            entries.push((*b"chad", v));
        }
        let table_len = 4 + entries.len() * 12;
        let data_base = 128 + table_len;
        let mut data = Vec::new();
        let mut offsets = Vec::new();
        for (_sig, d) in &entries {
            offsets.push(data_base + data.len());
            data.extend_from_slice(d);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        let mut out = vec![0u8; 128]; // header (parser reads only the tag count at 128)
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (i, (sig, d)) in entries.iter().enumerate() {
            out.extend_from_slice(sig);
            out.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
            out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(&data);
        out
    }

    /// A single-value `curv` (one u8Fixed8 gamma).
    fn curv_gamma(gamma: f32) -> Vec<u8> {
        let mut v = b"curv".to_vec();
        v.extend_from_slice(&[0u8; 4]);
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
        v
    }

    /// A sampled `curv` table from a forward device→linear fn (the measured case).
    fn curv_table(n: usize, f: impl Fn(f32) -> f32) -> Vec<u8> {
        let mut v = b"curv".to_vec();
        v.extend_from_slice(&[0u8; 4]);
        v.extend_from_slice(&(n as u32).to_be_bytes());
        for i in 0..n {
            let val = (f(i as f32 / (n - 1) as f32) * 65535.0).round().clamp(0.0, 65535.0) as u16;
            v.extend_from_slice(&val.to_be_bytes());
        }
        v
    }

    fn build_matrix_icc(m: [[f32; 3]; 3], chad: Option<[[f32; 3]; 3]>, gamma: f32) -> Vec<u8> {
        build_matrix_icc_raw(m, chad, curv_gamma(gamma))
    }

    fn mat_close(a: [[f32; 3]; 3], b: [[f32; 3]; 3], tol: f32) -> bool {
        (0..3).all(|i| (0..3).all(|j| (a[i][j] - b[i][j]).abs() <= tol))
    }

    #[test]
    fn parse_no_chad_adapts_d50_pcs_to_d65() {
        // A v2/no-chad profile stores colorants in the D50 PCS. Build M_d50 = chad(D65→D50)·sRGB_D65 and
        // OMIT the chad tag; the parser must adapt D50→D65 and recover the sRGB primaries (the white-point
        // fix proven on AdobeRGB1998.icc — a bare D50 matrix would render warm).
        let m_d65 = Gamut::Srgb.rgb_to_xyz();
        let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, m_d65);
        let icc = build_matrix_icc(m_d50, None, 2.2);
        let p = parse_display_icc(&icc, "test").expect("matrix profile parses");
        assert!(mat_close(p.rgb_to_xyz, m_d65, 5e-3), "recovered {:?} vs {:?}", p.rgb_to_xyz, m_d65);
        assert!((p.gamma - 2.2).abs() < 0.02, "gamma {}", p.gamma);
        assert_eq!(p.label, "test");
    }

    #[test]
    fn parse_chad_adapts_d50_colorants_back_to_d65() {
        // Store the colorants in the D50 PCS (M_d50 = chad · M_d65) with a real chad; the parser must
        // undo it (inv(chad) · M_d50) and recover the original D65 primaries — the trickiest path.
        let m_d65 = Gamut::Srgb.rgb_to_xyz();
        let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, m_d65);
        let icc = build_matrix_icc(m_d50, Some(BRADFORD_D65_TO_D50), 2.4);
        let p = parse_display_icc(&icc, "chad").expect("parses");
        assert!(mat_close(p.rgb_to_xyz, m_d65, 2e-3), "recovered {:?} vs {:?}", p.rgb_to_xyz, m_d65);
        assert!((p.gamma - 2.4).abs() < 0.02, "gamma {}", p.gamma);
    }

    #[test]
    fn parse_rejects_non_matrix_and_truncated() {
        // A profile with a tag table but no colorant tags (a LUT/cLUT profile) → None (unsupported).
        let curv_only = build_matrix_icc(Gamut::Srgb.rgb_to_xyz(), None, 2.2);
        // Corrupt the rXYZ signature so no colorant is found.
        let mut no_colorants = curv_only.clone();
        // tag table starts at 132; first entry sig at 132 — clobber it.
        no_colorants[132..136].copy_from_slice(b"XXXX");
        assert!(parse_display_icc(&no_colorants, "x").is_none());
        // Truncated buffer → None, no panic.
        assert!(parse_display_icc(&curv_only[..100], "x").is_none());
        assert!(parse_display_icc(&[], "x").is_none());
    }

    #[test]
    fn parse_keeps_measured_curve_not_single_gamma() {
        // A 1024-point sRGB-piecewise curv is NOT a pure gamma. The old code reduced it to its midpoint
        // gamma (~2.22) and mis-fit the shadows; the LUT must keep the real curve. Verify: encode of the
        // linearised sRGB value round-trips (curve == sRGB ⇒ inverse-LUT-encode(srgb_linear(x)) ≈ x),
        // while the midpoint-gamma path would lift it.
        let icc = build_matrix_icc_raw(Gamut::Srgb.rgb_to_xyz(), None, curv_table(1024, srgb_to_linear));
        let p = parse_display_icc(&icc, "srgb-curve").expect("parses");
        let lut = &p.enc_lut[..CUSTOM_LUT_N];
        for code in [16u8, 32, 64, 128, 192] {
            let x = code as f32 / 255.0;
            let back = lut_encode(lut, srgb_to_linear(x)) * 255.0;
            assert!((back - code as f32).abs() <= 1.0, "measured-curve LUT off at {code}: got {back}");
            // The buggy single-gamma path (x_lin^(1/2.22)) lifts the shadows; document the gap at #16.
            if code == 16 {
                let single = srgb_to_linear(x).powf(1.0 / p.gamma) * 255.0;
                assert!(single - code as f32 >= 4.0, "single-gamma should lift #16 by >=4, got {single}");
            }
        }
        assert!(p.trc_summary.contains("curv[1024]"), "summary {}", p.trc_summary);
    }

    #[test]
    fn custom_profile_end_to_end() {
        // The ONLY unit test that mutates the process-global custom profile — kept in ONE function so it
        // can't race the other (parallel) tests, none of which touch `Gamut::Custom`.
        assert_eq!(Gamut::from_i32(4), Gamut::Custom);

        // ── directional sanity: a P3-primaries custom profile behaves like sRGB→P3 ──
        let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, Gamut::DisplayP3.rgb_to_xyz());
        let icc = build_matrix_icc(m_d50, Some(BRADFORD_D65_TO_D50), 2.2);
        let prof = parse_display_icc(&icc, "MyMonitor").expect("parses");
        let gen0 = custom_profile_gen();
        set_custom_profile(prof);
        assert_eq!(custom_profile_gen(), gen0 + 1, "set_custom_profile bumps the generation");
        assert!(CUSTOM.read().unwrap().is_some(), "custom profile installed");
        assert_eq!(CUSTOM.read().unwrap().as_ref().unwrap().label, "MyMonitor");
        assert!(custom_encode_lut().is_some(), "encode LUT exposed for the GPU/parity path");
        let mut px = [255u8, 0, 0, 255];
        transform_rgba(&mut px, Gamut::Srgb, Gamut::Custom);
        assert!(px[0] > 200, "red should stay high, got {}", px[0]);
        assert!(px[1] > 5, "green should lift above zero, got {}", px[1]);
        assert_eq!(Gamut::Custom.trc_kind(), 2);

        // ── reference vectors: a synthetic Display-P3-primaries + 1024-pt sRGB-curv profile, expected
        // values from littleCMS (PIL ImageCms, sRGB → synthetic, relative-colorimetric, no BPC). The
        // dark grays are the headline (single-gamma lifted #101010 → #181818; the LUT lands #101010). ──
        let icc = build_matrix_icc_raw(
            mat3_mul(BRADFORD_D65_TO_D50, Gamut::DisplayP3.rgb_to_xyz()),
            Some(BRADFORD_D65_TO_D50),
            curv_table(1024, srgb_to_linear),
        );
        let prof = parse_display_icc(&icc, "synth").expect("parses");
        set_custom_profile(prof);
        // (input 0xRRGGBB, expected [R,G,B]) — littleCMS reference (scratchpad ref_gen.py).
        let cases: &[(u32, [u8; 3])] = &[
            (0x101010, [16, 16, 16]),   // near-black gray (the +8 shadow-lift probe)
            (0x202020, [32, 32, 32]),   // dark gray
            (0x404040, [64, 64, 64]),   // low-mid gray
            (0x808080, [128, 128, 128]), // mid gray
            (0xc0c0c0, [192, 192, 192]), // light gray
            (0x000000, [0, 0, 0]),      // black
            (0xffffff, [255, 255, 255]), // white
            (0x3f8cff, [83, 138, 247]), // blue accent
            (0x00d7c8, [97, 212, 200]), // teal accent-web
            (0xe5484d, [212, 84, 83]),  // red danger
            (0xff8040, [239, 135, 80]), // warm sunset midtone
        ];
        for &(hex, exp) in cases {
            let mut px = [((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8, 255];
            transform_rgba(&mut px, Gamut::Srgb, Gamut::Custom);
            for c in 0..3 {
                let d = px[c] as i32 - exp[c] as i32;
                assert!(d.abs() <= 1, "#{hex:06x} ch{c}: got {} want {} (Δ{d})", px[c], exp[c]);
            }
            // v0.8.48: the single-triple UI-chrome entry point rides the SAME custom-LUT path —
            // byte-identical to the bulk transform for every reference vector.
            let rgb8 = transform_rgb8(
                [((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8],
                Gamut::Srgb,
                Gamut::Custom,
            );
            assert_eq!(rgb8, [px[0], px[1], px[2]], "transform_rgb8 vs bulk for #{hex:06x}");
        }

        // ── optional integration: the owner's REAL Dell profile, if present on this machine. Same gate:
        // #101010 → #101010 ±1 (littleCMS reference). Gracefully skips when the file is absent. ──
        let dell = std::path::Path::new(r"C:\Windows\System32\spool\drivers\color\Dell_S2725QS_Native_v2.icm");
        if let Ok(bytes) = std::fs::read(dell) {
            if let Some(prof) = parse_display_icc(&bytes, "Dell") {
                set_custom_profile(prof);
                // littleCMS relcol/no-BPC references for the real Dell (scratchpad cross-check).
                let dell_cases: &[(u32, [u8; 3])] = &[
                    (0x101010, [16, 16, 16]),
                    (0x202020, [32, 32, 32]),
                    (0x404040, [64, 64, 64]),
                    (0x808080, [128, 128, 128]),
                    (0x3f8cff, [86, 137, 252]),
                    (0x00d7c8, [52, 212, 199]),
                ];
                for &(hex, exp) in dell_cases {
                    let mut px = [((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8, 255];
                    transform_rgba(&mut px, Gamut::Srgb, Gamut::Custom);
                    for c in 0..3 {
                        assert!(
                            (px[c] as i32 - exp[c] as i32).abs() <= 1,
                            "Dell #{hex:06x} ch{c}: got {} want {}",
                            px[c],
                            exp[c]
                        );
                    }
                }
                // v0.8.48 (the UI-token transform's Dell proof): the single-triple chrome entry point
                // reproduces the owner's v0.8.46 hand-sampled dims BYTE-EXACTLY from the ORIGINAL
                // design values — accent #3f8cff → #5689fc, accent-web #00d7c8 → #34d4c7 — so
                // reverting the tokens + transforming at runtime shows him the identical pixels.
                assert_eq!(
                    transform_rgb8([0x3f, 0x8c, 0xff], Gamut::Srgb, Gamut::Custom),
                    [0x56, 0x89, 0xfc],
                    "Dell: design accent must transform to the v0.8.46 hand-dim exactly"
                );
                assert_eq!(
                    transform_rgb8([0x00, 0xd7, 0xc8], Gamut::Srgb, Gamut::Custom),
                    [0x34, 0xd4, 0xc7],
                    "Dell: design accent-web must transform to the v0.8.46 hand-dim exactly"
                );
            }
        }
    }

    // ─── ICC serialization (v0.9.21 — the macOS CAMetalLayer colorspace tag) ───

    /// Every named gamut's serialized ICC must ROUND-TRIP through our own parser: the recovered
    /// matrix equals the gamut's absolute native-white matrix (proves the chad ↔ un-adapt pair),
    /// and the recovered white (column sums) equals the native white — D65 for four gamuts, the
    /// DCI white for DciP3. This is the exact-by-construction claim the Mac layer tag rests on.
    #[test]
    fn serialized_icc_round_trips_matrix_and_white() {
        for g in [Gamut::Srgb, Gamut::DisplayP3, Gamut::AdobeRgb, Gamut::Rec2020, Gamut::DciP3] {
            let icc = icc_bytes_for_gamut(g).expect("named gamut serializes");
            assert_eq!(&icc[36..40], b"acsp", "ICC magic");
            assert_eq!(&icc[12..16], b"mntr", "display class");
            let p = parse_display_icc(&icc, "rt").unwrap_or_else(|| panic!("{g:?} parses"));
            let want = g.rgb_to_xyz();
            assert!(
                mat_close(p.rgb_to_xyz, want, 2e-3),
                "{g:?} recovered {:?} vs {:?}",
                p.rgb_to_xyz,
                want
            );
            let white: Vec<f32> = (0..3).map(|i| want[i][0] + want[i][1] + want[i][2]).collect();
            let got: Vec<f32> =
                (0..3).map(|i| p.rgb_to_xyz[i][0] + p.rgb_to_xyz[i][1] + p.rgb_to_xyz[i][2]).collect();
            for (a, b) in got.iter().zip(&white) {
                assert!((a - b).abs() < 2e-3, "{g:?} white recovery: got {got:?} want {white:?}");
            }
        }
        // Custom deliberately refuses — its own loaded file IS the exact bytes.
        assert!(icc_bytes_for_gamut(Gamut::Custom).is_none());
    }

    /// v0.8.104 (Round-A C3): the SHIPPING sRGB profile — the one that leaves the machine on every
    /// ./Web JPEG. Three separable claims: it IDENTIFIES itself as the standard colour space (not as
    /// Falcon), it is a v2 profile with a v2-legal tag set, and it is still colorimetrically the same
    /// sRGB the pixels were converted into (so the honest name is not a lie about the numbers).
    ///
    /// FALSIFIERS (L28): point `srgb_icc_for_export` back at `icc_bytes_for_gamut(Srgb)` and the desc
    /// row fails on "Falcon sRGB (exact)" AND the version row fails on 4.2; emit `mluc`/`para` (the v4
    /// types) and the tag-type rows fail; drop the `chad` tag or the D50 adaptation and the matrix
    /// round-trip fails; leave the tag table unsorted and the ascending-order row fails; stamp a
    /// wall-clock creation date and the byte-reproducibility row fails. v0.8.105 (W7): sample the
    /// curve with `linear_to_srgb` instead of `srgb_to_linear` — the inversion the old comment above
    /// `curv_tag` invited — and the direction-oracle rows fail; the round-trip row below CANNOT
    /// catch that (it re-reads our own table through our own parser).
    #[test]
    fn the_shipped_srgb_profile_names_the_standard_and_is_v2() {
        let icc = srgb_icc_for_export();
        assert_eq!(&icc[36..40], b"acsp", "ICC magic");
        assert_eq!(&icc[12..16], b"mntr", "display class");
        assert_eq!(u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize, icc.len());
        // ── v2, not v4: the artefact a web consumer expects ──
        assert_eq!(icc[8], 0x02, "must be a v2 profile (got major {})", icc[8]);
        // ── the identity, read out of the tag table exactly as a consumer would ──
        let tag_count = u32::from_be_bytes([icc[128], icc[129], icc[130], icc[131]]) as usize;
        let mut sigs: Vec<[u8; 4]> = Vec::new();
        let mut tags: Vec<([u8; 4], usize, usize)> = Vec::new();
        for k in 0..tag_count {
            let e = 132 + k * 12;
            let sig = [icc[e], icc[e + 1], icc[e + 2], icc[e + 3]];
            let off = u32::from_be_bytes([icc[e + 4], icc[e + 5], icc[e + 6], icc[e + 7]]) as usize;
            let size = u32::from_be_bytes([icc[e + 8], icc[e + 9], icc[e + 10], icc[e + 11]]) as usize;
            assert!(off >= 128 && off + size <= icc.len(), "tag {sig:?} is out of bounds");
            sigs.push(sig);
            tags.push((sig, off, size));
        }
        let mut sorted = sigs.clone();
        sorted.sort_unstable();
        assert_eq!(sigs, sorted, "ICC §7.3.1: the tag table must be in ascending signature order");
        let tag = |want: &[u8; 4]| -> (usize, usize) {
            tags.iter().find(|(s, _, _)| s == want).map(|&(_, o, n)| (o, n)).expect("tag present")
        };
        let (doff, dsize) = tag(b"desc");
        assert_eq!(&icc[doff..doff + 4], b"desc", "v2 desc must be textDescriptionType, not mluc");
        let ascii_n =
            u32::from_be_bytes([icc[doff + 8], icc[doff + 9], icc[doff + 10], icc[doff + 11]])
                as usize;
        let name = String::from_utf8_lossy(&icc[doff + 12..doff + 12 + ascii_n - 1]).to_string();
        assert_eq!(
            name, SRGB_EXPORT_DESC,
            "the shipped deliverable must name the STANDARD colour space, not the app"
        );
        assert!(!name.contains("Falcon"), "no vendor branding on a client-facing file: {name}");
        assert!(dsize >= 12 + ascii_n + 15, "the textDescriptionType trailer must be present");
        let (coff, _) = tag(b"cprt");
        assert_eq!(&icc[coff..coff + 4], b"text", "v2 cprt must be textType, not mluc");
        let cprt = String::from_utf8_lossy(&icc[coff + 8..])
            .split('\0')
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(cprt, "Public Domain", "the cprt must be an ordinary colour-space claim");
        assert!(!cprt.contains("No copyright"), "the odd generated-description string is gone");
        // ── v2-legal curve type, and ONE shared block for the three channels ──
        for s in [b"rTRC", b"gTRC", b"bTRC"] {
            let (o, _) = tag(s);
            assert_eq!(&icc[o..o + 4], b"curv", "v2 TRC must be curveType (para is a v4 type)");
        }
        assert_eq!(tag(b"rTRC"), tag(b"gTRC"), "the three TRC tags share one curve block");
        assert_eq!(tag(b"rTRC"), tag(b"bTRC"));
        // ── v0.8.105 (W7): the DIRECTION ORACLE, read straight out of the curve block ──
        // An ICC `curv` holds device → LINEAR. The round-trip below goes through
        // `parse_display_icc`, which treats whatever table it finds as authoritative — it would
        // recover an INVERTED curve just as happily — so it cannot pin the direction. This can:
        // sample [512] of the 1024-point table is the linearisation of 512/1023 (≈0.216), and it is
        // nowhere near the encoding of the same value (≈0.735).
        {
            let (o, _) = tag(b"rTRC");
            let n = u32::from_be_bytes([icc[o + 8], icc[o + 9], icc[o + 10], icc[o + 11]]) as usize;
            assert_eq!(n, 1024, "the shipped curve is the 1024-point table");
            let s = u16::from_be_bytes([icc[o + 12 + 512 * 2], icc[o + 12 + 512 * 2 + 1]]) as f32
                / 65535.0;
            let x = 512.0 / 1023.0;
            let (lin, enc) = (srgb_to_linear(x), linear_to_srgb(x));
            assert!(
                (s - lin).abs() < 1e-3,
                "curv[512] = {s} must be srgb_to_linear({x}) = {lin} — an ICC TRC is device→linear"
            );
            assert!(
                (s - enc).abs() > 0.1,
                "curv[512] = {s} is the ENCODING function {enc} — the shipped profile's gamma is \
                 inverted, and every ./Web JPEG with it"
            );
        }
        // ── the numbers are still sRGB: our own parser recovers the matrix and the curve ──
        let p = parse_display_icc(&icc, "ship").expect("the shipped profile must parse");
        let want = Gamut::Srgb.rgb_to_xyz();
        assert!(mat_close(p.rgb_to_xyz, want, 2e-3), "recovered {:?} vs {want:?}", p.rgb_to_xyz);
        let lut = &p.enc_lut[..CUSTOM_LUT_N];
        for code in [8u8, 16, 32, 64, 128, 192, 240] {
            let back = lut_encode(lut, srgb_to_linear(code as f32 / 255.0)) * 255.0;
            assert!((back - code as f32).abs() <= 1.0, "sRGB curve at {code}: got {back}");
        }
        // ── deterministic: the same photo exported twice must produce the same file ──
        assert_eq!(icc, srgb_icc_for_export(), "the profile bytes must be reproducible");
    }

    /// The serialized TRC must be OUR encoding curve, not an approximation: parsing the profile and
    /// encoding a linearised value through its inverse LUT returns the original device value. For the
    /// sRGB-family gamuts (incl. Rec.2020 — the gamut with NO matching CG named space) the curve is
    /// the sRGB piecewise; Adobe/DCI are their pure gammas.
    #[test]
    fn serialized_icc_trc_matches_encoding() {
        // sRGB-family (the para type-3 arm) — probe via the piecewise linearise.
        for g in [Gamut::Srgb, Gamut::DisplayP3, Gamut::Rec2020] {
            let icc = icc_bytes_for_gamut(g).expect("serializes");
            let p = parse_display_icc(&icc, "trc").expect("parses");
            let lut = &p.enc_lut[..CUSTOM_LUT_N];
            for code in [8u8, 16, 32, 64, 128, 192, 240] {
                let x = code as f32 / 255.0;
                let back = lut_encode(lut, srgb_to_linear(x)) * 255.0;
                assert!(
                    (back - code as f32).abs() <= 1.0,
                    "{g:?} sRGB-curve round-trip at {code}: got {back}"
                );
            }
        }
        // Pure-gamma family (the para type-0 arm) — the recovered effective gamma is the constant.
        for (g, gamma) in [(Gamut::AdobeRgb, ADOBE_GAMMA), (Gamut::DciP3, DCI_GAMMA)] {
            let icc = icc_bytes_for_gamut(g).expect("serializes");
            let p = parse_display_icc(&icc, "trc").expect("parses");
            assert!((p.gamma - gamma).abs() < 1e-3, "{g:?} gamma: got {} want {gamma}", p.gamma);
            let lut = &p.enc_lut[..CUSTOM_LUT_N];
            for code in [16u8, 64, 128, 200] {
                let x = code as f32 / 255.0;
                let back = lut_encode(lut, x.powf(gamma)) * 255.0;
                assert!(
                    (back - code as f32).abs() <= 1.0,
                    "{g:?} gamma round-trip at {code}: got {back}"
                );
            }
        }
    }

    /// The Bradford CAT: adapting the D65-anchored sRGB matrix to D50 and back is identity, and the
    /// adapted matrix's white is exactly D50 (what the ICC PCS stores).
    #[test]
    fn bradford_to_d50_is_consistent() {
        let m = Gamut::Srgb.rgb_to_xyz();
        let white = [
            m[0][0] + m[0][1] + m[0][2],
            m[1][0] + m[1][1] + m[1][2],
            m[2][0] + m[2][1] + m[2][2],
        ];
        let chad = bradford_to_d50(white);
        let m_d50 = mat3_mul(chad, m);
        let w50: Vec<f32> = (0..3).map(|i| m_d50[i][0] + m_d50[i][1] + m_d50[i][2]).collect();
        for (got, want) in w50.iter().zip(D50_XYZ) {
            assert!((got - want).abs() < 1e-4, "adapted white {w50:?} vs D50");
        }
        let back = mat3_mul(mat3_inv(chad), m_d50);
        assert!(mat_close(back, m, 1e-5), "chad un-adapt must be the exact inverse");
    }

    // ───────── v0.8.140 (THE COLOR ROUND): resolution by colorimetry, not by name ─────────

    /// Re-describe a serialized profile: append a fresh `mluc` block and re-point the `desc` tag
    /// entry at it. Tag data need not be contiguous or ordered in an ICC — the tag TABLE locates
    /// every block — so this keeps the colorimetry tags byte-identical while accepting a name of any
    /// length (the probe's in-place rename can only shrink the original slot).
    fn renamed(mut icc: Vec<u8>, desc: &str) -> Vec<u8> {
        let n = icc_tag_count(&icc).expect("a profile we serialized");
        let e = (0..n)
            .map(|k| 132 + k * 12)
            .find(|&e| &icc[e..e + 4] == b"desc")
            .expect("icc_bytes_for_gamut writes a desc tag");
        while icc.len() % 4 != 0 {
            icc.push(0);
        }
        let (off, tag) = (icc.len(), mluc_tag(desc));
        icc.extend_from_slice(&tag);
        icc[e + 4..e + 8].copy_from_slice(&(off as u32).to_be_bytes());
        icc[e + 8..e + 12].copy_from_slice(&(tag.len() as u32).to_be_bytes());
        let size = icc.len() as u32;
        icc[0..4].copy_from_slice(&size.to_be_bytes());
        icc
    }

    fn icc_named(g: Gamut, desc: &str) -> Vec<u8> {
        renamed(icc_bytes_for_gamut(g).expect("a modeled gamut serializes"), desc)
    }

    /// C1.2 / C4(f) — THE DISCRIMINABILITY TEST, the measurement τ is derived FROM.
    ///
    /// Two bounds must hold at once for a tolerance to mean anything: the modeled gamuts must be far
    /// enough apart that no profile can be ambiguous (every pair > k·τ with k ≥ 5), and the encoding
    /// noise of a real ICC round-trip must be far INSIDE τ so an exactly-standard profile cannot miss
    /// its own gamut. This measures both and prints the numbers.
    #[test]
    fn tau_is_discriminating() {
        let mut closest = (Gamut::Srgb, Gamut::Srgb, f32::INFINITY);
        for (i, a) in SOURCE_GAMUTS.iter().enumerate() {
            for b in &SOURCE_GAMUTS[i + 1..] {
                let d = colorant_distance(a.rgb_to_xyz(), b.rgb_to_xyz());
                eprintln!("pair {:>10} ↔ {:<10} d = {d:.6}", a.label(), b.label());
                if d < closest.2 {
                    closest = (*a, *b, d);
                }
            }
        }
        let k = closest.2 / GAMUT_MATCH_TOL;
        eprintln!(
            "closest modeled pair: {} ↔ {} at {:.6} = {k:.2}·τ  (τ = {GAMUT_MATCH_TOL})",
            closest.0.label(),
            closest.1.label(),
            closest.2
        );
        assert!(
            k >= 5.0,
            "τ = {GAMUT_MATCH_TOL} leaves only {k:.2}·τ between {} and {} — a profile could be \
             ambiguous between two modeled gamuts",
            closest.0.label(),
            closest.1.label()
        );

        // The other side: serialize each gamut as a REAL ICC (s15Fixed16 fixed point, chad-adapted
        // into the D50 PCS) and measure it back. That is the whole encoding noise a standard profile
        // carries, and it must be a small fraction of τ.
        let mut worst_noise = 0.0f32;
        for g in SOURCE_GAMUTS {
            let icc = icc_bytes_for_gamut(g).expect("a modeled gamut serializes");
            let m = icc_colorants_d65(&icc).expect("our own serialization is matrix/TRC");
            let d = colorant_distance(m, g.rgb_to_xyz());
            eprintln!("round-trip noise {:>10}: {d:.8}", g.label());
            worst_noise = worst_noise.max(d);
            assert_eq!(Gamut::from_icc_bytes(&icc), Some(g), "{} must resolve to itself", g.label());
        }
        eprintln!(
            "worst round-trip noise {worst_noise:.8} = τ/{:.0}",
            GAMUT_MATCH_TOL / worst_noise.max(f32::MIN_POSITIVE)
        );
        assert!(
            worst_noise * 50.0 < GAMUT_MATCH_TOL,
            "fixed-point round-trip noise {worst_noise} is not comfortably inside τ = {GAMUT_MATCH_TOL}"
        );
    }

    /// C1.5 — THE STRICTLY-BETTER INVARIANT. Every input the OLD name-only path resolved to a named
    /// gamut still resolves to that same gamut. Pairs each name the old matcher knew with the
    /// COLORIMETRY a profile of that name really carries, and checks the new answer against the old
    /// one. The single designed exception (a mislabeled profile) has its own test below.
    #[test]
    fn colorimetry_never_loses_a_name_the_old_path_knew() {
        // (description the old path resolved, the colorimetry a profile so described really carries)
        let cases: &[(&str, Option<Gamut>)] = &[
            ("sRGB IEC61966-2.1", Some(Gamut::Srgb)),
            ("sRGB", Some(Gamut::Srgb)),
            ("sRGB v4 ICC preference perceptual", Some(Gamut::Srgb)),
            ("Adobe RGB (1998)", Some(Gamut::AdobeRgb)),
            ("AdobeRGB1998", Some(Gamut::AdobeRgb)),
            ("Display P3", Some(Gamut::DisplayP3)),
            ("Display-P3", Some(Gamut::DisplayP3)),
            ("Apple Display P3", Some(Gamut::DisplayP3)),
            ("P3-D65", Some(Gamut::DisplayP3)),
            ("Rec. 2020", Some(Gamut::Rec2020)),
            ("ITU-R BT.2020", Some(Gamut::Rec2020)),
            // Names the old path knew for which we have no modeled colorimetry to serialize: the
            // bytes-less half of the domain, where the new path IS the old path.
            ("ProPhoto RGB", None),
            ("DCI-P3", None),
        ];
        for (desc, carries) in cases {
            let old = Gamut::from_description(desc).expect("the table lists names the old path knew");
            // (a) with no bytes at all — the enumerated-tag / EXIF door.
            let bytes_less = resolve_source_gamut(None, Some(desc));
            assert_eq!(bytes_less.gamut, old, "{desc:?} with no profile must still resolve {old:?}");
            assert_eq!(bytes_less.route, GamutRoute::Description);
            // (b) with the bytes a profile of that name really carries.
            if let Some(g) = carries {
                let icc = icc_named(*g, desc);
                let r = resolve_source_gamut(Some(&icc), Some(desc));
                assert_eq!(
                    r.gamut, old,
                    "{desc:?} resolved {old:?} by name before this round and must still — got {:?} via {:?}",
                    r.gamut, r.route
                );
                assert_eq!(r.route, GamutRoute::Colorimetry, "{desc:?} should now answer from its bytes");
            }
        }
        // And a true-DCI profile — colorants nowhere near any candidate — keeps the deliberate
        // v0.8.67 name mapping (files labelled DCI-P3 are near-always P3-D65 content).
        let dci = icc_bytes_for_gamut(Gamut::DciP3).expect("DciP3 serializes");
        let (near, d) = Gamut::nearest_from_icc_bytes(&dci).expect("it is a matrix profile");
        assert!(d > GAMUT_MATCH_TOL, "true DCI-P3 must MISS every source candidate (nearest {near:?} at {d})");
        assert_eq!(resolve_source_gamut(Some(&dci), Some("DCI-P3 D65")).gamut, Gamut::DisplayP3);
        assert_eq!(Gamut::from_icc_bytes(&dci), None, "DciP3 is destination-only — never resolved as a source");
    }

    /// C4(g) — THE DESIGNED EXCEPTION: a profile whose NAME says Display P3 over colorants that are
    /// plain sRGB now follows the bytes. This is the only way the new path may differ from the old.
    #[test]
    fn a_mislabeled_profile_follows_its_bytes() {
        let icc = icc_named(Gamut::Srgb, "Display P3");
        assert_eq!(
            Gamut::from_description("Display P3"),
            Some(Gamut::DisplayP3),
            "the name alone still says P3 — that is what makes this a falsifier"
        );
        let r = resolve_source_gamut(Some(&icc), Some("Display P3"));
        assert_eq!(r.gamut, Gamut::Srgb, "the colorants are sRGB; the bytes win");
        assert_eq!(r.route, GamutRoute::Colorimetry);
    }

    /// C4(e) — a LUT-class profile (an `A2B0` and no `rXYZ`) cannot be measured, so the name decides;
    /// and when the name is unrecognised too, sRGB with an honest reason.
    #[test]
    fn a_lut_class_profile_falls_back_to_the_name() {
        let icc = lut_class_icc();
        assert_eq!(icc_colorants_d65(&icc), None, "a cLUT profile has no colorant tags to measure");
        assert_eq!(Gamut::from_icc_bytes(&icc), None);

        let named = resolve_source_gamut(Some(&icc), Some("Adobe RGB (1998)"));
        assert_eq!(named.gamut, Gamut::AdobeRgb);
        assert_eq!(named.route, GamutRoute::Description);
        assert!(named.unreadable_profile, "the bytes were there and would not parse — say so");
        assert_eq!(named.nearest, None);

        let blind = resolve_source_gamut(Some(&icc), Some("Display"));
        assert_eq!(blind.gamut, Gamut::Srgb);
        assert_eq!(blind.route, GamutRoute::Fallback);
        assert_eq!(blind.why(), "sRGB default — profile not matrix/TRC, name unrecognised");
        eprintln!("LUT-class, unrecognised name → {} ({})", blind.gamut.label(), blind.why());
    }

    /// A minimal LUT/cLUT-class display profile: a valid header and tag table carrying an `A2B0`
    /// (and a `desc`), with NO `rXYZ`/`gXYZ`/`bXYZ` — the shape `parse_display_icc` has always
    /// refused, and the shape the source side must now refuse the same way.
    fn lut_class_icc() -> Vec<u8> {
        let a2b0 = {
            let mut v = b"mft1".to_vec(); // lut8Type — a real signature, deliberately not matrix/TRC
            v.extend_from_slice(&[0u8; 44]);
            v
        };
        let desc = mluc_tag("Generic LUT Display");
        let entries: Vec<([u8; 4], Vec<u8>)> = vec![(*b"desc", desc), (*b"A2B0", a2b0)];
        let mut icc = vec![0u8; 128];
        icc[8..12].copy_from_slice(&0x0420_0000u32.to_be_bytes());
        icc[12..16].copy_from_slice(b"mntr");
        icc[16..20].copy_from_slice(b"RGB ");
        icc[20..24].copy_from_slice(b"XYZ ");
        icc[36..40].copy_from_slice(b"acsp");
        let data_base = 128 + 4 + entries.len() * 12;
        let mut data = Vec::new();
        let mut offsets = Vec::new();
        for (_s, d) in &entries {
            offsets.push(data_base + data.len());
            data.extend_from_slice(d);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        icc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (i, (sig, d)) in entries.iter().enumerate() {
            icc.extend_from_slice(sig);
            icc.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
            icc.extend_from_slice(&(d.len() as u32).to_be_bytes());
        }
        icc.extend_from_slice(&data);
        let size = icc.len() as u32;
        icc[0..4].copy_from_slice(&size.to_be_bytes());
        icc
    }

    /// The v0.8.140 split must not have changed what `parse_display_icc` computes: its colorants are
    /// EXACTLY `icc_colorants_d65`'s, on every profile we can serialize.
    ///
    /// v0.8.141 (R6/A6) — THE WORDING NARROWED TO WHAT IS RUN. This test used to close by claiming
    /// "the two agree about which profiles are matrix/TRC at all", which is broader than the three
    /// inputs it actually checks AND broader than the truth: the two readers deliberately DISAGREE
    /// on a profile that carries colorants but no usable tone curve. That corner is now a row of
    /// its own rather than a sentence nobody tested.
    #[test]
    fn the_colorant_split_did_not_change_parse_display_icc() {
        for g in [Gamut::Srgb, Gamut::DisplayP3, Gamut::AdobeRgb, Gamut::Rec2020, Gamut::DciP3] {
            let icc = icc_bytes_for_gamut(g).expect("serializes");
            let full = parse_display_icc(&icc, "t").expect("matrix/TRC");
            let just = icc_colorants_d65(&icc).expect("matrix/TRC");
            assert_eq!(full.rgb_to_xyz, just, "{}: the two readers must agree bit for bit", g.label());
        }
        // A LUT-class profile and malformed input are refused by BOTH, without panicking.
        let lut = lut_class_icc();
        assert!(parse_display_icc(&lut, "t").is_none() && icc_colorants_d65(&lut).is_none());
        for bad in [vec![], vec![0u8; 8], vec![0xFFu8; 200]] {
            assert!(parse_display_icc(&bad, "t").is_none() && icc_colorants_d65(&bad).is_none());
        }
    }

    /// v0.8.141 (R6/A6) — THE CORNER THE TWO READERS ARE MEANT TO DISAGREE ON: colorants present,
    /// no usable `rTRC`/`gTRC`/`bTRC`.
    ///
    /// The SOURCE side answers, and must: a file's primaries are what name its gamut, and the
    /// transform linearises a source with the MODELED gamut's analytic curve, so the profile's own
    /// curve is never consulted. The DESTINATION side must refuse, because encoding INTO a display
    /// is exactly where the missing curve would be needed. `(Some, None)` is the designed shape.
    #[test]
    fn a_profile_with_colorants_but_no_tone_curve_answers_the_source_and_refuses_the_display() {
        let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, Gamut::DisplayP3.rgb_to_xyz());
        let mut icc = build_matrix_icc(m_d50, None, 2.2);
        // Entries are written r/g/bXYZ then r/g/bTRC — clobber the three TRC signatures in the tag
        // table so no channel curve can be found, leaving the colorant tags byte-untouched.
        for k in 3..6 {
            let e = 132 + k * 12;
            assert!(icc[e..e + 4].ends_with(b"TRC"), "entry {k} should be a TRC tag");
            icc[e..e + 4].copy_from_slice(b"XXXX");
        }
        assert!(
            icc_colorants_d65(&icc).is_some(),
            "the source side must read colorants without a tone curve — that is the whole point of \
             the v0.8.140 split"
        );
        assert_eq!(Gamut::from_icc_bytes(&icc), Some(Gamut::DisplayP3), "…and still place them");
        assert!(
            parse_display_icc(&icc, "t").is_none(),
            "the destination side has no curve to encode with and must refuse"
        );
    }

    /// v0.8.141 (R6/M6) — THE ICC v2 / NO-`chad` CLASS, END TO END.
    ///
    /// Every other fixture in this round rides `icc_bytes_for_gamut`, which always writes ICC v4
    /// WITH a `chad` tag. The profiles a Windows machine actually ships (`sRGB Color Space
    /// Profile.icm`, `AdobeRGB1998.icc` — the very files the v0.8.140 commit message quotes
    /// measurements from) are the OTHER shape: v2, colorants stored in the D50 PCS, no `chad`, so
    /// the fixed Bradford arm runs instead of the inverse-chad one. `parse_no_chad_adapts_d50_pcs_to_d65`
    /// pins that arm's arithmetic; nothing pinned that a profile of this shape actually RESOLVES.
    ///
    /// The gamut list is spelled out rather than taken from `SOURCE_GAMUTS` on purpose: if a future
    /// round adds a gamut to that array, this row must fail to compile-or-be-updated rather than
    /// silently start claiming something else (the `DciP3` trap — it is destination-only and would
    /// never resolve here).
    #[test]
    fn a_v2_no_chad_profile_resolves_its_gamut_end_to_end() {
        for g in [Gamut::Srgb, Gamut::DisplayP3, Gamut::AdobeRgb, Gamut::Rec2020] {
            let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, g.rgb_to_xyz());
            let icc = build_matrix_icc(m_d50, None, 2.2);
            let (near, d) = Gamut::nearest_from_icc_bytes(&icc).expect("a v2 matrix/TRC profile parses");
            eprintln!("v2/no-chad {:>10}: nearest {} at d = {d:.6} (τ = {GAMUT_MATCH_TOL})", g.label(), near.label());
            assert_eq!(
                Gamut::from_icc_bytes(&icc),
                Some(g),
                "a v2/no-chad {} profile must resolve {} — nearest was {} at {d}",
                g.label(),
                g.label(),
                near.label()
            );
        }
    }

    /// v0.8.141 (R9) — THE CRAFTED-`chad` TABLE. Four profiles that differ ONLY in their `chad`
    /// tag, over byte-identical sRGB colorants. Rows 1-3 all resolved to SOMETHING before this
    /// round; row 4 is the control that proves the guards did not simply refuse everything.
    ///
    /// FALSIFIERS: delete the det guard and row 1 answers (the identity arm keeps the D50
    /// colorants); delete the result bound and rows 2-3 answer with colorant entries in the
    /// hundreds; delete both and all three come back.
    #[test]
    fn a_degenerate_chad_is_refused_rather_than_silently_absorbed() {
        let m_d65 = Gamut::Srgb.rgb_to_xyz();
        let m_d50 = mat3_mul(BRADFORD_D65_TO_D50, m_d65);
        let scale = |s: f32| [[s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]];
        let rows: &[(&str, [[f32; 3]; 3], bool)] = &[
            // (1) SINGULAR — det 0. `mat3_inv` answers the IDENTITY here, so the profile used to
            //     keep its D50-PCS colorants and be measured as if they were D65.
            ("all-zero chad (det 0)", [[0.0; 3]; 3], false),
            // (2) UNIFORMLY NEAR-SINGULAR — det 1e-9, PAST mat3_inv's 1e-12 floor, so it really was
            //     inverted: 1e-3·I → 1e3·I, and every colorant grew by a thousand.
            ("uniform 1e-3 chad (det 1e-9)", scale(1e-3), false),
            // (3) DEGENERATE ON ONE AXIS ONLY — det 1e-3, a perfectly healthy determinant. No det
            //     threshold can see this one; the RESULT bound is what catches it.
            ("one-axis 1e-3 chad (det 1e-3)", [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1e-3]], false),
            // (4) THE CONTROL — a real Bradford chad, which must still parse to sRGB exactly as it
            //     did before. Without this row the whole test would pass by refusing everything.
            ("real Bradford D65→D50 chad", BRADFORD_D65_TO_D50, true),
        ];
        for (name, chad, want_ok) in rows {
            let icc = build_matrix_icc(m_d50, Some(*chad), 2.2);
            let got = icc_colorants_d65(&icc);
            match got {
                Some(m) => {
                    let (near, d) = nearest_source_gamut(m);
                    eprintln!("chad {name:<30} → colorants worst |el| {:.3}, nearest {} at {d:.4}",
                        m.iter().flatten().fold(0.0f32, |a, v| a.max(v.abs())), near.label());
                }
                None => eprintln!("chad {name:<30} → refused (unreadable profile)"),
            }
            assert_eq!(
                got.is_some(),
                *want_ok,
                "chad row {name:?}: expected {}, got {got:?}",
                if *want_ok { "a parse" } else { "a refusal" }
            );
            if *want_ok {
                assert_eq!(Gamut::from_icc_bytes(&icc), Some(Gamut::Srgb), "the control must still place sRGB");
                // …and the destination reader agrees, which is where the declared coupling bites.
                assert!(parse_display_icc(&icc, "monitor").is_some(), "a good chad still loads as a display profile");
            } else {
                assert!(
                    parse_display_icc(&icc, "monitor").is_none(),
                    "DECLARED COUPLING: a monitor profile with chad {name:?} now REFUSES to load \
                     (the IccFailure card) instead of loading with a colour cast"
                );
            }
        }
    }

    /// v0.8.141 (R14) — the true-DCI miss margin, MEASURED, because the v0.8.140 comment and commit
    /// message both quoted it wrong ("~0.14, twice the nearest-pair distance"). The real numbers:
    /// nearest candidate **sRGB** at **0.08045**, i.e. **1.16×** the nearest modeled pair. The
    /// v0.8.67 name mapping (files labelled DCI-P3 are P3-D65 content) rests on this miss.
    #[test]
    fn dci_p3_misses_by_its_measured_margin() {
        let (near, d) = nearest_source_gamut(Gamut::DciP3.rgb_to_xyz());
        let (mut pa, mut pb, mut pair) = (Gamut::Srgb, Gamut::Srgb, f32::INFINITY);
        for (i, a) in SOURCE_GAMUTS.iter().enumerate() {
            for b in &SOURCE_GAMUTS[i + 1..] {
                let dd = colorant_distance(a.rgb_to_xyz(), b.rgb_to_xyz());
                if dd < pair {
                    (pa, pb, pair) = (*a, *b, dd);
                }
            }
        }
        eprintln!(
            "true DCI-P3: nearest source candidate {} at {d:.5}; nearest modeled pair {} ↔ {} at \
             {pair:.5} ⇒ the miss is {:.2}× the pair distance",
            near.label(),
            pa.label(),
            pb.label(),
            d / pair
        );
        assert_eq!(near, Gamut::Srgb, "the nearest candidate is sRGB, not Display P3");
        assert!((d - 0.080_45).abs() < 5e-4, "the DCI miss margin is 0.08045, measured {d}");
        assert!((d / pair - 1.16).abs() < 0.02, "…which is 1.16× the nearest pair, not twice");
        assert!(d > GAMUT_MATCH_TOL, "and it is comfortably outside τ, which is what matters");
    }

    /// A profile far from everything we model (ProPhoto's colorants) must MISS — that is what keeps
    /// `from_description`'s deliberate ProPhoto → Rec.2020 mapping alive.
    ///
    /// v0.8.141 (R5/M5) — REBUILT, because the v0.8.140 constant was not ProPhoto at ANY illuminant.
    /// Its rows 1 and 2 were the published D50 ROMM matrix verbatim while row 0 was something else,
    /// so the implied white point was no illuminant at all, the printed margin ("Adobe RGB at
    /// 0.1801") was fictitious, and the test never touched the parser. Now the D65 matrix is DERIVED
    /// here by the crate's own Bradford constant from the published D50 ROMM primaries, and the same
    /// primaries are serialized as a real v2/no-`chad` ICC and routed through `icc_colorants_d65` —
    /// so the row measures the shipping path, and both routes must agree.
    #[test]
    fn prophoto_colorants_miss_every_modeled_gamut() {
        // ROMM RGB (ProPhoto) as PUBLISHED — referenced to its native D50 white, which is how every
        // real ProPhoto ICC stores it.
        const PROPHOTO_D50: [[f32; 3]; 3] = [
            [0.797_675, 0.135_192, 0.031_353],
            [0.288_040, 0.711_874, 0.000_086],
            [0.000_000, 0.000_000, 0.825_210],
        ];
        let d65 = mat3_mul(BRADFORD_D50_TO_D65, PROPHOTO_D50);
        let (g, d) = nearest_source_gamut(d65);
        eprintln!("ProPhoto (Bradford D50→D65 here): nearest {} at d = {d:.5} (τ = {GAMUT_MATCH_TOL})", g.label());
        assert_eq!(g, Gamut::Rec2020, "Rec.2020 is the closest thing we model to ProPhoto");
        assert!((d - 0.1186).abs() < 2e-3, "the ProPhoto → Rec.2020 margin is ~0.1186, measured {d}");
        assert!(d > GAMUT_MATCH_TOL, "ProPhoto must not be mistaken for {} at {d}", g.label());

        // …and through the PARSER, on bytes shaped like a real ProPhoto profile (v2, D50, no chad).
        let icc = build_matrix_icc(PROPHOTO_D50, None, 1.8);
        let (pg, pd) = Gamut::nearest_from_icc_bytes(&icc).expect("a matrix/TRC profile");
        eprintln!("ProPhoto (through icc_colorants_d65): nearest {} at d = {pd:.5}", pg.label());
        assert_eq!(pg, g, "the parser and the hand-adapted matrix must agree on the nearest gamut");
        assert!((pd - d).abs() < 1e-3, "…and on the distance ({pd} vs {d})");
        assert_eq!(Gamut::from_icc_bytes(&icc), None, "so a ProPhoto file falls through to its NAME");
        // v0.8.177 — THIS ROW NOW GUARDS ROUTE (2)'s ENTRY, not a Rec.2020 mapping. The miss is
        // still the fact it asserts (and must stay: a ProPhoto profile that PASSED τ would be
        // rendered as Rec.2020 by the colorimetry route, which is the defect). What changed is what
        // the miss leads to — `prophoto_renders_through_its_own_profile_not_as_rec2020` below.
        let r = resolve_source_gamut(Some(&icc), Some("ProPhoto RGB"));
        assert_eq!(r.route, GamutRoute::Faithful, "the miss now opens the faithful route, not the name");
        assert_ne!(r.gamut, Gamut::Rec2020, "and the answer is no longer the Rec.2020 approximation");
    }

    // ───────────── v0.8.177 — THE PROFILE-FAITHFUL SOURCE ROUTE ─────────────

    /// ROMM RGB (ProPhoto) as PUBLISHED — D50-referenced, which is how every real ProPhoto ICC
    /// stores it. Shared by the rows below and by `prophoto_colorants_miss_every_modeled_gamut`'s
    /// twin constant on purpose: if one is ever "corrected" the two stop agreeing and both fail.
    const PROPHOTO_D50: [[f32; 3]; 3] = [
        [0.797_675, 0.135_192, 0.031_353],
        [0.288_040, 0.711_874, 0.000_086],
        [0.000_000, 0.000_000, 0.825_210],
    ];

    /// The gamma a `curv` N=1 tag can actually carry: u8Fixed8, so 1.8 stores as 461/256. The
    /// reference pipeline must use THIS number, not 1.8, or the row would measure the encoding's
    /// rounding instead of the transform.
    const PROPHOTO_GAMMA_ENCODED: f32 = 461.0 / 256.0;

    /// `build_matrix_icc_raw` with a v2 `desc` tag — the name a faithful source is shown under.
    fn build_named_matrix_icc(m: [[f32; 3]; 3], curv: Vec<u8>, name: &str) -> Vec<u8> {
        // Build the plain profile, then RE-EMIT it with one extra tag. Re-emitting (rather than
        // appending) is what keeps every offset in the table honest — a hand-patched offset is how a
        // fixture ends up testing the parser's tolerance for its own bugs.
        let icc = build_matrix_icc_raw(m, None, curv);
        let mut desc_body = b"desc".to_vec();
        desc_body.extend_from_slice(&[0u8; 4]);
        let bytes = name.as_bytes();
        desc_body.extend_from_slice(&((bytes.len() + 1) as u32).to_be_bytes());
        desc_body.extend_from_slice(bytes);
        desc_body.push(0);
        // Table entry count lives at 128; data follows the table. Re-emit the whole profile with one
        // extra entry so every offset stays honest.
        let n = u32::from_be_bytes([icc[128], icc[129], icc[130], icc[131]]) as usize;
        let mut entries: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for k in 0..n {
            let e = 132 + k * 12;
            let sig: [u8; 4] = icc[e..e + 4].try_into().unwrap();
            let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().unwrap()) as usize;
            let sz = u32::from_be_bytes(icc[e + 8..e + 12].try_into().unwrap()) as usize;
            entries.push((sig, icc[off..off + sz].to_vec()));
        }
        entries.push((*b"desc", desc_body));
        let table_len = 4 + entries.len() * 12;
        let data_base = 128 + table_len;
        let (mut data, mut offsets) = (Vec::new(), Vec::new());
        for (_s, d) in &entries {
            offsets.push(data_base + data.len());
            data.extend_from_slice(d);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        let mut icc = icc;
        icc.truncate(128);
        icc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (i, (sig, d)) in entries.iter().enumerate() {
            icc.extend_from_slice(sig);
            icc.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
            icc.extend_from_slice(&(d.len() as u32).to_be_bytes());
        }
        icc.extend_from_slice(&data);
        icc
    }

    /// The CIE L\* companding curve, device → linear — the shape the owner's real LStar-RGB-v2 /
    /// LStar-RGB-v4 profiles carry as a `curv[1024]` table (they are the files that produced the
    /// 08-08 log's "resolving to the sRGB default at 0.03733 from Adobe RGB" lines).
    fn lstar_forward(d: f32) -> f32 {
        let l = d * 100.0;
        if l > 8.0 {
            ((l + 16.0) / 116.0).powi(3)
        } else {
            l / 903.3
        }
    }

    /// A HAND-COMPUTED reference transform: linearise with the profile's own curve, matrix into the
    /// destination, encode with the destination's curve, quantise. Deliberately written out here
    /// rather than composed from the crate's own helpers — a reference that calls the code under
    /// test proves nothing.
    fn reference_pixel(src_d65: [[f32; 3]; 3], lin: impl Fn(f32) -> f32, dst: Gamut, px: [u8; 3]) -> [u8; 3] {
        let m = mat3_mul(mat3_inv(dst.rgb_to_xyz()), src_d65);
        let l = [lin(px[0] as f32 / 255.0), lin(px[1] as f32 / 255.0), lin(px[2] as f32 / 255.0)];
        let mut out = [0u8; 3];
        for (r, o) in out.iter_mut().enumerate() {
            let v = (m[r][0] * l[0] + m[r][1] * l[1] + m[r][2] * l[2]).clamp(0.0, 1.0);
            let enc = match dst {
                Gamut::AdobeRgb => v.powf(1.0 / ADOBE_GAMMA),
                Gamut::DciP3 => v.powf(1.0 / DCI_GAMMA),
                _ => {
                    if v <= 0.003_130_8 {
                        v * 12.92
                    } else {
                        1.055 * v.powf(1.0 / 2.4) - 0.055
                    }
                }
            };
            *o = (enc * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        out
    }

    /// A spread of test pixels: greys, primaries, saturated mixes, and the near-black codes where a
    /// tone-curve error is largest in relative terms.
    fn probe_pixels() -> Vec<[u8; 3]> {
        let mut v = vec![[0, 0, 0], [1, 1, 1], [4, 2, 8], [16, 16, 16], [128, 128, 128], [255, 255, 255]];
        for i in 0..=16u32 {
            let c = (i * 255 / 16) as u8;
            v.push([c, (255 - c as u32) as u8, (c as u32 * 2 % 256) as u8]);
        }
        v
    }

    /// Compare the SHIPPING transform against a hand-computed reference over `pixels`, returning the
    /// worst per-channel 8-bit difference.
    fn worst_delta(src: Gamut, dst: Gamut, src_d65: [[f32; 3]; 3], lin: impl Fn(f32) -> f32) -> i32 {
        let mut worst = 0i32;
        for p in probe_pixels() {
            let mut got = [p[0], p[1], p[2], 255u8];
            transform_rgba(&mut got, src, dst);
            let want = reference_pixel(src_d65, &lin, dst, p);
            for ch in 0..3 {
                worst = worst.max((got[ch] as i32 - want[ch] as i32).abs());
            }
        }
        worst
    }

    /// **THE HEADLINE ROW.** A ProPhoto-tagged image renders through the profile's OWN colorants and
    /// its OWN gamma-1.8 curve, matching a hand-computed reference — and NOT through the Rec.2020
    /// approximation, whose error against the same reference this row also measures so the fix's
    /// size is on the record rather than in a commit message.
    ///
    /// FALSIFIER: delete route (2) from `resolve_source_gamut`. The gamut becomes `Rec2020`, the
    /// faithful assertion fails, and the second half of this row shows exactly what the owner
    /// measured on his weic2212a pair.
    #[test]
    fn prophoto_renders_through_its_own_profile_not_as_rec2020() {
        let icc = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.8), "ProPhoto RGB");
        let r = resolve_source_gamut(Some(&icc), Some("ProPhoto RGB"));
        assert_eq!(r.route, GamutRoute::Faithful, "a matrix-TRC profile no modeled gamut fits");
        assert!(r.gamut.is_source_profile(), "…resolves to its own registry entry, got {:?}", r.gamut);
        assert_eq!(r.gamut.display_name(), "ProPhoto RGB", "and it is shown under its own name");

        // The reference's source matrix is the crate's own D50→D65 Bradford applied to the PUBLISHED
        // primaries — the same adaptation `icc_colorants_d65` performs for a v2/no-chad profile, but
        // computed here from the constants rather than by calling it.
        let src_d65 = mat3_mul(BRADFORD_D50_TO_D65, PROPHOTO_D50);
        let lin = |c: f32| c.max(0.0).powf(PROPHOTO_GAMMA_ENCODED);

        for dst in [Gamut::Srgb, Gamut::AdobeRgb, Gamut::Rec2020] {
            let d = worst_delta(r.gamut, dst, src_d65, lin);
            eprintln!("faithful ProPhoto → {}: worst |Δ| = {d}/255 vs the reference", dst.label());
            assert!(d <= 1, "the faithful path must match the reference to ±1 code, got {d} for {:?}", dst);
        }

        // …and what the OLD answer cost, measured against the same reference. The name fallback is
        // still reachable (a LUT-class profile named prophoto), so this is not a dead comparison.
        let mut old_worst = 0i32;
        for p in probe_pixels() {
            let mut got = [p[0], p[1], p[2], 255u8];
            transform_rgba(&mut got, Gamut::Rec2020, Gamut::Srgb);
            let want = reference_pixel(src_d65, lin, Gamut::Srgb, p);
            for ch in 0..3 {
                old_worst = old_worst.max((got[ch] as i32 - want[ch] as i32).abs());
            }
        }
        eprintln!("the RETIRED prophoto→Rec.2020 approximation: worst |Δ| = {old_worst}/255");
        assert!(old_worst > 10, "the approximation must be visibly wrong, else this fix is pointless");
    }

    /// The other real shape in this class: an L\*-companded working space whose TRC is a
    /// `curv[1024]` sampled table, not a gamma. The owner's own LStar-RGB-v2/v4 files are these, and
    /// the 08-08 log shows them resolving to the sRGB DEFAULT at 0.03733 from Adobe RGB — i.e. this
    /// class was landing on the floor, not even on the approximation.
    ///
    /// The table path is the one that can go wrong quietly: it is inverted nowhere and interpolated
    /// twice (1024 samples → the 4096-entry forward LUT → the 8-bit fetch), so the row pins the
    /// result against a reference that evaluates the SAME table directly.
    #[test]
    fn lstar_class_curv_table_source_renders_faithfully() {
        // Primaries need only be un-modelable; the point of the row is the CURVE. Use ProPhoto's so
        // the profile is guaranteed to miss τ, and an L*-companded 1024-point table.
        let icc = build_named_matrix_icc(PROPHOTO_D50, curv_table(1024, lstar_forward), "LStar-RGB-v2.icc");
        let r = resolve_source_gamut(Some(&icc), Some("LStar-RGB-v2.icc"));
        assert_eq!(r.route, GamutRoute::Faithful);
        let p = source_profile(r.gamut).expect("registered");
        assert!(p.trc_summary.starts_with("curv[1024]"), "the table must survive as a table: {}", p.trc_summary);
        assert_eq!(r.gamut.display_name(), "LStar-RGB-v2.icc");

        // The reference reads the 1024-point table with the same linear interpolation the forward
        // LUT build uses — quantised through u16 exactly as the tag stores it.
        // Quantised exactly as `curv_table` writes the tag: `(v * 65535).round()`.
        let tbl: Vec<f32> = (0..1024)
            .map(|i| ((lstar_forward(i as f32 / 1023.0) * 65535.0).round().clamp(0.0, 65535.0) as u16) as f32 / 65535.0)
            .collect();
        let lin = |c: f32| {
            let pos = c.clamp(0.0, 1.0) * 1023.0;
            let i0 = (pos as usize).min(1023);
            let i1 = (i0 + 1).min(1023);
            tbl[i0] + (tbl[i1] - tbl[i0]) * (pos - i0 as f32)
        };
        let src_d65 = mat3_mul(BRADFORD_D50_TO_D65, PROPHOTO_D50);
        for dst in [Gamut::Srgb, Gamut::DisplayP3] {
            let d = worst_delta(r.gamut, dst, src_d65, lin);
            eprintln!("faithful LStar → {}: worst |Δ| = {d}/255 vs the reference", dst.label());
            assert!(d <= 1, "the table curve must reproduce to ±1 code, got {d}");
        }
    }

    /// **CHAIN ORDER, PINNED.** A profile whose colorants DO match a modeled gamut must take route
    /// (1) and never route (2) — otherwise every ordinary sRGB / P3 / Adobe RGB / Rec.2020 file in
    /// the world would start rendering through a per-file registry entry, the modeled gamuts would
    /// stop being the answer to anything, and `src == dst` (the no-op that makes an sRGB file on an
    /// sRGB output free) would never fire again.
    ///
    /// FALSIFIER: move the faithful step above the τ test in `resolve_source_gamut` — every row here
    /// reddens on the route, and `srgb_on_srgb_is_still_a_no_op` on the identity.
    #[test]
    fn a_colorant_matching_profile_never_takes_the_faithful_route() {
        for g in SOURCE_GAMUTS {
            let icc = icc_bytes_for_gamut(g).expect("a modeled gamut serializes");
            let r = resolve_source_gamut(Some(&icc), Some("something unhelpful"));
            assert_eq!(r.route, GamutRoute::Colorimetry, "{}: the colorants must answer first", g.label());
            assert_eq!(r.gamut, g, "{}: …with the modeled gamut itself", g.label());
            assert!(!r.gamut.is_source_profile(), "{}: never a registry entry", g.label());
        }
    }

    /// THE STRICTLY-BETTER INVARIANT, spelled out for this round: every modeled-gamut-tagged file
    /// resolves to exactly the gamut it did before v0.8.177, by exactly the same route, and an
    /// sRGB source on an sRGB output is still the byte-identical no-op.
    #[test]
    fn modeled_tagged_files_resolve_exactly_as_before() {
        for (g, name) in [
            (Gamut::Srgb, "sRGB IEC61966-2.1"),
            (Gamut::DisplayP3, "Display P3"),
            (Gamut::AdobeRgb, "Adobe RGB (1998)"),
            (Gamut::Rec2020, "Rec. 2020"),
        ] {
            let icc = icc_bytes_for_gamut(g).unwrap();
            let r = resolve_source_gamut(Some(&icc), Some(name));
            assert_eq!((r.gamut, r.route), (g, GamutRoute::Colorimetry), "{name} must be unchanged");
        }
        // The no-op: same gamut in and out ⇒ the bytes are untouched.
        let mut px = [7u8, 99, 200, 255, 0, 0, 0, 255];
        let before = px;
        transform_rgba(&mut px, Gamut::Srgb, Gamut::Srgb);
        assert_eq!(px, before, "src == dst must still cost nothing and change nothing");
    }

    /// The name fallback is still REACHABLE — for the one profile class that cannot reach route (2):
    /// a LUT/cLUT profile (no colorant tags to measure) whose description says prophoto.
    #[test]
    fn name_fallback_still_reachable_for_a_lut_class_prophoto() {
        // An A2B0-only profile: the LUT/cLUT class. No rXYZ/gXYZ/bXYZ ⇒ nothing to measure.
        let mut icc = vec![0u8; 128];
        let body = b"mAB \0\0\0\0some lut payload".to_vec();
        icc.extend_from_slice(&1u32.to_be_bytes());
        icc.extend_from_slice(b"A2B0");
        icc.extend_from_slice(&(144u32).to_be_bytes());
        icc.extend_from_slice(&(body.len() as u32).to_be_bytes());
        icc.extend_from_slice(&body);
        assert_eq!(icc_colorants_d65(&icc), None, "a cLUT profile has no colorants");
        let r = resolve_source_gamut(Some(&icc), Some("ProPhoto RGB"));
        assert_eq!(r.route, GamutRoute::Description, "…so the NAME answers, exactly as it always did");
        assert_eq!(r.gamut, Gamut::Rec2020, "and `from_description`'s prophoto arm is still alive");
        assert!(r.unreadable_profile, "the bytes were there; we just could not measure them");
    }

    /// **THE 5c(iii) FALSIFIER.** A `curv` tag whose DECLARED extent (12 bytes — a header and
    /// nothing else) cannot hold the 1024 samples its own count field promises. Before this round
    /// every read was bounded by the WHOLE profile buffer, so the parser walked straight past the
    /// end of the tag into whatever followed it and returned 1024 bytes of the next tag's colorants
    /// as a tone curve — a confident, fabricated curve, no error, no refusal.
    ///
    /// RED-FIRST: run against the pre-fix `parse_tone_curve` (bounds `icc.len()`) this returns
    /// `Some(Table(1024 samples))` and the assertion below fails.
    #[test]
    fn truncated_curv_tag_is_refused_not_fabricated() {
        // A profile whose `rTRC` tag is DECLARED 12 bytes long but whose count says 1024, followed
        // immediately by 4 KB of plausible-looking bytes still inside the buffer.
        let mut curv_hdr = b"curv".to_vec();
        curv_hdr.extend_from_slice(&[0u8; 4]);
        curv_hdr.extend_from_slice(&1024u32.to_be_bytes()); // the lie: 1024 samples in a 12-byte tag
        let trailing: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        let mut icc = vec![0u8; 128];
        icc.extend_from_slice(&2u32.to_be_bytes());
        let table_len = 4 + 2 * 12;
        let base = 128 + table_len;
        // entry 0: rTRC → the 12-byte header. entry 1: a filler tag holding the trailing bytes, so
        // they are INSIDE the buffer (which is precisely why the old bound did not catch this).
        for (sig, off, sz) in [(*b"rTRC", base, 12usize), (*b"gXYZ", base + 12, trailing.len())] {
            icc.extend_from_slice(&sig);
            icc.extend_from_slice(&(off as u32).to_be_bytes());
            icc.extend_from_slice(&(sz as u32).to_be_bytes());
        }
        icc.extend_from_slice(&curv_hdr);
        icc.extend_from_slice(&trailing);

        let (off, size) = icc_find_tag(&icc, 2, b"rTRC").expect("the tag table is well-formed");
        assert_eq!(size, 12, "the tag declares 12 bytes…");
        assert!(off + 12 + 2048 <= icc.len(), "…and 2048 more bytes DO sit inside the buffer");
        assert_eq!(
            parse_tone_curve(&icc, off, size).map(|c| c.kind_summary()),
            None,
            "a tag that cannot hold its own curve is a REFUSAL — never a curve read from its neighbours"
        );

        // The same rule for the other two reads: an N=1 gamma and a `para`, each declared too short.
        let short_gamma = {
            let mut v = b"curv".to_vec();
            v.extend_from_slice(&[0u8; 4]);
            v.extend_from_slice(&1u32.to_be_bytes());
            v
        };
        assert_eq!(short_gamma.len(), 12, "a 12-byte N=1 curv has no room for its gamma word");
        let mut buf = short_gamma.clone();
        buf.extend_from_slice(&[0x01, 0xCC]); // a gamma the OLD bound would have happily read
        assert!(parse_tone_curve(&buf, 0, 12).is_none(), "N=1 needs 14 bytes INSIDE the tag");
        assert!(matches!(parse_tone_curve(&buf, 0, 14), Some(ToneCurve::Gamma(_))), "…and 14 is enough");

        let mut para = b"para".to_vec();
        para.extend_from_slice(&[0u8; 4]);
        para.extend_from_slice(&0u16.to_be_bytes()); // funcType 0 → 1 param
        para.extend_from_slice(&[0u8; 2]);
        para.extend_from_slice(&s15_bytes(2.2));
        assert_eq!(para.len(), 16);
        assert!(parse_tone_curve(&para, 0, 12).is_none(), "a 12-byte para cannot hold its parameter");
        assert!(matches!(parse_tone_curve(&para, 0, 16), Some(ToneCurve::Para { .. })), "…16 can");
    }

    /// A faithful source is labelled by the PROFILE'S OWN description — read from the same bytes the
    /// transform came from, never from a name a caller passed in (the v0.8.141 R7 discipline), and
    /// never from a modeled gamut it is not.
    #[test]
    fn a_faithful_source_is_labelled_by_its_own_profile() {
        // The caller's description says one thing; the profile says another. The profile wins.
        let icc = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.8), "Kodak ProPhoto RGB v2");
        let r = resolve_source_gamut(Some(&icc), Some("sRGB"));
        assert_eq!(r.route, GamutRoute::Faithful);
        assert_eq!(r.gamut.display_name(), "Kodak ProPhoto RGB v2", "the PROFILE names it, not the caller");
        assert_ne!(r.gamut.display_name(), Gamut::Rec2020.label(), "never a modeled name it is not");

        // An absurd description is bounded, not passed through, and a control character cannot forge
        // a second log line.
        let long = format!("{}\nSECOND LINE", "W".repeat(200));
        let icc2 = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.9), &long);
        let r2 = resolve_source_gamut(Some(&icc2), None);
        let name = r2.gamut.display_name();
        assert!(name.chars().count() <= SOURCE_DESC_MAX + 1, "bounded, got {} chars", name.chars().count());
        assert!(!name.contains('\n'), "no control character survives into a log line");

        // A profile with NO description still gets a stated stand-in, never an empty cell.
        let icc3 = build_matrix_icc(PROPHOTO_D50, None, 2.0);
        let r3 = resolve_source_gamut(Some(&icc3), None);
        assert_eq!(r3.route, GamutRoute::Faithful);
        assert_eq!(r3.gamut.display_name(), "embedded profile");
    }

    /// v0.8.177 (H-Y1, H-Y2) — THE LABEL SURVIVES A HOSTILE NAME. An ICC description is untrusted
    /// file content and the label is a log line and a panel row, so the two remaining ways to attack
    /// it are closed: the INVISIBLE formatting characters (which a control-character filter does not
    /// catch and which a space would fabricate word boundaries out of), and a truncation that lands
    /// between a base character and its combining marks.
    ///
    /// FALSIFIER: drop `is_invisible_format` from the filter and row (a) shows "Adobe RGB" for a
    /// profile that does not say it; drop the mark-stripping loop and row (b)'s ellipsis wears three
    /// marks the profile never put on it.
    #[test]
    fn a_hostile_profile_description_is_neutralised_without_being_mangled() {
        // (a) H-Y1: the invisible set is REMOVED, so the visible text is exactly what it reads —
        // a zero-width space inside a word must not become a space that splits it into a name we
        // recognise, and a bidi MARK must not survive to reorder the row.
        let hostile = "Adobe\u{200B}RGB\u{200E} \u{FEFF}Wide\u{2060}Gamut\u{061C}";
        let got = bounded_profile_desc(Some(hostile));
        eprintln!("H-Y1 invisible-format name → {got:?}");
        assert_eq!(got, "AdobeRGB WideGamut", "invisible characters are dropped, the visible text is kept");
        assert!(!got.chars().any(is_invisible_format), "no invisible character survives into a log line");
        assert_ne!(got, "Adobe RGB WideGamut", "…and NONE of them may fabricate a word boundary");

        // (b) H-Y2: a cut at exactly `SOURCE_DESC_MAX` lands mid-cluster here — the 41st character
        // is a base whose three combining marks would otherwise be left to stack onto the '…'.
        let marks = "\u{0301}\u{0308}\u{0327}"; // acute + diaeresis + cedilla
        let long = format!("{}A{marks} and more", "W".repeat(SOURCE_DESC_MAX));
        let got = bounded_profile_desc(Some(&long));
        eprintln!("H-Y2 truncated at a mark boundary → {got:?}");
        assert_eq!(got, format!("{}…", "W".repeat(SOURCE_DESC_MAX)), "the marks go with the base they lost");
        // The dangling form: the cut falls INSIDE the marks, so one is the last character kept and
        // would compose onto whatever comes next — the ellipsis.
        let dangling = format!("{}A{marks}tail", "W".repeat(SOURCE_DESC_MAX - 2));
        let got = bounded_profile_desc(Some(&dangling));
        eprintln!("H-Y2 truncated inside the marks → {got:?}");
        assert!(!got.chars().any(is_combining_mark), "no orphan mark composes onto the ellipsis: {got:?}");
        assert_eq!(got, format!("{}A…", "W".repeat(SOURCE_DESC_MAX - 2)));
        // The bound still holds — stripping marks may only ever shorten the label.
        assert!(got.chars().count() <= SOURCE_DESC_MAX + 1);
    }

    /// A matrix profile with NO usable TRC on any channel is not renderable faithfully — it is
    /// refused and the chain moves on, rather than being rendered with an invented curve.
    #[test]
    fn a_matrix_profile_without_a_usable_trc_falls_through() {
        // Colorants present, all three TRC tags of an unknown type ⇒ `parse_tone_curve` refuses each.
        let mut junk = b"junk".to_vec();
        junk.extend_from_slice(&[0u8; 8]);
        let icc = build_matrix_icc_raw(PROPHOTO_D50, None, junk);
        assert!(icc_colorants_d65(&icc).is_some(), "the colorants are readable");
        assert_eq!(
            register_source_profile(&icc),
            Err(FaithfulRefusal::NoUsableTrc),
            "…but there is no curve to render through"
        );
        let r = resolve_source_gamut(Some(&icc), Some("ProPhoto RGB"));
        assert_eq!(r.route, GamutRoute::Description, "so the name answers");
        assert_eq!(r.gamut, Gamut::Rec2020);
        // G-O4: and the line says WHICH refusal sent it there — not the catch-all sentence.
        assert_eq!(r.faithful_refusal, Some(FaithfulRefusal::NoUsableTrc));
        assert_eq!(r.why(), "name — measured, no usable TRC to render through");
    }

    /// Identical profile bytes must collapse to ONE registry entry — a folder of 400 exports from
    /// one application costs one slot, not 400. (The cap's exhaustion behaviour is pinned in its own
    /// test BINARY, `tests/source_registry_cap.rs`, because the registry is process-global and
    /// filling it here would starve every row above.)
    #[test]
    fn identical_profiles_share_one_registry_entry() {
        let icc = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.7), "Dedup Probe RGB");
        let a = register_source_profile(&icc).expect("registers");
        let b = register_source_profile(&icc.clone()).expect("registers again");
        assert_eq!(a, b, "the same bytes must yield the same gamut, or gamut-keyed caches thrash");
        // …and different bytes must NOT collide.
        let other = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.6), "Dedup Probe RGB");
        assert_ne!(register_source_profile(&other).expect("registers"), a);
    }

    /// **THE WHITE GUARD.** A source is honoured faithfully only when its native white is one this
    /// crate may adapt FROM — D65, or (since the owner's 08-09 ruling) D50 — see
    /// [`SOURCE_WHITE_TOL`]. This is what keeps the deliberate v0.8.67 `DCI-P3 → Display P3` name
    /// ruling alive after the chain reorder: a true-DCI profile IS matrix-TRC and would otherwise
    /// have been claimed by route (2), tinting every neutral it rendered. Its ~6300 K white is
    /// outside the tolerance of BOTH admissible whites, which is exactly why the D50 widening does
    /// not reach it.
    ///
    /// FALSIFIER: delete the `source_colorants_d65` call in `parse_source_icc`. The DCI row here
    /// reddens, and so does `colorimetry_never_loses_a_name_the_old_path_knew` — the shipped
    /// ruling's own test, which is how this collision was found in the first place.
    #[test]
    fn a_source_white_that_is_neither_d65_nor_d50_is_refused_and_falls_through() {
        // (1) True cinema DCI-P3 — the shipped ruling's case. Its white sits 0.022 from D65.
        let dci = icc_bytes_for_gamut(Gamut::DciP3).expect("DciP3 serializes");
        assert!(icc_colorants_d65(&dci).is_some(), "it IS a readable matrix profile…");
        assert!(Gamut::from_icc_bytes(&dci).is_none(), "…that misses τ…");
        assert_eq!(
            register_source_profile(&dci),
            Err(FaithfulRefusal::NonAdaptableWhite),
            "…and the white guard refuses it"
        );
        // The margin, measured rather than asserted from the doc: outside tolerance of BOTH whites.
        let m = icc_colorants_d65(&dci).expect("parses");
        let w = [m[0][0] + m[0][1] + m[0][2], m[1][0] + m[1][1] + m[1][2], m[2][0] + m[2][1] + m[2][2]];
        let xy = |v: [f32; 3]| (v[0] / (v[0] + v[1] + v[2]), v[1] / (v[0] + v[1] + v[2]));
        let (x, y) = xy(w);
        for (label, t) in [("D65", white_of(Gamut::Srgb.rgb_to_xyz())), ("D50", D50_XYZ)] {
            let (tx, ty) = xy(t);
            let d = ((x - tx).powi(2) + (y - ty).powi(2)).sqrt();
            eprintln!("true-DCI white is {d:.5} from {label} (τ_w {SOURCE_WHITE_TOL})");
            assert!(d > SOURCE_WHITE_TOL, "the DCI white must miss {label}, else the ruling collapses");
        }
        let r = resolve_source_gamut(Some(&dci), Some("DCI-P3"));
        assert_eq!((r.gamut, r.route), (Gamut::DisplayP3, GamutRoute::Description), "the v0.8.67 ruling stands");
        assert_eq!(r.faithful_refusal, Some(FaithfulRefusal::NonAdaptableWhite), "G-O4: the cause is named");
        assert_eq!(r.why(), "name — measured, white neither D65 nor D50");

        // (2) A white that is NEITHER — xy (0.330, 0.360), between the two admissible ones and
        // outside both (0.016 from D50, 0.036 from D65). sRGB colorants stored in the D50 PCS with a
        // `chad` that recovers that white, which is the real ICC v4 shape. Still refused: the ruling
        // widened the gate by ONE named illuminant, it did not open it.
        const ODD_WHITE: [f32; 3] = [0.330 / 0.360, 1.0, (1.0 - 0.330 - 0.360) / 0.360];
        let icc = build_matrix_icc(
            mat3_mul(BRADFORD_D65_TO_D50, Gamut::Srgb.rgb_to_xyz()),
            Some(bradford_to_d50(ODD_WHITE)),
            2.2,
        );
        let (wx, wy) = xy(white_of(icc_colorants_d65(&icc).expect("parses")));
        eprintln!("the in-between white recovers as xy ({wx:.4}, {wy:.4})");
        assert!((wx - 0.330).abs() < 2e-3 && (wy - 0.360).abs() < 2e-3, "the fixture must carry the white it claims");
        assert_eq!(register_source_profile(&icc), Err(FaithfulRefusal::NonAdaptableWhite));
    }

    /// **THE OWNER'S 08-09 RULING, PINNED.** A profile that RECOVERS a D50 device white (a `chad`,
    /// the ICC v4 spelling of a D50-native working space) is Bradford-adapted exactly like its
    /// chad-LESS v2 twin — the same colorants, the same faithful route, one answer for two spellings
    /// of one colour space. Before this, the v2 file rendered faithfully and the v4 file fell back to
    /// its name, which is the contradiction skeptic G found (ColorMatchRGB, ECI RGB v2 and ProPhoto
    /// v4 are all D50-native, so "no real profile is affected" was false).
    ///
    /// FALSIFIER: drop the D50 arm from `source_colorants_d65` — both rows fall to `Description`.
    #[test]
    fn a_d50_device_white_is_adapted_exactly_like_its_chad_less_twin() {
        const IDENT: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        // A D50-native profile's `chad` maps its white (D50) to the PCS white (D50) — the identity.
        // So `icc_colorants_d65` un-adapts to D50, which is precisely the case that used to be
        // refused, and the case the ruling is about.
        for (label, m_d50) in [("ProPhoto v4", PROPHOTO_D50), ("ECI-class", eci_class_d50())] {
            let v4 = build_matrix_icc(m_d50, Some(IDENT), 1.8);
            let v2 = build_matrix_icc(m_d50, None, 1.8); // the chad-less twin, byte-distinct
            let raw = icc_colorants_d65(&v4).expect("parses");
            assert!(source_colorants_d65(raw).is_some(), "{label}: a D50 white is now adaptable");
            assert!(Gamut::from_icc_bytes(&v4).is_none(), "{label}: it must still MISS τ, or route 1 owns it");

            let (r4, r2) = (resolve_source_gamut(Some(&v4), None), resolve_source_gamut(Some(&v2), None));
            assert_eq!(r4.route, GamutRoute::Faithful, "{label}: the chad spelling renders faithfully");
            assert_eq!(r2.route, GamutRoute::Faithful, "{label}: …as the chad-less one already did");
            assert_ne!(r4.gamut, r2.gamut, "{label}: byte-distinct profiles are distinct entries");

            let (p4, p2) = (source_profile(r4.gamut).unwrap(), source_profile(r2.gamut).unwrap());
            let d = colorant_distance(p4.rgb_to_xyz, p2.rgb_to_xyz);
            eprintln!("{label}: chad-recovered D50 vs chad-less twin — worst colorant |Δ| = {d:.2e}");
            assert!(d < 1e-4, "{label}: the two spellings must land on the SAME colorants, got {d}");
            // …and on D65, which is what makes them safe to render on a D65 destination.
            assert!(source_colorants_d65(p4.rgb_to_xyz).is_some());
            let (wx, wy) = {
                let w = white_of(p4.rgb_to_xyz);
                (w[0] / (w[0] + w[1] + w[2]), w[1] / (w[0] + w[1] + w[2]))
            };
            let d65 = white_of(Gamut::Srgb.rgb_to_xyz());
            let (rx, ry) = (d65[0] / (d65[0] + d65[1] + d65[2]), d65[1] / (d65[0] + d65[1] + d65[2]));
            assert!(
                ((wx - rx).powi(2) + (wy - ry).powi(2)).sqrt() < 1e-3,
                "{label}: the registered colorants must be D65-referenced"
            );
        }
    }

    /// The white a colorant matrix implies: its columns are the colorants and they sum to it.
    fn white_of(m: [[f32; 3]; 3]) -> [f32; 3] {
        [m[0][0] + m[0][1] + m[0][2], m[1][0] + m[1][1] + m[1][2], m[2][0] + m[2][1] + m[2][2]]
    }

    /// An ECI-RGB-v2-class working space: its published primaries at D50, DERIVED here (scale each
    /// primary's unit-luminance XYZ so the columns sum to the white) rather than pasted as a matrix
    /// nobody can check. D50-native and unplaceable — the second real shape the 08-09 ruling admits.
    fn eci_class_d50() -> [[f32; 3]; 3] {
        let prim = [(0.67f32, 0.33f32), (0.21, 0.71), (0.14, 0.08)];
        let mut p = [[0.0f32; 3]; 3];
        for (i, (x, y)) in prim.iter().enumerate() {
            p[0][i] = x / y;
            p[1][i] = 1.0;
            p[2][i] = (1.0 - x - y) / y;
        }
        let inv = mat3_inv(p);
        let s: Vec<f32> = (0..3)
            .map(|r| inv[r][0] * D50_XYZ[0] + inv[r][1] * D50_XYZ[1] + inv[r][2] * D50_XYZ[2])
            .collect();
        for row in p.iter_mut() {
            for (i, v) in row.iter_mut().enumerate() {
                *v *= s[i];
            }
        }
        p
    }

    /// `SourceIcc` is SOURCE-ONLY, exactly as `Custom`/`DciP3` are destination-only: no settings
    /// index can produce one, so it can never be selected as an output gamut.
    #[test]
    fn settings_indices_never_emit_a_source_profile() {
        for i in -3..12 {
            assert!(!Gamut::from_i32(i).is_source_profile(), "index {i} produced a source profile");
        }
    }

    /// `to_i32` is the exact inverse of `from_i32` — the assumption the five retired
    /// `Gamut::X as u32` casts were making silently.
    #[test]
    fn settings_index_round_trips() {
        for i in 0..6 {
            assert_eq!(Gamut::from_i32(i).to_i32(), i, "index {i} must round-trip");
        }
        assert_eq!(Gamut::SourceIcc(3).to_i32(), 0, "a source profile is not a settings value");
    }

    /// The GPU's kind-4 branch and the CPU's `Trc::SrcLut` must sample THE SAME BYTES. `falcon-gpu`
    /// uploads exactly what `source_linearize_lut` returns; this pins the layout it assumes —
    /// `3 × CUSTOM_LUT_N`, rows R/G/B, forward (device → linear), so row 0 is monotone rising.
    #[test]
    fn the_source_lut_has_the_layout_the_gpu_uploads() {
        let icc = build_named_matrix_icc(PROPHOTO_D50, curv_gamma(1.8), "Layout Probe RGB");
        let g = register_source_profile(&icc).expect("registers");
        let (id, lut) = source_linearize_lut(g).expect("a faithful gamut exposes its LUT");
        assert_eq!(Gamut::SourceIcc(id), g, "the id is the gamut's own payload — the texture cache key");
        assert_eq!(lut.len(), 3 * CUSTOM_LUT_N, "3 rows of CUSTOM_LUT_N, matching the texture");
        assert_eq!(lut[0], 0, "forward curve: device 0 → linear 0");
        assert_eq!(lut[CUSTOM_LUT_N - 1], 65535, "…and device 1 → linear 1");
        assert!(lut.windows(2).take(CUSTOM_LUT_N - 1).all(|w| w[0] <= w[1]), "row 0 must be monotone");
        // A gamma > 1 curve sits BELOW the identity everywhere in between — the direction that
        // distinguishes a forward curve from the inverse one row 0..2 of the texture carries.
        let mid = CUSTOM_LUT_N / 2;
        assert!((lut[mid] as f32) < 0.5 * 65535.0, "a forward gamma-1.8 curve darkens the midpoint");
        // Modeled gamuts expose nothing — the GPU then fills rows 3..5 with the identity.
        assert!(source_linearize_lut(Gamut::Srgb).is_none());
        assert!(source_linearize_lut(Gamut::Custom).is_none());
    }

    /// ROUND 35 (queue item 35) -- **THE 16-BIT TRANSFORM IS THE 8-BIT ONE AT A DIFFERENT SCALE.**
    ///
    /// Every 8-bit value `v` has an exact 16-bit spelling `v * 257` (0 to 0, 255 to 65535), so a
    /// colour converted at 16 bits and then narrowed by the export's own `>> 8` must land on the
    /// value the 8-bit converter produces, give or take the one count that two roundings at two
    /// scales can differ by. That is the whole claim: the u16 entries are a WIDER PIPE, never a
    /// second colour policy.
    ///
    /// FALSIFIER (L28): swap the matrix for `dst -> src` and every probe reddens on the `channel`
    /// assert. The SCALE is a separate question and the 8-bit comparison above cannot answer it --
    /// `/ 65536.0` moves a sample by one part in 65 536, which `>> 8` cannot see -- so the last
    /// block below asks it at full precision: white must stay white to the count. That block is
    /// what `/ 65536.0` reddens, with 65534 on the left.
    #[test]
    fn the_16_bit_transform_agrees_with_the_8_bit_one() {
        for (src, dst) in [(Gamut::DisplayP3, Gamut::Srgb), (Gamut::AdobeRgb, Gamut::Srgb)] {
            for probe in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255], [17, 130, 200]] {
                let mut eight = probe;
                transform_rgb(&mut eight, src, dst);
                let mut deep: Vec<u16> = probe.iter().map(|&v| v as u16 * 257).collect();
                transform_rgb16(&mut deep, src, dst);
                for (i, (a, b)) in eight.iter().zip(deep.iter()).enumerate() {
                    let narrowed = (b >> 8) as i32;
                    assert!(
                        (narrowed - *a as i32).abs() <= 1,
                        "channel {i} of {probe:?} under {src:?} to {dst:?}: 8-bit {a}, 16-bit narrowed {narrowed}"
                    );
                }
            }
        }
        // ANTI-VACUITY: on this pair the transform really does move the numbers, so "they agree"
        // is a measurement and not the identity dressed up. The probe is a MIXED colour, not a
        // primary: P3 red is outside sRGB, so it clamps back to 255,0,0 and would prove nothing.
        let mut moved = [17u8, 130, 200];
        transform_rgb(&mut moved, Gamut::DisplayP3, Gamut::Srgb);
        assert_ne!(moved, [17u8, 130, 200], "a mixed P3 colour must not survive unchanged as sRGB");

        // THE SCALE, at full precision. Every modelled gamut here is D65-referenced, so its white
        // point IS the destination's: full scale in must be full scale out, to the count. This is
        // the assert that sees the difference between `/ 65535.0` (the value that makes 65 535 mean
        // 1.0) and `/ 65536.0` (the constant that looks right and leaves white one count short) --
        // a difference of one part in 65 536, which the 8-bit comparison above cannot reach.
        for (src, dst) in [(Gamut::DisplayP3, Gamut::Srgb), (Gamut::AdobeRgb, Gamut::Srgb)] {
            let mut white = [65_535u16, 65_535, 65_535];
            transform_rgb16(&mut white, src, dst);
            assert_eq!(
                white,
                [65_535u16, 65_535, 65_535],
                "white must stay white at 16 bits under {src:?} to {dst:?}"
            );
            let mut black = [0u16, 0, 0];
            transform_rgb16(&mut black, src, dst);
            assert_eq!(black, [0u16, 0, 0], "…and black must stay black");
        }
    }

    /// ROUND 35 (queue item 35) -- **THE ALPHA PLANE IS NOT A COLOUR.** The RGB triples of an
    /// RGBA16 buffer move; every fourth sample is identical; and the serial and parallel arms of
    /// the same call produce the same buffer, which is the guarantee the 8-bit entries carry.
    ///
    /// FALSIFIER (L28): write `c[3]` in `transform16_impl`'s closure, or hand the RGBA entry a `ch`
    /// of 3, and the "alpha of pixel 0 is untouched" assert reddens with a converted value.
    #[test]
    fn the_16_bit_rgba_transform_leaves_alpha_alone() {
        let alphas: [u16; 4] = [0, 1, 30_000, 65_535];
        let mut px: Vec<u16> = Vec::new();
        for (i, a) in alphas.iter().enumerate() {
            px.extend_from_slice(&[(i as u16 + 1) * 9_000, 12_345, 60_000, *a]);
        }
        let before = px.clone();
        transform_rgba16(&mut px, Gamut::DisplayP3, Gamut::Srgb);
        for (i, a) in alphas.iter().enumerate() {
            assert_eq!(px[i * 4 + 3], *a, "alpha of pixel {i} is untouched");
        }
        assert_ne!(px, before, "ANTI-VACUITY: the colour really did convert");
        let mut ser = before.clone();
        let mut par = before.clone();
        transform16_impl(&mut ser, Gamut::DisplayP3, Gamut::Srgb, 4, false);
        transform16_impl(&mut par, Gamut::DisplayP3, Gamut::Srgb, 4, true);
        assert_eq!(ser, par, "serial and parallel are the same buffer");
    }
}
