//! Clay: soft blobs that melt together into one surface.
//!
//! Each blob is a signed distance field (SDF: a function that gives the
//! distance to the blob's surface, negative inside). Blobs are applied in
//! order: `add` melts a blob onto everything before it and `carve` cuts it
//! away, both with a smooth `blend` radius so joints look like clay rather
//! than stacked primitives. `mirror` also applies the blob mirrored across
//! X = 0, for symmetric characters.
//!
//! `Clay::build_mesh` samples the field on a grid and turns it into triangles
//! with surface nets (one vertex per grid cell the surface crosses, one quad
//! per grid edge it crosses). Normals come from the field's gradient and
//! vertex colors from the blended blob colors. Each vertex also takes the
//! material of the blob that wins there (see `material.rs`).
//!
//! Blob coordinates are in the owning object's own space.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use glam::{EulerRot, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::material::Material;
use crate::mesh::{Mesh, Vertex};
use crate::paint::Paint;

#[path = "membrane.rs"]
mod membrane;
#[path = "hard_surface.rs"]
mod hard_surface;
#[cfg(test)]
pub(crate) use hard_surface::path_turns;

pub const MIN_RESOLUTION: u32 = 16;
pub const MAX_RESOLUTION: u32 = 512;
pub const DEFAULT_RESOLUTION: u32 = 96;
/// More blobs than this are refused (and cut from loaded files).
pub const MAX_BLOBS: usize = 1024;
/// Grid points per brick side. Each brick only evaluates the blobs near it.
const BRICK: usize = 8;
/// One block of the meshing grid: sampled, or all inside / all outside.
#[derive(Clone)]
enum Block {
    Out,
    In,
    Data(Box<[f32]>),
}

/// Offsets of a cube's 8 corners.
const CORNERS: [(usize, usize, usize); 8] = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Clay {
    #[serde(default)]
    pub blobs: Vec<Blob>,
    /// Grid cells across the longest side of the clay's bounds.
    #[serde(default = "default_resolution")]
    pub resolution: u32,
    /// Sculpt-brush offsets on top of the blobs (see `sculpt.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    /// Paint over the blob colors (see `paint.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<Paint>,
    /// Painted mask channels for smart material layers (`smart.rs`),
    /// by name; coverage is the mask.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub masks: std::collections::BTreeMap<String, Paint>,
    /// Decals stamped on the surface (`smart.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decals: Vec<crate::smart::Decal>,
    /// Size of the sculpt and paint grids' cells, finer than the mesh for
    /// pores and wrinkles (None = the mesh cell size at the first stroke).
    /// Film renders and close-up views show it on a fine mesh; bakes put it
    /// in the normal and color maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_cell: Option<f32>,
    /// Hollow the complete CSG inward, before sculpt detail.
    #[serde(default)]
    pub shell: Option<f32>,
    /// Feature-preserving placement; false is useful for comparison renders.
    #[serde(default = "yes")]
    pub sharp: bool,
}

/// Smallest `Clay::detail_cell`.
pub const MIN_DETAIL_CELL: f32 = 1e-4;
/// Most grid cells across a fine mesh's coarse grid (see `Clay::build_fine`).
pub const MAX_FINE_CELLS: u32 = 2400;
/// Most triangles in a fine mesh after refining its detail regions.
pub const MAX_FINE_TRIANGLES: usize = 24_000_000;
/// Most BRICK^3 blocks in a meshing grid's bounding box.
const MAX_FINE_BLOCKS: usize = 4_000_000;

impl Default for Clay {
    fn default() -> Self {
        Self { blobs: Vec::new(), resolution: DEFAULT_RESOLUTION, detail: None, paint: None, masks: Default::default(), decals: Vec::new(), detail_cell: None, shell: None, sharp: true }
    }
}

fn default_resolution() -> u32 {
    DEFAULT_RESOLUTION
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    #[default]
    Add,
    Carve,
    /// Clothing: everything before this blob, grown by `offset`, kept only
    /// inside this blob's shape (a tunic's torso box, a sleeve's capsule).
    /// It fits the body exactly, with a crisp edge and its own color and
    /// material, and moves with the body.
    Cover,
    Plate,
    Engrave,
    Emboss,
}

/// How much thicker than its mesh a human blob is (0.6 mm).
const HUMAN_SKIN: f32 = 6e-4;
/// How far a human body part reaches above the head cut, and how far it
/// sinks under the head there.
const HEAD_OVERLAP: f32 = 0.04;
const HEAD_OVERLAP_INSET: f32 = 1.2e-3;

/// 0 below 0, 1 above 1, smooth between.
fn smooth_ramp(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Which part of a `Form::Human` a blob keeps.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum HumanPart {
    #[default]
    Whole,
    Head,
    Body,
}

/// Blob geometry. JSON uses a `type` tag, e.g.
/// `{"type": "capsule", "a": [0,0,0], "b": [0,1,0], "radius": 0.2}`.
/// `rotate` is in degrees (XYZ order), like object rotation.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Form {
    Extrude {
        #[serde(deserialize_with = "hard_surface::profile")]
        profile: Vec<hard_surface::Corner>,
        depth: f32,
        #[serde(default)] center: Vec3,
        #[serde(default = "z_axis")] axis: Vec3,
        #[serde(default)] bevel: f32,
    },
    Lathe {
        #[serde(deserialize_with = "hard_surface::profile")]
        profile: Vec<hard_surface::Corner>,
        #[serde(default)] center: Vec3,
        #[serde(default = "y_axis")] axis: Vec3,
    },
    Sweep {
        #[serde(deserialize_with = "hard_surface::profile")]
        profile: Vec<hard_surface::Corner>,
        #[serde(deserialize_with = "chain_points")] points: Vec<Vec3>,
        #[serde(default = "one")] taper: f32,
        #[serde(default)] twist: f32,
        #[serde(default = "yes", rename = "path_smooth")] smooth: bool,
    },
    Shell { source: Box<Form>, thickness: f32 },
    Array {
        source: Box<Form>,
        #[serde(deserialize_with = "hard_surface::count")] count: usize,
        #[serde(default)] step: Vec3,
        #[serde(default)] radius: f32,
        #[serde(default)] center: Vec3,
        #[serde(default = "y_axis")] axis: Vec3,
        #[serde(default)] points: Vec<Vec3>,
        /// `path`: copies along `points` turn with the path.
        #[serde(default, skip_serializing_if = "hard_surface::is_fixed")] orient: hard_surface::Orient,
    },
    /// Surface path footprint; op engrave/emboss supplies constant depth.
    Band {
        #[serde(deserialize_with = "chain_points")] points: Vec<Vec3>,
        width: f32,
        #[serde(default = "z_axis")] normal: Vec3,
    },
    /// Connected ellipsoidal beads along an arc-length sampled path.
    Chain {
        #[serde(deserialize_with = "chain_points")]
        points: Vec<Vec3>,
        size: Vec3,
        #[serde(default)]
        twist: f32,
        #[serde(default = "one")]
        taper: f32,
        /// Degrees each bead leans across the path, left and right in turn
        /// (braid strands; 0 = straight beads).
        #[serde(default)]
        tilt: f32,
    },
    /// A polygon sheet with full thickness and optional cupping / edge cut-ins.
    Membrane {
        #[serde(deserialize_with = "membrane_points")]
        points: Vec<Vec3>,
        thickness: f32,
        #[serde(default)]
        sag: f32,
        #[serde(default)]
        scallop: f32,
        #[serde(default)]
        scallop_edges: Vec<usize>,
    },
    Sphere {
        center: Vec3,
        radius: f32,
    },
    Ellipsoid {
        center: Vec3,
        radii: Vec3,
        #[serde(default)]
        rotate: Vec3,
    },
    /// A rounded cone from `a` (radius) to `b` (radius_b, default radius):
    /// limbs, fingers, necks, horns.
    Capsule {
        a: Vec3,
        b: Vec3,
        radius: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius_b: Option<f32>,
    },
    /// `size` is the full width, height, depth. `round` rounds the edges.
    Box {
        center: Vec3,
        size: Vec3,
        #[serde(default)]
        rotate: Vec3,
        #[serde(default)]
        round: f32,
    },
    /// Ring in the XZ plane (before rotation): `radius` to the tube center.
    Torus {
        center: Vec3,
        radius: f32,
        thickness: f32,
        #[serde(default)]
        rotate: Vec3,
    },
    /// A human body from the built-in base (see `human.rs`), in this
    /// object's space with the feet at y = 0. `part` keeps the whole body,
    /// just the head (cut at the neck, for a fine head clay), or the body
    /// without the head.
    Human {
        #[serde(default)]
        human: crate::human::Human,
        #[serde(default)]
        part: HumanPart,
    },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Blob {
    /// Optional label so agents can address blobs ("head", "left_ear").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(flatten)]
    pub form: Form,
    #[serde(default)]
    pub op: Op,
    /// Smooth-blend radius with what came before; 0 is a hard edge.
    #[serde(default = "default_blend")]
    pub blend: f32,
    /// sRGB 0..1.
    #[serde(default = "default_color")]
    pub color: [f32; 3],
    /// Also apply the blob mirrored across X = 0.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// Apply the blob mirrored across X = 0 instead of as written (the
    /// blobs of a mirrored object, see `pose.rs`; with `mirror` both
    /// sides still show).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip: bool,
    /// Material name (scene library or preset); empty = the object's material.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub material: String,
    /// Override the automatic bone assignment for this blob.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bone: String,
    /// `cover` only: how far the garment stands off what is under it.
    #[serde(default = "default_offset", skip_serializing_if = "is_default_offset")]
    pub offset: f32,
    /// Cover smoothing footprint in scene units; loose is an alias.
    #[serde(default, alias = "loose")]
    pub smooth: f32,
    #[serde(default)] pub join: hard_surface::Join,
    #[serde(default)] pub join_size: Option<f32>,
    /// Plate thickness or engraving/embossing depth.
    #[serde(default = "default_offset")] pub wall: f32,
    #[serde(default)] pub rim: f32,
    /// Forged center crest height and outward lip flare, in meters.
    #[serde(default)] pub ridge: f32,
    #[serde(default)] pub flare: f32,
    /// Width of the color fade where this blob meets earlier ones; None =
    /// `blend` (color fades over the whole shape blend), 0 = a hard color edge
    /// (a sleeve end) on a smooth shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_blend: Option<f32>,
}

impl Blob {
    pub(crate) fn validate(&self) -> Result<(), String> {
        hard_surface::validate(&self.form)?;
        if !self.ridge.is_finite() || self.ridge < 0.0 || !self.flare.is_finite() || self.flare < 0.0 { return Err("ridge and flare must be finite and nonnegative".into()); }
        if !self.wall.is_finite() || self.wall <= 0.0 || !self.rim.is_finite() || self.rim < 0.0 || self.join_size.is_some_and(|v| !v.is_finite() || v < 0.0) {
            return Err("wall must be positive; rim and join_size must be nonnegative".into());
        }
        if matches!(self.op, Op::Engrave | Op::Emboss) && !matches!(self.form, Form::Band { .. }) {
            return Err("engrave/emboss require a band form".into());
        }
        Ok(())
    }
}
fn yes() -> bool { true }
fn z_axis() -> Vec3 { Vec3::Z }
fn y_axis() -> Vec3 { Vec3::Y }
fn one() -> f32 { 1.0 }
fn chain_points<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec3>, D::Error> {
    let points = Vec::<Vec3>::deserialize(d)?;
    if !(2..=128).contains(&points.len()) || points.iter().any(|p| !p.is_finite()) {
        return Err(serde::de::Error::custom("chain needs 2-128 finite path points"));
    }
    Ok(points)
}

fn membrane_points<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec3>, D::Error> {
    let points = Vec::<Vec3>::deserialize(d)?;
    if !(3..=20).contains(&points.len()) || points.iter().any(|p| !p.is_finite()) {
        return Err(serde::de::Error::custom("membrane needs 3-20 finite boundary points"));
    }
    Ok(points)
}

fn default_offset() -> f32 {
    0.004
}

fn is_default_offset(v: &f32) -> bool {
    *v == default_offset()
}

fn default_blend() -> f32 {
    0.1
}

fn default_color() -> [f32; 3] {
    [0.8, 0.8, 0.8]
}

pub(crate) fn rotation(deg: Vec3) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.0);
    Quat::from_euler(EulerRot::ZYX, r.z, r.y, r.x)
}

/// A blob ready to evaluate: rotations inverted, bounds known.
struct Prim {
    geo: Geo,
    op: Op,
    k: f32,
    /// Color fade width (`Blob::color_blend`, at most `k`).
    ck: f32,
    /// `Op::Cover`'s stand-off.
    offset: f32,
    smooth: f32,
    join: hard_surface::Join,
    wall: f32,
    rim: f32,
    ridge: f32,
    flare: f32,
    envelope: Option<Arc<hard_surface::Envelope>>,
    name: String,
    color: Vec3,
    /// Index into `Clay::material_names`.
    mat: u16,
    /// Evaluate at (-x, y, z): the mirrored copy.
    flip: bool,
    bone: String,
    lo: Vec3,
    hi: Vec3,
}

enum Geo {
    Hard(hard_surface::Geometry),
    Chain { beads: Vec<Geo>, core: Vec<Geo> },
    Sheet { sdf: crate::human::MeshSdf, half: f32 },
    Sphere { c: Vec3, r: f32 },
    Ellipsoid { c: Vec3, r: Vec3, inv: Quat },
    Cone { a: Vec3, b: Vec3, r1: f32, r2: f32 },
    Box { c: Vec3, half: Vec3, round: f32, inv: Quat },
    Torus { c: Vec3, big: f32, small: f32, inv: Quat },
    /// A mesh's exact distance, kept above `above` and below `below` (y).
    Mesh { sdf: Arc<crate::human::Shape>, above: f32, below: f32 },
}

