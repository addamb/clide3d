//! `edit_clay` lids from Eye objects (character follow-ups, feature 2): a
//! blob `{type: "lid", eye: NAME, ...}` in `add`, `replace` or `update` is
//! filled from the eye as it sits at rest in the clay's space (its centre,
//! width, forward and cornea), the clay's cell (the lid's thickness and
//! clearance), the skin colour over the eye and the eye's bone, then
//! stored as an ordinary `Form::Lid` blob (resolved values are stored,
//! the sculpt plan's §3.3).

use glam::Vec3;
use serde_json::{Map, Value, json};

use super::ClideApp;
use crate::clay::Clay;
use crate::lids::{Seat, Spec};
use crate::scene::Shape;

/// The keys an eye lid takes besides the blob's own: the eye, the
/// clearance over its cornea, and `upper_cover` / `lower_cover` (a cover
/// from the top or the bottom).
const EYE_KEYS: [&str; 4] = ["eye", "clearance", "upper_cover", "lower_cover"];

impl ClideApp {
    /// Create > Eyelids on Selected Clay's request: an upper lid (the
    /// defaults) for every Eye object under the selected clay, in name
    /// order; None without a selected clay with eyes.
    pub(super) fn lids_request(&self) -> Option<Value> {
        let clay = self.selection.first().and_then(|id| self.scene.get(*id)).filter(|o| o.shape == Shape::Clay)?;
        let mut eyes: Vec<&str> = self.scene.objects.iter().filter(|e| e.shape == Shape::Eye && e.parent == Some(clay.id)).map(|e| e.name.as_str()).collect();
        eyes.sort_unstable();
        if eyes.is_empty() {
            return None;
        }
        Some(json!({ "target": clay.name, "add": eyes.iter().map(|e| json!({ "type": "lid", "eye": e })).collect::<Vec<_>>() }))
    }

    /// `args` with every lid blob that names an `eye` filled in; notes
    /// say what each became.
    /// Only the lists named in `keys` (`replace` is read against the clay
    /// before it, `update` and `add` after the split step).
    pub(super) fn resolve_lid_eyes(&self, clay_id: u64, clay: &Clay, args: &Value, keys: &[&str]) -> Result<(Value, Vec<String>), String> {
        let mut out = args.clone();
        let mut notes = Vec::new();
        let mut cell: Option<f32> = None;
        for &key in keys {
            let Some(list) = out.get_mut(key).and_then(Value::as_array_mut) else { continue };
            for b in list.iter_mut() {
                let Some(m) = b.as_object_mut() else { continue };
                if !m.contains_key("eye") {
                    if m.keys().any(|k| EYE_KEYS.contains(&k.as_str())) {
                        return Err("clearance, upper_cover and lower_cover go with eye (a lid filled from an Eye object); a lid without one takes cover and lower".into());
                    }
                    continue;
                }
                let is_lid = m.get("type").and_then(Value::as_str) == Some("lid")
                    || (key == "update" && m.get("type").is_none() && m.get("blob").is_some_and(|k| clay.find(k).is_some_and(|i| matches!(clay.blobs[i].form, crate::clay::Form::Lid { .. }))));
                if !is_lid {
                    return Err("eye goes with a lid blob ({type: \"lid\", eye: NAME}) only".into());
                }
                let cell = match cell {
                    Some(c) => c,
                    None => {
                        let c = clay.cell_size().unwrap_or(0.01);
                        cell = Some(c);
                        c
                    }
                };
                // A flipped blob is read at -x: its lid is stored mirrored.
                let existing = (key == "update").then(|| m.get("blob").and_then(|k| clay.find(k))).flatten();
                let flip = m.get("flip").and_then(Value::as_bool).unwrap_or_else(|| existing.is_some_and(|i| clay.blobs[i].flip));
                let note = self.fill_lid(clay_id, clay, cell, m, flip)?;
                notes.push(note);
            }
        }
        Ok((out, notes))
    }

