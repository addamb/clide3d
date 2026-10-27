//! Stage 4c through MCP, on a headless editor (skips without a GPU
//! adapter): `groom` on guides grown over a clay head, the head kept out,
//! the reply's numbers and pictures, undo, save and load, and refusals.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, ok, settle};
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

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

fn hair(app: &ClideApp, id: u64) -> crate::hair::Hair {
    app.scene.get(id).and_then(|o| o.hair.clone()).expect("a Hair")
}

fn refused(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, args: Value, says: &str) {
    let (v, err) = mcp(app, ctx, tx, "groom", args.clone());
    assert!(err && v.to_string().contains(says), "groom {args}: {v}");
}

/// A head (a clay sphere, moved and scaled) with 60 guides grown over it.
pub(super) fn grown(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) -> (u64, u64) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Head", "translate": [0.2, 1.5, 0.0] }));
    settle(app, ctx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Locks", "parent": "Head", "hair": { "grow": { "on": "Head", "count": 60, "length": 0.25 } } }));
    settle(app, ctx);
    (id_of(app, "Head"), id_of(app, "Locks"))
}

/// Points of the hair (world) inside the head's drawn surface, roots left
/// out; and the strands of its film render that enter the head.
fn inside(app: &ClideApp, head: u64, id: u64) -> (usize, usize) {
    let c = app.head_collider(head).unwrap();
    let w = app.scene.world_matrix(id);
    let h = hair(app, id);
    let points = h.guides.iter().flat_map(|g| g.points.iter().skip(1)).filter(|p| c.inside(w.transform_point3(**p))).count();
    let mut sample = h.clone();
    sample.strands = 2000;
    let strands = crate::hair::inside_count(&sample.strands(), |p| c.inside(w.transform_point3(p)));
    (points, strands)
}

/// Film strands, as `render_film` and the hair summary build them, stay
/// out of the clay the hair sits on: freshly grown and after a comb that
/// presses the guides onto the scalp, 0 of 2,000 sampled strands enter it
/// (the strands blended between the guides without the head do).
/// Strands the head does not reach are the ones `Hair::strands` makes.
#[test]
fn film_strands_keep_out_of_the_head() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (head, id) = grown(app, ctx, tx);
    let count = |app: &ClideApp| {
        let c = app.head_collider(head).unwrap();
        let w = app.scene.world_matrix(id);
        let mut sample = hair(app, id);
        sample.strands = 2000;
        let free = sample.strands();
        let t = std::time::Instant::now();
        let kept = app.hair_strands(id, Some(2000));
        assert!(kept.error.is_none() && kept.head == Some(head));
        let (kept, moved) = (kept.strands, kept.moved);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let inside = |s: &[crate::hair::Strand]| crate::hair::inside_count(s, |p| c.inside(w.transform_point3(p)));
        assert_eq!(kept.len(), free.len());
        let same = free.iter().zip(&kept).filter(|(a, b)| a.points == b.points).count();
        assert_eq!(same, free.len() - moved, "only the strands the head moved differ");
        // The summary samples the same strands.
        let summary = app.hair_summary(id);
        (inside(&free), inside(&kept), moved, ms, summary["strands_inside_head"].as_u64().unwrap())
    };
    let (free, kept, moved, ms, summary) = count(app);
    eprintln!("grown: {free}/2000 blended strands enter the head; kept out: {kept}, {moved} moved ({ms:.1} ms with the head's field); summary {summary}/256");
    assert!(free > 0, "the fixture has strands that enter the head without it");
    assert_eq!(kept, 0);
    assert_eq!(summary, 0);
    // A comb that presses every guide onto the scalp.
    ok(app, ctx, tx, "groom", json!({ "target": id, "render": false, "strokes": [{ "brush": "comb", "point": [0.2, 1.62, 0.0], "radius": 0.3, "direction": [0, -1, 0.3], "affect": "all", "falloff": "flat" }] }));
    let (free, kept, moved, ms, summary) = count(app);
    eprintln!("combed flat: {free}/2000 blended strands enter the head; kept out: {kept}, {moved} moved ({ms:.1} ms); summary {summary}/256");
    assert!(free > 0);
    assert_eq!(kept, 0);
    assert_eq!(summary, 0);
    // render_film keeps them out too and says so.
    let film = ok(app, ctx, tx, "render_film", json!({ "size": 64, "samples": 1, "denoise": false, "views": ["front"] }));
    eprintln!("render_film hair_kept_out: {}", film["hair_kept_out"]);
    assert_eq!(film["hair_kept_out"][0]["head"], "Head");
    assert!(film["hair_kept_out"][0]["strands_moved"].as_u64().unwrap() > 0);
    // The full film budget, timed (printed, not asserted).
    let t = std::time::Instant::now();
    let all = app.hair_strands(id, None);
    let (all, moved) = (all.strands, all.moved);
    eprintln!("film budget: {} strands, {moved} moved, {:.1} ms", all.len(), t.elapsed().as_secs_f64() * 1e3);
    // A hair with lift 0 (saved before groom): strands still end outside.
    let mut flat = hair(app, id);
    flat.lift = 0.0;
    app.scene.get_mut(id).unwrap().hair = Some(flat);
    let (free, kept, moved, _, _) = count(app);
    eprintln!("lift 0: {free}/2000 blended strands enter the head; kept out: {kept}, {moved} moved");
    assert_eq!(kept, 0);
}

