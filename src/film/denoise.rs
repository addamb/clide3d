//! Removes the grain of a path-traced image: an edge-aware "à-trous"
//! wavelet filter. It blurs the lighting (the image divided by the first
//! hit's color) in growing steps, but only between pixels that see similar
//! surfaces (similar normal and depth) with similar brightness, so edges
//! and texture stay sharp. Hair pixels carry the strand direction instead
//! of a normal, so only strands running the same way are blended.
//!
//! How different "similar brightness" may be comes from each pixel's
//! measured noise when the tracer gives it (variance-guided, as SVGF):
//! the variance of the pixel's mean lighting, from the sum and square sum
//! of each sample's color over its own first-hit albedo. Clean pixels
//! (lit flats, shadow edges that every sample agrees on) keep their
//! neighbours out; noisy ones (soft shadows, glossy bounces) are smoothed
//! harder. Each pass carries the variance along (weights squared), so
//! later, wider passes only blend what is still noise. One approximation:
//! the filtered lighting is the pixel's mean color over its mean albedo,
//! not the mean of each sample's ratio; they differ only where the first
//! hit's albedo changes inside a pixel (texture and silhouette edges).
//!
//! Without a variance (fewer than 4 samples, or `guided` false: the
//! classic filter) the brightness width follows only the sample count.
//! Every pass reads the previous pass's whole picture and writes each
//! pixel on its own, so rows run on all cores and the result does not
//! depend on the thread count.

use glam::Vec3;

/// The brightness width in standard deviations of the pixel's lighting
/// (SVGF uses 4; 3 keeps soft shadows a little crisper). Tuned with
/// `graphics_checks::variance_denoiser_beats_classic_at_low_samples`.
const SIGMA_VARIANCE: f32 = 3.0;
/// How much the local spatial noise widens it: a floor, so a pixel whose
/// samples happened to agree still blends with its noisy neighbours.
const NOISE_FLOOR: f32 = 0.1;
/// Rows per parallel chunk.
const ROWS: usize = 8;

/// `guide`: per pixel the first hit's mean albedo, normal (zero: nothing
/// hit) and depth, and the variance of the pixel's mean lighting
/// luminance (zero everywhere: unknown).
pub fn filter(color: &[Vec3], guide: &[(Vec3, Vec3, f32, f32)], w: usize, h: usize, samples: u32, guided: bool) -> Vec<Vec3> {
    let guided = guided && guide.iter().any(|g| g.3 > 0.0);
    // Lighting only: texture detail comes back when multiplying by albedo.
    let mut light: Vec<Vec3> = color.iter().zip(guide).map(|(c, g)| *c / g.0.max(Vec3::splat(0.02))).collect();
    let mut variance: Vec<f32> = guide.iter().map(|g| g.3).collect();
    let passes = if samples >= 256 { 2 } else if samples >= 64 { 3 } else { 5 };
    let sigma_l = 4.0 / (samples as f32).sqrt();
    let at = |x: usize, y: usize, dx: i32, dy: i32| super::post::reflect(y as i32 + dy, h) * w + super::post::reflect(x as i32 + dx, w);
    // Neighbours on the same surface, for the local statistics below.
    let same = |i: usize, j: usize| {
        let ((_, n0, d0, _), (_, n1, d1, _)) = (guide[i], guide[j]);
        n0.dot(n1) >= 0.9 && (d0 - d1).abs() <= 0.02 * d0.max(1e-3)
    };
    // Estimate a fixed local noise scale before the bilateral iterations.
    // A dark Monte Carlo outlier used to get an almost-zero range width,
    // rejecting every lit neighbour and freezing into a black speck/patch.
    let noise: Vec<f32> = per_pixel(w, h, |i| {
        let (x, y) = (i % w, i / w);
        let (mut sum, mut square, mut count) = (0.0, 0.0, 0.0f32);
        for dy in -1..=1 { for dx in -1..=1 {
            let j = at(x, y, dx, dy);
            if !same(i, j) { continue; }
            let l = luminance(light[j]);
            sum += l; square += l*l; count += 1.0;
        } }
        (square/count.max(1.0) - (sum/count.max(1.0)).powi(2)).max(0.0).sqrt()
    });
    const K: [f32; 3] = [3.0 / 8.0, 1.0 / 4.0, 1.0 / 16.0];
    for pass in 0..passes {
        let step = 1usize << pass;
        // The variance is itself noisy: smooth it over the same surface
        // first (3x3 Gaussian), as SVGF does before each pass.
        let spread: Vec<f32> = if guided {
            per_pixel(w, h, |i| {
                let (x, y) = (i % w, i / w);
                let (mut sum, mut wsum) = (0.0, 0.0);
                for dy in -1i32..=1 { for dx in -1i32..=1 {
                    let j = at(x, y, dx, dy);
                    if !same(i, j) { continue; }
                    let k = [0.5, 0.25][dx.unsigned_abs() as usize] * [0.5, 0.25][dy.unsigned_abs() as usize];
                    sum += variance[j] * k; wsum += k;
                } }
                (sum / wsum.max(1e-6)).sqrt()
            })
        } else {
            Vec::new()
        };
        let (src, var) = (&light, &variance);
        let next: Vec<(Vec3, f32)> = per_pixel(w, h, |i| {
            let (x, y) = (i % w, i / w);
            let (_, n0, d0, _) = guide[i];
            if n0 == Vec3::ZERO {
                return (src[i], var[i]);
            }
            let l0 = luminance(src[i]);
            let (mut sum, mut wsum, mut vsum) = (Vec3::ZERO, 0.0, 0.0);
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    // Dropping out-of-frame taps changes the kernel abruptly
                    // at each à-trous stride and puts steps into smooth ramps.
                    let j = at(x, y, dx * step as i32, dy * step as i32);
                    let (_, n1, d1, _) = guide[j];
                    if n1 == Vec3::ZERO {
                        continue;
                    }
                    // Sharp on normals, so fine bumps (pores) survive.
                    let wn = n0.dot(n1).max(0.0).powi(128);
                    let wd = (-(d0 - d1).abs() / (0.02 * d0.max(1e-3) * step as f32)).exp();
                    let range = if guided {
                        SIGMA_VARIANCE * spread[i] + NOISE_FLOOR * (noise[i] + noise[j]) + 1e-4
                    } else {
                        sigma_l * (l0 + 0.05) + noise[i] + noise[j]
                    };
                    let wl = (-(l0 - luminance(src[j])).abs() / range).exp();
                    let k = K[dx.unsigned_abs() as usize] * K[dy.unsigned_abs() as usize] * wn * wd * wl;
                    sum += src[j] * k;
                    wsum += k;
                    vsum += k * k * var[j];
                }
            }
            if wsum > 0.0 { (sum / wsum, vsum / (wsum * wsum)) } else { (src[i], var[i]) }
        });
        (light, variance) = next.into_iter().unzip();
    }
    light.iter().zip(guide).map(|(l, g)| *l * g.0.max(Vec3::splat(0.02))).collect()
}

