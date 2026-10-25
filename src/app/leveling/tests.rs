//! Stage 8a through MCP, on a headless editor (skips without a GPU
//! adapter): `detail_level` lists (changing nothing), adds levels (an
//! undo step) and picks the active one (saved clay state, never an undo
//! step); `sculpt` strokes default to the active level, keep it on their
//! records and replay bit for bit.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::clay::Clay;
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

fn clay(app: &ClideApp) -> Clay {
    app.scene.objects.iter().find(|o| o.name == "Ball").and_then(|o| o.clay.clone()).expect("the clay")
}

fn cells(v: &Value) -> Vec<f64> {
    v["levels"].as_array().unwrap_or_else(|| panic!("{v}")).iter().map(|l| l["cell"].as_f64().unwrap()).collect()
}

#[test]
fn detail_levels_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.02 }));
    settle(app, ctx);
    let listed = ok(app, ctx, tx, "detail_level", json!({ "target": "Ball" }));
    assert_eq!(cells(&listed).len(), 1, "{listed}");
    assert_eq!(listed["level"], 0);
    // Refusals change nothing.
    for bad in [json!({ "add": 0.03 }), json!({ "add": 0.00001 }), json!({ "level": 3 }), json!({ "level": 1, "cell": 0.01 }), json!({ "add": "fine" })] {
        let mut args = bad.clone();
        args["target"] = json!("Ball");
        let (v, err) = mcp(app, ctx, tx, "detail_level", args);
        assert!(err, "{bad}: {v}");
    }
    let step = app.history.current();
    let added = ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "add": 0.01 }));
    assert_eq!(added["level"], 1, "{added}");
    let picked = ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "cell": 0.005 }));
    assert_eq!(cells(&picked).len(), 3, "{picked}");
    assert_eq!(picked["level"], 2);
    assert!((picked["active_cell"].as_f64().unwrap() - 0.005).abs() < 1e-9);
    assert!(app.history.current() > step, "added levels are undo steps");
    // The same pick again changes nothing: no step.
    let at = app.history.current();
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "level": 2 }));
    assert_eq!(app.history.current(), at);
    assert_eq!(ok(app, ctx, tx, "get_clay", json!({ "target": "Ball" }))["levels"]["level"], 2);

    // A stroke without level works at the active one: it grows a 5 mm
    // region over itself and keeps level and cell on its record.
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "noise", "point": [0, 0.5, 0], "radius": 0.2, "strength": 1.0, "scale": 0.015 }] }));
    assert_eq!(r["detail_regions"], 1, "{r}");
    settle(app, ctx);
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "full": true }));
    let last = log["records"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["spec"]["level"], 2, "{last}");
    assert!((last["spec"]["cell"].as_f64().unwrap() - 0.005).abs() < 1e-9, "{last}");
    let c = clay(app);
    let fine = c.detail.as_ref().unwrap().grid().levels()[0].grid.brick_count();
    assert!(fine > 0 && c.detail_regions()[0].cell == 0.005);

    // A level-0 grab (named per stroke) writes the coarse grid and carries
    // the pores: the 5 mm level changes too.
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "grab", "point": [0, 0.5, 0], "radius": 0.15, "delta": [0.05, 0, 0], "level": 0 }] }));
    settle(app, ctx);
    let moved = clay(app);
    let fine_of = |c: &Clay| c.detail.as_ref().unwrap().grid().levels()[0].grid.bricks().map(|(k, b)| (*k, b.iter().map(|v| v.to_bits()).collect::<Vec<_>>())).collect::<std::collections::BTreeMap<_, _>>();
    assert_ne!(fine_of(&moved), fine_of(&c), "the finer level moved with the grab");
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "full": true }));
    let grab = log["records"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(grab["spec"]["level"], 0, "{grab}");
    assert!(grab["spec"]["cell"].is_null(), "{grab}");

    // The ledger replays bit for bit (the level is in the records).
    let replay = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replay" }));
    assert_eq!(replay["mismatched"].as_array().map_or(0, Vec::len), 0, "{replay}");

    // stroke_edit replace fills levels as sculpt does: the noise record's
    // spec without its level and cell takes the active level (2) and its
    // cell again, and gives the same grid.
    let noise = log["records"].as_array().unwrap().iter().find(|r| r["spec"]["brush"] == "noise").cloned().unwrap_or_else(|| panic!("{log}"));
    let mut bare = noise["spec"].clone();
    bare.as_object_mut().unwrap().remove("level");
    bare.as_object_mut().unwrap().remove("cell");
    let hash = |c: &Clay| <crate::clay::Detail as crate::ledger::Grid>::hash(c.detail.as_ref());
    let was = hash(&clay(app));
    ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replace", "id": noise["id"], "spec": bare }));
    settle(app, ctx);
    assert_eq!(hash(&clay(app)), was, "the replaced stroke does what it did");
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "full": true }));
    let noise = log["records"].as_array().unwrap().iter().find(|r| r["spec"]["brush"] == "noise").cloned().unwrap();
    assert_eq!(noise["spec"]["level"], 2, "{noise}");
    assert!((noise["spec"]["cell"].as_f64().unwrap() - 0.005).abs() < 1e-9, "{noise}");
    // Refused as sculpt refuses them: level 0 with a cell, and a stroke
    // that would grow a detail region (replace cannot).
    for spec in [
        json!({ "brush": "standard", "point": [0, 0.5, 0], "radius": 0.05, "level": 0, "cell": 0.01 }),
        json!({ "brush": "standard", "point": [0, -0.5, 0], "radius": 0.05, "level": 2 }),
    ] {
        let (v, err) = mcp(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replace", "id": noise["id"], "spec": spec }));
        assert!(err, "{spec}: {v}");
    }
    assert_eq!(hash(&clay(app)), was);

    // Saved and read back: levels and the active one kept.
    let text = serde_json::to_string(&clay(app)).unwrap();
    let back: Clay = serde_json::from_str(&text).unwrap();
    assert_eq!(crate::levels::cells(&back), vec![0.02, 0.01, 0.005]);
    assert_eq!(crate::levels::active(&back), 2);

    // Picking a level is selection-like: no undo step, and undo (of the
    // replace before it) keeps it; redo too.
    let at = app.history.current();
    let replaced = hash(&clay(app));
    let picked = ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "level": 0 }));
    assert_eq!(crate::levels::active(&clay(app)), 0);
    assert_eq!(app.history.current(), at, "a level pick is no undo step");
    assert!(picked["step"].is_null(), "and its reply names none: {picked}");
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "cell": 0.01 }));
    assert_eq!((crate::levels::active(&clay(app)), app.history.current()), (1, at), "picking a listed cell neither");
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.history.current(), at - 1);
    assert_eq!(crate::levels::active(&clay(app)), 1, "undo keeps the active level");
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(hash(&clay(app)), replaced);
    assert_eq!(crate::levels::active(&clay(app)), 1, "redo keeps it too");
}

