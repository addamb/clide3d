//! The agent's eyes: a reference image, off-screen renders from any angle,
//! and a silhouette comparison of the model against the reference.
//!
//! The comparison lines the model up with the reference automatically: it
//! scales an orthographic camera so the model's height on screen matches the
//! subject's height in the image, and centers them on each other. It then
//! reports how well the outlines overlap and where they differ, plus a
//! pixel-to-world mapping so the agent can turn image features into
//! coordinates.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use crate::camera::ViewKind;
use crate::mcp::ToolResult;
use crate::mesh::MeshLib;
use crate::render::{self, Instance, ViewportCallback};

/// Reference images are scaled down to this on the long side when loaded.
const REF_MAX: u32 = 1024;
/// Comparison images are at most this on the long side.
const COMPARE_MAX: usize = 512;
pub(crate) const BACKGROUND: [u8; 3] = crate::refplane::BACKGROUND;

/// The view a reference image shows (D13): one image per slot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Slot {
    Front,
    Back,
    Left,
    Right,
    Top,
    ThreeQuarter,
}

pub(super) const SLOT_NAMES: &str = "front, back, left, right (or side), top, three_quarter";

impl Slot {
    pub(super) fn parse(s: &str) -> Option<Slot> {
        Some(match s {
            "front" => Slot::Front,
            "back" => Slot::Back,
            "left" => Slot::Left,
            "right" | "side" => Slot::Right,
            "top" => Slot::Top,
            "three_quarter" => Slot::ThreeQuarter,
            _ => return None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Slot::Front => "front",
            Slot::Back => "back",
            Slot::Left => "left",
            Slot::Right => "right",
            Slot::Top => "top",
            Slot::ThreeQuarter => "three_quarter",
        }
    }

    /// Toward the camera of the view named like the slot.
    pub(super) fn camera_dir(self) -> Vec3 {
        let (yaw, pitch, _) = named_view(self.name()).expect("every slot names a view");
        direction(yaw, pitch)
    }

    /// The viewport pane that shows the slot's image as an overlay by
    /// default (none for views no pane looks from).
    fn pane(self) -> Option<ViewKind> {
        match self {
            Slot::Front => Some(ViewKind::Front),
            Slot::Left | Slot::Right => Some(ViewKind::Side),
            Slot::Top => Some(ViewKind::Top),
            Slot::Back | Slot::ThreeQuarter => None,
        }
    }
}

/// The loaded reference images, by slot.
#[derive(Default)]
pub(super) struct References {
    slots: std::collections::BTreeMap<Slot, Reference>,
    /// Slots a plane's view took over when undo or redo brought the plane
    /// back (a single image, or a sheet's view loaded without planes):
    /// they come back when the plane goes again, so redo restores them.
    stash: std::collections::BTreeMap<Slot, Reference>,
}

impl References {
    pub(super) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub(super) fn get(&self, slot: Slot) -> Option<&Reference> {
        self.slots.get(&slot)
    }

    /// The loaded slots, in order.
    pub(super) fn loaded(&self) -> Vec<Slot> {
        self.slots.keys().copied().collect()
    }

    /// Puts `r` in its slot (replacing what was there): a handler's load,
    /// which also forgets what the slot held before a plane took it.
    pub(super) fn insert(&mut self, r: Reference) {
        self.stash.remove(&r.slot);
        self.slots.insert(r.slot, r);
    }

    /// Puts a plane's view in its slot; a slot not from a plane that it
    /// takes over is kept aside until the plane goes (`drop_planes`).
    pub(super) fn insert_from_plane(&mut self, r: Reference) {
        if let Some(old) = self.slots.insert(r.slot, r)
            && !old.from_plane()
        {
            self.stash.insert(old.slot, old);
        }
    }

    /// Removes the slots that came from the scene's reference planes; a
    /// slot one had taken over comes back.
    pub(super) fn drop_planes(&mut self) {
        let gone: Vec<Slot> = self.slots.iter().filter(|(_, r)| r.from_plane()).map(|(s, _)| *s).collect();
        for s in gone {
            self.slots.remove(&s);
            if let Some(r) = self.stash.remove(&s) {
                self.slots.insert(s, r);
            }
        }
    }

    /// Removes a slot (`set_reference clear`), and what a plane took over in it.
    fn remove(&mut self, slot: Slot) -> Option<Reference> {
        self.stash.remove(&slot);
        self.slots.remove(&slot)
    }

    fn clear(&mut self) {
        self.stash.clear();
        self.slots.clear();
    }

    fn names(&self) -> Vec<&'static str> {
        self.slots.keys().map(|s| s.name()).collect()
    }

    /// The reference a view compares against: the view's `slot`, else the
    /// slot named like the view, else the only one loaded (one image is
    /// compared from any view, as before slots).
    pub(super) fn slot_for(&self, view: &Value) -> Result<Slot, String> {
        if self.slots.is_empty() {
            return Err("no reference image; call set_reference first".into());
        }
        if let Some(s) = view.get("slot").filter(|s| !s.is_null()) {
            let name = s.as_str().ok_or("a view's slot must be a slot name")?;
            let slot = Slot::parse(name).ok_or(format!("unknown slot {name}; use {SLOT_NAMES}"))?;
            return self.slots.contains_key(&slot).then_some(slot).ok_or(format!("no reference in slot {name}; loaded: {}", self.names().join(", ")));
        }
        let name = view.as_str().or_else(|| view.get("view").and_then(Value::as_str));
        if let Some(slot) = name.and_then(Slot::parse).filter(|s| self.slots.contains_key(s)) {
            return Ok(slot);
        }
        match self.loaded().as_slice() {
            [only] => Ok(*only),
            _ => Err(format!("no reference for view {}; loaded: {} (name one, or pass {{view, slot}})", name.unwrap_or("given"), self.names().join(", "))),
        }
    }
}

pub(super) struct Reference {
    pub image: egui::ColorImage,
    pub texture: Option<egui::TextureHandle>,
    /// The view it shows.
    pub slot: Slot,
    /// The viewport pane that shows it as an overlay (None: no pane).
    pub view: Option<ViewKind>,
    pub opacity: f32,
    pub show: bool,
    /// Which pixels belong to the subject (not the background).
    mask: Vec<bool>,
    /// Subject bounds in pixels: min x, min y, max x, max y (inclusive).
    bbox: [usize; 4],
    path: PathBuf,
    /// A turnaround sheet's view (Stage 5a): the shared scale and ground
    /// line it is lined up by (`align`). None for a single image, which
    /// keeps the per-view fit.
    pub sheet: Option<SheetFit>,
}

/// How a sheet's view lines up (`app/sheet.rs`): one scale for every view
/// of the sheet, and the view's ground line at a world height.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SheetFit {
    /// Reference pixels per world unit.
    pub px_per_unit: f32,
    /// The reference row edge of the ground line.
    pub ground_row: f32,
    /// The world height of the ground line.
    pub ground_y: f32,
    /// The reference plane showing it, if any.
    pub plane: Option<u64>,
}

impl Reference {
    /// A sheet's view whose slot follows a reference plane in the scene.
    pub(super) fn from_plane(&self) -> bool {
        self.sheet.as_ref().is_some_and(|f| f.plane.is_some())
    }

    pub(super) fn new(image: egui::ColorImage, slot: Slot, mask: Vec<bool>, fallback_box: [usize; 4], path: PathBuf, sheet: Option<SheetFit>) -> Reference {
        let bbox = mask_bbox(&mask, image.size[0]).unwrap_or(fallback_box);
        Reference { image, texture: None, slot, view: slot.pane(), opacity: 0.5, show: true, mask, bbox, path, sheet }
    }

    /// Subject bounds in pixels: min x, min y, max x, max y (inclusive).
    pub(super) fn bbox(&self) -> [usize; 4] {
        self.bbox
    }

    /// Which pixels belong to the subject (row by row, the image's width).
    pub(super) fn mask(&self) -> &[bool] {
        &self.mask
    }
}

/// A camera for one off-screen render.
/// The camera and mapping that line the model up with the reference.
struct Align {
    view_name: String,
    slot: Slot,
    /// Toward the camera.
    dir: Vec3,
    right: Vec3,
    up: Vec3,
    /// Pixels per world unit.
    s: f32,
    /// Screen-axis world coordinates at the panel center.
    uc: f32,
    vc: f32,
    w: usize,
    h: usize,
    /// Panel pixels per reference pixel.
    k: f32,
    shot: Shot,
    ref_mask: Vec<bool>,
    bbox: [f32; 4],
    /// The subject's height on the panel (pixels).
    subject_px: f32,
    plane_center: Vec3,
}

impl Align {
    /// World point -> (panel x, panel y, depth toward the camera).
    fn project(&self, p: Vec3) -> (f32, f32, f32) {
        let x = (p.dot(self.right) - self.uc) * self.s + self.w as f32 * 0.5;
        let y = (self.vc - p.dot(self.up)) * self.s + self.h as f32 * 0.5;
        (x, y, p.dot(self.dir))
    }
}

pub(super) struct Shot {
    pub eye: Vec3,
    target: Vec3,
    up: Vec3,
    /// Some(half height) for orthographic, None for perspective.
    ortho: Option<f32>,
    /// Vertical field of view in degrees (perspective).
    fov: f32,
    pub name: String,
}

impl Shot {
    /// A camera looking at `center` from `view`, framing `radius`.
    pub(super) fn framing(view: &Value, center: Vec3, radius: f32) -> Result<Shot, String> {
        let (name, dir, ortho, fov) = parse_view(view)?;
        // A view may frame its own point: {view or yaw/pitch, center, radius}.
        let center = match view.get("center").filter(|c| !c.is_null()) {
            Some(c) => Vec3::from(serde_json::from_value::<[f32; 3]>(c.clone()).map_err(|_| "a view's center must be [x, y, z]")?),
            None => center,
        };
        let radius = view.get("radius").and_then(Value::as_f64).map_or(radius, |r| (r as f32).clamp(0.01, 1000.0));
        let r = radius * 1.08;
        Ok(Shot {
            eye: center + dir * if ortho { r * 4.0 } else { r / (fov * 0.5).to_radians().sin() },
            target: center,
            up: up_for(dir),
            ortho: ortho.then_some(r),
            fov,
            name: format!("{name}{}", if ortho { " (ortho)" } else { "" }),
        })
    }

    /// The camera for the film renderer.
    pub(super) fn film_camera(&self) -> crate::film::Camera {
        crate::film::Camera { eye: self.eye, target: self.target, up: self.up, fov_deg: self.fov, ortho: self.ortho }
    }

