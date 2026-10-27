//! Curvature flow on a sampled field: the mean-curvature step that
//! DynaMesh polish runs (`to_clay::remesh`) and ClayPolish, which removes
//! lumps while it keeps creases, edges and long lines (docs/QUALITY_PLAN.md
//! Stage 1c). ClayPolish runs as a `clay_polish` dab (`brushes::Job`, a box
//! around the dab) and as `edit_clay polish` over a region (`polish`, a band
//! of bricks around the surface); both step every point with `curvature`
//! weighted by `guard`.
//!
//! Each pass is Jacobi (every output from the pass before alone), and the
//! volume shift's sum is taken in a fixed order (z slices, or bricks in key
//! order, each summed in its own order), so the result is the same on any
//! number of threads.

use std::collections::HashMap;

use glam::{IVec3, Vec3};
use serde::{Deserialize, Serialize};

use super::engine::{Diff, Gate, field_for, first_voxel, take_back};
use crate::brush::resolve::FrozenRef;
use crate::clay::{Clay, Detail, Field, LevelView, MAX_BRICKS, MAX_OFFSET_VOXELS, MAX_TOTAL_BRICKS, par_chunks};

/// The flow's time step per pass (explicit, stable below 1/6 at unit
/// spacing with weights of at most 1).
pub const DT: f32 = 1.0 / 6.0;
/// Passes a full `amount` (1) gives: lumps two cells across take about 80
/// to fall to a quarter (the flow is a heat equation on the surface, so
/// their height decays like exp(-k^2 t)).
pub const AMOUNT_PASSES: f32 = 128.0;
/// `edit_clay polish`'s amount when none is given: 13 passes. It takes
/// the lumps Stage 2's lumpiness reads (up to about 4 cells across) and
/// keeps short grooves on a curved head (at least 85% of their depth;
/// 0.5 kept 36-52%, the guard holding their bottoms but not their flanks).
pub const DEFAULT_AMOUNT: f32 = 0.1;
/// Most passes one polish may run.
pub const MAX_PASSES: u32 = 256;
/// Most surface points (`|T| < BAND` cells) one polish may work on in
/// one detail level (levels run one after another). With every brick read
/// holding band points and every point changed this bounds a level's
/// working memory to about 134 MB (`working_bytes`); a smooth 1M-point
/// band takes about 35 MB.
pub const MAX_BAND: usize = 3_000_000;
/// Points within this many cells of the surface flow.
const BAND: f32 = 2.0;
/// Bricks holding a point within this many cells of the surface are read:
/// the band, the guard's two points around it and its normals' one more.
const READ: f32 = 4.5;
/// Most bricks a polish reads (the band, and the points around it the guard
/// reads, in whole bricks: about five times the band's points).
const MAX_READ: usize = 5 * MAX_BAND / 512;
/// Largest box (grid points) a polish may cover; only tiles near the
/// surface inside it are read.
const MAX_BOX: f64 = 4.0e9;
/// Tile side (grid points) the surface is found in.
const TILE: i32 = 32;
/// Anisotropy over which a line is kept (`guard`), and the curvature (per
/// cell) a line must have to count as an intended one.
const LINE: (f32, f32) = (0.5, 0.8);
const LINE_CURVATURE: f32 = 0.15;

/// The mean-curvature speed |∇φ| div(∇φ/|∇φ|) at a lattice point, from φ at
/// unit spacing (`f(dx, dy, dz)`: the value that many points away), or
/// None where the gradient vanishes. DynaMesh polish adds 0.15 of it per
/// pass (its field in cells); this is its step, operation for operation.
#[inline]
pub fn curvature(f: impl Fn(isize, isize, isize) -> f32) -> Option<f32> {
    curvature_at(f, 1e-8)
}

/// `curvature` on a field in world units at cell `voxel`: the gradient
/// counts as vanished below the same fraction of a cell's slope at any
/// cell (an absolute cutoff would stop the flow at fine cells, where
/// per-cell differences are about the cell).
#[inline]
pub fn curvature_scaled(f: impl Fn(isize, isize, isize) -> f32, voxel: f32) -> Option<f32> {
    curvature_at(f, 1e-8 * voxel * voxel)
}

