//! `quality_report` (docs/QUALITY_PLAN.md Stage 2): one call that measures
//! a clay against thresholds and says where to fix it. Lumpiness,
//! curvature and wall thickness come from the clay's field (`quality.rs`,
//! D12), topology from its regular mesh. Every point it names is in world
//! space at rest, where `sculpt` and `edit_clay polish {region}` take them.
//!
//! The field part is a pure function of the clay and the call, so a large
//! report (more than 40,000 samples) runs it as a job; the reply is the
//! same either way, on any number of threads.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::Started;
use super::tools::text;
use crate::clay::{Clay, Form};
use crate::mcp::ToolResult;
use crate::mesh::Mesh;
use crate::progress::Progress;
use crate::quality::{self, Lumpiness, Point, Region};

/// Most regions and close-ups a call scores.
pub(super) const MAX_REGIONS: usize = 16;
pub(super) const MAX_CLOSEUPS: usize = 16;
/// Reports with more samples run as jobs.
const JOB_SAMPLES: usize = quality::DEFAULT_SAMPLES;
/// Thin spots listed.
const MAX_THIN: usize = 8;

/// The contour p95 threshold (% of the subject's height), D-c: a thin
/// part missing from a silhouette (a limb, an antenna) moves the IoU and
/// the mean little and the p95 by about its length. Clean fixtures read
/// 0-0.34%; a missing antenna 6.7% and missing forearms 9.8%.
const CONTOUR_P95_PCT: f32 = 3.0;

/// Reports comparing more views run as jobs.
const JOB_VIEWS: usize = 6;

/// Whether a `quality_report` call runs as a background job.
pub(super) fn is_job(args: &Value) -> bool {
    args["samples"].as_u64().is_some_and(|n| n as usize > JOB_SAMPLES) || args["views"].as_array().is_some_and(|v| v.len() > JOB_VIEWS)
}

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

fn round3(v: Vec3) -> [f64; 3] {
    v.to_array().map(round)
}

/// D14's thresholds, each overridable per call.
#[derive(Clone, Copy, Debug)]
struct Thresholds {
    lumpiness: f32,
    iou: f32,
    contour_pct: f32,
    /// The contour's 95th percentile (thin parts the mean averages away).
    contour_p95_pct: f32,
    feature_pct: f32,
    min_wall: f32,
    /// Poly targets: the largest distance from the shown level to the
    /// clay it was made from, in that clay's cells.
    hausdorff_cells: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { lumpiness: 0.05, iou: 0.95, contour_pct: 1.0, contour_p95_pct: CONTOUR_P95_PCT, feature_pct: 1.5, min_wall: 0.002, hausdorff_cells: 1.0 }
    }
}

impl Thresholds {
    fn parse(v: &Value) -> Result<Thresholds, String> {
        let mut t = Thresholds::default();
        let Some(map) = v.as_object() else {
            return if v.is_null() { Ok(t) } else { Err("thresholds must be an object".into()) };
        };
        for (key, value) in map {
            let x = value.as_f64().filter(|x| x.is_finite() && *x >= 0.0).ok_or(format!("thresholds.{key} must be a number >= 0"))? as f32;
            match key.as_str() {
                "lumpiness" => t.lumpiness = x,
                "iou" if x <= 1.0 => t.iou = x,
                "contour_pct" => t.contour_pct = x,
                "contour_p95_pct" => t.contour_p95_pct = x,
                "feature_pct" => t.feature_pct = x,
                "min_wall" => t.min_wall = x,
                "hausdorff_cells" => t.hausdorff_cells = x,
                "iou" => return Err("thresholds.iou is at most 1".into()),
                other => return Err(format!("unknown threshold {other}; use lumpiness, iou, contour_pct, contour_p95_pct, feature_pct, min_wall or hausdorff_cells")),
            }
        }
        Ok(t)
    }

    fn json(&self) -> Value {
        json!({ "lumpiness": round(self.lumpiness), "iou": round(self.iou), "contour_pct": round(self.contour_pct), "contour_p95_pct": round(self.contour_p95_pct), "feature_pct": round(self.feature_pct), "min_wall": round(self.min_wall) })
    }
}

/// A call, checked from its arguments alone (before anything is sampled).
struct Request {
    /// The silhouette views asked for (None: every loaded slot).
    views: Option<Vec<Value>>,
    /// Close-ups: (name, view, center, radius, size), checked.
    closeups: Vec<(String, Value, Vec3, f32, usize)>,
    samples: usize,
    /// World space: (name, center, radius, exclude).
    regions: Vec<(String, Vec3, f32, bool)>,
    thresholds: Thresholds,
    images: bool,
}

