//! Editor state and the per-frame layout. The Maya-style screen is built from:
//! `chrome` (menu bar, status line, shelf, toolbox), `panels` (outliner,
//! channel box, attribute editor), `viewport` (single or four view),
//! `timeline` (time and range sliders), `command` (MEL command line + help
//! line) and `hotkeys` (rebindable keys + Hotkey Editor).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use egui::{Key, KeyboardShortcut, Modifiers, Rect};
use glam::Vec3;
use transform_gizmo_egui::prelude::*;

use crate::camera::{Cameras, ViewKind};
use crate::mcp::{self, ToolCall, ToolResult};
use crate::mesh::MeshLib;
use crate::render;
use crate::scene::{Scene, Shape};
use hotkeys::{Action, HotkeyEditor, Hotkeys};

mod anim;
mod chrome;
mod cinema;
mod envtools;
mod command;
mod eyes;
mod game;
mod garments;
mod fires;
mod hotkeys;
mod icons;
mod jobs;
mod look;
mod panels;
mod people;
mod posing;
mod probe;
mod theme;
mod timeline;
pub(crate) mod tools;
mod viewport;
#[cfg(test)]
mod speed_checks;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Move,
    Rotate,
    Scale,
}

impl Tool {
    fn help(self) -> &'static str {
        match self {
            Tool::Select => "Select Tool: select an object. Drag a box to select many; Shift toggles, Ctrl removes.",
            Tool::Move => "Move Tool: Use manipulator to move object(s). Hold X to snap to the grid, D to move the pivot.",
            Tool::Rotate => "Rotate Tool: Use manipulator to rotate object(s). Hold X to snap to 15°.",
            Tool::Scale => "Scale Tool: Use manipulator to scale object(s).",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shading {
    Wireframe,
    Shaded,
    WireOnShaded,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightTab {
    ChannelBox,
    AttributeEditor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Orbit,
    Pan,
    Dolly,
    /// Left drag without Alt: a click picks, a drag draws a selection box.
    Click,
}

struct ViewDrag {
    button: egui::PointerButton,
    mode: DragMode,
    start: egui::Pos2,
    moved: bool,
    view: ViewKind,
    rect: Rect,
}

/// Time slider state: the frame clock that rigs' active clips play on.
struct Timeline {
    frame: f32,
    /// Playback range (inner) and animation range (outer), Maya's defaults.
    play_start: f32,
    play_end: f32,
    anim_start: f32,
    anim_end: f32,
    fps: f32,
    /// +1 forward, -1 backward.
    playing: Option<f32>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self { frame: 1.0, play_start: 1.0, play_end: 120.0, anim_start: 1.0, anim_end: 200.0, fps: 24.0, playing: None }
    }
}

pub struct ClideApp {
    scene: Scene,
    /// Selected objects in pick order; the last one is the "lead" (green).
    selection: Vec<u64>,
    cams: Cameras,
    tool: Tool,
    world_space: bool,
    snap_grid: bool,
    /// Modify > Edit Pivot toggle (holding D does the same while held).
    pivot_mode: bool,
    shading: Shading,
    show_grid: bool,
    /// Draw rig skeletons in the viewport.
    show_bones: bool,
    /// One manipulator per view pane; each keeps its own drag state.
    gizmos: Vec<Gizmo>,
    meshes: MeshLib,
    /// GPU handles, for off-screen agent renders.
    render_state: egui_wgpu::RenderState,
    view_drag: Option<ViewDrag>,
    /// The point a scroll zoom goes toward, kept while the mouse stays put
    /// (it stays under the mouse while zooming, and picking it is slow).
    zoom_anchor: Option<(ViewKind, egui::Pos2, f64, Vec3)>,

    // View layout.
    four_view: bool,
    single_view: ViewKind,
    active_view: ViewKind,
    hover_view: Option<ViewKind>,

    // Layout.
    menu_set: usize,
    shelf_tab: usize,
    right_tab: RightTab,
    attr_tab: usize,
    show_outliner: bool,
    show_right_panel: bool,
    outliner_filter: String,
    outliner_collapsed: HashSet<u64>,
    /// Rows being dragged in the Outliner to reparent them.
    outliner_drag: Option<Vec<u64>>,
    /// Last plainly clicked Outliner row, the start of Shift+click ranges.
    outliner_anchor: Option<u64>,
    renaming: Option<(u64, String)>,
    timeline: Timeline,
    cmd_input: String,
    cmd_result: String,
    cmd_python: bool,
    /// Help line text; widgets overwrite it on hover each frame.
    help: String,
    hotkeys: Hotkeys,
    hotkey_editor: HotkeyEditor,

    undo: Vec<Scene>,
    redo: Vec<Scene>,
    /// True while one continuous edit (a drag, a focused text box) is running,
    /// so the whole edit becomes a single undo step.
    edit_session: bool,
    /// Counts undo/redo jumps, so the frame's undo diff does not record a jump
    /// as an edit. (The undo + redo lengths alone do not change on a jump.)
    history_jumps: u64,

    file_path: Option<PathBuf>,
    dirty: bool,
    title: String,
    status: String,

    /// Agent calls from the built-in MCP server (None if it failed to start).
    mcp_rx: Option<Receiver<ToolCall>>,
    mcp_status: String,
    /// (request id, reply, whole window instead of just the viewport)
    pending_shots: Vec<(u64, Sender<ToolResult>, bool)>,
    /// Long agent work running in the background (see `jobs.rs`).
    jobs: jobs::Jobs,
    /// Agent calls waiting for background mesh builds, in arrival order.
    next_shot_id: u64,
    /// Last drawn view area (all panes), in points; agent screenshots crop to it.
    viewport_rect: Rect,
    /// Image the agent is sculpting from (see `eyes.rs`).
    reference: Option<eyes::Reference>,
}

const HOTKEYS_KEY: &str = "hotkeys";

impl ClideApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let meshes = MeshLib::new();
        let rs = cc.wgpu_render_state.as_ref().expect("Clide3D needs the wgpu renderer");
        render::init(rs, meshes.prims());
        theme::apply(&cc.egui_ctx);

        let (mcp_rx, mcp_status) = match mcp::start(cc.egui_ctx.clone()) {
            Ok((rx, url)) => (Some(rx), format!("MCP: {url}")),
            Err(e) => (None, format!("MCP off ({e})")),
        };
        let hotkeys = cc.storage.and_then(|s| eframe::get_value(s, HOTKEYS_KEY)).unwrap_or_default();

        Self {
            // Empty, like Maya: agents build everything themselves.
            scene: Scene::default(),
            selection: Vec::new(),
            cams: Cameras::default(),
            tool: Tool::Move,
            world_space: true,
            snap_grid: false,
            pivot_mode: false,
            shading: Shading::Shaded,
            show_grid: true,
            show_bones: true,
            gizmos: (0..render::SLOTS).map(|_| Gizmo::default()).collect(),
            meshes,
            render_state: rs.clone(),
            view_drag: None,
            zoom_anchor: None,
            four_view: false,
            single_view: ViewKind::Persp,
            active_view: ViewKind::Persp,
            hover_view: None,
            menu_set: 0,
            shelf_tab: 1,
            right_tab: RightTab::ChannelBox,
            attr_tab: 0,
            show_outliner: true,
            show_right_panel: true,
            outliner_filter: String::new(),
            outliner_collapsed: HashSet::new(),
            outliner_drag: None,
            outliner_anchor: None,
            renaming: None,
            timeline: Timeline::default(),
            cmd_input: String::new(),
            cmd_result: String::new(),
            cmd_python: false,
            help: String::new(),
            hotkeys,
            hotkey_editor: HotkeyEditor::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            edit_session: false,
            history_jumps: 0,
            file_path: None,
            dirty: false,
            title: String::new(),
            status: String::new(),
            mcp_rx,
            mcp_status,
            pending_shots: Vec::new(),
            jobs: jobs::Jobs::default(),
            next_shot_id: 0,
            viewport_rect: Rect::NOTHING,
            reference: None,
        }
    }

