//! Stage 4a acceptance: dicing with known answers, watertightness,
//! budgets, and pictures against the fully subdivided surface and the
//! clay.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Vec3};

use super::*;
use crate::film::look::{Device, Look};
use crate::film::{Camera, Item, Settings};
use crate::material::Material;
use crate::poly::project::tests::{cube_sphere, noise_sphere, run};
use crate::poly::project::{Args, Mode, REACH_EDGES, mean_edge};

fn eyes(cam: &Camera, height: usize) -> Vec<Eye> {
    vec![Eye::new(cam, height, height)]
}

fn opts() -> Options {
    Options::default()
}

/// Every edge of the mesh used exactly twice, once each way (closed and
/// consistently wound), as indices; and the number of distinct edges.
fn closed(m: &Mesh) -> Result<usize, String> {
    let mut uses: HashMap<(u32, u32), (u32, i32)> = HashMap::new();
    for t in m.indices.chunks_exact(3) {
        for e in 0..3 {
            let (a, b) = (t[e], t[(e + 1) % 3]);
            let u = uses.entry((a.min(b), a.max(b))).or_default();
            u.0 += 1;
            u.1 += if a < b { 1 } else { -1 };
        }
    }
    if let Some((e, u)) = uses.iter().find(|(_, u)| **u != (2, 0)) {
        return Err(format!("edge {e:?} used {} times, wound {} more one way", u.0, u.1));
    }
    Ok(uses.len())
}

/// A noise-bumped sphere's cage projected over `levels` levels, and the
/// clay.
fn projected(levels: u8) -> (crate::clay::Clay, Poly) {
    let c = noise_sphere();
    let p = cube_sphere(12, 0.5);
    let (out, _, _) = run(&p, &c, Args { levels, mode: Mode::Vector, cone: 45.0, reach: REACH_EDGES * mean_edge(&p) });
    (c, out)
}

fn persp(eye: Vec3) -> Camera {
    Camera { eye, target: Vec3::ZERO, up: Vec3::Y, fov_deg: 35.0, ortho: None }
}

