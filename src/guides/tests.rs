//! Stage 5b acceptance for the measures (docs/QUALITY_PLAN.md), on
//! synthetic clays whose proportions are known exactly.

use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

use super::*;

fn clay(blobs: Value, resolution: u32) -> Clay {
    serde_json::from_value(json!({ "resolution": resolution, "blobs": blobs })).unwrap()
}

/// A figure of `n` stacked balls of radius 0.1 standing on y = 0 (so
/// exactly `n` heads tall, 0.2 · n), the top one named head, with eyes
/// (eye.L at x = 0.05, mirrored) and mirrored ears without a side suffix.
fn stacked(n: usize, resolution: u32) -> Clay {
    let mut blobs = Vec::new();
    for k in 0..n {
        let y = 0.1 + 0.2 * k as f32;
        let name = if k + 1 == n { "head".to_string() } else { format!("ball{k}") };
        blobs.push(json!({ "type": "sphere", "name": name, "center": [0, y, 0], "radius": 0.1, "blend": 0 }));
    }
    let top = 0.2 * n as f32 - 0.1;
    blobs.push(json!({ "type": "sphere", "name": "eye.L", "center": [0.05, top, 0.08], "radius": 0.02, "blend": 0, "mirror": true }));
    blobs.push(json!({ "type": "sphere", "name": "ear", "center": [0.09, top + 0.06, 0], "radius": 0.025, "blend": 0, "mirror": true }));
    clay(json!(blobs), resolution)
}

