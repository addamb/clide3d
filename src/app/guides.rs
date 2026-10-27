//! The `guides` tool (docs/QUALITY_PLAN.md Stage 5b): proportion guides
//! set, cleared and measured, and drawn over the viewport and over
//! `render_views guides: true`. The measures are `crate::guides`; this
//! reads the scene for them: the target clay (its blobs and composed
//! field), other objects by name, and the turnaround sheet's views.
//!
//! Guides live in `Scene.guides` (saved; `set` and `clear` are undo steps
//! through the frame's scene diff) and are never objects: they are lines
//! painted after a render, like the skeleton, so no export, print, film,
//! mask, silhouette or ray ever meets them.
//!
//! The editor's Display menu sends the same calls: Proportion Guides is
//! `set_display {guides}`, Measure Proportions is `guides {action:
//! measure, target: the selection}`, whose reply the Proportions window
//! shows.

use std::cell::RefCell;
use std::collections::BTreeMap;

use egui::{Color32, ColorImage};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::Slot;
use super::tools::text;
use crate::guides::{self, Axis, ClayModel, Guides, HeadUnits, Kind, Line, Measured, Model, SheetView, Target};
use crate::mcp::ToolResult;
use crate::scene::Shape;

/// The keys each action takes.
const SET_KEYS: [&str; 5] = ["action", "head_units", "lines", "targets", "target"];
const CLEAR_KEYS: [&str; 2] = ["action", "parts"];
const MEASURE_KEYS: [&str; 2] = ["action", "target"];
const PARTS: [&str; 3] = ["head_units", "lines", "targets"];

const UNIT: Color32 = Color32::from_rgb(80, 200, 230);
const NAMED: Color32 = Color32::from_rgb(250, 170, 60);

fn known_keys(v: &Value, keys: &[&str], what: &str) -> Result<(), String> {
    let Some(map) = v.as_object() else { return Err(format!("{what} must be an object")) };
    match map.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("{what} takes no {k} (it takes {})", keys.join(", "))),
        None => Ok(()),
    }
}

/// A list of at most `max` objects, each with only `keys`, read as `T`.
fn list<T: serde::de::DeserializeOwned>(v: &Value, what: &str, max: usize, keys: &[&str]) -> Result<Vec<T>, String> {
    let Some(items) = v.as_array() else { return Err(format!("{what} must be a list")) };
    if items.len() > max {
        return Err(format!("at most {max} {what} (got {})", items.len()));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            known_keys(item, keys, &format!("{what}[{i}]"))?;
            serde_json::from_value(item.clone()).map_err(|e| format!("{what}[{i}]: {e}"))
        })
        .collect()
}

/// The world extent along `axis` of an object's drawn mesh (all its
/// vertices placed), as `align` reads a model.
fn mesh_extent(app: &ClideApp, id: u64, axis: Axis) -> Option<(f32, f32)> {
    let o = app.scene.get(id)?;
    let mesh = app.meshes.get(o)?;
    let world = app.scene.world_matrix(id);
    let a = axis.index();
    mesh.vertices.iter().map(|v| world.transform_point3(Vec3::from(v.pos))[a]).fold(None, |acc, x| Some(acc.map_or((x, x), |(lo, hi): (f32, f32)| (lo.min(x), hi.max(x)))))
}

/// The scene as a measure reads it: the target (a clay's blobs and
/// field, else its mesh), and other objects by name (a clay by its field,
/// anything else by its mesh). Reference planes are never parts.
struct SceneModel<'a> {
    app: &'a ClideApp,
    target: u64,
    clay: Option<ClayModel<'a>>,
    others: RefCell<BTreeMap<u64, Option<ClayModel<'a>>>>,
}

