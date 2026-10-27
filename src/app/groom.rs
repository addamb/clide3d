//! `groom` (docs/QUALITY_PLAN.md Stage 4c): groom brushes on a Hair
//! object's guides, kept out of the head. The strokes come in world space
//! (or the hair's own), go into the hair's space, and `crate::groom`
//! does the rest; the guides change in place (D20: no ledger, one undo
//! step per call). The editor's Groom tool sends the same call.

use std::collections::BTreeMap;

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::look::{Collider as Head, InHair};
use super::tools::{object_json_in, text};
use crate::groom::{Affect, Collider, Kind, Stroke};
use crate::mcp::ToolResult;
use crate::scene::Shape;

const KEYS: &[&str] = &["target", "strokes", "space", "render", "size"];
const STROKE_KEYS: &[&str] = &["brush", "points", "point", "radius", "strength", "falloff", "direction", "length", "plane", "angle", "affect", "mirror", "collide", "iterations"];

/// Where a stroke's collider comes from.
#[derive(Clone, Copy, PartialEq)]
enum Collide {
    None,
    Clay(u64),
}

fn vec3(v: &Value, what: &str) -> Result<Vec3, String> {
    let a: [f32; 3] = serde_json::from_value(v.clone()).map_err(|_| format!("{what} must be [x, y, z]"))?;
    let p = Vec3::from(a);
    if !p.is_finite() {
        return Err(format!("{what} must be finite"));
    }
    Ok(p)
}

fn number(v: &Value, what: &str) -> Result<Option<f32>, String> {
    match v {
        Value::Null => Ok(None),
        v => v.as_f64().map(|x| x as f32).filter(|x| x.is_finite()).map(Some).ok_or(format!("{what} must be a finite number")),
    }
}

