//! The brush tools' HUD (Stage 7 of docs/SCULPT_PLAN.md): a panel over the
//! viewport while a brush tool is active, and the brush hotkeys. Every
//! control is a front end for one tool call (§4.3's table):
//!
//! - size, strength, focal shift, the falloff curve, the alpha, symmetry,
//!   LazyMouse and pressure: `brush_preset set_current` (editor state);
//! - the layer list: `sculpt_layer select | enable | disable | record |
//!   add`, and the amount slider one `sculpt_layer amount` on release
//!   (previewed while it drags);
//! - mask and groups: `set_display overlay`, `mask` actions, `group
//!   show | hide | isolate | show_all`;
//! - Adjust Last: `stroke_edit {action: scale, last: true}` on release;
//! - the history scrubber: `history goto` on release, and `history mark`;
//! - Save: `brush_preset save {spec}` with the current brush.
//!
//! Requests that change the scene go through `send_ui_call`, so they run
//! in the line of agent calls and show in `ui_activity`.
//!
//! Hotkeys while a brush tool is on and the mouse is over the view: hold
//! S, U or O and move the mouse sideways to change size, strength or
//! focal shift; X turns X symmetry on and off (the hotkeys' SymmetryX,
//! which wins over Snap to Grid's X while a brush tool is on); D and
//! Shift+D (the hotkeys' LevelFiner and LevelCoarser, which win over Edit
//! Pivot's D while a brush tool is on) go to the next finer and coarser detail
//! level (`detail_level`), except while a stroke is being drawn.

use egui::{Color32, Key, RichText, Ui};
use serde_json::{Value, json};

use super::recorder::symmetry_with;
use super::{ClideApp, theme};
use crate::brush::curve::{Falloff, Preset};
use crate::brush::symmetry::Axis;

/// HUD width in points.
pub(super) const WIDTH: f32 = 250.0;

/// The HUD's sliders and the hotkey drags in progress.
#[derive(Default)]
pub(super) struct HudState {
    /// Adjust Last's factor while it drags.
    pub adjust: f32,
    /// The history scrubber while it drags.
    pub scrub: Option<u64>,
    /// A layer amount while it drags: (clay, layer, amount).
    pub amount: Option<(u64, String, f32)>,
    /// Whether the HUD drew the slider of `amount` this frame, still
    /// dragging: a drag the HUD stopped drawing (the tool or the selection
    /// changed under it) is dropped with its preview (`tick_hud`).
    pub amount_drawn: bool,
    /// A layer amount sent: its preview shows until the call is done.
    pub amount_sent: Option<(u64, u64)>,
    /// A brush hotkey held: the key, the mouse x and the value at the press.
    pub key_drag: Option<(Key, f32, f32)>,
    /// The name the Save field holds.
    pub save_name: String,
    /// Where the controls were drawn last frame, by name (checks drive
    /// them with the pointer).
    pub rects: std::collections::HashMap<String, egui::Rect>,
    /// The brush the HUD showed last frame (`current_brush_json`).
    pub shown: Value,
    /// "More brush options" is open.
    pub options: bool,
}

