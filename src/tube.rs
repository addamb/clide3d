//! Exact tube fields for sweeps (`field: segments`).
//!
//! The legacy sweep field (`hard_surface.rs`) solves a foot point on the
//! path by Newton steps and evaluates the section in an interpolated
//! frame. Where that foot point jumps, nearby points get unrelated
//! frames, so the field is not 1-Lipschitz and thin tubes on curved paths
//! mesh fuzzy. Here the path is cut once into short straight spans (by
//! sagitta), and the field is the minimum over the spans of one piece
//! each: an exact round cone for a circle profile, or a rigid prism cut by
//! mitre planes for a polygon. A minimum of 1-Lipschitz functions is
//! 1-Lipschitz, and of exact distances is exact outside the union. A tree
//! of span boxes (`CopyTree`, as path arrays use) visits the near spans
//! only and honours the caller's bound, so far carves cost a few box tests.

use std::borrow::Cow;

use glam::{Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use super::hard_surface::{CopyTree, Corner, Walk, check_profile, polygon, polygon_sdf};

/// Most path samples (span ends) in one tube; the sagitta tolerance is
/// loosened until a sweep fits, counted before anything is allocated.
pub const MAX_SAMPLES: usize = 4096;
/// Circle radii (`profile.circle`, `radii`).
pub const MIN_RADIUS: f32 = 0.0005;
pub const MAX_RADIUS: f32 = 10.0;
/// A legacy circle's polygon, unless `sides` says otherwise.
const SIDES: u32 = 16;
/// Twist per span at most (polygon profiles), radians (4 degrees).
const TWIST_STEP: f32 = 0.07;
/// Chord sagitta at most this share of the thinnest radius.
const SAGITTA: f32 = 1.0 / 128.0;

/// How a sweep's field is made.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SweepField {
    /// The field from before tubes: what saved sweeps without `field` use.
    #[default]
    Legacy,
    /// A union of exact pieces along the densified path (this module).
    Segments,
}

impl SweepField {
    pub fn is_legacy(&self) -> bool {
        *self == SweepField::Legacy
    }
}

/// A sweep's cross-section. JSON: the polygon as a plain list of corners
/// (as before tubes), or `{circle: r, sides}`.
#[derive(Clone, PartialEq, Debug)]
pub enum SweepProfile {
    Polygon(Vec<Corner>),
    /// `sides` only shapes a legacy field (a regular polygon on the circle).
    Circle { r: f32, sides: u32 },
}

impl SweepProfile {
    /// The polygon a legacy field sweeps: a circle as its regular polygon.
    pub fn corners(&self) -> Cow<'_, [Corner]> {
        match self {
            SweepProfile::Polygon(p) => Cow::Borrowed(p),
            SweepProfile::Circle { r, sides } => Cow::Owned((0..*sides).map(|i| {
                let a = std::f32::consts::TAU * i as f32 / *sides as f32;
                Corner::Point(Vec2::new(a.cos(), a.sin()) * *r)
            }).collect()),
        }
    }

    pub fn circle(&self) -> Option<f32> {
        match self {
            SweepProfile::Circle { r, .. } => Some(*r),
            SweepProfile::Polygon(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Circle {
    circle: f32,
    #[serde(default = "sides")]
    sides: u32,
}

fn sides() -> u32 {
    SIDES
}

impl Serialize for SweepProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SweepProfile::Polygon(p) => p.serialize(s),
            SweepProfile::Circle { r, sides } => Circle { circle: *r, sides: *sides }.serialize(s),
        }
    }
}

/// `Form::Sweep.profile`: the checks `hard_surface::profile` makes on a
/// polygon, and a circle radius of 0.0005-10 with 3-128 sides.
pub fn profile<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SweepProfile, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Polygon(Vec<Corner>),
        Circle(Circle),
    }
    use serde::de::Error;
    match Raw::deserialize(d).map_err(|_| D::Error::custom("profile is a polygon [[u,v], ...] or {circle: radius, sides}"))? {
        Raw::Polygon(p) => check_profile(p).map(SweepProfile::Polygon).map_err(D::Error::custom),
        Raw::Circle(Circle { circle, sides }) if (MIN_RADIUS..=MAX_RADIUS).contains(&circle) && (3..=128).contains(&sides) => Ok(SweepProfile::Circle { r: circle, sides }),
        Raw::Circle(_) => Err(D::Error::custom(format!("profile circle needs a radius of {MIN_RADIUS}-{MAX_RADIUS} and 3-128 sides"))),
    }
}

/// What `Blob::validate` adds for a sweep: `radii` go with a circle and
/// the segments field (a legacy field has no use for them), one per path
/// point, each 0.0005-10.
pub(super) fn check(profile: &SweepProfile, points: &[Vec3], radii: Option<&[f32]>, field: SweepField) -> Result<(), String> {
    let Some(radii) = radii else { return Ok(()) };
    if profile.circle().is_none() || field.is_legacy() {
        return Err("sweep radii need a circle profile ({circle: r}) and field segments".into());
    }
    if radii.len() != points.len() || radii.iter().any(|r| !(MIN_RADIUS..=MAX_RADIUS).contains(r)) {
        return Err(format!("sweep radii need one radius of {MIN_RADIUS}-{MAX_RADIUS} per path point ({} points, {} radii)", points.len(), radii.len()));
    }
    Ok(())
}

/// Gives sweeps in agent JSON without a `field` the new one (D5: saved
/// files keep theirs, absent meaning legacy); nested `source`s too.
pub(crate) fn fill_field(blob: &mut serde_json::Value) {
    let Some(o) = blob.as_object_mut() else { return };
    if o.get("type").and_then(|t| t.as_str()) == Some("sweep") && !o.contains_key("field") {
        o.insert("field".into(), serde_json::json!("segments"));
    }
    if let Some(source) = o.get_mut("source") {
        fill_field(source);
    }
}

/// Writes a legacy sweep's `field` out in agent JSON (`get_clay`, an
/// `update` patch base): saved files leave it out, but JSON an agent sends
/// back (`edit_clay replace`) must keep it, or `fill_field` would take
/// the sweep for a new one and change its mesh (D5); nested `source`s too.
pub(crate) fn show_field(blob: &mut serde_json::Value) {
    let Some(o) = blob.as_object_mut() else { return };
    if o.get("type").and_then(|t| t.as_str()) == Some("sweep") && !o.contains_key("field") {
        o.insert("field".into(), serde_json::json!("legacy"));
    }
    if let Some(source) = o.get_mut("source") {
        show_field(source);
    }
}

