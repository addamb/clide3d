//! Stage 3 acceptance (docs/SCULPT_PLAN.md 6): the persistent mask and
//! its ops, auto-masking, polygroups and the masked transform, on clays
//! directly (the MCP half is `app/masking/tests.rs`).

use glam::Vec3;
use serde_json::{Value, json};

use crate::band::Band;
use crate::brush::resolve::{ResolveCtx, Resolved, resolve};
use crate::brush::spec::StrokeSpec;
use crate::clay::{Clay, Detail};
use crate::groups::{GroupOp, Groups};
use crate::ledger::{self, Grid};
use crate::mask::{MaskOp, Mode, Select, SculptMask};
use crate::mesh::Mesh;

fn clay(blobs: Value, resolution: u32, cell: Option<f32>) -> Clay {
    let mut c: Clay = serde_json::from_value(json!({ "resolution": resolution, "blobs": blobs })).unwrap();
    c.detail_cell = cell;
    c
}

/// A ball of radius 0.5, 2 cm detail cells.
fn ball() -> Clay {
    clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 64, Some(0.02))
}

fn resolved(spec: Value) -> Resolved {
    let spec: StrokeSpec = serde_json::from_value(spec).unwrap();
    resolve(&spec, &ResolveCtx::local()).unwrap()
}

/// A stroke as the sculpt tool commits it: the mask and groups it reads
/// kept by hash, then applied.
fn stroke(c: &mut Clay, spec: Value) -> super::StrokeReport {
    let mut r = resolved(spec);
    ledger::freeze(c, &mut r).unwrap();
    super::run(c, &r).unwrap()
}

fn act(c: &Clay, op: MaskOp) -> Option<SculptMask> {
    crate::mask::act(c, c.sculpt_mask.as_ref(), &op, &|h| ledger::frozen_groups(c, h)).unwrap()
}

fn select(select: Select) -> MaskOp {
    MaskOp::Select { select, mode: Mode::Set }
}

/// Every band point of the clay's mask cell at value `v`.
fn uniform(c: &Clay, v: u8) -> SculptMask {
    let voxel = crate::mask::voxel_of(c).unwrap();
    let band = Band::of(c, voxel).unwrap();
    crate::mask::write(SculptMask::new(voxel), &band, &vec![v; band.len()]).unwrap()
}

fn masked_vertices(mesh: &Mesh, m: Option<&SculptMask>) -> Vec<bool> {
    mesh.vertices.iter().map(|v| m.is_some_and(|m| m.grid().value(Vec3::from(v.pos)) >= 0.5)).collect()
}

fn iou(a: &[bool], b: &[bool]) -> f64 {
    let both = a.iter().zip(b).filter(|(x, y)| **x && **y).count() as f64;
    let either = a.iter().zip(b).filter(|(x, y)| **x || **y).count() as f64;
    if either == 0.0 { 1.0 } else { both / either }
}

/// The strokes of the brush check: every brush, one dab (or a short
/// path) on the ball's top.
fn every_brush() -> Vec<Value> {
    let top = json!([0.0, 0.5, 0.0]);
    vec![
        json!({ "brush": "standard", "point": top, "radius": 0.15 }),
        json!({ "brush": "inflate", "point": top, "radius": 0.15 }),
        json!({ "brush": "deflate", "point": top, "radius": 0.15 }),
        json!({ "brush": "smooth", "point": top, "radius": 0.15, "strength": 0.8 }),
        json!({ "brush": "flatten", "point": top, "radius": 0.15 }),
        json!({ "brush": "crease", "point": top, "radius": 0.1, "strength": 0.01 }),
        json!({ "brush": "pinch", "point": top, "radius": 0.15 }),
        json!({ "brush": "layer", "point": top, "radius": 0.15, "strength": 0.01 }),
        json!({ "brush": "add", "point": top, "radius": 0.06 }),
        json!({ "brush": "carve", "point": top, "radius": 0.06 }),
        json!({ "brush": "clay", "point": top, "radius": 0.15 }),
        json!({ "brush": "clay_buildup", "point": top, "radius": 0.15 }),
        json!({ "brush": "trim_dynamic", "point": top, "radius": 0.15 }),
        json!({ "brush": "polish", "point": top, "radius": 0.15 }),
        json!({ "brush": "clay_tubes", "points": [[-0.05, 0.5, 0.0], [0.05, 0.5, 0.0]], "radius": 0.08 }),
        json!({ "brush": "grab", "point": top, "radius": 0.15, "delta": [0.0, 0.04, 0.0] }),
        json!({ "brush": "stretch", "point": top, "radius": 0.1, "delta": [0.0, 0.06, 0.0] }),
        json!({ "brush": "snakehook", "points": [[0.0, 0.5, 0.0], [0.0, 0.6, 0.0]], "radius": 0.08 }),
        json!({ "brush": "noise", "point": top, "radius": 0.15, "pattern": "bumps", "scale": 0.06, "strength": 0.5 }),
        json!({ "brush": "paint", "point": top, "radius": 0.15, "color": [1, 0, 0] }),
        json!({ "brush": "transform", "transform": { "translate": [0.0, 0.05, 0.0] } }),
    ]
}

