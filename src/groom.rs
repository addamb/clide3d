//! Groom brushes (Quality Stage 4c): comb, lengthen, shorten, cut, clump,
//! twist, smooth, puff and relax, acting on a Hair object's guides in
//! place (decision D20: no ledger; whole-scene undo and `history` cover
//! them, and each call is deterministic).
//!
//! Everything here is in the hair object's own space: the `groom` handler
//! (`app/groom.rs`) moves the strokes into it, so the X mirror is about
//! the hair's own x = 0, as `Hair::mirror` is.
//!
//! Each guide point gets a weight from the falloff of its distance to the
//! brush path, times an `affect` ramp along the guide (tips: 0 at the root
//! to 1 at the tip; roots: the reverse; all: 1), times the strength.
//! With `mirror`, the stroke reflected across x = 0 is a second brush;
//! a point takes the one that weighs it more (the first on a tie), so a
//! stroke on one side and its reflection on the other give mirrored
//! guides. Roots never move.
//!
//! Every guide's new points are a pure function of the guides as the
//! brush found them (Jacobi): brushes that read neighbours (clump, twist,
//! relax) read the snapshot, so the result does not depend on the thread
//! count (`clay::par_chunks`). After a brush, each changed guide gets its
//! segment lengths back by follow-the-leader from the root and every point
//! is pushed out of the collider (`Collider::push`), a few rounds, ending
//! with the push: no point is left inside, and lengths stay within the
//! slack the push needs.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::brush::curve::{Curve, Falloff};
use crate::hair::Hair;

/// Strokes per call.
pub const MAX_STROKES: usize = 64;
/// Points in one stroke's path.
pub const MAX_PATH: usize = 256;
/// Passes of smooth and relax.
pub const MAX_ITERATIONS: u32 = 32;
/// Largest twist, degrees.
pub const MAX_ANGLE: f32 = 720.0;
/// Fractions along the guides at which clump centres are tabulated.
const TABLE: usize = 32;
/// Rounds of follow-the-leader and collision.
const ROUNDS: usize = 3;
/// Neighbours relax evens the spacing to.
const NEIGHBOURS: usize = 6;
/// Shorten keeps at least this part of a guide.
const MIN_KEEP: f32 = 0.01;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Comb,
    Lengthen,
    Shorten,
    Cut,
    Clump,
    Twist,
    Smooth,
    Puff,
    Relax,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Affect {
    Tips,
    Roots,
    All,
}

impl Kind {
    /// Where along the guides the brush acts unless the stroke says.
    pub fn default_affect(self) -> Affect {
        match self {
            Kind::Puff => Affect::Roots,
            Kind::Comb | Kind::Clump | Kind::Twist => Affect::Tips,
            Kind::Lengthen | Kind::Shorten | Kind::Cut | Kind::Smooth | Kind::Relax => Affect::All,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Comb => "comb",
            Kind::Lengthen => "lengthen",
            Kind::Shorten => "shorten",
            Kind::Cut => "cut",
            Kind::Clump => "clump",
            Kind::Twist => "twist",
            Kind::Smooth => "smooth",
            Kind::Puff => "puff",
            Kind::Relax => "relax",
        }
    }
}

/// One groom stroke, in the hair's space.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub kind: Kind,
    /// The brush path (or one point).
    pub points: Vec<Vec3>,
    pub radius: f32,
    pub strength: f32,
    pub falloff: Falloff,
    /// Comb: the way to comb (default: the path's direction where it is
    /// nearest each point).
    pub direction: Option<Vec3>,
    /// Lengthen / shorten: by this much; cut: keep at most this long.
    pub length: Option<f32>,
    /// Cut: remove what lies on the side the normal points to (point, normal).
    pub plane: Option<(Vec3, Vec3)>,
    /// Twist, radians.
    pub angle: f32,
    pub affect: Affect,
    pub mirror: bool,
    /// Smooth and relax passes.
    pub iterations: u32,
}

impl Stroke {
    /// A stroke with the defaults for `kind`.
    pub fn new(kind: Kind, points: Vec<Vec3>, radius: f32) -> Stroke {
        Stroke { kind, points, radius, strength: 1.0, falloff: Falloff::default(), direction: None, length: None, plane: None, angle: 0.0, affect: kind.default_affect(), mirror: false, iterations: 4 }
    }

