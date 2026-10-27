//! Character follow-ups, feature 3: fur over a clay's body. On Pip (the
//! user's character, a copy in tests/fixtures, with the new lids), `hair
//! grow {fur: true}` spreads 2,000 guide roots evenly by area, grows the
//! film strands in their cells with no root on an excluded part or in an
//! eye's sphere, none inside the clay or into an eye, each coloured from
//! the clay at its root (blue body, pale belly), the same every time and
//! on any thread count, kept by save and load and taken back by one undo;
//! bad sizes are refused before anything is made. Timings are printed.

use std::collections::HashSet;

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::pose_split_tests::{headless, open_pip};
use super::recorder::tests::{mcp, ok, settle};
use crate::hair::Hair;

const EXCLUDE: [&str; 6] = ["nose", "mouth", "mouth_line", "lid.L", "eye_socket.L", "brow.L"];

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

fn hair(app: &ClideApp, name: &str) -> Hair {
    app.scene.get(id_of(app, name)).unwrap().hair.clone().unwrap()
}

/// Pip with the new lids and fur grown over it.
fn furry(app: &mut ClideApp, ctx: &egui::Context, tx: &std::sync::mpsc::Sender<crate::mcp::ToolCall>) -> Value {
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "remove": ["lid.L"], "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.42, "mirror": true }] }));
    settle(app, ctx);
    let t = std::time::Instant::now();
    let r = ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Pip_fur", "parent": "Pip", "hair": { "grow": {
        "fur": true, "on": "Pip", "exclude": EXCLUDE, "lengths": [{ "center": [0, 0.72, 0.17], "radius": 0.12, "length": 0.005 }] } } }));
    eprintln!("grow fur over Pip: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    settle(app, ctx);
    r
}

