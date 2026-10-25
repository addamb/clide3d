//! Pressure curves: how a point's pen pressure (0..1) scales the brush.
//!
//! Each curve maps pressure to a factor: "linear" (the factor is the
//! pressure, the default), "off" (always 1) or [[pressure, factor], ..]
//! points joined by a monotone cubic. Points without pressure use the
//! brush as set; so do strokes where no point has pressure.

use serde::{Deserialize, Serialize};

use super::curve::{MAX_CURVE_POINTS, Monotone};

#[derive(Clone, PartialEq, Debug, Default)]
pub enum PressureCurve {
    #[default]
    Linear,
    Off,
    Points(Vec<[f32; 2]>),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum CurveFile {
    Name(String),
    Points(Vec<[f32; 2]>),
}

impl Serialize for PressureCurve {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PressureCurve::Linear => CurveFile::Name("linear".into()),
            PressureCurve::Off => CurveFile::Name("off".into()),
            PressureCurve::Points(p) => CurveFile::Points(p.clone()),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for PressureCurve {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = || serde::de::Error::custom(format!("a pressure curve is \"linear\", \"off\" or 2 to {MAX_CURVE_POINTS} [pressure, factor] points with pressure rising 0..1 and factors 0..4"));
        match CurveFile::deserialize(d).map_err(|_| bad())? {
            CurveFile::Name(n) if n == "linear" => Ok(PressureCurve::Linear),
            CurveFile::Name(n) if n == "off" => Ok(PressureCurve::Off),
            CurveFile::Name(_) => Err(bad()),
            CurveFile::Points(p) => {
                let ok = (2..=MAX_CURVE_POINTS).contains(&p.len())
                    && p.iter().all(|[x, y]| (0.0..=1.0).contains(x) && (0.0..=4.0).contains(y))
                    && p.windows(2).all(|w| w[1][0] > w[0][0]);
                if ok { Ok(PressureCurve::Points(p)) } else { Err(bad()) }
            }
        }
    }
}

impl PressureCurve {
    pub fn eval(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        match self {
            PressureCurve::Linear => p,
            PressureCurve::Off => 1.0,
            PressureCurve::Points(points) => Monotone::new(points).eval(p),
        }
    }

    fn is_linear(&self) -> bool {
        *self == PressureCurve::Linear
    }
}

/// How pressure scales a dab's size and strength.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct PressureCurves {
    #[serde(default, skip_serializing_if = "PressureCurve::is_linear")]
    pub size: PressureCurve,
    #[serde(default, skip_serializing_if = "PressureCurve::is_linear")]
    pub strength: PressureCurve,
}

impl PressureCurves {
    pub fn is_default(&self) -> bool {
        self.size.is_linear() && self.strength.is_linear()
    }

    /// (size factor, strength factor) at `pressure`; (1, 1) without one.
    pub fn factors(&self, pressure: Option<f32>) -> (f32, f32) {
        match pressure {
            Some(p) => (self.size.eval(p), self.strength.eval(p)),
            None => (1.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_map_pressure() {
        let c = PressureCurves::default();
        assert_eq!(c.factors(None), (1.0, 1.0));
        assert_eq!(c.factors(Some(0.25)), (0.25, 0.25));
        let c: PressureCurves = serde_json::from_value(serde_json::json!({"size": "off", "strength": [[0, 0.2], [1, 1]]})).unwrap();
        assert_eq!(c.factors(Some(0.0)), (1.0, 0.2));
        assert_eq!(serde_json::from_value::<PressureCurves>(serde_json::to_value(&c).unwrap()).unwrap(), c);
        assert!(serde_json::from_value::<PressureCurves>(serde_json::json!({"size": "cubic"})).is_err());
    }
}
