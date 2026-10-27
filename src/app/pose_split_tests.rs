//! Character follow-ups, feature 1: `pose_bone` poses mirrored blobs per
//! side. Pip (tests/fixtures/pip.json, the user's create_character base
//! with mirrored brows, lids, cheeks and ears) is the fixture. A head
//! tilt writes each mirrored blob on the head out as its two sides and
//! turns both by the same tilt (no V scowl); one arm leaves the other
//! alone; symmetric poses keep one blob; undo, save and load give the
//! blobs back; the same call gives the same blobs.

use std::sync::mpsc::Sender;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::ClideApp;
use super::recorder::tests::{frame, mcp, ok, settle};
use crate::clay::{Blob, Clay, Form};
use crate::mcp::ToolCall;

pub(super) const PIP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pip.json");

pub(super) fn headless() -> Option<(ClideApp, Sender<ToolCall>, egui::Context)> {
    let Some(rs) = crate::render::test_support::headless() else {
        eprintln!("skipped: no GPU adapter for the editor's render state");
        return None;
    };
    let (mut app, tx) = ClideApp::headless(rs);
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, Vec::new());
    Some((app, tx, ctx))
}

/// Pip, opened from the fixture (never the user's own file).
pub(super) fn open_pip(app: &mut ClideApp, ctx: &egui::Context, tx: &Sender<ToolCall>) {
    ok(app, ctx, tx, "open_scene", json!({ "path": PIP }));
    settle(app, ctx);
}

pub(super) fn pip_clay(app: &ClideApp) -> Clay {
    app.scene.objects.iter().find(|o| o.name == "Pip").and_then(|o| o.clay.clone()).expect("Pip's clay")
}

fn blob<'a>(clay: &'a Clay, name: &str) -> &'a Blob {
    clay.blobs.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("no blob {name}"))
}

fn flip(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

/// A capsule blob's ends where it is drawn (a flipped blob is read at -x).
fn ends(b: &Blob) -> (Vec3, Vec3) {
    let Form::Capsule { a, b: e, .. } = b.form else { panic!("{} is not a capsule", b.name) };
    if b.flip { (flip(a), flip(e)) } else { (a, e) }
}

/// The head tilt the user asked for: [-4, 0, 9] degrees about the head
/// bone's head (world axes; Pip stands at the origin unrotated).
fn tilt(clay_rig_head: Vec3) -> Mat4 {
    Mat4::from_translation(clay_rig_head) * Mat4::from_quat(crate::clay::rotation(Vec3::new(-4.0, 0.0, 9.0)).normalize()) * Mat4::from_translation(-clay_rig_head)
}

fn head_pivot(app: &ClideApp) -> Vec3 {
    let o = app.scene.objects.iter().find(|o| o.name == "Pip").unwrap();
    o.rig.as_ref().unwrap().bones.iter().find(|b| b.name == "head").unwrap().head
}

/// The 2D (front view) angle of a brow from its inner end to its outer
/// end, measured left to right across the picture (degrees).
fn brow_angle(inner: Vec3, outer: Vec3) -> f32 {
    let (l, r) = if inner.x < outer.x { (inner, outer) } else { (outer, inner) };
    (r.y - l.y).atan2(r.x - l.x).to_degrees()
}

/// `head_tilt_poses_each_brow_by_the_tilt`: the head tilted [-4, 0, 9]
/// splits every mirrored head blob (brows, lids, cheeks, ears, sockets)
/// into its two sides, and each brow ends exactly where the tilt takes
/// that brow (1e-5), so both turn the same way in the front view: no V.
/// The reply says what was split and why, and lists no approximate
/// blobs (the head's round tubes are exact).
#[test]
fn head_tilt_poses_each_brow_by_the_tilt() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    let before = pip_clay(app);
    let (la, lb) = ends(blob(&before, "brow.L"));
    let (ra, rb) = (flip(la), flip(lb));
    let d = tilt(head_pivot(app));
    let r = ok(app, ctx, tx, "pose_bone", json!({ "bone": "head", "rotate": [-4, 0, 9] }));
    let split: Vec<String> = r["split_blobs"].as_array().expect("split_blobs").iter().map(|s| s["from"].as_str().unwrap().to_string()).collect();
    for name in ["brow.L", "lid.L", "cheek.L", "ear.L", "ear_inner.L", "eye_socket.L"] {
        assert!(split.contains(&name.to_string()), "{name} split: {r}");
    }
    let s = r["split_blobs"].as_array().unwrap().iter().find(|s| s["from"] == "brow.L").unwrap();
    assert_eq!(s["into"], json!(["brow.L", "brow.R"]));
    assert!(s["why"].as_str().unwrap().contains("not mirror-symmetric"), "{s}");
    // Only the head's blobs: the arms and legs did not move.
    assert!(!split.iter().any(|n| n.contains("arm") || n.contains("thigh")), "{split:?}");
    assert!(r["posed"][0]["approximate_blobs"].is_null(), "round tubes and capsules are exact: {r}");
    let after = pip_clay(app);
    let (bl, br) = (blob(&after, "brow.L"), blob(&after, "brow.R"));
    assert!(!bl.mirror && !br.mirror && br.bone == "head");
    let (la2, lb2) = ends(bl);
    let (ra2, rb2) = ends(br);
    for (got, want) in [(la2, d.transform_point3(la)), (lb2, d.transform_point3(lb)), (ra2, d.transform_point3(ra)), (rb2, d.transform_point3(rb))] {
        assert!(got.abs_diff_eq(want, 1e-5), "{got} against {want}");
    }
    // The front view: both brows turned by the same angle (the tilt's 9
    // degrees, nearly), not by opposite angles (the V of the old code).
    let (l0, r0) = (brow_angle(la, lb), brow_angle(ra, rb));
    let (l1, r1) = (brow_angle(la2, lb2), brow_angle(ra2, rb2));
    let (dl, dr) = (l1 - l0, r1 - r0);
    eprintln!("brow angles before L {l0:.3} R {r0:.3}, after L {l1:.3} R {r1:.3}: turned {dl:.3} and {dr:.3} degrees");
    // (Each about 9: the tilt's -4 degree nod turns the brows' depth into
    // the picture a little, differently for the two.)
    assert!((dl - 9.0).abs() < 1.5 && (dr - 9.0).abs() < 1.5, "both brows turn with the head: {dl} and {dr}");
    // The copy comes just before the stored side (where `Field::new`
    // drew the reflection).
    let i = after.blobs.iter().position(|b| b.name == "brow.L").unwrap();
    assert_eq!(after.blobs[i - 1].name, "brow.R");
    let order: Vec<&str> = before.blobs.iter().map(|b| b.name.as_str()).collect();
    let kept: Vec<&str> = after.blobs.iter().map(|b| b.name.as_str()).filter(|n| order.contains(n)).collect();
    assert_eq!(kept, order, "every old blob keeps its name and order");
    // The eyes rode the head bone.
    assert_eq!(r["posed"][0]["objects_carried"], json!(["Pip_eye.L", "Pip_eye.R"]));
}

