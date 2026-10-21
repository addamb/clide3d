//! How surfaces scatter light in the film renderer: a layered material
//! (a matte base, a GGX glossy layer whose color comes from metallic, an
//! optional clear coat, and sheen), sampled so each bounce picks one layer
//! in proportion to how much light it carries.
//!
//! Directions are in a local frame where the shading normal is +Z.

use std::f32::consts::{FRAC_1_PI, PI, TAU};

use glam::{Vec2, Vec3};

/// What a surface is at one point (linear color).
#[derive(Clone, Copy, Debug)]
pub struct Surface {
    pub albedo: Vec3,
    pub rough: f32,
    pub metal: f32,
    pub coat: f32,
    pub sheen: f32,
    /// Share of a broader skin reflection lobe.
    pub skin: f32,
}

/// An orthonormal frame around a normal.
pub struct Frame {
    pub t: Vec3,
    pub b: Vec3,
    pub n: Vec3,
}

impl Frame {
    pub fn new(n: Vec3) -> Frame {
        let t = n.any_orthonormal_vector();
        Frame { t, b: n.cross(t), n }
    }

    pub fn to_local(&self, v: Vec3) -> Vec3 {
        Vec3::new(v.dot(self.t), v.dot(self.b), v.dot(self.n))
    }

    pub fn to_world(&self, v: Vec3) -> Vec3 {
        self.t * v.x + self.b * v.y + self.n * v.z
    }
}

const COAT_ROUGH: f32 = 0.06;

fn schlick(f0: Vec3, cos: f32) -> Vec3 {
    f0 + (Vec3::ONE - f0) * (1.0 - cos.clamp(0.0, 1.0)).powi(5)
}

fn alpha(rough: f32) -> f32 {
    (rough.clamp(0.03, 1.0)).powi(2)
}

/// GGX normal distribution.
fn d_ggx(h: Vec3, a: f32) -> f32 {
    let a2 = a * a;
    let c = h.z * h.z * (a2 - 1.0) + 1.0;
    a2 / (PI * c * c)
}

/// Smith masking for one direction.
fn g1(v: Vec3, a: f32) -> f32 {
    let z2 = v.z * v.z;
    if z2 <= 0.0 {
        return 0.0;
    }
    let tan2 = (1.0 - z2) / z2;
    2.0 / (1.0 + (1.0 + a * a * tan2).sqrt())
}

/// Samples a visible microfacet normal (Heitz 2018).
fn sample_vndf(v: Vec3, a: f32, u: Vec2) -> Vec3 {
    let vh = Vec3::new(a * v.x, a * v.y, v.z).normalize();
    let len2 = vh.x * vh.x + vh.y * vh.y;
    let t1 = if len2 > 0.0 { Vec3::new(-vh.y, vh.x, 0.0) / len2.sqrt() } else { Vec3::X };
    let t2 = vh.cross(t1);
    let r = u.x.sqrt();
    let phi = TAU * u.y;
    let p1 = r * phi.cos();
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * (1.0 - p1 * p1).max(0.0).sqrt() + s * r * phi.sin();
    let nh = t1 * p1 + t2 * p2 + vh * (1.0 - p1 * p1 - p2 * p2).max(0.0).sqrt();
    Vec3::new(a * nh.x, a * nh.y, nh.z.max(1e-6)).normalize()
}

/// Pdf of `sample_vndf` + reflection, for direction l.
fn pdf_spec(v: Vec3, l: Vec3, a: f32) -> f32 {
    let h = (v + l).normalize_or_zero();
    if h.z <= 0.0 || v.z <= 0.0 {
        return 0.0;
    }
    g1(v, a) * d_ggx(h, a) / (4.0 * v.z)
}

fn spec(v: Vec3, l: Vec3, a: f32, f0: Vec3) -> Vec3 {
    let h = (v + l).normalize_or_zero();
    if l.z <= 0.0 || v.z <= 0.0 || h.z <= 0.0 {
        return Vec3::ZERO;
    }
    schlick(f0, v.dot(h)) * d_ggx(h, a) * g1(v, a) * g1(l, a) / (4.0 * v.z * l.z)
}