fn target(v: Value) -> Target {
    serde_json::from_value(v).unwrap()
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// `measure_head_ratio`: a figure exactly 5 heads tall reads its height,
/// the head's extent and the ratio exactly (1e-5; the plan asks 1e-3),
/// and the head units place every named blob where it is.
#[test]
fn measure_head_ratio() {
    let c = stacked(5, 96);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let (lo, hi) = m.total(Axis::Y).unwrap();
    eprintln!("total y {lo} .. {hi}");
    assert!((lo - 0.0).abs() < 1e-5 && (hi - 1.0).abs() < 1e-5, "{lo} {hi}");
    let (hlo, hhi) = m.blob_extent("head", Axis::Y).unwrap().unwrap();
    assert!((hlo - 0.8).abs() < 1e-5 && (hhi - 1.0).abs() < 1e-5, "{hlo} {hhi}");
    let g = Guides {
        head_units: Some(HeadUnits { count: 5, bottom: 0.0, top: 1.0 }),
        lines: vec![Line { name: "eye_line".into(), x: None, y: Some(0.9), z: None }],
        targets: vec![
            target(json!({ "name": "head_ratio", "of": ["head"], "to": "total_height", "value": 0.2, "tolerance": 0.001 })),
            target(json!({ "name": "heads_tall", "of": ["head", "ball0"], "to": "head_unit", "value": 5.0, "tolerance": 0.001 })),
            target(json!({ "name": "too_big", "of": ["head"], "to": "total_height", "value": 0.33, "tolerance": 0.03 })),
        ],
    };
    g.check().unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("{}", serde_json::to_string_pretty(&out.reply).unwrap());
    let t = &out.reply["targets"];
    assert!(near(t[0]["measured"].as_f64().unwrap(), 0.2, 1e-5) && t[0]["pass"] == true);
    assert!(near(t[1]["measured"].as_f64().unwrap(), 5.0, 1e-4) && t[1]["pass"] == true);
    assert!(t[2]["pass"] == false && near(t[2]["error"].as_f64().unwrap(), -0.13, 1e-5));
    assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
    assert!(out.failures[0].starts_with("proportion too_big"));
    assert_eq!(out.reply["pass"], false);
    let h = &out.reply["head_units"];
    assert!(near(h["model_heads"].as_f64().unwrap(), 5.0, 1e-4));
    let head = h["blobs"].as_array().unwrap().iter().find(|b| b["name"] == "head").unwrap();
    assert!(near(head["top_heads"].as_f64().unwrap(), 0.0, 1e-4) && near(head["bottom_heads"].as_f64().unwrap(), 1.0, 1e-4) && near(head["heads_tall"].as_f64().unwrap(), 1.0, 1e-4), "{head}");
    let ball0 = h["blobs"].as_array().unwrap().iter().find(|b| b["name"] == "ball0").unwrap();
    assert!(near(ball0["bottom_heads"].as_f64().unwrap(), 5.0, 1e-4), "{ball0}");
    assert!(near(out.reply["lines"][0]["heads"].as_f64().unwrap(), 0.5, 1e-5));
    // Other heights: 2, 3 and 8 heads tall read the same way.
    for n in [2, 3, 8] {
        let c = stacked(n, 64);
        let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
        let g = Guides { targets: vec![target(json!({ "name": "r", "of": ["head"], "to": "total_height", "value": 1.0 / n as f32, "tolerance": 1e-4 }))], ..Default::default() };
        let out = measure(&g, &m, &[]);
        assert!(out.failures.is_empty(), "{n}: {}", out.reply);
    }
}

/// Placement: scaled and moved, the figure reads twice the height in
/// world units and the same ratios; the mesh resolution changes nothing.
#[test]
fn measures_are_world_space_and_resolution_free() {
    let c = stacked(4, 96);
    let world = Mat4::from_scale_rotation_translation(Vec3::splat(2.0), Quat::IDENTITY, Vec3::new(1.0, 0.5, -3.0));
    let m = ClayModel::new(&c, world).unwrap();
    let (lo, hi) = m.total(Axis::Y).unwrap();
    assert!((lo - 0.5).abs() < 1e-4 && (hi - 2.1).abs() < 1e-4, "{lo} {hi}");
    let (xlo, xhi) = m.total(Axis::X).unwrap();
    // The ears stand out 0.115 each side.
    assert!((xlo - 0.77).abs() < 1e-4 && (xhi - 1.23).abs() < 1e-4, "{xlo} {xhi}");
    let ratio = |m: &ClayModel| {
        let g = Guides { targets: vec![target(json!({ "name": "r", "of": ["head"], "to": "total_height", "value": 0.25, "tolerance": 1e-4 }))], ..Default::default() };
        measure(&g, m, &[]).reply["targets"][0]["measured"].as_f64().unwrap()
    };
    assert!(near(ratio(&m), 0.25, 1e-5));
    let coarse = stacked(4, 32);
    let fine = stacked(4, 256);
    let (a, b) = (ratio(&ClayModel::new(&coarse, Mat4::IDENTITY).unwrap()), ratio(&ClayModel::new(&fine, Mat4::IDENTITY).unwrap()));
    assert!(near(a, b, 1e-5), "resolution 32 reads {a}, 256 reads {b}");
}

/// Mirrored blobs: `eye.L` is the side as written and `eye.R` its copy,
/// 0.1 apart; a mirrored blob without a side suffix (`ear`) is both
/// sides; distances to a line are signed along its axis.
#[test]
fn mirrored_sides_points_and_lines() {
    let c = stacked(3, 64);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let (l, _) = m.blob_extent("eye.L", Axis::X).unwrap().unwrap();
    let (_, r) = m.blob_extent("eye.R", Axis::X).unwrap().unwrap();
    assert!((l - 0.03).abs() < 1e-5 && (r + 0.03).abs() < 1e-5, "{l} {r}");
    let (elo, ehi) = m.blob_extent("ear", Axis::X).unwrap().unwrap();
    assert!((elo + 0.115).abs() < 1e-5 && (ehi - 0.115).abs() < 1e-5, "{elo} {ehi}");
    assert!(m.blob_extent("nose", Axis::X).is_none());
    let g = Guides {
        head_units: Some(HeadUnits { count: 3, bottom: 0.0, top: 0.6 }),
        lines: vec![Line { name: "eye_line".into(), x: None, y: Some(0.5), z: None }, Line { name: "center".into(), x: Some(0.0), y: None, z: None }],
        targets: vec![
            target(json!({ "name": "eye_spacing", "between": ["eye.L", "eye.R"], "value": 0.1, "tolerance": 0.001 })),
            target(json!({ "name": "eye_spacing_x", "between": ["eye.R", "eye.L"], "axis": "x", "value": 0.1, "tolerance": 0.001 })),
            target(json!({ "name": "eyes_on_line", "between": ["eye_line", "eye.L"], "value": 0.0, "tolerance": 0.001 })),
            target(json!({ "name": "eye_off_center", "between": ["center", "eye.L"], "value": 0.05, "tolerance": 0.001 })),
            target(json!({ "name": "spacing_per_head", "between": ["eye.L", "eye.R"], "per": "head_unit", "value": 0.5, "tolerance": 0.001 })),
            target(json!({ "name": "to_a_point", "between": [[0.05, 0.0, 0.08], "eye.L"], "value": 0.5, "tolerance": 0.001 })),
            target(json!({ "name": "unknown", "between": ["nose", "eye.L"], "value": 0.0, "tolerance": 0.001 })),
            target(json!({ "name": "wrong_axis", "between": ["eye_line", "eye.L"], "axis": "x", "value": 0.0, "tolerance": 0.001 })),
        ],
    };
    g.check().unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("{}", serde_json::to_string_pretty(&out.reply["targets"]).unwrap());
    let pass: Vec<bool> = out.reply["targets"].as_array().unwrap().iter().map(|t| t["pass"].as_bool().unwrap()).collect();
    assert_eq!(pass, [true, true, true, true, true, true, false, false]);
    assert!(out.reply["targets"][6]["problem"].as_str().unwrap().contains("no blob or object called nose"));
    assert!(out.reply["targets"][7]["problem"].as_str().unwrap().contains("differs from the line's axis"));
    // The same reply twice.
    assert_eq!(measure(&g, &m, &[]).reply, out.reply);
}

/// The search finds extents of shapes not lined up with the axes: an
/// ellipsoid turned 30 degrees about z, whose y extent is known in closed
/// form, and the section of a ball at a height.
#[test]
fn extents_of_turned_shapes_and_sections() {
    let c = clay(json!([{ "type": "ellipsoid", "name": "egg", "center": [0, 1, 0], "radii": [0.3, 0.1, 0.2], "rotate": [0, 0, 30], "blend": 0 }]), 64);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let t = 30f32.to_radians();
    let half = ((0.3 * t.sin()).powi(2) + (0.1 * t.cos()).powi(2)).sqrt();
    let (lo, hi) = m.blob_extent("egg", Axis::Y).unwrap().unwrap();
    eprintln!("turned egg: {lo} .. {hi}, exact ±{half}");
    assert!((hi - 1.0 - half).abs() < 1e-4 && (1.0 - lo - half).abs() < 1e-4);
    let (tlo, thi) = m.total(Axis::Y).unwrap();
    assert!((thi - hi).abs() < 1e-4 && (tlo - lo).abs() < 1e-4);
    let ball = clay(json!([{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.2, "blend": 0 }]), 64);
    let m = ClayModel::new(&ball, Mat4::IDENTITY).unwrap();
    let (a, b) = m.section(0.62, Axis::X).unwrap();
    let w = 2.0 * (0.2f32 * 0.2 - 0.12 * 0.12).sqrt();
    assert!((b - a - w).abs() < 1e-4, "section {} against {w}", b - a);
    assert!(m.section(0.75, Axis::X).is_none(), "above the ball: nothing");
}

/// Review fix: `per` a part divides by the part's height whatever axis the
/// distance takes, unless `per_axis` names another; total_width and
/// total_depth are lengths too.
#[test]
fn per_a_part_is_its_height() {
    let c = clay(json!([{ "type": "ellipsoid", "name": "head", "center": [0, 1, 0], "radii": [0.2, 0.1, 0.05], "blend": 0 }]), 64);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let g = Guides { targets: vec![
        target(json!({ "name": "h", "between": [[0, 0, 0], [0.1, 0, 0]], "axis": "x", "per": "head", "value": 0.5, "tolerance": 1e-4 })),
        target(json!({ "name": "w", "between": [[0, 0, 0], [0.1, 0, 0]], "axis": "x", "per": "head", "per_axis": "x", "value": 0.25, "tolerance": 1e-4 })),
        target(json!({ "name": "tw", "between": [[0, 0, 0], [0.1, 0, 0]], "per": "total_width", "value": 0.25, "tolerance": 1e-4 })),
        target(json!({ "name": "td", "of": ["head"], "axis": "z", "to": "total_depth", "value": 1.0, "tolerance": 1e-4 })),
    ], ..Default::default() };
    g.check().unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("{}", out.reply["targets"]);
    assert_eq!(out.reply["pass"], true, "{}", out.reply["failures"]);
    assert_eq!(out.reply["targets"][0]["per_axis"], "y");
    // Review fix (5c): per_axis is a part's; total_width's own axis is x.
    assert!(out.reply["targets"][2]["per_axis"].is_null(), "{}", out.reply["targets"][2]);
    assert_eq!(out.reply["targets"][2]["per"], "total_width");
    let bad = Guides { targets: vec![target(json!({ "name": "b", "of": ["head"], "per_axis": "x", "value": 1, "tolerance": 0 }))], ..Default::default() };
    assert!(bad.check().unwrap_err().contains("per_axis goes with per"));
    for per in [TOTAL_WIDTH, HEAD_UNIT] {
        let bad = Guides { targets: vec![target(json!({ "name": "b", "between": [[0, 0, 0], [0.1, 0, 0]], "per": per, "per_axis": "x", "value": 1, "tolerance": 0 }))], ..Default::default() };
        assert!(bad.check().unwrap_err().contains("per_axis goes with per naming a part"), "{per}");
    }
}

/// Review fix (5c): guides saved before per_axis measure as they did. A
/// part `per` with an `axis` and no `per_axis` was taken along that axis
/// (the head's width here, 0.4: 0.1 per it is 0.25); loading keeps that
/// by writing `per_axis: x`. Without an axis it was the height, as now.
/// A per_axis next to head_unit or a total (which never read it) goes,
/// so such a file still loads its guides.
#[test]
fn guides_saved_before_per_axis_measure_as_they_did() {
    let c = clay(json!([{ "type": "ellipsoid", "name": "head", "center": [0, 1, 0], "radii": [0.2, 0.1, 0.05], "blend": 0 }]), 64);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let file = json!({ "head_units": { "count": 2, "bottom": 0.9, "top": 1.1 }, "targets": [
        { "name": "old_x", "between": [[0, 0, 0], [0.1, 0, 0]], "axis": "x", "per": "head", "value": 0.25, "tolerance": 1e-4 },
        { "name": "old_none", "between": [[0, 0, 0], [0.1, 0, 0]], "per": "head", "value": 0.5, "tolerance": 1e-4 },
        { "name": "unit", "between": [[0, 0, 0], [0.1, 0, 0]], "per": "head_unit", "per_axis": "z", "value": 1.0, "tolerance": 1e-4 },
    ] });
    let g = lenient(file).unwrap();
    assert_eq!(g.targets.len(), 3, "the guides load");
    assert_eq!(g.targets[0].per_axis, Some(Axis::X));
    assert_eq!(g.targets[1].per_axis, Some(Axis::Y));
    assert_eq!(g.targets[2].per_axis, None);
    g.check().unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("{}", out.reply["targets"]);
    assert_eq!(out.reply["pass"], true, "{}", out.reply["failures"]);
    assert_eq!(out.reply["targets"][0]["per_axis"], "x");
    assert!(out.reply["targets"][2]["per_axis"].is_null());
    // Written back out, they read the same again.
    let again = lenient(serde_json::to_value(&g).unwrap()).unwrap();
    assert_eq!(again, g);
}

/// Review fix: a thin part reaching past the rest is found. A 2 m figure
/// with a 1 cm horn (thinner than the clay's cell) tilted 45 degrees up
/// from the crown: the total height, width and depth reach its tip, and
/// the horn blob alone reads the same; the old column march (10 x 10
/// columns, 40 steps) read the crown, 9 cm short.
#[test]
fn thin_parts_are_not_stepped_over() {
    let s = 0.5f32.sqrt();
    let a = [0.0f32, 1.98, 0.0];
    let dir = [0.5f32, s, 0.5];
    let b = [a[0] + 0.15 * dir[0], a[1] + 0.15 * dir[1], a[2] + 0.15 * dir[2]];
    let c = clay(json!([
        { "type": "ellipsoid", "name": "body", "center": [0, 1.0, 0], "radii": [0.3, 1.0, 0.25], "blend": 0 },
        { "type": "sphere", "name": "head", "center": [0, 1.75, 0], "radius": 0.25, "blend": 0 },
        { "type": "capsule", "name": "horn", "a": a, "b": b, "radius": 0.005, "blend": 0 },
    ]), 192);
    let cell = c.cell_size().unwrap();
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let t = std::time::Instant::now();
    let (lo, hi) = m.total(Axis::Y).unwrap();
    let ty = t.elapsed().as_secs_f64() * 1e3;
    let (_, xhi) = m.total(Axis::X).unwrap();
    let (_, zhi) = m.total(Axis::Z).unwrap();
    eprintln!("horn (height {ty:.1} ms, all three {:.1} ms, cell {cell}): y {lo} .. {hi}, x .. {xhi}, z .. {zhi}", t.elapsed().as_secs_f64() * 1e3);
    assert!(cell > 0.01, "the horn is thinner than the cell");
    assert!(lo.abs() < 1e-3, "bottom {lo}");
    assert!((hi - (b[1] + 0.005)).abs() < 1e-3, "top {hi} against {}", b[1] + 0.005);
    assert!((xhi - 0.3).abs() < 1e-3, "the body is widest: {xhi}");
    assert!((zhi - 0.25).abs() < 1e-3, "the head and body are deepest: {zhi}");
    let (_, horn) = m.blob_extent("horn", Axis::Y).unwrap().unwrap();
    assert!((horn - hi).abs() < 1e-4, "the horn blob alone {horn}");
    // The same bits again (one thread, a fixed order).
    let m2 = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    assert_eq!(m2.total(Axis::Y).unwrap(), (lo, hi));
    // The horn as an exact round tube (a 1b sweep, as limbs and fingers
    // are): the composed field reads the tube's own top.
    let c = clay(json!([
        { "type": "ellipsoid", "name": "body", "center": [0, 1.0, 0], "radii": [0.3, 1.0, 0.25], "blend": 0 },
        { "type": "sphere", "name": "head", "center": [0, 1.75, 0], "radius": 0.25, "blend": 0 },
        { "type": "sweep", "name": "horn", "profile": { "circle": 0.005 }, "field": "segments", "points": [[0, 1.98, 0], [0.05, 2.04, 0.03], [0.08, 2.10, 0.06]], "blend": 0 },
    ]), 192);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let (_, hi) = m.total(Axis::Y).unwrap();
    let (_, horn) = m.blob_extent("horn", Axis::Y).unwrap().unwrap();
    eprintln!("tube horn: total top {hi}, the tube alone {horn}");
    assert!((hi - 2.105).abs() < 2e-3 && (hi - horn).abs() < 1e-4, "{hi} {horn}");
    // A long thin ellipsoid tilted 45 degrees (its distance is only about
    // right): the composed top is its own, in closed form.
    let c = clay(json!([
        { "type": "ellipsoid", "name": "body", "center": [0, 1.0, 0], "radii": [0.3, 1.0, 0.25], "blend": 0 },
        { "type": "sphere", "name": "head", "center": [0, 1.75, 0], "radius": 0.25, "blend": 0 },
        { "type": "ellipsoid", "name": "horn", "center": [0.04, 2.02, 0], "radii": [0.006, 0.08, 0.006], "rotate": [0, 0, 45], "blend": 0 },
    ]), 192);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let (_, hi) = m.total(Axis::Y).unwrap();
    let (_, horn) = m.blob_extent("horn", Axis::Y).unwrap().unwrap();
    let a = 45f32.to_radians();
    let exact = 2.02 + ((0.006 * a.sin()).powi(2) + (0.08 * a.cos()).powi(2)).sqrt();
    eprintln!("ellipsoid horn: total top {hi}, alone {horn}, exact {exact}");
    assert!((hi - exact).abs() < 1e-4 && (horn - exact).abs() < 1e-4, "{hi} {horn} {exact}");
}

/// Every bound and rule, refused by `check`.
#[test]
fn guides_are_bounded() {
    let ok = |g: &Guides| g.check().is_ok();
    let line = |n: &str, y: f32| Line { name: n.into(), x: None, y: Some(y), z: None };
    let t = |v: Value| Guides { targets: vec![target(v)], ..Default::default() };
    assert!(ok(&Guides::default()));
    for count in [1, 9] {
        assert!(!ok(&Guides { head_units: Some(HeadUnits { count, bottom: 0.0, top: 1.0 }), ..Default::default() }));
    }
    assert!(!ok(&Guides { head_units: Some(HeadUnits { count: 3, bottom: 1.0, top: 1.0 }), ..Default::default() }));
    assert!(!ok(&Guides { head_units: Some(HeadUnits { count: 3, bottom: f32::NAN, top: 1.0 }), ..Default::default() }));
    assert!(!ok(&Guides { head_units: Some(HeadUnits { count: 3, bottom: 0.0, top: 2e4 }), ..Default::default() }));
    assert!(!ok(&Guides { lines: (0..33).map(|i| line(&format!("l{i}"), 0.0)).collect(), ..Default::default() }));
    assert!(!ok(&Guides { lines: vec![line("a", 0.0), line("a", 1.0)], ..Default::default() }));
    assert!(!ok(&Guides { lines: vec![Line { name: "both".into(), x: Some(0.0), y: Some(1.0), z: None }], ..Default::default() }));
    assert!(!ok(&Guides { lines: vec![Line { name: "none".into(), x: None, y: None, z: None }], ..Default::default() }));
    assert!(!ok(&Guides { lines: vec![line("", 0.0)], ..Default::default() }));
    assert!(!ok(&Guides { lines: vec![line(&"n".repeat(65), 0.0)], ..Default::default() }));
    assert!(!ok(&t(json!({ "name": "a", "of": ["head"], "to": "total_height", "value": 0.2, "tolerance": -0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "of": ["head"], "between": ["a", "b"], "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "between": ["a"], "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "between": ["a", "b"], "to": "x", "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "of": ["a"], "per": "x", "to": "total_height", "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "of": (0..17).map(|i| format!("b{i}")).collect::<Vec<_>>(), "to": "total_height", "value": 0.2, "tolerance": 0.1 }))));
    assert!(!ok(&t(json!({ "name": "a", "between": [[0, 0, 1e5], "b"], "value": 0.2, "tolerance": 0.1 }))));
    let many: Vec<Target> = (0..33).map(|i| target(json!({ "name": format!("t{i}"), "of": ["a"], "to": "total_height", "value": 0.2, "tolerance": 0.1 }))).collect();
    assert!(!ok(&Guides { targets: many, ..Default::default() }));
    assert!(ok(&t(json!({ "name": "a", "of": ["head"], "to": "total_height", "value": 0.2, "tolerance": 0.1 }))));
}

/// A turnaround sheet's view: its subject's height, heads and the widths
/// at the lines, against the model's.
#[test]
fn measured_against_a_sheet() {
    let c = stacked(4, 64);
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    // A sheet whose subject is 0.84 tall from 0 and 0.2 wide at y = 0.5.
    let width = |y: f32| (0.0..0.84).contains(&y).then_some(0.2);
    let views = [SheetView { view: "front".into(), across: Axis::X, top: 0.84, bottom: 0.0, width_at: &width }];
    let g = Guides {
        head_units: Some(HeadUnits { count: 4, bottom: 0.0, top: 0.8 }),
        lines: vec![Line { name: "belly".into(), x: None, y: Some(0.5), z: None }],
        targets: vec![target(json!({ "name": "as_tall_as_sheet", "of": ["head", "ball0"], "to": "sheet_height", "value": 1.0, "tolerance": 0.01 }))],
    };
    let out = measure(&g, &m, &views);
    eprintln!("{}", serde_json::to_string_pretty(&out.reply["sheet"]).unwrap());
    let s = &out.reply["sheet"][0];
    assert!(near(s["height"].as_f64().unwrap(), 0.84, 1e-5) && near(s["heads"].as_f64().unwrap(), 4.2, 1e-5));
    assert!(near(s["model_over_sheet"].as_f64().unwrap(), 0.8 / 0.84, 1e-4));
    let l = &s["lines"][0];
    // The ball at y = 0.5 is centred there: 0.2 across, as the sheet.
    assert!(near(l["model_width"].as_f64().unwrap(), 0.2, 1e-4) && near(l["difference"].as_f64().unwrap(), 0.0, 1e-4), "{l}");
    assert_eq!(out.reply["targets"][0]["pass"], false, "0.8 against 0.84 is outside 1%");
    // Without a sheet, sheet_height is a problem, not a crash.
    let none = measure(&g, &m, &[]);
    assert!(none.reply["targets"][0]["problem"].as_str().unwrap().contains("turnaround sheet"));
}

/// Timing of the measures (printed, not asserted).
#[test]
fn measure_timing() {
    let c = stacked(5, 96);
    let g = Guides {
        head_units: Some(HeadUnits { count: 5, bottom: 0.0, top: 1.0 }),
        targets: vec![
            target(json!({ "name": "head_ratio", "of": ["head"], "to": "total_height", "value": 0.2, "tolerance": 0.001 })),
            target(json!({ "name": "eye_spacing", "between": ["eye.L", "eye.R"], "value": 0.1, "tolerance": 0.001 })),
        ],
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("measure: {:.1} ms ({} named blobs listed)", t.elapsed().as_secs_f64() * 1e3, out.reply["head_units"]["blobs"].as_array().unwrap().len());
}

/// Timing on a clay of 120 blended blobs (a chain of balls winding up,
/// with 40 named), every measure a reply makes (printed, not asserted).
#[test]
fn measure_timing_on_many_blobs() {
    let blobs: Vec<Value> = (0..120)
        .map(|k| {
            let t = k as f32 * 0.21;
            let name = if k % 3 == 0 { format!("part{k}") } else { String::new() };
            json!({ "type": "sphere", "name": name, "center": [0.3 * t.cos(), 0.012 * k as f32, 0.3 * t.sin()], "radius": 0.05 + 0.01 * (k % 4) as f32, "blend": 0.02 })
        })
        .collect();
    let c = clay(json!(blobs), 128);
    let g = Guides {
        head_units: Some(HeadUnits { count: 6, bottom: 0.0, top: 1.5 }),
        lines: vec![Line { name: "mid".into(), x: None, y: Some(0.7), z: None }],
        targets: vec![
            target(json!({ "name": "r", "of": ["part60"], "to": "total_height", "value": 0.1, "tolerance": 0.1 })),
            target(json!({ "name": "d", "between": ["part3", "part99"], "value": 1.0, "tolerance": 1.0 })),
        ],
    };
    let t = std::time::Instant::now();
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let out = measure(&g, &m, &[]);
    eprintln!("measure on 120 blobs: {:.1} ms ({} named blobs listed)", t.elapsed().as_secs_f64() * 1e3, out.reply["head_units"]["blobs"].as_array().unwrap().len());
}

/// Review fix (5c): sculpted material far from every blob is measured.
/// Grab and snakehook write offsets down to twice the field's clamp, so
/// they make solid where no blob reaches; the old bound called boxes there
/// empty (infinite) whenever no blob was near them. A grab pulls the top of
/// a ball 0.3 across up by about 0.1, past the clamp (4 detail cells) and
/// the reach of the ball's box: a box round a solid point of the pulled
/// lump has no blob near it and its bound must not exceed the field there.
/// Then a snakehook hooks a horn out sideways from the crown, over empty
/// columns, and the total extents read its tip as the clay's own mesh
/// does (the old search read the ball's side, 9 cm short).
#[test]
fn sculpted_material_past_the_blobs_is_measured() {
    let stroke = |brush, points: Vec<Vec3>, radius, delta| crate::sculpt::Stroke { brush, points, radius, strength: 1.0, delta, normal: None, mirror: false, scale: None, pattern: None, invert: false, mask: None };
    let mut c = clay(json!([{ "type": "sphere", "name": "ball", "center": [0, 0, 0], "radius": 0.3, "blend": 0 }]), 64);
    c.set_detail_cell(0.01).unwrap();
    crate::sculpt::apply(&mut c, &stroke(crate::sculpt::Brush::Grab, vec![Vec3::new(0.0, 0.3, 0.0)], 0.2, Some(Vec3::new(0.0, 0.18, 0.0)))).unwrap();
    let field = crate::clay::Field::new(&c, 0.01);
    let p = Vec3::new(0.0, 0.38, 0.0);
    let (lo, hi) = (p - 0.005, p + 0.005);
    let list = field.near(lo, hi);
    let at = field.sample(p, &list).0;
    let bound = field.solid_bound(p, &list, lo, hi);
    eprintln!("pulled lump: limit {}, blobs near {}, field {at}, bound {bound}", field.limit, list.len());
    assert!(list.is_empty() && at < 0.0, "the point is solid with no blob near: {} blobs, field {at}", list.len());
    assert!(bound <= at, "the bound {bound} passes the field {at}");

    let mut c = clay(json!([{ "type": "sphere", "name": "ball", "center": [0, 0, 0], "radius": 0.3, "blend": 0 }]), 64);
    c.set_detail_cell(0.01).unwrap();
    let hook = [[0.0, 0.3, 0.0], [0.1, 0.35, 0.0], [0.25, 0.38, 0.0], [0.35, 0.42, 0.0]].map(Vec3::from).to_vec();
    crate::sculpt::apply(&mut c, &stroke(crate::sculpt::Brush::Snakehook, hook, 0.1, None)).unwrap();
    let (mlo, mhi) = c.build_mesh().bounds();
    let m = ClayModel::new(&c, Mat4::IDENTITY).unwrap();
    let (ylo, yhi) = m.total(Axis::Y).unwrap();
    let (_, xhi) = m.total(Axis::X).unwrap();
    let (_, zhi) = m.total(Axis::Z).unwrap();
    eprintln!("hooked horn: mesh {mlo} .. {mhi}, measured y {ylo} .. {yhi}, x .. {xhi}, z .. {zhi}");
    // The old search read the ball's side, x 0.3, 9 cm short.
    assert!(mhi.x > 0.3 + 2.0 * field.limit, "the horn reaches past the clamp: {mhi}");
    for (what, got, want) in [("x", xhi, mhi.x), ("top", yhi, mhi.y), ("z", zhi, mhi.z), ("bottom", ylo, mlo.y)] {
        assert!((got - want).abs() < 0.01, "{what}: measured {got}, mesh {want}");
    }
}
