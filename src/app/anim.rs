//! Agent tools for rigs and animation (`rig`, `animate`, `playback`,
//! `render_animation`) and the skeleton overlay in renders and the viewport.
//! The data and the math live in `crate::rig`.

use egui::{Color32, ColorImage};
use glam::{Mat4, Vec3, Vec4Swizzles};
use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::{Shot, contact_sheet};
use crate::clay::Field;
use crate::mcp::ToolResult;
use crate::rig::{self, Bone, Clip, Key, MAX_BONES, MAX_CLIPS, MAX_KEYS, MAX_NAME, Rig, Track};
use crate::scene::Shape;

const BONE: Color32 = Color32::from_rgb(255, 170, 30);
const BONE_LEAD: Color32 = Color32::from_rgb(120, 255, 140);

impl ClideApp {
    /// The clay object a rig tool works on, and its rig (empty if none).
    fn rig_of(&self, args: &Value) -> Result<(u64, Rig), String> {
        let id = self.resolve(&args["target"])?;
        let obj = self.scene.get(id).ok_or("object vanished")?;
        if obj.shape != Shape::Clay {
            return Err(format!("{} is not a clay object; only clay can be rigged", obj.name));
        }
        Ok((id, obj.rig.clone().unwrap_or_default()))
    }

    /// `rig`: clear, remove, update, then add bones. Coordinates are in the
    /// clay's own space, like blobs.
    pub(super) fn rig_tool(&mut self, args: &Value) -> ToolResult {
        let (id, mut rig) = self.rig_of(args)?;
        let mut clay = self.scene.get(id).expect("resolved").clay.clone();
        if args["clear"].as_bool().unwrap_or(false) {
            rig = Rig::default();
        }
        for name in args["remove"].as_array().into_iter().flatten() {
            let name = name.as_str().ok_or("remove takes bone names")?;
            let i = rig.bone_index(name).ok_or_else(|| format!("no bone {name}"))?;
            let gone = rig.bones.remove(i);
            for blob in clay.iter_mut().flat_map(|c| &mut c.blobs) {
                if blob.bone == name { blob.bone.clear(); }
            }
            for b in &mut rig.bones {
                if b.parent.as_deref() == Some(name) {
                    b.parent = gone.parent.clone();
                }
            }
            rig.weights.retain(|s| s.bone != name);
            for c in &mut rig.clips {
                c.tracks.retain(|t| t.bone != name);
            }
        }
        for (n, u) in args["update"].as_array().into_iter().flatten().enumerate() {
            let name = u["bone"].as_str().ok_or(format!("update {n}: bone (name) is required"))?;
            let i = rig.bone_index(name).ok_or_else(|| format!("update {n}: no bone {name}"))?;
            if let Some(v) = vec3(u, "head")? {
                rig.bones[i].head = v;
            }
            if let Some(v) = vec3(u, "tail")? {
                rig.bones[i].tail = v;
            }
            if let Some(d) = u["deform"].as_bool() {
                rig.bones[i].deform = d;
            }
            match u.get("parent") {
                None => {}
                Some(Value::Null) => rig.bones[i].parent = None,
                Some(p) => {
                    let p = p.as_str().ok_or("parent must be a bone name or null")?;
                    rig.bone_index(p).ok_or_else(|| format!("update {n}: no bone {p}"))?;
                    let mut cur = Some(p.to_string());
                    while let Some(c) = cur {
                        if c == name {
                            return Err(format!("update {n}: {p} is under {name}; that would make a loop"));
                        }
                        cur = rig.bones.iter().find(|b| b.name == c).and_then(|b| b.parent.clone());
                    }
                    rig.bones[i].parent = Some(p.into());
                }
            }
            if let Some(new) = u["name"].as_str() {
                check_name(new)?;
                if new != name && rig.bone_index(new).is_some() {
                    return Err(format!("update {n}: bone {new} already exists"));
                }
                rig.bones[i].name = new.into();
                for blob in clay.iter_mut().flat_map(|c| &mut c.blobs) {
                    if blob.bone == name { blob.bone = new.into(); }
                }
                for b in &mut rig.bones {
                    if b.parent.as_deref() == Some(name) {
                        b.parent = Some(new.into());
                    }
                }
                for stroke in &mut rig.weights {
                    if stroke.bone == name { stroke.bone = new.into(); }
                }
                for t in rig.clips.iter_mut().flat_map(|c| c.tracks.iter_mut()) {
                    if t.bone == name {
                        t.bone = new.into();
                    }
                }
            }
        }
        if let Some(split) = args["split"].as_bool() {
            rig.split = split;
        }
        for (n, a) in args["add"].as_array().into_iter().flatten().enumerate() {
            for bone in parse_bones(a, &rig).map_err(|e| format!("add {n}: {e}"))? {
                if rig.bones.len() >= MAX_BONES {
                    return Err(format!("at most {MAX_BONES} bones"));
                }
                rig.bones.push(bone);
            }
        }
        if args["clear_weights"].as_bool().unwrap_or(false) { rig.weights.clear(); }
        if let Some(strokes) = args.get("weights") {
            let strokes = strokes.as_array().ok_or("weights must be an array of strokes")?;
            if rig.weights.len() + strokes.len() > rig::MAX_WEIGHT_STROKES {
                return Err(format!("at most {} weight strokes", rig::MAX_WEIGHT_STROKES));
            }
            let world = self.scene.world_matrix(id);
            let inv = world.inverse();
            if !inv.is_finite() { return Err("object transform is singular".into()); }
            for value in strokes {
                let mut stroke: rig::WeightStroke = serde_json::from_value(value.clone()).map_err(|e| format!("weight stroke: {e}"))?;
                if !stroke.valid() { return Err("weight stroke: use 1-256 finite points, positive radius, and strength in 0..1".into()); }
                let b = rig.bone_index(&stroke.bone).ok_or_else(|| format!("no bone {}", stroke.bone))?;
                if !rig.bones[b].deform { return Err(format!("{} is not a deform bone", stroke.bone)); }
                stroke.points.iter_mut().for_each(|p| *p = inv.transform_point3(*p));
                stroke.space = world;
                rig.weights.push(stroke);
            }
        }
        rig.sanitize();
        let obj = self.scene.get_mut(id).expect("resolved");
        obj.clay = clay;
        obj.rig = (!rig.bones.is_empty() || !rig.clips.is_empty()).then_some(rig);
        // The summary reads the new skin weights: answer once they are built.
        let details = args["details"].as_bool().unwrap_or(false);
        self.defer_reply(move |app| Ok(text(app.rig_summary(id, details))))
    }

