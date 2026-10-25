//! The viewport's lighting, taken from render_film's: the same environment
//! maps (the studio, overcast, sunset, golden_hour, forest, night and storm
//! presets, the physical sun and sky, or an HDR image) and the same lights.
//!
//! An `EnvProbe` is baked once per environment on the CPU: its diffuse
//! light as nine spherical-harmonic irradiance coefficients, and its
//! reflections as an equirectangular mip chain whose level k is the map
//! blurred by a GGX lobe of roughness k / 5. A film sun disc is not in
//! either: the viewport draws it as a directional light carrying the same
//! power, as render_film's suns are. `ViewLighting` is what a viewport pane
//! shades with: a probe, up to four suns and the local lights of a look.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, Mutex, OnceLock};

use glam::{Vec2, Vec3};
use serde_json::Value;

use crate::film::lighting::{Environment, Light};

/// Reflection mip levels: level k is prefiltered at roughness k / 5.
pub const ENV_LEVELS: usize = 6;
/// The widest mip 0; larger HDR images are box-filtered down to it.
const MAX_WIDTH: usize = 512;
/// Suns the viewport shades with directly; more are folded into the
/// irradiance.
pub const MAX_SUNS: usize = 4;
/// Side of the split-sum BRDF table (`brdf_lut`).
pub const BRDF_SIZE: usize = 32;
/// The camera ring light of the studio lighting (viewport only: it puts a
/// catchlight in every eye from any view), as irradiance.
pub const CATCHLIGHT: f32 = 0.12 * PI;

/// An environment baked for the viewport.
pub struct EnvProbe {
    /// Irradiance as SH9 coefficients (already convolved with the cosine
    /// lobe), in world space: E(n) = sum of sh[i] * Y_i(n).
    pub sh: [Vec3; 9],
    /// Rgba16Float levels (width, height, texels), in the map's own
    /// orientation; the shader applies `rotate`.
    pub mips: Vec<(u32, u32, Vec<u16>)>,
    /// Radians around +Y, as `Environment`'s.
    pub rotate: f32,
}

impl EnvProbe {
    pub fn bake(env: &Environment) -> EnvProbe {
        let parts = env.parts();
        let (mut w, mut h, mut pixels) = (parts.width, parts.height, parts.pixels.to_vec());
        while w > MAX_WIDTH && h >= 2 {
            (w, h, pixels) = halve(w, h, &pixels);
        }
        let sh = sh9(w, h, &pixels, parts.rotate);
        // A pyramid of the map for filtered importance sampling.
        let mut pyramid = vec![(w, h, pixels)];
        while let Some((pw, ph, p)) = pyramid.last()
            && *pw > 1
            && *ph > 1
        {
            let next = halve(*pw, *ph, p);
            pyramid.push(next);
        }
        let levels = ENV_LEVELS.min(pyramid.len());
        let mut mips = Vec::with_capacity(levels);
        for k in 0..levels {
            let (lw, lh) = ((w >> k).max(1), (h >> k).max(1));
            let mut texels = vec![[0u16; 4]; lw * lh];
            if k == 0 {
                for (t, c) in texels.iter_mut().zip(&pyramid[0].2) {
                    *t = rgba16(*c);
                }
            } else {
                let rough = k as f32 / (ENV_LEVELS - 1) as f32;
                let pyramid = &pyramid;
                crate::clay::par_chunks(&mut texels, lw, |y, row| {
                    let cos = (PI * (y as f32 + 0.5) / lh as f32).cos() as f64;
                    for (x, t) in row.iter_mut().enumerate() {
                        let n = Environment::direction((x as f32 + 0.5) / lw as f32, cos);
                        *t = rgba16(prefilter(pyramid, n, rough));
                    }
                });
            }
            mips.push((lw as u32, lh as u32, texels.into_iter().flatten().collect()));
        }
        EnvProbe { sh, mips, rotate: parts.rotate }
    }

    /// The highest mip level.
    pub fn max_lod(&self) -> f32 {
        (self.mips.len() - 1) as f32
    }
}

/// Every texel's solid angle in a `w` x `h` equirectangular map, by row
/// (as `Environment::map` weighs them).
fn row_solid_angle(y: usize, w: usize, h: usize) -> f32 {
    (std::f64::consts::TAU / w as f64 * ((std::f64::consts::PI * y as f64 / h as f64).cos() - (std::f64::consts::PI * (y + 1) as f64 / h as f64).cos())) as f32
}

