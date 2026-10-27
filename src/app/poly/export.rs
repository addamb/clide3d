//! `export_poly` (docs/QUALITY_PLAN.md Stage 3d): a Poly's subdivision
//! cage and maps as files (`crate::poly::export`), run as a job.
//!
//! Everything is checked from the arguments and the counts before the
//! job starts: the levels by the shown-face bound, the map size, the map
//! level against the projected levels, UVs and a source clay for maps, a
//! placement maps can be written in, and every output path (nothing is
//! overwritten that this writer did not make).

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::jobs::{Finish, Started};
use super::super::tools::{object_json_in, text};
use crate::outputs::Format;
use crate::poly::export::{Geometry, MAP_SIZES, MapKind, MapSource, Placement, Request, Space};
use crate::poly::{MAX_LEVELS, MAX_SHOWN_FACES, Poly};

/// The finest level holding projected detail (0 without).
fn displaced(p: &Poly) -> u8 {
    p.levels.iter().rposition(|l| l.displacement.is_some()).map_or(0, |k| k as u8 + 1)
}

fn level_arg(args: &Value, key: &str) -> Result<Option<u8>, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_u64().filter(|n| *n <= MAX_LEVELS as u64).map(|n| Some(n as u8)).ok_or_else(|| format!("{key} must be a whole number from 0 to {MAX_LEVELS}")),
    }
}

