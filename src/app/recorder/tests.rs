//! Stage 1c acceptance: a headless editor driven by synthetic egui input.
//! Needs a wgpu device for the editor's render state (skips without one);
//! nothing is drawn.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use serde_json::{Value, json};

use super::super::ClideApp;
use super::reply_json;
use crate::brush::spec::Brush;
use crate::clay::Detail;
use crate::ledger::{self, Source};
use crate::mcp::ToolCall;
use crate::paint::Paint;

/// The window the tests draw in, in points.
const WINDOW: egui::Vec2 = vec2(1400.0, 900.0);

pub(in crate::app) fn frame(app: &mut ClideApp, ctx: &egui::Context, events: Vec<Event>) {
    frame_in(app, ctx, events, WINDOW);
}

/// A frame in a window of `size` points.
fn frame_in(app: &mut ClideApp, ctx: &egui::Context, events: Vec<Event>, size: egui::Vec2) {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), events, ..Default::default() };
    let _ = ctx.run(raw, |ctx| app.frame(ctx));
}

/// A headless editor with the clay Ball, the camera close, and the Sculpt
/// tool's brush set; None without a GPU adapter.
fn with_ball() -> Option<(ClideApp, Sender<ToolCall>, egui::Context)> {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return None;
    };
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0.0 }] }));
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "inflate", "radius": 0.1 }));
    settle(&mut app, &ctx);
    app.cams.persp.distance = 3.0;
    Some((app, tx, ctx))
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::default() }
}

