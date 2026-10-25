//! Repeated geometry, end to end. The ignored report
//! `cargo test --release tower_report -- --ignored --nocapture` builds a
//! grid of identical towers (stacked floor bands, posts around every floor,
//! a ring of columns: nested array blobs), meshes them the way the editor
//! does, renders them through render_film's path and exports them, three
//! ways: every object building its own mesh (`separate`, as before
//! sharing), identical objects sharing one (`shared`), and the posts and
//! columns turned into instances as well (`instanced`). It prints and
//! writes (to graphics_check/, or OUT=dir) triangle counts, build times,
//! the device that traced, buffer and export sizes, and a picture per way.
//! TOWERS=n (9), FLOORS=n (48), POSTS=n (40), FINE=0 skips the fine meshes,
//! SIZE=WxH (320x240), SAMPLES=n (16), ONLY=separate|shared|instanced.
//! The tests below check that sharing changes nothing but the cost.

use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use serde_json::{Value, json};

use crate::mesh::MeshLib;
use crate::scene::{Scene, Shape};

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// `towers` identical tower objects on a grid, 3 m apart.
pub(crate) fn tower_scene(towers: usize, floors: usize, posts: usize, detail_cell: f32) -> Scene {
    let clay = crate::clay::tower(floors, posts, detail_cell);
    let mut scene = Scene::default();
    let side = (towers as f32).sqrt().ceil() as usize;
    for i in 0..towers {
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.clay = Some(clay.clone());
        o.translation = Vec3::new((i % side) as f32 * 3.0, 0.0, (i / side) as f32 * 3.0);
    }
    scene
}

/// render_film's items for a scene (see `cinema::prepare_film`).
pub(crate) fn film_items(scene: &Scene, meshes: &MeshLib) -> Vec<crate::film::Item> {
    let eval = scene.evaluate();
    crate::envkit::scatter::draws(scene, &eval)
        .iter()
        .filter(|d| d.object.shape.is_mesh())
        .filter_map(|d| Some(crate::film::Item { mesh: meshes.get(d.object)?.clone(), world: d.world, tint: d.object.color, surface: scene.surface_of(d.object), material: scene.material_of(d.object) }))
        .collect()
}

/// Triangles drawn, triangles of the distinct meshes (by pointer) behind
/// them, and how many distinct meshes.
fn triangles(items: &[crate::film::Item]) -> (usize, usize, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut unique = 0;
    for it in items {
        if seen.insert(Arc::as_ptr(&it.mesh)) {
            unique += it.mesh.indices.len() / 3;
        }
    }
    (items.iter().map(|i| i.mesh.indices.len() / 3).sum(), unique, seen.len())
}

fn mb(bytes: u64) -> f64 {
    (bytes as f64 / 1048576.0 * 10.0).round() / 10.0
}

