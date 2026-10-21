//! Hair cards (`Shape::Hair`): how game characters get hair.
//!
//! The agent draws guide curves (root on the scalp, out to the tip). Each
//! guide grows a few thin strips ("cards") around it, each a little offset
//! and turned, tapering to the tip. The cards show a generated texture of
//! many strands with see-through gaps (an alpha mask), colored from
//! `root_color` to `tip_color`, so a few hundred strips read as thousands of
//! hairs. Cards face away from `center` (the head's middle).
//!
//! Film renders use real strands instead (`Hair::strands`): thousands of
//! thin hairs grown from the same guides, filling the gaps between
//! neighboring guides, softly clumping at the tips, with a
//! little frizz.

use std::f32::consts::TAU;
use std::sync::Arc;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::material::Material;
use crate::mesh::{Mesh, Textures, Vertex};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Guide {
    /// Root first, in the hair object's own space.
    pub points: Vec<Vec3>,
    /// Card width at the root for this guide (default: `Hair::width`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Hair {
    pub guides: Vec<Guide>,
    /// Card width at the root.
    pub width: f32,
    /// Width at the tip, as a part of the root width.
    pub taper: f32,
    /// Cards per guide.
    pub cards: u32,
    /// How far extra cards sit from their guide.
    pub spread: f32,
    pub root_color: [f32; 3],
    pub tip_color: [f32; 3],
    /// Point the cards face away from (the head's middle), in object space;
    /// None = the middle of the guide roots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<Vec3>,
    /// Also use every guide mirrored across X = 0.
    pub mirror: bool,
    pub seed: u32,
    /// Film renders: strand budget; the hairline thins out at open edges.
    pub strands: u32,
    /// Film renders: strand width at the root (scene units).
    pub strand_width: f32,
    /// 0 = strands spread evenly, 1 = they gather tightly into locks
    /// toward the tips.
    pub clump: f32,
    /// Width of the biggest locks at the root (scene units); 0 = the guide
    /// spacing.
    pub clump_size: f32,
    /// Lock levels: 1 = locks only; 2 and 3 split each lock into smaller
    /// strands of hair inside it.
    pub clump_levels: u32,
    /// Random waviness, as a distance at the tip.
    pub frizz: f32,
    /// Thickness of the hair layer (scene units): strands rise off the
    /// scalp at the root and lie in layers up to this far out, the outer
    /// ones sparser and looser. 0 = every strand on its guide's shell.
    pub volume: f32,
    /// Share of stray strands (flyaways) that leave the surface, 0..0.3.
    pub flyaways: f32,
    /// Per-strand length variation, 0..1 (0.2 = some strands up to 20%
    /// shorter).
    pub length_variation: f32,
    /// Per-strand pigment variation, 0..1: some hairs darker, some lighter.
    pub melanin_variation: f32,
    /// Blend neighboring guides for a continuous scalp; false keeps
    /// individual brow/lash guides distinct.
    pub interpolate: bool,
    /// How far blended strands arch off the scalp just after the root, as
    /// a part of the usual (about a sixth of the root spacing); peach fuzz
    /// lies almost flat.
    pub rise: f32,
}

/// One film-render hair, in the hair object's space.
pub struct Strand {
    pub points: Vec<Vec3>,
    pub root_width: f32,
    pub tip_width: f32,
    /// Linear color at root and tip.
    pub root_color: Vec3,
    pub tip_color: Vec3,
}

/// Sample nodes and segment middles: a chord can enter the head even
/// when both endpoints are outside it.
pub fn inside_count(strands: &[Strand], inside: impl Fn(Vec3) -> bool) -> usize {
    strands.iter().filter(|s| s.points.iter().copied()
        .chain(s.points.windows(2).map(|p| (p[0] + p[1]) * 0.5)).any(&inside)).count()
}

/// A short brow follows the surface; a lash leaves it and curls toward
/// the comb direction. No gravity is applied at this scale.
pub fn facial_guide(root: Vec3, normal: Vec3, comb: Vec3, length: f32, lift: f32, lash: bool, surface: impl Fn(Vec3) -> Option<(Vec3, Vec3)>) -> Guide {
    let tangent = (comb - normal * normal.dot(comb)).normalize_or(normal.any_orthonormal_vector());
    let mut points = vec![root + normal * lift];
    for k in 1..=12 {
        let t = k as f32 / 12.0;
        let p = if lash {
            root + normal * (lift + length * t * (1.0 - 0.35 * t)) + tangent * length * 0.65 * t * t
        } else {
            let p = points[k - 1] + tangent * (length / 12.0);
            surface(p).map_or(p, |(q, n)| q + n * (lift + length * 0.04 * t * t))
        };
        points.push(p);
    }
    Guide { points, width: None }
}

pub const MAX_STRANDS: u32 = 200_000;
/// Segments along a film strand.
const STRAND_SEGMENTS: usize = 16;

impl Default for Hair {
    fn default() -> Self {
        Hair {
            guides: Vec::new(),
            width: 0.03,
            taper: 0.35,
            cards: 3,
            spread: 0.008,
            root_color: [0.25, 0.12, 0.05],
            tip_color: [0.55, 0.3, 0.12],
            center: None,
            mirror: false,
            seed: 1,
            strands: 30_000,
            strand_width: 0.00008,
            clump: 0.5,
            clump_size: 0.0,
            clump_levels: 2,
            frizz: 0.002,
            volume: 0.008,
            flyaways: 0.015,
            length_variation: 0.15,
            melanin_variation: 0.25,
            interpolate: true,
            rise: 1.0,
        }
    }
}

pub const MAX_GUIDES: usize = 512;
pub const MAX_POINTS: usize = 64;
const TEXTURE: u32 = 512;
/// Strand patterns side by side in the texture; each card uses one.
const VARIANTS: u32 = 4;
/// Segments along a card.
const SEGMENTS: usize = 12;

