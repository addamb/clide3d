//! Stage 6 checks (docs/SCULPT_PLAN.md): the live sculpt engine's
//! performance on the 1.65M-triangle fixture (the hard figure at a 2 mm
//! detail cell) and the Human, and its renders against the monolithic
//! anchored mesh (`CLIDE_GPU=1`). Timings are logged to target/perf.json
//! and asserted with 1.5x the plan's bound (3x that again without
//! `CLIDE_PERF=1`, see `perf.rs`), except the Human's and the layer
//! amount and mask edit's: in the full suite, whose parallel tests load the
//! machine past what dividing by the measured load makes up for, they
//! failed now and then, so they are asserted only with `CLIDE_PERF=1` (a
//! quiet machine) and otherwise logged (`timing`); what does not depend
//! on the load is checked either way.

use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use serde_json::json;

use super::{figure, image, perf};
use crate::clay::Clay;
use crate::clay::chunks::{ChunkSet, Mesher, dirty_detail};
use crate::mesh::{MeshKey, MeshLib};
use crate::scene::{Scene, Shape};

/// The plan's bounds are asserted with this slack.
const SLACK: f64 = 1.5;

/// How long `reference` takes on a quiet M2 Max (ms).
const REFERENCE_MS: f64 = 8.7;

/// Work of the same shape as a chunk build (a small sphere's chunks, a
/// fresh mesher on a warm blob-field cache): its time against
/// `REFERENCE_MS` says how loaded the machine is (the whole suite runs in
/// parallel). Strict runs (`CLIDE_PERF=1`, a quiet machine) take none.
pub(crate) fn load() -> f64 {
    if perf::strict() {
        return 1.0;
    }
    static REFERENCE: std::sync::OnceLock<(Clay, Vec<crate::material::Material>)> = std::sync::OnceLock::new();
    let (c, palette) = REFERENCE.get_or_init(|| {
        let c = super::sphere(0.5, 160);
        let p = c.palette(&[], "");
        (c, p)
    });
    let (_, ms) = perf::time(3, || ChunkSet::build(&Mesher::new(c, palette).unwrap()).triangles());
    (ms / REFERENCE_MS).max(1.0)
}

/// Runs `f` between two load readings: its time (ms) over the load then.
pub(crate) fn timed_under_load<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let before = load();
    let t = Instant::now();
    let r = f();
    let ms = ms(t);
    (r, ms / ((before + load()) * 0.5))
}

/// The heavy checks run one at a time (they would load each other).
pub(crate) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A timing asserted against `bound_ms` with `CLIDE_PERF=1`, else only
/// logged to target/perf.json (with its bound) for the record.
fn timing(name: &str, ms: f64, bound_ms: f64, extra: serde_json::Value) {
    if perf::strict() {
        perf::check(name, ms, bound_ms, extra);
    } else {
        let mut extra = extra;
        if let serde_json::Value::Object(x) = &mut extra {
            x.insert("bound_ms".into(), json!(bound_ms));
            x.insert("asserted".into(), json!(false));
        }
        perf::log(name, ms, extra);
    }
}

fn stroke(c: &mut Clay, v: serde_json::Value) {
    let s: crate::sculpt::Stroke = serde_json::from_value(v).expect("stroke");
    crate::sculpt::apply(c, &s).expect("applied");
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * 0.95).ceil() as usize).saturating_sub(1).min(v.len() - 1)]
}

/// The 1.65M-triangle fixture: the hard figure at a 2 mm detail cell,
/// sculpted once (so the chunks mesh on the detail grid).
pub fn fixture() -> Clay {
    let mut c = figure(96, true);
    c.set_detail_cell(0.002).unwrap();
    stroke(&mut c, json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.03}));
    c
}

/// Points on the figure's front where dabs land.
pub(crate) fn spots() -> Vec<[f32; 3]> {
    vec![[0.0, 1.25, 0.18], [0.08, 1.1, 0.17], [-0.1, 1.3, 0.16], [0.0, 1.72, 0.13], [0.12, 0.7, 0.07], [-0.12, 0.5, 0.06], [0.05, 1.0, 0.15], [0.0, 1.4, 0.17], [-0.05, 1.18, 0.18], [0.1, 1.35, 0.15], [-0.14, 0.75, 0.08], [0.0, 1.55, 0.06]]
}

