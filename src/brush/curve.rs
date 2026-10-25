//! Brush falloff curves: how a dab's effect fades from its center (t = 0)
//! to its edge (t = 1). Nothing is changed at t >= 1.
//!
//! Authored as a preset name, a list of [t, value] points (a monotone
//! cubic through them), or an object with either plus `focal_shift`:
//! - smooth: (1 - t^2)^2, the classic clay falloff;
//! - linear: 1 - t;
//! - sharp: (1 - t)^2, a pointed peak;
//! - flat: 1 out to 0.9, then a quick smooth step down to 0 at the edge;
//! - sphere: sqrt(1 - t^2), a round dome.
//!
//! `focal_shift` (-1..1) moves the falloff in or out: t is raised to the
//! power 2^(-2 * focal_shift) first, so +0.5 makes the dab narrower and
//! pointier (t -> sqrt(t)) and -0.5 fuller (t -> t^2).
//!
//! Strokes without a `falloff` keep the curve they always had (`Curve::
//! Legacy` for sculpt, `Curve::Hardness` for paint), bit for bit.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Most points in a custom curve.
pub const MAX_CURVE_POINTS: usize = 16;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    #[default]
    Smooth,
    Linear,
    Sharp,
    Flat,
    Sphere,
}

impl Preset {
    fn eval(self, t: f32) -> f32 {
        match self {
            Preset::Smooth => {
                let x = (1.0 - t * t).max(0.0);
                x * x
            }
            Preset::Linear => (1.0 - t).max(0.0),
            Preset::Sharp => (1.0 - t).max(0.0).powi(2),
            Preset::Flat => 1.0 - smooth01((t - 0.9) / 0.1),
            Preset::Sphere => (1.0 - t * t).max(0.0).sqrt(),
        }
    }
}

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The authored falloff (see the module doc).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Falloff {
    pub preset: Preset,
    /// [t, value] points, t rising from 0 to 1; replaces the preset.
    pub curve: Option<Vec<[f32; 2]>>,
    pub focal_shift: f32,
}

/// What `Falloff` reads from and writes to JSON.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum FalloffFile {
    Preset(Preset),
    Curve(Vec<[f32; 2]>),
    Full {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<Preset>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        curve: Option<Vec<[f32; 2]>>,
        #[serde(default, skip_serializing_if = "is_zero")]
        focal_shift: f32,
    },
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Serialize for Falloff {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match (&self.curve, self.focal_shift == 0.0) {
            (None, true) => FalloffFile::Preset(self.preset),
            (Some(c), true) => FalloffFile::Curve(c.clone()),
            (c, false) => FalloffFile::Full { preset: c.is_none().then_some(self.preset), curve: c.clone(), focal_shift: self.focal_shift },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Falloff {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let f = match FalloffFile::deserialize(d).map_err(|_| serde::de::Error::custom("falloff must be a preset (smooth, linear, sharp, flat, sphere), a list of [t, value] points, or {preset or curve, focal_shift}"))? {
            FalloffFile::Preset(preset) => Falloff { preset, ..Falloff::default() },
            FalloffFile::Curve(curve) => Falloff { curve: Some(curve), ..Falloff::default() },
            FalloffFile::Full { preset, curve, focal_shift } => {
                if preset.is_some() && curve.is_some() {
                    return Err(serde::de::Error::custom("falloff takes a preset or a curve, not both"));
                }
                Falloff { preset: preset.unwrap_or_default(), curve, focal_shift }
            }
        };
        f.validate().map_err(serde::de::Error::custom)?;
        Ok(f)
    }
}

impl Falloff {
    pub fn validate(&self) -> Result<(), String> {
        if !self.focal_shift.is_finite() || self.focal_shift.abs() > 1.0 {
            return Err("focal_shift must be -1..1".into());
        }
        if let Some(c) = &self.curve {
            if !(2..=MAX_CURVE_POINTS).contains(&c.len()) {
                return Err(format!("a falloff curve needs 2 to {MAX_CURVE_POINTS} [t, value] points"));
            }
            if c.iter().any(|[t, v]| !t.is_finite() || !v.is_finite() || !(0.0..=1.0).contains(t) || !(0.0..=1.0).contains(v)) {
                return Err("falloff curve points must be [t, value] with both 0..1".into());
            }
            if c.windows(2).any(|w| w[1][0] <= w[0][0]) {
                return Err("falloff curve t values must rise".into());
            }
        }
        Ok(())
    }

    /// The curve a stroke evaluates.
    pub fn resolve(&self) -> Curve {
        let shape = match &self.curve {
            Some(points) => Shape::Points(Arc::new(Monotone::new(points))),
            None => Shape::Preset(self.preset),
        };
        Curve::Shaped { shape, power: (-2.0 * self.focal_shift).exp2() }
    }
}

/// A resolved falloff curve. Records keep it (see `ledger`): it saves as
/// the form it came from and resolves again on load, which gives the same
/// curve bit for bit.
#[derive(Clone, PartialEq, Debug)]
pub enum Curve {
    /// The sculpt falloff strokes always had, (1 - t^2)^2, computed exactly
    /// as before so legacy strokes are bit-identical.
    Legacy,
    /// The paint falloff strokes always had: 1 inside `hardness` of the
    /// radius, easing to 0 at the edge.
    Hardness(f32),
    Shaped { shape: Shape, power: f32 },
}

#[derive(Clone, PartialEq, Debug)]
pub enum Shape {
    Preset(Preset),
    Points(Arc<Monotone>),
}

impl Curve {
    /// The weight at distance `d` from a dab of `radius`.
    pub fn weight(&self, d: f32, radius: f32) -> f32 {
        match self {
            Curve::Legacy => legacy(d / radius),
            Curve::Hardness(h) => hardness(d, radius, *h),
            Curve::Shaped { .. } => self.eval(d / radius),
        }
    }

    /// The weight at `t` = distance / radius.
    pub fn eval(&self, t: f32) -> f32 {
        match self {
            Curve::Legacy => legacy(t),
            Curve::Hardness(h) => hardness(t, 1.0, *h),
            Curve::Shaped { shape, power } => {
                if t.is_nan() || t >= 1.0 {
                    return 0.0;
                }
                let t = t.max(0.0);
                let t = if *power == 1.0 { t } else { t.powf(*power) };
                match shape {
                    Shape::Preset(p) => p.eval(t),
                    Shape::Points(m) => m.eval(t),
                }
            }
        }
    }
}

/// What `Curve` reads from and writes to a record.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CurveFile {
    Legacy,
    Hardness(f32),
    Preset { preset: Preset, power: f32 },
    Points { points: Vec<[f32; 2]>, power: f32 },
}