impl ClideApp {
    /// `groom`: see the module doc and the tool's schema. Every argument
    /// is checked, and the strokes' sizes bounded, before the hair is
    /// touched; the reply waits for the hair's cards when it shows them.
    pub(super) fn groom_tool(&mut self, args: &Value) -> ToolResult {
        let obj = args.as_object().ok_or("groom takes an object")?;
        if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("unknown groom field {k}; use {}", KEYS.join(", ")));
        }
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if o.shape != Shape::Hair {
            return Err(format!("{} is not a Hair object; groom shapes a Hair's guides", o.name));
        }
        let name = o.name.clone();
        let mut hair = o.hair.clone().unwrap_or_default();
        crate::groom::check_size(&hair)?;
        let raws = args["strokes"].as_array().ok_or("strokes is required: a list of groom strokes")?;
        if raws.is_empty() || raws.len() > crate::groom::MAX_STROKES {
            return Err(format!("strokes takes 1 to {} strokes ({} sent)", crate::groom::MAX_STROKES, raws.len()));
        }
        let world = match args["space"].as_str().unwrap_or("world") {
            "world" => true,
            "local" => false,
            other => return Err(format!("space must be world or local, not {other}")),
        };
        let render = match args.get("render").filter(|v| !v.is_null()) {
            None => true,
            Some(v) => v.as_bool().ok_or("render must be true or false")?,
        };
        let size = match args.get("size").filter(|v| !v.is_null()) {
            None => 256,
            Some(v) => v.as_u64().filter(|s| (64..=1024).contains(s)).ok_or("size must be 64 to 1024 pixels")? as usize,
        };
        let to_world = self.scene.world_matrix(id);
        let to_hair = to_world.inverse();
        let scale = to_world.to_scale_rotation_translation().0.abs().max_element().max(1e-9);
        // World sizes into the hair's units.
        let unit = if world { 1.0 / scale } else { 1.0 };
        let parent_clay = self.scene.ancestors(id).into_iter().find(|&a| self.scene.get(a).is_some_and(|o| o.shape == Shape::Clay));
        let mut strokes: Vec<(Stroke, Collide)> = Vec::with_capacity(raws.len());
        for (n, raw) in raws.iter().enumerate() {
            let at = |e: String| format!("stroke {n}: {e}");
            let r = raw.as_object().ok_or_else(|| at("a stroke is an object".into()))?;
            if let Some(k) = r.keys().find(|k| !STROKE_KEYS.contains(&k.as_str())) {
                return Err(at(format!("unknown field {k}; a stroke takes {}", STROKE_KEYS.join(", "))));
            }
            let kind: Kind = serde_json::from_value(raw["brush"].clone()).map_err(|_| at("brush must be comb, lengthen, shorten, cut, clump, twist, smooth, puff or relax".into()))?;
            // The path: its length is checked before anything is read.
            let points: Vec<Vec3> = match (raw.get("points").filter(|v| !v.is_null()), raw.get("point").filter(|v| !v.is_null())) {
                (Some(_), Some(_)) => return Err(at("send points or point, not both".into())),
                (Some(v), None) => {
                    let list = v.as_array().ok_or_else(|| at("points must be a list of [x, y, z]".into()))?;
                    if list.is_empty() || list.len() > crate::groom::MAX_PATH {
                        return Err(at(format!("a stroke takes 1 to {} points ({} sent)", crate::groom::MAX_PATH, list.len())));
                    }
                    list.iter().map(|p| vec3(p, "a point")).collect::<Result<_, _>>().map_err(at)?
                }
                (None, Some(v)) => vec![vec3(v, "point").map_err(at)?],
                (None, None) => return Err(at("points (the brush path) or point is required".into())),
            };
            let radius = number(&raw["radius"], "radius").map_err(at)?.ok_or_else(|| at("radius is required".into()))?;
            let mut s = Stroke::new(kind, points, radius);
            if let Some(v) = number(&raw["strength"], "strength").map_err(at)? {
                s.strength = v;
            }
            if let Some(v) = raw.get("falloff").filter(|v| !v.is_null()) {
                s.falloff = serde_json::from_value(v.clone()).map_err(|e| at(e.to_string()))?;
            }
            if let Some(v) = raw.get("direction").filter(|v| !v.is_null()) {
                s.direction = Some(vec3(v, "direction").map_err(at)?);
            }
            s.length = number(&raw["length"], "length").map_err(at)?;
            if let Some(v) = raw.get("plane").filter(|v| !v.is_null()) {
                s.plane = Some((vec3(&v["point"], "plane.point").map_err(at)?, vec3(&v["normal"], "plane.normal").map_err(at)?));
            }
            if let Some(a) = number(&raw["angle"], "angle").map_err(at)? {
                if a.abs() > crate::groom::MAX_ANGLE {
                    return Err(at(format!("angle must be within ±{} degrees", crate::groom::MAX_ANGLE)));
                }
                s.angle = a.to_radians();
            }
            if let Some(v) = raw.get("affect").filter(|v| !v.is_null()) {
                s.affect = serde_json::from_value::<Affect>(v.clone()).map_err(|_| at("affect must be tips, roots or all".into()))?;
            }
            s.mirror = match raw.get("mirror").filter(|v| !v.is_null()) {
                None => hair.mirror,
                Some(v) => v.as_bool().ok_or_else(|| at("mirror must be true or false".into()))?,
            };
            if let Some(v) = raw.get("iterations").filter(|v| !v.is_null()) {
                s.iterations = v.as_u64().filter(|i| (1..=crate::groom::MAX_ITERATIONS as u64).contains(i)).ok_or_else(|| at(format!("iterations must be 1 to {}", crate::groom::MAX_ITERATIONS)))? as u32;
            }
            let collide = match raw.get("collide").filter(|v| !v.is_null()) {
                None => parent_clay.map_or(Collide::None, Collide::Clay),
                Some(Value::Bool(false)) => Collide::None,
                Some(Value::Bool(true)) => Collide::Clay(parent_clay.ok_or_else(|| at("collide: true needs the hair under a clay (its head); name the clay instead".into()))?),
                Some(v) => {
                    let c = self.resolve(v).map_err(at)?;
                    if self.scene.get(c).map(|o| o.shape) != Some(Shape::Clay) {
                        return Err(at("collide must be a clay object (the head), true or false".into()));
                    }
                    Collide::Clay(c)
                }
            };
            // Into the hair's space.
            if world {
                s.points = s.points.iter().map(|p| to_hair.transform_point3(*p)).collect();
                s.direction = s.direction.map(|d| to_hair.transform_vector3(d));
                s.plane = s.plane.map(|(p, n)| (to_hair.transform_point3(p), to_world.transpose().transform_vector3(n)));
            }
            s.radius *= unit;
            s.length = s.length.map(|l| l * unit);
            s.validate().map_err(at)?;
            strokes.push((s, collide));
        }
        // The heads, as they are drawn (their composed field; unchecked
        // ledgers settle first, §0.2 rule 1).
        let heads: Vec<u64> = strokes.iter().filter_map(|(_, c)| if let Collide::Clay(c) = c { Some(*c) } else { None }).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        self.settle_clays(heads.iter().copied())?;
        self.meshes.sync(&self.scene);
        let mut built: BTreeMap<u64, Head> = BTreeMap::new();
        for &h in &heads {
            built.insert(h, self.head_collider(h)?);
        }
        let lift = hair.lift * scale;
        // The head's field is its rest shape: meet it with the hair at rest.
        let rest = self.scene.rest_world_matrix(id);
        let in_hair: BTreeMap<u64, InHair> = built.iter().map(|(&h, head)| (h, InHair::new(head, rest, lift))).collect();
        let list: Vec<(Stroke, Option<&dyn Collider>)> = strokes.iter().map(|(s, c)| (s.clone(), match c {
            Collide::None => None,
            Collide::Clay(h) => in_hair.get(h).map(|x| x as &dyn Collider),
        })).collect();
        let timer = std::time::Instant::now();
        let report = crate::groom::apply(&mut hair, &list)?;
        let ms = timer.elapsed().as_secs_f64() * 1e3;
        hair.sanitize();
        let collide_names: Vec<String> = heads.iter().filter_map(|h| self.scene.get(*h).map(|o| o.name.clone())).collect();
        let mut reply = json!({
            "target": id,
            "guides": hair.guides.len(),
            "guides_changed": report.guides_changed,
            "mean_move": report.mean_move * scale,
            "max_move": report.max_move * scale,
            "mean_length_before": report.mean_length_before * scale,
            "mean_length_after": report.mean_length_after * scale,
            "length_change": (report.mean_length_after - report.mean_length_before) * scale,
            "points_before": report.points_before,
            "points_after": report.points_after,
            "collide": collide_names,
            "lift": lift,
            "collisions_resolved": report.collisions_resolved,
            "still_inside": report.still_inside,
            "roots_beyond_plane": report.roots_beyond_plane,
            "points_capped": report.points_capped,
            "timing_ms": (ms * 100.0).round() / 100.0,
        });
        if heads.is_empty() {
            reply["note"] = json!(format!("{name} is under no clay and no stroke named collide: nothing kept the guides out of a head"));
        }
        self.scene.get_mut(id).expect("checked").hair = Some(hair);
        if !render {
            reply["object"] = object_json_in(&self.scene, id);
            return Ok(text(reply));
        }
        // The guides from the front and the right, once the cards are built.
        self.defer_reply(move |app| {
            app.meshes.sync(&app.scene);
            reply["object"] = object_json_in(&app.scene, id);
            let (lo, hi) = app.scene.get(id).and_then(|o| o.hair.as_ref()).map(|h| {
                h.guides.iter().flat_map(|g| g.points.iter()).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| {
                    let w = app.scene.world_matrix(id).transform_point3(*p);
                    (a.min(w), b.max(w))
                })
            }).unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let (center, radius) = if lo.cmple(hi).all() { ((lo + hi) * 0.5, ((hi - lo).length() * 0.6).max(0.01)) } else { (Vec3::ZERO, 1.0) };
            let shots = app.render_views_now(&json!({ "views": ["front", "right"], "center": center.to_array(), "radius": radius, "size": size, "include_reference": false }))?;
            let mut out = text(reply);
            out.extend(shots.into_iter().filter(|v| v["type"] == "image"));
            Ok(out)
        })
    }
}
