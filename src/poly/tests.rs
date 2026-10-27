//! Stage 3a's known answers: the cube's limit surface, Euler
//! characteristic and watertightness kept, DeRose's semi-sharp rules,
//! borders, bounds, thread independence, multires detail, save and load.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::Vec3;
use serde_json::json;

use super::subdiv::{Refiner, evaluate, limit, to_tangent};
use super::topology::Topology;
use super::*;

/// The cube [-1, 1]³, faces wound outward.
pub fn cube() -> Poly {
    let points: Vec<Vec3> = (0..8).map(|i| Vec3::new(if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 })).collect();
    let faces = vec![vec![0, 4, 6, 2], vec![1, 3, 7, 5], vec![0, 1, 5, 4], vec![2, 6, 7, 3], vec![0, 2, 3, 1], vec![4, 5, 7, 6]];
    Poly::from_faces(&points, &faces).unwrap()
}

/// A torus of `n` × `m` quads (genus 1).
pub fn torus(n: u32, m: u32) -> Poly {
    let mut points = Vec::new();
    for i in 0..n {
        for j in 0..m {
            let (a, b) = (i as f32 / n as f32 * std::f32::consts::TAU, j as f32 / m as f32 * std::f32::consts::TAU);
            points.push(Vec3::new((1.0 + 0.4 * b.cos()) * a.cos(), 0.4 * b.sin(), (1.0 + 0.4 * b.cos()) * a.sin()));
        }
    }
    let at = |i: u32, j: u32| (i % n) * m + (j % m);
    let faces: Vec<Vec<u32>> = (0..n).flat_map(|i| (0..m).map(move |j| vec![at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i + 1, j)])).collect();
    Poly::from_faces(&points, &faces).unwrap()
}

/// A pentagonal prism (two 5-gons and five quads) and a tetrahedron
/// (triangles), as one mesh of two parts.
fn ngons() -> Poly {
    let mut points: Vec<Vec3> = (0..5).map(|i| { let a = i as f32 * std::f32::consts::TAU / 5.0; Vec3::new(a.cos(), -0.5, a.sin()) }).collect();
    points.extend((0..5).map(|i| { let a = i as f32 * std::f32::consts::TAU / 5.0; Vec3::new(a.cos(), 0.5, a.sin()) }));
    let mut faces = vec![vec![0, 1, 2, 3, 4], vec![9, 8, 7, 6, 5]];
    faces.extend((0..5u32).map(|i| vec![(i + 1) % 5, i, i + 5, (i + 1) % 5 + 5]));
    let o = points.len() as u32;
    points.extend([Vec3::new(3.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0), Vec3::new(3.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 1.0)]);
    faces.extend([vec![o, o + 2, o + 1], vec![o, o + 1, o + 3], vec![o, o + 3, o + 2], vec![o + 1, o + 2, o + 3]]);
    Poly::from_faces(&points, &faces).unwrap()
}

/// An open `n` × `n` grid of quads on [0, n]² at z = 0.
fn grid(n: u32) -> Poly {
    let points: Vec<Vec3> = (0..=n).flat_map(|y| (0..=n).map(move |x| Vec3::new(x as f32, y as f32, 0.0))).collect();
    let at = |x: u32, y: u32| y * (n + 1) + x;
    let faces: Vec<Vec<u32>> = (0..n).flat_map(|y| (0..n).map(move |x| vec![at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1)])).collect();
    Poly::from_faces(&points, &faces).unwrap()
}

/// `p` keeping `n` levels (past the tool's 6 when the faces fit).
fn with_levels(mut p: Poly, n: u8) -> Poly {
    check_faces(p.faces() as u64, p.corners.len() as u64, n).unwrap();
    p.levels = vec![Level::default(); n as usize];
    p
}

fn creased(p: Poly, s: f32) -> Poly {
    let t = Topology::build(p.cage.len(), p.face_start.to_vec(), p.corners.to_vec());
    let creases: BTreeMap<(u32, u32), f32> = t.edges.iter().map(|e| ((e[0], e[1]), s)).collect();
    Poly { creases: Arc::new(creases), ..p }
}

fn max_dist(a: &[Vec3], b: &[Vec3]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x.distance(*y)).fold(0.0, f32::max)
}

