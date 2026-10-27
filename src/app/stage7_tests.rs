//! Stage 7 acceptance (docs/SCULPT_PLAN.md): the interactive brush UI on
//! a headless editor driven by synthetic egui input (it skips without a
//! GPU adapter for the render state; nothing is drawn unless
//! `CLIDE_GPU=1`). Every UI action leaves a `ui_activity` request that,
//! sent through MCP on the scene before the action, gives the same scene
//! and the same reply.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::reply_json;
use super::recorder::tests::{mcp, ok, settle};
use crate::brush::spec::Brush;
use crate::clay::Detail;
use crate::ledger::{self, PressureSource, Source};
use crate::mcp::ToolCall;

const WINDOW: egui::Vec2 = vec2(1400.0, 900.0);

/// A frame with `mods` held.
fn frame_m(app: &mut ClideApp, ctx: &egui::Context, events: Vec<Event>, mods: Modifiers) {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW)), events, modifiers: mods, ..Default::default() };
    let _ = ctx.run(raw, |ctx| app.frame(ctx));
}

fn frame(app: &mut ClideApp, ctx: &egui::Context, events: Vec<Event>) {
    frame_m(app, ctx, events, Modifiers::NONE);
}

fn button(pos: Pos2, pressed: bool, mods: Modifiers) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: mods }
}

/// A headless editor with clay `Ball` (two blobs) close to the camera and
/// the Sculpt tool on; None without a GPU adapter.
fn editor() -> Option<(ClideApp, Sender<ToolCall>, egui::Context)> {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return None;
    };
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "name": "body", "center": [0, 0, 0], "radius": 0.5, "blend": 0.0 }, { "type": "sphere", "name": "cap", "center": [0, 0.45, 0], "radius": 0.2, "blend": 0.05 }] }));
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball" }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "standard", "radius": 0.1, "strength": 0.5 }));
    // An agent stroke first, so the clay's ledger exists before the UI's
    // (a ledger's first stroke seeds its head from a fresh uid).
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "points": [[0, 0, 0.5]], "radius": 0.08, "snap": true }] }));
    app.cams.persp.distance = 3.0;
    settle(&mut app, &ctx);
    Some((app, tx, ctx))
}

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).expect("the object")
}

fn clay(app: &ClideApp) -> crate::clay::Clay {
    app.scene.get(id_of(app, "Ball")).and_then(|o| o.clay.clone()).expect("the clay")
}

/// A horizontal drag across the middle of the view.
fn across(app: &ClideApp, dy: f32) -> Vec<Pos2> {
    let c = app.viewport_rect.center();
    (0..=24).map(|i| c + vec2(-72.0 + 6.0 * i as f32, dy + (i as f32 * 0.4).sin() * 4.0)).collect()
}

/// A left drag through `path` with `mods` held, one frame per sample,
/// with `extra` events each frame. The scene does not change while the
/// button is down. Returns whether a preview was drawn during the drag.
fn drag_with(app: &mut ClideApp, ctx: &egui::Context, path: &[Pos2], mods: Modifiers, extra: impl Fn(Pos2) -> Vec<Event>) -> bool {
    let scene = app.scene.clone();
    let mut previewed = false;
    let mut step = |app: &mut ClideApp, mut events: Vec<Event>, p: Pos2| {
        events.extend(extra(p));
        frame_m(app, ctx, events, mods);
        previewed |= app.capture.as_ref().is_some_and(|c| app.meshes.preview_shown(c.target).is_some());
    };
    step(app, vec![Event::PointerMoved(path[0])], path[0]);
    step(app, vec![button(path[0], true, mods)], path[0]);
    for &p in &path[1..] {
        step(app, vec![Event::PointerMoved(p)], p);
        // Let the worker catch up now and then, so previews show.
        std::thread::sleep(Duration::from_millis(3));
        assert!(app.scene == scene, "nothing applies during the drag");
    }
    step(app, vec![button(path[path.len() - 1], false, mods)], path[path.len() - 1]);
    assert!(app.scene == scene, "nothing applies until the request runs");
    previewed
}

fn drag(app: &mut ClideApp, ctx: &egui::Context, path: &[Pos2], mods: Modifiers) -> bool {
    drag_with(app, ctx, path, mods, |_| Vec::new())
}

/// A click at `at`.
fn click(app: &mut ClideApp, ctx: &egui::Context, at: Pos2) {
    frame(app, ctx, vec![Event::PointerMoved(at)]);
    frame(app, ctx, vec![button(at, true, Modifiers::NONE)]);
    frame(app, ctx, vec![button(at, false, Modifiers::NONE)]);
}

/// Drags a HUD slider from `from` to `to` (fractions across its track).
fn slide(app: &mut ClideApp, ctx: &egui::Context, key: &str, from: f32, to: f32) {
    frame(app, ctx, Vec::new());
    let r = *app.hud.rects.get(key).unwrap_or_else(|| panic!("the HUD shows {key}: {:?}", app.hud.rects.keys().collect::<Vec<_>>()));
    // The track is the slider's width; its label follows.
    let width = 130.0;
    let at = |f: f32| egui::pos2(r.left() + 6.0 + (width - 12.0) * f, r.center().y);
    frame(app, ctx, vec![Event::PointerMoved(at(from))]);
    frame(app, ctx, vec![button(at(from), true, Modifiers::NONE)]);
    for k in 1..=8 {
        let p = at(from + (to - from) * k as f32 / 8.0);
        frame(app, ctx, vec![Event::PointerMoved(p)]);
    }
    frame(app, ctx, vec![button(at(to), false, Modifiers::NONE)]);
}

/// The scene as a file holds it, without record times and sources (an
/// agent's resent request makes `agent` records where the UI's were `ui`).
fn content(scene: &crate::scene::Scene) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.remove("time");
                o.remove("source");
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(scene).expect("json");
    strip(&mut v);
    v
}

/// JSON equal up to numbers equal as f32 (a request read back from a
/// reply's text prints its f32s another way).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64().map(|v| v as f32) == y.as_f64().map(|v| v as f32),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// A reply without its timings (`timing_ms`, `composite_ms` ...).
fn without_timing(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| !k.ends_with("_ms"));
    }
    v
}

/// The editor before a UI action: its history step and Adjust Last's
/// strokes (editor state the action may use up).
struct Before {
    step: u64,
    last: HashMap<u64, ledger::LastDelta>,
    sent: usize,
}

fn before(app: &ClideApp) -> Before {
    Before { step: app.history.current(), last: app.last_deltas.clone(), sent: app.ui_activity.len() }
}

/// UI-to-tool parity (§4.3): the action left one `ui_activity` request
/// for `tool`; sent through MCP on the editor as it was before the action
/// (history and Adjust Last back where they were), it gives the same
/// scene and the same reply.
fn parity(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, pre: Before, tool: &str) -> Value {
    settle(app, ctx);
    assert_eq!(app.ui_activity.len(), pre.sent + 1, "one request: {}", app.status);
    let a = app.ui_activity.back().expect("sent");
    assert_eq!(a.tool, tool, "{}", a.request);
    let request = a.request.clone();
    let reply = without_timing(reply_json(app.jobs.result(a.job).expect("done").as_ref().unwrap_or_else(|e| panic!("{tool} failed: {e}"))));
    let after = content(&app.scene);
    let listed = ok(app, ctx, tx, "get_scene", json!({ "detail": true }))["ui_activity"].as_array().and_then(|l| l.last().cloned()).expect("listed");
    assert!(listed["tool"].as_str() == Some(tool) && same(&listed["request"], &request), "ui_activity lists it: {listed}");
    app.history_goto(pre.step).expect("back to before the action");
    app.last_deltas = pre.last;
    settle(app, ctx);
    let agent = without_timing(ok(app, ctx, tx, tool, request.clone()));
    settle(app, ctx);
    assert_eq!(agent, reply, "{tool}: the agent's reply equals the UI's");
    assert!(content(&app.scene) == after, "{tool}: the agent's call leaves the scene the UI's did ({request})");
    request
}