#[test]
fn dirty_remesh_pick_and_dab_timings_on_the_big_fixture() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut c = fixture();
    let palette = c.palette(&[], "");
    crate::clay::base_cache::clear();
    let t = Instant::now();
    let mut mesher = Mesher::new(&c, &palette).unwrap();
    let mut set = ChunkSet::build(&mesher);
    let cold = ms(t);
    let tris = set.triangles();
    eprintln!("big fixture: {} chunks, {tris} triangles, cold build {cold:.0} ms", set.chunks.len());
    assert!(tris > 1_500_000, "{tris}");
    // Dabs of r = 40 cells, each followed by a dirty-only remesh (times
    // over the load each ran under).
    let (mut remesh, mut apply, mut chunks) = (Vec::new(), Vec::new(), Vec::new());
    for round in 0..2 {
        for p in spots() {
            let before = c.detail.clone();
            let (_, a) = timed_under_load(|| stroke(&mut c, json!({"brush": if round == 0 { "inflate" } else { "deflate" }, "points": [p], "radius": 0.08, "strength": 0.3})));
            apply.push(a);
            let ((next, dirty), r) = timed_under_load(|| {
                let next = Mesher::new(&c, &palette).unwrap();
                let dirty = dirty_detail(before.as_ref(), c.detail.as_ref(), next.voxel).unwrap();
                set = set.rebuilt(&next, &dirty);
                (next, dirty)
            });
            remesh.push(r);
            chunks.push(dirty.len());
            mesher = next;
        }
    }
    let (remesh_p95, apply_p95) = (p95(remesh.clone()), p95(apply.clone()));
    eprintln!("dab r=40 cells: apply p95 {apply_p95:.1} ms {apply:.1?}; dirty remesh p95 {remesh_p95:.1} ms {remesh:.1?} over {chunks:?} chunks");
    // The plan's 4-8 ms goal (Stage 7's dab-to-photon budget).
    perf::check("live: dab apply p95, r=40 cells at 2 mm", apply_p95, 8.0 * SLACK, json!({ "dabs": apply.len() }));
    perf::check("live: dirty remesh p95, r=40 cells at 2 mm (1.65M triangles)", remesh_p95, 14.0 * SLACK, json!({ "triangles": tris, "chunks": chunks }));
    // Hover pick, warm: the field through the cache.
    let rays: Vec<(Vec3, Vec3)> = spots().iter().map(|p| (Vec3::new(p[0], p[1], 2.0), Vec3::new(0.0, 0.0, -1.0))).collect();
    for (o, d) in &rays {
        assert!(mesher.pick(*o, *d).is_some());
    }
    let before = load();
    let (_, pick) = perf::time(9, || rays.iter().filter_map(|(o, d)| mesher.pick(*o, *d)).count());
    let factor = (before + load()) * 0.5;
    let pick = pick / rays.len() as f64;
    eprintln!("hover pick (pick_field, warm): {:.3} ms", pick);
    perf::check("live: hover pick, warm (1.65M triangles)", pick, 0.2 * SLACK * factor, json!({ "rays": rays.len(), "load": factor }));
}

/// A scene holding `clay`, drawn in chunks (the Sculpt tool on it).
fn session(clay: Clay) -> (Scene, u64, MeshLib) {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    scene.get_mut(id).unwrap().clay = Some(clay);
    let mut lib = MeshLib::new();
    lib.set_sculpt_tool(&[id]);
    lib.sync_joined(&scene);
    assert!(lib.chunked(id));
    (scene, id, lib)
}

fn clay_mut(scene: &mut Scene, id: u64) -> &mut Clay {
    scene.get_mut(id).unwrap().clay.as_mut().unwrap()
}