impl ClideApp {
    /// The HUD over the viewport `area`, while a brush tool is on.
    pub(super) fn hud_window(&mut self, ctx: &egui::Context, area: egui::Rect) {
        if !self.stroke_tool() {
            return;
        }
        let at = egui::pos2(area.right() - WIDTH - 10.0, area.top() + 30.0);
        egui::Area::new(egui::Id::new("brush_hud")).fixed_pos(at).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::NONE.fill(theme::PANEL.gamma_multiply(0.94)).corner_radius(4.0).inner_margin(6.0).stroke(egui::Stroke::new(1.0, theme::BORDER)).show(ui, |ui| {
                ui.set_width(WIDTH);
                self.hud(ui);
            });
        });
    }

    /// The HUD's controls.
    pub(super) fn hud(&mut self, ui: &mut Ui) {
        ui.spacing_mut().slider_width = 130.0;
        self.hud.rects.clear();
        self.hud.shown = self.current_brush_json();
        let mut patch = serde_json::Map::new();
        let b = self.brush.clone();
        ui.label(RichText::new(format!("Brush: {}{}", json!(b.brush).as_str().unwrap_or_default(), b.preset.as_ref().map(|p| format!(" ({p})")).unwrap_or_default())).strong());
        let (mut radius, mut strength) = (b.radius, b.strength);
        let r = ui.add(egui::Slider::new(&mut radius, 0.002..=1.0).logarithmic(true).text("Size (S)"));
        self.hud.rects.insert("size".into(), r.rect);
        if r.changed() {
            patch.insert("radius".into(), json!(radius));
        }
        if ui.add(egui::Slider::new(&mut strength, 0.0..=1.0).text("Strength (U)")).changed() {
            patch.insert("strength".into(), json!(strength));
        }
        let falloff = b.falloff.clone().unwrap_or_default();
        let mut focal = falloff.focal_shift;
        if ui.add(egui::Slider::new(&mut focal, -1.0..=1.0).text("Focal (O)")).changed() {
            patch.insert("falloff".into(), json!(Falloff { focal_shift: focal, ..falloff.clone() }));
        }
        // The falloff: a preset, or a curve dragged by its points.
        ui.horizontal(|ui| {
            ui.label("Falloff");
            let name = |p: Preset| json!(p).as_str().unwrap_or_default().to_string();
            let shown = if b.falloff.is_none() { "brush".to_string() } else if falloff.curve.is_some() { "curve".to_string() } else { name(falloff.preset) };
            egui::ComboBox::from_id_salt("hud_falloff").width(80.0).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(b.falloff.is_none(), "brush").clicked() {
                    patch.insert("falloff".into(), Value::Null);
                }
                for p in [Preset::Smooth, Preset::Linear, Preset::Sharp, Preset::Flat, Preset::Sphere] {
                    if ui.selectable_label(b.falloff.is_some() && falloff.curve.is_none() && falloff.preset == p, name(p)).clicked() {
                        patch.insert("falloff".into(), json!(Falloff { preset: p, curve: None, focal_shift: falloff.focal_shift }));
                    }
                }
                if ui.selectable_label(falloff.curve.is_some(), "curve").clicked() {
                    patch.insert("falloff".into(), json!(Falloff { curve: Some(vec![[0.0, 1.0], [0.5, 0.6], [1.0, 0.0]]), ..falloff.clone() }));
                }
            });
        });
        if let Some(points) = &falloff.curve
            && let Some(edited) = curve_editor(ui, points)
        {
            patch.insert("falloff".into(), json!(Falloff { curve: Some(edited), ..falloff.clone() }));
        }
        ui.horizontal(|ui| {
            ui.label("Symmetry");
            for axis in [Axis::X, Axis::Y, Axis::Z] {
                let mut on = b.symmetry.as_ref().is_some_and(|s| s.axes.contains(&axis));
                let label = json!(axis).as_str().unwrap_or_default().to_uppercase();
                if ui.toggle_value(&mut on, label).changed() {
                    patch.insert("symmetry".into(), symmetry_with(b.symmetry.as_ref(), axis, on));
                }
            }
        });
        // The rest of the brush: alpha, LazyMouse, pressure, Save.
        ui.toggle_value(&mut self.hud.options, "More brush options");
        if self.hud.options {
            // An alpha from the scene's library.
            ui.horizontal(|ui| {
                ui.label("Alpha");
                egui::ComboBox::from_id_salt("hud_alpha").width(120.0).selected_text(b.alpha.clone().unwrap_or_else(|| "none".into())).show_ui(ui, |ui| {
                    if ui.selectable_label(b.alpha.is_none(), "none").clicked() {
                        patch.insert("alpha".into(), Value::Null);
                    }
                    // Only alphas of the brush's kind (VDM stamps for vdm).
                    let fits = |a: &crate::brush::alpha::Alpha| !b.brush.dabs() || a.vdm().is_some() == (b.brush == crate::brush::spec::Brush::Vdm);
                    for n in self.scene.alphas.iter().filter(|(_, a)| fits(a)).map(|(n, _)| n) {
                        if ui.selectable_label(b.alpha.as_ref() == Some(n), n).clicked() {
                            patch.insert("alpha".into(), json!(n));
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                let lazy = b.lazy.unwrap_or(super::brush::Lazy { radius: 0.0, step: 3.0, snap: false, backtrack: false });
                let mut l = lazy;
                ui.label("Lazy");
                let mut changed = ui.add(egui::DragValue::new(&mut l.radius).range(0.0..=200.0).suffix(" pt")).changed();
                changed |= ui.checkbox(&mut l.snap, "Snap").changed();
                changed |= ui.checkbox(&mut l.backtrack, "Back").changed();
                if changed {
                    patch.insert("lazy".into(), json!(l));
                }
            });
            ui.horizontal(|ui| {
                let (mut velocity, mut tablet) = (b.velocity, b.tablet);
                if ui.checkbox(&mut velocity, "Velocity").on_hover_text("Pressure from the pointer's speed when no pen gives any").changed() {
                    patch.insert("velocity".into(), json!(velocity));
                }
                if cfg!(target_os = "macos") && ui.checkbox(&mut tablet, "Tablet").on_hover_text("macOS tablet pen pressure").changed() {
                    patch.insert("tablet".into(), json!(tablet));
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.hud.save_name).hint_text("preset name").desired_width(120.0));
                let r = ui.add_enabled(!self.hud.save_name.trim().is_empty(), egui::Button::new("Save brush"));
                self.hud.rects.insert("save".into(), r.rect);
                if r.clicked() {
                    let name = self.hud.save_name.trim().to_string();
                    self.send_ui_call("brush_preset", json!({ "action": "save", "name": name, "spec": self.current_brush_spec() }));
                }
            });
        }
        if !patch.is_empty() {
            patch.insert("action".into(), json!("set_current"));
            if let Err(e) = self.brush_preset(&Value::Object(patch)) {
                self.status = e;
            }
        }
        if let Some(id) = self.lead().filter(|id| self.scene.get(*id).is_some_and(|o| o.clay.is_some())) {
            ui.separator();
            self.hud_layers(ui, id);
            ui.separator();
            self.hud_masking(ui, id);
            ui.separator();
            self.hud_adjust_last(ui, id);
        }
        ui.separator();
        self.hud_history(ui);
    }

    /// The layer list: select, eye, record dot and amount per layer.
    fn hud_layers(&mut self, ui: &mut Ui, id: u64) {
        let Some(clay) = self.scene.get(id).and_then(|o| o.clay.clone()) else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Layers").strong());
            if ui.small_button("+").on_hover_text("sculpt_layer add").clicked() {
                let n = (1..).map(|k| format!("Layer{k}")).find(|n| crate::layers::kind_of(&clay, n).is_err()).expect("a free name");
                self.send_ui_call("sculpt_layer", json!({ "target": id, "action": "add", "name": n }));
            }
        });
        let selected = self.layer_selection.get(&id).cloned();
        let recording = crate::layers::default_sculpt_layer(&clay);
        for l in crate::layers::extra(&clay).to_vec() {
            ui.horizontal(|ui| {
                let sculpt = l.kind == crate::layers::Kind::Sculpt;
                let rec = sculpt && recording == l.name;
                let r = ui.add_enabled(sculpt, egui::Button::selectable(rec, RichText::new("R").color(if rec { Color32::from_rgb(230, 70, 70) } else { theme::TEXT_DIM }))).on_hover_text("Record: agent sculpt calls without a layer write it");
                self.hud.rects.insert(format!("record:{}", l.name), r.rect);
                if r.clicked() {
                    self.send_ui_call("sculpt_layer", json!({ "target": id, "action": "record", "name": if rec { Value::Null } else { json!(l.name) } }));
                }
                let mut eye = l.enabled;
                let r = ui.checkbox(&mut eye, "").on_hover_text("Shown (enable / disable)");
                self.hud.rects.insert(format!("eye:{}", l.name), r.rect);
                if r.changed() {
                    self.send_ui_call("sculpt_layer", json!({ "target": id, "action": if eye { "enable" } else { "disable" }, "name": l.name }));
                }
                if ui.selectable_label(selected.as_ref() == Some(&l.name), &l.name).on_hover_text("Select: the brush tools stroke this layer").clicked() {
                    self.send_ui_call("sculpt_layer", json!({ "target": id, "action": "select", "name": l.name }));
                }
                let dragging = self.hud.amount.as_ref().filter(|a| a.0 == id && a.1 == l.name).map(|a| a.2);
                let mut amount = dragging.unwrap_or(l.amount);
                let range = if sculpt { -2.0..=2.0 } else { 0.0..=1.0 };
                let r = ui.add(egui::Slider::new(&mut amount, range));
                self.hud.rects.insert(format!("amount:{}", l.name), r.rect);
                if r.changed() {
                    self.hud.amount = Some((id, l.name.clone(), amount));
                    self.preview_amount(id, &l.name, amount);
                }
                if self.hud.amount.as_ref().is_some_and(|a| a.0 == id && a.1 == l.name) && (r.dragged() || r.changed()) {
                    self.hud.amount_drawn = true;
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.hud.amount = None;
                    let job = self.send_ui_call("sculpt_layer", json!({ "target": id, "action": "amount", "name": l.name, "amount": amount }));
                    self.hud.amount_sent = Some((id, job));
                }
            });
        }
    }

    /// Shows clay `id` with layer `name` at `amount` while the slider
    /// drags (the call is sent on release).
    fn preview_amount(&mut self, id: u64, name: &str, amount: f32) {
        let Some(mut clay) = self.scene.get(id).and_then(|o| o.clay.clone()) else { return };
        if let Some(l) = crate::ledger::stack_mut(&mut clay).layers.iter_mut().find(|l| l.name == name) {
            l.amount = amount;
        }
        self.preview_seq += 1;
        self.meshes.set_preview(id, Some((self.preview_seq, clay)));
    }

    /// The mask overlay and actions, and the polygroups.
    fn hud_masking(&mut self, ui: &mut Ui, id: u64) {
        let overlay = self.meshes.overlay.name();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mask").strong());
            for o in ["none", "mask", "groups"] {
                if ui.selectable_label(overlay == o, o).on_hover_text("set_display overlay").clicked() {
                    self.send_ui_call("set_display", json!({ "overlay": o }));
                }
            }
        });
        ui.horizontal(|ui| {
            for (label, action) in [("Invert", json!("invert")), ("Clear", json!("clear")), ("Grow", json!({ "grow": 1 })), ("Shrink", json!({ "shrink": 1 })), ("Blur", json!({ "blur": 1 }))] {
                let r = ui.small_button(label);
                self.hud.rects.insert(format!("mask:{label}"), r.rect);
                if r.clicked() {
                    self.send_ui_call("mask", json!({ "target": id, "actions": [action] }));
                }
            }
        });
        let groups = self.scene.get(id).and_then(|o| o.clay.as_ref()).and_then(|c| c.groups.clone());
        let Some(groups) = groups else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Groups").strong());
            if ui.small_button("show all").clicked() {
                self.send_ui_call("group", json!({ "target": id, "actions": ["show_all"] }));
            }
        });
        for (gid, info) in groups.data().info.iter().take(12) {
            ui.horizontal(|ui| {
                let mut visible = info.visible;
                let r = ui.checkbox(&mut visible, "").on_hover_text("group show / hide");
                self.hud.rects.insert(format!("group:{gid}"), r.rect);
                if r.changed() {
                    self.send_ui_call("group", json!({ "target": id, "actions": [{ if visible { "show" } else { "hide" }: [gid] }] }));
                }
                let name = if info.name.is_empty() { format!("group {gid}") } else { info.name.clone() };
                if ui.selectable_label(self.brush.group == *gid, name).on_hover_text("Paint this group (the Groups tool)").clicked() {
                    let _ = self.brush_preset(&json!({ "action": "set_current", "group": gid }));
                }
                if ui.small_button("solo").on_hover_text("group isolate").clicked() {
                    self.send_ui_call("group", json!({ "target": id, "actions": [{ "isolate": [gid] }] }));
                }
            });
        }
    }

    /// Adjust Last: the newest stroke on the clay at 0..2 of itself, sent
    /// as one `stroke_edit scale last` on release.
    fn hud_adjust_last(&mut self, ui: &mut Ui, id: u64) {
        if self.hud.adjust == 0.0 {
            self.hud.adjust = 1.0;
        }
        let enabled = self.last_deltas.contains_key(&id);
        let r = ui.add_enabled(enabled, egui::Slider::new(&mut self.hud.adjust, 0.0..=2.0).text("Adjust Last"));
        self.hud.rects.insert("adjust_last".into(), r.rect);
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            let factor = self.hud.adjust;
            self.hud.adjust = 1.0;
            self.send_ui_call("stroke_edit", json!({ "target": id, "action": "scale", "last": true, "factor": factor }));
        }
    }

    /// The history scrubber (one `history goto` on release) and a mark.
    fn hud_history(&mut self, ui: &mut Ui) {
        let (first, last) = self.history.bounds();
        let current = self.history.current();
        let mut at = self.hud.scrub.unwrap_or(current);
        ui.horizontal(|ui| {
            let r = ui.add_enabled(last > first, egui::Slider::new(&mut at, first..=last.max(first + 1)).text("History"));
            self.hud.rects.insert("history".into(), r.rect);
            if r.changed() {
                self.hud.scrub = Some(at);
            }
            if (r.drag_stopped() || (r.changed() && !r.dragged())) && at != current {
                self.hud.scrub = None;
                self.send_ui_call("history", json!({ "action": "goto", "index": at }));
            } else if r.drag_stopped() {
                self.hud.scrub = None;
            }
            if ui.small_button("Mark").on_hover_text("history mark").clicked() {
                self.send_ui_call("history", json!({ "action": "mark", "name": format!("mark {current}") }));
            }
        });
    }

    /// A layer amount sent from the HUD shows its preview until it ran. A
    /// slider drag the HUD did not go on with last frame (it was not drawn,
    /// or let go without a release it saw) is dropped: its preview goes and
    /// no call is sent.
    pub(super) fn tick_hud(&mut self) {
        if !std::mem::take(&mut self.hud.amount_drawn)
            && let Some((id, _, _)) = self.hud.amount.take()
            && self.hud.amount_sent.is_none_or(|s| s.0 != id)
            && !self.lives.iter().any(|l| l.target == id)
            && !self.strokes.iter().chain(&self.capture).any(|s| s.target == id)
        {
            self.meshes.set_preview(id, None);
        }
        if let Some((id, job)) = self.hud.amount_sent
            && self.jobs.result(job).is_some()
        {
            self.hud.amount_sent = None;
            if !self.lives.iter().any(|l| l.target == id) && !self.strokes.iter().chain(&self.capture).any(|s| s.target == id) {
                self.meshes.set_preview(id, None);
            }
        }
    }

    /// Moves the lead clay's active detail level up (finer) or down by
    /// `by`, through `detail_level` (the hotkeys' LevelFiner and
    /// LevelCoarser, D and Shift+D); nothing past either end, nor while a
    /// stroke is being drawn (it works at the level it began at).
    pub(super) fn step_level(&mut self, by: i32) {
        if self.capture.is_some() || self.view_drag.as_ref().is_some_and(|d| d.mode == super::DragMode::Stroke) {
            return;
        }
        let Some(id) = self.lead().filter(|id| self.scene.get(*id).is_some_and(|o| o.clay.is_some())) else { return };
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).expect("checked");
        let (count, now) = (crate::levels::cells(clay).len() as i32, crate::levels::active(clay) as i32);
        let to = now + by;
        if (0..count).contains(&to) && to != now {
            self.send_ui_call("detail_level", json!({ "target": id, "level": to }));
        }
    }

    /// X symmetry on or off (the hotkeys' SymmetryX, X), through
    /// `brush_preset set_current`.
    pub(super) fn toggle_symmetry_x(&mut self) {
        let on = !self.brush.symmetry.as_ref().is_some_and(|s| s.axes.contains(&Axis::X));
        let _ = self.brush_preset(&json!({ "action": "set_current", "symmetry": symmetry_with(self.brush.symmetry.as_ref(), Axis::X, on) }));
    }

    /// The brush hotkeys (see the module doc), each through
    /// `brush_preset set_current`.
    pub(super) fn brush_keys(&mut self, ctx: &egui::Context) {
        if !self.stroke_tool() || ctx.wants_keyboard_input() || self.hotkey_editor.open {
            self.hud.key_drag = None;
            return;
        }
        let (mods, pointer) = ctx.input(|i| (i.modifiers, i.pointer.hover_pos()));
        let Some(x) = pointer.map(|p| p.x) else { return };
        let focal = |b: &super::recorder::CurrentBrush| b.falloff.as_ref().map_or(0.0, |f| f.focal_shift);
        match self.hud.key_drag {
            None => {
                for key in [Key::S, Key::U, Key::O] {
                    if ctx.input(|i| i.key_pressed(key)) && mods.is_none() && self.hover_view.is_some() {
                        let start = match key {
                            Key::S => self.brush.radius,
                            Key::U => self.brush.strength,
                            _ => focal(&self.brush),
                        };
                        self.hud.key_drag = Some((key, x, start));
                    }
                }
            }
            Some((key, x0, start)) => {
                let dx = x - x0;
                let patch = match key {
                    Key::S => json!({ "radius": (start * 2f32.powf(dx / 150.0)).clamp(1e-4, 100.0) }),
                    Key::U => json!({ "strength": (start + dx / 300.0).clamp(0.0, 1.0) }),
                    _ => {
                        let f = self.brush.falloff.clone().unwrap_or_default();
                        json!({ "falloff": Falloff { focal_shift: (start + dx / 300.0).clamp(-1.0, 1.0), ..f } })
                    }
                };
                if dx != 0.0 {
                    let mut p = patch;
                    p["action"] = json!("set_current");
                    let _ = self.brush_preset(&p);
                }
                if !ctx.input(|i| i.key_down(key)) {
                    self.hud.key_drag = None;
                }
            }
        }
    }
}