/// The plan's `film_subdiv_silhouette`: a cube cage with no creases
/// dices to its Catmull-Clark limit: the middle of each face sits at
/// 68/81 of the half-size (3a's known answer), within half a pixel.
#[test]
fn film_subdiv_silhouette() {
    let cube = crate::poly::tests::cube();
    let cam = Camera { eye: Vec3::new(0.0, 0.0, 5.0), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 30.0, ortho: Some(1.5) };
    let height = 400;
    let pixel = 3.0 / height as f32;
    let d = dice(&cube, Mat4::IDENTITY, &eyes(&cam, height), &opts(), usize::MAX).unwrap().0;
    let extent = d.mesh.vertices.iter().map(|v| v.pos[0]).fold(f32::MIN, f32::max);
    eprintln!("cube: levels {:?}, {} triangles, x extent {extent} against {} ({:.3} px)", d.levels, d.triangles, 68.0 / 81.0, (extent - 68.0 / 81.0).abs() / pixel);
    assert_eq!(d.levels, (6, 6), "an edge of 2 at 0.0075 a pixel wants more than 6 levels");
    assert!((extent - 68.0 / 81.0).abs() <= 0.5 * pixel, "{extent}");
    closed(&d.mesh).unwrap();
    // The picture's silhouette: its half-width in pixels along the middle
    // row, from coverage against the background, within a pixel.
    let item = Item { mesh: Arc::new(d.mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: Material::default().surface(), material: Material::default() };
    let s = Settings { width: 200, height: 200, samples: 16, bounces: 1, time_limit: Duration::from_secs(300), floor: false, exposure: 1.0, denoise: false };
    let cam200 = Camera { ortho: Some(1.5), ..cam };
    let img = crate::film::render(vec![item], &[], &cam200, s, &Default::default());
    let row = &img.pixels[100 * 200..101 * 200];
    let bg = row[0];
    let lit = row.iter().filter(|p| p.iter().zip(bg.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap() > 24).count();
    let want = 2.0 * (68.0 / 81.0) / (3.0 / 200.0);
    eprintln!("silhouette row: {lit} pixels against {want:.1}");
    assert!((lit as f32 - want).abs() <= 2.0, "{lit} vs {want}");
}

/// Watertight dicing across level boundaries: with the camera close to
/// one side the faces want different levels, and every edge is still
/// used exactly twice, once each way, on closed cages (the projected
/// sphere, and n-gons, triangles and creases).
#[test]
fn dicing_is_watertight_across_levels() {
    let (_, p) = projected(3);
    let cam = persp(Vec3::new(0.0, 0.0, 0.7));
    let d = dice(&p, Mat4::IDENTITY, &eyes(&cam, 400), &opts(), usize::MAX).unwrap().0;
    eprintln!("sphere: levels {:?}, {} triangles", d.levels, d.triangles);
    assert!(d.levels.0 < d.levels.1, "the levels should differ: {:?}", d.levels);
    let edges = closed(&d.mesh).unwrap();
    // Euler: a closed sphere has V - E + F = 2.
    assert_eq!(d.mesh.vertices.len() as i64 - edges as i64 + d.triangles as i64, 2);
    let mixed = crate::poly::export::tests::mixed();
    let cam = persp(Vec3::new(-0.3, 0.0, 1.5));
    let d = dice(&mixed, Mat4::IDENTITY, &eyes(&cam, 60), &opts(), usize::MAX).unwrap().0;
    eprintln!("mixed: levels {:?}, {} triangles", d.levels, d.triangles);
    assert!(d.levels.0 < d.levels.1, "{:?}", d.levels);
    closed(&d.mesh).unwrap();
}

/// Uniform levels give the level's own triangles: every face at level M
/// dices to `Mesh::poly` of level M, point for point and triangle for
/// triangle.
#[test]
fn uniform_dicing_is_the_level() {
    let (_, p) = projected(2);
    let cam = persp(Vec3::new(0.0, 0.0, 3.0));
    let o = Options { max_level: 2, ..opts() };
    let d = dice(&p, Mat4::IDENTITY, &eyes(&cam, 1600), &o, usize::MAX).unwrap().0;
    assert_eq!(d.levels, (2, 2));
    let mut q = p.clone();
    q.level = 2;
    let m = Mesh::poly(&q);
    let tri = |mesh: &Mesh| {
        let mut t: Vec<[[u32; 3]; 3]> = mesh.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| mesh.vertices[t[k] as usize].pos.map(f32::to_bits))).collect();
        t.sort_unstable();
        t
    };
    assert_eq!(tri(&d.mesh), tri(&m), "the same triangles as the level");
}

/// The plan's `dicing_is_view_adaptive`: twice the picture height makes
/// about four times the triangles, ten times the distance far fewer, a
/// tight budget is kept (each face coarser), and one below level 1 is
/// refused naming the arguments.
#[test]
fn dicing_is_view_adaptive() {
    let (_, p) = projected(3);
    let o = Options { max_level: 5, ..opts() };
    let n = |eye: Vec3, h: usize, budget: usize| dice(&p, Mat4::IDENTITY, &eyes(&persp(eye), h), &o, budget).map(|d| d.0);
    let lost = |eye: Vec3, h: usize, budget: usize| dice(&p, Mat4::IDENTITY, &eyes(&persp(eye), h), &o, budget).unwrap().1;
    let near = Vec3::new(0.0, 0.3, 2.4);
    let (a, b) = (n(near, 200, usize::MAX).unwrap(), n(near, 400, usize::MAX).unwrap());
    let far = n(near * 10.0, 400, usize::MAX).unwrap();
    let ratio = b.triangles as f32 / a.triangles as f32;
    eprintln!("200 px {} ({:?}), 400 px {} ({:?}): x{ratio:.2}; 10x farther {} ({:?})", a.triangles, a.levels, b.triangles, b.levels, far.triangles, far.levels);
    assert!((2.5..=5.5).contains(&ratio), "{ratio}");
    assert!(far.triangles * 8 < b.triangles, "{} vs {}", far.triangles, b.triangles);
    let budget = b.triangles / 3;
    let tight = n(near, 400, budget).unwrap();
    let coarser = lost(near, 400, budget);
    eprintln!("budget {budget}: {} triangles, {coarser} levels coarser", tight.triangles);
    assert!(tight.triangles <= budget && coarser >= 1);
    let e = n(near, 400, 100).err().unwrap();
    assert!(e.contains("max_diced"), "{e}");
    // The evaluation bound, from counts: a cage whose level 1 passes it.
    let big = cube_sphere(600, 0.5);
    assert!(big.faces_at(1) > MAX_EVALUATED);
    let e = dice(&big, Mat4::IDENTITY, &eyes(&persp(near), 64), &o, usize::MAX).err().unwrap();
    assert!(e.contains("evaluation bound"), "{e}");
}