#[test]
fn the_human_remeshes_dirty_chunks_and_warms_its_cache_in_time() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // A Human as `set_human` makes it (resolution 384), sculpted at its
    // mesh cell.
    let mut c: Clay = serde_json::from_value(json!({ "resolution": 384, "blobs": [{ "type": "human" }] })).unwrap();
    stroke(&mut c, json!({"brush": "inflate", "points": [[0.0, 1.3, 0.12]], "radius": 0.03}));
    let palette = c.palette(&[], "");
    let before = load();
    crate::clay::base_cache::clear();
    let t = Instant::now();
    let mut set = ChunkSet::build(&Mesher::new(&c, &palette).unwrap());
    let cold = ms(t);
    let factor = (before + load()) * 0.5;
    eprintln!("human at its mesh cell {:.4}: {} chunks, {} triangles, cold cache and build {cold:.0} ms", c.detail.as_ref().unwrap().voxel(), set.chunks.len(), set.triangles());
    timing("live: cold Human cache and chunk build (resolution 384)", cold, 3000.0 * factor, json!({ "triangles": set.triangles(), "load": factor }));
    let spots = [[0.0, 1.3, 0.12], [0.08, 1.15, 0.1], [-0.08, 1.4, 0.1], [0.0, 1.62, 0.1], [0.1, 0.9, 0.1], [-0.1, 0.5, 0.08], [0.05, 1.05, 0.12], [-0.05, 1.2, 0.13], [0.12, 1.3, 0.08], [-0.12, 0.7, 0.07]];
    let (mut remesh, mut dirtied) = (Vec::new(), Vec::new());
    for p in spots {
        let before = c.detail.clone();
        stroke(&mut c, json!({"brush": "inflate", "points": [p], "radius": 0.08, "strength": 0.3}));
        let (n, r) = timed_under_load(|| {
            let m = Mesher::new(&c, &palette).unwrap();
            let dirty = dirty_detail(before.as_ref(), c.detail.as_ref(), m.voxel).unwrap();
            set = set.rebuilt(&m, &dirty);
            dirty.len()
        });
        remesh.push(r);
        dirtied.push(n);
    }
    let p = p95(remesh.clone());
    eprintln!("human dirty remesh p95 {p:.1} ms {remesh:.1?} over {dirtied:?} chunks");
    // Load-independent: every dab remeshed some chunks, never all of them.
    assert!(dirtied.iter().all(|&n| n > 0 && n < set.chunks.len()), "dirty chunks per dab {dirtied:?} of {}", set.chunks.len());
    timing("live: Human dirty remesh p95, r=0.08 (resolution 384)", p, 25.0 * SLACK, json!({ "triangles": set.triangles() }));
}

#[test]
fn layer_amounts_and_mask_edits_reach_the_chunks_in_time() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use crate::layers::{Kind, Layer};
    let c = fixture();
    let (mut scene, id, mut lib) = session(c);
    // A sculpt layer of 10k bricks along the surface (the bricks the
    // chunks' vertices fall in, in order): a smooth swell of a few cells.
    let voxel = 0.002;
    let mut keys: Vec<[i32; 3]> = lib.sculpt_chunks().iter().flat_map(|(_, ch)| ch.vertices.iter().map(|v| (Vec3::from(v.pos) / voxel / 8.0).floor().as_ivec3().to_array())).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.truncate(10_000);
    assert_eq!(keys.len(), 10_000);
    let mut d = crate::clay::Detail::new(voxel);
    for k in &keys {
        for i in 0..512 {
            let l = [i % 8, i / 8 % 8, i / 64];
            let g = [k[0] * 8 + l[0], k[1] * 8 + l[1], k[2] * 8 + l[2]];
            // A smooth swell, as a sculpted layer holds.
            d.grid_mut().set(g, voxel * 1.5 * (g[0] as f32 * 0.05).sin() * (g[1] as f32 * 0.03).cos());
        }
    }
    let mut layer = Layer::new("skin", Kind::Sculpt, 1.0);
    layer.detail = Some(d);
    crate::ledger::stack_mut(clay_mut(&mut scene, id)).layers.push(layer);
    lib.sync_joined(&scene);
    let mut times = Vec::new();
    for amount in [0.7, 0.4, 0.9, 0.6, 0.8] {
        crate::ledger::stack_mut(clay_mut(&mut scene, id)).layers[0].amount = amount;
        let builds = lib.chunk_builds;
        // As the window does: until the chunks are drawn (the regular mesh
        // builds after them).
        let (_, t) = timed_under_load(|| {
            lib.sync_background(&scene);
            while lib.chunks_building(id) {
                std::thread::yield_now();
                lib.sync_background(&scene);
            }
        });
        times.push(t);
        assert!(lib.chunk_builds > builds);
        lib.sync_joined(&scene);
    }
    let amount_ms = p95(times.clone());
    eprintln!("layer amount change, 10k-brick layer, to draw-ready chunks: {times:.1?} ms");
    timing("live: layer amount change on a 10k-brick layer to draw-ready chunks (1.65M triangles)", amount_ms, 80.0, json!({ "bricks": 10_000 }));
    // A mask edit: attributes only.
    let mut mask = crate::mask::SculptMask::new(voxel);
    for x in -30..30 {
        for y in 600..660 {
            for z in 60..100 {
                mask.grid_mut().set([x, y, z], 200);
            }
        }
    }
    let (builds, regular) = (lib.chunk_builds, lib.geometry_builds);
    clay_mut(&mut scene, id).sculpt_mask = Some(mask);
    let (_, mask_ms) = timed_under_load(|| lib.sync(&scene));
    assert_eq!((lib.chunk_builds, lib.geometry_builds), (builds, regular), "a mask edit builds no mesh");
    let masked = lib.sculpt_chunks().iter().flat_map(|(_, c)| c.vertices.iter()).filter(|v| v.attrs[0] > 0).count();
    eprintln!("mask edit to draw-ready chunks: {mask_ms:.1} ms ({masked} vertices masked)");
    assert!(masked > 1000);
    timing("live: mask edit to draw-ready chunks (1.65M triangles)", mask_ms, 16.0 * SLACK, json!({ "masked": masked }));
}

