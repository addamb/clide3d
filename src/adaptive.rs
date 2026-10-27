//! Adaptive fine meshes: a surface-net mesh at the coarse detail cell,
//! refined only where a finer cell is wanted (detail regions, see
//! `clay::Region`) and moved onto the exact surface there.
//!
//! Refinement splits every edge longer than the wanted size at that place
//! (its midpoint). An edge is split for both triangles that share it, and
//! each triangle is cut by the pattern of its split edges (into 2, 3 or 4),
//! so the mesh never has a T-junction: what was closed stays closed, with
//! no cracks where fine and coarse parts meet. New vertices are then pulled
//! onto the surface along the field's gradient (a few Newton steps), so the
//! fine part shows the fine sculpt detail.

use std::collections::HashMap;

use glam::Vec3;

use crate::clay::par_chunks;

/// How much longer than the wanted size an edge may be before it splits.
const SLACK: f32 = 1.5;
/// Most rounds of splitting (each halves edges; 2^8 is far past any cell ratio).
const MAX_PASSES: usize = 8;

/// Splits edges of `indices` (over `positions`) until each is at most about
/// `size(midpoint)` long, moving new points with `project` (point, the
/// edge's length, the mesh's normal there) onto the surface. Errs when the mesh would pass
/// `max_triangles`, saying how many it needed.
pub(crate) fn refine(
    positions: &mut Vec<Vec3>,
    indices: &mut Vec<u32>,
    size: &(dyn Fn(Vec3) -> f32 + Sync),
    project: &(dyn Fn(Vec3, f32, Vec3) -> Vec3 + Sync),
    max_triangles: usize,
) -> Result<Refined, String> {
    let mut parents = Vec::new();
    for _ in 0..MAX_PASSES {
        // Edges to split this round, each once (by its sorted ends).
        let mut tris: Vec<[u32; 3]> = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        let mut marks: Vec<[bool; 3]> = vec![[false; 3]; tris.len()];
        {
            let pos = &*positions;
            let mut pairs: Vec<([u32; 3], [bool; 3])> = tris.iter().map(|t| (*t, [false; 3])).collect();
            par_chunks(&mut pairs, 4096, |_, chunk| {
                for (t, m) in chunk {
                    for e in 0..3 {
                        let (a, b) = (pos[t[e] as usize], pos[t[(e + 1) % 3] as usize]);
                        let len = a.distance(b);
                        m[e] = len > SLACK * size((a + b) * 0.5).max(1e-7);
                    }
                }
            });
            for (k, (_, m)) in pairs.into_iter().enumerate() {
                marks[k] = m;
            }
        }
        // An edge is split when either triangle beside it asks.
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        for (t, m) in tris.iter().zip(&marks) {
            for e in 0..3 {
                if m[e] {
                    let (a, b) = (t[e], t[(e + 1) % 3]);
                    mid.entry((a.min(b), a.max(b))).or_insert(u32::MAX);
                }
            }
        }
        if mid.is_empty() {
            break;
        }
        let estimate = tris.len() + mid.len() * 2;
        if estimate > max_triangles {
            return Err(format!("the fine mesh would need more than {max_triangles} triangles (about {estimate} at this round); use larger detail cells or smaller regions"));
        }
        // New points at the midpoints, in a fixed order, moved along the
        // mesh's normal there.
        let mut keys: Vec<(u32, u32)> = mid.keys().copied().collect();
        keys.sort_unstable();
        let first = positions.len();
        let normals = vertex_normals(positions, &tris);
        let mut fresh: Vec<(Vec3, f32, Vec3)> = keys.iter().map(|&(a, b)| {
            let (pa, pb) = (positions[a as usize], positions[b as usize]);
            ((pa + pb) * 0.5, pa.distance(pb), (normals[a as usize] + normals[b as usize]).normalize_or_zero())
        }).collect();
        par_chunks(&mut fresh, 1024, |_, chunk| {
            for (p, len, n) in chunk {
                *p = project(*p, *len, *n);
            }
        });
        for (k, key) in keys.iter().enumerate() {
            mid.insert(*key, (first + k) as u32);
        }
        positions.extend(fresh.iter().map(|(p, _, _)| *p));
        parents.extend_from_slice(&keys);
        let at = |a: u32, b: u32| mid.get(&(a.min(b), a.max(b))).copied();
        let mut out = Vec::with_capacity(estimate * 3);
        for t in tris.iter_mut() {
            let m = [at(t[0], t[1]), at(t[1], t[2]), at(t[2], t[0])];
            match m.iter().filter(|x| x.is_some()).count() {
                0 => out.extend_from_slice(t),
                3 => {
                    let (a, b, c) = (t[0], t[1], t[2]);
                    let (ab, bc, ca) = (m[0].unwrap(), m[1].unwrap(), m[2].unwrap());
                    out.extend_from_slice(&[a, ab, ca, ab, b, bc, ca, bc, c, ab, bc, ca]);
                }
                1 => {
                    // Turn so the split edge is the first (a-b).
                    let e = m.iter().position(|x| x.is_some()).unwrap();
                    let (a, b, c) = (t[e], t[(e + 1) % 3], t[(e + 2) % 3]);
                    let ab = m[e].unwrap();
                    out.extend_from_slice(&[a, ab, c, ab, b, c]);
                }
                _ => {
                    // Two split edges: turn so the whole edge is c-a.
                    let e = m.iter().position(|x| x.is_none()).unwrap();
                    let (a, b, c) = (t[(e + 1) % 3], t[(e + 2) % 3], t[e]);
                    let (ab, bc) = (m[(e + 1) % 3].unwrap(), m[(e + 2) % 3].unwrap());
                    out.extend_from_slice(&[ab, b, bc]);
                    // The quad a, ab, bc, c: cut along its shorter diagonal.
                    let p = |i: u32| positions[i as usize];
                    if p(a).distance_squared(p(bc)) <= p(ab).distance_squared(p(c)) {
                        out.extend_from_slice(&[a, ab, bc, a, bc, c]);
                    } else {
                        out.extend_from_slice(&[a, ab, c, ab, bc, c]);
                    }
                }
            }
        }
        *indices = out;
    }
    Ok(Refined { parents })
}

/// What `refine` did.
pub(crate) struct Refined {
    /// The edge (two older vertices) each new vertex split, in order: vertex
    /// `first + k` came from `parents[k]`, `first` the old vertex count.
    pub parents: Vec<(u32, u32)>,
}

/// Area-weighted vertex normals.
fn vertex_normals(positions: &[Vec3], tris: &[[u32; 3]]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; positions.len()];
    for t in tris {
        let (a, b, c) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
        let f = (b - a).cross(c - a);
        for &i in t {
            n[i as usize] += f;
        }
    }
    n.iter().map(|v| v.normalize_or_zero()).collect()
}

/// The surface of `f` nearest `p` along the line through it in direction
/// `n` (either way, at most `reach`); falls back to Newton steps when `n`
/// is zero, and stays at `p` when the line meets no surface. Walking the
/// normal line, unlike free Newton steps, never jumps sideways across a
/// thin gap (lips, between fingers) onto the wrong surface.
pub(crate) fn along(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, e: f32, reach: f32) -> Vec3 {
    if n == Vec3::ZERO {
        return onto(f, p, e, reach);
    }
    let d0 = f(p);
    if d0 == 0.0 {
        return p;
    }
    // Toward the surface first (out of the solid along +n, into it along
    // -n), then the other way.
    let first = if d0 > 0.0 { -n } else { n };
    const STEPS: usize = 8;
    let h = reach / STEPS as f32;
    let mut best: Option<(f32, Vec3)> = None;
    for dir in [first, -first] {
        let mut prev = 0.0;
        for k in 1..=STEPS {
            let t = k as f32 * h;
            if best.is_some_and(|(b, _)| t >= b) {
                break;
            }
            if (f(p + dir * t) > 0.0) != (d0 > 0.0) {
                let (mut lo, mut hi) = (prev, t);
                for _ in 0..12 {
                    let m = (lo + hi) * 0.5;
                    if (f(p + dir * m) > 0.0) == (d0 > 0.0) { lo = m } else { hi = m }
                }
                let t = (lo + hi) * 0.5;
                best = Some((t, p + dir * t));
                break;
            }
            prev = t;
        }
    }
    best.map_or(p, |(_, q)| q)
}

