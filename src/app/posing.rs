//! Agent tools for mirroring, aligning and posing (see `pose.rs`):
//! `mirror_object`, `pose_bone`, and `align` / `frame` / `grip` on
//! `update_object`.

use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::tools::{object_json_in, text, vec3};
use crate::mcp::ToolResult;
use crate::pose;

/// The mirror plane of a call (world): `plane` "x" | "y" | "z" (with
/// `at`, default 0) or {normal, point}; `plane_x` is a shorthand.
fn plane(args: &Value) -> Result<(Vec3, f32), String> {
    if let Some(x) = args["plane_x"].as_f64() {
        return Ok((Vec3::X, x as f32));
    }
    let at = args["at"].as_f64().unwrap_or(0.0) as f32;
    match &args["plane"] {
        Value::Null => Ok((Vec3::X, at)),
        Value::String(a) => match a.as_str() {
            "x" | "yz" => Ok((Vec3::X, at)),
            "y" | "xz" => Ok((Vec3::Y, at)),
            "z" | "xy" => Ok((Vec3::Z, at)),
            other => Err(format!("unknown plane {other}; use x, y, z or {{normal, point}}")),
        },
        v @ Value::Object(_) => {
            let n = vec3(v, "normal")?.ok_or("plane needs normal [x, y, z]")?.normalize_or_zero();
            if n == Vec3::ZERO || !n.is_finite() {
                return Err("plane normal must be nonzero".into());
            }
            let p = vec3(v, "point")?.unwrap_or(Vec3::ZERO);
            Ok((n, n.dot(p)))
        }
        _ => Err("plane must be x, y, z or {normal, point}".into()),
    }
}

/// Euler degrees (XYZ order) to a rotation.
fn euler(v: Option<Vec3>) -> Quat {
    v.map_or(Quat::IDENTITY, crate::clay::rotation)
}

impl ClideApp {
    /// `mirror_object`: a mirror copy (or `action: unlink`).
    pub(super) fn mirror_object(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        if args["action"].as_str() == Some("unlink") {
            let n = pose::unlink(&mut self.scene, id);
            return Ok(text(json!({ "unlinked": n, "target": id })));
        }
        if let Some(a) = args["action"].as_str().filter(|a| *a != "mirror") {
            return Err(format!("unknown action {a}; use mirror (default) or unlink"));
        }
        let (normal, d) = plane(args)?;
        // Older scenes: find which object each kitbash group sits on.
        let inferred = self.at_rest(|app| {
            app.meshes.sync(&app.scene);
            pose::infer_follow(&mut app.scene, &app.meshes)
        });
        let src = self.scene.get(id).ok_or("object vanished")?.clone();
        let name = args["name"].as_str().map(str::to_string).unwrap_or_else(|| pose::side_name(&src.name));
        let mut replaced = Vec::new();
        if args["replace"].as_bool().unwrap_or(false)
            && let Some(old) = self.scene.objects.iter().find(|o| o.name == name && o.id != id).map(|o| o.id)
        {
            if self.scene.is_ancestor(old, id) {
                return Err(format!("{name} holds {}; cannot replace it", src.name));
            }
            let mut doomed = vec![old];
            doomed.extend(self.scene.descendants(old));
            let groups: Vec<u64> = self.scene.objects.iter().filter(|g| g.follow.as_ref().is_some_and(|f| doomed.contains(&f.object))).map(|g| g.id).collect();
            for g in groups.into_iter().chain([old]) {
                if let Some(o) = self.scene.get(g) {
                    replaced.push(o.name.clone());
                    self.scene.remove_tree(g);
                }
            }
            crate::kitbash::place::collect_garbage(&mut self.scene);
        }
        let linked = args["linked"].as_bool().unwrap_or(false);
        let opts = pose::MirrorOptions { normal, d, linked, name: Some(name), kitbash: args["kitbash"].as_bool().unwrap_or(true) };
        let made = pose::mirror(&mut self.scene, id, &opts)?;
        self.prune_selection();
        self.select_only(made.root);
        let names = |ids: &[u64]| ids.iter().filter_map(|&i| self.scene.get(i).map(|o| o.name.clone())).collect::<Vec<_>>();
        let mut out = object_json_in(&self.scene, made.root);
        out["linked"] = json!(linked);
        out["objects_made"] = json!(made.objects.len());
        out["kitbash_groups"] = json!(names(&made.groups));
        if !made.negative.is_empty() {
            out["mirrored_by_negative_scale"] = json!({ "objects": names(&made.negative), "why": "their geometry cannot be reflected itself (kit pieces, hair, terrain, single parts), so the copy's matrix mirrors it" });
        }
        if !made.skipped.is_empty() {
            out["skipped"] = json!({ "objects": made.skipped, "why": "cloth and fire are simulated: mirror their parent and drape or simulate again" });
        }
        if !replaced.is_empty() {
            out["replaced"] = json!(replaced);
        }
        if inferred > 0 {
            out["kitbash_groups_bound"] = json!(inferred);
        }
        out["next"] = json!(if linked {
            "Linked: edits of the source (blobs, sculpt, paint, materials, moves, new kitbash passes) show on the copy. mirror_object {target, action: unlink} frees it."
        } else {
            "Done. Mirror again (replace: true) after editing the source, or use linked: true."
        });
        Ok(text(out))
    }