impl Geo {
    fn new(form: &Form) -> (Geo, Vec3, Vec3) {
        let pos = |v: f32| v.abs().max(1e-4);
        match *form {
            Form::Extrude { .. } | Form::Lathe { .. } | Form::Sweep { .. } | Form::Shell { .. } | Form::Array { .. } | Form::Band { .. } => {
                let g = hard_surface::Geometry::new(form);
                let (lo, hi) = g.bounds();
                (Geo::Hard(g), lo, hi)
            },
            Form::Chain { ref points, size, twist, taper, tilt } => {
                let r = size.abs().max(Vec3::splat(1e-4)) * 0.5;
                let length: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
                let taper = if taper.is_finite() { taper.clamp(0.05, 2.0) } else { 1.0 };
                // Beads 0.7 of their own length apart, so tapered chains stay
                // joined to the thin end; spaced wider past 511 beads.
                let spacing = |t: f32| r.y * 1.4 * (1.0 + (taper - 1.0) * t);
                let natural = {
                    let (mut at, mut n) = (0.0, 0usize);
                    while at < length && n < 100_000 { at += spacing(at / length.max(1e-9)); n += 1; }
                    n
                };
                let stretch = (natural as f32 / 511.0).max(1.0);
                let mut stops = vec![0.0f32];
                while let Some(&at) = stops.last() {
                    let next = at + spacing(at / length.max(1e-9)) * stretch;
                    if next >= length || stops.len() >= 511 { break; }
                    stops.push(next);
                }
                if length > 0.0 { stops.push(length); }
                let twist = if twist.is_finite() { twist.to_radians() } else { 0.0 };
                let (mut beads, mut core) = (Vec::new(), Vec::new());
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                let mut last: Option<(Vec3, f32)> = None;
                for (i, &stop) in stops.iter().enumerate() {
                    let t = if length > 0.0 { stop / length } else { 0.0 };
                    let mut at = stop;
                    let mut a = points[0];
                    let mut dir = Vec3::Y;
                    for (j, w) in points.windows(2).enumerate() {
                        let len = w[0].distance(w[1]);
                        if at <= len || j + 2 == points.len() {
                            dir = (w[1] - w[0]).normalize_or(Vec3::Y);
                            a = w[0] + dir * at.min(len);
                            break;
                        }
                        at -= len;
                    }
                    let radii = r * (1.0 + (taper - 1.0) * t);
                    let lean = if tilt.is_finite() { tilt.clamp(-80.0, 80.0).to_radians() } else { 0.0 };
                    let lean = if i % 2 == 0 { lean } else { -lean };
                    let rotation = Quat::from_rotation_arc(Vec3::Y, dir) * Quat::from_rotation_y(twist * i as f32) * Quat::from_rotation_z(lean);
                    beads.push(Geo::Ellipsoid { c: a, r: radii, inv: rotation.inverse() });
                    let m = radii.max_element();
                    lo = lo.min(a - m); hi = hi.max(a + m);
                    let cr = radii.min_element() * 0.45;
                    if let Some((b, br)) = last { core.push(Geo::Cone { a: b, b: a, r1: br, r2: cr }); }
                    last = Some((a, cr));
                }
                (Geo::Chain { beads, core }, lo, hi)
            }
            Form::Membrane { ref points, thickness, sag, scallop, ref scallop_edges } => {
                let sdf = membrane::sheet(points, if sag.is_finite() { sag } else { 0.0 }, if scallop.is_finite() { scallop.clamp(0.0, 0.5) } else { 0.0 }, scallop_edges);
                let half = pos(thickness) * 0.5;
                let (lo, hi) = (sdf.lo - half, sdf.hi + half);
                (Geo::Sheet { sdf, half }, lo, hi)
            }
            Form::Human { ref human, part } => {
                let Ok(shape) = crate::human::shape(human) else {
                    return (Geo::Sphere { c: Vec3::ZERO, r: 1e-4 }, Vec3::splat(-1e-4), Vec3::splat(1e-4));
                };
                // Head and body meet at a cut across the neck; the body
                // reaches a little past it so the seam is hidden inside.
                let cut = shape.neck_cut;
                let (above, below) = match part {
                    HumanPart::Whole => (f32::MIN, f32::MAX),
                    HumanPart::Head => (cut, f32::MAX),
                    HumanPart::Body => (f32::MIN, cut + HEAD_OVERLAP),
                };
                // The head's box is the head's own (arms reach far wider).
                let (lo, hi) = match part {
                    HumanPart::Head => (shape.head_lo.max(Vec3::new(f32::MIN, above, f32::MIN)), shape.head_hi),
                    _ => (shape.sdf.lo.max(Vec3::new(f32::MIN, above, f32::MIN)), shape.sdf.hi.min(Vec3::new(f32::MAX, below, f32::MAX))),
                };
                (Geo::Mesh { sdf: shape, above, below }, lo, hi)
            }
            Form::Sphere { center, radius } => {
                let r = pos(radius);
                (Geo::Sphere { c: center, r }, center - r, center + r)
            }
            Form::Ellipsoid { center, radii, rotate } => {
                let r = radii.abs().max(Vec3::splat(1e-4));
                let m = r.max_element();
                (Geo::Ellipsoid { c: center, r, inv: rotation(rotate).inverse() }, center - m, center + m)
            }
            Form::Capsule { a, b, radius, radius_b } => {
                let (r1, r2) = (pos(radius), pos(radius_b.unwrap_or(radius)));
                (Geo::Cone { a, b, r1, r2 }, (a - r1).min(b - r2), (a + r1).max(b + r2))
            }
            Form::Box { center, size, rotate, round } => {
                let half = (size.abs() * 0.5).max(Vec3::splat(1e-4));
                let round = round.clamp(0.0, half.min_element());
                let rot=rotation(rotate);
                let extent=(rot*Vec3::X).abs()*half.x+(rot*Vec3::Y).abs()*half.y+(rot*Vec3::Z).abs()*half.z;
                (Geo::Box { c: center, half, round, inv: rot.inverse() }, center - extent, center + extent)
            }
            Form::Torus { center, radius, thickness, rotate } => {
                let (big, small) = (pos(radius), pos(thickness));
                let m = big + small;
                (Geo::Torus { c: center, big, small, inv: rotation(rotate).inverse() }, center - m, center + m)
            }
        }
    }

    fn project(&self,p:Vec3,lo:Vec3,hi:Vec3)->Option<Vec3> {
        match self {
            Geo::Hard(g)=>g.project(p,lo,hi),
            Geo::Box {c,half,round,inv} if *round==0.0=> {
                let rot=inv.inverse();let mut features=hard_surface::FeatureProject::new(p,lo,hi);
                let corners=CORNERS.map(|(x,y,z)|*c+rot*(*half*Vec3::new(if x==0 {-1.0} else {1.0},if y==0 {-1.0} else {1.0},if z==0 {-1.0} else {1.0})));
                for &q in &corners {features.corner(q);}
                for i in 0..8 {for axis in [1,2,4] {if i&axis==0 {features.edge(corners[i],corners[i|axis]);}}}
                if let Some(q)=features.point() {return Some(q);}
                let mut q=*inv*(p-*c);
                if (q.abs()-*half).max_element()>0.0 {q=q.clamp(-*half,*half);}
                else {let d=q.abs()-*half;let axis=if d.x>=d.y && d.x>=d.z {0} else if d.y>=d.z {1} else {2};q[axis]=half[axis].copysign(q[axis]);}
                Some(*c+rot*q)
            },
            _=>None,
        }
    }

    fn sdf(&self, p: Vec3) -> f32 {
        match *self {
            Geo::Hard(ref g) => g.sdf(p),
            Geo::Chain { ref beads, ref core } => beads.iter().chain(core).map(|g| g.sdf(p)).fold(f32::MAX, f32::min),
            Geo::Sheet { ref sdf, half } => sdf.distance(p).abs() - half,
            Geo::Sphere { c, r } => (p - c).length() - r,
            Geo::Ellipsoid { c, r, inv } => {
                // Inigo Quilez's ellipsoid bound: close to exact near the surface.
                let q = inv * (p - c);
                let k0 = (q / r).length();
                let k1 = (q / (r * r)).length();
                if k1 < 1e-9 { -r.min_element() } else { k0 * (k0 - 1.0) / k1 }
            }
            Geo::Cone { a, b, r1, r2 } => round_cone(p, a, b, r1, r2),
            Geo::Box { c, half, round, inv } => {
                let q = (inv * (p - c)).abs() - (half - round);
                q.max(Vec3::ZERO).length() + q.max_element().min(0.0) - round
            }
            Geo::Torus { c, big, small, inv } => {
                let q = inv * (p - c);
                let ring = glam::Vec2::new(glam::Vec2::new(q.x, q.z).length() - big, q.y);
                ring.length() - small
            }
            // A little thicker than the mesh: closes slits thinner than a grid
            // cell (parted lips, eyelid corners, ear rims) that would break up.
            Geo::Mesh { ref sdf, above, below } => {
                // A body part sinks a little under the head part where they
                // overlap, so its coarser mesh never pokes through the head
                // (most at the concave jaw-neck corner).
                // The head part's lower edge dips under the body instead
                // of ending in a visible step.
                // Both ramps are long and shallow, so where the surfaces
                // cross there is no visible kink in the shading.
                let inset = if below < f32::MAX { HEAD_OVERLAP_INSET * smooth_ramp((p.y - (below - HEAD_OVERLAP + 0.005)) / 0.03) } else { 0.0 }
                    + if above > f32::MIN { HEAD_OVERLAP_INSET * 0.35 * (1.0 - smooth_ramp((p.y - above) / 0.025)) } else { 0.0 };
                (sdf.sdf.distance(p) - HUMAN_SKIN + inset).max(above - p.y).max(p.y - below)
            }
        }
    }
}

/// Exact SDF of a rounded cone (Inigo Quilez's sdRoundCone).
fn round_cone(p: Vec3, a: Vec3, b: Vec3, r1: f32, r2: f32) -> f32 {
    let ba = b - a;
    let l2 = ba.dot(ba);
    let rr = r1 - r2;
    let a2 = l2 - rr * rr;
    // Ends at one point, or one end sphere swallows the other.
    if l2 < 1e-12 || a2 <= 1e-9 {
        return ((p - a).length() - r1).min((p - b).length() - r2);
    }
    let il2 = 1.0 / l2;
    let pa = p - a;
    let y = pa.dot(ba);
    let z = y - l2;
    let x = pa * l2 - ba * y;
    let x2 = x.dot(x);
    let y2 = y * y * l2;
    let z2 = z * z * l2;
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        return (x2 + z2).sqrt() * il2 - r2;
    }
    if y.signum() * a2 * y2 < k {
        return (x2 + y2).sqrt() * il2 - r1;
    }
    ((x2 * a2 * il2).sqrt() + y * rr) * il2 - r1
}

/// Distance from a point to a box (0 inside).
fn box_distance(p: Vec3, lo: Vec3, hi: Vec3) -> f32 {
    (lo - p).max(p - hi).max(Vec3::ZERO).length()
}

/// The compiled field: the blobs, then the detail layer's offsets on top.
pub(crate) struct Field {
    prims: Vec<Prim>,
    max_k: f32,
    /// Blob values are clamped to +-limit after every step. Beyond it only the
    /// sign matters, and the clamp makes skipping far-away blobs exact, so the
    /// blob field is the same at any grid spacing. The detail layer stores
    /// offsets from this field, so it relies on that.
    pub(crate) limit: f32,
    detail: Option<Detail>,
    /// Only these prims make the surface (one part of a split rig, see
    /// `rig::build_parts`); None = all of them.
    keep: Option<Vec<bool>>,
    shell: Option<f32>,
    damage: Vec<crate::smart::Decal>,
}

impl Field {
    pub(crate) fn new(clay: &Clay, voxel: f32) -> Field {
        let mut prims: Vec<Prim> = Vec::new();
        let mut max_k = 0.0f32;
        let mut max_offset = 0.0f32;
        let mut max_smooth = 0.0f32;
        let names = clay.material_names();
        for blob in clay.blobs.iter().take(MAX_BLOBS) {
            let mat = names.iter().position(|n| *n == blob.material).unwrap_or(0) as u16;
            let (mut geo, mut lo, mut hi) = Geo::new(&blob.form);
            if let Geo::Hard(ref mut g) = geo { g.band_depth(blob.wall); (lo,hi)=g.bounds(); }
            let size = blob.join_size.unwrap_or(blob.blend);
            let k = if blob.join == hard_surface::Join::Sharp { 0.0 } else if size.is_finite() { size.max(0.0) } else { 0.0 };
            let ck = blob.color_blend.filter(|c| c.is_finite()).map_or(k, |c| c.clamp(0.0, k));
            let offset = if matches!(blob.op, Op::Cover | Op::Plate) && blob.offset.is_finite() { blob.offset.clamp(0.0, 0.5) } else { 0.0 };
            let smooth = if matches!(blob.op, Op::Cover | Op::Plate) && blob.smooth.is_finite() { blob.smooth.clamp(0.0, 0.5) } else { 0.0 };
            max_smooth = max_smooth.max(smooth);
            max_offset = max_offset.max(offset + smooth + blob.ridge + blob.flare);
            max_k = max_k.max(k);
            let color = Vec3::from(blob.color).clamp(Vec3::ZERO, Vec3::ONE);
            let (mlo, mhi) = (Vec3::new(-hi.x, lo.y, lo.z), Vec3::new(-lo.x, hi.y, hi.z));
            if blob.mirror {
                let (mut geo2, _, _) = Geo::new(&blob.form);
                if let Geo::Hard(ref mut g) = geo2 { g.band_depth(blob.wall); }
                let (a, b) = if blob.flip { (lo, hi) } else { (mlo, mhi) };
                prims.push(Prim { geo: geo2, op: blob.op, k, ck, offset, smooth, join: blob.join, wall: blob.wall.abs().max(1e-4), rim: blob.rim.max(0.0), ridge: blob.ridge, flare: blob.flare, envelope: None, name: blob.name.clone(), color, mat, bone: crate::rig::mirror_name(&blob.bone).unwrap_or_else(|| blob.bone.clone()), flip: !blob.flip, lo: a, hi: b });
            }
            let (a, b) = if blob.flip { (mlo, mhi) } else { (lo, hi) };
            prims.push(Prim { geo, op: blob.op, k, ck, offset, smooth, join: blob.join, wall: blob.wall.abs().max(1e-4), rim: blob.rim.max(0.0), ridge: blob.ridge, flare: blob.flare, envelope: None, name: blob.name.clone(), color, mat, bone: blob.bone.clone(), flip: blob.flip, lo: a, hi: b });
        }
        let detail = clay.detail.clone();
        // With a detail layer the limit must not depend on the mesh grid.
        let base = match &detail {
            Some(d) => 4.0 * d.voxel(),
            None => 2.0 * voxel,
        };
        // Covers read the field up to their offset away from the surface.
        let mut field = Field { prims, max_k, limit: base + clay.decals.iter().map(|d|crate::damage::depth(d).abs()*3.0 + if d.hinge.is_some() {d.size*0.5*d.angle.to_radians().sin().abs()}else{0.0}).fold(0.0,f32::max) + 2.0 * max_k + max_offset + max_smooth + clay.shell.unwrap_or(0.0) + clay.blobs.iter().filter(|b| matches!(b.op, Op::Plate | Op::Engrave | Op::Emboss)).map(|b| b.wall).fold(0.0, f32::max), detail, keep: None, shell: clay.shell, damage: clay.decals.iter().filter(|d|d.shape.code()>=5 && (crate::damage::depth(d)!=0.0 || d.hinge.is_some())).cloned().collect() };
        // Cache the large fitting filter once; fine meshing must not issue
        // dozens of human-distance queries for every grid point.
        for i in 0..field.prims.len() {
            let prim = &field.prims[i];
            if prim.op != Op::Plate || prim.smooth <= 0.0 { continue; }
            let radius = prim.smooth;
            let (lo, hi) = (prim.lo, prim.hi);
            let prefix: Vec<u32> = (0..i as u32).filter(|&j|field.prims[j as usize].op!=Op::Plate).collect();
            let key = serde_json::to_vec(&(&clay.blobs, i)).unwrap_or_default();
            let envelope = hard_surface::Envelope::cached(&key, || {
                let fitting_limit=((hi-lo).length()+max_k*2.0).max(radius*4.0);
                let body=|p|field.prefix_full(p,&prefix,false,false,fitting_limit).0;
                let mut envelope=hard_surface::Envelope::new(lo, hi, radius, |p| {
                let mut sum = 0.0;
                for z in -1..=1 { for y in -1..=1 { for x in -1..=1 {
                    let shift = Vec3::new(x as f32, y as f32, z as f32) * radius;
                    sum += body(p+shift);
                }}}
                sum / 27.0 - radius * 0.35
                });
                envelope.fit_outside(body,|p| {
                    let p=if field.prims[i].flip {Vec3::new(-p.x,p.y,p.z)} else {p};
                    field.prims[i].geo.sdf(p)<=0.0
                });
                envelope
            });
            field.prims[i].envelope = Some(envelope);
        }
        field
    }

    /// Restricts the surface to the prims marked true.
    pub(crate) fn keep_only(mut self, keep: Vec<bool>) -> Field {
        self.keep = Some(keep);
        self
    }