/// One Newton projection onto the zero set of `f`, moving at most `reach`:
/// a point near the surface goes to the surface along the gradient.
pub(crate) fn onto(f: &dyn Fn(Vec3) -> f32, p: Vec3, e: f32, reach: f32) -> Vec3 {
    let mut q = p;
    for _ in 0..4 {
        let d = f(q);
        let g = Vec3::new(f(q + Vec3::X * e) - f(q - Vec3::X * e), f(q + Vec3::Y * e) - f(q - Vec3::Y * e), f(q + Vec3::Z * e) - f(q - Vec3::Z * e)) / (2.0 * e);
        let g2 = g.length_squared();
        if g2 < 1e-12 {
            break;
        }
        let step = g * (d / g2);
        q -= step;
        if step.length() < e * 0.01 {
            break;
        }
    }
    // Keep the point near where it started: a far jump means another
    // surface was nearer, or the field was flat.
    let moved = q - p;
    if !q.is_finite() || moved.length() > reach { p + if moved.is_finite() { moved.clamp_length_max(reach) } else { Vec3::ZERO } } else { q }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cube cases: one corner in (3 edges, one piece), two opposite corners
    /// in (two pieces of 3), a face's two diagonal corners in (resolved by
    /// the face middle).
    #[test]
    fn cube_pieces() {
        let count = |p: [u8; 12]| (p.iter().filter(|&&x| x != OTHER).count(), p.iter().filter(|&&x| x != OTHER).max().map_or(0, |m| m + 1));
        let mut f = [1.0f32; 8];
        f[0] = -1.0;
        assert_eq!(count(pieces(&f)), (3, 1));
        f[7] = -1.0;
        assert_eq!(count(pieces(&f)), (6, 2));
        let mut g = [1.0f32; 8];
        g[0] = -1.0;
        g[3] = -1.0;
        // Face z = 0 holds corners 0 and 3 diagonally: middle outside -> apart.
        assert_eq!(count(pieces(&g)), (6, 2));
        g[0] = -5.0;
        g[3] = -5.0;
        // Strongly inside: middle inside -> one tube piece.
        assert_eq!(count(pieces(&g)), (6, 1));
        // Half the cube in: one quad.
        let h = [-1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0];
        assert_eq!(count(pieces(&h)), (4, 1));
    }

    /// An octahedron refined in one corner stays closed (every edge in two
    /// triangles, consistently wound) and its new points sit on the sphere.
    #[test]
    fn refinement_is_conforming_and_projects() {
        let mut positions = vec![Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
        let mut indices = vec![0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5];
        let size = |p: Vec3| if p.x > 0.3 { 0.05 } else { 2.0 };
        let sphere = |p: Vec3| p.length() - 1.0;
        let project = |p: Vec3, len: f32, n: Vec3| along(&sphere, p, n, 1e-3, len);
        refine(&mut positions, &mut indices, &size, &project, 1_000_000).unwrap();
        assert!(indices.len() / 3 > 500, "{}", indices.len() / 3);
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for t in indices.chunks_exact(3) {
            for e in 0..3 {
                *edges.entry((t[e], t[(e + 1) % 3])).or_default() += 1;
            }
        }
        for (&(a, b), &n) in &edges {
            assert_eq!(n, 1, "edge {a}-{b} used {n} times one way");
            assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}-{b} has no twin");
        }
        for p in &positions {
            assert!((p.length() - 1.0).abs() < 1e-3, "{p} {}", p.length());
        }
        let err = refine(&mut positions.clone(), &mut indices.clone(), &|_| 0.001, &project, 10_000);
        assert!(err.is_err());
    }
}

/// Detail regions on real clay: the old single-grid meshes are unchanged,
/// refined meshes are closed where the uniform mesh is, and they carry the
/// fine sculpt detail.
#[cfg(test)]
pub(crate) mod clay_tests {
    use glam::Vec3;
    use serde_json::json;

    use crate::clay::{Clay, Region};
    use crate::mesh::Mesh;
    use crate::sculpt::{self, Stroke};

