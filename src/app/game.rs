//! Agent tools for game-ready output: `bake` turns clay into a simple
//! textured mesh (see `bake.rs`), shown in place of the clay with
//! `set_display baked` or `render_views baked`, and used by both exports.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use super::ClideApp;
use super::jobs::Started;
use crate::bake::{Displacement, MAX_MAP_SIZE, Maps, Settings, TEXTURE_SIZES};
use crate::mcp::ToolResult;
use crate::outputs::{Format, Kind};
use crate::scene::Shape;

impl ClideApp {
    /// `bake`: set an object's bake settings (or turn baking off) and bake,
    /// as a background job; `reply` gets the counts and the textures.
    pub(super) fn bake_tool(&mut self, args: &Value) -> Result<Started, String> {
        let id = self.resolve(&args["target"])?;
        let o = self.scene.get(id).ok_or("object vanished")?;
        if !matches!(o.shape, Shape::Clay | Shape::Cloth) {
            return Err(format!("{} is not a clay object", o.name));
        }
        let name = o.name.clone();
        if args["off"].as_bool().unwrap_or(false) {
            self.scene.get_mut(id).expect("checked").bake = None;
            return Ok(Started::Now(Ok(super::tools::text(json!({ "target": id, "bake": null })))));
        }
        // A face head bakes at 4K by default so pores and lines survive.
        let face = o.bake.is_none() && o.clay.as_ref().is_some_and(|c| c.face().is_some());
        let mut s = o.bake.unwrap_or_default();
        if face {
            s.texture = 4096;
        }
        if let Some(t) = args["triangles"].as_u64() {
            s.triangles = t.min(u32::MAX as u64) as u32;
        }
        if let Some(t) = args["texture"].as_u64() {
            if !TEXTURE_SIZES.contains(&(t as u32)) {
                return Err(format!("texture must be one of {TEXTURE_SIZES:?}"));
            }
            s.texture = t as u32;
        }
        if let Some(d) = args["region_density"].as_f64() {
            s.region_density = d as f32;
        }
        if let Some(e) = args["exact"].as_bool() {
            s.exact = e;
        }
        s.sanitize();
        let maps = MapRequest::parse(args, s.texture, o.shape == Shape::Cloth)?;
        self.scene.get_mut(id).expect("checked").bake = Some(s);
        let show = args["show"].as_bool();
        let start = Instant::now();
        self.meshes.sync(&self.scene);
        let Some(job) = self.meshes.bake_job(&self.scene, id, s)? else {
            let Some(maps) = maps else {
                return Ok(Started::Now(self.bake_reply(id, s, false, start, show, None)));
            };
            // A fresh bake: only the maps are new.
            let job = self.meshes.bake_inputs(&self.scene, id, s)?;
            let mesh = self.meshes.baked(id).ok_or("the bake is gone (the clay changed); run bake again")?.0.clone();
            return Ok(Started::Job(format!("Displacement maps of {name}"), Box::new(move |progress| {
                let made = maps.make(&job, &mesh, &name, progress)?;
                Ok(Box::new(move |app: &mut ClideApp| app.bake_reply(job.id, job.settings, false, start, show, Some(made))) as super::jobs::Finish)
            })));
        };
        Ok(Started::Job(format!("Bake {name}"), Box::new(move |progress| {
            let (mesh, skin) = job.run(progress)?;
            let made = match &maps {
                Some(maps) => Some(maps.make(&job, &mesh, &name, progress)?),
                None => None,
            };
            Ok(Box::new(move |app: &mut ClideApp| {
                if !app.meshes.install_bake(&job, mesh, skin) {
                    return Err("the clay changed while it was baking; run bake again".into());
                }
                app.bake_reply(job.id, job.settings, true, start, show, made)
            }) as super::jobs::Finish)
        })))
    }

