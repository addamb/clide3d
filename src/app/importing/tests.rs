//! Stage 5 through MCP, on a headless editor (skips without a GPU
//! adapter): import_mesh in every format, to_clay in place and across
//! objects, export_print read back, and the Remesh as Clay… menu sending
//! the same call an agent would.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use egui::{Event, Key, Modifiers};
use glam::Vec3;
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::mcp::ToolCall;
use crate::mesh_asset::tests::icosphere;
use crate::mesh_import::write;
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

/// A fresh folder for this test's files.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clide3d_stage5_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn object<'a>(app: &'a ClideApp, name: &str) -> &'a crate::scene::Object {
    app.scene.objects.iter().find(|o| o.name == name).unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn import_convert_and_print_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let dir = folder("io");
    let (p, t) = icosphere(3, 0.5);
    for (ext, bytes) in [("obj", write::obj(&p, &t)), ("stl", write::stl(&p, &t)), ("ply", write::ply(&p, &t))] {
        let path = dir.join(format!("ico.{ext}"));
        std::fs::write(&path, bytes).unwrap();
        let r = ok(app, ctx, tx, "import_mesh", json!({ "path": path, "as": "mesh", "name": format!("ico_{ext}") }));
        assert_eq!(r["triangles"], t.len(), "{ext}: {r}");
        assert_eq!(r["watertight"], true);
        assert_eq!(object(app, &format!("ico_{ext}")).shape, Shape::Mesh);
    }
    // A mesh object to clay in place at res 128.
    let r = ok(app, ctx, tx, "to_clay", json!({ "target": "ico_obj", "resolution": 128 }));
    let cells = r["hausdorff_cells"].as_f64().unwrap();
    eprintln!("to_clay of the imported icosphere: hausdorff_est {cells:.3} cell");
    assert!(cells <= 0.75, "{r}");
    assert_eq!(object(app, "ico_obj").shape, Shape::Clay);
    assert!(object(app, "ico_obj").mesh.is_none());
    // Import as clay straight away.
    let r = ok(app, ctx, tx, "import_mesh", json!({ "path": dir.join("ico.stl"), "name": "ball" }));
    assert_eq!(r["as"], "clay");
    let ball = object(app, "ball").clay.clone().unwrap();
    assert_eq!(ball.assets.len(), 1);
    // Our own GLB and FBX exports read back with the clay's triangles.
    settle(app, ctx);
    let drawn = app.meshes.get(object(app, "ball")).unwrap().indices.len() / 3;
    for (tool, ext) in [("export_glb", "glb"), ("export_fbx", "fbx")] {
        let path = dir.join(format!("ball.{ext}"));
        ok(app, ctx, tx, tool, json!({ "path": path, "targets": ["ball"], "animations": false }));
        let i = ok(app, ctx, tx, "import_mesh", json!({ "path": path, "as": "mesh", "name": format!("ball_{ext}") }));
        assert_eq!(i["triangles"], drawn, "{ext}: {i}");
        let b = &i["bounds"];
        assert!((b["max"][1].as_f64().unwrap() - 0.5).abs() < 0.01, "{ext}: in meters at its place: {b}");
    }
    // Print: STL scaled to 50 mm along the scene's y (Z in the file).
    let stl = dir.join("print.stl");
    let r = ok(app, ctx, tx, "export_print", json!({ "path": stl, "targets": ["ball"], "size_mm": { "axis": "y", "value": 50 } }));
    assert_eq!(r["watertight"], true, "{r}");
    let back = crate::mesh_import::read(&stl).unwrap();
    assert_eq!(back.tris.len() as u64, r["triangles"].as_u64().unwrap());
    let (edges, bad, flipped) = crate::mesh_asset::edge_use(&back.tris);
    assert!(edges > 0 && bad == 0 && flipped == 0, "every edge used exactly twice: {bad} not, {flipped} flipped");
    let (lo, hi) = back.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    eprintln!("print STL: {} triangles, height {:.6} mm, volume {} cm3", back.tris.len(), hi.z - lo.z, r["volume_cm3"]);
    assert!((hi.z - lo.z - 50.0).abs() <= 1e-4, "{}", hi.z - lo.z);
    assert!(lo.z.abs() <= 1e-6, "on the plate");
    // OBJ and PLY come back with the exact triangle count; 3MF is written.
    for ext in ["obj", "ply", "3mf"] {
        let path = dir.join(format!("print.{ext}"));
        let r = ok(app, ctx, tx, "export_print", json!({ "path": path, "targets": ["ball"] }));
        if ext == "3mf" {
            assert!(std::fs::read(&path).unwrap().starts_with(b"PK"));
            continue;
        }
        let i = ok(app, ctx, tx, "import_mesh", json!({ "path": path, "as": "mesh" }));
        assert_eq!(i["triangles"], r["triangles"], "{ext}");
    }
    // Hollowed: less material, still closed.
    let solid = ok(app, ctx, tx, "export_print", json!({ "path": dir.join("solid.stl"), "targets": ["ball"] }))["volume_cm3"].as_f64().unwrap();
    let h = ok(app, ctx, tx, "export_print", json!({ "path": dir.join("hollow.stl"), "targets": ["ball"], "hollow": { "wall_mm": 40 } }));
    eprintln!("hollow 40 mm wall: {} cm3 of {solid} cm3, watertight {}", h["volume_cm3"], h["watertight"]);
    assert!(h["volume_cm3"].as_f64().unwrap() < 0.9 * solid && h["watertight"] == true, "{h}");
    // Bad calls change nothing.
    let before = app.scene.clone();
    for (tool, args) in [("import_mesh", json!({ "path": dir.join("missing.obj") })), ("import_mesh", json!({ "path": "relative.obj" })), ("to_clay", json!({ "target": "ball", "op": "melt" })), ("export_print", json!({ "path": dir.join("x.gltf") })), ("export_print", json!({ "path": dir.join("y.stl"), "targets": ["ico_stl"], "hollow": { "wall_mm": 1 } }))] {
        let (v, err) = mcp(app, ctx, tx, tool, args);
        assert!(err, "{tool}: {v}");
    }
    assert!(app.scene == before);
    // The wall is in millimeters however the clay is scaled: at twice the
    // size (and printed twice as large) a wall twice as thick hollows the
    // same share of it.
    let share = h["volume_cm3"].as_f64().unwrap() / solid;
    ok(app, ctx, tx, "update_object", json!({ "target": "ball", "scale": [2, 2, 2] }));
    let solid2 = ok(app, ctx, tx, "export_print", json!({ "path": dir.join("solid2.stl"), "targets": ["ball"] }))["volume_cm3"].as_f64().unwrap();
    let h2 = ok(app, ctx, tx, "export_print", json!({ "path": dir.join("hollow2.stl"), "targets": ["ball"], "hollow": { "wall_mm": 80 } }));
    let share2 = h2["volume_cm3"].as_f64().unwrap() / solid2;
    eprintln!("hollow share at scale 1 (40 mm): {share:.4}, at scale 2 (80 mm): {share2:.4}");
    assert!((share2 / share - 1.0).abs() < 0.03, "{share} {share2}");
    // Unevenly scaled clay would have a varying wall: refused.
    ok(app, ctx, tx, "update_object", json!({ "target": "ball", "scale": [1, 2, 1] }));
    let (v, err) = mcp(app, ctx, tx, "export_print", json!({ "path": dir.join("uneven.stl"), "targets": ["ball"], "hollow": { "wall_mm": 40 } }));
    assert!(err, "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn booleans_across_objects_and_insert_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Cube", "name": "Block" }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Sphere", "name": "Ball" }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Ball", "translate": [0.3, 0.8, 0], "scale": [1.1, 1.1, 1.1] }));
    let r = ok(app, ctx, tx, "to_clay", json!({ "targets": ["Block", "Ball"], "op": "intersect", "resolution": 96 }));
    assert_eq!(r["hidden"], json!(["Ball"]), "{r}");
    let block = object(app, "Block");
    assert_eq!(block.shape, Shape::Clay);
    let clay = block.clay.clone().unwrap();
    assert_eq!(clay.blobs.len(), 2);
    assert!(matches!(clay.blobs[0].form, crate::clay::Form::Box { .. }) && clay.blobs[1].op == crate::clay::Op::Intersect);
    assert!(!object(app, "Ball").visible);
    settle(app, ctx);
    let mesh = app.meshes.get(object(app, "Block")).unwrap().clone();
    let v = crate::harness::geom::volume(&mesh);
    assert!(v > 0.05 && v < 1.0, "the overlap: {v}");
    // A cylinder carved into the result: a hole.
    ok(app, ctx, tx, "create_object", json!({ "shape": "Cylinder", "name": "Drill" }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Drill", "translate": [3, 0, 0], "scale": [0.2, 0.6, 0.2] }));
    let r = ok(app, ctx, tx, "kitbash", json!({ "target": "Block", "part": { "object": "Drill", "op": "carve" }, "at": [0.1, 1.2, 0] }));
    assert_eq!(r["inserted"], 1, "{r}");
    // The insert job is one undo step.
    assert_eq!(object(app, "Block").clay.as_ref().unwrap().blobs.len(), 3);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(object(app, "Block").clay.as_ref().unwrap().blobs.len(), 2, "undone");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(object(app, "Block").clay.as_ref().unwrap().blobs.len(), 3, "redone");
    // into: everything else goes into an existing clay.
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Base", "resolution": 48, "blobs": [{ "type": "box", "center": [0, 0, 0], "size": [2, 0.2, 2], "blend": 0 }] }));
    let r = ok(app, ctx, tx, "to_clay", json!({ "target": "Drill", "into": "Base", "op": "carve" }));
    assert_eq!(r["blobs"], 2, "{r}");
    assert!(!object(app, "Drill").visible);
}

/// A shell of an array of a mesh through MCP: `create_object` and
/// `edit_clay` take the nested blob by its asset's hash (from a mesh this
/// session imported), adopt the asset into the clay, and draw a hollow
/// ball at every copy.
#[test]
fn nested_mesh_blobs_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let dir = folder("nested");
    let (p, t) = icosphere(3, 0.3);
    let path = dir.join("ball.obj");
    std::fs::write(&path, write::obj(&p, &t)).unwrap();
    ok(app, ctx, tx, "import_mesh", json!({ "path": path, "as": "mesh", "name": "source" }));
    let hash = object(app, "source").mesh.as_ref().unwrap().hash().to_string();
    let nested = |step: [f32; 3]| json!({ "type": "shell", "thickness": 0.05, "blend": 0, "source": { "type": "array", "count": 3, "step": step, "source": { "type": "mesh", "asset": hash, "sign": "winding" } } });
    let step = Vec3::new(0.8, 0.0, 0.0);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Row", "resolution": 64, "blobs": [nested(step.to_array())] }));
    let clay = object(app, "Row").clay.clone().unwrap();
    assert_eq!(clay.assets.keys().collect::<Vec<_>>(), vec![&hash], "create_object adopts the nested asset");
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Plain", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, -1, 0], "radius": 0.2 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Plain", "add": [nested([0.0, 0.8, 0.0])] }));
    let plain = object(app, "Plain").clay.clone().unwrap();
    assert_eq!((plain.blobs.len(), plain.assets.keys().collect::<Vec<_>>()), (2, vec![&hash]), "edit_clay adopts the nested asset");
    settle(app, ctx);
    let mesh = app.meshes.get(object(app, "Row")).unwrap().clone();
    for i in 0..3 {
        let c = step * i as f32;
        let near = mesh.vertices.iter().filter(|v| Vec3::from(v.pos).distance(c) < 0.32).count();
        assert!(near > 100, "copy {i}: {near} vertices");
    }
    assert!(mesh.vertices.iter().any(|v| Vec3::from(v.pos).length() < 0.27), "hollow: an inner wall");
    let drawn = app.meshes.get(object(app, "Plain")).unwrap();
    assert!(drawn.vertices.iter().any(|v| Vec3::from(v.pos).distance(Vec3::new(0.0, 1.6, 0.0)) < 0.32), "edit_clay's copies drawn");
    // Removing the nested blob takes its asset out of the clay.
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Plain", "remove": [1] }));
    assert!(object(app, "Plain").clay.as_ref().unwrap().assets.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Modify > Remesh as Clay… sends the to_clay call an agent would: the
/// stored request, sent again through MCP after an undo, gives the same
/// reply and the same object.
#[test]
fn the_remesh_menu_sends_the_agents_call() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Lump", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.4 }, { "type": "box", "center": [0.3, 0, 0], "size": [0.4, 0.3, 0.3], "blend": 0.05 }] }));
    settle(app, ctx);
    let id = object(app, "Lump").id;
    app.selection = vec![id];
    app.open_remesh_dialog();
    app.remesh_dialog.as_mut().unwrap().resolution = 64;
    app.remesh_dialog.as_mut().unwrap().blur = 1.0;
    frame(app, ctx, Vec::new());
    frame(app, ctx, vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::default() }]);
    assert!(app.remesh_dialog.is_none(), "Enter sent it");
    settle(app, ctx);
    let activity = app.ui_activity.back().expect("the menu's call");
    assert_eq!(activity.tool, "to_clay");
    let (request, job) = (activity.request.clone(), activity.job);
    assert_eq!(request, json!({ "target": id, "mode": "volume", "resolution": 64, "blur": 1.0 }));
    let reply: Value = match app.jobs.result(job).expect("finished") {
        Ok(content) => serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap(),
        Err(e) => panic!("{e}"),
    };
    let hash = |app: &ClideApp| xxhash_rust::xxh3::xxh3_64(serde_json::to_string(object(app, "Lump")).unwrap().as_bytes());
    let menu_hash = hash(app);
    assert_eq!(object(app, "Lump").clay.as_ref().unwrap().blobs.len(), 1, "one remeshed blob");
    let scene = ok(app, ctx, tx, "get_scene", json!({}));
    assert!(scene["ui_activity"].to_string().contains("to_clay"), "{}", scene["ui_activity"]);
    // The agent sends the same request on the scene from before.
    ok(app, ctx, tx, "undo", json!({}));
    let again = ok(app, ctx, tx, "to_clay", request);
    assert_eq!(again, reply);
    assert_eq!(hash(app), menu_hash, "the same object");
}