    /// Refuses what the brush cannot use, before any work.
    pub fn validate(&self) -> Result<(), String> {
        let k = self.kind.name();
        if self.points.is_empty() || self.points.len() > MAX_PATH {
            return Err(format!("{k}: a stroke takes 1 to {MAX_PATH} points ({} sent)", self.points.len()));
        }
        if self.points.iter().any(|p| !p.is_finite()) {
            return Err(format!("{k}: points must be finite"));
        }
        if !self.radius.is_finite() || self.radius <= 0.0 {
            return Err(format!("{k}: radius must be a positive number"));
        }
        if !self.strength.is_finite() || !(0.0..=1.0).contains(&self.strength) {
            return Err(format!("{k}: strength must be 0..1"));
        }
        self.falloff.validate()?;
        if !(1..=MAX_ITERATIONS).contains(&self.iterations) {
            return Err(format!("{k}: iterations must be 1..{MAX_ITERATIONS}"));
        }
        if let Some(d) = self.direction
            && (!d.is_finite() || d.length_squared() < 1e-12)
        {
            return Err(format!("{k}: direction must be a finite, non-zero vector"));
        }
        if let Some(l) = self.length
            && (!l.is_finite() || l <= 0.0)
        {
            return Err(format!("{k}: length must be a positive number"));
        }
        if let Some((p, n)) = self.plane
            && (!p.is_finite() || !n.is_finite() || n.length_squared() < 1e-12)
        {
            return Err(format!("{k}: plane needs a finite point and a non-zero normal"));
        }
        if !self.angle.is_finite() || self.angle.abs() > MAX_ANGLE.to_radians() {
            return Err(format!("{k}: angle must be within ±{MAX_ANGLE} degrees"));
        }
        match self.kind {
            Kind::Comb if self.direction.is_none() && !self.points.windows(2).any(|w| w[0].distance_squared(w[1]) > 0.0) => {
                Err("comb needs a direction or a path of 2 or more distinct points (it combs along the path)".into())
            }
            Kind::Lengthen | Kind::Shorten if self.length.is_none() => Err(format!("{k} needs length (how much)")),
            Kind::Cut if self.length.is_some() == self.plane.is_some() => Err("cut needs length (keep at most this long) or plane, not both".into()),
            _ => Ok(()),
        }
    }
}

/// Something the hair stays out of (the head), in the hair's space.
pub trait Collider: Sync {
    /// `p` moved out to at least the hair's lift outside the surface;
    /// None when it already is.
    fn push(&self, p: Vec3) -> Option<Vec3>;
    /// The outward surface normal near `p` (None: not near the surface).
    fn normal(&self, p: Vec3) -> Option<Vec3>;
    /// Whether `p` is inside the surface.
    fn inside(&self, p: Vec3) -> bool;
    /// The spacing segments are checked at (the surface's finest detail,
    /// e.g. the head's cell), in the hair's units.
    fn step(&self) -> f32;
    /// `p` moved onto the surface along its gradient, and the outward
    /// normal there (fur roots); None where the surface cannot be found.
    fn snap(&self, _p: Vec3) -> Option<(Vec3, Vec3)> {
        None
    }
    /// The surface's colour (sRGB 0..1) near `p`, when it has one.
    fn color(&self, _p: Vec3) -> Option<Vec3> {
        None
    }
    /// Whether the surface near `p` belongs to one of the named parts
    /// (the blob that makes the surface there, as masks see it).
    fn owned_by(&self, _p: Vec3, _names: &[String]) -> bool {
        false
    }
}

/// Samples inside one segment checked against the collider, at most.
pub const SEGMENT_SAMPLES: usize = 16;
/// Times one segment is turned around the surface, at most.
const SEGMENT_TRIES: usize = 3;

/// Keeps the segment from `a` (already clear: the root side) to `b` out
/// of the collider: it is sampled at the collider's step (2 to
/// `SEGMENT_SAMPLES` pieces, an even count), and while a sample from `a`'s side is
/// inside, the segment turns about `a` to run through that sample pushed
/// out (as far again as its push), with any part into the surface normal
/// there removed (head-on, a
/// fixed tangent), keeping its length, and `b` is pushed out. Turning
/// from the root's side bends the segment around a thin part (an ear)
/// where pushing the sample alone could take it through to the far
/// side. Returns whether `b` moved.
pub fn clear_segment(a: Vec3, b: &mut Vec3, collider: &dyn Collider) -> bool {
    let len = a.distance(*b);
    if !(len > 0.0 && len.is_finite()) {
        return false;
    }
    // An even count, so the middle (where a chord sags most) is a sample.
    let pieces = ((len / collider.step().max(1e-9)).ceil() as usize).clamp(2, SEGMENT_SAMPLES).next_multiple_of(2);
    let mut moved = false;
    for _ in 0..SEGMENT_TRIES {
        let Some(s) = (1..pieces).map(|i| a.lerp(*b, i as f32 / pieces as f32)).find(|s| collider.inside(*s)) else { break };
        let d = (*b - a) / len;
        let q = collider.push(s).unwrap_or(s);
        let n = collider.normal(s).or_else(|| (q - s).try_normalize()).unwrap_or(-d);
        // Through the sample pushed out as far again as it was in (a
        // chord between two points on the surface still sags into it),
        // with no part into the surface.
        let through = (q + (q - s) - a).normalize_or(d);
        let along = through - n * through.dot(n).min(0.0);
        let d = along.try_normalize().unwrap_or_else(|| n.any_orthonormal_vector());
        *b = a + d * len;
        if let Some(q) = collider.push(*b) {
            *b = q;
        }
        moved = true;
    }
    moved
}

