//! Quads from the two fields (Jakob et al. 2015 §5), made deterministic.
//!
//! Every input edge compares its two lattices (`position::compat`, with
//! the far vertex's orientation turned to match): vertices on the same
//! lattice point collapse into one output vertex (union-find over the
//! edges in index order), and vertices one lattice step apart on one axis
//! give an output edge. The output graph is then cleaned (vertices with
//! fewer than three edges dissolve) and its faces traced: around each
//! vertex the edges are sorted by angle about its normal, and each face is
//! the walk that always takes the next edge clockwise after the one it came
//! in on. Every directed edge is in exactly one walk, so the faces always
//! make a closed surface, every edge in two faces wound opposite ways;
//! walks that pass a vertex twice (a dangling or crossing edge) lose that
//! edge and are traced again.

use glam::Vec3;

use super::field::compat_orientation;
use super::hierarchy::Level;
use super::position::{Side, compat};

/// The output graph: one vertex per lattice point the input reaches.
pub struct Graph {
    pub p: Vec<Vec3>,
    pub n: Vec<Vec3>,
    /// The lattice spacing there.
    pub h: Vec<f32>,
    /// Edges, lower index first, sorted.
    pub edges: Vec<[u32; 2]>,
}

fn find(root: &mut [u32], mut x: u32) -> u32 {
    while root[x as usize] != x {
        root[x as usize] = root[root[x as usize] as usize];
        x = root[x as usize];
    }
    x
}

/// The output graph of the input level's fields.
pub fn graph(level: &Level, q: &[Vec3], o: &[Vec3], scale: &[f32]) -> Graph {
    let n = level.len();
    let mut root: Vec<u32> = (0..n as u32).collect();
    let mut links: Vec<[u32; 2]> = Vec::new();
    for i in 0..n {
        for &(j, _) in level.neighbours(i) {
            let j = j as usize;
            if j <= i {
                continue;
            }
            let (qi, qj) = compat_orientation(q[i], level.n[i], q[j], level.n[j]);
            let h = 0.5 * (scale[i] + scale[j]);
            let (_, _, a, b) = compat(Side { v: level.v[i], n: level.n[i], q: qi, o: o[i] }, Side { v: level.v[j], n: level.n[j], q: qj, o: o[j] }, h);
            let d = [(a[0] - b[0]).abs(), (a[1] - b[1]).abs()];
            match d[0] + d[1] {
                0 => {
                    let (x, y) = (find(&mut root, i as u32), find(&mut root, j as u32));
                    if x != y {
                        root[x.max(y) as usize] = x.min(y);
                    }
                }
                1 => links.push([i as u32, j as u32]),
                _ => {}
            }
        }
    }
    let mut slot = vec![u32::MAX; n];
    let mut count = 0u32;
    for i in 0..n as u32 {
        if find(&mut root, i) == i {
            slot[i as usize] = count;
            count += 1;
        }
    }
    let of: Vec<u32> = (0..n as u32).map(|i| slot[find(&mut root, i) as usize]).collect();
    let m = count as usize;
    let (mut sp, mut sn, mut sh, mut sa) = (vec![Vec3::ZERO; m], vec![Vec3::ZERO; m], vec![0.0f32; m], vec![0.0f32; m]);
    for i in 0..n {
        let c = of[i] as usize;
        let a = level.area[i].max(1e-30);
        sp[c] += o[i] * a;
        sn[c] += level.n[i] * a;
        sh[c] += scale[i] * a;
        sa[c] += a;
    }
    let p = (0..m).map(|c| sp[c] / sa[c]).collect();
    let normals = (0..m).map(|c| sn[c].normalize_or(Vec3::Z)).collect();
    let h = (0..m).map(|c| sh[c] / sa[c]).collect();
    let mut edges: Vec<[u32; 2]> = links
        .iter()
        .filter_map(|&[i, j]| {
            let (a, b) = (of[i as usize], of[j as usize]);
            (a != b).then(|| [a.min(b), a.max(b)])
        })
        .collect();
    edges.sort_unstable();
    edges.dedup();
    Graph { p, n: normals, h, edges }
}

/// Neighbour lists of a graph's vertices (sorted by index).
pub fn adjacency(vertices: usize, edges: &[[u32; 2]]) -> Vec<Vec<u32>> {
    let mut adj = vec![Vec::new(); vertices];
    for &[a, b] in edges {
        adj[a as usize].push(b);
        adj[b as usize].push(a);
    }
    for l in &mut adj {
        l.sort_unstable();
        l.dedup();
    }
    adj
}

/// Removes `b` from `a`'s list and `a` from `b`'s.
fn unlink(adj: &mut [Vec<u32>], a: u32, b: u32) {
    adj[a as usize].retain(|&x| x != b);
    adj[b as usize].retain(|&x| x != a);
}

fn link(adj: &mut [Vec<u32>], a: u32, b: u32) {
    for (x, y) in [(a, b), (b, a)] {
        let l = &mut adj[x as usize];
        if let Err(k) = l.binary_search(&y) {
            l.insert(k, y);
        }
    }
}

