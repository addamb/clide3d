//! Stage 5 acceptance for clay from objects: intersect volumes, the
//! volume remesh's blur, primitives as analytic blobs, a carve insert,
//! ledger replay on an imported clay and the conversion's speed.

use glam::{Mat4, Vec3};
use serde_json::json;

use super::*;
use crate::harness::{geom, perf};
use crate::ledger::Grid;
use crate::mesh_asset::tests::icosphere;

fn clay(blobs: serde_json::Value, res: u32) -> Clay {
    serde_json::from_value(json!({ "resolution": res, "blobs": blobs })).unwrap()
}

/// Volume of an exact shape by midpoint sampling over a box (n^3 points).
fn integrate(lo: Vec3, hi: Vec3, n: usize, inside: impl Fn(Vec3) -> bool) -> f64 {
    let step = (hi - lo) / n as f32;
    let mut count = 0usize;
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                count += inside(lo + step * (Vec3::new(x as f32, y as f32, z as f32) + 0.5)) as usize;
            }
        }
    }
    count as f64 * step.element_product() as f64
}

#[test]
fn intersections_have_the_right_volume() {
    // A sphere cut to a box: against the exact shape's volume.
    let c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "blend": 0 }, { "type": "box", "center": [0, 0, 0], "size": [0.8, 0.8, 0.8], "op": "intersect", "blend": 0 }]), 128);
    let v = geom::volume(&c.build_mesh());
    let truth = integrate(Vec3::splat(-0.5), Vec3::splat(0.5), 300, |p| p.length() <= 0.5 && p.abs().max_element() <= 0.4);
    eprintln!("sphere x box: {v:.5} against {truth:.5} ({:+.2}%)", (v / truth - 1.0) * 100.0);
    assert!((v / truth - 1.0).abs() <= 0.02);
    // The lens of two spheres: twice a cap of height r - d/2.
    let (r, d) = (0.5f64, 0.6f64);
    let h = r - d / 2.0;
    let lens = 2.0 * std::f64::consts::PI * h * h * (3.0 * r - h) / 3.0;
    let c = clay(json!([{ "type": "sphere", "center": [-0.3, 0, 0], "radius": 0.5, "blend": 0 }, { "type": "sphere", "center": [0.3, 0, 0], "radius": 0.5, "op": "intersect", "blend": 0 }]), 128);
    let v = geom::volume(&c.build_mesh());
    eprintln!("lens: {v:.5} against {lens:.5} ({:+.2}%)", (v / lens - 1.0) * 100.0);
    assert!((v / lens - 1.0).abs() <= 0.03);
    // A far intersect leaves nothing; a smooth one rounds the cut.
    let far = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }, { "type": "sphere", "center": [5, 0, 0], "radius": 0.5, "op": "intersect" }]), 48);
    assert!(far.build_mesh().indices.is_empty());
}

/// The largest curvature on a mesh, as the largest turn across one of
/// its welded edges (the angle between the two faces' normals): meshes
/// at the same resolution have edges of about a cell.
fn max_curvature(mesh: &crate::mesh::Mesh) -> f32 {
    let pts: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let tris: Vec<[u32; 3]> = mesh.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    let (pts, tris) = crate::mesh_asset::weld(&pts, &tris);
    let mut faces: std::collections::HashMap<(u32, u32), Vec<Vec3>> = std::collections::HashMap::new();
    for t in &tris {
        let n = (pts[t[1] as usize] - pts[t[0] as usize]).cross(pts[t[2] as usize] - pts[t[0] as usize]).normalize_or_zero();
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            faces.entry((a.min(b), a.max(b))).or_default().push(n);
        }
    }
    faces.iter().filter(|(_, n)| n.len() == 2).map(|(_, n)| n[0].dot(n[1]).clamp(-1.0, 1.0).acos().to_degrees()).fold(0.0, f32::max)
}