/// Inside a full mask nothing changes (every grid point under it keeps its
/// bits); under a half mask a single dab changes half as much.
#[test]
fn a_full_mask_freezes_every_brush_and_a_half_mask_halves_the_change() {
    let mut base = ball();
    // A grid to change, sculpted before masking.
    stroke(&mut base, json!({ "brush": "inflate", "point": [0.3, 0.4, 0.0], "radius": 0.1 }));
    let full = uniform(&base, 255);
    let voxel = full.grid().voxel;
    for spec in every_brush() {
        let brush = spec["brush"].as_str().unwrap().to_string();
        let mut c = base.clone();
        c.sculpt_mask = Some(full.clone());
        stroke(&mut c, spec.clone());
        let mut changed_masked = 0;
        ledger::detail_changes(base.detail.as_ref(), c.detail.as_ref(), |p, _, _, _| {
            if full.grid().value(p) >= 1.0 {
                changed_masked += 1;
            }
        });
        assert_eq!(changed_masked, 0, "{brush}: masked grid points changed");
        if let Some(p) = &c.paint {
            let band = Band::of(&base, voxel).unwrap();
            let painted = band.keys.iter().filter(|i| p.grid().get(**i)[3] > 0).count();
            assert_eq!(painted, 0, "{brush}: masked surface painted");
        }
        // The surface stays (nothing grew out of the air beside it).
        let (a, b) = (base.build_mesh(), c.build_mesh());
        let moved = crate::harness::geom::hausdorff(&a, &b);
        assert!(moved <= 0.05 * voxel, "{brush}: the masked surface moved {moved}");
    }
    // Half mask: single dabs of the brushes whose change scales linearly.
    let half = uniform(&base, 128);
    let band = Band::of(&base, voxel).unwrap();
    for spec in every_brush().into_iter().filter(|s| !matches!(s["brush"].as_str(), Some("noise" | "paint" | "transform"))) {
        let brush = spec["brush"].as_str().unwrap().to_string();
        let mut free = base.clone();
        stroke(&mut free, spec.clone());
        let mut c = base.clone();
        c.sculpt_mask = Some(half.clone());
        stroke(&mut c, spec);
        let (b, f, h) = (base.detail.as_ref().unwrap().grid(), free.detail.as_ref().unwrap().grid(), c.detail.as_ref().unwrap().grid());
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for (k, i) in band.keys.iter().enumerate() {
            let df = f.get(*i) - b.get(*i);
            // Clamped values are not linear in the change.
            if df.abs() < 1e-5 || (band.t[k] + df).abs() >= 3.9 * voxel {
                continue;
            }
            num += (h.get(*i) - b.get(*i)) as f64 * df as f64;
            den += (df as f64).powi(2);
        }
        let ratio = num / den.max(1e-30);
        eprintln!("{brush}: half mask keeps {ratio:.4} of the change");
        assert!(den > 0.0 && (ratio - 0.5).abs() <= 0.02, "{brush}: {ratio}");
    }
}

#[test]
fn inverting_twice_is_bit_exact() {
    let mut c = ball();
    c.sculpt_mask = act(&c, select(Select::Normal { dir: Vec3::Y, angle: 40.0 }));
    stroke(&mut c, json!({ "brush": "inflate", "point": [0.0, 0.0, 0.5], "radius": 0.2 }));
    let before = SculptMask::hash(c.sculpt_mask.as_ref());
    let once = act(&c, MaskOp::Invert);
    assert_ne!(SculptMask::hash(once.as_ref()), before);
    let twice = act(&Clay { sculpt_mask: once, ..c.clone() }, MaskOp::Invert);
    assert_eq!(SculptMask::hash(twice.as_ref()), before);
}

/// Masked band points with an unmasked band neighbour.
fn boundary(band: &Band, m: &SculptMask) -> Vec<Vec3> {
    let on = |k: usize| m.grid().get(band.keys[k]) >= 128;
    (0..band.len()).filter(|&k| on(k) && band.neighbours(k).into_iter().flatten().any(|n| !on(n))).map(|k| band.point(k)).collect()
}

fn hausdorff(a: &[Vec3], b: &[Vec3]) -> f32 {
    let one = |x: &[Vec3], y: &[Vec3]| x.iter().map(|p| y.iter().map(|q| p.distance(*q)).fold(f32::MAX, f32::min)).fold(0.0f32, f32::max);
    one(a, b).max(one(b, a))
}