impl Request {
    fn parse(args: &Value) -> Result<Request, String> {
        let samples = match args.get("samples").filter(|v| !v.is_null()) {
            None => quality::DEFAULT_SAMPLES,
            Some(v) => v.as_u64().map(|n| n as usize).filter(|n| (quality::MIN_SAMPLES..=quality::MAX_SAMPLES).contains(n)).ok_or(format!("samples must be a whole number from {} to {}", quality::MIN_SAMPLES, quality::MAX_SAMPLES))?,
        };
        let regions = match args.get("regions").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(Value::Array(list)) if list.len() <= MAX_REGIONS => list
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let center: [f32; 3] = serde_json::from_value(r["center"].clone()).map_err(|_| format!("regions[{i}].center must be [x, y, z]"))?;
                    let radius = r["radius"].as_f64().filter(|x| x.is_finite() && *x > 0.0).ok_or(format!("regions[{i}].radius must be a number > 0"))? as f32;
                    let name = r["name"].as_str().map_or_else(|| format!("region {i}"), str::to_string);
                    let center = Vec3::from(center);
                    if !center.is_finite() {
                        return Err(format!("regions[{i}].center must be finite"));
                    }
                    Ok((name, center, radius, r["exclude"].as_bool().unwrap_or(false)))
                })
                .collect::<Result<_, String>>()?,
            Some(Value::Array(list)) => return Err(format!("at most {MAX_REGIONS} regions (got {})", list.len())),
            Some(_) => return Err("regions must be a list of {name, center, radius, exclude}".into()),
        };
        let views = match args.get("views").filter(|v| !v.is_null()) {
            None => None,
            Some(Value::Array(v)) if v.len() <= super::eyes::MAX_VIEWS => Some(v.clone()),
            Some(Value::Array(v)) => return Err(format!("at most {} views (got {})", super::eyes::MAX_VIEWS, v.len())),
            Some(_) => return Err("views must be a list of views".into()),
        };
        let closeups = match args.get("closeups").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(Value::Array(list)) if list.len() <= MAX_CLOSEUPS => list
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let (center, radius, size) = super::eyes::closeup_args(c).map_err(|e| format!("closeups[{i}]: {e}"))?.ok_or(format!("closeups[{i}] needs center and radius"))?;
                    let name = c["name"].as_str().map_or_else(|| format!("closeup {i}"), str::to_string);
                    Ok((name, c.get("view").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| Value::from("front")), center, radius, size))
                })
                .collect::<Result<_, String>>()?,
            Some(Value::Array(list)) => return Err(format!("at most {MAX_CLOSEUPS} closeups (got {})", list.len())),
            Some(_) => return Err("closeups must be a list of {name, center, radius, view, size}".into()),
        };
        Ok(Request { views, closeups, samples, regions, thresholds: Thresholds::parse(&args["thresholds"])?, images: args["images"].as_bool().unwrap_or(false) })
    }
}

/// What the field part found, in the clay's space.
struct Measured {
    points: Vec<Point>,
    unfit: usize,
    lumpiness: Lumpiness,
    p95_abs_h: Option<f32>,
    /// Each point's wall thickness up to `reach` (None: thicker).
    walls: Vec<Option<f32>>,
    reach: f32,
    /// Legacy sweeps owning surface points: (blob index, name, points).
    legacy: Vec<(usize, String, usize)>,
    /// The worst points' owners, as `lumpiness.worst`.
    worst_owner: Vec<Option<usize>>,
    /// Welded triangles, edges not used twice, edges used the same way.
    triangles: usize,
    non_manifold: usize,
    flipped: usize,
}

/// Failures of the silhouettes and close-ups against `t`.
fn view_failures(silhouette: &[Silhouette], closeups: &[Value], t: &Thresholds) -> Vec<String> {
    let mut failures = Vec::new();
    for v in silhouette {
        if v.iou < t.iou {
            failures.push(format!("iou {} {:.4} < {}", v.view, v.iou, t.iou));
        }
        if let Some(c) = v.contour.filter(|c| c.mean_pct > t.contour_pct) {
            failures.push(format!("contour {} {:.2}% > {}%", v.view, c.mean_pct, t.contour_pct));
        }
        if let Some(c) = v.contour.filter(|c| c.p95_pct > t.contour_p95_pct) {
            failures.push(format!("contour_p95 {} {:.2}% > {}%", v.view, c.p95_pct, t.contour_p95_pct));
        }
    }
    for c in closeups {
        let name = c["name"].as_str().unwrap_or("?");
        // The model shows no line where the reference draws some.
        if c["model_line_px"] == 0 {
            failures.push(format!("features {name}: no model lines where the reference has {} line pixels (recall 0)", c["reference_line_px"]));
        } else if let Some(p) = c["feature_pct"].as_f64().filter(|p| *p as f32 > t.feature_pct) {
            failures.push(format!("features {name} {p:.2}% > {}%", t.feature_pct));
        }
    }
    failures
}