    /// Bones, how much of the mesh each one moves, and bones that sit outside the clay.
    /// Compact unless `details`: each bone's name and how many vertices it
    /// moves (plus the warnings); details add parents, heads, tails.
    fn rig_summary(&mut self, id: u64, details: bool) -> Value {
        self.meshes.sync(&self.scene);
        let Some(o) = self.scene.get(id) else { return Value::Null };
        let Some(rig) = &o.rig else { return json!({ "id": id, "bones": [], "note": "No rig. Add bones with rig {add: [...]}." }) };
        let counts = self.meshes.rest(id).and_then(|(_, s)| s).map(|s| s.counts(rig.bones.len())).unwrap_or_default();
        let clay = o.clay.clone().unwrap_or_default();
        let inside = bones_inside(&clay, &rig.bones);
        // Bones that carry their own clay (a head clay on the head bone),
        // and the bones under them, sit outside this clay by design.
        let ridden: Vec<&str> = self.scene.objects.iter().filter(|c| c.parent == Some(id) && c.shape == Shape::Clay).filter_map(|c| c.bone.as_deref()).collect();
        let carried = |i: usize| {
            let mut b = Some(i);
            while let Some(k) = b {
                if ridden.contains(&rig.bones[k].name.as_str()) {
                    return true;
                }
                b = rig.bones[k].parent.as_ref().and_then(|p| rig.bone_index(p));
            }
            false
        };
        let mut warnings = Vec::new();
        let bones: Vec<Value> = rig
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let moves = counts.get(i).copied().unwrap_or(0);
                if inside[i] < 0.5 && b.deform && !carried(i) {
                    warnings.push(format!("{} is mostly outside the clay; move it inside the part it should move", b.name));
                } else if b.deform && moves == 0 {
                    warnings.push(format!("{} moves no vertices (other bones are closer)", b.name));
                }
                if !details {
                    return json!([b.name, moves]);
                }
                json!({
                    "name": b.name,
                    "parent": b.parent,
                    "head": round3(b.head),
                    "tail": round3(b.tail),
                    "deform": b.deform,
                    "moves_vertices": moves,
                    "inside_clay": round(inside[i]),
                })
            })
            .collect();
        json!({
            "id": id,
            "name": o.name,
            "bones": bones,
            "bones_are": if details { "full" } else { "[name, vertices it moves]; pass details: true for parents, heads and tails" },
            "weight_strokes": rig.weights.len(),
            "clips": rig.clips.iter().map(clip_summary).collect::<Vec<_>>(),
            "active_clip": rig.active,
            "split": rig.split,
            "warnings": warnings,
        })
    }

    /// `animate`: edit one clip's keys.
    pub(super) fn animate(&mut self, args: &Value) -> ToolResult {
        let (id, mut rig) = self.rig_of(args)?;
        if rig.bones.is_empty() {
            return Err("no bones; add them with the rig tool first".into());
        }
        let name = args["clip"].as_str().unwrap_or("action").to_string();
        check_name(&name)?;
        if args["delete"].as_bool().unwrap_or(false) {
            let before = rig.clips.len();
            rig.clips.retain(|c| c.name != name);
            if rig.clips.len() == before {
                return Err(format!("no clip {name}"));
            }
            if rig.active.as_deref() == Some(name.as_str()) {
                rig.active = None;
            }
            self.scene.get_mut(id).expect("resolved").rig = Some(rig);
            return Ok(text(json!({ "deleted": name })));
        }
        let ci = match rig.clips.iter().position(|c| c.name == name) {
            Some(i) => i,
            None => {
                if rig.clips.len() >= MAX_CLIPS {
                    return Err(format!("at most {MAX_CLIPS} clips"));
                }
                rig.clips.push(Clip::new(&name));
                rig.clips.len() - 1
            }
        };
        let mirror_all = args["mirror"].as_bool().unwrap_or(false);
        let bones: Vec<String> = rig.bones.iter().map(|b| b.name.clone()).collect();
        let clip = &mut rig.clips[ci];
        if let Some(l) = args["loop"].as_bool() {
            clip.looped = l;
        }
        for (n, r) in args["remove"].as_array().into_iter().flatten().enumerate() {
            let frame = r["frame"].as_f64().map(|f| f as f32);
            match (r["bone"].as_str(), frame) {
                (Some(b), Some(f)) => clip.track_mut(b).keys.retain(|k| (k.frame - f).abs() >= 1e-4),
                (Some(b), None) => clip.tracks.retain(|t| t.bone != b),
                (None, Some(f)) => clip.tracks.iter_mut().for_each(|t| t.keys.retain(|k| (k.frame - f).abs() >= 1e-4)),
                (None, None) => return Err(format!("remove {n}: pass bone, frame, or both")),
            }
        }
        let keys = args["keys"].as_array().map(Vec::as_slice).unwrap_or_default();
        for (n, k) in keys.iter().enumerate() {
            let bone = k["bone"].as_str().ok_or(format!("key {n}: bone is required"))?;
            if !bones.iter().any(|b| b == bone) {
                return Err(format!("key {n}: no bone {bone}"));
            }
            let frame = k["frame"].as_f64().filter(|f| f.is_finite() && f.abs() < 1e6).ok_or(format!("key {n}: frame (number) is required"))? as f32;
            let key = set_key(clip.track_mut(bone), frame, k).map_err(|e| format!("key {n}: {e}"))?;
            if k["mirror"].as_bool().unwrap_or(mirror_all)
                && let Some(other) = rig::mirror_name(bone).filter(|o| bones.contains(o))
            {
                clip.track_mut(&other).insert(rig::mirror_key(&key));
            }
        }
        clip.tracks.retain(|t| !t.keys.is_empty());
        if clip.key_count() > MAX_KEYS {
            return Err(format!("at most {MAX_KEYS} keys per clip"));
        }
        if args["play"].as_bool().unwrap_or(true) {
            rig.active = Some(name.clone());
        }
        rig.sanitize();
        let summary = rig.clip(&name).map(clip_summary);
        let range = rig.clip(&name).and_then(Clip::range);
        let active = rig.active.clone();
        self.scene.get_mut(id).expect("resolved").rig = Some(rig);
        if active.as_deref() == Some(name.as_str())
            && let Some((a, b)) = range
        {
            self.show_range(a, b);
        }
        Ok(text(json!({ "clip": summary, "active_clip": active, "fps": self.timeline.fps })))
    }

    /// After loading a scene: show the first playing clip's range.
    pub(super) fn fit_range_to_clips(&mut self) {
        let range = self.scene.objects.iter().find_map(|o| o.rig.as_ref()?.active_clip()?.range());
        if let Some((a, b)) = range {
            self.show_range(a, b);
        }
    }

    /// Fits the time slider's playback range to a clip.
    fn show_range(&mut self, a: f32, b: f32) {
        let t = &mut self.timeline;
        let (a, b) = (a.floor(), b.ceil().max(a.floor() + 1.0));
        t.anim_start = t.anim_start.min(a);
        t.anim_end = t.anim_end.max(b);
        t.play_start = a;
        t.play_end = b;
        t.frame = t.frame.clamp(a, b);
    }

    /// `playback`: the time slider, and which clip each rig plays.
    pub(super) fn playback(&mut self, args: &Value) -> ToolResult {
        if args.get("clip").is_some() {
            let (id, mut rig) = self.rig_of(args)?;
            let clip = args["clip"].as_str();
            if let Some(c) = clip
                && rig.clip(c).is_none()
            {
                return Err(format!("no clip {c}; clips: {:?}", rig.clips.iter().map(|c| &c.name).collect::<Vec<_>>()));
            }
            rig.active = clip.map(Into::into);
            let range = rig.active_clip().and_then(Clip::range);
            self.scene.get_mut(id).expect("resolved").rig = Some(rig);
            if let Some((a, b)) = range {
                self.show_range(a, b);
            }
        }
        if let Some(fps) = args["fps"].as_f64() {
            self.timeline.fps = (fps as f32).clamp(1.0, 120.0);
        }
        if let Some(f) = args["frame"].as_f64().filter(|f| f.is_finite()) {
            let t = &mut self.timeline;
            let f = (f as f32).clamp(-1e5, 1e5);
            t.anim_start = t.anim_start.min(f.floor());
            t.anim_end = t.anim_end.max(f.ceil());
            t.frame = f;
        }
        if let Some(p) = args["play"].as_bool() {
            self.timeline.playing = p.then_some(1.0);
        }
        let playing: Vec<Value> = self
            .scene
            .objects
            .iter()
            .filter_map(|o| Some((o, o.rig.as_ref()?)))
            .map(|(o, r)| json!({ "object": o.name, "active_clip": r.active, "clips": r.clips.iter().map(|c| &c.name).collect::<Vec<_>>() }))
            .collect();
        let t = &self.timeline;
        Ok(text(json!({
            "frame": t.frame,
            "playing": t.playing.is_some(),
            "fps": t.fps,
            "play_range": [t.play_start, t.play_end],
            "rigs": playing,
        })))
    }

    /// `render_animation`: one view at several frames, as a contact sheet.
    /// It renders a snapshot of the scene on its own thread (a background
    /// job like render_film), so long clips and big scenes never time out.
    pub(super) fn render_animation(&mut self, args: &Value) -> Result<super::jobs::Started, String> {
        let mut scene = self.scene.clone();
        if let Some(clip) = args["clip"].as_str() {
            let mut found = false;
            for rig in scene.objects.iter_mut().filter_map(|o| o.rig.as_mut()) {
                if rig.clip(clip).is_some() {
                    rig.active = Some(clip.to_string());
                    found = true;
                }
            }
            if !found {
                return Err(format!("no rig has a clip {clip}"));
            }
        }
        let ranges: Vec<(f32, f32, bool)> = scene
            .objects
            .iter()
            .filter_map(|o| o.rig.as_ref()?.active_clip())
            .filter_map(|c| c.range().map(|(a, b)| (a, b, c.looped)))
            .collect();
        let frames: Vec<f32> = match &args["frames"] {
            Value::Array(list) => list.iter().map(|f| f.as_f64().filter(|f| f.is_finite()).map(|f| f as f32).ok_or("frames must be numbers")).collect::<Result<_, _>>()?,
            other => {
                let count = other.as_u64().unwrap_or(8).clamp(2, 16) as usize;
                let (a, b) = ranges.iter().fold((f32::MAX, f32::MIN), |(a, b), r| (a.min(r.0), b.max(r.1)));
                if a > b {
                    return Err("no clip is playing; make one with animate, or pick one with playback or clip".into());
                }
                // A loop's last frame repeats its first, so leave it out.
                let looped = ranges.iter().all(|r| r.2);
                let span = if looped { (b - a) / count as f32 } else { (b - a) / (count - 1) as f32 };
                (0..count).map(|i| ((a + span * i as f32) * 10.0).round() / 10.0).collect()
            }
        };
        if frames.is_empty() || frames.len() > 16 {
            return Err("pass 1 to 16 frames".into());
        }
        let size = args["size"].as_u64().unwrap_or(256).clamp(64, 512) as usize;
        let view = args.get("view").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| Value::from("three_quarter"));
        Shot::framing(&view, Vec3::ZERO, 1.0)?;
        let bones = args["show_bones"].as_bool().unwrap_or(false);
        let center = vec3(args, "center")?;
        let radius = args["radius"].as_f64().map(|r| (r as f32).clamp(0.01, 1000.0));
        let save = match args["save_path"].as_str() {
            Some(p) => {
                let p = std::path::PathBuf::from(p);
                if !p.is_absolute() || p.extension().and_then(|e| e.to_str()) != Some("png") {
                    return Err("save_path must be an absolute .png path".into());
                }
                Some(p)
            }
            None => None,
        };
        // Meshes as they are now (waiting for builds), then a copy for the job.
        let keep = self.meshes.frame;
        self.set_frame(None);
        self.meshes.sync(&self.scene);
        let meshes = self.meshes.clone();
        self.set_frame(keep);
        let job = AnimJob { frames, size, view, bones, center, radius, save, lead: self.lead(), fps: self.timeline.fps };
        let label = format!("Render animation ({} frames)", job.frames.len());
        Ok(super::jobs::Started::Job(label, animation_job(scene, meshes, self.render_state.clone(), job)))
    }

    /// `weights` argument: a bone name -> (object, bone index).
    pub(super) fn weight_target(&self, v: &Value) -> Result<Option<(u64, usize)>, String> {
        let Some(name) = v.as_str() else { return Ok(None) };
        self.scene
            .objects
            .iter()
            .find_map(|o| Some((o.id, o.rig.as_ref()?.bone_index(name)?)))
            .map(Some)
            .ok_or_else(|| format!("no bone {name}"))
    }

    /// Posed bones of every visible rig: (lead object?, world head, world tail).
    fn world_bones(&self) -> Vec<(bool, Vec3, Vec3)> {
        world_bones_of(&self.scene, self.meshes.frame, self.lead())
    }

    /// Draws the skeletons over an off-screen render (seen through the clay).
    pub(super) fn draw_bones(&self, img: &mut ColorImage, view_proj: Mat4) {
        draw_bones_on(img, view_proj, &self.world_bones());
    }

    /// Draws the skeletons over a viewport pane.
    pub(super) fn paint_bones(&self, painter: &egui::Painter, rect: egui::Rect, view_proj: Mat4) {
        if !self.show_bones {
            return;
        }
        let to_px = |p: Vec3| {
            let c = view_proj * p.extend(1.0);
            (c.w > 1e-6).then(|| {
                let n = c.xyz() / c.w;
                egui::pos2(rect.left() + (n.x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - n.y * 0.5) * rect.height())
            })
        };
        for (lead, head, tail) in self.world_bones() {
            let (Some(a), Some(b)) = (to_px(head), to_px(tail)) else { continue };
            let color = if lead { BONE_LEAD } else { BONE };
            painter.line_segment([a, b], egui::Stroke::new(3.0, Color32::from_black_alpha(160)));
            painter.line_segment([a, b], egui::Stroke::new(1.5, color));
            painter.circle(a, 3.0, Color32::WHITE, egui::Stroke::new(1.0, Color32::BLACK));
        }
    }

    /// Key frames of the lead object's playing clip, for the time slider.
    pub(super) fn lead_key_frames(&self) -> Vec<f32> {
        self.lead()
            .and_then(|id| self.scene.get(id))
            .and_then(|o| o.rig.as_ref())
            .and_then(|r| r.active_clip())
            .map(Clip::frames)
            .unwrap_or_default()
    }
}

