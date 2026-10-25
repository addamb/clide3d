//! The brush tools (Sculpt, Paint, Mask and Groups): the mouse is a stroke
//! recorder (Stages 1c and 7 of docs/SCULPT_PLAN.md). A drag on a clay
//! records screen samples (`brush::Trail`: LazyMouse, pen pressure) and a
//! worker previews the stroke so far (`brush::Live`), but nothing in the
//! scene changes while it runs. On release the samples become a `sculpt`
//! (`paint`, `mask` or `group`) request, the same JSON an agent sends: a
//! screen frame with the camera stored, so the request means the same
//! thing after the human orbits. The request joins the line of MCP calls
//! and runs through the same handler (`process_mcp`), so snapping,
//! validation, refusals, the install guard, history labels and the reply
//! are the agent's. It is kept in `ui_activity`, so an agent sees what the
//! human did and can send the request again verbatim.
//!
//! Modifiers pick what a Sculpt drag sends (§4.3): Shift smooths, Alt
//! inverts, Ctrl paints the mask (Ctrl+Alt erases it) and Ctrl+Shift
//! draws a mask lasso. Grab and Snakehook drag across the view plane at
//! the press's depth and send world points. A posed clay takes posable
//! strokes (points on the posed clay, which the handler takes back to the
//! rest pose).
//!
//! The brush (`CurrentBrush`) is editor state, read and set by agents with
//! `brush_preset get_current|set_current`; the shelf, the HUD and the
//! brush hotkeys call the same handler. `brush_preset` also lists, reads,
//! saves and deletes the presets (`brush::preset`); a preset picked as the
//! current brush goes into the request (`preset`), under the stroke's own
//! settings.

use std::sync::mpsc;

use egui::{Color32, Modifiers, Pos2, Rect, RichText, Stroke, Ui};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::brush::{self, Input, Lazy, Live, Sample, Trail};
use super::icons::{self, Icon};
use super::{ClideApp, Tool, theme};
use crate::brush::curve::Falloff;
use crate::brush::pressure::PressureCurves;
use crate::brush::spec::{Brush, Camera, Frame, Screen};
use crate::brush::symmetry::{Axis, Symmetry};
use crate::camera::ViewKind;
use crate::ledger::{PressureSource, Source};
use crate::mcp::{ToolCall, ToolResult};
use crate::scene::Shape;

/// Sculpt brushes the tool offers: the ones a drag over the surface
/// gives (grab and snakehook take the drag across the view plane; noise
/// needs a pattern, stretch a delta per point).
pub(super) const UI_BRUSHES: [Brush; 19] = [
    Brush::Standard,
    Brush::Clay,
    Brush::ClayBuildup,
    Brush::ClayTubes,
    Brush::TrimDynamic,
    Brush::Polish,
    Brush::Add,
    Brush::Carve,
    Brush::Inflate,
    Brush::Deflate,
    Brush::Smooth,
    Brush::Flatten,
    Brush::Crease,
    Brush::Pinch,
    Brush::Layer,
    Brush::Grab,
    Brush::Snakehook,
    // Stamps the alpha (a VDM stamp) along the drag.
    Brush::Vdm,
    // Sent to the mask tool.
    Brush::MaskPaint,
];
/// Requests kept in `ui_activity`, and how many replies show.
const KEEP_ACTIVITY: usize = 32;
const SHOW_ACTIVITY: usize = 5;
/// Samples closer than this many points to the last one are skipped.
const MIN_STEP_PX: f32 = 3.0;

/// The brush tools' brush.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CurrentBrush {
    pub brush: Brush,
    /// World units, like an agent's strokes.
    pub radius: f32,
    pub strength: f32,
    pub invert: bool,
    /// The Paint tool's color (sRGB 0..1).
    pub color: [f32; 3],
    pub symmetry: Option<Symmetry>,
    /// A preset the strokes go on top of (its falloff, alpha, pattern ...).
    pub preset: Option<String>,
    /// The falloff curve and focal shift (None: the brush's own).
    pub falloff: Option<Falloff>,
    /// An alpha from the scene's library, by name (dab brushes).
    pub alpha: Option<String>,
    /// How pen pressure scales size and strength.
    pub pressure: PressureCurves,
    /// LazyMouse (None: off).
    pub lazy: Option<Lazy>,
    /// Pressure from the pointer's speed when no pen gives any.
    pub velocity: bool,
    /// The macOS tablet monitor (`pen_macos`).
    pub tablet: bool,
    /// The polygroup the Groups tool paints.
    pub group: u16,
}

impl Default for CurrentBrush {
    fn default() -> Self {
        Self { brush: Brush::Standard, radius: 0.05, strength: 0.5, invert: false, color: [0.75, 0.2, 0.15], symmetry: None, preset: None, falloff: None, alpha: None, pressure: PressureCurves::default(), lazy: None, velocity: false, tablet: false, group: 1 }
    }
}

/// What a drag sends, chosen at the press from the tool, the brush and
/// the modifiers (§4.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Drag {
    /// A sculpt stroke with the brush (Shift: smooth; Alt: inverted).
    Sculpt { smooth: bool, invert: bool },
    Paint { erase: bool },
    /// A mask stroke (Ctrl, or the Mask tool; Alt erases).
    MaskPaint { erase: bool },
    /// A mask lasso (Ctrl+Shift, or Shift with the Mask tool; Alt takes away).
    Lasso { sub: bool },
    /// A polygroup stroke (the Groups tool).
    Group,
    /// Grab and Snakehook: the drag across the view plane at the press.
    Grab,
    Snakehook,
}

impl Drag {
    /// The drag for a press with `m` held.
    pub fn of(tool: Tool, brush: &CurrentBrush, m: Modifiers) -> Drag {
        let ctrl = m.ctrl || m.command;
        match tool {
            Tool::Paint => Drag::Paint { erase: m.alt },
            Tool::Mask if m.shift => Drag::Lasso { sub: m.alt },
            Tool::Mask => Drag::MaskPaint { erase: m.alt },
            Tool::Groups => Drag::Group,
            _ if ctrl && m.shift => Drag::Lasso { sub: m.alt },
            _ if ctrl => Drag::MaskPaint { erase: m.alt },
            _ if m.shift => Drag::Sculpt { smooth: true, invert: false },
            _ if brush.brush == Brush::MaskPaint => Drag::MaskPaint { erase: brush.invert ^ m.alt },
            _ if brush.brush == Brush::Grab => Drag::Grab,
            _ if brush.brush == Brush::Snakehook => Drag::Snakehook,
            _ => Drag::Sculpt { smooth: false, invert: brush.invert ^ m.alt },
        }
    }

    /// The tool its request goes to.
    pub fn tool(self) -> &'static str {
        match self {
            Drag::Sculpt { .. } | Drag::Grab | Drag::Snakehook => "sculpt",
            Drag::Paint { .. } => "paint",
            Drag::MaskPaint { .. } | Drag::Lasso { .. } => "mask",
            Drag::Group => "group",
        }
    }

    /// Whether its samples are screen pixels on the clay.
    fn screen(self) -> bool {
        !matches!(self, Drag::Grab | Drag::Snakehook)
    }
}

/// A clay shown away from its rest pose at the press.
#[derive(Clone, Copy)]
pub(super) struct Posed {
    /// Its rig is posed (posable strokes) at this frame; else it hangs
    /// from an animated bone (its points go back through its placement).
    pub rigged: Option<Option<f32>>,
    /// Its placement as shown, and at rest.
    pub shown: Mat4,
    pub rest: Mat4,
}

/// A stroke being drawn, or drawn and waiting in `ClideApp::strokes` to
/// be sent.
pub(super) struct Capture {
    pub view: ViewKind,
    /// The clay pressed on.
    pub target: u64,
    /// `ClideApp::scene_generation` at the press: ids are reused across
    /// new/open/undo, so a stroke from an older generation is dropped.
    pub generation: u64,
    pub drag: Drag,
    /// The view at the press: its matrices and size in pixels.
    pub camera: Camera,
    /// The view's projection at aspect 1: with `camera.view`, what a real
    /// camera move changes and a resized pane does not.
    pub lens: Mat4,
    /// Samples in the press's pixels (see `sample`), LazyMouse applied.
    pub trail: Trail,
    /// The brush at the press: one changed while the stroke waits does not
    /// change it.
    pub brush: CurrentBrush,
    /// The pixels per point at the press (LazyMouse is in points).
    pub ppp: f32,
    pub pen: brush::Pen,
    /// Where the samples' pressure came from.
    pub pressure_source: Option<PressureSource>,
    /// Grab and Snakehook: the point pressed on (world) and the path the
    /// drag takes across the view plane through it.
    pub anchor: Option<Vec3>,
    pub path: Vec<Vec3>,
    pub posed: Option<Posed>,
    /// The live preview (None: none for this stroke).
    pub live: Option<Live>,
}

