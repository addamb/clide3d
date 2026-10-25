//! The surface band: the grid points of a cell size within a few cells of
//! a clay's surface (blobs plus sculpt detail). The persistent mask and
//! the polygroups (`mask.rs`, `groups.rs`) live on it: their ops (grow,
//! blur, lasso, by_normal, from_blobs ...) read and write band points
//! only, and walk the band through its 6-neighbours, so a grow follows
//! the surface rather than jumping across a thin wall.

use std::collections::HashMap;

use glam::{IVec3, Vec3};

use crate::clay::{Clay, Field, split};

/// Most grid points a band may test (near-surface blocks only).
const MAX_VISIT: f64 = 60_000_000.0;
/// Largest box (in grid points) a band may span.
const MAX_BOX: f64 = 4.0e9;
/// Most points a band may hold.
pub const MAX_POINTS: usize = 16_000_000;

/// The band's half-width at cell `voxel`: every grid corner around a mesh
/// vertex is in it (vertices sit within about half a mesh cell of the
/// field's surface, corners within sqrt(3) cells of a vertex), within the
/// field's clamp (`field`: 4 cells).
pub fn width(clay: &Clay, voxel: f32) -> f32 {
    (2.0 * voxel + 0.5 * clay.cell_size().unwrap_or(voxel).max(voxel)).min(3.5 * voxel)
}

/// The clay's field at cell `voxel` as the band reads it: with a detail
/// grid of that cell (empty if the clay has none), so values are clamped
/// no nearer than 4 cells, outside the band.
pub fn field(clay: &Clay, voxel: f32) -> Field {
    // The composed surface (every sculpt layer).
    let detail = crate::layers::composed_detail(clay);
    if detail.as_ref().is_some_and(|d| d.voxel() >= voxel) {
        return Field::new(clay, voxel);
    }
    let probe = Clay { detail: Some(detail.unwrap_or_else(|| crate::clay::Detail::new(voxel))), sculpt: None, ..clay.clone() };
    Field::new(&probe, voxel)
}

/// Band points, sorted by brick key then position in the brick (the
/// order bricks are hashed in), with the field there.
pub struct Band {
    pub voxel: f32,
    pub width: f32,
    pub keys: Vec<[i32; 3]>,
    /// The field (blobs plus detail) at each point.
    pub t: Vec<f32>,
    index: HashMap<[i32; 3], u32>,
}

impl Band {
    /// The band of `clay` at cell `voxel`.
    pub fn of(clay: &Clay, voxel: f32) -> Result<Band, String> {
        let field = crate::band::field(clay, voxel);
        Band::with(clay, &field, voxel)
    }