/// A sculpt drag previews as it records (nothing in the scene changes)
/// and its record equals the preview of the stroke as it ended; every dab
/// is on the surface; the request resent through MCP on the scene before
/// the drag gives the same record; undo takes the drag back in one
/// labelled step. Dab brushes (run a few dabs at a time; clay_polish too),
/// path brushes (run whole) and a stroke on a sculpt layer all check.
#[test]
fn a_drag_previews_what_it_commits_and_replays_through_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let id = id_of(&app, "Ball");
    for (n, (brush, layer)) in [("standard", None), ("clay_buildup", None), ("inflate", None), ("clay_polish", None), ("standard", Some("detail"))].into_iter().enumerate() {
        if let Some(l) = layer {
            ok(&mut app, &ctx, &tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": l, "amount": 0.8 }));
            ok(&mut app, &ctx, &tx, "sculpt_layer", json!({ "target": "Ball", "action": "select", "name": l }));
            ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "layer": l, "strokes": [{ "brush": "inflate", "points": [[0, 0.3, 0.4]], "radius": 0.05, "snap": true }] }));
        }
        ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": brush, "radius": 0.09 }));
        settle(&mut app, &ctx);
        let (pre, pre_scene, checks) = (before(&app), app.scene.clone(), app.preview_checks);
        let path = across(&app, -30.0 + 20.0 * n as f32);
        assert!(drag(&mut app, &ctx, &path, Modifiers::NONE), "{brush}: a preview showed during the drag");
        settle(&mut app, &ctx);
        assert_eq!(app.preview_checks.0, checks.0 + 1, "{brush}: the preview was checked against the record");
        assert_eq!(app.preview_checks.1, 0, "{brush}: the record equals the preview of the stroke as it ended");
        assert_eq!(app.meshes.preview_shown(id), None, "{brush}: the preview went once the record installed");
        let c = clay(&app);
        let l = layer.unwrap_or("base");
        let records = crate::layers::ledger_of::<Detail>(&c, l).unwrap().records().to_vec();
        let record = records.last().expect("the stroke").clone();
        assert_eq!((record.source, record.tool.as_deref()), (Source::Ui, Some("sculpt")));
        assert!(record.stats.changed_points > 0);
        assert_eq!(app.history.current(), pre.step + 1, "one history step");
        let label = app.history.list()["steps"][pre.step as usize]["label"].as_str().unwrap_or_default().to_string();
        assert!(label.starts_with(&format!("sculpt {brush}")) && label.ends_with("[ui]"), "{label}");
        // Every dab point is on the surface it was drawn on (within half
        // a cell of the field before the stroke).
        let before_clay = pre_scene.get(id).and_then(|o| o.clay.clone()).unwrap();
        let cell = before_clay.cell_size().unwrap();
        let field = crate::clay::Field::new(&crate::layers::composed_clay(&before_clay), cell);
        let (_, stroke) = record.stroke().expect("a stroke");
        let mut worst = 0f32;
        for p in stroke.dabs.iter().flat_map(|d| d.points.iter()) {
            worst = worst.max(field.sample(*p, &field.near(*p - 0.2, *p + 0.2)).0.abs());
        }
        eprintln!("{brush}{}: {} dabs, worst distance from the surface {:.4} ({:.2} cell)", layer.map(|l| format!(" on {l}")).unwrap_or_default(), stroke.dabs.len(), worst, worst / cell);
        assert!(worst <= 0.5 * cell, "{brush}: a dab {worst} from the surface (cell {cell})");
        let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
        let again = crate::layers::ledger_of::<Detail>(&clay(&app), l).unwrap().records().last().cloned().unwrap();
        assert_eq!((again.result_hash, again.source), (record.result_hash, Source::Agent));
        if layer.is_some() {
            assert_eq!(request["layer"], json!(l), "the selected layer is named in the request");
        }
        // Undo takes the stroke back: the scene from before the drag.
        app.undo();
        assert!(app.scene == pre_scene, "undo restores the pre-drag scene");
        app.redo();
        settle(&mut app, &ctx);
    }
}

/// Shift records smooth; Alt inverts; Ctrl paints the mask (the detail is
/// left alone), Ctrl+Alt erases it, Ctrl+Shift draws a mask lasso; the
/// Mask tool paints it and the Groups tool paints a polygroup.
#[test]
fn modifiers_and_the_mask_and_groups_tools_send_their_tools_requests() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let base_len = |app: &ClideApp| ledger::ledger::<Detail>(&clay(app)).len();
    let mask_len = |app: &ClideApp| ledger::ledger::<crate::mask::SculptMask>(&clay(app)).len();
    // Shift: smooth.
    let pre = before(&app);
    let path = across(&app, -20.0);
    drag(&mut app, &ctx, &path, Modifiers::SHIFT);
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
    assert_eq!(request["strokes"][0]["brush"], "smooth", "{request}");
    let r = ledger::ledger::<Detail>(&clay(&app)).records().last().cloned().unwrap();
    assert_eq!(r.stroke().map(|s| s.0.brush), Some(Brush::Smooth));
    // Alt: inverted.
    let pre = before(&app);
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path, Modifiers::ALT);
    assert_eq!(parity(&mut app, &ctx, &tx, pre, "sculpt")["strokes"][0]["invert"], true);
    // Ctrl: the mask, not the detail.
    let (detail, masks) = (base_len(&app), mask_len(&app));
    let pre = before(&app);
    let path = across(&app, 10.0);
    drag(&mut app, &ctx, &path, Modifiers::CTRL);
    let request = parity(&mut app, &ctx, &tx, pre, "mask");
    assert_eq!(request["actions"][0]["paint"][0]["brush"], "mask_paint");
    assert_eq!((base_len(&app), mask_len(&app)), (detail, masks + 1), "Ctrl writes the mask, not the detail");
    assert!(clay(&app).sculpt_mask.is_some());
    // Ctrl+Alt: erases.
    let pre = before(&app);
    let path = across(&app, 10.0);
    drag(&mut app, &ctx, &path, Modifiers::CTRL | Modifiers::ALT);
    assert_eq!(parity(&mut app, &ctx, &tx, pre, "mask")["actions"][0]["paint"][0]["invert"], true);
    // Ctrl+Shift: a lasso (the outline as drawn, through the camera).
    let c = app.viewport_rect.center();
    let ring: Vec<Pos2> = (0..=30).map(|k| c + vec2(60.0 * (k as f32 * 0.21).cos(), 40.0 * (k as f32 * 0.21).sin())).collect();
    let pre = before(&app);
    drag(&mut app, &ctx, &ring, Modifiers::CTRL | Modifiers::SHIFT);
    let request = parity(&mut app, &ctx, &tx, pre, "mask");
    let lasso = &request["actions"][0]["lasso"];
    assert!(lasso["camera"].is_object() && lasso["px"].as_array().is_some_and(|p| p.len() > 20) && lasso["mode"] == "add", "{request}");
    // The Mask tool paints it; the Groups tool paints a polygroup.
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "mask" }));
    let pre = before(&app);
    let path = across(&app, 30.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    parity(&mut app, &ctx, &tx, pre, "mask");
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "groups", "group": 7 }));
    let pre = before(&app);
    let path = across(&app, 30.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "group");
    assert_eq!(request["actions"][0]["paint"]["id"], 7);
    assert!(clay(&app).groups.as_ref().is_some_and(|g| g.data().counts().contains_key(&7)), "group 7 painted");
    // Every stroke's record equalled the preview of the stroke as it ended
    // (smooth, inverted, mask paint and erase, mask tool, group paint).
    assert_eq!(app.preview_checks, (6, 0), "previews checked against their records (the lasso has none)");
}

