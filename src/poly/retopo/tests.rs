//! Stage 3b acceptance on synthetic shapes with known answers.

use glam::Vec3;

use super::*;
use crate::progress::Progress;

/// A clay from blobs at `resolution`.
pub(crate) fn clay(blobs: serde_json::Value, resolution: u32) -> crate::clay::Clay {
    serde_json::from_value(serde_json::json!({ "resolution": resolution, "blobs": blobs })).unwrap()
}

/// A clay's rest mesh welded and made manifold (as `poly from_clay`
/// makes it), as triangles.
pub(crate) fn welded(c: &crate::clay::Clay) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mesh = c.build_mesh();
    let points: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let m = crate::poly::from_mesh::from_triangles(&points, &mesh.indices, &[], 1e-5).unwrap();
    let (s, corners) = m.poly.cage_faces();
    let mut tris = Vec::new();
    for f in s.windows(2) {
        let face = &corners[f[0] as usize..f[1] as usize];
        for k in 1..face.len() - 1 {
            tris.push([face[0], face[k], face[k + 1]]);
        }
    }
    (m.poly.cage.to_vec(), tris)
}

/// Retopology of a clay with `args`, groups none.
pub(crate) fn run_clay(c: &crate::clay::Clay, args: &Args) -> Outcome {
    run_with(c, args, None, &Progress::default()).unwrap()
}

/// Retopology of a clay with `args`, groups and progress.
pub(crate) fn run_with(c: &crate::clay::Clay, args: &Args, groups: Option<&(dyn Fn(Vec3) -> u16 + Sync)>, progress: &Progress) -> Result<Outcome, String> {
    let (points, tris) = welded(c);
    let probe = crate::quality::Probe::new(c).unwrap();
    let input = Input { points: &points, tris: &tris, surface: &probe, cell: c.cell_size().unwrap(), groups };
    retopo(&input, args, progress)
}

fn report(name: &str, o: &Outcome) {
    eprintln!(
        "{name}: {} faces ({} before split), quads {}% before split, irregular {}% before / {}% after, valence {:?}, edge cv {}, hausdorff {:.3} (mean {:.3}) coverage {:.3} cells, open {} nm {} flipped {}, symmetric {} ({:e}), input {} pts, creases {}, singularities {}, {:?}, notes {:?}",
        o.shape.faces, o.before_split.faces, o.before_split.quads_pct, o.before_split.irregular_pct, o.shape.irregular_pct, o.before_split.valence, o.shape.edge_cv, o.hausdorff_cells, o.mean_distance_cells, o.coverage_cells,
        o.shape.open_edges, o.shape.non_manifold_edges, o.shape.flipped_edges, o.symmetric, o.symmetric_error, o.input_points, o.crease_edges, o.singularities, o.timings, o.notes
    );
}




/// The fixtures: name, blobs, resolution.
pub(crate) fn fixtures() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("sphere", serde_json::json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }])),
        ("torus", serde_json::json!([{ "type": "torus", "center": [0, 0, 0], "radius": 0.4, "thickness": 0.15 }])),
        ("box", serde_json::json!([{ "type": "box", "center": [0, 0, 0], "size": [0.8, 0.6, 0.5], "round": 0 }])),
        ("cylinder", serde_json::json!([{ "type": "lathe", "profile": [[0, -0.3], [0.3, -0.3], [0.3, 0.3], [0, 0.3]] }])),
        ("head", head()),
        ("sockets", sockets()),
        ("live", serde_json::json!([{"type":"ellipsoid","center":[0,0,0],"radii":[0.32,0.42,0.36]},{"type":"sphere","center":[0,-0.02,0.36],"radius":0.07,"blend":0.05},{"type":"ellipsoid","center":[0,0.12,0.27],"radii":[0.24,0.06,0.08],"blend":0.06},{"type":"sphere","center":[0.12,0.05,0.3],"radius":0.05,"op":"carve","blend":0.02},{"type":"sphere","center":[-0.12,0.05,0.3],"radius":0.05,"op":"carve","blend":0.02}])),
    ]
}

/// The head with two eye sockets carved.
pub(crate) fn sockets() -> serde_json::Value {
    let mut b = head().as_array().unwrap().clone();
    for x in [0.12, -0.12] {
        b.push(serde_json::json!({ "type": "sphere", "center": [x, 0.05, 0.3], "radius": 0.05, "op": "carve", "blend": 0.02 }));
    }
    serde_json::Value::Array(b)
}

/// An ellipsoid head with a nose, brow and chin (X-symmetric).
pub(crate) fn head() -> serde_json::Value {
    serde_json::json!([
        { "type": "ellipsoid", "center": [0, 0, 0], "radii": [0.32, 0.42, 0.36] },
        { "type": "sphere", "center": [0, -0.02, 0.36], "radius": 0.07, "blend": 0.05 },
        { "type": "ellipsoid", "center": [0, 0.12, 0.27], "radii": [0.24, 0.06, 0.08], "blend": 0.06 },
        { "type": "ellipsoid", "center": [0, -0.3, 0.2], "radii": [0.12, 0.08, 0.1], "blend": 0.08 },
    ])
}