    fn kept(&self, i: usize) -> bool {
        self.keep.as_ref().is_none_or(|k| k.get(i).copied().unwrap_or(false))
    }

    pub(crate) fn is_carve(&self, i: usize) -> bool {
        matches!(self.prims[i].op, Op::Carve | Op::Engrave)
    }

    /// Fitted armor explicitly assigned to a bone stays rigid.
    pub(crate) fn plate_bone(&self, p: Vec3) -> Option<&str> {
        let list = self.near(p - self.limit, p + self.limit);
        let i = self.prefix_full(p, &list, false, true, self.limit).3?;
        let prim = &self.prims[i];
        (prim.op == Op::Plate && !prim.bone.is_empty()).then_some(prim.bone.as_str())
    }

    pub(crate) fn blob_bone(&self, i: usize) -> &str { &self.prims[i].bone }

    /// Center of a prim's box.
    pub(crate) fn blob_center(&self, i: usize) -> Vec3 {
        (self.prims[i].lo + self.prims[i].hi) * 0.5
    }

    /// Blobs farther than this from a point cannot change its clamped value.
    fn reach(&self) -> f32 {
        self.limit + self.max_k
    }

    /// Bounds of everything that can be solid. A smooth blend bulges at most
    /// k/4 past its blobs, so a small pad is enough.
    pub(crate) fn bounds(&self) -> Option<(Vec3, Vec3)> {
        // Every cover can grow the previous envelope; include their total
        // clearance even when blend is zero and on a very fine mesh.
        let garment_pad: f32 = self.prims.iter().filter(|p| matches!(p.op, Op::Cover | Op::Emboss)).map(|p| p.offset + if matches!(p.op, Op::Emboss) { p.wall } else { 0.0 } + p.smooth * 1.6).sum();
        let join_pad = self.prims.iter().map(|p| p.k * if p.join == hard_surface::Join::Chamfer { 0.71 } else { 0.3 }).fold(0.0, f32::max);
        let pad = join_pad + garment_pad + self.damage.iter().map(|d|crate::damage::depth(d).abs()*2.0 + if d.hinge.is_some() {d.size*0.5*d.angle.to_radians().sin().abs()}else{0.0}).fold(0.0,f32::max);
        if self.keep.is_some() {
            // A part: its own blobs, padded for sculpted bumps on them.
            let pad = pad + self.detail.as_ref().map_or(0.0, |d| 8.0 * d.voxel());
            let kept = (0..self.prims.len()).filter(|&i| self.kept(i) && matches!(self.prims[i].op, Op::Add | Op::Plate | Op::Emboss));
            return kept.map(|i| (self.prims[i].lo - pad, self.prims[i].hi + pad)).reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        }
        let blobs = self.prims.iter().filter(|p| matches!(p.op, Op::Add | Op::Plate | Op::Emboss)).map(|p| (p.lo - pad, p.hi + pad));
        let detail = self.detail.as_ref().and_then(|d| d.grid().solid_bounds());
        blobs.chain(detail).reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// Indices of prims that can matter inside the box.
    pub(crate) fn near(&self, lo: Vec3, hi: Vec3) -> Vec<u32> {
        let reach = self.reach();
        (0..self.prims.len() as u32)
            .filter(|&i| {
                let p = &self.prims[i as usize];
                // Box-to-box gap.
                let gap = (p.lo - hi).max(lo - p.hi).max(Vec3::ZERO).length();
                gap <= reach
            })
            .collect()
    }

    fn has_detail(&self, lo: Vec3, hi: Vec3) -> bool {
        self.detail.as_ref().is_some_and(|d| d.grid().touches(lo, hi))
    }

    /// Blob field value (clamped) and sRGB color at `p`, using only the prims in `list`.
    pub(crate) fn sample_base(&self, p: Vec3, list: &[u32]) -> (f32, Vec3) {
        self.base(p, list, true)
    }

    fn base(&self, p: Vec3, list: &[u32], only_kept: bool) -> (f32, Vec3) {
        let (d, col, _) = self.base_full(p, list, only_kept);
        (d, col)
    }

    /// Field value, blended color, and the material of the blob that wins.
    pub(crate) fn base_full(&self, p: Vec3, list: &[u32], only_kept: bool) -> (f32, Vec3, u16) {
        let (d, c, m, _) = self.prefix_full(p, list, only_kept, true, self.limit);
        let d = self.shell.map_or(d, |t| (d + t * 0.5).abs() - t * 0.5);
        (d, c, m)
    }

    fn prefix_full(&self, p: Vec3, list: &[u32], only_kept: bool, smoothing: bool, limit: f32) -> (f32, Vec3, u16, Option<usize>) {
        let mut d = limit;
        let mut owner = None;
        let mut col = list.first().map(|&i| self.prims[i as usize].color).unwrap_or(Vec3::splat(0.8));
        let mut mat = list.first().map_or(0, |&i| self.prims[i as usize].mat);
        for (position, &i) in list.iter().enumerate() {
            if only_kept && !self.kept(i as usize) {
                continue;
            }
            let prim = &self.prims[i as usize];
            // `lo`/`hi` are already mirrored for flipped prims, so test `p` itself.
            if prim.op != Op::Carve && box_distance(p, prim.lo, prim.hi) > limit + prim.k {
                continue;
            }
            let q = if prim.flip { Vec3::new(-p.x, p.y, p.z) } else { p };
            let db = prim.geo.sdf(q);
            let k = prim.k;
            match prim.op {
                Op::Add => {
                    if k <= 1e-6 {
                        if db < d {
                            d = db;
                            col = prim.color;
                            mat = prim.mat;
                            owner = Some(i as usize);
                        }
                    } else {
                        let h = (0.5 + 0.5 * (d - db) / k).clamp(0.0, 1.0);
                        // Color can switch over a narrower band than the shape.
                        let hc = if prim.ck >= k {
                            h
                        } else if prim.ck <= 1e-6 {
                            if h >= 0.5 { 1.0 } else { 0.0 }
                        } else {
                            (0.5 + 0.5 * (d - db) / prim.ck).clamp(0.0, 1.0)
                        };
                        d = if prim.join == hard_surface::Join::Chamfer { d.min(db).min((d + db - k) * std::f32::consts::FRAC_1_SQRT_2) } else { d * (1.0 - h) + db * h - k * h * (1.0 - h) };
                        col = col * (1.0 - hc) + prim.color * hc;
                        if h >= 0.5 {
                            mat = prim.mat;
                            owner = Some(i as usize);
                        }
                    }
                }
                Op::Carve => {
                    if k <= 1e-6 {
                        d = d.max(-db);
                    } else {
                        let h = (0.5 - 0.5 * (d + db) / k).clamp(0.0, 1.0);
                        d = if prim.join == hard_surface::Join::Chamfer { d.max(-db).max((d - db + k) * std::f32::consts::FRAC_1_SQRT_2) } else { d * (1.0 - h) - db * h + k * h * (1.0 - h) };
                    }
                }
                Op::Engrave => { d = d.max((d + prim.wall).min(-db)); }
                Op::Emboss => {
                    let raised = (d - prim.wall).max(db);
                    if raised < d { d = raised; col = prim.color; mat = prim.mat; owner = Some(i as usize); }
                }
                Op::Cover | Op::Plate => {
                    // What came before, grown, cut to this blob (`blend`
                    // rounds the garment's edge); it wins outright where it
                    // stands out, so its color and material are crisp.
                    let body = if prim.op == Op::Plate {
                        prim.envelope.as_ref().map_or_else(|| self.prefix_full(p, &list[..position], false, smoothing, limit).0, |e| e.sample(p))
                    } else if smoothing && prim.smooth > 0.0 {
                        // A low-pass envelope: average the earlier body over a
                        // spherical footprint, with clearance to bridge pits
                        // and conceal protrusions without cutting the body.
                        let prefix = &list[..position];
                        let sum: f32 = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z].iter()
                            .map(|v| self.prefix_full(p + *v * prim.smooth, prefix, only_kept, false, limit).0).sum();
                        sum / 6.0 - prim.smooth * 0.6
                    } else { d };
                    let center = (prim.lo + prim.hi) * 0.5;
                    let width = ((prim.hi.x - prim.lo.x) * 0.12).max(prim.wall);
                    let ridge = prim.ridge * (1.0 - (p.x - center.x).abs() / width).max(0.0);
                    let flare = prim.flare * (db / (prim.wall * 4.0).max(prim.rim)).min(0.0).exp();
                    let grown = body - prim.offset - ridge - flare;
                    let grown = if prim.op == Op::Plate {
                        (grown - prim.wall * 0.5).abs() - prim.wall * 0.5
                    } else { grown };
                    let k = if prim.op == Op::Plate { prim.rim } else { k };
                    let cover = if k <= 1e-6 {
                        grown.max(db)
                    } else {
                        // Smooth max of the two (as carve uses).
                        let h = (0.5 - 0.5 * (grown - db) / k).clamp(0.0, 1.0);
                        grown * (1.0 - h) + db * h + k * h * (1.0 - h)
                    };
                    if cover < d {
                        d = cover;
                        col = prim.color;
                        mat = prim.mat;
                        owner = Some(i as usize);
                    }
                }
            }
            d = d.clamp(-limit, limit);
        }
        (d, col, mat, owner)
    }

    fn project_planar(&self,p:Vec3,lo:Vec3,hi:Vec3,list:&[u32],voxel:f32)->Vec3 {
        let mut best=(f32::MAX,p);
        for &i in list {
            let prim=&self.prims[i as usize];
            if !matches!(&prim.geo,Geo::Box {round,..} if *round==0.0) && !matches!(&prim.geo,Geo::Hard(hard_surface::Geometry::Extrude {bevel,..}) if *bevel==0.0) {continue;}
            let flip=|p:Vec3|if prim.flip {Vec3::new(-p.x,p.y,p.z)} else {p};
            let q=flip(p);
            if prim.geo.sdf(q).abs()>voxel*2.0 {continue;}
            let (a,b)=if prim.flip {(flip(hi),flip(lo))} else {(lo,hi)};
            let (a,b)=(a.min(b),a.max(b));
            let Some(projected)=prim.geo.project(q,a,b) else {continue;};
            let projected=flip(projected).clamp(lo+voxel*0.001,hi-voxel*0.001);
            let d=projected.distance_squared(p);
            if d<best.0 && self.sample(projected,list).0.abs()<voxel*0.01 {best=(d,projected);}
        }
        // Retain a tiny amount of cell spacing to avoid collapsed render
        // triangles where multiple cell intersections meet an exact corner.
        p.lerp(best.1,0.99)
    }

    /// Whether the clay surface is surely farther than `far` from `p`, and
    /// on which side: Some(true) inside, Some(false) outside, None unsure.
    /// Unlike `sample`, which clamps to a few cells, this uses the blobs'
    /// own distances (union bounds), so whole blocks can be skipped.
    pub(crate) fn far_side(&self, p: Vec3, list: &[u32], far: f32) -> Option<bool> {
        if self.damage.iter().any(|d| Vec3::from(d.center).distance(p)<d.size*d.aspect.max(1.0)+far) {return None;}
        // Lower bound of the distance to any surface (for "outside"), and to
        // a solid blob (for "inside": a cover's shape only limits where the
        // garment may be, it is not solid).
        let (mut nearest, mut solid) = (f32::MAX, f32::MAX);
        let mut carve_near = false;
        for &i in list {
            if !self.kept(i as usize) {
                continue;
            }
            let prim = &self.prims[i as usize];
            let q = if prim.flip { Vec3::new(-p.x, p.y, p.z) } else { p };
            let d = prim.geo.sdf(q);
            match prim.op {
                Op::Add => {
                    nearest = nearest.min(d - prim.k);
                    solid = solid.min(d - prim.k);
                }
                // A cover's surface lies inside its own shape.
                Op::Cover | Op::Plate | Op::Emboss => nearest = nearest.min(d - prim.k),
                // A carve within reach could open the inside to the surface.
                Op::Carve | Op::Engrave => carve_near |= d < far + prim.k,
            }
        }
        if self.shell.is_some() || list.iter().any(|&i| matches!(self.prims[i as usize].op, Op::Plate | Op::Engrave | Op::Emboss)) { return None; }
        if nearest > far {
            Some(false)
        } else if solid < -far - self.max_k && !carve_near {
            Some(true)
        } else {
            None
        }
    }

    pub(crate) fn prim_count(&self) -> usize {
        self.prims.len()
    }

    /// Raw distance to one `add` blob (None for carve blobs). Mirrored blobs
    /// count as their own blob.
    pub(crate) fn blob_distance(&self, i: usize, p: Vec3) -> Option<f32> {
        let prim = self.prims.get(i).filter(|p| p.op == Op::Add)?;
        let q = if prim.flip { Vec3::new(-p.x, p.y, p.z) } else { p };
        Some(prim.geo.sdf(q))
    }

    /// Surface normal, with an unclamped CSG fallback away from
    /// the narrow distance band. This lets texture displacements exceed
    /// that band without shifting saturated interior values across zero.
    pub(crate) fn sculpt_normal(&self, p: Vec3, list: &[u32], e: f32) -> Vec3 {
        let gradient = |f: &dyn Fn(Vec3) -> f32| Vec3::new(
            f(p + Vec3::X * e) - f(p - Vec3::X * e),
            f(p + Vec3::Y * e) - f(p - Vec3::Y * e),
            f(p + Vec3::Z * e) - f(p - Vec3::Z * e));
        if self.has_detail(p - e, p + e) {
            let g = gradient(&|q| self.sample(q,list).0);
            if g.length_squared() > 1e-12 { return g.normalize(); }
        }
        // Differentiate the full CSG before clamping. Near the saturation
        // boundary, finite differences of a clamped distance have biased
        // directions; switching there would leave stripes in deep folds.
        gradient(&|q| { let d = self.prefix_full(q,list,true,true,f32::MAX / 1024.0).0; self.shell.map_or(d, |t| (d+t*0.5).abs()-t*0.5) }).normalize_or(Vec3::Y)
    }

    /// Unclamped base geometry for curvature. Do not use shading normals or
    /// damage texture relief to decide where paint is exposed at creases.
    pub(crate) fn wear_distance(&self, p: Vec3, list: &[u32]) -> f32 {
        let d = self.prefix_full(p, list, true, true, f32::MAX / 1024.0).0;
        self.shell.map_or(d, |t| (d + t * 0.5).abs() - t * 0.5)
    }

    /// Ownership follows CSG and blending, so buried blobs and cover
    /// bounding shapes cannot steal a mask from the visible surface.
    pub(crate) fn nearest_named(&self, p: Vec3, names: &[String]) -> bool {
        let mut list = self.near(p - self.limit, p + self.limit);
        if list.is_empty() { list.extend(0..self.prims.len() as u32); }
        // Ownership must also exist outside the clamped band: raised folds
        // and stretched tips need permission at their destination points.
        self.prefix_full(p, &list, true, true, f32::MAX / 1024.0).3.is_some_and(|i| names.contains(&self.prims[i].name))
    }

    /// Lower bound of `blob_distance` (distance to the blob's box) and the box volume.
    pub(crate) fn blob_box(&self, i: usize, p: Vec3) -> (f32, f32) {
        let prim = &self.prims[i];
        (box_distance(p, prim.lo, prim.hi), (prim.hi - prim.lo).max(Vec3::ZERO).element_product())
    }