/// Frames until no call, job, stroke or mesh build is pending.
pub(in crate::app) fn settle(app: &mut ClideApp, ctx: &egui::Context) {
    let start = Instant::now();
    loop {
        frame(app, ctx, Vec::new());
        let idle = app.jobs.running.is_empty() && app.jobs.queue.is_empty() && app.jobs.deferred.is_empty() && app.capture.is_none() && app.strokes.is_empty() && app.lives.is_empty() && app.hud.amount_sent.is_none() && !app.meshes.busy() && !app.meshes.chunks_pending();
        if idle {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "the editor never settled: running {}, queue {}, deferred {}, drawing {}, strokes {}, building {:?}", app.jobs.running.len(), app.jobs.queue.len(), app.jobs.deferred.len(), app.capture.is_some(), app.strokes.len(), app.meshes.building());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A tool call through the MCP server's dispatcher (JSON-RPC in, the
/// window's thread answers), as an agent sends it. Returns (reply JSON, is
/// error).
pub(in crate::app) fn mcp(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, args: Value) -> (Value, bool) {
    let msg = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let (tx, c) = (tx.clone(), ctx.clone());
    let call = std::thread::spawn(move || crate::mcp::dispatch(&msg, &tx, &c));
    let start = Instant::now();
    while !call.is_finished() {
        frame(app, ctx, Vec::new());
        assert!(start.elapsed() < Duration::from_secs(120), "{name} never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    let r = call.join().expect("dispatch thread").expect("a reply")["result"].clone();
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    (serde_json::from_str(text).unwrap_or(json!(text)), r["isError"] == true)
}

pub(in crate::app) fn ok(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, name: &str, args: Value) -> Value {
    let (v, err) = mcp(app, ctx, tx, name, args);
    assert!(!err, "{name}: {v}");
    v
}

/// A left drag through `path`, one frame per sample. The scene must not
/// change while the button is down.
fn drag(app: &mut ClideApp, ctx: &egui::Context, path: &[Pos2]) {
    drag_with(app, ctx, path, |_| {});
}

/// `drag`, calling `before` ahead of every frame.
fn drag_with(app: &mut ClideApp, ctx: &egui::Context, path: &[Pos2], mut before: impl FnMut(&mut ClideApp)) {
    let scene = app.scene.clone();
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::default() };
    let mut step = |app: &mut ClideApp, events| {
        before(app);
        frame(app, ctx, events);
    };
    step(app, vec![Event::PointerMoved(path[0])]);
    step(app, vec![button(path[0], true)]);
    for &p in &path[1..] {
        step(app, vec![Event::PointerMoved(p)]);
        assert!(app.scene == scene, "nothing applies during the drag");
    }
    step(app, vec![button(path[path.len() - 1], false)]);
    assert!(app.scene == scene, "nothing applies until the request runs");
}

/// Puts a read-only agent call in the line of tool calls, so a released
/// stroke has to wait its turn this frame.
fn hold_line(app: &mut ClideApp) {
    let (reply, _) = std::sync::mpsc::channel();
    let ticket = app.jobs.ticket("get_scene".into(), reply.clone());
    app.jobs.queue.push_back((ToolCall { name: "get_scene".into(), args: json!({}), reply, source: Source::Agent }, ticket));
}

fn clay(app: &ClideApp, name: &str) -> crate::clay::Clay {
    app.scene.objects.iter().find(|o| o.name == name).and_then(|o| o.clay.clone()).expect("the clay")
}

/// A horizontal drag across the middle of the view.
fn across(app: &ClideApp, dy: f32) -> Vec<Pos2> {
    let c = app.viewport_rect.center();
    (0..=24).map(|i| c + vec2(-72.0 + 6.0 * i as f32, dy + (i as f32 * 0.4).sin() * 4.0)).collect()
}

fn without_timing(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("timing_ms");
    }
    v
}

#[test]
fn a_ui_drag_sends_the_agents_request_and_replays_through_mcp() {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return;
    };
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    ok(&mut app, &ctx, &tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0.0 }] }));
    settle(&mut app, &ctx);
    app.cams.persp.distance = 3.0;

    // The agent sets the human's brush; bad settings change nothing.
    let current = ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "inflate", "radius": 0.15, "strength": 0.6 }));
    assert_eq!((current["tool"].as_str(), current["brush"].as_str(), current["radius"].as_f64()), (Some("sculpt"), Some("inflate"), Some(0.15f32 as f64)));
    for bad in [json!({ "action": "set_current", "brush": "noise" }), json!({ "action": "set_current", "radius": 0 }), json!({ "action": "set_current", "radius": 0.2, "size": 3 }), json!({ "action": "save", "name": "x" })] {
        let (why, err) = mcp(&mut app, &ctx, &tx, "brush_preset", bad.clone());
        assert!(err, "{bad} is refused: {why}");
    }
    assert_eq!(ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "get_current" })), current);
    assert!(app.tool == super::Tool::Sculpt);

    // An agent stroke first: the clay's ledger exists before the drag. (A
    // stroke that creates the ledger gives it a fresh uid, which seeds the
    // head, so only then would a resent request's ledger_head differ.)
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "standard", "points": [[0, 0.5, 0]], "radius": 0.1, "snap": true }] }));
    // The drag: one record from the UI, one labelled history step.
    settle(&mut app, &ctx);
    let pre = app.scene.clone();
    let step = app.history.current();
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path);
    let released = Instant::now();
    settle(&mut app, &ctx);
    let settled = released.elapsed();
    let ball = clay(&app, "Ball");
    let records = ledger::ledger::<Detail>(&ball).records().to_vec();
    assert_eq!(records.iter().filter(|r| r.stroke().is_some()).count(), 2, "the agent's stroke and one from the drag");
    let record = records.last().expect("the stroke").clone();
    assert!(record.stroke().is_some());
    assert_eq!(record.source, Source::Ui);
    assert_eq!(record.tool.as_deref(), Some("sculpt"));
    assert!(record.stats.changed_points > 0, "the stroke changed the clay");
    assert_eq!(app.history.current(), step + 1, "one history step");
    let list = app.history.list();
    let last = &list["steps"][step as usize];
    let label = last["label"].as_str().unwrap_or_default();
    assert!(label.contains("sculpt inflate") && label.ends_with("[ui]") && last["source"] == "ui", "{last}");

    // The agent sees it in get_scene: its job, step and record, and with
    // detail the request itself.
    let scene = ok(&mut app, &ctx, &tx, "get_scene", json!({ "detail": true }));
    let activity = scene["ui_activity"].as_array().expect("ui_activity").clone();
    assert_eq!(activity.len(), 1, "{activity:?}");
    let a = &activity[0];
    assert_eq!((a["tool"].as_str(), a["status"].as_str(), a["source"].as_str()), (Some("sculpt"), Some("done"), Some("ui")), "{a}");
    assert_eq!(a["record_ids"], json!([record.id]));
    assert_eq!(a["step"], json!(step + 1));
    let request = a["request"].clone();
    let stroke = &request["strokes"][0];
    assert!(stroke["frame"]["screen"]["camera"].is_object() && stroke["frame"]["screen"]["px"].as_array().is_some_and(|p| p.len() > 10), "{stroke}");
    assert_eq!((stroke["brush"].as_str(), stroke["radius"].as_f64()), (Some("inflate"), Some(0.15f32 as f64)));
    let ui_reply = reply_json(app.jobs.result(a["job"].as_u64().expect("job")).expect("finished").as_ref().expect("applied"));
    eprintln!("ui stroke: {} samples, timing_ms {}, release to remeshed {:.0} ms", stroke["frame"]["screen"]["px"].as_array().map_or(0, Vec::len), ui_reply["timing_ms"], settled.as_secs_f64() * 1000.0);
    let ui_reply = without_timing(ui_reply);
    let compact = ok(&mut app, &ctx, &tx, "get_scene", json!({}))["ui_activity"].clone();
    assert_eq!(ok(&mut app, &ctx, &tx, "job_status", json!({}))["ui_activity"], compact);
    let mut short = a.clone();
    short.as_object_mut().expect("entry").remove("request");
    assert_eq!(compact, json!([short]), "compact entries leave the request out");
    assert_eq!((compact[0]["brush"].as_str(), compact[0]["target"].as_u64()), (Some("inflate"), Some(1)));

    // Undo restores the scene from before the drag.
    app.undo();
    assert!(app.scene == pre, "undo restores the pre-drag scene");
    assert_eq!(app.history.current(), step);

    // The stored request, sent by an agent through MCP on that scene:
    // the same record and the same reply (timing aside).
    settle(&mut app, &ctx);
    let agent_reply = without_timing(ok(&mut app, &ctx, &tx, "sculpt", request));
    assert_eq!(agent_reply, ui_reply, "the agent's reply equals the UI's");
    let again = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().last().expect("the stroke").clone();
    assert_eq!(again.result_hash, record.result_hash);
    assert_eq!(again.source, Source::Agent);
    assert_eq!(again.stroke().map(|(s, _)| s), record.stroke().map(|(s, _)| s), "the same spec");

    // Paint: the same route, to the paint layer's ledger.
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "paint", "color": [0.1, 0.3, 0.9], "radius": 0.1, "strength": 1.0 }));
    let step = app.history.current();
    let path = across(&app, 25.0);
    drag(&mut app, &ctx, &path);
    settle(&mut app, &ctx);
    let ball = clay(&app, "Ball");
    let painted: Vec<_> = ledger::ledger::<Paint>(&ball).records().iter().filter(|r| r.stroke().is_some()).cloned().collect();
    assert_eq!(painted.len(), 1);
    assert_eq!((painted[0].source, painted[0].tool.as_deref()), (Source::Ui, Some("paint")));
    assert_eq!(app.history.current(), step + 1);
    let list = app.history.list();
    assert!(list["steps"][step as usize]["label"].as_str().is_some_and(|l| l.starts_with("paint") && l.ends_with("[ui]")), "{list}");
    let scene = ok(&mut app, &ctx, &tx, "get_scene", json!({}));
    let last = scene["ui_activity"].as_array().and_then(|a| a.last()).cloned().unwrap_or_default();
    assert_eq!((last["tool"].as_str(), last["status"].as_str()), (Some("paint"), Some("done")), "{last}");
    assert_eq!(last["record_ids"], json!([painted[0].id]));

    // Strokes released while the line is busy wait, in order; pressing
    // again meanwhile starts a new one and loses none.
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "inflate", "radius": 0.1 }));
    let step = app.history.current();
    let before = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().len();
    let (first, second) = (across(&app, -20.0), across(&app, 20.0));
    drag_with(&mut app, &ctx, &first, hold_line);
    assert_eq!(app.strokes.len(), 1, "the first stroke waits");
    // A brush change while it waits (the shelf's handler, no frame between)
    // does not change it.
    app.brush_preset(&json!({ "action": "set_current", "brush": "deflate" })).expect("deflate");
    drag_with(&mut app, &ctx, &second, hold_line);
    assert_eq!(app.strokes.len(), 2, "both strokes wait");
    settle(&mut app, &ctx);
    assert_eq!(app.history.current(), step + 2, "two history steps");
    let records = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().to_vec();
    assert_eq!(records.len(), before + 2);
    let brushes: Vec<_> = records[before..].iter().map(|r| r.stroke().map(|(s, _)| s.brush)).collect();
    assert_eq!(brushes, [Some(Brush::Inflate), Some(Brush::Deflate)], "sent in order, each with its press's brush");
    let activity = ok(&mut app, &ctx, &tx, "get_scene", json!({}))["ui_activity"].as_array().cloned().unwrap_or_default();
    let tail: Vec<_> = activity[activity.len() - 2..].iter().map(|a| (a["brush"].clone(), a["status"].clone())).collect();
    assert_eq!(tail, vec![(json!("inflate"), json!("done")), (json!("deflate"), json!("done"))]);

    // A stroke waiting while the scene is replaced is dropped, even when a
    // new clay takes its clay's id.
    settle(&mut app, &ctx);
    let id = app.scene.objects.iter().find(|o| o.name == "Ball").map(|o| o.id).expect("Ball");
    let sent = app.ui_activity.len();
    let path = across(&app, 0.0);
    drag_with(&mut app, &ctx, &path, hold_line);
    assert_eq!(app.strokes.len(), 1, "the stroke waits");
    app.new_scene();
    app.run_tool("create_object", &json!({ "shape": "Clay", "name": "Other", "resolution": 48, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0.0 }] })).expect("create");
    assert_eq!(app.scene.objects.iter().find(|o| o.name == "Other").map(|o| o.id), Some(id), "the new clay reuses the id");
    let other = clay(&app, "Other");
    settle(&mut app, &ctx);
    assert!(clay(&app, "Other") == other, "the old stroke left the new clay alone");
    assert_eq!(app.ui_activity.len(), sent, "nothing was sent");

    // An agent moving the camera mid-drag drops the stroke: the later
    // samples would be pixels of the new view, replayed through the press's.
    app.cams.persp.distance = 3.0;
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "brush": "inflate", "radius": 0.1 }));
    settle(&mut app, &ctx);
    let (scene, step) = (app.scene.clone(), app.history.current());
    let path = across(&app, 0.0);
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::default() };
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true)]);
    for &p in &path[1..8] {
        frame(&mut app, &ctx, vec![Event::PointerMoved(p)]);
    }
    assert!(app.capture.is_some(), "the stroke is recording");
    ok(&mut app, &ctx, &tx, "set_camera", json!({ "yaw": 80 }));
    for &p in &path[8..] {
        frame(&mut app, &ctx, vec![Event::PointerMoved(p)]);
    }
    assert!(app.capture.is_none() && app.strokes.is_empty(), "the stroke was dropped");
    frame(&mut app, &ctx, vec![button(path[path.len() - 1], false)]);
    settle(&mut app, &ctx);
    assert!(app.status.contains("view moved"), "{}", app.status);
    assert!(app.scene == scene && app.history.current() == step, "nothing applied");
    assert_eq!(app.ui_activity.len(), sent, "nothing was sent");
}