    pub(super) fn view_proj(&self, w: usize, h: usize) -> Mat4 {
        self.proj(w as f32 / h as f32) * self.view()
    }

    fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye, self.target, self.up)
    }

    fn proj(&self, aspect: f32) -> Mat4 {
        let far = (self.eye - self.target).length() * 3.0 + 10.0;
        match self.ortho {
            Some(h) => Mat4::orthographic_rh(-h * aspect, h * aspect, -h, h, 0.01, far),
            None => Mat4::perspective_rh(self.fov.to_radians(), aspect, 0.05, far),
        }
    }
}

const FOV: f32 = 30.0;

/// Extra things an off-screen render can show.
#[derive(Default, Clone)]
pub(super) struct Extras {
    /// Draw the skeletons on top.
    pub bones: bool,
    /// (object, bone): color that object by the bone's skin weights.
    pub weights: Option<(u64, usize)>,
    /// Lighting and ambient occlusion for this render only (else the
    /// viewport's).
    pub lighting: Option<std::sync::Arc<crate::view_light::ViewLighting>>,
    pub ao: Option<crate::render::Ao>,
    /// A quality shade for this render only (`render_views shade`).
    pub shade: crate::quality::Shade,
    /// Draw just these objects (None: everything).
    pub only: Option<std::sync::Arc<HashSet<u64>>>,
    /// Draw the reference image planes (`render_views`; never in masks or
    /// with `only`).
    pub references: bool,
    /// Draw the proportion guides through this framed (centre, radius)
    /// after the render (`render_views guides`; never in masks).
    pub guides: Option<(Vec3, f32)>,
}

/// Direction from the target to the camera, yaw/pitch in degrees.
/// Yaw 0 looks at the model's front (camera on +Z), 90 is the camera on +X.
fn direction(yaw: f32, pitch: f32) -> Vec3 {
    let (y, p) = (yaw.to_radians(), pitch.clamp(-90.0, 90.0).to_radians());
    Vec3::new(p.cos() * y.sin(), p.sin(), p.cos() * y.cos())
}

/// The Diablo-style view: high three-quarter, long lens.
pub(super) const ISO: (f32, f32, f32) = (45.0, 52.0, 18.0);

/// Named views: (yaw, pitch, orthographic by default).
fn named_view(name: &str) -> Option<(f32, f32, bool)> {
    Some(match name {
        "iso" => (ISO.0, ISO.1, false),
        "front" => (0.0, 0.0, true),
        "back" => (180.0, 0.0, true),
        "right" | "side" => (90.0, 0.0, true),
        "left" => (-90.0, 0.0, true),
        "top" => (0.0, 90.0, true),
        "bottom" => (0.0, -90.0, true),
        "three_quarter" => (35.0, 20.0, false),
        "three_quarter_back" => (145.0, 20.0, false),
        _ => return None,
    })
}

pub(super) const VIEW_NAMES: &str = "front, back, right (camera on +X, Maya's side view), left, top, bottom, three_quarter, three_quarter_back, iso, or {dir: [x, y, z]}";

/// Parses `"front"` or `{"yaw": 30, "pitch": 10, "ortho": false, "fov": 20}`;
/// `{"view": "iso", "yaw": 135}` turns a named view. Returns (name,
/// direction toward the camera, orthographic, vertical field of view).
fn parse_view(v: &Value) -> Result<(String, Vec3, bool, f32), String> {
    let named = |s: &str| named_view(s).ok_or_else(|| format!("unknown view {s}; use {VIEW_NAMES}, or {{yaw, pitch}}"));
    let (name, mut yaw, mut pitch, ortho) = match v {
        Value::String(s) => {
            let (y, p, o) = named(s)?;
            (s.clone(), y, p, o)
        }
        Value::Object(_) if v.get("view").is_some_and(Value::is_string) => {
            let s = v["view"].as_str().expect("checked");
            let (y, p, o) = named(s)?;
            (s.to_string(), y, p, o)
        }
        Value::Object(_) if v.get("dir").is_some_and(|d| !d.is_null()) => {
            let d: [f32; 3] = serde_json::from_value(v["dir"].clone()).map_err(|_| "dir must be [x, y, z] (from the model toward the camera)")?;
            let dir = Vec3::from(d);
            if !dir.is_finite() || dir.length_squared() < 1e-12 {
                return Err("dir must be a finite, nonzero [x, y, z]".into());
            }
            let ortho = v.get("ortho").and_then(Value::as_bool).unwrap_or(true);
            let fov = v.get("fov").and_then(Value::as_f64).map_or(FOV, |f| (f as f32).clamp(1.0, 150.0));
            let n = dir.normalize();
            return Ok((format!("dir [{:.3}, {:.3}, {:.3}]", n.x, n.y, n.z), n, ortho, fov));
        }
        Value::Object(_) => {
            let yaw = v["yaw"].as_f64().unwrap_or(0.0) as f32;
            let pitch = v["pitch"].as_f64().unwrap_or(0.0) as f32;
            (format!("yaw {yaw} pitch {pitch}"), yaw, pitch, false)
        }
        _ => return Err("a view is a name, {yaw, pitch}, {dir: [x, y, z]} or {view: name}; each object may add center and radius".into()),
    };
    if v.get("view").is_some() {
        yaw = v["yaw"].as_f64().map_or(yaw, |y| y as f32);
        pitch = v["pitch"].as_f64().map_or(pitch, |p| p as f32);
    }
    let ortho = v.get("ortho").and_then(Value::as_bool).unwrap_or(ortho);
    let named_fov = if name == "iso" || v.get("view").and_then(Value::as_str) == Some("iso") { ISO.2 } else { FOV };
    let fov = v.get("fov").and_then(Value::as_f64).map_or(named_fov, |f| (f as f32).clamp(1.0, 150.0));
    if !yaw.is_finite() || !pitch.is_finite() {
        return Err("yaw and pitch must be finite".into());
    }
    Ok((name, direction(yaw, pitch), ortho, fov))
}

fn up_for(dir: Vec3) -> Vec3 {
    // Looking straight down or up: use Maya's top-view up (-Z) instead of Y.
    if dir.y.abs() > 0.999 { Vec3::new(0.0, 0.0, -dir.y.signum()) } else { Vec3::Y }
}