impl Capture {
    /// Adds the pointer at `pos` (points in a pane at `rect`, seen through
    /// `now`). The pane may have changed size since the press (the jobs
    /// panel comes and goes) while the camera did not: the sample is the
    /// press pixel on the same ray.
    pub fn sample(&mut self, pos: Pos2, rect: Rect, now: &Camera, time: f64) {
        let ppp = self.ppp;
        let Some(p) = reproject(now, [(pos.x - rect.min.x) * ppp, (pos.y - rect.min.y) * ppp], &self.camera) else { return };
        let (pressure, source) = self.pen.pressure(p, time, self.brush.velocity);
        if source.is_some() {
            self.pressure_source = source;
        }
        // A lasso is the outline as drawn; LazyMouse smooths strokes.
        let lazy = self.brush.lazy.filter(|_| !matches!(self.drag, Drag::Lasso { .. }));
        let (radius, step, back) = lazy.map_or((0.0, MIN_STEP_PX, false), |l| (l.radius, l.step.max(MIN_STEP_PX), l.backtrack));
        self.trail.push(p, pressure, radius * ppp, step * ppp, back);
        // Grab and Snakehook: where the ray meets the view plane at the
        // press's depth.
        if let Some(a) = self.anchor {
            let (o, d) = self.camera.ray(p[0], p[1]);
            let n = self.camera.view.inverse().transform_vector3(Vec3::NEG_Z).normalize_or_zero();
            let denom = d.dot(n);
            if denom.abs() > 1e-6 {
                let q = o + d * ((a - o).dot(n) / denom);
                if q.is_finite() && self.path.last().is_none_or(|l| l.distance(q) > 1e-6) {
                    self.path.push(q);
                }
            }
        }
    }

    /// The samples as recorded (pixels).
    pub fn px(&self) -> Vec<[f32; 2]> {
        self.trail.samples.iter().map(|s| s.px).collect()
    }

    /// What the preview worker takes now.
    fn input(&self, app: &ClideApp) -> Option<Input> {
        if self.drag.screen() {
            return (!self.trail.samples.is_empty()).then(|| Input::Samples(self.trail.samples.clone()));
        }
        app.view_plane_stroke(self).map(Input::Stroke)
    }
}

/// The pixel in `to` on the ray through pixel `p` of `from`. The two must
/// share the eye and view direction (one view, two pane sizes): then any
/// point on the ray lands on the same pixel. None behind the eye.
pub(super) fn reproject(from: &Camera, p: [f32; 2], to: &Camera) -> Option<[f32; 2]> {
    let (o, d) = from.ray(p[0], p[1]);
    let c = to.proj * to.view * (o + d).extend(1.0);
    let [w, h] = to.viewport;
    (c.w > 1e-6).then(|| [(c.x / c.w + 1.0) * 0.5 * w, (1.0 - c.y / c.w) * 0.5 * h]).filter(|q| q.iter().all(|v| v.is_finite()))
}

/// A tool request the UI sent.
pub(super) struct UiActivity {
    pub tool: &'static str,
    pub request: Value,
    /// The call's job id (`job_status` reads its result too).
    pub job: u64,
    pub time: u64,
    /// The status line told the human how it went.
    pub reported: bool,
}

pub(super) fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Select => "select",
        Tool::Move => "move",
        Tool::Rotate => "rotate",
        Tool::Scale => "scale",
        Tool::Sculpt => "sculpt",
        Tool::Paint => "paint",
        Tool::Mask => "mask",
        Tool::Groups => "groups",
    }
}

/// At most `max` (2 or more) of `px`, evenly, keeping both ends.
pub(super) fn thin<T: Copy>(px: &[T], max: usize) -> Vec<T> {
    let n = px.len();
    if n <= max {
        return px.to_vec();
    }
    (0..max).map(|i| px[i * (n - 1) / (max - 1)]).collect()
}

/// The samples a stroke keeps: thinned to `MAX_POINTS` (before the casts,
/// each of which walks the clay's tree), then those whose ray through
/// `camera` hits the clay (the handler refuses a stroke for one miss).
/// The editor's request and its preview both keep them this way.
pub(super) fn kept(samples: &[Sample], camera: &Camera, to_local: Mat4, cast: &dyn Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>) -> Vec<Sample> {
    thin(samples, crate::brush::resolve::MAX_POINTS)
        .into_iter()
        .filter(|s| {
            let (o, d) = camera.ray(s.px[0], s.px[1]);
            cast(to_local.transform_point3(o), to_local.transform_vector3(d).normalize_or_zero()).is_some()
        })
        .collect()
}

/// A reply's JSON text, parsed.
pub(super) fn reply_json(content: &[Value]) -> Value {
    content.first().and_then(|t| t["text"].as_str()).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null)
}

/// What a drag's request is besides its points: the tool, the stroke's
/// brush settings, and how the stroke sits in the request.
#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub tool: &'static str,
    pub target: u64,
    /// The stroke without its points or frame.
    pub stroke: Value,
    /// The layer named in the request (sculpt and paint).
    pub layer: Option<String>,
    /// The Groups tool's id.
    pub group: Option<u16>,
}

impl Plan {
    /// The request with `points` (a screen frame's px, or world points)
    /// filled into its stroke.
    pub fn request(&self, fill: impl FnOnce(&mut Value)) -> Value {
        let mut stroke = self.stroke.clone();
        fill(&mut stroke);
        let mut r = match self.tool {
            "mask" => json!({ "target": self.target, "actions": [{ "paint": [stroke] }] }),
            "group" => json!({ "target": self.target, "actions": [{ "paint": { "id": self.group.unwrap_or(1), "strokes": [stroke] } }] }),
            _ => json!({ "target": self.target, "strokes": [stroke] }),
        };
        if let Some(l) = &self.layer {
            r["layer"] = json!(l);
        }
        r
    }

    /// The stroke on its own, with a screen frame through `camera`.
    pub fn screen_stroke(&self, camera: &Camera, px: &[Sample]) -> Value {
        let mut stroke = self.stroke.clone();
        stroke["frame"] = json!(Frame::Screen(Box::new(Screen { view: None, camera: Some(*camera), at: Vec::new(), px: px.iter().map(Sample::json).collect() })));
        stroke
    }

    /// The layer the handler writes when the request names none.
    fn layer_or_default(&self, clay: &crate::clay::Clay) -> String {
        match self.tool {
            "paint" => self.layer.clone().unwrap_or_else(|| crate::layers::PAINT.into()),
            "mask" => "mask".into(),
            "group" => "groups".into(),
            _ => self.layer.clone().unwrap_or_else(|| crate::layers::default_sculpt_layer(clay)),
        }
    }

    /// The brush the handler fills in for strokes without one.
    fn default_brush(&self) -> Option<Brush> {
        match self.tool {
            "paint" => Some(Brush::Paint),
            "mask" => Some(Brush::MaskPaint),
            "group" => Some(Brush::GroupPaint),
            _ => None,
        }
    }
}

impl ClideApp {
    /// True while the tool draws strokes instead of picking.
    pub(super) fn stroke_tool(&self) -> bool {
        matches!(self.tool, Tool::Sculpt | Tool::Paint | Tool::Mask | Tool::Groups)
    }