/// The same mesh on 1 and 12 threads, bit for bit (§0.2 rule 4).
#[test]
fn dicing_is_thread_independent() {
    let (_, p) = projected(3);
    let cam = persp(Vec3::new(0.1, 0.2, 0.9));
    let hash = |m: &Mesh| {
        let mut bytes = Vec::new();
        for v in &m.vertices {
            bytes.extend(v.pos.iter().chain(&v.normal).flat_map(|x| x.to_le_bytes()));
        }
        bytes.extend(m.indices.iter().flat_map(|i| i.to_le_bytes()));
        xxhash_rust::xxh3::xxh3_64(&bytes)
    };
    let one = crate::clay::with_threads(1, || hash(&dice(&p, Mat4::IDENTITY, &eyes(&cam, 300), &opts(), usize::MAX).unwrap().0.mesh));
    let many = crate::clay::with_threads(12, || hash(&dice(&p, Mat4::IDENTITY, &eyes(&cam, 300), &opts(), usize::MAX).unwrap().0.mesh));
    assert_eq!(one, many);
}

/// `displacement: false` dices the smooth surface: the projected Poly
/// with its displacements dropped, the same triangles.
#[test]
fn displacement_off_is_the_smooth_surface() {
    let (_, p) = projected(2);
    let cam = persp(Vec3::new(0.0, 0.0, 3.0));
    let off = dice(&p, Mat4::IDENTITY, &eyes(&cam, 400), &Options { displacement: false, ..opts() }, usize::MAX).unwrap().0;
    let mut smooth = p.clone();
    smooth.levels.iter_mut().for_each(|l| l.displacement = None);
    let plain = dice(&smooth, Mat4::IDENTITY, &eyes(&cam, 400), &opts(), usize::MAX).unwrap().0;
    assert!(off.mesh.vertices.iter().zip(&plain.mesh.vertices).all(|(a, b)| a.pos == b.pos) && off.mesh.indices == plain.mesh.indices);
    let on = dice(&p, Mat4::IDENTITY, &eyes(&cam, 400), &opts(), usize::MAX).unwrap().0;
    assert!(on.mesh.vertices.iter().zip(&off.mesh.vertices).any(|(a, b)| a.pos != b.pos), "the displacement shows");
}

/// Pixel differences: (mean, 99th percentile) of each pixel's largest
/// channel difference, in 255ths.
fn compare(a: &[[u8; 3]], b: &[[u8; 3]]) -> (f32, f32) {
    let mut d: Vec<u8> = a.iter().zip(b).map(|(x, y)| (0..3).map(|k| (x[k] as i16 - y[k] as i16).unsigned_abs() as u8).max().unwrap()).collect();
    let mean = d.iter().map(|&x| x as f32).sum::<f32>() / d.len() as f32;
    d.sort_unstable();
    (mean, d[(d.len() * 99) / 100] as f32)
}

fn picture(items: Vec<Item>, cam: &Camera, device: Device) -> Vec<[u8; 3]> {
    let mut look = Look::default();
    look.device = device;
    let s = Settings { width: 160, height: 160, samples: 256, bounces: 2, time_limit: Duration::from_secs(600), floor: false, exposure: 1.0, denoise: true };
    let img = crate::film::render_views_with_look(&crate::film::Meshes::new(items), &[], std::slice::from_ref(cam), s, Arc::new(look), &Default::default()).pop().unwrap();
    img.traced.assert_honours(device);
    img.pixels
}

fn item(mesh: Mesh) -> Item {
    let m = Material::default();
    Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [0.8, 0.7, 0.6], surface: m.surface(), material: m }
}

