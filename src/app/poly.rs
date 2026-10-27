//! The `poly` tool (docs/QUALITY_PLAN.md Stage 3a): polygon objects with
//! Catmull-Clark levels and semi-sharp creases (`crate::poly`).
//!
//! `from_clay` (a job) makes a cage from a clay's surface-nets quads,
//! `subdivide` keeps a number of levels, `set_level` picks the one shown,
//! rendered and exported, `crease` sets edge sharpness, `info` reports
//! the cage, `rebuild` runs `from_clay` again from the stored call, and
//! `to_clay` converts the shown level to clay in place (as `to_clay`).
//! Page Up and Page Down in the editor send `set_level`.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::{Finish, Started};
use super::tools::{object_json_in, text};
use crate::clay::Clay;
use crate::mcp::ToolResult;
use crate::poly::topology::Topology;
use crate::poly::{INFINITE_SHARPNESS, MAX_CREASES, MAX_LEVELS, MAX_SHOWN_FACES, Poly};
use crate::scene::Shape;

pub(super) mod export;
pub(super) mod paint;
pub(super) mod project;
pub(super) mod retopo;
pub(super) mod tiles;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod film_tests;
#[cfg(test)]
mod udim_tests;

/// UDIM tiles `poly uv udim: true` fills by default.
const DEFAULT_UDIM_TILES: u32 = 4;

/// Most cage or level points the distance to a clay is measured at.
const DISTANCE_SAMPLES: usize = 20_000;

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// What `action` may be.
const ACTIONS: &str = "action is required: info, from_clay, subdivide, set_level, crease, uv, project, rebuild or to_clay";

/// `poly` actions that run as jobs: `info` too, which builds the cage's
/// adjacency (linear in its corners, but 16M of them) off the window's
/// thread.
pub(super) fn is_job(args: &Value) -> bool {
    is_edit_job(args) || args["action"].as_str() == Some("info")
}

/// `poly` jobs that edit the scene (later edits wait in line behind them).
pub(super) fn is_edit_job(args: &Value) -> bool {
    matches!(args["action"].as_str(), Some("from_clay" | "rebuild" | "to_clay" | "crease" | "project" | "uv"))
}

/// The largest and mean distance from `points` (in the clay's space) to
/// the clay's surface, in the clay's mesh cells (the regular mesh
/// `from_clay` reads), and how many points were measured: at most
/// `DISTANCE_SAMPLES`, evenly picked, so the largest is a sampled
/// one-sided Hausdorff distance, not the true one. None for an empty clay.
pub(super) fn distance_cells(clay: &Clay, points: &[Vec3]) -> Option<(f32, f32, usize)> {
    let probe = crate::quality::Probe::new(clay)?;
    let cell = clay.cell_size()?;
    let step = points.len().div_ceil(DISTANCE_SAMPLES).max(1);
    let samples: Vec<Vec3> = points.iter().step_by(step).copied().collect();
    let mut d = vec![0.0f32; samples.len()];
    crate::clay::par_chunks(&mut d, 256, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let p = samples[i * 256 + k];
            *o = probe.near(p, 2.0 * cell)(p).abs() / cell;
        }
    });
    let max = d.iter().copied().fold(0.0, f32::max);
    let mean = d.iter().map(|&x| x as f64).sum::<f64>() / d.len().max(1) as f64;
    Some((max, mean as f32, samples.len()))
}

/// `crease angle`'s edges (sorted point pairs): every cage edge whose two
/// faces meet at more than `limit` degrees, and for a removal only those
/// creased now (so a removal never passes the creases kept). Refused as
/// soon as more than `MAX_CREASES` are picked, before more are kept.
pub(super) fn edges_over_angle(p: &Poly, t: &Topology, limit: f32, removing: bool) -> Result<Vec<(u32, u32)>, String> {
    let cage = p.cage.to_vec();
    let normals: Vec<Vec3> = (0..t.faces()).map(|f| crate::poly::subdiv::newell(&cage, t.face(f)).normalize_or_zero()).collect();
    let mut chosen = Vec::new();
    for e in 0..t.edges.len() {
        let fs = t.faces_of_edge(e);
        if fs.len() == 2 && normals[fs[0] as usize].angle_between(normals[fs[1] as usize]).to_degrees() > limit {
            let [a, b] = t.edges[e];
            let k = (a.min(b), a.max(b));
            if removing && !p.creases.contains_key(&k) {
                continue;
            }
            if chosen.len() == MAX_CREASES {
                return Err(format!("angle {limit} picks more than {MAX_CREASES} edges, the most one call may set; a larger angle picks fewer"));
            }
            chosen.push(k);
        }
    }
    Ok(chosen)
}

