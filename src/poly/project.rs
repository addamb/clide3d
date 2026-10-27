//! Projecting a clay's detail onto a Poly's subdivision levels
//! (docs/QUALITY_PLAN.md Stage 3c, D17).
//!
//! Level k is `CC(level k − 1) + F_k · D_k` (see `subdiv`). Projection
//! runs coarse to fine: level k's points are subdivided from level k − 1
//! as projected, each is moved onto the clay, and the move is stored in
//! its frame as `D_k`. Each level therefore holds what is left after the
//! coarser ones (its own band of detail), and the finest level lies on
//! the clay.
//!
//! Where a point goes:
//! - **scalar**: the nearest crossing of the clay's surface along the
//!   point's normal line, either way, within `reach` (a walk that never
//!   jumps sideways across a thin gap, as `adaptive::along`);
//! - **vector** (the default): the nearest point of the surface (Newton
//!   steps on the field from the point), taken when the line to it leans
//!   at most `cone` degrees from the normal line and lies within `reach`.
//!   This catches detail pushed sideways and overhangs up to the cone,
//!   which a normal-only search cannot. Where the scalar and vector
//!   points disagree by more than reach / 2, the vector one has latched
//!   onto another surface (across lips, between fingers): the scalar one
//!   wins and is counted.
//!
//! Every point is a pure function of the level before it, worked in
//! parallel chunks into its own slot, so a projection is bit-identical on
//! any thread count.

use glam::{Mat3, Mat4, Vec3};
use serde::Serialize;

use super::retopo::Surface;
use super::subdiv::{Refiner, frames, subdivide_points};
use super::{Chunked, Poly};
use crate::clay::par_chunks;
use crate::progress::Progress;

/// Most degrees the vector search may lean from the normal line.
pub const MAX_CONE: f32 = 80.0;
/// The default lean.
pub const DEFAULT_CONE: f32 = 45.0;
/// The default reach, in mean level-0 edge lengths.
pub const REACH_EDGES: f32 = 3.0;
/// Most points distances and normals are measured at.
pub const MEASURE_SAMPLES: usize = 40_000;
/// Most triangles the clay-to-level coverage is measured against.
pub const COVERAGE_TRIANGLES: usize = 3_000_000;
/// Points per parallel chunk.
const PAR: usize = 2048;
/// Newton steps of the vector search.
const NEWTON: usize = 12;
/// Bisection steps once a crossing is bracketed.
const BISECT: usize = 18;
/// Most smoothing passes a level's moves get (see `band_limit`).
pub const MAX_SMOOTH: usize = 128;
/// Weight of the neighbours' mean in one smoothing pass.
const SMOOTH_LAMBDA: f32 = 0.5;

/// How a point finds the clay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Vector,
    Scalar,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Vector => "vector",
            Mode::Scalar => "scalar",
        }
    }
}

/// What a projection does (bounded by the caller).
#[derive(Clone, Copy, Debug)]
pub struct Args {
    /// Levels 1..=levels get detail.
    pub levels: u8,
    pub mode: Mode,
    /// Degrees the vector search may lean from the normal line.
    pub cone: f32,
    /// Farthest a point may move, in the Poly's units.
    pub reach: f32,
}

/// The clay a projection reads, seen from the Poly's space.
pub struct Target<'a> {
    pub surface: &'a dyn Surface,
    /// The Poly's space to the clay's.
    pub to_clay: Mat4,
    /// The clay's mesh cell, in its own units (what distances are
    /// reported in).
    pub cell: f32,
}

impl Target<'_> {
    /// The most a Poly-space length grows into the clay's space: the
    /// linear part's largest singular value (1 when the Poly sits in the
    /// clay's place, as `retopo` and `from_clay` put it).
    pub fn stretch(&self) -> f32 {
        stretch(Mat3::from_mat4(self.to_clay))
    }

    /// The clay's field around `p` (Poly space), exact within `reach`
    /// (Poly units) of it; values in the clay's units.
    fn near(&self, p: Vec3, reach: f32) -> impl Fn(Vec3) -> f32 + '_ {
        let f = self.surface.near(self.to_clay.transform_point3(p), reach * self.stretch());
        let m = self.to_clay;
        move |q: Vec3| f(m.transform_point3(q))
    }
}

