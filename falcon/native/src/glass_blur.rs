//! Small opaque-canvas blur kernels. No GPU or UI dependencies.
//! Three normalized sliding box passes approximate a Gaussian without negative
//! weights or radius-dependent work per pixel (see W3C feGaussianBlur guidance).

pub(crate) fn box_blur(buf: &mut [u8], w: u32, h: u32, r: i32) {
    if r < 1 {
        return;
    }
    let (w, h) = (w as i32, h as i32);
    let idx = |x: i32, y: i32| ((y * w + x) * 4) as usize;
    let mut tmp = buf.to_vec();
    for y in 0..h {
        for x in 0..w {
            let (mut a, mut b, mut c, mut n) = (0u32, 0u32, 0u32, 0u32);
            for d in -r..=r {
                let i = idx((x + d).clamp(0, w - 1), y);
                a += buf[i] as u32;
                b += buf[i + 1] as u32;
                c += buf[i + 2] as u32;
                n += 1;
            }
            let i = idx(x, y);
            tmp[i] = (a / n) as u8;
            tmp[i + 1] = (b / n) as u8;
            tmp[i + 2] = (c / n) as u8;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let (mut a, mut b, mut c, mut n) = (0u32, 0u32, 0u32, 0u32);
            for d in -r..=r {
                let i = idx(x, (y + d).clamp(0, h - 1));
                a += tmp[i] as u32;
                b += tmp[i + 1] as u32;
                c += tmp[i + 2] as u32;
                n += 1;
            }
            let i = idx(x, y);
            buf[i] = (a / n) as u8;
            buf[i + 1] = (b / n) as u8;
            buf[i + 2] = (c / n) as u8;
        }
    }
}

pub(crate) fn gaussian_blur(buf: &mut [u8], w: u32, h: u32, sigma: f32) {
    if w == 0 || h == 0 || !sigma.is_finite() || sigma <= 0. {
        return;
    }
    let (w, h) = (w as usize, h as usize);
    if w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(buf.len()) {
        return;
    }
    // A box of radius r has variance r(r+1)/3. Pick three neighboring integer
    // radii whose combined variance is closest to sigma².
    let variance = sigma.min(w.max(h) as f32).powi(2);
    let low = (((1. + 4. * variance).sqrt() - 1.) / 2.).floor() as usize;
    let high_count = ((variance - (low * (low + 1)) as f32) * 3. / (2 * low + 2) as f32)
        .round()
        .clamp(0., 3.) as usize;
    let mut tmp = buf.to_vec();
    for pass in 0..3 {
        let radius = low + usize::from(pass >= 3 - high_count);
        if radius == 0 {
            continue;
        }
        sliding_box::<true>(buf, &mut tmp, w, h, radius);
        sliding_box::<false>(&tmp, buf, w, h, radius);
    }
}

fn sliding_box<const HORIZONTAL: bool>(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let (lines, len) = if HORIZONTAL { (h, w) } else { (w, h) };
    let diameter = (2 * r + 1) as u32;
    let half = diameter / 2;
    for line in 0..lines {
        let index = |p: usize| {
            if HORIZONTAL {
                (line * w + p) * 4
            } else {
                (p * w + line) * 4
            }
        };
        let mut sum = [0u32; 3];
        for k in 0..2 * r + 1 {
            let p = index(k.saturating_sub(r).min(len - 1));
            for c in 0..3 {
                sum[c] += u32::from(src[p + c]);
            }
        }
        for x in 0..len {
            let p = index(x);
            for c in 0..3 {
                dst[p + c] = ((sum[c] + half) / diameter) as u8;
            }
            dst[p + 3] = src[p + 3]; // input is already composited onto opaque chrome
            if x + 1 < len {
                let left = index(x.saturating_sub(r));
                let right = index((x + r + 1).min(len - 1));
                for c in 0..3 {
                    sum[c] = sum[c] - u32::from(src[left + c]) + u32::from(src[right + c]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaussian_preserves_constant_fields_and_monotone_edges_without_overshoot() {
        for sigma in [1., 3., 7., 14.] {
            let mut constant = [37, 85, 123, 255].repeat(129 * 3);
            gaussian_blur(&mut constant, 129, 3, sigma);
            assert!(constant.chunks_exact(4).all(|p| p == [37, 85, 123, 255]));
            let mut edge: Vec<u8> = (0..129)
                .flat_map(|x| {
                    if x < 64 {
                        [64, 64, 64, 255]
                    } else {
                        [192, 192, 192, 255]
                    }
                })
                .collect();
            gaussian_blur(&mut edge, 129, 1, sigma);
            let values: Vec<_> = edge.chunks_exact(4).map(|p| p[0]).collect();
            assert!(values.iter().all(|v| (64..=192).contains(v)));
            assert!(
                values.windows(2).all(|p| p[0] <= p[1]),
                "a step has no bright/dark rim or ringing"
            );
        }
    }
    #[test]
    fn gaussian_is_symmetric_on_a_bright_point() {
        let mut pixels = vec![0u8; 129 * 4];
        for p in pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }
        pixels[64 * 4..64 * 4 + 3].fill(255);
        gaussian_blur(&mut pixels, 129, 1, 7.);
        for x in 0..64 {
            assert_eq!(pixels[x * 4], pixels[(128 - x) * 4]);
        }
        let right: Vec<_> = pixels[64 * 4..].chunks_exact(4).map(|p| p[0]).collect();
        assert!(right.windows(2).all(|p| p[0] >= p[1]));
    }
}