/// Every other row of §4.3's table, driven through the HUD with the
/// pointer: the layer slider (previewed while it drags), the eye and the
/// record dot, group hide, a mask action, Adjust Last, the history
/// scrubber and Save brush; and the masked gizmo transform. Each request
/// resent through MCP on the scene before it gives the same scene and
/// reply.
#[test]
fn every_hud_control_sends_its_tools_request() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let id = id_of(&app, "Ball");
    ok(&mut app, &ctx, &tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "detail" }));
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "layer": "detail", "strokes": [{ "brush": "inflate", "points": [[0, 0.3, 0.4]], "radius": 0.06, "snap": true }] }));
    ok(&mut app, &ctx, &tx, "group", json!({ "target": "Ball", "actions": ["from_blobs"] }));
    settle(&mut app, &ctx);

    // The layer slider: previewed while it drags, one call on release.
    let pre = before(&app);
    frame(&mut app, &ctx, Vec::new());
    let r = app.hud.rects["amount:detail"];
    let at = |f: f32| egui::pos2(r.left() + 6.0 + 118.0 * f, r.center().y);
    frame(&mut app, &ctx, vec![Event::PointerMoved(at(0.75))]);
    frame(&mut app, &ctx, vec![button(at(0.75), true, Modifiers::NONE)]);
    for k in 1..=6 {
        frame(&mut app, &ctx, vec![Event::PointerMoved(at(0.75 - 0.05 * k as f32))]);
        assert!(app.meshes.preview_shown(id).is_some() && app.ui_activity.len() == pre.sent, "previewed, not sent, while it drags");
    }
    frame(&mut app, &ctx, vec![button(at(0.45), false, Modifiers::NONE)]);
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt_layer");
    assert_eq!(request["action"], "amount");
    let amount = request["amount"].as_f64().unwrap();
    assert!(amount > -0.6 && amount < 0.2, "{request}");
    assert_eq!(app.meshes.preview_shown(id), None, "the preview went once the call ran");

    // The eye, then the record dot.
    for (key, action) in [("eye:detail", "disable"), ("record:detail", "record")] {
        let pre = before(&app);
        frame(&mut app, &ctx, Vec::new());
        let at = app.hud.rects[key].center();
        click(&mut app, &ctx, at);
        let request = parity(&mut app, &ctx, &tx, pre, "sculpt_layer");
        assert_eq!(request["action"], action);
    }

    // Group hide.
    let pre = before(&app);
    frame(&mut app, &ctx, Vec::new());
    let key = app.hud.rects.keys().find(|k| k.starts_with("group:")).cloned().expect("a group row");
    let at = app.hud.rects[&key].center();
    click(&mut app, &ctx, at);
    let request = parity(&mut app, &ctx, &tx, pre, "group");
    assert!(request["actions"][0]["hide"].is_array(), "{request}");
    assert!(clay(&app).groups.as_ref().is_some_and(|g| !g.hidden().is_empty()));

    // The quality shade (D-d): the Shading menu's set_display call.
    let pre = before(&app);
    frame(&mut app, &ctx, Vec::new());
    let at = app.hud.rects["shade:curvature"].center();
    click(&mut app, &ctx, at);
    let request = parity(&mut app, &ctx, &tx, pre, "set_display");
    assert_eq!(request, json!({ "shade": "curvature" }));
    assert_eq!(app.meshes.shade, crate::quality::Shade::Curvature);
    ok(&mut app, &ctx, &tx, "set_display", json!({ "shade": "beauty" }));
    settle(&mut app, &ctx);

    // A mask action (Grow, after a mask exists).
    ok(&mut app, &ctx, &tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "min": 0.3 } }] }));
    settle(&mut app, &ctx);
    let pre = before(&app);
    frame(&mut app, &ctx, Vec::new());
    let at = app.hud.rects["mask:Grow"].center();
    click(&mut app, &ctx, at);
    parity(&mut app, &ctx, &tx, pre, "mask");

    // Adjust Last: the newest stroke at about half of itself.
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "inflate", "points": [[0.2, 0, 0.45]], "radius": 0.08, "snap": true }] }));
    settle(&mut app, &ctx);
    let pre = before(&app);
    slide(&mut app, &ctx, "adjust_last", 0.5, 0.25);
    let request = parity(&mut app, &ctx, &tx, pre, "stroke_edit");
    assert_eq!((request["action"].as_str(), request["last"].as_bool()), (Some("scale"), Some(true)), "{request}");
    assert!((request["factor"].as_f64().unwrap() - 0.5).abs() < 0.1, "{request}");

    // The history scrubber: back a few steps.
    let pre = before(&app);
    let (first, last) = app.history.bounds();
    let f = |n: u64| (n - first) as f32 / (last - first).max(1) as f32;
    slide(&mut app, &ctx, "history", f(pre.step), f(pre.step.saturating_sub(3).max(first)));
    let request = parity(&mut app, &ctx, &tx, pre, "history");
    assert_eq!(request["action"], "goto");
    assert!(request["index"].as_u64().unwrap() < app.history.bounds().1, "{request}");

    // Save brush.
    let pre = before(&app);
    (app.hud.save_name, app.hud.options) = ("MyBrush".into(), true);
    frame(&mut app, &ctx, Vec::new());
    let at = app.hud.rects["save"].center();
    click(&mut app, &ctx, at);
    let request = parity(&mut app, &ctx, &tx, pre, "brush_preset");
    assert_eq!((request["action"].as_str(), request["name"].as_str()), (Some("save"), Some("MyBrush")));
    assert!(app.scene.brushes.contains_key("MyBrush"));

    // The gizmo on a masked clay: one transform stroke on release.
    app.select_only(id);
    app.tool = super::Tool::Move;
    let at = |y: f64| transform_gizmo_egui::math::Transform { scale: glam::DVec3::ONE.into(), rotation: glam::DQuat::IDENTITY.into(), translation: glam::DVec3::new(0.0, y, 0.0).into() };
    settle(&mut app, &ctx);
    let pre = before(&app);
    app.masked_drag = Some((id, at(0.0), at(0.04)));
    frame(&mut app, &ctx, Vec::new());
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
    assert_eq!(request["strokes"][0]["brush"], "transform");
}

/// `brush_preset set_current` from MCP changes what the HUD shows, and the
/// next drag records those settings.
#[test]
fn an_agents_brush_shows_in_the_hud_and_goes_into_the_next_drag() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let set = json!({ "action": "set_current", "brush": "clay_buildup", "radius": 0.07, "strength": 0.35, "falloff": { "preset": "sharp", "focal_shift": 0.3 }, "symmetry": { "axes": ["x"] }, "pressure": { "size": "off" }, "lazy": { "radius": 8.0 } });
    let current = ok(&mut app, &ctx, &tx, "brush_preset", set);
    frame(&mut app, &ctx, Vec::new());
    assert_eq!(app.hud.shown, current, "the HUD shows the agent's brush");
    let falloff = |v: &Value| serde_json::from_value::<crate::brush::curve::Falloff>(v.clone()).unwrap();
    let want = falloff(&json!({ "preset": "sharp", "focal_shift": 0.3 }));
    assert_eq!(falloff(&app.hud.shown["falloff"]), want);
    let pre = before(&app);
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
    let s = &request["strokes"][0];
    assert_eq!((s["brush"].as_str(), s["radius"].as_f64(), s["strength"].as_f64()), (Some("clay_buildup"), Some(0.07f32 as f64), Some(0.35f32 as f64)), "{s}");
    assert_eq!((falloff(&s["falloff"]), &s["symmetry"], &s["pressure"]), (want, &json!({ "axes": ["x"] }), &json!({ "size": "off" })));
    // LazyMouse is in the samples, not the spec: a replay smooths nothing.
    assert!(s.get("lazy").is_none());
    let r = ledger::ledger::<Detail>(&clay(&app)).records().last().cloned().unwrap();
    let (spec, resolved) = r.stroke().unwrap();
    assert_eq!((spec.brush, resolved.instances), (Brush::ClayBuildup, 2), "mirrored");
    // The HUD's own controls go through the same handler: dragging Size.
    slide(&mut app, &ctx, "size", 0.6, 0.3);
    assert!(app.brush.radius < 0.07, "{}", app.brush.radius);
    assert_eq!(ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" }))["radius"].as_f64().map(|r| r as f32), Some(app.brush.radius));
}

/// A touch's force of 0.25 presses the brush to the size curve's value
/// at 0.25 (linear: a quarter of the radius), and the records say where
/// the pressure came from.
#[test]
fn touch_force_scales_the_radius_by_the_size_curve() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "edit_clay", json!({ "target": "Ball", "detail_cell": 0.005 }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "standard", "radius": 0.16, "pressure": { "size": "linear", "strength": "off" } }));
    settle(&mut app, &ctx);
    let touch = |p: Pos2| vec![Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(3), phase: egui::TouchPhase::Move, pos: p, force: Some(0.25) }];
    let pre = before(&app);
    let path = across(&app, 0.0);
    drag_with(&mut app, &ctx, &path, Modifiers::NONE, touch);
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
    assert_eq!(request["pressure_source"], "touch_force");
    assert!(request["strokes"][0]["frame"]["screen"]["px"].as_array().unwrap().iter().all(|p| p[2] == 0.25), "{request}");
    let r = ledger::ledger::<Detail>(&clay(&app)).records().last().cloned().unwrap();
    assert_eq!(r.pressure_source, Some(PressureSource::TouchForce));
    let reply = reply_json(app.jobs.result(app.ui_activity.back().unwrap().job).unwrap().as_ref().unwrap());
    let used = reply["strokes"][0]["radius_used"].as_f64().unwrap() as f32;
    let want = 0.16 * 0.25;
    eprintln!("touch force 0.25: radius_used {used} (curve {want})");
    assert!((used - want).abs() <= 0.05 * want, "radius_used {used}, curve value {want}");
}

