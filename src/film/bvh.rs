//! Bounding volume hierarchy: a tree of boxes over the triangles, so a ray
//! tests a few dozen triangles instead of all of them. Built with binned
//! SAH (surface area heuristic: split where the two halves' boxes are
//! cheapest to test), stored flat for fast traversal.

use glam::Vec3;

#[derive(Clone, Copy)]
struct Node {
    lo: Vec3,
    hi: Vec3,
    /// Leaf: first triangle in `order`; inner: index of the left child (the
    /// right child follows the left subtree... stored explicitly in `right`).
    start: u32,
    /// Leaf: triangle count (> 0); inner: 0.
    count: u32,
    right: u32,
}

pub struct Bvh {
    nodes: Vec<Node>,
    /// Triangle indices in leaf order.
    order: Vec<u32>,
    /// Corner positions per triangle (a, b - a, c - a), for the hit test.
    tris: Vec<[Vec3; 3]>,
}

/// A ray hit: distance, triangle, and barycentric weights of corners 1 and 2.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f32,
    pub tri: u32,
    pub u: f32,
    pub v: f32,
}

const BINS: usize = 12;
const LEAF: usize = 4;

impl Bvh {
    pub fn new(corners: &[[Vec3; 3]]) -> Bvh {
        let tris: Vec<[Vec3; 3]> = corners.iter().map(|&[a, b, c]| [a, b - a, c - a]).collect();
        let boxes: Vec<(Vec3, Vec3)> = corners.iter().map(|&[a, b, c]| (a.min(b).min(c), a.max(b).max(c))).collect();
        let centers: Vec<Vec3> = boxes.iter().map(|(lo, hi)| (*lo + *hi) * 0.5).collect();
        let mut order: Vec<u32> = (0..corners.len() as u32).collect();
        let mut nodes = Vec::with_capacity(corners.len() * 2 / LEAF + 1);
        if !corners.is_empty() {
            build(&mut nodes, &mut order, &boxes, &centers, 0, corners.len());
        }
        Bvh { nodes, order, tris }
    }

    /// The tree for the GPU (`film/gpu.rs`): nodes as (lo, start or right
    /// child, hi, count), with the left child always next; triangles in
    /// leaf order as (a, e1, e2) with the original index in a's w.
    pub(super) fn gpu(&self) -> (Vec<[u32; 8]>, Vec<[f32; 12]>) {
        let nodes = self.nodes.iter().enumerate().map(|(i, n)| {
            debug_assert!(n.count > 0 || n.start == i as u32 + 1, "left child follows its parent");
            let a = if n.count > 0 { n.start } else { n.right };
            [n.lo.x.to_bits(), n.lo.y.to_bits(), n.lo.z.to_bits(), a, n.hi.x.to_bits(), n.hi.y.to_bits(), n.hi.z.to_bits(), n.count]
        }).collect();
        let tris = self.order.iter().map(|&i| {
            let [a, e1, e2] = self.tris[i as usize];
            [a.x, a.y, a.z, f32::from_bits(i), e1.x, e1.y, e1.z, 0.0, e2.x, e2.y, e2.z, 0.0]
        }).collect();
        (nodes, tris)
    }

    pub(super) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Levels from the root to the deepest leaf (0 when empty).
    pub(super) fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = if self.nodes.is_empty() { Vec::new() } else { vec![(0u32, 1usize)] };
        while let Some((at, level)) = stack.pop() {
            let n = &self.nodes[at as usize];
            deepest = deepest.max(level);
            if n.count == 0 {
                stack.push((n.start, level + 1));
                stack.push((n.right, level + 1));
            }
        }
        deepest
    }

    /// Nearest hit with t in (t_min, t_max). `skip` decides whether a hit
    /// counts (alpha cut-outs); it is called only for candidate hits.
    pub fn hit(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32, skip: &dyn Fn(&Hit) -> bool) -> Option<Hit> {
        self.walk(origin, dir, t_min, t_max, skip, false)
    }

    /// Whether anything is hit with t in (t_min, t_max): stops at the
    /// first hit found (shadow rays need no nearest one).
    pub fn any_hit(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32, skip: &dyn Fn(&Hit) -> bool) -> bool {
        self.walk(origin, dir, t_min, t_max, skip, true).is_some()
    }

    fn walk(&self, origin: Vec3, dir: Vec3, t_min: f32, mut t_max: f32, skip: &dyn Fn(&Hit) -> bool, any: bool) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv = dir.recip();
        let mut best = None;
        let mut stack = [0u32; 64];
        let mut sp = 0;
        let mut at = 0u32;
        loop {
            let node = &self.nodes[at as usize];
            if node.count > 0 {
                for &i in &self.order[node.start as usize..(node.start + node.count) as usize] {
                    if let Some(h) = self.triangle(i, origin, dir, t_min, t_max)
                        && !skip(&h)
                    {
                        if any {
                            return Some(h);
                        }
                        t_max = h.t;
                        best = Some(h);
                    }
                }
            } else {
                // Visit the nearer child first, so far boxes get cut by t_max.
                let (l, r) = (node.start, node.right);
                let tl = slab(origin, inv, self.nodes[l as usize].lo, self.nodes[l as usize].hi, t_max);
                let tr = slab(origin, inv, self.nodes[r as usize].lo, self.nodes[r as usize].hi, t_max);
                match (tl, tr) {
                    (Some(a), Some(b)) => {
                        let (near, far) = if a <= b { (l, r) } else { (r, l) };
                        if sp < stack.len() {
                            stack[sp] = far;
                            sp += 1;
                        }
                        at = near;
                        continue;
                    }
                    (Some(_), None) => {
                        at = l;
                        continue;
                    }
                    (None, Some(_)) => {
                        at = r;
                        continue;
                    }
                    (None, None) => {}
                }
            }
            // Next box from the stack that the ray still reaches.
            loop {
                if sp == 0 {
                    return best;
                }
                sp -= 1;
                let n = &self.nodes[stack[sp] as usize];
                if slab(origin, inv, n.lo, n.hi, t_max).is_some() {
                    at = stack[sp];
                    break;
                }
            }
        }
    }

    /// Möller–Trumbore, both sides.
    fn triangle(&self, i: u32, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> Option<Hit> {
        let [a, e1, e2] = self.tris[i as usize];
        let p = dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let s = origin - a;
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = dir.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inv;
        (t > t_min && t < t_max).then_some(Hit { t, tri: i, u, v })
    }
}

