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
use std::sync::atomic::{AtomicUsize, Ordering};
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
pub(crate) mod base_cache;
pub(crate) mod chunks;
pub(crate) mod sculptris;
#[cfg(test)]
pub(crate) use hard_surface::path_turns;
pub(crate) use hard_surface::Corner;
#[cfg(test)]
pub(crate) use hard_surface::array_tests::tower;

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

#[cfg(test)]
thread_local! {
    /// Meshes this thread built whole with marching tetrahedra because
    /// one surface-nets edge joined more than two quads (see
    /// `build_window_cells`; the Stage 0 triangle-count finding).
    pub(crate) static TETRA_FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    /// The stroke ledgers behind `detail` and `paint` (see `ledger.rs`):
    /// every stroke as a replayable record. The grids stay where they
    /// are; a clay sculpted before ledgers existed has none until its
    /// next stroke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sculpt: Option<crate::ledger::SculptStack>,
    /// The persistent sculpt mask (see `mask.rs`; not `masks`, the smart
    /// materials' channels): brushes move each point by 1 - mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sculpt_mask: Option<crate::mask::SculptMask>,
    /// Polygroups (see `groups.rs`), with their names and visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<crate::groups::Groups>,
    /// The triangle meshes its `Form::Mesh` blobs name, by content hash
    /// (see `mesh_asset.rs`), so the clay saves and replays on its own.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "crate::mesh_asset::library")]
    pub assets: std::collections::BTreeMap<String, crate::mesh_asset::MeshAsset>,
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
        Self { blobs: Vec::new(), resolution: DEFAULT_RESOLUTION, detail: None, paint: None, masks: Default::default(), decals: Vec::new(), detail_cell: None, shell: None, sharp: true, sculpt: None, sculpt_mask: None, groups: None, assets: Default::default() }
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
    /// Keep only what is inside both this blob and everything before it
    /// (a smooth max over `blend`): booleans across objects (`to_clay`).
    Intersect,
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
    /// A triangle mesh (`mesh_asset.rs`) by content hash, kept in the
    /// clay's `assets`: imports, `to_clay` and InsertMesh. Placed at
    /// `center`, turned by `rotate` (degrees XYZ) and scaled uniformly.
    /// `sign: winding` reads open meshes (holes close).
    Mesh {
        asset: String,
        #[serde(default)]
        sign: crate::mesh_asset::Sign,
        #[serde(default)]
        center: Vec3,
        #[serde(default)]
        rotate: Vec3,
        #[serde(default = "one")]
        scale: f32,
    },
}

/// One array blob as copies of a single shape (see `Clay::instancing`).
#[derive(Debug)]
pub struct Instancing {
    /// The shape alone: the blob with the array's source as its form.
    pub unit: Clay,
    /// Where each copy goes in the clay's space: (translation, rotation).
    pub copies: Vec<(Vec3, glam::Quat)>,
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
    /// A plain blob of `form` (no blend, the defaults otherwise).
    pub fn of(form: Form, op: Op, color: [f32; 3]) -> Blob {
        Blob { name: String::new(), form, op, blend: 0.0, color, mirror: false, flip: false, material: String::new(), bone: String::new(), offset: default_offset(), smooth: 0.0, join: Default::default(), join_size: None, wall: default_offset(), rim: 0.0, ridge: 0.0, flare: 0.0, color_blend: None }
    }

