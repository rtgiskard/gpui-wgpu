//! The material sprite's separable 9×9 Gaussian, evaluated at source texel centers.
use std::sync::Arc;

use super::RenderImage;
use image::RgbaImage;

// Positive half (including zero) of GPUI's normalized +/-3 sigma quadrature.
const WEIGHTS: [f32; 5] = [
    0.299_372_4,
    0.225_978_15,
    0.097_191_99,
    0.023_817_923,
    0.003_325_727,
];
type Pixel = [f32; 4];

/// How the blur samples beyond the centers of the outermost texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageBlurEdge {
    /// Repeat the outermost texel.
    Clamp,
    /// Mirror the image about the outermost texel centers.
    Reflect,
}

impl RenderImage {
    /// Blur one frame on the CPU using the material sprite's Gaussian kernel.
    /// The result has a fresh image ID and preserves the source dimensions and scale.
    /// Returns `None` for a missing or empty frame or a negative or non-finite radius.
    ///
    /// This runs synchronously on one thread and costs roughly 34 weighted row passes
    /// over the frame, so call it on a background executor for large images. Cache the
    /// returned image: each call has a new ID, which the renderer uploads to the atlas again.
    pub fn blurred_frame(
        &self,
        frame_index: usize,
        radius_texels: f32,
        edge: ImageBlurEdge,
    ) -> Option<Arc<RenderImage>> {
        let buffer = self.data.get(frame_index)?.buffer();
        let (width, height) = buffer.dimensions();
        if width == 0 || height == 0 || !radius_texels.is_finite() || radius_texels < 0.0 {
            return None;
        }
        // ImageBuffer can wrap storage larger than its logical pixel extent.
        let len = width as usize * height as usize * 4;
        let pixels = dispatch(
            &buffer.as_raw()[..len],
            width as usize,
            height as usize,
            radius_texels,
            edge == ImageBlurEdge::Reflect,
        );
        let frame = RgbaImage::from_raw(width, height, pixels)?;
        let mut result = RenderImage::new([image::Frame::new(frame)]);
        result.scale_factor = self.scale_factor;
        Some(Arc::new(result))
    }
}

fn dispatch(source: &[u8], width: usize, height: usize, radius: f32, reflect: bool) -> Vec<u8> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: Both row operations run synchronously after AVX2 detection.
        return convolve(
            source,
            width,
            height,
            radius,
            reflect,
            |dst, src, w| unsafe { avx2::add_bytes(dst, src, w) },
            |dst, src, w| unsafe { avx2::add_floats(dst, src, w) },
        );
    }
    convolve(
        source, width, height, radius, reflect, add_bytes, add_floats,
    )
}

struct Tap {
    offset: i64,
    weight: f32,
}

// Split each fractional sample between its integer neighbors. Merge coincident
// offsets so small radii and one-pixel axes need fewer row passes.
fn taps(radius: f32, len: usize, reflect: bool) -> Vec<Tap> {
    let limit = (len - 1) as f64;
    let mut taps: Vec<Tap> = Vec::with_capacity(17);
    for i in -4i32..=4 {
        let w = WEIGHTS[i.unsigned_abs() as usize];
        let mut delta = f64::from(radius) * f64::from(i) * 0.25;
        if reflect && limit > 0.0 {
            let period = 2.0 * limit;
            delta = delta.rem_euclid(period);
            if delta > limit {
                delta -= period;
            }
        } else {
            delta = delta.clamp(-limit, limit);
        }
        let base = delta.floor();
        let fraction = (delta - base) as f32;
        for (offset, weight) in [
            (base as i64, w * (1.0 - fraction)),
            (base as i64 + 1, w * fraction),
        ] {
            if weight > 0.0 {
                if let Some(tap) = taps.iter_mut().find(|tap| tap.offset == offset) {
                    tap.weight += weight;
                } else {
                    taps.push(Tap { offset, weight });
                }
            }
        }
    }
    taps
}

fn row_index(position: i64, height: usize, reflect: bool) -> usize {
    let last = height as i64 - 1;
    // The tap builder bounds offsets to +/-last, so at most one fold is needed.
    if reflect {
        (last - (last - position.abs()).abs()) as usize
    } else {
        position.clamp(0, last) as usize
    }
}

