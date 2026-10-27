//! Proportion guides (docs/QUALITY_PLAN.md Stage 5b): head units, named
//! lines and proportion targets kept with the scene (`Scene.guides`), and
//! their measurement against a model and a turnaround sheet.
//!
//! Guides are not objects. They are drawn over the viewport and over
//! `render_views guides: true` tiles after the render, as the skeleton is,
//! so exports, print, film, picking, masks and silhouettes never see them.
//!
//! Extents are measured on the clay's field (D12), not on its mesh: a
//! blob's on its own shape, the model's on the composed field. Each is the
//! highest (or lowest) point of the solid along an axis, found by a
//! best-first search over boxes that skips only boxes the distance shows
//! to be empty, down to a quarter of the clay's cell (so a part about as
//! thin as the cell is not stepped over; much thinner ones can be), carried to the
//! surface along a column and refined by a compass search over the
//! columns, so a sphere's top comes out within about 1e-6 of its size at
//! any mesh resolution.
//! Everything runs in a fixed order on one thread: the same scene gives
//! the same numbers.

use std::cell::RefCell;
use std::collections::BTreeMap;

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clay::{BlobShape, Clay, Field, MAX_BLOBS};

/// Head units: at least this many and at most `MAX_UNITS`.
pub const MIN_UNITS: u32 = 2;
pub const MAX_UNITS: u32 = 8;
/// Most lines and targets a scene keeps.
pub const MAX_LINES: usize = 32;
pub const MAX_TARGETS: usize = 32;
/// Most names in one target's `of`.
pub const MAX_OF: usize = 16;
/// Longest name, in characters.
pub const MAX_NAME: usize = 64;
/// Coordinates, values and tolerances are at most this big.
pub const MAX_COORD: f32 = 1.0e4;
/// Named blobs listed in head units.
pub const MAX_LISTED: usize = 64;

/// The names `to` and `per` give special meanings.
pub const TOTAL_HEIGHT: &str = "total_height";
pub const TOTAL_WIDTH: &str = "total_width";
pub const TOTAL_DEPTH: &str = "total_depth";
pub const HEAD_UNIT: &str = "head_unit";
pub const SHEET_HEIGHT: &str = "sheet_height";

/// f32s written as the shortest decimal that reads back the same f32
/// (0.92, not 0.9200000166893005).
mod short {
    use serde::Serializer;

    pub fn f(x: f32) -> f64 {
        format!("{x}").parse().unwrap_or(x as f64)
    }

    pub fn one<S: Serializer>(x: &f32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(f(*x))
    }

    pub fn opt<S: Serializer>(x: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
        match x {
            Some(v) => s.serialize_some(&f(*v)),
            None => s.serialize_none(),
        }
    }

    pub fn three<S: Serializer>(p: &[f32; 3], s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&p.map(f), s)
    }
}

/// A world axis.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        ["x", "y", "z"][self as usize]
    }

    pub fn unit(self) -> Vec3 {
        [Vec3::X, Vec3::Y, Vec3::Z][self as usize]
    }
}

/// Horizontal lines every head unit, from `bottom` to `top` (world y).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct HeadUnits {
    pub count: u32,
    #[serde(serialize_with = "short::one")]
    pub bottom: f32,
    #[serde(serialize_with = "short::one")]
    pub top: f32,
}

impl HeadUnits {
    /// One head unit, in world units.
    pub fn unit(&self) -> f32 {
        (self.top - self.bottom) / self.count as f32
    }

    /// The lines' heights, bottom to top (count + 1 of them).
    pub fn lines(&self) -> Vec<f32> {
        (0..=self.count).map(|k| if k == self.count { self.top } else { self.bottom + self.unit() * k as f32 }).collect()
    }

    /// Head units from the top line down to height `y`.
    pub fn heads_down(&self, y: f32) -> f32 {
        (self.top - y) / self.unit()
    }
}

/// A named plane across one world axis: `{name, y}` (eye line), `{name,
/// x}` (centre line) or `{name, z}`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Line {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "short::opt")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "short::opt")]
    pub y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "short::opt")]
    pub z: Option<f32>,
}

impl Line {
    /// Its axis and where it crosses it (`check` makes sure there is one).
    pub fn at(&self) -> (Axis, f32) {
        match (self.x, self.y, self.z) {
            (Some(v), _, _) => (Axis::X, v),
            (_, Some(v), _) => (Axis::Y, v),
            (_, _, Some(v)) => (Axis::Z, v),
            _ => (Axis::Y, 0.0),
        }
    }
}

/// One end of a `between`: a world point, or a name (a blob or object:
/// its centre; a line: the plane).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(untagged)]
pub enum End {
    At(#[serde(serialize_with = "short::three")] [f32; 3]),
    Name(String),
}

/// A proportion target: a ratio of lengths (`of` over `to`) or a distance
/// (`between` two ends, perhaps `per` a length), within `tolerance` of
/// `value`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Target {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub of: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub between: Vec<End>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<Axis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<String>,
    /// The axis `per`'s part is measured along (default y, its height,
    /// whatever `axis` the distance takes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_axis: Option<Axis>,
    #[serde(serialize_with = "short::one")]
    pub value: f32,
    #[serde(serialize_with = "short::one")]
    pub tolerance: f32,
}

