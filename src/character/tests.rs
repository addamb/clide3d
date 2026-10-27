//! Stage 5c acceptance for the generator (docs/QUALITY_PLAN.md): the
//! proportions asked for measure back on the clay's field, every option
//! builds, the output is the same every time and on any thread count,
//! and the arguments are bounded.

use glam::{Mat4, Vec3};

use super::*;
use crate::guides::{Axis, ClayModel};

fn spec(build: Build, p: Proportions) -> Spec {
    Spec { name: "Buddy".into(), build, proportions: p, symmetry: true, eyes: true, color: [0.95, 0.62, 0.38], translate: Vec3::ZERO, resolution: None, eye_bulge: 0.042 }
}

fn with_heads(n: f32) -> Proportions {
    Proportions { head_units: n, ..Default::default() }
}

/// (total bottom, total top, head blob bottom, head blob top) on the field.
fn measured(b: &Built) -> (f32, f32, f32, f32) {
    let clay = clay_of(&b.blobs, b.resolution).unwrap();
    let m = ClayModel::new(&clay, Mat4::IDENTITY).unwrap();
    let (lo, hi) = m.total(Axis::Y).unwrap();
    let (hlo, hhi) = m.blob_extent("head", Axis::Y).unwrap().unwrap();
    (lo, hi, hlo, hhi)
}

/// `character_proportions` on the field: 2, 2.5, 3, 4 and 5 heads tall,
/// biped and quadruped, stand on y = 0 with the crown at the height asked
/// (1e-3 of it) and read 1 / head_units for the head ratio within 0.2%
/// (the plan asks 2%); nothing reaches above the crown.
#[test]
fn proportions_measure_back() {
    for build in [Build::Biped, Build::Quadruped] {
        for n in [2.0, 2.5, 3.0, 4.0, 5.0] {
            for height in [1.0, 1.8] {
                let b = generate(&spec(build, Proportions { height, ..with_heads(n) })).unwrap();
                let (lo, hi, hlo, hhi) = measured(&b);
                let ratio = (hhi - hlo) / (hi - lo);
                eprintln!("{build:?} {n} heads, {height} tall: {lo:.6} .. {hi:.6}, head {hlo:.6} .. {hhi:.6}, ratio {ratio:.6} (1/n {:.6}), resolution {}, finger {:.5}", 1.0 / n, b.resolution, b.finger_radius);
                assert!(lo.abs() < 1e-3 * height, "soles on the ground: {lo}");
                assert!((hi - height).abs() < 1e-3 * height, "crown at the height: {hi}");
                assert!((hhi - hi).abs() < 1e-4 * height, "the head is the top: {hhi} {hi}");
                assert!((ratio * n - 1.0).abs() < 2e-3, "{build:?} {n}: {ratio}");
                assert!((b.unit - height / n).abs() < 1e-6);
            }
        }
    }
}

