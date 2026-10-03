//! Bounded content-space glass mini-render. No decoding, GPU readback or file I/O.
//! Photo pixels are colour-managed per layer AFTER sampling at canvas resolution;
//! a grid of sixty tiles never transforms sixty full 160px source mips.
use falcon_color::Gamut;
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn intersection(self, b: Self) -> Self {
        let x = self.x.max(b.x);
        let y = self.y.max(b.y);
        Self::new(
            x,
            y,
            (self.x + self.w).min(b.x + b.w) - x,
            (self.y + self.h).min(b.y + b.h) - y,
        )
    }
    pub fn visible(self) -> bool {
        self.w > 0. && self.h > 0.
    }
    fn fingerprint(self, h: &mut impl Hasher) {
        for v in [self.x, self.y, self.w, self.h] {
            v.to_bits().hash(h);
        }
    }
}
pub(crate) type Mip = (Arc<[u8]>, u32, u32, Gamut);
#[derive(Clone)]
pub(crate) struct Layer {
    pub id: usize,
    pub rect: Rect,
    pub clip: Rect,
    pub mip: Option<Mip>,
    pub turns: u8,
    pub contain: bool,
    pub cover: bool,
    pub ready: bool,
}
impl Layer {
    pub fn fingerprint(&self, h: &mut impl Hasher) {
        self.id.hash(h);
        self.rect.fingerprint(h);
        self.clip.fingerprint(h);
        self.turns.hash(h);
        self.contain.hash(h);
        self.cover.hash(h);
        if let Some((m, w, ht, src)) = &self.mip {
            (Arc::as_ptr(m) as *const u8 as usize).hash(h);
            w.hash(h);
            ht.hash(h);
            std::mem::discriminant(src).hash(h);
            if let Gamut::SourceIcc(index) = src {
                index.hash(h);
            }
        } else {
            0usize.hash(h);
        }
    }
}

/// Keep the stage's previous 192px scale where possible; cap expanded canvases at
/// 256px per axis. Adjust blur radius with scale so a wide dock does not strengthen it.
pub(crate) fn compose(
    layers: &[Layer],
    stage: Rect,
    width: f32,
    height: f32,
    dst: Gamut,
    chrome: [u8; 3],
) -> (Vec<u8>, u32, u32) {
    let old_scale = (192. / stage.w.max(height).max(1.)).min(1.);
    let scale = old_scale.min(256. / width.max(height).max(1.));
    let radius = (5. * scale / old_scale).round().max(1.) as i32;
    compose_canvas(
        layers,
        stage,
        width,
        height,
        dst,
        chrome,
        (scale, Blur::Box(radius)),
    )
}

/// A menu-sized crop: useful detail without increasing the whole-window canvas.
/// Kernel padding is provided by the caller. At most512px on the long edge;
/// source pixels remain shared cache mips, with no additional photo decoding.
pub(crate) fn compose_menu(
    layers: &[Layer],
    stage: Rect,
    region: Rect,
    dst: Gamut,
    chrome: [u8; 3],
) -> (Vec<u8>, u32, u32) {
    let shift = |r: Rect| Rect::new(r.x - region.x, r.y - region.y, r.w, r.h);
    let layers: Vec<_> = layers
        .iter()
        .filter(|l| l.rect.intersection(l.clip).intersection(region).visible())
        .map(|l| Layer {
            rect: shift(l.rect),
            clip: shift(l.clip),
            ..l.clone()
        })
        .collect();
    let scale = 0.5f32.min(512. / region.w.max(region.h).max(1.));
    compose_canvas(
        &layers,
        shift(stage),
        region.w,
        region.h,
        dst,
        chrome,
        (scale, Blur::Gaussian(14. * scale)),
    )
}

enum Blur {
    Box(i32),
    Gaussian(f32),
}

