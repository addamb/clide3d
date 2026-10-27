//! Side panels: Outliner (left), Channel Box and Attribute Editor (right),
//! and the vertical tab strip on the right edge that switches between them.

use egui::{Align, Color32, Layout, Modifiers, RichText, Sense, Stroke, Ui, Vec2};
use glam::Vec3;
use serde_json::{Value, json};

use super::icons::{self, Icon};
use super::{ClideApp, RightTab, theme};
use crate::scene::Shape;

const ROW_H: f32 = 18.0;
const INDENT: f32 = 14.0;

/// A thin menu-like row of words (Maya puts one atop every panel).
fn panel_menu(ui: &mut Ui, menus: &[&str]) {
    egui::MenuBar::new().ui(ui, |ui| {
        for m in menus {
            ui.menu_button(*m, |ui| {
                ui.add_enabled(false, egui::Button::new("Not built yet"));
            });
        }
    });
}

/// Dark, full-width section header like Maya's "INPUTS" / collapsed frames.
fn section(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(11.0).color(theme::TEXT_DIM));
}

/// One Outliner line: object id, tree depth, and whether it has children.
struct Row {
    id: u64,
    depth: usize,
    has_children: bool,
}

impl ClideApp {
    // ---------- outliner ----------

    /// Visible Outliner rows: the tree (minus collapsed branches), or a flat
    /// list of name matches while searching.
    fn outliner_rows(&self) -> Vec<Row> {
        let filter = self.outliner_filter.to_lowercase();
        if !filter.is_empty() {
            return self
                .scene
                .objects
                .iter()
                .filter(|o| o.name.to_lowercase().contains(&filter))
                .map(|o| Row { id: o.id, depth: 0, has_children: false })
                .collect();
        }
        let mut rows = Vec::new();
        let mut stack: Vec<(u64, usize)> = self.scene.children(None).into_iter().rev().map(|id| (id, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            let kids = self.scene.children(Some(id));
            rows.push(Row { id, depth, has_children: !kids.is_empty() });
            if !self.outliner_collapsed.contains(&id) && depth < 64 {
                stack.extend(kids.into_iter().rev().map(|k| (k, depth + 1)));
            }
        }
        rows
    }

    pub(super) fn outliner(&mut self, ui: &mut Ui) {
        panel_menu(ui, &["Display", "Show", "Help"]);
        ui.add(egui::TextEdit::singleline(&mut self.outliner_filter).hint_text("Search…").desired_width(f32::INFINITY));
        ui.add_space(2.0);
        let rows = self.outliner_rows();
        let filter = self.outliner_filter.to_lowercase();
        let (pointer, released, modifiers) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.any_released(), i.modifiers));

