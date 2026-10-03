//! One-file CPU RAW baseline using the existing production develop and export bodies.
//! The encoded result stays in memory unless an explicit diagnostic output directory is supplied.
//! Diagnostic files use create-new; sources and settings are never modified.
//! Run each sample in a fresh process so Windows' process-wide peak is meaningful.
//! --export-source exercises the production RGB16/RGB8 export decoder, RAW
//! orientation and file encoder; it requires a new --save-dir directory.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use falcon_decode::{develop_raw_rgb_full, export_web_image, Shot, SrcKind, WebFormat};

#[cfg(windows)]
fn peak_working_set_bytes() -> Option<usize> {
    // PROCESS_MEMORY_COUNTERS from psapi.h. Query only this process; no polling or
    // allocator replacement, so the codec's Rayon workers are included in the peak.
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    let mut counters = ProcessMemoryCounters {
        cb: size,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle identifies this process, and the initialized
    // writable counters buffer has the exact repr(C) layout and supplied size.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (ok != 0).then_some(counters.peak_working_set_size)
}

#[cfg(not(windows))]
fn peak_working_set_bytes() -> Option<usize> {
    None // On macOS run with /usr/bin/time -l for native maximum resident set size.
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: raw_export_bench <one-local-raw-path> <jpg|png> <long-side|full> [--save-dir <diagnostic-output-directory>] [--export-source]";
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or(usage)?).canonicalize()?;
    let format = match args.next().as_deref().and_then(|v| v.to_str()) {
        Some("jpg") => WebFormat::Jpeg,
        Some("png") => WebFormat::Png,
        _ => return Err(usage.into()),
    };
    let long_arg = args.next().ok_or(usage)?;
    let long_arg = long_arg.to_str().ok_or(usage)?;
    let long = if long_arg == "full" {
        u32::MAX
    } else {
        long_arg.parse::<u32>()?
    };
    let mut save_dir = None;
    let mut export_source = false;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--save-dir") if save_dir.is_none() => {
                save_dir = Some(PathBuf::from(args.next().ok_or(usage)?));
            }
            Some("--export-source") if !export_source => export_source = true,
            _ => return Err(usage.into()),
        }
    }
    if long < 16 || (export_source && save_dir.is_none()) {
        return Err(usage.into());
    }
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err("the input must name one local RAW file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // OFFLINE, RECALL_ON_OPEN and RECALL_ON_DATA_ACCESS: do not hydrate a
        // cloud placeholder merely to benchmark it.
        if metadata.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0 {
            return Err("the RAW must already be downloaded locally".into());
        }
    }
    let shot = Shot {
        id: 0,
        name: path
            .file_stem()
            .ok_or(usage)?
            .to_string_lossy()
            .into_owned(),
        has_raw: true,
        has_jpg: false,
        raw: Some(path.clone()),
        jpg: None,
        kind: SrcKind::Jpeg,
        sniffed: None,
        cloud_placeholder: false,
    };
    if export_source {
        return benchmark_export_source(
            &shot,
            metadata.len(),
            format,
            long,
            save_dir
                .as_deref()
                .expect("export mode requires a directory"),
        );
    }
    println!(
        "input={} input_bytes={} source=cpu-raw-develop output=RGB8-sRGB format={} long={} orientation=unapplied sink=in-memory threads={}",
        path.display(), metadata.len(), format.noun(), long_arg,
        std::thread::available_parallelism().map_or(1, usize::from),
    );
    let total = Instant::now();
    let (rgb, width, height) = develop_raw_rgb_full(&shot)?;
    let develop_ms = total.elapsed().as_secs_f64() * 1000.0;
    let source_bytes = rgb.len();
    let encode = Instant::now();
    let bytes = export_web_image(
        rgb,
        width,
        height,
        long,
        92,
        None,
        falcon_color::Gamut::Srgb,
        format,
    )?;
    let export_ms = encode.elapsed().as_secs_f64() * 1000.0;
    println!(
        "width={width} height={height} source_bytes={source_bytes} develop_ms={develop_ms:.3} export_ms={export_ms:.3} total_ms={:.3} encoded_bytes={} peak_working_set_bytes={:?}",
        total.elapsed().as_secs_f64() * 1000.0, bytes.len(), peak_working_set_bytes(),
    );
    if let Some(dir) = save_dir {
        std::fs::create_dir_all(&dir)?;
        write_new(&dir.join(format!("cpu_developed.{}", format.ext())), &bytes)?;
        let preview = falcon_decode::jpeg_bytes(&shot)?;
        write_new(&dir.join("camera_preview.jpg"), &preview)?;
        println!("diagnostic_outputs={} source_unchanged=true", dir.display());
    }
    Ok(())
}