    // ---------- selection ----------

    fn lead(&self) -> Option<u64> {
        self.selection.last().copied()
    }

    fn is_selected(&self, id: u64) -> bool {
        self.selection.contains(&id)
    }

    fn select_only(&mut self, id: u64) {
        self.selection = vec![id];
    }

    /// Applies Maya's click modifiers to a set of picked objects:
    /// none = replace, Shift (or ⌘) = toggle, Ctrl = remove, Ctrl+Shift = add.
    fn apply_pick(&mut self, picked: &[u64], m: Modifiers) {
        let toggle = m.shift || m.mac_cmd;
        match (toggle, m.ctrl && !m.mac_cmd) {
            (false, false) => self.selection = picked.to_vec(),
            (true, false) => {
                for &id in picked {
                    if let Some(i) = self.selection.iter().position(|&s| s == id) {
                        self.selection.remove(i);
                    } else {
                        self.selection.push(id);
                    }
                }
            }
            (false, true) => self.selection.retain(|id| !picked.contains(id)),
            (true, true) => {
                for &id in picked {
                    self.selection.retain(|&s| s != id);
                    self.selection.push(id);
                }
            }
        }
    }

    /// Drops ids that no longer exist (after undo, delete, ...).
    fn prune_selection(&mut self) {
        let scene = &self.scene;
        self.selection.retain(|&id| scene.get(id).is_some());
    }

