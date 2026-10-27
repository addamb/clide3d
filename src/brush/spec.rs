//! The authored stroke: what an agent (or, later, the UI) sends in a
//! `sculpt` or `paint` call, and what a record stores. It reads today's
//! sculpt and paint JSON unchanged (`point`, `mirror`, top-level paint
//! `color`/`hardness`/`erase`) and writes one canonical form, so
//! JSON -> spec -> JSON -> spec is the identity.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use super::alpha::AlphaRef;
use super::curve::Falloff;
use super::path::PathPoint;
use super::pressure::PressureCurves;
use super::symmetry::{Axis, Symmetry};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Brush {
    Add,
    Carve,
    Inflate,
    Deflate,
    Smooth,
    Flatten,
    Grab,
    Noise,
    Crease,
    Pinch,
    Layer,
    #[serde(alias = "extrude")]
    Stretch,
    /// Raise (or with invert, carve) along the surface's area normal,
    /// dab by dab.
    Standard,
    /// Color only: the stroke paints and leaves the shape alone.
    Paint,
    /// Fills hollows up to the plane of the area under the dab (its mean
    /// surface), with a square footprint; it never cuts.
    Clay,
    /// Builds a layer: fills up to a tenth of the radius over the area's
    /// plane, so hollows fill first and passes build up.
    ClayBuildup,
    /// Lays a strip of even height (strength x radius / 2) along the path
    /// over the surface the stroke started on, flat across (a square
    /// tube); overlapping dabs do not build up.
    ClayTubes,
    /// Cuts what stands above the area's plane, with a sharp edge; it
    /// never adds.
    TrimDynamic,
    /// Presses both ways onto the area's plane with a sharp edge (hPolish).
    Polish,
    /// Curvature flow under each dab that keeps creases, edges and long
    /// lines (`sculpt::flow`): lumps go, intended lines stay. Strength
    /// sets the passes per dab; `angle` and `keep_volume` tune it.
    ClayPolish,
    /// Pulls a horn out along the path from its first point, curling it
    /// by `curl_deg` on the way.
    Snakehook,
    /// Paints the persistent sculpt mask (the `mask` tool's strokes):
    /// raises it toward strength by the falloff; invert lowers it.
    MaskPaint,
    /// Paints a polygroup id (the `group` tool's strokes).
    GroupPaint,
    /// Moves, turns and scales the clay's material by `transform`, each
    /// point by 1 - mask (the gizmo with a mask; Transpose).
    Transform,
    /// Stamps a VDM (`alpha create_vdm`, named by `alpha`) on the surface
    /// under each dab, its width the dab's (`brush::vdm`); invert carves it.
    Vdm,
}

impl Brush {
    /// Brushes applied as separate dabs spaced along the path; the others
    /// (paint too) sweep the whole path at once.
    pub fn dabs(self) -> bool {
        matches!(self, Brush::Standard | Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish | Brush::Vdm | Brush::ClayPolish)
    }

    /// Dab brushes that work toward the plane of the area under the dab.
    pub fn planar(self) -> bool {
        matches!(self, Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish)
    }

    /// The footprint is a square in the dab's tangent frame, not a circle.
    pub fn square(self) -> bool {
        matches!(self, Brush::Clay | Brush::ClayBuildup | Brush::ClayTubes)
    }

    /// Strength is a signed scene-unit depth or height, not 0..1.
    pub fn signed_depth(self) -> bool {
        matches!(self, Brush::Crease | Brush::Layer)
    }

    /// Brushes that write the mask or groups grid rather than the shape.
    pub fn marks(self) -> bool {
        matches!(self, Brush::MaskPaint | Brush::GroupPaint)
    }
}

/// How the points become dabs.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StrokeKind {
    /// Along the path (dab brushes: every `spacing` x radius).
    #[default]
    Path,
    /// One dab at each point, no spacing.
    Dots,
    /// `count` dabs scattered within `jitter` x radius of each spaced
    /// path point, drawn from `seed` (the same seed, the same dabs).
    Spray {
        #[serde(default = "spray_count")]
        count: u32,
        #[serde(default = "half")]
        jitter: f32,
        #[serde(default)]
        seed: u64,
    },
    /// One dab at `from` as wide as the drag to `to` (DragRect).
    DragRect { from: Vec3, to: Vec3 },
    /// One dab where the drag ends (the last point; DragDot).
    DragDot,
    /// The points are a curve's control points: the stroke runs along a
    /// smooth curve through them (closed: back to the first), as a path.
    /// Records keep the control points, so a stroke_edit replace with one
    /// of them moved is MoveCurve.
    Curve {
        #[serde(default)]
        closed: bool,
    },
}

