use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::mesh::MeshKey;
use crate::scene::{Scene, Shape};

fn stroke(c: &mut Clay, v: serde_json::Value) -> usize {
    let s: crate::sculpt::Stroke = serde_json::from_value(v).expect("stroke");
    crate::sculpt::apply(c, &s).expect("applied").changed_points
}

fn scene_with(clay: Clay) -> (Scene, u64) {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    scene.get_mut(id).unwrap().clay = Some(clay);
    (scene, id)
}

fn clay_mut(scene: &mut Scene, id: u64) -> &mut Clay {
    scene.get_mut(id).unwrap().clay.as_mut().unwrap()
}

/// The chunks drawn, as one welded mesh, equal a fresh session's.
fn same_as_fresh(lib: &MeshLib, scene: &Scene, id: u64) {
    let mut fresh = MeshLib::new();
    fresh.set_sculpt_tool(&[id]);
    fresh.sync_joined(scene);
    let (a, b) = (lib.assembled(id).unwrap(), fresh.assembled(id).unwrap());
    assert_eq!(a.indices, b.indices);
    assert!(a.vertices.iter().zip(&b.vertices).all(|(x, y)| x.pos == y.pos && x.normal == y.normal && x.color == y.color));
    let slim = |l: &MeshLib| l.sculpt_chunks().into_iter().map(|(k, c)| (k, c.vertices.clone(), c.indices.clone())).collect::<Vec<_>>();
    let (mut sa, mut sb) = (slim(lib), slim(&fresh));
    sa.sort_by_key(|x| x.0);
    sb.sort_by_key(|x| x.0);
    assert!(sa == sb, "draw-ready chunks differ from a fresh session's");
}

#[test]
fn a_stroke_starts_a_session_and_rebuilds_only_dirty_chunks() {
    let mut c = crate::harness::figure(96, false);
    c.set_detail_cell(0.005).unwrap();
    stroke(&mut c, json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.04}));
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    let t0 = Instant::now();
    lib.session_now = Some(t0);
    lib.sync_joined(&scene);
    assert!(!lib.chunked(id), "no session before a stroke or a tool");
    // A stroke (the grid changed) starts one: every chunk builds once.
    stroke(clay_mut(&mut scene, id), json!({"brush": "standard", "points": [[0.05, 1.3, 0.17]], "radius": 0.03, "strength": 0.5}));
    lib.sync_joined(&scene);
    assert!(lib.chunked(id));
    let all = lib.chunk_builds;
    let shown: Vec<(MeshKey, Arc<SculptChunk>)> = lib.sculpt_chunks();
    assert!(all > 10 && shown.len() == all, "{all} chunks built, {} shown", shown.len());
    same_as_fresh(&lib, &scene, id);
    // The next stroke rebuilds a few; the others keep their Arcs (the GPU
    // uploads only chunks whose pointer changed).
    let regular = lib.geometry_builds;
    stroke(clay_mut(&mut scene, id), json!({"brush": "inflate", "points": [[-0.05, 1.3, 0.17]], "radius": 0.03}));
    lib.sync_joined(&scene);
    let dirty = lib.chunk_builds - all;
    assert!(dirty > 0 && dirty * 4 < all, "{dirty} rebuilt of {all}");
    let after = lib.sculpt_chunks();
    let kept = after.iter().filter(|(k, c)| shown.iter().any(|(k2, c2)| k == k2 && Arc::ptr_eq(c, c2))).count();
    assert!(kept >= after.len() - dirty, "{kept} of {} chunks kept their upload", after.len());
    same_as_fresh(&lib, &scene, id);
    // The regular mesh still builds (snapping, film and exports read it).
    assert!(lib.geometry_builds > regular);
    // Sixty seconds on, the session ends.
    lib.session_now = Some(t0 + SESSION + Duration::from_secs(1));
    lib.sync_joined(&scene);
    assert!(!lib.chunked(id));
}

#[test]
fn undo_redo_and_goto_rebuild_only_what_differs() {
    let mut c = crate::harness::figure(96, false);
    c.set_detail_cell(0.005).unwrap();
    stroke(&mut c, json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.04}));
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    lib.set_sculpt_tool(&[id]);
    lib.sync_joined(&scene);
    let snapshots: Vec<Scene> = (0..3)
        .map(|k| {
            let s = scene.clone();
            stroke(clay_mut(&mut scene, id), json!({"brush": "inflate", "points": [[-0.1 + 0.1 * k as f32, 1.3, 0.17]], "radius": 0.03}));
            s
        })
        .collect();
    lib.sync_joined(&scene);
    for back in snapshots.iter().rev().chain(std::iter::once(&scene)) {
        let before = lib.chunk_builds;
        let shown = lib.sculpt_chunks();
        lib.sync_joined(back);
        let rebuilt = lib.chunk_builds - before;
        assert!(rebuilt < shown.len() / 3, "{rebuilt} of {}", shown.len());
        same_as_fresh(&lib, back, id);
    }
}