    /// A sphere of radius 0.1 with pores and a crease, on a 2 mm grid.
    fn sculpted() -> Clay {
        let mut c: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "material": "skin" }], "resolution": 64, "detail_cell": 0.002 })).unwrap();
        for s in [
            json!({ "brush": "noise", "points": [[0, 0, 0.1]], "radius": 0.08, "strength": 0.6, "scale": 0.004, "pattern": "pores" }),
            json!({ "brush": "crease", "points": [[-0.05, 0.02, 0.09], [0.05, 0.02, 0.09]], "radius": 0.006, "strength": 0.002 }),
        ] {
            let s: Stroke = serde_json::from_value(s).unwrap();
            sculpt::apply(&mut c, &s).unwrap();
        }
        c
    }

    fn hash(m: &Mesh) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |x: u32| { h ^= x as u64; h = h.wrapping_mul(0x0100_0000_01b3); };
        for v in &m.vertices {
            v.pos.iter().chain(&v.normal).chain(&v.color).for_each(|f| eat(f.to_bits()));
        }
        m.indices.iter().for_each(|&i| eat(i));
        h
    }

    /// Each directed edge appears once and its reverse once: closed and
    /// consistently wound, with no cracks.
    fn closed(m: &Mesh) -> bool {
        let mut seen = std::collections::HashMap::new();
        for t in m.indices.chunks_exact(3) {
            for e in 0..3 {
                *seen.entry((t[e], t[(e + 1) % 3])).or_insert(0) += 1;
            }
        }
        let holes = seen.iter().filter(|&(&(a, b), _)| !seen.contains_key(&(b, a))).count();
        let doubled = seen.iter().filter(|&(_, &n)| n > 1).count();
        let uneven = seen.iter().filter(|&(&(a, b), &n)| seen.get(&(b, a)).is_some_and(|&m| m != n)).count();
        if holes + doubled + uneven > 0 {
            eprintln!("edges {}: {holes} without a twin, {doubled} used twice one way, {uneven} uneven", seen.len());
            for (&(a, b), _) in seen.iter().filter(|&(_, &n)| n > 1).take(6) {
                eprintln!("  {} -> {}", Vec3::from(m.vertices[a as usize].pos), Vec3::from(m.vertices[b as usize].pos));
                for t in m.indices.chunks_exact(3).filter(|t| t.contains(&a) && t.contains(&b)) {
                    eprintln!("    tri {t:?}");
                }
            }
        }
        seen.iter().all(|(&(a, b), &n)| n == 1 && seen.get(&(b, a)) == Some(&1))
    }

    /// Spread of the distance to the center over vertices in a patch: the
    /// sculpt relief the mesh shows there.
    fn relief(m: &Mesh, around: Vec3, within: f32) -> f32 {
        let r: Vec<f32> = m.vertices.iter().map(|v| Vec3::from(v.pos)).filter(|p| p.distance(around) < within).map(|p| p.length()).collect();
        let mean = r.iter().sum::<f32>() / r.len() as f32;
        (r.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / r.len() as f32).sqrt()
    }

    /// Hashes of the regular and fine meshes made by the code before detail
    /// regions existed (same clay, same strokes).
    #[test]
    fn old_scenes_mesh_identically() {
        let c = sculpted();
        let round: Clay = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        let regular = round.build_mesh();
        let fine = round.build_fine(&[], "").unwrap();
        println!("regular {:x} fine {:x}", hash(&regular), hash(&fine));
        assert_eq!((hash(&regular), hash(&fine)), (OLD_REGULAR, OLD_FINE));
    }

    /// A unit sphere at detail cell 0.02 with one 5 mm detail region and
    /// pore noise of `scale` reaching past the region's edge, so the
    /// mesh changes cell size under sculpt detail.
    pub(crate) fn seam_fixture(scale: f32, strength: f32) -> Clay {
        let mut c: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 1.0 }], "resolution": 64, "detail_cell": 0.02 })).unwrap();
        let face = Region { name: "face".into(), center: Vec3::Z, radii: Vec3::splat(0.3), cell: 0.005 };
        c.set_detail_regions(std::slice::from_ref(&face)).unwrap();
        let s: Stroke = serde_json::from_value(json!({ "brush": "noise", "points": [[0, 0, 1]], "radius": 0.6, "strength": strength, "scale": scale, "pattern": "pores" })).unwrap();
        sculpt::apply(&mut c, &s).unwrap();
        c
    }

    /// The fine mesh as `export_print` sees it: points welded by position,
    /// in millimeters, triangles the weld collapsed dropped.
    fn welded(m: &Mesh) -> (Vec<Vec3>, Vec<[u32; 3]>) {
        let pts: Vec<Vec3> = m.vertices.iter().map(|v| Vec3::from(v.pos) * 1000.0).collect();
        let tris: Vec<[u32; 3]> = m.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        let (pts, mut tris) = crate::mesh_asset::weld(&pts, &tris);
        tris.retain(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
        (pts, tris)
    }

    /// Defect 2 of the quality plan (186 edges not used twice at region
    /// seams). Two causes, each on its own fixture: deep pores made the
    /// adaptive nets skip boxes the surface passes through (the walk took
    /// the field for a distance), leaving holes; and the weld merged the
    /// copies `make_manifold` gives pinched sheets. No crossing's ring
    /// collapses on either (the plan's other suspect).
    #[test]
    fn region_seams_stay_closed_after_the_weld() {
        // Holes: 46 open edges before the walk allowed for the offsets.
        super::COLLAPSED_RINGS.with(|n| n.set(0));
        let deep = seam_fixture(0.04, 1.0).build_fine(&[], "").unwrap();
        assert!(closed(&deep), "the deep pores left holes");
        let (_, tris) = welded(&deep);
        assert_eq!(crate::mesh_asset::edge_use_kinds(&tris), Default::default());
        // Pinches: closed as built, pinched once welded, apart once separated.
        let pinched = seam_fixture(0.02, 0.8).build_fine(&[], "").unwrap();
        assert!(closed(&pinched));
        let (mut pts, mut tris) = welded(&pinched);
        let kinds = crate::mesh_asset::edge_use_kinds(&tris);
        println!("welded: {kinds:?}");
        assert!(kinds.pinched > 0 && kinds.open == 0 && kinds.over_used == 0, "the fixture no longer pinches: {kinds:?}");
        let s = super::separate_pinches(&mut pts, &mut tris, 0.005, 0.02);
        println!("separated {s:?}");
        assert!(s.copies > 0 && s.min_offset >= 0.005);
        let (pts, tris) = crate::mesh_asset::weld(&pts, &tris);
        let (edges, bad, flipped) = crate::mesh_asset::edge_use(&tris);
        assert!(edges > 0 && bad == 0 && flipped == 0, "{bad} non-manifold, {flipped} flipped after separating");
        assert!(pts.len() > 1000);
        assert_eq!(super::COLLAPSED_RINGS.with(|n| n.get()), 0);
    }

    /// A third cause of open and over-used edges: a capsule whose surface
    /// runs exactly along a grid line (0.096^2 + 0.072^2 = 0.12^2, the
    /// coarse cell 8 mm) where cell sizes change at the edge of sculpt
    /// detail. Cells of different sizes worked out the same corner with
    /// different rounding, so the field there (about 0) had different
    /// signs in each and their crossings disagreed (37 such edges).
    #[test]
    fn a_surface_on_a_grid_line_meshes_closed() {
        let mut c: Clay = serde_json::from_value(json!({ "resolution": 128, "detail_cell": 0.001, "blobs": [
            { "type": "capsule", "a": [0, 0.45, 0], "b": [0, 0.75, 0], "radius": 0.12 },
            { "type": "sphere", "center": [0, 0.9, 0], "radius": 0.1 } ] })).unwrap();
        let face = Region { name: "face".into(), center: Vec3::new(0.0, 0.9, 0.08), radii: Vec3::splat(0.03), cell: 0.00025 };
        c.set_detail_regions(std::slice::from_ref(&face)).unwrap();
        let s: Stroke = serde_json::from_value(json!({ "brush": "noise", "points": [[0.0, 0.6, 0.12]], "radius": 0.1, "strength": 0.6, "scale": 0.0012, "pattern": "pores" })).unwrap();
        sculpt::apply(&mut c, &s).unwrap();
        let fine = c.build_fine(&[], "").unwrap();
        let tris: Vec<[u32; 3]> = fine.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        assert_eq!(crate::mesh_asset::edge_use_kinds(&tris), Default::default());
        assert!(closed(&fine));
    }

    /// Separating moves only the copies, by the offset, along their sheet:
    /// the surface is where it was (Hausdorff within the offset) and so is
    /// the volume.
    #[test]
    fn separate_pinches_keeps_geometry() {
        let (before, tris) = welded(&seam_fixture(0.02, 0.8).build_fine(&[], "").unwrap());
        let (mut pts, mut moved) = (before.clone(), tris.clone());
        let s = super::separate_pinches(&mut pts, &mut moved, 0.005, 0.02);
        assert!(s.copies > 0);
        assert_eq!(s.copies, pts.len() - before.len(), "every copy moved");
        // Every point is an old one or a copy of one moved by its offset.
        let worst = pts.iter().enumerate().map(|(i, p)| {
            let from = if i < before.len() { before[i] } else { *before.iter().min_by(|a, b| a.distance_squared(*p).total_cmp(&b.distance_squared(*p))).unwrap() };
            p.distance(from)
        }).fold(0.0f32, f32::max);
        println!("copies {} offsets {}..{} mm, largest move {worst} mm", s.copies, s.min_offset, s.max_offset);
        assert!(worst <= 1.01 * s.max_offset, "{worst} vs {}", s.max_offset);
        let (v0, v1) = (crate::mesh_asset::volume(&before, &tris), crate::mesh_asset::volume(&pts, &moved));
        println!("volume {v0} -> {v1} mm3");
        assert!(((v1 - v0) / v0).abs() <= 1e-4, "{v0} {v1}");
        // A mesh with no pinches is left exactly as it was.
        let (mut clean, mut clean_tris) = welded(&seam_fixture(0.04, 1.0).build_fine(&[], "").unwrap());
        let (p0, t0) = (clean.clone(), clean_tris.clone());
        assert_eq!(super::separate_pinches(&mut clean, &mut clean_tris, 0.005, 0.02), Default::default());
        assert!(clean == p0 && clean_tris == t0);
    }

    /// A copy whose fan's normals cancel (a sheet folded onto itself, here
    /// two double-sided triangles pinched to a tetrahedron's edge) still
    /// moves, toward its own middle, and every counted copy moved.
    #[test]
    fn separate_pinches_moves_folded_fans() {
        // The tetrahedron on the edge P A (z axis), each face turned outward.
        let mut pts = vec![Vec3::ZERO, Vec3::Z, Vec3::new(1.0, 0.0, 0.5), Vec3::new(0.0, 1.0, 0.5)];
        let mid = pts.iter().sum::<Vec3>() / 4.0;
        let mut tris: Vec<[u32; 3]> = [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]].iter().map(|&[a, b, c]| {
            let [pa, pb, pc] = [a, b, c].map(|i| pts[i as usize]);
            if (pb - pa).cross(pc - pa).dot(pa - mid) < 0.0 { [a, c, b] } else { [a, b, c] }
        }).collect();
        // Two sheets on the same edge, outside the tetrahedron, both ways round.
        for c in [Vec3::new(-1.0, 0.0, 0.5), Vec3::new(0.0, -1.0, 0.5)] {
            let n = pts.len() as u32;
            pts.push(c);
            tris.extend([[0, 1, n], [1, 0, n]]);
        }
        let first = pts.len();
        let before = pts.clone();
        let s = super::separate_pinches(&mut pts, &mut tris, 0.005, 0.02);
        println!("{s:?}");
        assert!(pts.len() > first, "no copies made");
        assert_eq!(s.copies, pts.len() - first, "every copy counted moved");
        for q in &pts[first..] {
            let from = before.iter().map(|b| b.distance(*q)).fold(f32::MAX, f32::min);
            assert!(from >= 0.99 * s.min_offset && s.min_offset >= 0.005, "a copy stayed at {q}");
        }
        let (_, tris) = crate::mesh_asset::weld(&pts, &tris);
        assert_eq!(crate::mesh_asset::edge_use_kinds(&tris), Default::default());
    }

    /// The walk's margin is per coarse cell: the same mesh on 1 thread and 12.
    #[test]
    fn deep_detail_meshes_alike_on_any_thread_count() {
        let c = seam_fixture(0.04, 1.0);
        let one = crate::clay::with_threads(1, || c.build_fine(&[], "").unwrap());
        let many = crate::clay::with_threads(12, || c.build_fine(&[], "").unwrap());
        assert_eq!(hash(&one), hash(&many));
    }

    /// The Stage 1a exit check on a body-scale clay (1 m tall, 1 mm detail
    /// cell, 0.25 mm regions with pores on the face and both hands): the
    /// print path (fine mesh, weld, pinches separated) is watertight at a
    /// quarter of the triangles of a uniform 0.25 mm mesh, which is all
    /// there was before regions. Slow: run with --ignored.
    #[test]
    #[ignore]
    fn body_scale_detail_prints_watertight() {
        let mut c: Clay = serde_json::from_value(json!({ "resolution": 128, "detail_cell": 0.001, "blobs": [
            { "type": "capsule", "a": [0, 0.45, 0], "b": [0, 0.75, 0], "radius": 0.12 },
            { "type": "sphere", "center": [0, 0.9, 0], "radius": 0.1 },
            { "type": "capsule", "a": [0.1, 0.0, 0], "b": [0.1, 0.4, 0], "radius": 0.06 },
            { "type": "capsule", "a": [-0.1, 0.0, 0], "b": [-0.1, 0.4, 0], "radius": 0.06 },
            { "type": "capsule", "a": [0.15, 0.72, 0], "b": [0.3, 0.45, 0], "radius": 0.04 },
            { "type": "capsule", "a": [-0.15, 0.72, 0], "b": [-0.3, 0.45, 0], "radius": 0.04 },
            { "type": "sphere", "center": [0.31, 0.41, 0], "radius": 0.045 },
            { "type": "sphere", "center": [-0.31, 0.41, 0], "radius": 0.045 } ] })).unwrap();
        let region = |name: &str, center: [f32; 3], r: f32| Region { name: name.into(), center: Vec3::from(center), radii: Vec3::splat(r), cell: 0.00025 };
        c.set_detail_regions(&[region("face", [0.0, 0.9, 0.08], 0.06), region("hand_l", [0.31, 0.41, 0.0], 0.05), region("hand_r", [-0.31, 0.41, 0.0], 0.05)]).unwrap();
        for (at, r) in [([0.0, 0.9, 0.1], 0.07), ([0.31, 0.41, 0.045], 0.05), ([-0.31, 0.41, 0.045], 0.05), ([0.0, 0.6, 0.12], 0.1)] {
            let s: Stroke = serde_json::from_value(json!({ "brush": "noise", "points": [at], "radius": r, "strength": 0.6, "scale": 0.0012, "pattern": "pores" })).unwrap();
            sculpt::apply(&mut c, &s).unwrap();
        }
        let rest = c.build_mesh();
        let estimate = c.fine_estimate(&rest);
        let t = std::time::Instant::now();
        let fine = c.build_fine(&[], "").unwrap();
        let build = t.elapsed();
        assert!(closed(&fine), "the fine mesh has cracks");
        let (mut pts, mut tris) = welded(&fine);
        let raw = crate::mesh_asset::edge_use_kinds(&tris);
        let t = std::time::Instant::now();
        let s = super::separate_pinches(&mut pts, &mut tris, 0.005, 0.02);
        let separate = t.elapsed();
        let (_, tris) = crate::mesh_asset::weld(&pts, &tris);
        let (edges, bad, flipped) = crate::mesh_asset::edge_use(&tris);
        let mut uniform = c.clone();
        uniform.set_detail_regions(&[]).unwrap();
        uniform.detail_cell = Some(0.00025);
        let uniform_estimate = uniform.fine_estimate(&rest);
        println!("body: {} triangles (estimated {estimate:.0}) in {build:?}; welded {raw:?}; separated {s:?} in {separate:?}; {bad} non-manifold, {flipped} flipped of {edges}; uniform 0.25 mm about {uniform_estimate:.0} ({:.1}%)", tris.len(), 100.0 * tris.len() as f64 / uniform_estimate);
        assert!(edges > 0 && bad == 0 && flipped == 0);
        assert!((tris.len() as f64) <= 0.25 * uniform_estimate);
    }

    const OLD_REGULAR: u64 = 0x2695_1b9d_65e7_054b;
    const OLD_FINE: u64 = 0xf3aa_93f6_1c6f_b2a7;

    #[test]
    fn a_detail_region_refines_crack_free_and_keeps_the_detail() {
        let face = Region { name: "face".into(), center: Vec3::new(0.0, 0.0, 0.1), radii: Vec3::splat(0.04), cell: 0.0005 };
        let mut c: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }], "resolution": 64, "detail_cell": 0.002 })).unwrap();
        c.set_detail_regions(std::slice::from_ref(&face)).unwrap();
        let pores: Stroke = serde_json::from_value(json!({ "brush": "noise", "points": [[0, 0, 0.1]], "radius": 0.06, "strength": 0.8, "scale": 0.0015, "pattern": "pores" })).unwrap();
        sculpt::apply(&mut c, &pores).unwrap();
        // The same pores on one uniform 0.5 mm grid, for reference.
        let mut uniform: Clay = serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.1 }], "resolution": 64, "detail_cell": 0.0005 })).unwrap();
        sculpt::apply(&mut uniform, &pores).unwrap();
        let uniform_mesh = uniform.build_fine(&[], "").unwrap();
        let adaptive = c.build_fine(&[], "").unwrap();
        // Saved and loaded, the regions and their detail come back.
        // (Saving rounds offsets to 1/256 cell, so compare two loads.)
        let back: Clay = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        let again: Clay = serde_json::from_str(&serde_json::to_string(&back).unwrap()).unwrap();
        assert_eq!(back.detail_regions(), vec![face.clone()]);
        assert_eq!(hash(&back.build_fine(&[], "").unwrap()), hash(&again.build_fine(&[], "").unwrap()));
        // Closed like the uniform mesh, with far fewer triangles.
        assert!(closed(&uniform_mesh), "uniform mesh not closed");
        assert!(closed(&adaptive), "adaptive mesh has cracks");
        let (na, nu) = (adaptive.indices.len() / 3, uniform_mesh.indices.len() / 3);
        assert!(na * 3 < nu, "adaptive {na} vs uniform {nu} triangles");
        // Inside the region the relief matches the uniform fine mesh; the
        // coarse mesh alone loses most of it.
        let at = Vec3::new(0.0, 0.0, 0.1);
        let (ra, ru) = (relief(&adaptive, at, 0.02), relief(&uniform_mesh, at, 0.02));
        let mut coarse = c.clone();
        coarse.set_detail_regions(&[]).unwrap();
        let rc = relief(&coarse.build_fine(&[], "").unwrap(), at, 0.02);
        println!("relief adaptive {ra} uniform {ru} coarse {rc}; triangles {na} vs {nu}");
        assert!((ra - ru).abs() < 0.25 * ru, "adaptive {ra} uniform {ru}");
        assert!(rc < 0.6 * ru, "coarse {rc} uniform {ru}");
        // Outside the region it is the coarse mesh's spacing.
        let far: Vec<f32> = adaptive.vertices.iter().map(|v| Vec3::from(v.pos)).filter(|p| p.z < -0.05).map(|p| p.z).collect();
        assert!(!far.is_empty());
    }
}

