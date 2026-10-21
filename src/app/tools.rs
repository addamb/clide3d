//! Executes MCP tool calls on the UI thread (see `mcp.rs` for the transport).
//!
//! Edits here change `self.scene` directly; the frame's undo diff in
//! `ClideApp::update` records them like any mouse edit.

use std::path::PathBuf;

use base64::Engine;
use glam::Vec3;
use serde_json::{Value, json};

use super::jobs::Started;
use super::{ClideApp, Shading};
use crate::camera::ViewKind;
use crate::mcp::ToolResult;
use crate::clay::{Blob, Clay, MAX_BLOBS};
use crate::scene::{Scene, Shape};

/// Calls that read no meshes: they may run while meshes build, if no
/// earlier call is still waiting (so answers stay in order).
const READ_ONLY: [&str; 1] = ["get_scene"];
/// Calls that may always run at once, even past waiting calls.
const READ_ONLY_ANYTIME: [&str; 3] = ["screenshot", "job_status", "check_export"];

/// Tools that run as background jobs (see `jobs.rs`).
const JOB_TOOLS: [&str; 10] = ["render_film", "check_export", "bake", "export_glb", "export_fbx", "sculpt", "cloth", "fire", "face_detail", "render_animation"];
/// Job tools that edit the scene: later edits wait in line behind them.
const EDIT_JOBS: [&str; 4] = ["sculpt", "cloth", "fire", "face_detail"];

/// Agent screenshots are scaled down to at most this width to stay small.
const SHOT_MAX_WIDTH: usize = 1024;

impl ClideApp {
    /// Runs every queued agent call. Screenshot calls finish on a later frame.
    fn start_job_tool(&mut self, name: &str, args: &Value) -> Result<Started, String> {
        match name {
            "render_film" => self.render_film(args),
            "render_animation" => self.render_animation(args),
            "check_export" => self.check_export(args),
            "bake" => self.bake_tool(args),
            "fire" => self.fire_tool(args),
            "cloth" => self.at_rest(|app| app.cloth_tool(args)),
            "sculpt" => self.at_rest(|app| app.sculpt(args)),
            "face_detail" => self.at_rest(|app| app.face_detail(args)),
            "export_glb" | "export_fbx" => {
                let path = PathBuf::from(args["path"].as_str().ok_or("path is required")?);
                let mut roots = Vec::new();
                for t in args["targets"].as_array().into_iter().flatten() {
                    roots.push(self.resolve(t)?);
                }
                let animations = args["animations"].as_bool().unwrap_or(true);
                let lods = args["lods"].as_bool().unwrap_or(false);
                self.start_export(name == "export_fbx", path, roots, animations, lods, args)
            }
            _ => Err(format!("unknown job tool {name}")),
        }
    }

