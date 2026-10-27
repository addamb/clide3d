//! The resolver: an authored `StrokeSpec` becomes the dabs the engine
//! applies, in the clay object's space. Everything that depends on where
//! the stroke came from happens here, once: the frame (world, object,
//! surface or screen) to object space, snapping, LazyMouse, spacing,
//! pressure curves, taper, kinds (dots, spray, drag) and symmetry. The
//! engine then only sees object-space dabs, so a resolved stroke applies
//! the same way whatever the camera, snapper or UI state.

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::alpha::{AlphaParams, AlphaRef, Mode};
use super::curve::{Curve, Falloff, Preset};
use super::path::{self, Sample};
use super::rng::Pcg32;
use super::spec::{Along, Brush, Frame, PaintArgs, Project, Side, SmoothMode, StrokeKind, StrokeMask, StrokeSpec};

/// Most points an authored stroke may give.
pub const MAX_POINTS: usize = 512;
/// Most dabs one stroke may resolve to, symmetric copies included.
pub const MAX_DABS: usize = 4096;
/// Most points LazyMouse may densify a stroke's path to (path brushes
/// then bring it back under `MAX_POINTS`).
pub const MAX_SAMPLES: usize = 65536;
/// Default dab spacing, as a fraction of the radius.
pub const DEFAULT_SPACING: f32 = 0.15;
/// Longest snakehook path, in radii.
pub const MAX_HOOK_RADII: f32 = 16.0;

/// Snaps an object-space point onto the clay: (point, normal).
pub type Snap<'a> = &'a dyn Fn(Vec3) -> Option<(Vec3, Vec3)>;
/// First clay hit of an object-space ray (origin, direction): (point, normal).
pub type Cast<'a> = &'a dyn Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>;
/// Binds an authored alpha to its image's content hash (the scene's
/// library by name, or a hash the scene or the clay holds).
pub type AlphaLookup<'a> = &'a dyn Fn(&AlphaRef) -> Result<super::super::ledger::Hash128, String>;
/// Posable symmetry: for a point in the call's space on the posed clay,
/// the map from the call's space back to the rest pose (object space)
/// there. Points, directions and normals near it all go through it.
pub type Unpose<'a> = &'a dyn Fn(Vec3) -> Option<Mat4>;

/// Where a stroke is resolved: the target clay and how to reach it.
pub struct ResolveCtx<'a> {
    /// The call's space (world unless `space: local`) to object space.
    pub to_local: Mat4,
    /// Radii in the call's space times this are object-space radii.
    pub radius_scale: f32,
    /// World to object space (screen rays, symmetry about the origin).
    pub world_to_local: Mat4,
    pub snap: Option<Snap<'a>>,
    pub cast: Option<Cast<'a>>,
    /// How far out named-view rays start (world units).
    pub view_far: f32,
    /// The call's `snap`, for strokes that do not say.
    pub snap_call: Option<bool>,
    /// Single-point strokes snap unless they say otherwise (paint).
    pub snap_single: bool,
    pub alpha: Option<AlphaLookup<'a>>,
    pub unpose: Option<Unpose<'a>>,
    /// The clay's detail levels' cells, coarse first (`levels::cells`):
    /// what a stroke's `level` resolves to.
    pub levels: Option<&'a [f32]>,
    /// The clay's middle in object space: which way a projected frame's
    /// one-way plane rays go (toward it).
    pub center: Option<Vec3>,
}

impl ResolveCtx<'_> {
    /// Points already in object space, nothing to snap to.
    #[cfg(test)]
    pub fn local() -> ResolveCtx<'static> {
        ResolveCtx { to_local: Mat4::IDENTITY, radius_scale: 1.0, world_to_local: Mat4::IDENTITY, snap: None, cast: None, view_far: 100.0, snap_call: None, snap_single: false, alpha: None, unpose: None, levels: None, center: None }
    }
}

/// Stroke-wide brush settings, after defaults.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Params {
    pub brush: Brush,
    pub curve: Curve,
    /// As authored (signed depth for crease/layer, already in object units).
    pub strength: f32,
    pub invert: bool,
    pub scale: Option<f32>,
    pub pattern: Option<String>,
    pub side: Side,
    pub smooth_mode: SmoothMode,
    /// The alpha each dab samples, bound to its image by hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<AlphaParams>,
    /// transform: the forward map in object space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Mat4>,
    /// The cell of the detail level the stroke works at (resolved from
    /// its `level`): it writes the levels at least this coarse and
    /// carries the finer ones along. None: it writes every level it
    /// reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
    /// clay_polish: the crease angle in degrees and whether the volume is
    /// kept, resolved (None for every other brush, so their records hash
    /// as before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_volume: Option<bool>,
}

impl Params {
    /// A path brush's settings with the original curve.
    #[cfg(test)]
    pub fn new(brush: Brush, strength: f32) -> Params {
        Params { brush, curve: Curve::Legacy, strength, invert: false, scale: None, pattern: None, side: Side::Both, smooth_mode: SmoothMode::Normal, alpha: None, transform: None, level: None, angle: None, keep_volume: None }
    }
}

/// One unit of engine work, in object space: a single dab, or for path
/// brushes (add, crease, inflate ...) the whole polyline swept at once.
/// Reading one checks it (`Dab::check`): records come from files.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(try_from = "DabFile")]
pub struct Dab {
    pub points: Vec<Vec3>,
    /// The largest radius on the dab.
    pub radius: f32,
    /// Per point radii (pressure); None: `radius` everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radii: Option<Vec<f32>>,
    /// Strength factor for the whole dab (pressure, spray).
    pub gain: f32,
    /// Per point strength factors (pressure, taper); None: 1 everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gains: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Vec3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<Vec3>,
    /// Which way the dab's frame points across the surface: the drag
    /// direction (drag_rect, dab brushes along a path), else object X.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tangent: Option<Vec3>,
    /// Which way an upright alpha (stamp and tile modes) runs across: object
    /// X, carried through the dab's symmetric copy. None: object X.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upright: Option<Vec3>,
    /// A mirrored copy: its frame is left-handed, so an alpha on it is the
    /// mirror image (see `alpha::frame`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip: bool,
}

/// What `Dab` reads from a record, before `Dab::check`.
#[derive(Deserialize)]
struct DabFile {
    points: Vec<Vec3>,
    radius: f32,
    #[serde(default)]
    radii: Option<Vec<f32>>,
    gain: f32,
    #[serde(default)]
    gains: Option<Vec<f32>>,
    #[serde(default)]
    delta: Option<Vec3>,
    #[serde(default)]
    normal: Option<Vec3>,
    #[serde(default)]
    tangent: Option<Vec3>,
    #[serde(default)]
    upright: Option<Vec3>,
    #[serde(default)]
    flip: bool,
}

impl TryFrom<DabFile> for Dab {
    type Error = String;

    fn try_from(f: DabFile) -> Result<Dab, String> {
        let dab = Dab { points: f.points, radius: f.radius, radii: f.radii, gain: f.gain, gains: f.gains, delta: f.delta, normal: f.normal, tangent: f.tangent, upright: f.upright, flip: f.flip };
        dab.check()?;
        Ok(dab)
    }
}

impl Dab {
    /// What the engine needs to sweep a dab without going out of bounds:
    /// points (finite, at least one), a finite radius and gain, and per
    /// point radii and gains, when given, one finite value per point.
    pub fn check(&self) -> Result<(), String> {
        let per_point = |v: &Option<Vec<f32>>| v.as_ref().is_none_or(|v| v.len() == self.points.len() && v.iter().all(|x| x.is_finite()));
        check(!self.points.is_empty() && self.points.iter().all(|p| p.is_finite()), "a dab needs finite points")?;
        check(self.radius.is_finite() && self.gain.is_finite(), "a dab needs a finite radius and gain")?;
        check(per_point(&self.radii) && per_point(&self.gains), "a dab's radii and gains need one finite value per point")
    }