/// The plan's image tests: a displaced Poly diced adaptively (levels
/// differ across it) renders as its fully subdivided level does, and as
/// its clay does (mean ≤ 2/255, 99th percentile ≤ 8/255). On the GPU too
/// when one is asked for (CLIDE_GPU=1).
#[test]
fn film_displacement_matches_subdivided_and_clay() {
    let (c, p) = projected(3);
    let cam = persp(Vec3::new(0.25, 0.2, 1.3));
    let d = dice(&p, Mat4::IDENTITY, &eyes(&cam, 160), &opts(), usize::MAX).unwrap().0;
    eprintln!("diced: levels {:?}, {} triangles", d.levels, d.triangles);
    assert!(d.levels.0 < d.levels.1, "{:?}", d.levels);
    let mut full = p.clone();
    full.set_levels(d.evaluated).unwrap();
    full.level = d.evaluated;
    let uniform = Mesh::poly(&full);
    let composed = crate::layers::composed_clay(&c);
    // Shaded as the Poly is: the object's material and tint.
    let mut clay_mesh = composed.build_fine(&[], "").unwrap();
    clay_mesh.vertices.iter_mut().for_each(|v| {
        v.surface = crate::mesh::OBJECT_SURFACE;
        v.color = [1.0; 3];
        v.material = 0;
    });
    clay_mesh.materials.clear();
    let diced = picture(vec![item(d.mesh.clone())], &cam, Device::Cpu);
    let (m1, p1) = compare(&diced, &picture(vec![item(uniform.clone())], &cam, Device::Cpu));
    let (m2, p2) = compare(&diced, &picture(vec![item(clay_mesh.clone())], &cam, Device::Cpu));
    let undisplaced = dice(&p, Mat4::IDENTITY, &eyes(&cam, 160), &Options { displacement: false, ..opts() }, usize::MAX).unwrap().0;
    let (m3, p3) = compare(&diced, &picture(vec![item(undisplaced.mesh)], &cam, Device::Cpu));
    eprintln!("diced vs level {} ({} triangles): mean {m1:.2}, p99 {p1}; vs clay ({} triangles): mean {m2:.2}, p99 {p2}; vs no displacement: mean {m3:.2}, p99 {p3}", d.evaluated, uniform.indices.len() / 3, clay_mesh.indices.len() / 3);
    assert!(m1 <= 2.0 && p1 <= 8.0, "against the subdivided level: {m1} {p1}");
    assert!(m2 <= 2.0 && p2 <= 8.0, "against the clay: {m2} {p2}");
    assert!(p3 > p1, "the displacement shows in the picture");
    if std::env::var("CLIDE_GPU").is_ok_and(|v| v == "1") && crate::film::gpu::get().is_some() {
        let (mg, pg) = compare(&diced, &picture(vec![item(d.mesh)], &cam, Device::Gpu));
        eprintln!("GPU vs CPU: mean {mg:.2}, p99 {pg}");
        assert!(mg <= 2.0 && pg <= 8.0, "GPU against CPU: {mg} {pg}");
    }
}

/// §6: a 5k-cage head projected over 4 levels, diced for a 1080p
/// close-up, in ≤ 2 s and ≤ 8M triangles (CLIDE_PERF=1 holds them).
#[test]
#[ignore]
fn film_dice_perf() {
    use crate::poly::project::tests::{retopo_cage, sculpt};
    let mut c = crate::poly::retopo::tests::clay(crate::poly::retopo::tests::head(), 160);
    c.detail_cell = Some(0.006);
    sculpt(&mut c, &[serde_json::json!({ "brush": "noise", "point": [0.0, 0.1, 0.35], "radius": 0.3, "strength": 1.0, "scale": 0.05, "pattern": "bumps" })]);
    let cage = retopo_cage(&c, 5000);
    let (p, _, _) = run(&cage, &c, Args { levels: 4, mode: Mode::Vector, cone: 45.0, reach: REACH_EDGES * mean_edge(&cage) });
    let (lo, hi) = p.cage.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), q| (a.min(q), b.max(q)));
    let (mid, size) = ((lo + hi) * 0.5, (hi - lo).max_element());
    // The face filling the frame: 1920 x 1080.
    let cam = Camera { eye: mid + Vec3::new(0.0, 0.0, 1.4 * size), target: mid, up: Vec3::Y, fov_deg: 30.0, ortho: None };
    for (name, cam) in [("full head", cam), ("close-up", Camera { eye: mid + Vec3::new(0.0, 0.0, 0.8 * size), ..cam })] {
        let t = std::time::Instant::now();
        let d = dice(&p, Mat4::IDENTITY, &eyes(&cam, 1080), &opts(), DEFAULT_MAX_TRIANGLES).unwrap().0;
        let s = t.elapsed().as_secs_f32();
        eprintln!("{name}: {} cage faces, levels {:?}, {} triangles, {s:.2} s", p.faces(), d.levels, d.triangles);
        if std::env::var("CLIDE_PERF").is_ok_and(|v| v == "1") {
            assert!(s <= 2.0 && d.triangles <= 8_000_000, "{s} s, {} triangles", d.triangles);
        }
    }
}

