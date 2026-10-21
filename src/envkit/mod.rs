//! The environment kit: what dungeon rooms, crypts and caves are made of.
//!
//! - `kit`: modular pieces made of real stones (walls, corners, doorways
//!   with arches, pillars, stairs, flagstone floors, vaults), rocks and
//!   rubble, and props that give off light (torches, candles, braziers),
//!   bones and chains. One `Kit` per object, its mesh built in `MeshLib`.
//! - `terrain`: a heightfield for cave floors and outdoor ground, shaped by
//!   noise and cracks, sculpted and painted with brushes.
//! - `scatter`: many copies of other objects over an area or a surface,
//!   stored as instances (a source and a transform each).
//! - `light`: light sources carried by objects (`Object::light`), which the
//!   film renderer and the viewport light the scene with.
//!
//! Everything is deterministic: the same settings and seed always build
//! the same stones, so scenes save only the settings.

#[cfg(test)]
mod check;
pub mod kit;
pub mod light;
pub mod scatter;
pub mod stone;
pub mod terrain;

use glam::{Vec2, Vec3};

use crate::material::Material;
use crate::mesh::{Mesh, Vertex};

/// The kit's modular grid, in meters: pieces are sized in whole cells and
/// snap to it, so walls, corners and floors meet without gaps.
pub const GRID: f32 = 1.0;

/// Small deterministic random numbers (SplitMix64).
#[derive(Clone)]
pub struct Rand(u64);

impl Rand {
    pub fn new(seed: u64) -> Rand {
        Rand(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    /// A stream for item `i` of a seed (each block of a wall gets its own).
    pub fn of(seed: u64, i: u64) -> Rand {
        let mut r = Rand::new(seed ^ i.wrapping_mul(0xA24B_AED4_963E_E407));
        r.next();
        r
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0..1.
    pub fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }

    /// -1..1.
    pub fn signed(&mut self) -> f32 {
        self.f() * 2.0 - 1.0
    }

    pub fn unit(&mut self) -> Vec3 {
        let z = self.signed();
        let a = self.f() * std::f32::consts::TAU;
        let r = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(r * a.cos(), z, r * a.sin())
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
}

fn hash(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ (z as u32).wrapping_mul(0xcb1a_b31f) ^ seed.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    h as f32 / u32::MAX as f32
}

/// Smooth value noise, -1..1.
pub fn noise(p: Vec3, seed: u32) -> f32 {
    let c = p.floor();
    let f = p - c;
    let f = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let (x, y, z) = (c.x as i32, c.y as i32, c.z as i32);
    let mut v = 0.0;
    for k in 0..8 {
        let (dx, dy, dz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
        let w = (if dx == 0 { 1.0 - f.x } else { f.x }) * (if dy == 0 { 1.0 - f.y } else { f.y }) * (if dz == 0 { 1.0 - f.z } else { f.z });
        v += hash(x + dx, y + dy, z + dz, seed) * w;
    }
    v * 2.0 - 1.0
}

/// Fractal noise: `octaves` layers, each twice as fine and half as strong
/// (about -1..1).
pub fn fbm(p: Vec3, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut q) = (0.0, 1.0, 0.0, p);
    for i in 0..octaves {
        sum += noise(q, seed.wrapping_add(i * 101)) * amp;
        norm += amp;
        amp *= 0.5;
        q = q * 2.03 + Vec3::splat(17.3);
    }
    sum / norm.max(1e-6)
}

/// Ridged noise for cracks and sharp crests: 0 on the ridge lines, 1 away.
pub fn ridge(p: Vec3, seed: u32) -> f32 {
    noise(p, seed).abs()
}

/// Distance to the nearest cell edge of a jittered (Voronoi) cell pattern
/// in the XZ plane, in cell units: 0 on a crack line. Also returns the cell id.
pub fn cells(p: Vec2, seed: u32) -> (f32, u32) {
    let c = p.floor();
    let (cx, cy) = (c.x as i32, c.y as i32);
    let point = |x: i32, y: i32| Vec2::new(x as f32 + hash(x, y, 1, seed), y as f32 + hash(x, y, 2, seed));
    let (mut best, mut second, mut id) = (f32::MAX, f32::MAX, 0u32);
    let mut nearest = Vec2::ZERO;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let q = point(cx + dx, cy + dy);
            let d = q.distance_squared(p);
            if d < best {
                second = best;
                best = d;
                nearest = q;
                id = ((cx + dx) as u32).wrapping_mul(73_856_093) ^ ((cy + dy) as u32).wrapping_mul(19_349_663);
            } else if d < second {
                second = d;
            }
        }
    }
    // Distance to the bisector between the nearest two points, measured
    // properly against every neighbor.
    let mut edge = f32::MAX;
    for dy in -2..=2 {
        for dx in -2..=2 {
            let q = point(cx + dx, cy + dy);
            if q.distance_squared(nearest) < 1e-10 {
                continue;
            }
            let mid = (q + nearest) * 0.5;
            let dir = (q - nearest).normalize_or_zero();
            edge = edge.min((mid - p).dot(dir));
        }
    }
    let _ = second;
    (edge.max(0.0), id)
}

pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Builds a mesh from parts with their own materials and vertex colors.
/// Vertices may be welded by key so displaced grids stay watertight.
pub struct Builder {
    pub mesh: Mesh,
    /// Wireframe outline edges (kept coarse: each stone outlines its block,
    /// not every triangle).
    outline: Vec<u32>,
}

impl Builder {
    pub fn new(materials: Vec<Material>) -> Builder {
        Builder { mesh: Mesh { materials, ..Mesh::default() }, outline: Vec::new() }
    }

