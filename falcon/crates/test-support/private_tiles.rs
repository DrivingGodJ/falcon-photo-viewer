//! Optional real-pixel fixture loading for tests only. Public source carries no camera pixels.
use std::path::{Path, PathBuf};
use falcon_decode::yuv_kernel::{GOLDEN_PINS, REAL_TILE_FIXTURES, YuvGoldenCase, YuvGoldenSrc};

pub struct PrivateTiles(Vec<(&'static str, Vec<u8>)>);
impl PrivateTiles {
    pub fn refs(&self) -> Vec<(&'static str, &[u8])> {
        self.0.iter().map(|(name, bytes)| (*name, bytes.as_slice())).collect()
    }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    pub fn cases(&self) -> impl Iterator<Item = &'static YuvGoldenCase> + '_ {
        GOLDEN_PINS.iter().filter(|case| match case.src {
            YuvGoldenSrc::Synth(_) => true,
            YuvGoldenSrc::Fixture(name) => self.0.iter().any(|(n, _)| *n == name),
        })
    }
}
fn load_from(dir: &Path, required: bool) -> std::io::Result<PrivateTiles> {
    let mut files = Vec::new();
    for &name in REAL_TILE_FIXTURES {
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => {
                if bytes.len() != 256 * 256 * 3 / 2 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid private NV12 length"));
                }
                files.push((name, bytes));
            }
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
    }
    if !files.is_empty() && files.len() != REAL_TILE_FIXTURES.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "incomplete private NV12 corpus"));
    }
    Ok(PrivateTiles(files))
}
pub fn load() -> PrivateTiles {
    let configured = std::env::var_os("FALCON_PRIVATE_TILE_DIR").filter(|p| !p.is_empty());
    let required = configured.is_some();
    let dir = configured.map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata"));
    let tiles = load_from(&dir, required).expect("private NV12 fixtures must be complete and valid when provided");
    if tiles.is_empty() {
        eprintln!("SKIP private NV12 rows: no private camera pixels supplied; synthetic golden rows remain active. Set FALCON_PRIVATE_TILE_DIR to opt in.");
    }
    tiles
}

#[test]
fn public_golden_cases_remain_active_without_private_pixels() {
    let tiles = PrivateTiles(Vec::new());
    let cases: Vec<_> = tiles.cases().collect();
    assert!(cases.len() >= 18, "the portable colour coverage cannot become an empty pass");
    assert!(cases.iter().all(|c| matches!(c.src, YuvGoldenSrc::Synth(_))));
}

#[test]
fn an_explicit_missing_private_corpus_is_an_error() {
    let missing = std::env::temp_dir().join(format!("falcon-no-private-tiles-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    assert!(!missing.exists());
    assert!(load_from(&missing, true).is_err(), "an opted-in corpus cannot silently skip");
    assert!(load_from(&missing, false).unwrap().is_empty());
}