/// The scene's guides (saved with it; setting or clearing them is an undo
/// step).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Guides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_units: Option<HeadUnits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
}

/// Reads a file's guides, or none when they do not parse (the rest of the
/// scene still loads; `Scene::sanitize` drops ones out of bounds).
pub fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Guides, D::Error> {
    let v = Value::deserialize(d)?;
    let mut g: Guides = serde_json::from_value(v).unwrap_or_default();
    g.migrate();
    Ok(g)
}

/// Whether `per` names a part (a blob or object), not one of the lengths
/// `to` and `per` give a meaning (total_height, head_unit, ...).
pub fn per_is_part(per: &str) -> bool {
    ![TOTAL_HEIGHT, TOTAL_WIDTH, TOTAL_DEPTH, HEAD_UNIT, SHEET_HEIGHT].contains(&per)
}

fn coord(what: &str, v: f32) -> Result<(), String> {
    if v.is_finite() && v.abs() <= MAX_COORD {
        Ok(())
    } else {
        Err(format!("{what} must be a number from -{MAX_COORD} to {MAX_COORD}"))
    }
}

fn check_name(what: &str, n: &str) -> Result<(), String> {
    if n.trim().is_empty() || n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(format!("{what} must be a name of 1 to {MAX_NAME} characters"));
    }
    Ok(())
}

impl Guides {
    pub fn is_empty(&self) -> bool {
        self.head_units.is_none() && self.lines.is_empty() && self.targets.is_empty()
    }

    /// Reads guides saved before `per_axis` as they were measured then:
    /// a part `per` was taken along the distance's `axis` (its height
    /// without one). `guides set` writes `per_axis` on every part `per`,
    /// so only those older files have a part `per` without it. A
    /// `per_axis` on a length that is not a part (head_unit, total_width,
    /// ...) never chose anything and goes.
    pub fn migrate(&mut self) {
        for t in &mut self.targets {
            match t.per.as_deref() {
                Some(per) if per_is_part(per) => {
                    if t.per_axis.is_none() {
                        t.per_axis = Some(t.axis.unwrap_or(Axis::Y));
                    }
                }
                _ => t.per_axis = None,
            }
        }
    }

    /// Every bound and rule (`guides set` refuses, a file's damaged guides
    /// are dropped on load).
    pub fn check(&self) -> Result<(), String> {
        if let Some(h) = &self.head_units {
            if !(MIN_UNITS..=MAX_UNITS).contains(&h.count) {
                return Err(format!("head_units.count must be {MIN_UNITS} to {MAX_UNITS} (got {})", h.count));
            }
            coord("head_units.bottom", h.bottom)?;
            coord("head_units.top", h.top)?;
            if h.top - h.bottom <= 1e-6 {
                return Err(format!("head_units.top ({}) must be above head_units.bottom ({})", h.top, h.bottom));
            }
        }
        if self.lines.len() > MAX_LINES {
            return Err(format!("at most {MAX_LINES} lines (got {})", self.lines.len()));
        }
        if self.targets.len() > MAX_TARGETS {
            return Err(format!("at most {MAX_TARGETS} targets (got {})", self.targets.len()));
        }
        for (i, l) in self.lines.iter().enumerate() {
            check_name(&format!("lines[{i}].name"), &l.name)?;
            if self.lines[..i].iter().any(|o| o.name == l.name) {
                return Err(format!("two lines are called {}", l.name));
            }
            let given: Vec<f32> = [l.x, l.y, l.z].into_iter().flatten().collect();
            if given.len() != 1 {
                return Err(format!("line {} needs exactly one of x, y or z", l.name));
            }
            coord(&format!("line {}", l.name), given[0])?;
        }
        for (i, t) in self.targets.iter().enumerate() {
            check_name(&format!("targets[{i}].name"), &t.name)?;
            if self.targets[..i].iter().any(|o| o.name == t.name) {
                return Err(format!("two targets are called {}", t.name));
            }
            let what = format!("target {}", t.name);
            coord(&format!("{what}: value"), t.value)?;
            if !t.tolerance.is_finite() || !(0.0..=MAX_COORD).contains(&t.tolerance) {
                return Err(format!("{what}: tolerance must be a number from 0 to {MAX_COORD}"));
            }
            if t.per_axis.is_some() && !t.per.as_deref().is_some_and(per_is_part) {
                return Err(format!("{what}: per_axis goes with per naming a part (head_unit and the totals have their own axis)"));
            }
            match (t.of.is_empty(), t.between.is_empty()) {
                (false, true) => {
                    if t.of.len() > MAX_OF {
                        return Err(format!("{what}: at most {MAX_OF} names in of"));
                    }
                    for n in &t.of {
                        check_name(&format!("{what}: of"), n)?;
                    }
                    if t.per.is_some() {
                        return Err(format!("{what}: per goes with between (of uses to)"));
                    }
                    check_name(&format!("{what}: to"), t.to.as_deref().unwrap_or(""))?;
                }
                (true, false) => {
                    if t.between.len() != 2 {
                        return Err(format!("{what}: between takes exactly two ends"));
                    }
                    if t.to.is_some() {
                        return Err(format!("{what}: to goes with of (between uses per)"));
                    }
                    for e in &t.between {
                        match e {
                            End::At(p) => p.iter().try_for_each(|v| coord(&format!("{what}: a between point"), *v))?,
                            End::Name(n) => check_name(&format!("{what}: between"), n)?,
                        }
                    }
                    if let Some(p) = &t.per {
                        check_name(&format!("{what}: per"), p)?;
                    }
                }
                _ => return Err(format!("{what} needs either of (a ratio of lengths) or between (a distance), not both")),
            }
        }
        Ok(())
    }

