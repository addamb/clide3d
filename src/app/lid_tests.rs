//! Character follow-ups, feature 2: eyelids that wrap the eyeball. On
//! Pip (the user's character, a copy in tests/fixtures), the old pillow
//! lids are replaced by `edit_clay {type: "lid", eye}` lids: no point of
//! the eyeball or cornea under the lid shows from the front (no glass
//! strip), none is inside the lid, the opening is the asked cover, the
//! clay still passes quality_report's lumpiness, a front close-up shows
//! the lid's edge bounding the white and iris, and one eye can close more
//! than the other through the per-side split. `create_character` makes
//! the same lids (`proportions.lids`).

use glam::{Mat4, Vec3};
use serde_json::json;

use super::ClideApp;
use super::pose_split_tests::{headless, open_pip, pip_clay};
use super::recorder::tests::{mcp, ok, settle};
use crate::clay::{Clay, Field, Form};

fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

/// The eye's matrix into the clay's space, at rest.
fn eye_to_clay(app: &ClideApp, clay: &str, eye: &str) -> Mat4 {
    app.scene.rest_world_matrix(id_of(app, clay)).inverse() * app.scene.rest_world_matrix(id_of(app, eye))
}

/// The eye's ball and cornea vertices (clay space): sclera, iris and
/// cornea slots of its mesh.
fn eye_points(app: &ClideApp, clay: &str, eye: &str) -> Vec<Vec3> {
    let o = app.scene.get(id_of(app, eye)).unwrap();
    let mesh = o.eye.as_ref().unwrap().build_mesh();
    let m = eye_to_clay(app, clay, eye);
    mesh.vertices.iter().filter(|v| v.material <= 2).map(|v| m.transform_point3(Vec3::from(v.pos))).collect()
}

/// What a lid hides of its eye, seen along forward through the eye's
/// middle: the part of its height (from the top, or the bottom for a
/// lower lid) where a ray meets the clay before the ball.
fn hidden(field: &Field, c: Vec3, r: f32, forward: Vec3, up: Vec3, lower: bool) -> f32 {
    let all: Vec<u32> = (0..field.prim_count() as u32).collect();
    let steps = 600;
    // Whether the ray down forward at height y meets the clay before the
    // ball.
    let blocked = |y: f32| {
        let start = c + up * y + forward * (3.0 * r);
        for k in 0..steps {
            let p = start - forward * (3.0 * r * k as f32 / steps as f32);
            if (p - c).length() < r {
                return false;
            }
            if field.sample(p, &all).0 < 0.0 {
                return true;
            }
        }
        false
    };
    // From the lid's side toward the middle, the first height whose ray
    // reaches the ball: the opening's edge.
    let n = 400;
    let ys = (0..=n).map(|i| r - 2.0 * r * i as f32 / n as f32);
    if lower {
        let edge = ys.rev().find(|&y| !blocked(y)).unwrap_or(r);
        (edge + r) / (2.0 * r)
    } else {
        let edge = ys.into_iter().find(|&y| !blocked(y)).unwrap_or(-r);
        (r - edge) / (2.0 * r)
    }
}

/// The lid's cap: its centre, axis, middle radius and half thickness.
fn cap(clay: &Clay, name: &str) -> (Vec3, Vec3, f32, f32, bool) {
    let b = clay.blobs.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no lid {name}"));
    let (c, n, rm, h) = crate::clay::lid_geometry(&b.form);
    let f = |v: Vec3| if b.flip { Vec3::new(-v.x, v.y, v.z) } else { v };
    (f(c), f(n), rm, h, b.flip)
}