/// Every option and the ends of every range build, keep the crown as the
/// top and the soles on the ground, and leave no blob thinner than the
/// mesh can carry.
#[test]
fn every_option_builds_clean() {
    let mut cases: Vec<(Build, Proportions)> = Vec::new();
    for body in [Body::Round, Body::Pear, Body::Slim] {
        for hands in [Hands::Mitten, Hands::FourFinger, Hands::FiveFinger] {
            cases.push((Build::Biped, Proportions { body, hands, ..Default::default() }));
        }
    }
    for ears in [Ears::None, Ears::Round, Ears::Pointed] {
        for feet in [Feet::Round, Feet::Long] {
            for head_width in [0.5, 1.5] {
                cases.push((Build::Biped, Proportions { ears, feet, head_width, ..Default::default() }));
                cases.push((Build::Quadruped, Proportions { ears, feet, head_width, ..Default::default() }));
            }
        }
    }
    for (limb_length, limb_thickness) in [(0.6, 0.6), (1.4, 1.6), (0.6, 1.6), (1.4, 0.6)] {
        for n in [2.0, 5.0] {
            cases.push((Build::Biped, Proportions { limb_length, limb_thickness, head_units: n, hands: Hands::FiveFinger, ..Default::default() }));
            cases.push((Build::Quadruped, Proportions { limb_length, limb_thickness, head_units: n, ..Default::default() }));
        }
    }
    cases.push((Build::Biped, Proportions { eye_size: 0.5, eye_spacing: 0.5, nose: Nose::None, ..Default::default() }));
    cases.push((Build::Biped, Proportions { eye_size: 1.6, eye_spacing: 1.5, head_width: 1.5, ..Default::default() }));
    for (build, p) in cases {
        let b = generate(&spec(build, p.clone())).unwrap_or_else(|e| panic!("{build:?} {p:?}: {e}"));
        let clay = clay_of(&b.blobs, b.resolution).unwrap();
        let (lo, hi, _, hhi) = measured(&b);
        assert!(lo.abs() < 1e-3 && (hi - 1.0).abs() < 1e-3 && (hhi - hi).abs() < 1e-4, "{build:?} {p:?}: {lo} {hi} {hhi}");
        let thin = clay.thin_blobs(clay.cell_size().unwrap());
        assert!(thin.is_empty(), "{build:?} {p:?}: thin {thin:?} at resolution {}", b.resolution);
        assert!(b.bones.iter().all(|x| x.head.is_finite() && x.tail.is_finite() && x.head != x.tail));
        let names: std::collections::BTreeSet<&str> = b.bones.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names.len(), b.bones.len(), "bone names are unique");
        assert!(b.bones.iter().all(|x| x.parent.as_deref().is_none_or(|p| names.contains(p))), "parents exist");
        for blob in &b.blobs {
            let bone = blob["bone"].as_str().unwrap_or("");
            assert!(bone.is_empty() || names.contains(bone), "blob {} on a missing bone {bone}", blob["name"]);
        }
    }
}

/// `character_deterministic`: the same arguments give the same blob JSON;
/// the field is the same bits on 1 and 12 threads (the mesh too); and the
/// sides written out (symmetry false) give exactly the mirrored field.
#[test]
fn deterministic_and_symmetric() {
    let s = spec(Build::Biped, Proportions::default());
    let a = generate(&s).unwrap();
    let b = generate(&s).unwrap();
    assert_eq!(a, b);
    assert_eq!(serde_json::to_string(&a.blobs).unwrap(), serde_json::to_string(&b.blobs).unwrap());
    let clay = clay_of(&a.blobs, a.resolution).unwrap();
    let one = crate::clay::with_threads(1, || clay.build_mesh());
    let many = crate::clay::with_threads(12, || clay.build_mesh());
    assert_eq!(one.vertices.len(), many.vertices.len());
    assert!(one.vertices.iter().zip(&many.vertices).all(|(x, y)| x.pos == y.pos) && one.indices == many.indices, "1 and 12 threads");
    eprintln!("default base: {} blobs, {} bones, resolution {}, {} triangles", a.blobs.len(), a.bones.len(), a.resolution, one.indices.len() / 3);
    let both = generate(&Spec { symmetry: false, ..s.clone() }).unwrap();
    assert_eq!(both.resolution, a.resolution);
    assert!(both.blobs.len() > a.blobs.len());
    assert!(both.blobs.iter().all(|b| b.get("mirror").is_none()));
    assert!(both.blobs.iter().any(|b| b["name"] == "upper_arm.R") && both.blobs.iter().any(|b| b["name"] == "finger1.R"));
    let sides = clay_of(&both.blobs, both.resolution).unwrap();
    let cell = clay.cell_size().unwrap();
    let (f1, f2) = (Field::new(&clay, cell), Field::new(&sides, cell));
    let (lo, hi) = f1.bounds().unwrap();
    let (l1, l2) = (f1.near(lo, hi), f2.near(lo, hi));
    let mut worst = 0.0f32;
    for i in 0..24 {
        for j in 0..24 {
            for k in 0..12 {
                let p = lo + (hi - lo) * Vec3::new((i as f32 + 0.5) / 24.0, (j as f32 + 0.5) / 24.0, (k as f32 + 0.5) / 12.0);
                worst = worst.max((f1.sample(p, &l1).0 - f2.sample(p, &l2).0).abs());
            }
        }
    }
    eprintln!("sides written out against mirrored: largest field difference {worst:e}");
    assert!(worst < 1e-5, "{worst}");
}