    fn line(&self, name: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.name == name)
    }
}

/// What an extent search reads from a solid: whether a point is inside,
/// and a lower bound on the distance from a box's centre to the solid
/// (within a factor `LIPSCHITZ`), reading only what can reach that box.
pub struct Solid<'a> {
    pub inside: &'a dyn Fn(Vec3) -> bool,
    pub distance: &'a dyn Fn(Vec3, Vec3) -> f32,
}

/// The highest and lowest points along `axis` of the solid in the box
/// `lo..hi` (the box may be flat across other axes: a section). `leaf` is
/// the smallest feature the search must not step over (the clay's cell),
/// at least 1/256 of the box's size. None when the box holds no solid.
pub fn extent(solid: &Solid, lo: Vec3, hi: Vec3, axis: Axis, leaf: f32) -> Option<(f32, f32)> {
    let low = support(solid, lo, hi, axis, true, leaf)?;
    let high = support(solid, lo, hi, axis, false, leaf)?;
    Some((low, high))
}

/// Bisection steps on a column (2^-34 of its length).
const BISECT: usize = 34;
/// How far the distances may understate how far the solid is: a box is
/// skipped only when its centre is more than this many half-diagonals
/// from the solid (blob distances are about right, not exact).
const LIPSCHITZ: f32 = 2.0;
/// Most boxes one search looks at (it stops there with what it found).
const MAX_BOXES: usize = 200_000;
/// The finest leaf, as a part of the box's size, so a fine detail cell on
/// a big flat face cannot make the search look at millions of boxes.
const MIN_LEAF: f32 = 1.0 / 256.0;

/// A box waiting in the search: its face furthest out (`top`, signed so
/// more is further out) and the order it was made in, so ties pop in a
/// fixed order.
struct Pending {
    top: f32,
    seq: u64,
    lo: Vec3,
    hi: Vec3,
}

impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == std::cmp::Ordering::Equal
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.top.total_cmp(&o.top).then(o.seq.cmp(&self.seq))
    }
}