fn compose_canvas(
    layers: &[Layer],
    stage: Rect,
    width: f32,
    height: f32,
    dst: Gamut,
    chrome: [u8; 3],
    sampling: (f32, Blur),
) -> (Vec<u8>, u32, u32) {
    let (scale, blur) = sampling;
    let w = (width * scale).round().max(1.) as u32;
    let h = (height * scale).round().max(1.) as u32;
    let canvas = Rect::new(0., 0., width, height);
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            let p = ((y * w + x) * 4) as usize;
            let lx = (x as f32 + 0.5) / scale;
            let ly = (y as f32 + 0.5) / scale;
            if lx < stage.x || lx >= stage.x + stage.w || ly < stage.y || ly >= stage.y + stage.h {
                buf[p..p + 3].copy_from_slice(&chrome);
            }
            buf[p + 3] = 255;
        }
    }
    let mut block = [38u8, 38, 38, 255];
    falcon_color::transform_rgba(&mut block, Gamut::Srgb, dst);
    for layer in layers {
        let clip = layer.rect.intersection(layer.clip).intersection(canvas);
        if !clip.visible() {
            continue;
        }
        if layer.contain || layer.cover {
            fill(&mut buf, w, h, scale, clip, &block);
        }
        let Some((m, mw, mh, src)) = &layer.mip else {
            continue;
        };
        if *mw == 0 || *mh == 0 || m.len() != *mw as usize * *mh as usize * 4 {
            continue;
        }
        let mut rect = layer.rect;
        if layer.contain || layer.cover {
            let (ow, oh) = if layer.turns % 2 == 0 {
                (*mw, *mh)
            } else {
                (*mh, *mw)
            };
            let fit = if layer.cover { (rect.w / ow as f32).max(rect.h / oh as f32) } else { (rect.w / ow as f32).min(rect.h / oh as f32) };
            let (rw, rh) = (ow as f32 * fit, oh as f32 * fit);
            rect = Rect::new(
                rect.x + (rect.w - rw) / 2.,
                rect.y + (rect.h - rh) / 2.,
                rw,
                rh,
            );
        }
        let draw = rect.intersection(clip);
        if !draw.visible() {
            continue;
        }
        let (x0, y0, x1, y1) = bounds(draw, scale, w, h);
        let mut patch = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 4) as usize);
        for y in y0..y1 {
            for x in x0..x1 {
                let u = (((x as f32 + 0.5) / scale - rect.x) / rect.w).clamp(0., 0.999999);
                let v = (((y as f32 + 0.5) / scale - rect.y) / rect.h).clamp(0., 0.999999);
                let (su, sv) = match layer.turns % 4 {
                    1 => (v, 1. - u),
                    2 => (1. - u, 1. - v),
                    3 => (1. - v, u),
                    _ => (u, v),
                };
                let sx = ((su * *mw as f32) as u32).min(*mw - 1);
                let sy = ((sv * *mh as f32) as u32).min(*mh - 1);
                let p = ((sy * *mw + sx) * 4) as usize;
                patch.extend_from_slice(&m[p..p + 4]);
            }
        }
        // Each patch contains only photo pixels; chrome never passes through a source gamut.
        let patch = crate::tick::frost_mip_for_display(std::borrow::Cow::Owned(patch), *src, dst);
        let stride = ((x1 - x0) * 4) as usize;
        for (row, y) in (y0..y1).enumerate() {
            let p = ((y * w + x0) * 4) as usize;
            for (out, source) in buf[p..p + stride]
                .chunks_exact_mut(4)
                .zip(patch[row * stride..(row + 1) * stride].chunks_exact(4))
            {
                let alpha = source[3] as u32;
                for c in 0..3 {
                    out[c] = ((source[c] as u32 * alpha + out[c] as u32 * (255 - alpha) + 127)
                        / 255) as u8;
                }
                // The backdrop stays opaque: transparent image pixels reveal chrome,
                // never a hole through the blur exposing sharp UI underneath the panel.
                out[3] = 255;
            }
        }
    }
    match blur {
        Blur::Box(r) => crate::support::box_blur(&mut buf, w, h, r),
        Blur::Gaussian(sigma) => crate::glass_blur::gaussian_blur(&mut buf, w, h, sigma),
    };
    (buf, w, h)
}