#[test]
fn mask_and_group_edits_change_attributes_only() {
    let mut c = crate::harness::sphere(0.5, 96);
    c.set_detail_cell(0.01).unwrap();
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    lib.set_sculpt_tool(&[id]);
    lib.sync_joined(&scene);
    let (builds, regular, slim) = (lib.chunk_builds, lib.geometry_builds, lib.slim_updates);
    // Mask the top cap.
    let mut m = crate::mask::SculptMask::new(0.01);
    for x in -10..=10 {
        for z in -10..=10 {
            for y in 46..=52 {
                m.grid_mut().set([x, y, z], 255);
            }
        }
    }
    clay_mut(&mut scene, id).sculpt_mask = Some(m);
    lib.sync_joined(&scene);
    assert_eq!((lib.chunk_builds, lib.geometry_builds), (builds, regular), "a mask edit builds no mesh");
    let touched = lib.slim_updates - slim;
    let chunks = lib.sculpt_chunks();
    assert!(touched > 0 && touched < chunks.len(), "{touched} of {} chunks", chunks.len());
    let masked = chunks.iter().flat_map(|(_, c)| c.vertices.iter()).filter(|v| v.attrs[0] == 255).count();
    let top = chunks.iter().flat_map(|(_, c)| c.vertices.iter()).filter(|v| v.pos[1] > 0.49 && v.pos[0].abs() < 0.05 && v.pos[2].abs() < 0.05).count();
    assert!(masked >= top && top > 0);
    // Groups: a hidden group's triangles leave its chunks' index lists.
    let mut g = crate::groups::Groups::new(0.01);
    for x in -20..=20 {
        for z in -20..=20 {
            for y in -52..=-44 {
                g.data_mut().set([x, y, z], 3);
            }
        }
    }
    g.data_mut().info.insert(3, crate::groups::GroupInfo { name: "bottom".into(), visible: false });
    let before = lib.sculpt_chunks().iter().map(|(_, c)| c.indices.len()).sum::<usize>();
    clay_mut(&mut scene, id).groups = Some(g);
    lib.sync_joined(&scene);
    assert_eq!(lib.chunk_builds, builds);
    let after = lib.sculpt_chunks().iter().map(|(_, c)| c.indices.len()).sum::<usize>();
    assert!(after < before);
    assert!(lib.sculpt_chunks().iter().flat_map(|(_, c)| c.vertices.iter()).any(|v| v.attrs[1] == 3));
}

#[test]
fn rigged_clay_chunks_at_rest_and_rebuilds_only_dirty_chunks() {
    let body = crate::human::Human::default().build().unwrap();
    let mut c: Clay = serde_json::from_value(json!({ "resolution": 96, "blobs": [{ "type": "human" }] })).unwrap();
    c.set_detail_cell(0.008).unwrap();
    let (mut scene, id) = scene_with(c);
    scene.get_mut(id).unwrap().rig = Some(crate::rig::Rig { bones: body.rig(), ..Default::default() });
    let mut lib = MeshLib::new();
    lib.set_sculpt_tool(&[id]);
    lib.sync_joined(&scene);
    assert!(lib.chunked(id), "rigged clay at rest is chunked");
    let all = lib.chunk_builds;
    let points: Vec<[f32; 3]> = (0..12).map(|k| [-0.1 + k as f32 * 0.02, 1.35, 0.15]).collect();
    let changed = stroke(clay_mut(&mut scene, id), json!({"brush": "add", "points": points, "radius": 0.03}));
    assert!(changed > 0);
    lib.sync_joined(&scene);
    let dirty = lib.chunk_builds - all;
    assert!(dirty > 0 && dirty * 3 < all, "{dirty} rebuilt of {all}");
    same_as_fresh(&lib, &scene, id);
}

#[test]
fn octahedral_normals_round_trip() {
    for k in 0..2000 {
        let a = k as f32 * 2.399_963;
        let z = 1.0 - 2.0 * (k as f32 + 0.5) / 2000.0;
        let n = Vec3::new((1.0 - z * z).sqrt() * a.cos(), (1.0 - z * z).sqrt() * a.sin(), z);
        assert!(oct_decode(oct_encode(n)).angle_between(n) < 1e-3, "{n}");
    }
}