impl<'a> SceneModel<'a> {
    fn new(app: &'a ClideApp, target: u64) -> SceneModel<'a> {
        let clay = app.scene.get(target).and_then(|o| o.clay.as_ref()).and_then(|c| ClayModel::new(c, app.scene.world_matrix(target)));
        SceneModel { app, target, clay, others: RefCell::new(BTreeMap::new()) }
    }

    fn object_extent(&self, id: u64, axis: Axis) -> Result<(f32, f32), String> {
        let o = self.app.scene.get(id).ok_or("object vanished")?;
        if let Some(c) = &o.clay {
            let mut others = self.others.borrow_mut();
            let m = others.entry(id).or_insert_with(|| ClayModel::new(c, self.app.scene.world_matrix(id)));
            if let Some(m) = m {
                return m.total(axis).map_err(|e| format!("{}: {e}", o.name));
            }
        }
        mesh_extent(self.app, id, axis).ok_or_else(|| format!("{} has no mesh to measure", o.name))
    }
}

impl Model for SceneModel<'_> {
    fn extent(&self, name: &str, axis: Axis) -> Result<(f32, f32), String> {
        if let Some(e) = self.clay.as_ref().and_then(|c| c.blob_extent(name, axis)) {
            return e;
        }
        let found = self.app.scene.geometry().find(|o| o.name == name).map(|o| o.id);
        match found {
            Some(id) => self.object_extent(id, axis),
            None => Err(format!("no blob or object called {name}")),
        }
    }

    fn total(&self, axis: Axis) -> Result<(f32, f32), String> {
        match &self.clay {
            Some(c) => c.total(axis),
            None => self.object_extent(self.target, axis),
        }
    }

    fn named(&self) -> (Vec<String>, usize) {
        self.clay.as_ref().map(ClayModel::named).unwrap_or_default()
    }

    fn section(&self, y: f32, axis: Axis) -> Option<(f32, f32)> {
        self.clay.as_ref().and_then(|c| c.section(y, axis))
    }
}

impl ClideApp {
    /// `guides` tool (run at rest).
    pub(super) fn guides_tool(&mut self, args: &Value) -> ToolResult {
        match args["action"].as_str() {
            Some("set") => self.guides_set(args),
            Some("clear") => {
                known_keys(args, &CLEAR_KEYS, "guides clear")?;
                let parts: Vec<String> = match args.get("parts").filter(|v| !v.is_null()) {
                    None => PARTS.map(String::from).to_vec(),
                    Some(v) => serde_json::from_value::<Vec<String>>(v.clone()).map_err(|_| "parts must be a list of head_units, lines or targets")?,
                };
                if let Some(p) = parts.iter().find(|p| !PARTS.contains(&p.as_str())) {
                    return Err(format!("unknown part {p}; use head_units, lines or targets"));
                }
                let g = &mut self.scene.guides;
                for p in &parts {
                    match p.as_str() {
                        "head_units" => g.head_units = None,
                        "lines" => g.lines.clear(),
                        _ => g.targets.clear(),
                    }
                }
                Ok(text(json!({ "cleared": parts, "guides": self.scene.guides })))
            }
            Some("get") => {
                known_keys(args, &["action"], "guides get")?;
                Ok(text(json!({ "guides": self.scene.guides, "shown": self.show_guides })))
            }
            Some("measure") => {
                known_keys(args, &MEASURE_KEYS, "guides measure")?;
                let id = self.resolve(args.get("target").filter(|v| !v.is_null()).ok_or("measure needs target: the object to measure (its blob names resolve on it)")?)?;
                let m = self.measure_guides(id)?;
                let mut reply = m.reply;
                reply["target"] = json!(id);
                reply["name"] = json!(self.scene.get(id).map(|o| o.name.clone()));
                reply["guides"] = json!(self.scene.guides);
                Ok(text(reply))
            }
            Some(other) => Err(format!("unknown action {other}; use set, clear, measure or get")),
            None => Err("action is required: set, clear, measure or get".into()),
        }
    }