/// `pip_lids_wrap_the_eyes`: Pip's ellipsoid lids replaced by lids from
/// the eyes (upper_cover 0.42, mirrored). Over every vertex of the eye's
/// ball and cornea: none is inside the lid (the lid's own field is
/// positive there); every one under the lid (past the margin's bead)
/// has lid overhead along its radius and is hidden from the front; the
/// opening hides 0.42 of the eye within 0.03; the clay passes
/// quality_report's lumpiness.
#[test]
fn pip_lids_wrap_the_eyes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    let r = ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "remove": ["lid.L"], "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.42, "mirror": true }] }));
    settle(app, ctx);
    assert!(r["notes"][0].as_str().unwrap().contains("lid.L\" from Pip_eye.L"), "{r}");
    let clay = pip_clay(app);
    let lid = clay.blobs.iter().find(|b| b.name == "lid.L").unwrap();
    assert!(lid.mirror && lid.bone == "head" && lid.blend > 0.0);
    let Form::Lid { radius, gap, thickness, cover, .. } = lid.form else { panic!("{lid:?}") };
    let eye = app.scene.get(id_of(app, "Pip_eye.L")).unwrap();
    let d = eye.scale.x;
    assert!((radius - 0.5 * d).abs() < 1e-6 && cover == 0.42);
    let bulge = eye.eye.as_ref().unwrap().cornea_bulge() * d;
    let cell = clay.cell_size().unwrap();
    eprintln!("Pip's lid: eye radius {radius:.4}, cornea bulge {bulge:.4}, gap {gap:.4}, thickness {thickness:.4} (cell {cell:.4})");
    assert!(gap > bulge && gap < bulge + cell, "inside: the ball, the bulge and a small clearance");
    assert!(thickness >= 3.0 * cell - 1e-6);
    let (c, n, rm, h, _) = cap(&clay, "lid.L");
    // None of the eye inside the lid.
    // The left lid by itself (mirror off: prim 0 is then this side).
    let alone = Clay { blobs: vec![crate::clay::Blob { mirror: false, ..lid.clone() }], resolution: clay.resolution, ..Default::default() };
    let lid_field = Field::new(&alone, cell);
    let points = eye_points(app, "Pip", "Pip_eye.L");
    let worst = points.iter().map(|p| lid_field.sample(*p, &[0]).0).fold(f32::MAX, f32::min);
    eprintln!("{} eye points: nearest to the lid {worst:.5} outside it", points.len());
    assert!(worst > 0.0, "an eye point is inside the lid: {worst}");
    // Under the lid: overhead and hidden from the front.
    let field = Field::new(&clay, cell);
    let all: Vec<u32> = (0..field.prim_count() as u32).collect();
    let solid = |p: Vec3| field.sample(p, &all).0 < 0.0;
    let forward = eye_to_clay(app, "Pip", "Pip_eye.L").transform_vector3(Vec3::Z).normalize();
    let bead = (h / rm).sin() + 0.02;
    let (mut under, mut bare, mut glass) = (0, 0, 0);
    for p in &points {
        let dir = (*p - c).normalize();
        // Only the front half shows; the rest is in the head.
        if dir.dot(n) < bead || dir.dot(forward) < 0.1 {
            continue;
        }
        under += 1;
        let overhead = (0..=40).any(|k| solid(c + dir * (rm - h + 2.0 * h * k as f32 / 40.0)));
        if !overhead {
            bare += 1;
        }
        // From the front: something solid before the camera.
        let seen = !(1..=200).any(|k| solid(*p + forward * (0.3 * d * k as f32 / 200.0)));
        if seen {
            glass += 1;
        }
    }
    eprintln!("{under} eye points under the lid: {bare} without lid overhead, {glass} seen from the front");
    assert!(under > 1000, "{under}");
    assert_eq!((bare, glass), (0, 0), "no glass strip under the lid");
    let up = Vec3::Y;
    let got = hidden(&field, c, radius, forward, up, false);
    eprintln!("upper_cover 0.42: the lid hides {got:.4} of the eye from the front");
    assert!((got - 0.42).abs() < 0.03, "{got}");
    let q = ok(app, ctx, tx, "quality_report", json!({ "target": "Pip", "proportions": false }));
    eprintln!("quality_report: pass {}, failures {}, lumpiness {}", q["pass"], q["failures"], q["lumpiness"]["overall"]);
    assert!(q["lumpiness"]["overall"].as_f64().unwrap() <= 0.05, "{}", q["lumpiness"]);
    assert!(!q["failures"].to_string().contains("lump"), "{}", q["failures"]);
}

