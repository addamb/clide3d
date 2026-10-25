//! The transform brush (Gizmo 3D with a mask, Transpose): moves, turns
//! and scales the clay's material by an affine map, each point by its
//! gate (1 - mask, 0 in hidden groups). Like grab it is a backward map:
//! the field at `p` becomes the field where the material there came
//! from, `p + w (M^-1 p - p)`, so a fully masked point keeps its value
//! exactly and an unmasked one moves rigidly with the map. `w` is the
//! smaller gate of `p` and of `M^-1 p`, each read on the surface nearest
//! it, so masked material neither moves nor is copied into the air.

use glam::{IVec3, Vec3};

use super::engine::{Gate, allowed};
use crate::brush::resolve::Params;
use crate::brush::spec::StrokeMask;
use crate::clay::{Field, LevelView, MAX_OFFSET_VOXELS, par_chunks};

/// Most 8^3 blocks a transform may visit.
const MAX_BLOCKS: f64 = 8_000_000.0;

/// The box a transform can change: the clay and where the map takes it.
pub(super) fn reach(field: &Field, params: &Params) -> Option<(Vec3, Vec3)> {
    let m = params.transform?;
    let (lo, hi) = field.bounds()?;
    let corners = (0..8).map(|n| Vec3::new(if n & 1 == 0 { lo.x } else { hi.x }, if n & 2 == 0 { lo.y } else { hi.y }, if n & 4 == 0 { lo.z } else { hi.z }));
    let (a, b) = corners.fold((lo, hi), |(a, b), c| {
        let q = m.transform_point3(c);
        (a.min(q), b.max(q))
    });
    let pad = field.limit;
    Some((a - pad, b + pad))
}

/// One level's new offsets for a transform, on grid `grid` of cell
/// `voxel`, only inside the box `clip`.
#[allow(clippy::too_many_arguments)]
pub(super) fn band(params: &Params, mask: Option<&StrokeMask>, gate: &Gate, grid: &LevelView, field: &Field, voxel: f32, clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let m = params.transform.ok_or("a transform stroke needs its map")?;
    let back = m.inverse();
    if !back.is_finite() {
        return Err("the transform must be invertible".into());
    }
    let amount = params.strength.clamp(0.0, 1.0);
    let Some((mut lo, mut hi)) = reach(field, params) else { return Ok(Vec::new()) };
    if let Some((a, b)) = clip {
        (lo, hi) = (lo.max(a), hi.min(b));
        if lo.cmpgt(hi).any() {
            return Ok(Vec::new());
        }
    }
    let side = 8.0 * voxel;
    let (k0, k1) = ((lo / side).floor(), (hi / side).floor());
    let count = (k1 - k0 + 1.0).max(Vec3::ZERO).as_dvec3();
    if !count.is_finite() || count.x * count.y * count.z > MAX_BLOCKS {
        return Err(format!("the transform covers too much of the detail grid (cell size {voxel:.5}); use a larger detail_cell"));
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
    let (limit, max_off) = (field.limit, MAX_OFFSET_VOXELS * voxel);
    par_chunks(&mut blocks, 16, |_, chunk| {
        for (key, out) in chunk {
            let a = key.as_vec3() * side;
            let b = a + Vec3::splat(side - voxel);
            // The block and where its material may come from.
            let (ulo, uhi) = (0..8).fold((a, b), |(l, h), n| {
                let c = Vec3::new(if n & 1 == 0 { a.x } else { b.x }, if n & 2 == 0 { a.y } else { b.y }, if n & 4 == 0 { a.z } else { b.z });
                let q = back.transform_point3(c);
                (l.min(q), h.max(q))
            });
            // Sculpting anywhere in that box (not only in the block) may
            // have moved the surface into or out of it.
            let pad = 2.0 * voxel + grid.carved_in(ulo, uhi);
            let list = field.near(ulo - pad - limit, uhi + pad + limit);
            if list.is_empty() || field.far_side((ulo + uhi) * 0.5, &list, (uhi - ulo).length() * 0.5 + pad).is_some() {
                continue;
            }
            for i in 0..512 {
                let g = *key * 8 + IVec3::new(i & 7, (i >> 3) & 7, i >> 6);
                let p = g.as_vec3() * voxel;
                if clip.is_some_and(|(a, b)| p.cmplt(a).any() || p.cmpgt(b).any()) || !allowed(mask, field, p) {
                    continue;
                }
                let e = Vec3::splat(voxel);
                let here = field.near(p - e, p + e);
                let base = field.sample_base(p, &here).0;
                let old = base + grid.get(g.to_array());
                let whole = back.transform_point3(p);
                let moved = field.sample(whole, &field.near(whole - e, whole + e)).0;
                if (moved - old).abs() <= 1e-7 {
                    continue;
                }
                // Material moves when both where it is going and where it
                // comes from are unmasked (each read on the surface nearest).
                let w = if gate.points() { gate.at_near(field, p, old, None).min(gate.at_near(field, whole, moved, None)) } else { 1.0 } * amount;
                if w <= 0.0 {
                    continue;
                }
                let new = if w >= 1.0 {
                    moved
                } else {
                    let source = p + (whole - p) * w;
                    field.sample(source, &field.near(source - e, source + e)).0
                };
                if (new - old).abs() <= 1e-7 {
                    continue;
                }
                out.push((g.to_array(), (new.clamp(-limit, limit) - base).clamp(-max_off, max_off)));
            }
        }
    });
    Ok(blocks.into_iter().flat_map(|b| b.1).collect())
}
