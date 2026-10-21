//! Removes the grain of a path-traced image: an edge-aware "à-trous"
//! wavelet filter. It blurs the lighting (the image divided by the first
//! hit's color) in growing steps, but only between pixels that see similar
//! surfaces (similar normal and depth) with similar brightness, so edges
//! and texture stay sharp. Fewer samples get stronger filtering. Hair
//! pixels carry the strand direction instead of a normal, so only strands
//! running the same way are blended.

use glam::Vec3;

pub fn filter(color: &[Vec3], guide: &[(Vec3, Vec3, f32)], w: usize, h: usize, samples: u32) -> Vec<Vec3> {
    // Lighting only: texture detail comes back when multiplying by albedo.
    let mut light: Vec<Vec3> = color.iter().zip(guide).map(|(c, g)| *c / g.0.max(Vec3::splat(0.02))).collect();
    let passes = if samples >= 256 { 2 } else if samples >= 64 { 3 } else { 5 };
    let sigma_l = 4.0 / (samples as f32).sqrt();
    // Estimate a fixed local noise scale before the bilateral iterations.
    // A dark Monte Carlo outlier used to get an almost-zero range width,
    // rejecting every lit neighbour and freezing into a black speck/patch.
    let noise: Vec<f32> = (0..w*h).map(|i| {
        let (x, y) = (i%w, i/w);
        let (_, n0, d0) = guide[i];
        let (mut sum, mut square, mut count) = (0.0, 0.0, 0.0f32);
        for dy in -1..=1 { for dx in -1..=1 {
            let xx = super::post::reflect(x as i32+dx, w);
            let yy = super::post::reflect(y as i32+dy, h);
            let j = yy*w+xx;
            let (_, n1, d1) = guide[j];
            if n0.dot(n1) < 0.9 || (d0-d1).abs() > 0.02*d0.max(1e-3) { continue; }
            let l = luminance(light[j]);
            sum += l; square += l*l; count += 1.0;
        } }
        (square/count.max(1.0) - (sum/count.max(1.0)).powi(2)).max(0.0).sqrt()
    }).collect();
    const K: [f32; 3] = [3.0 / 8.0, 1.0 / 4.0, 1.0 / 16.0];
    for pass in 0..passes {
        let step = 1usize << pass;
        let src = light.clone();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (_, n0, d0) = guide[i];
                if n0 == Vec3::ZERO {
                    continue;
                }
                let l0 = luminance(src[i]);
                let (mut sum, mut wsum) = (Vec3::ZERO, 0.0);
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        // Dropping out-of-frame taps changes the kernel abruptly
                        // at each à-trous stride and puts steps into smooth ramps.
                        let xx = super::post::reflect(x as i32 + dx * step as i32, w);
                        let yy = super::post::reflect(y as i32 + dy * step as i32, h);
                        let j = yy * w + xx;
                        let (_, n1, d1) = guide[j];
                        if n1 == Vec3::ZERO {
                            continue;
                        }
                        // Sharp on normals, so fine bumps (pores) survive.
                        let wn = n0.dot(n1).max(0.0).powi(128);
                        let wd = (-(d0 - d1).abs() / (0.02 * d0.max(1e-3) * step as f32)).exp();
                        let range = sigma_l * (l0 + 0.05) + noise[i] + noise[j];
                        let wl = (-(l0 - luminance(src[j])).abs() / range).exp();
                        let k = K[dx.unsigned_abs() as usize] * K[dy.unsigned_abs() as usize] * wn * wd * wl;
                        sum += src[j] * k;
                        wsum += k;
                    }
                }
                light[i] = if wsum > 0.0 { sum / wsum } else { src[i] };
            }
        }
    }
    light.iter().zip(guide).map(|(l, g)| *l * g.0.max(Vec3::splat(0.02))).collect()
}

fn luminance(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dark_sampling_outliers_do_not_freeze_into_flat_patches() {
        let (w,h) = (32,16);
        let color: Vec<_> = (0..w*h).map(|i| Vec3::splat(if (i%w+i/w)%2==0 {0.0} else {0.6})).collect();
        let guide = vec![(Vec3::ONE,Vec3::Z,25.0);w*h];
        let out = filter(&color,&guide,w,h,64);
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
            let guides = vec![(tint.max(Vec3::splat(0.02)), Vec3::Z, 25.0); w*h];
            for samples in [4, 64, 256] {
                let filtered = filter(&color, &guides, w, h, samples);
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
            guides.push((Vec3::ONE,Vec3::Z,if x<w/2 {1.0} else {10.0}));
        } }
        let out=filter(&color,&guides,w,h,4);
        let variance=|c:&[Vec3]| c.iter().enumerate().map(|(i,c)| (c.x-if i%w<w/2 {0.3} else {0.8}).powi(2)).sum::<f32>();
        assert!(variance(&out)<variance(&color)*0.2);
        assert!(out.iter().enumerate().all(|(i,c)| (c.x-if i%w<w/2 {0.3} else {0.8}).abs()<0.03));
    }
}