    pub(super) fn process_mcp(&mut self, ctx: &egui::Context) {
        // While meshes build in the background, calls wait in line (the
        // window stays live) and run in order once the meshes are ready;
        // only calls that read no meshes go ahead of the line.
        let Some(rx) = &self.mcp_rx else { return };
        let incoming: Vec<_> = rx.try_iter().collect();
        for call in incoming {
            let ticket = self.jobs.ticket(call.name.clone(), call.reply.clone());
            self.jobs.queue.push_back((call, ticket));
        }
        let editing = self.jobs.running.iter().any(|j| j.edits);
        let waiting = self.meshes.busy() || !self.jobs.deferred.is_empty() || self.jobs.running.iter().any(|j| j.edits);
        let mut calls = Vec::new();
        let mut keep = std::collections::VecDeque::new();
        for (call, ticket) in self.jobs.queue.drain(..) {
            if !waiting || (!editing && keep.is_empty() && shading_call(&call.name, &call.args)) || (keep.is_empty() && READ_ONLY.contains(&call.name.as_str())) || READ_ONLY_ANYTIME.contains(&call.name.as_str()) {
                calls.push((call, ticket));
            } else {
                keep.push_back((call, ticket));
            }
        }
        self.jobs.queue = keep;
        for (call, ticket) in calls {
            if call.name == "screenshot" {
                self.next_shot_id += 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(self.next_shot_id)));
                let whole = call.args["whole_window"].as_bool().unwrap_or(false);
                self.pending_shots.push((self.next_shot_id, call.reply, whole));
                continue;
            }
            if call.name == "job_status" {
                if let Some(result) = self.job_status(&call.args, &call.reply) {
                    let _ = call.reply.send(result);
                }
                continue;
            }
            // Long work runs in the background with a progress bar.
            if JOB_TOOLS.contains(&call.name.as_str()) {
                self.scene.frame = self.meshes.frame;
                match self.start_job_tool(&call.name, &call.args) {
                    Ok(Started::Job(label, work)) => {
                        self.spawn_job(label, ticket, work);
                        if EDIT_JOBS.contains(&call.name.as_str())
                            && let Some(job) = self.jobs.running.last_mut()
                        {
                            job.edits = true;
                        }
                    }
                    Ok(Started::Now(result)) => self.jobs.answer(ticket, result),
                    Err(e) => self.jobs.answer(ticket, Err(e)),
                }
                continue;
            }
            let result = self.run_tool(&call.name, &call.args);
            // Some tools answer only after the meshes they changed are built.
            if let Some(answer) = self.jobs.defer.take() {
                let mut ticket = ticket;
                ticket.label = format!("{} (answer waits for meshes)", call.name);
                self.jobs.deferred.push_back((ticket, answer));
                continue;
            }
            // The agent may have given up waiting; that is fine.
            self.jobs.answer(ticket, result);
        }
    }

    pub(super) fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_shots.is_empty() {
            return;
        }
        let shots: Vec<(u64, std::sync::Arc<egui::ColorImage>)> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let id = *user_data.data.as_ref()?.downcast_ref::<u64>()?;
                        Some((id, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        let ppp = ctx.pixels_per_point();
        for (id, image) in shots {
            let Some(pos) = self.pending_shots.iter().position(|(p, _, _)| *p == id) else { continue };
            let (_, reply, whole) = self.pending_shots.remove(pos);
            let shot = if whole { (*image).clone() } else { image.region(&self.viewport_rect, Some(ppp)) };
            let max_width = if whole { SHOT_MAX_WIDTH * 3 / 2 } else { SHOT_MAX_WIDTH };
            let _ = reply.send(encode_png(&downscale(&shot, max_width)));
        }
        // Keep frames coming until the screenshot event arrives.
        if !self.pending_shots.is_empty() {
            ctx.request_repaint();
        }
    }

    fn run_tool(&mut self, name: &str, args: &Value) -> ToolResult {
        // A loaded or undone scene has no frame yet.
        self.scene.frame = self.meshes.frame;
        match name {
            "get_scene" => Ok(text(self.scene_json(args))),
            "create_object" => {
                let shape: Shape = serde_json::from_value(args["shape"].clone())
                    .map_err(|_| format!("unknown shape {}", args["shape"]))?;
                if shape == Shape::Fire { return Err("use the fire tool to simulate a Fire object".into()); }
                if shape == Shape::Cloth { return Err("use the cloth tool with parent and pattern to create a garment".into()); }
                let id = self.scene.add(shape);
                let keep = self.selection.clone();
                self.select_only(id);
                // A call that fails leaves nothing behind.
                let mirror = match self.fill_new_object(id, shape, args) {
                    Ok(m) => m,
                    Err(e) => {
                        self.scene.remove_tree(id);
                        self.selection = keep;
                        self.prune_selection();
                        return Err(e);
                    }
                };
                // Clay answers once its mesh is built (the summary reads it).
                self.defer_reply(move |app| {
                    let mut out = if shape == Shape::Clay {
                        app.clay_summary(id)
                    } else if matches!(shape, Shape::Kit | Shape::Terrain) {
                        app.env_summary(id)
                    } else {
                        object_json_in(&app.scene, id)
                    };
                    if shape == Shape::Hair { out["hair"] = app.hair_summary(id); }
                    if let Some(m) = mirror {
                        out["mirror"] = object_json_in(&app.scene, m);
                    }
                    Ok(text(out))
                })
            }
            "edit_clay" => self.edit_clay(args),
            "kit" => self.kit_tool(args),
            "scatter" => self.scatter_tool(args),
            // Kitbash parts and detail passes place onto rest-pose surfaces.
            "kitbash" => self.at_rest(|app| crate::kitbash::tool::kitbash(&mut app.scene, &mut app.meshes, args).map(text)),
            "detail_pass" => self.at_rest(|app| crate::kitbash::tool::detail_pass(&mut app.scene, &mut app.meshes, args).map(text)),
            "edit_terrain" => self.edit_terrain(args),
            // Points in and out are always rest-pose points, even while a
            // clip plays (else strokes land where the pose moved things).
            "damage" => self.at_rest(|app| app.damage_tool(args)),
            "wear" => self.at_rest(|app| app.damage_tool(args)),
            "paint" => self.at_rest(|app| app.paint(args)),
            "decal" => self.at_rest(|app| app.decal(args)),
            "project_reference" => self.project_reference(args),
            "get_clay" => {
                let id = self.resolve(&args["target"])?;
                let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
                let blobs: Vec<Value> = clay
                    .blobs
                    .iter()
                    .enumerate()
                   .map(|(i, b)| {
                        let mut v = if crate::replies::verbose(args) {serde_json::to_value(b).unwrap_or_default()} else {json!({"name":b.name,"material":b.material,"bone":b.bone,"op":b.op})};
                        v["index"] = json!(i);
                        v
                    })
                    .collect();
                let total=blobs.len();let offset=args["offset"].as_u64().unwrap_or(0) as usize;let limit=args["limit"].as_u64().unwrap_or(if crate::replies::verbose(args){1024}else{24}) as usize;
                let blobs:Vec<_>=blobs.into_iter().skip(offset).take(limit).collect();
                Ok(text(json!({ "target": id, "blob_count":total,"offset":offset,"next_offset":offset.checked_add(limit).filter(|n|*n<total), "resolution": clay.resolution, "detail_cell": clay.detail_cell, "detail_regions": clay.detail_regions(), "blobs": blobs, "mask_channels": clay.masks.keys().collect::<Vec<_>>(), "decals": clay.decals })))
            }
            "set_reference" => self.set_reference(args),
            "rig" => self.rig_tool(args),
            "animate" => self.animate(args),
            "playback" => self.playback(args),
            "render_views" => self.render_views(args),
            "compare_reference" => {
                self.meshes.sync(&self.scene);
                self.compare_reference(args)
            }
            "update_object" => {
                let id = self.resolve(&args["target"])?;
                self.apply_props(id, args)?;
                // Parts on it and mirror copies of it follow at once.
                crate::pose::sync(&mut self.scene);
                let shape = self.scene.get(id).map(|o| o.shape);
                let mut out = if matches!(shape, Some(Shape::Kit | Shape::Terrain | Shape::Scatter)) { self.env_summary(id) } else { object_json_in(&self.scene, id) };
                if shape == Some(Shape::Hair) { out["hair"] = self.hair_summary(id); }
                if ["align", "frame", "grip"].iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) {
                    let w = crate::pose::at_rest(&mut self.scene, |s| s.world_matrix(id));
                    let axis = |v: Vec3| round3(w.transform_vector3(v).normalize_or_zero());
                    out["world_axes_at_rest"] = json!({ "x": axis(Vec3::X), "y": axis(Vec3::Y), "z": axis(Vec3::Z) });
                }
                if self.scene.get(id).is_some_and(|o| o.link.is_some()) {
                    out["note"] = json!("a linked mirror copy takes its shape and placement from its source; mirror_object {target, action: unlink} frees it");
                }
                Ok(text(out))
            }
            "mirror_object" => self.at_rest(|app| app.mirror_object(args)),
            "pose_bone" => self.at_rest(|app| app.pose_bone(args)),
            "set_material" => self.set_material(args),
            "surface_point" => self.at_rest(|app| app.surface_point(args)),
            "create_human" => self.create_human(args),
            "set_human" => self.set_human(args),
            "human_controls" => self.human_controls(args),
            "face_hair" => self.face_hair(args),
            "delete_object" => {
                let id = self.resolve(&args["target"])?;
                let mut deleted = self.scene.descendants(id);
                deleted.insert(0, id);
                self.scene.remove_tree(id);
                self.prune_selection();
                Ok(text(json!({ "deleted": deleted })))
            }
            "duplicate_object" => {
                let id = self.resolve(&args["target"])?;
                let copy = self.scene.duplicate_tree(id).expect("resolved");
                self.select_only(copy);
                Ok(text(object_json_in(&self.scene, copy)))
            }
            "select_object" => {
                let mut ids = Vec::new();
                if let Some(t) = args.get("target").filter(|t| !t.is_null()) {
                    ids.push(self.resolve(t)?);
                }
                for t in args["targets"].as_array().into_iter().flatten() {
                    ids.push(self.resolve(t)?);
                }
                if args["add"].as_bool().unwrap_or(false) {
                    for id in ids {
                        self.selection.retain(|&s| s != id);
                        self.selection.push(id);
                    }
                } else {
                    self.selection = ids;
                }
                Ok(text(json!({ "selection": self.selection, "lead": self.lead() })))
            }
            "group_objects" => {
                let mut ids = Vec::new();
                for t in args["targets"].as_array().into_iter().flatten() {
                    ids.push(self.resolve(t)?);
                }
                if ids.is_empty() {
                    ids = self.selection.clone();
                }
                if ids.is_empty() {
                    return Err("nothing to group: pass targets or select objects".into());
                }
                let g = self.scene.group(&ids);
                if let Some(n) = args["name"].as_str() {
                    self.scene.get_mut(g).expect("just created").name = n.to_string();
                }
                self.select_only(g);
                Ok(text(object_json_in(&self.scene, g)))
            }
            "parent_objects" => {
                let parent = match args.get("parent") {
                    None | Some(Value::Null) => None,
                    Some(p) => Some(self.resolve(p)?),
                };
                let mut moved = Vec::new();
                for t in args["children"].as_array().ok_or("children must be a list")? {
                    let id = self.resolve(t)?;
                    self.scene.reparent(id, parent)?;
                    moved.push(id);
                }
                Ok(text(json!({ "parent": parent, "children": moved })))
            }
            "set_pivot" => {
                let id = self.resolve(&args["target"])?;
                if args["center"].as_bool().unwrap_or(false) {
                    let keep = std::mem::replace(&mut self.selection, vec![id]);
                    self.center_pivot_selected();
                    self.selection = keep;
                } else {
                    let p = vec3(args, "pivot")?.ok_or("pass pivot [x, y, z] (world space) or center: true")?;
                    self.scene.set_pivot_world(id, p);
                }
                Ok(text(object_json_in(&self.scene, id)))
            }
            "set_camera" => {
                match args["preset"].as_str() {
                    Some("iso") => {
                        let (center, radius) = self.scene_sphere();
                        let cam = &mut self.cams.persp;
                        cam.target = center;
                        cam.yaw = super::eyes::ISO.0.to_radians();
                        cam.pitch = super::eyes::ISO.1.to_radians();
                        cam.distance = (radius * 2.2).clamp(1.0, 1000.0);
                    }
                    Some(other) => return Err(format!("unknown camera preset {other}; use iso")),
                    None => {}
                }
                let cam = &mut self.cams.persp;
                if let Some(v) = vec3(args, "look_at")? {
                    cam.target = v;
                }
                if let Some(y) = args["yaw"].as_f64() {
                    cam.yaw = (y as f32).to_radians();
                }
                if let Some(p) = args["pitch"].as_f64() {
                    cam.pitch = (p as f32).clamp(-89.0, 89.0).to_radians();
                }
                if let Some(d) = args["distance"].as_f64() {
                    cam.distance = (d as f32).clamp(0.1, 1000.0);
                }
                Ok(text(self.camera_json()))
            }
            "frame_view" => {
                let ids = match args.get("target") {
                    None | Some(Value::Null) => self.scene.children(None),
                    Some(t) => vec![self.resolve(t)?],
                };
                let keep = (self.hover_view, self.active_view);
                if let Some(v) = args["view"].as_str() {
                    self.hover_view = Some(ViewKind::from_name(v).ok_or(format!("unknown view {v}"))?);
                } else {
                    self.hover_view = Some(ViewKind::Persp);
                }
                self.frame_ids(&ids);
                (self.hover_view, self.active_view) = keep;
                Ok(text(self.camera_json()))
            }
            "run_mel" => {
                let line = args["command"].as_str().ok_or("command is required")?;
                let was_python = std::mem::replace(&mut self.cmd_python, false);
                let out = self.run_command_line(line);
                self.cmd_python = was_python;
                self.cmd_result = out.clone();
                if out.starts_with("// Error") {
                    return Err(out);
                }
                Ok(text(if out.is_empty() { "OK".to_string() } else { out }))
            }
            "set_display" => {
                if let Some(s) = args["shading"].as_str() {
                    self.shading = match s {
                        "wireframe" => Shading::Wireframe,
                        "shaded" => Shading::Shaded,
                        "wireframe_on_shaded" => Shading::WireOnShaded,
                        other => return Err(format!("unknown shading {other}")),
                    };
                }
                if let Some(g) = args["grid"].as_bool() {
                    self.show_grid = g;
                }
                if let Some(b) = args["bones"].as_bool() {
                    self.show_bones = b;
                }
                if let Some(b) = args["baked"].as_bool() {
                    self.meshes.show_baked = b;
                }
                if let Some(b) = args["fine"].as_bool() {
                    self.meshes.show_fine = b;
                }
                match args["layout"].as_str() {
                    Some("four") => self.four_view = true,
                    Some("single") => self.four_view = false,
                    Some(other) => return Err(format!("unknown layout {other}")),
                    None => {}
                }
                if let Some(v) = args["view"].as_str() {
                    self.single_view = ViewKind::from_name(v).ok_or(format!("unknown view {v}"))?;
                    self.active_view = self.single_view;
                }
                Ok(text(json!({
                    "shading": format!("{:?}", self.shading),
                    "grid": self.show_grid,
                    "bones": self.show_bones,
                    "layout": if self.four_view { "four" } else { "single" },
                    "view": self.single_view.name(),
                })))
            }
            "undo" => {
                let had = !self.undo.is_empty();
                self.undo();
                Ok(text(if had { "Undone." } else { "Nothing to undo." }))
            }
            "redo" => {
                let had = !self.redo.is_empty();
                self.redo();
                Ok(text(if had { "Redone." } else { "Nothing to redo." }))
            }
            "new_scene" => {
                self.scene = Scene::default();
                self.selection.clear();
                Ok(text("Scene cleared. Use undo to bring it back."))
            }
            "open_scene" => {
                let path = agent_path(args["path"].as_str().ok_or("path is required")?)?;
                // Generic error: never echo another file's contents back to the agent.
                self.scene = load_scene(&path).map_err(|_| format!("{} is not a readable Clide3D scene", path.display()))?;
                self.selection.clear();
                self.fit_range_to_clips();
                self.status = format!("Agent opened {}", path.display());
                self.file_path = Some(path);
                Ok(text(self.scene_json(args)))
            }
            "save_scene" => {
                let path = match args["path"].as_str() {
                    Some(p) => agent_path(p)?,
                    None => self.file_path.clone().ok_or("no current file; pass a path")?,
                };
                // Only overwrite files that are already scenes, so an agent (or any
                // local process reaching the MCP port) cannot clobber other .json files.
                if path.exists() && load_scene(&path).is_err() {
                    return Err(format!("{} exists and is not a Clide3D scene; refusing to overwrite", path.display()));
                }
                self.save_to(path.clone())?;
                Ok(text(format!("Saved {}", path.display())))
            }
            _ => Err(format!("unknown tool: {name}")),
        }
    }

    pub(super) fn apply_props(&mut self, id: u64, args: &Value) -> Result<(), String> {
        if let Some(hair) = args.get("hair").filter(|v| !v.is_null()) { super::look::validate_hair_keys(hair)?; }
        let translate = vec3(args, "translate")?;
        let rotate = vec3(args, "rotate")?;
        let scale = vec3(args, "scale")?;
        let color = vec3(args, "color")?;
        match args.get("bone") {
            None => {}
            Some(Value::Null) => self.scene.get_mut(id).ok_or("object vanished")?.bone = None,
            Some(b) => {
                let b = b.as_str().ok_or("bone must be a bone name or null")?;
                let o = self.scene.get(id).ok_or("object vanished")?;
                let rig = o.parent.and_then(|p| self.scene.get(p)).and_then(|p| p.rig.as_ref()).ok_or("bone needs a parent with a rig; pass parent (the rigged clay object) first")?;
                if rig.bone_index(b).is_none() {
                    return Err(format!("the parent's rig has no bone {b}; bones: {:?}", rig.bones.iter().map(|x| &x.name).collect::<Vec<_>>()));
                }
                self.scene.get_mut(id).expect("checked").bone = Some(b.to_string());
            }
        }
        // World space: place it where it should be seen now (in the current
        // pose, for an object on a bone); the local values follow.
        let world = match args["space"].as_str().unwrap_or("local") {
            "local" => false,
            "world" => true,
            other => return Err(format!("unknown space {other}; use local or world")),
        };
        if world && (translate.is_some() || rotate.is_some() || scale.is_some()) {
            let (s0, r0, t0) = self.scene.world_matrix(id).to_scale_rotation_translation();
            let r = rotate.map_or(r0, crate::clay::rotation);
            let m = glam::Mat4::from_scale_rotation_translation(scale.unwrap_or(s0), r, translate.unwrap_or(t0));
            self.scene.set_world_matrix(id, m);
        }
        let obj = self.scene.get_mut(id).ok_or("object vanished")?;
        if let Some(name) = args["name"].as_str() {
            obj.name = name.to_string();
        }
        if !world {
            if let Some(v) = translate {
                obj.translation = v;
            }
            if let Some(v) = rotate {
                obj.set_euler_degrees(v);
            }
            if let Some(v) = scale {
                obj.scale = v;
            }
        }
        if let Some(v) = color {
            obj.color = v.clamp(Vec3::ZERO, Vec3::ONE).into();
        }
        if let Some(v) = args["visible"].as_bool() {
            obj.visible = v;
        }
        self.apply_look(id, args)?;
        self.apply_env_props(id, args)?;
        // A kitbash group moved on purpose stays put relative to its object.
        if (translate.is_some() || rotate.is_some() || scale.is_some()) && self.scene.get(id).is_some_and(|o| o.follow.is_some()) {
            crate::pose::refollow(&mut self.scene, id);
        }
        self.apply_align(id, args)?;
        Ok(())
    }

    /// `edit_clay`: replace, then remove, then update, then add blobs.
    fn edit_clay(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let mut clay = obj.clay.clone().unwrap_or_default();
        if let Some(v) = args.get("shell") {
            clay.shell = if v.is_null() { None } else {
                let t = v.as_f64().ok_or("shell must be a positive thickness or null")? as f32;
                if !t.is_finite() || t <= 0.0 { return Err("shell must be positive".into()); }
                Some(t)
            };
        }
        if let Some(v) = args["sharp"].as_bool() { clay.sharp = v; }
        if let Some(r) = args["resolution"].as_u64() {
            clay.resolution = r as u32;
        }
        if args["clear_detail"].as_bool().unwrap_or(false) {
            clay.detail = None;
        }
        if let Some(c) = args["detail_cell"].as_f64() {
            clay.set_detail_cell(c as f32)?;
            clay.fine_check()?;
        }
        if let Some(v) = args.get("detail_regions").filter(|v| !v.is_null()) {
            clay.set_detail_regions(&parse_regions(v)?)?;
            clay.fine_check()?;
        }
        if args.get("replace").is_some_and(|v| !v.is_null()) {
            clay.blobs = parse_blobs(&args["replace"])?;
        }
        // Remove by position from the back so earlier indices stay valid.
        let mut gone: Vec<usize> = Vec::new();
        for key in args["remove"].as_array().into_iter().flatten() {
            gone.push(clay.find(key).ok_or_else(|| format!("no blob {key}"))?);
        }
        gone.sort_unstable();
        gone.dedup();
        for i in gone.into_iter().rev() {
            clay.blobs.remove(i);
        }
        for patch in args["update"].as_array().into_iter().flatten() {
            let key = patch.get("blob").ok_or("each update needs blob: index or name")?;
            let i = clay.find(key).ok_or_else(|| format!("no blob {key}"))?;
            let mut v = serde_json::to_value(&clay.blobs[i]).map_err(|e| e.to_string())?;
            for (k, val) in patch.as_object().into_iter().flatten().filter(|(k, _)| k.as_str() != "blob") {
                v[k] = val.clone();
            }
            clay.blobs[i] = serde_json::from_value(v).map_err(|e| format!("update of blob {key}: {e}"))?;
            clay.blobs[i].validate()?;
        }
        // `before` (a blob index or name) inserts there instead of at the end,
        // so a new filler blob does not cover detail blobs that came later.
        let before = match args.get("before").filter(|v| !v.is_null()) {
            Some(k) => Some(clay.find(k).ok_or_else(|| format!("no blob {k} for before"))?),
            None => None,
        };
        let added = parse_blobs(&args["add"])?;
        match before {
            Some(i) => {
                clay.blobs.splice(i..i, added);
            }
            None => clay.blobs.extend(added),
        }
        if clay.blobs.len() > MAX_BLOBS {
            return Err(format!("at most {MAX_BLOBS} blobs per clay object"));
        }
        clay.sanitize();
        self.scene.get_mut(id).expect("resolved").clay = Some(clay);
        self.defer_reply(move |app| Ok(text(app.clay_summary(id))))
    }

    /// `sculpt`: apply brush strokes in order. Points are world space unless
    /// space is "local"; they are mapped into the clay object's own space.
    /// `sculpt`: strokes are prepared here (world to clay space, snapping,
    /// in the rest pose) and applied on a job thread, so big strokes never
    /// block the window; the result installs if the clay did not change
    /// meanwhile (calls that edit wait in line while it runs).
    fn sculpt(&mut self, args: &Value) -> Result<Started, String> {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let name = obj.name.clone();
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let raws = args["strokes"].as_array().ok_or("strokes must be a list")?;
        if raws.is_empty() || raws.len() > 64 {
            return Err("pass 1 to 64 strokes".into());
        }
        let before = obj.clay.clone().unwrap_or_default();
        self.meshes.sync(&self.scene);
        let snap = self.snapper(id).ok();
        let mut strokes = Vec::new();
        for (n, raw) in raws.iter().enumerate() {
            let mut raw = raw.clone();
            if raw.get("points").is_none()
                && let Some(p) = raw.get("point").cloned()
            {
                raw["points"] = json!([p]);
            }
            let snapped = raw["snap"].as_bool().or(args["snap"].as_bool()).unwrap_or(false);
            let mut s: crate::sculpt::Stroke = serde_json::from_value(raw).map_err(|e| format!("stroke {n}: {e}"))?;
            for p in &mut s.points {
                *p = to_local.transform_point3(*p);
            }
            if snapped {
                let snap = snap.as_ref().ok_or("snap needs a clay surface")?;
                for p in &mut s.points {
                    *p = snap(*p).map_or(*p, |(q, _)| q);
                }
            }
            s.radius *= radius_scale;
            if matches!(s.brush, crate::sculpt::Brush::Crease | crate::sculpt::Brush::Layer) {
                s.strength *= radius_scale;
            }
            s.delta = s.delta.map(|d| to_local.transform_vector3(d));
            s.normal = s.normal.map(|v| to_local.inverse().transpose().transform_vector3(v));
            strokes.push(s);
        }
        drop(snap);
        let label = format!("Sculpt {name} ({} stroke{})", strokes.len(), if strokes.len() == 1 { "" } else { "s" });
        Ok(Started::Job(
            label,
            Box::new(move |progress| {
                let mut clay = before.clone();
                let mut reports = Vec::new();
                for (n, s) in strokes.iter().enumerate() {
                    progress.check()?;
                    progress.set(n as f32 / strokes.len() as f32);
                    let r = crate::sculpt::apply(&mut clay, s).map_err(|e| format!("stroke {n}: {e} (no stroke was applied)"))?;
                    if s.mirror {
                        crate::sculpt::apply(&mut clay, &s.mirrored()).map_err(|e| format!("stroke {n} (mirror): {e} (no stroke was applied)"))?;
                    }
                    reports.push(json!({ "stroke": n, "changed_points": r.changed_points, "radius_used": round(r.radius / radius_scale) }));
                }
                Ok(Box::new(move |app: &mut ClideApp| {
                    let o = app.scene.get_mut(id).ok_or("the object was deleted while sculpting")?;
                    if o.clay.as_ref() != Some(&before) {
                        return Err("the clay changed while sculpting; nothing was applied, send the strokes again".into());
                    }
                    let detail_cell = clay.detail.as_ref().map(|d| round(d.voxel() / radius_scale));
                    let bricks = clay.detail.as_ref().map(|d| d.grid().total_bricks());
                    o.clay = Some(clay);
                    Ok(text(json!({
                        "target": id,
                        "strokes": reports,
                        "detail_cell_size": detail_cell,
                        "detail_bricks": bricks,
                        "next": "Applied. The mesh rebuilds in the background; render_views and other calls wait for it.",
                    })))
                }) as super::jobs::Finish)
            }),
        ))
    }

    /// Maps tool coordinates into a clay object's space: (matrix, radius
    /// factor). Points are world space unless `space` is "local".
    fn clay_space(&self, id: u64, args: &Value) -> Result<(glam::Mat4, f32), String> {
        let to_local = match args["space"].as_str().unwrap_or("world") {
            "world" => self.scene.world_matrix(id).inverse(),
            "local" => glam::Mat4::IDENTITY,
            other => return Err(format!("unknown space {other}; use world or local")),
        };
        // Radii scale by the object's average scale.
        let scale = to_local.to_scale_rotation_translation().0;
        Ok((to_local, (scale.x.abs() + scale.y.abs() + scale.z.abs()) / 3.0))
    }

    /// `paint`: clear, then fill, then strokes.
    fn paint(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let mut clay = obj.clay.clone().unwrap_or_default();
        // A mask channel paints coverage only: it takes the color layer's
        // place while the steps run.
        let channel = args["channel"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
        if let Some(c) = &channel {
            if c.len() > 64 {
                return Err("channel names are at most 64 characters".into());
            }
            if !clay.masks.contains_key(c) && clay.masks.len() >= crate::smart::MAX_CHANNELS {
                return Err(format!("at most {} mask channels per clay (has {:?})", crate::smart::MAX_CHANNELS, clay.masks.keys().collect::<Vec<_>>()));
            }
            let mask = clay.masks.remove(c);
            clay.masks.insert(String::new(), clay.paint.take().unwrap_or_else(|| crate::paint::Paint::new(1.0)));
            clay.paint = mask;
        }
        if args["clear"].as_bool().unwrap_or(false) {
            clay.paint = None;
        }
        if let Some(c) = vec3(args, "fill")? {
            // Paint lives in the rest pose.
            self.meshes.sync(&self.scene);
            let mesh = self.meshes.rest(id).ok_or("clay has no mesh yet")?.0.clone();
            let w = args["fill_strength"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
            let color = c.clamp(Vec3::ZERO, Vec3::ONE);
            let samples: Vec<(Vec3, Vec3, f32)> = mesh.vertices.iter().map(|v| (Vec3::from(v.pos), color, w)).collect();
            crate::paint::splat(&mut clay, &samples)?;
        }
        let strokes = args["strokes"].as_array().map(Vec::as_slice).unwrap_or_default();
        if strokes.len() > 64 {
            return Err("at most 64 strokes per call".into());
        }
        let mut touched = Vec::new();
        self.meshes.sync(&self.scene);
        let snap = self.snapper(id).ok();
        for (n, raw) in strokes.iter().enumerate() {
            let mut raw = raw.clone();
            if raw.get("points").is_none()
                && let Some(p) = raw.get("point").cloned()
            {
                raw["points"] = json!([p]);
            }
            let snapped = raw["snap"].as_bool().or(args["snap"].as_bool()).unwrap_or(raw["points"].as_array().is_some_and(|p|p.len()==1));
            if channel.is_some() && raw.get("color").is_none_or(Value::is_null) && !raw["erase"].as_bool().unwrap_or(false) {
                raw["color"] = json!([1.0, 1.0, 1.0]);
            }
            let mut s: crate::paint::PaintStroke = serde_json::from_value(raw).map_err(|e| format!("stroke {n}: {e}"))?;
            for p in &mut s.points {
                *p = to_local.transform_point3(*p);
            }
            if snapped {
                let snap = snap.as_ref().ok_or("snap needs a clay surface")?;
                for p in &mut s.points {
                    *p = snap(*p).map_or(*p, |(q, _)| q);
                }
            }
            s.radius *= radius_scale;
            touched.push(crate::paint::stroke(&mut clay, &s).map_err(|e| format!("stroke {n}: {e}"))?);
        }
        drop(snap);
        if let Some(c) = channel {
            let mask = clay.paint.take();
            let color = clay.masks.remove("").filter(|p| p.grid().brick_count() > 0);
            clay.paint = color;
            if let Some(m) = mask {
                clay.masks.insert(c, m);
            }
        }
        self.scene.get_mut(id).expect("resolved").clay = Some(clay);
        let mut summary = self.clay_summary(id);
        summary["stroke_points_touched"] = json!(touched);
        Ok(text(summary))
    }

    /// Atomic combined wear and damage transaction for clay, kits and primitives.
    fn damage_tool(&mut self, args: &Value) -> ToolResult {
        let mut ids=Vec::new();
        for target in args["targets"].as_array().into_iter().flatten() {ids.push(self.resolve(target)?);}
        if let Some(target)=args.get("target") {ids.push(self.resolve(target)?);}
        ids.sort_unstable();ids.dedup();
        if ids.is_empty(){return Err("damage needs target or targets".into());}
        let local=match args["space"].as_str().unwrap_or("world") {"local"=>true,"world"=>false,_=>return Err("space must be local or world".into())};
        let amount=args["amount"].as_f64().unwrap_or(0.65) as f32;
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount){return Err("amount must be 0..1".into());}
        self.meshes.sync(&self.scene);
        let mut scene=self.scene.clone();let mut reports=Vec::new();
        for &id in &ids {
            let o=scene.get(id).ok_or("object vanished")?;
            if !matches!(o.shape,Shape::Clay|Shape::Kit|Shape::Cube|Shape::Sphere|Shape::Cylinder|Shape::Cone|Shape::Plane|Shape::Torus) {return Err("damage supports Clay, Kit and mesh primitives".into());}
            let mesh=self.meshes.get(o).ok_or("target has no mesh")?;
            let world=scene.world_matrix(id);
            let mut stamps=Vec::new();
            for raw in args["stamps"].as_array().into_iter().flatten() {
                let mut request:crate::damage::Stamp=serde_json::from_value(raw.clone()).map_err(|e|e.to_string())?;
                if ids.len() > 1 && !local && request.region.is_none() {
                    let owner = |p: Vec3| -> Option<u64> {
                        crate::damage::stamp_owner(p, ids.iter().filter_map(|&candidate| {
                            let object = scene.get(candidate)?;
                            Some((candidate, self.meshes.get(object)?.as_ref(), scene.world_matrix(candidate)))
                        }))
                    };
                    let path = matches!(request.kind, crate::smart::Shape::Gouge | crate::smart::Shape::Scrape);
                    if path {
                        if request.point.or_else(||request.points.first().copied()).and_then(owner) != Some(id) {continue;}
                    } else {
                        request.point = request.point.filter(|&p|owner(p)==Some(id));
                        request.points.retain(|&p|owner(p)==Some(id));
                        if request.point.is_none() && request.points.is_empty() {continue;}
                    }
                }
                stamps.extend(crate::damage::stamps(mesh,world,local,&request)?);
            }
            let o=scene.get_mut(id).unwrap();
            let list=if let Some(c)=&mut o.clay {&mut c.decals}else{&mut o.damage};
            if args["clear"].as_bool().unwrap_or(false){list.retain(|d|d.shape.code()<5);}
            if list.len()+stamps.len()>crate::smart::MAX_DECALS {return Err("at most 64 decals/damage segments per object".into());}
            let added=stamps.len();list.extend(stamps);
            if let Some(preset)=args["preset"].as_str(){crate::damage::apply_wear(&mut scene,id,preset,amount)?;}
            reports.push(json!({"target":id,"segments_added":added}));
        }
        self.scene=scene;
        Ok(text(json!({"objects":reports,"preset":args["preset"],"amount":amount,"material_scope":args["preset"].as_str().map(|_|"shared: wear layers replaced; all users receive base edits")})))
    }

    /// `decal`: clear or remove decals, then stamp new ones.
    fn decal(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let (to_local, size_scale) = self.clay_space(id, args)?;
        let mut clay = obj.clay.clone().unwrap_or_default();
        if args["clear"].as_bool().unwrap_or(false) {
            clay.decals.clear();
        }
        if let Some(remove) = args["remove"].as_array() {
            let mut idx: Vec<usize> = remove.iter().filter_map(Value::as_u64).map(|i| i as usize).collect();
            idx.sort_unstable();
            for i in idx.into_iter().rev() {
                if i >= clay.decals.len() {
                    return Err(format!("no decal {i} (has {})", clay.decals.len()));
                }
                clay.decals.remove(i);
            }
        }
        let list = args["decals"].as_array().map(Vec::as_slice).unwrap_or_default();
        if clay.decals.len() + list.len() > crate::smart::MAX_DECALS {
            return Err(format!("at most {} decals per clay", crate::smart::MAX_DECALS));
        }
        self.meshes.sync(&self.scene);
        let snap = self.snapper(id).ok();
        let mut added = Vec::new();
        for (n, raw) in list.iter().enumerate() {
            let mut raw = raw.clone();
            let point = raw.get("point").or(raw.get("center")).cloned().ok_or(format!("decal {n}: needs point [x, y, z]"))?;
            let p: [f32; 3] = serde_json::from_value(point).map_err(|_| format!("decal {n}: point must be [x, y, z]"))?;
            let mut center = to_local.transform_point3(Vec3::from(p));
            let normal = match raw.get("normal").filter(|v| !v.is_null()) {
                Some(v) => {
                    let d: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("decal {n}: normal must be [x, y, z]"))?;
                    Some(to_local.transform_vector3(Vec3::from(d)).normalize_or(Vec3::Z))
                }
                None => None,
            };
            // Without a normal, the decal faces out of the nearest surface.
            let snapped = snap.as_ref().and_then(|s| s(center));
            let normal = match (normal, snapped) {
                (Some(d), _) => d,
                (None, Some((q, nn))) => {
                    center = q;
                    nn
                }
                (None, None) => return Err(format!("decal {n}: no surface near the point; give a normal")),
            };
            if raw["snap"].as_bool().unwrap_or(false)
                && let Some((q, _)) = snapped
            {
                center = q;
            }
            raw["center"] = json!(center.to_array());
            raw["normal"] = json!(normal.to_array());
            if let Some(obj) = raw.as_object_mut() {
                obj.remove("point");
                obj.remove("snap");
            }
            let mut d: crate::smart::Decal = serde_json::from_value(raw).map_err(|e| format!("decal {n}: {e}"))?;
            d.size *= size_scale;
            d.sanitize();
            if d.shape == crate::smart::Shape::Image {
                let path = d.image.as_deref().ok_or(format!("decal {n}: shape image needs image (a PNG path)"))?;
                crate::smart::picture(path).ok_or(format!("decal {n}: cannot read image {path}"))?;
            }
            added.push(clay.decals.len());
            clay.decals.push(d);
        }
        drop(snap);
        self.scene.get_mut(id).expect("resolved").clay = Some(clay);
        let mut summary = self.clay_summary(id);
        summary["decals_added"] = json!(added);
        summary["decal_count"] = json!(self.scene.get(id).and_then(|o| o.clay.as_ref()).map_or(0, |c| c.decals.len()));
        Ok(text(summary))
    }

    /// Blob count and the built mesh's size and bounds, so agents can check an edit.
    /// Runs `f` with every rig in its rest pose, then puts the frame back.
    pub(super) fn at_rest<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let keep = self.meshes.frame;
        self.set_frame(None);
        let out = f(self);
        self.set_frame(keep);
        out
    }

    /// Sets up an object `create_object` just added; returns the mirror copy.
    fn fill_new_object(&mut self, id: u64, shape: Shape, args: &Value) -> Result<Option<u64>, String> {
        if let Some(p) = args.get("parent").filter(|p| !p.is_null()) {
            let parent = self.resolve(p)?;
            if self.scene.ancestors(parent).len() + 1 >= crate::scene::MAX_DEPTH {
                return Err(format!("hierarchy would be deeper than {} levels", crate::scene::MAX_DEPTH));
            }
            // A new object has no world placement yet: put it in the
            // parent's space directly rather than keeping world position.
            self.scene.get_mut(id).expect("just added").parent = Some(parent);
        }
        self.apply_props(id, args)?;
        if shape == Shape::Clay {
            let mut clay = Clay::default();
            if let Some(r) = args["resolution"].as_u64() {
                clay.resolution = r as u32;
            }
            clay.blobs = parse_blobs(&args["blobs"])?;
            clay.sanitize();
            self.scene.get_mut(id).expect("just added").clay = Some(clay);
        }
        Ok(if args["mirror"].as_bool().unwrap_or(false) { Some(self.mirror_copy(id)?) } else { None })
    }

    fn clay_summary(&mut self, id: u64) -> Value {
        self.meshes.sync(&self.scene);
        let Some(o) = self.scene.get(id) else { return Value::Null };
        let clay = o.clay.as_ref();
        let mesh = self.meshes.get(o);
        let bounds = self.meshes.bounds(o).map(|(lo, hi)| json!({ "min": round3(lo), "max": round3(hi) }));
        json!({
            "id": id,
            "name": o.name,
            "blobs": clay.map_or(0, |c| c.blobs.len()),
            "blob_materials": clay.map(|c|c.blobs.iter().enumerate().map(|(i,b)|json!({"index":i,"name":b.name,"material":if b.material.is_empty(){o.material.as_deref().unwrap_or("clay")}else{&b.material}})).collect::<Vec<_>>()),
            "blob_names": clay.map(|c| c.blobs.iter().enumerate().map(|(i, b)| if b.name.is_empty() { i.to_string() } else { b.name.clone() }).collect::<Vec<_>>()),
            "resolution": clay.map(|c| c.resolution),
            "cell_size": clay.and_then(|c| c.cell_size()).map(round),
            "detail_cell": clay.and_then(|c| c.detail_cell.or_else(|| c.detail.as_ref().map(|d| d.voxel()))).map(round),
            "detail_bricks": clay.and_then(|c| c.detail.as_ref()).map(|d| d.grid().total_bricks()),
            "detail_regions": clay.map(|c| c.detail_regions()).filter(|r| !r.is_empty()).map(|r| r.iter().map(|r| json!({ "name": r.name, "center": round3(r.center), "radii": round3(r.radii), "cell": r.cell })).collect::<Vec<_>>()),
            "paint_bricks": clay.and_then(|c| c.paint.as_ref()).map(|p| p.grid().brick_count()),
            "materials": mesh.map(|m| m.materials.iter().map(|x| x.name.clone()).collect::<Vec<_>>()),
            "unknown_materials": Some(super::look::unknown_blob_materials(&self.scene, id)).filter(|v| !v.is_empty()),
            "thin_blobs": clay.and_then(|c| Some(c.thin_blobs(c.cell_size()?))).filter(|v| !v.is_empty()).map(|v| json!({
                "blobs": v,
                "why": "thinner than about 2 grid cells, so they come out lumpy or broken. Raise resolution (a finer detail_cell does not refine blobs), or move small parts such as a face or hands into their own clay object.",
            })),
            "vertices": mesh.map_or(0, |m| m.vertices.len()),
            "triangles": mesh.map_or(0, |m| m.indices.len() / 3),
            "local_bounds": bounds,
            "fine_mesh_errors": self.fine_errors_json(),
        })
    }

    /// Fine meshes that failed (shown at the regular mesh instead), by
    /// object name; null when none did.
    pub(super) fn fine_errors_json(&self) -> Value {
        let errors: serde_json::Map<String, Value> = self.meshes.fine_errors().into_iter().filter_map(|(id, why)| Some((self.scene.get(id)?.name.clone(), json!(why)))).collect();
        if errors.is_empty() { Value::Null } else { Value::Object(errors) }
    }

    pub(super) fn resolve(&self, target: &Value) -> Result<u64, String> {
        let found = match target {
            Value::Number(n) => n.as_u64().and_then(|id| self.scene.get(id)),
            Value::String(name) => self.scene.objects.iter().find(|o| &o.name == name),
            _ => return Err("target must be an object id or name".into()),
        };
        found.map(|o| o.id).ok_or_else(|| format!("no object {target}"))
    }

    fn scene_json(&self, args: &Value) -> Value {
        let mut summary = crate::replies::scene(&self.scene, args);
        summary["selection"] = json!(self.selection);
        summary["camera"] = self.camera_json();
        summary["file"] = json!(self.file_path);
        summary["budget"] = self.budget_json();
        summary
    }


    fn camera_json(&self) -> Value {
        let c = &self.cams.persp;
        json!({ "look_at": round3(c.target), "yaw": round(c.yaw.to_degrees()), "pitch": round(c.pitch.to_degrees()), "distance": round(c.distance) })
    }
}