/// A plain list reads the clay and changes nothing: no settle, no sync, no
/// step, the stack (and the cells a coarser detail cell hides) as it was.
/// An add keeps the hidden cells too.
#[test]
fn a_list_changes_nothing_and_hidden_levels_stay_kept() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.02 }));
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "add": 0.01 }));
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "add": 0.005 }));
    // A coarser detail cell hides the 1 cm level (it is not finer).
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.008 }));
    settle(app, ctx);
    let before = app.scene.clone();
    let step = app.history.current();
    let listed = ok(app, ctx, tx, "detail_level", json!({ "target": "Ball" }));
    assert_eq!(cells(&listed).len(), 2, "{listed}");
    assert!(app.scene == before, "a list leaves the scene as it was (grids and stacks by identity)");
    assert_eq!(app.history.current(), step);
    let kept = |app: &ClideApp| clay(app).sculpt.as_ref().unwrap().data().levels.clone();
    assert_eq!(kept(app), vec![0.01, 0.005]);
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "add": 0.004 }));
    assert_eq!(kept(app), vec![0.01, 0.005, 0.004], "the hidden 1 cm level stays kept");
    assert_eq!(crate::levels::cells(&clay(app)), vec![0.008, 0.005, 0.004]);
    assert_eq!(crate::levels::active(&clay(app)), 2);
}