    /// A path dab of one radius and full strength.
    #[cfg(test)]
    pub fn path(points: Vec<Vec3>, radius: f32) -> Dab {
        Dab { points, radius, radii: None, gain: 1.0, gains: None, delta: None, normal: None, tangent: None, upright: None, flip: false }
    }
}

/// Color a stroke lays.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PaintParams {
    pub color: Vec3,
    pub erase: bool,
    pub opacity: f32,
    pub curve: Curve,
}

/// A stroke ready for the engine; a ledger record keeps it, so a replay
/// applies exactly these dabs (no snapping, camera or UI state again).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Resolved {
    pub params: Params,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<StrokeMask>,
    /// Every dab, symmetric copy after copy, in the order they apply.
    pub dabs: Vec<Dab>,
    /// Symmetric copies (1 without symmetry).
    pub instances: usize,
    /// Color the stroke lays (paint strokes, MRGB sculpt strokes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<PaintParams>,
    /// MRGB: the dabs the color follows, the same as a paint stroke with
    /// this spec (the sculpt dabs are spaced and scaled for shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_dabs: Option<Vec<Dab>>,
    /// The persistent mask and polygroups the stroke read, by content
    /// hash (the clay keeps them: `ledger::Frozen`), filled in when it
    /// is committed; None: it read neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<FrozenRef>,
    /// group_paint: the id it paints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u16>,
}

/// The persistent mask and polygroups (with their visibility) a stroke
/// read, by content hash.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct FrozenRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<super::super::ledger::Hash128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<super::super::ledger::Hash128>,
}

/// The cell of a stroke's `level` on the clay it is resolved for.
fn level_cell(spec: &StrokeSpec, ctx: &ResolveCtx) -> Result<Option<f32>, String> {
    let Some(l) = spec.level else { return Ok(None) };
    let cells = ctx.levels.ok_or("level is for sculpt strokes on a clay (detail_level)")?;
    cells.get(l as usize).copied().map(Some).ok_or_else(|| format!("no detail level {l}: the clay has levels 0 to {} (detail_level)", cells.len().saturating_sub(1)))
}

fn check(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.into()) }
}

/// Resolves one authored stroke (see the module doc).
pub fn resolve(spec: &StrokeSpec, ctx: &ResolveCtx) -> Result<Resolved, String> {
    resolve_with(spec, ctx, false)
}

