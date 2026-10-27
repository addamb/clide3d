//! Long hair on a body (fur that drapes): a shaggy sheepdog-like base
//! (`create_character` quadruped, reshaped: a round barrel body, short
//! neck, short thick legs, a muzzle) grows a long coat through `hair grow
//! {fur: true}`. Each test builds the dog from the same calls, so it is
//! the same every run.

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::pose_split_tests::headless;
use super::recorder::tests::{ok, settle};
use crate::hair::Hair;

type Tx = std::sync::mpsc::Sender<crate::mcp::ToolCall>;

pub(super) fn id_of(app: &ClideApp, name: &str) -> u64 {
    app.scene.objects.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| panic!("no {name}"))
}

pub(super) fn hair(app: &ClideApp, name: &str) -> Hair {
    app.scene.get(id_of(app, name)).unwrap().hair.clone().unwrap()
}

/// The test dog: 0.8 tall, white with a grey rear, eyes on the head bone.
pub(super) fn sheepdog(app: &mut ClideApp, ctx: &egui::Context, tx: &Tx) {
    let (w, g) = ([0.95, 0.94, 0.90], [0.50, 0.53, 0.58]);
    ok(app, ctx, tx, "create_character", json!({ "name": "Dog", "build": "quadruped", "proportions": { "height": 0.8, "ears": "round" }, "color": w, "resolution": 160 }));
    ok(app, ctx, tx, "edit_clay", json!({ "target": "Dog", "update": [
        { "blob": "pelvis", "center": [0, 0.36, -0.15], "radii": [0.17, 0.165, 0.21], "color": g },
        { "blob": "torso", "center": [0, 0.37, 0.12], "radii": [0.175, 0.17, 0.22], "blend": 0.10 },
        { "blob": "neck", "a": [0, 0.42, 0.24], "b": [0, 0.56, 0.29], "radius": 0.10, "radius_b": 0.085 },
        { "blob": "front_upper_leg.L", "a": [0.095, 0.30, 0.20], "b": [0.095, 0.15, 0.21], "radius": 0.055, "radius_b": 0.046 },
        { "blob": "front_lower_leg.L", "a": [0.095, 0.15, 0.21], "b": [0.095, 0.05, 0.22], "radius": 0.046, "radius_b": 0.042 },
        { "blob": "front_paw.L", "center": [0.095, 0.035, 0.245], "radii": [0.06, 0.035, 0.075] },
        { "blob": "hind_upper_leg.L", "a": [0.10, 0.30, -0.22], "b": [0.10, 0.15, -0.21], "radius": 0.062, "radius_b": 0.048 },
        { "blob": "hind_lower_leg.L", "a": [0.10, 0.15, -0.21], "b": [0.10, 0.05, -0.20], "radius": 0.048, "radius_b": 0.042 },
        { "blob": "hind_paw.L", "center": [0.10, 0.035, -0.175], "radii": [0.06, 0.035, 0.075] },
        { "blob": "tail", "points": [[0, 0.40, -0.34], [0, 0.44, -0.38], [0, 0.46, -0.40]], "radii": [0.04, 0.035, 0.03], "color": g },
        { "blob": "ear.L", "center": [0.15, 0.62, 0.28], "radii": [0.03, 0.085, 0.06], "rotate": [0, 0, 18] },
        { "blob": "nose", "color": [0.06, 0.05, 0.05], "radii": [0.036, 0.027, 0.03], "center": [0, 0.575, 0.455] },
    ], "add": [{ "name": "muzzle", "type": "ellipsoid", "center": [0, 0.56, 0.39], "radii": [0.085, 0.07, 0.08], "op": "add", "blend": 0.05, "color": w, "bone": "head" }] }));
    settle(app, ctx);
}

