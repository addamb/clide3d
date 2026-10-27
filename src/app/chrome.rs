//! Window chrome around the viewport: main menu bar, status line, shelf, and
//! the toolbox on the left edge.

use egui::{Button, RichText, Ui};

use super::hotkeys::Action;
use super::icons::{self, Icon};
use super::{ClideApp, RightTab, Shading, Tool, theme};
use crate::camera::ViewKind;
use crate::scene::Shape;

const MENU_SETS: [&str; 5] = ["Modeling", "Rigging", "Animation", "FX", "Rendering"];
const SHELF_TABS: [&str; 13] = [
    "Curves / Surfaces", "Poly Modeling", "Sculpting", "Rigging", "Animation", "Rendering", "FX",
    "FX Caching", "Custom", "Bifrost", "MASH", "Motion Graphics", "XGen",
];

/// Menus that only exist as placeholders today, keyed by menu set. Items use
/// Maya's real names so the layout reads right; they are shown disabled.
fn menu_set_menus(set: usize) -> &'static [(&'static str, &'static [&'static str])] {
    match set {
        0 => &[
            ("Mesh", &["Booleans", "Combine", "Separate", "Smooth", "Reduce"]),
            ("Edit Mesh", &["Add Divisions", "Bevel", "Bridge", "Extrude", "Merge"]),
            ("Mesh Tools", &["Insert Edge Loop", "Multi-Cut", "Quad Draw"]),
            ("Mesh Display", &["Soften Edge", "Harden Edge"]),
            ("Curves", &["Rebuild", "Reverse Direction"]),
            ("Surfaces", &["Loft", "Planar", "Revolve"]),
            ("Deform", &["Blend Shape", "Lattice", "Wrap"]),
            ("UV", &["Automatic", "Planar", "UV Editor"]),
            ("Generate", &["Paint Effects"]),
        ],
        1 => &[
            ("Skeleton", &["Create Joints", "Insert Joint"]),
            ("Skin", &["Bind Skin", "Paint Skin Weights"]),
            ("Deform", &["Blend Shape", "Lattice", "Wrap"]),
            ("Constrain", &["Parent", "Point", "Orient", "Aim"]),
            ("Control", &["Create Controls"]),
        ],
        2 => &[
            ("Key", &["Set Key", "Graph Editor"]),
            ("Visualize", &["Create Editable Motion Trail"]),
            ("Deform", &["Blend Shape", "Lattice", "Wrap"]),
            ("Constrain", &["Parent", "Point", "Orient", "Aim"]),
        ],
        3 => &[
            ("nParticles", &["Create Emitter"]),
            ("Fluids", &["3D Container"]),
            ("Effects", &["Fire", "Smoke"]),
        ],
        _ => &[
            ("Lighting/Shading", &["Hypershade", "Assign New Material"]),
            ("Texturing", &["3D Paint Tool"]),
            ("Render", &["Render Current Frame"]),
        ],
    }
}

fn disabled(ui: &mut Ui, label: &str) {
    ui.add_enabled(false, Button::new(label)).on_disabled_hover_text("Not built yet");
}