/// Adaptive surface nets: a coarse grid whose cells near the surface are
/// each cut into r^3 subcells (r a power of two per cell, from how fine
/// the detail there is), meshed crack-free across the changes of size.
///
/// This is dual contouring on an octree reduced to surface nets: every
/// *minimal* grid edge (one no finer neighbor cuts into smaller edges)
/// that the surface crosses makes a quad joining the four leaf cells
/// around it, whatever their sizes; a leaf's vertex is the mean of the
/// crossings of the minimal edges around it. Where a coarse cell meets a
/// finer one, its vertex joins the finer cells' quads too, so the surface
/// closes across the change (a quad there may come out as a triangle).
pub(crate) struct Nets<'a> {
    /// Grid point (0, 0, 0) and the coarse cell size.
    pub lo: Vec3,
    pub cell: f32,
    /// Coarse grid points per axis.
    pub n: [usize; 3],
    /// The field at a coarse grid point.
    pub coarse: &'a (dyn Fn(usize, usize, usize) -> f32 + Sync),
    /// The field anywhere (subcell corners), given the blobs `near` a box.
    pub field: &'a (dyn Fn(Vec3, &[u32]) -> f32 + Sync),
    pub near: &'a (dyn Fn(Vec3, Vec3) -> Vec<u32> + Sync),
    /// The subdivision wanted for the coarse cell with this middle.
    pub factor: &'a (dyn Fn(Vec3) -> u32 + Sync),
    /// At most how far `field` is from a distance in a box (the sculpt
    /// offsets there).
    pub margin: &'a (dyn Fn(Vec3, Vec3) -> f32 + Sync),
}