/// `resolve`; `fit` coarsens an explicit spacing that would give a path
/// dab more than `MAX_POINTS` points instead of refusing it (an MRGB
/// stroke's color follows the sculpt dabs, which may be spaced finer).
fn resolve_with(spec: &StrokeSpec, ctx: &ResolveCtx, fit: bool) -> Result<Resolved, String> {
    let brush = spec.brush;
    check(spec.radius.is_finite() && spec.radius > 0.0, "points and radius must be finite, radius > 0")?;
    check(spec.points.len() <= MAX_POINTS, &format!("a stroke needs 1 to {MAX_POINTS} points"))?;
    check(spec.points.iter().all(|p| p.p.is_finite()), "points and radius must be finite, radius > 0")?;
    check(spec.strength.is_none_or(f32::is_finite), "strength must be finite")?;
    check(spec.spacing.is_none_or(|s| (0.01..=4.0).contains(&s)), "spacing must be 0.01 to 4 (a fraction of the radius)")?;
    check(spec.lazy.is_none_or(|l| l == 0.0 || (0.01..=1.0).contains(&l)), "lazy must be 0 (off) or 0.01 to 1")?;
    check(spec.taper.is_none_or(|[a, b]| (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b) && a + b <= 1.0), "taper must be [start, end] fractions 0..1 adding up to at most 1")?;
    check(spec.delta.is_none_or(|d| d.is_finite()) && spec.normal.is_none_or(|n| n.is_finite()), "delta and normal must be finite")?;
    if let Some(m) = &spec.mask {
        check(m.above.is_none_or(f32::is_finite) && m.below.is_none_or(f32::is_finite), "mask heights must be finite")?;
        check(m.cavity.is_none_or(|c| (-1.0..=1.0).contains(&c)), "mask cavity must be -1 to 1")?;
        check(m.normal_angle.is_none_or(|a| (0.0..=180.0).contains(&a)), "mask normal_angle must be 0 to 180 degrees")?;
        check(m.groups.as_ref().is_none_or(|g| g.len() <= 256), "mask groups lists at most 256 ids")?;
        check(m.blobs.as_ref().is_none_or(|b| b.len() <= crate::clay::MAX_BLOBS), "mask blobs lists too many names")?;
        check(!(brush.marks() && (m.auto() || m.groups.is_some() || m.channel.is_some() || m.invert_channel)), "mask and group strokes take blobs/above/below limits only")?;
        check(!(brush == Brush::Noise && m.auto()), "noise strokes take no auto-masking (backface, cavity, connected, normal_angle)")?;
    }
    check(spec.transform.is_none() || brush == Brush::Transform, "transform is for brush transform")?;
    check((spec.angle.is_none() && spec.keep_volume.is_none()) || brush == Brush::ClayPolish, "angle and keep_volume are for brush clay_polish")?;
    check(spec.angle.is_none_or(|a| (5.0..=90.0).contains(&a)), "angle must be 5 to 90 degrees")?;
    check(!(brush == Brush::ClayPolish && spec.alpha.is_some()), "clay_polish takes no alpha")?;
    if matches!(spec.frame, Some(Frame::Project(_))) {
        check(brush != Brush::Transform && !spec.symmetry.as_ref().is_some_and(|s| s.posable), "a projected frame is for strokes on the rest clay (not transform or posable symmetry)")?;
        check(spec.snap != Some(true), "a projected frame puts points on the clay itself; leave snap out")?;
        check(!matches!(spec.kind, StrokeKind::DragRect { .. }), "a projected frame takes points (not drag_rect)")?;
    }
    check(!(brush.marks() && spec.paint.is_some()), "mask and group strokes take no paint")?;
    if brush == Brush::Transform {
        return resolve_transform(spec, ctx);
    }
    if let Some(s) = &spec.symmetry {
        s.validate()?;
    }
    if let StrokeKind::Spray { count, jitter, .. } = spec.kind {
        check((1..=64).contains(&count) && (0.0..=4.0).contains(&jitter), "spray needs count 1 to 64 and jitter 0 to 4")?;
    }
    // Paint and mask strokes with an alpha go dab by dab, as the dab
    // brushes do (the image needs each dab's frame).
    let spaced = brush.dabs() || (spec.alpha.is_some() && matches!(brush, Brush::Paint | Brush::MaskPaint));
    if let Some(a) = &spec.alpha {
        check(spaced, "alpha works with the dab brushes (standard, clay, clay_buildup, trim_dynamic, polish, vdm), paint and mask_paint")?;
        a.validate()?;
        check(brush == Brush::Standard || a.usage != Some(super::alpha::Use::Height), "alpha use height shapes standard dabs; other brushes take use mask")?;
    }
    if brush == Brush::Vdm {
        let a = spec.alpha.as_ref().ok_or("brush vdm needs alpha: {name} of a VDM stamp (alpha create_vdm makes one)")?;
        check(a.usage.is_none() && a.depth.is_none() && a.mid == 0.0 && a.mode != Mode::Tile, "a VDM stamp takes rotate, scale and mode stamp or drag (not use, depth, mid or tile)")?;
        check(spec.falloff.is_none(), "a VDM stamp takes no falloff (it fades out at its own edge)")?;
    }
    if let StrokeKind::Curve { closed } = spec.kind {
        check(spec.points.len() >= if closed { 3 } else { 2 } || matches!(spec.frame, Some(Frame::Screen(_))), "a curve needs 2 or more control points (3 when closed)")?;
        check(spec.lazy.is_none_or(|l| l == 0.0), "a curve is smooth already; leave lazy out")?;
    }
    check(spec.curl_deg.is_none() || brush == Brush::Snakehook, "curl_deg is for snakehook")?;
    check(spec.curl_deg.is_none_or(|c| c.is_finite() && c.abs() <= 720.0), "curl_deg must be -720 to 720 (degrees)")?;
    check(brush != Brush::Snakehook || matches!(spec.kind, StrokeKind::Path | StrokeKind::Curve { closed: false }), "snakehook takes a path (kind path or an open curve): from the surface out")?;
    let posable = spec.symmetry.as_ref().is_some_and(|s| s.posable);

    // Frame: points (and directions) into object space.
    let (to_local, scale) = match &spec.frame {
        Some(Frame::Object) => (Mat4::IDENTITY, 1.0),
        // A projected frame's points and directions are in the call's space.
        Some(Frame::Screen(_)) => (ctx.world_to_local, mat_scale(ctx.world_to_local)),
        _ => (ctx.to_local, ctx.radius_scale),
    };
    // Posable strokes go back to the rest pose point by point; the
    // stroke-wide directions (delta, normal) and a drag rect's corners go
    // through the pose where the stroke starts.
    let mut posed_at: Option<Mat4> = None;
    let mut samples: Vec<Sample> = match &spec.frame {
        _ if posable => {
            check(matches!(spec.frame, None | Some(Frame::World)), "posable symmetry takes points on the posed clay in the call's space; leave frame out")?;
            let unpose = ctx.unpose.ok_or("posable symmetry works in sculpt calls (it needs the posed clay)")?;
            let samples = spec
                .points
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let m = unpose(p.p).ok_or(format!("point {k} is not on the posed clay"))?;
                    posed_at.get_or_insert(m);
                    Ok(Sample { p: m.transform_point3(p.p), pressure: p.pressure, normal: p.normal.map(|n| m.inverse().transpose().transform_vector3(n).normalize_or_zero()) })
                })
                .collect::<Result<_, String>>()?;
            if let StrokeKind::DragRect { from, .. } = spec.kind {
                posed_at = Some(unpose(from).ok_or("drag_rect from is not on the posed clay")?);
            }
            samples
        }
        Some(Frame::Screen(screen)) => {
            check(spec.points.is_empty(), "a screen frame gives its points as at or px; leave points out")?;
            screen_points(screen, ctx)?
        }
        _ => spec.points.iter().map(|p| Sample { p: to_local.transform_point3(p.p), pressure: p.pressure, normal: p.normal.map(|n| to_local.inverse().transpose().transform_vector3(n).normalize_or_zero()) }).collect(),
    };
    let project = match &spec.frame {
        Some(Frame::Project(p)) => Some(Projector::new(p, to_local, scale, spec.radius * scale, ctx)?),
        _ => None,
    };
    if let Some(p) = &project {
        p.place(&mut samples, ctx)?;
    }
    let snapped = project.is_none() && (matches!(spec.frame, Some(Frame::Surface { .. })) || spec.snap.or(ctx.snap_call).unwrap_or(ctx.snap_single && samples.len() == 1));
    if snapped && !matches!(spec.frame, Some(Frame::Screen(_))) {
        let snap = ctx.snap.ok_or("snap needs a clay surface")?;
        let offset = match spec.frame {
            Some(Frame::Surface { offset }) => offset * scale,
            _ => 0.0,
        };
        // A snakehook's path leaves the surface: only its start snaps.
        for s in samples.iter_mut().take(if brush == Brush::Snakehook { 1 } else { usize::MAX }) {
            if let Some((q, n)) = snap(s.p) {
                s.p = q + n * offset;
                s.normal = s.normal.or(Some(n));
            }
        }
    }
    // Where the stroke-wide directions are mapped from.
    let to_local = posed_at.unwrap_or(to_local);
    let kind = match spec.kind {
        StrokeKind::DragRect { from, to } if posable => {
            let unpose = ctx.unpose.ok_or("posable symmetry works in sculpt calls (it needs the posed clay)")?;
            let to = unpose(to).ok_or("drag_rect to is not on the posed clay")?.transform_point3(to);
            StrokeKind::DragRect { from: to_local.transform_point3(from), to }
        }
        StrokeKind::DragRect { from, to } => StrokeKind::DragRect { from: to_local.transform_point3(from), to: to_local.transform_point3(to) },
        k => k,
    };
    if !matches!(kind, StrokeKind::DragRect { .. }) {
        check(!samples.is_empty(), &format!("a stroke needs 1 to {MAX_POINTS} points"))?;
    }
    let radius = spec.radius * scale;
    // A curve: the smooth curve through its control points (each span in
    // steps of a quarter radius, bounded per span), back on the surface
    // when its points snap; from here on it is a path.
    let kind = match kind {
        StrokeKind::Curve { closed } => {
            check(samples.len() >= if closed { 3 } else { 2 }, "a curve needs 2 or more control points (3 when closed)")?;
            samples = path::curve(&samples, closed, radius * 0.25);
            let resnap = snapped || matches!(spec.frame, Some(Frame::Screen(_)));
            // Between its control points the curve leaves the surface: a
            // projected one is cast again, across the curve.
            if let Some(p) = &project {
                p.place(&mut samples, ctx)?;
            } else if resnap && brush != Brush::Snakehook && let Some(snap) = ctx.snap {
                let offset = match spec.frame {
                    Some(Frame::Surface { offset }) => offset * scale,
                    _ => 0.0,
                };
                for s in samples.iter_mut() {
                    if let Some((q, n)) = snap(s.p) {
                        s.p = q + n * offset;
                        s.normal = s.normal.or(Some(n));
                    }
                }
            }
            StrokeKind::Path
        }
        k => k,
    };
    let default_strength = if matches!(brush, Brush::Paint | Brush::MaskPaint | Brush::GroupPaint | Brush::Vdm) { 1.0 } else { 0.5 };
    let mut strength = spec.strength.unwrap_or(default_strength);
    if brush.signed_depth() {
        strength *= scale;
    }
    let delta = spec.delta.map(|d| to_local.transform_vector3(d));
    let normal = spec.normal.map(|v| to_local.inverse().transpose().transform_vector3(v));
    // Agent paths can be long and steps tiny: bound every resample
    // before it allocates (a NaN or infinite count fails too).
    let fits = |pts: &[Sample], step: f32, per: usize, limit: usize, what: &str| {
        check(path::resample_count(pts, step) * per as f32 <= limit as f32, &format!("the stroke makes more than {limit} {what}; use a larger spacing or radius, a shorter path or fewer symmetric copies"))
    };
    if let Some(l) = spec.lazy.filter(|l| *l > 0.0) {
        fits(&samples, path::lazy_step(l * radius), 1, MAX_SAMPLES, "lazy path points")?;
        samples = path::lazy(&samples, l * radius);
    }

    let curve = match &spec.falloff {
        Some(f) => f.resolve(),
        // Sharp-edged by default: a flat plateau.
        None if matches!(brush, Brush::TrimDynamic | Brush::Polish | Brush::ClayTubes) => Falloff { preset: Preset::Flat, ..Falloff::default() }.resolve(),
        None => Curve::Legacy,
    };
    // Dabs for one copy of the stroke.
    let pressured = samples.iter().any(|s| s.pressure.is_some());
    let factors = |s: &Sample| spec.pressure.factors(s.pressure);
    // VDM stamps go a stamp's width apart by default.
    let spacing = spec.spacing.unwrap_or(if brush == Brush::Vdm { 2.0 } else { DEFAULT_SPACING }) * radius;
    // Dabs that orient a footprint or a dragged alpha keep the stroke's
    // direction (object X where it has none); plain standard dabs stay as
    // they were. Stamp and tile alphas stay upright on object X instead.
    let drag = spec.alpha.as_ref().is_some_and(|a| a.mode == Mode::Drag);
    let oriented = spaced && (brush != Brush::Standard || drag);
    let dir = |pts: &[Sample], i: usize| -> Option<Vec3> {
        oriented.then(|| (pts[(i + 1).min(pts.len() - 1)].p - pts[i.saturating_sub(1)].p).try_normalize().unwrap_or(Vec3::X))
    };
    let single = |s: &Sample, extra: f32, tangent: Option<Vec3>| {
        let (size, gain) = factors(s);
        Dab { points: vec![s.p], radius: radius * size.max(0.0), radii: None, gain: gain * extra, gains: None, delta, normal, tangent, upright: None, flip: false }
    };
    let mut base: Vec<Dab> = Vec::new();
    match kind {
        StrokeKind::Path if brush == Brush::Snakehook => {
            let points = hook_path(&samples, delta, normal, spec.curl_deg.unwrap_or(0.0), radius)?;
            base.push(Dab { points, radius, radii: None, gain: 1.0, gains: None, delta: None, normal: None, tangent: None, upright: None, flip: false });
        }
        StrokeKind::Path if !spaced => {
            // The engine tests every grid point against every segment of
            // a path dab, so it keeps the authored bound of MAX_POINTS:
            // an explicit spacing that would pass it is refused (unless
            // `fit`), while taper (and a long lazy path) coarsen their
            // step to fit.
            let explicit = spec.spacing.is_some() && !(fit && fits(&samples, spacing, 1, MAX_POINTS, "path points").is_err());
            let pts = if explicit {
                fits(&samples, spacing, 1, MAX_POINTS, "path points")?;
                path::resample(&samples, spacing)
            } else if spec.spacing.is_some() || spec.taper.is_some() || samples.len() > MAX_POINTS {
                let step = spacing.max(path::length(&samples) / (MAX_POINTS - 3) as f32);
                fits(&samples, step, 1, MAX_POINTS, "path points")?;
                path::resample(&samples, step)
            } else {
                samples.clone()
            };
            let fr = path::fractions(&pts);
            let mut dab = Dab { points: pts.iter().map(|s| s.p).collect(), radius, radii: None, gain: 1.0, gains: None, delta, normal, tangent: None, upright: None, flip: false };
            if pressured {
                let radii: Vec<f32> = pts.iter().map(|s| radius * factors(s).0.max(0.0)).collect();
                dab.radius = radii.iter().copied().fold(0.0, f32::max);
                dab.radii = Some(radii);
            }
            if pressured || spec.taper.is_some() {
                dab.gains = Some(pts.iter().zip(&fr).map(|(s, u)| factors(s).1 * spec.taper.map_or(1.0, |t| path::taper(*u, t))).collect());
            }
            base.push(dab);
        }
        StrokeKind::Path => {
            fits(&samples, spacing, 1, MAX_DABS, "dabs")?;
            let pts = path::resample(&samples, spacing);
            let fr = path::fractions(&pts);
            // Overlapping standard (and buildup) dabs build up: along a
            // path each is scaled so the ridge rises about as high as one
            // dab, at any spacing. The other plane brushes work toward a
            // plane, so they take every dab at full strength.
            let build = if matches!(brush, Brush::Standard | Brush::ClayBuildup) && pts.len() > 1 { (spacing / (2.0 * radius * area(&curve))).min(1.0) } else { 1.0 };
            base.extend(pts.iter().zip(&fr).enumerate().map(|(i, (s, u))| single(s, build * spec.taper.map_or(1.0, |t| path::taper(*u, t)), dir(&pts, i))));
        }
        StrokeKind::Dots => {
            let n = samples.len().max(2) - 1;
            base.extend(samples.iter().enumerate().map(|(i, s)| single(s, spec.taper.map_or(1.0, |t| path::taper(i as f32 / n as f32, t)), dir(&samples, i))));
        }
        StrokeKind::DragDot => base.push(single(samples.last().expect("checked"), 1.0, dir(&samples, samples.len() - 1))),
        // Made a path above.
        StrokeKind::Curve { .. } => return Err("a curve was not made a path".into()),
        StrokeKind::DragRect { from, to } => {
            let reach = from.distance(to);
            check(reach > 0.0 && reach.is_finite(), "drag_rect needs from and to apart")?;
            base.push(Dab { points: vec![from], radius: reach, radii: None, gain: 1.0, gains: None, delta, normal, tangent: Some((to - from) / reach), upright: None, flip: false });
        }
        StrokeKind::Spray { count, jitter, seed } => {
            fits(&samples, spacing, count as usize, MAX_DABS, "dabs")?;
            let mut rng = Pcg32::new(seed, 0x5eed);
            let centers = path::resample(&samples, spacing);
            // A projected spray casts each jittered dab again, along its
            // center's ray; dabs that land on nothing are left out (the
            // same ones on every replay: the seed fixes them).
            let rays = project.as_ref().map(|p| p.directions(&centers, ctx)).transpose()?;
            for (i, s) in centers.iter().copied().enumerate() {
                let n = s.normal.or(normal).map(|n| n.normalize_or(Vec3::Y)).unwrap_or(Vec3::Y);
                let (u, v) = n.any_orthonormal_pair();
                for _ in 0..count {
                    let (x, y) = rng.in_disk();
                    let mut at = Sample { p: s.p + (u * x + v * y) * jitter * radius, ..s };
                    if let (Some(p), Some(rays)) = (&project, &rays) {
                        let Some((q, n)) = p.cast_one(at.p, rays[i], ctx) else { continue };
                        (at.p, at.normal) = (q, Some(n));
                    } else if snapped && let Some((q, _)) = ctx.snap.and_then(|f| f(at.p)) {
                        at.p = q;
                    }
                    base.push(single(&at, 1.0, dir(&centers, i)));
                }
            }
        }
    }
    base.retain(|d| d.radius > 0.0);
    if spec.alpha.is_some() && !drag {
        for d in &mut base {
            d.upright = Some(Vec3::X);
        }
    }

    // Symmetric copies.
    let symmetry = spec.symmetry.clone().unwrap_or_default();
    let instances = symmetry.instances(ctx.world_to_local.transform_point3(Vec3::ZERO));
    check(base.len() * instances.len() <= MAX_DABS, &format!("the stroke makes more than {MAX_DABS} dabs; use a larger spacing, a shorter path or fewer symmetric copies"))?;
    let mut dabs = Vec::with_capacity(base.len() * instances.len());
    for inst in &instances {
        for d in &base {
            if inst.is_identity() {
                dabs.push(d.clone());
                continue;
            }
            dabs.push(Dab {
                points: d.points.iter().map(|p| inst.point(*p)).collect(),
                delta: d.delta.map(|v| inst.vector(v)),
                normal: d.normal.map(|v| inst.vector(v)),
                tangent: d.tangent.map(|v| inst.vector(v)),
                upright: d.upright.map(|v| inst.vector(v)),
                flip: d.flip ^ inst.flips(),
                ..d.clone()
            });
        }
    }

    let paint = paint_params(spec, spec.falloff.as_ref().map(|f| f.resolve()))?;
    let paint_dabs = match (&spec.paint, brush) {
        (Some(args), b) if b != Brush::Paint => {
            // The sculpt stroke passed its own dab bound, so the color
            // resamples to fit the path-dab bound rather than refuse it.
            let as_paint = StrokeSpec { brush: Brush::Paint, strength: args.strength, alpha: None, curl_deg: None, ..spec.clone() };
            Some(resolve_with(&as_paint, ctx, true)?.dabs)
        }
        _ => None,
    };
    let alpha = match &spec.alpha {
        Some(a) => {
            let lookup = ctx.alpha.ok_or("alphas come from the scene's library: use them in sculpt calls")?;
            let usage = a.usage.unwrap_or(if brush == Brush::Standard { super::alpha::Use::Height } else { super::alpha::Use::Mask });
            Some(AlphaParams { name: a.name.clone(), hash: lookup(a)?, rotate: a.rotate, scale: a.scale, mid: a.mid, depth: a.depth.map(|d| d * scale), mode: a.mode, usage })
        }
        None => None,
    };
    Ok(Resolved {
        params: Params {
            brush,
            curve,
            strength,
            invert: spec.invert,
            scale: spec.scale.map(|s| s * scale),
            pattern: spec.pattern.clone(),
            side: spec.side.unwrap_or_default(),
            smooth_mode: spec.smooth_mode.unwrap_or_default(),
            alpha,
            transform: None,
            level: level_cell(spec, ctx)?,
            angle: (brush == Brush::ClayPolish).then(|| spec.angle.unwrap_or(30.0)),
            keep_volume: (brush == Brush::ClayPolish).then(|| spec.keep_volume.unwrap_or(true)),
        },
        mask: spec.mask.clone(),
        dabs,
        instances: if symmetry.is_none() { 1 } else { instances.len() },
        paint,
        paint_dabs,
        frozen: None,
        group: None,
    })
}

