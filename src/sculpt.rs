//! Sculpt brushes for clay.
//!
//! A stroke edits the clay's detail layer (see `clay::Detail`): a fine grid of
//! offsets added to the blob field. Each stroke:
//!
//! 1. reads the current field T (blobs + offsets) on the grid points around
//!    the stroke,
//! 2. computes the new field T' with the brush,
//! 3. stores T' minus the blob field back as offsets.
//!
//! Brushes, with `w` = a smooth falloff from 1 on the stroke to 0 at `radius`:
//! - add / carve: melt on / cut away a tube of `radius` along the points.
//! - inflate / deflate: move the surface out / in by up to radius * strength / 2.
//! - smooth: blur the field (removes bumps and creases).
//! - flatten: pull the surface onto a plane through the stroke.
//! - grab: pull the surface by `delta` (the nose forward, a chin down).
//!   Strength does not apply; `delta` is the exact pull, at most `radius`.
//! - crease: sharp V groove, with signed scene-unit depth; invert gives a ridge.
//! - pinch: compress tangential features toward the path.
//! - layer: fixed signed height with a flat influence plateau per stroke.
//! - stretch/extrude: continuous tapered transport around a moving path.
//! - noise: fine texture over an area: `pattern` "pores" (small pits, like
//!   skin), "bumps" (soft lumps) or "wrinkles" (fine lines across `normal`),
//!   with features `scale` apart, as deep as strength x scale / 2.
//!
//! All brushes support named-blob ownership and object-local height masks.
//!
//! The blobs stay editable. Offsets stay where they are in space, so moving
//! a blob does not move detail sculpted on it.

use glam::{IVec3, Vec3};
use serde::Deserialize;

use crate::clay::{Clay, Detail, Field, LevelView, MAX_BRICKS, MAX_OFFSET_VOXELS, par_chunks};

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Brush {
    Add,
    Carve,
    Inflate,
    Deflate,
    Smooth,
    Flatten,
    Grab,
    Noise,
    Crease,
    Pinch,
    Layer,
    #[serde(alias = "extrude")]
    Stretch,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Stroke {
    pub brush: Brush,
    /// One point (a dab) or a path, in the clay object's space.
    pub points: Vec<Vec3>,
    pub radius: f32,
    /// 0..1 for traditional brushes; scene-unit depth/height for crease/layer.
    #[serde(default = "default_strength")]
    pub strength: f32,
    /// grab: how far to pull (at most `radius`).
    #[serde(default)]
    pub delta: Option<Vec3>,
    /// flatten: plane normal (default: the surface normal at the stroke).
    #[serde(default)]
    pub normal: Option<Vec3>,
    /// Also apply the stroke mirrored across X = 0 (object space).
    #[serde(default)]
    pub mirror: bool,
    /// noise: feature spacing (default radius / 10).
    #[serde(default)]
    pub scale: Option<f32>,
    /// noise: pores, bumps, wrinkles, scales, plates or folds (default pores).
    #[serde(default)]
    pub pattern: Option<String>,
    /// Reverse crease depth or layer height.
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub mask: Option<Mask>,
}

/// Hard limits evaluated on the pre-stroke field, in object space.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    #[serde(default)]
    pub blobs: Option<Vec<String>>,
    pub above: Option<f32>,
    pub below: Option<f32>,
}

fn allowed(stroke: &Stroke, field: &Field, p: Vec3) -> bool {
    stroke.mask.as_ref().is_none_or(|m| {
        !m.above.is_some_and(|y| p.y < y) && !m.below.is_some_and(|y| p.y > y)
            && m.blobs.as_ref().is_none_or(|names| field.nearest_named(p, names))
    })
}

