//! Surface texture: the noise brush's patterns (pores, bumps, wrinkles,
//! scales, plates, folds) and whole-area displacement (`displace`). Both
//! only change the surface, so they visit only the detail-grid blocks the
//! surface passes through.
//!
//! Hashes are integer only and the noise has no transcendentals, so the
//! patterns are the same on every platform.

use glam::{IVec3, Vec3};

use super::engine::{Sweep, allowed};
use crate::brush::resolve::Params;
use crate::brush::spec::StrokeMask;
use crate::clay::{Field, LevelView, MAX_OFFSET_VOXELS, par_chunks};

pub(super) fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Overlapping scales, 0..1, about 1 unit apart (`q` in scale units): a
/// cell pattern (each point belongs to its nearest jittered lattice point)
/// where each scale is domed across, rises toward its free edge along
/// `flow` so it overlaps the next one, and is parted from its neighbors by
/// a groove.
pub(super) fn scale_height(q: Vec3, flow: Vec3) -> f32 {
    // Scales are a little longer along the flow.
    let q = q - flow * q.dot(flow) * 0.2;
    let cell = q.floor().as_ivec3();
    let (mut d1, mut d2, mut c1) = (f32::MAX, f32::MAX, Vec3::ZERO);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let c = cell + IVec3::new(x, y, z);
                let jitter = Vec3::new(hash01(c, 1), hash01(c, 2), hash01(c, 3));
                let at = c.as_vec3() + Vec3::splat(0.3) + jitter * 0.4;
                let d = at.distance_squared(q);
                if d < d1 {
                    (d2, d1, c1) = (d1, d, at);
                } else if d < d2 {
                    d2 = d;
                }
            }
        }
    }
    // 0 on the border between two scales.
    let edge = d2.sqrt() - d1.sqrt();
    let local = q - c1;
    let along = local.dot(flow);
    let side = (local - flow * along).length();
    let rise = smooth01(0.5 + along * 1.2);
    let dome = (1.0 - 2.0 * side * side).max(0.0);
    // A sharp step at the free edge (where the scale overlaps the next),
    // a soft groove at the sides.
    let groove = 0.25 - 0.21 * smooth01(along * 3.0);
    (0.15 + 0.85 * rise) * (0.35 + 0.65 * dome) * smooth01(edge / groove)
}

/// Belly plates, 0..1: bands across the flow (`t` in plate widths), each
/// rising toward its free edge and parted by a groove.
pub(super) fn plate_height(t: f32) -> f32 {
    let f = t - t.floor();
    smooth01(f / 0.85) * (1.0 - smooth01((f - 0.9) / 0.1))
}

/// A repeatable 0..1 value for a lattice point (and a channel).
fn hash01(i: IVec3, k: u32) -> f32 {
    let mut h = (i.x as u32).wrapping_mul(0x8da6_b343) ^ (i.y as u32).wrapping_mul(0xd816_3841) ^ (i.z as u32).wrapping_mul(0xcb1a_b31f) ^ k.wrapping_mul(0x9e37_79b9);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

/// Smooth 3D value noise in 0..1 with features about 1 unit apart.
pub(crate) fn noise(p: Vec3) -> f32 {
    let hash = |i: IVec3| -> f32 {
        let mut h = (i.x as u32).wrapping_mul(0x8da6_b343) ^ (i.y as u32).wrapping_mul(0xd816_3841) ^ (i.z as u32).wrapping_mul(0xcb1a_b31f);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h & 0xffff) as f32 / 65535.0
    };
    let f = p.floor();
    let t = p - f;
    let s = t * t * (Vec3::splat(3.0) - 2.0 * t);
    let i = f.as_ivec3();
    let c = |dx: i32, dy: i32, dz: i32| hash(i + IVec3::new(dx, dy, dz));
    let l = |a: f32, b: f32, w: f32| a + (b - a) * w;
    let x00 = l(c(0, 0, 0), c(1, 0, 0), s.x);
    let x10 = l(c(0, 1, 0), c(1, 1, 0), s.x);
    let x01 = l(c(0, 0, 1), c(1, 0, 1), s.x);
    let x11 = l(c(0, 1, 1), c(1, 1, 1), s.x);
    l(l(x00, x10, s.y), l(x01, x11, s.y), s.z)
}

