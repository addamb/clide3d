//! The agent's eyes: a reference image, off-screen renders from any angle,
//! and a silhouette comparison of the model against the reference.
//!
//! The comparison lines the model up with the reference automatically: it
//! scales an orthographic camera so the model's height on screen matches the
//! subject's height in the image, and centers them on each other. It then
//! reports how well the outlines overlap and where they differ, plus a
//! pixel-to-world mapping so the agent can turn image features into
//! coordinates.

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
pub(crate) const BACKGROUND: [u8; 3] = [96, 100, 106];

pub(super) struct Reference {
    pub image: egui::ColorImage,
    pub texture: Option<egui::TextureHandle>,
    /// The viewport pane that shows it as an overlay.
    pub view: ViewKind,
    pub opacity: f32,
    pub show: bool,
    /// Which pixels belong to the subject (not the background).
    mask: Vec<bool>,
    /// Subject bounds in pixels: min x, min y, max x, max y (inclusive).
    bbox: [usize; 4],
    path: PathBuf,
}

/// A camera for one off-screen render.
/// The camera and mapping that line the model up with the reference.
struct Align {
    view_name: String,
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
    match_width: bool,
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

pub(super) const VIEW_NAMES: &str = "front, back, right (camera on +X, Maya's side view), left, top, bottom, three_quarter, three_quarter_back, iso";

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
        Value::Object(_) => {
            let yaw = v["yaw"].as_f64().unwrap_or(0.0) as f32;
            let pitch = v["pitch"].as_f64().unwrap_or(0.0) as f32;
            (format!("yaw {yaw} pitch {pitch}"), yaw, pitch, false)
        }
        _ => return Err("a view is a name, {yaw, pitch}, or {view: name}; either object may add center and radius".into()),
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
        match self.world_bounds(&self.scene.children(None)) {
            Some((lo, hi)) => ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.05)),
            None => (Vec3::ZERO, 1.0),
        }
    }

    /// A callback that draws every visible mesh. `mask` draws flat white.
    fn scene_callback(&self, view_proj: Mat4, eye: Vec3, mask: bool, grid: bool) -> ViewportCallback {
        scene_callback_of(&self.scene, &self.meshes, view_proj, eye, mask, grid)
    }

    fn render_shot(&self, shot: &Shot, w: usize, h: usize, mask: bool, grid: bool) -> Result<egui::ColorImage, String> {
        self.render_shot_extras(shot, w, h, mask, grid, Extras::default())
    }

    pub(super) fn render_shot_extras(&self, shot: &Shot, w: usize, h: usize, mask: bool, grid: bool, ex: Extras) -> Result<egui::ColorImage, String> {
        let view_proj = shot.view_proj(w, h);
        let mut cb = self.scene_callback(view_proj, shot.eye, mask, grid);
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
        Ok(img)
    }

    /// `set_reference` tool.
    pub(super) fn set_reference(&mut self, args: &Value) -> ToolResult {
        if args["clear"].as_bool().unwrap_or(false) {
            self.reference = None;
            return Ok(text("Reference cleared."));
        }
        if args.get("path").is_none_or(Value::is_null) {
            let r = self.reference.as_mut().ok_or("no reference loaded; pass path")?;
            apply_display(r, args)?;
            return Ok(text(json!({ "view": r.view.name(), "opacity": r.opacity, "show": r.show })));
        }
        let path = image_path(args["path"].as_str().ok_or("path must be a string")?)?;
        let image = load_image(&path).map_err(|_| format!("{} is not a readable PNG or JPEG image", path.display()))?;
        let threshold = args["threshold"].as_f64().unwrap_or(40.0).clamp(1.0, 255.0) as u8;
        let (mask, mode) = subject_mask(&image, threshold);
        let bbox = mask_bbox(&mask, image.size[0]).ok_or(
            "could not find a subject in the image; use a plain background or a PNG with transparency, or change threshold",
        )?;
        let coverage = mask.iter().filter(|m| **m).count() as f32 / mask.len() as f32;
        let mut r = Reference { image, texture: None, view: ViewKind::Front, opacity: 0.5, show: true, mask, bbox, path };
        apply_display(&mut r, args)?;
        let info = json!({
            "path": r.path,
            "size": r.image.size,
            "subject_bbox_px": r.bbox,
            "subject_fraction": round(coverage),
            "mask_from": mode,
            "palette": palette(&r.image, &r.mask),
            "overlay_view": r.view.name(),
            "note": "The image below is the subject mask (white = subject). If it is wrong, call again with another threshold.",
        });
        let preview = mask_image(&r.mask, r.image.size);
        self.reference = Some(r);
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

    fn render_views_now(&mut self, args: &Value) -> ToolResult {
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
        let with_ref = args["include_reference"].as_bool().unwrap_or(true) && self.reference.is_some();
        let extras = Extras {
            bones: args["show_bones"].as_bool().unwrap_or(false),
            weights: self.weight_target(&args["weights"])?,
            lighting: args.get("lighting").filter(|v| !v.is_null()).map(|v| self.parse_lighting(v)).transpose()?,
            ao: args.get("ambient_occlusion").filter(|v| !v.is_null()).map(|v| super::tools::parse_ao(v, self.ao)).transpose()?,
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
        if with_ref {
            let r = self.reference.as_ref().expect("checked");
            tiles.push(fit(&r.image, size, size));
            names.push("reference".to_string());
        }
        for v in &views {
            let shot = Shot::framing(v, center, radius)?;
            tiles.push(self.render_shot_extras(&shot, size, size, false, grid, extras.clone())?);
            names.push(shot.name);
        }
        let cols = (tiles.len() as f32).sqrt().ceil() as usize;
        let sheet = contact_sheet(&tiles, cols);
        let mut out = text(json!({
            "tiles": names,
            "layout": format!("{cols} per row, left to right, top to bottom"),
            "framed_center": [center.x, center.y, center.z],
            "framed_radius": radius,
            "frame": self.meshes.frame,
            "fine_mesh_errors": self.fine_errors_json(),
            "device": raster_device(&self.render_state),
        }));
        out.extend(super::tools::encode_png(&sheet)?);
        Ok(out)
    }

    /// Lines the model up with the reference from one view: an orthographic
    /// camera where the model's height (or width) on screen matches the
    /// subject's in the image, centered on it.
    fn align(&self, args: &Value) -> Result<Align, String> {
        let r = self.reference.as_ref().ok_or("no reference image; call set_reference first")?;
        let view = args.get("view").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| Value::from("front"));
        let (view_name, dir, _, _) = parse_view(&view)?;
        let up = up_for(dir);
        let right = up.cross(dir).normalize();
        let up = dir.cross(right);

        // Model extent on screen axes.
        let eval = self.scene.evaluate();
        let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
        for o in self.scene.objects.iter().filter(|o| o.shape.is_mesh()) {
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
        let [bx0, by0, bx1, by1] = bbox;
        let (bw_px, bh_px) = ((bx1 - bx0 + 1.0).max(1.0), (by1 - by0 + 1.0).max(1.0));

        // Pixels per world unit: match heights (or widths for a lying-down subject).
        let match_width = args["match"].as_str() == Some("width");
        let s = if match_width { bw_px / (hi.x - lo.x).max(1e-6) } else { bh_px / (hi.y - lo.y) };
        let mid = (lo + hi) * 0.5;
        let (pxm, pym) = ((bx0 + bx1) * 0.5, (by0 + by1) * 0.5);
        let uc = mid.x - (pxm - w as f32 * 0.5) / s;
        let vc = mid.y + (pym - h as f32 * 0.5) / s;
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
        Ok(Align { view_name, dir, right, up, s, uc, vc, w, h, k, shot, ref_mask, bbox, match_width, plane_center })
    }

    /// `compare_reference` tool.
    pub(super) fn compare_reference(&mut self, args: &Value) -> ToolResult {
        let Align { view_name, right, up, s, w, h, shot, ref_mask, bbox, match_width, plane_center, .. } = self.align(args)?;
        let r = self.reference.as_ref().expect("align checked");
        let [bx0, by0, bx1, by1] = bbox;
        let mask_img = self.render_shot(&shot, w, h, true, false)?;
        let beauty = self.render_shot(&shot, w, h, false, false)?;
        let model: Vec<bool> = mask_img.pixels.iter().map(|p| p.r() > 127).collect();

        let (mut both, mut only_ref, mut only_model) = (0usize, 0usize, 0usize);
        let mut diff = Vec::with_capacity(w * h);
        for (&a, &b) in ref_mask.iter().zip(&model) {
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
        let diff_img = egui::ColorImage::new([w, h], diff);
        let regions = region_report(&ref_mask, &model, w, [bx0, by0, bx1, by1]);

        let origin = plane_center + right * (-(w as f32) * 0.5 / s) + up * (h as f32 * 0.5 / s);
        let round3 = |v: Vec3| [round(v.x), round(v.y), round(v.z)];
        let reference_tile = fit(&r.image, w, h);
        let mut out = text(json!({
            "view": view_name,
            "overlap_iou": round(both as f32 / union),
            "missing_fraction": round(only_ref as f32 / union),
            "extra_fraction": round(only_model as f32 / union),
            "regions": regions,
            "image": "Three panels: reference | model from the same camera | overlap (gray = both, red = in reference but missing from the model, blue = model but not in reference).",
            "pixel_to_world": {
                "panel_size": [w, h],
                "origin": round3(origin),
                "per_px_right": round3(right / s),
                "per_px_down": round3(-up / s),
                "note": "world = origin + x * per_px_right + y * per_px_down for pixel (x, y) of one panel, on the plane through the model's center.",
            },
            "aligned_by": if match_width { "width" } else { "height" },
        }));
        out.extend(super::tools::encode_png(&contact_sheet(&[reference_tile, beauty, diff_img], 3))?);
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
        let a = self.align(args)?;
        let strength = args["strength"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        let min_facing = args["min_facing"].as_f64().unwrap_or(0.3).clamp(0.0, 0.95) as f32;

        // Depth buffer of everything visible, so hidden surfaces get no paint.
        let eval = self.scene.evaluate();
        let mut zbuf = vec![f32::MIN; a.w * a.h];
        for o in self.scene.objects.iter().filter(|o| o.shape.is_mesh()) {
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
        let r = self.reference.as_ref().expect("align checked");
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
        let r = self.reference.as_ref().expect("align checked");
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

    /// Paints the reference over a pane, fitted and centered, if it belongs there.
    pub(super) fn paint_reference(&mut self, ui: &egui::Ui, kind: ViewKind, rect: egui::Rect) {
        let Some(r) = self.reference.as_mut().filter(|r| r.show && r.view == kind) else { return };
        let tex = r
            .texture
            .get_or_insert_with(|| ui.ctx().load_texture("reference", r.image.clone(), egui::TextureOptions::LINEAR));
        let [iw, ih] = r.image.size;
        let scale = (rect.width() / iw as f32).min(rect.height() / ih as f32);
        let area = egui::Rect::from_center_size(rect.center(), egui::vec2(iw as f32, ih as f32) * scale);
        let tint = egui::Color32::from_white_alpha((r.opacity.clamp(0.0, 1.0) * 255.0) as u8);
        ui.painter_at(rect).image(tex.id(), area, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    }
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

fn apply_display(r: &mut Reference, args: &Value) -> Result<(), String> {
    if let Some(v) = args["view"].as_str() {
        r.view = ViewKind::from_name(v).ok_or(format!("unknown overlay view {v}; use persp, top, front, side"))?;
    }
    if let Some(o) = args["opacity"].as_f64() {
        r.opacity = o.clamp(0.0, 1.0) as f32;
    }
    if let Some(s) = args["show"].as_bool() {
        r.show = s;
    }
    Ok(())
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
fn image_path(p: &str) -> Result<PathBuf, String> {
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
fn subject_mask(img: &egui::ColorImage, threshold: u8) -> (Vec<bool>, &'static str) {
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
    let eval = scene.evaluate();
    let mut fills = Vec::new();
    for d in crate::envkit::scatter::draws(scene, &eval).iter().filter(|d| d.object.shape.is_mesh()) {
        push_fills(&mut fills, meshes, d.object, Instance::new(d.world, if mask { [1.0; 3] } else { d.object.color }).with_material(&scene.material_of(d.object)));
    }
    fills.sort_by_key(|(k, _)| *k);
    ViewportCallback {
        slot: 0,
        view_proj,
        grid_model: Mat4::IDENTITY,
        cam_pos: eye,
        fills,
        wires: Vec::new(),
        dynamic: meshes.own_meshes(),
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