/// The map at half size (rounded down), each texel the solid-angle weighted
/// mean of the 2x2 it covers, so the total light is kept.
fn halve(w: usize, h: usize, pixels: &[Vec3]) -> (usize, usize, Vec<Vec3>) {
    let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        let rows = [(2 * y).min(h - 1), (2 * y + 1).min(h - 1)];
        let weights = rows.map(|r| row_solid_angle(r, w, h));
        for x in 0..hw {
            let cols = [(2 * x).min(w - 1), (2 * x + 1).min(w - 1)];
            let mut sum = Vec3::ZERO;
            for (r, wt) in rows.iter().zip(weights) {
                for c in cols {
                    sum += pixels[r * w + c] * wt;
                }
            }
            out.push(sum / (2.0 * (weights[0] + weights[1])).max(1e-30));
        }
    }
    (hw, hh, out)
}

/// The nine real spherical harmonics (bands 0-2) at unit `d`.
pub fn sh_basis(d: Vec3) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * d.y,
        0.488_603 * d.z,
        0.488_603 * d.x,
        1.092_548 * d.x * d.y,
        1.092_548 * d.y * d.z,
        0.315_392 * (3.0 * d.z * d.z - 1.0),
        1.092_548 * d.x * d.z,
        0.546_274 * (d.x * d.x - d.y * d.y),
    ]
}

/// The cosine lobe's SH weights per band (Ramamoorthi and Hanrahan).
const BAND: [f32; 9] = [PI, 2.0 * PI / 3.0, 2.0 * PI / 3.0, 2.0 * PI / 3.0, PI / 4.0, PI / 4.0, PI / 4.0, PI / 4.0, PI / 4.0];

/// Irradiance SH9 of a map turned `rotate` radians, each texel weighted by
/// its exact solid angle.
fn sh9(w: usize, h: usize, pixels: &[Vec3], rotate: f32) -> [Vec3; 9] {
    let mut sh = [Vec3::ZERO; 9];
    for y in 0..h {
        let solid = row_solid_angle(y, w, h);
        let cos = (PI * (y as f32 + 0.5) / h as f32).cos() as f64;
        for x in 0..w {
            // Map space to world space as `Environment::sample` does.
            let d = Environment::direction((x as f32 + 0.5) / w as f32 + rotate / TAU, cos);
            let c = pixels[y * w + x] * solid;
            for (s, b) in sh.iter_mut().zip(sh_basis(d)) {
                *s += c * b;
            }
        }
    }
    for (s, a) in sh.iter_mut().zip(BAND) {
        *s *= a;
    }
    sh
}

/// Adds a directional light of irradiance `e` from `dir` to irradiance SH.
fn add_sun(sh: &mut [Vec3; 9], dir: Vec3, e: Vec3) {
    for ((s, b), a) in sh.iter_mut().zip(sh_basis(dir)).zip(BAND) {
        *s += e * b * a;
    }
}

/// Irradiance from SH9 for unit normal `n` (what the shader computes).
#[cfg(test)]
pub fn sh_irradiance(sh: &[Vec3; 9], n: Vec3) -> Vec3 {
    sh.iter().zip(sh_basis(n)).fold(Vec3::ZERO, |e, (s, b)| e + *s * b).max(Vec3::ZERO)
}

/// Bilinear lookup of an equirectangular map in its own orientation, as
/// `Environment::at` (u wraps, v clamps) and the GPU sampler do.
pub fn bilinear(w: usize, h: usize, pixels: &[Vec3], d: Vec3) -> Vec3 {
    let u = (d.z.atan2(d.x) / TAU).rem_euclid(1.0);
    let v = Vec2::new(d.x, d.z).length().atan2(d.y) / PI;
    let x = u * w as f32 - 0.5;
    let y = (v * h as f32 - 0.5).clamp(0.0, h as f32 - 1.0);
    let (x0, y0) = (x.floor() as isize, y.floor() as usize);
    let at = |xx: isize, yy: usize| pixels[yy * w + xx.rem_euclid(w as isize) as usize];
    let y1 = (y0 + 1).min(h - 1);
    let top = at(x0, y0).lerp(at(x0 + 1, y0), x - x.floor());
    let bottom = at(x0, y1).lerp(at(x0 + 1, y1), x - x.floor());
    top.lerp(bottom, y - y.floor())
}

/// Point `i` of `n` in the Hammersley set.
fn hammersley(i: u32, n: u32) -> Vec2 {
    Vec2::new((i as f32 + 0.5) / n as f32, i.reverse_bits() as f32 * 2.328_306_4e-10)
}