/// Sets the channels given in `k` on the key at `frame`. Channels left out
/// keep the value the track already has there.
fn set_key(track: &mut Track, frame: f32, k: &Value) -> Result<Key, String> {
    let mut key = track.key_at(frame).copied().unwrap_or_else(|| {
        let mut base = track.sample_key(frame);
        base.frame = frame;
        base
    });
    if let Some(r) = vec3(k, "rotate")? {
        key.rotate = r;
    }
    if let Some(t) = vec3(k, "translate")? {
        key.translate = t;
    }
    match &k["scale"] {
        Value::Null => {}
        Value::Number(n) => key.scale = Vec3::splat(n.as_f64().unwrap_or(1.0) as f32),
        _ => key.scale = vec3(k, "scale")?.expect("not null"),
    }
    key.scale = key.scale.clamp(Vec3::splat(0.01), Vec3::splat(100.0));
    if !k["interp"].is_null() {
        key.interp = serde_json::from_value(k["interp"].clone()).map_err(|_| "interp must be smooth, linear or step")?;
    }
    track.insert(key);
    Ok(key)
}

/// One `add` entry -> bones: a single bone (head, tail), a chain (points),
/// and the mirrored copies when `mirror` is set.
fn parse_bones(a: &Value, rig: &Rig) -> Result<Vec<Bone>, String> {
    let name = a["name"].as_str().ok_or("name is required")?;
    check_name(name)?;
    let parent = a["parent"].as_str().map(String::from);
    let deform = a["deform"].as_bool().unwrap_or(true);
    let points: Vec<Vec3> = match (a.get("points"), vec3(a, "head")?, vec3(a, "tail")?) {
        (Some(p), _, _) if !p.is_null() => {
            let pts: Vec<[f32; 3]> = serde_json::from_value(p.clone()).map_err(|_| "points must be a list of [x, y, z]")?;
            if pts.len() < 2 || pts.len() > 33 {
                return Err("points needs 2 to 33 points".into());
            }
            pts.into_iter().map(Vec3::from).collect()
        }
        (_, Some(h), Some(t)) => vec![h, t],
        _ => return Err("pass head and tail, or points for a chain".into()),
    };
    if points.iter().any(|p| !p.is_finite()) {
        return Err("coordinates must be finite".into());
    }
    let chain = points.len() > 2;
    let make = |name: &str, parent: Option<String>, pts: &[Vec3]| -> Vec<Bone> {
        let mut out = Vec::new();
        let mut up = parent;
        for (i, w) in pts.windows(2).enumerate() {
            let n = if chain { format!("{name}{}", i + 1) } else { name.to_string() };
            out.push(Bone { name: n.clone(), parent: up.clone(), head: w[0], tail: w[1], deform });
            up = Some(n);
        }
        out
    };
    // A chain "tail.L" makes tail1.L, tail2.L... so mirror names still work.
    let (stem, suffix) = split_side(name);
    let named = |stem: &str, suffix: &str, i: usize| if chain { format!("{stem}{i}{suffix}") } else { format!("{stem}{suffix}") };
    let mut bones = make(name, parent.clone(), &points);
    if chain {
        for (i, b) in bones.iter_mut().enumerate() {
            b.name = named(stem, suffix, i + 1);
            if i > 0 {
                b.parent = Some(named(stem, suffix, i));
            }
        }
    }
    if a["mirror"].as_bool().unwrap_or(false) {
        let other = rig::mirror_name(name).ok_or(format!("{name} has no side suffix to mirror (use .L/.R, _L/_R or Left/Right)"))?;
        let (ostem, osuffix) = split_side(&other);
        let flip = |p: &Vec3| Vec3::new(-p.x, p.y, p.z);
        let mpoints: Vec<Vec3> = points.iter().map(flip).collect();
        let mparent = parent.as_ref().map(|p| rig::mirror_name(p).filter(|m| rig.bone_index(m).is_some()).unwrap_or(p.clone()));
        let mut mirrored = make(&other, mparent, &mpoints);
        if chain {
            for (i, b) in mirrored.iter_mut().enumerate() {
                b.name = named(ostem, osuffix, i + 1);
                if i > 0 {
                    b.parent = Some(named(ostem, osuffix, i));
                }
            }
        }
        bones.extend(mirrored);
    }
    let mut known: Vec<String> = rig.bones.iter().map(|b| b.name.clone()).collect();
    for b in &bones {
        if known.contains(&b.name) {
            return Err(format!("bone {} already exists", b.name));
        }
        if let Some(p) = &b.parent
            && !known.contains(p)
        {
            return Err(format!("no parent bone {p} (add parents first)"));
        }
        known.push(b.name.clone());
    }
    Ok(bones)
}