/// A film strand kept out of the collider (`Hair::strands_kept_out`):
/// from the root, each point pushed out, then the segment to it cleared
/// from the side before it (`clear_segment`); once something has moved,
/// each later point first follows the one before it at its old distance
/// (follow-the-leader, as `settle` does), so the tail keeps its shape and
/// segments never stretch. Points before the first move are left exactly
/// as they are, so a strand the collider does not reach is unchanged bit
/// for bit. Returns whether anything moved.
pub fn keep_out(points: &mut [Vec3], collider: &dyn Collider) -> bool {
    let lengths: Vec<f32> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    let mut moved = false;
    let mut prev = Vec3::NEG_Y;
    for k in 0..points.len() {
        if k > 0 {
            let d = (points[k] - points[k - 1]).normalize_or(prev);
            if moved {
                points[k] = points[k - 1] + d * lengths[k - 1];
            }
            prev = d;
        }
        if let Some(q) = collider.push(points[k]) {
            points[k] = q;
            moved = true;
        }
        if k > 0 {
            let (before, after) = points.split_at_mut(k);
            moved |= clear_segment(before[k - 1], &mut after[0], collider);
        }
    }
    moved
}

/// What a groom did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub guides_changed: usize,
    /// Over the points of changed guides that exist before and after.
    pub mean_move: f32,
    pub max_move: f32,
    /// Mean guide length of the changed guides, before and after.
    pub mean_length_before: f32,
    pub mean_length_after: f32,
    pub points_before: usize,
    pub points_after: usize,
    /// Points the colliders pushed out in the last round, over all strokes.
    pub collisions_resolved: usize,
    /// Points still inside a collider after the call (changed guides).
    pub still_inside: usize,
    /// Guides a plane cut left alone because their root is on the cut side.
    pub roots_beyond_plane: usize,
    /// Lengthen strokes that hit the per-guide point bound and stretched
    /// the last segment instead.
    pub points_capped: usize,
}

/// Runs `strokes` in order on `hair.guides`, each with its collider.
pub fn apply(hair: &mut Hair, strokes: &[(Stroke, Option<&dyn Collider>)]) -> Result<Report, String> {
    if strokes.len() > MAX_STROKES {
        return Err(format!("at most {MAX_STROKES} strokes per call ({} sent)", strokes.len()));
    }
    for (s, _) in strokes {
        s.validate()?;
    }
    check_size(hair)?;
    let w = work(hair, strokes);
    if w > MAX_WORK {
        return Err(format!("this groom would take about {:.2e} units of work ({} guide points x path points, more with a head and for smooth and relax passes); at most {MAX_WORK:.0e} run in one call: send fewer strokes, shorter paths or fewer iterations, or split the call", w as f64, hair.guides.iter().map(|g| g.points.len()).sum::<usize>()));
    }
    let before = hair.guides.clone();
    let mut report = Report::default();
    // The stroke that last changed each guide: its collider is the one
    // the guide was kept out of.
    let mut last: Vec<Option<usize>> = vec![None; hair.guides.len()];
    for (i, (s, c)) in strokes.iter().enumerate() {
        for g in stroke(hair, s, *c, &mut report) {
            last[g] = Some(i);
        }
    }
    let mut lengths = (0.0f64, 0.0f64);
    let (mut moves, mut sum) = (0usize, 0.0f64);
    for (g, (a, b)) in before.iter().zip(hair.guides.iter()).enumerate() {
        if a.points == b.points {
            continue;
        }
        report.guides_changed += 1;
        lengths.0 += length(&a.points) as f64;
        lengths.1 += length(&b.points) as f64;
        report.points_before += a.points.len();
        report.points_after += b.points.len();
        for (p, q) in a.points.iter().zip(&b.points) {
            let d = p.distance(*q);
            sum += d as f64;
            moves += 1;
            report.max_move = report.max_move.max(d);
        }
        if let Some(c) = last[g].and_then(|i| strokes[i].1) {
            report.still_inside += b.points.iter().skip(1).filter(|p| c.inside(**p)).count();
        }
    }
    if report.guides_changed > 0 {
        report.mean_length_before = (lengths.0 / report.guides_changed as f64) as f32;
        report.mean_length_after = (lengths.1 / report.guides_changed as f64) as f32;
        report.mean_move = (sum / moves.max(1) as f64) as f32;
    }
    Ok(report)
}

