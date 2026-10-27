//! Eyeballs: a ready-made eye object (`Shape::Eye`).
//!
//! Painting an eye onto clay gives a smudge, because paint is stored per
//! vertex and a face on a full-body model has too few. An eye object has
//! its own mesh and texture instead: a ball of diameter 1 looking down
//! local +Z, built like a real eye.
//!
//! - The sclera and the iris carry one generated texture (color, a normal
//!   map for relief, and roughness and occlusion): iris fibers fanning from
//!   the pupil, crypts, a lighter collarette, contraction furrows, a dark
//!   limbal ring; a white with fine red veins toward the corners.
//! - The iris is a slightly cone-shaped disc set back behind the limbus,
//!   under a clear, refracting cornea (glass of index 1.376) that bulges
//!   over it, so the iris sits in depth and catch-lights are sharp.
//! - Eyes made by `create_human` know where their lids touch them (`lids`,
//!   in the eye's parent space). Then the white darkens where the lids
//!   meet it, a glossy tear meniscus runs along the lower lid, and a pink
//!   caruncle fills the inner corner. These follow the lids when the eye
//!   turns.
//!
//! Size it with the object's scale and aim it by rotating the object.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::{Arc, Mutex, OnceLock};

use glam::{Mat4, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::material::{Material, noise};
use crate::mesh::{Mesh, Textures, Vertex};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Eye {
    /// sRGB 0..1.
    pub iris_color: [f32; 3],
    /// Iris width as a part of the eye's width, seen from the front.
    pub iris_size: f32,
    /// Pupil width as a part of the iris width.
    pub pupil_size: f32,
    pub sclera_color: [f32; 3],
    pub pupil_color: [f32; 3],
    /// How far the cornea bulges out over the iris, as a part of the eye's width.
    pub cornea: f32,
    /// Red veins in the white, 0..1.
    pub veins: f32,
    /// Darkness of the ring around the iris, 0..1.
    pub limbal_ring: f32,
    /// Where the lids touch the eye (see `Lids`); set by `create_human`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lids: Option<Lids>,
    /// Realistic (fibers, crypts, veins) or stylised (Stage 4d: a clean
    /// radial iris with a crisp limbal ring and a few soft fibers).
    #[serde(skip_serializing_if = "EyeStyle::is_realistic")]
    pub style: EyeStyle,
    /// Scales `iris_size` (1..1.6): the big irises of stylised eyes. The
    /// iris stays within `MAX_IRIS` of the eye's width.
    #[serde(skip_serializing_if = "is_one")]
    pub iris_scale: f32,
    /// A small glowing spot on the iris, up and to the right in the eye's
    /// own frame, seen through the cornea: the catch-light of animated
    /// eyes, there whatever the lighting.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub catchlight: bool,
}

/// How an eye is drawn (Stage 4d).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum EyeStyle {
    #[default]
    Realistic,
    Stylized,
}

impl EyeStyle {
    fn is_realistic(&self) -> bool {
        *self == EyeStyle::Realistic
    }
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// Widest iris, as a part of the eye's width.
pub const MAX_IRIS: f32 = 0.9;
/// Bounds of `Eye::iris_scale`.
pub const IRIS_SCALE: (f32, f32) = (1.0, 1.6);
/// The catch-light's glow (`Material::emission`).
const CATCHLIGHT_GLOW: f32 = 4.0;

/// What `style: stylized` fills in when a call does not say otherwise
/// (stored, so the eye reads back as it is drawn): iris_scale 1.3, a
/// flatter cornea (0.03 against 0.05), no veins, a crisp limbal ring
/// (0.8), a catch-light.
pub const STYLIZED: [(&str, f64); 4] = [("iris_scale", 1.3), ("cornea", 0.03), ("veins", 0.0), ("limbal_ring", 0.8)];

/// The lid margins (the lid edges around the eye opening), in the eye's
/// parent space (a human head's space), each from the inner corner to the
/// outer one. The lids meet the ball along the same directions from its
/// center.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Lids {
    pub upper: Vec<[f32; 3]>,
    pub lower: Vec<[f32; 3]>,
}

impl Default for Eye {
    fn default() -> Self {
        Eye {
            iris_color: [0.3, 0.5, 0.33],
            iris_size: 0.5,
            pupil_size: 0.34,
            sclera_color: [0.9, 0.86, 0.82],
            pupil_color: [0.02, 0.02, 0.025],
            cornea: 0.05,
            veins: 0.4,
            limbal_ring: 0.6,
            lids: None,
            style: EyeStyle::Realistic,
            iris_scale: 1.0,
            catchlight: false,
        }
    }
}

const SEGMENTS: usize = 96;
/// Most lid points kept per margin.
const MAX_LID_POINTS: usize = 48;
/// Texture size of the eye.
const TEXTURE: u32 = 1024;
/// The cornea's index of refraction.
pub const CORNEA_IOR: f32 = 1.376;
/// How far the iris sits behind the limbus plane, in eye widths.
const IRIS_DEPTH: f32 = 0.028;

/// Material slots of an eye mesh.
const SCLERA: u32 = 0;
const IRIS: u32 = 1;
const CORNEA: u32 = 2;
const TEAR: u32 = 3;
const CARUNCLE: u32 = 4;
/// Present only on eyes with a catch-light.
const CATCHLIGHT: u32 = 5;

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Texture coordinates of the direction `t` from +Z, turned `p` around it:
/// an azimuthal map with the front (iris) given most of the texture.
fn uv_of(t: f32, p: f32) -> Vec2 {
    let g = (t / PI).clamp(0.0, 1.0).sqrt();
    Vec2::new(0.5 + 0.5 * g * p.cos(), 0.5 + 0.5 * g * p.sin())
}