/// The largest singular value of `m` (power iteration on mᵀm, from a
/// fixed start: deterministic).
pub fn stretch(m: Mat3) -> f32 {
    let mtm = m.transpose() * m;
    let mut v = Vec3::new(0.577_350_3, 0.577_350_3, 0.577_350_3);
    let mut l = 0.0;
    for _ in 0..64 {
        let w = mtm * v;
        l = w.length();
        if !(l > 1e-30) {
            return 1e-12;
        }
        v = w / l;
    }
    // A start orthogonal to the top direction: try the axes too.
    let axes = [Vec3::X, Vec3::Y, Vec3::Z].map(|a| (m * a).length_squared());
    l.max(axes.into_iter().fold(0.0, f32::max)).sqrt().max(1e-12)
}

/// How a point was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// Already on the surface.
    On,
    /// The nearest surface point, within the cone.
    Vector,
    /// Along the normal line (scalar mode, or the vector search found
    /// nothing within the cone and reach).
    Scalar,
    /// The vector point was more than reach / 2 from the scalar one: the
    /// scalar won.
    ScalarWon,
    /// Nothing within reach: left where it was.
    Missed,
}

/// The search's lengths, in the Poly's units.
#[derive(Clone, Copy, Debug)]
pub struct Search {
    pub mode: Mode,
    /// cos of the cone.
    pub cos_cone: f32,
    pub reach: f32,
    /// Shortest walk step (a quarter of the clay's cell, in Poly units).
    pub step: f32,
    /// Field values within this (clay units) count as on the surface.
    pub tol: f32,
    /// `Target::stretch`.
    pub stretch: f32,
}

impl Search {
    pub fn new(target: &Target, mode: Mode, cone: f32, reach: f32) -> Search {
        let stretch = target.stretch();
        Search { mode, cos_cone: cone.clamp(0.0, MAX_CONE).to_radians().cos(), reach, step: 0.25 * target.cell / stretch, tol: 1e-4 * target.cell, stretch }
    }
}

/// The nearest `t` in (0, limit] where `f(p + dir t)` changes sign from
/// `d0` (= f(p)), walking by at least `s.step` and by half the field's
/// value (a distance bound) when that is more.
fn crossing(f: &dyn Fn(Vec3) -> f32, p: Vec3, d0: f32, dir: Vec3, limit: f32, s: &Search) -> Option<f32> {
    let (mut t, mut v) = (0.0f32, d0);
    for _ in 0..4096 {
        if t >= limit {
            return None;
        }
        let next = (t + (0.5 * v.abs() / s.stretch).max(s.step)).min(limit);
        let w = f(p + dir * next);
        if (w > 0.0) != (d0 > 0.0) {
            let (mut lo, mut hi) = (t, next);
            for _ in 0..BISECT {
                let m = 0.5 * (lo + hi);
                if (f(p + dir * m) > 0.0) == (d0 > 0.0) { lo = m } else { hi = m }
            }
            return Some(0.5 * (lo + hi));
        }
        (t, v) = (next, w);
    }
    None
}

/// The field's gradient at `q` (central differences at `s.step`).
fn gradient(f: &dyn Fn(Vec3) -> f32, q: Vec3, s: &Search) -> Vec3 {
    let e = s.step;
    Vec3::new(f(q + Vec3::X * e) - f(q - Vec3::X * e), f(q + Vec3::Y * e) - f(q - Vec3::Y * e), f(q + Vec3::Z * e) - f(q - Vec3::Z * e)) / (2.0 * e)
}

/// Whether surface point `q` faces the way `n` does: the wall a point
/// with normal `n` belongs on. The far wall of a part thinner than the
/// search (a plate, an ear, a lip) faces the other way and is refused.
fn faces_with(f: &dyn Fn(Vec3) -> f32, q: Vec3, n: Vec3, s: &Search) -> bool {
    gradient(f, q, s).dot(n) > 0.0
}

/// The nearest crossing along the line through `p` in direction `n`,
/// either way, within `s.reach`, on a wall facing the way `n` does.
fn along_normal(f: &dyn Fn(Vec3) -> f32, p: Vec3, d0: f32, n: Vec3, s: &Search) -> Option<Vec3> {
    // Toward the surface first (out of the solid along +n, into it along
    // -n), then the other way only as far as the first found.
    let first = if d0 > 0.0 { -n } else { n };
    let a = crossing(f, p, d0, first, s.reach, s).map(|t| p + first * t).filter(|&q| faces_with(f, q, n, s));
    let b = crossing(f, p, d0, -first, a.map_or(s.reach, |q| q.distance(p)), s).map(|t| p - first * t).filter(|&q| faces_with(f, q, n, s));
    b.or(a)
}