/// The Hermite span `i` of a smooth path: ends and tangents as
/// `hard_surface::spline_path` makes them (distance-weighted Catmull-Rom).
fn hermite(points: &[Vec3], i: usize) -> [Vec3; 4] {
    let (a, b) = (points[i], points[i + 1]);
    let prior = points[i.saturating_sub(1)];
    let next = points[(i + 2).min(points.len() - 1)];
    let len = a.distance(b);
    let u = (b - prior) * (len / (a.distance(prior) + len).max(1e-9));
    let v = (next - a) * (len / (b.distance(next) + len).max(1e-9));
    [a, b, u, v]
}

fn at([a, b, u, v]: [Vec3; 4], t: f32) -> Vec3 {
    a * (2.0 * t * t * t - 3.0 * t * t + 1.0) + b * (-2.0 * t * t * t + 3.0 * t * t) + u * (t * t * t - 2.0 * t * t + t) + v * (t * t * t - t * t)
}

/// Pieces span `i` is cut into: enough that each chord's sagitta is at
/// most `tol` (a cubic's second derivative is linear, so its largest is at
/// an end, and a chord over dt deviates at most max|c''| dt^2 / 8), and
/// that each turns at most `twist` radians of the section.
fn pieces(points: &[Vec3], smooth: bool, i: usize, tol: f32, twist: f32) -> usize {
    let curved = if smooth {
        let [a, b, u, v] = hermite(points, i);
        let second = (b - a) * 6.0 - u * 4.0 - v * 2.0;
        let end = (a - b) * 6.0 + u * 2.0 + v * 4.0;
        (second.length().max(end.length()) / (8.0 * tol)).sqrt()
    } else {
        0.0
    };
    // As floats first: huge counts saturate instead of overflowing.
    curved.max(twist).ceil().clamp(1.0, MAX_SAMPLES as f32) as usize
}

/// The sampled path: span ends and their arc length.
fn densify(points: &[Vec3], smooth: bool, tol: f32, twist: f32) -> (Vec<Vec3>, Vec<f32>, Vec<usize>) {
    let length: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>().max(1e-9);
    let mut tol = tol;
    let mut step = TWIST_STEP;
    // Count first, loosening until it fits: nothing is allocated for a
    // sweep too curved for its radius.
    let counts = loop {
        let counts: Vec<usize> = (0..points.len() - 1).map(|i| pieces(points, smooth, i, tol, twist.abs() * points[i].distance(points[i + 1]) / length / step)).collect();
        if counts.iter().sum::<usize>() < MAX_SAMPLES || tol > 1e30 {
            break counts;
        }
        tol *= 2.0;
        step *= 2.0;
    };
    let mut samples = Vec::with_capacity(counts.iter().sum::<usize>() + 1);
    // The sample each control point became.
    let mut controls = Vec::with_capacity(points.len());
    // A cusp can repeat a point, and a span needs a direction: a repeat
    // stands for the sample before it.
    let add = |samples: &mut Vec<Vec3>, p: Vec3| {
        if samples.last().is_none_or(|q: &Vec3| q.distance_squared(p) > 1e-14) {
            samples.push(p);
        }
        samples.len() - 1
    };
    for (i, &n) in counts.iter().enumerate() {
        let span = hermite(points, i);
        for k in 0..n {
            let t = k as f32 / n as f32;
            let at = add(&mut samples, if smooth { at(span, t) } else { points[i].lerp(points[i + 1], t) });
            if k == 0 {
                controls.push(at);
            }
        }
    }
    let end = *points.last().expect("2+ points");
    if samples.len() > 1 && samples.last().is_some_and(|q| q.distance_squared(end) <= 1e-14) {
        samples.pop();
    }
    // At least one span, even for a path a validated blob cannot have.
    controls.push(if samples.len() == 1 { samples.push(end); 1 } else { add(&mut samples, end) });
    let mut s = vec![0.0f32];
    for w in samples.windows(2) {
        s.push(s.last().expect("one") + w[0].distance(w[1]));
    }
    (samples, s, controls)
}

/// A sweep with `field: segments`: spans between consecutive samples.
pub(super) struct Tube {
    points: Vec<Vec3>,
    /// Circle: the radius at each sample. Polygon: the section's scale.
    radii: Vec<f32>,
    /// None for a circle.
    prism: Option<Prism>,
    tree: CopyTree,
    lo: Vec3,
    hi: Vec3,
}

/// A polygon tube's spans as rigid prisms on rotation-minimising frames.
struct Prism {
    section: Vec<Vec2>,
    /// Per span: world to the span's frame (section in XY, the span along +Z).
    frames: Vec<Quat>,
    /// Per span: the start and end planes' normals (along the span). At a
    /// joint both spans share the bisector plane, so their sections meet
    /// on it exactly; past 120 degrees each ends square.
    planes: Vec<(Vec3, Vec3)>,
    /// Section twist at each sample, radians.
    twist: Vec<f32>,
    /// Per span: every point of the prism is within this of the span.
    reach: Vec<f32>,
}