impl Surface {
    fn f0(&self) -> Vec3 {
        Vec3::splat(0.04).lerp(self.albedo, self.metal)
    }

    /// Lobe weights (diffuse, specular, coat), summing to 1.
    fn weights(&self, v: Vec3) -> [f32; 3] {
        let fs = schlick(self.f0(), v.z).max_element();
        let coat = self.coat * schlick(Vec3::splat(0.04), v.z).x;
        let diffuse = (1.0 - self.metal) * self.albedo.max_element().max(0.05) * (1.0 - fs);
        let specular = fs.max(0.04);
        let total = diffuse + specular + coat;
        [diffuse / total, specular / total, coat / total]
    }

    /// Scattering (BSDF times cos) from v to l, both local.
    pub fn eval(&self, v: Vec3, l: Vec3) -> Vec3 {
        if l.z <= 0.0 || v.z <= 0.0 {
            return Vec3::ZERO;
        }
        let a = alpha(self.rough);
        let coat_f = self.coat * schlick(Vec3::splat(0.04), v.z).x;
        let fs = schlick(self.f0(), v.z);
        let diffuse = self.albedo * (1.0 - self.metal) * FRAC_1_PI * (Vec3::ONE - fs);
        // Sheen: a soft rim of the surface color at grazing angles.
        let sheen = self.albedo * self.sheen * (1.0 - l.z).powi(4).max((1.0 - v.z).powi(4)) * 0.5 * FRAC_1_PI;
        let broad = self.skin * 0.5;
        let reflection = spec(v, l, a, self.f0()) * (1.0 - broad) + spec(v, l, alpha((self.rough + 0.25).min(1.0)), self.f0()) * broad;
        let base = (diffuse + sheen + reflection) * (1.0 - coat_f);
        let coat = spec(v, l, alpha(COAT_ROUGH), Vec3::splat(0.04)) * self.coat;
        (base + coat) * l.z
    }

    pub fn pdf(&self, v: Vec3, l: Vec3) -> f32 {
        if l.z <= 0.0 || v.z <= 0.0 {
            return 0.0;
        }
        let w = self.weights(v);
        w[0] * l.z * FRAC_1_PI + w[1] * ((1.0 - self.skin * 0.5) * pdf_spec(v, l, alpha(self.rough)) + self.skin * 0.5 * pdf_spec(v, l, alpha((self.rough + 0.25).min(1.0)))) + w[2] * pdf_spec(v, l, alpha(COAT_ROUGH))
    }

    /// A direction l for light leaving along v, chosen by one lobe.
    pub fn sample(&self, v: Vec3, u: Vec2, pick: f32) -> Option<Vec3> {
        if v.z <= 0.0 {
            return None;
        }
        let w = self.weights(v);
        let l = if pick < w[0] {
            cosine(u)
        } else {
            let a = if pick < w[0] + w[1] {
                let broad = pick < w[0] + w[1] * self.skin * 0.5;
                alpha(if broad { (self.rough + 0.25).min(1.0) } else { self.rough })
            } else { alpha(COAT_ROUGH) };
            let h = sample_vndf(v, a, u);
            (2.0 * v.dot(h) * h - v).normalize()
        };
        (l.z > 0.0).then_some(l)
    }
}

/// Cylinder scattering with shifted R, TT and TRT lobes (Marschner
/// style). Fresnel reflection is neutral; transmission absorbs pigment
/// over one or two paths through the fiber. Lobes integrate over solid angle.
///
/// The few explicit lobes miss part of the light a real fiber scatters
/// (higher internal paths, rough cuticles), and a groom is many fibers
/// deep, so whatever they miss is lost on every bounce and hair renders
/// darker than set. That missing share (measured once for a white fiber,
/// per view angle) is given back as a broad lobe in the fiber's color:
/// a white fiber keeps all the light, a colored one absorbs only by its
/// pigment, and a thick groom looks the color it was set to (see
/// `texture_check::hair_slab`).
pub fn hair(t: Vec3, color: Vec3, v: Vec3, l: Vec3) -> Vec3 {
    let pigment = fiber_albedo(color);
    let (lobes, sl) = hair_lobes(t, pigment, v, l);
    let tv = t.dot(v).clamp(-1.0, 1.0);
    let missing = (1.0 - HAIR_LOBES * white_fiber(tv)).max(0.0);
    lobes * HAIR_LOBES + pigment.powf(HAIR_PIGMENT) * missing * sl / (PI * PI)
}

