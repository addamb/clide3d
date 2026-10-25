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
use crate::ledger::Source;
use crate::scene::{Scene, Shape};

/// Calls that read no meshes: they may run while meshes build, if no
/// earlier call is still waiting (so answers stay in order).
const READ_ONLY: [&str; 2] = ["get_scene", "brush_preset"];
/// `READ_ONLY` calls, but for the actions that edit the scene.
fn read_only(name: &str, args: &Value) -> bool {
    READ_ONLY.contains(&name) && !(name == "brush_preset" && matches!(args["action"].as_str(), Some("save" | "delete")))
}

/// Calls that may always run at once, even past waiting calls.
const READ_ONLY_ANYTIME: [&str; 3] = ["screenshot", "job_status", "check_export"];

/// Tools that run as background jobs (see `jobs.rs`).
const JOB_TOOLS: [&str; 17] = ["render_film", "check_export", "bake", "export_glb", "export_fbx", "sculpt", "cloth", "fire", "face_detail", "render_animation", "stroke_edit", "sculpt_diff", "mask", "group", "import_mesh", "to_clay", "export_print"];
/// Job tools that edit the scene: later edits wait in line behind them.
const EDIT_JOBS: [&str; 9] = ["sculpt", "cloth", "fire", "face_detail", "stroke_edit", "mask", "group", "import_mesh", "to_clay"];

/// Whether a call runs as a background job (`JOB_TOOLS`, and kitbash's
/// InsertMesh, which may weld and decimate a large mesh).
fn job_call(name: &str, args: &Value) -> bool {
    JOB_TOOLS.contains(&name) || (name == "kitbash" && crate::kitbash::tool::is_insert(args))
}