        egui::Frame::NONE.fill(theme::FIELD).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for cam in ["persp", "top", "front", "side"] {
                    if filter.is_empty() || cam.contains(&filter) {
                        draw_row(ui, Icon::CameraNode, cam, 0, None, false, theme::TEXT_DIM, false);
                    }
                }
                let mut action = None;
                let mut drop_target: Option<Option<u64>> = None;
                for row in &rows {
                    let Some(obj) = self.scene.get(row.id) else { continue };
                    if let Some((id, buf)) = &mut self.renaming
                        && *id == obj.id
                    {
                        let r = ui.add(egui::TextEdit::singleline(buf).desired_width(f32::INFINITY));
                        r.request_focus();
                        if r.lost_focus() {
                            action = Some(RowAction::FinishRename);
                        }
                        continue;
                    }
                    let selected = self.is_selected(obj.id);
                    // Maya draws hidden objects in a dimmer blue-gray.
                    let color = if self.scene.is_visible(obj.id) { theme::TEXT } else { Color32::from_rgb(110, 125, 150) };
                    let icon = if obj.shape == Shape::Group { Icon::SetNode } else { Icon::MeshNode };
                    let expander = row.has_children.then_some(!self.outliner_collapsed.contains(&obj.id));
                    let is_drop = self.outliner_drag.as_ref().is_some_and(|d| !d.contains(&obj.id))
                        && pointer.is_some_and(|p| ui.max_rect().x_range().contains(p.x) && (ui.cursor().top()..ui.cursor().top() + ROW_H).contains(&p.y));
                    let (r, toggled) = draw_row(ui, icon, &obj.name, row.depth, expander, selected, color, is_drop);
                    if is_drop {
                        drop_target = Some(Some(obj.id));
                    }
                    if toggled {
                        action = Some(RowAction::ToggleCollapse(obj.id));
                    } else if r.double_clicked() {
                        action = Some(RowAction::Rename(obj.id, obj.name.clone()));
                    } else if r.clicked() {
                        action = Some(RowAction::Click(obj.id, modifiers));
                    }
                    if r.drag_started() {
                        action = Some(RowAction::StartDrag(obj.id));
                    }
                    let id = obj.id;
                    let name = obj.name.clone();
                    let visible = obj.visible;
                    r.context_menu(|ui| {
                        let mut pick = |ui: &mut Ui, label: &str, a: RowAction| {
                            if ui.button(label).clicked() {
                                action = Some(a);
                                ui.close();
                            }
                        };
                        pick(ui, "Rename", RowAction::Rename(id, name.clone()));
                        pick(ui, if visible { "Hide" } else { "Show" }, RowAction::ToggleVisible(id));
                        pick(ui, "Duplicate", RowAction::Duplicate(id));
                        pick(ui, "Group", RowAction::Group(id));
                        pick(ui, "Unparent", RowAction::Unparent(id));
                        pick(ui, "Delete", RowAction::Delete(id));
                    });
                }
                for set in ["defaultLightSet", "defaultObjectSet"] {
                    if filter.is_empty() || set.to_lowercase().contains(&filter) {
                        draw_row(ui, Icon::SetNode, set, 0, None, false, theme::TEXT_DIM, false);
                    }
                }
                // Empty space: click clears the selection, dropping here unparents.
                let rest = ui.allocate_response(ui.available_size().max(Vec2::new(0.0, ROW_H)), Sense::click());
                if self.outliner_drag.is_some() && rest.contains_pointer() {
                    drop_target = Some(None);
                    ui.painter().hline(rest.rect.x_range(), rest.rect.top() + 1.0, Stroke::new(1.0, theme::SELECT));
                }
                if rest.clicked() {
                    action = Some(RowAction::Clear);
                }
                // Any release ends the drag; it only reparents over a drop target.
                if released
                    && let Some(dragged) = self.outliner_drag.take()
                    && let Some(target) = drop_target
                {
                    action = Some(RowAction::Drop(dragged, target));
                }
                if let Some(a) = action {
                    self.apply_row_action(a, &rows);
                }
            });
        });
        if self.outliner_drag.is_some() {
            self.help = "Drop on an object to parent under it, or on empty space to unparent".into();
        }
    }

    fn apply_row_action(&mut self, action: RowAction, rows: &[Row]) {
        match action {
            RowAction::Click(id, m) => {
                if m.shift
                    && let Some(anchor) = self.outliner_anchor
                {
                    // Shift+click: select the whole visible range from the anchor.
                    let pos = |x: u64| rows.iter().position(|r| r.id == x);
                    if let (Some(a), Some(b)) = (pos(anchor), pos(id)) {
                        let (lo, hi) = (a.min(b), a.max(b));
                        let mut range: Vec<u64> = rows[lo..=hi].iter().map(|r| r.id).collect();
                        if b < a {
                            range.reverse();
                        }
                        self.selection = range;
                    }
                } else if m.ctrl || m.mac_cmd {
                    self.apply_pick(&[id], Modifiers::SHIFT);
                    self.outliner_anchor = Some(id);
                } else {
                    self.select_only(id);
                    self.outliner_anchor = Some(id);
                }
            }
            RowAction::Clear => self.selection.clear(),
            RowAction::ToggleCollapse(id) => {
                if !self.outliner_collapsed.remove(&id) {
                    self.outliner_collapsed.insert(id);
                }
            }
            RowAction::StartDrag(id) => {
                let dragged = if self.is_selected(id) { self.scene.roots_of(&self.selection) } else { vec![id] };
                self.outliner_drag = Some(dragged);
            }
            RowAction::Drop(dragged, target) => {
                let errors: Vec<String> = dragged.into_iter().filter_map(|id| self.scene.reparent(id, target).err()).collect();
                self.status = errors.first().cloned().unwrap_or_default();
                if let Some(t) = target {
                    self.outliner_collapsed.remove(&t);
                }
            }
            RowAction::Rename(id, name) => self.renaming = Some((id, name)),
            RowAction::FinishRename => {
                if let Some((id, name)) = self.renaming.take()
                    && !name.trim().is_empty()
                    && let Some(o) = self.scene.get_mut(id)
                {
                    o.name = name.trim().to_string();
                }
            }
            RowAction::ToggleVisible(id) => {
                if let Some(o) = self.scene.get_mut(id) {
                    o.visible = !o.visible;
                }
            }
            RowAction::Duplicate(id) => {
                if let Some(copy) = self.scene.duplicate_tree(id) {
                    self.select_only(copy);
                }
            }
            RowAction::Group(id) => {
                if !self.is_selected(id) {
                    self.select_only(id);
                }
                self.group_selected();
            }
            RowAction::Unparent(id) => {
                let _ = self.scene.reparent(id, None);
            }
            RowAction::Delete(id) => {
                self.scene.remove_tree(id);
                self.prune_selection();
            }
        }
    }

    // ---------- right edge tabs ----------

    pub(super) fn vertical_tabs(&mut self, ui: &mut Ui) {
        let tabs = [(RightTab::AttributeEditor, "Attribute Editor"), (RightTab::ChannelBox, "Channel Box / Layer Editor")];
        let mut y = ui.max_rect().top() + 6.0;
        let x = ui.max_rect().center().x;
        for (tab, label) in tabs {
            let font = egui::FontId::proportional(11.0);
            let active = self.show_right_panel && self.right_tab == tab;
            let color = if active { theme::TEXT } else { theme::TEXT_DIM };
            let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
            let size = galley.size();
            let rect = egui::Rect::from_min_size(egui::pos2(x - 9.0, y), Vec2::new(18.0, size.x + 14.0));
            let r = ui.interact(rect, ui.id().with(label), Sense::click());
            if active || r.hovered() {
                ui.painter().rect_filled(rect, 2.0, if active { theme::BUTTON } else { theme::HOVER });
            }
            // Rotate 90° clockwise: the text then reads top to bottom, as in Maya.
            let pos = egui::pos2(x + size.y * 0.5, y + 7.0);
            ui.painter().add(egui::epaint::TextShape::new(pos, galley, color).with_angle(std::f32::consts::FRAC_PI_2));
            if r.clicked() {
                if active {
                    self.show_right_panel = false;
                } else {
                    self.show_right_panel = true;
                    self.right_tab = tab;
                }
            }
            y = rect.bottom() + 4.0;
        }
    }

    // ---------- channel box ----------

    pub(super) fn channel_box(&mut self, ui: &mut Ui) {
        panel_menu(ui, &["Channels", "Edit", "Object", "Show"]);
        let Some(lead) = self.lead().and_then(|id| self.scene.get(id)).cloned() else {
            ui.add_space(6.0);
            ui.label(RichText::new("Nothing selected").color(theme::TEXT_DIM));
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&lead.name).strong());
            if self.selection.len() > 1 {
                ui.label(RichText::new(format!("(+{} more)", self.selection.len() - 1)).color(theme::TEXT_DIM));
            }
        });
        ui.add_space(2.0);

        // Show the lead's values; an edit applies that channel to every
        // selected object, like Maya's channel box.
        let euler = lead.euler_degrees();
        let mut values = [
            lead.translation.x, lead.translation.y, lead.translation.z,
            euler.x, euler.y, euler.z,
            lead.scale.x, lead.scale.y, lead.scale.z,
        ];
        let labels = ["Translate X", "Translate Y", "Translate Z", "Rotate X", "Rotate Y", "Rotate Z", "Scale X", "Scale Y", "Scale Z"];
        let mut changed = None;
        for (i, label) in labels.iter().enumerate() {
            let speed = [0.02, 0.5, 0.01][i / 3];
            channel_row(ui, label, |ui| {
                if ui
                    .add_sized([ui.available_width(), ROW_H], egui::DragValue::new(&mut values[i]).speed(speed).max_decimals(3))
                    .changed()
                {
                    changed = Some(i);
                }
            });
        }
        let mut toggle_vis = false;
        channel_row(ui, "Visibility", |ui| {
            let text = if lead.visible { "on" } else { "off" };
            toggle_vis = ui.add_sized([ui.available_width(), ROW_H], egui::Button::new(text).fill(theme::FIELD)).clicked();
        });
        if let Some(i) = changed {
            let v = values[i];
            for id in self.selection.clone() {
                let Some(o) = self.scene.get_mut(id) else { continue };
                match i {
                    0..=2 => o.translation[i] = v,
                    3..=5 => {
                        let mut e = o.euler_degrees();
                        e[i - 3] = v;
                        o.set_euler_degrees(e);
                    }
                    _ => o.scale[i - 6] = v,
                }
            }
        }
        if toggle_vis {
            self.set_selected_visible(!lead.visible);
        }

        section(ui, "SHAPES");
        ui.label(if lead.shape.is_mesh() { format!("{}Shape", lead.name) } else { "(none — group)".into() });
        section(ui, "INPUTS");
        ui.label(if lead.shape.is_mesh() { format!("poly{}", lead.shape.label()) } else { "—".into() });

        ui.add_space(10.0);
        ui.separator();
        // Layer editor (display layers are not built yet).
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(true, "Display");
            let _ = ui.selectable_label(false, "Anim");
        });
        panel_menu(ui, &["Layers", "Options", "Help"]);
        egui::Frame::NONE.fill(theme::FIELD).show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), ui.available_height().max(40.0)));
        });
    }

    // ---------- attribute editor ----------

    pub(super) fn attribute_editor(&mut self, ui: &mut Ui) {
        panel_menu(ui, &["List", "Selected", "Focus", "Attributes", "Show", "Help"]);
        let Some(id) = self.lead() else {
            ui.add_space(6.0);
            ui.label(RichText::new("Nothing selected").color(theme::TEXT_DIM));
            return;
        };
        let pivot_world = self.scene.pivot_world(id);
        let parent_name = self.scene.get(id).and_then(|o| o.parent).and_then(|p| self.scene.get(p)).map(|p| p.name.clone());
        let mut new_pivot = None;
        let mut center = false;
        let mut stylise: Option<(&'static str, Value)> = None;
        let Some(obj) = self.scene.get_mut(id) else { return };
        let is_mesh = obj.shape.is_mesh();
        let material = format!("lambert{}", obj.id + 1);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.attr_tab, 0, obj.name.as_str());
            if is_mesh {
                ui.selectable_value(&mut self.attr_tab, 1, material.as_str());
            } else {
                self.attr_tab = 0;
            }
        });
        ui.separator();

        if self.attr_tab == 0 {
            ui.horizontal(|ui| {
                ui.label("transform:");
                ui.add(egui::TextEdit::singleline(&mut obj.name).desired_width(140.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Hide").clicked() {
                        obj.visible = false;
                    }
                    if ui.small_button("Show").clicked() {
                        obj.visible = true;
                    }
                });
            });
            ui.label(RichText::new(format!("Parent: {}", parent_name.as_deref().unwrap_or("world"))).color(theme::TEXT_DIM));
            ui.add_space(4.0);
            let vec_row = |ui: &mut Ui, label: &str, v: &mut Vec3, speed: f64| -> bool {
                let mut changed = false;
                channel_row(ui, label, |ui| {
                    let w = (ui.available_width() - 8.0) / 3.0;
                    for c in [&mut v.x, &mut v.y, &mut v.z] {
                        changed |= ui.add_sized([w, ROW_H], egui::DragValue::new(c).speed(speed).max_decimals(3)).changed();
                    }
                });
                changed
            };
            egui::CollapsingHeader::new("Transform Attributes").default_open(true).show(ui, |ui| {
                vec_row(ui, "Translate", &mut obj.translation, 0.02);
                let mut euler = obj.euler_degrees();
                if vec_row(ui, "Rotate", &mut euler, 0.5) {
                    obj.set_euler_degrees(euler);
                }
                vec_row(ui, "Scale", &mut obj.scale, 0.01);
            });
            egui::CollapsingHeader::new("Pivots").default_open(true).show(ui, |ui| {
                let mut p = pivot_world;
                if vec_row(ui, "World Pivot", &mut p, 0.02) {
                    new_pivot = Some(p);
                }
                ui.horizontal(|ui| {
                    ui.add_space(94.0);
                    center = ui.button("Center Pivot").clicked();
                });
            });
            egui::CollapsingHeader::new("Display").default_open(true).show(ui, |ui| {
                ui.checkbox(&mut obj.visible, "Visibility");
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("lambert:");
                ui.label(RichText::new(&material).color(theme::TEXT_DIM));
            });
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Common Material Attributes").default_open(true).show(ui, |ui| {
                channel_row(ui, "Color", |ui| {
                    egui::color_picker::color_edit_button_rgb(ui, &mut obj.color);
                });
            });
            // Stage 4d: the stylised looks, as the agent sets them
            // (set_material preset with apply_to; an eye's style).
            egui::CollapsingHeader::new("Stylised Look").default_open(true).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (label, preset) in STYLIZED_LOOKS {
                        if ui.button(label).on_hover_text(format!("set_material {{name: {preset}, preset: {preset}, apply_to: this object}}")).clicked() {
                            stylise = Some(stylised_look_request(id, Some(preset)));
                        }
                    }
                    if obj.shape == Shape::Eye && ui.button("Stylised eye").on_hover_text("update_object {eye: {style: stylized}}: big clear iris, catch-light").clicked() {
                        stylise = Some(stylised_look_request(id, None));
                    }
                });
            });
        }
        if let Some((tool, args)) = stylise {
            self.send_ui_call(tool, args);
        }
        if let Some(p) = new_pivot {
            self.scene.set_pivot_world(id, p);
        }
        if center {
            let keep = std::mem::replace(&mut self.selection, vec![id]);
            self.center_pivot_selected();
            self.selection = keep;
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.columns(3, |cols| {
                if cols[0].add_sized([cols[0].available_width(), 22.0], egui::Button::new("Select")).clicked() {
                    self.select_only(id);
                }
                cols[1].add_enabled_ui(false, |ui| ui.add_sized([ui.available_width(), 22.0], egui::Button::new("Load Attributes")));
                cols[2].add_enabled_ui(false, |ui| ui.add_sized([ui.available_width(), 22.0], egui::Button::new("Copy Tab")));
            });
        });
    }
}