impl ClideApp {
    /// Starts an export job (.glb or binary .fbx): stale bakes run first,
    /// then the file is written. Only replaces files of the same kind (and
    /// PNGs next to an .fbx). `reply` gets the summary.
    pub(super) fn start_export(&mut self, fbx: bool, path: PathBuf, roots: Vec<u64>, animations: bool, lods: bool, args: &Value) -> Result<Started, String> {
        let ext = if fbx { "fbx" } else { "glb" };
        if !path.is_absolute() || path.extension().and_then(|e| e.to_str()) != Some(ext) || path.is_dir() {
            return Err(format!("path must be an absolute file path ending in .{ext}"));
        }
        let same_kind = |b: &[u8]| if fbx { crate::fbx::is_fbx(b) } else { crate::gltf::is_glb(b) };
        if path.exists() && !std::fs::read(&path).is_ok_and(|b| same_kind(&b)) {
            return Err(format!("{} exists and is not a .{ext} file; refusing to overwrite", path.display()));
        }
        self.meshes.sync(&self.scene);
        let mut bakes = Vec::new();
        for (id, s) in self.scene.objects.iter().filter_map(|o| o.bake.map(|s| (o.id, s))) {
            bakes.extend(self.meshes.bake_job(&self.scene, id, s)?);
        }
        let mut scene = self.scene.clone();
        scene.frame = None;
        let mut meshes = self.meshes.clone();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("export").to_string();
        let opts = crate::gltf::Options { roots, animations, fps: self.timeline.fps, file_stem: if fbx { stem } else { String::new() }, lods, atlas: args["atlas"].as_bool().unwrap_or(true), texel_density: args["texel_density"].as_f64().map(|v|v as f32) };
        let label = format!("Export {}", path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()));
        Ok(Started::Job(label, Box::new(move |progress| {
            // Each bake's share of the bar follows its work (texels and
            // triangles); writing the file takes the last bit.
            let cost: Vec<f32> = bakes.iter().map(|j| j.cost()).collect();
            let total = cost.iter().sum::<f32>().max(1e-6);
            let mut at = 0.0;
            for (job, c) in bakes.iter().zip(&cost) {
                progress.check()?;
                let share = 0.95 * c / total;
                progress.range(at, at + share);
                at += share;
                let (mesh, skin) = job.run(progress)?;
                meshes.install_bake(job, mesh, skin);
            }
            progress.check()?;
            progress.range(at, 1.0);
            progress.stage("writing the file", 0.0);
            let out = if fbx { crate::fbx::export(&scene, &meshes, &opts)? } else { crate::gltf::export(&scene, &meshes, &opts)? };
            // Baked textures go next to the .fbx; only PNGs are ever replaced.
            let dir = path.parent().ok_or("path has no folder")?;
            for (name, _) in &out.files {
                let p = dir.join(name);
                if p.exists() && !std::fs::read(&p).is_ok_and(|b| b.starts_with(b"\x89PNG")) {
                    return Err(format!("{} exists and is not a PNG; refusing to overwrite", p.display()));
                }
            }
            for (name, bytes) in &out.files {
                std::fs::write(dir.join(name), bytes).map_err(|e| format!("{name}: {e}"))?;
            }
            std::fs::write(&path, &out.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut summary = out.summary;
            summary["path"] = json!(path);
            summary["bytes"] = json!(out.bytes.len());
            summary["fps"] = json!(opts.fps);
            if !out.files.is_empty() {
                summary["texture_files"] = json!(out.files.iter().map(|f| &f.0).collect::<Vec<_>>());
            }
            Ok(Box::new(move |app: &mut ClideApp| {
                // Keep the bakes, so the next export does not redo them.
                app.meshes.take_bakes(&meshes);
                app.status = format!("Exported {}", path.display());
                Ok(text(summary))
            }) as super::jobs::Finish)
        })))
    }
}