/// The highest (`low`: lowest) coordinate along `axis` of the solid: a
/// best-first search over boxes (the box reaching furthest out first,
/// skipped when the distance bound says it holds no solid, split into
/// eight otherwise, down to `leaf`), so a thin part (a horn, an ear tip)
/// is found wherever it is; each solid point found is carried out to the
/// surface along its column by bisection, and the best column is then
/// refined by a compass search to about 1e-7 of the size. Sequential and
/// in a fixed order: the same solid gives the same bits.
fn support(solid: &Solid, lo: Vec3, hi: Vec3, axis: Axis, low: bool, leaf: f32) -> Option<f32> {
    let inside = solid.inside;
    let a = axis.index();
    let (u, v) = ((a + 1) % 3, (a + 2) % 3);
    let size = (hi - lo).max_element().max(1e-6);
    let pad = size * 1e-3;
    let leaf = leaf.max(size * MIN_LEAF);
    // From outside (`start`) inward to `end`; `s` turns "higher" into "more".
    let (start, end, s) = if low { (lo[a] - pad, hi[a] + pad, -1.0f32) } else { (hi[a] + pad, lo[a] - pad, 1.0f32) };
    let at = |cu: f32, cv: f32, h: f32| {
        let mut p = Vec3::ZERO;
        p[a] = h;
        p[u] = cu;
        p[v] = cv;
        p
    };
    // Between an outside height and an inside one: the crossing.
    let bisect = |cu: f32, cv: f32, mut out: f32, mut inn: f32| {
        for _ in 0..BISECT {
            let mid = 0.5 * (out + inn);
            if inside(at(cu, cv, mid)) {
                inn = mid;
            } else {
                out = mid;
            }
        }
        0.5 * (out + inn)
    };
    // From a solid point out along its column to the surface (a crossing
    // at least as far out as the point).
    let crossing = |cu: f32, cv: f32, h: f32| if inside(at(cu, cv, start)) { start } else { bisect(cu, cv, start, h) };
    let flat = |d: usize| hi[d] - lo[d] <= 1e-9;
    let (mut blo, mut bhi) = (lo, hi);
    blo[a] = lo[a] - pad;
    bhi[a] = hi[a] + pad;
    let top_of = |l: Vec3, h: Vec3| if s > 0.0 { h[a] } else { -l[a] };
    let mut queue = std::collections::BinaryHeap::new();
    let mut seq = 0u64;
    queue.push(Pending { top: top_of(blo, bhi), seq, lo: blo, hi: bhi });
    // The best crossing so far, signed (more is further out), and its column.
    let mut best: Option<(f32, f32, f32)> = None;
    let mut boxes = 0usize;
    while let Some(Pending { top, lo: mut l, hi: mut h, .. }) = queue.pop() {
        boxes += 1;
        if boxes > MAX_BOXES {
            break;
        }
        if let Some((b, _, _)) = best {
            // What is left reaches at most half a leaf past the best: the
            // compass search below finishes from the best column.
            if top <= b + leaf * 0.5 {
                break;
            }
            // Only what lies further out than the best can improve it.
            if s > 0.0 {
                l[a] = l[a].max(b);
            } else {
                h[a] = h[a].min(-b);
            }
        }
        let c = (l + h) * 0.5;
        let r = (h - l).length() * 0.5;
        let d = (solid.distance)(l, h);
        if d > LIPSCHITZ * r {
            continue;
        }
        let extent = (0..3).filter(|&k| !(k != a && flat(k))).map(|k| h[k] - l[k]).fold(0.0f32, f32::max);
        if inside(c) {
            let x = crossing(c[u], c[v], c[a]);
            if best.is_none_or(|(b, _, _)| s * x > b) {
                best = Some((s * x, c[u], c[v]));
            }
        }
        // Split down to a leaf; a leaf whose centre is close to the solid
        // (a part thinner than a leaf passing near it) splits on to a
        // quarter leaf. A leaf over a flat face is not close, so a flat
        // top costs (its width / leaf)^2 boxes, not more.
        if extent <= leaf && !(extent > leaf * 0.25 && d <= 0.5 * r) {
            continue;
        }
        // Eight children (fewer across flat axes), in a fixed order.
        let split = |k: usize| !(k != a && flat(k)) && h[k] - l[k] > leaf * 0.125;
        let halves = |k: usize| if split(k) { vec![(l[k], c[k]), (c[k], h[k])] } else { vec![(l[k], h[k])] };
        for &(x0, x1) in &halves(0) {
            for &(y0, y1) in &halves(1) {
                for &(z0, z1) in &halves(2) {
                    let (cl, ch) = (Vec3::new(x0, y0, z0), Vec3::new(x1, y1, z1));
                    seq += 1;
                    queue.push(Pending { top: top_of(cl, ch), seq, lo: cl, hi: ch });
                }
            }
        }
    }
    let (bh, mut bu, mut bv) = best?;
    let mut bh = s * bh;
    // Compass search over the columns: a neighbour column replaces the
    // best when its crossing is further out; the step halves when none is.
    let mut step = [if flat(u) { 0.0 } else { leaf }, if flat(v) { 0.0 } else { leaf }];
    let inward = (end - start).signum() * leaf * 0.25;
    let refine = |cu: f32, cv: f32, bh: f32| -> Option<f32> {
        // A column whose crossing is further out is solid just inside the
        // best height; anything else is no better.
        let probe = bh + inward;
        inside(at(cu, cv, probe)).then(|| crossing(cu, cv, probe))
    };
    for _ in 0..400 {
        if step[0].max(step[1]) < size * 1e-7 {
            break;
        }
        let mut moved = false;
        for (du, dv) in [(1.0f32, 0.0f32), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let (cu, cv) = ((bu + du * step[0]).clamp(lo[u], hi[u]), (bv + dv * step[1]).clamp(lo[v], hi[v]));
            if (cu, cv) == (bu, bv) {
                continue;
            }
            if let Some(h) = refine(cu, cv, bh)
                && s * h > s * bh
            {
                (bu, bv, bh) = (cu, cv, h);
                moved = true;
                break;
            }
        }
        if !moved {
            step = [step[0] * 0.5, step[1] * 0.5];
        }
    }
    Some(bh)
}

/// The smallest stretch of a placement (a lower bound on how much it
/// scales a distance), for distance bounds measured in an object's space.
fn min_stretch(m: Mat4) -> f32 {
    [m.x_axis, m.y_axis, m.z_axis].iter().map(|c| c.truncate().length()).fold(f32::MAX, f32::min)
}

/// The world box of a box in a space `m` maps into the world.
fn world_box(m: Mat4, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    (0..8)
        .map(|i| m.transform_point3(Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z })))
        .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)))
}

/// A clay at its world placement: its blobs' own shapes and its composed
/// field, measured on demand and remembered.
pub struct ClayModel<'a> {
    clay: &'a Clay,
    world: Mat4,
    inv: Mat4,
    field: Field,
    /// The field's box in the clay's space and the cell of `lists`.
    llo: Vec3,
    cell: f32,
    /// The blobs that matter in each cell of the field's box (`Field::near`):
    /// a sample reads only those.
    lists: RefCell<std::collections::HashMap<[i32; 3], Vec<u32>>>,
    lo: Vec3,
    hi: Vec3,
    /// The clay's cell in the world: the smallest part extents never miss.
    leaf: f32,
    /// How little the placement can shrink a distance (distance bounds
    /// are taken in the clay's space).
    stretch: f32,

    cache: RefCell<Extents>,
}

/// Extents measured so far: (blob name, "" for the composed field, or
/// "\0section" and the height's bits for a section; axis).
type Extents = BTreeMap<(String, Axis), Option<(f32, f32)>>;