/// The cube's limit surface at three of its points, worked out by hand
/// from Halstead's masks at level 1 (where they are exact): a corner
/// (valence 3) at (1/2, 1/2, 1/2), a face's middle at (68/81, 0, 0) and an
/// edge's middle at (395/648, 395/648, 0). Level 5 is within 1e-3 of the
/// limit everywhere, level 8 within 1e-4 of the limit masks, and the limit
/// masks agree with level 8 at every level-5 point.
#[test]
fn cc_cube_converges_to_known_limit() {
    let cube = with_levels(cube(), 8);
    let t0 = Topology::build(8, cube.face_start.to_vec(), cube.corners.to_vec());
    // +x is face 1; the edge (1,1,-1)-(1,1,1) joins points 3 and 7.
    let face_x = 8 + 1;
    let edge = 8 + 6 + t0.edge_between(3, 7).unwrap();
    let known = [(7usize, Vec3::splat(0.5)), (face_x, Vec3::new(68.0 / 81.0, 0.0, 0.0)), (edge, Vec3::new(395.0 / 648.0, 395.0 / 648.0, 0.0))];
    let lim1 = limit(&cube, 1).unwrap();
    for (i, want) in known {
        assert!(lim1[i].distance(want) < 1e-6, "limit mask at {i}: {} vs {want}", lim1[i]);
    }
    let l5 = evaluate(&cube, 5).unwrap().points;
    let l8 = evaluate(&cube, 8).unwrap().points;
    for (i, want) in known {
        assert!(l5[i].distance(want) < 1e-3, "level 5 at {i}: {} vs {want}", l5[i]);
        assert!(l8[i].distance(want) < 1e-4, "level 8 at {i}: {} vs {want}", l8[i]);
    }
    let lim5 = limit(&cube, 5).unwrap();
    let to_limit = max_dist(&l5, &lim5);
    let masks_vs_l8 = max_dist(&lim5, &l8[..lim5.len()]);
    // The radius profile (center to surface) along every level-5 point.
    let profile = l5.iter().zip(&l8).map(|(a, b)| (a.length() - b.length()).abs()).fold(0.0, f32::max);
    eprintln!("cube: level 5 to its limit {to_limit:.2e}, limit masks vs level 8 {masks_vs_l8:.2e}, radius profile level 5 vs 8 {profile:.2e}; corner {} face {} edge {}", l8[7], l8[face_x], l8[edge]);
    assert!(to_limit < 1e-3 && profile < 1e-3);
    assert!(masks_vs_l8 < 1e-4);
    // Symmetric: every corner at the same radius.
    let r: Vec<f32> = (0..8).map(|i| l8[i].length()).collect();
    assert!(r.iter().all(|x| (x - r[0]).abs() < 1e-6));
}

/// V − E + F is kept at every level (V' = V + E + F, E' = 2E + C, F' = C),
/// every edge of a closed mesh stays in two faces wound opposite ways, and
/// an open grid keeps its one boundary loop and χ = 1.
#[test]
fn euler_characteristic_and_watertightness_are_kept() {
    for (name, poly, chi, loops) in [("cube", cube(), 2, 0), ("torus", torus(8, 6), 0, 0), ("n-gons", ngons(), 4, 0), ("grid", grid(3), 1, 1)] {
        let r = Refiner::new(&poly, 4).unwrap();
        let mut tops = r.topology.iter().map(|t| (t.vertices, t.edges.len(), t.faces(), t.non_manifold(), t.boundary_loops(), t.components())).collect::<Vec<_>>();
        let top = r.top_topology();
        tops.push((top.vertices, top.edges.len(), top.faces(), top.non_manifold(), top.boundary_loops(), top.components()));
        let counts = subdiv::counts(&r.topology[0], 4);
        for (k, (v, e, f, bad, b, parts)) in tops.into_iter().enumerate() {
            assert_eq!(v as i64 - e as i64 + f as i64, chi, "{name} level {k}");
            assert_eq!(bad, (0, 0), "{name} level {k}: non-manifold or flipped edges");
            assert_eq!(b, loops, "{name} level {k}: boundary loops");
            assert_eq!(parts, if name == "n-gons" { 2 } else { 1 });
            assert_eq!((counts[k].vertices, counts[k].edges, counts[k].faces), (v as u64, e as u64, f as u64));
        }
        // Watertight as triangles, too.
        let s = evaluate(&with_levels(poly, 3), 3).unwrap();
        assert_eq!(crate::mesh_asset::watertight(&s.points, &s.triangles()), loops == 0, "{name}");
    }
}

