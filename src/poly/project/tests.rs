//! Stage 3c acceptance: projection on fixtures with known answers.

use glam::{Mat4, Vec3};
use serde_json::json;

use super::*;
use crate::clay::Clay;
use crate::poly::subdiv::{Refiner, evaluate};

/// A sphere of radius `r` as a quad cage: a cube's faces cut `n` by `n`,
/// pushed out onto the sphere (6 n² quads, wound outward).
pub(crate) fn cube_sphere(n: usize, r: f32) -> Poly {
    let mut index = std::collections::HashMap::new();
    let mut points = Vec::new();
    let mut id = |c: [usize; 3], points: &mut Vec<Vec3>| -> u32 {
        *index.entry(c).or_insert_with(|| {
            let p = Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) * (2.0 / n as f32) - Vec3::ONE;
            points.push(p.normalize() * r);
            points.len() as u32 - 1
        })
    };
    let mut faces = Vec::new();
    for a in 0..3 {
        let (u, v) = ((a + 1) % 3, (a + 2) % 3);
        for side in [0, n] {
            for i in 0..n {
                for j in 0..n {
                    let at = |di: usize, dj: usize| {
                        let mut c = [0usize; 3];
                        c[a] = side;
                        c[u] = i + di;
                        c[v] = j + dj;
                        c
                    };
                    let mut q = [at(0, 0), at(1, 0), at(1, 1), at(0, 1)].map(|c| id(c, &mut points));
                    if side == 0 {
                        q.reverse();
                    }
                    faces.push(q.to_vec());
                }
            }
        }
    }
    Poly::from_faces(&points, &faces).unwrap()
}

/// A clay from blobs at `resolution` with `detail_cell`.
pub(crate) fn clay(blobs: serde_json::Value, resolution: u32, detail_cell: Option<f32>) -> Clay {
    let mut v = json!({ "resolution": resolution, "blobs": blobs });
    if let Some(c) = detail_cell {
        v["detail_cell"] = json!(c);
    }
    serde_json::from_value(v).unwrap()
}

/// Sculpt strokes on a clay, as the tool runs them.
pub(crate) fn sculpt(c: &mut Clay, strokes: &[serde_json::Value]) {
    let ctx = crate::brush::resolve::ResolveCtx::local();
    for (_, r) in crate::sculpt::prepare(strokes, None, &ctx).unwrap() {
        crate::sculpt::run(c, &r).unwrap();
    }
}

/// A sphere of radius 0.5 with noise bumps all over.
pub(crate) fn noise_sphere() -> Clay {
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, Some(0.006));
    sculpt(&mut c, &[json!({ "brush": "noise", "point": [0, 0, 0], "radius": 0.9, "strength": 1.0, "scale": 0.06, "pattern": "bumps" })]);
    c
}

/// Projects `c` onto `p` (identity placement) and measures the finest level.
pub(crate) fn run(p: &Poly, c: &Clay, args: Args) -> (Poly, Projection, Measure) {
    let composed = crate::layers::composed_clay(c);
    let probe = crate::quality::Probe::new(&composed).unwrap();
    let cell = c.cell_size().unwrap();
    let target = Target { surface: &probe, to_clay: Mat4::IDENTITY, cell };
    let progress = crate::progress::Progress::default();
    let made = project(p, &target, &args, &progress).unwrap();
    let out = install(p, &made).unwrap();
    let r = Refiner::new(&out, args.levels).unwrap();
    let mesh = composed.build_mesh();
    let rest: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let m = measure(&made.points, &r.top.face_start, &r.top.corners, &r.top.edges, &target, Some(&rest), &progress).unwrap();
    (out, made, m)
}

/// The same measure for a Poly's level as it is (no projection).
pub(crate) fn measure_level(p: &Poly, level: u8, c: &Clay) -> Measure {
    let composed = crate::layers::composed_clay(c);
    let probe = crate::quality::Probe::new(&composed).unwrap();
    let target = Target { surface: &probe, to_clay: Mat4::IDENTITY, cell: c.cell_size().unwrap() };
    let mut q = p.clone();
    q.set_levels(q.level_count().max(level)).unwrap();
    let s = evaluate(&q, level).unwrap();
    let mesh = composed.build_mesh();
    let rest: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    measure(&s.points, &s.face_start, &s.corners, &s.edges, &target, Some(&rest), &Default::default()).unwrap()
}