impl Tube {
    pub(super) fn new(profile: &SweepProfile, points: &[Vec3], taper: f32, twist: f32, smooth: bool, radii: Option<&[f32]>) -> Tube {
        let taper = taper.clamp(0.01, 10.0);
        let twist = if profile.circle().is_some() { 0.0 } else { twist.to_radians() };
        let section = match profile {
            SweepProfile::Polygon(p) => Some(polygon(p)),
            SweepProfile::Circle { .. } => None,
        };
        // The thinnest radius sets the sagitta.
        let thinnest = match (profile, radii) {
            (SweepProfile::Circle { .. }, Some(r)) => r.iter().copied().fold(f32::MAX, f32::min),
            (SweepProfile::Circle { r, .. }, None) => r * taper.min(1.0),
            (SweepProfile::Polygon(_), _) => {
                let p = section.as_deref().expect("polygon");
                let (lo, hi) = p.iter().fold((Vec2::MAX, Vec2::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
                (hi - lo).min_element() * 0.5 * taper.min(1.0)
            }
        };
        let (samples, s, controls) = densify(points, smooth, (thinnest * SAGITTA).max(1e-7), twist);
        let length = s.last().copied().unwrap_or(0.0).max(1e-9);
        let scale = |k: usize| 1.0 + (taper - 1.0) * s[k] / length;
        let sized: Vec<f32> = match (profile, radii) {
            // Along arc length between control points.
            (SweepProfile::Circle { .. }, Some(r)) => {
                let mut out = Vec::with_capacity(samples.len());
                for (i, w) in controls.windows(2).enumerate() {
                    let (s0, s1) = (s[w[0]], s[w[1]]);
                    out.extend((w[0]..w[1]).map(|k| r[i] + (r[i + 1] - r[i]) * ((s[k] - s0) / (s1 - s0).max(1e-12))));
                }
                out.push(*r.last().expect("one per point"));
                out
            }
            (SweepProfile::Circle { r, .. }, None) => (0..samples.len()).map(|k| r * scale(k)).collect(),
            (SweepProfile::Polygon(_), _) => (0..samples.len()).map(scale).collect(),
        };
        let spans = samples.len() - 1;
        let prism = section.map(|section| {
            let tangents: Vec<Vec3> = samples.windows(2).map(|w| (w[1] - w[0]).normalize_or(Vec3::Z)).collect();
            // Parallel transport: each span's frame is the last one turned
            // by the least rotation between their directions.
            let mut frames = Vec::with_capacity(spans);
            let mut frame = Quat::from_rotation_arc(Vec3::Z, tangents[0]);
            frames.push(frame.inverse());
            for w in tangents.windows(2) {
                frame = (Quat::from_rotation_arc(w[0], w[1]) * frame).normalize();
                frames.push(frame.inverse());
            }
            let mut planes: Vec<(Vec3, Vec3)> = tangents.iter().map(|t| (*t, *t)).collect();
            for i in 1..spans {
                let (a, b) = (tangents[i - 1], tangents[i]);
                if a.dot(b) >= -0.5 {
                    let mitre = (a + b).normalize();
                    planes[i - 1].1 = mitre;
                    planes[i].0 = mitre;
                }
            }
            let widest = section.iter().map(|v| v.length()).fold(0.0, f32::max);
            let reach = (0..spans).map(|i| widest * sized[i].max(sized[i + 1]) / planes[i].0.dot(tangents[i]).min(planes[i].1.dot(tangents[i])).max(0.5)).collect();
            Prism { section, frames, planes, twist: s.iter().map(|s| -twist * s / length).collect(), reach }
        });
        let reach = |i: usize| prism.as_ref().map_or(sized[i].max(sized[i + 1]), |p| p.reach[i]);
        let boxes: Vec<(Vec3, Vec3)> = (0..spans).map(|i| {
            let (a, b) = (samples[i], samples[i + 1]);
            (a.min(b) - reach(i), a.max(b) + reach(i))
        }).collect();
        let (lo, hi) = boxes.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), b| (lo.min(b.0), hi.max(b.1)));
        Tube { tree: CopyTree::new(&boxes), points: samples, radii: sized, prism, lo, hi }
    }

    pub(super) fn bounds(&self) -> (Vec3, Vec3) {
        (self.lo, self.hi)
    }

    /// Spans (for tests and timings).
    #[cfg(test)]
    pub(super) fn spans(&self) -> usize {
        self.points.len() - 1
    }

    /// Bytes held, tree included.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        let prism = self.prism.as_ref().map_or(0, |p| p.section.len() * 8 + p.frames.len() * 16 + p.planes.len() * 24 + p.twist.len() * 4 + p.reach.len() * 4);
        self.points.len() * 12 + self.radii.len() * 4 + prism + self.tree.bytes()
    }

    /// The tube's distance where it is below `bound`; elsewhere a value
    /// from `bound` up to it (see `Geometry::sdf_within`). Every span's
    /// value is at least the distance to its box, so the spans the walk
    /// skips could not have lowered the minimum.
    pub(super) fn sdf_within(&self, q: Vec3, bound: f32) -> f32 {
        let size = q.abs().max(self.lo.abs()).max(self.hi.abs()).max_element();
        let mut walk = Walk { q, bound, best: f32::MAX, floor: f32::MAX, slack: size * 1e-6 };
        self.tree.walk(&mut walk, |walk, i| {
            let d = self.span(q, i as usize);
            walk.best = walk.best.min(d);
        });
        walk.best.min(walk.floor)
    }

    /// Whether some span's box comes within `reach` of the box lo..hi:
    /// if not, the tube is more than `reach` from every point in it.
    pub(super) fn near_box(&self, lo: Vec3, hi: Vec3, reach: f32) -> bool {
        self.tree.near_box(lo, hi, reach)
    }

    fn span(&self, q: Vec3, i: usize) -> f32 {
        let (a, b) = (self.points[i], self.points[i + 1]);
        let Some(p) = &self.prism else {
            return super::round_cone(q, a, b, self.radii[i], self.radii[i + 1]);
        };
        let (start, end) = p.planes[i];
        let (into, left) = ((q - a).dot(start), (b - q).dot(end));
        // Taper and twist by where the point lies between the two planes,
        // so neighbouring spans agree on their shared plane.
        let w = if into + left > 1e-12 { (into / (into + left)).clamp(0.0, 1.0) } else { 0.5 };
        let scale = self.radii[i] + (self.radii[i + 1] - self.radii[i]) * w;
        let (sin, cos) = (p.twist[i] + (p.twist[i + 1] - p.twist[i]) * w).sin_cos();
        let local = p.frames[i] * (q - a);
        let uv = Vec2::new(local.x * cos - local.y * sin, local.x * sin + local.y * cos);
        let prism = (polygon_sdf(uv / scale, &p.section) * scale).max(-into).max(-left);
        // At least the distance to a capsule holding the prism (so to its
        // box, which the walk relies on): still a bound of the distance.
        let ab = b - a;
        let t = ((q - a).dot(ab) / ab.length_squared().max(1e-20)).clamp(0.0, 1.0);
        prism.max(q.distance(a + ab * t) - p.reach[i])
    }
}

#[cfg(test)]
mod tests {
    use super::super::hard_surface::Geometry;
    use super::super::{Clay, Field};
    use super::*;
    use crate::mesh::Mesh;
    use serde_json::{Value, json};

