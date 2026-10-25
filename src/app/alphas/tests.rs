//! Stage 2 through MCP, on a headless editor (skips without a GPU
//! adapter): alpha import and rebinding, brush presets saved and used, and
//! posable symmetry.

use std::sync::mpsc::Sender;

use glam::Vec3;
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::brush::alpha::Alpha;
use crate::clay::Detail;
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

fn checker(n: u32, cell: u32) -> Alpha {
    Alpha::new(n, n, (0..n * n).map(|i| if (i % n / cell + i / n / cell) % 2 == 0 { 65535 } else { 0 }).collect()).unwrap()
}

/// A scratch file for this test run.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("clide3d-stage2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn png(name: &str, a: &Alpha) -> String {
    let path = scratch(name);
    std::fs::write(&path, a.png()).unwrap();
    path.to_string_lossy().into_owned()
}

/// A slab clay whose top is y = 0, with a 4 mm detail cell.
fn slab(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": name, "resolution": 64, "blobs": [{ "type": "box", "center": [0, -0.15, 0], "size": [0.6, 0.3, 0.6], "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": name, "detail_cell": 0.004 }));
    settle(app, ctx);
}

fn stale(log: &Value) -> Vec<bool> {
    log["records"].as_array().unwrap().iter().map(|r| r["stale_context"].as_bool().unwrap()).collect()
}

#[test]
fn alpha_import_binds_by_hash_and_rebinding_makes_records_stale() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    slab(app, ctx, tx, "Slab");
    let a = checker(64, 16);
    let first = ok(app, ctx, tx, "alpha", json!({ "action": "import", "name": "checker", "path": png("a.png", &a) }));
    assert_eq!(first["hash"], json!(a.hash()));
    assert_eq!((first["w"].clone(), first["bits"].clone()), (json!(64), json!(16)));
    let again = ok(app, ctx, tx, "alpha", json!({ "action": "import", "name": "checker", "path": png("a.png", &a) }));
    assert_eq!((again["hash"].clone(), again["replaced"].clone()), (first["hash"].clone(), json!(false)), "the same PNG, the same hash");
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Slab", "strokes": [
        { "brush": "standard", "point": [0, 0, 0], "radius": 0.08, "falloff": "flat", "alpha": { "name": "checker", "depth": 0.006 } },
        { "brush": "inflate", "point": [0.2, 0, 0.2], "radius": 0.05 },
    ] }));
    assert!(r["strokes"][0]["changed_points"].as_u64().unwrap() > 100, "{r}");
    settle(app, ctx);
    assert_eq!(stale(&ok(app, ctx, tx, "stroke_log", json!({ "target": "Slab" }))), [false, false]);
    // Other pixels under the same name: the alpha stroke goes stale.
    let b = checker(64, 8);
    let rebound = ok(app, ctx, tx, "alpha", json!({ "action": "import", "name": "checker", "path": png("b.png", &b) }));
    assert_eq!((rebound["replaced"].clone(), rebound["stale_records"].clone()), (json!(true), json!(1)), "{rebound}");
    assert_eq!(stale(&ok(app, ctx, tx, "stroke_log", json!({ "target": "Slab" }))), [true, false]);
    let full = ok(app, ctx, tx, "stroke_log", json!({ "target": "Slab", "full": true }));
    assert_eq!(full["records"][0]["resolved"]["params"]["alpha"]["hash"], json!(a.hash()), "the record keeps the image it was made with");
    ok(app, ctx, tx, "stroke_edit", json!({ "target": "Slab", "action": "rebase" }));
    settle(app, ctx);
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Slab", "full": true }));
    assert_eq!(stale(&log), [false, false]);
    assert_eq!(log["records"][0]["resolved"]["params"]["alpha"]["hash"], json!(b.hash()), "rebase takes the new image");
    // List, preview, capture, delete.
    let list = ok(app, ctx, tx, "alpha", json!({ "action": "list" }));
    assert_eq!(list["alphas"][0]["used_by"], json!(["Slab"]));
    let (shot, err) = mcp(app, ctx, tx, "alpha", json!({ "action": "preview", "name": "checker", "size": 32 }));
    assert!(!err && shot["w"] == json!(64), "{shot}");
    let view = ok(app, ctx, tx, "alpha", json!({ "action": "from_view", "name": "top", "target": "Slab", "view": "top", "size": 64 }));
    assert_eq!(view["w"], json!(64));
    assert!(mcp(app, ctx, tx, "alpha", json!({ "action": "create_vdm", "name": "x" })).1);
    assert!(mcp(app, ctx, tx, "alpha", json!({ "action": "import", "name": "x", "path": "/nonexistent/x.png" })).1);
    let gone = ok(app, ctx, tx, "alpha", json!({ "action": "delete", "name": "checker" }));
    assert_eq!(gone["kept_by"], json!(["Slab"]));
    // The clay still holds it: strokes still replay, and the name still
    // reaches the image the clay binds.
    ok(app, ctx, tx, "sculpt", json!({ "target": "Slab", "strokes": [{ "brush": "clay", "point": [-0.15, 0, -0.15], "radius": 0.05, "alpha": { "name": "checker" } }] }));
    settle(app, ctx);
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Slab", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
}