impl ClideApp {
    /// World bounds of everything visible, as (center, radius).
    pub(super) fn scene_sphere(&self) -> (Vec3, f32) {
        // Planes are left out at any depth (`world_bounds_of_roots`); objects
        // parented under a plane still count.
        let roots = self.scene.children(None);
        match self.world_bounds_of_roots(&roots) {
            Some((lo, hi)) => ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.05)),
            None => (Vec3::ZERO, 1.0),
        }
    }

    fn render_shot(&self, shot: &Shot, w: usize, h: usize, mask: bool, grid: bool) -> Result<egui::ColorImage, String> {
        self.render_shot_extras(shot, w, h, mask, grid, Extras::default())
    }

    pub(super) fn render_shot_extras(&self, shot: &Shot, w: usize, h: usize, mask: bool, grid: bool, ex: Extras) -> Result<egui::ColorImage, String> {
        let shaded = if mask { Default::default() } else { self.shaded_meshes(ex.shade) };
        self.render_shaded(shot, w, h, mask, grid, ex, &shaded)
    }

    /// A beauty render with some objects drawn with their own colored
    /// meshes (`quality_report images`).
    pub(super) fn render_with(&self, shot: &Shot, w: usize, h: usize, shaded: &std::collections::HashMap<u64, std::sync::Arc<crate::mesh::Mesh>>) -> Result<egui::ColorImage, String> {
        self.render_shaded(shot, w, h, false, false, Extras::default(), shaded)
    }

    /// Front and right views of object `id` drawn with `mesh` and its
    /// wireframe, framed on it (the `retopo` reply's image).
    pub(super) fn wire_sheet(&self, id: u64, mesh: std::sync::Arc<crate::mesh::Mesh>) -> Result<Vec<Value>, String> {
        let world = self.scene.world_matrix(id);
        if mesh.vertices.is_empty() {
            return Err("the mesh is empty".into());
        }
        let (lo, hi) = mesh.bounds();
        let corners = (0..8).map(|i| world.transform_point3(Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][i >> 1 & 1], [lo.z, hi.z][i >> 2])));
        let (a, b) = corners.fold((Vec3::MAX, Vec3::MIN), |(a, b), p| (a.min(p), b.max(p)));
        let (center, radius) = ((a + b) * 0.5, ((b - a).length() * 0.5).max(0.01));
        let only: std::sync::Arc<HashSet<u64>> = std::sync::Arc::new([id].into_iter().collect());
        let shaded = std::collections::HashMap::from([(id, mesh)]);
        let mut tiles = Vec::new();
        for view in ["front", "right"] {
            let shot = Shot::framing(&json!(view), center, radius)?;
            let ex = Extras { shade: crate::quality::Shade::Wire, only: Some(only.clone()), ..Extras::default() };
            tiles.push(self.render_shaded(&shot, 384, 384, false, false, ex, &shaded)?);
        }
        super::tools::encode_png(&contact_sheet(&tiles, 2))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_shaded(&self, shot: &Shot, w: usize, h: usize, mask: bool, grid: bool, ex: Extras, shaded: &std::collections::HashMap<u64, std::sync::Arc<crate::mesh::Mesh>>) -> Result<egui::ColorImage, String> {
        let view_proj = shot.view_proj(w, h);
        // Off-screen renders draw their own shade (`ex.shade`), never the
        // viewport's.
        let mut cb = scene_callback_with(&self.scene, &self.meshes, view_proj, shot.eye, mask, grid, shaded, ex.shade == crate::quality::Shade::Wire && !mask, ex.only.as_deref(), false, ex.references && !mask && ex.only.is_none());
        cb.shadows = self.shadows;
        cb.post = self.view_look;
        cb.lighting = ex.lighting.clone().unwrap_or_else(|| self.view_lighting.clone());
        cb.ao = ex.ao.unwrap_or(self.ao);
        cb.matcap = self.matcap.clone();
        // Weight view: one bone's skin weights as colors, on a white object.
        if let Some((id, bone)) = ex.weights
            && let Some(o) = self.scene.get(id)
            && let (Some(shown), Some((_, Some(skin)))) = (self.meshes.get(o), self.meshes.rest(id))
        {
            let mut m = crate::mesh::Mesh { vertices: shown.vertices.clone(), indices: shown.indices.clone(), edges: Vec::new(), materials: shown.materials.clone(), textures: None, imported: None, decals: shown.decals.clone(), face: shown.face.clone(), unshaded: None };
            crate::rig::weight_colors(&mut m, skin, bone);
            let key = crate::mesh::MeshKey::Own(id);
            let m = std::sync::Arc::new(m);
            for (k, mesh) in &mut cb.dynamic {
                if *k == key {
                    *mesh = m.clone();
                }
            }
            for (k, inst) in &mut cb.fills {
                if *k == key {
                    *inst = inst.with_color([1.0; 3]);
                }
            }
        }
        let mut img = render::render_image(&self.render_state, &cb, w as u32, h as u32, if mask { [0, 0, 0] } else { BACKGROUND })?;
        if ex.bones {
            self.draw_bones(&mut img, view_proj);
        }
        if let Some((center, radius)) = ex.guides.filter(|_| !mask) {
            self.draw_guides_on(&mut img, view_proj, shot.target - shot.eye, center, radius);
        }
        Ok(img)
    }

    /// Each clay's drawn mesh colored by a quality shade, measured on its
    /// field (none for shades that keep the colors).
    fn shaded_meshes(&self, shade: crate::quality::Shade) -> std::collections::HashMap<u64, std::sync::Arc<crate::mesh::Mesh>> {
        let mut out = std::collections::HashMap::new();
        if !shade.colors() {
            return out;
        }
        #[cfg(test)]
        self.shade_measures.set(self.shade_measures.get() + 1);
        for o in self.scene.objects.iter().filter(|o| o.shape == crate::scene::Shape::Clay) {
            let (Some(clay), Some(unposed), Some(drawn)) = (o.clay.as_ref(), self.meshes.unposed(o.id), self.meshes.get(o)) else { continue };
            let Some(colors) = crate::quality::shade_colors(shade, clay, unposed, crate::quality::DEFAULT_SAMPLES) else { continue };
            let mut m = (**drawn).clone();
            for (v, c) in m.vertices.iter_mut().zip(colors) {
                v.color = c;
            }
            out.insert(o.id, std::sync::Arc::new(m));
        }
        out
    }

    /// `set_reference` tool: one image per view slot (D13). `view` names
    /// the slot (default front); the panes it used to name still work
    /// (side is the right slot, persp the front slot shown on the
    /// perspective pane). `overlay` picks the pane that shows it.
    pub(super) fn set_reference(&mut self, args: &Value) -> ToolResult {
        let out = self.set_reference_now(args);
        // Slots and planes were changed together.
        if out.is_ok() {
            self.planes_synced();
        }
        out
    }

    fn set_reference_now(&mut self, args: &Value) -> ToolResult {
        if args.get("sheet").is_some_and(|v| !v.is_null()) {
            return self.at_rest(|app| app.set_reference_sheet(args));
        }
        for k in ["planes", "plane_opacity"] {
            if args.get(k).is_some_and(|v| !v.is_null()) {
                return Err(format!("{k} goes with sheet (a turnaround sheet's image planes)"));
            }
        }
        let (slot, legacy_pane) = match args["view"].as_str() {
            None if args.get("view").is_some_and(|v| !v.is_null()) => return Err(format!("view must be a slot name: {SLOT_NAMES}")),
            None => (None, None),
            Some("persp") => (Some(Slot::Front), Some(Some(ViewKind::Persp))),
            Some(v) => (Some(Slot::parse(v).ok_or(format!("unknown view {v}; the slots are {SLOT_NAMES}"))?), None),
        };
        let pane = match args.get("overlay").filter(|v| !v.is_null()) {
            None => legacy_pane,
            Some(Value::String(p)) if p == "none" => Some(None),
            Some(Value::String(p)) => Some(Some(ViewKind::from_name(p).ok_or(format!("unknown overlay pane {p}; use persp, top, front, side or none"))?)),
            Some(_) => return Err("overlay must be persp, top, front, side or none".into()),
        };
        if args["clear"].as_bool().unwrap_or(false) {
            match slot {
                Some(s) => {
                    // A plane of the slot without its slot still clears.
                    let plane = self.scene.objects.iter().any(|o| o.reference.as_ref().is_some_and(|r| r.slot == s.name()));
                    if self.references.remove(s).is_none() && !plane {
                        return Err(format!("no reference in slot {}; loaded: {}", s.name(), self.references.names().join(", ")));
                    }
                }
                None => self.references.clear(),
            }
            // A sheet's planes of the cleared slots go too (one undo step).
            let planes: Vec<u64> = self.scene.objects.iter().filter(|o| o.reference.as_ref().is_some_and(|r| slot.is_none_or(|s| r.slot == s.name()))).map(|o| o.id).collect();
            for id in planes {
                self.scene.remove_reference_plane(id);
            }
            let what = slot.map_or("References cleared.".to_string(), |s| format!("Reference {} cleared.", s.name()));
            return Ok(text(json!({ "cleared": what, "slots": self.references.names() })));
        }
        if args.get("path").is_none_or(Value::is_null) {
            let only = self.references.loaded().first().copied().filter(|_| self.references.slots.len() == 1);
            let s = slot.or(only).or(Some(Slot::Front)).filter(|s| self.references.slots.contains_key(s)).ok_or_else(|| if self.references.is_empty() { "no reference loaded; pass path".to_string() } else { format!("no reference in that slot; loaded: {}", self.references.names().join(", ")) })?;
            let r = self.references.slots.get_mut(&s).expect("checked");
            apply_display(r, args, pane);
            let r = &self.references.slots[&s];
            return Ok(text(json!({ "view": s.name(), "overlay": r.view.map(ViewKind::name), "opacity": r.opacity, "show": r.show, "slots": self.references.names() })));
        }
        let slot = slot.unwrap_or(Slot::Front);
        let path = image_path(args["path"].as_str().ok_or("path must be a string")?)?;
        let image = load_image(&path).map_err(|_| format!("{} is not a readable PNG or JPEG image", path.display()))?;
        let threshold = args["threshold"].as_f64().unwrap_or(40.0).clamp(1.0, 255.0) as u8;
        let (mask, mode) = subject_mask(&image, threshold);
        let bbox = mask_bbox(&mask, image.size[0]).ok_or(
            "could not find a subject in the image; use a plain background or a PNG with transparency, or change threshold",
        )?;
        let coverage = mask.iter().filter(|m| **m).count() as f32 / mask.len() as f32;
        let mut r = Reference { image, texture: None, slot, view: slot.pane(), opacity: 0.5, show: true, mask, bbox, path, sheet: None };
        apply_display(&mut r, args, pane);
        // A single image replaces a sheet's view in its slot, plane and all.
        let planes: Vec<u64> = self.scene.objects.iter().filter(|o| o.reference.as_ref().is_some_and(|p| p.slot == slot.name())).map(|o| o.id).collect();
        for id in planes {
            self.scene.remove_reference_plane(id);
        }
        let preview = mask_image(&r.mask, r.image.size);
        let mut info = json!({
            "path": r.path,
            "view": slot.name(),
            "size": r.image.size,
            "subject_bbox_px": r.bbox,
            "subject_fraction": round(coverage),
            "mask_from": mode,
            "palette": palette(&r.image, &r.mask),
            "overlay_view": r.view.map(ViewKind::name),
            "note": "The image below is the subject mask (white = subject). If it is wrong, call again with another threshold.",
        });
        self.references.insert(r);
        info["slots"] = json!(self.references.names());
        let mut out = text(info);
        out.extend(super::tools::encode_png(&super::tools::downscale(&preview, 384))?);
        Ok(out)
    }

    /// `render_views` tool: rigs at rest (where sculpt and paint work),
    /// or posed at `frame` for this call only.
    pub(super) fn render_views(&mut self, args: &Value) -> ToolResult {
        let keep = self.meshes.frame;
        let keep_baked = (self.meshes.show_baked, self.meshes.show_fine);
        self.set_frame(args["frame"].as_f64().map(|f| f as f32));
        if let Some(b) = args["baked"].as_bool() {
            self.meshes.show_baked = b;
        }
        if let Some(b) = args["fine"].as_bool() {
            self.meshes.show_fine = b;
        }
        self.meshes.sync(&self.scene);
        let out = self.render_views_now(args);
        self.set_frame(keep);
        (self.meshes.show_baked, self.meshes.show_fine) = keep_baked;
        self.meshes.sync(&self.scene);
        out
    }

    pub(super) fn render_views_now(&mut self, args: &Value) -> ToolResult {
        let views: Vec<Value> = match args.get("views") {
            Some(Value::Array(v)) if !v.is_empty() => v.clone(),
            None | Some(Value::Null) => ["front", "right", "back", "three_quarter"].map(Value::from).to_vec(),
            _ => return Err("views must be a non-empty list".into()),
        };
        if views.len() > 9 {
            return Err("at most 9 views per call".into());
        }
        let size = args["size"].as_u64().unwrap_or(384).clamp(64, 1024) as usize;
        let grid = args["grid"].as_bool().unwrap_or(false);
        // Without `shade`, the viewport's (`set_display shade`), wire too;
        // either way this call measures and draws it itself.
        let shade = match args.get("shade").filter(|v| !v.is_null()) {
            None => self.meshes.shade,
            Some(v) => crate::quality::Shade::parse(v.as_str().ok_or("shade must be beauty, curvature, lumpiness, normals or wire")?)?,
        };
        let with_ref = args["include_reference"].as_bool().unwrap_or(true);
        let extras = Extras {
            bones: args["show_bones"].as_bool().unwrap_or(false),
            weights: self.weight_target(&args["weights"])?,
            lighting: args.get("lighting").filter(|v| !v.is_null()).map(|v| self.parse_lighting(v)).transpose()?,
            ao: args.get("ambient_occlusion").filter(|v| !v.is_null()).map(|v| super::tools::parse_ao(v, self.ao)).transpose()?,
            shade,
            only: None,
            references: with_ref,
            guides: None,
        };
        let guides = match args.get("guides").filter(|v| !v.is_null()) {
            None => false,
            Some(v) => v.as_bool().ok_or("guides must be true or false")?,
        };

        let (mut center, mut radius) = self.scene_sphere();
        // Close-ups: frame a chosen point instead of the whole scene.
        if let Some(c) = args.get("center").filter(|v| !v.is_null()) {
            let c: [f32; 3] = serde_json::from_value(c.clone()).map_err(|_| "center must be [x, y, z]")?;
            center = Vec3::from(c);
        }
        if let Some(r) = args["radius"].as_f64() {
            radius = (r as f32).clamp(0.01, 1000.0);
        }
        let mut tiles = Vec::new();
        let mut names = Vec::new();
        // The loaded references first (one per slot).
        for r in self.references.slots.values().filter(|_| with_ref) {
            tiles.push(fit(&r.image, size, size));
            names.push(if self.references.slots.len() == 1 { "reference".to_string() } else { format!("reference {}", r.slot.name()) });
        }
        // The shade's colors once for every tile.
        let shaded = self.shaded_meshes(shade);
        for v in &views {
            let shot = Shot::framing(v, center, radius)?;
            // Guides through the point the tile frames.
            let ex = Extras { guides: guides.then_some((shot.target, radius)), ..extras.clone() };
            tiles.push(self.render_shaded(&shot, size, size, false, grid, ex, &shaded)?);
            names.push(shot.name);
        }
        let cols = (tiles.len() as f32).sqrt().ceil() as usize;
        let sheet = contact_sheet(&tiles, cols);
        let mut out = text(json!({
            "tiles": names,
            "layout": format!("{cols} per row, left to right, top to bottom"),
            "framed_center": [center.x, center.y, center.z],
            "framed_radius": radius,
            "shade": shade.name(),
            "guides": guides,
            "frame": self.meshes.frame,
            "fine_mesh_errors": self.fine_errors_json(),
            "device": raster_device(&self.render_state),
        }));
        out.extend(super::tools::encode_png(&sheet)?);
        Ok(out)
    }

    /// Lines the model up with the reference in `slot` from `view`: an
    /// orthographic camera where the model's height (or width) on screen
    /// matches the subject's in the image, centered on it. `only`: line up
    /// (and later draw) just these objects.
    fn align(&self, view: &Value, slot: Slot, match_width: bool, only: Option<&HashSet<u64>>) -> Result<Align, String> {
        let r = self.references.get(slot).ok_or("no reference image; call set_reference first")?;
        let (view_name, dir, _, _) = parse_view(view)?;
        let up = up_for(dir);
        let right = up.cross(dir).normalize();
        let up = dir.cross(right);

        // Model extent on screen axes.
        let eval = self.scene.evaluate();
        let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
        for o in self.scene.objects.iter().filter(|o| o.shape.is_mesh() && !o.is_reference() && only.is_none_or(|ids| ids.contains(&o.id))) {
            let Some(&(world, true)) = eval.get(&o.id) else { continue };
            let Some(mesh) = self.meshes.get(o) else { continue };
            for v in &mesh.vertices {
                let p = world.transform_point3(Vec3::from(v.pos));
                let q = glam::Vec2::new(p.dot(right), p.dot(up));
                lo = lo.min(q);
                hi = hi.max(q);
            }
        }
        if lo.x > hi.x || hi.y - lo.y < 1e-6 {
            return Err("nothing visible to line up with the reference".into());
        }

        // Panel size: the reference's aspect, long side COMPARE_MAX.
        let [rw, rh] = r.image.size;
        let k = (COMPARE_MAX as f32 / rw.max(rh) as f32).min(1.0);
        let (w, h) = (((rw as f32 * k) as usize).max(16), ((rh as f32 * k) as usize).max(16));
        let ref_mask = resize_mask(&r.mask, r.image.size, [w, h]);
        let bbox = r.bbox.map(|v| v as f32 * k);
        // The subject's edges on the panel: its first pixels' left and top
        // edges and its last pixels' right and bottom edges (pixel i spans
        // i..i + 1; the box's middle was half a pixel off before Stage 2).
        let [ex0, ey0, ex1, ey1] = [r.bbox[0] as f32 * k, r.bbox[1] as f32 * k, (r.bbox[2] + 1) as f32 * k, (r.bbox[3] + 1) as f32 * k];
        let (bw_px, bh_px) = ((ex1 - ex0).max(1.0), (ey1 - ey0).max(1.0));

        // Pixels per world unit: match heights (or widths for a lying-down
        // subject). A sheet's view instead keeps the sheet's one scale and
        // stands the model on its ground line (one fit for every view, so
        // proportions and size show).
        let mid = (lo + hi) * 0.5;
        let (pxm, pym) = ((ex0 + ex1) * 0.5, (ey0 + ey1) * 0.5);
        let (s, vc) = match &r.sheet {
            Some(f) => {
                let s = f.px_per_unit * k;
                (s, f.ground_y + (f.ground_row * k - h as f32 * 0.5) / s)
            }
            None => {
                let s = if match_width { bw_px / (hi.x - lo.x).max(1e-6) } else { bh_px / (hi.y - lo.y) };
                (s, mid.y + (pym - h as f32 * 0.5) / s)
            }
        };
        let uc = mid.x - (pxm - w as f32 * 0.5) / s;
        let (center, radius) = self.scene_sphere();
        let plane_center = right * uc + up * vc + dir * center.dot(dir);
        let shot = Shot {
            eye: plane_center + dir * (radius * 4.0 + 1.0),
            target: plane_center,
            up,
            ortho: Some(h as f32 * 0.5 / s),
            fov: FOV,
            name: view_name.clone(),
        };
        Ok(Align { view_name, slot, dir, right, up, s, uc, vc, w, h, k, shot, ref_mask, bbox, subject_px: bh_px, plane_center })
    }

    /// A render of just `only` (everything with None).
    fn render_only(&self, shot: &Shot, w: usize, h: usize, mask: bool, only: Option<&HashSet<u64>>) -> Result<egui::ColorImage, String> {
        let ex = Extras { only: only.map(|o| std::sync::Arc::new(o.clone())), ..Default::default() };
        self.render_shot_extras(shot, w, h, mask, false, ex)
    }

    /// The views a `compare_reference` call compares, each with the slot
    /// it compares against: `views` (at most `MAX_VIEWS`), else `view`,
    /// else every loaded slot from its own view.
    pub(super) fn compared_views(&self, args: &Value) -> Result<Vec<(Value, Slot)>, String> {
        let list: Vec<Value> = match args.get("views").filter(|v| !v.is_null()) {
            Some(Value::Array(v)) if v.is_empty() => return Err("views must not be empty".into()),
            Some(Value::Array(v)) if v.len() > MAX_VIEWS => return Err(format!("at most {MAX_VIEWS} views per call (got {})", v.len())),
            Some(Value::Array(v)) => v.clone(),
            Some(_) => return Err("views must be a list of views".into()),
            None => match args.get("view").filter(|v| !v.is_null()) {
                Some(v) => vec![v.clone()],
                None if self.references.is_empty() => return Err("no reference image; call set_reference first".into()),
                None => self.references.loaded().iter().map(|s| Value::from(s.name())).collect(),
            },
        };
        list.into_iter().map(|v| self.references.slot_for(&v).map(|s| (v, s))).collect()
    }

    /// Compares one view: the silhouettes' overlap and contour distance,
    /// whole or as a close-up (`closeup_args`), drawing `only` (everything
    /// with None).
    pub(super) fn compare_view(&self, view: &Value, slot: Slot, match_width: bool, closeup: Option<(Vec3, f32, usize)>, features: bool, only: Option<&HashSet<u64>>) -> Result<Compared, String> {
        let a = self.align(view, slot, match_width, only)?;
        let r = self.references.get(slot).expect("align checked");
        let subject_px = a.subject_px;
        let round3 = |v: Vec3| [round(v.x), round(v.y), round(v.z)];
        let Some((center, radius, size)) = closeup else {
            let [bx0, by0, bx1, by1] = a.bbox;
            let mask_img = self.render_only(&a.shot, a.w, a.h, true, only)?;
            let beauty = self.render_only(&a.shot, a.w, a.h, false, only)?;
            let model: Vec<bool> = mask_img.pixels.iter().map(|p| p.r() > 127).collect();
            let (iou, missing, extra, diff_img, _) = overlap(&a.ref_mask, &model, a.w, a.h);
            let regions = region_report(&a.ref_mask, &model, a.w, [bx0, by0, bx1, by1]);
            let origin = a.plane_center + a.right * (-(a.w as f32) * 0.5 / a.s) + a.up * (a.h as f32 * 0.5 / a.s);
            let contour = super::outline::contour(&a.ref_mask, &model, a.w, a.h);
            return Ok(Compared {
                view: a.view_name.clone(),
                slot,
                iou,
                missing,
                extra,
                regions,
                contour: contour.map(|c| Distances { mean: c.mean / a.s, p95: c.p95 / a.s, mean_pct: 100.0 * c.mean / subject_px, p95_pct: 100.0 * c.p95 / subject_px }),
                panels: vec![fit(&r.image, a.w, a.h), beauty, diff_img],
                features: None,
                pixel_to_world: json!({
                    "panel_size": [a.w, a.h],
                    "origin": round3(origin),
                    "per_px_right": round3(a.right / a.s),
                    "per_px_down": round3(-a.up / a.s),
                    "note": "world = origin + x * per_px_right + y * per_px_down for pixel (x, y) of one panel, on the plane through the model's center.",
                }),
                closeup: None,
                aligned_by: if r.sheet.is_some() { "sheet" } else if match_width { "width" } else { "height" },
            });
        };
        // A close-up: the square of half-size `radius` around `center`,
        // lined up as for the whole model. The model is rendered again
        // there at `size` pixels, and the reference is cut from its own
        // pixels there (not from the smaller panel).
        let (x, y, _) = a.project(center);
        let half = radius * a.s;
        let (w, h) = (a.w as f32, a.h as f32);
        if x + half <= 0.0 || y + half <= 0.0 || x - half >= w || y - half >= h {
            return Err(format!("center is outside the lined-up view (at panel pixel [{:.0}, {:.0}] of {} x {}); pick a point on the model", x, y, a.w, a.h));
        }
        let ref_px = 2.0 * half / a.k;
        if ref_px < MIN_CROP_REF_PX {
            return Err(format!("the crop would be {ref_px:.1} reference pixels across, under {MIN_CROP_REF_PX}: too few to compare; use radius >= {:.4}", MIN_CROP_REF_PX * a.k / (2.0 * a.s)));
        }
        let crop_px = [(x - half).max(0.0), (y - half).max(0.0), (x + half).min(w), (y + half).min(h)];
        // The camera: the whole-model one moved to the point, framing the square.
        let (u0, v0) = (center.dot(a.right), center.dot(a.up));
        let target = a.right * u0 + a.up * v0 + a.dir * a.plane_center.dot(a.dir);
        let shot = Shot { eye: target + (a.shot.eye - a.shot.target), target, up: a.up, ortho: Some(radius), fov: FOV, name: a.view_name.clone() };
        let mask_img = self.render_only(&shot, size, size, true, only)?;
        let beauty = self.render_only(&shot, size, size, false, only)?;
        let model: Vec<bool> = mask_img.pixels.iter().map(|p| p.r() > 127).collect();
        // Crop pixel (i, j) -> world screen axes -> reference pixel.
        let per = 2.0 * radius / size as f32;
        let (reference_tile, ref_mask) = crop_reference(r, size, |i, j| {
            let (u, v) = (u0 - radius + (i as f32 + 0.5) * per, v0 + radius - (j as f32 + 0.5) * per);
            (((u - a.uc) * a.s + w * 0.5) / a.k, ((a.vc - v) * a.s + h * 0.5) / a.k)
        });
        let (iou, missing, extra, diff_img, both) = overlap(&ref_mask, &model, size, size);
        let regions = region_report(&ref_mask, &model, size, [0.0, 0.0, size as f32 - 1.0, size as f32 - 1.0]);
        let origin = target - a.right * radius + a.up * radius;
        let subject = subject_px / a.s;
        let contour = super::outline::contour(&ref_mask, &model, size, size);
        let mut panels = vec![reference_tile, beauty, diff_img];
        // Feature lines inside both outlines: the reference's drawn lines
        // against the model's creases, curvature edges and contours.
        let features = features.then(|| {
            let crop = super::features::Crop { right: a.right, up: a.up, dir: a.dir, u0, v0, radius, size };
            let model_lines = self.model_lines(&crop, only);
            let reference_lines = super::features::reference_lines(&panels[0], &ref_mask);
            panels.push(super::features::lines_panel(&reference_lines, &model_lines.lines, &model_lines.mask, size));
            FeatureScore { lines: super::outline::features(&reference_lines, &model_lines.lines, size, size), per, radius }
        });
        Ok(Compared {
            view: a.view_name.clone(),
            slot,
            iou,
            missing,
            extra,
            regions,
            contour: contour.map(|c| Distances { mean: c.mean * per, p95: c.p95 * per, mean_pct: 100.0 * c.mean * per / subject, p95_pct: 100.0 * c.p95 * per / subject }),
            panels,
            features,
            pixel_to_world: json!({
                "panel_size": [size, size],
                "origin": round3(origin),
                "per_px_right": round3(a.right * per),
                "per_px_down": round3(-a.up * per),
                "note": "world = origin + x * per_px_right + y * per_px_down for pixel (x, y) of one crop panel, on the plane through the model's center.",
            }),
            closeup: Some(json!({
                "center": round3(center),
                "radius": round(radius),
                "size": size,
                "crop_px": crop_px.map(|v| (v as f64 * 10.0).round() / 10.0),
                "reference_px_per_unit": round(a.s / a.k),
                "interior": interior(both, size),
                "note": "crop_px is the square on the whole-model panel (as compare_reference without center draws it); the IoU, regions and pixel_to_world are the crop's.",
            })),
            aligned_by: if r.sheet.is_some() { "sheet" } else if match_width { "width" } else { "height" },
        })
    }

    /// `compare_reference` tool: one view (`view`) answers flat, as before
    /// slots; `views` (or several loaded slots) answer per view with the
    /// mean IoU and the worst view.
    pub(super) fn compare_reference(&mut self, args: &Value) -> ToolResult {
        let closeup = closeup_args(args)?;
        let contour = args["contour"].as_bool().unwrap_or(true);
        let match_width = args["match"].as_str() == Some("width");
        let features = match args["features"].as_bool() {
            Some(true) if closeup.is_none() => return Err("features compares a close-up's interior lines: pass center and radius".into()),
            Some(f) => f,
            None => closeup.is_some(),
        };
        let picks = self.compared_views(args)?;
        let compared: Vec<Compared> = picks.iter().map(|(v, s)| self.compare_view(v, *s, match_width, closeup, features, None)).collect::<Result<_, _>>()?;
        let lines_note = if features { " | lines (red: the reference's, cyan: the model's, white: both, inside both outlines)" } else { "" };
        let panels_note = if closeup.is_some() { "crop panels" } else { "panels" };
        if args.get("views").is_none_or(Value::is_null) && compared.len() == 1 {
            let c = compared.into_iter().next().expect("one");
            let mut reply = c.json(contour);
            if contour {
                reply["contour_note"] = json!(CONTOUR_NOTE);
            }
            reply["image"] = json!(format!("{panels_note}: reference | model from the same camera | overlap (gray = both, red = in reference but missing from the model, blue = model but not in reference){lines_note}."));
            let mut out = text(reply);
            out.extend(super::tools::encode_png(&contact_sheet(&c.panels, c.panels.len()))?);
            return Ok(out);
        }
        let worst = compared.iter().min_by(|a, b| a.iou.total_cmp(&b.iou)).expect("at least one view");
        let mean = compared.iter().map(|c| c.iou).sum::<f32>() / compared.len() as f32;
        let mut reply = json!({
            "views": compared.iter().map(|c| c.json(contour)).collect::<Vec<_>>(),
            "mean_iou": round(mean),
            "worst_view": worst.view,
            "image": format!("One row per view, in order, of {panels_note}: reference | model from the same camera | overlap (gray = both, red = in reference but missing from the model, blue = model but not in reference){lines_note}."),
        });
        if contour {
            reply["contour_note"] = json!(CONTOUR_NOTE);
        }
        // Rows of three; past three views each panel is shown at half size.
        let tiles: Vec<egui::ColorImage> = compared.iter().flat_map(|c| c.panels.iter().map(|p| if compared.len() > 3 { super::tools::downscale(p, p.size[0].div_ceil(2)) } else { p.clone() })).collect();
        let mut out = text(reply);
        out.extend(super::tools::encode_png(&contact_sheet(&tiles, compared[0].panels.len()))?);
        Ok(out)
    }

    /// `project_reference` tool: paints the reference image onto a clay
    /// object like a slide projector, from a lined-up camera (see `align`).
    /// Only surfaces that face the camera and are not hidden get color.
    pub(super) fn project_reference(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != crate::scene::Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        // Paint lives in the rest pose, so project onto the rest pose.
        let keep = self.meshes.frame;
        self.set_frame(None);
        self.meshes.sync(&self.scene);
        let out = self.project_reference_now(id, args);
        self.set_frame(keep);
        self.meshes.sync(&self.scene);
        out
    }

    fn project_reference_now(&mut self, id: u64, args: &Value) -> ToolResult {
        let view = args.get("view").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| Value::from("front"));
        let slot = self.references.slot_for(&view)?;
        let a = self.align(&view, slot, args["match"].as_str() == Some("width"), None)?;
        let strength = args["strength"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        let min_facing = args["min_facing"].as_f64().unwrap_or(0.3).clamp(0.0, 0.95) as f32;

        // Depth buffer of everything visible, so hidden surfaces get no paint.
        let eval = self.scene.evaluate();
        let mut zbuf = vec![f32::MIN; a.w * a.h];
        for o in self.scene.objects.iter().filter(|o| o.shape.is_mesh() && !o.is_reference()) {
            let Some(&(world, true)) = eval.get(&o.id) else { continue };
            let Some(mesh) = self.meshes.get(o) else { continue };
            let pts: Vec<(f32, f32, f32)> = mesh.vertices.iter().map(|v| a.project(world.transform_point3(Vec3::from(v.pos)))).collect();
            for t in mesh.indices.chunks_exact(3) {
                raster(&mut zbuf, a.w, a.h, pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            }
        }

        let o = self.scene.get(id).expect("checked");
        let world = eval.get(&id).map_or(Mat4::IDENTITY, |e| e.0);
        let normal_m = glam::Mat3::from_mat4(world).inverse().transpose();
        let mesh = self.meshes.get(o).ok_or("clay has no mesh yet")?.clone();
        let cell = o.clay.as_ref().and_then(|c| c.cell_size()).unwrap_or(0.02);
        let eps = cell * 3.0 * world.to_scale_rotation_translation().0.max_element();
        let r = self.references.get(a.slot).expect("align checked");
        let [rw, rh] = r.image.size;
        let mut samples = Vec::new();
        for v in &mesh.vertices {
            let p = world.transform_point3(Vec3::from(v.pos));
            let n = (normal_m * Vec3::from(v.normal)).normalize_or_zero();
            let facing = n.dot(a.dir);
            let weight = smoothstep(min_facing, min_facing + 0.25, facing) * strength;
            if weight <= 0.0 {
                continue;
            }
            let (x, y, depth) = a.project(p);
            if x < 0.0 || y < 0.0 || x >= a.w as f32 || y >= a.h as f32 {
                continue;
            }
            if depth < zbuf[y as usize * a.w + x as usize] - eps {
                continue;
            }
            // Panel pixel -> reference pixel; skip the background.
            let (fx, fy) = (x / a.k, y / a.k);
            let (ix, iy) = ((fx as usize).min(rw - 1), (fy as usize).min(rh - 1));
            if !r.mask[iy * rw + ix] {
                continue;
            }
            samples.push((Vec3::from(v.pos), bilinear(&r.image, fx, fy), weight));
        }
        if samples.is_empty() {
            return Err("no visible surface of the object faces this view inside the subject".into());
        }
        let painted = samples.len();
        let obj = self.scene.get_mut(id).expect("checked");
        let mut clay = obj.clay.clone().unwrap_or_default();
        crate::ledger::settle_clay(&mut clay, None)?;
        crate::paint::splat(&mut clay, &samples)?;
        crate::layers::sync_within_budget(&mut clay)?;
        obj.clay = Some(clay);
        self.meshes.sync(&self.scene);

        let beauty = self.render_shot(&a.shot, a.w, a.h, false, false)?;
        let r = self.references.get(a.slot).expect("align checked");
        let mut out = text(json!({
            "view": a.view_name,
            "painted_vertices": painted,
            "of_vertices": mesh.vertices.len(),
            "image": "reference | model from the same camera after projecting",
            "note": "Surfaces facing away (sides, back) keep their color; project from other views or paint them.",
        }));
        out.extend(super::tools::encode_png(&contact_sheet(&[fit(&r.image, a.w, a.h), beauty], 2))?);
        Ok(out)
    }

    /// Paints the references that belong on a pane over it, fitted and centered.
    pub(super) fn paint_reference(&mut self, ui: &egui::Ui, kind: ViewKind, rect: egui::Rect) {
        for r in self.references.slots.values_mut().filter(|r| r.show && r.view == Some(kind)) {
            let tex = r.texture.get_or_insert_with(|| ui.ctx().load_texture(format!("reference {}", r.slot.name()), r.image.clone(), egui::TextureOptions::LINEAR));
            let [iw, ih] = r.image.size;
            let scale = (rect.width() / iw as f32).min(rect.height() / ih as f32);
            let area = egui::Rect::from_center_size(rect.center(), egui::vec2(iw as f32, ih as f32) * scale);
            let tint = egui::Color32::from_white_alpha((r.opacity.clamp(0.0, 1.0) * 255.0) as u8);
            ui.painter_at(rect).image(tex.id(), area, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
    }
}

/// A close-up crop must cover at least this many reference pixels across.
const MIN_CROP_REF_PX: f32 = 8.0;
const CONTOUR_NOTE: &str = "contour: outline to outline, both ways: the mean and the larger 95th percentile, in world units and % of the subject's height in the reference";

/// Most views one `compare_reference` (or `quality_report`) compares.
pub(super) const MAX_VIEWS: usize = 12;

/// A contour distance in world units and in percent of the subject's
/// height in the reference.
#[derive(Clone, Copy, Debug)]
pub(super) struct Distances {
    pub mean: f32,
    pub p95: f32,
    pub mean_pct: f32,
    pub p95_pct: f32,
}

/// One view of `compare_reference`.
pub(super) struct Compared {
    pub view: String,
    pub slot: Slot,
    pub iou: f32,
    pub missing: f32,
    pub extra: f32,
    pub regions: Vec<Value>,
    pub contour: Option<Distances>,
    /// reference | model | overlap (| lines, with features).
    pub panels: Vec<egui::ColorImage>,
    /// A close-up's feature lines compared.
    pub features: Option<FeatureScore>,
    pub pixel_to_world: Value,
    pub closeup: Option<Value>,
    pub aligned_by: &'static str,
}

/// A close-up's feature lines compared, with the crop's scale.
#[derive(Clone, Copy, Debug)]
pub(super) struct FeatureScore {
    /// None when the reference crop has no line.
    pub lines: Option<super::outline::Features>,
    /// World units per pixel, and the crop's half-size.
    pub per: f32,
    pub radius: f32,
}

impl FeatureScore {
    /// The chamfer in % of the crop's radius.
    pub(super) fn pct(&self) -> Option<f32> {
        self.lines.map(|f| 100.0 * f.chamfer * self.per / self.radius)
    }

    fn json(&self) -> Value {
        match self.lines {
            Some(f) => json!({ "chamfer": round(f.chamfer * self.per), "chamfer_pct": self.pct().map(round), "recall": round(f.recall), "precision": round(f.precision), "reference_line_px": f.reference_px, "model_line_px": f.model_px }),
            None => json!({ "skipped": "no lines to compare in the reference crop" }),
        }
    }
}

impl Compared {
    /// The view's reply (with its contour distance when `contour`).
    pub(super) fn json(&self, contour: bool) -> Value {
        let mut v = json!({
            "view": self.view,
            "slot": self.slot.name(),
            "overlap_iou": round(self.iou),
            "missing_fraction": round(self.missing),
            "extra_fraction": round(self.extra),
            "regions": self.regions,
            "pixel_to_world": self.pixel_to_world,
            "aligned_by": self.aligned_by,
        });
        if contour {
            v["contour"] = self.contour.map_or(Value::Null, |c| json!({ "mean": round(c.mean), "p95": round(c.p95), "mean_pct": round(c.mean_pct), "p95_pct": round(c.p95_pct) }));
        }
        if let Some(c) = &self.closeup {
            v["closeup"] = c.clone();
        }
        if let Some(f) = &self.features {
            v["features"] = f.json();
        }
        v
    }
}

/// `compare_reference`'s close-up arguments, checked before anything is
/// rendered: Some((center, radius, size)) with `center`.
pub(super) fn closeup_args(args: &Value) -> Result<Option<(Vec3, f32, usize)>, String> {
    let center = args.get("center").filter(|c| !c.is_null());
    let radius = args.get("radius").filter(|r| !r.is_null());
    let size = args.get("size").filter(|s| !s.is_null());
    let Some(center) = center else {
        return match (radius, size) {
            (None, None) => Ok(None),
            _ => Err("radius and size are for a close-up: pass center too".into()),
        };
    };
    let center = Vec3::from(serde_json::from_value::<[f32; 3]>(center.clone()).map_err(|_| "center must be [x, y, z]")?);
    let radius = radius.ok_or("a close-up needs radius (half the crop's size in world units) with center")?;
    let radius = radius.as_f64().filter(|r| (0.001..=1000.0).contains(r)).ok_or("radius must be from 0.001 to 1000 world units")? as f32;
    let size = match size {
        None => 512,
        Some(v) => v.as_u64().filter(|s| (64..=1024).contains(s)).ok_or("size must be a whole number of pixels from 64 to 1024")? as usize,
    };
    if !center.is_finite() {
        return Err("center must be finite".into());
    }
    Ok(Some((center, radius, size)))
}

/// A close-up is interior when at least 98% of its pixels are inside
/// both outlines: the silhouette says little there.
fn interior(both: usize, size: usize) -> bool {
    both as f32 >= 0.98 * (size * size) as f32
}

/// Silhouette overlap of two masks: (IoU, missing and extra shares of the
/// union, the overlap picture, pixels in both).
fn overlap(reference: &[bool], model: &[bool], w: usize, h: usize) -> (f32, f32, f32, egui::ColorImage, usize) {
    let (mut both, mut only_ref, mut only_model) = (0usize, 0usize, 0usize);
    let mut diff = Vec::with_capacity(w * h);
    for (&a, &b) in reference.iter().zip(model) {
        diff.push(match (a, b) {
            (true, true) => {
                both += 1;
                egui::Color32::from_gray(200)
            }
            (true, false) => {
                only_ref += 1;
                egui::Color32::from_rgb(235, 60, 60)
            }
            (false, true) => {
                only_model += 1;
                egui::Color32::from_rgb(60, 130, 245)
            }
            _ => egui::Color32::from_gray(28),
        });
    }
    let union = (both + only_ref + only_model).max(1) as f32;
    (both as f32 / union, only_ref as f32 / union, only_model as f32 / union, egui::ColorImage::new([w, h], diff), both)
}

/// A `size` x `size` crop of the reference: `at(i, j)` is the reference
/// pixel position (fractional) under crop pixel (i, j). Colors are
/// bilinear over the background color (transparent pixels and outside the
/// image are background); the mask is the nearest reference pixel's.
fn crop_reference(r: &Reference, size: usize, at: impl Fn(usize, usize) -> (f32, f32)) -> (egui::ColorImage, Vec<bool>) {
    let [rw, rh] = r.image.size;
    let bg = Vec3::from(BACKGROUND.map(|c| c as f32 / 255.0));
    let mut pixels = Vec::with_capacity(size * size);
    let mut mask = Vec::with_capacity(size * size);
    for j in 0..size {
        for i in 0..size {
            let (fx, fy) = at(i, j);
            if !(fx >= 0.0 && fy >= 0.0 && fx < rw as f32 && fy < rh as f32) {
                pixels.push(egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]));
                mask.push(false);
                continue;
            }
            let (ix, iy) = ((fx as usize).min(rw - 1), (fy as usize).min(rh - 1));
            let alpha = r.image.pixels[iy * rw + ix].a() as f32 / 255.0;
            let c = bg.lerp(bilinear(&r.image, fx, fy), alpha) * 255.0;
            pixels.push(egui::Color32::from_rgb(c.x.round() as u8, c.y.round() as u8, c.z.round() as u8));
            mask.push(r.mask[iy * rw + ix]);
        }
    }
    (egui::ColorImage::new([size, size], pixels), mask)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rasterizes one triangle into a depth buffer that keeps the nearest
/// (largest) depth. Points are (x, y, depth) in panel pixels.
fn raster(zbuf: &mut [f32], w: usize, h: usize, a: (f32, f32, f32), b: (f32, f32, f32), c: (f32, f32, f32)) {
    let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    if area.abs() < 1e-9 || !area.is_finite() {
        return;
    }
    let x0 = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
    let y0 = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
    let x1 = (a.0.max(b.0).max(c.0).ceil().max(0.0) as usize).min(w);
    let y1 = (a.1.max(b.1).max(c.1).ceil().max(0.0) as usize).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let w0 = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
            let w1 = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let d = a.2 * w0 + b.2 * w1 + c.2 * w2;
            let z = &mut zbuf[y * w + x];
            if d > *z {
                *z = d;
            }
        }
    }
}