    /// FNV-1a over positions, normals, colors and indices (as `adaptive`'s tests).
    pub(crate) fn hash(m: &Mesh) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |x: u32| {
            h ^= x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        for v in &m.vertices {
            v.pos.iter().chain(&v.normal).chain(&v.color).for_each(|f| eat(f.to_bits()));
        }
        m.indices.iter().for_each(|&i| eat(i));
        h
    }

    fn square() -> Value {
        json!([[-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1]])
    }

    /// An n-gon of circumradius r (what agents wrote before `circle`).
    fn ngon(r: f32, n: usize) -> Value {
        json!((0..n).map(|i| { let a = std::f32::consts::TAU * i as f32 / n as f32; [r * a.cos(), r * a.sin()] }).collect::<Vec<_>>())
    }

    fn circle(r: f32) -> Value {
        json!({ "circle": r })
    }

    fn path(points: &[Vec3]) -> Value {
        json!(points.iter().map(|p| p.to_array()).collect::<Vec<_>>())
    }

    /// Saved sweeps from before `field`, and their mesh hashes on the code
    /// before tubes (the same at 1 and 12 threads).
    fn legacy_fixtures() -> Vec<(&'static str, Clay, u64)> {
        let clay = |blobs: Value| serde_json::from_value::<Clay>(json!({ "blobs": blobs, "resolution": 64 })).unwrap();
        vec![
            ("pinned_twist", clay(json!([{ "type": "sweep", "profile": square(), "points": [[0, 0, 0], [0, 0, 1], [0.3, 0, 2]], "taper": 0.5, "twist": 90, "blend": 0 }])), 0x3094_aca8_ce6a_53fe),
            ("pinned_taper", clay(json!([{ "type": "sweep", "profile": square(), "points": [[0, 0, -0.5], [0, 0, 0], [0.15, 0, 0.5]], "taper": 0.7, "twist": 30, "blend": 0 }])), 0x2930_1d1c_ecdf_71a8),
            ("polygonal", clay(json!([{ "type": "sweep", "profile": square(), "points": [[0, 0, 0], [0.5, 0, 0], [0.5, 0.5, 0], [0.5, 0.5, 0.6]], "path_smooth": false, "blend": 0 }])), 0xf0cd_3936_048b_dbcd),
            ("carved", clay(json!([
                { "type": "sphere", "center": [0, 0, 0], "radius": 0.5 },
                { "type": "sweep", "op": "carve", "profile": ngon(0.04, 16), "points": [[-0.6, 0.1, 0.45], [0, 0.2, 0.52], [0.6, 0.0, 0.4]], "blend": 0.02 },
                { "type": "sweep", "op": "carve", "join": "chamfer", "profile": ngon(0.03, 8), "points": [[-0.5, -0.2, 0.45], [0.5, -0.25, 0.42]], "blend": 0.02 },
            ])), 0x6259_a1f1_fcc7_3b4b),
        ]
    }

    /// Helix: helix radius 0.05, pitch 0.03, 3 turns, 8 control points a turn.
    fn helix() -> Vec<Vec3> {
        (0..=24).map(|i| { let a = std::f32::consts::TAU * i as f32 / 8.0; Vec3::new(0.05 * a.cos(), 0.03 * a / std::f32::consts::TAU, 0.05 * a.sin()) }).collect()
    }

    fn s_curve(scale: f32) -> Vec<Vec3> {
        [[0.0, 0.0, 0.0], [0.03, 0.02, 0.0], [0.06, -0.02, 0.01], [0.09, 0.02, 0.0], [0.12, 0.0, 0.0]].map(|p| Vec3::from(p) * scale).to_vec()
    }

    /// The smooth sweep's path (`spline_path`'s curve), 512 samples a
    /// span: the dense analytic capsule chain tubes are measured against.
    fn dense(points: &[Vec3]) -> Vec<Vec3> {
        let mut out: Vec<Vec3> = (0..points.len() - 1).flat_map(|i| (0..512).map(move |k| at(hermite(points, i), k as f32 / 512.0))).collect();
        out.push(*points.last().unwrap());
        out
    }

    /// The nearest point of a dense polyline, and its arc length.
    fn nearest(path: &[Vec3], p: Vec3) -> (Vec3, f32) {
        let (mut best, mut s) = ((f32::MAX, p, 0.0), 0.0);
        for w in path.windows(2) {
            let v = w[1] - w[0];
            let t = ((p - w[0]).dot(v) / v.length_squared().max(1e-20)).clamp(0.0, 1.0);
            let c = w[0] + v * t;
            let d = p.distance_squared(c);
            if d < best.0 {
                best = (d, c, s + v.length() * t);
            }
            s += v.length();
        }
        (best.1, best.2)
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
        fn dir(&mut self) -> Vec3 {
            loop {
                let v = Vec3::new(self.next(), self.next(), self.next()) * 2.0 - 1.0;
                if (0.01..=1.0).contains(&v.length_squared()) {
                    return v.normalize();
                }
            }
        }
    }