    /// One lid filled from its eye (see `resolve_lid_eyes`).
    fn fill_lid(&self, clay_id: u64, clay: &Clay, cell: f32, m: &mut Map<String, Value>, flip: bool) -> Result<String, String> {
        let eye_id = self.resolve(&m["eye"])?;
        let e = self.scene.get(eye_id).ok_or("eye vanished")?;
        if e.shape != Shape::Eye {
            return Err(format!("lid eye: {} is not an Eye object", e.name));
        }
        let eye_name = e.name.clone();
        let look = e.eye.clone().unwrap_or_default();
        // The eye at rest in the clay's space (its mesh: a ball of diameter 1).
        let to_clay = self.scene.rest_world_matrix(clay_id).inverse() * self.scene.rest_world_matrix(eye_id);
        let (scale, _, _) = to_clay.to_scale_rotation_translation();
        let diameter = (scale.x.abs() + scale.y.abs() + scale.z.abs()) / 3.0;
        if !(diameter.is_finite() && diameter > 0.0) || (scale.abs().max_element() - scale.abs().min_element()) > 1e-3 * diameter {
            return Err(format!("lid eye: {eye_name} must have a uniform scale"));
        }
        let center = to_clay.transform_point3(Vec3::ZERO);
        let forward = to_clay.transform_vector3(Vec3::Z).normalize_or(Vec3::Z);
        let num = |k: &str| -> Result<Option<f32>, String> {
            match m.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => v.as_f64().map(|x| Some(x as f32)).ok_or(format!("lid {k} must be a number")),
            }
        };
        let (upper, lower_cover, cover) = (num("upper_cover")?, num("lower_cover")?, num("cover")?);
        if [upper, lower_cover, cover].iter().filter(|c| c.is_some()).count() > 1 {
            return Err("a lid takes one of cover, upper_cover or lower_cover".into());
        }
        let lower = lower_cover.is_some() || m.get("lower").and_then(Value::as_bool).unwrap_or(false);
        let cover = upper.or(lower_cover).or(cover).unwrap_or(if lower { 0.15 } else { 0.3 });
        if !(0.0..=1.0).contains(&cover) {
            return Err("a lid's cover must be 0 (open) to 1 (closed)".into());
        }
        let up = match m.get("up").filter(|v| !v.is_null()) {
            Some(v) => Vec3::from(serde_json::from_value::<[f32; 3]>(v.clone()).map_err(|_| "lid up must be [x, y, z]")?),
            None => Vec3::Y,
        };
        let clearance = num("clearance")?;
        if clearance.is_some_and(|c| !(c.is_finite() && c >= 0.0 && c <= diameter)) {
            return Err("lid clearance must be 0 to the eye's width".into());
        }
        let thickness = num("thickness")?;
        if thickness.is_some_and(|t| !(t.is_finite() && t > 0.0 && t <= diameter)) {
            return Err("lid thickness must be above 0 and at most the eye's width".into());
        }
        let seat = Seat { center, diameter, forward, up, bulge: look.cornea_bulge() };
        let spec = Spec { cover, lower, tilt: num("tilt")?.unwrap_or(0.0), thickness, clearance };
        let mut form = crate::lids::lid_form(&seat, &spec, cell);
        if flip {
            for k in ["center", "forward", "up"] {
                let x = form[k][0].as_f64().unwrap_or(0.0);
                form[k][0] = json!(-x);
            }
            form["tilt"] = json!(-spec.tilt);
        }
        let t = form["thickness"].as_f64().unwrap_or(0.0) as f32;
        for k in EYE_KEYS.iter().chain(&["cover", "lower"]) {
            m.remove(*k);
        }
        m.insert("lower".into(), json!(lower));
        // An explicit gap wins over the eye's.
        let gap = m.get("gap").cloned();
        for (k, v) in form.as_object().expect("lid form").iter() {
            if k == "gap" && gap.is_some() {
                continue;
            }
            m.insert(k.clone(), v.clone());
        }
        let (blend, color_blend) = crate::lids::default_blend(t, diameter);
        m.entry("blend").or_insert(json!(blend));
        m.entry("color_blend").or_insert(json!(color_blend));
        if !m.contains_key("bone")
            && let Some(b) = e.bone.clone()
        {
            m.insert("bone".into(), json!(b));
        }
        if !m.contains_key("name") && !m.contains_key("blob") {
            m.insert("name".into(), json!(crate::lids::default_name(&eye_name, lower)));
        }
        if !m.contains_key("color") {
            // The skin over the eye: the clay's colour just above the lid.
            let (_, u, _) = crate::clay::lid_frame(forward, up, 0.0);
            let probe = center + u * (0.5 * diameter + seat.bulge * diameter + t);
            let field = crate::clay::Field::new(clay, cell);
            let all: Vec<u32> = (0..field.prim_count() as u32).collect();
            if !all.is_empty() {
                let c = field.sample(probe, &all).1;
                m.insert("color".into(), json!([c.x, c.y, c.z]));
            }
        }
        Ok(format!("{} from {eye_name}: center {:.4?}, radius {:.4}, gap {:.4} (cornea {:.4} + clearance), thickness {t:.4}, cover {cover}{}", m.get("name").or(m.get("blob")).map_or(String::new(), |v| v.to_string()), center.to_array(), 0.5 * diameter, m["gap"].as_f64().unwrap_or(0.0), seat.bulge * diameter, if lower { " from below" } else { "" }))
    }
}