/// A transform stroke: the move in object space (conjugated by the
/// call's space), applied as one dab at its pivot. The points, if any,
/// only place that dab.
fn resolve_transform(spec: &StrokeSpec, ctx: &ResolveCtx) -> Result<Resolved, String> {
    let t = spec.transform.as_ref().ok_or("brush transform needs transform: {translate, rotate, scale, pivot} or {matrix}")?;
    check(spec.symmetry.as_ref().is_none_or(|s| s.is_none()) && spec.alpha.is_none() && spec.paint.is_none() && spec.kind == StrokeKind::Path, "transform takes no symmetry, alpha, paint or stroke kind")?;
    check(!matches!(spec.frame, Some(Frame::Screen(_)) | Some(Frame::Surface { .. })), "transform takes points in the call's space or frame object")?;
    let to_local = match spec.frame {
        Some(Frame::Object) => Mat4::IDENTITY,
        _ => ctx.to_local,
    };
    let m = to_local * t.matrix()? * to_local.inverse();
    check(m.is_finite() && m.determinant().abs() > 1e-9, "transform must be invertible in the clay's space")?;
    let pivot = to_local.transform_point3(t.pivot.unwrap_or(Vec3::ZERO));
    let at = spec.points.first().map_or(pivot, |p| to_local.transform_point3(p.p));
    check(at.is_finite(), "transform pivot must be finite")?;
    let strength = spec.strength.unwrap_or(1.0);
    check((0.0..=1.0).contains(&strength), "transform strength is 0 to 1 (how much of the move applies)")?;
    Ok(Resolved {
        params: Params { brush: Brush::Transform, curve: Curve::Legacy, strength, invert: false, scale: None, pattern: None, side: Side::Both, smooth_mode: SmoothMode::Normal, alpha: None, transform: Some(m), level: level_cell(spec, ctx)?, angle: None, keep_volume: None },
        mask: spec.mask.clone(),
        dabs: vec![Dab { points: vec![at], radius: spec.radius * ctx.radius_scale, radii: None, gain: 1.0, gains: None, delta: None, normal: None, tangent: None, upright: None, flip: false }],
        instances: 1,
        paint: None,
        paint_dabs: None,
        frozen: None,
        group: None,
    })
}