/// A hair under no clay builds exactly the strands it always did.
#[test]
fn strands_without_a_clay_parent_are_unchanged() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Loose", "hair": { "grow": { "on": "Head", "count": 60, "length": 0.25 } } }));
    settle(app, ctx);
    let id = id_of(app, "Loose");
    assert!(app.hair_head(id).is_none(), "the hair is under no clay");
    let h = hair(app, id);
    let kept = app.hair_strands(id, None);
    assert!(kept.head.is_none() && kept.error.is_none());
    let (strands, moved) = (kept.strands, kept.moved);
    let plain = h.strands();
    assert_eq!(moved, 0);
    assert_eq!(strands.len(), plain.len());
    assert!(strands.iter().zip(&plain).all(|(a, b)| a.points == b.points && a.root_width == b.root_width && a.tip_width == b.tip_width && a.root_color == b.root_color && a.tip_color == b.tip_color));
}

/// The plan's 4c flow through MCP: a comb straight into the head, then
/// every other brush, each leaving no guide point inside the head; the
/// reply's numbers in world units and its front and right pictures; one
/// undo step per call that restores the guides exactly; save and load.
#[test]
fn groom_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (head, id) = grown(app, ctx, tx);
    let start = hair(app, id);
    assert_eq!(start.guides.len(), 60);
    assert!((start.lift - 0.0015).abs() < 1e-7, "grow kept its lift: {}", start.lift);
    let grown_inside = inside(app, head, id);
    eprintln!("grown: {} guide points inside the head, {}/2000 film strands enter it", grown_inside.0, grown_inside.1);
    assert_eq!(grown_inside.0, 0);
    let top = [0.2, 1.62, 0.0];
    // A comb down through the head, with the pictures.
    let (v, err) = mcp(app, ctx, tx, "groom", json!({ "target": id, "strokes": [{ "brush": "comb", "point": top, "radius": 0.3, "direction": [0, -1, 0.3], "affect": "all", "falloff": "flat" }] }));
    assert!(!err, "{v}");
    settle(app, ctx);
    let reply = &v;
    eprintln!("comb reply: {reply}");
    assert_eq!(reply["guides_changed"], 60);
    assert!(reply["collisions_resolved"].as_u64().unwrap() > 0, "the comb into the head needed the collider");
    assert_eq!(reply["still_inside"], 0);
    assert_eq!(reply["collide"], json!(["Head"]));
    let (pts, strands) = inside(app, head, id);
    eprintln!("after the comb: {pts} guide points inside the head, {strands}/2000 film strands enter it");
    assert_eq!(pts, 0);
    let combed = hair(app, id);
    // Every other brush, each through MCP, each keeping the head out.
    for stroke in [
        json!({ "brush": "clump", "point": [0.2, 1.4, -0.08], "radius": 0.12, "strength": 0.7 }),
        json!({ "brush": "twist", "point": [0.2, 1.4, -0.08], "radius": 0.12, "angle": 60 }),
        json!({ "brush": "smooth", "point": top, "radius": 0.4, "iterations": 6 }),
        json!({ "brush": "puff", "point": top, "radius": 0.4, "strength": 0.5 }),
        json!({ "brush": "relax", "point": top, "radius": 0.4 }),
        json!({ "brush": "lengthen", "point": [0.2, 1.3, -0.1], "radius": 0.2, "length": 0.03 }),
        json!({ "brush": "shorten", "point": top, "radius": 0.4, "length": 0.02 }),
        json!({ "brush": "cut", "point": top, "radius": 0.4, "plane": { "point": [0, 1.3, 0], "normal": [0, -1, 0] } }),
    ] {
        let r = ok(app, ctx, tx, "groom", json!({ "target": id, "render": false, "strokes": [stroke.clone()] }));
        let (pts, _) = inside(app, head, id);
        eprintln!("{}: {} changed, max move {:.4}, length change {:.4}, {} pushed, {pts} inside", stroke["brush"], r["guides_changed"], r["max_move"].as_f64().unwrap(), r["length_change"].as_f64().unwrap(), r["collisions_resolved"]);
        assert!(r["guides_changed"].as_u64().unwrap() > 0, "{stroke}: {r}");
        assert_eq!(pts, 0, "{stroke}");
    }
    // Cut: nothing below the plane in world space.
    let w = app.scene.world_matrix(id);
    let lowest = hair(app, id).guides.iter().flat_map(|g| g.points.iter()).map(|p| w.transform_point3(*p).y).fold(f32::MAX, f32::min);
    assert!(lowest >= 1.3 - 1e-4, "{lowest}");
    // Undo: one step per call, back to the comb's guides exactly, then to
    // the grown ones.
    for _ in 0..8 {
        ok(app, ctx, tx, "undo", json!({}));
    }
    assert!(hair(app, id) == combed, "undo restores the comb's guides exactly");
    ok(app, ctx, tx, "undo", json!({}));
    assert!(hair(app, id) == start, "and the grown guides");
    ok(app, ctx, tx, "redo", json!({}));
    assert!(hair(app, id) == combed);
    // Save and load.
    let dir = std::env::temp_dir().join(format!("clide3d_stage4c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("groomed.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    let back = hair(app, id_of(app, "Locks"));
    assert!(back == combed, "save and load keep the guides bit for bit");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reply carries the guides from the front and the right.
#[test]
fn groom_reply_shows_the_guides() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, id) = grown(app, ctx, tx);
    let (tx2, rx) = std::sync::mpsc::channel();
    tx.send(ToolCall { name: "groom".into(), args: json!({ "target": id, "size": 128, "strokes": [{ "brush": "comb", "point": [0.2, 1.62, 0.0], "radius": 0.3, "direction": [0, -1, 0] }] }), reply: tx2, source: crate::ledger::Source::Agent }).unwrap();
    let content = loop {
        frame(app, ctx, Vec::new());
        if let Ok(r) = rx.try_recv() {
            break r.expect("groom");
        }
    };
    let images = content.iter().filter(|c| c["type"] == "image").count();
    assert_eq!(images, 1, "one contact sheet of front and right");
}