/// Whether a job call edits the scene (`EDIT_JOBS`, and InsertMesh).
fn edit_job_call(name: &str, args: &Value) -> bool {
    EDIT_JOBS.contains(&name) || (name == "kitbash" && crate::kitbash::tool::is_insert(args))
}

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
            "sculpt" => {
                // Posable strokes that name no pose frame take the one the
                // editor shows (and keep it in their records).
                let posed = self.meshes.frame;
                self.at_rest(|app| app.sculpt(args, posed))
            }
            "face_detail" => self.at_rest(|app| app.face_detail(args)),
            "stroke_edit" => {
                // A posable replacement that names no pose frame takes the
                // editor's, as sculpt does.
                let posed = self.meshes.frame;
                self.at_rest(|app| app.stroke_edit(args, posed))
            }
            "sculpt_diff" => self.at_rest(|app| app.sculpt_diff(args)),
            "mask" => self.at_rest(|app| app.mask_tool(args)),
            "group" => self.at_rest(|app| app.group_tool(args)),
            "import_mesh" => self.import_mesh(args),
            // Sources are read where they stand at rest.
            "to_clay" => self.at_rest(|app| app.to_clay(args)),
            "export_print" => self.at_rest(|app| app.export_print(args)),
            // InsertMesh (`job_call`): placed at rest, as other kitbash calls.
            "kitbash" => self.at_rest(|app| app.insert_mesh(args)),
            "export_glb" | "export_fbx" => {
                let path = PathBuf::from(args["path"].as_str().ok_or("path is required")?);
                let mut roots = Vec::new();
                for t in args["targets"].as_array().into_iter().flatten() {
                    roots.push(self.resolve(t)?);
                }
                let animations = args["animations"].as_bool().unwrap_or(true);
                let lods = args["lods"].as_bool().unwrap_or(false);
                if !matches!(args["instancing"].as_str(), None | Some("nodes" | "ext")) || (name == "export_fbx" && !args["instancing"].is_null()) {
                    return Err("instancing (export_glb only) must be nodes or ext".into());
                }
                self.start_export(name == "export_fbx", path, roots, animations, lods, args)
            }
            _ => Err(format!("unknown job tool {name}")),
        }
    }

    pub(super) fn process_mcp(&mut self, ctx: &egui::Context) {
        // While meshes build in the background, calls wait in line (the
        // window stays live) and run in order once the meshes are ready;
        // only calls that read no meshes go ahead of the line. UI strokes
        // join the same line (`send_stroke`), with or without a server.
        let incoming: Vec<_> = self.mcp_rx.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for call in incoming {
            let ticket = self.jobs.ticket(call.name.clone(), call.reply.clone());
            self.jobs.queue.push_back((call, ticket));
        }
        if self.jobs.queue.is_empty() {
            return;
        }
        let editing = self.jobs.running.iter().any(|j| j.edits);
        let waiting = self.meshes.busy() || !self.jobs.deferred.is_empty() || self.jobs.running.iter().any(|j| j.edits);
        let mut calls = Vec::new();
        let mut keep = std::collections::VecDeque::new();
        for (call, ticket) in self.jobs.queue.drain(..) {
            if !waiting || (!editing && keep.is_empty() && shading_call(&call.name, &call.args)) || (keep.is_empty() && read_only(&call.name, &call.args)) || READ_ONLY_ANYTIME.contains(&call.name.as_str()) {
                calls.push((call, ticket));
            } else {
                keep.push_back((call, ticket));
            }
        }
        self.jobs.queue = keep;
        for (call, ticket) in calls {
            // Each call that edits is one undo step of its own.
            if !matches!(call.name.as_str(), "screenshot" | "job_status") {
                self.flush_ui_step();
            }
            let jumps = self.history_jumps;
            let source = call.source;
            let label = super::history::call_label(&call.name, &call.args, &self.scene, if source == Source::Ui { "ui" } else { "agent" });
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
            if job_call(&call.name, &call.args) {
                self.scene.frame = self.meshes.frame;
                self.call_source = source;
                let started = self.start_job_tool(&call.name, &call.args);
                self.call_source = Source::Agent;
                match started {
                    Ok(Started::Job(job_label, work)) => {
                        self.spawn_job(job_label, ticket, work);
                        if edit_job_call(&call.name, &call.args)
                            && let Some(job) = self.jobs.running.last_mut()
                        {
                            job.edits = true;
                            job.step = Some((call.name.clone(), label.clone(), source));
                        }
                    }
                    Ok(Started::Now(result)) => self.jobs.answer(ticket, result),
                    Err(e) => self.jobs.answer(ticket, Err(e)),
                }
                self.agent_step(&call.name, label, jumps, source);
                continue;
            }
            self.call_source = source;
            let result = self.run_tool(&call.name, &call.args);
            self.call_source = Source::Agent;
            self.agent_step(&call.name, label, jumps, source);
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

    pub(super) fn run_tool(&mut self, name: &str, args: &Value) -> ToolResult {
        // A loaded or undone scene has no frame yet.
        self.scene.frame = self.meshes.frame;
        match name {
            "get_scene" => Ok(text(self.scene_json(args))),
            "brush_preset" => self.brush_preset(args),
            "alpha" => self.at_rest(|app| app.alpha_tool(args)),
            "create_object" => {
                let shape: Shape = serde_json::from_value(args["shape"].clone())
                    .map_err(|_| format!("unknown shape {}", args["shape"]))?;
                if shape == Shape::Fire { return Err("use the fire tool to simulate a Fire object".into()); }
                if shape == Shape::Cloth { return Err("use the cloth tool with parent and pattern to create a garment".into()); }
                if shape == Shape::Mesh { return Err("use import_mesh {path, as: mesh} to make a Mesh object".into()); }
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
            // Both can add or remove seam blobs on any clay: ledgers settle first.
            "kitbash" => self.at_rest(|app| app.settle_all_clays().and_then(|()| crate::kitbash::tool::kitbash(&mut app.scene, &mut app.meshes, args)).map(text)),
            "detail_pass" => self.at_rest(|app| app.settle_all_clays().and_then(|()| crate::kitbash::tool::detail_pass(&mut app.scene, &mut app.meshes, args)).map(text)),
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
                // Every sculpt and paint layer bottom first (base, paint, then
                // the clay's own), then the mask and groups.
                let mut layers: Vec<Value> = crate::layers::names(clay).iter().filter_map(|n| crate::layers::layer_json(clay, n).ok()).collect();
                layers.push(crate::ledger::layer_json::<crate::mask::SculptMask>(clay));
                layers.push(crate::ledger::layer_json::<crate::groups::Groups>(clay));
                let stack = clay.sculpt.as_ref().map(|s| s.data());
                let morphs: Vec<Value> = stack.map(|s| s.morphs.iter().map(|m| json!({ "name": m.name, "layers": m.layers.iter().map(|l| &l.name).collect::<Vec<_>>(), "blobs_changed": m.context != crate::ledger::clay_context(clay) })).collect()).unwrap_or_default();
                // The alphas the clay's strokes use: the name each is bound to.
                let alphas_used: Vec<Value> = clay.sculpt.as_ref().map(|s| &s.data().alphas).into_iter().flat_map(|a| a.images.keys().map(move |h| json!({ "hash": h, "names": a.names.iter().filter(|(_, b)| *b == h).map(|(n, _)| n).collect::<Vec<_>>() }))).collect();
                Ok(text(json!({ "target": id, "alphas_used": alphas_used, "blob_count":total,"offset":offset,"next_offset":offset.checked_add(limit).filter(|n|*n<total), "resolution": clay.resolution, "detail_cell": clay.detail_cell, "detail_regions": clay.detail_regions(), "blobs": blobs, "mask_channels": clay.masks.keys().collect::<Vec<_>>(), "decals": clay.decals, "layers": layers, "record_layer": stack.and_then(|s| s.record_layer.clone()), "levels": crate::levels::json(clay), "morphs": morphs, "grid_mb": (crate::layers::grid_bytes(clay) as f64 / 1048576.0 * 10.0).round() / 10.0, "mask": self.mask_json(id), "groups": self.groups_json(id)["groups"].clone(), "replay_cache": { "checkpoints": self.replay_cache.checkpoints() }, "session_cache": self.meshes.session_cache(id).map(|c| json!({ "bricks": c.bricks(), "mb": (c.bytes() as f64 / 1048576.0 * 10.0).round() / 10.0, "warm": c.filled() > 0, "chunked": self.meshes.chunked(id), "last_remesh_ms": (self.meshes.chunk_timing().0 * 10.0).round() / 10.0 })), "mesh_assets": clay.assets.values().map(|a| json!({ "hash": a.hash(), "triangles": a.tris().len(), "points": a.points().len() })).collect::<Vec<_>>() })))
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
            "instance_array" => {
                let id = self.resolve(&args["target"])?;
                let o = self.scene.get(id).ok_or("object vanished")?;
                let blob = o.clay.as_ref().ok_or_else(|| format!("{} is not a clay object", o.name))?.find(&args["blob"]).ok_or_else(|| format!("no blob {}", args["blob"]))?;
                let leaf = match args["level"].as_str() {
                    None | Some("outer") => false,
                    Some("leaf") => true,
                    Some(other) => return Err(format!("level must be outer or leaf, not {other}")),
                };
                self.settle_clays([id])?;
                let o = self.scene.get(id).ok_or("object vanished")?;
                self.meshes.sync(&self.scene);
                let before = self.meshes.get(o).map_or(0, |m| m.indices.len() / 3);
                let made = crate::envkit::scatter::instance_array(&mut self.scene, id, blob, leaf, args["force"].as_bool().unwrap_or(false))?;
                self.defer_reply(move |app| {
                    app.meshes.sync(&app.scene);
                    let tris = |id: u64| app.scene.get(id).and_then(|o| app.meshes.get(o)).map_or(0, |m| m.indices.len() / 3);
                    let (rest, unit) = (tris(id), tris(made.unit));
                    Ok(text(json!({
                        "target": id, "unit": made.unit, "scatter": made.scatter, "copies": made.copies,
                        "triangles_before": before,
                        "triangles_after": { "drawn": rest + unit * made.copies, "unique": rest + unit },
                        "resolution": app.scene.get(id).and_then(|o| o.clay.as_ref()).map(|c| c.resolution),
                        "note": "copies are separate closed shapes now: they no longer merge with each other or their neighbours, and wear, curvature and occlusion are measured on one copy. The object's resolution was set to keep its mesh cell size",
                    })))
                })
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
                // Checked before anything changes.
                let shading = args["shading"].as_str().map(|s| match s {
                    "wireframe" => Ok((Shading::Wireframe, false)),
                    "shaded" => Ok((Shading::Shaded, false)),
                    "wireframe_on_shaded" => Ok((Shading::WireOnShaded, false)),
                    "matcap" => Ok((Shading::Shaded, true)),
                    other => Err(format!("unknown shading {other}; use wireframe, shaded, wireframe_on_shaded or matcap")),
                }).transpose()?;
                let matcap = match args.get("matcap").filter(|v| !v.is_null()) {
                    None => None,
                    Some(Value::String(n)) => Some(match crate::render::matcap::Matcap::builtin(n) {
                        Some(m) => m,
                        None => crate::render::matcap::Matcap::load(std::path::Path::new(n)).map_err(|e| format!("{e} (built in: {})", crate::render::matcap::BUILTIN.join(", ")))?,
                    }),
                    Some(_) => return Err(format!("matcap must be a built-in name ({}) or an image path", crate::render::matcap::BUILTIN.join(", "))),
                };
                if let Some(m) = &matcap {
                    self.matcap_choice = m.clone();
                }
                match shading {
                    Some((s, on)) => {
                        self.shading = s;
                        self.matcap = on.then(|| self.matcap_choice.clone());
                    }
                    // A matcap named alone turns matcap shading on.
                    None if matcap.is_some() => self.matcap = Some(self.matcap_choice.clone()),
                    None => {}
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
                if let Some(b) = args["brush_cursor"].as_bool() {
                    self.show_brush_cursor = b;
                }
                // Checked before anything changes, like the rest below.
                let overlay = args["overlay"].as_str().map(crate::overlay::Overlay::parse).transpose()?;
                if let Some(o) = overlay {
                    self.meshes.overlay = o;
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
                if !args["shadows"].is_null() {
                    self.shadows = parse_shadows(&args["shadows"], self.shadows)?;
                }
                if !args["post"].is_null() {
                    self.set_post(&args["post"])?;
                }
                // Parse both before changing either, so an error changes nothing.
                let lighting = args.get("lighting").filter(|v| !v.is_null()).map(|v| self.parse_lighting(v).map(|l| (v.clone(), l))).transpose()?;
                let mut ao = args.get("ambient_occlusion").filter(|v| !v.is_null()).map(|v| parse_ao(v, self.ao)).transpose()?;
                // `ao` and `ao_radius`: the same settings, shorter.
                if let Some(v) = args.get("ao").filter(|v| !v.is_null()) {
                    ao = Some(parse_ao(v, ao.unwrap_or(self.ao)).map_err(|e| e.replace("ambient_occlusion", "ao"))?);
                }
                if let Some(v) = args.get("ao_radius").filter(|v| !v.is_null()) {
                    let r = v.as_f64().map(|x| x as f32).filter(|x| (0.01..=10.0).contains(x)).ok_or("ao_radius must be a number from 0.01 to 10")?;
                    ao = Some(crate::render::Ao { radius: r, ..ao.unwrap_or(self.ao) });
                }
                if let Some((v, l)) = lighting {
                    self.view_lighting_args = v;
                    self.view_lighting = l;
                }
                if let Some(ao) = ao {
                    self.ao = ao;
                }
                Ok(text(json!({
                    "shading": if self.matcap.is_some() { "Matcap".to_string() } else { format!("{:?}", self.shading) },
                    "matcap": self.matcap.as_ref().map(|m| m.name.clone()),
                    "overlay": self.meshes.overlay.name(),
                    "grid": self.show_grid,
                    "bones": self.show_bones,
                    "brush_cursor": self.show_brush_cursor,
                    "layout": if self.four_view { "four" } else { "single" },
                    "view": self.single_view.name(),
                    "shadows": shadows_json(&self.shadows, crate::render::shadow_status(&self.render_state)),
                    "post": post_json(&self.view_post, &self.view_look),
                    "lighting": lighting_json(&self.view_lighting_args, &self.view_lighting),
                    "ambient_occlusion": ao_json(&self.ao, crate::render::ao_status(&self.render_state)),
                })))
            }
            "undo" => {
                let had = self.history.can_undo();
                self.undo();
                Ok(text(if had { "Undone." } else { "Nothing to undo." }))
            }
            "redo" => {
                let had = self.history.can_redo();
                self.redo();
                Ok(text(if had { "Redone." } else { "Nothing to redo." }))
            }
            "history" => self.history_tool(args),
            "sculpt_layer" => self.at_rest(|app| app.sculpt_layer(args)),
            "detail_level" => self.at_rest(|app| app.detail_level(args)),
            "morph" => self.at_rest(|app| app.morph_tool(args)),
            "stroke_log" => self.stroke_log(args),
            "probe_clay" => self.at_rest(|app| app.probe_clay(args)),
            "new_scene" => {
                self.replace_scene(Scene::default());
                self.selection.clear();
                Ok(text("Scene cleared. Use undo to bring it back."))
            }
            "open_scene" => {
                let path = agent_path(args["path"].as_str().ok_or("path is required")?)?;
                // Generic error: never echo another file's contents back to the agent.
                let scene = load_scene(&path).map_err(|_| format!("{} is not a readable Clide3D scene", path.display()))?;
                self.replace_scene(scene);
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
                // Parse only: no ledger replays (`load_scene`) for a check.
                if path.exists() && read_scene(&path).is_err() {
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

    /// Settles the unchecked stroke ledgers of `ids` (replays them against the
    /// blobs they were made on) before a tool changes those blobs.
    pub(super) fn settle_clays(&mut self, ids: impl IntoIterator<Item = u64>) -> Result<(), String> {
        for id in ids {
            if let Some(clay) = self.scene.get_mut(id).and_then(|o| o.clay.as_mut()) {
                crate::ledger::settle_clay(clay, Some(&self.replay_cache))?;
            }
        }
        Ok(())
    }

    /// `settle_clays` on every object (a no-op for ledgers already checked).
    pub(super) fn settle_all_clays(&mut self) -> Result<(), String> {
        let ids: Vec<u64> = self.scene.objects.iter().map(|o| o.id).collect();
        self.settle_clays(ids)
    }

    /// `edit_clay`: replace, then remove, then update, then add blobs.
    fn edit_clay(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let mut clay = obj.clay.clone().unwrap_or_default();
        // Unchecked ledgers replay against the blobs they were made on.
        crate::ledger::settle_clay(&mut clay, Some(&self.replay_cache))?;
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
        // Mesh blobs bring their assets along (from this session's).
        clay.adopt_assets()?;
        clay.sanitize();
        // A new detail cell, regions or a cleared grid: the ledgers record
        // it, so replays still end at the grid the clay shows. The relaid
        // layer and morph grids (shared ones once) and those records may
        // not fit the clay's budget: checked with them, before the scene
        // takes the clay.
        crate::layers::sync_within_budget(&mut clay)?;
        self.scene.get_mut(id).expect("resolved").clay = Some(clay);
        self.defer_reply(move |app| Ok(text(app.clay_summary(id))))
    }

    /// `sculpt`: strokes are prepared here (world to clay space, snapping,
    /// in the rest pose) and committed to the base layer's ledger on a job
    /// thread, so big strokes never block the window. The result installs
    /// if the base layer's ledger, its grid and the blobs did not change
    /// meanwhile (calls that edit wait in line while it runs); other
    /// changes to the clay (paint, materials) are kept. `dry_run` measures
    /// without installing.
    fn sculpt(&mut self, args: &Value, posed: Option<f32>) -> Result<Started, String> {
        use crate::clay::Detail;
        use crate::ledger::{self, Meta};
        use crate::paint::Paint;
        let t0 = std::time::Instant::now();
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object", obj.name));
        }
        let name = obj.name.clone();
        // The layer it writes: as named, else the clay's recording layer
        // (`sculpt_layer record`), else base; never the editor's selection.
        let layer = match &args["layer"] {
            Value::Null => obj.clay.as_ref().map_or_else(|| crate::layers::BASE.to_string(), crate::layers::default_sculpt_layer),
            Value::String(l) => l.clone(),
            _ => return Err("layer must be a sculpt layer's name (sculpt_layer list)".into()),
        };
        if let Some(clay) = &obj.clay {
            crate::layers::stroke_amount(clay, &layer)?;
        } else if layer != crate::layers::BASE {
            return Err(format!("no layer {layer}: the clay has only base and paint"));
        }
        // Sculptris Pro: `cell` (per stroke, or for every stroke here) grows
        // a finer detail region over each stroke first.
        let call_cell = match args.get("cell").filter(|c| !c.is_null()) {
            None => None,
            Some(c) => Some(c.as_f64().map(|c| c as f32).filter(|c| c.is_finite() && *c >= crate::clay::MIN_DETAIL_CELL).ok_or_else(|| format!("cell must be a number of at least {} (finer than the clay's detail cell)", crate::clay::MIN_DETAIL_CELL))?),
        };
        let (dry_run, measure) = (args["dry_run"].as_bool().unwrap_or(false), args["measure"].as_bool().unwrap_or(false));
        let label = args["label"].as_str().map(|l| l.chars().take(120).collect::<String>());
        let (to_local, radius_scale) = self.clay_space(id, args)?;
        let raws = args["strokes"].as_array().ok_or("strokes must be a list")?;
        if raws.is_empty() || raws.len() > 64 {
            return Err("pass 1 to 64 strokes".into());
        }
        let before = obj.clay.clone().unwrap_or_default();
        // Presets go under each stroke (the stroke's own fields win); the
        // records keep the merged strokes.
        let preset = match args.get("preset") {
            None | Some(Value::Null) => None,
            Some(Value::String(n)) => Some(n.as_str()),
            Some(_) => return Err("preset must be a preset's name (brush_preset list)".into()),
        };
        let mut raws = raws.iter().enumerate().map(|(n, r)| crate::brush::preset::merge(&self.scene.brushes, preset, r).map_err(|e| format!("stroke {n}: {e}"))).collect::<Result<Vec<_>, _>>()?;
        // The call's cell goes on strokes without their own (records keep it).
        if let Some(c) = call_cell {
            for r in &mut raws {
                if r.get("cell").is_none_or(Value::is_null) && let Some(o) = r.as_object_mut() {
                    o.insert("cell".into(), json!(c));
                }
            }
        }
        // Detail levels: each stroke's, else the call's, else the clay's
        // active one (records keep the level and its cell).
        let call_level = crate::levels::call_level(args)?;
        for (n, r) in raws.iter_mut().enumerate() {
            crate::levels::fill(&before, r, call_level).map_err(|e| format!("stroke {n}: {e}"))?;
        }
        self.meshes.sync(&self.scene);
        let unpose = self.posable_unposer(id, &mut raws, posed, to_local)?;
        let strokes = self.resolve_strokes_posed(id, args, &raws, None, to_local, radius_scale, unpose.as_ref().map(|f| f as crate::brush::resolve::Unpose))?;
        let alphas = self.alphas_for(id, &strokes)?;
        let resolve_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let job_label = format!("Sculpt {name} ({} stroke{})", strokes.len(), if strokes.len() == 1 { "" } else { "s" });
        // A dry run's records are never installed: it plants no checkpoints.
        let cache = (!dry_run).then(|| self.replay_cache.clone());
        let source = self.call_source;
        let meta = Meta::by(source, "sculpt", label).pressed(args)?;
        Ok(Started::Job(
            job_label,
            Box::new(move |progress| {
                let t1 = std::time::Instant::now();
                let mut clay = before.clone();
                ledger::settle_clay(&mut clay, cache.as_deref())?;
                if let Some(store) = alphas {
                    ledger::set_alphas(&mut clay, store);
                }
                let mut reports = Vec::new();
                let mut records = Vec::new();
                let mut last = None;
                let mut painted = false;
                // Whether a stroke's cell grew the detail regions (a relayout
                // of every grid the clay has).
                let mut relaid = false;
                for (n, (spec, s)) in strokes.iter().enumerate() {
                    progress.check()?;
                    progress.set(n as f32 / strokes.len() as f32);
                    let fail = |e: String| format!("stroke {n}: {e} (no stroke was applied)");
                    if let Some(cell) = spec.cell {
                        let (lo, hi) = s.dabs.iter().flat_map(|d| d.points.iter().enumerate().map(move |(k, p)| (*p, d.radii.as_ref().and_then(|r| r.get(k).copied()).unwrap_or(d.radius)))).fold((glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)), |(a, b), (p, r)| (a.min(p - r), b.max(p + r)));
                        let now = clay.detail_regions();
                        let next = crate::clay::sculptris::after_stroke(&now, lo, hi, cell);
                        if next != now {
                            // Recorded as the regions (a replayable relayout,
                            // not the whole grid) on base and the sculpt
                            // layers; then within the budgets.
                            for (r, seq) in crate::layers::set_regions_recorded(&mut clay, &next, &meta, cache.as_deref()).map_err(fail)? {
                                records.push(ledger::record_json(&r, seq, false, false));
                            }
                            clay.fine_check().map_err(fail)?;
                            crate::layers::sync_within_budget(&mut clay).map_err(fail)?;
                            relaid = true;
                        }
                    }
                    // The mask and groups it reads, kept by hash.
                    let mut s = s.clone();
                    ledger::freeze(&mut clay, &mut s).map_err(fail)?;
                    let s = &s;
                    let c = {
                        let v = crate::layers::view_for::<Detail>(&clay, &layer).map_err(fail)?;
                        ledger::commit::<Detail>(&v, &ledger::ledger(&v), &meta, spec, s, measure, cache.as_deref()).map_err(fail)?
                    };
                    if let Some(r) = &c.synced {
                        records.push(ledger::record_json(r, c.ledger.position(r.id).map_or(0, |i| i + 1), false, false));
                    }
                    let seq = c.ledger.len();
                    let mut report = json!({ "stroke": n, "id": c.record.id, "seq": seq, "changed_points": c.record.stats.changed_points, "bricks": c.record.stats.bricks_touched, "bbox": c.record.stats.bbox, "max_disp": c.record.stats.max_disp, "mean_disp": c.record.stats.mean_disp, "radius_used": round(c.radius / radius_scale), "dabs": s.dabs.len() });
                    // The chunks a sculpt session remeshes for it (null: all).
                    if let Some(g) = c.grid.as_ref() {
                        report["dirty_chunks"] = json!(crate::clay::chunks::dirty_detail(c.before.as_ref(), Some(g), g.voxel()).map(|d| d.len()));
                    }
                    if let Some(v) = c.record.stats.volume_delta {
                        report["volume_delta"] = json!(v);
                    }
                    if s.instances > 1 {
                        report["copies"] = json!(s.instances);
                    }
                    if let Some(f) = spec.symmetry.as_ref().filter(|y| y.posable).and_then(|y| y.frame) {
                        report["pose_frame"] = json!(f);
                    }
                    last = Some((c.record.id, c.before.clone(), c.grid.clone()));
                    crate::layers::install::<Detail>(&mut clay, &layer, c.ledger, c.grid);
                    if s.paint.is_some() {
                        let p = ledger::commit::<Paint>(&clay, &ledger::ledger(&clay), &meta, spec, s, false, cache.as_deref()).map_err(fail)?;
                        report["painted_points"] = json!(p.record.stats.changed_points);
                        report["paint_record"] = json!(p.record.id);
                        ledger::install(&mut clay, p.ledger, p.grid);
                        painted = true;
                    }
                    reports.push(report);
                }
                let apply_ms = t1.elapsed().as_secs_f64() * 1000.0;
                // Layers and morphs share one byte budget.
                crate::layers::check_budget(&clay)?;
                let view = crate::layers::view_for::<Detail>(&clay, &layer)?.into_owned();
                let base_head = view.sculpt.as_ref().map(|s| s.data().base.head());
                Ok(Box::new(move |app: &mut ClideApp| {
                    let detail_cell = view.detail.as_ref().or(clay.detail.as_ref()).map(|d| round(d.voxel() / radius_scale));
                    let bricks = view.detail.as_ref().map(|d| d.grid().total_bricks());
                    let ctx = <Detail as ledger::Grid>::context(&view);
                    let stale = ledger::ledger::<Detail>(&view).records().iter().filter(|r| r.stale(ctx, &view)).count();
                    let mut out = json!({
                        "target": id,
                        "layer": layer,
                        "strokes": reports,
                        "record_ids": reports.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(),
                        "records": records,
                        "ledger_head": base_head,
                        "stale_context": stale,
                        "detail_cell_size": detail_cell,
                        "detail_bricks": bricks,
                        "timing_ms": { "resolve": round(resolve_ms as f32), "apply": round(apply_ms as f32) },
                    });
                    if dry_run {
                        out["dry_run"] = json!(true);
                        out["step"] = Value::Null;
                        out["next"] = json!("Measured only: nothing was applied. Send it again without dry_run to apply it.");
                        return Ok(text(out));
                    }
                    let o = app.scene.get_mut(id).ok_or("the object was deleted while sculpting")?;
                    let cur = o.clay.as_mut().ok_or("the object is no longer clay")?;
                    let head = |c: &Clay| c.sculpt.as_ref().map(|s| (s.data().base.head(), s.data().paint.head()));
                    // The layer written, and what its strokes read (the blobs and
                    // the other layers), must be as they were.
                    let state = |c: &Clay| (c.sculpt.as_ref().and_then(|_| crate::layers::ledger_of::<Detail>(c, &layer).ok()).map(|l| l.head()), crate::layers::grid_of::<Detail>(c, &layer).ok().flatten(), crate::layers::context_of::<Detail>(c, &layer).ok());
                    if state(cur) != state(&before) || ledger::clay_context(cur) != ledger::clay_context(&before) || (painted && (head(cur).map(|h| h.1) != head(&before).map(|h| h.1) || cur.paint != before.paint)) {
                        return Err(format!("the clay's sculpt layer {layer}, the layers it reads or its blobs changed while sculpting; nothing was applied, send the strokes again"));
                    }
                    if relaid {
                        // New detail regions moved every grid: the clay must
                        // be as the strokes found it, and takes all of them.
                        if *cur != before {
                            return Err("the clay changed while its detail regions grew (Sculptris Pro); nothing was applied, send the strokes again".into());
                        }
                        *cur = clay.clone();
                        out["detail_regions"] = json!(cur.detail_regions().len());
                    }
                    crate::layers::install::<Detail>(cur, &layer, ledger::ledger::<Detail>(&view), view.detail.clone());
                    ledger::set_alphas(cur, clay.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default());
                    if painted {
                        ledger::install(cur, ledger::ledger::<Paint>(&clay), clay.paint.clone());
                    }
                    ledger::merge_frozen(cur, &clay);
                    ledger::adopt_settled(cur, &before, &clay);
                    if let (Some((record, before, after)), Some(h)) = (last, base_head) {
                        app.last_deltas.insert(id, ledger::LastDelta { head: h, record, source, before, after, layer: layer.clone() });
                    }
                    out["step"] = json!(app.history.current() + 1);
                    out["next"] = json!("Applied. The mesh rebuilds in the background; render_views and other calls wait for it. stroke_log lists the records; stroke_edit scales, disables or replays them; undo removes the whole call.");
                    Ok(text(out))
                }) as super::jobs::Finish)
            }),
        ))
    }

    /// Maps tool coordinates into a clay object's space: (matrix, radius
    /// factor). Points are world space unless `space` is "local".
    pub(super) fn clay_space(&self, id: u64, args: &Value) -> Result<(glam::Mat4, f32), String> {
        let to_local = match args["space"].as_str().unwrap_or("world") {
            "world" => self.scene.world_matrix(id).inverse(),
            "local" => glam::Mat4::IDENTITY,
            other => return Err(format!("unknown space {other}; use world or local")),
        };
        // Radii scale by the object's average scale.
        let scale = to_local.to_scale_rotation_translation().0;
        Ok((to_local, (scale.x.abs() + scale.y.abs() + scale.z.abs()) / 3.0))
    }

    /// Reads and resolves a call's strokes for clay `id` (see
    /// `sculpt::prepare`): frames, snapping, screen rays, spacing, pressure
    /// and symmetry all resolve here, so the job only applies dabs.
    pub(super) fn resolve_strokes(&self, id: u64, args: &Value, raws: &[Value], default_brush: Option<crate::sculpt::Brush>, to_local: glam::Mat4, radius_scale: f32) -> Result<Vec<(crate::brush::spec::StrokeSpec, crate::brush::resolve::Resolved)>, String> {
        self.resolve_strokes_posed(id, args, raws, default_brush, to_local, radius_scale, None)
    }

    /// Posable symmetry for strokes about to resolve: each posable stroke
    /// names the pose it is on; one left out takes `posed` (the editor's
    /// frame), written into the stroke so its record (and the same spec
    /// sent again) never reads the timeline. All posable strokes share one
    /// frame. Returns the way back to the rest pose (`unposer`), or `None`
    /// when no stroke is posable. The meshes must be synced.
    pub(super) fn posable_unposer(&self, id: u64, raws: &mut [Value], posed: Option<f32>, to_local: glam::Mat4) -> Result<Option<super::alphas::Unposer>, String> {
        use crate::brush::symmetry::PoseFrame;
        let mut pose: Option<PoseFrame> = None;
        for (n, r) in raws.iter_mut().enumerate() {
            if !r["symmetry"]["posable"].as_bool().unwrap_or(false) {
                continue;
            }
            let sym = r["symmetry"].as_object_mut().ok_or(format!("stroke {n}: symmetry must be an object"))?;
            let given = match sym.get("frame") {
                None | Some(Value::Null) => PoseFrame::of(posed),
                Some(f) => serde_json::from_value(f.clone()).map_err(|_| format!("stroke {n}: symmetry frame must be a frame number or \"rest\""))?,
            };
            sym.insert("frame".into(), json!(given));
            if pose.is_some_and(|p| p != given) {
                return Err("posable strokes in one sculpt call share one pose frame; send strokes on other frames in their own calls".into());
            }
            pose = Some(given);
        }
        pose.map(|p| self.unposer(id, p.frame(), to_local)).transpose()
    }

    /// `resolve_strokes`, with posable symmetry's way back to the rest pose.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_strokes_posed(&self, id: u64, args: &Value, raws: &[Value], default_brush: Option<crate::sculpt::Brush>, to_local: glam::Mat4, radius_scale: f32, unpose: Option<crate::brush::resolve::Unpose>) -> Result<Vec<(crate::brush::spec::StrokeSpec, crate::brush::resolve::Resolved)>, String> {
        let snap = self.snapper(id).ok();
        let cast = self.clay_caster(id).ok();
        let lookup = |a: &crate::brush::alpha::AlphaRef| self.find_alpha(id, a).map(|x| x.hash());
        let levels = self.scene.get(id).and_then(|o| o.clay.as_ref()).map(crate::levels::cells).unwrap_or_default();
        let ctx = crate::brush::resolve::ResolveCtx {
            to_local,
            radius_scale,
            world_to_local: self.scene.world_matrix(id).inverse(),
            snap: snap.as_ref().map(|f| f as crate::brush::resolve::Snap),
            cast: cast.as_ref().map(|f| f as crate::brush::resolve::Cast),
            view_far: self.scene_sphere().1 * 4.0 + 10.0,
            snap_call: args["snap"].as_bool(),
            snap_single: default_brush.is_some_and(|b| b == crate::sculpt::Brush::Paint || b.marks()),
            alpha: Some(&lookup),
            unpose,
            levels: Some(&levels),
        };
        crate::sculpt::prepare(raws, default_brush, &ctx)
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
        // Unchecked ledgers settle first: paint builds on the exact
        // sculpted surface, and the swaps below must not reach a replay.
        crate::ledger::settle_clay(&mut clay, Some(&self.replay_cache))?;
        // A paint layer (`layers.rs`): its grid takes the paint layer's
        // place while the steps run (a view of it), and goes back after.
        let layer = args["layer"].as_str().unwrap_or(crate::layers::PAINT).to_string();
        let mut owner = None;
        if layer != crate::layers::PAINT {
            if args["channel"].as_str().is_some() {
                return Err("channel paints a smart-material mask channel; leave layer out".into());
            }
            if crate::layers::kind_of(&clay, &layer)? != crate::layers::Kind::Paint {
                return Err(format!("{layer} is a sculpt layer; the paint tool paints paint layers (sculpt with paint: {{color}} colors the paint layer as it sculpts)"));
            }
            let view = crate::layers::view(&clay, &layer)?;
            owner = Some(std::mem::replace(&mut clay, view));
        }
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
        let mut raws = strokes.to_vec();
        if channel.is_some() {
            for raw in &mut raws {
                if raw.get("color").is_none_or(Value::is_null) && !raw["erase"].as_bool().unwrap_or(false) {
                    raw["color"] = json!([1.0, 1.0, 1.0]);
                }
            }
        }
        // Color strokes are records on the paint layer's ledger; mask
        // channel strokes paint the channel directly.
        let meta = crate::ledger::Meta::by(self.call_source, "paint", args["label"].as_str().map(str::to_string)).pressed(args)?;
        let mut ids = Vec::new();
        let strokes = self.resolve_strokes(id, args, &raws, Some(crate::sculpt::Brush::Paint), to_local, radius_scale)?;
        // The alphas paint strokes use, kept on the clay as sculpt's are.
        if let Some(store) = self.alphas_for(id, &strokes)? {
            crate::ledger::set_alphas(&mut clay, store);
        }
        for (n, (spec, s)) in strokes.iter().enumerate() {
            if spec.brush != crate::sculpt::Brush::Paint {
                return Err(format!("stroke {n}: paint strokes take no brush (sculpt with paint: {{color}} sculpts and colors at once)"));
            }
            if channel.is_some() {
                touched.push(crate::sculpt::run(&mut clay, s).map_err(|e| format!("stroke {n}: {e}"))?.painted.unwrap_or(0));
                continue;
            }
            let mut s = s.clone();
            crate::ledger::freeze(&mut clay, &mut s).map_err(|e| format!("stroke {n}: {e}"))?;
            let c = crate::ledger::commit::<crate::paint::Paint>(&clay, &crate::ledger::ledger(&clay), &meta, spec, &s, false, Some(&self.replay_cache)).map_err(|e| format!("stroke {n}: {e}"))?;
            touched.push(c.record.stats.changed_points as usize);
            ids.push(c.record.id);
            crate::ledger::install(&mut clay, c.ledger, c.grid);
        }
        if let Some(c) = channel {
            let mask = clay.paint.take();
            let color = clay.masks.remove("").filter(|p| p.grid().brick_count() > 0);
            clay.paint = color;
            if let Some(m) = mask {
                clay.masks.insert(c, m);
            }
        }
        // A clear or fill without strokes is an outside edit of the layer
        // (a layer's view has no layers of its own: the whole clay is
        // checked below).
        crate::layers::sync_within_budget(&mut clay)?;
        if let Some(mut whole) = owner {
            // The view's paint and ledger go back into the layer.
            crate::layers::install::<crate::paint::Paint>(&mut whole, &layer, crate::ledger::ledger(&clay), clay.paint.clone());
            if let Some(s) = clay.sculpt.as_ref() {
                crate::ledger::set_alphas(&mut whole, s.data().alphas.clone());
            }
            crate::ledger::merge_frozen(&mut whole, &clay);
            crate::layers::check_budget(&whole)?;
            clay = whole;
        }
        self.scene.get_mut(id).expect("resolved").clay = Some(clay);
        let mut summary = self.clay_summary(id);
        summary["stroke_points_touched"] = json!(touched);
        if !ids.is_empty() {
            summary["layer"] = json!(layer);
            summary["record_ids"] = json!(ids);
            summary["step"] = json!(self.history.current() + 1);
        }
        Ok(text(summary))
    }

    /// Atomic combined wear and damage transaction for clay, kits and primitives.
    fn damage_tool(&mut self, args: &Value) -> ToolResult {
        let mut ids=Vec::new();
        for target in args["targets"].as_array().into_iter().flatten() {ids.push(self.resolve(target)?);}
        if let Some(target)=args.get("target") {ids.push(self.resolve(target)?);}
        ids.sort_unstable();ids.dedup();
        // Damage decals shape the surface paint replays on.
        self.settle_clays(ids.clone())?;
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
        // Damage and deep decals shape the surface paint replays on.
        self.settle_clays([id])?;
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
            clay.adopt_assets()?;
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
        let mut out = json!({
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
        });
        // The mask and polygroups, when the clay has them.
        if let Some(m) = clay.and_then(|c| c.sculpt_mask.as_ref()) {
            out["mask_bricks"] = json!(m.grid().brick_count());
        }
        if let Some(g) = clay.and_then(|c| c.groups.as_ref()) {
            out["groups"] = json!(g.data().info.len());
            out["hidden_groups"] = json!(g.hidden());
        }
        // Sculpt and paint layers above base and paint.
        if let Some(c) = clay.filter(|c| !crate::layers::extra(c).is_empty()) {
            out["layers"] = json!(crate::layers::extra(c).iter().map(|l| json!({ "name": l.name, "kind": l.kind, "amount": l.amount, "enabled": l.enabled })).collect::<Vec<_>>());
        }
        out
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
        summary["ui_activity"] = self.ui_activity_json(crate::replies::verbose(args));
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
        // Blend shapes: each named layer or morph on every exported clay
        // that has it, as the clay (composite) to fit the mesh to.
        let names: Vec<String> = match args.get("blend_shapes") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) if a.len() <= 32 => a.iter().map(|v| v.as_str().map(str::to_string).ok_or("blend_shapes are names: layer:NAME or morph:NAME")).collect::<Result<_, _>>()?,
            Some(_) => return Err("blend_shapes is a list of at most 32 names (layer:NAME or morph:NAME)".into()),
        };
        let max_residual = match args.get("max_residual") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_f64().filter(|r| r.is_finite() && *r > 0.0).ok_or("max_residual must be a positive number")? as f32),
        };
        let mut shapes: Vec<(u64, String, String, Result<Clay, String>)> = Vec::new();
        if !names.is_empty() {
            let mut within: std::collections::HashSet<u64> = std::collections::HashSet::new();
            let mut stack: Vec<u64> = if roots.is_empty() { self.scene.objects.iter().map(|o| o.id).collect() } else { roots.clone() };
            while let Some(id) = stack.pop() {
                if within.insert(id) {
                    stack.extend(self.scene.children(Some(id)));
                }
            }
            for name in &names {
                let mut found = false;
                for o in self.scene.objects.iter().filter(|o| within.contains(&o.id) && o.shape == Shape::Clay && self.scene.is_visible(o.id)) {
                    let Some(clay) = &o.clay else { continue };
                    match crate::layers::shape_target(clay, name) {
                        Ok(target) => {
                            found = true;
                            shapes.push((o.id, o.name.clone(), name.clone(), target));
                        }
                        Err(_) if within.len() > 1 => {}
                        Err(e) => return Err(e),
                    }
                }
                if !found {
                    return Err(format!("blend shape {name}: no exported clay has it (sculpt_layer list, morph list)"));
                }
            }
        }
        let mut scene = self.scene.clone();
        scene.frame = None;
        let mut meshes = self.meshes.clone();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("export").to_string();
        let mut opts = crate::gltf::Options { roots, animations, fps: self.timeline.fps, file_stem: if fbx { stem } else { String::new() }, lods, atlas: args["atlas"].as_bool().unwrap_or(true), texel_density: args["texel_density"].as_f64().map(|v|v as f32), gpu_instancing: args["instancing"].as_str() == Some("ext"), blend_shapes: Vec::new() };
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
            // Blend shapes, fitted to the meshes being written.
            let mut fitted = Vec::new();
            for (id, object, name, target) in &shapes {
                progress.stage("fitting blend shapes", 0.0);
                progress.check()?;
                let report = |extra: Value| {
                    let mut v = json!({ "object": object, "name": name });
                    for (k, x) in extra.as_object().into_iter().flatten() {
                        v[k] = x.clone();
                    }
                    v
                };
                let target = match target {
                    Ok(t) => t,
                    Err(why) => {
                        fitted.push(report(json!({ "refused": why })));
                        continue;
                    }
                };
                let o = scene.get(*id).ok_or("an exported object vanished")?;
                let mesh = match o.bake.is_some().then(|| meshes.baked(*id)).flatten() {
                    Some((m, _)) => m.clone(),
                    None => match meshes.rest(*id) {
                        Some((m, _)) => m.clone(),
                        None => {
                            fitted.push(report(json!({ "refused": "the object has no mesh to export" })));
                            continue;
                        }
                    },
                };
                let positions: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| v.pos).collect();
                let f = crate::layers::project_shape(target, &positions, &mesh.indices);
                let limit = max_residual.unwrap_or(0.5 * f.cell);
                let residual = f.residual();
                let mut v = report(json!({ "residual": residual, "vertex_residual": f.vertex_residual, "coverage_residual": f.coverage_residual, "max_residual": limit, "vertices": positions.len(), "max_disp": f.deltas.iter().map(|d| glam::Vec3::from(*d).length()).fold(0.0f32, f32::max) }));
                if residual > limit {
                    v["refused"] = json!(format!("the mesh's vertices cannot follow this shape: the target surface is {residual:.5} from them (max_residual {limit:.5}); big pulls stretch the surface past the vertices. Sculpt it smaller, raise the clay's resolution, or export it as its own mesh"));
                } else {
                    opts.blend_shapes.push(crate::gltf::BlendShape { object: *id, name: name.clone(), deltas: std::sync::Arc::new(f.deltas) });
                }
                fitted.push(v);
            }
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
            // Only what the file holds is reported as written: a shape the
            // writer left out is refused, and the count leaves it out.
            for (v, (id, _, name, _)) in fitted.iter_mut().zip(&shapes) {
                if v.get("refused").is_none() && !out.shapes_written.iter().any(|(o, n)| o == id && n == name) {
                    v["refused"] = json!("the writer could not attach it to the object's exported mesh (the mesh written has other vertices than the one it was fitted to); nothing of it is in the file");
                }
            }
            if !fitted.is_empty() {
                summary["blend_shapes_written"] = json!(out.shapes_written.len());
                summary["blend_shapes"] = json!(fitted);
            }
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

/// A scene file, parsed and sanitized (no ledger replays).
pub(super) fn read_scene(path: &std::path::Path) -> Result<Scene, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // One inflate budget for the whole file (see `clay::INFLATE_BUDGET`).
    let mut scene: Scene = crate::clay::inflate_budget(crate::clay::INFLATE_BUDGET, || serde_json::from_str(&s)).map_err(|e| format!("{}: {e}", path.display()))?;
    scene.sanitize();
    Ok(scene)
}

/// Opens a scene file: `read_scene`, then the ledgers replay to check
/// their saved grids (see `ledger::after_load`), within one time budget.
pub(super) fn load_scene(path: &std::path::Path) -> Result<Scene, String> {
    let mut scene = read_scene(path)?;
    let deadline = std::time::Instant::now() + crate::ledger::VERIFY_BUDGET;
    for o in &mut scene.objects {
        if let Some(c) = &mut o.clay {
            crate::ledger::after_load_by(c, deadline);
        }
    }
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

/// `set_display shadows`: true/false, or an object changing some settings.
pub(super) fn parse_shadows(v: &Value, mut s: crate::render::Shadows) -> Result<crate::render::Shadows, String> {
    if let Some(on) = v.as_bool() {
        s.enabled = on;
        return Ok(s);
    }
    let o = v.as_object().ok_or("shadows must be true, false or an object")?;
    let flag = |name: &str| o.get(name).map(|v| v.as_bool().ok_or(format!("shadows.{name} must be true or false"))).transpose();
    let number = |name: &str, lo: f32, hi: f32| {
        o.get(name)
            .map(|v| v.as_f64().map(|x| x as f32).filter(|x| (lo..=hi).contains(x)).ok_or(format!("shadows.{name} must be a number from {lo} to {hi}")))
            .transpose()
    };
    if let Some(name) = o.keys().find(|k| !["enabled", "soft", "quality", "strength", "softness", "ground", "key_direction"].contains(&k.as_str())) {
        return Err(format!("unknown shadows setting {name}"));
    }
    s.enabled = flag("enabled")?.unwrap_or(s.enabled);
    s.soft = flag("soft")?.unwrap_or(s.soft);
    s.ground = flag("ground")?.unwrap_or(s.ground);
    s.strength = number("strength", 0.0, 1.0)?.unwrap_or(s.strength);
    s.softness = number("softness", 0.005, 0.3)?.unwrap_or(s.softness);
    if let Some(q) = o.get("quality") {
        s.quality = match q.as_str() {
            Some("normal") => crate::render::ShadowQuality::Normal,
            Some("high") => crate::render::ShadowQuality::High,
            _ => return Err("shadows.quality must be \"normal\" or \"high\"".into()),
        };
    }
    if let Some(d) = o.get("key_direction") {
        let d: [f32; 3] = serde_json::from_value(d.clone()).map_err(|_| "shadows.key_direction must be [x, y, z]")?;
        let d = glam::Vec3::from(d).try_normalize().ok_or("shadows.key_direction must not be zero")?;
        // The key lights from above; a light under the floor casts nothing useful.
        if d.y < 0.1 {
            return Err("shadows.key_direction must point up (y > 0.1 once normalised)".into());
        }
        s.key_dir = d;
    }
    Ok(s)
}

/// `set_display ambient_occlusion` (and render_views'): true/false, or an
/// object changing some settings.
pub(super) fn parse_ao(v: &Value, mut ao: crate::render::Ao) -> Result<crate::render::Ao, String> {
    if let Some(on) = v.as_bool() {
        ao.enabled = on;
        return Ok(ao);
    }
    let o = v.as_object().ok_or("ambient_occlusion must be true, false or an object")?;
    if let Some(name) = o.keys().find(|k| !["enabled", "radius", "strength"].contains(&k.as_str())) {
        return Err(format!("unknown ambient_occlusion setting {name}"));
    }
    let number = |name: &str, lo: f32, hi: f32| {
        o.get(name)
            .map(|v| v.as_f64().map(|x| x as f32).filter(|x| (lo..=hi).contains(x)).ok_or(format!("ambient_occlusion.{name} must be a number from {lo} to {hi}")))
            .transpose()
    };
    if let Some(v) = o.get("enabled") {
        ao.enabled = v.as_bool().ok_or("ambient_occlusion.enabled must be true or false")?;
    }
    ao.radius = number("radius", 0.01, 10.0)?.unwrap_or(ao.radius);
    ao.strength = number("strength", 0.0, 2.0)?.unwrap_or(ao.strength);
    Ok(ao)
}

/// The lighting settings for replies.
pub(super) fn lighting_json(args: &Value, l: &crate::view_light::ViewLighting) -> Value {
    json!({
        "lighting": args,
        "label": l.label,
        "environment": l.probe.is_some(),
        "suns": l.suns.len(),
        "local_lights": l.locals.len(),
    })
}

/// The ambient occlusion settings for replies, with the window's last frame.
pub(super) fn ao_json(ao: &crate::render::Ao, status: Option<crate::render::AoStatus>) -> Value {
    json!({
        "enabled": ao.enabled,
        "radius": ao.radius,
        "strength": ao.strength,
        "last_frame": status.map(|st| json!({ "active": st.active, "reason": st.reason })),
        "triangle_budget": crate::render::EXTRA_PASS_TRIANGLE_BUDGET,
    })
}

impl ClideApp {
    /// `lighting` for set_display and render_views: "film" is the last
    /// render_film's lighting, anything else `ViewLighting::parse`.
    pub(super) fn parse_lighting(&self, v: &Value) -> Result<std::sync::Arc<crate::view_light::ViewLighting>, String> {
        if v == "film" {
            let args = self.last_film_args.as_ref().ok_or("lighting \"film\" mirrors the last render_film, and there has been none yet")?;
            let l = crate::view_light::ViewLighting::from_args(args)?;
            return Ok(std::sync::Arc::new(crate::view_light::ViewLighting { label: format!("film ({})", l.label), ..l }));
        }
        crate::view_light::ViewLighting::parse(v)
    }

    /// Applies a `set_display.post` change (see `parse_post`); on an error
    /// nothing changes.
    pub(super) fn set_post(&mut self, patch: &Value) -> Result<(), String> {
        let (args, look) = parse_post(patch, &self.view_post)?;
        self.view_post = args;
        self.view_look = look;
        Ok(())
    }
}

const POST_KEYS: [&str; 9] = ["enabled", "look", "tonemap", "exposure", "bloom", "vignette", "white_balance", "grade", "reset"];

/// `set_display.post`: true/false, or an object whose settings replace
/// those in `current` (`reset` starts from the defaults). Returns the new
/// settings and the finishing they give. Everything but `enabled`,
/// `exposure` and tonemap `none` means what it means to render_film, which
/// parses it: `exposure` is in stops, EV 12 - exposure on film's camera.
pub(super) fn parse_post(patch: &Value, current: &Value) -> Result<(Value, crate::render::post::ViewLook), String> {
    let patch = match patch {
        Value::Bool(on) => json!({ "enabled": on }),
        Value::Object(_) => patch.clone(),
        _ => return Err("post must be true, false or an object".into()),
    };
    let o = patch.as_object().unwrap();
    if let Some(name) = o.keys().find(|k| !POST_KEYS.contains(&k.as_str())) {
        return Err(format!("unknown post setting {name}"));
    }
    let reset = match o.get("reset") {
        None => false,
        Some(v) => v.as_bool().ok_or("post.reset must be true or false")?,
    };
    let mut args = if reset { json!({}) } else { current.clone() };
    for (k, v) in o.iter().filter(|(k, _)| *k != "reset") {
        args[k] = v.clone();
    }
    let enabled = match &args["enabled"] {
        Value::Null => true,
        v => v.as_bool().ok_or("post.enabled must be true or false")?,
    };
    let ev = match &args["exposure"] {
        Value::Null => 0.0,
        v => v.as_f64().filter(|x| (-10.0..=10.0).contains(x)).ok_or("post.exposure must be a number of stops from -10 to 10")?,
    };
    let none = args["tonemap"] == "none";
    let mut film = json!({ "camera": { "ev": 12.0 - ev } });
    for k in ["look", "tonemap", "bloom", "vignette", "white_balance", "grade"].into_iter().filter(|&k| !args[k].is_null() && (k != "tonemap" || !none)) {
        film[k] = args[k].clone();
    }
    // The viewport blooms by default; a look preset brings its own.
    if args["bloom"].is_null() && args["look"].as_str().is_none_or(|l| l == "neutral") {
        film["bloom"] = json!(true);
    }
    let mut look = crate::film::Look::parse(&film).map_err(|e| format!("post: {e}"))?.view_look();
    look.enabled = enabled;
    if none {
        look.tone = crate::render::post::Tone::None;
    }
    Ok((args, look))
}

/// The HDR settings for replies.
pub(super) fn post_json(args: &Value, look: &crate::render::post::ViewLook) -> Value {
    json!({
        "enabled": look.enabled,
        "look": args["look"].as_str().unwrap_or("neutral"),
        "tonemap": look.tone.name(),
        "exposure": look.exposure.log2(),
        "bloom": { "strength": look.bloom, "threshold": look.bloom_threshold },
        "vignette": look.vignette,
        "white_balance": look.balance.to_array(),
    })
}

/// The shadow settings for replies, with the window's last frame.
pub(super) fn shadows_json(s: &crate::render::Shadows, status: Option<crate::render::ShadowStatus>) -> Value {
    json!({
        "enabled": s.enabled,
        "soft": s.soft,
        "quality": s.quality.name(),
        "strength": s.strength,
        "softness": s.softness,
        "ground": s.ground,
        "key_direction": s.key_dir.to_array(),
        "last_frame": status.map(|st| json!({ "active": st.active, "triangles": st.triangles, "reason": st.reason })),
        "triangle_budget": crate::render::SHADOW_TRIANGLE_BUDGET,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ShadowQuality, Shadows};

    #[test]
    fn set_display_shadows_parse_and_echo() {
        let d = Shadows::default();
        assert!(!parse_shadows(&json!(false), d).unwrap().enabled);
        assert!(parse_shadows(&json!(true), Shadows { enabled: false, ..d }).unwrap().enabled);
        let s = parse_shadows(&json!({ "soft": true, "quality": "high", "strength": 0.5, "softness": 0.05, "ground": false, "key_direction": [0, 2, 0] }), d).unwrap();
        assert_eq!(s, Shadows { enabled: true, soft: true, quality: ShadowQuality::High, strength: 0.5, softness: 0.05, ground: false, key_dir: glam::Vec3::Y });
        // Settings not given stay as they were.
        assert_eq!(parse_shadows(&json!({ "strength": 0.25 }), s).unwrap(), Shadows { strength: 0.25, ..s });
        for bad in [json!({ "key_direction": [0, 0, 0] }), json!({ "key_direction": [1, -1, 0] }), json!({ "key_direction": [1, 2] }), json!({ "quality": "ultra" }), json!({ "strength": 2 }), json!({ "softness": 0 }), json!({ "soft": "yes" }), json!({ "shiny": true }), json!(3)] {
            assert!(parse_shadows(&bad, d).is_err(), "{bad} should be refused");
        }
        let echo = shadows_json(&s, Some(crate::render::ShadowStatus { active: false, triangles: 9, reason: Some("why") }));
        assert_eq!(echo["quality"], "high");
        assert_eq!(echo["key_direction"], json!([0.0, 1.0, 0.0]));
        assert_eq!(echo["last_frame"]["reason"], "why");
        // The echo parses back to the same settings.
        let back = json!({ "enabled": echo["enabled"], "soft": echo["soft"], "quality": echo["quality"], "strength": echo["strength"], "softness": echo["softness"], "ground": echo["ground"], "key_direction": echo["key_direction"] });
        assert_eq!(parse_shadows(&back, d).unwrap(), s);
    }

    #[test]
    fn set_display_lighting_and_ao_parse_and_echo() {
        use crate::render::Ao;
        let d = Ao::default();
        assert!(d.enabled && d.radius == 0.5 && d.strength == 1.0);
        assert!(!parse_ao(&json!(false), d).unwrap().enabled);
        let ao = parse_ao(&json!({ "radius": 1.5, "strength": 0.5 }), d).unwrap();
        assert_eq!(ao, Ao { enabled: true, radius: 1.5, strength: 0.5 });
        // Settings not given stay as they were.
        assert_eq!(parse_ao(&json!({ "enabled": false }), ao).unwrap(), Ao { enabled: false, ..ao });
        for bad in [json!({ "radius": 0 }), json!({ "radius": 11 }), json!({ "strength": 3 }), json!({ "enabled": "yes" }), json!({ "spread": 1 }), json!("on")] {
            assert!(parse_ao(&bad, d).is_err(), "{bad} should be refused");
        }
        let echo = ao_json(&ao, Some(crate::render::AoStatus { active: false, reason: Some("why") }));
        assert_eq!(echo["radius"], 1.5);
        assert_eq!(echo["last_frame"]["reason"], "why");
        let back = json!({ "enabled": echo["enabled"], "radius": echo["radius"], "strength": echo["strength"] });
        assert_eq!(parse_ao(&back, d).unwrap(), ao);

        // Lighting echoes what was asked and what it gave.
        let args = json!({ "environment": "sunset", "lights": "none" });
        let l = crate::view_light::ViewLighting::parse(&args).unwrap();
        let echo = lighting_json(&args, &l);
        assert_eq!(echo["lighting"], args);
        assert_eq!((echo["label"].as_str(), echo["environment"].as_bool(), echo["suns"].as_u64()), (Some("sunset"), Some(true), Some(1)));
        let studio = lighting_json(&json!("studio"), &crate::view_light::ViewLighting::studio_default());
        assert_eq!(studio["suns"], 3);
        assert_eq!(lighting_json(&json!("classic"), &crate::view_light::ViewLighting::classic())["environment"], false);
    }

    #[test]
    fn set_display_post_parse_and_echo() {
        use crate::render::post::{Tone, ViewLook};
        // No settings is the default look, which is render_film's bloom: true.
        let (empty, d) = parse_post(&json!({}), &json!({})).unwrap();
        assert_eq!(d, ViewLook::default());
        assert!(!parse_post(&json!(false), &empty).unwrap().1.enabled);
        // Settings merge into the current ones; reset starts over.
        let (args, look) = parse_post(&json!({ "tonemap": "agx", "exposure": 1, "bloom": { "strength": 0.2, "threshold": 2 } }), &empty).unwrap();
        assert_eq!((look.tone, look.exposure, look.bloom, look.bloom_threshold), (Tone::Agx, 2.0, 0.2, 2.0));
        let (args, look) = parse_post(&json!({ "tonemap": "none", "vignette": 0.3 }), &args).unwrap();
        assert_eq!((look.tone, look.exposure, look.vignette), (Tone::None, 2.0, 0.3));
        assert_eq!(parse_post(&json!({ "reset": true }), &args).unwrap().1, ViewLook::default());
        // A look preset brings its tonemap, bloom and grade.
        let trailer = parse_post(&json!({ "look": "trailer" }), &empty).unwrap().1;
        assert_eq!((trailer.tone, trailer.bloom_threshold), (Tone::Agx, 1.5));
        assert!(trailer.grade != ViewLook::default().grade && trailer.vignette > 0.0);
        assert_eq!(parse_post(&json!({ "bloom": false }), &empty).unwrap().1.bloom, 0.0);
        for bad in [json!({ "tonemap": "reinhard" }), json!({ "exposure": 11 }), json!({ "exposure": "bright" }), json!({ "look": "noir" }), json!({ "glow": 1 }), json!({ "enabled": 1 }), json!({ "white_balance": 0 }), json!(3)] {
            assert!(parse_post(&bad, &args).is_err(), "{bad} should be refused");
        }
        let echo = post_json(&args, &look);
        assert_eq!(echo["tonemap"], "none");
        assert_eq!(echo["exposure"], 1.0);
        assert_eq!(echo["bloom"]["threshold"], 2.0);
    }
}
