//! Stage 3a through MCP, on a headless editor (skips without a GPU
//! adapter): `poly` from a sculpted clay, levels, creases, info, undo,
//! save and load, export, `quality_report` and `to_clay` on a Poly, and
//! Page Up sending the request an agent would.

use std::sync::mpsc::Sender;

use egui::{Event, Key, Modifiers};
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::reply_json;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::mcp::ToolCall;
use crate::scene::Shape;

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

/// A ball sculpted with a few strokes, made a Poly.
fn sculpted_poly(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) -> Value {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Ball", "translate": [0.2, 0.1, 0] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [
        { "brush": "inflate", "points": [[0.2, 0.1, 0.5], [0.4, 0.2, 0.4]], "radius": 0.12, "strength": 0.5, "snap": true },
        { "brush": "standard", "points": [[0.2, 0.4, 0.3], [0.0, 0.5, 0.2]], "radius": 0.06, "strength": 0.4, "snap": true },
    ] }));
    settle(app, ctx);
    ok(app, ctx, tx, "poly", json!({ "action": "from_clay", "source": "Ball" }))
}

/// `from_clay` of a sculpted ball is manifold and closed and lies on the
/// clay; levels, creases, info, undo and redo, save and load (the same
/// hash), the drawn and exported triangles, `quality_report`, `rebuild`
/// and `to_clay`.
#[test]
fn poly_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let r = sculpted_poly(app, ctx, tx);
    eprintln!("from_clay: {} faces ({}% quads), irregular {}%, hausdorff {} cells (mean {}), {}", r["faces"], r["quads_pct"], r["irregular_pct"], r["hausdorff_cells"], r["mean_distance_cells"], r["from"]);
    assert_eq!((r["non_manifold_edges"].as_u64(), r["open_edges"].as_u64(), r["flipped_edges"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
    assert_eq!((r["boundary_loops"].as_i64(), r["genus"].as_i64(), r["components"].as_i64()), (Some(0), Some(0), Some(1)));
    assert!(r["hausdorff_cells"].as_f64().unwrap() < 0.5, "{r}");
    assert_eq!((r["cage_points"].clone(), r["non_manifold_points"].as_u64()), (r["points"].clone(), Some(0)), "from_clay's cage uses every point and splits pinches");
    assert!(r["distance_samples"].as_u64().is_some_and(|n| n > 0 && n <= 20_000), "{r}");
    assert!(r["quads_pct"].as_f64().unwrap() > 90.0);
    let id = id_of(app, "Ball_poly");
    let o = app.scene.get(id).unwrap();
    assert_eq!((o.shape, o.translation), (Shape::Poly, glam::Vec3::new(0.2, 0.1, 0.0)), "in the clay's place");
    assert!(!app.scene.get(id_of(app, "Ball")).unwrap().visible, "the clay is hidden");
    let faces = r["faces"].as_u64().unwrap();
    // Wound outward: the cage and its levels enclose a positive volume.
    let cage = crate::poly::subdiv::evaluate(app.scene.get(id).unwrap().poly.as_ref().unwrap(), 0).unwrap();
    let v0 = crate::mesh_asset::volume(&cage.points, &cage.triangles());
    assert!(v0 > 0.4, "the cage winds outward: volume {v0}");
    // Levels: 6 would pass 16M faces and is refused, changing nothing.
    let (err, failed) = mcp(app, ctx, tx, "poly", json!({ "target": id, "action": "subdivide", "levels": 6 }));
    assert!(failed && err.to_string().contains("at most"), "{err}");
    let s = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "subdivide", "levels": 2 }));
    assert_eq!((s["levels"].as_u64(), s["level"].as_u64()), (Some(2), Some(2)), "{s}");
    let corners = 4 * faces - r["triangles"].as_u64().unwrap();
    assert_eq!(s["level_faces"], json!([faces, corners, 4 * corners]));
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "set_level", "level": 1 }));
    settle(app, ctx);
    let drawn = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    assert_eq!(drawn.indices.len() / 3, 2 * corners as usize, "level 1 drawn as two triangles per quad");
    let (pts, tris): (Vec<glam::Vec3>, Vec<[u32; 3]>) = (drawn.vertices.iter().map(|v| glam::Vec3::from(v.pos)).collect(), drawn.indices.chunks(3).map(|t| [t[0], t[1], t[2]]).collect());
    let v1 = crate::mesh_asset::volume(&pts, &tris);
    eprintln!("volume: cage {v0:.4}, level 1 {v1:.4}");
    assert!(v1 > 0.4 && v1 < v0, "level 1 winds outward and lies inside its cage");
    assert_eq!(drawn.edges.len() / 2, (2 * r["edges"].as_u64().unwrap() + corners) as usize, "the quads' sides as the wire");
    assert!(mcp(app, ctx, tx, "poly", json!({ "target": id, "action": "set_level", "level": 3 })).1, "past the levels kept");
    // Creases by angle and by edge; undo and redo.
    let before = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"].clone();
    // By angle: every edge whose faces meet more sharply than half the
    // sharpest (counted here from the cage).
    let (dihedral, cage) = {
        let p = app.scene.get(id).unwrap().poly.as_ref().unwrap();
        (p.topology(), p.cage.to_vec())
    };
    let normals: Vec<glam::Vec3> = (0..dihedral.faces()).map(|f| crate::poly::subdiv::newell(&cage, dihedral.face(f)).normalize()).collect();
    let angles: Vec<f32> = (0..dihedral.edges.len()).map(|e| { let f = dihedral.faces_of_edge(e); normals[f[0] as usize].angle_between(normals[f[1] as usize]).to_degrees() }).collect();
    let half = angles.iter().copied().fold(0.0, f32::max) / 2.0;
    let c = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "crease", "angle": half, "sharpness": 2.5 }));
    let by_angle = c["creases"].as_u64().unwrap();
    assert_eq!(by_angle as usize, angles.iter().filter(|a| **a > half).count(), "{c}");
    assert!(by_angle > 0);
    let edge = app.scene.get(id).unwrap().poly.as_ref().unwrap().topology().edges[0];
    let c = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "crease", "edges": [[edge[1], edge[0]]] }));
    assert!(c["creases"].as_u64().unwrap() >= by_angle && c["infinite"].as_u64().unwrap() >= 1);
    let (bad, failed) = mcp(app, ctx, tx, "poly", json!({ "target": id, "action": "crease", "edges": [[0, 0]] }));
    assert!(failed && bad.to_string().contains("not an edge"), "{bad}");
    // Out-of-range points are refused, not a panic.
    for pair in [json!([0, 4_000_000_000u64]), json!([4_294_967_295u64, 0]), json!([99_999_999, 99_999_998]), json!([5_000_000_000u64, 1]), json!([-1, 2])] {
        let (bad, failed) = mcp(app, ctx, tx, "poly", json!({ "target": id, "action": "crease", "edges": [pair] }));
        assert!(failed, "{pair}: {bad}");
    }
    for (action, args) in [("set_level", json!({ "level": 99 })), ("subdivide", json!({ "levels": 300 })), ("set_level", json!({ "level": -1 }))] {
        let mut call = args.clone();
        call["target"] = json!(id);
        call["action"] = json!(action);
        assert!(mcp(app, ctx, tx, "poly", call).1, "{action} {args}");
    }
    let after = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"].clone();
    assert_ne!(before, after);
    ok(app, ctx, tx, "undo", json!({}));
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"], before, "undo gives the object back");
    ok(app, ctx, tx, "redo", json!({}));
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"], after, "redo too");
    // Save and load: the same object, bit for bit.
    let dir = std::env::temp_dir().join(format!("clide3d_stage3a_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("poly.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    let kept = app.scene.get(id).unwrap().clone();
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    let loaded = app.scene.get(id).unwrap();
    assert_eq!(loaded.poly.as_ref().unwrap().hash(), kept.poly.as_ref().unwrap().hash());
    assert!(*loaded == kept);
    assert_eq!(ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"], after);
    // Exported at the shown level.
    let glb = dir.join("poly.glb");
    ok(app, ctx, tx, "export_glb", json!({ "path": glb, "targets": [id] }));
    let read = crate::glb_read::read(&glb, None, None, &Default::default()).unwrap();
    let triangles: usize = read.items.iter().map(|i| i.mesh.indices.len() / 3).sum();
    assert_eq!(triangles, 2 * corners as usize, "export_glb takes the shown level");
    let print = ok(app, ctx, tx, "export_print", json!({ "path": dir.join("poly.stl"), "targets": [id] }));
    assert_eq!((print["watertight"].as_bool(), print["triangles"].as_u64()), (Some(true), Some(2 * corners)), "{print}");
    ok(app, ctx, tx, "export_fbx", json!({ "path": dir.join("poly.fbx"), "targets": [id] }));
    assert!(std::fs::metadata(dir.join("poly.fbx")).unwrap().len() > 1000);
    // quality_report on a Poly: closed, on its clay.
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": id }));
    eprintln!("quality_report on the Poly (level 1): {}", q["poly"]);
    assert_eq!(q["pass"], true, "{q}");
    assert_eq!(q["topology"]["watertight"], true);
    assert!(q["poly"]["hausdorff_cells"].as_f64().unwrap() < 1.0);
    let strict = ok(app, ctx, tx, "quality_report", json!({ "target": id, "thresholds": { "hausdorff_cells": 0.0 } }));
    assert_eq!(strict["pass"], false, "{strict}");
    // rebuild: the same clay gives the same cage, so creases and levels stay.
    let rb = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "rebuild" }));
    assert_eq!((rb["levels"].as_u64(), rb["creases"]["edges"].as_u64()), (Some(2), c["creases"].as_u64()), "{rb}");
    // to_clay in place.
    let t = ok(app, ctx, tx, "poly", json!({ "target": id, "action": "to_clay", "resolution": 48 }));
    let o = app.scene.get(id).unwrap();
    assert!(o.shape == Shape::Clay && o.poly.is_none() && o.clay.is_some(), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Page Up and Page Down step the shown level through `poly set_level`:
/// the request, sent through MCP on the scene before it, gives the same
/// reply and the same object.
#[test]
fn ui_poly_level_matches_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sculpted_poly(app, ctx, tx);
    let id = id_of(app, "Ball_poly");
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "subdivide", "levels": 2 }));
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "set_level", "level": 0 }));
    settle(app, ctx);
    let key = |key: Key| vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }, Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }];
    let level = |app: &ClideApp| app.scene.get(id).unwrap().poly.as_ref().unwrap().level;
    // Up twice, up past the finest (nothing sent), down once.
    for (k, want) in [(Key::PageUp, Some(1u8)), (Key::PageUp, Some(2)), (Key::PageUp, None), (Key::PageDown, Some(1))] {
        let step = app.history.current();
        let sent = app.ui_activity.len();
        frame(app, ctx, key(k));
        settle(app, ctx);
        let Some(want) = want else {
            assert_eq!((app.ui_activity.len(), level(app)), (sent, 2), "past the finest level nothing is sent");
            continue;
        };
        assert_eq!(app.ui_activity.len(), sent + 1, "{k:?}: one request");
        assert_eq!(level(app), want);
        let a = app.ui_activity.back().unwrap();
        assert_eq!(a.tool, "poly");
        let request = a.request.clone();
        assert_eq!((request["action"].as_str(), request["level"].as_u64()), (Some("set_level"), Some(want as u64)));
        let reply = reply_json(app.jobs.result(a.job).unwrap().as_ref().unwrap());
        let after = app.scene.get(id).unwrap().clone();
        app.history_goto(step).unwrap();
        settle(app, ctx);
        let agent = ok(app, ctx, tx, "poly", request);
        assert_eq!(agent, reply, "the agent's reply equals the UI's");
        assert!(*app.scene.get(id).unwrap() == after, "and leaves the same object");
    }
    assert_eq!(level(app), 1);
}