/// The surface point Newton steps on the field reach from `p` (the
/// nearest one where the field is a distance), within `s.reach`.
fn nearest_point(f: &dyn Fn(Vec3) -> f32, p: Vec3, s: &Search) -> Option<Vec3> {
    let mut q = p;
    for _ in 0..NEWTON {
        let v = f(q);
        if v.abs() <= s.tol {
            return Some(q);
        }
        let g = gradient(f, q, s);
        let gl = g.length_squared();
        if !(gl > 1e-12) {
            return None;
        }
        let mut d = g * (v / gl);
        let most = 0.5 * s.reach;
        if d.length() > most {
            d = d.normalize() * most;
        }
        q -= d;
        if q.distance(p) > s.reach {
            return None;
        }
    }
    // Not converged to `tol`: kept within a twentieth of a cell.
    (f(q).abs() <= 500.0 * s.tol).then_some(q)
}

/// Where `p` (normal `n`, unit) goes on the clay `f`, and how it was found.
pub fn find(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, s: &Search) -> (Vec3, Found) {
    let d0 = f(p);
    if d0.abs() <= s.tol {
        return (p, Found::On);
    }
    let scalar = along_normal(f, p, d0, n, s);
    if s.mode == Mode::Scalar {
        return scalar.map_or((p, Found::Missed), |q| (q, Found::Scalar));
    }
    let vector = nearest_point(f, p, s).filter(|&q| {
        let o = q - p;
        let l = o.length();
        l <= s.reach && (l <= s.step * 1e-3 || o.dot(n).abs() >= s.cos_cone * l) && faces_with(f, q, n, s)
    });
    // The normal's crossing unless the nearest point is under half as
    // far (the normal line skims past the surface: sideways detail, an
    // overhang's flank) or the normal finds nothing; a nearest point more
    // than reach / 2 from the crossing has latched onto another surface
    // (across lips, between fingers), and the crossing wins.
    match (vector, scalar) {
        (Some(v), Some(q)) if v.distance(p) < 0.5 * q.distance(p) => {
            if v.distance(q) > 0.5 * s.reach { (q, Found::ScalarWon) } else { (v, Found::Vector) }
        }
        (_, Some(q)) => (q, Found::Scalar),
        (Some(v), None) => (v, Found::Vector),
        (None, None) => (p, Found::Missed),
    }
}

/// One level's projection, in the reply.
#[derive(Clone, Debug, Default, Serialize)]
pub struct LevelStats {
    pub level: u8,
    pub points: usize,
    /// Root mean square and largest move, in the clay's cells.
    pub rms_cells: f32,
    pub max_cells: f32,
    pub on: usize,
    pub vector: usize,
    pub scalar: usize,
    pub scalar_won: usize,
    pub missed: usize,
    /// Smoothing passes the level's moves took (0 where its edges are at
    /// least the clay's cell; see `band_limit`).
    pub smoothing_passes: usize,
}

/// A projection: the displacements of levels 1..=levels and the finest
/// level's points as projected (Poly space).
pub struct Projection {
    pub displacement: Vec<Chunked<Vec3>>,
    pub stats: Vec<LevelStats>,
    pub points: Vec<Vec3>,
}