#[test]
fn a_long_stroke_is_thinned_evenly_keeping_both_ends() {
    let px: Vec<[f32; 2]> = (0..5000).map(|i| [i as f32, 0.0]).collect();
    let max = crate::brush::resolve::MAX_POINTS;
    let t = super::thin(&px, max);
    assert_eq!(t.len(), max);
    assert_eq!((t[0], t[max - 1]), (px[0], px[4999]));
    assert!(t.windows(2).all(|w| w[1][0] > w[0][0]), "in order, no repeats");
    assert_eq!(super::thin(&px[..10], max), px[..10].to_vec(), "short strokes are kept whole");
}

/// The jobs panel at the bottom comes and goes (and changes height) as
/// jobs and mesh builds start and end, so the viewport changes size during
/// a drag while the camera holds still. Such a stroke is kept, and every
/// sample is the ray under the mouse when it was taken; a real camera
/// move still drops it.
#[test]
fn a_stroke_outlives_a_pane_resize_but_not_a_camera_move() {
    let Some((mut app, _tx, ctx)) = with_ball() else { return };
    let path = across(&app, 0.0);
    let (scene, step) = (app.scene.clone(), app.history.current());
    let small = vec2(WINDOW.x, WINDOW.y - 80.0);
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true)]);
    let before = app.viewport_rect;
    for &p in &path[1..12] {
        frame(&mut app, &ctx, vec![Event::PointerMoved(p)]);
    }
    // The panel shows up: the pane is shorter from here on.
    for &p in &path[12..] {
        frame_in(&mut app, &ctx, vec![Event::PointerMoved(p)], small);
        assert!(app.capture.is_some(), "a resized pane keeps the stroke");
    }
    let rect = app.viewport_rect;
    assert!(rect.height() < before.height() - 50.0, "the pane shrank: {before:?} -> {rect:?}");
    // The last sample, through the press's camera, is the ray under the
    // mouse in the pane as it is now (not the same pixel offset read in
    // the press's pane, which a shorter pane scales).
    let last = *path.last().expect("a path");
    let now = app.stroke_camera(crate::camera::ViewKind::Persp, rect, 1.0);
    let c = app.capture.as_ref().expect("recording");
    let p = c.trail.samples.last().expect("samples").px;
    let (_, want) = now.ray(last.x - rect.min.x, last.y - rect.min.y);
    let (_, got) = c.camera.ray(p[0], p[1]);
    assert!(got.angle_between(want) < 1e-4, "the sample's ray {got} is the mouse's {want}");
    let naive = [last.x - before.min.x, last.y - before.min.y];
    assert!((naive[0] - p[0]).hypot(naive[1] - p[1]) > 5.0, "the resize mattered for this check: {naive:?} vs {p:?}");
    frame_in(&mut app, &ctx, vec![button(last, false)], small);
    settle(&mut app, &ctx);
    let records = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().to_vec();
    assert_eq!(records.iter().filter(|r| r.source == Source::Ui).count(), 1, "the stroke was sent: {}", app.status);
    assert_eq!(app.history.current(), step + 1);
    assert!(app.scene != scene);
    // Every sample hit the clay and was kept.
    let request = app.ui_activity.back().expect("sent").request.clone();
    assert_eq!(request["strokes"][0]["frame"]["screen"]["px"].as_array().map(Vec::len), Some(path.len()), "no sample was lost");

    // A dolly mid-drag (the camera, not the pane) still drops it.
    let (scene, sent) = (app.scene.clone(), app.ui_activity.len());
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true)]);
    frame_in(&mut app, &ctx, vec![Event::PointerMoved(path[1])], small);
    assert!(app.capture.is_some());
    app.cams.persp.distance = 2.5;
    frame_in(&mut app, &ctx, vec![Event::PointerMoved(path[2])], small);
    assert!(app.capture.is_none() && app.status.contains("view moved"), "{}", app.status);
    frame_in(&mut app, &ctx, vec![button(path[2], false)], small);
    settle(&mut app, &ctx);
    assert!(app.scene == scene && app.ui_activity.len() == sent, "nothing was sent");
}