fn convolve(
    source: &[u8],
    width: usize,
    height: usize,
    radius: f32,
    reflect: bool,
    add_bytes: impl Fn(&mut [Pixel], &[[u8; 4]], f32),
    add_floats: impl Fn(&mut [Pixel], &[Pixel], f32),
) -> Vec<u8> {
    let (source, remainder) = source.as_chunks::<4>();
    assert!(remainder.is_empty() && source.len() == width * height);
    if source.is_empty() {
        return Vec::new();
    }
    let x_taps = taps(radius, width, reflect);
    let y_taps = taps(radius, height, reflect);
    let pad = x_taps
        .iter()
        .map(|tap| tap.offset.unsigned_abs() as usize)
        .max()
        .unwrap();
    // Vertical-first streaming keeps only two float rows plus the horizontal halo,
    // even for sparse taps spanning the entire image. Each inner loop is contiguous.
    let mut padded = vec![[0.0; 4]; width + 2 * pad];
    let mut horizontal = vec![[0.0; 4]; width];
    let mut output = Vec::with_capacity(source.len() * 4);
    for y in 0..height {
        let (left, rest) = padded.split_at_mut(pad);
        let (row, right) = rest.split_at_mut(width);
        row.fill([0.0; 4]);
        for tap in &y_taps {
            let start = row_index(y as i64 + tap.offset, height, reflect) * width;
            add_bytes(row, &source[start..start + width], tap.weight);
        }
        if reflect {
            for (i, (l, r)) in left.iter_mut().rev().zip(right).enumerate() {
                *l = row[i + 1];
                *r = row[width - 2 - i];
            }
        } else {
            left.fill(row[0]);
            right.fill(row[width - 1]);
        }
        horizontal.fill([0.0; 4]);
        for tap in &x_taps {
            let start = (pad as i64 + tap.offset) as usize;
            add_floats(&mut horizontal, &padded[start..start + width], tap.weight);
        }
        for &[b, g, r, a] in &horizontal {
            let alpha = a.round() as u8;
            let pixel = if alpha == 0 {
                [0; 4]
            } else {
                let inv = a.recip();
                [
                    (b * inv).round() as u8,
                    (g * inv).round() as u8,
                    (r * inv).round() as u8,
                    alpha,
                ]
            };
            output.extend_from_slice(&pixel);
        }
    }
    output
}

// RenderImage stores straight BGRA. Keep color * alpha in float across both
// passes, and divide once on output to retain precision at very low alpha.
fn add_bytes(dst: &mut [Pixel], src: &[[u8; 4]], weight: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        let a = f32::from(s[3]);
        for i in 0..3 {
            d[i] += (f32::from(s[i]) * a) * weight;
        }
        d[3] += a * weight;
    }
}

