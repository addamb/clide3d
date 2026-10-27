//! The retopology's input (the plan's step 1): the clay's mesh decimated
//! with meshopt to about the triangles wanted, then remeshed isotropically
//! (Botsch and Kobbelt 2004: long edges split, short ones collapsed,
//! edges flipped toward valence 6, points relaxed tangentially and moved
//! back onto the surface), so the field solve sees well-shaped triangles
//! of one size and no edge is long against the lattice.
//!
//! Crease edges (dihedral over `keep_creases`) are kept: they split into
//! crease halves, never flip, collapse only along themselves, and their
//! points relax only along them; corners (where creases meet or turn)
//! never move.
//!
//! Every pass is sequential in a fixed order, or (relaxation) a pure
//! function per point, so the mesh is the same on any number of threads.

use std::collections::BTreeSet;

use glam::Vec3;

use super::Surface;
use super::cleanup::project;
use super::hierarchy::par_map;

/// What a point may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lock {
    Free,
    /// On crease edges (two of them).
    Crease,
    /// Where creases meet, end or turn sharply.
    Corner,
}

/// A triangle mesh with its crease edges.
#[derive(Clone, Debug, Default)]
pub struct TriMesh {
    pub points: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
    /// Crease edges, lower point first.
    pub creases: BTreeSet<(u32, u32)>,
    /// How far along a crease its turn is measured (`locks`).
    pub span: f32,
}

fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn normal(p: &[Vec3], t: [u32; 3]) -> Vec3 {
    (p[t[1] as usize] - p[t[0] as usize]).cross(p[t[2] as usize] - p[t[0] as usize])
}

/// Edges with the triangles on them: (edge, first triangle, second or
/// u32::MAX), sorted by edge.
fn edge_tris(tris: &[[u32; 3]]) -> Vec<((u32, u32), u32, u32)> {
    let mut all: Vec<((u32, u32), u32)> = tris.iter().enumerate().flat_map(|(i, t)| (0..3).map(move |k| (key(t[k], t[(k + 1) % 3]), i as u32))).collect();
    all.sort_unstable();
    let mut out: Vec<((u32, u32), u32, u32)> = Vec::with_capacity(all.len() / 2 + 1);
    for (e, t) in all {
        match out.last_mut() {
            Some(last) if last.0 == e => {
                if last.2 == u32::MAX {
                    last.2 = t;
                }
            }
            _ => out.push((e, t, u32::MAX)),
        }
    }
    out
}