fn args(levels: u8, mode: Mode, reach: f32) -> Args {
    Args { levels, mode, cone: DEFAULT_CONE, reach }
}

#[test]
#[ignore]
fn explore_projection() {
    let t = std::time::Instant::now();
    let c = noise_sphere();
    let p = cube_sphere(12, 0.5);
    eprintln!("clay {:.2}s, cell {}, cage {} faces, edge {}", t.elapsed().as_secs_f32(), c.cell_size().unwrap(), p.faces(), mean_edge(&p));
    let base = measure_level(&p, 4, &c);
    eprintln!("unprojected level 4: {base:?}");
    for mode in [Mode::Vector, Mode::Scalar] {
        let t = std::time::Instant::now();
        let (_, made, m) = run(&p, &c, args(4, mode, REACH_EDGES * mean_edge(&p)));
        eprintln!("{mode:?} {:.2}s: {m:?}", t.elapsed().as_secs_f32());
        for s in &made.stats {
            eprintln!("  {s:?}");
        }
    }
    // Noise free: the bands.
    let smooth = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, None);
    let (_, made, m) = run(&p, &smooth, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
    eprintln!("smooth: {m:?}");
    for s in &made.stats {
        eprintln!("  {s:?}");
    }
}

/// A retopology of `c` at `faces` quads (symmetry off).
pub(crate) fn retopo_cage(c: &Clay, faces: usize) -> Poly {
    crate::poly::retopo::tests::run_clay(c, &crate::poly::retopo::Args { faces, symmetry: crate::poly::retopo::Symmetry::Off, ..Default::default() }).poly
}

/// A cylinder (radius 0.3, height 0.6) and the same with a lip at its
/// top: radius 0.4 from height 0.1 up, so its underside overhangs the
/// wall by 0.1 facing down.
pub(crate) fn lip(with: bool) -> Clay {
    let profile = if with { json!([[0, -0.3], [0.3, -0.3], [0.3, 0.1], [0.4, 0.1], [0.4, 0.3], [0, 0.3]]) } else { json!([[0, -0.3], [0.3, -0.3], [0.3, 0.3], [0, 0.3]]) };
    clay(json!([{ "type": "lathe", "profile": profile }]), 96, None)
}