/// What one fiber must scatter for a thick groom to look `color`: light
/// bounces between many fibers, so the groom is much darker than a single
/// fiber (as snow is whiter than ice). This inverts the diffusion relation
/// between single-scatter albedo and the reflectance of a deep layer.
fn fiber_albedo(color: Vec3) -> Vec3 {
    let c = color.clamp(Vec3::splat(0.0), Vec3::splat(1.0));
    // The exponent is 2 for isotropic scattering; fibers scatter forward,
    // and 1.7 matches thick slabs (see `texture_check::hair_slab`).
    (Vec3::ONE - ((Vec3::ONE - c) / (Vec3::ONE + c)).powf(1.7)).clamp(Vec3::splat(0.001), Vec3::splat(0.999))
}

/// Share of the explicit lobes, and how a fiber's color maps to the color
/// of its multiple scattering.
const HAIR_LOBES: f32 = 0.6;
const HAIR_PIGMENT: f32 = 1.0;

/// R, TT and TRT: (longitudinal shift, width, azimuth: +1 = back toward
/// the viewer's side, -1 = through the fiber).
const HAIR_PARTS: [(f32, f32, f32); 3] = [(-0.07, 0.16, 1.0), (0.035, 0.22, -1.0), (0.14, 0.30, 1.0)];

/// One fiber seen from `v`: angles, Fresnel, and each lobe's color.
struct FiberView {
    tv: f32,
    colors: [Vec3; 3],
}

impl FiberView {
    fn new(t: Vec3, pigment: Vec3, v: Vec3) -> FiberView {
        let tv = t.dot(v).clamp(-1.0, 1.0);
        let sv = (1.0 - tv * tv).max(0.0).sqrt();
        let f = 0.0465 + (1.0 - 0.0465) * (1.0 - sv).powi(5);
        // Artist reflectance mapped to diameter absorption: blond
        // transmits, while dark fibers retain a neutral surface highlight.
        let absorption = Vec3::new(-pigment.x.ln(), -pigment.y.ln(), -pigment.z.ln()) * 0.4;
        let path = 1.0 / (1.0 - (tv / 1.55).powi(2)).max(0.1).sqrt();
        let atten = -absorption * path;
        let transmission = Vec3::new(atten.x.exp(), atten.y.exp(), atten.z.exp());
        let colors = [Vec3::splat(f), transmission * (1.0 - f).powi(2), transmission * transmission * (1.0 - f).powi(2) * f];
        FiberView { tv, colors }
    }

    /// Mean of lobe `k`'s longitudinal Gaussian (in cosine to the fiber).
    fn mean(&self, k: usize) -> f32 {
        (-self.tv + HAIR_PARTS[k].0).clamp(-0.95, 0.95)
    }

    /// Share of lobe `k`'s Gaussian that falls on real directions.
    fn mass(&self, k: usize) -> f32 {
        let (m, s) = (self.mean(k), HAIR_PARTS[k].1 * std::f32::consts::SQRT_2);
        0.5 * (erf((1.0 - m) / s) - erf((-1.0 - m) / s))
    }
}