/// Where the ray enters the box, if it does before `t_max`.
fn slab(origin: Vec3, inv: Vec3, lo: Vec3, hi: Vec3, t_max: f32) -> Option<f32> {
    let t0 = (lo - origin) * inv;
    let t1 = (hi - origin) * inv;
    let near = t0.min(t1).max_element();
    let far = t0.max(t1).min_element();
    (near <= far && far >= 0.0 && near <= t_max).then_some(near)
}

fn area(lo: Vec3, hi: Vec3) -> f32 {
    let d = (hi - lo).max(Vec3::ZERO);
    d.x * d.y + d.y * d.z + d.z * d.x
}

/// Builds the subtree over `order[start..end]`; returns its node index.
fn build(nodes: &mut Vec<Node>, order: &mut [u32], boxes: &[(Vec3, Vec3)], centers: &[Vec3], start: usize, end: usize) -> u32 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut clo, mut chi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for &i in &order[start..end] {
        let (a, b) = boxes[i as usize];
        lo = lo.min(a);
        hi = hi.max(b);
        clo = clo.min(centers[i as usize]);
        chi = chi.max(centers[i as usize]);
    }
    let me = nodes.len() as u32;
    nodes.push(Node { lo, hi, start: start as u32, count: (end - start) as u32, right: 0 });
    let n = end - start;
    if n <= LEAF {
        return me;
    }
    // Binned SAH over the widest center axis.
    let extent = chi - clo;
    let axis = if extent.x >= extent.y && extent.x >= extent.z { 0 } else if extent.y >= extent.z { 1 } else { 2 };
    if extent[axis] <= 1e-9 {
        return me;
    }
    let scale = BINS as f32 / extent[axis];
    let bin_of = |i: u32| (((centers[i as usize][axis] - clo[axis]) * scale) as usize).min(BINS - 1);
    let mut bins = [(Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), 0usize); BINS];
    for &i in &order[start..end] {
        let b = &mut bins[bin_of(i)];
        b.0 = b.0.min(boxes[i as usize].0);
        b.1 = b.1.max(boxes[i as usize].1);
        b.2 += 1;
    }
    let mut best = (f32::MAX, 0);
    for split in 1..BINS {
        let side = |r: std::ops::Range<usize>| {
            r.fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), 0), |(l, h, c), k| (l.min(bins[k].0), h.max(bins[k].1), c + bins[k].2))
        };
        let (l0, h0, c0) = side(0..split);
        let (l1, h1, c1) = side(split..BINS);
        if c0 == 0 || c1 == 0 {
            continue;
        }
        let cost = area(l0, h0) * c0 as f32 + area(l1, h1) * c1 as f32;
        if cost < best.0 {
            best = (cost, split);
        }
    }
    if best.1 == 0 || best.0 >= area(lo, hi) * n as f32 {
        return me;
    }
    // Partition by bin.
    let (mut i, mut j) = (start, end);
    while i < j {
        if bin_of(order[i]) < best.1 {
            i += 1;
        } else {
            j -= 1;
            order.swap(i, j);
        }
    }
    let mid = i;
    if mid == start || mid == end {
        return me;
    }
    let left = build(nodes, order, boxes, centers, start, mid);
    let right = build(nodes, order, boxes, centers, mid, end);
    let node = &mut nodes[me as usize];
    node.start = left;
    node.count = 0;
    node.right = right;
    me
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_of_many_triangles() {
        // A wall of small triangles at z = 0..9; the ray should hit z = 0 first.
        let mut tris = Vec::new();
        for z in 0..10 {
            for x in 0..20 {
                let p = Vec3::new(x as f32 * 0.1 - 1.0, -1.0, z as f32);
                tris.push([p, p + Vec3::new(0.1, 0.0, 0.0), p + Vec3::new(0.0, 2.0, 0.0)]);
            }
        }
        let bvh = Bvh::new(&tris);
        let h = bvh.hit(Vec3::new(-0.97, 0.0, -5.0), Vec3::Z, 1e-4, f32::MAX, &|_| false).unwrap();
        assert!((h.t - 5.0).abs() < 1e-4);
        assert!(bvh.hit(Vec3::new(5.0, 0.0, -5.0), Vec3::Z, 1e-4, f32::MAX, &|_| false).is_none());
        // Skipping the first wall's hits finds the next wall.
        let h = bvh.hit(Vec3::new(-0.97, 0.0, -5.0), Vec3::Z, 1e-4, f32::MAX, &|h| h.t < 5.5).unwrap();
        assert!((h.t - 6.0).abs() < 1e-4);
    }
}
