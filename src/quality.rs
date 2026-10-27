//! Surface quality measured on the clay field (docs/QUALITY_PLAN.md
//! Stage 2, D12): lumpiness, curvature and wall thickness, at points
//! spread evenly over the regular mesh and moved onto the field. The
//! numbers depend on neither the camera nor the tessellation; the unit is
//! the clay's cell (its finest sculpt cell where it has detail).
//!
//! **Lumpiness** at a point is the band-pass residual of the surface: how
//! far the surface there lies from a smooth fit through the surface around
//! it (heights along the normal at 24 offsets within σ = 4 cells, a
//! Gaussian-weighted quadric), in cells. Where the fit itself bends hard
//! (a small ball, a thin part) its scale shrinks so the fit can follow the
//! intended shape. Where the surface is a line at that scale (a groove, a
//! ridge, a thin tube, an edge: one strong principal curvature) the fit runs
//! along the line only, so the line's own section is kept and lumps along
//! it still count. Points in the footprint of a texture stroke (noise,
//! crease, pinch, VDM, any alpha) or on a sharp edge are intended detail and
//! left out. Lumpiness is the RMS over the rest.
//!
//! Every point is a pure function of the field and its seed, measured in
//! parallel into its own slot, and sums run in point order: the results
//! are the same on any number of threads (§0.2 rule 4).

use std::collections::HashMap;

use glam::Vec3;

use crate::brush::spec::Brush;
use crate::clay::{Clay, Field, par_chunks};
use crate::ledger::RecordOp;
use crate::mesh::Mesh;

/// Surface samples a report takes by default, and the range it accepts.
pub const DEFAULT_SAMPLES: usize = 40_000;
pub const MIN_SAMPLES: usize = 1_000;
pub const MAX_SAMPLES: usize = 200_000;
/// The smooth fit's scale, in cells.
pub const SIGMA: f32 = 4.0;
/// Where the fit's curvature times its scale passes this, the scale
/// shrinks to keep it there (no smaller than `MIN_SIGMA` cells).
const BEND: f32 = 0.5;
const MIN_SIGMA: f32 = 1.5;
/// A line at the fit's scale: anisotropy at least this, and the larger
/// curvature times σ at least `LINE_BEND` (rule (b) of the plan).
const LINE_ANISOTROPY: f32 = 0.6;
const LINE_BEND: f32 = 0.15;
/// An edge: the normal turns by more than this between points half a cell
/// apart, and by most of that between points a quarter cell apart (a kink,
/// not a steep lump, whose turn halves with the spacing).
const EDGE_DEGREES: f32 = 30.0;
/// Worst points listed, and how far apart (in σ) they are kept.
pub const WORST: usize = 8;
const WORST_APART: f32 = 4.0;

/// One measured surface point, in the clay's space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// On the surface, and its outward normal.
    pub p: Vec3,
    pub n: Vec3,
    /// The unit (world size of a cell there).
    pub cell: f32,
    /// Surface height over the smooth fit there, in cells.
    pub residual: f32,
    /// Mean curvature at the cell's scale, per cell (convex +).
    pub h: f32,
    /// The smooth fit's principal curvatures per cell, |k1| >= |k2|.
    pub k1: f32,
    pub k2: f32,
    /// The fit's scale, in cells.
    pub sigma: f32,
    /// Measured along a line (rule b).
    pub line: bool,
    /// On a sharp edge (rule c): intended.
    pub edge: bool,
    /// In a texture stroke's footprint (rule a): intended.
    pub textured: bool,
}

impl Point {
    /// Intended detail: left out of lumpiness.
    pub fn intended(&self) -> bool {
        self.edge || self.textured
    }
}