    /// Final field value and color: blobs plus detail offsets.
    pub(crate) fn sample(&self, p: Vec3, list: &[u32]) -> (f32, Vec3) {
        let source=crate::damage::source_point(&self.damage,p);
        let (d, col) = self.sample_base(source, list);
        let off = self.detail.as_ref().map_or(0.0, |g| g.grid().offset(source));
        // Sculpting belongs to the part that makes the surface there; other
        // parts would grow copies of it.
        if off != 0.0 && self.keep.is_some() && d - self.base(p, list, false).0 > 0.25 * self.max_k + 1e-4 {
            return (d, col);
        }
        ((d + off).max(-crate::damage::tear_cut(&self.damage,p)), col)
    }
}

/// Runs `f` on every chunk of `data` using all CPU cores.
pub(crate) fn par_chunks<T: Send>(data: &mut [T], chunk: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let work = Mutex::new(data.chunks_mut(chunk.max(1)).enumerate());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let next = work.lock().expect("no panics while locked").next();
                    let Some((i, c)) = next else { break };
                    f(i, c);
                }
            });
        }
    });
}

impl Clay {
    /// The clay mirrored across its own x = 0: blobs flip (and swap `.L` /
    /// `.R` bones), sculpt offsets, paint, mask channels and decals too.
    pub fn mirrored_x(&self) -> Clay {
        let mut c = self.clone();
        for b in &mut c.blobs {
            b.flip = !b.flip;
            if let Some(m) = crate::rig::mirror_name(&b.bone) {
                b.bone = m;
            }
        }
        c.detail = self.detail.as_ref().map(Detail::mirrored_x);
        c.paint = self.paint.as_ref().map(crate::paint::Paint::mirrored_x);
        c.masks = self.masks.iter().map(|(k, v)| (k.clone(), v.mirrored_x())).collect();
        for d in &mut c.decals {
            crate::pose::mirror_decal_x(d);
        }
        c
    }

    /// Names (or indexes) of `add` blobs thinner than about two `cell`s.
    pub fn thin_blobs(&self, cell: f32) -> Vec<String> {
        self.blobs
            .iter()
            .enumerate()
            .filter(|(_, b)| b.op == Op::Add)
            .filter(|(_, b)| {
                let thick = match b.form {
                    Form::Chain { size, taper, .. } => size.abs().min_element() * 0.225 * taper.clamp(0.05, 1.0),
                    Form::Sphere { radius, .. } => radius,
                    Form::Ellipsoid { radii, .. } => radii.abs().min_element(),
                    Form::Capsule { radius, radius_b, .. } => radius.min(radius_b.unwrap_or(radius)),
                    Form::Box { size, .. } => size.abs().min_element() * 0.5,
                    Form::Torus { thickness, .. } => thickness,
                    Form::Human { .. } => f32::MAX,
                    Form::Membrane { thickness, .. } | Form::Shell { thickness, .. } => thickness * 0.5,
                    Form::Extrude { depth, .. } => depth * 0.5,
                    _ => self.shell.unwrap_or(f32::MAX) * 0.5,
                };
                thick.abs() < cell
            })
            .map(|(i, b)| {
                let name = if b.name.is_empty() { i.to_string() } else { b.name.clone() };
                if let Form::Membrane { thickness, .. } = b.form {
                    format!("{name}: membrane thickness {thickness} is below two grid cells ({})", 2.0 * cell)
                } else { name }
            })
            .collect()
    }

    /// Changes the sculpt and paint cell size, resampling what is there.
    pub fn set_detail_cell(&mut self, cell: f32) -> Result<(), String> {
        if !cell.is_finite() || cell < MIN_DETAIL_CELL {
            return Err(format!("detail_cell must be at least {MIN_DETAIL_CELL}"));
        }
        if let Some(d) = &self.detail {
            let grid = d.grid().resampled(cell)?;
            self.detail = Some(Detail(Arc::new(grid)));
        }
        if let Some(p) = &self.paint {
            self.paint = Some(p.resampled(cell)?);
        }
        for m in self.masks.values_mut() {
            *m = m.resampled(cell)?;
        }
        self.detail_cell = Some(cell);
        Ok(())
    }

    /// The refined detail regions (see `Region`).
    pub fn detail_regions(&self) -> Vec<Region> {
        self.detail.as_ref().map_or_else(Vec::new, |d| d.grid().levels().iter().map(|l| l.region.clone()).collect())
    }

    /// Replaces the refined detail regions: finer sculpt cells in parts of
    /// the clay (a face, hands) while `detail_cell` holds everywhere else.
    /// Without a `detail_cell` the mesh cell size becomes it.
    pub fn set_detail_regions(&mut self, regions: &[Region]) -> Result<(), String> {
        if regions.is_empty() && self.detail.as_ref().is_none_or(|d| d.grid().levels().is_empty()) {
            return Ok(());
        }
        let coarse = match self.detail.as_ref() {
            Some(d) => d.voxel(),
            None => self.detail_cell.or_else(|| self.cell_size()).ok_or("add blobs before detail regions")?,
        };
        let mut detail = self.detail.clone().unwrap_or_else(|| Detail::new(coarse));
        detail.grid_mut().set_regions(regions)?;
        self.detail = Some(detail);
        self.detail_cell = Some(self.detail_cell.unwrap_or(coarse));
        Ok(())
    }

    /// A mesh with cells of `detail_cell`, so fine sculpting shows. Errs
    /// without a `detail_cell` or when it is too fine for the clay's size
    /// (see `fine_check`).
    pub fn build_fine(&self, library: &[Material], fallback: &str) -> Result<Mesh, String> {
        let cell = self.detail_cell.ok_or("no detail_cell")?;
        self.fine_check()?;
        let (lo, hi) = Field::new(self, 0.0).bounds().ok_or("the clay is empty")?;
        let cells = ((hi - lo).max_element() / cell).ceil() as u32;
        // With detail regions the mesh starts at the regular resolution and
        // is refined only where there is sculpt or paint, to its cell.
        let refine = self.detail.as_ref().is_some_and(|d| !d.grid().levels().is_empty());
        // The coarse cell is the finest detail cell times a power of two,
        // near the regular mesh cell, so every level's cell is met exactly.
        let res = match self.detail.as_ref().filter(|_| refine) {
            Some(d) => {
                let (finest, extent) = (d.finest(), (hi - lo).max_element());
                let regular = extent / self.mesh_resolution() as f32;
                let k = (regular / finest).log2().round().clamp(0.0, crate::adaptive::MAX_FACTOR.ilog2() as f32);
                extent / (finest * k.exp2())
            }
            None => cells.max(MIN_RESOLUTION) as f32,
        };
        let mesh = self.build_window_cells(None, library, fallback, res, None, refine)?;
        if mesh.indices.is_empty() {
            return Err("the fine mesh came out empty".into());
        }
        Ok(mesh)
    }

    /// The most detailed mesh a bake reads: the fine mesh when there is a
    /// `detail_cell` (falling back to the regular mesh if it cannot be
    /// built), else the regular mesh at twice its resolution.
    #[cfg(test)]
    pub fn build_high(&self, library: &[Material], fallback: &str) -> Mesh {
        if self.detail_cell.is_some() && let Ok(m) = self.build_fine(library, fallback) {
            return m;
        }
        let res = (self.mesh_resolution() * 2).min(MAX_RESOLUTION * 2);
        self.build_cells(None, library, fallback, res as f32)
    }

    /// The surface a bake reads when no fine mesh was built: the regular
    /// (rest) mesh, its edges split where sculpt or paint is finer than it
    /// (down to that cell) and the new points moved onto the clay along the
    /// mesh normal. Cheaper than a fine mesh; a bake only casts short rays
    /// at it. Only positions and normals are meaningful.
    pub(crate) fn refine_rest(&self, rest: &Mesh, max_triangles: usize) -> Mesh {
        let Some(grid) = self.detail.as_ref().map(|d| d.grid()) else { return rest.clone() };
        let Some(cell) = self.cell_size() else { return rest.clone() };
        let field = Field::new(self, cell);
        let all: Vec<u32> = (0..field.prims.len() as u32).collect();
        let painted = |p: Vec3| self.paint.as_ref().is_some_and(|g| g.grid().near(p)) || self.masks.values().any(|g| g.grid().near(p));
        let size = |p: Vec3| { let c = grid.wanted_cell(p); if painted(p) { c.min(grid.voxel) } else { c } };
        let (field, all) = (&field, &all);
        let sample = |p: Vec3, len: f32| {
            let near = field.near(p - len, p + len);
            move |q: Vec3| field.sample(q, if near.is_empty() { all } else { &near }).0
        };
        let project = |p: Vec3, len: f32, n: Vec3| crate::adaptive::along(&sample(p, len), p, n, grid.cell_at(p) * 0.25, len * 0.75);
        let mut positions: Vec<Vec3> = rest.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
        let mut indices = rest.indices.clone();
        let Ok(refined) = crate::adaptive::refine(&mut positions, &mut indices, &size, &project, max_triangles) else { return rest.clone() };
        let first = rest.vertices.len();
        let mut vertices = rest.vertices.clone();
        vertices.extend(refined.parents.iter().map(|&(a, _)| rest.vertices.get(a as usize).copied().unwrap_or(rest.vertices[0])));
        par_chunks(&mut vertices[first..], 1024, |chunk, out| {
            for (k, v) in out.iter_mut().enumerate() {
                let p = positions[first + chunk * 1024 + k];
                let e = grid.cell_at(p) * 0.75;
                let f = sample(p, e * 2.0);
                let g = Vec3::new(f(p + Vec3::X * e) - f(p - Vec3::X * e), f(p + Vec3::Y * e) - f(p - Vec3::Y * e), f(p + Vec3::Z * e) - f(p - Vec3::Z * e));
                v.pos = p.to_array();
                v.normal = g.normalize_or(Vec3::from(v.normal)).to_array();
            }
        });
        // Old corners keep their place and normal; only new points moved.
        Mesh { vertices, indices, materials: rest.materials.clone(), ..Mesh::default() }
    }

    /// Resolve a film detail window without meshing distant geometry. The
    /// window caps lie outside its camera framing and retain closed topology.
    #[cfg(test)]
    pub(crate) fn build_fine_region(&self, lo:Vec3, hi:Vec3, library:&[Material], fallback:&str) -> Result<Mesh,String> {
        let cell=self.detail_cell.ok_or("no detail_cell")?;
        if !lo.is_finite() || !hi.is_finite() || (hi-lo).min_element()<=0.0 || (hi-lo).max_element()/cell>MAX_FINE_CELLS as f32 {return Err("invalid fine region".into());}
        let (a,b)=Field::new(self,0.0).bounds().ok_or("empty clay")?;
        let mesh=self.build_window_cells(None,library,fallback,(b-a).max_element()/cell,Some((lo,hi)),false)?;
        if mesh.indices.is_empty() {return Err("empty fine region".into());}
        Ok(mesh)
    }

    /// Whether a fine mesh at `detail_cell` fits the limits; the error says
    /// the smallest cell that does.
    pub fn fine_check(&self) -> Result<(), String> {
        let Some(cell) = self.detail_cell else { return Ok(()) };
        // Adaptive fine meshes (with detail regions) start from the regular
        // grid; only their triangle count limits them.
        if self.detail.as_ref().is_some_and(|d| !d.grid().levels().is_empty()) {
            return Ok(());
        }
        let Some((lo, hi)) = Field::new(self, 0.0).bounds() else { return Ok(()) };
        let size = hi - lo;
        let across = size.max_element() / cell;
        let blocks = ((size / cell + 5.0) / BRICK as f32).ceil();
        let block_count = blocks.x as f64 * blocks.y as f64 * blocks.z as f64;
        if across <= MAX_FINE_CELLS as f32 && block_count <= MAX_FINE_BLOCKS as f64 {
            return Ok(());
        }
        // Smallest cell within both limits.
        let by_cells = size.max_element() / MAX_FINE_CELLS as f32;
        let by_blocks = (size.x as f64 * size.y as f64 * size.z as f64 / (MAX_FINE_BLOCKS as f64 * (BRICK * BRICK * BRICK) as f64)).cbrt() as f32;
        let least = by_cells.max(by_blocks) * 1.05;
        Err(format!(
            "detail_cell {cell} is too fine for this clay ({:.2} x {:.2} x {:.2} across; the fine mesh would be {across:.0} cells across, at most {MAX_FINE_CELLS}). Use detail_cell >= {least:.4}, or put the part that needs fine detail (a head, a hand, a face) in its own clay object.",
            size.x, size.y, size.z
        ))
    }

    /// Turns the blobs into a triangle mesh in the object's space (preset
    /// materials only; see `build_mesh_with`).
    #[cfg(test)]
    pub fn build_mesh(&self) -> Mesh {
        self.build_part(None, &[], "")
    }

    /// `build_mesh` with the scene's material library; blobs with no material
    /// use `fallback` (the object's material).
    pub fn build_mesh_with(&self, library: &[Material], fallback: &str) -> Mesh {
        self.build_part(None, library, fallback)
    }