/// A checked request from the arguments and the Poly (no files touched).
pub(in crate::app) fn parse(args: &Value, p: &Poly) -> Result<Request, String> {
    let path = PathBuf::from(args["path"].as_str().ok_or("path is required (an absolute path ending in .obj, .glb or .usda)")?);
    let geometry = Geometry::of(&path)?;
    if !path.is_absolute() {
        return Err("path must be an absolute file path ending in .obj, .glb or .usda".into());
    }
    let udim = match args.get("udim").filter(|v| !v.is_null()) {
        None => false,
        Some(v) => v.as_bool().ok_or("udim must be true or false")?,
    };
    if udim && geometry == Geometry::Glb {
        return Err("glTF has no UDIM tiles: write .obj or .usda with udim: true (the tiles go beside it)".into());
    }
    if udim && p.uv.is_none() {
        return Err("udim: true writes texture tiles and per-tile maps: run poly uv (udim: true) first".into());
    }
    let spans = p.uv.as_ref().is_some_and(|u| crate::poly::uv::spans_tiles(&u.to_vec()));
    let level = level_arg(args, "level")?.unwrap_or(if geometry == Geometry::Glb { p.level } else { 0 });
    if p.faces_at(level) > MAX_SHOWN_FACES {
        return Err(format!("level {level} has {} faces; export_poly writes at most {MAX_SHOWN_FACES} (a lower level)", p.faces_at(level)));
    }
    let maps: Vec<MapKind> = match args.get("maps").filter(|v| !v.is_null()) {
        // UDIM: color, normal and ao live in the Poly's tiles (bake udim).
        None if udim => if displaced(p) > 0 { vec![MapKind::VectorDisplacement] } else { vec![] },
        None if spans => vec![],
        None => vec![MapKind::VectorDisplacement, MapKind::Normal],
        Some(Value::Array(list)) if list.len() <= MapKind::ALL.len() => {
            let mut out = Vec::new();
            for m in list {
                let k = m.as_str().and_then(MapKind::parse).ok_or_else(|| format!("unknown map {m}: use vector_displacement, displacement, normal, color or ao"))?;
                if out.contains(&k) {
                    return Err(format!("map {} is listed twice", k.name()));
                }
                out.push(k);
            }
            out.sort();
            out
        }
        Some(_) => return Err("maps must list vector_displacement, displacement, normal, color and/or ao (or be empty)".into()),
    };
    if udim && maps.iter().any(|k| matches!(k, MapKind::Normal | MapKind::Color | MapKind::Ao)) {
        return Err("with udim: true the color, normal and ao tiles come from the Poly's texture tiles (bake udim: true transfers them from the clay, paint adds to them); maps lists only vector_displacement and displacement".into());
    }
    if !udim && spans && !maps.is_empty() {
        return Err("the Poly's UVs are on several UDIM tiles: one map cannot hold them; send udim: true (a file per tile)".into());
    }
    let projected = displaced(p);
    let map_level = match level_arg(args, "map_level")? {
        Some(m) => m,
        // A .glb's maps are textures on the level it holds.
        None if geometry == Geometry::Glb => level,
        None => (if projected > 0 { projected } else { p.level }).max(level),
    };
    if !maps.is_empty() {
        if map_level < level {
            return Err(format!("map_level {map_level} is below the written level {level}: maps sit on the written level or a finer one"));
        }
        if projected > 0 && map_level > projected.max(level) {
            return Err(format!("map_level {map_level} is past the {projected} projected levels (poly project more levels first)"));
        }
        if projected == 0 && map_level > p.level_count().max(level) {
            return Err(format!("map_level {map_level} is past the {} levels kept (poly subdivide, or poly project)", p.level_count()));
        }
        if p.faces_at(map_level) > MAX_SHOWN_FACES {
            return Err(format!("map_level {map_level} has {} faces; maps are baked over at most {MAX_SHOWN_FACES} (a lower map_level)", p.faces_at(map_level)));
        }
        if geometry == Geometry::Glb && map_level != level {
            return Err(format!("a .glb's maps are textures on the mesh it holds: map_level must equal level ({level}), or leave it out"));
        }
    }
    let map_size = match args.get("map_size").filter(|v| !v.is_null()) {
        None => 2048,
        // Checked as the number given, before any narrowing cast.
        Some(v) => v.as_u64().filter(|s| MAP_SIZES.iter().any(|m| *m as u64 == *s)).ok_or(format!("map_size must be one of {MAP_SIZES:?}"))? as u32,
    };
    let (vector_format, height_format) = match args.get("map_format").filter(|v| !v.is_null()) {
        None => (Format::Exr, Format::Png),
        Some(v) => {
            let f = Format::parse(v, "map_format", Format::Exr)?;
            (f, f)
        }
    };
    let space = match args["space"].as_str() {
        None | Some("tangent") => Space::Tangent,
        Some("object") => Space::Object,
        Some(other) => return Err(format!("space must be tangent or object, not {other}")),
    };
    let udim = udim.then(|| {
        let uv = p.uv.as_ref().map(|u| u.to_vec()).unwrap_or_default();
        let mut tiles: Vec<u16> = uv.iter().filter_map(|t| crate::poly::texture::tile_of(*t)).collect();
        tiles.sort_unstable();
        tiles.dedup();
        let textures = p.paint.as_ref().map(|paint| paint.textures.channels().into_iter().map(|c| (c, paint.textures.resolution(c).unwrap_or(0), paint.textures.tiles(c))).collect()).unwrap_or_default();
        crate::poly::export::UdimFiles { tiles, textures }
    });
    Ok(Request { path, geometry, level, maps, map_level, map_size, vector_format, height_format, space, verify: args["verify"].as_bool().unwrap_or(true), udim })
}