/// R + TT + TRT (unscaled), and the sine of the light's angle to the fiber.
fn hair_lobes(t: Vec3, pigment: Vec3, v: Vec3, l: Vec3) -> (Vec3, f32) {
    let fv = FiberView::new(t, pigment, v);
    let tl = t.dot(l).clamp(-1.0, 1.0);
    let (sl, sv) = ((1.0 - tl * tl).max(0.0).sqrt(), (1.0 - fv.tv * fv.tv).max(0.0).sqrt());
    let phi = ((l - t * tl).dot(v - t * fv.tv) / (sl * sv).max(1e-5)).clamp(-1.0, 1.0);
    let mut sum = Vec3::ZERO;
    for (k, &(_, sigma, side)) in HAIR_PARTS.iter().enumerate() {
        let x = (tl - fv.mean(k)) / sigma;
        let longitudinal = (-0.5 * x * x).exp() / (sigma * (2.0 * PI).sqrt());
        sum += fv.colors[k] * longitudinal * (1.0 + side * phi) / TAU;
    }
    (sum, sl)
}

/// How likely each part is picked when sampling: R, TT, TRT, multiple.
fn hair_weights(fv: &FiberView, pigment: Vec3) -> [f32; 4] {
    let missing = (1.0 - HAIR_LOBES * white_fiber(fv.tv)).max(0.0);
    let mut w = [0.0; 4];
    for (k, item) in w.iter_mut().take(3).enumerate() {
        *item = HAIR_LOBES * fv.colors[k].max_element() * fv.mass(k);
    }
    w[3] = pigment.powf(HAIR_PIGMENT).max_element() * missing;
    let total: f32 = w.iter().sum::<f32>().max(1e-8);
    w.map(|x| x / total)
}

/// Density (per solid angle) of `hair_sample` picking `l`.
pub fn hair_pdf(t: Vec3, color: Vec3, v: Vec3, l: Vec3) -> f32 {
    let pigment = fiber_albedo(color);
    let fv = FiberView::new(t, pigment, v);
    let w = hair_weights(&fv, pigment);
    let tl = t.dot(l).clamp(-1.0, 1.0);
    let (sl, sv) = ((1.0 - tl * tl).max(0.0).sqrt(), (1.0 - fv.tv * fv.tv).max(0.0).sqrt());
    let phi = ((l - t * tl).dot(v - t * fv.tv) / (sl * sv).max(1e-5)).clamp(-1.0, 1.0);
    let mut pdf = w[3] * sl / (PI * PI);
    for (k, &(_, sigma, side)) in HAIR_PARTS.iter().enumerate() {
        let x = (tl - fv.mean(k)) / sigma;
        let gauss = (-0.5 * x * x).exp() / (sigma * (2.0 * PI).sqrt());
        pdf += w[k] * gauss / fv.mass(k).max(1e-6) * (1.0 + side * phi) / TAU;
    }
    pdf
}