/// Band-limits a level's moves to the clay's sampling grid (Stage 3c
/// follow-up). The clay's field is reconstructed trilinearly from a grid
/// of samples a cell apart, so its surface turns in small steps at the
/// cell faces; a level whose edges are shorter than the cell resolves
/// them, and its projected points copy them, which shows in renders as
/// steps along the grid. The clay holds no detail finer than its cell, so
/// those moves are smoothed over the level's edges with a kernel about
/// half a cell wide: σ² = (c² − e²) / 4 at each point (c its cell, e its
/// mean edge, both in the clay's units), nothing where e ≥ c. Jacobi
/// passes (`SMOOTH_LAMBDA` of the neighbours' mean, scaled at each point
/// to its own σ), each a pure function of the last: bit-identical on any
/// thread count. Returns the moves and the passes run (0: unchanged).
pub fn band_limit(points: &[Vec3], edges: &[[u32; 2]], moves: Vec<Vec3>, target: &Target) -> (Vec<Vec3>, usize) {
    let stretch = target.stretch();
    let n = points.len();
    let (mut sum, mut count) = (vec![0.0f32; n], vec![0u32; n]);
    for &[a, b] in edges {
        let l = points[a as usize].distance(points[b as usize]) * stretch;
        for v in [a, b] {
            sum[v as usize] += l;
            count[v as usize] += 1;
        }
    }
    // Passes each point wants: on a quad grid of spacing e one pass
    // spreads the kernel by λ e² / 2 per axis.
    let mut want = vec![0.0f32; n];
    par_chunks(&mut want, PAR, |i, chunk| {
        for (j, w) in chunk.iter_mut().enumerate() {
            let v = i * PAR + j;
            if count[v] == 0 {
                continue;
            }
            let e = sum[v] / count[v] as f32;
            let c = target.surface.cell(target.to_clay.transform_point3(points[v])).unwrap_or(target.cell);
            if e > 0.0 && e < c {
                *w = 2.0 * 0.25 * (c * c - e * e) / (SMOOTH_LAMBDA * e * e);
            }
        }
    });
    let most = want.iter().copied().fold(0.0f32, f32::max);
    if !(most > 0.0) {
        return (moves, 0);
    }
    let passes = (most.ceil() as usize).clamp(1, MAX_SMOOTH);
    let weight: Vec<f32> = want.iter().map(|w| SMOOTH_LAMBDA * (w / passes as f32).min(1.0)).collect();
    // Neighbours, in edge order (a fixed order: the sums are the same on
    // any thread count).
    let mut start = vec![0u32; n + 1];
    for &[a, b] in edges {
        start[a as usize + 1] += 1;
        start[b as usize + 1] += 1;
    }
    for v in 0..n {
        start[v + 1] += start[v];
    }
    let mut fill = start.clone();
    let mut nb = vec![0u32; start[n] as usize];
    for &[a, b] in edges {
        nb[fill[a as usize] as usize] = b;
        fill[a as usize] += 1;
        nb[fill[b as usize] as usize] = a;
        fill[b as usize] += 1;
    }
    let mut cur = moves;
    let mut next = vec![Vec3::ZERO; n];
    for _ in 0..passes {
        par_chunks(&mut next, PAR, |i, chunk| {
            for (j, o) in chunk.iter_mut().enumerate() {
                let v = i * PAR + j;
                let around = &nb[start[v] as usize..start[v + 1] as usize];
                *o = if around.is_empty() || weight[v] == 0.0 {
                    cur[v]
                } else {
                    let mean = around.iter().fold(Vec3::ZERO, |s, &u| s + cur[u as usize]) / around.len() as f32;
                    cur[v] + (mean - cur[v]) * weight[v]
                };
            }
        });
        std::mem::swap(&mut cur, &mut next);
    }
    (cur, passes)
}

/// Mean level-0 edge length (the default reach's unit).
pub fn mean_edge(poly: &Poly) -> f32 {
    let p = poly.cage.to_vec();
    let (s, c) = poly.cage_faces();
    let (mut sum, mut n) = (0.0f64, 0usize);
    for f in s.windows(2) {
        let face = &c[f[0] as usize..f[1] as usize];
        for j in 0..face.len() {
            sum += p[face[j] as usize].distance(p[face[(j + 1) % face.len()] as usize]) as f64;
            n += 1;
        }
    }
    (sum / n.max(1) as f64) as f32
}