/// `lid_edge_bounds_the_eye_in_a_front_render`: a front close-up of Pip
/// with the new lids (the viewport's render): down the column through
/// the eye's middle, skin (blue) shows from the top of the eye's ball to
/// the lid's edge and the eye (white, iris, pupil) from there down,
/// without a gap; the edge is where the lid's cover puts it (within 4%
/// of the eye's height); in columns across the iris no eye shows above
/// the middle's edge (the margin arches over it).
#[test]
fn lid_edge_bounds_the_eye_in_a_front_render() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "remove": ["lid.L"], "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.42, "mirror": true }] }));
    settle(app, ctx);
    let world = app.scene.rest_world_matrix(id_of(app, "Pip_eye.L"));
    let c = world.transform_point3(Vec3::ZERO);
    let r = 0.5 * app.scene.get(id_of(app, "Pip_eye.L")).unwrap().scale.x;
    let (w, hgt) = (512usize, 512usize);
    let shot = super::eyes::Shot::framing(&json!("front"), c, 1.6 * r).unwrap();
    let img = app.render_shot_extras(&shot, w, hgt, false, false, super::eyes::Extras::default()).unwrap();
    let vp = shot.view_proj(w, hgt);
    let px = |p: Vec3| {
        let q = vp.project_point3(p);
        (((q.x + 1.0) * 0.5 * w as f32) as i32, ((1.0 - q.y) * 0.5 * hgt as f32) as i32)
    };
    let row_y = |row: i32| {
        // World y of a pixel row in the eye's column (orthographic front).
        let (_, top) = px(c + Vec3::Y * r);
        let (_, bottom) = px(c - Vec3::Y * r);
        c.y + r - 2.0 * r * (row - top) as f32 / (bottom - top) as f32
    };
    let eye_pixel = |x: i32, y: i32| {
        let p = img.pixels[y as usize * w + x as usize];
        // Pip's skin is blue; the white, the orange iris and the black
        // pupil are not.
        p.r() as i32 >= p.b() as i32 - 6
    };
    let (cx, top) = px(c + Vec3::Y * r);
    let (_, mid) = px(c);
    let first = (top..=mid).find(|&y| eye_pixel(cx, y)).expect("the eye shows under the lid");
    assert!((first..=mid).all(|y| eye_pixel(cx, y)), "the eye shows without a gap from the lid's edge down");
    assert!((top..first).all(|y| !eye_pixel(cx, y)), "skin from the eye's top to the lid's edge");
    let hid = (c.y + r - row_y(first)) / (2.0 * r);
    eprintln!("front render: the lid's edge in the eye's middle at row {first} (eye top {top}, middle {mid}): it hides {hid:.4} of the eye (asked 0.42)");
    assert!((hid - 0.42).abs() < 0.04, "{hid}");
    for dx in [-0.5f32, -0.25, 0.25, 0.5] {
        let (x, _) = px(c + Vec3::X * dx * r);
        let above = (top..first - 1).filter(|&y| eye_pixel(x, y)).count();
        assert_eq!(above, 0, "column {dx}: eye above the lid's edge");
    }
    let d = std::env::temp_dir().join(format!("clide3d_lids_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_lid_front.png");
    image::RgbaImage::from_raw(w as u32, hgt as u32, img.pixels.iter().flat_map(|p| p.to_array()).collect()).unwrap().save(&path).unwrap();
    eprintln!("front close-up: {}", path.display());
}