/// sRGB 0..1 color at a fractional pixel position.
fn bilinear(img: &egui::ColorImage, x: f32, y: f32) -> Vec3 {
    let [w, h] = img.size;
    let (x, y) = ((x - 0.5).clamp(0.0, (w - 1) as f32), (y - 0.5).clamp(0.0, (h - 1) as f32));
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let px = |x: usize, y: usize| {
        let c = img.pixels[y * w + x].to_srgba_unmultiplied();
        Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0
    };
    let top = px(x0, y0).lerp(px(x1, y0), tx);
    let bottom = px(x0, y1).lerp(px(x1, y1), tx);
    top.lerp(bottom, ty)
}

/// The main colors of the subject (k-means), with where each one sits.
fn palette(img: &egui::ColorImage, mask: &[bool]) -> Vec<Value> {
    let w = img.size[0];
    let subject: Vec<usize> = (0..mask.len()).filter(|&i| mask[i]).collect();
    if subject.is_empty() {
        return Vec::new();
    }
    let step = (subject.len() / 6000).max(1);
    let pts: Vec<(Vec3, glam::Vec2)> = subject
        .iter()
        .step_by(step)
        .map(|&i| {
            let c = img.pixels[i].to_srgba_unmultiplied();
            (Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0, glam::Vec2::new((i % w) as f32, (i / w) as f32))
        })
        .collect();
    // Start from colors spread over the brightness range.
    let mut by_light: Vec<Vec3> = pts.iter().map(|p| p.0).collect();
    by_light.sort_by(|a, b| a.dot(Vec3::ONE).total_cmp(&b.dot(Vec3::ONE)));
    let k = 6.min(by_light.len());
    let mut centers: Vec<Vec3> = (0..k).map(|j| by_light[(j * 2 + 1) * by_light.len() / (2 * k)]).collect();
    let mut owner = vec![0usize; pts.len()];
    for _ in 0..12 {
        for (o, p) in owner.iter_mut().zip(&pts) {
            *o = (0..k).min_by(|&a, &b| (p.0 - centers[a]).length_squared().total_cmp(&(p.0 - centers[b]).length_squared())).unwrap_or(0);
        }
        for (j, c) in centers.iter_mut().enumerate() {
            let (sum, n) = owner.iter().zip(&pts).filter(|(o, _)| **o == j).fold((Vec3::ZERO, 0), |(s, n), (_, p)| (s + p.0, n + 1));
            if n > 0 {
                *c = sum / n as f32;
            }
        }
    }
    let mut out: Vec<(f32, Value)> = (0..k)
        .filter_map(|j| {
            let members: Vec<&(Vec3, glam::Vec2)> = owner.iter().zip(&pts).filter(|(o, _)| **o == j).map(|(_, p)| p).collect();
            if members.is_empty() {
                return None;
            }
            let share = members.len() as f32 / pts.len() as f32;
            let at = members.iter().map(|m| m.1).sum::<glam::Vec2>() / members.len() as f32;
            let c = centers[j];
            Some((share, json!({ "color": [round(c.x), round(c.y), round(c.z)], "share": round(share), "center_px": [at.x.round(), at.y.round()] })))
        })
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().filter(|(s, _)| *s >= 0.01).map(|(_, v)| v).collect()
}

