//! Diagnostic: time the ROI (adaptive hi-res) operations on a real image so we know where
//! the zoom lag goes. Usage: `cargo run --release --example roi_bench -- <file>`.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: roi_bench <file>"));
    let shot = falcon_decode::Shot {
        id: 0,
        name: "x".into(),
        has_raw: false,
        has_jpg: true,
        raw: None,
        jpg: Some(path),
        kind: falcon_decode::SrcKind::Jpeg,
        cloud_placeholder: false,
        sniffed: None,
    };

    let t = Instant::now();
    let (rgb, w, h) = falcon_decode::decode_full_rgb(&shot).expect("decode");
    println!("full CPU decode      {w}×{h}  : {} ms", t.elapsed().as_millis());

    // progressive preview: DCT scale-on-load to ~screen long side (much less work)
    for plong in [3840u32, 2560u32] {
        let t = Instant::now();
        let (_p, pw, ph) = falcon_decode::decode_source_scaled(&shot, plong).expect("scaled");
        println!("preview decode <= {plong:<5} -> {pw}×{ph} : {} ms", t.elapsed().as_millis());
    }

    // a typical "moderate zoom" region: centre half of the image → screen-sized tile
    let t = Instant::now();
    let (_tile, ow, oh, ..) =
        falcon_decode::crop_region_rgba(&rgb, w, h, w / 4, h / 4, w / 2, h / 2, 3840).unwrap();
    println!("crop half→{ow}×{oh}      : {} ms", t.elapsed().as_millis());

    // a deep-zoom region: ~1:1, little/no downscale
    let t = Instant::now();
    let (_tile, ow, oh, ..) =
        falcon_decode::crop_region_rgba(&rgb, w, h, w / 2, h / 2, 2000.min(w), 1300.min(h), 3840)
            .unwrap();
    println!("crop deep→{ow}×{oh}     : {} ms", t.elapsed().as_millis());
}
