//! Agent tools for human bases (see `human.rs`): `create_human` makes a
//! rigged body, a finer head clay on the head bone, and eyes in the sockets;
//! `set_human` changes the controls and moves the eyes and bones to match;
//! `human_controls` lists the shape controls.

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use crate::clay::{Blob, Clay, Form, HumanPart};
use crate::human::Human;
use crate::mcp::ToolResult;
use crate::rig::Rig;
use crate::scene::Shape;

impl ClideApp {
    pub(super) fn create_human(&mut self, args: &Value) -> ToolResult {
        let human = parse_human(Human::default(), args.get("human"))?;
        let human = parse_human(human, skin_patch(args).as_ref())?;
        let body = human.build()?;
        let name = args["name"].as_str().unwrap_or("Human").to_string();
        let skin = color(args, "skin_color", [0.82, 0.64, 0.54])?;
        let blob = |part: HumanPart, name: &str| -> Blob {
            serde_json::from_value(json!({ "type": "human", "name": name, "part": part, "human": human, "color": skin, "blend": 0.0, "material": "skin" })).expect("valid blob")
        };
        let res = |k: &str, d: u64| args[k].as_u64().unwrap_or(d).clamp(16, crate::clay::MAX_RESOLUTION as u64) as u32;

        // One clay for the whole person (head included), or a body and a
        // finer head clay on the head bone.
        let one = args["one_object"].as_bool().unwrap_or(false) || args.get("detail").is_some_and(|d| !d.is_null());
        let body_id = self.scene.add(Shape::Clay);
        let o = self.scene.get_mut(body_id).expect("added");
        o.name = name.clone();
        o.material = Some("skin".into());
        let part = if one { HumanPart::Whole } else { HumanPart::Body };
        let mut clay = Clay { blobs: vec![blob(part, "body")], resolution: res("resolution", 384), ..Clay::default() };
        clay.sanitize();
        let regions = match args.get("detail").filter(|d| !d.is_null()) {
            Some(d) => Some(human_detail(&mut clay, &body, d)?),
            None => None,
        };
        let o = self.scene.get_mut(body_id).expect("added");
        o.clay = Some(clay);
        if args["rig"].as_bool().unwrap_or(true) {
            o.rig = Some(Rig { bones: body.rig(), clips: Vec::new(), active: None, split: false, weights: Vec::new() });
        }

        let rigged = self.scene.get(body_id).is_some_and(|b| b.rig.is_some());
        let head_id = if one { body_id } else { self.scene.add(Shape::Clay) };
        if !one {
        let o = self.scene.get_mut(head_id).expect("added");
        o.name = format!("{name}_head");
        o.material = Some("skin".into());
        o.parent = Some(body_id);
        o.bone = rigged.then(|| "head".to_string());
        let mut clay = Clay { blobs: vec![blob(HumanPart::Head, "head")], resolution: res("head_resolution", 320), ..Clay::default() };
        if let Some(c) = args["head_detail_cell"].as_f64() {
            clay.set_detail_cell(c as f32)?;
        }
        clay.sanitize();
        o.clay = Some(clay);
        }

        let iris = color(args, "eye_color", [0.33, 0.42, 0.36])?;
        let mut eyes = Vec::new();
        for (side, (center, size)) in ["L", "R"].iter().zip(body.eyes()) {
            let id = self.scene.add(Shape::Eye);
            let o = self.scene.get_mut(id).expect("added");
            o.name = format!("{name}_eye.{side}");
            o.parent = Some(head_id);
            if one && rigged {
                o.bone = Some("head".into());
            }
            let (center, seated, lids) = seat_eye(&human, center, size);
            o.translation = center;
            o.scale = Vec3::splat(seated);
            if let Some(e) = o.eye.as_mut() {
                e.iris_color = iris;
                // The same iris width when the ball had to shrink.
                e.iris_size = (crate::human::EYE_IRIS * size / seated).min(0.6);
                e.lids = lids;
            }
            eyes.push(id);
        }
        // Look straight ahead.
        for &id in &eyes {
            let c = self.scene.world_matrix(id).w_axis.truncate();
            self.apply_props(id, &json!({ "aim": [c.x, c.y, c.z + 1.0] }))?;
        }
        if args["face_hair"].as_bool().unwrap_or(false) {
            self.face_hair(&json!({ "target": body_id }))?;
        }
        self.select_only(body_id);
        let height = body.points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        Ok(super::tools::text(json!({
            "body": name,
            "head": if one { Value::Null } else { json!(format!("{name}_head")) },
            "detail_regions": regions,
            "eyes": [format!("{name}_eye.L"), format!("{name}_eye.R")],
            "height": (height * 1000.0).round() / 1000.0,
            "bones": self.scene.get(body_id).and_then(|o| o.rig.as_ref()).map(|r| r.bones.iter().map(|b| b.name.clone()).collect::<Vec<_>>()),
            "next": if one { "One clay holds the whole person; detail regions (see detail_regions) keep a finer sculpt cell on the face and hands. Sculpt pores and creases there, paint, and set materials as usual. Change the human with set_human." } else { "The body and head are clay: add blobs (clothes, hair base), sculpt (detail_cell on the head for pores and lines), paint, and set materials as usual. Change the human with set_human. The head clay rides the head bone." },
        })))
    }