/// splitmix64: the samples' seeded random numbers.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// 0..1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// About `n` points spread evenly over a mesh's surface, with its
/// interpolated normals: area-weighted random points (seeded), thinned
/// to a Poisson disc in the order drawn. Sequential, so the same mesh
/// always gives the same points.
pub fn seeds(mesh: &Mesh, n: usize) -> Vec<(Vec3, Vec3)> {
    let tris: Vec<[usize; 3]> = mesh.indices.chunks_exact(3).map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).filter(|t| t.iter().all(|&i| i < mesh.vertices.len())).collect();
    let pos = |i: usize| Vec3::from(mesh.vertices[i].pos);
    let mut prefix = Vec::with_capacity(tris.len());
    let mut area = 0.0f64;
    for t in &tris {
        area += 0.5 * (pos(t[1]) - pos(t[0])).cross(pos(t[2]) - pos(t[0])).length() as f64;
        prefix.push(area);
    }
    if n == 0 || area <= 0.0 || !area.is_finite() {
        return Vec::new();
    }
    // A disc radius that leaves room for n points in random packing.
    let r = (0.5 * (area / n as f64).sqrt()) as f32;
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    let key = |p: Vec3| (p / r).floor().as_ivec3().to_array();
    let mut out: Vec<(Vec3, Vec3)> = Vec::with_capacity(n);
    let mut rng = Rng(0x51ab_1e5e_ed00_0001 ^ n as u64);
    for _ in 0..n * 4 {
        if out.len() == n {
            break;
        }
        let at = rng.unit() * area;
        let t = tris[prefix.partition_point(|&a| a < at).min(tris.len() - 1)];
        let (mut u, mut v) = (rng.unit() as f32, rng.unit() as f32);
        if u + v > 1.0 {
            (u, v) = (1.0 - u, 1.0 - v);
        }
        let w = 1.0 - u - v;
        let p = pos(t[0]) * w + pos(t[1]) * u + pos(t[2]) * v;
        let normal = |i: usize| Vec3::from(mesh.vertices[i].normal);
        let nrm = (normal(t[0]) * w + normal(t[1]) * u + normal(t[2]) * v).normalize_or_zero();
        let k = key(p);
        let crowded = (-1..=1).any(|x| (-1..=1).any(|y| (-1..=1).any(|z| grid.get(&[k[0] + x, k[1] + y, k[2] + z]).is_some_and(|l| l.iter().any(|&j| out[j as usize].0.distance_squared(p) < r * r)))));
        if crowded {
            continue;
        }
        grid.entry(k).or_default().push(out.len() as u32);
        out.push((p, nrm));
    }
    out
}

/// The field's gradient by central differences at spacing `h`.
fn gradient(f: &dyn Fn(Vec3) -> f32, p: Vec3, h: f32) -> Vec3 {
    let d = |a: Vec3| f(p + a * h) - f(p - a * h);
    Vec3::new(d(Vec3::X), d(Vec3::Y), d(Vec3::Z)) / (2.0 * h)
}

/// The surface's height over `q` along `n` (signed, at most `reach` either
/// way), to `tol`: secant steps from the distance guess, else a bracketing
/// walk (`adaptive::along`). None when the line meets no surface.
fn height(f: &dyn Fn(Vec3) -> f32, q: Vec3, n: Vec3, reach: f32, tol: f32) -> Option<f32> {
    let v = f(q);
    if v.abs() <= tol {
        return Some(0.0);
    }
    let (mut t0, mut v0) = (0.0f32, v);
    let mut t1 = (-v).clamp(-reach, reach);
    let mut v1 = f(q + n * t1);
    for _ in 0..10 {
        if v1.abs() <= tol {
            return Some(t1);
        }
        let d = v1 - v0;
        if d == 0.0 {
            break;
        }
        let t2 = t1 - v1 * (t1 - t0) / d;
        if !t2.is_finite() || t2.abs() > reach {
            break;
        }
        (t0, v0, t1) = (t1, v1, t2);
        v1 = f(q + n * t1);
    }
    let hit = crate::adaptive::along(f, q, n, tol, reach);
    (hit != q).then(|| (hit - q).dot(n))
}

