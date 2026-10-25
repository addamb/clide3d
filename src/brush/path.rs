//! Stroke paths: points with optional pressure, resampling by spacing,
//! LazyMouse smoothing and taper.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// One authored path point: [x, y, z], [x, y, z, pressure] or
/// {p: [x, y, z], pressure?, normal?, t?} (`t`: seconds since the stroke
/// began, recorded by the UI).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PathPoint {
    pub p: Vec3,
    pub pressure: Option<f32>,
    pub normal: Option<Vec3>,
    pub t: Option<f32>,
}

impl PathPoint {
    pub fn at(p: Vec3) -> PathPoint {
        PathPoint { p, pressure: None, normal: None, t: None }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PointFile {
    List(Vec<f32>),
    Full {
        p: Vec3,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressure: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal: Option<Vec3>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        t: Option<f32>,
    },
}

impl Serialize for PathPoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let PathPoint { p, pressure, normal, t } = *self;
        match (pressure, normal, t) {
            (None, None, None) => PointFile::List(p.to_array().to_vec()),
            (Some(pr), None, None) => PointFile::List(vec![p.x, p.y, p.z, pr]),
            _ => PointFile::Full { p, pressure, normal, t },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for PathPoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = || serde::de::Error::custom("a point is [x, y, z], [x, y, z, pressure] or {p: [x, y, z], pressure, normal, t}");
        let point = match PointFile::deserialize(d).map_err(|_| bad())? {
            PointFile::List(v) if v.len() == 3 => PathPoint::at(Vec3::new(v[0], v[1], v[2])),
            PointFile::List(v) if v.len() == 4 => PathPoint { pressure: Some(v[3]), ..PathPoint::at(Vec3::new(v[0], v[1], v[2])) },
            PointFile::List(_) => return Err(bad()),
            PointFile::Full { p, pressure, normal, t } => PathPoint { p, pressure, normal, t },
        };
        if point.pressure.is_some_and(|p| !(0.0..=1.0).contains(&p)) {
            return Err(serde::de::Error::custom("pressure must be 0..1"));
        }
        Ok(point)
    }
}

/// A path point after resolving: object space, with its pressure.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sample {
    pub p: Vec3,
    pub pressure: Option<f32>,
    pub normal: Option<Vec3>,
}

/// Total length of a polyline.
pub fn length(points: &[Sample]) -> f32 {
    points.windows(2).map(|w| w[0].p.distance(w[1].p)).sum()
}

fn lerp(a: &Sample, b: &Sample, s: f32) -> Sample {
    let mix = |x: Option<f32>, y: Option<f32>| match (x, y) {
        (Some(x), Some(y)) => Some(x + (y - x) * s),
        (x, y) => x.or(y),
    };
    let normal = match (a.normal, b.normal) {
        (Some(x), Some(y)) => Some(x.lerp(y, s).normalize_or(x)),
        (x, y) => x.or(y),
    };
    Sample { p: a.p.lerp(b.p, s), pressure: mix(a.pressure, b.pressure), normal }
}

/// Most points `curve` makes per span between two control points.
pub const CURVE_SPAN_POINTS: usize = 32;

