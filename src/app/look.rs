//! Agent tools for how surfaces look: the scene's material library
//! (`set_material`), object materials and eye settings (through
//! `create_object` / `update_object`), aiming an object (`aim`), and
//! mirrored copies (`mirror` on `create_object`).

use glam::{Mat3, Quat, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use crate::eyeball::Eye;
use crate::hair::{Guide, Hair};
use crate::material::{MAX_MATERIALS, Material, PRESETS};
use crate::mcp::ToolResult;
use crate::scene::{Scene, Shape};
pub(super) use crate::groom::head::{Head as Collider, InHair};

/// Objects one `set_material apply_to` takes.
const MAX_APPLY_TO: usize = 64;

fn material_edit_summary(name: &str, args: &Value, layers: usize, users: usize, materials: usize) -> Value {
    let fields: Vec<_> = args.as_object().into_iter().flat_map(|o|o.keys())
        .filter(|k|k.as_str() != "name").collect();
    json!({"name":name,"changed":fields,"layers":layers,"users":users,"materials":materials})
}

impl ClideApp {
    /// `set_material`: define, change or remove a scene material. Edits
    /// return a compact summary; read-only calls list the library and presets.
    pub(super) fn set_material(&mut self, args: &Value) -> ToolResult {
        // apply_to only goes with setting a named material: refused (not
        // ignored) with a listing, a rename, a replace or delete_unused.
        if args.get("apply_to").is_some_and(|v| !v.is_null()) {
            let manages = args["delete_unused"].as_bool() == Some(true) || ["rename", "replace", "replace_with"].iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null()));
            if manages || args["list"].as_bool() == Some(true) || args["name"].is_null() {
                return Err("apply_to needs a material name to set (not a listing, rename, replace or delete_unused)".into());
            }
        }
        if let Some(result) = crate::material_management::manage(&mut self.scene, args)? { return Ok(super::tools::text(result)); }
        if args["list"].as_bool()==Some(true) || args["name"].is_null() {
            return Ok(super::tools::text(crate::material_management::listing(&self.scene,args)));
        }
        let mut changed = None;
        // apply_to: objects that take the material in the same call (the
        // editor's Stylised buttons send it), checked before anything
        // changes.
        let apply_to: Vec<u64> = match args.get("apply_to") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(list)) => {
                if list.len() > MAX_APPLY_TO {
                    return Err(format!("apply_to takes at most {MAX_APPLY_TO} objects ({} sent)", list.len()));
                }
                list.iter().map(|v| self.resolve(v)).collect::<Result<_, _>>()?
            }
            Some(v) => vec![self.resolve(v)?],
        };
        if !apply_to.is_empty() && (args["remove"].as_bool().unwrap_or(false) || args["name"].as_str().is_none()) {
            return Err("apply_to needs a material name to set (not remove)".into());
        }
        if let Some(name) = args["name"].as_str() {
            let name = name.trim();
            if name.is_empty() {
                return Err("name must not be empty".into());
            }
            if args["remove"].as_bool().unwrap_or(false) {
                let before = self.scene.materials.len();
                self.scene.materials.retain(|m| m.name != name);
                if self.scene.materials.len() == before {
                    return Err(format!("the scene has no material {name} (presets cannot be removed)"));
                }
            } else {
                // preset: start again from a built-in preset's values
                // (Stage 4d's stylised looks, or any other), even when the
                // scene already has a material of this name.
                let preset = match args.get("preset") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(p)) => Some(p.trim()),
                    Some(_) => return Err("preset must be a preset name".into()),
                };
                let start = match (preset, args["based_on"].as_str()) {
                    (Some(_), Some(_)) => return Err("send preset or based_on, not both".into()),
                    (Some(p), None) => Material::preset(p).ok_or_else(|| format!("unknown preset {p}; use one of {}, or a smart preset ({})", PRESETS.iter().map(|p| p.0).collect::<Vec<_>>().join(", "), smart_names()))?,
                    (None, Some(b)) => Material::find(&self.scene.materials, b).ok_or_else(|| format!("unknown material {b} in based_on"))?,
                    (None, None) => Material::find(&self.scene.materials, name).unwrap_or_default(),
                };
                let mut m = Material { name: name.to_string(), ..start };
                for (key, field) in [("subsurface_radius", &mut m.subsurface_radius), ("roughness", &mut m.roughness), ("metallic", &mut m.metallic), ("clearcoat", &mut m.clearcoat), ("sheen", &mut m.sheen), ("subsurface", &mut m.subsurface), ("pores", &mut m.pores), ("mottling", &mut m.mottling), ("skin_specular", &mut m.skin_specular), ("emission", &mut m.emission), ("transmission", &mut m.transmission), ("ior", &mut m.ior), ("cavity", &mut m.cavity), ("edge_wear", &mut m.edge_wear), ("dirt", &mut m.dirt), ("variation", &mut m.variation), ("gloss_variation", &mut m.gloss_variation), ("variation_scale", &mut m.variation_scale)] {
                    if let Some(v) = args[key].as_f64() {
                        *field = v as f32;
                    }
                }
                if let Some(v) = args.get("base_color") {
                    m.base_color = if v.is_null() { None } else {
                        Some(serde_json::from_value(v.clone()).map_err(|_| "base_color must be [r,g,b] or null")?)
                    };
                }
                if let Some(v) = args.get("keep_paint") {
                    m.keep_paint = v.as_bool().ok_or("keep_paint must be boolean")?;
                }
                if let Some(v) = args.get("dirt_color").filter(|v| !v.is_null()) {
                    m.dirt_color = serde_json::from_value(v.clone()).map_err(|_| "dirt_color must be [r, g, b]")?;
                }
                if let Some(v) = args.get("detail") {
                    m.detail = match v {
                        Value::Null => Some(String::new()),
                        Value::String(d) if d.is_empty() || d == "none" || crate::detail::entry(d).is_some() => Some(d.clone()),
                        _ => return Err(format!("unknown detail {v}; use one of {} or none", crate::detail::names().join(", "))),
                    };
                }
                for (key, field) in [("detail_scale", &mut m.detail_scale), ("detail_strength", &mut m.detail_strength), ("detail_color", &mut m.detail_color), ("detail_rotate", &mut m.detail_rotate), ("detail_displace", &mut m.detail_displace)] {
                    if let Some(v) = args[key].as_f64() {
                        *field = v as f32;
                    }
                }
                if let Some(v) = args.get("scatter").filter(|v| !v.is_null()) {
                    m.scatter = serde_json::from_value(v.clone()).map_err(|_| "scatter must be [red, green, blue] distances")?;
                }
                for (key, field) in [("wear", &mut m.wear), ("rust", &mut m.rust)] {
                    match args.get(key) {
                        Some(Value::Null) => *field = None,
                        Some(v) => *field = Some(v.as_f64().ok_or(format!("{key} must be a number 0..1"))? as f32),
                        None => {}
                    }
                }
                let layers = |key: &str| -> Result<Option<Vec<crate::smart::Layer>>, String> {
                    match args.get(key).filter(|v| !v.is_null()) {
                        None => Ok(None),
                        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|e| format!("bad {key}: {e}")),
                    }
                };
                if let Some(l) = layers("layers")? {
                    m.layers = l;
                }
                if let Some(l) = layers("add_layers")? {
                    m.layers.extend(l);
                }
                for l in &m.layers {
                    if let Some(name) = &l.smart
                        && crate::smart::preset(name).is_none()
                    {
                        return Err(format!("unknown smart preset {name}; use one of {}", smart_names()));
                    }
                    for name in l.detail.iter().chain(l.mask.iter().filter(|t| t.source == crate::smart::Source::Grunge && !t.map.is_empty()).map(|t| &t.map)) {
                        if !name.is_empty() && name != "none" && crate::detail::entry(name).is_none() {
                            return Err(format!("unknown detail map {name}; use one of {}", crate::detail::names().join(", ")));
                        }
                    }
                }
                m.layers = crate::smart::expand(&m.layers, 0);
                m.sanitize();
                let full = self.scene.materials.len() >= MAX_MATERIALS;
                match self.scene.materials.iter_mut().find(|x| x.name == m.name) {
                    Some(x) => *x = m.clone(),
                    None if full => return Err(format!("at most {MAX_MATERIALS} scene materials")),
                    None => self.scene.materials.push(m.clone()),
                }
                for &id in &apply_to {
                    self.scene.get_mut(id).ok_or("object vanished")?.material = Some(m.name.clone());
                }
                changed = Some(m);
            }
        }
        let users = |name: &str| -> Vec<&str> {
            let scene = &self.scene;
            scene
                .objects
                .iter()
                .filter(|o| o.material.as_deref() == Some(name) || o.clay.as_ref().is_some_and(|c| c.blobs.iter().any(|b| b.material == name)))
                .map(|o| o.name.as_str())
                .collect()
        };
        // Edits answer with fields and counts; a call without name lists everything.
        if let Some(name) = args["name"].as_str() {
            let mut summary = material_edit_summary(name, args, changed.as_ref().map_or(0, |m|m.layers.len()), users(name).len(), self.scene.materials.len());
            if !apply_to.is_empty() {
                summary["applied_to"] = json!(apply_to.iter().filter_map(|id| self.scene.get(*id).map(|o| o.name.clone())).collect::<Vec<_>>());
            }
            // A preset's look, as stored (overrides applied).
            if let (Some(m), Some(p)) = (&changed, args["preset"].as_str()) {
                summary["preset"] = json!(p.trim());
                summary["values"] = json!({ "roughness": m.roughness, "sheen": m.sheen, "clearcoat": m.clearcoat, "subsurface": m.subsurface, "subsurface_radius": m.subsurface_radius, "scatter": m.scatter, "pores": m.pores, "mottling": m.mottling, "skin_specular": m.skin_specular, "detail": m.detail_name() });
            }
            return Ok(super::tools::text(summary));
        }
        Ok(super::tools::text(crate::material_management::listing(&self.scene,args)))
    }

    /// The `material`, `eye` and `aim` properties of create/update_object.
    pub(super) fn apply_look(&mut self, id: u64, args: &Value) -> Result<(), String> {
        match args.get("material") {
            None => {}
            Some(Value::Null) => self.scene.get_mut(id).ok_or("object vanished")?.material = None,
            Some(v) => {
                let name = v.as_str().ok_or("material must be a material name or null")?.trim();
                if Material::find(&self.scene.materials, name).is_none() {
                    return Err(unknown_material(&self.scene, name));
                }
                self.scene.get_mut(id).ok_or("object vanished")?.material = Some(name.to_string());
            }
        }
        if let Some(patch) = args.get("eye").filter(|v| !v.is_null()) {
            let o = self.scene.get_mut(id).ok_or("object vanished")?;
            if o.shape != Shape::Eye {
                return Err(format!("{} is not an Eye; eye settings need shape Eye", o.name));
            }
            let Value::Object(patch) = patch else { return Err("eye must be an object".into()) };
            const EYE_KEYS: [&str; 12] = ["iris_color", "iris_size", "pupil_size", "pupil", "sclera_color", "pupil_color", "cornea", "veins", "limbal_ring", "style", "iris_scale", "catchlight"];
            if let Some(k) = patch.keys().find(|k| !EYE_KEYS.contains(&k.as_str())) {
                return Err(format!("unknown eye setting {k}; use {}", EYE_KEYS.join(", ")));
            }
            if patch.contains_key("pupil") && patch.contains_key("pupil_size") {
                return Err("send pupil or pupil_size, not both (pupil is pupil_size)".into());
            }
            let style = match patch.get("style") {
                None | Some(Value::Null) => None,
                Some(v) => Some(serde_json::from_value::<crate::eyeball::EyeStyle>(v.clone()).map_err(|_| "eye style must be realistic or stylized")?),
            };
            if let Some(v) = patch.get("iris_scale").filter(|v| !v.is_null()) {
                let (lo, hi) = crate::eyeball::IRIS_SCALE;
                v.as_f64().filter(|x| (lo as f64..=hi as f64).contains(x)).ok_or(format!("iris_scale must be {lo} to {hi}"))?;
            }
            if let Some(v) = patch.get("pupil").filter(|v| !v.is_null()) {
                v.as_f64().filter(|x| (0.05..=0.95).contains(x)).ok_or("pupil must be 0.05 to 0.95 (a part of the iris width, as pupil_size)")?;
            }
            let before = o.eye.clone().unwrap_or_default();
            // Every field, defaults too, so a patch can name any of them.
            let mut merged = serde_json::to_value(&before).map_err(|e| e.to_string())?;
            merged["style"] = json!(before.style);
            merged["iris_scale"] = json!(before.iris_scale);
            merged["catchlight"] = json!(before.catchlight);
            for (k, v) in patch {
                merged[if k == "pupil" { "pupil_size" } else { k.as_str() }] = v.clone();
            }
            let mut eye: Eye = serde_json::from_value(merged).map_err(|e| format!("bad eye settings: {e}"))?;
            // Turning stylised fills in its look (stored), except what the
            // call sets itself.
            if style == Some(crate::eyeball::EyeStyle::Stylized) && before.style != crate::eyeball::EyeStyle::Stylized {
                eye.stylize(&|k| patch.contains_key(k));
            }
            eye.sanitize();
            o.eye = Some(eye);
        }
        if let Some(patch) = args.get("hair").filter(|v| !v.is_null()) {
            self.apply_hair(id, patch, args["space"].as_str() == Some("world"))?;
        }
        if let Some(v) = args.get("aim").filter(|v| !v.is_null()) {
            let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| "aim must be a world point [x, y, z]")?;
            self.aim(id, Vec3::from(a))?;
        }
        Ok(())
    }

    /// Merges hair settings: `guides` replaces the list, `add_guides`
    /// appends; a guide is {points, width} or just a list of points. In
    /// world space, guide points and center are converted to object space.
    fn apply_hair(&mut self, id: u64, patch: &Value, world: bool) -> Result<(), String> {
        validate_hair_keys(patch)?;
        // `grow: {fur: true}` spreads short guides over a whole clay body
        // (`app/fur.rs`); they replace the list.
        let fur_grown = match patch.get("grow").filter(|g| g.get("fur").and_then(Value::as_bool) == Some(true)) {
            Some(g) => {
                if g.get("style").is_some() {
                    return Err("grow takes fur or style, not both".into());
                }
                Some(self.grow_fur(id, g)?)
            }
            None => None,
        };
        // `grow` makes guides over a scalp; they join (or replace) the list.
        let style = patch["grow"]["style"].as_str().unwrap_or("drape");
        let facial = matches!(style, "brow" | "lash");
        let grown = match patch.get("grow").filter(|v| !v.is_null() && fur_grown.is_none()) {
            Some(g) => Some(self.grow_guides(id, g)?),
            None => None,
        };
        let to_local = self.scene.world_matrix(id).inverse();
        let hair_scale = self.scene.world_matrix(id).to_scale_rotation_translation().0.abs().max_element().max(1e-9);
        let grown_lift = grown.is_some().then(|| patch["grow"]["lift"].as_f64().map_or(if facial { 0.0001 } else { crate::hair::GROW_LIFT }, |v| v as f32).clamp(0.0, 0.1));
        let grown_center = if grown.is_some() {
            let on = self.resolve(&patch["grow"]["on"])?;
            self.meshes.whole(on).map(|m| {
                let (lo, hi) = m.bounds();
                to_local.transform_point3(self.scene.world_matrix(on).transform_point3((lo + hi) * 0.5))
            })
        } else { None };
        let o = self.scene.get_mut(id).ok_or("object vanished")?;
        if o.shape != Shape::Hair {
            return Err(format!("{} is not Hair; hair settings need shape Hair", o.name));
        }
        let Value::Object(patch) = patch else { return Err("hair must be an object".into()) };
        let mut hair = o.hair.clone().unwrap_or_default();
        if facial {
            hair.width = 0.0005;
            hair.cards = 1;
            hair.spread = 0.00005;
            hair.strands = if style == "lash" { 120 } else { 600 };
            hair.strand_width = 0.00006;
            hair.taper = 0.05;
            hair.frizz = 0.00005;
            hair.clump = 0.8;
            hair.interpolate = false;
            hair.center = grown_center;
            hair.root_color = [0.16, 0.09, 0.05];
            hair.tip_color = [0.24, 0.14, 0.08];
        }
        if hair.center.is_none() && grown.is_some() { hair.center = grown_center; }
        let guide = |g: &Value| -> Result<Guide, String> {
            let g = if g.is_array() { serde_json::json!({ "points": g }) } else { g.clone() };
            let mut g: Guide = serde_json::from_value(g).map_err(|e| format!("bad guide: {e}"))?;
            if world {
                g.points = g.points.iter().map(|p| to_local.transform_point3(*p)).collect();
            }
            if g.points.len() < 2 {
                return Err("a guide needs at least 2 points (root first)".into());
            }
            Ok(g)
        };
        if let Some((guides, replace)) = grown {
            if replace {
                hair.guides.clear();
            }
            hair.guides.extend(guides);
        }
        // The guides stay out of the JSON round trip: a settings patch keeps
        // them shared with the undo history (no copy), and only `guides`,
        // `add_guides` and `grow` change them.
        let mut guides = std::mem::take(&mut hair.guides);
        let mut merged = serde_json::to_value(&hair).map_err(|e| e.to_string())?;
        for (k, v) in patch {
            match k.as_str() {
                "grow" => {}
                "fur" if v.is_null() => merged["fur"] = Value::Null,
                "fur" => {
                    // Merged into the fur's settings (a hair without fur
                    // gets them from the defaults).
                    let mut f = merged.get("fur").filter(|f| !f.is_null()).cloned().unwrap_or_else(|| serde_json::to_value(crate::hair::Fur::default()).expect("fur"));
                    for (fk, fv) in v.as_object().ok_or("fur must be an object of fur settings")? {
                        f[fk] = fv.clone();
                    }
                    let _: crate::hair::Fur = serde_json::from_value(f.clone()).map_err(|e| format!("bad fur settings: {e}"))?;
                    merged["fur"] = f;
                }
                "guides" => guides = v.as_array().ok_or("guides must be a list")?.iter().map(guide).collect::<Result<_, _>>()?,
                "add_guides" => {
                    let more: Vec<Guide> = v.as_array().ok_or("add_guides must be a list")?.iter().map(guide).collect::<Result<_, _>>()?;
                    guides.extend(more);
                }
                "center" if world && !v.is_null() => {
                    let c: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| "center must be [x, y, z]")?;
                    merged["center"] = serde_json::json!(to_local.transform_point3(glam::Vec3::from(c)).to_array());
                }
                _ if merged.get(k).is_some() || k == "center" => merged[k] = v.clone(),
                _ => return Err(format!("unknown hair setting {k}; use {}", HAIR_KEYS.join(", "))),
            }
        }
        let mut hair: Hair = serde_json::from_value(merged).map_err(|e| format!("bad hair settings: {e}"))?;
        hair.guides = guides;
        if let Some(f) = fur_grown {
            // The fur's own settings, except what the patch sets itself.
            hair.guides = f.guides.into();
            hair.fur = Some(f.fur);
            hair.mirror = false;
            hair.center = None;
            if !patch.contains_key("strands") {
                hair.strands = f.strands;
            }
            if !patch.contains_key("strand_width") {
                hair.strand_width = f.strand_width;
            }
            if !patch.contains_key("lift") {
                hair.lift = f.lift;
            }
            if !patch.contains_key("frizz") {
                hair.frizz = f.frizz;
            }
            if !patch.contains_key("length_variation") {
                hair.length_variation = 0.25;
            }
            self.fur_reports.insert(id, f.report);
        }
        // Grown guides keep their lift for groom (the hair's units), unless
        // the patch sets one.
        if grown_lift.is_some() && !patch.contains_key("lift") {
            hair.lift = grown_lift.unwrap_or(crate::hair::GROW_LIFT) / hair_scale;
        }
        if hair.guides.len() > crate::hair::MAX_GUIDES {
            return Err(format!("at most {} guides", crate::hair::MAX_GUIDES));
        }
        hair.sanitize();
        o.hair = Some(hair);
        Ok(())
    }

    /// Guides grown over a clay scalp (`hair.grow`): roots spread evenly over
    /// the part of the clay's surface inside `center` / `radii` (world
    /// space; by default the scalp from the hairline to the nape). Each
    /// guide is draped like a hanging chain: it leaves the scalp toward
    /// `comb` (away from the `part` line), falls under gravity, and slides
    /// over the head and the body below it, kept `lift` outside them.
    /// Brow/lash styles instead comb or curl millimeter hairs without
    /// gravity, from a region or explicitly supplied world roots.
    /// Returns the guides in the hair object's space, and whether they
    /// replace the old ones.
    fn grow_guides(&mut self, hair_id: u64, g: &Value) -> Result<(Vec<Guide>, bool), String> {
        let num = |k: &str, d: f32| g[k].as_f64().map_or(d, |v| v as f32);
        let vec = |k: &str| -> Result<Option<Vec3>, String> {
            match g.get(k).filter(|v| !v.is_null()) {
                Some(v) => serde_json::from_value::<[f32; 3]>(v.clone()).map(|a| Some(Vec3::from(a))).map_err(|_| format!("grow.{k} must be [x, y, z]")),
                None => Ok(None),
            }
        };
        let style = g["style"].as_str().unwrap_or("drape");
        if !matches!(style, "drape" | "brow" | "lash") {
            return Err("grow.style must be drape, brow or lash".into());
        }
        let facial = style != "drape";
        if facial && g.get("roots").is_none() && (g.get("center").is_none() || g.get("radii").is_none()) {
            return Err("brow/lash grow needs center and radii, or roots along the brow/lid in world space".into());
        }
        let on = self.resolve(g.get("on").ok_or("grow.on: the clay to grow from (the head) is required")?)?;
        if self.scene.get(on).map(|o| o.shape) != Some(Shape::Clay) {
            return Err("grow.on must be a clay object (the head)".into());
        }
        self.meshes.sync(&self.scene);
        let clay_world = self.scene.world_matrix(on);
        let to_clay = clay_world.inverse();
        let snap = self.snapper(on)?;
        let (lo, hi) = self.meshes.whole(on).map(|m| m.bounds()).ok_or("the clay has no mesh")?;
        let (wlo, whi) = (clay_world.transform_point3(lo), clay_world.transform_point3(hi));
        let size = (whi - wlo).abs();
        // Default region: the scalp from the hairline down to the nape.
        let given = g.get("center").is_some_and(|v| !v.is_null()) || g.get("radii").is_some_and(|v| !v.is_null());
        let center = vec("center")?.unwrap_or(Vec3::new((wlo.x + whi.x) * 0.5, whi.y - size.y * 0.36, (wlo.z + whi.z) * 0.5 - size.z * 0.14));
        let radii = vec("radii")?.unwrap_or(Vec3::new(size.x * 0.56, size.y * 0.36, size.z * 0.58)).abs().max(Vec3::splat(1e-3));
        let count = (num("count", 40.0) as usize).clamp(1, 400);
        let length = num("length", if facial { 0.007 } else { 0.3 }).clamp(0.001, 3.0);
        let lift = num("lift", if facial { 0.0001 } else { 0.0015 }).clamp(0.0, 0.1);
        let comb = vec("comb")?.unwrap_or(if style == "lash" { Vec3::Y } else if facial { Vec3::new(1.0, 0.4, 0.0) } else { Vec3::new(0.0, -1.0, -0.25) }).normalize_or(Vec3::NEG_Y);
        let part = num("part", center.x);
        let half = g["mirror"].as_bool().unwrap_or(false);
        // World surface point and normal nearest a world point.
        let surface = |p: Vec3| snap(to_clay.transform_point3(p)).map(|(q, n)| (clay_world.transform_point3(q), clay_world.inverse().transpose().transform_vector3(n).normalize_or(Vec3::Y)));
        // Candidate roots: an even spread of directions from the center, snapped.
        let mut candidates = Vec::new();
        let tries = count * 6;
        let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
        for i in 0..tries {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / tries as f32;
            let r = (1.0 - y * y).sqrt();
            let dir = Vec3::new(r * (golden * i as f32).cos(), y, r * (golden * i as f32).sin());
            let Some((q, n)) = surface(center + dir * radii * 1.5) else { continue };
            let inside = ((q - center) / radii).length() <= 1.0;
            // The default region leaves the face bare: no roots facing
            // forward below the hairline.
            let face = !given && n.z > 0.3 && q.y < whi.y - size.y * 0.22;
            if inside && !face && (!half || q.x >= part - 1e-4) {
                candidates.push((q, n));
            }
        }
        if let Some(v) = g.get("roots") {
            let points: Vec<Vec3> = serde_json::from_value(v.clone()).map_err(|_| "grow.roots must be a list of world points")?;
            if points.len() > crate::hair::MAX_GUIDES || points.iter().any(|p| !p.is_finite()) {
                return Err(format!("grow.roots needs at most {} finite points", crate::hair::MAX_GUIDES));
            }
            candidates = points.into_iter().filter_map(surface).collect();
        }
        if candidates.is_empty() {
            return Err("grow found no scalp inside the region; check grow.on, center and radii".into());
        }
        // Keep `count` roots, each as far as possible from those kept,
        // starting with the crown (the highest), which is easy to leave bare.
        let crown = *candidates.iter().max_by(|a, b| a.0.y.total_cmp(&b.0.y)).expect("not empty");
        let mut roots = if g.get("roots").is_some() { candidates.clone() } else { vec![crown] };
        while roots.len() < count.min(candidates.len()) {
            let next = candidates.iter().max_by(|a, b| {
                let d = |c: &(Vec3, Vec3)| roots.iter().map(|r| r.0.distance_squared(c.0)).fold(f32::MAX, f32::min);
                d(a).total_cmp(&d(b))
            });
            roots.push(*next.expect("candidates"));
        }
        // Hair rests on the head and on the body the head sits on.
        let mut colliders = vec![self.collider(on)?];
        if let Some(body) = self.scene.ancestors(on).into_iter().find(|&a| self.scene.get(a).is_some_and(|o| o.shape == Shape::Clay)) {
            colliders.push(self.collider(body)?);
        }
        let to_hair = self.scene.world_matrix(hair_id).inverse();
        let drape = |&(q, n): &(Vec3, Vec3)| -> Guide {
            // Away from the part line and back from the face, then the comb.
            let away = Vec3::X * (q.x - part).signum() * 0.3;
            let front = ((q.z - center.z) / radii.z).max(0.0);
            let flow = (comb + away + Vec3::NEG_Z * front * 1.5).normalize_or(comb);
            let points = if facial {
                crate::hair::facial_guide(q, n, comb, length, lift, style == "lash", surface).points
            } else {
                drape_strand(q + n * lift, n, flow, length, lift, &colliders)
            };
            Guide { points: points.iter().map(|p| to_hair.transform_point3(*p)).collect(), width: None }
        };
        let guides = std::thread::scope(|sc| {
            let per = roots.len().div_ceil(std::thread::available_parallelism().map_or(8, |n| n.get())).max(1);
            let handles: Vec<_> = roots.chunks(per).map(|chunk| sc.spawn(|| chunk.iter().map(&drape).collect::<Vec<_>>())).collect();
            handles.into_iter().flat_map(|h| h.join().expect("drape thread")).collect::<Vec<_>>()
        });
        Ok((guides, g["replace"].as_bool().unwrap_or(true)))
    }

    /// A clay as groom keeps hair out of it: its composed field (sculpt
    /// layers too), as its mesh is drawn.
    pub(super) fn head_collider(&self, id: u64) -> Result<Collider, String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        let clay = crate::layers::composed_clay(o.clay.as_ref().ok_or("not a clay object")?);
        clay.cell_size().ok_or("clay is empty")?;
        let bounds = self.meshes.whole(id).map(|m| m.bounds()).ok_or("the clay has no mesh")?;
        Collider::new(&clay, bounds, self.scene.rest_world_matrix(id))
    }

    /// A clay object as something hair rests on.
    fn collider(&self, id: u64) -> Result<Collider, String> {
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        clay.cell_size().ok_or("clay is empty")?;
        let bounds = self.meshes.whole(id).map(|m| m.bounds()).ok_or("the clay has no mesh")?;
        Collider::new(clay, bounds, self.scene.rest_world_matrix(id))
    }

    /// The clay a hair object sits on: its nearest clay ancestor, which
    /// `groom` keeps the guides out of and film keeps the strands out of.
    pub(super) fn hair_head(&self, id: u64) -> Option<u64> {
        self.scene.ancestors(id).into_iter().find(|&a| self.scene.get(a).is_some_and(|o| o.shape == Shape::Clay))
    }

    /// The film strands of hair `id` (in its own space), kept out of the
    /// clay it sits on as that clay is drawn, to the hair's lift
    /// (`Hair::strands_kept_out`); `budget` caps the strand count (the
    /// summary's sample). A hair under no clay gets exactly
    /// `Hair::strands`; under one whose collider cannot be built (no mesh
    /// yet) it gets them too, with the reason in `error` for the caller to
    /// report. Film renders and `hair_summary` both take their strands
    /// from here, so what the summary counts is what film draws.
    pub(super) fn hair_strands(&self, id: u64, budget: Option<u32>) -> KeptStrands {
        let Some(hair) = self.scene.get(id).and_then(|o| o.hair.as_ref()) else { return KeptStrands::default() };
        let mut h = hair.clone();
        if let Some(b) = budget {
            h.strands = h.strands.min(b);
        }
        h.sanitize();
        let head = self.hair_head(id);
        let collider = head.map(|c| self.head_collider(c));
        let head_collider = match &collider {
            None => return KeptStrands { strands: h.strands(), ..Default::default() },
            Some(Err(e)) => return KeptStrands { strands: h.strands(), head, error: Some(e.clone()), moved: 0 },
            Some(Ok(c)) => c,
        };
        // At rest, in the hair's own space: the head's field is its rest
        // shape, so the hair is placed at rest too (`pose::at_rest`'s
        // matrices); film then places the strands where the pose puts the
        // hair, and they follow it.
        let to_world = self.scene.rest_world_matrix(id);
        let scale = to_world.to_scale_rotation_translation().0.abs().max_element().max(1e-9);
        let (strands, moved) = h.strands_kept_out(Some(&InHair::new(head_collider, to_world, h.lift * scale)));
        KeptStrands { strands, moved, head, error: None }
    }

    /// Sample real fibers against nearby head clay, without hiding bad guides.
    pub(super) fn hair_summary(&self, id: u64) -> Value {
        let Some(hair) = self.scene.get(id).and_then(|o| o.hair.as_ref()) else { return Value::Null };
        let mut report = hair.coverage();
        if hair.fur.is_some()
            && let Some(grown) = self.fur_reports.get(&id)
        {
            report["grown"] = grown.clone();
        }
        // At rest, as the strands are kept out (the colliders are rest fields).
        let world = self.scene.rest_world_matrix(id);
        let kept = self.hair_strands(id, Some(256));
        let (fibers, lifted) = (kept.strands, kept.moved);
        if let Some(e) = &kept.error {
            report["warnings"].as_array_mut().unwrap().push(json!(format!("strands not kept out of the head: {e}")));
        }
        let roots: Vec<_> = fibers.iter().map(|s| world.transform_point3(s.points[0])).collect();
        let middle = hair.center.map(|p| world.transform_point3(p)).unwrap_or_else(|| roots.iter().copied().sum::<Vec3>() / roots.len().max(1) as f32);
        // The clay the hair sits on (whatever its size: a whole body under
        // fur), and other head-sized clays near the roots.
        let own = self.hair_head(id);
        let colliders: Vec<_> = self.scene.objects.iter().filter(|o| o.shape == Shape::Clay).filter_map(|o| {
            if Some(o.id) == own {
                return self.collider(o.id).ok();
            }
            let (lo, hi) = self.meshes.whole(o.id)?.bounds();
            if (hi - lo).max_element() > 0.7 { return None; }
            let inverse = self.scene.world_matrix(o.id).inverse();
            let q = inverse.transform_point3(middle);
            if !q.cmpge(lo - Vec3::splat(0.02)).all() || !q.cmple(hi + Vec3::splat(0.02)).all() { return None; }
            if !roots.iter().any(|p| {
                let q = inverse.transform_point3(*p);
                q.cmpge(lo - Vec3::splat(0.02)).all() && q.cmple(hi + Vec3::splat(0.02)).all()
            }) { return None; }
            self.collider(o.id).ok()
        }).collect();
        let inside = crate::hair::inside_count(&fibers, |p| {
            let p = world.transform_point3(p);
            colliders.iter().any(|c| {
                let q = c.to_local.transform_point3(p);
                c.field.far_side(q, &c.list, c.cell) != Some(false)
                    && c.field.sample(q, &c.list).0 < -c.cell * 0.25
            })
        });
        report["collision_check"] = json!(if colliders.is_empty() { "no nearby head clay" } else { "sampled against nearby head clay" });
        report["sampled_strands"] = json!(fibers.len());
        report["strands_inside_head"] = json!(inside);
        report["strands_kept_out_of_head"] = json!(lifted);
        if let Some(moved) = self.meshes.hair_cards_moved(id) {
            report["cards_kept_out_of_head"] = json!(moved);
        }
        if inside > 0 {
            report["warnings"].as_array_mut().unwrap().push(json!(format!("{inside}/{} sampled strands enter the head; move guides outside the scalp or increase grow.lift", fibers.len())));
        }
        report
    }

    /// Turns an object so its local +Z (an eye's front) points at a world
    /// point, keeping its up close to world +Y.
    fn aim(&mut self, id: u64, target: Vec3) -> Result<(), String> {
        let (scale, _, pos) = self.scene.world_matrix(id).to_scale_rotation_translation();
        let z = (target - pos).normalize_or_zero();
        if z == Vec3::ZERO {
            return Err("aim point is at the object's own position".into());
        }
        let up = if z.y.abs() > 0.999 { Vec3::Z } else { Vec3::Y };
        let x = up.cross(z).normalize();
        let y = z.cross(x);
        let rot = Quat::from_mat3(&Mat3::from_cols(x, y, z));
        self.scene.set_world_matrix(id, glam::Mat4::from_scale_rotation_translation(scale, rot, pos));
        Ok(())
    }

    /// A copy mirrored across its parent's X = 0 plane: position and
    /// rotation flipped, `.L`/`.R` names (and bones) swapped. Children are
    /// copied as they are.
    pub(super) fn mirror_copy(&mut self, id: u64) -> Result<u64, String> {
        let original = self.scene.get(id).ok_or("object vanished")?.clone();
        let copy = self.scene.duplicate_tree(id).ok_or("could not copy")?;
        let rig = original.parent.and_then(|p| self.scene.get(p)).and_then(|p| p.rig.clone());
        let o = self.scene.get_mut(copy).expect("just made");
        o.translation.x = -o.translation.x;
        o.pivot.x = -o.pivot.x;
        o.pivot_comp.x = -o.pivot_comp.x;
        let q = o.rotation;
        o.rotation = Quat::from_xyzw(q.x, -q.y, -q.z, q.w).normalize();
        o.name = crate::rig::mirror_name(&original.name).unwrap_or_else(|| format!("{}_mirror", original.name));
        if let Some(b) = &original.bone
            && let Some(m) = crate::rig::mirror_name(b)
            && rig.as_ref().is_some_and(|r| r.bone_index(&m).is_some())
        {
            o.bone = Some(m);
        }
        Ok(copy)
    }
}