    /// `set_human`: new controls for a human made by `create_human`; the
    /// head, eyes and bones follow.
    pub(super) fn set_human(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        // The body is the target or its parent.
        let body_id = self.human_body(id)?;
        let current = body_human(self.scene.get(body_id).and_then(|o| o.clay.as_ref())).cloned().ok_or("target is not a human made by create_human")?;
        let human = parse_human(current, args.get("human"))?;
        let human = parse_human(human, skin_patch(args).as_ref())?;
        let body = human.build()?;
        let mut touched = Vec::new();
        for oid in std::iter::once(body_id).chain(self.scene.descendants(body_id)) {
            let Some(o) = self.scene.get_mut(oid) else { continue };
            let mut changed = false;
            for b in o.clay.iter_mut().flat_map(|c| c.blobs.iter_mut()) {
                if let Form::Human { human: h, .. } = &mut b.form {
                    *h = human.clone();
                    changed = true;
                }
            }
            if changed {
                touched.push(o.name.clone());
            }
        }
        // Bones keep their names (and so the clips); only positions move.
        if let Some(rig) = self.scene.get_mut(body_id).and_then(|o| o.rig.as_mut()) {
            let fresh = body.rig();
            for b in &mut rig.bones {
                if let Some(f) = fresh.iter().find(|f| f.name == b.name) {
                    (b.head, b.tail) = (f.head, f.tail);
                }
            }
        }
        let name = self.scene.get(body_id).map(|o| o.name.clone()).unwrap_or_default();
        for (side, (center, size)) in ["L", "R"].iter().zip(body.eyes()) {
            if let Some(eid) = self.scene.objects.iter().find(|o| o.name == format!("{name}_eye.{side}")).map(|o| o.id) {
                let o = self.scene.get_mut(eid).expect("found");
                // Eyes are seated under the lids (older scenes get lids too).
                let (center, fitted, lids) = seat_eye(&human, center, size);
                o.translation = center;
                o.scale = Vec3::splat(fitted);
                if let Some(e) = o.eye.as_mut() {
                    e.lids = lids;
                    e.iris_size = (crate::human::EYE_IRIS * size / fitted).min(0.6);
                }
            }
        }
        Ok(super::tools::text(json!({ "updated": touched, "human": human, "skin": human.skin })))
    }

