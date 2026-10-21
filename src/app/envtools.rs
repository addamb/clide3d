//! Agent tools for environments (see `envkit`): `kit` places dungeon
//! pieces, rocks and props on the grid, `scatter` spreads copies of
//! objects over an area or a surface, `edit_terrain` shapes and paints a
//! heightfield; `light` on create/update_object makes any object glow.

use glam::{Quat, Vec2, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use crate::envkit::kit::{Kit, Piece};
use crate::envkit::light::LightSource;
use crate::envkit::scatter::{self, Ground, Scatter, Settings};
use crate::envkit::terrain::{Stroke, Terrain};
use crate::envkit::GRID;
use crate::mcp::ToolResult;
use crate::scene::Shape;

/// `kit` arguments that are not piece settings.
const KIT_TOOL_KEYS: [&str; 10] = ["target", "name", "at", "turn", "snap", "parent", "material", "light", "visible", "space"];

/// Rounds `v` to the nearest multiple of `step` (0 = no snapping).
pub fn snap(v: f32, step: f32) -> f32 {
    if step > 0.0 { (v / step).round() * step } else { v }
}

impl ClideApp {
    /// `kit`: makes a piece (or changes `target`), snapped to the grid.
    pub(super) fn kit_tool(&mut self, args: &Value) -> ToolResult {
        let existing = match args.get("target").filter(|t| !t.is_null()) {
            Some(t) => {
                let id = self.resolve(t)?;
                let o = self.scene.get(id).ok_or("object vanished")?;
                Some((id, o.kit.clone().ok_or_else(|| format!("{} is not a kit piece", o.name))?))
            }
            None => None,
        };
        // Settings merge onto the piece's own (or a new piece's defaults).
        let mut merged = match &existing {
            Some((_, k)) => serde_json::to_value(k).map_err(|e| e.to_string())?,
            None => {
                let piece = args.get("piece").ok_or_else(|| format!("piece is required: one of {}", Piece::ALL.map(Piece::name).join(", ")))?;
                let piece: Piece = serde_json::from_value(piece.clone()).map_err(|_| format!("unknown piece {piece}; use {}", Piece::ALL.map(Piece::name).join(", ")))?;
                serde_json::to_value(Kit::new(piece)).map_err(|e| e.to_string())?
            }
        };
        for (k, v) in args.as_object().into_iter().flatten().filter(|(k, _)| !KIT_TOOL_KEYS.contains(&k.as_str())) {
            if v.is_null() {
                merged.as_object_mut().expect("object").remove(k);
            } else {
                merged[k] = v.clone();
            }
        }
        let mut kit: Kit = serde_json::from_value(merged).map_err(|e| format!("kit settings: {e}"))?;
        kit.validate()?;
        let step = match args.get("snap").and_then(Value::as_f64) {
            Some(s) if s.is_finite() && s >= 0.0 => s as f32,
            Some(_) => return Err("snap must be 0 (off) or a positive step in meters".into()),
            None => if is_architecture(kit.piece) { GRID * 0.5 } else { 0.0 },
        };
        if step > 0.0 {
            kit.snap();
        }
        kit.sanitize();
        let piece = kit.piece;
        let id = match existing {
            Some((id, _)) => id,
            None => {
                let id = self.scene.add(Shape::Kit);
                let name = self.unique_name(piece.name());
                let o = self.scene.get_mut(id).expect("just added");
                o.name = name;
                o.light = piece.light();
                id
            }
        };
        let fresh = args.get("target").is_none_or(Value::is_null);
        let result = (|| -> Result<(), String> {
            if let Some(p) = args.get("parent").filter(|p| !p.is_null()) {
                let parent = self.resolve(p)?;
                self.scene.reparent(id, Some(parent))?;
            }
            let o = self.scene.get_mut(id).ok_or("object vanished")?;
            o.kit = Some(kit);
            if let Some(n) = args["name"].as_str() {
                o.name = n.to_string();
            }
            if let Some(v) = args["visible"].as_bool() {
                o.visible = v;
            }
            if let Some(a) = args.get("at").filter(|v| !v.is_null()) {
                let a: [f32; 3] = serde_json::from_value(a.clone()).map_err(|_| "at must be [x, y, z]")?;
                if !Vec3::from(a).is_finite() {
                    return Err("at must be finite".into());
                }
                // Snap X and Z to the grid; Y stays (torches hang at a height).
                o.translation = Vec3::new(snap(a[0], step), a[1], snap(a[2], step));
            }
            if let Some(t) = args.get("turn").filter(|v| !v.is_null()) {
                let t = t.as_f64().filter(|t| t.is_finite()).ok_or("turn must be degrees about Y")? as f32;
                let t = if step > 0.0 { snap(t, 90.0) } else { t };
                o.rotation = Quat::from_rotation_y(t.to_radians());
            }
            self.apply_light(id, args)?;
            if args.get("material").is_some() {
                self.apply_look(id, &json!({ "material": args["material"] }))?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            if fresh {
                self.scene.remove_tree(id);
            }
            return Err(e);
        }
        self.select_only(id);
        Ok(super::tools::text(self.env_summary(id)))
    }

    /// `light` on create/update_object (and `kit`): a preset name, settings
    /// (merged onto the current light), or null / false to remove it.
    pub(super) fn apply_light(&mut self, id: u64, args: &Value) -> Result<(), String> {
        let Some(v) = args.get("light") else { return Ok(()) };
        let o = self.scene.get_mut(id).ok_or("object vanished")?;
        let presets = crate::envkit::light::PRESETS.map(|p| p.0).join(", ");
        o.light = match v {
            Value::Null | Value::Bool(false) => None,
            Value::Bool(true) => Some(o.light.clone().unwrap_or_default()),
            Value::String(name) => Some(LightSource { offset: o.light.as_ref().map_or(Vec3::ZERO, |l| l.offset), ..LightSource::preset(name).ok_or_else(|| format!("unknown light preset {name}; use {presets}"))? }),
            Value::Object(patch) => {
                let mut base = match patch.get("preset").and_then(Value::as_str) {
                    Some(name) => LightSource { offset: o.light.as_ref().map_or(Vec3::ZERO, |l| l.offset), ..LightSource::preset(name).ok_or_else(|| format!("unknown light preset {name}; use {presets}"))? },
                    None => o.light.clone().unwrap_or_default(),
                };
                let mut value = serde_json::to_value(&base).map_err(|e| e.to_string())?;
                for (k, v) in patch.iter().filter(|(k, _)| k.as_str() != "preset") {
                    value[k] = v.clone();
                }
                base = serde_json::from_value(value).map_err(|e| format!("light: {e} (fields: preset, offset, temperature, color, intensity, radius, flicker)"))?;
                Some(base)
            }
            _ => return Err(format!("light must be a preset ({presets}), settings, or null")),
        };
        if let Some(l) = &mut o.light {
            l.sanitize();
        }
        Ok(())
    }

    /// `kit`, `terrain` and `light` settings of create/update_object.
    pub(super) fn apply_env_props(&mut self, id: u64, args: &Value) -> Result<(), String> {
        if let Some(v) = args.get("kit").filter(|v| !v.is_null()) {
            let o = self.scene.get(id).ok_or("object vanished")?;
            if o.shape != Shape::Kit {
                return Err(format!("{} is not a Kit object", o.name));
            }
            let mut merged = serde_json::to_value(o.kit.clone().unwrap_or_else(|| Kit::new(Piece::Wall))).map_err(|e| e.to_string())?;
            for (k, val) in v.as_object().ok_or("kit must be an object of piece settings")? {
                merged[k] = val.clone();
            }
            let mut kit: Kit = serde_json::from_value(merged).map_err(|e| format!("kit settings: {e}"))?;
            kit.validate()?;
            kit.sanitize();
            let fresh_light = o.kit.as_ref().is_none_or(|k| k.piece != kit.piece) && o.light.is_none();
            let o = self.scene.get_mut(id).expect("checked");
            if fresh_light {
                o.light = kit.piece.light();
            }
            o.kit = Some(kit);
        }
        if let Some(v) = args.get("terrain").filter(|v| !v.is_null()) {
            self.merge_terrain(id, v)?;
        }
        self.apply_light(id, args)
    }

    fn merge_terrain(&mut self, id: u64, v: &Value) -> Result<(), String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        if o.shape != Shape::Terrain {
            return Err(format!("{} is not a Terrain object", o.name));
        }
        let mut merged = serde_json::to_value(o.terrain.clone().unwrap_or_default()).map_err(|e| e.to_string())?;
        for (k, val) in v.as_object().ok_or("terrain must be an object of settings")? {
            if matches!(k.as_str(), "sculpt" | "paint") {
                return Err("sculpt and paint grids change through edit_terrain strokes".into());
            }
            merged[k] = val.clone();
        }
        let mut t: Terrain = serde_json::from_value(merged).map_err(|e| format!("terrain settings: {e}"))?;
        t.sanitize();
        self.scene.get_mut(id).expect("checked").terrain = Some(t);
        Ok(())
    }

    /// `edit_terrain`: settings, then brush strokes.
    pub(super) fn edit_terrain(&mut self, args: &Value) -> ToolResult {
        let id = self.resolve(&args["target"])?;
        if let Some(v) = args.get("settings").filter(|v| !v.is_null()) {
            self.merge_terrain(id, v)?;
        }
        let o = self.scene.get(id).ok_or("object vanished")?;
        let mut t = o.terrain.clone().ok_or_else(|| format!("{} is not a Terrain object", o.name))?;
        if args["clear_sculpt"].as_bool().unwrap_or(false) {
            t.sculpt = None;
        }
        if args["clear_paint"].as_bool().unwrap_or(false) {
            t.paint = None;
        }
        let mut changed = 0;
        if let Some(v) = args.get("strokes").filter(|v| !v.is_null()) {
            let mut strokes: Vec<Stroke> = serde_json::from_value(v.clone()).map_err(|e| format!("strokes: {e}"))?;
            if strokes.len() > 512 {
                return Err("at most 512 strokes per call".into());
            }
            if args["space"].as_str() == Some("world") {
                let to_local = self.scene.world_matrix(id).inverse();
                for s in &mut strokes {
                    for p in &mut s.points {
                        let w = match p.len() {
                            2 => Vec3::new(p[0], 0.0, p[1]),
                            3 => Vec3::new(p[0], p[1], p[2]),
                            _ => return Err("points are [x, z] or [x, y, z]".into()),
                        };
                        let l = to_local.transform_point3(w);
                        *p = vec![l.x, l.y, l.z];
                    }
                }
            }
            changed = t.apply(&strokes)?;
        }
        self.scene.get_mut(id).expect("checked").terrain = Some(t);
        let mut out = self.env_summary(id);
        out["vertices_changed"] = json!(changed);
        Ok(super::tools::text(out))
    }

    /// `scatter`: places copies of `sources`.
    pub(super) fn scatter_tool(&mut self, args: &Value) -> ToolResult {
        let existing = match args.get("target").filter(|t| !t.is_null()) {
            Some(t) => {
                let id = self.resolve(t)?;
                let o = self.scene.get(id).ok_or("object vanished")?;
                Some((id, o.scatter.clone().ok_or_else(|| format!("{} is not a scatter", o.name))?))
            }
            None => None,
        };
        let mut sources = Vec::new();
        for t in args["sources"].as_array().into_iter().flatten() {
            let id = self.resolve(t)?;
            let o = self.scene.get(id).ok_or("object vanished")?;
            if !(o.shape.is_mesh() || o.light.is_some()) {
                return Err(format!("{} has no mesh to copy", o.name));
            }
            sources.push(id);
        }
        if sources.is_empty() {
            sources = existing.as_ref().map(|e| e.1.sources.clone()).unwrap_or_default();
        }
        if sources.is_empty() || sources.len() > 64 {
            return Err("sources must list 1 to 64 objects to copy".into());
        }
        // Settings: the scatter's own, then this call's.
        let mut merged = serde_json::to_value(existing.as_ref().map_or_else(Settings::default, |e| e.1.settings.clone())).map_err(|e| e.to_string())?;
        const SETTINGS: [&str; 17] = ["center", "size", "radius", "y", "density", "count", "min_distance", "scale", "yaw", "tilt", "align", "sink", "avoid", "weights", "max_slope", "seed", "on"];
        for (k, v) in args.as_object().into_iter().flatten() {
            if SETTINGS.contains(&k.as_str()) && k != "on" {
                merged[k] = v.clone();
            }
        }
        if args.get("radius").is_some_and(|v| !v.is_null()) && args.get("size").is_none() {
            merged.as_object_mut().expect("object").remove("size");
        }
        let mut settings: Settings = serde_json::from_value(merged).map_err(|e| format!("scatter settings: {e}"))?;
        settings.sanitize()?;
        // Keep copies away from these objects' footprints.
        for t in args["avoid_objects"].as_array().into_iter().flatten() {
            let id = self.resolve(t)?;
            self.meshes.sync(&self.scene);
            let (lo, hi) = self.world_bounds_of(id).ok_or_else(|| format!("{t} has nothing to avoid"))?;
            let c = (lo + hi) * 0.5;
            let r = Vec2::new(hi.x - lo.x, hi.z - lo.z).length() * 0.5 + settings.min_distance * 0.5;
            settings.avoid.push([c.x, c.z, r]);
        }
        // Surfaces to drop onto: these objects (and copies they scatter).
        let mut on = Vec::new();
        for t in args["on"].as_array().into_iter().flatten() {
            on.push(self.resolve(t)?);
        }
        self.meshes.sync(&self.scene);
        let ground = if on.is_empty() {
            None
        } else {
            let eval = self.scene.evaluate();
            let mut tris = Vec::new();
            let mut ids: Vec<u64> = on.clone();
            for &id in &on {
                ids.extend(self.scene.descendants(id));
            }
            for d in scatter::draws(&self.scene, &eval) {
                let owner = if d.scattered { None } else { Some(d.object.id) };
                let from_scatter = self.scene.objects.iter().any(|s| ids.contains(&s.id) && s.scatter.as_ref().is_some_and(|sc| sc.sources.contains(&d.object.id)) && d.scattered);
                if !(owner.is_some_and(|o| ids.contains(&o)) || from_scatter) {
                    continue;
                }
                let Some(m) = self.meshes.get(d.object) else { continue };
                for t in m.indices.chunks_exact(3) {
                    tris.push([0, 1, 2].map(|i| d.world.transform_point3(Vec3::from(m.vertices[t[i] as usize].pos))));
                }
            }
            if tris.is_empty() {
                return Err("the on objects have no surface to scatter onto".into());
            }
            if tris.len() > 4_000_000 {
                return Err("the on surfaces have too many triangles".into());
            }
            Some(Ground::new(tris))
        };
        let y = settings.y;
        let placed = scatter::place(&settings, sources.len(), &|p: Vec2| match &ground {
            Some(g) => g.top(p),
            None => Some((y, Vec3::Y)),
        });
        let append = args["mode"].as_str() == Some("add");
        let id = match &existing {
            Some((id, _)) => *id,
            None => {
                let id = self.scene.add(Shape::Scatter);
                let name = args["name"].as_str().map(str::to_string).unwrap_or_else(|| self.unique_name("scatter"));
                self.scene.get_mut(id).expect("just added").name = name;
                id
            }
        };
        // Instances are in world space: the scatter object stays at the origin.
        let to_local = self.scene.world_matrix(id).inverse();
        let local: Vec<scatter::Instance> = placed.instances.iter().map(|i| {
            let m = to_local * i.matrix();
            let (s, r, t) = m.to_scale_rotation_translation();
            scatter::Instance { source: i.source, position: t, rotation: r, scale: s.x }
        }).collect();
        let o = self.scene.get_mut(id).expect("exists");
        let mut sc = if append { o.scatter.clone().unwrap_or_default() } else { Scatter::default() };
        // Appending with other sources: renumber the new copies' sources.
        let mut map = Vec::new();
        for s in &sources {
            map.push(match sc.sources.iter().position(|x| x == s) {
                Some(i) => i as u32,
                None => {
                    sc.sources.push(*s);
                    sc.sources.len() as u32 - 1
                }
            });
        }
        sc.instances.extend(local.into_iter().map(|mut i| {
            i.source = map[i.source as usize];
            i
        }));
        sc.instances.truncate(scatter::MAX_INSTANCES);
        sc.settings = settings;
        o.scatter = Some(sc);
        if let Some(n) = args["name"].as_str() {
            o.name = n.to_string();
        }
        if args["hide_sources"].as_bool().unwrap_or(true) {
            for s in &sources {
                if let Some(src) = self.scene.get_mut(*s) {
                    src.visible = false;
                }
            }
        }
        let mut out = self.env_summary(id);
        out["placed"] = json!(placed.instances.len());
        out["wanted"] = json!(placed.wanted);
        if placed.instances.len() < placed.wanted {
            out["fewer_because"] = json!({
                "too_close": placed.crowded, "in_avoided_areas": placed.avoided, "off_the_surfaces": placed.missed, "too_steep": placed.steep,
                "hint": "lower min_distance or density, widen the area, or add surfaces with on",
            });
        }
        Ok(super::tools::text(out))
    }

    /// World bounds of one object (and what is under it).
    fn world_bounds_of(&self, id: u64) -> Option<(Vec3, Vec3)> {
        let bounds = |o: &crate::scene::Object| self.meshes.bounds(o);
        self.scene.subtree_bounds(id, self.scene.world_matrix(id), &bounds)
    }

    fn unique_name(&self, base: &str) -> String {
        (1..).map(|n| format!("{base}{n}")).find(|n| !self.scene.objects.iter().any(|o| &o.name == n)).expect("unbounded")
    }

    /// What an environment object holds: piece, size, triangles, copies, light.
    pub(super) fn env_summary(&mut self, id: u64) -> Value {
        self.meshes.sync(&self.scene);
        let Some(o) = self.scene.get(id) else { return Value::Null };
        let mut out = super::tools::object_json_in(&self.scene, id);
        let triangles = |m: Option<&std::sync::Arc<crate::mesh::Mesh>>| m.map_or(0, |m| m.indices.len() / 3);
        if let Some(k) = &o.kit {
            out["kit"] = serde_json::to_value(k).unwrap_or_default();
            out["triangles"] = json!(triangles(self.meshes.get(o)));
        }
        if let Some(t) = &o.terrain {
            let mut settings = serde_json::to_value(t).unwrap_or_default();
            if let Some(m) = settings.as_object_mut() {
                m.remove("sculpt");
                m.remove("paint");
            }
            let (nx, nz) = t.cells();
            out["terrain"] = settings;
            out["cells"] = json!([nx, nz]);
            out["sculpted"] = json!(t.sculpt.is_some());
            out["painted"] = json!(t.paint.is_some());
            out["triangles"] = json!(triangles(self.meshes.get(o)));
        }
        if let Some(sc) = &o.scatter {
            let per: Vec<usize> = sc.sources.iter().map(|&s| self.scene.get(s).map_or(0, |src| triangles(self.meshes.get(src)))).collect();
            let total: usize = sc.instances.iter().map(|i| per.get(i.source as usize).copied().unwrap_or(0)).sum();
            let lit = sc.instances.iter().filter(|i| sc.sources.get(i.source as usize).and_then(|&s| self.scene.get(s)).is_some_and(|s| s.light.is_some())).count();
            out["copies"] = json!(sc.instances.len());
            out["sources"] = json!(sc.sources.iter().map(|&s| self.scene.get(s).map(|o| o.name.clone())).collect::<Vec<_>>());
            out["triangles_drawn"] = json!(total);
            out["unique_triangles"] = json!(per.iter().sum::<usize>());
            out["lights"] = json!(lit);
            out["settings"] = serde_json::to_value(&sc.settings).unwrap_or_default();
        }
        if let Some(b) = self.world_bounds_of(id) {
            out["world_bounds"] = json!({ "min": b.0.to_array(), "max": b.1.to_array() });
        }
        tidy(out)
    }

    /// What the whole scene costs to draw: triangles (copies counted),
    /// copies, and lights.
    pub(super) fn budget_json(&self) -> Value {
        let eval = self.scene.evaluate();
        let draws = scatter::draws(&self.scene, &eval);
        let triangles: usize = draws.iter().filter(|d| d.object.shape.is_mesh()).filter_map(|d| self.meshes.get(d.object)).map(|m| m.indices.len() / 3).sum();
        let unique: usize = self.scene.objects.iter().filter(|o| o.shape.is_mesh()).filter_map(|o| self.meshes.get(o)).map(|m| m.indices.len() / 3).sum();
        json!({
            "triangles_drawn": triangles,
            "unique_triangles": unique,
            "copies": draws.iter().filter(|d| d.scattered).count(),
            "lights": draws.iter().filter(|d| d.object.light.is_some()).count(),
        })
    }
}

/// Rounds every number to 4 decimals (f32 settings print as 0.6000000238).
fn tidy(v: Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => json!((n.as_f64().unwrap_or(0.0) * 1e4).round() / 1e4),
        Value::Array(a) => Value::Array(a.into_iter().map(tidy).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, tidy(v))).collect()),
        other => other,
    }
}