/// What a Stylised Look button sends for object `id`: the material
/// preset on it (set_material with apply_to), or with None the stylised
/// eye (update_object eye style).
pub(super) fn stylised_look_request(id: u64, preset: Option<&str>) -> (&'static str, Value) {
    match preset {
        Some(p) => ("set_material", json!({ "name": p, "preset": p, "apply_to": id })),
        None => ("update_object", json!({ "target": id, "eye": { "style": "stylized" } })),
    }
}

/// The material tab's stylised looks (Stage 4d): (button, preset).
pub(super) const STYLIZED_LOOKS: [(&str, &str); 4] = [("Skin", "skin_stylized"), ("Lips", "lips_stylized"), ("Blush", "cheek_blush"), ("Fabric", "fabric_soft")];

enum RowAction {
    Click(u64, Modifiers),
    Clear,
    ToggleCollapse(u64),
    StartDrag(u64),
    /// Dragged roots and the new parent (None = world).
    Drop(Vec<u64>, Option<u64>),
    Rename(u64, String),
    FinishRename,
    ToggleVisible(u64),
    Duplicate(u64),
    Group(u64),
    Unparent(u64),
    Delete(u64),
}

/// Draws one Outliner row. `expander` is Some(open) for rows with children.
/// Returns the row response and whether the expander triangle was clicked.
#[allow(clippy::too_many_arguments)]
fn draw_row(ui: &mut Ui, icon: Icon, text: &str, depth: usize, expander: Option<bool>, selected: bool, color: Color32, drop: bool) -> (egui::Response, bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
    if selected {
        ui.painter().rect_filled(rect, 0.0, theme::SELECT);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, Color32::from_gray(55));
    }
    if drop {
        ui.painter().rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0, Color32::from_rgb(255, 230, 0)), egui::StrokeKind::Inside);
    }
    let x = rect.left() + 4.0 + depth as f32 * INDENT;
    let mut toggled = false;
    if let Some(open) = expander {
        let tri = egui::Rect::from_center_size(egui::pos2(x + 5.0, rect.center().y), Vec2::splat(10.0));
        let c = tri.center();
        let pts = if open {
            vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(4.0, -2.0), c + Vec2::new(0.0, 3.0)]
        } else {
            vec![c + Vec2::new(-2.0, -4.0), c + Vec2::new(3.0, 0.0), c + Vec2::new(-2.0, 4.0)]
        };
        ui.painter().add(egui::Shape::convex_polygon(pts, theme::TEXT_DIM, Stroke::NONE));
        toggled = response.clicked() && response.interact_pointer_pos().is_some_and(|p| tri.expand(3.0).contains(p));
    }
    let icon_rect = egui::Rect::from_min_size(egui::pos2(x + 12.0, rect.top() + 2.0), Vec2::splat(14.0));
    icons::paint(ui.painter(), icon_rect, icon, 1.0);
    ui.painter().text(
        egui::pos2(icon_rect.right() + 5.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(12.0),
        if selected { Color32::WHITE } else { color },
    );
    (response, toggled)
}

/// Right-aligned label column then the value widget, like Maya's channel box.
fn channel_row(ui: &mut Ui, label: &str, value: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(90.0, ROW_H), Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(label).color(theme::TEXT_DIM));
        });
        value(ui);
    });
}