/// An error naming what materials exist.
fn smart_names() -> String {
    crate::smart::PRESETS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
}

pub(super) fn unknown_material(scene: &Scene, name: &str) -> String {
    let mut names: Vec<&str> = scene.materials.iter().map(|m| m.name.as_str()).collect();
    names.extend(PRESETS.iter().map(|p| p.0));
    format!("unknown material {name}; define it with set_material or use one of {names:?} or a smart preset ({})", smart_names())
}

/// Blob material names that are neither in the library nor presets.
pub(super) fn unknown_blob_materials(scene: &Scene, id: u64) -> Vec<String> {
    let Some(clay) = scene.get(id).and_then(|o| o.clay.as_ref()) else { return Vec::new() };
    clay.material_names().into_iter().filter(|n| !n.is_empty() && Material::find(&scene.materials, n).is_none()).collect()
}

const HAIR_KEYS: &[&str] = &["guides", "add_guides", "grow", "fur", "lift", "width", "taper", "cards", "spread", "root_color", "tip_color", "center", "mirror", "seed", "strands", "strand_width", "clump", "clump_size", "clump_levels", "frizz", "volume", "flyaways", "length_variation", "melanin_variation", "interpolate", "rise"];
const GROW_KEYS: &[&str] = &["on", "style", "center", "radii", "count", "length", "lift", "comb", "part", "mirror", "roots", "replace"];