fn benchmark_export_source(
    shot: &Shot,
    input_bytes: u64,
    format: WebFormat,
    long: u32,
    dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use falcon_decode::{
        develop_raw_pixels_for_export, export_web_file, orientation_to_turns, read_orientation,
        rotate_pixels, Keep, WebSpec,
    };
    // An exclusively created directory guarantees that export_web_file cannot
    // truncate a prior diagnostic or an original. The parent must already exist.
    std::fs::create_dir(dir)?;
    let output = dir.join(format!("export_developed.{}", format.ext()));
    println!(
        "input={} input_bytes={input_bytes} source=production-raw-export format={} long={long} sink=file orientation=RAW-once",
        shot.raw.as_deref().expect("benchmark has a RAW").display(), format.noun(),
    );
    let total = Instant::now();
    let (pixels, width, height) =
        develop_raw_pixels_for_export(shot, Keep::for_web(format), || false)?;
    let develop_ms = total.elapsed().as_secs_f64() * 1000.0;
    let layout = pixels.layout();
    let rotate = Instant::now();
    let orientation = read_orientation(shot, true).unwrap_or(1);
    let turns = orientation_to_turns(orientation);
    if falcon_decode::orientation_is_mirrored(orientation) {
        eprintln!(
            "mirrored EXIF {orientation}: rotation only, matching the production export contract"
        );
    }
    let (pixels, oriented_width, oriented_height) = rotate_pixels(pixels, width, height, turns);
    let orientation_ms = rotate.elapsed().as_secs_f64() * 1000.0;
    let encode = Instant::now();
    let spec = WebSpec {
        long,
        quality: 92,
        wm: None,
        src: falcon_color::Gamut::Srgb,
        fmt: format,
    };
    let encoded_bytes = export_web_file(pixels, oriented_width, oriented_height, &spec, &output)?;
    let export_ms = encode.elapsed().as_secs_f64() * 1000.0;
    println!(
        "width={width} height={height} layout={layout} raw_orientation={orientation} applied_turns={turns} oriented_width={oriented_width} oriented_height={oriented_height} develop_ms={develop_ms:.3} orientation_ms={orientation_ms:.3} export_ms={export_ms:.3} total_ms={:.3} encoded_bytes={encoded_bytes} peak_working_set_bytes={:?} output={}",
        total.elapsed().as_secs_f64() * 1000.0, peak_working_set_bytes(), output.display(),
    );
    // Decode AFTER recording the pipeline's time and peak, so verification does
    // not become part of the reported export cost.
    match format {
        WebFormat::Png => {
            let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&output)?));
            let mut reader = decoder.read_info()?;
            let srgb = reader.info().srgb.is_some();
            let mut data = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut data)?;
            if info.bit_depth != png::BitDepth::Sixteen
                || info.color_type != png::ColorType::Rgb
                || !srgb
            {
                return Err("production RAW PNG did not retain RGB16 and sRGB metadata".into());
            }
            let fine_samples = data[..info.buffer_size()]
                .chunks_exact(2)
                .filter(|sample| !u16::from_be_bytes([sample[0], sample[1]]).is_multiple_of(257))
                .count();
            println!("verified_width={} verified_height={} decoded_depth=16 decoded_channels=3 srgb_tag=true samples_not_on_8bit_grid={fine_samples}", info.width, info.height);
        }
        WebFormat::Jpeg => {
            let mut decoder =
                jpeg_decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(&output)?));
            let _ = decoder.decode()?;
            let info = decoder.info().ok_or("JPEG decoder returned no metadata")?;
            if info.pixel_format != jpeg_decoder::PixelFormat::RGB24 {
                return Err("production RAW JPEG did not decode as RGB8".into());
            }
            println!("verified_width={} verified_height={} decoded_depth=8 decoded_channels=3 icc_bytes={}", info.width, info.height, decoder.icc_profile().map_or(0, |icc| icc.len()));
        }
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}
