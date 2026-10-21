//! Command line (a small MEL subset) and the help line at the very bottom.
//!
//! Supported MEL (names are optional; without them commands use the selection):
//! polyCube/polySphere/polyCylinder/polyCone/polyPlane/polyTorus [-n name],
//! select [-add|-tgl|-d|-cl|-all|-hi] [names], move/rotate/scale [-r] x y z [names],
//! delete, duplicate, hide, showHidden [-all], group [-em] [-n name] [names],
//! ungroup, parent [names] parent | parent -w [names], xform -cp | -piv x y z,
//! setAttr name.attr value, ls [-sl], currentTime n, undo, redo, file -new.

use egui::{RichText, Ui};
use glam::Vec3;

use super::icons::{self, Icon};
use super::{ClideApp, theme};
use crate::scene::{Scene, Shape};

impl ClideApp {
    pub(super) fn command_line(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let lang = if self.cmd_python { "Python" } else { "MEL" };
            if ui.add_sized([50.0, 18.0], egui::Button::new(lang)).on_hover_text("Click to switch MEL / Python").clicked() {
                self.cmd_python = !self.cmd_python;
            }
            let input_w = (ui.available_width() * 0.6).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.cmd_input).desired_width(input_w).font(egui::TextStyle::Monospace));
            if r.hovered() {
                self.help = "Command Line: type MEL, e.g. polyCube -n box; move 0 2 0".into();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let line = std::mem::take(&mut self.cmd_input);
                self.cmd_result = self.run_command_line(&line);
                r.request_focus();
            }
            let result_w = (ui.available_width() - 28.0).max(60.0);
            let color = if self.cmd_result.starts_with("// Error") { egui::Color32::from_rgb(230, 120, 110) } else { theme::TEXT };
            egui::Frame::NONE.fill(theme::FIELD).inner_margin(egui::Margin::symmetric(4, 2)).show(ui, |ui| {
                ui.set_width(result_w);
                ui.label(RichText::new(&self.cmd_result).monospace().color(color));
            });
            icons::button(ui, Icon::ScriptEditor, 20.0, false, false, "Script Editor");
        });
    }

    pub(super) fn help_line(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.help).size(11.0).color(theme::TEXT_DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(&self.status).size(11.0).color(theme::TEXT_DIM));
            });
        });
    }

    /// Runs one line (several commands may be separated by `;`) and returns
    /// Maya-style feedback for the result field.
    pub(super) fn run_command_line(&mut self, line: &str) -> String {
        if self.cmd_python {
            return "// Error: Python is not supported yet. Switch to MEL. //".into();
        }
        let mut last = String::new();
        for cmd in line.split(';').map(str::trim).filter(|c| !c.is_empty()) {
            match self.run_mel(cmd) {
                Ok(out) => last = if out.is_empty() { String::new() } else { format!("// Result: {out} //") },
                Err(e) => return format!("// Error: {e} //"),
            }
        }
        last
    }

    pub(super) fn run_mel(&mut self, cmd: &str) -> Result<String, String> {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let (name, args) = words.split_first().ok_or("empty command")?;
        let flag = |f: &[&str]| args.iter().any(|a| f.contains(a));
        let flag_value = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).map(|v| v.trim_matches('"'));
        // Positional words: not flags (negative numbers are fine), not a -n value.
        let mut positional = Vec::new();
        let mut skip = false;
        for a in args {
            if skip {
                skip = false;
            } else if *a == "-n" || *a == "-name" {
                skip = true;
            } else if !a.starts_with('-') || a.parse::<f32>().is_ok() {
                positional.push(a.trim_matches('"'));
            }
        }
        let numbers: Vec<f32> = positional.iter().filter_map(|a| a.parse().ok()).collect();
        let names: Vec<&str> = positional.iter().copied().filter(|a| a.parse::<f32>().is_err()).collect();
        let ok = || Ok(String::new());

        match *name {
            "polyCube" | "polySphere" | "polyCylinder" | "polyCone" | "polyPlane" | "polyTorus" => {
                let shape = Shape::ALL.into_iter().find(|s| format!("poly{}", s.label()) == *name).expect("matched above");
                let id = self.create(shape);
                if let Some(n) = flag_value("-n").or(flag_value("-name")) {
                    self.scene.get_mut(id).expect("just created").name = n.to_string();
                }
                Ok(self.scene.get(id).expect("just created").name.clone())
            }
            "select" => {
                if flag(&["-cl", "-clear"]) {
                    self.selection.clear();
                    return ok();
                }
                if flag(&["-all"]) {
                    self.select_all();
                    return ok();
                }
                let ids = self.objects_named(&names)?;
                let m = |shift, ctrl| egui::Modifiers { shift, ctrl, ..Default::default() };
                if flag(&["-add"]) {
                    self.apply_pick(&ids, m(true, true));
                } else if flag(&["-tgl", "-toggle"]) {
                    self.apply_pick(&ids, m(true, false));
                } else if flag(&["-d", "-deselect"]) {
                    self.apply_pick(&ids, m(false, true));
                } else {
                    self.selection = ids;
                }
                if flag(&["-hi", "-hierarchy"]) {
                    self.select_hierarchy();
                }
                ok()
            }
            "move" | "rotate" | "scale" => {
                if numbers.len() != 3 {
                    return Err(format!("{name}: expected 3 numbers"));
                }
                let ids = self.targets(&names, name)?;
                let relative = flag(&["-r", "-relative"]);
                let v = Vec3::new(numbers[0], numbers[1], numbers[2]);
                for id in ids {
                    let obj = self.scene.get_mut(id).expect("resolved");
                    match (*name, relative) {
                        ("move", false) => obj.translation = v,
                        ("move", true) => obj.translation += v,
                        ("rotate", false) => obj.set_euler_degrees(v),
                        ("rotate", true) => {
                            let e = obj.euler_degrees();
                            obj.set_euler_degrees(e + v);
                        }
                        ("scale", false) => obj.scale = v,
                        _ => obj.scale *= v,
                    }
                }
                ok()
            }
            "delete" => {
                let ids = self.targets(&names, name)?;
                for id in self.scene.roots_of(&ids) {
                    self.scene.remove_tree(id);
                }
                self.prune_selection();
                ok()
            }
            "duplicate" => {
                let ids = self.targets(&names, name)?;
                let roots = self.scene.roots_of(&ids);
                self.selection = roots.into_iter().filter_map(|id| self.scene.duplicate_tree(id)).collect();
                Ok(self.selected_names())
            }
            "hide" | "showHidden" => {
                let visible = *name == "showHidden";
                if visible && flag(&["-all"]) {
                    self.show_all();
                    return ok();
                }
                for id in self.targets(&names, name)? {
                    self.scene.get_mut(id).expect("resolved").visible = visible;
                }
                ok()
            }
            "group" => {
                let group = if flag(&["-em", "-empty"]) {
                    self.create(Shape::Group)
                } else {
                    let ids = self.targets(&names, name)?;
                    let g = self.scene.group(&ids);
                    self.select_only(g);
                    g
                };
                if let Some(n) = flag_value("-n").or(flag_value("-name")) {
                    self.scene.get_mut(group).expect("just created").name = n.to_string();
                }
                Ok(self.scene.get(group).expect("just created").name.clone())
            }
            "ungroup" => {
                if !names.is_empty() {
                    self.selection = self.objects_named(&names)?;
                }
                self.ungroup_selected();
                ok()
            }
            "parent" => {
                if flag(&["-w", "-world"]) {
                    for id in self.targets(&names, name)? {
                        self.scene.reparent(id, None)?;
                    }
                    return ok();
                }
                // Maya: the last name (or last selected) is the parent.
                let ids = self.targets(&names, name)?;
                let (parent, children) = ids.split_last().ok_or("parent: nothing to parent")?;
                if children.is_empty() {
                    return Err("parent: give the children, then the parent".into());
                }
                for &c in children {
                    self.scene.reparent(c, Some(*parent))?;
                }
                ok()
            }
            "xform" => {
                let ids = self.targets(&names, name)?;
                if flag(&["-cp", "-centerPivots"]) {
                    let keep = std::mem::replace(&mut self.selection, ids);
                    self.center_pivot_selected();
                    self.selection = keep;
                    return ok();
                }
                if flag(&["-piv", "-pivots", "-rp", "-rotatePivot"]) && numbers.len() == 3 {
                    let p = Vec3::new(numbers[0], numbers[1], numbers[2]);
                    for id in ids {
                        self.scene.set_pivot_world(id, p);
                    }
                    return ok();
                }
                Err("xform: supported flags are -cp and -piv x y z (world space)".into())
            }
            "setAttr" => {
                let (target, attr) = positional.first().and_then(|p| p.split_once('.')).ok_or("setAttr: use name.attribute value")?;
                let value: f32 = positional.get(1).and_then(|v| v.parse().ok()).ok_or("setAttr: missing number")?;
                let id = self.object_named(target)?;
                let obj = self.scene.get_mut(id).expect("resolved");
                let mut euler = obj.euler_degrees();
                match attr {
                    "translateX" | "tx" => obj.translation.x = value,
                    "translateY" | "ty" => obj.translation.y = value,
                    "translateZ" | "tz" => obj.translation.z = value,
                    "rotateX" | "rx" => euler.x = value,
                    "rotateY" | "ry" => euler.y = value,
                    "rotateZ" | "rz" => euler.z = value,
                    "scaleX" | "sx" => obj.scale.x = value,
                    "scaleY" | "sy" => obj.scale.y = value,
                    "scaleZ" | "sz" => obj.scale.z = value,
                    "visibility" | "v" => obj.visible = value != 0.0,
                    _ => return Err(format!("No attribute '{attr}'")),
                }
                if attr.starts_with('r') {
                    obj.set_euler_degrees(euler);
                }
                ok()
            }
            "ls" if flag(&["-sl", "-selection"]) => Ok(self.selected_names()),
            "ls" => Ok(self.scene.objects.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(" ")),
            "currentTime" => {
                let f = *numbers.first().ok_or("currentTime: missing frame")?;
                self.timeline.frame = f.clamp(self.timeline.anim_start, self.timeline.anim_end);
                Ok(format!("{f}"))
            }
            "undo" => {
                self.undo();
                ok()
            }
            "redo" => {
                self.redo();
                ok()
            }
            "file" if flag(&["-new"]) => {
                // Undoable here (unlike File > New), so a stray command is recoverable.
                self.scene = Scene::default();
                self.selection.clear();
                ok()
            }
            other => Err(format!("Cannot find procedure \"{other}\".")),
        }
    }

    fn object_named(&self, name: &str) -> Result<u64, String> {
        let name = name.trim_matches('"');
        self.scene
            .objects
            .iter()
            .find(|o| o.name == name)
            .map(|o| o.id)
            .ok_or_else(|| format!("No object matches name: {name}"))
    }

    fn objects_named(&self, names: &[&str]) -> Result<Vec<u64>, String> {
        names.iter().map(|n| self.object_named(n)).collect()
    }

    /// Named objects, or the selection when no names are given.
    fn targets(&self, names: &[&str], cmd: &str) -> Result<Vec<u64>, String> {
        if names.is_empty() {
            if self.selection.is_empty() {
                return Err(format!("{cmd}: nothing selected"));
            }
            Ok(self.selection.clone())
        } else {
            self.objects_named(names)
        }
    }

    fn selected_names(&self) -> String {
        self.selection.iter().filter_map(|&id| self.scene.get(id)).map(|o| o.name.as_str()).collect::<Vec<_>>().join(" ")
    }
}
