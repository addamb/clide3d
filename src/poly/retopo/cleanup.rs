//! Cleanup of the traced faces (the plan's step 7) and the fit to the
//! surface (step 8).
//!
//! - Odd faces next to each other merge: two triangles into a quad, a
//!   triangle and a pentagon into a hexagon split into two quads, two
//!   pentagons into an octagon split into three. Merges never cross a
//!   protected edge (one on a crease, guide or border) and never make an
//!   edge that already exists.
//! - Hexagons split into two quads; faces of seven or more sides get a
//!   middle point and a fan of quads (one triangle when odd).
//! - `split` is one Catmull-Clark-style split without smoothing (every
//!   face into one quad per corner), for `pure_quads`.
//! - `fit` moves points onto the surface along their normals, then relaxes
//!   them tangentially (each to its neighbours' middle in its tangent
//!   plane) and moves them onto the surface again, three times. Feature
//!   points slide along their feature only; corners stay.
//!
//! Every pass reads the mesh as it was before the pass (`Topology` of it,
//! in face and edge order) and writes new lists, so the result is a pure
//! function of the input.

use glam::Vec3;

use super::Surface;
use crate::poly::topology::Topology;

/// A polygon mesh being cleaned.
#[derive(Clone, Debug, Default)]
pub struct PolyMesh {
    pub points: Vec<Vec3>,
    pub faces: Vec<Vec<u32>>,
}

impl PolyMesh {
    pub fn topology(&self) -> Topology {
        let mut face_start = vec![0u32];
        let mut corners = Vec::new();
        for f in &self.faces {
            corners.extend_from_slice(f);
            face_start.push(corners.len() as u32);
        }
        Topology::build(self.points.len(), face_start, corners)
    }

    /// Area-weighted vertex normals (Newell's face normals).
    pub fn normals(&self) -> Vec<Vec3> {
        let mut n = vec![Vec3::ZERO; self.points.len()];
        for f in &self.faces {
            let fn_ = crate::poly::subdiv::newell(&self.points, f);
            for &v in f {
                n[v as usize] += fn_;
            }
        }
        n.iter().map(|v| v.normalize_or(Vec3::Z)).collect()
    }

    /// Drops points no face uses, keeping the order of the rest; returns
    /// each old point's new index (u32::MAX for dropped ones).
    pub fn compact(&mut self) -> Vec<u32> {
        let mut slot = vec![u32::MAX; self.points.len()];
        let mut points = Vec::new();
        let mut used = vec![false; self.points.len()];
        for f in &self.faces {
            for &v in f {
                used[v as usize] = true;
            }
        }
        for (v, &u) in used.iter().enumerate() {
            if u {
                slot[v] = points.len() as u32;
                points.push(self.points[v]);
            }
        }
        for f in &mut self.faces {
            for v in f.iter_mut() {
                *v = slot[*v as usize];
            }
        }
        self.points = points;
        slot
    }
}

/// The polygon `f` and `g` make when joined across their shared edge
/// `[a, b]` (None when they share more than that edge's points).
fn joined(f: &[u32], g: &[u32], a: u32, b: u32) -> Option<Vec<u32>> {
    // Rotate f so it runs a → b at its end... f has a then b (its own
    // direction) and g has b then a.
    let rot = |face: &[u32], from: u32, to: u32| -> Option<Vec<u32>> {
        let n = face.len();
        let k = (0..n).find(|&k| face[k] == from && face[(k + 1) % n] == to)?;
        // Starting after `to`, around to `from`.
        Some((0..n).map(|i| face[(k + 1 + i) % n]).collect())
    };
    let (f, g) = match (rot(f, a, b), rot(g, b, a)) {
        (Some(x), Some(y)) => (x, y),
        _ => match (rot(f, b, a), rot(g, a, b)) {
            (Some(x), Some(y)) => (x, y),
            _ => return None,
        },
    };
    // f runs to, ..., from; g runs from, ..., to: joined, f then g's inner points.
    let mut out = f.clone();
    out.extend_from_slice(&g[1..g.len() - 1]);
    let mut sorted = out.clone();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return None;
    }
    Some(out)
}