fn out_dir() -> std::path::PathBuf {
    let dir = std::env::var("OUT").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("graphics_check"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A film render of the towers as render_film makes it, with what it cost.
fn report_film(scene: &Scene, meshes: &MeshLib, picture: Option<String>) -> Value {
    let (w, h) = std::env::var("SIZE").ok().and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).unwrap_or((320, 240));
    let items = film_items(scene, meshes);
    let (drawn, unique, distinct) = triangles(&items);
    let t = Instant::now();
    let film = crate::film::Meshes::new(items);
    let tree = t.elapsed().as_secs_f32();
    let plan = crate::film::gpu::BufferPlan::of(&film, &[(w, h)]);
    let s = crate::film::Settings { width: w, height: h, samples: env("SAMPLES", 16) as u32, bounces: 4, time_limit: std::time::Duration::from_secs(120), floor: true, exposure: 1.0, denoise: true };
    let cam = crate::film::Camera { eye: Vec3::new(14.0, 14.0, 16.0), target: Vec3::new(3.0, 5.0, 3.0), up: Vec3::Y, fov_deg: 40.0, ortho: None };
    let t = Instant::now();
    let image = crate::film::render_views_with_look(&film, &[], &[cam], s, Arc::new(crate::film::Look::default()), &Default::default()).pop().unwrap();
    let seconds = t.elapsed().as_secs_f32();
    if let Some(name) = picture {
        let rgb: Vec<u8> = image.pixels.iter().flatten().copied().collect();
        let path = out_dir().join(name);
        image::save_buffer(&path, &rgb, w as u32, h as u32, image::ColorType::Rgb8).unwrap();
        eprintln!("wrote {}", path.display());
    }
    json!({
        "triangles_traced": drawn, "unique_triangles": unique, "distinct_meshes": distinct,
        "ray_tree_s": tree, "render_s": seconds, "samples": image.samples,
        "device": image.traced.json(),
        "gpu_buffers_mb": plan.entries.iter().map(|(l, b)| (l.to_string(), json!(mb(*b)))).collect::<serde_json::Map<_, _>>(),
    })
}

fn report_exports(scene: &Scene, meshes: &MeshLib) -> Value {
    let mut opts = crate::gltf::Options { roots: Vec::new(), animations: false, fps: 24.0, file_stem: "towers".into(), lods: false, atlas: false, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
    let t = Instant::now();
    let glb = crate::gltf::export(scene, meshes, &opts).unwrap();
    let glb_s = t.elapsed().as_secs_f32();
    let t = Instant::now();
    let fbx = crate::fbx::export(scene, meshes, &opts).unwrap();
    let fbx_s = t.elapsed().as_secs_f32();
    opts.gpu_instancing = true;
    let ext = crate::gltf::export(scene, meshes, &opts).unwrap();
    json!({
        "glb_mb": mb(glb.bytes.len() as u64), "glb_s": glb_s, "glb_meshes": glb.summary["meshes"], "glb_nodes_sharing_a_mesh": glb.summary["shared_mesh_nodes"],
        "glb_ext_instancing_mb": mb(ext.bytes.len() as u64), "glb_ext_instanced_nodes": ext.summary["instanced_nodes"],
        "fbx_mb": mb(fbx.bytes.len() as u64), "fbx_s": fbx_s, "fbx_geometries": fbx.summary["geometries"],
    })
}

/// The tower scene measured three ways: meshing (regular and fine), film
/// and exports.
#[test]
#[ignore]
fn tower_report() {
    let (towers, floors, posts) = (env("TOWERS", 9), env("FLOORS", 48), env("POSTS", 40));
    let base = tower_scene(towers, floors, posts, 0.02);
    let only = std::env::var("ONLY").ok();
    let mut out = serde_json::Map::new();
    out.insert("scene".into(), json!({ "towers": towers, "floors": floors, "posts": posts, "detail_cell": 0.02 }));
    for way in ["separate", "shared", "instanced"] {
        if only.as_deref().is_some_and(|o| o != way) {
            continue;
        }
        let mut scene = base.clone();
        if way == "instanced" {
            // Posts (blob 2: a ring per floor) and columns (blob 3, now 2).
            let ids: Vec<u64> = scene.objects.iter().map(|o| o.id).collect();
            for id in ids {
                crate::envkit::scatter::instance_array(&mut scene, id, 2, false, false).unwrap();
                crate::envkit::scatter::instance_array(&mut scene, id, 2, false, false).unwrap();
            }
        }
        let mut meshes = MeshLib::new();
        meshes.no_sharing = way == "separate";
        let mut r = serde_json::Map::new();
        let t = Instant::now();
        meshes.sync(&scene);
        r.insert("regular_build_s".into(), json!(t.elapsed().as_secs_f32()));
        r.insert("regular_film".into(), report_film(&scene, &meshes, None));
        r.insert("regular_export".into(), report_exports(&scene, &meshes));
        if env("FINE", 1) != 0 {
            meshes.show_fine = true;
            let t = Instant::now();
            meshes.sync(&scene);
            r.insert("fine_build_s".into(), json!(t.elapsed().as_secs_f32()));
            r.insert("fine_film".into(), report_film(&scene, &meshes, Some(format!("instancing_{way}.png"))));
        }
        r.insert("builds".into(), json!({ "regular": meshes.geometry_builds, "fine": meshes.fine_builds, "shared": meshes.aliases_installed }));
        eprintln!("{way}: {}", serde_json::to_string_pretty(&r).unwrap());
        out.insert(way.into(), Value::Object(r));
    }
    let path = out_dir().join(if only.is_some() { "instancing_perf_partial.json" } else { "instancing_perf.json" });
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(out)).unwrap()).unwrap();
    eprintln!("wrote {}", path.display());
}