/// `one_eye_closes_more`: the mirrored lid split by hand (`edit_clay
/// split`), its right side filled again from the right eye with a bigger
/// cover: the left eye stays at 0.42, the right hides 0.6; one undo gives
/// back the mirrored lid; save and load keep both.
#[test]
fn one_eye_closes_more() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "remove": ["lid.L"], "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.42, "mirror": true }] }));
    let mirrored = pip_clay(app);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "split": ["lid.L"], "update": [{ "blob": "lid.R", "eye": "Pip_eye.R", "upper_cover": 0.6 }] }));
    settle(app, ctx);
    let clay = pip_clay(app);
    let field = Field::new(&clay, clay.cell_size().unwrap());
    for (eye, lid, want) in [("Pip_eye.L", "lid.L", 0.42), ("Pip_eye.R", "lid.R", 0.6)] {
        let (c, _, _, _, _) = cap(&clay, lid);
        let m = eye_to_clay(app, "Pip", eye);
        assert!(c.abs_diff_eq(m.transform_point3(Vec3::ZERO), 1e-5), "{lid} sits on {eye}");
        let r = 0.5 * app.scene.get(id_of(app, eye)).unwrap().scale.x;
        let got = hidden(&field, c, r, m.transform_vector3(Vec3::Z).normalize(), Vec3::Y, false);
        eprintln!("{lid}: hides {got:.4} (asked {want})");
        assert!((got - want).abs() < 0.03, "{lid}: {got}");
    }
    assert!(clay.blobs.iter().find(|b| b.name == "lid.R").unwrap().flip);
    let split = pip_clay(app);
    ok(app, ctx, tx, "undo", json!({}));
    assert_eq!(pip_clay(app), mirrored);
    ok(app, ctx, tx, "redo", json!({}));
    let d = std::env::temp_dir().join(format!("clide3d_lids_rt_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_lids.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    assert_eq!(pip_clay(app), split, "save and load keep the lids");
    let _ = std::fs::remove_dir_all(&d);
    // Refusals: eye on something not a lid, an eye that is not an Eye,
    // two covers, a cover past 1.
    let scene = app.scene.clone();
    for (args, says) in [
        (json!({ "target": "Pip", "add": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "eye": "Pip_eye.L" }] }), "lid blob"),
        (json!({ "target": "Pip", "add": [{ "type": "lid", "eye": "Pip" }] }), "not an Eye"),
        (json!({ "target": "Pip", "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.3, "lower_cover": 0.2 }] }), "one of"),
        (json!({ "target": "Pip", "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 1.3 }] }), "0 (open) to 1"),
        (json!({ "target": "Pip", "add": [{ "type": "lid", "center": [0, 0, 0], "radius": 0.1, "thickness": 0.01, "cover": 0.5, "tilt": 95 }] }), "tilt"),
    ] {
        let (v, err) = mcp(app, ctx, tx, "edit_clay", args.clone());
        assert!(err && v.to_string().contains(says), "{args}: {v}");
        assert_eq!(app.scene, scene);
    }
}

/// `create_character` lids: upper by default (one mirrored lid.L on the
/// head bone, after the sockets), both adds lid_lower.L, none leaves
/// them out; the default base's lid clears its eye's cornea and hides
/// 0.3 of it; covers that close the eye are refused.
#[test]
fn character_lids() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    ok(app, ctx, tx, "create_character", json!({ "name": "Buddy" }));
    settle(app, ctx);
    let clay = app.scene.get(id_of(app, "Buddy")).unwrap().clay.clone().unwrap();
    let names: Vec<&str> = clay.blobs.iter().map(|b| b.name.as_str()).collect();
    let (socket, lid) = (names.iter().position(|n| *n == "eye_socket.L").unwrap(), names.iter().position(|n| *n == "lid.L").unwrap());
    assert!(lid > socket && !names.contains(&"lid_lower.L"));
    let b = &clay.blobs[lid];
    assert!(b.mirror && b.bone == "head");
    let field = Field::new(&clay, clay.cell_size().unwrap());
    let (c, _, _, _, _) = cap(&clay, "lid.L");
    let m = eye_to_clay(app, "Buddy", "Buddy_eye.L");
    assert!(c.abs_diff_eq(m.transform_point3(Vec3::ZERO), 1e-5));
    let r = 0.5 * app.scene.get(id_of(app, "Buddy_eye.L")).unwrap().scale.x;
    let got = hidden(&field, c, r, Vec3::Z, Vec3::Y, false);
    eprintln!("default base: the lid hides {got:.4} (0.3 asked)");
    assert!((got - 0.3).abs() < 0.03);
    let alone = Clay { blobs: vec![crate::clay::Blob { mirror: false, ..b.clone() }], resolution: clay.resolution, ..Default::default() };
    let lf = Field::new(&alone, clay.cell_size().unwrap());
    assert!(eye_points(app, "Buddy", "Buddy_eye.L").iter().all(|p| lf.sample(*p, &[0]).0 > 0.0), "the eye is clear of its lid");
    ok(app, ctx, tx, "create_character", json!({ "name": "Both", "proportions": { "lids": "both", "upper_cover": 0.4, "lower_cover": 0.2 }, "guides": false, "translate": [2, 0, 0] }));
    ok(app, ctx, tx, "create_character", json!({ "name": "Bare", "proportions": { "lids": "none" }, "guides": false, "translate": [-2, 0, 0] }));
    settle(app, ctx);
    let both = app.scene.get(id_of(app, "Both")).unwrap().clay.clone().unwrap();
    assert!(both.blobs.iter().any(|b| b.name == "lid_lower.L" && matches!(b.form, Form::Lid { lower: true, .. })));
    let bf = Field::new(&both, both.cell_size().unwrap());
    let (c, _, _, _, _) = cap(&both, "lid_lower.L");
    let r = 0.5 * app.scene.get(id_of(app, "Both_eye.L")).unwrap().scale.x;
    let low = hidden(&bf, c, r, Vec3::Z, Vec3::Y, true);
    let high = hidden(&bf, c, r, Vec3::Z, Vec3::Y, false);
    eprintln!("lids both: upper hides {high:.4} (0.4), lower {low:.4} (0.2)");
    assert!((high - 0.4).abs() < 0.03 && (low - 0.2).abs() < 0.03);
    assert!(!app.scene.get(id_of(app, "Bare")).unwrap().clay.as_ref().unwrap().blobs.iter().any(|b| b.name.starts_with("lid")));
    let (v, err) = mcp(app, ctx, tx, "create_character", json!({ "name": "Shut", "proportions": { "lids": "both", "upper_cover": 0.6, "lower_cover": 0.5 } }));
    assert!(err && v.to_string().contains("leave the eye open"), "{v}");
}