    /// `align`, `frame` and `grip` on update_object / create_object.
    /// Returns whether any was given.
    pub(super) fn apply_align(&mut self, id: u64, args: &Value) -> Result<bool, String> {
        let mut any = false;
        if let Some(a) = args.get("align").filter(|v| !v.is_null()) {
            self.align_axes(id, a)?;
            any = true;
        }
        if let Some(f) = args.get("frame").filter(|v| !v.is_null()) {
            self.align_frame(id, f, false)?;
            any = true;
        }
        if let Some(g) = args.get("grip").filter(|v| !v.is_null()) {
            let mut g = g.clone();
            if g.get("parent").is_none() {
                g["parent"] = g.get("bone").cloned().ok_or("grip needs bone (the hand bone)")?;
            }
            if g.get("at").is_none() {
                g["at"] = json!(0.5);
            }
            self.align_frame(id, &g, true)?;
            any = true;
        }
        if any && self.scene.get(id).is_some_and(|o| o.follow.is_some()) {
            pose::refollow(&mut self.scene, id);
        }
        Ok(any)
    }

    /// Two world directions for two of the object's own axes.
    fn align_axes(&mut self, id: u64, a: &Value) -> Result<(), String> {
        const KEYS: [(&str, Vec3); 5] = [("forward", Vec3::Z), ("x_axis", Vec3::X), ("y_axis", Vec3::Y), ("z_axis", Vec3::Z), ("up", Vec3::Y)];
        if let Some(k) = a.as_object().and_then(|o| o.keys().find(|k| !KEYS.iter().any(|(n, _)| n == k) && k.as_str() != "primary")) {
            return Err(format!("align: unknown key {k}; use two of x_axis, y_axis, z_axis, forward, up"));
        }
        let mut pairs: Vec<(&str, Vec3, Vec3)> = Vec::new();
        for (k, axis) in KEYS {
            if let Some(v) = vec3(a, k)? {
                if !v.is_finite() || v.length() < 1e-9 {
                    return Err(format!("align.{k} must be a nonzero direction"));
                }
                pairs.push((k, axis, v.normalize()));
            }
        }
        if let Some(p) = a["primary"].as_str() {
            let i = pairs.iter().position(|x| x.0 == p).ok_or_else(|| format!("align.primary {p} is not one of the given axes"))?;
            let first = pairs.remove(i);
            pairs.insert(0, first);
        }
        let pairs: Vec<(Vec3, Vec3)> = pairs.iter().map(|(_, a, v)| (*a, *v)).collect();
        pose::align_axes(&mut self.scene, id, &pairs)
    }