/// The dog's eyes (world centre and radius).
pub(super) fn eyes(app: &ClideApp) -> Vec<(Vec3, f32)> {
    ["Dog_eye.L", "Dog_eye.R"].iter().map(|e| {
        let m = app.scene.rest_world_matrix(id_of(app, e));
        (m.transform_point3(Vec3::ZERO), 0.5 * m.to_scale_rotation_translation().0.abs().max_element())
    }).collect()
}

pub(super) fn grow(app: &mut ClideApp, ctx: &egui::Context, tx: &Tx, name: &str, grow: Value) -> Value {
    let r = ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": name, "parent": "Dog", "hair": { "grow": grow } }));
    settle(app, ctx);
    r
}

/// `long_fur_keeps_the_face_furred`: the no-root sphere round each eye
/// grows with the fur's length only up to half the eye's radius, so a
/// 0.22 coat still roots round the eyes (it bared a sphere 0.27 across:
/// the whole face), while short fur keeps its old margin exactly (Pip:
/// eyes 0.068 across the radius, fur 0.013 long).
#[test]
fn long_fur_keeps_the_face_furred() {
    assert_eq!(super::fur::eye_bare_radius(0.0677, 0.0133), 0.0677 * 1.15 + 0.0133);
    assert_eq!(super::fur::eye_bare_radius(0.0416, 0.22), 0.0416 * 1.15 + 0.0208);
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sheepdog(app, ctx, tx);
    grow(app, ctx, tx, "Coat", json!({ "fur": true, "on": "Dog", "length": 0.22, "count": 1500, "strands": 20000 }));
    let h = hair(app, "Coat");
    let eyes = eyes(app);
    let fur = h.fur.clone().unwrap();
    for (c, r) in &eyes {
        assert!(fur.bare.iter().any(|s| Vec3::new(s[0], s[1], s[2]).distance(*c) < 1e-5 && (s[3] - r * 1.65).abs() < 1e-5), "{:?}", fur.bare);
    }
    // (Guides on the lids' caps grow the lids' short fur, `Fur::lids`.)
    assert_eq!(fur.lids.len(), 2);
    let on_lids = h.guides.iter().filter(|g| fur.lids.iter().any(|l| l.holds(g.points[0]))).count();
    let roots: Vec<Vec3> = h.guides.iter().map(|g| g.points[0]).filter(|p| !fur.lids.iter().any(|l| l.holds(*p))).collect();
    let near = |k: f32| roots.iter().filter(|p| eyes.iter().any(|(c, r)| p.distance(*c) < r * k)).count();
    eprintln!("guide roots off the lids within 1.65 eye radii: {}, within 3 radii: {}; {on_lids} on the lids", near(1.65), near(3.0));
    assert_eq!(near(1.65), 0);
    assert!(near(3.0) >= 20, "the face round the eyes grows fur");
}