/// The noise brush. It only changes the surface, so it visits only the
/// detail-grid blocks the surface passes through (a face at 0.5 mm would be
/// tens of millions of points as a solid box) and only points within its
/// depth of the surface.
#[allow(clippy::too_many_arguments)]
pub(super) fn noise_band(params: &Params, sweep: &Sweep, normal: Option<Vec3>, mask: Option<&StrokeMask>, gate: &super::engine::Gate, grid: &LevelView, field: &Field, voxel: f32, radius: f32, strength: f32, clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let scale = params.scale.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(radius / 10.0).max(voxel);
    let depth = strength * scale * 0.5;
    let pattern = params.pattern.as_deref().unwrap_or("pores");
    if !matches!(pattern, "pores" | "bumps" | "wrinkles" | "scales" | "plates" | "folds") {
        return Err(format!("unknown pattern {pattern}; use pores, bumps, wrinkles, scales, plates or folds"));
    }
    // Wrinkles run across `normal` (default: horizontal lines); scales and
    // plates point their free edges along it (default: down, like shingles).
    let given = normal.filter(|n| n.is_finite() && n.length() > 1e-6).map(|n| n.normalize());
    let across = given.unwrap_or(if matches!(pattern, "scales" | "plates" | "folds") { Vec3::NEG_Y } else { Vec3::Y });
    let height = |p: Vec3| match pattern {
        "folds" => {
            // Long soft ridges along `across`, all around it: smooth noise
            // over the plane across the folds, barely changing along them,
            // so a tube gets folds on every side that wander a little.
            let (u, v) = across.any_orthonormal_pair();
            let q = Vec3::new(p.dot(u), p.dot(v), p.dot(across) * 0.12) / scale;
            // Crests where the noise crosses its middle: winding lines in
            // that plane, drawn out along the folds into long ridges.
            let n = noise(q * 1.3) * 0.8 + noise(q * 2.7 + 9.0) * 0.2;
            // Ridges only stand out (cloth bunches up); it never digs pits.
            let crest = 1.0 - (n * 2.0 - 1.0).abs() * 1.4;
            smooth01(crest)
        }
        "scales" => scale_height(p / scale, across) - 0.5,
        "plates" => plate_height(p.dot(across) / scale) - 0.5,
        // Pits where the noise peaks; flat skin between.
        "pores" => -smooth01((noise(p / scale) - 0.62) / 0.2),
        "wrinkles" => {
            // Stretch the noise along the lines, sharpen it to creases.
            let q = p - across * p.dot(across) * 0.85;
            -(1.0 - (noise(q / scale) * 2.0 - 1.0).abs()).powi(4)
        }
        _ => noise(p / scale) * 2.0 - 1.0,
    };
    let band = depth + 3.0 * voxel;
    let side = 8.0 * voxel;
    let points = sweep.points;
    let (plo, phi) = points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    let (mut blo, mut bhi) = (plo - radius, phi + radius);
    if let Some((a, b)) = clip {
        (blo, bhi) = (blo.max(a), bhi.min(b));
        if blo.cmpgt(bhi).any() {
            return Ok(Vec::new());
        }
    }
    let (k0, k1) = ((blo / side).floor(), (bhi / side).floor());
    let count = (k1 - k0 + 1.0).max(Vec3::ZERO);
    if !count.is_finite() || count.x * count.y * count.z > 8_000_000.0 {
        return Err(format!("noise stroke is too big for the detail grid (cell size {voxel:.5}); use a smaller radius"));
    }
    let (k0, k1) = (k0.as_ivec3(), k1.as_ivec3());
    // Each block and the new offsets found in it.
    type Block = (IVec3, Vec<([i32; 3], f32)>);
    let mut blocks: Vec<Block> = Vec::new();
    for z in k0.z..=k1.z {
        for y in k0.y..=k1.y {
            for x in k0.x..=k1.x {
                blocks.push((IVec3::new(x, y, z), Vec::new()));
            }
        }
    }
    let max_off = MAX_OFFSET_VOXELS * voxel;
    par_chunks(&mut blocks, 64, |_, chunk| {
        for (key, out) in chunk {
            let lo = key.as_vec3() * side;
            let center = lo + Vec3::splat(side * 0.5);
            if sweep.distance(center) > radius + side {
                continue;
            }
            let carved = grid.carved(*key * 8, *key * 8 + 7);
            let near = field.near(lo - voxel - carved - band, lo + Vec3::splat(side + voxel + carved + band));
            if field.far_side(center, &near, side + band + carved).is_some() {
                continue;
            }
            for i in 0..512 {
                let g = *key * 8 + IVec3::new(i & 7, (i >> 3) & 7, i >> 6);
                let p = g.as_vec3() * voxel;
                let mut w = sweep.weight(&params.curve, p);
                if clip.is_some_and(|(a, b)| p.cmplt(a).any() || p.cmpgt(b).any()) {
                    continue;
                }
                if !allowed(mask, field, p) || w <= 0.0 {
                    continue;
                }
                let t = field.sample(p, &near).0;
                if t.abs() > band {
                    continue;
                }
                if gate.points() {
                    w *= gate.at_near(field, p, t, None);
                    if w <= 0.0 {
                        continue;
                    }
                }
                // Advect the old surface, instead of adding depth to a
                // clamped field (which would cut holes when depth > limit).
                let mut displacement = height(p) * depth * w;
                if displacement.abs() <= 1e-7 {
                    continue;
                }
                let normal = field.sculpt_normal(p, &near, voxel);
                if pattern == "folds" {
                    // Folds hang down the sides; faces turned along the
                    // folds (a sleeve's open end, a shoulder top) stay smooth.
                    displacement *= (1.0 - normal.dot(across).powi(2)).max(0.0).powi(2);
                }
                let source = p - normal * displacement;
                let value = field.sample(source, &field.near(source - voxel, source + voxel)).0;
                let base = field.sample_base(p, &near).0;
                let offset = (value - base).clamp(-max_off, max_off);
                if (offset - grid.get(g.to_array())).abs() > 1e-7 {
                    out.push((g.to_array(), offset));
                }
            }
        }
    });
    Ok(blocks.into_iter().flat_map(|(_, out)| out).collect())
}

