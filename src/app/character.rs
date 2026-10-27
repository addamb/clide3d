//! The `create_character` tool (docs/QUALITY_PLAN.md Stage 5c): a
//! stylised character base made by `crate::character::generate`, put in
//! the scene the way `create_object`, `rig`, `set_material` and `guides`
//! would put it: an ordinary clay with named blobs, bones inside them,
//! stylised eye objects in carved sockets riding the head bone, and the
//! proportion guides it was made to. One call is one undo step.
//!
//! The editor's Create > Character Base sends the same call
//! (`character_request`).

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::tools::{text, vec3};
use crate::character::{self, Build, Proportions, Spec};
use crate::mcp::ToolResult;
use crate::rig::Rig;
use crate::scene::Shape;

/// The keys the tool takes.
const KEYS: [&str; 11] = ["name", "build", "proportions", "eyes", "rig", "symmetry", "guides", "look", "color", "translate", "resolution"];

/// The default skin colour: a warm, friendly orange.
const SKIN: [f32; 3] = [0.95, 0.62, 0.38];

/// What a call asks for, read and checked before anything changes.
struct Request {
    spec: Spec,
    rig: bool,
    guides: bool,
    /// The skin material to make (`look: stylized`): a name no scene
    /// material has, so an existing one is never reset.
    skin: Option<String>,
}

/// `{name}_skin`, or with the first number from 2 that no scene material
/// has.
fn free_skin(app: &ClideApp, name: &str) -> String {
    let taken = |n: &str| app.scene.materials.iter().any(|m| m.name == n);
    let first = format!("{name}_skin");
    if !taken(&first) {
        return first;
    }
    (2..).map(|k| format!("{name}_skin{k}")).find(|n| !taken(n)).expect("a free name")
}

fn flag(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => v.as_bool().ok_or(format!("{key} must be true or false")),
    }
}

fn request(app: &ClideApp, args: &Value) -> Result<Request, String> {
    let map = args.as_object().ok_or("create_character takes an object")?;
    if let Some(k) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("create_character takes no {k} (it takes {})", KEYS.join(", ")));
    }
    let name = match args.get("name").filter(|v| !v.is_null()) {
        None => "Character".to_string(),
        Some(v) => v.as_str().ok_or("name must be a string")?.to_string(),
    };
    if app.scene.objects.iter().any(|o| o.name == name || o.name == format!("{name}_eye.L") || o.name == format!("{name}_eye.R")) {
        return Err(format!("an object called {name} (or its eyes) is already in the scene: pass another name"));
    }
    let build: Build = match args.get("build").filter(|v| !v.is_null()) {
        None => Build::Biped,
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| "build must be biped or quadruped")?,
    };
    let given = args.get("proportions").filter(|v| !v.is_null());
    if build == Build::Quadruped && given.is_some_and(|p| p.get("hands").is_some()) {
        return Err("hands apply to bipeds; a quadruped stands on four paws (feet)".into());
    }
    let proportions: Proportions = match given {
        None => Proportions::default(),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("proportions: {e}"))?,
    };
    let color = match vec3(args, "color")? {
        None => SKIN,
        Some(c) => c.to_array(),
    };
    let translate = vec3(args, "translate")?.unwrap_or(Vec3::ZERO);
    let resolution = match args.get("resolution").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_u64().filter(|r| *r <= u32::MAX as u64).ok_or("resolution must be a whole number")? as u32),
    };
    let stylized = match args.get("look").filter(|v| !v.is_null()).map(|v| v.as_str()) {
        None | Some(Some("stylized")) => true,
        Some(Some("plain")) => false,
        _ => return Err("look must be stylized or plain".into()),
    };
    // The lids clear the corneas of the eyes that will sit there.
    let mut eye = crate::eyeball::Eye::default();
    if stylized {
        eye.stylize(&|_| false);
    }
    let spec = Spec { name, build, proportions, symmetry: flag(args, "symmetry", true)?, eyes: flag(args, "eyes", true)?, color, translate, resolution, eye_bulge: eye.cornea_bulge() };
    spec.validate()?;
    // Checked here, so a refusal never adds anything (nor an undo step).
    if stylized && app.scene.materials.len() >= crate::material::MAX_MATERIALS {
        return Err(format!("the scene has {} materials, the most it can hold, and look: stylized adds one: pass look: plain, or remove a material", crate::material::MAX_MATERIALS));
    }
    let skin = stylized.then(|| free_skin(app, &spec.name));
    Ok(Request { spec, rig: flag(args, "rig", true)?, guides: flag(args, "guides", true)?, skin })
}