#[test]
fn grow_then_shrink_gives_the_disc_back_and_blur_keeps_mass() {
    let mut c = ball();
    let mesh = c.build_mesh();
    c.sculpt_mask = act(&c, select(Select::Normal { dir: Vec3::Y, angle: 35.0 }));
    let voxel = c.sculpt_mask.as_ref().unwrap().grid().voxel;
    let band = Band::of(&c, voxel).unwrap();
    let disc = c.sculpt_mask.clone().unwrap();
    let cov = crate::mask::coverage(Some(&disc), &mesh);
    let grown = act(&c, MaskOp::Grow { cells: 3 });
    let back = act(&Clay { sculpt_mask: grown.clone(), ..c.clone() }, MaskOp::Shrink { cells: 3 }).unwrap();
    let cov2 = crate::mask::coverage(Some(&back), &mesh);
    let h = hausdorff(&boundary(&band, &disc), &boundary(&band, &back));
    eprintln!("disc coverage {cov:.4}, grown {:.4}, back {cov2:.4}; boundary Hausdorff {:.2} cells", crate::mask::coverage(grown.as_ref(), &mesh), h / voxel);
    assert!((cov2 - cov).abs() <= 0.02 * cov, "coverage {cov} -> {cov2}");
    assert!(h <= voxel * 1.0001, "boundary moved {} cells", h / voxel);
    // Blur: the mask's sum over the band stays.
    let mass = |m: &SculptMask| band.keys.iter().map(|i| m.grid().get(*i) as f64).sum::<f64>();
    let blurred = act(&c, MaskOp::Blur { cells: 3 }).unwrap();
    let (m0, m1) = (mass(&disc), mass(&blurred));
    eprintln!("blur: mass {m0} -> {m1} ({:+.3}%)", (m1 / m0 - 1.0) * 100.0);
    assert!((m1 / m0 - 1.0).abs() <= 0.03);
    assert!(blurred.grid().values().any(|(_, v)| v > 0 && v < 255), "blur softens the edge");
}

#[test]
fn a_front_rect_masks_its_projection_and_through_false_spares_the_back() {
    // Fine cells: the mask is read between grid points at the vertices.
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 64, Some(0.004));
    let mesh = c.build_mesh();
    let e = 20.0;
    let (camera, to_px) = crate::mask::named_camera("front", e).unwrap();
    let (lo, hi) = ([-0.3f32, -0.1f32], [0.2f32, 0.35f32]);
    let px: Vec<[f32; 2]> = [[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]].iter().map(|a| to_px(*a, e)).collect();
    let inside = |p: Vec3| (lo[0]..=hi[0]).contains(&p.x) && (lo[1]..=hi[1]).contains(&p.y);
    for through in [true, false] {
        c.sculpt_mask = None;
        let m = act(&c, select(Select::Lasso { camera, px: px.clone(), through }));
        let got = masked_vertices(&mesh, m.as_ref());
        let want: Vec<bool> = mesh.vertices.iter().map(|v| inside(Vec3::from(v.pos)) && (through || v.pos[2] > 0.0)).collect();
        let score = iou(&got, &want);
        let back = mesh.vertices.iter().zip(&got).filter(|(v, g)| **g && v.pos[2] < -0.05).count();
        eprintln!("rect through {through}: IoU {score:.4}, back vertices masked {back}");
        assert!(score >= 0.98, "through {through}: IoU {score}");
        if !through {
            assert_eq!(back, 0, "through: false leaves back faces unmasked");
        }
    }
}

#[test]
fn by_cavity_finds_a_groove() {
    // A plane (a slab's top at y = 0) with a groove 8 cells wide along x.
    let cell = 0.005;
    let rho = 0.02;
    let c = clay(json!([
        { "type": "box", "center": [0, -0.1, 0], "size": [0.4, 0.2, 0.4], "blend": 0 },
        { "type": "capsule", "a": [-0.25, 0, 0], "b": [0.25, 0, 0], "radius": rho, "op": "carve", "blend": 0 }
    ]), 96, Some(cell));
    let mesh = c.build_mesh();
    let m = act(&c, select(Select::Cavity { radius: 2.0 * cell }));
    let got = masked_vertices(&mesh, m.as_ref());
    // The groove: below the plane, within its width, away from the slab's ends.
    let want: Vec<bool> = mesh.vertices.iter().map(|v| v.pos[1] < -0.002 && v.pos[2].abs() < rho && v.pos[0].abs() < 0.15).collect();
    let near: Vec<(bool, bool)> = mesh.vertices.iter().zip(got.iter().zip(&want)).filter(|(v, _)| v.pos[0].abs() < 0.15 && v.pos[2].abs() < 0.15 && v.pos[1] > -0.05).map(|(_, (g, w))| (*g, *w)).collect();
    let (g, w): (Vec<bool>, Vec<bool>) = near.into_iter().unzip();
    let score = iou(&g, &w);
    eprintln!("by_cavity: IoU {score:.3} against the groove ({} groove vertices)", w.iter().filter(|x| **x).count());
    assert!(score >= 0.8, "IoU {score}");
}