/// Solves the n x n system `a x = b` (row-major, Gaussian elimination with
/// partial pivoting); None when singular.
fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    for c in 0..N {
        let pivot = (c..N).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[pivot][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, pivot);
        b.swap(c, pivot);
        for r in c + 1..N {
            let k = a[r][c] / a[c][c];
            let (pivot_rows, rows) = a.split_at_mut(r);
            for (x, y) in rows[0][c..].iter_mut().zip(&pivot_rows[c][c..]) {
                *x -= k * y;
            }
            b[r] -= k * b[c];
        }
    }
    let mut x = [0.0; N];
    for c in (0..N).rev() {
        let s: f64 = (c + 1..N).map(|j| a[c][j] * x[j]).sum();
        x[c] = (b[c] - s) / a[c][c];
    }
    Some(x)
}

/// A smooth fit: the height at the point, and the surface's principal
/// curvatures (1/world, convex +) with the direction of the smaller.
struct Fit {
    c0: f32,
    k1: f32,
    k2: f32,
    along: Vec3,
}

/// The weighted quadric h = c0 + c1 x + c2 y + c3 x^2 + c4 xy + c5 y^2
/// (x, y in units of `sigma` on the tangent frame u, w) through the
/// surface heights at 24 offsets on three rings within `sigma`.
fn fit_2d(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, u: Vec3, w: Vec3, sigma: f32, tol: f32) -> Option<Fit> {
    let (mut a, mut b) = ([[0.0f64; 6]; 6], [0.0f64; 6]);
    let mut hits = 0;
    for ring in 1..=3 {
        let r = ring as f32 / 3.0;
        for j in 0..8 {
            let angle = (j as f32 + 0.5 * (ring % 2) as f32) * std::f32::consts::FRAC_PI_4;
            let (x, y) = (r * angle.cos(), r * angle.sin());
            let Some(h) = height(f, p + (u * x + w * y) * sigma, n, sigma, tol) else { continue };
            hits += 1;
            let weight = (-2.0 * r * r).exp() as f64;
            let row = [1.0, x as f64, y as f64, (x * x) as f64, (x * y) as f64, (y * y) as f64];
            for i in 0..6 {
                for k in 0..6 {
                    a[i][k] += weight * row[i] * row[k];
                }
                b[i] += weight * row[i] * h as f64;
            }
        }
    }
    if hits < 12 {
        return None;
    }
    let c = solve(a, b)?;
    // The Hessian on the tangent plane (1/world); curvature is its negative
    // (heights fall away from a convex surface).
    let s2 = (sigma * sigma) as f64;
    let (hxx, hxy, hyy) = (2.0 * c[3] / s2, c[4] / s2, 2.0 * c[5] / s2);
    let (mid, half) = ((hxx + hyy) * 0.5, (((hxx - hyy) * 0.5).powi(2) + hxy * hxy).sqrt());
    let (e1, e2) = (mid + half, mid - half);
    let (big, small) = if e1.abs() >= e2.abs() { (e1, e2) } else { (e2, e1) };
    // The smaller curvature's direction: the eigenvector of `small`.
    let (dx, dy) = if hxy.abs() > 1e-12 { (small - hyy, hxy) } else if (hxx - small).abs() < (hyy - small).abs() { (1.0, 0.0) } else { (0.0, 1.0) };
    let along = (u * dx as f32 + w * dy as f32).normalize_or(u);
    Some(Fit { c0: c[0] as f32, k1: -big as f32, k2: -small as f32, along })
}

/// The weighted parabola h = c0 + c1 s + c2 s^2 through the surface
/// heights at 8 offsets along `along`, within `sigma` either way.
fn fit_line(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, along: Vec3, sigma: f32, tol: f32) -> Option<f32> {
    let (mut a, mut b) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
    let mut hits = 0;
    for j in 1..=4 {
        for side in [-1.0f32, 1.0] {
            let s = side * j as f32 / 4.0;
            let Some(h) = height(f, p + along * (s * sigma), n, sigma, tol) else { continue };
            hits += 1;
            let weight = (-2.0 * s * s).exp() as f64;
            let row = [1.0, s as f64, (s * s) as f64];
            for i in 0..3 {
                for k in 0..3 {
                    a[i][k] += weight * row[i] * row[k];
                }
                b[i] += weight * row[i] * h as f64;
            }
        }
    }
    if hits < 6 {
        return None;
    }
    solve(a, b).map(|c| c[0] as f32)
}