/// The plan's round trip on a sphere: a 864-quad cage of a noise-bumped
/// sphere, projected over 4 levels, lies within 0.5 cell of the clay with
/// normals within 4 degrees RMS (the plan's bounds); unprojected, level 4
/// is more than a cell off and 12 degrees out.
#[test]
fn noise_sphere_round_trip() {
    let c = noise_sphere();
    let p = cube_sphere(12, 0.5);
    let before = measure_level(&p, 4, &c);
    let (out, made, m) = run(&p, &c, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
    eprintln!("noise sphere: unprojected {before:?}\nprojected {m:?}\n{:?}", made.stats);
    assert!(before.hausdorff_cells > 1.0 && before.normal_rms_deg > 8.0, "the noise is there to project: {before:?}");
    assert!(m.hausdorff_cells <= 0.5 && m.coverage_cells.unwrap() <= 0.5, "{m:?}");
    assert!(m.rms_cells <= 0.05, "{m:?}");
    assert!(m.normal_rms_deg <= 4.0, "{m:?}");
    assert_eq!(made.stats.iter().map(|s| s.missed).sum::<usize>(), 0);
    // The installed Poly evaluates to the projected points exactly.
    assert!(evaluate(&out, 4).unwrap().points == made.points, "evaluation differs from the projection");
    assert_eq!(out.level, 4);
}

/// Each level holds only its band: on a noise-free sphere the RMS move
/// falls by about 4 per level (Catmull-Clark's shrinkage at half the
/// edge), and it falls on the noisy one too.
#[test]
fn project_bands() {
    let p = cube_sphere(12, 0.5);
    for (name, c) in [("smooth", clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, None)), ("noise", noise_sphere())] {
        let (_, made, _) = run(&p, &c, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
        let rms: Vec<f32> = made.stats.iter().map(|s| s.rms_cells).collect();
        eprintln!("{name}: rms by level {rms:?}");
        for w in rms.windows(2) {
            assert!(w[1] < w[0] * if name == "smooth" { 0.3 } else { 0.5 }, "{name}: {rms:?}");
        }
    }
}

/// An overhang (the lipped cylinder: the lip's underside faces down 0.1
/// out from the wall, 22 cells) reached by projecting a cage of the plain
/// cylinder: within reach, both modes put level 4 within a cell of the
/// clay both ways, since each level's normals turn toward it. With the
/// default reach (3 cage edges, less than the lip) the lip is missed and
/// counted.
#[test]
fn projection_reaches_an_overhang() {
    let p = retopo_cage(&lip(false), 4000);
    let c = lip(true);
    for mode in [Mode::Vector, Mode::Scalar] {
        let (_, made, m) = run(&p, &c, args(4, mode, 0.15));
        eprintln!("lip {mode:?}: {m:?}");
        assert!(m.hausdorff_cells <= 1.0 && m.coverage_cells.unwrap() <= 1.0, "{mode:?}: {m:?}");
        assert_eq!(made.stats.iter().map(|s| s.missed).sum::<usize>(), 0);
    }
    let (_, made, m) = run(&p, &c, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
    let missed: usize = made.stats.iter().map(|s| s.missed).sum();
    eprintln!("lip at the default reach: {missed} missed, {m:?}");
    assert!(missed > 1000 && m.coverage_cells.unwrap() > 5.0);
}

/// The plan's `project_round_trip`: a retopology (5k) of a sculpted head
/// fixture, projected over 4 levels: within 0.5 cell, normals within 4
/// degrees RMS.
#[test]
fn head_round_trip() {
    let mut c = crate::poly::retopo::tests::clay(crate::poly::retopo::tests::head(), 96);
    c.detail_cell = Some(0.006);
    sculpt(&mut c, &[
        json!({ "brush": "noise", "point": [0.0, 0.1, 0.35], "radius": 0.3, "strength": 1.0, "scale": 0.05, "pattern": "bumps" }),
        json!({ "brush": "standard", "points": [[0.2, 0.2, 0.25], [0.25, 0.1, 0.25]], "radius": 0.05, "strength": 0.5 }),
        json!({ "brush": "inflate", "points": [[-0.2, -0.1, 0.28]], "radius": 0.06, "strength": 0.5 }),
    ]);
    let p = retopo_cage(&c, 5000);
    let before = measure_level(&p, 4, &c);
    let t = std::time::Instant::now();
    let (_, made, m) = run(&p, &c, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
    eprintln!("head: {} faces, {:.2}s; unprojected {before:?}\nprojected {m:?}\n{:?}", p.faces(), t.elapsed().as_secs_f32(), made.stats);
    assert!(m.hausdorff_cells <= 0.5 && m.normal_rms_deg <= 4.0, "{m:?}");
    assert!(m.coverage_cells.unwrap() <= 0.5, "{m:?}");
    assert!(before.normal_rms_deg > m.normal_rms_deg * 2.0, "{before:?}");
}

/// The same bits on 1 and 12 threads (§0.2 rule 4).
#[test]
fn project_is_thread_independent() {
    let c = noise_sphere();
    let p = cube_sphere(8, 0.5);
    let hash = |threads: usize| crate::clay::with_threads(threads, || run(&p, &c, args(3, Mode::Vector, REACH_EDGES * mean_edge(&p))).0.hash());
    assert_eq!(hash(1), hash(12));
}

/// Detail is kept in each point's frame, so it moves with the cage
/// (the multires storage of Stage 3a): a rigid turn of the cage turns
/// the projected level 4 with it (to float rounding), and moving one cage
/// point leaves the detail heights elsewhere as they were and keeps them
/// correlated near it.
#[test]
fn detail_survives_a_cage_edit() {
    let c = noise_sphere();
    let p = cube_sphere(12, 0.5);
    let (out, made, _) = run(&p, &c, args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)));
    let turn = glam::Quat::from_euler(glam::EulerRot::XYZ, 0.7, -0.4, 1.1);
    let mut turned = out.clone();
    turned.cage = Chunked::new(&out.cage.iter().map(|q| turn * q).collect::<Vec<_>>());
    let after = evaluate(&turned, 4).unwrap().points;
    let worst = made.points.iter().zip(&after).map(|(a, b)| (turn * *a).distance(*b)).fold(0.0f32, f32::max);
    eprintln!("turned cage: level 4 within {worst:e} of the turned projection");
    assert!(worst < 1e-5, "{worst}");
    // Heights: each level-4 point's offset along its undisplaced normal.
    let heights = |q: &Poly| {
        let mut bare = q.clone();
        for l in &mut bare.levels {
            l.displacement = None;
        }
        let (a, b) = (evaluate(q, 4).unwrap(), evaluate(&bare, 4).unwrap());
        let n = b.normals();
        a.points.iter().zip(&b.points).zip(&n).map(|((x, y), n)| (*x - *y).dot(*n)).collect::<Vec<f32>>()
    };
    let h0 = heights(&out);
    let mut moved = out.clone();
    let edge = mean_edge(&p);
    let v = 100;
    moved.cage.set(v, out.cage.get(v) * (1.0 + 0.3 * edge / 0.5));
    let h1 = heights(&moved);
    let pos = evaluate(&moved, 0).unwrap().points[v];
    let at = evaluate(&out, 4).unwrap().points;
    let near: Vec<usize> = (0..at.len()).filter(|&i| at[i].distance(pos) < 2.0 * edge).collect();
    let far: Vec<usize> = (0..at.len()).filter(|&i| at[i].distance(pos) > 4.0 * edge).collect();
    let corr = |idx: &[usize]| {
        let (a, b): (Vec<f64>, Vec<f64>) = idx.iter().map(|&i| (h0[i] as f64, h1[i] as f64)).unzip();
        let (ma, mb) = (a.iter().sum::<f64>() / a.len() as f64, b.iter().sum::<f64>() / b.len() as f64);
        let cov: f64 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
        let va: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
        let vb: f64 = b.iter().map(|y| (y - mb).powi(2)).sum();
        cov / (va * vb).sqrt()
    };
    let far_diff = far.iter().map(|&i| (h0[i] - h1[i]).abs()).fold(0.0f32, f32::max);
    let (cn, cf) = (corr(&near), corr(&far));
    eprintln!("moved cage point: detail heights correlate {cn:.4} near it ({} points), {cf:.4} away (largest change {far_diff:e})", near.len());
    assert!(cn >= 0.98 && cf >= 0.9999, "{cn} {cf}");
}

/// A mirror image of a projected Poly keeps its detail: every level is
/// the source's reflected, point for point as sets (the finer levels'
/// numbering differs), within float rounding.
#[test]
fn mirrored_keeps_projected_detail() {
    let c = noise_sphere();
    let mut p = cube_sphere(6, 0.5);
    p.cage = Chunked::new(&p.cage.iter().map(|q| q + Vec3::new(0.3, 0.1, 0.0)).collect::<Vec<_>>());
    let target_clay = {
        let mut c = c.clone();
        for b in &mut c.blobs {
            let _ = b;
        }
        c
    };
    let composed = crate::layers::composed_clay(&target_clay);
    let probe = crate::quality::Probe::new(&composed).unwrap();
    // The clay sits at the origin; the cage 0.3 to the side: the clay is
    // read in the cage's space through the placement.
    let target = Target { surface: &probe, to_clay: Mat4::from_translation(Vec3::new(0.3, 0.1, 0.0)).inverse(), cell: c.cell_size().unwrap() };
    let made = project(&p, &target, &args(3, Mode::Vector, REACH_EDGES * mean_edge(&p)), &Default::default()).unwrap();
    let out = install(&p, &made).unwrap();
    let m = out.mirrored(0);
    assert!(m.levels.iter().all(|l| l.displacement.is_some()));
    for level in 1..=3 {
        let (a, b) = (evaluate(&out, level).unwrap().points, evaluate(&m, level).unwrap().points);
        let key = |q: Vec3| (q.x.to_bits(), q.y.to_bits(), q.z.to_bits());
        let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<Vec3>> = Default::default();
        let cell = 0.01;
        for q in &a {
            let r = Vec3::new(-q.x, q.y, q.z);
            grid.entry(((r.x / cell).floor() as i32, (r.y / cell).floor() as i32, (r.z / cell).floor() as i32)).or_default().push(r);
        }
        let mut worst = 0.0f32;
        for q in &b {
            let c0 = ((q.x / cell).floor() as i32, (q.y / cell).floor() as i32, (q.z / cell).floor() as i32);
            let mut best = f32::MAX;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for r in grid.get(&(c0.0 + dx, c0.1 + dy, c0.2 + dz)).into_iter().flatten() {
                            best = best.min(r.distance(*q));
                        }
                    }
                }
            }
            worst = worst.max(best);
        }
        let _ = key;
        eprintln!("mirrored level {level}: every point within {worst:e} of a reflected source point");
        assert!(worst < 1e-5, "level {level}: {worst}");
    }
    // Without carrying the displacement the copy would be the bare
    // levels: measurably off.
    let bare = Poly { levels: vec![crate::poly::Level::default(); 3], ..m.clone() };
    let (x, y) = (evaluate(&bare, 3).unwrap().points, evaluate(&m, 3).unwrap().points);
    assert!(x.iter().zip(&y).map(|(a, b)| a.distance(*b)).fold(0.0f32, f32::max) > 1e-3);
}



