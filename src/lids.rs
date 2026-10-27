//! Eyelids that wrap the eye (character follow-ups, feature 2): a lid is a
//! `Form::Lid` blob, a spherical shell round an eyeball that clears the
//! cornea's bulge, a few cells thick, covering the eye down to a rounded
//! margin and blended into the socket and the skin. `create_character`
//! makes them (`proportions.lids`), and `edit_clay` fills one from any Eye
//! object (`{type: "lid", eye: NAME}`), both through `lid_blob`.

use glam::Vec3;
use serde_json::{Value, json};

/// Where an eye sits, in the clay's space: its centre, diameter (the Eye
/// object's scale: its mesh is a ball of diameter 1), its forward (the
/// object's +Z), the head's up, and how far its cornea reaches past the
/// ball (`Eye::cornea_bulge`, in eye widths).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seat {
    pub center: Vec3,
    pub diameter: f32,
    pub forward: Vec3,
    pub up: Vec3,
    pub bulge: f32,
}

/// One lid, as asked: how much of the eye it covers (0 open, 1 closed,
/// seen from the front), from below (`lower`), its margin's tilt
/// (degrees, raising its +x end), and optional sizes (defaults from the
/// eye and the clay's cell: `default_thickness`, `default_clearance`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub cover: f32,
    pub lower: bool,
    pub tilt: f32,
    pub thickness: Option<f32>,
    pub clearance: Option<f32>,
}

/// A lid's thickness: 0.08 of the eye's width, at least three cells
/// (thinner shells break up on the grid).
pub fn default_thickness(diameter: f32, cell: f32) -> f32 {
    (0.08 * diameter).max(3.0 * cell)
}

/// The space between the cornea's crown and the lid's inside: half a cell,
/// at least a hundredth of the eye.
pub fn default_clearance(diameter: f32, cell: f32) -> f32 {
    (0.5 * cell).max(0.01 * diameter)
}

/// The lid's form JSON for `seat`: inner radius the eyeball's radius plus
/// the cornea's bulge plus the clearance (`gap` is those two), so no part
/// of the eye's surface reaches the lid.
pub fn lid_form(seat: &Seat, spec: &Spec, cell: f32) -> Value {
    let d = seat.diameter;
    let thickness = spec.thickness.unwrap_or_else(|| default_thickness(d, cell));
    let gap = seat.bulge * d + spec.clearance.unwrap_or_else(|| default_clearance(d, cell));
    let mut v = json!({
        "type": "lid",
        "center": [seat.center.x, seat.center.y, seat.center.z],
        "radius": 0.5 * d,
        "gap": gap,
        "thickness": thickness,
        "forward": [seat.forward.x, seat.forward.y, seat.forward.z],
        "up": [seat.up.x, seat.up.y, seat.up.z],
        "cover": spec.cover,
        "tilt": spec.tilt,
    });
    if spec.lower {
        v["lower"] = json!(true);
    }
    v
}

/// The blend a lid takes with the skin before it by default: twice its
/// thickness, so the dome over the eye rolls into the brow and the
/// socket's edge as one skin (a thinner blend leaves a crease all round
/// it: a pillow on the face), but at most a fifth of the eye's width (on
/// small eyes, whose lids are three cells thick, more would fill the
/// opening); the colour fades over half the thickness.
pub fn default_blend(thickness: f32, diameter: f32) -> (f32, f32) {
    ((2.0 * thickness).min(0.2 * diameter), 0.5 * thickness)
}