/// A GGX half vector around +Z for roughness alpha `a` (D-proportional).
fn ggx_half(u: Vec2, a: f32) -> Vec3 {
    let cos = ((1.0 - u.x) / (1.0 + (a * a - 1.0) * u.x)).max(0.0).sqrt();
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let phi = TAU * u.y;
    Vec3::new(sin * phi.cos(), sin * phi.sin(), cos)
}

/// A frame (t, b) completing unit `n`.
fn basis(n: Vec3) -> (Vec3, Vec3) {
    let t = if n.x.abs() > 0.9 { Vec3::Y } else { Vec3::X };
    let t = (t - n * n.dot(t)).normalize();
    (t, n.cross(t))
}

const PREFILTER_SAMPLES: u32 = 64;

/// The map seen in direction `n` through a GGX lobe of `rough` (n = v = r,
/// as the split-sum approximation takes it), by filtered importance
/// sampling: each sample reads the pyramid level whose texels are about
/// the sample's share of the lobe.
fn prefilter(pyramid: &[(usize, usize, Vec<Vec3>)], n: Vec3, rough: f32) -> Vec3 {
    let a = rough * rough;
    let (t, b) = basis(n);
    let (w0, h0) = (pyramid[0].0, pyramid[0].1);
    let texel = 4.0 * PI / (w0 * h0) as f32;
    let (mut sum, mut weight) = (Vec3::ZERO, 0.0);
    for i in 0..PREFILTER_SAMPLES {
        let hl = ggx_half(hammersley(i, PREFILTER_SAMPLES), a);
        let h = t * hl.x + b * hl.y + n * hl.z;
        let l = 2.0 * n.dot(h) * h - n;
        let nl = n.dot(l);
        if nl <= 0.0 {
            continue;
        }
        // pdf of l = D(h) / 4 when n = v.
        let nh = hl.z;
        let dd = nh * nh * (a * a - 1.0) + 1.0;
        let pdf = a * a / (PI * dd * dd) / 4.0;
        let lod = (0.5 * (1.0 / (PREFILTER_SAMPLES as f32 * pdf.max(1e-6)) / texel).log2() + 1.0).clamp(0.0, (pyramid.len() - 1) as f32);
        let (lo, f) = (lod.floor() as usize, lod.fract());
        let at = |k: usize| {
            let (w, h, p) = &pyramid[k.min(pyramid.len() - 1)];
            bilinear(*w, *h, p, l)
        };
        sum += at(lo).lerp(at(lo + 1), f) * nl;
        weight += nl;
    }
    sum / weight.max(1e-6)
}

/// f32 to IEEE half-float bits, rounding to nearest even; out of range
/// clamps to the largest half, NaN to 0.
pub fn f16_bits(x: f32) -> u16 {
    let x = if x.is_nan() { 0.0 } else { x.clamp(-65504.0, 65504.0) };
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x7f_ffff;
    let round = |value: u32, shift: u32| {
        let rem = man_bits(value, shift);
        let half = 1u32 << (shift - 1);
        (value >> shift) + u32::from(rem > half || (rem == half && (value >> shift) & 1 == 1))
    };
    fn man_bits(value: u32, shift: u32) -> u32 {
        value & ((1 << shift) - 1)
    }
    if exp <= 0 {
        // Subnormal (or zero): the implicit bit joins the mantissa.
        if exp < -10 {
            return sign;
        }
        return sign | round(man | 0x80_0000, (14 - exp) as u32) as u16;
    }
    sign | round(((exp as u32) << 23) | man, 13) as u16
}

/// IEEE half-float bits to f32 (for checks).
#[cfg(test)]
pub fn f16_value(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
    }
}

fn rgba16(c: Vec3) -> [u16; 4] {
    [f16_bits(c.x), f16_bits(c.y), f16_bits(c.z), f16_bits(1.0)]
}

/// The split-sum BRDF table (Karis): Rg16Float texels (scale, bias) for
/// f0 at n.v = (x + 0.5) / 32 and roughness (y + 0.5) / 32, with the same
/// Smith-Schlick k = (r + 1)^2 / 8 as the viewport's analytic highlight.
pub fn brdf_lut() -> &'static [u16] {
    static LUT: OnceLock<Vec<u16>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut out = Vec::with_capacity(BRDF_SIZE * BRDF_SIZE * 2);
        for y in 0..BRDF_SIZE {
            for x in 0..BRDF_SIZE {
                let [a, b] = brdf(((x as f32 + 0.5) / BRDF_SIZE as f32).max(1e-3), (y as f32 + 0.5) / BRDF_SIZE as f32);
                out.extend([f16_bits(a), f16_bits(b)]);
            }
        }
        out
    })
}