/// The brush ring is drawn on the surface, depth-tested: where it shows it
/// stands out from what is behind it, and where the surface hides it
/// nothing of it shows. Needs `CLIDE_GPU=1`.
#[test]
#[ignore = "needs a GPU: CLIDE_GPU=1 cargo test --release -- --ignored brush_ring"]
fn the_brush_ring_is_depth_tested_on_the_surface() {
    use crate::harness::image::{View, gpu_enabled};
    use crate::render::test_support;
    if !gpu_enabled() {
        return;
    }
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    scene.get_mut(id).unwrap().clay = Some(serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.6 }], "resolution": 64 })).unwrap());
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    let rs = test_support::headless().expect("CLIDE_GPU=1 needs an adapter");
    let view = View::FRONT;
    let size = 512;
    let vp = glam::Mat4::perspective_rh(view.fov.to_radians(), 1.0, 0.05, 20.0) * glam::Mat4::look_at_rh(view.eye, view.target, Vec3::Y);
    // A ring on the sphere's silhouette: half of it lies behind the sphere.
    let toward = (view.eye - view.target).normalize();
    let side = toward.cross(Vec3::Y).normalize();
    let center = side * 0.6;
    let normal = side;
    let cursor = crate::render::Cursor { center, normal, radius: 0.25, focal: 0.5, color: [1.0, 1.0, 1.0, 1.0] };
    let mut cb = crate::app::scene_callback_of(&scene, &meshes, vp, view.eye, false, false);
    let plain = test_support::render(&rs, &cb, size, size, test_support::BACKGROUND).unwrap();
    cb.cursor = Some(cursor);
    let ringed = test_support::render(&rs, &cb, size, size, test_support::BACKGROUND).unwrap();
    let lum = |c: egui::Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    let changed = (0..plain.pixels.len()).filter(|&i| plain.pixels[i] != ringed.pixels[i]).count();
    assert!(changed > 100, "the ring shows ({changed} pixels)");
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("golden");
    let _ = std::fs::create_dir_all(&dir);
    for (name, img) in [("brush_ring", &ringed), ("brush_ring_plain", &plain)] {
        let rgb: Vec<u8> = img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
        let _ = image::save_buffer(dir.join(format!("{name}.png")), &rgb, size, size, image::ColorType::Rgb8);
    }
    // The ring's points: where one shows (the surface does not hide it and
    // its neighbours), the line through it stands out by 30 or more; where
    // the surface hides it, nothing of it shows.
    let (u, v) = normal.any_orthonormal_pair();
    let lift = normal * 0.25 * 0.02;
    let hidden = |p: Vec3| {
        let d = (p - view.eye).normalize();
        let (b, c) = (view.eye.dot(d), view.eye.length_squared() - 0.6 * 0.6);
        let disc = b * b - c;
        disc > 0.0 && (-b - disc.sqrt()) < (p - view.eye).length() - 0.02
    };
    // The segment ends the cursor draws (64 a ring, as `render::Cursor`).
    let (mut hidden_checked, mut shown_checked) = (0, 0);
    for k in 0..64 {
        let a = |k: i32| (k as f32 / 64.0) * std::f32::consts::TAU;
        let at = |k: i32| center + lift + (u * a(k).cos() + v * a(k).sin()) * 0.25;
        let c = vp * at(k).extend(1.0);
        let (x, y) = ((c.x / c.w + 1.0) * 0.5 * size as f32, (1.0 - c.y / c.w) * 0.5 * size as f32);
        if !(2.0..size as f32 - 2.0).contains(&x) || !(2.0..size as f32 - 2.0).contains(&y) {
            continue;
        }
        // The largest change around the point (a line covers about one
        // pixel, anti-aliased).
        let near = |r: i32| (-r..=r).flat_map(|dy| (-r..=r).map(move |dx| (y as i32 + dy) as usize * size as usize + (x as i32 + dx) as usize)).map(|i| (lum(plain.pixels[i]) - lum(ringed.pixels[i])).abs()).fold(0.0, f32::max);
        let all_hidden = hidden(at(k - 1)) && hidden(at(k)) && hidden(at(k + 1));
        let none_hidden = !hidden(at(k - 1)) && !hidden(at(k)) && !hidden(at(k + 1));
        if all_hidden {
            hidden_checked += 1;
            assert!(near(1) < 4.0, "a hidden ring point shows at {x},{y}: {}", near(1));
        } else if none_hidden {
            shown_checked += 1;
            assert!(near(1) >= 30.0, "a ring point at {x},{y} differs by only {} from what is behind it", near(1));
        }
    }
    eprintln!("ring: {changed} pixels changed; {shown_checked} shown points stand out by 30+, {hidden_checked} hidden points show nothing");
    assert!(hidden_checked > 8 && shown_checked > 8, "{hidden_checked} hidden and {shown_checked} shown points checked");
}

/// The HUD as drawn matches its golden (mean error 2/255 or less). Needs
/// `CLIDE_GPU=1`; `BLESS=1` rewrites the golden.
#[test]
#[ignore = "needs a GPU: CLIDE_GPU=1 cargo test --release -- --ignored hud_golden"]
fn hud_golden() {
    if !crate::harness::image::gpu_enabled() {
        return;
    }
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "detail" }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "clay_buildup", "radius": 0.07, "strength": 0.35, "symmetry": { "axes": ["x"] } }));
    settle(&mut app, &ctx);
    let rs = crate::render::test_support::headless().unwrap();
    let hud_ctx = egui::Context::default();
    super::theme::apply(&hud_ctx);
    let size = [300u32, 560];
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32))), ..Default::default() };
    // Two passes: the first lays fonts out (and uploads the font atlas,
    // which the second's painting needs too).
    let mut textures = egui::TexturesDelta::default();
    let mut out = None;
    for _ in 0..2 {
        let mut o = hud_ctx.run(raw.clone(), |c| {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(super::theme::PANEL).inner_margin(6.0)).show(c, |ui| app.hud(ui));
        });
        textures.append(std::mem::take(&mut o.textures_delta));
        out = Some(o);
    }
    let mut out = out.unwrap();
    out.textures_delta = textures;
    let img = crate::render::test_support::paint_ui(&rs, &hud_ctx, out, size).unwrap();
    crate::harness::image::assert_golden("hud", &img, 2.0, 40);
}

