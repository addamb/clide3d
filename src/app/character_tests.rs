//! Stage 5c through MCP, on a headless editor (skips without a GPU
//! adapter): `create_character` bases whose proportions measure back with
//! `guides measure`, that pass `quality_report` at the default thresholds,
//! build the same on 1 and 12 threads, take sculpt strokes, polish and
//! blob edits like any clay, save and load whole, undo in one step, and
//! refuse bad arguments with nothing changed.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, settle};
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

/// A tool call that must succeed; a long reply's compact wrapper
/// (`{summary, next}`) unwrapped.
fn ok(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, args: Value) -> Value {
    let v = super::recorder::tests::ok(app, ctx, tx, name, args);
    if v["summary"].is_object() && v["next"].is_string() { v["summary"].clone() } else { v }
}

fn by_name<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no {name} in {list}"))
}

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

/// `character_proportions` through MCP: 2.5, 3 and 4 heads tall with eye
/// sizes 1, 0.7 and 1.5 measure back with `guides measure`: the model is
/// as tall as asked (1e-3), the head ratio, eye size, eye spacing and eye
/// line within their 2% tolerances; the create reply's own measure
/// agrees.
#[test]
fn character_proportions_measure_back() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    for (n, eye, height) in [(2.5f64, 1.0f64, 1.0f64), (3.0, 0.7, 1.6), (4.0, 1.5, 2.0)] {
        ok(app, ctx, tx, "new_scene", json!({}));
        // The 3-head base stands somewhere else: the guides move with it.
        let at = if n == 3.0 { json!([0.5, 0.2, -0.3]) } else { json!([0, 0, 0]) };
        let r = ok(app, ctx, tx, "create_character", json!({ "name": "Buddy", "proportions": { "head_units": n, "eye_size": eye, "height": height }, "translate": at }));
        settle(app, ctx);
        assert_eq!(r["measured"]["pass"], true, "{}", r["measured"]);
        let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Buddy" }));
        assert_eq!(m["pass"], true, "{}", m["failures"]);
        assert_eq!(m["targets"], r["measured"]["targets"], "the create reply measured the same");
        let ts = &m["targets"];
        let got = |t: &str| by_name(ts, t)["measured"].as_f64().unwrap();
        let total = m["model"]["height"].as_f64().unwrap();
        let heads = m["head_units"]["model_heads"].as_f64().unwrap();
        let unit = height / n;
        eprintln!(
            "{n} heads, eye_size {eye}, {height} tall: height {total}, model heads {heads}, head_ratio {} (1/n {:.5}), eye_size {} (asked {:.5}), eye_spacing {} per head unit, eyes_on_line {}",
            got("head_ratio"), 1.0 / n, got("eye_size"), crate::character::EYE as f64 * eye, got("eye_spacing"), got("eyes_on_line")
        );
        assert!((total - height).abs() < 1e-3 * height, "{total}");
        assert!((heads - n).abs() < 0.02 * n, "{heads} heads");
        assert!((got("head_ratio") * n - 1.0).abs() < 0.02, "{}", got("head_ratio"));
        assert!((got("eye_size") / (crate::character::EYE as f64 * eye) - 1.0).abs() < 0.02, "{}", got("eye_size"));
        assert!(got("eyes_on_line").abs() < 0.01 * unit);
        assert_eq!(r["proportions"]["head_units"], json!(n));
    }
}

