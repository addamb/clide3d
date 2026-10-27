//! The sculpt engine: folds resolved dabs (see `brush::resolve`) into a
//! clay's detail grid.
//!
//! Jacobi contract: each dab is a pure function of the grid before it, the
//! clay's blobs and the dab. Dabs apply one after another; inside a dab the
//! work is split across output points only (each point is computed by one
//! thread from the pre-dab grid), with no float sums shared between
//! threads, so the result is bit-identical on any number of threads.

use glam::{IVec3, Vec3};

use super::brushes::Job;
use super::noise::{displace_band, noise_band, smooth01};
use crate::brush::alpha::{self, Alpha, Stamp};
use crate::brush::vdm;
use crate::brush::curve::{Curve, legacy};
use crate::brush::resolve::{Dab, Params};
use crate::brush::spec::{Brush, StrokeMask};
use crate::clay::{Clay, Detail, DetailGrid, Field, LevelView, MAX_BRICKS, MAX_TOTAL_BRICKS, par_chunks};

/// The brush kernel's version, kept on every ledger record (see
/// `ledger.rs`). Any change that makes a brush give another grid for the
/// same resolved stroke (a fix, a float reordering, new transport) must
/// raise it: records from older kernels are then reported engine_stale
/// rather than replayed into other shapes.
///
/// - 1: Stages 1a-7.
/// - 2: detail transport (Stage 8a). A dab on a refined level reads the
///   grid before the dab, not the one its coarser levels were just
///   written into (version 1 applied inflate, grab and the others that
///   read the grid twice there); strokes with a level carry the written
///   layer's finer levels along (`carry`; other layers keep theirs).
pub const ENGINE_VERSION: u16 = 2;

/// Most grid points one stroke reads at once; bigger strokes go in tiles.
const MAX_REGION: usize = 6_000_000;
/// Most grid points a tiled stroke may read in all.
const MAX_VISIT: usize = 60_000_000;
/// Largest box (in grid points) a stroke may span; only tiles near the
/// surface inside it are read.
const MAX_BOX: f64 = 4.0e9;
/// Tile side (grid points) for strokes too big to hold at once.
pub(super) const TILE: i32 = 32;

/// What the engine needs besides the grid and the dabs.
pub struct Ctx<'a> {
    /// The clay being sculpted (its blobs make the base field).
    pub clay: &'a Clay,
    pub params: &'a Params,
    /// The image `params.alpha` names (see `alpha_of`).
    pub alpha: Option<&'a Alpha>,
    /// The persistent mask, polygroups and auto-masking (see `Gate`).
    pub gate: &'a Gate,
    /// On a clay with sculpt layers, what the other layers add to the
    /// composite the engine runs on (`ledger::SculptInput`): detail
    /// transport carries only the written layer's own finer detail, so
    /// layers stay separable. None: the grid is the written layer's own.
    pub others: Option<&'a Detail>,
}

/// Where a stroke may work beyond its hard limits (`allowed`): every
/// point changes by `at(p)` of what the brush would do there, from the
/// persistent mask (1 - mask) and polygroups (0 in hidden groups, and
/// outside the stroke's `groups`) the stroke read, which the clay keeps
/// by hash (`ledger::Frozen`); and auto-masking from the surface around
/// each dab (`auto`, applied per region by the brushes). A stroke that
/// read neither changes exactly as it did before.
#[derive(Default)]
pub struct Gate {
    mask: Option<crate::mask::SculptMask>,
    invert: bool,
    groups: Option<crate::groups::Groups>,
    hidden: Vec<u16>,
    only: Option<Vec<u16>>,
    /// The stroke's mask, when it auto-masks.
    pub auto: Option<StrokeMask>,
}

impl Gate {
    /// The gate of a resolved stroke on `clay`.
    pub fn of(clay: &Clay, stroke: &crate::brush::resolve::Resolved) -> Result<Gate, String> {
        let m = stroke.mask.as_ref();
        let f = stroke.frozen.unwrap_or_default();
        let lost = "the clay no longer keeps the mask or groups this stroke read; rebase or resend it";
        let mask = f.mask.map(|h| crate::ledger::frozen_mask(clay, h).ok_or(lost)).transpose()?;
        let groups = f.groups.map(|h| crate::ledger::frozen_groups(clay, h).ok_or(lost)).transpose()?;
        let hidden = groups.as_ref().map(|g| g.hidden()).unwrap_or_default();
        Ok(Gate { mask, invert: m.is_some_and(|m| m.invert_channel), groups, hidden, only: m.and_then(|m| m.groups.clone()), auto: m.filter(|m| m.auto()).cloned() })
    }

    /// The gate of the persistent mask and polygroups a record froze
    /// (`ledger::freeze`), with no stroke mask: an action's (ClayPolish).
    pub fn frozen(clay: &Clay, frozen: Option<crate::brush::resolve::FrozenRef>) -> Result<Gate, String> {
        let f = frozen.unwrap_or_default();
        let lost = "the clay no longer keeps the mask or groups this record read; resend it";
        let mask = f.mask.map(|h| crate::ledger::frozen_mask(clay, h).ok_or(lost)).transpose()?;
        let groups = f.groups.map(|h| crate::ledger::frozen_groups(clay, h).ok_or(lost)).transpose()?;
        let hidden = groups.as_ref().map(|g| g.hidden()).unwrap_or_default();
        Ok(Gate { mask, invert: false, groups, hidden, only: None, auto: None })
    }

    /// Whether the persistent mask or groups hold anything back.
    pub fn points(&self) -> bool {
        self.mask.is_some() || !self.hidden.is_empty() || self.only.is_some() || self.invert
    }

    /// How much of the brush's change applies at `p`, 0..1 (exactly 1
    /// where nothing holds it back, exactly 0 under a full mask).
    pub fn at(&self, p: Vec3) -> f32 {
        if !self.points() {
            return 1.0;
        }
        let v = self.mask.as_ref().map_or(0.0, |m| m.grid().value(p));
        let k = if self.invert { v } else { 1.0 - v };
        if k <= 0.0 {
            return 0.0;
        }
        if !self.hidden.is_empty() || self.only.is_some() {
            let id = self.groups.as_ref().map_or(0, |g| g.label(p));
            if (id != 0 && self.hidden.contains(&id)) || self.only.as_ref().is_some_and(|o| !o.contains(&id)) {
                return 0.0;
            }
        }
        k
    }

    /// `at` for a point anywhere: the mask and groups live on the surface
    /// band, so a point off it takes them from the surface point nearest
    /// it (along `normal`, the field's gradient there, when the caller has
    /// it). So a strong brush, or a transform, cannot grow new surface out
    /// of the air or the inside next to masked surface. `t` is the field
    /// at `p` (clamped to the field's limit).
    pub fn at_near(&self, field: &Field, p: Vec3, t: f32, normal: Option<Vec3>) -> f32 {
        if !self.points() {
            return 1.0;
        }
        let cell = self.mask.as_ref().map(|m| m.grid().voxel).or(self.groups.as_ref().map(|g| g.data().voxel)).unwrap_or(0.0);
        if t.abs() <= 1.5 * cell {
            return self.at(p);
        }
        let e = Vec3::splat(cell.max(1e-6) * 2.0);
        let list = field.near(p - e - field.limit, p + e + field.limit);
        // Beyond the clamp the field says nothing of distance: the
        // unclamped blobs (and detail) do.
        let (d, n) = if t.abs() >= field.limit * 0.999 || normal.is_none() {
            (field.surface_distance(p, &list), field.sculpt_normal(p, &list, cell.max(1e-6)))
        } else {
            (t, normal.expect("checked"))
        };
        if !d.is_finite() || n.length_squared() < 0.5 {
            return self.at(p);
        }
        self.at(p - n * d)
    }
}