#[test]
fn volume_blur_softens_and_keeps_the_volume() {
    let progress = crate::progress::Progress::default();
    let boxy = clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [1, 0.8, 0.6], "blend": 0 }]), 96);
    let (plain, _) = remesh(&boxy, 96, 0.0, 0.0, &progress).unwrap();
    let (blurred, _) = remesh(&boxy, 96, 1.0, 0.0, &progress).unwrap();
    let (polished, _) = remesh(&boxy, 96, 0.0, 1.0, &progress).unwrap();
    let (m0, m1, m2) = (plain.build_mesh(), blurred.build_mesh(), polished.build_mesh());
    let (k0, k1, k2) = (max_curvature(&m0), max_curvature(&m1), max_curvature(&m2));
    let (v0, v1, v2) = (geom::volume(&m0), geom::volume(&m1), geom::volume(&m2));
    eprintln!("volume remesh of a box: max curvature {k0:.1} -> blur 1: {k1:.1} ({:+.0}%), polish 1: {k2:.1} ({:+.0}%); volume {v0:.5} -> {v1:.5} ({:+.2}%), {v2:.5} ({:+.2}%)", (k1 / k0 - 1.0) * 100.0, (k2 / k0 - 1.0) * 100.0, (v1 / v0 - 1.0) * 100.0, (v2 / v0 - 1.0) * 100.0);
    assert!(k1 <= 0.7 * k0, "blur 1 cuts max curvature by at least 30%");
    assert!((v1 / v0 - 1.0).abs() <= 0.03 && (v2 / v0 - 1.0).abs() <= 0.03);
    // The remesh itself is the box within a cell and a half (surface nets
    // of a mesh blob cut the box's corners: it has no sharp placement).
    let cell = plain.cell_size().unwrap();
    let h = geom::hausdorff(&m0, &boxy.build_mesh());
    eprintln!("the remesh against the box: {:.3} cell", h / cell);
    assert!(h <= 1.5 * cell);
    assert!(blurred.detail.is_some() && plain.detail.is_none());
}