/// Most work one `apply` takes on (`work`'s units: about a nanosecond
/// each, so a call stays within a few seconds on the editor's thread).
pub const MAX_WORK: u64 = 4_000_000_000;
/// What one collider call costs, in `work` units, against a point test.
const COLLIDER_COST: u64 = 8;
/// Collider calls one point can take in one settle round, at most: its
/// push, then `clear_segment`'s tries, each scanning the segment's inner
/// samples (`SEGMENT_SAMPLES - 1`) and then a push, a normal and a push.
const COLLIDER_CALLS: u64 = 1 + SEGMENT_TRIES as u64 * (SEGMENT_SAMPLES as u64 - 1 + 3);

/// An upper estimate of what `strokes` cost on `hair`, from the sizes
/// alone (nothing runs): every guide point against every path segment
/// (twice with mirror), the collision rounds (a push and the segment
/// samples, each point, each round), smooth's and relax's passes, and
/// relax's neighbour search. A lengthen can take guides up to
/// `MAX_POINTS`, so its own collision rounds and every stroke after it
/// count them full.
pub fn work(hair: &Hair, strokes: &[(Stroke, Option<&dyn Collider>)]) -> u64 {
    let guides = hair.guides.len() as u64;
    let mut points: u64 = hair.guides.iter().map(|g| g.points.len() as u64).sum();
    let mut total = 0u64;
    for (s, c) in strokes {
        let instances = 1 + u64::from(s.mirror);
        let path = (s.points.len() as u64).max(1);
        let mut w = points * path * instances;
        // A lengthen's own settle runs over the guides it made longer.
        if s.kind == Kind::Lengthen {
            points = guides * crate::hair::MAX_POINTS as u64;
        }
        if c.is_some() {
            w += points * ROUNDS as u64 * COLLIDER_CALLS * COLLIDER_COST;
        } else {
            w += points;
        }
        match s.kind {
            Kind::Smooth => w += points * s.iterations as u64,
            Kind::Relax => w += guides * guides + points * s.iterations as u64 * NEIGHBOURS as u64 * 2,
            Kind::Clump | Kind::Twist => w += points * 2 * TABLE as u64,
            _ => {}
        }
        total = total.saturating_add(w);
    }
    total
}

/// The hair's guides and points are within what a groom works on.
pub fn check_size(hair: &Hair) -> Result<(), String> {
    if hair.guides.len() > crate::hair::MAX_GUIDES {
        return Err(format!("the hair has {} guides; a groom works on at most {}", hair.guides.len(), crate::hair::MAX_GUIDES));
    }
    if let Some(g) = hair.guides.iter().position(|g| g.points.len() > crate::hair::MAX_POINTS) {
        return Err(format!("guide {g} has more than {} points", crate::hair::MAX_POINTS));
    }
    Ok(())
}

