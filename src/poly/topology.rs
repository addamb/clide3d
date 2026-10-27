//! CSR adjacency of one subdivision level: edges, the faces around each
//! edge and the edges and faces around each vertex, all in a fixed order
//! (no hash maps), so every sum over a neighbourhood runs in the same
//! order on any thread count. Half-edges are not stored.
//!
//! Level 0's edges come from sorting the cage's corners by their edge
//! (lower vertex first). Finer levels are made from the coarser one
//! directly (`refine`): Catmull-Clark's split is regular, so the child
//! faces, edges and creases are known without searching.

use std::collections::BTreeMap;

/// One level's faces, edges and adjacency.
pub struct Topology {
    pub vertices: usize,
    /// Faces + 1 offsets into `corners`.
    pub face_start: Vec<u32>,
    pub corners: Vec<u32>,
    /// Each edge's two points.
    pub edges: Vec<[u32; 2]>,
    /// Per corner: the edge from it to the next corner of its face.
    pub corner_edge: Vec<u32>,
    /// Crease sharpness per edge (0 smooth, `f32::INFINITY` never decays).
    pub sharpness: Vec<f32>,
    /// The faces on each edge, in face order (CSR).
    pub edge_face_start: Vec<u32>,
    pub edge_faces: Vec<u32>,
    /// The edges at each vertex, in edge order (CSR).
    pub vert_edge_start: Vec<u32>,
    pub vert_edges: Vec<u32>,
    /// The faces at each vertex, in face order (CSR).
    pub vert_face_start: Vec<u32>,
    pub vert_faces: Vec<u32>,
}

/// A level's faces, edges and creases, without the adjacency (the finest
/// level shown needs only these).
#[derive(Clone)]
pub struct Core {
    pub vertices: usize,
    pub face_start: Vec<u32>,
    pub corners: Vec<u32>,
    pub edges: Vec<[u32; 2]>,
    pub corner_edge: Vec<u32>,
    pub sharpness: Vec<f32>,
}