#[inline]
fn curvature_at(f: impl Fn(isize, isize, isize) -> f32, min_g2: f32) -> Option<f32> {
    let v = f(0, 0, 0);
    let (px, py, pz) = ((f(1, 0, 0) - f(-1, 0, 0)) * 0.5, (f(0, 1, 0) - f(0, -1, 0)) * 0.5, (f(0, 0, 1) - f(0, 0, -1)) * 0.5);
    let (pxx, pyy, pzz) = (f(1, 0, 0) - 2.0 * v + f(-1, 0, 0), f(0, 1, 0) - 2.0 * v + f(0, -1, 0), f(0, 0, 1) - 2.0 * v + f(0, 0, -1));
    let pxy = (f(1, 1, 0) - f(1, -1, 0) - f(-1, 1, 0) + f(-1, -1, 0)) * 0.25;
    let pxz = (f(1, 0, 1) - f(1, 0, -1) - f(-1, 0, 1) + f(-1, 0, -1)) * 0.25;
    let pyz = (f(0, 1, 1) - f(0, 1, -1) - f(0, -1, 1) + f(0, -1, -1)) * 0.25;
    let g2 = px * px + py * py + pz * pz;
    if g2 < min_g2 {
        return None;
    }
    Some((pxx * (py * py + pz * pz) + pyy * (px * px + pz * pz) + pzz * (px * px + py * py) - 2.0 * (px * py * pxy + px * pz * pxz + py * pz * pyz)) / g2)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much of the flow a surface point takes, 0..1, from the field
/// around it (`f` at unit spacing, two points out each way):
/// - 0 on creases and edges: where the normal turns by more than `angle`
///   (radians) from the point to one of its six neighbours (1 below 0.6
///   of it);
/// - 0 on intended lines (lip and lid lines, grooves) rounder than that:
///   strongly anisotropic (one principal curvature much larger than the
///   other, from the Hessian on the tangent plane) and strongly curved
///   (the larger at least `LINE_CURVATURE` per cell).
///
/// A lump is short, isotropic and turns the normal by less than `angle`
/// across a cell, so it flows away.
pub fn guard(f: &impl Fn(isize, isize, isize) -> f32, angle: f32) -> f32 {
    let grad = |x: isize, y: isize, z: isize| Vec3::new(f(x + 1, y, z) - f(x - 1, y, z), f(x, y + 1, z) - f(x, y - 1, z), f(x, y, z + 1) - f(x, y, z - 1)) * 0.5;
    let g0 = grad(0, 0, 0);
    let Some(n0) = g0.try_normalize() else { return 0.0 };
    let mut turn = 0.0f32;
    for (x, y, z) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
        let Some(n) = grad(x, y, z).try_normalize() else { return 0.0 };
        turn = turn.max(n0.dot(n).clamp(-1.0, 1.0).acos());
    }
    let g = smoothstep(angle, 0.6 * angle, turn);
    if g <= 0.0 {
        return 0.0;
    }
    // The shape operator on the tangent plane: the Hessian there over |∇T|
    // (in cells, as T is sampled at unit spacing).
    let v = f(0, 0, 0);
    let second = |a: (isize, isize, isize)| f(a.0, a.1, a.2) - 2.0 * v + f(-a.0, -a.1, -a.2);
    let mixed = |a: (isize, isize, isize), b: (isize, isize, isize)| (f(a.0 + b.0, a.1 + b.1, a.2 + b.2) - f(a.0 - b.0, a.1 - b.1, a.2 - b.2) - f(-a.0 + b.0, -a.1 + b.1, -a.2 + b.2) + f(-a.0 - b.0, -a.1 - b.1, -a.2 - b.2)) * 0.25;
    let (ex, ey, ez) = ((1, 0, 0), (0, 1, 0), (0, 0, 1));
    let (hxy, hxz, hyz) = (mixed(ex, ey), mixed(ex, ez), mixed(ey, ez));
    let h = glam::Mat3::from_cols(Vec3::new(second(ex), hxy, hxz), Vec3::new(hxy, second(ey), hyz), Vec3::new(hxz, hyz, second(ez)));
    let (u, w) = n0.any_orthonormal_pair();
    let len = g0.length();
    let (a, b, c) = (u.dot(h * u) / len, u.dot(h * w) / len, w.dot(h * w) / len);
    let (mid, half) = ((a + c) * 0.5, (((a - c) * 0.5).powi(2) + b * b).sqrt());
    let (k1, k2) = ((mid + half).abs(), (mid - half).abs());
    let (big, small) = (k1.max(k2), k1.min(k2));
    let line = (big - small) / (big + small + 1e-6);
    let kept = if big >= LINE_CURVATURE { smoothstep(LINE.0, LINE.1, line) } else { 0.0 };
    g * (1.0 - kept)
}

/// ClayPolish's settings, resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Degrees: a normal turning faster than this across a cell is a crease.
    pub angle: f32,
    pub keep_volume: bool,
    pub passes: usize,
}

