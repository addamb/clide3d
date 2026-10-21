//! Film renderer: a path tracer, like Blender's Cycles, in Clide3D.
//!
//! Every visible mesh (as drawn now, posed) goes into one BVH in world
//! space. Each pixel sends jittered camera and thin-lens rays. Every bounce
//! samples the environment and optional studio or local lights with shadows,
//! then picks a direction from the surface's material. Environment and BSDF
//! samples share the light through MIS. Hair cards and other cut-out
//! textures let rays through where their alpha is clear.
//!
//! Skin and other materials with `subsurface` let light in: a ray that
//! enters comes back out at a nearby point of the same mesh, picked from a
//! diffusion profile (Christensen–Burley) per color, so red light spreads
//! furthest and skin glows softly at shadow edges, ears and fingers.
//!
//! Hair renders as real strands (`Fiber`): each segment is a thin ribbon
//! turned toward the camera, shaded with shifted R / TT / TRT lobes and
//! pigment absorption. Light scatters between fibers, softly lighting
//! the interior of a head of hair.
//!
//! The result is denoised (an edge-aware filter guided by the first hit's
//! color, normal and depth) and tone-mapped with a filmic curve.

pub(crate) mod bvh;
mod denoise;
pub mod dice;
pub mod gpu;
#[cfg(test)]
mod checks;
pub mod lighting;
pub mod look;
mod post;
pub use look::Look;
pub mod shade;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glam::{Mat3, Mat4, Vec2, Vec3, Vec4};

use crate::material::Material;
use crate::mesh::{Mesh, Textures};
use bvh::{Bvh, Hit};
use shade::{Frame, Surface};

/// One hair strand in world space (see `hair::Strand`).
pub struct Fiber {
    pub points: Vec<Vec3>,
    pub root_width: f32,
    pub tip_width: f32,
    /// Linear colors.
    pub root_color: Vec3,
    pub tip_color: Vec3,
}

/// Shading data of one hair ribbon triangle.
struct HairSeg {
    tangent: Vec3,
    color: Vec3,
}

/// Set in `Hit::tri` for hits on hair ribbons (they have their own BVH;
/// ribbon triangle `i` belongs to `HairSeg` `i / 2`).
const HAIR: u32 = 1 << 31;

/// One mesh instance to render: its mesh, world matrix, tint (sRGB) and
/// shader values for vertices that defer to the object (primitives).
pub struct Item {
    pub mesh: Arc<Mesh>,
    pub world: Mat4,
    pub tint: [f32; 3],
    pub surface: [f32; 4],
    /// The object's material, for vertices that defer to it.
    pub material: Material,
}

#[derive(Clone, Copy)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub fov_deg: f32,
    /// Some(half height) for orthographic.
    pub ortho: Option<f32>,
}

#[derive(Clone, Copy)]
pub struct Settings {
    pub width: usize,
    pub height: usize,
    /// Samples per pixel (the most; a time limit can stop sooner).
    pub samples: u32,
    pub bounces: u32,
    pub time_limit: Duration,
    /// A floor under the scene that catches shadows.
    pub floor: bool,
    pub exposure: f32,
    pub denoise: bool,
}

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// sRGB 8-bit.
    pub pixels: Vec<[u8; 3]>,
    /// Denoised scene-linear radiance, retained for temporal integration.
    pub linear: Vec<Vec3>,
    pub samples: u32,
    pub seconds: f32,
}

/// Per-triangle shading data, in world space.
struct Tri {
    item: u32,
    v: [u32; 3],
}

/// The meshes of a render, in world space: built once and shared by every
/// view (hair is not here: its ribbons turn toward each view's camera).
pub struct Meshes {
    bvh: Bvh,
    tris: Vec<Tri>,
    items: Vec<ItemData>,
    lowest: f32,
}

struct Scene<'a> {
    look: Arc<Look>,
    lights: lighting::LightSet,
    meshes: &'a Meshes,
    hair_bvh: Bvh,
    hair: Vec<HairSeg>,
    scalp: ScalpTint,
    floor_y: Option<f32>,
}

/// Root pigment under dense hair prevents bright skin glints between
/// follicles. A small world-space grid keeps this local to the scalp.
struct ScalpTint {
    grid: std::collections::HashMap<(i32, i32, i32), (Vec3, Vec3, f32)>,
    bounds: (Vec3, Vec3),
}

impl ScalpTint {
    const CELL: f32 = 0.009;

    fn key(p: Vec3) -> (i32, i32, i32) {
        let p = (p / Self::CELL).floor().as_ivec3();
        (p.x, p.y, p.z)
    }

    fn new(fibers: &[Fiber]) -> Self {
        let mut grid = std::collections::HashMap::new();
        for fiber in fibers {
            // Brows and lashes should never dye the surrounding skin.
            if fiber.points.len() < 2 || fiber.points[0].distance(*fiber.points.last().unwrap()) < 0.015 { continue; }
            let entry = grid.entry(Self::key(fiber.points[0])).or_insert((Vec3::ZERO, Vec3::ZERO, 0.0));
            entry.0 += fiber.points[0];
            entry.1 += fiber.root_color;
            entry.2 += 1.0;
        }
        for (point, color, count) in grid.values_mut() {
            *point /= *count;
            *color /= *count;
        }
        let bounds = grid.values().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), &(p, _, _)|
            (lo.min(p - Vec3::splat(Self::CELL)), hi.max(p + Vec3::splat(Self::CELL))));
        Self { grid, bounds }
    }

    fn at(&self, p: Vec3) -> Option<(Vec3, f32)> {
        if !p.cmpge(self.bounds.0).all() || !p.cmple(self.bounds.1).all() { return None; }
        let (x, y, z) = Self::key(p);
        let mut best = None;
        let mut nearest = Self::CELL;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(&(root, color, count)) = self.grid.get(&(x + dx, y + dy, z + dz)) {
                        let d = root.distance(p);
                        if d < nearest {
                            nearest = d;
                            let t = 1.0 - d / Self::CELL;
                            best = Some((color * 0.7, 0.85 * t * t * (3.0 - 2.0 * t) * count / (count + 8.0)));
                        }
                    }
                }
            }
        }
        best
    }
}

struct ItemData {
    mesh: Arc<Mesh>,
    normal: Mat3,
    model: Mat3,
    tint: Vec3,
    surface: [f32; 4],
    material: Material,
    textures: Option<Arc<Textures>>,
}

/// Light-related material values at a hit.
#[derive(Clone, Copy, Default)]
struct Optics {
    /// Light given off, times the surface color.
    emission: f32,
    /// Independent emissive color from an imported glTF material.
    emitted: Vec3,
    /// Chance a ray passes through (glass).
    transmission: f32,
    ior: f32,
    /// Whether the ray hit the outside (meshes wind outward).
    front: bool,
    /// Native alpha hair cards scatter along their UV bitangent.
    hair: Option<Vec3>,
    /// A flame: seen by the camera, passed by every other ray (the light
    /// it stands for is a light of its object).
    flame: bool,
}

/// Light picked up after the first bounce is clamped to this (per
/// sample, brightest channel), like Cycles' indirect clamp: a diffuse
/// bounce that lands a few centimeters from a candle flame would
/// otherwise be a white speck on a dark wall.
pub const INDIRECT_CLAMP: f32 = 10.0;

fn clamp_indirect(c: Vec3, bounce: u32) -> Vec3 {
    if bounce == 0 { c } else { c * (INDIRECT_CLAMP / c.max_element().max(INDIRECT_CLAMP)) }
}

/// Most glass crossings per path; they do not use up bounces.
const MAX_GLASS_HOPS: u32 = 12;
/// Scatterings inside hair that do not use up bounces: light crosses many
/// fibers in a groom, and cutting it short darkens blond hair.
const MAX_HAIR_HOPS: u32 = 8;

/// Thin parts let light through (see `Scene::probe`): how far in to look
/// for the far side, in scatter distances, and the chance to follow the
/// light through when there is one.
const THIN: f32 = 6.0;
const THROUGH: f32 = 0.3;

/// Subsurface settings at a hit: amount and per-color distance.
#[derive(Clone, Copy)]
struct Sss {
    amount: f32,
    radius: Vec3,
    item: u32,
}

/// Studio lights: (direction toward the light, angular radius, the light
/// it casts on a surface facing it). A key, a large soft fill, and a rim
/// light from behind that outlines hair and shoulders.
const LIGHTS: [(Vec3, f32, Vec3); 3] = [
    (Vec3::new(0.45, 0.75, 0.55), 0.12, Vec3::new(3.1, 2.95, 2.75)),
    (Vec3::new(-0.7, 0.25, 0.45), 0.35, Vec3::new(0.7, 0.75, 0.85)),
    (Vec3::new(-0.2, 0.5, -0.85), 0.1, Vec3::new(2.2, 2.2, 2.3)),
];

