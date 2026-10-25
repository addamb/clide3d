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

mod alphas;
mod anim;
mod brush;
mod chrome;
mod cinema;
mod envtools;
mod command;
mod eyes;
#[cfg(test)]
pub(crate) use eyes::{BACKGROUND, contact_sheet, scene_callback_of};
mod game;
mod garments;
mod fires;
pub(crate) mod history;
mod hotkeys;
mod hud;
mod importing;
mod icons;
mod jobs;
mod layering;
mod leveling;
mod look;
mod masking;
mod panels;
mod pen_macos;
mod people;
mod posing;
mod probe;
mod quicksave;
mod recorder;
pub(crate) mod theme;
mod timeline;
pub(crate) mod tools;
mod viewport;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod speed_checks;
#[cfg(test)]
mod stage7_tests;
mod strokes;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Move,
    Rotate,
    Scale,
    /// Drag on a clay to record a sculpt stroke (see `recorder.rs`).
    Sculpt,
    /// Drag on a clay to record a paint stroke.
    Paint,
    /// Drag on a clay to paint the sculpt mask (Alt erases, Shift lassos).
    Mask,
    /// Drag on a clay to paint a polygroup.
    Groups,
}

impl Tool {
    fn help(self) -> &'static str {
        match self {
            Tool::Select => "Select Tool: select an object. Drag a box to select many; Shift toggles, Ctrl removes.",
            Tool::Move => "Move Tool: Use manipulator to move object(s). Hold X to snap to the grid, D to move the pivot.",
            Tool::Rotate => "Rotate Tool: Use manipulator to rotate object(s). Hold X to snap to 15°.",
            Tool::Scale => "Scale Tool: Use manipulator to scale object(s).",
            Tool::Sculpt => "Sculpt Tool: drag on a clay to sculpt (the stroke shows as you draw and applies when you let go). Shift smooths, Alt inverts, Ctrl masks, Ctrl+Shift lassos the mask. S, U, O + drag: size, strength, focal; X: symmetry. Right-drag orbits.",
            Tool::Paint => "Paint Tool: drag on a clay to paint with the brush color (Alt erases). Right-drag orbits.",
            Tool::Mask => "Mask Tool: drag on a clay to paint the sculpt mask; Alt erases, Shift draws a lasso. Right-drag orbits.",
            Tool::Groups => "Groups Tool: drag on a clay to paint the HUD's polygroup. Right-drag orbits.",
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
    /// Left drag with the Sculpt or Paint tool: records a stroke.
    Stroke,
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
    /// Key-light shadows in the viewport and its screenshots.
    shadows: render::Shadows,
    /// HDR finishing (bloom, exposure, tonemap) of the viewport and its
    /// screenshots, and the `set_display.post` settings it came from.
    view_look: render::post::ViewLook,
    view_post: serde_json::Value,
    /// What lights the viewport and its screenshots (render_film's
    /// environment and lights), and the `set_display.lighting` it came from.
    view_lighting: std::sync::Arc<crate::view_light::ViewLighting>,
    view_lighting_args: serde_json::Value,
    /// Screen-space ambient occlusion in the viewport and its screenshots.
    ao: render::Ao,
    /// MatCap shading in the viewport and its screenshots (None: off), and
    /// the matcap `set_display shading: matcap` turns on.
    matcap: Option<std::sync::Arc<render::matcap::Matcap>>,
    matcap_choice: std::sync::Arc<render::matcap::Matcap>,
    /// The last render_film's arguments (`set_display lighting: "film"`).
    last_film_args: Option<serde_json::Value>,
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
    /// The Remesh as Clay… dialog, while open.
    remesh_dialog: Option<importing::RemeshDialog>,

    /// Undo and redo, one labelled step per agent call or UI edit.
    history: history::History,
    /// The scene as of the last recorded step this frame: what the next
    /// step is measured against.
    step_base: Scene,
    /// Label for the UI step this frame records (new scene, open ...),
    /// and whether it leaves the scene saved (a new or opened file).
    ui_label: Option<String>,
    ui_clean: bool,
    /// True while one continuous edit (a drag, a focused text box) is running,
    /// so the whole edit becomes a single undo step.
    edit_session: bool,
    /// Counts undo/redo jumps, so the frame's undo diff does not record a jump
    /// as an edit. (The undo + redo lengths alone do not change on a jump.)
    history_jumps: u64,
    /// Counts scene replacements (new, open) and history jumps, after which
    /// an object id may name a different object (ids are reused).
    scene_generation: u64,

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
    /// Ledger replay checkpoints (never saved, never undone).
    replay_cache: std::sync::Arc<crate::ledger::ReplayCache>,
    /// The newest stroke per clay object, for Adjust Last.
    last_deltas: std::collections::HashMap<u64, crate::ledger::LastDelta>,
    /// A gizmo drag on a masked clay: (object, transform at the press,
    /// transform now). The clay does not move while it drags; on release
    /// it sends `sculpt {brush: transform}` (the masked transform).
    masked_drag: Option<(u64, transform_gizmo_egui::math::Transform, transform_gizmo_egui::math::Transform)>,
    quicksave: quicksave::QuickSave,
    /// The Sculpt and Paint tools' brush (`brush_preset get_current`).
    brush: recorder::CurrentBrush,
    /// The stroke being drawn (the button is down).
    capture: Option<recorder::Capture>,
    /// Released strokes waiting to be sent, oldest first.
    strokes: std::collections::VecDeque<recorder::Capture>,
    /// The tool requests the human's strokes sent, newest last.
    ui_activity: std::collections::VecDeque<recorder::UiActivity>,
    /// Who sent the tool call running now (records name it).
    call_source: crate::ledger::Source,
    /// The layer the editor's Sculpt and Paint tools stroke, per clay
    /// (`sculpt_layer select`; editor state, never an undo step).
    layer_selection: std::collections::HashMap<u64, String>,
    /// Strokes sent whose previews show until their requests install.
    lives: Vec<brush::Live>,
    /// Numbers previews across strokes.
    preview_seq: u64,
    /// Milliseconds from a sample to its preview's chunks being ready to
    /// draw (the newest few thousand).
    preview_latency: Vec<f64>,
    /// How long each preview took its worker (ms), the newest few thousand.
    preview_work_ms: Vec<f64>,
    /// Previews checked against their records, and how many differed.
    preview_checks: (usize, usize),
    /// Asks the window to draw again (preview workers).
    repaint: egui::Context,
    /// Draw the brush ring (`set_display brush_cursor`).
    show_brush_cursor: bool,
    /// The HUD's sliders and the brush hotkeys being dragged.
    hud: hud::HudState,
}

const HOTKEYS_KEY: &str = "hotkeys";
/// The actions the saved hotkeys knew (later ones get their defaults).
const HOTKEYS_KNOWN_KEY: &str = "hotkeys_known";
const QUICKSAVE_KEY: &str = "quicksave";
/// The brush tools' macOS tablet monitor (`brush_preset set_current tablet`).
const TABLET_KEY: &str = "tablet";

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
        let hotkeys = match cc.storage.and_then(|s| eframe::get_value::<Hotkeys>(s, HOTKEYS_KEY)) {
            Some(saved) => {
                // Saved before the list was kept: every action but the
                // brush tools' level steps and X symmetry was known.
                let known = cc.storage.and_then(|s| eframe::get_value::<Vec<Action>>(s, HOTKEYS_KNOWN_KEY)).unwrap_or_else(|| Action::ALL.into_iter().filter(|a| !matches!(a, Action::LevelFiner | Action::LevelCoarser | Action::SymmetryX)).collect());
                saved.with_new_defaults(&known)
            }
            None => Hotkeys::default(),
        };
        let quicksave_on = cc.storage.and_then(|s| eframe::get_value(s, QUICKSAVE_KEY)).unwrap_or(true);
        let mut app = Self::with(meshes, rs.clone(), mcp_rx, mcp_status, hotkeys, quicksave_on);
        // The tablet monitor, if it was on (opt-in; the window's thread).
        if cc.storage.and_then(|s| eframe::get_value::<bool>(s, TABLET_KEY)).unwrap_or(false) {
            app.brush.tablet = pen_macos::set_enabled(true).is_ok();
        }
        app
    }

    /// The editor around a render state `render::init` has set up.
    fn with(meshes: MeshLib, render_state: egui_wgpu::RenderState, mcp_rx: Option<Receiver<ToolCall>>, mcp_status: String, hotkeys: Hotkeys, quicksave_on: bool) -> Self {
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
            shadows: render::Shadows::default(),
            view_look: render::post::ViewLook::default(),
            view_post: serde_json::json!({}),
            view_lighting: crate::view_light::ViewLighting::studio_default(),
            view_lighting_args: serde_json::json!("studio"),
            ao: render::Ao::default(),
            matcap: None,
            matcap_choice: render::matcap::Matcap::builtin("clay").expect("built in"),
            last_film_args: None,
            show_bones: true,
            gizmos: (0..render::SLOTS).map(|_| Gizmo::default()).collect(),
            meshes,
            render_state,
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
            remesh_dialog: None,
            history: history::History::default(),
            step_base: Scene::default(),
            ui_label: None,
            ui_clean: false,
            edit_session: false,
            history_jumps: 0,
            scene_generation: 0,
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
            replay_cache: Default::default(),
            last_deltas: Default::default(),
            masked_drag: None,
            quicksave: quicksave::QuickSave::new(quicksave_on),
            brush: Default::default(),
            capture: None,
            strokes: std::collections::VecDeque::new(),
            ui_activity: Default::default(),
            call_source: crate::ledger::Source::Agent,
            layer_selection: Default::default(),
            lives: Vec::new(),
            preview_seq: 0,
            preview_latency: Vec::new(),
            preview_work_ms: Vec::new(),
            preview_checks: (0, 0),
            repaint: egui::Context::default(),
            show_brush_cursor: true,
            hud: Default::default(),
        }
    }

    /// A windowless editor for tests: agent calls arrive on the returned
    /// sender as if from the MCP server. QuickSave is off.
    #[cfg(test)]
    pub(crate) fn headless(rs: egui_wgpu::RenderState) -> (Self, std::sync::mpsc::Sender<ToolCall>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self::with(MeshLib::new(), rs, Some(rx), "MCP: test".into(), Hotkeys::default(), false), tx)
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

    /// Drops ids that no longer exist (after undo, delete, ...), and
    /// layer selections the scene no longer has (`prune_layer_selection`).
    fn prune_selection(&mut self) {
        let scene = &self.scene;
        self.selection.retain(|&id| scene.get(id).is_some());
        prune_layer_selection(&mut self.layer_selection, scene);
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
            let label = self.ui_label.take().unwrap_or_else(|| "edit [ui]".into());
            self.history.push(before, &self.scene, history::StepInfo::new(label, "ui", Vec::new()));
            self.edit_session = true;
        }
        self.ui_label = None;
        self.dirty = !std::mem::take(&mut self.ui_clean);
        self.quicksave.pending = true;
    }

    /// Records the UI's changes since the last step (before an agent call
    /// runs, so the call's step holds only the call's changes).
    fn flush_ui_step(&mut self) {
        if edited(&self.step_base, &self.scene) {
            crate::pose::sync(&mut self.scene);
            let before = std::mem::take(&mut self.step_base);
            self.record_edit(before);
        }
        self.step_base = self.scene.clone();
    }

    /// Records what a tool call (or a job's finisher) changed as one
    /// step labelled `label`. `jumps` is `history_jumps` before the call:
    /// undo, redo and history goto calls are jumps, not edits. A UI
    /// stroke's request is a call too, from `Source::Ui`.
    fn agent_step(&mut self, tool: &str, label: String, jumps: u64, source: crate::ledger::Source) {
        if self.history_jumps == jumps && edited(&self.step_base, &self.scene) {
            crate::pose::sync(&mut self.scene);
            let before = std::mem::take(&mut self.step_base);
            let ui_call = source == crate::ledger::Source::Ui;
            let source = if ui_call { "ui" } else if self.edit_session { "agent+ui" } else { "agent" };
            let label = if self.edit_session && !ui_call { label.replace("[agent]", "[agent+ui]") } else { label };
            self.history.push(before, &self.scene, history::StepInfo::new(label, source, vec![tool.to_string()]));
            // A UI drag going on goes on as a step of its own.
            self.edit_session = false;
            self.dirty = true;
            self.quicksave.pending = true;
        }
        self.step_base = self.scene.clone();
    }

    fn undo(&mut self) {
        let playing = active_clips(&self.scene);
        let levels = leveling::active_levels(&self.scene);
        if self.history.undo(&mut self.scene) {
            set_active_clips(&mut self.scene, &playing);
            leveling::set_active_levels(&mut self.scene, &levels);
            self.after_history_jump();
        }
    }

    fn redo(&mut self) {
        let playing = active_clips(&self.scene);
        let levels = leveling::active_levels(&self.scene);
        if self.history.redo(&mut self.scene) {
            set_active_clips(&mut self.scene, &playing);
            leveling::set_active_levels(&mut self.scene, &levels);
            self.after_history_jump();
        }
    }

    /// Undoes or redoes to snapshot `n` (see `history.rs`).
    fn history_goto(&mut self, n: u64) -> Result<(), String> {
        let playing = active_clips(&self.scene);
        let levels = leveling::active_levels(&self.scene);
        self.history.goto(n, &mut self.scene)?;
        set_active_clips(&mut self.scene, &playing);
        leveling::set_active_levels(&mut self.scene, &levels);
        self.after_history_jump();
        Ok(())
    }

    fn after_history_jump(&mut self) {
        self.history_jumps += 1;
        self.scene_generation += 1;
        self.edit_session = false;
        self.prune_selection();
        self.dirty = true;
        self.quicksave.pending = true;
        self.step_base = self.scene.clone();
    }

    /// Swaps in a whole new scene (new, open). Ids in it may name other
    /// objects than before.
    fn replace_scene(&mut self, scene: Scene) {
        self.scene = scene;
        self.scene_generation += 1;
        // Its ids name other objects: no layer stays selected.
        self.layer_selection.clear();
    }

    /// File > New: an undoable step, like the agent's new_scene.
    fn new_scene(&mut self) {
        self.replace_scene(Scene::default());
        self.selection.clear();
        self.ui_label = Some("new scene [ui]".into());
        self.ui_clean = true;
        self.file_path = None;
        self.dirty = false;
        self.status = "New scene".into();
    }

    /// File > Open: an undoable step, like the agent's open_scene.
    fn open(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Clide3D scene", &["json"]).pick_file() else {
            return;
        };
        match tools::load_scene(&path) {
            Ok(scene) => {
                self.replace_scene(scene);
                self.selection.clear();
                self.ui_label = Some(format!("open {} [ui]", path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())));
                self.ui_clean = true;
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
        write_scene(&self.scene, &path)?;
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
            // Over a view, with a brush tool (`brush_keys` has the rest).
            Action::LevelFiner | Action::LevelCoarser => {
                if self.stroke_tool() && self.hover_view.is_some() {
                    self.step_level(if action == Action::LevelFiner { 1 } else { -1 });
                }
            }
            Action::SymmetryX => {
                if self.stroke_tool() && self.hover_view.is_some() {
                    self.toggle_symmetry_x();
                }
            }
            Action::GridSnapHold | Action::PivotHold => {}
        }
    }

    /// True while a hold-type hotkey is down (ignored while typing).
    fn holding(&self, ctx: &egui::Context, action: Action) -> bool {
        !ctx.wants_keyboard_input() && !self.hotkey_editor.open && self.hotkeys.held(ctx, action)
    }

    /// True while Edit Pivot is held. Brush tools have no manipulator, and
    /// their D steps the detail level instead.
    fn pivot_held(&self, ctx: &egui::Context) -> bool {
        !self.stroke_tool() && self.holding(ctx, Action::PivotHold)
    }

    /// True while Snap to Grid is held; brush tools' X toggles symmetry.
    fn snap_held(&self, ctx: &egui::Context) -> bool {
        !self.stroke_tool() && self.holding(ctx, Action::GridSnapHold)
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
        self.frame(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, HOTKEYS_KEY, &self.hotkeys);
        eframe::set_value(storage, HOTKEYS_KNOWN_KEY, &Action::ALL.to_vec());
        eframe::set_value(storage, QUICKSAVE_KEY, &self.quicksave.enabled);
        eframe::set_value(storage, TABLET_KEY, &self.brush.tablet);
    }
}