/// `draped_coat_hangs_and_stays_out`: `drape: 1` hangs a 0.22 coat: the
/// guides on the flanks end well below their roots (each tip's last
/// segment near straight down), every guide keeps its length within 3% (the body's push moves a
/// segment's end a little), no guide
/// point or film strand point is inside the clay or under the ground
/// (the soles' height), the strands are the same bits on 1 and 12
/// threads, and the drape, floor and locks are kept by save and load; drape 0 (absent) stores neither.
#[test]
fn draped_coat_hangs_and_stays_out() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sheepdog(app, ctx, tx);
    let t = std::time::Instant::now();
    let r = grow(app, ctx, tx, "Coat", json!({ "fur": true, "on": "Dog", "length": 0.22, "drape": 1, "count": 1200, "strands": 30000 }));
    eprintln!("grow: {:.0} ms; {}", t.elapsed().as_secs_f64() * 1e3, r["hair"]["grown"]);
    // The reply's sampled check reads the dog itself (0.8 across: past
    // the head-sized clays the check looks at besides the hair's own).
    assert_eq!(r["hair"]["collision_check"], "sampled against nearby head clay", "{}", r["hair"]);
    assert_eq!(r["hair"]["strands_inside_head"], 0);
    let h = hair(app, "Coat");
    let fur = h.fur.clone().unwrap();
    assert_eq!((fur.drape, fur.clump, fur.clump_size), (1.0, 0.6, 3.0), "long fur gathers into locks by default");
    assert!((fur.wave - 0.03 * 0.22).abs() < 1e-6 && fur.waves == 2.5 && fur.ragged == 0.25, "a gentle wave and ragged ends by default");
    let floor = fur.floor.expect("a floor");
    assert!(floor[1] > 0.999 && floor[3].abs() < 0.002, "{floor:?}");
    let head = app.head_collider(id_of(app, "Dog")).unwrap();
    let (mut flank, mut hanging, mut inside, mut under, mut worst) = (0, 0, 0, 0, 0.0f32);
    // The lids' short fur aside (`Fur::lids`).
    let root = |g: &crate::hair::Guide| head.snap(g.points[0]).map_or(g.points[0], |(q, _)| q);
    for g in h.guides.iter().filter(|g| !fur.lids.iter().any(|l| l.holds(root(g)))) {
        let p = &g.points;
        assert_eq!(p.len(), 17);
        let l: f32 = p.windows(2).map(|w| w[0].distance(w[1])).sum();
        worst = worst.max((l - 0.22).abs() / 0.22);
        let n = head.normal_at(p[0]).unwrap_or(Vec3::Y);
        if n.y.abs() < 0.3 && p[0].y > 0.25 && p[0].y < 0.45 {
            flank += 1;
            let tip = (p[16] - p[15]).normalize();
            if p[16].y < p[0].y - 0.12 && tip.y < -0.9 {
                hanging += 1;
            }
        }
        inside += p.iter().filter(|q| head.inside(**q)).count();
        under += p.iter().filter(|q| q.y < floor[3] - 1e-4).count();
    }
    eprintln!("{flank} flank guides, {hanging} hang (tip 0.12 below the root, last segment within 25 degrees of down); {inside} guide points inside, {under} under the floor");
    eprintln!("guide lengths within {:.3}% of 0.22", worst * 100.0);
    assert!(worst < 0.03, "{worst}");
    assert!(flank > 50 && hanging as f32 >= 0.9 * flank as f32);
    assert_eq!((inside, under), (0, 0));
    let one = crate::clay::with_threads(1, || app.hair_strands(id_of(app, "Coat"), Some(8000)).strands);
    let kept = app.hair_strands(id_of(app, "Coat"), Some(8000));
    assert!(one.len() == kept.strands.len() && one.iter().zip(&kept.strands).all(|(a, b)| a.points == b.points && a.root_color == b.root_color), "1 and 12 threads");
    let strands = kept.strands;
    let inside = crate::hair::inside_count(&strands, |p| head.inside(p));
    let under = strands.iter().filter(|s| s.points.iter().any(|p| p.y < floor[3] - 1e-4)).count();
    eprintln!("{} strands sampled: {inside} inside the clay, {under} under the floor", strands.len());
    assert_eq!((inside, under), (0, 0));
    // Save and load.
    let d = std::env::temp_dir().join(format!("clide3d_longhair_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("dog.json");
    ok(app, ctx, tx, "save_scene", json!({ "path": path.display().to_string() }));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"drape\"") && text.contains("\"floor\""));
    ok(app, ctx, tx, "new_scene", json!({}));
    ok(app, ctx, tx, "open_scene", json!({ "path": path.display().to_string() }));
    settle(app, ctx);
    assert_eq!(hair(app, "Coat"), h, "save and load keep the coat");
    let _ = std::fs::remove_dir_all(&d);
    // Short fur stores neither.
    grow(app, ctx, tx, "Short", json!({ "fur": true, "on": "Dog", "count": 200, "strands": 1000 }));
    let short = hair(app, "Short").fur.unwrap();
    assert_eq!((short.drape, short.floor), (0.0, None));
    assert!(!serde_json::to_string(&short).unwrap().contains("drape"));
}

