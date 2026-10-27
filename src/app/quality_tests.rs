//! Stage 2 (docs/QUALITY_PLAN.md) through MCP in a headless editor: the
//! quality shades, per-view references and contour compare, feature
//! close-ups and `quality_report`. Needs a wgpu device for the editor's
//! render state (skips without one).

use std::sync::mpsc::Sender;

use base64::Engine;
use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, ok, settle};
use super::recorder::reply_json;
use crate::mcp::ToolCall;
use crate::quality::Shade;

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

/// The first image of a reply, decoded.
fn image_of(content: &[Value]) -> image::RgbaImage {
    let data = content.iter().find(|c| c["type"] == "image").and_then(|c| c["data"].as_str()).expect("an image");
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
    image::load_from_memory(&bytes).unwrap().to_rgba8()
}

/// A tool call's whole reply content (text and images) through MCP.
fn content_of(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, args: Value) -> Vec<Value> {
    let msg = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (tx, c) = (tx.clone(), ctx.clone());
    let call = std::thread::spawn(move || crate::mcp::dispatch(&msg, &tx, &c));
    let start = std::time::Instant::now();
    while !call.is_finished() {
        frame(app, ctx, Vec::new());
        assert!(start.elapsed() < std::time::Duration::from_secs(120), "{name} never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r = call.join().expect("dispatch thread").expect("a reply")["result"].clone();
    assert!(r["isError"] != true, "{name}: {r}");
    r["content"].as_array().unwrap().clone()
}

/// The mean absolute difference of two images' channels (0..255).
fn image_diff(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions());
    a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.as_raw().len() as f64
}

/// `render_views shade` colors a clay by its field (a dent shows blue in
/// curvature), `set_display shade` shows the same in the viewport, and
/// the Shading menu sends the call an agent would, with the same reply.
#[test]
fn shades_from_agents_and_the_shading_menu() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }, { "type": "sphere", "op": "carve", "center": [0, 0, 0.55], "radius": 0.15, "blend": 0.05 }] }));
    settle(app, ctx);
    let args = |shade: &str| json!({ "views": ["front"], "size": 128, "include_reference": false, "shade": shade });
    let beauty = app.render_views(&args("beauty")).unwrap();
    let curved = app.render_views(&args("curvature")).unwrap();
    assert_eq!(reply_json(&curved)["shade"], "curvature");
    // The dent faces the camera at the tile's middle: concave, blue.
    let (b, c) = (image_of(&beauty), image_of(&curved));
    let mid = c.get_pixel(64, 64).0;
    assert!(mid[2] as i32 > mid[0] as i32 + 40, "the dent is blue in curvature: {mid:?} (beauty {:?})", b.get_pixel(64, 64).0);
    assert!(app.render_views(&args("lumpiness")).is_ok() && app.render_views(&args("normals")).is_ok() && app.render_views(&args("wire")).is_ok());
    let (e, err) = mcp(app, ctx, tx, "render_views", args("glossy"));
    assert!(err && e.to_string().contains("unknown shade"), "{e}");

    // The viewport: set_display shade colors the drawn mesh.
    let id = id_of(app, "Ball");
    let r = ok(app, ctx, tx, "set_display", json!({ "shade": "curvature" }));
    assert_eq!(r["shade"], "curvature");
    settle(app, ctx);
    let o = app.scene.get(id).unwrap().clone();
    let shown = app.meshes.display(&o).unwrap().clone();
    let truth = crate::quality::shade_colors(Shade::Curvature, o.clay.as_ref().unwrap(), app.meshes.unposed(id).unwrap(), crate::mesh::SHADE_SAMPLES).unwrap();
    assert!(shown.vertices.iter().zip(&truth).all(|(v, c)| v.color == *c), "the viewport draws the field's curvature");
    // An explicit shade wins over the viewport's: beauty draws no colors
    // (as with the viewport in beauty), and without one the viewport's
    // shade is drawn and named, measured once for every tile.
    let beauty_now = image_of(&app.render_views(&args("beauty")).unwrap());
    assert_eq!(reply_json(&app.render_views(&args("beauty")).unwrap())["shade"], "beauty");
    assert!(image_diff(&beauty_now, &b) == 0.0, "an explicit beauty is beauty under a viewport shade: {}", image_diff(&beauty_now, &b));
    let default = app.render_views(&json!({ "views": ["front", "right", "back"], "size": 128, "include_reference": false })).unwrap();
    assert_eq!(reply_json(&default)["shade"], "curvature");
    let measured = app.shade_measures.get();
    let one = app.render_views(&json!({ "views": ["front"], "size": 128, "include_reference": false })).unwrap();
    assert_eq!(image_diff(&image_of(&one), &c), 0.0, "the default is the viewport's shade");
    app.render_views(&json!({ "views": ["front", "right", "back", "top"], "size": 64, "include_reference": false, "shade": "lumpiness" })).unwrap();
    assert_eq!(app.shade_measures.get(), measured + 2, "one measure per call, not per tile");
    ok(app, ctx, tx, "set_display", json!({ "shade": "wire" }));
    let wire_default = app.render_views(&json!({ "views": ["front"], "size": 128, "include_reference": false })).unwrap();
    assert_eq!(reply_json(&wire_default)["shade"], "wire");
    assert_eq!(image_diff(&image_of(&wire_default), &image_of(&app.render_views(&args("wire")).unwrap())), 0.0, "the viewport's wire is drawn by default");
    assert!(image_diff(&image_of(&wire_default), &b) > 0.0, "wire draws edges over beauty");
    let agent = ok(app, ctx, tx, "set_display", json!({ "shade": "beauty" }));
    settle(app, ctx);
    assert!(app.meshes.display(&o).unwrap().vertices.iter().zip(&truth).any(|(v, c)| v.color != *c), "beauty drops the colors");
    // The menu's request, through the UI path: the same reply and effect.
    let job = app.send_ui_call("set_display", json!({ "shade": "lumpiness" }));
    settle(app, ctx);
    assert_eq!(app.meshes.shade, Shade::Lumpiness);
    let ui = reply_json(app.jobs.result(job).unwrap().as_ref().unwrap());
    let agent_lumpy = ok(app, ctx, tx, "set_display", json!({ "shade": "lumpiness" }));
    assert_eq!(ui, agent_lumpy);
    assert_eq!(agent["shade"], "beauty");
    let scene = ok(app, ctx, tx, "get_scene", json!({ "detail": true }));
    assert!(scene["ui_activity"].as_array().unwrap().iter().any(|a| a["tool"] == "set_display" && a["request"]["shade"] == "lumpiness"));
    let (e, err) = mcp(app, ctx, tx, "set_display", json!({ "shade": 3 }));
    assert!(err, "{e}");
    assert_eq!(app.meshes.shade, Shade::Lumpiness, "a refused call changes nothing");
}