/// One texel of `brdf_lut`.
fn brdf(nv: f32, rough: f32) -> [f32; 2] {
    const N: u32 = 128;
    let v = Vec3::new((1.0 - nv * nv).max(0.0).sqrt(), 0.0, nv);
    let a = rough * rough;
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let g1 = |c: f32| c / (c * (1.0 - k) + k);
    let (mut scale, mut bias) = (0.0, 0.0);
    for i in 0..N {
        let h = ggx_half(hammersley(i, N), a);
        let l = 2.0 * v.dot(h) * h - v;
        let (nl, nh, vh) = (l.z, h.z, v.dot(h).max(0.0));
        if nl <= 0.0 {
            continue;
        }
        let vis = g1(nv) * g1(nl) * vh / (nh * nv).max(1e-6);
        let fc = (1.0 - vh).powi(5);
        scale += (1.0 - fc) * vis;
        bias += fc * vis;
    }
    [scale / N as f32, bias / N as f32]
}

/// What a pane is lit by.
pub struct ViewLighting {
    /// None draws the viewport's old fixed studio (`classic`).
    pub probe: Option<Arc<EnvProbe>>,
    /// Irradiance SH9: the probe's, plus suns past `MAX_SUNS`.
    pub sh: [Vec3; 9],
    /// Directional lights (toward the light, irradiance on a surface facing
    /// it), brightest first; the first is the one that casts shadows.
    pub suns: Vec<(Vec3, Vec3)>,
    /// Point-like lights of the look: (position, radius, intensity), as
    /// `ViewportCallback::lights`.
    pub locals: Vec<(Vec3, f32, Vec3)>,
    /// The studio lighting: the camera's catchlight, dimming when objects
    /// carry lights, and `Shadows::key_dir` moving the key.
    pub studio: bool,
    /// What it is, for replies and the menu.
    pub label: String,
}

impl ViewLighting {
    /// render_film's default (the studio environment and its three studio
    /// lights), with the viewport's catchlight.
    pub fn studio_default() -> Arc<ViewLighting> {
        static STUDIO: OnceLock<Arc<ViewLighting>> = OnceLock::new();
        STUDIO
            .get_or_init(|| {
                let look = crate::film::Look::default();
                Arc::new(ViewLighting { studio: true, label: "studio".into(), ..ViewLighting::from_look(&look, &Value::Null) })
            })
            .clone()
    }

    /// The viewport's lighting before it took render_film's: a fixed sky
    /// gradient, key, fill and catchlight (kept to compare against).
    pub fn classic() -> Arc<ViewLighting> {
        static CLASSIC: OnceLock<Arc<ViewLighting>> = OnceLock::new();
        CLASSIC.get_or_init(|| Arc::new(ViewLighting { probe: None, sh: [Vec3::ZERO; 9], suns: Vec::new(), locals: Vec::new(), studio: true, label: "classic".into() })).clone()
    }

    /// The lighting of a render_film look; `environment` (its resolved
    /// argument) keys the probe cache.
    pub fn from_look(look: &crate::film::Look, environment: &Value) -> ViewLighting {
        let probe = probe(&look.environment, environment);
        let mut suns: Vec<(Vec3, Vec3)> = Vec::new();
        let mut locals = Vec::new();
        // An environment's sun disc as a directional light of the same power.
        if let Some((dir, size, radiance)) = look.environment.parts().sun {
            suns.push((dir, radiance * (TAU * (1.0 - size.cos()))));
        }
        for l in &look.lights {
            match *l {
                Light::Sun { direction, power, .. } => suns.push((direction, power)),
                Light::Point { position, power } => locals.push((position, 1e-3, power)),
                Light::Sphere { position, radius, power } => locals.push((position, radius, power)),
                // Its intensity facing it, from its centre.
                Light::Area { position, size, power, .. } => locals.push((position, size, power * (PI * size * size))),
            }
        }
        let luminance = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        suns.sort_by(|a, b| luminance(b.1).total_cmp(&luminance(a.1)));
        let mut sh = probe.sh;
        for (dir, e) in suns.split_off(suns.len().min(MAX_SUNS)) {
            add_sun(&mut sh, dir, e);
        }
        ViewLighting { probe: Some(probe), sh, suns, locals, studio: false, label: "film look".into() }
    }