#[cfg(test)]
thread_local! {
    /// Crossings whose four leaves collapsed to fewer than three distinct
    /// vertices on this thread (no face was made for them).
    pub(crate) static COLLAPSED_RINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Most subcells across one coarse cell.
pub(crate) const MAX_FACTOR: u32 = 32;

type Leaf = u64;

/// One surface crossing: where, the four leaves around it in winding
/// order, and which of each leaf's 12 edges it is (`OTHER` when the leaf is
/// bigger than the edge, at a change of size).
struct Crossing {
    at: Vec3,
    leaves: [Leaf; 4],
    edges: [u8; 4],
}

pub(crate) const OTHER: u8 = u8::MAX;

fn corner_point(c: usize) -> Vec3 {
    Vec3::new((c & 1) as f32, ((c >> 1) & 1) as f32, ((c >> 2) & 1) as f32)
}

/// A cell's 12 edges as corner pairs; corner c is at (c & 1, c >> 1 & 1,
/// c >> 2 & 1). Edge `axis * 4 + bu * 2 + bv` runs along `axis` with
/// coordinate `bu` on axis + 1 and `bv` on axis + 2 (mod 3).
pub(crate) fn cube_edge(e: usize) -> (usize, usize) {
    let (axis, bu, bv) = (e / 4, (e >> 1) & 1, e & 1);
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let a = (bu << u) | (bv << v);
    (a, a | (1 << axis))
}

/// Which surface piece inside a cell each of its 12 edges belongs to
/// (`OTHER`: not crossed), from the cell's 8 corner values: crossed edges
/// on a face join up, and a face crossed four times is resolved by its
/// middle value, a rule the cell on the other side of the face agrees on,
/// so each piece gets its own vertex and the surface stays manifold.
pub(crate) fn pieces(f: &[f32; 8]) -> [u8; 12] {
    let inside = |c: usize| f[c] < 0.0;
    let crossed: [bool; 12] = std::array::from_fn(|e| { let (a, b) = cube_edge(e); inside(a) != inside(b) });
    let mut root: [usize; 12] = std::array::from_fn(|e| e);
    fn find(r: &mut [usize; 12], mut i: usize) -> usize {
        while r[i] != i { r[i] = r[r[i]]; i = r[i]; }
        i
    }
    for axis in 0..3 {
        for side in 0..2 {
            let on = |c: usize| (c >> axis) & 1 == side;
            let edges: Vec<usize> = (0..12).filter(|&e| { let (a, b) = cube_edge(e); on(a) && on(b) && crossed[e] }).collect();
            let join = |a: usize, b: usize, r: &mut [usize; 12]| { let (x, y) = (find(r, a), find(r, b)); r[x] = y; };
            match edges.len() {
                2 => join(edges[0], edges[1], &mut root),
                4 => {
                    let corners: Vec<usize> = (0..8).filter(|&c| on(c)).collect();
                    let middle = corners.iter().map(|&c| f[c]).sum::<f32>() / 4.0;
                    // Inside corners connect through the middle when it is
                    // inside: then each outside corner is cut off alone.
                    let lone = middle >= 0.0;
                    for &c in &corners {
                        if inside(c) == lone {
                            let near: Vec<usize> = edges.iter().copied().filter(|&e| { let (a, b) = cube_edge(e); a == c || b == c }).collect();
                            if near.len() == 2 {
                                join(near[0], near[1], &mut root);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = [OTHER; 12];
    let mut labels: Vec<usize> = Vec::new();
    for e in 0..12 {
        if crossed[e] {
            let r = find(&mut root, e);
            let k = labels.iter().position(|&l| l == r).unwrap_or_else(|| { labels.push(r); labels.len() - 1 });
            out[e] = k as u8;
        }
    }
    out
}

impl Nets<'_> {
    fn index(&self, c: [usize; 3]) -> usize {
        c[0] + self.n[0] * (c[1] + self.n[1] * c[2])
    }

    /// Mesh for the coarse `cells` (x, y, z of the cell's low corner) that
    /// can hold surface: positions and triangles. Errs past `max_triangles`.
    pub fn mesh(&self, cells: &[[usize; 3]], max_triangles: usize) -> Result<(Vec<Vec3>, Vec<u32>), String> {
        // Subdivision of each cell that can hold surface (others: 1).
        let c = self.cell;
        let point = |g: [usize; 3]| self.lo + Vec3::new(g[0] as f32, g[1] as f32, g[2] as f32) * c;
        // A subcell corner from its place in units of the finest subcell
        // (c / MAX_FACTOR, a power of two, so exact): the same point read
        // from cells of different sizes is the same float, and so has the
        // same sign where the surface passes through it.
        let finest = c / MAX_FACTOR as f32;
        let corner_at = |g: [usize; 3], sub: [usize; 3], r: usize| {
            let step = MAX_FACTOR as usize / r;
            let n = |a: usize| (g[a] * MAX_FACTOR as usize + sub[a] * step) as f32;
            self.lo + Vec3::new(n(0), n(1), n(2)) * finest
        };
        let mut factors: Vec<([usize; 3], u32)> = cells.iter().map(|&g| (g, 1)).collect();
        par_chunks(&mut factors, 1024, |_, chunk| {
            for (g, r) in chunk {
                let mid = point(*g) + Vec3::splat(c * 0.5);
                *r = (self.factor)(mid).clamp(1, MAX_FACTOR).next_power_of_two().min(MAX_FACTOR);
            }
        });
        let mut factor_of: HashMap<usize, u32> = HashMap::with_capacity(factors.len());
        for &(g, r) in &factors {
            factor_of.insert(self.index(g), r);
        }
        // Balance: a cell is at most twice as coarse as any neighbor, so
        // every change of size is a single halving.
        loop {
            let mut changed = false;
            for (g, r) in factors.iter_mut() {
                let mut need = *r;
                for dz in -1isize..=1 { for dy in -1isize..=1 { for dx in -1isize..=1 {
                    let o = [g[0] as isize + dx, g[1] as isize + dy, g[2] as isize + dz];
                    if o.iter().any(|&v| v < 0) { continue; }
                    if let Some(&ro) = factor_of.get(&self.index(o.map(|v| v as usize))) {
                        need = need.max(ro / 2);
                    }
                }}}
                if need > *r {
                    *r = need;
                    factor_of.insert(self.index(*g), need);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        // About a third of the cells near the surface are crossed by it,
        // each making about 2 r^2 triangles.
        let estimate: f64 = factors.iter().map(|&(_, r)| (r as f64).powi(2) * 2.0).sum::<f64>() / 3.0;
        if estimate > max_triangles as f64 * 1.5 {
            return Err(format!("the fine mesh would need about {estimate:.0} triangles, more than {max_triangles}; use larger detail cells or smaller regions"));
        }
        let r_at = |g: [isize; 3]| -> u32 {
            if g.iter().zip(&self.n).any(|(&v, &n)| v < 0 || v as usize + 1 >= n) {
                return 1;
            }
            factor_of.get(&self.index([g[0] as usize, g[1] as usize, g[2] as usize])).copied().unwrap_or(1)
        };
        // The leaf holding point q: (coarse cell, subcell) packed.
        let leaf = |q: Vec3| -> Leaf {
            let g = (q - self.lo) / c;
            let cell = g.floor();
            let ci = [cell.x as isize, cell.y as isize, cell.z as isize];
            let r = r_at(ci);
            let sub = ((g - cell) * r as f32).floor().clamp(Vec3::ZERO, Vec3::splat(r as f32 - 1.0));
            let ci = ci.map(|v| v.max(0) as usize);
            let s = sub.x as u64 + MAX_FACTOR as u64 * (sub.y as u64 + MAX_FACTOR as u64 * sub.z as u64);
            (self.index(ci) as u64) << 15 | s
        };
        // Each cell's minimal crossing edges, in parallel.
        // Per cell: (where, subdivision, its crossings, its corner values).
        type Work = ([usize; 3], u32, Vec<Crossing>, Vec<f32>);
        let mut work: Vec<Work> = factors.iter().map(|&(g, r)| (g, r, Vec::new(), Vec::new())).collect();
        par_chunks(&mut work, 16, |_, chunk| {
            for (g, r, out, kept) in chunk {
                let (g, r) = (*g, *r);
                let ru = r as usize;
                let h = c / r as f32;
                let origin = point(g);
                let near = (self.near)(origin - Vec3::splat(c), origin + Vec3::splat(2.0 * c));
                // Corner values of the subcells near the surface (coarse grid
                // points from the grid); far subcells stay unread (NaN). An
                // octree walk skips boxes whose middle is too far from the
                // surface for it to pass through: the blobs' field is about
                // a distance, and sculpt offsets (which can be far steeper,
                // like deep pores) move it by at most `margin` either way,
                // so twice that is added to the reach.
                let slack = 2.0 * (self.margin)(origin, origin + Vec3::splat(c));
                let side = ru + 1;
                let mut values = vec![f32::NAN; side * side * side];
                let read = |i: usize, j: usize, k: usize, values: &mut Vec<f32>| {
                    let slot = &mut values[i + side * (j + side * k)];
                    if slot.is_nan() {
                        *slot = if i.is_multiple_of(ru) && j.is_multiple_of(ru) && k.is_multiple_of(ru) {
                            (self.coarse)(g[0] + i / ru, g[1] + j / ru, g[2] + k / ru)
                        } else {
                            (self.field)(corner_at(g, [i, j, k], ru), &near)
                        };
                    }
                };
                let mut stack = vec![([0usize; 3], ru)];
                while let Some((o, size)) = stack.pop() {
                    if size == 1 {
                        for &(dx, dy, dz) in &[(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)] {
                            read(o[0] + dx, o[1] + dy, o[2] + dz, &mut values);
                        }
                        continue;
                    }
                    let mid = origin + (Vec3::new(o[0] as f32, o[1] as f32, o[2] as f32) + Vec3::splat(size as f32 * 0.5)) * h;
                    if (self.field)(mid, &near).abs() > size as f32 * h * 0.9 + h + slack {
                        continue;
                    }
                    let half = size / 2;
                    for k in 0..8 {
                        stack.push(([o[0] + (k & 1) * half, o[1] + ((k >> 1) & 1) * half, o[2] + ((k >> 2) & 1) * half], half));
                    }
                }
                let at = |i: usize, j: usize, k: usize| values[i + side * (j + side * k)];
                let gi = [g[0] as isize, g[1] as isize, g[2] as isize];
                let me = self.index(g);
                for axis in 0..3 {
                    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                    for a in 0..ru {
                        for pu in 0..side {
                            for pv in 0..side {
                                let mut s = [0usize; 3];
                                s[axis] = a;
                                s[u] = pu;
                                s[v] = pv;
                                let mut e = s;
                                e[axis] += 1;
                                let (f0, f1) = (at(s[0], s[1], s[2]), at(e[0], e[1], e[2]));
                                if f0.is_nan() || f1.is_nan() || (f0 < 0.0) == (f1 < 0.0) {
                                    continue;
                                }
                                // Coarse cells sharing this edge: across the
                                // u and v faces it lies on.
                                let du: &[isize] = if pu == 0 { &[0, -1] } else if pu == ru { &[0, 1] } else { &[0] };
                                let dv: &[isize] = if pv == 0 { &[0, -1] } else if pv == ru { &[0, 1] } else { &[0] };
                                let mut minimal = true;
                                let mut owner = true;
                                for &x in du {
                                    for &y in dv {
                                        if x == 0 && y == 0 {
                                            continue;
                                        }
                                        let mut o = gi;
                                        o[u] += x;
                                        o[v] += y;
                                        let ro = r_at(o);
                                        if ro > r {
                                            minimal = false;
                                        } else if ro == r && o.iter().all(|&q| q >= 0) && self.index(o.map(|q| q as usize)) < me {
                                            owner = false;
                                        }
                                    }
                                }
                                if !minimal || !owner {
                                    continue;
                                }
                                let t = f0 / (f0 - f1);
                                let ps = origin + Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32) * h;
                                let mut dir = Vec3::ZERO;
                                dir[axis] = h;
                                let crossing = ps + dir * t;
                                let mid = ps + dir * 0.5;
                                let (mut uu, mut vv) = (Vec3::ZERO, Vec3::ZERO);
                                uu[u] = h * 0.5;
                                vv[v] = h * 0.5;
                                // Winding as the regular nets (`Clay::build_window_cells`).
                                let ring = [mid - uu - vv, mid + uu - vv, mid + uu + vv, mid - uu + vv];
                                let mut leaves = ring.map(leaf);
                                // The edge seen from each leaf: on its high u
                                // side when the leaf lies below in u, and so on.
                                let sides = [(1, 1), (0, 1), (0, 0), (1, 0)];
                                let mut edges: [u8; 4] = std::array::from_fn(|k| {
                                    let q = ring[k];
                                    let gq = (q - self.lo) / c;
                                    let cell = gq.floor();
                                    let rq = r_at([cell.x as isize, cell.y as isize, cell.z as isize]);
                                    if rq == r { (axis * 4 + sides[k].0 * 2 + sides[k].1) as u8 } else { OTHER }
                                });
                                if f0 >= 0.0 {
                                    leaves.reverse();
                                    edges.reverse();
                                }
                                out.push(Crossing { at: crossing, leaves, edges });
                            }
                        }
                    }
                }
                // Kept for splitting leaves into their surface pieces.
                *kept = values;
            }
        });
        let work_of: HashMap<usize, usize> = work.iter().enumerate().map(|(k, w)| (self.index(w.0), k)).collect();
        // A leaf's 8 corner values (None if any was not read).
        let corners = |l: Leaf| -> Option<[f32; 8]> {
            let ci = (l >> 15) as usize;
            let s = (l & 0x7fff) as usize;
            let m = MAX_FACTOR as usize;
            let sub = [s % m, (s / m) % m, s / (m * m)];
            let g = [ci % self.n[0], (ci / self.n[0]) % self.n[1], ci / (self.n[0] * self.n[1])];
            let (r, values) = match work_of.get(&ci) {
                Some(&k) if work[k].1 > 1 => (work[k].1 as usize, Some(&work[k].3)),
                _ => (1, None),
            };
            let side = r + 1;
            let size = c / r as f32;
            let origin = self.lo + Vec3::new(g[0] as f32, g[1] as f32, g[2] as f32) * c + Vec3::new(sub[0] as f32, sub[1] as f32, sub[2] as f32) * size;
            let mut f = [0.0f32; 8];
            let mut near: Option<Vec<u32>> = None;
            for (corner, slot) in f.iter_mut().enumerate() {
                let (dx, dy, dz) = (corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
                *slot = match values {
                    Some(v) => v[(sub[0] + dx) + side * ((sub[1] + dy) + side * (sub[2] + dz))],
                    None => (self.coarse)(g[0] + dx, g[1] + dy, g[2] + dz),
                };
                // Skipped by the walk from its own cell: read it now.
                if slot.is_nan() {
                    let list = near.get_or_insert_with(|| (self.near)(origin - Vec3::splat(size), origin + Vec3::splat(2.0 * size)));
                    *slot = (self.field)(corner_at(g, [sub[0] + dx, sub[1] + dy, sub[2] + dz], r), list);
                }
            }
            Some(f)
        };
        let mut split: HashMap<Leaf, [u8; 12]> = HashMap::new();
        let mut piece = |l: Leaf, e: u8, at: Vec3| -> u64 {
            let labels = *split.entry(l).or_insert_with(|| corners(l).map_or([0; 12], |f| pieces(&f)));
            let k = if e != OTHER && labels[e as usize] != OTHER {
                labels[e as usize]
            } else {
                // A finer edge on this leaf's face (or an unread corner):
                // the piece whose crossed edge is nearest.
                let ci = (l >> 15) as usize;
                let r = work_of.get(&ci).map_or(1, |&k| work[k].1) as f32;
                let s = (l & 0x7fff) as usize;
                let m = MAX_FACTOR as usize;
                let g = [ci % self.n[0], (ci / self.n[0]) % self.n[1], ci / (self.n[0] * self.n[1])];
                let size = c / r;
                let origin = self.lo + Vec3::new(g[0] as f32, g[1] as f32, g[2] as f32) * c + Vec3::new((s % m) as f32, ((s / m) % m) as f32, (s / (m * m)) as f32) * size;
                (0..12).filter(|&q| labels[q] != OTHER).min_by(|&a, &b| {
                    let mid = |q: usize| { let (x, y) = cube_edge(q); origin + (corner_point(x) + corner_point(y)) * 0.5 * size };
                    mid(a).distance_squared(at).total_cmp(&mid(b).distance_squared(at))
                }).map_or(0, |q| labels[q])
            };
            l << 4 | k as u64
        };
        // Leaves become vertices at the mean of their crossings.
        let mut ids: HashMap<u64, u32> = HashMap::new();
        let mut sums: Vec<(Vec3, f32)> = Vec::new();
        let mut indices = Vec::new();
        let crossings: Vec<&Crossing> = work.iter().flat_map(|w| w.2.iter()).collect();
        {
            for x in crossings {
                let keys: [u64; 4] = std::array::from_fn(|k| piece(x.leaves[k], x.edges[k], x.at));
                let q = keys.map(|l| {
                    *ids.entry(l).or_insert_with(|| {
                        sums.push((Vec3::ZERO, 0.0));
                        (sums.len() - 1) as u32
                    })
                });
                for &i in &q {
                    sums[i as usize].0 += x.at;
                    sums[i as usize].1 += 1.0;
                }
                // Drop repeats (a coarse leaf on two corners of the ring).
                let mut ring: Vec<u32> = Vec::with_capacity(4);
                for &i in &q {
                    if ring.last() != Some(&i) {
                        ring.push(i);
                    }
                }
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                match ring.len() {
                    4 if ring[0] != ring[2] && ring[1] != ring[3] => indices.extend_from_slice(&[ring[0], ring[1], ring[2], ring[0], ring[2], ring[3]]),
                    3 => indices.extend_from_slice(&[ring[0], ring[1], ring[2]]),
                    _ => {
                        #[cfg(test)]
                        COLLAPSED_RINGS.with(|n| n.set(n.get() + 1));
                    }
                }
            }
        }
        if indices.len() / 3 > max_triangles {
            return Err(format!("the fine mesh came to {} triangles, more than {max_triangles}; use larger detail cells or smaller regions", indices.len() / 3));
        }
        let positions = sums.iter().map(|(s, n)| *s / n.max(1.0)).collect();
        Ok((positions, indices))
    }
}

/// Where surface nets pinch two sheets together (an edge in four or more
/// triangles, from a saddle too small for the grid, like the rim of a
/// pore), gives each sheet its own copies of the shared corners. Around a
/// pinched edge the triangles pair up by angle into sheets (each pair one
/// in each direction); a corner on several pinched edges tries their
/// pairings together and keeps the one that splits its fan. The surface
/// stays closed and every edge ends up in two triangles. Returns the
/// copies made.
pub(crate) fn make_manifold(positions: &mut Vec<Vec3>, indices: &mut [u32]) -> usize {
    // Corners of each undirected edge, by sorting.
    let mut keys: Vec<(u64, u32)> = Vec::with_capacity(indices.len());
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        for e in 0..3 {
            let (a, b) = (tri[e], tri[(e + 1) % 3]);
            keys.push(((a.min(b) as u64) << 32 | a.max(b) as u64, (t * 3 + e) as u32));
        }
    }
    keys.sort_unstable();
    // Each pinched edge: its ways to pair its corners into sheets.
    let mut options: HashMap<u64, Vec<Vec<(u32, u32)>>> = HashMap::new();
    let mut at_vertex: HashMap<u32, Vec<u64>> = HashMap::new();
    let mut i = 0;
    while i < keys.len() {
        let mut j = i;
        while j < keys.len() && keys[j].0 == keys[i].0 {
            j += 1;
        }
        if j - i > 2 && (j - i) % 2 == 0 {
            let key = keys[i].0;
            let (lo, hi) = ((key >> 32) as u32, key as u32);
            let (pa, pb) = (positions[lo as usize], positions[hi as usize]);
            let axis = (pb - pa).normalize_or(Vec3::X);
            let (u, v) = axis.any_orthonormal_pair();
            // Around the edge by angle: (angle, corner, runs lo -> hi).
            let mut around: Vec<(f32, u32, bool)> = keys[i..j].iter().map(|&(_, c)| {
                let t = (c / 3) as usize;
                let e = (c % 3) as usize;
                let third = indices[t * 3 + (e + 2) % 3];
                let d = positions[third as usize] - pa;
                (d.dot(v).atan2(d.dot(u)), c, indices[t * 3 + e] == lo)
            }).collect();
            around.sort_by(|a, b| a.0.total_cmp(&b.0));
            let n = around.len();
            let mut ways = Vec::new();
            for s in 0..2 {
                let pairs: Vec<(u32, u32)> = (0..n).step_by(2).map(|k| (around[(s + k) % n].1, around[(s + k + 1) % n].1)).collect();
                let valid = (0..n).step_by(2).all(|k| around[(s + k) % n].2 != around[(s + k + 1) % n].2);
                if valid {
                    ways.push(pairs);
                }
            }
            if !ways.is_empty() {
                options.insert(key, ways);
                at_vertex.entry(lo).or_default().push(key);
                at_vertex.entry(hi).or_default().push(key);
            }
        }
        i = j;
    }
    if options.is_empty() {
        return 0;
    }
    let mut fans: HashMap<u32, Vec<u32>> = HashMap::new();
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        for &v in tri {
            if at_vertex.contains_key(&v) {
                fans.entry(v).or_default().push(t as u32);
            }
        }
    }
    let edge_corner = |tri: &[u32], a: u32, b: u32| (0..3).find(|&e| (tri[e] == a && tri[(e + 1) % 3] == b) || (tri[e] == b && tri[(e + 1) % 3] == a));
    fn root(g: &mut [usize], mut i: usize) -> usize {
        while g[i] != i { g[i] = g[g[i]]; i = g[i]; }
        i
    }
    // The fan's groups when its pinched edges pair as `chosen`.
    let groups = |v: u32, fan: &[u32], chosen: &HashMap<u64, usize>, indices: &[u32]| -> Vec<usize> {
        let mut partner: HashMap<u32, u32> = HashMap::new();
        for (&key, &w) in chosen {
            for &(a, b) in &options[&key][w] {
                partner.insert(a, b);
                partner.insert(b, a);
            }
        }
        let pinched = |corner: u32| options.contains_key(&{
            let t = (corner / 3) as usize;
            let e = (corner % 3) as usize;
            let (a, b) = (indices[t * 3 + e], indices[t * 3 + (e + 1) % 3]);
            (a.min(b) as u64) << 32 | a.max(b) as u64
        });
        let mut group: Vec<usize> = (0..fan.len()).collect();
        for (k, &t) in fan.iter().enumerate() {
            let tri = [indices[t as usize * 3], indices[t as usize * 3 + 1], indices[t as usize * 3 + 2]];
            for &x in tri.iter().filter(|&&x| x != v) {
                let Some(e) = edge_corner(&tri, v, x) else { continue };
                let corner = t * 3 + e as u32;
                let other = if pinched(corner) {
                    partner.get(&corner).and_then(|&p| fan.iter().position(|&f| f == p / 3))
                } else {
                    fan.iter().enumerate().position(|(m, &f)| m != k && {
                        let o = [indices[f as usize * 3], indices[f as usize * 3 + 1], indices[f as usize * 3 + 2]];
                        edge_corner(&o, v, x).is_some()
                    })
                };
                if let Some(m) = other {
                    let (ra, rb) = (root(&mut group, k), root(&mut group, m));
                    group[ra] = rb;
                }
            }
        }
        (0..fan.len()).map(|k| root(&mut group, k)).collect()
    };
    let mut fixed: HashMap<u64, usize> = HashMap::new();
    let mut copies = 0;
    let mut order: Vec<u32> = fans.keys().copied().collect();
    order.sort_unstable();
    for v in order {
        let fan = fans[&v].clone();
        let edges = &at_vertex[&v];
        let free: Vec<u64> = edges.iter().copied().filter(|k| !fixed.contains_key(k)).collect();
        // Try every pairing of the edges not fixed yet (few), keep the one
        // with the most groups.
        let combos = 1usize << free.len().min(6);
        let mut best: Option<(usize, HashMap<u64, usize>, Vec<usize>)> = None;
        for m in 0..combos {
            let mut chosen: HashMap<u64, usize> = edges.iter().filter_map(|k| Some((*k, *fixed.get(k)?))).collect();
            for (bit, k) in free.iter().enumerate() {
                let n = options[k].len();
                chosen.insert(*k, if bit < 6 { (m >> bit) & 1 } else { 0 }.min(n - 1));
            }
            let g = groups(v, &fan, &chosen, indices);
            let count = { let mut r = g.clone(); r.sort_unstable(); r.dedup(); r.len() };
            if best.as_ref().is_none_or(|b| count > b.0) {
                best = Some((count, chosen, g));
            }
        }
        let Some((count, chosen, g)) = best else { continue };
        fixed.extend(chosen);
        if count < 2 {
            continue;
        }
        // The first group keeps v; each other gets a copy.
        let mut copy_of: HashMap<usize, u32> = HashMap::new();
        let first = g[0];
        for (k, &t) in fan.iter().enumerate() {
            if g[k] == first {
                continue;
            }
            let id = *copy_of.entry(g[k]).or_insert_with(|| {
                positions.push(positions[v as usize]);
                copies += 1;
                (positions.len() - 1) as u32
            });
            let t = t as usize;
            for c in &mut indices[t * 3..t * 3 + 3] {
                if *c == v {
                    *c = id;
                }
            }
        }
    }
    copies
}

/// What `separate_pinches` did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Separated {
    /// Copies of pinched corners made and moved (a copy with nowhere to
    /// go, its fan without area, stays and is not counted).
    pub copies: usize,
    /// The smallest and largest move.
    pub min_offset: f32,
    pub max_offset: f32,
}

/// Print meshes are welded by position, which merges `make_manifold`'s
/// copies back and pinches the sheets together again. This makes the
/// copies on a welded mesh and moves each one off the shared point, so a
/// weld (here or in a slicer) keeps them apart: by
/// max(`min_offset`, `cell_share` x the mean length of its edges), along
/// the mean normal of its own triangles, toward their area-weighted
/// middle. That is the side its sheet lies on, whether two parts touch
/// (the sheets face each other) or a thin wall closes (they face away);
/// a flat fan goes the way its middle lies from the other sheets' there,
/// and a fan whose normals cancel (a sheet folded onto itself) toward its
/// own middle.
/// The original corner stays; moves are worked out from the unmoved
/// points, so the result does not depend on the order. Returns what was
/// moved.
pub(crate) fn separate_pinches(points: &mut Vec<Vec3>, tris: &mut [[u32; 3]], min_offset: f32, cell_share: f32) -> Separated {
    let first = points.len();
    let mut indices: Vec<u32> = tris.iter().flatten().copied().collect();
    // A corner on two pinches can need a second pass (as for clay meshes).
    for _ in 0..4 {
        if make_manifold(points, &mut indices) == 0 {
            break;
        }
    }
    let copies = points.len() - first;
    if copies == 0 {
        return Separated::default();
    }
    for (t, c) in tris.iter_mut().zip(indices.chunks_exact(3)) {
        *t = [c[0], c[1], c[2]];
    }
    // Every corner at a copy's place (the copies and the one that kept
    // it), by position: copies are bit-equal to their original.
    let key = |p: Vec3| p.to_array().map(f32::to_bits);
    let mut slot: HashMap<[u32; 3], usize> = HashMap::with_capacity(copies * 2);
    let mut at: HashMap<u32, usize> = HashMap::with_capacity(copies * 2);
    for (i, &p) in points.iter().enumerate().skip(first) {
        let next = slot.len();
        let s = *slot.entry(key(p)).or_insert(next);
        at.insert(i as u32, s);
    }
    for (i, &p) in points[..first].iter().enumerate() {
        if let Some(&s) = slot.get(&key(p)) {
            at.insert(i as u32, s);
        }
    }
    // Per corner there: area-weighted middle (sum, area), area normal,
    // edge lengths (sum, count) of its own triangles.
    #[derive(Clone, Copy, Default)]
    struct Fan {
        mid: Vec3,
        area: f32,
        normal: Vec3,
        length: f32,
        edges: u32,
    }
    let mut fans: HashMap<u32, Fan> = HashMap::with_capacity(at.len());
    for t in tris.iter() {
        let [a, b, c] = t.map(|i| points[i as usize]);
        let n = (b - a).cross(c - a);
        let area = n.length() * 0.5;
        for (k, &i) in t.iter().enumerate() {
            if !at.contains_key(&i) {
                continue;
            }
            let f = fans.entry(i).or_default();
            f.mid += (a + b + c) / 3.0 * area;
            f.area += area;
            f.normal += n;
            let p = points[i as usize];
            f.length += p.distance(points[t[(k + 1) % 3] as usize]) + p.distance(points[t[(k + 2) % 3] as usize]);
            f.edges += 2;
        }
    }
    let middle = |f: &Fan, p: Vec3| if f.area > 0.0 { f.mid / f.area } else { p };
    // The middle of all the sheets at each place (summed in index order).
    let mut shared = vec![(Vec3::ZERO, 0.0f32); slot.len()];
    let mut ids: Vec<u32> = fans.keys().copied().collect();
    ids.sort_unstable();
    for i in ids {
        let s = &mut shared[at[&i]];
        s.0 += middle(&fans[&i], points[i as usize]);
        s.1 += 1.0;
    }
    let mut out = Separated { copies: 0, min_offset: f32::INFINITY, max_offset: 0.0 };
    let moved: Vec<Vec3> = (first..points.len())
        .map(|i| {
            let p = points[i];
            let f = fans.get(&(i as u32)).copied().unwrap_or_default();
            let n = f.normal.normalize_or_zero();
            let mid = middle(&f, p);
            let (sum, count) = shared[at[&(i as u32)]];
            let others = (sum - mid) / (count - 1.0).max(1.0);
            let dir = if n != Vec3::ZERO {
                let toward = (mid - p).dot(n);
                let away = (mid - others).dot(n);
                let side = if toward.abs() > 1e-6 * f.length.max(1e-12) { toward } else { away };
                if side < 0.0 { -n } else { n }
            } else {
                // A fan whose normals cancel (a sheet folded back on itself):
                // toward its own middle, else away from the other sheets'.
                let toward = (mid - p).normalize_or_zero();
                if toward != Vec3::ZERO { toward } else { (mid - others).normalize_or_zero() }
            };
            if dir == Vec3::ZERO {
                // Nowhere to go (no area at all): left where it is, and not counted.
                return p;
            }
            let offset = min_offset.max(cell_share * f.length / f.edges.max(1) as f32);
            out.copies += 1;
            out.min_offset = out.min_offset.min(offset);
            out.max_offset = out.max_offset.max(offset);
            p + dir * offset
        })
        .collect();
    points[first..].copy_from_slice(&moved);
    if out.copies == 0 {
        out.min_offset = 0.0;
    }
    out
}