#[test]
fn options_are_bounded() {
    let o = |v: serde_json::Value| Options::parse(&v);
    assert_eq!(o(serde_json::json!({})).unwrap(), Options::default());
    assert_eq!(o(serde_json::json!({ "subdiv_max_level": 0 })).unwrap().max_level, 0);
    for bad in [
        serde_json::json!({ "subdiv_max_level": 7 }),
        serde_json::json!({ "subdiv_max_level": -1 }),
        serde_json::json!({ "subdiv_max_level": 2.5 }),
        serde_json::json!({ "dice_rate": 0.1 }),
        serde_json::json!({ "dice_rate": 17 }),
        serde_json::json!({ "max_diced": 5 }),
        serde_json::json!({ "max_diced": 25_000_000 }),
        serde_json::json!({ "displacement": "yes" }),
    ] {
        assert!(o(bad.clone()).is_err(), "{bad}");
    }
}

/// Faces seen from behind dice coarser (as off-screen ones), the same
/// under a reflecting placement (a mirror copy), and the surface stays
/// closed.
#[test]
fn back_faces_dice_coarser() {
    let (_, p) = projected(2);
    let cam = persp(Vec3::new(0.0, 0.0, 2.0));
    let d = dice(&p, Mat4::IDENTITY, &eyes(&cam, 400), &opts(), usize::MAX).unwrap().0;
    let front_of = |m: &Mesh, sign: f32| m.indices.chunks_exact(3).filter(|t| t.iter().all(|&i| sign * m.vertices[i as usize].pos[2] > 0.2)).count();
    let (front, back) = (front_of(&d.mesh, 1.0), front_of(&d.mesh, -1.0));
    eprintln!("levels {:?}: {front} triangles in front, {back} behind", d.levels);
    assert!(front > 8 * back, "{front} vs {back}");
    closed(&d.mesh).unwrap();
    let mirror = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    let m = dice(&p, mirror, &eyes(&cam, 400), &opts(), usize::MAX).unwrap().0;
    assert_eq!((front_of(&m.mesh, 1.0), front_of(&m.mesh, -1.0)), (front, back), "a mirror image dices the same");
}

/// Two Polys share `max_triangles`: both are made coarser together and
/// both render (the second is not refused for the first's triangles).
#[test]
fn polys_share_the_budget() {
    let (_, p) = projected(2);
    let poly = Arc::new(p);
    let drawn = Mesh::poly(&poly);
    let mut items: Vec<Item> = [-0.6f32, 0.6].iter().map(|x| Item { world: Mat4::from_translation(Vec3::new(*x, 0.0, 0.0)), ..item(drawn.clone()) }).collect();
    let sources: Vec<Source> = (0..2).map(|i| Source { item: i, name: format!("p{i}"), poly: poly.clone() }).collect();
    let cam = persp(Vec3::new(0.0, 0.0, 2.5));
    let free = subdivide(&mut items, &sources, &[cam], 400, 400, &opts(), &Default::default()).unwrap().1;
    let total = |r: &[serde_json::Value]| r.iter().map(|x| x["triangles"].as_u64().unwrap()).sum::<u64>();
    let mut items2: Vec<Item> = [-0.6f32, 0.6].iter().map(|x| Item { world: Mat4::from_translation(Vec3::new(*x, 0.0, 0.0)), ..item(drawn.clone()) }).collect();
    let budget = (total(&free) / 2) as usize;
    let tight = subdivide(&mut items2, &sources, &[cam], 400, 400, &Options { max_triangles: budget, ..opts() }, &Default::default()).unwrap().1;
    eprintln!("free {} triangles, budget {budget}: {:?}", total(&free), tight);
    assert!(total(&tight) <= budget as u64);
    assert!(tight.iter().all(|r| r["triangles"].as_u64().unwrap() > 0 && r["note"].as_str().is_some_and(|n| n.contains("coarser"))), "{tight:?}");
}