impl ClideApp {
    /// `create_character` (run at rest).
    pub(super) fn create_character(&mut self, args: &Value) -> ToolResult {
        let req = request(self, args)?;
        let built = character::generate(&req.spec)?;
        let blobs = super::tools::parse_blobs(&json!(built.blobs))?;
        let name = req.spec.name.clone();
        let keep_selection = self.selection.clone();
        // A failure puts the whole scene back (ids, materials and guides
        // too), so it leaves no undo step.
        let keep_scene = self.scene.clone();
        let id = self.scene.add(Shape::Clay);
        let made = (|| -> Result<Vec<u64>, String> {
            let o = self.scene.get_mut(id).ok_or("object vanished")?;
            o.name = name.clone();
            o.translation = req.spec.translate;
            let mut clay = crate::clay::Clay { blobs, resolution: built.resolution, ..Default::default() };
            clay.sanitize();
            o.clay = Some(clay);
            if req.rig {
                o.rig = Some(Rig { bones: built.bones.clone(), clips: Vec::new(), active: None, split: false, weights: Vec::new() });
            }
            let mut eyes = Vec::new();
            for e in &built.eyes {
                let eye = self.scene.add(Shape::Eye);
                let o = self.scene.get_mut(eye).ok_or("object vanished")?;
                o.name = e.name.clone();
                o.parent = Some(id);
                o.bone = req.rig.then(|| "head".to_string());
                o.translation = e.center;
                o.scale = Vec3::splat(e.diameter);
                if req.skin.is_some() {
                    self.apply_props(eye, &json!({ "eye": { "style": "stylized" } }))?;
                }
                eyes.push(eye);
            }
            if let Some(skin) = &req.skin {
                self.set_material(&json!({ "name": skin, "preset": "skin_stylized", "subsurface_radius": built.skin_radius, "apply_to": [id] }))?;
            }
            if req.guides {
                self.scene.guides = built.guides.clone();
            }
            Ok(eyes)
        })();
        let eyes = match made {
            Ok(e) => e,
            Err(e) => {
                self.scene = keep_scene;
                self.selection = keep_selection;
                self.prune_selection();
                return Err(e);
            }
        };
        self.select_only(id);
        let spec = req.spec.clone();
        let unit = built.unit;
        let finger = built.finger_radius;
        let measure = req.guides;
        self.defer_reply(move |app| {
            let summary = app.clay_summary(id);
            let mut out = json!({
                "id": id,
                "name": spec.name,
                "build": spec.build,
                "proportions": spec.proportions,
                "symmetry": spec.symmetry,
                "head_unit": crate::guides::round(unit),
                "finger_radius": crate::guides::round(finger),
                "clay": summary,
                "eyes": eyes.iter().filter_map(|e| app.scene.get(*e).map(|o| json!({ "id": e, "name": o.name, "diameter": crate::guides::round(o.scale.x), "at": o.translation.to_array().map(crate::guides::round) }))).collect::<Vec<_>>(),
                "bones": app.scene.get(id).and_then(|o| o.rig.as_ref()).map(|r| r.bones.iter().map(|b| b.name.clone()).collect::<Vec<_>>()),
                "material": app.scene.get(id).and_then(|o| o.material.clone()),
            });
            if measure {
                out["guides"] = json!(app.scene.guides);
                // The base is in the scene by now: a measure that cannot
                // run is reported, not a failed call.
                out["measured"] = match app.measure_guides(id) {
                    Ok(m) => json!({ "pass": m.reply["pass"], "failures": m.reply["failures"], "model": m.reply["model"], "head_units": { "unit": m.reply["head_units"]["unit"], "model_heads": m.reply["head_units"]["model_heads"] }, "targets": m.reply["targets"] }),
                    Err(e) => json!({ "pass": false, "problem": e }),
                };
            }
            out["note"] = json!("an ordinary clay: edit_clay, sculpt, paint and polish work on it; blob names (head, torso, upper_arm.L, finger1.L, ...) address its parts, and mirrored .L blobs make the .R side. The guides replace the scene's; guides measure {target} reads them again after edits.");
            Ok(text(out))
        })
    }

    /// Create > Character Base's request: the default base under a free name.
    pub(super) fn character_request(&self) -> Value {
        let taken = |n: &str| self.scene.objects.iter().any(|o| o.name == n || o.name == format!("{n}_eye.L") || o.name == format!("{n}_eye.R"));
        let name = if taken("Character") { (2..).map(|k| format!("Character{k}")).find(|n| !taken(n)).expect("a free name") } else { "Character".to_string() };
        json!({ "name": name })
    }
}
