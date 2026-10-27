//! Lines the edge flow must follow, and what each input point is held to.
//!
//! - **Creases**: the input mesh's crease edges (`remesh`), with corners
//!   where they meet or turn.
//! - **Guides**: the agent's polylines, laid onto the surface.
//! - **Polygroup borders**: where the clay's groups change, through the
//!   middles of the edges whose ends differ.
//! - **The symmetry plane** x = 0, where the surface crosses it.
//!
//! Each line is a chain of segments with an id. An input point on a crease
//! is held to the crease (its direction, through itself); one within a
//! band of a guide, border or the plane is held to the nearest line's
//! direction through its nearest point (the field then lays a lattice
//! line along it). After extraction, output points within a third of a
//! lattice step of a line are put on it (`snap`) and slide along it while
//! the mesh is fitted.

use std::collections::HashMap;

use glam::Vec3;

use super::Surface;
use super::cleanup::project;
use super::field::{Constraint, Pull, tangent};
use super::remesh::{Lock, TriMesh};

/// What made a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Crease,
    Plane,
    Border,
    Guide,
}

/// One segment of a line.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub a: Vec3,
    pub b: Vec3,
    pub chain: u32,
    pub kind: Kind,
}

/// The lines, and a grid to find the nearest.
#[derive(Default)]
pub struct Features {
    pub segments: Vec<Segment>,
    /// Points where creases meet, end or turn.
    pub corners: Vec<Vec3>,
    /// Each chain's kind.
    pub chains: Vec<Kind>,
    size: f32,
    grid: HashMap<[i32; 3], Vec<u32>>,
    corner_grid: HashMap<[i32; 3], Vec<u32>>,
}

/// The nearest point to `p` on segment a-b.
pub fn nearest_on(a: Vec3, b: Vec3, p: Vec3) -> Vec3 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0);
    a + ab * t
}

impl Features {
    /// Chains `kind` segments given as (point-id pairs, positions): ids
    /// that meet join a chain.
    fn add_chains(&mut self, pieces: &[(u64, u64, Vec3, Vec3)], kind: Kind) {
        let mut ids: Vec<u64> = pieces.iter().flat_map(|p| [p.0, p.1]).collect();
        ids.sort_unstable();
        ids.dedup();
        let at = |id: u64| ids.binary_search(&id).expect("listed") as u32;
        let mut root: Vec<u32> = (0..ids.len() as u32).collect();
        fn find(r: &mut [u32], mut x: u32) -> u32 {
            while r[x as usize] != x {
                r[x as usize] = r[r[x as usize] as usize];
                x = r[x as usize];
            }
            x
        }
        for p in pieces {
            let (x, y) = (find(&mut root, at(p.0)), find(&mut root, at(p.1)));
            if x != y {
                root[x.max(y) as usize] = x.min(y);
            }
        }
        let mut chain_of: Vec<u32> = vec![u32::MAX; ids.len()];
        for p in pieces {
            let r = find(&mut root, at(p.0)) as usize;
            if chain_of[r] == u32::MAX {
                chain_of[r] = self.chains.len() as u32;
                self.chains.push(kind);
            }
            self.segments.push(Segment { a: p.2, b: p.3, chain: chain_of[r], kind });
        }
    }

    /// The creases of the input mesh, and its corners.
    pub fn add_creases(&mut self, m: &TriMesh, locks: &[Lock]) {
        let pieces: Vec<(u64, u64, Vec3, Vec3)> = m.creases.iter().map(|&(a, b)| (a as u64, b as u64, m.points[a as usize], m.points[b as usize])).collect();
        // Chains break at corners: a corner's id is per segment.
        let pieces: Vec<(u64, u64, Vec3, Vec3)> = pieces
            .iter()
            .enumerate()
            .map(|(i, &(a, b, pa, pb))| {
                let id = |v: u64| if locks[v as usize] == Lock::Corner { (1u64 << 40) + i as u64 * 2 + (v == b) as u64 } else { v };
                (id(a), id(b), pa, pb)
            })
            .collect();
        self.add_chains(&pieces, Kind::Crease);
        self.corners.extend(locks.iter().enumerate().filter(|(_, l)| **l == Lock::Corner).map(|(v, _)| m.points[v]));
    }

