//! Stage 5b through MCP, on a headless editor (skips without a GPU
//! adapter): proportion guides set, measured on a figure of known
//! proportions and against a turnaround sheet, in quality_report, saved,
//! undone, refused out of bounds, and never part of exports, print, film,
//! masks or rays.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, ok, settle};
use super::sheet::synthetic;
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

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("clide3d_guides_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A clay figure of five stacked balls of radius 0.1 on y = 0: exactly 5
/// heads tall (1.0), the top ball named head; two eye objects 0.12 apart.
fn figure(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) {
    let blobs: Vec<Value> = (0..5).map(|k| json!({ "type": "sphere", "name": if k == 4 { "head".to_string() } else { format!("ball{k}") }, "center": [0, 0.1 + 0.2 * k as f32, 0], "radius": 0.1, "blend": 0 })).collect();
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Totem", "resolution": 64, "blobs": blobs }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Eye", "name": "eye.L", "translate": [0.06, 0.92, 0.07], "scale": [0.02, 0.02, 0.02] }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Eye", "name": "eye.R", "translate": [-0.06, 0.92, 0.07], "scale": [0.02, 0.02, 0.02] }));
    settle(app, ctx);
}

fn targets() -> Value {
    json!([
        { "name": "head_ratio", "of": ["head"], "value": 0.2, "tolerance": 0.001 },
        { "name": "heads_tall", "of": ["head", "ball0"], "to": "head_unit", "value": 5, "tolerance": 0.001 },
        { "name": "eye_spacing", "between": ["eye.R", "eye.L"], "axis": "x", "value": 0.12, "tolerance": 0.001 },
        { "name": "eyes_on_line", "between": ["eye_line", "eye.L"], "value": 0.0, "tolerance": 0.001 },
        { "name": "big_head", "of": ["head"], "value": 0.33, "tolerance": 0.03 },
    ])
}

fn by_name<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no {name} in {list}"))
}

/// `measure_head_ratio` through MCP: head units from the model's height,
/// the ratio, the head units, the eye spacing and the eye line read back
/// exactly; a target off by 0.13 fails; the same reply twice; one undo
/// step per set.
#[test]
fn guides_measure_a_figure_of_known_proportions() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    figure(app, ctx, tx);
    let step = app.history.current();
    let set = ok(app, ctx, tx, "guides", json!({ "action": "set", "target": "Totem", "head_units": { "count": 5 }, "lines": [{ "name": "eye_line", "y": 0.92 }, { "name": "center", "x": 0 }], "targets": targets() }));
    eprintln!("set: {set}");
    assert_eq!(app.history.current(), step + 1, "one undo step");
    let h = &set["guides"]["head_units"];
    assert!(h["bottom"].as_f64().unwrap().abs() < 1e-4 && (h["top"].as_f64().unwrap() - 1.0).abs() < 1e-4, "from the model: {h}");
    assert_eq!(set["guides"]["targets"][0]["to"], "total_height", "the default is stored");
    let t = std::time::Instant::now();
    let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Totem" }));
    eprintln!("measure ({:.1} ms): {}", t.elapsed().as_secs_f64() * 1e3, serde_json::to_string_pretty(&m).unwrap());
    let ts = &m["targets"];
    let measured = |n: &str| by_name(ts, n)["measured"].as_f64().unwrap();
    assert!((measured("head_ratio") - 0.2).abs() < 1e-4);
    assert!((measured("heads_tall") - 5.0).abs() < 1e-3);
    assert!((measured("eye_spacing") - 0.12).abs() < 1e-4, "{}", measured("eye_spacing"));
    assert!(measured("eyes_on_line").abs() < 1e-4);
    for n in ["head_ratio", "heads_tall", "eye_spacing", "eyes_on_line"] {
        assert_eq!(by_name(ts, n)["pass"], true, "{n}");
    }
    assert_eq!(by_name(ts, "big_head")["pass"], false);
    assert_eq!(m["failures"].as_array().unwrap().len(), 1);
    assert_eq!(m["pass"], false);
    assert!((m["head_units"]["model_heads"].as_f64().unwrap() - 5.0).abs() < 1e-3);
    let head = by_name(&m["head_units"]["blobs"], "head");
    assert!(head["top_heads"].as_f64().unwrap().abs() < 1e-3 && (head["bottom_heads"].as_f64().unwrap() - 1.0).abs() < 1e-3, "{head}");
    assert_eq!(ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Totem" })), m, "the same reply twice");
    // Measuring is not an edit.
    assert_eq!(app.history.current(), step + 1);
    // A head of radius 0.2 on the same body: 0.4 of 1.2, a third, and
    // big_head passes.
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Totem", "update": [{ "blob": 4, "radius": 0.2, "center": [0, 1.0, 0] }] }));
    settle(app, ctx);
    let grown = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Totem" }));
    let r = by_name(&grown["targets"], "big_head")["measured"].as_f64().unwrap();
    assert!((r - 1.0 / 3.0).abs() < 1e-4, "{r}");
    assert_eq!(by_name(&grown["targets"], "big_head")["pass"], true);
    // Undo and redo: one step each way.
    ok(app, ctx, tx, "undo", json!({}));
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.scene.guides.is_empty(), "undone");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(app.scene.guides.targets.len(), 5);
}