    /// `face_hair`: brows, lashes and peach fuzz for a human, grown from its
    /// face layout and lid margins (see `face_hair.rs`). Each is a Hair
    /// object on the head (NAME_brows, NAME_lashes, NAME_fuzz), replaced
    /// when grown again; `false` removes one.
    pub(super) fn face_hair(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        let body_id = self.human_body(id)?;
        let name = self.scene.get(body_id).map(|o| o.name.clone()).unwrap_or_default();
        let find = |app: &Self, n: &str| app.scene.objects.iter().find(|o| o.name == n).map(|o| o.id);
        let (head_id, human) = self.human_head(body_id)?;
        let landmarks = crate::human::shape(&human)?.landmarks.ok_or("this human has no face landmarks")?;
        let face = crate::face::Face::new(&landmarks, &human.skin, human.age, human.sex);
        let eye = find(self, &format!("{name}_eye.L")).and_then(|e| self.scene.get(e)).and_then(|o| Some((o.eye.as_ref()?.lids.clone()?, o.translation)));
        self.meshes.sync(&self.scene);
        let seed = args["seed"].as_u64().unwrap_or(1) as u32;
        // Each part: true / {density, color} / false.
        let part = |key: &str, color: [f32; 3]| -> Result<Option<(f32, [f32; 3])>, String> {
            match args.get(key) {
                None | Some(Value::Null) | Some(Value::Bool(true)) => Ok(Some((1.0, color))),
                Some(Value::Bool(false)) => Ok(None),
                Some(Value::Object(o)) => {
                    let density = o.get("density").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                    let c = match o.get("color") {
                        Some(v) => serde_json::from_value(v.clone()).map_err(|_| format!("{key}.color must be [r, g, b]"))?,
                        None => color,
                    };
                    Ok(Some((density.clamp(0.0, 2.0), c)))
                }
                _ => Err(format!("{key} must be true, false or {{density, color}}")),
            }
        };
        let brows = part("brows", [0.13, 0.085, 0.055])?;
        let lashes = part("lashes", [0.035, 0.028, 0.024])?;
        let fuzz = part("fuzz", [0.78, 0.68, 0.56])?;
        let mut made = Vec::new();
        let mut hairs = Vec::new();
        {
            let snap = self.snapper(head_id)?;
            if let Some((d, c)) = brows {
                hairs.push(("brows", Some(crate::face_hair::brows(&face, &snap, c, d, seed))));
            } else {
                hairs.push(("brows", None));
            }
            match (lashes, &eye) {
                (Some((d, c)), Some((lids, center))) => hairs.push(("lashes", Some(crate::face_hair::lashes(&face, lids, *center, c, d, seed)))),
                (Some(_), None) => return Err("lashes need the human's eyes with lids (made by create_human)".into()),
                (None, _) => hairs.push(("lashes", None)),
            }
            if let Some((d, c)) = fuzz {
                hairs.push(("fuzz", Some(crate::face_hair::fuzz(&face, &snap, c, d, seed))));
            } else {
                hairs.push(("fuzz", None));
            }
        }
        for (what, hair) in hairs {
            let object = format!("{name}_{what}");
            if let Some(old) = find(self, &object) {
                self.scene.remove_tree(old);
            }
            let Some(mut hair) = hair else { continue };
            hair.sanitize();
            let hid = self.scene.add(Shape::Hair);
            let o = self.scene.get_mut(hid).expect("added");
            o.name = object.clone();
            o.parent = Some(head_id);
            o.material = Some("hair".into());
            made.push(json!({ "object": object, "guides": hair.guides.len() * 2, "strands": hair.strands }));
            o.hair = Some(hair);
        }
        Ok(super::tools::text(json!({ "made": made, "note": "Film renders these as strands (render_film device auto picks the CPU with strands; device gpu works too). Exports get thin cards." })))
    }

    /// `face_detail`: sculpts the broad part of the face's folds and
    /// wrinkles (nasolabial folds, forehead furrows, crow's feet, under-eye
    /// creases, marionette lines, as aged by `skin`) into the head clay as
    /// real geometry, on a fine detail grid (`detail_cell`, default 0.25 mm).
    /// Finer detail (pores, crease lines, micro lines) stays in shading.
    pub(super) fn face_detail(&mut self, args: &Value) -> Result<super::jobs::Started, String> {
        let id = self.resolve(&args["target"])?;
        let body_id = self.human_body(id)?;
        let (head_id, human) = self.human_head(body_id)?;
        let head_name = self.scene.get(head_id).map(|o| o.name.clone()).unwrap_or_default();
        let before = self.scene.get(head_id).and_then(|o| o.clay.clone()).ok_or("the head is not clay")?;
        let shape = crate::human::shape(&human)?;
        let face = crate::face::Face::new(&shape.landmarks.ok_or("this human has no face landmarks")?, &human.skin, human.age, human.sex);
        let strength = args["strength"].as_f64().unwrap_or(1.0).clamp(0.0, 3.0) as f32;
        let cell = args["detail_cell"].as_f64().unwrap_or(0.00025) as f32;
        let mut clay = before.clone();
        let note = if clay.detail.is_none() {
            clay.set_detail_cell(cell)?;
            format!("detail_cell {cell}")
        } else {
            format!("the head already has sculpt detail at {:.5}; folds use that grid", clay.detail.as_ref().map_or(0.0, |d| d.voxel()))
        };
        let (lo, hi) = (shape.head_lo, shape.head_hi);
        let depth = face.lines.iter().map(|l| l.depth).fold(0.0f32, f32::max) * 2.0 * strength + 1e-5;
        let label = format!("Face detail on {head_name}");
        Ok(super::jobs::Started::Job(label, Box::new(move |progress| {
            progress.stage("sculpting folds", 0.1);
            let report = crate::sculpt::displace(&mut clay, lo, hi, depth, &|c, side| face.near_folds(c, side), &|p| {
                let h = face.fold_height(p) * strength;
                // Below a micron is no shape.
                if h.abs() < 1e-6 { 0.0 } else { h }
            })?;
            Ok(Box::new(move |app: &mut ClideApp| {
                let o = app.scene.get_mut(head_id).ok_or("the head was deleted while sculpting")?;
                if o.clay.as_ref() != Some(&before) {
                    return Err("the head changed while sculpting; nothing was applied, run face_detail again".into());
                }
                let bricks = clay.detail.as_ref().map(|d| d.grid().brick_count());
                o.clay = Some(clay);
                Ok(super::tools::text(json!({
                    "target": head_name,
                    "changed_points": report.changed_points,
                    "detail_bricks": bricks,
                    "note": note,
                    "next": "The folds are sculpted (running it again deepens them). render_film shows them on the fine mesh; bake carries them into the normal map.",
                })))
            }) as super::jobs::Finish)
        })))
    }