#[test]
#[ignore]
fn explore_all() {
    let faces: Vec<usize> = std::env::var("RT_FACES").ok().map(|s| s.split(',').map(|x| x.parse().unwrap()).collect()).unwrap_or(vec![2000, 8000]);
    for (name, blobs) in fixtures() {
        let c = clay(blobs, 96);
        for &f in &faces {
            let t = std::time::Instant::now();
            let o = run_clay(&c, &Args { faces: f, ..Default::default() });
            eprintln!(
                "{name:9} {f:6}: {:6} faces ({:+5.1}%), quads {:6.2}%, irr {:5.2}%/{:5.2}%, cv {:.3}, hd {:.3} cov {:.3}, sym {}, sing {}, {:.2}s {:?} {:?}",
                o.shape.faces, (o.shape.faces as f64 / f as f64 - 1.0) * 100.0, o.before_split.quads_pct, o.before_split.irregular_pct, o.shape.irregular_pct, o.shape.edge_cv, o.hausdorff_cells, o.coverage_cells, o.symmetric, o.singularities, t.elapsed().as_secs_f32(), o.notes, o.timings.iter().map(|x| format!("{}:{:.2}", x.0, x.1)).collect::<Vec<_>>()
            );
        }
    }
}

/// The share of `lines`' length within `tol` of a cage edge running along
/// it (within about 25 degrees): where an edge loop follows the line.
fn on_edges(poly: &crate::poly::Poly, lines: &[(Vec3, Vec3)], tol: f32) -> f64 {
    let p = poly.cage.to_vec();
    let t = poly.topology();
    let size = tol.max(1e-4) * 4.0;
    let mut grid: std::collections::HashMap<[i32; 3], Vec<usize>> = std::collections::HashMap::new();
    for (e, &[a, b]) in t.edges.iter().enumerate() {
        let (pa, pb) = (p[a as usize], p[b as usize]);
        let lo = ((pa.min(pb) - tol) / size).floor().as_ivec3();
        let hi = ((pa.max(pb) + tol) / size).floor().as_ivec3();
        for x in lo.x..=hi.x {
            for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    grid.entry([x, y, z]).or_default().push(e);
                }
            }
        }
    }
    let (mut near, mut total) = (0.0f64, 0.0f64);
    for &(a, b) in lines {
        let n = ((a.distance(b) / (0.25 * tol)).ceil() as usize).max(1);
        let w = a.distance(b) as f64 / n as f64;
        for k in 0..n {
            let q = a.lerp(b, (k as f32 + 0.5) / n as f32);
            let cell = (q / size).floor().as_ivec3().to_array();
            let dir = (b - a).normalize_or_zero();
            let d = grid.get(&cell).into_iter().flatten().filter_map(|&e| {
                let [x, y] = t.edges[e];
                let along = (p[y as usize] - p[x as usize]).normalize_or_zero().dot(dir).abs() > 0.9;
                along.then(|| features::nearest_on(p[x as usize], p[y as usize], q).distance(q))
            }).fold(f32::INFINITY, f32::min);
            total += w;
            if d <= tol {
                near += w;
            }
        }
    }
    near / total.max(1e-30)
}

/// Watertight, manifold, wound one way, all quads after the split, the
/// genus kept.
fn assert_clean(name: &str, o: &Outcome, genus: i64) {
    let info = o.poly.info();
    assert_eq!((info["open_edges"].as_u64(), info["non_manifold_edges"].as_u64(), info["flipped_edges"].as_u64(), info["non_manifold_points"].as_u64()), (Some(0), Some(0), Some(0), Some(0)), "{name}: {info}");
    assert_eq!(info["genus"].as_i64(), Some(genus), "{name}: {info}");
    assert_eq!(info["quads_pct"].as_f64(), Some(100.0), "{name}: pure quads after the split");
    let cage = crate::poly::subdiv::evaluate(&o.poly, 0).unwrap();
    assert!(crate::mesh_asset::volume(&cage.points, &cage.triangles()) > 0.0, "{name}: wound outward");
}