/// The press is picked on the clay as the timeline frame shows it, and
/// the handler strokes the rest pose. A sculpt drag on a posed clay sends
/// points on the posed clay with posable symmetry at that frame (the
/// handler takes them back to rest), which replays the same through MCP;
/// paint, mask and group drags there are refused, with a reason.
#[test]
fn a_drag_on_a_posed_clay_sends_a_posable_stroke() {
    let Some((mut app, tx, ctx)) = with_ball() else { return };
    ok(&mut app, &ctx, &tx, "rig", json!({ "target": "Ball", "add": [{ "name": "root", "head": [0, -0.5, 0], "tail": [0, 0.5, 0] }] }));
    ok(&mut app, &ctx, &tx, "animate", json!({ "target": "Ball", "keys": [{ "bone": "root", "frame": 0, "rotate": [0, 0, 0] }, { "bone": "root", "frame": 10, "rotate": [0, 0, 60] }] }));
    // An agent stroke first, so the ledger exists before the drag.
    ok(&mut app, &ctx, &tx, "sculpt", json!({ "target": "Ball", "strokes": [{ "brush": "standard", "points": [[0, 0.5, 0]], "radius": 0.1, "snap": true }] }));
    settle(&mut app, &ctx);
    let path = across(&app, 0.0);

    // Mid-clip the rig is posed: the Paint tool's press is refused.
    app.timeline.frame = 5.0;
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "paint" }));
    let (scene, sent) = (app.scene.clone(), app.ui_activity.len());
    frame(&mut app, &ctx, vec![Event::PointerMoved(path[0])]);
    frame(&mut app, &ctx, vec![button(path[0], true)]);
    assert!(app.capture.is_none(), "the press is refused");
    assert!(app.status.contains("rest pose"), "{}", app.status);
    for &p in &path[1..] {
        frame(&mut app, &ctx, vec![Event::PointerMoved(p)]);
    }
    frame(&mut app, &ctx, vec![button(path[path.len() - 1], false)]);
    settle(&mut app, &ctx);
    assert!(app.scene == scene && app.ui_activity.len() == sent, "nothing was sent");

    // The Sculpt tool's drag sends points on the posed clay, posable at
    // frame 5, and applies.
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt" }));
    let (pre, step) = (app.scene.clone(), app.history.current());
    drag(&mut app, &ctx, &path);
    settle(&mut app, &ctx);
    assert_eq!(app.history.current(), step + 1, "{}", app.status);
    let request = app.ui_activity.back().expect("sent").request.clone();
    let stroke = &request["strokes"][0];
    assert_eq!((stroke["symmetry"]["posable"].as_bool(), stroke["symmetry"]["frame"].as_f64()), (Some(true), Some(5.0)), "{stroke}");
    assert!(stroke["points"].as_array().is_some_and(|p| p.len() > 10) && stroke.get("frame").is_none(), "{stroke}");
    let record = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().last().cloned().expect("the stroke");
    assert_eq!(record.source, Source::Ui);
    assert!(record.stats.changed_points > 0);
    // The request through MCP on the scene before it: the same stroke.
    app.undo();
    assert!(app.scene == pre);
    settle(&mut app, &ctx);
    ok(&mut app, &ctx, &tx, "sculpt", request);
    let again = ledger::ledger::<Detail>(&clay(&app, "Ball")).records().last().cloned().expect("the stroke");
    assert_eq!((again.result_hash, again.source), (record.result_hash, Source::Agent));

    // A clay hung from the animated bone moves with it: its world matrix
    // is off rest mid-clip, and back on it at frame 0.
    let ball = app.scene.objects.iter().find(|o| o.name == "Ball").map(|o| o.id).expect("Ball");
    app.run_tool("create_object", &json!({ "shape": "Clay", "name": "Hung", "resolution": 32, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "blend": 0.0 }] })).expect("create");
    let hung = app.scene.objects.iter().find(|o| o.name == "Hung").map(|o| o.id).expect("Hung");
    let o = app.scene.get_mut(hung).expect("Hung");
    (o.parent, o.bone) = (Some(ball), Some("root".into()));
    o.translation = glam::Vec3::new(0.0, 0.8, 0.0);
    for (frame, posed) in [(Some(5.0), true), (Some(0.0), false), (None, false)] {
        app.scene.frame = frame;
        let eval = app.scene.evaluate();
        assert_eq!(app.posed_now(hung, &eval), posed, "Hung at frame {frame:?}");
        assert_eq!(app.posed_now(ball, &eval), posed, "Ball at frame {frame:?}");
    }

    // At the clip's rest frame the drag sends a screen stroke, as ever.
    app.timeline.frame = 0.0;
    let (step, sent) = (app.history.current(), app.ui_activity.len());
    drag(&mut app, &ctx, &path);
    settle(&mut app, &ctx);
    assert_eq!(app.history.current(), step + 1, "{}", app.status);
    assert_eq!(app.ui_activity.len(), sent + 1);
    assert!(app.ui_activity.back().unwrap().request["strokes"][0]["frame"]["screen"].is_object());
}