impl Hair {
    pub fn sanitize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Hair::default();
        self.guides.truncate(MAX_GUIDES);
        for g in &mut self.guides {
            g.points.truncate(MAX_POINTS);
            g.points.retain(|p| p.is_finite());
            g.width = g.width.map(|w| fix(w, 1e-4, 10.0, d.width));
        }
        self.width = fix(self.width, 1e-4, 10.0, d.width);
        self.taper = fix(self.taper, 0.0, 1.0, d.taper);
        self.cards = self.cards.clamp(1, 8);
        self.spread = fix(self.spread, 0.0, 10.0, d.spread);
        for c in [&mut self.root_color, &mut self.tip_color] {
            *c = c.map(|v| fix(v, 0.0, 1.0, 0.5));
        }
        self.center = self.center.filter(|c| c.is_finite());
        self.strands = self.strands.min(MAX_STRANDS);
        self.strand_width = fix(self.strand_width, 1e-6, 0.1, d.strand_width);
        self.clump = fix(self.clump, 0.0, 1.0, d.clump);
        self.clump_size = fix(self.clump_size, 0.0, 1.0, 0.0);
        self.clump_levels = self.clump_levels.clamp(1, 3);
        self.frizz = fix(self.frizz, 0.0, 1.0, d.frizz);
        self.volume = fix(self.volume, 0.0, 0.2, d.volume);
        self.flyaways = fix(self.flyaways, 0.0, 0.3, d.flyaways);
        self.length_variation = fix(self.length_variation, 0.0, 1.0, d.length_variation);
        self.melanin_variation = fix(self.melanin_variation, 0.0, 1.0, d.melanin_variation);
        self.rise = fix(self.rise, 0.0, 2.0, d.rise);
    }

    /// Area-distributed roots follow a blend of several guides. Each
    /// strand lies in a layer of the hair's volume (lifted off the scalp
    /// just after the root), gathers into locks and smaller locks inside
    /// them toward the free end, and varies in length, width and pigment;
    /// a few stray away (flyaways). Brows, lashes and braids
    /// (`interpolate: false`) keep to their guides.
    pub fn strands(&self) -> Vec<Strand> {
        let mut h = self.clone();
        h.sanitize();
        let guides = h.all_guides();
        let mut scalp = Scalp::new(&guides, h.center, STRAND_SEGMENTS);
        scalp.rise = h.rise;
        if scalp.paths.is_empty() || h.strands == 0 {
            return Vec::new();
        }
        let srgb = |c: [f32; 3]| Vec3::from(c).powf(2.2);
        let (root_c, tip_c) = (srgb(h.root_color), srgb(h.tip_color));
        let mut rng = Rng(h.seed.wrapping_mul(1597334677).max(1));
        let volume = if h.interpolate { h.volume } else { 0.0 };
        let clumps = Clumps::new(&h, &scalp, volume);
        (0..h.strands).filter_map(|index| {
            let (ids, weights) = scalp.sample(index, h.strands, h.interpolate, &mut rng);
            let root = scalp.blend(&ids, weights, 0);
            let edge = if h.interpolate { scalp.edge_distance(root) / scalp.spacing.max(1e-6) } else { 1.0 };
            if h.interpolate && edge < 0.18 && rng.next() > 0.35 + 0.65 * edge / 0.18 { return None; }
            let baby = h.interpolate && edge < 0.22 && rng.next() > edge / 0.22;
            let stray = h.interpolate && !baby && rng.next() < h.flyaways;
            let length = if baby { 0.12 + rng.next() * 0.38 }
                else { 1.0 - h.length_variation * rng.next().powf(1.5) - if stray { 0.5 * rng.next() } else { 0.0 } };
            // Layer in the volume: dense near the scalp, sparse outside.
            let layer = 1.0 - rng.next().sqrt();
            let radial = (root - scalp.center).normalize_or(Vec3::Y);
            let side = radial.any_orthonormal_vector();
            let cross = radial.cross(side);
            let jitter = if h.interpolate { side * (rng.next() - 0.5) + cross * (rng.next() - 0.5) } else { rng.unit() } * h.spread * 0.08;
            let phase = rng.next() * TAU;
            let freq = 1.0 + rng.next() * 2.0;
            let fine = (rng.next() * TAU, 5.0 + rng.next() * 6.0);
            // Outer strands are looser; strays wander far.
            let curl = h.frizz * (0.3 + rng.next() * 0.7) * (0.6 + 0.8 * layer) * if stray { 5.0 } else { 1.0 };
            let away = if stray { (volume * 2.5).max(h.frizz * 4.0) * (0.4 + rng.next()) } else { 0.0 };
            let base = scalp.path(&ids, weights, length, layer * volume, jitter);
            let (mut points, lock_shade) = clumps.pull(base, root, length, if stray { 0.15 } else { 1.0 });
            let n = points.len();
            for k in 1..n {
                let t = k as f32 / (n - 1) as f32;
                let tangent = (points[(k + 1).min(n - 1)] - points[k - 1]).normalize_or(Vec3::NEG_Y);
                let out = (points[k] - scalp.center).normalize_or(radial);
                let out = (out - tangent * tangent.dot(out)).normalize_or(side);
                let wave = side * ((t * freq * TAU + phase).sin() - phase.sin())
                    + cross * ((t * freq * TAU + phase).cos() - phase.cos())
                    + (side * (t * fine.1 * TAU + fine.0).sin() + cross * (t * fine.1 * 1.3 * TAU + fine.0).cos()) * 0.35;
                points[k] += wave * curl * t * if baby { 0.3 } else { 1.0 } + out * away * t * t;
            }
            // Pigment: melanin varies per hair (darker or lighter in its own
            // hue); outer layers and tips are a little sun-bleached.
            let gauss = (rng.next() + rng.next() + rng.next() - 1.5) * 2.0;
            let melanin = (gauss * 0.45 * h.melanin_variation).exp() * lock_shade;
            let tint = Vec3::new(1.0 + (rng.next() - 0.5) * 0.06, 1.0, 1.0 + (rng.next() - 0.5) * 0.06);
            let bleach = 1.0 + (0.12 * layer + 0.1 * rng.next()) * h.melanin_variation.min(0.5) * 2.0;
            let pigment = |c: Vec3| (c.clamp(Vec3::splat(1e-4), Vec3::ONE).powf(melanin) * tint).min(Vec3::ONE);
            // Real hair keeps its width until close to the tip; film
            // ribbons taper linearly, so scalp hair only narrows a little.
            let width = h.strand_width * (0.55 + 0.65 * rng.next()) * if baby { 0.4 } else if stray { 0.7 } else { 1.0 };
            Some(Strand { points, root_width: width, tip_width: width * if h.interpolate { 0.45 } else { 0.06 }, root_color: pigment(root_c), tip_color: (pigment(tip_c) * bleach).min(Vec3::ONE) })
        }).collect()
    }

    fn all_guides(&self) -> Vec<Guide> {
        let mut out: Vec<Guide> = self.guides.iter().filter(|g| g.points.len() >= 2).cloned().collect();
        if self.mirror {
            let mirrored: Vec<Guide> = out
                .iter()
                .map(|g| Guide { points: g.points.iter().map(|p| Vec3::new(-p.x, p.y, p.z)).collect(), width: g.width })
                .collect();
            out.extend(mirrored);
        }
        out
    }

    /// Geometry-only coverage estimates; collision checks use the scene's clay.
    pub fn coverage(&self) -> serde_json::Value {
        let mut h = self.clone();
        h.sanitize();
        let guides = h.all_guides();
        let scalp = Scalp::new(&guides, h.center, STRAND_SEGMENTS);
        let mut warnings = Vec::new();
        let gap = scalp.triangles.iter().map(|(ids, _)| {
            let p = ids.iter().map(|&i| scalp.paths[i][0]).sum::<Vec3>() / 3.0;
            ids.iter().map(|&i| p.distance(scalp.paths[i][0])).fold(f32::MAX, f32::min)
        }).fold(0.0f32, f32::max);
        let sparse = scalp.triangles.iter().filter(|(ids, _)| {
            let p = ids.iter().map(|&i| scalp.paths[i][0]).sum::<Vec3>() / 3.0;
            ids.iter().all(|&i| p.distance(scalp.paths[i][0]) > scalp.spacing * 1.5)
        }).count();
        if guides.is_empty() { warnings.push("no guides: no hair coverage".to_string()); }
        if h.interpolate && !guides.is_empty() {
            if scalp.triangles.is_empty() { warnings.push("roots do not span a scalp surface; add guides between the rows or set interpolate:false for isolated locks".into()); }
            if sparse > 0 { warnings.push(format!("{sparse} scalp patches have widely spaced guides; add roots in thin crown/part areas")); }
            let coverage = h.strands as f32 * h.strand_width * scalp.spacing / scalp.area.max(1e-9);
            if coverage < 2.0 { warnings.push("low root coverage: increase strands or add closer scalp guides; skin may show through".into()); }
        }
        serde_json::json!({"guides": h.guides.len(), "mirror": h.mirror, "cards": h.cards,
            "width": h.width, "strands": h.strands, "card_triangles": guides.len() * h.cards as usize * SEGMENTS * 4,
            "scalp_patches": scalp.triangles.len(), "scalp_area": scalp.area,
            "root_spacing": scalp.spacing, "largest_root_gap": gap, "sparse_patches": sparse, "warnings": warnings})
    }

    pub fn build_mesh(&self) -> Mesh {
        let mut h = self.clone();
        h.sanitize();
        let guides = h.all_guides();
        let material = Material::preset("hair").expect("hair preset");
        let mut mesh = Mesh { materials: vec![material.clone()], textures: Some(texture(&h)), ..Mesh::default() };
        if guides.is_empty() {
            return mesh;
        }
        let scalp = Scalp::new(&guides, h.center, SEGMENTS);
        let mut rng = Rng(h.seed.wrapping_mul(2654435761).max(1));
        let count = guides.len() as u32 * h.cards;
        for index in 0..count {
            // Keep one card on each drawn guide, including the hairline
            // and temples. The other layers fill the patch interiors.
            let anchor = index < guides.len() as u32;
            let (ids, weights) = if anchor {
                ([index as usize; 3], [1.0, 0.0, 0.0])
            } else {
                scalp.sample(index - guides.len() as u32, count - guides.len() as u32, h.interpolate, &mut rng)
            };
            let length = if h.interpolate { 0.84 + rng.next() * 0.16 } else { 1.0 };
            let path: Vec<_> = (0..=SEGMENTS).map(|k| {
                let at = k as f32 * length;
                let lo = (at.floor() as usize).min(SEGMENTS);
                let hi = (lo + 1).min(SEGMENTS);
                scalp.blend(&ids, weights, lo).lerp(scalp.blend(&ids, weights, hi), at.fract())
            }).collect();
            let width: f32 = ids.iter().zip(weights).map(|(&i, w)| guides[i].width.unwrap_or(h.width) * w).sum();
            let width = if !anchor && h.interpolate && scalp.area > 0.0 && ids.iter().all(|&i| guides[i].width.is_none()) { width.min(scalp.spacing * 1.3).max(width * 0.35) } else { width };
            let width = width * (0.75 + rng.next() * 0.5);
            let turn = (rng.next() - 0.5) * 0.4;
            let layer = index % h.cards;
            // Layers stack through the hair's volume like the strands do:
            // lifted off the scalp just after the root.
            let lift = if h.interpolate { h.spread * (0.08 + 0.18 * layer as f32 / h.cards as f32) } else { 0.0 };
            let depth = if h.interpolate { h.volume * (layer as f32 + 0.5) / h.cards as f32 } else { 0.0 };
            let variant = (rng.next() * VARIANTS as f32).min(VARIANTS as f32 - 1.0).floor();
            let base = mesh.vertices.len() as u32;
            let mut previous = None;
            for (i, &p) in path.iter().enumerate() {
                let t = i as f32 / SEGMENTS as f32;
                let tangent = (path[(i + 1).min(SEGMENTS)] - path[i.saturating_sub(1)]).normalize_or(Vec3::NEG_Y);
                let out = (p - scalp.center).normalize_or(Vec3::Z);
                let mut side = tangent.cross(out).normalize_or(tangent.any_orthonormal_vector());
                if previous.is_some_and(|s: Vec3| s.dot(side) < 0.0) { side = -side; }
                side = glam::Quat::from_axis_angle(tangent, turn) * side;
                previous = Some(side);
                let normal = side.cross(tangent).normalize_or(out);
                let half = 0.5 * width * (1.0 - (1.0 - h.taper) * t);
                let rise = (t / 0.12).min(1.0);
                let at = p + normal * (lift + depth * rise * (2.0 - rise) * (1.0 - 0.3 * t));
                // A shallow transverse arch gives the card volume and a
                // continuous normal field, with UV U across / V along hair.
                for (k, u) in [-1.0f32, 0.0, 1.0].into_iter().enumerate() {
                    let n = (normal + side * u * 0.28).normalize();
                    let across = (side - n * n.dot(side)).normalize();
                    mesh.vertices.push(Vertex {
                        pos: (at + side * half * u + normal * half * 0.14 * (1.0 - u * u)).to_array(),
                        normal: n.to_array(), color: [1.0; 3], surface: material.surface(), material: 0,
                        uv: [(variant + k as f32 * 0.5) / VARIANTS as f32, t],
                        tangent: [across.x, across.y, across.z, 1.0], light: material.light(),
                        base: [1.0, 1.0, 1.0, -1.0], wear: [0.0; 3], painted: [0; 4],
                    });
                }
            }
            for i in 0..SEGMENTS as u32 {
                for col in 0..2 {
                    let (a, b, c, d) = (base + 3 * i + col, base + 3 * i + col + 1, base + 3 * (i + 1) + col + 1, base + 3 * (i + 1) + col);
                    mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
                    mesh.edges.extend_from_slice(&[a, d]);
                }
            }
        }
        mesh
    }
}