#[test]
fn primitives_become_analytic_blobs() {
    let at = Mat4::from_scale_rotation_translation(Vec3::new(2.0, 1.0, 0.5), glam::Quat::from_rotation_y(0.3), Vec3::new(1.0, 2.0, 3.0));
    let Some(Form::Box { center, size, .. }) = primitive(Shape::Cube, at) else { panic!("a box") };
    assert!((center - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5 && (size - Vec3::new(2.0, 1.0, 0.5)).length() < 1e-5);
    assert!(matches!(primitive(Shape::Sphere, Mat4::from_scale(Vec3::splat(3.0))), Some(Form::Sphere { radius, .. }) if (radius - 1.5).abs() < 1e-6));
    assert!(matches!(primitive(Shape::Sphere, at), Some(Form::Ellipsoid { .. })));
    assert!(matches!(primitive(Shape::Cylinder, Mat4::IDENTITY), Some(Form::Lathe { .. })));
    assert!(primitive(Shape::Cylinder, at).is_none(), "an oval cylinder is a mesh");
    assert!(primitive(Shape::Torus, Mat4::from_cols_array(&[1.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0])).is_none(), "sheared");
    // Each primitive's clay is its mesh within half a cell (the faceted
    // primitive meshes sit inside their round shapes by up to 0.25%).
    for shape in [Shape::Cube, Shape::Sphere, Shape::Cylinder, Shape::Cone, Shape::Torus] {
        let form = primitive(shape, Mat4::IDENTITY).unwrap();
        let mut c = Clay { resolution: 128, ..Clay::default() };
        c.blobs.push(Blob::of(form, Op::Add, [0.8; 3]));
        let mesh = c.build_mesh();
        let cell = c.cell_size().unwrap();
        let h = geom::hausdorff(&mesh, &crate::mesh::Mesh::build(shape));
        eprintln!("{shape:?} as clay: hausdorff {:.3} cell", h / cell);
        assert!(h <= 0.75 * cell, "{shape:?}: {} cells", h / cell);
    }
}

#[test]
fn a_carve_insert_makes_a_hole() {
    let mut scene = Scene::default();
    let target = scene.add(Shape::Clay);
    scene.get_mut(target).unwrap().clay = Some(clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [1, 1, 1], "blend": 0 }]), 96));
    let tool = scene.add(Shape::Cylinder);
    let o = scene.get_mut(tool).unwrap();
    (o.scale, o.translation) = (Vec3::new(0.3, 0.8, 0.3), Vec3::new(3.0, 0.0, 0.0));
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "target": target, "part": { "object": tool, "op": "carve" }, "at": [0.0, 0.6, 0.0] })).unwrap();
    assert_eq!(r["inserted"], 1, "{r}");
    meshes.sync(&scene);
    let mesh = meshes.get(scene.get(target).unwrap()).unwrap();
    // A ray down the hole's middle passes the top (y 0.5) and meets the
    // hole's floor, 0.4 down (half the cylinder sinks).
    let t = mesh.ray_hit(Vec3::new(0.0, 2.0, 0.0), Vec3::NEG_Y).unwrap();
    eprintln!("carve insert: the ray meets y = {:.4}", 2.0 - t);
    assert!(((2.0 - t) - 0.1).abs() < 0.02, "floor at {}", 2.0 - t);
    // Beside the hole the top is where it was.
    let t = mesh.ray_hit(Vec3::new(0.35, 2.0, 0.0), Vec3::NEG_Y).unwrap();
    assert!(((2.0 - t) - 0.5).abs() < 0.01);
    // Removing the group takes the blob (and its asset) out again.
    let tag = r["group"].as_str().unwrap().to_string();
    crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "action": "remove", "group": tag })).unwrap();
    assert_eq!(scene.get(target).unwrap().clay.as_ref().unwrap().blobs.len(), 1);
}

/// InsertMesh of a mesh object at several spots: one asset (welded and
/// decimated once), a blob per spot placing it; an unevenly scaled
/// target takes it at one spot at a time.
#[test]
fn a_mesh_insert_makes_one_asset_for_every_spot() {
    let mut scene = Scene::default();
    let target = scene.add(Shape::Clay);
    scene.get_mut(target).unwrap().clay = Some(clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [1, 1, 1], "blend": 0 }]), 96));
    let (p, t) = icosphere(3, 0.5);
    let ball = scene.add(Shape::Mesh);
    let o = scene.get_mut(ball).unwrap();
    (o.mesh, o.scale, o.translation) = (Some(crate::mesh_asset::MeshAsset::new(p, t).unwrap()), Vec3::splat(0.2), Vec3::new(3.0, 0.0, 0.0));
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    let spots = json!([[-0.3, 0.6, -0.3], [0.3, 0.6, 0.0], [0.0, 0.6, 0.3]]);
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "target": target, "part": { "object": ball }, "points": spots })).unwrap();
    assert_eq!(r["inserted"], 3, "{r}");
    let c = scene.get(target).unwrap().clay.clone().unwrap();
    assert_eq!(c.assets.len(), 1, "one asset for every spot");
    let centers: Vec<Vec3> = c.blobs[1..].iter().map(|b| match b.form {
        Form::Mesh { center, scale, .. } => {
            assert!((scale - 1.0).abs() < 1e-4, "the size is in the asset: {scale}");
            center
        }
        _ => panic!("a mesh blob"),
    }).collect();
    // On the top where each spot found it, apart.
    assert!(centers.iter().all(|c| (c.y - 0.5).abs() < 1e-3) && centers[0].distance(centers[1]) > 0.2 && centers[1].distance(centers[2]) > 0.2, "{centers:?}");
    // A bump of radius 0.1 on the top at each spot.
    meshes.sync(&scene);
    let mesh = meshes.get(scene.get(target).unwrap()).unwrap();
    let top = 2.0 - mesh.ray_hit(Vec3::new(centers[1].x, 2.0, centers[1].z), Vec3::NEG_Y).unwrap();
    eprintln!("mesh insert: the bump's top at y = {top:.4}");
    assert!((top - 0.6).abs() < 0.02, "{top}");
    let tag = r["group"].as_str().unwrap().to_string();
    crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "action": "remove", "group": tag })).unwrap();
    let c = scene.get(target).unwrap().clay.as_ref().unwrap();
    assert!(c.blobs.len() == 1 && c.assets.is_empty());
    // Into an unevenly scaled clay a mesh goes one spot at a time.
    scene.get_mut(target).unwrap().scale = Vec3::new(1.0, 2.0, 1.0);
    meshes.sync(&scene);
    let many = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "target": target, "part": { "object": ball }, "points": [[0.0, 1.1, 0.0], [0.3, 1.1, 0.0]] }));
    assert!(many.as_ref().is_err_and(|e| e.contains("unevenly")), "{many:?}");
    let one = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "target": target, "part": { "object": ball }, "at": [0.0, 1.1, 0.0] })).unwrap();
    assert_eq!(one["inserted"], 1, "{one}");
}