/// The field part: samples, lumpiness, curvature, walls and topology of
/// `clay` (its regular mesh `rest`), all in the clay's space. Pure.
fn measure(clay: &Clay, rest: &Mesh, samples: usize, regions: &[Region], reach: f32, progress: &Progress) -> Result<Measured, String> {
    progress.stage("measuring the surface", 0.0);
    let (probe, points, unfit) = quality::sample_clay(clay, rest, samples).ok_or("the clay is empty")?;
    progress.check()?;
    progress.stage("measuring walls", 0.7);
    let walls = probe.walls(&points, reach);
    progress.check()?;
    progress.stage("checking the mesh", 0.9);
    let lumpiness = quality::lumpiness(&points, regions);
    let legacy_sweep = |i: usize| clay.blobs.get(i).filter(|b| matches!(&b.form, Form::Sweep { field, .. } if field.is_legacy()));
    let mut legacy: Vec<(usize, String, usize)> = Vec::new();
    // Owners of a spread of the points (every point would cost a field
    // walk each): which legacy sweeps make surface at all.
    let step = (points.len() / 2000).max(1);
    for p in points.iter().step_by(step) {
        if let Some(i) = probe.owner(p.p).filter(|&i| legacy_sweep(i).is_some()) {
            match legacy.iter_mut().find(|l| l.0 == i) {
                Some(l) => l.2 += step,
                None => legacy.push((i, clay.blobs[i].name.clone(), step)),
            }
        }
    }
    legacy.sort_by_key(|l| l.0);
    let worst_owner = lumpiness.worst.iter().map(|w| probe.owner(w.0)).collect();
    let positions: Vec<Vec3> = rest.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let tris: Vec<[u32; 3]> = rest.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    let (_, welded) = crate::mesh_asset::weld(&positions, &tris);
    let (_, kinds, flipped) = crate::mesh_asset::edge_report(&welded);
    progress.set(1.0);
    Ok(Measured { p95_abs_h: quality::p95_abs_h(&points), points, unfit, lumpiness, walls, reach, legacy, worst_owner, triangles: tris.len(), non_manifold: kinds.open + kinds.over_used + kinds.pinched, flipped })
}

/// One view's silhouette against its reference.
struct Silhouette {
    view: String,
    slot: &'static str,
    iou: f32,
    missing: f32,
    extra: f32,
    regions: Vec<Value>,
    contour: Option<super::eyes::Distances>,
}

fn silhouette_json(views: &[Silhouette]) -> Value {
    let Some(worst) = views.iter().min_by(|a, b| a.iou.total_cmp(&b.iou)) else {
        return json!({ "skipped": "no reference loaded: set_reference (one image per view slot) to compare silhouettes" });
    };
    let list: Vec<Value> = views
        .iter()
        .map(|v| json!({ "view": v.view, "slot": v.slot, "iou": round(v.iou), "contour_pct": v.contour.map(|c| round(c.mean_pct)), "contour_p95_pct": v.contour.map(|c| round(c.p95_pct)), "missing": round(v.missing), "extra": round(v.extra), "regions": v.regions }))
        .collect();
    json!({ "views": list, "mean_iou": round(views.iter().map(|v| v.iou).sum::<f32>() / views.len() as f32), "worst_view": worst.view })
}

/// The smallest and largest scale of `world` (along any direction).
fn scales(world: Mat4) -> (f32, f32) {
    let s = world.to_scale_rotation_translation().0.abs();
    (s.min_element().max(1e-9), s.max_element().max(1e-9))
}

/// A wall `w` thick (clay units) behind a point with normal `n`, in world
/// units: the inward segment mapped by `world`, so a non-uniform scale
/// measures it along the normal's own direction.
fn world_wall(world: Mat4, n: Vec3, w: f32) -> f32 {
    world.transform_vector3(n * w).length()
}

