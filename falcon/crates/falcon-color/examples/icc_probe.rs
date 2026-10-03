//! Dev probe: parse real ICC/ICM profiles and print the extracted D65 matrix + effective gamma,
//! so `parse_display_icc` can be validated against actual system profiles (Adobe RGB has a known
//! answer ≈ `Gamut::AdobeRgb`). Usage: cargo run -p falcon-color --example icc_probe -- <file.icc> ...
use falcon_color::parse_display_icc;

fn main() {
    let fmt = |m: [[f32; 3]; 3]| {
        format!("[[{:.4},{:.4},{:.4}],[{:.4},{:.4},{:.4}],[{:.4},{:.4},{:.4}]]",
            m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2])
    };
    // Known references (columns = R/G/B colorants, D65): sRGB row0 ≈ 0.4124,0.3576,0.1805;
    // AdobeRGB row0 ≈ 0.5767,0.1856,0.1882; DisplayP3 row0 ≈ 0.4866,0.2657,0.1982.
    for path in std::env::args().skip(1) {
        match std::fs::read(&path) {
            Ok(bytes) => match parse_display_icc(&bytes, "probe") {
                Some(p) => println!("OK  {} : gamma={:.4} matrix={}", path, p.gamma, fmt(p.rgb_to_xyz)),
                None => println!("REJ {} : non-matrix / malformed", path),
            },
            Err(e) => println!("ERR {} : {e}", path),
        }
    }
}