/// The most a re-split quad's corner may be from square (|cos| of its
/// worst corner). Badness is at most 1, so 1.01 sets no bound: a pairing
/// is judged by the irregular points it leaves (`delta`), and among
/// ladders the least bad is taken (`split_even`); folded quads and
/// collinear corners are refused in `rejoin`. A bound of about 40 degrees
/// (0.77) was measured (2026-10-26, `explore_all`) and refused: quads
/// before the split fell 1 to 6 points (sphere at 8,000 faces 98.34% to
/// 95.66%, head 97.95% to 94.35%, the live head at 2,000 95.51% to
/// 89.82%) and irregular points rose, for no gain in distance.
const MAX_BADNESS: f32 = 1.01;

/// The same for a quad re-split from a turning strip (no bound either,
/// for the same reason).
const TURN_BADNESS: f32 = 1.01;

/// How far a quad is from a square: the largest |cos| of its corners.
fn quad_badness(p: &[Vec3], q: [u32; 4]) -> f32 {
    let mut worst = 0.0f32;
    for k in 0..4 {
        let (a, b, c) = (p[q[(k + 3) % 4] as usize], p[q[k] as usize], p[q[(k + 1) % 4] as usize]);
        let (u, v) = ((a - b).normalize_or_zero(), (c - b).normalize_or_zero());
        worst = worst.max(u.dot(v).abs());
    }
    worst
}

/// A split of a polygon into quads, and the diagonals it adds.
type Ladder = (Vec<[u32; 4]>, Vec<(u32, u32)>);

/// The ways to split an even polygon into quads by a ladder of
/// diagonals between its own points (rungs `s - 1 - i` to `s + 2 + i`),
/// none of them an edge already: each one's quads and rungs.
fn ladders(poly: &[u32], t: &Topology) -> Vec<Ladder> {
    let n = poly.len();
    if n < 4 || n % 2 == 1 {
        return Vec::new();
    }
    let at = |i: isize| poly[i.rem_euclid(n as isize) as usize];
    let count = (n / 2 - 1) as isize;
    let mut out = Vec::new();
    for s in 0..(if n == 4 { 1 } else { n / 2 }) as isize {
        let quads: Vec<[u32; 4]> = (0..count).map(|i| [at(s - i), at(s + 1 + i), at(s + 2 + i), at(s - 1 - i)]).collect();
        let rungs: Vec<(u32, u32)> = (0..count - 1).map(|i| (at(s - 1 - i), at(s + 2 + i))).collect();
        if rungs.iter().all(|&(a, b)| a != b && t.edge_between(a, b).is_none()) {
            out.push((quads, rungs));
        }
    }
    out
}

/// The best ladder of an even polygon (`ladders`) by the badness of its
/// worst quad, among those whose rungs are not in `made` (diagonals other
/// splits in the same pass made); returns the quads and the rungs.
fn split_even(p: &[Vec3], poly: &[u32], t: &Topology, made: &std::collections::BTreeSet<(u32, u32)>) -> Option<Ladder> {
    ladders(poly, t)
        .into_iter()
        .filter(|(_, rungs)| rungs.iter().all(|&(a, b)| !made.contains(&(a.min(b), a.max(b)))))
        .map(|(q, rungs)| {
            let bad = q.iter().map(|q| quad_badness(p, *q)).fold(0.0, f32::max);
            (q, rungs, bad)
        })
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(q, r, _)| (q, r))
}

/// Whether point `v` (with `edges` edges) counts as irregular: an interior
/// point without four edges.
fn irregular(t: &Topology, v: u32, edges: isize) -> bool {
    edges != 4 && !t.edges_of(v as usize).iter().any(|&e| t.is_border(e as usize))
}

