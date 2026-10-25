//! Stage 0 baselines: how well today's mesher matches an analytic sphere,
//! remesh timings (target/perf.json), the default-sphere image golden, and
//! the triangle-count finding.
//!
//! **Finding (triangle counts are not monotone in resolution).** Blobby
//! clay gives 52k triangles at res 96 but 23k at res 112, and 211k at 192
//! but 120k at 256. Neither slivers nor adaptive factors cause it: when
//! any surface-nets edge joins more than two quads, `build_window_cells`
//! throws the whole surface-nets mesh away and remeshes the object with
//! marching tetrahedra, which makes about 3.5 times as many triangles.
//! On the organic figure only 2-8 such edges trigger it, in mirrored
//! saddle cells where two sheets pass within a cell of each other (the
//! arm-torso creases near (+-0.20, 1.42, +-0.04), the elbows, the carved
//! eye sockets), and whether a resolution hits one is luck. Each method's
//! own counts are monotone. A second, deliberate jump is the hard-surface
//! floor: a hard clay left at `DEFAULT_RESOLUTION` (96) meshes at 192 or
//! more (`Clay::mesh_resolution`), so a hard figure at 96 gives 211k
//! triangles against 23k at 112.
//!
//! For Stage 6: chunked meshing must not reproduce a whole-object
//! fallback (a chunk would switch method on its own and the seams would
//! disagree), so its reference is a new monolithic anchored surface-nets
//! pass, not today's `build_window_cells` output, and ambiguous edges
//! must be resolved locally.

use std::time::Instant;

use glam::Vec3;
use serde_json::json;

use super::{figure, geom, image, perf, sphere};
use crate::clay::{Clay, TETRA_FALLBACKS};
use crate::mesh::{Mesh, MeshLib};
use crate::scene::{Scene, Shape};

/// Builds `clay`'s regular mesh; true if it fell back to marching
/// tetrahedra.
fn build(clay: &Clay) -> (Mesh, bool) {
    let before = TETRA_FALLBACKS.with(|n| n.get());
    let mesh = clay.build_mesh_with(&[], "");
    (mesh, TETRA_FALLBACKS.with(|n| n.get()) > before)
}

/// Resolutions the triangle-count checks sweep.
fn sweep() -> impl Iterator<Item = u32> {
    (64..=256).step_by(16)
}

/// Whether `counts` (in rising resolution) never drop by more than 15%.
fn monotone(counts: &[(u32, usize)]) -> Result<(), String> {
    for w in counts.windows(2) {
        let ((r0, a), (r1, b)) = (w[0], w[1]);
        if (b as f64) < a as f64 * 0.85 {
            return Err(format!("res {r0}: {a} triangles, res {r1}: {b}"));
        }
    }
    Ok(())
}

#[test]
fn sphere_matches_the_analytic_sphere() {
    let radius = 0.5;
    let clay = sphere(radius, 128);
    let cell = clay.cell_size().expect("not empty");
    let (mesh, fallback) = build(&clay);
    assert!(!fallback, "a sphere should mesh with surface nets");
    let e = geom::sdf_error(&mesh, |p| p.length() - radius, &geom::sphere_points(Vec3::ZERO, radius, 20_000));
    let truth = 4.0 / 3.0 * std::f64::consts::PI * f64::from(radius).powi(3);
    let volume_error = (geom::volume(&mesh) - truth).abs() / truth;
    let area_error = (geom::area(&mesh) - 4.0 * std::f64::consts::PI * f64::from(radius).powi(2)).abs() / (4.0 * std::f64::consts::PI * f64::from(radius).powi(2));
    let normals = geom::normal_error(&mesh, |p| p);
    eprintln!(
        "sphere res 128: hausdorff {:.3} cell (mesh->surface {:.3}, surface->mesh {:.3}, mean {:.3}), volume error {:.3}%, area error {:.3}%, normals mean {:.2} max {:.2} deg",
        e.hausdorff() / cell,
        e.mesh_to_surface / cell,
        e.surface_to_mesh / cell,
        e.mean / cell,
        volume_error * 100.0,
        area_error * 100.0,
        normals.mean_deg,
        normals.max_deg
    );
    assert!(e.hausdorff() <= 0.6 * cell, "Hausdorff {:.3} cells", e.hausdorff() / cell);
    assert!(volume_error <= 0.01, "volume off by {:.3}%", volume_error * 100.0);
    assert!(normals.max_deg <= 5.0, "{normals:?}");
}

/// The control: one convex blob, one meshing method, counts that grow
/// with the square of the resolution.
#[test]
fn sphere_triangle_count_is_monotone() {
    let counts: Vec<(u32, usize)> = sweep()
        .map(|res| {
            let (m, fallback) = build(&sphere(0.5, res));
            assert!(!fallback, "res {res}");
            (res, m.indices.len() / 3)
        })
        .collect();
    monotone(&counts).unwrap();
}

/// The organic figure's counts across res 64-256, with whether each build
/// fell back to marching tetrahedra.
fn organic_counts() -> Vec<(u32, usize, bool)> {
    sweep()
        .map(|res| {
            let (m, fallback) = build(&figure(res, false));
            (res, m.indices.len() / 3, fallback)
        })
        .collect()
}