/// The largest angle (radians) between the normal at `p` and the normals
/// `h` away along the six axes.
fn turn(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, h: f32) -> f32 {
    let mut most = 0.0f32;
    for a in [Vec3::X, Vec3::Y, Vec3::Z] {
        for s in [h, -h] {
            let m = gradient(f, p + a * s, h * 0.5).normalize_or_zero();
            most = most.max(n.dot(m).clamp(-1.0, 1.0).acos());
        }
    }
    most
}

/// Mean curvature per cell at `p` (convex +), from the field at spacing
/// `cell` (the curvature flow's own stencil, `flow::curvature`).
pub fn mean_curvature(f: &dyn Fn(Vec3) -> f32, p: Vec3, cell: f32) -> f32 {
    crate::sculpt::flow::curvature(|x, y, z| f(p + Vec3::new(x as f32, y as f32, z as f32) * cell) / cell).map_or(0.0, |c| c * 0.5)
}

/// Measures the surface near `p` (a point near it, `n` about its normal):
/// None when no surface is within two cells along `n`, or the fit has too
/// few points.
pub fn measure(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, cell: f32) -> Option<Point> {
    let tol = 1e-4 * cell;
    let p0 = crate::adaptive::along(f, p, n.normalize_or_zero(), tol, 2.0 * cell);
    if f(p0).abs() > 0.05 * cell {
        return None;
    }
    let n0 = gradient(f, p0, 0.25 * cell).try_normalize()?;
    let (u, w) = n0.any_orthonormal_pair();
    let sigma0 = SIGMA * cell;
    let first = fit_2d(f, p0, n0, u, w, sigma0, tol)?;
    let bend = first.k1.abs();
    let anisotropy = (first.k1.abs() - first.k2.abs()) / (first.k1.abs() + first.k2.abs() + 1e-12);
    let line = anisotropy >= LINE_ANISOTROPY && bend * sigma0 >= LINE_BEND;
    let (c0, sigma) = if line {
        (fit_line(f, p0, n0, first.along, sigma0, tol)?, sigma0)
    } else if bend * sigma0 > BEND {
        let sigma = (BEND / bend).clamp(MIN_SIGMA * cell, sigma0);
        (fit_2d(f, p0, n0, u, w, sigma, tol)?.c0, sigma)
    } else {
        (first.c0, sigma0)
    };
    let half = turn(f, p0, n0, 0.5 * cell);
    let edge = half > EDGE_DEGREES.to_radians() && turn(f, p0, n0, 0.25 * cell) > 0.7 * half;
    Some(Point { p: p0, n: n0, cell, residual: -c0 / cell, h: mean_curvature(f, p0, cell), k1: first.k1 * cell, k2: first.k2 * cell, sigma: sigma / cell, line, edge, textured: false })
}

/// The footprints of a clay's texture strokes (rule a): every dab of an
/// enabled stroke whose brush makes texture (noise, crease, pinch, VDM, or
/// any brush with an alpha), on the base layer and every enabled sculpt
/// layer, as capsules of the dab's radius along its points.
pub struct Footprints {
    size: f32,
    segments: Vec<(Vec3, Vec3, f32)>,
    grid: HashMap<[i32; 3], Vec<u32>>,
}

/// Whether strokes of these settings make texture (intended detail).
fn makes_texture(params: &crate::brush::resolve::Params) -> bool {
    matches!(params.brush, Brush::Noise | Brush::Crease | Brush::Pinch | Brush::Vdm) || params.alpha.is_some()
}

