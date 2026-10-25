//! Stage 6 through MCP, on a headless editor (skips without a GPU
//! adapter): Sculptris Pro regions, matcap and ambient occlusion settings
//! (the Display menu sends the same call), sculpt sessions drawing in
//! chunks, and the agent's sculpt latency on the 1.65M-triangle fixture.

use std::sync::mpsc::Sender;

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, ok, settle};
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

/// Whether region `r` (center, radii) holds the box lo..hi.
fn holds(r: &Value, lo: Vec3, hi: Vec3) -> bool {
    let v = |k: &str| Vec3::from(serde_json::from_value::<[f32; 3]>(r[k].clone()).unwrap());
    let (c, radii) = (v("center"), v("radii"));
    (0..8).all(|i| {
        let p = Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][i >> 2]);
        ((p - c) / radii).length_squared() <= 1.0 + 1e-3
    })
}

#[test]
fn sculptris_pro_grows_at_most_sixteen_regions_over_the_strokes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Pot", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pot", "detail_cell": 0.01 }));
    // 30 strokes over the upper half, at three fine cells.
    let (r, mut strokes, mut boxes) = (0.03f32, Vec::new(), Vec::new());
    for k in 0..30 {
        let (theta, phi) = (0.2 + 1.2 * (k as f32 * 0.618_034).fract(), k as f32 * 2.399_963);
        let p = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()) * 0.5;
        let cell = [0.005, 0.004, 0.0045][k % 3];
        strokes.push(json!({ "brush": "inflate", "points": [p.to_array()], "radius": r, "strength": 0.3, "cell": cell }));
        boxes.push((p - r, p + r));
    }
    let reply = ok(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": strokes }));
    let summary = if reply.get("summary").is_some() { &reply["summary"] } else { &reply };
    assert!(summary["detail_regions"].as_u64().is_some_and(|n| n <= 16), "{reply}");
    let clay = ok(app, ctx, tx, "get_clay", json!({ "target": "Pot" }));
    let regions = clay["detail_regions"].as_array().unwrap().clone();
    assert!(!regions.is_empty() && regions.len() <= 16, "{} regions", regions.len());
    for (lo, hi) in &boxes {
        assert!(regions.iter().any(|r| holds(r, *lo, *hi)), "a stroke box {lo} {hi} is in no region");
    }
    eprintln!("Sculptris Pro: 30 strokes, {} regions", regions.len());
    // The ledger replays to the same grid, region changes and all.
    let replay = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Pot", "action": "replay" }));
    assert!(replay["mismatched"].as_array().is_none_or(|m| m.is_empty()), "{replay}");
    // A stroke inside a region of its cell changes no region.
    let before = app.scene.get(id_of(app, "Pot")).unwrap().clay.as_ref().unwrap().detail_regions();
    let middle = ((boxes[0].0 + boxes[0].1) * 0.5).to_array();
    ok(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": [{ "brush": "inflate", "points": [middle], "radius": 0.01, "cell": 0.005 }] }));
    assert_eq!(app.scene.get(id_of(app, "Pot")).unwrap().clay.as_ref().unwrap().detail_regions(), before);
    // Coarser than the clay's cell is refused, and changes nothing.
    let (e, err) = super::recorder::tests::mcp(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": [{ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.03, "cell": 0.02 }] }));
    assert!(err && e.to_string().contains("finer"), "{e}");
}

#[test]
fn a_drag_that_grows_its_region_keeps_the_first_strokes_fine_detail() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Pot", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pot", "detail_cell": 0.01 }));
    let id = id_of(app, "Pot");
    // Ten strokes over the top, each half a radius past the last (the
    // second a radius and a half past the first): every one grows the one
    // Sculptris region.
    let (r, cell) = (0.03f32, 0.004);
    let at = |x: f32| Vec3::new(x, (0.25 - x * x).sqrt(), 0.0);
    // Points only the first stroke reaches (behind it, against the drag),
    // more than a coarse cell from the rest: where the coarse grid under
    // them never changes again.
    let probes: Vec<Vec3> = (0..12).map(|k| at(-0.6 * r) + Vec3::new(0.0, (k / 4) as f32 * 0.004 - 0.004, (k % 4) as f32 * 0.006 - 0.009)).collect();
    // The surface there, and what the fine level adds to it.
    let read = |app: &ClideApp| -> Vec<(f32, f32)> {
        let clay = app.scene.get(id).unwrap().clay.as_ref().unwrap();
        let g = clay.detail.as_ref().unwrap().grid();
        probes.iter().map(|p| (g.offset(*p), g.levels()[0].grid.level_offset(*p))).collect()
    };
    let (mut first, mut regions) = (Vec::new(), Vec::new());
    for k in 0..10 {
        let p = at(if k == 0 { 0.0 } else { (k + 2) as f32 * r * 0.5 });
        ok(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": [{ "brush": "inflate", "points": [p.to_array()], "radius": r, "strength": 0.5, "cell": cell }] }));
        let now = app.scene.get(id).unwrap().clay.as_ref().unwrap().detail_regions();
        assert_eq!(now.len(), 1, "{now:?}");
        regions.push(now[0].clone());
        if k == 0 {
            first = read(app);
            // The fine level holds it (not only the coarse grid).
            let clay = app.scene.get(id).unwrap().clay.as_ref().unwrap();
            assert!(clay.detail.as_ref().unwrap().grid().levels()[0].grid.brick_count() > 0);
            assert!(first.iter().all(|(_, fine)| fine.abs() > 1e-4), "the probes miss the first stroke's fine detail: {first:?}");
        }
    }
    assert!(regions.windows(2).filter(|w| w[0] != w[1]).count() >= 5, "the region hardly grew: {regions:?}");
    let last = read(app);
    // Exactly: a relayout is recorded as its regions (the grid is the
    // record's own replay), not rounded as a file keeps a whole grid.
    assert_eq!(first, last, "the first stroke's detail changed");
    // Each growth is a regions record, and none keeps a grid.
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Pot", "detail": true }));
    let kinds: Vec<&str> = log["records"].as_array().unwrap().iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(kinds.iter().filter(|k| **k == "regions").count() >= 5 && !kinds.contains(&"opaque"), "{kinds:?}");
    // The ledger replays to the same grid, region changes and all.
    let replay = ok(app, ctx, tx, "stroke_edit", json!({ "target": "Pot", "action": "replay" }));
    assert!(replay["mismatched"].as_array().is_none_or(|m| m.is_empty()), "{replay}");
}