/// Groups `items` (key, value) by key into CSR in item order: a counting
/// sort, stable, so each group keeps the order the items came in.
fn csr(keys: usize, items: impl Iterator<Item = (u32, u32)> + Clone) -> (Vec<u32>, Vec<u32>) {
    let mut start = vec![0u32; keys + 1];
    for (k, _) in items.clone() {
        start[k as usize + 1] += 1;
    }
    for i in 0..keys {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut out = vec![0u32; start[keys] as usize];
    for (k, v) in items {
        out[fill[k as usize] as usize] = v;
        fill[k as usize] += 1;
    }
    (start, out)
}

impl Topology {
    /// Level 0 from a cage (faces already checked by `Poly::new`).
    pub fn build(vertices: usize, face_start: Vec<u32>, corners: Vec<u32>) -> Topology {
        let mut keys: Vec<(u64, u32)> = Vec::with_capacity(corners.len());
        for f in 0..face_start.len() - 1 {
            let (s, e) = (face_start[f] as usize, face_start[f + 1] as usize);
            for c in s..e {
                let (a, b) = (corners[c], corners[if c + 1 == e { s } else { c + 1 }]);
                keys.push(((a.min(b) as u64) << 32 | a.max(b) as u64, c as u32));
            }
        }
        keys.sort_unstable();
        let mut edges = Vec::new();
        let mut corner_edge = vec![0u32; corners.len()];
        let mut last = None;
        for (k, c) in keys {
            if last != Some(k) {
                edges.push([(k >> 32) as u32, k as u32]);
                last = Some(k);
            }
            corner_edge[c as usize] = edges.len() as u32 - 1;
        }
        let sharpness = vec![0.0; edges.len()];
        Topology::from_core(Core { vertices, face_start, corners, edges, corner_edge, sharpness })
    }

    /// The adjacency from the faces and edges.
    pub fn from_core(core: Core) -> Topology {
        let Core { vertices, face_start, corners, edges, corner_edge, sharpness } = core;
        let faces = face_start.len() - 1;
        let mut corner_faces = Vec::with_capacity(corners.len());
        for f in 0..faces {
            corner_faces.extend(std::iter::repeat_n(f as u32, (face_start[f + 1] - face_start[f]) as usize));
        }
        let (edge_face_start, edge_faces) = csr(edges.len(), corner_edge.iter().zip(&corner_faces).map(|(&e, &f)| (e, f)));
        let (vert_edge_start, vert_edges) = csr(vertices, edges.iter().enumerate().flat_map(|(i, e)| [(e[0], i as u32), (e[1], i as u32)]));
        let (vert_face_start, vert_faces) = csr(vertices, corners.iter().zip(&corner_faces).map(|(&v, &f)| (v, f)));
        Topology { vertices, face_start, corners, edges, corner_edge, sharpness, edge_face_start, edge_faces, vert_edge_start, vert_edges, vert_face_start, vert_faces }
    }

    pub fn faces(&self) -> usize {
        self.face_start.len() - 1
    }

    pub fn face(&self, f: usize) -> &[u32] {
        &self.corners[self.face_start[f] as usize..self.face_start[f + 1] as usize]
    }

    pub fn faces_of_edge(&self, e: usize) -> &[u32] {
        &self.edge_faces[self.edge_face_start[e] as usize..self.edge_face_start[e + 1] as usize]
    }

    /// The edges at point `v` (none past the points).
    pub fn edges_of(&self, v: usize) -> &[u32] {
        match (self.vert_edge_start.get(v), self.vert_edge_start.get(v + 1)) {
            (Some(&a), Some(&b)) => &self.vert_edges[a as usize..b as usize],
            _ => &[],
        }
    }

    /// The faces at point `v` (none past the points).
    pub fn faces_of(&self, v: usize) -> &[u32] {
        match (self.vert_face_start.get(v), self.vert_face_start.get(v + 1)) {
            (Some(&a), Some(&b)) => &self.vert_faces[a as usize..b as usize],
            _ => &[],
        }
    }

    /// The other point of edge `e` from `v`.
    pub fn other(&self, e: usize, v: u32) -> u32 {
        let [a, b] = self.edges[e];
        if a == v { b } else { a }
    }

    /// Whether edge `e` is on a boundary or shared by more than two faces:
    /// subdivision keeps it sharp (D16's "edges and corners").
    pub fn is_border(&self, e: usize) -> bool {
        self.edge_face_start[e + 1] - self.edge_face_start[e] != 2
    }

    /// Sharpness subdivision uses: borders are infinitely sharp.
    pub fn sharpness_of(&self, e: usize) -> f32 {
        if self.is_border(e) { f32::INFINITY } else { self.sharpness[e] }
    }

    /// Sets the cage's crease sharpness (`Poly::creases`): 10 and above is
    /// infinite. Pairs that are not edges are left out.
    pub fn set_creases(&mut self, creases: &BTreeMap<(u32, u32), f32>) {
        if creases.is_empty() {
            return;
        }
        for (i, e) in self.edges.iter().enumerate() {
            if let Some(&s) = creases.get(&(e[0].min(e[1]), e[0].max(e[1]))) {
                self.sharpness[i] = if s >= super::INFINITE_SHARPNESS { f32::INFINITY } else { s.max(0.0) };
            }
        }
    }

    /// The edge joining `a` and `b`, if any (None for points out of range).
    pub fn edge_between(&self, a: u32, b: u32) -> Option<usize> {
        if a as usize >= self.vertices || b as usize >= self.vertices {
            return None;
        }
        self.edges_of(a as usize).iter().map(|&e| e as usize).find(|&e| self.other(e, a) == b)
    }

    /// File format: saved displacements (Stage 3c) are per point in this
    /// order, so it must not change.
    ///
    /// The next level's faces, edges and creases (`subdiv` puts its
    /// points in the same order): this level's points, then one per face,
    /// then one per edge. Face `c` of the child is the quad at corner `c`
    /// (its point, the point on the corner's next edge, the face's point,
    /// the point on its previous edge). Edge `2e` is the half of edge `e`
    /// at its first point, `2e + 1` the other half, and `2E + c` joins the
    /// point on corner `c`'s next edge to its face's point. Halves keep
    /// sharpness less one (DeRose et al. 1998; infinite stays infinite);
    /// inner edges are smooth.
    pub fn refine(&self) -> Topology {
        Topology::from_core(self.refine_core())
    }

    /// This level's faces, edges and creases.
    pub fn core(&self) -> Core {
        Core { vertices: self.vertices, face_start: self.face_start.clone(), corners: self.corners.clone(), edges: self.edges.clone(), corner_edge: self.corner_edge.clone(), sharpness: self.sharpness.clone() }
    }

    /// `refine` without the child's adjacency.
    pub fn refine_core(&self) -> Core {
        let (v, f, e) = (self.vertices as u32, self.faces() as u32, self.edges.len() as u32);
        let n_corners = self.corners.len();
        let face_point = |face: u32| v + face;
        let edge_point = |edge: u32| v + f + edge;
        let half = |edge: u32, at: u32| 2 * edge + (self.edges[edge as usize][0] != at) as u32;
        let mut corners = Vec::with_capacity(n_corners * 4);
        let mut corner_edge = Vec::with_capacity(n_corners * 4);
        for face in 0..self.faces() {
            let (s, end) = (self.face_start[face] as usize, self.face_start[face + 1] as usize);
            for c in s..end {
                let prev = if c == s { end - 1 } else { c - 1 };
                let (point, next_edge, prev_edge) = (self.corners[c], self.corner_edge[c], self.corner_edge[prev]);
                corners.extend_from_slice(&[point, edge_point(next_edge), face_point(face as u32), edge_point(prev_edge)]);
                corner_edge.extend_from_slice(&[half(next_edge, point), 2 * e + c as u32, 2 * e + prev as u32, half(prev_edge, point)]);
            }
        }
        let mut edges = Vec::with_capacity(2 * e as usize + n_corners);
        let mut sharpness = Vec::with_capacity(edges.capacity());
        for (i, &[a, b]) in self.edges.iter().enumerate() {
            let p = edge_point(i as u32);
            edges.push([a, p]);
            edges.push([p, b]);
            let s = (self.sharpness[i] - 1.0).max(0.0);
            sharpness.push(s);
            sharpness.push(s);
        }
        for face in 0..self.faces() {
            for c in self.face_start[face] as usize..self.face_start[face + 1] as usize {
                edges.push([edge_point(self.corner_edge[c]), face_point(face as u32)]);
                sharpness.push(0.0);
            }
        }
        let face_start = (0..=n_corners as u32).map(|i| 4 * i).collect();
        Core { vertices: (v + f + e) as usize, face_start, corners, edges, corner_edge, sharpness }
    }

    /// Boundary loops: connected chains of edges in one face.
    pub fn boundary_loops(&self) -> usize {
        let open: Vec<usize> = (0..self.edges.len()).filter(|&e| self.edge_face_start[e + 1] - self.edge_face_start[e] == 1).collect();
        if open.is_empty() {
            return 0;
        }
        // Union-find over the points, with path halving.
        let mut root: Vec<u32> = (0..self.vertices as u32).collect();
        fn find(r: &mut [u32], mut x: u32) -> u32 {
            while r[x as usize] != x {
                r[x as usize] = r[r[x as usize] as usize];
                x = r[x as usize];
            }
            x
        }
        for &e in &open {
            let [a, b] = self.edges[e];
            let (x, y) = (find(&mut root, a), find(&mut root, b));
            if x != y {
                root[x.max(y) as usize] = x.min(y);
            }
        }
        let mut loops: Vec<u32> = open.iter().map(|&e| find(&mut root, self.edges[e][0])).collect();
        loops.sort_unstable();
        loops.dedup();
        loops.len()
    }

    /// Connected parts (by shared edges), counting faces only.
    pub fn components(&self) -> usize {
        let mut part = vec![u32::MAX; self.faces()];
        let mut count = 0;
        let mut stack = Vec::new();
        for start in 0..self.faces() {
            if part[start] != u32::MAX {
                continue;
            }
            part[start] = count;
            stack.push(start);
            while let Some(face) = stack.pop() {
                for c in self.face_start[face] as usize..self.face_start[face + 1] as usize {
                    for &g in self.faces_of_edge(self.corner_edge[c] as usize) {
                        if part[g as usize] == u32::MAX {
                            part[g as usize] = count;
                            stack.push(g as usize);
                        }
                    }
                }
            }
            count += 1;
        }
        count as usize
    }

    /// The fans of faces around point `v`: for each of `faces_of(v)`, the
    /// first of its faces reached from it across `v`'s two-face edges (a
    /// union-find over the faces around `v`). A point whose faces are not
    /// one fan is non-manifold: parts meet there only at the point.
    pub fn fans(&self, v: usize) -> Vec<usize> {
        let around = self.faces_of(v);
        let mut root: Vec<usize> = (0..around.len()).collect();
        fn find(r: &mut [usize], mut i: usize) -> usize {
            while r[i] != i {
                r[i] = r[r[i]];
                i = r[i];
            }
            i
        }
        let slot = |f: u32| around.iter().position(|&g| g == f);
        for &e in self.edges_of(v) {
            if let [a, b] = *self.faces_of_edge(e as usize)
                && let (Some(x), Some(y)) = (slot(a), slot(b))
            {
                let (x, y) = (find(&mut root, x), find(&mut root, y));
                root[x.max(y)] = x.min(y);
            }
        }
        (0..around.len()).map(|i| find(&mut root, i)).collect()
    }

    /// How many fans each point's faces make (`fans`; 0 for an unused
    /// point).
    pub fn fan_counts(&self) -> Vec<usize> {
        (0..self.vertices)
            .map(|v| {
                let mut f = self.fans(v);
                f.sort_unstable();
                f.dedup();
                f.len()
            })
            .collect()
    }

    /// Edges in more than two faces, and edges two faces use the same way
    /// round (inconsistent winding).
    pub fn non_manifold(&self) -> (usize, usize) {
        let mut over = 0;
        let mut flipped = 0;
        for e in 0..self.edges.len() {
            let fs = self.faces_of_edge(e);
            if fs.len() > 2 {
                over += 1;
            } else if fs.len() == 2 {
                let [a, b] = self.edges[e];
                let forward = |face: u32| {
                    let f = self.face(face as usize);
                    (0..f.len()).any(|i| f[i] == a && f[(i + 1) % f.len()] == b)
                };
                flipped += (forward(fs[0]) == forward(fs[1])) as usize;
            }
        }
        (over, flipped)
    }

}