    fn select_all(&mut self) {
        self.selection = self.scene.objects.iter().map(|o| o.id).collect();
    }

    fn select_hierarchy(&mut self) {
        let mut all = self.selection.clone();
        for id in self.selection.clone() {
            for d in self.scene.descendants(id) {
                if !all.contains(&d) {
                    all.push(d);
                }
            }
        }
        self.selection = all;
    }

    fn select_inverse(&mut self) {
        let sel = self.selection.clone();
        self.selection = self.scene.objects.iter().map(|o| o.id).filter(|id| !sel.contains(id)).collect();
    }

    // ---------- actions ----------

    fn create(&mut self, shape: Shape) -> u64 {
        let id = self.scene.add(shape);
        self.select_only(id);
        id
    }

    fn delete_selected(&mut self) {
        for id in self.scene.roots_of(&self.selection) {
            self.scene.remove_tree(id);
        }
        self.selection.clear();
    }

    fn duplicate_selected(&mut self) {
        let roots = self.scene.roots_of(&self.selection);
        self.selection = roots.into_iter().filter_map(|id| self.scene.duplicate_tree(id)).collect();
    }

    fn set_selected_visible(&mut self, visible: bool) {
        for &id in &self.selection {
            if let Some(o) = self.scene.get_mut(id) {
                o.visible = visible;
            }
        }
    }

    fn show_all(&mut self) {
        for o in &mut self.scene.objects {
            o.visible = true;
        }
    }

    fn group_selected(&mut self) {
        if self.selection.is_empty() {
            self.status = "Group: select objects first".into();
            return;
        }
        let g = self.scene.group(&self.selection);
        self.select_only(g);
    }

    fn ungroup_selected(&mut self) {
        let groups: Vec<u64> = self.selection.iter().copied().filter(|&id| self.scene.get(id).is_some_and(|o| o.shape == Shape::Group)).collect();
        if groups.is_empty() {
            self.status = "Ungroup: select a group".into();
            return;
        }
        self.selection = groups.into_iter().flat_map(|g| self.scene.ungroup(g)).collect();
    }

    /// Maya's P: the last selected object becomes the parent of the others.
    fn parent_selected(&mut self) {
        let Some(parent) = self.lead() else { return };
        if self.selection.len() < 2 {
            self.status = "Parent: select the children, then the parent last".into();
            return;
        }
        let children: Vec<u64> = self.selection.iter().copied().filter(|&c| c != parent).collect();
        let errors: Vec<String> = children.into_iter().filter_map(|c| self.scene.reparent(c, Some(parent)).err()).collect();
        self.status = errors.first().cloned().unwrap_or_default();
    }

    fn unparent_selected(&mut self) {
        for id in self.selection.clone() {
            let _ = self.scene.reparent(id, None);
        }
    }