    /// The `bake` reply: counts and an image of the three textures.
    fn bake_reply(&mut self, id: u64, s: Settings, fresh: bool, start: Instant, show: Option<bool>, maps: Option<Value>) -> ToolResult {
        if let Some(show) = show {
            self.meshes.show_baked = show;
        }
        self.meshes.sync(&self.scene);
        let (mesh, skin) = self.meshes.baked(id).ok_or("the bake is gone (the clay changed); run bake again")?;
        let (rest, _) = self.meshes.rest(id).ok_or("not a clay object")?;
        let tex = mesh.textures.clone().ok_or("baked meshes have textures")?;
        let mut out = super::tools::text(json!({
            "target": id,
            "bake": s,
            "rebaked": fresh,
            "seconds": (start.elapsed().as_secs_f32() * 10.0).round() / 10.0,
            "triangles": mesh.indices.len() / 3,
            "vertices": mesh.vertices.len(),
            "clay_triangles": rest.indices.len() / 3,
            "note": if self.scene.get(id).is_some_and(|o|o.shape==Shape::Cloth) { "Pattern UVs and skin are preserved; fine folds and weave are baked into the normal map. Lower cloth resolution if seams prevent your triangle budget." } else { "A bake cannot have more triangles than the clay mesh (clay_triangles); raise the clay resolution for a bigger budget." },
            "skinned": skin.is_some(),
            "shown_in_viewport": self.meshes.show_baked,
            "image": "The baked textures: color | normal map (bumps; flat = light blue) | AO, roughness, metallic as red, green, blue.",
            "maps": maps,
        }));
        let tile = |data: &[u8]| {
            let n = tex.size as usize;
            let pixels = data.chunks_exact(4).map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect();
            super::tools::downscale(&egui::ColorImage::new([n, n], pixels), 512)
        };
        let sheet = super::eyes::contact_sheet(&[tile(&tex.color), tile(&tex.normal), tile(&tex.orm)], 3);
        out.extend(super::tools::encode_png(&sheet)?);
        Ok(out)
    }

}

/// The displacement maps a `bake` call asks for (`maps`, `map_size`,
/// `map_format`, `save_dir`, `verify`): files written by the job, not
/// saved on the object.
struct MapRequest {
    maps: Maps,
    format: Format,
    dir: PathBuf,
    verify: bool,
}

/// Most triangles the `verify` check may dice the simple mesh into.
const VERIFY_TRIANGLES: usize = 4_000_000;

impl MapRequest {
    fn parse(args: &Value, texture: u32, cloth: bool) -> Result<Option<MapRequest>, String> {
        match args["lowpoly"].as_str() {
            None | Some("meshopt") => {}
            Some("retopo") => return Err("lowpoly: retopo needs the Stage 9 retopology, which is not built yet; use meshopt".into()),
            Some(other) => return Err(format!("lowpoly must be meshopt, not {other}")),
        }
        let list = match &args["maps"] {
            Value::Null => return Ok(None),
            Value::Array(list) if !list.is_empty() && list.len() <= 2 => list,
            _ => return Err("maps must list displacement and/or vector_displacement".into()),
        };
        let mut maps = Maps { displacement: false, vector: false, size: texture.min(2048) };
        for m in list {
            match m.as_str() {
                Some("displacement") => maps.displacement = true,
                Some("vector_displacement") => maps.vector = true,
                _ => return Err(format!("unknown map {m}: use displacement or vector_displacement")),
            }
        }
        if cloth {
            return Err("cloth bakes have no displacement maps".into());
        }
        if !args["map_size"].is_null() {
            // Checked as the number given, before any narrowing cast.
            let size = args["map_size"].as_u64().filter(|s| TEXTURE_SIZES.iter().any(|t| *t as u64 == *s) && *s <= MAX_MAP_SIZE as u64);
            let Some(size) = size else {
                return Err(format!("map_size must be one of {:?}", TEXTURE_SIZES.iter().filter(|s| **s <= MAX_MAP_SIZE).collect::<Vec<_>>()));
            };
            maps.size = size as u32;
        }
        let format = Format::parse(&args["map_format"], "map_format", Format::Png)?;
        if args["save_dir"].is_null() {
            return Err("save_dir (an absolute folder) is required with maps".into());
        }
        let dir = crate::outputs::folder(&args["save_dir"], "save_dir")?;
        Ok(Some(MapRequest { maps, format, dir, verify: args["verify"].as_bool().unwrap_or(false) }))
    }