/// §6: projection of a 5k cage over 4 levels (1.3M points) in ≤ 8 s, a
/// 4K vector map in ≤ 6 s (`--ignored`; CLIDE_PERF=1 holds the bounds).
#[test]
#[ignore]
fn projection_perf() {
    let mut c = crate::poly::retopo::tests::clay(crate::poly::retopo::tests::head(), 160);
    c.detail_cell = Some(0.004);
    sculpt(&mut c, &[json!({ "brush": "noise", "point": [0.0, 0.1, 0.35], "radius": 0.4, "strength": 1.0, "scale": 0.04, "pattern": "bumps" })]);
    let p = retopo_cage(&c, 5000);
    let composed = crate::layers::composed_clay(&c);
    let probe = crate::quality::Probe::new(&composed).unwrap();
    let target = Target { surface: &probe, to_clay: Mat4::IDENTITY, cell: c.cell_size().unwrap() };
    let t = std::time::Instant::now();
    let made = project(&p, &target, &args(4, Mode::Vector, REACH_EDGES * mean_edge(&p)), &Default::default()).unwrap();
    let project_s = t.elapsed().as_secs_f32();
    let points: usize = made.stats.iter().map(|s| s.points).sum();
    let mut out = install(&p, &made).unwrap();
    out.uv = Some(Chunked::new(&crate::poly::uv::unwrap(&out, 4096, 4).unwrap().uv));
    let rest = composed.build_mesh();
    let progress = crate::progress::Progress::default();
    let low = crate::poly::low::Low::new(&out, 2, Mat4::IDENTITY, &rest, 4096, &progress).unwrap();
    let src = crate::bake::Source { clay: &composed, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None, low: Some(&low) };
    let t = std::time::Instant::now();
    let d = crate::bake::displacement(&src, &low.mesh, crate::bake::Maps { displacement: false, vector: true, size: 4096, object: false }, &progress).unwrap();
    let map_s = t.elapsed().as_secs_f32();
    eprintln!("projection: {} cage faces, {points} points over 4 levels in {project_s:.2}s (bound 8); 4K vector map over level 2 ({} triangles) in {map_s:.2}s (bound 6), {} missed", p.faces(), low.mesh.indices.len() / 3, d.missed);
    if std::env::var("CLIDE_PERF").is_ok() {
        assert!(project_s <= 8.0 && map_s <= 6.0);
    }
}

