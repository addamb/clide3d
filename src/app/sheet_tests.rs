//! Stage 5a through MCP, on a headless editor (skips without a GPU
//! adapter): a synthetic turnaround sheet (a box-and-ball character drawn
//! front, right and back, with captions) split into per-view slots at one
//! scale, its image planes, compares and quality reports against them,
//! proportion and size errors the shared scale shows, exports and film
//! leaving the planes out, save and load, undo, and refusals.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::Slot;
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
    let d = std::env::temp_dir().join(format!("clide3d_sheet_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The sheet (400 pixels a unit, views 1.25 units apart, captions) saved
/// as a PNG, with each view's true subject box.
fn sheet_png(dir: &std::path::Path, depth: f32) -> (std::path::PathBuf, Vec<[usize; 4]>) {
    let (img, boxes) = synthetic::sheet(400.0, 1.25, depth, true);
    let path = dir.join("sheet.png");
    img.save(&path).unwrap();
    (path, boxes)
}

/// The character the sheet draws, as clay: a box body and a ball head.
fn character(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": name, "resolution": 96, "blobs": [
        { "type": "box", "name": "body", "center": [0, 0.3, 0], "size": [synthetic::WIDTH, 0.6, synthetic::DEPTH], "blend": 0 },
        { "type": "sphere", "name": "head", "center": [0, 0.75, 0], "radius": 0.18, "blend": 0 },
    ] }));
    settle(app, ctx);
}

fn planes(app: &ClideApp) -> Vec<(String, crate::refplane::RefPlane)> {
    app.scene.objects.iter().filter_map(|o| o.reference.clone().map(|r| (o.name.clone(), r))).collect()
}

fn ious(r: &Value) -> Vec<(String, f64, String)> {
    r["views"].as_array().unwrap().iter().map(|v| (v["slot"].as_str().unwrap().to_string(), v["overlap_iou"].as_f64().unwrap(), v["aligned_by"].as_str().unwrap_or_default().to_string())).collect()
}