#[test]
fn ledger_replay_on_an_imported_clay_is_deterministic() {
    let (p, t) = icosphere(4, 1.0);
    let (blob, asset, _) = mesh_blob(&p, &t, Mat4::IDENTITY, Op::Add, [0.8; 3]).unwrap();
    let mut base = Clay { resolution: 48, detail_cell: Some(0.02), ..Clay::default() };
    base.blobs.push(blob);
    base.assets.insert(asset.hash().into(), asset);
    let strokes: Vec<serde_json::Value> = (0..12).map(|i| {
        let a = i as f32 * 0.5;
        stroke(i, [a.cos() * 0.8, 0.6, a.sin() * 0.8])
    }).collect();
    replays_the_same(&base, &strokes);
}

/// The i-th of a cycle of inflate, standard and smooth strokes at `q`.
fn stroke(i: usize, q: [f32; 3]) -> serde_json::Value {
    match i % 3 {
        0 => json!({ "brush": "inflate", "point": q, "radius": 0.15, "strength": 0.4 }),
        1 => json!({ "brush": "standard", "points": [q, [q[0] * 0.9, q[1] + 0.1, q[2] * 0.9]], "radius": 0.08, "strength": 0.5 }),
        _ => json!({ "brush": "smooth", "point": q, "radius": 0.12, "strength": 0.5 }),
    }
}

/// `strokes` on `base` give the same grid on 1 and 12 threads, replay
/// from the ledger to it bit for bit, and still do after a save and load
/// (the assets come back with the clay). Returns the sculpted clay.
fn replays_the_same(base: &Clay, strokes: &[serde_json::Value]) -> Clay {
    let run = |threads: usize| crate::clay::with_threads(threads, || {
        let mut c = base.clone();
        let meta = crate::ledger::Meta::agent("sculpt", None);
        for (spec, r) in crate::sculpt::prepare(strokes, None, &crate::brush::resolve::ResolveCtx::local()).unwrap() {
            let done = crate::ledger::commit::<Detail>(&c, &crate::ledger::ledger(&c), &meta, &spec, &r, false, None).unwrap();
            crate::ledger::install(&mut c, done.ledger, done.grid);
        }
        c
    });
    let (one, many) = (run(1), run(12));
    assert!(one.detail.is_some(), "the strokes left a grid");
    assert_eq!(Detail::hash(one.detail.as_ref()), Detail::hash(many.detail.as_ref()), "1 and 12 threads");
    let l = crate::ledger::ledger::<Detail>(&one);
    let again = crate::ledger::replay(&l, &one, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(again.as_ref()), Detail::hash(one.detail.as_ref()), "replayed from the ledger");
    // Through a save and load the asset comes back and the ledger replays.
    let text = serde_json::to_string(&one).unwrap();
    let back: Clay = serde_json::from_str(&text).unwrap();
    assert_eq!(back.assets, one.assets);
    let l = crate::ledger::ledger::<Detail>(&back);
    let replayed = crate::ledger::replay(&l, &back, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(replayed.as_ref()), Detail::hash(one.detail.as_ref()));
    one
}