/// `f` of every pixel index, rows on all cores.
fn per_pixel<T: Send + Copy + Default>(w: usize, h: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let mut out = vec![T::default(); w * h];
    crate::clay::par_chunks(&mut out, w * ROWS, |c, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            *o = f(c * w * ROWS + k);
        }
    });
    out
}

pub(super) fn luminance(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dark_sampling_outliers_do_not_freeze_into_flat_patches() {
        let (w,h) = (32,16);
        let color: Vec<_> = (0..w*h).map(|i| Vec3::splat(if (i%w+i/w)%2==0 {0.0} else {0.6})).collect();
        let guide = vec![(Vec3::ONE,Vec3::Z,25.0,0.0);w*h];
        let out = filter(&color,&guide,w,h,64,true);
        let error = |c:&[Vec3]| c.iter().map(|v|(v.x-0.3).powi(2)).sum::<f32>();
        assert!(error(&out) < error(&color)*0.06);
        assert!(out.iter().all(|c| (c.x-0.3).abs()<0.15));
    }

    #[test]
    fn smooth_shadow_and_green_gradients_survive_denoise_and_grade() {
        use serde_json::json;
        let (w, h) = (512, 8);
        for tint in [Vec3::ONE, Vec3::new(0.12, 1.0, 0.35)] {
            let color: Vec<_> = (0..w*h).map(|i| {
                tint * (0.002 + 0.15 * (i % w) as f32 / (w-1) as f32)
            }).collect();
            for (samples, variance) in [(4, 0.0), (64, 0.0), (256, 0.0), (16, 1e-4), (64, 1e-3)] {
                let guides = vec![(tint.max(Vec3::splat(0.02)), Vec3::Z, 25.0, variance); w*h];
                let filtered = filter(&color, &guides, w, h, samples, true);
                // Check float precision before the one final display quantization.
                assert!(filtered[w..2*w].windows(2).all(|p| p[1].y > p[0].y));
                let look = super::super::Look::parse(&json!({
                    "look":"trailer", "grain":0, "sharpen":0, "vignette":0,
                    "camera":{"distortion":0,"chromatic_aberration":0}, "bloom":false
                })).unwrap();
                let pixels = look.finish(filtered, w, h, 1.0);
                let row = &pixels[w..2*w];
                assert!(row.windows(2).all(|p| p[1][1] >= p[0][1] && p[1][1]-p[0][1] <= 2));
                // The old affine grade crushed the first ~100 pixels to black.
                assert!(row[16][1] > 5, "shadow ramp was crushed: {:?}", row[16]);
                let levels: std::collections::BTreeSet<_> = row.iter().map(|p|p[1]).collect();
                assert!(levels.len() > 75, "only {} display levels", levels.len());
            }
        }
    }

    #[test]
    fn low_sample_filter_reduces_variance_and_respects_depth_edges() {
        let (w,h)=(32,16);
        let mut color=Vec::new(); let mut guides=Vec::new();
        for y in 0..h { for x in 0..w {
            let base=if x<w/2 {0.3} else {0.8};
            color.push(Vec3::splat(base+if (x+y)%2==0 {0.06} else {-0.06}));
            guides.push((Vec3::ONE,Vec3::Z,if x<w/2 {1.0} else {10.0},0.0));
        } }
        let truth=|i: usize| if i%w<w/2 {0.3} else {0.8};
        let variance=|c:&[Vec3]| c.iter().enumerate().map(|(i,c)| (c.x-truth(i)).powi(2)).sum::<f32>();
        let out=filter(&color,&guides,w,h,4,true);
        assert!(variance(&out)<variance(&color)*0.2);
        assert!(out.iter().enumerate().all(|(i,c)| (c.x-truth(i)).abs()<0.03));
        // The same with the measured noise (0.06 per pixel at 16 samples).
        for g in &mut guides { g.3 = 0.06f32.powi(2); }
        let out=filter(&color,&guides,w,h,16,true);
        assert!(variance(&out)<variance(&color)*0.2, "{} {}", variance(&out), variance(&color));
        assert!(out.iter().enumerate().all(|(i,c)| (c.x-truth(i)).abs()<0.03));
    }

    /// A lighting edge on one flat surface that every sample agrees on
    /// (a sharp shadow) stays sharp when its pixels are known to be clean,
    /// while the classic filter, which cannot tell, softens it.
    #[test]
    fn clean_lighting_edges_stay_sharp_with_variance() {
        let (w, h) = (48, 16);
        let color: Vec<_> = (0..w*h).map(|i| Vec3::splat(if i % w < w/2 { 0.08 } else { 0.5 })).collect();
        let guides = vec![(Vec3::ONE, Vec3::Z, 5.0, 1e-6); w*h];
        let step = |c: &[Vec3]| c[8*w + w/2].x - c[8*w + w/2 - 1].x;
        let guided = filter(&color, &guides, w, h, 16, true);
        let classic = filter(&color, &guides, w, h, 16, false);
        assert!(step(&guided) > 0.4, "guided edge {}", step(&guided));
        assert!(step(&classic) < step(&guided), "classic {} guided {}", step(&classic), step(&guided));
    }

    /// Rows run on all cores, but the picture is the same on one.
    #[test]
    fn denoise_is_thread_count_independent() {
        let (w, h) = (83, 61);
        let mut rng = 7u32;
        let mut next = || { rng = rng.wrapping_mul(1664525).wrapping_add(1013904223); (rng >> 8) as f32 / (1u32 << 24) as f32 };
        let color: Vec<_> = (0..w*h).map(|_| Vec3::new(next(), next(), next())).collect();
        let guides: Vec<_> = (0..w*h).map(|i| (Vec3::splat(0.3 + 0.5 * next()), if i % 17 == 0 { Vec3::ZERO } else { Vec3::Z }, 2.0 + (i % w) as f32 * 0.01, next() * 0.01)).collect();
        for guided in [true, false] {
            let one = crate::clay::with_threads(1, || filter(&color, &guides, w, h, 16, guided));
            let all = filter(&color, &guides, w, h, 16, guided);
            assert!(one.iter().zip(&all).all(|(a, b)| a.to_array().map(f32::to_bits) == b.to_array().map(f32::to_bits)));
        }
    }

    /// Time at 1600x900, 16 samples (five passes), on one thread and all
    /// cores: `cargo test --release denoise_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn denoise_timing() {
        let (w, h) = (1600, 900);
        let color: Vec<_> = (0..w*h).map(|i| Vec3::splat(((i * 7919) % 1000) as f32 / 1000.0)).collect();
        let guides: Vec<_> = (0..w*h).map(|i| (Vec3::splat(0.5), Vec3::Z, 3.0 + (i % w) as f32 * 1e-3, 0.01)).collect();
        for guided in [false, true] {
            let time = |threads: usize| {
                let t = std::time::Instant::now();
                crate::clay::with_threads(threads, || filter(&color, &guides, w, h, 16, guided));
                t.elapsed().as_secs_f32()
            };
            eprintln!("{w}x{h}, guided {guided}: one thread {:.2} s, all cores {:.2} s", time(1), time(usize::MAX));
        }
    }
}