/// A 2 s, 120-sample drag (r = 40 cells) on the 1.65M-triangle fixture:
/// from each sample to its preview's chunks being ready to draw, p95
/// within 33 ms (over the machine's load; the GPU upload of a dab's chunks
/// that follows is `harness::live`'s check, under 1 ms). Heavy; runs
/// alone among the heavy checks (`harness::live::SERIAL`). The suite's own
/// parallel tests load the machine past what dividing by the measured
/// load makes up for, so the timings are asserted only with
/// `CLIDE_PERF=1` (a quiet machine: `CLIDE_PERF=1 cargo test --release
/// reaches_the_screen`); otherwise they are logged to target/perf.json
/// and the run checks what does not depend on the load.
#[test]
fn a_drag_on_the_big_fixture_reaches_the_screen_in_time() {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return;
    };
    let _serial = crate::harness::live::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    let figure = crate::harness::figure(96, true);
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Figure", "resolution": 96, "blobs": figure.blobs }));
    ok(&mut app, &ctx, &tx, "edit_clay", json!({ "target": "Figure", "detail_cell": 0.002 }));
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Figure", "strokes": [{ "brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.03 }] }));
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Figure" }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "standard", "radius": 0.08, "strength": 0.4 }));
    ok(&mut app, &ctx, &tx, "set_camera", json!({ "look_at": [0.0, 1.2, 0.0], "distance": 1.2, "yaw": 0, "pitch": 0 }));
    settle(&mut app, &ctx);
    let id = id_of(&app, "Figure");
    let mut tris = 0;
    if let Some(c) = app.meshes.sculpt_chunks().iter().filter(|(k, _)| matches!(k, crate::mesh::MeshKey::Chunk(o, _) if *o == id)).map(|(_, c)| c.indices.len() / 3).reduce(|a, b| a + b) {
        tris = c;
    }
    // Over the chest: 120 samples in 2 s, frames in between as the
    // window draws them (when the worker or a chunk build asks).
    let c = app.viewport_rect.center();
    let path: Vec<Pos2> = (0..120).map(|i| c + vec2(60.0 * (i as f32 * 0.053).cos(), -20.0 + 60.0 * (i as f32 * 0.053).sin())).collect();
    app.preview_latency.clear();
    app.preview_work_ms.clear();
    // How loaded the machine is (the suite runs alongside), before and
    // after: the timings are over it (1 on a quiet machine and strict runs).
    let load_before = crate::harness::live::load();
    let start = Instant::now();
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true, Modifiers::NONE)]);
    assert!(app.capture.as_ref().is_some_and(|c| c.live.is_some()), "a preview runs: {}", app.status);
    let mut frames = Vec::new();
    let step = |app: &mut ClideApp, events: Vec<Event>| frame(app, &ctx, events);
    for (k, p) in path.iter().enumerate().skip(1) {
        let due = start + Duration::from_secs_f64(2.0 * k as f64 / 120.0);
        let t = Instant::now();
        step(&mut app, vec![Event::PointerMoved(*p)]);
        frames.push(t.elapsed().as_secs_f64() * 1000.0);
        while Instant::now() < due {
            step(&mut app, Vec::new());
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // The last samples reach the screen too.
    let tail = Instant::now();
    while app.capture.as_ref().and_then(|c| c.live.as_ref()).is_some_and(|l| !l.sent.is_empty()) && tail.elapsed() < Duration::from_secs(5) {
        step(&mut app, Vec::new());
        std::thread::sleep(Duration::from_millis(1));
    }
    let load = (load_before + crate::harness::live::load()) * 0.5;
    let mut lat: Vec<f64> = app.preview_latency.iter().map(|l| l / load).collect();
    let mut work = app.preview_work_ms.clone();
    work.sort_by(f64::total_cmp);
    eprintln!("preview worker: {} results, p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; last chunk build {:?}", work.len(), work[work.len() / 2], work[(work.len() * 95 / 100).min(work.len() - 1)], work[work.len() - 1], app.meshes.chunk_timing());
    frame(&mut app, &ctx, vec![button(path[path.len() - 1], false, Modifiers::NONE)]);
    settle(&mut app, &ctx);
    assert_eq!(app.preview_checks, (1, 0), "the record equals the preview of the stroke as it ended");
    lat.sort_by(f64::total_cmp);
    frames.sort_by(f64::total_cmp);
    let p = |v: &[f64], q: f64| v[((v.len() as f64 * q).ceil() as usize).saturating_sub(1).min(v.len() - 1)];
    eprintln!("drag on the {tris}-triangle fixture (load x{load:.2}): {} samples drawn, sample to draw-ready chunks p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; window frame with a sample p50 {:.1} ms, p95 {:.1} ms", lat.len(), p(&lat, 0.5), p(&lat, 0.95), p(&lat, 1.0), p(&frames, 0.5), p(&frames, 0.95));
    let strict = crate::harness::perf::strict();
    // Latest wins: a loaded machine draws fewer of the samples.
    assert!(lat.len() >= if strict { 60 } else { 10 }, "most samples reached the screen: {}", lat.len());
    let timing = |name: &str, ms: f64, bound: f64, extra: Value| if strict { crate::harness::perf::check(name, ms, bound, extra) } else { crate::harness::perf::log(name, ms, extra) };
    timing("live: drag sample to draw-ready chunks p95 (1.65M triangles)", p(&lat, 0.95), 33.0, json!({ "samples": lat.len(), "p50": p(&lat, 0.5), "load": load, "bound_ms": 33.0 }));
    timing("live: window frame with a drag sample p95 (1.65M triangles)", p(&frames, 0.95), 16.0, json!({ "p50": p(&frames, 0.5), "bound_ms": 16.0 }));
}

/// A stroke released while the clay's mesh is out of date (the stroke
/// before it just installed) waits for the background build instead of
/// building it on the window's thread: no `sync` there waits on a build.
#[test]
fn a_stroke_after_an_install_waits_for_the_mesh_off_the_window_thread() {
    let Some((mut app, _tx, ctx)) = editor() else { return };
    let id = id_of(&app, "Ball");
    let records = crate::layers::ledger_of::<Detail>(&clay(&app), "base").unwrap().records().len();
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    assert_eq!(app.strokes.len(), 1, "the stroke waits to be sent");
    // As if the stroke before it had installed this frame: the clay's
    // shape changed and no build has started yet.
    app.scene.get_mut(id).and_then(|o| o.clay.as_mut()).unwrap().blobs[1].blend = 0.06;
    assert!(app.meshes.stale(&app.scene) && !app.meshes.busy());
    app.meshes.blocking_joins = 0;
    frame(&mut app, &ctx, Vec::new());
    assert_eq!(app.strokes.len(), 1, "not sent while the mesh is stale");
    settle(&mut app, &ctx);
    assert_eq!(app.meshes.blocking_joins, 0, "nothing on the window's thread waited for a mesh build");
    assert_eq!(crate::layers::ledger_of::<Detail>(&clay(&app), "base").unwrap().records().len(), records + 1, "the stroke went once the mesh was built");
}

/// A layer amount slider drag the HUD stops drawing midway (the tool
/// switched, or an agent selected another object) drops its preview and
/// sends nothing; held still while drawn, it keeps its preview.
#[test]
fn a_layer_amount_drag_cut_short_drops_its_preview() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let id = id_of(&app, "Ball");
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Cube", "name": "Box", "position": [3, 0, 0] }));
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball" }));
    ok(&mut app, &ctx, &tx, "sculpt_layer", json!({ "target": "Ball", "action": "add", "name": "detail" }));
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "layer": "detail", "strokes": [{ "brush": "inflate", "points": [[0, 0.3, 0.4]], "radius": 0.06, "snap": true }] }));
    settle(&mut app, &ctx);
    for cut in ["tool", "selection"] {
        let sent = app.ui_activity.len();
        frame(&mut app, &ctx, Vec::new());
        let r = app.hud.rects["amount:detail"];
        let at = |f: f32| egui::pos2(r.left() + 6.0 + 118.0 * f, r.center().y);
        frame(&mut app, &ctx, vec![Event::PointerMoved(at(0.75))]);
        frame(&mut app, &ctx, vec![button(at(0.75), true, Modifiers::NONE)]);
        for k in 1..=3 {
            frame(&mut app, &ctx, vec![Event::PointerMoved(at(0.75 - 0.05 * k as f32))]);
        }
        // Held still: the drag goes on, previewed.
        for _ in 0..3 {
            frame(&mut app, &ctx, Vec::new());
        }
        assert!(app.meshes.preview_shown(id).is_some() && app.hud.amount.is_some(), "{cut}: previewed while it drags");
        match cut {
            "tool" => app.tool = super::Tool::Move,
            _ => {
                ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Box" }));
            }
        }
        for _ in 0..3 {
            frame(&mut app, &ctx, Vec::new());
        }
        assert_eq!(app.meshes.preview_shown(id), None, "{cut}: the preview went with the HUD");
        assert!(app.hud.amount.is_none(), "{cut}: the drag was dropped");
        frame(&mut app, &ctx, vec![button(at(0.6), false, Modifiers::NONE)]);
        settle(&mut app, &ctx);
        let tools: Vec<&str> = app.ui_activity.iter().skip(sent).map(|a| a.tool).collect();
        assert!(!tools.contains(&"sculpt_layer"), "{cut}: no amount call was sent: {tools:?}");
        assert_eq!(app.meshes.preview_shown(id), None, "{cut}: no preview came back");
        // Back to the Sculpt tool on the ball.
        app.tool = super::Tool::Sculpt;
        ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball" }));
        settle(&mut app, &ctx);
    }
}