/// Sharpness 10 keeps the cube a cube at level 4 (every point on its
/// faces, the 12 edges straight); sharper is never rounder; sharpness 1
/// gives the midpoint at level 1; 0.5 the halfway blend; children lose one
/// per level and infinite never decays; and sharpness 2 is exactly two
/// sharp levels then smooth ones (DeRose et al.'s semi-sharp rule).
#[test]
fn creases_follow_the_semi_sharp_rules() {
    let hard = with_levels(creased(cube(), INFINITE_SHARPNESS), 6);
    let s4 = evaluate(&hard, 4).unwrap().points;
    let off_faces = s4.iter().map(|p| (p.abs().max_element() - 1.0).abs()).fold(0.0, f32::max);
    let on_edges = s4.iter().filter(|p| p.abs().to_array().iter().filter(|c| (**c - 1.0).abs() < 1e-6).count() >= 2).count();
    eprintln!("sharpness 10 at level 4: off the faces by {off_faces:.1e}, {on_edges} points on the edges");
    assert!(off_faces <= 1e-6);
    assert_eq!(on_edges, 8 + 12 * 15, "every edge point stays on its edge line");
    let r = Refiner::new(&hard, 6).unwrap();
    assert!(r.top.sharpness[..24].iter().all(|s| s.is_infinite()), "infinite sharpness never decays");
    // The cage edge (3, 7)'s point, at level 4, by sharpness.
    let t0 = Topology::build(8, hard.face_start.to_vec(), hard.corners.to_vec());
    let e = 8 + 6 + t0.edge_between(3, 7).unwrap();
    let mid = |s: f32| evaluate(&with_levels(creased(cube(), s), 4), 4).unwrap().points[e].length();
    let by: Vec<f32> = [0.0, 0.5, 1.0, 2.0, 3.0, 10.0].iter().map(|&s| mid(s)).collect();
    eprintln!("edge middle radius by sharpness 0, 0.5, 1, 2, 3, 10: {by:?}");
    assert!(by.windows(2).all(|w| w[0] < w[1]), "sharper is never rounder");
    assert!((by[5] - 2f32.sqrt()).abs() < 1e-6);
    // Level 1, one edge creased: sharpness 1 is the midpoint, 0.5 the blend.
    let one = |s: f32| {
        let mut c = cube();
        c.creases = Arc::new([((3, 7), s)].into_iter().collect());
        evaluate(&with_levels(c, 1), 1).unwrap().points[e]
    };
    assert!(one(1.0).distance(Vec3::new(1.0, 1.0, 0.0)) < 1e-7);
    assert!(one(0.5).distance((one(0.0) + Vec3::new(1.0, 1.0, 0.0)) * 0.5) < 1e-7);
    // Child sharpness: 2.5 -> 1.5 -> 0.5 -> 0.
    let mut c = cube();
    c.creases = Arc::new([((3, 7), 2.5)].into_iter().collect());
    let r = Refiner::new(&c, 4).unwrap();
    let e0 = t0.edge_between(3, 7).unwrap();
    assert_eq!(r.topology[0].sharpness[e0], 2.5);
    assert_eq!((r.topology[1].sharpness[2 * e0], r.topology[1].sharpness[2 * e0 + 1]), (1.5, 1.5));
    // Sharpness 2: two levels with the sharp rules, then smooth ones.
    let two = evaluate(&with_levels(creased(cube(), 2.0), 3), 3).unwrap().points;
    let sharp2 = evaluate(&with_levels(creased(cube(), INFINITE_SHARPNESS), 2), 2).unwrap();
    let again = Poly::new(&sharp2.points, &sharp2.face_start, &sharp2.corners).unwrap();
    let smooth1 = evaluate(&with_levels(again.clone(), 1), 1).unwrap().points;
    // Points and face points line up; edge points by their edge (the
    // cage's edges are sorted, the refined level's are not).
    let (v2, f2) = (sharp2.points.len(), sharp2.face_start.len() - 1);
    let t2 = Topology::build(v2, again.face_start.to_vec(), again.corners.to_vec());
    let mut d = max_dist(&two[..v2 + f2], &smooth1[..v2 + f2]);
    for (e, &[a, b]) in sharp2.edges.iter().enumerate() {
        let k = t2.edge_between(a, b).unwrap();
        d = d.max(two[v2 + f2 + e].distance(smooth1[v2 + f2 + k]));
    }
    eprintln!("sharpness 2 at level 3 vs sharp, sharp, smooth: {d:.1e}");
    assert!(d < 1e-6);
}

/// Borders (D16 "edges and corners"): an open grid's four corners stay,
/// its sides stay straight and it stays flat.
#[test]
fn borders_keep_edges_and_corners() {
    let g = with_levels(grid(3), 3);
    let p = evaluate(&g, 3).unwrap().points;
    for c in [Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), Vec3::new(0.0, 3.0, 0.0), Vec3::new(3.0, 3.0, 0.0)] {
        assert!(p.contains(&c), "corner {c} kept");
    }
    assert!(p.iter().all(|q| q.z == 0.0 && q.x >= 0.0 && q.x <= 3.0 && q.y >= 0.0 && q.y <= 3.0));
    let t = Refiner::new(&g, 3).unwrap().top_topology();
    let on_side = |q: Vec3| q.x == 0.0 || q.x == 3.0 || q.y == 0.0 || q.y == 3.0;
    for e in (0..t.edges.len()).filter(|&e| t.is_border(e)) {
        let [a, b] = t.edges[e];
        assert!(on_side(p[a as usize]) && on_side(p[b as usize]), "border edge {e} off the sides");
    }
}

/// The same bits on 1 and 12 threads, with creases and displacement.
#[test]
fn levels_are_thread_independent() {
    let mut p = with_levels(torus(24, 16), 3);
    let mut creases = BTreeMap::new();
    for i in 0..24u32 {
        let (a, b) = (i * 16, ((i + 1) % 24) * 16);
        creases.insert((a.min(b), a.max(b)), [0.5, 1.5, 10.0][i as usize % 3]);
    }
    p.creases = Arc::new(creases);
    let n3 = evaluate(&p, 3).unwrap().points.len();
    p.levels[2].displacement = Some(Chunked::new(&(0..n3).map(|i| Vec3::new((i as f32 * 0.37).sin(), (i as f32 * 0.11).cos(), (i as f32 * 0.07).sin()) * 0.01).collect::<Vec<_>>()));
    let run = || evaluate(&p, 3).unwrap().points.iter().flat_map(|v| v.to_array().map(f32::to_bits)).collect::<Vec<u32>>();
    let one = crate::clay::with_threads(1, run);
    let many = crate::clay::with_threads(12, run);
    assert_eq!(one, many);
}