/// A remade cage (`rebuild`) keeps the old one's creases when its faces
/// are the same, its levels as far as they fit, and its shown level.
pub(super) fn keep_levels(poly: &mut Poly, old: &Poly, notes: &mut Vec<String>) {
    if old.face_start == poly.face_start && old.corners == poly.corners {
        poly.creases = old.creases.clone();
        poly.uv = old.uv.clone();
        // The tiles are laid out in those UVs: kept.
        poly.paint = old.paint.clone();
    } else {
        if !old.creases.is_empty() {
            notes.push(format!("the cage's faces changed: its {} creases were dropped", old.creases.len()));
        }
        if old.uv.is_some() {
            notes.push("the cage's faces changed: its UVs were dropped (poly uv makes new ones)".into());
        }
        if old.paint.as_ref().is_some_and(|p| !p.is_empty()) {
            notes.push("the cage's faces changed: its texture tiles and paint strokes were dropped with its UVs".into());
        }
    }
    if old.levels.iter().any(|l| l.displacement.is_some()) {
        notes.push("the projected detail was dropped (it belonged to the old cage): poly project again".into());
    }
    let keep = (0..=old.level_count()).rev().find(|&l| poly.set_levels(l).is_ok()).unwrap_or(0);
    if keep < old.level_count() {
        notes.push(format!("{keep} of {} levels kept (the new cage is denser)", old.level_count()));
    }
    poly.level = old.level.min(poly.finest_shown());
}

/// A whole number argument in `0..=hi`.
fn level_arg(args: &Value, key: &str, hi: u8) -> Result<Option<u8>, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_u64().filter(|&n| n <= hi as u64).map(|n| Some(n as u8)).ok_or_else(|| format!("{key} must be a whole number from 0 to {hi}")),
    }
}

impl ClideApp {
    fn poly_of(&self, args: &Value) -> Result<(u64, &Poly), String> {
        let id = self.resolve(args.get("target").filter(|t| !t.is_null()).ok_or("target (a Poly object) is required")?)?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        o.poly.as_ref().map(|p| (id, p)).ok_or_else(|| format!("{} is not a Poly object (poly from_clay makes one)", o.name))
    }

    /// `poly` actions that run at once.
    pub(super) fn poly_tool(&mut self, args: &Value) -> ToolResult {
        match args["action"].as_str().ok_or(ACTIONS)? {
            "subdivide" => {
                let levels = level_arg(args, "levels", MAX_LEVELS)?.ok_or_else(|| format!("levels (0 to {MAX_LEVELS}: the levels to keep) is required"))?;
                let (id, p) = self.poly_of(args)?;
                let mut p = p.clone();
                // Refused from the counts before anything is built.
                p.set_levels(levels)?;
                p.level = p.finest_shown();
                let reply = self.poly_levels_reply(id, &p);
                self.scene.get_mut(id).expect("resolved").poly = Some(p);
                Ok(text(reply))
            }
            "set_level" => {
                let level = level_arg(args, "level", MAX_LEVELS)?.ok_or("level (the level to show, 0 is the cage) is required")?;
                let (id, p) = self.poly_of(args)?;
                if level > p.level_count() {
                    return Err(format!("level {level} is past the {} levels kept (poly subdivide adds levels)", p.level_count()));
                }
                if p.faces_at(level) > MAX_SHOWN_FACES {
                    return Err(format!("level {level} has {} faces; the shown level may have at most {MAX_SHOWN_FACES} (level {} fits)", p.faces_at(level), p.finest_shown()));
                }
                let mut p = p.clone();
                p.level = level;
                let reply = self.poly_levels_reply(id, &p);
                self.scene.get_mut(id).expect("resolved").poly = Some(p);
                Ok(text(reply))
            }
            "info" | "from_clay" | "rebuild" | "to_clay" | "crease" | "project" | "uv" => Err("this poly action runs as a job".into()),
            other => Err(format!("unknown poly action {other}; {ACTIONS}")),
        }
    }