    /// Every distinct blob material name, in order of first use ("" is the
    /// object's material). Never empty, so clay meshes always carry materials.
    pub fn material_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for b in self.blobs.iter().take(MAX_BLOBS) {
            if !names.contains(&b.material) {
                names.push(b.material.clone());
            }
        }
        if names.is_empty() {
            names.push(String::new());
        }
        names
    }

    /// `material_names` resolved against the library (see `Material::resolve`).
    pub fn palette(&self, library: &[Material], fallback: &str) -> Vec<Material> {
        let bounds = Field::new(self, self.cell_size().unwrap_or(0.01)).bounds();
        self.material_names().iter().map(|n| {
            let mut m = Material::resolve(library, if n.is_empty() { fallback } else { n });
            if let Some((lo,hi)) = bounds { crate::damage::part_grime(&mut m, lo, hi); }
            m
        }).collect()
    }

    /// The mesh of only the prims marked in `keep` (by `Field` prim index),
    /// on the same grid as the whole clay.
    pub fn build_part(&self, keep: Option<Vec<bool>>, library: &[Material], fallback: &str) -> Mesh {
        self.build_cells(keep, library, fallback, self.mesh_resolution() as f32)
    }

    /// `build_part` with `res` cells across the longest side.
    fn build_cells(&self, keep: Option<Vec<bool>>, library: &[Material], fallback: &str, res: f32) -> Mesh {
        self.build_window_cells(keep,library,fallback,res,None,false).unwrap_or_default()
    }

    /// Surface nets on a grid of `res` cells across, cut to `window`; with
    /// `refine`, then refined to the detail regions' cells (see
    /// `adaptive.rs`), which can fail on the triangle limit.
    fn build_window_cells(&self, keep: Option<Vec<bool>>, library: &[Material], fallback: &str, res: f32, window:Option<(Vec3,Vec3)>, refine: bool) -> Result<Mesh, String> {
        let palette = self.palette(library, fallback);
        // First pass with a rough voxel size, only to find the bounds.
        let Some((lo, hi)) = Field::new(self, 0.0).bounds() else { return Ok(Mesh::default()) };
        let extent = hi - lo;
        if !extent.is_finite() || extent.max_element() <= 0.0 {
            return Ok(Mesh::default());
        }
        let voxel = extent.max_element() / res;
        if self.sharp && window.is_none() && self.blobs.len()==1 && !self.blobs[0].mirror && !self.blobs[0].flip && self.blobs[0].op==Op::Add && self.shell.is_none() && self.detail.is_none() && self.paint.is_none() && self.decals.is_empty() && !palette.iter().any(crate::smart::layered) && keep.as_ref().is_none_or(|k|k.len()==1 && k[0])
            && let Some(mut mesh)=hard_surface::planar_mesh(&self.blobs[0].form,voxel) {
                let (geo,_,_)=Geo::new(&self.blobs[0].form);
                let normal=|p:Vec3,e:f32| {let g=|a:Vec3|geo.sdf(p+a*e)-geo.sdf(p-a*e);Vec3::new(g(Vec3::X),g(Vec3::Y),g(Vec3::Z)).normalize_or(Vec3::Y)};
                mesh.materials=palette;
                for v in &mut mesh.vertices {
                    v.normal=normal(Vec3::from(v.pos),voxel*0.05).to_array();v.color=mesh.materials[0].painted_color(Vec3::from(self.blobs[0].color), None, Vec3::ZERO).to_array();v.surface=mesh.materials[0].surface();v.light=mesh.materials[0].light();
                }
                if mesh.materials[0].weathered() {
                    let width=self.wear_width();let wear=crate::weather::wear_maps(&mesh,width);let m=crate::weather::with_scale(&mesh.materials[0],self);
                    for (v,w) in mesh.vertices.iter_mut().zip(wear) {let (c,r)=crate::weather::apply(&m,Vec3::from(v.pos),Vec3::from(v.normal),Vec3::from(v.color),v.surface[0],w,width);v.color=c.to_array();v.surface[0]=r;}
                }
                hard_surface::split_normals(&mut mesh,|p|normal(p,voxel*0.005));return Ok(mesh);
        }
        let field = Field::new(self, voxel);
        let field = match keep {
            Some(k) => field.keep_only(k),
            None => field,
        };
        let Some((mut lo,mut hi))=field.bounds() else {return Ok(Mesh::default())};
        if let Some((a,b))=window {lo=lo.max(a);hi=hi.min(b);}
        if (hi-lo).min_element()<=0.0 {return Ok(Mesh::default());}
        let clip=|p:Vec3|window.map_or(f32::NEG_INFINITY,|(a,b)| {
            let q=(p-(a+b)*0.5).abs()-(b-a)*0.5;
            q.max(Vec3::ZERO).length()+q.max_element().min(0.0)
        });
        let sample=|p:Vec3,list:&[u32]| {let (d,c)=field.sample(p,list);(d.max(clip(p)),c)};
        let normal=|p:Vec3,list:&[u32],e:f32| {
            let g=|axis:Vec3|sample(p+axis*e,list).0-sample(p-axis*e,list).0;
            Vec3::new(g(Vec3::X),g(Vec3::Y),g(Vec3::Z)).normalize_or(Vec3::Y)
        };
        // Grid points line up with the whole clay's grid, so parts match it.
        let lo = (lo / voxel).floor() * voxel;
        // Two empty cells on every side so the surface always closes.
        let lo = lo - 2.0 * voxel;
        let dims = ((hi - lo) / voxel).ceil().as_uvec3() + 3;
        let (nx, ny, nz) = (dims.x as usize, dims.y as usize, dims.z as usize);
        // The grid is stored in BRICK^3 blocks; blocks with no blob near them
        // are never sampled or stored (they stay `limit`, as `sample` would
        // give), so memory follows the surface, not the bounding box.
        let (bnx, bny, bnz) = (nx.div_ceil(BRICK), ny.div_ceil(BRICK), nz.div_ceil(BRICK));
        if bnx * bny * bnz > MAX_FINE_BLOCKS {
            return Ok(Mesh::default());
        }
        let point = |x: usize, y: usize, z: usize| lo + Vec3::new(x as f32, y as f32, z as f32) * voxel;
        // Blocks far from the surface (judged at their center: blob fields
        // change no faster than distance) are all inside or all outside and
        // are not sampled point by point; most of a solid clay is like that.
        let far = 1.5 * BRICK as f32 * voxel;
        let timer = std::time::Instant::now();
        let lap = |what: &str| if std::env::var("MESH_TIMES").is_ok() { eprintln!("mesh {what}: {:.2}s", timer.elapsed().as_secs_f32()) };
        let mut bricks: Vec<Block> = vec![Block::Out; bnx * bny * bnz];
        par_chunks(&mut bricks, bnx * bny, |bz, layer| {
            let z0 = bz * BRICK;
            for by in 0..bny {
                for bx in 0..bnx {
                    let (x0, y0) = (bx * BRICK, by * BRICK);
                    let (x1, y1, z1) = ((x0 + BRICK).min(nx), (y0 + BRICK).min(ny), (z0 + BRICK).min(nz));
                    let (blo, bhi) = (point(x0, y0, z0) - voxel, point(x1, y1, z1) + voxel);
                    let near = field.near(blo, bhi);
                    let detail = field.has_detail(blo, bhi);
                    if near.is_empty() && !detail {
                        continue;
                    }
                    if !detail && clip((blo+bhi)*0.5).abs()>far && let Some(inside) = field.far_side((blo + bhi) * 0.5, &near, far) {
                        layer[by * bnx + bx] = if inside { Block::In } else { Block::Out };
                        continue;
                    }
                    let mut b = vec![field.limit; BRICK * BRICK * BRICK];
                    for z in z0..z1 {
                        for y in y0..y1 {
                            for x in x0..x1 {
                                b[(x - x0) + BRICK * ((y - y0) + BRICK * (z - z0))] = sample(point(x, y, z), &near).0;
                            }
                        }
                    }
                    layer[by * bnx + bx] = Block::Data(b.into_boxed_slice());
                }
            }
        });
        let brick = |x: usize, y: usize, z: usize| x / BRICK + bnx * (y / BRICK + bny * (z / BRICK));
        let at = |x: usize, y: usize, z: usize| match &bricks[brick(x, y, z)] {
            Block::Data(b) => b[x % BRICK + BRICK * (y % BRICK + BRICK * (z % BRICK))],
            Block::Out => field.limit,
            Block::In => -field.limit,
        };
        // A cell can only hold surface if its block or a +1 neighbor block is
        // sampled, or those blocks are both inside and outside.
        let live: Vec<bool> = (0..bricks.len())
            .map(|i| {
                let (bx, by, bz) = (i % bnx, (i / bnx) % bny, i / (bnx * bny));
                let (mut data, mut inside, mut outside) = (false, false, false);
                for &(dx, dy, dz) in &CORNERS {
                    let (x, y, z) = (bx + dx, by + dy, bz + dz);
                    if x < bnx && y < bny && z < bnz {
                        match bricks[x + bnx * (y + bny * z)] {
                            Block::Data(_) => data = true,
                            Block::In => inside = true,
                            Block::Out => outside = true,
                        }
                    }
                }
                data || (inside && outside)
            })
            .collect();
        // Cells (x, y, z < n - 1) of live blocks, in a fixed order.
        let cells_in_live_blocks = || {
            (0..bricks.len()).filter(|&i| live[i]).flat_map(move |i| {
                let (bx, by, bz) = (i % bnx, (i / bnx) % bny, i / (bnx * bny));
                let (x0, y0, z0) = (bx * BRICK, by * BRICK, bz * BRICK);
                let (x1, y1, z1) = ((x0 + BRICK).min(nx - 1), (y0 + BRICK).min(ny - 1), (z0 + BRICK).min(nz - 1));
                (z0..z1).flat_map(move |z| (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y, z))))
            })
        };

        // Smooth-only clay keeps the original fast surface-net path.
        let sharp = self.sharp && (self.shell.is_some() || self.blobs.iter().any(|b|
            !matches!(b.form, Form::Sphere { .. } | Form::Ellipsoid { .. } | Form::Capsule { .. } | Form::Human { .. } | Form::Torus { .. } | Form::Chain { .. } | Form::Membrane { .. })
            || matches!(b.op, Op::Plate | Op::Engrave | Op::Emboss) || b.join == hard_surface::Join::Chamfer));
        // With detail regions: adaptive surface nets, each coarse cell near
        // the surface cut down to the cell the detail there needs (see
        // `adaptive::Nets`).
        let detail_grid = self.detail.as_ref().map(|d| d.grid());
        let adaptive = refine && detail_grid.is_some_and(|g| !g.levels().is_empty());
        let (positions, indices, edges): (Vec<Vec3>, Vec<u32>, Vec<u32>) = if adaptive {
            let grid = detail_grid.expect("checked");
            let painted = |p: Vec3| self.paint.as_ref().is_some_and(|g| g.grid().near(p)) || self.masks.values().any(|g| g.grid().near(p));
            let factor = |mid: Vec3| {
                let half = Vec3::splat(voxel * 0.5);
                // The finest wanted at the middle and corners of the cell.
                let wanted = [mid, mid - half, mid + half, mid + Vec3::new(half.x, -half.y, -half.z), mid + Vec3::new(-half.x, half.y, half.z)]
                    .iter().map(|&p| { let c = grid.wanted_cell(p); if painted(p) { c.min(grid.voxel) } else { c } }).fold(f32::INFINITY, f32::min);
                if !wanted.is_finite() || wanted >= voxel * 0.75 { 1 } else { (voxel / wanted).log2().round().exp2() as u32 }
            };
            let mut cells = Vec::new();
            for (x, y, z) in cells_in_live_blocks() {
                let f = CORNERS.map(|(dx, dy, dz)| at(x + dx, y + dy, z + dz));
                let inside = f.iter().filter(|v| **v < 0.0).count();
                // Surface can pass through a cell with all corners on one
                // side (a thin ridge); the field is about a distance.
                if (inside > 0 && inside < 8) || f.iter().any(|v| v.abs() < 1.8 * voxel) {
                    cells.push([x, y, z]);
                }
            }
            let coarse = |x: usize, y: usize, z: usize| at(x, y, z);
            let fine = |p: Vec3, list: &[u32]| sample(p, list).0;
            let near = |a: Vec3, b: Vec3| field.near(a, b);
            let nets = crate::adaptive::Nets { lo, cell: voxel, n: [nx, ny, nz], coarse: &coarse, field: &fine, near: &near, factor: &factor };
            lap("coarse grid");
            let (mut positions, mut indices) = nets.mesh(&cells, MAX_FINE_TRIANGLES)?;
            lap("adaptive nets");
            // A vertex on two pinches can need a second pass.
            for _ in 0..4 {
                let n = crate::adaptive::make_manifold(&mut positions, &mut indices);
                if std::env::var("MESH_TIMES").is_ok() { eprintln!("manifold copies {n}"); }
                if n == 0 {
                    break;
                }
            }
            lap("manifold");
            let edges = crate::bake::edges(&indices);
            (positions, indices, edges)
        } else {
            // One vertex per cell the surface passes through.
            let cell = |x: usize, y: usize, z: usize| x + (nx - 1) * (y + (ny - 1) * z);
            let mut cell_vertex: HashMap<usize, u32> = HashMap::new();
            let mut positions = Vec::new();
            const EDGES: [(usize, usize); 12] =
                [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];
            for (x, y, z) in cells_in_live_blocks() {
                let f = CORNERS.map(|(dx, dy, dz)| at(x + dx, y + dy, z + dz));
                let inside = f.iter().filter(|v| **v < 0.0).count();
                if inside == 0 || inside == 8 {
                    continue;
                }
                let (mut sum, mut n) = (Vec3::ZERO, 0.0);
                let mut hits = [(Vec3::ZERO,Vec3::ZERO);12];
                let mut crossing_edges = [(0usize,0usize);12];
                let origin = point(x,y,z);
                for (a,b) in EDGES {
                    if (f[a]<0.0)!=(f[b]<0.0) {
                        let t=f[a]/(f[a]-f[b]);
                        let pa=Vec3::new(CORNERS[a].0 as f32,CORNERS[a].1 as f32,CORNERS[a].2 as f32);
                        let pb=Vec3::new(CORNERS[b].0 as f32,CORNERS[b].1 as f32,CORNERS[b].2 as f32);
                        let hit=pa+(pb-pa)*t;
                        if sharp {
                            // Cheap trilinear gradients identify candidate bends
                            // without extra field evaluations on planar cells.
                            hits[n as usize]=(hit,Vec3::ZERO);
                            crossing_edges[n as usize]=(a,b);
                        }
                        sum+=hit; n+=1.0;
                    }
                }
                let mean=sum/n;
                let mut position=mean;
                let corner_normals = if sharp { hard_surface::corner_normals(&f) } else { [(Vec3::ZERO,Vec3::ZERO);8] };
                if sharp && hard_surface::bent(&corner_normals,0.98) {
                    let near=field.near(origin-voxel,origin+2.0*voxel);
                    for (i,&(a,b)) in crossing_edges[..n as usize].iter().enumerate() {
                        let pa=Vec3::new(CORNERS[a].0 as f32,CORNERS[a].1 as f32,CORNERS[a].2 as f32);
                        let pb=Vec3::new(CORNERS[b].0 as f32,CORNERS[b].1 as f32,CORNERS[b].2 as f32);
                        let (mut left,mut right)=(0.0,1.0);
                        for _ in 0..16 {
                            let mid=(left+right)*0.5;
                            let value=sample(origin+(pa+(pb-pa)*mid)*voxel,&near).0;
                            if (value<0.0)==(f[a]<0.0) {left=mid;}else{right=mid;}
                        }
                        let hit=pa+(pb-pa)*((left+right)*0.5);
                        hits[i]=(hit,normal(origin+hit*voxel,&near,voxel*0.01));
                    }
                    // Even a planar cell beside a corner needs the refined
                    // crossings: its corner distances are nonlinear outside the
                    // solid, so the original linear mean can sit off the face.
                    let refined_mean=hits[..n as usize].iter().map(|(p,_)|*p).sum::<Vec3>()/n;
                    position=hard_surface::feature_vertex(&hits[..n as usize],refined_mean);
                }
                if sharp {
                    let near=field.near(origin-voxel,origin+2.0*voxel);
                    position=(field.project_planar(origin+position*voxel,origin,origin+voxel,&near,voxel)-origin)/voxel;
                }
                cell_vertex.insert(cell(x,y,z),positions.len() as u32);
                if sharp { position=position.clamp(Vec3::splat(1e-3),Vec3::splat(1.0-1e-3)); }
                positions.push(origin+position*voxel);
            }

            // One quad per grid edge the surface crosses, joining the 4 cells around it.
            let mut indices = Vec::new();
            let mut edges = Vec::new();
            let mut seen: HashMap<(u32,u32),u8> = HashMap::new();
            let mut ambiguous=false;
            let mut quads = Vec::new();
            for (x, y, z) in cells_in_live_blocks() {
                let v0 = at(x, y, z) < 0.0;
                for axis in 0..3 {
                    let ok = match axis {
                        0 => y > 0 && z > 0,
                        1 => x > 0 && z > 0,
                        _ => x > 0 && y > 0,
                    };
                    // Cell loops stop one short, so the +1 neighbor always exists.
                    let (ax, ay, az) = [(x + 1, y, z), (x, y + 1, z), (x, y, z + 1)][axis];
                    if !ok || v0 == (at(ax, ay, az) < 0.0) {
                        continue;
                    }
                    let cells = match axis {
                        0 => [cell(x, y - 1, z - 1), cell(x, y, z - 1), cell(x, y, z), cell(x, y - 1, z)],
                        1 => [cell(x - 1, y, z - 1), cell(x - 1, y, z), cell(x, y, z), cell(x, y, z - 1)],
                        _ => [cell(x - 1, y - 1, z), cell(x, y - 1, z), cell(x, y, z), cell(x - 1, y, z)],
                    };
                    let q = cells.map(|c| cell_vertex.get(&c).copied().unwrap_or(u32::MAX));
                    if q.contains(&u32::MAX) {
                        continue;
                    }
                    let [a, b, c, d] = if v0 { q } else { [q[3], q[2], q[1], q[0]] };
                    let direction = [Vec3::X,Vec3::Y,Vec3::Z][axis] * if v0 {1.0} else {-1.0};
                    if sharp { quads.push(([a,b,c,d],direction,point(x,y,z),point(ax,ay,az))); } else { indices.extend_from_slice(&[a,b,c,a,c,d]); }
                    for (s, e) in [(a, b), (b, c), (c, d), (d, a)] {
                        let count=seen.entry((s.min(e),s.max(e))).or_default();*count+=1;
                        ambiguous|=*count>2;
                        if *count==1 {edges.extend_from_slice(&[s,e]);}
                    }
                }
            }

            for (q,direction,start,end) in quads {
                let [a,b,c,d]=q;
                match hard_surface::quad_diagonal(q,direction,&positions) {
                    Some(true)=>indices.extend_from_slice(&[a,b,d,b,c,d]),
                    Some(false)=>indices.extend_from_slice(&[a,b,c,a,c,d]),
                    None=> {
                        // A primal-edge intersection is inside all four projected
                        // cell quadrants. A fan around it preserves winding while
                        // retaining sharp QEF vertices, instead of rounding an
                        // entire row back to surface-net positions.
                        let near=field.near(start-voxel,end+voxel);
                        let inside=sample(start,&near).0<0.0;
                        let (mut left,mut right)=(0.0,1.0);
                        for _ in 0..16 {
                            let t=(left+right)*0.5;
                            if (sample(start.lerp(end,t),&near).0<0.0)==inside {left=t;} else {right=t;}
                        }
                        let center=positions.len() as u32;
                        positions.push(start.lerp(end,((left+right)*0.5).clamp(0.001,0.999)));
                        indices.extend_from_slice(&[a,b,center,b,c,center,c,d,center,d,a,center]);
                    }
                }
            }

            if ambiguous {
                let mut tetra=hard_surface::TetraMesh::default();
                for (x,y,z) in cells_in_live_blocks() {
                    let values=CORNERS.map(|(dx,dy,dz)|at(x+dx,y+dy,z+dz));
                    if values.iter().all(|v|*v<0.0) || values.iter().all(|v|*v>=0.0) {continue;}
                    let ids=CORNERS.map(|(dx,dy,dz)|(x+dx)+nx*((y+dy)+ny*(z+dz)));
                    let points=CORNERS.map(|(dx,dy,dz)|point(x+dx,y+dy,z+dz));
                    tetra.cell(ids,points,values);
                }
                positions=tetra.positions;indices=tetra.indices;
                edges.clear();seen.clear();
                for t in indices.chunks_exact(3) {for (a,b) in [(t[0],t[1]),(t[1],t[2]),(t[2],t[0])] {
                    if seen.insert((a.min(b),a.max(b)),1).is_none() {edges.extend_from_slice(&[a,b]);}
                }}
            }
            (positions, indices, edges)
        };

        // Normals from the field's gradient, colors from the blend.
        let mut vertices: Vec<Vertex> =
            positions.iter().map(|p| Vertex { pos: p.to_array(), normal: [0.0, 1.0, 0.0], color: [1.0; 3], surface: [0.0; 4], material: 0, uv: [0.0; 2], tangent: [0.0; 4], light: [0.0; 2], base: [1.0, 1.0, 1.0, -1.0], wear: [0.0; 3], painted: [0; 4] }).collect();
        let all: Vec<u32> = (0..field.prims.len() as u32).collect();
        // Detail offsets are trilinear, so take the gradient over a detail cell
        // or more to keep the shading smooth.
        let normal_cell=voxel * if sharp {0.05} else {0.5};
        let e = self.detail.as_ref().map_or(normal_cell, |d| normal_cell.max(d.voxel() * 0.75));
        let refined = refine && detail_grid.is_some_and(|g| !g.levels().is_empty());
        lap("edges");
        par_chunks(&mut vertices, 1024, |_, chunk| {
            for v in chunk {
                let p = Vec3::from(v.pos);
                // Inside detail regions the gradient step shrinks with the cell.
                let e = match detail_grid.filter(|_| refined) {
                    Some(g) => { let c = g.cell_at(p); (normal_cell * c / g.voxel).max(c * 0.75) }
                    None => e,
                };
                let near = field.near(p - voxel, p + voxel);
                let list = if near.is_empty() { &all } else { &near };
                let g = |d: Vec3| sample(p + d, list).0 - sample(p - d, list).0;
                let n = Vec3::new(g(Vec3::X * e), g(Vec3::Y * e), g(Vec3::Z * e)).normalize_or(Vec3::Y);
                v.normal = n.to_array();
                // Color from the blob surface under any sculpted clay, so new
                // material takes the color of what it grew from, not of nearby blobs.
                let off = field.detail.as_ref().map_or(0.0, |d| d.grid().offset(p));
                let (_, base, mat) = field.base_full(p + n * off, list, true);
                v.color = palette[(mat as usize).min(palette.len()-1)].painted_color(base, self.paint.as_ref(), p).to_array();
                let mat = (mat as usize).min(palette.len() - 1);
                v.material = mat as u32;
                v.surface = palette[mat].surface();
                v.light = palette[mat].light();
            }
        });
        let mut mesh = Mesh { vertices, indices, edges, materials: palette, ..Mesh::default() };
        // A caliber-sized hit should resolve in one call even on a large
        // clay object: refine just its triangle patch, then project new
        // vertices onto the damaged field. No global fine-grid allocation.
        let damage:Vec<_>=self.decals.iter().filter(|d|d.shape.code()>=5 && crate::damage::depth(d)!=0.0).cloned().collect();
        if !damage.is_empty() {
            let old=mesh.vertices.len();crate::damage::refine_mesh(&mut mesh,&damage,None);
            let e=damage.iter().map(|d|d.size/80.0).fold(voxel*0.25,f32::min).max(1e-5);
            let materials=mesh.materials.clone();
            par_chunks(&mut mesh.vertices[old..],512,|_,chunk| {for v in chunk {
                let start=Vec3::from(v.pos);let mut p=start;
                let near=field.near(start-voxel*4.0,start+voxel*4.0);
                let grad=|p:Vec3| {let g=|a:Vec3|field.sample(p+a*e,&near).0-field.sample(p-a*e,&near).0;Vec3::new(g(Vec3::X),g(Vec3::Y),g(Vec3::Z)).normalize_or(Vec3::from(v.normal))};
                for _ in 0..8 {let d=field.sample(p,&near).0;p-=grad(p)*d; if d.abs()<e*0.1 {break;}}
                let n=grad(p);v.pos=p.to_array();v.normal=n.to_array();
                let source=crate::damage::source_point(&damage,p);let (_,base,mat)=field.base_full(source,&near,true);
                v.color=materials[(mat as usize).min(materials.len()-1)].painted_color(base,self.paint.as_ref(),p).to_array();v.material=(mat as usize).min(materials.len()-1) as u32;
                let m=&materials[v.material as usize];v.surface=m.surface();v.light=m.light();
            }});
        }
        // Weathering, smart layers and decals follow the shape; the width
        // comes from the regular mesh cell, so fine and regular meshes
        // weather alike.
        lap("normals and colors");
        crate::smart::finish(self, &field, &mut mesh);
        lap("smart finish");
        mesh.face = self.face().map(Arc::new);
        if sharp { hard_surface::split_normals(&mut mesh, |p| {
            let near=field.near(p-voxel,p+voxel);
            normal(p,&near,voxel*0.005)
        }); }
        Ok(mesh)
    }

    /// The face of a human head blob (whole body or head part), if its
    /// skin detail is on.
    pub fn face(&self) -> Option<crate::face::Face> {
        self.blobs.iter().find_map(|b| match &b.form {
            Form::Human { human, part } if *part != HumanPart::Body && human.skin.enabled => {
                let l = crate::human::shape(human).ok()?.landmarks?;
                Some(crate::face::Face::new(&l, &human.skin, human.age, human.sex))
            }
            _ => None,
        })
    }

    /// How wide weathering bands are: a few regular mesh cells.
    pub fn wear_width(&self) -> f32 {
        self.cell_size().unwrap_or(0.01) * 2.5
    }

    fn mesh_resolution(&self) -> u32 {
        let hard = self.shell.is_some() || self.blobs.iter().any(|b| matches!(b.form, Form::Box {..} | Form::Extrude {..} | Form::Lathe {..} | Form::Sweep {..} | Form::Shell {..} | Form::Array {..} | Form::Band {..}) || matches!(b.op, Op::Plate | Op::Engrave | Op::Emboss));
        // A uniform refinement preserves shared connectivity at feature cells.
        // Explicit fine meshes still use detail_cell without this floor.
        if !hard || self.resolution != DEFAULT_RESOLUTION { return self.resolution.clamp(MIN_RESOLUTION,MAX_RESOLUTION); }
        let feature=self.blobs.iter().filter_map(|b| {
            if matches!(b.op,Op::Plate|Op::Engrave|Op::Emboss) {Some(b.wall)} else {hard_surface::feature_size(&b.form)}
        }).chain(self.shell).filter(|x|x.is_finite() && *x>0.0).fold(f32::MAX,f32::min);
        let extent=Field::new(self,0.0).bounds().map_or(0.0,|(lo,hi)|(hi-lo).max_element());
        ((extent/(feature/3.0)).ceil() as u32).clamp(192,MAX_RESOLUTION)
    }

    /// The mesh cell size at the current resolution (None when empty).
    pub fn cell_size(&self) -> Option<f32> {
        let res = self.mesh_resolution() as f32;
        let (lo, hi) = Field::new(self, 0.0).bounds()?;
        let e = (hi - lo).max_element();
        (e.is_finite() && e > 0.0).then_some(e / res)
    }

    /// Drops what a hostile or broken file could use to stall the editor.
    pub fn sanitize(&mut self) {
        self.blobs.truncate(MAX_BLOBS);
        self.blobs.retain(|b| b.validate().is_ok());
        self.shell = self.shell.filter(|t| t.is_finite() && *t > 0.0);
        self.resolution = self.resolution.clamp(MIN_RESOLUTION, MAX_RESOLUTION);
        if self.detail.as_ref().is_some_and(|d| !d.is_sane()) {
            self.detail = None;
        }
        if self.paint.as_ref().is_some_and(|p| !p.is_sane()) {
            self.paint = None;
        }
        self.masks.retain(|name, m| m.is_sane() && !name.trim().is_empty() && name.len() <= 64);
        while self.masks.len() > crate::smart::MAX_CHANNELS {
            let last = self.masks.keys().next_back().cloned().expect("not empty");
            self.masks.remove(&last);
        }
        self.decals.truncate(crate::smart::MAX_DECALS);
        for d in &mut self.decals {
            d.sanitize();
        }
        self.detail_cell = self.detail_cell.filter(|c| c.is_finite() && *c >= MIN_DETAIL_CELL);
        for b in &mut self.blobs {
            b.bone = b.bone.trim().chars().take(64).collect();
            b.material = b.material.trim().chars().take(64).collect();
        }
    }

    /// Finds a blob by index (number) or name (string).
    pub fn find(&self, key: &serde_json::Value) -> Option<usize> {
        match key {
            serde_json::Value::Number(n) => n.as_u64().map(|i| i as usize).filter(|&i| i < self.blobs.len()),
            serde_json::Value::String(s) => self.blobs.iter().position(|b| &b.name == s),
            _ => None,
        }
    }
}