#[test]
fn presets_save_load_and_merge_under_strokes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    slab(app, ctx, tx, "A");
    slab(app, ctx, tx, "B");
    let spec = json!({ "brush": "clay_tubes", "strength": 0.4, "falloff": "sharp" });
    let saved = ok(app, ctx, tx, "brush_preset", json!({ "action": "save", "name": "MyTubes", "spec": spec }));
    assert!(saved["step"].as_u64().is_some(), "{saved}");
    assert!(mcp(app, ctx, tx, "brush_preset", json!({ "action": "save", "name": "Clay", "spec": spec })).1, "built-in names are taken");
    assert!(mcp(app, ctx, tx, "brush_preset", json!({ "action": "save", "name": "Bad", "spec": { "brush": "clay", "points": [[0, 0, 0]] } })).1);
    let list = ok(app, ctx, tx, "brush_preset", json!({ "action": "list" }));
    let names: Vec<&str> = list["brushes"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
    assert!(names.len() == 22 && names.contains(&"MyTubes") && names.contains(&"MaskPen") && names.contains(&"hPolish"), "{names:?}");
    assert_eq!(ok(app, ctx, tx, "brush_preset", json!({ "action": "get", "name": "MyTubes" }))["spec"], spec);
    // Preset + overrides equals the stroke spelled out.
    let path = json!([[-0.1, 0, 0], [0, 0, 0.03], [0.1, 0, 0]]);
    ok(app, ctx, tx, "sculpt", json!({ "target": "A", "preset": "MyTubes", "strokes": [{ "points": path, "radius": 0.03, "strength": 0.2 }] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "B", "strokes": [{ "brush": "clay_tubes", "falloff": "sharp", "points": path, "radius": 0.03, "strength": 0.2 }] }));
    settle(app, ctx);
    let (la, lb) = (ok(app, ctx, tx, "stroke_log", json!({ "target": "A", "full": true })), ok(app, ctx, tx, "stroke_log", json!({ "target": "B", "full": true })));
    assert_eq!(la["records"][0]["result_hash"], lb["records"][0]["result_hash"]);
    assert_eq!(la["records"][0]["spec"], lb["records"][0]["spec"]);
    // From a record; saved with the scene and read back the same.
    let rid = la["records"][0]["id"].clone();
    let from = ok(app, ctx, tx, "brush_preset", json!({ "action": "save", "name": "FromA", "from_record": { "target": "A", "id": rid } }));
    assert_eq!(from["spec"]["brush"], json!("clay_tubes"));
    let file = scratch("presets.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": file.to_string_lossy() }));
    let brushes = app.scene.brushes.clone();
    ok(app, ctx, tx, "new_scene", json!({}));
    assert!(app.scene.brushes.is_empty());
    ok(app, ctx, tx, "open_scene", json!({ "path": file.to_string_lossy() }));
    settle(app, ctx);
    assert_eq!(app.scene.brushes, brushes);
    // The human's brush on a preset; deleting it leaves the brush.
    let cur = ok(app, ctx, tx, "brush_preset", json!({ "action": "set_current", "preset": "ClayBuildup", "radius": 0.04 }));
    assert_eq!((cur["brush"].clone(), cur["preset"].clone(), cur["strength"].clone()), (json!("clay_buildup"), json!("ClayBuildup"), json!(0.5)));
    ok(app, ctx, tx, "brush_preset", json!({ "action": "delete", "name": "MyTubes" }));
    assert!(mcp(app, ctx, tx, "brush_preset", json!({ "action": "get", "name": "MyTubes" })).1);
    assert!(mcp(app, ctx, tx, "brush_preset", json!({ "action": "delete", "name": "Clay" })).1, "built-ins stay");
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.scene.brushes.contains_key("MyTubes"), "a delete is an undo step");
}