/// A clay_polish dab on a dense box of `n` grid points (x fastest): `t`
/// the field, `falloff` how much each point takes (the dab's weight;
/// 0 outside it). Points within `BAND` cells of the surface flow (those at
/// least two points inside the box), each pass by the guard read from the
/// pass before; with `keep_volume` each pass is then shifted by one amount
/// (times the falloff) so the band's sum, and so the enclosed volume,
/// stays. Returns the new field.
pub fn polish_box(t: &[f32], n: IVec3, falloff: &[f32], voxel: f32, s: &Settings) -> Vec<f32> {
    let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
    let strides = (1isize, nx as isize, (nx * ny) as isize);
    fn at(v: &[f32], i: usize, (sx, sy, sz): (isize, isize, isize)) -> impl Fn(isize, isize, isize) -> f32 + '_ {
        move |x, y, z| v[(i as isize + x * sx + y * sy + z * sz) as usize]
    }
    let angle = s.angle.to_radians();
    // The band, from the field before the dab: each point's falloff (the
    // shift's weight), 0 off it.
    let mut band = vec![0.0f32; t.len()];
    par_chunks(&mut band, nx * ny, |z, layer| {
        if z < 2 || z + 2 >= nz {
            return;
        }
        for y in 2..ny.saturating_sub(2) {
            for x in 2..nx.saturating_sub(2) {
                let i = x + nx * (y + ny * z);
                if falloff[i] > 0.0 && t[i].abs() < BAND * voxel {
                    layer[x + nx * y] = falloff[i];
                }
            }
        }
    });
    let mut cur = t.to_vec();
    for _ in 0..s.passes {
        let prev = cur.clone();
        // Each z slice's sums in its own order, the slices in z order.
        let mut moved = vec![Moved::default(); nz];
        {
            let mut slices: Vec<(&mut [f32], &mut Moved)> = cur.chunks_mut(nx * ny).zip(moved.iter_mut()).collect();
            par_chunks(&mut slices, 1, |z, s| {
                let (layer, sum) = &mut s[0];
                for (j, v) in layer.iter_mut().enumerate() {
                    let i = z * nx * ny + j;
                    if band[i] <= 0.0 {
                        continue;
                    }
                    let f = at(&prev, i, strides);
                    let w = band[i] * guard(&f, angle);
                    let d = if w > 0.0 { curvature_scaled(f, voxel).map_or(0.0, |k| DT * w * k) } else { 0.0 };
                    if d != 0.0 {
                        *v = prev[i] + d;
                    }
                    sum.add(prev[i], d, band[i], voxel);
                }
            });
        }
        if s.keep_volume && let Some(c) = Moved::shift(&moved) {
            par_chunks(&mut cur, nx * ny, |z, layer| {
                for (j, v) in layer.iter_mut().enumerate() {
                    let w = band[z * nx * ny + j];
                    if w > 0.0 {
                        *v += c * w;
                    }
                }
            });
        }
    }
    cur
}

/// A pass's change near the surface, for the volume shift: the volume
/// moves where the field crosses zero, so each point's change counts by a
/// hat of its distance from the surface (1 on it, 0 from 1.5 cells out).
#[derive(Clone, Copy, Default)]
struct Moved {
    /// The changes, and the shift's weights, by the hat.
    change: f64,
    weight: f64,
}

impl Moved {
    fn add(&mut self, t: f32, change: f32, weight: f32, voxel: f32) {
        let hat = (1.0 - t.abs() / (1.5 * voxel)).max(0.0) as f64;
        self.change += hat * change as f64;
        self.weight += hat * weight as f64;
    }

    /// The shift (times each point's weight) that puts the pass's volume
    /// back, from partial sums added in their order.
    fn shift(parts: &[Moved]) -> Option<f32> {
        let (c, w) = parts.iter().fold((0.0, 0.0), |(c, w), m| (c + m.change, w + m.weight));
        (w > 0.0).then(|| (-c / w) as f32)
    }
}

/// Where `edit_clay polish` works, in the clay's space: an ellipsoid.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub center: Vec3,
    pub radii: Vec3,
}

impl Region {
    /// 1 inside, fading to 0 over the outer fifth of the radii.
    fn falloff(&self, p: Vec3) -> f32 {
        1.0 - smoothstep(0.8, 1.0, ((p - self.center) / self.radii).length())
    }
}

/// An `edit_clay polish` record's action (`ledger::SculptAction::Polish`):
/// the call's arguments resolved, and the cell of the lattice it ran on.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Polish {
    /// As given (default `DEFAULT_AMOUNT`); `passes` is what it resolved to.
    pub amount: f32,
    pub angle: f32,
    /// None: the whole clay.
    pub region: Option<Region>,
    pub passes: u32,
    pub keep_volume: bool,
    /// The coarse detail cell the record ran at.
    pub cell: f32,
    /// The persistent mask and polygroups it read (by hash, as a stroke's
    /// `Resolved::frozen`): masked points and hidden groups hold still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<FrozenRef>,
}

