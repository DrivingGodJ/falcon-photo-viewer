//! Read-only real-sample verification. No app/settings/source writes.
use falcon_decode::{browse_frame_rgba, FolderCatalogue, Lane};
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for arg in std::env::args_os().skip(1) {
        let dir = PathBuf::from(arg);
        let mut catalogue = FolderCatalogue::read(&dir, || false)?;
        let candidates = catalogue.simple_candidates();
        let mut early_reads = 0;
        if let Some(candidates) = &candidates {
            let paths: Vec<_> = candidates.iter().take(21).map(|c|c.path.clone()).collect();
            if let Some(batch) = catalogue.scan_batch(&paths, || false)? {
                early_reads = batch.header_reads;
                println!("early folder={} shots={} reads={}",dir.display(),batch.shots.len(),batch.header_reads);
            }
        }
        let full = catalogue.finish(|| false)?;
        let reference = falcon_decode::scan_folder(&dir)?;
        assert_eq!(full.shots.len(),reference.len());
        for (a,b) in full.shots.iter().zip(reference.iter()) {
            assert_eq!((&a.name,&a.jpg,&a.raw,a.kind,a.has_jpg,a.cloud_placeholder),
                (&b.name,&b.jpg,&b.raw,b.kind,b.has_jpg,b.cloud_placeholder));
        }
        let pairs = full.shots.iter().filter(|s|s.has_raw && s.has_jpg).count();
        let mut tested: BTreeMap<String,usize> = BTreeMap::new();
        for shot in &full.shots {
            if shot.cloud_placeholder || shot.is_unsupported() { continue; }
            let key = format!("{}-{}",falcon_decode::kind_tag(shot.kind),if shot.has_raw && shot.has_jpg { "pair" } else if shot.has_raw { "raw" } else { "image" });
            let n = tested.entry(key.clone()).or_default();
            if *n >= 2 { continue; }
            let start = std::time::Instant::now();
            let thumb = browse_frame_rgba(shot,256,true,Lane::Thumb)?;
            let color = falcon_decode::frame_source_gamut(shot,thumb.source);
            let orientation = falcon_decode::read_orientation(shot,false);
            assert!(thumb.w > 0 && thumb.h > 0 && thumb.w.max(thumb.h) <= 288);
            assert_eq!(thumb.rgba.len(),thumb.w as usize*thumb.h as usize*4);
            println!("decode folder={} name={} class={} size={}x{} source={:?} color={:?} orientation={:?} ms={}",
                dir.display(),shot.name,key,thumb.w,thumb.h,thumb.source,color,orientation,start.elapsed().as_millis());
            *n += 1;
        }
        println!("PASS folder={} shots={} pairs={} early_eligible={} total_header_reads={} tested={:?}",
            dir.display(),full.shots.len(),pairs,candidates.is_some(),early_reads+full.header_reads,tested);
    }
    Ok(())
}