/// Bounds and refusals from the arguments, before the hair changes.
#[test]
fn groom_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (head, id) = grown(app, ctx, tx);
    let before = hair(app, id);
    let one = json!({ "brush": "comb", "point": [0.2, 1.62, 0.0], "radius": 0.3, "direction": [0, -1, 0] });
    refused(app, ctx, tx, json!({ "target": id, "strokes": vec![one.clone(); 65] }), "1 to 64");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "comb", "points": vec![[0.2, 1.6, 0.0]; 257], "radius": 0.3, "direction": [0, -1, 0] }] }), "1 to 256");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [] }), "1 to 64");
    refused(app, ctx, tx, json!({ "target": head, "strokes": [one.clone()] }), "not a Hair");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [one.clone()], "colour": 1 }), "unknown groom field");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "comb", "point": [0, 1, 0], "radius": 0.3, "direction": [0, -1, 0], "wobble": 1 }] }), "unknown field wobble");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "brush", "point": [0, 1, 0], "radius": 0.3 }] }), "brush must be");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "comb", "point": [0, 1, 0], "radius": 0.3 }] }), "comb needs a direction");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "lengthen", "point": [0, 1, 0], "radius": 0.3 }] }), "needs length");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "twist", "point": [0, 1, 0], "radius": 0.3, "angle": 1000 }] }), "angle");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "smooth", "point": [0, 1, 0], "radius": 0.3, "iterations": 100 }] }), "iterations");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "smooth", "point": [0, 1, 0], "radius": -1 }] }), "radius");
    refused(app, ctx, tx, json!({ "target": id, "strokes": [{ "brush": "smooth", "point": [0, 1, 0], "radius": 0.3, "collide": "Locks" }] }), "collide must be a clay");
    // The Groom tool does not cut (agents send cut with a length or plane).
    let (v, err) = mcp(app, ctx, tx, "brush_preset", json!({ "action": "set_current", "groom": "cut" }));
    assert!(err && v.to_string().contains("does not cut"), "{v}");
    // A good stroke then a bad one: nothing applies.
    refused(app, ctx, tx, json!({ "target": id, "strokes": [one, { "brush": "smooth", "point": [0, 1, 0], "radius": 0.3, "strength": 2 }] }), "strength");
    assert!(hair(app, id) == before);
}