/// The plan's `retopo_basic_shapes`: sphere, torus and an ellipsoid head.
#[test]
fn retopo_basic_shapes() {
    for (name, genus, irregular_max) in [("sphere", 0, 6.0), ("torus", 1, 6.0), ("head", 0, 12.0)] {
        let blobs = fixtures().into_iter().find(|f| f.0 == name).unwrap().1;
        let c = clay(blobs, 96);
        let o = run_clay(&c, &Args { faces: 8000, ..Default::default() });
        report(name, &o);
        assert_clean(name, &o, genus);
        let b = &o.before_split;
        assert!(b.quads_pct >= 92.0, "{name}: {}% quads before the split", b.quads_pct);
        assert!((o.shape.faces as f64 - 8000.0).abs() <= 0.15 * 8000.0, "{name}: {} faces for 8000", o.shape.faces);
        assert!(o.hausdorff_cells <= 0.75, "{name}: hausdorff {} cells", o.hausdorff_cells);
        assert!(o.coverage_cells <= 0.75, "{name}: coverage {} cells", o.coverage_cells);
        assert!(o.shape.irregular_pct <= irregular_max, "{name}: {}% irregular", o.shape.irregular_pct);
        let four = o.shape.valence.get(&4).copied().unwrap_or(0) as f64 / o.shape.valence.values().sum::<usize>() as f64;
        assert!(four >= 0.9, "{name}: {four} of the points have four edges");
        assert!(o.shape.edge_cv <= 0.2, "{name}: edge length cv {}", o.shape.edge_cv);
        assert!(o.symmetric && o.symmetric_error <= 1e-4, "{name}: symmetric {} ({})", o.symmetric, o.symmetric_error);
    }
}

/// The plan's `retopo_follows_creases`: a box with round 0 (≥ 95% of its
/// edges within 0.5 cell of a cage edge) and a lathed cylinder (its rims,
/// which surface nets leave a cell rough, within a cell).
#[test]
fn retopo_follows_creases() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "box").unwrap().1, 96);
    let cell = c.cell_size().unwrap();
    let o = run_clay(&c, &Args { faces: 3000, ..Default::default() });
    report("box", &o);
    assert_clean("box", &o, 0);
    let h = Vec3::new(0.4, 0.3, 0.25);
    let corner = |i: usize| Vec3::new(if i & 1 == 0 { -h.x } else { h.x }, if i & 2 == 0 { -h.y } else { h.y }, if i & 4 == 0 { -h.z } else { h.z });
    let edges: Vec<(Vec3, Vec3)> = (0..8).flat_map(|i| [1usize, 2, 4].into_iter().filter(move |b| i & b == 0).map(move |b| (corner(i), corner(i | b)))).collect();
    assert_eq!(edges.len(), 12);
    let share = on_edges(&o.poly, &edges, 0.5 * cell);
    eprintln!("box edges within 0.5 cell of a cage edge: {:.4}", share);
    assert!(share >= 0.95, "{share}");
    // Every corner is a cage point, and cage points near an edge are on it
    // (holds kept through every cleanup step that renumbers points).
    let cage = o.poly.cage.to_vec();
    for i in 0..8 {
        let d = cage.iter().map(|p| p.distance(corner(i))).fold(f32::INFINITY, f32::min);
        assert!(d < 1e-4, "corner {} is {d} from the cage", corner(i));
    }
    for p in &cage {
        let d = edges.iter().map(|&(a, b)| features::nearest_on(a, b, *p).distance(*p)).fold(f32::INFINITY, f32::min);
        assert!(d < 1e-4 || d > 0.3 * cell, "{p} is {d} from a box edge: near it, not on it");
    }
    assert!(o.hausdorff_cells <= 0.75, "{}", o.hausdorff_cells);
    let c = clay(fixtures().into_iter().find(|f| f.0 == "cylinder").unwrap().1, 96);
    let cell = c.cell_size().unwrap();
    let o = run_clay(&c, &Args { faces: 3000, ..Default::default() });
    report("cylinder", &o);
    assert_clean("cylinder", &o, 0);
    let rims: Vec<(Vec3, Vec3)> = [-0.3f32, 0.3]
        .iter()
        .flat_map(|&y| (0..256).map(move |k| {
            let (a, b) = (k as f32 / 256.0 * std::f32::consts::TAU, (k + 1) as f32 / 256.0 * std::f32::consts::TAU);
            (Vec3::new(0.3 * a.cos(), y, 0.3 * a.sin()), Vec3::new(0.3 * b.cos(), y, 0.3 * b.sin()))
        }))
        .collect();
    let share = on_edges(&o.poly, &rims, cell);
    eprintln!("cylinder rims within a cell of a cage edge: {:.4}", share);
    assert!(share >= 0.95, "{share}");
}