/// A reference's overlay settings from a call (`pane` already checked).
fn apply_display(r: &mut Reference, args: &Value, pane: Option<Option<ViewKind>>) {
    if let Some(p) = pane {
        r.view = p;
    }
    if let Some(o) = args["opacity"].as_f64() {
        r.opacity = o.clamp(0.0, 1.0) as f32;
    }
    if let Some(s) = args["show"].as_bool() {
        r.show = s;
    }
}

fn text(v: impl Into<Value>) -> Vec<Value> {
    let v: Value = v.into();
    let s = match v {
        Value::String(s) => s,
        other => serde_json::to_string_pretty(&other).unwrap_or_default(),
    };
    vec![json!({ "type": "text", "text": s })]
}

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// Reference images: absolute path to an existing .png/.jpg/.jpeg file.
pub(super) fn image_path(p: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(p);
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg")) {
        return Err("path must end in .png, .jpg, or .jpeg".into());
    }
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    Ok(path)
}

fn load_image(path: &Path) -> Result<egui::ColorImage, String> {
    let mut reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut img = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    if img.width().max(img.height()) > REF_MAX {
        let k = REF_MAX as f32 / img.width().max(img.height()) as f32;
        let (w, h) = (((img.width() as f32 * k) as u32).max(1), ((img.height() as f32 * k) as u32).max(1));
        img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
    }
    let size = [img.width() as usize, img.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Subject pixels: from transparency when the image has it, else everything
/// that differs from the border color by more than `threshold`.
pub(super) fn subject_mask(img: &egui::ColorImage, threshold: u8) -> (Vec<bool>, &'static str) {
    let clear = img.pixels.iter().filter(|p| p.a() < 128).count();
    if clear > img.pixels.len() / 100 {
        return (img.pixels.iter().map(|p| p.a() >= 128).collect(), "transparency");
    }
    let [w, h] = img.size;
    let mut border: Vec<[u8; 3]> = Vec::new();
    for x in 0..w {
        for y in [0, h - 1] {
            let p = img.pixels[y * w + x];
            border.push([p.r(), p.g(), p.b()]);
        }
    }
    for y in 0..h {
        for x in [0, w - 1] {
            let p = img.pixels[y * w + x];
            border.push([p.r(), p.g(), p.b()]);
        }
    }
    let bg: [u8; 3] = std::array::from_fn(|c| {
        let mut v: Vec<u8> = border.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    });
    let mask = img
        .pixels
        .iter()
        .map(|p| [p.r(), p.g(), p.b()].iter().zip(bg).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0) > threshold)
        .collect();
    (mask, "background color")
}

fn mask_bbox(mask: &[bool], w: usize) -> Option<[usize; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
        let (x, y) = (i % w, i / w);
        b = Some(match b {
            None => [x, y, x, y],
            Some([a, c, d, e]) => [a.min(x), c.min(y), d.max(x), e.max(y)],
        });
    }
    b
}