#[test]
#[ignore = "diagnostic: dab apply p50/p95 on the big fixture (CLIDE_PROFILE_SECS loops dabs for a profiler)"]
fn dab_profile() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut c = fixture();
    // Warm the blob-field cache as a session would.
    stroke(&mut c, json!({"brush": "inflate", "points": [spots()[0]], "radius": 0.08, "strength": 0.3}));
    let secs: f64 = std::env::var("CLIDE_PROFILE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let start = Instant::now();
    let cold = std::env::var("CLIDE_COLD").is_ok_and(|v| v == "1");
    loop {
        if cold {
            crate::clay::base_cache::clear();
        }
        let mut apply = Vec::new();
        for round in 0..2 {
            for p in spots() {
                let t = Instant::now();
                stroke(&mut c, json!({"brush": if round == 0 { "inflate" } else { "deflate" }, "points": [p], "radius": 0.08, "strength": 0.3}));
                apply.push(ms(t));
            }
        }
        let mut v = apply.clone();
        v.sort_by(f64::total_cmp);
        eprintln!("dab apply r=40 cells: p50 {:.2} ms, p95 {:.2} ms, min {:.2}, max {:.2}; first round {:.1?}", v[v.len() / 2], p95(apply.clone()), v[0], v[v.len() - 1], &apply[..12]);
        if start.elapsed().as_secs_f64() >= secs {
            break;
        }
    }
}

#[test]
#[ignore = "diagnostic: where one chunk build spends its time"]
fn chunk_profile() {
    let c = fixture();
    let palette = c.palette(&[], "");
    let m = Mesher::new(&c, &palette).unwrap();
    let set = ChunkSet::build(&m);
    let keys: Vec<_> = set.chunks.keys().copied().collect();
    let m = Mesher::new(&c, &palette).unwrap();
    let t = Instant::now();
    let mut tris = 0;
    for k in &keys[..100] {
        tris += m.mesh_chunk(*k).indices.len() / 3;
    }
    eprintln!("100 chunks one thread: {:.1} ms, {tris} triangles", ms(t));
    let t = Instant::now();
    for k in &keys[..100] {
        tris += m.mesh_chunk(*k).indices.len() / 3;
    }
    eprintln!("again (lattice memo warm): {:.1} ms", ms(t));
}