/// Projects `target`'s clay onto `poly`'s levels 1..=args.levels (see
/// the module doc); the cage itself is not moved.
pub fn project(poly: &Poly, target: &Target, args: &Args, progress: &Progress) -> Result<Projection, String> {
    super::check_levels(poly.faces() as u64, poly.corners.len() as u64, args.levels)?;
    if args.levels == 0 {
        return Err("project onto at least one level".into());
    }
    let r = Refiner::new(poly, args.levels)?;
    progress.check()?;
    let s = Search::new(target, args.mode, args.cone, args.reach);
    let counts = super::subdiv::counts(&r.topology[0], args.levels);
    let total: f64 = counts[1..].iter().map(|c| c.vertices as f64).sum();
    let mut done = 0.0f64;
    let mut p = poly.cage.to_vec();
    let mut displacement = Vec::new();
    let mut stats = Vec::new();
    for k in 1..=args.levels as usize {
        progress.check()?;
        let base = subdivide_points(&r.topology[k - 1], &p);
        let (fs, fc) = r.faces_of_level(k);
        let frames = frames(&base, fs, fc);
        let mut out: Vec<(Vec3, Found)> = vec![(Vec3::ZERO, Found::Missed); base.len()];
        let start = done;
        let chunks = std::sync::atomic::AtomicUsize::new(0);
        par_chunks(&mut out, PAR, |i, chunk| {
            if progress.cancelled() {
                return;
            }
            for (j, o) in chunk.iter_mut().enumerate() {
                let v = i * PAR + j;
                let f = target.near(base[v], s.reach);
                *o = find(&f, base[v], frames[v][2], &s);
            }
            let n = chunks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress.set(((start + (n * PAR).min(base.len()) as f64) / total).min(1.0) as f32 * 0.9);
        });
        progress.check()?;
        done += base.len() as f64;
        let edges = if k < r.level() { &r.topology[k].edges } else { &r.top.edges };
        let moves: Vec<Vec3> = base.iter().zip(&out).map(|(b, (q, _))| *q - *b).collect();
        let (moves, passes) = band_limit(&base, edges, moves, target);
        let mut d = Vec::with_capacity(base.len());
        let mut st = LevelStats { level: k as u8, points: base.len(), smoothing_passes: passes, ..Default::default() };
        let (mut sq, mut max) = (0.0f64, 0.0f32);
        let scale = s.stretch / target.cell;
        let mut next = Vec::with_capacity(base.len());
        for (((b, f), (_, how)), w) in base.iter().zip(&frames).zip(&out).zip(&moves) {
            let w = *w;
            let t = Vec3::new(w.dot(f[0]), w.dot(f[1]), w.dot(f[2]));
            // As `Refiner::points` applies it, so a later evaluation gives
            // these points bit for bit.
            next.push(*b + (f[0] * t.x + f[1] * t.y + f[2] * t.z));
            d.push(t);
            let l = w.length() * scale;
            sq += (l * l) as f64;
            max = max.max(l);
            match how {
                Found::On => st.on += 1,
                Found::Vector => st.vector += 1,
                Found::Scalar => st.scalar += 1,
                Found::ScalarWon => st.scalar_won += 1,
                Found::Missed => st.missed += 1,
            }
        }
        st.rms_cells = (sq / base.len().max(1) as f64).sqrt() as f32;
        st.max_cells = max;
        stats.push(st);
        displacement.push(Chunked::new(&d));
        p = next;
    }
    Ok(Projection { displacement, stats, points: p })
}

/// How close a level is to the clay, in the clay's cells.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Measure {
    /// The largest, mean and root mean square distance from the level's
    /// points, edge middles and face middles to the clay (sampled).
    pub hausdorff_cells: f32,
    pub mean_distance_cells: f32,
    pub rms_cells: f32,
    pub distance_samples: usize,
    /// Root mean square angle between the level's normals and the clay's
    /// at its points (sampled), degrees.
    pub normal_rms_deg: f32,
    /// The largest distance from the clay's surface points to the level's
    /// triangles (where the level leaves part of the clay out), when the
    /// level has at most `COVERAGE_TRIANGLES` triangles.
    pub coverage_cells: Option<f32>,
}

/// Evenly picked indices of `n`, at most `most`.
fn picked(n: usize, most: usize) -> impl Iterator<Item = usize> {
    (0..n).step_by(n.div_ceil(most).max(1))
}