impl<'a> ClayModel<'a> {
    /// None for an empty clay.
    pub fn new(clay: &'a Clay, world: Mat4) -> Option<ClayModel<'a>> {
        let cell = clay.detail_cell.filter(|c| c.is_finite() && *c > 0.0).or_else(|| clay.cell_size()).unwrap_or(0.01);
        let field = Field::new(clay, cell);
        let (llo, lhi) = field.bounds()?;
        let stretch = min_stretch(world).max(1e-9);
        let leaf = cell * [world.x_axis, world.y_axis, world.z_axis].iter().map(|c| c.truncate().length()).fold(0.0f32, f32::max);
        // A few blobs: one list for the whole box (a cell lookup costs more
        // than it saves).
        let cell = if field.prim_count() <= 24 { f32::INFINITY } else { ((lhi - llo).max_element() / 16.0).max(1e-6) };
        let (lo, hi) = world_box(world, llo, lhi);
        Some(ClayModel { clay, world, inv: world.inverse(), field, llo, cell, lists: RefCell::new(std::collections::HashMap::new()), lo, hi, leaf, stretch, cache: RefCell::new(BTreeMap::new()) })
    }

    /// The shapes `name` stands for: blobs of that name (both sides of a
    /// mirrored one, unless the name has a side suffix such as `.L`: then
    /// the side as written, and the mirrored copy answers to the other
    /// side's name when no blob has it). Empty when none.
    fn shapes(&self, name: &str) -> Vec<BlobShape> {
        let blobs = &self.clay.blobs;
        let literal = blobs.iter().take(MAX_BLOBS).any(|b| b.name == name);
        let mut out = Vec::new();
        for (i, b) in blobs.iter().enumerate().take(MAX_BLOBS) {
            if b.name.is_empty() {
                continue;
            }
            let side = crate::rig::mirror_name(&b.name);
            let Some((own, copy)) = self.clay.blob_shapes(i) else { continue };
            if b.name == name {
                out.push(own);
                if side.is_none() {
                    out.extend(copy);
                }
            } else if !literal && side.as_deref() == Some(name) {
                out.extend(copy);
            }
        }
        out
    }

    /// The world extent along `axis` of the blobs called `name`; None when
    /// the clay has none (an error when they hold no solid).
    pub fn blob_extent(&self, name: &str, axis: Axis) -> Option<Result<(f32, f32), String>> {
        let shapes = self.shapes(name);
        if shapes.is_empty() {
            return None;
        }
        let key = (name.to_string(), axis);
        if let Some(e) = self.cache.borrow().get(&key) {
            return Some(e.ok_or_else(|| format!("blob {name} holds no solid")));
        }
        let mut acc: Option<(f32, f32)> = None;
        for shape in &shapes {
            let (lo, hi) = world_box(self.world, shape.lo, shape.hi);
            let inv = self.inv;
            let inside = |p: Vec3| shape.distance(inv.transform_point3(p)) <= 0.0;
            let distance = |l: Vec3, h: Vec3| shape.distance(inv.transform_point3((l + h) * 0.5)) * self.stretch;
            if let Some((a, b)) = extent(&Solid { inside: &inside, distance: &distance }, lo, hi, axis, self.leaf) {
                acc = Some(acc.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
        }
        self.cache.borrow_mut().insert(key, acc);
        Some(acc.ok_or_else(|| format!("blob {name} holds no solid")))
    }

    fn solid(&self, p: Vec3) -> bool {
        let q = self.inv.transform_point3(p);
        let k = if self.cell.is_finite() { ((q - self.llo) / self.cell).floor().as_ivec3().to_array() } else { [0; 3] };
        let mut lists = self.lists.borrow_mut();
        let list = lists.entry(k).or_insert_with(|| {
            if !self.cell.is_finite() {
                return (0..self.field.prim_count() as u32).collect();
            }
            let a = self.llo + glam::IVec3::from_array(k).as_vec3() * self.cell;
            // A hair over the cell, so points on its faces are inside.
            self.field.near(a - self.cell * 0.01, a + self.cell * 1.01)
        });
        self.field.sample(q, list).0 <= 0.0
    }

    /// A lower bound (within `LIPSCHITZ`) on the world distance from the
    /// centre of the world box `lo..hi` to the solid, reading the blobs
    /// that can reach the box (all of them: a centre's own cell list would
    /// miss blobs elsewhere in a big box).
    fn distance(&self, lo: Vec3, hi: Vec3) -> f32 {
        let (clo, chi) = world_box(self.inv, lo, hi);
        let list = self.field.near(clo, chi);
        let q = self.inv.transform_point3((lo + hi) * 0.5);
        // The field is clamped a few cells from the surface; past that the
        // blobs' boxes bound the distance.
        let d = self.field.sample(q, &list).0.max(self.field.solid_bound(q, &list, clo, chi));
        d * self.stretch
    }

    fn measure(&self, lo: Vec3, hi: Vec3, axis: Axis) -> Option<(f32, f32)> {
        let inside = |p: Vec3| self.solid(p);
        let distance = |l: Vec3, h: Vec3| self.distance(l, h);
        extent(&Solid { inside: &inside, distance: &distance }, lo, hi, axis, self.leaf)
    }

    /// The composed surface's world extent along `axis`.
    pub fn total(&self, axis: Axis) -> Result<(f32, f32), String> {
        let key = (String::new(), axis);
        if let Some(e) = self.cache.borrow().get(&key) {
            return e.ok_or_else(|| "the clay holds no solid".to_string());
        }
        let e = self.measure(self.lo, self.hi, axis);
        self.cache.borrow_mut().insert(key, e);
        e.ok_or_else(|| "the clay holds no solid".to_string())
    }

    /// The composed solid's extent along `axis` across its section at
    /// world height `y` (None where the section is empty).
    pub fn section(&self, y: f32, axis: Axis) -> Option<(f32, f32)> {
        if axis == Axis::Y || y < self.lo.y || y > self.hi.y {
            return None;
        }
        // Kept: front and back (left and right) read the same sections.
        let key = (format!("\u{0}section {:08x}", y.to_bits()), axis);
        if let Some(e) = self.cache.borrow().get(&key) {
            return *e;
        }
        let (lo, hi) = (Vec3::new(self.lo.x, y, self.lo.z), Vec3::new(self.hi.x, y, self.hi.z));
        let e = self.measure(lo, hi, axis);
        self.cache.borrow_mut().insert(key, e);
        e
    }

    /// The clay's blob names, each once, in blob order (a mirrored blob
    /// with a side suffix also gives its other side's name); at most
    /// `MAX_LISTED`, and how many more there were.
    pub fn named(&self) -> (Vec<String>, usize) {
        let mut out: Vec<String> = Vec::new();
        for b in self.clay.blobs.iter().take(MAX_BLOBS).filter(|b| !b.name.is_empty()) {
            let side = crate::rig::mirror_name(&b.name).filter(|_| b.mirror);
            for n in std::iter::once(b.name.clone()).chain(side) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        let more = out.len().saturating_sub(MAX_LISTED);
        out.truncate(MAX_LISTED);
        (out, more)
    }
}

/// What a measurement reads from the model.
pub trait Model {
    /// The world extent along `axis` of the parts called `name`: the
    /// target's blobs of that name, else an object of that name.
    fn extent(&self, name: &str, axis: Axis) -> Result<(f32, f32), String>;
    /// The target's own extent along `axis`.
    fn total(&self, axis: Axis) -> Result<(f32, f32), String>;
    /// The target's blob names (`ClayModel::named`).
    fn named(&self) -> (Vec<String>, usize);
    /// The target's extent along `axis` across its section at height `y`.
    fn section(&self, y: f32, axis: Axis) -> Option<(f32, f32)>;
}

impl Model for ClayModel<'_> {
    fn extent(&self, name: &str, axis: Axis) -> Result<(f32, f32), String> {
        self.blob_extent(name, axis).unwrap_or_else(|| Err(format!("no blob or object called {name}")))
    }

    fn total(&self, axis: Axis) -> Result<(f32, f32), String> {
        ClayModel::total(self, axis)
    }

    fn named(&self) -> (Vec<String>, usize) {
        ClayModel::named(self)
    }

    fn section(&self, y: f32, axis: Axis) -> Option<(f32, f32)> {
        ClayModel::section(self, y, axis)
    }
}

/// A turnaround sheet's view as `measure` reads it: its subject's top and
/// bottom in world heights (by the sheet's scale and ground line), the
/// axis across it, and its subject's width at a world height.
pub struct SheetView<'a> {
    pub view: String,
    pub across: Axis,
    pub top: f32,
    pub bottom: f32,
    pub width_at: &'a dyn Fn(f32) -> Option<f32>,
}

/// A measurement: the reply, and a line per failed target.
pub struct Measured {
    pub reply: Value,
    pub failures: Vec<String>,
}

/// Rounded for replies (1e-5).
pub fn round(x: f32) -> f64 {
    // (+ 0.0: no "-0.0" in replies.)
    (x as f64 * 1e5).round() / 1e5 + 0.0
}

fn round3(p: Vec3) -> [f64; 3] {
    p.to_array().map(round)
}

/// Where `measure` gets lengths.
struct Reader<'a> {
    guides: &'a Guides,
    model: &'a dyn Model,
    sheets: &'a [SheetView<'a>],
}