/// `pip_fur_covers_evenly_and_stays_out`: see the module. Root spacing
/// varies by under a fifth (blue noise; random roots vary by a half); no
/// guide or strand root is on an excluded blob (either side) or within an
/// eye's sphere; no strand enters the clay or an eye; every strand's root
/// colour is the clay's colour at its root (linear, within 0.01 for all
/// but 0.5%, the strands kept out of the clay at a colour edge), so the
/// body's strands are blue and the belly's pale. The strand roots are
/// spread evenly too (sunflower spirals in each guide's cell).
#[test]
fn pip_fur_covers_evenly_and_stays_out() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    let r = furry(app, ctx, tx);
    let grown = &r["hair"]["grown"];
    eprintln!("grown: {grown}");
    let h = hair(app, "Pip_fur");
    let fur = h.fur.clone().expect("fur");
    assert_eq!(h.guides.len(), 2000);
    assert!(h.strands <= crate::hair::MAX_STRANDS && h.strands >= 100_000, "{}", h.strands);
    let roots: Vec<Vec3> = h.guides.iter().map(|g| g.points[0]).collect();
    let (mean, cv) = crate::hair::fur::spacing_stats(&roots);
    eprintln!("guide roots: mean nearest spacing {mean:.4}, cv {cv:.3}");
    assert!(cv < 0.2, "{cv}");
    // Strands, as film builds them.
    let t = std::time::Instant::now();
    let kept = app.hair_strands(id_of(app, "Pip_fur"), None);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(kept.error.is_none());
    let strands = kept.strands;
    eprintln!("{} film strands of {} asked in {ms:.0} ms ({} kept out of the clay)", strands.len(), h.strands, kept.moved);
    assert!(strands.len() as f32 > 0.9 * h.strands as f32);
    let (smean, scv) = crate::hair::fur::spacing_stats(&strands.iter().map(|s| s.points[0]).collect::<Vec<_>>());
    eprintln!("strand roots: mean nearest spacing {smean:.5}, cv {scv:.3}");
    assert!(scv < 0.35, "{scv}");
    // The head as film sees it (the hair sits at the clay's origin).
    let pip = id_of(app, "Pip");
    let head = app.head_collider(pip).unwrap();
    let inside = crate::hair::inside_count(&strands, |p| head.inside(p));
    eprintln!("strands inside the clay: {inside}");
    assert_eq!(inside, 0);
    let clay = crate::layers::composed_clay(app.scene.get(pip).unwrap().clay.as_ref().unwrap());
    let field = crate::clay::Field::new(&clay, clay.cell_size().unwrap());
    let excluded: HashSet<String> = EXCLUDE.iter().flat_map(|n| [n.to_string(), crate::pose::swap_side_token(n).unwrap_or_else(|| n.to_string())]).collect();
    let eyes: Vec<(Vec3, f32)> = ["Pip_eye.L", "Pip_eye.R"].iter().map(|e| {
        let m = app.scene.rest_world_matrix(id_of(app, e));
        (m.transform_point3(Vec3::ZERO), 0.5 * m.to_scale_rotation_translation().0.x)
    }).collect();
    let mut on_excluded = 0;
    let mut near_eye = 0;
    let mut into_eye = 0;
    let mut color_err = 0.0f32;
    let (mut sampled, mut off_color) = (0usize, 0usize);
    let (mut blue, mut pale) = (0usize, 0usize);
    let all: Vec<u32> = (0..field.prim_count() as u32).collect();
    for s in strands.iter().step_by(7) {
        // The root on the surface (the strand starts a hair's lift off it).
        let root = head.snap(s.points[0]).map_or(s.points[0], |(q, _)| q);
        if field.owner_name(root).is_some_and(|o| excluded.contains(&o)) {
            on_excluded += 1;
        }
        if eyes.iter().any(|(c, r)| root.distance(*c) < *r * 1.15) {
            near_eye += 1;
        }
        if s.points.iter().any(|p| eyes.iter().any(|(c, r)| p.distance(*c) < *r)) {
            into_eye += 1;
        }
        let c = field.sample(root, &all).1.clamp(Vec3::ZERO, Vec3::ONE).powf(2.2);
        let e = (c - s.root_color).abs().max_element();
        color_err = color_err.max(e);
        sampled += 1;
        if e > 0.01 {
            off_color += 1;
        }
        if s.root_color.z > 1.5 * s.root_color.x { blue += 1; }
        if s.root_color.min_element() > 0.5 { pale += 1; }
    }
    eprintln!("sampled every 7th strand ({sampled}): {on_excluded} on excluded blobs, {near_eye} rooted by an eye, {into_eye} into an eye; root colour off the clay's by at most {color_err:.4}, by more than 0.01 on {off_color}; {blue} blue, {pale} pale");
    assert_eq!((on_excluded, near_eye, into_eye), (0, 0, 0));
    // Where a strand was kept out of the clay its first point moved, and
    // the surface under it is a little off its root: a colour edge (the
    // nose's 6 mm fade) shows there.
    assert!(off_color * 200 < sampled && color_err < 0.06, "{off_color} of {sampled}, {color_err}");
    assert!(blue > 10 * pale && pale > 500, "a blue body and a pale belly: {blue} {pale}");
    let guides_off = h.guides.iter().filter(|g| {
        let root = head.snap(g.points[0]).map_or(g.points[0], |(q, _)| q);
        field.owner_name(root).is_some_and(|o| excluded.contains(&o))
    }).count();
    assert_eq!(guides_off, 0, "guides on excluded blobs");
    // Lengths: short on the face, the default elsewhere.
    let len = |g: &crate::hair::Guide| g.points.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
    let face: Vec<f32> = h.guides.iter().filter(|g| g.points[0].distance(Vec3::new(0.0, 0.72, 0.17)) < 0.1).map(len).collect();
    let body: Vec<f32> = h.guides.iter().filter(|g| g.points[0].y < 0.5).map(len).collect();
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    eprintln!("guide length on the face {:.4} ({} guides), on the body {:.4}", mean(&face), face.len(), mean(&body));
    assert!(mean(&face) < 0.006 && mean(&body) > 0.01);
    assert!(fur.avoid.len() == 2 && fur.exclude.contains(&"lid.R".to_string()));
}