impl Polish {
    /// What a record may hold (records come from files too).
    pub fn check(&self) -> Result<(), String> {
        let region_ok = self.region.is_none_or(|r| r.center.is_finite() && r.radii.is_finite() && r.radii.min_element() > 0.0);
        if !((0.0..=1.0).contains(&self.amount) && (5.0..=90.0).contains(&self.angle) && (1..=MAX_PASSES).contains(&self.passes) && self.cell.is_finite() && self.cell > 0.0 && region_ok) {
            return Err(format!("polish needs amount 0-1, angle 5-90 degrees, passes 1-{MAX_PASSES}, a cell > 0 and a region with a finite center and radii > 0"));
        }
        Ok(())
    }

    /// The mirror image across x = 0.
    pub fn mirrored_x(&self) -> Polish {
        Polish { region: self.region.map(|r| Region { center: r.center * Vec3::new(-1.0, 1.0, 1.0), ..r }), ..*self }
    }

    fn settings(&self) -> Settings {
        Settings { angle: self.angle, keep_volume: self.keep_volume, passes: self.passes as usize }
    }
}

/// The pass count an `amount` gives.
pub fn passes_for(amount: f32) -> u32 {
    ((amount * AMOUNT_PASSES).round() as u32).clamp(1, MAX_PASSES)
}

/// The coarse cell a polish of `clay` runs at when the engine's grid is
/// `grid` (a clay with no grid yet takes its detail cell, else its mesh's).
pub fn lattice(clay: &Clay, grid: Option<&Detail>) -> f32 {
    grid.map_or_else(|| first_voxel(clay, 1.0), Detail::voxel)
}

/// What a polish worked on, over the levels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Worked {
    /// Surface points (`|T| < BAND` cells) it flowed.
    pub points: usize,
    /// The most working memory one level took (`working_bytes`).
    pub bytes: usize,
}

/// ClayPolish over `p.region` (or the whole clay) on `clay`, whose grid the
/// engine reads is `detail`: on the coarse grid and on every detail region
/// it reaches, each at its own cell, as a stroke's dab is (`engine::apply`).
/// The persistent mask and hidden polygroups it froze (`p.frozen`) hold
/// points still, as they do a stroke's. Also returns what it worked on.
pub fn polish(clay: &Clay, detail: Option<&Detail>, p: &Polish) -> Result<(Diff, Worked), String> {
    p.check()?;
    // It runs on the grid the records before it left; with no grid yet it
    // starts one at the cell it ran at (as a regions record does). A grid
    // at another cell (earlier records replayed after a detail_cell change)
    // is polished at its own cell, as a stroke would be: the result then
    // differs, and the record's result hash says so (never a refusal, so
    // a ledger always replays).
    let voxel = detail.map_or(p.cell, Detail::voxel);
    let pre = detail.cloned().unwrap_or_else(|| Detail::new(voxel));
    let gate = Gate::frozen(clay, p.frozen)?;
    let field = field_for(clay, &pre, voxel);
    // A region is clipped to the clay's bounds first, so one larger than
    // the clay works on the clay; one beside it works on nothing.
    let bounds = field.bounds();
    let (lo, hi) = match (p.region, bounds) {
        (Some(r), Some((a, b))) => ((r.center - r.radii).max(a), (r.center + r.radii).min(b)),
        (Some(_), None) => return Ok((Diff { detail: pre, changed_points: 0, radius: 0.0, dabs: 1 }, Worked::default())),
        (None, b) => b.ok_or("the clay has no surface to polish")?,
    };
    let mut detail = pre.clone();
    let mut targets: Vec<Option<usize>> = vec![None];
    for (k, l) in pre.grid().levels().iter().enumerate() {
        let (a, b) = l.extent(0.0);
        if lo.cmple(b).all() && hi.cmpge(a).all() {
            targets.push(Some(k));
        }
    }
    let (mut changed, mut worked) = (0, Worked::default());
    // The points the coarser levels changed, with their cells.
    let mut lower: Vec<(Vec3, f32)> = Vec::new();
    for target in targets {
        // Every level reads the grid before the polish, and stores what it
        // adds to the levels under it as they are after it (`apply_dab`).
        let view = LevelView::new(pre.grid(), target);
        let v = view.voxel();
        let (mut a, mut b) = (lo - 3.0 * v, hi + 3.0 * v);
        if let Some(k) = target {
            let (c, d) = pre.grid().levels()[k].extent(0.0);
            (a, b) = (a.max(c), b.min(d));
        }
        if a.cmpgt(b).any() {
            continue;
        }
        let (full, level) = band(&field, &view, &gate, v, a, b, p)?;
        worked = Worked { points: worked.points + level.points, bytes: worked.bytes.max(level.bytes) };
        let changes = match target {
            None => full,
            Some(k) => {
                let g = detail.grid();
                let mut out: Vec<([i32; 3], f32)> = full.into_iter().map(|(i, f)| (i, f - g.parent(k, Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v))).collect();
                out.extend(take_back(k, g, pre.grid(), &lower, &out));
                out
            }
        };
        lower.extend(changes.iter().map(|(i, _)| (Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v, v)));
        changed += changes.len();
        let g = detail.grid_mut().target_mut(target);
        g.write(&changes, true);
        if g.brick_count() > MAX_BRICKS {
            return Err(format!("too much sculpt detail (over {MAX_BRICKS} bricks{}); use a larger cell or a smaller region", if target.is_some() { " in one detail region" } else { "" }));
        }
    }
    if detail.grid().total_bricks() > MAX_TOTAL_BRICKS {
        return Err(format!("too much sculpt detail (over {MAX_TOTAL_BRICKS} bricks in all); use larger cells or a smaller region"));
    }
    Ok((Diff { detail, changed_points: changed, radius: 0.0, dabs: 1 }, worked))
}