/// A snakehook's path, in steps of an eighth of the radius (so each step
/// of the pull stays a small, fold-free move): the stroke's points (or its
/// one point and `delta`), curled by `curl` degrees about `normal`
/// (default: level and across the first step). The curl turns the path
/// evenly over its first three quarters; the last quarter, the tip, keeps
/// the full turn.
fn hook_path(samples: &[Sample], delta: Option<Vec3>, normal: Option<Vec3>, curl: f32, radius: f32) -> Result<Vec<Vec3>, String> {
    let mut pts = samples.to_vec();
    if let (1, Some(d)) = (pts.len(), delta) {
        pts.push(Sample { p: pts[0].p + d, ..pts[0] });
    }
    check(pts.len() >= 2, "snakehook needs a path of 2 or more points (or one point and delta): from the surface out")?;
    let len = path::length(&pts);
    check(len > 0.0 && len <= MAX_HOOK_RADII * radius, &format!("a snakehook path is at most {MAX_HOOK_RADII} radii long; pull further with another stroke"))?;
    let mut out: Vec<Vec3> = path::resample(&pts, radius / 8.0).into_iter().map(|s| s.p).collect();
    if curl != 0.0 && out.len() > 1 {
        let d0 = (out[1] - out[0]).normalize_or(Vec3::Y);
        let axis = normal.and_then(|n| n.try_normalize()).filter(|n| n.cross(d0).length() > 1e-3).unwrap_or_else(|| d0.cross(Vec3::Y).try_normalize().unwrap_or_else(|| d0.cross(Vec3::X).normalize()));
        let n = out.len() - 1;
        let mut curled = vec![out[0]];
        for i in 1..=n {
            let turn = Quat::from_axis_angle(axis, curl.to_radians() * ((i as f32 - 0.5) / (0.75 * n as f32)).min(1.0));
            curled.push(curled[i - 1] + turn * (out[i] - out[i - 1]));
        }
        out = curled;
    }
    Ok(out)
}

/// The area under a falloff curve over 0..1 (8/15 for the default).
fn area(curve: &Curve) -> f32 {
    (0..64).map(|i| curve.eval((i as f32 + 0.5) / 64.0)).sum::<f32>() / 64.0
}

/// The color a stroke lays, if any: paint strokes always, sculpt strokes
/// with `paint` (MRGB).
fn paint_params(spec: &StrokeSpec, curve: Option<Curve>) -> Result<Option<PaintParams>, String> {
    let args = match (&spec.paint, spec.brush) {
        (Some(p), _) => p.clone(),
        (None, Brush::Paint) => PaintArgs::default(),
        (None, _) => return Ok(None),
    };
    let color = match (args.erase, args.color) {
        (true, _) => Vec3::ZERO,
        (false, Some(c)) => Vec3::from(c).clamp(Vec3::ZERO, Vec3::ONE),
        (false, None) => return Err("a paint stroke needs color [r, g, b] (or erase: true)".into()),
    };
    let opacity = if spec.brush == Brush::Paint { spec.strength } else { args.strength }.unwrap_or(1.0);
    let opacity = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
    let curve = curve.unwrap_or(Curve::Hardness(args.hardness.unwrap_or(0.5)));
    Ok(Some(PaintParams { color, erase: args.erase, opacity, curve }))
}

/// Average scale of a matrix (what radii scale by).
pub fn mat_scale(m: Mat4) -> f32 {
    let s = m.to_scale_rotation_translation().0;
    (s.x.abs() + s.y.abs() + s.z.abs()) / 3.0
}

/// A projected frame (`Frame::Project`) resolved in object space.
struct Projector {
    /// The ray direction, or for rays across the path in a plane, that
    /// plane's normal.
    dir: Vec3,
    across: bool,
    both_ways: bool,
    reach: f32,
    offset: f32,
    /// Rays across the path take its direction over this much of it (a
    /// radius) each way, so at a bend they fan across rather than jump.
    window: f32,
}