/// `posing_one_arm_leaves_the_other`: upper_arm.L alone, turned 30
/// degrees, writes the arm's mirrored blobs out and moves only the left
/// side; every right-side blob is drawn exactly where it was, and the
/// field around the right arm is unchanged. Posing upper_arm.R alone
/// (the old code moved nothing: no blob was on that bone) moves the
/// right side by the .R bone.
#[test]
fn posing_one_arm_leaves_the_other() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    let before = pip_clay(app);
    let r = ok(app, ctx, tx, "pose_bone", json!({ "bone": "upper_arm.L", "rotate": [0, 0, -30] }));
    let split: Vec<&str> = r["split_blobs"].as_array().unwrap().iter().map(|s| s["from"].as_str().unwrap()).collect();
    assert_eq!(split, ["upper_arm.L", "forearm.L", "hand.L", "finger1.L", "finger2.L", "finger3.L", "thumb.L"], "{r}");
    assert!(r["split_blobs"][0]["why"].as_str().unwrap().contains("only one side moves"), "{r}");
    let after = pip_clay(app);
    let field = |c: &Clay| crate::clay::Field::new(c, 0.0);
    let (f0, f1) = (field(&before), field(&after));
    let all: Vec<u32> = (0..2048).collect();
    let mut worst = 0.0f32;
    for i in 0..20 {
        for j in 0..20 {
            for k in 0..8 {
                // Around the right arm and hand (x < -0.1).
                let p = Vec3::new(-0.12 - 0.2 * i as f32 / 19.0, 0.18 + 0.42 * j as f32 / 19.0, -0.05 + 0.3 * k as f32 / 7.0);
                let (a, b) = (f0.sample(p, &all[..f0.prim_count()]).0, f1.sample(p, &all[..f1.prim_count()]).0);
                worst = worst.max((a - b).abs());
            }
        }
    }
    eprintln!("right arm field change after posing the left: {worst:e}");
    assert!(worst <= 1e-6, "the right arm is untouched: {worst}");
    for name in ["upper_arm.R", "forearm.R", "hand.R", "thumb.R"] {
        let b = blob(&after, name);
        let left = blob(&before, &crate::rig::mirror_name(name).unwrap());
        let bone = crate::rig::mirror_name(&left.bone).unwrap();
        assert_eq!((b.flip, &b.form, b.bone.as_str()), (true, &left.form, bone.as_str()), "{name} is the old reflection, on the .R bone");
    }
    assert_ne!(blob(&after, "upper_arm.L").form, blob(&before, "upper_arm.L").form);
    // Now the right arm by itself: its .R blobs ride the .R bones.
    let mid = pip_clay(app);
    ok(app, ctx, tx, "pose_bone", json!({ "bone": "upper_arm.R", "rotate": [0, 0, 20] }));
    let end = pip_clay(app);
    assert_ne!(blob(&end, "hand.R").form, blob(&mid, "hand.R").form, "the .R bone moves the .R side");
    assert_eq!(blob(&end, "hand.L").form, blob(&mid, "hand.L").form);
    // On a fresh Pip, the right arm alone splits too (the old code moved
    // no blob at all).
    open_pip(app, ctx, tx);
    let r = ok(app, ctx, tx, "pose_bone", json!({ "bone": "upper_arm.R", "rotate": [0, 0, 20] }));
    assert!(r["posed"][0]["blobs_carried"].as_array().unwrap().iter().any(|b| b == "upper_arm.R"), "{r}");
    assert_eq!(blob(&pip_clay(app), "upper_arm.L").form, blob(&before, "upper_arm.L").form);
}

