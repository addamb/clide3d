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
                ui.separator();
                disabled(ui, "Import…");
                if item(ui, "Export glTF (.glb)…", "", true) {
                    self.export_dialog("glb");
                }
                if item(ui, "Export FBX (.fbx)…", "", true) {
                    self.export_dialog("fbx");
                }
                ui.separator();
                if item(ui, "Exit", "⌘Q", true) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if item(ui, "Undo", "⌘Z", !self.undo.is_empty()) {
                    self.undo();
                }
                if item(ui, "Redo", "⇧⌘Z", !self.redo.is_empty()) {
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
            if icons::button(ui, Icon::Undo, s, false, !self.undo.is_empty(), "Undo").clicked() {
                self.undo();
            }
            if icons::button(ui, Icon::Redo, s, false, !self.redo.is_empty(), "Redo").clicked() {
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
                (None, Icon::Paint, "Paint Selection Tool"),
                (Some(Tool::Move), Icon::Move, "Move Tool (W)"),
                (Some(Tool::Rotate), Icon::Rotate, "Rotate Tool (E)"),
                (Some(Tool::Scale), Icon::Scale, "Scale Tool (R)"),
            ];
            for (tool, icon, tip) in tools {
                let r = icons::button(ui, icon, 26.0, tool == Some(self.tool), tool.is_some(), tip);
                if r.hovered() {
                    self.help = tip.to_string();
                }
                if r.clicked()
                    && let Some(t) = tool
                {
                    self.tool = t;
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
