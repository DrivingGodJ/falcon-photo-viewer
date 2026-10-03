//! Read-only folder-scan benchmark. FALCON_SCAN_WORKERS=1 supplies the serial
//! control; unset uses the bounded default. Never opens the app or writes review data.
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: scan_bench <folder>")?,
    );
    let start = Instant::now();
    if let Some(file) = std::env::args_os().nth(2) {
        let early = falcon_decode::scan_requested_shot(&path.join(file), || false)?;
        println!("requested_shot={} requested_ms={}", early.as_ref().map_or("(deferred)", |s| s.name.as_str()), start.elapsed().as_millis());
    }
    let start = Instant::now();
    let scan = falcon_decode::scan_folder_with_metadata(&path)?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    for shot in &scan.shots {
        (
            &shot.name,
            &shot.raw,
            &shot.jpg,
            shot.id,
            shot.has_raw,
            shot.has_jpg,
            shot.cloud_placeholder,
            falcon_decode::kind_tag(shot.kind),
            shot.sniffed.map(falcon_decode::kind_tag),
        )
            .hash(&mut identity);
    }
    println!(
        "shots={} metadata={} readers={} reads={} enumerate_ms={} headers_ms={} finish_ms={} total_ms={} identity={:016x}",
        scan.shots.len(), scan.metadata.len(), scan.readers, scan.header_reads,
        scan.enumerate_ms, scan.headers_ms, scan.finish_ms, start.elapsed().as_millis(), identity.finish(),
    );
    Ok(())
}