#[test]
fn a_connected_grab_moves_one_finger_only() {
    let cell = 0.01;
    let c0 = clay(json!([
        { "type": "box", "center": [0, -0.2, 0], "size": [0.6, 0.2, 0.3], "blend": 0.02 },
        { "type": "capsule", "name": "a", "a": [-0.13, -0.1, 0], "b": [-0.13, 0.3, 0], "radius": 0.1, "blend": 0 },
        { "type": "capsule", "name": "b", "a": [0.13, -0.1, 0], "b": [0.13, 0.3, 0], "radius": 0.1, "blend": 0 }
    ]), 64, Some(cell));
    let grab = |connected: bool| {
        let mut c = c0.clone();
        let mut spec = json!({ "brush": "grab", "point": [-0.13, 0.4, 0.0], "radius": 0.3, "delta": [0.0, 0.05, 0.0] });
        if connected {
            spec["mask"] = json!({ "connected": true });
        }
        stroke(&mut c, spec);
        // How far the other finger's surface moved: the field's change on
        // it (its band points within a cell of the surface).
        let band = Band::of(&c0, cell).unwrap();
        let (f0, f1) = (crate::clay::Field::new(&c0, cell), crate::clay::Field::new(&c, cell));
        let at = |f: &crate::clay::Field, p: Vec3| f.sample(p, &f.near(p - cell, p + cell)).0;
        (0..band.len()).filter(|&k| band.point(k).x > 0.03 && band.point(k).y > -0.05 && band.t[k].abs() <= cell).map(|k| (at(&f1, band.point(k)) - at(&f0, band.point(k))).abs()).fold(0.0f32, f32::max)
    };
    let (free, kept) = (grab(false), grab(true));
    eprintln!("other finger: max_disp {:.3} cells without connected, {:.4} with", free / cell, kept / cell);
    assert!(free > 0.5 * cell, "the plain grab reaches the other finger");
    assert!(kept <= 0.1 * cell, "connected: {} cells", kept / cell);
}

#[test]
fn the_mask_survives_a_resolution_change() {
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, None);
    c.sculpt_mask = act(&c, select(Select::Normal { dir: Vec3::new(0.3, 1.0, 0.2), angle: 50.0 }));
    let a = crate::mask::coverage(c.sculpt_mask.as_ref(), &c.build_mesh());
    c.resolution = 160;
    let b = crate::mask::coverage(c.sculpt_mask.as_ref(), &c.build_mesh());
    eprintln!("coverage at res 96 {a:.4}, at 160 {b:.4}");
    assert!((b - a).abs() <= 0.02 * a, "{a} -> {b}");
}

/// A mirrored blob makes two field prims but one group, labelled by its
/// blob index (`Clay::blobs`), so the blobs after it keep their numbers.
#[test]
fn from_blobs_numbers_blobs_not_prims() {
    let c = clay(json!([
        { "type": "sphere", "name": "body", "center": [0, 0, 0], "radius": 0.4, "blend": 0.05 },
        { "type": "sphere", "name": "ear", "center": [0.3, 0.3, 0], "radius": 0.12, "blend": 0.05, "mirror": true },
        { "type": "sphere", "center": [0, -0.45, 0], "radius": 0.12, "blend": 0.05 }
    ]), 64, Some(0.02));
    let g = crate::groups::act(&c, None, &GroupOp::FromBlobs, &|_| None).unwrap().unwrap();
    let info: Vec<(u16, String)> = g.data().info.iter().map(|(id, i)| (*id, i.name.clone())).collect();
    assert_eq!(info, vec![(1, "body".to_string()), (2, "ear".to_string()), (3, "blob 2".to_string())], "{info:?}");
    // Both ears in the ear's group.
    let (right, left) = (g.label(Vec3::new(0.38, 0.38, 0.0)), g.label(Vec3::new(-0.38, 0.38, 0.0)));
    assert_eq!((right, left), (2, 2));
}