    pub(super) fn human_controls(&mut self, args: &Value) -> ToolResult {
        let filter = args["filter"].as_str().unwrap_or("");
        let controls: Vec<Value> = crate::human::asset()
            .controls()
            .into_iter()
            .filter(|(n, _)| n.contains(filter))
            .map(|(n, k)| json!({ "name": n, "range": k }))
            .collect();
        Ok(super::tools::text(json!({
            "macro": "sex (0 female .. 1 male), age (18..90 years), weight, muscle, height, proportions (0..1, 0.5 average), african / asian / caucasian (ancestry mix)",
            "shapes": controls,
            "use": "human.shapes {name: value}; names without l-/r- set both sides.",
        })))
    }
}

impl ClideApp {
    /// The body of a human made by `create_human`: `id` itself (a body or
    /// one-clay person) or its parent (the head, an eye).
    fn human_body(&self, id: u64) -> Result<u64, String> {
        let is_body = |i: u64| body_human(self.scene.get(i).and_then(|o| o.clay.as_ref())).is_some();
        if is_body(id) {
            return Ok(id);
        }
        self.scene.get(id).and_then(|o| o.parent).filter(|&p| is_body(p)).ok_or_else(|| "target is not a human made by create_human".to_string())
    }

    /// The clay holding a human's face and its human: the head clay
    /// (NAME_head), or the body itself for a one-clay person.
    fn human_head(&self, body_id: u64) -> Result<(u64, Human), String> {
        let name = self.scene.get(body_id).map(|o| o.name.clone()).unwrap_or_default();
        if let Some(h) = self.scene.objects.iter().find(|o| o.name == format!("{name}_head"))
            && let Some(human) = human_blob(h.clay.as_ref(), HumanPart::Head)
        {
            return Ok((h.id, human.clone()));
        }
        human_blob(self.scene.get(body_id).and_then(|o| o.clay.as_ref()), HumanPart::Whole).map(|h| (body_id, h.clone())).ok_or_else(|| "the human has no head clay (NAME_head)".to_string())
    }
}

/// The human of a body clay: its body part, or the whole person.
fn body_human(clay: Option<&Clay>) -> Option<&Human> {
    human_blob(clay, HumanPart::Body).or_else(|| human_blob(clay, HumanPart::Whole))
}

/// The human of a clay's blob of this part.
fn human_blob(clay: Option<&Clay>, part: HumanPart) -> Option<&Human> {
    clay?.blobs.iter().find_map(|b| match &b.form {
        Form::Human { human, part: p } if *p == part => Some(human),
        _ => None,
    })
}

/// Merges human controls from JSON into `base` (shapes merge by name; a
/// shape set to 0 is removed).
fn parse_human(base: Human, patch: Option<&Value>) -> Result<Human, String> {
    let Some(Value::Object(patch)) = patch.filter(|v| !v.is_null()) else { return Ok(base) };
    let mut merged = serde_json::to_value(&base).map_err(|e| e.to_string())?;
    for (k, v) in patch {
        if k == "skin" {
            let Value::Object(fields) = v else { return Err("skin must be an object, e.g. {age: 35, pores: 1, wrinkles: 0.6}".into()) };
            let mut skin = serde_json::to_value(&base.skin).map_err(|e| e.to_string())?;
            for (name, val) in fields {
                skin[name] = val.clone();
            }
            merged["skin"] = skin;
        } else if k == "shapes" {
            let Value::Object(shapes) = v else { return Err("human.shapes must be an object {name: value}".into()) };
            for (name, val) in shapes {
                merged["shapes"][name] = val.clone();
            }
        } else if merged.get(k).is_some() {
            merged[k] = v.clone();
        } else {
            return Err(format!("unknown human control {k}; see human_controls"));
        }
    }
    let mut h: Human = serde_json::from_value(merged).map_err(|e| format!("bad human controls: {e}"))?;
    h.shapes.retain(|_, v| *v != 0.0);
    h.sanitize();
    h.weights()?;
    Ok(h)
}