fn closest(p: Vec3, points: &[Vec3]) -> Vec3 {
    if points.len() == 1 { return points[0]; }
    points.windows(2).map(|w| {
        let ab = w[1] - w[0];
        w[0] + ab * ((p - w[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0)
    }).min_by(|a, b| p.distance_squared(*a).total_cmp(&p.distance_squared(*b))).unwrap()
}

/// Compose small injective backward maps around a moving anchor. The
/// displacement Lipschitz constant of each map is below 1/2, so neither
/// it nor the composition can fold or detach a continuous surface.
/// Evaluating backwards advects the old field, with exact tip displacement.
fn stretch_source(p: Vec3, points: &[Vec3], delta: Vec3, radius: f32) -> Vec3 {
    let steps = (delta.length() * 8.0 / radius).ceil().max(1.0) as usize;
    let step = delta / steps as f32;
    let mut q = p;
    for j in (1..=steps).rev() {
        let shift = step * j as f32;
        q -= step * falloff(path_distance(q - shift, points) / radius);
    }
    q
}

impl Stroke {
    /// The same stroke reflected across X = 0.
    pub fn mirrored(&self) -> Stroke {
        let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
        Stroke {
            points: self.points.iter().map(|p| flip(*p)).collect(),
            delta: self.delta.map(flip),
            normal: self.normal.map(flip),
            mirror: false,
            ..self.clone()
        }
    }
}

fn default_strength() -> f32 {
    0.5
}

/// Most grid points one stroke reads at once; bigger strokes go in tiles.
const MAX_REGION: usize = 6_000_000;
/// Most grid points a tiled stroke may read in all.
const MAX_VISIT: usize = 60_000_000;
/// Largest box (in grid points) a stroke may span; only tiles near the
/// surface inside it are read.
const MAX_BOX: f64 = 4.0e9;
const MAX_POINTS: usize = 512;

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Overlapping scales, 0..1, about 1 unit apart (`q` in scale units): a
/// cell pattern (each point belongs to its nearest jittered lattice point)
/// where each scale is domed across, rises toward its free edge along
/// `flow` so it overlaps the next one, and is parted from its neighbors by
/// a groove.
fn scale_height(q: Vec3, flow: Vec3) -> f32 {
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
fn plate_height(t: f32) -> f32 {
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

/// Smooth falloff: 1 at 0, 0 at 1 and beyond.
fn falloff(t: f32) -> f32 {
    let x = (1.0 - t * t).max(0.0);
    x * x
}

fn segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-12 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
    (p - (a + ab * t)).length()
}

fn path_distance(p: Vec3, points: &[Vec3]) -> f32 {
    if points.len() == 1 {
        return (p - points[0]).length();
    }
    points.windows(2).map(|w| segment_distance(p, w[0], w[1])).fold(f32::MAX, f32::min)
}

/// The detail grid spacing a clay gets on its first stroke: its
/// `detail_cell`, else its mesh cell size.
fn first_voxel(clay: &Clay, radius: f32) -> f32 {
    clay.detail_cell.or_else(|| clay.cell_size()).unwrap_or(radius / 6.0)
}

/// What a stroke did, for the agent.
pub struct Report {
    pub changed_points: usize,
    /// The radius used, after raising it to at least 1.5 detail cells.
    pub radius: f32,
}

/// Applies one stroke to the clay's detail layer.
pub fn apply(clay: &mut Clay, stroke: &Stroke) -> Result<Report, String> {
    if stroke.points.is_empty() || stroke.points.len() > MAX_POINTS {
        return Err(format!("a stroke needs 1 to {MAX_POINTS} points"));
    }
    if !stroke.points.iter().all(|p| p.is_finite()) || !stroke.radius.is_finite() || stroke.radius <= 0.0 {
        return Err("points and radius must be finite, radius > 0".into());
    }
    if !stroke.strength.is_finite() { return Err("strength must be finite".into()); }
    if let Some(m) = &stroke.mask {
        if m.above.is_some_and(|v| !v.is_finite()) || m.below.is_some_and(|v| !v.is_finite()) { return Err("mask heights must be finite".into()); }
        if let Some(names) = &m.blobs {
            for name in names { if !clay.blobs.iter().any(|b| &b.name == name) { return Err(format!("unknown mask blob {name}")); } }
        }
    }
    let strength = if matches!(stroke.brush, Brush::Crease | Brush::Layer) { stroke.strength } else { stroke.strength.clamp(0.0, 1.0) };

    let voxel = clay.detail.as_ref().map_or_else(|| first_voxel(clay, stroke.radius), Detail::voxel);
    if !voxel.is_finite() || voxel <= 0.0 {
        return Err("clay is too large or broken for sculpting".into());
    }
    let mut detail = clay.detail.clone().unwrap_or_else(|| Detail::new(voxel));
    let field = {
        let probe = Clay { blobs: clay.blobs.clone(), resolution: clay.resolution, detail: Some(detail.clone()), paint: None, detail_cell: clay.detail_cell, shell: clay.shell, sharp: clay.sharp, ..Clay::default() };
        Field::new(&probe, voxel)
    };
    // The coarse grid always takes the stroke; so does every refined
    // level it reaches, each at its own cell (see `clay::Region`).
    let (plo, phi) = stroke.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    let reach = stroke.radius * 2.0 + stroke.delta.map_or(0.0, |d| d.length()) + if matches!(stroke.brush, Brush::Crease | Brush::Layer) { strength.abs() } else { 0.0 };
    let mut targets: Vec<Option<usize>> = vec![None];
    for (k, l) in detail.grid().levels().iter().enumerate() {
        let (lo, hi) = l.extent(0.0);
        if (plo - reach).cmple(hi).all() && (phi + reach).cmpge(lo).all() {
            targets.push(Some(k));
        }
    }
    // Noise finer than a grid can hold (a pore pattern on a 2 mm grid)
    // would come out as coarse lumps; when the stroke reaches a level fine
    // enough for it, coarser levels leave it out (it fades out at that
    // region's edge instead).
    if stroke.brush == Brush::Noise && targets.len() > 1 {
        let scale = stroke.scale.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(stroke.radius / 10.0);
        let cell = |t: &Option<usize>| t.map_or(detail.voxel(), |k| detail.grid().levels()[k].grid.voxel);
        let fits = |t: &Option<usize>| scale >= 2.0 * cell(t);
        if targets.iter().any(fits) {
            targets.retain(fits);
        }
    }
    let mut report = Report { changed_points: 0, radius: stroke.radius };
    for target in targets {
        let (changes, radius) = {
            let view = LevelView::new(detail.grid(), target);
            let clip = target.map(|k| detail.grid().levels()[k].extent(0.0));
            let v = view.voxel();
            let radius = stroke.radius.max(v * 1.5);
            let changes = if stroke.brush == Brush::Noise {
                noise_band(stroke, &view, &field, v, radius, strength, clip)?
            } else {
                brush(stroke, &view, &field, v, radius, strength, clip)?
            };
            // A refined level stores what it adds to the levels under it
            // (already holding this stroke).
            let changes = match target {
                None => changes,
                Some(k) => {
                    let g = detail.grid();
                    changes.into_iter().map(|(i, full)| (i, full - g.parent(k, Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v))).collect()
                }
            };
            (changes, radius)
        };
        report.changed_points += changes.len();
        report.radius = radius;
        let g = match target {
            None => detail.grid_mut(),
            Some(k) => &mut detail.grid_mut().levels_mut()[k].grid,
        };
        for (gi, v) in changes {
            g.set(gi, v);
        }
        g.prune();
        if g.brick_count() > MAX_BRICKS {
            return Err(format!("too much sculpt detail (over {MAX_BRICKS} bricks{}); undo some, use a larger cell or a smaller area", if target.is_some() { " in one detail region" } else { "" }));
        }
    }
    if detail.grid().total_bricks() > crate::clay::MAX_TOTAL_BRICKS {
        return Err(format!("too much sculpt detail (over {} bricks in all); undo some, or use larger cells or smaller detail regions", crate::clay::MAX_TOTAL_BRICKS));
    }
    clay.detail = Some(detail);
    Ok(report)
}

/// One level's new offsets for a brush stroke (all brushes but noise), on
/// grid `grid` of cell `voxel`, only inside the box `clip`.
#[allow(clippy::too_many_arguments)]
fn brush(stroke: &Stroke, grid: &LevelView, field: &Field, voxel: f32, radius: f32, strength: f32, clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let limit = field.limit;

    let delta = match stroke.brush {
        Brush::Grab | Brush::Stretch => {
            let d = stroke.delta.ok_or("grab/stretch needs delta [x, y, z]")?;
            if !d.is_finite() {
                return Err("delta must be finite".into());
            }
            // Pulling farther than the brush folds the surface over itself.
            if stroke.brush == Brush::Stretch {
                if d.length() > radius * 8.0 { return Err("stretch delta must be at most 8 radii; use successive strokes".into()); }
                d
            } else { d.clamp_length_max(radius) }
        }
        _ => Vec3::ZERO,
    };
    let smooth_passes = 1 + (strength.clamp(0.0, 1.0) * 7.0).round() as usize;

    // Region of grid points the brush can read or change.
    let margin = match stroke.brush {
        Brush::Add | Brush::Carve => radius * 1.6,
        Brush::Smooth => radius + (smooth_passes + 1) as f32 * voxel,
        Brush::Grab | Brush::Stretch => radius + delta.length(),
        Brush::Crease | Brush::Layer => radius + strength.abs(),
        _ => radius,
    } + 2.0 * voxel;
    let (plo, phi) = stroke.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    let (mut blo, mut bhi) = (plo - margin, phi + margin);
    if let Some((a, b)) = clip {
        (blo, bhi) = (blo.max(a), bhi.min(b));
        if blo.cmpgt(bhi).any() {
            return Ok(Vec::new());
        }
    }
    // Check the size in floating point first, so huge strokes cannot overflow.
    let cells = ((bhi - blo) / voxel + 2.0).as_dvec3();
    if !cells.is_finite() || cells.x * cells.y * cells.z > MAX_BOX {
        let most = (MAX_BOX.cbrt() as f32 - 2.0) * voxel / 4.0;
        return Err(format!("stroke is too big for the detail grid (cell size {voxel:.4}): keep radius under about {most:.3} for a dab, or use a shorter path"));
    }
    let i0 = (blo / voxel).floor().as_ivec3();
    let i1 = (bhi / voxel).ceil().as_ivec3();

    let normal = match stroke.brush {
        Brush::Flatten | Brush::Crease | Brush::Layer | Brush::Pinch => match stroke.normal {
            Some(n) if n.is_finite() && n.length() > 1e-6 => n.normalize(),
            _ => {
                let c = stroke.points.iter().copied().sum::<Vec3>() / stroke.points.len() as f32;
                let list = field.near(c - radius, c + radius);
                let f = |p: Vec3| field.sample(p, &list).0;
                let e = voxel;
                Vec3::new(f(c + Vec3::X * e) - f(c - Vec3::X * e), f(c + Vec3::Y * e) - f(c - Vec3::Y * e), f(c + Vec3::Z * e) - f(c - Vec3::Z * e)).normalize_or(Vec3::Y)
            }
        },
        _ => Vec3::Y,
    };
    let job = Job { stroke, field, grid, voxel, radius, strength, delta, smooth_passes, normal };
    let n = i1 - i0 + 1;
    let changes = if (n.x as f64) * (n.y as f64) * (n.z as f64) <= MAX_REGION as f64 {
        job.region(i0, i1, i0, i1)
    } else {
        // Too big to hold at once: tiles, skipping those far from the
        // surface (a brush only matters where the surface is).
        let reach = match stroke.brush {
            Brush::Grab | Brush::Stretch => delta.length(),
            Brush::Crease | Brush::Layer => strength.abs(),
            Brush::Pinch => radius,
            Brush::Inflate | Brush::Deflate => radius * 0.5 * strength,
            Brush::Add | Brush::Carve => radius * 2.0,
            Brush::Flatten => radius,
            _ => 0.0,
        } + limit + 3.0 * voxel;
        let halo = if stroke.brush == Brush::Smooth { smooth_passes as i32 + 1 } else { 1 };
        let tiles: Vec<IVec3> = (i0.z..=i1.z)
            .step_by(TILE as usize)
            .flat_map(|z| (i0.y..=i1.y).step_by(TILE as usize).flat_map(move |y| (i0.x..=i1.x).step_by(TILE as usize).map(move |x| IVec3::new(x, y, z))))
            .collect();
        let mut out = Vec::new();
        let mut visited = 0usize;
        for c0 in tiles {
            let c1 = (c0 + TILE - 1).min(i1);
            let (lo, hi) = (c0.as_vec3() * voxel, c1.as_vec3() * voxel);
            let center = (lo + hi) * 0.5;
            let half = (hi - lo).length() * 0.5;
            // Not touched by the brush, or surely far from the surface.
            if box_path_gap(lo, hi, &stroke.points) > margin {
                continue;
            }
            let carved = grid.carved(c0 - halo, c1 + halo);
            let list = field.near(lo - reach - half - carved, hi + reach + half + carved);
            if field.far_side(center, &list, half + reach + carved).is_some() {
                continue;
            }
            visited += 1;
            if visited * ((TILE + 2 * halo) as usize).pow(3) > MAX_VISIT {
                let most = radius * 0.5;
                return Err(format!("stroke is too big for the detail grid (cell size {voxel:.4}); split it into strokes of radius about {most:.3}"));
            }
            out.extend(job.region(c0 - halo, c1 + halo, c0, c1));
        }
        out
    };

    Ok(changes)
}

/// Tile side (grid points) for strokes too big to hold at once.
const TILE: i32 = 32;

/// Gap between a box and the nearest stroke point's segment path.
fn box_path_gap(lo: Vec3, hi: Vec3, points: &[Vec3]) -> f32 {
    let c = (lo + hi) * 0.5;
    (path_distance(c, points) - (hi - lo).length() * 0.5).max(0.0)
}

/// One stroke's brush, applied region by region.
struct Job<'a> {
    stroke: &'a Stroke,
    field: &'a Field,
    /// The detail offsets before the stroke.
    grid: &'a LevelView<'a>,
    voxel: f32,
    radius: f32,
    strength: f32,
    delta: Vec3,
    smooth_passes: usize,
    /// flatten: the plane's normal.
    normal: Vec3,
}

impl Job<'_> {
    /// Reads the field on grid points `i0..=i1`, applies the brush, and
    /// returns the new offsets of changed points within `c0..=c1`.
    fn region(&self, i0: IVec3, i1: IVec3, c0: IVec3, c1: IVec3) -> Vec<([i32; 3], f32)> {
        let (stroke, field, voxel, radius, strength, delta) = (self.stroke, self.field, self.voxel, self.radius, self.strength, self.delta);
        let limit = field.limit;
        let n = (i1 - i0 + 1).max(IVec3::ONE);
        let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
        let point = |x: usize, y: usize, z: usize| (i0 + IVec3::new(x as i32, y as i32, z as i32)).as_vec3() * voxel;
        let idx = |x: usize, y: usize, z: usize| x + nx * (y + ny * z);

        // Current blob field and total field on the region.
        let near = field.near(point(0, 0, 0) - voxel - delta.length(), point(nx - 1, ny - 1, nz - 1) + voxel + delta.length());
        let mut base = vec![0.0f32; nx * ny * nz];
        par_chunks(&mut base, nx * ny, |z, layer| {
            for y in 0..ny {
                for x in 0..nx {
                    layer[x + nx * y] = field.sample_base(point(x, y, z), &near).0;
                }
            }
        });
        let mut t = vec![0.0f32; base.len()];
        let mut w = vec![0.0f32; base.len()];
        let mut dist = vec![0.0f32; base.len()];
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let i = idx(x, y, z);
                    let g = i0 + IVec3::new(x as i32, y as i32, z as i32);
                    t[i] = base[i] + self.grid.get(g.to_array());
                    let d = path_distance(point(x, y, z), &stroke.points);
                    dist[i] = d;
                    w[i] = if allowed(stroke, field, point(x, y, z)) { falloff(d / radius) } else { 0.0 };
                }
            }
        }

        let mut out = t.clone();
        match stroke.brush {
            Brush::Inflate | Brush::Deflate => {
                let sign = if stroke.brush == Brush::Inflate { -1.0 } else { 1.0 };
                for i in 0..out.len() {
                    out[i] += sign * w[i] * strength * radius * 0.5;
                }
            }
            Brush::Add | Brush::Carve => {
                let k = radius * 0.5;
                for i in 0..out.len() {
                    let tube = dist[i] - radius;
                    let d = t[i];
                    let v = if stroke.brush == Brush::Add {
                        let h = (0.5 + 0.5 * (d - tube) / k).clamp(0.0, 1.0);
                        d * (1.0 - h) + tube * h - k * h * (1.0 - h)
                    } else {
                        let h = (0.5 - 0.5 * (d + tube) / k).clamp(0.0, 1.0);
                        d * (1.0 - h) - tube * h + k * h * (1.0 - h)
                    };
                    out[i] = d + (v - d) * strength;
                }
            }
            Brush::Crease | Brush::Layer | Brush::Pinch | Brush::Stretch => {
                for z in 0..nz { for y in 0..ny { for x in 0..nx {
                    let i = idx(x, y, z);
                    let p = point(x, y, z);
                    if !allowed(stroke, field, p) { continue; }
                    let sign = if stroke.invert { -1.0 } else { 1.0 };
                    out[i] = match stroke.brush {
                        Brush::Crease => {
                            let v = p - closest(p, &stroke.points);
                            let side = (v - self.normal * v.dot(self.normal)).length();
                            let band = 1.0 - smooth01((v.dot(self.normal).abs() - strength.abs() - voxel * 2.0) / radius);
                            let source = p + self.normal * (sign * strength * (1.0 - side / radius).max(0.0) * band);
                            field.sample(source, &field.near(source - voxel, source + voxel)).0
                        },
                        // One target per stroke, rather than accumulating dabs.
                        Brush::Layer => {
                            let v = p - closest(p, &stroke.points);
                            let side = (v - self.normal * v.dot(self.normal)).length();
                            let band = 1.0 - smooth01((v.dot(self.normal).abs() - strength.abs() - voxel * 2.0) / radius);
                            let source = p - self.normal * (sign * strength * (1.0 - smooth01((side / radius - 0.65) / 0.35)) * band);
                            field.sample(source, &field.near(source - voxel, source + voxel)).0
                        },
                        Brush::Pinch => {
                            let q = closest(p, &stroke.points);
                            let e = Vec3::splat(voxel);
                            let n = self.normal;
                            let side = p - q - n * (p - q).dot(n);
                            let source = p + side * w[i] * strength * 2.0;
                            field.sample(source, &field.near(source - e, source + e)).0
                        }
                        Brush::Stretch => {
                            let source = stretch_source(p, &stroke.points, delta, radius);
                            field.sample(source, &field.near(source - voxel, source + voxel)).0
                        }
                        _ => unreachable!(),
                    };
                } } }
            }
            Brush::Smooth => {
                for _ in 0..self.smooth_passes {
                    let prev = out.clone();
                    for z in 1..nz.saturating_sub(1) {
                        for y in 1..ny.saturating_sub(1) {
                            for x in 1..nx.saturating_sub(1) {
                                let i = idx(x, y, z);
                                if w[i] <= 0.0 {
                                    continue;
                                }
                                let sum = prev[idx(x - 1, y, z)] + prev[idx(x + 1, y, z)] + prev[idx(x, y - 1, z)]
                                    + prev[idx(x, y + 1, z)] + prev[idx(x, y, z - 1)] + prev[idx(x, y, z + 1)];
                                let avg = (sum + prev[i]) / 7.0;
                                out[i] = prev[i] + (avg - prev[i]) * w[i];
                            }
                        }
                    }
                }
            }
            Brush::Flatten => {
                let c = stroke.points.iter().copied().sum::<Vec3>() / stroke.points.len() as f32;
                for z in 0..nz {
                    for y in 0..ny {
                        for x in 0..nx {
                            let i = idx(x, y, z);
                            let plane = (point(x, y, z) - c).dot(self.normal);
                            out[i] = t[i] + (plane - t[i]) * w[i] * strength;
                        }
                    }
                }
            }
            // Handled before the region is read (see `noise_band`).
            Brush::Noise => {}
            Brush::Grab => {
                // Each point takes the old value from where the pull came
                // from: from the region when it is there, else the field.
                let sample = |p: Vec3| -> f32 {
                    let g = p / voxel - i0.as_vec3();
                    if g.cmplt(Vec3::ZERO).any() || g.cmpgt((n - 1).as_vec3()).any() {
                        return field.sample(p, &near).0;
                    }
                    let f = g.floor();
                    let (fx, fy, fz) = (f.x as usize, f.y as usize, f.z as usize);
                    let (x1, y1, z1) = ((fx + 1).min(nx - 1), (fy + 1).min(ny - 1), (fz + 1).min(nz - 1));
                    let r = g - f;
                    let l = |a: f32, b: f32, s: f32| a + (b - a) * s;
                    let c00 = l(t[idx(fx, fy, fz)], t[idx(x1, fy, fz)], r.x);
                    let c10 = l(t[idx(fx, y1, fz)], t[idx(x1, y1, fz)], r.x);
                    let c01 = l(t[idx(fx, fy, z1)], t[idx(x1, fy, z1)], r.x);
                    let c11 = l(t[idx(fx, y1, z1)], t[idx(x1, y1, z1)], r.x);
                    l(l(c00, c10, r.y), l(c01, c11, r.y), r.z)
                };
                let anchor = stroke.points[0];
                for z in 0..nz {
                    for y in 0..ny {
                        for x in 0..nx {
                            let p = point(x, y, z);
                            let wg = falloff((p - anchor).length() / radius);
                            if wg > 0.0 {
                                out[idx(x, y, z)] = sample(p - delta * wg);
                            }
                        }
                    }
                }
            }
        }

        let max_off = MAX_OFFSET_VOXELS * voxel;
        let mut changes = Vec::new();
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let gi = i0 + IVec3::new(x as i32, y as i32, z as i32);
                    let i = idx(x, y, z);
                    if !allowed(stroke, field, point(x, y, z)) || gi.cmplt(c0).any() || gi.cmpgt(c1).any() || (out[i] - t[i]).abs() <= 1e-7 {
                        continue;
                    }
                    changes.push((gi.to_array(), (out[i].clamp(-limit, limit) - base[i]).clamp(-max_off, max_off)));
                }
            }
        }
        changes
    }
}

