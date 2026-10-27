//! The orientation field (4-RoSy, Jakob et al. 2015 §3): a tangent
//! direction per vertex, meaningful up to quarter turns about its normal,
//! smoothed so neighbours agree. Edge flow follows it.
//!
//! Each update rotates every neighbour's direction by the quarter turn
//! that best matches (the extrinsic rule: the closest pair among
//! {q, n × q} of the two vertices), then averages them in order, as
//! Instant Meshes does. Constrained vertices (creases, guides, polygroup
//! borders, the symmetry plane) keep their direction; a soft pull toward
//! the principal curvature direction joins the average with a weight from
//! its confidence. Levels are solved coarse to fine and each level's
//! result is the next finer level's start (`hierarchy::sweep`, so the
//! field is the same on any number of threads).

use glam::Vec3;

use super::hierarchy::{Hierarchy, Level, sweep};

/// A fixed direction or point at a vertex: the edge flow must run along
/// `dir` (a line through `point`), or pass through `point` both ways
/// (`corner`, where lines meet: a cube's corner).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Constraint {
    /// Unit tangent of the line (zero for a corner).
    pub dir: Vec3,
    pub point: Vec3,
    pub corner: bool,
}

/// Per vertex: Some for constrained vertices.
pub type Constraints = Vec<Option<Constraint>>;

/// The pair among {q0, n0 × q0} and ±{q1, n1 × q1} that agree best: q0's
/// representative and q1's turned to match it.
#[inline]
pub fn compat_orientation(q0: Vec3, n0: Vec3, q1: Vec3, n1: Vec3) -> (Vec3, Vec3) {
    let a = [q0, n0.cross(q0)];
    let b = [q1, n1.cross(q1)];
    let (mut best, mut bi, mut bj) = (f32::NEG_INFINITY, 0, 0);
    for (i, x) in a.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            let s = x.dot(*y).abs();
            if s > best {
                (best, bi, bj) = (s, i, j);
            }
        }
    }
    let d = a[bi].dot(b[bj]);
    (a[bi], if d < 0.0 { -b[bj] } else { b[bj] })
}

/// `q1` turned by the quarter turn about `n1` that best matches `q0`.
#[inline]
pub fn rotate_to(q0: Vec3, q1: Vec3, n1: Vec3) -> Vec3 {
    // The best of the four turns of q1 (q1, n1 × q1, -q1, -(n1 × q1)) by
    // their dot with q0 (q0 itself is kept, unlike `compat_orientation`).
    let t1 = n1.cross(q1);
    let mut best = q1;
    let mut score = q0.dot(q1);
    for c in [t1, -q1, -t1] {
        let s = q0.dot(c);
        if s > score {
            (best, score) = (c, s);
        }
    }
    best
}

/// `d` in the tangent plane of `n`, unit (None when it is along `n`).
#[inline]
pub fn tangent(d: Vec3, n: Vec3) -> Option<Vec3> {
    (d - n * n.dot(d)).try_normalize()
}

/// A soft pull at a vertex: a direction and its weight.
pub type Pull = Option<(Vec3, f32)>;

/// The constraints of each level: a coarse vertex takes the first of its
/// children's (lowest index), its direction laid in its own tangent plane.
pub fn restrict_constraints(h: &Hierarchy, fine: Constraints) -> Vec<Constraints> {
    let mut out = vec![fine];
    for l in 0..h.levels.len() - 1 {
        let (lv, coarse) = (&h.levels[l], &h.levels[l + 1]);
        let mut c: Constraints = vec![None; coarse.len()];
        for (i, k) in out[l].iter().enumerate() {
            let p = lv.parent[i] as usize;
            if let (Some(k), None) = (k, &c[p]) {
                let dir = if k.corner { Vec3::ZERO } else { tangent(k.dir, coarse.n[p]).unwrap_or(Vec3::ZERO) };
                c[p] = Some(Constraint { dir, point: k.point, corner: k.corner || dir == Vec3::ZERO });
            }
        }
        out.push(c);
    }
    out
}

/// The soft pulls of each level: children's directions summed (each
/// turned to match the first's) weighted by area and pull, then
/// normalised; the weight is the children's area-weighted mean.
pub fn restrict_pulls(h: &Hierarchy, fine: Vec<Pull>) -> Vec<Vec<Pull>> {
    let mut out = vec![fine];
    for l in 0..h.levels.len() - 1 {
        let (lv, coarse) = (&h.levels[l], &h.levels[l + 1]);
        let mut sum: Vec<(Vec3, f32, f32)> = vec![(Vec3::ZERO, 0.0, 0.0); coarse.len()];
        for (i, k) in out[l].iter().enumerate() {
            let p = lv.parent[i] as usize;
            let a = lv.area[i];
            sum[p].2 += a;
            if let Some((d, w)) = *k {
                let np = coarse.n[p];
                let d = tangent(d, np).unwrap_or(Vec3::ZERO);
                let d = if sum[p].0 == Vec3::ZERO { d } else { rotate_to(sum[p].0, d, np) };
                sum[p].0 += d * (a * w);
                sum[p].1 += a * w;
            }
        }
        out.push(
            sum.iter()
                .enumerate()
                .map(|(p, &(d, w, a))| tangent(d, coarse.n[p]).filter(|_| w > 0.0).map(|d| (d, w / a.max(1e-30))))
                .collect(),
        );
    }
    out
}

