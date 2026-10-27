//! Stage 4a through MCP (headless editor; skips without a GPU adapter):
//! `render_film` subdivides and displaces Poly objects, and leaves every
//! other picture as it was.

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::recorder::tests::{frame, ok, settle};
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

/// The picture of a `render_film` reply (its PNG's base64) and the reply.
fn film(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>, args: Value) -> (String, Value) {
    let msg = json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "render_film", "arguments": args } });
    let (tx, c) = (tx.clone(), ctx.clone());
    let call = std::thread::spawn(move || crate::mcp::dispatch(&msg, &tx, &c));
    let start = std::time::Instant::now();
    while !call.is_finished() {
        frame(app, ctx, Vec::new());
        assert!(start.elapsed() < std::time::Duration::from_secs(300), "render_film never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r = call.join().expect("dispatch thread").expect("a reply")["result"].clone();
    assert!(r["isError"] != true, "{r}");
    let content = r["content"].as_array().cloned().unwrap_or_default();
    let reply: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    (content[1]["data"].as_str().unwrap().to_string(), reply)
}

fn hash(s: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(s.as_bytes())
}

/// Pinned on the code before Stage 4a (2c074fc): a scene without a Poly
/// renders bit for bit as before, and a Poly with `subdiv_max_level: 0`
/// renders its drawn level as before. (The hashes are this machine's:
/// the CPU tracer is deterministic here, not across platforms.)
#[test]
fn film_without_poly_dicing_is_unchanged() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let args = json!({ "size": 64, "samples": 4, "device": "cpu", "denoise": false, "views": ["front"], "time_limit": 300 });
    ok(app, ctx, tx, "create_object", json!({ "shape": "Sphere", "name": "Plain", "translate": [0.6, 0.0, 0.0] }));
    ok(app, ctx, tx, "set_material", json!({ "name": "rockring", "detail": "stone", "detail_scale": 0.3, "detail_displace": 0.03 }));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Torus", "name": "Ring", "translate": [-0.2, 0.5, 0.0], "material": "rockring" }));
    settle(app, ctx);
    let (plain, reply) = film(app, ctx, tx, args.clone());
    eprintln!("plain {:#x} {reply}", hash(&plain));
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Ball", "resolution": 32, "blobs": [{ "type": "sphere", "center": [-0.6, 0, 0], "radius": 0.4 }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "poly", json!({ "action": "from_clay", "source": "Ball" }));
    settle(app, ctx);
    ok(app, ctx, tx, "poly", json!({ "target": "Ball_poly", "action": "subdivide", "levels": 1 }));
    ok(app, ctx, tx, "poly", json!({ "target": "Ball_poly", "action": "set_level", "level": 1 }));
    settle(app, ctx);
    let mut off = args.clone();
    off["subdiv_max_level"] = json!(0);
    let (drawn, reply) = film(app, ctx, tx, off);
    eprintln!("drawn {:#x} {reply}", hash(&drawn));
    assert_eq!((hash(&plain), hash(&drawn)), (0xd659_6590_8244_9c5a, 0xb363_c4ed_3ac4_4c0b), "pictures changed");
}