/// §0.2 rule 4 for `from_clay`'s kernels (it runs on a job thread, where
/// the MCP tests cannot set the thread count): the cage made from a
/// clay's mesh and its distance to the clay are the same on 1 and 12
/// threads.
#[test]
fn from_clay_is_thread_independent() {
    use crate::clay::{Blob, Clay, Form, Op};
    let mut clay = Clay { blobs: vec![Blob::of(Form::Sphere { center: glam::Vec3::ZERO, radius: 0.5 }, Op::Add, [0.8; 3]), Blob::of(Form::Sphere { center: glam::Vec3::new(0.0, 0.0, 0.45), radius: 0.15 }, Op::Add, [0.8; 3])], ..Clay::default() };
    clay.resolution = 64;
    let mesh = clay.build_mesh();
    let points: Vec<glam::Vec3> = mesh.vertices.iter().map(|v| glam::Vec3::from(v.pos)).collect();
    let cell = clay.cell_size().unwrap();
    let run = || {
        let m = crate::poly::from_mesh::from_triangles(&points, &mesh.indices, &mesh.edges, cell * 1e-3).unwrap();
        let d = super::distance_cells(&clay, &m.poly.cage.to_vec()).unwrap();
        (m.poly.hash(), d.0.to_bits(), d.1.to_bits())
    };
    let one = crate::clay::with_threads(1, run);
    let many = crate::clay::with_threads(12, run);
    assert_eq!(one, many);
}

/// `crease angle` is bounded as it picks edges, before anything is kept:
/// a 40,000-quad torus has 80,000 edges, all bent, so angle 0 is refused
/// (saying a larger angle picks fewer), while a removal by angle picks
/// only the edges creased now and so is never refused.
#[test]
fn crease_by_angle_is_bounded_before_it_is_kept() {
    let mut p = crate::poly::tests::torus(200, 200);
    let t = p.topology();
    assert!(t.edges.len() > crate::poly::MAX_CREASES);
    let err = super::edges_over_angle(&p, &t, 0.0, false).unwrap_err();
    assert!(err.contains("larger angle"), "{err}");
    assert!(super::edges_over_angle(&p, &t, 0.0, true).unwrap().is_empty(), "nothing creased: nothing to remove");
    let [a, b] = t.edges[5];
    p.creases = std::sync::Arc::new([((a.min(b), a.max(b)), 3.0)].into_iter().collect());
    assert_eq!(super::edges_over_angle(&p, &t, 0.0, true).unwrap(), vec![(a.min(b), a.max(b))]);
    // A larger angle picks fewer: none of the torus's bends is a right angle.
    assert!(super::edges_over_angle(&p, &t, 60.0, false).unwrap().is_empty());
}