/// Levels past the 16M-face bound are refused from the counts alone.
#[test]
fn subdivide_bounds() {
    // 4,000 quads: 5 levels make 4,096,000 faces, 6 levels 16,384,000.
    let g = grid(1);
    let mut faces = Vec::new();
    for _ in 0..4000 {
        faces.extend_from_slice(&[0u32, 1, 3, 2]);
    }
    let starts: Vec<u32> = (0..=4000).map(|i| 4 * i).collect();
    let mut big = Poly::new(&g.cage.to_vec(), &starts, &faces).unwrap();
    assert!(big.set_levels(5).is_ok());
    let err = big.set_levels(6).unwrap_err();
    assert!(err.contains("16384000") && err.contains("at most 5 levels"), "{err}");
    assert_eq!(big.level_count(), 5, "a refused call changes nothing");
    assert!(Refiner::new(&big, 6).is_err());
    assert!(check_levels(MAX_CAGE_FACES as u64, MAX_CAGE_CORNERS as u64, 6).is_err());
    assert_eq!(faces_at(u64::MAX, u64::MAX, 6), u64::MAX, "saturates");
    // Bad cages.
    let p = [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z];
    assert!(Poly::from_faces(&p, &[vec![0, 1]]).is_err());
    assert!(Poly::from_faces(&p, &[vec![0, 1, 1]]).is_err());
    assert!(Poly::from_faces(&p, &[vec![0, 1, 9]]).is_err());
    assert!(Poly::from_faces(&[Vec3::NAN, Vec3::X, Vec3::Y], &[vec![0, 1, 2]]).is_err());
    assert!(Poly::from_faces(&p, &[]).is_err());
}

/// Level-3 detail of `poly` (stored in tangent space) after `edit`
/// moves its cage: the correlation of the detail read in the moved
/// level's frames with the detail stored, kept in tangent space and kept
/// as world-space offsets, over the points that moved (and whose normal
/// turned at least `turned` degrees); and the most a normal turned there.
fn detail_after(poly: &Poly, turned: f32, edit: impl Fn(&mut Poly)) -> (f64, f64, f32, usize) {
    let n = evaluate(poly, 3).unwrap().points.len();
    let detail: Vec<Vec3> = (0..n).map(|i| { let x = i as f32; Vec3::new((x * 0.71).sin(), (x * 1.37).cos(), (x * 0.53).sin()) * 0.02 }).collect();
    let mut poly = poly.clone();
    poly.levels[2].displacement = Some(Chunked::new(&detail));
    let plain = |p: &Poly| { let mut q = p.clone(); q.levels[2].displacement = None; q };
    let r = Refiner::new(&poly, 3).unwrap();
    let frames_of = |pts: &[Vec3]| subdiv::frames(pts, &r.top.face_start, &r.top.corners);
    let before = evaluate(&plain(&poly), 3).unwrap().points;
    let f0 = frames_of(&before);
    let world: Vec<Vec3> = f0.iter().zip(&detail).map(|(f, d)| f[0] * d.x + f[1] * d.y + f[2] * d.z).collect();
    // to_tangent inverts the frames.
    assert!(max_dist(&to_tangent(&poly, 3, &world).unwrap(), &detail) < 1e-5);
    let mut moved = poly.clone();
    edit(&mut moved);
    let after = evaluate(&moved, 3).unwrap().points;
    let after_base = evaluate(&plain(&moved), 3).unwrap().points;
    let f1 = frames_of(&after_base);
    let read = |offsets: &[Vec3]| -> Vec<Vec3> { offsets.iter().zip(&f1).map(|(w, f)| Vec3::new(w.dot(f[0]), w.dot(f[1]), w.dot(f[2]))).collect() };
    let tangent = read(&after.iter().zip(&after_base).map(|(a, b)| *a - *b).collect::<Vec<_>>());
    let worldly = read(&world);
    let moved_at: Vec<usize> = (0..n).filter(|&i| before[i].distance(after_base[i]) > 1e-4 && f0[i][2].angle_between(f1[i][2]).to_degrees() >= turned).collect();
    let corr = |got: &[Vec3]| { let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64); for &i in &moved_at { let (a, b) = (detail[i].as_dvec3(), got[i].as_dvec3()); ab += a.dot(b); aa += a.dot(a); bb += b.dot(b); } ab / (aa * bb).sqrt() };
    let turn = moved_at.iter().map(|&i| f0[i][2].angle_between(f1[i][2]).to_degrees()).fold(0.0, f32::max);
    (corr(&tangent), corr(&worldly), turn, moved_at.len())
}

/// Multires: detail stored in tangent space at level 3 moves with the
/// surface when the cage moves. Read in the moved level's frames it is
/// unchanged (correlation 1); the same detail kept as world-space offsets
/// reads as the frames turned: nearly the same for the plan's move (a cage
/// corner by 0.3 of an edge turns normals under 10 degrees), far off where
/// an elbow bends a quarter turn.
#[test]
fn multires_keeps_detail() {
    let cube = with_levels(cube(), 3);
    // The plan's move: corner 7 by 0.3 of an edge (2), along and across.
    let (ct, cw, turn, n) = detail_after(&cube, 0.0, |p| p.cage.set(7, p.cage.get(7) + Vec3::new(0.3, -0.2, 0.1).normalize() * 0.6));
    eprintln!("multires, a corner moved 0.3 edge: {n} level-3 points moved (normals turned up to {turn:.1} degrees); detail correlation tangent space {ct:.4}, world space {cw:.4}");
    assert!(n > 100 && ct >= 0.98 && cw < ct);
    // A column four cubes tall, its top ring bent a quarter turn about z
    // around the ring below's left side (an elbow): the surface there
    // turns up to 90 degrees.
    let (col, top) = column(4);
    let col = with_levels(col, 3);
    let bend = |p: &mut Poly| for &i in &top { let pivot = Vec3::new(-1.0, 3.0, 0.0); p.cage.set(i, glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2) * (p.cage.get(i) - pivot) + pivot); };
    let (tt, tw, turn, n) = detail_after(&col, 0.0, bend);
    let (bt, bw, _, nb) = detail_after(&col, 45.0, bend);
    eprintln!("multires, an elbow bent 90 degrees: {n} points moved (normals turned up to {turn:.1} degrees); tangent space {tt:.4}, world space {tw:.4}; the {nb} that turned 45 degrees or more: {bt:.4} and {bw:.4}");
    assert!(tt >= 0.98 && tw < tt - 0.15);
    assert!(bt >= 0.98 && bw < 0.6, "world-space detail does not follow a bend");
}

