//! One stone: a block between eight corners (any hexahedron: bricks,
//! wedges for arches, drums for columns) with rounded edges, chipped
//! corners and a rough, slightly uneven face. Walls, floors, arches and
//! rubble are all made of these.
//!
//! The block is a grid on each face of a unit cube, welded at the seams.
//! Each grid point is rounded and chipped in a box the size of the stone,
//! then carried to the corners by trilinear interpolation, then pushed out
//! or in by noise. Everything depends on position and the seed only, so a
//! stone always comes out the same.

use std::collections::HashMap;

use glam::{Quat, Vec3};

use super::{Builder, Rand, fbm, noise};

#[derive(Clone, Copy)]
pub struct Style {
    /// Target grid spacing (scene units): smaller is smoother, more triangles.
    pub cell: f32,
    /// Edge rounding radius.
    pub round: f32,
    /// How far the face goes in and out (scene units).
    pub rough: f32,
    /// Chipped corners and edges, and how deep (share of the smallest side).
    pub chips: u32,
    pub chip_depth: f32,
    /// Map the XZ section to a disc (column drums, round stones).
    pub round_section: bool,
    pub seed: u64,
    /// sRGB.
    pub color: Vec3,
    pub material: u32,
}

impl Default for Style {
    fn default() -> Self {
        Style { cell: 0.08, round: 0.02, rough: 0.008, chips: 2, chip_depth: 0.15, round_section: false, seed: 1, color: Vec3::splat(0.45), material: 0 }
    }
}

/// Corner `i` of a block has x = bit 0, y = bit 1, z = bit 2.
pub type Corners = [Vec3; 8];

/// An axis-aligned box from `lo` to `hi`.
pub fn aabb(lo: Vec3, hi: Vec3) -> Corners {
    std::array::from_fn(|i| Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }))
}

/// Turns the corners about their center by `q`.
pub fn turn(c: Corners, q: Quat) -> Corners {
    let center = c.iter().copied().sum::<Vec3>() / 8.0;
    c.map(|p| center + q * (p - center))
}

fn trilinear(c: &Corners, a: f32, b: f32, w: f32) -> Vec3 {
    let x0 = c[0].lerp(c[1], a);
    let x1 = c[2].lerp(c[3], a);
    let x2 = c[4].lerp(c[5], a);
    let x3 = c[6].lerp(c[7], a);
    x0.lerp(x1, b).lerp(x2.lerp(x3, b), w)
}

/// Grid fractions along one side of length `len`: rows at the bevel's
/// start and middle near both ends, so the rounded edge is resolved.
fn fractions(len: f32, cell: f32, round: f32) -> Vec<f32> {
    let r = (round / len.max(1e-6)).min(0.45);
    let mut f = vec![0.0, 1.0];
    let lo = if r > 1e-3 {
        f.extend([r * 0.5, r, 1.0 - r, 1.0 - r * 0.5]);
        r
    } else {
        0.0
    };
    let m = ((1.0 - 2.0 * lo) * len / cell).round().clamp(1.0, 40.0) as usize;
    f.extend((1..m).map(|i| lo + (1.0 - 2.0 * lo) * i as f32 / m as f32));
    f.sort_by(f32::total_cmp);
    f.dedup_by(|a, b| (*a - *b).abs() < 1e-5);
    f
}