fn spray_count() -> u32 {
    4
}

fn half() -> f32 {
    0.5
}

impl StrokeKind {
    fn is_path(&self) -> bool {
        *self == StrokeKind::Path
    }
}

/// What the points are given in.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    /// The call's `space` (world unless it says local).
    World,
    /// The clay object's own space, whatever the call's `space`.
    Object,
    /// Snapped onto the clay surface, then `offset` out along its normal.
    Surface {
        #[serde(default)]
        offset: f32,
    },
    /// Rays into the view hit the clay; see `Screen`.
    Screen(Box<Screen>),
    /// Each point (in the call's space) cast onto the clay along a ray; see
    /// `Project`. The record keeps the points where they landed.
    Project(Box<Project>),
}

/// A projected frame: each point goes to where a ray from it first meets
/// the clay, so a path drawn a little off the surface stays on the sheet
/// under it (nearest-point snapping jumps between two blobs at a concave
/// junction).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// The ray's direction: [x, y, z] in the call's space, "view:front"
    /// (back, left, right, top, bottom: that view's rays), or "plane": in
    /// `plane` through each point, across the path there.
    pub along: Along,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<ProjectPlane>,
    /// Also cast the other way; the nearer hit wins (default true).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub both_ways: bool,
    /// Hits farther than this are misses (default 4 radii).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<f32>,
    /// Lift each point this far along the surface normal.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f32,
}

/// `Project::along`: a direction, or a word (`view:<name>`, `plane`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(untagged)]
pub enum Along {
    Dir(Vec3),
    Word(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct ProjectPlane {
    pub normal: Vec3,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Screen points: `at` [h, v] pairs in a named orthographic view (world
/// units across the view, like surface_point), or `px` pixels through a
/// stored `camera`. A third number in a pair is pressure.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub px: Vec<Vec<f32>>,
}

/// A camera: world-to-view and projection matrices (wgpu depth 0..1) and
/// the viewport size in pixels.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub view: Mat4,
    pub proj: Mat4,
    pub viewport: [f32; 2],
}

impl Camera {
    /// The world ray (origin on the near plane, unit direction) through
    /// pixel (x, y): how screen frames and the editor's stroke recorder
    /// both turn a pixel into a ray.
    pub fn ray(&self, x: f32, y: f32) -> (Vec3, Vec3) {
        let [w, h] = self.viewport;
        let inv = (self.proj * self.view).inverse();
        let ndc = |z: f32| Vec3::new(2.0 * x / w - 1.0, 1.0 - 2.0 * y / h, z);
        let (a, b) = (inv.project_point3(ndc(0.0)), inv.project_point3(ndc(1.0)));
        (a, (b - a).normalize_or_zero())
    }
}

/// Which side of the plane flatten works on.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Cut down what stands above the plane only.
    Above,
    /// Fill up to the plane only.
    Below,
    #[default]
    Both,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SmoothMode {
    /// Half as strong.
    Gentle,
    #[default]
    Normal,
    /// Only lowers: takes bumps down, never fills dips.
    Peaks,
    /// Only raises: fills dips, never takes bumps down.
    Valleys,
}

/// Color laid by a stroke (paint strokes, and sculpt strokes that also
/// color: MRGB).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct PaintArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// 0 soft edge, 1 hard edge (default 0.5); ignored with a falloff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardness: Option<f32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub erase: bool,
    /// Opacity of the color (default 1) when a sculpt stroke paints too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
}

/// Where a stroke may work: hard limits evaluated on the pre-stroke field
/// in object space (blobs, above, below), the clay's persistent sculpt
/// mask (`channel`), its polygroups, and auto-masking read from the
/// surface around each dab (backface, cavity, connected, normal_angle).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct StrokeMask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<f32>,
    /// Whether the persistent sculpt mask holds the brush back (default
    /// true: every point moves by 1 - mask).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<bool>,
    /// Work where the persistent mask is, not where it is not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert_channel: bool,
    /// Leave surface facing away from the dab alone (the far side of a
    /// thin wall).
    #[serde(default, skip_serializing_if = "is_false")]
    pub backface: bool,
    /// -1..1: positive spares cavities (by that much), negative spares
    /// ridges (the smart materials' cavity estimate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cavity: Option<f32>,
    /// Only surface joined to the dab's center within 1.5 radii (Move
    /// Topological: one finger, not its neighbour).
    #[serde(default, skip_serializing_if = "is_false")]
    pub connected: bool,
    /// Only these polygroup ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<u16>>,
    /// Only surface within this many degrees of the dab's normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_angle: Option<f32>,
}

