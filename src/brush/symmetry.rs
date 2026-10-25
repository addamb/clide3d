//! Stroke symmetry: mirror copies across the X, Y and Z planes and radial
//! copies around an axis, about the object's pivot (its own origin) or the
//! world origin. A stroke with mirror axes [x] and no radial is applied
//! twice: as given, then mirrored (the old `mirror: true`). With `posable`
//! the points are on the posed clay; the resolver takes them back to the
//! rest pose (`resolve::Unpose`) before the copies are made.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// Most radial copies.
pub const MAX_RADIAL: u32 = 32;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn index(self) -> usize {
        self as usize
    }

    fn unit(self) -> Vec3 {
        [Vec3::X, Vec3::Y, Vec3::Z][self.index()]
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum About {
    /// The object's own origin (its pivot), like mirrored blobs.
    #[default]
    Pivot,
    /// The world origin.
    Origin,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Radial {
    pub axis: Axis,
    pub count: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Symmetry {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<Axis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radial: Option<Radial>,
    #[serde(default, skip_serializing_if = "is_pivot")]
    pub about: About,
    /// The points are on the posed clay (at the editor's frame): each is
    /// taken back to the rest pose through the skin weights where it is
    /// (inverse linear blend skinning), the copies are made there, and the
    /// stroke sculpts the rest pose.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub posable: bool,
    /// The pose `posable` points are on: a frame of the rig's active clip,
    /// or "rest". Left out, the sculpt call fills in the editor's frame and
    /// the record keeps it, so the same spec sent again lands the same way
    /// wherever the timeline is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<PoseFrame>,
}

/// Which pose posable symmetry reads.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(untagged)]
pub enum PoseFrame {
    At(f32),
    Rest(RestWord),
}

/// The word "rest".
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum RestWord {
    Rest,
}

impl PoseFrame {
    /// The editor's frame (None: the rest pose).
    pub fn of(frame: Option<f32>) -> PoseFrame {
        frame.map_or(PoseFrame::Rest(RestWord::Rest), PoseFrame::At)
    }

    /// As a frame for the rig (None: the rest pose).
    pub fn frame(self) -> Option<f32> {
        match self {
            PoseFrame::At(f) => Some(f),
            PoseFrame::Rest(_) => None,
        }
    }
}

fn is_pivot(a: &About) -> bool {
    *a == About::Pivot
}

impl Symmetry {
    pub fn is_none(&self) -> bool {
        self.axes.is_empty() && self.radial.is_none_or(|r| r.count <= 1)
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(r) = self.radial
            && !(1..=MAX_RADIAL).contains(&r.count)
        {
            return Err(format!("radial symmetry count must be 1 to {MAX_RADIAL}"));
        }
        if let Some(PoseFrame::At(f)) = self.frame
            && !f.is_finite()
        {
            return Err("symmetry frame must be a finite frame or \"rest\"".into());
        }
        if self.frame.is_some() && !self.posable {
            return Err("symmetry frame is the pose posable strokes are on; set posable: true or leave frame out".into());
        }
        Ok(())
    }

    /// Every copy, in the order they are applied: the stroke as given
    /// first; mirror sets in x, y, z bit order; radial turns innermost.
    /// `origin` is the world origin in object space.
    pub fn instances(&self, origin: Vec3) -> Vec<Instance> {
        let center = match self.about {
            About::Pivot => Vec3::ZERO,
            About::Origin => origin,
        };
        let mut axes = self.axes.clone();
        axes.sort();
        axes.dedup();
        let turns = self.radial.map_or(1, |r| r.count.max(1));
        let mut out = Vec::new();
        for set in 0..1u32 << axes.len() {
            let mut mirror = [false; 3];
            for (bit, a) in axes.iter().enumerate() {
                mirror[a.index()] = set >> bit & 1 == 1;
            }
            for k in 0..turns {
                let rot = self.radial.filter(|_| k > 0).map(|r| Quat::from_axis_angle(r.axis.unit(), std::f32::consts::TAU * k as f32 / turns as f32));
                out.push(Instance { mirror, center, rot });
            }
        }
        out
    }
}

/// One symmetric copy: mirror, then turn, about `center`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Instance {
    pub mirror: [bool; 3],
    pub center: Vec3,
    pub rot: Option<Quat>,
}

impl Instance {
    pub fn is_identity(&self) -> bool {
        self.mirror == [false; 3] && self.rot.is_none()
    }

    /// Mirrors an odd number of times (turns the handedness of a frame).
    pub fn flips(&self) -> bool {
        self.mirror.iter().filter(|m| **m).count() % 2 == 1
    }

    pub fn point(&self, p: Vec3) -> Vec3 {
        let mut a = p.to_array();
        for ((v, c), m) in a.iter_mut().zip(self.center.to_array()).zip(self.mirror) {
            if m {
                // Exactly -x about the pivot, as the old mirror did.
                *v = if c == 0.0 { -*v } else { 2.0 * c - *v };
            }
        }
        let p = Vec3::from(a);
        match self.rot {
            Some(q) => self.center + q * (p - self.center),
            None => p,
        }
    }

    pub fn vector(&self, v: Vec3) -> Vec3 {
        let mut a = v.to_array();
        for (v, m) in a.iter_mut().zip(self.mirror) {
            if m {
                *v = -*v;
            }
        }
        let v = Vec3::from(a);
        self.rot.map_or(v, |q| q * v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_x_is_the_old_mirror_and_comes_second() {
        let s = Symmetry { axes: vec![Axis::X], ..Symmetry::default() };
        let i = s.instances(Vec3::ZERO);
        assert_eq!(i.len(), 2);
        assert!(i[0].is_identity());
        let p = Vec3::new(0.3, -0.2, 0.7);
        assert_eq!(i[1].point(p), Vec3::new(-p.x, p.y, p.z));
        assert_eq!(i[1].vector(p), Vec3::new(-p.x, p.y, p.z));
    }

    #[test]
    fn radial_and_mirror_sets_multiply() {
        let s = Symmetry { axes: vec![Axis::X, Axis::Z], radial: Some(Radial { axis: Axis::Y, count: 6 }), about: About::Pivot, posable: false, frame: None };
        let i = s.instances(Vec3::ZERO);
        assert_eq!(i.len(), 24);
        let p = Vec3::new(1.0, 0.0, 0.0);
        let q = i[1].point(p);
        assert!((q.x.atan2(-q.z).to_degrees().abs() - 60.0).abs() < 1e-3 || (q.z.atan2(q.x).to_degrees().abs() - 60.0).abs() < 1e-3, "{q}");
        // About the world origin: the mirror plane passes through it.
        let o = Symmetry { axes: vec![Axis::X], about: About::Origin, ..Symmetry::default() }.instances(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(o[1].point(Vec3::new(1.5, 0.0, 0.0)), Vec3::new(0.5, 0.0, 0.0));
        assert!(serde_json::from_str::<Symmetry>(r#"{"axes":["w"]}"#).is_err());
    }
}
