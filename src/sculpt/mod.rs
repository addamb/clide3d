//! Sculpt brushes for clay.
//!
//! A stroke edits the clay's detail layer (see `clay::Detail`): a fine grid of
//! offsets added to the blob field. Each dab of a stroke:
//!
//! 1. reads the current field T (blobs + offsets) on the grid points around
//!    the dab,
//! 2. computes the new field T' with the brush,
//! 3. stores T' minus the blob field back as offsets.
//!
//! Strokes are authored as `brush::spec::StrokeSpec` (today's JSON still
//! reads; `Stroke` is the older form), resolved into object-space dabs by
//! `brush::resolve` and applied by `engine::apply`; `run` does both halves
//! for the sculpt and paint tools.
//!
//! Brushes, with `w` = the falloff curve from 1 on the stroke to 0 at
//! `radius` (see `brush::curve`):
//! - add / carve: melt on / cut away a tube of `radius` along the points.
//! - inflate / deflate: move the surface out / in by up to radius * strength / 2.
//! - smooth: blur the field (removes bumps and creases); `smooth_mode`
//!   gentle (half as strong), peaks (only lowers) or valleys (only raises).
//! - flatten: pull the surface onto a plane through the stroke; `side`
//!   above only cuts, below only fills.
//! - grab: pull the surface by `delta` (the nose forward, a chin down).
//!   Strength does not apply; `delta` is the exact pull, at most `radius`.
//! - crease: sharp V groove, with signed scene-unit depth; invert gives a
//!   ridge; `taper` fades its ends (Dam Standard).
//! - pinch: compress tangential features toward the path.
//! - layer: fixed signed height with a flat influence plateau per stroke.
//! - stretch/extrude: continuous tapered transport around a moving path.
//! - noise: fine texture over an area: `pattern` "pores" (small pits, like
//!   skin), "bumps" (soft lumps) or "wrinkles" (fine lines across `normal`),
//!   with features `scale` apart, as deep as strength x scale / 2.
//! - standard: dab by dab, the surface moves out along its area normal
//!   (the mean surface normal over the middle of the dab) by strength *
//!   radius / 2 x
//!   the falloff of the distance across that normal; invert carves. On a
//!   flat or gently curved surface the dab's profile is exactly that
//!   height times the falloff curve, and nothing changes beyond the radius.
//!   An alpha (`use: height`) shapes the dab: depth x (alpha - mid).
//! - clay, clay_buildup, trim_dynamic, polish: dab by dab toward the plane
//!   of the area under the dab (its mean surface along the area normal):
//!   clay fills hollows up to it with a square footprint, buildup fills to
//!   a tenth of the radius above it (so passes build up), trim_dynamic only
//!   cuts what stands above it, polish presses both ways near the surface.
//!   The last two have a flat, sharp-edged falloff unless given one.
//! - clay_tubes: a strip of even height strength x radius / 2 over the
//!   surface under the path (each point's own normal), flat across.
//! - snakehook: the material at the path's start is pulled along the path
//!   (each step a small fold-free move, as stretch), so a horn follows it;
//!   `curl_deg` curls the path from base to tip.
//!
//! Dab brushes take an alpha (`brush::alpha`) as a mask on their weight.
//!
//! All brushes support named-blob ownership and object-local height masks.
//!
//! The blobs stay editable. Offsets stay where they are in space, so moving
//! a blob does not move detail sculpted on it.

mod brushes;
pub mod engine;
mod noise;
mod transform;
pub mod preview;

#[cfg(test)]
use glam::Vec3;
#[cfg(test)]
use serde::{Deserialize, Serialize};

pub use crate::brush::spec::Brush;
#[cfg(test)]
pub use crate::brush::spec::StrokeMask as Mask;
use crate::brush::resolve::{self, ResolveCtx, Resolved};
use crate::brush::spec::StrokeSpec;
use crate::clay::Clay;
pub use engine::displace;
pub(crate) use noise::noise;