    /// Where the surface crosses x = 0: through each edge crossing.
    pub fn add_plane(&mut self, m: &TriMesh) {
        let cross = |a: u32, b: u32| -> Option<(u64, Vec3)> {
            let (pa, pb) = (m.points[a as usize], m.points[b as usize]);
            if (pa.x > 0.0) == (pb.x > 0.0) {
                return None;
            }
            let t = pa.x / (pa.x - pb.x);
            let mut q = pa + (pb - pa) * t;
            q.x = 0.0;
            Some(((a.min(b) as u64) << 32 | a.max(b) as u64, q))
        };
        let mut pieces = Vec::new();
        for t in &m.tris {
            let hits: Vec<(u64, Vec3)> = (0..3).filter_map(|k| cross(t[k], t[(k + 1) % 3])).collect();
            if let [x, y] = hits[..] {
                pieces.push((x.0, y.0, x.1, y.1));
            }
        }
        self.add_chains(&pieces, Kind::Plane);
    }

    /// Where the groups change: in each triangle whose points' groups
    /// differ, from the middle of each edge whose ends differ to the next
    /// (or to the triangle's middle when all three differ).
    pub fn add_borders(&mut self, m: &TriMesh, label: &[u16]) {
        let mut pieces = Vec::new();
        for (i, t) in m.tris.iter().enumerate() {
            let l = t.map(|v| label[v as usize]);
            if l[0] == l[1] && l[1] == l[2] {
                continue;
            }
            let mid = |k: usize| {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                ((a.min(b) as u64) << 32 | a.max(b) as u64, (m.points[a as usize] + m.points[b as usize]) * 0.5)
            };
            let split: Vec<(u64, Vec3)> = (0..3).filter(|&k| l[k] != l[(k + 1) % 3]).map(mid).collect();
            match split[..] {
                [x, y] => pieces.push((x.0, y.0, x.1, y.1)),
                _ => {
                    let c = ((1u64 << 63) | i as u64, (m.points[t[0] as usize] + m.points[t[1] as usize] + m.points[t[2] as usize]) / 3.0);
                    for x in split {
                        pieces.push((x.0, c.0, x.1, c.1));
                    }
                }
            }
        }
        // The border runs through edge middles, so it zigzags at the input's
        // edge length: each inner point moves toward its two neighbours'
        // middle (three Jacobi passes; ends and junctions stay).
        let mut ids: Vec<u64> = pieces.iter().flat_map(|p| [p.0, p.1]).collect();
        ids.sort_unstable();
        ids.dedup();
        let at = |id: u64| ids.binary_search(&id).expect("listed");
        let mut pos = vec![Vec3::ZERO; ids.len()];
        let mut next: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
        for p in &pieces {
            let (a, b) = (at(p.0), at(p.1));
            (pos[a], pos[b]) = (p.2, p.3);
            next[a].push(b);
            next[b].push(a);
        }
        for _ in 0..3 {
            pos = (0..ids.len()).map(|i| match next[i][..] {
                [a, b] => pos[i] * 0.5 + (pos[a] + pos[b]) * 0.25,
                _ => pos[i],
            }).collect();
        }
        let pieces: Vec<(u64, u64, Vec3, Vec3)> = pieces.iter().map(|p| (p.0, p.1, pos[at(p.0)], pos[at(p.1)])).collect();
        self.add_chains(&pieces, Kind::Border);
    }