/// The handler's time with the real head (its clay field as collider):
/// the grow cap of 400 guides combed into the head (`--ignored`; prints).
#[test]
#[ignore]
fn groom_handler_time() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 96, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Locks", "parent": "Head", "hair": { "grow": { "on": "Head", "count": 400, "length": 0.3 } } }));
    settle(app, ctx);
    let id = id_of(app, "Locks");
    for (brush, extra) in [("comb", json!({ "direction": [0, -1, 0.3], "affect": "all" })), ("clump", json!({})), ("relax", json!({}))] {
        let mut stroke = json!({ "brush": brush, "point": [0, 0.12, 0], "radius": 0.4 });
        for (k, v) in extra.as_object().unwrap() {
            stroke[k] = v.clone();
        }
        let t = std::time::Instant::now();
        let r = ok(app, ctx, tx, "groom", json!({ "target": id, "render": false, "strokes": [stroke] }));
        eprintln!("{brush}: {} guides x {} points changed, kernel {} ms, call {:.1} ms, {} pushed", r["guides_changed"], r["points_after"].as_u64().unwrap() / r["guides_changed"].as_u64().unwrap().max(1), r["timing_ms"], t.elapsed().as_secs_f64() * 1e3, r["collisions_resolved"]);
    }
}

/// Card vertices (hair space, as drawn) inside the head's surface, and
/// the cards the hair would draw with no head.
fn cards_inside(app: &ClideApp, head: u64, id: u64) -> (usize, usize, usize) {
    let c = app.head_collider(head).unwrap();
    let w = app.scene.rest_world_matrix(id);
    let o = app.scene.get(id).unwrap();
    let drawn = app.meshes.get(o).expect("the hair's cards").clone();
    let plain = hair(app, id).build_mesh();
    let count = |m: &crate::mesh::Mesh| m.vertices.iter().filter(|v| c.inside(w.transform_point3(glam::Vec3::from(v.pos)))).count();
    (count(&plain), count(&drawn), drawn.vertices.len())
}