#[test]
fn a_posed_arm_stroke_is_the_mirrored_stroke_at_rest() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let body = json!({ "shape": "Clay", "name": "Body", "resolution": 96, "blobs": [
        { "type": "box", "center": [0, 1.1, 0], "size": [0.4, 0.6, 0.25], "blend": 0.02 },
        { "type": "capsule", "a": [0.15, 1.3, 0], "b": [0.85, 1.3, 0], "radius": 0.07, "bone": "arm.L" },
        { "type": "capsule", "a": [-0.15, 1.3, 0], "b": [-0.85, 1.3, 0], "radius": 0.07, "bone": "arm.R" },
    ] });
    let clay = ok(app, ctx, tx, "create_object", body);
    let id = clay["id"].as_u64().unwrap();
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Body", "detail_cell": 0.006 }));
    let rig: crate::rig::Rig = serde_json::from_value(json!({ "bones": [
        { "name": "spine", "head": [0, 0.8, 0], "tail": [0, 1.5, 0] },
        { "name": "arm.L", "parent": "spine", "head": [0.2, 1.3, 0], "tail": [0.85, 1.3, 0] },
        { "name": "arm.R", "parent": "spine", "head": [-0.2, 1.3, 0], "tail": [-0.85, 1.3, 0] },
    ], "split": false, "active": "wave", "clips": [{ "name": "wave", "loop": false, "tracks": [{ "bone": "arm.L", "keys": [{ "frame": 0 }, { "frame": 10, "rotate": [0, 0, 35] }] }] }] })).unwrap();
    app.scene.get_mut(id).unwrap().rig = Some(rig.clone());
    app.timeline.frame = 10.0;
    app.set_frame(Some(10.0));
    settle(app, ctx);
    // A point on top of the left forearm, at rest and as posed at frame 10.
    let rest = Vec3::new(0.7, 1.37, 0.0);
    let posed = rig.skin_matrices(Some(10.0))[1].transform_point3(rest);
    assert!(posed.distance(rest) > 0.2, "the arm is posed: {posed}");
    let stroke = |p: Vec3, posable: bool| json!({ "target": "Body", "strokes": [{ "brush": "standard", "point": p.to_array(), "radius": 0.05, "strength": 0.6, "symmetry": { "axes": ["x"], "posable": posable } }] });
    let reply = ok(app, ctx, tx, "sculpt", stroke(posed, true));
    assert_eq!(reply["strokes"][0]["pose_frame"], json!(10.0), "the editor's frame is kept: {reply}");
    settle(app, ctx);
    let a = app.scene.get(id).unwrap().clay.as_ref().unwrap().detail.clone().unwrap();
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Body", "full": true }));
    assert!(log.to_string().contains("\"frame\":10.0"), "the record keeps the pose frame: {log}");
    // stroke_edit replace takes a posable record's own spec (it names its
    // frame) and, with the frame left out, the editor's frame: both land
    // where the stroke did.
    let rec = &log["records"][0];
    let spec = rec["spec"].clone();
    assert_eq!(spec["symmetry"]["frame"], json!(10.0), "{spec}");
    let mut unframed = spec.clone();
    unframed["symmetry"].as_object_mut().unwrap().remove("frame");
    for (what, spec) in [("its own spec", spec), ("the frame left out", unframed)] {
        let id_now = ok(app, ctx, tx, "stroke_log", json!({ "target": "Body" }))["records"][0]["id"].clone();
        ok(app, ctx, tx, "stroke_edit", json!({ "target": "Body", "action": "replace", "id": id_now, "spec": spec }));
        settle(app, ctx);
        let replaced = app.scene.get(id).unwrap().clay.as_ref().unwrap().detail.clone().unwrap();
        let mut off = 0.0f32;
        crate::ledger::detail_changes(Some(&a), Some(&replaced), |_, _, u, v| off = off.max((u - v).abs()));
        assert!(off == 0.0, "a posable record replaced by {what} lands where it did: off by {off}");
        ok(app, ctx, tx, "undo", json!({}));
        settle(app, ctx);
    }
    ok(app, ctx, tx, "undo", json!({}));
    // The same stroke naming its frame lands the same with the timeline at rest.
    app.set_frame(None);
    settle(app, ctx);
    let mut named = stroke(posed, true);
    named["strokes"][0]["symmetry"]["frame"] = json!(10.0);
    ok(app, ctx, tx, "sculpt", named);
    settle(app, ctx);
    let again = app.scene.get(id).unwrap().clay.as_ref().unwrap().detail.clone().unwrap();
    let mut off = 0.0f32;
    crate::ledger::detail_changes(Some(&a), Some(&again), |_, _, u, v| off = off.max((u - v).abs()));
    assert!(off == 0.0, "a named pose frame does not read the timeline: off by {off}");
    ok(app, ctx, tx, "undo", json!({}));
    // The same stroke given at rest on the right arm, mirrored to the left.
    ok(app, ctx, tx, "sculpt", stroke(Vec3::new(-rest.x, rest.y, rest.z), false));
    settle(app, ctx);
    let b = app.scene.get(id).unwrap().clay.as_ref().unwrap().detail.clone().unwrap();
    let (mut worst, mut changed) = (0.0f32, 0);
    crate::ledger::detail_changes(Some(&a), Some(&b), |_, _, u, v| worst = worst.max((u - v).abs()));
    crate::ledger::detail_changes(None, Some(&a), |_, _, _, _| changed += 1);
    let voxel = Detail::voxel(&a);
    eprintln!("posable symmetry: the posed-arm stroke and the rest stroke differ by at most {:.3} voxel over {changed} changed points", worst / voxel);
    assert!(changed > 100, "{changed}");
    assert!(worst <= voxel, "{worst} > {voxel}");
}