/// The direction (t, p) a texel shows; t > PI is outside the map.
fn polar_of(uv: Vec2) -> (f32, f32) {
    let d = (uv - Vec2::splat(0.5)) * 2.0;
    (PI * d.length_squared(), d.y.atan2(d.x))
}

impl Eye {
    /// Clamps values into ranges that build a sane eye.
    pub fn sanitize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Eye::default();
        self.iris_size = fix(self.iris_size, 0.05, MAX_IRIS, d.iris_size);
        self.iris_scale = fix(self.iris_scale, IRIS_SCALE.0, IRIS_SCALE.1, 1.0);
        self.pupil_size = fix(self.pupil_size, 0.05, 0.95, d.pupil_size);
        self.cornea = fix(self.cornea, 0.0, 0.2, d.cornea);
        self.veins = fix(self.veins, 0.0, 1.0, d.veins);
        self.limbal_ring = fix(self.limbal_ring, 0.0, 1.0, d.limbal_ring);
        for c in [&mut self.iris_color, &mut self.sclera_color, &mut self.pupil_color] {
            *c = c.map(|v| fix(v, 0.0, 1.0, 0.5));
        }
        if let Some(l) = &mut self.lids {
            for m in [&mut l.upper, &mut l.lower] {
                m.retain(|p| p.iter().all(|v| v.is_finite()));
                m.truncate(MAX_LID_POINTS);
            }
            if l.upper.len() < 2 || l.lower.len() < 2 {
                self.lids = None;
            }
        }
    }

    /// The iris width drawn (a part of the eye's width): `iris_size`
    /// times `iris_scale`, at most `MAX_IRIS`.
    pub fn iris_width(&self) -> f32 {
        (self.iris_size * self.iris_scale).min(MAX_IRIS)
    }

    /// Applies `style: stylized`'s values (`STYLIZED`) except those
    /// `given` names, and a catch-light unless `given` has catchlight.
    pub fn stylize(&mut self, given: &dyn Fn(&str) -> bool) {
        self.style = EyeStyle::Stylized;
        for (k, v) in STYLIZED {
            if given(k) {
                continue;
            }
            let v = v as f32;
            match k {
                "iris_scale" => self.iris_scale = v,
                "cornea" => self.cornea = v,
                "veins" => self.veins = v,
                "limbal_ring" => self.limbal_ring = v,
                _ => unreachable!("STYLIZED keys"),
            }
        }
        if !given("catchlight") {
            self.catchlight = true;
        }
    }

    /// Angles from +Z of the pupil edge, iris edge, and cornea edge.
    fn angles(&self) -> (f32, f32, f32) {
        let mut e = self.clone();
        e.sanitize();
        let size = e.iris_width();
        let iris = size.asin();
        let pupil = (size * e.pupil_size).asin();
        (pupil, iris, (iris * 1.04).min(FRAC_PI_2))
    }

    /// The eye's materials, by slot: sclera, iris, cornea, tear, caruncle.
    pub fn materials() -> Vec<Material> {
        vec![
            Material { name: "sclera".into(), roughness: 0.12, metallic: 0.0, clearcoat: 0.5, sheen: 0.0, ..Material::default() },
            Material { name: "iris".into(), roughness: 0.5, metallic: 0.0, clearcoat: 0.0, sheen: 0.0, ..Material::default() },
            Material { name: "cornea".into(), roughness: 0.0, transmission: 1.0, ior: CORNEA_IOR, ..Material::default() },
            Material { name: "tear".into(), roughness: 0.04, clearcoat: 1.0, subsurface: 0.3, scatter: [0.0012, 0.0006, 0.0003], ..Material::default() },
            Material { name: "caruncle".into(), roughness: 0.28, clearcoat: 0.6, subsurface: 0.5, scatter: [0.0012, 0.0006, 0.0003], skin_specular: 0.5, ..Material::default() },
        ]
    }

    /// Point and normal of the sclera / iris surface at (t, p).
    fn surface_at(&self, t: f32, p: f32) -> (Vec3, Vec3) {
        let (_, iris, _) = self.angles();
        let around = Vec3::new(p.cos(), p.sin(), 0.0);
        if t >= iris {
            let d = Vec3::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos());
            return (d * 0.5, d);
        }
        // The iris: a shallow bowl under the cornea, meeting the white at
        // the limbus without a step, its pupil margin pushed a little
        // forward by the lens.
        let rl = 0.5 * iris.sin();
        let zl = 0.5 * iris.cos();
        let s = t / iris;
        let rho = rl * s;
        let z = zl - IRIS_DEPTH * (0.9 * (1.0 - s * s) - 0.25 * (1.0 - s) * (1.0 - s));
        let slope = -IRIS_DEPTH * (-1.8 * s + 0.5 * (1.0 - s)) / rl;
        let n = (Vec3::Z - around * slope).normalize();
        (around * rho + Vec3::Z * z, n)
    }

    /// How far the cornea's crown reaches past the ball (radius 0.5), in
    /// eye widths: nothing of the eye is farther from its centre than
    /// 0.5 plus this. Lids clear it (`lids.rs`).
    pub fn cornea_bulge(&self) -> f32 {
        let c = if self.cornea.is_finite() { self.cornea.clamp(0.0, 0.2) } else { Eye::default().cornea };
        c + 0.012
    }

    /// Height of the cornea dome over the limbus plane at radius `rho`.
    fn cornea_at(&self, rho: f32) -> (Vec3, f32) {
        let (_, _, edge) = self.angles();
        let rc = 0.5 * edge.sin();
        let zc = 0.5 * edge.cos() + 0.0006;
        // (Summed in this order: the realistic eye is pinned bit for bit.)
        let apex = 0.5 + self.cornea.clamp(0.0, 0.2) + 0.012;
        // A steep rim: no thin glass wedge where the cornea meets the white
        // (light bounces around inside one and darkens the limbus).
        let s = (rho / rc).clamp(0.0, 0.999);
        let k = (1.0 - s * s).sqrt();
        let z = zc + (apex - zc) * k;
        let slope = -(apex - zc) * s / (k * rc);
        (Vec3::new(0.0, 0.0, z), slope)
    }

    #[cfg(test)]
    pub fn build_mesh(&self) -> Mesh {
        self.build_mesh_in(None)
    }

    /// The mesh; `to_mesh` maps the eye's parent space into mesh space (the
    /// inverse of the object's local matrix), so the lids can be placed.
    pub fn build_mesh_in(&self, to_mesh: Option<Mat4>) -> Mesh {
        let mut eye = self.clone();
        eye.sanitize();
        let (pupil, iris, edge) = eye.angles();
        let materials = Eye::materials();
        let mut mesh = Mesh { materials, ..Mesh::default() };
        let mut push = |mesh: &mut Mesh, pos: Vec3, n: Vec3, color: Vec3, m: u32, uv: Vec2| {
            let mat = &mesh.materials[m as usize];
            mesh.vertices.push(Vertex {
                pos: pos.to_array(),
                normal: n.normalize_or(Vec3::Z).to_array(),
                color: color.to_array(),
                surface: mat.surface(),
                material: m,
                uv: uv.to_array(),
                tangent: [0.0; 4],
                light: mat.light(),
                base: [color.x, color.y, color.z, -1.0],
                wear: [0.0; 3],
                painted: [0; 4],
            });
        };
        // Ball: iris rings, then the sclera out to the back.
        let mut rings: Vec<f32> = (0..=20).map(|i| iris * i as f32 / 20.0).collect();
        rings.extend([pupil * 0.98, pupil * 1.02]);
        rings.extend((1..=36).map(|i| iris + (PI - iris) * (i as f32 / 36.0).powf(1.3)));
        rings.push(iris + 0.003);
        rings.sort_by(f32::total_cmp);
        rings.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        for &t in &rings {
            let m = if t <= iris { IRIS } else { SCLERA };
            for s in 0..SEGMENTS {
                let p = TAU * s as f32 / SEGMENTS as f32;
                let (pos, n) = eye.surface_at(t, p);
                push(&mut mesh, pos, n, Vec3::ONE, m, uv_of(t, p));
            }
        }
        let ring_count = rings.len();
        let at = |ring: usize, s: usize| (ring * SEGMENTS + s % SEGMENTS) as u32;
        for ring in 0..ring_count - 1 {
            for s in 0..SEGMENTS {
                let (a, b, c, d) = (at(ring, s), at(ring, s + 1), at(ring + 1, s + 1), at(ring + 1, s));
                if ring > 0 {
                    mesh.indices.extend_from_slice(&[a, c, b]);
                }
                if ring + 2 < ring_count {
                    mesh.indices.extend_from_slice(&[a, d, c]);
                }
                if s % 8 == 0 {
                    mesh.edges.extend_from_slice(&[a, d]);
                }
            }
        }
        for (ring, &t) in rings.iter().enumerate() {
            if [iris, FRAC_PI_2].iter().any(|e| (t - e).abs() < 2e-3) {
                for s in 0..SEGMENTS {
                    mesh.edges.extend_from_slice(&[at(ring, s), at(ring, s + 1)]);
                }
            }
        }
        crate::bake::tangents(&mut mesh);
        // Before the cornea, so the viewport draws it under the dome.
        if eye.catchlight {
            catchlight(&mut mesh, &eye, &mut push);
        }

        // Cornea: a clear dome from the limbus over the iris.
        let base = mesh.vertices.len() as u32;
        let rc = 0.5 * edge.sin();
        let cr = 16;
        for i in 0..=cr {
            let rho = rc * i as f32 / cr as f32;
            let (c, slope) = eye.cornea_at(rho);
            for s in 0..SEGMENTS {
                let p = TAU * s as f32 / SEGMENTS as f32;
                let around = Vec3::new(p.cos(), p.sin(), 0.0);
                push(&mut mesh, around * rho + c, Vec3::Z - around * slope, Vec3::ONE, CORNEA, Vec2::ZERO);
            }
        }
        let cat = |ring: usize, s: usize| base + (ring * SEGMENTS + s % SEGMENTS) as u32;
        for ring in 0..cr {
            for s in 0..SEGMENTS {
                let (a, b, c, d) = (cat(ring, s), cat(ring, s + 1), cat(ring + 1, s + 1), cat(ring + 1, s));
                if ring > 0 {
                    mesh.indices.extend_from_slice(&[a, c, b]);
                }
                mesh.indices.extend_from_slice(&[a, d, c]);
            }
        }

        let lids = to_mesh.and_then(|m| eye.lids.as_ref().map(|l| LidsIn::new(l, m)));
        let tex = texture(&eye, lids.as_ref());
        mesh.textures = Some(tex);
        if let Some(l) = &lids {
            tear_line(&mut mesh, &l.lower, &mut push);
            caruncle(&mut mesh, l, &mut push);
        }
        mesh
    }
}