    /// Corners where two lines cross (a guide or border over the symmetry
    /// plane, two guides, a guide over a crease): segments of different
    /// chains that pass within `tol` of each other, at the middle of their
    /// nearest points; none within `merge` of a corner there already.
    /// Needs `index` first (and indexes the corners again).
    pub fn add_crossings(&mut self, tol: f32, merge: f32, progress: &crate::progress::Progress) -> Result<(), String> {
        let mut found: Vec<Vec3> = Vec::new();
        for (i, s) in self.segments.iter().enumerate() {
            if i % 4096 == 0 {
                progress.check()?;
            }
            let mid = (s.a + s.b) * 0.5;
            for j in self.around(&self.grid, mid) {
                let t = &self.segments[j as usize];
                if j as usize <= i || t.chain == s.chain {
                    continue;
                }
                let (p, q) = closest_between(s.a, s.b, t.a, t.b);
                if p.distance(q) <= tol {
                    found.push((p + q) * 0.5);
                }
            }
        }
        let size = self.size;
        for c in found {
            let near = self.corners.iter().any(|k| k.distance(c) <= merge);
            if !near {
                self.corners.push(c);
            }
        }
        self.index(size);
        Ok(())
    }

    /// The agent's guides, laid onto the surface: each polyline split into
    /// pieces no longer than `step` and each point moved onto the surface.
    pub fn add_guides(&mut self, guides: &[Vec<Vec3>], step: f32, surface: &dyn Surface, cell: f32, progress: &crate::progress::Progress) -> Result<(), String> {
        for (g, line) in guides.iter().enumerate() {
            progress.check()?;
            let mut dense: Vec<Vec3> = Vec::new();
            for w in line.windows(2) {
                // `guide_samples` bounded the total before any work.
                let n = ((w[0].distance(w[1]) / step).ceil() as usize).max(1);
                for k in 0..n {
                    dense.push(w[0].lerp(w[1], k as f32 / n as f32));
                }
            }
            if let Some(&last) = line.last() {
                dense.push(last);
            }
            let on: Vec<Vec3> = dense.iter().map(|&p| project(surface, p, Vec3::ZERO, 4.0 * step, cell)).collect();
            let base = (g as u64) << 32;
            let pieces: Vec<(u64, u64, Vec3, Vec3)> = on.windows(2).enumerate().map(|(k, w)| (base + k as u64, base + k as u64 + 1, w[0], w[1])).collect();
            self.add_chains(&pieces, Kind::Guide);
        }
        Ok(())
    }