    /// `set_display lighting`: "studio", "classic", an environment preset
    /// (as render_film's environment, with its studio lights), a look
    /// preset, or an object of render_film's lighting arguments
    /// (environment, lights, extra_lights, look).
    pub fn parse(v: &Value) -> Result<Arc<ViewLighting>, String> {
        const KEYS: [&str; 4] = ["environment", "lights", "extra_lights", "look"];
        let (args, label) = match v {
            Value::String(s) if s == "studio" => return Ok(Self::studio_default()),
            Value::String(s) if s == "classic" => return Ok(Self::classic()),
            Value::String(s) if ["neutral", "trailer", "modern_warfare", "desert_heat", "night_ops", "cinematic_teal_orange"].contains(&s.as_str()) => (serde_json::json!({ "look": s }), s.clone()),
            Value::String(s) => (serde_json::json!({ "environment": s }), s.clone()),
            Value::Object(o) => {
                if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
                    return Err(format!("lighting.{k} is not a lighting setting (environment, lights, extra_lights, look)"));
                }
                (v.clone(), describe(v))
            }
            _ => return Err("lighting must be studio, classic, an environment or look preset name, or {environment, lights, extra_lights, look}".into()),
        };
        Self::from_args(&args).map(|l| Arc::new(ViewLighting { label, ..l }))
    }

    /// The lighting part of render_film arguments (anything else in them
    /// is ignored).
    pub fn from_args(args: &Value) -> Result<ViewLighting, String> {
        let mut only = serde_json::json!({});
        for k in ["environment", "lights", "extra_lights", "look"] {
            if !args[k].is_null() {
                only[k] = args[k].clone();
            }
        }
        let look = crate::film::Look::parse(&only).map_err(|e| format!("lighting: {e}"))?;
        let resolved = crate::film::look::resolved_args(&only)?;
        Ok(ViewLighting { label: describe(&only), ..ViewLighting::from_look(&look, &resolved["environment"]) })
    }
}

/// A short name for lighting arguments.
fn describe(v: &Value) -> String {
    match (&v["look"], &v["environment"]) {
        (Value::String(l), _) => l.clone(),
        (_, Value::String(e)) => e.clone(),
        (_, Value::Object(o)) => o.get("preset").and_then(Value::as_str).map_or_else(|| "hdr".to_string(), str::to_string),
        _ => "studio".into(),
    }
}