/// Two quick strokes in a row over the same place: the second is pressed
/// before the first installs, so its preview casts its samples on the
/// regular mesh without the first stroke, where its request (sent once the
/// mesh is rebuilt) casts on the mesh with it. That preview is marked
/// inexact (not checked against its record), so no false "differed"
/// warning reaches the status line.
#[test]
fn a_second_quick_stroke_previews_without_a_false_mismatch() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "standard", "radius": 0.09, "strength": 0.8 }));
    settle(&mut app, &ctx);
    let records = crate::layers::ledger_of::<Detail>(&clay(&app), "base").unwrap().records().len();
    let checks = app.preview_checks;
    let path = across(&app, 0.0);
    assert!(drag(&mut app, &ctx, &path, Modifiers::NONE), "the first stroke previewed");
    // The second goes at once, over the first (which installs during it:
    // `drag` would refuse that).
    let mut stale_mesh = None;
    let mut previewed = false;
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true, Modifiers::NONE)]);
    for &p in &path[1..] {
        frame(&mut app, &ctx, vec![Event::PointerMoved(p)]);
        previewed |= app.capture.as_ref().is_some_and(|c| c.live.is_some() && app.meshes.preview_shown(c.target).is_some());
        std::thread::sleep(Duration::from_millis(3));
    }
    frame(&mut app, &ctx, vec![button(path[path.len() - 1], false, Modifiers::NONE)]);
    assert!(previewed, "the second stroke previewed");
    // When the second is sent, the mesh its request casts on is not the
    // one its preview cast on.
    let start = Instant::now();
    while app.lives.len() + app.strokes.len() > 0 && start.elapsed() < Duration::from_secs(60) {
        frame(&mut app, &ctx, Vec::new());
        if let Some(l) = app.lives.iter().find(|l| l.job.is_some() && l.cast_on.is_some()) {
            stale_mesh = Some(stale_mesh.unwrap_or(false) | l.inexact());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(&mut app, &ctx);
    assert_eq!(crate::layers::ledger_of::<Detail>(&clay(&app), "base").unwrap().records().len(), records + 2, "both strokes applied");
    eprintln!("two quick strokes: preview checks {:?} (from {checks:?}), second marked inexact: {stale_mesh:?}; status: {}", app.preview_checks, app.status);
    assert_eq!(app.preview_checks.1, checks.1, "no preview differed from its record: {}", app.status);
    assert!(!app.status.contains("differed"), "{}", app.status);
    // The first was checked (its mesh was current); the second was not.
    assert_eq!(app.preview_checks.0, checks.0 + 1, "only the first preview was checked");
    assert_eq!(stale_mesh, Some(true), "the second preview was marked inexact when it was sent");
}

/// A symmetric drag's preview follows each copy's new dabs (not the whole
/// stroke again per sample), says it is inexact, is not checked against
/// its record and goes once the record is in; the record is the stroke as
/// drawn.
#[test]
fn a_symmetric_drag_previews_without_rerunning_the_stroke() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let id = id_of(&app, "Ball");
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "standard", "radius": 0.08, "symmetry": { "axes": ["x"] } }));
    settle(&mut app, &ctx);
    let checks = app.preview_checks;
    let path = across(&app, -20.0);
    assert!(drag(&mut app, &ctx, &path, Modifiers::NONE), "a preview showed during the drag");
    // The worker's flags outlive the stroke (it goes when the record is in).
    let live = app.strokes.back().and_then(|s| s.live.as_ref()).expect("the released stroke's preview");
    let (inexact, dabs_run) = (live.inexact.clone(), live.dabs_run.clone());
    settle(&mut app, &ctx);
    let c = clay(&app);
    let record = crate::layers::ledger_of::<Detail>(&c, "base").unwrap().records().last().cloned().unwrap();
    let (_, stroke) = record.stroke().expect("a stroke");
    assert_eq!((record.source, stroke.instances), (Source::Ui, 2));
    let (total, ran) = (stroke.dabs.len(), dabs_run.load(std::sync::atomic::Ordering::Relaxed));
    eprintln!("symmetric drag: {total} dabs, {ran} run by the preview, inexact {}", inexact.load(std::sync::atomic::Ordering::Relaxed));
    assert!(inexact.load(std::sync::atomic::Ordering::Relaxed), "the per-copy preview says it is inexact");
    assert_eq!(app.preview_checks, checks, "an inexact preview is not checked against its record");
    // Every dab once since the stroke went per copy (plus those run
    // before), not the stroke so far again per sample.
    assert!(ran >= total && ran < 2 * total, "{ran} dabs run for a stroke of {total}");
    assert_eq!(app.meshes.preview_shown(id), None, "the preview went once the record installed");
}

/// Grab and Snakehook drag across the view plane at the press's depth:
/// grab sends the press point and the delta to the mouse, snakehook the
/// path; both replay through MCP.
#[test]
fn grab_and_snakehook_drag_across_the_view_plane() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let c = app.viewport_rect.center();
    let up: Vec<Pos2> = (0..=12).map(|i| c + vec2(10.0, 20.0 - 4.0 * i as f32)).collect();
    for brush in ["grab", "snakehook"] {
        ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": brush, "radius": 0.12 }));
        settle(&mut app, &ctx);
        let pre = before(&app);
        drag(&mut app, &ctx, &up, Modifiers::NONE);
        let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
        let s = &request["strokes"][0];
        assert_eq!(s["brush"], brush, "{s}");
        assert!(s.get("frame").is_none(), "world points: {s}");
        let points = s["points"].as_array().unwrap();
        if brush == "grab" {
            assert_eq!(points.len(), 1);
            let d = Vec3::from(serde_json::from_value::<[f32; 3]>(s["delta"].clone()).unwrap());
            // The drag went up the screen: the pull goes up (the view
            // looks a little down at the clay), across the view plane.
            assert!(d.y > 0.02 && d.length() < 0.3, "{d}");
            let view_dir = (app.cams.target(crate::camera::ViewKind::Persp) - app.cams.eye(crate::camera::ViewKind::Persp)).normalize();
            assert!(d.normalize().dot(view_dir).abs() < 1e-3, "in the view plane: {d}");
        } else {
            assert!(points.len() > 5, "{s}");
        }
        let r = ledger::ledger::<Detail>(&clay(&app)).records().last().cloned().unwrap();
        assert!(r.stats.changed_points > 0, "{brush} moved the clay");
    }
}

/// The brush hotkeys: S held while the mouse moves sideways sizes the
/// brush, U its strength, O its focal shift, X turns X symmetry on and
/// off; each goes through `brush_preset set_current` (the agent reads it).
#[test]
fn brush_hotkeys_drag_size_strength_and_focal_and_toggle_symmetry() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let c = app.viewport_rect.center() + vec2(0.0, 150.0);
    let key = |key, pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    for (k, field) in [(egui::Key::S, "radius"), (egui::Key::U, "strength"), (egui::Key::O, "falloff")] {
        let before = ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" }));
        frame(&mut app, &ctx, vec![Event::PointerMoved(c)]);
        frame(&mut app, &ctx, vec![key(k, true)]);
        for i in 1..=5 {
            frame(&mut app, &ctx, vec![Event::PointerMoved(c + vec2(12.0 * i as f32, 0.0))]);
        }
        frame(&mut app, &ctx, vec![key(k, false)]);
        let after = ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" }));
        assert_ne!(after[field], before[field], "{k:?} changed {field}: {after}");
        assert!(app.ui_activity.is_empty() || app.capture.is_none(), "no stroke");
    }
    let sym = |app: &mut ClideApp| ok(app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" }))["symmetry"].clone();
    assert!(sym(&mut app).is_null());
    frame(&mut app, &ctx, vec![key(egui::Key::X, true)]);
    assert!(!app.snap_held(&ctx), "a brush tool's X does not hold Snap to Grid");
    frame(&mut app, &ctx, vec![key(egui::Key::X, false)]);
    assert_eq!(sym(&mut app), json!({ "axes": ["x"] }));
    frame(&mut app, &ctx, vec![key(egui::Key::X, true)]);
    frame(&mut app, &ctx, vec![key(egui::Key::X, false)]);
    assert!(sym(&mut app).is_null());
    // The Select tool: X holds Snap to Grid and leaves symmetry alone.
    app.tool = super::Tool::Select;
    frame(&mut app, &ctx, vec![key(egui::Key::X, true)]);
    assert!(app.snap_held(&ctx), "Select: X holds Snap to Grid");
    frame(&mut app, &ctx, vec![key(egui::Key::X, false)]);
    assert!(sym(&mut app).is_null());
}