/// The MaskPen preset: a drag sends the mask tool's request (one mask
/// record from the UI), and that request, resent through MCP after an
/// undo, gives the same record.
#[test]
fn a_mask_pen_drag_sends_the_mask_tools_request() {
    let Some((mut app, tx, ctx)) = with_ball() else { return };
    ok(&mut app, &ctx, &tx, "brush_preset", json!({ "action": "set_current", "tool": "sculpt", "preset": "MaskPen", "radius": 0.12 }));
    // The agent masks first, so the mask ledger exists before the drag.
    ok(&mut app, &ctx, &tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "min": 0.45 } }] }));
    settle(&mut app, &ctx);
    let (pre, step) = (app.scene.clone(), app.history.current());
    let path = across(&app, 0.0);
    drag(&mut app, &ctx, &path);
    settle(&mut app, &ctx);
    let ball = clay(&app, "Ball");
    let l = ledger::ledger::<crate::mask::SculptMask>(&ball);
    let record = l.records().last().expect("a record").clone();
    assert_eq!((l.len(), record.source, record.tool.as_deref()), (2, Source::Ui, Some("mask")));
    assert!(record.stats.changed_points > 0);
    assert_eq!(app.history.current(), step + 1);
    let scene = ok(&mut app, &ctx, &tx, "get_scene", json!({ "detail": true }));
    let a = scene["ui_activity"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((a["tool"].as_str(), a["record_ids"].clone()), (Some("mask"), json!([record.id])), "{a}");
    let request = a["request"].clone();
    ok(&mut app, &ctx, &tx, "undo", json!({}));
    assert!(app.scene == pre);
    ok(&mut app, &ctx, &tx, "mask", request);
    let again = ledger::ledger::<crate::mask::SculptMask>(&clay(&app, "Ball")).records().last().unwrap().clone();
    assert_eq!((again.result_hash, again.source), (record.result_hash, Source::Agent));
}