/// A tool call through the MCP dispatcher, with the reply's whole content
/// (text and images) and whether it is an error.
fn mcp_content(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, args: Value) -> (Vec<Value>, bool) {
    let msg = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (tx, c) = (tx.clone(), ctx.clone());
    let call = std::thread::spawn(move || crate::mcp::dispatch(&msg, &tx, &c));
    let start = std::time::Instant::now();
    while !call.is_finished() {
        frame(app, ctx, Vec::new());
        assert!(start.elapsed() < std::time::Duration::from_secs(300), "{name} never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r = call.join().expect("dispatch thread").expect("a reply")["result"].clone();
    (r["content"].as_array().cloned().unwrap_or_default(), r["isError"] == true)
}

/// A head-like clay: an ellipsoid with a nose and a brow, X-symmetric.
fn head_clay(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) -> u64 {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 80, "blobs": [
        { "type": "ellipsoid", "center": [0, 0, 0], "radii": [0.32, 0.42, 0.36] },
        { "type": "sphere", "center": [0, -0.02, 0.36], "radius": 0.07, "blend": 0.05 },
        { "type": "ellipsoid", "center": [0, 0.12, 0.27], "radii": [0.24, 0.06, 0.08], "blend": 0.06 },
    ] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Head", "translate": [0.5, 1.0, 0] }));
    settle(app, ctx);
    id_of(app, "Head")
}

/// Stage 3b through MCP: `retopo` makes a clean quad cage in the clay's
/// place with its numbers and a wireframe image; oversize calls are
/// refused before any work; undo and redo, save and load, `poly rebuild`
/// and `quality_report` on the result.
#[test]
fn retopo_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let head = head_clay(app, ctx, tx);
    // Refused from the arguments alone, before a job starts.
    let jobs = app.jobs.running.len();
    let guide = |n: usize| json!((0..n).map(|k| [0.5 + 0.001 * k as f32, 1.0, 0.36]).collect::<Vec<_>>());
    for (args, says) in [
        (json!({ "target": head, "faces": 300 }), "faces must be"),
        (json!({ "target": head, "faces": 200001 }), "faces must be"),
        (json!({ "target": head, "faces": -5 }), "faces must be"),
        (json!({ "target": head, "guides": (0..65).map(|_| guide(2)).collect::<Vec<_>>() }), "at most 64 guides"),
        (json!({ "target": head, "guides": [guide(257)] }), "2 to 256"),
        (json!({ "target": head, "guides": [guide(1)] }), "2 to 256"),
        (json!({ "target": head, "guides": [[[0, 0, "x"], [1, 1, 1]]] }), "[x, y, z]"),
        (json!({ "target": head, "density": (0..17).map(|_| json!({ "center": [0, 0, 0], "radius": 0.1 })).collect::<Vec<_>>() }), "at most 16"),
        (json!({ "target": head, "density": [{ "center": [0, 0, 0], "radius": 0.1, "scale": 5 }] }), "scale must be"),
        (json!({ "target": head, "density": [{ "center": [0, 0, 0], "radius": -1 }] }), "radius"),
        (json!({ "target": head, "symmetry": "y" }), "symmetry must be"),
        (json!({ "target": head, "adaptive": 2 }), "adaptive must be"),
        (json!({ "target": head, "keep_creases": 181 }), "keep_creases must be"),
        (json!({ "target": head, "source": "baked" }), "source must be"),
        (json!({ "target": head, "guides": [[[-1e5, 1, 0], [1e5, 1, 0]]] }), "bound"),
        (json!({}), "target"),
    ] {
        let (err, failed) = mcp(app, ctx, tx, "retopo", args.clone());
        assert!(failed && err.to_string().contains(says), "{args}: {err}");
        assert_eq!(app.jobs.running.len(), jobs, "refused before a job started");
    }
    let step = app.history.current();
    let (content, failed) = mcp_content(app, ctx, tx, "retopo", json!({ "target": "Head", "faces": 2000 }));
    let r = super::super::recorder::reply_json(&content);
    assert!(!failed, "{r}");
    eprintln!("retopo: {} faces ({}% quads, {}% irregular), before split {}, hausdorff {} cells, coverage {}, symmetric {} ({}), {} s, notes {}", r["faces"], r["quads_pct"], r["irregular_pct"], r["before_split"], r["hausdorff_cells"], r["coverage_cells"], r["symmetric"], r["symmetric_error"], r["seconds"], r["notes"]);
    assert!(content.iter().any(|c| c["type"] == "image"), "a wireframe image");
    assert_eq!(r["quads_pct"].as_f64(), Some(100.0));
    assert!(r["before_split"]["quads_pct"].as_f64().unwrap() >= 92.0, "{r}");
    let faces = r["faces"].as_u64().unwrap();
    assert!((1700..=2300).contains(&faces), "{faces} faces for 2000");
    assert!(r["hausdorff_cells"].as_f64().unwrap() <= 0.75, "{r}");
    assert_eq!((r["symmetric"].as_bool(), r["symmetric_error"].as_f64()), (Some(true), Some(0.0)), "{r}");
    assert_eq!(r["topology"], json!({ "open_edges": 0, "non_manifold_edges": 0, "flipped_edges": 0, "non_manifold_points": 0, "genus": 0, "components": 1 }));
    let id = id_of(app, "Head_retopo");
    let o = app.scene.get(id).unwrap();
    assert_eq!((o.shape, o.translation), (Shape::Poly, glam::Vec3::new(0.5, 1.0, 0.0)), "in the clay's place");
    assert!(!app.scene.get(head).unwrap().visible, "the clay is hidden");
    let made = o.poly.as_ref().unwrap().made_by.clone().unwrap();
    assert_eq!((made["action"].as_str(), made["source"].as_u64(), made["faces"].as_u64(), made["space"].as_str()), (Some("retopo"), Some(head), Some(2000), Some("object")));
    let hash = r["hash"].clone();
    assert_eq!(ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"], hash);
    // One undo step: the Poly goes and the clay shows again; redo is the same.
    assert_eq!(app.history.current(), step + 1);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.scene.get(id).is_none() && app.scene.get(head).unwrap().visible);
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(ok(app, ctx, tx, "poly", json!({ "target": id, "action": "info" }))["hash"], hash, "redo gives it back");
    // Rebuild: the same call on the same clay makes the same cage, and keeps levels.
    ok(app, ctx, tx, "poly", json!({ "target": id, "action": "subdivide", "levels": 1 }));
    let before_rebuild = app.scene.get(id).unwrap().poly.clone().unwrap();
    let (content, failed) = mcp_content(app, ctx, tx, "poly", json!({ "target": id, "action": "rebuild", "images": false }));
    let rb = super::super::recorder::reply_json(&content);
    assert!(!failed, "{rb}");
    assert!(!content.iter().any(|c| c["type"] == "image"), "images: false is honoured");
    assert_eq!((rb["levels"].as_u64(), rb["faces"].as_u64()), (Some(1), Some(faces)), "{rb}");
    let after_rebuild = app.scene.get(id).unwrap().poly.clone().unwrap();
    assert!(after_rebuild == before_rebuild, "the same call on the same clay makes the same cage");
    assert_eq!(after_rebuild.hash(), before_rebuild.hash());
    // Save and load: the same object, made_by kept.
    let dir = std::env::temp_dir().join(format!("clide3d_stage3b_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("retopo.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    let kept = app.scene.get(id).unwrap().clone();
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    let loaded = app.scene.get(id).unwrap();
    assert_eq!(loaded.poly.as_ref().unwrap().hash(), kept.poly.as_ref().unwrap().hash());
    assert!(*loaded == kept);
    // quality_report reads the clay it was made from.
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": id }));
    eprintln!("quality_report on the retopo: {}", q["poly"]);
    assert_eq!((q["pass"].as_bool(), q["poly"]["source"].as_u64()), (Some(true), Some(head)), "{q}");
    // A guide and a density region (world space) reach made_by in the clay's space.
    let g = ok(app, ctx, tx, "retopo", json!({ "target": head, "faces": 1000, "name": "Guided", "images": false, "symmetry": "none", "guides": [[[0.4, 1.0, 0.36], [0.6, 1.0, 0.36]]], "density": [{ "center": [0.5, 1.2, 0.3], "radius": 0.1, "scale": 0.5 }] }));
    let made = app.scene.get(id_of(app, "Guided")).unwrap().poly.as_ref().unwrap().made_by.clone().unwrap();
    let near = |v: &Value, want: [f32; 3]| v.as_array().unwrap().iter().zip(want).all(|(a, b)| (a.as_f64().unwrap() as f32 - b).abs() < 1e-5);
    assert!(near(&made["guides"][0][0], [-0.1, 0.0, 0.36]), "{made}");
    assert!(near(&made["density"][0]["center"], [0.0, 0.2, 0.3]), "{made}");
    assert_eq!(g["symmetric"].as_bool(), Some(false));
    // source: fine fits the fine (detail) surface when the clay has one,
    // in its detail cells; without one it says the rest mesh was used.
    let rest = ok(app, ctx, tx, "retopo", json!({ "target": head, "faces": 1000, "name": "Rest", "images": false, "source": "fine" }));
    assert_eq!(rest["input"]["mesh"], "rest", "{rest}");
    assert!(rest["notes"].to_string().contains("no detail regions"), "{rest}");
    ok(app, ctx, tx, "edit_clay", json!({ "target": head, "detail_cell": 0.008 }));
    let fine = ok(app, ctx, tx, "retopo", json!({ "target": head, "faces": 1000, "name": "Fine", "images": false, "source": "fine" }));
    eprintln!("retopo of the fine surface: {} faces, hausdorff {} detail cells, input {}", fine["faces"], fine["hausdorff_cells"], fine["input"]);
    assert_eq!((fine["input"]["mesh"].as_str(), fine["input"]["cell"].as_f64().map(|c| (c - 0.008).abs() < 1e-6)), (Some("fine"), Some(true)), "{fine}");
    assert_eq!(fine["topology"]["open_edges"], 0);
    let made = app.scene.get(id_of(app, "Fine")).unwrap().poly.as_ref().unwrap().made_by.clone().unwrap();
    assert_eq!(made["mesh"], "fine");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Modify > Retopologize… sends the `retopo` request an agent would: the
/// same request through MCP on the scene before it gives the same reply
/// and object.
#[test]
fn ui_retopo_matches_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let head = head_clay(app, ctx, tx);
    app.select_only(head);
    app.open_retopo_dialog();
    let d = app.retopo_dialog.as_mut().expect("a clay is selected");
    d.faces = 1200;
    let step = app.history.current();
    let sent = app.ui_activity.len();
    let enter = vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];
    frame(app, ctx, enter);
    settle(app, ctx);
    assert!(app.retopo_dialog.is_none(), "the dialog closes");
    assert_eq!(app.ui_activity.len(), sent + 1, "one request");
    let a = app.ui_activity.back().unwrap();
    assert_eq!(a.tool, "retopo");
    let request = a.request.clone();
    assert_eq!(request, json!({ "target": head, "faces": 1200, "adaptive": 0.3f32, "symmetry": "auto", "keep_creases": 40.0, "pure_quads": true }));
    let mut reply = reply_json(app.jobs.result(a.job).unwrap().as_ref().unwrap());
    let made = id_of(app, "Head_retopo");
    let after = app.scene.get(made).unwrap().clone();
    app.history_goto(step).unwrap();
    settle(app, ctx);
    let mut agent = ok(app, ctx, tx, "retopo", request);
    for r in [&mut reply, &mut agent] {
        for k in ["seconds", "timings"] {
            r[k] = Value::Null;
        }
    }
    assert_eq!(agent, reply, "the agent's reply equals the UI's (timings aside)");
    assert!(*app.scene.get(made).unwrap() == after, "and leaves the same object");
}

/// A noise-bumped ball retopologized: (clay, Poly).
fn bumpy_retopo(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) -> (u64, u64) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Bumpy", "resolution": 64, "detail_cell": 0.01, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Bumpy", "translate": [0.3, 0.2, 0] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Bumpy", "strokes": [{ "brush": "noise", "point": [0, 0, 0], "radius": 0.9, "strength": 1.0, "scale": 0.08, "pattern": "bumps" }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "retopo", json!({ "target": "Bumpy", "faces": 1000, "images": false }));
    settle(app, ctx);
    (id_of(app, "Bumpy"), id_of(app, "Bumpy_retopo"))
}

/// Stage 3c's `poly project` through MCP: the clay's unchecked ledger is
/// settled first (`settled`), the detail lands on the levels (distance and
/// normals), the same call gives the same object, refusals come from the
/// arguments, and undo, redo, save and load give the object back.
#[test]
fn poly_project_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, poly) = bumpy_retopo(app, ctx, tx);
    // Refused from the arguments (the last on the job's thread, once the
    // cage's size is known).
    for (args, says) in [
        (json!({ "levels": 0 }), "levels"),
        (json!({ "levels": 7 }), "levels"),
        (json!({ "levels": 2, "cone": 95 }), "cone"),
        (json!({ "levels": 2, "reach": -1 }), "reach"),
        (json!({ "levels": 2, "reach": 1e30 }), "reach"),
        (json!({ "levels": 2, "mode": "sideways" }), "mode"),
        (json!({ "levels": 2, "source": poly }), "not a Clay"),
        (json!({ "levels": 2, "reach": 50 }), "cage's size"),
    ] {
        let mut call = args.clone();
        call["target"] = json!(poly);
        call["action"] = json!("project");
        let (e, failed) = mcp(app, ctx, tx, "poly", call);
        assert!(failed && e.to_string().contains(says), "{args}: {e}");
    }
    // The clay as a file leaves it: its ledger unchecked.
    let text = serde_json::to_string(app.scene.get(clay).unwrap().clay.as_ref().unwrap()).unwrap();
    let mut unchecked: crate::clay::Clay = serde_json::from_str(&text).unwrap();
    crate::ledger::after_load_by(&mut unchecked, std::time::Instant::now());
    assert!(unchecked != *app.scene.get(clay).unwrap().clay.as_ref().unwrap(), "unchecked differs until settled");
    app.scene.get_mut(clay).unwrap().clay = Some(unchecked);
    let before = app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash();
    let steps = app.history.current();
    let r = ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 3 }));
    let projected_step = app.history.current();
    eprintln!("project: {r}");
    assert_eq!(r["settled"], json!(true), "the unchecked ledger settled first");
    assert_eq!(r["source"], json!(clay), "the source from made_by");
    assert_eq!((r["levels"].as_u64(), r["level"].as_u64(), r["mode"].as_str()), (Some(3), Some(3), Some("vector")));
    assert!(r["hausdorff_cells"].as_f64().unwrap() < 0.5 && r["normal_rms_deg"].as_f64().unwrap() < 4.0, "{r}");
    assert_eq!(r["per_level"].as_array().unwrap().len(), 3);
    let after = app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash();
    assert_ne!(before, after);
    let info = ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "info" }));
    assert_eq!((info["displaced_levels"].clone(), info["projected"]["source"].clone()), (json!([1, 2, 3]), json!(clay)), "{info}");
    // Again: nothing to settle, the same object (deterministic), the
    // source now from `projected`.
    let again = ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 3 }));
    assert_eq!(again["settled"], json!(false));
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), after);
    // The same object again is no new undo step; undo and redo.
    assert_eq!(app.history.current(), projected_step, "an unchanged object adds no step");
    assert!(projected_step > steps);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), before, "undo takes the detail away");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), after);
    // quality_report on the Poly reads its shown (projected) level.
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": poly, "views": [] }));
    eprintln!("quality_report: {}", q["poly"]);
    assert!(q["poly"]["hausdorff_cells"].as_f64().unwrap() < 0.5, "{q}");
    // Save and load: the same object, bit for bit.
    let dir = std::env::temp_dir().join(format!("clide3d_stage3c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("projected.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    let kept = app.scene.get(poly).unwrap().clone();
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    let loaded = app.scene.get(poly).unwrap();
    assert_eq!(loaded.poly.as_ref().unwrap().hash(), kept.poly.as_ref().unwrap().hash());
    assert!(*loaded == kept);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Modify > Project Clay Detail… sends the `poly project` an agent would,
/// and the agent's reply and object match the UI's.
#[test]
fn ui_project_matches_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, poly) = bumpy_retopo(app, ctx, tx);
    app.select_only(poly);
    app.open_project_dialog();
    let d = app.project_dialog.as_mut().expect("a Poly is selected");
    assert_eq!(d.source, Some(clay), "the clay it was made from");
    d.levels = 2;
    let step = app.history.current();
    let sent = app.ui_activity.len();
    let enter = vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];
    frame(app, ctx, enter);
    settle(app, ctx);
    assert!(app.project_dialog.is_none(), "the dialog closes");
    assert_eq!(app.ui_activity.len(), sent + 1, "one request");
    let a = app.ui_activity.back().unwrap();
    assert_eq!(a.tool, "poly");
    let request = a.request.clone();
    assert_eq!(request, json!({ "target": poly, "action": "project", "source": clay, "levels": 2, "mode": "vector", "cone": 45.0 }));
    let mut reply = reply_json(app.jobs.result(a.job).unwrap().as_ref().unwrap());
    let after = app.scene.get(poly).unwrap().clone();
    app.history_goto(step).unwrap();
    settle(app, ctx);
    let mut agent = ok(app, ctx, tx, "poly", request);
    for r in [&mut reply, &mut agent] {
        r["seconds"] = Value::Null;
    }
    assert_eq!(agent, reply, "the agent's reply equals the UI's (time aside)");
    assert!(*app.scene.get(poly).unwrap() == after, "and leaves the same object");
}