/// `character_is_clean`: the default base passes `quality_report` at the
/// default thresholds (lumpiness, walls, watertight), no blob is too thin
/// for its resolution, its rig has no warnings, and it retopologises into
/// a cage of almost all quads.
#[test]
fn character_is_clean() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let t = std::time::Instant::now();
    let r = ok(app, ctx, tx, "create_character", json!({}));
    settle(app, ctx);
    eprintln!("create_character ({:.1} s): {}", t.elapsed().as_secs_f64(), serde_json::to_string(&r).unwrap());
    assert!(r["clay"]["thin_blobs"].is_null(), "{}", r["clay"]["thin_blobs"]);
    assert_eq!(r["eyes"].as_array().unwrap().len(), 2);
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": "Character" }));
    eprintln!("quality_report: pass {}, failures {}, lumpiness {}, topology {}", q["pass"], q["failures"], q["lumpiness"]["overall"], q["topology"]);
    assert_eq!(q["pass"], true, "{}", q["failures"]);
    assert_eq!(q["topology"]["watertight"], true);
    // The plan's own bound for the base (D14's default is 0.05).
    assert!(q["lumpiness"]["overall"].as_f64().unwrap() <= 0.02, "{}", q["lumpiness"]["overall"]);
    assert_eq!(q["proportions"]["pass"], true);
    let rig = ok(app, ctx, tx, "rig", json!({ "target": "Character" }));
    eprintln!("rig: {rig}");
    assert!(rig["warnings"].as_array().is_none_or(|w| w.is_empty()), "{}", rig["warnings"]);
    ok(app, ctx, tx, "create_character", json!({ "name": "Pet", "build": "quadruped", "guides": false, "translate": [2, 0, 0] }));
    settle(app, ctx);
    let rig = ok(app, ctx, tx, "rig", json!({ "target": "Pet" }));
    eprintln!("quadruped rig: {rig}");
    assert!(rig["warnings"].as_array().is_none_or(|w| w.is_empty()), "{}", rig["warnings"]);
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": "Pet" }));
    eprintln!("quadruped quality_report: pass {}, failures {}, lumpiness {}, topology {}", q["pass"], q["failures"], q["lumpiness"]["overall"], q["topology"]);
    assert_eq!(q["pass"], true, "{}", q["failures"]);
    // Other options pass at the default thresholds too.
    for (k, p) in [json!({ "head_units": 4, "hands": "five_finger", "body": "slim", "ears": "pointed", "feet": "long" }), json!({ "head_units": 2, "hands": "mitten", "body": "pear", "limb_thickness": 1.6, "eye_size": 1.5 })].into_iter().enumerate() {
        let name = format!("Variant{k}");
        ok(app, ctx, tx, "create_character", json!({ "name": name, "proportions": p, "guides": false, "translate": [-2.0 - 2.0 * k as f64, 0, 0] }));
        settle(app, ctx);
        let q = ok(app, ctx, tx, "quality_report", json!({ "target": name, "proportions": false }));
        eprintln!("{p}: pass {}, failures {}, lumpiness {}, watertight {}", q["pass"], q["failures"], q["lumpiness"]["overall"], q["topology"]["watertight"]);
        assert_eq!(q["pass"], true, "{p}: {}", q["failures"]);
    }
    let t = std::time::Instant::now();
    let rt = ok(app, ctx, tx, "retopo", json!({ "target": "Character", "faces": 3000, "images": false }));
    eprintln!("retopo ({:.1} s): {} faces, {}% quads ({}% before split), {}% irregular, hausdorff {} cells", t.elapsed().as_secs_f64(), rt["faces"], rt["quads_pct"], rt["before_split"]["quads_pct"], rt["irregular_pct"], rt["hausdorff_cells"]);
    assert!(rt["before_split"]["quads_pct"].as_f64().unwrap() >= 95.0, "{rt}");
}

/// The same base made under a 1-thread and a 12-thread limit: the same
/// blobs, mesh (positions and indices) and measures. The mesh may be built
/// on the mesh library's own thread, where the calling thread's limit does
/// not reach; the strict 1-against-12 mesh check is the generator's
/// `deterministic_and_symmetric` (`build_mesh` under `with_threads`).
#[test]
fn character_same_on_1_and_12_threads() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let one = crate::clay::with_threads(1, || {
        let r = ok(app, ctx, tx, "create_character", json!({ "name": "One", "guides": false }));
        settle(app, ctx);
        r
    });
    let many = crate::clay::with_threads(12, || {
        let r = ok(app, ctx, tx, "create_character", json!({ "name": "Many", "guides": false }));
        settle(app, ctx);
        r
    });
    let (a, b) = (app.scene.get(id_of(app, "One")).unwrap(), app.scene.get(id_of(app, "Many")).unwrap());
    assert_eq!(a.clay, b.clay);
    assert_eq!(a.rig, b.rig);
    let (ma, mb) = (app.meshes.get(a).unwrap(), app.meshes.get(b).unwrap());
    assert!(ma.vertices.len() > 10_000 && ma.vertices.len() == mb.vertices.len());
    assert!(ma.vertices.iter().zip(&mb.vertices).all(|(x, y)| x.pos == y.pos) && ma.indices == mb.indices, "the same mesh");
    assert_eq!(one["clay"]["triangles"], many["clay"]["triangles"]);
    let measure = |app: &mut ClideApp, name: &str| crate::clay::with_threads(if name == "One" { 1 } else { 12 }, || ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": name }))["model"].clone());
    ok(app, ctx, tx, "guides", json!({ "action": "set", "lines": [{ "name": "c", "x": 0 }] }));
    assert_eq!(measure(app, "One"), measure(app, "Many"));
}