/// Cancel is honoured before the start and from another thread once the
/// bar passes 30% and 60% (every chunk of points checks; no timing is
/// asserted).
#[test]
fn project_cancel() {
    let c = noise_sphere();
    let p = cube_sphere(12, 0.5);
    let composed = crate::layers::composed_clay(&c);
    let probe = crate::quality::Probe::new(&composed).unwrap();
    let target = Target { surface: &probe, to_clay: Mat4::IDENTITY, cell: c.cell_size().unwrap() };
    let a = args(4, Mode::Vector, REACH_EDGES * mean_edge(&p));
    let before = crate::progress::Progress::default();
    before.cancel();
    assert_eq!(project(&p, &target, &a, &before).err().as_deref(), Some("cancelled"));
    for at in [0.3f32, 0.6] {
        let progress = std::sync::Arc::new(crate::progress::Progress::default());
        let watcher = {
            let progress = progress.clone();
            std::thread::spawn(move || {
                while progress.fraction() < at {
                    std::thread::yield_now();
                }
                progress.cancel();
            })
        };
        let r = project(&p, &target, &a, &progress);
        watcher.join().unwrap();
        assert_eq!(r.err().as_deref(), Some("cancelled"), "cancelled past {at}");
    }
}

/// Review fix (Stage 3c): a point with a front-facing normal near the
/// back of a thin plate (2 cells thick) lands on the front sheet, not on
/// the nearer back sheet, in both modes; the old search took the nearer
/// crossing either way (and the nearest point by |field|) and snapped
/// it through the plate.
#[test]
fn thin_plates_keep_their_side() {
    use crate::poly::project::{Found, Search, find};
    let (cell, half) = (0.01f32, 0.01f32);
    let plate = |q: Vec3| q.z.abs() - half;
    for mode in [Mode::Scalar, Mode::Vector] {
        let s = Search { mode, cos_cone: 45f32.to_radians().cos(), reach: 0.05, step: 0.25 * cell, tol: 1e-4 * cell, stretch: 1.0 };
        // Inside, 0.6 cells from the back and 1.4 from the front.
        let (q, how) = find(&plate, Vec3::new(0.0, 0.0, -0.004), Vec3::Z, &s);
        assert!((q.z - half).abs() < 1e-4 && how != Found::Missed, "{mode:?}: {q} {how:?}");
        // Outside behind the plate: the front faces +Z, so nothing within
        // reach faces that way except across the plate's front.
        let (q, _) = find(&plate, Vec3::new(0.0, 0.0, -0.012), Vec3::Z, &s);
        assert!(q.z > 0.0 || q.z == -0.012, "{mode:?}: never onto the back sheet: {q}");
        // A back-facing point keeps the back sheet.
        let (q, _) = find(&plate, Vec3::new(0.0, 0.0, -0.004), -Vec3::Z, &s);
        assert!((q.z + half).abs() < 1e-4, "{mode:?}: {q}");
    }
}