impl Footprints {
    pub fn of(clay: &Clay) -> Footprints {
        let mut segments = Vec::new();
        if let Some(stack) = &clay.sculpt {
            let data = stack.data();
            let layers = data.layers.iter().filter(|l| l.enabled && l.kind == crate::layers::Kind::Sculpt && l.amount != 0.0).map(|l| &l.strokes);
            for ledger in std::iter::once(&data.base).chain(layers) {
                for r in ledger.records().iter().filter(|r| r.enabled && r.factor != 0.0) {
                    let RecordOp::Stroke { stroke, .. } = &r.op else { continue };
                    if !makes_texture(&stroke.params) {
                        continue;
                    }
                    for dab in &stroke.dabs {
                        let r = dab.radius.max(0.0);
                        match dab.points.as_slice() {
                            [] => {}
                            [p] => segments.push((*p, *p, r)),
                            points => segments.extend(points.windows(2).map(|w| (w[0], w[1], r))),
                        }
                    }
                }
            }
        }
        segments.retain(|s| s.0.is_finite() && s.1.is_finite() && s.2.is_finite());
        let size = segments.iter().map(|s| s.2).fold(0.0f32, f32::max).max(1e-6) * 2.0;
        let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (i, &(a, b, r)) in segments.iter().enumerate() {
            // Bucketed out to 2 r: `contains` adds a margin (a cell) to r.
            let lo = ((a.min(b) - 2.0 * r) / size).floor().as_ivec3();
            let hi = ((a.max(b) + 2.0 * r) / size).floor().as_ivec3();
            // A dab segment is short (dabs are spaced under their radius);
            // a long one is bucketed by its ends' cells only.
            if (hi - lo).max_element() > 64 {
                for p in [a, b] {
                    grid.entry((p / size).floor().as_ivec3().to_array()).or_default().push(i as u32);
                }
                continue;
            }
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        grid.entry([x, y, z]).or_default().push(i as u32);
                    }
                }
            }
        }
        Footprints { size, segments, grid }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Whether `p` lies within a dab's radius plus `margin` (a margin up
    /// to the radius is found everywhere).
    pub fn contains(&self, p: Vec3, margin: f32) -> bool {
        let Some(list) = self.grid.get(&(p / self.size).floor().as_ivec3().to_array()) else { return false };
        list.iter().any(|&i| {
            let (a, b, r) = self.segments[i as usize];
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_squared().max(1e-30)).clamp(0.0, 1.0);
            p.distance(a + ab * t) <= r + margin
        })
    }
}

/// A clay's field and unit, ready to measure.
pub struct Probe {
    field: Field,
    /// The cell where there is no sculpt detail.
    fallback: f32,
    /// Each field prim's blob (a mirrored blob makes two prims).
    blob_of: Vec<usize>,
}

impl Probe {
    /// None for an empty clay.
    pub fn new(clay: &Clay) -> Option<Probe> {
        let fallback = clay.detail_cell.filter(|c| c.is_finite() && *c > 0.0).or_else(|| clay.cell_size())?;
        let blob_of = clay.prim_blobs();
        Some(Probe { field: Field::new(clay, fallback), fallback, blob_of })
    }

    /// The unit at `p`: the finest sculpt cell counting there, else the
    /// clay's detail cell, else its mesh cell.
    pub fn cell(&self, p: Vec3) -> f32 {
        self.field.detail().map_or(self.fallback, |d| d.grid().cell_at(p))
    }