/// Seats an eyeball of diameter `size` (looking +Z, center near
/// `center`) under a human's lids, and finds where they touch it.
///
/// The lid margins are the edge of the eye opening: for each direction
/// around the eye, the steepest ray from the eye center that still leaves
/// through the opening grazes the margin. The ball moves forward or back so
/// the margins rest on it (no dark gap between lid and eye, as real lids lie
/// on the eye), never so far that the front of the ball comes through the
/// lids. Returns the new center and the margins: upper and lower, each from
/// the inner corner (toward the nose) out.
pub(crate) fn seat_eye(human: &Human, center: Vec3, size: f32) -> (Vec3, f32, Option<crate::eyeball::Lids>) {
    use std::f32::consts::PI;
    let Ok(shape) = crate::human::shape(human) else { return (center, size, None) };
    // The clay skin stands 0.6 mm off the human mesh.
    let skin = |q: Vec3| shape.sdf.distance(q) - 6e-4;
    let dir = |t: f32, p: f32| Vec3::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos());
    // Where a ray from `o` meets skin between `reach.0` and `reach.1` out
    // (sphere tracing).
    let hit = |o: Vec3, d: Vec3, reach: (f32, f32)| -> Option<Vec3> {
        let mut s = reach.0;
        while s < reach.1 {
            let g = skin(o + d * s);
            if g < 1e-4 {
                return Some(o + d * s);
            }
            s += g.max(1e-4);
        }
        None
    };
    let inner = if center.x > 0.0 { PI } else { 0.0 };
    let toward = if center.x > 0.0 { -1.0 } else { 1.0 };
    let n = 24;
    // The margins around a ball of radius `r` at `c`: only skin close
    // around the ball counts (not a brow or a hooded fold further out).
    let margins = |c: Vec3, r: f32| -> (Vec<Vec3>, Vec<Vec3>) {
        let reach = (r * 0.8, r * 1.4);
        // The opening's middle: the mean of the open directions in front
        // (heavy lids can hang over the front of the eye).
        let mut open = Vec3::ZERO;
        for i in 0..10 {
            for k in 0..24 {
                let d = dir(0.9 * i as f32 / 9.0, std::f32::consts::TAU * k as f32 / 24.0);
                if hit(c, d, reach).is_none() {
                    open += d;
                }
            }
        }
        let axis = open.normalize_or(Vec3::Z);
        let side = (Vec3::X - axis * axis.x).normalize_or(Vec3::X);
        let up = axis.cross(side);
        let about = |t: f32, p: f32| axis * t.cos() + (side * p.cos() + up * p.sin()) * t.sin();
        let margin = |p: f32| -> Option<Vec3> {
            let (mut a, mut b) = (0.02f32, 1.7f32);
            if hit(c, about(a, p), reach).is_some() || hit(c, about(b, p), reach).is_none() {
                return None;
            }
            for _ in 0..16 {
                let m = 0.5 * (a + b);
                if hit(c, about(m, p), reach).is_some() { b = m } else { a = m }
            }
            hit(c, about(b, p), reach)
        };
        let trace = |side: f32| -> Vec<Vec3> { (0..=n).filter_map(|i| margin(inner + side * toward * PI * i as f32 / n as f32)).collect() };
        (trace(1.0), trace(-1.0))
    };
    // Some faces (heavy old lids) cover the front of a full-size ball:
    // sink it and make it a little smaller until the lids open over it.
    let mut found = None;
    'search: for back in [0.0f32, 0.1, 0.2] {
        for scale in [1.0f32, 0.94, 0.88, 0.82, 0.76, 0.7] {
            let r = size * 0.5 * scale;
            let c = center - Vec3::Z * back * r;
            let (upper, lower) = margins(c, r);
            if upper.len() > n / 2 && lower.len() > n / 2 {
                found = Some((c, r, upper, lower, back == 0.0));
                break 'search;
            }
        }
    }
    let Some((center, r, upper, lower, free)) = found else { return (center, size, None) };
    // Forward until the margins lie on the ball (median distance = r plus
    // a tear film), but keep the front of the ball out of the lid skin.
    let all: Vec<Vec3> = upper.iter().chain(lower.iter()).copied().collect();
    let gap = |dz: f32| {
        let c = center + Vec3::Z * dz;
        let mut d: Vec<f32> = all.iter().map(|m| m.distance(c) - r).collect();
        d.sort_by(f32::total_cmp);
        d[d.len() / 2]
    };
    let (mut lo, mut hi) = (-r * 0.3, r * 0.6);
    for _ in 0..30 {
        let m = 0.5 * (lo + hi);
        if gap(m) > 2e-4 { lo = m } else { hi = m }
    }
    let mut dz = if free { lo } else { lo.min(0.0) };
    // The ball must not poke through closed skin (a lid, the brow).
    let pokes = |dz: f32| {
        let c = center + Vec3::Z * dz;
        (0..12).flat_map(|i| (0..24).map(move |k| (0.35 + 1.1 * i as f32 / 11.0, std::f32::consts::TAU * k as f32 / 24.0))).filter(|&(t, p)| skin(c + dir(t, p) * r) < -2e-4).count()
    };
    let allowed = pokes(0.0);
    while dz > 0.0 && pokes(dz) > allowed + 4 {
        dz -= r * 0.01;
    }
    let c = center + Vec3::Z * dz;
    // The margins themselves (the lid edge; the ball's contact is along
    // the same directions from its center).
    let onto = |v: &Vec<Vec3>| v.iter().map(|m| m.to_array()).collect();
    (c, r * 2.0, Some(crate::eyeball::Lids { upper: onto(&upper), lower: onto(&lower) }))
}