/// Dissolves vertices with fewer than three edges, in index order, until
/// none is left: one edge or none, the vertex goes; two, its neighbours
/// are joined in its place. Returns how many went.
pub fn dissolve(adj: &mut [Vec<u32>], alive: &mut [bool]) -> usize {
    let mut gone = 0;
    let mut stack: Vec<u32> = (0..adj.len() as u32).rev().filter(|&v| alive[v as usize] && adj[v as usize].len() < 3).collect();
    while let Some(v) = stack.pop() {
        let vi = v as usize;
        if !alive[vi] || adj[vi].len() >= 3 {
            continue;
        }
        let around = std::mem::take(&mut adj[vi]);
        for &u in &around {
            adj[u as usize].retain(|&x| x != v);
        }
        if let [a, b] = around[..] {
            link(adj, a, b);
        }
        alive[vi] = false;
        gone += 1;
        let mut next: Vec<u32> = around.iter().copied().filter(|&u| adj[u as usize].len() < 3).collect();
        next.sort_unstable_by(|a, b| b.cmp(a));
        stack.extend(next);
    }
    gone
}

/// Each vertex's neighbours sorted counter-clockwise about its normal
/// (ties by index).
fn rotations(p: &[Vec3], n: &[Vec3], adj: &[Vec<u32>]) -> Vec<Vec<u32>> {
    adj.iter()
        .enumerate()
        .map(|(v, l)| {
            let nv = n[v];
            let u = nv.any_orthonormal_vector();
            let w = nv.cross(u);
            let mut keyed: Vec<(f32, u32)> = l
                .iter()
                .map(|&x| {
                    let d = p[x as usize] - p[v];
                    (d.dot(w).atan2(d.dot(u)), x)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            keyed.into_iter().map(|k| k.1).collect()
        })
        .collect()
}

/// The faces of a rotation system: every directed edge's walk, taking at
/// each vertex the edge just clockwise of the one it came in on.
fn trace(rot: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut start = Vec::with_capacity(rot.len() + 1);
    start.push(0usize);
    for l in rot {
        start.push(start.last().expect("one") + l.len());
    }
    let mut used = vec![false; *start.last().expect("one")];
    let slot = |v: u32, u: u32| rot[v as usize].iter().position(|&x| x == u).expect("edges are symmetric");
    let mut faces = Vec::new();
    for v in 0..rot.len() {
        for k in 0..rot[v].len() {
            if used[start[v] + k] {
                continue;
            }
            let mut face = Vec::new();
            let (mut a, mut ka) = (v as u32, k);
            while !used[start[a as usize] + ka] {
                used[start[a as usize] + ka] = true;
                face.push(a);
                let b = rot[a as usize][ka];
                let deg = rot[b as usize].len();
                let kb = (slot(b, a) + deg - 1) % deg;
                (a, ka) = (b, kb);
            }
            faces.push(face);
        }
    }
    faces
}

/// The surface the graph traces (see the module doc), after dissolving
/// vertices of fewer than three edges and removing, until every face is a
/// simple polygon, one edge of each face that passes a vertex twice (an
/// edge it runs both ways, else its longest edge at the vertex). Returns
/// the faces (indices into `g.p`, unused vertices left in) and how many
/// edges were removed.
pub fn faces(g: &Graph, progress: &crate::progress::Progress) -> Result<(Vec<Vec<u32>>, usize), String> {
    let mut adj = adjacency(g.p.len(), &g.edges);
    let mut alive = vec![true; g.p.len()];
    let mut removed = 0;
    for _ in 0..64 {
        progress.check()?;
        dissolve(&mut adj, &mut alive);
        let rot = rotations(&g.p, &g.n, &adj);
        let faces = trace(&rot);
        let mut cut: Vec<[u32; 2]> = Vec::new();
        for f in &faces {
            if let Some(e) = bad_edge(f, &g.p) {
                cut.push(e);
            }
        }
        if cut.is_empty() {
            return Ok((faces, removed));
        }
        cut.sort_unstable();
        cut.dedup();
        for [a, b] in cut {
            unlink(&mut adj, a, b);
            removed += 1;
        }
    }
    Err("quad extraction did not settle (faces kept passing a vertex twice)".into())
}

/// For a face that passes a vertex twice: an edge it runs both ways, else
/// its longest edge at the first repeated vertex (None for a simple face).
fn bad_edge(f: &[u32], p: &[Vec3]) -> Option<[u32; 2]> {
    let n = f.len();
    let mut sorted = f.to_vec();
    sorted.sort_unstable();
    let repeated = sorted.windows(2).find(|w| w[0] == w[1]).map(|w| w[0])?;
    let mut sides: Vec<[u32; 2]> = (0..n).map(|k| [f[k], f[(k + 1) % n]]).collect();
    for k in 0..n {
        let [a, b] = sides[k];
        if sides.contains(&[b, a]) {
            return Some([a.min(b), a.max(b)]);
        }
    }
    sides.retain(|s| s.contains(&repeated));
    sides.into_iter().max_by(|x, y| p[x[0] as usize].distance(p[x[1] as usize]).total_cmp(&p[y[0] as usize].distance(p[y[1] as usize])).then(y.cmp(x))).map(|[a, b]| [a.min(b), a.max(b)])
}
