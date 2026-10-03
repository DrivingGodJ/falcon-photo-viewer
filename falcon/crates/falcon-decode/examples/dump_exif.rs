//! Diagnostic: dump every EXIF field (IFD + tag + value) for a JPEG, then show what
//! `exif_rows` currently extracts. Usage: `cargo run --example dump_exif -- <file>`.

use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: dump_exif <file>"));
    println!("=== {} ===", path.display());

    let file = std::fs::File::open(&path).expect("open");
    let mut br = std::io::BufReader::new(file);
    match exif::Reader::new().read_from_container(&mut br) {
        Ok(reader) => {
            for f in reader.fields() {
                println!(
                    "[{:?}] {:<28} = {}",
                    f.ifd_num,
                    format!("{}", f.tag),
                    f.display_value().with_unit(&reader)
                );
            }
        }
        Err(e) => println!("EXIF read error: {e}"),
    }

    println!("\n--- exif_rows() ---");
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
    // turns=0: this dump shows the file's stored dimension order (pass the composed display turns
    // to preview the oriented row the app shows).
    for (k, v) in falcon_decode::exif_rows(&shot, 0) {
        println!("{k}: {v}");
    }
}
