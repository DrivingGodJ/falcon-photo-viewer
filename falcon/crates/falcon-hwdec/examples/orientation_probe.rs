//! Read-only HEIC lane comparison. Outputs go only to the explicitly supplied directory.
//! Usage: orientation_probe <photo.heic> <output-directory>
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe compares Windows WIC and D3D11VA.");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use falcon_decode::*;
    use std::path::PathBuf;
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("photo.heic"));
    let out = PathBuf::from(args.next().expect("output directory"));
    std::fs::create_dir_all(&out)?;
    let shot = Shot {
        id: 0,
        name: "probe".into(),
        has_raw: false,
        has_jpg: true,
        raw: None,
        jpg: Some(path.clone()),
        kind: SrcKind::Heic,
        cloud_placeholder: false,
        sniffed: None,
    };
    let bytes = std::fs::read(&path)?;
    println!(
        "residual {:?}; dimensions {:?}",
        read_orientation(&shot, false),
        source_dimensions(&shot)
    );
    let plan = parse_heif_grid(&bytes).map_err(|e| format!("{e:?}"))?;
    println!(
        "irot={} imir={:?} crop={:?} display={:?}",
        plan.irot, plan.imir, plan.crop, plan.display
    );
    let turns = orientation_to_turns(read_orientation(&shot, false).unwrap_or(1));
    for (name, lane) in [
        ("thumb", Lane::Thumb),
        ("fast", Lane::Fast),
        ("native", Lane::Native),
    ] {
        let f = browse_frame_rgba(&shot, 256, false, lane)?;
        println!("{name}: {}x{} {:?}", f.w, f.h, f.source);
        let (rgba, w, h) = rotate_rgba(&f.rgba, f.w, f.h, turns);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        save(&out.join(format!("{name}.ppm")), &rgb, w, h)?;
    }
    let src = falcon_hwdec::tile_source(&path).map_err(|e| format!("{e:?}"))?;
    let mut dec = falcon_hwdec::PhotoDecoder::new(&src, 8).map_err(|e| format!("{e:?}"))?;
    let hw = dec.decode(&src, Some(256)).map_err(|e| format!("{e:?}"))?;
    let (rgb, w, h) = rotate_rgb(&hw.rgb, hw.w, hw.h, turns);
    save(&out.join("hardware.ppm"), &rgb, w, h)?;
    Ok(())
}

#[cfg(windows)]
fn save(path: &std::path::Path, rgb: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{w} {h}\n255\n")?;
    f.write_all(rgb)
}