/// The catch-light: a small glowing disc lying on the iris (a hair above
/// it, so it neither floats under the cornea nor shadows the iris), up
/// and to the right of the pupil in the eye's frame, seen through the
/// cornea's refraction. Its own material slot (`CATCHLIGHT`) glows white;
/// its area is so small that the light it gives the scene is lost in the
/// render's noise (measured in `film::style_checks`).
fn catchlight(mesh: &mut Mesh, eye: &Eye, push: &mut Push) {
    mesh.materials.push(Material { name: "catchlight".into(), roughness: 1.0, metallic: 0.0, clearcoat: 0.0, sheen: 0.0, emission: CATCHLIGHT_GLOW, ..Material::default() });
    let (pupil, iris, _) = eye.angles();
    // Between the pupil's edge and the iris's, 45 degrees up.
    let t = pupil + (iris - pupil) * 0.35;
    let p = std::f32::consts::FRAC_PI_4;
    let (center, n) = eye.surface_at(t, p);
    let radius = 0.5 * iris.sin() * 0.16;
    let center = center + n * 0.0015;
    let side = n.cross(Vec3::Y).normalize_or(Vec3::X);
    let up = side.cross(n);
    let base = mesh.vertices.len() as u32;
    let segments = 24;
    push(mesh, center, n, Vec3::ONE, CATCHLIGHT, Vec2::ZERO);
    for k in 0..segments {
        let a = TAU * k as f32 / segments as f32;
        // A soft oval, a little wider than tall.
        push(mesh, center + (side * a.cos() * 1.2 + up * a.sin()) * radius, n, Vec3::ONE, CATCHLIGHT, Vec2::ZERO);
    }
    // Counter-clockwise seen from outside (the viewport culls back faces).
    for k in 0..segments as u32 {
        mesh.indices.extend_from_slice(&[base, base + 1 + (k + 1) % segments as u32, base + 1 + k]);
    }
}