    /// `guides set`: each part given replaces that part; the others stay.
    fn guides_set(&mut self, args: &Value) -> ToolResult {
        known_keys(args, &SET_KEYS, "guides set")?;
        let given = |k: &str| args.get(k).is_some();
        if !["head_units", "lines", "targets"].iter().any(|k| given(k)) {
            return Err("set needs head_units, lines or targets".into());
        }
        let mut g: Guides = self.scene.guides.clone();
        if let Some(h) = args.get("head_units") {
            g.head_units = match h {
                Value::Null => None,
                _ => {
                    known_keys(h, &["count", "bottom", "top"], "head_units")?;
                    let count = h["count"].as_u64().filter(|c| (guides::MIN_UNITS as u64..=guides::MAX_UNITS as u64).contains(c)).ok_or(format!("head_units.count must be a whole number from {} to {}", guides::MIN_UNITS, guides::MAX_UNITS))? as u32;
                    let num = |k: &str| -> Result<Option<f32>, String> {
                        match h.get(k).filter(|v| !v.is_null()) {
                            None => Ok(None),
                            Some(v) => v.as_f64().map(|x| Some(x as f32)).ok_or(format!("head_units.{k} must be a number")),
                        }
                    };
                    let (bottom, top) = (num("bottom")?, num("top")?);
                    // Missing ends come from the target's height (stored
                    // as numbers, so the guides never move with the model).
                    let (bottom, top) = match (bottom, top) {
                        (Some(b), Some(t)) => (b, t),
                        _ => {
                            let target = args.get("target").filter(|v| !v.is_null()).ok_or("head_units without bottom and top takes them from target: name the object")?;
                            let id = self.resolve(target)?;
                            let (lo, hi) = self.model_height(id)?;
                            // To the reply's 1e-5, so the stored ends read as measured.
                            let r = |x: f32| guides::round(x) as f32;
                            (bottom.unwrap_or(r(lo)), top.unwrap_or(r(hi)))
                        }
                    };
                    Some(HeadUnits { count, bottom, top })
                }
            };
        }
        if let Some(v) = args.get("lines") {
            g.lines = if v.is_null() { Vec::new() } else { list::<Line>(v, "lines", guides::MAX_LINES, &["name", "x", "y", "z"])? };
        }
        if let Some(v) = args.get("targets") {
            let mut targets: Vec<Target> = if v.is_null() { Vec::new() } else { list(v, "targets", guides::MAX_TARGETS, &["name", "of", "to", "between", "axis", "per", "per_axis", "value", "tolerance"])? };
            // Resolved defaults are stored: a ratio's `to` is total_height,
            // and a part `per` is taken along y (its height), written out so
            // a saved file never reads as one from before per_axis
            // (`Guides::migrate`).
            for t in &mut targets {
                if !t.of.is_empty() && t.to.is_none() {
                    t.to = Some(guides::TOTAL_HEIGHT.to_string());
                }
                if t.per_axis.is_none() && t.per.as_deref().is_some_and(guides::per_is_part) {
                    t.per_axis = Some(guides::Axis::Y);
                }
            }
            g.targets = targets;
        }
        g.check()?;
        let changed = g != self.scene.guides;
        self.scene.guides = g;
        Ok(text(json!({
            "guides": self.scene.guides,
            "changed": changed,
            "note": "guides measure {target} measures the targets; render_views guides: true draws the lines; set_display guides shows them in the viewport (on by default)",
        })))
    }

    /// The target and every clay a target names: their composed fields
    /// are read, so their ledgers settle first (§0.2 rule 1).
    fn settle_measured(&mut self, id: u64) -> Result<(), String> {
        let g = &self.scene.guides;
        let names: Vec<&str> = g
            .targets
            .iter()
            .flat_map(|t| t.of.iter().map(String::as_str).chain(t.to.as_deref()).chain(t.per.as_deref()).chain(t.between.iter().filter_map(|e| match e {
                guides::End::Name(n) => Some(n.as_str()),
                guides::End::At(_) => None,
            })))
            .collect();
        let ids: Vec<u64> = std::iter::once(id).chain(self.scene.geometry().filter(|o| o.clay.is_some() && names.contains(&o.name.as_str())).map(|o| o.id)).collect();
        self.settle_clays(ids)
    }

    /// The target's height at rest (its clay field, else its mesh).
    fn model_height(&mut self, id: u64) -> Result<(f32, f32), String> {
        if self.scene.get(id).is_some_and(|o| o.is_reference()) {
            return Err("a reference image plane is not a model to measure".into());
        }
        self.settle_clays([id])?;
        self.meshes.sync(&self.scene);
        let keep = self.scene.frame.take();
        let out = SceneModel::new(self, id).total(Axis::Y);
        self.scene.frame = keep;
        out
    }