/// Joins `path` (faces in order, each sharing an edge with the next)
/// into one polygon and splits it again into quads: the ladder that
/// leaves the fewest irregular points, then the squarest. Returns the
/// quads, the change in irregular points and the worst badness.
fn rejoin(m: &PolyMesh, t: &Topology, normals: &[Vec3], path: &[u32], protected: &dyn Fn(u32, u32) -> bool) -> Option<(Vec<[u32; 4]>, isize, f32)> {
    let mut poly = m.faces[path[0] as usize].clone();
    let mut removed: Vec<[u32; 2]> = Vec::new();
    for w in path.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        let shared = (t.face_start[a]..t.face_start[a + 1]).map(|c| t.corner_edge[c as usize] as usize).find(|&e| t.faces_of_edge(e).contains(&(b as u32)))?;
        let [x, y] = t.edges[shared];
        removed.push([x, y]);
        poly = joined(&poly, &m.faces[b], x, y)?;
    }
    let mut best: Option<(Vec<[u32; 4]>, isize, f32)> = None;
    let options = if poly.len() == 4 { vec![(vec![[poly[0], poly[1], poly[2], poly[3]]], Vec::new())] } else { ladders(&poly, t) };
    for (quads, rungs) in options {
        // A quad with two sides in a row on a line (three of its points
        // along a crease or guide) would fold flat there.
        if quads.iter().any(|q| (0..4).any(|k| protected(q[k], q[(k + 1) % 4]) && protected(q[(k + 1) % 4], q[(k + 2) % 4]))) {
            continue;
        }
        // No quad may face against the surface (its points' normals), or
        // fold: each corner's triangle faces the same way as the quad.
        let folds = quads.iter().any(|q| {
            let up = q.iter().map(|&v| normals[v as usize]).sum::<Vec3>().normalize_or_zero();
            crate::poly::subdiv::newell(&m.points, q).dot(up) <= 0.0
                || (0..4).any(|k| {
                    let (a, b, c) = (m.points[q[(k + 3) % 4] as usize], m.points[q[k] as usize], m.points[q[(k + 1) % 4] as usize]);
                    (c - b).cross(a - b).normalize_or_zero().dot(up) < -0.3
                })
        });
        if folds {
            continue;
        }
        let mut change: std::collections::BTreeMap<u32, isize> = poly.iter().map(|&v| (v, 0)).collect();
        for [x, y] in &removed {
            *change.entry(*x).or_default() -= 1;
            *change.entry(*y).or_default() -= 1;
        }
        for (x, y) in &rungs {
            *change.entry(*x).or_default() += 1;
            *change.entry(*y).or_default() += 1;
        }
        let delta: isize = change
            .iter()
            .map(|(&v, &d)| {
                let before = t.edges_of(v as usize).len() as isize;
                irregular(t, v, before + d) as isize - irregular(t, v, before) as isize
            })
            .sum();
        let bad = quads.iter().map(|q| quad_badness(&m.points, *q)).fold(0.0, f32::max);
        if best.as_ref().is_none_or(|b| (delta, bad) < (b.1, b.2)) {
            best = Some((quads, delta, bad));
        }
    }
    best
}