/// Paths an agent may name: absolute, `.json`, and not a directory.
fn agent_path(p: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(p);
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return Err("path must end in .json".into());
    }
    if path.is_dir() {
        return Err("path is a directory".into());
    }
    Ok(path)
}

pub(super) fn load_scene(path: &std::path::Path) -> Result<Scene, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut scene: Scene = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    scene.sanitize();
    Ok(scene)
}

pub(crate) fn object_json_in(scene: &Scene, id: u64) -> Value {
    let Some(o) = scene.get(id) else { return Value::Null };
    let mut out = json!({
        "id": o.id,
        "name": o.name,
        "shape": o.shape,
        "parent": o.parent,
        "bone": o.bone,
        "pivot_world": round3(scene.pivot_world(id)),
        "translate": round3(o.translation),
        "rotate": round3(o.euler_degrees()),
        "scale": round3(o.scale),
        "color": round3(Vec3::from(o.color)),
        "visible": o.visible,
        "blobs": o.clay.as_ref().map(|c| c.blobs.len()),
        "material": o.material,
        "eye": o.eye,
        "hair": o.hair.as_ref().map(|h| h.coverage()),
        "cloth": o.cloth.as_ref().map(|c| json!({"settings":c.settings,"vertices":c.positions.len(),"cached_frames":c.cache.len()})),
        "piece": o.kit.as_ref().map(|k| k.piece),
        "terrain_size": o.terrain.as_ref().map(|t| t.size),
        "copies": o.scatter.as_ref().map(|s| s.instances.len()),
        "light": o.light,
        "fire": o.fire.as_ref().map(|f| json!({"settings":f.settings,"cached_frames":f.cache.len()})),
    });
    if let Some(s) = o.link.as_ref().and_then(|l| scene.get(l.source)) {
        out["mirror_of"] = json!(s.name);
    }
    if let Some(s) = o.follow.as_ref().and_then(|f| scene.get(f.object)) {
        out["follows"] = json!(s.name);
    }
    out
}