/// D and Shift+D (Stage 8a) step the clay's active detail level finer and
/// coarser through `detail_level` requests that replay through MCP; past
/// either end they send nothing.
#[test]
fn d_and_shift_d_step_the_detail_level_through_its_tool() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "detail_level", json!({ "target": "Ball", "add": 0.004 }));
    ok(&mut app, &ctx, &tx, "detail_level", json!({ "target": "Ball", "level": 0 }));
    settle(&mut app, &ctx);
    let c = app.viewport_rect.center();
    let press = |app: &mut ClideApp, mods: Modifiers| {
        let key = |pressed| Event::Key { key: egui::Key::D, physical_key: None, pressed, repeat: false, modifiers: mods };
        frame_m(app, &ctx, vec![Event::PointerMoved(c)], mods);
        frame_m(app, &ctx, vec![key(true)], mods);
        frame_m(app, &ctx, vec![key(false)], mods);
    };
    let level = |app: &ClideApp| crate::levels::active(&clay(app));
    let pre = before(&app);
    press(&mut app, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "detail_level");
    assert_eq!(request["level"], 1, "{request}");
    assert_eq!(level(&app), 1);
    // The finest level: D sends nothing.
    let sent = app.ui_activity.len();
    press(&mut app, Modifiers::NONE);
    settle(&mut app, &ctx);
    assert_eq!(app.ui_activity.len(), sent);
    let pre = before(&app);
    press(&mut app, Modifiers::SHIFT);
    let request = parity(&mut app, &ctx, &tx, pre, "detail_level");
    assert_eq!(request["level"], 0, "{request}");
    assert_eq!(level(&app), 0);
}

/// D is Edit Pivot (hold) and, with a brush tool, the finer detail level
/// (the hotkeys' LevelFiner): with a brush tool it steps the level and
/// does not hold Edit Pivot; with the Select tool it holds Edit Pivot and
/// sends nothing. During a stroke D and Shift+D send nothing (the stroke
/// works at the level it began at), and the stroke still goes.
#[test]
fn d_steps_levels_only_with_a_brush_tool_and_never_mid_stroke() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "detail_level", json!({ "target": "Ball", "add": 0.004 }));
    ok(&mut app, &ctx, &tx, "detail_level", json!({ "target": "Ball", "level": 0 }));
    settle(&mut app, &ctx);
    let c = app.viewport_rect.center();
    let d = |pressed, mods| Event::Key { key: egui::Key::D, physical_key: None, pressed, repeat: false, modifiers: mods };
    let level = |app: &ClideApp| crate::levels::active(&clay(app));
    // The Select tool: D holds Edit Pivot, no level step.
    app.tool = super::Tool::Select;
    let sent = app.ui_activity.len();
    frame(&mut app, &ctx, vec![Event::PointerMoved(c)]);
    frame(&mut app, &ctx, vec![d(true, Modifiers::NONE)]);
    assert!(app.pivot_held(&ctx), "Select: D holds Edit Pivot");
    frame(&mut app, &ctx, vec![d(false, Modifiers::NONE)]);
    settle(&mut app, &ctx);
    assert_eq!((app.ui_activity.len(), level(&app)), (sent, 0), "Select: D sends no detail_level");
    // The Sculpt tool: D steps the level, Edit Pivot is not held.
    app.tool = super::Tool::Sculpt;
    frame(&mut app, &ctx, vec![Event::PointerMoved(c)]);
    frame(&mut app, &ctx, vec![d(true, Modifiers::NONE)]);
    assert!(!app.pivot_held(&ctx), "Sculpt: D does not hold Edit Pivot");
    frame(&mut app, &ctx, vec![d(false, Modifiers::NONE)]);
    settle(&mut app, &ctx);
    assert_eq!(level(&app), 1);
    // Mid-stroke: D and Shift+D while the button is down send nothing.
    let path = across(&app, 0.0);
    let sent = app.ui_activity.len();
    drag_with(&mut app, &ctx, &path, Modifiers::NONE, |p| if p == path[8] { vec![d(true, Modifiers::SHIFT), d(false, Modifiers::SHIFT)] } else if p == path[4] { vec![d(true, Modifiers::NONE), d(false, Modifiers::NONE)] } else { Vec::new() });
    settle(&mut app, &ctx);
    let tools: Vec<&str> = app.ui_activity.iter().skip(sent).map(|a| a.tool).collect();
    assert_eq!(tools, ["sculpt"], "only the stroke was sent");
    assert_eq!(level(&app), 1, "the level did not move mid-stroke");
}

/// The vdm brush from the UI: with a preset that carries a falloff and an
/// image alpha's settings (use, mid, mode tile), the drag's stroke drops
/// what a VDM refuses and stamps (the request replays through MCP);
/// `set_current` refuses an alpha of the wrong kind for the brush and
/// drops a kept one when the brush changes kind.
#[test]
fn a_vdm_drag_with_a_preset_sends_only_what_a_stamp_takes() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Horn", "resolution": 64, "blobs": [
        { "type": "box", "center": [5, -0.15, 0], "size": [0.6, 0.3, 0.6], "blend": 0 },
        { "type": "capsule", "a": [5, -0.01, 0], "b": [5, 0.06, 0], "radius": 0.012, "blend": 0 },
    ] }));
    ok(&mut app, &ctx, &tx, "edit_clay", json!({ "target": "Horn", "detail_cell": 0.004 }));
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "alpha", json!({ "action": "create_vdm", "name": "horn", "target": "Horn", "center": [5, 0, 0], "radius": 0.08, "above": 0.1, "below": 0.02 }));
    let image = std::env::temp_dir().join(format!("clide-vdm-ui-{}.png", std::process::id()));
    std::fs::write(&image, crate::brush::alpha::Alpha::new(4, 4, vec![30000; 16]).unwrap().png()).unwrap();
    ok(&mut app, &ctx, &tx, "alpha", json!({ "action": "import", "name": "dots", "path": image.to_str().unwrap() }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "save", "name": "Stampy", "spec": { "brush": "standard", "strength": 0.6, "falloff": { "preset": "sharp", "focal_shift": 0.3 }, "alpha": { "name": "horn", "rotate": 30, "use": "mask", "mid": 0.5, "mode": "tile" } } }));
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball" }));
    // The wrong kind is refused when chosen.
    let (v, err) = mcp(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "vdm", "alpha": "dots" }));
    assert!(err && v.to_string().contains("is an image"), "{v}");
    let (v, err) = mcp(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "standard", "alpha": "horn" }));
    assert!(err && v.to_string().contains("VDM stamp"), "{v}");
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "standard", "alpha": "dots" }));
    let now = ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "brush": "vdm" }));
    assert!(now["alpha"].is_null(), "an image kept from standard is dropped for vdm: {now}");
    // The preset's falloff and image settings stay out of the stamp.
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "preset": "Stampy", "brush": "vdm", "radius": 0.08, "falloff": { "preset": "sharp", "focal_shift": 0.2 } }));
    let pre = before(&app);
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "sculpt");
    let stroke = &request["strokes"][0];
    assert_eq!(stroke["brush"], "vdm", "{request}");
    assert!(stroke["falloff"].is_null(), "{stroke}");
    assert_eq!(stroke["alpha"], json!({ "name": "horn", "rotate": 30 }), "{stroke}");
    let r = ledger::ledger::<Detail>(&clay(&app)).records().last().cloned().unwrap();
    assert!(r.stats.changed_points > 0, "the stamp went on");
    let _ = std::fs::remove_file(&image);
}

/// Stage 4b's `ui_poly_paint_matches_mcp`: the Paint tool dragged across
/// a Poly (UDIM tiles) sends `paint`, and the same request from an agent
/// on the scene before it gives the same reply and the same Poly, its
/// stroke record included; sculpt drags on a Poly are refused in the
/// editor with a status, nothing sent.
#[test]
fn ui_poly_paint_matches_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "poly", json!({ "action": "from_clay", "source": "Ball" }));
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "update_object", json!({ "target": "Ball", "translate": [6, 0, 0] }));
    let poly = id_of(&app, "Ball_poly");
    ok(&mut app, &ctx, &tx, "poly", json!({ "target": poly, "action": "uv", "udim": true, "tiles": 2, "size": 512 }));
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball_poly" }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "radius": 0.08 }));
    settle(&mut app, &ctx);
    let sent = app.ui_activity.len();
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    settle(&mut app, &ctx);
    assert_eq!(app.ui_activity.len(), sent, "a sculpt drag on a Poly sends nothing");
    assert!(app.status.contains("Paint"), "{}", app.status);
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "paint", "radius": 0.08 }));
    settle(&mut app, &ctx);
    let pre = before(&app);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "paint");
    assert_eq!(request["target"], json!(poly), "{request}");
    assert!(request["strokes"][0]["frame"]["screen"]["px"].as_array().is_some_and(|p| p.len() > 3), "{request}");
    let p = app.scene.get(poly).and_then(|o| o.poly.clone()).unwrap();
    let paint = p.paint.as_ref().expect("painted");
    assert_eq!(paint.records.len(), 1);
    let r = &paint.records[0];
    eprintln!("UI paint: record {} on {:?}, {} dabs, tiles {:?}", r.id, r.channel, r.stroke.dabs.len(), paint.textures.tiles(r.channel));
    assert!(r.digest == r.digest() && !paint.textures.tiles(r.channel).is_empty());
}