fn is_architecture(p: Piece) -> bool {
    matches!(p, Piece::Wall | Piece::Corner | Piece::Doorway | Piece::Pillar | Piece::Stairs | Piece::Floor | Piece::Vault)
}

/// World matrix of a kit placement (for tests).
#[cfg(test)]
pub fn placed(at: Vec3, turn: f32, step: f32) -> glam::Mat4 {
    glam::Mat4::from_rotation_translation(Quat::from_rotation_y(snap(turn, if step > 0.0 { 90.0 } else { 0.0 }).to_radians()), Vec3::new(snap(at.x, step), at.y, snap(at.z, step)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_snaps_to_the_grid_and_quarter_turns() {
        let m = placed(Vec3::new(1.23, 1.9, -3.74), 87.0, 0.5);
        let (_, r, t) = m.to_scale_rotation_translation();
        assert_eq!(t, Vec3::new(1.0, 1.9, -3.5));
        assert!(r.angle_between(Quat::from_rotation_y(90f32.to_radians())) < 1e-3);
        assert_eq!(snap(2.26, 0.0), 2.26);
        // Two 2 m walls side by side on grid points meet end to end.
        let w = Kit::new(Piece::Wall).build(&[], None);
        let (lo, hi) = w.bounds();
        let a = placed(Vec3::new(-1.1, 0.0, 0.1), 2.0, 0.5).transform_point3(Vec3::new(hi.x, 0.0, 0.0));
        let b = placed(Vec3::new(0.9, 0.0, -0.2), -3.0, 0.5).transform_point3(Vec3::new(lo.x, 0.0, 0.0));
        assert!(a.distance(b) < 0.03, "{a} {b}");
    }
}