#[test]
fn posable_nearest_refuses_far_and_non_finite_queries() {
    use super::Nearest;
    // A small mesh far from the origin: cell keys count from its corner.
    let base = Vec3::new(1000.0, -2000.0, 300.0);
    let pts: Vec<Vec3> = (0..8).map(|i| base + Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, (i >> 2) as f32) * 0.01).collect();
    let near = Nearest::new(pts.clone());
    assert_eq!(near.find(pts[5]), Some(5), "a point on the mesh finds itself");
    for q in [Vec3::splat(1.0e30), Vec3::new(-1.0e30, 0.0, 1.0e30), Vec3::splat(f32::MAX), Vec3::new(f32::NAN, 0.0, 0.0), Vec3::new(0.0, f32::INFINITY, 0.0), base + Vec3::splat(1.0)] {
        assert_eq!(near.find(q), None, "{q} is off the clay");
    }
    assert_eq!(Nearest::new(Vec::new()).find(Vec3::ZERO), None, "no points, no nearest");
}

#[test]
fn posable_unposer_skips_vertices_without_a_skin_blend() {
    use glam::Mat4;
    let rest = [Vec3::ZERO, Vec3::new(0.1, 0.0, 0.0), Vec3::new(0.2, 0.0, 0.0), Vec3::new(0.3, 0.0, 0.0)];
    let shift = Mat4::from_translation(Vec3::new(0.0, 0.01, 0.0));
    let blends = [shift, Mat4::ZERO, Mat4::from_scale(Vec3::new(1.0, 0.0, 1.0)), shift];
    let (kept, pts) = super::posed_points(rest.iter().copied(), |i| blends[i]);
    assert_eq!(kept, vec![0, 3], "zero and singular blends are left out");
    assert_eq!(pts, vec![Vec3::new(0.0, 0.01, 0.0), Vec3::new(0.3, 0.01, 0.0)]);
    // A zero blend put its vertex at the origin, the nearest to a stroke
    // there, with no inverse: the stroke was refused. Now the nearest
    // usable vertex answers.
    let near = super::Nearest::new(pts);
    assert_eq!(near.find(Vec3::ZERO).map(|i| kept[i]), Some(0));
}