/// A falloff curve's points, dragged in a small plot (t across, weight
/// up; the ends stay at t 0 and 1). Returns the points when they moved.
fn curve_editor(ui: &mut Ui, points: &[[f32; 2]]) -> Option<Vec<[f32; 2]>> {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(WIDTH - 10.0, 60.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(30));
    let to_screen = |p: [f32; 2]| egui::pos2(rect.left() + p[0] * rect.width(), rect.bottom() - p[1] * rect.height());
    let curve = crate::brush::curve::Monotone::new(points);
    let line: Vec<egui::Pos2> = (0..=40).map(|i| i as f32 / 40.0).map(|t| to_screen([t, curve.eval(t).clamp(0.0, 1.0)])).collect();
    painter.add(egui::Shape::line(line, egui::Stroke::new(1.5, Color32::from_gray(220))));
    let mut out = points.to_vec();
    let mut moved = false;
    for (k, p) in points.iter().enumerate() {
        let id = ui.id().with(("falloff_point", k));
        let r = ui.interact(egui::Rect::from_center_size(to_screen(*p), egui::vec2(10.0, 10.0)), id, egui::Sense::drag());
        if r.dragged() {
            let d = r.drag_delta();
            let mut q = [p[0] + d.x / rect.width(), (p[1] - d.y / rect.height()).clamp(0.0, 1.0)];
            q[0] = drag_t(points, k, q[0]);
            out[k] = q;
            moved = true;
        }
        painter.circle_filled(to_screen(*p), 3.5, if r.hovered() || r.dragged() { theme::SELECT } else { Color32::from_gray(200) });
    }
    moved.then_some(out)
}

