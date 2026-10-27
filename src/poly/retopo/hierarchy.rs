//! The multigrid hierarchy the fields are solved on (Jakob et al. 2015,
//! §4): the input mesh's vertex graph, then coarser graphs made by
//! pairing each vertex with its best unmatched neighbour, down to about
//! `COARSEST` vertices.
//!
//! **Determinism.** Matching runs sequentially in index order, coarse
//! neighbour lists are summed in fine-vertex order and sorted, and each
//! level is coloured greedily in index order so no two neighbours share a
//! colour. `sweep` then updates one colour at a time (in colour order);
//! within a colour every update reads only other colours' values, so the
//! colour's updates run in parallel and the result is the same on any
//! number of threads (§0.2 rule 4).

use glam::Vec3;

use crate::clay::par_chunks;

/// Matching stops once a level has at most this many vertices.
pub const COARSEST: usize = 1000;
/// Most levels (a mesh of 2M vertices needs about 11).
const MAX_LEVELS: usize = 24;

/// One level of the hierarchy: a weighted vertex graph.
pub struct Level {
    pub v: Vec<Vec3>,
    pub n: Vec<Vec3>,
    pub area: Vec<f32>,
    /// Neighbours (CSR): `adj[adj_start[i]..adj_start[i + 1]]`, by index.
    pub adj_start: Vec<u32>,
    pub adj: Vec<(u32, f32)>,
    /// Vertices of each colour, in index order.
    pub colours: Vec<Vec<u32>>,
    /// Each vertex's vertex on the next coarser level (empty on the
    /// coarsest).
    pub parent: Vec<u32>,
}

impl Level {
    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn neighbours(&self, i: usize) -> &[(u32, f32)] {
        &self.adj[self.adj_start[i] as usize..self.adj_start[i + 1] as usize]
    }
}

/// Levels 0 (the input) to the coarsest.
pub struct Hierarchy {
    pub levels: Vec<Level>,
}

/// CSR from per-vertex lists.
fn csr(lists: Vec<Vec<(u32, f32)>>) -> (Vec<u32>, Vec<(u32, f32)>) {
    let mut start = Vec::with_capacity(lists.len() + 1);
    start.push(0u32);
    let mut adj = Vec::with_capacity(lists.iter().map(Vec::len).sum());
    for l in lists {
        adj.extend(l);
        start.push(adj.len() as u32);
    }
    (start, adj)
}

/// Greedy colouring in index order: each vertex takes the smallest colour
/// none of its neighbours before it took.
fn colour(n: usize, adj_start: &[u32], adj: &[(u32, f32)]) -> Vec<Vec<u32>> {
    let mut of = vec![u32::MAX; n];
    let mut used: Vec<u32> = Vec::new();
    let mut colours: Vec<Vec<u32>> = Vec::new();
    for i in 0..n {
        used.clear();
        for &(j, _) in &adj[adj_start[i] as usize..adj_start[i + 1] as usize] {
            if of[j as usize] != u32::MAX {
                used.push(of[j as usize]);
            }
        }
        used.sort_unstable();
        used.dedup();
        let mut c = 0;
        for &u in &used {
            if u == c {
                c += 1;
            } else if u > c {
                break;
            }
        }
        of[i] = c;
        if colours.len() <= c as usize {
            colours.push(Vec::new());
        }
        colours[c as usize].push(i as u32);
    }
    colours
}

impl Hierarchy {
    /// The hierarchy of a triangle mesh (`normals` per vertex, unit).
    /// Edges weigh 1 on the input level; coarse edges sum their fine ones.
    pub fn build(points: &[Vec3], tris: &[[u32; 3]], normals: &[Vec3], progress: &crate::progress::Progress) -> Result<Hierarchy, String> {
        let n = points.len();
        let mut area = vec![0.0f32; n];
        let mut lists: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
        for t in tris {
            let a = (points[t[1] as usize] - points[t[0] as usize]).cross(points[t[2] as usize] - points[t[0] as usize]).length() * 0.5;
            for k in 0..3 {
                area[t[k] as usize] += a / 3.0;
                lists[t[k] as usize].push((t[(k + 1) % 3], 1.0));
                lists[t[k] as usize].push((t[(k + 2) % 3], 1.0));
            }
        }
        for l in &mut lists {
            l.sort_unstable_by_key(|e| e.0);
            l.dedup_by_key(|e| e.0);
        }
        let (adj_start, adj) = csr(lists);
        let colours = colour(n, &adj_start, &adj);
        let mut levels = vec![Level { v: points.to_vec(), n: normals.to_vec(), area, adj_start, adj, colours, parent: Vec::new() }];
        while levels.len() < MAX_LEVELS && levels.last().expect("one").len() > COARSEST {
            progress.check()?;
            let fine = levels.last_mut().expect("one");
            let (coarse, parent) = coarsen(fine);
            if coarse.len() as f32 > 0.95 * fine.len() as f32 {
                break;
            }
            fine.parent = parent;
            levels.push(coarse);
        }
        Ok(Hierarchy { levels })
    }
}