/// Detail-layer grid points per brick side.
const DB: i32 = 8;
pub(crate) const BRICK_VALUES: usize = (DB * DB * DB) as usize;
/// More detail bricks than this in one level are refused (and dropped
/// from loaded files).
pub const MAX_BRICKS: usize = 131_072;
/// Most bricks over every level together (about 512 MB of offsets).
pub const MAX_TOTAL_BRICKS: usize = 262_144;
/// Most refined detail regions on one clay.
pub const MAX_REGIONS: usize = 16;

/// A part of the clay that keeps its sculpt detail on a finer grid than
/// the rest (a face, the hands, a dragon's head): an ellipsoid in object
/// space and the cell size inside it. Outside, the detail fades back to
/// the coarser grid over a band of a few coarse cells, so the surface
/// stays continuous.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Region {
    #[serde(default)]
    pub name: String,
    pub center: Vec3,
    /// Half sizes along X, Y, Z (`radius` sets all three).
    #[serde(default, alias = "radius", deserialize_with = "radii")]
    pub radii: Vec3,
    pub cell: f32,
}

fn radii<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec3, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum R { One(f32), Three([f32; 3]) }
    Ok(match R::deserialize(d)? { R::One(r) => Vec3::splat(r), R::Three(r) => Vec3::from(r) })
}

impl Region {
    pub(crate) fn validate(&self, coarse: f32) -> Result<(), String> {
        let name = if self.name.is_empty() { "a detail region".to_string() } else { format!("detail region {}", self.name) };
        if !self.center.is_finite() || !self.radii.is_finite() || self.radii.min_element() <= 0.0 {
            return Err(format!("{name} needs a finite center and radii > 0"));
        }
        if !self.cell.is_finite() || self.cell < MIN_DETAIL_CELL {
            return Err(format!("{name}: cell must be at least {MIN_DETAIL_CELL}"));
        }
        if self.cell >= coarse * 0.99 {
            return Err(format!("{name}: cell {} must be finer than the clay's detail_cell {coarse}", self.cell));
        }
        if self.radii.max_element() / self.cell > 20_000.0 {
            return Err(format!("{name} is too big for its cell"));
        }
        Ok(())
    }

    /// About the distance outside the ellipsoid (negative inside), in
    /// scene units; never more than the true distance.
    pub(crate) fn distance(&self, p: Vec3) -> f32 {
        let k = ((p - self.center) / self.radii).length();
        (k - 1.0) * self.radii.min_element()
    }
}

/// One refined level: a region and its own grid. The grid holds what the
/// level adds to the coarser levels (a difference), so a new region starts
/// empty and only stores blocks where finer sculpting changed something.
/// Inside the region the difference counts fully, fading out over `band`.
#[derive(Clone, Debug)]
pub struct Level {
    pub region: Region,
    /// Width of the fade outside the region.
    pub band: f32,
    pub grid: DetailGrid,
}

impl Level {
    /// How much this level counts at `p`: 1 inside, fading to 0 at `band`.
    pub(crate) fn weight(&self, p: Vec3) -> f32 {
        let d = self.region.distance(p);
        if d <= 0.0 {
            1.0
        } else if d >= self.band {
            0.0
        } else {
            let t = d / self.band;
            1.0 - t * t * (3.0 - 2.0 * t)
        }
    }

    /// Box holding every point where `weight` > 0, plus `pad`.
    pub(crate) fn extent(&self, pad: f32) -> (Vec3, Vec3) {
        let r = &self.region;
        let half = r.radii * (1.0 + self.band / r.radii.min_element()) + pad;
        (r.center - half, r.center + half)
    }
}

/// The fade width for a level refined from a grid of `coarse` cells.
fn band_for(coarse: f32, region: &Region) -> f32 {
    (6.0 * coarse).max(4.0 * region.cell).min(region.radii.min_element())
}

/// The sculpt layer: field offsets on a fine grid of `voxel` spacing, stored
/// as sparse 8x8x8 bricks. Grid point `i` sits at `i * voxel` in object space.
/// `levels` refine it inside regions (see `Region`).
///
/// Shared with copy-on-write (`grid_mut`), and compared by identity, so the
/// per-frame undo snapshot and the mesh cache stay cheap however much
/// sculpting there is. Every edit makes a new copy, so identity changes;
/// bricks are shared between copies until written.
#[derive(Clone, Debug)]
pub struct Detail(Arc<DetailGrid>);

#[derive(Clone, Debug, Default)]
pub struct DetailGrid {
    pub voxel: f32,
    bricks: HashMap<[i32; 3], Arc<[f32; BRICK_VALUES]>>,
    /// Finer levels, coarsest first; finer ones win where they overlap.
    levels: Vec<Level>,
}