    /// Buckets the segments and corners for `nearest` (cells of `size`).
    pub fn index(&mut self, size: f32) {
        self.size = size.max(1e-6);
        self.grid.clear();
        self.corner_grid.clear();
        for (i, s) in self.segments.iter().enumerate() {
            let lo = (s.a.min(s.b) / self.size).floor().as_ivec3();
            let hi = (s.a.max(s.b) / self.size).floor().as_ivec3();
            if (hi - lo).max_element() > 4 {
                // A long segment: the cells along it (every half cell), not
                // its whole box. Segments are input edges or guide pieces
                // no longer than the input's edges, so this is rare.
                let n = ((s.a.distance(s.b) / (0.5 * self.size)).ceil() as usize).clamp(1, 1 << 16);
                let mut cells: Vec<[i32; 3]> = (0..=n).map(|k| (s.a.lerp(s.b, k as f32 / n as f32) / self.size).floor().as_ivec3().to_array()).collect();
                cells.dedup();
                for c in cells {
                    let list = self.grid.entry(c).or_default();
                    if list.last() != Some(&(i as u32)) {
                        list.push(i as u32);
                    }
                }
                continue;
            }
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        self.grid.entry([x, y, z]).or_default().push(i as u32);
                    }
                }
            }
        }
        for (i, c) in self.corners.iter().enumerate() {
            self.corner_grid.entry((*c / self.size).floor().as_ivec3().to_array()).or_default().push(i as u32);
        }
    }

    /// Candidates within one cell of `p` (`within` at most the cell size).
    fn around<'a>(&'a self, grid: &'a HashMap<[i32; 3], Vec<u32>>, p: Vec3) -> impl Iterator<Item = u32> + 'a {
        let c = (p / self.size).floor().as_ivec3();
        (0..27).flat_map(move |n| grid.get(&[c.x + n % 3 - 1, c.y + n / 3 % 3 - 1, c.z + n / 9 - 1]).into_iter().flatten().copied())
    }

    /// The nearest segment within `within` (at most the index's cell):
    /// its index and the nearest point on it; the lowest kind wins ties of
    /// distance, then the lowest index.
    pub fn nearest(&self, p: Vec3, within: f32, filter: impl Fn(&Segment) -> bool) -> Option<(u32, Vec3)> {
        self.nearest_by(p, within, |s, _| filter(s))
    }

    /// `nearest`, among segments `accept` takes at their distance from
    /// `p` (a line's own reach is checked here, so a nearer line out of
    /// its reach never hides a farther one in reach).
    pub fn nearest_by(&self, p: Vec3, within: f32, accept: impl Fn(&Segment, f32) -> bool) -> Option<(u32, Vec3)> {
        let mut best: Option<(f32, Kind, u32, Vec3)> = None;
        for i in self.around(&self.grid, p) {
            let s = &self.segments[i as usize];
            let q = nearest_on(s.a, s.b, p);
            let d = q.distance(p);
            if d <= within && accept(s, d) && best.is_none_or(|b| (d, s.kind, i) < (b.0, b.1, b.2)) {
                best = Some((d, s.kind, i, q));
            }
        }
        best.map(|b| (b.2, b.3))
    }

    /// The nearest corner within `within`.
    pub fn corner(&self, p: Vec3, within: f32) -> Option<Vec3> {
        let mut best: Option<(f32, u32)> = None;
        for i in self.around(&self.corner_grid, p) {
            let d = self.corners[i as usize].distance(p);
            if d <= within && best.is_none_or(|b| (d, i) < b) {
                best = Some((d, i));
            }
        }
        best.map(|b| self.corners[b.1 as usize])
    }

    /// The index's cell size (the farthest `nearest` looks).
    pub fn cell(&self) -> f32 {
        self.size
    }

    /// Corners where creases cross the symmetry plane (x = 0).
    pub fn add_plane_corners(&mut self) {
        let mut found = Vec::new();
        for s in self.segments.iter().filter(|s| s.kind == Kind::Crease) {
            if s.a.x == 0.0 {
                found.push(s.a);
            } else if (s.a.x > 0.0) != (s.b.x > 0.0) && s.b.x != 0.0 {
                let t = s.a.x / (s.a.x - s.b.x);
                let mut q = s.a + (s.b - s.a) * t;
                q.x = 0.0;
                found.push(q);
            }
        }
        self.corners.extend(found);
    }

    /// `p` put on chain `chain` (its nearest point within the cell size;
    /// on the plane, x = 0).
    pub fn slide(&self, chain: u32, p: Vec3) -> Vec3 {
        if self.chains[chain as usize] == Kind::Plane {
            return Vec3::new(0.0, p.y, p.z);
        }
        self.nearest(p, self.size, |s| s.chain == chain).map_or(p, |(_, q)| q)
    }
}

/// What each input point is held to (see the module doc): crease points
/// to their crease, corners to themselves, points within `band` of a
/// guide or border (up to `line_wide` for a large one) or `wide` of the plane,
/// to the nearest one.
pub fn constraints(m: &TriMesh, locks: &[Lock], normals: &[Vec3], f: &Features, band: f32, wide: f32, line_wide: f32) -> Vec<Option<Constraint>> {
    // Each chain's band: `wide` for the plane; for a guide or border at
    // most a fifth of its extent (a loop tighter than the lattice keeps
    // its inside free), and never under `band`.
    let mut lo = vec![Vec3::MAX; f.chains.len()];
    let mut hi = vec![Vec3::MIN; f.chains.len()];
    for s in &f.segments {
        let c = s.chain as usize;
        lo[c] = lo[c].min(s.a.min(s.b));
        hi[c] = hi[c].max(s.a.max(s.b));
    }
    let reach: Vec<f32> = (0..f.chains.len()).map(|c| if f.chains[c] == Kind::Plane { wide } else { (0.2 * (hi[c] - lo[c]).length()).clamp(band, line_wide.max(band)) }).collect();
    let mut along: Vec<Vec<u32>> = vec![Vec::new(); m.points.len()];
    for &(a, b) in &m.creases {
        along[a as usize].push(b);
        along[b as usize].push(a);
    }
    (0..m.points.len())
        .map(|i| {
            let (p, n) = (m.points[i], normals[i]);
            match locks[i] {
                Lock::Corner => Some(Constraint { dir: Vec3::ZERO, point: p, corner: true }),
                Lock::Crease => {
                    let d = m.points[along[i][1] as usize] - m.points[along[i][0] as usize];
                    tangent(d, n).map(|dir| Constraint { dir, point: p, corner: false })
                }
                Lock::Free => {
                    // Each line's reach is checked in the search: a guide or
                    // border that is nearest but out of its reach must not
                    // take the point from the plane (or another line).
                    let (k, q) = f.nearest_by(p, wide.max(band), |s, d| s.kind != Kind::Crease && d <= reach[s.chain as usize])?;
                    let s = &f.segments[k as usize];
                    let dir = if s.kind == Kind::Plane { tangent(n.cross(Vec3::X), n)? } else { tangent(s.b - s.a, n)? };
                    Some(Constraint { dir, point: q, corner: false })
                }
            }
        })
        .collect()
}