impl ClideApp {
    /// One frame of the editor (tests drive it with synthetic input).
    fn frame(&mut self, ctx: &egui::Context) {
        self.repaint = ctx.clone();
        self.history_shortcuts(ctx);
        let jumps = self.history_jumps;
        self.step_base = self.scene.clone();

        self.shortcuts(ctx);
        self.tick_playback(ctx);
        self.set_frame(Some(self.timeline.frame));
        self.brush_keys(ctx);
        // A finished UI stroke joins the line of tool calls.
        self.send_stroke(ctx);
        self.process_mcp(ctx);
        self.tick_previews();
        self.tick_hud();
        // A sculpt tool on a clay keeps its sculpt session (chunks) going,
        // as does a stroke still previewing on it.
        let previewing = self.capture.iter().chain(&self.strokes).map(|c| c.target).chain(self.lives.iter().map(|l| l.target));
        let tool_on: Vec<u64> = if self.stroke_tool() { self.selection.iter().copied().chain(previewing).collect() } else { previewing.collect() };
        self.meshes.set_sculpt_tool(&tool_on);
        // The clays a brush tool is on get their picking trees ready for
        // the stroke's samples.
        for id in &tool_on {
            self.meshes.warm_bvh(*id);
        }
        // Slow mesh builds run on other threads; the old meshes show meanwhile.
        self.meshes.sync_background(&self.scene);
        // A sculpt session's chunks show as soon as they are built.
        if self.meshes.chunks_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.answer_deferred(ctx);
        self.poll_jobs(ctx);
        self.report_ui_activity();
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
        self.remesh_dialog_window(ctx);
        self.prune_selection();

        // Record one undo step per continuous edit (agent calls recorded
        // their own steps as they ran). A menu undo/redo changed the
        // history this frame; it is not an edit.
        let history_jumped = self.history_jumps != jumps;
        // Kitbash parts follow what they sit on; linked mirror copies their
        // sources. Part of the same undo step as the edit that moved them.
        if !history_jumped && edited(&self.step_base, &self.scene) {
            crate::pose::sync(&mut self.scene);
            let before = std::mem::take(&mut self.step_base);
            self.record_edit(before);
        }
        // A New or Open that changed nothing labels no later edit.
        (self.ui_label, self.ui_clean) = (None, false);
        let editing = ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some());
        if !editing {
            self.edit_session = false;
        }
        // QuickSave backs up unsaved changes only.
        if self.dirty {
            self.quicksave.tick(&self.scene);
            if let Some(wait) = self.quicksave.due_in() {
                ctx.request_repaint_after(wait.max(std::time::Duration::from_secs(1)));
            }
        }

        self.update_title(ctx);
    }
}

/// Writes a scene file (Save, Save As, save_scene and QuickSave).
pub(crate) fn write_scene(scene: &Scene, path: &std::path::Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(scene).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether `after` differs from `before` by an edit. Which clip plays is
/// playback, not an edit: undo skips it.
/// Keeps the editor's layer selections (`sculpt_layer select`) the scene
/// still has: the object is a clay with a layer of that name. An undo of
/// a rename or an add takes the name away, so a stroke is never sent to a
/// layer other than the one shown as selected (or quietly to base).
fn prune_layer_selection(selection: &mut std::collections::HashMap<u64, String>, scene: &Scene) {
    selection.retain(|id, name| scene.get(*id).and_then(|o| o.clay.as_ref()).is_some_and(|c| crate::layers::kind_of(c, name).is_ok()));
}

fn edited(before: &Scene, after: &Scene) -> bool {
    if before == after {
        return false;
    }
    let playing = active_clips(after);
    if playing == active_clips(before) {
        return true;
    }
    let mut b = before.clone();
    set_active_clips(&mut b, &playing);
    b != *after
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