fn parse_blobs(v: &Value) -> Result<Vec<Blob>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => {
            if items.len() > MAX_BLOBS {
                return Err(format!("at most {MAX_BLOBS} blobs per clay object"));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let blob: Blob = serde_json::from_value(b.clone()).map_err(|e| format!("blob {i}: {e}"))?;
                    blob.validate().map_err(|e| format!("blob {i}: {e}"))?;
                    Ok(blob)
                })
                .collect()
        }
        _ => Err("blobs must be a list".into()),
    }
}

/// f32 -> JSON prints noise like 0.30000001192; agents read cleaner numbers.
fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

fn round3(v: Vec3) -> [f64; 3] {
    [round(v.x), round(v.y), round(v.z)]
}

pub(crate) fn vec3(args: &Value, key: &str) -> Result<Option<Vec3>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("{key} must be [x, y, z]"))?;
            Ok(Some(Vec3::from(a)))
        }
    }
}

pub(super) fn text(v: impl Into<TextBody>) -> Vec<Value> {
    vec![json!({ "type": "text", "text": v.into().0 })]
}

pub(super) struct TextBody(String);
impl From<Value> for TextBody {
    fn from(v: Value) -> Self {
        TextBody(serde_json::to_string_pretty(&v).unwrap_or_default())
    }
}
impl From<&str> for TextBody {
    fn from(s: &str) -> Self {
        TextBody(s.to_string())
    }
}
impl From<String> for TextBody {
    fn from(s: String) -> Self {
        TextBody(s)
    }
}

