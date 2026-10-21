//! Shared HDR lens response and deterministic display grading for both tracers.
use glam::{Vec2, Vec3};
use serde_json::Value;
use super::lighting::{number, vector};

#[derive(Clone)]
pub struct Post {
    lift: Vec3,
    gamma: Vec3,
    gain: Vec3,
    shadow: Vec3,
    highlight: Vec3,
    saturation: f32,
    contrast: f32,
    grain: f32,
    seed: u32,
    sharpen: f32,
    distortion: f32,
    aberration: f32,
    streaks: f32,
    dirt: f32,
}
impl Default for Post {
    fn default() -> Self { Self { lift: Vec3::ZERO, gamma: Vec3::ONE, gain: Vec3::ONE, shadow: Vec3::ONE, highlight: Vec3::ONE, saturation: 1.0, contrast: 1.0, grain: 0.0, seed: 1, sharpen: 0.0, distortion: 0.0, aberration: 0.0, streaks: 0.0, dirt: 0.0 } }
}
fn lum(c: Vec3) -> f32 { c.dot(Vec3::new(0.2126, 0.7152, 0.0722)) }
fn hash(mut x: u32) -> f32 {
    x ^= x >> 16; x = x.wrapping_mul(0x7feb352d); x ^= x >> 15; x = x.wrapping_mul(0x846ca68b); x ^= x >> 16;
    (x & 0xffffff) as f32 / 16777216.0
}
/// Reflecting boundary conditions conserve flux even for lights at the edge.
pub(super) fn reflect(i: i32, n: usize) -> usize {
    let j = i.rem_euclid(2 * n as i32);
    if j < n as i32 { j as usize } else { (2 * n as i32 - 1 - j) as usize }
}
impl Post {
    pub fn parse(args: &Value) -> Result<Self, String> {
        let mut p = Self::default();
        let g = &args["grade"];
        if !g.is_null() && !g.is_object() { return Err("grade must be an object".into()); }
        if !g["look"].is_null() && !g["look"].is_string() { return Err("grade.look must be a preset name".into()); }
        match g["look"].as_str().unwrap_or("neutral") {
            "neutral" => {},
            "modern_warfare" => { p.shadow = Vec3::new(0.83, 0.96, 1.08); p.highlight = Vec3::new(1.08, 1.02, 0.91); p.saturation = 0.72; p.contrast = 1.08; },
            "desert_heat" => { p.shadow = Vec3::new(1.02, 0.94, 0.83); p.highlight = Vec3::new(1.12, 1.02, 0.85); p.saturation = 0.85; },
            "night_ops" => { p.shadow = Vec3::new(0.7, 0.99, 1.12); p.highlight = Vec3::new(0.85, 1.08, 1.02); p.saturation = 0.65; },
            "cinematic_teal_orange" => { p.shadow = Vec3::new(0.75, 1.02, 1.12); p.highlight = Vec3::new(1.16, 1.0, 0.8); p.saturation = 0.9; },
            _ => return Err("unknown grade.look".into()),
        }
        for (name, field, default, lo, hi) in [("lift", &mut p.lift, Vec3::ZERO, -0.5, 0.5), ("gamma", &mut p.gamma, Vec3::ONE, 0.1, 5.0), ("gain", &mut p.gain, Vec3::ONE, 0.0, 5.0)] {
            *field = vector(g, name, Some(default))?;
            if field.min_element() < lo || field.max_element() > hi { return Err(format!("grade.{name} components must be {lo}..{hi}")); }
        }
        p.saturation = number(g, "saturation", p.saturation, 0.0, 2.0)?;
        p.contrast = number(g, "contrast", p.contrast, 0.1, 3.0)?;
        p.grain = number(args, "grain", 0.0, 0.0, 0.15)?;
        let seed = number(args, "grain_seed", 1.0, 0.0, 16777215.0)?;
        if seed.fract() != 0.0 { return Err("grain_seed must be an integer".into()); }
        p.seed = seed as u32;
        p.sharpen = number(args, "sharpen", 0.0, 0.0, 1.0)?;
        p.distortion = number(&args["camera"], "distortion", 0.0, -0.3, 0.3)?;
        p.aberration = number(&args["camera"], "chromatic_aberration", 0.0, 0.0, 0.02)?;
        p.streaks = number(&args["bloom"], "streaks", 0.0, 0.0, 1.0)?;
        p.dirt = number(&args["bloom"], "dirt", 0.0, 0.0, 1.0)?;
        Ok(p)
    }
    pub fn optics(&self, src: &[Vec3], w: usize, h: usize) -> Vec<Vec3> {
        if self.distortion == 0.0 && self.aberration == 0.0 { return src.to_vec(); }
        let at = |p: Vec2| {
            let p = Vec2::new(p.x.clamp(0.0, w as f32 - 1.0), p.y.clamp(0.0, h as f32 - 1.0));
            let (x, y) = (p.x as usize, p.y as usize);
            src[y*w+x].lerp(src[y*w+(x+1).min(w-1)], p.x.fract()).lerp(src[(y+1).min(h-1)*w+x].lerp(src[(y+1).min(h-1)*w+(x+1).min(w-1)], p.x.fract()), p.y.fract())
        };
        let center = Vec2::new(w as f32, h as f32) * 0.5;
        (0..src.len()).map(|i| {
            let delta = Vec2::new((i%w) as f32 + 0.5, (i/w) as f32 + 0.5) - center;
            let r2 = (delta / center).length_squared() * 0.5;
            let base = center + delta * (1.0 + self.distortion*r2) - Vec2::splat(0.5);
            let offset = delta * (self.aberration * r2);
            Vec3::new(at(base + offset).x, at(base).y, at(base - offset).z)
        }).collect()
    }
    /// Scatter a fraction of highlight energy out of the core into a normalized PSF.
    pub fn glare(&self, mut src: Vec<Vec3>, bright: &[Vec3], w: usize, h: usize, strength: f32, aperture: (u32, f32)) -> Vec<Vec3> {
        let (blades, rotation) = aperture;
        let mut halo = vec![Vec3::ZERO; src.len()];
        for (radius, weight) in [(0.006, 0.5), (0.018, 0.3), (0.05, 0.2)] {
            let b = super::look::blur(bright, w, h, (w.min(h) as f32*radius).max(1.0));
            for (a,b) in halo.iter_mut().zip(b) { *a += b * weight * (1.0 - self.streaks); }
        }
        if self.streaks > 0.0 {
            let directions = if blades == 0 { 2 } else if blades.is_multiple_of(2) { blades/2 } else { blades };
            let radius = (w.min(h) as f32 * 0.08).max(2.0) as i32;
            let norm: f32 = (-radius..=radius).map(|t| (1.0-(t as f32).abs()/(radius+1) as f32).powi(2)).sum();
            // Symmetric reflected gathers have unit row sums; renormalize total
            // flux below to account for rounded diagonal streak coordinates.
            for a in 0..directions {
                let phi = a as f32 * std::f32::consts::PI / directions as f32 + rotation;
                for y in 0..h { for x in 0..w { for t in -radius..=radius {
                    let xx = reflect(x as i32 + (t as f32*phi.cos()).round() as i32, w);
                    let yy = reflect(y as i32 + (t as f32*phi.sin()).round() as i32, h);
                    let weight = (1.0-(t as f32).abs()/(radius+1) as f32).powi(2)/norm;
                    halo[y*w+x] += bright[yy*w+xx] * (weight*self.streaks/directions as f32);
                } } }
            }
        }
        if self.dirt > 0.0 {
            for (i,c) in halo.iter_mut().enumerate() {
                let x = (i%w) as f32/w as f32; let y = (i/w) as f32/h as f32;
                let speck = hash(((x*89.0) as u32).wrapping_add(((y*67.0) as u32)*193));
                *c *= 1.0 - self.dirt * (0.2 + 0.8 * speck.powi(8));
            }
        }
        let energy = bright.iter().copied().sum::<Vec3>();
        let total = halo.iter().copied().sum::<Vec3>();
        let correction = energy / total.max(Vec3::splat(1e-20));
        for ((c,b),g) in src.iter_mut().zip(bright).zip(halo) { *c += (g*correction - *b)*strength; }
        src
    }
    pub fn grade(&self, c: Vec3) -> Vec3 {
        let y = lum(c).clamp(0.0, 1.0);
        let c = c * self.shadow.lerp(self.highlight, y);
        let c = Vec3::splat(lum(c)).lerp(c, self.saturation);
        // Contrast in log exposure about middle grey. Affine contrast in linear
        // light subtracted 0.0288 at the trailer setting, clipping every dark
        // housing value below that into one flat black patch before encoding.
        // Two joined power curves preserve shadow/highlight gradients, black,
        // white and the grey pivot, with matching slopes at the pivot.
        let contrast = |v: f32| {
            if v <= 0.18 { 0.18 * (v.max(0.0) / 0.18).powf(self.contrast) }
            else { 1.0 - 0.82 * ((1.0-v).max(0.0) / 0.82).powf(self.contrast) }
        };
        let c = Vec3::new(contrast(c.x), contrast(c.y), contrast(c.z));
        let c = (c + self.lift).max(Vec3::ZERO);
        Vec3::new(c.x.powf(1.0/self.gamma.x), c.y.powf(1.0/self.gamma.y), c.z.powf(1.0/self.gamma.z)) * self.gain
    }
    pub fn texture(&self, src: &mut [Vec3], w: usize, h: usize) {
        if self.sharpen > 0.0 {
            let soft = super::look::blur(src, w, h, 0.8);
            let original = src.to_vec();
            for (i, (c,b)) in src.iter_mut().zip(soft).enumerate() {
                // An unconstrained unsharp mask overshoots both sides of a
                // silhouette, embossing rocks/tracks against fog. Limit each
                // channel to the original local range, before adding grain.
                // Read immutable input so bounds do not depend on scan order.
                let (x,y) = (i%w, i/w);
                let (mut lo, mut hi) = (original[i], original[i]);
                for dy in -1..=1 { for dx in -1..=1 {
                    let j = reflect(y as i32+dy,h)*w + reflect(x as i32+dx,w);
                    lo = lo.min(original[j]); hi = hi.max(original[j]);
                } }
                *c = (*c + (*c-b)*self.sharpen).clamp(lo,hi).max(Vec3::ZERO);
            }
        }
        if self.grain > 0.0 {
            for (i,c) in src.iter_mut().enumerate() {
                let n = hash((i as u32).wrapping_add(self.seed.wrapping_mul(7919))) + hash((i as u32).wrapping_add(self.seed.wrapping_mul(104729))) - 1.0;
                *c += Vec3::splat(n * self.grain * (0.15 + lum(*c).max(0.0).sqrt()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn sharpening_retains_detail_without_new_extrema() {
        let (w,h) = (32,8);
        let input: Vec<_> = (0..w*h).map(|i| Vec3::new(0.7,0.5,0.3) *
            (0.1 + 0.8*((i%w) as f32/31.0).powi(2))).collect();
        let mut out = input.clone();
        Post::parse(&json!({"sharpen":1})).unwrap().texture(&mut out,w,h);
        assert!((out[16]-input[16]).abs().max_element()>0.0001);
        for (i,c) in out.iter().enumerate() {
            let x = i%w;
            assert!(c.cmpge(input[x.saturating_sub(1)]).all());
            assert!(c.cmple(input[(x+1).min(w-1)]).all());
        }
        // Constants and single-pixel image dimensions must remain stable.
        for (w,h) in [(1,1),(1,8),(8,1)] {
            let mut constant = vec![Vec3::splat(0.4);w*h];
            Post::parse(&json!({"sharpen":1})).unwrap().texture(&mut constant,w,h);
            assert_eq!(constant,vec![Vec3::splat(0.4);w*h]);
        }
    }
    #[test]
    fn dark_silhouette_against_haze_has_no_post_ringing() {
        let (w, h) = (64, 48);
        let dark = Vec3::new(0.025, 0.035, 0.045);
        let haze = Vec3::new(0.9, 0.8, 0.65);
        for soft_edge in [false, true] {
            let coverage = |i: usize| {
                let p = Vec2::new((i%w) as f32 - 31.5, (i/w) as f32 - 23.5);
                if soft_edge { (p.length() - 13.0).clamp(0.0, 1.0) }
                else { f32::from(p.length() > 13.0) }
            };
            let color: Vec<_> = (0..w*h).map(|i| dark.lerp(haze, coverage(i))).collect();
            let guides: Vec<_> = (0..w*h).map(|i| (Vec3::ONE, Vec3::Z, if coverage(i)<0.5 {2.0} else {50.0})).collect();
            let filtered = super::super::denoise::filter(&color, &guides, w, h, 256);
            for strength in [0.12, 1.0] {
                // Keep the trailer tone/grade and glare, but remove intentional
                // stochastic grain and position-dependent vignette for bounds.
                let look = super::super::Look::parse(&json!({
                    "look":"trailer", "grain":0, "vignette":0, "sharpen":strength
                })).unwrap();
                let lo = look.finish(vec![dark; w*h], w, h, 1.0)[0];
                let hi = look.finish(vec![haze; w*h], w, h, 1.0)[0];
                let out = look.finish(filtered.clone(), w, h, 1.0);
                for (i, c) in out.iter().enumerate() { for channel in 0..3 {
                    assert!(c[channel] >= lo[channel] && c[channel] <= hi[channel],
                        "ring at pixel {i}, channel {channel}: {} outside {}..{} (sharpen {strength}, soft {soft_edge})",
                        c[channel], lo[channel], hi[channel]);
                } }
            }
        }
    }
    #[test]
    fn glare_conserves_hdr_energy_at_center_and_edges() {
        for index in [0, 16*32+16, 1023] {
            let mut c = vec![Vec3::ZERO; 1024]; c[index] = Vec3::new(30.0, 5.0, 2.0);
            for args in [json!({}), json!({"bloom":{"streaks":0.7,"dirt":0.8}})] {
                let p = Post::parse(&args).unwrap();
                let out = p.glare(c.clone(), &c, 32, 32, 0.25, (7, 0.2));
                assert!((out.iter().copied().sum::<Vec3>() - c[index]).abs().max_element() < 0.001);
                assert!(out.iter().all(|c| c.min_element() >= 0.0));
                assert!(out[index].x < c[index].x);
            }
        }
    }
    #[test]
    fn grades_are_monotonic_and_finishing_deterministic() {
        for name in ["neutral", "modern_warfare", "desert_heat", "night_ops", "cinematic_teal_orange"] {
            let p = Post::parse(&json!({"grade":{"look":name}})).unwrap();
            let mut last = Vec3::ZERO;
            for i in 0..1001 { let c = p.grade(Vec3::splat(i as f32/1000.0)); assert!(c.cmpge(last).all()); last=c; }
            let look = super::super::Look::parse(&json!({"look":name,"grain":0.02})).unwrap();
            let c = vec![Vec3::splat(0.3); 64];
            assert_eq!(look.finish(c.clone(),8,8,1.0),look.finish(c,8,8,1.0));
        }
    }
}