#[test]
#[ignore]
fn explore_one() {
    let name = std::env::var("RT_SHAPE").unwrap_or("sphere".into());
    let faces = std::env::var("RT_FACES").ok().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let res = std::env::var("RT_RES").ok().and_then(|s| s.parse().ok()).unwrap_or(96);
    let c = clay(fixtures().into_iter().find(|f| f.0 == name).unwrap().1, res);
    let mut args = Args { faces, ..Default::default() };
    if std::env::var("RT_EYE").is_ok() {
        args.guides = vec![vec![Vec3::new(0.2, 0.05, 0.33), Vec3::new(0.12, 0.13, 0.33), Vec3::new(0.04, 0.05, 0.34), Vec3::new(0.12, -0.03, 0.33), Vec3::new(0.2, 0.05, 0.33)]];
    }
    if std::env::var("RT_GUIDE").is_ok() {
        let probe = crate::quality::Probe::new(&c).unwrap();
        args.guides = vec![(0..=48)
            .map(|k| {
                let a = k as f32 / 48.0 * std::f32::consts::TAU;
                let p = Vec3::new(0.09 * a.cos(), -0.16 + 0.045 * a.sin(), 0.6);
                let f = Surface::near(&probe, p, 0.5);
                crate::adaptive::along(&*f, p, -Vec3::Z, 1e-5, 0.5)
            })
            .collect()];
    }
    let o = run_clay(&c, &args);
    report(&name, &o);
    eprintln!("info: {}", o.poly.info());
    {
        let t = o.poly.topology();
        let mut part = vec![usize::MAX; t.faces()];
        let mut parts: Vec<(usize, Vec3)> = Vec::new();
        let cage = o.poly.cage.to_vec();
        for f0 in 0..t.faces() {
            if part[f0] != usize::MAX { continue; }
            let id = parts.len();
            let mut stack = vec![f0];
            part[f0] = id;
            let (mut n, mut c) = (0, Vec3::ZERO);
            while let Some(f) = stack.pop() {
                n += 1;
                c += cage[t.face(f)[0] as usize];
                for k in t.face_start[f]..t.face_start[f + 1] {
                    for &g in t.faces_of_edge(t.corner_edge[k as usize] as usize) {
                        if part[g as usize] == usize::MAX { part[g as usize] = id; stack.push(g as usize); }
                    }
                }
            }
            parts.push((n, c / n as f32));
        }
        eprintln!("parts: {parts:?}");
    }
    let p = o.poly.cage.to_vec();
    let t = o.poly.topology();
    let probe = crate::quality::Probe::new(&c).unwrap();
    let cell = c.cell_size().unwrap();
    let mut worst: Vec<(f32, Vec3, usize)> = (0..t.faces()).map(|f| {
        let m = t.face(f).iter().map(|&v| p[v as usize]).sum::<Vec3>() / 4.0;
        (Surface::near(&probe, m, 0.1)(m).abs() / cell, m, f)
    }).collect();
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    eprintln!("worst faces: {:?}", &worst[..4]);
    let hi: Vec<(usize, Vec3)> = (0..t.vertices).filter(|&v| t.edges_of(v).len() > 6).map(|v| (t.edges_of(v).len(), p[v])).collect();
    eprintln!("high valence: {hi:?}");
}

/// The plan's `retopo_follows_guides`: a ring around the head's mouth
/// (laid on the surface, as an agent would from surface_point) gives an
/// edge loop within a cell along at least 85% of its length.
#[test]
fn retopo_follows_guides() {
    let c = clay(head(), 96);
    let cell = c.cell_size().unwrap();
    let probe = crate::quality::Probe::new(&c).unwrap();
    let ring: Vec<Vec3> = (0..=48)
        .map(|k| {
            let a = k as f32 / 48.0 * std::f32::consts::TAU;
            let p = Vec3::new(0.09 * a.cos(), -0.16 + 0.045 * a.sin(), 0.6);
            let f = Surface::near(&probe, p, 0.5);
            crate::adaptive::along(&*f, p, -Vec3::Z, 1e-5, 0.5)
        })
        .collect();
    assert!(ring.iter().all(|p| p.z > 0.2 && p.z < 0.45), "{ring:?}");
    let lines: Vec<(Vec3, Vec3)> = ring.windows(2).map(|w| (w[0], w[1])).collect();
    let plain = run_clay(&c, &Args { faces: 4000, ..Default::default() });
    let guided = run_clay(&c, &Args { faces: 4000, guides: vec![ring.clone()], ..Default::default() });
    report("head guided", &guided);
    assert_clean("head guided", &guided, 0);
    let (without, with) = (on_edges(&plain.poly, &lines, cell), on_edges(&guided.poly, &lines, cell));
    eprintln!("mouth ring within a cell of a cage edge: {without:.4} without the guide, {with:.4} with it");
    assert!(with >= 0.85, "{with}");
    assert!(with > without + 0.2, "the guide makes the loop ({without} -> {with})");
    assert!(guided.symmetric, "a symmetric guide keeps the cage symmetric");
}

