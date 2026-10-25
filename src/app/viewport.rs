//! The view panel: its menu and icon bar, then one pane (single view) or four
//! panes (top / persp / front / side). Each pane draws the wgpu scene, runs
//! its own manipulator, handles camera navigation, click picking and box
//! (marquee) selection.

use std::collections::HashMap;

use egui::{Color32, PointerButton, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use glam::{DQuat, DVec3, Mat4, Vec3, Vec4Swizzles};
use transform_gizmo_egui::config::{GizmoModeKind, GizmoVisuals, TransformPivotPoint};
use transform_gizmo_egui::math::Transform;
use transform_gizmo_egui::prelude::*;

use super::hotkeys::Action;
use super::icons::{self, Icon};
use super::{DragMode, ClideApp, Shading, Tool, ViewDrag, theme};
use crate::camera::ViewKind;
use crate::render::{Instance, ViewportCallback};
use crate::mesh::MeshKey;

/// World matrix and effective visibility per object, from `Scene::evaluate`.
pub(super) type Evaluated = HashMap<u64, (Mat4, bool)>;

/// Lead selection is light green, the rest of the selection white (Maya).
const WIRE_OTHER: [f32; 3] = [1.0, 1.0, 1.0];

impl ClideApp {
    pub(super) fn viewport_panel(&mut self, ui: &mut Ui) {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 0)).show(ui, |ui| {
            self.view_menu(ui);
            self.view_toolbar(ui);
        });
        let (area, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        self.viewport_rect = area;

        let panes: Vec<(ViewKind, Rect)> = if self.four_view {
            let gap = 2.0;
            let half = (area.size() - Vec2::splat(gap)) * 0.5;
            ViewKind::FOUR
                .into_iter()
                .enumerate()
                .map(|(i, kind)| {
                    let col = (i % 2) as f32;
                    let row = (i / 2) as f32;
                    let min = area.min + Vec2::new(col * (half.x + gap), row * (half.y + gap));
                    (kind, Rect::from_min_size(min, half))
                })
                .collect()
        } else {
            vec![(self.single_view, area)]
        };

        ui.painter().rect_filled(area, 0.0, theme::BORDER);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        self.hover_view = panes.iter().find(|(_, r)| pointer.is_some_and(|p| r.contains(p))).map(|(k, _)| *k);
        let pivot_mode = self.pivot_mode || self.pivot_held(ui.ctx());
        let snap = self.snap_grid || self.snap_held(ui.ctx());
        for &(kind, rect) in &panes {
            self.pane(ui, kind, rect, pivot_mode, snap);
        }
        self.hud_window(ui.ctx(), area);
        // A drag whose pane vanished (layout switched mid-drag) or whose
        // button is up can no longer finish normally; drop it.
        if let Some(d) = &self.view_drag
            && (!panes.iter().any(|(k, _)| *k == d.view) || !ui.input(|i| i.pointer.button_down(d.button)))
        {
            // A stroke cut short this way is dropped, not sent.
            if d.mode == DragMode::Stroke {
                self.drop_capture();
            }
            self.view_drag = None;
        }
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("View", |ui| {
                if super::hotkeys::menu_hint(ui, "Frame All", self.hotkeys.hint(Action::FrameAll), true) {
                    self.frame_all();
                }
                if super::hotkeys::menu_hint(ui, "Frame Selection", self.hotkeys.hint(Action::FrameSelected), true) {
                    self.frame_selected();
                }
                if ui.button("Default View").clicked() {
                    self.cams = Default::default();
                    ui.close();
                }
            });
            ui.menu_button("Shading", |ui| {
                ui.radio_value(&mut self.shading, Shading::Wireframe, "Wireframe");
                ui.radio_value(&mut self.shading, Shading::Shaded, "Smooth Shade All");
                ui.radio_value(&mut self.shading, Shading::WireOnShaded, "Wireframe on Shaded");
            });
            ui.menu_button("Lighting", |ui| {
                // render_film's environments (with its studio lights), as
                // set_display lighting.
                let presets = [("studio", "Studio (render_film default)"), ("overcast", "Overcast"), ("sunset", "Sunset"), ("golden_hour", "Golden Hour"), ("forest", "Forest"), ("night", "Night"), ("storm", "Storm"), ("sky_noon", "Physical Sky: Noon"), ("sky_afternoon", "Physical Sky: Afternoon"), ("sky_golden", "Physical Sky: Golden"), ("sky_sunset", "Physical Sky: Sunset"), ("classic", "Classic (no environment)")];
                for (name, label) in presets {
                    if ui.radio(self.view_lighting_args == name, label).clicked() {
                        self.choose_lighting(serde_json::json!(name));
                        ui.close();
                    }
                }
                if ui.add_enabled(self.last_film_args.is_some(), egui::RadioButton::new(self.view_lighting_args == "film", "Last Film Look")).clicked() {
                    self.choose_lighting(serde_json::json!("film"));
                    ui.close();
                }
                ui.separator();
                ui.checkbox(&mut self.ao.enabled, "Ambient Occlusion");
                if let Some(s) = crate::render::ao_status(&self.render_state)
                    && self.ao.enabled
                    && !s.active
                    && let Some(why) = s.reason
                {
                    ui.weak(format!("No occlusion: {why}"));
                }
                ui.checkbox(&mut self.shadows.enabled, "Shadows");
                ui.add_enabled_ui(self.shadows.enabled, |ui| {
                    ui.checkbox(&mut self.shadows.ground, "Ground Shadow");
                    ui.checkbox(&mut self.shadows.soft, "Soft Shadows (PCSS)");
                });
                if let Some(s) = crate::render::shadow_status(&self.render_state)
                    && self.shadows.enabled
                    && !s.active
                    && let Some(why) = s.reason
                {
                    ui.weak(format!("No shadows: {why}"));
                }
                ui.separator();
                ui.menu_button("Display", |ui| self.display_menu(ui));
            });
            ui.menu_button("Show", |ui| {
                ui.checkbox(&mut self.show_grid, "Grid");
                ui.checkbox(&mut self.show_bones, "Joints");
            });
            ui.menu_button("Renderer", |ui| {
                let _ = ui.radio(true, "Viewport (wgpu)");
            });
            ui.menu_button("Panels", |ui| {
                for kind in [ViewKind::Persp, ViewKind::Top, ViewKind::Front, ViewKind::Side] {
                    let label = if kind == ViewKind::Persp { "Perspective: persp".to_string() } else { format!("Orthographic: {}", kind.name()) };
                    if ui.radio(!self.four_view && self.single_view == kind, label).clicked() {
                        self.four_view = false;
                        self.single_view = kind;
                        self.active_view = kind;
                        ui.close();
                    }
                }
                ui.separator();
                ui.menu_button("Layouts", |ui| {
                    if ui.radio(!self.four_view, "Single Pane").clicked() {
                        self.four_view = false;
                        ui.close();
                    }
                    if ui.radio(self.four_view, "Four Panes").clicked() {
                        self.four_view = true;
                        ui.close();
                    }
                });
            });
        });
    }

    /// Lighting > an environment: as `set_display lighting`.
    fn choose_lighting(&mut self, v: serde_json::Value) {
        match self.parse_lighting(&v) {
            Ok(l) => {
                self.view_lighting = l;
                self.view_lighting_args = v;
            }
            Err(e) => self.status = e,
        }
    }

    /// Lighting > Display: the HDR finishing, as `set_display.post`.
    fn display_menu(&mut self, ui: &mut Ui) {
        use crate::render::post::Tone;
        let look = self.view_look;
        let mut patch = None;
        let mut hdr = look.enabled;
        if ui.checkbox(&mut hdr, "HDR (bloom + tonemap)").clicked() {
            patch = Some(serde_json::json!({ "enabled": hdr }));
        }
        ui.add_enabled_ui(look.enabled, |ui| {
            let mut bloom = look.bloom > 0.0;
            if ui.checkbox(&mut bloom, "Bloom").clicked() {
                patch = Some(serde_json::json!({ "bloom": bloom }));
            }
            ui.label("Tonemap");
            for tone in [Tone::Aces, Tone::Agx, Tone::Filmic, Tone::None] {
                let label = match tone {
                    Tone::Aces => "ACES",
                    Tone::Agx => "AgX",
                    Tone::Filmic => "Filmic",
                    Tone::None => "None (clip)",
                };
                if ui.radio(look.tone == tone, label).clicked() {
                    patch = Some(serde_json::json!({ "tonemap": tone.name() }));
                }
            }
            let mut ev = look.exposure.max(1e-6).log2();
            if ui.add(egui::Slider::new(&mut ev, -4.0..=4.0).step_by(0.25).text("Exposure (EV)")).changed() {
                // Exposure alone needs no re-parse (which builds film's
                // environment) on every step of a drag.
                self.view_look.exposure = ev.exp2();
                self.view_post["exposure"] = serde_json::json!(ev);
            }
        });
        if let Some(patch) = patch
            && let Err(e) = self.set_post(&patch)
        {
            self.status = e;
        }
    }

    fn view_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let s = 20.0;
            if icons::button(ui, Icon::FrameAll, s, false, true, "Frame All (A)").clicked() {
                self.frame_all();
            }
            if icons::button(ui, Icon::FrameSelected, s, false, !self.selection.is_empty(), "Frame Selection (F)").clicked() {
                self.frame_selected();
            }
            ui.separator();
            if icons::button(ui, Icon::Grid, s, self.show_grid, true, "Grid").clicked() {
                self.show_grid = !self.show_grid;
            }
            ui.separator();
            for (shading, icon, tip) in [
                (Shading::Wireframe, Icon::Wireframe, "Wireframe (4)"),
                (Shading::Shaded, Icon::Shaded, "Smooth Shade All (5)"),
                (Shading::WireOnShaded, Icon::WireOnShaded, "Wireframe on Shaded"),
            ] {
                if icons::button(ui, icon, s, self.shading == shading, true, tip).clicked() {
                    self.shading = shading;
                }
            }
            ui.separator();
            ui.selectable_value(&mut self.world_space, true, RichText::new("World").size(11.0));
            ui.selectable_value(&mut self.world_space, false, RichText::new("Object").size(11.0));
        });
    }

    fn pane(&mut self, ui: &mut Ui, kind: ViewKind, rect: Rect, pivot_mode: bool, snap: bool) {
        let painter = ui.painter_at(rect);
        paint_background(&painter, rect);

        let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
        let view = self.cams.view(kind);
        let proj = self.cams.proj(kind, aspect);
        let view_proj = proj * view;

        // Selecting a group highlights everything under it.
        let lead_tree: Vec<u64> = self.lead().map(|l| std::iter::once(l).chain(self.scene.descendants(l)).collect()).unwrap_or_default();
        let selected_tree: Vec<u64> = self.selection.iter().flat_map(|&s| std::iter::once(s).chain(self.scene.descendants(s))).collect();

        let mut fills = Vec::new();
        let mut wires = Vec::new();
        // One O(n) pass for every world matrix, instead of a parent walk per object.
        let eval = self.scene.evaluate();
        // Scattered copies draw their source's mesh, instanced.
        if self.shading != Shading::Wireframe {
            for d in crate::envkit::scatter::draws(&self.scene, &eval).iter().filter(|d| d.object.shape.is_mesh() && d.scattered) {
                super::eyes::push_fills(&mut fills, &self.meshes, d.object, Instance::new(d.world, d.object.color).with_material(&self.scene.material_of(d.object)));
            }
        }
        for o in self.scene.objects.iter().filter(|o| o.shape.is_mesh()) {
            let Some(&(world, true)) = eval.get(&o.id) else { continue };
            let key = self.meshes.key_of(o);
            if self.shading != Shading::Wireframe {
                super::eyes::push_fills(&mut fills, &self.meshes, o, Instance::new(world, o.color).with_material(&self.scene.material_of(o)));
            }
            // Maya: shaded mode outlines only the selection; the other modes wire everything.
            let color = if lead_tree.contains(&o.id) {
                Some(theme::WIRE_SELECTED)
            } else if selected_tree.contains(&o.id) {
                Some(WIRE_OTHER)
            } else if self.shading != Shading::Shaded {
                Some(theme::WIRE_DEFAULT)
            } else {
                None
            };
            if let Some(c) = color {
                wires.push((key, Instance::new(world, c)));
            }
        }
        fills.sort_by_key(|(k, _): &(MeshKey, Instance)| *k);
        wires.sort_by_key(|(k, _): &(MeshKey, Instance)| *k);
        // The brush ring, on the surface under the mouse.
        let cursor = self.brush_cursor(ui, rect, view_proj, &eval);
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewportCallback {
                slot: kind.slot(),
                view_proj,
                grid_model: kind.grid_model(),
                cam_pos: self.cams.eye(kind),
                fills,
                wires,
                dynamic: self.meshes.own_meshes(),
                show_grid: self.show_grid,
                flat: false,
                lights: crate::envkit::light::view_lights(&self.scene, &eval, self.cams.target(kind)),
                shadows: self.shadows,
                lighting: self.view_lighting.clone(),
                ao: self.ao,
                // Every pane fits shadows around the persp camera, so the
                // panes share one shadow map.
                shadow_focus: Some((self.cams.persp.target, self.cams.persp.distance)),
                // Pure wireframe has no fills to finish.
                post: crate::render::post::ViewLook { enabled: self.view_look.enabled && self.shading != Shading::Wireframe, ..self.view_look },
                pane: Some(rect),
                frame: ui.ctx().cumulative_frame_nr(),
                background: [theme::VIEW_TOP, theme::VIEW_BOTTOM],
                chunks: self.meshes.sculpt_chunks(),
                matcap: self.matcap.clone(),
                // A sculpt drag keeps its shadow until release.
                freeze_shadows: self.capture.is_some(),
                cursor,
            },
        ));

        self.paint_reference(ui, kind, rect);
        self.paint_bones(&painter, rect, view_proj);
        let alt = ui.input(|i| i.modifiers.alt);
        self.run_gizmo(ui, kind, rect, view, proj, alt, snap, pivot_mode);
        self.camera_and_picking(ui, kind, rect, view_proj, alt, &eval);
        if self.stroke_tool() {
            self.paint_brush_cursor(ui, &painter, kind, rect, view_proj, cursor.is_some());
        }

        paint_axis_indicator(&painter, rect, view);
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 8.0),
            egui::Align2::CENTER_BOTTOM,
            kind.name(),
            egui::FontId::proportional(12.0),
            Color32::from_gray(225),
        );
        // How to move the camera, while the mouse is over the view.
        if ui.rect_contains_pointer(rect) && rect.width() > 520.0 {
            let hint = if kind == ViewKind::Persp {
                "Scroll/pinch: zoom to mouse · Right-drag: orbit · Shift+scroll or Shift+right-drag: pan · Double-click: focus · F: frame"
            } else {
                "Scroll/pinch: zoom to mouse · Shift+scroll or Shift+right-drag: pan · F: frame"
            };
            painter.text(rect.left_top() + Vec2::new(8.0, 8.0), egui::Align2::LEFT_TOP, hint, egui::FontId::proportional(11.0), Color32::from_gray(170));
        }
        if pivot_mode && !self.selection.is_empty() {
            painter.text(rect.center_top() + Vec2::new(0.0, 8.0), egui::Align2::CENTER_TOP, "Edit Pivot", egui::FontId::proportional(12.0), Color32::from_rgb(255, 230, 0));
        }
        if self.four_view && kind == self.active_view {
            painter.rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0, theme::SELECT), egui::StrokeKind::Inside);
        }
        if let Some(d) = &self.view_drag
            && d.view == kind
            && d.mode == DragMode::Click
            && d.moved
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        {
            let r = Rect::from_two_pos(d.start, p);
            painter.rect_filled(r, 0.0, Color32::from_white_alpha(12));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Middle);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_gizmo(&mut self, ui: &mut Ui, kind: ViewKind, rect: Rect, view: Mat4, proj: Mat4, alt: bool, snap: bool, pivot_mode: bool) {
        let modes = match (self.tool, pivot_mode) {
            (Tool::Sculpt | Tool::Paint | Tool::Mask | Tool::Groups, _) => return,
            (_, true) => GizmoMode::all_from_kind(GizmoModeKind::Translate),
            (Tool::Select, false) => return,
            (Tool::Move, _) => GizmoMode::all_from_kind(GizmoModeKind::Translate),
            (Tool::Rotate, _) => GizmoMode::all_from_kind(GizmoModeKind::Rotate),
            (Tool::Scale, _) => GizmoMode::all_from_kind(GizmoModeKind::Scale),
        };
        // Alt means camera navigation; hide the manipulator so it cannot grab the drag.
        if alt || self.view_drag.as_ref().is_some_and(|d| d.mode != DragMode::Click || d.view != kind) {
            return;
        }
        // Children follow their parents, so only transform the top-most selected objects.
        let targets: Vec<u64> = if pivot_mode {
            self.lead().into_iter().collect()
        } else {
            self.scene.roots_of(&self.selection)
        };
        let targets: Vec<u64> = targets.into_iter().filter(|&id| self.scene.is_visible(id)).collect();
        if targets.is_empty() {
            return;
        }

        let before: Vec<(Mat4, Transform)> = targets
            .iter()
            .map(|&id| {
                let world = self.scene.world_matrix(id);
                let (s, r, _) = world.to_scale_rotation_translation();
                let t = Transform {
                    scale: s.as_dvec3().into(),
                    rotation: r.as_dquat().into(),
                    translation: self.scene.pivot_world(id).as_dvec3().into(),
                };
                (world, t)
            })
            .collect();

        // A clay with a sculpt mask is not moved by the gizmo: its drag
        // becomes a transform stroke, sent on release (the same request an
        // agent sends), moving what is unmasked.
        let masked = (!pivot_mode && targets.len() == 1).then(|| targets[0]).filter(|&id| self.scene.get(id).and_then(|o| o.clay.as_ref()).is_some_and(|c| c.sculpt_mask.is_some()));
        if let Some((id, start, now)) = self.masked_drag
            && !ui.input(|i| i.pointer.primary_down())
        {
            self.masked_drag = None;
            let delta = transform_matrix(&now) * transform_matrix(&start).inverse();
            if !delta.abs_diff_eq(Mat4::IDENTITY, 1e-6) && self.scene.get(id).is_some() {
                let m: Vec<f32> = delta.to_cols_array().to_vec();
                self.send_ui_call("sculpt", serde_json::json!({ "target": id, "strokes": [{ "brush": "transform", "transform": { "matrix": m } }] }));
            }
        }
        let before: Vec<(Mat4, Transform)> = match (masked, self.masked_drag) {
            (Some(id), Some((d, _, now))) if d == id => vec![(before[0].0, now)],
            _ => before,
        };
        let gizmo = &mut self.gizmos[kind.slot()];
        gizmo.update_config(GizmoConfig {
            view_matrix: view.as_dmat4().into(),
            projection_matrix: proj.as_dmat4().into(),
            viewport: rect,
            modes,
            orientation: if self.world_space || pivot_mode { GizmoOrientation::Global } else { GizmoOrientation::Local },
            pivot_point: TransformPivotPoint::MedianPoint,
            // Maya: grid snap moves in whole units, rotate snaps to 15°.
            snapping: snap,
            snap_distance: 1.0,
            snap_angle: 15f32.to_radians(),
            snap_scale: 0.25,
            visuals: GizmoVisuals {
                x_color: Color32::from_rgb(230, 40, 40),
                y_color: Color32::from_rgb(60, 220, 60),
                z_color: Color32::from_rgb(40, 100, 240),
                s_color: if pivot_mode { Color32::from_rgb(255, 230, 0) } else { Color32::from_rgb(90, 200, 230) },
                highlight_color: Some(Color32::from_rgb(255, 230, 0)),
                inactive_alpha: 0.9,
                stroke_width: 3.0,
                gizmo_size: if pivot_mode { 60.0 } else { 80.0 },
                ..Default::default()
            },
            ..Default::default()
        });
        let ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let inputs: Vec<Transform> = before.iter().map(|(_, t)| *t).collect();
        let Some((_, after)) = gizmo.interact(&ui, &inputs) else { return };
        if let (Some(id), Some(now)) = (masked, after.first()) {
            let start = self.masked_drag.filter(|d| d.0 == id).map_or(before[0].1, |d| d.1);
            self.masked_drag = Some((id, start, *now));
            return;
        }

        for ((&id, (world, old)), new) in targets.iter().zip(&before).zip(&after) {
            if pivot_mode {
                self.scene.set_pivot_world(id, DVec3::from(new.translation).as_vec3());
                continue;
            }
            // Apply the manipulator's change as a world-space delta, so parents,
            // pivots and multi-selection all compose.
            let delta = transform_matrix(new) * transform_matrix(old).inverse();
            self.scene.set_world_matrix(id, delta * *world);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn camera_and_picking(&mut self, ui: &mut Ui, kind: ViewKind, rect: Rect, view_proj: Mat4, alt: bool, eval: &Evaluated) {
        let hovering = ui.rect_contains_pointer(rect);
        let (pos, delta, scroll, pinch, modifiers, double, time) = ui.input(|i| {
            (i.pointer.hover_pos(), i.pointer.delta(), i.smooth_scroll_delta, i.zoom_delta(), i.modifiers, i.pointer.button_double_clicked(PointerButton::Primary), i.time)
        });
        let events = ui.input(|i| i.events.clone());
        // The camera holds still while a stroke records (it stores the view).
        let stroking = self.capture.is_some();
        if hovering && !stroking && let Some(at) = pos {
            // Shift + scroll (two fingers on a trackpad) pans: trackpads have
            // no middle button. Plain scroll and pinch zoom toward the mouse.
            if modifiers.shift && scroll != egui::Vec2::ZERO {
                self.cams.pan(kind, glam::Vec2::new(scroll.x, scroll.y), rect.height());
            } else {
                let amount = scroll.y * 0.004 + if pinch != 1.0 { pinch.ln() } else { 0.0 };
                if amount != 0.0 {
                    let now = ui.input(|i| i.time);
                    let point = match self.zoom_anchor {
                        Some((k, p, t, point)) if k == kind && p.distance(at) < 2.0 && now - t < 0.5 => point,
                        _ => self.zoom_point(kind, at, rect, view_proj, eval),
                    };
                    self.zoom_anchor = Some((kind, at, now, point));
                    self.cams.dolly_toward(kind, point, amount);
                }
            }
            // Double-click a model to turn the camera to that spot; orbit and
            // zoom then work around it.
            if double && let Some(point) = self.hit_point(at, rect, view_proj, eval) {
                self.cams.look_at(kind, point);
            }
        }

        if self.view_drag.is_none() && hovering {
            let pressed = [PointerButton::Primary, PointerButton::Middle, PointerButton::Secondary]
                .into_iter()
                .find(|&b| ui.input(|i| i.pointer.button_pressed(b)));
            let gizmo_grab = !self.selection.is_empty() && self.gizmos[kind.slot()].is_focused();
            if let (Some(button), Some(start)) = (pressed, pos) {
                self.active_view = kind;
                let mode = match (alt, button) {
                    // A brush tool takes Alt as invert (or erase); the
                    // right button orbits.
                    (_, PointerButton::Primary) if self.stroke_tool() => {
                        let ppp = ui.ctx().pixels_per_point();
                        self.begin_stroke(kind, rect, start, view_proj, eval, ppp, modifiers, time, &events).then_some(DragMode::Stroke)
                    }
                    (true, PointerButton::Primary) => Some(DragMode::Orbit),
                    (_, PointerButton::Middle) => Some(DragMode::Pan),
                    // Shift + right-drag pans too, for mice and trackpads
                    // without a middle button.
                    (false, PointerButton::Secondary) if modifiers.shift => Some(DragMode::Pan),
                    (true, PointerButton::Secondary) => Some(DragMode::Dolly),
                    (false, PointerButton::Secondary) => Some(DragMode::Orbit),
                    (false, PointerButton::Primary) if !gizmo_grab => Some(DragMode::Click),
                    _ => None,
                };
                if let Some(mode) = mode {
                    self.view_drag = Some(ViewDrag { button, mode, start, moved: false, view: kind, rect });
                }
            }
        }

        if self.view_drag.as_ref().is_some_and(|d| d.mode == DragMode::Stroke && d.view == kind)
            && self.drop_stroke_if_view_moved(kind)
        {
            self.view_drag = None;
            return;
        }
        // The pane as it is this frame, for a stroke's samples.
        let ppp = ui.ctx().pixels_per_point();
        let now = self.stroke_camera(kind, rect, ppp);
        let Some(drag) = &mut self.view_drag else { return };
        if drag.view != kind {
            return;
        }
        let d = glam::Vec2::new(delta.x, delta.y);
        let (mode, height) = (drag.mode, drag.rect.height());
        match mode {
            DragMode::Orbit => self.cams.orbit(kind, d),
            DragMode::Pan => self.cams.pan(kind, d, height),
            DragMode::Dolly => self.cams.dolly(kind, (delta.x - delta.y) * 0.01),
            DragMode::Click => {}
            DragMode::Stroke => {
                if let Some(p) = pos {
                    self.stroke_sample(p, rect, &now, time, &events);
                }
            }
        }
        let Some(drag) = &mut self.view_drag else { return };
        if pos.is_some_and(|p| p.distance(drag.start) > 4.0) {
            drag.moved = true;
        }
        if ui.input(|i| i.pointer.button_released(drag.button)) {
            let drag = self.view_drag.take().expect("checked above");
            if drag.mode == DragMode::Stroke {
                let at = ui.input(|i| i.pointer.interact_pos()).or(pos);
                self.end_stroke(ui.ctx(), at, rect, &now, time);
            }
            if drag.mode != DragMode::Click {
                return;
            }
            let picked: Vec<u64> = if drag.moved {
                let end = pos.unwrap_or(drag.start);
                self.marquee(Rect::from_two_pos(drag.start, end), rect, view_proj, eval)
            } else {
                self.pick(drag.start, rect, view_proj, eval).into_iter().collect()
            };
            self.apply_pick(&picked, modifiers);
        }
    }

    /// World-space ray through a screen point of a pane.
    pub(super) fn ray(pos: Pos2, rect: Rect, view_proj: Mat4) -> (Vec3, Vec3) {
        let ndc_x = (pos.x - rect.min.x) / rect.width() * 2.0 - 1.0;
        let ndc_y = 1.0 - (pos.y - rect.min.y) / rect.height() * 2.0;
        let inv = view_proj.inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        (near, (far - near).normalize())
    }

    /// The nearest visible mesh under a screen point.
    fn pick(&self, pos: Pos2, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Option<u64> {
        self.pick_hit(pos, rect, view_proj, eval).map(|(id, _)| id)
    }

    /// The point on the nearest visible mesh under a screen point.
    fn hit_point(&self, pos: Pos2, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Option<Vec3> {
        let (origin, dir) = Self::ray(pos, rect, view_proj);
        self.pick_hit(pos, rect, view_proj, eval).map(|(_, t)| origin + dir * t)
    }

    /// Where zooming at a screen point goes: the mesh under it, else the
    /// point at the camera target's depth.
    fn zoom_point(&self, kind: ViewKind, pos: Pos2, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Vec3 {
        if let Some(p) = self.hit_point(pos, rect, view_proj, eval) {
            return p;
        }
        let (origin, dir) = Self::ray(pos, rect, view_proj);
        let target = self.cams.target(kind);
        let normal = (self.cams.eye(kind) - target).normalize_or_zero();
        let denom = dir.dot(normal);
        if denom.abs() < 1e-6 {
            return target;
        }
        origin + dir * ((target - origin).dot(normal) / denom)
    }

    pub(super) fn pick_hit(&self, pos: Pos2, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Option<(u64, f32)> {
        self.pick_surface(pos, rect, view_proj, eval).map(|(id, t, _)| (id, t))
    }

    /// `pick_hit` with the world normal there.
    pub(super) fn pick_surface(&self, pos: Pos2, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Option<(u64, f32, Vec3)> {
        let (origin, dir) = Self::ray(pos, rect, view_proj);
        self.scene
            .objects
            .iter()
            .filter(|o| o.shape.is_mesh())
            .filter_map(|o| {
                let &(world, visible) = eval.get(&o.id)?;
                if !visible {
                    return None;
                }
                // Affine transforms keep the ray parameter, so `t` stays a world distance.
                let to_local = world.inverse();
                let (lo, ld) = (to_local.transform_point3(origin), to_local.transform_vector3(dir));
                // A clay in a sculpt session: the field it shows (hidden
                // groups clip only its mesh); else a tree over the mesh.
                let hidden = o.clay.as_ref().and_then(|c| c.groups.as_ref()).is_some_and(|g| !g.hidden().is_empty());
                let normal = |n: Vec3| world.inverse().transpose().transform_vector3(n).normalize_or_zero();
                if !hidden && let Some(hit) = self.meshes.pick_field(o.id, lo, ld) {
                    return hit.map(|(t, n)| (o.id, t, normal(n)));
                }
                let hit = self.meshes.ray_hit(self.meshes.display(o)?, lo, ld)?;
                Some((o.id, hit.t, normal(hit.normal)))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Visible meshes with any part inside the screen rectangle.
    fn marquee(&self, sel: Rect, rect: Rect, view_proj: Mat4, eval: &Evaluated) -> Vec<u64> {
        let to_screen = |m: Mat4, p: [f32; 3]| -> Option<Pos2> {
            let clip = m * Vec3::from(p).extend(1.0);
            if clip.w <= 1e-6 {
                return None;
            }
            let ndc = clip.xy() / clip.w;
            Some(egui::pos2(rect.min.x + (ndc.x * 0.5 + 0.5) * rect.width(), rect.min.y + (0.5 - ndc.y * 0.5) * rect.height()))
        };
        let center_hit = self.pick(sel.center(), rect, view_proj, eval);
        self.scene
            .objects
            .iter()
            .filter(|o| o.shape.is_mesh())
            .filter(|o| {
                let Some(&(world, true)) = eval.get(&o.id) else { return false };
                let Some(mesh) = self.meshes.get(o) else { return false };
                let m = view_proj * world;
                let pts: Vec<Option<Pos2>> = mesh.vertices.iter().map(|v| to_screen(m, v.pos)).collect();
                center_hit == Some(o.id)
                    || pts.iter().flatten().any(|p| sel.contains(*p))
                    || mesh.edges.chunks_exact(2).any(|e| match (pts[e[0] as usize], pts[e[1] as usize]) {
                        (Some(a), Some(b)) => segment_hits_rect(a, b, sel),
                        _ => false,
                    })
            })
            .map(|o| o.id)
            .collect()
    }
}

fn transform_matrix(t: &Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(
        DVec3::from(t.scale).as_vec3(),
        DQuat::from(t.rotation).as_quat().normalize(),
        DVec3::from(t.translation).as_vec3(),
    )
}

/// Liang–Barsky: does segment a-b pass through the rectangle?
fn segment_hits_rect(a: Pos2, b: Pos2, r: Rect) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-d.x, a.x - r.min.x), (d.x, r.max.x - a.x), (-d.y, a.y - r.min.y), (d.y, r.max.y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

fn paint_background(painter: &egui::Painter, rect: Rect) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), theme::VIEW_TOP);
    mesh.colored_vertex(rect.right_top(), theme::VIEW_TOP);
    mesh.colored_vertex(rect.right_bottom(), theme::VIEW_BOTTOM);
    mesh.colored_vertex(rect.left_bottom(), theme::VIEW_BOTTOM);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// Small XYZ axis triad in the bottom-left corner, like Maya's view axis.
fn paint_axis_indicator(painter: &egui::Painter, rect: Rect, view: Mat4) {
    let origin = rect.left_bottom() + Vec2::new(30.0, -30.0);
    let axes = [
        (Vec3::X, "X", Color32::from_rgb(230, 70, 70)),
        (Vec3::Y, "Y", Color32::from_rgb(110, 210, 90)),
        (Vec3::Z, "Z", Color32::from_rgb(80, 130, 240)),
    ];
    for (axis, label, color) in axes {
        let v = view.transform_vector3(axis);
        // Axes pointing straight at the camera vanish in ortho views; skip them.
        if v.truncate().length() < 0.05 {
            continue;
        }
        let end = origin + Vec2::new(v.x, -v.y) * 18.0;
        painter.line_segment([origin, end], egui::Stroke::new(1.5, color));
        painter.text(end + Vec2::new(v.x, -v.y) * 6.0, egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rect_cases() {
        let r = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0));
        assert!(segment_hits_rect(egui::pos2(-5.0, 5.0), egui::pos2(15.0, 5.0), r));
        assert!(!segment_hits_rect(egui::pos2(-5.0, -5.0), egui::pos2(-1.0, 20.0), r));
        assert!(segment_hits_rect(egui::pos2(2.0, 2.0), egui::pos2(3.0, 3.0), r));
    }
}