    /// Measures the scene's guides on `id` at rest (and against a loaded
    /// turnaround sheet). Its field is read: its ledger settles first.
    pub(super) fn measure_guides(&mut self, id: u64) -> Result<Measured, String> {
        if self.scene.get(id).is_some_and(|o| o.is_reference()) {
            return Err("a reference image plane is not a model to measure".into());
        }
        self.settle_measured(id)?;
        self.meshes.sync(&self.scene);
        let keep = self.scene.frame.take();
        let out = {
            let model = SceneModel::new(self, id);
            let fits: Vec<(Slot, &super::eyes::Reference)> = self.references.loaded().into_iter().filter_map(|s| self.references.get(s).filter(|r| r.sheet.is_some()).map(|r| (s, r))).collect();
            let widths: Vec<Box<dyn Fn(f32) -> Option<f32> + '_>> = fits
                .iter()
                .map(|(_, r)| {
                    let f = r.sheet.clone().expect("a sheet's view");
                    let [w, h] = r.image.size;
                    let mask = r.mask();
                    Box::new(move |y: f32| -> Option<f32> {
                        let row = (f.ground_row - (y - f.ground_y) * f.px_per_unit).floor();
                        if !(0.0..h as f32).contains(&row) {
                            return None;
                        }
                        let line = &mask[row as usize * w..][..w];
                        let first = line.iter().position(|m| *m)?;
                        let last = line.iter().rposition(|m| *m)?;
                        Some((last + 1 - first) as f32 / f.px_per_unit)
                    }) as Box<dyn Fn(f32) -> Option<f32> + '_>
                })
                .collect();
            let views: Vec<SheetView> = fits
                .iter()
                .zip(&widths)
                .map(|((slot, r), width)| {
                    let f = r.sheet.as_ref().expect("a sheet's view");
                    let b = r.bbox();
                    let y_of = |row: f32| f.ground_y + (f.ground_row - row) / f.px_per_unit;
                    SheetView {
                        view: slot.name().to_string(),
                        across: if matches!(slot, Slot::Left | Slot::Right) { Axis::Z } else { Axis::X },
                        top: y_of(b[1] as f32),
                        bottom: y_of((b[3] + 1) as f32),
                        width_at: width.as_ref(),
                    }
                })
                .collect();
            if self.scene.get(id).is_some_and(|o| o.clay.is_some()) && model.clay.is_none() {
                Err("the clay is empty".to_string())
            } else {
                Ok(guides::measure(&self.scene.guides, &model, &views))
            }
        };
        self.scene.frame = keep;
        out
    }

    /// The guides as `quality_report` reports them on `id`: None when no
    /// guides are set (or the call says `proportions: false`).
    pub(super) fn proportions(&mut self, id: u64, args: &Value) -> Result<Option<Measured>, String> {
        let on = match args.get("proportions").filter(|v| !v.is_null()) {
            None => true,
            Some(v) => v.as_bool().ok_or("proportions must be true or false")?,
        };
        if !on || self.scene.guides.is_empty() {
            return Ok(None);
        }
        self.measure_guides(id).map(Some)
    }

    /// The guide lines on an off-screen render (`render_views guides`):
    /// through the framed middle, as seen from the shot.
    pub(super) fn draw_guides_on(&self, img: &mut ColorImage, view_proj: Mat4, forward: Vec3, center: Vec3, radius: f32) {
        let [w, h] = img.size;
        let to_px = |p: Vec3| {
            let c = view_proj * p.extend(1.0);
            (c.w > 1e-6).then(|| {
                let n = c.truncate() / c.w;
                ((n.x * 0.5 + 0.5) * w as f32, (0.5 - n.y * 0.5) * h as f32)
            })
        };
        for s in guides::segments(&self.scene.guides, center, radius, forward) {
            let (Some(a), Some(b)) = (to_px(s.a), to_px(s.b)) else { continue };
            let (color, width) = match s.kind {
                Kind::Edge => (UNIT, 1.6),
                Kind::Unit => (UNIT, 1.0),
                Kind::Named => (NAMED, 1.0),
            };
            super::anim::line(img, a, b, width, color);
        }
    }

