//! Stage 4 through MCP, on a headless editor (skips without a GPU
//! adapter): sculpt layers as their own ledgers, amounts marking records
//! stale until rebase, select as editor state (no undo step), the
//! recording layer, morphs, and blend shapes in an exported GLB.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::clay::Clay;
use crate::layers;
use crate::mcp::ToolCall;

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

fn clay(app: &ClideApp, name: &str) -> Clay {
    app.scene.objects.iter().find(|o| o.name == name).and_then(|o| o.clay.clone()).expect("the clay")
}

fn records(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, layer: &str) -> Vec<Value> {
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "layer": layer, "verbose": true }));
    log["records"].as_array().unwrap_or_else(|| panic!("{log}")).clone()
}

#[test]
fn layers_and_morphs_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.01 }));
    settle(app, ctx);
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0.15, 0.48, 0.0], "radius": 0.15, "strength": 0.3 }] }));
    let added = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "detail" }));
    let names: Vec<&str> = added["layers"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["base", "paint", "detail"], "{added}");
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "layer": "detail", "strokes": [{ "brush": "standard", "point": [0.0, 0.5, 0.0], "radius": 0.12, "strength": 0.3 }] }));
    assert_eq!(r["layer"], "detail", "{r}");
    settle(app, ctx);
    let detail = records(app, ctx, tx, "detail");
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0]["layer"], "detail");
    assert_eq!(records(app, ctx, tx, "base").len(), 1, "the base ledger keeps its own records");
    let c = clay(app, "Ball");
    assert!(layers::layer(&c, "detail").unwrap().detail.is_some() && layers::composed_detail(&c) != c.detail, "the clay shows base and the layer");
    // A mask action and a paint stroke on the layered clay change its
    // ledgers but not its shape: the composite keeps its identity and no
    // mesh is built.
    let builds = app.meshes.geometry_builds;
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_normal": { "dir": [0, 1, 0], "angle": 30 } }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "paint", json!({ "target": "Ball", "strokes": [{ "points": [[0.0, 0.5, 0.0]], "radius": 0.05, "color": [1, 0, 0] }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": ["clear"] }));
    settle(app, ctx);
    assert_eq!(app.meshes.geometry_builds, builds, "mask and paint edits rebuild no mesh on a layered clay");
    let get = ok(app, ctx, tx, "get_clay", json!({ "target": "Ball" }));
    let listed: Vec<&str> = get["layers"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(listed, ["base", "paint", "detail", "mask", "groups"], "{get}");
    // select is the editor's state: no scene change, no undo step.
    let (step, scene) = (app.history.current(), app.scene.clone());
    let sel = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "select", "name": "detail" }));
    settle(app, ctx);
    assert_eq!(sel["selected"], "detail");
    assert_eq!(app.history.current(), step, "select adds no undo step");
    assert!(app.scene == scene);
    // Another layer's amount marks records stale; rebase clears them.
    let amount = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "amount", "name": "detail", "amount": 0.5 }));
    assert!(amount["composite_ms"].is_number(), "{amount}");
    assert_eq!(app.history.current(), step + 1, "an amount change is an undo step");
    settle(app, ctx);
    assert!(records(app, ctx, tx, "base")[0]["stale_context"].as_bool().unwrap(), "base read the layer at 1");
    assert!(records(app, ctx, tx, "detail")[0]["stale_context"].as_bool().unwrap(), "the layer's strokes are stored at its amount");
    let rebased = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "layer": "detail", "action": "rebase" }));
    assert_eq!(rebased["layer"], "detail", "{rebased}");
    settle(app, ctx);
    assert!(!records(app, ctx, tx, "detail")[0]["stale_context"].as_bool().unwrap());
    // The recording layer is what calls without layer write.
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "record", "name": "detail" }));
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [-0.15, 0.47, 0.0], "radius": 0.1, "strength": 0.3 }] }));
    assert_eq!(r["layer"], "detail", "{r}");
    settle(app, ctx);
    assert_eq!(records(app, ctx, tx, "detail").len(), 2);
    // Undo takes an amount change back.
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "amount", "name": "detail", "amount": 0.25 }));
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(layers::layer(&clay(app, "Ball"), "detail").unwrap().amount, 0.5);
    // Morphs: store, sculpt on, switch and back (the very grids).
    ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "store", "name": "a" }));
    let stored = clay(app, "Ball");
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "layer": "base", "strokes": [{ "brush": "inflate", "point": [0.0, 0.0, 0.5], "radius": 0.15, "strength": 0.4 }] }));
    settle(app, ctx);
    let sculpted = clay(app, "Ball");
    let sw = ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "switch", "name": "a" }));
    assert!(clay(app, "Ball").detail == stored.detail, "{sw}");
    ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "switch", "name": "a" }));
    assert!(clay(app, "Ball").detail == sculpted.detail);
    let list = ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "list" }));
    assert!(list["morphs"][0]["max"].as_f64().unwrap() > 0.0, "{list}");
    // A pull three radii long as a morph too.
    ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "store", "name": "now" }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "layer": "base", "strokes": [{ "brush": "stretch", "point": [0.0, 0.5, 0.0], "radius": 0.08, "delta": [0.0, 0.24, 0.0] }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "store", "name": "horn" }));
    ok(app, ctx, tx, "morph", json!({ "target": "Ball", "action": "switch", "name": "now" }));
    settle(app, ctx);
    // Blend shapes in the GLB: the morph back to before the switch, and
    // the horn, refused with its residual.
    let path = std::env::temp_dir().join(format!("clide_stage4_{}.glb", std::process::id()));
    let out = ok(app, ctx, tx, "export_glb", json!({ "path": path, "blend_shapes": ["morph:a", "morph:horn"] }));
    let shapes = out["blend_shapes"].as_array().unwrap_or_else(|| panic!("{out}")).clone();
    let a = shapes.iter().find(|s| s["name"] == "morph:a").unwrap();
    let horn = shapes.iter().find(|s| s["name"] == "morph:horn").unwrap();
    assert!(a["refused"].is_null() && a["residual"].as_f64().unwrap() <= a["max_residual"].as_f64().unwrap(), "{a}");
    assert!(horn["refused"].is_string() && horn["residual"].as_f64().unwrap() > horn["max_residual"].as_f64().unwrap(), "{horn}");
    let rest = crate::glb_read::read(&path, None, None, &Default::default()).unwrap();
    let on = crate::glb_read::read(&path, None, None, &[("morph:a".to_string(), 1.0)].into_iter().collect()).unwrap();
    assert_eq!(rest.summary["vertices"], on.summary["vertices"]);
    assert_eq!(on.summary["meshes"][0]["morph_targets"], json!(["morph:a"]), "{}", on.summary);
    assert!(rest.items[0].mesh.vertices.iter().zip(&on.items[0].mesh.vertices).any(|(x, y)| x.pos != y.pos));
    let _ = std::fs::remove_file(&path);
    // An unknown shape is refused before anything is written.
    let (e, err) = mcp(app, ctx, tx, "export_glb", json!({ "path": path, "blend_shapes": ["morph:nope"] }));
    assert!(err && e.to_string().contains("nope"), "{e}");
    // A split-off layer cannot take a reserved name (a file with one
    // would not load its layers).
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_normal": { "dir": [0, 1, 0], "angle": 30 } }] }));
    settle(app, ctx);
    let (e, err) = mcp(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "split_by_mask", "to": "archived part" }));
    assert!(err && e.to_string().contains("reserved"), "{e}");
    let made = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "split_by_mask", "to": "part" }));
    assert_eq!(made["made"], "part", "{made}");
}

