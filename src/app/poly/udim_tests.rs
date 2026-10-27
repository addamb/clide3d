//! Stage 4b through MCP, on a headless editor (skips without a GPU
//! adapter): `poly uv udim`, `paint` on a Poly, undo, save and load,
//! `bake udim`, `export_poly udim` and `import_mesh as: poly` reading the
//! tiles back, with the bounds and refusals.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::mcp::ToolCall;
use crate::poly::texture::Channel;

fn headless() -> Option<(ClideApp, Sender<ToolCall>, egui::Context)> {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return None;
    };
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    Some((app, tx, ctx))
}

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

fn poly(app: &ClideApp, id: u64) -> crate::poly::Poly {
    app.scene.get(id).and_then(|o| o.poly.clone()).expect("a Poly")
}

fn refused(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, tool: &str, args: Value, says: &str) {
    let (v, err) = mcp(app, ctx, tx, tool, args.clone());
    assert!(err && v.to_string().contains(says), "{tool} {args}: {v}");
}

/// A painted, bumpy ball retopologized (1,000 quads): (clay, Poly).
fn painted_retopo(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) -> (u64, u64) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Bumpy", "resolution": 64, "detail_cell": 0.01, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "color": [0.3, 0.6, 0.3] }] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Bumpy", "translate": [0.3, 0.2, 0] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Bumpy", "strokes": [{ "brush": "noise", "point": [0, 0, 0], "radius": 0.9, "strength": 1.0, "scale": 0.08, "pattern": "bumps" }] }));
    ok(app, ctx, tx, "paint", json!({ "target": "Bumpy", "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.2, "color": [0.9, 0.1, 0.1] }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "retopo", json!({ "target": "Bumpy", "faces": 1000, "images": false }));
    settle(app, ctx);
    (id_of(app, "Bumpy"), id_of(app, "Bumpy_retopo"))
}

/// The plan's 4b flow through MCP, with its numbers: a UDIM unwrap (each
/// chart on one tile, densities kept), paint on colour and roughness
/// (records, tiles, undo and redo, save and load), `bake udim` (the clay's
/// colour, roughness, normal and AO into the tiles, the strokes on colour
/// and roughness dropped with a note), paint after the bake, then the
/// plan's `udim_export_names` (files `name.<channel>.<UDIM>.png` only for
/// tiles with content, their pixels the tiles' texels) and the re-import
/// giving the same texels; per-tile displacement maps verified from disk;
/// and the refusals.
#[test]
fn udim_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, id) = painted_retopo(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_stage4b_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Before UVs, and the arguments' bounds.
    refused(app, ctx, tx, "paint", json!({ "target": id, "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.1, "color": [1, 0, 0] }] }), "poly uv");
    refused(app, ctx, tx, "poly", json!({ "target": id, "action": "uv", "udim": true, "tiles": 101 }), "tiles must be");
    refused(app, ctx, tx, "poly", json!({ "target": id, "action": "uv", "tiles": 4 }), "udim: true");
    let r = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "uv", "udim": true, "tiles": 4, "size": 1024 }));
    settle(app, ctx);
    eprintln!("udim unwrap: {}", r["udim"]);
    let tiles = r["udim"]["tiles"].as_array().unwrap().len();
    assert!((1..=4).contains(&tiles), "{r}");
    assert_eq!(r["udim"]["charts_crossing_tiles"], 0);
    let ratio = |k: &str| r["udim"]["chart_density"][k].as_f64().unwrap();
    assert!(ratio("min_ratio") >= 0.9 && ratio("max_ratio") <= 1.1, "{r}");
    let start = app.history.current();
    let before = poly(app, id).hash();
    for (args, says) in [
        (json!({ "target": id, "resolution": 16384, "strokes": [] }), "resolution"),
        (json!({ "target": id, "channel": "normal", "strokes": [] }), "channel"),
        (json!({ "target": id, "channel": "roughness", "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.1 }] }), "value"),
        (json!({ "target": id, "layer": "paint", "strokes": [] }), "layer"),
        (json!({ "target": id, "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.1, "color": [1, 0, 0], "alpha": { "name": "x" } }] }), "alpha"),
    ] {
        refused(app, ctx, tx, "paint", args, says);
    }
    let p = ok(app, ctx, tx, "paint", json!({ "target": id, "resolution": 1024, "strokes": [{ "points": [[0.3, 0.7, 0.2], [0.6, 0.4, 0.4], [0.75, 0.1, 0.2]], "radius": 0.12, "color": [0.1, 0.3, 0.9], "snap": true }] }));
    eprintln!("paint: {p}");
    assert!(p["texels_painted"][0].as_u64().unwrap() > 1000 && p["record_ids"] == json!([1]), "{p}");
    let q = ok(app, ctx, tx, "paint", json!({ "target": id, "channel": "roughness", "value": 0.2, "resolution": 512, "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.2, "snap": true }] }));
    assert_eq!(q["record_ids"], json!([2]));
    // The same resolution or none: a channel keeps its own.
    refused(app, ctx, tx, "paint", json!({ "target": id, "resolution": 2048, "strokes": [{ "point": [0.3, 0.2, 0.5], "radius": 0.1, "color": [1, 0, 0] }] }), "1024");
    let painted = poly(app, id);
    let h = painted.hash();
    assert_ne!(h, before);
    // Undo, redo, save and load give the same Poly back.
    app.history_goto(start).unwrap();
    settle(app, ctx);
    assert_eq!(poly(app, id).hash(), before);
    app.history_goto(start + 2).unwrap();
    settle(app, ctx);
    assert_eq!(poly(app, id).hash(), h);
    let path = dir.join("scene.json").display().to_string();
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    let id = id_of(app, "Bumpy_retopo");
    let clay = {
        let _ = clay;
        id_of(app, "Bumpy")
    };
    assert_eq!(poly(app, id).hash(), h, "save and load keep the tiles and records");
    // The drawn level shows the colour.
    let o = app.scene.get(id).unwrap();
    let drawn = app.meshes.get(o).unwrap();
    assert!(drawn.vertices.iter().any(|v| v.color[2] > v.color[0] + 0.3), "the drawn mesh shows the blue stroke");
    // Bake the clay into the tiles.
    refused(app, ctx, tx, "bake", json!({ "target": clay, "lowpoly": { "poly": id } }), "udim: true");
    let b = ok(app, ctx, tx, "bake", json!({ "target": clay, "lowpoly": { "poly": id }, "udim": true, "texture": 512 }));
    settle(app, ctx);
    eprintln!("bake udim: {b}");
    assert_eq!(b["channels"], json!(["color", "roughness", "normal", "ao"]));
    assert!(b["notes"][0].as_str().unwrap().contains("2 paint strokes"), "{b}");
    let baked = poly(app, id);
    let t = &baked.paint.as_ref().unwrap().textures;
    // The clay's red spot and green body came across, fully covering.
    let texels: Vec<[u16; 4]> = t.tiles(Channel::Color).into_iter().flat_map(|k| t.tile_texels(Channel::Color, k).unwrap()).filter(|x| x[3] == 65535).collect();
    let red = texels.iter().filter(|x| x[0] > 40000 && x[1] < 20000).count();
    let green = texels.iter().filter(|x| x[1] > x[0] + 10000).count();
    eprintln!("baked colour: {red} red texels, {green} green");
    assert!(red > 100 && green > 1000);
    for c in [Channel::Normal, Channel::Ao, Channel::Roughness] {
        assert!(!t.tiles(c).is_empty(), "{c:?} baked");
    }
    ok(app, ctx, tx, "paint", json!({ "target": id, "strokes": [{ "point": [0.3, 0.2, -0.5], "radius": 0.1, "color": [1, 1, 0], "snap": true }] }));
    let last = poly(app, id);
    let textures = last.paint.as_ref().unwrap().textures.clone();
    // Export: the plan's udim_export_names.
    let obj = dir.join("ball.obj");
    refused(app, ctx, tx, "export_poly", json!({ "target": id, "path": obj.display().to_string(), "maps": ["normal"], "udim": true }), "maps lists only");
    refused(app, ctx, tx, "export_poly", json!({ "target": id, "path": dir.join("ball.glb").display().to_string(), "udim": true }), "glTF");
    let e = ok(app, ctx, tx, "export_poly", json!({ "target": id, "path": obj.display().to_string(), "udim": true, "maps": [] }));
    settle(app, ctx);
    eprintln!("export: {}", e["files"]["textures"]);
    let mut listed = 0;
    for c in textures.channels() {
        let res = textures.resolution(c).unwrap() as usize;
        for tile in 1001..=1100u16 {
            let file = dir.join(format!("ball.{}.{tile}.png", c.name()));
            let has = textures.tiles(c).contains(&tile);
            assert_eq!(file.exists(), has, "{}: a file only for a tile with content", file.display());
            if !has {
                continue;
            }
            listed += 1;
            let (w, hh, ch, codes) = crate::outputs::read_png16(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!((w, hh, ch), (res, res, 4));
            let want = textures.tile_texels(c, tile).unwrap();
            assert!(codes == bytemuck::cast_slice::<[u16; 4], u16>(&want), "{}: the pixels are the tile's texels", file.display());
        }
    }
    assert!(listed >= 4, "{listed} files");
    // Re-import: the same texels.
    let i = ok(app, ctx, tx, "import_mesh", json!({ "path": obj.display().to_string(), "as": "poly", "name": "Back" }));
    settle(app, ctx);
    eprintln!("import: {}", i["texture_tiles"]);
    let back = poly(app, id_of(app, "Back"));
    assert_eq!(back.paint.as_ref().unwrap().textures.hash(), textures.hash(), "export and re-import keep the texels");
    assert_eq!(back.uv.as_ref().unwrap().to_vec(), last.uv.as_ref().unwrap().to_vec(), "the UDIM UVs come back bit for bit");
    // Per-tile displacement maps, verified from disk.
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "project", "source": clay, "levels": 1 }));
    settle(app, ctx);
    refused(app, ctx, tx, "export_poly", json!({ "target": id, "path": dir.join("one.obj").display().to_string(), "maps": ["vector_displacement"] }), "udim: true");
    let m = ok(app, ctx, tx, "export_poly", json!({ "target": id, "path": dir.join("maps.obj").display().to_string(), "udim": true, "maps": ["vector_displacement"], "map_size": 512 }));
    settle(app, ctx);
    eprintln!("udim maps: {} verify {}", m["files"]["maps"], m["verify"]["maps"]);
    let v = &m["verify"]["maps"];
    assert!(v["max_error_cells"].as_f64().unwrap() <= 0.75 && v["tiles"].as_u64().unwrap() as usize == tiles, "{v}");
    for tile in m["udim"]["tiles"].as_array().unwrap() {
        assert!(dir.join(format!("maps.vector_displacement.{tile}.exr")).exists());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Per-tile displacement maps verify as well as one tile does: the same
/// Poly unwrapped once into one tile and once over UDIM tiles, its level
/// 1 baked into 512 maps (one, or one per tile) and checked from disk.
#[test]
fn udim_maps_verify_as_one_tile() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, id) = painted_retopo(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_stage4b_maps_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "project", "source": clay, "levels": 1 }));
    settle(app, ctx);
    let mut max = Vec::new();
    for udim in [false, true] {
        let uv = if udim { json!({ "target": id, "action": "uv", "udim": true, "tiles": 4, "size": 1024 }) } else { json!({ "target": id, "action": "uv", "size": 1024 }) };
        ok(app, ctx, tx, "poly", uv);
        settle(app, ctx);
        let m = ok(app, ctx, tx, "export_poly", json!({ "target": id, "path": dir.join(format!("m{udim}.obj")).display().to_string(), "udim": udim, "maps": ["vector_displacement"], "map_size": 512 }));
        settle(app, ctx);
        let v = &m["verify"]["maps"];
        eprintln!("udim {udim}: max {} mean {} cells over {} tiles", v["max_error_cells"], v["mean_error_cells"], v["tiles"]);
        max.push(v["max_error_cells"].as_f64().unwrap());
    }
    assert!(max[1] <= 0.75 && max[1] <= 1.5 * max[0] + 0.01, "{max:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 4b review: Poly paint runs as a job and answers like the call
/// it was; `label` (clay records') is refused on a Poly and
/// `pressure_source` is named as not kept; the job undoes as one step.
#[test]
fn poly_paint_is_a_job_and_says_what_it_does_not_keep() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, id) = painted_retopo(app, ctx, tx);
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "uv", "udim": true, "tiles": 2, "size": 512 }));
    settle(app, ctx);
    let stroke = json!([{ "point": [0.3, 0.2, 0.5], "radius": 0.1, "color": [1, 0, 0] }]);
    refused(app, ctx, tx, "paint", json!({ "target": id, "label": "x", "strokes": stroke }), "label is for clay paint");
    let before = poly(app, id);
    let r = ok(app, ctx, tx, "paint", json!({ "target": id, "pressure_source": "velocity", "resolution": 512, "strokes": stroke }));
    settle(app, ctx);
    assert!(r["notes"][0].as_str().is_some_and(|n| n.contains("pressure_source")), "{r}");
    assert!(r["texels_painted"][0].as_u64().unwrap() > 0 && r["object"].is_object(), "{r}");
    assert_ne!(poly(app, id), before);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(poly(app, id) == before, "one undo step");
}