impl StrokeMask {
    /// Auto-masking that reads the surface around each dab.
    pub fn auto(&self) -> bool {
        self.backface || self.cavity.is_some() || self.connected || self.normal_angle.is_some()
    }

    /// Whether the stroke reads the persistent mask.
    pub fn channel(&self) -> bool {
        self.channel.unwrap_or(true)
    }
}

/// The `transform` brush's move, in the call's space: scale, then turn
/// (degrees about X, then Y, then Z), both about `pivot`, then move by
/// `translate`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Affine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<Vec3>,
    /// One factor, or one per axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pivot: Option<Vec3>,
    /// A whole matrix (column-major, 16 numbers) instead of the parts
    /// (what the gizmo sends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Mat4>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(untagged)]
pub enum Scale {
    Uniform(f32),
    Axes(Vec3),
}

impl Affine {
    /// The forward map in the call's space.
    pub fn matrix(&self) -> Result<Mat4, String> {
        if let Some(m) = self.matrix {
            let parts = self.translate.is_some() || self.rotate.is_some() || self.scale.is_some() || self.pivot.is_some();
            if parts {
                return Err("transform takes matrix or translate/rotate/scale/pivot, not both".into());
            }
            if !m.is_finite() || m.determinant().abs() < 1e-6 || m.row(3) != glam::Vec4::W {
                return Err("transform matrix must be a finite, invertible affine matrix".into());
            }
            return Ok(m);
        }
        let s = match self.scale {
            None => Vec3::ONE,
            Some(Scale::Uniform(s)) => Vec3::splat(s),
            Some(Scale::Axes(v)) => v,
        };
        let r = self.rotate.unwrap_or(Vec3::ZERO);
        let (t, pivot) = (self.translate.unwrap_or(Vec3::ZERO), self.pivot.unwrap_or(Vec3::ZERO));
        if !(s.is_finite() && r.is_finite() && t.is_finite() && pivot.is_finite()) || s.cmple(Vec3::splat(1e-3)).any() || s.cmpgt(Vec3::splat(1e3)).any() || r.abs().cmpgt(Vec3::splat(3600.0)).any() {
            return Err("transform needs finite translate, rotate (degrees) and pivot, and scale 0.001 to 1000".into());
        }
        let q = glam::Quat::from_euler(glam::EulerRot::ZYX, r.z.to_radians(), r.y.to_radians(), r.x.to_radians());
        Ok(Mat4::from_translation(t + pivot) * Mat4::from_quat(q) * Mat4::from_scale(s) * Mat4::from_translation(-pivot))
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The authored stroke (docs/SCULPT_PLAN.md 3.2).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(try_from = "SpecFile")]
pub struct StrokeSpec {
    pub brush: Brush,
    #[serde(default, skip_serializing_if = "StrokeKind::is_path")]
    pub kind: StrokeKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<PathPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    pub radius: f32,
    /// Default 0.5 for sculpt brushes, 1 (opacity) for paint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub falloff: Option<Falloff>,
    #[serde(default, skip_serializing_if = "PressureCurves::is_default")]
    pub pressure: PressureCurves,
    /// Dab spacing as a fraction of the radius (default 0.15 for dab
    /// brushes; path brushes sweep the path as given unless set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<f32>,
    /// LazyMouse string length as a fraction of the radius, 0..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetry: Option<Symmetry>,
    /// Fade in over the first and out over the last fraction of the path
    /// (Dam Standard: crease with taper [0.2, 0.2]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taper: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<StrokeMask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smooth_mode: Option<SmoothMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<PaintArgs>,
    /// Move every point onto the nearest clay surface first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snap: Option<bool>,
    /// An alpha image shaping each dab (dab brushes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<AlphaRef>,
    /// snakehook: degrees the horn curls from its base to its tip, about
    /// `normal` (default: level, across the pull).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curl_deg: Option<f32>,
    /// transform: the move (its radius is unused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Affine>,
    /// Sculptris Pro: a detail cell finer than the clay's; the `sculpt`
    /// call grows a detail region of that cell over the stroke first (see
    /// `clay::sculptris`), so the stroke lands on the finer grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<f32>,
    /// The detail level the stroke works at (`detail_level`; 0 is the
    /// clay's own cell): it writes that level and the coarser ones, and
    /// carries the finer ones along (see `sculpt::engine`). The `sculpt`
    /// call fills it in from the clay's active level and stores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// clay_polish: degrees; a normal turning faster than this across a
    /// cell is a crease and is kept (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f32>,
    /// clay_polish: keep the enclosed volume (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_volume: Option<bool>,
}

/// What a stroke may be read from: the canonical fields plus today's.
#[derive(Deserialize)]
struct SpecFile {
    brush: Brush,
    #[serde(default)]
    kind: StrokeKind,
    #[serde(default)]
    points: Vec<PathPoint>,
    #[serde(default)]
    point: Option<PathPoint>,
    #[serde(default)]
    frame: Option<Frame>,
    /// Required but for the transform brush.
    #[serde(default)]
    radius: Option<f32>,
    #[serde(default)]
    strength: Option<f32>,
    #[serde(default)]
    invert: bool,
    #[serde(default)]
    falloff: Option<Falloff>,
    #[serde(default)]
    pressure: PressureCurves,
    #[serde(default)]
    spacing: Option<f32>,
    #[serde(default)]
    lazy: Option<f32>,
    #[serde(default)]
    symmetry: Option<Symmetry>,
    #[serde(default)]
    mirror: bool,
    #[serde(default)]
    taper: Option<[f32; 2]>,
    #[serde(default)]
    mask: Option<StrokeMask>,
    #[serde(default)]
    side: Option<Side>,
    #[serde(default)]
    smooth_mode: Option<SmoothMode>,
    #[serde(default)]
    delta: Option<Vec3>,
    #[serde(default)]
    normal: Option<Vec3>,
    #[serde(default)]
    scale: Option<f32>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    paint: Option<PaintArgs>,
    #[serde(default)]
    color: Option<[f32; 3]>,
    #[serde(default)]
    hardness: Option<f32>,
    #[serde(default)]
    erase: bool,
    #[serde(default)]
    snap: Option<bool>,
    #[serde(default)]
    alpha: Option<AlphaRef>,
    #[serde(default)]
    curl_deg: Option<f32>,
    #[serde(default)]
    transform: Option<Affine>,
    #[serde(default)]
    cell: Option<f32>,
    #[serde(default)]
    level: Option<u8>,
    #[serde(default)]
    angle: Option<f32>,
    #[serde(default)]
    keep_volume: Option<bool>,
}

impl TryFrom<SpecFile> for StrokeSpec {
    type Error = String;