#[test]
fn a_sculptris_drag_the_saved_budget_refuses_leaves_the_scene_as_it_was() {
    // Relayouts keep their regions, not the grid, so a plain clay can no
    // longer reach the saved budget by growing regions (it is held to the
    // per-grid caps, about 270 MB in a file). A clay with morphs can: each
    // morph writes its grids again in a file. Here 31 morphs share one
    // padded grid (68 MB in memory, a little under 1 GiB saved), and a
    // Sculptris drag grows the clay's own grid until a stroke would pass
    // the budget: that call is refused and changes nothing.
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Pot", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pot", "detail_cell": 0.01 }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": [{ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.05 }] }));
    let morphs = crate::layers::MAX_MORPHS - 1;
    for k in 0..morphs {
        ok(app, ctx, tx, "morph", json!({ "target": "Pot", "action": "store", "name": format!("m{k}") }));
    }
    let id = id_of(app, "Pot");
    // The padding: one grid all the morphs share, far from the surface,
    // sized to leave about 60 KB of the budget (bricks inflate to 1,036
    // bytes each in a file, once per morph).
    let clay = app.scene.get(id).unwrap().clay.clone().unwrap();
    let mut probe = clay.clone();
    for m in &mut crate::ledger::stack_mut(&mut probe).morphs {
        m.base = Some(crate::clay::Detail::new(0.01));
    }
    let left = crate::layers::MAX_SAVED_BYTES - crate::layers::saved_bytes(&probe);
    let n = ((left - 60_000) / (1036 * morphs as u64)) as i32;
    let mut pad = crate::clay::Detail::new(0.01);
    let side = (n as f32).sqrt().ceil() as i32;
    for k in 0..n {
        let (x, z) = (k % side, k / side);
        for i in 0..512 {
            pad.grid_mut().set([(x + 20) * 8 + i % 8, -40 * 8 + i / 8 % 8, z * 8 + i / 64], 0.001);
        }
    }
    let mut clay = clay;
    for m in &mut crate::ledger::stack_mut(&mut clay).morphs {
        m.base = Some(pad.clone());
    }
    app.scene.get_mut(id).unwrap().clay = Some(clay);
    let saved = crate::layers::saved_bytes(app.scene.get(id).unwrap().clay.as_ref().unwrap());
    assert!(saved < crate::layers::MAX_SAVED_BYTES && saved + 120_000 > crate::layers::MAX_SAVED_BYTES, "{saved}");
    // The drag: each stroke half a radius past the last, each growing the
    // one Sculptris region, until the budget refuses one.
    let (r, cell) = (0.03f32, 0.004);
    let at = |x: f32| Vec3::new(x, (0.25 - x * x).sqrt(), 0.0);
    let mut refused = None;
    for k in 0..40 {
        let before = app.scene.get(id).unwrap().clay.clone().unwrap();
        let p = at(-0.3 + k as f32 * r * 0.5);
        let (reply, err) = super::recorder::tests::mcp(app, ctx, tx, "sculpt", json!({ "target": "Pot", "strokes": [{ "brush": "inflate", "points": [p.to_array()], "radius": r, "strength": 0.5, "cell": cell }] }));
        let after = app.scene.get(id).unwrap().clay.clone().unwrap();
        if err {
            refused = Some((k, reply, before, after));
            break;
        }
        assert_ne!(before.detail_regions(), after.detail_regions(), "stroke {k} grew the region");
    }
    let Some((k, reply, before, after)) = refused else { panic!("40 strokes and the budget never refused one") };
    eprintln!("the saved budget refused stroke {k} of the drag: {reply}");
    assert!(k >= 1, "the first stroke fit");
    assert!(reply.to_string().contains("saved file"), "{reply}");
    // Nothing changed: the same grid, regions, ledgers and morphs.
    assert!(after.detail == before.detail && after.sculpt == before.sculpt, "the refused call changed the clay");
    assert_eq!(after.detail_regions(), before.detail_regions());
    assert_eq!(crate::layers::saved_bytes(&after), crate::layers::saved_bytes(&before));
    let log = ok(app, ctx, tx, "stroke_log", json!({ "target": "Pot", "limit": 1, "detail": true }));
    assert_eq!(log["head"], json!(crate::ledger::ledger::<crate::clay::Detail>(&before).head()));
}