fn mask_image(mask: &[bool], size: [usize; 2]) -> egui::ColorImage {
    egui::ColorImage::new(size, mask.iter().map(|&m| egui::Color32::from_gray(if m { 255 } else { 0 })).collect())
}

/// Nearest-neighbor mask resize.
fn resize_mask(mask: &[bool], from: [usize; 2], to: [usize; 2]) -> Vec<bool> {
    let mut out = Vec::with_capacity(to[0] * to[1]);
    for y in 0..to[1] {
        let sy = (y * from[1] / to[1]).min(from[1] - 1);
        for x in 0..to[0] {
            let sx = (x * from[0] / to[0]).min(from[0] - 1);
            out.push(mask[sy * from[0] + sx]);
        }
    }
    out
}

/// Fits an image inside w x h (nearest neighbor), centered on the background color.
fn fit(img: &egui::ColorImage, w: usize, h: usize) -> egui::ColorImage {
    let [iw, ih] = img.size;
    let k = (w as f32 / iw as f32).min(h as f32 / ih as f32);
    let (fw, fh) = ((iw as f32 * k) as usize, (ih as f32 * k) as usize);
    let (ox, oy) = ((w - fw.min(w)) / 2, (h - fh.min(h)) / 2);
    let bg = egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]);
    let mut out = vec![bg; w * h];
    for y in 0..fh.min(h) {
        for x in 0..fw.min(w) {
            let sx = ((x as f32 / k) as usize).min(iw - 1);
            let sy = ((y as f32 / k) as usize).min(ih - 1);
            let p = img.pixels[sy * iw + sx];
            // Show transparent reference pixels as background.
            let a = p.a() as u16;
            let mix = |c: u8, b: u8| ((c as u16 * a + b as u16 * (255 - a)) / 255) as u8;
            out[(y + oy) * w + x + ox] = egui::Color32::from_rgb(mix(p.r(), bg.r()), mix(p.g(), bg.g()), mix(p.b(), bg.b()));
        }
    }
    egui::ColorImage::new([w, h], out)
}