    /// Puts the object in a frame of another object or a bone:
    /// `{parent, offset, rotate, roll, at, point, attach}`.
    fn align_frame(&mut self, id: u64, f: &Value, grip: bool) -> Result<(), String> {
        let parent = f.get("parent").filter(|v| !v.is_null()).ok_or("frame needs parent: an object or a bone name")?;
        let offset = vec3(f, "offset")?.unwrap_or(Vec3::ZERO);
        let rotate = euler(vec3(f, "rotate")?);
        let roll = f["roll"].as_f64().unwrap_or(0.0) as f32;
        let at = f["at"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
        let attach = f["attach"].as_bool().unwrap_or(grip);
        // A bone (of the object's rig parent first, else any rig), or an object.
        let target = match parent.as_str().and_then(|b| self.rig_with_bone(id, b).map(|r| (r, b.to_string()))) {
            Some((rig, bone)) => pose::FrameOf::Bone { rig, bone, at },
            None => pose::FrameOf::Object(self.resolve(parent).map_err(|_| format!("frame parent {parent} is neither an object nor a bone"))?),
        };
        let o = self.scene.get(id).ok_or("object vanished")?;
        let anchor = match f.get("point") {
            None | Some(Value::Null) => o.pivot,
            Some(Value::String(n)) => pose::named_point(o, n).ok_or_else(|| format!("{} has no blob named {n} to hold it by", o.name))?,
            Some(v) => vec3(&json!({ "p": v }), "p")?.ok_or("point must be [x, y, z] (object space) or a blob name")?,
        };
        pose::frame_place(&mut self.scene, id, &pose::FramePlace { of: target, offset, rotate, roll, anchor, attach })
    }

    /// The rig object holding bone `b`: the object's own rig parent first.
    fn rig_with_bone(&self, id: u64, b: &str) -> Option<u64> {
        let has = |o: &crate::scene::Object| o.rig.as_ref().is_some_and(|r| r.bone_index(b).is_some());
        let own = self.scene.get(id).and_then(|o| o.parent).and_then(|p| self.scene.get(p)).filter(|p| has(p));
        own.or_else(|| self.scene.objects.iter().find(|o| has(o))).map(|o| o.id)
    }

    /// Splits each mirrored blob of the rig clay that `moves` would move
    /// asymmetrically (`pose::asymmetric_blobs`) into its two sides,
    /// checked first so a refusal changes nothing.
    pub(super) fn split_for_moves(&mut self, rig_id: u64, moves: &[pose::Move]) -> Result<Vec<pose::Split>, String> {
        let o = self.scene.get(rig_id).ok_or("object vanished")?;
        let (Some(rig), Some(clay)) = (o.rig.as_ref(), o.clay.as_ref()) else { return Ok(Vec::new()) };
        let found = pose::asymmetric_blobs(clay, rig, moves);
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let (rig, mut clay) = (rig.clone(), clay.clone());
        let mut out = Vec::new();
        // From the last, so earlier indices stay put.
        for (i, why) in found.into_iter().rev() {
            let from = clay.blobs[i].name.clone();
            let (a, b) = pose::split_mirrored(&mut clay, i, Some(&rig))?;
            out.push(pose::Split { from, into: [a, b], why });
        }
        out.reverse();
        self.scene.get_mut(rig_id).expect("checked").clay = Some(clay);
        Ok(out)
    }

    /// `pose_bone`: moves a bone's rest pose, carrying what rides it.
    pub(super) fn pose_bone(&mut self, args: &Value) -> ToolResult {
        let bone = args["bone"].as_str().ok_or("bone is required")?.to_string();
        let rig_id = match args.get("target").filter(|v| !v.is_null()) {
            Some(t) => self.resolve(t)?,
            None => self.scene.objects.iter().find(|o| o.rig.as_ref().is_some_and(|r| r.bone_index(&bone).is_some())).map(|o| o.id).ok_or_else(|| format!("no rig has a bone {bone}"))?,
        };
        let o = self.scene.get(rig_id).ok_or("object vanished")?;
        let rig = o.rig.as_ref().ok_or_else(|| format!("{} has no rig", o.name))?;
        let b = rig.bones.iter().find(|x| x.name == bone).ok_or_else(|| format!("{} has no bone {bone}", o.name))?.clone();
        let had_detail = o.clay.as_ref().is_some_and(|c| c.detail.is_some() || c.paint.is_some() || !crate::layers::extra(c).is_empty());
        let world = pose::at_rest(&mut self.scene, |s| s.world_matrix(rig_id));
        let local = match args["space"].as_str().unwrap_or("world") {
            "world" => world.inverse(),
            "local" => Mat4::IDENTITY,
            other => return Err(format!("unknown space {other}; use world or local")),
        };
        let pt = |k: &str| -> Result<Option<Vec3>, String> { Ok(vec3(args, k)?.map(|v| local.transform_point3(v))) };
        let dir = |v: Vec3| local.transform_vector3(v);
        // The motion in rig space: turn about the head (or `pivot`), then move.
        let pivot = pt("pivot")?.unwrap_or(b.head);
        let mut turn = euler(vec3(args, "rotate")?);
        if args["space"].as_str().unwrap_or("world") == "world" && vec3(args, "rotate")?.is_some() {
            // World axes -> rig axes.
            let (_, r, _) = world.to_scale_rotation_translation();
            turn = r.inverse() * turn * r;
        }
        let old_dir = (b.tail - b.head).normalize_or(Vec3::Y);
        if let Some(t) = pt("aim")? {
            turn = Quat::from_rotation_arc(old_dir, (t - b.head).normalize_or(old_dir)) * turn;
        }
        let mut shift = vec3(args, "translate")?.map(dir).unwrap_or(Vec3::ZERO);
        if let Some(h) = pt("head")? {
            shift += h - b.head;
        }
        if let Some(t) = pt("tail")? {
            let head = b.head + shift;
            turn = Quat::from_rotation_arc((turn * old_dir).normalize_or(old_dir), (t - head).normalize_or(old_dir)) * turn;
        }
        let delta = Mat4::from_translation(shift) * Mat4::from_translation(pivot) * Mat4::from_quat(turn.normalize()) * Mat4::from_translation(-pivot);
        let carry = args["carry"].as_bool().unwrap_or(true);
        let extra: Vec<String> = args["blobs"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        if carry {
            // Carried blobs move; their sculpt ledgers settle on the old shape first.
            let ids: Vec<u64> = self.scene.objects.iter().map(|o| o.id).collect();
            self.settle_clays(ids)?;
        }
        let mut moves = vec![pose::Move { bone: bone.clone(), delta, extra: extra.clone() }];
        if args["mirror"].as_bool().unwrap_or(false) {
            let other = crate::rig::mirror_name(&bone).ok_or_else(|| format!("{bone} has no side suffix (.L/.R) to mirror"))?;
            let o = self.scene.get(rig_id).ok_or("object vanished")?;
            if o.rig.as_ref().is_none_or(|r| r.bone_index(&other).is_none()) {
                return Err(format!("{} has no bone {other} to mirror {bone} onto", o.name));
            }
            let f = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
            let extra_m: Vec<String> = extra.iter().map(|n| pose::side_name(n)).collect();
            moves.push(pose::Move { bone: other, delta: f * delta * f, extra: extra_m });
        }
        // A mirrored blob is one stored side reflected across x = 0: a
        // move that is not its own mirror (a head tilt, one arm) writes it
        // out as its two sides first, each then moved by its own bone.
        let split = if carry { self.split_for_moves(rig_id, &moves)? } else { Vec::new() };
        let mut reports = Vec::new();
        for m in &moves {
            reports.push((m.bone.clone(), pose::pose_rest(&mut self.scene, rig_id, &m.bone, m.delta, carry, &m.extra)?));
        }
        let name = |id: u64| self.scene.get(id).map(|o| o.name.clone());
        let mut list = Vec::new();
        for (b, r) in &reports {
            list.push(json!({
                "bone": b,
                "bones_moved": r.bones,
                "objects_carried": r.objects.iter().filter_map(|&i| name(i)).collect::<Vec<_>>(),
                "blobs_carried": r.blobs,
                "weight_strokes_carried": r.strokes,
                "approximate_blobs": Some(r.approximate.iter().map(|(n, why)| json!({ "name": n, "why": why })).collect::<Vec<_>>()).filter(|a| !a.is_empty()),
            }));
        }
        let rig = self.scene.get(rig_id).and_then(|o| o.rig.as_ref()).expect("still there");
        let joints: serde_json::Map<String, Value> = reports.iter().flat_map(|(_, r)| r.bones.iter()).filter_map(|n| rig.bones.iter().find(|x| &x.name == n)).map(|x| (x.name.clone(), json!({ "head": round3(world.transform_point3(x.head)), "tail": round3(world.transform_point3(x.tail)) }))).collect();
        let mut out = json!({ "rig": name(rig_id), "posed": list, "rest_joints_world": joints });
        if !split.is_empty() {
            out["split_blobs"] = json!(split);
        }
        if had_detail && carry {
            out["note"] = json!("sculpt and paint grids (and layers) of the rig clay stay where they are; only blobs moved");
        }
        Ok(text(out))
    }
}

fn round3(v: Vec3) -> [f32; 3] {
    [(v.x * 1000.0).round() / 1000.0, (v.y * 1000.0).round() / 1000.0, (v.z * 1000.0).round() / 1000.0]
}