/// "arm.L" -> ("arm", ".L"); "spine" -> ("spine", "").
fn split_side(name: &str) -> (&str, &str) {
    for s in [".L", ".R", "_L", "_R", ".l", ".r", "_l", "_r", "Left", "Right", "left", "right"] {
        if let Some(stem) = name.strip_suffix(s) {
            return (stem, s);
        }
    }
    (name, "")
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("names must be 1 to {MAX_NAME} characters"));
    }
    Ok(())
}

/// Share of each bone's length that lies inside the clay.
fn bones_inside(clay: &crate::clay::Clay, bones: &[Bone]) -> Vec<f32> {
    let cell = clay.cell_size().unwrap_or(0.02);
    let field = Field::new(clay, cell);
    let list = field.bounds().map(|(lo, hi)| field.near(lo, hi)).unwrap_or_default();
    bones
        .iter()
        .map(|b| {
            let inside = (0..=8).filter(|i| field.sample(b.head.lerp(b.tail, *i as f32 / 8.0), &list).0 <= 0.0).count();
            inside as f32 / 9.0
        })
        .collect()
}

fn clip_summary(c: &Clip) -> Value {
    json!({
        "name": c.name,
        "loop": c.looped,
        "range": c.range().map(|(a, b)| [a, b]),
        "key_frames": c.frames(),
        "bones": c.tracks.iter().map(|t| &t.bone).collect::<Vec<_>>(),
        "keys": c.key_count(),
    })
}