/// The baked probe for an environment, cached by its arguments (an HDR
/// path or a preset with its rotation and intensity): baking takes tens of
/// milliseconds.
fn probe(env: &Environment, key: &Value) -> Arc<EnvProbe> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<EnvProbe>>>> = OnceLock::new();
    let key = match key {
        Value::Null => "\"studio\"".to_string(),
        v => v.to_string(),
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Some(p) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return p.clone();
    }
    let p = Arc::new(EnvProbe::bake(env));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    // A handful of environments at a time; an HDR probe is a few MB. The
    // default studio stays: `ViewLighting::studio_default` holds it anyway,
    // and keeping it keeps one probe for it.
    if cache.len() >= 16 {
        cache.retain(|k, _| k == "\"studio\"");
    }
    cache.entry(key).or_insert(p).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Irradiance at `n` by brute force over the map's texels.
    fn quadrature(env: &Environment, n: Vec3) -> Vec3 {
        let p = env.parts();
        let mut e = Vec3::ZERO;
        for y in 0..p.height {
            let solid = row_solid_angle(y, p.width, p.height);
            let cos = (PI * (y as f32 + 0.5) / p.height as f32).cos() as f64;
            for x in 0..p.width {
                let d = Environment::direction((x as f32 + 0.5) / p.width as f32 + p.rotate / TAU, cos);
                e += p.pixels[y * p.width + x] * solid * n.dot(d).max(0.0);
            }
        }
        e
    }

    fn normals() -> Vec<Vec3> {
        let mut n: Vec<Vec3> = vec![Vec3::Y, Vec3::NEG_Y, Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z];
        n.extend([[1.0, 1.0, 1.0], [-1.0, 1.0, 0.3], [0.5, -1.0, 0.2], [0.2, 0.3, -1.0], [-0.7, -0.4, -0.6], [0.9, 0.2, -0.3], [0.1, 0.95, 0.1], [-0.3, 0.1, 0.9]].map(|v| Vec3::from(v).normalize()));
        n
    }

    #[test]
    fn sh9_irradiance_matches_quadrature() {
        for (name, tolerance) in [("studio", 0.03), ("overcast", 0.03), ("sunset", 0.04), ("golden_hour", 0.03), ("forest", 0.03), ("night", 0.03), ("storm", 0.03)] {
            let env = Environment::preset(name).unwrap();
            let probe = EnvProbe::bake(&env);
            // Relative to the brightest irradiance, so the dim side of a
            // map with a bright band is judged fairly. SH9 rings a little on
            // sunset's narrow horizon glow (3.3%; the others are under 2%).
            let truth: Vec<Vec3> = normals().into_iter().map(|n| quadrature(&env, n)).collect();
            let peak = truth.iter().map(|e| e.max_element()).fold(0.0, f32::max);
            let worst = normals().into_iter().zip(&truth).map(|(n, want)| (sh_irradiance(&probe.sh, n) - *want).abs().max_element() / peak).fold(0.0, f32::max);
            eprintln!("{name}: SH9 irradiance within {:.2}% of the peak", worst * 100.0);
            for (n, want) in normals().into_iter().zip(&truth) {
                let got = sh_irradiance(&probe.sh, n);
                let err = (got - *want).abs().max_element() / peak;
                assert!(err < tolerance, "{name} at {n}: SH {got} vs quadrature {want} ({:.1}% of the peak)", err * 100.0);
            }
        }
        // A rotated environment turns its irradiance with it.
        // (Forest's canopy gaps vary around the horizon; the others do not.)
        let env = Environment::parse(&json!({ "preset": "forest", "rotate": 40 })).unwrap();
        let probe = EnvProbe::bake(&env);
        let peak = normals().into_iter().map(|n| quadrature(&env, n).max_element()).fold(0.0, f32::max);
        for n in normals() {
            let (got, want) = (sh_irradiance(&probe.sh, n), quadrature(&env, n));
            assert!((got - want).abs().max_element() < 0.04 * peak, "rotated at {n}: {got} vs {want}");
        }
        // And it is the unrotated map's, turned: the sun side moved.
        let plain = EnvProbe::bake(&Environment::preset("forest").unwrap());
        assert!(normals().into_iter().any(|n| (sh_irradiance(&probe.sh, n) - sh_irradiance(&plain.sh, n)).abs().max_element() > 1e-3 * peak), "forest looks the same turned");
        let turned = glam::Quat::from_rotation_y(40f32.to_radians());
        for n in normals() {
            let (a, b) = (sh_irradiance(&probe.sh, n), sh_irradiance(&plain.sh, turned * n));
            assert!((a - b).abs().max_element() < 1e-3 * peak, "rotation at {n}: {a} vs {b}");
        }
    }

    #[test]
    fn prefilter_mip0_matches_environment_at() {
        for args in [json!("sunset"), json!({ "preset": "forest", "rotate": 40 }), json!("storm")] {
            let mut env = Environment::parse(&args).unwrap();
            let probe = EnvProbe::bake(&env);
            let (w, h, bits) = &probe.mips[0];
            let mip0: Vec<Vec3> = bits.chunks_exact(4).map(|c| Vec3::new(f16_value(c[0]), f16_value(c[1]), f16_value(c[2]))).collect();
            // Environment::at without its sun disc (the viewport's is a light).
            let sun = env.parts().sun;
            for i in 0..500 {
                let u = hammersley(i, 500);
                let d = Vec3::new((TAU * u.y).cos() * (1.0 - (2.0 * u.x - 1.0).powi(2)).sqrt(), 2.0 * u.x - 1.0, (TAU * u.y).sin() * (1.0 - (2.0 * u.x - 1.0).powi(2)).sqrt());
                if sun.is_some_and(|(s, size, _)| d.dot(s) >= (size * 1.5).cos()) {
                    continue;
                }
                let want = env.at(d);
                // The shader turns the lookup by -rotate.
                let local = glam::Quat::from_rotation_y(probe.rotate) * d;
                let got = bilinear(*w as usize, *h as usize, &mip0, local);
                assert!((got - want).abs().max_element() <= 2e-3 * want.max_element().max(0.05), "{args} at {d}: {got} vs {want}");
            }
            let _ = &mut env;
        }
    }

    #[test]
    fn prefilter_mips_conserve_mean() {
        for name in ["studio", "sunset", "forest", "storm"] {
            let probe = EnvProbe::bake(&Environment::preset(name).unwrap());
            assert_eq!(probe.mips.len(), ENV_LEVELS, "{name}");
            let mean = |(w, h, bits): &(u32, u32, Vec<u16>)| {
                let (w, h) = (*w as usize, *h as usize);
                let mut sum = Vec3::ZERO;
                for y in 0..h {
                    for x in 0..w {
                        let c = &bits[(y * w + x) * 4..][..3];
                        sum += Vec3::new(f16_value(c[0]), f16_value(c[1]), f16_value(c[2])) * row_solid_angle(y, w, h);
                    }
                }
                sum / (4.0 * PI)
            };
            let base = mean(&probe.mips[0]);
            for (k, m) in probe.mips.iter().enumerate().skip(1) {
                let got = mean(m);
                assert!((got - base).abs().max_element() <= 0.02 * base.max_element(), "{name} level {k}: mean {got} vs {base}");
            }
        }
    }

    /// The physical sky reaches the viewport through the same
    /// environment: its sun becomes the first directional light (the one
    /// that casts shadows) with the irradiance the tracers sample, and its
    /// map bakes into SH and mips like any preset's.
    #[test]
    fn physical_sky_lights_the_viewport() {
        let args = json!({ "preset": "physical", "sun_elevation": 20, "sun_azimuth": 120 });
        let sky = crate::film::sky::Sky::parse(&args, crate::film::sky::Sky::named("physical").unwrap()).unwrap();
        let l = ViewLighting::from_args(&json!({ "environment": args, "lights": "none" })).unwrap();
        assert_eq!(l.label, "physical");
        assert_eq!(l.suns.len(), 1);
        let (dir, e) = l.suns[0];
        assert!((dir - sky.sun_direction()).length() < 1e-5, "{dir}");
        let want = sky.bake().sun.unwrap();
        assert!((e - want).abs().max_element() < 1e-3 * want.max_element(), "{e} vs {want}");
        let env = Environment::parse(&args).unwrap();
        let probe = l.probe.as_ref().unwrap();
        assert_eq!(probe.mips.len(), ENV_LEVELS);
        // The sky's diffuse light, against brute force.
        let truth: Vec<Vec3> = normals().into_iter().map(|n| quadrature(&env, n)).collect();
        let peak = truth.iter().map(|e| e.max_element()).fold(0.0, f32::max);
        let worst = normals().into_iter().zip(&truth).map(|(n, want)| (sh_irradiance(&probe.sh, n) - *want).abs().max_element() / peak).fold(0.0, f32::max);
        eprintln!("physical sky: SH9 irradiance within {:.2}% of the peak", worst * 100.0);
        assert!(worst < 0.04, "SH9 off by {:.1}% of the peak", worst * 100.0);
        // The shortcut names light the viewport too.
        for name in ["physical", "sky_noon", "sky_afternoon", "sky_golden", "sky_sunset"] {
            let l = ViewLighting::parse(&json!(name)).unwrap();
            assert!(l.probe.is_some() && !l.suns.is_empty(), "{name}");
        }
    }

    #[test]
    fn env_sun_becomes_directional_irradiance() {
        let l = ViewLighting::from_args(&json!({ "environment": "sunset", "lights": "none" })).unwrap();
        assert_eq!(l.suns.len(), 1);
        let (dir, e) = l.suns[0];
        assert!((dir - Vec3::new(-0.8, 0.08, 0.5).normalize()).length() < 1e-6);
        assert!((e - Vec3::new(4.0, 0.9, 0.25)).abs().max_element() < 1e-4, "{e}");
        let bright = ViewLighting::from_args(&json!({ "environment": { "preset": "sunset", "intensity": 2 }, "lights": "none" })).unwrap();
        assert!((bright.suns[0].1 - e * 2.0).abs().max_element() < 1e-4);
        // A sun with no environment sun: overcast has none.
        assert!(ViewLighting::from_args(&json!({ "environment": "overcast", "lights": "none" })).unwrap().suns.is_empty());
    }

    #[test]
    fn studio_lighting_uses_film_lights() {
        let l = ViewLighting::studio_default();
        assert!(l.studio && l.probe.is_some());
        assert_eq!(l.suns.len(), crate::film::LIGHTS.len());
        for (dir, _, power) in crate::film::LIGHTS {
            assert!(l.suns.iter().any(|(d, e)| (*d - dir.normalize()).length() < 1e-6 && (*e - power).length() < 1e-6), "no sun {dir} {power}");
        }
        // The key (render_film's, which the viewport shadows) comes first.
        assert!((l.suns[0].0 - crate::film::KEY_DIR.normalize()).length() < 1e-6);
        // The same probe as render_film's default environment, baked once.
        assert!(Arc::ptr_eq(l.probe.as_ref().unwrap(), ViewLighting::parse(&json!({ "environment": "studio" })).unwrap().probe.as_ref().unwrap()));
    }

    #[test]
    fn extra_suns_fold_into_irradiance() {
        let sun = |d: [f32; 3], i: f32| json!({ "type": "sun", "direction": d, "intensity": i });
        let lights: Vec<Value> = (0..6).map(|i| sun([i as f32 - 2.5, 1.0, 0.3], 1.0 + i as f32)).collect();
        let l = ViewLighting::from_args(&json!({ "environment": "night", "lights": lights })).unwrap();
        assert_eq!(l.suns.len(), MAX_SUNS);
        // Brightest first; the two dimmest became ambient light.
        assert!(l.suns.windows(2).all(|w| w[0].1.x >= w[1].1.x));
        assert!((l.suns[3].1.x - 3.0).abs() < 1e-6);
        let base = l.probe.as_ref().unwrap().sh;
        assert!(sh_irradiance(&l.sh, Vec3::Y).x > sh_irradiance(&base, Vec3::Y).x + 1.0);
    }

    #[test]
    fn brdf_lut_is_bounded_and_monotone() {
        let lut = brdf_lut();
        assert_eq!(lut.len(), BRDF_SIZE * BRDF_SIZE * 2);
        let at = |x: usize, y: usize| [f16_value(lut[(y * BRDF_SIZE + x) * 2]), f16_value(lut[(y * BRDF_SIZE + x) * 2 + 1])];
        for y in 0..BRDF_SIZE {
            for x in 0..BRDF_SIZE {
                let [a, b] = at(x, y);
                assert!(a >= 0.0 && b >= 0.0 && a + b <= 1.0 + 1e-3, "({x}, {y}): {a} + {b}");
            }
        }
        // A smooth mirror seen head on reflects all of a white f0.
        let [a, b] = at(BRDF_SIZE - 1, 0);
        assert!((a + b - 1.0).abs() < 0.05, "smooth head-on {a} + {b}");
        // Rougher reflects less of it (masking).
        let rough = at(BRDF_SIZE - 1, BRDF_SIZE - 1);
        assert!(rough[0] + rough[1] < a + b);
    }

    #[test]
    fn f16_roundtrip() {
        for x in [0.0f32, 1.0, -2.5, 0.333_333, 65504.0, 1e-5, 6.1e-5, 3.0e-7, 1234.5] {
            let back = f16_value(f16_bits(x));
            assert!((back - x).abs() <= x.abs() * 1e-3 + 6e-8, "{x} -> {back}");
        }
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(-2.0), 0xc000);
        assert_eq!(f16_bits(65504.0), 0x7bff);
        assert_eq!(f16_bits(1e9), 0x7bff);
        assert_eq!(f16_bits(f32::NAN), 0);
        // Ties round to even.
        assert_eq!(f16_bits(f32::from_bits(0x3f80_1000)), 0x3c00);
        assert_eq!(f16_bits(f32::from_bits(0x3f80_3000)), 0x3c02);
    }

    #[test]
    fn lighting_parse_accepts_render_film_args_and_rejects_bad() {
        for good in [json!("studio"), json!("classic"), json!("overcast"), json!("night"), json!("trailer"), json!("desert_heat"), json!({ "environment": { "preset": "golden_hour", "rotate": 40 }, "lights": "none" }), json!({ "environment": "forest", "extra_lights": [{ "type": "point", "position": [0, 2, 0], "intensity": 5 }] })] {
            let l = ViewLighting::parse(&good).unwrap_or_else(|e| panic!("{good}: {e}"));
            assert!(!l.label.is_empty());
        }
        let point = ViewLighting::parse(&json!({ "environment": "forest", "lights": "none", "extra_lights": [{ "type": "point", "position": [0, 2, 0], "intensity": 5 }] })).unwrap();
        assert_eq!(point.locals.len(), 1);
        assert_eq!(point.suns.len(), 1, "forest's sun");
        assert!(ViewLighting::parse(&json!("classic")).unwrap().probe.is_none());
        for bad in [json!("unknown"), json!({ "environment": { "hdr": "relative.hdr" } }), json!({ "camera": { "ev": 3 } }), json!(4), json!({ "lights": "some" })] {
            assert!(ViewLighting::parse(&bad).is_err(), "{bad} should be refused");
        }
    }
}