impl Serialize for Curve {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Curve::Legacy => CurveFile::Legacy,
            Curve::Hardness(h) => CurveFile::Hardness(*h),
            Curve::Shaped { shape: Shape::Preset(preset), power } => CurveFile::Preset { preset: *preset, power: *power },
            Curve::Shaped { shape: Shape::Points(m), power } => CurveFile::Points { points: m.points(), power: *power },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Curve {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = |what: &str| serde::de::Error::custom(format!("bad falloff curve: {what}"));
        Ok(match CurveFile::deserialize(d)? {
            CurveFile::Legacy => Curve::Legacy,
            CurveFile::Hardness(h) if h.is_finite() => Curve::Hardness(h),
            CurveFile::Hardness(_) => return Err(bad("hardness")),
            CurveFile::Preset { preset, power } if power.is_finite() && power > 0.0 => Curve::Shaped { shape: Shape::Preset(preset), power },
            CurveFile::Points { points, power } if power.is_finite() && power > 0.0 => {
                Falloff { curve: Some(points.clone()), ..Falloff::default() }.validate().map_err(|e| bad(&e))?;
                Curve::Shaped { shape: Shape::Points(Arc::new(Monotone::new(&points))), power }
            }
            _ => return Err(bad("power")),
        })
    }
}

/// Smooth falloff: 1 at 0, 0 at 1 and beyond (the original sculpt curve).
pub fn legacy(t: f32) -> f32 {
    let x = (1.0 - t * t).max(0.0);
    x * x
}

/// Paint falloff with adjustable hardness: 1 inside `hardness * radius`,
/// easing to 0 at the radius (the original paint curve).
pub fn hardness(d: f32, radius: f32, hardness: f32) -> f32 {
    let inner = radius * hardness.clamp(0.0, 0.95);
    if d <= inner {
        return 1.0;
    }
    let t = ((d - inner) / (radius - inner)).clamp(0.0, 1.0);
    let s = 1.0 - t;
    s * s * (3.0 - 2.0 * s)
}

/// A monotone cubic (Fritsch-Carlson) through [x, y] points: it never
/// overshoots, so a falling curve keeps falling. Flat beyond the ends.
#[derive(Clone, PartialEq, Debug)]
pub struct Monotone {
    xs: Vec<f32>,
    ys: Vec<f32>,
    slopes: Vec<f32>,
}

impl Monotone {
    pub fn new(points: &[[f32; 2]]) -> Monotone {
        let xs: Vec<f32> = points.iter().map(|p| p[0]).collect();
        let ys: Vec<f32> = points.iter().map(|p| p[1]).collect();
        let n = xs.len();
        let secant: Vec<f32> = (0..n.saturating_sub(1)).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
        let mut slopes = vec![0.0; n];
        if n >= 2 {
            slopes[0] = secant[0];
            slopes[n - 1] = secant[n - 2];
        }
        for i in 1..n.saturating_sub(1) {
            slopes[i] = if secant[i - 1] * secant[i] <= 0.0 { 0.0 } else { (secant[i - 1] + secant[i]) * 0.5 };
        }
        for i in 0..n.saturating_sub(1) {
            if secant[i] == 0.0 {
                slopes[i] = 0.0;
                slopes[i + 1] = 0.0;
                continue;
            }
            let (a, b) = (slopes[i] / secant[i], slopes[i + 1] / secant[i]);
            let h = a * a + b * b;
            if h > 9.0 {
                let k = 3.0 / h.sqrt();
                slopes[i] = k * a * secant[i];
                slopes[i + 1] = k * b * secant[i];
            }
        }
        Monotone { xs, ys, slopes }
    }