/// The plan's `keep_group_borders`: a polygroup border (a tilted circle on
/// a sphere, where the plain field runs across it) gives an edge loop
/// within a cell along at least 85% of it.
#[test]
fn keep_group_borders() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "sphere").unwrap().1, 96);
    let cell = c.cell_size().unwrap();
    let axis = Vec3::new(0.0, 1.0, 0.5).normalize();
    let groups = move |p: Vec3| if p.dot(axis) > 0.1 { 1u16 } else { 2 };
    let (u, w) = axis.any_orthonormal_pair();
    let r = (0.25f32 - 0.01).sqrt();
    let at = |a: f32| axis * 0.1 + (u * a.cos() + w * a.sin()) * r;
    let border: Vec<(Vec3, Vec3)> = (0..256).map(|k| (at(k as f32 / 256.0 * std::f32::consts::TAU), at((k + 1) as f32 / 256.0 * std::f32::consts::TAU))).collect();
    let args = Args { faces: 4000, ..Default::default() };
    let kept = run_with(&c, &args, Some(&groups), &Progress::default()).unwrap();
    let ignored = run_with(&c, &Args { keep_group_borders: false, ..args.clone() }, Some(&groups), &Progress::default()).unwrap();
    report("sphere grouped", &kept);
    assert_clean("sphere grouped", &kept, 0);
    let (without, with) = (on_edges(&ignored.poly, &border, cell), on_edges(&kept.poly, &border, cell));
    eprintln!("group border within a cell of a cage edge along it: {without:.4} ignored, {with:.4} kept");
    assert!(with >= 0.85, "{with}");
    assert!(with > without + 0.2, "{without} -> {with}");
    // Groups that are not mirrored turn automatic symmetry off.
    let lopsided = |p: Vec3| if p.x > 0.2 { 1u16 } else { 2 };
    let o = run_with(&c, &Args { faces: 2000, ..Default::default() }, Some(&lopsided), &Progress::default()).unwrap();
    assert!(!o.symmetric, "lopsided groups are not mirrored");
}

/// The plan's `retopo_is_deterministic`: the same cage on 1 and 12
/// threads (§0.2 rule 4), with creases, symmetry and a guide.
#[test]
fn retopo_is_deterministic() {
    for name in ["head", "box"] {
        let c = clay(fixtures().into_iter().find(|f| f.0 == name).unwrap().1, 80);
        let args = Args { faces: 2000, guides: vec![vec![Vec3::new(-0.2, 0.0, 0.4), Vec3::new(0.2, 0.0, 0.4)]], ..Default::default() };
        let one = crate::clay::with_threads(1, || run_clay(&c, &args));
        let twelve = crate::clay::with_threads(12, || run_clay(&c, &args));
        assert_eq!(one.poly.hash(), twelve.poly.hash(), "{name}");
        assert_eq!((one.hausdorff_cells, one.coverage_cells, one.singularities), (twelve.hausdorff_cells, twelve.coverage_cells, twelve.singularities));
        eprintln!("{name}: {} faces, the same on 1 and 12 threads", one.shape.faces);
    }
}

/// The plan's `retopo_cancel`, without timing: a cancelled job stops at
/// its next check (one before it starts, and one cancelled from another
/// thread once the bar passes the field solve).
#[test]
fn retopo_cancel() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "sphere").unwrap().1, 64);
    let p = Progress::default();
    p.cancel();
    assert_eq!(run_with(&c, &Args::default(), None, &p).err().as_deref(), Some("cancelled"));
    for at in [0.3f32, 0.6, 0.8] {
        let p = std::sync::Arc::new(Progress::default());
        let watcher = {
            let p = p.clone();
            std::thread::spawn(move || {
                while p.fraction() < at {
                    std::thread::yield_now();
                }
                p.cancel();
            })
        };
        let r = run_with(&c, &Args::default(), None, &p);
        watcher.join().unwrap();
        assert_eq!(r.err().as_deref(), Some("cancelled"), "cancelled past {at}");
    }
}