/// The arguments are bounded: each range's ends, a bad colour, a far
/// translation, a resolution out of range, a nameless base and eyes too
/// wide for the head are refused.
#[test]
fn arguments_are_bounded() {
    let refused = |p: Proportions, says: &str| {
        let e = generate(&spec(Build::Biped, p.clone())).unwrap_err();
        assert!(e.contains(says), "{p:?}: {e}");
    };
    refused(Proportions { height: 0.05, ..Default::default() }, "height");
    refused(Proportions { height: f32::NAN, ..Default::default() }, "height");
    refused(Proportions { head_units: 1.9, ..Default::default() }, "head_units");
    refused(Proportions { head_units: 5.1, ..Default::default() }, "head_units");
    refused(Proportions { head_width: 1.6, ..Default::default() }, "head_width");
    refused(Proportions { limb_length: 0.5, ..Default::default() }, "limb_length");
    refused(Proportions { limb_thickness: 1.7, ..Default::default() }, "limb_thickness");
    refused(Proportions { eye_size: 2.1, ..Default::default() }, "eye_size");
    refused(Proportions { eye_spacing: 0.4, ..Default::default() }, "eye_spacing");
    refused(Proportions { eye_size: 2.0, eye_spacing: 1.5, head_width: 0.5, ..Default::default() }, "reach past the head");
    let base = spec(Build::Biped, Proportions::default());
    for (s, says) in [
        (Spec { color: [1.2, 0.0, 0.0], ..base.clone() }, "color"),
        (Spec { translate: Vec3::new(2e3, 0.0, 0.0), ..base.clone() }, "translate"),
        (Spec { resolution: Some(1024), ..base.clone() }, "resolution"),
        (Spec { name: String::new(), ..base.clone() }, "name"),
        // Review fix (5c): names whose eyes would not be guide names.
        (Spec { name: "   ".into(), ..base.clone() }, "name"),
        (Spec { name: "a\tb".into(), ..base.clone() }, "name"),
        (Spec { name: "x".repeat(MAX_NAME + 1), ..base.clone() }, "name"),
    ] {
        let e = generate(&s).unwrap_err();
        assert!(e.contains(says), "{e}");
    }
    // The longest name: its eyes' names are guide names, so the guides
    // pass `guides set`'s check and a saved file keeps them.
    let long = generate(&Spec { name: "x".repeat(MAX_NAME), ..base.clone() }).unwrap();
    assert_eq!(long.eyes[0].name.chars().count(), crate::guides::MAX_NAME);
    long.guides.check().unwrap();
    let p: Result<Proportions, _> = serde_json::from_value(serde_json::json!({ "tail": "long" }));
    assert!(p.unwrap_err().to_string().contains("unknown field"));
}

/// The guides the base carries: head units from the crown down in whole
/// heads (the last line under the soles for 2.5 heads), the eye line
/// through the eyes, and the targets as asked.
#[test]
fn guides_follow_the_proportions() {
    let b = generate(&Spec { translate: Vec3::new(1.0, 0.5, 0.0), ..spec(Build::Biped, Proportions { eye_size: 1.4, ..Default::default() }) }).unwrap();
    let h = b.guides.head_units.as_ref().unwrap();
    assert_eq!(h.count, 3);
    assert!((h.top - 1.5).abs() < 1e-6 && (h.bottom - (1.5 - 1.2)).abs() < 1e-5, "{h:?}");
    assert!((h.unit() - 0.4).abs() < 1e-6);
    let eye = &b.eyes[0];
    let line = b.guides.lines.iter().find(|l| l.name == "eye_line").unwrap();
    assert!((line.y.unwrap() - (eye.center.y + 0.5)).abs() < 1e-6);
    assert!((eye.diameter / b.unit - crate::character::EYE * 1.4).abs() < 1e-6);
    assert_eq!(b.guides.targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["head_ratio", "eye_size", "eye_spacing", "eyes_on_line"]);
    b.guides.check().unwrap();
    let no_eyes = generate(&Spec { eyes: false, ..spec(Build::Biped, Proportions::default()) }).unwrap();
    assert!(no_eyes.eyes.is_empty() && no_eyes.guides.targets.len() == 1);
    assert!(!no_eyes.blobs.iter().any(|b| b["name"].as_str().is_some_and(|n| n.starts_with("eye_socket"))));
}