/// The finding, pinned (see the module docs): split by meshing method, the
/// counts are monotone, and the jumps are all marching-tetrahedra
/// fallbacks. When the fallback goes away this fails; then un-ignore
/// `triangle_count_is_monotone_across_resolutions` and drop this.
#[test]
fn triangle_count_jumps_are_whole_mesh_tetra_fallbacks() {
    let counts = organic_counts();
    eprintln!("organic figure (res, triangles, tetra fallback): {counts:?}");
    let nets: Vec<(u32, usize)> = counts.iter().filter(|c| !c.2).map(|c| (c.0, c.1)).collect();
    let tetra: Vec<(u32, usize)> = counts.iter().filter(|c| c.2).map(|c| (c.0, c.1)).collect();
    monotone(&nets).unwrap_or_else(|e| panic!("surface nets alone: {e}"));
    monotone(&tetra).unwrap_or_else(|e| panic!("marching tetrahedra alone: {e}"));
    assert!(!tetra.is_empty(), "no build fell back to marching tetrahedra: the finding is resolved, see the doc comment");
    // The fallback is what breaks the plain sequence.
    let all: Vec<(u32, usize)> = counts.iter().map(|c| (c.0, c.1)).collect();
    assert!(monotone(&all).is_err());
    // Its meshes are about 3.5 times as dense (triangles per area in cells).
    let density = |res: u32, tris: usize| {
        let c = figure(res, false);
        tris as f64 * f64::from(c.cell_size().unwrap()).powi(2)
    };
    let mean = |v: &[(u32, usize)]| v.iter().map(|&(r, t)| density(r, t)).sum::<f64>() / v.len() as f64;
    let ratio = mean(&tetra) / mean(&nets);
    eprintln!("tetra / nets triangle density: {ratio:.2}");
    assert!((2.5..5.0).contains(&ratio), "{ratio}");
}

/// What Stage 6's chunked mesher depends on. Ignored: today's mesher
/// fails it on blobby clay because a few ambiguous surface-nets edges
/// switch the whole object to marching tetrahedra (module docs;
/// `triangle_count_jumps_are_whole_mesh_tetra_fallbacks` pins that).
/// Stage 6 measures its anchored surface-nets reference against this.
#[test]
#[ignore = "Stage 0 finding: whole-mesh marching-tetrahedra fallback on ambiguous surface-nets edges (see harness/baseline.rs)"]
fn triangle_count_is_monotone_across_resolutions() {
    let counts: Vec<(u32, usize)> = organic_counts().iter().map(|c| (c.0, c.1)).collect();
    monotone(&counts).unwrap();
}

/// The hard-surface floor: a hard clay at the default resolution meshes
/// at 192 cells or more. Logs both sides to perf.json.
#[test]
fn hard_surface_floor_raises_the_default_resolution() {
    let at_default = figure(crate::clay::DEFAULT_RESOLUTION, true);
    let explicit = figure(112, true);
    assert_eq!(at_default.mesh_resolution(), 192);
    assert_eq!(explicit.mesh_resolution(), 112);
    let ((floored, _), floored_ms) = perf::time(3, || build(&at_default));
    let ((plain, _), plain_ms) = perf::time(3, || build(&explicit));
    let (floor_ms, _) = {
        let t = Instant::now();
        let r = at_default.mesh_resolution();
        (t.elapsed().as_secs_f64() * 1000.0, r)
    };
    perf::log("remesh hard figure res 96 (floored to 192)", floored_ms, json!({ "triangles": floored.indices.len() / 3, "mesh_resolution_ms": floor_ms }));
    perf::log("remesh hard figure res 112", plain_ms, json!({ "triangles": plain.indices.len() / 3 }));
    assert!(floored.indices.len() > plain.indices.len() * 4);
}

/// Full remesh timings of today's mesher (the numbers Stage 6's dirty-chunk
/// remesh is measured against).
#[test]
fn remesh_baselines() {
    for res in [64, 128, 192, 256] {
        let c = figure(res, false);
        let ((m, fallback), ms) = perf::time(3, || build(&c));
        perf::log(&format!("remesh organic figure res {res}"), ms, json!({ "triangles": m.indices.len() / 3, "tetra_fallback": fallback }));
    }
    let c = sphere(0.5, 128);
    let ((m, _), ms) = perf::time(3, || build(&c));
    // Loose: about 70 ms on an M2 Max with the suite running alongside.
    perf::check("remesh sphere res 128", ms, 400.0, json!({ "triangles": m.indices.len() / 3 }));
}

/// The default clay sphere (one sphere blob at the default resolution),
/// on its own, rendered 512 x 512 from `View::FRONT`. `CLIDE_GPU=1` only.
#[test]
fn golden_default_sphere() {
    if !image::gpu_enabled() {
        eprintln!("golden_default_sphere: set CLIDE_GPU=1 to run");
        return;
    }
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.color = [0.85, 0.72, 0.55];
    o.clay = Some(sphere(0.5, crate::clay::DEFAULT_RESOLUTION));
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    let img = image::render(&scene, &meshes, image::View::FRONT).unwrap_or_else(|e| panic!("CLIDE_GPU=1 but: {e}"));
    image::assert_golden("default_sphere", &img, 1.5, 6);
}