/// The plan's `ui_groom_matches_mcp`: the Groom tool's drag over a
/// selected Hair sends one `groom` request (world points across the view
/// plane at the press, the HUD's groom brush, radius and strength, no
/// pictures) and, sent through MCP on the scene before the drag, gives the
/// same guides and the same reply.
#[test]
fn ui_groom_matches_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Hair", "name": "Locks", "parent": "Ball", "hair": { "grow": { "on": "Ball", "count": 40, "length": 0.4 } } }));
    settle(&mut app, &ctx);
    let hair = id_of(&app, "Locks");
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Locks" }));
    // A press with nothing to groom says so and sends nothing.
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "groom", "groom": "comb", "radius": 0.6, "strength": 1.0 }));
    assert_eq!(ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" }))["groom"], "comb");
    settle(&mut app, &ctx);
    let before_hair = app.scene.get(hair).and_then(|o| o.hair.clone()).unwrap();
    let pre = before(&app);
    let path = across(&app, -40.0);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "groom");
    assert_eq!(request["target"], json!(hair), "{request}");
    assert_eq!(request["render"], json!(false));
    assert_eq!(request["strokes"][0]["brush"], "comb");
    assert!(request["strokes"][0]["points"].as_array().is_some_and(|p| p.len() > 3), "{request}");
    let after = app.scene.get(hair).and_then(|o| o.hair.clone()).unwrap();
    assert_ne!(after.guides, before_hair.guides, "the drag combed the guides");
    // Clump through the HUD's groom brush, then undo in one step.
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "groom": "clump" }));
    settle(&mut app, &ctx);
    let pre = before(&app);
    drag(&mut app, &ctx, &path, Modifiers::NONE);
    let request = parity(&mut app, &ctx, &tx, pre, "groom");
    assert_eq!(request["strokes"][0]["brush"], "clump");
    ok(&mut app, &ctx, &tx, "undo", json!({}));
    assert!(app.scene.get(hair).and_then(|o| o.hair.clone()).unwrap() == after, "undo takes the clump back exactly");
}

/// Stage 4d: the material tab's Stylised Look buttons send the agent's
/// calls (set_material preset with apply_to; update_object eye style),
/// each one request whose agent copy gives the same reply and scene.
#[test]
fn ui_stylised_look_matches_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let ball = id_of(&app, "Ball");
    for (_, preset) in super::panels::STYLIZED_LOOKS {
        let pre = before(&app);
        let (tool, args) = super::panels::stylised_look_request(ball, Some(preset));
        app.send_ui_call(tool, args);
        let request = parity(&mut app, &ctx, &tx, pre, "set_material");
        assert_eq!((request["preset"].as_str(), request["apply_to"].as_u64()), (Some(preset), Some(ball)), "{request}");
        assert_eq!(app.scene.get(ball).unwrap().material.as_deref(), Some(preset));
    }
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Eye", "name": "Peeper" }));
    settle(&mut app, &ctx);
    let eye = id_of(&app, "Peeper");
    let pre = before(&app);
    let (tool, args) = super::panels::stylised_look_request(eye, None);
    app.send_ui_call(tool, args);
    let request = parity(&mut app, &ctx, &tx, pre, "update_object");
    assert_eq!(request["eye"]["style"], "stylized");
    let e = app.scene.get(eye).unwrap().eye.clone().unwrap();
    assert!(e.catchlight && e.style == crate::eyeball::EyeStyle::Stylized);
}

/// File > Load Turnaround Sheet… sends `set_reference {sheet}` (the
/// request `sheet::sheet_request` builds, the one function the menu
/// calls), and the agent's copy gives the same reply and scene.
#[test]
fn ui_sheet_matches_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let dir = std::env::temp_dir().join(format!("clide3d_ui_sheet_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sheet.png");
    super::sheet::synthetic::sheet(400.0, 1.25, super::sheet::synthetic::DEPTH, true).0.save(&path).unwrap();
    let pre = before(&app);
    app.send_ui_call("set_reference", super::sheet::sheet_request(&path));
    let request = parity(&mut app, &ctx, &tx, pre, "set_reference");
    assert_eq!(request["sheet"]["path"].as_str(), Some(path.to_str().unwrap()));
    assert_eq!(app.scene.objects.iter().filter(|o| o.is_reference()).count(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stage 5b: Display > Measure Proportions sends `guides {action:
/// measure, target: the selection}` (`measure_request`, the one function
/// the menu calls) and Display > Proportion Guides `set_display
/// {guides}`; the agent's copies give the same replies and scene.
#[test]
fn ui_guides_match_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    assert!(app.measure_request().is_none(), "no guides: nothing to measure");
    ok(&mut app, &ctx, &tx, "guides", json!({ "action": "set", "head_units": { "count": 3, "bottom": -0.5, "top": 0.65 }, "targets": [{ "name": "cap_ratio", "of": ["cap"], "value": 0.35, "tolerance": 0.01 }] }));
    let ball = id_of(&app, "Ball");
    let pre = before(&app);
    let req = app.measure_request().expect("guides and a selection");
    app.guides_job = Some(app.send_ui_call("guides", req));
    let request = parity(&mut app, &ctx, &tx, pre, "guides");
    assert_eq!((request["action"].as_str(), request["target"].as_u64()), (Some("measure"), Some(ball)));
    let pre = before(&app);
    app.send_ui_call("set_display", json!({ "guides": false }));
    let request = parity(&mut app, &ctx, &tx, pre, "set_display");
    assert_eq!(request["guides"], false);
    assert!(!app.show_guides);
}

/// Stage 5c: Create > Character Base sends `create_character` with the
/// defaults under a free name (`character_request`, the one function the
/// menu calls); the agent's copy gives the same reply and scene.
#[test]
fn ui_character_matches_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    let req = app.character_request();
    assert_eq!(req, json!({ "name": "Character" }));
    let pre = before(&app);
    app.send_ui_call("create_character", req);
    let request = parity(&mut app, &ctx, &tx, pre, "create_character");
    assert_eq!(request["name"], "Character");
    assert!(app.scene.objects.iter().any(|o| o.name == "Character_eye.R"));
    // A second base takes the next free name.
    assert_eq!(app.character_request(), json!({ "name": "Character2" }));
}

/// Character follow-ups: Create > Eyelids on Selected Clay and Create >
/// Fur on Selected Clay send the requests an agent would (one lid a
/// selected clay's eye; a Hair of fur under it), with the same reply and
/// scene through MCP; without a selected clay neither is offered.
#[test]
fn ui_lids_and_fur_match_mcp() {
    let Some((mut app, tx, ctx)) = editor() else { return };
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Ball" }));
    assert!(app.lids_request().is_none(), "no eyes under Ball");
    super::pose_split_tests::open_pip(&mut app, &ctx, &tx);
    app.selection.clear();
    assert!(app.fur_request().is_none() && app.lids_request().is_none());
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Pip" }));
    let lids = app.lids_request().expect("Pip has eyes");
    assert_eq!(lids, json!({ "target": "Pip", "add": [{ "type": "lid", "eye": "Pip_eye.L" }, { "type": "lid", "eye": "Pip_eye.R" }] }));
    let pre = before(&app);
    app.send_ui_call("edit_clay", lids);
    parity(&mut app, &ctx, &tx, pre, "edit_clay");
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Pip" }));
    let fur = app.fur_request().expect("a clay is selected");
    assert_eq!(fur["name"], "Pip_fur");
    let pre = before(&app);
    app.send_ui_call("create_object", fur);
    parity(&mut app, &ctx, &tx, pre, "create_object");
    ok(&mut app, &ctx, &tx, "select_object", json!({ "target": "Pip" }));
    assert_eq!(app.fur_request().unwrap()["name"], "Pip_fur2");
}