/// §6: a 2M-triangle clay to 5k quads in at most 30 s and to 20k in at
/// most 45 s (as the job runs it: weld, remesh, fields, extraction, fit).
/// Timings are logged; the bounds hold with CLIDE_PERF=1 (3x slack
/// otherwise). `--ignored`: the clay alone takes a while to mesh.
#[test]
#[ignore]
fn retopo_perf() {
    let resolution: u32 = std::env::var("RT_RES").ok().and_then(|s| s.parse().ok()).unwrap_or(512);
    let name = std::env::var("RT_SHAPE").unwrap_or("cluster".into());
    let blobs = if name == "cluster" {
        // 3 x 3 x 2 balls joined by thin bars: about 2M triangles at 512.
        let mut b = Vec::new();
        for i in 0..18 {
            let c = [(i % 3) as f32 * 0.4 - 0.4, (i / 3 % 3) as f32 * 0.4 - 0.4, (i / 9) as f32 * 0.4 - 0.2];
            b.push(serde_json::json!({ "type": "sphere", "center": c, "radius": 0.15 }));
            if i % 3 != 2 {
                b.push(serde_json::json!({ "type": "capsule", "a": c, "b": [c[0] + 0.4, c[1], c[2]], "radius": 0.04, "blend": 0.03 }));
            }
        }
        serde_json::Value::Array(b)
    } else {
        fixtures().into_iter().find(|f| f.0 == name).unwrap().1
    };
    let c = clay(blobs, resolution);
    let t = std::time::Instant::now();
    let mesh = c.build_mesh();
    eprintln!("clay at {resolution}: {} triangles in {:.1} s", mesh.indices.len() / 3, t.elapsed().as_secs_f32());
    let probe = crate::quality::Probe::new(&c).unwrap();
    let cell = c.cell_size().unwrap();
    let mut cases = vec![(5000usize, 30_000.0), (20_000, 45_000.0)];
    if std::env::var("RT_LARGE").is_ok() {
        // The largest request (no bound in the plan: logged).
        cases.push((200_000, f64::INFINITY));
    }
    for (faces, bound) in cases {
        let t = std::time::Instant::now();
        let points: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
        let m = crate::poly::from_mesh::from_triangles(&points, &mesh.indices, &[], cell * 1e-3).unwrap();
        let (s, corners) = m.poly.cage_faces();
        let mut tris = Vec::new();
        for f in s.windows(2) {
            let face = &corners[f[0] as usize..f[1] as usize];
            for k in 1..face.len() - 1 {
                tris.push([face[0], face[k], face[k + 1]]);
            }
        }
        let pts = m.poly.cage.to_vec();
        let weld = t.elapsed().as_secs_f32();
        let input = Input { points: &pts, tris: &tris, surface: &probe, cell, groups: None };
        // Resident memory sampled while it runs (macOS/Linux `ps`), less
        // what the process held before: the retopology's own peak.
        let rss = || std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok()).unwrap_or(0);
        let base = rss();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sampler = {
            let done = done.clone();
            std::thread::spawn(move || {
                let mut most = 0;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    most = most.max(rss());
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                most
            })
        };
        let o = retopo(&input, &Args { faces, ..Default::default() }, &Progress::default()).unwrap();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let peak = sampler.join().unwrap();
        eprintln!("memory: {} MB before, {} MB at most during, {} MB for the retopology", base / 1024, peak / 1024, peak.saturating_sub(base) / 1024);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        report(&format!("{} triangles to {faces}", mesh.indices.len() / 3), &o);
        eprintln!("weld {weld:.2} s, total {:.2} s", ms / 1000.0);
        crate::harness::perf::check(&format!("retopo: {}M-triangle head to {faces} quads", (mesh.indices.len() / 3) as f64 / 1e6), ms, bound, serde_json::json!({ "triangles": mesh.indices.len() / 3, "faces": o.shape.faces, "timings": o.timings.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>() }));
    }
}

/// Review fix 1: `dissolve_two` renumbers points, and the per-point holds
/// follow them (`remap`): a point with two edges first in the list, so
/// every other point moves down one, keeps its hold with its position.
#[test]
fn dissolved_points_keep_their_holds() {
    use cleanup::{Hold, PolyMesh};
    // A 2 x 2 grid of quads folded into a closed box-like surface is
    // more than needed: two quads sharing two sides in a row around point
    // 0 (a valence-2 point), inside a strip of quads.
    let p = |x: f32, y: f32| Vec3::new(x, y, 0.0);
    let points = vec![p(1.0, 0.5), p(0.0, 0.0), p(1.0, 0.0), p(2.0, 0.0), p(2.0, 1.0), p(1.0, 1.0), p(0.0, 1.0)];
    // Faces 1-2-0-6 and 2-3-4-5-0 share sides 2-0 and 0-... no: point 0
    // sits on edges 2-0 and 0-5 only, between faces [1, 2, 0, 5, 6] and
    // [2, 3, 4, 5, 0].
    let mut m = PolyMesh { points: points.clone(), faces: vec![vec![1, 2, 0, 5, 6], vec![2, 3, 4, 5, 0]] };
    let hold: Vec<Hold> = (0..points.len() as u32).map(|v| if v == 3 { Hold::Fixed } else { Hold::Line(v) }).collect();
    // On an open strip the point is on the border: close it with a back.
    m.faces.push(vec![1, 6, 5, 4, 3, 2]);
    let map = cleanup::dissolve_two(&mut m);
    assert_eq!(map[0], u32::MAX, "the two-edge point went: {:?}", m.faces);
    let kept = cleanup::remap(&hold, &map, m.points.len(), Hold::Free);
    for (old, &new) in map.iter().enumerate() {
        if new != u32::MAX {
            assert_eq!(m.points[new as usize], points[old]);
            assert_eq!(kept[new as usize], hold[old], "point {old} keeps its hold");
        }
    }
    assert_eq!(kept.iter().filter(|h| **h == Hold::Fixed).count(), 1);
}