/// A file's levels are tidied on read (bad, repeated and extra cells
/// dropped, coarse first), never refused; a coarser detail cell set since
/// hides the levels it passed.
#[test]
fn saved_levels_are_tidied_and_follow_the_detail_cell() {
    let mut file = json!({ "resolution": 48, "detail_cell": 0.02, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }], "sculpt": { "levels": [0.005, -1.0, null, 0.01, 0.01, 1e-9, 0.0101], "level": 0.01 } });
    let many: Vec<f64> = (1..200).map(|k| 0.019 - k as f64 * 0.00005).collect();
    let c: Clay = serde_json::from_value(file.clone()).unwrap();
    assert_eq!(crate::levels::cells(&c), vec![0.02, 0.0101, 0.01, 0.005]);
    file["sculpt"]["levels"] = json!(many);
    let c: Clay = serde_json::from_value(file.clone()).unwrap();
    assert_eq!(crate::levels::cells(&c).len(), crate::levels::MAX_LEVELS + 1);
    file["sculpt"]["levels"] = json!([0.01, 0.005]);
    file["detail_cell"] = json!(0.008);
    let c: Clay = serde_json::from_value(file).unwrap();
    assert_eq!(crate::levels::cells(&c), vec![0.008, 0.005]);
    assert_eq!(crate::levels::active(&c), 0, "the active level is gone: level 0");
}

/// Transport through layers: pores on a sculpt layer at level 2, then a
/// level-0 grab on base. The grab reads every layer and writes base, and
/// carries only base's own finer detail: the pores' layer keeps its grid
/// and ledger, base's finer level takes none of the pores (so hiding the
/// pores' layer leaves no pores), and base replays bit for bit with the
/// grab not stale (the pores' record, which read base, is stale by Q1's
/// rule, as after any base stroke). A grab on the pores' layer moves them.
#[test]
fn a_grab_on_base_leaves_another_layers_detail_to_it() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.02 }));
    settle(app, ctx);
    ok(app, ctx, tx, "detail_level", json!({ "target": "Ball", "add": 0.005 }));
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "pores" }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "layer": "pores", "strokes": [{ "brush": "noise", "point": [0, 0.5, 0], "radius": 0.2, "strength": 1.0, "scale": 0.015 }] }));
    settle(app, ctx);
    let before = clay(app);
    let pores = crate::layers::layer(&before, "pores").unwrap().detail.clone();
    assert!(pores.as_ref().is_some_and(|d| d.grid().levels().iter().any(|l| l.grid.brick_count() > 0)), "the pores are on the layer's 5 mm level");
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "grab", "point": [0, 0.5, 0], "radius": 0.15, "delta": [0.05, 0, 0], "level": 0 }] }));
    let grab = r["record_ids"][0].clone();
    settle(app, ctx);
    let after = clay(app);
    assert!(crate::layers::layer(&after, "pores").unwrap().detail == pores, "the pores' layer is not written");
    let fine = |c: &Clay| c.detail.as_ref().map_or(0, |d| d.grid().levels().iter().map(|l| l.grid.brick_count()).sum::<usize>());
    assert_eq!(fine(&after), fine(&before), "base's finer level takes none of the pores' layer's detail");
    let replay = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replay" }));
    assert_eq!(replay["mismatched"].as_array().map_or(0, Vec::len), 0, "{replay}");
    // (Base's records from before the layer was added read no layer: those
    // are stale by Q1's rule; the grab read the pores and is not.)
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball" }));
    let rec = log["records"].as_array().unwrap().iter().find(|r| r["id"] == grab).cloned().unwrap_or_else(|| panic!("{log}"));
    assert_eq!(rec["stale_context"], false, "{rec}");
    // With the pores' layer off, the clay shows no finer detail at all.
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "disable", "name": "pores" }));
    settle(app, ctx);
    let shown = crate::layers::composed_detail(&clay(app));
    assert_eq!(shown.as_ref().map_or(0, |d| d.grid().levels().iter().map(|l| l.grid.brick_count()).sum::<usize>()), 0, "no pores left on base");
    ok(app, ctx, tx, "sculpt_layer", json!({ "target": "Ball", "action": "enable", "name": "pores" }));
    // A grab on the pores' layer moves its pores (and writes only it).
    let base = clay(app).detail.clone();
    let pores = crate::layers::layer(&clay(app), "pores").unwrap().detail.clone();
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "layer": "pores", "strokes": [{ "brush": "grab", "point": [0, 0.5, 0], "radius": 0.15, "delta": [0.05, 0, 0], "level": 0 }] }));
    settle(app, ctx);
    let after = clay(app);
    assert!(after.detail == base, "base is not written");
    let fine_bits = |d: &Option<crate::clay::Detail>| d.as_ref().map(|d| d.grid().levels()[0].grid.bricks().map(|(k, b)| (*k, b.iter().map(|v| v.to_bits()).collect::<Vec<_>>())).collect::<std::collections::BTreeMap<_, _>>());
    assert_ne!(fine_bits(&crate::layers::layer(&after, "pores").unwrap().detail), fine_bits(&pores), "the pores moved on their layer");
}