    /// The mesh assets a blob names: its own `Form::Mesh`, or one an
    /// array or shell copies (the clay must keep those too).
    pub fn mesh_assets(&self) -> Vec<&str> {
        let mut out = Vec::new();
        let mut form = &self.form;
        loop {
            match form {
                Form::Mesh { asset, .. } => out.push(asset.as_str()),
                Form::Array { source, .. } | Form::Shell { source, .. } => {
                    form = source;
                    continue;
                }
                _ => {}
            }
            return out;
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        // Mesh forms (nested ones too) are checked in `hard_surface::validate`.
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

/// A clay's mesh assets by content hash (`Clay::assets`): all its
/// geometry reads.
pub(crate) type Assets = std::collections::BTreeMap<String, crate::mesh_asset::MeshAsset>;

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
    /// A `Form::Mesh`: the asset's field, placed.
    Imported { field: Arc<crate::mesh_asset::MeshField>, sign: crate::mesh_asset::Sign, c: Vec3, inv: Quat, scale: f32 },
}

impl Geo {
    /// A form's geometry and box; `Form::Mesh` reads its asset from
    /// `assets` (the clay's own) and nothing else.
    fn new(form: &Form, assets: &Assets) -> (Geo, Vec3, Vec3) {
        let pos = |v: f32| v.abs().max(1e-4);
        match *form {
            Form::Extrude { .. } | Form::Lathe { .. } | Form::Sweep { .. } | Form::Shell { .. } | Form::Array { .. } | Form::Band { .. } => {
                let g = hard_surface::Geometry::new(form, assets);
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
            Form::Mesh { ref asset, sign, center, rotate, scale } => {
                // The clay's own assets only (`Clay::assets`), so its
                // geometry depends on the clay alone; a blob naming one it
                // does not keep is a speck. Only field-sized assets
                // (`adopt_assets` and file reads keep to that).
                let Some(asset) = assets.get(asset).filter(|a| a.tris().len() <= crate::mesh_asset::MAX_FIELD_TRIANGLES) else {
                    return (Geo::Sphere { c: Vec3::ZERO, r: 1e-4 }, Vec3::splat(-1e-4), Vec3::splat(1e-4));
                };
                let field = asset.field();
                let scale = if scale.is_finite() { scale.abs().max(1e-6) } else { 1.0 };
                let rot = rotation(rotate);
                let (flo, fhi) = (field.lo, field.hi);
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for (x, y, z) in CORNERS {
                    let q = Vec3::new(if x == 0 { flo.x } else { fhi.x }, if y == 0 { flo.y } else { fhi.y }, if z == 0 { flo.z } else { fhi.z });
                    let p = center + rot * (q * scale);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                (Geo::Imported { field, sign, c: center, inv: rot.inverse(), scale }, lo, hi)
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

    /// `sdf` where it is below `bound`; elsewhere a value from `bound` up
    /// to it (see `hard_surface::Geometry::sdf_within`).
    fn sdf_within(&self, p: Vec3, bound: f32) -> f32 {
        match self {
            Geo::Hard(g) => g.sdf_within(p, bound),
            Geo::Imported { field, sign, c, inv, scale } => field.distance_within(*inv * (p - *c) / *scale, bound / *scale, *sign) * *scale,
            _ => self.sdf(p),
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
            Geo::Imported { ref field, sign, c, inv, scale } => field.distance(inv * (p - c) / scale, sign) * scale,
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
    /// The blob field memoized on a lattice (see `base_cache.rs`), when a
    /// caller attached one: lattice reads then cost a lookup.
    base: Option<Arc<base_cache::BaseCache>>,
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
            let (mut geo, mut lo, mut hi) = Geo::new(&blob.form, &clay.assets);
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
                let (mut geo2, _, _) = Geo::new(&blob.form, &clay.assets);
                if let Geo::Hard(ref mut g) = geo2 { g.band_depth(blob.wall); }
                let (a, b) = if blob.flip { (lo, hi) } else { (mlo, mhi) };
                prims.push(Prim { geo: geo2, op: blob.op, k, ck, offset, smooth, join: blob.join, wall: blob.wall.abs().max(1e-4), rim: blob.rim.max(0.0), ridge: blob.ridge, flare: blob.flare, envelope: None, name: blob.name.clone(), color, mat, bone: crate::rig::mirror_name(&blob.bone).unwrap_or_else(|| blob.bone.clone()), flip: !blob.flip, lo: a, hi: b });
            }
            let (a, b) = if blob.flip { (mlo, mhi) } else { (lo, hi) };
            prims.push(Prim { geo, op: blob.op, k, ck, offset, smooth, join: blob.join, wall: blob.wall.abs().max(1e-4), rim: blob.rim.max(0.0), ridge: blob.ridge, flare: blob.flare, envelope: None, name: blob.name.clone(), color, mat, bone: blob.bone.clone(), flip: blob.flip, lo: a, hi: b });
        }
        // Every enabled sculpt layer (`layers.rs`; the clay's own grid
        // without any).
        let detail = crate::layers::composed_detail(clay);
        // With a detail layer the limit must not depend on the mesh grid.
        let base = match &detail {
            Some(d) => 4.0 * d.voxel(),
            None => 2.0 * voxel,
        };
        // Covers read the field up to their offset away from the surface.
        let mut field = Field { prims, max_k, limit: base + clay.decals.iter().map(|d|crate::damage::depth(d).abs()*3.0 + if d.hinge.is_some() {d.size*0.5*d.angle.to_radians().sin().abs()}else{0.0}).fold(0.0,f32::max) + 2.0 * max_k + max_offset + max_smooth + clay.shell.unwrap_or(0.0) + clay.blobs.iter().filter(|b| matches!(b.op, Op::Plate | Op::Engrave | Op::Emboss)).map(|b| b.wall).fold(0.0, f32::max), detail, keep: None, shell: clay.shell, damage: clay.decals.iter().filter(|d|d.shape.code()>=5 && (crate::damage::depth(d)!=0.0 || d.hinge.is_some())).cloned().collect(), base: None };
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

    /// Swaps the detail layer the field adds (the sculpt engine's next dab
    /// reads the grid the last one wrote). `limit` stays: it depends only on
    /// whether there is a detail layer, and both have one.
    pub(crate) fn set_detail(&mut self, detail: Detail) {
        debug_assert!(self.detail.as_ref().is_none_or(|d| d.voxel() == detail.voxel()));
        self.detail = Some(detail);
    }

    /// Lets go of the detail layer (until `set_detail`), so the sculpt
    /// engine can write the grid without copying it.
    pub(crate) fn take_detail(&mut self) -> Option<Detail> {
        self.detail.take()
    }

    pub(crate) fn detail_mut(&mut self) -> Option<&mut Detail> {
        self.detail.as_mut()
    }

    pub(crate) fn detail(&self) -> Option<&Detail> {
        self.detail.as_ref()
    }

    /// Reads the blob field at lattice points through `cache` (made for
    /// this field: `base_cache::for_clay`).
    pub(crate) fn with_base(mut self, cache: Arc<base_cache::BaseCache>) -> Field {
        self.base = Some(cache);
        self
    }

    /// `sample_base` at lattice points `(i0 + (x, y, z)) * voxel`, x
    /// fastest, into `out`: from the attached cache when it has this
    /// lattice, else worked out with `near` (the same values either way).
    pub(crate) fn base_block(&self, i0: glam::IVec3, n: glam::IVec3, voxel: f32, near: &[u32], out: &mut [f32]) {
        if let Some(c) = self.base.as_ref().filter(|c| c.voxel == voxel && self.keep.is_none()) {
            return c.block(self, i0, n, out);
        }
        let (nx, ny) = (n.x as usize, n.y as usize);
        par_chunks(out, nx * ny, |z, layer| {
            for y in 0..ny {
                for x in 0..nx {
                    layer[x + nx * y] = self.sample_base((i0 + glam::IVec3::new(x as i32, y as i32, z as i32)).as_vec3() * voxel, near).0;
                }
            }
        });
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
                // Box-to-box gap. An intersect blob matters everywhere:
                // far from it nothing is left.
                let gap = (p.lo - hi).max(lo - p.hi).max(Vec3::ZERO).length();
                gap <= reach || p.op == Op::Intersect
            })
            .collect()
    }

    /// `near(lo, hi)` into `out`, testing only `from` (a `near` list of a
    /// box holding lo..hi, so a superset in the same order): the same list
    /// `near` gives, without walking every prim or allocating.
    pub(crate) fn near_from(&self, lo: Vec3, hi: Vec3, from: &[u32], out: &mut Vec<u32>) {
        let reach = self.reach();
        out.clear();
        out.extend(from.iter().copied().filter(|&i| {
            let p = &self.prims[i as usize];
            let gap = (p.lo - hi).max(lo - p.hi).max(Vec3::ZERO).length();
            gap <= reach || p.op == Op::Intersect
        }));
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
            if !matches!(prim.op, Op::Carve | Op::Intersect) && box_distance(p, prim.lo, prim.hi) > limit + prim.k {
                continue;
            }
            let q = if prim.flip { Vec3::new(-p.x, p.y, p.z) } else { p };
            // Every op below leaves `d` (within +-limit) as it is once the
            // blob is limit + k away, so nearer than that is all that counts.
            let db = prim.geo.sdf_within(q, limit + prim.k);
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
                Op::Intersect => {
                    if k <= 1e-6 {
                        d = d.max(db);
                    } else {
                        // Smooth max (as carve's, with the blob not inverted).
                        let h = (0.5 - 0.5 * (db - d) / k).clamp(0.0, 1.0);
                        d = if prim.join == hard_surface::Join::Chamfer { d.max(db).max((d + db + k) * std::f32::consts::FRAC_1_SQRT_2) } else { db * (1.0 - h) + d * h + k * h * (1.0 - h) };
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
            // Only whether a blob is within far + k matters (a little more,
            // so a value of at least the bound still counts as beyond it).
            let d = prim.geo.sdf_within(q, (far + prim.k) * 1.001 + 1e-6);
            match prim.op {
                Op::Add => {
                    nearest = nearest.min(d - prim.k);
                    solid = solid.min(d - prim.k);
                }
                // A cover's surface lies inside its own shape.
                Op::Cover | Op::Plate | Op::Emboss => nearest = nearest.min(d - prim.k),
                // A carve within reach could open the inside to the surface.
                Op::Carve | Op::Engrave => carve_near |= d < far + prim.k,
                // Outside it nothing stays; inside, its surface may cut.
                Op::Intersect => {
                    nearest = nearest.min(d - prim.k);
                    carve_near = true;
                }
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

    /// `sample` without the clamp to a few cells: the unclamped blobs
    /// plus sculpt detail (curvature over a footprint wider than the band).
    pub(crate) fn surface_distance(&self, p: Vec3, list: &[u32]) -> f32 {
        self.wear_distance(p, list) + self.detail.as_ref().map_or(0.0, |g| g.grid().offset(p))
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

    /// The name (or index) of the blob that makes the surface nearest
    /// `p`, as masks see ownership (`nearest_named`).
    pub(crate) fn owner_name(&self, p: Vec3) -> Option<String> {
        let mut list = self.near(p - self.limit, p + self.limit);
        if list.is_empty() { list.extend(0..self.prims.len() as u32); }
        let i = self.prefix_full(p, &list, true, true, f32::MAX / 1024.0).3?;
        Some(if self.prims[i].name.is_empty() { i.to_string() } else { self.prims[i].name.clone() })
    }

    /// The prim that makes the surface nearest `p` (ownership as
    /// `owner_name` sees it).
    pub(crate) fn owner_index(&self, p: Vec3) -> Option<usize> {
        let mut list = self.near(p - self.limit, p + self.limit);
        if list.is_empty() { list.extend(0..self.prims.len() as u32); }
        self.prefix_full(p, &list, true, true, f32::MAX / 1024.0).3
    }

    /// A prim's blob name (mirrored copies of a blob share it).
    pub(crate) fn prim_name(&self, i: usize) -> String {
        self.prims.get(i).map(|p| p.name.clone()).unwrap_or_default()
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

/// Helper threads running `par_chunks` work, over all callers: builds of
/// several objects at once share the cores instead of each taking them all.
static PAR_HELPERS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Most threads a `par_chunks` call made on this thread may use.
    static PAR_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// Runs `f` with `par_chunks` calls made on this thread limited to `n`
/// threads (1: no other thread). Results never depend on the count.
#[cfg(test)]
pub(crate) fn with_threads<R>(n: usize, f: impl FnOnce() -> R) -> R {
    let old = PAR_LIMIT.replace(n.max(1));
    let r = f();
    PAR_LIMIT.set(old);
    r
}

/// Runs `f` on every chunk of `data` (with its index), on this thread and
/// as many helpers as there are free cores. What `f` does to a chunk must
/// not depend on which thread runs it.
pub(crate) fn par_chunks<T: Send>(data: &mut [T], chunk: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let chunk = chunk.max(1);
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let wanted = cores.min(data.len().div_ceil(chunk)).min(PAR_LIMIT.get()).saturating_sub(1);
    // Take what other calls leave free (this thread is already running).
    let mut helpers = 0;
    let _ = PAR_HELPERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |busy| {
        helpers = wanted.min(cores.saturating_sub(busy + 1));
        Some(busy + helpers)
    });
    /// Gives the helpers back, even when `f` panics.
    struct Release(usize);
    impl Drop for Release {
        fn drop(&mut self) {
            PAR_HELPERS.fetch_sub(self.0, Ordering::AcqRel);
        }
    }
    let _release = Release(helpers);
    let work = Mutex::new(data.chunks_mut(chunk).enumerate());
    let run = || loop {
        let next = work.lock().expect("no panics while locked").next();
        let Some((i, c)) = next else { break };
        f(i, c);
    };
    std::thread::scope(|s| {
        for _ in 0..helpers {
            s.spawn(run);
        }
        run();
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
        c.sculpt_mask = self.sculpt_mask.as_ref().map(crate::mask::SculptMask::mirrored_x);
        c.groups = self.groups.as_ref().map(crate::groups::Groups::mirrored_x);
        c.sculpt = self.sculpt.as_ref().map(|s| s.mirrored_x(&c));
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
        let old_base = self.detail.clone();
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
        // The layers above follow (their ledgers record it).
        crate::layers::relayout(self, Some(cell), old_base)
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
        let old_base = self.detail.clone();
        let mut detail = self.detail.clone().unwrap_or_else(|| Detail::new(coarse));
        detail.grid_mut().set_regions(regions)?;
        self.detail = Some(detail);
        self.detail_cell = Some(self.detail_cell.unwrap_or(coarse));
        crate::layers::relayout(self, None, old_base)
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

    /// An array blob as separate copies of one shape (see `Instancing`):
    /// what `instance_array` meshes once and draws as instances. Refused
    /// when the copies would not look the same apart: the blob is not a
    /// plain added array, or (unless `force`) other blobs blend with, cut
    /// into or cover the copies, or sculpt, paint or decals would be lost.
    pub fn instancing(&self, blob: usize, leaf: bool, force: bool) -> Result<Instancing, String> {
        let b = self.blobs.get(blob).ok_or_else(|| format!("no blob {blob} (the clay has {})", self.blobs.len()))?;
        let (unit, copies) = hard_surface::array_copies(&b.form, leaf).ok_or_else(|| format!("blob {blob} is not an array"))?;
        if b.op != Op::Add {
            return Err(format!("blob {blob} is {:?}; only added arrays become instances", b.op).to_lowercase());
        }
        if b.mirror || b.flip {
            return Err(format!("blob {blob} is mirrored; turn mirror off or instance each side"));
        }
        if self.shell.is_some() {
            return Err("the clay is hollowed (shell); its copies cannot be hollowed apart".into());
        }
        // Every copy's box, at the innermost level (nested rings of posts
        // are checked post by post, not ring by ring).
        let (leaf_form, leaves) = hard_surface::array_copies(&b.form, true).expect("an array");
        let (a, c) = hard_surface::form_bounds(&leaf_form, &self.assets);
        let corners: Vec<Vec3> = (0..8).map(|i| Vec3::new(if i & 1 == 0 { a.x } else { c.x }, if i & 2 == 0 { a.y } else { c.y }, if i & 4 == 0 { a.z } else { c.z })).collect();
        let boxes: Vec<(Vec3, Vec3)> = leaves.iter().map(|&(t, r)| corners.iter().map(|&p| t + r * p).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)))).collect();
        let join = |x: &Blob| if x.join == hard_surface::Join::Sharp { 0.0 } else { x.join_size.unwrap_or(x.blend).max(0.0) };
        let own = join(b);
        if !force {
            for (j, other) in self.blobs.iter().enumerate().filter(|(j, _)| *j != blob) {
                let k = join(other);
                // Plain added blobs only meet the copies with a hard edge
                // (a union), which separate meshes give too; the array's
                // own blend reaches blobs before it.
                let plain = other.op == Op::Add && k <= 1e-6;
                if plain && (own <= 1e-6 || j > blob) {
                    continue;
                }
                let (lo, hi) = hard_surface::form_bounds(&other.form, &self.assets);
                let (lo, hi) = if other.mirror || other.flip { (lo.min(Vec3::new(-hi.x, lo.y, lo.z)), hi.max(Vec3::new(-lo.x, hi.y, hi.z))) } else { (lo, hi) };
                let pad = k + own + other.offset.max(0.0) + other.smooth.max(0.0) + other.wall.max(0.0) + other.rim.max(0.0) + other.ridge.max(0.0) + other.flare.max(0.0);
                if boxes.iter().any(|(a, c)| a.cmple(hi + pad).all() && c.cmpge(lo - pad).all()) {
                    let name = if other.name.is_empty() { String::new() } else { format!(" ({})", other.name) };
                    return Err(format!("blob {j}{name} blends with, cuts into or covers the copies; separate copies would not join it the same way (force: true instances anyway)"));
                }
            }
            if self.detail.is_some() || self.paint.is_some() || self.sculpt.is_some() || !self.masks.is_empty() || !self.decals.is_empty() {
                return Err("the clay has sculpt, paint, masks or decals, which stay on it and would not show on the copies (force: true instances anyway)".into());
            }
        }
        let mut blob = b.clone();
        // A lone box or extrude would get the exact planar mesh single
        // primitives get, far denser than the field mesh it had in the
        // array: as a one-copy array it meshes the way it did there.
        blob.form = if matches!(unit, Form::Array { .. }) { unit } else {
            Form::Array { source: Box::new(unit), count: 1, step: Vec3::ZERO, radius: 0.0, center: Vec3::ZERO, axis: Vec3::Y, points: Vec::new(), orient: Default::default() }
        };
        let mut clay = Clay { blobs: vec![blob], resolution: self.resolution, detail: None, paint: None, masks: Default::default(), decals: Vec::new(), detail_cell: self.detail_cell, shell: None, sharp: self.sharp, sculpt: None, sculpt_mask: None, groups: None, assets: self.assets.clone() };
        // The copy meshes at the clay's own cell size, so it looks the same.
        if let Some(cell) = self.cell_size() {
            clay.keep_cell(cell);
        }
        Ok(Instancing { unit: clay, copies })
    }

    /// Removes blob `i`; the rest keeps meshing at the cell size it had
    /// (removing the finest part would otherwise coarsen an automatic
    /// resolution).
    pub fn remove_keeping_cell(&mut self, i: usize) {
        let cell = self.cell_size();
        self.blobs.remove(i);
        if let Some(cell) = cell {
            self.keep_cell(cell);
        }
    }

    /// Sets the resolution that meshes these blobs at about `cell`.
    fn keep_cell(&mut self, cell: f32) {
        let Some((lo, hi)) = Field::new(self, 0.0).bounds() else { return };
        let res = (((hi - lo).max_element() / cell).round() as u32).clamp(MIN_RESOLUTION, MAX_RESOLUTION);
        // The default resolution means "pick one" for hard surfaces.
        self.resolution = if res == DEFAULT_RESOLUTION { res + 1 } else { res };
    }

    /// Array blobs: (index, copies at the innermost level, nested).
    pub fn arrays(&self) -> Vec<(usize, usize, bool)> {
        self.blobs.iter().enumerate().filter_map(|(i, b)| {
            let Form::Array { source, .. } = &b.form else { return None };
            let copies = hard_surface::array_copies(&b.form, true)?.1.len();
            Some((i, copies, matches!(**source, Form::Array { .. })))
        }).collect()
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
                let (geo,_,_)=Geo::new(&self.blobs[0].form, &self.assets);
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
        // A row of blocks at a time: z slabs alone would leave cores idle
        // on flat or small grids.
        par_chunks(&mut bricks, bnx, |row, line| {
            let (by, bz) = (row % bny, row / bny);
            let (y0, z0) = (by * BRICK, bz * BRICK);
            for bx in 0..bnx {
                let x0 = bx * BRICK;
                let (x1, y1, z1) = ((x0 + BRICK).min(nx), (y0 + BRICK).min(ny), (z0 + BRICK).min(nz));
                let (blo, bhi) = (point(x0, y0, z0) - voxel, point(x1, y1, z1) + voxel);
                let near = field.near(blo, bhi);
                let detail = field.has_detail(blo, bhi);
                if near.is_empty() && !detail {
                    continue;
                }
                if !detail && clip((blo+bhi)*0.5).abs()>far && let Some(inside) = field.far_side((blo + bhi) * 0.5, &near, far) {
                    line[bx] = if inside { Block::In } else { Block::Out };
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
                line[bx] = Block::Data(b.into_boxed_slice());
            }
        });
        lap("blocks");
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
        // Cells (x, y, z < n - 1) of one block, in a fixed order.
        let cells_of = move |i: usize| {
            let (bx, by, bz) = (i % bnx, (i / bnx) % bny, i / (bnx * bny));
            let (x0, y0, z0) = (bx * BRICK, by * BRICK, bz * BRICK);
            let (x1, y1, z1) = ((x0 + BRICK).min(nx - 1), (y0 + BRICK).min(ny - 1), (z0 + BRICK).min(nz - 1));
            (z0..z1).flat_map(move |z| (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y, z))))
        };
        let live_blocks: Vec<usize> = (0..bricks.len()).filter(|&i| live[i]).collect();
        // Cells of live blocks, block by block, in a fixed order.
        let cells_in_live_blocks = || live_blocks.iter().flat_map(|&i| cells_of(i));

        // Smooth-only clay keeps the original fast surface-net path.
        let sharp = self.sharp_features();
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
            // Each block's vertices on any thread; numbered in block order
            // after, so the mesh is the same however the work was split.
            let mut found: Vec<Vec<(usize, Vec3)>> = vec![Vec::new(); live_blocks.len()];
            par_chunks(&mut found, 4, |chunk, outs| {
                for (j, out) in outs.iter_mut().enumerate() {
                    for (x, y, z) in cells_of(live_blocks[chunk * 4 + j]) {
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
                        if sharp { position=position.clamp(Vec3::splat(1e-3),Vec3::splat(1.0-1e-3)); }
                        out.push((cell(x,y,z),origin+position*voxel));
                    }
                }
            });
            for (c, p) in found.into_iter().flatten() {
                cell_vertex.insert(c, positions.len() as u32);
                positions.push(p);
            }
            lap("vertices");

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

            lap("quads");
            // Split each quad: Ok(diagonal), or Err(a fan center), found on
            // any thread and added in quad order.
            let mut splits: Vec<(([u32;4],Vec3,Vec3,Vec3),Result<bool,Vec3>)> = quads.into_iter().map(|q| (q,Ok(false))).collect();
            par_chunks(&mut splits, 1024, |_, chunk| {
                for ((q,direction,start,end),split) in chunk {
                    *split = match hard_surface::quad_diagonal(*q,*direction,&positions) {
                        Some(diagonal)=>Ok(diagonal),
                        None=> {
                            // A primal-edge intersection is inside all four projected
                            // cell quadrants. A fan around it preserves winding while
                            // retaining sharp QEF vertices, instead of rounding an
                            // entire row back to surface-net positions.
                            let (start,end)=(*start,*end);
                            let near=field.near(start-voxel,end+voxel);
                            let inside=sample(start,&near).0<0.0;
                            let (mut left,mut right)=(0.0,1.0);
                            for _ in 0..16 {
                                let t=(left+right)*0.5;
                                if (sample(start.lerp(end,t),&near).0<0.0)==inside {left=t;} else {right=t;}
                            }
                            Err(start.lerp(end,((left+right)*0.5).clamp(0.001,0.999)))
                        }
                    };
                }
            });
            for (([a,b,c,d],..),split) in splits {
                match split {
                    Ok(true)=>indices.extend_from_slice(&[a,b,d,b,c,d]),
                    Ok(false)=>indices.extend_from_slice(&[a,b,c,a,c,d]),
                    Err(p)=> {
                        let center=positions.len() as u32;
                        positions.push(p);
                        indices.extend_from_slice(&[a,b,center,b,c,center,c,d,center,d,a,center]);
                    }
                }
            }

            if ambiguous {
                lap("ambiguous quads: marching tetrahedra instead");
                #[cfg(test)]
                TETRA_FALLBACKS.with(|n| n.set(n.get() + 1));
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

    /// Whether meshes place vertices on sharp features (hard forms, plates,
    /// chamfers, a shell); smooth-only clay keeps plain surface nets.
    pub(crate) fn sharp_features(&self) -> bool {
        self.sharp && (self.shell.is_some() || self.blobs.iter().any(|b|
            !matches!(b.form, Form::Sphere { .. } | Form::Ellipsoid { .. } | Form::Capsule { .. } | Form::Human { .. } | Form::Torus { .. } | Form::Chain { .. } | Form::Membrane { .. })
            || matches!(b.op, Op::Plate | Op::Engrave | Op::Emboss) || b.join == hard_surface::Join::Chamfer))
    }

    /// Whether the regular mesh is the exact planar mesh of one hard blob
    /// (`build_window_cells`), which no sampled mesh may replace.
    pub(crate) fn exact_planar(&self) -> bool {
        self.sharp && self.blobs.len()==1 && !self.blobs[0].mirror && !self.blobs[0].flip && self.blobs[0].op==Op::Add && self.shell.is_none() && self.detail.is_none() && self.paint.is_none() && self.decals.is_empty()
            && hard_surface::planar_mesh(&self.blobs[0].form, 1.0).is_some()
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

    pub(crate) fn mesh_resolution(&self) -> u32 {
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
        // Mesh blobs need their asset in the file; assets no blob names go.
        let assets = std::mem::take(&mut self.assets);
        self.blobs.retain(|b| b.mesh_assets().iter().all(|h| assets.contains_key(*h)));
        self.assets = assets;
        self.prune_assets();
        self.shell = self.shell.filter(|t| t.is_finite() && *t > 0.0);
        self.resolution = self.resolution.clamp(MIN_RESOLUTION, MAX_RESOLUTION);
        if self.detail.as_ref().is_some_and(|d| !d.is_sane()) {
            self.detail = None;
        }
        if self.paint.as_ref().is_some_and(|p| !p.is_sane()) {
            self.paint = None;
        }
        if self.sculpt.as_ref().is_some_and(|s| !s.is_sane()) {
            self.sculpt = None;
        }
        if self.sculpt_mask.as_ref().is_some_and(|m| !m.is_sane()) {
            self.sculpt_mask = None;
        }
        if self.groups.as_ref().is_some_and(|g| !g.is_sane()) {
            self.groups = None;
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

    /// Drops the mesh assets no blob names (after blobs went). Never
    /// adds one, so it cannot fail; `adopt_assets` takes new ones.
    pub fn prune_assets(&mut self) {
        let named: std::collections::BTreeSet<&str> = self.blobs.iter().flat_map(Blob::mesh_assets).collect();
        self.assets.retain(|h, _| named.contains(h.as_str()));
    }

    /// Keeps exactly the mesh assets its blobs name (see `Form::Mesh`):
    /// one it does not hold yet comes from those this process made
    /// (`MeshAsset::lookup`); a blob naming one found nowhere is an error.
    pub fn adopt_assets(&mut self) -> Result<(), String> {
        let mut assets = std::collections::BTreeMap::new();
        for h in self.blobs.iter().flat_map(Blob::mesh_assets) {
            if assets.contains_key(h) {
                continue;
            }
            let a = self.assets.get(h).cloned().or_else(|| crate::mesh_asset::MeshAsset::lookup(h)).ok_or_else(|| format!("no mesh asset {h} (import_mesh or to_clay makes them)"))?;
            assets.insert(h.to_string(), a);
        }
        if assets.len() > crate::mesh_asset::MAX_ASSETS {
            return Err(format!("a clay may keep at most {} mesh assets", crate::mesh_asset::MAX_ASSETS));
        }
        // A blob may name any asset made in this process (a 4M-triangle
        // mesh object's too): only field-sized ones become clay.
        crate::mesh_asset::check_clay_assets(assets.values())?;
        self.assets = assets;
        Ok(())
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
/// A detail grid's bricks by key (a quick hash: grids are read point by
/// point while meshing).
pub(crate) type Bricks = base_cache::KeyMap<Arc<[f32; BRICK_VALUES]>>;
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

#[derive(Debug, Default)]
pub struct DetailGrid {
    pub voxel: f32,
    bricks: Bricks,
    /// Finer levels, coarsest first; finer ones win where they overlap.
    levels: Vec<Level>,
    /// `solid_bounds`, once worked out (it reads every brick; every frame
    /// asks for a clay's bounds). Any change forgets it.
    solid: std::sync::OnceLock<Option<(Vec3, Vec3)>>,
    /// Known to hold no brick that `prune` would drop (set by a full prune,
    /// kept by `write`, which drops the bricks it leaves at zero; anything
    /// else that changes bricks clears it). A stroke's first `write` then
    /// skips the full prune it would otherwise run: the result is the same.
    clean: bool,
}

#[cfg(test)]
thread_local! {
    static GRID_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many `DetailGrid`s (levels count one each) this thread has copied:
/// the sculpt engine's tests bound it per stroke.
#[cfg(test)]
pub(crate) fn grid_copies() -> usize {
    GRID_COPIES.with(|c| c.get())
}

impl Clone for DetailGrid {
    fn clone(&self) -> Self {
        #[cfg(test)]
        GRID_COPIES.with(|c| c.set(c.get() + 1));
        DetailGrid { voxel: self.voxel, bricks: self.bricks.clone(), levels: self.levels.clone(), solid: self.solid.clone(), clean: self.clean }
    }
}

impl PartialEq for Detail {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Detail {
    /// The grid's identity, as `==` compares it (cheap to hash).
    pub fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

/// Grid arithmetic for the stroke ledger (see `ledger.rs`): Adjust Last
/// blends a stroke's before and after, outside edits become deltas.
impl Detail {
    /// `before + t x (after - before)` point by point (`before` None:
    /// zeros); t = 1 is `after` itself and t = 0 `before`.
    pub(crate) fn blend(before: Option<&Detail>, after: &Detail, t: f32) -> Result<Detail, String> {
        let empty = Detail::new(after.voxel());
        let before_grid = before.unwrap_or(&empty);
        if t == 1.0 {
            return Ok(after.clone());
        }
        if !before_grid.0.same_layout(&after.0) && before.is_some() {
            return Err("the detail cell or regions changed since the stroke; it cannot be scaled".into());
        }
        if t == 0.0 {
            return Ok(before.cloned().unwrap_or(empty));
        }
        let b = if before.is_some() { &before_grid.0 } else { &DetailGrid { voxel: after.voxel(), bricks: Bricks::default(), levels: after.0.levels.iter().map(|l| Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, ..Default::default() } }).collect(), solid: Default::default(), clean: false } };
        Ok(Detail(Arc::new(b.zip_levels(&after.0, |a| Some(a.clone()), &|b, a| b + t * (a - b)))))
    }

    /// `after - before` when both have the same cell and regions (None:
    /// no shared layout, so the change can only be kept whole).
    pub(crate) fn delta(before: Option<&Detail>, after: &Detail) -> Option<Detail> {
        let b = before?;
        b.0.same_layout(&after.0).then(|| Detail(Arc::new(after.0.zip_levels(&b.0, |_| None, &|a, b| a - b))))
    }

    /// `base + delta` (same layout as `delta` was taken with).
    pub(crate) fn add(base: &Detail, delta: &Detail) -> Result<Detail, String> {
        if !base.0.same_layout(&delta.0) {
            return Err("an outside edit no longer lines up with the detail grid".into());
        }
        Ok(Detail(Arc::new(base.0.zip_levels(&delta.0, |a| Some(Arc::new(a.map(|v| v + v))), &|a, d| a + d))))
    }

    /// Same cell and same regions as `other` (layers compose point by point).
    pub(crate) fn same_layout(&self, other: &Detail) -> bool {
        self.0.same_layout(&other.0)
    }

    /// An empty grid with this one's cell and regions.
    pub(crate) fn empty_like(&self) -> Detail {
        let g = &self.0;
        let levels = g.levels.iter().map(|l| Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, ..Default::default() } }).collect();
        Detail(Arc::new(DetailGrid { voxel: g.voxel, bricks: Bricks::default(), levels, solid: Default::default(), clean: false }))
    }

    /// `sum(amount x grid)` point by point, the parts in order (sculpt
    /// layers composed: `layers.rs`). Every part must share the first's
    /// layout. A brick only one part has at amount 1 is shared, not
    /// copied, so one part at amount 1 is that grid itself. Bricks are
    /// summed independently (in parallel), always in the parts' order, so
    /// the result never depends on the thread count.
    pub(crate) fn compose(parts: &[(&Detail, f32)]) -> Result<Detail, String> {
        let Some((first, a0)) = parts.first() else { return Err("nothing to compose".into()) };
        if parts.len() == 1 && *a0 == 1.0 {
            return Ok((*first).clone());
        }
        if parts.iter().any(|(d, _)| !d.0.same_layout(&first.0)) {
            return Err("sculpt layers no longer line up (their detail cell or regions differ)".into());
        }
        fn level(grids: &[(&DetailGrid, f32)]) -> Bricks {
            let mut keys: Vec<[i32; 3]> = grids.iter().flat_map(|(g, _)| g.bricks.keys().copied()).collect();
            keys.sort_unstable();
            keys.dedup();
            sum_keys(grids, keys).into_iter().filter(|(_, b)| b.iter().any(|x| *x != 0.0)).collect()
        }
        let coarse: Vec<(&DetailGrid, f32)> = parts.iter().map(|(d, a)| (&d.0 as &DetailGrid, *a)).collect();
        let g = &first.0;
        let levels = g
            .levels
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let grids: Vec<(&DetailGrid, f32)> = parts.iter().map(|(d, a)| (&d.0.levels[k].grid, *a)).collect();
                Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, bricks: level(&grids), levels: Vec::new(), solid: Default::default(), clean: false } }
            })
            .collect();
        Ok(Detail(Arc::new(DetailGrid { voxel: g.voxel, bricks: level(&coarse), levels, solid: Default::default(), clean: false })))
    }

    /// `compose` again after one part (`touched`) changed: only the bricks
    /// it has are summed again, the rest are shared with `old` (the
    /// composite before, of the same layout). Point for point what
    /// `compose` gives.
    pub(crate) fn recompose_keys(parts: &[(&Detail, f32)], old: &Detail, touched: &Detail) -> Result<Detail, String> {
        let Some((first, _)) = parts.first() else { return Err("nothing to compose".into()) };
        if parts.iter().any(|(d, _)| !d.0.same_layout(&first.0)) || !old.0.same_layout(&first.0) || !touched.0.same_layout(&first.0) {
            return Err("sculpt layers no longer line up (their detail cell or regions differ)".into());
        }
        fn level(grids: &[(&DetailGrid, f32)], old: &DetailGrid, touched: &DetailGrid) -> Bricks {
            let mut out = old.bricks.clone();
            let mut keys: Vec<[i32; 3]> = touched.bricks.keys().copied().collect();
            keys.sort_unstable();
            for (k, b) in keys.iter().zip(sum_keys(grids, keys.clone()).into_iter().map(|(_, b)| b)) {
                out.insert(*k, b);
            }
            out
        }
        let coarse: Vec<(&DetailGrid, f32)> = parts.iter().map(|(d, a)| (&d.0 as &DetailGrid, *a)).collect();
        let g = &old.0;
        let levels = g
            .levels
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let grids: Vec<(&DetailGrid, f32)> = parts.iter().map(|(d, a)| (&d.0.levels[k].grid, *a)).collect();
                Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, bricks: level(&grids, &l.grid, &touched.0.levels[k].grid), levels: Vec::new(), solid: Default::default(), clean: false } }
            })
            .collect();
        let mut out = DetailGrid { voxel: g.voxel, bricks: level(&coarse, g, &touched.0), levels, solid: Default::default(), clean: false };
        // Bricks that sum to zero are left out, as `compose` leaves them.
        out.bricks.retain(|_, b| b.iter().any(|x| *x != 0.0));
        for l in &mut out.levels {
            l.grid.bricks.retain(|_, b| b.iter().any(|x| *x != 0.0));
        }
        Ok(Detail(Arc::new(out)))
    }

    /// Every stored value through `f(object-space point, value)`, on every
    /// level (`split_by_mask` weighs a layer by the mask).
    pub(crate) fn map_points(&self, f: impl Fn(Vec3, f32) -> f32) -> Detail {
        fn level(g: &DetailGrid, f: &dyn Fn(Vec3, f32) -> f32) -> Bricks {
            let mut out = Bricks::default();
            for (key, b) in &g.bricks {
                let mut v = **b;
                for (i, x) in v.iter_mut().enumerate() {
                    let l = [i as i32 % DB, i as i32 / DB % DB, i as i32 / (DB * DB)];
                    let p = Vec3::new((key[0] * DB + l[0]) as f32, (key[1] * DB + l[1]) as f32, (key[2] * DB + l[2]) as f32) * g.voxel;
                    *x = f(p, *x);
                }
                if v.iter().any(|x| x.abs() > 1e-7) {
                    out.insert(*key, Arc::new(v));
                }
            }
            out
        }
        let g = &self.0;
        let levels = g.levels.iter().map(|l| Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, bricks: level(&l.grid, &f), levels: Vec::new(), solid: Default::default(), clean: false } }).collect();
        Detail(Arc::new(DetailGrid { voxel: g.voxel, bricks: level(g, &f), levels, solid: Default::default(), clean: false }))
    }

    /// A grid that passes the checks a file's grids get.
    pub(crate) fn sane(&self) -> bool {
        self.is_sane()
    }

    /// `grid + (after - before) / amount` where `before` and `after` differ
    /// (bricks compared by address; `grid` None: zeros with `before`'s
    /// layout): a stroke made on the composed layers (`before` to `after`)
    /// folded back into the one layer it writes, at that layer's amount.
    pub(crate) fn fold_change(grid: Option<&Detail>, before: &Detail, after: &Detail, amount: f32) -> Result<Detail, String> {
        let empty;
        let grid = match grid {
            Some(g) => g,
            None => {
                empty = before.empty_like();
                &empty
            }
        };
        if !grid.0.same_layout(&before.0) || !before.0.same_layout(&after.0) {
            return Err("the layer no longer lines up with the others (its detail cell or regions differ)".into());
        }
        fn level(g: &DetailGrid, b: &DetailGrid, a: &DetailGrid, amount: f32) -> Bricks {
            let zero = [0.0f32; BRICK_VALUES];
            let mut out = g.bricks.clone();
            let mut keys: Vec<&[i32; 3]> = a.bricks.keys().chain(b.bricks.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let (x, y) = (b.bricks.get(key), a.bricks.get(key));
                if let (Some(x), Some(y)) = (x, y)
                    && Arc::ptr_eq(x, y)
                {
                    continue;
                }
                let (x, y) = (x.map_or(&zero, |x| &**x), y.map_or(&zero, |y| &**y));
                let mut v = g.bricks.get(key).map_or(zero, |b| **b);
                for i in 0..BRICK_VALUES {
                    v[i] += (y[i] - x[i]) / amount;
                }
                if v.iter().any(|x| x.abs() > 1e-7) {
                    out.insert(*key, Arc::new(v));
                } else {
                    out.remove(key);
                }
            }
            out
        }
        let (g, b, a) = (&grid.0, &before.0, &after.0);
        let levels = g.levels.iter().zip(b.levels.iter().zip(&a.levels)).map(|(l, (lb, la))| Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.grid.voxel, bricks: level(&l.grid, &lb.grid, &la.grid, amount), levels: Vec::new(), solid: Default::default(), clean: false } }).collect();
        Ok(Detail(Arc::new(DetailGrid { voxel: g.voxel, bricks: level(g, b, a, amount), levels, solid: Default::default(), clean: false })))
    }

    /// The grid resampled to `other`'s layout (cell and regions), as
    /// `edit_clay detail_cell` and `detail_regions` move a clay's base
    /// grid: the layers above follow it.
    pub(crate) fn relaid_like(&self, other: &Detail) -> Result<Detail, String> {
        if self.0.same_layout(&other.0) {
            return Ok(self.clone());
        }
        let mut g = if self.0.voxel == other.0.voxel { (*self.0).clone() } else { self.0.resampled(other.0.voxel)? };
        let regions: Vec<Region> = other.0.levels.iter().map(|l| l.region.clone()).collect();
        g.set_regions(&regions)?;
        let out = Detail(Arc::new(g));
        if !out.0.same_layout(&other.0) {
            return Err("a layer could not follow the base grid's new regions".into());
        }
        Ok(out)
    }

    /// The coarse grid alone, without refined levels (bricks shared).
    pub(crate) fn without_levels(&self) -> Detail {
        Detail(Arc::new(DetailGrid { voxel: self.0.voxel, bricks: self.0.bricks.clone(), levels: Vec::new(), solid: Default::default(), clean: false }))
    }

    /// See `DetailGrid::mirrored_x`.
    pub fn mirrored_x(&self) -> Detail {
        Detail(Arc::new(self.0.mirrored_x()))
    }

    pub fn new(voxel: f32) -> Self {
        Detail(Arc::new(DetailGrid { voxel, bricks: Bricks::default(), levels: Vec::new(), solid: Default::default(), clean: false }))
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
        let g = Arc::make_mut(&mut self.0);
        g.forget_bounds();
        g
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

/// Brick `key` of `sum(amount x grid)` for each key (in parallel; in the
/// grids' order within a brick): a brick only one grid has at amount 1
/// is shared, all-zero sums are a zero brick (None).
fn sum_keys(grids: &[(&DetailGrid, f32)], keys: Vec<[i32; 3]>) -> Vec<([i32; 3], Arc<[f32; BRICK_VALUES]>)> {
    let zero = Arc::new([0.0f32; BRICK_VALUES]);
    let mut out: Vec<([i32; 3], Option<Arc<[f32; BRICK_VALUES]>>)> = keys.into_iter().map(|k| (k, None)).collect();
    par_chunks(&mut out, 256, |_, chunk| {
        for (key, slot) in chunk.iter_mut() {
            let have: Vec<(&Arc<[f32; BRICK_VALUES]>, f32)> = grids.iter().filter_map(|(g, a)| g.bricks.get(key).map(|b| (b, *a))).filter(|(_, a)| *a != 0.0).collect();
            if let [(b, a)] = &have[..]
                && *a == 1.0
            {
                *slot = Some((*b).clone());
                continue;
            }
            let mut v = [0.0f32; BRICK_VALUES];
            for (b, a) in &have {
                for i in 0..BRICK_VALUES {
                    v[i] += a * b[i];
                }
            }
            *slot = Some(if v.iter().any(|x| *x != 0.0) { Arc::new(v) } else { zero.clone() });
        }
    });
    out.into_iter().map(|(k, b)| (k, b.expect("filled"))).collect()
}

pub(crate) fn split(i: [i32; 3]) -> ([i32; 3], usize) {
    // DB is 8: an arithmetic shift floors (div_euclid) and the low bits
    // are rem_euclid, for every i32.
    const _: () = assert!(DB == 8);
    let key = i.map(|c| c >> 3);
    let l = i.map(|c| (c & 7) as usize);
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
        self.solid = std::sync::OnceLock::new();
        self.clean = false;
        let (key, at) = split(i);
        if v == 0.0 && !self.bricks.contains_key(&key) {
            return;
        }
        Arc::make_mut(self.bricks.entry(key).or_insert_with(|| Arc::new([0.0; BRICK_VALUES])))[at] = v;
    }

    /// This level's bricks by key, in no particular order.
    pub fn bricks(&self) -> impl Iterator<Item = (&[i32; 3], &[f32; BRICK_VALUES])> {
        self.bricks.iter().map(|(k, b)| (k, &**b))
    }

    /// One brick of this level, shared (its address says whether two
    /// grids share it).
    pub(crate) fn brick(&self, key: &[i32; 3]) -> Option<&Arc<[f32; BRICK_VALUES]>> {
        self.bricks.get(key)
    }

    /// This level's brick keys, in no particular order.
    pub(crate) fn brick_keys(&self) -> impl Iterator<Item = &[i32; 3]> {
        self.bricks.keys()
    }

    /// Same cell and same regions (so grid points line up level by level).
    pub(crate) fn same_layout(&self, other: &DetailGrid) -> bool {
        self.voxel == other.voxel && self.levels.len() == other.levels.len() && self.levels.iter().zip(&other.levels).all(|(a, b)| a.region == b.region && a.band == b.band)
    }

    /// Brick by brick over both grids' keys (this layout): `f(mine, other)`
    /// with absent bricks as zeros; `keep` says what a brick both grids
    /// share becomes (None: dropped, the result's zeros).
    fn zip(&self, other: &DetailGrid, shared: fn(&Arc<[f32; BRICK_VALUES]>) -> Option<Arc<[f32; BRICK_VALUES]>>, f: &dyn Fn(f32, f32) -> f32) -> DetailGrid {
        let zero = [0.0f32; BRICK_VALUES];
        let mut bricks = Bricks::default();
        for key in self.bricks.keys().chain(other.bricks.keys().filter(|k| !self.bricks.contains_key(*k))) {
            let (a, b) = (self.bricks.get(key), other.bricks.get(key));
            if let (Some(a), Some(b)) = (a, b)
                && Arc::ptr_eq(a, b)
            {
                if let Some(v) = shared(a) {
                    bricks.insert(*key, v);
                }
                continue;
            }
            let (a, b) = (a.map_or(&zero, |a| &**a), b.map_or(&zero, |b| &**b));
            let mut out = [0.0f32; BRICK_VALUES];
            for i in 0..BRICK_VALUES {
                out[i] = f(a[i], b[i]);
            }
            if out.iter().any(|v| *v != 0.0) {
                bricks.insert(*key, Arc::new(out));
            }
        }
        DetailGrid { voxel: self.voxel, bricks, levels: Vec::new(), solid: Default::default(), clean: false }
    }

    /// `zip` on every level (the layouts must match).
    fn zip_levels(&self, other: &DetailGrid, shared: fn(&Arc<[f32; BRICK_VALUES]>) -> Option<Arc<[f32; BRICK_VALUES]>>, f: &dyn Fn(f32, f32) -> f32) -> DetailGrid {
        let mut out = self.zip(other, shared, f);
        out.levels = self.levels.iter().zip(&other.levels).map(|(a, b)| Level { region: a.region.clone(), band: a.band, grid: a.grid.zip(&b.grid, shared, f) }).collect();
        out
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
        let mut out = DetailGrid { voxel: self.voxel, bricks: Bricks::default(), levels: Vec::new(), solid: Default::default(), clean: false };
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
        self.forget_bounds();
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
        self.solid = std::sync::OnceLock::new();
        self.bricks.retain(|_, b| b.iter().any(|v| v.abs() > 1e-7));
        self.clean = true;
    }

    /// The grid a sculpt target writes: this one (None) or level `k`'s.
    pub(crate) fn target_mut(&mut self, target: Option<usize>) -> &mut DetailGrid {
        self.forget_bounds();
        match target {
            None => self,
            Some(k) => &mut self.levels[k].grid,
        }
    }

    pub(crate) fn target(&self, target: Option<usize>) -> &DetailGrid {
        match target {
            None => self,
            Some(k) => &self.levels[k].grid,
        }
    }

    /// Sets grid points on this level, then drops the bricks left all zero:
    /// every brick when `all`, else only the ones written (enough once the
    /// grid has been pruned, as only written bricks change). Returns the
    /// keys of the bricks written or dropped: what a copy of the grid must
    /// take over (`share_bricks`) to match it again.
    pub(crate) fn write(&mut self, changes: &[([i32; 3], f32)], all: bool) -> Vec<[i32; 3]> {
        self.solid = std::sync::OnceLock::new();
        // Brushes list their changes x fastest, so they come in runs of up
        // to 8 points of one brick: each run takes one map lookup (and one
        // copy-on-write check), and only the runs' keys are sorted. The
        // values written are exactly `set`'s, point by point in order (a
        // run of zeros on a brick that is not there makes none, as `set`
        // makes none).
        let mut keys: Vec<[i32; 3]> = Vec::new();
        let mut s = 0;
        while s < changes.len() {
            let key = split(changes[s].0).0;
            let mut e = s + 1;
            while e < changes.len() && split(changes[e].0).0 == key {
                e += 1;
            }
            let run = &changes[s..e];
            // The rows after this one cross the same bricks: a key among
            // the last few listed is not listed again (the sort and dedup
            // below catch the rest).
            if !keys[keys.len().saturating_sub(32)..].contains(&key) {
                keys.push(key);
            }
            s = e;
            if !self.bricks.contains_key(&key) && run.iter().all(|(_, v)| *v == 0.0) {
                continue;
            }
            let brick = Arc::make_mut(self.bricks.entry(key).or_insert_with(|| Arc::new([0.0; BRICK_VALUES])));
            for &(i, v) in run {
                brick[split(i).1] = v;
            }
        }
        keys.sort_unstable();
        keys.dedup();
        let zero = |b: &[f32; BRICK_VALUES]| !b.iter().any(|v| v.abs() > 1e-7);
        // A clean grid has no zero brick but the ones written now.
        if all && !self.clean {
            self.clean = true;
            let mut dropped = Vec::new();
            self.bricks.retain(|k, b| {
                let keep = !zero(b);
                if !keep && keys.binary_search(k).is_err() {
                    dropped.push(*k);
                }
                keep
            });
            keys.extend(dropped);
        } else {
            for k in &keys {
                if self.bricks.get(k).is_some_and(|b| zero(b)) {
                    self.bricks.remove(k);
                }
            }
        }
        keys
    }

    /// Makes bricks `keys` the same as `from`'s, sharing them (no copy).
    pub(crate) fn share_bricks(&mut self, from: &DetailGrid, keys: &[[i32; 3]]) {
        self.solid = std::sync::OnceLock::new();
        self.clean &= from.clean;
        for k in keys {
            match from.bricks.get(k) {
                Some(b) => {
                    self.bricks.insert(*k, b.clone());
                }
                None => {
                    self.bricks.remove(k);
                }
            }
        }
    }

    /// The same offsets on a grid of `voxel` spacing (refined levels keep
    /// their own cells, and drop out when no longer finer).
    pub fn resampled(&self, voxel: f32) -> Result<DetailGrid, String> {
        let mut out = DetailGrid { voxel, bricks: Bricks::default(), levels: Vec::new(), solid: Default::default(), clean: false };
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

    /// The offset at grid point `i` with every refined level: this
    /// level's own value there (as brushes read it) plus what the finer
    /// levels add at its place.
    pub(crate) fn lattice(&self, i: [i32; 3]) -> f32 {
        let mut v = self.get(i);
        if !self.levels.is_empty() {
            let p = Vec3::new(i[0] as f32, i[1] as f32, i[2] as f32) * self.voxel;
            for l in &self.levels {
                let w = l.weight(p);
                if w > 0.0 {
                    v += l.grid.level_offset(p) * w;
                }
            }
        }
        v
    }

    /// Whether a refined level reaches into the box (`lo`..`hi`).
    pub(crate) fn levels_touch(&self, lo: Vec3, hi: Vec3) -> bool {
        self.levels.iter().any(|l| {
            let (a, b) = l.extent(0.0);
            a.cmple(hi).all() && b.cmpge(lo).all() && l.grid.level_touches(lo, hi)
        })
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

    /// `get` at grid points `i0 + (x, 0, 0)` for each x in `out` (this
    /// level): one brick lookup per run of points in one brick.
    pub(crate) fn row(&self, i0: [i32; 3], out: &mut [f32]) {
        let (ky, kz) = (i0[1].div_euclid(DB), i0[2].div_euclid(DB));
        let at = (i0[1].rem_euclid(DB) * DB + i0[2].rem_euclid(DB) * DB * DB) as usize;
        let mut x = 0;
        while x < out.len() {
            let gx = i0[0] + x as i32;
            let (kx, lx) = (gx.div_euclid(DB), gx.rem_euclid(DB));
            let run = ((DB - lx) as usize).min(out.len() - x);
            match self.bricks.get(&[kx, ky, kz]) {
                Some(b) => out[x..x + run].copy_from_slice(&b[at + lx as usize..at + lx as usize + run]),
                None => out[x..x + run].fill(0.0),
            }
            x += run;
        }
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

    pub(crate) fn level_touches(&self, lo: Vec3, hi: Vec3) -> bool {
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

    /// Forgets `solid_bounds` here and on every level (before a change).
    fn forget_bounds(&mut self) {
        self.solid = std::sync::OnceLock::new();
        for l in &mut self.levels {
            l.grid.forget_bounds();
        }
    }

    /// Bounds of bricks that can add material outside the blobs.
    fn solid_bounds(&self) -> Option<(Vec3, Vec3)> {
        *self.solid.get_or_init(|| self.solid_bounds_now())
    }

    fn solid_bounds_now(&self) -> Option<(Vec3, Vec3)> {
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
    /// place and cell) keeps its detail. A new one takes over the detail of
    /// the regions that went (each goes to one new region: the one of its
    /// cell, else the finest, that overlaps it most), as the old levels
    /// showed it: inside both the old and the new region the surface does
    /// not change, so growing or merging regions (Sculptris Pro) keeps
    /// every finer stroke. What a removed region held outside every new
    /// one goes, but nothing else: every stroke also landed on the coarse
    /// grid.
    pub(crate) fn set_regions(&mut self, regions: &[Region]) -> Result<(), String> {
        self.forget_bounds();
        if regions.len() > MAX_REGIONS {
            return Err(format!("at most {MAX_REGIONS} detail regions"));
        }
        for r in regions {
            r.validate(self.voxel)?;
        }
        let kept: Vec<Option<usize>> = regions.iter().map(|r| self.levels.iter().position(|l| &l.region == r)).collect();
        let dropped: Vec<&Level> = self.levels.iter().filter(|l| !regions.contains(&l.region)).collect();
        let mut fresh: Vec<Level> = regions
            .iter()
            .zip(&kept)
            .map(|(r, k)| match k {
                Some(k) => self.levels[*k].clone(),
                // Empty until it takes over a removed region's detail.
                None => Level { band: band_for(self.voxel, r), region: r.clone(), grid: DetailGrid { voxel: r.cell, ..Default::default() } },
            })
            .collect();
        // Which new region each removed one's detail goes to (in the old
        // levels' order, so overlapping ones add up as they showed).
        for old in dropped {
            let (a, b) = old.extent(0.0);
            let cell = old.grid.voxel;
            let best = fresh
                .iter()
                .enumerate()
                .filter(|(n, l)| kept[*n].is_none() && {
                    let (c, d) = l.extent(0.0);
                    a.cmple(d).all() && c.cmple(b).all()
                })
                .map(|(n, l)| {
                    let (c, d) = l.extent(0.0);
                    let s = (b.min(d) - a.max(c)).max(Vec3::ZERO);
                    ((l.grid.voxel != cell, l.grid.voxel, -(s.x as f64 * s.y as f64 * s.z as f64)), n)
                })
                .min_by(|(x, _), (y, _)| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(_, n)| n);
            if let Some(n) = best
                && !old.grid.bricks.is_empty()
            {
                let into = &mut fresh[n];
                let (lo, hi) = into.extent(0.0);
                carry(old, &mut into.grid, &into.region.name, lo, hi)?;
                if into.grid.bricks.len() > MAX_BRICKS {
                    return Err(format!("detail region {} would need more than {MAX_BRICKS} sculpt blocks; use a smaller region or a larger cell", into.region.name));
                }
            }
        }
        // Coarse first, so finer cells win where regions overlap.
        fresh.sort_by(|a, b| b.region.cell.total_cmp(&a.region.cell));
        self.levels = fresh;
        if self.total_bricks() > MAX_TOTAL_BRICKS {
            return Err(format!("detail regions need more than {MAX_TOTAL_BRICKS} sculpt blocks in all; use smaller regions or larger cells"));
        }
        Ok(())
    }
}

/// Adds what level `old` showed (its difference times its weight) to the
/// grid `into`, at `into`'s grid points within `lo..=hi` (bricks around
/// them). A brick of the same cell that `old` showed whole and `into`
/// lacks is shared, not copied: a grown region keeps its strokes exactly.
fn carry(old: &Level, into: &mut DetailGrid, region: &str, lo: Vec3, hi: Vec3) -> Result<(), String> {
    into.solid = std::sync::OnceLock::new();
    into.clean = false;
    let mut keys: Vec<[i32; 3]> = old.grid.bricks.keys().copied().collect();
    keys.sort_unstable();
    let inside = |p: Vec3| ((p - old.region.center) / old.region.radii).length() <= 1.0;
    let v = into.voxel;
    let near = |key: [i32; 3]| {
        let (a, b) = old.grid.brick_box(key);
        a.cmple(hi).all() && lo.cmple(b).all()
    };
    if old.grid.voxel != v {
        // Another cell: the old level read at this grid's points, brick by
        // brick of this grid (each once, though the old bricks' boxes share
        // their edges).
        let size = DB as f32 * v;
        let too_many = || format!("detail region {region} would need more than {MAX_BRICKS} sculpt blocks to keep the finer detail it takes over; use a smaller region or a larger cell");
        let mut news: Vec<[i32; 3]> = Vec::new();
        for key in keys.into_iter().filter(|k| near(*k)) {
            let (a, b) = old.grid.brick_box(key);
            let (k0, k1) = ((a.max(lo) / size).floor().as_ivec3(), (b.min(hi) / size).floor().as_ivec3());
            let n = (k1 - k0 + 1).max(glam::IVec3::ZERO).as_i64vec3();
            if n.x * n.y * n.z > MAX_BRICKS as i64 {
                return Err(too_many());
            }
            if news.len() > 4 * MAX_BRICKS {
                news.sort_unstable();
                news.dedup();
                if news.len() > MAX_BRICKS {
                    return Err(too_many());
                }
            }
            for z in k0.z..=k1.z {
                for y in k0.y..=k1.y {
                    for x in k0.x..=k1.x {
                        news.push([x, y, z]);
                    }
                }
            }
        }
        news.sort_unstable();
        news.dedup();
        if news.len() > MAX_BRICKS {
            return Err(too_many());
        }
        for key in news {
            let base = glam::IVec3::from(key) * DB;
            let mut add = [0.0f32; BRICK_VALUES];
            for (at, slot) in add.iter_mut().enumerate() {
                let p = (base + glam::IVec3::new(at as i32 % DB, at as i32 / DB % DB, at as i32 / (DB * DB))).as_vec3() * v;
                *slot = old.grid.level_offset(p) * old.weight(p);
            }
            if add.iter().all(|x| *x == 0.0) {
                continue;
            }
            let dst = Arc::make_mut(into.bricks.entry(key).or_insert_with(|| Arc::new([0.0; BRICK_VALUES])));
            for (d, s) in dst.iter_mut().zip(add) {
                *d += s;
            }
            if dst.iter().all(|x| *x == 0.0) {
                into.bricks.remove(&key);
            }
        }
        return Ok(());
    }
    for key in keys.into_iter().filter(|k| near(*k)) {
        let (a, b) = old.grid.brick_box(key);
        let whole = (0..8).all(|c| inside(Vec3::new(if c & 1 == 0 { a.x } else { b.x }, if c & 2 == 0 { a.y } else { b.y }, if c & 4 == 0 { a.z } else { b.z })));
        let src = &old.grid.bricks[&key];
        if whole && !into.bricks.contains_key(&key) {
            into.bricks.insert(key, src.clone());
            continue;
        }
        let base = glam::IVec3::from(key) * DB;
        let mut add = [0.0f32; BRICK_VALUES];
        for (at, slot) in add.iter_mut().enumerate() {
            let i = base + glam::IVec3::new(at as i32 % DB, at as i32 / DB % DB, at as i32 / (DB * DB));
            *slot = src[at] * old.weight(i.as_vec3() * v);
        }
        let dst = Arc::make_mut(into.bricks.entry(key).or_insert_with(|| Arc::new([0.0; BRICK_VALUES])));
        for (d, s) in dst.iter_mut().zip(add) {
            *d += s;
        }
        if dst.iter().all(|x| *x == 0.0) {
            into.bricks.remove(&key);
        }
    }
    Ok(())
}

/// One level of a detail grid as brushes see it: whole offsets at its grid
/// points (a refined level's difference plus the coarser levels under it).
pub(crate) struct LevelView<'a> {
    pub top: &'a DetailGrid,
    pub level: Option<usize>,
    /// Coarse grid: the largest offset in each brick; levels: one bound.
    /// Found on first use (`carved`): it reads every brick, too slow for
    /// every dab of a long stroke on a big grid.
    bricks: std::sync::OnceLock<HashMap<[i32; 3], f32>>,
    most: std::sync::OnceLock<f32>,
}

impl<'a> LevelView<'a> {
    pub(crate) fn new(top: &'a DetailGrid, level: Option<usize>) -> Self {
        LevelView { top, level, bricks: std::sync::OnceLock::new(), most: std::sync::OnceLock::new() }
    }

    pub(crate) fn voxel(&self) -> f32 {
        self.level.map_or(self.top.voxel, |k| self.top.levels[k].grid.voxel)
    }

    pub(crate) fn get(&self, i: [i32; 3]) -> f32 {
        self.top.full(self.level, i)
    }

    /// `get` at grid points `i0 + (x, 0, 0)` for each x in `out`.
    pub(crate) fn row(&self, i0: [i32; 3], out: &mut [f32]) {
        match self.level {
            None => self.top.row(i0, out),
            Some(_) => {
                for (x, o) in out.iter_mut().enumerate() {
                    *o = self.get([i0[0] + x as i32, i0[1], i0[2]]);
                }
            }
        }
    }

    /// The largest offset in the world box `lo..=hi` (a bound over the
    /// whole grid for a box too large or too far to look at brick by brick).
    pub(crate) fn carved_in(&self, lo: Vec3, hi: Vec3) -> f32 {
        let v = self.voxel();
        let (a, b) = ((lo / v).floor(), (hi / v).ceil());
        let bound = 1e8;
        if !a.is_finite() || !b.is_finite() || (b - a).max_element() > (4 * DB) as f32 || a.abs().max_element() > bound || b.abs().max_element() > bound {
            return *self.most.get_or_init(|| self.top.max_abs());
        }
        self.carved(a.as_ivec3(), b.as_ivec3())
    }

    /// The largest offset around grid points `lo..=hi`: how far sculpting
    /// can have moved the surface there.
    pub(crate) fn carved(&self, lo: glam::IVec3, hi: glam::IVec3) -> f32 {
        if self.level.is_some() {
            return *self.most.get_or_init(|| self.top.max_abs());
        }
        let bricks = self.bricks.get_or_init(|| self.top.brick_max_abs());
        let (b0, b1) = (lo.div_euclid(glam::IVec3::splat(DB)), hi.div_euclid(glam::IVec3::splat(DB)));
        let mut m = 0.0f32;
        for z in b0.z..=b1.z {
            for y in b0.y..=b1.y {
                for x in b0.x..=b1.x {
                    m = m.max(bricks.get(&[x, y, z]).copied().unwrap_or(0.0));
                }
            }
        }
        m
    }
}

/// File form: offsets as i16 in 1/256 voxel steps. `packed` holds every
/// brick of a level deflated together (see `pack`); files from before
/// that have one base64 `bricks` entry per brick, and still read.
#[derive(Serialize, Deserialize)]
struct DetailFile {
    voxel: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bricks: Vec<BrickFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packed: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    levels: Vec<LevelFile>,
}

#[derive(Serialize, Deserialize)]
struct LevelFile {
    region: Region,
    band: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bricks: Vec<BrickFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packed: Option<String>,
}

/// Most bytes `unpack` inflates over one read of a scene (or of a grid or
/// ledger read on its own): a file that deflates to more is refused
/// before it is allocated, however many objects, levels and ledger
/// records it spreads them over. Detail bricks take twice their inflated
/// size in memory (f32 for i16), so this is about 8 GiB of grids.
pub const INFLATE_BUDGET: u64 = 4 << 30;

thread_local! {
    /// Bytes `unpack` may still inflate in the read going on (None: no
    /// read has opened a budget).
    static INFLATE_LEFT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// The open budget's size, for errors.
    static INFLATE_BUDGET_SIZE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Runs `f`, a deserialization, with `bytes` of inflate budget shared by
/// every grid it reads. Inside another read's budget it shares that one:
/// scene loads open one for the whole file, and grids and ledgers open
/// the default for themselves, so no read is unbounded.
pub fn inflate_budget<T>(bytes: u64, f: impl FnOnce() -> T) -> T {
    if INFLATE_LEFT.with(|b| b.get()).is_some() {
        return f();
    }
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            INFLATE_LEFT.with(|b| b.set(None));
            INFLATE_BUDGET_SIZE.with(|b| b.set(None));
        }
    }
    INFLATE_LEFT.with(|b| b.set(Some(bytes)));
    INFLATE_BUDGET_SIZE.with(|b| b.set(Some(bytes)));
    let _close = Close;
    f()
}

/// Deserializes with `INFLATE_BUDGET` unless a budget is open
/// (`deserialize_with` for fields that hold grids).
pub fn in_inflate_budget<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    inflate_budget(INFLATE_BUDGET, || T::deserialize(d))
}

/// `in_inflate_budget` with a deserializing function of its own.
pub fn in_inflate_budget_with<'de, D: serde::Deserializer<'de>, T>(d: D, f: impl FnOnce(D) -> Result<T, D::Error>) -> Result<T, D::Error> {
    inflate_budget(INFLATE_BUDGET, || f(d))
}

/// Takes `bytes` of the read's inflate budget (the default outside a
/// read) for data decoded some other way than `unpack` (alpha images).
pub fn charge_inflate(bytes: u64) -> Result<(), String> {
    let left = INFLATE_LEFT.with(|b| b.get()).unwrap_or(INFLATE_BUDGET);
    if bytes > left {
        let budget = INFLATE_BUDGET_SIZE.with(|b| b.get()).unwrap_or(INFLATE_BUDGET);
        return Err(format!("the file's packed data inflates to more than its budget of {budget} bytes ({} MiB) in all", budget >> 20));
    }
    INFLATE_LEFT.with(|b| b.set(b.get().map(|l| l - bytes)));
    Ok(())
}

/// Packs bricks for a file: a brick count, the keys (i32 x 3), then the
/// values (`width` bytes each, little-endian) split into byte planes (all
/// first bytes, then all second bytes ...), deflated and base64. With
/// `delta` the values (i16) are stored as differences from the one
/// before in the brick: smooth offsets and the planes compress well.
pub(crate) fn pack(keys: &[[i32; 3]], values: &[u8], width: usize, delta: bool) -> String {
    use std::io::Write;
    let mut raw = Vec::with_capacity(4 + keys.len() * 12 + values.len());
    raw.extend((keys.len() as u32).to_le_bytes());
    for k in keys {
        for c in k {
            raw.extend(c.to_le_bytes());
        }
    }
    let mut vals = values.to_vec();
    if delta {
        for brick in vals.chunks_exact_mut(BRICK_VALUES * 2) {
            let mut prev = 0i16;
            for pair in brick.chunks_exact_mut(2) {
                let v = i16::from_le_bytes([pair[0], pair[1]]);
                pair.copy_from_slice(&v.wrapping_sub(prev).to_le_bytes());
                prev = v;
            }
        }
    }
    for plane in 0..width {
        raw.extend(vals.iter().skip(plane).step_by(width));
    }
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).expect("deflate into memory");
    base64::engine::general_purpose::STANDARD.encode(z.finish().expect("deflate into memory"))
}

/// Reads `pack`'s form back: (keys, values), at most `max` bricks of
/// `width`-byte values.
pub(crate) fn unpack(text: &str, width: usize, delta: bool, max: usize) -> Result<(Vec<[i32; 3]>, Vec<u8>), String> {
    use std::io::Read;
    let bytes = base64::engine::general_purpose::STANDARD.decode(text).map_err(|e| e.to_string())?;
    let limit = (4 + max * (12 + BRICK_VALUES * width)) as u64;
    // Outside any read's budget (a direct call) the default applies.
    let left = INFLATE_LEFT.with(|b| b.get()).unwrap_or(INFLATE_BUDGET);
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[..]).take(limit.min(left) + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() as u64 > left {
        let budget = INFLATE_BUDGET_SIZE.with(|b| b.get()).unwrap_or(INFLATE_BUDGET);
        return Err(format!("the file's packed grids inflate to more than its budget of {budget} bytes ({} MiB) in all", budget >> 20));
    }
    INFLATE_LEFT.with(|b| b.set(b.get().map(|l| l - raw.len() as u64)));
    if raw.len() < 4 {
        return Err("packed bricks are cut short".into());
    }
    let n = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    if n > max {
        return Err("too many bricks".into());
    }
    if raw.len() != 4 + n * (12 + BRICK_VALUES * width) {
        return Err("packed bricks have the wrong size".into());
    }
    let keys = raw[4..4 + n * 12].chunks_exact(12).map(|c| [0, 4, 8].map(|o| i32::from_le_bytes([c[o], c[o + 1], c[o + 2], c[o + 3]]))).collect();
    let planes = &raw[4 + n * 12..];
    let count = n * BRICK_VALUES;
    let mut values = vec![0u8; count * width];
    for plane in 0..width {
        for (i, b) in planes[plane * count..(plane + 1) * count].iter().enumerate() {
            values[i * width + plane] = *b;
        }
    }
    if delta {
        for brick in values.chunks_exact_mut(BRICK_VALUES * 2) {
            let mut prev = 0i16;
            for pair in brick.chunks_exact_mut(2) {
                let v = prev.wrapping_add(i16::from_le_bytes([pair[0], pair[1]]));
                pair.copy_from_slice(&v.to_le_bytes());
                prev = v;
            }
        }
    }
    Ok((keys, values))
}

#[derive(Serialize, Deserialize)]
struct BrickFile {
    at: [i32; 3],
    data: String,
}

/// Offsets are kept within this many voxels so the file form is lossless enough.
pub const MAX_OFFSET_VOXELS: f32 = 127.0;

/// A brick's offsets as file values: i16 steps of 1/256 voxel.
fn brick_bytes(g: &DetailGrid, k: &[i32; 3]) -> Vec<u8> {
    g.bricks[k].iter().flat_map(|v| ((v / g.voxel * 256.0).round().clamp(-32767.0, 32767.0) as i16).to_le_bytes()).collect()
}

fn sorted_keys(g: &DetailGrid) -> Vec<[i32; 3]> {
    let mut keys: Vec<[i32; 3]> = g.bricks.keys().copied().collect();
    keys.sort();
    keys
}

/// The packed file form of a level's bricks (None when it has none).
fn bricks_packed(g: &DetailGrid) -> Option<String> {
    let keys = sorted_keys(g);
    (!keys.is_empty()).then(|| pack(&keys, &keys.iter().flat_map(|k| brick_bytes(g, k)).collect::<Vec<u8>>(), 2, true))
}

/// The older file form, one base64 entry per brick (tests measure the
/// packed form against it).
#[cfg(test)]
fn bricks_file(g: &DetailGrid) -> Vec<BrickFile> {
    let b64 = base64::engine::general_purpose::STANDARD;
    sorted_keys(g).into_iter().map(|k| BrickFile { at: k, data: b64.encode(brick_bytes(g, &k)) }).collect()
}

/// A level's bricks; at most `max` (`MAX_BRICKS` or fewer: what is left
/// of `MAX_TOTAL_BRICKS`).
fn bricks_unpack<E: serde::de::Error>(packed: Option<String>, file: Vec<BrickFile>, voxel: f32, max: usize) -> Result<Bricks, E> {
    if file.len() > max {
        return Err(E::custom("too many detail bricks"));
    }
    let mut bricks = bricks_read(file, voxel)?;
    if let Some(text) = packed {
        let (keys, values) = unpack(&text, 2, true, max).map_err(|e| E::custom(format!("detail bricks: {e}")))?;
        for (at, bytes) in keys.into_iter().zip(values.chunks_exact(BRICK_VALUES * 2)) {
            bricks.insert(at, Arc::new(brick_values(bytes, voxel)));
        }
        if bricks.len() > max {
            return Err(E::custom("too many detail bricks"));
        }
    }
    Ok(bricks)
}

fn brick_values(bytes: &[u8], voxel: f32) -> [f32; BRICK_VALUES] {
    let mut values = [0.0f32; BRICK_VALUES];
    for (v, pair) in values.iter_mut().zip(bytes.chunks_exact(2)) {
        *v = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 256.0 * voxel;
    }
    values
}

fn bricks_read<E: serde::de::Error>(file: Vec<BrickFile>, voxel: f32) -> Result<Bricks, E> {
    if file.len() > MAX_BRICKS {
        return Err(E::custom("too many detail bricks"));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut bricks = Bricks::default();
    for b in file {
        let bytes = b64.decode(&b.data).map_err(E::custom)?;
        if bytes.len() != BRICK_VALUES * 2 {
            return Err(E::custom("bad detail brick size"));
        }
        bricks.insert(b.at, Arc::new(brick_values(&bytes, voxel)));
    }
    Ok(bricks)
}

impl Serialize for Detail {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let g = &self.0;
        let levels = g.levels.iter().map(|l| LevelFile { region: l.region.clone(), band: l.band, bricks: Vec::new(), packed: bricks_packed(&l.grid) }).collect();
        DetailFile { voxel: g.voxel, bricks: Vec::new(), packed: bricks_packed(g), levels }.serialize(s)
    }
}

impl Detail {
    /// The grid as a file reads it back: every offset rounded to the
    /// file's 1/256 voxel steps (levels at their region's cell, as they
    /// load). Rounding twice changes nothing, so a quantized grid saves
    /// and loads bit for bit.
    pub fn quantized(&self) -> Detail {
        let round = |g: &DetailGrid, voxel: f32| g.bricks.keys().map(|k| (*k, Arc::new(brick_values(&brick_bytes(g, k), voxel)))).collect();
        let g = &self.0;
        let levels = g.levels.iter().map(|l| Level { region: l.region.clone(), band: l.band, grid: DetailGrid { voxel: l.region.cell, bricks: round(&l.grid, l.region.cell), levels: Vec::new(), solid: Default::default(), clean: false } }).collect();
        Detail(Arc::new(DetailGrid { voxel: g.voxel, bricks: round(g, g.voxel), levels, solid: Default::default(), clean: false }))
    }

    /// The file form from before bricks were deflated (for size checks).
    #[cfg(test)]
    pub(crate) fn raw_file_json(&self) -> String {
        let g = &self.0;
        let levels = g.levels.iter().map(|l| LevelFile { region: l.region.clone(), band: l.band, bricks: bricks_file(&l.grid), packed: None }).collect();
        serde_json::to_string(&DetailFile { voxel: g.voxel, bricks: bricks_file(g), packed: None, levels }).expect("json")
    }
}

impl<'de> Deserialize<'de> for Detail {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        inflate_budget(INFLATE_BUDGET, || {
            let file = DetailFile::deserialize(d)?;
            if file.levels.len() > MAX_REGIONS {
                return Err(serde::de::Error::custom("too many detail regions"));
            }
            // Every level counts toward MAX_TOTAL_BRICKS as it is read.
            let bricks = bricks_unpack(file.packed, file.bricks, file.voxel, MAX_BRICKS)?;
            let mut total = bricks.len();
            let mut levels = Vec::new();
            for l in file.levels {
                let max = MAX_BRICKS.min(MAX_TOTAL_BRICKS - total);
                let grid = DetailGrid { voxel: l.region.cell, bricks: bricks_unpack(l.packed, l.bricks, l.region.cell, max)?, levels: Vec::new(), solid: Default::default(), clean: false };
                total += grid.bricks.len();
                levels.push(Level { region: l.region, band: l.band, grid });
            }
            Ok(Detail(Arc::new(DetailGrid { voxel: file.voxel, bricks, levels, solid: Default::default(), clean: false })))
        })
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
        let (geo, lo, hi) = Geo::new(&blob.form, &Default::default());
        assert!((geo.sdf(Vec3::ZERO) + 0.05).abs() < 1e-6);
        assert!((geo.sdf(Vec3::Z * 0.2) - 0.15).abs() < 1e-6);
        assert!((geo.sdf(Vec3::new(1.03, 0.0, 0.04))).abs() < 1e-6);
        assert!(lo.z <= -0.05 && hi.z >= 0.05);
        let mut form = blob.form;
        if let Form::Membrane { scallop, scallop_edges, .. } = &mut form {
            *scallop = 0.2;
            *scallop_edges = vec![1];
        }
        let (cut, _, _) = Geo::new(&form, &Default::default());
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
        let (geo, lo, hi) = Geo::new(&c.blobs[0].form, &c.assets);
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
    fn region(name: &str, center: Vec3, radii: Vec3, cell: f32) -> Region {
        Region { name: name.into(), center, radii, cell }
    }

    /// A level's grid points inside `lo..=hi` set to a pattern (by point).
    fn sculpt_level(g: &mut DetailGrid, k: usize, lo: Vec3, hi: Vec3, scale: f32) {
        let v = g.levels[k].grid.voxel;
        let (a, b) = ((lo / v).ceil().as_ivec3(), (hi / v).floor().as_ivec3());
        for z in a.z..=b.z {
            for y in a.y..=b.y {
                for x in a.x..=b.x {
                    g.levels[k].grid.set([x, y, z], scale * (1.0 + ((x * 7 + y * 3 + z) % 5) as f32));
                }
            }
        }
    }

    fn offsets(g: &DetailGrid, lo: Vec3, hi: Vec3, step: f32) -> Vec<f32> {
        let n = ((hi - lo) / step).ceil().as_ivec3();
        let mut out = Vec::new();
        for z in 0..=n.z {
            for y in 0..=n.y {
                for x in 0..=n.x {
                    out.push(g.offset(lo + Vec3::new(x as f32, y as f32, z as f32) * step));
                }
            }
        }
        out
    }

    #[test]
    fn a_grown_region_keeps_its_finer_detail() {
        let mut g = DetailGrid { voxel: 0.01, ..Default::default() };
        g.set_regions(&[region("a", Vec3::ZERO, Vec3::splat(0.1), 0.002)]).unwrap();
        let (lo, hi) = (Vec3::splat(-0.04), Vec3::splat(0.04));
        sculpt_level(&mut g, 0, lo, hi, 1e-4);
        let want = offsets(&g, lo, hi, 0.0013);
        // Grown (and renamed, as Sculptris does): the same offsets, exactly.
        g.set_regions(&[region("b", Vec3::new(0.05, 0.0, 0.0), Vec3::new(0.2, 0.12, 0.12), 0.002)]).unwrap();
        assert!(g.levels[0].grid.brick_count() > 0);
        assert_eq!(offsets(&g, lo, hi, 0.0013), want);
        // Removed: only the coarse grid's (none here) is left.
        g.set_regions(&[]).unwrap();
        assert!(offsets(&g, lo, hi, 0.0013).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn merged_regions_keep_both_details() {
        let mut g = DetailGrid { voxel: 0.01, ..Default::default() };
        let (a, b) = (region("a", Vec3::ZERO, Vec3::splat(0.1), 0.002), region("b", Vec3::new(0.12, 0.0, 0.0), Vec3::splat(0.1), 0.002));
        g.set_regions(&[a, b]).unwrap();
        let (lo0, hi0) = (Vec3::splat(-0.04), Vec3::splat(0.04));
        let (lo1, hi1) = (Vec3::new(0.08, -0.04, -0.04), Vec3::new(0.16, 0.04, 0.04));
        sculpt_level(&mut g, 0, lo0, hi0, 1e-4);
        sculpt_level(&mut g, 1, lo1, hi1, 2e-4);
        let (w0, w1) = (offsets(&g, lo0, hi0, 0.0013), offsets(&g, lo1, hi1, 0.0013));
        g.set_regions(&[region("ab", Vec3::new(0.06, 0.0, 0.0), Vec3::new(0.3, 0.2, 0.2), 0.002)]).unwrap();
        assert_eq!(g.levels.len(), 1);
        for (got, want) in [(offsets(&g, lo0, hi0, 0.0013), w0), (offsets(&g, lo1, hi1, 0.0013), w1)] {
            assert!(got.iter().zip(&want).all(|(x, y)| (x - y).abs() <= 1e-7), "lost in the merge");
        }
        // Merged at a finer cell (past sixteen regions): kept to tolerance.
        let want = offsets(&g, lo0, hi0, 0.0013);
        g.set_regions(&[region("fine", Vec3::new(0.06, 0.0, 0.0), Vec3::new(0.3, 0.2, 0.2), 0.001)]).unwrap();
        assert_eq!(g.levels[0].grid.voxel, 0.001);
        let got = offsets(&g, lo0, hi0, 0.0013);
        assert!(got.iter().zip(&want).all(|(x, y)| (x - y).abs() <= 1e-6), "lost in the finer merge");
    }

    #[test]
    fn a_layer_follows_a_grown_region_with_its_detail() {
        let mut base = Detail::new(0.01);
        base.grid_mut().set_regions(&[region("a", Vec3::ZERO, Vec3::splat(0.1), 0.002)]).unwrap();
        let mut layer = base.clone();
        let (lo, hi) = (Vec3::splat(-0.04), Vec3::splat(0.04));
        sculpt_level(layer.grid_mut(), 0, lo, hi, 1e-4);
        let want = offsets(layer.grid(), lo, hi, 0.0013);
        base.grid_mut().set_regions(&[region("sculptris1", Vec3::new(0.02, 0.0, 0.0), Vec3::splat(0.15), 0.002)]).unwrap();
        let moved = layer.relaid_like(&base).unwrap();
        assert!(moved.same_layout(&base));
        assert_eq!(offsets(moved.grid(), lo, hi, 0.0013), want);
    }
}
