//! Stage 5 tools: `import_mesh`, `to_clay` (Make PolyMesh3D, DynaMesh,
//! Remesh All and booleans across objects) and `export_print`, and the
//! menu entries that send them (File > Import…, File > Export for
//! Print…, Modify > Remesh as Clay…). The menus build the same requests
//! an agent sends and put them in the same line (`send_ui_call`), so
//! they run through these handlers and show in `ui_activity`.

use std::path::PathBuf;

use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::{Finish, Started};
use super::tools::{object_json_in, text};
use crate::clay::{Blob, Clay, Form, MAX_BLOBS, MAX_RESOLUTION, MIN_RESOLUTION, Op};
use crate::mesh_asset::{self, MAX_FIELD_TRIANGLES, MeshAsset, Sign};
use crate::mesh_import::{self, Format};
use crate::scene::{Object, Shape};

#[cfg(test)]
mod tests;

fn round(x: f32) -> f64 {
    (x as f64 * 1e5).round() / 1e5
}

fn round3(v: Vec3) -> [f64; 3] {
    v.to_array().map(round)
}

/// A number argument within `lo..=hi` (None when absent).
fn number(args: &Value, key: &str, lo: f32, hi: f32) -> Result<Option<f32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(|x| x as f32).filter(|x| x.is_finite() && (lo..=hi).contains(x)).map(Some).ok_or_else(|| format!("{key} must be a number in {lo}..{hi}")),
    }
}

fn resolution(args: &Value) -> Result<Option<u32>, String> {
    Ok(number(args, "resolution", MIN_RESOLUTION as f32, MAX_RESOLUTION as f32)?.map(|r| r.round() as u32))
}

/// Boundary loops of a welded mesh (edges one triangle uses), each as
/// its corners in order; edges that do not chain into loops are skipped.
fn boundary_loops(tris: &[[u32; 3]]) -> Vec<Vec<u32>> {
    use std::collections::{HashMap, HashSet};
    let mut directed: HashSet<(u32, u32)> = HashSet::with_capacity(tris.len() * 3);
    for t in tris {
        for k in 0..3 {
            directed.insert((t[k], t[(k + 1) % 3]));
        }
    }
    // A boundary edge a -> b has no b -> a; the hole runs b -> a.
    let mut next: HashMap<u32, u32> = HashMap::new();
    let mut bad = HashSet::new();
    for &(a, b) in &directed {
        if !directed.contains(&(b, a)) && next.insert(b, a).is_some() {
            bad.insert(b);
        }
    }
    let mut loops = Vec::new();
    let mut seen = HashSet::new();
    let mut starts: Vec<u32> = next.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        if seen.contains(&s) || bad.contains(&s) {
            continue;
        }
        let mut lp = vec![s];
        seen.insert(s);
        let mut cur = s;
        let closed = loop {
            let Some(&n) = next.get(&cur) else { break false };
            if n == s {
                break true;
            }
            if !seen.insert(n) || bad.contains(&n) || lp.len() > 1 << 20 {
                break false;
            }
            lp.push(n);
            cur = n;
        };
        if closed && lp.len() >= 3 {
            loops.push(lp);
        }
    }
    loops
}

/// Fills each boundary loop with a fan around its middle; returns how
/// many holes were filled.
fn fill_holes(points: &mut Vec<Vec3>, tris: &mut Vec<[u32; 3]>) -> usize {
    let loops = boundary_loops(tris);
    for lp in &loops {
        let mid = lp.iter().map(|&i| points[i as usize]).sum::<Vec3>() / lp.len() as f32;
        let c = points.len() as u32;
        points.push(mid);
        for k in 0..lp.len() {
            tris.push([lp[k], lp[(k + 1) % lp.len()], c]);
        }
    }
    loops.len()
}

/// One object to print: name, world matrix, scene units per unit of its
/// own (an even scale; 1 for meshes) and its mesh or clay.
type Part = (String, Mat4, f32, Result<std::sync::Arc<crate::mesh::Mesh>, Clay>);

/// `export_print`'s triangle limit: by default, and the range it may be set to.
const DEFAULT_PRINT_TRIANGLES: u64 = 20_000_000;
const MIN_PRINT_TRIANGLES: u64 = 1_000;
const MAX_PRINT_TRIANGLES: u64 = 50_000_000;
/// Pinched sheets move apart by the larger of this many millimeters (above
/// slicers' weld tolerance, about 1e-4 mm) and this share of the edges there.
const PINCH_MIN_MM: f32 = 0.005;
const PINCH_CELL_SHARE: f32 = 0.02;