/// New whole offsets by grid point.
type Changes = Vec<([i32; 3], f32)>;

/// A brick's 8^3 values, x fastest.
type Brick = [f32; 512];

const NONE: u32 = u32::MAX;

/// The bricks around the surface in a box, for one level, in read order
/// (tiles in order, each one's bricks in order): their keys, values (as
/// read, never written), each one's 27 neighbours (itself in the middle),
/// and for those holding band points which points those are (`slot`: its
/// place in `band`, or `NONE` for the rest, which never change).
struct Bricks {
    keys: Vec<[i32; 3]>,
    vals: Vec<Brick>,
    near: Vec<[u32; 27]>,
    slot: Vec<u32>,
    band: Vec<BandBrick>,
}

/// A brick's band points: which of its 512 (a bit each, with the count
/// before each word), and where its run starts in the per-point arrays.
#[derive(Clone, Copy)]
struct BandBrick {
    bits: [u64; 8],
    rank: [u16; 8],
    start: u32,
    count: u32,
}

impl BandBrick {
    fn of(points: &[(u16, f32)], start: u32) -> BandBrick {
        let mut bits = [0u64; 8];
        for (j, _) in points {
            bits[*j as usize >> 6] |= 1 << (j & 63);
        }
        let mut rank = [0u16; 8];
        for w in 1..8 {
            rank[w] = rank[w - 1] + bits[w - 1].count_ones() as u16;
        }
        BandBrick { bits, rank, start, count: points.len() as u32 }
    }

    /// Point `j`'s place in the per-point arrays, if it is a band point.
    #[inline]
    fn at(&self, j: usize) -> Option<usize> {
        let (w, bit) = (j >> 6, 1u64 << (j & 63));
        (self.bits[w] & bit != 0).then(|| self.start as usize + self.rank[w] as usize + (self.bits[w] & (bit - 1)).count_ones() as usize)
    }
}

impl Bricks {
    /// Brick `b`'s values with `pad` points of its neighbours around it
    /// (side 8 + 2 pad, x fastest): band points from `now` (per point),
    /// the rest as read; a missing neighbour's points take the nearest of
    /// its own.
    fn padded(&self, now: &[f32], b: usize, pad: i32, out: &mut Vec<f32>) {
        let get = |n: usize, j: usize| -> f32 {
            match self.slot[n] {
                NONE => self.vals[n][j],
                s => self.band[s as usize].at(j).map_or(self.vals[n][j], |k| now[k]),
            }
        };
        let side = 8 + 2 * pad;
        out.clear();
        for z in -pad..8 + pad {
            for y in -pad..8 + pad {
                for x in -pad..8 + pad {
                    let (bx, by, bz) = (x.div_euclid(8), y.div_euclid(8), z.div_euclid(8));
                    let n = self.near[b][((bx + 1) + 3 * (by + 1) + 9 * (bz + 1)) as usize];
                    let v = if n == NONE {
                        let c = |a: i32| a.clamp(0, 7) as usize;
                        get(b, c(x) + 8 * (c(y) + 8 * c(z)))
                    } else {
                        get(n as usize, (x.rem_euclid(8) + 8 * (y.rem_euclid(8) + 8 * z.rem_euclid(8))) as usize)
                    };
                    out.push(v);
                }
            }
        }
        debug_assert_eq!(out.len(), (side * side * side) as usize);
    }
}