/// A thick line (or a dot when `a == b`) on an image.
fn line(img: &mut ColorImage, a: (f32, f32), b: (f32, f32), width: f32, color: Color32) {
    let [w, h] = img.size;
    let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    if !len.is_finite() || len > 4.0 * (w + h) as f32 {
        return;
    }
    let steps = (len * 2.0).ceil().max(1.0) as usize;
    let r = width.ceil() as i32;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let (cx, cy) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    continue;
                }
                if ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() <= width {
                    img.pixels[y as usize * w + x as usize] = color;
                }
            }
        }
    }
}

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

fn round3(v: Vec3) -> [f64; 3] {
    [round(v.x), round(v.y), round(v.z)]
}

fn vec3(args: &Value, key: &str) -> Result<Option<Vec3>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("{key} must be [x, y, z]"))?;
            Ok(Some(Vec3::from(a)))
        }
    }
}

fn text(v: Value) -> Vec<Value> {
    vec![json!({ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() })]
}

/// Posed bones of every visible rig: (lead object?, world head, world tail).
fn world_bones_of(scene: &crate::scene::Scene, frame: Option<f32>, lead: Option<u64>) -> Vec<(bool, Vec3, Vec3)> {
    let eval = scene.evaluate();
    let mut out = Vec::new();
    for o in &scene.objects {
        let (Some(rig), Some(&(world, true))) = (o.rig.as_ref(), eval.get(&o.id)) else { continue };
        let frame = if rig.is_posed(frame) { frame } else { None };
        for (h, t) in rig.joints(frame) {
            out.push((lead == Some(o.id), world.transform_point3(h), world.transform_point3(t)));
        }
    }
    out
}