    /// Index of the material called `name` (added if new).
    pub fn material(&mut self, m: Material) -> u32 {
        if let Some(i) = self.mesh.materials.iter().position(|x| x.name == m.name) {
            return i as u32;
        }
        self.mesh.materials.push(m);
        self.mesh.materials.len() as u32 - 1
    }

    pub fn vertex(&mut self, pos: Vec3, normal: Vec3, color: Vec3, material: u32) -> u32 {
        let m = &self.mesh.materials[material as usize];
        let color = color.clamp(Vec3::ZERO, Vec3::ONE);
        self.mesh.vertices.push(Vertex {
            pos: pos.into(),
            normal: normal.normalize_or(Vec3::Y).into(),
            color: color.into(),
            surface: m.surface(),
            material,
            uv: [0.0; 2],
            tangent: [0.0; 4],
            light: m.light(),
            // Smart-material masks are not measured on kit pieces (yet).
            base: [color.x, color.y, color.z, -1.0],
            wear: [0.0; 3],
            painted: [0; 4],
        });
        self.mesh.vertices.len() as u32 - 1
    }

    pub fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.mesh.indices.extend_from_slice(&[a, b, c]);
    }

    pub fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    pub fn edge(&mut self, a: u32, b: u32) {
        self.outline.extend_from_slice(&[a, b]);
    }

    /// Vertex normals from the triangles (area weighted) for vertices in
    /// `from..`, keeping each triangle's winding: normals point the way the
    /// triangles wind counter-clockwise.
    pub fn smooth_normals(&mut self, from: usize) {
        let n = self.mesh.vertices.len();
        let mut acc = vec![Vec3::ZERO; n - from];
        for t in self.mesh.indices.chunks_exact(3) {
            if (t[0] as usize) < from {
                continue;
            }
            let p = |i: u32| Vec3::from(self.mesh.vertices[i as usize].pos);
            let face = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
            for &i in t {
                acc[i as usize - from] += face;
            }
        }
        for (v, a) in self.mesh.vertices[from..].iter_mut().zip(acc) {
            v.normal = a.normalize_or(Vec3::from(v.normal)).into();
        }
    }

    /// Appends a finished mesh (its materials matched by name).
    pub fn append(&mut self, other: &Mesh, transform: glam::Mat4, tint: Vec3) {
        let base = self.mesh.vertices.len() as u32;
        let map: Vec<u32> = other.materials.iter().map(|m| self.material(m.clone())).collect();
        let normal = glam::Mat3::from_mat4(transform).inverse().transpose();
        for v in &other.vertices {
            let mut v = *v;
            v.pos = transform.transform_point3(Vec3::from(v.pos)).into();
            v.normal = (normal * Vec3::from(v.normal)).normalize_or(Vec3::Y).into();
            v.color = (Vec3::from(v.color) * tint).into();
            v.material = map.get(v.material as usize).copied().unwrap_or(0);
            self.mesh.vertices.push(v);
        }
        self.mesh.indices.extend(other.indices.iter().map(|i| i + base));
        self.outline.extend(other.edges.iter().map(|i| i + base));
    }

    /// The mesh, with weathering applied (cavities darker, edges worn)
    /// for weathered materials, and the outline as wireframe edges.
    pub fn finish(mut self, wear_width: f32) -> Mesh {
        self.mesh.unshaded=Some((std::sync::Arc::new(self.mesh.vertices.iter().map(|v|v.color).collect()),wear_width));
        if self.mesh.materials.iter().any(Material::weathered) && !self.mesh.indices.is_empty() {
            let wear = crate::weather::wear_maps(&self.mesh, wear_width);
            let mats: Vec<Material> = self.mesh.materials.iter().map(|m| {
                let mut m = m.clone();
                if m.variation_scale <= 0.0 { m.variation_scale = wear_width * 8.0; }
                m
            }).collect();
            for (v, w) in self.mesh.vertices.iter_mut().zip(wear) {
                let m = &mats[v.material as usize];
                if m.weathered() {
                    let (c, r) = crate::weather::apply(m, Vec3::from(v.pos), Vec3::from(v.normal), Vec3::from(v.color), v.surface[0], w, wear_width);
                    v.color = c.to_array();
                    v.surface[0] = r;
                }
            }
        }
        crate::smart::finish_mesh(&mut self.mesh, wear_width);
        self.mesh.edges = self.outline;
        self.mesh
    }
}

/// sRGB color from a JSON-ish array with a fallback.
pub fn color(c: Option<[f32; 3]>, default: [f32; 3]) -> Vec3 {
    Vec3::from(c.unwrap_or(default)).clamp(Vec3::ZERO, Vec3::ONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_smooth_bounded_and_repeatable() {
        let p = Vec3::new(1.3, -2.7, 0.4);
        assert_eq!(noise(p, 3), noise(p, 3));
        assert_ne!(noise(p, 3), noise(p, 4));
        for i in 0..1000 {
            let q = Vec3::new(i as f32 * 0.137, i as f32 * 0.071, -(i as f32) * 0.053);
            let v = fbm(q, 4, 1);
            assert!((-1.0..=1.0).contains(&v));
            assert!((noise(q, 1) - noise(q + Vec3::splat(1e-3), 1)).abs() < 0.02);
        }
        let (d, _) = cells(Vec2::new(0.37, 4.2), 9);
        assert!((0.0..1.5).contains(&d));
    }
}