/// A spherical Delaunay patch keeps root sampling off the chords inside
/// the head. Open boundary edges define the soft hairline.
struct Scalp {
    paths: Vec<Vec<Vec3>>,
    center: Vec3,
    triangles: Vec<([usize; 3], f32)>,
    boundary: Vec<(Vec3, Vec3)>,
    area: f32,
    spacing: f32,
    /// `Hair::rise`.
    rise: f32,
}

impl Scalp {
    fn new(guides: &[Guide], center: Option<Vec3>, segments: usize) -> Self {
        let paths: Vec<_> = guides.iter().map(|g| resample(&g.points, segments)).collect();
        let center = center.unwrap_or_else(|| {
            let mean = paths.iter().map(|p| p[0]).sum::<Vec3>() / paths.len().max(1) as f32;
            let (lo, hi) = paths.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[0].y), hi.max(p[0].y)));
            mean - Vec3::Y * if paths.is_empty() { 0.0 } else { (hi - lo) * 0.2 }
        });
        let roots: Vec<_> = paths.iter().map(|p| p[0]).collect();
        let dirs: Vec<_> = roots.iter().map(|p| (*p - center).normalize_or(Vec3::Y)).collect();
        let active: Vec<_> = (0..roots.len()).filter(|&i| !(0..i).any(|j| roots[i].distance_squared(roots[j]) < 1e-10)).collect();
        let mut triangles = Vec::new();
        let mut area = 0.0;
        let mut spacing = 0.0;
        for &i in &active {
            let mut near: Vec<_> = active.iter().copied().filter(|&j| j != i).collect();
            near.sort_by(|&a, &b| dirs[i].distance_squared(dirs[a]).total_cmp(&dirs[i].distance_squared(dirs[b])));
            near.truncate(12);
            if let Some(&j) = near.first() { spacing += roots[i].distance(roots[j]); }
            for (a, &j) in near.iter().enumerate().filter(|(_, j)| **j > i) {
                for &k in near.iter().skip(a + 1).filter(|&&k| k > i) {
                    let mut n = (dirs[j] - dirs[i]).cross(dirs[k] - dirs[i]);
                    if n.length_squared() < 1e-12 { continue; }
                    n = n.normalize();
                    if n.dot(dirs[i]) < 0.0 { n = -n; }
                    let plane = n.dot(dirs[i]);
                    if plane < 0.35 || active.iter().any(|&l| n.dot(dirs[l]) > plane + 1e-6) { continue; }
                    // Cocircular roots form one polygon, not overlapping
                    // triangles. Triangulate that face from a stable anchor.
                    let mut face: Vec<_> = active.iter().copied().filter(|&l| (n.dot(dirs[l]) - plane).abs() <= 1e-6).collect();
                    if face.len() > 3 {
                        let anchor = *face.iter().min().unwrap();
                        if i != anchor { continue; }
                        let middle = face.iter().map(|&l| dirs[l]).sum::<Vec3>() / face.len() as f32;
                        let x = (dirs[anchor] - middle).normalize();
                        let y = n.cross(x);
                        let angle = |l: usize| { let d = dirs[l] - middle; d.dot(y).atan2(d.dot(x)).rem_euclid(TAU) };
                        face.sort_by(|&a, &b| angle(a).total_cmp(&angle(b)));
                        let start = face.iter().position(|&l| l == anchor).unwrap();
                        face.rotate_left(start);
                        let a = face.iter().position(|&l| l == j).unwrap();
                        let b = face.iter().position(|&l| l == k).unwrap();
                        if a.abs_diff(b) != 1 || a == 0 || b == 0 { continue; }
                    }
                    let local = near.iter().take(3).map(|&l| roots[i].distance(roots[l])).fold(0.0f32, f32::max);
                    if roots[j].distance(roots[k]).max(roots[i].distance(roots[j])).max(roots[i].distance(roots[k])) > local * 3.0 { continue; }
                    let size = (roots[j] - roots[i]).cross(roots[k] - roots[i]).length() * 0.5;
                    area += size;
                    triangles.push(([i, j, k], area));
                }
            }
        }
        let mut edges = std::collections::BTreeMap::new();
        for (ids, _) in &triangles {
            for (a, b) in [(ids[0], ids[1]), (ids[1], ids[2]), (ids[2], ids[0])] {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        let boundary = edges.into_iter().filter(|(_, count)| *count == 1).map(|((a, b), _)| (roots[a], roots[b])).collect();
        Self { paths, center, triangles, boundary, area, spacing: spacing / active.len().max(1) as f32, rise: 1.0 }
    }

    fn sample(&self, index: u32, count: u32, interpolate: bool, rng: &mut Rng) -> ([usize; 3], [f32; 3]) {
        if !interpolate || self.triangles.is_empty() {
            let i = index as usize % self.paths.len();
            // Two-guide and narrow grooms still fill the gap uniformly.
            if interpolate && self.paths.len() > 1 {
                let j = (0..self.paths.len()).filter(|&j| j != i).min_by(|&a, &b| self.paths[a][0].distance_squared(self.paths[i][0]).total_cmp(&self.paths[b][0].distance_squared(self.paths[i][0]))).unwrap();
                let w = rng.next();
                return ([i, j, i], [1.0 - w, w, 0.0]);
            }
            return ([i; 3], [1.0, 0.0, 0.0]);
        }
        let area = self.area * (index as f32 + rng.next()) / count as f32;
        let slot = self.triangles.partition_point(|t| t.1 < area).min(self.triangles.len() - 1);
        let u = rng.next().sqrt();
        let v = rng.next();
        (self.triangles[slot].0, [1.0 - u, u * (1.0 - v), u * v])
    }

    fn blend(&self, ids: &[usize; 3], weights: [f32; 3], k: usize) -> Vec3 {
        let p: Vec3 = ids.iter().zip(weights).map(|(&i, w)| self.paths[i][k] * w).sum();
        if self.triangles.is_empty() { return p; }
        let radius: f32 = ids.iter().zip(weights).map(|(&i, w)| self.paths[i][k].distance(self.center) * w).sum();
        let t = k as f32 / (self.paths[0].len() - 1) as f32;
        // Follow the curved scalp near the roots rather than cutting into it.
        let radial = (p - self.center).normalize_or(Vec3::Y);
        let lift = if ids[0] == ids[1] && ids[1] == ids[2] { 0.0 }
            else { self.spacing * 0.16 * self.rise * (std::f32::consts::PI * (t / 0.35).min(1.0)).sin().max(0.0) };
        p + radial * ((radius - p.distance(self.center)) * (1.0 - t).powi(4) + lift)
    }

    /// A strand following the blend of guides `ids`: `length` of the way
    /// out along them, `lift` off the guide shell (rising just after the
    /// root, settling a little toward the tip), moved by `jitter`.
    fn path(&self, ids: &[usize; 3], weights: [f32; 3], length: f32, lift: f32, jitter: Vec3) -> Vec<Vec3> {
        let n = self.paths[0].len() - 1;
        let mut points: Vec<Vec3> = (0..=n).map(|k| {
            let at = k as f32 * length;
            let lo = (at.floor() as usize).min(n);
            let hi = (lo + 1).min(n);
            self.blend(ids, weights, lo).lerp(self.blend(ids, weights, hi), at.fract()) + jitter
        }).collect();
        if lift > 0.0 {
            let original = points.clone();
            for k in 1..=n {
                let t = k as f32 / n as f32;
                let tangent = (original[(k + 1).min(n)] - original[k - 1]).normalize_or(Vec3::NEG_Y);
                let out = (original[k] - self.center).normalize_or(Vec3::Y);
                let out = (out - tangent * tangent.dot(out)).normalize_or(out);
                let rise = (t / 0.12).min(1.0);
                points[k] += out * lift * rise * (2.0 - rise) * (1.0 - 0.3 * t);
            }
        }
        points
    }

    fn edge_distance(&self, p: Vec3) -> f32 {
        self.boundary.iter().map(|&(a, b)| {
            let ab = b - a;
            p.distance(a + ab * ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0))
        }).fold(f32::MAX, f32::min)
    }
}