#[test]
#[ignore = "diagnostic: the steps of a layer amount change"]
fn layer_profile() {
    use crate::layers::{Kind, Layer};
    let mut c = fixture();
    let palette = c.palette(&[], "");
    let voxel = 0.002;
    let m = Mesher::new(&c, &palette).unwrap();
    let set = ChunkSet::build(&m);
    let mut keys: Vec<[i32; 3]> = set.chunks.values().flat_map(|ch| ch.positions.iter().map(|p| (*p / voxel / 8.0).floor().as_ivec3().to_array())).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.truncate(10_000);
    let mut d = crate::clay::Detail::new(voxel);
    for k in &keys {
        for i in 0..512 {
            let l = [i % 8, i / 8 % 8, i / 64];
            let g = [k[0] * 8 + l[0], k[1] * 8 + l[1], k[2] * 8 + l[2]];
            // A smooth swell, as a sculpted layer holds.
            d.grid_mut().set(g, voxel * 1.5 * (g[0] as f32 * 0.05).sin() * (g[1] as f32 * 0.03).cos());
        }
    }
    let mut layer = Layer::new("skin", Kind::Sculpt, 1.0);
    layer.detail = Some(d);
    crate::ledger::stack_mut(&mut c).layers.push(layer);
    let a = crate::layers::composed_clay(&c);
    let m = Mesher::new(&a, &palette).unwrap();
    let t = Instant::now();
    let set = ChunkSet::build(&m);
    eprintln!("build with layer: {:.0} ms, {} chunks", ms(t), set.chunks.len());
    crate::ledger::stack_mut(&mut c).layers[0].amount = 0.7;
    let t = Instant::now();
    let b = crate::layers::composed_clay(&c);
    eprintln!("compose: {:.1} ms", ms(t));
    let t = Instant::now();
    let m2 = Mesher::new(&b, &palette).unwrap();
    eprintln!("mesher: {:.1} ms", ms(t));
    let t = Instant::now();
    let dirty = dirty_detail(a.detail.as_ref(), b.detail.as_ref(), voxel).unwrap();
    eprintln!("dirty: {:.1} ms, {} chunks", ms(t), dirty.len());
    let t = Instant::now();
    let set2 = set.rebuilt(&m2, &dirty);
    eprintln!("rebuilt: {:.1} ms, {} chunks", ms(t), set2.chunks.len());
    let t = Instant::now();
    let set3 = set.rebuilt(&m2, &dirty);
    eprintln!("rebuilt again (memo warm): {:.1} ms, {} chunks", ms(t), set3.chunks.len());
    let t = Instant::now();
    let one = crate::clay::with_threads(1, || set.rebuilt(&m2, &dirty));
    eprintln!("rebuilt on one thread (memo warm): {:.1} ms, {} chunks", ms(t), one.chunks.len());
    // CLIDE_PROFILE_SECS: amount changes in a loop (compose, mesher,
    // dirty set, cold rebuild), for a profiler.
    let secs: f64 = std::env::var("CLIDE_PROFILE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let start = Instant::now();
    let mut k = 0;
    while start.elapsed().as_secs_f64() < secs {
        k += 1;
        crate::ledger::stack_mut(&mut c).layers[0].amount = [0.7, 0.4, 0.9][k % 3];
        let t = Instant::now();
        let b = crate::layers::composed_clay(&c);
        let m2 = Mesher::new(&b, &palette).unwrap();
        let dirty = dirty_detail(a.detail.as_ref(), b.detail.as_ref(), voxel).unwrap();
        let n = set.rebuilt(&m2, &dirty).chunks.len();
        eprintln!("amount change: {:.1} ms ({n} chunks)", ms(t));
    }
}

#[test]
#[ignore = "diagnostic: calibrates REFERENCE_MS on a quiet machine"]
fn load_reference() {
    let c = super::sphere(0.5, 160);
    let palette = c.palette(&[], "");
    let (_, ms) = perf::time(9, || ChunkSet::build(&Mesher::new(&c, &palette).unwrap()).triangles());
    eprintln!("reference: {ms:.2} ms");
}

/// A sculpted figure drawn in chunks, for the render checks.
fn gpu_scene() -> (Scene, u64, MeshLib) {
    let mut c = figure(96, false);
    c.set_detail_cell(0.005).unwrap();
    for (p, b) in [([0.0, 1.25, 0.18], "inflate"), ([0.1, 1.35, 0.15], "standard"), ([0.0, 1.72, 0.13], "inflate")] {
        stroke(&mut c, json!({"brush": b, "points": [p], "radius": 0.05, "strength": 0.4}));
    }
    let (mut scene, id, lib) = session(c);
    scene.get_mut(id).unwrap().color = [0.85, 0.72, 0.55];
    (scene, id, lib)
}

fn views() -> [image::View; 4] {
    let t = Vec3::new(0.0, 1.0, 0.0);
    [
        image::View { eye: Vec3::new(1.2, 1.6, 3.0), target: t, fov: 35.0 },
        image::View { eye: Vec3::new(-3.0, 1.2, 0.8), target: t, fov: 35.0 },
        image::View { eye: Vec3::new(0.3, 1.3, -3.2), target: t, fov: 35.0 },
        image::View { eye: Vec3::new(0.0, 1.5, 1.2), target: Vec3::new(0.0, 1.45, 0.0), fov: 30.0 },
    ]
}

fn callback(scene: &Scene, meshes: &MeshLib, v: &image::View) -> crate::render::ViewportCallback {
    let far = (v.eye - v.target).length() * 3.0 + 10.0;
    let view_proj = glam::Mat4::perspective_rh(v.fov.to_radians(), 1.0, 0.05, far) * glam::Mat4::look_at_rh(v.eye, v.target, Vec3::Y);
    crate::app::scene_callback_of(scene, meshes, view_proj, v.eye, false, false)
}

fn render(rs: &egui_wgpu::RenderState, cb: &crate::render::ViewportCallback) -> egui::ColorImage {
    crate::render::test_support::render(rs, cb, image::SIZE, image::SIZE, crate::render::test_support::BACKGROUND).unwrap_or_else(|e| panic!("CLIDE_GPU=1 but: {e}"))
}

#[test]
fn chunked_renders_match_the_monolithic_anchored_mesh() {
    if !image::gpu_enabled() {
        eprintln!("chunked_renders_match_the_monolithic_anchored_mesh: set CLIDE_GPU=1 to run");
        return;
    }
    let rs = crate::render::test_support::headless().expect("CLIDE_GPU=1 but no adapter");
    let (scene, id, lib) = gpu_scene();
    let whole = Arc::new(lib.assembled(id).unwrap());
    for (k, v) in views().iter().enumerate() {
        let chunked = callback(&scene, &lib, v);
        assert!(chunked.fills.iter().any(|(key, _)| matches!(key, MeshKey::Chunk(..))));
        // The same scene with the chunks as one mesh, drawn the regular way.
        let mut mono = callback(&scene, &lib, v);
        let inst = mono.fills.iter().find(|(key, _)| matches!(key, MeshKey::Chunk(..))).map(|(_, i)| *i).unwrap();
        mono.fills.retain(|(key, _)| !matches!(key, MeshKey::Chunk(..)));
        mono.fills.push((MeshKey::Own(id), inst));
        mono.fills.sort_by_key(|(key, _)| *key);
        mono.dynamic.retain(|(key, _)| *key != MeshKey::Own(id));
        mono.dynamic.push((MeshKey::Own(id), whole.clone()));
        mono.chunks.clear();
        let (a, b) = (render(&rs, &chunked), render(&rs, &mono));
        let (psnr, max, mean) = (crate::render::test_support::psnr(&a, &b), crate::render::test_support::max_abs_diff(&a, &b), crate::render::test_support::mean_abs_diff(&a, &b));
        eprintln!("view {k}: chunked against monolithic: PSNR {psnr:.1} dB, max {max}/255, mean {mean:.3}/255");
        assert!(psnr >= 50.0 && max <= 8 && mean <= 0.5, "view {k}: PSNR {psnr:.1}, max {max}, mean {mean:.3}");
    }
}

#[test]
fn partial_uploads_draw_what_full_uploads_draw_through_undo_redo_and_goto() {
    if !image::gpu_enabled() {
        eprintln!("partial_uploads_draw_what_full_uploads_draw_through_undo_redo_and_goto: set CLIDE_GPU=1 to run");
        return;
    }
    use crate::render::test_support::{chunk_upload, clear_chunk_pool, max_abs_diff};
    let rs = crate::render::test_support::headless().expect("CLIDE_GPU=1 but no adapter");
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut scene, id, mut lib) = gpu_scene();
    let v = &views()[0];
    let _gpu = crate::render::test_support::hold(&rs);
    let first = render(&rs, &callback(&scene, &lib, v));
    let total = chunk_upload(&rs).1;
    // History: the scene before each of three strokes, then after.
    let mut steps = vec![scene.clone()];
    let mut uploads = Vec::new();
    for p in [[0.05, 1.2, 0.17], [-0.08, 1.32, 0.16], [0.02, 1.62, 0.1]] {
        stroke(clay_mut(&mut scene, id), json!({"brush": "inflate", "points": [p], "radius": 0.06, "strength": 0.4}));
        steps.push(scene.clone());
    }
    // Forward (strokes), back (undo, undo), forward (redo), a jump (goto 0).
    for (what, k) in [("stroke", 1), ("stroke", 2), ("stroke", 3), ("undo", 2), ("undo", 1), ("redo", 2), ("goto", 0), ("goto", 3)] {
        lib.sync_joined(&steps[k]);
        let partial = render(&rs, &callback(&steps[k], &lib, v));
        let ((bytes, chunks, ms), held) = chunk_upload(&rs);
        assert!(chunks * 3 < held, "{what} to {k}: {chunks} of {held} chunks went up");
        uploads.push((bytes, ms));
        clear_chunk_pool(&rs);
        let full = render(&rs, &callback(&steps[k], &lib, v));
        let d = max_abs_diff(&partial, &full);
        eprintln!("{what} to step {k}: {chunks} chunks ({:.2} MB) uploaded in {ms:.2} ms; partial against full re-upload {d}/255", bytes as f64 / 1048576.0);
        assert!(d <= 1, "{what} to step {k}: {d}/255");
        if k == 0 {
            assert!(max_abs_diff(&partial, &first) <= 1, "going back draws the first frame again");
        }
    }
    let upload = p95(uploads.iter().map(|u| u.1).collect());
    let mb = uploads.iter().map(|u| u.0).max().unwrap() as f64 / 1048576.0;
    assert!(mb <= 8.0, "{mb:.2} MB");
    assert!(total > 0);
    perf::check("live: GPU upload of a stroke's dirty chunks", upload, 3.0 * SLACK, json!({ "mb": mb }));
}