/// The image a stroke's alpha names, from the clay's store (its records'
/// images, kept with its ledgers).
pub fn alpha_of(clay: &Clay, params: &Params) -> Result<Option<Alpha>, String> {
    let Some(a) = &params.alpha else { return Ok(None) };
    let store = clay.sculpt.as_ref().map(|s| &s.data().alphas);
    store.and_then(|s| s.images.get(&a.hash)).cloned().map(Some).ok_or_else(|| format!("the clay does not hold alpha {} ({}); import it again and resend the stroke", a.hash, a.name.as_deref().unwrap_or("unnamed")))
}

/// How far a snakehook's material may come from: its path's length.
fn hook_reach(points: &[Vec3]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// The grid after a run of dabs, and what changed.
pub struct Diff {
    pub detail: Detail,
    /// Grid points written, over all dabs and levels.
    pub changed_points: usize,
    /// The radius used by the last dab, after raising it to at least 1.5
    /// detail cells.
    pub radius: f32,
    pub dabs: usize,
}

/// A backward map: where the material now at `p` came from. Brushes that
/// move material (grab, stretch, snakehook, transform) have one: a stroke
/// at a detail level moves the finer levels by it (`carry`), so their
/// detail follows the move.
pub trait Transport: Sync {
    fn source(&self, p: Vec3) -> Vec3;
}

struct Pull {
    anchor: Vec3,
    delta: Vec3,
    radius: f32,
    gain: f32,
    curve: Curve,
}

impl Transport for Pull {
    fn source(&self, p: Vec3) -> Vec3 {
        p - self.delta * (self.curve.weight((p - self.anchor).length(), self.radius) * self.gain)
    }
}

struct Stretch {
    points: Vec<Vec3>,
    delta: Vec3,
    radius: f32,
}

impl Transport for Stretch {
    fn source(&self, p: Vec3) -> Vec3 {
        stretch_source(p, &self.points, self.delta, self.radius)
    }
}

struct Hook {
    points: Vec<Vec3>,
    radius: f32,
}

impl Transport for Hook {
    fn source(&self, p: Vec3) -> Vec3 {
        hook_source(p, &self.points, self.radius)
    }
}

/// The backward map of one dab, if its brush moves material, as the brush
/// moves the level it writes at `radius` (the dab's, at least 1.5 of that
/// level's cells); `gain` is the first point's strength factor (grab).
pub fn transport(params: &Params, dab: &Dab, radius: f32, gain: f32) -> Option<Box<dyn Transport>> {
    if params.brush == Brush::Transform {
        return transform_transport(params);
    }
    if params.brush == Brush::Snakehook {
        return Some(Box::new(Hook { points: dab.points.clone(), radius }));
    }
    let delta = dab.delta?;
    match params.brush {
        Brush::Grab => Some(Box::new(Pull { anchor: dab.points[0], delta: delta.clamp_length_max(radius), radius, gain, curve: params.curve.clone() })),
        Brush::Stretch => Some(Box::new(Stretch { points: dab.points.clone(), delta, radius })),
        _ => None,
    }
}

/// A transform's backward map, `amount` of the whole move (the mask
/// scales it per point when it applies).
struct Moved(glam::Mat4, f32);

impl Transport for Moved {
    fn source(&self, p: Vec3) -> Vec3 {
        p + (self.0.transform_point3(p) - p) * self.1
    }
}

/// The backward map of a transform stroke.
fn transform_transport(params: &Params) -> Option<Box<dyn Transport>> {
    params.transform.map(|m| Box::new(Moved(m.inverse(), params.strength.clamp(0.0, 1.0))) as Box<dyn Transport>)
}

/// How a dab at a detail level carries a finer level's detail (what that
/// level adds to the coarser ones) along with what it did to them.
enum Carry {
    /// Brushes that move material: the finer detail at `p` is the detail
    /// that was where the map says the material came from.
    Map(Box<dyn Transport>),
    /// The others displace the surface along its normal: where the
    /// coarser field T changed by dT, the finer detail moves with the
    /// surface, by -dT/|grad T| along grad T.
    Normal,
}

/// Most 8^3 blocks one carry may visit.
const MAX_CARRY_BLOCKS: f64 = 8_000_000.0;

#[cfg(test)]
thread_local! {
    /// Tests turn detail transport off to measure what it does.
    pub(crate) static NO_CARRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn carry_off() -> bool {
    #[cfg(test)]
    return NO_CARRY.with(|c| c.get());
    #[cfg(not(test))]
    false
}

/// Level `k`'s new values (finer than the dab's level, which did not
/// write it) inside `lo..hi`, carried by `how`: `post` is the grid with
/// the dab's levels written, `pre` the grid before the dab (Normal reads
/// the coarser field's change from the two). `others` is level `k` of the
/// other layers' composite (`Ctx::others`): they keep their detail where
/// it is, and only the rest (the written layer's) is carried. Each point
/// reads only the grids, block by block in parallel, joined in block
/// order: the same on any number of threads.
#[allow(clippy::too_many_arguments)]
fn carry(how: &Carry, k: usize, post: &DetailGrid, pre: Option<&DetailGrid>, others: Option<&DetailGrid>, field: &Field, gate: &Gate, mask: Option<&StrokeMask>, lo: Vec3, hi: Vec3, lower: &[(Vec3, f32)]) -> Result<Vec<([i32; 3], f32)>, String> {
    let level = &post.levels()[k];
    let own = &level.grid;
    let (a, b) = level.extent(0.0);
    let (lo, hi) = (lo.max(a), hi.min(b));
    // Other layers' detail of exactly zero is no detail.
    let others = others.filter(|o| o.brick_count() > 0);
    if lo.cmpgt(hi).any() || own.brick_count() == 0 {
        return Ok(Vec::new());
    }
    let voxel = own.voxel;
    let side = 8.0 * voxel;
    type Block = (IVec3, Vec<([i32; 3], f32)>);
    let mut blocks: Vec<Block> = Vec::new();
    match how {
        // The surface moved only where the coarser field changed.
        Carry::Normal => blocks.extend(under(level, lower).into_iter().map(|k| (IVec3::from(k), Vec::new()))),
        Carry::Map(_) => {
            let (k0, k1) = ((lo / side).floor(), (hi / side).floor());
            let count = (k1 - k0 + 1.0).max(Vec3::ZERO).as_dvec3();
            if !count.is_finite() || count.x * count.y * count.z > MAX_CARRY_BLOCKS {
                return Err(format!("the stroke covers too much of a finer detail level (cell {voxel:.5}) to carry it along; use a smaller brush or work at a finer level"));
            }
            let (k0, k1) = (k0.as_ivec3(), k1.as_ivec3());
            for z in k0.z..=k1.z {
                for y in k0.y..=k1.y {
                    for x in k0.x..=k1.x {
                        blocks.push((IVec3::new(x, y, z), Vec::new()));
                    }
                }
            }
        }
    }
    let limit = field.limit;
    // The coarser field's step for its gradient: the coarse cell.
    let e = post.voxel;
    par_chunks(&mut blocks, 16, |_, chunk| {
        for (key, out) in chunk {
            let a = key.as_vec3() * side;
            let b = a + Vec3::splat(side - voxel);
            // (A composite brick that summed to zero is dropped: the other
            // layers' bricks count too.)
            let here = own.brick(&key.to_array()).is_some() || others.is_some_and(|o| o.brick(&key.to_array()).is_some());
            // Skip blocks that hold no detail and cannot take any.
            let reach = match how {
                Carry::Map(map) => {
                    let (s0, s1) = (0..8).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), n| {
                        let c = Vec3::new(if n & 1 == 0 { a.x } else { b.x }, if n & 2 == 0 { a.y } else { b.y }, if n & 4 == 0 { a.z } else { b.z });
                        let q = map.source(c);
                        (l.min(q), h.max(q))
                    });
                    (s0 - 2.0 * voxel, s1 + 2.0 * voxel)
                }
                Carry::Normal => (a - limit - voxel, b + limit + voxel),
            };
            if !here && !own.level_touches(reach.0, reach.1) && !others.is_some_and(|o| o.level_touches(reach.0, reach.1)) {
                continue;
            }
            let list = match how {
                Carry::Normal => field.near(a - e - limit, b + e + limit),
                Carry::Map(_) => Vec::new(),
            };
            for i in 0..512 {
                let g = *key * 8 + IVec3::new(i & 7, (i >> 3) & 7, i >> 6);
                let p = g.as_vec3() * voxel;
                if p.cmplt(lo).any() || p.cmpgt(hi).any() {
                    continue;
                }
                let src = match how {
                    Carry::Map(map) => {
                        let w = if allowed(mask, field, p) { gate.at(p) } else { 0.0 };
                        if w <= 0.0 {
                            continue;
                        }
                        let s = map.source(p);
                        if w >= 1.0 { s } else { p + (s - p) * w }
                    }
                    Carry::Normal => {
                        let pre = pre.expect("a normal carry reads the grid before the dab");
                        let dt = post.parent(k, p) - pre.parent(k, p);
                        if dt.abs() <= 1e-7 {
                            continue;
                        }
                        let t = |q: Vec3| field.sample_base(q, &list).0 + pre.parent(k, q);
                        let grad = Vec3::new(t(p + Vec3::X * e) - t(p - Vec3::X * e), t(p + Vec3::Y * e) - t(p - Vec3::Y * e), t(p + Vec3::Z * e) - t(p - Vec3::Z * e)) / (2.0 * e);
                        let g2 = grad.length_squared();
                        // Far from the surface (a clamped field) or at a
                        // seam the normal says nothing: leave it.
                        if g2.is_nan() || g2 <= 0.01 {
                            continue;
                        }
                        p + (grad * (dt / g2)).clamp_length_max(limit)
                    }
                };
                let old = own.get(g.to_array());
                let new = match others {
                    None => own.level_offset(src),
                    // The written layer's detail (the composite less the
                    // others') moves; the others' stays at `g`.
                    Some(o) => own.level_offset(src) - o.level_offset(src) + o.get(g.to_array()),
                };
                if (new - old).abs() > 1e-7 {
                    out.push((g.to_array(), new));
                }
            }
        }
    });
    Ok(blocks.into_iter().flat_map(|b| b.1).collect())
}