    /// The field near `p` (within `reach` of it, exact), as a closure.
    pub fn near(&self, p: Vec3, reach: f32) -> impl Fn(Vec3) -> f32 + '_ {
        let m = Vec3::splat(reach + self.field.limit);
        let list = self.field.near(p - m, p + m);
        move |q| self.field.sample(q, &list).0
    }

    /// Which blob (its index in `Clay::blobs`) makes the surface at `p`,
    /// as masks see ownership.
    pub fn owner(&self, p: Vec3) -> Option<usize> {
        self.field.owner_index(p).and_then(|i| self.blob_of.get(i).copied())
    }

    /// Measures the surface at each seed, in parallel, each into its slot.
    pub fn measure(&self, seeds: &[(Vec3, Vec3)]) -> Vec<Option<Point>> {
        let mut out = vec![None; seeds.len()];
        const CHUNK: usize = 64;
        par_chunks(&mut out, CHUNK, |c, slots| {
            for (j, slot) in slots.iter_mut().enumerate() {
                let (p, n) = seeds[c * CHUNK + j];
                let cell = self.cell(p);
                let f = self.near(p, (SIGMA + 3.0) * cell);
                *slot = measure(&f, p, n, cell);
            }
        });
        out
    }

    /// Wall thickness inward from each point, up to `reach` (None where it
    /// is thicker), in parallel.
    pub fn walls(&self, points: &[Point], reach: f32) -> Vec<Option<f32>> {
        let mut out = vec![None; points.len()];
        const CHUNK: usize = 64;
        par_chunks(&mut out, CHUNK, |c, slots| {
            for (j, slot) in slots.iter_mut().enumerate() {
                let pt = &points[c * CHUNK + j];
                let f = self.near(pt.p, reach + pt.cell);
                *slot = wall(&f, pt.p, pt.n, pt.cell, reach);
            }
        });
        out
    }

    /// Mean curvature per cell at each point (shading), in parallel.
    pub fn curvatures(&self, points: &[Vec3]) -> Vec<f32> {
        let mut out = vec![0.0; points.len()];
        const CHUNK: usize = 256;
        par_chunks(&mut out, CHUNK, |c, slots| {
            let mine = &points[c * CHUNK..(c * CHUNK + slots.len())];
            let (lo, hi) = mine.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
            let cell = self.cell((lo + hi) * 0.5);
            let m = Vec3::splat(2.0 * cell + self.field.limit);
            let list = self.field.near(lo - m, hi + m);
            let f = |q: Vec3| self.field.sample(q, &list).0;
            for (slot, p) in slots.iter_mut().zip(mine) {
                *slot = mean_curvature(&f, *p, self.cell(*p));
            }
        });
        out
    }
}

/// How thick the solid is behind surface point `p` (normal `n`): the
/// distance inward to where the field turns positive again, up to `reach`.
pub fn wall(f: &dyn Fn(Vec3) -> f32, p: Vec3, n: Vec3, cell: f32, reach: f32) -> Option<f32> {
    let mut t = 0.05 * cell;
    let mut last = 0.0;
    while t <= reach {
        let v = f(p - n * t);
        if v > 0.0 {
            let (mut lo, mut hi) = (last, t);
            for _ in 0..24 {
                let m = (lo + hi) * 0.5;
                if f(p - n * m) > 0.0 { hi = m } else { lo = m }
            }
            return Some((lo + hi) * 0.5);
        }
        last = t;
        t += (-v).max(0.25 * cell);
    }
    None
}

/// Measures a clay at about `samples` points spread over its regular mesh
/// (`rest`, in the clay's space), with texture footprints marked.
pub fn sample_clay(clay: &Clay, rest: &Mesh, samples: usize) -> Option<(Probe, Vec<Point>, usize)> {
    let probe = Probe::new(clay)?;
    let seeds = seeds(rest, samples);
    let measured = probe.measure(&seeds);
    let unfit = measured.iter().filter(|p| p.is_none()).count();
    let mut points: Vec<Point> = measured.into_iter().flatten().collect();
    let prints = Footprints::of(clay);
    if !prints.is_empty() {
        for p in &mut points {
            p.textured = prints.contains(p.p, p.cell);
        }
    }
    Some((probe, points, unfit))
}

/// A named part of the clay (its space) scored on its own, or left out.
#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub center: Vec3,
    pub radius: f32,
    pub exclude: bool,
}