/// Pairs odd faces (triangles, pentagons) joined by a straight strip of
/// at most `reach` quads, across unprotected edges: the strip and the two
/// faces become one even polygon, split again into quads (a ladder of
/// diagonals) when that leaves at most `worsen` more irregular points.
/// Fewest irregular points first, then short strips and square quads;
/// faces in one change are left for the next pass. Returns how many
/// pairs were joined.
#[allow(clippy::type_complexity)]
pub fn pair_odd(m: &mut PolyMesh, protected: &dyn Fn(u32, u32) -> bool, reach: usize, worsen: isize) -> usize {
    let t = m.topology();
    let normals = m.normals();
    let odd = |f: usize| m.faces[f].len() % 2 == 1;
    // (irregular change, faces joined, badness, the faces, their quads)
    let mut candidates: Vec<(isize, usize, f32, Vec<u32>, Vec<[u32; 4]>)> = Vec::new();
    for f in (0..m.faces.len()).filter(|&f| odd(f) && m.faces[f].len() <= 5) {
        {
            // Breadth first through quads (any side), to the nearest odd faces.
            let mut parent: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
            parent.insert(f as u32, u32::MAX);
            let mut frontier = vec![f as u32];
            for _ in 0..=reach {
                let mut next = Vec::new();
                for &g in &frontier {
                    if g as usize != f && m.faces[g as usize].len() != 4 {
                        continue;
                    }
                    for c in t.face_start[g as usize]..t.face_start[g as usize + 1] {
                        let e = t.corner_edge[c as usize] as usize;
                        let fs = t.faces_of_edge(e);
                        let [a, b] = t.edges[e];
                        if fs.len() != 2 || protected(a, b) {
                            continue;
                        }
                        let h = if fs[0] == g { fs[1] } else { fs[0] };
                        if parent.contains_key(&h) {
                            continue;
                        }
                        parent.insert(h, g);
                        if odd(h as usize) {
                            if h as usize > f && m.faces[h as usize].len() <= 5 {
                                let mut path = vec![h];
                                while let Some(&p) = parent.get(path.last().expect("one")).filter(|&&p| p != u32::MAX) {
                                    path.push(p);
                                }
                                path.reverse();
                                if let Some((quads, delta, bad)) = rejoin(m, &t, &normals, &path, protected)
                                    && delta <= worsen
                                    && (path.len() == 2 || bad <= TURN_BADNESS)
                                {
                                    candidates.push((delta, path.len(), bad, path, quads));
                                }
                            }
                        } else if m.faces[h as usize].len() == 4 {
                            next.push(h);
                        }
                    }
                }
                frontier = next;
            }
        }
        let (s, e) = (t.face_start[f] as usize, t.face_start[f + 1] as usize);
        for c in s..e {
            let mut edge = t.corner_edge[c] as usize;
            let mut path = vec![f as u32];
            loop {
                let fs = t.faces_of_edge(edge);
                let [a, b] = t.edges[edge];
                if fs.len() != 2 || protected(a, b) {
                    break;
                }
                let g = if fs[0] as usize == *path.last().expect("one") as usize { fs[1] } else { fs[0] } as usize;
                if path.contains(&(g as u32)) {
                    break;
                }
                path.push(g as u32);
                if odd(g) {
                    // Each pair once, from its lower face.
                    if g > f
                        && m.faces[g].len() <= 5
                        && let Some((quads, delta, bad)) = rejoin(m, &t, &normals, &path, protected)
                        && delta <= worsen
                        && bad <= MAX_BADNESS
                    {
                        candidates.push((delta, path.len(), bad, path.clone(), quads));
                    }
                    break;
                }
                if m.faces[g].len() != 4 || path.len() > reach + 1 {
                    break;
                }
                // Across the quad: the edge two corners on.
                let gs = t.face_start[g] as usize;
                let k = (0..4).find(|&k| t.corner_edge[gs + k] as usize == edge).expect("the edge is the quad's");
                edge = t.corner_edge[gs + (k + 2) % 4] as usize;
            }
        }
    }
    candidates.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.total_cmp(&y.2)).then(x.3.cmp(&y.3)));
    // Faces in a change go; faces next to one are left alone in this
    // pass too (their points' valences were counted before it).
    let mut gone = vec![false; m.faces.len()];
    let mut taken = vec![false; m.faces.len()];
    let mut made: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
    let mut replace: Vec<Option<Vec<[u32; 4]>>> = vec![None; m.faces.len()];
    let mut count = 0;
    for (_, _, _, path, quads) in candidates {
        if path.iter().any(|&f| taken[f as usize]) {
            continue;
        }
        let new_edges: Vec<(u32, u32)> = quads.iter().flat_map(|q| (0..4).map(move |k| (q[k].min(q[(k + 1) % 4]), q[k].max(q[(k + 1) % 4])))).filter(|&(a, b)| t.edge_between(a, b).is_none()).collect();
        if new_edges.iter().any(|e| made.contains(e)) {
            continue;
        }
        made.extend(new_edges);
        for &f in &path {
            gone[f as usize] = true;
            for &v in &m.faces[f as usize] {
                for &g in t.faces_of(v as usize) {
                    taken[g as usize] = true;
                }
            }
        }
        replace[path[0] as usize] = Some(quads);
        count += 1;
    }
    let mut faces = Vec::with_capacity(m.faces.len());
    for (i, f) in m.faces.iter().enumerate() {
        match &replace[i] {
            Some(quads) => faces.extend(quads.iter().map(|q| q.to_vec())),
            None if gone[i] => {}
            None => faces.push(f.clone()),
        }
    }
    m.faces = faces;
    count
}