/// `fur_is_deterministic_and_round_trips`: the same grow twice gives the
/// same guides; the strands are the same bits on 1 and 12 threads; save,
/// new and open keep the hair (guides and fur settings) and its strands;
/// one undo takes the fur away; the viewport's cards (one a guide) build
/// and lie outside the clay. Prints the strand and card timings.
#[test]
fn fur_is_deterministic_and_round_trips() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    furry(app, ctx, tx);
    let first = hair(app, "Pip_fur");
    let id = id_of(app, "Pip_fur");
    let one = crate::clay::with_threads(1, || app.hair_strands(id, Some(20_000)).strands);
    let many = app.hair_strands(id, Some(20_000)).strands;
    assert_eq!(one.len(), many.len());
    assert!(one.iter().zip(&many).all(|(a, b)| a.points == b.points && a.root_color == b.root_color && a.root_width == b.root_width), "1 and 12 threads");
    // Cards.
    let pip = id_of(app, "Pip");
    let head = app.head_collider(pip).unwrap();
    let t = std::time::Instant::now();
    let (cards, moved) = first.build_mesh_kept_out(Some(&crate::groom::head::InHair::new(&head, app.scene.rest_world_matrix(id), first.lift)));
    eprintln!("viewport cards: {} triangles in {:.0} ms ({moved} kept out)", cards.indices.len() / 3, t.elapsed().as_secs_f64() * 1e3);
    assert_eq!(cards.indices.len() / 3, 2000 * 6 * 4);
    assert_eq!(cards.vertices.iter().filter(|v| head.inside(Vec3::from(v.pos))).count(), 0, "no card vertex inside");
    // Save and load.
    let d = std::env::temp_dir().join(format!("clide3d_fur_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_fur.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    assert_eq!(hair(app, "Pip_fur"), first, "save and load keep the fur");
    let again = app.hair_strands(id_of(app, "Pip_fur"), Some(20_000)).strands;
    assert!(again.iter().zip(&many).all(|(a, b)| a.points == b.points && a.root_color == b.root_color));
    let _ = std::fs::remove_dir_all(&d);
    // The same grow again: the same guides.
    furry(app, ctx, tx);
    assert_eq!(hair(app, "Pip_fur"), first, "deterministic");
    let step = app.history.current();
    ok(app, ctx, tx, "update_object", json!({ "target": "Pip_fur", "hair": { "fur": { "tint": [1.1, 1.0, 0.9] } } }));
    assert_eq!(hair(app, "Pip_fur").fur.unwrap().tint, [1.1, 1.0, 0.9]);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(app.history.current(), step);
    assert_eq!(hair(app, "Pip_fur"), first);
    ok(app, ctx, tx, "undo", json!({}));
    assert!(app.scene.objects.iter().all(|o| o.name != "Pip_fur"), "one undo takes the fur away");
}

/// `fur_refusals`: guides past 4,096, strands past the film budget, a
/// density whose strands over Pip's area pass it, an unknown key, a bad
/// lie, a style with fur, and an excluded part that leaves nothing are
/// refused, each with the scene unchanged.
#[test]
fn fur_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Fur", "parent": "Pip" }));
    let scene = app.scene.clone();
    for (grow, says) in [
        (json!({ "fur": true, "on": "Pip", "count": 5000 }), "1 to 4096"),
        (json!({ "fur": true, "on": "Pip", "strands": 300000 }), "film strand budget"),
        (json!({ "fur": true, "on": "Pip", "density": 1.0e7 }), "film budget is 200000"),
        (json!({ "fur": true, "on": "Pip", "fluff": 1 }), "unknown fur grow key"),
        (json!({ "fur": true, "on": "Pip", "lie": 2 }), "grow.lie"),
        (json!({ "fur": true, "on": "Pip", "style": "drape" }), "fur or style"),
        (json!({ "fur": true, "on": "Pip", "include": [{ "center": [5, 5, 5], "radius": 0.1 }] }), "nothing"),
        (json!({ "fur": true, "on": "Pip", "lengths": [{ "blobs": ["head"], "length": -1 }] }), "length must be above 0"),
    ] {
        let (v, err) = mcp(app, ctx, tx, "update_object", json!({ "target": "Fur", "hair": { "grow": grow.clone() } }));
        assert!(err && v.to_string().contains(says), "{grow}: {v}");
        assert_eq!(app.scene, scene, "{grow}: nothing changed");
    }
}