    /// The guide lines over a viewport pane, labelled (when shown:
    /// `set_display guides`).
    pub(super) fn paint_guides(&self, painter: &egui::Painter, rect: egui::Rect, view_proj: Mat4) {
        if !self.show_guides || self.scene.guides.head_units.is_none() && self.scene.guides.lines.is_empty() {
            return;
        }
        let inv = view_proj.inverse();
        let forward = (inv.project_point3(Vec3::new(0.0, 0.0, 1.0)) - inv.project_point3(Vec3::new(0.0, 0.0, 0.0))).normalize_or_zero();
        // The scene's sphere walks every mesh: once a frame, not per pane.
        let pass = painter.ctx().cumulative_pass_nr();
        let (center, radius) = match self.guides_sphere.get() {
            Some((p, c, r)) if p == pass => (c, r),
            _ => {
                let (c, r) = self.scene_sphere();
                self.guides_sphere.set(Some((pass, c, r)));
                (c, r)
            }
        };
        let to_px = |p: Vec3| {
            let c = view_proj * p.extend(1.0);
            (c.w > 1e-6).then(|| {
                let n = c.truncate() / c.w;
                egui::pos2(rect.left() + (n.x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - n.y * 0.5) * rect.height())
            })
        };
        for s in guides::segments(&self.scene.guides, center, radius, forward) {
            let (Some(a), Some(b)) = (to_px(s.a), to_px(s.b)) else { continue };
            let (color, width) = match s.kind {
                Kind::Edge => (UNIT, 1.6),
                Kind::Unit => (UNIT.gamma_multiply(0.7), 1.0),
                Kind::Named => (NAMED, 1.2),
            };
            painter.line_segment([a, b], egui::Stroke::new(width, color));
            painter.text(b + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, &s.label, egui::FontId::proportional(11.0), color);
        }
    }

    /// Display > Measure Proportions: the request it sends (the selection
    /// as the target).
    pub(super) fn measure_request(&self) -> Option<Value> {
        let id = *self.selection.first()?;
        (!self.scene.guides.is_empty() && self.scene.get(id).is_some_and(|o| !o.is_reference() && o.shape != Shape::Group)).then(|| json!({ "action": "measure", "target": id }))
    }

    /// The Proportions window: the last Measure Proportions reply, target
    /// by target.
    pub(super) fn proportions_window(&mut self, ctx: &egui::Context) {
        let Some(job) = self.guides_job else { return };
        let mut open = true;
        let result = self.jobs.result(job).cloned();
        egui::Window::new("Proportions").open(&mut open).resizable(true).default_width(320.0).show(ctx, |ui| match &result {
            None => {
                ui.label("Measuring…");
            }
            Some(Err(e)) => {
                ui.colored_label(Color32::from_rgb(230, 90, 80), e);
            }
            Some(Ok(parts)) => {
                let reply: Value = parts.first().and_then(|p| p["text"].as_str()).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
                if let Some(h) = reply["model"]["height"].as_f64() {
                    ui.label(format!("{}: {h:.4} tall", reply["name"].as_str().unwrap_or("model")));
                }
                if let Some(heads) = reply["head_units"]["model_heads"].as_f64() {
                    ui.label(format!("{heads:.3} head units"));
                }
                ui.separator();
                for t in reply["targets"].as_array().into_iter().flatten() {
                    let pass = t["pass"].as_bool().unwrap_or(false);
                    let color = if pass { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(230, 90, 80) };
                    let what = match t["measured"].as_f64() {
                        Some(m) => format!("{}: {m:.4} (target {} ± {})", t["name"].as_str().unwrap_or(""), t["value"], t["tolerance"]),
                        None => format!("{}: {}", t["name"].as_str().unwrap_or(""), t["problem"].as_str().unwrap_or("")),
                    };
                    ui.colored_label(color, what);
                }
            }
        });
        if !open {
            self.guides_job = None;
        }
    }
}