#[test]
fn a_layer_selection_goes_when_undo_or_a_new_scene_takes_its_layer() {
    // Without a GPU: the pruning itself.
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    let mut c: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] })).unwrap();
    crate::ledger::stack_mut(&mut c).layers.push(layers::Layer::new("A", layers::Kind::Sculpt, 1.0));
    scene.get_mut(id).unwrap().clay = Some(c);
    let mut sel: std::collections::HashMap<u64, String> = [(id, "B".to_string()), (id + 100, "A".to_string())].into_iter().collect();
    super::super::prune_layer_selection(&mut sel, &scene);
    assert!(sel.is_empty(), "a name the clay lacks and a gone object are dropped");
    sel.insert(id, "A".into());
    super::super::prune_layer_selection(&mut sel, &scene);
    assert_eq!(sel.get(&id).map(String::as_str), Some("A"));
    // Through MCP: rename A to B, then undo. The selection named B, which
    // the clay no longer has: it goes, so strokes cannot go elsewhere.
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "A" }));
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "select", "name": "A" }));
    let r = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "rename", "name": "A", "to": "B" }));
    assert_eq!(r["selected"], "B", "{r}");
    ok(app, ctx, tx, "undo", json!({}));
    let l = ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "list" }));
    assert!(l["selected"].is_null(), "{l}");
    assert!(app.layer_selection.is_empty());
    // A refused rename leaves the selection as it was.
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "select", "name": "A" }));
    let (e, err) = mcp(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "rename", "name": "A", "to": "mask" }));
    assert!(err, "{e}");
    assert_eq!(app.layer_selection.values().next().map(String::as_str), Some("A"));
    // A new scene: nothing stays selected.
    ok(app, ctx, tx, "new_scene", json!({}));
    assert!(app.layer_selection.is_empty());
}