/// `guides_saved_and_undone`: saved with the scene and back on open; a
/// new scene has none; clear is an undo step; damaged guides in a file
/// load as none.
#[test]
fn guides_saved_and_undone() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("save");
    figure(app, ctx, tx);
    ok(app, ctx, tx, "guides", json!({ "action": "set", "head_units": { "count": 4, "bottom": 0, "top": 1 }, "lines": [{ "name": "eye_line", "y": 0.92 }], "targets": targets() }));
    let saved = app.scene.guides.clone();
    let file = d.join("scene.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": file.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    assert!(app.scene.guides.is_empty());
    ok(app, ctx, tx, "open_scene", json!({ "path": file.display().to_string() }));
    assert_eq!(app.scene.guides, saved, "the guides come back equal");
    let step = app.history.current();
    let r = ok(app, ctx, tx, "guides", json!({ "action": "clear", "parts": ["lines"] }));
    assert_eq!(r["guides"]["lines"], Value::Null);
    assert!(app.scene.guides.lines.is_empty() && app.scene.guides.head_units.is_some());
    assert_eq!(app.history.current(), step + 1);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.scene.guides, saved);
    // A file whose guides are out of bounds opens without them.
    let mut text: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    text["guides"]["head_units"]["count"] = json!(40);
    let broken = d.join("broken.json");
    std::fs::write(&broken, text.to_string()).unwrap();
    ok(app, ctx, tx, "open_scene", json!({ "path": broken.display().to_string() }));
    assert!(app.scene.guides.is_empty());
    assert!(app.scene.objects.iter().any(|o| o.name == "Totem"), "the rest of the file loads");
    // And guides that do not parse at all.
    text["guides"] = json!({ "lines": "not a list" });
    std::fs::write(&broken, text.to_string()).unwrap();
    ok(app, ctx, tx, "open_scene", json!({ "path": broken.display().to_string() }));
    assert!(app.scene.guides.is_empty() && app.scene.objects.iter().any(|o| o.name == "Totem"));
    // Review fix (5c): `set` writes a part `per`'s axis (y, its height),
    // so the file it saves keeps that meaning; a file from before
    // per_axis (a part per along the distance's axis) opens measuring as
    // it did then.
    let per = json!([{ "name": "wide", "between": ["eye.R", "eye.L"], "axis": "x", "per": "head", "value": 0.6, "tolerance": 0.01 }]);
    let r = ok(app, ctx, tx, "guides", json!({ "action": "set", "targets": per }));
    assert_eq!(r["guides"]["targets"][0]["per_axis"], "y", "{}", r["guides"]);
    ok(app, ctx, tx, "save_scene", json!({ "path": file.display().to_string() }));
    let mut text: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(text["guides"]["targets"][0]["per_axis"], "y");
    ok(app, ctx, tx, "open_scene", json!({ "path": file.display().to_string() }));
    let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Totem" }));
    assert_eq!(by_name(&m["targets"], "wide")["per_axis"], "y");
    text["guides"]["targets"][0].as_object_mut().unwrap().remove("per_axis");
    std::fs::write(&broken, text.to_string()).unwrap();
    ok(app, ctx, tx, "open_scene", json!({ "path": broken.display().to_string() }));
    assert_eq!(app.scene.guides.targets[0].per_axis, Some(crate::guides::Axis::X));
    let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Totem" }));
    assert_eq!(by_name(&m["targets"], "wide")["per_axis"], "x");
    let _ = std::fs::remove_dir_all(&d);
}