/// A ball (radius 0.3, detail cell 1 cm) with bumps dabbed on its front
/// by inflate strokes (shape, not texture: they count as lumps).
fn lumpy_ball(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.01 }));
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 40) as f32 / (1u64 << 24) as f32
    };
    let strokes: Vec<Value> = (0..48)
        .map(|_| {
            let d = glam::Vec3::new(next() - 0.5, next() - 0.5, 0.6 + next() * 0.4).normalize();
            json!({ "brush": "inflate", "points": [(d * 0.3).to_array()], "radius": 0.025, "strength": 0.5 })
        })
        .collect();
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": strokes }));
    settle(app, ctx);
}

/// `quality_report` fails a lumpy front, points at it in world space,
/// and passes once its hint's polish ran; bad sizes are refused before
/// anything is sampled, and its reply is the same twice and on 1 and 12
/// threads (and as a job).
#[test]
fn quality_report_finds_and_fixes_lumps() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    lumpy_ball(app, ctx, tx);
    let call = json!({ "target": "Ball", "samples": 6000, "regions": [{ "name": "front", "center": [0, 0, 0.3], "radius": 0.2 }, { "name": "back", "center": [0, 0, -0.3], "radius": 0.2 }] });
    let r = ok(app, ctx, tx, "quality_report", call.clone());
    println!("lumpy: {r}");
    assert_eq!(r["pass"], false, "{r}");
    let failures: Vec<&str> = r["failures"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(failures.iter().any(|f| f.starts_with("lumpiness front")), "{failures:?}");
    assert!(!failures.iter().any(|f| f.starts_with("lumpiness back")), "the back is clean: {failures:?}");
    let worst = r["lumpiness"]["worst"].as_array().unwrap();
    assert!(!worst.is_empty() && worst.iter().all(|w| w["at"][2].as_f64().unwrap() > 0.0), "the worst lumps are on the front: {worst:?}");
    assert_eq!(r["topology"]["watertight"], true, "{r}");
    // The same reply again, and on one thread.
    assert_eq!(ok(app, ctx, tx, "quality_report", call.clone()), r);
    let one = crate::clay::with_threads(1, || ok(app, ctx, tx, "quality_report", call.clone()));
    assert_eq!(one, r, "1 thread against all");
    // The hint's fix: a light polish over the front.
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "polish": { "amount": 0.1, "region": { "center": [0, 0, 0.3], "radius": 0.25 } } }));
    settle(app, ctx);
    let fixed = ok(app, ctx, tx, "quality_report", call.clone());
    println!("polished: {fixed}");
    let (before, after) = (r["lumpiness"]["overall"].as_f64().unwrap(), fixed["lumpiness"]["overall"].as_f64().unwrap());
    assert!(after < 0.5 * before && fixed["pass"] == true, "{before} -> {after}: {fixed}");
    // Images: the curvature and lumpiness sheets.
    let (out, _) = (app.at_rest(|a| a.quality_report(&json!({ "target": "Ball", "samples": 2000, "images": true }), false)).unwrap(), ());
    let super::jobs::Started::Now(Ok(content)) = out else { panic!("no reply") };
    assert!(image_of(&content).width() > 600);
    // Refused from the arguments alone.
    for (bad, says) in [
        (json!({ "target": "Ball", "samples": 200_001 }), "samples"),
        (json!({ "target": "Ball", "samples": 999 }), "samples"),
        (json!({ "target": "Ball", "regions": vec![json!({ "center": [0, 0, 0], "radius": 0.1 }); 17] }), "at most 16 regions"),
        (json!({ "target": "Ball", "regions": [{ "center": [0, 0], "radius": 0.1 }] }), "center"),
        (json!({ "target": "Ball", "thresholds": { "lumps": 1 } }), "unknown threshold"),
    ] {
        let start = std::time::Instant::now();
        let (e, err) = mcp(app, ctx, tx, "quality_report", bad.clone());
        assert!(err && e.to_string().contains(says), "{bad}: {e}");
        assert!(start.elapsed().as_secs_f32() < 5.0);
    }
    ok(app, ctx, tx, "create_object", json!({ "shape": "Cube", "name": "Box" }));
    let (e, err) = mcp(app, ctx, tx, "quality_report", json!({ "target": "Box" }));
    assert!(err && e.to_string().contains("not a clay"), "{e}");
    // A job (over 40,000 samples) answers the same twice.
    let big = json!({ "target": "Ball", "samples": 45_000 });
    let a = ok(app, ctx, tx, "quality_report", big.clone());
    assert_eq!(a["samples"], 45_000, "{a}");
    assert_eq!(ok(app, ctx, tx, "quality_report", big), a);
}

