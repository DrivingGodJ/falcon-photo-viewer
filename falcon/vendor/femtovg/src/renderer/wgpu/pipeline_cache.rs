// Falcon patch, 2026-10-04. See FALCON-CHANGES.md and Slint issue #12030.
pub(super) fn keep(clear_only: bool, accessed: &mut bool) -> bool {
    clear_only || std::mem::replace(accessed, false)
}

#[cfg(test)]
mod tests {
    use super::keep;
    use std::collections::HashMap;

    /// The old unconditional replace(accessed, false) recompiles text/image on each
    /// frame after Slint's clear-only rendering-notifier flush.
    #[test]
    fn notifier_clear_preserves_scene_pipelines_and_normal_scene_still_prunes() {
        let mut cache = HashMap::new();
        let mut compiled = 0;
        for _ in 0..10 {
            // ClearRect/SetRenderTarget-only flush; no scene pipeline is accessed.
            cache.retain(|_, accessed| keep(true, accessed));
            for name in ["text", "image", "stroke"] {
                *cache.entry(name).or_insert_with(|| { compiled += 1; false }) = true;
            }
            cache.retain(|_, accessed| keep(false, accessed));
        }
        assert_eq!(compiled, 3, "warm frames must reuse all three scene pipelines");
        *cache.get_mut("text").unwrap() = true;
        cache.retain(|_, accessed| keep(false, accessed));
        assert_eq!(cache.len(), 1, "unused pipelines still leave on a real scene flush");
        assert!(cache.contains_key("text"));
    }
}