/// The sheet splits into its three views at one scale: the slots fill,
/// each view's box is found within 2 sheet pixels (captions ignored),
/// pixels per unit are equal, every plane shows its subject the same
/// height in world units to within a pixel, and the model built to match
/// passes compare_reference and quality_report's silhouettes in every
/// view, lined up by the sheet. One undo step takes the planes back.
#[test]
fn sheet_fills_slots_at_a_shared_scale() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("fill");
    character(app, ctx, tx, "Buddy");
    let (path, truth) = sheet_png(&d, synthetic::DEPTH);
    let step = app.history.current();
    let r = ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    eprintln!("sheet reply: {r}");
    assert_eq!(r["slots"], json!(["front", "back", "right"]));
    assert_eq!(r["found_by"], "automatic");
    let views = r["views"].as_array().unwrap();
    assert_eq!(views.iter().map(|v| v["view"].as_str().unwrap()).collect::<Vec<_>>(), ["front", "right", "back"]);
    for (v, t) in views.iter().zip(&truth) {
        let b: Vec<f64> = v["subject_box_px"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        for c in 0..4 {
            assert!((b[c] - t[c] as f64).abs() <= 2.0, "{v} against {t:?}");
        }
        assert_eq!(v["px_per_unit"], views[0]["px_per_unit"]);
    }
    // The model's height is the sheet's subject: 400 pixels a unit.
    let ppu = r["px_per_unit"].as_f64().unwrap();
    assert!((ppu - 400.0).abs() < 400.0 * 0.02, "{ppu}");
    let ps = planes(app);
    assert_eq!(ps.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["Reference front", "Reference right", "Reference back"]);
    let heights: Vec<f32> = ps.iter().map(|(_, p)| {
        let r = super::sheet::reference_of(p).unwrap();
        let b = r.bbox();
        (b[3] + 1 - b[1]) as f32 / p.px_per_unit
    }).collect();
    let px = 1.0 / ps[0].1.px_per_unit;
    eprintln!("plane subject heights {heights:?} (a pixel is {px:.5})");
    assert!(ps.iter().all(|(_, p)| p.px_per_unit == ps[0].1.px_per_unit));
    assert!(heights.iter().all(|h| (h - heights[0]).abs() <= px), "{heights:?}");
    // The planes stand behind the model, facing their views' cameras.
    for (name, slot, axis) in [("Reference front", "front", glam::Vec3::Z), ("Reference right", "right", glam::Vec3::X), ("Reference back", "back", glam::Vec3::NEG_Z)] {
        let o = app.scene.objects.iter().find(|o| o.name == name).unwrap();
        assert_eq!(o.reference.as_ref().unwrap().slot, slot);
        let facing = o.rotation * glam::Vec3::Z;
        assert!(facing.distance(axis) < 1e-5, "{name} faces {facing}");
        assert!(o.translation.dot(axis) < -0.3, "{name} stands behind the model: {}", o.translation);
    }
    // The matching model passes in every view, lined up by the sheet
    // (the planes are not part of any silhouette).
    let all = ok(app, ctx, tx, "compare_reference", json!({}));
    eprintln!("matching model: {:?}", ious(&all));
    for (slot, iou, by) in ious(&all) {
        assert!(iou >= 0.95, "{slot}: {iou}");
        assert_eq!(by, "sheet");
    }
    let report = ok(app, ctx, tx, "quality_report", json!({ "target": "Buddy", "samples": 2000 }));
    eprintln!("quality_report silhouettes: {}", report["silhouette"]);
    assert_eq!(report["silhouette"]["views"].as_array().unwrap().len(), 3);
    let failures = report["failures"].to_string();
    assert!(!failures.contains("iou") && !failures.contains("contour"), "{failures}");
    // One undo step takes the planes back.
    assert_eq!(app.history.current(), step + 1);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(planes(app).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// `shared_scale_reveals_proportion`: a model 10% too big all over passes
/// with each view fitted on its own (the old per-view fit absorbs the
/// size) and fails against the sheet at the subject's stated height; a
/// model 10% too deep fails the sheet's side view only.
#[test]
fn shared_scale_reveals_size_and_proportion() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("scale");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": synthetic::HEIGHT }, "planes": false }));
    assert!(planes(app).is_empty(), "planes: false places none");
    let fitted = ious(&ok(app, ctx, tx, "compare_reference", json!({})));
    // The same views as single images: each fitted on its own.
    let slots: Vec<(String, crate::refplane::RefPlane)> = {
        ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
        ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": synthetic::HEIGHT } }));
        planes(app).into_iter().map(|(_, p)| (p.slot.clone(), p)).collect()
    };
    ok(app, ctx, tx, "update_object", json!({ "target": "Buddy", "scale": [1.1, 1.1, 1.1] }));
    settle(app, ctx);
    let big_sheet = ious(&ok(app, ctx, tx, "compare_reference", json!({})));
    let report = ok(app, ctx, tx, "quality_report", json!({ "target": "Buddy", "samples": 2000 }));
    for (slot, p) in &slots {
        let file = d.join(format!("{slot}.png"));
        let [w, h] = p.image.size();
        image::RgbaImage::from_raw(w as u32, h as u32, p.image.rgba().to_vec()).unwrap().save(&file).unwrap();
        ok(app, ctx, tx, "set_reference", json!({ "path": file, "view": slot }));
    }
    let big_single = ious(&ok(app, ctx, tx, "compare_reference", json!({})));
    eprintln!("matching, sheet: {fitted:?}\n10% bigger, sheet: {big_sheet:?}\n10% bigger, each view on its own: {big_single:?}");
    assert!(fitted.iter().all(|v| v.1 >= 0.95));
    assert!(big_sheet.iter().all(|v| v.1 < 0.95 && v.2 == "sheet"), "the sheet shows the size");
    assert!(big_single.iter().all(|v| v.1 >= 0.95 && v.2 == "height"), "a per-view fit hides it");
    assert!(report["failures"].to_string().contains("iou"), "{}", report["failures"]);
    // 10% too deep: the side view fails, the front and back do not.
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": synthetic::HEIGHT } }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Buddy", "scale": [1.0, 1.0, 1.1] }));
    settle(app, ctx);
    let deep = ious(&ok(app, ctx, tx, "compare_reference", json!({})));
    eprintln!("10% deeper, sheet: {deep:?}");
    for (slot, iou, _) in &deep {
        assert_eq!(*iou < 0.95, slot == "right", "{slot}: {iou}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Reference planes are not geometry: a .glb and a print export are the
/// same with and without them, film leaves them out unless asked, while
/// render_views shows them (and leaves them out with include_reference
/// false).
#[test]
fn reference_planes_are_not_geometry() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("geometry");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    let glb = |app: &mut ClideApp, name: &str| {
        let p = d.join(name);
        ok(app, ctx, tx, "export_glb", json!({ "path": p }));
        std::fs::read(&p).unwrap()
    };
    let print = |app: &mut ClideApp, name: &str| ok(app, ctx, tx, "export_print", json!({ "path": d.join(name) }))["triangles"].clone();
    let before = (glb(app, "a.glb"), print(app, "a.stl"));
    let film_items = |app: &mut ClideApp, show: bool| ok(app, ctx, tx, "render_film", json!({ "size": 64, "samples": 1, "denoise": false, "views": ["front"], "show_references": show }))["triangles"].clone();
    let film_before = film_items(app, false);
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    assert_eq!(planes(app).len(), 3);
    let after = (glb(app, "b.glb"), print(app, "b.stl"));
    assert!(after.0 == before.0, "the .glb is the same bytes with the planes in the scene");
    assert_eq!(after.1, before.1, "print triangles");
    let film_without = film_items(app, false);
    let film_with = film_items(app, true);
    eprintln!("film triangles: before {film_before}, with planes {film_without}, show_references {film_with}");
    assert_eq!(film_without, film_before);
    if film_with.is_number() {
        assert_ne!(film_with, film_before, "show_references renders them");
    }
    // render_views draws them (include_reference, default true); masks
    // and `only` renders never do.
    let render = |app: &ClideApp, references: bool, mask: bool| {
        let (center, radius) = app.scene_sphere();
        let shot = super::eyes::Shot::framing(&json!("front"), center, radius * 2.0).unwrap();
        app.render_shot_extras(&shot, 128, 128, mask, false, super::eyes::Extras { references, ..Default::default() }).unwrap()
    };
    let differ = |a: &egui::ColorImage, b: &egui::ColorImage| a.pixels.iter().zip(&b.pixels).filter(|(x, y)| x != y).count();
    let (shown, hidden) = (render(app, true, false), render(app, false, false));
    eprintln!("render_views front: {} pixels differ with the planes", differ(&shown, &hidden));
    assert!(differ(&shown, &hidden) > 500);
    assert_eq!(differ(&render(app, true, true), &render(app, false, true)), 0, "masks never draw them");
    let (v, err) = mcp(app, ctx, tx, "render_views", json!({ "views": ["front"], "size": 128 }));
    assert!(!err, "{v}");
    // A material on a plane does not turn it into a plain primitive: it
    // still draws its image.
    ok(app, ctx, tx, "set_material", json!({ "name": "shiny", "roughness": 0.2, "apply_to": "Reference front" }));
    settle(app, ctx);
    let plane = app.scene.objects.iter().find(|o| o.name == "Reference front").unwrap();
    let mesh = app.meshes.get(plane).unwrap();
    assert!(mesh.textures.is_some() && mesh.vertices.len() == 4, "the plane keeps its image");
    // Picking passes through planes: a click on the front plane beside
    // the model hits nothing.
    let eval = app.scene.evaluate();
    let shot = super::eyes::Shot::framing(&json!("front"), glam::Vec3::new(0.0, 0.45, 0.0), 0.6).unwrap();
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 200.0));
    let vp = shot.view_proj(200, 200);
    let beside = vp.project_point3(glam::Vec3::new(0.22, 0.85, -0.5));
    let pos = egui::pos2((beside.x * 0.5 + 0.5) * 200.0, (0.5 - beside.y * 0.5) * 200.0);
    assert_eq!(app.pick_hit(pos, rect, vp, &eval), None, "the click passes through the plane");
    let _ = std::fs::remove_dir_all(&d);
}