/// The field on one level: `q` smoothed `iterations` times.
fn smooth(level: &Level, q: &mut [Vec3], constraints: &Constraints, pulls: &[Pull], iterations: usize, progress: &crate::progress::Progress) -> Result<(), String> {
    for _ in 0..iterations {
        progress.check()?;
        sweep(level, q, |i, q| {
            let ni = level.n[i];
            if let Some(c) = constraints[i].filter(|c| !c.corner) {
                return c.dir;
            }
            let mut sum = q[i];
            let mut weight = 0.0f32;
            for &(j, w) in level.neighbours(i) {
                let (a, b) = compat_orientation(sum, ni, q[j as usize], level.n[j as usize]);
                sum = a * weight + b * w;
                sum -= ni * ni.dot(sum);
                weight += w;
                sum = sum.normalize_or(q[i]);
            }
            if let Some((d, pull)) = pulls[i] {
                let w = pull * weight;
                let (a, b) = compat_orientation(sum, ni, d, ni);
                sum = a * weight + b * w;
                sum -= ni * ni.dot(sum);
                sum = sum.normalize_or(q[i]);
            }
            sum
        });
    }
    Ok(())
}

/// The orientation field of every level (input level first), solved
/// coarse to fine from `start` (input-level directions, restricted to the
/// coarse levels): each level as it was when its smoothing ended.
pub fn solve(h: &Hierarchy, start: &[Vec3], constraints: &[Constraints], pulls: &[Vec<Pull>], iterations: usize, progress: &crate::progress::Progress) -> Result<Vec<Vec<Vec3>>, String> {
    // Start values on every level: children's summed, turned to agree.
    let mut starts: Vec<Vec<Vec3>> = vec![start.to_vec()];
    for l in 0..h.levels.len() - 1 {
        let (lv, coarse) = (&h.levels[l], &h.levels[l + 1]);
        let mut sum = vec![Vec3::ZERO; coarse.len()];
        for (i, &q) in starts[l].iter().enumerate() {
            let p = lv.parent[i] as usize;
            let np = coarse.n[p];
            let q = tangent(q, np).unwrap_or(q);
            let turned = if sum[p] == Vec3::ZERO { q } else { rotate_to(sum[p], q, np) };
            sum[p] += turned * lv.area[i];
        }
        starts.push(sum.iter().enumerate().map(|(p, &s)| tangent(s, coarse.n[p]).unwrap_or_else(|| coarse.n[p].any_orthonormal_vector())).collect());
    }
    let top = h.levels.len() - 1;
    let mut out: Vec<Vec<Vec3>> = vec![Vec::new(); top + 1];
    let mut q = std::mem::take(&mut starts[top]);
    for l in (0..=top).rev() {
        smooth(&h.levels[l], &mut q, &constraints[l], &pulls[l], iterations, progress)?;
        if l > 0 {
            let fine = &h.levels[l - 1];
            let next = (0..fine.len()).map(|i| tangent(q[fine.parent[i] as usize], fine.n[i]).unwrap_or(starts[l - 1][i])).collect();
            out[l] = std::mem::replace(&mut q, next);
        }
    }
    out[0] = q;
    Ok(out)
}

/// The quarter turns (0-3) about `n1` that bring `q1` closest to `q0`.
pub fn turns(q0: Vec3, q1: Vec3, n1: Vec3) -> u32 {
    let t1 = n1.cross(q1);
    let c = [q1, t1, -q1, -t1];
    (0..4).max_by(|&a, &b| q0.dot(c[a]).total_cmp(&q0.dot(c[b])).then(b.cmp(&a))).expect("four") as u32
}

/// Triangles around which the field turns (its singularities): carried
/// from corner to corner by the best quarter turn each time, it comes back
/// to the first corner turned.
pub fn singularities(tris: &[[u32; 3]], q: &[Vec3], n: &[Vec3]) -> usize {
    tris.iter()
        .filter(|t| {
            let mut cur = q[t[0] as usize];
            for &b in &t[1..] {
                cur = rotate_to(cur, q[b as usize], n[b as usize]);
            }
            turns(cur, q[t[0] as usize], n[t[0] as usize]) != 0
        })
        .count()
}