/// Follow-up (the film preview): the eyes are seated. Round each eyeball
/// there is no crevice: no direction where the space just outside the
/// eye (3% of its radius out) is empty while skin overhangs it a little
/// further out (20%), so no gap or dark crease ring shows at any mesh
/// resolution; and inside the socket's seat nothing covers the eye. The
/// old socket, a ball 4% wider than the eye, left that crevice all round
/// (the dark rim the preview showed). Checked on the default, big and
/// small eyes, a tall narrow head and a quadruped.
#[test]
fn eyes_are_seated_without_a_gap() {
    // The socket's own seat, without lids (which cover the eye on
    // purpose: `lids_wrap_the_eye`).
    let bare = Proportions { lids: super::Lids::None, ..Default::default() };
    let cases = [
        (Build::Biped, bare.clone()),
        (Build::Biped, Proportions { eye_size: 1.6, eye_spacing: 0.6, ..bare.clone() }),
        (Build::Biped, Proportions { eye_size: 0.5, eye_spacing: 1.5, ..bare.clone() }),
        (Build::Biped, Proportions { head_units: 4.0, head_width: 0.7, ..bare.clone() }),
        (Build::Quadruped, bare.clone()),
    ];
    for (build, p) in cases {
        let b = generate(&spec(build, p.clone())).unwrap();
        let clay = clay_of(&b.blobs, b.resolution).unwrap();
        let field = Field::new(&clay, clay.cell_size().unwrap());
        let eye = &b.eyes[0];
        let r = 0.5 * eye.diameter;
        let at = |q: Vec3| field.sample(q, &field.near(q - r, q + r)).0;
        let (mut crevices, mut covered, mut seen) = (Vec::new(), 0, 0);
        for i in 0..=45 {
            for j in 0..36 {
                let theta = (i as f32 * 2.0).to_radians();
                let phi = (j as f32 * 10.0).to_radians();
                let dir = Vec3::new(theta.sin() * phi.cos(), theta.sin() * phi.sin(), theta.cos());
                let (inner, outer) = (at(eye.center + dir * (1.03 * r)), at(eye.center + dir * (1.2 * r)));
                // Along the socket's wall itself (it runs along the eye's
                // radius) the field is 0 to rounding: no direction there.
                let on_wall = (theta.to_degrees() - super::SEAT).abs() < 1.0;
                if inner > 0.0 && outer <= 0.0 && !on_wall {
                    crevices.push((i * 2, j * 10));
                }
                if theta.to_degrees() <= super::SEAT - 6.0 {
                    seen += 1;
                    if inner <= 0.0 || outer <= 0.0 {
                        covered += 1;
                    }
                }
            }
        }
        eprintln!("{build:?} eye_size {} head_width {}: {} crevices round the eye (degrees from its axis, round it), {covered} of {seen} directions inside the seat covered", p.eye_size, p.head_width, crevices.len());
        assert!(crevices.is_empty(), "{build:?} {p:?}: a gap round the eye at {crevices:?}");
        assert_eq!(covered, 0, "{build:?} {p:?}: skin over the eye inside the seat");
    }
}