/// Save and open bring the sheet's slots back from its planes (the same
/// compare, bit for bit); a new scene drops them; damaged planes in a file
/// load as plain planes.
#[test]
fn sheet_round_trips_through_save_and_load() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("save");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path }, "plane_opacity": 0.4 }));
    let compare = ok(app, ctx, tx, "compare_reference", json!({}));
    let saved_planes = planes(app);
    let file = d.join("scene.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": file.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    assert!(app.references.is_empty(), "a new scene has no sheet slots");
    ok(app, ctx, tx, "open_scene", json!({ "path": file.display().to_string() }));
    settle(app, ctx);
    assert_eq!(app.references.loaded().iter().map(|s| s.name()).collect::<Vec<_>>(), ["front", "back", "right"]);
    assert!(planes(app) == saved_planes, "the planes come back equal");
    assert_eq!(planes(app)[0].1.opacity, 0.4);
    let again = ok(app, ctx, tx, "compare_reference", json!({}));
    assert_eq!(again["views"], compare["views"], "the same compare after open");
    // A damaged plane (a scale of 0) loads as a plain plane.
    let mut text: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for o in text["objects"].as_array_mut().unwrap() {
        if o.get("reference").is_some() {
            o["reference"]["px_per_unit"] = json!(0.0);
        }
    }
    let broken = d.join("broken.json");
    std::fs::write(&broken, text.to_string()).unwrap();
    ok(app, ctx, tx, "open_scene", json!({ "path": broken.display().to_string() }));
    assert!(planes(app).is_empty() && app.references.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// Refusals through MCP, each leaving the scene and the slots as they
/// were; explicit boxes split as the automatic split does.
#[test]
fn sheet_refusals_and_explicit_boxes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("refuse");
    let (path, truth) = sheet_png(&d, synthetic::DEPTH);
    let refused = |app: &mut ClideApp, args: Value, says: &str| {
        let before = app.scene.objects.len();
        let (v, err) = mcp(app, ctx, tx, "set_reference", args.clone());
        assert!(err && v.to_string().contains(says), "{args}: {v}");
        assert_eq!(app.scene.objects.len(), before);
        assert!(app.references.is_empty());
    };
    refused(app, json!({ "sheet": { "path": path } }), "pass sheet.height");
    character(app, ctx, tx, "Buddy");
    refused(app, json!({ "sheet": { "path": path, "views": ["front", "right"] } }), "splits into 3 views, not 2");
    refused(app, json!({ "sheet": { "path": path, "views": ["front", "top", "back"] } }), "not top");
    refused(app, json!({ "sheet": { "path": path }, "view": "front" }), "does not go with sheet");
    refused(app, json!({ "path": path, "planes": true }), "goes with sheet");
    refused(app, json!({ "sheet": { "path": path, "boxes": [[0, 0, 10, 10], [20, 0, 30, 10], [9000, 0, 9010, 10]] } }), "reaches past the sheet");
    refused(app, json!({ "sheet": { "path": path, "boxes": [[0, 0, 10, 10], [20, 0, 30, 10], [40, 0, 50, 10]] } }), "holds no subject pixels");
    let auto = ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    let boxes: Vec<[usize; 4]> = truth.iter().map(|b| [b[0] - 20, b[1] - 20, b[2] + 20, b[3] + 20]).collect();
    let given = ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "boxes": boxes } }));
    assert_eq!(given["found_by"], "boxes");
    assert_eq!(given["views"], auto["views"], "loose boxes find the same subjects");
    assert_eq!(planes(app).len(), 3, "a second sheet replaces the planes of its slots");
    let _ = std::fs::remove_dir_all(&d);
}