/// Sub-cell normal roughness (degrees RMS): each point's normal against
/// the mean of the normals around it within about a clay cell (Jacobi
/// passes over the level's edges until the kernel's spread is half a
/// cell). A smooth surface's normals barely change within a cell; steps
/// at the clay's sampling grid (a cell apart) do, so this is the level's
/// energy at the grid's frequency and above. Area weighted.
pub(crate) fn subcell_roughness(points: &[Vec3], face_start: &[u32], corners: &[u32], edges: &[[u32; 2]], cell: f32) -> f32 {
    let mut normal = vec![Vec3::ZERO; points.len()];
    for w in face_start.windows(2) {
        let face = &corners[w[0] as usize..w[1] as usize];
        let n = crate::poly::subdiv::newell(points, face);
        for &c in face {
            normal[c as usize] += n;
        }
    }
    let area: Vec<f32> = normal.iter().map(|n| n.length()).collect();
    let unit: Vec<Vec3> = normal.iter().map(|n| n.normalize_or_zero()).collect();
    let e = edges.iter().map(|[a, b]| points[*a as usize].distance(points[*b as usize]) as f64).sum::<f64>() / edges.len().max(1) as f64;
    let passes = ((cell as f64 / e).powi(2).ceil() as usize).clamp(1, 400);
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); points.len()];
    for [a, b] in edges {
        nb[*a as usize].push(*b);
        nb[*b as usize].push(*a);
    }
    let mut s = unit.clone();
    for _ in 0..passes {
        s = (0..s.len()).map(|v| if nb[v].is_empty() { s[v] } else { s[v] * 0.5 + nb[v].iter().map(|&u| s[u as usize]).sum::<Vec3>() * (0.5 / nb[v].len() as f32) }).collect();
    }
    let (mut sum, mut total) = (0.0f64, 0.0f64);
    for v in 0..points.len() {
        let a = area[v] as f64;
        let ang = unit[v].dot(s[v].normalize_or_zero()).clamp(-1.0, 1.0).acos() as f64;
        sum += a * ang * ang;
        total += a;
    }
    (sum / total.max(1e-30)).sqrt().to_degrees() as f32
}