    fn poly_levels_reply(&self, id: u64, p: &Poly) -> Value {
        json!({
            "target": id,
            "levels": p.level_count(),
            "level": p.level,
            "level_faces": (0..=p.level_count()).map(|l| p.faces_at(l)).collect::<Vec<_>>(),
            "shown_faces": p.faces_at(p.level),
        })
    }

    /// `poly crease` (a job): sharpness on the listed cage edges, or on
    /// every edge whose faces meet at more than `angle` degrees. The
    /// arguments are checked here; the cage's adjacency and face normals
    /// are built on the job's thread.
    fn poly_crease(&mut self, args: &Value) -> Result<Started, String> {
        let sharpness = match args.get("sharpness").filter(|v| !v.is_null()) {
            None => INFINITE_SHARPNESS,
            Some(v) => v.as_f64().map(|s| s as f32).filter(|s| (0.0..=INFINITE_SHARPNESS).contains(s)).ok_or(format!("sharpness must be a number from 0 (removes) to {INFINITE_SHARPNESS} (infinitely sharp)"))?,
        };
        let edges = args.get("edges").filter(|v| !v.is_null());
        let angle = args.get("angle").filter(|v| !v.is_null());
        if edges.is_some() == angle.is_some() {
            return Err("give edges (cage point pairs) or angle (degrees), one of them".into());
        }
        // Bounded before anything is read.
        let pairs = match edges {
            Some(v) => {
                let list = v.as_array().ok_or("edges must be a list of [i, j] point pairs")?;
                if list.len() > MAX_CREASES {
                    return Err(format!("at most {MAX_CREASES} edges per call ({} sent)", list.len()));
                }
                let pair = |e: &Value| -> Option<(u32, u32)> {
                    let a = e.as_array().filter(|a| a.len() == 2)?;
                    let (i, j) = (a[0].as_u64()?, a[1].as_u64()?);
                    (i <= u32::MAX as u64 && j <= u32::MAX as u64).then_some((i as u32, j as u32))
                };
                Some(list.iter().map(|e| pair(e).ok_or_else(|| format!("{e} is not a pair of point indices"))).collect::<Result<Vec<_>, _>>()?)
            }
            None => None,
        };
        let angle = match angle {
            Some(v) => Some(v.as_f64().map(|a| a as f32).filter(|a| (0.0..=180.0).contains(a)).ok_or("angle must be degrees from 0 to 180")?),
            None => None,
        };
        let (id, p) = self.poly_of(args)?;
        let before = p.clone();
        let label = format!("Crease {}", self.scene.get(id).map_or("?", |o| o.name.as_str()));
        Ok(Started::Job(label, Box::new(move |progress| {
            let p = &before;
            let t = p.topology();
            progress.check()?;
            let mut chosen: Vec<(u32, u32)> = Vec::new();
            if let Some(pairs) = pairs {
                let points = p.cage.len();
                for (i, j) in pairs {
                    if i as usize >= points || j as usize >= points {
                        return Err(format!("[{i}, {j}]: the cage has points 0 to {} (poly info's cage_points is {points})", points - 1));
                    }
                    if t.edge_between(i, j).is_none() {
                        return Err(format!("[{i}, {j}] is not an edge of the cage (edges join neighbouring corners of a face)"));
                    }
                    chosen.push((i.min(j), i.max(j)));
                }
            }
            if let Some(limit) = angle {
                chosen = edges_over_angle(p, &t, limit, sharpness <= 0.0)?;
            }
            let mut creases: BTreeMap<(u32, u32), f32> = (*p.creases).clone();
            for &k in &chosen {
                if sharpness <= 0.0 {
                    creases.remove(&k);
                } else {
                    creases.insert(k, sharpness);
                }
            }
            if creases.len() > MAX_CREASES {
                return Err(format!("a cage keeps at most {MAX_CREASES} creases (this would make {})", creases.len()));
            }
            let mut next = p.clone();
            next.creases = Arc::new(creases);
            let reply = json!({
                "target": id,
                "edges_set": chosen.len(),
                "sharpness": sharpness,
                "creases": next.creases.len(),
                "infinite": next.creases.values().filter(|&&s| s >= INFINITE_SHARPNESS).count(),
            });
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(id).and_then(|o| o.poly.as_ref()) != Some(&before) {
                    return Err("the Poly changed meanwhile; nothing was changed, send the call again".into());
                }
                app.scene.get_mut(id).expect("checked").poly = Some(next);
                Ok(text(reply))
            }) as Finish)
        })))
    }

    /// `poly uv` (a job): the cage unwrapped into one tile (xatlas, quads
    /// whole), or with `udim: true` over UDIM tiles 1001+ (each chart on
    /// one tile, at `texel_density` or a density that fills about
    /// `tiles`), kept per corner; levels take face-varying linear UVs. A
    /// Poly's texture tiles belong to its old UVs: they are made again
    /// from its paint strokes in the new layout (baked or imported texels
    /// are dropped, with a note).
    fn poly_uv(&mut self, args: &Value) -> Result<Started, String> {
        use crate::poly::uv;
        match args.get("method").filter(|v| !v.is_null()).map(|v| v.as_str()) {
            None | Some(Some("xatlas")) => {}
            _ => return Err("method must be xatlas".into()),
        }
        let udim = match args.get("udim").filter(|v| !v.is_null()) {
            None => false,
            Some(v) => v.as_bool().ok_or("udim must be true or false")?,
        };
        let tiles = match args.get("tiles").filter(|v| !v.is_null()) {
            None => DEFAULT_UDIM_TILES,
            Some(v) => v.as_u64().filter(|t| (1..=uv::MAX_TILES as u64).contains(t)).ok_or(format!("tiles must be a whole number from 1 to {}", uv::MAX_TILES))? as u32,
        };
        let density = match args.get("texel_density").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_f64().filter(|d| d.is_finite() && *d > 0.0 && *d <= 1e9).ok_or("texel_density must be a positive number of texels per unit")? as f32),
        };
        if !udim && (args.get("tiles").is_some_and(|v| !v.is_null()) || density.is_some()) {
            return Err("tiles and texel_density lay out UDIM tiles: send udim: true with them".into());
        }
        let size = match args.get("size").filter(|v| !v.is_null()) {
            None => uv::DEFAULT_SIZE,
            Some(v) => v.as_u64().filter(|s| uv::SIZES.iter().any(|t| *t as u64 == *s)).ok_or(format!("size must be one of {:?}", uv::SIZES))? as u32,
        };
        let padding = match args.get("padding_px").filter(|v| !v.is_null()) {
            None => uv::DEFAULT_PADDING,
            Some(v) => v.as_u64().filter(|p| *p <= uv::MAX_PADDING as u64).ok_or(format!("padding_px must be a whole number from 0 to {}", uv::MAX_PADDING))? as u32,
        };
        let (id, p) = self.poly_of(args)?;
        if p.faces() > uv::MAX_UV_FACES {
            return Err(format!("the cage has {} faces; poly uv unwraps at most {} (retopo makes a lighter cage)", p.faces(), uv::MAX_UV_FACES));
        }
        let before = p.clone();
        let label = format!("UVs of {}", self.scene.get(id).map_or("?", |o| o.name.as_str()));
        Ok(Started::Job(label, Box::new(move |progress| {
            let timer = std::time::Instant::now();
            progress.stage("unwrapping", 0.0);
            let made = if udim { uv::unwrap_udim(&before, size, padding, uv::Udim { tiles, texel_density: density })? } else { uv::unwrap(&before, size, padding)? };
            progress.check()?;
            let mut next = before.clone();
            next.uv = Some(crate::poly::Chunked::new(&made.uv));
            let mut notes = Vec::new();
            // The paint's strokes again, in the new layout.
            if let Some(paint) = before.paint.as_ref().filter(|p| !p.is_empty()) {
                progress.stage("painting the strokes again in the new layout", 0.8);
                let layout = crate::poly::texture::paint::layout_hash(&next);
                // A stroke the new layout cannot hold is left out (named in
                // the notes); the unwrap still lands.
                let (replayed, kept, left) = crate::poly::texture::paint::replay_kept(&next, &paint.records, progress)?;
                notes.extend(left);
                let records: Vec<_> = kept.into_iter().map(|mut r| { r.layout = layout.clone(); r.digest = r.digest(); r }).collect();
                if paint.imported {
                    notes.push("texels that came from a bake or an import (not from strokes) were dropped: they were laid out in the old UVs (bake udim: true again)".to_string());
                }
                if !records.is_empty() {
                    notes.push(format!("{} paint strokes were painted again in the new layout", records.len()));
                }
                next.paint = Some(std::sync::Arc::new(crate::poly::texture::paint::PolyPaint { textures: replayed, records, imported: false })).filter(|p| !p.is_empty());
            }
            let seams = uv::seams(&next.topology(), &made.uv);
            let spread = uv::stretch_spread(&next, &made.uv);
            let image = uv::layout_image(&next, &made.uv, 512);
            let mut reply = json!({
                "target": id,
                "corners": made.uv.len(),
                "charts": made.charts,
                "faces_made_whole": made.repaired,
                "seams": seams,
                "utilization": round(made.utilization),
                "area_spread": if spread.is_finite() { json!(round(spread)) } else { Value::Null },
                "size": size,
                "padding_px": padding,
                "image": if udim { "The UV layout: the UDIM tiles in use (1001 bottom left, 10 a row, rows up), the cage's faces in them." } else { "The UV layout: the cage's faces in the tile (u right, v up)." },
                "notes": notes,
            });
            if udim {
                let stats = uv::chart_stats(&next, &made.uv, &made.face_chart, size);
                let d: Vec<f32> = stats.values().map(|s| s.0).filter(|d| *d > 0.0).collect();
                let want = made.texel_density.unwrap_or(0.0);
                let (lo, hi) = d.iter().fold((f32::MAX, 0.0f32), |(a, b), x| (a.min(*x), b.max(*x)));
                reply["udim"] = json!({
                    "tiles": made.tiles,
                    "texel_density": round(want),
                    "chart_density": { "min": round(lo.min(hi)), "max": round(hi), "min_ratio": round(lo.min(hi) / want.max(1e-12)), "max_ratio": round(hi / want.max(1e-12)) },
                    "charts_crossing_tiles": stats.values().filter(|s| s.1.len() > 1).count(),
                    "tile_size": size,
                });
            }
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(id).and_then(|o| o.poly.as_ref()) != Some(&before) {
                    return Err("the Poly changed meanwhile; nothing was changed, send the call again".into());
                }
                app.scene.get_mut(id).expect("checked").poly = Some(next);
                let mut reply = reply;
                reply["seconds"] = json!(round(timer.elapsed().as_secs_f32()));
                let mut out = text(reply);
                out.extend(super::tools::encode_png(&image)?);
                Ok(out)
            }) as Finish)
        })))
    }

    /// `poly` actions that run as jobs (`is_job`).
    pub(super) fn poly_job(&mut self, args: &Value) -> Result<Started, String> {
        match args["action"].as_str().unwrap_or_default() {
            "crease" => self.poly_crease(args),
            "project" => self.poly_project(args),
            "uv" => self.poly_uv(args),
            "info" => {
                let (id, p) = self.poly_of(args)?;
                let p = p.clone();
                let label = format!("Poly info {}", self.scene.get(id).map_or("?", |o| o.name.as_str()));
                Ok(Started::Job(label, Box::new(move |progress| {
                    let mut out = p.info();
                    progress.set(0.95);
                    Ok(Box::new(move |app: &mut ClideApp| {
                        out["object"] = object_json_in(&app.scene, id);
                        Ok(text(out))
                    }) as Finish)
                })))
            }
            "from_clay" => {
                let source = self.resolve(args.get("source").filter(|v| !v.is_null()).ok_or("source (a Clay object) is required")?)?;
                self.poly_from_clay(source, None, args)
            }
            "rebuild" => {
                let (id, p) = self.poly_of(args)?;
                let made = p.made_by.clone().ok_or("this cage records no call to rebuild from")?;
                let source = made["source"].as_u64().filter(|s| self.scene.get(*s).is_some()).ok_or("the clay this cage was made from is gone")?;
                match made["action"].as_str() {
                    Some("from_clay") => self.poly_from_clay(source, Some(id), args),
                    Some("retopo") => {
                        if args["name"].as_str().is_some_and(|n| !n.trim().is_empty()) {
                            return Err("rebuild keeps the object's name".into());
                        }
                        let mut request = retopo::Request::from_made_by(&made)?;
                        if let Some(images) = args["images"].as_bool() {
                            request.images = images;
                        }
                        self.retopo_job(source, Some(id), request)
                    }
                    _ => Err(format!("cannot rebuild from {made}")),
                }
            }
            "to_clay" => {
                let (id, _) = self.poly_of(args)?;
                let mut forward = json!({ "target": id });
                for k in ["resolution", "mode", "blur", "polish"] {
                    if let Some(v) = args.get(k).filter(|v| !v.is_null()) {
                        forward[k] = v.clone();
                    }
                }
                self.to_clay(&forward)
            }
            other => Err(format!("poly {other} does not run as a job")),
        }
    }

    /// A cage from `source`'s rest surface-nets mesh: a new Poly object in
    /// its place (`into` None), or `into`'s cage made again (`rebuild`:
    /// levels kept within the bound, creases kept when the faces are the
    /// same).
    fn poly_from_clay(&mut self, source: u64, into: Option<u64>, args: &Value) -> Result<Started, String> {
        let o = self.scene.get(source).ok_or("object vanished")?;
        if o.clay.is_none() {
            return Err(format!("{} is not a Clay object (from_clay reads a clay's surface)", o.name));
        }
        let name = match (into, args["name"].as_str().filter(|n| !n.trim().is_empty())) {
            (Some(_), Some(_)) => return Err("rebuild keeps the object's name".into()),
            (_, Some(n)) if n.chars().count() > 64 => return Err("name is at most 64 characters".into()),
            (_, Some(n)) => n.to_string(),
            (_, None) => format!("{}_poly", o.name),
        };
        let hide = args["hide_source"].as_bool().unwrap_or(into.is_none());
        // Its mesh is the composed field's: unchecked ledgers settle first.
        self.settle_clays([source])?;
        self.meshes.sync(&self.scene);
        let clay = self.scene.get(source).and_then(|o| o.clay.clone()).expect("checked");
        let rest = self.meshes.rest(source).map(|m| m.0.clone()).ok_or("the clay has no mesh yet")?;
        if rest.indices.is_empty() {
            return Err("the clay has no surface".into());
        }
        let cell = clay.cell_size().unwrap_or(0.01);
        let before = self.scene.get(source).cloned();
        let target_before = into.and_then(|i| self.scene.get(i).cloned());
        let made_by = json!({ "action": "from_clay", "source": source });
        let label = format!("Poly from {}", before.as_ref().map(|o| o.name.as_str()).unwrap_or("?"));
        Ok(Started::Job(label, Box::new(move |progress| {
            let points: Vec<Vec3> = rest.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
            progress.check()?;
            let made = crate::poly::from_mesh::from_triangles(&points, &rest.indices, &rest.edges, cell * 1e-3)?;
            progress.set(0.6);
            progress.check()?;
            let distance = distance_cells(&clay, &made.poly.cage.to_vec());
            let mut poly = made.poly;
            poly.made_by = Some(Arc::new(made_by));
            // A rebuild keeps the old cage's creases (when the faces are the
            // same), levels and shown level, worked out here so the
            // finisher only installs it.
            let mut notes: Vec<String> = Vec::new();
            if let Some(old) = target_before.as_ref().and_then(|o| o.poly.as_ref()) {
                keep_levels(&mut poly, old, &mut notes);
            }
            progress.check()?;
            let mut out = poly.info();
            progress.set(0.95);
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(source) != before.as_ref() || into.and_then(|i| app.scene.get(i)) != target_before.as_ref() {
                    return Err("the clay or the Poly changed meanwhile; nothing was changed, send the call again".into());
                }
                let id = match into {
                    Some(id) => {
                        app.scene.get_mut(id).expect("checked").poly = Some(poly);
                        id
                    }
                    None => app.place_poly(before.as_ref().expect("checked"), name, poly, hide, &mut notes),
                };
                out["object"] = object_json_in(&app.scene, id);
                out["from"] = json!({ "quads": made.quads, "triangles": made.triangles, "dropped": made.dropped, "pinches_separated": made.separated, "points_split": made.split_points });
                out["hausdorff_cells"] = json!(distance.map(|d| round(d.0)));
                out["mean_distance_cells"] = json!(distance.map(|d| round(d.1)));
                out["distance_samples"] = json!(distance.map(|d| d.2));
                out["notes"] = json!(notes);
                Ok(text(out))
            }) as Finish)
        })))
    }

    /// `poly` as a new Poly object in the place of clay `src` (its parent,
    /// placement, bone and material), named `name` (made unique), selected;
    /// the clay hidden when `hide`.
    pub(super) fn place_poly(&mut self, src: &crate::scene::Object, name: String, poly: Poly, hide: bool, notes: &mut Vec<String>) -> u64 {
        let id = self.scene.add(Shape::Poly);
        let taken = |n: &str| self.scene.objects.iter().any(|o| o.id != id && o.name == n);
        let unique = if taken(&name) { (1..).map(|n| format!("{name}{n}")).find(|n| !taken(n)).expect("unbounded") } else { name };
        let o = self.scene.get_mut(id).expect("just added");
        o.name = unique;
        (o.parent, o.translation, o.rotation, o.scale, o.pivot, o.pivot_comp) = (src.parent, src.translation, src.rotation, src.scale, src.pivot, src.pivot_comp);
        o.bone = src.bone.clone();
        o.material = src.material.clone();
        o.poly = Some(poly);
        if hide {
            if let Some(s) = self.scene.get_mut(src.id) {
                s.visible = false;
            }
            notes.push("the clay is hidden (hide_source: false keeps it shown)".into());
        }
        if src.rig.is_some() {
            notes.push("made from the clay's rest pose; the Poly does not follow its rig".into());
        }
        self.select_only(id);
        id
    }

    /// The Poly's shown level and its world matrix (`quality_report`).
    pub(super) fn poly_shown(&self, id: u64) -> Result<(crate::poly::subdiv::Surface, Mat4), String> {
        let o = self.scene.get(id).ok_or("object vanished")?;
        let p = o.poly.as_ref().ok_or("not a Poly object")?;
        Ok((crate::poly::subdiv::evaluate(p, p.level)?, self.scene.world_matrix(id)))
    }

    /// Modify > Unwrap Poly UVs: `poly uv` on the lead Poly, as an agent
    /// sends it.
    pub(super) fn unwrap_lead_poly(&mut self) {
        if let Some(id) = self.lead().filter(|id| self.scene.get(*id).is_some_and(|o| o.poly.is_some())) {
            self.send_ui_call("poly", json!({ "target": id, "action": "uv" }));
        }
    }

    /// Steps the lead Poly's shown level by `by` (Page Up and Page Down),
    /// through `poly set_level`; nothing past either end.
    pub(super) fn step_poly_level(&mut self, by: i32) {
        let Some((id, p)) = self.lead().and_then(|id| self.scene.get(id).and_then(|o| o.poly.as_ref()).map(|p| (id, p))) else { return };
        let to = p.level as i32 + by;
        if (0..=p.level_count() as i32).contains(&to) && p.faces_at(to as u8) <= MAX_SHOWN_FACES {
            self.send_ui_call("poly", json!({ "target": id, "action": "set_level", "level": to }));
        }
    }
}