/// Review fix: rays, framing and placement pass through reference planes.
/// A `surface_point` from the front hits the model where it is and nothing
/// beside it (the back view's plane stands behind the model, facing the
/// front camera); a plane parented under the model does not grow its
/// framing bounds.
#[test]
fn rays_and_framing_pass_through_planes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("rays");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    let buddy = app.scene.objects.iter().find(|o| o.name == "Buddy").unwrap().id;
    let bounds = app.world_bounds(&[buddy]).unwrap();
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    let r = ok(app, ctx, tx, "surface_point", json!({ "view": "front", "at": [[0.0, 0.3], [0.32, 0.85]] }));
    let results = r["results"].as_array().unwrap();
    eprintln!("front probes: {results:?}");
    assert_eq!(results[0]["object"], "Buddy", "the model, not a plane");
    assert_eq!(results[1]["hit"], false, "beside the model the ray passes through the back plane");
    // A plane nested under the model is not framed with it.
    ok(app, ctx, tx, "update_object", json!({ "target": "Reference back", "parent": "Buddy" }));
    settle(app, ctx);
    assert_eq!(app.world_bounds(&[buddy]), Some(bounds), "framing leaves the nested plane out");
    let (c0, r0) = app.scene_sphere();
    assert!(r0 < 1.0, "the scene sphere ({c0}, {r0}) holds the model only");
    // Review fix: an object parented under a top-level plane is still
    // framed and counted (scene sphere, world bounds, the sheet's model).
    ok(app, ctx, tx, "create_object", json!({ "shape": "Cube", "name": "Far", "parent": "Reference front", "space": "world", "translate": [3.0, 0.5, 0.0], "scale": [0.2, 0.2, 0.2] }));
    settle(app, ctx);
    let far = app.scene.objects.iter().find(|o| o.name == "Far").unwrap().id;
    let front = app.scene.objects.iter().find(|o| o.name == "Reference front").unwrap().id;
    let (c1, r1) = app.scene_sphere();
    eprintln!("with a cube under the front plane: {c1} {r1}");
    assert!(c1.x + r1 > 3.0, "the cube under the plane is in the scene sphere");
    let fb = app.world_bounds(&[far]).unwrap();
    assert!((fb.0.x - 2.9).abs() < 0.02 && (fb.1.x - 3.1).abs() < 0.02, "the cube framed: {fb:?}");
    let pb = app.world_bounds(&[front]).unwrap();
    assert_eq!(pb, fb, "the plane itself is not framed, its child is");
    let roots = app.scene.children(None);
    let model = app.world_bounds_of_roots(&roots).unwrap();
    assert!(model.1.x > 3.0, "the sheet's model bounds hold the cube: {model:?}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Review fix: the sheet slots follow the planes through undo, redo,
/// history goto and deletion.
#[test]
fn slots_follow_planes_through_history() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("history");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    let slots = |app: &ClideApp| app.references.loaded().iter().map(|s| s.name()).collect::<Vec<_>>();
    let scale = |app: &ClideApp| app.references.get(app.references.loaded()[0]).and_then(|r| r.sheet.as_ref()).map(|f| f.px_per_unit);
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": 0.93 } }));
    let a = scale(app);
    assert_eq!(slots(app).len(), 3);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(slots(app).is_empty(), "undo takes the slots with the planes");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(slots(app).len(), 3, "redo brings them back");
    assert_eq!(scale(app), a);
    // A second sheet at another height, undone: the first one's slots.
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": 1.86 } }));
    assert_ne!(scale(app), a);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(scale(app), a, "the slots are the first sheet's again");
    // Clear, undone: back.
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    assert!(slots(app).is_empty());
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(slots(app).len(), 3, "undoing clear brings the slots back");
    // Goto the step before any sheet.
    let h = ok(app, ctx, tx, "history", json!({ "action": "list" }));
    eprintln!("history: {h}");
    let first = app.history.current() - 1;
    ok(app, ctx, tx, "history", json!({ "action": "goto", "index": first }));
    assert!(slots(app).is_empty(), "goto before the sheet: no slots");
    ok(app, ctx, tx, "history", json!({ "action": "goto", "index": first + 1 }));
    assert_eq!(slots(app).len(), 3);
    // Deleting a plane takes its slot; the others stay.
    ok(app, ctx, tx, "delete_object", json!({ "target": "Reference right" }));
    assert_eq!(slots(app), ["front", "back"]);
    // A slot cleared while only its plane is left still clears.
    app.references = Default::default();
    ok(app, ctx, tx, "set_reference", json!({ "clear": true, "view": "front" }));
    assert!(!app.scene.objects.iter().any(|o| o.reference.as_ref().is_some_and(|r| r.slot == "front")));
    let _ = std::fs::remove_dir_all(&d);
}