#[test]
fn matcap_and_ao_settings_from_agents_and_the_display_menu() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let r = ok(app, ctx, tx, "set_display", json!({ "shading": "matcap" }));
    assert_eq!((r["shading"].as_str(), r["matcap"].as_str()), (Some("Matcap"), Some("clay")), "{r}");
    let r = ok(app, ctx, tx, "set_display", json!({ "matcap": "red_wax" }));
    assert_eq!(r["matcap"], "red_wax");
    let r = ok(app, ctx, tx, "set_display", json!({ "shading": "shaded" }));
    assert!(r["matcap"].is_null());
    let (e, err) = super::recorder::tests::mcp(app, ctx, tx, "set_display", json!({ "matcap": "/no/such/matcap.png" }));
    assert!(err && e.to_string().contains("built in"), "{e}");
    // ao and ao_radius: ambient_occlusion's settings.
    let r = ok(app, ctx, tx, "set_display", json!({ "ao": false }));
    assert_eq!(r["ambient_occlusion"]["enabled"], false, "{r}");
    let r = ok(app, ctx, tx, "set_display", json!({ "ao": true, "ao_radius": 0.3 }));
    assert_eq!(r["ambient_occlusion"]["enabled"], true);
    assert!((r["ambient_occlusion"]["radius"].as_f64().unwrap() - 0.3).abs() < 1e-6, "{r}");
    // The Display > MatCap menu sends the call an agent would (listed in
    // ui_activity), with the same effect.
    app.send_ui_call("set_display", json!({ "shading": "matcap", "matcap": "skin" }));
    settle(app, ctx);
    assert_eq!(app.matcap.as_ref().map(|m| m.name.as_str()), Some("skin"));
    let scene = ok(app, ctx, tx, "get_scene", json!({ "detail": true }));
    let activity = scene["ui_activity"].as_array().unwrap();
    assert!(activity.iter().any(|a| a["tool"] == "set_display"), "{activity:?}");
}