/// The field part's checks against the thresholds: the failures (in
/// order), the thinnest wall in world units, and the thin spots (clay
/// space, apart). A report that measured nothing does not pass: no point
/// measured, most samples unfit, no counted point, or a region holding
/// none, each fails with what was missing.
fn field_checks(m: &Measured, world: Mat4, t: &Thresholds) -> (Vec<String>, Option<f32>, Vec<Vec3>) {
    let mut failures: Vec<String> = Vec::new();
    let l = &m.lumpiness;
    let samples = m.points.len() + m.unfit;
    if m.points.is_empty() {
        failures.push(format!("unmeasured: none of the {samples} samples could be measured on the field"));
    } else if m.unfit > m.points.len() {
        failures.push(format!("unmeasured: {} of {samples} samples could not be measured on the field (too thin or too rough to fit there)", m.unfit));
    }
    match l.overall {
        Some(s) if s > t.lumpiness => failures.push(format!("lumpiness overall {:.4} > {}", s, t.lumpiness)),
        Some(_) => {}
        None if !m.points.is_empty() => failures.push("lumpiness unmeasured: every measured point is intended detail or in an excluded region".into()),
        None => {}
    }
    for r in &l.regions {
        match r.score {
            Some(s) if s > t.lumpiness => failures.push(format!("lumpiness {} {:.4} > {}", r.name, s, t.lumpiness)),
            Some(_) => {}
            None => failures.push(format!("lumpiness {} unmeasured: no counted point in the region", r.name)),
        }
    }
    // Thin walls: the thinnest points, apart (in world units).
    let mut thin: Vec<(f32, Vec3)> = m.points.iter().zip(&m.walls).filter_map(|(p, w)| w.map(|w| (world_wall(world, p.n, w), p.p))).collect();
    thin.sort_by(|a, b| a.0.total_cmp(&b.0));
    let min_wall = thin.first().map(|w| w.0);
    let mut thin_at: Vec<Vec3> = Vec::new();
    for (w, p) in thin.iter().filter(|w| w.0 < t.min_wall) {
        if thin_at.len() == MAX_THIN {
            break;
        }
        if thin_at.iter().all(|q| world.transform_point3(*q).distance(world.transform_point3(*p)) >= 4.0 * w.max(t.min_wall)) {
            thin_at.push(*p);
        }
    }
    if let Some(w) = min_wall.filter(|w| *w < t.min_wall) {
        failures.push(format!("min_wall {w:.4} < {}", t.min_wall));
    }
    (failures, min_wall, thin_at)
}

/// The quality sheets' meshes: `mesh` (the clay's drawn mesh unposed, in
/// its space) colored by curvature, then by lumpiness from the measured
/// points. Pure: a job makes them from its snapshot.
fn sheet_meshes(clay: &Clay, mesh: &Mesh, points: &[Point]) -> [Arc<Mesh>; 2] {
    let at: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let curvature = quality::Probe::new(clay).map(|p| p.curvatures(&at)).unwrap_or_default();
    let colored = |colors: Vec<[f32; 3]>| {
        let mut out = mesh.clone();
        for (v, c) in out.vertices.iter_mut().zip(colors) {
            v.color = c;
        }
        Arc::new(out)
    };
    [colored(curvature.into_iter().map(|h| quality::curvature_color(h).to_array()).collect()), colored(quality::lumpiness_colors(points, &at))]
}

/// The clay-space regions of world-space ones (`to_local` at rest, its
/// largest scale for the radius).
fn local_regions(req: &Request, to_local: Mat4) -> Vec<Region> {
    let scale = to_local.to_scale_rotation_translation().0.max_element();
    req.regions.iter().map(|(name, c, r, exclude)| Region { name: name.clone(), center: to_local.transform_point3(*c), radius: r * scale, exclude: *exclude }).collect()
}

impl ClideApp {
    /// `quality_report` tool (run at rest): the field part now, or as a
    /// job when `job` (`is_job`).
    pub(super) fn quality_report(&mut self, args: &Value, job: bool) -> Result<Started, String> {
        let req = Request::parse(args)?;
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if o.shape == crate::scene::Shape::Poly {
            let proportions = self.proportions(id, args)?;
            return Ok(Started::Now(self.poly_quality(id, &req, proportions)));
        }
        if o.shape != crate::scene::Shape::Clay {
            return Err(format!("{} is not a clay or Poly object (quality_report measures those)", o.name));
        }
        // It reads the composed field (and draws what is under the target):
        // unchecked ledgers settle first.
        let drawn: Vec<u64> = std::iter::once(id).chain(self.scene.descendants(id)).collect();
        self.settle_clays(drawn)?;
        self.meshes.sync(&self.scene);
        let clay = self.scene.get(id).and_then(|o| o.clay.clone()).ok_or("not a clay object")?;
        let rest = self.meshes.rest(id).map(|(m, _)| m.clone()).ok_or("the clay has no mesh yet")?;
        // The sheets color the mesh the viewport draws (fine or baked when
        // shown), unposed: vertex for vertex where its field is.
        let sheet = req.images.then(|| self.meshes.unposed(id).cloned()).flatten();
        let world = crate::pose::at_rest(&mut self.scene, |s| s.world_matrix(id));
        let to_local = world.inverse();
        let regions = local_regions(&req, to_local);
        // Walls are followed to four times the threshold (in the clay's
        // units, along its most shrunken axis).
        let reach = (4.0 * req.thresholds.min_wall / scales(world).0).max(clay.detail_cell.or_else(|| clay.cell_size()).unwrap_or(0.01) * 2.0);
        let name = self.scene.get(id).map(|o| o.name.clone()).unwrap_or_default();
        // Silhouettes now (renders, on this thread), of the target and
        // what is under it (its eyes, say).
        let silhouette = self.silhouettes(id, &req)?;
        let closeups = self.closeups(id, &req)?;
        // The proportion guides, when set (`guides measure` on the target).
        let proportions = self.proportions(id, args)?;
        if !job {
            let m = measure(&clay, &rest, req.samples, &regions, reach, &Progress::default())?;
            let sheets = sheet.map(|mesh| sheet_meshes(&clay, &mesh, &m.points));
            return Ok(Started::Now(self.quality_reply(id, &name, &req, world, &clay, m, silhouette, closeups, sheets, proportions)));
        }
        let samples = req.samples;
        Ok(Started::Job(format!("Quality report {name}"), Box::new(move |progress| {
            let m = measure(&clay, &rest, samples, &regions, reach, progress)?;
            // The sheets from the snapshot measured, drawn at rest.
            let sheets = sheet.map(|mesh| sheet_meshes(&clay, &mesh, &m.points));
            Ok(Box::new(move |app: &mut ClideApp| {
                app.at_rest(|app| {
                    // Whatever else is drawn, at rest as the call drew it.
                    app.meshes.sync(&app.scene);
                    app.quality_reply(id, &name, &req, world, &clay, m, silhouette, closeups, sheets, proportions)
                })
            }))
        })))
    }