/// A top-level `skin` argument as a human patch.
fn skin_patch(args: &Value) -> Option<Value> {
    args.get("skin").filter(|v| !v.is_null()).map(|v| json!({ "skin": v }))
}

fn color(args: &Value, key: &str, default: [f32; 3]) -> Result<[f32; 3], String> {
    match args.get(key).filter(|v| !v.is_null()) {
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| format!("{key} must be [r, g, b]")),
        None => Ok(default),
    }
}

/// `create_human detail`: {cell (everywhere, default 0.001), face, hands
/// (cells there, default 0.00025; 0 or null = none)}. Sets the body's
/// detail_cell and adds face and hand regions placed from the skeleton.
fn human_detail(clay: &mut Clay, body: &crate::human::Body, d: &Value) -> Result<Value, String> {
    let get = |k: &str, default: f64| match d.get(k) {
        None => Ok(Some(default as f32)),
        Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(|c| (c > 0.0).then_some(c as f32)).ok_or_else(|| format!("detail.{k} must be a cell size")),
    };
    let cell = get("cell", 0.001)?.ok_or("detail.cell is required")?;
    clay.set_detail_cell(cell)?;
    let regions = body.detail_regions(get("face", 0.00025)?, get("hands", 0.00025)?);
    clay.set_detail_regions(&regions)?;
    clay.fine_check()?;
    Ok(json!(regions.iter().map(|r| json!({ "name": r.name, "center": r.center.to_array(), "radii": r.radii.to_array(), "cell": r.cell })).collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyes_seat_under_the_lids() {
        for human in [Human { sex: 0.0, ..Human::default() }, Human { sex: 1.0, age: 78.0, ..Human::default() }, Human { sex: 1.0, age: 45.0, ..Human::default() }] {
        let body = human.build().unwrap();
        let shape = crate::human::shape(&human).unwrap();
        for (center, size) in body.eyes() {
            eprintln!("center {center} inside {}", shape.sdf.distance(center));
            let (c, _, lids) = seat_eye(&human, center, size);
            eprintln!("seated {c} lids {:?}", lids.as_ref().map(|l| (l.upper.len(), l.lower.len())));
            let lids = lids.expect("lids");
            assert!(c.z >= center.z - size * 0.3);
            // Upper margin above the lower, inner corner toward the nose.
            let mid = |v: &Vec<[f32; 3]>| Vec3::from(v[v.len() / 2]);
            eprintln!("opening {}", mid(&lids.upper).y - mid(&lids.lower).y);
            assert!(mid(&lids.upper).y > mid(&lids.lower).y + size * 0.05);
            assert!(Vec3::from(lids.upper[0]).x.abs() < Vec3::from(*lids.upper.last().unwrap()).x.abs());
        }
        }
    }
}