/// A stroke in its older form (what `sculpt` calls sent before StrokeSpec),
/// kept for the checks that build strokes directly; tools read StrokeSpec,
/// which reads this JSON too. Applied through StrokeSpec.
#[cfg(test)]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Stroke {
    pub brush: Brush,
    /// One point (a dab) or a path, in the clay object's space.
    pub points: Vec<Vec3>,
    pub radius: f32,
    /// 0..1 for traditional brushes; scene-unit depth/height for crease/layer.
    #[serde(default = "default_strength")]
    pub strength: f32,
    /// grab: how far to pull (at most `radius`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Vec3>,
    /// flatten: plane normal (default: the surface normal at the stroke).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<Vec3>,
    /// Also apply the stroke mirrored across X = 0 (object space).
    #[serde(default)]
    pub mirror: bool,
    /// noise: feature spacing (default radius / 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// noise: pores, bumps, wrinkles, scales, plates or folds (default pores).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Reverse crease depth or layer height.
    #[serde(default)]
    pub invert: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
}

#[cfg(test)]
impl Stroke {
    /// The same stroke reflected across X = 0.
    pub fn mirrored(&self) -> Stroke {
        let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
        Stroke {
            points: self.points.iter().map(|p| flip(*p)).collect(),
            delta: self.delta.map(flip),
            normal: self.normal.map(flip),
            mirror: false,
            ..self.clone()
        }
    }

    /// The same stroke as a StrokeSpec (`mirror` becomes X symmetry).
    pub fn to_spec(&self) -> StrokeSpec {
        let mut s = StrokeSpec::new(self.brush, &self.points, self.radius);
        s.strength = Some(self.strength);
        (s.delta, s.normal, s.scale, s.pattern, s.invert, s.mask) = (self.delta, self.normal, self.scale, self.pattern.clone(), self.invert, self.mask.clone());
        if self.mirror {
            s.symmetry = Some(crate::brush::symmetry::Symmetry { axes: vec![crate::brush::symmetry::Axis::X], ..Default::default() });
        }
        s
    }
}

#[cfg(test)]
fn default_strength() -> f32 {
    0.5
}

/// What a stroke did.
#[cfg(test)]
pub struct Report {
    pub changed_points: usize,
}

/// Applies one stroke to the clay's detail layer, as given: `mirror` is
/// left to the caller (see `Stroke::mirrored`).
#[cfg(test)]
pub fn apply(clay: &mut Clay, stroke: &Stroke) -> Result<Report, String> {
    if stroke.points.is_empty() || stroke.points.len() > resolve::MAX_POINTS {
        return Err(format!("a stroke needs 1 to {} points", resolve::MAX_POINTS));
    }
    let mut spec = stroke.to_spec();
    spec.symmetry = None;
    let r = run(clay, &resolve::resolve(&spec, &ResolveCtx::local())?)?;
    Ok(Report { changed_points: r.changed_points })
}

/// What a resolved stroke did.
pub struct StrokeReport {
    pub changed_points: usize,
    pub radius: f32,
    #[cfg_attr(not(test), expect(dead_code, reason = "the checks read it; tools report dabs from ledger records"))]
    pub dabs: usize,
    #[cfg_attr(not(test), expect(dead_code, reason = "see dabs"))]
    pub instances: usize,
    /// Paint grid points colored (paint strokes and MRGB sculpt strokes).
    pub painted: Option<usize>,
}