/// Validate before growing or mutating anything, including nested settings.
pub(super) fn validate_hair_keys(patch: &Value) -> Result<(), String> {
    let object = patch.as_object().ok_or("hair must be an object")?;
    for key in object.keys() {
        if !HAIR_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown hair setting {key}; use {}", HAIR_KEYS.join(", ")));
        }
    }
    if let Some(grow) = patch.get("grow").filter(|v| !v.is_null() && v.get("fur").and_then(Value::as_bool) != Some(true)) {
        let object = grow.as_object().ok_or("hair.grow must be an object")?;
        for key in object.keys() {
            if !GROW_KEYS.contains(&key.as_str()) {
                return Err(format!("unknown grow setting {key}; use {} (center and radii go directly in grow)", GROW_KEYS.join(", ")));
            }
        }
    }
    Ok(())
}

/// A hair's film strands and what kept them out of its head.
#[derive(Default)]
pub(super) struct KeptStrands {
    pub(super) strands: Vec<crate::hair::Strand>,
    /// Strands the head moved.
    pub(super) moved: usize,
    /// The clay the hair sits on, if any.
    pub(super) head: Option<u64>,
    /// Why the strands could not be kept out of it.
    pub(super) error: Option<String>,
}

/// One guide hung like a chain from `root`: it leaves the scalp along its
/// normal `n` and `flow`, then falls under gravity, kept `lift` outside the
/// colliders. Segment lengths stay fixed (each point follows the one
/// before it), so the guide keeps its `length`.
fn drape_strand(root: Vec3, n: Vec3, flow: Vec3, length: f32, lift: f32, colliders: &[Collider]) -> Vec<Vec3> {
    let segments = ((length / 0.02).ceil() as usize).clamp(8, 32);
    let l = length / segments as f32;
    let tangent = (flow - n * n.dot(flow)).normalize_or(flow);
    // Hair stands off the scalp a little at the root (volume).
    let leave = (tangent + n * 0.12).normalize_or(n);
    let mut p: Vec<Vec3> = (0..=segments).map(|k| root + leave * l * k as f32).collect();
    // Where each point touches a collider: its normal there. Touching
    // hair is combed along the surface (the comb holds it, as styling
    // does); free hair falls.
    let mut touch: Vec<Option<Vec3>> = vec![Some(n); segments + 1];
    let fall = l * 0.15;
    let steps = 20 + (1.6 * segments as f32 / 0.15) as usize;
    for _ in 0..steps {
        for (q, t) in p.iter_mut().zip(&touch).skip(1) {
            *q += match t {
                Some(sn) => {
                    // Along the surface toward the comb; gravity only
                    // where the surface faces down enough to let go.
                    let along = (flow - *sn * sn.dot(flow)).normalize_or(Vec3::NEG_Y);
                    let slide = (Vec3::NEG_Y - *sn * sn.dot(Vec3::NEG_Y)) * 0.3;
                    (along + slide).normalize_or(along) * fall - *sn * fall * 0.2
                }
                None => Vec3::NEG_Y * fall + flow * fall * 0.2,
            };
        }
        // Stiff near the root: the first segments keep their way out.
        for k in 1..=2.min(segments) {
            let keep = if k == 1 { 0.5 } else { 0.25 };
            let d = (p[k] - p[k - 1]).normalize_or(leave).lerp(leave, keep).normalize_or(leave);
            p[k] = p[k - 1] + d * l;
        }
        for k in 1..=segments {
            let d = (p[k] - p[k - 1]).normalize_or(leave);
            p[k] = p[k - 1] + d * l;
            touch[k] = None;
            for c in colliders {
                let (q, sn) = c.push_out(p[k], lift);
                p[k] = q;
                touch[k] = touch[k].or(sn);
            }
        }
    }
    p
}

#[cfg(test)]
mod hair_tests {
    use super::*;

    #[test]
    fn material_edit_reply_does_not_echo_layers_or_library() {
        let args=json!({"name":"paint","layers":vec![crate::smart::Layer::default();16],"roughness":0.8});
        let reply=material_edit_summary("paint",&args,16,650,50);
        assert!(reply.to_string().len()<200);
        assert_eq!(reply["users"],650);
        assert_eq!(reply["layers"],16);
        assert!(reply.get("library").is_none());
    }

    #[test]
    fn unknown_hair_and_grow_keys_fail_before_growing() {
        for patch in [json!({"strand_wdith": 0.001}), json!({"grow": {"region": {"center": [0, 1, 0]}}}), json!({"grow": 2}), json!([])] {
            assert!(validate_hair_keys(&patch).is_err(), "{patch}");
        }
        assert!(validate_hair_keys(&json!({"strands": 50000, "grow": {"on": "head", "center": [0, 1, 0], "radii": [0.1, 0.1, 0.1]}})).is_ok());
    }
}