/// Adds one stone. Returns the range of vertices it made.
pub fn block(b: &mut Builder, c: Corners, s: &Style) -> std::ops::Range<usize> {
    let start = b.mesh.vertices.len();
    let len = |i: usize, j: usize| c[i].distance(c[j]);
    let size = Vec3::new(
        (len(0, 1) + len(2, 3) + len(4, 5) + len(6, 7)) * 0.25,
        (len(0, 2) + len(1, 3) + len(4, 6) + len(5, 7)) * 0.25,
        (len(0, 4) + len(1, 5) + len(2, 6) + len(3, 7)) * 0.25,
    )
    .max(Vec3::splat(1e-4));
    let half = size * 0.5;
    let r = s.round.min(size.min_element() * 0.42);
    let inner = half - Vec3::splat(r);
    let axes = [
        ((c[1] - c[0]) + (c[3] - c[2]) + (c[5] - c[4]) + (c[7] - c[6])).normalize_or(Vec3::X),
        ((c[2] - c[0]) + (c[3] - c[1]) + (c[6] - c[4]) + (c[7] - c[5])).normalize_or(Vec3::Y),
        ((c[4] - c[0]) + (c[5] - c[1]) + (c[6] - c[2]) + (c[7] - c[3])).normalize_or(Vec3::Z),
    ];
    let flip = axes[0].dot(axes[1].cross(axes[2])) < 0.0;
    let mut rng = Rand::new(s.seed);
    // Chips: planes that cut a corner or an edge off (box coordinates).
    let chips: Vec<(Vec3, f32)> = (0..s.chips)
        .map(|_| {
            let mut corner = Vec3::new(rng.signed().signum(), rng.signed().signum(), rng.signed().signum());
            // Half the chips take a whole edge instead of a corner.
            if rng.chance(0.5) {
                let k = (rng.f() * 3.0) as usize % 3;
                corner[k] = 0.0;
            }
            let point = corner * half;
            let n = (corner * Vec3::new(1.0 / half.x, 1.0 / half.y, 1.0 / half.z) + rng.unit() * 0.35).normalize_or(Vec3::Y);
            let depth = s.chip_depth * size.min_element() * rng.range(0.3, 1.0);
            (n, point.dot(n) - depth)
        })
        .collect();
    let fx = fractions(size.x, s.cell, r);
    let fy = fractions(size.y, s.cell, r);
    let fz = fractions(size.z, s.cell, r);
    let dims = [fx.len(), fy.len(), fz.len()];
    // Each stone its own tone: lighter and darker, warmer and cooler,
    // now and then a greenish damp one.
    let tone = s.color * rng.range(0.7, 1.18);
    let warm = rng.signed() * 0.03;
    let damp = if rng.chance(0.15) { rng.f() * 0.025 } else { 0.0 };
    let tone = (tone + Vec3::new(warm - damp, damp * 0.4, -warm - damp)).max(Vec3::splat(0.01));
    let seed = (s.seed & 0xffff_ffff) as u32;
    let freq = 1.0 / (size.min_element() * 0.45).max(0.02);
    let mut welded: HashMap<[u16; 3], u32> = HashMap::new();
    let mut point = |b: &mut Builder, i: [usize; 3]| -> u32 {
        let key = [i[0] as u16, i[1] as u16, i[2] as u16];
        if let Some(&v) = welded.get(&key) {
            return v;
        }
        let t = Vec3::new(fx[i[0]], fy[i[1]], fz[i[2]]);
        let q = (t - Vec3::splat(0.5)) * size;
        let mut p = if s.round_section {
            // Square to disc in XZ, then round the top and bottom rims.
            let (u, w) = (q.x / half.x, q.z / half.z);
            let (du, dw) = (u * (1.0 - w * w * 0.5).max(0.0).sqrt(), w * (1.0 - u * u * 0.5).max(0.0).sqrt());
            let rho = (du * du + dw * dw).sqrt();
            let (ry, rr) = (q.y, rho * half.x.min(half.z));
            let rc = rr.min(half.x.min(half.z) - r);
            let yc = ry.clamp(-inner.y, inner.y);
            let d = glam::Vec2::new(rr - rc, ry - yc);
            let (rr, ry) = if d.length_squared() > 1e-12 { let e = d.normalize() * r; (rc + e.x, yc + e.y) } else { (rr, ry) };
            let k = if rho > 1e-6 { rr / rho / half.x.min(half.z) } else { 0.0 };
            Vec3::new(du * k * half.x, ry, dw * k * half.z)
        } else {
            let cq = q.clamp(-inner, inner);
            let d = q - cq;
            if d.length_squared() > 1e-12 { cq + d.normalize() * r } else { q }
        };
        for &(n, limit) in &chips {
            let over = p.dot(n) - limit;
            if over > 0.0 {
                p -= n * over;
            }
        }
        // Back to the corners, then out or in along the surface.
        let tp = p / size + Vec3::splat(0.5);
        let world = trilinear(&c, tp.x, tp.y, tp.z);
        let out = {
            let core = if s.round_section {
                let rad = half.x.min(half.z);
                let flat = glam::Vec2::new(p.x / half.x, p.z / half.z) * rad;
                let keep = flat.clamp_length_max((rad - r).max(0.0));
                Vec3::new(keep.x / rad * half.x, p.y.clamp(-inner.y, inner.y), keep.y / rad * half.z)
            } else {
                p.clamp(-inner, inner)
            };
            let d = p - core;
            let d = if d.length_squared() > 1e-10 { d } else { p };
            (axes[0] * d.x + axes[1] * d.y + axes[2] * d.z).normalize_or(Vec3::Y)
        };
        let bump = fbm(world * freq, 3, seed) * 0.7 + noise(world * freq * 4.3, seed ^ 0x55) * 0.3;
        let pit = noise(world * freq * 9.0, seed ^ 0x77).max(0.0).powi(3);
        let pos = world + out * s.rough * (bump - 1.6 * pit);
        // Color: the stone's own tone, mottled, a little darker in pits.
        // Grime settles low on each stone; pits and blotches darken.
        let low = 0.82 + 0.18 * super::smoothstep(0.0, 0.7, tp.y);
        let mottle = (1.0 + 0.2 * fbm(world * freq * 1.6, 3, seed ^ 0x99) - 0.35 * pit) * low;
        let v = b.vertex(pos, out, tone * mottle, s.material);
        welded.insert(key, v);
        v
    };
    // Six faces; quads wind counter-clockwise seen from outside.
    for axis in 0..3 {
        let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
        for side in [0, dims[axis] - 1] {
            for a in 0..dims[u] - 1 {
                for c2 in 0..dims[w] - 1 {
                    let at = |da: usize, dc: usize| {
                        let mut i = [0; 3];
                        i[axis] = side;
                        i[u] = a + da;
                        i[w] = c2 + dc;
                        i
                    };
                    let (p0, p1, p2, p3) = (point(b, at(0, 0)), point(b, at(1, 0)), point(b, at(1, 1)), point(b, at(0, 1)));
                    if (side != 0) != flip { b.quad(p0, p1, p2, p3) } else { b.quad(p0, p3, p2, p1) }
                }
            }
        }
    }
    // The block's 12 edges as its wireframe outline.
    let m = [dims[0] - 1, dims[1] - 1, dims[2] - 1];
    let corner = |k: usize| [if k & 1 == 0 { 0 } else { m[0] }, if k & 2 == 0 { 0 } else { m[1] }, if k & 4 == 0 { 0 } else { m[2] }];
    for (a, z) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
        let (pa, pz) = (point(b, corner(a)), point(b, corner(z)));
        b.edge(pa, pz);
    }
    b.smooth_normals(start);
    start..b.mesh.vertices.len()
}

