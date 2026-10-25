//! Ray picking on meshes through a bounding volume hierarchy (docs/
//! SCULPT_PLAN.md Stage 6): the fallback to `pick_field` for meshes that
//! are not a clay in a sculpt session (other objects, clay outside one),
//! and the screen-frame resolver's caster.
//!
//! The triangle test and the choice between equal hits are exactly
//! `Mesh::ray_hit_triangle`'s (the nearest t, then the lowest triangle),
//! so a hit is bit for bit the brute-force one, only found in a few dozen
//! triangle tests instead of all of them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::Vec3;

use super::Mesh;

const LEAF: usize = 4;

#[derive(Clone, Copy)]
struct Node {
    lo: Vec3,
    hi: Vec3,
    /// Leaf: first triangle in `order`; inner: the left child.
    start: u32,
    /// Leaf: triangle count (> 0); inner: 0.
    count: u32,
    right: u32,
}

pub struct MeshBvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

/// A ray hit: distance, triangle, barycentric weights of its corners 1
/// and 2, and the normal there (the vertex normals blended, facing the
/// ray's origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub t: f32,
    pub tri: usize,
    pub u: f32,
    pub v: f32,
    pub normal: Vec3,
}

impl MeshBvh {
    pub fn new(mesh: &Mesh) -> MeshBvh {
        let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
        let boxes: Vec<(Vec3, Vec3)> = mesh.indices.chunks_exact(3).map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a.min(b).min(c), a.max(b).max(c))
        }).collect();
        let mut order: Vec<u32> = (0..boxes.len() as u32).collect();
        let mut nodes = Vec::with_capacity(boxes.len() / 2 + 1);
        if !boxes.is_empty() {
            build(&mut nodes, &mut order, &boxes, 0, boxes.len());
        }
        MeshBvh { nodes, order }
    }

    /// The nearest hit of a ray (as `Mesh::ray_hit_triangle`), with its
    /// barycentrics and normal.
    pub fn hit(&self, mesh: &Mesh, origin: Vec3, dir: Vec3) -> Option<RayHit> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv = dir.recip();
        let mut best: Option<(f32, usize, f32, f32)> = None;
        let mut stack = vec![0u32];
        while let Some(at) = stack.pop() {
            let n = &self.nodes[at as usize];
            if slab(origin, inv, n.lo, n.hi, best.map_or(f32::INFINITY, |b| b.0)).is_none() {
                continue;
            }
            if n.count > 0 {
                for &i in &self.order[n.start as usize..(n.start + n.count) as usize] {
                    if let Some((t, u, v)) = triangle(mesh, i as usize, origin, dir)
                        && best.is_none_or(|b| t < b.0 || (t == b.0 && (i as usize) < b.1))
                    {
                        best = Some((t, i as usize, u, v));
                    }
                }
            } else {
                stack.push(n.right);
                stack.push(n.start);
            }
        }
        let (t, tri, u, v) = best?;
        let ix = &mesh.indices[tri * 3..tri * 3 + 3];
        let vn = |k: usize| Vec3::from(mesh.vertices[ix[k] as usize].normal);
        let mut normal = (vn(0) * (1.0 - u - v) + vn(1) * u + vn(2) * v).normalize_or_zero();
        if normal == Vec3::ZERO {
            let p = |k: usize| Vec3::from(mesh.vertices[ix[k] as usize].pos);
            normal = (p(1) - p(0)).cross(p(2) - p(0)).normalize_or_zero();
        }
        if normal.dot(dir) > 0.0 {
            normal = -normal;
        }
        Some(RayHit { t, tri, u, v, normal })
    }
}

/// `Mesh::ray_hit_triangle`'s test for one triangle: (t, u, v).
fn triangle(mesh: &Mesh, n: usize, origin: Vec3, dir: Vec3) -> Option<(f32, f32, f32)> {
    let tri = &mesh.indices[n * 3..n * 3 + 3];
    let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
    let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
    let e1 = b - a;
    let e2 = c - a;
    let pv = dir.cross(e2);
    let det = e1.dot(pv);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - a;
    let u = s.dot(pv) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 1e-5).then_some((t, u, v))
}