/// The principal curvatures (1/world) and the direction of the larger at
/// `p` (normal `n`), from the field's Hessian by central differences at
/// spacing `h`.
pub fn principal(surface: &dyn Surface, p: Vec3, n: Vec3, h: f32) -> Option<(f32, f32, Vec3)> {
    let f = surface.near(p, 2.0 * h);
    let at = |x: f32, y: f32, z: f32| f(p + Vec3::new(x, y, z) * h);
    let c = at(0.0, 0.0, 0.0);
    let g = Vec3::new(at(1.0, 0.0, 0.0) - at(-1.0, 0.0, 0.0), at(0.0, 1.0, 0.0) - at(0.0, -1.0, 0.0), at(0.0, 0.0, 1.0) - at(0.0, 0.0, -1.0)) / (2.0 * h);
    let gl = g.length();
    if gl < 1e-6 {
        return None;
    }
    let d2 = |a: usize| {
        let mut e = [0.0f32; 3];
        e[a] = 1.0;
        (at(e[0], e[1], e[2]) - 2.0 * c + at(-e[0], -e[1], -e[2])) / (h * h)
    };
    let mixed = |a: usize, b: usize| {
        let mut e = [[0.0f32; 3]; 4];
        for (k, s) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)].iter().enumerate() {
            e[k][a] = s.0;
            e[k][b] = s.1;
        }
        (at(e[0][0], e[0][1], e[0][2]) - at(e[1][0], e[1][1], e[1][2]) - at(e[2][0], e[2][1], e[2][2]) + at(e[3][0], e[3][1], e[3][2])) / (4.0 * h * h)
    };
    let (xx, yy, zz, xy, xz, yz) = (d2(0), d2(1), d2(2), mixed(0, 1), mixed(0, 2), mixed(1, 2));
    let hm = |v: Vec3| Vec3::new(xx * v.x + xy * v.y + xz * v.z, xy * v.x + yy * v.y + yz * v.z, xz * v.x + yz * v.y + zz * v.z);
    let u = n.any_orthonormal_vector();
    let w = n.cross(u);
    // The shape operator on the tangent plane (convex +).
    let (a, b, d) = (u.dot(hm(u)) / gl, u.dot(hm(w)) / gl, w.dot(hm(w)) / gl);
    let (mid, half) = ((a + d) * 0.5, (((a - d) * 0.5).powi(2) + b * b).sqrt());
    let (k1, k2) = (mid + half, mid - half);
    let big = if k1.abs() >= k2.abs() { k1 } else { k2 };
    let small = if k1.abs() >= k2.abs() { k2 } else { k1 };
    let (dx, dy) = if b.abs() > 1e-12 { (big - d, b) } else if (a - big).abs() < (d - big).abs() { (1.0, 0.0) } else { (0.0, 1.0) };
    Some((big, small, (u * dx + w * dy).normalize_or(u)))
}