/// Lids in mesh space, with the angle from +Z where each direction around
/// the eye meets them.
struct LidsIn {
    upper: Vec<Vec3>,
    lower: Vec<Vec3>,
    /// Contact angle t per azimuth bin.
    contact: Vec<f32>,
}

const BINS: usize = 180;

impl LidsIn {
    fn new(l: &Lids, to_mesh: Mat4) -> LidsIn {
        let map = |v: &Vec<[f32; 3]>| v.iter().map(|p| to_mesh.transform_point3(Vec3::from(*p))).collect::<Vec<_>>();
        let (upper, lower) = (map(&l.upper), map(&l.lower));
        // Contact angle around the eye: nearest lid point by azimuth.
        let polar = |q: &Vec3| {
            let d = q.normalize_or(Vec3::Z);
            (d.z.clamp(-1.0, 1.0).acos(), d.y.atan2(d.x))
        };
        let pts: Vec<(f32, f32)> = upper.iter().chain(lower.iter()).map(polar).collect();
        let contact = (0..BINS)
            .map(|b| {
                let p = TAU * b as f32 / BINS as f32 - PI;
                let mut best = (f32::MAX, FRAC_PI_2);
                for &(t, q) in &pts {
                    let mut d = (q - p).abs();
                    if d > PI {
                        d = TAU - d;
                    }
                    if d < best.0 {
                        best = (d, t);
                    }
                }
                best.1
            })
            .collect();
        LidsIn { upper, lower, contact }
    }

    fn contact_at(&self, p: f32) -> f32 {
        let x = (p + PI) / TAU * BINS as f32;
        let i = x.floor() as i64;
        let f = x - i as f32;
        let at = |k: i64| self.contact[k.rem_euclid(BINS as i64) as usize];
        at(i) * (1.0 - f) + at(i + 1) * f
    }
}

type Push<'a> = dyn FnMut(&mut Mesh, Vec3, Vec3, Vec3, u32, Vec2) + 'a;