    /// The worst |f(p) - f(q)| / |p - q| over `pairs` pairs within two
    /// cells of the reference tube, at most a cell apart.
    fn lipschitz(f: &dyn Fn(Vec3) -> f32, path: &[Vec3], r: f32, cell: f32, pairs: usize) -> f32 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut worst, mut n) = (0.0f32, 0);
        while n < pairs {
            let c = path[(rng.next() * (path.len() - 1) as f32) as usize];
            let p = c + rng.dir() * (r + 2.0 * cell) * rng.next().cbrt();
            if (p.distance(nearest(path, p).0) - r).abs() > 2.0 * cell {
                continue;
            }
            let q = p + rng.dir() * cell * rng.next().max(0.01);
            worst = worst.max((f(p) - f(q)).abs() / p.distance(q));
            n += 1;
        }
        worst
    }

    /// (one-sided Hausdorff in cells, RMS normal angle in degrees) of a
    /// tube mesh against the reference tube; `caps: false` leaves out what
    /// lies within 2r of an end (legacy sweeps end flat).
    fn against_reference(m: &Mesh, path: &[Vec3], r: f32, cell: f32, caps: bool) -> (f32, f32) {
        let length: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
        let (mut far, mut sum, mut n) = (0.0f32, 0.0f64, 0usize);
        for v in &m.vertices {
            let p = Vec3::from(v.pos);
            let (c, s) = nearest(path, p);
            if !caps && (s < 2.0 * r || s > length - 2.0 * r) {
                continue;
            }
            let away = p - c;
            far = far.max((away.length() - r).abs());
            let angle = Vec3::from(v.normal).normalize_or_zero().dot(away.normalize_or_zero()).clamp(-1.0, 1.0).acos().to_degrees();
            sum += (angle as f64).powi(2);
            n += 1;
        }
        (far / cell, (sum / n.max(1) as f64).sqrt() as f32)
    }

    fn tube_clay(blob: Value, resolution: u32) -> Clay {
        serde_json::from_value(json!({ "blobs": [blob], "resolution": resolution })).unwrap()
    }

    fn geometry(c: &Clay) -> Geometry {
        Geometry::new(&c.blobs[0].form, &c.assets)
    }

    fn tube(c: &Clay) -> Box<Tube> {
        match geometry(c) {
            Geometry::Tube(t) => t,
            _ => panic!("not a tube"),
        }
    }

    /// A clay at about `cell` (not the default resolution, which hard
    /// clays floor to 192).
    fn at_cell(blob: Value, cell: f32) -> Clay {
        let (lo, hi) = geometry(&tube_clay(blob.clone(), 64)).bounds();
        tube_clay(blob, match ((hi - lo).max_element() / cell).ceil() as u32 { 96 => 97, n => n.clamp(16, 512) })
    }

    /// The thin tubes of the figurine session (defect 1), and a thick one.
    fn thin_tubes() -> Vec<(&'static str, Vec<Vec3>, f32)> {
        vec![("helix", helix(), 0.006), ("s_curve", s_curve(1.0), 0.004), ("wide_s", s_curve(5.0), 0.02)]
    }

    /// Defect 1 regression: the segments field is 1-Lipschitz near thin
    /// curved tubes; the legacy field (a 16-gon) is far from it.
    #[test]
    fn thin_tube_field_is_lipschitz() {
        for (name, points, r) in thin_tubes() {
            let cell = r / 3.0;
            let reference = dense(&points);
            let new = tube_clay(json!({ "type": "sweep", "profile": circle(r), "points": path(&points), "field": "segments", "blend": 0 }), 64);
            let g = geometry(&new);
            let worst = lipschitz(&|p| g.sdf(p), &reference, r, cell, 10_000);
            let old = geometry(&tube_clay(json!({ "type": "sweep", "profile": ngon(r, 16), "points": path(&points), "blend": 0 }), 64));
            let legacy = lipschitz(&|p| old.sdf(p), &reference, r, cell, 10_000);
            println!("{name}: worst ratio {worst:.4} (legacy {legacy:.3})");
            assert!(worst <= 1.01, "{name}: {worst}");
            if name != "wide_s" {
                assert!(legacy > 1.5, "{name}: the legacy field should show the defect ({legacy})");
            }
        }
        // Per-point radii, and a polygon section on the same path (whose
        // section is a bound, not a distance).
        let points = helix();
        let reference = dense(&points);
        let radii: Vec<f32> = (0..points.len()).map(|i| 0.004 + 0.004 * (i as f32 * 0.7).sin().abs()).collect();
        let g = geometry(&tube_clay(json!({ "type": "sweep", "profile": circle(0.006), "radii": radii, "points": path(&points), "field": "segments", "blend": 0 }), 64));
        assert!(lipschitz(&|p| g.sdf(p), &reference, 0.006, 0.002, 4_000) <= 1.01);
        let g = geometry(&tube_clay(json!({ "type": "sweep", "profile": ngon(0.006, 6), "points": path(&points), "field": "segments", "twist": 90, "blend": 0 }), 64));
        let worst = lipschitz(&|p| g.sdf(p), &reference, 0.006, 0.002, 4_000);
        println!("hexagon tube: worst ratio {worst:.4}");
        assert!(worst <= 1.05, "{worst}");
    }

    /// A thin tube meshes as the dense capsule chain it stands for.
    #[test]
    fn thin_tube_mesh_matches_capsule_chain() {
        for (name, points, r) in thin_tubes() {
            let cell = r / 3.0;
            let reference = dense(&points);
            let c = at_cell(json!({ "type": "sweep", "profile": circle(r), "points": path(&points), "field": "segments", "blend": 0 }), cell);
            let m = c.build_mesh();
            let cell = c.cell_size().unwrap();
            let (far, rms) = against_reference(&m, &reference, r, cell, true);
            let old = at_cell(json!({ "type": "sweep", "profile": ngon(r, 16), "points": path(&points), "blend": 0 }), r / 3.0);
            let (old_far, old_rms) = against_reference(&old.build_mesh(), &reference, r, old.cell_size().unwrap(), false);
            println!("{name}: hausdorff {far:.3} cells, normal rms {rms:.2} deg, {} triangles (legacy 16-gon: {old_far:.2} cells, {old_rms:.2} deg)", m.indices.len() / 3);
            assert!(far <= 0.5, "{name}: {far} cells");
            assert!(rms <= 3.0, "{name}: {rms} degrees");
        }
    }

    /// The dense capsule chain of radius `r` along `path` as a field:
    /// exact within `3 r` of the path (segments bucketed by cells that
    /// size), `3 r` beyond.
    fn capsule_chain(path: &[Vec3], r: f32) -> impl Fn(Vec3) -> f32 + Sync {
        let size = 3.0 * r;
        let mut grid: std::collections::HashMap<[i32; 3], Vec<(Vec3, Vec3)>> = std::collections::HashMap::new();
        for w in path.windows(2) {
            let (lo, hi) = (((w[0].min(w[1]) - size) / size).floor().as_ivec3(), ((w[0].max(w[1]) + size) / size).floor().as_ivec3());
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        grid.entry([x, y, z]).or_default().push((w[0], w[1]));
                    }
                }
            }
        }
        move |p: Vec3| {
            let segs = grid.get(&(p / size).floor().as_ivec3().to_array());
            let d = segs.into_iter().flatten().map(|&(a, b)| {
                let ab = b - a;
                let t = ((p - a).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0);
                p.distance(a + ab * t)
            }).fold(f32::MAX, f32::min);
            (d - r).min(size)
        }
    }

    /// Stage 1b's exit, re-run with Stage 2's measure: a segments tube of
    /// the figurine's sizes is within 1.5 times the analytic capsule
    /// chain's lumpiness (both near none), the helix too since the sagitta
    /// is r/128; the legacy 16-gon is recorded.
    /// Measured raw (over every point, intended or not); at the fit's scale
    /// a thin tube is a line, so it is measured along its length.
    #[test]
    fn thin_tube_lumpiness_matches_capsule_chain() {
        for (name, points, r) in thin_tubes() {
            let cell = r / 3.0;
            let reference = dense(&points);
            let c = at_cell(json!({ "type": "sweep", "profile": circle(r), "points": path(&points), "field": "segments", "blend": 0 }), cell);
            let seeds = crate::quality::seeds(&c.build_mesh(), 3000);
            let score = |f: &(dyn Fn(Vec3) -> f32 + Sync)| {
                let pts: Vec<crate::quality::Point> = seeds.iter().filter_map(|&(p, n)| crate::quality::measure(f, p, n, cell)).collect();
                let l = crate::quality::lumpiness(&pts, &[]);
                (l.raw.unwrap(), l.overall.unwrap_or(0.0), pts.len(), l.lines)
            };
            let g = geometry(&c);
            let tube = score(&|p| g.sdf(p));
            let chain = score(&capsule_chain(&reference, r));
            let old = geometry(&tube_clay(json!({ "type": "sweep", "profile": ngon(r, 16), "points": path(&points), "blend": 0 }), 64));
            let legacy = score(&|p| old.sdf(p));
            // How far each regular mesh sits off its field (cells), recorded
            // beside the field measure (which reads the zero set alone).
            let off = |c: &Clay, f: &dyn Fn(Vec3) -> f32| {
                let cell = c.cell_size().unwrap();
                let d: Vec<f32> = crate::quality::seeds(&c.build_mesh(), 3000).iter().map(|&(p, n)| (crate::adaptive::along(f, p, n, 1e-5, 2.0 * cell) - p).dot(n) / cell).collect();
                (d.iter().map(|x| x * x).sum::<f32>() / d.len() as f32).sqrt()
            };
            let legacy_clay = at_cell(json!({ "type": "sweep", "profile": ngon(r, 16), "points": path(&points), "blend": 0 }), r / 3.0);
            println!("{name}: mesh off its field (RMS cells): tube {:.4}, legacy {:.4}", off(&c, &|p| g.sdf(p)), off(&legacy_clay, &|p| old.sdf(p)));
            println!("{name} (r = {r}): lumpiness raw {:.4} / counted {:.4} over {} points ({} lines); capsule chain {:.4} / {:.4}; legacy 16-gon {:.4} / {:.4}", tube.0, tube.1, tube.2, tube.3, chain.0, chain.1, legacy.0, legacy.1);
            assert!(tube.2 >= 2900, "{name}: {} points measured", tube.2);
            // The 1.5x holds on all three since the sagitta is r/128 (D-a);
            // at r/64 the helix's own chords (about 0.05 cells every 3)
            // read 1.9x the chain's floor.
            assert!(tube.0 <= 1.5 * chain.0 && tube.0 <= 0.02, "{name}: tube {} against the chain's {}", tube.0, chain.0);
        }
    }

    /// D6: a round tube keeps the clay's resolution and plain surface nets.
    #[test]
    fn round_sweeps_are_smooth_forms() {
        let points = path(&s_curve(1.0));
        let round: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sweep", "profile": circle(0.004), "points": points, "field": "segments" }] })).unwrap();
        assert_eq!(round.mesh_resolution(), crate::clay::DEFAULT_RESOLUTION);
        assert!(!round.sharp_features());
        // A polygon, or a legacy circle (its polygon), is still hard.
        for blob in [json!({ "type": "sweep", "profile": ngon(0.004, 16), "points": points, "field": "segments" }), json!({ "type": "sweep", "profile": circle(0.004), "points": points })] {
            let hard: Clay = serde_json::from_value(json!({ "blobs": [blob] })).unwrap();
            assert!(hard.mesh_resolution() >= 192 && hard.sharp_features());
        }
        let form = &round.blobs[0].form;
        assert_eq!(super::super::hard_surface::feature_size(form), Some(0.008));
    }

    /// D5: sweeps saved without `field` mesh as before, at 1 and 12
    /// threads, and save back to the same JSON (so ledger contexts hold).
    #[test]
    fn legacy_sweeps_mesh_identically() {
        for (name, c, old) in legacy_fixtures() {
            for threads in [1, 12] {
                assert_eq!(crate::clay::with_threads(threads, || hash(&c.build_mesh())), old, "{name} at {threads} threads");
            }
            let saved = serde_json::to_value(&c.blobs).unwrap();
            assert!(saved.as_array().unwrap().iter().all(|b| b.get("field").is_none() && b.get("radii").is_none()), "{name}: {saved}");
            let back: Vec<crate::clay::Blob> = serde_json::from_value(saved.clone()).unwrap();
            assert_eq!(serde_json::to_value(&back).unwrap(), saved);
            assert_eq!(crate::ledger::clay_context(&c), crate::ledger::clay_context(&Clay { blobs: back, ..c.clone() }));
        }
        // A circle saves as {circle, sides} and reads back the same.
        let c = tube_clay(json!({ "type": "sweep", "profile": circle(0.01), "points": [[0, 0, 0], [1, 0, 0]], "field": "segments" }), 64);
        let saved = serde_json::to_value(&c.blobs[0]).unwrap();
        assert_eq!((saved["profile"].clone(), saved["field"].clone()), (json!({ "circle": 0.01f32, "sides": 16 }), json!("segments")));
        assert_eq!(serde_json::from_value::<crate::clay::Blob>(saved).unwrap(), c.blobs[0]);
    }

    #[test]
    fn agent_sweeps_get_segments_and_bad_tubes_are_refused() {
        let mut v = json!({ "type": "array", "count": 3, "step": [1, 0, 0], "source": { "type": "sweep", "profile": circle(0.01), "points": [[0, 0, 0], [0, 1, 0]] } });
        fill_field(&mut v);
        assert_eq!(v["source"]["field"], "segments");
        let mut legacy = json!({ "type": "sweep", "field": "legacy", "profile": circle(0.01), "points": [[0, 0, 0], [0, 1, 0]] });
        fill_field(&mut legacy);
        assert_eq!(legacy["field"], "legacy");
        let parse = |b: Value| serde_json::from_value::<crate::clay::Blob>(b).map_err(|e| e.to_string()).and_then(|b| b.validate());
        let points = json!([[0, 0, 0], [0, 1, 0], [1, 1, 0]]);
        assert!(parse(json!({ "type": "sweep", "profile": circle(0.01), "radii": [0.01, 0.02, 0.01], "points": points, "field": "segments" })).is_ok());
        for (bad, why) in [
            (json!({ "type": "sweep", "profile": circle(0.0001), "points": points }), "radius of 0.0005-10"),
            (json!({ "type": "sweep", "profile": { "circle": 0.01, "sides": 2 }, "points": points }), "3-128 sides"),
            (json!({ "type": "sweep", "profile": { "circle": 0.01, "corners": 4 }, "points": points }), "polygon"),
            (json!({ "type": "sweep", "profile": circle(0.01), "radii": [0.01, 0.02], "points": points, "field": "segments" }), "one radius"),
            (json!({ "type": "sweep", "profile": circle(0.01), "radii": [0.01, 0.02, 20], "points": points, "field": "segments" }), "one radius"),
            (json!({ "type": "sweep", "profile": square(), "radii": [0.01, 0.02, 0.01], "points": points, "field": "segments" }), "circle profile"),
            (json!({ "type": "sweep", "profile": circle(0.01), "radii": [0.01, 0.02, 0.01], "points": points }), "field segments"),
            (json!({ "type": "sweep", "profile": circle(0.01), "points": points, "field": "exact" }), "unknown variant"),
        ] {
            let e = parse(bad.clone()).expect_err(&bad.to_string());
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    /// The bounded walk gives the full value below the bound and at least
    /// the bound above it.
    #[test]
    fn bounded_walk_keeps_the_contract() {
        let points = helix();
        for profile in [circle(0.006), ngon(0.006, 6)] {
            let g = geometry(&tube_clay(json!({ "type": "sweep", "profile": profile, "points": path(&points), "field": "segments" }), 64));
            let mut rng = Rng(7);
            for _ in 0..20_000 {
                let p = Vec3::new(rng.next(), rng.next(), rng.next()) * 0.16 - Vec3::new(0.08, 0.02, 0.08);
                let (full, bound) = (g.sdf(p), rng.next() * 0.02);
                let within = g.sdf_within(p, bound);
                if full < bound {
                    assert_eq!(within, full);
                } else {
                    assert!(within >= bound * (1.0 - 1e-5) - 1e-6 && within <= full, "{within} {bound} {full}");
                }
            }
        }
    }

    /// A 512-point wiggly path stays within the sample cap, counted before
    /// sampling, and its tree keeps evaluation logarithmic.
    #[test]
    fn long_paths_stay_within_the_cap() {
        let points: Vec<Vec3> = (0..512).map(|i| { let t = i as f32 * 0.01; Vec3::new(t, (t * 40.0).sin() * 0.01, (t * 31.0).cos() * 0.01) }).collect();
        let c = tube_clay(json!({ "type": "sweep", "profile": circle(0.0005), "points": path(&points), "field": "segments" }), 64);
        let t = tube(&c);
        assert!(t.spans() < MAX_SAMPLES, "{}", t.spans());
        assert!(t.spans() > 2000, "{} spans: the cap loosened the tolerance only as far as needed", t.spans());
        let polygon = tube(&tube_clay(json!({ "type": "sweep", "profile": ngon(0.0005, 8), "points": path(&points), "field": "segments" }), 64));
        println!("{} spans: circle {} KB, polygon {} KB", t.spans(), t.bytes() / 1024, polygon.bytes() / 1024);
        assert!(t.bytes() <= 200 * 1024);
        // Every sample of a tube with no tolerance to spare is a span end.
        let few = tube(&tube_clay(json!({ "type": "sweep", "profile": circle(0.01), "points": [[0, 0, 0], [1, 0, 0]], "path_smooth": false, "field": "segments" }), 64));
        assert_eq!(few.spans(), 1);
    }

    /// Evaluation cost at about 2,000 spans (perf: CLIDE_PERF=1 asserts).
    #[test]
    fn tube_evaluation_cost() {
        let points: Vec<Vec3> = (0..200).map(|i| { let a = i as f32 * 0.25; Vec3::new(0.05 * a.cos(), i as f32 * 0.002, 0.05 * a.sin()) }).collect();
        let strict = std::env::var("CLIDE_PERF").is_ok();
        for (profile, bound) in [(circle(0.0005), 0.5), (ngon(0.0005, 8), 2.0)] {
            let c = tube_clay(json!({ "type": "sweep", "profile": profile, "points": path(&points), "field": "segments" }), 64);
            let (t, g) = (tube(&c), geometry(&c));
            let mut rng = Rng(11);
            let near: Vec<Vec3> = (0..200_000).map(|_| points[(rng.next() * 199.0) as usize] + rng.dir() * 0.002 * rng.next()).collect();
            let start = std::time::Instant::now();
            let sum: f32 = near.iter().map(|p| g.sdf(*p)).sum();
            let us = start.elapsed().as_secs_f64() * 1e6 / near.len() as f64;
            println!("{} spans: {us:.3} us per point near the tube ({sum})", t.spans());
            assert!((1500..4096).contains(&t.spans()));
            assert!(us <= bound || !strict && us <= 3.0 * bound, "{us} us");
        }
    }

    /// Skipping far carves by their box changes no value, for round,
    /// sharp and chamfer joins, and meshes the same at 1 and 12 threads.
    #[test]
    fn carve_cull_is_exact() {
        let mut blobs = vec![json!({ "type": "box", "center": [0, 0, 0], "size": [0.3, 0.1, 0.3], "round": 0.01 })];
        let mut rng = Rng(3);
        for i in 0..40 {
            let start = Vec3::new(rng.next() - 0.5, 0.0, rng.next() - 0.5) * 0.28 + Vec3::Y * 0.05;
            let points: Vec<Vec3> = (0..6).map(|k| start + Vec3::new(k as f32 * 0.02, (rng.next() - 0.5) * 0.004, (rng.next() - 0.5) * 0.03)).collect();
            let join = ["round", "sharp", "chamfer"][i % 3];
            blobs.push(json!({ "type": "sweep", "op": "carve", "join": join, "profile": circle(0.004), "points": path(&points), "field": "segments", "blend": 0.003 }));
        }
        blobs.push(json!({ "type": "sphere", "op": "carve", "center": [0.1, 0.05, 0.1], "radius": 0.02, "blend": 0.004, "mirror": true }));
        // Forms whose value is only a bound, a legacy sweep and an array.
        blobs.push(json!({ "type": "ellipsoid", "op": "carve", "center": [-0.05, 0.05, 0.08], "radii": [0.04, 0.01, 0.02], "rotate": [0, 30, 0], "blend": 0.004 }));
        blobs.push(json!({ "type": "sweep", "op": "carve", "profile": ngon(0.006, 8), "points": [[-0.12, 0.05, 0.0], [0.0, 0.055, 0.02], [0.12, 0.05, 0.0]], "blend": 0.002 }));
        blobs.push(json!({ "type": "array", "op": "carve", "source": { "type": "sphere", "center": [0, 0.05, -0.05], "radius": 0.008 }, "count": 5, "step": [0.03, 0, 0], "blend": 0.002, "flip": true }));
        blobs.push(json!({ "type": "capsule", "op": "carve", "join": "chamfer", "a": [-0.1, 0.05, -0.1], "b": [0.0, 0.05, -0.12], "radius": 0.01, "blend": 0.004 }));
        let c: Clay = serde_json::from_value(json!({ "blobs": blobs, "resolution": 160 })).unwrap();
        let voxel = c.cell_size().unwrap();
        let mut culled = Field::new(&c, voxel);
        let (mut differ, mut n) = (0, 0);
        for z in 0..48 {
            for y in 0..24 {
                for x in 0..48 {
                    let p = Vec3::new(x as f32 / 47.0 - 0.5, y as f32 / 23.0 - 0.5, z as f32 / 47.0 - 0.5) * Vec3::new(0.34, 0.14, 0.34);
                    let list = culled.near(p - voxel, p + voxel);
                    culled.cull_carves = true;
                    let a = culled.sample(p, &list).0;
                    culled.cull_carves = false;
                    let b = culled.sample(p, &list).0;
                    differ += usize::from(a.to_bits() != b.to_bits());
                    n += 1;
                }
            }
        }
        assert_eq!(differ, 0, "{differ} of {n} values changed");
        let one = crate::clay::with_threads(1, || hash(&c.build_mesh()));
        assert_eq!(crate::clay::with_threads(12, || hash(&c.build_mesh())), one);
    }

    /// 48 carve sweeps (tube radius r, 20 control points each) wandering
    /// over a 0.3 m ball, at detail cell 0.0008.
    fn heavy_carves(profile: impl Fn(f32) -> Value, field: &str) -> Clay {
        let mut blobs = vec![json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.15 })];
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for _ in 0..48 {
            let (mut at, mut heading) = (rng.dir(), rng.dir());
            let mut points = Vec::new();
            for _ in 0..20 {
                points.push((at * 0.15).to_array());
                heading = (heading + rng.dir() * 0.6).normalize();
                heading = (heading - at * heading.dot(at)).normalize_or(at.any_orthonormal_vector());
                at = (at + heading * 0.06).normalize();
            }
            blobs.push(json!({ "type": "sweep", "op": "carve", "profile": profile(0.003), "points": points, "blend": 0.001, "field": field }));
        }
        serde_json::from_value(json!({ "blobs": blobs, "detail_cell": 0.0008 })).unwrap()
    }

    /// Defect 6 regression (perf: CLIDE_PERF=1 asserts the 10 s bound,
    /// else 30 s): the 48-carve ball's fine mesh, with a progress bar that
    /// only rises and ends full. The legacy field took 64 s (regular) and
    /// 534 s (fine). Ignored by default: a timing bound is flaky in a full
    /// parallel run (`cargo test --release -- --ignored heavy_carve`).
    #[test]
    #[ignore]
    fn heavy_carve_rebuild_time() {
        let c = heavy_carves(circle, "segments");
        let start = std::time::Instant::now();
        let regular = c.build_mesh();
        let regular_s = start.elapsed().as_secs_f32();
        let progress = std::sync::Arc::new(crate::progress::Progress::default());
        let (reports, build) = (progress.clone(), c.clone());
        let start = std::time::Instant::now();
        let thread = std::thread::spawn(move || crate::clay::with_progress(reports, || build.build_fine(&[], "")));
        let mut seen = vec![0.0f32];
        while !thread.is_finished() {
            seen.push(progress.fraction());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let fine = thread.join().unwrap().unwrap();
        let fine_s = start.elapsed().as_secs_f32();
        seen.push(progress.fraction());
        println!("regular: {} triangles at {} cells in {regular_s:.2} s; fine: {} triangles in {fine_s:.2} s; progress read {} times, last {}", regular.indices.len() / 3, c.mesh_resolution(), fine.indices.len() / 3, seen.len(), seen.last().unwrap());
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "progress went back");
        assert!(*seen.last().unwrap() >= 1.0, "the bar ends full");
        let strict = std::env::var("CLIDE_PERF").is_ok();
        assert!(fine_s <= 10.0 || !strict && fine_s <= 30.0, "{fine_s} s");
    }

    /// Polygon tubes: closed, the same at 1 and 12 threads, and a sharp
    /// bend in a polygonal path keeps them closed (mitre to 120 degrees,
    /// square ends past it).
    #[test]
    fn polygon_tubes_mesh_closed() {
        for (points, smooth) in [(helix(), true), (vec![Vec3::ZERO, Vec3::X * 0.1, Vec3::new(0.1, 0.1, 0.0), Vec3::new(0.02, 0.02, 0.0)], false)] {
            let c = tube_clay(json!({ "type": "sweep", "profile": ngon(0.008, 5), "points": path(&points), "path_smooth": smooth, "twist": 45, "taper": 0.6, "field": "segments", "blend": 0 }), 120);
            let one = crate::clay::with_threads(1, || c.build_mesh());
            assert!(!one.indices.is_empty());
            assert_eq!(crate::clay::with_threads(12, || hash(&c.build_mesh())), hash(&one));
            let mut edges = std::collections::HashMap::new();
            let weld: Vec<[u32; 3]> = one.vertices.iter().map(|v| v.pos.map(f32::to_bits)).collect();
            for t in one.indices.chunks_exact(3) {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    *edges.entry((weld[a as usize], weld[b as usize])).or_insert(0) += 1;
                }
            }
            assert!(edges.iter().all(|(&(a, b), &n)| n == 1 && edges.get(&(b, a)) == Some(&1)), "open or doubled edges (smooth {smooth})");
        }
    }
}