/// One level's new offsets for `displace`: the surface moved along its
/// normal by `height(p)` (+ out) inside `lo..hi` (and `clip`), visiting only
/// grid blocks the surface passes through and `near` lets in.
#[allow(clippy::too_many_arguments)]
pub(super) fn displace_band(grid: &LevelView, field: &Field, voxel: f32, lo: Vec3, hi: Vec3, depth: f32, near: &(dyn Fn(Vec3, f32) -> bool + Sync), height: &(dyn Fn(Vec3) -> f32 + Sync), clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let band = depth + 3.0 * voxel;
    let side = 8.0 * voxel;
    let (mut blo, mut bhi) = (lo, hi);
    if let Some((a, b)) = clip {
        (blo, bhi) = (blo.max(a), bhi.min(b));
        if blo.cmpgt(bhi).any() {
            return Ok(Vec::new());
        }
    }
    let (k0, k1) = ((blo / side).floor(), (bhi / side).floor());
    let count = (k1 - k0 + 1.0).max(Vec3::ZERO);
    if !count.is_finite() || count.x * count.y * count.z > 8_000_000.0 {
        return Err(format!("the area is too big for the detail grid (cell size {voxel:.5}); use a larger cell"));
    }
    let (k0, k1) = (k0.as_ivec3(), k1.as_ivec3());
    type Block = (IVec3, Vec<([i32; 3], f32)>);
    let mut blocks: Vec<Block> = Vec::new();
    for z in k0.z..=k1.z {
        for y in k0.y..=k1.y {
            for x in k0.x..=k1.x {
                blocks.push((IVec3::new(x, y, z), Vec::new()));
            }
        }
    }
    let inside = |p: Vec3| p.cmpge(blo).all() && p.cmple(bhi).all();
    let max_off = MAX_OFFSET_VOXELS * voxel;
    par_chunks(&mut blocks, 64, |_, chunk| {
        for (key, out) in chunk {
            let lo = key.as_vec3() * side;
            let center = lo + Vec3::splat(side * 0.5);
            if !near(center, side) {
                continue;
            }
            let carved = grid.carved(*key * 8, *key * 8 + 7);
            let list = field.near(lo - voxel - carved - band, lo + Vec3::splat(side + voxel + carved + band));
            if field.far_side(center, &list, side + band + carved).is_some() {
                continue;
            }
            for i in 0..512 {
                let g = *key * 8 + IVec3::new(i & 7, (i >> 3) & 7, i >> 6);
                let p = g.as_vec3() * voxel;
                if !inside(p) || field.sample(p, &list).0.abs() > band {
                    continue;
                }
                let d = height(p);
                if d.abs() <= 1e-7 {
                    continue;
                }
                // Advect the old surface (see `noise_band`).
                let normal = field.sculpt_normal(p, &list, voxel);
                let source = p - normal * d;
                let value = field.sample(source, &field.near(source - voxel, source + voxel)).0;
                let base = field.sample_base(p, &list).0;
                let offset = (value - base).clamp(-max_off, max_off);
                if (offset - grid.get(g.to_array())).abs() > 1e-7 {
                    out.push((g.to_array(), offset));
                }
            }
        }
    });
    Ok(blocks.into_iter().flat_map(|(_, out)| out).collect())
}