/// Lays images out in rows of `cols`, separated by thin dark gaps.
/// A callback that draws every visible mesh of a scene (any thread: an
/// off-screen job renders a snapshot). `mask` draws flat white.
pub(crate) fn scene_callback_of(scene: &crate::scene::Scene, meshes: &MeshLib, view_proj: Mat4, eye: Vec3, mask: bool, grid: bool) -> ViewportCallback {
    // Animations and checks draw the scene as made: no reference planes.
    scene_callback_with(scene, meshes, view_proj, eye, mask, grid, &Default::default(), false, None, true, false)
}

/// `scene_callback_of`, with some objects drawn with their own `shaded`
/// meshes (white, so the mesh's colors show), with `wire` every mesh's
/// edges over the fills, with `only` just those objects, and clay in the
/// viewport's quality shade (`MeshLib::shade`) only with `viewport_shade`
/// (`render_views` colors its own); reference image planes only with
/// `references`.
#[allow(clippy::too_many_arguments)]
fn scene_callback_with(scene: &crate::scene::Scene, meshes: &MeshLib, view_proj: Mat4, eye: Vec3, mask: bool, grid: bool, shaded: &std::collections::HashMap<u64, std::sync::Arc<crate::mesh::Mesh>>, wire: bool, only: Option<&HashSet<u64>>, viewport_shade: bool, references: bool) -> ViewportCallback {
    let eval = scene.evaluate();
    let mut fills = Vec::new();
    let mut wires = Vec::new();
    for d in crate::envkit::scatter::draws(scene, &eval).iter().filter(|d| d.object.shape.is_mesh() && (references || !d.object.is_reference()) && only.is_none_or(|ids| ids.contains(&d.object.id))) {
        let instance = Instance::new(d.world, if mask { [1.0; 3] } else { d.object.color }).with_material(&scene.material_of(d.object));
        let own = (!d.scattered && shaded.contains_key(&d.object.id)).then_some(crate::mesh::MeshKey::Own(d.object.id));
        match own {
            Some(key) => fills.push((key, instance.with_color([1.0; 3]))),
            None => push_fills_with(&mut fills, meshes, d.object, instance, viewport_shade),
        }
        if wire && !d.scattered {
            wires.push((own.unwrap_or_else(|| meshes.key_of(d.object)), Instance::new(d.world, super::theme::WIRE_DEFAULT)));
        }
    }
    fills.sort_by_key(|(k, _)| *k);
    wires.sort_by_key(|(k, _)| *k);
    let mut dynamic = meshes.own_meshes_with(viewport_shade);
    dynamic.retain(|(k, _)| !matches!(k, crate::mesh::MeshKey::Own(id) if shaded.contains_key(id)));
    dynamic.extend(shaded.iter().map(|(id, m)| (crate::mesh::MeshKey::Own(*id), m.clone())));
    ViewportCallback {
        slot: 0,
        view_proj,
        grid_model: Mat4::IDENTITY,
        cam_pos: eye,
        fills,
        wires,
        dynamic,
        show_grid: grid && !mask,
        flat: mask,
        lights: if mask { Vec::new() } else { crate::envkit::light::view_lights(scene, &eval, eye) },
        shadows: render::Shadows::default(),
        lighting: crate::view_light::ViewLighting::studio_default(),
        ao: render::Ao::default(),
        shadow_focus: None,
        post: render::post::ViewLook::default(),
        pane: None,
        frame: 0,
        background: [egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]); 2],
        chunks: meshes.sculpt_chunks(),
        matcap: None,
        freeze_shadows: false,
        cursor: None,
    }
}

/// An object's fills: its mesh, or each of its chunks while it is drawn
/// in chunks (a sculpt session; with the mask and group overlay).
pub(crate) fn push_fills(fills: &mut Vec<(crate::mesh::MeshKey, Instance)>, meshes: &MeshLib, o: &crate::scene::Object, instance: Instance) {
    push_fills_with(fills, meshes, o, instance, true)
}

/// `push_fills`, with clay in the viewport's quality shade only with
/// `viewport_shade`.
fn push_fills_with(fills: &mut Vec<(crate::mesh::MeshKey, Instance)>, meshes: &MeshLib, o: &crate::scene::Object, instance: Instance, viewport_shade: bool) {
    // A quality shade colors the whole mesh (`MeshLib::shade`), drawn
    // white so its colors show; chunks would not carry them. While its
    // colors are measured, or strokes come in, it draws as without one.
    if viewport_shade && o.shape == crate::scene::Shape::Clay && meshes.shaded_now(o.id) {
        fills.push((meshes.key_of(o), instance.with_color([1.0; 3])));
        return;
    }
    match meshes.chunk_keys(o) {
        Some(keys) => fills.extend(keys.into_iter().map(|k| (k, instance.with_overlay(meshes.overlay)))),
        None => fills.push((meshes.key_of(o), instance)),
    }
}

/// The reply's `device` for viewport renders (render_views,
/// render_animation): always the window's GPU rasterizer, never the tracer.
pub(super) fn raster_device(rs: &egui_wgpu::RenderState) -> Value {
    let info = rs.adapter.get_info();
    json!({ "used": "gpu", "gpu": format!("{} ({:?})", info.name, info.backend), "fallback_reason": null, "renderer": "viewport" })
}