/// 4a review fix 1: the frustum is the frame's, width and height both. A
/// ball near the left edge of a 3:1 frame (inside it, outside the old
/// cone built from the vertical half-angle) dices about as finely as the
/// same ball in the middle; it was 3 levels coarser.
#[test]
fn wide_frames_dice_their_edges() {
    let (_, p) = projected(2);
    let o = Options { max_level: 6, ..opts() };
    let cam = persp(Vec3::new(0.0, 0.0, 3.0));
    let wide = |x: f32| {
        let e = vec![Eye::new(&Camera { target: Vec3::new(0.0, 0.0, 0.0), ..cam }, 1200, 400)];
        dice(&p, Mat4::from_translation(Vec3::new(x, 0.0, 0.0)), &e, &o, usize::MAX).unwrap().0.levels
    };
    let (middle, side) = (wide(0.0), wide(-2.0));
    eprintln!("3:1 frame: middle levels {middle:?}, left edge {side:?}");
    assert!(side.1 + 1 >= middle.1, "{side:?} against {middle:?}");
    // Off the frame it is coarser (8 times the pixel: 3 levels).
    let off = wide(-4.5);
    assert!(off.1 + 2 <= middle.1, "{off:?}");
    // The Eye itself: on the frame's horizontal edge, not above it.
    let e = Eye::new(&cam, 1200, 400);
    let pixel = |p: Vec3| e.footprint(p) / p.distance(cam.eye);
    assert_eq!(pixel(Vec3::new(-2.0, 0.0, 0.0)), pixel(Vec3::ZERO));
    assert!(pixel(Vec3::new(0.0, -2.0, 0.0)) > 7.9 * pixel(Vec3::ZERO));
}

/// 4a review fix 5: a large face partly on screen dices for the part the
/// camera sees, nearest it: a long strip whose corners and middle are all
/// off screen, with the camera 2.5 above one end's part of it.
#[test]
fn partly_seen_faces_dice_where_seen() {
    let points = [Vec3::new(-2.0, -0.05, 0.0), Vec3::new(2.0, -0.05, 0.0), Vec3::new(2.0, 0.05, 0.0), Vec3::new(-2.0, 0.05, 0.0)];
    let p = Poly::from_faces(&points, &[vec![0, 1, 2, 3]]).unwrap();
    let cam = Camera { eye: Vec3::new(1.0, 0.0, 2.5), target: Vec3::new(1.0, 0.0, 0.0), up: Vec3::Y, fov_deg: 35.0, ortho: None };
    let e = Eye::new(&cam, 400, 400);
    let (pixel, seen) = e.face_footprint(&points);
    assert!(seen && (pixel - e.footprint(Vec3::new(1.0, 0.0, 0.0))).abs() < 1e-6, "{pixel} {seen}");
    assert!(points.iter().all(|q| e.footprint(*q) > 7.9 * pixel * q.distance(cam.eye) / 2.5), "every corner is off screen");
    let o = Options { max_level: 6, rate: 16.0, ..opts() };
    let d = dice(&p, Mat4::IDENTITY, &[e], &o, usize::MAX).unwrap().0;
    // 4 long at 0.0039 a pixel, 16 pixels an edge: 63 edges, level 6.
    assert_eq!(d.levels, (6, 6), "the strip dices where it is seen");
}