/// The next coarser level: each vertex, in index order, pairs with its
/// unmatched neighbour of the most similar normal per summed area (ties to
/// the lower index); faces turned away (normals more than 90 degrees
/// apart, the two sides of a thin part) never pair.
fn coarsen(l: &Level) -> (Level, Vec<u32>) {
    let n = l.len();
    let mut parent = vec![u32::MAX; n];
    let mut members: Vec<[u32; 2]> = Vec::with_capacity(n / 2 + 1);
    for i in 0..n {
        if parent[i] != u32::MAX {
            continue;
        }
        let mut best: Option<(f32, u32)> = None;
        for &(j, _) in l.neighbours(i) {
            let j = j as usize;
            if j == i || parent[j] != u32::MAX {
                continue;
            }
            let d = l.n[i].dot(l.n[j]);
            if d <= 0.0 {
                continue;
            }
            let score = d / (l.area[i] + l.area[j]).max(1e-30);
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, j as u32));
            }
        }
        let c = members.len() as u32;
        parent[i] = c;
        match best {
            Some((_, j)) => {
                parent[j as usize] = c;
                members.push([i as u32, j]);
            }
            None => members.push([i as u32, u32::MAX]),
        }
    }
    let m = members.len();
    let mut v = Vec::with_capacity(m);
    let mut nn = Vec::with_capacity(m);
    let mut area = Vec::with_capacity(m);
    for pair in &members {
        let (mut sv, mut sn, mut sa) = (Vec3::ZERO, Vec3::ZERO, 0.0f32);
        for &i in pair.iter().filter(|&&i| i != u32::MAX) {
            let a = l.area[i as usize].max(1e-30);
            sv += l.v[i as usize] * a;
            sn += l.n[i as usize] * a;
            sa += a;
        }
        v.push(sv / sa);
        nn.push(sn.normalize_or(l.n[pair[0] as usize]));
        area.push(sa);
    }
    let mut lists: Vec<Vec<(u32, f32)>> = vec![Vec::new(); m];
    for i in 0..n {
        let pi = parent[i];
        for &(j, w) in l.neighbours(i) {
            let pj = parent[j as usize];
            if pj != pi {
                lists[pi as usize].push((pj, w));
            }
        }
    }
    for list in &mut lists {
        // Stable: equal neighbours keep their fine order, so the sums run
        // in the same order on every run.
        list.sort_by_key(|e| e.0);
        let mut merged: Vec<(u32, f32)> = Vec::with_capacity(list.len());
        for &(j, w) in list.iter() {
            match merged.last_mut() {
                Some(last) if last.0 == j => last.1 += w,
                _ => merged.push((j, w)),
            }
        }
        *list = merged;
    }
    let (adj_start, adj) = csr(lists);
    let colours = colour(m, &adj_start, &adj);
    (Level { v, n: nn, area, adj_start, adj, colours, parent: Vec::new() }, parent)
}

/// One multicolour Gauss-Seidel sweep: each colour in order, every vertex
/// of it updated in parallel from the values as they are (other colours'
/// values only, as no two neighbours share a colour), then written back.
pub fn sweep<T: Copy + Send + Sync>(level: &Level, values: &mut [T], update: impl Fn(usize, &[T]) -> T + Sync) {
    const CHUNK: usize = 512;
    for colour in &level.colours {
        let Some(&first) = colour.first() else { continue };
        let mut out = vec![values[first as usize]; colour.len()];
        let read: &[T] = values;
        par_chunks(&mut out, CHUNK, |c, slots| {
            for (k, slot) in slots.iter_mut().enumerate() {
                *slot = update(colour[c * CHUNK + k] as usize, read);
            }
        });
        for (k, &i) in colour.iter().enumerate() {
            values[i as usize] = out[k];
        }
    }
}

/// `f(i)` for every vertex `i < n`, in parallel, each into its own slot.
pub fn par_map<T: Copy + Send + Sync + Default>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    const CHUNK: usize = 512;
    let mut out = vec![T::default(); n];
    par_chunks(&mut out, CHUNK, |c, slots| {
        for (k, slot) in slots.iter_mut().enumerate() {
            *slot = f(c * CHUNK + k);
        }
    });
    out
}

/// `par_map`, in slices of `SLICE` with a cancel check between them (for
/// passes that measure the field at every point).
pub fn par_map_checked<T: Copy + Send + Sync + Default>(n: usize, progress: &crate::progress::Progress, f: impl Fn(usize) -> T + Sync) -> Result<Vec<T>, String> {
    const SLICE: usize = 1 << 15;
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    while start < n {
        progress.check()?;
        let len = SLICE.min(n - start);
        out.extend(par_map(len, |i| f(start + i)));
        start += len;
    }
    Ok(out)
}