/// Follow-up: the mouth. By default a smile is carved under the nose
/// (the face's surface there is cut in), its corners higher than its
/// middle; a grin is wider; `mouth: none` leaves the face whole. The nose
/// is a blended button whose colour is the skin's moved toward pink.
#[test]
fn the_mouth_and_nose() {
    let base = generate(&spec(Build::Biped, Proportions::default())).unwrap();
    let blob = |b: &Built, name: &str| b.blobs.iter().find(|x| x["name"] == name).cloned();
    let smile = blob(&base, "mouth").expect("a mouth by default");
    assert_eq!(smile["op"], "carve");
    let points: Vec<Vec3> = serde_json::from_value(smile["points"].clone()).unwrap();
    let (first, mid, last) = (points[0], points[points.len() / 2], points[points.len() - 1]);
    assert!(first.y > mid.y + 0.04 * base.unit && (first.y - last.y).abs() < 1e-6 && (first.x + last.x).abs() < 1e-6, "corners up: {first} {mid} {last}");
    // The face's surface at the smile's middle is cut in.
    let clay = clay_of(&base.blobs, base.resolution).unwrap();
    let field = Field::new(&clay, clay.cell_size().unwrap());
    let plain = generate(&spec(Build::Biped, Proportions { mouth: Mouth::None, ..Default::default() })).unwrap();
    assert!(blob(&plain, "mouth").is_none() && blob(&plain, "mouth_line").is_none());
    let whole = Field::new(&clay_of(&plain.blobs, plain.resolution).unwrap(), clay.cell_size().unwrap());
    // The surface point in front of the groove's middle, on the plain face.
    let mut q = mid + Vec3::Z * 0.2 * base.unit;
    let at = |f: &Field, q: Vec3| f.sample(q, &f.near(q - 0.1, q + 0.1)).0;
    for _ in 0..400 {
        q -= Vec3::Z * (0.5 * at(&whole, q));
    }
    let cut = at(&field, q);
    eprintln!("smile: {} points, corners {:.4} over the middle (u {:.4}); at the plain face's surface the smiling face reads {cut:.5}", points.len(), first.y - mid.y, base.unit);
    assert!(at(&whole, q).abs() < 1e-4 && cut > 0.01 * base.unit, "the smile is cut in: {cut} (plain {})", at(&whole, q));
    let grin = generate(&spec(Build::Biped, Proportions { mouth: Mouth::Grin, ..Default::default() })).unwrap();
    let wide: Vec<Vec3> = serde_json::from_value(blob(&grin, "mouth").unwrap()["points"].clone()).unwrap();
    assert!(wide[0].x.abs() > first.x.abs() * 1.2, "a grin is wider");
    let nose = blob(&base, "nose").unwrap();
    let c: [f32; 3] = serde_json::from_value(nose["color"].clone()).unwrap();
    assert!(c[0] < 0.95 && c[1] < 0.62 && c[1] > 0.45, "a blushed skin, not a pink bead: {c:?}");
    assert!(nose["blend"].as_f64().unwrap() as f32 >= 0.05 * base.unit);
}

/// Follow-up: the skin's subsurface reach scales with the character, so
/// the film tracer's gather (about twelve scatter distances) stays well
/// under the gap between arm and body (the dark red patches on the hip
/// and feet in the preview, from light gathered off the shadowed side of
/// the arm and shin). The arms are tapered tubes, full at the shoulder,
/// swelling past the elbow and slim at the wrist.
#[test]
fn skin_reach_and_arms() {
    for height in [0.5, 1.0, 1.8] {
        let b = generate(&spec(Build::Biped, Proportions { height, ..Default::default() })).unwrap();
        let reach = 12.0 * crate::material::STYLIZED_SCATTER[0] * b.skin_radius;
        eprintln!("height {height}: subsurface_radius {}, reach {reach:.4} ({:.3} of the height)", b.skin_radius, reach / height);
        assert!((b.skin_radius - SKIN_RADIUS * height).abs() < 1e-6);
        assert!(reach <= 0.035 * height, "{reach}");
    }
    let b = generate(&spec(Build::Biped, Proportions::default())).unwrap();
    let radii = |name: &str| -> Vec<f32> { serde_json::from_value(b.blobs.iter().find(|x| x["name"] == name).unwrap()["radii"].clone()).unwrap() };
    let (up, fore) = (radii("upper_arm.L"), radii("forearm.L"));
    assert!(up[0] > up[2] && up[1] > up[2], "the upper arm narrows to the elbow: {up:?}");
    assert!(fore[1] > fore[0] && fore[1] > fore[2] && fore[2] < up[2], "the forearm swells past the elbow, slim at the wrist: {fore:?}");
}