/// `drape_refusals`: a drape past 1, segments past 32, a floor that is
/// not true or false, a clump past 1, a clump size past 8, a clump
/// without drape, a wave past 1, waves past 16, ragged past 0.9 and a
/// wave without drape are refused with the scene unchanged.
#[test]
fn drape_refusals() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sheepdog(app, ctx, tx);
    ok(app, ctx, tx, "create_object", json!({ "shape": "Hair", "name": "Coat", "parent": "Dog" }));
    let scene = app.scene.clone();
    for (g, says) in [
        (json!({ "fur": true, "on": "Dog", "drape": 2 }), "grow.drape"),
        (json!({ "fur": true, "on": "Dog", "drape": -0.5 }), "grow.drape"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "segments": 33 }), "2 to 32"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "floor": "yes" }), "grow.floor"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "clump": 2 }), "grow.clump"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "clump_size": 9 }), "clump_size"),
        (json!({ "fur": true, "on": "Dog", "clump": 0.5 }), "needs drape"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "wave": 2 }), "grow.wave"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "waves": 20 }), "grow.waves"),
        (json!({ "fur": true, "on": "Dog", "drape": 1, "ragged": 1 }), "grow.ragged"),
        (json!({ "fur": true, "on": "Dog", "wave": 0.01 }), "need drape"),
    ] {
        let (v, err) = super::recorder::tests::mcp(app, ctx, tx, "update_object", json!({ "target": "Coat", "hair": { "grow": g.clone() } }));
        assert!(err && v.to_string().contains(says), "{g}: {v}");
        assert_eq!(app.scene, scene, "{g}: nothing changed");
    }
}

/// `coat_parts_along_the_spine`: with `part: true` the guides rooted
/// where the skin faces up (the back, the crown) leave the parting
/// sideways and end on their own side, lower; without it the back's
/// hair leaves along the spine (and only slides off the barrel); no
/// long strand crosses the parting (each blends and gathers only with
/// guides on its own side), the plane is stored in the hair's space and
/// kept by save; a parting that is not a boolean or a number is refused.
#[test]
fn coat_parts_along_the_spine() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sheepdog(app, ctx, tx);
    let base = json!({ "fur": true, "on": "Dog", "length": 0.2, "drape": 1, "count": 1500, "strands": 30000 });
    let mut parted = base.clone();
    parted["part"] = json!(true);
    grow(app, ctx, tx, "Parted", parted);
    grow(app, ctx, tx, "Flat", base);
    let head = app.head_collider(id_of(app, "Dog")).unwrap();
    // Guides on the back (up-facing, off the line by a cell or more,
    // behind the shoulders: the neck under the head is not on top).
    let back = |h: &Hair| -> (usize, usize, usize, usize) {
        let (mut n, mut own_side, mut crossed, mut across) = (0, 0, 0, 0);
        for g in h.guides.iter() {
            let (r, t) = (g.points[0], *g.points.last().unwrap());
            if head.normal_at(r).is_some_and(|n| n.y > 0.5) && r.x.abs() > 0.005 && r.z > -0.3 && r.z < 0.1 && r.y > 0.3 && r.y < 0.56 {
                n += 1;
                if t.x * r.x.signum() > r.x.abs() + 0.02 && t.y < r.y - 0.05 {
                    own_side += 1;
                }
                if t.x * r.x.signum() < 0.0 {
                    crossed += 1;
                }
                // Leaving sideways: the first segment runs across the back.
                if (g.points[1] - g.points[0]).normalize().x * r.x.signum() > 0.5 {
                    across += 1;
                }
            }
        }
        (n, own_side, crossed, across)
    };
    let (n, side, crossed, across) = back(&hair(app, "Parted"));
    let (fn_, fside, fcrossed, facross) = back(&hair(app, "Flat"));
    eprintln!("back guides parted: {n}, {across} leave sideways, {side} end down their own side, {crossed} cross; unparted: {fn_}, {facross} leave sideways, {fside} end down their side, {fcrossed} cross");
    assert!(n > 40 && across as f32 >= 0.95 * n as f32 && side as f32 >= 0.95 * n as f32 && crossed == 0);
    assert!(facross as f32 <= 0.1 * fn_ as f32, "{facross} {fcrossed}");
    let h = hair(app, "Parted");
    let plane = h.fur.as_ref().unwrap().part.expect("stored");
    assert!((plane[0] - 1.0).abs() < 1e-5 && plane[3].abs() < 1e-5, "{plane:?}");
    // No strand crosses: rooted off the line on the back, it ends on its side.
    let strands = app.hair_strands(id_of(app, "Parted"), Some(12000)).strands;
    let (mut on_back, mut cross) = (0, 0);
    for s in &strands {
        let (r, t) = (s.points[0], *s.points.last().unwrap());
        if r.y > 0.45 && r.y < 0.56 && r.z > -0.3 && r.z < 0.1 && r.x.abs() > 0.005 {
            on_back += 1;
            if t.x * r.x.signum() < 0.0 {
                cross += 1;
            }
        }
    }
    eprintln!("{on_back} strands rooted on the back, {cross} end across the parting");
    assert!(on_back > 300 && cross == 0);
    let text = serde_json::to_string(&h).unwrap();
    assert!(text.contains("\"part\""));
    assert!(!serde_json::to_string(&hair(app, "Flat")).unwrap().contains("\"part\""));
    let scene = app.scene.clone();
    let (v, err) = super::recorder::tests::mcp(app, ctx, tx, "update_object", json!({ "target": "Flat", "hair": { "grow": { "fur": true, "on": "Dog", "drape": 1, "part": "middle" } } }));
    assert!(err && v.to_string().contains("grow.part"), "{v}");
    assert_eq!(app.scene, scene);
}