/// A print mesh simplified by meshopt to within `error_mm` (absolute, the
/// mesh is in millimeters), unused points dropped; None when it would not
/// be watertight (every edge in two triangles, opposite ways) or nothing
/// was removed.
fn simplified(points: &[Vec3], tris: &[[u32; 3]], error_mm: f32) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let indices: Vec<u32> = tris.iter().flatten().copied().collect();
    let adapter = meshopt::VertexDataAdapter::new(bytemuck::cast_slice(points), 12, 0).ok()?;
    let out = meshopt::simplify(&indices, &adapter, 0, error_mm, meshopt::SimplifyOptions::ErrorAbsolute, None);
    if out.is_empty() || out.len() >= indices.len() {
        return None;
    }
    let mut remap = vec![u32::MAX; points.len()];
    let mut kept = Vec::new();
    let tris: Vec<[u32; 3]> = out
        .chunks_exact(3)
        .map(|t| {
            [t[0], t[1], t[2]].map(|i| {
                let slot = &mut remap[i as usize];
                if *slot == u32::MAX {
                    *slot = kept.len() as u32;
                    kept.push(points[i as usize]);
                }
                *slot
            })
        })
        .collect();
    let (edges, bad, flipped) = mesh_asset::edge_use(&tris);
    (edges > 0 && bad == 0 && flipped == 0).then_some((kept, tris))
}

/// Whether a matrix's axes are at right angles (no shear).
fn orthogonal(m: Mat4) -> bool {
    let [x, y, z] = [m.x_axis.truncate(), m.y_axis.truncate(), m.z_axis.truncate()];
    let ok = |a: Vec3, b: Vec3| a.dot(b).abs() <= 1e-4 * a.length() * b.length();
    ok(x, y) && ok(y, z) && ok(x, z)
}

/// What the Remesh as Clay… dialog holds (editor state).
#[derive(Clone, Debug)]
pub(super) struct RemeshDialog {
    pub target: u64,
    pub resolution: u32,
    pub blur: f32,
    pub polish: f32,
}

impl RemeshDialog {
    /// The `to_clay` request the dialog sends: what an agent would.
    pub fn request(&self) -> Value {
        let mut r = json!({ "target": self.target, "mode": "volume", "resolution": self.resolution });
        if self.blur > 0.0 {
            r["blur"] = json!(self.blur);
        }
        if self.polish > 0.0 {
            r["polish"] = json!(self.polish);
        }
        r
    }
}

impl ClideApp {
    /// `import_mesh`: a file as a clay (a mesh blob, decimated to
    /// `MAX_FIELD_TRIANGLES`) or a mesh object.
    pub(super) fn import_mesh(&mut self, args: &Value) -> Result<Started, String> {
        let path = PathBuf::from(args["path"].as_str().ok_or("path is required (an absolute path to .obj, .stl, .ply, .glb or .fbx)")?);
        if !path.is_absolute() {
            return Err("path must be absolute".into());
        }
        if args["as"].as_str() == Some("poly") {
            return self.import_poly(args, path);
        }
        if args.get("udim").is_some_and(|v| !v.is_null()) {
            return Err("udim is for as: poly (a Poly's UV layout)".into());
        }
        let format = Format::of(&path)?;
        let as_clay = match args["as"].as_str().unwrap_or("clay") {
            "clay" => true,
            "mesh" => false,
            other => return Err(format!("as must be clay, mesh or poly, not {other}")),
        };
        let scale = number(args, "scale", 1e-6, 1e6)?.unwrap_or(1.0);
        let z_up = match args["up"].as_str().unwrap_or("y") {
            "y" => false,
            "z" => true,
            other => return Err(format!("up must be y or z, not {other}")),
        };
        let center = args["center"].as_bool().unwrap_or(false);
        let close_holes = args["close_holes"].as_bool().unwrap_or(true);
        let res = resolution(args)?;
        if res.is_some() && !as_clay {
            return Err("resolution is for as: clay".into());
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("import").chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').take(48).collect::<String>();
        let name = args["name"].as_str().map(str::to_string).filter(|n| !n.trim().is_empty()).unwrap_or(if stem.is_empty() { "import".into() } else { stem });
        if name.chars().count() > 64 {
            return Err("name is at most 64 characters".into());
        }
        let label = format!("Import {}", path.file_name().and_then(|f| f.to_str()).unwrap_or("mesh"));
        Ok(Started::Job(label, Box::new(move |progress| {
            let soup = mesh_import::read(&path)?;
            progress.check()?;
            progress.set(0.3);
            let triangles = soup.tris.len();
            let turn = if z_up { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) } else { Quat::IDENTITY };
            let mut points: Vec<Vec3> = soup.points.iter().map(|p| turn * (*p * scale)).collect();
            if center && !points.is_empty() {
                let (lo, hi) = points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
                let mid = (lo + hi) * 0.5;
                points.iter_mut().for_each(|p| *p -= mid);
            }
            let mut tris = soup.tris;
            let watertight = mesh_asset::watertight(&points, &tris);
            let mut warnings = soup.warnings;
            let holes = if watertight { 0 } else { boundary_loops(&tris).len() };
            let full = MeshAsset::new(points, tris.clone())?;
            let (lo, hi) = full.bounds();
            let (shape, clay, mesh, kept, holes_closed) = if as_clay {
                if !watertight && !close_holes {
                    return Err(format!("the mesh is not closed ({holes} holes, or flipped or non-manifold edges); as clay its holes close by the winding number: send close_holes true, or import as mesh"));
                }
                let asset = full.decimated(MAX_FIELD_TRIANGLES)?;
                // What a scene load accepts, so a saved scene reads back.
                mesh_asset::check_clay_assets([&asset])?;
                if asset.tris().len() < triangles {
                    warnings.push(format!("decimated from {triangles} to {} triangles for the clay's distance field", asset.tris().len()));
                }
                let sign = if mesh_asset::watertight(asset.points(), asset.tris()) { Sign::Closed } else { Sign::Winding };
                progress.set(0.6);
                // Built here, so the first mesh build does not wait for it.
                asset.field();
                let mut clay = Clay { blobs: vec![Blob::of(Form::Mesh { asset: asset.hash().to_string(), sign, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 }, Op::Add, [0.8; 3])], ..Clay::default() };
                if let Some(r) = res {
                    clay.resolution = r;
                }
                let kept = asset.tris().len();
                clay.assets.insert(asset.hash().to_string(), asset);
                (Shape::Clay, Some(clay), None, kept, if sign == Sign::Winding { holes } else { 0 })
            } else if !watertight && close_holes && holes > 0 {
                let mut points = full.points().to_vec();
                let filled = fill_holes(&mut points, &mut tris);
                let asset = MeshAsset::new(points, tris)?;
                (Shape::Mesh, None, Some(asset), triangles, filled)
            } else {
                (Shape::Mesh, None, Some(full), triangles, 0)
            };
            Ok(Box::new(move |app: &mut ClideApp| {
                let id = app.scene.add(shape);
                let o = app.scene.get_mut(id).expect("just added");
                (o.clay, o.mesh) = (clay, mesh);
                let taken = |n: &str| app.scene.objects.iter().any(|o| o.id != id && o.name == n);
                let unique = if taken(&name) { (1..).map(|n| format!("{name}{n}")).find(|n| !taken(n)).expect("unbounded") } else { name };
                if let Some(o) = app.scene.get_mut(id) {
                    o.name = unique;
                }
                app.select_only(id);
                Ok(text(json!({
                    "object": object_json_in(&app.scene, id),
                    "format": format!("{format:?}").to_lowercase(),
                    "as": if shape == Shape::Clay { "clay" } else { "mesh" },
                    "triangles": triangles,
                    "triangles_kept": kept,
                    "watertight": watertight,
                    "holes": holes,
                    "holes_closed": holes_closed,
                    "bounds": { "min": round3(lo), "max": round3(hi) },
                    "warnings": warnings,
                })))
            }) as Finish)
        })))
    }