/// Stage 5b review fix: a slot a plane's view took over during undo comes
/// back on redo. A single image loaded into the front slot (its plane
/// goes), undone (the plane and its view come back) and redone: the front
/// slot holds the single image again, not nothing. The same for a sheet
/// loaded again without planes.
#[test]
fn redo_restores_slots_planes_took_over() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("redo_slots");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    let front = |app: &ClideApp| app.references.get(Slot::Front).map(|r| (r.sheet.is_some(), r.from_plane()));
    let slots = |app: &ClideApp| app.references.loaded().iter().map(|s| s.name()).collect::<Vec<_>>();
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": 0.93 } }));
    assert_eq!(front(app), Some((true, true)));
    ok(app, ctx, tx, "set_reference", json!({ "path": path, "view": "front" }));
    assert_eq!(front(app), Some((false, false)), "the single image");
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(front(app), Some((true, true)), "undo: the plane's view");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(front(app), Some((false, false)), "redo: the single image again");
    assert_eq!(slots(app), ["front", "back", "right"]);
    // A sheet loaded again without planes, undone and redone.
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": 0.93 } }));
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path, "height": 0.93 }, "planes": false }));
    let planeless = |app: &ClideApp| app.references.loaded().iter().all(|s| app.references.get(*s).is_some_and(|r| r.sheet.is_some() && !r.from_plane()));
    assert_eq!(slots(app).len(), 3);
    assert!(planeless(app));
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.references.loaded().iter().all(|s| app.references.get(*s).unwrap().from_plane()), "undo: the planes' views");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(slots(app).len(), 3, "redo: the planeless views, not nothing");
    assert!(planeless(app));
    // Clearing forgets them: an undo then redo leaves nothing behind.
    ok(app, ctx, tx, "set_reference", json!({ "clear": true }));
    assert!(slots(app).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// Review fixes: exporting only planes is refused (not the whole scene);
/// a sheet loaded again keeps objects parented under the old planes,
/// where they stood; big captions are left out of the views and reported.
#[test]
fn plane_exports_children_and_captions() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let d = dir("children");
    character(app, ctx, tx, "Buddy");
    let (path, _) = sheet_png(&d, synthetic::DEPTH);
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    let (v, err) = mcp(app, ctx, tx, "export_glb", json!({ "path": d.join("planes.glb"), "targets": ["Reference front", "Reference back"] }));
    assert!(err && v.to_string().contains("nothing to export"), "{v}");
    assert!(!d.join("planes.glb").exists());
    ok(app, ctx, tx, "create_object", json!({ "shape": "Cube", "name": "Pin", "parent": "Reference front", "translate": [0.1, 0.2, 0.3] }));
    settle(app, ctx);
    let pin = app.scene.objects.iter().find(|o| o.name == "Pin").unwrap().id;
    let world = app.scene.world_matrix(pin);
    // Exported with the planes left out, the pin stays (under no plane).
    ok(app, ctx, tx, "export_glb", json!({ "path": d.join("pin.glb"), "targets": ["Pin"] }));
    assert!(d.join("pin.glb").exists());
    ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    let o = app.scene.get(pin).expect("the pin survives a sheet loaded again");
    assert_eq!(o.parent, None);
    assert!(app.scene.world_matrix(pin).abs_diff_eq(world, 1e-5), "where it stood");
    // Big captions: the split leaves them out and says so.
    let (mut img, _) = synthetic::sheet(400.0, 1.25, synthetic::DEPTH, false);
    synthetic::big_captions(&mut img, 400.0, 1.25);
    let big = d.join("captions.png");
    img.save(&big).unwrap();
    let plain = ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": path } }));
    let r = ok(app, ctx, tx, "set_reference", json!({ "sheet": { "path": big } }));
    assert_eq!(r["captions_left_out"].as_array().unwrap().len(), 3, "{r}");
    assert!(r["warning"].as_str().unwrap().contains("caption"));
    assert_eq!(r["px_per_unit"], plain["px_per_unit"], "the scale is the subjects'");
    assert_eq!(r["views"].as_array().unwrap().iter().map(|v| v["subject_box_px"].clone()).collect::<Vec<_>>(), plain["views"].as_array().unwrap().iter().map(|v| v["subject_box_px"].clone()).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&d);
}