/// A small clay object and `copies` duplicates of it beside it.
fn duplicates(copies: usize, detail_cell: Option<f32>) -> (Scene, Vec<u64>) {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    let mut clay: crate::clay::Clay = serde_json::from_value(json!({
        "blobs": [
            { "type": "capsule", "a": [0, 0.2, 0], "b": [0, 0.9, 0], "radius": 0.2 },
            { "type": "box", "center": [0, 1.1, 0], "size": [0.4, 0.3, 0.3], "round": 0.05, "blend": 0.05, "color": [0.8, 0.4, 0.3] },
        ],
        "resolution": 24,
    }))
    .unwrap();
    clay.detail_cell = detail_cell;
    scene.get_mut(id).unwrap().clay = Some(clay);
    let mut ids = vec![id];
    for i in 0..copies {
        let c = scene.duplicate_tree(id).unwrap();
        scene.get_mut(c).unwrap().translation = Vec3::new((i + 1) as f32 * 0.8, 0.0, (i % 2) as f32 * 0.5);
        ids.push(c);
    }
    (scene, ids)
}

fn ptr(meshes: &MeshLib, scene: &Scene, id: u64) -> *const crate::mesh::Mesh {
    Arc::as_ptr(meshes.get(scene.get(id).unwrap()).unwrap())
}

#[test]
fn duplicates_share_one_build() {
    use crate::mesh::MeshKey;
    let (mut scene, ids) = duplicates(8, None);
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    assert_eq!((meshes.geometry_builds, meshes.aliases_installed), (1, 8));
    let first = ptr(&meshes, &scene, ids[0]);
    for &id in &ids {
        assert_eq!(ptr(&meshes, &scene, id), first);
        assert_eq!(meshes.key_of(scene.get(id).unwrap()), MeshKey::Own(ids[0]));
    }
    // The viewport uploads it once.
    assert_eq!(meshes.own_meshes().iter().filter(|(k, _)| matches!(k, MeshKey::Own(_))).count(), 1);
    // Editing one copy builds it alone; the others keep sharing.
    scene.get_mut(ids[3]).unwrap().clay.as_mut().unwrap().resolution = 28;
    meshes.sync(&scene);
    assert_eq!(meshes.geometry_builds, 2);
    assert_ne!(ptr(&meshes, &scene, ids[3]), first);
    assert_eq!(meshes.key_of(scene.get(ids[3]).unwrap()), MeshKey::Own(ids[3]));
    assert_eq!(meshes.key_of(scene.get(ids[4]).unwrap()), MeshKey::Own(ids[0]));
    // Deleting the object everyone draws with hands the key to the next.
    scene.remove_tree(ids[0]);
    meshes.sync(&scene);
    assert_eq!(meshes.geometry_builds, 2);
    assert_eq!(meshes.key_of(scene.get(ids[4]).unwrap()), MeshKey::Own(ids[1]));
    let keys: Vec<MeshKey> = meshes.own_meshes().into_iter().map(|(k, _)| k).collect();
    assert!(keys.contains(&MeshKey::Own(ids[1])) && keys.contains(&MeshKey::Own(ids[3])) && keys.len() == 2, "{keys:?}");
    // A copy edited back to match shares again, without a build.
    scene.get_mut(ids[3]).unwrap().clay.as_mut().unwrap().resolution = 24;
    meshes.sync(&scene);
    assert_eq!(meshes.geometry_builds, 2);
    assert_eq!(ptr(&meshes, &scene, ids[3]), ptr(&meshes, &scene, ids[1]));
}

#[test]
fn shared_meshes_stay_per_object_for_bakes_and_rigs() {
    let (mut scene, ids) = duplicates(2, None);
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    // A bake belongs to the object baked, not to the objects sharing its mesh.
    let settings = crate::bake::Settings { triangles: 600, texture: 64, ..Default::default() };
    assert!(meshes.bake(&scene, ids[1], settings).unwrap());
    assert!(meshes.baked(ids[1]).is_some());
    assert!(meshes.baked(ids[0]).is_none() && meshes.baked(ids[2]).is_none());
    // Rigged clay is posed per object: never shared.
    let rig: crate::rig::Rig = serde_json::from_value(json!({ "bones": [{ "name": "root", "head": [0, 0.2, 0], "tail": [0, 1.2, 0] }] })).unwrap();
    for &id in &ids[1..] {
        scene.get_mut(id).unwrap().rig = Some(rig.clone());
    }
    let aliases = meshes.aliases_installed;
    meshes.sync(&scene);
    assert_eq!(meshes.aliases_installed, aliases);
    assert_ne!(ptr(&meshes, &scene, ids[1]), ptr(&meshes, &scene, ids[2]));
    assert_ne!(meshes.key_of(scene.get(ids[1]).unwrap()), meshes.key_of(scene.get(ids[2]).unwrap()));
}