/// Review fix 3: a half's border points all go onto the plane, so the
/// mirror joins every one of them (no open seam).
#[test]
fn sealed_halves_mirror_closed() {
    use cleanup::PolyMesh;
    // Half of a box's surface (x >= 0), one border point a little off the
    // plane, as a fit could leave it.
    let pts = vec![
        Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.0), Vec3::new(0.001, 1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 1.0), Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.0, 1.0, 1.0),
    ];
    let faces = vec![vec![0, 3, 2, 1], vec![4, 5, 6, 7], vec![0, 1, 5, 4], vec![1, 2, 6, 5], vec![2, 3, 7, 6]];
    let half = PolyMesh { points: pts, faces };
    let open = |m: &PolyMesh| shape(&cleanup::mirror(m).0).open_edges;
    assert!(open(&half) > 0, "unsealed, the off-plane point leaves a seam");
    let mut sealed = half.clone();
    assert_eq!(cleanup::seal_plane(&mut sealed), 1);
    let whole = cleanup::mirror(&sealed).0;
    let s = shape(&whole);
    assert_eq!((s.open_edges, s.non_manifold_edges, s.flipped_edges), (0, 0, 0), "{s:?}");
}

/// Review fix 4: two hexagons that both offer the same diagonal do not
/// both take it (an edge in three faces).
#[test]
fn split_hexagons_share_no_diagonal() {
    use cleanup::PolyMesh;
    // Points 0 and 3 are opposite corners of both hexagons (a flat
    // double cover would be degenerate, so they sit on two sides of a
    // ridge), with the 0-3 diagonal the squarest split of each.
    let a = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let points = vec![a(0.0, 0.0, 0.0), a(0.5, -0.8, 0.2), a(1.5, -0.8, 0.2), a(2.0, 0.0, 0.0), a(0.5, 0.8, 0.2), a(1.5, 0.8, 0.2), a(1.5, -0.8, -0.2), a(0.5, -0.8, -0.2), a(1.5, 0.8, -0.2), a(0.5, 0.8, -0.2)];
    let mut m = PolyMesh { points, faces: vec![vec![0, 1, 2, 3, 5, 4], vec![3, 6, 7, 0, 9, 8]] };
    cleanup::split_large(&mut m);
    let t = m.topology();
    let (over, _) = t.non_manifold();
    assert_eq!(over, 0, "{:?}", m.faces);
    let diagonal = t.edge_between(0, 3).map(|e| t.faces_of_edge(e).len()).unwrap_or(0);
    assert!(diagonal <= 2, "the 0-3 diagonal is in {diagonal} faces");
}

/// Review fix 2: guides that would lay too many points on the surface are
/// refused before any work.
#[test]
fn oversized_guides_are_refused() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "sphere").unwrap().1, 48);
    let far = vec![vec![Vec3::new(-1e5, 0.0, 0.0), Vec3::new(1e5, 0.0, 0.0)]];
    let t = std::time::Instant::now();
    let err = run_with(&c, &Args { faces: 2000, guides: far, ..Default::default() }, None, &Progress::default()).err().unwrap();
    assert!(err.contains("bound"), "{err}");
    eprintln!("refused in {:?}: {err}", t.elapsed());
}

/// A guide ring tight around a carved eye socket (found live): holding
/// its full band tore the lattice, so the socket came out as closed
/// pieces of its own with the cage bridging over the hole. The retopology
/// notices the extra pieces and solves again with a narrower band.
#[test]
fn a_tight_guide_on_a_socket_keeps_one_piece() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "live").unwrap().1, 160);
    let ring = vec![Vec3::new(0.2, 0.05, 0.33), Vec3::new(0.12, 0.13, 0.33), Vec3::new(0.04, 0.05, 0.34), Vec3::new(0.12, -0.03, 0.33), Vec3::new(0.2, 0.05, 0.33)];
    let o = run_clay(&c, &Args { faces: 2000, guides: vec![ring], ..Default::default() });
    report("socket guided", &o);
    let info = o.poly.info();
    assert_eq!((info["components"].as_i64(), info["open_edges"].as_u64(), info["non_manifold_edges"].as_u64()), (Some(1), Some(0), Some(0)), "{info}");
    assert!(o.notes.iter().any(|n| n.contains("narrower band")), "{:?}", o.notes);
    assert!(o.coverage_cells < 3.0, "the sockets are covered: {}", o.coverage_cells);
}

/// A density region with scale 0.5 halves the edges near its middle: the
/// cage's edges there are clearly shorter than across the rest.
#[test]
fn density_regions_shorten_edges() {
    let c = clay(fixtures().into_iter().find(|f| f.0 == "sphere").unwrap().1, 80);
    let center = Vec3::new(0.0, 0.5, 0.0);
    let o = run_clay(&c, &Args { faces: 2000, density: vec![Density { center, radius: 0.3, scale: 0.5 }], ..Default::default() });
    assert_clean("sphere dense top", &o, 0);
    let p = o.poly.cage.to_vec();
    let t = o.poly.topology();
    let mean = |near: bool| {
        let l: Vec<f32> = t.edges.iter().filter(|&&[a, b]| (((p[a as usize] + p[b as usize]) * 0.5).distance(center) < 0.12) == near).map(|&[a, b]| p[a as usize].distance(p[b as usize])).collect();
        l.iter().sum::<f32>() / l.len() as f32
    };
    let (inside, outside) = (mean(true), mean(false));
    eprintln!("edges: {inside:.4} near the region's middle, {outside:.4} elsewhere");
    assert!(inside < 0.8 * outside, "{inside} vs {outside}");
}