#[test]
fn from_blobs_labels_each_blob_and_hidden_groups_stop_brushes() {
    let c0 = clay(json!([
        { "type": "sphere", "name": "head", "center": [0, 0.6, 0], "radius": 0.25, "blend": 0.05 },
        { "type": "sphere", "name": "body", "center": [0, 0, 0], "radius": 0.4, "blend": 0.05 },
        { "type": "sphere", "name": "tail", "center": [0.5, -0.1, 0], "radius": 0.15, "blend": 0.05 }
    ]), 64, Some(0.02));
    let g = crate::groups::act(&c0, None, &GroupOp::FromBlobs, &|_| None).unwrap().unwrap();
    assert_eq!(g.data().info.len(), 3, "{:?}", g.data().info);
    let field = crate::clay::Field::new(&c0, 0.02);
    let band = Band::of(&c0, 0.02).unwrap();
    let names: Vec<String> = g.data().info.values().map(|i| i.name.clone()).collect();
    let owned = (0..band.len()).filter(|&k| field.owner_name(band.point(k)).as_deref() == g.data().info.get(&g.label(band.point(k))).map(|i| i.name.as_str())).count();
    let share = owned as f64 / band.len() as f64;
    eprintln!("from_blobs: {names:?}, {share:.4} of band points owned by their group's blob");
    assert!(share >= 0.95);
    // Hide the head: a brush over it changes nothing, and its triangles
    // leave the viewport's mesh.
    let head = *g.data().info.iter().find(|(_, i)| i.name == "head").unwrap().0;
    let hidden = crate::groups::act(&c0, Some(&g), &GroupOp::Hide { ids: vec![head] }, &|_| None).unwrap().unwrap();
    let mut c = Clay { groups: Some(hidden.clone()), ..c0.clone() };
    let r = stroke(&mut c, json!({ "brush": "inflate", "point": [0.0, 0.85, 0.0], "radius": 0.12 }));
    assert_eq!(r.changed_points, 0, "a brush over a hidden group");
    let mesh = c0.build_mesh();
    let shown = crate::overlay::tint(&mesh, &mesh, None, Some(&hidden), false);
    let label = |i: u32| hidden.label(Vec3::from(mesh.vertices[i as usize].pos));
    let inside = |t: &[u32]| t.iter().all(|i| label(*i) == head);
    assert!(mesh.indices.chunks_exact(3).any(inside));
    assert!(!shown.indices.chunks_exact(3).any(inside), "hidden triangles are left out");
    assert!(shown.indices.len() < mesh.indices.len() && shown.indices.len() > mesh.indices.len() / 2);
    // Groups by mask: from_mask and by_groups round trip.
    let mask = act(&c0, select(Select::Height { along: Vec3::Y, offset: 0.0, min: 0.5, max: 2.0 })).unwrap();
    let mut c = Clay { sculpt_mask: Some(mask.clone()), groups: Some(g.clone()), ..c0.clone() };
    ledger::keep_frozen(&mut c, Some(mask.clone()), Some(g.clone())).unwrap();
    let mh = SculptMask::hash(Some(&mask));
    let fm = crate::groups::act(&c, Some(&g), &GroupOp::FromMask { threshold: 0.5, id: None, mask: mh }, &|h| ledger::frozen_mask(&c, h)).unwrap().unwrap();
    assert_eq!(fm.data().info.len(), 4);
    let gh = Groups::hash(Some(&g));
    let by = crate::mask::act(&c, None, &MaskOp::Select { select: Select::Groups { ids: vec![head], groups: gh }, mode: Mode::Set }, &|h| ledger::frozen_groups(&c, h)).unwrap().unwrap();
    assert!(by.grid().value(Vec3::new(0.0, 0.85, 0.0)) > 0.9 && by.grid().value(Vec3::new(0.0, -0.4, 0.0)) == 0.0);
}

/// The masked transform: points under the mask keep their values to the
/// bit; the free part moves with the map.
#[test]
fn a_masked_transform_moves_only_the_free_part() {
    let mut c = ball();
    stroke(&mut c, json!({ "brush": "inflate", "point": [0.0, 0.3, 0.4], "radius": 0.1 }));
    // Freeze everything below y = 0.15.
    c.sculpt_mask = act(&c, select(Select::Height { along: Vec3::Y, offset: 0.0, min: -2.0, max: 0.15 }));
    let before = c.clone();
    let mask = c.sculpt_mask.clone().unwrap();
    stroke(&mut c, json!({ "brush": "transform", "transform": { "translate": [0.0, 0.04, 0.0] } }));
    let voxel = mask.grid().voxel;
    let (mut masked_max, mut moved) = (0.0f32, 0usize);
    let band = Band::of(&before, voxel).unwrap();
    let field_before = crate::clay::Field::new(&before, voxel);
    let field_after = crate::clay::Field::new(&c, voxel);
    for (k, i) in band.keys.iter().enumerate() {
        let p = band.point(k);
        let list = field_before.near(p - voxel, p + voxel);
        let (t0, t1) = (field_before.sample(p, &list).0, field_after.sample(p, &field_after.near(p - voxel, p + voxel)).0);
        if mask.grid().get(*i) == 255 {
            masked_max = masked_max.max((t1 - t0).abs());
        } else if (t1 - t0).abs() > 1e-4 {
            moved += 1;
        }
    }
    eprintln!("masked transform: masked band points moved at most {masked_max:e}; {moved} free points changed");
    assert!(masked_max <= 1e-6, "{masked_max}");
    assert!(moved > 100);
    // The free cap rose by the move: the top is 4 cm higher.
    let top = |cl: &Clay| cl.build_mesh().vertices.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
    let rise = top(&c) - top(&before);
    eprintln!("the cap rose {rise:.4} (moved 0.04)");
    assert!((rise - 0.04).abs() <= voxel * 0.5, "{rise}");
}

/// A transform moving the free part further than a block: the old free
/// surface goes (no cap left behind) and no air stays inside the moved
/// material (a block is skipped only when it and its source are both far
/// from the surface).
#[test]
fn a_long_masked_transform_leaves_no_old_surface() {
    let mut base = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }]), 64, Some(0.02));
    base.sculpt_mask = act(&base, select(Select::Height { along: Vec3::Y, offset: 0.0, min: -2.0, max: 0.15 }));
    let at = |c: &Clay, p: Vec3| {
        let field = crate::clay::Field::new(c, 0.02);
        field.sample(p, &field.near(p - 0.02, p + 0.02)).0
    };
    // Sideways: the old cap is air now, the moved one solid.
    let mut c = base.clone();
    stroke(&mut c, json!({ "brush": "transform", "transform": { "translate": [1.0, 0.0, 0.0] } }));
    let (old, new) = (at(&c, Vec3::new(0.0, 0.46, 0.0)), at(&c, Vec3::new(1.0, 0.46, 0.0)));
    eprintln!("sideways: old cap {old:.4}, moved cap {new:.4}");
    assert!(old > 0.0 && new < 0.0, "old cap {old}, moved cap {new}");
    // Upwards by half the ball: solid all along the axis to the new top.
    let mut c = base.clone();
    stroke(&mut c, json!({ "brush": "transform", "transform": { "translate": [0.0, 0.5, 0.0] } }));
    let worst = (25..48).map(|k| at(&c, Vec3::new(0.0, k as f32 * 0.02, 0.0))).fold(f32::MIN, f32::max);
    eprintln!("upwards: the axis from 0.5 to 0.94 is at most {worst:.4}");
    assert!(worst < 0.0, "air inside the moved material: {worst}");
}