#[test]
fn matcap_shading_reads_the_matcap_from_centre_to_rim() {
    if !image::gpu_enabled() {
        eprintln!("matcap_shading_reads_the_matcap_from_centre_to_rim: set CLIDE_GPU=1 to run");
        return;
    }
    use crate::render::matcap::{Matcap, uv};
    let rs = crate::render::test_support::headless().expect("CLIDE_GPU=1 but no adapter");
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    scene.get_mut(id).unwrap().color = [1.0; 3];
    scene.get_mut(id).unwrap().clay = Some(serde_json::from_value(json!({ "resolution": 160, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "color": [1, 1, 1] }] })).unwrap());
    let mut lib = MeshLib::new();
    lib.sync_joined(&scene);
    let v = image::View::FRONT;
    let mut cb = callback(&scene, &lib, &v);
    let m = Matcap::builtin("clay").unwrap();
    cb.matcap = Some(m.clone());
    cb.post.enabled = false;
    cb.shadows.enabled = false;
    cb.ao.enabled = false;
    let img = render(&rs, &cb);
    // Pixels from the sphere's middle (as seen) to near its rim.
    let far = (v.eye - v.target).length() * 3.0 + 10.0;
    let view_proj = glam::Mat4::perspective_rh(v.fov.to_radians(), 1.0, 0.05, far) * glam::Mat4::look_at_rh(v.eye, v.target, Vec3::Y);
    let inv = view_proj.inverse();
    let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let bilinear = |p: glam::Vec2| -> [f32; 3] {
        let s = m.side as f32;
        let (x, y) = (p.x * s - 0.5, p.y * s - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |i: f32, j: f32| {
            let (i, j) = (i.clamp(0.0, s - 1.0) as usize, j.clamp(0.0, s - 1.0) as usize);
            let k = (j * m.side as usize + i) * 4;
            [m.rgba[k], m.rgba[k + 1], m.rgba[k + 2]].map(|c| (c as f32 / 255.0).powf(2.2))
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        std::array::from_fn(|n| ((a[n] * (1.0 - fx) + b[n] * fx) * (1.0 - fy) + (c[n] * (1.0 - fx) + d[n] * fx) * fy).powf(1.0 / 2.2) * 255.0)
    };
    let centre = view_proj.project_point3(Vec3::ZERO);
    let (cx, cy) = ((centre.x * 0.5 + 0.5) * image::SIZE as f32, (0.5 - centre.y * 0.5) * image::SIZE as f32);
    let (mut worst, mut n) = (0.0f32, 0);
    for step in 0..60 {
        let (px, py) = (cx + step as f32 * 2.0, cy);
        let ndc = glam::Vec2::new(px / image::SIZE as f32 * 2.0 - 1.0, 1.0 - py / image::SIZE as f32 * 2.0);
        let (a, b) = (inv.project_point3(ndc.extend(0.0)), inv.project_point3(ndc.extend(1.0)));
        let d = (b - a).normalize();
        // The ray against the true sphere.
        let (bq, cq) = (a.dot(d), a.length_squared() - 0.25);
        let disc = bq * bq - cq;
        if disc <= 0.0 {
            break;
        }
        let hit = a + d * (-bq - disc.sqrt());
        let normal = hit.normalize();
        // Stay off the silhouette (one pixel is mostly background there).
        if normal.dot((v.eye - hit).normalize()) < 0.2 {
            break;
        }
        let want = lum(bilinear(uv(normal, (v.eye - hit).normalize())));
        let p = img.pixels[py as usize * image::SIZE as usize + px as usize];
        let got = lum([p.r() as f32, p.g() as f32, p.b() as f32]);
        worst = worst.max((got - want).abs());
        n += 1;
    }
    eprintln!("matcap: {n} pixels centre to rim, worst luminance {worst:.2}/255");
    assert!(n > 20 && worst <= 6.0, "{n} pixels, worst {worst:.2}/255");
}

#[test]
fn a_dab_on_the_big_fixture_uploads_only_its_chunks_in_time() {
    if !image::gpu_enabled() {
        eprintln!("a_dab_on_the_big_fixture_uploads_only_its_chunks_in_time: set CLIDE_GPU=1 to run");
        return;
    }
    use crate::render::test_support::chunk_upload;
    let rs = crate::render::test_support::headless().expect("CLIDE_GPU=1 but no adapter");
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (mut scene, id, mut lib) = session(fixture());
    let v = image::View { eye: Vec3::new(0.4, 1.3, 2.6), target: Vec3::new(0.0, 1.0, 0.0), fov: 35.0 };
    // Under the GPU lock throughout: another test's render would hold
    // other chunks and drop these.
    let uploads = crate::render::test_support::checked(&rs, || {
        render(&rs, &callback(&scene, &lib, &v));
        let total = chunk_upload(&rs).1;
        let mut uploads = Vec::new();
        for p in spots().into_iter().take(6) {
            stroke(clay_mut(&mut scene, id), json!({"brush": "inflate", "points": [p], "radius": 0.08, "strength": 0.3}));
            lib.sync_joined(&scene);
            render(&rs, &callback(&scene, &lib, &v));
            let ((bytes, chunks, ms), held) = chunk_upload(&rs);
            eprintln!("dab: {chunks} of {held} chunks, {:.2} MB, uploaded in {ms:.2} ms", bytes as f64 / 1048576.0);
            assert!(chunks * 5 < held && held >= total / 2);
            uploads.push((ms, bytes));
        }
        uploads
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let mb = uploads.iter().map(|u| u.1).max().unwrap() as f64 / 1048576.0;
    assert!(mb <= 8.0, "{mb:.2} MB");
    perf::check("live: GPU upload of a dab's dirty chunks, r=40 cells at 2 mm (1.65M triangles)", p95(uploads.iter().map(|u| u.0).collect()), 3.0 * SLACK, json!({ "mb": mb }));
}