impl PartialEq for Detail {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Detail {
    /// See `DetailGrid::mirrored_x`.
    pub fn mirrored_x(&self) -> Detail {
        Detail(Arc::new(self.0.mirrored_x()))
    }

    pub fn new(voxel: f32) -> Self {
        Detail(Arc::new(DetailGrid { voxel, bricks: HashMap::new(), levels: Vec::new() }))
    }

    pub fn voxel(&self) -> f32 {
        self.0.voxel
    }

    /// The finest cell anywhere (the base voxel without regions).
    pub fn finest(&self) -> f32 {
        self.0.levels.iter().map(|l| l.grid.voxel).fold(self.0.voxel, f32::min)
    }

    pub fn grid(&self) -> &DetailGrid {
        &self.0
    }

    pub fn grid_mut(&mut self) -> &mut DetailGrid {
        Arc::make_mut(&mut self.0)
    }

    fn is_sane(&self) -> bool {
        let sane = |g: &DetailGrid| {
            g.voxel.is_finite() && (1e-5..=1e3).contains(&g.voxel) && g.bricks.len() <= MAX_BRICKS
                && g.bricks.keys().all(|k| k.iter().all(|c| c.abs() < 1 << 22))
        };
        let g = &self.0;
        sane(g) && g.levels.len() <= MAX_REGIONS && g.total_bricks() <= MAX_TOTAL_BRICKS
            && g.levels.iter().all(|l| sane(&l.grid) && l.grid.levels.is_empty() && l.region.validate(g.voxel).is_ok() && l.band.is_finite() && l.band > 0.0)
    }
}

pub(crate) fn split(i: [i32; 3]) -> ([i32; 3], usize) {
    let key = i.map(|c| c.div_euclid(DB));
    let l = i.map(|c| c.rem_euclid(DB) as usize);
    (key, l[0] + DB as usize * (l[1] + DB as usize * l[2]))
}

impl DetailGrid {
    /// A grid point's offset on this level (not the finer levels).
    pub fn get(&self, i: [i32; 3]) -> f32 {
        let (key, at) = split(i);
        self.bricks.get(&key).map_or(0.0, |b| b[at])
    }

    /// Sets a grid point on this level.
    pub fn set(&mut self, i: [i32; 3], v: f32) {
        let (key, at) = split(i);
        if v == 0.0 && !self.bricks.contains_key(&key) {
            return;
        }
        Arc::make_mut(self.bricks.entry(key).or_insert_with(|| Arc::new([0.0; BRICK_VALUES])))[at] = v;
    }

    /// Bricks on this level.
    pub fn brick_count(&self) -> usize {
        self.bricks.len()
    }

    /// Bricks on every level.
    pub fn total_bricks(&self) -> usize {
        self.bricks.len() + self.levels.iter().map(|l| l.grid.bricks.len()).sum::<usize>()
    }

    /// The refined levels, coarsest first.
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// The same offsets mirrored across x = 0 (grid point x -> -x), finer
    /// levels and their regions too.
    pub fn mirrored_x(&self) -> DetailGrid {
        let mut out = DetailGrid { voxel: self.voxel, bricks: HashMap::new(), levels: Vec::new() };
        for (key, brick) in &self.bricks {
            for (at, &v) in brick.iter().enumerate().filter(|(_, v)| **v != 0.0) {
                let l = [at as i32 % DB, at as i32 / DB % DB, at as i32 / (DB * DB)];
                out.set([-(key[0] * DB + l[0]), key[1] * DB + l[1], key[2] * DB + l[2]], v);
            }
        }
        for l in &self.levels {
            let mut region = l.region.clone();
            region.center.x = -region.center.x;
            region.name = crate::rig::mirror_name(&region.name).unwrap_or(region.name);
            out.levels.push(Level { region, band: l.band, grid: l.grid.mirrored_x() });
        }
        out
    }

    pub(crate) fn levels_mut(&mut self) -> &mut Vec<Level> {
        &mut self.levels
    }

    /// The largest absolute value in each brick of this level, by brick key.
    pub fn brick_max_abs(&self) -> HashMap<[i32; 3], f32> {
        self.bricks.iter().map(|(k, b)| (*k, b.iter().fold(0.0f32, |m, v| m.max(v.abs())))).collect()
    }

    /// The largest absolute offset there can be anywhere (levels add up).
    pub fn max_abs(&self) -> f32 {
        let own = self.bricks.values().flat_map(|b| b.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
        own + self.levels.iter().map(|l| l.grid.max_abs()).sum::<f32>()
    }

    /// Drops bricks that hold only zeros.
    pub fn prune(&mut self) {
        self.bricks.retain(|_, b| b.iter().any(|v| v.abs() > 1e-7));
    }

    /// The same offsets on a grid of `voxel` spacing (refined levels keep
    /// their own cells, and drop out when no longer finer).
    pub fn resampled(&self, voxel: f32) -> Result<DetailGrid, String> {
        let mut out = DetailGrid { voxel, bricks: HashMap::new(), levels: Vec::new() };
        for &key in self.bricks.keys() {
            let (lo, hi) = self.brick_box(key);
            let (i0, i1) = ((lo / voxel).floor().as_ivec3(), (hi / voxel).ceil().as_ivec3());
            for z in i0.z..=i1.z {
                for y in i0.y..=i1.y {
                    for x in i0.x..=i1.x {
                        let v = self.level_offset(Vec3::new(x as f32, y as f32, z as f32) * voxel);
                        if v.abs() > 1e-7 {
                            out.set([x, y, z], v);
                        }
                    }
                }
            }
            if out.bricks.len() > MAX_BRICKS {
                return Err(format!("detail_cell {voxel} needs more than {MAX_BRICKS} sculpt blocks; use a larger cell"));
            }
        }
        for l in &self.levels {
            if l.region.validate(voxel).is_ok() {
                out.levels.push(Level { band: band_for(voxel, &l.region), ..l.clone() });
            }
        }
        Ok(out)
    }

    /// Trilinear offset at an object-space point, with every refined level.
    pub fn offset(&self, p: Vec3) -> f32 {
        self.parent(self.levels.len(), p)
    }

    /// The offset at `p` from the coarse grid and the first `k` levels
    /// (what level `k` adds its difference to).
    pub fn parent(&self, k: usize, p: Vec3) -> f32 {
        let mut v = self.level_offset(p);
        for l in &self.levels[..k.min(self.levels.len())] {
            let w = l.weight(p);
            if w > 0.0 {
                v += l.grid.level_offset(p) * w;
            }
        }
        v
    }

    /// Level `k`'s whole offset at its grid point `i` (the coarse grid for
    /// None): what sculpting reads and writes there.
    pub fn full(&self, k: Option<usize>, i: [i32; 3]) -> f32 {
        match k {
            None => self.get(i),
            Some(k) => {
                let l = &self.levels[k];
                self.parent(k, Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * l.grid.voxel) + l.grid.get(i)
            }
        }
    }

    /// Trilinear offset on this level alone.
    pub fn level_offset(&self, p: Vec3) -> f32 {
        if self.bricks.is_empty() {
            return 0.0;
        }
        let g = p / self.voxel;
        let f = g.floor();
        let t = g - f;
        let i = f.as_ivec3();
        // All eight corners usually sit in one brick: look it up once.
        let (key, _) = split(i.to_array());
        let local = i - glam::IVec3::from(key) * DB;
        let mut v = [0.0f32; 8];
        if local.max_element() < DB - 1 {
            let Some(b) = self.bricks.get(&key) else { return 0.0 };
            let base = (local.x + DB * (local.y + DB * local.z)) as usize;
            for (n, slot) in v.iter_mut().enumerate() {
                *slot = b[base + (n & 1) + DB as usize * ((n >> 1) & 1) + (DB * DB) as usize * ((n >> 2) & 1)];
            }
        } else {
            for (n, slot) in v.iter_mut().enumerate() {
                let c = [i.x + (n & 1) as i32, i.y + ((n >> 1) & 1) as i32, i.z + ((n >> 2) & 1) as i32];
                *slot = self.get(c);
            }
        }
        let x0 = v[0] + (v[1] - v[0]) * t.x;
        let x1 = v[2] + (v[3] - v[2]) * t.x;
        let x2 = v[4] + (v[5] - v[4]) * t.x;
        let x3 = v[6] + (v[7] - v[6]) * t.x;
        let y0 = x0 + (x1 - x0) * t.y;
        let y1 = x2 + (x3 - x2) * t.y;
        y0 + (y1 - y0) * t.z
    }

    /// The cell a mesh needs at `p` to show the detail there: the finest
    /// level with sculpt near `p` (infinite where there is none).
    pub fn wanted_cell(&self, p: Vec3) -> f32 {
        let near = |g: &DetailGrid, reach: f32| g.level_touches(p - reach * g.voxel, p + reach * g.voxel);
        let mut c = if near(self, 2.0) { self.voxel } else { f32::INFINITY };
        // A level stores only where it differs from the coarser ones (flat
        // skin between pores stores nothing): reach a block around it, so
        // the mesh changes size only where the surface is smooth.
        for l in &self.levels {
            if l.grid.voxel < c && l.weight(p) > 0.0 && near(&l.grid, DB as f32) {
                c = l.grid.voxel;
            }
        }
        c
    }

    /// The finest cell whose level counts at `p`, blended across fades.
    pub fn cell_at(&self, p: Vec3) -> f32 {
        let mut c = self.voxel;
        for l in &self.levels {
            let w = l.weight(p);
            if w > 0.0 {
                c += (l.grid.voxel.min(c) - c) * w;
            }
        }
        c
    }

    fn brick_box(&self, key: [i32; 3]) -> (Vec3, Vec3) {
        let lo = Vec3::new(key[0] as f32, key[1] as f32, key[2] as f32) * (DB as f32 * self.voxel);
        (lo - self.voxel, lo + Vec3::splat(DB as f32 * self.voxel))
    }

    /// Does any brick, on any level, overlap the box?
    fn touches(&self, lo: Vec3, hi: Vec3) -> bool {
        self.level_touches(lo, hi) || self.levels.iter().any(|l| {
            let (a, b) = l.extent(0.0);
            a.cmple(hi).all() && b.cmpge(lo).all() && l.grid.level_touches(lo, hi)
        })
    }

    fn level_touches(&self, lo: Vec3, hi: Vec3) -> bool {
        if self.bricks.is_empty() {
            return false;
        }
        let size = DB as f32 * self.voxel;
        let (a, b) = (((lo - self.voxel) / size).floor().as_ivec3(), (hi / size).floor().as_ivec3());
        let n = (b - a + 1).max(glam::IVec3::ZERO);
        if (n.x as i64) * (n.y as i64) * (n.z as i64) <= 64 {
            for z in a.z..=b.z {
                for y in a.y..=b.y {
                    for x in a.x..=b.x {
                        if self.bricks.contains_key(&[x, y, z]) {
                            return true;
                        }
                    }
                }
            }
            return false;
        }
        self.bricks.keys().any(|k| {
            let (blo, bhi) = self.brick_box(*k);
            blo.cmple(hi).all() && bhi.cmpge(lo).all()
        })
    }

    /// Bounds of bricks that can add material outside the blobs.
    fn solid_bounds(&self) -> Option<(Vec3, Vec3)> {
        let pad = Vec3::splat(2.0 * self.voxel);
        let own = self.bricks
            .iter()
            .filter(|(_, b)| b.iter().any(|v| *v < -2.0 * self.voxel))
            .map(|(k, _)| {
                let (lo, hi) = self.brick_box(*k);
                (lo - pad, hi + pad)
            });
        let levels: Vec<(Vec3, Vec3)> = self.levels.iter().filter_map(|l| l.grid.solid_bounds()).collect();
        own.chain(levels).reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// Replaces the refined regions. A region already there (same name,
    /// place and cell) keeps its detail; a new one starts empty (the
    /// surface does not change). A removed region's finer detail goes, but
    /// nothing else: every stroke also landed on the coarse grid.
    pub(crate) fn set_regions(&mut self, regions: &[Region]) -> Result<(), String> {
        if regions.len() > MAX_REGIONS {
            return Err(format!("at most {MAX_REGIONS} detail regions"));
        }
        for r in regions {
            r.validate(self.voxel)?;
        }
        // A level that goes away takes its finer detail with it; the coarse
        // grid holds its own version of every stroke.
        let mut kept: Vec<Level> = Vec::new();
        for r in regions {
            if let Some(l) = self.levels.iter().find(|l| &l.region == r) {
                kept.push(l.clone());
                continue;
            }
            // Empty: it adds nothing until something finer is sculpted.
            let level = Level { band: band_for(self.voxel, r), region: r.clone(), grid: DetailGrid { voxel: r.cell, ..Default::default() } };
            kept.push(level);
        }
        // Coarse first, so finer cells win where regions overlap.
        kept.sort_by(|a, b| b.region.cell.total_cmp(&a.region.cell));
        self.levels = kept;
        if self.total_bricks() > MAX_TOTAL_BRICKS {
            return Err(format!("detail regions need more than {MAX_TOTAL_BRICKS} sculpt blocks in all; use smaller regions or larger cells"));
        }
        Ok(())
    }
}

/// One level of a detail grid as brushes see it: whole offsets at its grid
/// points (a refined level's difference plus the coarser levels under it).
pub(crate) struct LevelView<'a> {
    pub top: &'a DetailGrid,
    pub level: Option<usize>,
    /// Coarse grid: the largest offset in each brick; levels: one bound.
    bricks: HashMap<[i32; 3], f32>,
    most: f32,
}

impl<'a> LevelView<'a> {
    pub(crate) fn new(top: &'a DetailGrid, level: Option<usize>) -> Self {
        match level {
            None => LevelView { top, level, bricks: top.brick_max_abs(), most: 0.0 },
            Some(_) => LevelView { top, level, bricks: HashMap::new(), most: top.max_abs() },
        }
    }

    pub(crate) fn voxel(&self) -> f32 {
        self.level.map_or(self.top.voxel, |k| self.top.levels[k].grid.voxel)
    }

    pub(crate) fn get(&self, i: [i32; 3]) -> f32 {
        self.top.full(self.level, i)
    }