/// Most misses a refusal lists.
const LISTED_MISSES: usize = 16;

impl Projector {
    fn new(p: &Project, to_local: Mat4, scale: f32, radius: f32, ctx: &ResolveCtx) -> Result<Projector, String> {
        let normal = |v: Vec3| to_local.inverse().transpose().transform_vector3(v).try_normalize();
        let (dir, across) = match &p.along {
            Along::Dir(d) => (to_local.transform_vector3(*d).try_normalize().filter(|_| d.is_finite()).ok_or("project along must be a nonzero, finite direction")?, false),
            Along::Word(w) if w == "plane" => {
                let n = p.plane.ok_or("project along plane needs plane: {normal: [x, y, z]}")?.normal;
                (normal(n).filter(|_| n.is_finite()).ok_or("project plane normal must be nonzero and finite")?, true)
            }
            Along::Word(w) => {
                let d = match w.strip_prefix("view:") {
                    Some("front") => Vec3::NEG_Z,
                    Some("back") => Vec3::Z,
                    Some("right") => Vec3::NEG_X,
                    Some("left") => Vec3::X,
                    Some("top") => Vec3::NEG_Y,
                    Some("bottom") => Vec3::Y,
                    _ => return Err(format!("project along {w}: use [x, y, z], view:front|back|left|right|top|bottom or plane")),
                };
                (ctx.world_to_local.transform_vector3(d).normalize(), false)
            }
        };
        check(p.plane.is_none() || across, "project plane goes with along: \"plane\"")?;
        let reach = p.reach.map_or(4.0 * radius, |r| r * scale);
        check(reach.is_finite() && reach > 0.0, "project reach must be positive")?;
        let offset = p.offset * scale;
        check(offset.is_finite(), "project offset must be finite")?;
        Ok(Projector { dir, across, both_ways: p.both_ways, reach, offset, window: radius })
    }

    /// How far behind each point its rays start: a quarter radius (at most
    /// half the reach), so a point already on the surface (a curve's
    /// landed control points, the curve between them, a jittered spray dab)
    /// hits the sheet it is on instead of passing through it to the far
    /// side. Distances still count from the point itself.
    fn back(&self) -> f32 {
        (0.25 * self.window).min(0.5 * self.reach)
    }

    /// Each sample's ray direction: `dir`, or for rays across the path in
    /// a plane, across it there (its direction over a window each way),
    /// one way toward the clay's middle unless `both_ways`.
    fn directions(&self, samples: &[Sample], ctx: &ResolveCtx) -> Result<Vec<Vec3>, String> {
        let n = samples.len();
        if !self.across {
            return Ok(vec![self.dir; n]);
        }
        check(n >= 2, "a projected frame along plane needs a path of 2 or more points")?;
        // Arc length to each sample, and the point that far along.
        let along: Vec<f32> = std::iter::once(0.0)
            .chain(samples.windows(2).scan(0.0f32, |a, w| {
                *a += w[0].p.distance(w[1].p);
                Some(*a)
            }))
            .collect();
        let at = |u: f32| -> Vec3 {
            let u = u.clamp(0.0, along[n - 1]);
            let j = along.partition_point(|a| *a <= u).clamp(1, n - 1);
            let (a, b) = (along[j - 1], along[j]);
            let f = if b > a { (u - a) / (b - a) } else { 0.0 };
            samples[j - 1].p.lerp(samples[j].p, f)
        };
        samples
            .iter()
            .enumerate()
            .map(|(k, s)| {
                let t = at(along[k] + self.window) - at(along[k] - self.window);
                let d = self.dir.cross(t).try_normalize().ok_or(format!("projected point {k}: the path there runs along the plane's normal"))?;
                Ok(match ctx.center {
                    Some(c) if !self.both_ways && d.dot(c - s.p) < 0.0 => -d,
                    _ => d,
                })
            })
            .collect()
    }

    /// Where the ray from `p` along `d` (and back, with `both_ways`; the
    /// nearer wins) first meets the clay within reach, lifted `offset`
    /// along the outward normal there.
    fn cast_one(&self, p: Vec3, d: Vec3, ctx: &ResolveCtx) -> Option<(Vec3, Vec3)> {
        let cast = ctx.cast?;
        let back = self.back();
        let hit = |d: Vec3| cast(p - d * back, d).filter(|(q, _)| q.distance(p) <= self.reach);
        let mut best = hit(d);
        if self.both_ways
            && let Some(b) = hit(-d)
            && best.is_none_or(|a| b.0.distance(p) < a.0.distance(p))
        {
            best = Some(b);
        }
        let (q, mut normal) = best?;
        // The hit's normal faces the ray: from inside the clay that is
        // inward. The surface there says which way out.
        if let Some((_, m)) = ctx.snap.and_then(|f| f(q))
            && m.dot(normal) < 0.0
        {
            normal = -normal;
        }
        Some((q + normal * self.offset, normal))
    }

    /// Moves every sample to where its ray first meets the clay (the nearer
    /// way with `both_ways`), lifted `offset` along the outward normal
    /// there; refuses the samples that hit nothing within `reach`.
    fn place(&self, samples: &mut [Sample], ctx: &ResolveCtx) -> Result<(), String> {
        check(ctx.cast.is_some(), "a projected frame needs a clay surface to hit")?;
        let n = samples.len();
        let dirs = self.directions(samples, ctx)?;
        let mut misses: Vec<usize> = Vec::new();
        let mut placed = Vec::with_capacity(n);
        for (k, (s, d)) in samples.iter().zip(&dirs).enumerate() {
            match self.cast_one(s.p, *d, ctx) {
                Some(hit) => placed.push(hit),
                None => misses.push(k),
            }
        }
        if !misses.is_empty() {
            let shown: Vec<String> = misses.iter().take(LISTED_MISSES).map(|k| k.to_string()).collect();
            let more = if misses.len() > LISTED_MISSES { format!(" and {} more", misses.len() - LISTED_MISSES) } else { String::new() };
            return Err(format!("projected points {}{more} (of {n}) hit no clay within reach {:.4}; move them nearer the surface, raise reach or change along", shown.join(", "), self.reach));
        }
        for (s, (q, normal)) in samples.iter_mut().zip(placed) {
            s.p = q;
            s.normal = Some(normal);
        }
        Ok(())
    }
}

