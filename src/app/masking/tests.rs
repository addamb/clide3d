//! Stage 3 through MCP, on a headless editor (skips without a GPU
//! adapter): mask and group actions as ledger records, a rect that
//! resends to the same mask after the camera moves, the overlay (no mesh
//! build, the tint's cost), hidden groups, and the masked transform.

use std::sync::mpsc::Sender;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, mcp, ok, settle};
use crate::clay::{Clay, Detail};
use crate::ledger::{self, Grid};
use crate::mask::SculptMask;
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

fn ball(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, resolution: u32) {
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": name, "resolution": resolution, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }] }));
    settle(app, ctx);
}

fn clay(app: &ClideApp, name: &str) -> Clay {
    app.scene.objects.iter().find(|o| o.name == name).and_then(|o| o.clay.clone()).expect("the clay")
}

fn mask_hash(app: &ClideApp, name: &str) -> ledger::Hash128 {
    SculptMask::hash(clay(app, name).sculpt_mask.as_ref())
}

#[test]
fn mask_actions_are_records_and_a_rect_resends_to_the_same_mask() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ball(app, ctx, tx, "Ball", 64);
    let camera = json!({ "view": Mat4::look_at_rh(Vec3::new(0.4, 0.3, 3.0), Vec3::ZERO, Vec3::Y).to_cols_array(), "proj": Mat4::perspective_rh(0.7, 1.0, 0.1, 20.0).to_cols_array(), "viewport": [400.0, 400.0] });
    let step = app.history.current();
    let r = ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [
        { "rect": { "camera": camera, "from": [150, 120], "to": [260, 230] } },
        { "grow": 1 },
        { "lasso": { "view": "front", "at": [[-0.4, -0.45], [0.0, -0.1], [0.4, -0.45]], "mode": "add", "through": false } },
        "invert", "invert",
    ] }));
    assert_eq!(r["records"].as_array().unwrap().len(), 5, "{r}");
    assert!(r["coverage"].as_f64().unwrap() > 0.02 && r["coverage"].as_f64().unwrap() < 0.6, "{r}");
    assert_eq!(app.history.current(), step + 1, "one undo step");
    settle(app, ctx);
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "layer": "mask", "full": true, "verbose": true }));
    let records = log["records"].as_array().unwrap_or_else(|| panic!("{log}")).clone();
    assert_eq!(records.len(), 5);
    assert!(records.iter().all(|r| r["kind"] == "action" && r["source"] == "agent" && !r["stale_context"].as_bool().unwrap()), "{log}");
    // The rect's record holds its camera (matrices, not the editor's view).
    assert_eq!(records[0]["action"]["rect"]["camera"]["viewport"], json!([400.0, 400.0]));
    assert!(records[0]["resolved"]["select"]["select"]["lasso"]["camera"]["view"].is_array(), "{}", records[0]);
    let first = records[0]["result_hash"].clone();
    let full = mask_hash(app, "Ball");
    // The viewport camera moves; the stored actions, sent again, give the
    // same masks.
    app.cams.orbit(crate::camera::ViewKind::Persp, glam::Vec2::new(0.9, -0.4));
    app.cams.persp.distance = 7.0;
    frame(app, ctx, Vec::new());
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": ["clear"] }));
    let again = ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [records[0]["action"].clone()] }));
    settle(app, ctx);
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "layer": "mask", "verbose": true }));
    assert_eq!(log["records"].as_array().unwrap().last().unwrap()["result_hash"], first, "{again}");
    let actions: Vec<Value> = records.iter().map(|r| r["action"].clone()).collect();
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [ "clear" ] }));
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": actions }));
    assert_eq!(mask_hash(app, "Ball"), full, "the stored actions rebuild the mask");
    // The ledger tools work on the mask layer: replay checks, disable
    // drops a record, undo goes back.
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "layer": "mask", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
    let ids: Vec<u64> = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "layer": "mask", "verbose": true }))["records"].as_array().unwrap().iter().map(|r| r["id"].as_u64().unwrap()).collect();
    ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "layer": "mask", "action": "disable", "ids": [ids[ids.len() - 1]] }));
    settle(app, ctx);
    assert_ne!(mask_hash(app, "Ball"), full);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(mask_hash(app, "Ball"), full);
    // Reads: get_clay, probe_clay and surface_point report the mask.
    let g = ok(app, ctx, tx, "get_clay", json!({ "target": "Ball" }));
    assert!(g["mask"]["bricks"].as_u64().unwrap() > 0 && g["layers"].as_array().unwrap().iter().any(|l| l["name"] == "mask" && l["records"].as_u64().unwrap() > 0), "{g}");
    let probe = ok(app, ctx, tx, "probe_clay", json!({ "target": "Ball", "points": [[0.0, -0.3, 0.4], [0.0, 0.5, -0.1]] }));
    let (low, top) = (probe["probes"][0]["mask"].as_f64().unwrap(), probe["probes"][1]["mask"].as_f64().unwrap());
    assert!(low > 0.9 && top < 0.1, "{probe}");
    let sp = ok(app, ctx, tx, "surface_point", json!({ "target": "Ball", "points": [[0.0, -0.3, 0.4]] }));
    assert!(sp["results"][0]["mask"].as_f64().unwrap() > 0.9, "{sp}");
    // Bad actions change nothing.
    for bad in [json!(["sharpen", { "grow": 0 }]), json!([{ "lasso": { "view": "sideways", "at": [[0, 0], [1, 0], [0, 1]] } }]), json!([{ "by_blobs": ["nope"] }]), json!([{ "teleport": 1 }]), json!([{ "rect": {} }])] {
        let before = mask_hash(app, "Ball");
        let (why, err) = mcp(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": bad.clone() }));
        assert!(err, "{bad}: {why}");
        assert_eq!(mask_hash(app, "Ball"), before);
    }
    // Channels: to_channel writes a smart-material channel, from_channel
    // reads it back.
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "to_channel": "masked" }] }));
    assert!(clay(app, "Ball").masks.contains_key("masked"));
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": ["clear", { "from_channel": "masked" }] }));
    assert_eq!(mask_hash(app, "Ball"), full, "the channel holds the mask exactly");
    // A channel holds the mask as it was at its to_channel, and a later
    // action of the same call can read it back.
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "to_channel": "snap" }, "invert", { "to_channel": "inverted" }, "clear", { "from_channel": "snap" }] }));
    assert_eq!(mask_hash(app, "Ball"), full, "to_channel takes the mask at its own action");
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "from_channel": "inverted" }, "invert"] }));
    assert_eq!(mask_hash(app, "Ball"), full, "the inverted channel holds the inverted mask");
    // The channel cap holds for the whole call: nothing changes when the
    // call would pass it.
    let have = clay(app, "Ball").masks.len();
    let names: Vec<Value> = (0..=crate::smart::MAX_CHANNELS - have).map(|k| json!({ "to_channel": format!("extra{k}") })).collect();
    let (step, before) = (app.history.current(), clay(app, "Ball").masks.len());
    let (why, err) = mcp(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": ([vec![json!("invert")], names].concat()) }));
    assert!(err && why.to_string().contains("mask channels"), "{why}");
    assert_eq!((mask_hash(app, "Ball"), clay(app, "Ball").masks.len(), app.history.current()), (full, before, step));
}