/// Locks of hair: strand paths that nearby strands gather toward along
/// their length. Each finer level's locks are themselves pulled into the
/// coarser ones, so big locks split into smaller strands of hair.
struct Clumps {
    levels: Vec<ClumpLevel>,
}

struct ClumpLevel {
    strength: f32,
    cell: f32,
    roots: Vec<Vec3>,
    paths: Vec<Vec<Vec3>>,
    /// Pigment factor per lock (locks differ a little in shade).
    shades: Vec<f32>,
    grid: std::collections::HashMap<(i32, i32, i32), Vec<u32>>,
}

/// Most lock centers per level.
const MAX_CLUMPS: usize = 4000;

impl Clumps {
    fn new(h: &Hair, scalp: &Scalp, volume: f32) -> Clumps {
        let mut out = Clumps { levels: Vec::new() };
        if !h.interpolate || h.clump <= 0.0 || scalp.paths.is_empty() {
            return out;
        }
        let base = if h.clump_size > 0.0 { h.clump_size } else { scalp.spacing.max(1e-4) };
        let mut rng = Rng(h.seed.wrapping_mul(2246822519).max(1));
        for (level, (shrink, strength)) in [(1.0, 0.85), (2.6, 0.6), (6.5, 0.45)].into_iter().take(h.clump_levels as usize).enumerate() {
            let size = base / shrink;
            let count = if scalp.area > 0.0 { (scalp.area / (size * size)).round() as usize } else { scalp.paths.len() << (2 * level) };
            let count = count.clamp(1, MAX_CLUMPS);
            let mut roots = Vec::with_capacity(count);
            let mut paths = Vec::with_capacity(count);
            let mut shades = Vec::with_capacity(count);
            for i in 0..count {
                let (ids, weights) = scalp.sample(i as u32, count as u32, true, &mut rng);
                let root = scalp.blend(&ids, weights, 0);
                let mut path = scalp.path(&ids, weights, 1.0, volume * (0.15 + 0.6 * rng.next()), Vec3::ZERO);
                // Each lock sways its own way along its length, across and
                // in and out of the layer, so locks part and overlap.
                let (phase, turns) = (rng.next() * TAU, 0.4 + rng.next() * 0.8);
                let sway = size * (0.2 + 0.3 * rng.next()) * h.clump;
                let n = path.len() - 1;
                for k in 1..=n {
                    let t = k as f32 / n as f32;
                    let tangent = (path[(k + 1).min(n)] - path[k - 1]).normalize_or(Vec3::NEG_Y);
                    let out_dir = (path[k] - scalp.center).normalize_or(Vec3::Y);
                    let out_dir = (out_dir - tangent * tangent.dot(out_dir)).normalize_or(tangent.any_orthonormal_vector());
                    let across = tangent.cross(out_dir);
                    let a = t * turns * TAU + phase;
                    path[k] += (across * (a.sin() - phase.sin()) + out_dir * 0.5 * (a.cos() - phase.cos())) * sway * t;
                }
                let (path, shade) = out.pull(path, root, 1.0, 1.0);
                paths.push(path);
                shades.push(shade * (((rng.next() + rng.next() - 1.0) * 0.35 * h.melanin_variation).exp()));
                roots.push(root);
            }
            let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<u32>> = std::collections::HashMap::new();
            for (i, r) in roots.iter().enumerate() {
                grid.entry(cell_of(*r, size)).or_default().push(i as u32);
            }
            out.levels.push(ClumpLevel { strength: h.clump * strength, cell: size, roots, paths, shades, grid });
        }
        out
    }