/// Stage 3c follow-up: levels finer than the clay's cell do not copy the
/// steps of its sampling grid. On a smoothly sculpted sphere and on the
/// noise sphere (cell 0.006), projected over 5 levels (level 4 edges 0.6
/// of a cell, level 5 0.3), the finest level's sub-cell normal roughness
/// is within 20% of the same clay projected one level less and subdivided
/// once more (smooth by construction); before the band limit it was 1.5
/// times that. Levels whose edges are all a cell or more are not smoothed, the accuracy stays within 3c's bounds, and the
/// smoothing is the same on 1 and 12 threads.
#[test]
fn fine_levels_do_not_copy_grid_steps() {
    let p = cube_sphere(12, 0.5);
    let mut smooth = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }]), 96, Some(0.006));
    sculpt(&mut smooth, &[
        json!({ "brush": "inflate", "points": [[0.0, 0.0, 0.5]], "radius": 0.15, "strength": 0.6 }),
        json!({ "brush": "standard", "points": [[0.3, 0.3, 0.25], [0.35, 0.1, 0.33]], "radius": 0.08, "strength": 0.6 }),
        json!({ "brush": "inflate", "points": [[-0.3, -0.2, 0.3]], "radius": 0.1, "strength": 0.6 }),
    ]);
    let reach = REACH_EDGES * mean_edge(&p);
    for (name, c) in [("smooth", smooth), ("noise", noise_sphere())] {
        let cell = crate::quality::Probe::new(&crate::layers::composed_clay(&c)).unwrap().cell(Vec3::new(0.0, 0.0, 0.5));
        let (out, made, m) = run(&p, &c, args(5, Mode::Vector, reach));
        let passes: Vec<usize> = made.stats.iter().map(|s| s.smoothing_passes).collect();
        let r = Refiner::new(&out, 5).unwrap();
        let rough = subcell_roughness(&made.points, &r.top.face_start, &r.top.corners, &r.top.edges, cell);
        let mut coarse = run(&p, &c, args(4, Mode::Vector, reach)).0;
        coarse.set_levels(5).unwrap();
        let s = evaluate(&coarse, 5).unwrap();
        let reference = subcell_roughness(&s.points, &s.face_start, &s.corners, &s.edges, cell);
        eprintln!("{name}: level 5 sub-cell roughness {rough:.4} deg, projected to 4 and subdivided {reference:.4}; passes {passes:?}; {m:?}");
        // Levels 1 and 2 (edges 2.7 and 5.4 cells) are as before; level 3
        // (1.4 cells on average) has a few short edges near the cube's
        // corners, which take a few passes.
        assert_eq!(passes[..2], [0, 0], "{name}: levels with edges over a cell are as before");
        assert!(passes[3] > 0 && passes[4] > passes[3], "{name}: {passes:?}");
        assert!(rough <= 1.2 * reference, "{name}: {rough} against {reference}");
        assert!(m.hausdorff_cells <= 0.5 && m.normal_rms_deg <= 4.0 && m.coverage_cells.unwrap() <= 0.5, "{name}: {m:?}");
    }
    let c = noise_sphere();
    let hash = |threads: usize| crate::clay::with_threads(threads, || run(&p, &c, args(5, Mode::Vector, reach)).0.hash());
    assert_eq!(hash(1), hash(12));
}