/// Guides are not geometry: with them set, the .glb is the same bytes,
/// print and film the same triangles, masks the same pixels and rays the
/// same hits, while render_views guides: true draws them.
#[test]
fn guides_never_appear_in_exports() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("geometry");
    figure(app, ctx, tx);
    let glb = |app: &mut ClideApp, name: &str| {
        let p = d.join(name);
        ok(app, ctx, tx, "export_glb", json!({ "path": p }));
        std::fs::read(&p).unwrap()
    };
    let print = |app: &mut ClideApp, name: &str| ok(app, ctx, tx, "export_print", json!({ "path": d.join(name) }))["triangles"].clone();
    let film = |app: &mut ClideApp| ok(app, ctx, tx, "render_film", json!({ "size": 64, "samples": 1, "denoise": false, "views": ["front"] }))["triangles"].clone();
    let probe = |app: &mut ClideApp| ok(app, ctx, tx, "surface_point", json!({ "view": "front", "at": [[0.0, 0.5], [0.3, 0.6], [0.0, 0.92]] }))["results"].clone();
    let before = (glb(app, "a.glb"), print(app, "a.stl"), film(app), probe(app), app.scene.objects.len());
    ok(app, ctx, tx, "guides", json!({ "action": "set", "head_units": { "count": 5, "bottom": 0, "top": 1 }, "lines": [{ "name": "eye_line", "y": 0.92 }, { "name": "center", "x": 0 }, { "name": "mid", "z": 0 }], "targets": targets() }));
    let after = (glb(app, "b.glb"), print(app, "b.stl"), film(app), probe(app), app.scene.objects.len());
    assert!(after.0 == before.0, "the .glb is the same bytes with guides set");
    assert_eq!(after.1, before.1, "print triangles");
    assert_eq!(after.2, before.2, "film triangles");
    assert_eq!(after.3, before.3, "rays hit the same");
    assert_eq!(after.4, before.4, "no objects");
    let render = |app: &ClideApp, guides: bool, mask: bool| {
        let shot = super::eyes::Shot::framing(&json!("front"), glam::Vec3::new(0.0, 0.5, 0.0), 0.7).unwrap();
        app.render_shot_extras(&shot, 128, 128, mask, false, super::eyes::Extras { guides: guides.then_some((glam::Vec3::new(0.0, 0.5, 0.0), 0.7)), ..Default::default() }).unwrap()
    };
    let differ = |a: &egui::ColorImage, b: &egui::ColorImage| a.pixels.iter().zip(&b.pixels).filter(|(x, y)| x != y).count();
    let drawn = differ(&render(app, true, false), &render(app, false, false));
    eprintln!("render_views front: {drawn} pixels differ with the guides drawn");
    assert!(drawn > 300);
    assert_eq!(differ(&render(app, true, true), &render(app, false, true)), 0, "masks never draw them");
    let r = ok(app, ctx, tx, "render_views", json!({ "views": ["front", "top"], "size": 128, "guides": true }));
    assert_eq!(r["guides"], true);
    let _ = std::fs::remove_dir_all(&d);
}

/// quality_report carries the guides as proportions, failing on a target
/// out of tolerance (and not with proportions: false, or without guides).
#[test]
fn quality_report_reports_proportions() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    figure(app, ctx, tx);
    let plain = ok(app, ctx, tx, "quality_report", json!({ "target": "Totem", "samples": 2000 }));
    assert!(plain.get("proportions").is_none());
    ok(app, ctx, tx, "guides", json!({ "action": "set", "head_units": { "count": 5, "bottom": 0, "top": 1 }, "lines": [{ "name": "eye_line", "y": 0.92 }], "targets": targets() }));
    let r = ok(app, ctx, tx, "quality_report", json!({ "target": "Totem", "samples": 2000 }));
    let failures = r["failures"].to_string();
    eprintln!("quality_report failures: {failures}");
    assert!(failures.contains("proportion big_head") && !failures.contains("head_ratio"), "{failures}");
    assert_eq!(r["pass"], false);
    assert_eq!(by_name(&r["proportions"]["targets"], "head_ratio")["pass"], true);
    let off = ok(app, ctx, tx, "quality_report", json!({ "target": "Totem", "samples": 2000, "proportions": false }));
    assert!(off.get("proportions").is_none() && !off["failures"].to_string().contains("proportion"));
    assert_eq!(off["pass"], plain["pass"]);
}