/// Hair cards, as the viewport and exports draw them, stay out of the
/// clay the hair sits on (Stage 4d follow-up), by the code that keeps
/// guides and film strands out: freshly grown and after a comb that
/// presses the guides onto the scalp, no card vertex is inside the head
/// (the plain cards have some). The cards build off the window's thread:
/// a background sync starts a build and shows the old cards until it is
/// in, and the result equals a waiting sync's. The hair summary says how
/// many cards moved.
#[test]
fn cards_keep_out_of_the_head() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (head, id) = grown(app, ctx, tx);
    let (plain, kept, all) = cards_inside(app, head, id);
    eprintln!("grown: {plain}/{all} plain card vertices inside the head; drawn: {kept}");
    assert!(plain > 0, "the fixture's plain cards enter the head");
    assert_eq!(kept, 0);
    let summary = ok(app, ctx, tx, "update_object", json!({ "target": id, "visible": true }));
    eprintln!("summary cards_kept_out_of_head: {}", summary["hair"]["cards_kept_out_of_head"]);
    assert!(summary["hair"]["cards_kept_out_of_head"].as_u64().unwrap() > 0, "{}", summary["hair"]);
    ok(app, ctx, tx, "groom", json!({ "target": id, "render": false, "strokes": [{ "brush": "comb", "point": [0.2, 1.62, 0.0], "radius": 0.3, "direction": [0, -1, 0.3], "affect": "all", "falloff": "flat" }] }));
    settle(app, ctx);
    let (plain, kept, all) = cards_inside(app, head, id);
    eprintln!("combed flat: {plain}/{all} plain card vertices inside the head; drawn: {kept}");
    assert!(plain > 0);
    assert_eq!(kept, 0);
    // In the background: a change starts a build on another thread; the
    // old cards show meanwhile; the result is a waiting sync's.
    let shown = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    let mut h = hair(app, id);
    h.width *= 1.5;
    app.scene.get_mut(id).unwrap().hair = Some(h);
    app.meshes.sync_background(&app.scene);
    assert!(app.meshes.busy(), "the cards build in the background");
    assert!(std::sync::Arc::ptr_eq(app.meshes.get(app.scene.get(id).unwrap()).unwrap(), &shown), "the old cards show meanwhile");
    settle(app, ctx);
    let background = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    let mut fresh = crate::mesh::MeshLib::new();
    fresh.sync(&app.scene);
    fresh.sync(&app.scene);
    let waited = fresh.get(app.scene.get(id).unwrap()).unwrap().clone();
    assert!(background.vertices.iter().zip(&waited.vertices).all(|(a, b)| a.pos == b.pos) && background.vertices.len() == waited.vertices.len());
    assert_eq!(cards_inside(app, head, id).1, 0);
}

/// A hair under no clay draws exactly the cards `Hair::build_mesh` makes.
#[test]
fn cards_without_a_clay_parent_are_unchanged() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Head", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Loose", "hair": { "grow": { "on": "Head", "count": 60, "length": 0.25 } } }));
    settle(app, ctx);
    let id = id_of(app, "Loose");
    let drawn = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    let plain = hair(app, id).build_mesh();
    assert_eq!(drawn.vertices.len(), plain.vertices.len());
    assert!(drawn.vertices.iter().zip(&plain.vertices).all(|(a, b)| a.pos == b.pos && a.normal == b.normal && a.uv == b.uv && a.tangent == b.tangent));
    assert_eq!((&drawn.indices, &drawn.edges), (&plain.indices, &plain.edges));
    assert_eq!(app.meshes.hair_cards_moved(id), Some(0));
}

