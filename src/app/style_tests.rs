//! Stage 4d through MCP, on a headless editor (skips without a GPU
//! adapter): stylised material presets with overrides and apply_to, the
//! stylised eye, refusals, one undo step, and save and load.

use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, ok, settle};
use crate::eyeball::{Eye, EyeStyle};
use crate::mcp::ToolCall;
use std::sync::mpsc::Sender;

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

fn eye(app: &ClideApp, name: &str) -> Eye {
    app.scene.get(id_of(app, name)).and_then(|o| o.eye.clone()).expect("an eye")
}

fn refused(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, tool: &str, args: Value, says: &str) {
    let (v, err) = mcp(app, ctx, tx, tool, args.clone());
    assert!(err && v.to_string().contains(says), "{tool} {args}: {v}");
}

/// The presets through `set_material` give the plan's values (with an
/// override on top), go onto objects with apply_to in one undo step, the
/// stylised eye stores its look, and everything survives save and load.
#[test]
fn stylized_looks_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }] }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Cheeks", "resolution": 32, "blobs": [{ "type": "sphere", "center": [0.2, 0, 0], "radius": 0.03 }] }));
    settle(app, ctx);
    let step = app.history.current();
    let r = ok(app, ctx, tx, "set_material", json!({ "name": "toon_skin", "preset": "skin_stylized", "roughness": 0.5, "apply_to": ["Head", "Cheeks"] }));
    eprintln!("set_material preset: {r}");
    let v = &r["values"];
    assert_eq!(r["preset"], "skin_stylized");
    assert_eq!((v["subsurface"].as_f64(), v["subsurface_radius"].as_f64(), v["pores"].as_f64(), v["skin_specular"].as_f64()), (Some(0.6f32 as f64), Some(1.6f32 as f64), Some(0.0), Some(0.4f32 as f64)));
    assert_eq!((v["mottling"].as_f64(), v["sheen"].as_f64()), (Some(0.1f32 as f64), Some(0.15f32 as f64)));
    assert_eq!(v["roughness"].as_f64(), Some(0.5), "the override wins over the preset's 0.45");
    assert_eq!(r["applied_to"], json!(["Head", "Cheeks"]));
    for name in ["Head", "Cheeks"] {
        assert_eq!(app.scene.get(id_of(app, name)).unwrap().material.as_deref(), Some("toon_skin"));
    }
    // One undo step takes both the material and its use back.
    assert_eq!(app.history.current(), step + 1);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.scene.materials.iter().all(|m| m.name != "toon_skin") && app.scene.get(id_of(app, "Head")).unwrap().material.is_none());
    ok(app, ctx, tx, "redo", json!({}));
    // preset restarts from the preset even over an existing material.
    let again = ok(app, ctx, tx, "set_material", json!({ "name": "toon_skin", "preset": "skin_stylized" }));
    assert_eq!(again["values"]["roughness"].as_f64(), Some(0.45f32 as f64));
    // Every preset by its own name, and subsurface_radius as a field.
    for p in ["lips_stylized", "cheek_blush", "fabric_soft"] {
        ok(app, ctx, tx, "set_material", json!({ "name": p, "preset": p }));
    }
    ok(app, ctx, tx, "set_material", json!({ "name": "wax", "subsurface": 0.5, "subsurface_radius": 2.5 }));
    // The stylised eye: its look filled in, the call's own values kept.
    ok(app, ctx, tx, "create_object", json!({ "shape": "Eye", "name": "EyeL", "eye": { "style": "stylized", "iris_scale": 1.4, "pupil": 0.3 } }));
    let e = eye(app, "EyeL");
    assert_eq!((e.style, e.iris_scale, e.pupil_size, e.cornea, e.veins, e.limbal_ring, e.catchlight), (EyeStyle::Stylized, 1.4, 0.3, 0.03, 0.0, 0.8, true));
    // A later change keeps the rest; switching back keeps the values.
    ok(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "catchlight": false, "style": "stylized" } }));
    assert_eq!((eye(app, "EyeL").catchlight, eye(app, "EyeL").iris_scale), (false, 1.4));
    ok(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "catchlight": true } }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Eye", "name": "EyeR", "eye": { "style": "stylized", "catchlight": false, "cornea": 0.04 } }));
    let r = eye(app, "EyeR");
    assert_eq!((r.catchlight, r.cornea, r.iris_scale), (false, 0.04, 1.3));
    // Refused before anything changes.
    let before = app.scene.clone();
    refused(app, ctx, tx, "set_material", json!({ "name": "x", "preset": "nope" }), "unknown preset");
    refused(app, ctx, tx, "set_material", json!({ "name": "x", "preset": "skin_stylized", "based_on": "skin" }), "not both");
    refused(app, ctx, tx, "set_material", json!({ "name": "x", "preset": 3 }), "preset must be");
    refused(app, ctx, tx, "set_material", json!({ "name": "x", "preset": "skin_stylized", "apply_to": vec!["Head"; 65] }), "at most 64");
    refused(app, ctx, tx, "set_material", json!({ "name": "x", "preset": "skin_stylized", "apply_to": "Nobody" }), "Nobody");
    refused(app, ctx, tx, "set_material", json!({ "name": "toon_skin", "remove": true, "apply_to": "Head" }), "apply_to needs");
    refused(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "iris_scale": 2.0 } }), "iris_scale must be");
    refused(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "pupil": 1.5 } }), "pupil must be");
    refused(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "pupil": 0.3, "pupil_size": 0.3 } }), "not both");
    refused(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "style": "anime" } }), "realistic or stylized");
    refused(app, ctx, tx, "update_object", json!({ "target": "EyeL", "eye": { "sparkle": 1 } }), "unknown eye setting sparkle");
    assert!(app.scene.materials == before.materials && eye(app, "EyeL") == before.get(id_of(app, "EyeL")).unwrap().eye.clone().unwrap());
    // Save and load.
    let dir = std::env::temp_dir().join(format!("clide3d_stage4d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stylized.json");
    let (materials, eyes) = (app.scene.materials.clone(), (eye(app, "EyeL"), eye(app, "EyeR")));
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"subsurface_radius\"") && text.contains("\"stylized\"") && text.contains("\"catchlight\""));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    assert!(app.scene.materials == materials, "materials round-trip");
    assert!((eye(app, "EyeL"), eye(app, "EyeR")) == eyes, "eyes round-trip");
    assert_eq!(app.scene.get(id_of(app, "Head")).unwrap().material.as_deref(), Some("toon_skin"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review fixes: `apply_to` is refused, not silently dropped, where the
/// call does not set a named material (a listing, delete_unused, rename,
/// replace), and the eye's `pupil` takes pupil_size's range (0.05-0.95)
/// rather than being clamped quietly from 0-1.
#[test]
fn apply_to_and_pupil_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Sphere", "name": "Ball" }));
    ok(app, ctx, tx, "set_material", json!({ "name": "toon", "preset": "skin_stylized" }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Eye", "name": "Eye" }));
    let before = serde_json::to_value(&app.scene).unwrap();
    for args in [
        json!({ "apply_to": "Ball" }),
        json!({ "list": true, "apply_to": "Ball" }),
        json!({ "delete_unused": true, "apply_to": "Ball" }),
        json!({ "name": "toon", "rename": "toon2", "apply_to": "Ball" }),
        json!({ "replace": "toon", "with": "skin", "apply_to": "Ball" }),
    ] {
        refused(app, ctx, tx, "set_material", args, "apply_to needs a material name");
    }
    for pupil in [0.0, 0.02, 0.97, 1.0] {
        refused(app, ctx, tx, "update_object", json!({ "target": "Eye", "eye": { "pupil": pupil } }), "pupil must be 0.05 to 0.95");
    }
    assert_eq!(serde_json::to_value(&app.scene).unwrap(), before, "nothing changed");
    ok(app, ctx, tx, "update_object", json!({ "target": "Eye", "eye": { "pupil": 0.05 } }));
    assert_eq!(eye(app, "Eye").pupil_size, 0.05);
    ok(app, ctx, tx, "update_object", json!({ "target": "Eye", "eye": { "pupil": 0.95 } }));
    assert_eq!(eye(app, "Eye").pupil_size, 0.95);
}