/// The gizmo on a masked clay: the drag moves nothing; on release it sends
/// the transform stroke an agent would (`sculpt {brush: transform}`).
#[test]
fn a_gizmo_drag_on_a_masked_clay_sends_a_transform() {
    let Some((mut app, tx, ctx)) = with_ball() else { return };
    ok(&mut app, &ctx, &tx, "mask", json!({ "target": "Ball", "actions": [{ "by_height": { "max": 0.0 } }] }));
    settle(&mut app, &ctx);
    let id = app.scene.objects.iter().find(|o| o.name == "Ball").map(|o| o.id).expect("Ball");
    app.select_only(id);
    app.tool = super::Tool::Move;
    let world = app.scene.world_matrix(id);
    // What the gizmo leaves after a drag up by 0.05 (its press, its now).
    let at = |y: f64| transform_gizmo_egui::math::Transform {
        scale: glam::DVec3::ONE.into(),
        rotation: glam::DQuat::IDENTITY.into(),
        translation: glam::DVec3::new(0.0, y, 0.0).into(),
    };
    app.masked_drag = Some((id, at(0.0), at(0.05)));
    let sent = app.ui_activity.len();
    frame(&mut app, &ctx, Vec::new());
    assert!(app.masked_drag.is_none() && app.ui_activity.len() == sent + 1, "released: one request");
    assert_eq!(app.scene.world_matrix(id), world, "the object did not move");
    settle(&mut app, &ctx);
    let a = app.ui_activity.back().unwrap();
    assert_eq!(a.tool, "sculpt");
    assert_eq!(a.request["strokes"][0]["brush"], "transform");
    let base = ledger::ledger::<Detail>(&clay(&app, "Ball"));
    let r = base.records().last().expect("the transform's record");
    assert_eq!((r.source, r.stroke().map(|s| s.0.brush)), (Source::Ui, Some(crate::brush::spec::Brush::Transform)));
    assert!(r.stats.changed_points > 0, "the free half moved");
}