    /// Bakes the maps of `mesh`, writes them, and says where and how to
    /// read them (on the job's thread).
    fn make(&self, job: &crate::mesh::BakeJob, mesh: &crate::mesh::Mesh, name: &str, progress: &crate::progress::Progress) -> Result<Value, String> {
        let t = Instant::now();
        let disp = job.displacement(mesh, self.maps, progress)?;
        let seconds = t.elapsed().as_secs_f32();
        crate::outputs::make_folder(&self.dir)?;
        let size = disp.size as usize;
        // Each map's own code range, so a height map keeps its precision
        // next to a vector map's larger offsets.
        let scale_of = |values: &[f32]| 2.0 * values.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut scales = serde_json::Map::new();
        let stem = crate::outputs::file_stem(name);
        let mut files = serde_json::Map::new();
        // What a reader gets back (PNG codes are rounded to 16 bits).
        let mut read = Displacement { size: disp.size, height: None, vector: None, from_field: 0, missed: 0 };
        let mut write = |kind: &str, channels: Vec<(&str, Vec<f32>)>| -> Result<(), String> {
            let path = self.dir.join(format!("{stem}_{kind}.{}", self.format.ext()));
            crate::outputs::writable(&path, Kind::of(self.format))?;
            match self.format {
                Format::Exr => crate::outputs::write_exr(&path, size, size, channels)?,
                Format::Png => {
                    let n = channels.len();
                    let scale = channels.iter().map(|(_, v)| scale_of(v)).fold(0.0f32, f32::max);
                    scales.insert(kind.into(), json!(scale));
                    let mut interleaved = Vec::with_capacity(size * size * n);
                    for i in 0..size * size {
                        interleaved.extend(channels.iter().map(|(_, v)| v[i]));
                    }
                    let codes = Displacement::encode16(interleaved.into_iter(), scale);
                    let decoded: Vec<f32> = codes.iter().map(|c| Displacement::decode16(*c, scale)).collect();
                    match n {
                        1 => read.height = Some(decoded),
                        _ => read.vector = Some(decoded.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()),
                    }
                    let bytes = crate::outputs::png16(size, size, n, &codes)?;
                    std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
                }
            }
            files.insert(kind.into(), json!(path.display().to_string()));
            Ok(())
        };
        if let Some(h) = &disp.height {
            write("displacement", vec![("Y", h.clone())])?;
        }
        if let Some(v) = &disp.vector {
            write("vector_displacement", vec![("R", v.iter().map(|c| c[0]).collect()), ("G", v.iter().map(|c| c[1]).collect()), ("B", v.iter().map(|c| c[2]).collect())])?;
        }
        if self.format == Format::Exr {
            read.height = disp.height.clone();
            read.vector = disp.vector.clone();
        }
        let mut out = json!({
            "files": files,
            "size": size,
            "format": self.format.ext(),
            "units": "the clay's own (object) units",
            "scale": scales,
            "decode": match self.format {
                Format::Png => "value = (code / 65535 - 0.5) * scale[map]; mid grey (32768) = 0",
                Format::Exr => "float values as they are (channel Y: height; R, G, B: vector)",
            },
            "vector_frame": "R along the +u tangent, G along the bitangent (normal x tangent, toward -v as glTF normal maps), B along the normal; the surface is found along the normal only, so R and G are about 0 (no overhangs or sideways detail)",
            "max_displacement": disp.max(),
            "texels_from_field": disp.from_field,
            "texels_missed": disp.missed,
            "seconds": (seconds * 10.0).round() / 10.0,
        });
        if self.verify {
            out["verify"] = verify(job, mesh, &read, progress)?;
        }
        Ok(out)
    }
}

/// Reapplies the maps as geometry (`film::dice::apply_baked`, as a
/// renderer would) and measures how far the result is from the clay, in
/// cells: the agent sees whether the maps carry the sculpt.
fn verify(job: &crate::mesh::BakeJob, mesh: &crate::mesh::Mesh, read: &Displacement, progress: &crate::progress::Progress) -> Result<Value, String> {
    progress.stage("checking the maps as geometry", 0.97);
    let cell = job.cell().ok_or("the clay is empty")?;
    let diced = match crate::film::dice::apply_baked(mesh, read, cell * 0.5, VERIFY_TRIANGLES) {
        Ok(m) => m,
        Err(e) => return Ok(json!({ "skipped": e })),
    };
    let errors = job.distances(&diced.vertices.iter().map(|v| glam::Vec3::from(v.pos)).collect::<Vec<_>>());
    let max = errors.iter().fold(0.0f32, |m, e| m.max(*e));
    let mean = errors.iter().sum::<f32>() / errors.len().max(1) as f32;
    Ok(json!({ "triangles": diced.indices.len() / 3, "max_error_cells": max / cell, "mean_error_cells": mean / cell, "cell": cell }))
}