impl Reader<'_> {
    /// The sheet's subject height: the front view's, else the first view's.
    fn sheet_height(&self) -> Result<f32, String> {
        let v = self.sheets.iter().find(|v| v.view == "front").or(self.sheets.first()).ok_or("sheet_height needs a turnaround sheet (set_reference sheet)")?;
        Ok(v.top - v.bottom)
    }

    /// A length: a special name's, else the part's extent along `axis`.
    fn length(&self, name: &str, axis: Axis) -> Result<f32, String> {
        let span = |e: (f32, f32)| e.1 - e.0;
        match name {
            TOTAL_HEIGHT => self.model.total(Axis::Y).map(span),
            TOTAL_WIDTH => self.model.total(Axis::X).map(span),
            TOTAL_DEPTH => self.model.total(Axis::Z).map(span),
            HEAD_UNIT => self.guides.head_units.as_ref().map(HeadUnits::unit).ok_or_else(|| "head_unit needs head_units (guides set head_units)".to_string()),
            SHEET_HEIGHT => self.sheet_height(),
            _ => self.model.extent(name, axis).map(span),
        }
    }

    /// A part's centre: the middle of its extents.
    fn center(&self, name: &str) -> Result<Vec3, String> {
        let mut c = Vec3::ZERO;
        for axis in Axis::ALL {
            let (a, b) = self.model.extent(name, axis)?;
            c[axis.index()] = 0.5 * (a + b);
        }
        Ok(c)
    }

    /// A target's measured value and the numbers it came from.
    fn target(&self, t: &Target) -> Result<(f32, Value), String> {
        if !t.of.is_empty() {
            let axis = t.axis.unwrap_or(Axis::Y);
            let mut acc: Option<(f32, f32)> = None;
            for n in &t.of {
                let (a, b) = match n.as_str() {
                    TOTAL_HEIGHT | TOTAL_WIDTH | TOTAL_DEPTH | HEAD_UNIT | SHEET_HEIGHT => return Err(format!("of names parts; {n} goes in to")),
                    _ => self.model.extent(n, axis)?,
                };
                acc = Some(acc.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
            let (a, b) = acc.ok_or("of is empty")?;
            let to = t.to.as_deref().unwrap_or(TOTAL_HEIGHT);
            let whole = self.length(to, axis)?;
            if whole <= 1e-9 {
                return Err(format!("{to} has no length"));
            }
            let of = b - a;
            return Ok((of / whole, json!({ "kind": "ratio", "axis": axis.name(), "of_length": round(of), "of_from": round(a), "of_to": round(b), "to": to, "to_length": round(whole) })));
        }
        // A distance.
        let end = |e: &End| -> Result<Result<Vec3, (Axis, f32)>, String> {
            match e {
                End::At(p) => Ok(Ok(Vec3::from(*p))),
                End::Name(n) => match self.guides.line(n) {
                    Some(l) => Ok(Err(l.at())),
                    None => self.center(n).map(Ok),
                },
            }
        };
        let (a, b) = (end(&t.between[0])?, end(&t.between[1])?);
        let along = |line_axis: Axis| match t.axis {
            Some(x) if x != line_axis => Err(format!("axis {} differs from the line's axis {}", x.name(), line_axis.name())),
            _ => Ok(line_axis),
        };
        let show = |e: &Result<Vec3, (Axis, f32)>| match e {
            Ok(p) => json!(round3(*p)),
            Err((ax, v)) => json!({ ax.name(): round(*v) }),
        };
        let (d, axis) = match (&a, &b) {
            (Ok(p), Ok(q)) => match t.axis {
                Some(x) => (q[x.index()] - p[x.index()], Some(x)),
                None => (p.distance(*q), None),
            },
            (Err((ax, v)), Ok(q)) => {
                let ax = along(*ax)?;
                (q[ax.index()] - v, Some(ax))
            }
            (Ok(p), Err((ax, v))) => {
                let ax = along(*ax)?;
                (v - p[ax.index()], Some(ax))
            }
            (Err((ax, v)), Err((bx, w))) => {
                if ax != bx {
                    return Err("two lines across different axes have no distance".into());
                }
                let ax = along(*ax)?;
                (w - v, Some(ax))
            }
        };
        let mut info = json!({ "kind": "distance", "from": show(&a), "to": show(&b), "distance": round(d), "axis": axis.map(Axis::name) });
        let measured = match &t.per {
            None => d,
            Some(per) => {
                // A part's height unless per_axis says otherwise (the
                // distance's own axis does not choose it).
                let len = self.length(per, t.per_axis.unwrap_or(Axis::Y))?;
                if len <= 1e-9 {
                    return Err(format!("{per} has no length"));
                }
                info["per"] = json!(per);
                info["per_length"] = json!(round(len));
                if per_is_part(per) {
                    info["per_axis"] = json!(t.per_axis.unwrap_or(Axis::Y).name());
                }
                d / len
            }
        };
        Ok((measured, info))
    }
}

/// Measures every target, the head units and lines against the model,
/// and the model against the sheet's views. Pure and in a fixed order.
pub fn measure(guides: &Guides, model: &dyn Model, sheets: &[SheetView]) -> Measured {
    let r = Reader { guides, model, sheets };
    let mut failures = Vec::new();
    let total = |axis: Axis| model.total(axis).ok();
    let (height, width, depth) = (total(Axis::Y), total(Axis::X), total(Axis::Z));
    let span = |e: Option<(f32, f32)>| e.map(|(a, b)| round(b - a));
    let mut reply = json!({
        "model": {
            "bottom": height.map(|e| round(e.0)),
            "top": height.map(|e| round(e.1)),
            "height": span(height),
            "width": span(width),
            "depth": span(depth),
        },
    });
    let units = guides.head_units;
    if let Some(h) = units {
        let unit = h.unit();
        let (names, more) = model.named();
        let blobs: Vec<Value> = names
            .iter()
            .filter_map(|n| {
                let (bottom, top) = model.extent(n, Axis::Y).ok()?;
                Some(json!({ "name": n, "top": round(top), "bottom": round(bottom), "top_heads": round(h.heads_down(top)), "bottom_heads": round(h.heads_down(bottom)), "heads_tall": round((top - bottom) / unit) }))
            })
            .collect();
        reply["head_units"] = json!({
            "count": h.count,
            "bottom": round(h.bottom),
            "top": round(h.top),
            "unit": round(unit),
            "lines": h.lines().into_iter().map(round).collect::<Vec<_>>(),
            "model_heads": height.map(|(a, b)| round((b - a) / unit)),
            "model_top_heads": height.map(|e| round(h.heads_down(e.1))),
            "model_bottom_heads": height.map(|e| round(h.heads_down(e.0))),
            "blobs": blobs,
            "blobs_not_listed": more,
            "note": "heads are counted down from the top line",
        });
    }
    if !guides.lines.is_empty() {
        reply["lines"] = json!(guides.lines.iter().map(|l| {
            let (axis, at) = l.at();
            let mut v = json!({ "name": l.name, axis.name(): round(at) });
            if let (Axis::Y, Some(h)) = (axis, units) {
                v["heads"] = json!(round(h.heads_down(at)));
            }
            v
        }).collect::<Vec<_>>());
    }
    let mut targets = Vec::new();
    for t in &guides.targets {
        match r.target(t) {
            Ok((m, info)) => {
                let error = m - t.value;
                // A hair of slack for the float rounding of the lengths.
                let pass = error.abs() <= t.tolerance + 1e-6 * t.value.abs().max(1.0);
                if !pass {
                    failures.push(format!("proportion {}: {:.4} is not within {} of {}", t.name, m, t.tolerance, t.value));
                }
                let mut v = json!({ "name": t.name, "measured": round(m), "value": short::f(t.value), "tolerance": short::f(t.tolerance), "error": round(error), "pass": pass });
                if let (Some(a), Some(b)) = (v.as_object_mut(), info.as_object()) {
                    a.extend(b.clone());
                }
                targets.push(v);
            }
            Err(e) => {
                failures.push(format!("proportion {}: {e}", t.name));
                targets.push(json!({ "name": t.name, "value": short::f(t.value), "tolerance": short::f(t.tolerance), "pass": false, "problem": e }));
            }
        }
    }
    if !sheets.is_empty() {
        let unit = units.map(|h| h.unit());
        reply["sheet"] = json!(sheets.iter().map(|v| {
            let sh = v.top - v.bottom;
            let lines: Vec<Value> = guides.lines.iter().filter_map(|l| {
                let (Axis::Y, y) = l.at() else { return None };
                let sheet = (v.width_at)(y);
                let model = model.section(y, v.across).map(|(a, b)| b - a);
                Some(json!({ "name": l.name, "y": round(y), "sheet_width": sheet.map(round), "model_width": model.map(round), "difference": sheet.zip(model).map(|(s, m)| round(m - s)) }))
            }).collect();
            json!({
                "view": v.view,
                "across": v.across.name(),
                "top": round(v.top),
                "bottom": round(v.bottom),
                "height": round(sh),
                "heads": unit.map(|u| round(sh / u)),
                "model_height": span(height),
                "model_over_sheet": height.map(|(a, b)| round((b - a) / sh.max(1e-9))),
                "top_difference": height.map(|e| round(e.1 - v.top)),
                "lines": lines,
            })
        }).collect::<Vec<_>>());
    }
    reply["targets"] = json!(targets);
    reply["pass"] = json!(failures.is_empty());
    reply["failures"] = json!(failures);
    Measured { reply, failures }
}

/// What a drawn guide line is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A head unit line between the top and the bottom.
    Unit,
    /// The top or bottom head unit line.
    Edge,
    /// A named line.
    Named,
}