/// Applies a resolved stroke to the clay: its dabs to the detail layer
/// (unless it is a paint stroke), then the color it lays, if any.
pub fn run(clay: &mut Clay, stroke: &Resolved) -> Result<StrokeReport, String> {
    let mut report = StrokeReport { changed_points: 0, radius: stroke.dabs.first().map_or(0.0, |d| d.radius), dabs: stroke.dabs.len(), instances: stroke.instances, painted: None };
    let gate = engine::Gate::of(clay, stroke)?;
    if stroke.params.brush.marks() {
        return Err("mask and group strokes go to the mask and group tools".into());
    }
    if stroke.params.brush != Brush::Paint {
        let alpha = engine::alpha_of(clay, &stroke.params)?;
        let ctx = engine::Ctx { clay, params: &stroke.params, alpha: alpha.as_ref(), gate: &gate, others: None };
        let diff = engine::apply(&ctx, clay.detail.as_ref(), &stroke.dabs, stroke.mask.as_ref())?;
        if diff.dabs > 0 {
            (report.changed_points, report.radius) = (diff.changed_points, diff.radius);
            clay.detail = Some(diff.detail);
        }
    }
    // A paint stroke's own alpha shapes its color (MRGB color follows
    // its own dabs, without the sculpt alpha).
    let color_alpha = if stroke.params.brush == Brush::Paint { engine::alpha_of(clay, &stroke.params)? } else { None };
    if let Some(paint) = &stroke.paint {
        report.painted = Some(crate::paint::paint_dabs(clay, paint, stroke.mask.as_ref(), stroke.paint_dabs.as_deref().unwrap_or(&stroke.dabs), &gate, color_alpha.as_ref().map(|a| (a, stroke.params.alpha.as_ref().expect("has an alpha"))))?);
    }
    Ok(report)
}

/// The `sculpt` (and `paint`) tool's strokes, read and resolved in order:
/// what the tool handler does before its job runs (see `app/tools.rs`).
/// Each JSON stroke is a StrokeSpec; `default_brush` fills in a missing
/// brush (paint strokes).
pub fn prepare(raws: &[serde_json::Value], default_brush: Option<Brush>, ctx: &ResolveCtx) -> Result<Vec<(StrokeSpec, Resolved)>, String> {
    raws.iter()
        .enumerate()
        .map(|(n, raw)| {
            let mut raw = raw.clone();
            if let (Some(b), Some(o)) = (default_brush, raw.as_object_mut()) {
                o.entry("brush").or_insert_with(|| serde_json::json!(b));
            }
            let spec: StrokeSpec = serde_json::from_value(raw).map_err(|e| format!("stroke {n}: {e}"))?;
            let resolved = resolve::resolve(&spec, ctx).map_err(|e| format!("stroke {n}: {e}"))?;
            Ok((spec, resolved))
        })
        .collect()
}

#[cfg(test)]
mod acceptance;
#[cfg(test)]
mod stage2;
#[cfg(test)]
mod stage3;
#[cfg(test)]
mod stage8;
#[cfg(test)]
mod stage8c;

#[cfg(test)]
mod tests {
    use super::*;
    use super::brushes::Job;
    use super::engine::TILE;
    use super::noise::{plate_height, scale_height};
    use crate::clay::{Field, LevelView};
    use glam::IVec3;