/// Where a dragged falloff point's t lands: the ends stay at the ends;
/// inner points stay between their neighbours (a point already closer
/// than the margin to both, as a valid agent-set curve may be, keeps its t).
fn drag_t(points: &[[f32; 2]], k: usize, t: f32) -> f32 {
    if k == 0 {
        return 0.0;
    }
    if k + 1 == points.len() {
        return 1.0;
    }
    let (lo, hi) = (points[k - 1][0] + 0.01, points[k + 1][0] - 0.01);
    if lo <= hi { t.clamp(lo, hi) } else { points[k][0] }
}

#[cfg(test)]
mod tests {
    use super::drag_t;

    /// A valid curve whose inner neighbours are under the margin apart
    /// (agent-set, or dragged together) keeps the point's t, no panic.
    #[test]
    fn a_crowded_falloff_point_drags_without_panicking() {
        let points = [[0.0, 1.0], [0.5, 0.6], [0.505, 0.5], [0.51, 0.4], [1.0, 0.0]];
        assert_eq!(drag_t(&points, 2, 0.9), 0.505);
        assert_eq!(drag_t(&points, 0, 0.3), 0.0);
        assert_eq!(drag_t(&points, 4, 0.3), 1.0);
        let roomy = [[0.0, 1.0], [0.5, 0.5], [1.0, 0.0]];
        assert_eq!(drag_t(&roomy, 1, 2.0), 0.99);
        assert_eq!(drag_t(&roomy, 1, -1.0), 0.01);
    }
}