    /// The target's silhouettes against the references (`views`, else
    /// every loaded slot; none when nothing is loaded and none asked).
    fn silhouettes(&self, id: u64, req: &Request) -> Result<Vec<Silhouette>, String> {
        let picks: Vec<(Value, super::eyes::Slot)> = match &req.views {
            Some(views) => views.iter().map(|v| self.references.slot_for(v).map(|s| (v.clone(), s))).collect::<Result<_, _>>()?,
            None => self.references.loaded().into_iter().map(|s| (Value::from(s.name()), s)).collect(),
        };
        let only: std::collections::HashSet<u64> = std::iter::once(id).chain(self.scene.descendants(id)).collect();
        picks
            .iter()
            .map(|(v, slot)| {
                let c = self.compare_view(v, *slot, false, None, false, Some(&only))?;
                Ok(Silhouette { view: c.view, slot: c.slot.name(), iou: c.iou, missing: c.missing, extra: c.extra, regions: c.regions, contour: c.contour })
            })
            .collect()
    }

    /// The close-ups against their views' references, with feature lines
    /// (none when nothing is loaded).
    fn closeups(&self, id: u64, req: &Request) -> Result<Vec<Value>, String> {
        if req.closeups.is_empty() || self.references.is_empty() {
            return Ok(Vec::new());
        }
        let only: std::collections::HashSet<u64> = std::iter::once(id).chain(self.scene.descendants(id)).collect();
        req.closeups
            .iter()
            .map(|(name, view, center, radius, size)| {
                let slot = self.references.slot_for(view)?;
                let c = self.compare_view(view, slot, false, Some((*center, *radius, *size)), true, Some(&only)).map_err(|e| format!("close-up {name}: {e}"))?;
                let f = c.features.and_then(|f| f.lines.map(|l| (f.pct(), l)));
                let interior = c.closeup.as_ref().map(|v| v["interior"].clone()).unwrap_or(Value::Null);
                Ok(json!({ "name": name, "view": c.view, "slot": c.slot.name(), "iou": round(c.iou), "feature_pct": f.and_then(|f| f.0).map(round), "recall": f.map(|f| round(f.1.recall)), "precision": f.map(|f| round(f.1.precision)), "reference_line_px": f.map(|f| f.1.reference_px), "model_line_px": f.map(|f| f.1.model_px), "interior": interior }))
            })
            .collect()
    }