    /// `path` (a strand rooted at `root`, `length` of the way out) pulled
    /// into its locks; `scale` weakens the pull (strays). Also returns the
    /// pigment factor of its finest lock.
    fn pull(&self, mut path: Vec<Vec3>, root: Vec3, length: f32, scale: f32) -> (Vec<Vec3>, f32) {
        let n = path.len() - 1;
        let mut shade = 1.0;
        for level in &self.levels {
            let (x, y, z) = cell_of(root, level.cell);
            let mut best = (f32::MAX, None);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &i in level.grid.get(&(x + dx, y + dy, z + dz)).into_iter().flatten() {
                            let d = level.roots[i as usize].distance_squared(root);
                            if d < best.0 { best = (d, Some(i as usize)); }
                        }
                    }
                }
            }
            let Some(c) = best.1 else { continue };
            shade = level.shades[c];
            let center = &level.paths[c];
            for (k, p) in path.iter_mut().enumerate().skip(1) {
                let t = k as f32 / n as f32;
                let at = t * length * n as f32;
                let (lo, hi) = ((at.floor() as usize).min(n), (at.floor() as usize + 1).min(n));
                let target = center[lo].lerp(center[hi], at.fract());
                *p = p.lerp(target, level.strength * scale * t.powf(0.6));
            }
        }
        (path, shade)
    }
}

fn cell_of(p: Vec3, size: f32) -> (i32, i32, i32) {
    let c = (p / size.max(1e-6)).floor().as_ivec3();
    (c.x, c.y, c.z)
}