/// Stage 8b through the MCP handlers, as an agent calls them: bake maps
/// (16-bit PNG, then EXR from the fresh bake), render passes, and the
/// refusals. Needs a GPU for the editor's render state.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::recorder::tests::{frame, mcp, ok};

    #[test]
    fn bake_maps_and_render_passes_through_the_tools() {
        let Some(rs) = crate::render::test_support::headless() else {
            eprintln!("skipped: no GPU adapter for the editor's render state");
            return;
        };
        let (mut app, tx) = super::ClideApp::headless(rs);
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Vec::new());
        let dir = std::env::temp_dir().join(format!("clide-8b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = |name: &str| dir.join(name).to_str().unwrap().to_string();
        ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Bumpy Ball", "resolution": 48, "blobs": [
            { "type": "sphere", "center": [0, 0, 0], "radius": 0.4 },
            { "type": "sphere", "center": [0, 0, 0.4], "radius": 0.08, "blend": 0.05 },
        ] }));
        let bake = json!({ "target": "Bumpy Ball", "triangles": 800, "texture": 512, "maps": ["displacement", "vector_displacement"], "save_dir": d("maps"), "verify": true });
        let reply = ok(&mut app, &ctx, &tx, "bake", bake.clone());
        let maps = &reply["maps"];
        assert!(maps["scale"]["displacement"].as_f64().unwrap() > 0.0 && maps["scale"]["vector_displacement"].as_f64().unwrap() > 0.0, "{reply}");
        let file = maps["files"]["displacement"].as_str().unwrap();
        assert!(file.ends_with("Bumpy_Ball_displacement.png"), "{file}");
        let (w, h, channels, _) = crate::outputs::read_png16(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!((w, h, channels), (512, 512, 1));
        assert!(maps["verify"]["max_error_cells"].as_f64().unwrap() <= 1.0, "{}", maps["verify"]);
        // The same bake again is fresh: only the maps are made, as EXR.
        let mut again = bake.clone();
        again["map_format"] = json!("exr");
        again["verify"] = json!(false);
        let reply = ok(&mut app, &ctx, &tx, "bake", again);
        assert_eq!(reply["rebaked"], false);
        let exr = reply["maps"]["files"]["vector_displacement"].as_str().unwrap();
        let (_, _, channels) = crate::outputs::read_exr(std::path::Path::new(exr)).unwrap();
        assert_eq!(channels.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["B", "G", "R"]);
        for (args, why) in [
            (json!({ "target": "Bumpy Ball", "maps": ["displacement"], "lowpoly": "retopo", "save_dir": d("maps") }), "Stage 9"),
            (json!({ "target": "Bumpy Ball", "maps": ["height"], "save_dir": d("maps") }), "unknown map"),
            (json!({ "target": "Bumpy Ball", "maps": ["displacement"] }), "save_dir"),
            // 2^32 + 1024 would wrap to 1024 as a u32; 1024.5 is no size.
            (json!({ "target": "Bumpy Ball", "maps": ["displacement"], "map_size": 4294968320u64, "save_dir": d("never") }), "map_size"),
            (json!({ "target": "Bumpy Ball", "maps": ["displacement"], "map_size": 1024.5, "save_dir": d("never") }), "map_size"),
        ] {
            let (v, err) = mcp(&mut app, &ctx, &tx, "bake", args);
            assert!(err && v.to_string().contains(why), "{v}");
        }
        // Passes: every one, as files, next to the picture.
        let film = json!({ "size": 64, "samples": 2, "device": "cpu", "passes": ["depth", "normal", "albedo", "ao", "shadow", "mask", "emission"], "passes_dir": d("passes") });
        let reply = ok(&mut app, &ctx, &tx, "render_film", film.clone());
        let passes = reply["passes"].as_object().unwrap();
        assert_eq!(passes.len(), 7, "{reply}");
        for (name, path) in passes {
            assert!(std::path::Path::new(path.as_str().unwrap()).is_file(), "{name}");
        }
        assert!(reply["passes_info"]["views"][0]["depth_range"][0].as_f64().unwrap() > 0.0);
        let mut blurred = film.clone();
        blurred["motion_blur"] = json!(true);
        blurred["frame"] = json!(1);
        blurred["passes_dir"] = json!(d("never"));
        let (v, err) = mcp(&mut app, &ctx, &tx, "render_film", blurred);
        assert!(err && v.to_string().contains("motion_blur"), "{v}");
        let mut twice = film.clone();
        twice["views"] = json!(["front", "front"]);
        twice["passes_dir"] = json!(d("never"));
        let (v, err) = mcp(&mut app, &ctx, &tx, "render_film", twice);
        assert!(err && v.to_string().contains("distinct names"), "{v}");
        assert!(!dir.join("never").exists(), "refused calls make no output folder");
        let (v, err) = mcp(&mut app, &ctx, &tx, "render_film", json!({ "passes": ["beauty"], "passes_dir": d("passes") }));
        assert!(err && v.to_string().contains("unknown pass"), "{v}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