/// `Measure` of a surface (`points` in the Poly's space with its faces
/// and edges) against `target`; `clay_points` (the clay's surface points,
/// in its own space) for the coverage.
pub fn measure(points: &[Vec3], face_start: &[u32], corners: &[u32], edges: &[[u32; 2]], target: &Target, clay_points: Option<&[Vec3]>, progress: &Progress) -> Result<Measure, String> {
    // Points, then edge middles, then face middles, picked by index (not
    // gathered first: at the face bound that would be gigabytes).
    let faces = face_start.len() - 1;
    let at = |i: usize| -> Vec3 {
        if i < points.len() {
            points[i]
        } else if i < points.len() + edges.len() {
            let [a, b] = edges[i - points.len()];
            (points[a as usize] + points[b as usize]) * 0.5
        } else {
            let f = i - points.len() - edges.len();
            let face = &corners[face_start[f] as usize..face_start[f + 1] as usize];
            face.iter().map(|&v| points[v as usize]).sum::<Vec3>() / face.len() as f32
        }
    };
    let sample: Vec<Vec3> = picked(points.len() + edges.len() + faces, MEASURE_SAMPLES).map(at).collect();
    let reach = 4.0 * target.cell / target.stretch();
    let mut d = vec![0.0f32; sample.len()];
    par_chunks(&mut d, 256, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let p = sample[i * 256 + k];
            *o = target.near(p, reach)(p).abs() / target.cell;
        }
    });
    progress.check()?;
    let n = d.len().max(1) as f64;
    let mut m = Measure {
        hausdorff_cells: d.iter().copied().fold(0.0, f32::max),
        mean_distance_cells: (d.iter().map(|&x| x as f64).sum::<f64>() / n) as f32,
        rms_cells: (d.iter().map(|&x| (x * x) as f64).sum::<f64>() / n).sqrt() as f32,
        distance_samples: d.len(),
        ..Default::default()
    };
    // Normals: the level's area-weighted normal at each picked point
    // against the field's gradient there, both in the clay's space.
    // Area-weighted normals (`subdiv::frames`' normal, without the rest).
    let mut normals = vec![Vec3::ZERO; points.len()];
    for w in face_start.windows(2) {
        let face = &corners[w[0] as usize..w[1] as usize];
        let n = super::subdiv::newell(points, face);
        for &c in face {
            normals[c as usize] += n;
        }
    }
    let to_clay = target.to_clay;
    let normal_map = Mat3::from_mat4(to_clay).inverse().transpose();
    let at_points: Vec<usize> = picked(points.len(), MEASURE_SAMPLES).collect();
    let mut a = vec![0.0f32; at_points.len()];
    let e = 0.25 * target.cell;
    par_chunks(&mut a, 256, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let v = at_points[i * 256 + k];
            let c = to_clay.transform_point3(points[v]);
            let f = target.surface.near(c, 2.0 * e);
            let g = Vec3::new(f(c + Vec3::X * e) - f(c - Vec3::X * e), f(c + Vec3::Y * e) - f(c - Vec3::Y * e), f(c + Vec3::Z * e) - f(c - Vec3::Z * e));
            let n = (normal_map * normals[v].normalize_or(Vec3::Z)).normalize_or_zero();
            *o = match g.try_normalize() {
                Some(g) if n != Vec3::ZERO => g.dot(n).clamp(-1.0, 1.0).acos().to_degrees(),
                _ => 0.0,
            };
        }
    });
    m.normal_rms_deg = (a.iter().map(|&x| (x * x) as f64).sum::<f64>() / a.len().max(1) as f64).sqrt() as f32;
    progress.check()?;
    let tris = corners.len() - 2 * (face_start.len() - 1);
    if let Some(clay) = clay_points.filter(|_| tris <= COVERAGE_TRIANGLES) {
        let mut t = Vec::with_capacity(tris);
        for w in face_start.windows(2) {
            let face = &corners[w[0] as usize..w[1] as usize];
            for k in 1..face.len() - 1 {
                t.push([face[0], face[k], face[k + 1]]);
            }
        }
        let in_clay: Vec<Vec3> = points.iter().map(|&p| to_clay.transform_point3(p)).collect();
        let field = crate::mesh_asset::MeshField::new(&in_clay, &t);
        progress.check()?;
        let picks: Vec<Vec3> = picked(clay.len(), MEASURE_SAMPLES).map(|i| clay[i]).collect();
        let mut c = vec![0.0f32; picks.len()];
        par_chunks(&mut c, 256, |i, chunk| {
            for (k, o) in chunk.iter_mut().enumerate() {
                *o = field.distance(picks[i * 256 + k], crate::mesh_asset::Sign::Closed).abs() / target.cell;
            }
        });
        m.coverage_cells = Some(c.iter().copied().fold(0.0, f32::max));
    }
    Ok(m)
}

/// `poly` with the projection installed: levels 1..=n hold its
/// displacements, finer levels kept (with no displacement: theirs were
/// relative to the old coarser levels), and the finest projected level
/// shown when it fits.
pub fn install(poly: &Poly, made: &Projection) -> Result<Poly, String> {
    let mut out = poly.clone();
    let n = made.displacement.len() as u8;
    out.set_levels(out.level_count().max(n))?;
    for (k, l) in out.levels.iter_mut().enumerate() {
        l.displacement = made.displacement.get(k).cloned();
    }
    out.level = (0..=n).rev().find(|&l| out.faces_at(l) <= super::MAX_SHOWN_FACES).unwrap_or(0);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests;