/// Where the ray enters the box, if before `t_max` (boxes are padded a
/// little, so a hit on a box's face is never cut).
fn slab(origin: Vec3, inv: Vec3, lo: Vec3, hi: Vec3, t_max: f32) -> Option<f32> {
    let pad = (hi - lo).max_element() * 1e-4 + 1e-6;
    let t0 = (lo - pad - origin) * inv;
    let t1 = (hi + pad - origin) * inv;
    let near = t0.min(t1).max_element();
    let far = t0.max(t1).min_element();
    (near <= far && far >= 0.0 && near <= t_max).then_some(near)
}

fn build(nodes: &mut Vec<Node>, order: &mut [u32], boxes: &[(Vec3, Vec3)], start: usize, end: usize) -> u32 {
    let (lo, hi) = order[start..end].iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &i| (a.min(boxes[i as usize].0), b.max(boxes[i as usize].1)));
    let at = nodes.len() as u32;
    nodes.push(Node { lo, hi, start: start as u32, count: (end - start) as u32, right: 0 });
    if end - start <= LEAF {
        return at;
    }
    // Median split along the longest side of the centers' box.
    let center = |i: u32| (boxes[i as usize].0 + boxes[i as usize].1) * 0.5;
    let (clo, chi) = order[start..end].iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &i| (a.min(center(i)), b.max(center(i))));
    let axis = (chi - clo).max_position();
    let mid = (start + end) / 2;
    order[start..end].select_nth_unstable_by(mid - start, |&a, &b| center(a)[axis].total_cmp(&center(b)[axis]).then(a.cmp(&b)));
    let left = build(nodes, order, boxes, start, mid);
    let right = build(nodes, order, boxes, mid, end);
    nodes[at as usize] = Node { lo, hi, start: left, count: 0, right };
    at
}

/// Trees by mesh address, with the mesh (kept while someone else holds it).
type Trees = HashMap<usize, (Arc<Mesh>, Arc<MeshBvh>)>;

/// Trees by mesh (kept while their mesh is), shared by copies of the
/// library, and the meshes whose trees are being built (`warm`).
#[derive(Clone, Default)]
pub struct BvhCache(Arc<Mutex<Trees>>, Arc<Mutex<std::collections::HashSet<usize>>>);

impl BvhCache {
    /// Builds `mesh`'s tree on a thread of its own if nobody has yet (a
    /// brush tool on a clay, before a stroke needs it).
    pub fn warm(&self, mesh: &Arc<Mesh>) {
        let key = Arc::as_ptr(mesh) as usize;
        if self.0.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&key) || !self.1.lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
            return;
        }
        let (cache, mesh) = (self.clone(), mesh.clone());
        let _ = std::thread::Builder::new().name("picking tree".into()).spawn(move || {
            cache.of(&mesh);
            cache.1.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        });
    }

    /// The tree of `mesh`, built the first time it is asked for.
    pub fn of(&self, mesh: &Arc<Mesh>) -> Arc<MeshBvh> {
        let key = Arc::as_ptr(mesh) as usize;
        if let Some((_, b)) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return b.clone();
        }
        let bvh = Arc::new(MeshBvh::new(mesh));
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Trees of meshes nobody else holds go.
        all.retain(|_, (m, _)| Arc::strong_count(m) > 1);
        all.insert(key, (mesh.clone(), bvh.clone()));
        bvh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_finds_the_brute_force_hit() {
        let clay = crate::harness::figure(128, true);
        let mesh = clay.build_mesh_with(&[], "");
        let bvh = MeshBvh::new(&mesh);
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let (mut hits, mut worst) = (0, 0.0f32);
        for _ in 0..10_000 {
            let origin = Vec3::new(next() * 2.0 - 1.0, next() * 2.0, 2.0);
            let target = Vec3::new(next() * 0.8 - 0.4, next() * 1.8, next() * 0.4 - 0.2);
            let dir = (target - origin).normalize();
            let brute = mesh.ray_hit_triangle(origin, dir);
            let tree = bvh.hit(&mesh, origin, dir);
            assert_eq!(brute.map(|b| b.1), tree.map(|h| h.tri));
            if let (Some(b), Some(h)) = (brute, tree) {
                hits += 1;
                worst = worst.max((b.0 - h.t).abs());
            }
        }
        assert!(hits > 3000 && worst <= 1e-5, "{hits} hits, worst {worst}");
    }
}