/// Follow-up: the lids wrap the eye. On the default base and big, small
/// and narrow-headed variants with both lids, over the front of the eye
/// where a lid covers it: the ball and its cornea's bulge are inside the
/// lid's hollow (the lids alone are empty there), the lid is solid over
/// them, and the ray from the cornea's crown toward the viewer meets
/// skin, so no glassy strip of the eye shows under a lid's edge; where no
/// lid covers the eye (between the margins, inside the seat) at least 95%
/// of directions are open (the rest at the corners, where the lid's root
/// rolls into the seat's wall).
#[test]
fn lids_wrap_the_eye() {
    let both = |p: Proportions| Proportions { lids: super::Lids::Both, upper_cover: 0.35, lower_cover: 0.2, ..p };
    let cases = [
        (Build::Biped, Proportions::default()),
        (Build::Biped, both(Proportions::default())),
        (Build::Biped, both(Proportions { eye_size: 1.6, eye_spacing: 0.6, ..Default::default() })),
        (Build::Biped, both(Proportions { eye_size: 0.5, eye_spacing: 1.5, ..Default::default() })),
        (Build::Biped, both(Proportions { head_units: 4.0, head_width: 0.7, ..Default::default() })),
        (Build::Quadruped, both(Proportions::default())),
    ];
    for (build, p) in cases {
        let b = generate(&spec(build, p.clone())).unwrap();
        let clay = clay_of(&b.blobs, b.resolution).unwrap();
        let cell = clay.cell_size().unwrap();
        let field = Field::new(&clay, cell);
        let eye = &b.eyes[0];
        let r = 0.5 * eye.diameter;
        let reach = r + 0.042 * eye.diameter;
        let at = |q: Vec3| field.sample(q, &field.near(q - r, q + r)).0;
        let caps: Vec<(Vec3, f32, f32)> = clay.blobs.iter().filter(|x| matches!(x.form, crate::clay::Form::Lid { .. })).map(|x| {
            let (c, n, rm, h) = crate::clay::lid_geometry(&x.form);
            assert!(c.abs_diff_eq(eye.center, 1e-5));
            (n, rm, h)
        }).collect();
        // The lids alone: the eye is inside their hollow. (Blended into
        // the socket, their roots fill some of the hollow too, hidden.)
        let lids_only = Clay { blobs: clay.blobs.iter().filter(|x| matches!(x.form, crate::clay::Form::Lid { .. })).cloned().collect(), resolution: clay.resolution, ..Default::default() };
        let lid_field = Field::new(&lids_only, cell);
        let all: Vec<u32> = (0..lid_field.prim_count() as u32).collect();
        assert_eq!(caps.len(), if p.lids == super::Lids::Both { 2 } else { 1 });
        let (mut under, mut clear, mut overhead, mut open, mut seen) = (0, 0, 0, 0, 0);
        for i in 0..=30 {
            for j in 0..72 {
                let theta = (i as f32 * 2.0).to_radians();
                let phi = (j as f32 * 5.0).to_radians();
                let dir = Vec3::new(theta.sin() * phi.cos(), theta.sin() * phi.sin(), theta.cos());
                let lid = caps.iter().find(|(n, rm, h)| dir.dot(*n) > (h / rm).sin() + 0.03);
                match lid {
                    Some((_, rm, h)) => {
                        under += 1;
                        if lid_field.sample(eye.center + dir * reach, &all).0 > 0.0 { clear += 1; }
                        // Solid over it, and from the front (no glass
                        // strip): the ray toward the viewer meets skin.
                        let over = at(eye.center + dir * *rm) < 0.0 || at(eye.center + dir * (rm + 0.5 * h)) < 0.0;
                        let front = (1..=100).any(|k| at(eye.center + dir * reach + Vec3::Z * (0.6 * r * k as f32 / 100.0)) < 0.0);
                        if over && front { overhead += 1; }
                    }
                    None if caps.iter().all(|(n, rm, h)| dir.dot(*n) < -(h / rm).sin() - 0.03) => {
                        seen += 1;
                        if at(eye.center + dir * (reach + 0.01 * r)) > 0.0 { open += 1; }
                    }
                    None => {}
                }
            }
        }
        eprintln!("{build:?} {:?} eye_size {} upper {} lower {}: {under} directions under a lid ({clear} clear inside, {overhead} with lid over them); {seen} open ({open} uncovered)", p.lids, p.eye_size, p.upper_cover, p.lower_cover);
        assert!(under > 100 && seen > 100);
        assert_eq!((clear, overhead), (under, under), "{build:?} {p:?}");
        // The lid's root rolls into the socket where the margin meets the
        // seat's wall (the corners): a few directions there.
        assert!(open as f32 >= 0.95 * seen as f32, "{build:?} {p:?}: skin over the open eye ({open} of {seen})");
    }
}