    /// `brush_preset`: the presets (built in and the scene's, which are
    /// scene edits) and the current brush (UI state; never an undo step).
    pub(super) fn brush_preset(&mut self, args: &Value) -> ToolResult {
        use crate::brush::preset::{self, Preset};
        match args["action"].as_str() {
            Some("get_current") => {}
            Some("set_current") => self.set_current_brush(args)?,
            Some("list") => {
                let mut list: Vec<Value> = preset::builtin().iter().filter(|(n, _)| !self.scene.brushes.contains_key(*n)).map(|(n, p)| p.summary(n, true)).collect();
                list.extend(self.scene.brushes.iter().map(|(n, p)| p.summary(n, false)));
                return Ok(super::tools::text(json!({ "brushes": list, "current": self.brush.preset })));
            }
            Some("get") => {
                let name = args["name"].as_str().ok_or("get needs name")?;
                let p = preset::find(&self.scene.brushes, name).ok_or_else(|| format!("no brush preset {name}"))?;
                return Ok(super::tools::text(json!({ "name": name, "builtin": !self.scene.brushes.contains_key(name), "spec": p.fields() })));
            }
            Some("save") => {
                let name = args["name"].as_str().map(str::trim).ok_or("save needs name")?;
                if !preset::name_ok(name) {
                    return Err(format!("preset names are 1 to {} characters", preset::MAX_NAME));
                }
                if preset::builtin().contains_key(name) {
                    return Err(format!("{name} is a built-in preset; save under another name"));
                }
                if !self.scene.brushes.contains_key(name) && self.scene.brushes.len() >= preset::MAX_PRESETS {
                    return Err(format!("a scene saves at most {} presets; delete one first", preset::MAX_PRESETS));
                }
                let p = match (args.get("spec").filter(|v| !v.is_null()), args.get("from_record").filter(|v| !v.is_null())) {
                    (Some(spec), None) => Preset::try_from(spec.clone())?,
                    (None, Some(from)) => {
                        let (id, clay) = self.clay_of(from)?;
                        let rid = from["id"].as_u64().ok_or("from_record needs id: the record (stroke_log)")?;
                        let spec = match from["layer"].as_str().unwrap_or("base") {
                            "base" => crate::ledger::ledger::<crate::clay::Detail>(clay).records().iter().find(|r| r.id == rid).and_then(|r| r.stroke().map(|(s, _)| s.clone())),
                            "paint" => crate::ledger::ledger::<crate::paint::Paint>(clay).records().iter().find(|r| r.id == rid).and_then(|r| r.stroke().map(|(s, _)| s.clone())),
                            other => return Err(format!("no layer {other}; a clay has base and paint")),
                        };
                        let spec = spec.ok_or_else(|| format!("no stroke record {rid} on object {id}"))?;
                        Preset::from_spec(&spec)?
                    }
                    _ => return Err("save takes spec (stroke settings: brush, strength, falloff, alpha ...) or from_record {target, layer, id}".into()),
                };
                let fields = p.fields().clone();
                self.scene.brushes.insert(name.to_string(), p);
                return Ok(super::tools::text(json!({ "saved": name, "spec": fields, "step": self.history.current() + 1 })));
            }
            Some("delete") => {
                let name = args["name"].as_str().ok_or("delete needs name")?;
                if self.scene.brushes.remove(name).is_none() {
                    return Err(if preset::builtin().contains_key(name) { format!("{name} is built in and cannot be deleted") } else { format!("no saved preset {name}") });
                }
                if self.brush.preset.as_deref() == Some(name) {
                    self.brush.preset = None;
                }
                return Ok(super::tools::text(json!({ "deleted": name, "step": self.history.current() + 1 })));
            }
            _ => return Err("action must be list, get, save, delete, get_current or set_current".into()),
        }
        Ok(super::tools::text(self.current_brush_json()))
    }

    pub(super) fn current_brush_json(&self) -> Value {
        let b = &self.brush;
        json!({ "tool": tool_name(self.tool), "brush": b.brush, "radius": b.radius, "strength": b.strength, "invert": b.invert, "color": b.color, "symmetry": b.symmetry, "preset": b.preset, "falloff": b.falloff, "alpha": b.alpha, "pressure": b.pressure, "lazy": b.lazy, "velocity": b.velocity, "tablet": b.tablet, "group": b.group })
    }

    /// The settings of the current brush as a preset would hold them
    /// (`brush_preset save {spec}`, the HUD's Save).
    pub(super) fn current_brush_spec(&self) -> Value {
        let b = &self.brush;
        let mut spec = json!({ "brush": b.brush, "radius": b.radius, "strength": b.strength });
        if b.invert {
            spec["invert"] = json!(true);
        }
        if let Some(f) = &b.falloff {
            spec["falloff"] = json!(f);
        }
        if !b.pressure.is_default() {
            spec["pressure"] = json!(b.pressure);
        }
        if let Some(a) = &b.alpha {
            spec["alpha"] = json!({ "name": a });
        }
        spec
    }