    /// The reply: numbers, failures against the thresholds, hints, and
    /// with `images` the curvature and lumpiness sheets.
    #[allow(clippy::too_many_arguments)]
    fn quality_reply(&self, id: u64, name: &str, req: &Request, world: Mat4, clay: &Clay, m: Measured, silhouette: Vec<Silhouette>, closeups: Vec<Value>, sheets: Option<[Arc<Mesh>; 2]>, proportions: Option<crate::guides::Measured>) -> ToolResult {
        let t = req.thresholds;
        let (least, scale) = scales(world);
        let at = |p: Vec3| round3(world.transform_point3(p));
        let mut hints: Vec<String> = Vec::new();
        let l = &m.lumpiness;
        let (mut failures, min_wall, thin_at) = field_checks(&m, world, &t);
        failures.extend(view_failures(&silhouette, &closeups, &t));
        if let Some(p) = &proportions {
            failures.extend(p.failures.iter().cloned());
        }
        let watertight = m.non_manifold == 0 && m.flipped == 0 && m.triangles > 0;
        if !watertight {
            failures.push(format!("not watertight: {} edges not used twice, {} used the same way round", m.non_manifold, m.flipped));
        }
        // Where to aim: polish the worst lump of the worst failing part (the
        // default light pass keeps grooves), and upgrade legacy sweeps that own lumps.
        let lumpy = failures.iter().any(|f| f.starts_with("lumpiness"));
        let failing = l.regions.iter().filter(|r| r.score.is_some_and(|s| s > t.lumpiness)).max_by(|a, b| a.score.unwrap_or(0.0).total_cmp(&b.score.unwrap_or(0.0)));
        let aim = failing.and_then(|r| r.worst_at).or_else(|| l.worst.first().map(|w| w.0));
        if let Some(p) = aim.filter(|_| lumpy) {
            let radius = round(quality::SIGMA * 4.0 * m.points.iter().map(|q| q.cell).fold(0.0, f32::max) * scale);
            hints.push(format!("aim at {:?}: edit_clay {{target: {id}, polish: {{amount: 0.1, region: {{center: {:?}, radius: {radius}}}}}}} smooths the lumps there and keeps grooves (polish's default amount; more rounds short grooves too); render_views shade: lumpiness shows where the rest are", at(p), at(p)));
        }
        // Legacy sweeps owning worst points: one hint per sweep.
        let mut on_legacy: Vec<(usize, Vec<usize>)> = Vec::new();
        for (k, owner) in m.worst_owner.iter().enumerate() {
            if let Some(i) = owner.filter(|i| l.worst[k].1.abs() > t.lumpiness && m.legacy.iter().any(|l| l.0 == *i)) {
                match on_legacy.iter_mut().find(|e| e.0 == i) {
                    Some(e) => e.1.push(k),
                    None => on_legacy.push((i, vec![k])),
                }
            }
        }
        for (i, points) in on_legacy {
            let blob = &clay.blobs[i];
            let what = if blob.name.is_empty() { format!("blob {i}") } else { format!("blob {i} ({})", blob.name) };
            let which: Vec<String> = points.iter().map(|k| k.to_string()).collect();
            let (noun, verb) = if points.len() == 1 { ("point", "lies") } else { ("points", "lie") };
            hints.push(format!("worst {noun} {} {verb} on legacy sweep {what}: edit_clay {{target: {id}, update: [{{blob: {i}, field: \"segments\"}}]}} gives it the exact sweep field (a circle profile becomes a smooth round tube)", which.join(", ")));
        }
        if !m.legacy.is_empty() && !hints.iter().any(|h| h.contains("legacy sweep")) {
            let list: Vec<String> = m.legacy.iter().map(|(i, n, _)| if n.is_empty() { i.to_string() } else { format!("{i} ({n})") }).collect();
            hints.push(format!("legacy sweeps make surface here (blobs {}): edit_clay update {{blob, field: \"segments\"}} upgrades each to the exact tube field", list.join(", ")));
        }
        let regions: Vec<Value> = l.regions.iter().map(|r| json!({ "name": r.name, "score": r.score.map(round), "worst_at": r.worst_at.map(at), "points": r.points })).collect();
        // Only points lumpier than the threshold are worth aiming at.
        let worst: Vec<Value> = l.worst.iter().filter(|w| w.1.abs() > t.lumpiness).map(|(p, r)| json!({ "at": at(*p), "residual_cells": round(*r) })).collect();
        let mut reply = json!({
            "target": id,
            "name": name,
            "pass": failures.is_empty(),
            "failures": failures,
            "hints": hints,
            "thresholds": t.json(),
            "lumpiness": {
                "overall": l.overall.map(round),
                "regions": regions,
                "worst": worst,
                "counted": l.counted,
                "intended": l.intended,
                "lines": l.lines,
                "unmeasured": m.unfit,
                "unit": "cells (the clay's finest sculpt cell at each point, else its detail_cell or mesh cell)",
            },
            "curvature": { "p95_abs_h_cell": m.p95_abs_h.map(round) },
            "silhouette": silhouette_json(&silhouette),
            "closeups": if closeups.is_empty() && !req.closeups.is_empty() { json!({ "skipped": "no reference loaded: set_reference to compare close-ups" }) } else { json!(closeups) },
            "topology": {
                "watertight": watertight,
                "non_manifold": m.non_manifold,
                "flipped": m.flipped,
                "min_wall": min_wall.map(round),
                "min_wall_reach": round(m.reach * least),
                "thin_at": thin_at.iter().map(|p| at(*p)).collect::<Vec<_>>(),
                "triangles": m.triangles,
            },
            "samples": m.points.len() + m.unfit,
        });
        if let Some(p) = proportions {
            reply["proportions"] = p.reply;
        }
        if !m.legacy.is_empty() {
            reply["legacy_sweeps"] = json!(m.legacy.iter().map(|(i, n, k)| json!({ "blob": i, "name": n, "points": k })).collect::<Vec<_>>());
        }
        let mut out = text(reply);
        if req.images {
            out.extend(self.quality_sheet(id, sheets.ok_or("the clay has no mesh yet")?)?);
        }
        Ok(out)
    }