/// Box-filter downscale by a whole factor so the width fits `max_width`.
pub(super) fn downscale(img: &egui::ColorImage, max_width: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let k = w.div_ceil(max_width).max(1);
    if k == 1 {
        return img.clone();
    }
    let (nw, nh) = (w / k, h / k);
    let mut pixels = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0u32; 4];
            for dy in 0..k {
                for dx in 0..k {
                    let p = img.pixels[(y * k + dy) * w + x * k + dx].to_array();
                    for c in 0..4 {
                        sum[c] += p[c] as u32;
                    }
                }
            }
            let n = (k * k) as u32;
            pixels.push(egui::Color32::from_rgba_premultiplied(
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                (sum[3] / n) as u8,
            ));
        }
    }
    egui::ColorImage::new([nw, nh], pixels)
}

pub(super) fn encode_png(img: &egui::ColorImage) -> ToolResult {
    let bytes = encode_png_bytes(img)?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(vec![json!({ "type": "image", "data": data, "mimeType": "image/png" })])
}

/// An image as PNG file bytes (opaque).
pub(super) fn encode_png_bytes(img: &egui::ColorImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, img.size[0] as u32, img.size[1] as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    // Force opaque: the window is opaque but some backends leave alpha at 0.
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b(), 255]).collect();
    writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// `detail_regions` for `edit_clay`: a list of {name, center, radii (or