/// Against a turnaround sheet: the sheet's subject (0.93 tall) reads its
/// height in head units, and the head's width at the eye line (a ball of
/// radius 0.18 at 0.75: 0.36 across) against the model's.
#[test]
fn guides_measure_against_the_sheet() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("sheet");
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Buddy", "resolution": 96, "blobs": [
        { "type": "box", "name": "body", "center": [0, 0.3, 0], "size": [synthetic::WIDTH, 0.6, synthetic::DEPTH], "blend": 0 },
        { "type": "sphere", "name": "head", "center": [0, 0.75, 0], "radius": 0.18, "blend": 0 },
    ] }));
    settle(app, ctx);
    let (img, _) = synthetic::sheet(400.0, 1.25, synthetic::DEPTH, true);
    let path = d.join("sheet.png");
    img.save(&path).unwrap();
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": synthetic::HEIGHT } }));
    ok(app, ctx, tx, "guides", json!({ "action": "set", "target": "Buddy", "head_units": { "count": 3 }, "lines": [{ "name": "eye_line", "y": 0.75 }, { "name": "belly", "y": 0.3 }], "targets": [
        { "name": "as_tall_as_the_sheet", "of": ["body", "head"], "to": "sheet_height", "value": 1.0, "tolerance": 0.01 },
        { "name": "head_of_sheet", "of": ["head"], "to": "sheet_height", "value": 0.36 / 0.93, "tolerance": 0.01 },
    ] }));
    // Review fix: a plane is no model to take head units from (set refused
    // it only at measure before).
    let kept = app.scene.guides.clone();
    let (v, err) = mcp(app, ctx, tx, "guides", json!({ "action": "set", "target": "Reference front", "head_units": { "count": 3 } }));
    assert!(err && v.to_string().contains("not a model"), "{v}");
    assert_eq!(app.scene.guides, kept);
    let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Buddy" }));
    eprintln!("against the sheet: {}", serde_json::to_string_pretty(&m["sheet"]).unwrap());
    let views = m["sheet"].as_array().unwrap();
    assert_eq!(views.len(), 3);
    let px = 1.0 / 400.0;
    for v in views {
        assert!((v["height"].as_f64().unwrap() - 0.93).abs() <= 2.0 * px, "{v}");
        assert!((v["model_over_sheet"].as_f64().unwrap() - 1.0).abs() < 0.01, "{v}");
        let eye = by_name(&v["lines"], "eye_line");
        assert!((eye["sheet_width"].as_f64().unwrap() - 0.36).abs() <= 2.0 * px, "{eye}");
        assert!((eye["model_width"].as_f64().unwrap() - 0.36).abs() < 1e-3, "{eye}");
        let belly = by_name(&v["lines"], "belly");
        let across = if v["view"] == "right" { synthetic::DEPTH } else { synthetic::WIDTH } as f64;
        assert!((belly["sheet_width"].as_f64().unwrap() - across).abs() <= 2.0 * px && (belly["model_width"].as_f64().unwrap() - across).abs() < 1e-3, "{belly}");
    }
    assert_eq!(m["pass"], true, "{}", m["failures"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Refusals from the arguments, each leaving the guides as they were.
#[test]
fn guides_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    figure(app, ctx, tx);
    ok(app, ctx, tx, "guides", json!({ "action": "set", "lines": [{ "name": "eye_line", "y": 0.92 }] }));
    let kept = app.scene.guides.clone();
    let refused = |app: &mut ClideApp, args: Value, says: &str| {
        let step = app.history.current();
        let (v, err) = mcp(app, ctx, tx, "guides", args.clone());
        assert!(err && v.to_string().contains(says), "{args}: {v}");
        assert_eq!(app.scene.guides, kept, "{args}");
        assert_eq!(app.history.current(), step);
    };
    let many: Vec<Value> = (0..33).map(|i| json!({ "name": format!("l{i}"), "y": 0 })).collect();
    refused(app, json!({}), "action is required");
    refused(app, json!({ "action": "draw" }), "unknown action");
    refused(app, json!({ "action": "set" }), "set needs head_units, lines or targets");
    refused(app, json!({ "action": "set", "colour": "red", "lines": [] }), "takes no colour");
    refused(app, json!({ "action": "set", "head_units": { "count": 9, "bottom": 0, "top": 1 } }), "head_units.count");
    refused(app, json!({ "action": "set", "head_units": { "count": 3, "bottom": 1, "top": 0 } }), "must be above");
    refused(app, json!({ "action": "set", "head_units": { "count": 3 } }), "from target");
    refused(app, json!({ "action": "set", "lines": many }), "at most 32 lines");
    refused(app, json!({ "action": "set", "lines": [{ "name": "a", "y": 0, "x": 1 }] }), "exactly one of x, y or z");
    refused(app, json!({ "action": "set", "lines": [{ "name": "a", "w": 0 }] }), "takes no w");
    refused(app, json!({ "action": "set", "targets": [{ "name": "a", "of": ["head"], "value": 1 }] }), "tolerance");
    refused(app, json!({ "action": "set", "targets": [{ "name": "a", "of": ["head"], "between": ["x", "y"], "value": 1, "tolerance": 0 }] }), "not both");
    refused(app, json!({ "action": "measure" }), "measure needs target");
    refused(app, json!({ "action": "measure", "target": "Nobody" }), "no object");
    refused(app, json!({ "action": "clear", "parts": ["everything"] }), "unknown part");
}