#[test]
fn a_mask_or_group_edit_builds_no_mesh_and_tints_fast() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ball(app, ctx, tx, "Ball", 128);
    let id = app.resolve(&json!("Ball")).unwrap();
    let vertices = app.meshes.rest(id).unwrap().0.vertices.len();
    assert!(vertices >= 45_000, "{vertices} vertices");
    let (builds, shading) = (app.meshes.geometry_builds, app.meshes.shading_updates);
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_normal": { "dir": [0, 1, 0], "angle": 50 } }] }));
    settle(app, ctx);
    assert_eq!((app.meshes.geometry_builds, app.meshes.shading_updates), (builds, shading), "a mask edit builds no mesh");
    let o = app.scene.get(id).unwrap().clone();
    let (shown, clean) = (app.meshes.display(&o).unwrap().clone(), app.meshes.get(&o).unwrap().clone());
    let mask = clay(app, "Ball").sculpt_mask.unwrap();
    // The tint's cost, against a reference of the same shape: a
    // `par_chunks` pass sampling the mask at every vertex into a scratch
    // buffer, the tint's own lookups without its copy and colors. Under
    // the suite's parallel load the tint loses `par_chunks` helpers and
    // CPU time, and so does the reference run next to it (a copy-only
    // reference does not: it barely depends on helpers). Each round
    // interleaves the two and takes their medians; the load is that
    // round's reference over its quiet time (REF_QUIET_MS, measured on an
    // M2 Max, where the tint alone takes about 1.3 ms), so tint and load come from the same run, and the quietest
    // round counts. CLIDE_PERF=1 (a quiet machine) takes no allowance.
    const REF_QUIET_MS: f64 = 1.05;
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let positions: Vec<Vec3> = clean.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let mut scratch = vec![0.0f32; positions.len()];
    let (ms, reference, load) = (0..5)
        .map(|_| {
            let (mut tints, mut refs) = (Vec::new(), Vec::new());
            for _ in 0..11 {
                tints.push(crate::harness::perf::time(1, || crate::overlay::tint(&clean, &clean, Some(&mask), None, false)).1);
                refs.push(crate::harness::perf::time(1, || crate::clay::par_chunks(&mut scratch, 2048, |chunk, out| {
                    for (j, k) in out.iter_mut().enumerate() {
                        *k = mask.grid().value(positions[chunk * 2048 + j]);
                    }
                })).1);
            }
            let (ms, reference) = (median(&mut tints), median(&mut refs));
            let load = if crate::harness::perf::strict() { 1.0 } else { (reference / REF_QUIET_MS).max(1.0) };
            (ms, reference, load)
        })
        .min_by(|a, b| (a.0 / a.2).total_cmp(&(b.0 / b.2)))
        .unwrap();
    std::hint::black_box(&scratch);
    eprintln!("mask overlay on {vertices} vertices: {ms:.2} ms (reference lookups {reference:.2} ms, load {load:.2}, last live tint {:.2} ms)", app.meshes.overlay_ms);
    crate::harness::perf::check("mask_overlay_50k", ms, 3.0 * load, json!({ "vertices": vertices, "reference_ms": reference, "load": load }));
    // The tint: masked surface at least 25% darker; film and exports
    // read the clean mesh.
    let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let (mut ratio, mut n) = (0.0f64, 0);
    for (a, b) in shown.vertices.iter().zip(&clean.vertices) {
        if mask.grid().value(Vec3::from(b.pos)) >= 0.99 {
            ratio += (lum(a.color) / lum(b.color)) as f64;
            n += 1;
        }
    }
    let ratio = ratio / n as f64;
    eprintln!("masked vertices: luminance x {ratio:.3} ({n} vertices)");
    assert!(n > 1000 && ratio <= 0.75, "{ratio}");
    assert!(!std::sync::Arc::ptr_eq(&shown, &clean));
    // Groups: hide one; its triangles leave the drawn mesh, no mesh builds,
    // a brush over it changes nothing.
    let (builds, shading) = (app.meshes.geometry_builds, app.meshes.shading_updates);
    let g = ok(app, ctx, tx, "group", json!({ "target": "Ball", "actions": ["from_blobs", { "paint": { "id": 7, "strokes": [{ "point": [0, 0, 0.5], "radius": 0.2 }] } }, { "hide": { "ids": [7] } }] }));
    settle(app, ctx);
    assert_eq!(g["hidden"], json!([7]), "{g}");
    assert!(g["groups"].as_array().unwrap().iter().any(|x| x["id"] == 7 && x["visible"] == false && x["area"].as_f64().unwrap() > 0.01), "{g}");
    assert_eq!((app.meshes.geometry_builds, app.meshes.shading_updates), (builds, shading), "a group edit builds no mesh");
    let shown = app.meshes.display(&o).unwrap().clone();
    assert!(shown.indices.len() < clean.indices.len(), "hidden triangles are left out");
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0, 0.5], "radius": 0.08 }] }));
    assert_eq!(r["strokes"][0]["changed_points"], json!(0), "{r}");
    let list = ok(app, ctx, tx, "group", json!({ "target": "Ball", "actions": ["list"] }));
    assert_eq!(list["groups"], g["groups"]);
    ok(app, ctx, tx, "group", json!({ "target": "Ball", "actions": ["show_all"] }));
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0, 0.5], "radius": 0.08 }] }));
    assert!(r["strokes"][0]["changed_points"].as_u64().unwrap() > 0);
    // The overlay is display state: set_display switches it, no undo step.
    let step = app.history.current();
    let d = ok(app, ctx, tx, "set_display", json!({ "overlay": "groups" }));
    assert_eq!(d["overlay"], "groups");
    settle(app, ctx);
    assert_eq!(app.history.current(), step);
    ok(app, ctx, tx, "set_display", json!({ "overlay": "none" }));
    settle(app, ctx);
    let none = app.meshes.display(&o).unwrap().clone();
    assert!(std::sync::Arc::ptr_eq(&none, app.meshes.get(&o).unwrap()), "no tint without an overlay or hidden groups");
}