/// A plain box (no rounding, chips or noise): mortar cores, grout beds.
pub fn plain(b: &mut Builder, lo: Vec3, hi: Vec3, color: Vec3, material: u32) {
    let c = aabb(lo, hi);
    for (axis, sign) in [(0usize, 1.0f32), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0), (2, -1.0)] {
        let mut n = Vec3::ZERO;
        n[axis] = sign;
        let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
        let bit = |k: usize, v: bool| if v { 1 << k } else { 0 };
        let side = sign > 0.0;
        let idx = |a: bool, c2: bool| bit(axis, side) | bit(u, a) | bit(w, c2);
        let quad = [idx(false, false), idx(true, false), idx(true, true), idx(false, true)].map(|i| b.vertex(c[i], n, color, material));
        if side { b.quad(quad[0], quad[1], quad[2], quad[3]) } else { b.quad(quad[0], quad[3], quad[2], quad[1]) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::Material;

    #[test]
    fn a_stone_is_closed_outward_and_stays_near_its_box() {
        let mut b = Builder::new(vec![Material::default()]);
        let lo = Vec3::new(-0.3, 0.0, -0.2);
        let hi = Vec3::new(0.3, 0.35, 0.2);
        let style = Style { chips: 3, ..Style::default() };
        block(&mut b, aabb(lo, hi), &style);
        let m = &b.mesh;
        // Every edge is shared by exactly two triangles: watertight.
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for t in m.indices.chunks_exact(3) {
            for (a, c) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(c), a.max(c))).or_default() += 1;
            }
        }
        assert!(edges.values().all(|&n| n == 2), "open or non-manifold edges");
        // Normals point away from the center; the stone fills its box.
        let center = (lo + hi) * 0.5;
        let out = m.vertices.iter().filter(|v| (Vec3::from(v.pos) - center).dot(Vec3::from(v.normal)) > 0.0).count();
        assert!(out as f32 > m.vertices.len() as f32 * 0.97);
        let (blo, bhi) = m.bounds();
        assert!((blo - lo).abs().max_element() < 0.03 && (bhi - hi).abs().max_element() < 0.03, "{blo} {bhi}");
        // Same seed, same stone.
        let mut again = Builder::new(vec![Material::default()]);
        block(&mut again, aabb(lo, hi), &style);
        assert_eq!(again.mesh.vertices.len(), m.vertices.len());
        assert_eq!(again.mesh.vertices[17].pos, m.vertices[17].pos);
    }
}