/// `fringe_falls_over_the_eyes`: a fringe combed forward from the
/// forehead (`direction`) hangs in front of the eyes: many strands pass
/// in front of each eye (seen from the front, over its disc), none
/// enters an eye (long strands slide over them rather than being left
/// out), and none is inside the clay.
#[test]
fn fringe_falls_over_the_eyes() {
    let Some((mut app, tx, ctx)) = headless() else { return };
    let (app, ctx, tx) = (&mut app, &ctx, &tx);
    sheepdog(app, ctx, tx);
    grow(app, ctx, tx, "Coat", json!({ "fur": true, "on": "Dog", "length": 0.2, "drape": 1, "part": true, "count": 2000, "strands": 40000,
        "direction": [{ "center": [0, 0.74, 0.40], "radius": 0.11, "direction": [0, -0.2, 1] }],
        "lengths": [{ "center": [0, 0.72, 0.42], "radius": 0.1, "length": 0.155 }] }));
    let eyes = eyes(app);
    let strands = app.hair_strands(id_of(app, "Coat"), None).strands;
    let head = app.head_collider(id_of(app, "Dog")).unwrap();
    let into = strands.iter().filter(|s| s.points.iter().chain(s.points.windows(2).map(|w| (w[0] + w[1]) * 0.5).collect::<Vec<_>>().iter()).any(|p| eyes.iter().any(|(c, r)| p.distance(*c) < *r))).count();
    let front: Vec<usize> = eyes.iter().map(|(c, r)| strands.iter().filter(|s| s.points.iter().any(|p| p.z > c.z && Vec3::new(p.x - c.x, p.y - c.y, 0.0).length() < *r)).count()).collect();
    let inside = crate::hair::inside_count(&strands, |p| head.inside(p));
    eprintln!("{} strands: {front:?} pass in front of the eyes, {into} enter an eye, {inside} inside the clay", strands.len());
    // (192 and 166 before the lids grew fur: their caps now take a few of
    // the 2,000 guides, and the forehead's spread a little thinner: 202
    // and 150.)
    assert!(front.iter().all(|f| *f > 130) && into == 0 && inside == 0);
}