/// A shell of an array of a mesh blob, end to end: a hollow ball at each
/// copy (from the clay's own asset), strokes on the copies that replay
/// the same on any thread count, from the ledger and after a save, and
/// the asset kept through `sanitize`.
#[test]
fn a_shell_of_an_array_of_a_mesh_builds_and_replays() {
    let (p, t) = icosphere(3, 0.3);
    let (blob, asset, _) = mesh_blob(&p, &t, Mat4::IDENTITY, Op::Add, [0.8; 3]).unwrap();
    let step = Vec3::new(0.8, 0.0, 0.1);
    let array = Form::Array { source: Box::new(blob.form.clone()), count: 4, step, radius: 0.0, center: Vec3::ZERO, axis: Vec3::Y, points: Vec::new(), orient: Default::default() };
    let mut base = Clay { resolution: 96, detail_cell: Some(0.02), ..Clay::default() };
    base.blobs.push(Blob { form: Form::Shell { source: Box::new(array), thickness: 0.05 }, blend: 0.0, ..blob });
    base.assets.insert(asset.hash().into(), asset.clone());
    base.sanitize();
    assert_eq!((base.blobs.len(), base.assets.len()), (1, 1), "sanitize keeps the nested asset");
    let mesh = base.build_mesh();
    assert!(!mesh.indices.is_empty());
    // Every vertex lies on a copy's outer wall (r 0.3) or inner one
    // (r 0.25), and every copy has both.
    let centers: Vec<Vec3> = (0..4).map(|i| step * i as f32).collect();
    let mut walls = [[0usize; 2]; 4];
    for v in &mesh.vertices {
        let (i, r) = centers.iter().enumerate().map(|(i, c)| (i, Vec3::from(v.pos).distance(*c))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!((0.22..0.32).contains(&r), "a vertex {r} from copy {i}");
        walls[i][usize::from(r > 0.275)] += 1;
    }
    eprintln!("shell of an array of a mesh: {} triangles, inner and outer wall vertices per copy {walls:?}", mesh.indices.len() / 3);
    assert!(walls.iter().all(|w| w[0] > 100 && w[1] > 100), "{walls:?}");
    // Strokes on top of each copy.
    let strokes: Vec<serde_json::Value> = (0..8).map(|i| {
        let c = centers[i % 4];
        stroke(i, [c.x + 0.05 * (i / 4) as f32, 0.3, c.z])
    }).collect();
    let sculpted = replays_the_same(&base, &strokes);
    assert_ne!(Detail::hash(sculpted.detail.as_ref()), Detail::hash(None), "the strokes changed the grid");
    assert!(!sculpted.build_mesh().indices.is_empty());
}

/// A closed torus of `u` x `v` quads (2uv triangles), wound outward.
fn torus(u: u32, v: u32, big: f32, small: f32) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mut p = Vec::new();
    for i in 0..u {
        let a = std::f32::consts::TAU * i as f32 / u as f32;
        for j in 0..v {
            let b = std::f32::consts::TAU * j as f32 / v as f32;
            let r = big + small * b.cos();
            p.push(Vec3::new(r * a.cos(), small * b.sin(), r * a.sin()));
        }
    }
    let at = |i: u32, j: u32| (i % u) * v + j % v;
    let mut t = Vec::new();
    for i in 0..u {
        for j in 0..v {
            t.push([at(i, j), at(i, j + 1), at(i + 1, j + 1)]);
            t.push([at(i, j), at(i + 1, j + 1), at(i + 1, j)]);
        }
    }
    (p, t)
}