    /// The [x, y] points it passes through.
    pub fn points(&self) -> Vec<[f32; 2]> {
        self.xs.iter().zip(&self.ys).map(|(x, y)| [*x, *y]).collect()
    }

    pub fn eval(&self, x: f32) -> f32 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let i = self.xs.partition_point(|v| *v <= x) - 1;
        let h = self.xs[i + 1] - self.xs[i];
        let s = (x - self.xs[i]) / h;
        let (s2, s3) = (s * s, s * s * s);
        let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
        let h10 = s3 - 2.0 * s2 + s;
        let h01 = -2.0 * s3 + 3.0 * s2;
        let h11 = s3 - s2;
        h00 * self.ys[i] + h10 * h * self.slopes[i] + h01 * self.ys[i + 1] + h11 * h * self.slopes[i + 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_curve_preset_is_the_old_falloff() {
        let smooth = Falloff::default().resolve();
        for i in 0..=2000 {
            let t = i as f32 / 1000.0;
            assert!((smooth.eval(t) - legacy(t)).abs() <= 1e-6, "t {t}");
            assert_eq!(Curve::Legacy.eval(t), legacy(t));
        }
    }

    #[test]
    fn presets_fall_from_one_to_zero() {
        for p in [Preset::Smooth, Preset::Linear, Preset::Sharp, Preset::Flat, Preset::Sphere] {
            let c = Falloff { preset: p, ..Falloff::default() }.resolve();
            assert_eq!(c.eval(0.0), 1.0, "{p:?}");
            assert_eq!(c.eval(1.0), 0.0, "{p:?}");
            assert_eq!(c.eval(1.5), 0.0, "{p:?}");
            let v: Vec<f32> = (0..=100).map(|i| c.eval(i as f32 / 100.0)).collect();
            assert!(v.windows(2).all(|w| w[1] <= w[0] + 1e-7), "{p:?} rises");
        }
        let flat = Falloff { preset: Preset::Flat, ..Falloff::default() }.resolve();
        assert_eq!(flat.eval(0.9), 1.0);
    }

    #[test]
    fn focal_shift_narrows_or_widens() {
        let half = |f: f32| {
            let c = Falloff { focal_shift: f, ..Falloff::default() }.resolve();
            (0..10_000).map(|i| i as f32 / 10_000.0).find(|t| c.eval(*t) < 0.5).unwrap()
        };
        let (narrow, plain, wide) = (half(0.5), half(0.0), half(-0.5));
        assert!(narrow < plain && plain < wide, "{narrow} {plain} {wide}");
        // (1 - u^2)^2 = 1/2 at u^2 = 1 - 1/sqrt 2; +0.5 maps t to sqrt t.
        assert!((narrow - (1.0 - 0.5f32.sqrt())).abs() < 1e-3, "{narrow}");
    }

    #[test]
    fn custom_curves_pass_through_their_points_without_overshoot() {
        let pts = [[0.0, 1.0], [0.3, 0.95], [0.5, 0.4], [0.7, 0.35], [1.0, 0.0]];
        let m = Monotone::new(&pts);
        for p in pts {
            assert!((m.eval(p[0]) - p[1]).abs() < 1e-6);
        }
        let v: Vec<f32> = (0..=1000).map(|i| m.eval(i as f32 / 1000.0)).collect();
        assert!(v.windows(2).all(|w| w[1] <= w[0] + 1e-6), "monotone data stays monotone");
    }

    #[test]
    fn falloff_json_forms_round_trip_and_bad_ones_are_refused() {
        for text in [r#""linear""#, r#"[[0,1],[0.5,0.5],[1,0]]"#, r#"{"preset":"flat","focal_shift":0.25}"#, r#"{"curve":[[0,1],[1,0]],"focal_shift":-0.5}"#] {
            let f: Falloff = serde_json::from_str(text).unwrap();
            let back: Falloff = serde_json::from_value(serde_json::to_value(&f).unwrap()).unwrap();
            assert_eq!(f, back, "{text}");
        }
        for bad in [r#""spiky""#, r#"[[0,1]]"#, r#"[[0.5,1],[0.2,0]]"#, r#"{"focal_shift":2}"#, r#"{"preset":"flat","curve":[[0,1],[1,0]]}"#, r#"[[0,2],[1,0]]"#] {
            assert!(serde_json::from_str::<Falloff>(bad).is_err(), "{bad}");
        }
    }
}