/// A menu item with a right-aligned hotkey hint.
fn item(ui: &mut Ui, label: &str, hotkey: &str, enabled: bool) -> bool {
    let clicked = ui
        .add_enabled(enabled, Button::new(label).shortcut_text(hotkey).min_size(egui::vec2(180.0, 0.0)))
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

impl ClideApp {
    pub(super) fn menu_bar(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if item(ui, "New Scene", "⌘N", true) {
                    self.new_scene();
                }
                if item(ui, "Open Scene…", "⌘O", true) {
                    self.open();
                }
                if item(ui, "Save Scene", "⌘S", true) {
                    self.save(false);
                }
                if item(ui, "Save Scene As…", "⇧⌘S", true) {
                    self.save(true);
                }
                // The same call an agent makes: history {action: quicksave}.
                let on = self.quicksave.enabled;
                if item(ui, if on { "QuickSave Every 5 Min ✓" } else { "QuickSave Every 5 Min" }, "", true) {
                    match self.history_tool(&serde_json::json!({ "action": "quicksave", "enabled": !on, "now": false })) {
                        Ok(_) => self.status = format!("QuickSave {}", if on { "off" } else { "on" }),
                        Err(e) => self.status = e,
                    }
                }
                ui.separator();
                // The same call an agent makes: import_mesh {path}.
                if item(ui, "Import…", "", true) {
                    self.import_dialog();
                }
                // set_reference {sheet: {path}}: split into views, planes placed.
                if item(ui, "Load Turnaround Sheet…", "", true) {
                    self.sheet_dialog();
                }
                if item(ui, "Export glTF (.glb)…", "", true) {
                    self.export_dialog("glb");
                }
                if item(ui, "Export FBX (.fbx)…", "", true) {
                    self.export_dialog("fbx");
                }
                // export_print {path, targets: the selection}.
                if item(ui, "Export for Print…", "", true) {
                    self.export_print_dialog();
                }
                // export_poly {target: the lead Poly, path}.
                let lead_poly = self.lead().and_then(|id| self.scene.get(id)).is_some_and(|o| o.poly.is_some());
                if item(ui, "Export Subdivision Cage…", "", lead_poly) {
                    self.export_poly_dialog();
                }
                ui.separator();
                if item(ui, "Exit", "⌘Q", true) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if item(ui, "Undo", "⌘Z", self.history.can_undo()) {
                    self.undo();
                }
                if item(ui, "Redo", "⇧⌘Z", self.history.can_redo()) {
                    self.redo();
                }
                ui.separator();
                let sel = !self.selection.is_empty();
                let hotkeys = self.hotkeys.clone();
                let hint = |a| hotkeys.hint(a);
                if item(ui, "Duplicate", &hint(Action::Duplicate), sel) {
                    self.duplicate_selected();
                }
                if item(ui, "Delete", &hint(Action::Delete), sel) {
                    self.delete_selected();
                }
                ui.separator();
                if item(ui, "Group", &hint(Action::Group), sel) {
                    self.group_selected();
                }
                if item(ui, "Ungroup", "", sel) {
                    self.ungroup_selected();
                }
                if item(ui, "Parent", &hint(Action::Parent), self.selection.len() > 1) {
                    self.parent_selected();
                }
                if item(ui, "Unparent", &hint(Action::Unparent), sel) {
                    self.unparent_selected();
                }
            });
            ui.menu_button("Create", |ui| {
                ui.menu_button("Polygon Primitives", |ui| {
                    for shape in Shape::ALL {
                        if item(ui, shape.label(), "", true) {
                            self.create(shape);
                        }
                    }
                });
                if item(ui, "Empty Group", "", true) {
                    self.create(Shape::Group);
                }
                if item(ui, "Eye", "", true) {
                    self.create(Shape::Eye);
                }
                // create_character with the default base (Stage 5c).
                if item(ui, "Character Base", "", true) {
                    let req = self.character_request();
                    self.send_ui_call("create_character", req);
                }
                // Lids from the selected clay's eyes, fur over it
                // (character follow-ups): the requests an agent sends.
                let lids = self.lids_request();
                if item(ui, "Eyelids on Selected Clay", "", lids.is_some())
                    && let Some(req) = lids
                {
                    self.send_ui_call("edit_clay", req);
                }
                let fur = self.fur_request();
                if item(ui, "Fur on Selected Clay", "", fur.is_some())
                    && let Some(req) = fur
                {
                    self.send_ui_call("create_object", req);
                }
                for sub in ["NURBS Primitives", "Lights", "Cameras"] {
                    ui.menu_button(sub, |ui| disabled(ui, "Not built yet"));
                }
                disabled(ui, "Text");
            });
            ui.menu_button("Select", |ui| {
                let sel = !self.selection.is_empty();
                if item(ui, "All", "", !self.scene.objects.is_empty()) {
                    self.select_all();
                }
                if item(ui, "Deselect All", "", sel) {
                    self.selection.clear();
                }
                if item(ui, "Hierarchy", "", sel) {
                    self.select_hierarchy();
                }
                if item(ui, "Inverse", "", !self.scene.objects.is_empty()) {
                    self.select_inverse();
                }
            });
            ui.menu_button("Modify", |ui| {
                let sel = !self.selection.is_empty();
                if item(ui, "Reset Transformations", "", sel) {
                    self.reset_selected();
                }
                disabled(ui, "Freeze Transformations");
                // to_clay {target, mode: volume, resolution, blur, polish}.
                if item(ui, "Remesh as Clay…", "", sel) {
                    self.open_remesh_dialog();
                }
                // poly {action: from_clay, source} and {action: subdivide}.
                let lead = self.lead().and_then(|id| self.scene.get(id));
                let clay = lead.filter(|o| o.clay.is_some()).map(|o| o.id);
                let poly = lead.and_then(|o| o.poly.as_ref().map(|p| (o.id, p.level_count())));
                if item(ui, "Poly from Clay", "", clay.is_some())
                    && let Some(id) = clay
                {
                    self.send_ui_call("poly", serde_json::json!({ "action": "from_clay", "source": id }));
                }
                // retopo {target, faces, adaptive, symmetry, keep_creases, pure_quads}.
                if item(ui, "Retopologize…", "", clay.is_some()) {
                    self.open_retopo_dialog();
                }
                // poly {target, action: uv}.
                if item(ui, "Unwrap Poly UVs", "", poly.is_some()) {
                    self.unwrap_lead_poly();
                }
                // poly {target, action: project, source, levels, mode, cone}.
                if item(ui, "Project Clay Detail…", "", poly.is_some()) {
                    self.open_project_dialog();
                }
                if item(ui, "Subdivide Poly", "", poly.is_some_and(|(_, n)| n < crate::poly::MAX_LEVELS))
                    && let Some((id, n)) = poly
                {
                    self.send_ui_call("poly", serde_json::json!({ "target": id, "action": "subdivide", "levels": n + 1 }));
                }
                ui.separator();
                if item(ui, "Center Pivot", "", sel) {
                    self.center_pivot_selected();
                }
                let pivot_hint = format!("hold {}", self.hotkeys.hint(Action::PivotHold));
                if ui.add(egui::Button::new("Edit Pivot").selected(self.pivot_mode).shortcut_text(pivot_hint)).clicked() {
                    self.pivot_mode = !self.pivot_mode;
                    ui.close();
                }
            });
            ui.menu_button("Display", |ui| {
                let sel = !self.selection.is_empty();
                ui.menu_button("Hide", |ui| {
                    if item(ui, "Hide Selection", &self.hotkeys.hint(Action::HideSelection), sel) {
                        self.set_selected_visible(false);
                    }
                });
                ui.menu_button("Show", |ui| {
                    if item(ui, "Show Selection", &self.hotkeys.hint(Action::ShowSelection), sel) {
                        self.set_selected_visible(true);
                    }
                    if item(ui, "All", "", true) {
                        self.show_all();
                    }
                });
                ui.separator();
                ui.checkbox(&mut self.show_grid, "Grid");
                // Proportion guides: the agent's set_display and guides calls.
                let mut shown = self.show_guides;
                if ui.checkbox(&mut shown, "Proportion Guides").clicked() {
                    self.send_ui_call("set_display", serde_json::json!({ "guides": shown }));
                }
                let measure = self.measure_request();
                if item(ui, "Measure Proportions", "", measure.is_some()) && let Some(req) = measure {
                    self.guides_job = Some(self.send_ui_call("guides", req));
                }
                // MatCaps: the same set_display call an agent sends.
                ui.menu_button("MatCap", |ui| {
                    for name in crate::render::matcap::BUILTIN {
                        let on = self.matcap.as_ref().is_some_and(|m| m.name == name);
                        if ui.selectable_label(on, name).clicked() {
                            self.send_ui_call("set_display", serde_json::json!({ "shading": "matcap", "matcap": name }));
                            ui.close();
                        }
                    }
                    if ui.selectable_label(self.matcap.is_none(), "Off (shaded)").clicked() {
                        self.send_ui_call("set_display", serde_json::json!({ "shading": "shaded" }));
                        ui.close();
                    }
                });
            });
            ui.menu_button("Windows", |ui| {
                ui.checkbox(&mut self.show_outliner, "Outliner");
                if item(ui, "Attribute Editor", "", true) {
                    self.show_right_panel = true;
                    self.right_tab = RightTab::AttributeEditor;
                }
                if item(ui, "Channel Box / Layer Editor", "", true) {
                    self.show_right_panel = true;
                    self.right_tab = RightTab::ChannelBox;
                }
                ui.separator();
                ui.menu_button("General Editors", |ui| disabled(ui, "Script Editor"));
                ui.menu_button("Settings/Preferences", |ui| {
                    if item(ui, "Hotkey Editor", "", true) {
                        self.hotkey_editor.open = true;
                    }
                });
            });
            for (name, items) in menu_set_menus(self.menu_set) {
                ui.menu_button(*name, |ui| {
                    for i in *items {
                        disabled(ui, i);
                    }
                });
            }
            ui.menu_button("Cache", |ui| disabled(ui, "Alembic Cache"));
            ui.menu_button("Help", |ui| {
                if item(ui, "About Clide3D", "", true) {
                    self.status = format!("Clide3D {} · a small Maya-style editor in Rust", env!("CARGO_PKG_VERSION"));
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("workspace")
                    .selected_text("Maya Classic")
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        let _ = ui.selectable_label(true, "Maya Classic");
                    });
                ui.label(RichText::new("Workspace :").color(theme::TEXT_DIM));
            });
        });
    }

    pub(super) fn status_line(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("menu_set")
                .selected_text(MENU_SETS[self.menu_set])
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (i, name) in MENU_SETS.iter().enumerate() {
                        ui.selectable_value(&mut self.menu_set, i, *name);
                    }
                });
            ui.separator();
            let s = 22.0;
            if icons::button(ui, Icon::NewFile, s, false, true, "New Scene").clicked() {
                self.new_scene();
            }
            if icons::button(ui, Icon::Open, s, false, true, "Open Scene").clicked() {
                self.open();
            }
            if icons::button(ui, Icon::Save, s, false, true, "Save Scene").clicked() {
                self.save(false);
            }
            ui.separator();
            if icons::button(ui, Icon::Undo, s, false, self.history.can_undo(), "Undo").clicked() {
                self.undo();
            }
            if icons::button(ui, Icon::Redo, s, false, self.history.can_redo(), "Redo").clicked() {
                self.redo();
            }
            ui.separator();
            icons::button(ui, Icon::ObjectMode, s, true, true, "Select by object type");
            icons::button(ui, Icon::VertexMode, s, false, false, "Select by component: vertex");
            icons::button(ui, Icon::EdgeMode, s, false, false, "Select by component: edge");
            icons::button(ui, Icon::FaceMode, s, false, false, "Select by component: face");
            ui.separator();
            if icons::button(ui, Icon::SnapGrid, s, self.snap_grid, true, "Snap to grids (or hold X)").clicked() {
                self.snap_grid = !self.snap_grid;
            }
            icons::button(ui, Icon::SnapCurve, s, false, false, "Snap to curves");
            icons::button(ui, Icon::SnapPoint, s, false, false, "Snap to points");
            ui.separator();
            ui.label(RichText::new("No Live Surface").color(theme::TEXT_DIM));
            ui.separator();
            egui::ComboBox::from_id_salt("symmetry").selected_text("Symmetry: Off").width(110.0).show_ui(ui, |ui| {
                let _ = ui.selectable_label(true, "Off");
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(&self.mcp_status).color(theme::TEXT_DIM))
                    .on_hover_text("Built-in MCP server: AI agents can drive this window.");
            });
        });
    }

    pub(super) fn shelf(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 1.0;
            for (i, name) in SHELF_TABS.iter().enumerate() {
                let active = self.shelf_tab == i;
                let text = RichText::new(*name).size(11.0).color(if active { theme::TEXT } else { theme::TEXT_DIM });
                let fill = if active { theme::BUTTON } else { egui::Color32::from_gray(58) };
                if ui.add(Button::new(text).fill(fill).corner_radius(egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 })).clicked() {
                    self.shelf_tab = i;
                }
            }
        });
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::BUTTON);
        let mut shelf = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        if SHELF_TABS[self.shelf_tab] == "Sculpting" {
            self.brush_shelf(&mut shelf);
            return;
        }
        if SHELF_TABS[self.shelf_tab] != "Poly Modeling" {
            shelf.label(RichText::new("(empty shelf)").color(theme::TEXT_DIM));
            return;
        }
        let shapes = [(Shape::Cube, Icon::Cube), (Shape::Sphere, Icon::Sphere), (Shape::Cylinder, Icon::Cylinder), (Shape::Cone, Icon::Cone), (Shape::Plane, Icon::Plane), (Shape::Torus, Icon::Torus)];
        for (shape, icon) in shapes {
            if icons::button(&mut shelf, icon, 32.0, false, true, &format!("Polygon {}: create a polygon {}", shape.label(), shape.label().to_lowercase())).clicked() {
                self.create(shape);
            }
        }
        shelf.separator();
        let sel = !self.selection.is_empty();
        if icons::button(&mut shelf, Icon::Duplicate, 32.0, false, sel, "Duplicate: copy the selected object").clicked() {
            self.duplicate_selected();
        }
        if icons::button(&mut shelf, Icon::Delete, 32.0, false, sel, "Delete: remove the selected object").clicked() {
            self.delete_selected();
        }
        shelf.separator();
        for (shading, icon, tip) in [
            (Shading::Wireframe, Icon::Wireframe, "Wireframe (4)"),
            (Shading::Shaded, Icon::Shaded, "Smooth Shade All (5)"),
            (Shading::WireOnShaded, Icon::WireOnShaded, "Wireframe on Shaded"),
        ] {
            if icons::button(&mut shelf, icon, 32.0, self.shading == shading, true, tip).clicked() {
                self.shading = shading;
            }
        }
    }

    pub(super) fn toolbox(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let tools = [
                (Some(Tool::Select), Icon::Select, "Select Tool (Q)"),
                (None, Icon::Lasso, "Lasso Tool"),
                (Some(Tool::Move), Icon::Move, "Move Tool (W)"),
                (Some(Tool::Rotate), Icon::Rotate, "Rotate Tool (E)"),
                (Some(Tool::Scale), Icon::Scale, "Scale Tool (R)"),
                (Some(Tool::Sculpt), Icon::Sculpt, "Sculpt Tool: drag on a clay (brush on the Sculpting shelf)"),
                (Some(Tool::Paint), Icon::Paint, "Paint Tool: drag on a clay (brush on the Sculpting shelf)"),
                (Some(Tool::Mask), Icon::Lasso, "Mask Tool: drag paints the sculpt mask (Alt erases, Shift lassos)"),
                (Some(Tool::Groups), Icon::FaceMode, "Groups Tool: drag paints a polygroup (pick it in the HUD)"),
                (Some(Tool::Groom), Icon::Paint, "Groom Tool: select a Hair and drag over it to groom its guides (brush on the Sculpting shelf)"),
            ];
            for (tool, icon, tip) in tools {
                let r = icons::button(ui, icon, 26.0, tool == Some(self.tool), tool.is_some(), tip);
                if r.hovered() {
                    self.help = tip.to_string();
                }
                if r.clicked()
                    && let Some(t) = tool
                {
                    // Sculpt and Paint go through brush_preset, as an agent
                    // switches them; their brush lives on the Sculpting shelf.
                    if matches!(t, Tool::Sculpt | Tool::Paint | Tool::Mask | Tool::Groups | Tool::Groom) {
                        let name = super::recorder::tool_name(t);
                        if let Err(e) = self.brush_preset(&serde_json::json!({ "action": "set_current", "tool": name })) {
                            self.status = e;
                        }
                        self.shelf_tab = SHELF_TABS.iter().position(|n| *n == "Sculpting").unwrap_or(self.shelf_tab);
                    } else {
                        self.tool = t;
                    }
                }
            }
            ui.add_space(24.0);
            ui.separator();
            ui.add_space(6.0);
            if icons::button(ui, Icon::SingleView, 22.0, !self.four_view, true, "Single Perspective View").clicked() {
                self.four_view = false;
                self.single_view = ViewKind::Persp;
                self.active_view = ViewKind::Persp;
            }
            if icons::button(ui, Icon::FourView, 22.0, self.four_view, true, "Four View (Space)").clicked() {
                self.four_view = true;
            }
            if icons::button(ui, Icon::PerspOutliner, 22.0, self.show_outliner, true, "Persp/Outliner").clicked() {
                self.show_outliner = !self.show_outliner;
            }
        });
    }
}