    /// Merges `set_current`'s fields; all or nothing.
    fn set_current_brush(&mut self, args: &Value) -> Result<(), String> {
        const KEYS: [&str; 16] = ["action", "tool", "brush", "radius", "strength", "invert", "color", "symmetry", "preset", "falloff", "alpha", "pressure", "lazy", "velocity", "tablet", "group"];
        if let Some(k) = args.as_object().into_iter().flat_map(|o| o.keys()).find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown field {k}; set_current takes {}", KEYS[1..].join(", ")));
        }
        let given = |k: &str| args.get(k).filter(|v| !v.is_null());
        let mut b = self.brush.clone();
        let tool = match given("tool").map(|t| t.as_str()) {
            None => None,
            Some(Some("sculpt")) => Some(Tool::Sculpt),
            Some(Some("paint")) => Some(Tool::Paint),
            Some(Some("mask")) => Some(Tool::Mask),
            Some(Some("groups")) => Some(Tool::Groups),
            Some(_) => return Err("tool must be sculpt, paint, mask or groups".into()),
        };
        // A preset sets the brush and its strength (the fields given with
        // it still win), and goes into every stroke the tool sends.
        match args.get("preset") {
            None => {}
            Some(Value::Null) => b.preset = None,
            Some(v) => {
                let name = v.as_str().ok_or("preset must be a preset's name or null")?;
                let p = crate::brush::preset::find(&self.scene.brushes, name).ok_or_else(|| format!("no brush preset {name}; brush_preset list shows them"))?;
                let brush: Brush = serde_json::from_value(p.fields()["brush"].clone()).map_err(|e| e.to_string())?;
                if !UI_BRUSHES.contains(&brush) {
                    return Err(format!("preset {name} ({}) needs more than a drag over the surface; agents use it in sculpt calls", json!(brush)));
                }
                b.brush = brush;
                if let Some(s) = p.fields().get("strength").and_then(Value::as_f64) {
                    b.strength = s as f32;
                }
                b.preset = Some(name.to_string());
            }
        }
        if let Some(v) = given("brush") {
            let brush: Brush = serde_json::from_value(v.clone()).map_err(|_| format!("unknown brush {v}"))?;
            if !UI_BRUSHES.contains(&brush) {
                let names: Vec<Value> = UI_BRUSHES.iter().map(|b| json!(b)).collect();
                return Err(format!("brush {v} needs more than a drag; the Sculpt tool takes {}", Value::from(names)));
            }
            b.brush = brush;
            // A brush picked on its own leaves the preset.
            if given("preset").is_none() {
                b.preset = None;
            }
        }
        let number = |k: &str| -> Result<Option<f32>, String> {
            given(k).map(|v| v.as_f64().map(|x| x as f32).filter(|x| x.is_finite()).ok_or(format!("{k} must be a number"))).transpose()
        };
        if let Some(r) = number("radius")? {
            if !(r > 0.0 && r <= 100.0) {
                return Err("radius must be above 0 and at most 100 (world units)".into());
            }
            b.radius = r;
        }
        if let Some(s) = number("strength")? {
            if !(-1.0..=1.0).contains(&s) {
                return Err("strength must be -1 to 1".into());
            }
            b.strength = s;
        }
        let flag = |k: &str| given(k).map(|v| v.as_bool().ok_or(format!("{k} must be true or false"))).transpose();
        if let Some(v) = flag("invert")? {
            b.invert = v;
        }
        if let Some(v) = given("color") {
            let c: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| "color must be [r, g, b]")?;
            if !c.iter().all(|x| (0.0..=1.0).contains(x)) {
                return Err("color channels must be 0 to 1".into());
            }
            b.color = c;
        }
        if let Some(v) = args.get("symmetry") {
            b.symmetry = match v {
                Value::Null => None,
                v => {
                    let s: Symmetry = serde_json::from_value(v.clone()).map_err(|e| format!("symmetry: {e}"))?;
                    s.validate()?;
                    if s.posable {
                        return Err("the brush tools make posable strokes themselves on a posed clay; leave posable out".into());
                    }
                    Some(s).filter(|s| !s.is_none())
                }
            };
        }
        if let Some(v) = args.get("falloff") {
            b.falloff = match v {
                Value::Null => None,
                v => Some(serde_json::from_value(v.clone()).map_err(|e| format!("falloff: {e}"))?),
            };
        }
        if let Some(v) = args.get("alpha") {
            b.alpha = match v {
                Value::Null => None,
                Value::String(n) if self.scene.alphas.contains_key(n) => Some(n.clone()),
                Value::String(n) => return Err(format!("no alpha named {n} in the scene; alpha import adds one")),
                _ => return Err("alpha must be an alpha's name (alpha list) or null".into()),
            };
        }
        if let Some(v) = args.get("pressure") {
            b.pressure = if v.is_null() { PressureCurves::default() } else { serde_json::from_value(v.clone()).map_err(|e| format!("pressure: {e}"))? };
        }
        if let Some(v) = args.get("lazy") {
            b.lazy = match v {
                Value::Null => None,
                v => {
                    let l: Lazy = serde_json::from_value(v.clone()).map_err(|e| format!("lazy: {e} (radius and step in points, snap, backtrack)"))?;
                    l.validate()?;
                    Some(l).filter(|l| l.radius > 0.0 || l.snap || l.backtrack)
                }
            };
        }
        if let Some(v) = flag("velocity")? {
            b.velocity = v;
        }
        if let Some(v) = given("group") {
            b.group = v.as_u64().filter(|g| (1..=u16::MAX as u64).contains(g)).ok_or("group must be a polygroup id, 1 to 65535")? as u16;
        }
        // A dab brush's alpha is of its kind (a VDM stamp for vdm, an image
        // for the others), so a drag is never refused for it: an alpha
        // given is refused, one kept from before a brush change is dropped.
        if let Some(n) = &b.alpha
            && b.brush.dabs()
            && self.scene.alphas.get(n).is_some_and(|a| a.vdm().is_some() != (b.brush == Brush::Vdm))
        {
            if given("alpha").is_some() {
                return Err(if b.brush == Brush::Vdm { format!("alpha {n} is an image; brush vdm stamps a VDM (alpha create_vdm makes one)") } else { format!("alpha {n} is a VDM stamp: use it with brush vdm") });
            }
            b.alpha = None;
        }
        if let Some(on) = flag("tablet")?
            && on != b.tablet
        {
            // Headless editors and other platforms say why it cannot run.
            if let Err(e) = super::pen_macos::set_enabled(on)
                && on
            {
                return Err(e);
            }
            b.tablet = on;
        }
        self.brush = b;
        if let Some(t) = tool {
            self.tool = t;
        }
        Ok(())
    }

    /// Starts a stroke at `start` if it is on a clay; false otherwise.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin_stroke(&mut self, kind: ViewKind, rect: Rect, start: Pos2, view_proj: Mat4, eval: &super::viewport::Evaluated, ppp: f32, m: Modifiers, time: f64, events: &[egui::Event]) -> bool {
        let Some((target, t, _)) = self.pick_surface(start, rect, view_proj, eval) else {
            self.status = "Press on a clay object to use the brush on it.".into();
            return false;
        };
        if self.scene.get(target).is_none_or(|o| o.shape != Shape::Clay) {
            self.status = "Only clay objects take brush strokes.".into();
            return false;
        }
        let drag = Drag::of(self.tool, &self.brush, m);
        // The press is picked on the clay the view shows, but the handler
        // strokes the rest pose (`at_rest`): a posed clay takes sculpt
        // strokes as points on the posed clay (posable), the rest waits.
        let posed = self.posed_now(target, eval).then(|| {
            let o = self.scene.get(target).expect("checked");
            let rigged = o.rig.as_ref().filter(|r| !r.bones.is_empty() && r.is_posed(self.scene.frame)).map(|_| self.scene.frame);
            let shown = eval.get(&target).map_or_else(|| self.scene.world_matrix(target), |e| e.0);
            let rest = crate::pose::at_rest(&mut self.scene, |s| s.world_matrix(target));
            Posed { rigged, shown, rest }
        });
        if posed.is_some() && !matches!(drag, Drag::Sculpt { .. }) {
            self.status = "This clay is posed or moved by animation at this frame: sculpt strokes take the pose, but paint, mask, groups, grab and snakehook work on the rest pose. Go to a frame where it is at rest (or turn its clip off).".into();
            return false;
        }
        let camera = self.stroke_camera(kind, rect, ppp);
        let (origin, dir) = Self::ray(start, rect, view_proj);
        let anchor = (!drag.screen()).then(|| origin + dir * t);
        let mut pen = brush::Pen::default();
        pen.read(events);
        let mut capture = Capture { view: kind, target, generation: self.scene_generation, drag, camera, lens: self.cams.proj(kind, 1.0), trail: Trail::default(), brush: self.brush.clone(), ppp, pen, pressure_source: None, anchor, path: anchor.into_iter().collect(), posed, live: None };
        capture.sample(start, rect, &camera, time);
        capture.live = self.start_preview(&capture);
        if let Some(input) = capture.input(self)
            && let Some(live) = capture.live.as_mut()
        {
            self.preview_seq += 1;
            live.send(self.preview_seq, input, false);
        }
        self.capture = Some(capture);
        true
    }

    /// Records the pointer at `pos` into the stroke being drawn and sends
    /// the stroke so far to its preview.
    pub(super) fn stroke_sample(&mut self, pos: Pos2, rect: Rect, now: &Camera, time: f64, events: &[egui::Event]) {
        let Some(mut c) = self.capture.take() else { return };
        c.pen.read(events);
        let before = (c.trail.samples.len(), c.trail.samples.last().copied(), c.path.len());
        c.sample(pos, rect, now, time);
        if (c.trail.samples.len(), c.trail.samples.last().copied(), c.path.len()) != before
            && let Some(input) = c.input(self)
            && let Some(live) = c.live.as_mut()
        {
            self.preview_seq += 1;
            live.send(self.preview_seq, input, false);
        }
        self.capture = Some(c);
    }

    /// True if the clay `id` is shown away from its rest pose at the
    /// timeline frame: its rig posed, or its world matrix moved (it hangs
    /// from an animated bone).
    pub(super) fn posed_now(&mut self, id: u64, eval: &super::viewport::Evaluated) -> bool {
        let frame = self.scene.frame;
        if frame.is_none() {
            return false;
        }
        let rest = Default::default();
        let rig = self.scene.get(id).and_then(|o| self.scene.rig_of(o)).filter(|r| !r.bones.is_empty());
        if rig.is_some_and(|r| r.is_posed(frame) && r.poses(frame).iter().any(|p| *p != rest)) {
            return true;
        }
        let shown = eval.get(&id).map_or_else(|| self.scene.world_matrix(id), |e| e.0);
        let at_rest = crate::pose::at_rest(&mut self.scene, |s| s.world_matrix(id));
        !shown.abs_diff_eq(at_rest, 1e-4 * (1.0 + at_rest.abs().to_cols_array().iter().fold(0.0f32, |a, b| a.max(*b))))
    }

    /// The view a stroke drawn now in this pane is stored with.
    pub(super) fn stroke_camera(&self, kind: ViewKind, rect: Rect, ppp: f32) -> Camera {
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
        Camera { view: self.cams.view(kind), proj: self.cams.proj(kind, aspect), viewport: [rect.width() * ppp, rect.height() * ppp] }
    }

    /// Drops the stroke being drawn in `kind` if its camera moved since the
    /// press (an agent's `set_camera` or `frame_view`, a Frame hotkey, the
    /// View menu): the press's camera could not replay later samples. A
    /// pane that only changed size or place (the jobs panel shown or
    /// hidden) keeps it: `sample` maps each pixel back to the press's.
    /// True if it was dropped.
    pub(super) fn drop_stroke_if_view_moved(&mut self, kind: ViewKind) -> bool {
        let Some(c) = self.capture.as_ref().filter(|c| c.view == kind) else { return false };
        if c.camera.view == self.cams.view(kind) && c.lens == self.cams.proj(kind, 1.0) {
            return false;
        }
        self.drop_capture();
        self.status = "The view moved during the stroke; it was dropped, nothing was sent.".into();
        true
    }

    /// Drops the stroke being drawn and its preview.
    pub(super) fn drop_capture(&mut self) {
        if let Some(c) = self.capture.take() {
            self.clear_preview_of(c.target);
        }
    }

    /// The button went up at `pos`: the stroke joins the released ones,
    /// sent in order when their turn comes. A new stroke may start
    /// meanwhile.
    pub(super) fn end_stroke(&mut self, ctx: &egui::Context, pos: Option<Pos2>, rect: Rect, now: &Camera, time: f64) {
        let Some(mut c) = self.capture.take() else { return };
        if let Some(p) = pos {
            c.sample(p, rect, now, time);
            let ppp = c.ppp;
            if let Some(at) = reproject(now, [(p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp], &c.camera) {
                let pressure = c.trail.samples.last().and_then(|s| s.pressure);
                let snap = c.brush.lazy.is_some_and(|l| l.snap) && !matches!(c.drag, Drag::Lasso { .. });
                c.trail.finish(at, pressure, snap);
            }
        }
        if let Some(input) = c.input(self)
            && let Some(live) = c.live.as_mut()
        {
            self.preview_seq += 1;
            live.send(self.preview_seq, input, true);
        }
        self.strokes.push_back(c);
        ctx.request_repaint();
    }

    /// Sends the oldest released stroke once no call is in line, so its
    /// request is built against the meshes the handler resolves it on.
    pub(super) fn send_stroke(&mut self, ctx: &egui::Context) {
        if self.strokes.is_empty() {
            return;
        }
        // The request casts on the clay's mesh as it is (`stroke_request`,
        // then the handler, sync and wait): it goes once every mesh is
        // built, in the background, so neither waits on this thread.
        if self.meshes.busy() || self.meshes.stale(&self.scene) || !self.jobs.queue.is_empty() || !self.jobs.deferred.is_empty() || self.jobs.running.iter().any(|j| j.edits) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        let mut capture = self.strokes.pop_front().expect("checked above");
        match self.stroke_request(&capture) {
            Ok((tool, args)) => {
                let job = self.send_ui_call(tool, args);
                if let Some(mut live) = capture.live.take() {
                    // The handler casts on the mesh as it is now (nothing is
                    // stale, see above); a preview that cast on another
                    // (the clay changed since the press: the stroke before
                    // it installed) is not what the record commits.
                    if let Some(m) = &live.cast_on {
                        live.mesh_moved = !self.meshes.whole(capture.target).is_some_and(|w| std::sync::Arc::ptr_eq(w, m));
                    }
                    live.job = Some(job);
                    self.lives.push(live);
                }
                ctx.request_repaint();
            }
            Err(e) => {
                self.status = e;
                self.clear_preview_of(capture.target);
            }
        }
    }

    /// The plan of a capture's request: the tool, the stroke's settings and
    /// where it sits.
    pub(super) fn plan(&self, c: &Capture) -> Plan {
        let b = &c.brush;
        let mut stroke = json!({ "radius": b.radius, "strength": b.strength });
        if let Some(s) = &b.symmetry {
            stroke["symmetry"] = json!(s);
        }
        // A VDM stamp fades at its own edge: it takes no falloff (a null
        // overrides a preset's too).
        let stamps = b.brush == Brush::Vdm && matches!(c.drag, Drag::Sculpt { smooth: false, .. });
        if stamps {
            stroke["falloff"] = Value::Null;
        } else if let Some(f) = &b.falloff {
            stroke["falloff"] = json!(f);
        }
        if !b.pressure.is_default() {
            stroke["pressure"] = json!(b.pressure);
        }
        let layer = |kind| {
            // The layer selected for the tools (`sculpt_layer select`),
            // named in the request, so it replays the same through MCP.
            let l = self.layer_selection.get(&c.target)?;
            let clay = self.scene.get(c.target)?.clay.as_ref()?;
            (crate::layers::kind_of(clay, l).ok()? == kind).then(|| l.clone())
        };
        let mut plan = Plan { tool: c.drag.tool(), target: c.target, stroke: Value::Null, layer: None, group: None };
        match c.drag {
            Drag::Sculpt { smooth, invert } => {
                if smooth {
                    stroke["brush"] = json!(Brush::Smooth);
                } else {
                    if let Some(p) = &b.preset {
                        stroke["preset"] = json!(p);
                    }
                    stroke["brush"] = json!(b.brush);
                    if let Some(a) = b.alpha.as_ref().filter(|_| b.brush.dabs()) {
                        stroke["alpha"] = json!({ "name": a });
                    } else if let Some(a) = stamps.then(|| self.preset_vdm_alpha(b)).flatten() {
                        stroke["alpha"] = a;
                    }
                }
                if invert {
                    stroke["invert"] = json!(true);
                }
                plan.layer = layer(crate::layers::Kind::Sculpt);
            }
            Drag::Grab | Drag::Snakehook => {
                if let Some(p) = &b.preset {
                    stroke["preset"] = json!(p);
                }
                stroke["brush"] = json!(b.brush);
                plan.layer = layer(crate::layers::Kind::Sculpt);
            }
            Drag::Paint { erase } => {
                stroke["color"] = json!(b.color);
                if erase {
                    stroke["erase"] = json!(true);
                }
                plan.layer = layer(crate::layers::Kind::Paint);
            }
            Drag::MaskPaint { erase } => {
                // The MaskPen preset (the brush's, or the mask's own).
                if b.brush == Brush::MaskPaint && let Some(p) = &b.preset {
                    stroke["preset"] = json!(p);
                }
                stroke["brush"] = json!(Brush::MaskPaint);
                if erase {
                    stroke["invert"] = json!(true);
                }
            }
            Drag::Group => {
                // An id is all or nothing: full strength (the default).
                if let Some(o) = stroke.as_object_mut() {
                    o.remove("strength");
                }
                stroke["brush"] = json!(Brush::GroupPaint);
                plan.group = Some(b.group);
            }
            Drag::Lasso { .. } => {}
        }
        plan.stroke = stroke;
        plan
    }

    /// A vdm brush's preset alpha with what a VDM stamp refuses (use,
    /// depth, mid, mode tile) left out, so the stroke's own alpha
    /// replaces it whole (None: the preset has no alpha).
    fn preset_vdm_alpha(&self, b: &CurrentBrush) -> Option<Value> {
        let p = crate::brush::preset::find(&self.scene.brushes, b.preset.as_ref()?)?;
        let mut a = p.fields().get("alpha")?.as_object()?.clone();
        for k in ["use", "depth", "mid"] {
            a.remove(k);
        }
        if a.get("mode").and_then(Value::as_str) == Some("tile") {
            a.remove("mode");
        }
        Some(Value::Object(a))
    }

    /// Grab and Snakehook's stroke: world points along the drag across the
    /// view plane (grab: the press point and the delta to the mouse).
    fn view_plane_stroke(&self, c: &Capture) -> Option<Value> {
        let a = c.anchor?;
        let plan = self.plan(c);
        let mut stroke = plan.stroke;
        match c.drag {
            Drag::Grab => {
                let delta = *c.path.last()? - a;
                stroke["points"] = json!([a.to_array()]);
                stroke["delta"] = json!(delta.to_array());
            }
            _ => {
                let path = thin(&c.path, crate::brush::resolve::MAX_POINTS);
                (path.len() >= 2).then_some(())?;
                stroke["points"] = json!(path.iter().map(|p| p.to_array()).collect::<Vec<_>>());
            }
        }
        Some(stroke)
    }

    /// The tool request for a finished stroke. Samples whose ray misses the
    /// clay are left out (the handler would refuse the whole stroke), cast
    /// with the handler's own caster in the rest pose.
    fn stroke_request(&mut self, c: &Capture) -> Result<(&'static str, Value), String> {
        let id = c.target;
        if c.generation != self.scene_generation {
            return Err("The scene changed (new, open, undo or redo) after the stroke; nothing was sent.".into());
        }
        if self.scene.get(id).is_none_or(|o| o.shape != Shape::Clay) {
            return Err("The stroke's clay object is gone; nothing was sent.".into());
        }
        let plan = self.plan(c);
        let camera = c.camera;
        let mut request = match c.drag {
            Drag::Lasso { sub } => {
                // The outline as drawn, through the press's camera.
                let px: Vec<[f32; 2]> = thin(&c.px(), 4096);
                if px.len() < 3 {
                    return Err("A lasso needs a longer drag; nothing was sent.".into());
                }
                json!({ "target": id, "actions": [{ "lasso": { "camera": camera, "px": px, "mode": if sub { "sub" } else { "add" } } }] })
            }
            Drag::Grab | Drag::Snakehook => {
                let stroke = self.view_plane_stroke(c).ok_or("The drag was too short; nothing was sent.")?;
                plan.request(|s| *s = stroke)
            }
            _ if c.posed.is_some() => self.posed_request(c, &plan)?,
            _ => {
                let px = self.at_rest(|app| -> Result<_, String> {
                    app.meshes.sync(&app.scene);
                    let cast = app.clay_caster(id)?;
                    Ok(kept(&c.trail.samples, &camera, app.scene.world_matrix(id).inverse(), &cast))
                })?;
                if px.is_empty() {
                    return Err("The stroke missed the clay; nothing was sent.".into());
                }
                let stroke = plan.screen_stroke(&camera, &px);
                plan.request(|s| *s = stroke)
            }
        };
        if let Some(p) = c.pressure_source {
            request["pressure_source"] = json!(p);
        }
        Ok((plan.tool, request))
    }

    /// A sculpt stroke on a posed clay: its samples cast on the clay as
    /// shown, sent as world points. A rig's pose goes back to rest in the
    /// handler (posable symmetry at the press's frame); a clay hung from
    /// an animated bone goes back through its placement here.
    fn posed_request(&mut self, c: &Capture, plan: &Plan) -> Result<Value, String> {
        let posed = c.posed.expect("posed");
        let o = self.scene.get(c.target).ok_or("The stroke's clay object is gone; nothing was sent.")?;
        let mesh = self.meshes.display(o).cloned().ok_or("The clay has no mesh; nothing was sent.")?;
        let to_local = posed.shown.inverse();
        // Many rays: one tree for all of them (brush tools warm it while on).
        let tree = self.meshes.bvh(&mesh);
        let mut points = Vec::new();
        for s in thin(&c.trail.samples, crate::brush::resolve::MAX_POINTS) {
            let (o, d) = c.camera.ray(s.px[0], s.px[1]);
            let Some(hit) = tree.hit(&mesh, to_local.transform_point3(o), to_local.transform_vector3(d)) else { continue };
            let local = to_local.transform_point3(o) + to_local.transform_vector3(d) * hit.t;
            let world = if posed.rigged.is_some() { posed.shown.transform_point3(local) } else { posed.rest.transform_point3(local) };
            let mut p = world.to_array().to_vec();
            p.extend(s.pressure);
            points.push(p);
        }
        if points.is_empty() {
            return Err("The stroke missed the clay; nothing was sent.".into());
        }
        Ok(plan.request(|s| {
            s["points"] = json!(points);
            if let Some(frame) = posed.rigged {
                let mut sym = s.get("symmetry").cloned().unwrap_or(json!({}));
                sym["posable"] = json!(true);
                sym["frame"] = json!(crate::brush::symmetry::PoseFrame::of(frame));
                s["symmetry"] = sym;
            }
        }))
    }

    /// Puts a UI request in the line of tool calls, from `Source::Ui`.
    pub(super) fn send_ui_call(&mut self, tool: &'static str, args: Value) -> u64 {
        // Nobody waits on the reply; it is read back from the job's result.
        let (reply, _) = mpsc::channel();
        let ticket = self.jobs.ticket(tool.into(), reply.clone());
        let job = ticket.id;
        self.jobs.queue.push_back((ToolCall { name: tool.into(), args: args.clone(), reply, source: Source::Ui }, ticket));
        self.ui_activity.push_back(UiActivity { tool, request: args, job, time: crate::ledger::now_secs(), reported: false });
        while self.ui_activity.len() > KEEP_ACTIVITY {
            self.ui_activity.pop_front();
        }
        job
    }

    /// The latest UI requests and how they went, newest last. Stroke
    /// requests carry their samples and camera, so only `full` replies
    /// (get_scene detail or verbose) include requests; the others say
    /// what they were.
    pub(super) fn ui_activity_json(&self, full: bool) -> Value {
        let skip = self.ui_activity.len().saturating_sub(SHOW_ACTIVITY);
        let list: Vec<Value> = self.ui_activity.iter().skip(skip).map(|a| {
            let r = &a.request;
            let mut v = json!({ "job": a.job, "tool": a.tool, "source": "ui", "time": a.time, "target": r["target"] });
            // What it was, in brief.
            let strokes = r["strokes"].as_array().or_else(|| r["actions"][0]["paint"].as_array()).or_else(|| r["actions"][0]["paint"]["strokes"].as_array());
            match strokes.and_then(|s| s.first()) {
                Some(stroke) => {
                    v["brush"] = stroke.get("brush").cloned().unwrap_or(json!("paint"));
                    v["radius"] = stroke["radius"].clone();
                    v["samples"] = json!(stroke["frame"]["screen"]["px"].as_array().or(stroke["points"].as_array()).map_or(0, Vec::len));
                }
                None => {
                    let action = r["action"].as_str().map(str::to_string).or_else(|| r["actions"][0].as_object().and_then(|o| o.keys().next().cloned())).or_else(|| r["actions"][0].as_str().map(str::to_string));
                    if let Some(a) = action {
                        v["action"] = json!(a);
                    }
                }
            }
            if full {
                v["request"] = r.clone();
            }
            match self.jobs.result(a.job) {
                Some(Ok(res)) => {
                    let reply = reply_json(res);
                    v["status"] = json!("done");
                    v["step"] = reply["step"].clone();
                    v["record_ids"] = reply["record_ids"].clone();
                }
                Some(Err(e)) => {
                    v["status"] = json!("failed");
                    v["error"] = json!(e);
                }
                None => v["status"] = self.jobs.state(a.job).unwrap_or(json!("unknown")),
            }
            v
        }).collect();
        json!(list)
    }

    /// Tells the human on the status line how their requests went.
    pub(super) fn report_ui_activity(&mut self) {
        for i in 0..self.ui_activity.len() {
            let a = &self.ui_activity[i];
            if a.reported {
                continue;
            }
            let Some(r) = self.jobs.result(a.job) else { continue };
            self.status = match r {
                Ok(content) => {
                    let reply = reply_json(content);
                    match (a.tool, reply["record_ids"][0].as_u64()) {
                        ("sculpt" | "paint" | "mask" | "group", Some(id)) => format!("{} applied (record {id}); agents see it in ui_activity", match a.tool { "paint" => "Paint stroke", "mask" => "Mask", "group" => "Groups", _ => "Sculpt stroke" }),
                        (tool, _) => format!("{tool} applied; agents see it in ui_activity"),
                    }
                }
                Err(e) => format!("Not applied: {e}"),
            };
            self.ui_activity[i].reported = true;
        }
    }

    /// The live preview of a stroke just pressed, when it can be made: the
    /// worker resolves the stroke so far as its request will resolve
    /// (the same caster, preset merge and resolver) on the clay as the
    /// strokes still waiting to install leave it.
    fn start_preview(&mut self, c: &Capture) -> Option<Live> {
        let id = c.target;
        if c.posed.is_some() || matches!(c.drag, Drag::Lasso { .. }) {
            return None;
        }
        let plan = self.plan(c);
        let brushes = self.scene.brushes.clone();
        // Alpha strokes commit with the library's image; the preview waits.
        if crate::brush::preset::merge(&brushes, None, &plan.stroke).ok()?.get("alpha").is_some_and(|a| !a.is_null()) {
            return None;
        }
        // The clay as the newest stroke still to install on it shows it.
        // (One that has not shown that stroke as it ended, exactly, makes
        // this preview inexact too.)
        let base = self.lives.iter().chain(self.strokes.iter().filter_map(|s| s.live.as_ref())).filter(|l| l.target == id).filter_map(|l| Some((l.shown.as_ref()?.clay.as_ref().ok()?, l))).next_back().map(|((c, _), l)| (c.clone(), !l.settled() || l.inexact()));
        let unsettled = base.as_ref().is_some_and(|b| b.1);
        let clay = base.map(|b| b.0).or_else(|| self.scene.get(id)?.clay.clone())?;
        let inexact = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(unsettled));
        let flag = inexact.clone();
        let dabs_run = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ran = dabs_run.clone();
        // The regular mesh as it is now (no wait: a build may run, or wait
        // for the strokes to pause); its picking tree is built on the worker
        // if it is not yet. If it is not the mesh the request casts on when
        // it is sent (`send_stroke`), the preview is marked inexact then.
        let mesh = self.meshes.whole(id)?.clone();
        let cast_on = c.drag.screen().then(|| mesh.clone());
        let bvhs = self.meshes.bvh_cache();
        let mut tree: Option<std::sync::Arc<crate::mesh::MeshBvh>> = None;
        let world_to_local = self.scene.world_matrix(id).inverse();
        let (to_local, radius_scale) = self.clay_space(id, &json!({})).ok()?;
        let cache = self.replay_cache.clone();
        let camera = c.camera;
        let layer = plan.layer_or_default(&clay);
        let (layer_name, tool) = (layer.clone(), plan.tool);
        let default_brush = plan.default_brush();
        let mut settled: Option<crate::clay::Clay> = None;
        let mut sculpt: Option<crate::sculpt::preview::SculptPreview> = None;
        // The settled clay's levels (it stays the same through the stroke).
        let mut levels: Option<Vec<f32>> = None;
        let work: brush::Work = Box::new(move |input: &Input, last: bool| {
            if settled.is_none() {
                let mut c = clay.clone();
                crate::ledger::settle_clay(&mut c, Some(&cache))?;
                settled = Some(c);
            }
            let clay = settled.as_ref().expect("settled");
            let bvh = tree.get_or_insert_with(|| bvhs.of(&mesh)).clone();
            let cast = |o: Vec3, d: Vec3| super::probe::cast(&mesh, &bvh, o, d);
            let raw = match input {
                Input::Samples(s) => {
                    let px = kept(s, &camera, world_to_local, &cast);
                    if px.is_empty() {
                        return Err("the stroke so far misses the clay".into());
                    }
                    plan.screen_stroke(&camera, &px)
                }
                Input::Stroke(v) => v.clone(),
            };
            let mut raw = crate::brush::preset::merge(&brushes, None, &raw)?;
            if let Some(b) = default_brush.filter(|b| b.marks()) {
                raw["brush"] = json!(b);
            }
            // The level the request will get (the clay's active one).
            if plan.tool == "sculpt" {
                crate::levels::fill(clay, &mut raw, None)?;
            }
            let levels = levels.get_or_insert_with(|| crate::levels::cells(clay));
            let ctx = crate::brush::resolve::ResolveCtx { to_local, radius_scale, world_to_local, snap: None, cast: Some(&cast), view_far: 100.0, snap_call: None, snap_single: default_brush.is_some(), alpha: None, unpose: None, levels: Some(levels.as_slice()) };
            let (spec, mut r) = crate::sculpt::prepare(&[raw], default_brush, &ctx)?.remove(0);
            // A stroke at a finer level first grows a detail region there,
            // which only the request does: the preview is close, not exact.
            let relaid = plan.tool == "sculpt" && crate::levels::grows_regions(clay, spec.cell, &r);
            use crate::sculpt::preview::whole;
            match plan.tool {
                "sculpt" => {
                    let p = sculpt.get_or_insert_with(|| crate::sculpt::preview::SculptPreview::new(clay.clone(), &layer));
                    // Spaced dabs near the path's end still move as it
                    // grows (each symmetric copy's): the last (its end
                    // point), and with a footprint turned along the path the
                    // one before. Path brushes run whole each time
                    // (`SculptPreview`).
                    let hold = match r.params.brush {
                        b if !b.dabs() => 0,
                        Brush::Standard => 1,
                        _ => 2,
                    };
                    let grid = p.update(&r, hold, last)?;
                    flag.store(unsettled || relaid || !p.exact(), std::sync::atomic::Ordering::Relaxed);
                    ran.store(p.dabs_run(), std::sync::atomic::Ordering::Relaxed);
                    let hash = if last { <crate::clay::Detail as crate::ledger::Grid>::hash(grid.as_ref()) } else { crate::ledger::Hash128::default() };
                    Ok((p.shown(grid), hash))
                }
                "paint" => whole::<crate::paint::Paint>(clay, &layer, &r, true),
                "mask" => whole::<crate::mask::SculptMask>(clay, &layer, &r, false),
                _ => {
                    r.group = plan.group;
                    whole::<crate::groups::Groups>(clay, &layer, &r, false)
                }
            }
        });
        let mut live = Live::start(id, tool, layer_name, c.generation, work, self.repaint.clone());
        live.inexact = inexact;
        live.cast_on = cast_on;
        live.dabs_run = dabs_run;
        Some(live)
    }

    /// Brings the viewport's previews up to date: the newest result of the
    /// newest stroke on each clay is drawn; a stroke whose request has
    /// installed is checked against its record and let go.
    pub(super) fn tick_previews(&mut self) {
        let generation = self.scene_generation;
        if self.capture.as_ref().is_some_and(|c| c.generation != generation) {
            self.drop_capture();
        }
        // Strokes released before a scene change are dropped when sent; their
        // previews go now (their clay's id may name another object).
        if self.strokes.iter().any(|c| c.generation != generation) {
            let gone: Vec<u64> = self.strokes.iter().filter(|c| c.generation != generation).map(|c| c.target).collect();
            for c in self.strokes.iter_mut().filter(|c| c.generation != generation) {
                c.live = None;
            }
            for id in gone {
                self.clear_preview_of(id);
            }
        }
        if self.lives.iter().any(|l| l.generation != generation) {
            let gone: Vec<u64> = self.lives.iter().filter(|l| l.generation != generation).map(|l| l.target).collect();
            self.lives.retain(|l| l.generation == generation);
            for id in gone {
                self.clear_preview_of(id);
            }
        }
        // Let go of the strokes whose requests are done (once their
        // preview showed the stroke as it ended, to check it).
        let mut done = Vec::new();
        for (i, live) in self.lives.iter_mut().enumerate() {
            live.poll();
            let Some(job) = live.job else { continue };
            match self.jobs.result(job) {
                // The record is in: an inexact preview goes now, unchecked
                // (it would only hold the committed clay back).
                Some(Ok(_)) if live.inexact() => done.push((i, None)),
                Some(Ok(reply)) if live.settled() || live.shown.as_ref().is_some_and(|s| s.clay.is_err()) => done.push((i, Some(reply_json(reply)))),
                Some(Ok(_)) => {}
                Some(Err(_)) => done.push((i, None)),
                None => {}
            }
        }
        for (i, reply) in done.into_iter().rev() {
            let live = self.lives.remove(i);
            if let (Some(reply), Some(Ok((_, hash)))) = (reply, live.shown.as_ref().map(|s| &s.clay)) {
                self.check_preview(&live, &reply, *hash);
            }
            if !self.lives.iter().any(|l| l.target == live.target) && !self.strokes.iter().chain(&self.capture).any(|s| s.target == live.target && s.live.is_some()) {
                self.meshes.set_preview(live.target, None);
            }
        }
        // The newest preview of each clay is drawn.
        let mut newest: std::collections::HashMap<u64, (u64, crate::clay::Clay)> = Default::default();
        for live in self.lives.iter_mut().chain(self.strokes.iter_mut().filter_map(|s| s.live.as_mut())).chain(self.capture.iter_mut().filter_map(|c| c.live.as_mut())) {
            if live.poll()
                && let Some(s) = &live.shown
            {
                self.preview_work_ms.push(s.ms);
            }
            if let Some(s) = &live.shown
                && let Ok((clay, _)) = &s.clay
            {
                newest.insert(live.target, (s.seq, clay.clone()));
            }
        }
        for (id, (seq, clay)) in newest {
            if self.meshes.preview_shown(id) != Some(seq) {
                self.meshes.set_preview(id, Some((seq, clay)));
            }
        }
        // How long samples took to reach the screen (the newest stroke's).
        let lives = self.lives.iter_mut().chain(self.strokes.iter_mut().filter_map(|s| s.live.as_mut())).chain(self.capture.iter_mut().filter_map(|c| c.live.as_mut()));
        for live in lives {
            let Some(drawn) = self.meshes.preview_drawn(live.target) else { continue };
            let now = std::time::Instant::now();
            live.sent.retain(|(seq, at)| {
                if *seq <= drawn {
                    self.preview_latency.push(now.duration_since(*at).as_secs_f64() * 1000.0);
                    false
                } else {
                    true
                }
            });
        }
        if self.preview_latency.len() > 4096 {
            self.preview_latency.drain(..2048);
        }
        if self.preview_work_ms.len() > 4096 {
            self.preview_work_ms.drain(..2048);
        }
    }

    /// Compares a finished stroke's record with its preview (§4.3: the
    /// preview of the stroke as it ended is the grid the record installed).
    fn check_preview(&mut self, live: &Live, reply: &Value, hash: crate::ledger::Hash128) {
        let Some(clay) = self.scene.get(live.target).and_then(|o| o.clay.as_ref()) else { return };
        let Some(id) = reply["record_ids"].as_array().and_then(|r| r.last()).and_then(Value::as_u64) else { return };
        fn find<G: crate::layers::Layered>(clay: &crate::clay::Clay, layer: &str, id: u64) -> Option<crate::ledger::Hash128> {
            crate::layers::ledger_of::<G>(clay, layer).ok()?.records().iter().find(|r| r.id == id).map(|r| r.result_hash)
        }
        // The ledger the request wrote (record ids are per ledger).
        let found = match live.tool {
            "sculpt" => find::<crate::clay::Detail>(clay, &live.layer, id),
            "paint" => find::<crate::paint::Paint>(clay, &live.layer, id),
            "mask" => find::<crate::mask::SculptMask>(clay, &live.layer, id),
            _ => find::<crate::groups::Groups>(clay, &live.layer, id),
        };
        let Some(record) = found else { return };
        self.preview_checks.0 += 1;
        if record != hash {
            self.preview_checks.1 += 1;
            self.status = format!("The stroke's preview differed from what it applied (record {id}); the applied stroke is shown.");
        }
    }

    /// Stops showing previews of clay `id` (none of its strokes still
    /// previews).
    fn clear_preview_of(&mut self, id: u64) {
        if !self.lives.iter().any(|l| l.target == id) && !self.strokes.iter().chain(&self.capture).any(|s| s.target == id && s.live.is_some()) {
            self.meshes.set_preview(id, None);
        }
    }

    /// The brush ring under the mouse and the strokes drawn and not yet
    /// sent. The ring itself is drawn in 3D, on the surface (`render`'s
    /// cursor); off the surface it is a circle at the camera target's
    /// depth.
    pub(super) fn paint_brush_cursor(&self, ui: &Ui, painter: &egui::Painter, kind: ViewKind, rect: Rect, view_proj: Mat4, on_surface: bool) {
        let ppp = ui.ctx().pixels_per_point();
        let now = self.stroke_camera(kind, rect, ppp);
        for c in self.strokes.iter().chain(&self.capture).filter(|c| c.view == kind) {
            let pts: Vec<Pos2> = c.px().iter().filter_map(|p| reproject(&c.camera, *p, &now)).map(|p| rect.min + egui::vec2(p[0], p[1]) / ppp).collect();
            let color = if matches!(c.drag, Drag::Lasso { .. }) { Color32::from_rgba_unmultiplied(120, 200, 255, 220) } else { Color32::from_rgba_unmultiplied(255, 220, 120, 200) };
            let mut pts = pts;
            if matches!(c.drag, Drag::Lasso { .. }) && let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
        }
        if on_surface {
            return;
        }
        let Some(at) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)) else { return };
        // The radius in pixels at the camera's target depth.
        let target = self.cams.target(kind);
        let side = (target - self.cams.eye(kind)).cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
        let to_screen = |p: Vec3| {
            let c = view_proj * p.extend(1.0);
            (c.w > 1e-6).then(|| egui::vec2(c.x / c.w * 0.5 * rect.width(), -c.y / c.w * 0.5 * rect.height()))
        };
        let r = match (to_screen(target), to_screen(target + side * self.brush.radius)) {
            (Some(a), Some(b)) => (b - a).length().clamp(2.0, rect.width().max(2.0)),
            _ => 20.0,
        };
        let color = self.cursor_color();
        painter.circle_stroke(at, r, Stroke::new(1.5, color));
        painter.circle_filled(at, 1.5, color);
    }

    /// The brush ring's color: the paint color for Paint, blue for the
    /// mask, green for groups, else light grey.
    pub(super) fn cursor_color(&self) -> Color32 {
        match self.tool {
            Tool::Paint => {
                let [r, g, b] = self.brush.color.map(|x| (x * 255.0) as u8);
                Color32::from_rgb(r, g, b)
            }
            Tool::Mask => Color32::from_rgb(90, 170, 255),
            Tool::Groups => Color32::from_rgb(110, 230, 120),
            _ => Color32::from_gray(235),
        }
    }

    /// The 3D brush ring for a pane: where the mouse meets a clay, its
    /// normal there, the radius and the focal ring (`render::Cursor`).
    pub(super) fn brush_cursor(&self, ui: &Ui, rect: Rect, view_proj: Mat4, eval: &super::viewport::Evaluated) -> Option<crate::render::Cursor> {
        if !self.stroke_tool() || !self.show_brush_cursor {
            return None;
        }
        let at = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p))?;
        let (id, t, normal) = self.pick_surface(at, rect, view_proj, eval)?;
        self.scene.get(id).filter(|o| o.shape == Shape::Clay)?;
        let (origin, dir) = Self::ray(at, rect, view_proj);
        // Where the falloff is at half: smooth's (1 - t^2)^2 = 0.5 at
        // 0.541, moved by the focal shift (t to t^(2^(-2 focal))).
        let focal = self.brush.falloff.as_ref().map_or(0.0, |f| f.focal_shift);
        let half = 0.541f32.powf(1.0 / 2f32.powf(-2.0 * focal));
        let [r, g, b, _] = self.cursor_color().to_array();
        Some(crate::render::Cursor { center: origin + dir * t, normal, radius: self.brush.radius, focal: half, color: [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0] })
    }

    /// The Sculpting shelf: the tools and their brush. Every control sends
    /// `brush_preset set_current`, as an agent would.
    pub(super) fn brush_shelf(&mut self, ui: &mut Ui) {
        let mut patch = serde_json::Map::new();
        for (tool, icon, tip) in [(Tool::Sculpt, Icon::Sculpt, "Sculpt Tool: drag on a clay"), (Tool::Paint, Icon::Paint, "Paint Tool: drag on a clay"), (Tool::Mask, Icon::Lasso, "Mask Tool: drag paints the mask (Alt erases, Shift lassos)"), (Tool::Groups, Icon::FaceMode, "Groups Tool: drag paints the polygroup")] {
            if icons::button(ui, icon, 32.0, self.tool == tool, true, tip).clicked() {
                patch.insert("tool".into(), json!(tool_name(tool)));
            }
        }
        ui.separator();
        let b = self.brush.clone();
        let mut names: Vec<String> = crate::brush::preset::builtin().keys().cloned().collect();
        names.extend(self.scene.brushes.keys().filter(|n| !names.contains(n)).cloned().collect::<Vec<_>>());
        egui::ComboBox::from_id_salt("sculpt_preset").width(100.0).selected_text(b.preset.clone().unwrap_or_else(|| "(no preset)".into())).show_ui(ui, |ui| {
            if ui.selectable_label(b.preset.is_none(), "(no preset)").clicked() {
                patch.insert("preset".into(), Value::Null);
            }
            for n in &names {
                if ui.selectable_label(b.preset.as_ref() == Some(n), n).clicked() {
                    patch.insert("preset".into(), json!(n));
                }
            }
        });
        // One button per built-in preset a drag can use.
        for n in ["Standard", "ClayBuildup", "Move", "Inflate", "Smooth", "Pinch", "Snakehook", "MaskPen"] {
            if crate::brush::preset::builtin().contains_key(n) && ui.selectable_label(b.preset.as_deref() == Some(n), n).clicked() {
                patch.insert("preset".into(), json!(n));
                patch.insert("tool".into(), json!("sculpt"));
            }
        }
        egui::ComboBox::from_id_salt("sculpt_brush").width(90.0).selected_text(json!(b.brush).as_str().unwrap_or_default().to_string()).show_ui(ui, |ui| {
            for brush in UI_BRUSHES {
                if ui.selectable_label(brush == b.brush, json!(brush).as_str().unwrap_or_default()).clicked() {
                    patch.insert("brush".into(), json!(brush));
                }
            }
        });
        let (mut radius, mut strength, mut invert) = (b.radius, b.strength, b.invert);
        if ui.add(egui::Slider::new(&mut radius, 0.002..=1.0).logarithmic(true).clamping(egui::SliderClamping::Edits).text("Radius")).changed() {
            patch.insert("radius".into(), json!(radius));
        }
        if ui.add(egui::Slider::new(&mut strength, 0.0..=1.0).clamping(egui::SliderClamping::Edits).text("Strength")).changed() {
            patch.insert("strength".into(), json!(strength));
        }
        if ui.checkbox(&mut invert, "Invert").changed() {
            patch.insert("invert".into(), json!(invert));
        }
        let mut mirror = b.symmetry.as_ref().is_some_and(|s| s.axes.contains(&Axis::X));
        if ui.checkbox(&mut mirror, "Symmetry X").changed() {
            patch.insert("symmetry".into(), symmetry_with(b.symmetry.as_ref(), Axis::X, mirror));
        }
        let mut color = b.color;
        if egui::color_picker::color_edit_button_rgb(ui, &mut color).changed() {
            patch.insert("color".into(), json!(color));
        }
        ui.label(RichText::new("Shift smooth · Alt invert · Ctrl mask · Ctrl+Shift lasso").color(theme::TEXT_DIM));
        if !patch.is_empty() {
            patch.insert("action".into(), json!("set_current"));
            if let Err(e) = self.brush_preset(&Value::Object(patch)) {
                self.status = e;
            }
        }
    }
}

/// `sym` with mirror `axis` on or off, as `set_current` takes it (null:
/// no symmetry left).
pub(super) fn symmetry_with(sym: Option<&Symmetry>, axis: Axis, on: bool) -> Value {
    let mut s = sym.cloned().unwrap_or_default();
    s.axes.retain(|a| *a != axis);
    if on {
        s.axes.push(axis);
        s.axes.sort();
    }
    if s.is_none() { Value::Null } else { json!(s) }
}

#[cfg(test)]
pub(super) mod tests;