/// A closed 2 × 2 column `n` units tall (rings of 4 points at y = 0..=n),
/// and its top ring's points.
fn column(n: u32) -> (Poly, Vec<usize>) {
    let ring = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    let points: Vec<Vec3> = (0..=n).flat_map(|y| ring.iter().map(move |&(x, z)| Vec3::new(x, y as f32, z))).collect();
    let at = |y: u32, k: u32| y * 4 + k % 4;
    let mut faces: Vec<Vec<u32>> = (0..n).flat_map(|y| (0..4).map(move |k| vec![at(y, k), at(y + 1, k), at(y + 1, k + 1), at(y, k + 1)])).collect();
    faces.push(vec![at(0, 0), at(0, 1), at(0, 2), at(0, 3)]);
    faces.push(vec![at(n, 3), at(n, 2), at(n, 1), at(n, 0)]);
    let poly = Poly::from_faces(&points, &faces).unwrap();
    (poly, (0..4).map(|k| at(n, k) as usize).collect())
}

/// Level 1 equals `human::subdivide` on the MakeHuman base (quads and
/// triangles, no creases) away from its borders, which `human::subdivide`
/// keeps fixed and D16 does not.
#[test]
fn cc_matches_human_subdivide() {
    let (points, faces) = crate::human::base_cage();
    let lists: Vec<Vec<u32>> = faces.iter().map(|f| f.iter().copied().filter(|&i| i != u32::MAX).collect()).collect();
    let poly = with_levels(Poly::from_faces(points, &lists).unwrap(), 1);
    let ours = evaluate(&poly, 1).unwrap().points;
    let (theirs, _) = crate::human::subdivide(points, faces, 1);
    assert_eq!(ours.len(), theirs.len());
    let t = Refiner::new(&poly, 1).unwrap();
    let t0 = &t.topology[0];
    // Points whose rule reads a border: on one, or a neighbour of one.
    let border_point: Vec<bool> = (0..t0.vertices).map(|v| t0.edges_of(v).iter().any(|&e| t0.is_border(e as usize))).collect();
    let v = t0.vertices;
    let f = t0.faces();
    let mut checked = 0;
    let mut worst = 0.0f32;
    for i in 0..ours.len() {
        let near_border = if i < v {
            border_point[i]
        } else if i < v + f {
            false
        } else {
            t0.is_border(i - v - f)
        };
        if near_border || (i < v && t0.edges_of(i).is_empty()) {
            continue;
        }
        worst = worst.max(ours[i].distance(theirs[i]));
        checked += 1;
    }
    let borders = (0..t0.edges.len()).filter(|&e| t0.is_border(e)).count();
    eprintln!("MakeHuman level 1: {checked} of {} points checked ({borders} border edges), max difference {worst:.2e}", ours.len());
    assert!(checked > ours.len() * 9 / 10);
    assert!(worst <= 1e-6);
}

/// Save and load give back the same object, bit for bit; oversized
/// counts are refused before anything is inflated.
#[test]
fn save_and_load_round_trip() {
    let mut p = with_levels(creased(cube(), 2.5), 3);
    p.level = 2;
    p.made_by = Some(std::sync::Arc::new(json!({ "action": "from_clay", "source": 4 })));
    p.projected = Some(std::sync::Arc::new(json!({ "source": 4, "levels": 2, "mode": "vector", "cone": 45.0, "reach": 0.1 })));
    p.uv = Some(Chunked::new(&(0..p.corners.len()).map(|i| [i as f32 / 24.0, 1.0 - i as f32 / 48.0]).collect::<Vec<_>>()));
    let n2 = subdiv::counts(&Topology::build(8, p.face_start.to_vec(), p.corners.to_vec()), 3)[2].vertices as usize;
    p.levels[1].displacement = Some(Chunked::new(&(0..n2).map(|i| Vec3::splat(i as f32 * 1e-3)).collect::<Vec<_>>()));
    let text = serde_json::to_string(&p).unwrap();
    let back: Poly = serde_json::from_str(&text).unwrap();
    assert_eq!(back.hash(), p.hash());
    assert_eq!(back, p);
    assert_ne!(Poly { projected: None, ..p.clone() }.hash(), p.hash(), "the projection's record is hashed");
    assert_ne!(Poly { uv: None, ..p.clone() }.hash(), p.hash(), "UVs are hashed");
    let mut w: serde_json::Value = serde_json::from_str(&text).unwrap();
    w["uv"]["corners"] = json!(5);
    assert!(serde_json::from_value::<Poly>(w).unwrap_err().to_string().contains("UVs for 5 corners"));
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["points"] = json!(1u64 << 40);
    assert!(serde_json::from_value::<Poly>(v.clone()).unwrap_err().to_string().contains("at most"));
    v["points"] = json!(8);
    v["level"] = json!(5);
    assert!(serde_json::from_value::<Poly>(v.clone()).is_err(), "a shown level past the levels kept");
    v["level"] = json!(2);
    v["displacement"][0]["vertices"] = json!(3);
    assert!(serde_json::from_value::<Poly>(v).is_err(), "a displacement of the wrong size");
}