impl Meshes {
    pub fn new(items: Vec<Item>) -> Meshes {
        let mut corners = Vec::new();
        let mut tris = Vec::new();
        let mut data = Vec::new();
        let mut lowest = f32::MAX;
        for (k, it) in items.into_iter().enumerate() {
            let p: Vec<Vec3> = it.mesh.vertices.iter().map(|v| it.world.transform_point3(Vec3::from(v.pos))).collect();
            lowest = p.iter().fold(lowest, |m, q| m.min(q.y));
            for t in it.mesh.indices.chunks_exact(3) {
                corners.push([p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]);
                tris.push(Tri { item: k as u32, v: [t[0], t[1], t[2]] });
            }
            let model = Mat3::from_mat4(it.world);
            data.push(ItemData {
                textures: it.mesh.textures.clone(),
                normal: model.inverse().transpose(),
                model,
                tint: srgb(Vec3::from(it.tint)),
                surface: it.surface,
                material: it.material,
                mesh: it.mesh,
            });
        }
        Meshes { bvh: Bvh::new(&corners), tris, items: data, lowest }
    }
}

impl<'a> Scene<'a> {
    fn new(meshes: &'a Meshes, fibers: &[Fiber], eye: Vec3, floor: bool, look: Arc<Look>) -> Scene<'a> {
        let mut corners = Vec::new();
        let mut lowest = meshes.lowest;
        let mut hair = Vec::new();
        // Strands: one ribbon quad per segment, turned toward the camera.
        for f in fibers {
            let n = f.points.len();
            if n < 2 {
                continue;
            }
            let side = |k: usize| {
                let t = f.points[(k + 1).min(n - 1)] - f.points[k.saturating_sub(1)];
                let s = t.cross(eye - f.points[k]).normalize_or(t.any_orthonormal_vector());
                let a = k as f32 / (n - 1) as f32;
                s * 0.5 * (f.root_width + (f.tip_width - f.root_width) * a)
            };
            for k in 0..n - 1 {
                let (a, b) = (f.points[k], f.points[k + 1]);
                let (sa, sb) = (side(k), side(k + 1));
                let mid = (k as f32 + 0.5) / (n - 1) as f32;
                hair.push(HairSeg { tangent: (b - a).normalize_or(Vec3::Y), color: f.root_color.lerp(f.tip_color, mid) });
                corners.push([a - sa, a + sa, b + sb]);
                corners.push([a - sa, b + sb, b - sb]);
                lowest = lowest.min(a.y);
            }
        }
        Scene { lights: lighting::LightSet::new(&look.lights), look, meshes, hair_bvh: Bvh::new(&corners), hair, scalp: ScalpTint::new(fibers), floor_y: (floor && lowest < f32::MAX).then_some(lowest - 0.002) }
    }

    fn hair_at(&self, h: &Hit) -> Option<&HairSeg> {
        (h.tri & HAIR != 0).then(|| &self.hair[((h.tri & !HAIR) / 2) as usize])
    }

    /// Whether a hit lands on a clear (cut-out) texel and should be passed through.
    fn clear(&self, h: &Hit) -> bool {
        if h.tri & HAIR != 0 {
            return false;
        }
        let tri = &self.meshes.tris[h.tri as usize];
        let it = &self.meshes.items[tri.item as usize];
        let Some(tex) = it.textures.as_ref().filter(|t| t.alpha) else { return false };
        let uv = self.lerp_uv(it, tri, h);
        let alpha = sample(&tex.color, tex.size, imported_uv(it, uv, 0))[3];
        if let Some(m) = &it.mesh.imported {
            let w = [1.0 - h.u - h.v, h.u, h.v];
            let vertex: f32 = tri.v.iter().zip(w).map(|(&i, w)| m.vertex_alpha.get(i as usize).copied().unwrap_or(1.0) * w).sum();
            alpha * m.base_alpha * vertex < m.alpha_cutoff.unwrap_or(0.5)
        } else { alpha < 0.5 }
    }

    fn lerp_uv(&self, it: &ItemData, tri: &Tri, h: &Hit) -> Vec2 {
        let w = [1.0 - h.u - h.v, h.u, h.v];
        tri.v.iter().zip(w).map(|(&i, w)| Vec2::from(it.mesh.vertices[i as usize].uv) * w).sum()
    }

    /// Nearest hit (meshes or floor): (t, shading point data). Only
    /// `camera` rays (and their glass hops) see flames.
    fn trace(&self, o: Vec3, d: Vec3, camera: bool) -> Option<(f32, Option<Hit>)> {
        let skip = |h: &Hit| self.clear(h) || (!camera && self.flame(h));
        let mut mesh = self.meshes.bvh.hit(o, d, 1e-4, f32::MAX, &skip);
        let far = mesh.map_or(f32::MAX, |h| h.t);
        if let Some(mut h) = self.hair_bvh.hit(o, d, 1e-4, far, &|_| false) {
            h.tri |= HAIR;
            mesh = Some(h);
        }
        let floor = self.floor_y.and_then(|y| {
            let t = (y - o.y) / d.y;
            (d.y < 0.0 && t > 1e-4).then_some(t)
        });
        match (mesh, floor) {
            (Some(h), Some(f)) if f < h.t => Some((f, None)),
            (Some(h), _) => Some((h.t, Some(h))),
            (None, Some(f)) => Some((f, None)),
            (None, None) => None,
        }
    }

    /// Whether a mesh hit is a flame (see `Optics::flame`).
    fn flame(&self, h: &Hit) -> bool {
        let tri = &self.meshes.tris[h.tri as usize];
        let it = &self.meshes.items[tri.item as usize];
        Self::material_of(it, tri).name == "flame"
    }

    /// Whether a mesh hit is glass or a flame, which shadow rays pass (no caustics).
    fn glass(&self, h: &Hit) -> bool {
        let tri = &self.meshes.tris[h.tri as usize];
        let it = &self.meshes.items[tri.item as usize];
        let m = Self::material_of(it, tri);
        m.transmission >= 0.5 || m.name == "flame"
    }

    fn material_of<'m>(it: &'m ItemData, tri: &Tri) -> &'m Material {
        let v = &it.mesh.vertices[tri.v[0] as usize];
        if v.surface[0] < 0.0 { &it.material } else { it.mesh.materials.get(v.material as usize).unwrap_or(&it.material) }
    }

    fn occluded(&self, o: Vec3, d: Vec3) -> bool {
        self.blocked(o, d, f32::MAX)
    }

    fn blocked(&self, o: Vec3, d: Vec3, far: f32) -> bool {
        let skip = |h: &Hit| self.clear(h) || self.glass(h);
        let floor = self.floor_y.is_some_and(|y| {
            let t = (y - o.y) / d.y;
            t > 1e-4 && t < far
        });
        floor || self.hair_bvh.any_hit(o, d, 1e-4, far, &|_| false) || self.meshes.bvh.any_hit(o, d, 1e-4, far, &skip)
    }

    /// Surface data at a mesh hit: position, geometric normal, shading
    /// normal, and the material.
    fn surface(&self, o: Vec3, d: Vec3, t: f32, h: &Hit) -> (Vec3, Vec3, Vec3, Surface, Option<Sss>, Optics) {
        let tri = &self.meshes.tris[h.tri as usize];
        let it = &self.meshes.items[tri.item as usize];
        let vs = tri.v.map(|i| &it.mesh.vertices[i as usize]);
        let w = [1.0 - h.u - h.v, h.u, h.v];
        let p = o + d * t;
        let pw = vs.map(|v| it.model * Vec3::from(v.pos));
        let mut ng = (pw[1] - pw[0]).cross(pw[2] - pw[0]).normalize_or(Vec3::Y);
        let mut n = (it.normal * (Vec3::from(vs[0].normal) * w[0] + Vec3::from(vs[1].normal) * w[1] + Vec3::from(vs[2].normal) * w[2])).normalize_or(ng);
        let front = ng.dot(d) < 0.0;
        // Meshes are drawn double sided: face both normals toward the ray.
        if ng.dot(d) > 0.0 {
            ng = -ng;
        }
        if n.dot(ng) < 0.0 {
            n = -n;
        }
        let color: Vec3 = vs.iter().zip(w).map(|(v, w)| srgb(Vec3::from(v.color)) * w).sum();
        let mut s4 = vs[0].surface;
        let material = if s4[0] < 0.0 { &it.material } else { it.mesh.materials.get(vs[0].material as usize).unwrap_or(&it.material) };
        if s4[0] < 0.0 {
            s4 = it.surface;
        }
        let mut optics = Optics { emitted: Vec3::ZERO, emission: material.emission, transmission: material.transmission, ior: material.ior.max(1.0), front, hair: None, flame: material.name == "flame" };
        let sss = (material.subsurface > 0.0 && material.transmission <= 0.0).then(|| Sss { amount: material.subsurface, radius: Vec3::from(material.scatter), item: tri.item });
        let mut surf = Surface { albedo: color * it.tint, rough: s4[0], metal: s4[1], coat: s4[2], sheen: s4[3], skin: material.skin_specular * (1.0 - s4[1]) };
        // Smart layers and decals (`smart.rs`) start from the color before
        // them and are laid on per pixel below; the tint goes on last.
        let smart = it.textures.is_none() && (crate::smart::layered(material) || !it.mesh.decals.is_empty());
        if smart {
            let base: Vec4 = vs.iter().zip(w).map(|(v, w)| Vec4::from(v.base) * w).sum();
            surf.albedo = srgb(base.truncate());
            if vs[0].base[3] >= 0.0 && vs[0].surface[0] >= 0.0 {
                surf.rough = base.w;
            }
            if vs[0].surface[0] >= 0.0 {
                (surf.metal, surf.coat, surf.sheen) = (material.metallic, material.clearcoat, material.sheen);
            }
            surf.skin = material.skin_specular * (1.0 - surf.metal);
        }
        if let Some(tex) = &it.textures
            && vs[0].tangent[3] != 0.0
        {
            let uv = self.lerp_uv(it, tri, h);
            let c = sample(&tex.color, tex.size, imported_uv(it, uv, 0));
            surf.albedo *= srgb(Vec3::new(c[0], c[1], c[2]));
            let orm = sample(&tex.orm, tex.size, imported_uv(it, uv, 2));
            surf.rough = orm[1];
            surf.metal = orm[2];
            let nm = sample(&tex.normal, tex.size, imported_uv(it, uv, 1));
            let tn = Vec3::new(nm[0] * 2.0 - 1.0, nm[1] * 2.0 - 1.0, nm[2] * 2.0 - 1.0);
            let tan: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from_slice(&v.tangent) * w).sum();
            let tan = it.model * tan;
            let tan = (tan - n * n.dot(tan)).normalize_or(n.any_orthonormal_vector());
            let bit = n.cross(tan) * vs[0].tangent[3].signum();
            if tex.alpha && material.name == "hair" { optics.hair = Some(if front { bit } else { -bit }); }
            n = (tan * tn.x + bit * tn.y + n * tn.z).normalize_or(n);
            // Baked AO darkens what the renderer cannot see (finer than the mesh).
            let ao = sample(&tex.orm, tex.size, imported_uv(it, uv, 3))[0];
            surf.albedo *= 0.5 + 0.5 * ao;
        }
        if let Some(m) = &it.mesh.imported {
            optics.emitted = Vec3::from(m.emissive);
            if let Some(tex) = &m.emissive_texture {
                let c = sample(tex, m.size, imported_uv(it, self.lerp_uv(it, tri, h), 4));
                optics.emitted *= srgb(Vec3::new(c[0], c[1], c[2]));
            }
        }
        // A human face shades its skin by region (`face.rs`); other skin
        // gets the even procedural pores.
        let face = it.mesh.face.as_ref().filter(|_| it.textures.is_none() && material.subsurface > 0.0 && material.transmission <= 0.0);
        // How much library detail shows: none where a face shades the skin.
        let mut detail_share = 1.0;
        if let Some(face) = face {
            let local: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.pos) * w).sum();
            let local_n = (it.normal.inverse() * n).normalize_or(Vec3::Z);
            // Below the face (down the neck) the plain skin detail shows.
            let keep = face.keep(local);
            detail_share = 1.0 - keep;
            let (mut nin, mut cin, mut rin) = (local_n, surf.albedo.powf(1.0 / 2.2), surf.rough);
            if keep < 1.0 && (material.pores > 0.0 || material.mottling > 0.0) {
                let (sn, sc, sr) = material.skin_detail(local, local_n, cin, rin);
                (nin, cin, rin) = (nin.lerp(sn, 1.0 - keep).normalize_or(nin), cin.lerp(sc, 1.0 - keep), rin + (sr - rin) * (1.0 - keep));
            }
            let look = face.shade(local, nin, cin, rin);
            n = (it.normal * look.n).normalize_or(n);
            surf.albedo = srgb(look.color);
            surf.rough = look.rough;
            surf.coat = surf.coat.max(look.coat);
            surf.sheen += look.sheen;
        } else if it.textures.is_none() && material.subsurface > 0.0 && (material.pores > 0.0 || material.mottling > 0.0) {
            let local: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.pos) * w).sum();
            let local_n = (it.normal.inverse() * n).normalize_or(Vec3::Z);
            let (detail_n, color, rough) = material.skin_detail(local, local_n, surf.albedo.powf(1.0 / 2.2), surf.rough);
            n = (it.normal * detail_n).normalize_or(n);
            surf.albedo = srgb(color);
            surf.rough = rough;
        }
        // Library detail (weave, grain, pores), triplanar in object space;
        // bakes already carry it in their textures.
        if it.textures.is_none()
            && let Some(mut detail) = crate::detail::Params::of(material)
        {
            detail.strength *= detail_share;
            detail.color *= detail_share;
            let local: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.pos) * w).sum();
            let local_n = (it.normal.inverse() * n).normalize_or(Vec3::Z);
            // Read the map at about one pixel's size (a pixel spans
            // roughly a thousandth of the distance in typical framings).
            let (detail_n, color, rough) = detail.apply(local, local_n, surf.albedo.powf(1.0 / 2.2), surf.rough, t * 0.001);
            n = (it.normal * detail_n).normalize_or(n);
            surf.albedo = srgb(color);
            surf.rough = rough;
        }
        if smart {
            let local: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.pos) * w).sum();
            let smooth: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.normal) * w).sum();
            let wear: Vec3 = vs.iter().zip(w).map(|(v, w)| Vec3::from(v.wear) * w).sum();
            let painted: Vec4 = vs.iter().zip(w).map(|(v, w)| Vec4::from(v.painted.map(|c| c as f32 / 255.0)) * w).sum();
            let at = crate::smart::At { p: local, n: smooth.normalize_or(Vec3::Y), edge: wear.x, cavity: wear.y, occlusion: wear.z, painted: painted.to_array(), footprint: t * 0.001 };
            let local_n = (it.normal.inverse() * n).normalize_or(Vec3::Z);
            let look = crate::smart::Look { color: surf.albedo.powf(1.0 / 2.2), rough: surf.rough, metal: surf.metal, coat: surf.coat, sheen: surf.sheen, emission: material.emission, n: local_n };
            let look = crate::smart::layers(material, &at, look);
            let look = if it.mesh.decals.is_empty() { look } else { crate::smart::decals(&it.mesh.decals, &at, look) };
            n = (it.normal * look.n).normalize_or(n);
            surf = Surface { albedo: srgb(look.color) * it.tint, rough: look.rough, metal: look.metal, coat: look.coat, sheen: look.sheen, skin: material.skin_specular * (1.0 - look.metal) };
            optics.emission = look.emission;
        }
        if material.subsurface > 0.0 && material.transmission <= 0.0
            && let Some((root, weight)) = self.scalp.at(p)
        {
            surf.albedo = surf.albedo.lerp(root, weight);
            surf.rough = surf.rough.max(0.65 * weight);
            surf.skin *= 1.0 - weight;
        }
        (p, ng, n, surf, sss, optics)
    }

    /// Where light that entered at `p` comes back out: a point of the same
    /// mesh about one diffusion distance away, found by casting a probe ray
    /// down through a random point of a disc around `p` onto the first
    /// surface of the same mesh. Returns the per-color weight of that
    /// distance, and the exit point with its normals (None if the probe
    /// found nothing).
    ///
    /// Thin parts (ears, nose wings, fingers) also let light through: a
    /// ray straight in finds the far side, and each color crosses that
    /// thickness with exp(-thickness / distance), red furthest, so a
    /// backlit ear glows red. That share leaves from the far side; the
    /// rest spreads on this side as above.
    fn probe(&self, p: Vec3, n: Vec3, sss: Sss, rng: &mut Rng) -> (Vec3, Option<(Vec3, Vec3, Vec3)>) {
        let d = sss.radius.max(Vec3::splat(1e-5));
        let skip = |h: &Hit| self.meshes.tris[h.tri as usize].item != sss.item || self.clear(h);
        let inward = self.meshes.bvh.hit(p, -n, 1e-4, THIN * d.max_element(), &skip);
        let through = inward.map_or(Vec3::ZERO, |h| Vec3::new((-h.t / d.x).exp(), (-h.t / d.y).exp(), (-h.t / d.z).exp()));
        let chance = if through.max_element() > 1e-3 { THROUGH } else { 0.0 };
        if let Some(h) = inward.filter(|_| rng.f() < chance) {
            // `surface` faces the normals back toward `p`: turn them out.
            let (p2, ng2, n2, _, _, _) = self.surface(p, -n, h.t, &h);
            return (through / chance, Some((p2, -ng2, -n2)));
        }
        let near = (Vec3::ONE - through) / (1.0 - chance);
        let c = ((rng.f() * 3.0) as usize).min(2);
        let u = rng.f().max(1e-7);
        let r = if rng.f() < 0.25 { -d[c] * u.ln() } else { -3.0 * d[c] * u.ln() };
        let rmax = 12.0 * d.max_element();
        let r = r.min(rmax);
        // Burley's profile, as a density over distance, per color.
        let radial = |k: usize| 0.25 * (-r / d[k]).exp() / d[k] + 0.75 * (-r / (3.0 * d[k])).exp() / (3.0 * d[k]);
        let pdf = (radial(0) + radial(1) + radial(2)) / 3.0;
        let weight = near * if pdf > 0.0 { Vec3::new(radial(0), radial(1), radial(2)) / pdf } else { Vec3::ONE };
        let f = Frame::new(n);
        let phi = std::f32::consts::TAU * rng.f();
        let o = p + (f.t * phi.cos() + f.b * phi.sin()) * r + n * rmax;
        let Some(h) = self.meshes.bvh.hit(o, -n, 0.0, 2.0 * rmax, &skip) else { return (weight, None) };
        // `surface` turns the normals toward the probe, which comes from
        // outside: they already face out.
        let (p2, ng2, n2, _, _, _) = self.surface(o, -n, h.t, &h);
        (weight, Some((p2, ng2, n2)))
    }
}

