//! Read-only comparison of the shipping GPU and CPU RAW developers.
//! Usage: cargo run --locked -p falcon-gpu --example raw_highlight_probe -- <raw> <new-output-dir>
//! Writes unrotated full-resolution PPMs and sensor data for independent analysis.
use falcon_decode::{Shot, SrcKind};
use std::{fs, io::Write, path::Path, time::Instant};

fn ppm(dir: &Path, name: &str, rgb: &[u8], w: u32, h: u32) -> anyhow::Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))?;
    write!(f, "P6\n{w} {h}\n255\n")?;
    f.write_all(rgb)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "expected <local-raw> <new-output-dir>");
    let path = Path::new(&args[0]).canonicalize()?;
    let meta = fs::metadata(&path)?;
    anyhow::ensure!(meta.is_file(), "source must be a file");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            meta.file_attributes() & (0x1000 | 0x40000 | 0x400000) == 0,
            "source must already be downloaded"
        );
    }
    let dir = Path::new(&args[1]);
    fs::create_dir(dir)?;
    let shot = Shot {
        id: 0,
        name: "highlight-probe".into(),
        has_raw: true,
        has_jpg: false,
        raw: Some(path),
        jpg: None,
        kind: SrcKind::Jpeg,
        sniffed: None,
        cloud_placeholder: false,
    };
    let start = Instant::now();
    let cfa = falcon_decode::extract_cfa(&shot)?;
    println!("decompress_ms={:.2} dims={}x{} crop={},{},{},{} black={} white={} wb={:?} matrix={:?} rggb={}",
        start.elapsed().as_secs_f64()*1000.0, cfa.width, cfa.height,
        cfa.crop_x, cfa.crop_y, cfa.crop_w, cfa.crop_h,
        cfa.black, cfa.white, cfa.wb, cfa.cam_to_srgb, cfa.rggb);
    anyhow::ensure!(cfa.rggb, "GPU probe requires RGGB");
    fs::write(dir.join("cfa.u16le"), bytemuck::cast_slice(&cfa.data))?;
    let gpu = falcon_gpu::GpuDeveloper::new()?;
    println!("adapter={}", gpu.adapter_name());
    let start = Instant::now();
    let (rgb, w, h) = gpu.develop(&cfa, u32::MAX)?;
    println!(
        "gpu_ms={:.2} dims={w}x{h}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    ppm(dir, "gpu.ppm", &rgb, w, h)?;
    drop(rgb);
    drop(cfa);
    let start = Instant::now();
    let (rgb, w, h) = falcon_decode::develop_raw_rgb_full(&shot)?;
    println!(
        "cpu_ms={:.2} dims={w}x{h}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    ppm(dir, "cpu.ppm", &rgb, w, h)?;
    Ok(())
}