/// Chunks are shared by clones; a set copies only its own chunk.
#[test]
fn chunked_copies_only_what_changes() {
    let values: Vec<u32> = (0..10_000).collect();
    let a = Chunked::new(&values);
    let mut b = a.clone();
    assert_eq!(a, b);
    b.set(5000, 7);
    assert_ne!(a, b);
    assert!(Arc::ptr_eq(&a.chunks()[0], &b.chunks()[0]) && Arc::ptr_eq(&a.chunks()[2], &b.chunks()[2]));
    assert!(!Arc::ptr_eq(&a.chunks()[1], &b.chunks()[1]));
    assert_eq!(b.get(5000), 7);
    assert_eq!(Chunked::new(&values), a, "equal contents in other chunks compare equal");
}

/// A scene holding a 2M-face Poly clones and compares without touching
/// its contents: the clone shares the cage's storage, and `==` stops at
/// the shared pointers. The time is logged (and held to §6's 50 µs with
/// CLIDE_PERF=1).
#[test]
fn poly_clone_compare_guard() {
    let (n, m) = (1000u32, 2000u32);
    let points: Vec<Vec3> = (0..=m).flat_map(|y| (0..=n).map(move |x| Vec3::new(x as f32, y as f32, 0.0))).collect();
    let at = |x: u32, y: u32| y * (n + 1) + x;
    let mut corners = Vec::with_capacity((n * m * 4) as usize);
    for y in 0..m {
        for x in 0..n {
            corners.extend_from_slice(&[at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1)]);
        }
    }
    let starts: Vec<u32> = (0..=n * m).map(|i| 4 * i).collect();
    let mut poly = Poly::new(&points, &starts, &corners).unwrap();
    poly.creases = Arc::new((0..1000).map(|i| ((i, i + 1), 2.0)).collect());
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Poly);
    scene.get_mut(id).unwrap().poly = Some(poly);
    let copy = scene.clone();
    let (a, b) = (scene.get(id).unwrap().poly.as_ref().unwrap(), copy.get(id).unwrap().poly.as_ref().unwrap());
    assert!(Arc::ptr_eq(&a.cage.chunks, &b.cage.chunks) && Arc::ptr_eq(&a.corners.chunks, &b.corners.chunks) && Arc::ptr_eq(&a.face_start.chunks, &b.face_start.chunks) && Arc::ptr_eq(&a.creases, &b.creases));
    let (eq, ms) = crate::harness::perf::time(201, || {
        let copy = scene.clone();
        copy == scene
    });
    assert!(eq);
    let extra = json!({ "faces": n * m });
    if crate::harness::perf::strict() {
        crate::harness::perf::check("scene clone + compare, 2M-face Poly", ms, 0.05, extra);
    } else {
        crate::harness::perf::log("scene clone + compare, 2M-face Poly", ms, extra);
    }
    eprintln!("scene clone + compare with a 2M-face Poly: {:.1} us", ms * 1000.0);
    // An edit copies one chunk of the cage, not the cage.
    let mut edited = scene.clone();
    edited.get_mut(id).unwrap().poly.as_mut().unwrap().cage.set(10, Vec3::splat(-1.0));
    let (a, b) = (scene.get(id).unwrap().poly.as_ref().unwrap(), edited.get(id).unwrap().poly.as_ref().unwrap());
    assert_eq!(b.unshared_bytes(Some(a)), CHUNK * 12);
    assert!(scene != edited);
}

/// §6: level 4 of a 5k-quad cage (1.28M faces) builds in 400 ms, and
/// re-evaluates after a cage point moves (topology kept) in 60 ms. Run
/// with --ignored (CLIDE_PERF=1 holds the bounds).
#[test]
#[ignore]
fn level_four_of_a_5k_cage_perf() {
    let cage = with_levels(torus(100, 50), 4);
    let (r, build) = crate::harness::perf::time(5, || { let r = Refiner::new(&cage, 4).unwrap(); let p = r.points(cage.cage.to_vec(), &cage.levels); (r, p.len()) });
    let (refiner, points) = r;
    let mut moved = cage.cage.to_vec();
    moved[0] += Vec3::splat(0.05);
    let (_, again) = crate::harness::perf::time(5, || refiner.points(moved.clone(), &cage.levels).len());
    let (mesh, draw) = crate::harness::perf::time(3, || crate::mesh::Mesh::poly(&Poly { level: 4, ..cage.clone() }).indices.len() / 3);
    eprintln!("level 4 of a 5,000-quad cage: {} faces, {points} points; build {build:.0} ms, re-evaluate {again:.0} ms, drawn mesh ({mesh} triangles) {draw:.0} ms", refiner.top.face_start.len() - 1);
    crate::harness::perf::check("poly level 4 of a 5k cage", build, 400.0, json!({ "faces": refiner.top.face_start.len() - 1 }));
    crate::harness::perf::check("poly level 4 re-evaluation", again, 60.0, json!({}));
}