impl Region {
    fn contains(&self, p: Vec3) -> bool {
        p.distance_squared(self.center) <= self.radius * self.radius
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionScore {
    pub name: String,
    /// None when the region holds no counted point.
    pub score: Option<f32>,
    pub worst_at: Option<Vec3>,
    pub points: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lumpiness {
    /// RMS residual (cells) over points that are not intended detail and
    /// not in an excluded region; None without any.
    pub overall: Option<f32>,
    /// The same over every measured point (intended detail too).
    pub raw: Option<f32>,
    pub counted: usize,
    pub intended: usize,
    pub lines: usize,
    pub regions: Vec<RegionScore>,
    /// The largest residuals, at least 4 σ apart: (point, residual cells).
    pub worst: Vec<(Vec3, f32)>,
}

fn rms(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for v in values {
        sum += (v as f64) * (v as f64);
        n += 1;
    }
    (n > 0).then(|| (sum / n as f64).sqrt() as f32)
}

/// Lumpiness of measured points (in their order, so the same points give
/// the same numbers).
pub fn lumpiness(points: &[Point], regions: &[Region]) -> Lumpiness {
    let excluded = |p: &Point| regions.iter().any(|r| r.exclude && r.contains(p.p));
    let counted: Vec<&Point> = points.iter().filter(|p| !p.intended() && !excluded(p)).collect();
    let scores = regions
        .iter()
        .filter(|r| !r.exclude)
        .map(|r| {
            let inside: Vec<&&Point> = counted.iter().filter(|p| r.contains(p.p)).collect();
            let worst = inside.iter().max_by(|a, b| a.residual.abs().total_cmp(&b.residual.abs()));
            RegionScore { name: r.name.clone(), score: rms(inside.iter().map(|p| p.residual)), worst_at: worst.map(|p| p.p), points: inside.len() }
        })
        .collect();
    // The worst, apart: largest first (ties in point order).
    let mut order: Vec<usize> = (0..counted.len()).collect();
    order.sort_by(|&a, &b| counted[b].residual.abs().total_cmp(&counted[a].residual.abs()).then(a.cmp(&b)));
    let mut worst: Vec<(Vec3, f32)> = Vec::new();
    for i in order {
        if worst.len() == WORST {
            break;
        }
        let p = counted[i];
        let apart = WORST_APART * p.sigma.max(SIGMA) * p.cell;
        if worst.iter().all(|w| w.0.distance(p.p) >= apart) {
            worst.push((p.p, p.residual));
        }
    }
    Lumpiness {
        overall: rms(counted.iter().map(|p| p.residual)),
        raw: rms(points.iter().map(|p| p.residual)),
        counted: counted.len(),
        intended: points.iter().filter(|p| p.intended()).count(),
        lines: points.iter().filter(|p| p.line).count(),
        regions: scores,
        worst,
    }
}

/// The 95th percentile of |mean curvature| per cell over the points.
pub fn p95_abs_h(points: &[Point]) -> Option<f32> {
    let mut h: Vec<f32> = points.iter().map(|p| p.h.abs()).filter(|h| h.is_finite()).collect();
    if h.is_empty() {
        return None;
    }
    h.sort_by(f32::total_cmp);
    Some(h[((h.len() - 1) as f32 * 0.95).round() as usize])
}

/// What a quality shade colors the surface by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Shade {
    /// The usual look.
    #[default]
    Beauty,
    Curvature,
    Lumpiness,
    Normals,
    /// Mesh edges over the usual look.
    Wire,
}

impl Shade {
    pub fn parse(s: &str) -> Result<Shade, String> {
        Ok(match s {
            "beauty" => Shade::Beauty,
            "curvature" => Shade::Curvature,
            "lumpiness" => Shade::Lumpiness,
            "normals" => Shade::Normals,
            "wire" => Shade::Wire,
            other => return Err(format!("unknown shade {other}; use beauty, curvature, lumpiness, normals or wire")),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Shade::Beauty => "beauty",
            Shade::Curvature => "curvature",
            Shade::Lumpiness => "lumpiness",
            Shade::Normals => "normals",
            Shade::Wire => "wire",
        }
    }

    /// Whether it recolors the surface (else it draws as beauty).
    pub fn colors(self) -> bool {
        matches!(self, Shade::Curvature | Shade::Lumpiness | Shade::Normals)
    }
}

/// Curvature's ramp is clamped at this mean curvature per cell.
pub const CURVATURE_RAMP: f32 = 0.3;
/// Lumpiness's ramp is full at this residual (cells).
pub const LUMPINESS_RAMP: f32 = 0.2;
const WHITE: Vec3 = Vec3::new(0.95, 0.95, 0.95);
const BLUE: Vec3 = Vec3::new(0.12, 0.32, 0.95);
const RED: Vec3 = Vec3::new(0.95, 0.18, 0.12);
const MAGENTA: Vec3 = Vec3::new(0.88, 0.08, 0.78);
const GREY: Vec3 = Vec3::new(0.5, 0.5, 0.5);

/// Diverging blue (concave) - white - red (convex).
pub fn curvature_color(h: f32) -> Vec3 {
    let t = (h / CURVATURE_RAMP).clamp(-1.0, 1.0);
    if t < 0.0 { WHITE.lerp(BLUE, -t) } else { WHITE.lerp(RED, t) }
}

/// White (smooth) to magenta (lumpy); intended detail grey.
pub fn lumpiness_color(residual: f32, intended: bool) -> Vec3 {
    if intended { GREY } else { WHITE.lerp(MAGENTA, (residual.abs() / LUMPINESS_RAMP).clamp(0.0, 1.0)) }
}

/// Per vertex colors of a clay's mesh (`mesh`, in the clay's space) for
/// a shade; None for shades that keep the mesh's own colors, or a clay
/// with nothing to measure. Lumpiness is measured at `samples` points and
/// spread to the vertices (each takes its nearest points' mean).
pub fn shade_colors(shade: Shade, clay: &Clay, mesh: &Mesh, samples: usize) -> Option<Vec<[f32; 3]>> {
    let at: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    match shade {
        Shade::Normals => Some(mesh.vertices.iter().map(|v| (Vec3::from(v.normal).normalize_or_zero() * 0.5 + 0.5).to_array()).collect()),
        Shade::Curvature => {
            let probe = Probe::new(clay)?;
            Some(probe.curvatures(&at).into_iter().map(|h| curvature_color(h).to_array()).collect())
        }
        Shade::Lumpiness => {
            let (_, points, _) = sample_clay(clay, mesh, samples)?;
            Some(lumpiness_colors(&points, &at))
        }
        Shade::Beauty | Shade::Wire => None,
    }
}

/// Lumpiness colors at `at` from measured points (see `splat`).
pub fn lumpiness_colors(points: &[Point], at: &[Vec3]) -> Vec<[f32; 3]> {
    splat(points, at).into_iter().map(|(r, intended)| lumpiness_color(r, intended).to_array()).collect()
}

/// Each target's value from the measured points: the mean |residual| of
/// the points within 1.5 spacings (the nearest one's where none), and
/// whether the nearest point is intended detail.
fn splat(points: &[Point], at: &[Vec3]) -> Vec<(f32, bool)> {
    if points.is_empty() {
        return vec![(0.0, false); at.len()];
    }
    // The mean spacing of the points, from their spread.
    let (lo, hi) = points.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p.p), hi.max(p.p)));
    let extent = (hi - lo).max_element().max(1e-6);
    let spacing = points.iter().map(|p| p.cell * SIGMA).fold(0.0f32, f32::max).max(extent / (points.len() as f32).sqrt());
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, p) in points.iter().enumerate() {
        grid.entry((p.p / spacing).floor().as_ivec3().to_array()).or_default().push(i as u32);
    }
    let mut out = vec![(0.0, false); at.len()];
    const CHUNK: usize = 1024;
    par_chunks(&mut out, CHUNK, |c, slots| {
        for (j, slot) in slots.iter_mut().enumerate() {
            let q = at[c * CHUNK + j];
            let k = (q / spacing).floor().as_ivec3();
            let (mut sum, mut n, mut best) = (0.0f32, 0usize, (f32::MAX, 0usize));
            for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        for &i in grid.get(&[k.x + x, k.y + y, k.z + z]).into_iter().flatten() {
                            let p = &points[i as usize];
                            let d = p.p.distance(q);
                            if d < best.0 {
                                best = (d, i as usize);
                            }
                            if d <= 0.75 * spacing && !p.intended() {
                                sum += p.residual.abs();
                                n += 1;
                            }
                        }
                    }
                }
            }
            let near = &points[best.1];
            *slot = if best.0 == f32::MAX { (0.0, false) } else if near.intended() { (0.0, true) } else if n > 0 { (sum / n as f32, false) } else { (near.residual.abs(), false) };
        }
    });
    out
}

#[cfg(test)]
mod tests;