/// A direction for light scattered by the fiber toward `v`, picked in
/// proportion to the lobes (so paths through thick hair keep even
/// weights), with its density. `rand` gives uniform numbers.
pub fn hair_sample(t: Vec3, color: Vec3, v: Vec3, rand: &mut impl FnMut() -> f32) -> Option<(Vec3, f32)> {
    let pigment = fiber_albedo(color);
    let fv = FiberView::new(t, pigment, v);
    let w = hair_weights(&fv, pigment);
    let a = (v - t * fv.tv).normalize_or(t.any_orthonormal_vector());
    let b = t.cross(a);
    let pick = rand();
    let k = if pick < w[0] { 0 } else if pick < w[0] + w[1] { 1 } else if pick < w[0] + w[1] + w[2] { 2 } else { 3 };
    let (tl, angle) = if k == 3 {
        // Density in proportion to the sine to the fiber: rejection.
        let mut tl = 0.0;
        for _ in 0..32 {
            tl = 1.0 - 2.0 * rand();
            if rand() <= (1.0 - tl * tl).sqrt() { break; }
        }
        (tl, TAU * rand())
    } else {
        let (_, sigma, side) = HAIR_PARTS[k];
        let mut tl = fv.mean(k);
        for _ in 0..16 {
            let (u1, u2) = (rand().max(1e-7), rand());
            let g = (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos();
            let x = fv.mean(k) + g * sigma;
            if (-1.0..=1.0).contains(&x) { tl = x; break; }
        }
        // Azimuth density (1 + side cos) / 2 pi: rejection.
        let mut angle = 0.0;
        for _ in 0..32 {
            angle = TAU * rand() - PI;
            if rand() * 2.0 <= 1.0 + side * angle.cos() { break; }
        }
        (tl, angle)
    };
    let sl = (1.0 - tl * tl).max(0.0).sqrt();
    let l = (t * tl + (a * angle.cos() + b * angle.sin()) * sl).normalize_or(t);
    let pdf = hair_pdf(t, color, v, l);
    (pdf > 1e-8).then_some((l, pdf))
}

/// Error function (Abramowitz and Stegun 7.1.26, |error| < 1.5e-7).
fn erf(x: f32) -> f32 {
    let s = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0 - (((((1.061_405_4 * t - 1.453_152) * t) + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6) * t * (-x * x).exp();
    s * y
}

/// How much light the lobes of a white fiber scatter in all, for a view at
/// `tv` (cosine to the fiber), from a table made on first use.
fn white_fiber(tv: f32) -> f32 {
    let table = white_fiber_table();
    let x = (tv.clamp(-1.0, 1.0) + 1.0) * 0.5 * WHITE_STEPS as f32;
    let i = (x.floor() as usize).min(WHITE_STEPS - 1);
    table[i] + (table[i + 1] - table[i]) * (x - i as f32)
}

pub(super) const WHITE_STEPS: usize = 64;

/// `white_fiber` at WHITE_STEPS + 1 even steps of tv from -1 to 1.
pub(super) fn white_fiber_table() -> &'static [f32] {
    const STEPS: usize = WHITE_STEPS;
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=STEPS).map(|i| {
            let tv = -1.0 + 2.0 * i as f32 / STEPS as f32;
            let v = Vec3::new(0.0, tv, (1.0 - tv * tv).max(0.0).sqrt());
            let n = 4096;
            (0..n).map(|k| {
                let l = sphere(Vec2::new((k as f32 + 0.5) / n as f32, (k as f32 * 0.618034).fract()));
                hair_lobes(Vec3::Y, Vec3::splat(0.999), v, l).0.x
            }).sum::<f32>() * 4.0 * PI / n as f32
        }).collect()
    })
}

/// Uniform direction over the whole sphere.
pub fn sphere(u: Vec2) -> Vec3 {
    let z = 1.0 - 2.0 * u.x;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = TAU * u.y;
    Vec3::new(r * phi.cos(), r * phi.sin(), z)
}

/// Cosine-weighted direction around +Z.
pub fn cosine(u: Vec2) -> Vec3 {
    let r = u.x.sqrt();
    let phi = TAU * u.y;
    Vec3::new(r * phi.cos(), r * phi.sin(), (1.0 - u.x).max(0.0).sqrt())
}

/// A direction within `angle` (radians) of `axis`, uniform over the cone.
pub fn cone(axis: Vec3, angle: f32, u: Vec2) -> Vec3 {
    let cos_max = angle.cos();
    let z = 1.0 - u.x * (1.0 - cos_max);
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = TAU * u.y;
    Frame::new(axis).to_world(Vec3::new(r * phi.cos(), r * phi.sin(), z))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rough check that sampling matches eval / pdf: a white matte surface
    /// under uniform light reflects close to everything.
    #[test]
    fn white_surface_conserves_energy() {
        for skin in [0.0, 0.8, 1.0] {
            let s = Surface { albedo: Vec3::ONE, rough: 0.5, metal: 0.0, coat: 0.0, sheen: 0.0, skin };
            let v = Vec3::new(0.3, 0.0, 0.95).normalize();
            let mut total = 0.0;
            let n = 64;
            let mut count = 0;
            for i in 0..n {
                for j in 0..n {
                    let u = Vec2::new((i as f32 + 0.5) / n as f32, (j as f32 + 0.5) / n as f32);
                    let pick = ((i * 7 + j * 13) % n) as f32 / n as f32;
                    if let Some(l) = s.sample(v, u, pick) {
                        let p = s.pdf(v, l);
                        if p > 0.0 {
                            total += s.eval(v, l).x / p;
                        }
                    }
                    count += 1;
                }
            }
            let albedo = total / count as f32;
            assert!((0.8..1.05).contains(&albedo), "{albedo}");
        }
    }
}