/// A small xorshift: the same cases on every run, no crates.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// A PolyFile of a few random points, faces and corners: offsets mostly
/// rising (sometimes anything), corners mostly in range, header counts
/// that sometimes lie, data that is sometimes not base64 or deflate,
/// creases, levels and displacements of any size.
fn random_file(r: &mut Rng) -> serde_json::Value {
    let (points, faces, corners) = (r.below(10) as usize, r.below(6) as usize, r.below(24) as usize);
    let mut raw = Vec::new();
    for _ in 0..points * 3 {
        let v = match r.below(10) {
            0 => f32::NAN,
            1 => 1e30,
            _ => (r.below(2000) as f32 - 1000.0) / 500.0,
        };
        raw.extend(v.to_le_bytes());
    }
    let mut at = 0u32;
    for f in 0..=faces {
        let v = match r.below(6) {
            0 => r.next() as u32,
            1 => r.below(corners as u64 + 12) as u32,
            _ if f == 0 => 0,
            _ if f == faces => corners as u32,
            _ => {
                at += r.below(7) as u32;
                at
            }
        };
        raw.extend(v.to_le_bytes());
    }
    for _ in 0..corners {
        let v = if r.below(12) == 0 { r.next() as u32 } else { r.below(points as u64 + 1) as u32 };
        raw.extend(v.to_le_bytes());
    }
    let data = match r.below(12) {
        0 => "not base64!".to_string(),
        1 => base64::engine::general_purpose::STANDARD.encode((0..r.below(64)).map(|_| r.next() as u8).collect::<Vec<u8>>()),
        2 => pack(&raw[..r.below(raw.len() as u64 + 1) as usize]).unwrap(),
        _ => pack(&raw).unwrap(),
    };
    let mut lie = |n: usize| if r.below(8) == 0 { r.below(30) as usize } else { n };
    let (points, faces, corners) = (lie(points), lie(faces), lie(corners));
    let sharp = [-1.0, 0.0, 0.5, 2.5, 10.0, 11.0];
    let creases: Vec<serde_json::Value> = (0..r.below(4)).map(|_| json!([r.below(12), r.below(12), sharp[r.below(6) as usize]])).collect();
    let displacement: Vec<serde_json::Value> = (0..r.below(3) / 2)
        .map(|_| {
            let n = r.below(120);
            let bytes: Vec<u8> = (0..n * 12).map(|_| if r.below(3) == 0 { r.next() as u8 } else { 0 }).collect();
            json!({ "level": r.below(4), "vertices": n, "data": pack(&bytes).unwrap() })
        })
        .collect();
    let mut file = json!({ "points": points, "faces": faces, "corners": corners, "data": data, "creases": creases, "levels": r.below(4), "level": r.below(4), "displacement": displacement, "made_by": { "action": "from_clay", "source": r.below(5) } });
    // UVs of any count, some not finite or far off the tile.
    if r.below(2) == 0 {
        let n = if r.below(3) == 0 { r.below(30) as usize } else { corners };
        let bytes: Vec<u8> = (0..n * 2)
            .flat_map(|_| match r.below(10) {
                0 => f32::NAN,
                1 => 1e9,
                _ => r.below(1000) as f32 / 1000.0,
            }.to_le_bytes())
            .collect();
        let data = if r.below(8) == 0 { "%%".to_string() } else { pack(&bytes[..if r.below(6) == 0 { r.below(bytes.len() as u64 + 1) as usize } else { bytes.len() }]).unwrap() };
        file["uv"] = json!({ "corners": if r.below(6) == 0 { r.below(40) as usize } else { n }, "data": data });
    }
    if r.below(4) == 0 {
        file["projected"] = json!({ "source": r.below(5), "levels": r.below(9) });
    }
    file
}

/// A saved Poly with bytes of its packed cage (or a header count)
/// changed at random.
fn mutated_file(r: &mut Rng, text: &str) -> serde_json::Value {
    use std::io::Read;
    let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD.decode(v["data"].as_str().unwrap()).unwrap();
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[..]).read_to_end(&mut raw).unwrap();
    for _ in 0..1 + r.below(4) {
        let i = r.below(raw.len() as u64) as usize;
        raw[i] = match r.below(3) {
            0 => r.next() as u8,
            1 => raw[i].wrapping_add(1),
            _ => raw[i] ^ 0x80,
        };
    }
    v["data"] = json!(pack(&raw).unwrap());
    if r.below(6) == 0 {
        let key = ["points", "faces", "corners", "levels", "level"][r.below(5) as usize];
        v[key] = json!(r.below(40));
    }
    v
}

/// Loads `file` and, when it loads, reads its info, its levels and its
/// mirror image: none of it may panic.
fn load_and_use(file: &serde_json::Value) {
    let Ok(p) = serde_json::from_value::<Poly>(file.clone()) else { return };
    let _ = p.info();
    for l in 0..=p.level_count().min(2) {
        let _ = evaluate(&p, l);
        let _ = crate::poly::uv::at_level(&p, l);
    }
    let _ = evaluate(&p.mirrored(0), p.level_count().min(1));
}