    /// The largest offset around grid points `lo..=hi`: how far sculpting
    /// can have moved the surface there.
    pub(crate) fn carved(&self, lo: glam::IVec3, hi: glam::IVec3) -> f32 {
        if self.level.is_some() {
            return self.most;
        }
        let (b0, b1) = (lo.div_euclid(glam::IVec3::splat(DB)), hi.div_euclid(glam::IVec3::splat(DB)));
        let mut m = 0.0f32;
        for z in b0.z..=b1.z {
            for y in b0.y..=b1.y {
                for x in b0.x..=b1.x {
                    m = m.max(self.bricks.get(&[x, y, z]).copied().unwrap_or(0.0));
                }
            }
        }
        m
    }
}

/// File form: offsets as little-endian i16 in 1/256 voxel steps, base64.
#[derive(Serialize, Deserialize)]
struct DetailFile {
    voxel: f32,
    bricks: Vec<BrickFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    levels: Vec<LevelFile>,
}

#[derive(Serialize, Deserialize)]
struct LevelFile {
    region: Region,
    band: f32,
    bricks: Vec<BrickFile>,
}

#[derive(Serialize, Deserialize)]
struct BrickFile {
    at: [i32; 3],
    data: String,
}

/// Offsets are kept within this many voxels so the file form is lossless enough.
pub const MAX_OFFSET_VOXELS: f32 = 127.0;

fn bricks_file(g: &DetailGrid) -> Vec<BrickFile> {
    let mut keys: Vec<&[i32; 3]> = g.bricks.keys().collect();
    keys.sort();
    let b64 = base64::engine::general_purpose::STANDARD;
    keys.into_iter()
        .map(|k| {
            let bytes: Vec<u8> = g.bricks[k]
                .iter()
                .flat_map(|v| ((v / g.voxel * 256.0).round().clamp(-32767.0, 32767.0) as i16).to_le_bytes())
                .collect();
            BrickFile { at: *k, data: b64.encode(bytes) }
        })
        .collect()
}

fn bricks_read<E: serde::de::Error>(file: Vec<BrickFile>, voxel: f32) -> Result<HashMap<[i32; 3], Arc<[f32; BRICK_VALUES]>>, E> {
    if file.len() > MAX_BRICKS {
        return Err(E::custom("too many detail bricks"));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut bricks = HashMap::new();
    for b in file {
        let bytes = b64.decode(&b.data).map_err(E::custom)?;
        if bytes.len() != BRICK_VALUES * 2 {
            return Err(E::custom("bad detail brick size"));
        }
        let mut values = [0.0f32; BRICK_VALUES];
        for (v, pair) in values.iter_mut().zip(bytes.chunks_exact(2)) {
            *v = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 256.0 * voxel;
        }
        bricks.insert(b.at, Arc::new(values));
    }
    Ok(bricks)
}

impl Serialize for Detail {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let g = &self.0;
        let levels = g.levels.iter().map(|l| LevelFile { region: l.region.clone(), band: l.band, bricks: bricks_file(&l.grid) }).collect();
        DetailFile { voxel: g.voxel, bricks: bricks_file(g), levels }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Detail {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let file = DetailFile::deserialize(d)?;
        if file.levels.len() > MAX_REGIONS {
            return Err(serde::de::Error::custom("too many detail regions"));
        }
        let bricks = bricks_read(file.bricks, file.voxel)?;
        let mut levels = Vec::new();
        for l in file.levels {
            let grid = DetailGrid { voxel: l.region.cell, bricks: bricks_read(l.bricks, l.region.cell)?, levels: Vec::new() };
            levels.push(Level { region: l.region, band: l.band, grid });
        }
        Ok(Detail(Arc::new(DetailGrid { voxel: file.voxel, bricks, levels })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_cover_hides_a_small_bump_and_keeps_zero_compatible() {
        let mut c: Clay = serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"box","center":[0,-0.3,0],"size":[1.2,0.6,1.2],"blend":0},
            {"type":"sphere","center":[0,0,0],"radius":0.04,"blend":0},
            {"type":"box","center":[0,0,0],"size":[1.0,0.6,1.0],"op":"cover","offset":0.01,"blend":0,"name":"shirt"}
        ], "resolution":96})).unwrap();
        let top = |c: &Clay,x:f32| {
            let f = Field::new(c,0.005);
            let list = f.near(Vec3::splat(-1.0),Vec3::splat(1.0));
            let (mut lo,mut hi) = (-0.1,0.3);
            for _ in 0..24 { let y = (lo+hi)*0.5; if f.sample(Vec3::new(x,y,0.0),&list).0 < 0.0 { lo=y; } else { hi=y; } }
            (lo+hi)*0.5
        };
        let exact = top(&c,0.0) - top(&c,0.18);
        assert!(exact > 0.03);
        let before = c.build_mesh();
        c.blobs[2].smooth = 0.0;
        assert_eq!(before.indices,c.build_mesh().indices);
        c.blobs[2].smooth = 0.12;
        let loose = top(&c,0.0) - top(&c,0.18);
        assert!(loose.abs() < exact * 0.3,"exact {exact}, loose {loose}");
        assert!(top(&c,0.0) > 0.04,"must enclose the body");
        let f = Field::new(&c,0.005);
        assert!(f.nearest_named(Vec3::new(0.1,top(&c,0.1),0.0), &["shirt".into()]));
        let alias: Blob = serde_json::from_value(serde_json::json!({"type":"sphere","center":[0,0,0],"radius":1,"op":"cover","loose":0.03})).unwrap();
        assert_eq!(alias.smooth,0.03);
    }

    #[test]
    fn chain_has_beads_on_a_bent_path_with_twist_taper_and_mirror() {
        let blob: Blob = serde_json::from_value(serde_json::json!({
            "type":"chain","points":[[0,0,0],[0,0.4,0],[0.4,0.4,0]],
            "size":[0.12,0.16,0.08],"twist":90,"taper":0.5,"mirror":true,"blend":0
        })).unwrap();
        let c = Clay {blobs:vec![blob.clone()],resolution:96,..Default::default()};
        let f = Field::new(&c,0.005);
        let list = f.near(Vec3::splat(-1.0),Vec3::splat(1.0));
        for p in [Vec3::ZERO,Vec3::new(0.0,0.2,0.0),Vec3::new(0.2,0.4,0.0),Vec3::new(-0.4,0.4,0.0)] {
            assert!(f.sample(p,&list).0 < 0.0,"missing bead/core at {p}");
        }
        assert!(f.sample(Vec3::new(0.2,0.0,0.0),&list).0 > 0.0);
        assert!(!c.build_mesh().indices.is_empty());
        assert!(!c.thin_blobs(0.012).is_empty(), "warn when the connecting core is too thin");
        let restored: Blob = serde_json::from_value(serde_json::to_value(&blob).unwrap()).unwrap();
        assert_eq!(blob,restored);
        assert!(serde_json::from_value::<Blob>(serde_json::json!({"type":"chain","points":[[0,0,0]],"size":[1,1,1]})).is_err());
    }

    fn sphere(r: f32) -> Blob {
        serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": r })).unwrap()
    }

    fn membrane() -> Blob {
        serde_json::from_value(serde_json::json!({"type":"membrane", "points":[[-1,-1,0],[1,-1,0],[1,1,0],[-1,1,0]], "thickness":0.1, "blend":0})).unwrap()
    }

    #[test]
    fn membrane_distances_round_edges_and_cut_scallops() {
        let blob = membrane();
        let (geo, lo, hi) = Geo::new(&blob.form);
        assert!((geo.sdf(Vec3::ZERO) + 0.05).abs() < 1e-6);
        assert!((geo.sdf(Vec3::Z * 0.2) - 0.15).abs() < 1e-6);
        assert!((geo.sdf(Vec3::new(1.03, 0.0, 0.04))).abs() < 1e-6);
        assert!(lo.z <= -0.05 && hi.z >= 0.05);
        let mut form = blob.form;
        if let Form::Membrane { scallop, scallop_edges, .. } = &mut form {
            *scallop = 0.2;
            *scallop_edges = vec![1];
        }
        let (cut, _, _) = Geo::new(&form);
        assert!(cut.sdf(Vec3::new(0.9, 0.0, 0.0)) > 0.2);
        assert!(cut.sdf(Vec3::new(0.4, 0.0, 0.0)) < 0.0);
        assert!(cut.sdf(Vec3::new(1.0, -1.0, 0.0)) < 0.0);
    }

    #[test]
    fn membrane_mesh_is_a_closed_thin_sheet_with_sag_and_nonplanar_points() {
        let mut c = Clay { blobs: vec![membrane()], resolution: 64, ..Default::default() };
        let mesh = c.build_mesh();
        assert!(mesh.indices.len() > 1000);
        let (lo, hi) = mesh.bounds();
        assert!((hi.z - lo.z - 0.1).abs() < 0.02);
        assert!(mesh.vertices.iter().any(|v| v.normal[2] > 0.9));
        assert!(mesh.vertices.iter().any(|v| v.normal[2] < -0.9));
        let mut edges = HashMap::new();
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0],t[1]),(t[1],t[2]),(t[2],t[0])] {
                *edges.entry((a.min(b),a.max(b))).or_insert(0) += 1;
            }
        }
        assert!(edges.values().all(|&n| n == 2));
        assert!(c.thin_blobs(0.04).is_empty());
        assert!(c.thin_blobs(0.06)[0].contains("two grid cells"));
        if let Form::Membrane { sag, points, .. } = &mut c.blobs[0].form {
            *sag = 0.3;
            points[0].z = 0.05;
        }
        let bowed = c.build_mesh();
        assert!(bowed.bounds().1.z > 0.3);
        assert!(!bowed.vertices.is_empty());
        let (geo, lo, hi) = Geo::new(&c.blobs[0].form);
        // A distance field must not overstep the sheet even at its rim.
        let a = Vec3::new(0.2,0.1,0.7);
        let b = Vec3::new(-0.3,0.2,0.4);
        assert!((geo.sdf(a) - geo.sdf(b)).abs() <= a.distance(b) + 1e-5);
        assert!(bowed.vertices.iter().all(|v| Vec3::from(v.pos).cmpge(lo-0.02).all() && Vec3::from(v.pos).cmple(hi+0.02).all()));
    }

    #[test]
    fn sphere_mesh_has_the_right_radius() {
        let clay = Clay { blobs: vec![sphere(1.0)], resolution: 48, detail: None, paint: None, detail_cell: None, ..Default::default() };
        let mesh = clay.build_mesh();
        assert!(mesh.indices.len() > 1000);
        for v in &mesh.vertices {
            let r = Vec3::from(v.pos).length();
            assert!((r - 1.0).abs() < 0.05, "vertex at radius {r}");
            let n = Vec3::from(v.normal);
            assert!(n.dot(Vec3::from(v.pos).normalize()) > 0.9, "normal points out");
        }
        assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.vertices.len()));
        assert!(mesh.edges.len().is_multiple_of(2));
    }

    #[test]
    fn carve_removes_material_and_mirror_adds_it() {
        let mut clay = Clay { blobs: vec![sphere(1.0)], resolution: 40, detail: None, paint: None, detail_cell: None, ..Default::default() };
        let mut hole: Blob = serde_json::from_value(serde_json::json!({
            "type": "box", "center": [0.9, 0, 0], "size": [1.2, 3, 3], "op": "carve", "blend": 0, "mirror": true
        }))
        .unwrap();
        clay.blobs.push(hole.clone());
        let (lo, hi) = clay.build_mesh().bounds();
        assert!(hi.x < 0.45 && lo.x > -0.45, "both sides cut: {lo} {hi}");
        hole.mirror = false;
        clay.blobs[1] = hole;
        let (lo, hi) = clay.build_mesh().bounds();
        assert!(hi.x < 0.45 && lo.x < -0.9, "only +X cut: {lo} {hi}");
    }

    #[test]
    fn mirrored_add_is_symmetric() {
        let arm: Blob = serde_json::from_value(serde_json::json!({
            "type": "sphere", "center": [-1.0, 0, 0], "radius": 0.3, "mirror": true
        }))
        .unwrap();
        let (lo, hi) = Clay { blobs: vec![arm], resolution: 48, ..Clay::default() }.build_mesh().bounds();
        assert!((lo.x + 1.3).abs() < 0.05 && (hi.x - 1.3).abs() < 0.05, "{lo} {hi}");
    }

    #[test]
    fn blobs_blend_and_carry_color() {
        let blobs = serde_json::json!([
            { "type": "sphere", "center": [-0.6, 0, 0], "radius": 0.5, "color": [1, 0, 0] },
            { "name": "arm", "type": "capsule", "a": [0.2, 0, 0], "b": [1.2, 0, 0], "radius": 0.3, "radius_b": 0.1, "color": [0, 0, 1], "blend": 0.3 },
        ]);
        let clay = Clay { blobs: serde_json::from_value(blobs).unwrap(), resolution: 48, detail: None, paint: None, detail_cell: None, ..Default::default() };
        assert_eq!(clay.find(&serde_json::json!("arm")), Some(1));
        let mesh = clay.build_mesh();
        let (lo, hi) = mesh.bounds();
        assert!(lo.x < -1.0 && hi.x > 1.25);
        let left = mesh.vertices.iter().find(|v| v.pos[0] < -1.0).unwrap();
        assert!(left.color[0] > 0.9 && left.color[2] < 0.1);
        let right = mesh.vertices.iter().find(|v| v.pos[0] > 1.2).unwrap();
        assert!(right.color[2] > 0.9);
    }

    #[test]
    fn color_blend_zero_makes_a_crisp_color_edge_on_a_smooth_shape() {
        let make = |cb: Option<f32>| -> Clay {
            let mut c: Clay = serde_json::from_value(serde_json::json!({ "blobs": [
                { "type": "capsule", "a": [-1, 0, 0], "b": [0, 0, 0], "radius": 0.3, "color": [1, 0, 0] },
                { "type": "capsule", "a": [0, 0, 0], "b": [1, 0, 0], "radius": 0.3, "color": [0, 0, 1], "blend": 0.3 }
            ], "resolution": 64 }))
            .unwrap();
            c.blobs[1].color_blend = cb;
            c
        };
        // Vertices whose color is a mix (neither nearly red nor nearly blue).
        let mixed = |c: &Clay| c.build_mesh().vertices.iter().filter(|v| v.color[0] > 0.1 && v.color[2] > 0.1).count();
        let (soft, crisp) = (make(None), make(Some(0.0)));
        assert!(mixed(&soft) > 50, "{}", mixed(&soft));
        assert_eq!(mixed(&crisp), 0);
        // Same shape either way.
        assert_eq!(soft.build_mesh().vertices.len(), crisp.build_mesh().vertices.len());
    }

    #[test]
    fn a_cover_dresses_only_the_body_inside_its_shape() {
        let c: Clay = serde_json::from_value(serde_json::json!({ "blobs": [
            { "type": "capsule", "a": [0, -1, 0], "b": [0, 1, 0], "radius": 0.2, "blend": 0, "color": [1, 0.8, 0.7] },
            { "name": "shirt", "type": "box", "center": [0, 0.5, 0], "size": [2, 1, 2], "op": "cover", "offset": 0.03, "blend": 0.01, "color": [0, 0.5, 0] }
        ], "resolution": 96 })).unwrap();
        let m = c.build_mesh();
        let (lo, hi) = m.bounds();
        // Only 3 cm wider than the body, not the size of the box.
        assert!(hi.x < 0.24 && lo.x > -0.24, "{lo} {hi}");
        // Green where covered (above y = 0), skin below.
        let at = |y: f32| m.vertices.iter().filter(|v| (v.pos[1] - y).abs() < 0.02 && v.pos[0] > 0.1).map(|v| v.color).next().unwrap();
        assert!(at(0.5)[1] > 0.4 && at(0.5)[0] < 0.1);
        assert!(at(-0.5)[0] > 0.9);
    }

    #[test]
    fn json_round_trip_and_bad_input() {
        let clay = Clay { blobs: vec![sphere(0.5)], resolution: 32, detail: None, paint: None, detail_cell: None, ..Default::default() };
        let text = serde_json::to_string(&clay).unwrap();
        assert!(text.contains("\"type\":\"sphere\""));
        let back: Clay = serde_json::from_str(&text).unwrap();
        assert_eq!(back, clay);
        // Empty, NaN, and huge input give a mesh (maybe empty) without hanging.
        assert!(Clay::default().build_mesh().vertices.is_empty());
        let with_radius = |r: f32| Blob { form: Form::Sphere { center: Vec3::ZERO, radius: r }, ..sphere(1.0) };
        let nan = Clay { blobs: vec![with_radius(f32::NAN)], resolution: 32, detail: None, paint: None, detail_cell: None, ..Default::default() };
        let _ = nan.build_mesh();
        let huge = Clay { blobs: vec![with_radius(1e30)], resolution: 32, detail: None, paint: None, detail_cell: None, ..Default::default() };
        let _ = huge.build_mesh();
    }
}