#[cfg(test)]
mod hair_tests {
    use super::*;

    #[test]
    fn scattering_is_finite_and_energy_bounded_for_blond_and_dark_fibers() {
        for color in [Vec3::splat(0.005), Vec3::new(0.65, 0.5, 0.3), Vec3::splat(0.999)] {
            for sin in [-1.0f32, -0.9, -0.85, -0.6, 0.0, 0.6, 0.85, 0.9, 1.0] {
                let v = Vec3::new(0.0, sin, (1.0 - sin * sin).sqrt());
                let mut integral = Vec3::ZERO;
                for i in 0..16000 {
                    let l = sphere(Vec2::new((i as f32 + 0.5) / 16000.0, (i as f32 * 0.618034).fract()));
                    let f = hair(Vec3::Y, color, v, l);
                    assert!(f.is_finite() && f.min_element() >= 0.0);
                    integral += f * (4.0 * PI / 16000.0);
                }
                assert!(integral.max_element() <= 1.02, "energy {integral} for {color}, {v}");
            }
        }
        // A white fiber keeps all the light (the missing share comes back).
        for sin in [-0.9f32, -0.3, 0.0, 0.5, 0.95] {
            let v = Vec3::new(0.0, sin, (1.0 - sin * sin).sqrt());
            let total: f32 = (0..16000).map(|i| hair(Vec3::Y, Vec3::ONE, v, sphere(Vec2::new((i as f32 + 0.5) / 16000.0, (i as f32 * 0.618034).fract()))).x).sum::<f32>() * 4.0 * PI / 16000.0;
            assert!((0.97..1.02).contains(&total), "white fiber keeps {total} at {sin}");
        }
        let dark = hair(Vec3::Y, Vec3::splat(0.005), Vec3::Z, Vec3::NEG_Z);
        let blond = hair(Vec3::Y, Vec3::splat(0.7), Vec3::Z, Vec3::NEG_Z);
        assert!(blond.min_element() > dark.max_element() * 3.0);
    }

    /// Sampling picks directions with the density `hair_pdf` reports: the
    /// importance-sampled integral matches a brute-force one, and the
    /// density integrates to one.
    #[test]
    fn hair_sampling_matches_its_density() {
        let t = Vec3::new(0.2, 1.0, 0.1).normalize();
        for color in [Vec3::new(0.65, 0.5, 0.3), Vec3::splat(0.05)] {
            for v in [Vec3::Z, Vec3::new(0.3, -0.6, 0.7).normalize()] {
                let n = 40000;
                let sphere_points = (0..n).map(|i| sphere(Vec2::new((i as f32 + 0.5) / n as f32, (i as f32 * 0.618034).fract())));
                let (mut brute, mut density) = (Vec3::ZERO, 0.0);
                for l in sphere_points {
                    brute += hair(t, color, v, l) * (4.0 * PI / n as f32);
                    density += hair_pdf(t, color, v, l) * (4.0 * PI / n as f32);
                }
                let mut state = 12345u32;
                let mut rand = || { state ^= state << 13; state ^= state >> 17; state ^= state << 5; (state >> 8) as f32 / (1u32 << 24) as f32 };
                let mut sampled = Vec3::ZERO;
                for _ in 0..n {
                    if let Some((l, pdf)) = hair_sample(t, color, v, &mut rand) {
                        sampled += hair(t, color, v, l) / pdf / n as f32;
                    }
                }
                assert!((density - 1.0).abs() < 0.03, "density {density}");
                assert!((sampled - brute).abs().max_element() < 0.03 * brute.max_element().max(0.1), "{color} {v}: sampled {sampled} brute {brute}");
            }
        }
    }
}