/// A normal editable clay: a sculpt stroke lands on the head, polish runs,
/// a blob edit rebuilds it, and the guides still measure (the head grown
/// by a tenth reads a bigger ratio and fails its target).
#[test]
fn character_is_editable() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_character", json!({ "name": "Buddy" }));
    settle(app, ctx);
    let id = id_of(app, "Buddy");
    // The cheek, on the surface, from the front.
    let hit = ok(app, ctx, tx, "surface_point", json!({ "view": "front", "at": [[0.12, 0.68]], "target": "Buddy" }));
    eprintln!("probe: {hit}");
    let p = hit["results"][0]["point"].clone();
    assert!(p.is_array(), "{hit}");
    let s = ok(app, ctx, tx, "sculpt", json!({ "target": "Buddy", "strokes": [{ "brush": "inflate", "points": [p], "radius": 0.03, "strength": 0.4 }] }));
    eprintln!("sculpt: {s}");
    settle(app, ctx);
    assert!(app.scene.get(id).and_then(|o| o.clay.as_ref()).is_some_and(|c| c.detail.is_some()), "the stroke wrote detail");
    let pol = ok(app, ctx, tx, "edit_clay", json!({ "target": "Buddy", "polish": { "amount": 0.1 } }));
    assert!(pol["polish"]["record_id"].is_string() || pol["polish"]["record_id"].is_number(), "{pol}");
    settle(app, ctx);
    let before = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Buddy" }));
    assert_eq!(before["pass"], true, "{}", before["failures"]);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Buddy", "update": [{ "blob": "head", "radii": [0.2376, 0.22, 0.2112] }] }));
    settle(app, ctx);
    let after = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Buddy" }));
    let ratio = by_name(&after["targets"], "head_ratio")["measured"].as_f64().unwrap();
    eprintln!("head grown: ratio {ratio}, failures {}", after["failures"]);
    assert!(ratio > 0.42 && after["pass"] == false, "{ratio}");
}

/// Save, new and open give the base back whole: blobs, rig, eyes (style,
/// place, bone), material and guides; one undo step takes all of it away
/// (the guides back as they were) and redo brings it back.
#[test]
fn character_round_trips_and_undoes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "guides", json!({ "action": "set", "lines": [{ "name": "old", "y": 0.5 }] }));
    let old = app.scene.guides.clone();
    let step = app.history.current();
    let objects = app.scene.objects.len();
    ok(app, ctx, tx, "create_character", json!({ "name": "Pal", "build": "quadruped", "proportions": { "head_units": 3, "ears": "pointed" }, "symmetry": false }));
    settle(app, ctx);
    assert_eq!(app.history.current(), step + 1, "one undo step");
    let pal = id_of(app, "Pal");
    let eye = app.scene.get(id_of(app, "Pal_eye.L")).unwrap().clone();
    assert_eq!((eye.parent, eye.bone.as_deref(), eye.shape), (Some(pal), Some("head"), Shape::Eye));
    assert_eq!(eye.eye.as_ref().unwrap().style, crate::eyeball::EyeStyle::Stylized);
    assert_eq!(app.scene.get(pal).unwrap().material.as_deref(), Some("Pal_skin"));
    let saved = app.scene.clone();
    let d = std::env::temp_dir().join(format!("clide3d_character_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pal.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    assert!(app.scene.objects.iter().all(|o| o.name != "Pal"));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    let pal2 = id_of(app, "Pal");
    let (a, b) = (saved.get(pal).unwrap(), app.scene.get(pal2).unwrap());
    assert_eq!(a.clay, b.clay);
    assert_eq!(a.rig, b.rig);
    assert_eq!(a.material, b.material);
    let eye2 = app.scene.get(id_of(app, "Pal_eye.L")).unwrap();
    assert_eq!((eye2.eye.as_ref(), eye2.translation, eye2.scale, eye2.bone.as_deref()), (eye.eye.as_ref(), eye.translation, eye.scale, Some("head")));
    assert_eq!(app.scene.guides, saved.guides);
    let m = ok(app, ctx, tx, "guides", json!({ "action": "measure", "target": "Pal" }));
    assert_eq!(m["pass"], true, "{}", m["failures"]);
    // Undo after reopening is the open; back on the original history:
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "guides", json!({ "action": "set", "lines": [{ "name": "old", "y": 0.5 }] }));
    let step = app.history.current();
    let objects2 = app.scene.objects.len();
    ok(app, ctx, tx, "create_character", json!({ "name": "Pal" }));
    settle(app, ctx);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.history.current(), step);
    assert_eq!(app.scene.objects.len(), objects2, "undo takes the clay and eyes");
    assert_eq!(app.scene.guides, old, "and puts the guides back");
    ok(app, ctx, tx, "redo", json!({}));
    assert!(app.scene.objects.iter().any(|o| o.name == "Pal_eye.R"));
    let _ = objects;
    let _ = std::fs::remove_dir_all(&d);
}