#[test]
fn a_failed_chunk_build_is_not_tried_again_until_the_clay_changes() {
    // Past the mesher's cells across (the detail regions' path skips the
    // fine check): the build fails.
    let mut c = crate::harness::sphere(0.5, 64);
    c.detail_cell = Some(0.0002);
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    lib.set_sculpt_tool(&[id]);
    lib.sync_joined(&scene);
    assert!(!lib.chunked(id) && !lib.chunks_pending());
    // Frames on, no build starts again (and nothing is busy).
    for _ in 0..3 {
        lib.sync_background(&scene);
        assert!(!lib.chunks_pending() && !lib.busy(), "a failed chunk build started again");
    }
    // A coarser cell: the build runs and the clay is drawn in chunks.
    clay_mut(&mut scene, id).detail_cell = Some(0.01);
    lib.sync_joined(&scene);
    assert!(lib.chunked(id));
}

/// D-e: the viewport's quality shade is measured off the window's thread,
/// only while it is on, and not while strokes come in (chunks show them;
/// the colors follow once the strokes pause).
#[test]
fn viewport_shades_measure_in_the_background_after_strokes() {
    let mut c = crate::harness::figure(48, false);
    c.set_detail_cell(0.01).unwrap();
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    let t0 = Instant::now();
    lib.session_now = Some(t0);
    lib.sync_joined(&scene);
    // Off: a stroke measures nothing.
    stroke(clay_mut(&mut scene, id), json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.04}));
    lib.sync_joined(&scene);
    assert_eq!(lib.shade_jobs, 0);
    assert!(!lib.shading());
    // On: one job, then the colors are drawn.
    lib.shade = crate::quality::Shade::Curvature;
    lib.sync_joined(&scene);
    assert_eq!(lib.shade_jobs, 1);
    while lib.shading() {
        lib.sync_joined(&scene);
    }
    assert!(lib.shaded_now(id));
    let o = scene.get(id).unwrap().clone();
    let truth = crate::quality::shade_colors(crate::quality::Shade::Curvature, o.clay.as_ref().unwrap(), lib.unposed(id).unwrap(), crate::mesh::SHADE_SAMPLES).unwrap();
    assert!(lib.display(&o).unwrap().vertices.iter().zip(&truth).all(|(v, c)| v.color == *c));
    // A sculpt tool's strokes: no measure while they come in, and the
    // clay draws without the stale colors.
    lib.set_sculpt_tool(&[id]);
    stroke(clay_mut(&mut scene, id), json!({"brush": "standard", "points": [[0.05, 1.3, 0.17]], "radius": 0.03, "strength": 0.5}));
    lib.sync_joined(&scene);
    assert!(lib.chunked(id) && lib.sculpting_now(id));
    assert_eq!(lib.shade_jobs, 1, "no measure while strokes come in");
    assert!(lib.shading() && !lib.shaded_now(id));
    let own = lib.own_meshes().into_iter().find(|(k, _)| *k == MeshKey::Own(id)).unwrap().1;
    assert!(Arc::ptr_eq(&own, lib.get(&o).unwrap()), "drawn plain meanwhile");
    // Once they pause, the colors are measured again for the new mesh.
    lib.session_now = Some(t0 + PAUSE + Duration::from_millis(1));
    lib.sync_joined(&scene);
    assert_eq!(lib.shade_jobs, 2);
    while lib.shading() {
        lib.sync_joined(&scene);
    }
    assert!(lib.shaded_now(id));
}

/// D-e: a burst of mesh changes measures one shade at a time per clay,
/// not one per change.
#[test]
fn viewport_shade_jobs_run_one_at_a_time() {
    let c = crate::harness::figure(48, false);
    let (mut scene, id) = scene_with(c);
    let mut lib = MeshLib::new();
    lib.shade = crate::quality::Shade::Lumpiness;
    // The first measure is held while three edits come in (each builds
    // a new mesh).
    lib.hold_shades.store(true, std::sync::atomic::Ordering::Release);
    lib.sync_joined(&scene);
    assert_eq!(lib.shade_jobs, 1);
    for k in 0..3 {
        clay_mut(&mut scene, id).blobs[0].color = [0.2 + 0.1 * k as f32, 0.5, 0.5];
        lib.sync_joined(&scene);
    }
    assert_eq!(lib.shade_jobs, 1, "the newer meshes wait for the running measure");
    assert!(lib.shading() && !lib.shaded_now(id));
    lib.hold_shades.store(false, std::sync::atomic::Ordering::Release);
    while lib.shading() {
        lib.sync_joined(&scene);
    }
    assert!(lib.shaded_now(id));
    assert_eq!(lib.shade_jobs, 2, "the stale one, then the last mesh's");
}