/// Studio environment: soft sky above, warm dark floor below.
fn environment(d: Vec3) -> Vec3 {
    let t = smoothstep(-0.3, 0.6, d.y);
    Vec3::new(0.06, 0.055, 0.05).lerp(Vec3::new(0.32, 0.34, 0.38), t)
}

fn imported_uv(it: &ItemData, uv: Vec2, slot: usize) -> Vec2 {
    let Some(m) = &it.mesh.imported else { return uv };
    let wrap = |v: f32, mode: u32| match mode {
        10497 => v.rem_euclid(1.0),
        33648 => 1.0 - (v.rem_euclid(2.0) - 1.0).abs(),
        _ => v.clamp(0.0, 1.0),
    };
    Vec2::new(wrap(uv.x, m.wraps[slot][0]), wrap(uv.y, m.wraps[slot][1]))
}

/// Bilinear texture read (0..1 per channel, no color conversion).
fn sample(data: &[u8], size: u32, uv: Vec2) -> [f32; 4] {
    let s = size as f32;
    let x = (uv.x * s - 0.5).clamp(0.0, s - 1.0);
    let y = (uv.y * s - 0.5).clamp(0.0, s - 1.0);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(size as usize - 1), (y0 + 1).min(size as usize - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize, c: usize| data[(y * size as usize + x) * 4 + c] as f32 / 255.0;
    [0, 1, 2, 3].map(|c| {
        let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
        let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
        top * (1.0 - fy) + bottom * fy
    })
}

fn srgb(c: Vec3) -> Vec3 {
    c.clamp(Vec3::ZERO, Vec3::ONE).powf(2.2)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Small fast random numbers per pixel and sample.
struct Rng(u64);

impl Rng {
    fn new(a: u64, b: u64) -> Rng {
        Rng((a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)) | 1)
    }

    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn v2(&mut self) -> Vec2 {
        Vec2::new(self.f(), self.f())
    }
}

/// First-hit data for the denoiser.
#[derive(Clone, Copy, Default)]
struct Guide {
    albedo: Vec3,
    normal: Vec3,
    depth: f32,
}

fn mis(a: f32, b: f32) -> f32 {
    let scale = a.max(b).max(1e-20);
    let (a, b) = (a / scale, b / scale);
    a * a / (a * a + b * b).max(1e-20)
}

/// Light scattered toward the camera by fog along `o + d * [0, t]`:
/// single scattering from one light (or the environment), chosen by how
/// much it could give the segment. Local lights pick the scattering
/// distance equi-angularly (dense near the light: bright halos and light
/// shafts at low noise); the sun and sky pick it evenly.
fn inscatter(scene: &Scene, fog: &lighting::Fog, o: Vec3, d: Vec3, t: f32, rng: &mut Rng) -> Vec3 {
    let tmax = t.min(fog.distance);
    if tmax <= 1e-5 {
        return Vec3::ZERO;
    }
    let lights = &scene.look.lights;
    let env = scene.look.environment.irradiance();
    let weight = |i: usize| -> f32 {
        match lights[i].local() {
            None => match lights[i] {
                lighting::Light::Sun { power, .. } => power.dot(Vec3::new(0.2126, 0.7152, 0.0722)),
                _ => 0.0,
            },
            Some((c, r, lum)) => {
                let along = (c - o).dot(d).clamp(0.0, tmax);
                lum / (o + d * along).distance_squared(c).max(r * r).max(1e-4)
            }
        }
    };
    let total: f32 = (0..lights.len()).map(weight).sum::<f32>() + env;
    if total <= 0.0 {
        return Vec3::ZERO;
    }
    let mut x = rng.f() * total;
    let mut pick = None;
    for i in 0..lights.len() {
        let w = weight(i);
        if x < w {
            pick = Some((i, w));
            break;
        }
        x -= w;
    }
    let chance = pick.map_or(env, |(_, w)| w) / total;
    if chance <= 0.0 {
        return Vec3::ZERO;
    }
    // Where along the ray it scatters.
    let point = pick.and_then(|(i, _)| lights[i].local()).map(|(c, _, _)| c);
    let (s, pdf) = match point {
        Some(c) => {
            let delta = (c - o).dot(d);
            let h = (c - o).length_squared() - delta * delta;
            let h = h.max(1e-8).sqrt();
            let (a0, a1) = ((-delta / h).atan(), ((tmax - delta) / h).atan());
            let a = a0 + (a1 - a0) * rng.f();
            let s = (delta + h * a.tan()).clamp(0.0, tmax);
            (s, h / ((a1 - a0).max(1e-9) * (h * h + (s - delta).powi(2))))
        }
        None => (rng.f() * tmax, 1.0 / tmax),
    };
    let x = o + d * s;
    let scatter = fog.albedo * fog.density_at_point(x) * fog.transmittance(o, d, s) * crate::fire::transmittance(&scene.look.volumes, o, d, s);
    let (l, light, far, light_pdf) = match pick {
        Some((i, _)) => {
            let (l, power, far) = lights[i].sample(x, rng.v2());
            (l, power, far, 1.0)
        }
        None => {
            let (l, c, pdf) = scene.look.environment.sample(rng);
            (l, c, f32::MAX, pdf)
        }
    };
    if light_pdf <= 0.0 || light == Vec3::ZERO || scene.blocked(x, l, far) {
        return Vec3::ZERO;
    }
    scatter * light * (fog.phase(l.dot(d)) * crate::fire::transmittance(&scene.look.volumes, x, l, far) * fog.transmittance(x, l, far) / (light_pdf * pdf * chance))
}

/// Radiance along one camera ray, and the first hit's guide data.
fn radiance(scene: &Scene, mut o: Vec3, mut d: Vec3, bounces: u32, rng: &mut Rng) -> (Vec3, Guide) {
    let mut color = Vec3::ZERO;
    let mut through = Vec3::ONE;
    let mut guide = Guide { albedo: Vec3::ONE, normal: Vec3::ZERO, depth: 1e9 };
    let fog = scene.look.fog;
    // Light reaching a point through fog is dimmed on the way.
    let fogged = |p: Vec3, l: Vec3, far: f32| fog.map_or(1.0, |f| f.transmittance(p, l, far)) * crate::fire::transmittance(&scene.look.volumes, p, l, far);
    // Glass crossings do not use up bounces (light needs two to pass
    // through a glass and more for a bottle).
    let mut hops = 0;
    let mut hair_hops = 0;
    let mut previous_pdf = None;
    for step in 0..=bounces + MAX_GLASS_HOPS + MAX_HAIR_HOPS {
        let bounce = step - hops;
        if bounce > bounces {
            break;
        }
        let traced = scene.trace(o, d, previous_pdf.is_none());
        let far = traced.map_or(f32::MAX, |h| h.0);
        let before_medium = through;
        if !scene.look.volumes.is_empty() {
            let (emission, tr) = crate::fire::march_with_fog(&scene.look.volumes, o, d, far, |p, view| {
                let mut incident = scene.look.environment.at(Vec3::Y) * (1.0 / (4.0 * std::f32::consts::PI));
                let picks=scene.lights.pick(p,None,rng);
                for (i,w) in scene.lights.suns.iter().map(|&i|(i,1.0)).chain(picks.iter()) {
                    let (l, power, distance) = scene.look.lights[i as usize].sample(p, [0.5, 0.5].into());
                    if !scene.blocked(p, l, distance) {
                        incident += power * (w * crate::fire::phase(l.dot(view)) * crate::fire::transmittance(&scene.look.volumes, p, l, distance) * fog.map_or(1.0, |f| f.transmittance(p,l,distance)));
                    }
                }
                incident
            }, fog);
            color += clamp_indirect(through * emission, bounce);
            through *= tr;
        }
        if let Some(f) = &fog {
            let t = far;
            if bounce == 0 {
                color += before_medium * inscatter(scene, f, o, d, t, rng);
            }
            through *= f.transmittance(o, d, t);
        }
        let Some((t, hit)) = traced else {
            let backdrop = step == 0 && !scene.look.background;
            let c = if backdrop { environment(d) } else { scene.look.environment.at(d) };
            let weight = previous_pdf.map_or(1.0, |pdf| mis(pdf, scene.look.environment.pdf(d)));
            color += through * c * weight;
            if bounce == 0 {
                guide.albedo = c;
                if let Some(f) = fog { guide.albedo = Vec3::ONE; guide.normal = -d; guide.depth = f.distance; }
            }
            break;
        };
        // Strands and textured native cards share the same scattering;
        // card UV V supplies the fiber direction instead of a surface lobe.
        let hair = hit.and_then(|h| {
            if let Some(seg) = scene.hair_at(&h) { return Some((seg.tangent, seg.color)); }
            let tri = &scene.meshes.tris[h.tri as usize];
            let item = &scene.meshes.items[tri.item as usize];
            if item.textures.as_ref().is_some_and(|t| t.alpha) && Scene::material_of(item, tri).name == "hair" {
                let (_, _, _, surf, _, optics) = scene.surface(o, d, t, &h);
                return optics.hair.map(|tangent| (tangent, surf.albedo));
            }
            None
        });
        if let Some((tangent, pigment)) = hair {
            let p = o + d * t;
            if bounce == 0 {
                // The strand's direction stands in for the normal, so the
                // denoiser only blends strands running the same way.
                let along = if tangent.y < 0.0 { -tangent } else { tangent };
                guide = Guide { albedo: pigment.max(Vec3::splat(0.02)), normal: along, depth: t };
            }
            let v = -d;
            let lift = 2e-4 * (1.0 + p.abs().max_element());
            let picks = scene.lights.pick(p, None, rng);
            for (i, w) in scene.lights.suns.iter().map(|&i| (i, 1.0)).chain(picks.iter()) {
                let (l, power, far) = scene.look.lights[i as usize].sample(p, rng.v2());
                // Leave along the light, clear of this fiber's ribbon.
                let from = p + l * lift;
                if scene.blocked(from, l, far - lift) {
                    continue;
                }
                color += clamp_indirect(through * shade::hair(tangent, pigment, v, l) * power * (w * fogged(p, l, far)), bounce);
            }
            let (l, emission, pdf) = scene.look.environment.sample(rng);
            let from = p + l * lift;
            if pdf > 0.0 && !scene.occluded(from, l) {
                let weight = if bounce == bounces { 1.0 } else { mis(pdf, shade::hair_pdf(tangent, pigment, v, l)) };
                color += clamp_indirect(through * shade::hair(tangent, pigment, v, l) * emission * (weight / pdf * fogged(p, l, f32::MAX)), bounce);
            }
            if bounce == bounces { break; }
            // Light scatters through hair in every direction; follow the
            // fiber's lobes so paths deep in a groom keep even weights.
            let Some((l, pdf)) = shade::hair_sample(tangent, pigment, v, &mut || rng.f()) else { break };
            through *= shade::hair(tangent, pigment, v, l) / pdf;
            if bounce >= 2 {
                let keep = through.max_element().clamp(0.05, 0.95);
                if rng.f() > keep {
                    break;
                }
                through /= keep;
            }
            o = p + l * lift;
            d = l;
            previous_pdf = Some(pdf);
            if bounce > 0 && hair_hops < MAX_HAIR_HOPS {
                hair_hops += 1;
                hops += 1;
            }
            continue;
        }
        let (p, ng, n, mut surf, sss, optics) = match hit {
            Some(h) => scene.surface(o, d, t, &h),
            None => {
                let p = o + d * t;
                (p, Vec3::Y, Vec3::Y, Surface { albedo: Vec3::splat(0.42), rough: 0.7, metal: 0.0, coat: 0.0, sheen: 0.0, skin: 0.0 }, None, Optics::default())
            }
        };
        if bounce == 0 {
            guide = Guide { albedo: surf.albedo.max(Vec3::splat(0.02)), normal: n, depth: t };
        }
        // Emission: the surface's own light, in its color.
        color += clamp_indirect(through * optics.emitted, bounce);
        if optics.emission > 0.0 {
            color += clamp_indirect(through * surf.albedo * optics.emission, bounce);
        }
        // A flame is all glow: its light reaches the scene as its object's light.
        if optics.flame {
            break;
        }
        // Glass: the ray reflects or passes through, bending (Fresnel
        // decides which). Roughness frosts it.
        if optics.transmission > 0.0 && rng.f() < optics.transmission {
            hops += 1;
            previous_pdf = None;
            let eta = if optics.front { 1.0 / optics.ior } else { optics.ior };
            let m = (n + shade::sphere(rng.v2()) * surf.rough * 0.5).normalize_or(n);
            let cosi = (-d).dot(m).clamp(1e-4, 1.0);
            let sin2t = eta * eta * (1.0 - cosi * cosi);
            let lift = ng * (1e-4 + 1e-4 * p.abs().max_element());
            let fresnel = if sin2t >= 1.0 {
                1.0
            } else {
                let cost = (1.0 - sin2t).sqrt();
                let rs = (eta * cosi - cost) / (eta * cosi + cost);
                let rp = (cosi - eta * cost) / (cosi + eta * cost);
                0.5 * (rs * rs + rp * rp)
            };
            if rng.f() < fresnel {
                d = (d + m * 2.0 * cosi).normalize_or(-d);
                o = p + lift;
            } else {
                let cost = (1.0 - sin2t).sqrt();
                d = (d * eta + m * (eta * cosi - cost)).normalize_or(d);
                o = p - lift;
                // Colored glass tints by hue only, so clear glass of any
                // gray stays clear.
                through *= (surf.albedo / surf.albedo.max_element().max(1e-4)).powf(0.5);
            }
            continue;
        }
        let frame = Frame::new(n);
        let v = frame.to_local(-d);
        if v.z <= 0.0 {
            break;
        }
        let lift = ng * (1e-4 + 1e-4 * p.abs().max_element());
        // Subsurface: half the time follow light into the surface (and
        // weigh it double), half the time the reflection without it.
        if let Some(sss) = sss {
            if rng.f() < 0.5 {
                // A probe that finds nothing (thin edges) lets the light
                // leave where it entered, keeping its color weight, so the
                // far-reaching red is not lost.
                let (weight, exit) = scene.probe(p, n, sss, rng);
                let (p2, ng2, n2) = exit.unwrap_or((p, ng, n));
                let enter = 1.0 - 0.04 - 0.96 * (1.0 - v.z).powi(5);
                through *= surf.albedo * weight * (sss.amount * enter * 2.0);
                let lift2 = ng2 * (1e-4 + 1e-4 * p2.abs().max_element());
                // Light leaves the exit point evenly in all directions.
                let picks = scene.lights.pick(p2, Some(ng2), rng);
                for (i, w) in scene.lights.suns.iter().map(|&i| (i, 1.0)).chain(picks.iter()) {
                    let (l, power, far) = scene.look.lights[i as usize].sample(p2, rng.v2());
                    let cos = l.dot(n2);
                    if cos <= 0.0 || l.dot(ng2) <= 0.0 || scene.blocked(p2 + lift2, l, far - lift2.length()) {
                        continue;
                    }
                    color += clamp_indirect(through * power * (cos * std::f32::consts::FRAC_1_PI * w * fogged(p2, l, far)), bounce);
                }
                let (l, emission, pdf) = scene.look.environment.sample(rng);
                let cos = l.dot(n2).max(0.0);
                if pdf > 0.0 && l.dot(ng2) > 0.0 && !scene.occluded(p2 + lift2, l) {
                    let bsdf_pdf = cos * std::f32::consts::FRAC_1_PI;
                    let weight = if bounce == bounces { 1.0 } else { mis(pdf, bsdf_pdf) };
                    color += clamp_indirect(through * emission * (bsdf_pdf * weight / pdf * fogged(p2, l, f32::MAX)), bounce);
                }
                if bounce == bounces { break; }
                d = Frame::new(n2).to_world(shade::cosine(rng.v2()));
                o = p2 + lift2;
                previous_pdf = Some(d.dot(n2).max(0.0) * std::f32::consts::FRAC_1_PI);
                continue;
            }
            through *= 2.0;
            surf.albedo *= 1.0 - sss.amount;
        }
        // Direct light: every sun, and local lights (all, or picked).
        let picks = scene.lights.pick(p, Some(ng), rng);
        for (i, w) in scene.lights.suns.iter().map(|&i| (i, 1.0)).chain(picks.iter()) {
            let (l, power, far) = scene.look.lights[i as usize].sample(p, rng.v2());
            if l.dot(ng) <= 0.0 {
                continue;
            }
            let ll = frame.to_local(l);
            let f = surf.eval(v, ll);
            if f == Vec3::ZERO || scene.blocked(p + lift, l, far - lift.length()) {
                continue;
            }
            // Uniform over the disc: radiance x solid angle = `power`.
            color += clamp_indirect(through * f * power * (w * fogged(p, l, far)), bounce);
        }
        let (l, emission, pdf) = scene.look.environment.sample(rng);
        if pdf > 0.0 && l.dot(ng) > 0.0 && !scene.occluded(p + lift, l) {
            let ll = frame.to_local(l);
            let weight = if bounce == bounces { 1.0 } else { mis(pdf, surf.pdf(v, ll)) };
            color += clamp_indirect(through * surf.eval(v, ll) * emission * (weight / pdf * fogged(p, l, f32::MAX)), bounce);
        }
        if bounce == bounces { break; }
        // Continue the path.
        let Some(l) = surf.sample(v, rng.v2(), rng.f()) else { break };
        let pdf = surf.pdf(v, l);
        if pdf <= 1e-6 {
            break;
        }
        through *= surf.eval(v, l) / pdf;
        // Russian roulette: stop dim paths early, without bias.
        if bounce >= 2 {
            let keep = through.max_element().clamp(0.05, 0.95);
            if rng.f() > keep {
                break;
            }
            through /= keep;
        }
        d = frame.to_world(l);
        if d.dot(ng) <= 0.0 { break; }
        previous_pdf = Some(pdf);
        o = p + if d.dot(ng) > 0.0 { lift } else { -lift };
    }
    (color, guide)
}

#[cfg(test)]
pub fn render(items: Vec<Item>, fibers: &[Fiber], cam: &Camera, s: Settings, progress: &crate::progress::Progress) -> Image {
    let start = Instant::now();
    progress.stage("building the ray tree", 0.0);
    let meshes = Meshes::new(items);
    let s = Settings { time_limit: s.time_limit.saturating_sub(start.elapsed()), ..s };
    render_views(&meshes, fibers, std::slice::from_ref(cam), s, progress).pop().expect("one view")
}

/// Renders several views of the same meshes. They take turns adding
/// samples, so each gets a fair share of `s.time_limit` (the whole call's
/// budget), and stop together when it runs out or `progress` is cancelled.
#[cfg(test)]
pub fn render_views(meshes: &Meshes, fibers: &[Fiber], cams: &[Camera], s: Settings, progress: &crate::progress::Progress) -> Vec<Image> {
    // Tests check the CPU tracer unless they ask for the GPU.
    let mut look = Look::default();
    look.device = look::Device::Cpu;
    render_views_with_look(meshes, fibers, cams, s, Arc::new(look), progress)
}

/// Shares lighting and camera optics across the views.
pub fn render_views_with_look(meshes: &Meshes, fibers: &[Fiber], cams: &[Camera], s: Settings, look: Arc<Look>, progress: &crate::progress::Progress) -> Vec<Image> {
    let start = Instant::now();
    progress.stage("building the hair", 0.0);
    let mut views: Vec<View> = cams.iter().map(|cam| View::new(Scene::new(meshes, fibers, cam.eye, s.floor, look.clone()), cam, s)).collect();
    progress.stage("tracing light", 0.02);
    // The GPU when it can take the render; otherwise every core.
    match gpu::render(&mut views, meshes, s, start, progress) {
        Ok(_) => {
            progress.stage("removing grain", 0.96);
            return views.into_iter().map(|v| v.finish(s)).collect();
        }
        Err(e) if look.device == look::Device::Gpu => eprintln!("film: GPU asked for but not used: {e}"),
        Err(_) => {}
    }
    let total = s.samples as usize * views.len();
    // Passes of a few samples each, so a time limit stops cleanly.
    loop {
        let mut any = false;
        for v in views.iter_mut().filter(|v| v.done < s.samples) {
            let t = Instant::now();
            v.pass(4.min(s.samples - v.done), s.bounces);
            v.seconds += t.elapsed().as_secs_f32();
            any = true;
        }
        // Whichever ends first: the samples or the time limit.
        let done: usize = views.iter().map(|v| v.done as usize).sum();
        let by_time = start.elapsed().as_secs_f32() / s.time_limit.as_secs_f32().max(1e-3);
        progress.set(0.02 + 0.93 * (done as f32 / total.max(1) as f32).max(by_time).min(1.0));
        if !any || start.elapsed() > s.time_limit || progress.cancelled() {
            break;
        }
    }
    progress.stage("removing grain", 0.96);
    views.into_iter().map(|v| v.finish(s)).collect()
}

/// One view being rendered: its scene (with hair facing its camera), and
/// the light summed so far.
struct View<'a> {
    scene: Scene<'a>,
    w: usize,
    h: usize,
    eye: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    half: f32,
    ortho: Option<f32>,
    lens: look::Lens,
    sum: Vec<Vec3>,
    guides: Vec<Guide>,
    done: u32,
    seconds: f32,
}