fn length(points: &[Vec3]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

fn reflect(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

/// The stroke or its mirror image.
struct Inst {
    path: Vec<Vec3>,
    lo: Vec3,
    hi: Vec3,
    direction: Option<Vec3>,
    plane: Option<(Vec3, Vec3)>,
    angle: f32,
}

impl Inst {
    fn new(s: &Stroke, mirrored: bool) -> Inst {
        let m = |v: Vec3| if mirrored { reflect(v) } else { v };
        let path: Vec<Vec3> = s.points.iter().map(|p| m(*p)).collect();
        let (lo, hi) = path.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        Inst {
            lo: lo - Vec3::splat(s.radius),
            hi: hi + Vec3::splat(s.radius),
            direction: s.direction.map(|d| m(d).normalize()),
            plane: s.plane.map(|(p, n)| (m(p), m(n).normalize())),
            // A reflection turns the other way.
            angle: if mirrored { -s.angle } else { s.angle },
            path,
        }
    }

    /// Distance from `p` to the path and the path's direction there
    /// (zero for a single point).
    fn nearest(&self, p: Vec3) -> (f32, Vec3) {
        if self.path.len() == 1 {
            return (p.distance(self.path[0]), Vec3::ZERO);
        }
        let mut best = (f32::MAX, Vec3::ZERO);
        for w in self.path.windows(2) {
            let ab = w[1] - w[0];
            let l2 = ab.length_squared();
            if l2 <= 0.0 {
                let d = p.distance(w[0]);
                if d < best.0 {
                    best.0 = d;
                }
                continue;
            }
            let t = ((p - w[0]).dot(ab) / l2).clamp(0.0, 1.0);
            let d = p.distance(w[0] + ab * t);
            if d < best.0 {
                best = (d, ab / l2.sqrt());
            }
        }
        best
    }
}

/// A point's weight from each instance (falloff only) and the path's
/// direction there.
#[derive(Clone, Copy, Default)]
struct Touch {
    w: [f32; 2],
    tan: [Vec3; 2],
}

impl Touch {
    /// The instance that weighs this point more (the stroke on a tie).
    fn dominant(&self) -> usize {
        usize::from(self.w[1] > self.w[0])
    }

    fn weight(&self) -> f32 {
        self.w[self.dominant()]
    }
}

/// Cumulative arc lengths (first 0).
fn arcs(points: &[Vec3]) -> Vec<f32> {
    let mut out = Vec::with_capacity(points.len());
    let mut s = 0.0;
    out.push(0.0);
    for w in points.windows(2) {
        s += w[0].distance(w[1]);
        out.push(s);
    }
    out
}

/// The point `f` (0..1) of the way along a polyline by length.
fn at_fraction(points: &[Vec3], cum: &[f32], f: f32) -> Vec3 {
    let total = *cum.last().unwrap_or(&0.0);
    if points.len() < 2 || total <= 0.0 {
        return points[0];
    }
    let want = f.clamp(0.0, 1.0) * total;
    let k = cum.partition_point(|&c| c <= want).clamp(1, points.len() - 1);
    let l = cum[k] - cum[k - 1];
    let t = if l > 0.0 { ((want - cum[k - 1]) / l).clamp(0.0, 1.0) } else { 0.0 };
    points[k - 1].lerp(points[k], t)
}

/// The polyline cut to `target` long (at least 2 points).
fn trim(points: &[Vec3], target: f32) -> Vec<Vec3> {
    let cum = arcs(points);
    let mut out = vec![points[0]];
    for k in 1..points.len() {
        if cum[k] < target {
            out.push(points[k]);
            continue;
        }
        let l = cum[k] - cum[k - 1];
        let t = if l > 0.0 { ((target - cum[k - 1]) / l).clamp(0.0, 1.0) } else { 0.0 };
        out.push(points[k - 1].lerp(points[k], t));
        break;
    }
    if out.len() < 2 {
        out.push(points[1]);
    }
    out
}

fn ramp(affect: Affect, f: f32) -> f32 {
    match affect {
        Affect::Tips => f,
        Affect::Roots => 1.0 - f,
        Affect::All => 1.0,
    }
}

/// `d` turned toward `to` by `t` of the angle between them (unit vectors).
fn turn(d: Vec3, to: Vec3, t: f32) -> Vec3 {
    if t <= 0.0 {
        return d;
    }
    let q = Quat::from_rotation_arc(d, to);
    (Quat::IDENTITY.slerp(q, t.min(1.0)) * d).normalize_or(d)
}

/// Per-guide work, chunked over threads by `par_chunks`.
fn per_guide<T: Send + Default + Clone>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let mut out = vec![T::default(); n];
    const CHUNK: usize = 32;
    crate::clay::par_chunks(&mut out, CHUNK, |c, slot| {
        for (j, o) in slot.iter_mut().enumerate() {
            *o = f(c * CHUNK + j);
        }
    });
    out
}

/// A clump centre line: the guides in an instance's brush, each weighted
/// by its largest falloff, averaged at `TABLE + 1` fractions.
struct Centre {
    points: Vec<Vec3>,
}

impl Centre {
    fn at(&self, f: f32) -> Vec3 {
        let x = f.clamp(0.0, 1.0) * TABLE as f32;
        let k = (x.floor() as usize).min(TABLE - 1);
        self.points[k].lerp(self.points[k + 1], x - k as f32)
    }

    /// The line's direction at `f`.
    fn axis(&self, f: f32) -> Vec3 {
        let x = f.clamp(0.0, 1.0) * TABLE as f32;
        let k = (x.round() as usize).min(TABLE);
        let (a, b) = (k.saturating_sub(1), (k + 1).min(TABLE));
        (self.points[b] - self.points[a]).normalize_or_zero()
    }
}

fn centres(old: &[Vec<Vec3>], touch: &[Option<Vec<Touch>>]) -> [Option<Centre>; 2] {
    std::array::from_fn(|i| {
        let mut sum = vec![Vec3::ZERO; TABLE + 1];
        let mut total = 0.0f32;
        for (g, t) in touch.iter().enumerate() {
            let Some(t) = t else { continue };
            let w = t.iter().map(|t| t.w[i]).fold(0.0f32, f32::max);
            if w <= 0.0 {
                continue;
            }
            let cum = arcs(&old[g]);
            for (j, s) in sum.iter_mut().enumerate() {
                *s += at_fraction(&old[g], &cum, j as f32 / TABLE as f32) * w;
            }
            total += w;
        }
        (total > 0.0).then(|| Centre { points: sum.into_iter().map(|s| s / total).collect() })
    })
}

/// One stroke on every guide it reaches; returns the guides it changed.
fn stroke(hair: &mut Hair, s: &Stroke, collider: Option<&dyn Collider>, report: &mut Report) -> Vec<usize> {
    let insts: Vec<Inst> = std::iter::once(Inst::new(s, false)).chain(s.mirror.then(|| Inst::new(s, true))).collect();
    let curve: Curve = s.falloff.resolve();
    let old: Vec<Vec<Vec3>> = hair.guides.iter().map(|g| g.points.clone()).collect();
    let n = old.len();
    // Which points each instance reaches.
    let touch: Vec<Option<Vec<Touch>>> = per_guide(n, |g| {
        let pts = &old[g];
        if pts.len() < 2 {
            return None;
        }
        let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let near: Vec<usize> = insts.iter().enumerate().filter(|(_, i)| lo.cmple(i.hi).all() && hi.cmpge(i.lo).all()).map(|(k, _)| k).collect();
        if near.is_empty() {
            return None;
        }
        let mut any = false;
        let t: Vec<Touch> = pts.iter().map(|p| {
            let mut t = Touch::default();
            for &k in &near {
                let (d, tan) = insts[k].nearest(*p);
                if d < s.radius {
                    t.w[k] = curve.eval(d / s.radius).max(0.0);
                    t.tan[k] = tan;
                    any |= t.w[k] > 0.0;
                }
            }
            t
        }).collect();
        any.then_some(t)
    });
    let centre = if matches!(s.kind, Kind::Clump | Kind::Twist) { centres(&old, &touch) } else { [None, None] };
    let roots_beyond = std::sync::atomic::AtomicUsize::new(0);
    let capped = std::sync::atomic::AtomicUsize::new(0);
    // The brush: new points and the segment lengths to keep.
    let mut out: Vec<Option<(Vec<Vec3>, Vec<f32>)>> = per_guide(n, |g| {
        let t = touch[g].as_ref()?;
        let pts = &old[g];
        let cum = arcs(pts);
        let total = *cum.last().expect("2+ points");
        let lengths: Vec<f32> = pts.windows(2).map(|w| w[0].distance(w[1])).collect();
        let frac = |k: usize| if total > 0.0 { cum[k] / total } else { 0.0 };
        let weight = |k: usize| t[k].weight() * ramp(s.affect, frac(k)) * s.strength;
        let guide_weight = |i: usize| t.iter().map(|t| t.w[i]).fold(0.0f32, f32::max);
        let whole = guide_weight(0).max(guide_weight(1));
        match s.kind {
            Kind::Comb | Kind::Puff => {
                let normal = (s.kind == Kind::Puff).then(|| {
                    collider.and_then(|c| c.normal(pts[0])).unwrap_or_else(|| (pts[0] - centre_of(hair)).normalize_or(Vec3::Y))
                });
                let mut p = vec![pts[0]];
                let mut prev = Vec3::NEG_Y;
                for k in 0..pts.len() - 1 {
                    let d = (pts[k + 1] - pts[k]).normalize_or(prev);
                    let to = match normal {
                        Some(nm) => nm,
                        None => {
                            let i = t[k + 1].dominant();
                            insts[i].direction.unwrap_or(t[k + 1].tan[i])
                        }
                    };
                    let nd = if to.length_squared() > 0.0 { turn(d, to.normalize(), weight(k + 1)) } else { d };
                    p.push(p[k] + nd * lengths[k]);
                    prev = d;
                }
                Some((p, lengths))
            }
            Kind::Lengthen => {
                let delta = s.length.unwrap_or(0.0) * whole * s.strength;
                if delta <= 0.0 {
                    return None;
                }
                let m = pts.len();
                let dir = (pts[m - 1] - pts[m - 2]).normalize_or((pts[m - 1] - pts[0]).normalize_or(Vec3::NEG_Y));
                let step = (total / (m - 1) as f32).max(1e-6);
                let room = crate::hair::MAX_POINTS.saturating_sub(m);
                let need = ((delta / step).ceil() as usize).max(1);
                let count = need.min(room.max(1));
                let mut p = pts.clone();
                let mut l = lengths.clone();
                // Fewer points than the guide's spacing asks for: the
                // last segments stretch.
                if need > room {
                    capped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if room == 0 {
                    p[m - 1] += dir * delta;
                    l[m - 2] += delta;
                } else {
                    let seg = delta / count as f32;
                    for _ in 0..count {
                        let q = *p.last().expect("points") + dir * seg;
                        p.push(q);
                        l.push(seg);
                    }
                }
                Some((p, l))
            }
            Kind::Shorten => {
                let delta = s.length.unwrap_or(0.0) * whole * s.strength;
                if delta <= 0.0 {
                    return None;
                }
                let p = trim(pts, (total - delta).max(total * MIN_KEEP));
                let l = p.windows(2).map(|w| w[0].distance(w[1])).collect();
                Some((p, l))
            }
            Kind::Cut => {
                if whole <= 0.0 {
                    return None;
                }
                let p = match s.plane {
                    None => {
                        let keep = s.length.unwrap_or(total);
                        if keep >= total {
                            return None;
                        }
                        trim(pts, keep)
                    }
                    Some(_) => {
                        // The plane of the instance that reaches the guide more.
                        let i = usize::from(guide_weight(1) > guide_weight(0));
                        let (c, nm) = insts[i].plane.expect("plane");
                        let side = |p: Vec3| (p - c).dot(nm);
                        if side(pts[0]) >= 0.0 {
                            roots_beyond.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            return None;
                        }
                        let k = (1..pts.len()).find(|&k| side(pts[k]) > 0.0)?;
                        let (a, b) = (side(pts[k - 1]), side(pts[k]));
                        let mut p = pts[..k].to_vec();
                        p.push(pts[k - 1].lerp(pts[k], a / (a - b)));
                        p
                    }
                };
                let l = p.windows(2).map(|w| w[0].distance(w[1])).collect();
                Some((p, l))
            }
            Kind::Clump | Kind::Twist => {
                let mut p = pts.clone();
                for k in 1..pts.len() {
                    let w = weight(k);
                    if w <= 0.0 {
                        continue;
                    }
                    let i = t[k].dominant();
                    let Some(c) = &centre[i] else { continue };
                    let f = frac(k);
                    p[k] = if s.kind == Kind::Clump {
                        pts[k].lerp(c.at(f), w.min(1.0))
                    } else {
                        let axis = c.axis(f);
                        if axis == Vec3::ZERO {
                            continue;
                        }
                        let o = c.at(f);
                        o + Quat::from_axis_angle(axis, insts[i].angle * w) * (pts[k] - o)
                    };
                }
                Some((p, lengths))
            }
            Kind::Smooth => {
                let mut p = pts.clone();
                let m = p.len();
                for _ in 0..s.iterations {
                    let q = p.clone();
                    for k in 1..m {
                        let target = if k + 1 < m { (q[k - 1] + q[k + 1]) * 0.5 } else if m >= 3 { q[k - 1] * 2.0 - q[k - 2] } else { continue };
                        p[k] = q[k] + (target - q[k]) * 0.5 * weight(k).min(1.0);
                    }
                }
                Some((p, lengths))
            }
            // Cross-guide passes run below.
            Kind::Relax => Some((pts.clone(), lengths)),
        }
    });
    if s.kind == Kind::Relax {
        relax(&old, &touch, s, &mut out);
    }
    report.roots_beyond_plane += roots_beyond.into_inner();
    report.points_capped += capped.into_inner();
    // Lengths back and out of the head.
    let settled: Vec<Option<(Vec<Vec3>, usize)>> = per_guide(n, |g| {
        let (mut p, l) = out[g].clone()?;
        if p == old[g] {
            return None;
        }
        let pushed = settle(&mut p, &l, collider);
        // A damaged guide (points near f32's end) can overflow: it is
        // left as it was.
        if !p.iter().all(|x| x.is_finite()) {
            return None;
        }
        Some((p, pushed))
    });
    out.clear();
    let mut changed = Vec::new();
    for (g, s) in settled.into_iter().enumerate() {
        if let Some((p, pushed)) = s {
            report.collisions_resolved += pushed;
            hair.guides[g].points = p;
            changed.push(g);
        }
    }
    changed
}

/// The middle the hair grows from (puff's way out without a collider).
fn centre_of(hair: &Hair) -> Vec3 {
    hair.center.unwrap_or_else(|| {
        let roots: Vec<Vec3> = hair.guides.iter().filter_map(|g| g.points.first().copied()).collect();
        roots.iter().copied().sum::<Vec3>() / roots.len().max(1) as f32 - Vec3::Y * 0.05
    })
}

/// Relax: each reached guide's points move to even out their distances to
/// its nearest neighbours' points at the same fraction, toward the
/// spacing of their roots; every pass reads the one before.
fn relax(old: &[Vec<Vec3>], touch: &[Option<Vec<Touch>>], s: &Stroke, out: &mut [Option<(Vec<Vec3>, Vec<f32>)>]) {
    let reached: Vec<usize> = (0..old.len()).filter(|&g| touch[g].is_some()).collect();
    if reached.len() < 2 {
        return;
    }
    let neighbours: Vec<Vec<(usize, f32)>> = per_guide(reached.len(), |a| {
        let ga = reached[a];
        let mut near: Vec<(f32, usize)> = reached.iter().enumerate().filter(|(b, _)| *b != a).map(|(b, &gb)| (old[ga][0].distance(old[gb][0]), b)).collect();
        near.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        near.truncate(NEIGHBOURS);
        near.into_iter().map(|(d, b)| (b, d)).collect()
    });
    let mut cur: Vec<Vec<Vec3>> = reached.iter().map(|&g| old[g].clone()).collect();
    for _ in 0..s.iterations {
        let cums: Vec<Vec<f32>> = cur.iter().map(|p| arcs(p)).collect();
        let next: Vec<Vec<Vec3>> = per_guide(reached.len(), |a| {
            let g = reached[a];
            let t = touch[g].as_ref().expect("reached");
            let p = &cur[a];
            let cum = &cums[a];
            let total = *cum.last().expect("points");
            let mut q = p.clone();
            for k in 1..p.len() {
                let f = if total > 0.0 { cum[k] / total } else { 0.0 };
                let w = t.get(k).map_or(0.0, |t| t.weight()) * ramp(s.affect, f) * s.strength;
                if w <= 0.0 || neighbours[a].is_empty() {
                    continue;
                }
                let mut push = Vec3::ZERO;
                for &(b, rest) in &neighbours[a] {
                    let o = at_fraction(&cur[b], &cums[b], f);
                    let d = p[k] - o;
                    let l = d.length();
                    if l > 1e-9 {
                        push += d / l * (rest - l);
                    }
                }
                q[k] = p[k] + push * (0.5 * w.min(1.0) / neighbours[a].len() as f32);
            }
            q
        });
        cur = next;
    }
    for (a, &g) in reached.iter().enumerate() {
        if let Some((p, _)) = out[g].as_mut() {
            *p = cur[a].clone();
        }
    }
}

/// Follow-the-leader from the root to `lengths`, each point pushed out of
/// the collider right after it is placed and the segment to it cleared
/// (`clear_segment`); a few rounds, so the last thing done to every point
/// is its push. Returns the points pushed in the last round.
fn settle(p: &mut [Vec3], lengths: &[f32], collider: Option<&dyn Collider>) -> usize {
    let rounds = if collider.is_some() { ROUNDS } else { 1 };
    let mut pushed = 0;
    for _ in 0..rounds {
        pushed = 0;
        let mut prev = Vec3::NEG_Y;
        for k in 1..p.len() {
            let d = (p[k] - p[k - 1]).normalize_or(prev);
            p[k] = p[k - 1] + d * lengths[k - 1];
            prev = d;
            if let Some(c) = collider {
                let mut hit = false;
                if let Some(q) = c.push(p[k]) {
                    p[k] = q;
                    hit = true;
                }
                // The segment too: a thin part can sit between two
                // points that are both outside.
                let (before, after) = p.split_at_mut(k);
                hit |= clear_segment(before[k - 1], &mut after[0], c);
                pushed += usize::from(hit);
            }
        }
    }
    pushed
}

pub mod head;

#[cfg(test)]
mod tests;