/// Review fix (Stage 4d review, finding 1): on a posed frame, strands
/// and cards are kept out of the head at rest in the hair's space, so a
/// hair riding a bone gets the same strands and cards as at rest, and,
/// placed by the pose, none of them is inside the posed head (the head
/// nods 30 degrees about its neck, which moves it). Before, the hair was
/// placed by the pose against the head's rest field.
#[test]
fn posed_hair_keeps_out_of_the_posed_head() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (head, id) = grown(app, ctx, tx);
    ok(app, ctx, tx, "rig", json!({ "target": "Head", "add": [{ "name": "neck", "head": [0, -0.15, 0], "tail": [0, -0.05, 0] }] }));
    ok(app, ctx, tx, "update_object", json!({ "target": "Locks", "bone": "neck" }));
    ok(app, ctx, tx, "animate", json!({ "target": "Head", "keys": [{ "bone": "neck", "frame": 0, "rotate": [0, 0, 0] }, { "bone": "neck", "frame": 20, "rotate": [60, 0, 0] }] }));
    settle(app, ctx);
    let rest_strands = app.hair_strands(id, Some(2000)).strands;
    let rest_cards = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    app.timeline.frame = 10.0;
    frame(app, ctx, Vec::new());
    settle(app, ctx);
    assert!(app.scene.frame.is_some(), "posed");
    let posed_world = app.scene.world_matrix(id);
    assert!(!posed_world.abs_diff_eq(app.scene.rest_world_matrix(id), 1e-4), "the pose moves the hair");
    let kept = app.hair_strands(id, Some(2000));
    assert!(kept.error.is_none());
    assert!(kept.strands.iter().zip(&rest_strands).all(|(a, b)| a.points == b.points), "the same strands in the hair's space as at rest");
    let cards = app.meshes.get(app.scene.get(id).unwrap()).unwrap().clone();
    assert!(cards.vertices.iter().zip(&rest_cards.vertices).all(|(a, b)| a.pos == b.pos), "the same cards as at rest");
    // The posed head: the rest sphere (r 0.1 at the clay's origin) carried
    // by the neck's skin matrix.
    let skin = app.scene.get(head).unwrap().rig.as_ref().unwrap().skin_matrices(app.scene.frame)[0];
    let center = (app.scene.world_matrix(head) * skin).transform_point3(glam::Vec3::ZERO);
    let rest_center = app.scene.world_matrix(head).transform_point3(glam::Vec3::ZERO);
    eprintln!("nod moves the head's centre by {:.4}", center.distance(rest_center));
    assert!(center.distance(rest_center) > 0.02);
    let inside = |p: glam::Vec3| posed_world.transform_point3(p).distance(center) < 0.1 - 0.002;
    let strands_in = crate::hair::inside_count(&kept.strands, inside);
    let cards_in = cards.vertices.iter().filter(|v| inside(glam::Vec3::from(v.pos))).count();
    eprintln!("posed: {strands_in}/{} strands and {cards_in} card vertices inside the posed head", kept.strands.len());
    assert_eq!((strands_in, cards_in), (0, 0));
    app.timeline.frame = 0.0;
}

/// Review fix (finding 2): a settings patch on a hair (`update_object
/// hair {width}`) keeps its guides shared with the undo history instead
/// of copying them through JSON; `add_guides` does change them.
#[test]
fn hair_settings_patch_keeps_guides_shared() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let (_, id) = grown(app, ctx, tx);
    let before = hair(app, id);
    ok(app, ctx, tx, "update_object", json!({ "target": id, "hair": { "width": 0.012, "frizz": 0.001 } }));
    let after = hair(app, id);
    assert!(after.guides.shares(&before.guides), "a settings patch does not copy the guides");
    assert_eq!(after.width, 0.012);
    ok(app, ctx, tx, "update_object", json!({ "target": id, "hair": { "add_guides": [[[0.2, 1.6, 0.0], [0.2, 1.7, 0.0]]] } }));
    let more = hair(app, id);
    assert!(!more.guides.shares(&before.guides));
    assert_eq!(more.guides.len(), before.guides.len() + 1);
    assert!(more.guides[..before.guides.len()] == before.guides[..]);
}