/// 200k triangles to a clay at res 128 within 3 s (asset, field, mesh).
#[test]
fn a_dense_mesh_becomes_clay_quickly() {
    let (p, t) = torus(500, 200, 0.4, 0.15);
    assert_eq!(t.len(), 200_000);
    assert!(crate::mesh_asset::watertight(&p, &t));
    let (tris, ms) = perf::time(1, || {
        let (blob, asset, _) = mesh_blob(&p, &t, Mat4::IDENTITY, Op::Add, [0.8; 3]).unwrap();
        assert!(matches!(blob.form, Form::Mesh { sign: Sign::Closed, .. }));
        let mut c = Clay { resolution: 128, ..Clay::default() };
        c.blobs.push(blob);
        c.assets.insert(asset.hash().into(), asset);
        c.build_mesh().indices.len() / 3
    });
    eprintln!("200k-triangle torus to clay at res 128: {ms:.0} ms ({tris} triangles)");
    perf::check("to_clay_200k_res128", ms, 3000.0, json!({ "triangles": 200_000, "clay_triangles": tris }));
}

/// Primitives made clay render like the primitives (`CLIDE_GPU=1`):
/// at least 40 dB PSNR (the faceted sphere 38).
#[test]
fn primitive_clay_renders_like_the_primitive() {
    use crate::harness::image;
    if !image::gpu_enabled() {
        eprintln!("skipped: image checks run with CLIDE_GPU=1");
        return;
    }
    for shape in [Shape::Cube, Shape::Sphere, Shape::Cylinder] {
        let mut scene = Scene::default();
        let id = scene.add(shape);
        scene.get_mut(id).unwrap().translation = Vec3::ZERO;
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);
        let before = image::render(&scene, &meshes, image::View::FRONT).unwrap();
        let color = scene.get(id).unwrap().color;
        let c = object_blobs(&scene, &meshes, id, Mat4::IDENTITY, Op::Add, tint(color, color)).unwrap();
        let o = scene.get_mut(id).unwrap();
        o.shape = Shape::Clay;
        o.clay = Some(Clay { resolution: 128, blobs: c.blobs, ..Clay::default() });
        meshes.sync(&scene);
        let after = image::render(&scene, &meshes, image::View::FRONT).unwrap();
        let mse = before.pixels.iter().zip(&after.pixels).flat_map(|(p, q)| [p.r().abs_diff(q.r()), p.g().abs_diff(q.g()), p.b().abs_diff(q.b())]).map(|d| (d as f64).powi(2)).sum::<f64>() / (before.pixels.len() * 3) as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-12)).log10();
        eprintln!("{shape:?} primitive vs its clay: PSNR {psnr:.1} dB");
        // The primitive sphere is 32 faceted segments: the true sphere its
        // clay is differs from it by 39 dB at any clay resolution (38.8 at
        // res 256), so it is held to 38.
        let bound = if shape == Shape::Sphere { 38.0 } else { 40.0 };
        assert!(psnr >= bound, "{shape:?}: {psnr:.1} dB");
    }
}

/// The DynaMesh polish (curvature flow on the remeshed field) gives the
/// grid it gave before its step moved into `sculpt::flow`, bit for bit.
#[test]
fn to_clay_polish_unchanged() {
    let progress = crate::progress::Progress::default();
    let boxy = clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [1, 0.8, 0.6], "blend": 0 }, { "type": "sphere", "center": [0.4, 0.3, 0.2], "radius": 0.25 }]), 64);
    let (polished, _) = remesh(&boxy, 64, 0.5, 0.7, &progress).unwrap();
    let h = Detail::hash(polished.detail.as_ref());
    eprintln!("dynamesh polish hash {h}");
    assert_eq!(h.to_string(), "0c4687b91789289d8593b76a65ced678");
}