#[test]
fn a_sculpt_tool_on_a_clay_draws_it_in_chunks() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    let id = id_of(app, "Ball");
    settle(app, ctx);
    assert!(!app.meshes.chunked(id), "no session yet");
    ok(app, ctx, tx, "select_object", json!({ "target": "Ball" }));
    ok(app, ctx, tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "inflate", "radius": 0.1 }));
    settle(app, ctx);
    assert!(app.meshes.chunked(id), "the Sculpt tool on the selected clay starts a session");
    // An agent stroke keeps it, and reports what it dirtied.
    let r = ok(app, ctx, tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.08 }] }));
    assert!(r["strokes"][0]["dirty_chunks"].as_u64().is_some_and(|n| n > 0) || r["strokes"][0]["dirty_chunks"].is_null(), "{r}");
    settle(app, ctx);
    let clay = ok(app, ctx, tx, "get_clay", json!({ "target": "Ball" }));
    assert_eq!(clay["session_cache"]["chunked"], true, "{clay}");
    assert!(clay["session_cache"]["bricks"].as_u64().unwrap() > 0);
}

#[test]
fn an_agent_sculpt_of_ten_strokes_replies_in_time() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let _serial = crate::harness::live::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    // The 1.65M-triangle fixture: the hard figure at a 2 mm detail cell.
    let figure = crate::harness::figure(96, true);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Figure", "resolution": 96, "blobs": figure.blobs }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Figure", "detail_cell": 0.002 }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Figure", "strokes": [{ "brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.03 }] }));
    settle(app, ctx);
    let strokes: Vec<Value> = crate::harness::live::spots().iter().take(10).map(|p| json!({ "brush": "standard", "points": [p], "radius": 0.03, "strength": 0.4, "snap": true })).collect();
    let (mut times, mut nexts) = (Vec::new(), Vec::new());
    for round in 0..3 {
        let strokes = strokes.clone();
        let (r, ms) = crate::harness::live::timed_under_load(|| ok(app, ctx, tx, "sculpt", json!({ "target": "Figure", "strokes": strokes, "label": format!("round {round}") })));
        assert_eq!(r["strokes"].as_array().unwrap().len(), 10);
        eprintln!("10-stroke sculpt on the 1.65M fixture: {ms:.0} ms (load-scaled), timing {}", r["timing_ms"]);
        times.push(ms);
        // The agent's next call waits for the regular remesh only: no tool
        // is on the clay, so its session's chunks neither hold the remesh
        // back nor make the call wait.
        let (_, next) = crate::harness::live::timed_under_load(|| ok(app, ctx, tx, "get_clay", json!({ "target": "Figure", "limit": 1 })));
        eprintln!("the next call (get_clay) after it: {next:.0} ms (load-scaled), building after: {:?}", app.meshes.building());
        nexts.push(next);
        settle(app, ctx);
    }
    times.sort_by(f64::total_cmp);
    nexts.sort_by(f64::total_cmp);
    crate::harness::perf::check("live: 10-stroke agent sculpt reply (1.65M triangles)", times[1], 300.0 * 1.5, json!({ "runs": times }));
    crate::harness::perf::check("live: the agent's next call after a 10-stroke sculpt (1.65M triangles)", nexts[1], 600.0, json!({ "runs": nexts }));
}