/// radius), cell, mirror}; `mirror` adds the copy across X = 0 (named
/// with ".R" when the name ends in ".L", else "_mirror").
pub(super) fn parse_regions(v: &Value) -> Result<Vec<crate::clay::Region>, String> {
    let list = v.as_array().ok_or("detail_regions must be a list of {name, center, radii, cell} (or [] to remove them)")?;
    let mut out = Vec::new();
    for r in list {
        let mut r = r.clone();
        let mirror = r.as_object_mut().and_then(|o| o.remove("mirror")).and_then(|m| m.as_bool()).unwrap_or(false);
        let region: crate::clay::Region = serde_json::from_value(r).map_err(|e| format!("detail region: {e}"))?;
        if mirror {
            let name = match region.name.strip_suffix(".L") { Some(n) => format!("{n}.R"), None => format!("{}_mirror", region.name) };
            out.push(crate::clay::Region { name, center: glam::Vec3::new(-region.center.x, region.center.y, region.center.z), ..region.clone() });
        }
        out.push(region);
    }
    Ok(out)
}

fn shading_call(name: &str, args: &Value) -> bool {
    match name {
        "set_material" => true,
        "update_object" => args.as_object().is_some_and(|a| a.keys().all(|k| matches!(k.as_str(),"target"|"material"|"color"|"detail"|"verbose"))),
        _ => false,
    }
}