/// The 3c backlog's defect: a crease stroke at strength 0.5 on the head
/// fixture leaves loose crumbs (tiny separate parts) in the clay's mesh.
fn crumbly_head() -> crate::clay::Clay {
    let mut c = clay(head(), 96);
    c.detail_cell = Some(0.006);
    crate::poly::project::tests::sculpt(&mut c, &[
        serde_json::json!({ "brush": "noise", "point": [0.0, 0.1, 0.35], "radius": 0.3, "strength": 1.0, "scale": 0.05, "pattern": "bumps" }),
        serde_json::json!({ "brush": "crease", "points": [[-0.15, 0.05, 0.33], [0.0, 0.0, 0.37], [0.15, 0.05, 0.33]], "radius": 0.05, "strength": 0.5 }),
        serde_json::json!({ "brush": "crease", "points": [[0.2, 0.2, 0.25], [0.25, 0.1, 0.25]], "radius": 0.04, "strength": 0.5 }),
    ]);
    c
}

/// Crumbs are dropped before remeshing (and said in the notes), so the
/// cage is one closed part; a real second part (a separate ball, larger
/// than a few faces) stays. (The fixture's cage still strays inside the
/// head where the crease cut the field: 13.7 cells at worst, from 21.7;
/// open, see the plan's Stage 4a paragraph.)
#[test]
fn loose_crumbs_are_dropped() {
    let c = crumbly_head();
    let (points, tris) = welded(&c);
    let parts = tri_parts(points.len(), &tris);
    assert!(parts > 1, "the fixture has crumbs ({parts} parts)");
    let o = run_clay(&c, &Args { faces: 5000, symmetry: Symmetry::Off, ..Default::default() });
    report("crumbly head", &o);
    assert!(o.notes.iter().any(|n| n.contains("loose part")), "{:?}", o.notes);
    assert_eq!(o.shape.open_edges, 0);
    assert_eq!(o.poly.topology().components(), 1);
    // Two balls: both stay.
    let two = clay(serde_json::json!([{ "type": "sphere", "center": [-0.3, 0, 0], "radius": 0.25 }, { "type": "sphere", "center": [0.35, 0, 0], "radius": 0.12 }]), 96);
    let o = run_clay(&two, &Args { faces: 2000, symmetry: Symmetry::Off, ..Default::default() });
    assert!(!o.notes.iter().any(|n| n.contains("loose part")), "{:?}", o.notes);
    assert_eq!(o.poly.topology().components(), 2);
}

/// 4a review fix: the crumb bound is 4 output faces' area, as documented
/// (it was 4 extracted faces, 16 output faces with `pure_quads`). A
/// tooth-sized separate ball of about 10 output faces' area beside a
/// larger one is kept.
#[test]
fn tooth_sized_parts_are_kept() {
    let faces = 2000;
    let (big, small) = (0.3f32, 0.022f32);
    let c = clay(serde_json::json!([{ "type": "sphere", "center": [0, 0, 0], "radius": big }, { "type": "sphere", "center": [0.38, 0, 0], "radius": small }]), 96);
    let (points, tris) = welded(&c);
    assert_eq!(tri_parts(points.len(), &tris), 2);
    let share = (small * small) / (big * big + small * small);
    let output_faces = share * faces as f32;
    assert!(output_faces > 2.0 * CRUMB_FACES && output_faces < 4.0 * CRUMB_FACES, "the tooth is {output_faces} output faces");
    let o = run_clay(&c, &Args { faces, symmetry: Symmetry::Off, ..Default::default() });
    report("tooth", &o);
    assert!(!o.notes.iter().any(|n| n.contains("loose part")), "{:?}", o.notes);
    assert!(o.poly.cage.iter().any(|p| p.x > 0.38 - 2.0 * small), "the tooth has cage points");
}

#[test]
#[ignore]
fn explore_crumbs() {
    let c = crumbly_head();
    for (k, faces) in [(40.0, 5000), (180.0, 5000), (40.0, 8000)] {
        let o = run_clay(&c, &Args { faces, symmetry: Symmetry::Off, keep_creases: k, ..Default::default() });
        report(&format!("crease {k} faces {faces}"), &o);
        let probe = crate::quality::Probe::new(&c).unwrap();
        let cell = c.cell_size().unwrap();
        let mut worst: Vec<(f32, Vec3)> = o.poly.cage.iter().map(|p| ((probe.near(p, 0.2))(p).abs() / cell, p)).collect();
        worst.sort_by(|a, b| b.0.total_cmp(&a.0));
        eprintln!("worst cage points: {:?}", &worst[..6]);
    }
}