/// Mask and group records replay to the same grid, bit for bit, and a
/// masked stroke replays the same after the mask changes.
#[test]
fn mask_and_group_ledgers_replay_bit_equal() {
    use crate::ledger::{Meta, commit, commit_action, edit, install, ledger as ledger_of};
    let mut c = ball();
    let meta = Meta::agent("mask", None);
    let ops = [
        select(Select::Normal { dir: Vec3::Y, angle: 45.0 }),
        MaskOp::Grow { cells: 2 },
        MaskOp::Blur { cells: 1 },
        MaskOp::Select { select: Select::Height { along: Vec3::X, offset: 0.0, min: 0.2, max: 1.0 }, mode: Mode::Add },
        MaskOp::Invert,
        MaskOp::Sharpen { amount: 1.0 },
    ];
    for op in ops {
        let r = commit_action::<SculptMask>(&c, &ledger_of(&c), &meta, json!("op"), op, None).unwrap();
        install(&mut c, r.ledger, r.grid);
    }
    let groups = [GroupOp::FromBlobs, GroupOp::FromNormals { angle: 40.0 }];
    for op in groups {
        let r = commit_action::<Groups>(&c, &ledger_of(&c), &Meta::agent("group", None), json!("op"), op, None).unwrap();
        install(&mut c, r.ledger, r.grid);
    }
    let paint = resolved(json!({ "brush": "mask_paint", "points": [[0.0, 0.0, 0.5], [0.2, 0.1, 0.45]], "radius": 0.1, "strength": 0.8 }));
    let spec: StrokeSpec = serde_json::from_value(json!({ "brush": "mask_paint", "points": [[0.0, 0.0, 0.5]], "radius": 0.1 })).unwrap();
    let r = commit::<SculptMask>(&c, &ledger_of(&c), &meta, &spec, &paint, false, None).unwrap();
    install(&mut c, r.ledger, r.grid);
    let (mh, gh) = (SculptMask::hash(c.sculpt_mask.as_ref()), Groups::hash(c.groups.as_ref()));
    // The mask and groups replay bit-equal on the surface they read.
    let e = edit::<SculptMask>(&c, &ledger_of(&c), ledger::Edit::Replay(0), None).unwrap();
    assert!(e.mismatched.is_empty() && SculptMask::hash(e.grid.as_ref()) == mh, "mask replay: {:?}", e.mismatched);
    let e = edit::<Groups>(&c, &ledger_of(&c), ledger::Edit::Replay(0), None).unwrap();
    assert!(e.mismatched.is_empty() && Groups::hash(e.grid.as_ref()) == gh, "groups replay: {:?}", e.mismatched);
    let saved: Clay = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    let mut loaded = saved.clone();
    ledger::after_load(&mut loaded);
    assert_eq!((ledger_of::<SculptMask>(&loaded).verified(), ledger_of::<Groups>(&loaded).verified()), (Some(true), Some(true)));
    assert_eq!((SculptMask::hash(loaded.sculpt_mask.as_ref()), Groups::hash(loaded.groups.as_ref())), (mh, gh));
    // A masked stroke on the base layer.
    let mut s = resolved(json!({ "brush": "inflate", "point": [0.0, 0.5, 0.0], "radius": 0.3 }));
    ledger::freeze(&mut c, &mut s).unwrap();
    let sspec: StrokeSpec = serde_json::from_value(json!({ "brush": "inflate", "point": [0.0, 0.5, 0.0], "radius": 0.3 })).unwrap();
    let r = commit::<Detail>(&c, &ledger_of(&c), &Meta::agent("sculpt", None), &sspec, &s, false, None).unwrap();
    install(&mut c, r.ledger, r.grid);
    let dh = Detail::hash(c.detail.as_ref());
    // Sculpting changed the surface the mask and group records read: they
    // report it (stale_context; rebase takes the new surface).
    let ctx = SculptMask::context(&c);
    assert!(ledger_of::<SculptMask>(&c).records().iter().all(|r| r.stale(ctx, &c)));
    // The mask changes afterwards; the stroke still replays as it was made.
    let r = commit_action::<SculptMask>(&c, &ledger_of(&c), &meta, json!("clear"), MaskOp::Clear, None).unwrap();
    install(&mut c, r.ledger, r.grid);
    for check in [true, false] {
        let e = edit::<Detail>(&c, &ledger_of(&c), ledger::Edit::Replay(0), None).unwrap();
        assert!(e.mismatched.is_empty(), "base replay after the mask changed: {:?}", e.mismatched);
        assert_eq!(Detail::hash(e.grid.as_ref()), dh);
        // Through a save and load.
        if check {
            let text = serde_json::to_string(&c).unwrap();
            c = serde_json::from_str(&text).unwrap();
            ledger::after_load(&mut c);
            assert_eq!(ledger_of::<Detail>(&c).verified(), Some(true));
        }
    }
    // Up to the clear, the mask ledger gave the mask the stroke read.
    let l = ledger_of::<SculptMask>(&saved);
    let upto = ledger::replay(&l, &saved, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(SculptMask::hash(upto.as_ref()), mh);
    let f = ledger_of::<Detail>(&c).records().last().unwrap().stroke().unwrap().1.frozen.unwrap();
    assert_eq!(f.mask, Some(mh), "the stroke names the mask it read");
}

/// Masked strokes (persistent mask, auto-masking) give the same grid on
/// 1 and 12 threads.
#[test]
fn masked_strokes_are_identical_on_any_thread_count() {
    let mut base = ball();
    base.sculpt_mask = act(&base, select(Select::Normal { dir: Vec3::new(0.2, 1.0, 0.0), angle: 50.0 }));
    base.sculpt_mask = act(&base, MaskOp::Blur { cells: 2 });
    let specs = [
        json!({ "brush": "inflate", "point": [0.0, 0.45, 0.2], "radius": 0.2 }),
        json!({ "brush": "standard", "points": [[0.2, 0.4, 0.2], [-0.2, 0.4, 0.2]], "radius": 0.1, "mask": { "backface": true, "cavity": 0.5 } }),
        json!({ "brush": "grab", "point": [0.0, 0.5, 0.0], "radius": 0.2, "delta": [0.0, 0.05, 0.0], "mask": { "connected": true, "normal_angle": 60 } }),
        json!({ "brush": "smooth", "point": [0.3, 0.3, 0.2], "radius": 0.2, "mask": { "invert_channel": true } }),
        json!({ "brush": "transform", "transform": { "rotate": [0, 20, 0], "pivot": [0, 0, 0] } }),
    ];
    let run = |n: usize| {
        crate::clay::with_threads(n, || {
            let mut c = base.clone();
            for s in &specs {
                stroke(&mut c, s.clone());
            }
            Detail::hash(c.detail.as_ref())
        })
    };
    assert_eq!(run(1), run(12));
    // Mask and group ops (saved hashes are checked on load).
    let ops = |n: usize| {
        crate::clay::with_threads(n, || {
            let mut c = base.clone();
            for op in [select(Select::Cavity { radius: 0.04 }), MaskOp::Grow { cells: 2 }, MaskOp::Blur { cells: 2 }, MaskOp::Sharpen { amount: 1.0 }, select(Select::Connected { seed: Vec3::new(0.0, 0.5, 0.0) })] {
                c.sculpt_mask = act(&c, op);
            }
            let g = crate::groups::act(&c, None, &GroupOp::FromNormals { angle: 35.0 }, &|_| None).unwrap();
            let b = crate::groups::act(&c, None, &GroupOp::FromBlobs, &|_| None).unwrap();
            (SculptMask::hash(c.sculpt_mask.as_ref()), Groups::hash(g.as_ref()), Groups::hash(b.as_ref()))
        })
    };
    assert_eq!(ops(1), ops(12));
}

/// The overlay without a GPU: masked vertices at least 25% darker, groups
/// tinted, the rest as built.
#[test]
fn the_overlay_darkens_masked_vertices() {
    let mut c = ball();
    c.sculpt_mask = act(&c, select(Select::Normal { dir: Vec3::Y, angle: 40.0 }));
    let groups = crate::groups::act(&c, None, &GroupOp::FromNormals { angle: 60.0 }, &|_| None).unwrap();
    let mesh = c.build_mesh();
    let lum = |v: [f32; 3]| 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
    let m = c.sculpt_mask.as_ref().unwrap();
    let tinted = crate::overlay::tint(&mesh, &mesh, Some(m), None, false);
    let (mut masked, mut free) = (Vec::new(), 0usize);
    for (a, b) in tinted.vertices.iter().zip(&mesh.vertices) {
        let k = m.grid().value(Vec3::from(b.pos));
        if k >= 0.99 {
            masked.push(lum(a.color) / lum(b.color));
        } else if k == 0.0 {
            assert_eq!(a.color, b.color);
            free += 1;
        }
    }
    let mean = masked.iter().sum::<f32>() / masked.len() as f32;
    eprintln!("overlay: masked luminance x {mean:.3} over {} vertices ({free} untouched)", masked.len());
    assert!(!masked.is_empty() && free > 0 && mean <= 0.75);
    let grouped = crate::overlay::tint(&mesh, &mesh, None, groups.as_ref(), true);
    assert!(grouped.vertices.iter().zip(&mesh.vertices).filter(|(a, b)| a.color != b.color).count() > mesh.vertices.len() / 2);
    assert_eq!(grouped.indices, mesh.indices, "nothing hidden");
}

/// Renders a clay as the goldens do, with `overlay`.
fn render(c: &Clay, overlay: crate::overlay::Overlay) -> egui::ColorImage {
    use crate::harness::image;
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.color = [0.85, 0.72, 0.55];
    o.clay = Some(c.clone());
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.overlay = overlay;
    meshes.sync(&scene);
    image::render(&scene, &meshes, image::View::FRONT).unwrap_or_else(|e| panic!("CLIDE_GPU=1 but: {e}"))
}

/// The mask overlay (a masked cap, rendered darker) and the group
/// overlay (groups tinted, one hidden). `CLIDE_GPU=1` only.
#[test]
fn golden_mask_and_group_overlays() {
    use crate::harness::image;
    if !image::gpu_enabled() {
        eprintln!("golden_mask_and_group_overlays: set CLIDE_GPU=1 to run");
        return;
    }
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, Some(0.01));
    c.sculpt_mask = act(&c, select(Select::Normal { dir: Vec3::new(0.3, 1.0, 0.4), angle: 45.0 }));
    let plain = render(&Clay { sculpt_mask: None, ..c.clone() }, crate::overlay::Overlay::Mask);
    let masked = render(&c, crate::overlay::Overlay::Mask);
    // Where the overlay changed the render (the masked cap), it is at
    // least 25% darker.
    let lum = |p: &egui::Color32| 0.2126 * p.r() as f64 + 0.7152 * p.g() as f64 + 0.0722 * p.b() as f64;
    let (mut a, mut b, mut n) = (0.0, 0.0, 0);
    for (p, q) in masked.pixels.iter().zip(&plain.pixels) {
        if p != q {
            (a, b, n) = (a + lum(p), b + lum(q), n + 1);
        }
    }
    eprintln!("mask overlay: {n} pixels changed, luminance x {:.3}", a / b);
    assert!(n > 2000 && a / b <= 0.75);
    image::assert_golden("mask_overlay", &masked, 2.0, 8);
    let groups = crate::groups::act(&c, None, &GroupOp::FromNormals { angle: 50.0 }, &|_| None).unwrap().unwrap();
    let first = *groups.data().info.keys().next().unwrap();
    let groups = crate::groups::act(&c, Some(&groups), &GroupOp::Hide { ids: vec![first] }, &|_| None).unwrap();
    image::assert_golden("group_overlay", &render(&Clay { groups, ..c.clone() }, crate::overlay::Overlay::Groups), 2.0, 8);
}