/// One level's working memory in bytes, with `read` bricks read, `band`
/// of them holding `points` band points and `changed` points written: per
/// brick read its values, key, neighbours and slot; per band brick its
/// bits and sums; per band point its place, weight and two pass values.
/// The changes list (of exactly their count) is made after the weights
/// and the pass buffer before the last are freed, so the peak is the
/// larger of the passes and the write.
pub fn working_bytes(read: usize, band: usize, points: usize, changed: usize) -> usize {
    let per_read = std::mem::size_of::<Brick>() + std::mem::size_of::<[i32; 3]>() + std::mem::size_of::<[u32; 27]>() + 4;
    let per_band = std::mem::size_of::<BandBrick>() + std::mem::size_of::<Moved>() + 4;
    let passes = band * per_band + points * (2 + 3 * 4);
    let write = band * (per_band + std::mem::size_of::<usize>() + std::mem::size_of::<(usize, &mut [u8])>()) + points * (2 + 4) + changed * std::mem::size_of::<([i32; 3], f32)>();
    read * per_read + passes.max(write)
}

/// One level's polish over the box `lo..hi` (world, at cell `voxel`): the
/// new whole offsets of the points it changed, in brick order, and what it
/// worked on. Each surface point flows by its region falloff times what
/// the gate lets through, so masked points hold still while the rest flow
/// past them.
fn band(field: &Field, view: &LevelView, gate: &Gate, voxel: f32, lo: Vec3, hi: Vec3, p: &Polish) -> Result<(Changes, Worked), String> {
    let cells = ((hi - lo) / voxel + 2.0).as_dvec3();
    if !cells.is_finite() || cells.x * cells.y * cells.z > MAX_BOX {
        return Err(format!("the polish region is too big for the detail grid (cell {voxel:.5}); use a smaller region or a coarser detail_cell"));
    }
    let (i0, i1) = ((lo / voxel).floor().as_ivec3(), (hi / voxel).ceil().as_ivec3());
    let (t0, t1) = (i0.div_euclid(IVec3::splat(TILE)), i1.div_euclid(IVec3::splat(TILE)));
    let mut tiles: Vec<(IVec3, bool)> = Vec::new();
    for z in t0.z..=t1.z {
        for y in t0.y..=t1.y {
            for x in t0.x..=t1.x {
                tiles.push((IVec3::new(x, y, z) * TILE, false));
            }
        }
    }
    // Tiles the surface may pass near.
    let reach = (READ + 1.0) * voxel;
    par_chunks(&mut tiles, 16, |_, chunk| {
        for (c0, near) in chunk {
            let c1 = *c0 + TILE - 1;
            let (a, b) = (c0.as_vec3() * voxel, c1.as_vec3() * voxel);
            let (center, half) = ((a + b) * 0.5, (b - a).length() * 0.5);
            let carved = view.carved(*c0 - 1, c1 + 1);
            let list = field.near(a - reach - half - carved, b + reach + half + carved);
            *near = field.far_side(center, &list, half + reach + carved).is_none();
        }
    });
    tiles.retain(|t| t.1);
    // The field on each near tile, kept by brick where it is near the
    // surface; past `MAX_READ` bricks in all, the rest are not read and the
    // polish is refused (so a region at too fine a cell never holds more).
    let too_much = || format!("polish would work on more than {MAX_BAND} surface points in one detail level (cell {voxel:.5}); use a smaller region or a coarser detail_cell");
    let read = std::sync::atomic::AtomicUsize::new(0);
    let limit = field.limit;
    type Read = Vec<([i32; 3], Brick)>;
    let mut reads: Vec<(IVec3, Read)> = tiles.into_iter().map(|(c, _)| (c, Vec::new())).collect();
    par_chunks(&mut reads, 1, |_, chunk| {
        let (c0, out) = &mut chunk[0];
        if read.load(std::sync::atomic::Ordering::Relaxed) > MAX_READ {
            return;
        }
        let n = IVec3::splat(TILE);
        let (a, b) = (c0.as_vec3() * voxel, (*c0 + TILE - 1).as_vec3() * voxel);
        let e = Vec3::splat(voxel);
        let near = field.near(a - e, b + e);
        let mut t = vec![0.0f32; (TILE * TILE * TILE) as usize];
        field.base_block(*c0, n, voxel, &near, &mut t);
        let mut row = [0.0f32; TILE as usize];
        let s = TILE as usize;
        for z in 0..s {
            for y in 0..s {
                view.row((*c0 + IVec3::new(0, y as i32, z as i32)).to_array(), &mut row);
                for x in 0..s {
                    t[x + s * (y + s * z)] += row[x];
                }
            }
        }
        let k0 = c0.div_euclid(IVec3::splat(8));
        for bz in 0..4 {
            for by in 0..4 {
                for bx in 0..4 {
                    let mut brick = [0.0f32; 512];
                    let mut keep = false;
                    for (j, v) in brick.iter_mut().enumerate() {
                        let (x, y, z) = (bx * 8 + j % 8, by * 8 + j / 8 % 8, bz * 8 + j / 64);
                        *v = t[x + s * (y + s * z)];
                        keep |= v.abs() < READ * voxel;
                    }
                    if keep {
                        out.push(((k0 + IVec3::new(bx as i32, by as i32, bz as i32)).to_array(), brick));
                    }
                }
            }
        }
        read.fetch_add(out.len(), std::sync::atomic::Ordering::Relaxed);
    });
    if read.into_inner() > MAX_READ {
        return Err(too_much());
    }
    // One copy of the bricks: each tile's list is moved over and freed in
    // turn (tile order is a fixed order, as key order was).
    let count = reads.iter().map(|r| r.1.len()).sum();
    let (mut keys, mut vals) = (Vec::with_capacity(count), Vec::with_capacity(count));
    for (_, r) in reads {
        for (k, b) in r {
            keys.push(k);
            vals.push(b);
        }
    }
    let near: Vec<[u32; 27]> = {
        let index: HashMap<[i32; 3], u32> = keys.iter().enumerate().map(|(i, k)| (*k, i as u32)).collect();
        keys.iter()
            .map(|k| {
                let mut n = [NONE; 27];
                for (j, slot) in n.iter_mut().enumerate() {
                    let d = [j as i32 % 3 - 1, j as i32 / 3 % 3 - 1, j as i32 / 9 - 1];
                    *slot = index.get(&[k[0] + d[0], k[1] + d[1], k[2] + d[2]]).copied().unwrap_or(NONE);
                }
                n
            })
            .collect()
    };
    let mut bricks = Bricks { slot: vec![NONE; keys.len()], keys, vals, near, band: Vec::new() };

    // The band, from the field before the polish: each point's falloff
    // times its gate (the flow's and the shift's weight), 0 off it; only
    // its points are kept, brick by brick.
    let region = p.region;
    let gated = gate.points();
    let mut found: Vec<Vec<(u16, f32)>> = vec![Vec::new(); bricks.keys.len()];
    par_chunks(&mut found, 64, |c, chunk| {
        let mut pad = Vec::new();
        for (k, list) in chunk.iter_mut().enumerate() {
            let b = c * 64 + k;
            let key = IVec3::from(bricks.keys[b]);
            for j in 0..512 {
                let g = key * 8 + IVec3::new(j as i32 % 8, j as i32 / 8 % 8, j as i32 / 64);
                let t = bricks.vals[b][j];
                if t.abs() >= BAND * voxel || g.cmplt(i0).any() || g.cmpgt(i1).any() {
                    continue;
                }
                let at = g.as_vec3() * voxel;
                let mut fall = region.map_or(1.0, |r| r.falloff(at));
                if fall > 0.0 && gated {
                    // Off the surface the gate is read where the surface is.
                    if pad.is_empty() {
                        bricks.padded(&[], b, 1, &mut pad);
                    }
                    let (x, y, z) = ((j % 8 + 1) as isize, (j / 8 % 8 + 1) as isize, (j / 64 + 1) as isize);
                    let f = |dx: isize, dy: isize, dz: isize| pad[(x + dx + 10 * (y + dy + 10 * (z + dz))) as usize];
                    let normal = Vec3::new(f(1, 0, 0) - f(-1, 0, 0), f(0, 1, 0) - f(0, -1, 0), f(0, 0, 1) - f(0, 0, -1)).try_normalize();
                    fall *= gate.at_near(field, at, t, normal);
                }
                if fall > 0.0 {
                    list.push((j as u16, fall));
                }
            }
            pad.clear();
        }
    });
    let points: usize = found.iter().map(Vec::len).sum();
    if points > MAX_BAND {
        return Err(too_much());
    }
    // The band bricks in read order, and their points in one run each.
    let mut members: Vec<u32> = Vec::new();
    let (mut place, mut weight) = (Vec::with_capacity(points), Vec::with_capacity(points));
    for (b, list) in found.into_iter().enumerate() {
        if list.is_empty() {
            continue;
        }
        bricks.slot[b] = members.len() as u32;
        members.push(b as u32);
        bricks.band.push(BandBrick::of(&list, place.len() as u32));
        for (j, w) in list {
            place.push(j);
            weight.push(w);
        }
    }
    let bricks = bricks;

    // The passes: each point from the pass before, its guard read there.
    // Two per-point buffers take turns.
    let (angle, settings) = (p.angle.to_radians(), p.settings());
    let mut prev: Vec<f32> = members.iter().zip(&bricks.band).flat_map(|(b, bb)| place[bb.start as usize..(bb.start + bb.count) as usize].iter().map(move |j| (*b, *j))).map(|(b, j)| bricks.vals[b as usize][j as usize]).collect();
    let mut cur = prev.clone();
    for _ in 0..settings.passes {
        let mut moved = vec![Moved::default(); members.len()];
        {
            // Each band brick's run of `cur`, with its sums.
            let mut work: Vec<(&mut [f32], &mut Moved)> = Vec::with_capacity(members.len());
            let mut rest: &mut [f32] = &mut cur;
            for (bb, m) in bricks.band.iter().zip(moved.iter_mut()) {
                let (run, tail) = std::mem::take(&mut rest).split_at_mut(bb.count as usize);
                work.push((run, m));
                rest = tail;
            }
            par_chunks(&mut work, 8, |c, chunk| {
                let mut pad = Vec::new();
                for (k, (out, sum)) in chunk.iter_mut().enumerate() {
                    let m = c * 8 + k;
                    let (b, bb) = (members[m] as usize, &bricks.band[m]);
                    bricks.padded(&prev, b, 2, &mut pad);
                    for (r, o) in out.iter_mut().enumerate() {
                        let i = bb.start as usize + r;
                        let (j, s) = (place[i] as usize, weight[i]);
                        let (x, y, z) = ((j % 8) as isize + 2, (j / 8 % 8) as isize + 2, (j / 64) as isize + 2);
                        let at = |dx: isize, dy: isize, dz: isize| pad[((x + dx) + 12 * ((y + dy) + 12 * (z + dz))) as usize];
                        let w = s * guard(&at, angle);
                        let d = if w > 0.0 { curvature_scaled(at, voxel).map_or(0.0, |k| DT * w * k) } else { 0.0 };
                        *o = if d != 0.0 { prev[i] + d } else { prev[i] };
                        sum.add(prev[i], d, s, voxel);
                    }
                }
            });
        }
        if settings.keep_volume && let Some(c) = Moved::shift(&moved) {
            par_chunks(&mut cur, 4096, |n, chunk| {
                for (k, v) in chunk.iter_mut().enumerate() {
                    *v += c * weight[n * 4096 + k];
                }
            });
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    // `prev` holds the last pass.
    drop((cur, weight));

    // The changed points' new whole offsets, brick by brick in order, into
    // one list of exactly their count.
    let max_off = MAX_OFFSET_VOXELS * voxel;
    // Below this the change is dropped (as dabs drop theirs; less at fine
    // cells, so the flow still writes there).
    let tiny = 1e-7f32.min(2e-4 * voxel);
    let (members, bricks, place, prev) = (&members, &bricks, &place, &prev);
    // Band brick `m`'s changed points: (brick, point, place).
    let run = move |m: usize| {
        let (b, bb) = (members[m] as usize, &bricks.band[m]);
        (bb.start as usize..(bb.start + bb.count) as usize).map(move |i| (b, place[i] as usize, i)).filter(move |(b, j, i)| (prev[*i] - bricks.vals[*b][*j]).abs() > tiny)
    };
    let mut counts = vec![0usize; members.len()];
    par_chunks(&mut counts, 64, |c, chunk| {
        for (k, n) in chunk.iter_mut().enumerate() {
            *n = run(c * 64 + k).count();
        }
    });
    let changed: usize = counts.iter().sum();
    let mut list: Changes = vec![([0; 3], 0.0); changed];
    {
        let mut work: Vec<(usize, &mut [([i32; 3], f32)])> = Vec::with_capacity(members.len());
        let mut rest: &mut [([i32; 3], f32)] = &mut list;
        for (m, n) in counts.iter().enumerate() {
            let (out, tail) = std::mem::take(&mut rest).split_at_mut(*n);
            work.push((m, out));
            rest = tail;
        }
        par_chunks(&mut work, 8, |_, chunk| {
            for (m, out) in chunk.iter_mut() {
                let key = IVec3::from(bricks.keys[members[*m] as usize]);
                for ((b, j, i), slot) in run(*m).zip(out.iter_mut()) {
                    let (old, new) = (bricks.vals[b][j], prev[i]);
                    let g = (key * 8 + IVec3::new(j as i32 % 8, j as i32 / 8 % 8, j as i32 / 64)).to_array();
                    let off = view.get(g);
                    // The field there is the blobs' plus the offset.
                    let base = old - off;
                    *slot = (g, (new.clamp(-limit, limit) - base).clamp(-max_off, max_off));
                }
            }
        });
    }
    let worked = Worked { points, bytes: working_bytes(bricks.keys.len(), members.len(), points, changed) };
    Ok((list, worked))
}