    fn mesh_bounds(&self) -> impl Fn(&crate::scene::Object) -> Option<(Vec3, Vec3)> + '_ {
        |o| self.meshes.bounds(o)
    }

    fn center_pivot_selected(&mut self) {
        self.meshes.sync(&self.scene);
        let meshes = &self.meshes;
        for id in self.selection.clone() {
            self.scene.center_pivot(id, &|o| meshes.bounds(o));
        }
    }

    fn reset_selected(&mut self) {
        for &id in &self.selection {
            if let Some(o) = self.scene.get_mut(id) {
                o.translation = Vec3::ZERO;
                o.rotation = glam::Quat::IDENTITY;
                o.scale = Vec3::ONE;
            }
        }
    }

    /// World bounds of the given objects and their children.
    fn world_bounds(&self, ids: &[u64]) -> Option<(Vec3, Vec3)> {
        let bounds = self.mesh_bounds();
        ids.iter()
            .filter(|&&id| self.scene.is_visible(id))
            .filter_map(|&id| {
                self.scene.subtree_bounds(id, self.scene.world_matrix(id), &bounds).or_else(|| {
                    let p = self.scene.pivot_world(id);
                    Some((p, p))
                })
            })
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// The pane keyboard view commands act on: under the mouse, else the active one.
    fn target_view(&self) -> ViewKind {
        self.hover_view.unwrap_or(self.active_view)
    }

    fn frame_ids(&mut self, ids: &[u64]) {
        let view = self.target_view();
        match self.world_bounds(ids) {
            Some((min, max)) => self.cams.frame(view, (min + max) * 0.5, ((max - min).length() * 0.5).max(0.5)),
            None => self.cams.frame(view, Vec3::ZERO, 5.0),
        }
    }

    fn frame_selected(&mut self) {
        let ids = if self.selection.is_empty() { self.scene.children(None) } else { self.selection.clone() };
        self.frame_ids(&ids);
    }

    fn frame_all(&mut self) {
        let roots = self.scene.children(None);
        self.frame_ids(&roots);
    }

    fn toggle_four_view(&mut self) {
        if self.four_view {
            self.single_view = self.target_view();
            self.active_view = self.single_view;
            self.four_view = false;
        } else {
            self.four_view = true;
        }
    }

    /// One undo step per continuous edit; `before` is the scene before it.
    fn record_edit(&mut self, before: Scene) {
        if !self.edit_session {
            self.undo.push(before);
            self.redo.clear();
            self.edit_session = true;
        }
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            let playing = active_clips(&self.scene);
            self.redo.push(std::mem::replace(&mut self.scene, prev));
            set_active_clips(&mut self.scene, &playing);
            self.after_history_jump();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let playing = active_clips(&self.scene);
            self.undo.push(std::mem::replace(&mut self.scene, next));
            set_active_clips(&mut self.scene, &playing);
            self.after_history_jump();
        }
    }

    fn after_history_jump(&mut self) {
        self.history_jumps += 1;
        self.edit_session = false;
        self.prune_selection();
        self.dirty = true;
    }

    fn new_scene(&mut self) {
        self.scene = Scene::default();
        self.selection.clear();
        self.undo.clear();
        self.redo.clear();
        self.file_path = None;
        self.dirty = false;
        self.status = "New scene".into();
    }

    fn open(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Clide3D scene", &["json"]).pick_file() else {
            return;
        };
        match tools::load_scene(&path) {
            Ok(scene) => {
                self.new_scene();
                self.scene = scene;
                self.fit_range_to_clips();
                self.status = format!("Opened {}", path.display());
                self.file_path = Some(path);
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
    }

    fn save(&mut self, save_as: bool) {
        let path = match (&self.file_path, save_as) {
            (Some(p), false) => p.clone(),
            _ => match rfd::FileDialog::new()
                .add_filter("Clide3D scene", &["json"])
                .set_file_name("scene.json")
                .save_file()
            {
                Some(p) => p,
                None => return,
            },
        };
        if let Err(e) = self.save_to(path) {
            self.status = format!("Save failed: {e}");
        }
    }

    /// The frame rigs and bone-attached objects are posed at (None = rest).
    fn set_frame(&mut self, frame: Option<f32>) {
        self.meshes.frame = frame;
        self.scene.frame = frame;
    }

    fn export_dialog(&mut self, ext: &str) {
        let label = if ext == "fbx" { "FBX" } else { "glTF binary" };
        let Some(path) = rfd::FileDialog::new().add_filter(label, &[ext]).set_file_name(format!("model.{ext}")).save_file() else {
            return;
        };
        // Runs as a job like an agent's export; nobody waits on the reply.
        let (reply, _) = std::sync::mpsc::channel();
        let ticket = self.jobs.ticket("Export".into(), reply);
        match self.start_export(ext == "fbx", path, Vec::new(), true, false, &serde_json::json!({})) {
            Ok(jobs::Started::Job(label, work)) => {
                self.spawn_job(label, ticket, work);
            }
            Ok(jobs::Started::Now(_)) => {}
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    fn save_to(&mut self, path: PathBuf) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.scene).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))?;
        self.dirty = false;
        self.status = format!("Saved {}", path.display());
        self.file_path = Some(path);
        Ok(())
    }

    // ---------- input ----------

    /// Undo/redo run before the frame's scene snapshot so they are not
    /// themselves recorded as edits.
    fn history_shortcuts(&mut self, ctx: &egui::Context) {
        let redo = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        let undo = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        if ctx.input_mut(|i| i.consume_shortcut(&redo)) {
            self.redo();
        } else if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
            self.undo();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        if ctx.input_mut(|i| i.consume_shortcut(&cmd_shift(Key::S))) {
            self.save(true);
        } else if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::S))) {
            self.save(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::O))) {
            self.open();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::N))) {
            self.new_scene();
        }
        if self.hotkey_editor.open {
            return;
        }

        let typing = ctx.wants_keyboard_input();
        let mut actions = self.hotkeys.pressed(ctx);
        if !typing && ctx.input(|i| i.modifiers.is_none() && i.key_pressed(Key::Delete)) {
            actions.push(Action::Delete);
        }
        for action in actions {
            // While a text box has focus, only modified keys (Ctrl+G, ...) act.
            let modified = self.hotkeys.get(action).is_some_and(|b| b.ctrl || b.command || b.alt);
            if typing && !modified {
                continue;
            }
            self.run_action(action);
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::SelectTool => self.tool = Tool::Select,
            Action::MoveTool => self.tool = Tool::Move,
            Action::RotateTool => self.tool = Tool::Rotate,
            Action::ScaleTool => self.tool = Tool::Scale,
            Action::Wireframe => self.shading = Shading::Wireframe,
            Action::Shaded => self.shading = Shading::Shaded,
            Action::FrameSelected => self.frame_selected(),
            Action::FrameAll => self.frame_all(),
            Action::Delete => self.delete_selected(),
            Action::Duplicate => self.duplicate_selected(),
            Action::Group => self.group_selected(),
            Action::Parent => self.parent_selected(),
            Action::Unparent => self.unparent_selected(),
            Action::HideSelection => self.set_selected_visible(false),
            Action::ShowSelection => self.set_selected_visible(true),
            Action::ToggleFourView => self.toggle_four_view(),
            Action::PlayToggle => {
                self.timeline.playing = if self.timeline.playing.is_some() { None } else { Some(1.0) };
            }
            Action::GridSnapHold | Action::PivotHold => {}
        }
    }

    /// True while a hold-type hotkey is down (ignored while typing).
    fn holding(&self, ctx: &egui::Context, action: Action) -> bool {
        !ctx.wants_keyboard_input() && !self.hotkey_editor.open && self.hotkeys.held(ctx, action)
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("untitled".to_string(), |n| n.to_string_lossy().into_owned());
        let title = format!("Clide3D — {name}{}", if self.dirty { "*" } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for ClideApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.history_shortcuts(ctx);
        let jumps = self.history_jumps;
        let before = self.scene.clone();

        self.shortcuts(ctx);
        self.tick_playback(ctx);
        self.set_frame(Some(self.timeline.frame));
        self.process_mcp(ctx);
        // Slow mesh builds run on other threads; the old meshes show meanwhile.
        self.meshes.sync_background(&self.scene);
        self.answer_deferred(ctx);
        self.poll_jobs(ctx);
        self.collect_screenshots(ctx);
        self.help = self.tool.help().to_string();

        // Panel order matters in egui: outer panels first.
        let frame = egui::Frame::NONE.fill(theme::PANEL).inner_margin(egui::Margin::symmetric(4, 2));
        egui::TopBottomPanel::top("menu_bar").frame(frame).show(ctx, |ui| self.menu_bar(ui));
        egui::TopBottomPanel::top("status_line").frame(frame).show(ctx, |ui| self.status_line(ui));
        egui::TopBottomPanel::top("shelf").frame(frame).show(ctx, |ui| self.shelf(ui));

        egui::TopBottomPanel::bottom("help_line").frame(frame).show(ctx, |ui| self.help_line(ui));
        if self.working() {
            egui::TopBottomPanel::bottom("jobs").frame(frame).show(ctx, |ui| self.jobs_panel(ui));
        }
        egui::TopBottomPanel::bottom("command_line").frame(frame).show(ctx, |ui| self.command_line(ui));
        egui::TopBottomPanel::bottom("range_slider").frame(frame).show(ctx, |ui| self.range_slider(ui));
        egui::TopBottomPanel::bottom("time_slider").frame(frame).show(ctx, |ui| self.time_slider(ui));

        egui::SidePanel::left("toolbox")
            .exact_width(34.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(theme::PANEL).inner_margin(egui::Margin::symmetric(4, 6)))
            .show(ctx, |ui| self.toolbox(ui));
        if self.show_outliner {
            egui::SidePanel::left("outliner")
                .default_width(210.0)
                .frame(egui::Frame::NONE.fill(theme::PANEL).inner_margin(2))
                .show(ctx, |ui| self.outliner(ui));
        }
        egui::SidePanel::right("vertical_tabs")
            .exact_width(20.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(theme::PANEL))
            .show(ctx, |ui| self.vertical_tabs(ui));
        if self.show_right_panel {
            egui::SidePanel::right("right_panel")
                .default_width(300.0)
                .frame(egui::Frame::NONE.fill(theme::PANEL).inner_margin(4))
                .show(ctx, |ui| match self.right_tab {
                    RightTab::ChannelBox => self.channel_box(ui),
                    RightTab::AttributeEditor => self.attribute_editor(ui),
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::PANEL))
            .show(ctx, |ui| self.viewport_panel(ui));

        self.hotkey_editor.show(ctx, &mut self.hotkeys);
        self.prune_selection();

        // Record one undo step per continuous edit. A menu undo/redo changed the
        // history this frame; it is not an edit.
        let history_jumped = self.history_jumps != jumps;
        // Which clip plays is playback, not an edit: undo skips it.
        let mut before = before;
        // Kitbash parts follow what they sit on; linked mirror copies their
        // sources. Part of the same undo step as the edit that moved them.
        if self.scene != before && !history_jumped {
            crate::pose::sync(&mut self.scene);
        }
        if self.scene != before && !history_jumped {
            let was = active_clips(&before);
            set_active_clips(&mut before, &active_clips(&self.scene));
            if self.scene != before {
                set_active_clips(&mut before, &was);
                self.record_edit(before);
            }
        }
        let editing = ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some());
        if !editing {
            self.edit_session = false;
        }

        self.update_title(ctx);
    }


    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, HOTKEYS_KEY, &self.hotkeys);
    }
}

/// The clip each rig plays, by object id.
fn active_clips(scene: &Scene) -> Vec<(u64, Option<String>)> {
    scene.objects.iter().filter_map(|o| Some((o.id, o.rig.as_ref()?.active.clone()))).collect()
}

/// Plays these clips again (where the rig and the clip still exist).
fn set_active_clips(scene: &mut Scene, playing: &[(u64, Option<String>)]) {
    for (id, clip) in playing {
        if let Some(rig) = scene.get_mut(*id).and_then(|o| o.rig.as_mut())
            && clip.as_ref().is_none_or(|c| rig.clip(c).is_some())
        {
            rig.active = clip.clone();
        }
    }
}