/// Corrupt and random files never panic on load (or after it): the
/// review's case (face offsets 0, 10, 6 over 6 corners, which sliced past
/// the corners) first, then 4,000 random files and 2,000 saved ones with
/// bytes changed. Each is refused with an error or loads a valid cage.
#[test]
fn corrupt_files_never_panic() {
    // Offsets that pass the corners and come back.
    let corners = [0u32, 4, 6, 2, 1, 3];
    let err = Poly::new(&cube().cage.to_vec(), &[0, 10, 6], &corners).unwrap_err();
    assert!(err.contains("must not decrease"), "{err}");
    let mut raw = Vec::new();
    points_bytes(cube().cage.iter(), &mut raw);
    for i in [0u32, 10, 6].iter().chain(&corners) {
        raw.extend(i.to_le_bytes());
    }
    let file = json!({ "points": 8, "faces": 2, "corners": 6, "data": pack(&raw).unwrap() });
    assert!(serde_json::from_value::<Poly>(file).is_err());
    let mut saved = with_levels(creased(cube(), 2.5), 2);
    let n1 = subdiv::counts(&Topology::build(8, saved.face_start.to_vec(), saved.corners.to_vec()), 2)[1].vertices as usize;
    saved.levels[0].displacement = Some(Chunked::new(&vec![Vec3::splat(0.01); n1]));
    saved.uv = Some(Chunked::new(&(0..saved.corners.len()).map(|i| [i as f32 / 24.0, 0.5]).collect::<Vec<_>>()));
    let saved = serde_json::to_string(&saved).unwrap();
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut loaded, mut refused) = (0, 0);
    for case in 0..6000 {
        let file = if case < 4000 { random_file(&mut r) } else { mutated_file(&mut r, &saved) };
        let ok = serde_json::from_value::<Poly>(file.clone()).is_ok();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_and_use(&file))).is_err() {
            panic!("case {case} panicked: {file}");
        }
        if ok { loaded += 1 } else { refused += 1 }
    }
    eprintln!("corrupt files: {loaded} loaded, {refused} refused");
    assert!(loaded > 100 && refused > 1000, "the cases reach both ends ({loaded} loaded, {refused} refused)");
}

/// A mirror image faces out (it encloses the volume the source does) and
/// its first level is the source's reflected point for point (winding
/// each face the other way from the same corner keeps the level's
/// numbering); creases and level counts stay.
#[test]
fn mirrored_faces_out_and_reflects_levels() {
    let mut p = with_levels(creased(cube(), 1.5), 2);
    p.cage = Chunked::new(&p.cage.iter().map(|q| q + Vec3::new(2.0, 0.5, 0.0)).collect::<Vec<_>>());
    p.level = 1;
    let m = p.mirrored(0);
    let volume = |q: &Poly, l: u8| {
        let s = evaluate(q, l).unwrap();
        crate::mesh_asset::volume(&s.points, &s.triangles())
    };
    assert!((volume(&m, 0) - volume(&p, 0)).abs() < 1e-5 && volume(&m, 0) > 7.9, "{} {}", volume(&m, 0), volume(&p, 0));
    assert!(volume(&m, 2) > 0.0);
    let (a, b) = (evaluate(&p, 1).unwrap().points, evaluate(&m, 1).unwrap().points);
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(&b).all(|(x, y)| (Vec3::new(-x.x, x.y, x.z) - *y).length() < 1e-5));
    assert_eq!((m.creases.len(), m.level_count(), m.level), (p.creases.len(), 2, 1));
    assert_eq!(Topology::build(8, m.face_start.to_vec(), m.corners.to_vec()).non_manifold(), (0, 0));
}

/// Two cubes sharing one corner point (a file can hold that; from_clay
/// splits such points): the point is counted once per fan, so the Euler
/// characteristic is two spheres' (4) and the genus 0, and info names it.
#[test]
fn info_counts_pinched_points_per_fan() {
    let a = cube();
    let mut points = a.cage.to_vec();
    points.extend(a.cage.iter().skip(1).map(|q| q + Vec3::splat(2.0)));
    // The second cube's corner 0 lands on the first's corner 7, (1, 1, 1).
    let at = |j: u32| if j == 0 { 7 } else { 7 + j };
    let (starts, corners) = a.cage_faces();
    let mut faces: Vec<Vec<u32>> = starts.windows(2).map(|w| corners[w[0] as usize..w[1] as usize].to_vec()).collect();
    faces.extend(starts.windows(2).map(|w| corners[w[0] as usize..w[1] as usize].iter().map(|&j| at(j)).collect::<Vec<_>>()));
    let info = Poly::from_faces(&points, &faces).unwrap().info();
    assert_eq!((info["points"].as_u64(), info["cage_points"].as_u64(), info["non_manifold_points"].as_u64()), (Some(15), Some(15), Some(1)), "{info}");
    assert_eq!((info["components"].as_i64(), info["euler"].as_i64(), info["genus"].as_i64()), (Some(2), Some(4), Some(0)), "{info}");
    // An unused point: in cage_points, not in points.
    let mut more = points.clone();
    more.push(Vec3::splat(9.0));
    let info = Poly::from_faces(&more, &faces).unwrap().info();
    assert_eq!((info["points"].as_u64(), info["cage_points"].as_u64()), (Some(15), Some(16)));
}