/// Pip with its lid replaced by one hiding `cover` of the eye (mirrored).
fn pip_with_lids(app: &mut ClideApp, ctx: &egui::Context, tx: &std::sync::mpsc::Sender<crate::mcp::ToolCall>, cover: f32) {
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "remove": ["lid.L"], "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": cover, "mirror": true }] }));
    settle(app, ctx);
}

/// `lids_grow_short_fur_to_a_clean_edge`: with the lids not excluded,
/// fur grows on them (`Fur::lids`): very short (0.35 of the length),
/// three strands a root, lying on the cap toward the margin and stopping
/// short of it, so no lid strand reaches the margin's plane or the eye
/// and none is inside the clay; elsewhere the eyes' bare spheres hold.
/// The strands are the same on 1 and 12 threads, save and load keep the
/// lids, `lid_length: 0` leaves them bare (and stores nothing), and a bad
/// lid length is refused with the scene unchanged.
#[test]
fn lids_grow_short_fur_to_a_clean_edge() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    pip_with_lids(app, ctx, tx, 0.25);
    let t = std::time::Instant::now();
    let r = ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Pip_fur", "parent": "Pip", "hair": { "grow": {
        "fur": true, "on": "Pip", "exclude": ["nose", "mouth", "mouth_line"] } } }));
    eprintln!("grow fur over Pip with furred lids: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    settle(app, ctx);
    let grown = &r["hair"]["grown"];
    let (length, lid_length) = (grown["length"].as_f64().unwrap() as f32, grown["lid_length"].as_f64().unwrap() as f32);
    eprintln!("lids furred {}, lid guides {}, lid length {lid_length:.4} (fur {length:.4})", grown["lids_furred"], grown["lid_guides"]);
    assert_eq!(grown["lids_furred"], 2);
    assert!(grown["lid_guides"].as_u64().unwrap() >= 10);
    assert!((lid_length - 0.35 * length).abs() < 1e-6);
    let h = hair(app, "Pip_fur");
    let fur = h.fur.clone().unwrap();
    assert_eq!(fur.lids.len(), 2);
    assert!(fur.lids.iter().all(|l| l.copies == 3), "{:?}", fur.lids);
    // Mirror images of each other (one mirrored lid blob).
    let (a, b) = (fur.lids[0], fur.lids[1]);
    assert!((a.center[0] + b.center[0]).abs() < 1e-5 && (a.axis[0] + b.axis[0]).abs() < 1e-5 && (a.center[1] - b.center[1]).abs() < 1e-6);
    let id = id_of(app, "Pip_fur");
    let kept = app.hair_strands(id, None);
    let strands = kept.strands;
    let pip = id_of(app, "Pip");
    let head = app.head_collider(pip).unwrap();
    let eyes: Vec<(Vec3, f32)> = ["Pip_eye.L", "Pip_eye.R"].iter().map(|e| {
        let m = app.scene.rest_world_matrix(id_of(app, e));
        (m.transform_point3(Vec3::ZERO), 0.5 * m.to_scale_rotation_translation().0.x)
    }).collect();
    let mut per_lid = [0usize; 2];
    let (mut below_margin, mut into_eye, mut too_long, mut total_len) = (0usize, 0usize, 0usize, 0.0f32);
    let len = |s: &crate::hair::Strand| s.points.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
    for s in &strands {
        let Some(k) = fur.lids.iter().position(|l| l.holds(s.points[0])) else { continue };
        let l = fur.lids[k];
        per_lid[k] += 1;
        let (c, n) = (Vec3::from(l.center), Vec3::from(l.axis));
        if s.points.iter().any(|p| (*p - c).dot(n) < 0.5 * l.half) {
            below_margin += 1;
        }
        if s.points.iter().any(|p| eyes.iter().any(|(e, r)| p.distance(*e) < *r * 1.02)) {
            into_eye += 1;
        }
        let sl = len(s);
        total_len += sl;
        if sl > 1.15 * l.length {
            too_long += 1;
        }
    }
    let lid_strands = per_lid[0] + per_lid[1];
    eprintln!("lid strands {per_lid:?}, mean length {:.4}; {below_margin} reach the margin, {into_eye} into an eye, {too_long} too long", total_len / lid_strands.max(1) as f32);
    assert!(per_lid[0] > 500 && per_lid[1] > 500, "{per_lid:?}");
    assert_eq!((below_margin, into_eye, too_long), (0, 0, 0));
    let mean = total_len / lid_strands as f32;
    assert!(mean < lid_length && mean > 0.7 * lid_length, "{mean}");
    assert_eq!(crate::hair::inside_count(&strands, |p| head.inside(p)), 0);
    // Off the lids the eyes' bare spheres still hold.
    let bare_off_lid = strands.iter().filter(|s| !fur.lids.iter().any(|l| l.holds(s.points[0]))).filter(|s| fur.bare.iter().any(|b| s.points[0].distance(Vec3::new(b[0], b[1], b[2])) < b[3] - 0.003)).count();
    assert_eq!(bare_off_lid, 0);
    // 1 and 12 threads.
    let one = crate::clay::with_threads(1, || app.hair_strands(id, None).strands);
    assert!(one.len() == strands.len() && one.iter().zip(&strands).all(|(a, b)| a.points == b.points && a.root_color == b.root_color && a.root_width == b.root_width), "1 and 12 threads");
    // Save and load keep the lids.
    let d = std::env::temp_dir().join(format!("clide3d_lidfur_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_lidfur.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    assert_eq!(hair(app, "Pip_fur"), h, "save and load keep the lids' fur");
    let _ = std::fs::remove_dir_all(&d);
    // lid_length 0: bare lids, nothing stored; one undo gives the furred lids back.
    ok(app, ctx, tx, "update_object", json!({ "target": "Pip_fur", "hair": { "grow": { "fur": true, "on": "Pip", "exclude": ["nose", "mouth", "mouth_line"], "lid_length": 0 } } }));
    let bare = hair(app, "Pip_fur");
    assert!(bare.fur.as_ref().unwrap().lids.is_empty());
    assert!(serde_json::to_value(&bare).unwrap()["fur"].get("lids").is_none());
    // (Past the eye's bare sphere the lid's blend into the brow grows the
    // body's fur either way.)
    let none_on_lids = app.hair_strands(id_of(app, "Pip_fur"), None).strands.iter().filter(|s| fur.lids.iter().any(|l| l.holds(s.points[0])) && fur.bare.iter().any(|b| s.points[0].distance(Vec3::new(b[0], b[1], b[2])) < b[3] - 0.003)).count();
    eprintln!("lid_length 0: {none_on_lids} strands on the lids inside the eyes' bare spheres");
    assert_eq!(none_on_lids, 0);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(hair(app, "Pip_fur"), h, "one undo");
    // Refused, nothing changed.
    let scene = app.scene.clone();
    for bad in [json!(-0.01), json!(3.5), json!("short")] {
        let (v, err) = mcp(app, ctx, tx, "update_object", json!({ "target": "Pip_fur", "hair": { "grow": { "fur": true, "on": "Pip", "lid_length": bad } } }));
        assert!(err && v.to_string().contains("lid_length"), "{v}");
        assert_eq!(app.scene, scene);
    }
}

/// `brows_grow_their_own_fur`: `lengths` and `direction` regions apply to
/// every short strand by its own root (`Fur::regions`), not only to the
/// few guides rooted in them, so a brow (a thin capsule only a handful
/// of guides land on) grows its own longer fur lying its own way, and a
/// strand outside every length region keeps the fur's length even where
/// its guide was rooted in one. Save and load keep the regions; a grow
/// without regions stores none.
#[test]
fn brows_grow_their_own_fur() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    pip_with_lids(app, ctx, tx, 0.25);
    let brow = 0.026f32;
    let way = Vec3::new(0.8, 0.6, 0.0).normalize();
    let grow = |regions: bool| {
        let mut g = json!({ "fur": true, "on": "Pip", "exclude": ["nose", "mouth", "mouth_line"] });
        if regions {
            g["lengths"] = json!([{ "blobs": ["brow.L"], "length": brow }, { "center": [0, 0.72, 0.17], "radius": 0.12, "length": 0.005 }]);
            g["direction"] = json!([{ "blobs": ["brow.L"], "direction": [way.x, way.y, way.z] }]);
        }
        g
    };
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Pip_fur", "parent": "Pip", "hair": { "grow": grow(false) } }));
    settle(app, ctx);
    assert!(hair(app, "Pip_fur").fur.unwrap().regions.is_empty());
    let pip = id_of(app, "Pip");
    let head = app.head_collider(pip).unwrap();
    let names: Vec<String> = vec!["brow.L".into(), "brow.R".into()];
    let len = |s: &crate::hair::Strand| s.points.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
    let on_brows = |app: &mut ClideApp| -> (usize, f32, f32, f32) {
        let strands = app.hair_strands(id_of(app, "Pip_fur"), None).strands;
        let on: Vec<&crate::hair::Strand> = strands.iter().filter(|s| head.owned_by(s.points[0], &names)).collect();
        let mean = on.iter().map(|s| len(s)).sum::<f32>() / on.len().max(1) as f32;
        // How well they lie along the way (flattened on the normal).
        let along = on.iter().filter_map(|s| {
            let n = head.normal_at(s.points[0])?;
            let d = s.points[s.points.len() - 1] - s.points[0];
            let f = (d - n * n.dot(d)).try_normalize()?;
            let w = (way - n * n.dot(way)).try_normalize()?;
            Some(f.dot(w))
        }).sum::<f32>() / on.len().max(1) as f32;
        // Just outside the face's length region (where guides rooted in
        // it lend their 5 mm to strands rooted outside it), off the lids
        // and the brows: the mean length.
        let lids = hair(app, "Pip_fur").fur.unwrap().lids;
        let root = |s: &crate::hair::Strand| head.snap(s.points[0]).map_or(s.points[0], |(q, _)| q);
        let by_lid = |q: Vec3| lids.iter().any(|l| (q - Vec3::from(l.center)).dot(Vec3::from(l.axis)) >= 0.0 && q.distance(Vec3::from(l.center)) < l.reach + 0.003);
        let ring: Vec<f32> = strands.iter().filter(|s| {
            let q = root(s);
            let d = q.distance(Vec3::new(0.0, 0.72, 0.17));
            d > 0.122 && d < 0.14 && !by_lid(q) && !head.owned_by(q, &names)
        }).map(len).collect();
        let short = ring.iter().sum::<f32>() / ring.len().max(1) as f32;
        (on.len(), mean, along, short)
    };
    let (n0, mean0, along0, ring0) = on_brows(app);
    ok(app, ctx, tx, "update_object", json!({ "target": "Pip_fur", "hair": { "grow": grow(true) } }));
    settle(app, ctx);
    let (n1, mean1, along1, ring1) = on_brows(app);
    eprintln!("brow strands: copying their guides {n0}, mean length {mean0:.4}, along the way {along0:.3}; by their own roots {n1}, mean length {mean1:.4}, along the way {along1:.3}; strands just outside the face's region {ring0:.4} and {ring1:.4} long");
    assert_eq!(n0, n1, "the same roots");
    assert!(mean0 < 0.016 && mean1 > 0.85 * brow && mean1 < brow, "{mean0} {mean1}");
    // (brow.L names both brows: both lie this one way.)
    assert!(along1 > 0.95 && along0 < 0.5, "{along0} {along1}");
    // Strands rooted just outside the face's region no longer copy the
    // 5 mm of a guide rooted in it.
    assert!(ring1 > ring0 && ring1 > 0.8 * 0.0133, "{ring0} {ring1}");
    let id = id_of(app, "Pip_fur");
    let one = crate::clay::with_threads(1, || app.hair_strands(id, Some(30_000)).strands);
    let many = app.hair_strands(id, Some(30_000)).strands;
    assert!(one.len() == many.len() && one.iter().zip(&many).all(|(a, b)| a.points == b.points && a.root_color == b.root_color), "1 and 12 threads");
    let h = hair(app, "Pip_fur");
    let fur = h.fur.clone().unwrap();
    assert_eq!(fur.regions.len(), 3);
    assert!((fur.length - 0.0133).abs() < 2e-4, "{}", fur.length);
    let d = std::env::temp_dir().join(format!("clide3d_browfur_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_browfur.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    assert_eq!(hair(app, "Pip_fur"), h, "save and load keep the regions");
    let _ = std::fs::remove_dir_all(&d);
}