#[test]
fn rigging_one_shared_copy_unshares_it() {
    use crate::mesh::MeshKey;
    let (mut scene, ids) = duplicates(2, Some(0.02));
    let mut meshes = MeshLib::new();
    meshes.show_fine = true;
    meshes.sync(&scene);
    assert_eq!((meshes.geometry_builds, meshes.fine_builds, meshes.aliases_installed), (1, 1, 2));
    // A one-shell rig on the last copy only (its key keeps the clay, so the
    // build reuses the shared shell as its rest mesh): it must skin its own
    // copy of it.
    let rig: crate::rig::Rig = serde_json::from_value(json!({ "bones": [{ "name": "root", "head": [0, 0.2, 0], "tail": [0, 1.2, 0] }], "split": false })).unwrap();
    scene.get_mut(ids[2]).unwrap().rig = Some(rig);
    meshes.sync(&scene);
    let (rest, skin) = meshes.rest(ids[2]).unwrap();
    assert!(skin.is_some());
    assert!(!Arc::ptr_eq(rest, meshes.rest(ids[0]).unwrap().0) && !Arc::ptr_eq(rest, meshes.whole(ids[0]).unwrap()));
    // Its fine mesh is built with its skin, not copied from the others.
    assert_eq!(meshes.fine_builds, 2);
    assert_ne!(ptr(&meshes, &scene, ids[2]), ptr(&meshes, &scene, ids[0]));
    assert_eq!(meshes.key_of(scene.get(ids[2]).unwrap()), MeshKey::Own(ids[2]));
    // The unrigged copies still share.
    assert_eq!(ptr(&meshes, &scene, ids[1]), ptr(&meshes, &scene, ids[0]));
    assert_eq!(meshes.key_of(scene.get(ids[1]).unwrap()), MeshKey::Own(ids[0]));
}