/// `poly uv` through MCP: refusals from the arguments, one tile with
/// every corner in it, info's uv, undo, save and load.
#[test]
fn poly_uv_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, poly) = bumpy_retopo(app, ctx, tx);
    for (args, says) in [(json!({ "udim": 1 }), "udim"), (json!({ "size": 3000 }), "size"), (json!({ "padding_px": 99 }), "padding_px"), (json!({ "method": "lscm" }), "method")] {
        let mut call = args.clone();
        call["target"] = json!(poly);
        call["action"] = json!("uv");
        let (e, failed) = mcp(app, ctx, tx, "poly", call);
        assert!(failed && e.to_string().contains(says), "{args}: {e}");
    }
    let before = app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash();
    let r = ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "uv", "size": 1024 }));
    eprintln!("uv: {r}");
    let p = app.scene.get(poly).unwrap().poly.clone().unwrap();
    let uv = p.uv.as_ref().unwrap().to_vec();
    assert_eq!((uv.len(), r["corners"].as_u64()), (p.corners.len(), Some(p.corners.len() as u64)));
    assert!(uv.iter().flatten().all(|x| (0.0..=1.0).contains(x)));
    assert!(r["charts"].as_u64().unwrap() >= 1 && r["seams"].as_u64().unwrap() > 0);
    let info = ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "info" }));
    assert_eq!(info["uv"]["seams"], r["seams"], "{info}");
    let after = p.hash();
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), before);
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), after);
    let dir = std::env::temp_dir().join(format!("clide3d_stage3c_uv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("uv.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    assert_eq!(app.scene.get(poly).unwrap().poly.as_ref().unwrap().hash(), after);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Modify > Unwrap Poly UVs sends the `poly uv` an agent would; the
/// replies (time aside) and objects match.
#[test]
fn ui_uv_matches_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, poly) = bumpy_retopo(app, ctx, tx);
    app.select_only(poly);
    let step = app.history.current();
    let sent = app.ui_activity.len();
    app.unwrap_lead_poly();
    settle(app, ctx);
    assert_eq!(app.ui_activity.len(), sent + 1, "one request");
    let a = app.ui_activity.back().unwrap();
    let request = a.request.clone();
    assert_eq!(a.tool, "poly");
    assert_eq!(request, json!({ "target": poly, "action": "uv" }));
    let mut reply = reply_json(app.jobs.result(a.job).unwrap().as_ref().unwrap());
    let after = app.scene.get(poly).unwrap().clone();
    app.history_goto(step).unwrap();
    settle(app, ctx);
    let mut agent = ok(app, ctx, tx, "poly", request);
    for r in [&mut reply, &mut agent] {
        r["seconds"] = Value::Null;
    }
    assert_eq!(agent, reply);
    assert!(*app.scene.get(poly).unwrap() == after);
}

