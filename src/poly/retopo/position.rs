//! The position field (Jakob et al. 2015 §4): per vertex, a point `o` of
//! a local square lattice of spacing h laid along its orientation field
//! (`o + h (a q + b (n × q))` for whole a, b), smoothed so neighbouring
//! lattices agree. Quads are the lattice cells: `extract` collapses
//! vertices on one lattice point and joins those one step apart.
//!
//! Each update moves every neighbour's lattice point to the translate
//! nearest the vertex's own (`compat`), averages them in order, then
//! rounds back to the lattice point nearest the vertex. A constrained
//! vertex snaps its lattice so a lattice line runs through the
//! constraint's point along its direction (both lines at a corner); the
//! snap moves by less than half a cell, so any whole shift of the lattice
//! still satisfies it. Levels run coarse to fine (`hierarchy::sweep`).

use glam::Vec3;

use super::field::{Constraint, Constraints};
use super::hierarchy::{Hierarchy, Level, sweep};

/// `o` moved by whole steps of `h` along `q` and `t` to the lattice point
/// at or below `p` in both (and those steps).
#[inline]
fn floor_index(o: Vec3, q: Vec3, t: Vec3, p: Vec3, h: f32) -> (Vec3, [i32; 2]) {
    let d = p - o;
    let (a, b) = ((q.dot(d) / h).floor(), (t.dot(d) / h).floor());
    (o + (q * a + t * b) * h, [a as i32, b as i32])
}

/// `o` moved by whole steps of `h` to the lattice point nearest `p`.
#[inline]
pub fn round(o: Vec3, q: Vec3, n: Vec3, p: Vec3, h: f32) -> Vec3 {
    let t = n.cross(q);
    let d = p - o;
    o + (q * (q.dot(d) / h).round() + t * (t.dot(d) / h).round()) * h
}

/// A point between two vertices on both tangent planes (Instant Meshes'
/// `middle_point`): where the lattices are compared.
#[inline]
fn middle(p0: Vec3, n0: Vec3, p1: Vec3, n1: Vec3) -> Vec3 {
    let (n0p0, n0p1, n1p0, n1p1, n0n1) = (n0.dot(p0), n0.dot(p1), n1.dot(p0), n1.dot(p1), n0.dot(n1));
    let denom = 1.0 / (1.0 - n0n1 * n0n1 + 1e-4);
    let l0 = 2.0 * (n0p1 - n0p0 - n0n1 * (n1p0 - n1p1)) * denom;
    let l1 = 2.0 * (n1p0 - n1p1 - n0n1 * (n0p1 - n0p0)) * denom;
    0.5 * (p0 + p1) - 0.25 * (n0 * l0 + n1 * l1)
}

/// One vertex's side of a lattice comparison.
#[derive(Clone, Copy)]
pub struct Side {
    /// The vertex and its normal.
    pub v: Vec3,
    pub n: Vec3,
    /// Its orientation and lattice point.
    pub q: Vec3,
    pub o: Vec3,
}

/// The translates of the two lattices nearest each other (near the
/// vertices' middle), and their whole steps from each lattice point.
#[inline]
pub fn compat(a: Side, b: Side, h: f32) -> (Vec3, Vec3, [i32; 2], [i32; 2]) {
    let (ta, tb) = (a.n.cross(a.q), b.n.cross(b.q));
    let m = middle(a.v, a.n, b.v, b.n);
    let (oa, ia) = floor_index(a.o, a.q, ta, m, h);
    let (ob, ib) = floor_index(b.o, b.q, tb, m, h);
    let mut best = (f32::INFINITY, Vec3::ZERO, Vec3::ZERO, [0; 2], [0; 2]);
    for i in 0..4i32 {
        let sa = [i & 1, i >> 1];
        let pa = oa + (a.q * sa[0] as f32 + ta * sa[1] as f32) * h;
        for j in 0..4i32 {
            let sb = [j & 1, j >> 1];
            let pb = ob + (b.q * sb[0] as f32 + tb * sb[1] as f32) * h;
            let d = pa.distance_squared(pb);
            if d < best.0 {
                best = (d, pa, pb, [ia[0] + sa[0], ia[1] + sa[1]], [ib[0] + sb[0], ib[1] + sb[1]]);
            }
        }
    }
    (best.1, best.2, best.3, best.4)
}

/// `o` moved by less than half a step so a lattice line along `c.dir`
/// runs through `c.point` (both lines at a corner).
#[inline]
pub fn snap(o: Vec3, q: Vec3, n: Vec3, c: &Constraint, h: f32) -> Vec3 {
    let wrap = |d: f32| d - h * (d / h).round();
    if c.corner {
        let t = n.cross(q);
        let d = c.point - o;
        return o + q * wrap(q.dot(d)) + t * wrap(t.dot(d));
    }
    let b = n.cross(c.dir);
    o + b * wrap(b.dot(c.point - o))
}

/// The lattice points on one level, smoothed `iterations` times.
fn smooth(level: &Level, q: &[Vec3], scale: &[f32], o: &mut [Vec3], constraints: &Constraints, iterations: usize, progress: &crate::progress::Progress) -> Result<(), String> {
    for _ in 0..iterations {
        progress.check()?;
        sweep(level, o, |i, o| {
            let (vi, ni, qi) = (level.v[i], level.n[i], q[i]);
            let mut sum = o[i];
            let mut weight = 0.0f32;
            for &(j, w) in level.neighbours(i) {
                let j = j as usize;
                let h = 0.5 * (scale[i] + scale[j]);
                let (a, b, _, _) = compat(Side { v: vi, n: ni, q: qi, o: sum }, Side { v: level.v[j], n: level.n[j], q: q[j], o: o[j] }, h);
                sum = (a * weight + b * w) / (weight + w);
                weight += w;
                sum -= ni * ni.dot(sum - vi);
            }
            let mut out = if weight > 0.0 { round(sum, qi, ni, vi, scale[i]) } else { o[i] };
            if let Some(c) = &constraints[i] {
                out = snap(out, qi, ni, c, scale[i]);
            }
            out
        });
    }
    Ok(())
}

/// The lattice points on the input level, solved coarse to fine. `q` is
/// each level's orientation field, `scale` each level's spacing.
pub fn solve(h: &Hierarchy, q: &[Vec<Vec3>], scale: &[Vec<f32>], constraints: &[Constraints], iterations: usize, progress: &crate::progress::Progress) -> Result<Vec<Vec3>, String> {
    let top = h.levels.len() - 1;
    let mut o = h.levels[top].v.clone();
    for l in (0..=top).rev() {
        let level = &h.levels[l];
        smooth(level, &q[l], &scale[l], &mut o, &constraints[l], iterations, progress)?;
        if l > 0 {
            let fine = &h.levels[l - 1];
            o = (0..fine.len())
                .map(|i| {
                    let (v, n) = (fine.v[i], fine.n[i]);
                    let p = o[fine.parent[i] as usize];
                    let p = p - n * n.dot(p - v);
                    round(p, q[l - 1][i], n, v, scale[l - 1][i])
                })
                .collect();
        }
    }
    Ok(o)
}