#[test]
fn background_builds_and_fine_meshes_are_shared() {
    let (scene, ids) = duplicates(4, Some(0.02));
    let mut meshes = MeshLib::new();
    meshes.show_fine = true;
    // The window's way: builds run on other threads, polled every frame.
    let start = Instant::now();
    loop {
        meshes.sync_background(&scene);
        if !meshes.busy() && ids.iter().all(|&id| meshes.fine_building(scene.get(id).unwrap()).is_none() && meshes.get(scene.get(id).unwrap()).is_some()) {
            break;
        }
        assert!(start.elapsed().as_secs() < 120, "builds never finished");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    meshes.sync_background(&scene);
    assert_eq!((meshes.geometry_builds, meshes.fine_builds, meshes.aliases_installed), (1, 1, 4));
    let first = ptr(&meshes, &scene, ids[0]);
    assert!(ids.iter().all(|&id| ptr(&meshes, &scene, id) == first));
    // The shared mesh is the fine one.
    let regular = MeshLib::new().tap_sync(&scene);
    assert!(meshes.get(scene.get(ids[0]).unwrap()).unwrap().indices.len() > regular.get(scene.get(ids[0]).unwrap()).unwrap().indices.len());
}

trait TapSync {
    fn tap_sync(self, scene: &Scene) -> Self;
}

impl TapSync for MeshLib {
    fn tap_sync(mut self, scene: &Scene) -> Self {
        self.sync(scene);
        self
    }
}

/// The same scene meshed with and without sharing.
fn shared_and_not(scene: &Scene) -> (MeshLib, MeshLib) {
    let mut apart = MeshLib::new();
    apart.no_sharing = true;
    (MeshLib::new().tap_sync(scene), apart.tap_sync(scene))
}

fn film(items: Vec<crate::film::Item>, device: &str, samples: u32) -> crate::film::Image {
    let look = crate::film::Look::parse(&json!({ "device": device })).unwrap();
    let s = crate::film::Settings { width: 64, height: 48, samples, bounces: 3, time_limit: std::time::Duration::from_secs(120), floor: true, exposure: 1.0, denoise: false };
    let cam = crate::film::Camera { eye: Vec3::new(2.5, 2.2, 4.5), target: Vec3::new(1.6, 0.6, 0.2), up: Vec3::Y, fov_deg: 40.0, ortho: None };
    crate::film::render_views_with_look(&crate::film::Meshes::new(items), &[], &[cam], s, Arc::new(look), &Default::default()).pop().unwrap()
}

fn pixel_mad(a: &crate::film::Image, b: &crate::film::Image) -> f32 {
    a.pixels.iter().flatten().zip(b.pixels.iter().flatten()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>() / (a.pixels.len() * 3) as f32
}

#[test]
fn shared_meshes_render_and_upload_like_separate_ones() {
    let (scene, _) = duplicates(4, None);
    let (shared, apart) = shared_and_not(&scene);
    let (a, b) = (film_items(&scene, &shared), film_items(&scene, &apart));
    assert_eq!(triangles(&a).2, 1);
    assert_eq!(triangles(&b).2, 5);
    // The GPU plan counts shared vertices once.
    let vertices = |items| crate::film::gpu::BufferPlan::of(&crate::film::Meshes::new(items), &[(64, 48)]).entries.iter().find(|e| e.0 == "vertices").unwrap().1;
    assert_eq!(vertices(film_items(&scene, &shared)) * 5, vertices(film_items(&scene, &apart)));
    // The CPU tracer sees the very same triangles either way.
    assert_eq!(film(a, "cpu", 16).pixels, film(b, "cpu", 16).pixels);
}

#[test]
fn gpu_matches_cpu_on_shared_meshes() {
    let (scene, _) = duplicates(4, None);
    let (shared, apart) = shared_and_not(&scene);
    let gpu = film(film_items(&scene, &shared), "gpu", 256);
    gpu.traced.assert_honours(crate::film::look::Device::Gpu);
    if crate::film::gpu::get().is_none() {
        eprintln!("no GPU: skipped");
        return;
    }
    let cpu = film(film_items(&scene, &shared), "cpu", 256);
    let d = pixel_mad(&gpu, &cpu);
    assert!(d < 2.0, "GPU differs from CPU by {d}");
    // Shared vertices upload once and trace the same as separate ones.
    let separate = film(film_items(&scene, &apart), "gpu", 256);
    let worst = gpu.pixels.iter().flatten().zip(separate.pixels.iter().flatten()).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
    assert!(worst <= 1, "shared and separate GPU renders differ by {worst}/255");
}

#[test]
fn viewport_draws_shared_meshes_once_and_the_same() {
    use crate::render::test_support;
    let (scene, _) = duplicates(4, None);
    let (shared, apart) = shared_and_not(&scene);
    let eye = Vec3::new(2.5, 2.2, 4.5);
    let view_proj = glam::Mat4::perspective_rh(40f32.to_radians(), 4.0 / 3.0, 0.05, 40.0) * glam::Mat4::look_at_rh(eye, Vec3::new(1.6, 0.6, 0.2), Vec3::Y);
    let cb = |m: &MeshLib, mask: bool| crate::app::scene_callback_of(&scene, m, view_proj, eye, mask, !mask);
    let keys = |c: &crate::render::ViewportCallback| c.fills.iter().map(|f| f.0).collect::<std::collections::BTreeSet<_>>().len();
    assert_eq!((keys(&cb(&shared, false)), keys(&cb(&apart, false))), (1, 5));
    for format in test_support::FORMATS {
        let Some(rs) = test_support::headless_with(format) else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let render = |c| test_support::render(&rs, &c, 160, 120, test_support::BACKGROUND).unwrap();
        let (a, b) = (render(cb(&shared, false)), render(cb(&apart, false)));
        assert!(test_support::max_abs_diff(&a, &b) <= 1, "{format:?}: shared meshes draw differently ({}/255)", test_support::max_abs_diff(&a, &b));
        assert_eq!(render(cb(&shared, true)).pixels, render(cb(&apart, true)).pixels, "{format:?}: masks differ");
    }
}

#[test]
fn identical_objects_export_one_mesh() {
    let (scene, ids) = duplicates(8, None);
    let (shared, apart) = shared_and_not(&scene);
    let opts = crate::gltf::Options { roots: Vec::new(), animations: false, fps: 24.0, file_stem: "dupes".into(), lods: false, atlas: false, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
    let one = crate::gltf::export(&scene, &shared, &opts).unwrap();
    let nine = crate::gltf::export(&scene, &apart, &opts).unwrap();
    assert_eq!((one.summary["meshes"].as_u64(), nine.summary["meshes"].as_u64()), (Some(1), Some(9)));
    assert_eq!(one.summary["shared_mesh_nodes"], 8);
    assert_eq!(one.summary["triangles"], nine.summary["triangles"]);
    assert!(one.bytes.len() * 4 < nine.bytes.len(), "{} vs {} bytes", one.bytes.len(), nine.bytes.len());
    let doc = gltf_json(&one.bytes);
    assert_eq!(doc["nodes"].as_array().unwrap().len(), ids.len());
    let fbx = crate::fbx::export(&scene, &shared, &opts).unwrap();
    assert_eq!(fbx.summary["geometries"], 1);
    assert!(fbx.bytes.len() * 4 < crate::fbx::export(&scene, &apart, &opts).unwrap().bytes.len());
    // Re-imported, both files hold the same triangles in the same places.
    let (a, b) = (crate::glb_read::from_bytes(&one.bytes, None, None).unwrap(), crate::glb_read::from_bytes(&nine.bytes, None, None).unwrap());
    assert_eq!(a.summary["triangles"], b.summary["triangles"]);
    assert!(a.bounds.0.abs_diff_eq(b.bounds.0, 1e-6) && a.bounds.1.abs_diff_eq(b.bounds.1, 1e-6));
}

fn gltf_json(glb: &[u8]) -> Value {
    let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&glb[20..20 + len]).unwrap()
}

#[test]
fn gpu_instancing_extension_round_trips() {
    let (mut scene, ids) = duplicates(0, None);
    let src = ids[0];
    scene.get_mut(src).unwrap().visible = false;
    let sc = scene.add(Shape::Scatter);
    let instances: Vec<crate::envkit::scatter::Instance> = (0..6)
        .map(|i| crate::envkit::scatter::Instance { source: 0, position: Vec3::new(i as f32 * 0.7, 0.1 * i as f32, -0.3 * i as f32), rotation: glam::Quat::from_rotation_y(0.4 * i as f32) * glam::Quat::from_rotation_x(0.1 * i as f32), scale: 0.8 + 0.1 * i as f32 })
        .collect();
    let o = scene.get_mut(sc).unwrap();
    o.translation = Vec3::new(0.5, 0.0, 1.0);
    o.scatter = Some(crate::envkit::scatter::Scatter { sources: vec![src], instances: instances.clone(), settings: Default::default(), placed_by: None });
    let meshes = MeshLib::new().tap_sync(&scene);
    let mut opts = crate::gltf::Options { roots: Vec::new(), animations: false, fps: 24.0, file_stem: String::new(), lods: false, atlas: false, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
    let nodes = crate::gltf::export(&scene, &meshes, &opts).unwrap();
    opts.gpu_instancing = true;
    let ext = crate::gltf::export(&scene, &meshes, &opts).unwrap();
    let doc = gltf_json(&ext.bytes);
    assert!(doc["extensionsUsed"].as_array().unwrap().contains(&json!("EXT_mesh_gpu_instancing")));
    assert!(doc["extensionsRequired"].is_null());
    assert_eq!((ext.summary["instanced_nodes"].as_u64(), ext.summary["scatter_copies"].as_u64()), (Some(1), Some(6)));
    assert_eq!(ext.summary["triangles"], nodes.summary["triangles"]);
    assert!(ext.bytes.len() < nodes.bytes.len());
    // Read back: one item per copy, where the scatter put it.
    let (a, b) = (crate::glb_read::from_bytes(&ext.bytes, None, None).unwrap(), crate::glb_read::from_bytes(&nodes.bytes, None, None).unwrap());
    assert_eq!(a.items.len(), 6);
    assert_eq!(a.summary["triangles"], b.summary["triangles"]);
    assert!(a.bounds.0.abs_diff_eq(b.bounds.0, 1e-5) && a.bounds.1.abs_diff_eq(b.bounds.1, 1e-5), "{:?} {:?}", a.bounds, b.bounds);
    let world = scene.world_matrix(sc);
    let shape = crate::envkit::scatter::source_shape(scene.get(src).unwrap());
    for (item, inst) in a.items.iter().zip(&instances) {
        assert!(item.world.abs_diff_eq(world * inst.matrix() * shape, 1e-5), "{} vs {}", item.world, world * inst.matrix() * shape);
    }
}

/// The tower fixture's blobs: 0 core, 1 floors, 2 posts (rings of posts
/// per floor), 3 columns, 4 dome (blended).
fn tower_object(floors: usize) -> (Scene, u64) {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    scene.get_mut(id).unwrap().clay = Some(crate::clay::tower(floors, 16, 0.0125));
    (scene, id)
}

#[test]
fn instance_array_checks_what_would_change() {
    let (_, clay) = {
        let (s, id) = tower_object(6);
        let c = s.get(id).unwrap().clay.clone().unwrap();
        (s, c)
    };
    // Posts and columns stand apart; the top floor slab meets the
    // blended dome; the core is not an array.
    assert!(clay.instancing(2, false, false).is_ok());
    assert!(clay.instancing(3, false, false).is_ok());
    let refused = clay.instancing(1, false, false).err().unwrap();
    assert!(refused.contains("dome"), "{refused}");
    assert!(clay.instancing(1, false, true).is_ok());
    assert!(clay.instancing(0, false, false).unwrap_err().contains("not an array"));
    assert_eq!(clay.instancing(2, false, false).unwrap().copies.len(), 6);
    assert_eq!(clay.instancing(2, true, false).unwrap().copies.len(), 6 * 16);
    let mut c = clay.clone();
    // A carve through the posts, a mirrored array, painted clay.
    c.blobs.push(serde_json::from_value(json!({"type":"box","center":[0.6,0.5,0],"size":[0.1,0.1,0.8],"op":"carve","blend":0})).unwrap());
    assert!(c.instancing(2, false, false).is_err());
    let mut c = clay.clone();
    c.blobs[3].mirror = true;
    assert!(c.instancing(3, false, false).unwrap_err().contains("mirrored"));
    let mut c = clay.clone();
    c.decals.push(serde_json::from_value(json!({"shape":"splat","center":[0,0.5,0.3],"normal":[0,0,1],"size":0.05})).unwrap());
    assert!(c.instancing(2, false, false).is_err());
    assert!(c.instancing(2, false, true).is_ok());
    // Arrays listed for the budget: (blob, copies, nested).
    assert_eq!(clay.arrays(), vec![(1, 6, false), (2, 96, true), (3, 8, false)]);
}

#[test]
fn instance_array_meshes_one_copy_and_looks_the_same() {
    use crate::render::test_support;
    let (before, id) = tower_object(6);
    let mut after = before.clone();
    let made = crate::envkit::scatter::instance_array(&mut after, id, 2, false, false).unwrap();
    assert_eq!(made.copies, 6);
    let unit = after.get(made.unit).unwrap();
    assert!(!unit.visible && unit.parent == Some(id));
    assert_eq!(after.get(made.scatter).unwrap().scatter.as_ref().unwrap().placed_by.as_deref(), Some("array"));
    assert_eq!(after.get(id).unwrap().clay.as_ref().unwrap().blobs.len(), 4);
    let (m0, m1) = (MeshLib::new().tap_sync(&before), MeshLib::new().tap_sync(&after));
    let (i0, i1) = (film_items(&before, &m0), film_items(&after, &m1));
    let ((drawn0, unique0, _), (drawn1, unique1, _)) = (triangles(&i0), triangles(&i1));
    eprintln!("triangles drawn {drawn0} -> {drawn1}, unique {unique0} -> {unique1}");
    assert!((drawn1 as f32 / drawn0 as f32 - 1.0).abs() < 0.1, "drawn {drawn0} -> {drawn1}");
    assert!(unique1 < unique0);
    assert_eq!(i1.len(), 1 + 6);
    // The silhouette is the same but for edge pixels.
    let eye = Vec3::new(2.6, 1.6, 3.2);
    let view_proj = glam::Mat4::perspective_rh(40f32.to_radians(), 4.0 / 3.0, 0.05, 40.0) * glam::Mat4::look_at_rh(eye, Vec3::new(0.0, 0.9, 0.0), Vec3::Y);
    let Some(rs) = test_support::headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mask = |s: &Scene, m: &MeshLib| test_support::render(&rs, &crate::app::scene_callback_of(s, m, view_proj, eye, true, false), 160, 120, [0, 0, 0]).unwrap();
    let (a, b) = (mask(&before, &m0), mask(&after, &m1));
    let mad = test_support::mean_abs_diff(&a, &b);
    assert!(mad < 1.0, "silhouettes differ by {mad}/255");
}