pub(crate) fn contact_sheet(tiles: &[egui::ColorImage], cols: usize) -> egui::ColorImage {
    const GAP: usize = 4;
    let cols = cols.max(1);
    let rows = tiles.len().div_ceil(cols);
    let cw = tiles.iter().map(|t| t.size[0]).max().unwrap_or(1);
    let ch = tiles.iter().map(|t| t.size[1]).max().unwrap_or(1);
    let (w, h) = (cols * cw + (cols - 1) * GAP, rows * ch + rows.saturating_sub(1) * GAP);
    let mut out = vec![egui::Color32::from_gray(20); w * h];
    for (i, t) in tiles.iter().enumerate() {
        let (ox, oy) = ((i % cols) * (cw + GAP), (i / cols) * (ch + GAP));
        for y in 0..t.size[1] {
            let row = &t.pixels[y * t.size[0]..][..t.size[0]];
            out[(oy + y) * w + ox..][..t.size[0]].copy_from_slice(row);
        }
    }
    egui::ColorImage::new([w, h], out)
}

/// Splits the subject's box into a 3x3 grid and names the cells where the
/// model is clearly missing material or has too much.
fn region_report(reference: &[bool], model: &[bool], w: usize, bbox: [f32; 4]) -> Vec<Value> {
    const ROWS: [&str; 3] = ["top", "middle", "bottom"];
    const COLS: [&str; 3] = ["left", "center", "right"];
    let h = reference.len() / w.max(1);
    let [x0, y0, x1, y1] = bbox;
    let mut cells = [[(0usize, 0usize, 0usize); 3]; 3];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = ((x as f32 - x0) / (x1 - x0 + 1.0), (y as f32 - y0) / (y1 - y0 + 1.0));
            let (cx, cy) = ((fx * 3.0).floor().clamp(0.0, 2.0) as usize, (fy * 3.0).floor().clamp(0.0, 2.0) as usize);
            let c = &mut cells[cy][cx];
            match (reference[y * w + x], model[y * w + x]) {
                (true, true) => c.0 += 1,
                (true, false) => c.1 += 1,
                (false, true) => c.2 += 1,
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (cy, row) in cells.iter().enumerate() {
        for (cx, &(both, miss, extra)) in row.iter().enumerate() {
            let union = (both + miss + extra) as f32;
            if union < 20.0 {
                continue;
            }
            let (m, e) = (miss as f32 / union, extra as f32 / union);
            if m > 0.15 || e > 0.15 {
                out.push(json!({
                    "region": format!("{} {}", ROWS[cy], COLS[cx]),
                    "missing": round(m),
                    "extra": round(e),
                }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_parse() {
        let (_, d, o, _) = parse_view(&json!("front")).unwrap();
        assert!(o && (d - Vec3::Z).length() < 1e-5);
        let (_, d, _, _) = parse_view(&json!("right")).unwrap();
        assert!((d - Vec3::X).length() < 1e-5);
        let (_, d, o, _) = parse_view(&json!({ "yaw": 0, "pitch": 90, "ortho": true })).unwrap();
        assert!(o && (d - Vec3::Y).length() < 1e-5);
        assert_eq!(up_for(d), Vec3::new(0.0, 0.0, -1.0));
        assert!(parse_view(&json!("sideways")).is_err());
        // The iso view looks down at about 52 degrees from the south-east
        // through a long lens; it can be turned.
        let (_, d, o, fov) = parse_view(&json!("iso")).unwrap();
        assert!(!o && fov < 25.0 && (d.y - 52f32.to_radians().sin()).abs() < 1e-4 && d.x > 0.0 && d.z > 0.0);
        let (_, d, _, fov2) = parse_view(&json!({ "view": "iso", "yaw": 135 })).unwrap();
        assert!(d.x > 0.0 && d.z < 0.0 && fov2 == fov);
    }

    #[test]
    fn mask_from_background_and_bbox() {
        let (w, h) = (20, 10);
        let mut px = vec![egui::Color32::WHITE; w * h];
        for y in 2..6 {
            for x in 5..9 {
                px[y * w + x] = egui::Color32::from_rgb(200, 30, 30);
            }
        }
        let img = egui::ColorImage::new([w, h], px);
        let (mask, mode) = subject_mask(&img, 40);
        assert_eq!(mode, "background color");
        assert_eq!(mask_bbox(&mask, w), Some([5, 2, 8, 5]));
        assert_eq!(resize_mask(&mask, [w, h], [10, 5]).iter().filter(|m| **m).count(), 4);
    }

    /// A close-up compares just the square around a point, lined up as
    /// for the whole model: the reference is a big disc with a dark small
    /// disc (an "eye") on its upper right rim, the model a sphere with a
    /// small sphere in the same place.
    #[test]
    fn closeup_compare_crops_both_panels() {
        use super::super::recorder::tests::{frame, mcp, ok, settle};
        let Some(rs) = crate::render::test_support::headless() else {
            eprintln!("skipped: no GPU adapter for the editor's render state");
            return;
        };
        let (mut app, tx) = ClideApp::headless(rs);
        let ctx = egui::Context::default();
        let (app, ctx, tx) = (&mut app, &ctx, &tx);
        frame(app, ctx, Vec::new());
        // 400 pixels per unit; the big disc (radius 0.5) at pixel (256, 280),
        // the eye (radius 0.1) at world (0.35, 0.35).
        let (px, cx, cy) = (400.0f32, 256.0f32, 280.0f32);
        let eye = (cx + 0.35 * px, cy - 0.35 * px);
        let img = image::RgbaImage::from_fn(512, 512, |x, y| {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if (fx - eye.0).powi(2) + (fy - eye.1).powi(2) <= (0.1 * px).powi(2) {
                image::Rgba([40, 30, 30, 255])
            } else if (fx - cx).powi(2) + (fy - cy).powi(2) <= (0.5 * px).powi(2) {
                image::Rgba([170, 140, 120, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        });
        let dir = std::env::temp_dir().join(format!("clide3d_closeup_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("eye.png");
        img.save(&path).unwrap();
        ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 160, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }, { "type": "sphere", "center": [0.35, 0.35, 0], "radius": 0.1, "blend": 0 }] }));
        settle(app, ctx);
        ok(app, ctx, tx, "set_reference", json!({ "path": path }));
        let time = |app: &mut ClideApp, args: Value| {
            let start = std::time::Instant::now();
            let out = app.compare_reference(&args).unwrap();
            (start.elapsed().as_secs_f64(), serde_json::from_str::<Value>(out[0]["text"].as_str().unwrap()).unwrap())
        };
        let (whole_s, whole) = time(app, json!({ "view": "front" }));
        // Without center the reply is the whole-model compare, as before.
        let keys: Vec<&String> = whole.as_object().unwrap().keys().collect();
        // (Stage 2 added the slot and the contour distance.)
        assert_eq!(keys, ["aligned_by", "contour", "contour_note", "extra_fraction", "image", "missing_fraction", "overlap_iou", "pixel_to_world", "regions", "slot", "view"], "{whole}");
        let (close_s, close) = time(app, json!({ "view": "front", "center": [0.35, 0.35, 0.0], "radius": 0.15 }));
        eprintln!("whole {whole_s:.3} s IoU {}, close-up {close_s:.3} s: {close}", whole["overlap_iou"]);
        let c = &close["closeup"];
        let crop: Vec<f64> = c["crop_px"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let mid = ((crop[0] + crop[2]) * 0.5, (crop[1] + crop[3]) * 0.5);
        // The panel is the reference's own size here (k = 1).
        assert!((mid.0 - eye.0 as f64).abs() <= 1.0 && (mid.1 - eye.1 as f64).abs() <= 1.0, "crop centred at {mid:?}, eye at {eye:?}");
        assert!((crop[2] - crop[0] - 0.3 * px as f64).abs() < 3.0, "{crop:?}");
        assert!((c["reference_px_per_unit"].as_f64().unwrap() - px as f64).abs() < 4.0, "{c}");
        assert_eq!(close["pixel_to_world"]["panel_size"], json!([512, 512]));
        assert_eq!(c["interior"], false);
        let iou = close["overlap_iou"].as_f64().unwrap();
        assert!(iou >= 0.9, "close-up IoU {iou}");
        // Same camera, other point: the crop follows; a crop of a few
        // reference pixels, out-of-range sizes and half a close-up are refused.
        let (_, inner) = time(app, json!({ "view": "front", "center": [0.0, 0.0, 0.0], "radius": 0.1, "size": 128 }));
        assert_eq!((inner["closeup"]["interior"].clone(), inner["pixel_to_world"]["panel_size"].clone()), (json!(true), json!([128, 128])), "{inner}");
        for bad in [json!({ "center": [0.35, 0.35, 0], "radius": 0.005 }), json!({ "center": [0.35, 0.35, 0], "radius": 0.15, "size": 2000 }), json!({ "radius": 0.15 }), json!({ "center": [0.35, 0.35, 0] }), json!({ "center": [9, 9, 0], "radius": 0.1 })] {
            let (v, err) = mcp(app, ctx, tx, "compare_reference", bad.clone());
            assert!(err, "{bad}: {v}");
        }
        if std::env::var("CLIDE_PERF").is_ok() {
            assert!(close_s <= 1.5 * whole_s, "close-up {close_s} s, whole {whole_s} s");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A close-up is interior only inside both outlines: one where the
    /// reference has a part the model lacks (no pixel in neither mask) is
    /// where the silhouette matters, so it is not interior.
    #[test]
    fn interior_means_inside_both_outlines() {
        let size = 40;
        let reference = vec![true; size * size];
        let lacking: Vec<bool> = (0..size * size).map(|i| i / size >= 10).collect();
        let (.., both) = overlap(&reference, &lacking, size, size);
        assert!(!interior(both, size), "a missing part is not interior");
        let (.., both) = overlap(&reference, &reference, size, size);
        assert!(interior(both, size));
        let empty = vec![false; size * size];
        let (.., both) = overlap(&empty, &empty, size, size);
        assert!(!interior(both, size), "outside both is not interior");
    }

    #[test]
    fn regions_name_missing_parts() {
        // Reference fills the box; the model lacks its top row of cells.
        let (w, h) = (30, 30);
        let reference = vec![true; w * h];
        let model: Vec<bool> = (0..w * h).map(|i| i / w >= 10).collect();
        let r = region_report(&reference, &model, w, [0.0, 0.0, 29.0, 29.0]);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|c| c["region"].as_str().unwrap().starts_with("top")));
    }
}