/// The noise brush. It only changes the surface, so it visits only the
/// detail-grid blocks the surface passes through (a face at 0.5 mm would be
/// tens of millions of points as a solid box) and only points within its
/// depth of the surface.
#[allow(clippy::too_many_arguments)]
fn noise_band(stroke: &Stroke, grid: &LevelView, field: &Field, voxel: f32, radius: f32, strength: f32, clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let scale = stroke.scale.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(radius / 10.0).max(voxel);
    let depth = strength * scale * 0.5;
    let pattern = stroke.pattern.as_deref().unwrap_or("pores");
    if !matches!(pattern, "pores" | "bumps" | "wrinkles" | "scales" | "plates" | "folds") {
        return Err(format!("unknown pattern {pattern}; use pores, bumps, wrinkles, scales, plates or folds"));
    }
    // Wrinkles run across `normal` (default: horizontal lines); scales and
    // plates point their free edges along it (default: down, like shingles).
    let given = stroke.normal.filter(|n| n.is_finite() && n.length() > 1e-6).map(|n| n.normalize());
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
    let (plo, phi) = stroke.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
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
            if path_distance(center, &stroke.points) > radius + side {
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
                let w = falloff(path_distance(p, &stroke.points) / radius);
                if clip.is_some_and(|(a, b)| p.cmplt(a).any() || p.cmpgt(b).any()) {
                    continue;
                }
                if !allowed(stroke, field, p) || w <= 0.0 || field.sample(p, &near).0.abs() > band {
                    continue;
                }
                // Advect the old surface, instead of adding depth to a
                // clamped field (which would cut holes when depth > limit).
                let mut displacement = height(p) * depth * w;
                if displacement.abs() <= 1e-7 { continue; }
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
fn displace_band(grid: &LevelView, field: &Field, voxel: f32, lo: Vec3, hi: Vec3, depth: f32, near: &(dyn Fn(Vec3, f32) -> bool + Sync), height: &(dyn Fn(Vec3) -> f32 + Sync), clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
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

/// Displaces a clay's surface by `height(p)` (scene units, + out) inside
/// the box `lo..hi`: whole-area detail such as a face's folds
/// (`face_detail`). `depth` bounds |height|; `near(block center, block
/// side)` skips blocks the height cannot reach. Like a stroke, it goes into
/// the coarse grid and every detail region it reaches.
pub fn displace(clay: &mut Clay, lo: Vec3, hi: Vec3, depth: f32, near: &(dyn Fn(Vec3, f32) -> bool + Sync), height: &(dyn Fn(Vec3) -> f32 + Sync)) -> Result<Report, String> {
    let voxel = clay.detail.as_ref().map_or_else(|| first_voxel(clay, (hi - lo).max_element()), Detail::voxel);
    if !voxel.is_finite() || voxel <= 0.0 || !lo.is_finite() || !hi.is_finite() {
        return Err("clay is too large or broken for sculpting".into());
    }
    let mut detail = clay.detail.clone().unwrap_or_else(|| Detail::new(voxel));
    let field = {
        let probe = Clay { blobs: clay.blobs.clone(), resolution: clay.resolution, detail: Some(detail.clone()), paint: None, detail_cell: clay.detail_cell, shell: clay.shell, sharp: clay.sharp, ..Clay::default() };
        Field::new(&probe, voxel)
    };
    let mut targets: Vec<Option<usize>> = vec![None];
    for (k, l) in detail.grid().levels().iter().enumerate() {
        let (a, b) = l.extent(0.0);
        if lo.cmple(b).all() && hi.cmpge(a).all() {
            targets.push(Some(k));
        }
    }
    let mut report = Report { changed_points: 0, radius: 0.0 };
    for target in targets {
        let changes = {
            let view = LevelView::new(detail.grid(), target);
            let clip = target.map(|k| detail.grid().levels()[k].extent(0.0));
            let changes = displace_band(&view, &field, view.voxel(), lo, hi, depth, near, height, clip)?;
            match target {
                None => changes,
                Some(k) => {
                    let g = detail.grid();
                    let v = view.voxel();
                    changes.into_iter().map(|(i, full)| (i, full - g.parent(k, Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v))).collect()
                }
            }
        };
        report.changed_points += changes.len();
        let g = match target {
            None => detail.grid_mut(),
            Some(k) => &mut detail.grid_mut().levels_mut()[k].grid,
        };
        for (gi, v) in changes {
            g.set(gi, v);
        }
        g.prune();
        if g.brick_count() > MAX_BRICKS {
            return Err(format!("too much sculpt detail (over {MAX_BRICKS} bricks); use a larger detail_cell"));
        }
    }
    if detail.grid().total_bricks() > crate::clay::MAX_TOTAL_BRICKS {
        return Err(format!("too much sculpt detail (over {} bricks in all); use larger cells", crate::clay::MAX_TOTAL_BRICKS));
    }
    clay.detail = Some(detail);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finer_detail_cell_keeps_the_sculpt_and_shows_on_the_fine_mesh() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 1.0)).unwrap();
        let before = max_y(&c);
        let at = Vec3::new(0.05, 1.0, 0.02);
        let old = c.detail.as_ref().unwrap().grid().offset(at);
        c.set_detail_cell(0.01).unwrap();
        let new = c.detail.as_ref().unwrap().grid().offset(at);
        assert!((old - new).abs() < 0.01 * old.abs().max(0.01), "{old} {new}");
        assert!((max_y(&c) - before).abs() < 0.02);
        // The fine mesh has far more vertices than the regular one.
        let fine = c.build_fine(&[], "").unwrap();
        // Too fine for the clay's size: a clear error, not an empty mesh.
        let mut huge = c.clone();
        huge.detail_cell = Some(0.0005);
        let Err(e) = huge.build_fine(&[], "") else { panic!("built a fine mesh too big") };
        assert!(e.contains("detail_cell >="), "{e}");
        assert!(fine.vertices.len() > 4 * c.build_mesh().vertices.len());
        assert!(c.set_detail_cell(0.0).is_err());
    }

    #[test]
    fn noise_pores_dent_only_the_surface_area_under_the_brush() {
        let mut c = ball();
        c.set_detail_cell(0.01).unwrap();
        let mut s = stroke(Brush::Noise, &[[0.0, 0.0, 1.0]], 0.4, 1.0);
        s.scale = Some(0.06);
        let r = apply(&mut c, &s).unwrap();
        assert!(r.changed_points > 100);
        let d = c.detail.as_ref().unwrap().grid();
        // Pores only push in (positive offsets), near the stroke.
        assert!(d.offset(Vec3::new(0.0, 0.0, 1.0)) >= 0.0);
        assert_eq!(d.offset(Vec3::new(0.0, 0.0, -1.0)), 0.0);
        let dented = (0..200).filter(|i| {
            let a = *i as f32 * 0.002 - 0.2;
            d.offset(Vec3::new(a, a * 0.5, (1.0 - a * a * 1.25).sqrt())) > 1e-4
        });
        assert!(dented.count() > 5);
    }

    fn slab() -> Clay {
        serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"box", "name":"slab", "center":[0,-0.3,0], "size":[1.2,0.6,1.2], "blend":0}
        ], "resolution":96, "detail_cell":0.01})).unwrap()
    }

    fn sample(c: &Clay, p: Vec3) -> f32 {
        let f = Field::new(c, c.detail_cell.unwrap_or(0.01));
        f.sample(p, &f.near(p - 0.5, p + 0.5)).0
    }

    fn top(c: &Clay, x: f32, z: f32) -> f32 {
        let (mut lo, mut hi) = (-0.25, 0.3);
        for _ in 0..25 {
            let y = (lo + hi) * 0.5;
            if sample(c, Vec3::new(x, y, z)) < 0.0 { lo = y; } else { hi = y; }
        }
        (lo + hi) * 0.5
    }

    #[test]
    fn crease_is_a_v_at_equal_depth_and_can_invert() {
        let (mut v, mut round) = (slab(), slab());
        let points = [[0.0,0.0,-0.3],[0.0,0.0,0.3]];
        apply(&mut v, &stroke(Brush::Crease, &points, 0.12, 0.1)).unwrap();
        // A circular carve with its center above the surface cuts 0.1 deep.
        apply(&mut round, &stroke(Brush::Carve, &[[0.0,0.02,-0.3],[0.0,0.02,0.3]], 0.12, 1.0)).unwrap();
        assert!((top(&v,0.0,0.0) + 0.1).abs() < 0.012, "crease depth {}",top(&v,0.0,0.0));
        assert!((top(&round,0.0,0.0) + 0.1).abs() < 0.02);
        let sharp = top(&v,0.02,0.0) - top(&v,0.0,0.0);
        let soft = top(&round,0.02,0.0) - top(&round,0.0,0.0);
        assert!(sharp > soft * 2.0, "V slope {sharp}, round slope {soft}");
        let mut ridge = slab();
        let mut s = stroke(Brush::Crease, &points, 0.12, 0.08);
        s.invert = true;
        apply(&mut ridge, &s).unwrap();
        assert!(top(&ridge,0.0,0.0) > 0.065);
        let mut negative = slab(); s.invert = false; s.strength = -0.08;
        apply(&mut negative, &s).unwrap();
        assert!((top(&ridge,0.0,0.0) - top(&negative,0.0,0.0)).abs() < 1e-6);
    }

    #[test]
    fn layer_has_one_height_despite_overlapping_dabs() {
        let (mut one, mut many) = (slab(), slab());
        apply(&mut one, &stroke(Brush::Layer, &[[0.0,0.0,0.0]], 0.2, 0.08)).unwrap();
        apply(&mut many, &stroke(Brush::Layer, &[[0.0,0.0,0.0];20], 0.2, 0.08)).unwrap();
        assert!((top(&one,0.0,0.0) - 0.08).abs() < 0.01);
        assert!((top(&many,0.0,0.0) - top(&one,0.0,0.0)).abs() < 1e-6);
        assert!((top(&many,0.08,0.0) - top(&many,0.0,0.0)).abs() < 0.002);
        let mut lower = slab();
        apply(&mut lower, &stroke(Brush::Layer, &[[0.0,0.0,0.0]], 0.2, -0.08)).unwrap();
        assert!(top(&lower,0.0,0.0) < -0.065);
    }

    #[test]
    fn pinch_narrows_a_soft_ridge() {
        let mut c = slab();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0,0.0,-0.3],[0.0,0.0,0.3]], 0.2, 1.0)).unwrap();
        let before = top(&c,0.09,0.0);
        apply(&mut c, &stroke(Brush::Pinch, &[[0.0,0.1,-0.3],[0.0,0.1,0.3]], 0.2, 0.7)).unwrap();
        assert!(top(&c,0.09,0.0) < before * 0.8, "before {before}, after {}",top(&c,0.09,0.0));
        assert!(top(&c,0.0,0.0) > 0.07, "ridge center should remain");
    }

    #[test]
    fn new_brushes_match_between_whole_regions_and_masked_tiles() {
        let mut c = slab();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0,0.0,0.0]],0.18,0.5)).unwrap();
        let voxel = 0.01;
        let field = Field::new(&c,voxel);
        let view = LevelView::new(c.detail.as_ref().unwrap().grid(), None);
        let grid = &view;
        let (lo, hi) = (IVec3::splat(-22),IVec3::splat(22));
        for brush in [Brush::Crease,Brush::Pinch,Brush::Layer,Brush::Stretch] {
            let mut s = stroke(brush,&[[0.0,0.04,-0.1],[0.0,0.04,0.1]],0.12,0.035);
            s.mask = Some(Mask {blobs:Some(vec!["slab".into()]),above:Some(-0.1),below:Some(0.18)});
            let job = Job {stroke:&s,field:&field,grid,voxel,radius:s.radius,strength:s.strength,
                delta:Vec3::new(0.02,0.08,0.0),smooth_passes:1,normal:Vec3::Y};
            let whole: std::collections::HashMap<_,_> = job.region(lo,hi,lo,hi).into_iter().collect();
            let mut tiled = std::collections::HashMap::new();
            for z in (lo.z..=hi.z).step_by(TILE as usize) { for y in (lo.y..=hi.y).step_by(TILE as usize) { for x in (lo.x..=hi.x).step_by(TILE as usize) {
                let a = IVec3::new(x,y,z); let b = (a+TILE-1).min(hi);
                tiled.extend(job.region(a-1,b+1,a,b));
            } } }
            assert_eq!(whole.len(),tiled.len(),"{brush:?}");
            assert!(whole.iter().all(|(key,value)| tiled.get(key).is_some_and(|v| (v-value).abs()<1e-6)),"{brush:?} tile seams");
        }
    }

    fn components(mesh: &crate::mesh::Mesh) -> usize {
        let geometry=mesh.welded_geometry().map(|(mesh,_)|mesh);
        let mesh=geometry.as_ref().unwrap_or(mesh);
        let mut links = vec![Vec::new();mesh.vertices.len()];
        for t in mesh.indices.chunks_exact(3) {
            for i in 0..3 { links[t[i] as usize].push(t[(i+1)%3] as usize); links[t[(i+1)%3] as usize].push(t[i] as usize); }
        }
        let mut seen = vec![false; links.len()];
        let mut count = 0;
        for i in 0..links.len() {
            if seen[i] || links[i].is_empty() { continue; }
            count += 1;
            let mut todo = vec![i]; seen[i] = true;
            while let Some(j) = todo.pop() { for &k in &links[j] { if !seen[k] { seen[k] = true; todo.push(k); } } }
        }
        count
    }

    #[test]
    fn stretch_extends_a_thin_tip_without_mesh_droplets() {
        let mut c: Clay = serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"capsule", "a":[0,-0.3,0], "b":[0,0,0], "radius":0.07, "radius_b":0.025, "blend":0}
        ], "resolution":96, "detail_cell":0.005})).unwrap();
        assert_eq!(components(&c.build_mesh()),1);
        let mut s = stroke(Brush::Stretch,&[[0.0,0.01,0.0]],0.09,1.0);
        s.delta = Some(Vec3::new(0.04,0.18,0.0));
        apply(&mut c,&s).unwrap();
        let mesh = c.build_mesh();
        assert_eq!(components(&mesh),1);
        assert!(mesh.bounds().1.y > 0.16, "tip {}",mesh.bounds().1.y);
    }

    #[test]
    fn every_brush_respects_blob_and_height_masks() {
        let base: Clay = serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"sphere", "name":"left", "center":[-0.3,0,0], "radius":0.2, "blend":0},
            {"type":"sphere", "name":"right", "center":[0.3,0,0], "radius":0.2, "blend":0}
        ], "resolution":64, "detail_cell":0.02})).unwrap();
        for brush in [Brush::Add,Brush::Carve,Brush::Inflate,Brush::Deflate,Brush::Smooth,Brush::Flatten,Brush::Grab,Brush::Noise,Brush::Crease,Brush::Pinch,Brush::Layer,Brush::Stretch] {
            let mut c = base.clone();
            let mut s = stroke(brush,&[[-0.3,0.0,0.2],[0.3,0.0,0.2]],0.45,0.03);
            s.delta = Some(Vec3::Y * 0.04);
            s.mask = Some(Mask { blobs:Some(vec!["left".into()]), above:Some(0.0), below:None });
            apply(&mut c,&s).unwrap();
            assert_eq!(sample(&c,Vec3::new(0.3,0.04,0.2)),sample(&base,Vec3::new(0.3,0.04,0.2)),"{brush:?} on right");
            assert_eq!(sample(&c,Vec3::new(-0.3,-0.06,0.2)),sample(&base,Vec3::new(-0.3,-0.06,0.2)),"{brush:?} below");
        }
    }

    #[test]
    fn a_blob_mask_does_not_clip_displacement_to_the_distance_band() {
        for brush in [Brush::Noise,Brush::Stretch] {
            let mut plain = slab(); let mut masked = plain.clone();
            let mut s = stroke(brush,&[[0.0,0.0,0.0]],0.18,0.8);
            s.delta = Some(Vec3::Y * 0.22);
            s.pattern = Some("folds".into()); s.scale = Some(0.16); s.normal = Some(Vec3::Z);
            apply(&mut plain,&s).unwrap();
            s.mask = Some(Mask {blobs:Some(vec!["slab".into()]),above:None,below:None});
            apply(&mut masked,&s).unwrap();
            assert!(serde_json::to_value(&plain.detail).unwrap() == serde_json::to_value(&masked.detail).unwrap(),"{brush:?} clipped by mask");
            assert_eq!(components(&masked.build_mesh()),1,"{brush:?} detached surface");
        }
    }

    #[test]
    fn folds_follow_gravity_and_are_repeatable() {
        let mut a = slab(); let mut b = a.clone();
        let mut s = stroke(Brush::Noise,&[[0.0,0.0,0.0]],0.4,0.6);
        s.pattern = Some("folds".into()); s.scale = Some(0.1); s.normal = Some(Vec3::Z);
        apply(&mut a,&s).unwrap(); apply(&mut b,&s).unwrap();
        assert!(serde_json::to_value(&a.detail).unwrap() == serde_json::to_value(&b.detail).unwrap(), "folds must be deterministic");
        // Folds run along Z: the height changes much more across them (x)
        // than along them (z).
        let spread = |f: &dyn Fn(f32) -> f32| {
            let h: Vec<f32> = (0..9).map(|i| f(-0.12 + 0.03 * i as f32)).collect();
            h.iter().cloned().fold(f32::MIN, f32::max) - h.iter().cloned().fold(f32::MAX, f32::min)
        };
        let across = spread(&|x| top(&a, x, 0.0));
        let along = spread(&|z| top(&a, 0.0, z));
        assert!(across > 0.008 && across > 2.0 * along, "across {across} along {along}");
    }

    fn ball() -> Clay {
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 1.0 })).unwrap();
        Clay { blobs: vec![blob], resolution: 48, detail: None, paint: None, detail_cell: None, ..Default::default() }
    }

    fn stroke(brush: Brush, points: &[[f32; 3]], radius: f32, strength: f32) -> Stroke {
        Stroke { brush, points: points.iter().map(|p| Vec3::from(*p)).collect(), radius, strength, delta: None, normal: None, mirror: false, scale: None, pattern: None, invert: false, mask: None }
    }

    fn max_y(c: &Clay) -> f32 {
        c.build_mesh().bounds().1.y
    }

    #[test]
    fn a_wide_grab_works_on_a_fine_head_sized_clay() {
        // A 2 cm grab on a 0.5 mm grid is too big to read at once; it goes
        // in tiles near the surface (elf6: "stroke is too big").
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "blend": 0.0 })).unwrap();
        let mut c = Clay { blobs: vec![blob], resolution: 128, detail: None, paint: None, detail_cell: None, ..Default::default() };
        c.set_detail_cell(0.0005).unwrap();
        let mut s = stroke(Brush::Grab, &[[0.0, 0.1, 0.0]], 0.03, 1.0);
        s.delta = Some(Vec3::new(0.0, 0.02, 0.0));
        let r = apply(&mut c, &s).unwrap();
        assert!(r.changed_points > 1000, "{}", r.changed_points);
        let field = Field::new(&c, 0.0005);
        let at = |p: Vec3| field.sample(p, &field.near(p - 0.01, p + 0.01)).0;
        // The top was pulled up about 1.3 cm; the bottom stays.
        assert!(at(Vec3::new(0.0, 0.108, 0.0)) < 0.0, "{}", at(Vec3::new(0.0, 0.108, 0.0)));
        assert!(at(Vec3::new(0.0, 0.118, 0.0)) > 0.0);
        assert!(at(Vec3::new(0.0, -0.105, 0.0)) > 0.0);
        // A wide inflate also works.
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 0.0, 0.1]], 0.03, 0.5)).unwrap();
    }

    #[test]
    fn scales_overlap_along_the_flow_and_part_with_grooves() {
        // Along the flow each scale rises to its free edge, then drops to
        // the next one's root: the height has sharp falls in that direction.
        let flow = Vec3::NEG_Y;
        let samples: Vec<f32> = (0..400).map(|i| scale_height(Vec3::new(0.37, -(i as f32) * 0.01, 0.21), flow)).collect();
        let falls = samples.windows(2).filter(|w| w[0] - w[1] > 0.15).count();
        assert!((2..=8).contains(&falls), "{falls} falls over 4 scales");
        assert!(samples.iter().all(|h| (0.0..=1.0).contains(h)));
        // Plates: one groove per plate width.
        let plates = (0..300).map(|i| plate_height(i as f32 * 0.01)).collect::<Vec<_>>();
        assert_eq!(plates.windows(2).filter(|w| w[0] >= 0.05 && w[1] < 0.05).count(), 3);
        // The brush raises scales on a clay surface.
        let mut c = ball();
        c.set_detail_cell(0.01).unwrap();
        let mut st = stroke(Brush::Noise, &[[0.0, 0.0, 1.0]], 0.5, 1.0);
        (st.scale, st.pattern) = (Some(0.08), Some("scales".into()));
        assert!(apply(&mut c, &st).unwrap().changed_points > 500);
    }

    #[test]
    fn untouched_detail_changes_nothing() {
        let mut c = ball();
        // A zero-strength stroke and a far-away one leave the ball alone.
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 0.0)).unwrap();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 5.0, 0.0]], 0.2, 1.0)).unwrap();
        let mesh = c.build_mesh();
        for v in &mesh.vertices {
            let r = Vec3::from(v.pos).length();
            assert!((r - 1.0).abs() < 0.05, "vertex at radius {r}");
        }
    }

    #[test]
    fn inflate_deflate_add_carve_move_the_top() {
        let top = [[0.0, 1.0, 0.0]];
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &top, 0.5, 1.0)).unwrap();
        assert!(max_y(&c) > 1.15, "inflate: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Deflate, &top, 0.5, 1.0)).unwrap();
        assert!(max_y(&c) < 0.9, "deflate: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Add, &[[0.0, 1.0, 0.0], [0.0, 1.5, 0.0]], 0.15, 1.0)).unwrap();
        assert!(max_y(&c) > 1.55, "add: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Carve, &[[-2.0, 1.0, 0.0], [2.0, 1.0, 0.0]], 0.4, 1.0)).unwrap();
        // The groove runs along X at z = 0: the middle of the top is gone.
        let groove = c.build_mesh().vertices.iter().filter(|v| v.pos[2].abs() < 0.05).map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert!(groove < 0.7, "carve: {groove}");
    }

    #[test]
    fn grab_pulls_and_flatten_cuts() {
        let mut c = ball();
        let mut s = stroke(Brush::Grab, &[[0.0, 0.0, 1.0]], 0.5, 1.0);
        s.delta = Some(Vec3::new(0.0, 0.0, 0.3));
        apply(&mut c, &s).unwrap();
        let hi = c.build_mesh().bounds().1;
        assert!(hi.z > 1.2, "grab: {hi}");

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Flatten, &[[0.0, 0.8, 0.0]], 0.9, 1.0)).unwrap();
        assert!(max_y(&c) < 0.9, "flatten: {}", max_y(&c));
    }

    #[test]
    fn smooth_removes_a_bump() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Add, &[[0.0, 1.0, 0.0]], 0.12, 1.0)).unwrap();
        let bumped = max_y(&c);
        for _ in 0..4 {
            apply(&mut c, &stroke(Brush::Smooth, &[[0.0, 1.0, 0.0]], 0.4, 1.0)).unwrap();
        }
        assert!(max_y(&c) < bumped - 0.03, "smooth: {bumped} -> {}", max_y(&c));
    }

    #[test]
    fn detail_survives_json_and_compares_by_identity() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 1.0)).unwrap();
        let text = serde_json::to_string(&c).unwrap();
        let back: Clay = serde_json::from_str(&text).unwrap();
        let (a, b) = (c.build_mesh().bounds().1.y, back.build_mesh().bounds().1.y);
        assert!((a - b).abs() < 0.01, "{a} vs {b}");
        let copy = c.clone();
        assert_eq!(copy, c);
        apply(&mut c, &stroke(Brush::Smooth, &[[0.0, 1.0, 0.0]], 0.3, 0.5)).unwrap();
        assert_ne!(copy, c, "an edit must change identity");
    }

    #[test]
    fn bad_strokes_are_refused() {
        let mut c = ball();
        assert!(apply(&mut c, &stroke(Brush::Add, &[], 0.2, 1.0)).is_err());
        assert!(apply(&mut c, &stroke(Brush::Add, &[[0.0, 0.0, 0.0]], f32::NAN, 1.0)).is_err());
        assert!(apply(&mut c, &stroke(Brush::Grab, &[[0.0, 0.0, 0.0]], 0.2, 1.0)).is_err(), "grab needs delta");
        assert!(apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 0.0, 0.0]], 1e6, 1.0)).is_err(), "too big");
    }
}