    fn try_from(f: SpecFile) -> Result<StrokeSpec, String> {
        let mut points = f.points;
        if let Some(p) = f.point {
            if !points.is_empty() {
                return Err("give point or points, not both".into());
            }
            points.push(p);
        }
        // mirror: true is symmetry across X.
        let mut symmetry = f.symmetry;
        if f.mirror {
            let s = symmetry.get_or_insert_with(Symmetry::default);
            if !s.axes.contains(&Axis::X) {
                s.axes.insert(0, Axis::X);
            }
        }
        // Paint's top-level color, hardness and erase.
        let mut paint = f.paint;
        if f.color.is_some() || f.hardness.is_some() || f.erase {
            let p = paint.get_or_insert_with(PaintArgs::default);
            p.color = p.color.or(f.color);
            p.hardness = p.hardness.or(f.hardness);
            p.erase |= f.erase;
        }
        let radius = match (f.radius, f.brush) {
            (Some(r), _) => r,
            (None, Brush::Transform) => 1.0,
            (None, _) => return Err("missing field `radius`".into()),
        };
        Ok(StrokeSpec {
            brush: f.brush,
            kind: f.kind,
            points,
            frame: f.frame,
            radius,
            strength: f.strength,
            invert: f.invert,
            falloff: f.falloff,
            pressure: f.pressure,
            spacing: f.spacing,
            lazy: f.lazy,
            symmetry,
            taper: f.taper,
            mask: f.mask,
            side: f.side,
            smooth_mode: f.smooth_mode,
            delta: f.delta,
            normal: f.normal,
            scale: f.scale,
            pattern: f.pattern,
            paint,
            snap: f.snap,
            alpha: f.alpha,
            curl_deg: f.curl_deg,
            transform: f.transform,
            cell: f.cell,
            level: f.level,
            angle: f.angle,
            keep_volume: f.keep_volume,
        })
    }
}

impl StrokeSpec {
    /// A plain stroke: `brush` along `points`, everything else default.
    #[cfg(test)]
    pub fn new(brush: Brush, points: &[Vec3], radius: f32) -> StrokeSpec {
        StrokeSpec {
            brush,
            kind: StrokeKind::Path,
            points: points.iter().map(|p| PathPoint::at(*p)).collect(),
            frame: None,
            radius,
            strength: None,
            invert: false,
            falloff: None,
            pressure: PressureCurves::default(),
            spacing: None,
            lazy: None,
            symmetry: None,
            taper: None,
            mask: None,
            side: None,
            smooth_mode: None,
            delta: None,
            normal: None,
            scale: None,
            pattern: None,
            paint: None,
            snap: None,
            alpha: None,
            curl_deg: None,
            transform: None,
            cell: None,
            level: None,
            angle: None,
            keep_volume: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::alpha::{Mode, Use};
    use crate::brush::curve::Preset;
    use crate::brush::pressure::PressureCurve;
    use crate::brush::symmetry::{About, Radial};
    use serde_json::json;

    /// Every optional field set.
    fn full() -> StrokeSpec {
        StrokeSpec {
            brush: Brush::Standard,
            kind: StrokeKind::Spray { count: 3, jitter: 0.4, seed: 7 },
            points: serde_json::from_value(json!([[0, 1, 0], [0.1, 1, 0, 0.5], {"p": [0.2, 1, 0], "normal": [0, 1, 0], "t": 0.1}])).unwrap(),
            frame: Some(Frame::Surface { offset: 0.01 }),
            radius: 0.05,
            strength: Some(0.4),
            invert: true,
            falloff: Some(Falloff { preset: Preset::Flat, curve: None, focal_shift: 0.25 }),
            pressure: PressureCurves { size: PressureCurve::Off, strength: PressureCurve::Points(vec![[0.0, 0.2], [1.0, 1.0]]) },
            spacing: Some(0.2),
            lazy: Some(0.3),
            symmetry: Some(Symmetry { axes: vec![Axis::X, Axis::Y], radial: Some(Radial { axis: Axis::Y, count: 6 }), about: About::Origin, posable: true, frame: Some(crate::brush::symmetry::PoseFrame::At(12.5)) }),
            taper: Some([0.2, 0.3]),
            mask: Some(StrokeMask { blobs: Some(vec!["head".into()]), above: Some(0.1), below: Some(0.9), channel: Some(false), invert_channel: true, backface: true, cavity: Some(0.5), connected: true, groups: Some(vec![2, 3]), normal_angle: Some(40.0) }),
            side: Some(Side::Above),
            smooth_mode: Some(SmoothMode::Peaks),
            delta: Some(Vec3::new(0.0, 0.1, 0.0)),
            normal: Some(Vec3::Z),
            scale: Some(0.01),
            pattern: Some("pores".into()),
            paint: Some(PaintArgs { color: Some([1.0, 0.5, 0.0]), hardness: Some(0.7), erase: false, strength: Some(0.8) }),
            snap: Some(true),
            alpha: Some(AlphaRef { name: Some("checker".into()), rotate: 15.0, scale: 1.5, mid: 0.5, depth: Some(0.002), mode: Mode::Drag, usage: Some(Use::Mask), ..AlphaRef::default() }),
            curl_deg: Some(45.0),
            transform: Some(Affine { translate: Some(Vec3::X), rotate: Some(Vec3::new(0.0, 30.0, 0.0)), scale: Some(Scale::Uniform(1.5)), pivot: Some(Vec3::Y), matrix: None }),
            cell: Some(0.001),
            level: Some(2),
            angle: Some(25.0),
            keep_volume: Some(false),
        }
    }

    #[test]
    fn json_round_trip_is_the_identity() {
        let mut specs = vec![full(), StrokeSpec::new(Brush::Inflate, &[Vec3::ONE], 0.1)];
        let mut screen = full();
        screen.frame = Some(Frame::Screen(Box::new(Screen { view: None, camera: Some(Camera { view: Mat4::look_at_rh(Vec3::Z * 3.0, Vec3::ZERO, Vec3::Y), proj: Mat4::perspective_rh(0.8, 1.5, 0.1, 100.0), viewport: [1200.0, 800.0] }), at: vec![], px: vec![vec![600.0, 400.0, 0.7]] })));
        screen.kind = StrokeKind::DragRect { from: Vec3::ZERO, to: Vec3::X };
        specs.push(screen);
        let mut curve = full();
        curve.falloff = Some(Falloff { curve: Some(vec![[0.0, 1.0], [0.6, 0.7], [1.0, 0.0]]), ..Falloff::default() });
        curve.kind = StrokeKind::DragDot;
        curve.symmetry.as_mut().unwrap().frame = Some(crate::brush::symmetry::PoseFrame::Rest(crate::brush::symmetry::RestWord::Rest));
        specs.push(curve);
        let mut vdm = StrokeSpec::new(Brush::Vdm, &[Vec3::ZERO, Vec3::X, Vec3::Y], 0.1);
        vdm.kind = StrokeKind::Curve { closed: true };
        vdm.alpha = Some(AlphaRef { name: Some("horn".into()), rotate: 30.0, ..AlphaRef::default() });
        specs.push(vdm);
        let mut projected = StrokeSpec::new(Brush::ClayPolish, &[Vec3::ZERO, Vec3::X], 0.1);
        projected.frame = Some(Frame::Project(Box::new(Project { along: Along::Word("plane".into()), plane: Some(ProjectPlane { normal: Vec3::X }), both_ways: false, reach: Some(0.3), offset: 0.01 })));
        specs.push(projected.clone());
        projected.frame = Some(Frame::Project(Box::new(Project { along: Along::Dir(Vec3::NEG_Z), plane: None, both_ways: true, reach: None, offset: 0.0 })));
        specs.push(projected);
        for s in specs {
            let text = serde_json::to_value(&s).unwrap();
            let back: StrokeSpec = serde_json::from_value(text.clone()).unwrap();
            assert_eq!(back, s, "{text}");
            assert_eq!(serde_json::to_value(&back).unwrap(), text);
        }
    }

    #[test]
    fn todays_sculpt_and_paint_json_still_reads() {
        let s: StrokeSpec = serde_json::from_value(json!({"brush": "extrude", "point": [0, 1, 0], "radius": 0.1, "mirror": true, "delta": [0, 0.1, 0], "snap": true, "mask": {"blobs": ["a"], "above": 0.2}})).unwrap();
        assert_eq!(s.brush, Brush::Stretch);
        assert_eq!(s.points.len(), 1);
        assert_eq!(s.symmetry.as_ref().unwrap().axes, vec![Axis::X]);
        let p: StrokeSpec = serde_json::from_value(json!({"brush": "paint", "points": [[0, 1, 0]], "radius": 0.1, "color": [1, 0, 0], "hardness": 0.9})).unwrap();
        assert_eq!(p.paint.unwrap().color, Some([1.0, 0.0, 0.0]));
        for bad in [json!({"brush": "add", "radius": 0.1, "mask": {"blob": ["a"]}}), json!({"brush": "add", "radius": 0.1, "point": [0, 0, 0], "points": [[0, 0, 0]]}), json!({"brush": "sand", "radius": 0.1}), json!({"brush": "add", "radius": 0.1, "kind": {"spray": {"cnt": 2}}})] {
            assert!(serde_json::from_value::<StrokeSpec>(bad.clone()).is_err(), "{bad}");
        }
    }
}