    /// `import_mesh as: poly`: an OBJ's polygons as a Poly object, faces
    /// kept as they are (quads, n-gons), a UV per corner from `vt`, and
    /// creases from `t crease 2/1/0 i j sharpness` tags (what `export_poly`
    /// writes, so its cage comes back the same).
    fn import_poly(&mut self, args: &Value, path: PathBuf) -> Result<Started, String> {
        // .obj (any polygon file), or the cage export_poly writes into a
        // .glb (extras.clide3d_cage) or a .usda.
        let format = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("obj") => crate::poly::export::Geometry::Obj,
            Some("glb") => crate::poly::export::Geometry::Glb,
            Some("usda") => crate::poly::export::Geometry::Usda,
            _ => return Err("as poly reads .obj (polygons, UVs and t crease tags), or the .glb and .usda export_poly writes".into()),
        };
        for key in ["resolution", "close_holes"] {
            if !args[key].is_null() {
                return Err(format!("{key} is not for as: poly (a Poly keeps the file's faces)"));
            }
        }
        let scale = number(args, "scale", 1e-6, 1e6)?;
        let z_up = match args["up"].as_str().unwrap_or("y") {
            "y" => false,
            "z" => true,
            other => return Err(format!("up must be y or z, not {other}")),
        };
        let center = args["center"].as_bool().unwrap_or(false);
        let textures = match args.get("textures").filter(|v| !v.is_null()) {
            None => true,
            Some(v) => v.as_bool().ok_or("textures must be true or false")?,
        };
        let udim = match args.get("udim").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_bool().ok_or("udim must be true or false")?),
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("import").chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').take(48).collect::<String>();
        let name = args["name"].as_str().map(str::to_string).filter(|n| !n.trim().is_empty()).unwrap_or(if stem.is_empty() { "import".into() } else { stem });
        if name.chars().count() > 64 {
            return Err("name is at most 64 characters".into());
        }
        let label = format!("Import {} as a Poly", path.file_name().and_then(|f| f.to_str()).unwrap_or("mesh"));
        Ok(Started::Job(label, Box::new(move |progress| {
            let len = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.len();
            if len > mesh_import::MAX_FILE_BYTES {
                return Err(format!("{}: {} MiB is more than the {} MiB import limit", path.display(), len >> 20, mesh_import::MAX_FILE_BYTES >> 20));
            }
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            progress.check()?;
            let read = match format {
                crate::poly::export::Geometry::Obj => mesh_import::read_cage_as(&bytes, udim),
                crate::poly::export::Geometry::Glb => crate::poly::export::check_glb(&bytes, progress).and_then(|c| c.cage.map(|c| (c, Vec::new())).ok_or("the .glb holds no subdivision cage (export_poly writes one in its mesh's extras)".into())),
                crate::poly::export::Geometry::Usda => crate::poly::export::usda::read_as(&bytes, udim).map(|c| (c, Vec::new())),
            };
            let (mut cage, warnings) = read.map_err(|e| format!("{}: {e}", path.display()))?;
            progress.set(0.5);
            // Points are changed only when asked, so a file comes back bit
            // for bit by default.
            if let Some(k) = scale {
                cage.points.iter_mut().for_each(|p| *p *= k);
            }
            if z_up {
                let turn = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
                cage.points.iter_mut().for_each(|p| *p = turn * *p);
            }
            if center {
                let (lo, hi) = cage.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
                let mid = (lo + hi) * 0.5;
                cage.points.iter_mut().for_each(|p| *p -= mid);
            }
            let mut poly = cage.to_poly().map_err(|e| format!("{}: {e}", path.display()))?;
            progress.check()?;
            // UDIM texture tiles an export_poly sidecar lists come back with
            // the cage (texels exactly as written).
            let mut texture_tiles = serde_json::Map::new();
            if textures && poly.uv.is_some()
                && let Some(t) = crate::poly::export::read_textures(&path, progress)?
                && !t.is_empty()
            {
                for c in t.channels() {
                    texture_tiles.insert(c.name().into(), json!({ "resolution": t.resolution(c), "tiles": t.tiles(c) }));
                }
                poly.paint = Some(std::sync::Arc::new(crate::poly::texture::paint::PolyPaint { textures: t, records: Vec::new(), imported: true }));
            }
            let info = poly.info();
            Ok(Box::new(move |app: &mut ClideApp| {
                let id = app.scene.add(Shape::Poly);
                let taken = |n: &str| app.scene.objects.iter().any(|o| o.id != id && o.name == n);
                let unique = if taken(&name) { (1..).map(|n| format!("{name}{n}")).find(|n| !taken(n)).expect("unbounded") } else { name };
                let o = app.scene.get_mut(id).expect("just added");
                o.name = unique;
                o.poly = Some(poly);
                app.select_only(id);
                Ok(text(json!({
                    "object": object_json_in(&app.scene, id),
                    "format": format.name(),
                    "as": "poly",
                    "poly": info,
                    "texture_tiles": texture_tiles,
                    "warnings": warnings,
                })))
            }) as Finish)
        })))
    }

    /// `to_clay`: objects as one clay, converted in place (the first
    /// target, or `into`); the other targets are hidden.
    pub(super) fn to_clay(&mut self, args: &Value) -> Result<Started, String> {
        let mut targets = Vec::new();
        if let Some(t) = args.get("target").filter(|t| !t.is_null()) {
            targets.push(self.resolve(t)?);
        }
        for t in args["targets"].as_array().into_iter().flatten() {
            targets.push(self.resolve(t)?);
        }
        let mut seen = std::collections::HashSet::new();
        targets.retain(|t| seen.insert(*t));
        if targets.is_empty() {
            return Err("target (or targets) is required".into());
        }
        if targets.len() > 64 {
            return Err("at most 64 targets".into());
        }
        let op = match args["op"].as_str().unwrap_or("add") {
            "add" => Op::Add,
            "carve" => Op::Carve,
            "intersect" => Op::Intersect,
            other => return Err(format!("op must be add, carve or intersect, not {other}")),
        };
        let into = args.get("into").filter(|v| !v.is_null()).map(|v| self.resolve(v)).transpose()?;
        if let Some(i) = into {
            if targets.contains(&i) {
                return Err("into must not be one of the targets".into());
            }
            if self.scene.get(i).is_none_or(|o| o.clay.is_none()) {
                return Err("into must be a clay object".into());
            }
        }
        let volume = match args["mode"].as_str().unwrap_or("exact") {
            "exact" => false,
            "volume" => true,
            other => return Err(format!("mode must be exact or volume, not {other}")),
        };
        let res = resolution(args)?;
        let blur = number(args, "blur", 0.0, 4.0)?.unwrap_or(0.0);
        let polish = number(args, "polish", 0.0, 1.0)?.unwrap_or(0.0);
        if !volume && (blur > 0.0 || polish > 0.0) {
            return Err("blur and polish need mode volume".into());
        }
        for &t in &targets {
            let o = self.scene.get(t).expect("resolved");
            if matches!(o.shape, Shape::Group | Shape::Scatter | Shape::Fire) {
                return Err(format!("{} ({}) has no surface of its own to make clay from", o.name, o.shape.label()));
            }
            if o.link.is_some() {
                return Err(format!("{} is a linked mirror copy; unlink it first", o.name));
            }
        }
        // Blob edits change what replays read: settle the ledgers first.
        self.settle_clays(targets.iter().copied().chain(into))?;
        self.meshes.sync(&self.scene);
        let dest = into.unwrap_or(targets[0]);
        let to_dest = self.scene.world_matrix(dest).inverse();
        let base_is_clay = into.is_some() || self.scene.get(dest).is_some_and(|o| o.shape == Shape::Clay);
        let clay = if base_is_clay { self.scene.get(dest).and_then(|o| o.clay.clone()).unwrap_or_default() } else { Clay::default() };
        let sources: Vec<(u64, Op)> = targets.iter().enumerate().filter(|(i, t)| !(into.is_none() && *i == 0 && base_is_clay && **t == dest)).map(|(i, &t)| (t, if i == 0 && into.is_none() { Op::Add } else { op })).collect();
        let mut samples = Vec::new();
        let mut notes = Vec::new();
        if base_is_clay && into.is_none() {
            // The clay converted in place: its own surface is the source.
            samples = self.meshes.rest(dest).map(|m| m.0.vertices.iter().step_by(m.0.vertices.len().div_ceil(4096).max(1)).map(|v| Vec3::from(v.pos)).collect()).unwrap_or_default();
        }
        // Gathered here (shared meshes), made blobs on the job's thread:
        // welding, hashing and decimating a mesh is the slow part.
        let mut gathered = Vec::new();
        for &(id, o) in &sources {
            let rel = to_dest * self.scene.world_matrix(id);
            let color = crate::to_clay::tint(self.scene.get(id).map_or([1.0; 3], |o| o.color), self.scene.get(dest).map_or([1.0; 3], |o| o.color));
            gathered.push((crate::to_clay::source(&self.scene, &self.meshes, id)?, rel, o, color));
        }
        // One blob per source.
        if clay.blobs.len() + gathered.len() > MAX_BLOBS {
            return Err(format!("the clay would have {} blobs (at most {MAX_BLOBS})", clay.blobs.len() + gathered.len()));
        }
        let single = targets.len() == 1 && into.is_none();
        let involved: Vec<(u64, Object)> = targets.iter().chain(into.iter()).filter_map(|&id| self.scene.get(id).map(|o| (id, o.clone()))).collect();
        let hidden: Vec<u64> = targets.iter().copied().filter(|&t| t != dest).collect();
        let label = format!("To clay {}", self.scene.get(dest).map(|o| o.name.as_str()).unwrap_or("?"));
        let mode = if volume { "volume" } else { "exact" };
        Ok(Started::Job(label, Box::new(move |progress| {
            let mut clay = clay;
            for (src, rel, o, color) in &gathered {
                progress.check()?;
                let c = src.convert(Mat4::IDENTITY, &[*rel], *o, *color)?;
                for (k, mut b) in c.blobs.into_iter().enumerate() {
                    b.name = if k == 0 { format!("to_clay/{}", src.name) } else { format!("to_clay/{}/{k}", src.name) };
                    clay.blobs.push(b);
                }
                for a in c.assets {
                    clay.assets.insert(a.hash().to_string(), a);
                }
                if single {
                    samples = c.samples;
                }
                notes.extend(c.note);
            }
            if clay.blobs.len() > MAX_BLOBS {
                return Err(format!("the clay would have {} blobs (at most {MAX_BLOBS})", clay.blobs.len()));
            }
            clay.adopt_assets()?;
            if let Some(r) = res {
                clay.resolution = r;
            }
            if let Some(b) = clay.blobs.iter().find_map(|b| b.validate().err()) {
                return Err(b);
            }
            if clay.blobs.first().is_some_and(|b| b.op == Op::Intersect) {
                notes.push("the first blob intersects nothing before it: the clay is empty".into());
            }
            progress.set(0.3);
            let clay = if volume {
                let (c, more) = crate::to_clay::remesh(&clay, clay.resolution, blur, polish, progress)?;
                notes.extend(more);
                c
            } else {
                // Its budgets with whatever the ledgers record (as edit_clay).
                let mut clay = clay;
                crate::layers::sync_within_budget(&mut clay)?;
                clay
            };
            progress.set(0.95);
            let hausdorff = if single { crate::to_clay::hausdorff_est(&clay, &samples) } else { None };
            let cell = clay.cell_size();
            Ok(Box::new(move |app: &mut ClideApp| {
                for (id, before) in &involved {
                    if app.scene.get(*id) != Some(before) {
                        return Err(format!("{} changed meanwhile; nothing was changed, send the call again", before.name));
                    }
                }
                let o = app.scene.get_mut(dest).ok_or("the object was deleted meanwhile")?;
                if o.shape != Shape::Clay {
                    // In place: the object keeps its id, name, placement and children.
                    o.shape = Shape::Clay;
                    (o.mesh, o.kit, o.part, o.terrain, o.eye, o.hair, o.poly) = (None, None, None, None, None, None, None);
                }
                let blobs = clay.blobs.len();
                o.clay = Some(clay);
                let mut hidden_names = Vec::new();
                for &h in &hidden {
                    if let Some(o) = app.scene.get_mut(h) {
                        o.visible = false;
                        hidden_names.push(o.name.clone());
                    }
                }
                Ok(text(json!({
                    "object": object_json_in(&app.scene, dest),
                    "mode": mode,
                    "op": op,
                    "blobs": blobs,
                    "cell": cell.map(round),
                    "hausdorff_est": hausdorff.map(round),
                    "hausdorff_cells": hausdorff.zip(cell).map(|(h, c)| round(h / c)),
                    "hidden": hidden_names,
                    "notes": notes,
                })))
            }) as Finish)
        })))
    }

    /// `kitbash` InsertMesh: the spots worked out here, the source made
    /// blobs on the job's thread (one asset, however many spots), added
    /// to the target if it has not changed meanwhile.
    pub(super) fn insert_mesh(&mut self, args: &Value) -> Result<Started, String> {
        // Blob edits change what replays read: settle the ledgers first.
        self.settle_all_clays()?;
        let plan = crate::kitbash::tool::insert_plan(&self.scene, &mut self.meshes, args)?;
        let before = self.scene.get(plan.target).cloned();
        let label = format!("Insert into {}", before.as_ref().map_or("?", |o| o.name.as_str()));
        Ok(Started::Job(label, Box::new(move |progress| {
            let built = plan.build()?;
            progress.check()?;
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(plan.target) != before.as_ref() {
                    return Err("the target changed meanwhile; nothing was changed, send the call again".into());
                }
                plan.apply(&mut app.scene, built).map(text)
            }) as Finish)
        })))
    }

    /// `export_print`: objects welded into one mesh, in millimeters, Z up,
    /// on the plate (lowest point at z = 0).
    pub(super) fn export_print(&mut self, args: &Value) -> Result<Started, String> {
        let path = PathBuf::from(args["path"].as_str().ok_or("path is required (absolute, .stl, .obj, .ply or .3mf)")?);
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        if !path.is_absolute() || !["stl", "obj", "ply", "3mf"].contains(&ext.as_str()) || path.is_dir() {
            return Err("path must be an absolute file path ending in .stl, .obj, .ply or .3mf".into());
        }
        let size = match args.get("size_mm").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => {
                let axis = match v["axis"].as_str().unwrap_or("y") {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    other => return Err(format!("size_mm axis must be x, y or z (scene axes), not {other}")),
                };
                let value = v["value"].as_f64().filter(|x| x.is_finite() && *x > 0.0 && *x <= 1e6).ok_or("size_mm needs value, a positive number of millimeters")?;
                Some((axis, value))
            }
        };
        let wall = match args.get("hollow").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v["wall_mm"].as_f64().filter(|x| x.is_finite() && *x > 0.0 && *x <= 1e4).ok_or("hollow needs wall_mm, a positive number of millimeters")?),
        };
        let fine = args["fine"].as_bool().unwrap_or(false);
        let max_triangles = match args.get("max_triangles").filter(|v| !v.is_null()) {
            None => DEFAULT_PRINT_TRIANGLES,
            Some(v) => v.as_u64().filter(|n| (MIN_PRINT_TRIANGLES..=MAX_PRINT_TRIANGLES).contains(n)).ok_or(format!("max_triangles must be a whole number from {MIN_PRINT_TRIANGLES} to {MAX_PRINT_TRIANGLES}"))?,
        };
        let simplify = match args.get("simplify").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v["error_mm"].as_f64().filter(|x| (0.001..=1.0).contains(x)).ok_or("simplify needs error_mm, from 0.001 to 1 millimeter")? as f32),
        };
        let eval = self.scene.evaluate();
        let mut ids = Vec::new();
        let roots: Vec<u64> = args["targets"].as_array().into_iter().flatten().map(|t| self.resolve(t)).collect::<Result<_, _>>()?;
        if roots.is_empty() {
            ids.extend(self.scene.objects.iter().filter(|o| eval.get(&o.id).is_some_and(|e| e.1)).map(|o| o.id));
        } else {
            for r in roots {
                ids.push(r);
                ids.extend(self.scene.descendants(r));
            }
        }
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(*id) && self.scene.get(*id).is_some_and(|o| o.shape.is_mesh() && !o.is_reference() && !matches!(o.shape, Shape::Fire | Shape::Hair | Shape::Cloth) && o.kitbash.as_deref() != Some(crate::kitbash::place::LIBRARY_TAG)));
        if ids.is_empty() {
            return Err("nothing to export: no visible solid objects (or targets)".into());
        }
        self.settle_clays(ids.clone())?;
        self.meshes.sync(&self.scene);
        // Each object: its world matrix (rest), the scene units one of
        // its own is, and its mesh, or the clay to mesh on the job's
        // thread (hollow or fine).
        let mut parts: Vec<Part> = Vec::new();
        // Triangles the file will have, from the meshes there are now:
        // refused before anything is built.
        let mut estimate = 0.0f64;
        for &id in &ids {
            let o = self.scene.get(id).expect("kept");
            let world = self.scene.world_matrix(id);
            if let Some(c) = o.clay.as_ref().filter(|c| wall.is_some() || (fine && c.detail_cell.is_some())) {
                // The shell is in the clay's own units: an even scale
                // turns the wall into them; an uneven one would vary it.
                let (scale, _, _) = world.to_scale_rotation_translation();
                let scale = scale.abs();
                if wall.is_some() && (!crate::to_clay::uniform(scale) || !scale.is_finite() || scale.min_element() <= 0.0 || !orthogonal(world)) {
                    return Err(format!("hollow needs evenly scaled clay ({} is scaled unevenly or sheared, so its wall would vary): scale it evenly", o.name));
                }
                let rest = self.meshes.rest(id).map(|m| m.0.clone()).ok_or_else(|| format!("{} has no mesh", o.name))?;
                // What is built: every enabled sculpt layer's detail (their
                // bricks set the cell size too), not the base layer's alone.
                let c = crate::layers::composed_clay(c);
                let one = if fine && c.detail_cell.is_some() { c.fine_estimate(&rest) } else { (rest.indices.len() / 3) as f64 };
                // A shell adds its inside wall.
                estimate += if wall.is_some() { one * 2.0 } else { one };
                parts.push((o.name.clone(), world, scale.x, Err(c)));
            } else if wall.is_some() {
                return Err(format!("hollow works on clay only ({} is a {})", o.name, o.shape.label()));
            } else {
                let mesh = if o.shape == Shape::Clay { self.meshes.rest(id).map(|m| m.0.clone()) } else { self.meshes.get(o).cloned() };
                let mesh = mesh.ok_or_else(|| format!("{} has no mesh", o.name))?;
                estimate += (mesh.indices.len() / 3) as f64;
                parts.push((o.name.clone(), world, 1.0, Ok(mesh)));
            }
        }
        let estimate = estimate.round() as u64;
        if estimate > max_triangles {
            return Err(format!("the print would have about {estimate} triangles, more than max_triangles {max_triangles}; raise max_triangles (at most {MAX_PRINT_TRIANGLES}), use larger detail cells or smaller detail regions, or export without fine"));
        }
        let library = self.scene.materials.clone();
        Ok(Started::Job(format!("Export for print {}", path.file_name().and_then(|f| f.to_str()).unwrap_or("")), Box::new(move |progress| {
            // Y up to Z up (slicers build along Z).
            let up = Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2);
            let gather = |parts: &[Part], wall_units: Option<f32>| -> Result<(Vec<Vec3>, Vec<[u32; 3]>), String> {
                let (mut pts, mut tris) = (Vec::new(), Vec::new());
                for (name, world, unit, m) in parts {
                    progress.check()?;
                    let mesh = match m {
                        Ok(m) => m.clone(),
                        Err(c) => {
                            let mut c = c.clone();
                            if let Some(w) = wall_units {
                                // Scene units to the clay's own.
                                c.shell = Some(w / unit);
                            }
                            std::sync::Arc::new(if fine && c.detail_cell.is_some() { c.build_fine(&library, "")? } else { c.build_mesh_with(&library, "") })
                        }
                    };
                    if mesh.indices.is_empty() {
                        return Err(format!("{name} has no triangles"));
                    }
                    let base = pts.len() as u32;
                    let m = up * *world;
                    pts.extend(mesh.vertices.iter().map(|v| m.transform_point3(Vec3::from(v.pos))));
                    let flip = world.determinant() < 0.0;
                    tris.extend(mesh.indices.chunks_exact(3).map(|t| if flip { [base + t[0], base + t[2], base + t[1]] } else { [base + t[0], base + t[1], base + t[2]] }));
                }
                Ok((pts, tris))
            };
            // The scale comes from the solid shape; hollowing keeps its outside.
            let (pts, tris) = gather(&parts, None)?;
            let (lo, hi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
            // Scene axes after Y up became Z up: x stays, y is z, z is -y.
            let extent = |axis: usize| (hi - lo)[[0, 2, 1][axis]] as f64;
            let mm = match size {
                Some((axis, value)) if extent(axis) > 0.0 => value / extent(axis),
                Some(_) => return Err("the objects have no extent along that axis".into()),
                None => 1000.0,
            };
            let (pts, tris) = match wall {
                Some(w) => gather(&parts, Some((w / mm) as f32))?,
                None => (pts, tris),
            };
            progress.set(0.6);
            let (pts, tris) = mesh_asset::weld(&pts, &tris);
            let before = tris.len();
            let tris: Vec<[u32; 3]> = tris.into_iter().filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect();
            let degenerate = before - tris.len();
            let (lo, _) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
            // In millimeters (f64, then f32), lowest point on the plate.
            let plate = glam::DVec3::new(lo.x as f64, lo.y as f64, lo.z as f64);
            let mut pts: Vec<Vec3> = pts.iter().map(|p| ((p.as_dvec3() - plate) * mm).as_vec3()).collect();
            let mut tris = tris;
            // Sheets pinched at one point (the weld merged the fine mesh's
            // copies back) are moved apart, so slicers keep them apart.
            progress.check()?;
            let separated = crate::adaptive::separate_pinches(&mut pts, &mut tris, PINCH_MIN_MM, PINCH_CELL_SHARE);
            let low = pts.iter().fold(f32::MAX, |a, p| a.min(p.z));
            if low < 0.0 {
                pts.iter_mut().for_each(|p| p.z -= low);
            }
            let mut warnings = Vec::new();
            let mut simplified_from = None;
            if let Some(error) = simplify {
                progress.set(0.8);
                progress.check()?;
                match simplified(&pts, &tris, error) {
                    Some((p, t)) => {
                        simplified_from = Some(tris.len());
                        (pts, tris) = (p, t);
                    }
                    None => warnings.push(format!("simplify at {error} mm did not stay watertight (or removed nothing), so the unsimplified mesh was written")),
                }
            }
            // The estimate can run low (several fine parts, balancing):
            // the limit holds for what was built too, and nothing is written.
            if tris.len() as u64 > max_triangles {
                return Err(format!("the print has {} triangles (estimated {estimate} before building), more than max_triangles {max_triangles}; nothing was written: raise max_triangles (at most {MAX_PRINT_TRIANGLES}), add simplify, use larger detail cells or smaller detail regions, or export without fine", tris.len()));
            }
            let (edges, kinds, flipped) = mesh_asset::edge_report(&tris);
            let bad = kinds.open + kinds.over_used + kinds.pinched;
            let volume = mesh_asset::volume(&pts, &tris) / 1000.0;
            let (blo, bhi) = pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
            let bytes = match ext.as_str() {
                "stl" => mesh_import::write::stl(&pts, &tris),
                "obj" => mesh_import::write::obj(&pts, &tris),
                "ply" => mesh_import::write::ply(&pts, &tris),
                _ => mesh_import::write::three_mf(&pts, &tris)?,
            };
            progress.check()?;
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            let out = json!({
                "path": path.display().to_string(),
                "format": ext,
                "triangles": tris.len(),
                "vertices": pts.len(),
                "edges": edges,
                "watertight": bad == 0 && flipped == 0 && edges > 0,
                "non_manifold_edges": bad,
                "non_manifold_by_kind": kinds,
                "inconsistent_edges": flipped,
                "pinches_separated": separated.copies,
                "pinch_offset_mm": (separated.copies > 0).then(|| json!({ "min": (separated.min_offset as f64 * 1e5).round() / 1e5, "max": (separated.max_offset as f64 * 1e5).round() / 1e5 })),
                "estimated_triangles": estimate,
                "simplified_from": simplified_from,
                "warnings": warnings,
                "degenerate_dropped": degenerate,
                "volume_cm3": (volume * 1e4).round() / 1e4,
                "bbox_mm": { "min": round3(blo), "max": round3(bhi), "size": round3(bhi - blo) },
                "mm_per_unit": mm,
                "objects": parts.iter().map(|p| p.0.clone()).collect::<Vec<_>>(),
                "bytes": bytes.len(),
            });
            Ok(Box::new(move |_: &mut ClideApp| Ok(text(out))) as Finish)
        })))
    }

    /// File > Import…: the same `import_mesh` request an agent sends.
    pub(super) fn import_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Meshes", &["obj", "stl", "ply", "glb", "fbx"]).pick_file() else { return };
        self.send_ui_call("import_mesh", json!({ "path": path.display().to_string() }));
    }

    /// File > Export for Print…: `export_print` of the selection (or the
    /// visible scene).
    pub(super) fn export_print_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("STL", &["stl"]).add_filter("3MF", &["3mf"]).add_filter("OBJ", &["obj"]).add_filter("PLY", &["ply"]).set_file_name("print.stl").save_file() else { return };
        let mut req = json!({ "path": path.display().to_string() });
        if !self.selection.is_empty() {
            req["targets"] = json!(self.selection);
        }
        self.send_ui_call("export_print", req);
    }

    /// Modify > Remesh as Clay…: opens the dialog for the lead object.
    pub(super) fn open_remesh_dialog(&mut self) {
        let Some(id) = self.selection.last().copied() else { return };
        let resolution = self.scene.get(id).and_then(|o| o.clay.as_ref()).map_or(128, |c| c.resolution);
        self.remesh_dialog = Some(RemeshDialog { target: id, resolution, blur: 0.0, polish: 0.0 });
    }

    /// The Remesh as Clay… window; Remesh (or Enter) sends `to_clay`.
    pub(super) fn remesh_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.remesh_dialog.clone() else { return };
        let mut open = true;
        let mut send = false;
        let name = self.scene.get(d.target).map(|o| o.name.clone()).unwrap_or_default();
        egui::Window::new("Remesh as Clay").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(format!("{name}: one closed clay surface (DynaMesh)"));
            ui.add(egui::Slider::new(&mut d.resolution, MIN_RESOLUTION..=320).text("Resolution"));
            ui.add(egui::Slider::new(&mut d.blur, 0.0..=4.0).text("Blur (cells)"));
            ui.add(egui::Slider::new(&mut d.polish, 0.0..=1.0).text("Polish"));
            send = ui.button("Remesh").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if send {
            self.send_ui_call("to_clay", d.request());
            self.remesh_dialog = None;
        } else {
            self.remesh_dialog = open.then_some(d);
        }
    }
}