/// One guide line as drawn: a segment through the framed middle, with a
/// label.
#[derive(Clone, PartialEq, Debug)]
pub struct Segment {
    pub a: Vec3,
    pub b: Vec3,
    pub kind: Kind,
    pub label: String,
}

/// The guide lines as seen looking along `forward`, through `center` and
/// `radius` wide each way: each plane's trace on the plane through
/// `center` facing the view. A plane seen nearly face-on (a y line from
/// the top view) has no trace and is left out.
pub fn segments(g: &Guides, center: Vec3, radius: f32, forward: Vec3) -> Vec<Segment> {
    let forward = forward.normalize_or_zero();
    let trace = |axis: Axis, v: f32| -> Option<(Vec3, Vec3)> {
        let n = axis.unit();
        if forward == Vec3::ZERO || n.dot(forward).abs() > 0.7 {
            return None;
        }
        let d = n.cross(forward).normalize_or_zero();
        let q = center + n * (v - center.dot(n));
        Some((q - d * radius * 1.15, q + d * radius * 1.15))
    };
    let mut out = Vec::new();
    if let Some(h) = &g.head_units {
        let lines = h.lines();
        let last = lines.len() - 1;
        for (k, y) in lines.into_iter().enumerate() {
            if let Some((a, b)) = trace(Axis::Y, y) {
                let kind = if k == 0 || k == last { Kind::Edge } else { Kind::Unit };
                out.push(Segment { a, b, kind, label: format!("{}", h.count as usize - k) });
            }
        }
    }
    for l in &g.lines {
        let (axis, v) = l.at();
        if let Some((a, b)) = trace(axis, v) {
            out.push(Segment { a, b, kind: Kind::Named, label: l.name.clone() });
        }
    }
    out
}

#[cfg(test)]
mod tests;