/// A smooth curve through control points (centripetal Catmull-Rom, which
/// neither overshoots nor loops on uneven spacing), as a polyline: each
/// span in steps of about `step` (at least 1, at most
/// `CURVE_SPAN_POINTS`), pressure and normals blended along it. `closed`
/// runs on from the last point back to the first. At most
/// (points + 1) x `CURVE_SPAN_POINTS` + 1 points, whatever the
/// coordinates; spans that are not finite take one step.
pub fn curve(points: &[Sample], closed: bool, step: f32) -> Vec<Sample> {
    let n = points.len();
    if n < 2 {
        return points.to_vec();
    }
    let at = |i: isize| -> &Sample {
        if closed {
            &points[i.rem_euclid(n as isize) as usize]
        } else {
            &points[i.clamp(0, n as isize - 1) as usize]
        }
    };
    let spans = if closed { n } else { n - 1 };
    let mut out = Vec::with_capacity(spans * CURVE_SPAN_POINTS + 1);
    for k in 0..spans as isize {
        let (a, b) = (at(k), at(k + 1));
        // The outer control points; an open curve's ends mirror the
        // first and last spans.
        let p0 = if !closed && k == 0 { a.p * 2.0 - b.p } else { at(k - 1).p };
        let p3 = if !closed && k + 2 > n as isize - 1 { b.p * 2.0 - a.p } else { at(k + 2).p };
        let chord = a.p.distance(b.p);
        let steps = if chord.is_finite() && step > 0.0 { ((chord / step).ceil() as usize).clamp(1, CURVE_SPAN_POINTS) } else { 1 };
        let knot = |x: Vec3, y: Vec3| x.distance(y).sqrt().max(1e-6);
        let (t1, t2, t3) = (knot(p0, a.p), knot(a.p, b.p), knot(b.p, p3));
        let (t1, t2, t3) = (t1, t1 + t2, t1 + t2 + t3);
        for j in 0..steps {
            let s = j as f32 / steps as f32;
            let t = t1 + (t2 - t1) * s;
            let mix = |x: Vec3, y: Vec3, ta: f32, tb: f32| x * ((tb - t) / (tb - ta)) + y * ((t - ta) / (tb - ta));
            let (a1, a2, a3) = (mix(p0, a.p, 0.0, t1), mix(a.p, b.p, t1, t2), mix(b.p, p3, t2, t3));
            let (b1, b2) = (mix(a1, a2, 0.0, t2), mix(a2, a3, t1, t3));
            let p = mix(b1, b2, t1, t2);
            let base = lerp(a, b, s);
            // Each span starts on its control point exactly.
            out.push(Sample { p: if j == 0 { a.p } else if p.is_finite() { p } else { base.p }, ..base });
        }
    }
    out.push(if closed { points[0] } else { points[n - 1] });
    out
}

/// How many points `resample(points, step)` makes, at most: infinite or
/// NaN for paths too long to count. Callers check this against a limit
/// before resampling agent input.
pub fn resample_count(points: &[Sample], step: f32) -> f32 {
    if points.len() < 2 || step.is_nan() || step <= 0.0 {
        return points.len() as f32;
    }
    (length(points) / step).floor() + 2.0
}

/// Points every `step` along the path from its start, plus its end: a
/// path of length L gives floor(L / step) + 1 points, or one more when
/// the end falls between two steps. Bound the count first with
/// `resample_count`.
pub fn resample(points: &[Sample], step: f32) -> Vec<Sample> {
    if points.len() < 2 || step.is_nan() || step <= 0.0 {
        return points.to_vec();
    }
    let total = length(points);
    if total <= 0.0 {
        return vec![points[0]];
    }
    let count = (total / step).floor() as usize;
    let mut out = Vec::with_capacity(count + 2);
    let (mut seg, mut seg_start) = (0usize, 0.0f32);
    for k in 0..=count {
        let s = k as f32 * step;
        while seg + 1 < points.len() - 1 && seg_start + points[seg].p.distance(points[seg + 1].p) < s {
            seg_start += points[seg].p.distance(points[seg + 1].p);
            seg += 1;
        }
        let len = points[seg].p.distance(points[seg + 1].p);
        let f = if len > 0.0 { ((s - seg_start) / len).clamp(0.0, 1.0) } else { 0.0 };
        out.push(lerp(&points[seg], &points[seg + 1], f));
    }
    if total - count as f32 * step > step * 1e-3 {
        out.push(*points.last().expect("two points"));
    }
    out
}

/// LazyMouse: the brush trails the path on a string of length `string`,
/// so small wobbles are smoothed out. The path is first densified, so
/// sparse agent paths are smoothed too (to `resample_count(points,
/// lazy_step(string))` points, which callers bound first).
pub fn lazy(points: &[Sample], string: f32) -> Vec<Sample> {
    if points.len() < 2 || string.is_nan() || string <= 0.0 {
        return points.to_vec();
    }
    let dense = resample(points, lazy_step(string));
    let mut at = dense[0];
    let mut out = vec![at];
    for q in &dense[1..] {
        let d = q.p.distance(at.p);
        if d > string {
            at = lerp(&at, q, (d - string) / d);
            out.push(at);
        }
    }
    out
}