/// The curvature pull at each point: the larger principal direction,
/// weighted by how much the curvatures differ at the lattice's scale
/// (`h`), at most `strength`.
pub fn pulls(curv: &[Option<(f32, f32, Vec3)>], h: f32, strength: f32) -> Vec<Pull> {
    curv.iter().map(|c| c.and_then(|(k1, k2, d)| {
        let w = ((k1 - k2).abs() * h).min(1.0) * strength;
        (w > 0.01 * strength).then_some((d, w))
    })).collect()
}

/// The nearest points of segments a-b and c-d.
pub fn closest_between(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> (Vec3, Vec3) {
    let (u, v, w) = (b - a, d - c, a - c);
    let (aa, bb, cc, dd, ee) = (u.dot(u), u.dot(v), v.dot(v), u.dot(w), v.dot(w));
    let den = aa * cc - bb * bb;
    let mut s = if den > 1e-20 { ((bb * ee - cc * dd) / den).clamp(0.0, 1.0) } else { 0.0 };
    let t = if cc > 1e-20 { ((bb * s + ee) / cc).clamp(0.0, 1.0) } else { 0.0 };
    if aa > 1e-20 {
        s = ((bb * t - dd) / aa).clamp(0.0, 1.0);
    }
    (a + u * s, c + v * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point nearer a small guide loop crossing x = 0 than the plane,
    /// but out of the guide's reach, keeps its plane hold (it had none:
    /// the nearest line was picked first and its reach checked after).
    #[test]
    fn a_guide_out_of_reach_keeps_the_plane_hold() {
        // A flat grid in z = 0 whose points miss x = 0, so the plane
        // crosses every row.
        let n = 21;
        let at = |i: usize| -1.0 + 0.1 * i as f32 + 0.05;
        let mut points = Vec::new();
        for j in 0..n {
            for i in 0..n {
                points.push(Vec3::new(at(i) - 0.1, at(j) - 0.1, 0.0));
            }
        }
        let mut tris = Vec::new();
        for j in 0..n - 1 {
            for i in 0..n - 1 {
                let v = (j * n + i) as u32;
                tris.push([v, v + 1, v + n as u32 + 1]);
                tris.push([v, v + n as u32 + 1, v + n as u32]);
            }
        }
        let m = TriMesh { points, tris, creases: Default::default(), span: 0.1 };
        let mut f = Features::default();
        f.add_plane(&m);
        // A guide loop of radius 0.05 about (0, 0.5), crossing the plane.
        let ring: Vec<Vec3> = (0..=24).map(|k| {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            Vec3::new(0.05 * a.cos(), 0.5 + 0.05 * a.sin(), 0.0)
        }).collect();
        let pieces: Vec<(u64, u64, Vec3, Vec3)> = ring.windows(2).enumerate().map(|(k, w)| (k as u64, (k as u64 + 1) % 24, w[0], w[1])).collect();
        f.add_chains(&pieces, Kind::Guide);
        let (band, wide, line_wide) = (0.02, 0.15, 0.15);
        f.index(wide);
        // The test point: 0.04 from the loop (its reach is about 0.028),
        // 0.09 from the plane (within `wide`).
        let probe = Vec3::new(0.09, 0.5, 0.0);
        let mut m = m;
        m.points.push(probe);
        let locks = vec![Lock::Free; m.points.len()];
        let normals = vec![Vec3::Z; m.points.len()];
        let c = constraints(&m, &locks, &normals, &f, band, wide, line_wide);
        let held = c.last().unwrap().expect("held to the plane");
        assert!(held.point.x.abs() < 1e-6, "held to {:?}, not the plane", held.point);
        assert!(held.dir.x.abs() < 1e-5, "the plane's direction runs along it: {:?}", held.dir);
        // Points near the seam around the loop keep a hold too.
        let near_seam = (0..m.points.len() - 1).filter(|&i| m.points[i].x.abs() < 0.1 && (m.points[i].y - 0.5).abs() < 0.2);
        for i in near_seam {
            assert!(c[i].is_some(), "point {i} at {:?} lost its hold", m.points[i]);
        }
    }
}