/// Splits hexagons into two quads and faces of seven or more sides into a
/// fan of quads around a new middle point (projected later by `fit`).
/// Returns how many faces were split.
pub fn split_large(m: &mut PolyMesh) -> usize {
    let t = m.topology();
    let mut faces = Vec::with_capacity(m.faces.len());
    let mut count = 0;
    // Diagonals made in this pass: two hexagons may both offer one.
    let mut made = std::collections::BTreeSet::new();
    let old = std::mem::take(&mut m.faces);
    for f in old {
        if f.len() == 6
            && let Some((quads, rungs)) = split_even(&m.points, &f, &t, &made)
        {
            made.extend(rungs.iter().map(|&(a, b)| (a.min(b), a.max(b))));
            faces.extend(quads.iter().map(|q| q.to_vec()));
            count += 1;
            continue;
        }
        if f.len() >= 7 {
            let c = f.iter().map(|&v| m.points[v as usize]).sum::<Vec3>() / f.len() as f32;
            let mid = m.points.len() as u32;
            m.points.push(c);
            let n = f.len();
            let mut k = 0;
            while k + 2 <= n {
                faces.push(vec![mid, f[k], f[k + 1], f[(k + 2) % n]]);
                k += 2;
            }
            if n % 2 == 1 {
                faces.push(vec![mid, f[n - 1], f[0]]);
            }
            count += 1;
            continue;
        }
        faces.push(f);
    }
    m.faces = faces;
    count
}

/// One split of every face into a quad per corner (face points at the
/// faces' middles, edge points at the edges' middles): the topology of
/// one Catmull-Clark level, without its smoothing.
pub fn split(m: &PolyMesh) -> PolyMesh {
    let t = m.topology();
    let core = t.refine_core();
    let mut points = m.points.clone();
    for f in 0..t.faces() {
        let face = t.face(f);
        points.push(face.iter().map(|&v| m.points[v as usize]).sum::<Vec3>() / face.len() as f32);
    }
    for &[a, b] in &t.edges {
        points.push((m.points[a as usize] + m.points[b as usize]) * 0.5);
    }
    let faces = core.face_start.windows(2).map(|w| core.corners[w[0] as usize..w[1] as usize].to_vec()).collect();
    PolyMesh { points, faces }
}

/// Where a point may go: anywhere on the surface, along a feature line
/// (`slide` gives the nearest point on it), or nowhere (a corner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hold {
    Free,
    /// On feature `k` (an index `slide` understands).
    Line(u32),
    Fixed,
}

/// Moves `p` onto the surface near it along `n` (within `reach`).
pub fn project(surface: &dyn Surface, p: Vec3, n: Vec3, reach: f32, cell: f32) -> Vec3 {
    let f = surface.near(p, reach);
    let q = crate::adaptive::along(&*f, p, n, 1e-4 * cell, reach);
    if f(q).abs() <= 0.05 * cell { q } else { crate::adaptive::onto(&*f, p, 0.25 * cell, reach) }
}