/// A head (an ellipsoid) with a nose on its front.
fn head(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 96, "blobs": [
        { "type": "ellipsoid", "name": "skull", "center": [0, 0, 0], "radii": [0.3, 0.4, 0.33], "blend": 0 },
        { "type": "capsule", "name": "nose", "a": [0, 0.02, 0.28], "b": [0, -0.06, 0.42], "radius": 0.045, "blend": 0.03 },
    ] }));
    settle(app, ctx);
}

/// Saves a mask render of the scene from `view` (white subject on black)
/// as a reference image.
fn mask_reference(app: &ClideApp, view: &str, dir: &std::path::Path) -> std::path::PathBuf {
    let (center, radius) = app.scene_sphere();
    let shot = super::eyes::Shot::framing(&json!(view), center, radius).unwrap();
    let img = app.render_shot_extras(&shot, 400, 400, true, false, Default::default()).unwrap();
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
    let path = dir.join(format!("{view}.png"));
    image::RgbaImage::from_raw(400, 400, rgba).unwrap().save(&path).unwrap();
    path
}

/// D13: one reference per view slot. Silhouettes rendered from the head
/// itself match it in every view; without its nose the right view (its
/// profile) is the worst, and quality_report says so too. Old one-image
/// calls keep working.
#[test]
fn multiview_compare() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    head(app, ctx, tx);
    let dir = std::env::temp_dir().join(format!("clide3d_multiview_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for view in ["front", "right", "back"] {
        let path = mask_reference(app, view, &dir);
        let r = ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": view }));
        assert_eq!(r["view"], view, "{r}");
    }
    let r = ok(app, ctx, tx, "set_reference", json!({ "view": "right", "opacity": 0.3 }));
    assert_eq!((r["slots"].clone(), r["overlay"].clone()), (json!(["front", "back", "right"]), json!("side")), "{r}");
    let all = ok(app, ctx, tx, "compare_reference", json!({}));
    println!("matching: {all}");
    let views = all["views"].as_array().unwrap();
    assert_eq!(views.len(), 3);
    for v in views {
        assert!(v["overlap_iou"].as_f64().unwrap() >= 0.97, "{v}");
        assert!(v["contour"]["mean_pct"].as_f64().unwrap() < 1.0, "{v}");
    }
    // A {dir} view is the named one's camera.
    let right = ok(app, ctx, tx, "compare_reference", json!({ "view": "right" }));
    let by_dir = ok(app, ctx, tx, "compare_reference", json!({ "views": [{ "dir": [1, 0, 0], "slot": "right" }] }));
    assert_eq!(by_dir["views"][0]["overlap_iou"], right["overlap_iou"], "{by_dir}");
    let (e, err) = mcp(app, ctx, tx, "compare_reference", json!({ "views": [{ "dir": [0, 0, 0], "slot": "right" }] }));
    assert!(err && e.to_string().contains("nonzero"), "{e}");
    let (e, err) = mcp(app, ctx, tx, "compare_reference", json!({ "views": vec![json!("front"); 13] }));
    assert!(err && e.to_string().contains("at most 12"), "{e}");
    let (e, err) = mcp(app, ctx, tx, "compare_reference", json!({ "view": "top" }));
    assert!(err && e.to_string().contains("no reference for view top"), "{e}");
    // Without the nose the profile differs most.
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Head", "remove": ["nose"] }));
    settle(app, ctx);
    let noseless = ok(app, ctx, tx, "compare_reference", json!({ "views": ["front", "right", "back"] }));
    println!("without the nose: {noseless}");
    assert_eq!(noseless["worst_view"], "right", "{noseless}");
    let report = ok(app, ctx, tx, "quality_report", json!({ "target": "Head", "samples": 2000 }));
    assert_eq!(report["silhouette"]["worst_view"], "right", "{report}");
    let failures = report["failures"].to_string();
    assert!(failures.contains("right") && !failures.contains("front") && !failures.contains("back"), "{failures}");
    // Old calls: a path alone fills front; side is right; clear takes a slot.
    let r = ok(app, ctx, tx, "set_reference", json!({ "clear": true, "view": "back" }));
    assert_eq!(r["slots"], json!(["front", "right"]), "{r}");
    let r = ok(app, ctx, tx, "set_reference", json!({ "path": dir.join("right.png"), "view": "side" }));
    assert_eq!((r["view"].clone(), r["overlay_view"].clone()), (json!("right"), json!("side")), "{r}");
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    ok(app, ctx, tx, "set_reference", json!({ "path": dir.join("front.png") }));
    let one = ok(app, ctx, tx, "compare_reference", json!({ "view": "three_quarter" }));
    assert_eq!((one["slot"].clone(), one["view"].clone()), (json!("front"), json!("three_quarter")), "the only image compares from any view: {one}");
    let report = ok(app, ctx, tx, "quality_report", json!({ "target": "Head", "samples": 2000, "views": ["front"] }));
    assert!(report["silhouette"]["views"][0]["iou"].as_f64().unwrap() > 0.97, "{report}");
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    let report = ok(app, ctx, tx, "quality_report", json!({ "target": "Head", "samples": 2000 }));
    assert!(report["silhouette"]["skipped"].is_string(), "{report}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The face of `closeup_features_match`: a ball (radius 0.5) with two eye
/// rings and a mouth arc as crease grooves (2 cells deep), the mouth
/// lowered by `mouth_drop` (world units).
fn face(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, mouth_drop: f32) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Face", "resolution": 128, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Face", "detail_cell": FACE_CELL }));
    let on = |x: f32, y: f32| [x, y, (0.25 - x * x - y * y).sqrt()];
    let mut strokes = Vec::new();
    for ex in [-EYE.0, EYE.0] {
        let ring: Vec<[f32; 3]> = (0..=24).map(|i| { let a = std::f32::consts::TAU * i as f32 / 24.0; on(ex + EYE.2 * a.cos(), EYE.1 + EYE.2 * a.sin()) }).collect();
        strokes.push(json!({ "brush": "crease", "points": ring, "radius": FACE_CREASE * FACE_CELL, "strength": 2.0 * FACE_CELL, "normal": on(ex, EYE.1) }));
    }
    let mouth: Vec<[f32; 3]> = (0..=16).map(|i| { let x = -0.2 + 0.4 * i as f32 / 16.0; on(x, mouth_y(x) - mouth_drop) }).collect();
    strokes.push(json!({ "brush": "crease", "points": mouth, "radius": FACE_CREASE * FACE_CELL, "strength": 2.0 * FACE_CELL, "normal": on(0.0, mouth_y(0.0) - mouth_drop) }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Face", "strokes": strokes }));
    settle(app, ctx);
}

const FACE_CELL: f32 = 0.004;
/// The grooves' crease radius, and the drawn lines' width, in cells (a
/// drawn line about as wide as the groove's concave core).
const FACE_CREASE: f32 = 3.5;
const FACE_LINE: f32 = 1.0;
/// The eye rings: (x of each, y, ring radius).
const EYE: (f32, f32, f32) = (0.17, 0.12, 0.07);

fn mouth_y(x: f32) -> f32 {
    -0.22 + 1.5 * x * x
}

/// The face drawn: a skin disc on white with the eye rings and the mouth
/// arc as dark lines `line` world units wide (`px` pixels per unit),
/// where the grooves' bottoms show from the front (2 cells in from the
/// sphere, so 1 - 2 cell / 0.5 of the way out).
fn face_drawing(path: &std::path::Path, line: f32) {
    let (n, px) = (1024u32, 800.0f32);
    let c = n as f32 * 0.5;
    let inward = 1.0 - 2.0 * FACE_CELL / 0.5;
    let img = image::RgbaImage::from_fn(n, n, |i, j| {
        let (x, y) = ((i as f32 + 0.5 - c) / px, (c - j as f32 - 0.5) / px);
        if x * x + y * y > 0.25 {
            return image::Rgba([255, 255, 255, 255]);
        }
        let (x, y) = (x / inward, y / inward);
        let eye = [-EYE.0, EYE.0].iter().map(|ex| ((x - ex).powi(2) + (y - EYE.1).powi(2)).sqrt() - EYE.2).map(f32::abs).fold(f32::MAX, f32::min);
        // The mouth arc, by distance to the curve (sampled).
        let mouth = (0..=400).map(|k| { let mx = -0.2 + 0.4 * k as f32 / 400.0; ((x - mx).powi(2) + (y - mouth_y(mx)).powi(2)).sqrt() }).fold(f32::MAX, f32::min);
        if eye.min(mouth) * inward <= line * 0.5 { image::Rgba([60, 35, 30, 255]) } else { image::Rgba([225, 180, 150, 255]) }
    });
    img.save(path).unwrap();
}

/// Close-up features: a drawn face's lines against the clay's grooves
/// (drawn where the grooves show from the front). The matching mouth's
/// lines lie within 1% of the crop's radius; a mouth lowered by 4% of it
/// is at least 3% off (each line is two edges, a cell apart, so a shift
/// reads about a cell short).
#[test]
fn closeup_features_match() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let dir = std::env::temp_dir().join(format!("clide3d_features_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("face.png");
    face_drawing(&path, FACE_LINE * FACE_CELL);
    face(app, ctx, tx, 0.0);
    ok(app, ctx, tx, "set_reference", json!({ "path": path }));
    // The lower face: the mouth and nothing else.
    let radius = 0.2;
    let mouth = [0.0, -0.19, (0.25f32 - 0.19 * 0.19).sqrt()];
    let call = json!({ "view": "front", "center": mouth, "radius": radius });
    let matching = ok(app, ctx, tx, "compare_reference", call.clone());
    println!("matching: {}", matching["features"]);
    let eyes = ok(app, ctx, tx, "compare_reference", json!({ "view": "front", "center": [EYE.0, EYE.1, 0.45], "radius": 0.1 }));
    println!("eye: {}", eyes["features"]);
    let report = |app: &mut ClideApp| ok(app, ctx, tx, "quality_report", json!({ "target": "Face", "samples": 2000, "closeups": [{ "name": "mouth", "center": mouth, "radius": radius }] }));
    let good = report(app);
    assert_eq!(good["closeups"][0]["feature_pct"], matching["features"]["chamfer_pct"], "{good}");
    assert!(!good["failures"].to_string().contains("features"), "{good}");
    ok(app, ctx, tx, "delete_object", json!({ "target": "Face" }));
    face(app, ctx, tx, 0.04 * radius);
    let shifted = ok(app, ctx, tx, "compare_reference", call);
    let bad = report(app);
    assert!(bad["failures"].to_string().contains("features mouth"), "{bad}");
    println!("mouth lowered 4% of the radius: {}", shifted["features"]);
    let pct = |v: &Value| v["features"]["chamfer_pct"].as_f64().unwrap();
    assert!(pct(&matching) <= 1.0, "{}", matching["features"]);
    assert!(pct(&shifted) >= 3.0, "{}", shifted["features"]);
    let recall = |v: &Value| v["features"]["recall"].as_f64().unwrap();
    assert!(recall(&matching) > 2.0 * recall(&shifted), "matched lines are recalled: {} against {}", matching["features"], shifted["features"]);
    // Features are for close-ups.
    let (e, err) = mcp(app, ctx, tx, "compare_reference", json!({ "view": "front", "features": true }));
    assert!(err && e.to_string().contains("close-up"), "{e}");
    // A smooth ball has no lines where the drawing has a mouth: recall 0,
    // and the report fails it (it passed with feature_pct null before).
    ok(app, ctx, tx, "delete_object", json!({ "target": "Face" }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Face", "resolution": 128, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Face", "detail_cell": FACE_CELL }));
    settle(app, ctx);
    let smooth = report(app);
    println!("smooth ball against the drawn mouth: {}", smooth["closeups"]);
    assert_eq!(smooth["closeups"][0]["recall"], 0.0, "{smooth}");
    assert_eq!(smooth["pass"], false, "{smooth}");
    assert!(smooth["failures"].to_string().contains("features mouth: no model lines"), "{smooth}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 2's exit criterion on a synthetic figurine (the sculpt timing
/// bench's blob figure, with front and right references rendered from
/// it): the clean figure passes, and once defects are injected (lumps on
/// the chest, a legacy 16-gon sweep strap across the back as old scenes
/// keep, a thin wall at the back of the head, forearms missing from the
/// silhouette) the report names each in `failures` and `worst` /
/// `thin_at` (and the strap in its hints), and fails no clean part.
#[test]
fn figurine_report_names_injected_defects() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let figure = crate::harness::figure(96, false);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Figure", "resolution": 96, "blobs": figure.blobs }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Figure", "detail_cell": 0.006 }));
    settle(app, ctx);
    let dir = std::env::temp_dir().join(format!("clide3d_figurine_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for view in ["front", "right"] {
        let path = mask_reference(app, view, &dir);
        ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": view }));
    }
    let regions = json!([
        { "name": "chest", "center": [0, 1.3, 0.17], "radius": 0.12 },
        { "name": "back", "center": [0, 1.3, -0.16], "radius": 0.14 },
        { "name": "head", "center": [0, 1.72, 0], "radius": 0.16 },
        { "name": "hips", "center": [0, 1.0, 0.0], "radius": 0.2 },
        { "name": "legs", "center": [0.14, 0.45, 0.0], "radius": 0.25 },
    ]);
    let call = json!({ "target": "Figure", "samples": 20_000, "regions": regions, "thresholds": { "min_wall": 0.004 } });
    let clean = ok(app, ctx, tx, "quality_report", call.clone());
    println!("clean: failures {} hints {} lumpiness {} silhouette {} min_wall {}", clean["failures"], clean["hints"], clean["lumpiness"]["regions"], clean["silhouette"]["views"], clean["topology"]["min_wall"]);
    assert_eq!(clean["pass"], true, "{clean}");
    // The defects.
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 40) as f32 / (1u64 << 24) as f32
    };
    // On the torso (an ellipsoid at [0, 1.2, 0], radii [0.25, 0.35, 0.18]).
    let lumps: Vec<Value> = (0..30)
        .map(|_| {
            let (x, y) = ((next() - 0.5) * 0.16, 1.24 + next() * 0.12);
            let z = 0.18 * (1.0 - (x / 0.25).powi(2) - ((y - 1.2) / 0.35).powi(2)).sqrt();
            json!({ "brush": "inflate", "points": [[x, y, z]], "radius": 0.02, "strength": 0.6 })
        })
        .collect();
    ok(app, ctx, tx, "sculpt", json!({ "target": "Figure", "strokes": lumps }));
    let ngon: Vec<[f32; 2]> = (0..16).map(|i| { let a = std::f32::consts::TAU * i as f32 / 16.0; [0.012 * a.cos(), 0.012 * a.sin()] }).collect();
    let strap = json!({ "type": "sweep", "name": "strap", "profile": ngon, "points": [[-0.2, 1.42, -0.12], [0.0, 1.3, -0.19], [0.2, 1.1, -0.13]], "field": "legacy", "blend": 0.01 });
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Figure", "remove": ["fore"], "add": [strap, { "type": "sphere", "name": "hollow", "op": "carve", "center": [0, 1.72, -0.012], "radius": 0.115, "blend": 0 }] }));
    settle(app, ctx);
    let r = ok(app, ctx, tx, "quality_report", call);
    println!("with defects: {r}");
    let failures = r["failures"].to_string();
    assert!(failures.contains("lumpiness chest"), "lumps: {failures}");
    assert!(r["lumpiness"]["worst"].as_array().unwrap().iter().any(|w| (w["at"][1].as_f64().unwrap() - 1.3).abs() < 0.12 && w["at"][2].as_f64().unwrap() > 0.1), "a worst point on the chest: {}", r["lumpiness"]["worst"]);
    assert!(failures.contains("lumpiness back"), "the legacy strap: {failures}");
    assert!(r["lumpiness"]["worst"].as_array().unwrap().iter().any(|w| w["at"][2].as_f64().unwrap() < -0.1 && (w["at"][1].as_f64().unwrap() - 1.26).abs() < 0.2), "a worst point on the strap: {}", r["lumpiness"]["worst"]);
    assert!(failures.contains("min_wall"), "thin wall: {failures}");
    assert!(r["topology"]["thin_at"].as_array().unwrap().iter().any(|p| (p[1].as_f64().unwrap() - 1.72).abs() < 0.13 && p[2].as_f64().unwrap() < 0.0), "thin at the back of the head: {}", r["topology"]["thin_at"]);
    assert!(failures.contains("iou front") || failures.contains("contour front"), "missing forearms: {failures}");
    assert!(failures.contains("contour_p95 front"), "missing forearms, by the p95: {failures}");
    assert!(r["hints"].to_string().contains("on legacy sweep blob 9 (strap)") && r["legacy_sweeps"][0]["name"] == "strap", "{}", r["hints"]);
    for clean_part in ["lumpiness head", "lumpiness hips", "lumpiness legs"] {
        assert!(!failures.contains(clean_part), "false failure {clean_part}: {failures}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// §6's target: `quality_report` with 40,000 samples, 3 views and 2
/// close-ups on a 2M-triangle clay in at most 6 s (12 s hard; perf:
/// `--ignored`, CLIDE_PERF=1 holds the bounds).
#[test]
#[ignore]
fn quality_report_perf() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    // 64 balls in a 4 x 4 x 4 lattice at the largest resolution: about
    // 3.9M triangles.
    let balls: Vec<Value> = (0..64).map(|i| { let c = |k: usize| -0.375 + 0.25 * k as f32; json!({ "type": "sphere", "center": [c(i % 4), c(i / 4 % 4), c(i / 16)], "radius": 0.07, "blend": 0 }) }).collect();
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Figure", "resolution": 512, "blobs": balls }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Figure", "detail_cell": 0.002 }));
    settle(app, ctx);
    let dir = std::env::temp_dir().join(format!("clide3d_quality_perf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for view in ["front", "right", "back"] {
        let path = mask_reference(app, view, &dir);
        ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": view }));
    }
    let call = json!({ "target": "Figure", "closeups": [{ "name": "a", "center": [0.125, 0.125, 0.445], "radius": 0.05 }, { "name": "b", "center": [-0.375, -0.125, 0.445], "radius": 0.05 }] });
    let start = std::time::Instant::now();
    let r = ok(app, ctx, tx, "quality_report", call.clone());
    let total = start.elapsed().as_secs_f64();
    let triangles = r["topology"]["triangles"].as_u64().unwrap();
    // The field part alone (no references: no renders).
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    let start = std::time::Instant::now();
    ok(app, ctx, tx, "quality_report", json!({ "target": "Figure" }));
    let field = start.elapsed().as_secs_f64();
    println!("quality_report on {triangles} triangles, 40,000 samples, 3 views, 2 close-ups: {total:.2} s (field part alone {field:.2} s); pass {}", r["pass"]);
    assert!(triangles >= 1_500_000, "{triangles} triangles");
    if std::env::var("CLIDE_PERF").is_ok() {
        assert!(total <= 6.0 && field <= 3.0, "{total} s, field {field} s");
    } else {
        assert!(total <= 18.0, "{total} s (3x slack)");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review fixes: the sheets color the mesh the viewport draws (a fine
/// mesh too), and a job draws them at rest from what it measured; a
/// posed rig compares as at rest; walls under a non-uniform scale are
/// measured along their normal; and a report that measured nothing does
/// not pass.
#[test]
fn quality_report_sheets_poses_scales_and_unmeasured() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    head(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Head", "detail_cell": 0.008 }));
    settle(app, ctx);
    let sheet = |content: &[Value]| {
        let img = image_of(content);
        let (w, h) = img.dimensions();
        (image::imageops::crop_imm(&img, 0, 0, w, h / 2).to_image(), image::imageops::crop_imm(&img, 0, h / 2, w, h / 2).to_image())
    };
    let images = json!({ "target": "Head", "samples": 2000, "images": true });
    // With the fine mesh shown, the curvature sheet colors its vertices
    // (it colored the regular mesh's onto the fine one's before).
    let regular = sheet(&content_of(app, ctx, tx, "quality_report", images.clone()));
    ok(app, ctx, tx, "set_display", json!({ "fine": true }));
    settle(app, ctx);
    let fine = sheet(&content_of(app, ctx, tx, "quality_report", images.clone()));
    let d = image_diff(&regular.0, &fine.0);
    println!("curvature sheet, regular against fine mesh: {d:.3}");
    assert!(d < 0.5, "the fine mesh's sheet is colored as the regular one's: {d}");
    ok(app, ctx, tx, "set_display", json!({ "fine": false }));
    settle(app, ctx);

    // Posed mid-clip: compare_reference and a job's sheets are at rest.
    let dir = std::env::temp_dir().join(format!("clide3d_posed_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = mask_reference(app, "front", &dir);
    ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": "front" }));
    ok(app, ctx, tx, "rig", json!({ "target": "Head", "add": [{ "name": "root", "head": [0, -0.5, 0], "tail": [0, 0.5, 0] }] }));
    ok(app, ctx, tx, "animate", json!({ "target": "Head", "keys": [{ "bone": "root", "frame": 0, "rotate": [0, 0, 0] }, { "bone": "root", "frame": 10, "rotate": [0, 0, 60] }] }));
    settle(app, ctx);
    let at_rest = ok(app, ctx, tx, "compare_reference", json!({ "view": "front", "center": [0, -0.02, 0.4], "radius": 0.12 }));
    let job = json!({ "target": "Head", "samples": 45_000, "images": true });
    let rest_sheet = {
        let super::jobs::Started::Now(Ok(content)) = app.at_rest(|a| a.quality_report(&job, false)).unwrap() else { panic!("no reply") };
        sheet(&content)
    };
    app.timeline.frame = 5.0;
    frame(app, ctx, Vec::new());
    let posed = ok(app, ctx, tx, "compare_reference", json!({ "view": "front", "center": [0, -0.02, 0.4], "radius": 0.12 }));
    assert_eq!(posed["overlap_iou"], at_rest["overlap_iou"], "posed: {posed}");
    assert_eq!(posed["features"], at_rest["features"], "posed: {posed}");
    let posed_sheet = sheet(&content_of(app, ctx, tx, "quality_report", job));
    let d = image_diff(&posed_sheet.0, &rest_sheet.0);
    println!("job sheet while posed against at rest: {d:.3}");
    assert_eq!(d, 0.0, "a job's sheets are drawn at rest");
    app.timeline.frame = 0.0;
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    let _ = std::fs::remove_dir_all(&dir);

    // A shell 3 cm thick (in the clay) squashed to a quarter along z: its
    // poles' walls are 7.5 mm, under the 1 cm threshold (the largest
    // scale made them 6 cm before).
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Shell", "resolution": 96, "scale": [2, 2, 0.25], "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3, "blend": 0 }, { "type": "sphere", "op": "carve", "center": [0, 0, 0], "radius": 0.27, "blend": 0 }] }));
    settle(app, ctx);
    let r = ok(app, ctx, tx, "quality_report", json!({ "target": "Shell", "samples": 4000, "thresholds": { "min_wall": 0.01 } }));
    println!("squashed shell: min_wall {} thin_at {}", r["topology"]["min_wall"], r["topology"]["thin_at"]);
    let w = r["topology"]["min_wall"].as_f64().unwrap();
    assert!((0.006..0.009).contains(&w), "{}", r["topology"]);
    assert!(r["failures"].to_string().contains("min_wall"), "{}", r["failures"]);
    assert!(r["topology"]["thin_at"].as_array().unwrap().iter().all(|p| p[2].as_f64().unwrap().abs() > 0.05), "thin at the poles: {}", r["topology"]["thin_at"]);

    // Nothing counted: every point in an excluded region.
    let r = ok(app, ctx, tx, "quality_report", json!({ "target": "Head", "samples": 2000, "regions": [{ "name": "all", "center": [0, 0, 0], "radius": 5, "exclude": true }, { "name": "far", "center": [9, 9, 9], "radius": 0.1 }] }));
    assert_eq!(r["pass"], false, "{r}");
    let failures = r["failures"].to_string();
    assert!(failures.contains("lumpiness unmeasured") && failures.contains("lumpiness far unmeasured"), "{failures}");
}