/// Vertex → triangles (CSR, in triangle order).
fn vertex_tris(n: usize, tris: &[[u32; 3]]) -> (Vec<u32>, Vec<u32>) {
    let mut start = vec![0u32; n + 1];
    for t in tris {
        for &v in t {
            start[v as usize + 1] += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut out = vec![0u32; start[n] as usize];
    for (i, t) in tris.iter().enumerate() {
        for &v in t {
            out[fill[v as usize] as usize] = i as u32;
            fill[v as usize] += 1;
        }
    }
    (start, out)
}

impl TriMesh {
    /// Crease edges: those whose two triangles meet at more than `angle`
    /// degrees, less chains of fewer than `min_edges` edges.
    pub fn find_creases(&mut self, angle: f32, min_edges: usize) {
        self.creases.clear();
        if angle >= 180.0 {
            return;
        }
        let cos = angle.to_radians().cos();
        let mut found: Vec<(u32, u32)> = Vec::new();
        for (e, a, b) in edge_tris(&self.tris) {
            if b == u32::MAX {
                found.push(e);
                continue;
            }
            if self.dihedral_cos(a, b).is_some_and(|c| c < cos) {
                found.push(e);
            }
        }
        // Chains: union-find over the crease edges' points.
        let mut root: Vec<u32> = (0..self.points.len() as u32).collect();
        fn find(r: &mut [u32], mut x: u32) -> u32 {
            while r[x as usize] != x {
                r[x as usize] = r[r[x as usize] as usize];
                x = r[x as usize];
            }
            x
        }
        for &(a, b) in &found {
            let (x, y) = (find(&mut root, a), find(&mut root, b));
            if x != y {
                root[x.max(y) as usize] = x.min(y);
            }
        }
        let mut size = vec![0usize; self.points.len()];
        for &(a, _) in &found {
            let r = find(&mut root, a);
            size[r as usize] += 1;
        }
        self.creases = found.into_iter().filter(|&(a, _)| size[find(&mut root, a) as usize] >= min_edges).collect();
    }

    /// Joins crease ends (points on one crease edge) across gaps: each end,
    /// in index order, to the other end within `reach` that best continues
    /// its crease (ahead of it, facing back), by the shortest path of mesh
    /// edges, whose edges become creases. Returns the gaps joined.
    pub fn bridge_gaps(&mut self, reach: f32) -> usize {
        let n = self.points.len();
        let mut along: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(a, b) in &self.creases {
            along[a as usize].push(b);
            along[b as usize].push(a);
        }
        let ends: Vec<u32> = (0..n as u32).filter(|&v| along[v as usize].len() == 1).collect();
        if ends.len() < 2 {
            return 0;
        }
        // Each end's outward direction: from the point `span` back along
        // its crease (a zigzag's last edge points anywhere).
        let out_dir = |v: u32| {
            let (mut prev, mut cur) = (v, along[v as usize][0]);
            let mut length = self.points[v as usize].distance(self.points[cur as usize]);
            for _ in 0..64 {
                if length >= self.span || along[cur as usize].len() != 2 {
                    break;
                }
                let next = if along[cur as usize][0] == prev { along[cur as usize][1] } else { along[cur as usize][0] };
                length += self.points[cur as usize].distance(self.points[next as usize]);
                (prev, cur) = (cur, next);
            }
            (self.points[v as usize] - self.points[cur as usize]).normalize_or_zero()
        };
        let mut ring: Vec<Vec<u32>> = vec![Vec::new(); n];
        for t in &self.tris {
            for k in 0..3 {
                ring[t[k] as usize].push(t[(k + 1) % 3]);
                ring[t[k] as usize].push(t[(k + 2) % 3]);
            }
        }
        for r in &mut ring {
            r.sort_unstable();
            r.dedup();
        }
        let mut used = vec![false; n];
        let mut joined = 0;
        for &e in &ends {
            if used[e as usize] {
                continue;
            }
            let (pe, de) = (self.points[e as usize], out_dir(e));
            let best = ends
                .iter()
                .copied()
                .filter(|&f| f != e && !used[f as usize])
                .filter_map(|f| {
                    let d = self.points[f as usize] - pe;
                    let l = d.length();
                    let u = d / l.max(1e-30);
                    // Ahead of e, and f's crease runs back toward e.
                    (l <= reach && u.dot(de) > 0.3 && (-u).dot(out_dir(f)) > 0.3).then_some((l * (2.0 - u.dot(de)), f))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let Some((_, f)) = best else { continue };
            // Shortest edge path from e to f within reach of e (Dijkstra).
            let mut dist: std::collections::BTreeMap<u32, (f32, u32)> = std::collections::BTreeMap::new();
            let mut heap = std::collections::BinaryHeap::new();
            dist.insert(e, (0.0, u32::MAX));
            heap.push((std::cmp::Reverse(OrdF32(0.0)), std::cmp::Reverse(e)));
            let mut found = false;
            while let Some((std::cmp::Reverse(OrdF32(d)), std::cmp::Reverse(v))) = heap.pop() {
                if v == f {
                    found = true;
                    break;
                }
                if dist.get(&v).is_some_and(|x| x.0 < d) {
                    continue;
                }
                for &w in &ring[v as usize] {
                    if self.points[w as usize].distance(pe) > 1.5 * reach {
                        continue;
                    }
                    let nd = d + self.points[v as usize].distance(self.points[w as usize]);
                    if dist.get(&w).is_none_or(|x| nd < x.0) {
                        dist.insert(w, (nd, v));
                        heap.push((std::cmp::Reverse(OrdF32(nd)), std::cmp::Reverse(w)));
                    }
                }
            }
            if !found {
                continue;
            }
            let mut v = f;
            while v != e {
                let p = dist[&v].1;
                self.creases.insert(key(v, p));
                v = p;
            }
            used[e as usize] = true;
            used[f as usize] = true;
            joined += 1;
        }
        joined
    }

    /// The angle (cosine) between the two triangles on an edge, None when
    /// either is too thin to have a reliable normal.
    fn dihedral_cos(&self, a: u32, b: u32) -> Option<f32> {
        let n = |t: u32| {
            let tri = self.tris[t as usize];
            let c = normal(&self.points, tri);
            let longest = (0..3).map(|k| self.points[tri[k] as usize].distance_squared(self.points[tri[(k + 1) % 3] as usize])).fold(0.0, f32::max);
            (c.length() > 1e-3 * longest).then(|| c.normalize())
        };
        Some(n(a)?.dot(n(b)?))
    }

    /// Each point's lock (see `Lock`): a point on two crease edges is on
    /// a crease unless the crease turns there by more than 45 degrees,
    /// measured over `span` of the crease either way (so a crease that
    /// zigzags within a cell, as surface nets leave a curved rim, is still
    /// one crease) and more than at its crease neighbours; on one, three
    /// or more crease edges it is a corner.
    pub fn locks(&self) -> Vec<Lock> {
        let n = self.points.len();
        let mut along: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(a, b) in &self.creases {
            along[a as usize].push(b);
            along[b as usize].push(a);
        }
        // The point `span` along the crease from v, leaving through w.
        let walk = |v: u32, w: u32| -> Vec3 {
            let (mut prev, mut cur) = (v, w);
            let mut length = self.points[v as usize].distance(self.points[w as usize]);
            for _ in 0..64 {
                if length >= self.span || along[cur as usize].len() != 2 {
                    break;
                }
                let next = if along[cur as usize][0] == prev { along[cur as usize][1] } else { along[cur as usize][0] };
                length += self.points[cur as usize].distance(self.points[next as usize]);
                (prev, cur) = (cur, next);
            }
            self.points[cur as usize]
        };
        let turn: Vec<f32> = (0..n)
            .map(|v| match along[v][..] {
                [a, b] => {
                    let p = self.points[v];
                    let (da, db) = ((p - walk(v as u32, a)).normalize_or_zero(), (walk(v as u32, b) - p).normalize_or_zero());
                    1.0 - da.dot(db)
                }
                _ => 0.0,
            })
            .collect();
        let sharp = 1.0 - std::f32::consts::FRAC_1_SQRT_2;
        (0..n)
            .map(|v| match along[v].len() {
                0 => Lock::Free,
                2 => {
                    let peak = along[v].iter().all(|&w| (turn[v], std::cmp::Reverse(v)) > (turn[w as usize], std::cmp::Reverse(w as usize)));
                    if turn[v] > sharp && peak { Lock::Corner } else { Lock::Crease }
                }
                _ => Lock::Corner,
            })
            .collect()
    }

    /// Splits every edge longer than `max` at its middle (repeated until
    /// none is, at most `rounds` times): triangles with one, two or three
    /// long edges split into two, three or four.
    pub fn split_long(&mut self, max: f32, rounds: usize) {
        for _ in 0..rounds {
            let edges = edge_tris(&self.tris);
            let mut mid: Vec<((u32, u32), u32)> = Vec::new();
            for &(e, _, _) in &edges {
                if self.points[e.0 as usize].distance(self.points[e.1 as usize]) > max {
                    mid.push((e, self.points.len() as u32));
                    self.points.push((self.points[e.0 as usize] + self.points[e.1 as usize]) * 0.5);
                    if self.creases.remove(&e) {
                        let m = self.points.len() as u32 - 1;
                        self.creases.insert(key(e.0, m));
                        self.creases.insert(key(m, e.1));
                    }
                }
            }
            if mid.is_empty() {
                return;
            }
            let at = |a: u32, b: u32| mid.binary_search_by_key(&key(a, b), |m| m.0).ok().map(|i| mid[i].1);
            let mut tris = Vec::with_capacity(self.tris.len() * 2);
            for &t in &self.tris {
                let m: [Option<u32>; 3] = std::array::from_fn(|k| at(t[k], t[(k + 1) % 3]));
                match m.iter().filter(|x| x.is_some()).count() {
                    0 => tris.push(t),
                    1 => {
                        let k = (0..3).find(|&k| m[k].is_some()).expect("one");
                        let (a, b, c, x) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3], m[k].expect("one"));
                        tris.push([a, x, c]);
                        tris.push([x, b, c]);
                    }
                    2 => {
                        // The unsplit edge is k → k + 1.
                        let k = (0..3).find(|&k| m[k].is_none()).expect("one");
                        let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
                        let (x, y) = (m[(k + 1) % 3].expect("split"), m[(k + 2) % 3].expect("split"));
                        // b → x → c → y → a: the corner triangle at c, and
                        // the quad a b x y split on its shorter diagonal.
                        tris.push([x, c, y]);
                        let p = &self.points;
                        if p[a as usize].distance(p[x as usize]) <= p[b as usize].distance(p[y as usize]) {
                            tris.push([a, b, x]);
                            tris.push([a, x, y]);
                        } else {
                            tris.push([b, x, y]);
                            tris.push([b, y, a]);
                        }
                    }
                    _ => {
                        let (x, y, z) = (m[0].expect("split"), m[1].expect("split"), m[2].expect("split"));
                        tris.extend_from_slice(&[[t[0], x, z], [x, t[1], y], [z, y, t[2]], [x, y, z]]);
                    }
                }
            }
            self.tris = tris;
        }
    }

    /// Collapses edges shorter than `min` (shortest first), where the
    /// mesh stays a manifold of the same topology, no edge passes `max`,
    /// no triangle turns over, and creases keep their shape. One pass:
    /// points around a collapse wait for the next.
    pub fn collapse_short(&mut self, min: f32, max: f32) -> usize {
        let locks = self.locks();
        let (vs, vt) = vertex_tris(self.points.len(), &self.tris);
        let tris_of = |v: u32| &vt[vs[v as usize] as usize..vs[v as usize + 1] as usize];
        let ring = |v: u32, tris: &[[u32; 3]]| -> Vec<u32> {
            let mut r: Vec<u32> = tris_of(v).iter().flat_map(|&t| tris[t as usize]).filter(|&x| x != v).collect();
            r.sort_unstable();
            r.dedup();
            r
        };
        let mut edges: Vec<(f32, (u32, u32))> = edge_tris(&self.tris)
            .into_iter()
            .filter(|e| e.2 != u32::MAX)
            .map(|(e, _, _)| (self.points[e.0 as usize].distance(self.points[e.1 as usize]), e))
            .filter(|(l, _)| *l < min)
            .collect();
        edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut dirty = vec![false; self.points.len()];
        let mut dead = vec![false; self.tris.len()];
        let mut count = 0;
        let tris_now = self.tris.clone();
        for (_, (a, b)) in edges {
            if dirty[a as usize] || dirty[b as usize] {
                continue;
            }
            let crease = self.creases.contains(&(a, b));
            let (la, lb) = (locks[a as usize], locks[b as usize]);
            // (removed, kept, where the kept point goes)
            let (r, k, at) = match (la, lb) {
                (Lock::Free, Lock::Free) => (b, a, (self.points[a as usize] + self.points[b as usize]) * 0.5),
                (Lock::Free, _) => (a, b, self.points[b as usize]),
                (_, Lock::Free) => (b, a, self.points[a as usize]),
                (Lock::Crease, Lock::Crease) | (Lock::Crease, Lock::Corner) if crease => (a, b, self.points[b as usize]),
                (Lock::Corner, Lock::Crease) if crease => (b, a, self.points[a as usize]),
                _ => continue,
            };
            let (ra, rk) = (ring(r, &tris_now), ring(k, &tris_now));
            if ra.iter().chain(&rk).any(|&x| dirty[x as usize]) {
                continue;
            }
            // Link condition: the edge's two far points only.
            let far: Vec<u32> = ra.iter().copied().filter(|x| rk.binary_search(x).is_ok()).collect();
            // The kept point keeps at least three edges, the far points
            // (which lose one) too.
            if far.len() != 2 || rk.len() + ra.len() < 7 || far.iter().any(|&x| ring(x, &tris_now).len() < 4) {
                continue;
            }
            if ra.iter().any(|&x| x != k && self.points[x as usize].distance(at) > max) {
                continue;
            }
            // No triangle turns over or folds flat.
            let ok = tris_of(r).iter().chain(tris_of(k)).all(|&t| {
                let tri = tris_now[t as usize];
                if tri.contains(&r) && tri.contains(&k) {
                    return true;
                }
                let before = normal(&self.points, tri);
                let moved: [Vec3; 3] = std::array::from_fn(|i| if tri[i] == r || tri[i] == k { at } else { self.points[tri[i] as usize] });
                let after = (moved[1] - moved[0]).cross(moved[2] - moved[0]);
                after.dot(before) > 0.0 && after.normalize_or_zero().dot(before.normalize_or_zero()) > 0.5
            });
            if !ok {
                continue;
            }
            for &t in tris_of(r) {
                let tri = tris_now[t as usize];
                if tri.contains(&k) {
                    dead[t as usize] = true;
                }
            }
            for &t in tris_of(r) {
                if !dead[t as usize] {
                    for v in self.tris[t as usize].iter_mut() {
                        if *v == r {
                            *v = k;
                        }
                    }
                }
            }
            let moved: Vec<(u32, u32)> = ra.iter().filter(|&&x| self.creases.contains(&key(r, x))).map(|&x| key(r, x)).collect();
            for e in moved {
                self.creases.remove(&e);
                let other = if e.0 == r { e.1 } else { e.0 };
                if other != k {
                    self.creases.insert(key(k, other));
                }
            }
            self.points[k as usize] = at;
            for &x in ra.iter().chain(&rk).chain([&r, &k]) {
                dirty[x as usize] = true;
            }
            count += 1;
        }
        let mut i = 0;
        self.tris.retain(|_| {
            i += 1;
            !dead[i - 1]
        });
        count
    }

    /// Flips edges (not creases) whose flip brings the four points'
    /// valences nearer 6, where the flip turns no triangle over. One pass.
    pub fn flip(&mut self) -> usize {
        let edges = edge_tris(&self.tris);
        let mut valence = vec![0i32; self.points.len()];
        for &(e, _, _) in &edges {
            valence[e.0 as usize] += 1;
            valence[e.1 as usize] += 1;
        }
        let exists: BTreeSet<(u32, u32)> = edges.iter().map(|e| e.0).collect();
        let mut dirty = vec![false; self.tris.len()];
        let mut count = 0;
        let mut added: BTreeSet<(u32, u32)> = BTreeSet::new();
        for &((a, b), t0, t1) in &edges {
            if t1 == u32::MAX || dirty[t0 as usize] || dirty[t1 as usize] || self.creases.contains(&(a, b)) {
                continue;
            }
            let opp = |t: [u32; 3]| t.into_iter().find(|&v| v != a && v != b).expect("a triangle");
            let (c, d) = (opp(self.tris[t0 as usize]), opp(self.tris[t1 as usize]));
            if c == d || exists.contains(&key(c, d)) || added.contains(&key(c, d)) {
                continue;
            }
            let dev = |v: u32, d: i32| ((valence[v as usize] + d) - 6).abs();
            let before = dev(a, 0) + dev(b, 0) + dev(c, 0) + dev(d, 0);
            let after = dev(a, -1) + dev(b, -1) + dev(c, 1) + dev(d, 1);
            if after >= before || valence[a as usize] <= 3 || valence[b as usize] <= 3 {
                continue;
            }
            // t0 runs a → b (or b → a); keep the winding.
            let t = self.tris[t0 as usize];
            let forward = (0..3).any(|k| t[k] == a && t[(k + 1) % 3] == b);
            let (x, y) = if forward { (a, b) } else { (b, a) };
            // t0 = x y c, t1 = y x d → x d c and y c d.
            let (n0, n1) = (normal(&self.points, [x, y, c]), normal(&self.points, [y, x, d]));
            let (m0, m1) = (normal(&self.points, [x, d, c]), normal(&self.points, [y, c, d]));
            let up = (n0 + n1).normalize_or_zero();
            if m0.normalize_or_zero().dot(up) < 0.5 || m1.normalize_or_zero().dot(up) < 0.5 {
                continue;
            }
            self.tris[t0 as usize] = [x, d, c];
            self.tris[t1 as usize] = [y, c, d];
            for v in [a, b] {
                valence[v as usize] -= 1;
            }
            for v in [c, d] {
                valence[v as usize] += 1;
            }
            added.insert(key(c, d));
            dirty[t0 as usize] = true;
            dirty[t1 as usize] = true;
            count += 1;
        }
        count
    }

    /// Points no triangle uses go (creases renumbered).
    pub fn compact(&mut self) {
        let mut slot = vec![u32::MAX; self.points.len()];
        let mut points = Vec::new();
        for t in &mut self.tris {
            for v in t.iter_mut() {
                if slot[*v as usize] == u32::MAX {
                    slot[*v as usize] = points.len() as u32;
                    points.push(self.points[*v as usize]);
                }
                *v = slot[*v as usize];
            }
        }
        self.creases = self.creases.iter().filter(|e| slot[e.0 as usize] != u32::MAX && slot[e.1 as usize] != u32::MAX).map(|e| key(slot[e.0 as usize], slot[e.1 as usize])).collect();
        self.points = points;
    }

    /// Free points move to their neighbours' middle in their tangent
    /// plane, then onto the surface along their normal; crease points to
    /// the middle of their two crease neighbours (then onto the surface);
    /// corners stay.
    pub fn relax(&mut self, surface: &dyn Surface, reach: f32, cell: f32) {
        let locks = self.locks();
        let normals = super::vertex_normals(&self.points, &self.tris);
        let mut ring: Vec<Vec<u32>> = vec![Vec::new(); self.points.len()];
        for t in &self.tris {
            for k in 0..3 {
                ring[t[k] as usize].push(t[(k + 1) % 3]);
            }
        }
        let mut along: Vec<Vec<u32>> = vec![Vec::new(); self.points.len()];
        for &(a, b) in &self.creases {
            along[a as usize].push(b);
            along[b as usize].push(a);
        }
        let p = &self.points;
        self.points = par_map(p.len(), |i| {
            let v = p[i];
            match locks[i] {
                Lock::Corner => v,
                Lock::Crease => {
                    let c = (p[along[i][0] as usize] + p[along[i][1] as usize]) * 0.5;
                    // Along the crease only: its direction there.
                    let dir = (p[along[i][1] as usize] - p[along[i][0] as usize]).normalize_or_zero();
                    v + dir * dir.dot(c - v) * 0.5
                }
                Lock::Free => {
                    if ring[i].is_empty() {
                        return v;
                    }
                    let c = ring[i].iter().map(|&j| p[j as usize]).sum::<Vec3>() / ring[i].len() as f32;
                    let n = normals[i];
                    let d = c - v;
                    project(surface, v + d - n * n.dot(d), n, reach, cell)
                }
            }
        });
    }
}

/// The input mesh for a retopology: `points`/`tris` (closed, welded)
/// decimated with meshopt to about `2 × vertices` triangles when larger,
/// creases over `keep_creases` degrees found, then `rounds` of isotropic
/// remeshing to edges of about `length`.
#[allow(clippy::too_many_arguments)]
pub fn prepare(points: &[Vec3], tris: &[[u32; 3]], vertices: usize, length: f32, keep_creases: f32, surface: &dyn Surface, cell: f32, rounds: usize, progress: &crate::progress::Progress) -> Result<TriMesh, String> {
    let mut m = TriMesh { points: points.to_vec(), tris: tris.to_vec(), creases: BTreeSet::new(), span: 1.5 * length };
    // Creases are found on the mesh as it comes: decimation keeps the
    // points it keeps (by index) but leaves thin triangles on flat parts,
    // whose normals would make false creases.
    m.find_creases(keep_creases, 3);
    let target = 2 * vertices;
    if m.tris.len() > target + target / 4 {
        let mut on_crease = vec![false; m.points.len()];
        for &(a, b) in &m.creases {
            on_crease[a as usize] = true;
            on_crease[b as usize] = true;
        }
        let indices: Vec<u32> = m.tris.iter().flatten().copied().collect();
        let adapter = meshopt::VertexDataAdapter::new(bytemuck::cast_slice(&m.points), 12, 0).map_err(|e| e.to_string())?;
        let out = meshopt::simplify(&indices, &adapter, target * 3, 1.0, meshopt::SimplifyOptions::None, None);
        if out.len() < 3 * 4 {
            return Err("simplifying the clay's mesh removed every triangle".into());
        }
        m.tris = out.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).filter(|t| t[0] != t[1] && t[1] != t[2] && t[2] != t[0]).collect();
        // A decimated edge is a crease where both its points were on
        // creases and its triangles still meet at a crease's angle (a
        // chord across a flat cap does not).
        let half = (keep_creases * 0.5).to_radians().cos();
        m.creases = edge_tris(&m.tris)
            .into_iter()
            .filter(|&((a, b), t0, t1)| t1 != u32::MAX && on_crease[a as usize] && on_crease[b as usize] && m.dihedral_cos(t0, t1).is_some_and(|c| c < half))
            .map(|e| e.0)
            .collect();
        m.compact();
    }
    // Surface nets leave gaps of a few cells in a curved sharp rim (where
    // its sharpness spreads over several edges): nearest ends first.
    for reach in [3.0, 6.0, 10.0] {
        m.bridge_gaps(reach * cell.max(length));
    }
    progress.check()?;
    let (lo, hi) = (length * 4.0 / 5.0, length * 4.0 / 3.0);
    for _ in 0..rounds {
        progress.check()?;
        m.split_long(hi, 12);
        progress.check()?;
        for _ in 0..4 {
            if m.collapse_short(lo, hi) == 0 {
                break;
            }
            progress.check()?;
        }
        m.compact();
        for _ in 0..3 {
            if m.flip() == 0 {
                break;
            }
        }
        progress.check()?;
        m.relax(surface, length, cell);
    }
    Ok(m)
}

/// An f32 ordered by `total_cmp` (for the path search's heap).
#[derive(Clone, Copy, PartialEq)]
struct OrdF32(f32);

impl Eq for OrdF32 {}

impl PartialOrd for OrdF32 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}