    /// `quality_report` on a Poly: its shown level's topology, the cage's
    /// quads and irregular points, the largest distance from the shown
    /// level to the clay it was made from (`made_by`), and silhouettes and
    /// close-ups as for clay. Lumpiness, curvature and walls read a clay's
    /// field: they are not measured on a Poly.
    fn poly_quality(&mut self, id: u64, req: &Request, proportions: Option<crate::guides::Measured>) -> ToolResult {
        let made_by = self.scene.get(id).and_then(|o| o.poly.as_ref()).and_then(|p| p.made_by.clone());
        let source = made_by.as_ref().and_then(|m| m["source"].as_u64()).filter(|s| self.scene.get(*s).is_some_and(|o| o.clay.is_some()));
        // Its field is read: unchecked ledgers settle first.
        let drawn: Vec<u64> = std::iter::once(id).chain(self.scene.descendants(id)).chain(source).collect();
        self.settle_clays(drawn)?;
        self.meshes.sync(&self.scene);
        let (surface, world) = self.poly_shown(id)?;
        let p = self.scene.get(id).and_then(|o| o.poly.clone()).expect("a Poly");
        let info = p.info();
        let t = req.thresholds;
        let mut failures = Vec::new();
        let triangles = surface.triangles();
        let (edges, bad, flipped) = crate::mesh_asset::edge_use(&triangles);
        let watertight = edges > 0 && bad == 0 && flipped == 0;
        if !watertight {
            failures.push(format!("not watertight: {bad} edges not used twice, {flipped} used the same way round"));
        }
        let distance = source.and_then(|s| {
            let clay = self.scene.get(s)?.clay.as_ref()?;
            let to_clay = self.scene.world_matrix(s).inverse() * world;
            // Its points, edge middles and face middles (as retopo
            // measures): a cage's points lie on the clay, its faces need not.
            let mut at: Vec<Vec3> = surface.points.clone();
            at.extend(surface.edges.iter().map(|&[a, b]| (surface.points[a as usize] + surface.points[b as usize]) * 0.5));
            at.extend(surface.face_start.windows(2).map(|w| {
                let face = &surface.corners[w[0] as usize..w[1] as usize];
                face.iter().map(|&v| surface.points[v as usize]).sum::<Vec3>() / face.len() as f32
            }));
            let points: Vec<Vec3> = at.iter().map(|q| to_clay.transform_point3(*q)).collect();
            super::poly::distance_cells(clay, &points)
        });
        if let Some((d, _, _)) = distance.filter(|d| d.0 > t.hausdorff_cells) {
            failures.push(format!("hausdorff {d:.3} cells > {}", t.hausdorff_cells));
        }
        let silhouette = self.silhouettes(id, req)?;
        let closeups = self.closeups(id, req)?;
        failures.extend(view_failures(&silhouette, &closeups, &t));
        if let Some(p) = &proportions {
            failures.extend(p.failures.iter().cloned());
        }
        let mut thresholds = t.json();
        thresholds["hausdorff_cells"] = json!(round(t.hausdorff_cells));
        let name = self.scene.get(id).map(|o| o.name.clone()).unwrap_or_default();
        let notes: Vec<&str> = [
            source.is_none().then_some("no source clay (made_by names none, or it is gone): hausdorff not measured"),
            (req.images || !req.regions.is_empty() || req.samples != quality::DEFAULT_SAMPLES).then_some("samples, regions and images apply to clay targets; a Poly's are not measured"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut reply = json!({
            "target": id,
            "name": name,
            "pass": failures.is_empty(),
            "failures": failures,
            "thresholds": thresholds,
            "poly": {
                "quads_pct": info["quads_pct"],
                "irregular_pct": info["irregular_pct"],
                "hausdorff_cells": distance.map(|d| round(d.0)),
                "mean_distance_cells": distance.map(|d| round(d.1)),
                "distance_samples": distance.map(|d| d.2),
                "source": source,
                "level": p.level,
                "faces": p.faces_at(p.level),
            },
            "silhouette": silhouette_json(&silhouette),
            "closeups": if closeups.is_empty() && !req.closeups.is_empty() { json!({ "skipped": "no reference loaded: set_reference to compare close-ups" }) } else { json!(closeups) },
            "topology": { "watertight": watertight, "non_manifold": bad, "flipped": flipped, "triangles": triangles.len(), "genus": info["genus"], "boundary_loops": info["boundary_loops"] },
            "lumpiness": { "skipped": "measured on clay fields; a Poly has none (report its source clay)" },
            "notes": notes,
        });
        if let Some(p) = proportions {
            reply["proportions"] = p.reply;
        }
        Ok(text(reply))
    }

    /// The curvature and lumpiness sheets of the target (front and right,
    /// framed on it): `sheet_meshes`, drawn in its place (at rest).
    fn quality_sheet(&self, id: u64, sheets: [Arc<Mesh>; 2]) -> ToolResult {
        let world = self.scene.world_matrix(id);
        let (lo, hi) = sheets[0].bounds();
        let corners = (0..8).map(|i| world.transform_point3(Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][i >> 1 & 1], [lo.z, hi.z][i >> 2])));
        let (center, radius) = match corners.fold(None, |b: Option<(Vec3, Vec3)>, p| Some(b.map_or((p, p), |(a, c)| (a.min(p), c.max(p))))) {
            Some((lo, hi)) if !sheets[0].vertices.is_empty() => ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.01)),
            _ => self.scene_sphere(),
        };
        let mut tiles = Vec::new();
        for mesh in sheets {
            let shaded = std::collections::HashMap::from([(id, mesh)]);
            for view in ["front", "right"] {
                let shot = super::eyes::Shot::framing(&json!(view), center, radius)?;
                tiles.push(self.render_with(&shot, 320, 320, &shaded)?);
            }
        }
        let mut out = text("Quality sheets, framed on the target: curvature (blue concave, white flat, red convex; mean curvature x cell, +-0.3) front | right, then lumpiness (white smooth to magenta 0.2 cells; grey intended detail) front | right.");
        out.extend(super::tools::encode_png(&super::eyes::contact_sheet(&tiles, 2))?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(p: Vec3, n: Vec3, residual: f32) -> Point {
        Point { p, n, cell: 0.01, residual, sigma: quality::SIGMA, ..Default::default() }
    }

    fn measured(points: Vec<Point>, unfit: usize, regions: &[Region], walls: Vec<Option<f32>>) -> Measured {
        Measured { lumpiness: quality::lumpiness(&points, regions), p95_abs_h: None, walls, reach: 0.1, legacy: Vec::new(), worst_owner: Vec::new(), triangles: 10, non_manifold: 0, flipped: 0, points, unfit }
    }

    /// Nothing measured is a failure, not a pass: no point, most samples
    /// unfit, nothing counted, or a region holding no counted point.
    #[test]
    fn unmeasured_reports_fail() {
        let t = Thresholds::default();
        let clean = |n: usize| (0..n).map(|i| point(Vec3::new(i as f32, 0.0, 0.0), Vec3::Z, 0.001)).collect::<Vec<_>>();
        let ok = measured(clean(10), 2, &[], vec![None; 10]);
        assert!(field_checks(&ok, Mat4::IDENTITY, &t).0.is_empty());
        let none = measured(Vec::new(), 1000, &[], Vec::new());
        assert!(field_checks(&none, Mat4::IDENTITY, &t).0.iter().any(|f| f.starts_with("unmeasured: none of the 1000")));
        let unfit = measured(clean(10), 11, &[], vec![None; 10]);
        assert!(field_checks(&unfit, Mat4::IDENTITY, &t).0.iter().any(|f| f.starts_with("unmeasured: 11 of 21")));
        let all = [Region { name: "all".into(), center: Vec3::ZERO, radius: 100.0, exclude: true }];
        let excluded = measured(clean(10), 0, &all, vec![None; 10]);
        assert!(field_checks(&excluded, Mat4::IDENTITY, &t).0.iter().any(|f| f.starts_with("lumpiness unmeasured")));
        let far = [Region { name: "far".into(), center: Vec3::splat(500.0), radius: 1.0, exclude: false }];
        let empty_region = measured(clean(10), 0, &far, vec![None; 10]);
        assert_eq!(field_checks(&empty_region, Mat4::IDENTITY, &t).0, vec!["lumpiness far unmeasured: no counted point in the region".to_string()]);
    }

    /// A wall is measured along its normal through the object's scale,
    /// not by its largest scale.
    #[test]
    fn walls_follow_a_non_uniform_scale() {
        let t = Thresholds { min_wall: 0.01, ..Default::default() };
        let m = measured(vec![point(Vec3::new(0.0, 0.0, 0.3), Vec3::Z, 0.0), point(Vec3::new(0.3, 0.0, 0.0), Vec3::X, 0.0)], 0, &[], vec![Some(0.03), Some(0.03)]);
        let world = Mat4::from_scale(Vec3::new(2.0, 2.0, 0.25));
        let (failures, min_wall, thin_at) = field_checks(&m, world, &t);
        assert!((min_wall.unwrap() - 0.0075).abs() < 1e-6, "{min_wall:?}");
        assert_eq!(thin_at, vec![Vec3::new(0.0, 0.0, 0.3)]);
        assert!(failures.iter().any(|f| f.starts_with("min_wall 0.0075")), "{failures:?}");
        assert_eq!(scales(world), (0.25, 2.0));
    }
}