/// A lid's default name from its eye's: `lid.L` / `lid_lower.L` for an
/// eye whose name ends in a side (`.L`, `_R`, ...), else `lid_EYE` /
/// `lid_lower_EYE`.
pub fn default_name(eye: &str, lower: bool) -> String {
    let stem = if lower { "lid_lower" } else { "lid" };
    for side in [".L", ".R", "_L", "_R", ".l", ".r", "_l", "_r"] {
        if eye.ends_with(side) {
            return format!("{stem}{side}");
        }
    }
    format!("{stem}_{eye}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clay::{Blob, Clay, Field, Form};

    fn seat() -> Seat {
        Seat { center: Vec3::ZERO, diameter: 1.0, forward: Vec3::Z, up: Vec3::Y, bulge: 0.042 }
    }

    fn lid(spec: Spec) -> Blob {
        let v = lid_form(&seat(), &spec, 0.02);
        let form: Form = serde_json::from_value(v).unwrap();
        Blob::of(form, crate::clay::Op::Add, [1.0; 3])
    }

    /// The lid's field is an exact distance: its gradient has length 1
    /// near the surface, outside.
    #[test]
    fn lid_field_is_a_distance() {
        let b = lid(Spec { cover: 0.4, lower: false, tilt: 12.0, thickness: Some(0.08), clearance: Some(0.01) });
        let clay = Clay { blobs: vec![b], resolution: 64, ..Default::default() };
        let field = Field::new(&clay, 0.2);
        let all: Vec<u32> = vec![0];
        let mut rng = 12345u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng as f32 / u32::MAX as f32
        };
        let mut worst = 0.0f32;
        for _ in 0..400 {
            let p = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5) * 1.6;
            let d = field.sample(p, &all).0;
            // Outside, near the surface (inside, the shell's middle is a
            // crease of the distance).
            if !(0.005..0.1).contains(&d) {
                continue;
            }
            let e = 1e-3;
            let g = Vec3::new(
                field.sample(p + Vec3::X * e, &all).0 - field.sample(p - Vec3::X * e, &all).0,
                field.sample(p + Vec3::Y * e, &all).0 - field.sample(p - Vec3::Y * e, &all).0,
                field.sample(p + Vec3::Z * e, &all).0 - field.sample(p - Vec3::Z * e, &all).0,
            ) / (2.0 * e);
            worst = worst.max((g.length() - 1.0).abs());
        }
        assert!(worst < 0.02, "gradient length off by {worst}");
    }

    /// Seen from the front, the upper lid's lowest solid point on the
    /// eye's middle line is `cover` of the eye's height down from its top,
    /// for covers from open to nearly closed; the lower lid's highest is
    /// `cover` up from the bottom.
    #[test]
    fn cover_is_the_part_of_the_eye_hidden_from_the_front() {
        for (cover, lower) in [(0.0, false), (0.2, false), (0.45, false), (0.7, false), (0.15, true), (0.4, true)] {
            let b = lid(Spec { cover, lower, tilt: 0.0, thickness: Some(0.08), clearance: Some(0.01) });
            let clay = Clay { blobs: vec![b], resolution: 64, ..Default::default() };
            let field = Field::new(&clay, 0.2);
            let all = vec![0u32];
            // Rays down -z at x = 0: the edge is the last (first) height
            // whose ray meets solid before the eye ball (radius 0.5).
            let hits = |y: f32| {
                for k in 0..400 {
                    let p = Vec3::new(0.0, y, 1.0 - k as f32 * 0.005);
                    if p.length() < 0.5 {
                        return false;
                    }
                    if field.sample(p, &all).0 < 0.0 {
                        return true;
                    }
                }
                false
            };
            // From the lid's side in, the first ray that reaches the ball.
            let ys: Vec<f32> = (0..=1000).map(|i| 0.5 - i as f32 * 0.001).collect();
            let edge = if lower { ys.iter().rev().find(|&&y| !hits(y)).copied() } else { ys.iter().find(|&&y| !hits(y)).copied() };
            let edge = edge.unwrap_or(if lower { 0.5 } else { -0.5 });
            let got = if lower { edge + 0.5 } else { 0.5 - edge };
            assert!((got - cover).abs() < 0.01, "cover {cover} lower {lower}: hides {got}");
        }
    }

    #[test]
    fn names_follow_the_eye() {
        assert_eq!(default_name("Pip_eye.L", false), "lid.L");
        assert_eq!(default_name("Pip_eye.R", true), "lid_lower.R");
        assert_eq!(default_name("Cyclops", false), "lid_Cyclops");
    }
}