/// The step LazyMouse densifies a path to.
pub fn lazy_step(string: f32) -> f32 {
    string * 0.1
}

/// Taper factor at fraction `u` (0..1) along a path that fades in over
/// the first `taper[0]` and out over the last `taper[1]` of its length.
pub fn taper(u: f32, taper: [f32; 2]) -> f32 {
    let ramp = |x: f32, span: f32| if span > 0.0 { smooth01(x / span) } else { 1.0 };
    ramp(u, taper[0]) * ramp(1.0 - u, taper[1])
}

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Fraction of the path's length at each point.
pub fn fractions(points: &[Sample]) -> Vec<f32> {
    let total = length(points);
    let mut run = 0.0;
    let mut out = vec![0.0];
    for w in points.windows(2) {
        run += w[0].p.distance(w[1].p);
        out.push(if total > 0.0 { run / total } else { 0.0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(len: f32) -> Vec<Sample> {
        [Vec3::ZERO, Vec3::new(len * 0.3, 0.0, 0.0), Vec3::new(len * 0.3, len * 0.7, 0.0)].iter().map(|p| Sample { p: *p, pressure: None, normal: None }).collect()
    }

    #[test]
    fn resampling_spaces_points_evenly() {
        let path = line(1.0);
        let out = resample(&path, 0.1);
        assert!((10..=12).contains(&out.len()), "{}", out.len());
        assert_eq!(out[0].p, Vec3::ZERO);
        assert_eq!(out.last().unwrap().p, path[2].p);
        let gaps: Vec<f32> = out.windows(2).map(|w| w[0].p.distance(w[1].p)).collect();
        // Chords across the corner are a little shorter than the step.
        assert!(gaps[..gaps.len() - 1].iter().all(|g| (0.07..=0.1001).contains(g)), "{gaps:?}");
        assert!(out.len() as f32 <= resample_count(&path, 0.1));
        assert!(!(resample_count(&[path[0], Sample { p: Vec3::splat(f32::MAX), ..path[0] }, path[0]], 0.1) <= 1e9), "overflow counts as too many");
    }

    #[test]
    fn points_parse_in_every_form() {
        let pts: Vec<PathPoint> = serde_json::from_str(r#"[[1,2,3],[1,2,3,0.5],{"p":[1,2,3],"normal":[0,1,0],"t":0.25}]"#).unwrap();
        assert_eq!(pts[1].pressure, Some(0.5));
        assert_eq!(pts[2].normal, Some(Vec3::Y));
        let back: Vec<PathPoint> = serde_json::from_value(serde_json::to_value(&pts).unwrap()).unwrap();
        assert_eq!(back, pts);
        assert!(serde_json::from_str::<PathPoint>("[1,2]").is_err());
        assert!(serde_json::from_str::<PathPoint>("[1,2,3,1.5]").is_err());
    }

    #[test]
    fn lazy_smooths_a_zigzag_and_taper_fades_both_ends() {
        let zig: Vec<Sample> = (0..40).map(|i| Sample { p: Vec3::new(i as f32 * 0.05, if i % 2 == 0 { 0.02 } else { -0.02 }, 0.0), pressure: None, normal: None }).collect();
        let smooth = lazy(&zig, 0.1);
        // Once the string is taut, the wobble is mostly gone.
        let tail = &smooth[smooth.len() / 2..];
        assert!(tail.len() > 5 && tail.iter().all(|s| s.p.y.abs() < 0.01), "zigzag kept: {:?}", tail.iter().map(|s| s.p.y).collect::<Vec<_>>());
        assert_eq!(taper(0.0, [0.2, 0.3]), 0.0);
        assert_eq!(taper(0.5, [0.2, 0.3]), 1.0);
        assert_eq!(taper(1.0, [0.2, 0.3]), 0.0);
        assert_eq!(taper(0.0, [0.0, 0.0]), 1.0);
    }
}