/// Whether the stroke mask lets the brush change `p`.
pub(crate) fn allowed(mask: Option<&StrokeMask>, field: &Field, p: Vec3) -> bool {
    mask.is_none_or(|m| !m.above.is_some_and(|y| p.y < y) && !m.below.is_some_and(|y| p.y > y) && m.blobs.as_ref().is_none_or(|names| field.nearest_named(p, names)))
}

pub(super) fn segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-12 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
    (p - (a + ab * t)).length()
}

pub(super) fn path_distance(p: Vec3, points: &[Vec3]) -> f32 {
    if points.len() == 1 {
        return (p - points[0]).length();
    }
    points.windows(2).map(|w| segment_distance(p, w[0], w[1])).fold(f32::MAX, f32::min)
}

pub(super) fn closest(p: Vec3, points: &[Vec3]) -> Vec3 {
    if points.len() == 1 {
        return points[0];
    }
    points
        .windows(2)
        .map(|w| {
            let ab = w[1] - w[0];
            w[0] + ab * ((p - w[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0)
        })
        .min_by(|a, b| p.distance_squared(*a).total_cmp(&p.distance_squared(*b)))
        .unwrap()
}

/// Compose small injective backward maps around a moving anchor. The
/// displacement Lipschitz constant of each map is below 1/2, so neither
/// it nor the composition can fold or detach a continuous surface.
/// Evaluating backwards advects the old field, with exact tip displacement.
pub(super) fn stretch_source(p: Vec3, points: &[Vec3], delta: Vec3, radius: f32) -> Vec3 {
    let steps = (delta.length() * 8.0 / radius).ceil().max(1.0) as usize;
    let step = delta / steps as f32;
    let mut q = p;
    for j in (1..=steps).rev() {
        let shift = step * j as f32;
        q -= step * legacy(path_distance(q - shift, points) / radius);
    }
    q
}

/// A snakehook's backward map: the material at the path's start is pulled
/// along it, step by step, so the horn follows the path and curls with
/// it. Each step moves everything within 0.4 radii of the moving point
/// fully, fading out by the radius: with steps of an eighth of the radius
/// the move's slope stays under a third, so no step folds the surface.
pub(super) fn hook_source(p: Vec3, points: &[Vec3], radius: f32) -> Vec3 {
    let mut q = p;
    for j in (1..points.len()).rev() {
        let t = q.distance(points[j]) / radius;
        q -= (points[j] - points[j - 1]) * (1.0 - smooth01((t - 0.4) / 0.6));
    }
    q
}

/// The closest point to `p` on a polyline, with the normal there (the
/// per-point `normals` blended along the segment).
pub(super) fn closest_normal(p: Vec3, points: &[Vec3], normals: &[Vec3]) -> (Vec3, Vec3) {
    if points.len() == 1 {
        return (points[0], normals[0]);
    }
    let (mut best, mut at) = (f32::MAX, (points[0], normals[0]));
    for (k, w) in points.windows(2).enumerate() {
        let ab = w[1] - w[0];
        let t = ((p - w[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
        let q = w[0] + ab * t;
        let d = p.distance_squared(q);
        if d < best {
            best = d;
            at = (q, normals[k].lerp(normals[k + 1], t).normalize_or(normals[k]));
        }
    }
    at
}

/// The polyline a dab sweeps, with its radius and strength along it.
pub(crate) struct Sweep<'a> {
    pub points: &'a [Vec3],
    pub radius: f32,
    /// Per point radii, at least the level's smallest radius.
    pub radii: Option<Vec<f32>>,
    pub gains: Option<&'a [f32]>,
}

impl Sweep<'_> {
    pub fn uniform(&self) -> bool {
        self.radii.is_none() && self.gains.is_none()
    }

    pub fn distance(&self, p: Vec3) -> f32 {
        path_distance(p, self.points)
    }

    /// Distance to the path, and the radius and gain at the closest point.
    pub fn locate(&self, p: Vec3) -> (f32, f32, f32) {
        if self.uniform() {
            return (path_distance(p, self.points), self.radius, 1.0);
        }
        let at = |k: usize, s: f32| {
            let r = self.radii.as_ref().map_or(self.radius, |r| if s == 0.0 { r[k] } else { r[k] + (r[k + 1] - r[k]) * s });
            let g = self.gains.map_or(1.0, |g| if s == 0.0 { g[k] } else { g[k] + (g[k + 1] - g[k]) * s });
            (r, g)
        };
        if self.points.len() == 1 {
            let (r, g) = at(0, 0.0);
            return ((p - self.points[0]).length(), r, g);
        }
        let (mut best, mut seg, mut t) = (f32::MAX, 0, 0.0);
        for (k, w) in self.points.windows(2).enumerate() {
            let ab = w[1] - w[0];
            let s = if ab.length_squared() < 1e-12 { 0.0 } else { ((p - w[0]).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
            let d = (p - (w[0] + ab * s)).length();
            if d < best {
                (best, seg, t) = (d, k, s);
            }
        }
        let (r, g) = at(seg, t);
        (best, r, g)
    }

    /// The brush weight at `p`: the falloff at the local radius times the
    /// local gain.
    pub fn weight(&self, curve: &Curve, p: Vec3) -> f32 {
        let (d, r, g) = self.locate(p);
        curve.weight(d, r) * g
    }

    /// The largest radius along the path.
    pub fn reach(&self) -> f32 {
        self.radii.as_ref().map_or(self.radius, |r| r.iter().copied().fold(self.radius, f32::max))
    }

    /// `weight` looking only at the segments `segs` (ascending): the same
    /// value when they hold every segment within `reach` of `p` (a falloff
    /// is 0 at its radius and beyond).
    pub fn weight_among(&self, curve: &Curve, p: Vec3, segs: &[u32]) -> f32 {
        if self.points.len() == 1 {
            return self.weight(curve, p);
        }
        if segs.is_empty() {
            return 0.0;
        }
        if self.uniform() {
            let d = segs.iter().map(|&k| segment_distance(p, self.points[k as usize], self.points[k as usize + 1])).fold(f32::MAX, f32::min);
            return curve.weight(d, self.radius);
        }
        let (mut best, mut seg, mut t) = (f32::MAX, 0, 0.0);
        for &k in segs {
            let (a, b) = (self.points[k as usize], self.points[k as usize + 1]);
            let ab = b - a;
            let s = if ab.length_squared() < 1e-12 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
            let d = (p - (a + ab * s)).length();
            if d < best {
                (best, seg, t) = (d, k as usize, s);
            }
        }
        let r = self.radii.as_ref().map_or(self.radius, |r| if t == 0.0 { r[seg] } else { r[seg] + (r[seg + 1] - r[seg]) * t });
        let g = self.gains.map_or(1.0, |g| if t == 0.0 { g[seg] } else { g[seg] + (g[seg + 1] - g[seg]) * t });
        curve.weight(best, r) * g
    }
}

/// The detail grid spacing a clay gets on its first stroke: its
/// `detail_cell`, else its mesh cell size.
pub(super) fn first_voxel(clay: &Clay, radius: f32) -> f32 {
    clay.detail_cell.or_else(|| clay.cell_size()).unwrap_or(radius / 6.0)
}

/// The field a stroke reads: the blobs plus `detail` (no paint or decals).
pub(super) fn field_for(clay: &Clay, detail: &Detail, voxel: f32) -> Field {
    let probe = Clay { blobs: clay.blobs.clone(), resolution: clay.resolution, detail: Some(detail.clone()), paint: None, detail_cell: clay.detail_cell, shell: clay.shell, sharp: clay.sharp, assets: clay.assets.clone(), ..Clay::default() };
    let field = Field::new(&probe, voxel);
    // The blob field on the detail grid's points, memoized (the same
    // values, see `base_cache.rs`): a dab then reads it with a lookup.
    let cache = crate::clay::base_cache::for_clay(&probe, &field, detail.voxel());
    field.with_base(cache)
}

/// Applies `dabs` in order to `detail` (or a new grid at the clay's
/// detail cell), reading the clay's blobs.
pub fn apply(ctx: &Ctx, detail: Option<&Detail>, dabs: &[Dab], mask: Option<&StrokeMask>) -> Result<Diff, String> {
    let (clay, params) = (ctx.clay, ctx.params);
    if !params.strength.is_finite() {
        return Err("strength must be finite".into());
    }
    if let Some(m) = mask {
        if m.above.is_some_and(|v| !v.is_finite()) || m.below.is_some_and(|v| !v.is_finite()) {
            return Err("mask heights must be finite".into());
        }
        if let Some(names) = &m.blobs {
            for name in names {
                if !clay.blobs.iter().any(|b| &b.name == name) {
                    return Err(format!("unknown mask blob {name}"));
                }
            }
        }
    }
    for d in dabs {
        if d.points.is_empty() || !d.points.iter().all(|p| p.is_finite()) || !d.radius.is_finite() || d.radius <= 0.0 || !d.gain.is_finite() {
            return Err("points and radius must be finite, radius > 0".into());
        }
        d.check()?;
    }
    let Some(first) = dabs.first() else {
        let detail = detail.cloned().unwrap_or_else(|| Detail::new(first_voxel(clay, 1.0)));
        return Ok(Diff { detail, changed_points: 0, radius: 0.0, dabs: 0 });
    };
    let voxel = detail.map_or_else(|| first_voxel(clay, first.radius), Detail::voxel);
    if !voxel.is_finite() || voxel <= 0.0 {
        return Err("clay is too large or broken for sculpting".into());
    }
    if params.alpha.is_some() && ctx.alpha.is_none_or(|a| params.alpha.as_ref().is_some_and(|p| p.hash != a.hash())) {
        return Err("the stroke's alpha image is missing".into());
    }
    let stamp = ctx.alpha.is_some_and(|a| a.vdm().is_some());
    if params.brush == Brush::Vdm && !stamp {
        return Err("brush vdm needs alpha: a VDM stamp (alpha create_vdm), not an image".into());
    }
    if params.brush != Brush::Vdm && stamp {
        return Err("that alpha is a VDM stamp: use it with brush vdm".into());
    }
    let mut detail = detail.cloned().unwrap_or_else(|| Detail::new(voxel));
    let mut field = field_for(clay, &detail, voxel);
    let mut writes = Writes::default();
    let (mut changed_points, mut radius) = (0, first.radius);
    for (n, dab) in dabs.iter().enumerate() {
        if !writes.apart {
            field.set_detail(detail.clone());
        }
        let (changed, used) = apply_dab(params, ctx.alpha, ctx.gate, dab, mask, ctx.others, &mut detail, &mut field, &mut writes)?;
        changed_points += changed;
        radius = used;
        // The field's own grid catches up with this dab's bricks (shared,
        // not copied); its first write copies it away from the clay's.
        if writes.apart && n + 1 < dabs.len() {
            let f = field.detail_mut().expect("the field keeps its grid while apart").grid_mut();
            for (target, keys) in &writes.written {
                f.target_mut(*target).share_bricks(detail.grid().target(*target), keys);
            }
        }
        writes.written.clear();
    }
    Ok(Diff { detail, changed_points, radius, dabs: dabs.len() })
}

/// How a stroke's dabs write the grid without copying it per dab.
///
/// The field shares the engine's grid, and lets go of it before a dab's
/// last write, so that write copies nothing (only the first, away from the
/// clay's own grid). A dab that reaches a refined level writes the coarse
/// grid first, while the field must still read the grid before the dab:
/// from then on the field keeps a grid of its own (`apart`), brought up to
/// date after each dab with just the bricks it wrote. So a stroke copies
/// the grid at most twice, however many dabs it has.
#[derive(Default)]
struct Writes {
    apart: bool,
    /// Levels (None: the coarse grid) pruned in full this stroke; after
    /// that only bricks the stroke writes can turn all zero.
    pruned: Vec<Option<usize>>,
    /// Bricks this dab wrote or dropped, by level.
    written: Vec<(Option<usize>, Vec<[i32; 3]>)>,
}

/// One dab: the coarse grid always takes it; so does every refined level
/// it reaches, each at its own cell (see `clay::Region`).
#[allow(clippy::too_many_arguments)]
fn apply_dab(params: &Params, alpha: Option<&Alpha>, gate: &Gate, dab: &Dab, mask: Option<&StrokeMask>, others: Option<&Detail>, detail: &mut Detail, field: &mut Field, writes: &mut Writes) -> Result<(usize, f32), String> {
    let raw = params.strength * dab.gain;
    let strength = if params.brush.signed_depth() { raw } else { raw.clamp(0.0, 1.0) };
    let (plo, phi) = dab.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    let height = match params.brush {
        Brush::Standard => match stamp_depth(params) {
            Some(d) => d.abs() * dab.gain.abs(),
            None => strength * dab.radius * 0.5,
        },
        Brush::ClayTubes => strength * dab.radius * 0.5,
        _ => 0.0,
    };
    let reach = dab.radius * 2.0 + dab.delta.map_or(0.0, |d| d.length()) + if params.brush.signed_depth() { strength.abs() } else { height } + vdm_reach(params, alpha, dab.radius);
    let mut targets: Vec<Option<usize>> = vec![None];
    // A transform moves everything unmasked: every level it reaches.
    let moved = (params.brush == Brush::Transform).then(|| super::transform::reach(field, params)).flatten();
    // A stroke at a detail level writes the levels at least that coarse;
    // finer levels it reaches (and that hold detail) are carried along.
    let finer = |cell: f32| params.level.is_some_and(|c| cell < c * (1.0 - 1e-4));
    let mut carried: Vec<usize> = Vec::new();
    for (k, l) in detail.grid().levels().iter().enumerate() {
        let (lo, hi) = l.extent(0.0);
        let hit = match moved {
            Some((a, b)) => a.cmple(hi).all() && b.cmpge(lo).all(),
            None => (plo - reach).cmple(hi).all() && (phi + reach).cmpge(lo).all(),
        };
        if !hit {
            continue;
        }
        if !finer(l.grid.voxel) {
            targets.push(Some(k));
        } else if l.grid.brick_count() > 0 {
            carried.push(k);
        }
    }
    if carry_off() {
        carried.clear();
    }
    // Noise finer than a grid can hold (a pore pattern on a 2 mm grid)
    // would come out as coarse lumps; when the stroke reaches a level fine
    // enough for it, coarser levels leave it out (it fades out at that
    // region's edge instead).
    if params.brush == Brush::Noise && targets.len() > 1 {
        let scale = params.scale.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(dab.radius / 10.0);
        let cell = |t: &Option<usize>| t.map_or(detail.voxel(), |k| detail.grid().levels()[k].grid.voxel);
        let fits = |t: &Option<usize>| scale >= 2.0 * cell(t);
        if targets.iter().any(fits) {
            targets.retain(fits);
        }
    }
    // Pressure curves may scale a point past full strength; 0..1 brushes
    // stay at most 1 there (signed depths scale freely).
    let gains: Option<Vec<f32>> = dab.gains.as_ref().map(|g| {
        let cap = if params.brush.signed_depth() { f32::MAX } else { 1.0 / strength.max(1e-6) };
        g.iter().map(|g| g.min(cap)).collect()
    });
    let mut changed = 0;
    let mut used = dab.radius;
    let count = targets.len();
    // How the finer levels go along: a moving brush's map at the radius it
    // moves the dab's level with, else with the surface.
    let how = (!carried.is_empty()).then(|| {
        let cell = params.level.expect("only strokes at a level carry");
        let gain = gains.as_ref().map_or(1.0, |g| g[0]);
        match transport(params, dab, dab.radius.max(cell * 1.5), gain) {
            Some(map) => Carry::Map(map),
            None => Carry::Normal,
        }
    });
    // The normal carry reads the grid before the dab: the field keeps it.
    let keep_pre = matches!(how, Some(Carry::Normal));
    // The grid points the dab's coarser levels changed, with their cells.
    let mut lower: Vec<(Vec3, f32)> = Vec::new();
    for (t, target) in targets.into_iter().enumerate() {
        let (changes, radius) = {
            // Every level reads the grid before the dab (the field's, once
            // the coarse grid has taken the dab), then stores what it adds
            // to the levels under it as they are after it.
            let before = if t == 0 { detail.grid() } else { field.detail().expect("the field keeps the grid before the dab").grid() };
            let view = LevelView::new(before, target);
            let clip = target.map(|k| detail.grid().levels()[k].extent(0.0));
            let v = view.voxel();
            let radius = dab.radius.max(v * 1.5);
            let sweep = Sweep { points: &dab.points, radius, radii: dab.radii.as_ref().map(|r| r.iter().map(|r| r.max(v * 1.5)).collect()), gains: gains.as_deref() };
            let changes = if params.brush == Brush::Noise {
                noise_band(params, &sweep, dab.normal, mask, gate, &view, field, v, radius, strength, clip)?
            } else if params.brush == Brush::Transform {
                super::transform::band(params, mask, gate, &view, field, v, clip)?
            } else {
                brush(params, alpha, gate, dab, &sweep, mask, &view, field, v, radius, strength, clip)?
            };
            // A refined level stores what it adds to the levels under it
            // (already holding this stroke). Where those changed and the
            // brush at this level did not, it takes their change back:
            // inside its region the level's own result shows, the brush
            // applied once.
            let changes = match target {
                None => changes,
                Some(k) => {
                    let g = detail.grid();
                    let mut out: Vec<([i32; 3], f32)> = changes.into_iter().map(|(i, full)| (i, full - g.parent(k, Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v))).collect();
                    out.extend(take_back(k, g, before, &lower, &out));
                    out
                }
            };
            (changes, radius)
        };
        // Where this level changed, for the finer levels' take-back and
        // the normal carry.
        if count > 1 || keep_pre {
            let v = detail.grid().target(target).voxel;
            lower.extend(changes.iter().map(|(i, _)| (Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * v, v)));
        }
        changed += changes.len();
        used = radius;
        if !writes.apart {
            if t + 1 == count && !keep_pre {
                field.take_detail();
            } else {
                writes.apart = true;
            }
        }
        let all = !writes.pruned.contains(&target);
        let g = detail.grid_mut().target_mut(target);
        let keys = g.write(&changes, all);
        if all {
            writes.pruned.push(target);
        }
        if g.brick_count() > MAX_BRICKS {
            return Err(format!("too much sculpt detail (over {MAX_BRICKS} bricks{}); undo some, use a larger cell or a smaller area", if target.is_some() { " in one detail region" } else { "" }));
        }
        writes.written.push((target, keys));
    }
    if let Some(how) = &how {
        // Where the finer detail may move: the dab's reach (a moving
        // brush); the normal carry visits only where the coarser field
        // changed (`under`).
        let (lo, hi) = match how {
            Carry::Map(_) => moved.unwrap_or((plo - reach, phi + reach)),
            Carry::Normal => (Vec3::splat(f32::MIN), Vec3::splat(f32::MAX)),
        };
        // Every carried level from the grids as the dab's levels left
        // them, then written (one level's carry never reads another's).
        let mut all = Vec::new();
        if lo.cmple(hi).all() {
            let pre = field.detail().map(Detail::grid);
            for &k in &carried {
                let theirs = match others {
                    None => None,
                    Some(o) => match o.grid().levels().get(k) {
                        Some(l) if l.grid.voxel == detail.grid().levels()[k].grid.voxel => Some(&l.grid),
                        _ => return Err("sculpt layers no longer line up (their detail cell or regions differ)".into()),
                    },
                };
                all.push((k, carry(how, k, detail.grid(), pre, theirs, field, gate, mask, lo, hi, &lower)?));
            }
        }
        if !writes.apart {
            field.take_detail();
        }
        for (k, changes) in all {
            changed += changes.len();
            let target = Some(k);
            let full = !writes.pruned.contains(&target);
            let g = detail.grid_mut().target_mut(target);
            let keys = g.write(&changes, full);
            if full {
                writes.pruned.push(target);
            }
            if g.brick_count() > MAX_BRICKS {
                return Err(format!("too much sculpt detail (over {MAX_BRICKS} bricks in one detail region); undo some, use a larger cell or a smaller area"));
            }
            writes.written.push((target, keys));
        }
    }
    if detail.grid().total_bricks() > MAX_TOTAL_BRICKS {
        return Err(format!("too much sculpt detail (over {MAX_TOTAL_BRICKS} bricks in all); undo some, or use larger cells or smaller detail regions"));
    }
    Ok((changed, used))
}

/// The bricks of `level` within a coarser cell of a point the dab changed
/// on a coarser level (`lower`: the points and their cells), where the
/// field under the level moved; sorted.
fn under(level: &crate::clay::Level, lower: &[(Vec3, f32)]) -> Vec<[i32; 3]> {
    let (a, b) = level.extent(0.0);
    let side = 8.0 * level.grid.voxel;
    let mut keys: Vec<[i32; 3]> = Vec::new();
    for (p, c) in lower {
        let (lo, hi) = ((*p - *c).max(a), (*p + *c).min(b));
        if lo.cmpgt(hi).any() {
            continue;
        }
        let (k0, k1) = ((lo / side).floor().as_ivec3(), (hi / side).floor().as_ivec3());
        for z in k0.z..=k1.z {
            for y in k0.y..=k1.y {
                for x in k0.x..=k1.x {
                    keys.push([x, y, z]);
                }
            }
        }
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Level `k`'s points the dab's brush left alone (not in `written`) where
/// the coarser levels it wrote (`lower`: their changed points and cells)
/// moved the field under the level: each keeps the field it had, `post`
/// (coarser levels written) against `pre` (the grid before the dab).
/// Bricks in key order, points in brick order, in parallel by brick: the
/// same on any number of threads.
pub(super) fn take_back(k: usize, post: &DetailGrid, pre: &DetailGrid, lower: &[(Vec3, f32)], written: &[([i32; 3], f32)]) -> Vec<([i32; 3], f32)> {
    if lower.is_empty() {
        return Vec::new();
    }
    let level = &post.levels()[k];
    let (own, v) = (&level.grid, level.grid.voxel);
    let (a, b) = level.extent(0.0);
    let keys = under(level, lower);
    let mut done: Vec<[i32; 3]> = written.iter().map(|(i, _)| *i).collect();
    done.sort_unstable();
    type Block = ([i32; 3], Vec<([i32; 3], f32)>);
    let mut blocks: Vec<Block> = keys.into_iter().map(|k| (k, Vec::new())).collect();
    par_chunks(&mut blocks, 16, |_, chunk| {
        for (key, out) in chunk {
            for i in 0..512 {
                let g = [key[0] * 8 + (i & 7), key[1] * 8 + ((i >> 3) & 7), key[2] * 8 + (i >> 6)];
                let p = Vec3::new(g[0] as f32, g[1] as f32, g[2] as f32) * v;
                if p.cmplt(a).any() || p.cmpgt(b).any() || done.binary_search(&g).is_ok() {
                    continue;
                }
                let d = post.parent(k, p) - pre.parent(k, p);
                if d.abs() > 1e-7 {
                    out.push((g, own.get(g) - d));
                }
            }
        }
    });
    blocks.into_iter().flat_map(|b| b.1).collect()
}

/// How far a VDM dab reaches from its point: the stamp's own reach at the
/// dab's radius, plus a radius for the move onto the surface (0 for other
/// brushes).
fn vdm_reach(params: &Params, alpha: Option<&Alpha>, radius: f32) -> f32 {
    match (params.brush, alpha.and_then(Alpha::vdm), &params.alpha) {
        (Brush::Vdm, Some(v), Some(a)) => v.reach() * radius * a.scale / v.radius + radius,
        _ => 0.0,
    }
}

/// A standard dab's alpha depth, when its alpha shapes its height.
fn stamp_depth(params: &Params) -> Option<f32> {
    params.alpha.as_ref().filter(|a| a.usage == alpha::Use::Height).and_then(|a| a.depth)
}

/// Clay buildup fills up to this fraction of the radius over the plane.
pub(super) const BUILDUP: f32 = 0.1;

/// One level's new offsets for a brush dab (all brushes but noise), on
/// grid `grid` of cell `voxel`, only inside the box `clip`.
#[allow(clippy::too_many_arguments)]
fn brush(params: &Params, alpha: Option<&Alpha>, gate: &Gate, dab: &Dab, sweep: &Sweep, mask: Option<&StrokeMask>, grid: &LevelView, field: &Field, voxel: f32, radius: f32, strength: f32, clip: Option<(Vec3, Vec3)>) -> Result<Vec<([i32; 3], f32)>, String> {
    let limit = field.limit;
    let points = &dab.points;

    let delta = match params.brush {
        Brush::Grab | Brush::Stretch => {
            let d = dab.delta.ok_or("grab/stretch needs delta [x, y, z]")?;
            if !d.is_finite() {
                return Err("delta must be finite".into());
            }
            // Pulling farther than the brush folds the surface over itself.
            if params.brush == Brush::Stretch {
                if d.length() > radius * 8.0 {
                    return Err("stretch delta must be at most 8 radii; use successive strokes".into());
                }
                d
            } else {
                d.clamp_length_max(radius)
            }
        }
        _ => Vec3::ZERO,
    };
    let smooth_passes = 1 + (strength.clamp(0.0, 1.0) * 7.0).round() as usize;
    // Standard: the dab's height along the area normal (an alpha's depth
    // when it gives one); clay tubes: the strip's height.
    let height = if params.invert { -1.0 } else { 1.0 }
        * match (params.brush, stamp_depth(params)) {
            (Brush::Standard, Some(d)) => d * dab.gain,
            _ => strength * radius * 0.5,
        };

    // Region of grid points the brush can read or change.
    let margin = match params.brush {
        Brush::Add | Brush::Carve => radius * 1.6,
        Brush::Smooth => radius + (smooth_passes + 1) as f32 * voxel,
        // The guard reads two points around each one it moves.
        Brush::ClayPolish => radius + 3.0 * voxel,
        Brush::Grab | Brush::Stretch => radius + delta.length(),
        Brush::Crease | Brush::Layer => radius + strength.abs(),
        Brush::Standard => radius + height.abs(),
        // Square footprints reach sqrt(2) radii; planes sit within one.
        Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish => radius * 1.5,
        Brush::ClayTubes => radius * 1.5 + height.abs(),
        Brush::Vdm => vdm_reach(params, alpha, radius),
        _ => radius,
    } + 2.0 * voxel;
    let (plo, phi) = points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
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

    // The area normal: the mean surface normal at the dab's center and
    // half a radius around it (normals read where the field is not
    // clamped, so the slope is the surface's at any dab size).
    let area_normal = |c: Vec3| {
        let list = field.near(c - radius - voxel, c + radius + voxel);
        let n0 = field.sculpt_normal(c, &list, voxel);
        let (u, v) = n0.any_orthonormal_pair();
        let h = radius * 0.5;
        [u * h, -u * h, v * h, -v * h].iter().fold(n0, |sum, o| sum + field.sculpt_normal(c + *o, &list, voxel)).normalize_or(n0)
    };
    let given = dab.normal.filter(|n| n.is_finite() && n.length() > 1e-6).map(|n| n.normalize());
    let normal = match params.brush {
        Brush::Flatten | Brush::Crease | Brush::Layer | Brush::Pinch => match given {
            Some(n) => n,
            None => {
                let c = points.iter().copied().sum::<Vec3>() / points.len() as f32;
                let list = field.near(c - radius, c + radius);
                let f = |p: Vec3| field.sample(p, &list).0;
                let e = voxel;
                Vec3::new(f(c + Vec3::X * e) - f(c - Vec3::X * e), f(c + Vec3::Y * e) - f(c - Vec3::Y * e), f(c + Vec3::Z * e) - f(c - Vec3::Z * e)).normalize_or(Vec3::Y)
            }
        },
        Brush::Standard | Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish | Brush::Vdm => given.unwrap_or_else(|| area_normal(points[0])),
        _ => Vec3::Y,
    };
    // The plane brushes: the area's plane. Points over the dab go onto the
    // surface along the first normal (Newton steps on the field); the
    // plane's normal is the mean surface normal there (unless given) and
    // its offset from the dab's center the mean height, both summed in a
    // fixed order.
    // A VDM stamp stands on that plane too.
    let (normal, plane) = if params.brush.planar() || params.brush == Brush::Vdm {
        let c = points[0];
        let list = field.near(c - radius * 2.5, c + radius * 2.5);
        let (u, v) = alpha::frame(normal, dab.tangent, dab.flip, 0.0);
        let mut on = Vec::with_capacity(17);
        for (reach, around) in [(0.0, 1), (0.45, 8), (0.85, 8)] {
            for j in 0..around {
                let a = std::f32::consts::TAU * j as f32 / around as f32;
                let q = c + (u * a.cos() + v * a.sin()) * radius * reach;
                let mut s = 0.0f32;
                for _ in 0..4 {
                    s = (s - field.sample(q + normal * s, &list).0).clamp(-radius, radius);
                }
                on.push(q + normal * s);
            }
        }
        let n = given.unwrap_or_else(|| on.iter().fold(Vec3::ZERO, |sum, p| sum + field.sculpt_normal(*p, &list, voxel)).normalize_or(normal));
        let k = on.iter().map(|p| (*p - c).dot(n)).sum::<f32>() / on.len() as f32;
        (n, k)
    } else {
        (normal, 0.0)
    };
    // Dab brushes: the dab's frame on the surface (footprints, alphas).
    let rotate = params.alpha.as_ref().map_or(0.0, |a| a.rotate);
    let frame = alpha::frame(normal, dab.tangent, dab.flip, rotate);
    // A dragged alpha turns with the stroke (the footprint's frame); stamp
    // and tile alphas stay upright on the object.
    let upright = |a: &crate::brush::alpha::AlphaParams| if a.mode == alpha::Mode::Drag { frame } else { alpha::frame(normal, Some(dab.upright.unwrap_or(Vec3::X)), dab.flip, rotate) };
    // A VDM stamp stands on the area's plane under the dab's point, in the
    // frame an alpha takes.
    let vdm = match (params.brush, alpha.and_then(Alpha::vdm), &params.alpha) {
        (Brush::Vdm, Some(v), Some(a)) => {
            let (u, w) = upright(a);
            Some(vdm::Stamp { vdm: v, center: points[0] + normal * plane, u, v: w, n: normal, scale: radius * a.scale / v.radius, limit, invert: params.invert })
        }
        (Brush::Vdm, ..) => return Err("brush vdm needs alpha: a VDM stamp (alpha create_vdm)".into()),
        _ => None,
    };
    let stamp = match (alpha, &params.alpha) {
        _ if vdm.is_some() => None,
        (Some(image), Some(a)) => {
            let (u, v) = upright(a);
            Some(Stamp { alpha: image, params: a, center: points[0], u, v, half: radius * a.scale })
        }
        _ => None,
    };
    // Clay tubes: the surface normal at each path point, from the field
    // before the stroke.
    let normals: Option<Vec<Vec3>> = (params.brush == Brush::ClayTubes).then(|| match given {
        Some(n) => vec![n; points.len()],
        None => points.iter().map(|c| area_normal(*c)).collect(),
    });
    let job = Job { params, sweep, mask, gate, dab_normal: given, field, grid, voxel, radius, strength, delta, smooth_passes, normal, height, frame, stamp, vdm, plane, normals: normals.as_deref() };
    let n = i1 - i0 + 1;
    let changes = if (n.x as f64) * (n.y as f64) * (n.z as f64) <= MAX_REGION as f64 {
        job.region(i0, i1, i0, i1)
    } else {
        // Its volume shift is one sum over the dab.
        if params.brush == Brush::ClayPolish {
            return Err(format!("a clay_polish dab is too big for the detail grid at cell size {voxel:.4}; use a smaller radius, or edit_clay polish with a region"));
        }
        // Auto-masking reads the surface around the dab as one region.
        if gate.auto.is_some() {
            return Err(format!("the stroke is too big to auto-mask (backface, cavity, connected, normal_angle) at cell size {voxel:.4}; use a smaller radius"));
        }
        // Too big to hold at once: tiles, skipping those far from the
        // surface (a brush only matters where the surface is).
        let reach = match params.brush {
            Brush::Grab | Brush::Stretch => delta.length(),
            Brush::Crease | Brush::Layer => strength.abs(),
            Brush::Standard => height.abs(),
            Brush::Pinch => radius,
            Brush::Inflate | Brush::Deflate => radius * 0.5 * strength,
            Brush::Add | Brush::Carve => radius * 2.0,
            Brush::Flatten | Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish => radius,
            Brush::ClayTubes => radius + height.abs(),
            Brush::Snakehook => hook_reach(points),
            Brush::Vdm => vdm_reach(params, alpha, radius),
            _ => 0.0,
        } + limit
            + 3.0 * voxel;
        let halo = if params.brush == Brush::Smooth { smooth_passes as i32 + 1 } else { 1 };
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
            if box_path_gap(lo, hi, points) > margin {
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

/// Gap between a box and the nearest stroke point's segment path.
fn box_path_gap(lo: Vec3, hi: Vec3, points: &[Vec3]) -> f32 {
    let c = (lo + hi) * 0.5;
    (path_distance(c, points) - (hi - lo).length() * 0.5).max(0.0)
}

/// What `displace` did.
pub struct Displaced {
    pub changed_points: usize,
}

/// Displaces a clay's surface by `height(p)` (scene units, + out) inside
/// the box `lo..hi`: whole-area detail such as a face's folds
/// (`face_detail`). `depth` bounds |height|; `near(block center, block
/// side)` skips blocks the height cannot reach. Like a stroke, it goes into
/// the coarse grid and every detail region it reaches.
pub fn displace(clay: &mut Clay, lo: Vec3, hi: Vec3, depth: f32, near: &(dyn Fn(Vec3, f32) -> bool + Sync), height: &(dyn Fn(Vec3) -> f32 + Sync)) -> Result<Displaced, String> {
    let voxel = clay.detail.as_ref().map_or_else(|| first_voxel(clay, (hi - lo).max_element()), Detail::voxel);
    if !voxel.is_finite() || voxel <= 0.0 || !lo.is_finite() || !hi.is_finite() {
        return Err("clay is too large or broken for sculpting".into());
    }
    let mut detail = clay.detail.clone().unwrap_or_else(|| Detail::new(voxel));
    let field = field_for(clay, &detail, voxel);
    let mut targets: Vec<Option<usize>> = vec![None];
    for (k, l) in detail.grid().levels().iter().enumerate() {
        let (a, b) = l.extent(0.0);
        if lo.cmple(b).all() && hi.cmpge(a).all() {
            targets.push(Some(k));
        }
    }
    let mut report = Displaced { changed_points: 0 };
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
    if detail.grid().total_bricks() > MAX_TOTAL_BRICKS {
        return Err(format!("too much sculpt detail (over {MAX_TOTAL_BRICKS} bricks in all); use larger cells"));
    }
    clay.detail = Some(detail);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clay::{DetailGrid, Region, grid_copies};

    fn ball(voxel: f32) -> Clay {
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 1.0 })).unwrap();
        Clay { blobs: vec![blob], resolution: 48, detail_cell: Some(voxel), ..Default::default() }
    }

    /// A coarse grid of `n` bricks, one small offset each, all well away
    /// from the ball (a clay that has been sculpted a lot elsewhere).
    fn crowded(voxel: f32, n: usize) -> Detail {
        let mut d = Detail::new(voxel);
        let g = d.grid_mut();
        let side = (n as f32).cbrt().ceil() as i32;
        for k in 0..n as i32 {
            g.set([(40 + k % side) * 8, (k / side % side) * 8, (k / side / side) * 8], 1e-3);
        }
        d
    }

    /// `n` small dabs in a ring on the ball's top.
    fn ring(n: usize, radius: f32) -> Vec<Dab> {
        (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                Dab::path(vec![Vec3::new(0.436 * a.cos(), 0.9, 0.436 * a.sin())], radius)
            })
            .collect()
    }

    fn bricks(g: &DetailGrid) -> Vec<([i32; 3], Vec<u32>)> {
        let mut out: Vec<_> = g.bricks().map(|(k, b)| (*k, b.iter().map(|v| v.to_bits()).collect())).collect();
        out.sort();
        out
    }

    fn same(a: &Detail, b: &Detail) -> bool {
        let (a, b) = (a.grid(), b.grid());
        bricks(a) == bricks(b) && a.levels().len() == b.levels().len() && a.levels().iter().zip(b.levels()).all(|(x, y)| bricks(&x.grid) == bricks(&y.grid))
    }

    #[test]
    fn a_long_stroke_on_a_big_grid_copies_it_at_most_twice() {
        let voxel = 0.02;
        let mut clay = ball(voxel);
        clay.detail = Some(crowded(voxel, 30_000));
        let params = Params::new(Brush::Standard, 0.2);
        let dabs = ring(crate::brush::resolve::MAX_DABS, 0.03);
        let ctx = Ctx { clay: &clay, params: &params, alpha: None, gate: &Gate::default(), others: None };
        let (before, start) = (grid_copies(), std::time::Instant::now());
        let diff = apply(&ctx, clay.detail.as_ref(), &dabs, None).unwrap();
        let (copies, took) = (grid_copies() - before, start.elapsed());
        eprintln!("{} dabs on {} bricks: {copies} grid copies, {took:?}", dabs.len(), diff.detail.grid().brick_count());
        assert!(diff.changed_points > 0);
        // Coarse only: one copy, away from the clay's own grid.
        assert_eq!(copies, 1, "{copies} grid copies");
        assert!(took.as_secs_f32() < 30.0, "{took:?}");

        // With a refined region every dab writes two levels: the field
        // keeps its own grid, so two copies of both levels.
        let mut clay = ball(voxel);
        clay.detail = Some(crowded(voxel, 30_000));
        clay.set_detail_regions(&[Region { name: "top".into(), center: Vec3::new(0.0, 0.9, 0.0), radii: Vec3::splat(0.6), cell: 0.01 }]).unwrap();
        let ctx = Ctx { clay: &clay, params: &params, alpha: None, gate: &Gate::default(), others: None };
        let (before, start) = (grid_copies(), std::time::Instant::now());
        let diff = apply(&ctx, clay.detail.as_ref(), &dabs, None).unwrap();
        let (copies, took) = (grid_copies() - before, start.elapsed());
        eprintln!("{} dabs with a region: {copies} grid copies, {took:?}", dabs.len());
        assert!(diff.detail.grid().levels()[0].grid.brick_count() > 0);
        assert!(copies <= 4, "{copies} grid copies");
        assert!(took.as_secs_f32() < 60.0, "{took:?}");
    }

    /// `DetailGrid::write` (brick runs, the clean flag) leaves exactly what
    /// setting every point and then pruning every brick leaves, and lists
    /// the bricks a copy must take over.
    #[test]
    fn grid_writes_match_point_by_point_sets_and_a_full_prune() {
        let mut rng = crate::brush::rng::Pcg32::new(5, 9);
        let mut g = Detail::new(0.01);
        for _ in 0..4000 {
            let i = [(rng.next_f32() * 64.0) as i32 - 32, (rng.next_f32() * 64.0) as i32 - 32, (rng.next_f32() * 64.0) as i32 - 32];
            g.grid_mut().set(i, rng.next_f32() - 0.5);
        }
        // A brick whose values are all within the prune threshold.
        g.grid_mut().set([200, 0, 0], 5e-8);
        let mut grid = g.grid().clone();
        for round in 0..6 {
            // Changes in a brush's order (x fastest) over a box, some zero,
            // some tiny, the box crossing negative coordinates.
            let o = [(rng.next_f32() * 40.0) as i32 - 20, (rng.next_f32() * 40.0) as i32 - 20, (rng.next_f32() * 40.0) as i32 - 20];
            let mut changes = Vec::new();
            for z in 0..13 {
                for y in 0..11 {
                    for x in 0..19 {
                        let v = match (x + y + z + round) % 5 {
                            0 => 0.0,
                            1 => 3e-8,
                            _ => rng.next_f32() - 0.5,
                        };
                        changes.push(([o[0] + x, o[1] + y, o[2] + z], v));
                    }
                }
            }
            if round == 3 {
                // An outside set leaves a near-zero brick on a clean grid:
                // the next full write must prune it again.
                grid.set([-200, 0, 0], 1e-8);
            }
            let all = round % 2 == 0 || round == 3;
            let mut want = grid.clone();
            for (i, v) in &changes {
                want.set(*i, *v);
            }
            let mut keys: Vec<[i32; 3]> = changes.iter().map(|(i, _)| crate::clay::split(*i).0).collect();
            if all {
                let before: Vec<[i32; 3]> = want.brick_keys().copied().collect();
                want.prune();
                keys.extend(before.into_iter().filter(|k| want.brick(k).is_none()));
            } else {
                for k in keys.clone() {
                    if want.brick(&k).is_some_and(|b| !b.iter().any(|v| v.abs() > 1e-7)) {
                        // (an empty grid's brick there: none, so it goes)
                        want.share_bricks(&DetailGrid::default(), &[k]);
                    }
                }
            }
            keys.sort_unstable();
            keys.dedup();
            let mut got_keys = grid.write(&changes, all);
            got_keys.sort_unstable();
            got_keys.dedup();
            assert_eq!(got_keys, keys, "round {round}");
            assert_eq!(bricks(&grid), bricks(&want), "round {round}");
        }
    }

    /// One stroke of many dabs gives exactly what the same dabs give as
    /// strokes of one dab each (each a fresh field on the grid before it).
    #[test]
    fn a_stroke_matches_its_dabs_one_by_one() {
        let voxel = 0.02;
        // No region; one the first dab reaches; one only later dabs reach
        // (the field keeps its own grid from mid-stroke on).
        for region in [None, Some(0.3), Some(-0.3)] {
            for brush in [Brush::Standard, Brush::Smooth, Brush::Inflate] {
                let mut clay = ball(voxel);
                clay.detail = Some(crowded(voxel, 500));
                if let Some(x) = region {
                    clay.set_detail_regions(&[Region { name: "top".into(), center: Vec3::new(x, 0.9, 0.0), radii: Vec3::splat(0.3), cell: 0.01 }]).unwrap();
                }
                let region = region.is_some();
                let params = Params::new(brush, 0.4);
                let dabs = ring(40, 0.06);
                let whole = apply(&Ctx { clay: &clay, params: &params, alpha: None, gate: &Gate::default(), others: None }, clay.detail.as_ref(), &dabs, None).unwrap();
                let mut one = clay.clone();
                for dab in &dabs {
                    let diff = apply(&Ctx { clay: &one, params: &params, alpha: None, gate: &Gate::default(), others: None }, one.detail.as_ref(), std::slice::from_ref(dab), None).unwrap();
                    one.detail = Some(diff.detail);
                }
                assert!(whole.changed_points > 0 && (!region || whole.detail.grid().levels()[0].grid.brick_count() > 0), "{brush:?} region {region}");
                assert!(same(&whole.detail, one.detail.as_ref().unwrap()), "{brush:?} region {region}");
                // The clay's own grid is left as it was.
                assert_eq!(clay.detail.as_ref().unwrap().grid().brick_count(), 500);
            }
        }
    }
}