fn add_floats(dst: &mut [Pixel], src: &[Pixel], weight: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        for i in 0..4 {
            d[i] += s[i] * weight;
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod avx2 {
    use super::Pixel;
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub(super) fn add_bytes(dst: &mut [Pixel], src: &[[u8; 4]], weight: f32) {
        let (dst, d_tail) = dst.as_chunks_mut::<2>();
        let (src, s_tail) = src.as_chunks::<2>();
        let w = _mm256_set1_ps(weight);
        for (d, s) in dst.iter_mut().zip(src) {
            // SAFETY: Each pair contains exactly 8 source bytes and 8 destination
            // floats. The loads/stores accept unaligned addresses.
            unsafe {
                let bytes = _mm_loadl_epi64(s.as_ptr().cast());
                let p = _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(bytes));
                let a = _mm256_permute_ps::<0xff>(p);
                let pm = _mm256_blend_ps::<0x88>(_mm256_mul_ps(p, a), p);
                let sum = _mm256_loadu_ps(d.as_ptr().cast());
                _mm256_storeu_ps(
                    d.as_mut_ptr().cast(),
                    _mm256_add_ps(sum, _mm256_mul_ps(pm, w)),
                );
            }
        }
        super::add_bytes(d_tail, s_tail, weight);
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn add_floats(dst: &mut [Pixel], src: &[Pixel], weight: f32) {
        let (dst, d_tail) = dst.as_chunks_mut::<2>();
        let (src, s_tail) = src.as_chunks::<2>();
        let w = _mm256_set1_ps(weight);
        for (d, s) in dst.iter_mut().zip(src) {
            // SAFETY: Both chunks contain exactly 8 contiguous floats. The slices
            // have disjoint storage and their alignment need only satisfy f32.
            unsafe {
                let sample = _mm256_loadu_ps(s.as_ptr().cast());
                let sum = _mm256_loadu_ps(d.as_ptr().cast());
                _mm256_storeu_ps(
                    d.as_mut_ptr().cast(),
                    _mm256_add_ps(sum, _mm256_mul_ps(sample, w)),
                );
            }
        }
        super::add_floats(d_tail, s_tail, weight);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: usize, height: usize) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                [
                    (i * 37) as u8,
                    (i * 107) as u8,
                    (i * 73) as u8,
                    (i * 13) as u8,
                ]
            })
            .collect()
    }

    // Direct 2D quadrature with bilinear samples in premultiplied space, independent
    // of the separable passes, integer taps, halo layout and shader weight literals.
    fn reference(src: &[u8], width: usize, height: usize, radius: f64, reflect: bool) -> Vec<u8> {
        let weight = |i: i32| (-0.5 * (f64::from(i) * 0.75).powi(2)).exp();
        let norm = (-4..=4).map(weight).sum::<f64>().powi(2);
        let coordinate = |p: f64, len: usize| {
            let max = (len - 1) as f64;
            if reflect && len > 1 {
                max - (p.rem_euclid(2.0 * max) - max).abs()
            } else {
                p.clamp(0.0, max)
            }
        };
        let mut result = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let mut sum = [0.0; 4];
                for j in -4..=4 {
                    for i in -4..=4 {
                        let sx = coordinate(x as f64 + f64::from(i) * radius / 4.0, width);
                        let sy = coordinate(y as f64 + f64::from(j) * radius / 4.0, height);
                        let w = weight(i) * weight(j) / norm;
                        for (dy, wy) in [(0, 1.0 - sy.fract()), (1, sy.fract())] {
                            for (dx, wx) in [(0, 1.0 - sx.fract()), (1, sx.fract())] {
                                let px = (sx as usize + dx).min(width - 1);
                                let py = (sy as usize + dy).min(height - 1);
                                let pixel = &src[4 * (py * width + px)..][..4];
                                let a = f64::from(pixel[3]) * wx * wy * w;
                                for c in 0..3 {
                                    sum[c] += f64::from(pixel[c]) * a;
                                }
                                sum[3] += a;
                            }
                        }
                    }
                }
                let alpha = sum[3].round() as u8;
                if alpha == 0 {
                    result.extend_from_slice(&[0; 4]);
                } else {
                    for &color in &sum[..3] {
                        result.push((color / sum[3]).round() as u8);
                    }
                    result.push(alpha);
                }
            }
        }
        result
    }

    #[test]
    fn matches_direct_gaussian_at_centers_with_fractional_offsets_and_edges() {
        for (width, height) in [(1, 7), (8, 1), (13, 9)] {
            let src = pattern(width, height);
            for radius in [0.0, 0.375, 5.5, 16.0, 117.25] {
                for reflect in [false, true] {
                    let expected = reference(&src, width, height, radius.into(), reflect);
                    let result = dispatch(&src, width, height, radius, reflect);
                    for (offset, (&actual, target)) in result.iter().zip(expected).enumerate() {
                        assert!(
                            actual.abs_diff(target) <= 1,
                            "{width}×{height}, radius {radius}, reflect={reflect}, byte {offset}: {actual} != {target}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn preserves_native_first_frame_and_ignores_transparent_color() {
        let first = RgbaImage::from_fn(19, 5, |x, _| {
            if x < 9 {
                image::Rgba([255, 31, 0, 0])
            } else {
                image::Rgba([0, 67, 211, 173])
            }
        });
        let second = RgbaImage::from_pixel(19, 5, image::Rgba([255, 0, 0, 255]));
        let source = RenderImage::new(vec![image::Frame::new(first), image::Frame::new(second)]);
        let second = source
            .blurred_frame(1, 6.25, ImageBlurEdge::Reflect)
            .unwrap();
        assert_eq!(second.frame_count(), 1);
        assert_eq!(second.as_bytes(0), source.as_bytes(1));
        let result = source.blurred_frame(0, 6.25, ImageBlurEdge::Clamp).unwrap();
        assert_eq!(result.size(0), source.size(0));
        assert_eq!(result.frame_count(), 1);
        let (pixels, _) = result.as_bytes(0).unwrap().as_chunks::<4>();
        assert!(pixels.iter().any(|p| (1..173).contains(&p[3])));
        for pixel in pixels {
            if pixel[3] == 0 {
                assert_eq!(*pixel, [0; 4]);
            } else {
                assert_eq!(pixel[..3], [0, 67, 211]);
            }
        }
    }

    #[test]
    fn rejects_missing_or_empty_frames_and_invalid_radii() {
        let source = RenderImage::new(vec![
            image::Frame::new(RgbaImage::new(1, 1)),
            image::Frame::new(RgbaImage::new(0, 1)),
        ]);
        for index in [1, 2] {
            assert!(
                source
                    .blurred_frame(index, 1., ImageBlurEdge::Clamp)
                    .is_none()
            );
        }
        for radius in [-1., f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                source
                    .blurred_frame(0, radius, ImageBlurEdge::Clamp)
                    .is_none()
            );
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_matches_scalar_for_whole_vectors_and_tails() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        for width in [1, 2, 3, 16, 17, 64, 65] {
            let src = pattern(width, 11);
            for reflect in [false, true] {
                let accelerated = dispatch(&src, width, 11, 8.375, reflect);
                let scalar = convolve(&src, width, 11, 8.375, reflect, add_bytes, add_floats);
                assert_eq!(accelerated, scalar);
            }
        }
    }
}
