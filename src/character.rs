//! Stylised character bases (docs/QUALITY_PLAN.md Stage 5c): a named,
//! blended blob skeleton generated from proportions alone, so no existing
//! character's design is involved (D23). It is an ordinary clay: the same
//! blobs an agent could send to `create_object`, with eye sockets carved
//! for stylised eye objects, bones inside the blobs, and proportion guides
//! (Stage 5b) set to what was asked, which `guides measure` reads back.
//!
//! Everything is measured in head units: `u = height / head_units` is the
//! head's height. The soles stand on y = 0 and the crown is the top of the
//! model at `height`: nothing (ears, a tail) reaches above it, and blends
//! are kept away from the crown and the soles, so the head's own extent
//! over the model's is exactly `1 / head_units`. The character faces +z;
//! its left (`.L`) side is +x.
//!
//! `generate` is pure and runs in a fixed order: the same arguments give
//! the same blobs, bones, eyes and guides, bit for bit.

use glam::{EulerRot, Mat3, Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clay::{Clay, Field};
use crate::guides::{End, Guides, HeadUnits, Line, Target};
use crate::rig::{Bone, mirror_name};

/// Bounds of the arguments (the plan's ranges).
pub const HEIGHT: (f32, f32) = (0.1, 10.0);
pub const HEAD_UNITS: (f32, f32) = (2.0, 5.0);
pub const HEAD_WIDTH: (f32, f32) = (0.5, 1.5);
pub const LIMB_LENGTH: (f32, f32) = (0.6, 1.4);
pub const LIMB_THICKNESS: (f32, f32) = (0.6, 1.6);
pub const EYE_SIZE: (f32, f32) = (0.5, 2.0);
pub const EYE_SPACING: (f32, f32) = (0.5, 1.5);
/// Grid cells across the clay chosen for the base (a `resolution`
/// argument may say otherwise within the clay's own bounds).
const RESOLUTION: (u32, u32) = (192, 384);
/// The thinnest round part (fingers, ear tips), in mesh cells of radius.
const MIN_CELLS: f32 = 2.6;
/// The longest name: its eye objects' names (`{name}_eye.L`, which the
/// guides' targets carry) stay within a guide name's length.
pub const MAX_NAME: usize = crate::guides::MAX_NAME - "_eye.L".len();
/// How far a placement may be moved (as guides coordinates).
pub const MAX_TRANSLATE: f32 = 1.0e3;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Build {
    #[default]
    Biped,
    Quadruped,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Body {
    #[default]
    Round,
    Pear,
    Slim,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Hands {
    Mitten,
    #[default]
    FourFinger,
    FiveFinger,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Feet {
    #[default]
    Round,
    Long,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Ears {
    None,
    #[default]
    Round,
    Pointed,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Nose {
    None,
    #[default]
    Button,
}

/// The mouth: a soft smile line carved under the nose (its corners turned
/// up and dimpled), a wider grin, or none.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mouth {
    None,
    #[default]
    Smile,
    Grin,
}

/// Eyelids (`lids.rs`): none (the socket's soft edge only), an upper
/// lid, or upper and lower lids, each a shell wrapping the eyeball.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Lids {
    None,
    #[default]
    Upper,
    Both,
}

/// Bounds of the lids' covers (0 open; together they leave the eye open).
pub const COVER: (f32, f32) = (0.0, 0.9);

/// What the base looks like, every field with a default (the plan's
/// benchmark creature, §5: 2.5 heads, a round body, four-finger hands,
/// round feet and round ears). Resolved values are what the reply shows.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Proportions {
    /// World height, soles to crown.
    pub height: f32,
    /// The model's height in head heights (2-5; need not be whole).
    pub head_units: f32,
    /// The head's width against its default (a little wider than tall).
    pub head_width: f32,
    pub body: Body,
    /// Arms and legs against their default length.
    pub limb_length: f32,
    /// Arms, legs and fingers against their default thickness.
    pub limb_thickness: f32,
    /// Bipeds only.
    pub hands: Hands,
    pub feet: Feet,
    /// The eyes' diameter against the default (0.26 of the head's height;
    /// a head narrower than the default has smaller eyes).
    pub eye_size: f32,
    /// The gap between the eyes against the default (0.4 of an eye).
    pub eye_spacing: f32,
    pub ears: Ears,
    pub nose: Nose,
    pub mouth: Mouth,
    /// Eyelids over the eyes (with `eyes`): upper (default), both or none.
    pub lids: Lids,
    /// How much of the eye's height the upper lid covers, seen from the
    /// front (0 open .. 0.9; default 0.3).
    pub upper_cover: f32,
    /// How much the lower lid covers from below (`lids: both`; default
    /// 0.15).
    pub lower_cover: f32,
}

impl Default for Proportions {
    fn default() -> Self {
        Proportions { height: 1.0, head_units: 2.5, head_width: 1.0, body: Body::Round, limb_length: 1.0, limb_thickness: 1.0, hands: Hands::FourFinger, feet: Feet::Round, eye_size: 1.0, eye_spacing: 1.0, ears: Ears::Round, nose: Nose::Button, mouth: Mouth::Smile, lids: Lids::Upper, upper_cover: 0.3, lower_cover: 0.15 }
    }
}

/// Everything `generate` reads.
#[derive(Clone, PartialEq, Debug)]
pub struct Spec {
    /// The object's name: eye objects are `{name}_eye.L` and `.R`, and the
    /// guides' targets name them.
    pub name: String,
    pub build: Build,
    pub proportions: Proportions,
    /// One blob with `mirror` per pair of sides (true), or both sides
    /// written out (false), to sculpt each on its own.
    pub symmetry: bool,
    pub eyes: bool,
    /// sRGB 0..1.
    pub color: [f32; 3],
    /// Where the clay object stands (its translation; guides are world).
    pub translate: Vec3,
    /// Cells across the clay; None chooses from the thinnest part.
    pub resolution: Option<u32>,
    /// How far the eyes' corneas reach past the ball, in eye widths
    /// (`Eye::cornea_bulge` of the eyes that will sit there): the lids
    /// clear it.
    pub eye_bulge: f32,
}

impl Spec {
    /// Every range the plan gives, and combinations that cannot be built
    /// (eyes wider than the head), refused with what fits.
    pub fn validate(&self) -> Result<(), String> {
        let p = &self.proportions;
        let range = |what: &str, v: f32, (lo, hi): (f32, f32)| {
            if v.is_finite() && (lo..=hi).contains(&v) { Ok(()) } else { Err(format!("proportions.{what} must be from {lo} to {hi} (got {v})")) }
        };
        range("height", p.height, HEIGHT)?;
        range("head_units", p.head_units, HEAD_UNITS)?;
        range("head_width", p.head_width, HEAD_WIDTH)?;
        range("limb_length", p.limb_length, LIMB_LENGTH)?;
        range("limb_thickness", p.limb_thickness, LIMB_THICKNESS)?;
        range("eye_size", p.eye_size, EYE_SIZE)?;
        range("eye_spacing", p.eye_spacing, EYE_SPACING)?;
        range("upper_cover", p.upper_cover, COVER)?;
        range("lower_cover", p.lower_cover, COVER)?;
        if p.lids == Lids::Both && p.upper_cover + p.lower_cover > COVER.1 {
            return Err(format!("proportions.upper_cover and lower_cover together must leave the eye open (at most {} together; got {})", COVER.1, p.upper_cover + p.lower_cover));
        }
        if !(self.eye_bulge.is_finite() && (0.0..=0.25).contains(&self.eye_bulge)) {
            return Err("the eyes' cornea bulge must be 0 to 0.25 eye widths".into());
        }
        if self.color.iter().any(|c| !c.is_finite() || !(0.0..=1.0).contains(c)) {
            return Err("color must be [r, g, b], each 0 to 1".into());
        }
        if !self.translate.is_finite() || self.translate.abs().max_element() > MAX_TRANSLATE {
            return Err(format!("translate must be finite and within {MAX_TRANSLATE}"));
        }
        if let Some(r) = self.resolution
            && !(crate::clay::MIN_RESOLUTION..=crate::clay::MAX_RESOLUTION).contains(&r)
        {
            return Err(format!("resolution must be from {} to {}", crate::clay::MIN_RESOLUTION, crate::clay::MAX_RESOLUTION));
        }
        // The eyes are `{name}_eye.L` and the guides' targets name them:
        // those must be guide names too.
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_NAME || self.name.chars().any(char::is_control) {
            return Err(format!("name must be 1 to {MAX_NAME} characters, not blank and without control characters (its eyes are NAME_eye.L and NAME_eye.R)"));
        }
        let face = Face::new(p, p.height / p.head_units, 0.0);
        let fits = face.sx + 0.5 * face.de <= 0.9 * face.radii.x;
        if !fits {
            return Err(format!(
                "eyes of eye_size {} at eye_spacing {} reach past the head (they need {:.2} of its half width, at most 0.9 fits): lower eye_size or eye_spacing, or raise head_width",
                p.eye_size,
                p.eye_spacing,
                (face.sx + 0.5 * face.de) / face.radii.x
            ));
        }
        Ok(())
    }
}

/// An eye object to place: its name, centre in the clay's space and
/// diameter (the eye object's scale).
#[derive(Clone, PartialEq, Debug)]
pub struct EyeSpec {
    pub name: String,
    pub center: Vec3,
    pub diameter: f32,
}

/// What `generate` makes.
#[derive(Clone, PartialEq, Debug)]
pub struct Built {
    /// Blob JSON, as `create_object` takes it (sweeps say `field:
    /// segments`, so they are exact round tubes on any path).
    pub blobs: Vec<Value>,
    pub resolution: u32,
    pub bones: Vec<Bone>,
    pub eyes: Vec<EyeSpec>,
    pub guides: Guides,
    /// The head's height (one head unit), in world units.
    pub unit: f32,
    /// The finger (or ear tip) radius used, after the cell floor.
    pub finger_radius: f32,
    /// The skin material's `subsurface_radius` (`look: stylized`): the
    /// preset's scatter scaled to the character's size (`SKIN_RADIUS`).
    pub skin_radius: f32,
}

/// The skin's `subsurface_radius` per unit of height. The film tracer
/// gathers subsurface light from the surface within about twelve scatter
/// distances; at the preset's 1.6 that is 11 cm, which on a 1 m base
/// reaches across the gaps between the arm and the body and between shin
/// and foot and gathers light from the shadowed side facing back (dark
/// red patches). Scaled to the height, the reach stays under those gaps
/// at any size and the shadow edges keep the same softness for the size.
pub const SKIN_RADIUS: f32 = 0.45;

/// The face: the head ellipsoid and where the eyes sit on it.
struct Face {
    center: Vec3,
    radii: Vec3,
    /// Eye diameter, half the eyes' centre distance, the eye line.
    de: f32,
    sx: f32,
    ey: f32,
}

impl Face {
    fn new(p: &Proportions, u: f32, z: f32) -> Face {
        let center = Vec3::new(0.0, p.height - 0.5 * u, z);
        let radii = Vec3::new(0.54 * u * p.head_width, 0.5 * u, 0.48 * u);
        // A narrow head has smaller eyes at the same eye_size.
        let de = EYE * u * p.eye_size * (0.4 + 0.6 * p.head_width).min(1.0);
        // The gap between the eyes is 0.4 of an eye at eye_spacing 1.
        let sx = 0.5 * de * (1.0 + EYE_GAP * p.eye_spacing);
        Face { center, radii, de, sx, ey: center.y - 0.04 * u }
    }

    /// The point on the head's front surface at (x, y).
    fn front(&self, x: f32, y: f32) -> Vec3 {
        let q = Vec3::new(x / self.radii.x, (y - self.center.y) / self.radii.y, 0.0);
        let z = self.radii.z * (1.0 - q.x * q.x - q.y * q.y).max(0.0).sqrt();
        Vec3::new(x, y, self.center.z + z)
    }

    /// The left eye's centre: sunk into the head by 0.3 of its width.
    fn eye(&self) -> Vec3 {
        self.front(self.sx, self.ey) - Vec3::Z * 0.3 * self.de
    }
}

/// Blob JSON helpers. `side` blobs are the .L side, mirrored by `emit`.
fn v3(p: Vec3) -> Value {
    json!([p.x, p.y, p.z])
}

struct Out {
    blobs: Vec<Value>,
    symmetry: bool,
    color: [f32; 3],
}

impl Out {
    fn push(&mut self, mut blob: Value, name: &str, blend: f32, bone: &str) {
        blob["name"] = json!(name);
        blob["blend"] = json!(blend);
        if !bone.is_empty() {
            blob["bone"] = json!(bone);
        }
        if blob.get("color").is_none() {
            blob["color"] = json!(self.color);
        }
        self.blobs.push(blob);
    }

    /// A left-side blob (name and bone end in .L): one mirrored blob, or
    /// its right copy first and then itself, the order `Field::new` uses
    /// for a mirrored blob, so both write the same field.
    fn side(&mut self, blob: Value, name: &str, blend: f32, bone: &str) {
        if self.symmetry {
            let mut b = blob;
            b["mirror"] = json!(true);
            self.push(b, name, blend, bone);
        } else {
            let right = mirrored(&blob);
            let rn = mirror_name(name).unwrap_or_else(|| name.to_string());
            let rb = mirror_name(bone).unwrap_or_else(|| bone.to_string());
            self.push(right, &rn, blend, &rb);
            self.push(blob, name, blend, bone);
        }
    }
}

/// A blob's copy across x = 0: points' x negated, rotations about y and z
/// negated (the mirror of an XYZ Euler turn).
fn mirrored(blob: &Value) -> Value {
    let mut b = blob.clone();
    let flip = |p: &mut Value| {
        if let Some(x) = p.get(0).and_then(Value::as_f64) {
            p[0] = json!(-x as f32);
        }
    };
    for k in ["center", "a", "b", "forward", "up"] {
        if let Some(p) = b.get_mut(k) {
            flip(p);
        }
    }
    if let Some(Value::Array(points)) = b.get_mut("points") {
        points.iter_mut().for_each(flip);
    }
    // A lid's tilt raises its +x end: the copy raises its own outer end.
    if let Some(t) = b.get("tilt").and_then(Value::as_f64) {
        b["tilt"] = json!(-t as f32);
    }
    if let Some(r) = b.get_mut("rotate") {
        for i in [1, 2] {
            if let Some(a) = r.get(i).and_then(Value::as_f64) {
                r[i] = json!(-a as f32);
            }
        }
    }
    b
}

fn ellipsoid(center: Vec3, radii: Vec3) -> Value {
    json!({ "type": "ellipsoid", "center": v3(center), "radii": v3(radii) })
}

/// An ellipsoid whose local x, y, z axes lie along `x`, `y`, `z` (an
/// orthonormal right-handed frame).
fn ellipsoid_in(center: Vec3, radii: Vec3, x: Vec3, y: Vec3, z: Vec3) -> Value {
    let q = Quat::from_mat3(&Mat3::from_cols(x, y, z));
    let (rz, ry, rx) = q.to_euler(EulerRot::ZYX);
    let deg = |r: f32| r.to_degrees();
    json!({ "type": "ellipsoid", "center": v3(center), "radii": v3(radii), "rotate": [deg(rx), deg(ry), deg(rz)] })
}

fn capsule(a: Vec3, b: Vec3, ra: f32, rb: f32) -> Value {
    json!({ "type": "capsule", "a": v3(a), "b": v3(b), "radius": ra, "radius_b": rb })
}

/// An exact round tube (Stage 1b) through `points` with a radius each.
fn tube(points: &[Vec3], radii: &[f32]) -> Value {
    json!({ "type": "sweep", "profile": { "circle": radii[0] }, "field": "segments", "points": points.iter().map(|p| v3(*p)).collect::<Vec<_>>(), "radii": radii })
}

/// A bone and, for a .L bone, its .R copy (x negated).
fn bones_side(out: &mut Vec<Bone>, name: &str, parent: Option<&str>, head: Vec3, tail: Vec3) {
    out.push(Bone { name: name.into(), parent: parent.map(String::from), head, tail, deform: true });
    if let Some(other) = mirror_name(name) {
        let flip = |p: Vec3| Vec3::new(-p.x, p.y, p.z);
        let parent = parent.map(|p| mirror_name(p).unwrap_or_else(|| p.to_string()));
        out.push(Bone { name: other, parent, head: flip(head), tail: flip(tail), deform: true });
    }
}

fn bone(out: &mut Vec<Bone>, name: &str, parent: Option<&str>, head: Vec3, tail: Vec3) {
    out.push(Bone { name: name.into(), parent: parent.map(String::from), head, tail, deform: true });
}

/// The default eye diameter in head heights, and the default gap between
/// the eyes in eye widths.
pub const EYE: f32 = 0.26;
pub const EYE_GAP: f32 = 0.4;

/// The base: blobs, bones, eyes and guides for `spec` (validated first).
pub fn generate(spec: &Spec) -> Result<Built, String> {
    spec.validate()?;
    // The finger radius sets the resolution (or is raised to what the
    // resolution can mesh): a first pass finds the clay's extent.
    let draft = build(spec, None, None)?;
    let clay = clay_of(&draft.blobs, RESOLUTION.0)?;
    let extent = Field::new(&clay, 0.0).bounds().map(|(lo, hi)| (hi - lo).max_element()).ok_or("the base came out empty")?;
    let resolution = spec.resolution.unwrap_or_else(|| ((extent * MIN_CELLS / draft.finger_radius).ceil() as u32).clamp(RESOLUTION.0, RESOLUTION.1));
    let cell = extent / resolution as f32;
    let floor = MIN_CELLS * cell;
    // Again with the cell known: the lids are at least three cells thick
    // (`lids::default_thickness`), and thin fingers are thickened.
    let mut built = build(spec, (draft.finger_radius < floor).then_some(floor), Some(cell))?;
    built.resolution = resolution;
    built.skin_radius = (SKIN_RADIUS * spec.proportions.height).clamp(crate::material::SUBSURFACE_RADIUS.0, crate::material::SUBSURFACE_RADIUS.1);
    // The guides go into the scene as they are: they must be guides
    // `guides set` takes and a saved file keeps.
    built.guides.check().map_err(|e| format!("the base's guides: {e}"))?;
    Ok(built)
}

/// The blobs as a clay (`field: segments` is in the JSON already).
pub fn clay_of(blobs: &[Value], resolution: u32) -> Result<Clay, String> {
    serde_json::from_value(json!({ "resolution": resolution, "blobs": blobs })).map_err(|e| format!("character blobs: {e}"))
}

fn build(spec: &Spec, finger_floor: Option<f32>, cell: Option<f32>) -> Result<Built, String> {
    let p = &spec.proportions;
    let u = p.height / p.head_units;
    let face = Face::new(p, u, if spec.build == Build::Quadruped { quadruped_head_z(p, u) } else { 0.0 });
    let mut out = Out { blobs: Vec::new(), symmetry: spec.symmetry, color: spec.color };
    let mut bones = Vec::new();
    let finger = match spec.build {
        Build::Biped => biped(spec, u, &face, finger_floor, &mut out, &mut bones),
        Build::Quadruped => quadruped(spec, u, &face, finger_floor, &mut out, &mut bones),
    };
    head(spec, u, &face, finger_floor, &mut out);
    let eyes = if spec.eyes {
        let c = face.eye();
        sockets(u, &face, &mut out);
        lids(spec, &face, cell, &mut out);
        let flip = Vec3::new(-c.x, c.y, c.z);
        vec![
            EyeSpec { name: format!("{}_eye.L", spec.name), center: c, diameter: face.de },
            EyeSpec { name: format!("{}_eye.R", spec.name), center: flip, diameter: face.de },
        ]
    } else {
        Vec::new()
    };
    let guides = guides_for(spec, u, &face, &eyes);
    Ok(Built { blobs: out.blobs, resolution: 0, bones, eyes, guides, unit: u, finger_radius: finger, skin_radius: 0.0 })
}

/// The head, ears and nose (after the body, so the head blends onto it).
fn head(spec: &Spec, u: f32, face: &Face, floor: Option<f32>, out: &mut Out) {
    let p = &spec.proportions;
    out.push(ellipsoid(face.center, face.radii), "head", 0.16 * u, "head");
    let (c, r) = (face.center, face.radii);
    match p.ears {
        Ears::None => {}
        Ears::Round => {
            // A disc facing forward on the top of the head's side, tilted
            // out; its top stays a fifth of a head under the crown.
            let a = 50f32.to_radians();
            let at = c + Vec3::new(r.x * a.sin(), r.y * a.cos(), -0.08 * u) * 0.92;
            let tilt = -(38f32.to_radians());
            let (x, y) = (Vec3::new(tilt.cos(), tilt.sin(), 0.0), Vec3::new(-tilt.sin(), tilt.cos(), 0.0));
            out.side(ellipsoid_in(at, Vec3::new(0.14 * u, 0.15 * u, 0.055 * u), x, y, Vec3::Z), "ear.L", 0.05 * u, "head");
        }
        Ears::Pointed => {
            let a = 58f32.to_radians();
            let base = c + Vec3::new(r.x * a.sin(), r.y * a.cos(), -0.06 * u) * 0.75;
            let d = 62f32.to_radians();
            let tip = base + Vec3::new(d.sin(), d.cos(), -0.05) * 0.34 * u;
            let tip_r = (0.02 * u).max(floor.unwrap_or(0.0));
            out.side(capsule(base, tip, 0.1 * u, tip_r), "ear.L", 0.05 * u, "head");
        }
    }
    if p.nose == Nose::Button {
        // A round button a little wider than tall, set into the face and
        // blended wide, its tip blushed toward a warm pink: part of the
        // face, not a bead on it.
        let n = face.front(0.0, face.ey - 0.17 * u) - Vec3::Z * 0.035 * u;
        let mut b = ellipsoid(n, Vec3::new(0.1, 0.075, 0.085) * u);
        b["color"] = json!(blush(spec.color, 0.55));
        b["color_blend"] = json!(0.05 * u);
        out.push(b, "nose", 0.07 * u, "head");
    }
    mouth(p.mouth, u, spec.color, face, out);
}

/// Sockets that seat the eyes: a cone opening forward from the eye's
/// centre, `SEAT` degrees round from its axis, carved out of the face (a
/// round cone between two balls inscribed in it, both centred on the
/// axis, so its side is exactly that cone where it meets the eye and the
/// face). Its wall runs along the eye's radius, so it meets the eyeball
/// square-on: nowhere does skin overhang the eye with a gap under it, at
/// any mesh resolution, and wherever the skin ends the eyeball is there.
/// The blend rolls the socket's edge into the face, a soft lid; the back
/// ball is inside the eye, hidden.
fn sockets(u: f32, face: &Face, out: &mut Out) {
    let c = face.eye();
    let r = 0.5 * face.de;
    let s = SEAT.to_radians().sin();
    // Inscribed balls at 0.3 and 3 eye radii along the axis: the cone's
    // side runs from 0.14 to 1.41 eye radii out, past the eye (1) and
    // the face (about 1.3).
    let (near, far) = (0.3 * r, 3.0 * r);
    let mut b = capsule(c + Vec3::Z * near, c + Vec3::Z * far, near * s, far * s);
    b["op"] = json!("carve");
    out.side(b, "eye_socket.L", 0.04 * u, "head");
}

/// How far round the eyeball, from its axis, the socket shows it.
const SEAT: f32 = 62.0;

/// The eyelids (after the sockets, so nothing carves them): shells round
/// the left eye that clear its cornea, covering `upper_cover` of it from
/// the top (and `lower_cover` from the bottom for `lids: both`), in the
/// skin's colour, blended into the socket and the brow, on the head bone;
/// one mirrored blob per lid (`lid.L`, `lid_lower.L`), so a pose or
/// `edit_clay split` can close one eye more than the other.
fn lids(spec: &Spec, face: &Face, cell: Option<f32>, out: &mut Out) {
    let p = &spec.proportions;
    if p.lids == Lids::None {
        return;
    }
    let seat = crate::lids::Seat { center: face.eye(), diameter: face.de, forward: Vec3::Z, up: Vec3::Y, bulge: spec.eye_bulge };
    // Before the cell is known (the draft), a lid a tenth of the eye.
    let cell = cell.unwrap_or(0.0);
    let mut make = |cover: f32, lower: bool, name: &str| {
        let lid = crate::lids::Spec { cover, lower, tilt: 0.0, thickness: None, clearance: None };
        let form = crate::lids::lid_form(&seat, &lid, cell);
        let t = form["thickness"].as_f64().unwrap_or(0.0) as f32;
        let (blend, color_blend) = crate::lids::default_blend(t, face.de);
        let mut b = form;
        b["color_blend"] = json!(color_blend);
        out.side(b, name, blend, "head");
    };
    make(p.upper_cover, false, "lid.L");
    if p.lids == Lids::Both {
        make(p.lower_cover, true, "lid_lower.L");
    }
}

/// `color` moved `k` of the way toward a warm pink.
fn blush(color: [f32; 3], k: f32) -> [f32; 3] {
    let pink = [0.92, 0.45, 0.42];
    [0, 1, 2].map(|i| color[i] + (pink[i] - color[i]) * k)
}

/// The mouth: a round-tube groove along a smile curve on the face under
/// the nose, its corners turned up and narrowing to soft dimples, carved
/// with a wide blend so the line is soft. A thin tube of a dark warm
/// colour lies just under the groove's floor, blended: the blend shades
/// the floor toward its colour smoothly (raising it under a millimetre),
/// so the line reads from any side without a hard colour edge, which a
/// mesh cell would step.
fn mouth(mouth: Mouth, u: f32, color: [f32; 3], face: &Face, out: &mut Out) {
    let (half, lift, y) = match mouth {
        Mouth::None => return,
        Mouth::Smile => (0.15 * u, 0.06 * u, face.ey - 0.32 * u),
        Mouth::Grin => (0.21 * u, 0.09 * u, face.ey - 0.335 * u),
    };
    let r = MOUTH_R * u;
    let n = 7;
    let (mut groove, mut line, mut radii, mut line_radii) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        let t = 2.0 * i as f32 / (n - 1) as f32 - 1.0;
        let s = face.front(t * half, y + lift * t * t);
        // The head's normal there: the groove's centre sits a little out
        // of the surface, the line's under the groove's floor.
        let normal = ((s - face.center) / (face.radii * face.radii)).normalize();
        let k = 1.0 - 0.3 * t * t;
        let (rg, rl) = (r * k, 0.45 * r * k);
        let centre = s + normal * (0.15 * rg);
        groove.push(centre);
        radii.push(rg);
        line.push(centre - normal * (rg + rl + 0.15 * rg));
        line_radii.push(rl);
    }
    let path = |p: &[Vec3]| p.iter().map(|q| v3(*q)).collect::<Vec<_>>();
    out.push(json!({ "type": "sweep", "op": "carve", "profile": { "circle": radii[0] }, "field": "segments", "points": path(&groove), "radii": radii }), "mouth", 0.02 * u, "head");
    let _ = color;
    out.push(json!({ "type": "sweep", "profile": { "circle": line_radii[0] }, "field": "segments", "points": path(&line), "radii": line_radii, "color": MOUTH_COLOR }), "mouth_line", 0.5 * r, "head");
}

/// The mouth line's colour (the blend shows a third of it on the floor).
const MOUTH_COLOR: [f32; 3] = [0.3, 0.07, 0.07];

/// The smile groove's radius in head units.
const MOUTH_R: f32 = 0.03;

/// Body widths (half width and half depth of the chest and pelvis, in
/// head units at 2.5 heads; taller characters are broader per head).
fn widths(body: Body, n: f32) -> ([f32; 2], [f32; 2]) {
    let ((cw, cd), (pw, pd)) = match body {
        Body::Round => ((0.46, 0.40), (0.52, 0.44)),
        Body::Pear => ((0.36, 0.31), (0.55, 0.45)),
        Body::Slim => ((0.35, 0.27), (0.32, 0.25)),
    };
    let s = 1.0 + 0.25 * (n - 2.5);
    ([cw * s, cd * s], [pw * s, pd * s])
}

/// A hand at the wrist `w`, pointing along `along`, palm facing `-t`
/// (toward the body), fingers spread across `wide`. Returns the finger radius.
#[allow(clippy::too_many_arguments)]
fn hand(out: &mut Out, hands: Hands, u: f32, lt: f32, floor: Option<f32>, w: Vec3, along: Vec3, wide: Vec3) -> f32 {
    let t = along.cross(wide);
    let hs = hand_scale(lt);
    let mitten = hands == Hands::Mitten;
    let palm_len = if mitten { 0.17 } else { 0.12 } * u * hs;
    let palm = w + along * (palm_len * 0.8);
    // A round, padded palm.
    out.side(ellipsoid_in(palm, Vec3::new(0.085 * u * hs, palm_len, 0.12 * u * hs), t, along, wide), "hand.L", 0.05 * u, "hand.L");
    let fingers = match hands {
        Hands::Mitten => 0,
        Hands::FourFinger => 3,
        Hands::FiveFinger => 4,
    };
    // Plump fingers with round tips.
    let mut rf = if fingers == 4 { 0.036 } else { 0.044 } * u * lt.sqrt();
    if let Some(f) = floor {
        rf = rf.max(f);
    }
    let spacing = if fingers == 4 { 0.06 } else { 0.075 } * u * hs;
    for i in 0..fingers {
        // Spread across the palm and fanned 12 degrees apart.
        let k = i as f32 - 0.5 * (fingers - 1) as f32;
        let at = palm + along * (palm_len * 0.55) + wide * (k * spacing);
        let fan = (k * 12f32).to_radians();
        let dir = (along * fan.cos() + wide * fan.sin()).normalize();
        let len = (0.15 - 0.02 * k.abs()) * u * hs;
        let p1 = at + dir * (0.55 * len) + t * (0.01 * u);
        let p2 = at + dir * len + t * (0.035 * u);
        out.side(tube(&[at, p1, p2], &[rf, rf * 0.98, rf * 0.94]), &format!("finger{}.L", i + 1), 0.025 * u, "hand.L");
    }
    // The thumb, from the palm's front edge near the wrist.
    let base = palm + wide * (0.07 * u * hs) - along * (0.03 * u);
    let dir = (along * 0.55 + wide * 0.75 + t * 0.35).normalize();
    let len = 0.12 * u * hs;
    let rt = rf * 1.15;
    out.side(tube(&[base, base + dir * (0.55 * len) + t * (0.01 * u), base + dir * len + t * (0.025 * u)], &[rt, rt * 0.98, rt * 0.94]), "thumb.L", 0.03 * u, "hand.L");
    if mitten { rt } else { rf }
}

/// How big hands are against the head (thicker limbs, bigger hands).
fn hand_scale(lt: f32) -> f32 {
    1.2 * (0.85 + 0.15 * lt)
}

/// A foot on the ground under `ankle_x`, toes toward +z.
fn foot(out: &mut Out, feet: Feet, u: f32, x: f32, z: f32, name: &str, bone: &str) -> (Vec3, Vec3) {
    let fh = 0.2 * u;
    let (radii, forward) = match feet {
        Feet::Round => (Vec3::new(0.2 * u, 0.5 * fh, 0.26 * u), 0.08 * u),
        Feet::Long => (Vec3::new(0.17 * u, 0.5 * fh, 0.36 * u), 0.15 * u),
    };
    let c = Vec3::new(x, 0.5 * fh, z + forward);
    out.side(ellipsoid(c, radii), name, 0.05 * u, bone);
    (c, radii)
}

/// A biped: pelvis, torso, neck, arms with hands, legs with feet.
fn biped(spec: &Spec, u: f32, face: &Face, floor: Option<f32>, out: &mut Out, bones: &mut Vec<Bone>) -> f32 {
    let p = &spec.proportions;
    let n = p.head_units;
    let (lt, ll) = (p.limb_thickness, p.limb_length);
    let b = p.height - u;
    let (chest, pelvis) = widths(p.body, n);
    let (r_thigh, r_knee, r_ankle) = (0.17 * u * lt, 0.14 * u * lt, 0.115 * u * lt);
    // The ankle sits a tenth of a head over the sole, under its own radius,
    // so the leg's blend never reaches the sole.
    let y_ankle = r_ankle + 0.1 * u;
    let leg = ((0.30 + 0.16 * (n - 2.0) / 3.0) * b * ll).min(0.6 * b);
    let y_hip = leg.max(y_ankle + 0.25 * u);
    let y_bot = y_hip - 0.12 * u;
    let y_top = b + 0.1 * u;
    let t = y_top - y_bot;
    let foot_rx = if p.feet == Feet::Round { 0.2 * u } else { 0.17 * u };
    let hx = (pelvis[0] * u * 0.55).max(foot_rx * 1.15);
    // Pelvis, then the torso (chest and belly) blended onto it.
    let pc = Vec3::new(0.0, y_bot + 0.3 * t, 0.0);
    out.push(ellipsoid(pc, Vec3::new(pelvis[0] * u, 0.32 * t, pelvis[1] * u)), "pelvis", 0.0, "hips");
    let cc = Vec3::new(0.0, y_bot + 0.66 * t, 0.0);
    out.push(ellipsoid(cc, Vec3::new(chest[0] * u, 0.36 * t, chest[1] * u)), "torso", 0.3 * t.min(chest[0] * u), "spine");
    let neck_a = Vec3::new(0.0, b - 0.1 * u, -0.02 * u);
    let neck_b = Vec3::new(0.0, b + 0.25 * u, 0.0);
    out.push(capsule(neck_a, neck_b, 0.2 * u, 0.2 * u), "neck", 0.1 * u, "head");
    // Legs: thigh and shin, then the foot.
    let hip = Vec3::new(hx, y_hip, 0.0);
    let knee = Vec3::new(hx, 0.5 * (y_hip + y_ankle), 0.02 * u);
    let ankle = Vec3::new(hx, y_ankle, 0.0);
    out.side(capsule(hip, knee, r_thigh, r_knee), "thigh.L", 0.08 * u, "thigh.L");
    out.side(capsule(knee, ankle, r_knee, r_ankle), "shin.L", 0.05 * u, "shin.L");
    let (fc, fr) = foot(out, p.feet, u, hx, 0.0, "foot.L", "foot.L");
    // Arms: down and out at 40 degrees (A pose), the forearm a little
    // forward.
    let y_shoulder = y_bot + 0.86 * t;
    let shoulder = Vec3::new(chest[0] * u * 0.82, y_shoulder, 0.0);
    let a1 = 40f32.to_radians();
    let d1 = Vec3::new(a1.sin(), -a1.cos(), 0.0);
    let a2 = 32f32.to_radians();
    let d2 = Vec3::new(a2.sin(), -a2.cos(), 0.22).normalize();
    // Long arms on short bodies stop with the fingertips a twentieth of a
    // head over the ground.
    let hs = hand_scale(lt);
    let hand_drop = (0.33 * u * hs + 0.05 * u * lt) * -d2.y;
    let most = (y_shoulder - hand_drop - 0.05 * u) / (0.5 * -d1.y + 0.45 * -d2.y);
    let reach = (ll * ((y_shoulder - y_hip) + 0.15 * u).max(0.7 * u)).min(most);
    let elbow = shoulder + d1 * (0.5 * reach);
    let wrist = elbow + d2 * (0.45 * reach);
    // Stylised volume: a full upper arm narrowing to the elbow, a forearm
    // swelling just past it and tapering to a slim wrist (exact round
    // tubes through straight paths, so the taper is smooth).
    let (r_sh, r_up, r_el, r_fo, r_wr) = (0.165 * u * lt, 0.16 * u * lt, 0.12 * u * lt, 0.14 * u * lt, 0.1 * u * lt);
    out.side(tube(&[shoulder, shoulder + (elbow - shoulder) * 0.4, elbow], &[r_sh, r_up, r_el]), "upper_arm.L", 0.08 * u, "upper_arm.L");
    out.side(tube(&[elbow, elbow + (wrist - elbow) * 0.3, wrist], &[r_el, r_fo, r_wr]), "forearm.L", 0.04 * u, "forearm.L");
    let wide = (Vec3::Z - d2 * d2.z).normalize();
    let finger = hand(out, p.hands, u, lt, floor, wrist, d2, wide);
    // Bones inside the blobs.
    bone(bones, "hips", None, Vec3::new(0.0, y_hip, 0.0), Vec3::new(0.0, pc.y + 0.15 * t, 0.0));
    bone(bones, "spine", Some("hips"), Vec3::new(0.0, pc.y + 0.15 * t, 0.0), Vec3::new(0.0, b - 0.05 * u, 0.0));
    // The neck is buried between torso and head: a helper that turns the
    // head, moving no skin of its own.
    bone(bones, "neck", Some("spine"), Vec3::new(0.0, b - 0.05 * u, 0.0), Vec3::new(0.0, face.center.y - 0.3 * u, 0.0));
    bones.last_mut().expect("just pushed").deform = false;
    bone(bones, "head", Some("neck"), Vec3::new(0.0, face.center.y - 0.3 * u, 0.0), Vec3::new(0.0, p.height - 0.08 * u, 0.0));
    bones_side(bones, "upper_arm.L", Some("spine"), shoulder, elbow);
    bones_side(bones, "forearm.L", Some("upper_arm.L"), elbow, wrist);
    bones_side(bones, "hand.L", Some("forearm.L"), wrist, wrist + d2 * (0.25 * u));
    bones_side(bones, "thigh.L", Some("hips"), hip, knee);
    bones_side(bones, "shin.L", Some("thigh.L"), knee, ankle);
    bones_side(bones, "foot.L", Some("shin.L"), ankle, Vec3::new(fc.x, fc.y, fc.z + 0.7 * fr.z));
    finger
}

/// A quadruped: a level body on four legs, a neck up to the head at the
/// front, and a tail (an exact round tube) at the back.
fn quadruped(spec: &Spec, u: f32, face: &Face, floor: Option<f32>, out: &mut Out, bones: &mut Vec<Bone>) -> f32 {
    let p = &spec.proportions;
    let n = p.head_units;
    let (lt, ll) = (p.limb_thickness, p.limb_length);
    let b = p.height - u;
    let (chest, pelvis) = widths(p.body, n);
    let (r_up, r_knee, r_ankle) = (0.14 * u * lt, 0.11 * u * lt, 0.09 * u * lt);
    let y_ankle = r_ankle + 0.1 * u;
    let ry = 0.36 * u * (chest[1] / 0.40).max(0.7);
    // The body's middle: legs below it, the head above and ahead.
    let y_body = (0.5 * b * ll).clamp(y_ankle + 0.3 * u + ry * 0.5, b - 0.1 * u);
    let half = body_half_length(n, u);
    let chest_c = Vec3::new(0.0, y_body + 0.04 * u, 0.45 * half);
    let hip_c = Vec3::new(0.0, y_body, -0.45 * half);
    out.push(ellipsoid(hip_c, Vec3::new(pelvis[0] * u * 0.9, ry, 0.62 * half)), "pelvis", 0.0, "hips");
    out.push(ellipsoid(chest_c, Vec3::new(chest[0] * u, ry * 1.05, 0.62 * half)), "torso", 0.25 * u, "chest");
    // The head sits ahead of the chest, its neck rising to it.
    let head_z = face.center.z;
    let neck_a = chest_c + Vec3::new(0.0, 0.3 * ry, 0.35 * half);
    let neck_b = Vec3::new(0.0, face.center.y - 0.25 * u, head_z - 0.1 * u);
    out.push(capsule(neck_a, neck_b, 0.22 * u, 0.2 * u), "neck", 0.12 * u, "neck");
    // Legs at the body's four corners.
    let x = (chest[0].min(pelvis[0]) * u * 0.6).max(0.2 * u * 1.1);
    let leg = |front: bool, out: &mut Out, bones: &mut Vec<Bone>| {
        let (z, top, pre) = if front { (chest_c.z + 0.25 * half, chest_c.y, "front") } else { (hip_c.z - 0.25 * half, hip_c.y, "hind") };
        let hip = Vec3::new(x, top, z);
        let knee = Vec3::new(x, 0.5 * (top + y_ankle), z + if front { -0.03 } else { 0.03 } * u);
        let ankle = Vec3::new(x, y_ankle, z);
        let (upper, lower, paw) = (format!("{pre}_upper_leg.L"), format!("{pre}_lower_leg.L"), format!("{pre}_paw.L"));
        out.side(capsule(hip, knee, r_up, r_knee), &upper, 0.08 * u, &upper);
        out.side(capsule(knee, ankle, r_knee, r_ankle), &lower, 0.05 * u, &lower);
        let (fc, fr) = foot(out, p.feet, u, x, z, &paw, &paw);
        let parent = if front { "chest" } else { "hips" };
        bones_side(bones, &upper, Some(parent), hip, knee);
        bones_side(bones, &lower, Some(&upper), knee, ankle);
        bones_side(bones, &paw, Some(&lower), ankle, Vec3::new(fc.x, fc.y, fc.z + 0.7 * fr.z));
    };
    bone(bones, "hips", None, hip_c, hip_c + Vec3::Z * (0.4 * half));
    bone(bones, "chest", Some("hips"), hip_c + Vec3::Z * (0.4 * half), chest_c + Vec3::Z * (0.3 * half));
    leg(true, out, bones);
    leg(false, out, bones);
    // A tail up and back, well under the crown.
    let root = hip_c + Vec3::new(0.0, 0.3 * ry, -0.55 * half);
    let tail = [root, root + Vec3::new(0.0, 0.25 * u, -0.25 * u), root + Vec3::new(0.0, 0.45 * u, -0.3 * u)];
    let rt = (0.07 * u * lt).max(floor.unwrap_or(0.0));
    out.push(tube(&tail, &[rt * 1.3, rt, rt * 0.8]), "tail", 0.05 * u, "tail");
    bone(bones, "neck", Some("chest"), neck_a, neck_b);
    bone(bones, "head", Some("neck"), neck_b, Vec3::new(0.0, p.height - 0.08 * u, head_z));
    bone(bones, "tail", Some("hips"), tail[0], tail[2]);
    rt
}

/// Half a quadruped's body length.
fn body_half_length(n: f32, u: f32) -> f32 {
    0.95 * u * (1.0 + 0.15 * (n - 2.5))
}

/// How far ahead of the middle a quadruped's head sits: past the chest.
fn quadruped_head_z(p: &Proportions, u: f32) -> f32 {
    let half = body_half_length(p.head_units, u);
    0.45 * half + 0.55 * half
}

/// The guides the base was made to: head units from the crown down (the
/// last line falls under the soles when head_units is not whole), the eye
/// line and centre line, and targets for the head ratio and the eyes.
fn guides_for(spec: &Spec, u: f32, face: &Face, eyes: &[EyeSpec]) -> Guides {
    let p = &spec.proportions;
    let t = spec.translate;
    let count = (p.head_units - 1e-4).ceil() as u32;
    let top = t.y + p.height;
    let head_units = Some(HeadUnits { count, bottom: top - count as f32 * u, top });
    let mut lines = vec![Line { name: "center".into(), x: Some(t.x), y: None, z: None }];
    let mut targets = vec![Target { name: "head_ratio".into(), of: vec!["head".into()], to: Some(crate::guides::TOTAL_HEIGHT.into()), between: Vec::new(), axis: None, per: None, per_axis: None, value: 1.0 / p.head_units, tolerance: 0.02 / p.head_units }];
    if let [l, r] = eyes {
        lines.push(Line { name: "eye_line".into(), x: None, y: Some(t.y + face.ey), z: None });
        let size = face.de / u;
        targets.push(Target { name: "eye_size".into(), of: vec![l.name.clone()], to: Some("head".into()), between: Vec::new(), axis: None, per: None, per_axis: None, value: size, tolerance: 0.02 * size });
        let spacing = 2.0 * face.sx / u;
        targets.push(Target { name: "eye_spacing".into(), of: Vec::new(), to: None, between: vec![End::Name(r.name.clone()), End::Name(l.name.clone())], axis: Some(crate::guides::Axis::X), per: Some(crate::guides::HEAD_UNIT.into()), per_axis: None, value: spacing, tolerance: 0.02 * spacing });
        targets.push(Target { name: "eyes_on_line".into(), of: Vec::new(), to: None, between: vec![End::Name("eye_line".into()), End::Name(l.name.clone())], axis: None, per: None, per_axis: None, value: 0.0, tolerance: 0.01 * u });
    }
    Guides { head_units, lines, targets }
}

#[cfg(test)]
mod tests;