    /// `of`, with the clay's field at `voxel` at hand.
    pub fn with(clay: &Clay, field: &Field, voxel: f32) -> Result<Band, String> {
        let width = width(clay, voxel);
        let empty = Band { voxel, width, keys: Vec::new(), t: Vec::new(), index: HashMap::new() };
        if !voxel.is_finite() || voxel <= 0.0 {
            return Err("the clay's cell size is broken".into());
        }
        let Some((lo, hi)) = field.bounds() else { return Ok(empty) };
        // Detail moves the surface off the blobs, which the block test
        // reads: blocks are kept that far nearer.
        let carved = crate::layers::composed_detail(clay).map_or(0.0, |d| d.grid().max_abs());
        let (lo, hi) = (lo - width - voxel, hi + width + voxel);
        let side = 8.0 * voxel;
        let cells = ((hi - lo) / voxel + 2.0).as_dvec3();
        if !cells.is_finite() || cells.x * cells.y * cells.z > MAX_BOX {
            return Err(format!("the clay is too big for a surface band at cell {voxel:.5}; use a larger detail_cell"));
        }
        let (b0, b1) = ((lo / side).floor().as_ivec3(), (hi / side).floor().as_ivec3());
        let mut blocks = Vec::new();
        let mut visit = 0.0f64;
        for z in b0.z..=b1.z {
            for y in b0.y..=b1.y {
                for x in b0.x..=b1.x {
                    let k = IVec3::new(x, y, z);
                    let (a, b) = (k.as_vec3() * side, (k.as_vec3() + 1.0) * side - voxel);
                    let (center, half) = ((a + b) * 0.5, (b - a).length() * 0.5);
                    let list = field.near(a - width - carved, b + width + carved);
                    if list.is_empty() || field.far_side(center, &list, half + width + carved).is_some() {
                        continue;
                    }
                    visit += 512.0;
                    if visit > MAX_VISIT {
                        return Err(format!("the clay's surface is too large for a band at cell {voxel:.5}; use a larger detail_cell"));
                    }
                    blocks.push(([x, y, z], Vec::new()));
                }
            }
        }
        crate::clay::par_chunks(&mut blocks, 1, |_, chunk| {
            for (k, out) in chunk.iter_mut() {
                let a = IVec3::from(*k).as_vec3() * side;
                let list = field.near(a - voxel, a + side + voxel);
                for at in 0..512usize {
                    let l = [at as i32 % 8, at as i32 / 8 % 8, at as i32 / 64];
                    let i = [k[0] * 8 + l[0], k[1] * 8 + l[1], k[2] * 8 + l[2]];
                    let t = field.sample(IVec3::from(i).as_vec3() * voxel, &list).0;
                    if t.abs() <= width {
                        out.push((i, t));
                    }
                }
            }
        });
        let n: usize = blocks.iter().map(|b| b.1.len()).sum();
        if n > MAX_POINTS {
            return Err(format!("the clay's surface band holds {n} points at cell {voxel:.5} (at most {MAX_POINTS}); use a larger detail_cell"));
        }
        let mut band = Band { voxel, width, keys: Vec::with_capacity(n), t: Vec::with_capacity(n), index: HashMap::with_capacity(n) };
        for (_, pts) in blocks {
            for (i, t) in pts {
                band.index.insert(i, band.keys.len() as u32);
                band.keys.push(i);
                band.t.push(t);
            }
        }
        Ok(band)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn find(&self, i: [i32; 3]) -> Option<usize> {
        self.index.get(&i).map(|k| *k as usize)
    }

    /// Point `k` in object space.
    pub fn point(&self, k: usize) -> Vec3 {
        IVec3::from(self.keys[k]).as_vec3() * self.voxel
    }

    /// Band neighbours of point `k` along each axis: [-x, +x, -y, +y, -z, +z].
    pub fn neighbours(&self, k: usize) -> [Option<usize>; 6] {
        let i = self.keys[k];
        let at = |d: [i32; 3]| self.find([i[0] + d[0], i[1] + d[1], i[2] + d[2]]);
        [at([-1, 0, 0]), at([1, 0, 0]), at([0, -1, 0]), at([0, 1, 0]), at([0, 0, -1]), at([0, 0, 1])]
    }

    /// The surface normal at each point (the field's gradient).
    pub fn normals(&self, field: &Field) -> Vec<Vec3> {
        self.per_point(field, |p, list| field.sculpt_normal(p, list, self.voxel * 0.5))
    }

    /// The smart materials' bend at each point (see
    /// `weather::clay_wear_maps`): the field's Laplacian over `radius`,
    /// per unit radius; positive on ridges, negative in cavities.
    pub fn bends(&self, field: &Field, radius: f32) -> Vec<f32> {
        self.per_point(field, |p, list| {
            let d = |q: Vec3| field.surface_distance(q, list);
            let c = d(p);
            let laplacian: f32 = [Vec3::X, Vec3::Y, Vec3::Z].iter().map(|a| (d(p + *a * radius) - c) + (d(p - *a * radius) - c)).sum();
            laplacian / radius
        })
    }

    /// `f(point, prims near its brick)` at every point, in parallel (each
    /// point by one thread: the result never depends on the count).
    pub fn per_point<T: Default + Clone + Send>(&self, field: &Field, f: impl Fn(Vec3, &[u32]) -> T + Sync) -> Vec<T> {
        let mut out = vec![T::default(); self.len()];
        // Runs of points in one brick share a prim list.
        let reach = self.width + 4.0 * self.voxel;
        crate::clay::par_chunks(&mut out, 512, |chunk, values| {
            let mut at: Option<([i32; 3], Vec<u32>)> = None;
            for (j, v) in values.iter_mut().enumerate() {
                let k = chunk * 512 + j;
                let key = split(self.keys[k]).0;
                if at.as_ref().is_none_or(|a| a.0 != key) {
                    let lo = IVec3::from(key).as_vec3() * 8.0 * self.voxel;
                    at = Some((key, field.near(lo - reach, lo + 8.0 * self.voxel + reach)));
                }
                *v = f(self.point(k), &at.as_ref().expect("set").1);
            }
        });
        out
    }

    /// The band points joined to `seeds` through band neighbours (within
    /// `within` of `center` when given), as a flag per point.
    pub fn flood(&self, seeds: &[usize], within: Option<(Vec3, f32)>) -> Vec<bool> {
        let mut seen = vec![false; self.len()];
        let mut stack: Vec<usize> = Vec::new();
        let inside = |k: usize| within.is_none_or(|(c, r)| self.point(k).distance(c) <= r);
        for &s in seeds {
            if s < self.len() && !seen[s] && inside(s) {
                seen[s] = true;
                stack.push(s);
            }
        }
        while let Some(k) = stack.pop() {
            for n in self.neighbours(k).into_iter().flatten() {
                if !seen[n] && inside(n) {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        seen
    }

    /// The band point nearest the surface near `p` (within `reach`).
    pub fn nearest(&self, p: Vec3, reach: f32) -> Option<usize> {
        if !p.is_finite() || !reach.is_finite() {
            return None;
        }
        let r = (reach / self.voxel).ceil().clamp(1.0, 64.0) as i32;
        let c = (p / self.voxel).round();
        // Off the grid's range: no band point there.
        if c.abs().max_element() > (1 << 28) as f32 {
            return None;
        }
        let c = c.as_ivec3();
        let mut best: Option<(f32, usize)> = None;
        for z in -r..=r {
            for y in -r..=r {
                for x in -r..=r {
                    if let Some(k) = self.find((c + IVec3::new(x, y, z)).to_array()) {
                        // Nearer the surface first, then nearer p.
                        let score = self.t[k].abs() + self.point(k).distance(p);
                        if best.is_none_or(|b| score < b.0 || (score == b.0 && k < b.1)) {
                            best = Some((score, k));
                        }
                    }
                }
            }
        }
        best.map(|b| b.1)
    }
}