/// A thin glossy tube along the lower lid margin: the tear meniscus (an
/// opaque wet strip; a clear one only adds noise at this size).
fn tear_line(mesh: &mut Mesh, lower: &[Vec3], push: &mut Push) {
    if lower.len() < 2 {
        return;
    }
    let base = mesh.vertices.len() as u32;
    let around = 8;
    let n = lower.len();
    for (i, q) in lower.iter().enumerate() {
        let tangent = (lower[(i + 1).min(n - 1)] - lower[i.saturating_sub(1)]).normalize_or(Vec3::X);
        let out = q.normalize_or(Vec3::Z);
        let side = tangent.cross(out).normalize_or(Vec3::Y);
        let t = i as f32 / (n - 1) as f32;
        let r = 0.0075 * (smoothstep(0.0, 0.12, t) * (1.0 - smoothstep(0.85, 1.0, t))).max(0.15);
        // In the corner between the lid edge and the ball.
        let center = out * (q.length().max(0.5) - r * 0.3);
        for k in 0..around {
            let a = TAU * k as f32 / around as f32;
            let dir = out * a.cos() + side * a.sin();
            push(mesh, center + dir * r, dir, Vec3::new(0.9, 0.72, 0.68), TEAR, Vec2::ZERO);
        }
    }
    for i in 0..n as u32 - 1 {
        for k in 0..around as u32 {
            let a = base + i * around as u32 + k;
            let b = base + i * around as u32 + (k + 1) % around as u32;
            let (c, d) = (b + around as u32, a + around as u32);
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
}

/// The pink caruncle in the inner corner, where the lids meet.
fn caruncle(mesh: &mut Mesh, lids: &LidsIn, push: &mut Push) {
    // Just inside the corner, where the margins have opened a little.
    let (Some(&a), Some(&b)) = (lids.upper.get(1), lids.lower.get(1)) else { return };
    let corner = (a + b) * 0.5;
    let next = (lids.upper.get(2).copied().unwrap_or(a) + lids.lower.get(2).copied().unwrap_or(b)) * 0.5;
    let inward = (corner - next).normalize_or(Vec3::X);
    let out = corner.normalize_or(Vec3::Z);
    let up = out.cross(inward).normalize_or(Vec3::Y);
    let center = corner.normalize_or(Vec3::Z) * 0.49 + inward * 0.01;
    let radii = Vec3::new(0.035, 0.045, 0.02);
    let color = Vec3::new(0.82, 0.56, 0.54);
    let base = mesh.vertices.len() as u32;
    let (rings, segs) = (10, 16);
    for i in 0..=rings {
        let th = PI * i as f32 / rings as f32;
        for k in 0..segs {
            let ph = TAU * k as f32 / segs as f32;
            let unit = Vec3::new(th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos());
            let p = center + inward * (unit.x * radii.x) + up * (unit.y * radii.y) + out * (unit.z * radii.z);
            let n = inward * (unit.x / radii.x) + up * (unit.y / radii.y) + out * (unit.z / radii.z);
            // Bumpy, a little lighter on top.
            let shade = 0.92 + 0.12 * noise(p * 60.0);
            push(mesh, p, n, color * shade, CARUNCLE, Vec2::ZERO);
        }
    }
    for i in 0..rings as u32 {
        for k in 0..segs as u32 {
            let a = base + i * segs as u32 + k;
            let b = base + i * segs as u32 + (k + 1) % segs as u32;
            let (c, d) = (b + segs as u32, a + segs as u32);
            mesh.indices.extend_from_slice(&[a, c, b, a, d, c]);
        }
    }
}

/// What the texture depends on (the eye without its lids).
fn texture_key(e: &Eye) -> Eye {
    Eye { lids: None, ..e.clone() }
}

/// The eye's texture: shared between eyes with the same look, then the
/// lids darken a copy.
fn texture(eye: &Eye, lids: Option<&LidsIn>) -> Arc<Textures> {
    type Cache = Mutex<Vec<(Eye, Arc<Textures>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = texture_key(eye);
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let found = cache.lock().expect("cache").iter().find(|(k, _)| *k == key).map(|(_, t)| t.clone());
    let plain = found.unwrap_or_else(|| {
        let t = Arc::new(paint_texture(&key));
        let mut c = cache.lock().expect("cache");
        c.push((key, t.clone()));
        if c.len() > 8 {
            c.remove(0);
        }
        t
    });
    let Some(l) = lids else { return plain };
    let mut t = Textures { size: plain.size, color: plain.color.clone(), normal: plain.normal.clone(), orm: plain.orm.clone(), alpha: false };
    let size = t.size as usize;
    crate::clay::par_chunks(&mut t.orm, size * 4 * 16, |chunk, rows| {
        for (i, px) in rows.chunks_exact_mut(4).enumerate() {
            let k = chunk * size * 16 + i;
            let uv = Vec2::new(((k % size) as f32 + 0.5) / size as f32, ((k / size) as f32 + 0.5) / size as f32);
            let (tt, p) = polar_of(uv);
            if tt > PI {
                continue;
            }
            // Shadow where lid and eye meet, deepest under the margin.
            let gap = l.contact_at(p) - tt;
            let ao = 0.45 + 0.55 * smoothstep(-0.02, 0.25, gap);
            px[0] = (px[0] as f32 * ao) as u8;
        }
    });
    Arc::new(t)
}

/// Iris and white in one image (see the module notes).
fn paint_texture(eye: &Eye) -> Textures {
    let (pupil, iris, _) = eye.angles();
    let size = TEXTURE as usize;
    let iris_color = Vec3::from(eye.iris_color);
    let sclera = Vec3::from(eye.sclera_color);
    let veins = vein_map(eye, size);
    // Color, height (for the normal map) and roughness per texel.
    let mut color = vec![0u8; size * size * 4];
    let mut height = vec![0f32; size * size];
    let mut rough = vec![0f32; size * size];
    let fbm = |x: f32, y: f32, seed: f32| noise(Vec3::new(x, y, seed)) * 0.6 + noise(Vec3::new(x * 2.3, y * 2.3, seed + 7.0)) * 0.3 + noise(Vec3::new(x * 5.1, y * 5.1, seed + 13.0)) * 0.1;
    let mut texels = vec![[0f32; 5]; size * size];
    let shade = |x: usize, y: usize| -> [f32; 5] {
        let uv = Vec2::new((x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
        let (t, p) = polar_of(uv);
        if t > PI {
            return [1.0, 1.0, 1.0, 0.0, 0.05];
        }
        if t < pupil {
            let c = Vec3::from(eye.pupil_color);
            return [c.x, c.y, c.z, -0.3, 0.3];
        }
        if t < iris && eye.style == EyeStyle::Stylized {
            // Stylised: a clean radial gradient, lighter toward the pupil,
            // a few soft fibers, a crisp dark limbal ring.
            let u = (t - pupil) / (iris - pupil);
            let light = (iris_color * 1.45 + Vec3::splat(0.08)).min(Vec3::ONE);
            let mut c = light.lerp(iris_color, smoothstep(0.0, 0.85, u));
            let fibers = fbm(p * 14.0, u * 2.0, 1.0);
            c *= 0.92 + 0.16 * fibers;
            c *= 1.0 - eye.limbal_ring * 0.85 * smoothstep(0.82, 0.97, u);
            c *= 0.7 + 0.3 * smoothstep(0.0, 0.05, u);
            let c = c.clamp(Vec3::ZERO, Vec3::ONE);
            return [c.x, c.y, c.z, fibers * 0.15, 0.4];
        }
        if t < iris {
            let u = (t - pupil) / (iris - pupil);
            // Fibers: streaks fanning out, a little wavy.
            let q = p + 0.04 * (u * 9.0 + p * 3.0).sin();
            let f1 = fbm(q * 38.0, u * 3.0, 1.0);
            let f2 = fbm(q * 110.0, u * 6.0, 2.0);
            let fibers = f1 * 0.55 + f2 * 0.45;
            // Collarette: a zigzag ring a third of the way out.
            let ring = 0.32 + 0.035 * (p * 13.0).sin() + 0.02 * fbm(p * 20.0, 0.0, 3.0);
            let inner = 1.0 - smoothstep(ring - 0.04, ring + 0.03, u);
            let collar = (-((u - ring) / 0.03).powi(2)).exp();
            // Crypts: dark pits around the collarette.
            let crypt = smoothstep(0.66, 0.78, fbm(q * 16.0, u * 5.0, 4.0)) * smoothstep(0.25, 0.4, u) * (1.0 - smoothstep(0.7, 0.85, u));
            // Contraction furrows near the edge.
            let furrow = smoothstep(0.55, 0.8, u) * (1.0 - (u * 40.0 + fbm(p * 6.0, 0.0, 5.0) * 4.0).sin().abs()).powi(6) * smoothstep(0.4, 0.6, fbm(p * 10.0, u, 6.0));
            let warm = (iris_color.lerp(Vec3::new(0.72, 0.52, 0.26), 0.45) * 1.25).min(Vec3::ONE);
            let mut c = iris_color.lerp(warm, inner * 0.55);
            c *= 0.45 + 1.1 * fibers;
            c *= 1.0 + 0.15 * collar;
            c *= 1.0 - 0.5 * crypt - 0.25 * furrow;
            // Pupillary ruff: a dark rim at the pupil.
            c *= 0.55 + 0.45 * smoothstep(0.0, 0.08, u);
            // Limbal ring.
            c *= 1.0 - eye.limbal_ring * 0.8 * smoothstep(0.78, 1.0, u);
            let h = fibers * 0.8 + collar * 0.8 - crypt * 1.2 - furrow * 0.6;
            let c = c.clamp(Vec3::ZERO, Vec3::ONE);
            return [c.x, c.y, c.z, h, 0.45];
        }
        // The white: a soft gray limbus blending out of the
        // iris, faint warm tint toward the corners, veins.
        let s = t - iris;
        let limbus = 1.0 - smoothstep(0.0, 0.08, s);
        let side = p.cos().abs();
        let mut c = sclera * (1.0 - 0.06 * smoothstep(0.3, 1.2, s));
        c = c.lerp(Vec3::new(0.93, 0.80, 0.76), 0.25 * smoothstep(0.4, 1.3, s) * side);
        c = c.lerp(Vec3::new(0.55, 0.56, 0.6), limbus * 0.5 * eye.limbal_ring);
        let v = veins[y * size + x];
        c = c.lerp(Vec3::new(0.72, 0.16, 0.14), v * 0.8);
        if t > 2.0 {
            c = c.lerp(Vec3::new(0.85, 0.55, 0.55), smoothstep(2.0, 2.8, t) * 0.45);
        }
        let mottle = if eye.style == EyeStyle::Stylized { 0.5 } else { fbm(p * 8.0, s * 8.0, 9.0) };
        c *= 0.97 + 0.06 * mottle;
        let c = c.clamp(Vec3::ZERO, Vec3::ONE);
        [c.x, c.y, c.z, v * 0.3 + mottle * 0.15, 0.12 + 0.05 * mottle]
    };
    crate::clay::par_chunks(&mut texels, size * 8, |chunk, out| {
        for (i, v) in out.iter_mut().enumerate() {
            let k = chunk * size * 8 + i;
            *v = shade(k % size, k / size);
        }
    });
    for (i, v) in texels.iter().enumerate() {
        let c = Vec3::new(v[0], v[1], v[2]) * 255.0;
        color[i * 4..i * 4 + 4].copy_from_slice(&[c.x as u8, c.y as u8, c.z as u8, 255]);
        height[i] = v[3];
        rough[i] = v[4];
    }
    let mut normal = vec![0u8; size * size * 4];
    let mut orm = vec![0u8; size * size * 4];
    let k = 0.35;
    for y in 0..size {
        for x in 0..size {
            let h = |dx: i64, dy: i64| height[((y as i64 + dy).clamp(0, size as i64 - 1) * size as i64 + (x as i64 + dx).clamp(0, size as i64 - 1)) as usize];
            let n = Vec3::new(-(h(1, 0) - h(-1, 0)) * k, (h(0, 1) - h(0, -1)) * k, 1.0).normalize();
            let i = y * size + x;
            let e = (n * 0.5 + 0.5) * 255.0;
            normal[i * 4..i * 4 + 4].copy_from_slice(&[e.x as u8, e.y as u8, e.z as u8, 255]);
            orm[i * 4..i * 4 + 4].copy_from_slice(&[255, (rough[i].clamp(0.0, 1.0) * 255.0) as u8, 0, 255]);
        }
    }
    Textures { size: TEXTURE, color, normal, orm, alpha: false }
}

/// Fine red veins over the white: branching, wandering paths from the hidden
/// back toward the iris, mostly in the corners. Returns coverage per texel.
fn vein_map(eye: &Eye, size: usize) -> Vec<f32> {
    let mut map = vec![0f32; size * size];
    let (_, iris, _) = eye.angles();
    let count = (eye.veins * 40.0) as usize;
    let rand = |k: u32| crate::face::hash(glam::IVec3::new(k as i32, 77, 5), 0x0e7e) as f32 / u32::MAX as f32;
    let mut stamp = |t: f32, p: f32, width: f32, strength: f32| {
        let uv = uv_of(t, p) * size as f32;
        let r = width.max(0.5);
        let (x0, x1) = ((uv.x - r - 1.0).floor() as i64, (uv.x + r + 1.0).ceil() as i64);
        let (y0, y1) = ((uv.y - r - 1.0).floor() as i64, (uv.y + r + 1.0).ceil() as i64);
        for y in y0.max(0)..=y1.min(size as i64 - 1) {
            for x in x0.max(0)..=x1.min(size as i64 - 1) {
                let d = Vec2::new(x as f32 + 0.5, y as f32 + 0.5).distance(uv);
                let a = (1.0 - smoothstep(r * 0.5, r + 0.5, d)) * strength;
                let m = &mut map[y as usize * size + x as usize];
                *m = m.max(a);
            }
        }
    };
    let mut k = 0u32;
    for v in 0..count {
        // Mostly toward the corners (p near 0 or PI), some anywhere.
        let corner = if rand(k) < 0.5 { 0.0 } else { PI };
        let mut p = if rand(k + 1) < 0.8 { corner + (rand(k + 2) - 0.5) * 1.6 } else { rand(k + 2) * TAU };
        let mut t = 1.5 + rand(k + 3) * 0.3;
        let end = iris + 0.12 + rand(k + 4) * 0.55;
        let strength = 0.35 + 0.65 * rand(k + 5);
        let mut heading = 0.0f32;
        let mut width = 2.2 + rand(k + 6) * 1.2;
        k += 10;
        let mut branches: Vec<(f32, f32, f32, f32)> = Vec::new();
        while t > end {
            stamp(t, p, width, strength);
            heading += (rand(k) - 0.5) * 0.5;
            heading = heading.clamp(-1.0, 1.0);
            k += 1;
            t -= 0.006;
            p += heading * 0.006 / t.sin().max(0.2);
            width = (width * 0.994).max(0.7);
            if rand(k) < 0.02 && branches.len() < 3 {
                branches.push((t, p, width * 0.8, if rand(k + 1) < 0.5 { -0.8 } else { 0.8 }));
            }
            k += 2;
        }
        for (mut bt, mut bp, mut bw, bh) in branches {
            let stop = bt - 0.08 - rand(k) * 0.2;
            let mut h = bh;
            while bt > stop.max(iris + 0.08) {
                stamp(bt, bp, bw, strength * 0.8);
                h += (rand(k) - 0.5) * 0.4;
                k += 1;
                bt -= 0.005;
                bp += h * 0.005 / bt.sin().max(0.2);
                bw = (bw * 0.99).max(0.6);
            }
        }
        let _ = v;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_looks_down_z_with_a_clear_cornea_over_a_deep_iris() {
        let eye = Eye::default();
        let m = eye.build_mesh();
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
        // The front is the clear cornea, bulging out.
        let front = m.vertices.iter().max_by(|a, b| a.pos[2].total_cmp(&b.pos[2])).unwrap();
        assert_eq!(front.material, CORNEA);
        assert!(front.pos[2] > 0.5, "the cornea bulges out");
        assert!(m.materials[CORNEA as usize].transmission > 0.9);
        // The iris sits behind the limbus, under the cornea.
        let iris = m.vertices.iter().filter(|v| v.material == IRIS).map(|v| v.pos[2]).fold(f32::MIN, f32::max);
        let (_, edge, _) = eye.angles();
        assert!(iris < 0.5 * edge.cos() + 0.01 && iris < front.pos[2] - 0.05);
        // Textured: the pupil is dark, the iris colored, the white light.
        let tex = m.textures.as_ref().unwrap();
        let at = |uv: Vec2| {
            let (x, y) = ((uv.x * tex.size as f32) as usize, (uv.y * tex.size as f32) as usize);
            let i = (y * tex.size as usize + x) * 4;
            Vec3::new(tex.color[i] as f32, tex.color[i + 1] as f32, tex.color[i + 2] as f32) / 255.0
        };
        assert!(at(Vec2::splat(0.5)).max_element() < 0.1);
        assert!(at(uv_of(1.0, FRAC_PI_2)).min_element() > 0.55, "{:?}", at(uv_of(1.0, FRAC_PI_2)));
        let iris_c = at(uv_of(edge * 0.6, 1.0));
        assert!(iris_c.y > iris_c.x * 0.8 && iris_c.max_element() < 0.9);
        // Textured vertices have tangents; the cornea does not use the map.
        assert!(m.vertices.iter().filter(|v| v.material == SCLERA).all(|v| v.tangent[3] != 0.0));
        assert!(m.vertices.iter().filter(|v| v.material == CORNEA).all(|v| v.tangent[3] == 0.0));
        // Unit diameter.
        let (lo, hi) = m.bounds();
        assert!((hi.x - lo.x - 1.0).abs() < 0.01);
        // Ball faces wind counter-clockwise from outside.
        for t in m.indices.chunks_exact(3).take(2000) {
            let p = |i: u32| Vec3::from(m.vertices[i as usize].pos);
            let face = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
            assert!(face.dot(Vec3::from(m.vertices[t[0] as usize].normal)) >= -1e-9, "triangle {t:?} winds inward");
        }
    }

    #[test]
    fn lids_add_a_tear_line_a_caruncle_and_shadow() {
        // Lids around a front-facing eye of diameter 1 at the origin.
        let ring = |from: f32, to: f32, t: f32| (0..12).map(|i| {
            let p = from + (to - from) * i as f32 / 11.0;
            let d = Vec3::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos()) * 0.5;
            d.to_array()
        }).collect::<Vec<_>>();
        let eye = Eye { lids: Some(Lids { upper: ring(PI, 0.0, 0.7), lower: ring(PI, TAU, 0.75) }), ..Eye::default() };
        let plain = Eye::default().build_mesh();
        let m = eye.build_mesh_in(Some(Mat4::IDENTITY));
        assert!(m.vertices.iter().any(|v| v.material == TEAR));
        let car: Vec<Vec3> = m.vertices.iter().filter(|v| v.material == CARUNCLE).map(|v| Vec3::from(v.pos)).collect();
        assert!(!car.is_empty() && car.iter().all(|p| p.x < -0.25), "caruncle at the inner corner (-X)");
        // Under the lid the white is shadowed; in the open it is not.
        let (a, b) = (plain.textures.as_ref().unwrap(), m.textures.as_ref().unwrap());
        let ao = |t: &Textures, uv: Vec2| t.orm[(((uv.y * t.size as f32) as usize) * t.size as usize + (uv.x * t.size as f32) as usize) * 4];
        assert!(ao(b, uv_of(1.2, FRAC_PI_2)) < 120 && ao(a, uv_of(1.2, FRAC_PI_2)) == 255);
        assert!(ao(b, uv_of(0.3, 0.0)) > 200);
        // Saved and loaded unchanged.
        let back: Eye = serde_json::from_str(&serde_json::to_string(&eye).unwrap()).unwrap();
        assert_eq!(back, eye);
    }

    /// The bits of an eye's mesh and texture.
    fn eye_hash(m: &Mesh) -> u64 {
        use std::hash::Hasher;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        for v in &m.vertices {
            for x in v.pos.iter().chain(&v.normal).chain(&v.color).chain(&v.uv).chain(&v.surface).chain(&v.light) { h.write_u32(x.to_bits()); }
            h.write_u32(v.material);
        }
        for i in &m.indices { h.write_u32(*i); }
        let t = m.textures.as_ref().unwrap();
        h.write(&t.color);
        h.write(&t.normal);
        h.write(&t.orm);
        for mat in &m.materials { h.write(serde_json::to_string(mat).unwrap().as_bytes()); }
        h.finish()
    }

    /// The realistic eye is the same bits as before Stage 4d (pinned).
    #[test]
    fn realistic_eye_is_pinned() {
        let got = eye_hash(&Eye::default().build_mesh());
        eprintln!("realistic eye: {got:#x}");
        assert_eq!(got, 0xb65dff6c8090cbfa);
    }

    /// A stylised eye: a bigger iris than the realistic one, a catch-light
    /// disc (its own glowing slot) on the iris under the cornea, facing
    /// out, and a texture without veins; turning the catch-light off
    /// leaves no disc and no slot.
    #[test]
    fn stylized_eye_has_a_big_iris_and_a_catchlight() {
        let mut e = Eye::default();
        e.stylize(&|_| false);
        assert!(e.catchlight && e.iris_scale == 1.3 && e.veins == 0.0 && e.cornea == 0.03);
        let (_, iris, _) = e.angles();
        let (_, real_iris, _) = Eye::default().angles();
        assert!(iris > real_iris * 1.25, "{iris} against {real_iris}");
        let m = e.build_mesh();
        assert_eq!(m.materials.len(), 6);
        assert!(m.materials[CATCHLIGHT as usize].emission > 1.0);
        let disc: Vec<u32> = m.indices.chunks_exact(3).filter(|t| m.vertices[t[0] as usize].material == CATCHLIGHT).flatten().copied().collect();
        assert!(!disc.is_empty());
        // Drawn before the cornea: the viewport draws the dome over it.
        let first = |slot: u32| m.indices.chunks_exact(3).position(|t| m.vertices[t[0] as usize].material == slot).unwrap();
        assert!(first(CATCHLIGHT) < first(CORNEA));
        let corner = |i: u32| Vec3::from(m.vertices[i as usize].pos);
        for t in disc.chunks_exact(3) {
            let face = (corner(t[1]) - corner(t[0])).cross(corner(t[2]) - corner(t[0]));
            assert!(face.dot(Vec3::from(m.vertices[t[0] as usize].normal)) > 0.0, "the disc faces out");
            // Up and to the right of the pupil, on the iris, under the cornea.
            let p = corner(t[0]);
            assert!(p.x > 0.0 && p.y > 0.0 && p.z < 0.5 + e.cornea);
        }
        // The iris texture of a stylised eye has no red veins.
        let tex = m.textures.as_ref().unwrap();
        let reddest = tex.color.chunks_exact(4).map(|c| c[0] as i32 - c[1] as i32).max().unwrap();
        assert!(reddest < 60, "{reddest}");
        // Huge scales stay a sane iris.
        let big = Eye { iris_size: 0.9, iris_scale: 1.6, ..e.clone() };
        assert!(big.angles().1.is_finite() && (big.iris_width() - MAX_IRIS).abs() < 1e-6);
        let off = Eye { catchlight: false, ..e };
        let m = off.build_mesh();
        assert!(m.materials.len() == 5 && m.vertices.iter().all(|v| v.material != CATCHLIGHT));
    }
}