/// 4a review fix 4: outline edges are diced at the front's level. A face
/// seen from behind next to one that is not takes at least that face's
/// level (edges take the lower level of their faces), and the surface
/// stays closed.
#[test]
fn outline_edges_take_the_front_level() {
    let (_, p) = projected(2);
    let cam = persp(Vec3::new(0.0, 0.0, 2.0));
    let e = eyes(&cam, 400);
    let o = opts();
    let levels = face_levels(&p, Mat4::IDENTITY, &e, &o);
    let (fs, corners) = p.cage_faces();
    let cage = p.cage.to_vec();
    let behind: Vec<bool> = (0..p.faces())
        .map(|f| {
            let face: Vec<Vec3> = corners[fs[f] as usize..fs[f + 1] as usize].iter().map(|&c| cage[c as usize]).collect();
            let middle = face.iter().sum::<Vec3>() / face.len() as f32;
            let n = (0..face.len()).map(|j| face[j].cross(face[(j + 1) % face.len()])).sum::<Vec3>().normalize();
            e[0].facing(middle, n) < -BACK_COS
        })
        .collect();
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for f in 0..p.faces() {
        let face = &corners[fs[f] as usize..fs[f + 1] as usize];
        for j in 0..face.len() {
            let (a, b) = (face[j], face[(j + 1) % face.len()]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(f);
        }
    }
    let mut outline = 0;
    for faces in by_edge.values() {
        let (b, f): (Vec<usize>, Vec<usize>) = faces.iter().partition(|&&f| behind[f]);
        if let (Some(&b), Some(&f)) = (b.first(), f.first()) {
            outline += 1;
            assert!(levels[b] >= levels[f], "outline edge: back face level {} under the front's {}", levels[b], levels[f]);
        }
    }
    assert!(outline > 0 && behind.iter().any(|b| *b));
    let back_levels: Vec<u8> = (0..p.faces()).filter(|&f| behind[f]).map(|f| levels[f]).collect();
    assert!(back_levels.iter().min() < back_levels.iter().max(), "faces away from the outline stay coarse: {back_levels:?}");
    closed(&dice(&p, Mat4::IDENTITY, &e, &o, usize::MAX).unwrap().0.mesh).unwrap();
}

/// 4a review fix 3: a Poly that cannot fit renders at its shown level
/// with a note instead of failing the render. Two Polys over a budget
/// that holds one at level 1: the larger keeps its shown level, the other
/// dices; a budget under both leaves both shown; a cage past the
/// evaluation bound is shown too. Nothing is refused.
#[test]
fn polys_that_cannot_fit_keep_their_shown_level() {
    // Each over the smallest budget at level 1 (12,288 and 19,200).
    let (small, big) = (cube_sphere(16, 0.5), cube_sphere(20, 0.5));
    let polys = [Arc::new(small), Arc::new(big)];
    let drawn: Vec<Mesh> = polys.iter().map(|p| Mesh::poly(p)).collect();
    let make = || -> Vec<Item> { drawn.iter().enumerate().map(|(i, m)| Item { world: Mat4::from_translation(Vec3::new(i as f32 * 1.2 - 0.6, 0.0, 0.0)), ..item(m.clone()) }).collect() };
    let sources: Vec<Source> = (0..2).map(|i| Source { item: i, name: format!("p{i}"), poly: polys[i].clone() }).collect();
    let cam = persp(Vec3::new(0.0, 0.0, 3.0));
    let level1 = |p: &Poly| (p.faces() * 8) as usize;
    let budget = level1(&polys[0]) + level1(&polys[1]) / 2;
    let mut items = make();
    let (_, r) = subdivide(&mut items, &sources, &[cam], 400, 400, &Options { max_triangles: budget, ..opts() }, &Default::default()).unwrap();
    eprintln!("{r:?}");
    let of = |r: &[serde_json::Value], name: &str| r.iter().find(|x| x["object"] == name).unwrap().clone();
    assert!(of(&r, "p0")["triangles"].as_u64().unwrap() > 0);
    assert_eq!(of(&r, "p1")["triangles"], 0);
    assert!(of(&r, "p1")["note"].as_str().unwrap().contains("shown level") && of(&r, "p1")["note"].as_str().unwrap().contains("max_diced"));
    assert!(Arc::ptr_eq(&items[1].mesh, &make()[1].mesh) || items[1].mesh.indices == drawn[1].indices, "p1 keeps its drawn mesh");
    let mut items = make();
    let (added, r) = subdivide(&mut items, &sources, &[cam], 400, 400, &Options { max_triangles: MIN_TRIANGLES, ..opts() }, &Default::default()).unwrap();
    assert_eq!(added, 0);
    assert!(r.iter().all(|x| x["triangles"] == 0 && x["note"].as_str().unwrap().contains("shown level")), "{r:?}");
    let huge = Arc::new(cube_sphere(600, 0.5));
    let mut items = vec![item(Mesh::poly(&cube_sphere(2, 0.5)))];
    let (added, r) = subdivide(&mut items, &[Source { item: 0, name: "huge".into(), poly: huge }], &[cam], 64, 64, &opts(), &Default::default()).unwrap();
    assert_eq!(added, 0);
    assert!(r[0]["note"].as_str().unwrap().contains("evaluation bound"), "{r:?}");
}

/// The plan's `udim_paint_seamless` in a film render: a Poly painted in
/// UDIM tiles (a soft stroke across a seam between charts on two tiles,
/// and a second Poly painted all over in one colour) renders as the same
/// diced surface whose vertex colours are the strokes' colours worked out
/// at each vertex, with no texture: seams, tile borders and gutters show
/// as no more than 2/255 anywhere a pixel's 99th percentile reaches.
#[test]
fn udim_paint_renders_seamless() {
    use crate::poly::texture::Channel;
    use crate::poly::texture::paint::{Surface, paint_poly};
    let p = crate::poly::texture::tests::sphere_udim();
    let s = crate::poly::subdiv::evaluate(&p, p.level).unwrap();
    let uv = crate::poly::uv::at_level(&p, p.level).unwrap();
    // A seam point whose corners lie on two tiles, facing the camera.
    let mut by_point: std::collections::BTreeMap<u32, std::collections::BTreeSet<u16>> = Default::default();
    for (c, &v) in s.corners.iter().enumerate() {
        by_point.entry(v).or_default().insert(crate::poly::texture::tile_of(uv[c]).unwrap());
    }
    let center = by_point.iter().filter(|(_, t)| t.len() > 1).map(|(v, _)| s.points[*v as usize]).max_by(|a, b| a.z.total_cmp(&b.z)).unwrap();
    let surface = Surface::new(&p, p.level).unwrap();
    let ink = [0.85f32, 0.25, 0.15];
    let cases = [("soft stroke across a tile seam", 0.2f32, 0.2f32), ("painted all over", 3.0, 1.0)];
    for (name, radius, hardness) in cases {
        let strokes = crate::poly::texture::tests::strokes(&[json!({ "point": center.to_array(), "radius": radius, "color": ink, "hardness": hardness })]);
        let curve = strokes[0].paint.clone().unwrap().curve;
        let painted = paint_poly(&p, &surface, Channel::Color, 1024, None, false, strokes).unwrap().poly;
        let cam = persp(center.normalize() * 1.6);
        let d = dice(&painted, Mat4::IDENTITY, &eyes(&cam, 160), &opts(), usize::MAX).unwrap().0;
        // The reference: each vertex's colour from the stroke itself (the
        // stroke paints only what faces the way the surface under it does:
        // the far side stays white).
        let facing = center.normalize();
        let mut reference = d.mesh.clone();
        for v in &mut reference.vertices {
            let q = Vec3::from(v.pos);
            let w = if Vec3::from(v.normal).dot(facing) > 0.0 { curve.weight(q.distance(center), radius).clamp(0.0, 1.0) } else { 0.0 };
            v.color = [0, 1, 2].map(|k| 1.0 + (ink[k] - 1.0) * w);
        }
        // Vertex colours away from where the facing rule turns (the
        // terminator, a silhouette from this camera).
        let worst = d.mesh.vertices.iter().zip(&reference.vertices).filter(|(a, _)| Vec3::from(a.normal).dot(facing) > 0.2).map(|(a, b)| (0..3).map(|k| (a.color[k] - b.color[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        let cpu = picture(vec![item(d.mesh.clone())], &cam, Device::Cpu);
        let (mean, p99) = compare(&cpu, &picture(vec![item(reference)], &cam, Device::Cpu));
        if std::env::var("CLIDE_GPU").is_ok_and(|v| v == "1") && crate::film::gpu::get().is_some() {
            let (mg, pg) = compare(&cpu, &picture(vec![item(d.mesh.clone())], &cam, Device::Gpu));
            eprintln!("{name}: GPU against CPU mean {mg:.2}, p99 {pg}");
            assert!(mg <= 2.0 && pg <= 8.0, "{name}: GPU against CPU {mg} {pg}");
        }
        eprintln!("{name}: picture against the stroke's own colours mean {mean:.3}/255, p99 {p99}/255; front vertex colours within {:.3}/255", worst * 255.0);
        assert!(mean <= 0.5 && p99 <= 2.0, "{name}: {mean} {p99}");
        assert!(worst * 255.0 <= 2.0, "{name}: {}", worst * 255.0);
    }
}