/// Draws bones over an off-screen render.
fn draw_bones_on(img: &mut ColorImage, view_proj: Mat4, bones: &[(bool, Vec3, Vec3)]) {
    let [w, h] = img.size;
    let to_px = |p: Vec3| {
        let c = view_proj * p.extend(1.0);
        (c.w > 1e-6).then(|| {
            let n = c.xyz() / c.w;
            ((n.x * 0.5 + 0.5) * w as f32, (0.5 - n.y * 0.5) * h as f32)
        })
    };
    for &(_, head, tail) in bones {
        let (Some(a), Some(b)) = (to_px(head), to_px(tail)) else { continue };
        line(img, a, b, 2.5, Color32::BLACK);
        line(img, a, b, 1.2, BONE);
        line(img, a, a, 3.5, Color32::BLACK);
        line(img, a, a, 2.5, Color32::WHITE);
    }
}

/// The scene's bounding sphere (visible top-level objects).
fn scene_sphere_of(scene: &crate::scene::Scene, meshes: &crate::mesh::MeshLib) -> (Vec3, f32) {
    let bounds = |o: &crate::scene::Object| meshes.bounds(o);
    let b = scene.children(None).into_iter().filter(|&id| scene.is_visible(id)).filter_map(|id| scene.subtree_bounds(id, scene.world_matrix(id), &bounds).or_else(|| { let p = scene.pivot_world(id); Some((p, p)) })).reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
    match b {
        Some((lo, hi)) => ((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(0.05)),
        None => (Vec3::ZERO, 1.0),
    }
}

/// What a `render_animation` job renders.
pub(super) struct AnimJob {
    pub frames: Vec<f32>,
    pub size: usize,
    pub view: Value,
    pub bones: bool,
    pub center: Option<Vec3>,
    pub radius: Option<f32>,
    pub save: Option<std::path::PathBuf>,
    pub lead: Option<u64>,
    pub fps: f32,
}

/// The work of a `render_animation` job: poses a snapshot of the scene at
/// each frame and renders it off screen (any thread).
pub(super) fn animation_job(mut scene: crate::scene::Scene, mut meshes: crate::mesh::MeshLib, rs: egui_wgpu::RenderState, job: AnimJob) -> super::jobs::Work {
    Box::new(move |progress| {
        let AnimJob { frames, size, view, bones, center, radius, save, lead, fps } = job;
        let n = frames.len() as f32;
        let at = |scene: &mut crate::scene::Scene, meshes: &mut crate::mesh::MeshLib, f: f32| {
            scene.frame = Some(f);
            meshes.frame = Some(f);
            meshes.sync(scene);
        };
        // One camera for every frame, framing the whole motion.
        let (center, radius) = match (center, radius) {
            (Some(c), Some(r)) => (c, r),
            _ => {
                let mut lo = Vec3::splat(f32::MAX);
                let mut hi = Vec3::splat(f32::MIN);
                for (i, &f) in frames.iter().enumerate() {
                    progress.check()?;
                    progress.set(i as f32 / n * 0.3);
                    at(&mut scene, &mut meshes, f);
                    let (c, r) = scene_sphere_of(&scene, &meshes);
                    lo = lo.min(c - r);
                    hi = hi.max(c + r);
                }
                (center.unwrap_or((lo + hi) * 0.5), radius.unwrap_or(((hi - lo) * 0.5).max_element() * 0.8))
            }
        };
        let shot = Shot::framing(&view, center, radius)?;
        let mut tiles = Vec::new();
        for (i, &f) in frames.iter().enumerate() {
            progress.check()?;
            progress.set(0.3 + i as f32 / n * 0.7);
            at(&mut scene, &mut meshes, f);
            let view_proj = shot.view_proj(size, size);
            let cb = super::eyes::scene_callback_of(&scene, &meshes, view_proj, shot.eye, false, false);
            let mut img = crate::render::render_image(&rs, &cb, size as u32, size as u32, super::eyes::BACKGROUND)?;
            if bones {
                draw_bones_on(&mut img, view_proj, &world_bones_of(&scene, Some(f), lead));
            }
            tiles.push(img);
        }
        let cols = frames.len().min(4);
        let sheet = contact_sheet(&tiles, cols);
        let png = super::tools::encode_png(&sheet)?;
        let mut out = text(json!({
            "view": shot.name,
            "frames": frames,
            "layout": format!("{cols} per row, left to right, top to bottom, in frame order"),
            "fps": fps,
            "saved": save.as_ref().map(|p| p.display().to_string()),
        }));
        if let Some(p) = &save {
            let bytes = super::tools::encode_png_bytes(&sheet)?;
            std::fs::write(p, bytes).map_err(|e| format!("could not save {}: {e}", p.display()))?;
        }
        out.extend(png);
        Ok(Box::new(move |_app: &mut ClideApp| Ok(out)) as super::jobs::Finish)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_wgpu::wgpu;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(Wake(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }

    /// A render state without a window (None where there is no GPU).
    fn headless() -> Option<egui_wgpu::RenderState> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let rs = egui_wgpu::RenderState { adapter, available_adapters: Vec::new(), device, queue, target_format: format, renderer: std::sync::Arc::new(egui::mutex::RwLock::new(renderer)) };
        crate::render::init(&rs, crate::mesh::MeshLib::new().prims());
        Some(rs)
    }

    #[test]
    fn render_animation_job_renders_eight_frames() {
        let Some(rs) = headless() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.clay = Some(serde_json::from_value(json!({ "blobs": [{ "type": "capsule", "a": [0, 0, 0], "b": [0, 1, 0], "radius": 0.15 }], "resolution": 24 })).unwrap());
        o.rig = Some(serde_json::from_value(json!({
            "bones": [{ "name": "root", "head": [0, 0, 0], "tail": [0, 0.5, 0] }, { "name": "top", "parent": "root", "head": [0, 0.5, 0], "tail": [0, 1, 0] }],
            "clips": [{ "name": "fire", "tracks": [{ "bone": "top", "keys": [{ "frame": 0 }, { "frame": 12, "rotate": [0, 0, 60] }, { "frame": 24 }] }] }],
            "active": "fire", "split": false,
        })).unwrap());
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.sync(&scene);
        let frames: Vec<f32> = (0..8).map(|i| i as f32 * 3.0).collect();
        let job = AnimJob { frames, size: 64, view: json!("front"), bones: true, center: None, radius: None, save: None, lead: None, fps: 24.0 };
        let progress = crate::progress::Progress::default();
        let finish = animation_job(scene, meshes, rs, job)(&progress);
        assert!(finish.is_ok(), "{:?}", finish.err());
        assert!(progress.fraction() > 0.9);
    }
}