/// `n` steps along a polyline, concentrated near the root (n + 1 points).
fn resample(points: &[Vec3], n: usize) -> Vec<Vec3> {
    let lengths: Vec<f32> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    let total: f32 = lengths.iter().sum::<f32>().max(1e-9);
    (0..=n)
        .map(|i| {
            // Spend more vertices near the curved scalp; long free ends
            // can use longer segments without cutting into the forehead.
            let mut want = total * (i as f32 / n as f32).powf(1.6);
            for (k, &l) in lengths.iter().enumerate() {
                if want <= l || k == lengths.len() - 1 {
                    return points[k].lerp(points[k + 1], (want / l.max(1e-9)).min(1.0));
                }
                want -= l;
            }
            *points.last().expect("2+ points")
        })
        .collect()
}

/// The strand texture: `VARIANTS` columns of strands on a clear background,
/// colored root (top, v = 0) to tip.
fn texture(h: &Hair) -> Arc<Textures> {
    let n = TEXTURE as usize;
    let mut color = vec![0u8; n * n * 4];
    let mut orm = vec![0u8; n * n * 4];
    let mut normal: Vec<u8> = [128u8, 128, 255, 255].repeat(n * n);
    let mut rng = Rng(h.seed.wrapping_mul(747796405).max(1));
    let col_w = n / VARIANTS as usize;
    let mut cover = vec![0.0f32; n * n];
    let mut shade = vec![0.0f32; n * n];
    // The same model as the film strands, flattened onto a card: strands
    // gather into locks (and smaller locks) toward the tip, vary in length,
    // width and pigment, and a few stray across.
    let mut tips = vec![0.0f32; n * n];
    for v in 0..VARIANTS as usize {
        let left = (v * col_w) as f32 + 3.0;
        let span = col_w as f32 - 6.0;
        let locks: Vec<f32> = (0..5).map(|_| left + rng.next() * span).collect();
        let small: Vec<f32> = (0..14).map(|_| left + rng.next() * span).collect();
        let nearest = |x: f32, list: &[f32]| list.iter().copied().min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs())).unwrap_or(x);
        for strand in 0..64 {
            let x0 = left + rng.next() * span;
            let stray = rng.next() < (h.flyaways * 6.0).min(0.5);
            let end = (1.0 - h.length_variation.max(0.1) * rng.next().powf(1.5) * 1.6).max(0.45) - if stray { 0.3 * rng.next() } else { 0.0 };
            let start = if strand < 6 { 0.12 * rng.next() } else if strand < 18 { 0.0 } else { 0.008 * rng.next() };
            let gauss = (rng.next() + rng.next() + rng.next() - 1.5) * 2.0;
            let bright = (gauss * 0.3 * h.melanin_variation.max(0.15)).exp();
            let (amp, freq, phase) = (rng.next() * if stray { 9.0 } else if strand < 6 { 5.0 } else { 1.5 }, 2.0 + rng.next() * 3.0, rng.next() * TAU);
            let thick = (0.25 + rng.next() * 0.55) * if stray { 0.7 } else { 1.0 };
            let (big, fine) = (nearest(x0, &locks), nearest(x0, &small));
            let fine = fine + (big - fine) * h.clump * 0.6;
            let pull = if stray { 0.15 } else { 1.0 };
            let bleach = 1.0 + 0.25 * rng.next();
            for y in (start * n as f32) as usize..(end * n as f32) as usize {
                let t = y as f32 / n as f32;
                let gather = t.powf(0.6);
                let mut x = x0;
                x += (big - x) * h.clump * 0.85 * pull * gather;
                if h.clump_levels > 1 { x += (fine - x) * h.clump * 0.6 * pull * gather; }
                let x = (x + amp * (t * freq * TAU + phase).sin()).clamp(left, left + span);
                // Thinner toward its own tip.
                let w = thick * (1.0 - 0.7 * (t / end).powi(2));
                let lo = (x - w - 1.0).floor().max((v * col_w) as f32) as usize;
                let hi = ((x + w + 1.0).ceil() as usize).min((v + 1) * col_w - 1);
                for px in lo..=hi {
                    let a = (1.0 - ((px as f32 + 0.5 - x).abs() - w).max(0.0)).clamp(0.0, 1.0)
                        * ((t - start) * 90.0 + 0.35).min(1.0) * ((end - t) * 80.0).min(1.0);
                    let i = y * n + px;
                    if a > cover[i] {
                        cover[i] = a;
                        shade[i] = bright;
                        tips[i] = (t / end).powi(2) * (bleach - 1.0);
                    }
                }
            }
        }
    }
    let (root, tip) = (Vec3::from(h.root_color), Vec3::from(h.tip_color));
    for y in 0..n {
        let t = y as f32 / n as f32;
        let base = root.lerp(tip, t.powf(0.7));
        for x in 0..n {
            let i = y * n + x;
            let nx = (cover[y * n + x.saturating_sub(1)] - cover[y * n + (x + 1).min(n - 1)]) * 0.4;
            normal[i * 4] = ((nx * 0.5 + 0.5) * 255.0) as u8;
            normal[i * 4 + 2] = (((1.0 - nx * nx).sqrt() * 0.5 + 0.5) * 255.0) as u8;
            // Keep pigment in transparent texels to avoid black fringes
            // when an engine filters the alpha mask.
            let brightness = if cover[i] > 0.0 { shade[i] } else { 1.0 };
            let c = (base * brightness * (1.0 + tips[i])).clamp(Vec3::ZERO, Vec3::ONE) * 255.0;
            // Coverage alpha preserves fine fibers when textures are filtered.
            let a = (cover[i] * 255.0) as u8;
            color[i * 4..i * 4 + 4].copy_from_slice(&[c.x as u8, c.y as u8, c.z as u8, a]);
            // Darker near the root, where hair packs together.
            let ao = (0.55 + 0.45 * t.sqrt()) * 255.0;
            orm[i * 4..i * 4 + 4].copy_from_slice(&[ao as u8, 165, 0, 255]);
        }
    }
    Arc::new(Textures { size: TEXTURE, color, normal, orm, alpha: true })
}