impl ClideApp {
    /// `export_poly` (a job, not an editing one): the files of a Poly's
    /// level and its maps, then the check from disk.
    pub(in crate::app) fn export_poly(&mut self, args: &Value) -> Result<Started, String> {
        let (id, p) = self.poly_of(args)?;
        let poly = p.clone();
        let request = parse(args, &poly)?;
        // Paths are refused before anything settles or bakes; the job
        // checks each file again as it writes it (`outputs::write_over`).
        request.check_paths()?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        let (name, own) = (o.name.clone(), self.scene.material_of(o));
        let world = self.scene.world_matrix(id);
        // Maps: UVs, the clay (settled), and a placement they fit.
        let mut source = None;
        let mut settled = false;
        let mut inputs = None;
        if !request.maps.is_empty() {
            if poly.uv.is_none() {
                return Err("maps need UVs on the Poly: run poly uv first (or send maps: [] for the geometry alone)".into());
            }
            let clay = match args.get("source").filter(|v| !v.is_null()) {
                Some(v) => self.resolve(v)?,
                None => [poly.projected.as_deref(), poly.made_by.as_deref()]
                    .into_iter()
                    .flatten()
                    .find_map(|m| m["source"].as_u64().filter(|s| self.scene.get(*s).is_some_and(|o| o.clay.is_some())))
                    .ok_or("maps are baked from a clay: send source (a Clay object), as this Poly records none (or send maps: [])")?,
            };
            let c = self.scene.get(clay).ok_or("object vanished")?;
            if c.clay.is_none() {
                return Err(format!("{} is not a Clay object (maps read a clay's surface)", c.name));
            }
            let place = Placement::of(self.scene.world_matrix(clay).inverse() * world)?;
            // The composed field is read: unchecked ledgers settle first.
            let unsettled = c.clay.clone();
            self.settle_clays([clay])?;
            settled = self.scene.get(clay).and_then(|o| o.clay.as_ref()) != unsettled.as_ref();
            self.meshes.sync(&self.scene);
            let settings = crate::bake::Settings { texture: request.map_size, ..crate::bake::Settings::default() };
            inputs = Some((self.meshes.bake_inputs(&self.scene, clay, settings)?, place));
            source = Some(clay);
        }
        // What udim: true would add, said when it is left out.
        let mut aside = Vec::new();
        if request.udim.is_none() {
            if poly.paint.as_ref().is_some_and(|p| !p.textures.is_empty()) {
                aside.push("the Poly's texture tiles are written with udim: true".to_string());
            }
            if poly.uv.as_ref().is_some_and(|u| crate::poly::uv::spans_tiles(&u.to_vec())) && request.maps.is_empty() {
                aside.push("its UVs span UDIM tiles: maps are written per tile with udim: true".to_string());
            }
        }
        let label = format!("Export {} as {}", name, request.path.file_name().and_then(|f| f.to_str()).unwrap_or("files"));
        Ok(Started::Job(label, Box::new(move |progress| {
            let maps = inputs.as_ref().map(|(job, place)| MapSource { src: job.source(None), place: *place });
            let reply = crate::poly::export::export(&poly, &name, world, &own, &request, maps.as_ref(), progress)?;
            Ok(Box::new(move |app: &mut ClideApp| {
                let mut reply = reply;
                reply["target"] = json!(id);
                reply["object"] = object_json_in(&app.scene, id);
                reply["source"] = json!(source);
                reply["settled"] = json!(settled);
                if !aside.is_empty()
                    && let Some(n) = reply["notes"].as_array_mut()
                {
                    n.extend(aside.into_iter().map(Value::from));
                }
                app.status = format!("Exported {}", request.path.display());
                Ok(text(reply))
            }) as Finish)
        })))
    }

    /// File > Export Subdivision Cage…: `export_poly` of the lead Poly to
    /// `path`, as an agent sends it (the dialog picks the path).
    pub(in crate::app) fn export_lead_poly(&mut self, path: PathBuf) {
        if let Some(id) = self.lead().filter(|id| self.scene.get(*id).is_some_and(|o| o.poly.is_some())) {
            self.send_ui_call("export_poly", json!({ "target": id, "path": path.display().to_string() }));
        }
    }

    /// The File menu's save dialog for `export_lead_poly`.
    pub(in crate::app) fn export_poly_dialog(&mut self) {
        let Some(name) = self.lead().and_then(|id| self.scene.get(id)).filter(|o| o.poly.is_some()).map(|o| crate::outputs::file_stem(&o.name)) else { return };
        let Some(path) = rfd::FileDialog::new().add_filter("OBJ cage", &["obj"]).add_filter("glTF binary", &["glb"]).add_filter("USD (text)", &["usda"]).set_file_name(format!("{name}.obj")).save_file() else { return };
        self.export_lead_poly(path);
    }
}