/// `symmetric_poses_keep_one_blob`: mirror: true on a side bone, a nod
/// and a straight lift of the head on the centre line keep every mirrored
/// blob one blob (no split_blobs), and the moved blob's both sides are
/// where the two moves take them.
#[test]
fn symmetric_poses_keep_one_blob() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    let n = pip_clay(app).blobs.len();
    for (bone, args) in [("upper_arm.L", json!({ "rotate": [0, 0, -30], "mirror": true })), ("head", json!({ "rotate": [10, 0, 0] })), ("head", json!({ "translate": [0, 0.01, 0.005] })), ("thigh.L", json!({ "rotate": [20, 0, 0], "mirror": true }))] {
        let mut a = args.clone();
        a["bone"] = json!(bone);
        let r = ok(app, ctx, tx, "pose_bone", a);
        assert!(r["split_blobs"].is_null(), "{bone} {args}: {r}");
        assert_eq!(pip_clay(app).blobs.len(), n);
    }
    let c = pip_clay(app);
    assert!(blob(&c, "brow.L").mirror && blob(&c, "upper_arm.L").mirror && blob(&c, "thigh.L").mirror);
}

/// `split_poses_undo_save_and_repeat`: one undo after the tilt gives
/// back the clay with its one mirrored brow, bit for bit; redo, save,
/// new and open give the split clay back equal; the same tilt on a fresh
/// Pip gives the same blob JSON.
#[test]
fn split_poses_undo_save_and_repeat() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    let before = pip_clay(app);
    let step = app.history.current();
    ok(app, ctx, tx, "pose_bone", json!({ "bone": "head", "rotate": [-4, 0, 9] }));
    let posed = pip_clay(app);
    assert_eq!(app.history.current(), step + 1, "one undo step");
    ok(app, ctx, tx, "undo", json!({}));
    settle(app, ctx);
    assert_eq!(pip_clay(app), before, "undo restores one mirrored blob");
    assert!(blob(&pip_clay(app), "brow.L").mirror && !pip_clay(app).blobs.iter().any(|b| b.name == "brow.R"));
    ok(app, ctx, tx, "redo", json!({}));
    assert_eq!(pip_clay(app), posed);
    let d = std::env::temp_dir().join(format!("clide3d_pose_split_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("pip_tilted.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    assert_eq!(pip_clay(app), posed, "save and load keep the split blobs");
    let _ = std::fs::remove_dir_all(&d);
    // Deterministic: a fresh Pip, the same call, the same blobs.
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "pose_bone", json!({ "bone": "head", "rotate": [-4, 0, 9] }));
    assert_eq!(serde_json::to_string(&pip_clay(app).blobs).unwrap(), serde_json::to_string(&posed.blobs).unwrap());
}