/// Distinct positions of a mesh (vertices split at UV seams count once).
fn distinct_positions(m: &crate::mesh::Mesh) -> usize {
    let mut keys: Vec<[u32; 3]> = m.vertices.iter().map(|v| v.pos.map(f32::to_bits)).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.len()
}

/// Stage 3b's leftover, `bake_uses_retopo`, through MCP: `bake lowpoly:
/// {poly, level}` makes the Poly's level the game mesh (its points, two
/// triangles a quad), its maps' verify error against the meshopt bake's
/// at the same triangle count, the refusals, the bake kept fresh until
/// the Poly changes, and a projection carried into the maps.
#[test]
fn bake_uses_retopo() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, poly) = bumpy_retopo(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_stage3c_bake_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (lowpoly, says) in [(json!({ "poly": clay }), "Poly"), (json!({ "poly": poly, "level": 7 }), "level"), (json!("retopo"), "meshopt or"), (json!({ "level": 1 }), "poly")] {
        let (e, failed) = mcp(app, ctx, tx, "bake", json!({ "target": clay, "lowpoly": lowpoly }));
        assert!(failed && e.to_string().contains(says), "{lowpoly}: {e}");
    }
    let maps = |lowpoly: Value, triangles: u64| json!({ "target": clay, "lowpoly": lowpoly, "triangles": triangles, "texture": 1024, "maps": ["vector_displacement"], "map_size": 1024, "map_format": "exr", "save_dir": dir, "verify": true });
    let p = app.scene.get(poly).unwrap().poly.clone().unwrap();
    let r = ok(app, ctx, tx, "bake", maps(json!({ "poly": poly, "level": 0 }), 500));
    eprintln!("bake on the retopology: {} triangles, {}, verify {}", r["triangles"], r["lowpoly"], r["maps"]["verify"]);
    let baked = app.meshes.baked(clay).unwrap().0.clone();
    let quads = p.faces();
    assert_eq!((r["triangles"].as_u64(), distinct_positions(&baked)), (Some(2 * quads as u64), p.info()["points"].as_u64().unwrap() as usize), "the cage's points, two triangles a quad");
    assert_eq!(r["lowpoly"]["poly"], json!(poly));
    let poly_verify = r["maps"]["verify"].clone();
    // The meshopt bake at the same triangle count.
    let m = ok(app, ctx, tx, "bake", maps(json!("meshopt"), 2 * quads as u64));
    eprintln!("meshopt bake: {} triangles, verify {}", m["triangles"], m["maps"]["verify"]);
    let (pv, mv) = (poly_verify["mean_error_cells"].as_f64().unwrap(), m["maps"]["verify"]["mean_error_cells"].as_f64().unwrap());
    assert!(pv <= mv, "the retopology's maps verify no worse than meshopt's ({pv} against {mv})");
    // Fresh until the Poly changes: then baked again, with its projection.
    let again = ok(app, ctx, tx, "bake", json!({ "target": clay, "lowpoly": { "poly": poly, "level": 2 } }));
    assert_eq!(again["rebaked"], json!(true));
    let same = ok(app, ctx, tx, "bake", json!({ "target": clay }));
    assert_eq!(same["rebaked"], json!(false), "the saved lowpoly setting, nothing changed");
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 2 }));
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "uv" }));
    let projected = ok(app, ctx, tx, "bake", maps(json!({ "poly": poly, "level": 2 }), 500));
    eprintln!("after project: {}, verify {}", projected["lowpoly"], projected["maps"]["verify"]);
    assert_eq!((projected["rebaked"].clone(), projected["lowpoly"]["projected"].clone()), (json!(true), json!(true)));
    assert!(projected["maps"]["verify"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{}", projected["maps"]);
    // Saved with the scene, the Poly's id kept.
    let path = dir.join("baked.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path }));
    ok(app, ctx, tx, "open_scene", json!({ "path": path }));
    settle(app, ctx);
    assert_eq!(app.scene.get(clay).unwrap().bake.unwrap().lowpoly, Some(crate::bake::LowPoly { poly, level: 2 }));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 3c review fixes, through MCP: a bake from a Poly stays fresh when
/// only the shown level changes (no full re-bake on Page Up/Down); a new
/// projection makes it stale, so it is neither shown nor exported until
/// baked again; and deleting the Poly never fails an export (the bake
/// falls back to meshopt, with a warning).
#[test]
fn lowpoly_bakes_follow_their_poly() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, poly) = bumpy_retopo(app, ctx, tx);
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "uv", "size": 512 }));
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "subdivide", "levels": 1 }));
    let first = ok(app, ctx, tx, "bake", json!({ "target": clay, "texture": 512, "lowpoly": { "poly": poly, "level": 1 } }));
    assert_eq!(first["rebaked"], json!(true));
    // The shown level is not what the bake read.
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "set_level", "level": 0 }));
    settle(app, ctx);
    assert!(app.meshes.baked(clay).is_some(), "still fresh after set_level");
    let same = ok(app, ctx, tx, "bake", json!({ "target": clay }));
    assert_eq!(same["rebaked"], json!(false), "set_level does not force a re-bake");
    // A projection changes what the bake read: stale, not shown.
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 1 }));
    settle(app, ctx);
    assert!(app.meshes.baked(clay).is_none(), "a stale bake is not shown or exported after poly project");
    let again = ok(app, ctx, tx, "bake", json!({ "target": clay }));
    assert_eq!(again["rebaked"], json!(true));
    // The Poly deleted: exports still work, with a warning.
    ok(app, ctx, tx, "delete_object", json!({ "target": poly }));
    ok(app, ctx, tx, "update_object", json!({ "target": clay, "visible": true }));
    settle(app, ctx);
    let dir = std::env::temp_dir().join(format!("clide3d_stage3c_dangling_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let glb = ok(app, ctx, tx, "export_glb", json!({ "path": dir.join("scene.glb") }));
    assert!(glb["warnings"].to_string().contains("lowpoly Poly is gone"), "{glb}");
    let fbx = ok(app, ctx, tx, "export_fbx", json!({ "path": dir.join("scene.fbx") }));
    assert!(fbx["warnings"].to_string().contains("meshopt"), "{fbx}");
    let b = ok(app, ctx, tx, "bake", json!({ "target": clay }));
    assert!(b["lowpoly"]["missing"].is_string(), "{}", b["lowpoly"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The cage, creases and UVs of a Poly with no levels, shown level or
/// provenance: what a file carries.
fn stripped(p: &crate::poly::Poly) -> crate::poly::Poly {
    crate::poly::Poly { levels: Vec::new(), level: 0, made_by: None, projected: None, ..p.clone() }
}

/// Stage 3d's `export_poly` through MCP, as an agent calls it: refusals
/// from the arguments before any work; an OBJ cage with creases, UVs and
/// five maps; `import_mesh as: poly` gives the same cage, creases and UVs
/// back; that re-imported cage subdivided to the maps' level with the
/// vector map from its file added reproduces the Poly's shown level and
/// the clay; object space and a moved, evenly scaled Poly verify too; the
/// same call writes the same bytes.
#[test]
fn export_poly_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (clay, poly) = bumpy_retopo(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_stage3d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name).display().to_string();
    // Without UVs: maps are refused, the geometry alone is not.
    let (e, failed) = mcp(app, ctx, tx, "export_poly", json!({ "target": poly, "path": path("plain.obj") }));
    assert!(failed && e.to_string().contains("poly uv"), "{e}");
    let plain = ok(app, ctx, tx, "export_poly", json!({ "target": poly, "path": path("plain.obj"), "maps": [] }));
    assert_eq!((plain["uv"].clone(), plain["verify"]["cage"]["same_topology"].clone()), (json!(false), json!(true)), "{plain}");
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "uv", "size": 1024 }));
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 2 }));
    // Creases: two infinitely sharp edges and one of 2.5.
    let edges: Vec<[u32; 2]> = app.scene.get(poly).unwrap().poly.as_ref().unwrap().topology().edges.iter().take(3).copied().collect();
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "crease", "edges": [edges[0], edges[1]], "sharpness": 10 }));
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "crease", "edges": [edges[2]], "sharpness": 2.5 }));
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "project", "levels": 2 }));
    let p = app.scene.get(poly).unwrap().poly.clone().unwrap();
    assert_eq!((p.level, p.creases.len()), (2, 3));
    // Refused from the arguments, before any file is written.
    std::fs::write(dir.join("notes.obj"), "my own notes").unwrap();
    std::fs::write(dir.join("taken.json"), "{\"mine\": true}").unwrap();
    for (args, says) in [
        (json!({ "map_size": 8192 }), "map_size"),
        (json!({ "map_level": 3 }), "projected levels"),
        (json!({ "level": 7 }), "level"),
        (json!({ "level": 3, "map_level": 2 }), "below the written level"),
        (json!({ "maps": ["bump"] }), "unknown map"),
        (json!({ "maps": ["normal", "normal"] }), "twice"),
        (json!({ "space": "sideways" }), "space"),
        (json!({ "udim": true, "path": path("cage.glb") }), "glTF"),
        (json!({ "udim": true, "maps": ["color"] }), "maps lists only"),
        (json!({ "path": "relative.obj" }), "absolute"),
        (json!({ "path": path("cage.stl") }), ".obj"),
        (json!({ "path": path("notes.obj") }), "refusing to overwrite"),
        (json!({ "path": path("taken.obj") }), "export_poly sidecar"),
        (json!({ "path": path("cage.glb"), "level": 0, "map_level": 2 }), "map_level must equal level"),
        (json!({ "source": poly }), "not a Clay"),
    ] {
        let mut call = json!({ "target": poly, "path": path("head.obj") });
        for (k, v) in args.as_object().unwrap() {
            call[k] = v.clone();
        }
        let (e, failed) = mcp(app, ctx, tx, "export_poly", call);
        assert!(failed && e.to_string().contains(says), "{args}: {e}");
    }
    assert_eq!(std::fs::read_to_string(dir.join("notes.obj")).unwrap(), "my own notes", "never overwritten");
    assert!(!dir.join("head.obj").exists(), "a refused call writes nothing");
    // The export: the cage, five maps, the sidecar.
    let all = json!(["vector_displacement", "displacement", "normal", "color", "ao"]);
    let call = json!({ "target": poly, "path": path("head.obj"), "maps": all, "map_size": 1024 });
    let r = ok(app, ctx, tx, "export_poly", call.clone());
    eprintln!("export_poly: {} faces, maps {}, verify {}", r["faces"], r["maps"], r["verify"]);
    assert_eq!((r["faces"].as_u64(), r["subdivision"]["creases"].as_u64(), r["maps"]["level"].as_u64(), r["source"].as_u64()), (Some(p.faces() as u64), Some(3), Some(2), Some(clay)));
    for f in ["head.obj", "head.json", "head_vector_displacement.exr", "head_displacement.png", "head_normal.png", "head_color.png", "head_ao.png"] {
        assert!(dir.join(f).metadata().is_ok_and(|m| m.len() > 0), "{f}");
    }
    let v = &r["verify"];
    assert_eq!(v["cage"], json!({ "same_topology": true, "max_point_delta": 0.0, "max_uv_delta": 0.0, "same_creases": true }));
    assert!(v["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{v}");
    assert!(v["maps"]["to_shown_level"]["max_cells"].as_f64().unwrap() <= 0.75, "{v}");
    let side: Value = serde_json::from_slice(&std::fs::read(dir.join("head.json")).unwrap()).unwrap();
    assert_eq!((side["generator"].as_str(), side["maps"]["level"].as_u64(), side["subdivision"]["creases"].as_u64()), (Some("Clide3D export_poly"), Some(2), Some(3)));
    // Re-imported: the same cage, creases and UVs.
    let imported = ok(app, ctx, tx, "import_mesh", json!({ "path": path("head.obj"), "as": "poly", "name": "Reimported" }));
    assert_eq!(imported["warnings"], json!([]), "{imported}");
    let back = app.scene.get(id_of(app, "Reimported")).unwrap().poly.clone().unwrap();
    assert_eq!(back.hash(), stripped(&p).hash(), "the cage, creases and UVs come back the same");
    // The re-imported cage subdivided with the map from its file
    // reproduces the Poly's shown level (and the clay).
    let (subdivided, shown) = reapply_reimported(app, clay, poly, &back, &dir.join("head_vector_displacement.exr"), 1024);
    eprintln!("re-imported cage + vector map: to the shown level {shown:.4} cells, to the clay {subdivided:.4}");
    assert!(shown <= 0.75 && subdivided <= 0.75, "{shown} {subdivided}");
    // The same call writes the same bytes; its own files are replaced.
    let first: Vec<(String, Vec<u8>)> = ["head.obj", "head.json", "head_vector_displacement.exr", "head_displacement.png", "head_normal.png", "head_color.png", "head_ao.png"].iter().map(|f| (f.to_string(), std::fs::read(dir.join(f)).unwrap())).collect();
    ok(app, ctx, tx, "export_poly", call);
    for (f, bytes) in &first {
        assert!(std::fs::read(dir.join(f)).unwrap() == *bytes, "{f} is written the same each time");
    }
    // Object space verifies too.
    let o = ok(app, ctx, tx, "export_poly", json!({ "target": poly, "path": path("object.obj"), "maps": ["vector_displacement"], "map_size": 1024, "space": "object" }));
    eprintln!("object space: {}", o["verify"]["maps"]);
    assert!(o["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{o}");
    // A .glb of the shown level with its textures and the cage inside.
    let g = ok(app, ctx, tx, "export_poly", json!({ "target": poly, "path": path("head.glb"), "maps": ["vector_displacement", "normal", "color", "ao"], "map_size": 1024 }));
    eprintln!("glb: {} {}", g["verify"]["glb"], g["verify"]["maps"]);
    assert_eq!((g["level"].as_u64(), g["maps"]["level"].as_u64()), (Some(2), Some(2)), "a .glb writes the shown level by default");
    assert_eq!((g["verify"]["glb"]["images"].as_u64(), g["verify"]["glb"]["triangles"].as_u64()), (Some(3), Some(p.faces_at(2) * 2)), "{g}");
    assert_eq!(g["verify"]["cage"]["same_topology"], json!(true));
    assert!(g["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{g}");
    let checked = crate::poly::export::check_glb(&std::fs::read(dir.join("head.glb")).unwrap(), &Default::default()).unwrap();
    assert_eq!(checked.cage.unwrap().to_poly().unwrap().hash(), stripped(&p).hash(), "the .glb's cage is the Poly's");
    // USDA: the cage with the height and normal maps on its material.
    let u = ok(app, ctx, tx, "export_poly", json!({ "target": poly, "path": path("head.usda"), "maps": ["displacement", "normal"], "map_size": 1024 }));
    eprintln!("usda: {}", u["verify"]);
    assert_eq!(u["verify"]["cage"], json!({ "same_topology": true, "max_point_delta": 0.0, "max_uv_delta": 0.0, "same_creases": true }));
    assert!(u["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 1.0, "a height map, 16-bit: {u}");
    for (file, name) in [("head.glb", "FromGlb"), ("head.usda", "FromUsda")] {
        ok(app, ctx, tx, "import_mesh", json!({ "path": path(file), "as": "poly", "name": name }));
        assert_eq!(app.scene.get(id_of(app, name)).unwrap().poly.as_ref().unwrap().hash(), stripped(&p).hash(), "{file}: the same cage, creases and UVs");
    }
    let text = std::fs::read_to_string(dir.join("head.usda")).unwrap();
    assert!(text.contains("@head_displacement.png@") && text.contains("@head_normal.png@"), "the material reads the maps beside it");
    // A Poly placed with a turn and an even scale relative to its clay
    // (still on it): the cage re-imported at half size and turned Z up,
    // its object turned back and scaled by 2 where the clay is. Maps are
    // in the Poly's units: half the clay's.
    ok(app, ctx, tx, "import_mesh", json!({ "path": path("head.obj"), "as": "poly", "name": "Half", "scale": 0.5, "up": "z" }));
    let half = id_of(app, "Half");
    ok(app, ctx, tx, "update_object", json!({ "target": half, "translate": [0.3, 0.2, 0], "rotate": [90, 0, 0], "scale": [2, 2, 2] }));
    ok(app, ctx, tx, "poly", json!({ "target": half, "action": "project", "source": clay, "levels": 2 }));
    let moved = ok(app, ctx, tx, "export_poly", json!({ "target": half, "path": path("half.obj"), "maps": ["vector_displacement", "displacement"], "map_size": 1024, "map_format": "png" }));
    eprintln!("turned and scaled: {}", moved["verify"]["maps"]);
    assert!(moved["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{moved}");
    let ratio = moved["maps"]["max_displacement"].as_f64().unwrap() / r["maps"]["max_displacement"].as_f64().unwrap();
    assert!((ratio - 0.5).abs() < 0.05, "maps are in the Poly's units, half the clay's ({ratio})");
    ok(app, ctx, tx, "update_object", json!({ "target": half, "scale": [-2, 2, 2] }));
    let (e, failed) = mcp(app, ctx, tx, "export_poly", json!({ "target": half, "path": path("mirrored.obj"), "map_size": 1024 }));
    assert!(failed && e.to_string().contains("reflects"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The re-imported cage subdivided to level 2, the vector map from its
/// file added as a renderer would: the largest distance (cells) to the
/// clay and to the Poly's shown level.
fn reapply_reimported(app: &ClideApp, clay: u64, poly: u64, back: &crate::poly::Poly, exr: &std::path::Path, size: u32) -> (f32, f32) {
    use glam::Vec3;
    let c = crate::layers::composed_clay(app.scene.get(clay).unwrap().clay.as_ref().unwrap());
    let rest = app.meshes.rest(clay).unwrap().0.clone();
    let to_clay = app.scene.world_matrix(clay).inverse() * app.scene.world_matrix(poly);
    let mut sub = back.clone();
    sub.set_levels(2).unwrap();
    let progress = crate::progress::Progress::default();
    let low = crate::poly::low::Low::new(&sub, 2, to_clay, &rest, size, &progress).unwrap();
    let (w, _, channels) = crate::outputs::read_exr(exr).unwrap();
    let ch = |n: &str| &channels.iter().find(|c| c.0 == n).unwrap().1;
    let (r, g, b) = (ch("R"), ch("G"), ch("B"));
    let disp = crate::bake::Displacement { size: w as u32, height: None, vector: Some((0..r.len()).map(|i| [r[i], g[i], b[i]]).collect()), from_field: 0, missed: 0, object: false };
    let cell = c.cell_size().unwrap();
    let diced = crate::film::dice::apply_baked(&low.mesh, &disp, cell * 0.5, 4_000_000).unwrap();
    let pts: Vec<Vec3> = diced.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let probe = crate::quality::Probe::new(&c).unwrap();
    let to_clay_max = pts.iter().map(|q| probe.near(*q, 4.0 * cell)(*q).abs() / cell).fold(0.0f32, f32::max);
    let p = app.scene.get(poly).unwrap().poly.clone().unwrap();
    let shown = crate::poly::subdiv::evaluate(&p, p.level).unwrap();
    let spts: Vec<Vec3> = shown.points.iter().map(|q| to_clay.transform_point3(*q)).collect();
    let field = crate::mesh_asset::MeshField::new(&spts, &shown.triangles());
    let to_shown = pts.iter().step_by(7).map(|q| field.distance(*q, crate::mesh_asset::Sign::Closed).abs() / cell).fold(0.0f32, f32::max);
    (to_clay_max, to_shown)
}

/// File > Export Subdivision Cage… sends the `export_poly` an agent
/// would; the replies (time aside) and files match.
#[test]
fn ui_export_poly_matches_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, poly) = bumpy_retopo(app, ctx, tx);
    ok(app, ctx, tx, "poly", json!({ "target": poly, "action": "uv", "size": 512 }));
    let dir = std::env::temp_dir().join(format!("clide3d_stage3d_ui_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    app.select_only(poly);
    let sent = app.ui_activity.len();
    app.export_lead_poly(dir.join("ui.obj"));
    settle(app, ctx);
    assert_eq!(app.ui_activity.len(), sent + 1, "one request");
    let a = app.ui_activity.back().unwrap();
    assert_eq!(a.tool, "export_poly");
    let request = a.request.clone();
    assert_eq!(request, json!({ "target": poly, "path": dir.join("ui.obj").display().to_string() }));
    let mut reply = reply_json(app.jobs.result(a.job).unwrap().as_ref().unwrap());
    let ui_bytes = std::fs::read(dir.join("ui.obj")).unwrap();
    let mut agent = ok(app, ctx, tx, "export_poly", request);
    // Under a loaded machine the call can outlast its wait: then the job
    // is waited for (no time asserted).
    while agent["state"] == "running" {
        agent = ok(app, ctx, tx, "job_status", json!({ "id": agent["job"], "wait": 45 }));
        if agent.get("result").is_some() {
            agent = agent["result"].clone();
        }
    }
    for r in [&mut reply, &mut agent] {
        r["seconds"] = Value::Null;
        r["timings"] = Value::Null;
    }
    assert_eq!(agent, reply, "the agent's reply equals the UI's (time aside)");
    assert!(std::fs::read(dir.join("ui.obj")).unwrap() == ui_bytes, "and writes the same file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review (3d, item 6): `export_poly` refuses its paths before any other
/// work (settling a clay, gathering bake inputs): a bad path is named
/// even when the maps would also be refused.
#[test]
fn export_poly_checks_paths_first() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sculpted_poly(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_paths_first_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // No UVs: the maps alone would be refused for that.
    let (v, err) = mcp(app, ctx, tx, "export_poly", json!({ "target": "Ball_poly", "path": dir.join("a@b.usda").display().to_string() }));
    assert!(err && v.to_string().contains('@'), "{v}");
    let (v, err) = mcp(app, ctx, tx, "export_poly", json!({ "target": "Ball_poly", "path": dir.join("ok.usda").display().to_string() }));
    assert!(err && v.to_string().contains("UVs"), "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}