/// Points where rays through a screen frame hit the clay.
fn screen_points(screen: &super::spec::Screen, ctx: &ResolveCtx) -> Result<Vec<Sample>, String> {
    let cast = ctx.cast.ok_or("a screen frame needs a clay surface to hit")?;
    let pressure = |v: &[f32]| v.get(2).copied();
    let mut rays: Vec<(Vec3, Vec3, Option<f32>)> = Vec::new();
    match (&screen.view, &screen.camera) {
        (Some(view), None) => {
            check(screen.px.is_empty() && !screen.at.is_empty(), "a named view takes at: [[horizontal, vertical], ..] in world units")?;
            let far = ctx.view_far;
            for a in &screen.at {
                check(a.len() == 2 || a.len() == 3, "each at is [horizontal, vertical] or [horizontal, vertical, pressure]")?;
                let (h, v) = (a[0], a[1]);
                let (o, d) = match view.as_str() {
                    "front" => (Vec3::new(h, v, far), Vec3::NEG_Z),
                    "back" => (Vec3::new(h, v, -far), Vec3::Z),
                    "right" => (Vec3::new(far, v, h), Vec3::NEG_X),
                    "left" => (Vec3::new(-far, v, h), Vec3::X),
                    "top" => (Vec3::new(h, far, v), Vec3::NEG_Y),
                    "bottom" => (Vec3::new(h, -far, v), Vec3::Y),
                    other => return Err(format!("unknown view {other}; use front, back, right, left, top or bottom")),
                };
                rays.push((o, d, pressure(a)));
            }
        }
        (None, Some(cam)) => {
            check(screen.at.is_empty() && !screen.px.is_empty(), "a camera takes px: [[x, y], ..] in pixels")?;
            let [w, h] = cam.viewport;
            check(w > 0.0 && h > 0.0, "camera viewport must be positive")?;
            let inv = (cam.proj * cam.view).inverse();
            check(inv.is_finite(), "camera matrices must be invertible")?;
            for p in &screen.px {
                check(p.len() == 2 || p.len() == 3, "each px is [x, y] or [x, y, pressure]")?;
                let (a, d) = cam.ray(p[0], p[1]);
                rays.push((a, d, pressure(p)));
            }
        }
        _ => return Err("a screen frame takes view (a named view) or camera, not both".into()),
    }
    check(rays.len() <= MAX_POINTS, &format!("a stroke needs 1 to {MAX_POINTS} points"))?;
    let m = ctx.world_to_local;
    rays.into_iter()
        .enumerate()
        .map(|(k, (o, d, pr))| {
            let (q, n) = cast(m.transform_point3(o), m.transform_vector3(d).normalize_or_zero()).ok_or(format!("screen point {k} hits no clay"))?;
            if pr.is_some_and(|p| !(0.0..=1.0).contains(&p)) {
                return Err("pressure must be 0..1".into());
            }
            Ok(Sample { p: q, pressure: pr, normal: Some(n) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::path::PathPoint;
    use crate::brush::symmetry::Symmetry;

    fn spec(brush: Brush, points: &[[f32; 3]], radius: f32) -> StrokeSpec {
        StrokeSpec::new(brush, &points.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>(), radius)
    }

    #[test]
    fn spacing_sets_the_dab_count() {
        let mut s = spec(Brush::Standard, &[[0.0, 0.0, 0.0], [0.37, 0.0, 0.0], [0.37, 0.41, 0.0]], 0.05);
        s.spacing = Some(0.25);
        let r = resolve(&s, &ResolveCtx::local()).unwrap();
        let want = (0.78f32 / (0.25 * 0.05)).ceil() as i64;
        assert!((r.dabs.len() as i64 - want).abs() <= 1, "{} dabs, want {want}", r.dabs.len());
        assert!(r.dabs.iter().all(|d| d.points.len() == 1));
    }

    #[test]
    fn spray_is_seeded() {
        let mut s = spec(Brush::Standard, &[[0.0, 0.0, 0.0], [0.3, 0.0, 0.0]], 0.05);
        s.kind = StrokeKind::Spray { count: 5, jitter: 0.8, seed: 7 };
        let a = resolve(&s, &ResolveCtx::local()).unwrap().dabs;
        let b = resolve(&s, &ResolveCtx::local()).unwrap().dabs;
        assert_eq!(a, b);
        s.kind = StrokeKind::Spray { count: 5, jitter: 0.8, seed: 8 };
        assert_ne!(resolve(&s, &ResolveCtx::local()).unwrap().dabs, a);
        // Scattered within jitter x radius of the path.
        assert!(a.iter().all(|d| d.points[0].y.abs().max(d.points[0].z.abs()) <= 0.8 * 0.05 + 1e-6));
    }

    #[test]
    fn legacy_paths_stay_one_dab_and_mirror_comes_second() {
        let mut s = spec(Brush::Add, &[[0.1, 0.0, 0.0], [0.2, 0.1, 0.0]], 0.05);
        s.symmetry = Some(Symmetry { axes: vec![super::super::symmetry::Axis::X], ..Symmetry::default() });
        let r = resolve(&s, &ResolveCtx::local()).unwrap();
        assert_eq!(r.dabs.len(), 2);
        assert_eq!(r.dabs[0], Dab::path(vec![Vec3::new(0.1, 0.0, 0.0), Vec3::new(0.2, 0.1, 0.0)], 0.05));
        assert_eq!(r.dabs[1].points[0], Vec3::new(-0.1, 0.0, 0.0));
        assert_eq!(r.params.curve, Curve::Legacy);
    }

    #[test]
    fn frames_bring_points_into_object_space() {
        // A unit sphere at the clay's origin; the clay sits at x = 2 in
        // the world, scaled by 2.
        let world_to_local = Mat4::from_scale_rotation_translation(Vec3::splat(2.0), glam::Quat::IDENTITY, Vec3::new(2.0, 0.0, 0.0)).inverse();
        let snap = |p: Vec3| Some((p.normalize(), p.normalize()));
        let cast = |o: Vec3, d: Vec3| {
            // Ray against the unit sphere.
            let b = o.dot(d);
            let disc = b * b - (o.length_squared() - 1.0);
            (disc >= 0.0).then(|| {
                let p = o + d * (-b - disc.sqrt());
                (p, p)
            })
        };
        let ctx = ResolveCtx { to_local: world_to_local, radius_scale: 0.5, world_to_local, snap: Some(&snap), cast: Some(&cast), view_far: 50.0, snap_call: None, snap_single: false, alpha: None, unpose: None, levels: None, center: None };
        let mut s = spec(Brush::Inflate, &[[2.0, 3.0, 0.0]], 0.2);
        let r = resolve(&s, &ctx).unwrap();
        assert!((r.dabs[0].points[0] - Vec3::new(0.0, 1.5, 0.0)).length() < 1e-6 && (r.dabs[0].radius - 0.1).abs() < 1e-6, "world");
        s.frame = Some(Frame::Object);
        let r = resolve(&s, &ctx).unwrap();
        assert_eq!((r.dabs[0].points[0], r.dabs[0].radius), (Vec3::new(2.0, 3.0, 0.0), 0.2), "object");
        s.frame = Some(Frame::Surface { offset: 0.2 });
        let r = resolve(&s, &ctx).unwrap();
        assert!((r.dabs[0].points[0] - Vec3::new(0.0, 1.1, 0.0)).length() < 1e-6, "surface {}", r.dabs[0].points[0]);
        // A named view: from the front at world (2.2, 0.4) hits the sphere.
        s.points.clear();
        s.frame = Some(Frame::Screen(Box::new(super::super::spec::Screen { view: Some("front".into()), camera: None, at: vec![vec![2.2, 0.4, 0.5]], px: vec![] })));
        let r = resolve(&s, &ctx).unwrap();
        let p = r.dabs[0].points[0];
        assert!((p.length() - 1.0).abs() < 1e-5 && (p.x - 0.1).abs() < 1e-5 && (p.y - 0.2).abs() < 1e-5 && p.z > 0.0, "front {p}");
        assert_eq!(r.dabs[0].radii.as_ref().map(|r| r[0]), Some(0.05), "pressure 0.5 halves the radius");
        // A stored camera: the viewport's center looks at the clay.
        let camera = super::super::spec::Camera { view: Mat4::look_at_rh(Vec3::new(2.0, 0.0, 10.0), Vec3::new(2.0, 0.0, 0.0), Vec3::Y), proj: Mat4::perspective_rh(0.5, 1.0, 0.1, 100.0), viewport: [400.0, 400.0] };
        s.frame = Some(Frame::Screen(Box::new(super::super::spec::Screen { view: None, camera: Some(camera), at: vec![], px: vec![vec![200.0, 200.0]] })));
        let p = resolve(&s, &ctx).unwrap().dabs[0].points[0];
        assert!((p - Vec3::Z).length() < 1e-4, "camera {p}");
        s.frame = Some(Frame::Screen(Box::new(super::super::spec::Screen { view: None, camera: Some(camera), at: vec![], px: vec![vec![0.0, 0.0]] })));
        assert!(resolve(&s, &ctx).unwrap_err().contains("hits no clay"));
    }

    #[test]
    fn pressure_scales_dabs_and_taper_fades_paths() {
        let mut s = spec(Brush::Standard, &[], 0.1);
        s.points = vec![PathPoint { pressure: Some(0.0), ..PathPoint::at(Vec3::ZERO) }, PathPoint { pressure: Some(1.0), ..PathPoint::at(Vec3::X) }];
        let r = resolve(&s, &ResolveCtx::local()).unwrap();
        assert!(r.dabs.windows(2).all(|w| w[1].radius >= w[0].radius && w[1].gain >= w[0].gain));
        // Full pressure at the end, scaled for the overlap of the dabs.
        let build = DEFAULT_SPACING / (2.0 * area(&Curve::Legacy));
        assert!((r.dabs.last().unwrap().gain - build).abs() < 1e-6 && (area(&Curve::Legacy) - 8.0 / 15.0).abs() < 1e-3);
        let mut c = spec(Brush::Crease, &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.1);
        c.taper = Some([0.3, 0.3]);
        let r = resolve(&c, &ResolveCtx::local()).unwrap();
        let g = r.dabs[0].gains.as_ref().unwrap();
        assert!(g[0] == 0.0 && *g.last().unwrap() == 0.0 && g[g.len() / 2] == 1.0);
    }

    #[test]
    fn huge_resamples_are_refused_before_they_allocate() {
        let far = |brush, to: f32, radius| spec(brush, &[[0.0, 0.0, 0.0], [to, 0.0, 0.0]], radius);
        for (s, why) in [
            (far(Brush::Standard, 1e6, 0.001), "dabs"),
            (far(Brush::Standard, 1e30, 0.001), "dabs"),
            (far(Brush::Standard, f32::MAX, 0.001), "dabs"),
            (StrokeSpec { spacing: Some(0.01), ..far(Brush::Add, 1e6, 0.001) }, "path points"),
            (StrokeSpec { spacing: Some(0.01), ..far(Brush::Paint, 30.0, 0.05) }, "path points"),
            (StrokeSpec { lazy: Some(0.01), ..far(Brush::Standard, 1e3, 0.001) }, "lazy path points"),
            (StrokeSpec { kind: StrokeKind::Spray { count: 64, jitter: 1.0, seed: 1 }, ..far(Brush::Standard, 1.0, 0.01) }, "dabs"),
        ] {
            let err = resolve(&s, &ResolveCtx::local()).unwrap_err();
            assert!(err.contains(why), "{err}");
        }
        let tiny = StrokeSpec { lazy: Some(1e-7), ..far(Brush::Standard, 1.0, 0.1) };
        assert!(resolve(&tiny, &ResolveCtx::local()).unwrap_err().contains("lazy must be"));
        // Ordinary strokes still resolve.
        assert!(resolve(&StrokeSpec { lazy: Some(0.3), ..far(Brush::Add, 1.0, 0.05) }, &ResolveCtx::local()).is_ok());
    }

    #[test]
    fn posable_strokes_unpose_corners_and_directions() {
        // The posed clay is the rest clay turned 90 degrees about Z and
        // lifted: everything the stroke gives comes back through that.
        let rest = Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0)) * Mat4::from_rotation_z(-std::f32::consts::FRAC_PI_2);
        let unpose = |_: Vec3| Some(rest);
        let ctx = ResolveCtx { unpose: Some(&unpose), ..ResolveCtx::local() };
        let near = |a: Vec3, b: Vec3| (a - b).length() < 1e-5;
        let posable = Some(Symmetry { posable: true, ..Symmetry::default() });
        let mut g = spec(Brush::Grab, &[[0.0, 2.0, 0.0]], 0.1);
        g.symmetry = posable.clone();
        g.delta = Some(Vec3::Y);
        g.normal = Some(Vec3::Y);
        let d = &resolve(&g, &ctx).unwrap().dabs[0];
        assert!(near(d.points[0], rest.transform_point3(Vec3::new(0.0, 2.0, 0.0))), "point {}", d.points[0]);
        assert!(near(d.delta.unwrap(), Vec3::X) && near(d.normal.unwrap().normalize(), Vec3::X), "{:?} {:?}", d.delta, d.normal);
        // A drag rect has no points: its corners go back to rest too.
        let mut r = spec(Brush::Standard, &[], 0.1);
        r.symmetry = posable;
        r.kind = StrokeKind::DragRect { from: Vec3::new(0.0, 2.0, 0.0), to: Vec3::new(0.0, 2.5, 0.0) };
        let d = &resolve(&r, &ctx).unwrap().dabs[0];
        assert!(near(d.points[0], rest.transform_point3(Vec3::new(0.0, 2.0, 0.0))) && near(d.tangent.unwrap(), Vec3::X), "{:?}", d);
        // A corner off the posed clay is refused.
        let off = |p: Vec3| (p.y < 2.2).then_some(rest);
        let ctx = ResolveCtx { unpose: Some(&off), ..ResolveCtx::local() };
        assert!(resolve(&r, &ctx).unwrap_err().contains("not on the posed clay"));
    }

    #[test]
    fn path_dabs_keep_the_authored_point_bound() {
        // Taper and LazyMouse on a long path coarsen to fit: the engine
        // tests every grid point against every segment of a path dab.
        let mut long = spec(Brush::Paint, &(0..512).map(|i| [i as f32 * 0.06, (i % 2) as f32 * 0.03, 0.0]).collect::<Vec<_>>(), 0.05);
        long.paint = Some(PaintArgs { color: Some([1.0, 0.0, 0.0]), ..PaintArgs::default() });
        for s in [StrokeSpec { taper: Some([0.2, 0.2]), ..long.clone() }, StrokeSpec { lazy: Some(0.3), ..long.clone() }, StrokeSpec { lazy: Some(0.3), taper: Some([0.1, 0.0]), ..long }] {
            let r = resolve(&s, &ResolveCtx::local()).unwrap();
            assert_eq!(r.dabs.len(), 1);
            assert!(r.dabs[0].points.len() <= MAX_POINTS, "{} points", r.dabs[0].points.len());
        }
        // An MRGB stroke spaced finer than a path dab may hold: the sculpt
        // dabs follow the spacing, the color resamples to fit.
        let mut mrgb = spec(Brush::Standard, &[[0.0, 0.0, 0.0], [3.0, 0.0, 0.0]], 0.05);
        mrgb.spacing = Some(0.05);
        mrgb.paint = Some(PaintArgs { color: Some([1.0, 0.0, 0.0]), ..PaintArgs::default() });
        let r = resolve(&mrgb, &ResolveCtx::local()).unwrap();
        assert!(r.dabs.len() > MAX_POINTS && r.dabs.len() <= MAX_DABS, "{} dabs", r.dabs.len());
        let paint = r.paint_dabs.unwrap();
        assert_eq!(paint.len(), 1);
        assert!(paint[0].points.len() > 2 && paint[0].points.len() <= MAX_POINTS, "{} points", paint[0].points.len());
        // A spacing that fits is kept as authored.
        mrgb.spacing = Some(0.5);
        let n = resolve(&mrgb, &ResolveCtx::local()).unwrap().paint_dabs.unwrap()[0].points.len() as i64;
        assert!((n - (3.0 / (0.5 * 0.05)) as i64).abs() <= 1, "{n} points");
        // Short tapered paths still sample at the default spacing.
        let mut c = spec(Brush::Crease, &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], 0.1);
        c.taper = Some([0.3, 0.3]);
        let n = resolve(&c, &ResolveCtx::local()).unwrap().dabs[0].points.len() as i64;
        assert!((n - (1.0 / (DEFAULT_SPACING * 0.1)).ceil() as i64).abs() <= 1, "{n} points");
    }
}