impl<'a> View<'a> {
    fn new(scene: Scene<'a>, cam: &Camera, s: Settings) -> View<'a> {
        let (w, h) = (s.width.max(8), s.height.max(8));
        let forward = (cam.target - cam.eye).normalize();
        let right = forward.cross(cam.up).normalize_or(forward.any_orthonormal_vector());
        let up = right.cross(forward);
        let lens = scene.look.camera.lens(cam);
        View { scene, lens, w, h, eye: cam.eye, forward, right, up, half: (cam.fov_deg.to_radians() * 0.5).tan(), ortho: cam.ortho, sum: vec![Vec3::ZERO; w * h], guides: vec![Guide::default(); w * h], done: 0, seconds: 0.0 }
    }

    fn ray(&self, x: f32, y: f32, lens_sample: Vec2) -> (Vec3, Vec3) {
        let sx = (x / self.w as f32 * 2.0 - 1.0) * (self.w as f32 / self.h as f32);
        let sy = 1.0 - y / self.h as f32 * 2.0;
        match self.ortho {
            Some(hh) => (self.eye + self.right * sx * hh + self.up * sy * hh, self.forward),
            None => self.lens.ray(self.eye, (self.forward + self.right * sx * self.half + self.up * sy * self.half).normalize(), self.forward, self.right, self.up, lens_sample),
        }
    }

    /// Adds `pass` samples to every pixel, on all cores.
    fn pass(&mut self, pass: u32, bounces: u32) {
        let (w, h, done) = (self.w, self.h, self.done);
        let next = AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
        let me = &*self;
        let rows: Vec<(usize, Vec<Vec3>, Vec<Guide>)> = std::thread::scope(|sc| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    sc.spawn(|| {
                        let mut out = Vec::new();
                        loop {
                            let y = next.fetch_add(1, Ordering::Relaxed);
                            if y >= h {
                                break;
                            }
                            let mut row = vec![Vec3::ZERO; w];
                            let mut g = vec![Guide::default(); w];
                            for x in 0..w {
                                for k in 0..pass {
                                    let mut rng = Rng::new((y * w + x) as u64 + 1, (done + k) as u64 + 7);
                                    let j = rng.v2();
                                    let (o, d) = me.ray(x as f32 + j.x, y as f32 + j.y, rng.v2());
                                    let (c, gd) = radiance(&me.scene, o, d, bounces, &mut rng);
                                    // Keep HDR highlights for bloom and the display shoulder.
                                    let c = if c.is_finite() { c } else { Vec3::ZERO };
                                    row[x] += c;
                                    g[x].albedo += gd.albedo;
                                    g[x].normal += gd.normal;
                                    g[x].depth += gd.depth.min(1e4);
                                }
                            }
                            out.push((y, row, g));
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().flat_map(|t| t.join().expect("render thread")).collect()
        });
        for (y, row, g) in rows {
            for x in 0..w {
                self.sum[y * w + x] += row[x];
                self.guides[y * w + x].albedo += g[x].albedo;
                self.guides[y * w + x].normal += g[x].normal;
                self.guides[y * w + x].depth += g[x].depth;
            }
        }
        self.done += pass;
    }

    fn finish(self, s: Settings) -> Image {
        let done = self.done.max(1);
        let mut color: Vec<Vec3> = self.sum.iter().map(|c| *c / done as f32).collect();
        if s.denoise {
            color = denoise::filter(&color, &self.guides.iter().map(|g| (g.albedo / done as f32, g.normal.normalize_or_zero(), g.depth / done as f32)).collect::<Vec<_>>(), self.w, self.h, self.done);
        }
        let pixels = self.scene.look.finish(color.clone(), self.w, self.h, s.exposure);
        Image { width: self.w, height: self.h, pixels, linear: color, samples: self.done, seconds: self.seconds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_rays_respect_the_floor_and_light_distance() {
        let meshes = Meshes::new(Vec::new());
        let mut scene = Scene::new(&meshes, &[], Vec3::Z, false, Arc::new(Look::default()));
        scene.floor_y = Some(0.0);
        assert!(scene.occluded(Vec3::Y, Vec3::NEG_Y));
        assert!(!scene.occluded(Vec3::Y, Vec3::Y));
        assert!(!scene.blocked(Vec3::Y, Vec3::NEG_Y, 0.5));
        assert!(scene.blocked(Vec3::Y, Vec3::NEG_Y, 2.0));
    }

    #[test]
    fn heterogeneous_god_rays_need_unoccluded_light() {
        let meshes=Meshes::new(Vec::new());
        let fog=lighting::Fog {density:0.2,noise:0.9,noise_scale:0.7,distance:8.0,falloff:0.0,..Default::default()};
        let estimate=|light: Option<Vec3>| {
            let mut look=Look::default();
            look.environment=Arc::new(lighting::Environment::map(1,1,vec![Vec3::ZERO],0.0));
            look.lights=light.into_iter().map(|direction| lighting::Light::Sun {direction,size:0.0,power:Vec3::ONE}).collect();
            look.fog=Some(fog);
            let mut scene=Scene::new(&meshes,&[],Vec3::Y,false,Arc::new(look)); scene.floor_y=Some(0.0);
            let mut rng=Rng::new(91,17);
            (0..1024).map(|_| inscatter(&scene,&fog,Vec3::Y,Vec3::X,5.0,&mut rng)).sum::<Vec3>()/1024.0
        };
        assert_eq!(estimate(None),Vec3::ZERO);
        assert_eq!(estimate(Some(Vec3::NEG_Y)),Vec3::ZERO);
        assert!(estimate(Some(Vec3::Y)).min_element()>0.001);
        let p=Vec3::new(-0.7,1.0,2.0); let d=Vec3::new(0.8,0.1,-0.3).normalize();
        let numeric=(0..10000).map(|i|fog.density_at_point(p+d*(i as f32+0.5)*0.0005)*0.0005).sum::<f32>();
        assert!((fog.depth(p,d,5.0)-numeric).abs()<0.003);
        assert!(fog.transmittance(p,d,5.0)<fog.transmittance(p,d,1.0));
    }

    #[test]
    fn white_sphere_under_uniform_radiance_conserves_energy() {
        let m = Material { roughness: 0.65, ..Material::default() };
        let meshes = Meshes::new(vec![Item { mesh: ball([1.0; 3]), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }]);
        let mut look = Look::default();
        look.environment = Arc::new(lighting::Environment::map(8, 4, vec![Vec3::splat(0.7); 32], 0.0));
        look.lights.clear();
        look.background = true;
        let scene = Scene::new(&meshes, &[], Vec3::new(0.0, 0.5, 4.0), false, Arc::new(look));
        let mut sum = Vec3::ZERO;
        for i in 0..20_000 {
            sum += radiance(&scene, Vec3::new(0.0, 0.5, 4.0), Vec3::NEG_Z, 3, &mut Rng::new(i + 1, 17)).0;
        }
        let mean = sum / 20_000.0;
        assert!((mean - Vec3::splat(0.7)).abs().max_element() < 0.035, "radiance {mean}");
    }

    #[test]
    fn fog_never_makes_light_and_scatters_lights_correctly() {
        let meshes = Meshes::new(Vec::new());
        // Even light all around: fog dims and scatters, but adds no light;
        // fog that also absorbs gives less.
        let mean = |albedo: f32, falloff: f32| {
            let mut look = Look::default();
            look.environment = Arc::new(lighting::Environment::map(8, 4, vec![Vec3::splat(0.7); 32], 0.0));
            look.lights.clear();
            look.background = true;
            look.fog = Some(lighting::Fog { density: 0.05, albedo: Vec3::splat(albedo), height: 0.0, falloff, anisotropy: 0.4, distance: 12.0, ..Default::default() });
            let scene = Scene::new(&meshes, &[], Vec3::ZERO, false, Arc::new(look));
            let mut rng = Rng::new(3, 3);
            let n = 40_000;
            (0..n).map(|i| {
                let d = shade::sphere(rng.v2());
                radiance(&scene, Vec3::ZERO, d, 3, &mut Rng::new(i + 1, 5)).0
            }).sum::<Vec3>() / n as f32
        };
        let (white, layered, dark) = (mean(1.0, 0.0), mean(1.0, 1.0), mean(0.5, 0.0));
        eprintln!("white {white} layered {layered} dark {dark}");
        assert!(white.max_element() < 0.7 * 1.02 && layered.max_element() < 0.7 * 1.02, "fog made light: {white} {layered}");
        assert!(white.x > 0.4 && layered.x > white.x && dark.x < white.x * 0.95, "{white} {layered} {dark}");
        // One point light in the dark: the in-scatter estimate equals the
        // single-scattering integral along the ray.
        let fog = lighting::Fog { density: 0.3, albedo: Vec3::splat(0.8), height: 0.5, falloff: 1.2, anisotropy: 0.5, distance: 50.0, ..Default::default() };
        let c = Vec3::new(1.5, 1.0, -0.7);
        let mut look = Look::default();
        look.environment = Arc::new(lighting::Environment::map(2, 2, vec![Vec3::ZERO; 4], 0.0));
        look.lights = vec![lighting::Light::Point { position: c, power: Vec3::ONE }];
        look.fog = Some(fog);
        let scene = Scene::new(&meshes, &[], Vec3::ZERO, false, Arc::new(look));
        let (o, d, t) = (Vec3::new(0.0, 0.2, 0.0), Vec3::new(1.0, 0.1, -0.3).normalize(), 6.0);
        let mut rng = Rng::new(8, 1);
        let n = 200_000;
        let estimate = (0..n).map(|_| inscatter(&scene, &fog, o, d, t, &mut rng).x).sum::<f32>() / n as f32;
        let steps = 100_000;
        let exact: f32 = (0..steps).map(|i| {
            let s = t * (i as f32 + 0.5) / steps as f32;
            let x = o + d * s;
            let to = c - x;
            let l = to.normalize();
            0.8 * fog.density_at_point(x) * fog.transmittance(o, d, s) * fog.phase(l.dot(d)) / to.length_squared() * fog.transmittance(x, l, to.length()) * t / steps as f32
        }).sum();
        assert!((estimate - exact).abs() < exact * 0.02, "in-scatter {estimate} against {exact}");
    }

    #[test]
    fn thirty_lights_cost_about_as_much_as_one() {
        // A floor lit by 1 or 30 candles: picking keeps the shadow rays
        // per hit the same, so the time stays about the same.
        let floor = Mesh::build(crate::scene::Shape::Plane);
        let items = || vec![Item { mesh: Arc::new(floor.clone()), world: Mat4::from_scale(Vec3::splat(8.0)), tint: [0.6; 3], surface: Material::default().surface(), material: Material::default() }];
        let time = |count: usize| {
            let mut look = Look::default();
            look.environment = Arc::new(lighting::Environment::map(2, 2, vec![Vec3::ZERO; 4], 0.0));
            look.lights = (0..count).map(|i| lighting::Light::Sphere { position: Vec3::new((i % 6) as f32 - 2.5, 0.3, (i / 6) as f32 - 2.0), radius: 0.02, power: Vec3::splat(0.3) }).collect();
            look.device = look::Device::Cpu;
            let cam = Camera { eye: Vec3::new(0.0, 6.0, 6.0), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 40.0, ortho: None };
            let s = Settings { width: 96, height: 96, samples: 24, bounces: 3, time_limit: Duration::from_secs(60), floor: false, exposure: 1.0, denoise: false };
            let meshes = Meshes::new(items());
            let t = Instant::now();
            let img = render_views_with_look(&meshes, &[], &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
            let lit = img.pixels.iter().map(|p| p[0] as f32).sum::<f32>() / img.pixels.len() as f32;
            (t.elapsed().as_secs_f32(), lit)
        };
        let _ = time(1); // warm up the threads
        let (one, lit_one) = time(1);
        let (many, lit_many) = time(30);
        eprintln!("1 light {one:.3}s, 30 lights {many:.3}s");
        assert!(many < one * 3.0, "30 lights took {many:.2}s against {one:.2}s for one");
        assert!(lit_many > lit_one, "more candles light more");
    }

    #[test]
    fn native_hair_cards_scatter_along_uv_v() {
        let hair = crate::hair::Hair { guides: vec![crate::hair::Guide { points: vec![Vec3::Y, Vec3::ZERO], width: None }], interpolate: false, ..Default::default() };
        let m = Material::preset("hair").unwrap();
        let meshes = Meshes::new(vec![Item { mesh: Arc::new(hair.build_mesh()), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }]);
        let scene = Scene::new(&meshes, &[], Vec3::Z * 2.0, false, Arc::new(Look::default()));
        let h = Hit { t: 1.0, tri: 0, u: 0.3, v: 0.3 };
        let (_, _, _, _, _, optics) = scene.surface(Vec3::Z, Vec3::NEG_Z, 1.0, &h);
        assert!(optics.hair.unwrap().dot(Vec3::NEG_Y) > 0.99);
        let (_, _, _, _, _, back) = scene.surface(Vec3::NEG_Z, Vec3::Z, 1.0, &h);
        assert!(back.hair.unwrap().dot(Vec3::NEG_Y) > 0.99);
    }

    #[test]
    fn root_tint_is_local_and_ignores_facial_hair() {
        let fiber = |length| Fiber { points: vec![Vec3::ZERO, Vec3::Y * length], root_width: 0.00008,
            tip_width: 0.00001, root_color: Vec3::splat(0.2), tip_color: Vec3::splat(0.3) };
        assert!(ScalpTint::new(&[fiber(0.007)]).at(Vec3::ZERO).is_none());
        assert!(ScalpTint::new(&[fiber(0.035)]).at(Vec3::ZERO).is_some());
        let tint = ScalpTint::new(&(0..30).map(|_| fiber(0.3)).collect::<Vec<_>>());
        let (_, weight) = tint.at(Vec3::ZERO).unwrap();
        assert!(weight > 0.6 && weight < 1.0);
        assert!(tint.at(Vec3::X * 0.003).unwrap().1 < weight);
        assert!(tint.at(Vec3::X * 0.02).is_none());
    }

    /// A sphere that takes its material from the item (like a primitive).
    fn ball(color: [f32; 3]) -> Arc<Mesh> {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": color, "blend": 0.0 }], "resolution": 32
        }))
        .unwrap();
        let mut mesh = clay.build_mesh();
        mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
        Arc::new(mesh)
    }

    /// Mean brightness (0..255) of the middle of a render of `mesh`.
    fn middle(mesh: &Arc<Mesh>, m: Material) -> Vec3 {
        let cam = Camera { eye: Vec3::new(0.0, 0.5, 4.0), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 20.0, ortho: None };
        let s = Settings { width: 32, height: 32, samples: 32, bounces: 3, time_limit: Duration::from_secs(30), floor: true, exposure: 1.0, denoise: false };
        let items = vec![Item { mesh: mesh.clone(), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }];
        let img = render(items, &[], &cam, s, &Default::default());
        let px: Vec<Vec3> = (12..20).flat_map(|y| (12..20).map(move |x| (x, y))).map(|(x, y)| Vec3::from(img.pixels[y * 32 + x].map(f32::from))).collect();
        px.iter().sum::<Vec3>() / px.len() as f32
    }

    #[test]
    fn emission_glows_in_the_surface_color() {
        let red = ball([0.8, 0.1, 0.1]);
        let plain = middle(&red, Material::default());
        let glow = middle(&red, Material::preset("glow").unwrap());
        assert!(glow.x > plain.x + 40.0, "plain {plain} glow {glow}");
        assert!(glow.x > glow.y * 1.5, "glows red: {glow}");
    }

    #[test]
    fn imported_emission_and_alpha_use_file_factors() {
        let mut mesh = Mesh::build(crate::scene::Shape::Cube);
        for v in &mut mesh.vertices { v.color = [0., 1., 0.]; }
        mesh.imported = Some(Arc::new(crate::mesh::ImportedMaterial { emissive: [3., 0., 0.], size: 1, emissive_texture: Some(vec![255, 255, 255, 255]), alpha_cutoff: Some(0.4), base_alpha: 0.5, ..Default::default() }));
        mesh.textures = Some(Arc::new(Textures { size: 1, color: vec![255, 255, 255, 128], normal: vec![128, 128, 255, 255], orm: vec![255, 255, 0, 255], alpha: true }));
        let meshes = Meshes::new(vec![Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.; 3], surface: Material::default().surface(), material: Material::default() }]);
        let scene = Scene::new(&meshes, &[], Vec3::Z * 3., false, Arc::new(Look::default()));
        let hit = Hit { tri: 0, t: 2.5, u: 0.25, v: 0.25 };
        assert!(scene.clear(&hit)); // 0.5 texture alpha * 0.5 factor is below 0.4.
        let (_, _, _, _, _, optics) = scene.surface(Vec3::Z * 3., Vec3::NEG_Z, 2.5, &hit);
        assert_eq!(optics.emitted, Vec3::X * 3.); // Emission stays red on a green surface.
    }

    #[test]
    fn glass_lets_the_background_through() {
        // A dark gray ball: opaque it is dark; as glass (gray = clear)
        // the bright studio shows through it.
        let gray = ball([0.1, 0.1, 0.1]);
        let opaque = middle(&gray, Material::default());
        let glass = middle(&gray, Material::preset("glass").unwrap());
        assert!(glass.x > opaque.x + 30.0, "opaque {opaque} glass {glass}");
        // Water bends less than crystal, so the two differ.
        let water = middle(&gray, Material::preset("water").unwrap());
        let crystal = middle(&gray, Material::preset("crystal").unwrap());
        assert!((water - crystal).length() > 2.0, "water {water} crystal {crystal}");
    }

    #[test]
    fn subsurface_softens_without_changing_overall_brightness() {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": [0.9, 0.7, 0.6] }], "resolution": 32
        }))
        .unwrap();
        let mesh = Arc::new(clay.build_mesh());
        let cam = Camera { eye: Vec3::new(0.0, 0.5, 4.0), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 20.0, ortho: None };
        let s = Settings { width: 48, height: 48, samples: 64, bounces: 2, time_limit: Duration::from_secs(30), floor: false, exposure: 1.0, denoise: false };
        let mean = |subsurface: f32| {
            // A large scatter distance so the effect is strong.
            let m = Material { subsurface, scatter: [0.05, 0.03, 0.02], ..Material::default() };
            let items = vec![Item { mesh: mesh.clone(), world: Mat4::IDENTITY, tint: [1.0; 3], surface: [-1.0, 0.0, 0.0, 0.0], material: m }];
            let img = render(items, &[], &cam, s, &Default::default());
            img.pixels[16 * 48..32 * 48].iter().map(|p| p[0] as f32 + p[1] as f32 + p[2] as f32).sum::<f32>() / (16.0 * 48.0 * 3.0)
        };
        let (plain, sss) = (mean(0.0), mean(1.0));
        assert!((sss - plain).abs() < plain * 0.12, "plain {plain} sss {sss}");
    }

    #[test]
    fn a_lit_sphere_on_a_floor_renders_with_a_shadow() {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": [0.8, 0.3, 0.2] }], "resolution": 32
        }))
        .unwrap();
        let mesh = Arc::new(clay.build_mesh());
        let items = vec![Item { mesh, world: Mat4::IDENTITY, tint: [1.0; 3], surface: [0.5, 0.0, 0.0, 0.0], material: Material::default() }];
        let cam = Camera { eye: Vec3::new(0.0, 1.5, 4.0), target: Vec3::new(0.0, 0.4, 0.0), up: Vec3::Y, fov_deg: 30.0, ortho: None };
        let s = Settings { width: 64, height: 48, samples: 16, bounces: 3, time_limit: Duration::from_secs(20), floor: true, exposure: 1.0, denoise: true };
        let img = render(items, &[], &cam, s, &Default::default());
        assert_eq!(img.pixels.len(), 64 * 48);
        let px = |x: usize, y: usize| img.pixels[y * 64 + x];
        // The sphere's middle is reddish; the background is not black.
        let mid = px(32, 22);
        assert!(mid[0] > mid[2] + 20, "{mid:?}");
        assert!(px(2, 2).iter().all(|&c| c > 10));
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    /// Saves a 512 px face close-up (64 samples by default) of a scene:
    /// `SCENE=path FACE_OUTPUT=dir cargo test --release film_timing -- --ignored --nocapture`.
    /// FACE_BASELINE restores old skin shading; FACE_UNPAINTED drops paint;
    /// FACE_SAMPLES and FACE_DENOISE control quality. HAIR_CARDS renders
    /// export cards; FACE_TARGET, FACE_YAW and FACE_PITCH set the camera.
    /// HAIR_CHECK saves face, crown and cards in one run, reusing scene meshes.
    #[test]
    #[ignore]
    fn film_timing() {
        let path = std::env::var("SCENE").expect("SCENE");
        let mut scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        if std::env::var("FACE_BASELINE").is_ok() {
            scene.materials.push(Material { name: "skin".into(), roughness: 0.42, sheen: 0.1, subsurface: 1.0, ..Material::default() });
        }
        if std::env::var("FACE_UNPAINTED").is_ok() {
            scene.objects.iter_mut().filter_map(|o| o.clay.as_mut()).for_each(|c| c.paint = None);
        }
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.show_fine = true;
        meshes.sync(&scene);
        let eval = scene.evaluate();
        let items = |cards: bool| -> Vec<Item> {
            scene
                .objects
                .iter()
                .filter(|o| o.shape.is_mesh())
                .filter_map(|o| {
                    let &(world, true) = eval.get(&o.id)? else { return None };
                    Some(Item { mesh: meshes.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
                })
                .filter(|i| cards || i.mesh.textures.as_ref().is_none_or(|t| !t.alpha))
                .collect()
        };
        let mut fibers = Vec::new();
        for o in scene.objects.iter().filter(|o| o.shape == crate::scene::Shape::Hair && std::env::var("HAIR_CARDS").is_err()) {
            let (Some(&(world, true)), Some(hair)) = (eval.get(&o.id), o.hair.as_ref()) else { continue };
            for s in hair.strands() {
                fibers.push(Fiber { points: s.points.iter().map(|p| world.transform_point3(*p)).collect(), root_width: s.root_width, tip_width: s.tip_width, root_color: s.root_color, tip_color: s.tip_color });
            }
        }
        let target = std::env::var("FACE_TARGET").ok().map(|v| {
            let v: Vec<f32> = v.split(',').map(|s| s.parse().unwrap()).collect();
            Vec3::new(v[0], v[1], v[2])
        }).unwrap_or(Vec3::new(0.0, 1.7, 0.06));
        let angle = |key| std::env::var(key).ok().map(|s| s.parse::<f32>().unwrap().to_radians()).unwrap_or(0.0);
        let (yaw, pitch) = (angle("FACE_YAW"), angle("FACE_PITCH"));
        let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
        let face = Camera { eye: target + dir * 0.668, target, up: Vec3::Y, fov_deg: 30.0, ortho: None };
        let s = Settings { width: 512, height: 512, samples: std::env::var("FACE_SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(64), bounces: 4, time_limit: Duration::from_secs(600), floor: true, exposure: 1.0, denoise: std::env::var("FACE_DENOISE").is_ok() };
        let camera = |yaw: f32, pitch: f32| {
            let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
            let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
            Camera { eye: target + dir * 0.668, target, up: Vec3::Y, fov_deg: 30.0, ortho: None }
        };
        let check = std::env::var("HAIR_CHECK").is_ok();
        let views = if check { vec![("face", face, false), ("crown", camera(180.0, 55.0), false), ("cards", camera(25.0, 10.0), true)] }
            else { vec![("face", face, std::env::var("HAIR_CARDS").is_ok())] };
        for (name, cam, cards) in views {
            let t = Instant::now();
            let strands = if cards { &[][..] } else { &fibers[..] };
            let img = render(items(cards), strands, &cam, s, &Default::default());
            if let Ok(dir) = std::env::var("FACE_OUTPUT") {
                let dir = if check { format!("{dir}/{name}") } else { dir };
                std::fs::create_dir_all(&dir).unwrap();
                image::save_buffer(format!("{dir}/face.png"), &img.pixels.iter().flatten().copied().collect::<Vec<_>>(), img.width as u32, img.height as u32, image::ColorType::Rgb8).unwrap();
            }
            for o in scene.objects.iter().filter(|o| o.hair.is_some()) {
                let h = o.hair.as_ref().unwrap();
                eprintln!("{}: {} card triangles; {}", o.name, h.build_mesh().indices.len() / 3, h.coverage());
            }
            assert_eq!(img.samples, s.samples, "render did not finish its sample budget");
            eprintln!("{name}: {} samples in {:.1}s total, {:.1}s tracing (fibers {})", img.samples, t.elapsed().as_secs_f32(), img.seconds, strands.len());
        }
    }
}