#[test]
fn the_masked_transform_and_auto_masking_through_mcp() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ball(app, ctx, tx, "Ball", 64);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.02 }));
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "axis": "y", "max": 0.1 } }] }));
    let before = clay(app, "Ball");
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "transform", "transform": { "translate": [0, 0.04, 0] } }] }));
    settle(app, ctx);
    assert!(r["strokes"][0]["changed_points"].as_u64().unwrap() > 100, "{r}");
    let after = clay(app, "Ball");
    let mask = after.sculpt_mask.clone().unwrap();
    let mut masked = 0.0f32;
    ledger::detail_changes(before.detail.as_ref(), after.detail.as_ref(), |p, _, u, v| {
        if mask.grid().get((p / mask.grid().voxel).round().as_ivec3().to_array()) == 255 {
            masked = masked.max((v - u).abs());
        }
    });
    assert!(masked <= 1e-6, "masked points moved {masked}");
    // The record names the mask it read; it replays the same after the
    // mask is cleared.
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Ball", "full": true, "verbose": true }));
    let rec = log["records"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(rec["brush"], "transform");
    assert_eq!(rec["resolved"]["frozen"]["mask"], json!(SculptMask::hash(Some(&mask))));
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": ["clear"] }));
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
    assert_eq!(Detail::hash(clay(app, "Ball").detail.as_ref()), Detail::hash(after.detail.as_ref()));
    // Save and load: the frozen mask travels with the clay.
    let path = std::env::temp_dir().join(format!("clide3d-stage3-{}.json", std::process::id()));
    ok(app, ctx, tx, "save_scene", json!({ "path": path.to_string_lossy() }));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.to_string_lossy() }));
    settle(app, ctx);
    let g = ok(app, ctx, tx, "get_clay", json!({ "target": "Ball" }));
    let base = g["layers"].as_array().unwrap().iter().find(|l| l["name"] == "base").unwrap().clone();
    assert_eq!(base["replay_verified"], json!(true), "{base}");
    // Auto-masking fields go through: a backface-masked stroke through a
    // thin slab's top leaves its bottom alone.
    let (_, err) = mcp(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0.5, 0], "radius": 0.1, "mask": { "backface": true, "normal_angle": 70, "cavity": 0.3, "connected": true, "groups": [1] } }] }));
    assert!(!err);
    let (why, err) = mcp(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0.5, 0], "radius": 0.1, "mask": { "cavity": 3 } }] }));
    assert!(err, "{why}");
    // The clay keeps only the masks its records still read: squashing the
    // strokes that read them lets them go (on the clay, not only the job's
    // copy), so the store never passes its cap while strokes need it.
    let kept = |app: &ClideApp| {
        let c = clay(app, "Ball");
        let used = ledger::frozen_in_use(&c);
        let masks: Vec<ledger::Hash128> = c.sculpt.as_ref().map(|s| s.data().frozen.masks.keys().copied().collect()).unwrap_or_default();
        assert!(masks.iter().all(|h| used.contains(h)), "the clay keeps masks no record reads");
        masks.len()
    };
    let mut ids = Vec::new();
    for k in 0..3 {
        ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "axis": "y", "max": (0.12 + 0.05 * k as f64) } }] }));
        let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0.5, 0], "radius": 0.1 }] }));
        ids.extend(r["record_ids"].as_array().unwrap().clone());
    }
    let many = kept(app);
    ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "squash", "ids": ids }));
    ok(app, ctx, tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "axis": "y", "max": 0.3 } }] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "point": [0, 0.5, 0], "radius": 0.1 }] }));
    let few = kept(app);
    eprintln!("frozen masks: {many} then {few}");
    assert!(few + 2 <= many, "the squashed strokes' masks went: {many} then {few}");
    let check = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Ball", "action": "replay" }));
    assert_eq!(check["mismatched"], json!([]), "{check}");
}