/// The fit (see the module doc): `rounds` of tangential relaxation, each
/// followed by projection; `slide(k, p)` puts a point back on feature `k`
/// (`plane(k)` is whether chain `k` is on the symmetry plane: its points
/// move onto the surface within the plane). `reach` bounds how far a point moves onto the
/// surface.
#[allow(clippy::too_many_arguments)]
pub fn fit(m: &mut PolyMesh, hold: &[Hold], surface: &dyn Surface, slide: &(dyn Fn(u32, Vec3) -> Vec3 + Sync), plane: &(dyn Fn(u32) -> bool + Sync), reach: f32, cell: f32, rounds: usize, progress: &crate::progress::Progress) -> Result<(), String> {
    let t = m.topology();
    let place_raw = |m: &PolyMesh, normals: &[Vec3], i: usize, p: Vec3| -> Vec3 {
        match hold[i] {
            Hold::Fixed => m.points[i],
            Hold::Line(k) if plane(k) => {
                let n = normals[i] * Vec3::new(0.0, 1.0, 1.0);
                slide(k, project(surface, slide(k, p), n.normalize_or_zero(), reach, cell))
            }
            Hold::Line(k) => slide(k, project(surface, slide(k, p), normals[i], reach, cell)),
            Hold::Free => project(surface, p, normals[i], reach, cell),
        }
    };
    let place = |m: &PolyMesh, normals: &[Vec3], i: usize, p: Vec3| -> Vec3 {
        let q = place_raw(m, normals, i, p);
        // A move that leaves the point farther off the surface (it slid
        // out of reach of it) is not taken.
        let off = |x: Vec3| surface.near(x, reach)(x).abs();
        if off(q) > 0.1 * cell && off(q) > off(m.points[i]) { m.points[i] } else { q }
    };
    let normals = m.normals();
    let first = super::hierarchy::par_map(m.points.len(), |i| place(m, &normals, i, m.points[i]));
    m.points = first;
    for _ in 0..rounds {
        progress.check()?;
        let normals = m.normals();
        let next = super::hierarchy::par_map(m.points.len(), |i| {
            let p = m.points[i];
            let around = t.edges_of(i);
            if around.is_empty() || hold[i] == Hold::Fixed {
                return p;
            }
            let c = around.iter().map(|&e| m.points[t.other(e as usize, i as u32) as usize]).sum::<Vec3>() / around.len() as f32;
            let n = normals[i];
            let d = c - p;
            place(m, &normals, i, p + d - n * n.dot(d))
        });
        m.points = next;
    }
    Ok(())
}

/// Points with two edges (where two faces share two sides in a row) go:
/// their two faces become one. Points no face uses are dropped: returns
/// each old point's new index (u32::MAX for dropped ones), which per-point
/// lists must follow (`remap`).
pub fn dissolve_two(m: &mut PolyMesh) -> Vec<u32> {
    for _ in 0..8 {
        let t = m.topology();
        let mut gone = vec![false; m.faces.len()];
        let mut replace: Vec<Option<Vec<u32>>> = vec![None; m.faces.len()];
        let mut round = 0;
        for v in 0..t.vertices {
            let (edges, faces) = (t.edges_of(v), t.faces_of(v));
            if edges.len() != 2 || faces.len() != 2 || edges.iter().any(|&e| t.is_border(e as usize)) {
                continue;
            }
            let (f, g) = (faces[0] as usize, faces[1] as usize);
            if gone[f] || gone[g] {
                continue;
            }
            let without = |face: &[u32]| face.iter().copied().filter(|&x| x != v as u32).collect::<Vec<u32>>();
            let (a, b) = (t.other(edges[0] as usize, v as u32), t.other(edges[1] as usize, v as u32));
            let Some(poly) = joined(&without(&m.faces[f]), &without(&m.faces[g]), a, b) else { continue };
            if poly.len() < 3 {
                continue;
            }
            gone[f] = true;
            gone[g] = true;
            replace[f] = Some(poly);
            round += 1;
        }
        if round == 0 {
            break;
        }
        let mut faces = Vec::with_capacity(m.faces.len());
        for (i, f) in m.faces.iter().enumerate() {
            match &replace[i] {
                Some(p) => faces.push(p.clone()),
                None if gone[i] => {}
                None => faces.push(f.clone()),
            }
        }
        m.faces = faces;
    }
    m.compact()
}

/// A per-point list after `compact` or `dissolve_two` (`map`: each old
/// point's new index), `n` points long now.
pub fn remap<T: Copy>(values: &[T], map: &[u32], n: usize, default: T) -> Vec<T> {
    let mut out = vec![default; n];
    for (old, &new) in map.iter().enumerate() {
        if new != u32::MAX {
            out[new as usize] = values[old];
        }
    }
    out
}