/// D-c: a thin part missing from the model (an antenna out of the head's
/// shoulder, inside its box so the lineup holds) barely moves the IoU or the contour's mean, but its p95 passes
/// `contour_p95_pct` and the report fails it; with the antenna it passes.
#[test]
fn contour_p95_catches_a_thin_antenna() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    head(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Head", "resolution": 160, "add": [{ "type": "capsule", "name": "antenna", "a": [0.12, 0.28, 0.1], "b": [0.27, 0.38, 0.1], "radius": 0.01, "blend": 0.01 }] }));
    settle(app, ctx);
    let dir = std::env::temp_dir().join(format!("clide3d_antenna_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = mask_reference(app, "front", &dir);
    ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": "front" }));
    let call = json!({ "target": "Head", "samples": 2000 });
    let with = ok(app, ctx, tx, "quality_report", call.clone());
    println!("with the antenna: {}", with["silhouette"]);
    assert!(!with["failures"].to_string().contains("contour"), "{}", with["failures"]);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Head", "remove": ["antenna"] }));
    settle(app, ctx);
    let without = ok(app, ctx, tx, "quality_report", call);
    println!("without: {} failures {}", without["silhouette"], without["failures"]);
    let v = &without["silhouette"]["views"][0];
    assert!(v["iou"].as_f64().unwrap() >= 0.95 && v["contour_pct"].as_f64().unwrap() <= 1.0, "IoU and the mean pass: {v}");
    let failures = without["failures"].to_string();
    assert!(failures.contains("contour_p95 front") && !failures.contains("iou front"), "{failures}");
    let (e, err) = mcp(app, ctx, tx, "quality_report", json!({ "target": "Head", "thresholds": { "contour_p95_pct": -1 } }));
    assert!(err && e.to_string().contains("contour_p95_pct"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}