/// Paint and mask strokes take alphas (Stage 2's deferral): a checker
/// alpha paints and masks only its white squares.
#[test]
fn paint_and_mask_strokes_take_alphas() {
    use crate::brush::alpha::{Alpha, AlphaRef, AlphaStore};
    let checker = Alpha::new(64, 64, (0..64 * 64).map(|i| if (i % 64 / 32 + i / 64 / 32) % 2 == 0 { 65535 } else { 0 }).collect()).unwrap();
    let mut c = clay(json!([{ "type": "box", "center": [0, -0.1, 0], "size": [0.6, 0.2, 0.6], "blend": 0 }]), 64, Some(0.005));
    let mut store = AlphaStore::default();
    store.keep(Some("checker"), &checker).unwrap();
    ledger::set_alphas(&mut c, store.clone());
    let lookup = |a: &AlphaRef| -> Result<ledger::Hash128, String> { a.hash.or_else(|| a.name.as_ref().and_then(|n| store.names.get(n).copied())).ok_or("no such alpha".into()) };
    let ctx = ResolveCtx { alpha: Some(&lookup), ..ResolveCtx::local() };
    let resolve_one = |v: Value| resolve(&serde_json::from_value::<StrokeSpec>(v).unwrap(), &ctx).unwrap();
    // White squares are the image's top-left and bottom-right quarters:
    // object -x +z and +x -z on the top face (upright on object X).
    let (white, black) = (Vec3::new(-0.05, 0.0, -0.05), Vec3::new(0.05, 0.0, -0.05));
    let paint = resolve_one(json!({ "brush": "paint", "point": [0, 0, 0], "radius": 0.15, "color": [1, 0, 0], "falloff": "flat", "alpha": { "name": "checker", "mode": "stamp" } }));
    assert!(paint.dabs.len() == 1 && paint.params.alpha.is_some());
    super::run(&mut c, &paint).unwrap();
    let p = c.paint.as_ref().unwrap().grid();
    let (cw, cb) = (p.coverage(white), p.coverage(black));
    let mask = resolve_one(json!({ "brush": "mask_paint", "point": [0, 0, 0], "radius": 0.15, "falloff": "flat", "alpha": { "name": "checker" } }));
    let m = crate::mask::paint(&c, None, &mask).unwrap().unwrap();
    let (mw, mb) = (m.grid().value(white), m.grid().value(black));
    eprintln!("checker alpha: paint {cw:.2} / {cb:.2}, mask {mw:.2} / {mb:.2} on white / black");
    assert!(cw > 0.9 && cb < 0.1 && mw > 0.9 && mb < 0.1, "paint {cw} {cb}, mask {mw} {mb}");
}