    #[test]
    fn a_finer_detail_cell_keeps_the_sculpt_and_shows_on_the_fine_mesh() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 1.0)).unwrap();
        let before = max_y(&c);
        let at = Vec3::new(0.05, 1.0, 0.02);
        let old = c.detail.as_ref().unwrap().grid().offset(at);
        c.set_detail_cell(0.01).unwrap();
        let new = c.detail.as_ref().unwrap().grid().offset(at);
        assert!((old - new).abs() < 0.01 * old.abs().max(0.01), "{old} {new}");
        assert!((max_y(&c) - before).abs() < 0.02);
        // The fine mesh has far more vertices than the regular one.
        let fine = c.build_fine(&[], "").unwrap();
        // Too fine for the clay's size: a clear error, not an empty mesh.
        let mut huge = c.clone();
        huge.detail_cell = Some(0.0005);
        let Err(e) = huge.build_fine(&[], "") else { panic!("built a fine mesh too big") };
        assert!(e.contains("detail_cell >="), "{e}");
        assert!(fine.vertices.len() > 4 * c.build_mesh().vertices.len());
        assert!(c.set_detail_cell(0.0).is_err());
    }

    #[test]
    fn noise_pores_dent_only_the_surface_area_under_the_brush() {
        let mut c = ball();
        c.set_detail_cell(0.01).unwrap();
        let mut s = stroke(Brush::Noise, &[[0.0, 0.0, 1.0]], 0.4, 1.0);
        s.scale = Some(0.06);
        let r = apply(&mut c, &s).unwrap();
        assert!(r.changed_points > 100);
        let d = c.detail.as_ref().unwrap().grid();
        // Pores only push in (positive offsets), near the stroke.
        assert!(d.offset(Vec3::new(0.0, 0.0, 1.0)) >= 0.0);
        assert_eq!(d.offset(Vec3::new(0.0, 0.0, -1.0)), 0.0);
        let dented = (0..200).filter(|i| {
            let a = *i as f32 * 0.002 - 0.2;
            d.offset(Vec3::new(a, a * 0.5, (1.0 - a * a * 1.25).sqrt())) > 1e-4
        });
        assert!(dented.count() > 5);
    }

    fn slab() -> Clay {
        serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"box", "name":"slab", "center":[0,-0.3,0], "size":[1.2,0.6,1.2], "blend":0}
        ], "resolution":96, "detail_cell":0.01})).unwrap()
    }

    fn sample(c: &Clay, p: Vec3) -> f32 {
        let f = Field::new(c, c.detail_cell.unwrap_or(0.01));
        f.sample(p, &f.near(p - 0.5, p + 0.5)).0
    }

    fn top(c: &Clay, x: f32, z: f32) -> f32 {
        let (mut lo, mut hi) = (-0.25, 0.3);
        for _ in 0..25 {
            let y = (lo + hi) * 0.5;
            if sample(c, Vec3::new(x, y, z)) < 0.0 { lo = y; } else { hi = y; }
        }
        (lo + hi) * 0.5
    }

    #[test]
    fn crease_is_a_v_at_equal_depth_and_can_invert() {
        let (mut v, mut round) = (slab(), slab());
        let points = [[0.0,0.0,-0.3],[0.0,0.0,0.3]];
        apply(&mut v, &stroke(Brush::Crease, &points, 0.12, 0.1)).unwrap();
        // A circular carve with its center above the surface cuts 0.1 deep.
        apply(&mut round, &stroke(Brush::Carve, &[[0.0,0.02,-0.3],[0.0,0.02,0.3]], 0.12, 1.0)).unwrap();
        assert!((top(&v,0.0,0.0) + 0.1).abs() < 0.012, "crease depth {}",top(&v,0.0,0.0));
        assert!((top(&round,0.0,0.0) + 0.1).abs() < 0.02);
        let sharp = top(&v,0.02,0.0) - top(&v,0.0,0.0);
        let soft = top(&round,0.02,0.0) - top(&round,0.0,0.0);
        assert!(sharp > soft * 2.0, "V slope {sharp}, round slope {soft}");
        let mut ridge = slab();
        let mut s = stroke(Brush::Crease, &points, 0.12, 0.08);
        s.invert = true;
        apply(&mut ridge, &s).unwrap();
        assert!(top(&ridge,0.0,0.0) > 0.065);
        let mut negative = slab(); s.invert = false; s.strength = -0.08;
        apply(&mut negative, &s).unwrap();
        assert!((top(&ridge,0.0,0.0) - top(&negative,0.0,0.0)).abs() < 1e-6);
    }

    #[test]
    fn layer_has_one_height_despite_overlapping_dabs() {
        let (mut one, mut many) = (slab(), slab());
        apply(&mut one, &stroke(Brush::Layer, &[[0.0,0.0,0.0]], 0.2, 0.08)).unwrap();
        apply(&mut many, &stroke(Brush::Layer, &[[0.0,0.0,0.0];20], 0.2, 0.08)).unwrap();
        assert!((top(&one,0.0,0.0) - 0.08).abs() < 0.01);
        assert!((top(&many,0.0,0.0) - top(&one,0.0,0.0)).abs() < 1e-6);
        assert!((top(&many,0.08,0.0) - top(&many,0.0,0.0)).abs() < 0.002);
        let mut lower = slab();
        apply(&mut lower, &stroke(Brush::Layer, &[[0.0,0.0,0.0]], 0.2, -0.08)).unwrap();
        assert!(top(&lower,0.0,0.0) < -0.065);
    }

    #[test]
    fn pinch_narrows_a_soft_ridge() {
        let mut c = slab();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0,0.0,-0.3],[0.0,0.0,0.3]], 0.2, 1.0)).unwrap();
        let before = top(&c,0.09,0.0);
        apply(&mut c, &stroke(Brush::Pinch, &[[0.0,0.1,-0.3],[0.0,0.1,0.3]], 0.2, 0.7)).unwrap();
        assert!(top(&c,0.09,0.0) < before * 0.8, "before {before}, after {}",top(&c,0.09,0.0));
        assert!(top(&c,0.0,0.0) > 0.07, "ridge center should remain");
    }

    #[test]
    fn new_brushes_match_between_whole_regions_and_masked_tiles() {
        let mut c = slab();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0,0.0,0.0]],0.18,0.5)).unwrap();
        let voxel = 0.01;
        let field = Field::new(&c,voxel);
        let view = LevelView::new(c.detail.as_ref().unwrap().grid(), None);
        let grid = &view;
        let (lo, hi) = (IVec3::splat(-22),IVec3::splat(22));
        for brush in [Brush::Crease,Brush::Pinch,Brush::Layer,Brush::Stretch,Brush::Standard,Brush::Clay,Brush::ClayBuildup,Brush::ClayTubes,Brush::TrimDynamic,Brush::Polish,Brush::Snakehook] {
            let mut s = stroke(brush,&[[0.0,0.04,-0.1],[0.0,0.04,0.1]],0.12,0.035);
            if brush == Brush::Snakehook { s.points = vec![Vec3::new(0.0,0.0,0.0), Vec3::new(0.02,0.1,0.0), Vec3::new(0.08,0.15,0.0)]; }
            let normals = vec![Vec3::Y; s.points.len()];
            s.mask = Some(Mask {blobs:Some(vec!["slab".into()]),above:Some(-0.1),below:Some(0.18),..Default::default()});
            let params = crate::brush::resolve::Params::new(brush, s.strength);
            let sweep = engine::Sweep { points: &s.points, radius: s.radius, radii: None, gains: None };
            let gate = engine::Gate::default();
            let job = Job {params:&params,sweep:&sweep,mask:s.mask.as_ref(),gate:&gate,dab_normal:None,field:&field,grid,voxel,radius:s.radius,strength:s.strength,
                delta:Vec3::new(0.02,0.08,0.0),smooth_passes:1,normal:Vec3::Y,height:0.03,frame:(Vec3::X,Vec3::NEG_Z),stamp:None,vdm:None,plane:-0.01,normals:Some(&normals)};
            let whole: std::collections::HashMap<_,_> = job.region(lo,hi,lo,hi).into_iter().collect();
            let mut tiled = std::collections::HashMap::new();
            for z in (lo.z..=hi.z).step_by(TILE as usize) { for y in (lo.y..=hi.y).step_by(TILE as usize) { for x in (lo.x..=hi.x).step_by(TILE as usize) {
                let a = IVec3::new(x,y,z); let b = (a+TILE-1).min(hi);
                tiled.extend(job.region(a-1,b+1,a,b));
            } } }
            assert_eq!(whole.len(),tiled.len(),"{brush:?}");
            assert!(whole.iter().all(|(key,value)| tiled.get(key).is_some_and(|v| (v-value).abs()<1e-6)),"{brush:?} tile seams");
        }
    }

    fn components(mesh: &crate::mesh::Mesh) -> usize {
        let geometry=mesh.welded_geometry().map(|(mesh,_)|mesh);
        let mesh=geometry.as_ref().unwrap_or(mesh);
        let mut links = vec![Vec::new();mesh.vertices.len()];
        for t in mesh.indices.chunks_exact(3) {
            for i in 0..3 { links[t[i] as usize].push(t[(i+1)%3] as usize); links[t[(i+1)%3] as usize].push(t[i] as usize); }
        }
        let mut seen = vec![false; links.len()];
        let mut count = 0;
        for i in 0..links.len() {
            if seen[i] || links[i].is_empty() { continue; }
            count += 1;
            let mut todo = vec![i]; seen[i] = true;
            while let Some(j) = todo.pop() { for &k in &links[j] { if !seen[k] { seen[k] = true; todo.push(k); } } }
        }
        count
    }

    #[test]
    fn stretch_extends_a_thin_tip_without_mesh_droplets() {
        let mut c: Clay = serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"capsule", "a":[0,-0.3,0], "b":[0,0,0], "radius":0.07, "radius_b":0.025, "blend":0}
        ], "resolution":96, "detail_cell":0.005})).unwrap();
        assert_eq!(components(&c.build_mesh()),1);
        let mut s = stroke(Brush::Stretch,&[[0.0,0.01,0.0]],0.09,1.0);
        s.delta = Some(Vec3::new(0.04,0.18,0.0));
        apply(&mut c,&s).unwrap();
        let mesh = c.build_mesh();
        assert_eq!(components(&mesh),1);
        assert!(mesh.bounds().1.y > 0.16, "tip {}",mesh.bounds().1.y);
    }

    #[test]
    fn every_brush_respects_blob_and_height_masks() {
        let base: Clay = serde_json::from_value(serde_json::json!({"blobs":[
            {"type":"sphere", "name":"left", "center":[-0.3,0,0], "radius":0.2, "blend":0},
            {"type":"sphere", "name":"right", "center":[0.3,0,0], "radius":0.2, "blend":0}
        ], "resolution":64, "detail_cell":0.02})).unwrap();
        for brush in [Brush::Add,Brush::Carve,Brush::Inflate,Brush::Deflate,Brush::Smooth,Brush::Flatten,Brush::Grab,Brush::Noise,Brush::Crease,Brush::Pinch,Brush::Layer,Brush::Stretch] {
            let mut c = base.clone();
            let mut s = stroke(brush,&[[-0.3,0.0,0.2],[0.3,0.0,0.2]],0.45,0.03);
            s.delta = Some(Vec3::Y * 0.04);
            s.mask = Some(Mask { blobs:Some(vec!["left".into()]), above:Some(0.0), below:None, ..Default::default() });
            apply(&mut c,&s).unwrap();
            assert_eq!(sample(&c,Vec3::new(0.3,0.04,0.2)),sample(&base,Vec3::new(0.3,0.04,0.2)),"{brush:?} on right");
            assert_eq!(sample(&c,Vec3::new(-0.3,-0.06,0.2)),sample(&base,Vec3::new(-0.3,-0.06,0.2)),"{brush:?} below");
        }
    }

    #[test]
    fn a_blob_mask_does_not_clip_displacement_to_the_distance_band() {
        for brush in [Brush::Noise,Brush::Stretch] {
            let mut plain = slab(); let mut masked = plain.clone();
            let mut s = stroke(brush,&[[0.0,0.0,0.0]],0.18,0.8);
            s.delta = Some(Vec3::Y * 0.22);
            s.pattern = Some("folds".into()); s.scale = Some(0.16); s.normal = Some(Vec3::Z);
            apply(&mut plain,&s).unwrap();
            s.mask = Some(Mask {blobs:Some(vec!["slab".into()]),above:None,below:None,..Default::default()});
            apply(&mut masked,&s).unwrap();
            assert!(serde_json::to_value(&plain.detail).unwrap() == serde_json::to_value(&masked.detail).unwrap(),"{brush:?} clipped by mask");
            assert_eq!(components(&masked.build_mesh()),1,"{brush:?} detached surface");
        }
    }

    #[test]
    fn folds_follow_gravity_and_are_repeatable() {
        let mut a = slab(); let mut b = a.clone();
        let mut s = stroke(Brush::Noise,&[[0.0,0.0,0.0]],0.4,0.6);
        s.pattern = Some("folds".into()); s.scale = Some(0.1); s.normal = Some(Vec3::Z);
        apply(&mut a,&s).unwrap(); apply(&mut b,&s).unwrap();
        assert!(serde_json::to_value(&a.detail).unwrap() == serde_json::to_value(&b.detail).unwrap(), "folds must be deterministic");
        // Folds run along Z: the height changes much more across them (x)
        // than along them (z).
        let spread = |f: &dyn Fn(f32) -> f32| {
            let h: Vec<f32> = (0..9).map(|i| f(-0.12 + 0.03 * i as f32)).collect();
            h.iter().cloned().fold(f32::MIN, f32::max) - h.iter().cloned().fold(f32::MAX, f32::min)
        };
        let across = spread(&|x| top(&a, x, 0.0));
        let along = spread(&|z| top(&a, 0.0, z));
        assert!(across > 0.008 && across > 2.0 * along, "across {across} along {along}");
    }

    fn ball() -> Clay {
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 1.0 })).unwrap();
        Clay { blobs: vec![blob], resolution: 48, detail: None, paint: None, detail_cell: None, ..Default::default() }
    }

    fn stroke(brush: Brush, points: &[[f32; 3]], radius: f32, strength: f32) -> Stroke {
        Stroke { brush, points: points.iter().map(|p| Vec3::from(*p)).collect(), radius, strength, delta: None, normal: None, mirror: false, scale: None, pattern: None, invert: false, mask: None }
    }

    fn max_y(c: &Clay) -> f32 {
        c.build_mesh().bounds().1.y
    }

    #[test]
    fn a_wide_grab_works_on_a_fine_head_sized_clay() {
        // A 2 cm grab on a 0.5 mm grid is too big to read at once; it goes
        // in tiles near the surface (elf6: "stroke is too big").
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "blend": 0.0 })).unwrap();
        let mut c = Clay { blobs: vec![blob], resolution: 128, detail: None, paint: None, detail_cell: None, ..Default::default() };
        c.set_detail_cell(0.0005).unwrap();
        let mut s = stroke(Brush::Grab, &[[0.0, 0.1, 0.0]], 0.03, 1.0);
        s.delta = Some(Vec3::new(0.0, 0.02, 0.0));
        let r = apply(&mut c, &s).unwrap();
        assert!(r.changed_points > 1000, "{}", r.changed_points);
        let field = Field::new(&c, 0.0005);
        let at = |p: Vec3| field.sample(p, &field.near(p - 0.01, p + 0.01)).0;
        // The top was pulled up about 1.3 cm; the bottom stays.
        assert!(at(Vec3::new(0.0, 0.108, 0.0)) < 0.0, "{}", at(Vec3::new(0.0, 0.108, 0.0)));
        assert!(at(Vec3::new(0.0, 0.118, 0.0)) > 0.0);
        assert!(at(Vec3::new(0.0, -0.105, 0.0)) > 0.0);
        // A wide inflate also works.
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 0.0, 0.1]], 0.03, 0.5)).unwrap();
    }

    #[test]
    fn scales_overlap_along_the_flow_and_part_with_grooves() {
        // Along the flow each scale rises to its free edge, then drops to
        // the next one's root: the height has sharp falls in that direction.
        let flow = Vec3::NEG_Y;
        let samples: Vec<f32> = (0..400).map(|i| scale_height(Vec3::new(0.37, -(i as f32) * 0.01, 0.21), flow)).collect();
        let falls = samples.windows(2).filter(|w| w[0] - w[1] > 0.15).count();
        assert!((2..=8).contains(&falls), "{falls} falls over 4 scales");
        assert!(samples.iter().all(|h| (0.0..=1.0).contains(h)));
        // Plates: one groove per plate width.
        let plates = (0..300).map(|i| plate_height(i as f32 * 0.01)).collect::<Vec<_>>();
        assert_eq!(plates.windows(2).filter(|w| w[0] >= 0.05 && w[1] < 0.05).count(), 3);
        // The brush raises scales on a clay surface.
        let mut c = ball();
        c.set_detail_cell(0.01).unwrap();
        let mut st = stroke(Brush::Noise, &[[0.0, 0.0, 1.0]], 0.5, 1.0);
        (st.scale, st.pattern) = (Some(0.08), Some("scales".into()));
        assert!(apply(&mut c, &st).unwrap().changed_points > 500);
    }

    #[test]
    fn untouched_detail_changes_nothing() {
        let mut c = ball();
        // A zero-strength stroke and a far-away one leave the ball alone.
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 0.0)).unwrap();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 5.0, 0.0]], 0.2, 1.0)).unwrap();
        let mesh = c.build_mesh();
        for v in &mesh.vertices {
            let r = Vec3::from(v.pos).length();
            assert!((r - 1.0).abs() < 0.05, "vertex at radius {r}");
        }
    }

    #[test]
    fn inflate_deflate_add_carve_move_the_top() {
        let top = [[0.0, 1.0, 0.0]];
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &top, 0.5, 1.0)).unwrap();
        assert!(max_y(&c) > 1.15, "inflate: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Deflate, &top, 0.5, 1.0)).unwrap();
        assert!(max_y(&c) < 0.9, "deflate: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Add, &[[0.0, 1.0, 0.0], [0.0, 1.5, 0.0]], 0.15, 1.0)).unwrap();
        assert!(max_y(&c) > 1.55, "add: {}", max_y(&c));

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Carve, &[[-2.0, 1.0, 0.0], [2.0, 1.0, 0.0]], 0.4, 1.0)).unwrap();
        // The groove runs along X at z = 0: the middle of the top is gone.
        let groove = c.build_mesh().vertices.iter().filter(|v| v.pos[2].abs() < 0.05).map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert!(groove < 0.7, "carve: {groove}");
    }

    #[test]
    fn grab_pulls_and_flatten_cuts() {
        let mut c = ball();
        let mut s = stroke(Brush::Grab, &[[0.0, 0.0, 1.0]], 0.5, 1.0);
        s.delta = Some(Vec3::new(0.0, 0.0, 0.3));
        apply(&mut c, &s).unwrap();
        let hi = c.build_mesh().bounds().1;
        assert!(hi.z > 1.2, "grab: {hi}");

        let mut c = ball();
        apply(&mut c, &stroke(Brush::Flatten, &[[0.0, 0.8, 0.0]], 0.9, 1.0)).unwrap();
        assert!(max_y(&c) < 0.9, "flatten: {}", max_y(&c));
    }

    #[test]
    fn smooth_removes_a_bump() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Add, &[[0.0, 1.0, 0.0]], 0.12, 1.0)).unwrap();
        let bumped = max_y(&c);
        for _ in 0..4 {
            apply(&mut c, &stroke(Brush::Smooth, &[[0.0, 1.0, 0.0]], 0.4, 1.0)).unwrap();
        }
        assert!(max_y(&c) < bumped - 0.03, "smooth: {bumped} -> {}", max_y(&c));
    }

    #[test]
    fn detail_survives_json_and_compares_by_identity() {
        let mut c = ball();
        apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 1.0, 0.0]], 0.5, 1.0)).unwrap();
        let text = serde_json::to_string(&c).unwrap();
        let back: Clay = serde_json::from_str(&text).unwrap();
        let (a, b) = (c.build_mesh().bounds().1.y, back.build_mesh().bounds().1.y);
        assert!((a - b).abs() < 0.01, "{a} vs {b}");
        let copy = c.clone();
        assert_eq!(copy, c);
        apply(&mut c, &stroke(Brush::Smooth, &[[0.0, 1.0, 0.0]], 0.3, 0.5)).unwrap();
        assert_ne!(copy, c, "an edit must change identity");
    }

    #[test]
    fn bad_strokes_are_refused() {
        let mut c = ball();
        assert!(apply(&mut c, &stroke(Brush::Add, &[], 0.2, 1.0)).is_err());
        assert!(apply(&mut c, &stroke(Brush::Add, &[[0.0, 0.0, 0.0]], f32::NAN, 1.0)).is_err());
        assert!(apply(&mut c, &stroke(Brush::Grab, &[[0.0, 0.0, 0.0]], 0.2, 1.0)).is_err(), "grab needs delta");
        assert!(apply(&mut c, &stroke(Brush::Inflate, &[[0.0, 0.0, 0.0]], 1e6, 1.0)).is_err(), "too big");
    }
}