/// `render_film` subdivides a projected Poly by default and says how
/// (`subdivision`), `displacement: false` changes the picture, a tight
/// `max_diced` dices coarser and says so, one below level 1 renders the
/// Poly at its shown level with a note naming the fix (4a review fix: it
/// was refused, so scenes that rendered before 4a failed), out-of-range
/// arguments are refused before any work, and `subdiv_max_level: 0`
/// renders the shown level.
#[test]
fn render_film_subdivides_polys() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Clay", "name": "Bumpy", "resolution": 40, "detail_cell": 0.012, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] }));
    ok(app, ctx, tx, "sculpt", json!({ "target": "Bumpy", "strokes": [{ "brush": "noise", "point": [0, 0, 0], "radius": 0.9, "strength": 1.0, "scale": 0.08, "pattern": "bumps" }] }));
    settle(app, ctx);
    ok(app, ctx, tx, "poly", json!({ "action": "from_clay", "source": "Bumpy" }));
    settle(app, ctx);
    ok(app, ctx, tx, "poly", json!({ "target": "Bumpy_poly", "action": "project", "source": "Bumpy", "levels": 2 }));
    settle(app, ctx);
    let args = json!({ "size": 320, "samples": 2, "device": "cpu", "denoise": false, "views": ["front"], "time_limit": 300 });
    let (on, reply) = film(app, ctx, tx, args.clone());
    eprintln!("{reply}");
    let s = &reply["subdivision"][0];
    assert_eq!(s["object"], "Bumpy_poly", "{reply}");
    assert_eq!(s["displaced"], true);
    let levels = s["levels"].as_array().unwrap();
    assert!(levels[1].as_u64().unwrap() >= 2, "{s}");
    let diced = s["triangles"].as_u64().unwrap();
    let mut off = args.clone();
    off["displacement"] = json!(false);
    let (smooth, r) = film(app, ctx, tx, off);
    assert_eq!(r["subdivision"][0]["displaced"], false);
    assert_ne!(on, smooth, "the displacement shows");
    let level1 = s["cage_faces"].as_u64().unwrap() * 8;
    assert!(diced > 2 * level1, "{s}");
    let mut tight = args.clone();
    tight["max_diced"] = json!(diced / 2);
    let (_, r) = film(app, ctx, tx, tight);
    assert!(r["subdivision"][0]["triangles"].as_u64().unwrap() <= diced / 2, "{r}");
    assert!(r["subdivision"][0]["note"].as_str().unwrap().contains("coarser"), "{r}");
    let mut shown = args.clone();
    shown["subdiv_max_level"] = json!(0);
    let (shown_picture, r) = film(app, ctx, tx, shown);
    assert_eq!(r["subdivision"], json!([]));
    // Refusals.
    let refused = |app: &mut ClideApp, extra: Value, says: &str| {
        let mut a = args.clone();
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        let (v, err) = super::super::recorder::tests::mcp(app, ctx, tx, "render_film", a);
        assert!(err && v.to_string().contains(says), "{v}");
    };
    refused(app, json!({ "subdiv_max_level": 9 }), "subdiv_max_level");
    refused(app, json!({ "dice_rate": 0 }), "dice_rate");
    refused(app, json!({ "max_diced": 5 }), "max_diced");
    refused(app, json!({ "displacement": 1 }), "displacement");
    let faces = s["cage_faces"].as_u64().unwrap();
    assert!(faces * 8 > 10_000, "level 1 alone is more than the smallest budget");
    let mut over = args.clone();
    over["max_diced"] = json!(10_000);
    let (picture, r) = film(app, ctx, tx, over);
    let s = &r["subdivision"][0];
    assert_eq!(s["triangles"], 0, "{r}");
    assert!(s["note"].as_str().unwrap().contains("shown level") && s["note"].as_str().unwrap().contains("max_diced"), "{r}");
    assert_eq!(picture, shown_picture, "the same picture as subdiv_max_level: 0");
    // 4a review fix 6: with motion blur the reply counts the diced
    // triangles (it counted the drawn meshes'), as a still frame does.
    let mut still = args.clone();
    still["frame"] = json!(0);
    let (_, r_still) = film(app, ctx, tx, still.clone());
    let mut blur = still;
    blur["motion_blur"] = json!({ "steps": 2 });
    let (_, r_blur) = film(app, ctx, tx, blur);
    eprintln!("still {} blur {}", r_still["triangles"], r_blur["triangles"]);
    assert_eq!(r_blur["triangles"], r_still["triangles"], "{r_blur}");
    assert!(r_blur["triangles"].as_u64().unwrap() >= diced, "{r_blur}");
}