/// Stage 8c through MCP: alpha create_vdm captures a horn, brush vdm
/// stamps it (a replayable stroke record, no opaque record) and keeps
/// replaying after the library drops it; the brushes refuse the wrong
/// kind of alpha; and a curve stroke replaced with one control point
/// moved (MoveCurve) equals a fresh stroke with the new points.
#[test]
fn vdm_stamps_and_curve_strokes_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Horn", "resolution": 64, "blobs": [
        { "type": "box", "center": [0, -0.15, 0], "size": [0.6, 0.3, 0.6], "blend": 0 },
        { "type": "capsule", "a": [0, -0.01, 0], "b": [0, 0.06, 0], "radius": 0.012, "blend": 0 },
        { "type": "capsule", "a": [0, 0.06, 0], "b": [0.045, 0.06, 0], "radius": 0.012, "blend": 0 },
    ] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Horn", "detail_cell": 0.004 }));
    slab(app, ctx, tx, "Plate");
    settle(app, ctx);
    let made = ok(app, ctx, tx, "alpha", json!({ "action": "create_vdm", "name": "horn", "target": "Horn", "center": [0, 0, 0], "radius": 0.08, "above": 0.1, "below": 0.02 }));
    assert!(made["vdm"]["bricks"].as_u64().unwrap() > 10 && made["vdm"]["cell"] == json!(0.004), "{made}");
    assert!(made["step"].is_null() || made["step"].is_number());
    let list = ok(app, ctx, tx, "alpha", json!({ "action": "list" }));
    assert_eq!(list["alphas"][0]["kind"], json!("vdm"), "{list}");
    let (shot, err) = mcp(app, ctx, tx, "alpha", json!({ "action": "preview", "name": "horn", "size": 64 }));
    assert!(!err, "{shot}");
    for bad in [json!({ "action": "create_vdm", "name": "flat", "target": "Plate", "center": [0, 0, 0], "radius": 0.08 }), json!({ "action": "create_vdm", "name": "x", "target": "Horn", "center": [0, 0, 0], "radius": 0.08, "cell": 1e-6 })] {
        assert!(mcp(app, ctx, tx, "alpha", bad).1);
    }
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Plate", "strokes": [{ "brush": "vdm", "point": [0.1, 0, 0.048], "radius": 0.08, "alpha": { "name": "horn" } }] }));
    assert!(r["strokes"][0]["changed_points"].as_u64().unwrap() > 1000, "{r}");
    settle(app, ctx);
    // The wrong kind of alpha for the brush is refused before anything runs.
    let checker_png = png("c.png", &checker(16, 4));
    ok(app, ctx, tx, "alpha", json!({ "action": "import", "name": "checker", "path": checker_png }));
    assert!(mcp(app, ctx, tx, "sculpt", json!({ "target": "Plate", "strokes": [{ "brush": "standard", "point": [0, 0, 0], "radius": 0.05, "alpha": { "name": "horn" } }] })).1);
    assert!(mcp(app, ctx, tx, "sculpt", json!({ "target": "Plate", "strokes": [{ "brush": "vdm", "point": [0, 0, 0], "radius": 0.05, "alpha": { "name": "checker" } }] })).1);
    // A name strokes use cannot change kind (no rebase could take it).
    let (e, err) = mcp(app, ctx, tx, "alpha", json!({ "action": "import", "name": "horn", "path": checker_png }));
    assert!(err && e.to_string().contains("cannot become"), "{e}");
    // Strokes kept only in an archived layer (baked into base) count too:
    // a stamp of its own name, used by nothing else.
    ok(app, ctx, tx, "alpha", json!({ "action": "create_vdm", "name": "spike", "target": "Horn", "center": [0, 0, 0], "radius": 0.08, "above": 0.1, "below": 0.02 }));
    slab(app, ctx, tx, "Shelf");
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Shelf", "action": "add", "name": "bumps" }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Shelf", "layer": "bumps", "strokes": [{ "brush": "vdm", "point": [0.1, 0, 0.048], "radius": 0.08, "alpha": { "name": "spike" } }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Shelf", "action": "bake", "name": "bumps" }));
    settle(app, ctx);
    let shelf = app.scene.objects.iter().find(|o| o.name == "Shelf").and_then(|o| o.clay.clone()).unwrap();
    let (mut live, mut archived) = (0, 0);
    crate::ledger::alpha_strokes(&shelf, |_| live += 1);
    crate::ledger::archived_alpha_strokes(&shelf, |_| archived += 1);
    assert_eq!((live, archived), (0, 1), "the stamp's record is only in the archive");
    let (e, err) = mcp(app, ctx, tx, "alpha", json!({ "action": "import", "name": "spike", "path": checker_png }));
    assert!(err && e.to_string().contains("Shelf") && e.to_string().contains("cannot become"), "{e}");
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Plate", "full": true, "verbose": true }));
    assert_eq!(log["total"], json!(1), "{log}");
    let rec = &log["records"][0];
    assert_eq!((rec["kind"].clone(), rec["brush"].clone()), (json!("stroke"), json!("vdm")));
    assert_eq!(rec["resolved"]["params"]["alpha"]["hash"], made["hash"]);
    // Replays bit for bit, also once the library no longer has it.
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Plate", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
    ok(app, ctx, tx, "alpha", json!({ "action": "delete", "name": "horn" }));
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Plate", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
    settle(app, ctx);

    // MoveCurve: a curve stroke, then its record replaced with one control
    // point moved, against a fresh stroke with the moved points.
    slab(app, ctx, tx, "A");
    slab(app, ctx, tx, "B");
    let pts = json!([[-0.12, 0, 0], [-0.04, 0, 0.06], [0.04, 0, -0.04], [0.12, 0, 0.02]]);
    let mut moved = pts.clone();
    moved[2] = json!([0.05, 0, 0.05]);
    let curve = |p: &Value| json!({ "brush": "standard", "kind": { "curve": { "closed": false } }, "points": p, "radius": 0.02, "strength": 0.4 });
    ok(app, ctx, tx, "sculpt", json!({ "target": "A", "strokes": [curve(&pts)] }));
    settle(app, ctx);
    let first = ok(app, ctx, tx, "stroke_log", json!({ "target": "A", "full": true, "verbose": true }));
    let f32s = |v: &Value| serde_json::from_value::<Vec<[f32; 3]>>(v.clone()).unwrap();
    assert_eq!(f32s(&first["records"][0]["spec"]["points"]), f32s(&pts), "the record keeps the control points");
    assert_eq!(first["records"][0]["stroke_kind"], json!({ "curve": { "closed": false } }));
    let mut spec = first["records"][0]["spec"].clone();
    spec["points"][2] = moved[2].clone();
    ok(app, ctx, tx, "stroke_edit", json!({ "target": "A", "action": "replace", "id": first["records"][0]["id"], "spec": spec }));
    settle(app, ctx);
    ok(app, ctx, tx, "sculpt", json!({ "target": "B", "strokes": [curve(&moved)] }));
    settle(app, ctx);
    let (la, lb) = (ok(app, ctx, tx, "stroke_log", json!({ "target": "A", "full": true, "verbose": true })), ok(app, ctx, tx, "stroke_log", json!({ "target": "B", "full": true, "verbose": true })));
    assert_eq!(la["total"], json!(1));
    assert_ne!(la["records"][0]["result_hash"], first["records"][0]["result_hash"], "the move changed the stroke");
    assert_eq!(la["records"][0]["result_hash"], lb["records"][0]["result_hash"], "MoveCurve equals a fresh stroke with the new points");
    assert_eq!(la["records"][0]["spec"], lb["records"][0]["spec"]);
    let grid = |n: &str| <Detail as crate::ledger::Grid>::hash(app.scene.objects.iter().find(|o| o.name == n).unwrap().clay.as_ref().unwrap().detail.as_ref());
    assert_eq!(grid("A"), grid("B"));
}