/// `split_refusals`: a split whose copy's name is taken refuses the
/// whole pose with nothing changed; `edit_clay split` of a blob that is
/// not mirrored is refused; `edit_clay split` of brow.L then updating
/// brow.R raises one brow only.
#[test]
fn split_refusals_and_edit_clay_split() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    open_pip(app, ctx, tx);
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "add": [{ "type": "sphere", "name": "lid.R", "center": [0, 2, 0], "radius": 0.01 }] }));
    let scene = app.scene.clone();
    let step = app.history.current();
    let (v, err) = mcp(app, ctx, tx, "pose_bone", json!({ "bone": "head", "rotate": [-4, 0, 9] }));
    assert!(err && v.as_str().unwrap_or_default().contains("lid.R already exists"), "{v}");
    assert_eq!(app.scene, scene, "nothing changed");
    assert_eq!(app.history.current(), step);
    let (v, err) = mcp(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "split": ["muzzle"] }));
    assert!(err && v.as_str().unwrap_or_default().contains("not mirrored"), "{v}");
    open_pip(app, ctx, tx);
    let before = pip_clay(app);
    let r = ok(app, ctx, tx, "edit_clay", json!({ "target": "Pip", "split": ["brow.L"], "update": [{ "blob": "brow.R", "a": [0.05, 0.935, 0.165] }] }));
    assert!(r.to_string().contains("brow.L and brow.R"), "{r}");
    let c = pip_clay(app);
    assert_eq!(blob(&c, "brow.L").form, blob(&before, "brow.L").form);
    assert!(blob(&c, "brow.R").flip && !blob(&c, "brow.R").mirror);
    let Form::Capsule { a, .. } = blob(&c, "brow.R").form else { panic!() };
    assert_eq!(a, Vec3::new(0.05, 0.935, 0.165), "stored on the left, drawn on the right (flip)");
}

/// The split of every mirrored blob of Pip, with no move, writes the same
/// field bit for bit: each copy is exactly the prim `Field::new`
/// reflected, in the same place in the prim order (just before its side),
/// so even where the two sides blend into each other across the centre
/// line (the brows, sockets and feet) nothing changes. Before the copy
/// came after its side, which changed the surface there by up to 0.56 mm.
/// Checked over a grid through the whole character, the field and the
/// colour, and against the same split with the copy after its side.
#[test]
fn splitting_without_a_move_keeps_the_field() {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(PIP).unwrap()).unwrap();
    let clay: Clay = serde_json::from_value(file["objects"][0]["clay"].clone()).unwrap();
    let rig: crate::rig::Rig = serde_json::from_value(file["objects"][0]["rig"].clone()).unwrap();
    let mut split = clay.clone();
    let mirrored: Vec<usize> = split.blobs.iter().enumerate().filter(|(_, b)| b.mirror).map(|(i, _)| i).collect();
    for &i in mirrored.iter().rev() {
        let name = split.blobs[i].name.clone();
        crate::pose::split_mirrored(&mut split, i, Some(&rig)).unwrap();
        assert_eq!(split.blobs[i + 1].name, name, "the stored side moves up one, its copy takes its place");
        assert!(split.blobs[i].flip != split.blobs[i + 1].flip);
    }
    assert_eq!(split.blobs.len(), clay.blobs.len() + mirrored.len());
    // The old order (each copy after its side), for the comparison.
    let mut after = split.clone();
    for k in 0..mirrored.len() {
        let i = mirrored[k] + k;
        after.blobs.swap(i, i + 1);
    }
    let (f0, f1, f2) = (crate::clay::Field::new(&clay, 0.0), crate::clay::Field::new(&split, 0.0), crate::clay::Field::new(&after, 0.0));
    assert_eq!(f0.prim_count(), f1.prim_count());
    let all: Vec<u32> = (0..2048).collect();
    let (mut differ, mut old_worst, mut near) = (0usize, 0.0f32, 0usize);
    for i in 0..40 {
        for j in 0..60 {
            for k in 0..30 {
                let p = Vec3::new(-0.4 + 0.8 * i as f32 / 39.0, -0.02 + 1.14 * j as f32 / 59.0, -0.3 + 0.6 * k as f32 / 29.0);
                let (a, ca) = f0.sample(p, &all[..f0.prim_count()]);
                let (b, cb) = f1.sample(p, &all[..f1.prim_count()]);
                if a.to_bits() != b.to_bits() || ca != cb {
                    differ += 1;
                }
                // The surface and a few cells round it (the clay's cell is
                // 3.5 mm).
                if a.abs() < 0.015 {
                    near += 1;
                    old_worst = old_worst.max((a - f2.sample(p, &all[..f2.prim_count()]).0).abs());
                }
            }
        }
    }
    eprintln!("Pip with {} mirrored blobs split: {differ} of 72,000 samples differ ({near} near the surface); the copy after its side changed the field by up to {old_worst:e}", mirrored.len());
    assert_eq!(differ, 0, "the split field is the same bits everywhere");
    assert!(old_worst > 1e-4, "the grid reaches where the sides blend across the centre line: {old_worst}");
    // Every copy rides the other side's bone when the rig has it.
    assert_eq!(split.blobs.iter().find(|b| b.name == "hand.R").unwrap().bone, "hand.R");
    assert_eq!(split.blobs.iter().find(|b| b.name == "brow.R").unwrap().bone, "head");
}