/// Small deterministic random numbers (xorshift), so hair looks the same
/// every time it is built.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    fn unit(&mut self) -> Vec3 {
        let v = Vec3::new(self.next() - 0.5, self.next() - 0.5, self.next() - 0.5);
        v.normalize_or(Vec3::X)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalp_roots_cover_patch_interiors_and_stay_on_the_surface() {
        let roots: Vec<_> = (0..5).flat_map(|x| (0..5).map(move |z| {
            Vec3::new((x as f32 - 2.0) * 0.1, 1.0, (z as f32 - 2.0) * 0.1).normalize()
        })).collect();
        let hair = Hair { guides: roots.iter().map(|&r| Guide { points: vec![r, r * 1.05 + Vec3::NEG_Z * 0.15], width: None }).collect(),
            center: Some(Vec3::ZERO), strands: 8000, spread: 0.0, frizz: 0.001, ..Hair::default() };
        let scalp = Scalp::new(&hair.guides, hair.center, STRAND_SEGMENTS);
        assert_eq!(scalp.triangles.len(), 32, "scalp faces must not overlap");
        let strands = hair.strands();
        let mut bins = [0; 16];
        for strand in &strands {
            let p = strand.points[0];
            assert!((p.length() - 1.0).abs() < 1e-5, "root inside scalp: {p}");
            let (x, z) = (p.x / p.y, p.z / p.y);
            let bx = (((x + 0.2) / 0.1) as usize).min(3);
            let bz = (((z + 0.2) / 0.1) as usize).min(3);
            bins[bx * 4 + bz] += 1;
            assert!(strand.points.iter().all(|p| p.is_finite()));
        }
        assert!(bins.iter().all(|&n| (250..750).contains(&n)), "uneven root density: {bins:?}");
        let again = hair.strands();
        assert_eq!(strands[73].points, again[73].points);
        assert!(strands.iter().any(|s| s.root_width < hair.strand_width * 0.5));
        let cards = hair.build_mesh();
        for (i, &root) in roots.iter().enumerate() {
            let base = i * (SEGMENTS + 1) * 3;
            let a = Vec3::from(cards.vertices[base].pos);
            let b = Vec3::from(cards.vertices[base + 2].pos);
            assert!(((a + b) * 0.5).distance(root) <= hair.spread + 1e-6, "missing boundary card");
        }
        assert_eq!(cards.indices.len() / 3, roots.len() * hair.cards as usize * SEGMENTS * 4);
        for v in &cards.vertices {
            let n = Vec3::from(v.normal);
            let t = Vec3::new(v.tangent[0], v.tangent[1], v.tangent[2]);
            assert!((n.length() - 1.0).abs() < 1e-5 && n.dot(t).abs() < 1e-5);
        }
    }

    #[test]
    fn collision_sampling_catches_chords_between_outside_nodes() {
        let strand = Strand { points: vec![Vec3::NEG_X * 2.0, Vec3::X * 2.0], root_width: 0.001,
            tip_width: 0.0001, root_color: Vec3::ONE, tip_color: Vec3::ONE };
        assert_eq!(inside_count(&[strand], |p| p.length() < 1.0), 1);
    }

    #[test]
    fn long_guides_keep_short_segments_at_the_scalp() {
        let path = resample(&[Vec3::ZERO, Vec3::Z * 0.08, Vec3::new(0.0, -0.7, 0.08)], STRAND_SEGMENTS);
        assert!(path[1].distance(path[0]) < 0.012);
        assert!(path[2].distance(path[1]) < 0.022);
        assert!(path.last().unwrap().distance(Vec3::new(0.0, -0.7, 0.08)) < 1e-6);
    }

    #[test]
    fn coverage_reports_thin_grooms_and_the_actual_card_budget() {
        let hair = Hair { guides: vec![Guide { points: vec![Vec3::Y, Vec3::Y + Vec3::Z], width: None }], strands: 10, ..Hair::default() };
        let report = hair.coverage();
        assert!(!report["warnings"].as_array().unwrap().is_empty());
        assert_eq!(report["card_triangles"].as_u64().unwrap(), (hair.build_mesh().indices.len() / 3) as u64);
    }

    #[test]
    fn individual_lashes_do_not_bridge_guides_and_export_as_cards() {
        let hair = Hair {
            guides: vec![
                Guide { points: vec![Vec3::ZERO, Vec3::Z * 0.006], width: None },
                Guide { points: vec![Vec3::X * 0.01, Vec3::X * 0.01 + Vec3::Z * 0.007], width: None },
            ],
            interpolate: false, strands: 2, spread: 0.0, frizz: 0.0,
            width: 0.0002, cards: 1, strand_width: 0.00006, taper: 0.05,
            ..Hair::default()
        };
        let strands = hair.strands();
        assert_eq!(strands[0].points[0], Vec3::ZERO);
        assert_eq!(strands[1].points[0], Vec3::X * 0.01);
        assert!(strands[0].points.iter().all(|p| p.x == 0.0));
        assert!(strands[1].points.iter().all(|p| (p.x - 0.01).abs() < 1e-7));
        let cards = hair.build_mesh();
        assert_eq!(cards.indices.len() / 3, 96);
        assert!(cards.textures.as_ref().unwrap().alpha);
    }

    #[test]
    fn facial_hair_follows_skin_or_curls_outward() {
        let surface = |p: Vec3| Some((Vec3::new(p.x, p.y, 0.0), Vec3::Z));
        let brow = facial_guide(Vec3::ZERO, Vec3::Z, Vec3::X, 0.006, 0.0001, false, surface);
        assert!(brow.points.iter().all(|p| p.z >= 0.0001 && p.z < 0.0004));
        assert!((brow.points.last().unwrap().x - 0.006).abs() < 1e-6);
        let lash = facial_guide(Vec3::ZERO, Vec3::Z, Vec3::Y, 0.008, 0.0001, true, surface);
        let tip = lash.points.last().unwrap();
        assert!(tip.z > 0.005 && tip.y > 0.005);
    }

    #[test]
    fn narrow_grooms_fill_the_gap_with_soft_clumping() {
        let g = |x: f32| Guide { points: vec![Vec3::new(x, 1.0, 0.0), Vec3::new(x, 0.5, 0.0)], width: None };
        let hair = Hair { guides: vec![g(0.0), g(0.1)], strands: 500, clump: 1.0, frizz: 0.0, ..Hair::default() };
        let s = hair.strands();
        assert_eq!(s.len(), 500);
        for st in &s {
            assert_eq!(st.points.len(), STRAND_SEGMENTS + 1);
            // Clumps stay soft even at full strength (locks may sway a
            // little past the outer guides).
            assert!((-0.01..=0.11).contains(&st.points[0].x));
            let tip = st.points.last().unwrap();
            assert!((-0.05..=0.15).contains(&tip.x), "{tip}");
            assert!(st.tip_width < st.root_width);
        }
        // Some roots sit in the gap between the guides.
        assert!(s.iter().any(|st| (0.02..0.08).contains(&st.points[0].x)));
    }

    #[test]
    fn guides_grow_tapered_cards_with_a_masked_texture() {
        let hair = Hair {
            guides: vec![Guide { points: vec![Vec3::new(0.05, 1.0, 0.0), Vec3::new(0.1, 0.8, 0.02), Vec3::new(0.12, 0.6, 0.0)], width: None }],
            mirror: true,
            center: Some(Vec3::new(0.0, 0.9, 0.0)),
            ..Hair::default()
        };
        let m = hair.build_mesh();
        // 2 guides (mirror) x 3 cards x (SEGMENTS + 1) x 2 vertices.
        assert_eq!(m.vertices.len(), 2 * 3 * (SEGMENTS + 1) * 3);
        assert_eq!(m.indices.len(), 2 * 3 * SEGMENTS * 12);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
        // Root is wider than tip.
        let w = |k: usize| Vec3::from(m.vertices[k].pos).distance(Vec3::from(m.vertices[k + 2].pos));
        assert!(w(0) > 2.0 * w(3 * SEGMENTS));
        let tex = m.textures.as_ref().unwrap();
        assert!(tex.alpha);
        let clear = tex.color.chunks_exact(4).filter(|p| p[3] == 0).count();
        assert!(clear > tex.color.len() / 4 / 10 && clear < tex.color.len() / 4 * 9 / 10, "{clear}");
        assert!(tex.color.chunks_exact(4).filter(|p| p[3] == 0).all(|p| p[0] > 0));
        // Faces wind outward (away from the center) for the first card.
        let p = |i: u32| Vec3::from(m.vertices[i as usize].pos);
        let t = &m.indices[0..3];
        let face = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
        assert!(face.dot(Vec3::from(m.vertices[t[0] as usize].normal)) > 0.0);
    }

    /// A 5 x 5 patch of guides on a unit sphere, hanging toward -Z.
    fn patch(h: Hair) -> Hair {
        let roots: Vec<_> = (0..5).flat_map(|x| (0..5).map(move |z| Vec3::new((x as f32 - 2.0) * 0.1, 1.0, (z as f32 - 2.0) * 0.1).normalize())).collect();
        Hair { guides: roots.iter().map(|&r| Guide { points: vec![r, r * 1.02 + Vec3::NEG_Z * 0.1, r * 1.05 + Vec3::NEG_Z * 0.3], width: None }).collect(),
            center: Some(Vec3::ZERO), strands: 3000, spread: 0.0, frizz: 0.0, ..h }
    }

    #[test]
    fn locks_gather_strand_tips_and_are_repeatable() {
        // Tips in fewer, denser places: count occupied 1 cm cells.
        let cells = |h: &Hair| {
            let tips: std::collections::HashSet<_> = h.strands().iter().map(|s| cell_of(*s.points.last().unwrap(), 0.01)).collect();
            tips.len()
        };
        let loose = patch(Hair { clump: 0.0, ..Hair::default() });
        let locks = patch(Hair { clump: 0.8, clump_levels: 3, ..Hair::default() });
        assert!(cells(&locks) * 10 < cells(&loose) * 8, "locks {} loose {}", cells(&locks), cells(&loose));
        // Roots stay where they were: clumping acts along the length.
        let (a, b) = (loose.strands(), locks.strands());
        assert!(a.iter().zip(&b).all(|(a, b)| a.points[0].distance(b.points[0]) < 1e-6));
        // Same settings, same hair; another seed, other hair.
        let again = locks.strands();
        assert!(b.iter().zip(&again).all(|(a, b)| a.points == b.points && a.root_color == b.root_color));
        let other = Hair { seed: 7, ..locks.clone() }.strands();
        assert!(b.iter().zip(&other).any(|(a, b)| a.points != b.points));
        // Strands vary in length, width and pigment.
        let lengths: Vec<f32> = b.iter().map(|s| s.points.windows(2).map(|w| w[0].distance(w[1])).sum()).collect();
        let (lo, hi) = lengths.iter().fold((f32::MAX, 0.0f32), |(lo, hi), &l| (lo.min(l), hi.max(l)));
        assert!(hi > lo * 1.12, "lengths {lo}..{hi}");
        assert!(b.iter().any(|s| s.root_color.x > b[0].root_color.x * 1.1) && b.iter().any(|s| s.root_color.x < b[0].root_color.x * 0.9));
        // Tips lighter than roots on average when tip_color is lighter.
        assert!(b.iter().all(|s| s.tip_color.x >= s.root_color.x));
    }

    #[test]
    fn volume_lifts_strands_off_the_scalp_after_the_root() {
        let radius_at = |h: &Hair, k: usize| {
            let s = h.strands();
            s.iter().map(|s| s.points[k].length()).sum::<f32>() / s.len() as f32
        };
        let flat = patch(Hair { volume: 0.0, clump: 0.0, ..Hair::default() });
        let full = patch(Hair { volume: 0.02, clump: 0.0, ..Hair::default() });
        // Roots on the scalp either way; just after the root, lifted.
        assert!((radius_at(&flat, 0) - 1.0).abs() < 1e-4 && (radius_at(&full, 0) - 1.0).abs() < 1e-4);
        let lift = radius_at(&full, 3) - radius_at(&flat, 3);
        assert!(lift > 0.004 && lift < 0.02, "lift {lift}");
        // Layers: some strands sit close in, a few far out.
        let s = full.strands();
        let base = flat.strands();
        let offsets: Vec<f32> = s.iter().zip(&base).map(|(a, b)| a.points[8].length() - b.points[8].length()).collect();
        let inner = offsets.iter().filter(|&&o| o < 0.005).count();
        let outer = offsets.iter().filter(|&&o| o > 0.012).count();
        assert!(inner > outer && outer > 0, "inner {inner} outer {outer}");
        // Flyaways leave the surface.
        let tidy = patch(Hair { flyaways: 0.0, ..Hair::default() });
        let wild = patch(Hair { flyaways: 0.3, ..Hair::default() });
        let farthest = |h: &Hair| h.strands().iter().flat_map(|s| s.points.clone()).map(|p| p.length()).fold(0.0f32, f32::max);
        assert!(farthest(&wild) > farthest(&tidy) + 0.005, "{} {}", farthest(&wild), farthest(&tidy));
        // Brows and lashes ignore volume.
        let lash = Hair { guides: vec![Guide { points: vec![Vec3::ZERO, Vec3::Z * 0.006], width: None }], interpolate: false, strands: 1, volume: 0.05, ..Hair::default() };
        assert!(lash.strands()[0].points.iter().all(|p| p.x.abs() < 0.003 && p.y.abs() < 0.003));
    }
}