fn bounds(r: Rect, s: f32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    // Pixel-centre bounds, so a partial scrolled cell cannot paint outside its viewport.
    let b = |v: f32, max: u32| ((v * s - 0.5).ceil().max(0.) as u32).min(max);
    (b(r.x, w), b(r.y, h), b(r.x + r.w, w), b(r.y + r.h, h))
}
fn fill(buf: &mut [u8], w: u32, h: u32, s: f32, r: Rect, c: &[u8; 4]) {
    let (x0, y0, x1, y1) = bounds(r, s, w, h);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = ((y * w + x) * 4) as usize;
            buf[p..p + 4].copy_from_slice(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_thumbnails_composite_onto_the_tile_without_sharp_holes() {
        let rect = Rect::new(0., 0., 100., 100.);
        let mut layer = solid(0, rect, rect, [255, 0, 0, 0]);
        layer.contain = true;
        let (b, _, _) = compose(&[layer], rect, 100., 100., Gamut::Srgb, [23; 3]);
        assert!(b.chunks_exact(4).all(|p| p == [38, 38, 38, 255]));
    }
    #[test]
    fn mixed_source_gamuts_are_managed_before_blurring_with_chrome() {
        let stage = Rect::new(64., 0., 128., 128.);
        let mut grid = solid(
            1,
            Rect::new(0., 0., 64., 96.),
            Rect::new(0., 0., 64., 128.),
            [90, 170, 190, 255],
        );
        grid.mip.as_mut().unwrap().3 = Gamut::DisplayP3;
        let dst = Gamut::AdobeRgb;
        let mut chrome = [23u8, 23, 23, 255];
        falcon_color::transform_rgba(&mut chrome, Gamut::Srgb, dst);
        let (b, w, _) = compose(
            &[grid],
            stage,
            192.,
            160.,
            dst,
            chrome[..3].try_into().unwrap(),
        );
        let mut expected = [90u8, 170, 190, 255];
        falcon_color::transform_rgba(&mut expected, Gamut::DisplayP3, dst);
        let at = |x, y| &b[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(at(30, 40), expected);
        assert_eq!(at(30, 150), chrome, "chrome has its own source space");
    }
    fn solid(id: usize, rect: Rect, clip: Rect, color: [u8; 4]) -> Layer {
        Layer {
            id,
            rect,
            clip,
            mip: Some((Arc::from(color), 1, 1, Gamut::Srgb)),
            turns: 0,
            contain: false, cover: false,
            ready: true,
        }
    }
    #[test]
    fn grid_and_strip_pixels_are_composited_and_clipped_in_both_axes() {
        let stage = Rect::new(64., 0., 128., 128.);
        let grid = Rect::new(0., 0., 64., 128.);
        let strip = Rect::new(0., 128., 192., 64.);
        let layers = [
            solid(0, stage, stage, [0, 255, 0, 255]),
            solid(1, Rect::new(-20., -20., 104., 180.), grid, [255, 0, 0, 255]),
            solid(2, strip, strip, [0, 0, 255, 255]),
        ];
        let (b, w, h) = compose(&layers, stage, 192., 192., Gamut::Srgb, [23; 3]);
        assert_eq!((w, h), (192, 192));
        let pixel = |x, y| &b[((y * w + x) * 4) as usize..((y * w + x) * 4 + 3) as usize];
        assert_eq!(pixel(30, 60), [255, 0, 0]);
        assert_eq!(pixel(100, 60), [0, 255, 0]);
        assert_eq!(pixel(30, 160), [0, 0, 255]);
        assert_eq!(pixel(100, 160), [0, 0, 255]);
    }
    #[test]
    fn late_pixels_scroll_and_rotation_change_the_key() {
        let mut l = solid(
            5,
            Rect::new(0., 0., 120., 80.),
            Rect::new(0., 0., 400., 400.),
            [255; 4],
        );
        let key = |l: &Layer| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            l.fingerprint(&mut h);
            h.finish()
        };
        let k = key(&l);
        l.rect.y = -2.;
        assert_ne!(k, key(&l));
        let k = key(&l);
        l.turns = 2;
        assert_ne!(k, key(&l));
        let k = key(&l);
        l.mip = None;
        assert_ne!(k, key(&l));
    }
    #[test]
    fn contain_letterboxing_and_quarter_turns_follow_thumbnail_geometry() {
        let rect = Rect::new(0., 0., 160., 160.);
        // A horizontal red/blue two-pixel image becomes red above blue after CW rotation.
        let mut l = solid(0, rect, rect, [0; 4]);
        l.contain = true;
        l.turns = 1;
        l.mip = Some((
            Arc::from([255, 0, 0, 255, 0, 0, 255, 255]),
            2,
            1,
            Gamut::Srgb,
        ));
        let (b, w, _) = compose(&[l], rect, 160., 160., Gamut::Srgb, [23; 3]);
        let pixel = |x, y| &b[((y * w + x) * 4) as usize..((y * w + x) * 4 + 3) as usize];
        assert_eq!(pixel(80, 25), [255, 0, 0]);
        assert_eq!(pixel(80, 135), [0, 0, 255]);
        assert_eq!(pixel(10, 80), [38; 3]);
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    #[test]
    fn review_cover_preserves_aspect_and_crops_source_edges() {
        let pixels:Vec<u8>=[[180,0,0,255],[0,180,0,255],[0,0,180,255],[180,180,0,255]]
            .into_iter().flat_map(|p|[p,p].concat()).collect();
        let layer=Layer {id:0,rect:Rect::new(0.,0.,8.,8.),clip:Rect::new(0.,0.,8.,8.),
            mip:Some((pixels.into(),2,4,Gamut::Srgb)),turns:0,contain:false,cover:true,ready:true};
        let (out,_,_)=compose_canvas(&[layer],Rect::new(0.,0.,8.,8.),8.,8.,Gamut::Srgb,[23;3],(1.,Blur::Box(0)));
        assert_eq!(&out[8*4..8*4+3],&[0,180,0]);
        assert_eq!(&out[6*8*4..6*8*4+3],&[0,0,180]);
    }

    #[test]
    fn cropped_menu_samples_its_own_world_region_with_bounded_pixels() {
        let layer = |id, x, y, color: [u8; 4]| Layer {
            id,
            rect: Rect::new(x, y, 120., 80.),
            clip: Rect::new(0., 0., 1600., 1000.),
            mip: Some((Arc::from(color), 1, 1, Gamut::Srgb)),
            turns: 0,
            contain: true, cover: false,
            ready: true,
        };
        let layers = vec![
            layer(1, 40., 100., [220, 20, 10, 255]),
            layer(2, 168., 100., [10, 220, 20, 255]),
        ];
        let stage = Rect::new(392., 0., 1208., 900.);
        let region = Rect::new(24., 80., 280., 200.);
        let (px, w, h) = compose_menu(&layers, stage, region, Gamut::Srgb, [23; 3]);
        let at = |x: usize, y: usize| &px[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4];
        assert_eq!((w, h), (140, 100));
        assert_eq!(
            at(38, 30),
            &[220, 20, 10, 255],
            "left thumbnail, not the stage or another row"
        );
        assert_eq!(
            at(102, 30),
            &[10, 220, 20, 255],
            "right thumbnail stays spatially distinct"
        );
        let moved: Vec<_> = layers
            .iter()
            .map(|l| Layer {
                rect: Rect::new(l.rect.x + 200., l.rect.y + 200., 120., 80.),
                clip: Rect::new(0., 0., 2000., 2000.),
                ..l.clone()
            })
            .collect();
        let (shifted, _, _) = compose_menu(
            &moved,
            Rect::new(592., 200., 1208., 900.),
            Rect::new(224., 280., 280., 200.),
            Gamut::Srgb,
            [23; 3],
        );
        assert_eq!(
            px, shifted,
            "window/host offsets must not change the sampled background"
        );
        let (_, cw, ch) = compose_menu(
            &layers,
            stage,
            Rect::new(0., 0., 272., 4000.),
            Gamut::Srgb,
            [23; 3],
        );
        assert!(
            cw <= 136 && ch <= 512,
            "bounded even on a very tall display"
        );
    }
}