/// Refusals from the arguments, each leaving the scene and history as
/// they were.
#[test]
fn character_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_character", json!({ "name": "Taken" }));
    settle(app, ctx);
    let scene = app.scene.clone();
    let step = app.history.current();
    let refused = |app: &mut ClideApp, args: Value, says: &str| {
        let (v, err) = mcp(app, ctx, tx, "create_character", args.clone());
        assert!(err && v.to_string().contains(says), "{args}: {v}");
        // The whole scene: objects, guides, materials and the next id.
        assert!(app.scene == scene, "{args}: the scene changed");
        assert_eq!(app.history.current(), step, "{args}");
    };
    refused(app, json!({ "size": 2 }), "takes no size");
    refused(app, json!({ "name": "Taken" }), "already in the scene");
    refused(app, json!({ "build": "snake" }), "biped or quadruped");
    refused(app, json!({ "build": "quadruped", "proportions": { "hands": "mitten" } }), "hands apply to bipeds");
    refused(app, json!({ "proportions": { "tail": 1 } }), "unknown field");
    refused(app, json!({ "proportions": { "head_units": 6 } }), "head_units");
    refused(app, json!({ "proportions": { "height": 0 } }), "height");
    refused(app, json!({ "proportions": { "body": "square" } }), "proportions");
    refused(app, json!({ "proportions": { "eye_size": 2, "eye_spacing": 1.5, "head_width": 0.5 } }), "reach past the head");
    refused(app, json!({ "look": "anime" }), "look must be");
    refused(app, json!({ "eyes": "yes" }), "eyes must be true or false");
    refused(app, json!({ "color": [2, 0, 0] }), "color");
    refused(app, json!({ "translate": [1e5, 0, 0] }), "translate");
    refused(app, json!({ "resolution": 4096 }), "resolution");
    refused(app, json!({ "name": "" }), "name");
    // Review fixes (5c): names whose eye objects would not be guide names
    // (the guides were set, then dropped on load).
    refused(app, json!({ "name": "x".repeat(crate::character::MAX_NAME + 1) }), "name must be");
    refused(app, json!({ "name": "a\nb" }), "name must be");
    refused(app, json!({ "name": "  " }), "name must be");
    // A scene whose materials are full: refused before anything is added
    // (the old call added the clay, failed on the skin and removed it,
    // leaving the next id moved on: an undo step that changed nothing).
    let mut full = scene.clone();
    let mut m = full.materials[0].clone();
    while full.materials.len() < crate::material::MAX_MATERIALS {
        m.name = format!("filler{}", full.materials.len());
        full.materials.push(m.clone());
    }
    app.scene = full.clone();
    let (v, err) = mcp(app, ctx, tx, "create_character", json!({ "name": "Full" }));
    assert!(err && v.to_string().contains("materials"), "{v}");
    assert!(app.scene == full, "the scene changed");
    assert_eq!(app.history.current(), step);
    // look: plain adds no material, so it fits.
    ok(app, ctx, tx, "create_character", json!({ "name": "Full", "look": "plain" }));
}

/// Review fix (5c): a scene material already called `NAME_skin` is never
/// reset; the base takes the first free name.
#[test]
fn character_keeps_an_existing_skin_material() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "set_material", json!({ "name": "Character_skin", "preset": "skin_stylized", "roughness": 0.9 }));
    ok(app, ctx, tx, "set_material", json!({ "name": "Character_skin2", "roughness": 0.1 }));
    let before: Vec<_> = app.scene.materials.clone();
    let r = ok(app, ctx, tx, "create_character", json!({}));
    assert_eq!(r["material"], "Character_skin3", "{}", r["material"]);
    for m in &before {
        assert_eq!(app.scene.materials.iter().find(|x| x.name == m.name), Some(m), "{} was reset", m.name);
    }
    let skin = app.scene.materials.iter().find(|x| x.name == "Character_skin3").unwrap();
    // The preset, its subsurface reach scaled to the base's height (1).
    assert_eq!(skin, &crate::material::Material { name: "Character_skin3".into(), subsurface_radius: crate::character::SKIN_RADIUS, ..crate::material::Material::preset("skin_stylized").unwrap() });
}