/// Puts every border point of a half (from `cut_half`: its border is
/// where it meets the plane) exactly on x = 0, so `mirror` joins it to its
/// image. Returns how many moved.
pub fn seal_plane(m: &mut PolyMesh) -> usize {
    let t = m.topology();
    let mut moved = 0;
    for v in 0..m.points.len() {
        if m.points[v].x != 0.0 && t.edges_of(v).iter().any(|&e| t.is_border(e as usize)) {
            m.points[v].x = 0.0;
            moved += 1;
        }
    }
    moved
}

/// The half of a mesh on x >= 0, for a mirror-symmetric result: points
/// within a third of their lattice step (`local`) of the plane count as
/// on it, faces with a point beyond it are kept, and the points where the
/// kept faces meet the others go onto the plane (x = 0). Refused when a
/// face crosses the plane or lies in it, or the half's border is pinched.
/// Returns the half and each old point's new index (u32::MAX if gone).
pub fn cut_half(m: &PolyMesh, local: &[f32]) -> Result<(PolyMesh, Vec<u32>), String> {
    let side = |v: u32| {
        let (x, h) = (m.points[v as usize].x, local[v as usize]);
        if x > 0.3 * h { 1 } else if x < -0.3 * h { -1 } else { 0 }
    };
    let mut kept = Vec::new();
    let mut used = vec![0u8; m.points.len()];
    for f in &m.faces {
        let (pos, neg) = (f.iter().any(|&v| side(v) > 0), f.iter().any(|&v| side(v) < 0));
        match (pos, neg) {
            (true, true) => {
                return Err("a face crosses the symmetry plane (the edge flow found no loop along it)".into());
            }
            (false, false) => return Err("a face lies in the symmetry plane".into()),
            (true, false) => {
                kept.push(f.clone());
                for &v in f {
                    used[v as usize] |= 1;
                }
            }
            (false, true) => {
                for &v in f {
                    used[v as usize] |= 2;
                }
            }
        }
    }
    if kept.is_empty() {
        return Err("nothing lies on the x > 0 side".into());
    }
    let mut out = PolyMesh { points: m.points.clone(), faces: kept };
    for (v, &u) in used.iter().enumerate() {
        if u == 3 {
            out.points[v].x = 0.0;
        } else if u == 1 && out.points[v].x < 0.0 {
            out.points[v].x = -out.points[v].x;
        }
    }
    let map = out.compact();
    let t = out.topology();
    for v in 0..t.vertices {
        let mut fans = t.fans(v);
        fans.sort_unstable();
        fans.dedup();
        if fans.len() > 1 {
            return Err("the half's border is pinched at a point".into());
        }
    }
    let (over, flipped) = t.non_manifold();
    if over + flipped > 0 {
        return Err("the half is not a manifold".into());
    }
    Ok((out, map))
}

/// A half (from `cut_half`) and its mirror image, joined at the points on
/// the plane (x = 0 on the border); the mirror's faces wound the other way.
/// Returns the whole and the largest distance between a mirrored point
/// and its image (0 when exact).
pub fn mirror(m: &PolyMesh) -> (PolyMesh, f32) {
    let t = m.topology();
    let n = m.points.len();
    let shared: Vec<bool> = (0..n).map(|v| m.points[v].x == 0.0 && t.edges_of(v).iter().any(|&e| t.is_border(e as usize))).collect();
    let mut points = m.points.clone();
    let mut image = vec![0u32; n];
    for v in 0..n {
        image[v] = if shared[v] {
            v as u32
        } else {
            points.push(Vec3::new(-m.points[v].x, m.points[v].y, m.points[v].z));
            points.len() as u32 - 1
        };
    }
    let mut faces = m.faces.clone();
    for f in &m.faces {
        let mut g: Vec<u32> = f.iter().map(|&v| image[v as usize]).collect();
        g[1..].reverse();
        faces.push(g);
    }
    let err = (0..n).map(|v| {
        let p = m.points[v];
        Vec3::new(-p.x, p.y, p.z).distance(points[image[v] as usize])
    }).fold(0.0, f32::max);
    (PolyMesh { points, faces }, err)
}
