//! Anchored, chunked surface nets (docs/SCULPT_PLAN.md Stage 6): the mesh
//! of a clay under sculpt, in chunks of `CHUNK`^3 cells that rebuild on
//! their own, so a dab remeshes only the chunks it touched.
//!
//! The lattice is anchored at the origin: point `i` sits at `i * voxel`
//! (the detail grid's points when the clay has one, so a lattice value is
//! exactly the blob field plus the grid's own offset there, what brushes
//! read), whatever the clay's bounds. Nothing is clipped. Rules that make
//! chunks agree bit for bit, with no welding:
//! - Each grid edge belongs to the chunk holding the cell at its low end
//!   (the cell whose low corner is the edge's start); a chunk meshes the
//!   edges it owns, and works out the vertices of the cells around them,
//!   one cell into its low neighbours (its halo).
//! - Every vertex is a function of its cell's lattice values and of field
//!   samples at fixed places in the cell, worked out the same way by every
//!   chunk that needs it: a seam vertex is the same f32s on both sides.
//! - A cell crossed by more than one sheet (a saddle) gets one vertex per
//!   surface piece (`adaptive::pieces`, a rule both cells beside a face
//!   agree on), so ambiguous edges resolve locally and the surface stays
//!   manifold; there is no whole-mesh fallback.
//! - Normals come from the field's gradient: central differences on the
//!   lattice, blended across the cell (sharp, bent cells: the field's own
//!   gradient at the vertex). Sharp-feature vertices follow the same rule
//!   as `Clay::build_window_cells`, cell by cell.
//! - Detail regions do not refine chunks: their finer offsets are read at
//!   the lattice's cell (every chunk at the coarse factor), so no chunk
//!   border ever meets a finer one. The fine mesh still shows them.
//!
//! `Mesher::mesh_range` meshes any box of cells, so the monolithic
//! anchored reference is the same kernel over one range covering the clay.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use glam::{IVec3, Vec3};

use super::base_cache::{self, BaseCache, KeyMap};
use super::{CORNERS, Clay, Detail, Field, hard_surface, par_chunks};
use crate::adaptive::{OTHER, cube_edge, pieces};
use crate::material::Material;
#[cfg(test)]
use crate::mesh::{Mesh, Vertex};
#[cfg(test)]
use crate::paint::Paint;

/// Cells per chunk side.
pub const CHUNK: i32 = 32;
/// Most chunks a clay's box may span (else it is drawn whole).
pub const MAX_CHUNKS: usize = 400_000;
/// Most cells across a chunked clay (as fine meshes, `MAX_FINE_CELLS`).
pub const MAX_CELLS: f32 = super::MAX_FINE_CELLS as f32;
/// A brick whose middle is surely this many cells from the blob surface
/// holds one value: its points are then 10 - 6.1 (half its diagonal)
/// cells away or more, past every point a vertex reads (a crossed cell's
/// corners and one point beyond, under 2.8 cells from the surface).
const FAR_CELLS: f32 = 10.0;
/// Largest lattice coordinate (cells), far from i32 overflow.
const MAX_COORD: f32 = (1 << 26) as f32;

pub type ChunkKey = [i32; 3];

/// One chunk's surface: positions and normals in the clay's space, the
/// blob color (sRGB, before paint) and material under each vertex, and
/// triangles.
#[derive(Default, Debug)]
pub struct ChunkMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub base: Vec<Vec3>,
    pub mats: Vec<u16>,
    /// How far each vertex lies in a crease (0..255), from the lattice's
    /// Laplacian (the slim vertex's cavity; matcap shading darkens it).
    pub cavity: Vec<u8>,
    pub indices: Vec<u32>,
}

/// Why a clay cannot be chunked (it is drawn whole as before).
pub fn eligible(clay: &Clay, palette: &[Material]) -> Result<(), &'static str> {
    if !clay.decals.is_empty() {
        return Err("decals and damage shape the whole mesh");
    }
    if palette.iter().any(|m| crate::smart::layered(m) || m.weathered()) {
        return Err("smart layers and weathering read the whole mesh");
    }
    if clay.exact_planar() {
        return Err("one exact planar blob keeps its exact mesh");
    }
    if clay.blobs.is_empty() {
        return Err("the clay is empty");
    }
    Ok(())
}

/// The lattice cell a chunked mesh uses: the detail grid's (so strokes
/// and chunks share points), else `detail_cell`, else the mesh cell.
pub fn voxel_of(clay: &Clay) -> Option<f32> {
    crate::layers::composed_detail(clay).map(|d| d.voxel()).or(clay.detail_cell).or_else(|| clay.cell_size()).filter(|v| v.is_finite() && *v > 0.0)
}

/// What a chunk is built from: the clay's field with its blob field cached
/// on the lattice, and what colors it.
pub struct Mesher {
    field: Field,
    cache: Arc<BaseCache>,
    pub voxel: f32,
    sharp: bool,
    palette: Vec<Material>,
    detail: Option<Detail>,
    /// Lattice cells holding the clay (with a margin), low and high.
    cells: (IVec3, IVec3),
    /// Every prim (for points no prim is near).
    all: Vec<u32>,
    /// Whether any prim has exact planar faces (`Field::project_planar`
    /// leaves vertices where they are without one).
    planar: bool,
    /// Prims with sharp features (hard forms, plates, chamfers; every prim
    /// with a shell): only cells near one place vertices on features, so
    /// sculpted organic surface meshes as plain surface nets.
    hard: Vec<bool>,
    /// Lattice bricks worked out so far (chunks overlap by their halos,
    /// and picking reads the same bricks again).
    lattice: std::sync::RwLock<KeyMap<Arc<Brick>>>,
}

/// Most lattice bricks a mesher keeps (past it they are worked out again).
const MAX_LATTICE: usize = 400_000;

/// A brick of lattice points: one value throughout, or its values.
enum Brick {
    Const(f32),
    /// The values: the blob field's brick (shared), plus the detail grid's
    /// brick where it has one (shared too: added as the block is read).
    Data(Arc<[f32; 512]>, Option<Arc<[f32; 512]>>),
}

impl Brick {
    fn at(&self, at: usize) -> f32 {
        match self {
            Brick::Const(v) => *v,
            Brick::Data(b, None) => b[at],
            Brick::Data(b, Some(d)) => b[at] + d[at],
        }
    }
}

impl Mesher {
    /// The mesher of `clay` (composed: layers folded in, see
    /// `layers::composed_clay`), colored by `palette`.
    pub fn new(clay: &Clay, palette: &[Material]) -> Result<Mesher, String> {
        eligible(clay, palette)?;
        let voxel = voxel_of(clay).ok_or("the clay has no cell size")?;
        let field = Field::new(clay, voxel);
        let (lo, hi) = field.bounds().ok_or("the clay is empty")?;
        let (a, b) = ((lo / voxel).floor(), (hi / voxel).ceil());
        if !a.is_finite() || !b.is_finite() || a.abs().max_element() > MAX_COORD || b.abs().max_element() > MAX_COORD {
            return Err("the clay is too far out or too big for its cell".into());
        }
        if (b - a).max_element() > MAX_CELLS {
            return Err(format!("the clay is more than {MAX_CELLS} cells across at cell {voxel}"));
        }
        // Two empty cells on every side, as the regular mesher's.
        let cells = (a.as_ivec3() - 2, b.as_ivec3() + 3);
        let span = (cells.1 - cells.0).as_dvec3() / CHUNK as f64 + 2.0;
        if span.x * span.y * span.z > MAX_CHUNKS as f64 {
            return Err("the clay spans too many chunks at its cell".into());
        }
        let cache = base_cache::for_clay(clay, &field, voxel);
        let field = field.with_base(cache.clone());
        let all = (0..field.prims.len() as u32).collect();
        // As `Clay::sharp_features`, prim by prim (a mirrored blob makes
        // two prims, its mirror image first, as `Field::new` does).
        let mut hard = Vec::with_capacity(field.prims.len());
        for b in clay.blobs.iter().take(super::MAX_BLOBS) {
            let h = clay.shell.is_some()
                || !matches!(b.form, super::Form::Sphere { .. } | super::Form::Ellipsoid { .. } | super::Form::Capsule { .. } | super::Form::Human { .. } | super::Form::Torus { .. } | super::Form::Chain { .. } | super::Form::Membrane { .. })
                || matches!(b.op, super::Op::Plate | super::Op::Engrave | super::Op::Emboss)
                || b.join == hard_surface::Join::Chamfer;
            hard.extend(std::iter::repeat_n(h, if b.mirror { 2 } else { 1 }));
        }
        debug_assert_eq!(hard.len(), field.prims.len());
        let planar = field.prims.iter().any(|p| matches!(&p.geo, super::Geo::Box { round, .. } if *round == 0.0) || matches!(&p.geo, super::Geo::Hard(hard_surface::Geometry::Extrude { bevel, .. }) if *bevel == 0.0));
        Ok(Mesher { cache, voxel, sharp: clay.sharp_features(), palette: palette.to_vec(), detail: crate::layers::composed_detail(clay), cells, all, planar, hard, field, lattice: Default::default() })
    }

    /// The blob field's cache (warm while a session holds this mesher).
    pub fn cache(&self) -> &Arc<BaseCache> {
        &self.cache
    }

    /// Chunks the clay's box spans.
    pub fn chunk_range(&self) -> (IVec3, IVec3) {
        (self.cells.0.div_euclid(IVec3::splat(CHUNK)), (self.cells.1 - 1).div_euclid(IVec3::splat(CHUNK)))
    }

    fn point(&self, i: IVec3) -> Vec3 {
        i.as_vec3() * self.voxel
    }

    /// Bricks `keys` of the lattice that hold one value throughout: where
    /// the surface is surely far and no detail is (as the regular mesher's
    /// blocks). A function of the key alone.
    fn constants(&self, keys: &[IVec3]) -> Vec<Option<f32>> {
        let v = self.voxel;
        let sides = self.cache.sides(&self.field, &keys.iter().map(|k| k.to_array()).collect::<Vec<_>>(), FAR_CELLS * v);
        keys.iter().zip(sides).map(|(key, side)| {
            if let Some(g) = self.detail.as_ref().map(|d| d.grid())
                && (g.brick(&key.to_array()).is_some() || g.levels_touch(self.point(*key * 8) - v, self.point(*key * 8 + 7) + v))
            {
                return None;
            }
            // Nothing near reads as outside (the clamp), as the regular
            // mesher's empty blocks.
            side.map(|inside| if inside { -self.field.limit } else { self.field.limit })
        }).collect()
    }

    /// Brick `key`'s values: the blob field (`base`, cached) plus the detail
    /// grid's offset at each point.
    fn data(&self, key: IVec3, base: &Arc<[f32; 512]>) -> Brick {
        let Some(g) = self.detail.as_ref().map(|d| d.grid()) else { return Brick::Data(base.clone(), None) };
        let v = self.voxel;
        let coarse = g.brick(&key.to_array()).cloned();
        if !g.levels_touch(self.point(key * 8) - v, self.point(key * 8 + 7) + v) {
            return Brick::Data(base.clone(), coarse);
        }
        // Finer levels add what they hold at each point.
        let mut out = **base;
        for (at, o) in out.iter_mut().enumerate() {
            if let Some(c) = &coarse {
                *o += c[at];
            }
            let i = key * 8 + IVec3::new(at as i32 % 8, at as i32 / 8 % 8, at as i32 / 64);
            *o += g.lattice(i.to_array()) - g.get(i.to_array());
        }
        Brick::Data(Arc::new(out), None)
    }

    /// Lattice bricks `keys`, from the memo or worked out (in one go for
    /// the blob field's cache).
    fn bricks(&self, keys: &[IVec3]) -> KeyMap<Arc<Brick>> {
        let mut out: KeyMap<Arc<Brick>> = KeyMap::with_capacity_and_hasher(keys.len(), Default::default());
        let mut missing = Vec::new();
        {
            let memo = self.lattice.read().unwrap_or_else(|e| e.into_inner());
            for k in keys {
                match memo.get(&k.to_array()) {
                    Some(b) => {
                        out.insert(k.to_array(), b.clone());
                    }
                    None => missing.push(*k),
                }
            }
        }
        if missing.is_empty() {
            return out;
        }
        let kinds: Vec<Option<f32>> = self.constants(&missing);
        let wanted: Vec<[i32; 3]> = missing.iter().zip(&kinds).filter(|(_, c)| c.is_none()).map(|(k, _)| k.to_array()).collect();
        let bases = self.cache.bricks_of(&self.field, &wanted);
        let mut memo = self.lattice.write().unwrap_or_else(|e| e.into_inner());
        for (k, c) in missing.iter().zip(&kinds) {
            let b = Arc::new(match c {
                Some(v) => Brick::Const(*v),
                None => self.data(*k, &bases[&k.to_array()]),
            });
            if memo.len() < MAX_LATTICE {
                memo.insert(k.to_array(), b.clone());
            }
            out.insert(k.to_array(), b);
        }
        out
    }

    /// Lattice values at points `p0 + (x, y, z)`, x < n.x and so on.
    /// None when every brick holds one value of one sign (no surface).
    fn samples(&self, p0: IVec3, n: IVec3) -> Option<Vec<f32>> {
        let s = IVec3::splat(8);
        let (b0, b1) = (p0.div_euclid(s), (p0 + n - 1).div_euclid(s));
        let keys: Vec<IVec3> = (b0.z..=b1.z).flat_map(|z| (b0.y..=b1.y).flat_map(move |y| (b0.x..=b1.x).map(move |x| IVec3::new(x, y, z)))).collect();
        let bricks = self.bricks(&keys);
        let signs: Vec<Option<bool>> = bricks.values().map(|b| match &**b {
            Brick::Const(v) => Some(*v < 0.0),
            Brick::Data(..) => None,
        }).collect();
        if signs.iter().all(|s| s.is_some() && *s == signs[0]) {
            return None;
        }
        let mut out = vec![0.0f32; (n.x * n.y * n.z) as usize];
        // Brick by brick: each brick's points inside the block.
        for k in &keys {
            let b = &bricks[&k.to_array()];
            let (lo, hi) = ((*k * 8).max(p0), (*k * 8 + 8).min(p0 + n));
            for z in lo.z..hi.z {
                for y in lo.y..hi.y {
                    let row = ((lo.x - p0.x) + n.x * ((y - p0.y) + n.y * (z - p0.z))) as usize;
                    let len = (hi.x - lo.x) as usize;
                    match &**b {
                        Brick::Const(v) => out[row..row + len].fill(*v),
                        Brick::Data(b, d) => {
                            let at = ((lo.x - k.x * 8) + 8 * ((y - k.y * 8) + 8 * (z - k.z * 8))) as usize;
                            match d {
                                None => out[row..row + len].copy_from_slice(&b[at..at + len]),
                                Some(d) => {
                                    for (o, (b, d)) in out[row..row + len].iter_mut().zip(b[at..at + len].iter().zip(&d[at..at + len])) {
                                        *o = b + d;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(out)
    }

    /// The lattice value at point `i`.
    fn at_point(&self, i: IVec3) -> f32 {
        let (key, at) = base_cache::split(i);
        let b = self.bricks(&[IVec3::from(key)]).remove(&key).expect("worked out");
        b.at(at)
    }

    /// The lattice field between points (trilinear): what the chunks mesh.
    pub fn value(&self, p: Vec3) -> f32 {
        let g = p / self.voxel;
        let f = g.floor();
        let t = g - f;
        let i = f.as_ivec3();
        let v: [f32; 8] = std::array::from_fn(|n| self.at_point(i + IVec3::new((n & 1) as i32, ((n >> 1) & 1) as i32, ((n >> 2) & 1) as i32)));
        let x0 = v[0] + (v[1] - v[0]) * t.x;
        let x1 = v[2] + (v[3] - v[2]) * t.x;
        let x2 = v[4] + (v[5] - v[4]) * t.x;
        let x3 = v[6] + (v[7] - v[6]) * t.x;
        let y0 = x0 + (x1 - x0) * t.y;
        let y1 = x2 + (x3 - x2) * t.y;
        y0 + (y1 - y0) * t.z
    }

    /// `pick_field`: where a ray (object space; `t` in units of `dir`)
    /// first meets the surface the chunks mesh, sphere-traced through the
    /// cached blob field plus the composed detail: (t, normal facing the
    /// ray). Exact against what brushes edit, and the same warm or cold.
    pub fn pick(&self, origin: Vec3, dir: Vec3) -> Option<(f32, Vec3)> {
        let len = dir.length();
        if !origin.is_finite() || !dir.is_finite() || len < 1e-12 {
            return None;
        }
        let v = self.voxel;
        let (lo, hi) = (self.cells.0.as_vec3() * v, self.cells.1.as_vec3() * v);
        let inv = dir.recip();
        let (t0, t1) = ((lo - origin) * inv, (hi - origin) * inv);
        let (enter, exit) = (t0.min(t1).max_element().max(0.0), t0.max(t1).min_element());
        if !(enter <= exit) {
            return None;
        }
        let mut t = enter;
        let mut f = self.value(origin + dir * t);
        if f < 0.0 {
            return Some((t, -dir / len));
        }
        // Steps of at most 4 cells (the clamped field says nothing of
        // distance far out), at least a quarter cell.
        while t < exit {
            let step = (f.abs() * 0.9).clamp(0.25 * v, 4.0 * v) / len;
            let next = (t + step).min(exit);
            let g = self.value(origin + dir * next);
            if g < 0.0 {
                let (mut a, mut b) = (t, next);
                for _ in 0..20 {
                    let m = (a + b) * 0.5;
                    if self.value(origin + dir * m) < 0.0 {
                        b = m;
                    } else {
                        a = m;
                    }
                }
                let p = origin + dir * b;
                let e = v * 0.5;
                let n = Vec3::new(self.value(p + Vec3::X * e) - self.value(p - Vec3::X * e), self.value(p + Vec3::Y * e) - self.value(p - Vec3::Y * e), self.value(p + Vec3::Z * e) - self.value(p - Vec3::Z * e)).normalize_or(-dir / len);
                return Some((b, if n.dot(dir) > 0.0 { -n } else { n }));
            }
            (t, f) = (next, g);
            if next >= exit {
                break;
            }
        }
        None
    }

    /// The surface through the cells `lo..hi`: every grid edge starting at
    /// a point in that box, with the vertices of the cells around them.
    pub fn mesh_range(&self, lo: IVec3, hi: IVec3) -> ChunkMesh {
        let v = self.voxel;
        let p0 = lo - 2;
        let n = hi - lo + 4;
        let Some(s) = self.samples(p0, n) else { return ChunkMesh::default() };
        let at = |i: IVec3| -> f32 {
            let d = i - p0;
            s[(d.x + n.x * (d.y + n.y * d.z)) as usize]
        };
        // Cells lo - 1 .. hi (one cell of halo below).
        let c0 = lo - 1;
        let cn = hi - c0;
        let cell_index = |c: IVec3| -> usize {
            let d = c - c0;
            (d.x + cn.x * (d.y + cn.y * d.z)) as usize
        };
        // Quick out: no corner of any cell changes sign.
        // Inside bits of every point, then: no corner of any cell changes
        // sign, no surface.
        let sign: Vec<bool> = s.iter().map(|v| *v < 0.0).collect();
        let idx = |i: IVec3| -> usize {
            let d = i - p0;
            (d.x + n.x * (d.y + n.y * d.z)) as usize
        };
        let first = sign[idx(c0)];
        let any = (c0.z..=hi.z).any(|z| (c0.y..=hi.y).any(|y| {
            let row = idx(IVec3::new(c0.x, y, z));
            sign[row..=row + (hi.x - c0.x) as usize].iter().any(|b| *b != first)
        }));
        if !any {
            return ChunkMesh::default();
        }
        // A step along each axis in the block.
        let step = [1usize, n.x as usize, (n.x * n.y) as usize];
        // Offsets of a cell's 8 corners in the block.
        let corner: [usize; 8] = CORNERS.map(|(dx, dy, dz)| dx + n.x as usize * (dy + n.y as usize * dz));
        // The prims near each 8^3 block of cells (with two cells around):
        // every list a cell's vertex, normal and color read, worked out once.
        let memo: RefCell<KeyMap<Rc<Vec<u32>>>> = RefCell::new(KeyMap::default());
        let lists = |c: IVec3| -> Rc<Vec<u32>> {
            let b = c.div_euclid(IVec3::splat(8));
            memo.borrow_mut().entry(b.to_array()).or_insert_with(|| Rc::new(self.field.near(self.point(b * 8 - 2), self.point(b * 8 + 10)))).clone()
        };
        // One vertex per surface piece of each crossed cell, in cell order.
        let mut positions: Vec<Vec3> = Vec::new();
        let mut cell_of: Vec<(IVec3, bool)> = Vec::new();
        let mut label_of: Vec<u32> = Vec::new();
        // Per cell: its first vertex and its edges' pieces (crossed cells).
        let mut first_vertex: Vec<u32> = vec![u32::MAX; (cn.x * cn.y * cn.z) as usize];
        let mut cell_labels: Vec<[u8; 12]> = Vec::new();
        // Whether the point rows at (y, z) and (y, z) + `d` all hold one
        // sign over x in c0.x ..= hi.x (then no cell between them crosses).
        let rows_uniform = |y: i32, z: i32, d: &[(i32, i32)]| -> bool {
            let first = sign[idx(IVec3::new(c0.x, y, z))];
            d.iter().all(|&(dy, dz)| {
                let row = idx(IVec3::new(c0.x, y + dy, z + dz));
                sign[row..=row + (hi.x - c0.x) as usize].iter().all(|b| *b == first)
            })
        };
        for z in c0.z..hi.z {
            for y in c0.y..hi.y {
                if rows_uniform(y, z, &[(0, 0), (1, 0), (0, 1), (1, 1)]) {
                    continue;
                }
                // Corner signs slide along x: the cell's low-x corners are
                // the last cell's high-x ones (corner k has x = k & 1).
                let row = idx(IVec3::new(c0.x, y, z));
                let high = |base: usize| -> usize { (1..8).step_by(2).fold(0, |m, k| m | (sign[base + corner[k]] as usize) << k) };
                let mut mask = (0..8).step_by(2).fold(0, |m, k| m | (sign[row + corner[k]] as usize) << k);
                for x in c0.x..hi.x {
                    let c = IVec3::new(x, y, z);
                    let base = row + (x - c0.x) as usize;
                    mask = (mask & 0x55) | high(base);
                    let this = mask;
                    // The next cell's low-x corners.
                    mask = (mask & 0xaa) >> 1;
                    if this == 0 || this == 255 {
                        continue;
                    }
                    let mask = this;
                    let f: [f32; 8] = corner.map(|o| s[base + o]);
                    let labels = pieces_of(mask, &f);
                    let count = labels.iter().filter(|l| **l != OTHER).map(|l| *l as usize + 1).max().unwrap_or(0);
                    first_vertex[cell_index(c)] = positions.len() as u32;
                    cell_labels.push(labels);
                    for k in 0..count {
                        let (p, bent) = self.cell_vertex(c, &f, &labels, k as u8, &lists(c));
                        positions.push(p);
                        cell_of.push((c, bent));
                        label_of.push((cell_labels.len() - 1) as u32);
                    }
                }
            }
        }
        // One quad per edge this range owns that the surface crosses.
        let mut indices = Vec::new();
        let axes = [IVec3::X, IVec3::Y, IVec3::Z];
        let mut quads: Vec<([u32; 4], usize, IVec3)> = Vec::new();
        for z in lo.z..hi.z {
            for y in lo.y..hi.y {
                // Edges from this row of points stay in it or reach the
                // rows above in y and z.
                if rows_uniform(y, z, &[(0, 0), (1, 0), (0, 1)]) {
                    continue;
                }
                for x in lo.x..hi.x {
                    let g = IVec3::new(x, y, z);
                    let here = idx(g);
                    let inside = sign[here];
                    for axis in 0..3 {
                        if inside == sign[here + step[axis]] {
                            continue;
                        }
                        let (eu, ev) = (axes[(axis + 1) % 3], axes[(axis + 2) % 3]);
                        let mut q = [0u32; 4];
                        let mut ok = true;
                        for (k, (du, dv)) in [(1, 1), (0, 1), (0, 0), (1, 0)].into_iter().enumerate() {
                            let c = g - eu * du - ev * dv;
                            let edge = axis * 4 + du as usize * 2 + dv as usize;
                            let v0 = first_vertex[cell_index(c)];
                            let labels = if v0 == u32::MAX { None } else { cell_labels.get(label_of[v0 as usize] as usize) };
                            match labels {
                                Some(labels) if labels[edge] != OTHER => q[k] = v0 + labels[edge] as u32,
                                _ => ok = false,
                            }
                        }
                        if !ok {
                            continue;
                        }
                        let q = if inside { q } else { [q[3], q[2], q[1], q[0]] };
                        quads.push((q, axis, g));
                    }
                }
            }
        }
        for (q, axis, g) in quads {
            let [a, b, c, d] = q;
            if !self.sharp_near(&lists(g)) {
                indices.extend_from_slice(&[a, b, c, a, c, d]);
                continue;
            }
            let start = g.as_vec3() * v;
            let inside = at(g) < 0.0;
            let direction = axes[axis].as_vec3() * if inside { 1.0 } else { -1.0 };
            match hard_surface::quad_diagonal(q, direction, &positions) {
                Some(true) => indices.extend_from_slice(&[a, b, d, b, c, d]),
                Some(false) => indices.extend_from_slice(&[a, b, c, a, c, d]),
                None => {
                    // A fan around the edge's crossing (as the regular
                    // mesher), which keeps the sharp vertices.
                    let end = (g + axes[axis]).as_vec3() * v;
                    let near = lists(g);
                    let side = self.field.sample(start, &near).0 < 0.0;
                    let (mut l, mut r) = (0.0f32, 1.0f32);
                    for _ in 0..16 {
                        let t = (l + r) * 0.5;
                        if (self.field.sample(start.lerp(end, t), &near).0 < 0.0) == side {
                            l = t;
                        } else {
                            r = t;
                        }
                    }
                    let center = positions.len() as u32;
                    positions.push(start.lerp(end, ((l + r) * 0.5).clamp(0.001, 0.999)));
                    cell_of.push((g, true));
                    indices.extend_from_slice(&[a, b, center, b, c, center, c, d, center, d, a, center]);
                }
            }
        }
        // Keep the vertices the triangles use, in order of creation.
        let mut used = vec![u32::MAX; positions.len()];
        for &i in &indices {
            used[i as usize] = 0;
        }
        let mut kept = Vec::new();
        for (i, u) in used.iter_mut().enumerate() {
            if *u == 0 {
                *u = kept.len() as u32;
                kept.push(i);
            }
        }
        for i in &mut indices {
            *i = used[*i as usize];
        }
        let mut mesh = ChunkMesh { positions: kept.iter().map(|&i| positions[i]).collect(), indices, ..Default::default() };
        let cells: Vec<(IVec3, bool)> = kept.iter().map(|&i| cell_of[i]).collect();
        let count = mesh.positions.len();
        (mesh.normals, mesh.base, mesh.mats) = (Vec::with_capacity(count), Vec::with_capacity(count), Vec::with_capacity(count));
        for (p, (c, bent)) in mesh.positions.iter().zip(&cells) {
            let list = lists(*c);
            let n = self.normal(*p, *c, *bent, &s, idx(*c), step, &list);
            let (b, m) = self.color(*p, n, &list);
            mesh.normals.push(n);
            mesh.base.push(b);
            mesh.mats.push(m);
        }
        mesh.cavity = cells.iter().map(|(c, _)| {
            let mut sum = 0.0f32;
            let base = idx(*c);
            for o in corner {
                let i = base + o;
                let mut l = -6.0 * s[i];
                for st in step {
                    l += s[i + st] + s[i - st];
                }
                sum += l;
            }
            // Laplacian / cell: about -2 cells / radius in a crease.
            ((-sum / 8.0 / v).clamp(0.0, 1.0) * 255.0).round() as u8
        }).collect();
        mesh
    }

    /// Piece `k`'s vertex in cell `c` (corner values `f`), and whether the
    /// cell is bent (a sharp feature: its normal comes from the field).
    fn cell_vertex(&self, c: IVec3, f: &[f32; 8], labels: &[u8; 12], k: u8, near: &[u32]) -> (Vec3, bool) {
        let v = self.voxel;
        let origin = c.as_vec3() * v;
        let corner = |i: usize| Vec3::new(CORNERS[i].0 as f32, CORNERS[i].1 as f32, CORNERS[i].2 as f32);
        let mut hit_store = [(Vec3::ZERO, Vec3::ZERO); 12];
        let mut edge_store = [(0usize, 0usize); 12];
        let mut count = 0;
        for e in 0..12 {
            if labels[e] != k {
                continue;
            }
            let (a, b) = cube_edge(e);
            let t = f[a] / (f[a] - f[b]);
            hit_store[count] = (corner(a) + (corner(b) - corner(a)) * t, Vec3::ZERO);
            edge_store[count] = (a, b);
            count += 1;
        }
        let (hits, edges) = (&mut hit_store[..count], &edge_store[..count]);
        let mean = hits.iter().map(|h| h.0).sum::<Vec3>() / count.max(1) as f32;
        if !self.sharp_near(near) {
            return (origin + mean * v, false);
        }
        let mut position = mean;
        let bent = hard_surface::bent(&hard_surface::corner_normals(f), 0.98);
        if bent {
            for (i, &(a, b)) in edges.iter().enumerate() {
                let (pa, pb) = (corner(a), corner(b));
                let (mut l, mut r) = (0.0f32, 1.0f32);
                for _ in 0..16 {
                    let mid = (l + r) * 0.5;
                    let value = self.field.sample(origin + (pa + (pb - pa) * mid) * v, near).0;
                    if (value < 0.0) == (f[a] < 0.0) {
                        l = mid;
                    } else {
                        r = mid;
                    }
                }
                let hit = pa + (pb - pa) * ((l + r) * 0.5);
                hits[i] = (hit, self.gradient(origin + hit * v, near, v * 0.01));
            }
            let refined = hits.iter().map(|(p, _)| *p).sum::<Vec3>() / hits.len().max(1) as f32;
            position = hard_surface::feature_vertex(hits, refined);
        }
        if self.planar {
            position = (self.field.project_planar(origin + position * v, origin, origin + Vec3::splat(v), near, v) - origin) / v;
        }
        position = position.clamp(Vec3::splat(1e-3), Vec3::splat(1.0 - 1e-3));
        (origin + position * v, bent)
    }

    /// Whether cells with prims `near` place vertices on sharp features.
    fn sharp_near(&self, near: &[u32]) -> bool {
        self.sharp && near.iter().any(|&i| self.hard.get(i as usize).copied().unwrap_or(true))
    }

    fn gradient(&self, p: Vec3, list: &[u32], e: f32) -> Vec3 {
        let g = |axis: Vec3| self.field.sample(p + axis * e, list).0 - self.field.sample(p - axis * e, list).0;
        Vec3::new(g(Vec3::X), g(Vec3::Y), g(Vec3::Z)).normalize_or(Vec3::Y)
    }

    /// The surface normal at vertex `p` of cell `c`: the lattice's central
    /// differences blended across the cell, or (bent sharp cells) the
    /// field's gradient right there.
    /// (`s`: the block's values, `base`: cell c's low corner in it, `step`:
    /// a step along each axis.)
    #[allow(clippy::too_many_arguments)]
    fn normal(&self, p: Vec3, c: IVec3, bent: bool, s: &[f32], base: usize, step: [usize; 3], near: &[u32]) -> Vec3 {
        let v = self.voxel;
        if bent {
            let list = if near.is_empty() { &self.all } else { near };
            return self.gradient(p, list, v * 0.05);
        }
        let t = (p / v - c.as_vec3()).clamp(Vec3::ZERO, Vec3::ONE);
        let mut g = Vec3::ZERO;
        for (n, &(dx, dy, dz)) in CORNERS.iter().enumerate() {
            let i = base + dx * step[0] + dy * step[1] + dz * step[2];
            let d = Vec3::new(s[i + step[0]] - s[i - step[0]], s[i + step[1]] - s[i - step[1]], s[i + step[2]] - s[i - step[2]]);
            let w = (if n & 1 == 1 { t.x } else { 1.0 - t.x }) * (if n & 2 == 2 { t.y } else { 1.0 - t.y }) * (if n & 4 == 4 { t.z } else { 1.0 - t.z });
            g += d * w;
        }
        g.normalize_or(Vec3::Y)
    }

    /// The blob color under any sculpt (sRGB) and material index at `p`, as
    /// the regular mesher: from where the surface grew (along the normal).
    fn color(&self, p: Vec3, n: Vec3, near: &[u32]) -> (Vec3, u16) {
        let list = if near.is_empty() { &self.all } else { near };
        // The offset at the nearest grid point is enough to look through
        // the sculpt (one lookup, not eight).
        let off = self.detail.as_ref().map_or(0.0, |d| d.grid().lattice((p / self.voxel).round().as_ivec3().to_array()));
        let (_, base, mat) = self.field.base_full(p + n * off, list, true);
        let mat = (mat as usize).min(self.palette.len().saturating_sub(1));
        let base = self.palette.get(mat).map_or(base, |m| m.base_color.map_or(base, Vec3::from));
        (base, mat as u16)
    }

    /// The chunks that can hold surface (their box's middle is not surely
    /// far from it, or detail is there).
    pub fn live_chunks(&self) -> Vec<ChunkKey> {
        let (a, b) = self.chunk_range();
        let v = self.voxel;
        let mut keys: Vec<ChunkKey> = Vec::new();
        for z in a.z..=b.z {
            for y in a.y..=b.y {
                for x in a.x..=b.x {
                    keys.push([x, y, z]);
                }
            }
        }
        let mut live: Vec<(ChunkKey, bool)> = keys.into_iter().map(|k| (k, false)).collect();
        par_chunks(&mut live, 8, |_, part| {
            for (k, keep) in part {
                let c = IVec3::from(*k) * CHUNK;
                let (lo, hi) = (self.point(c - 2), self.point(c + CHUNK + 2));
                let detail = self.detail.as_ref().is_some_and(|d| d.grid().touches(lo, hi));
                let near = self.field.near(lo, hi);
                *keep = detail || (!near.is_empty() && self.field.far_side((lo + hi) * 0.5, &near, (hi - lo).length() * 0.5 + v).is_none());
            }
        });
        live.into_iter().filter(|(_, k)| *k).map(|(k, _)| k).collect()
    }

    /// One chunk's mesh.
    pub fn mesh_chunk(&self, key: ChunkKey) -> ChunkMesh {
        let c = IVec3::from(key) * CHUNK;
        self.mesh_range(c, c + CHUNK)
    }

    /// Works out the lattice bricks chunks `keys` read, in parallel, into
    /// the memo (chunk builds then only read it).
    fn prefill(&self, keys: &[ChunkKey]) {
        // The bricks every chunk reads (four a side and one around), as
        // marks on the box of them all.
        let Some((lo, hi)) = keys.iter().map(|k| IVec3::from(*k) * 4).fold(None, |b: Option<(IVec3, IVec3)>, k| Some(b.map_or((k, k), |(a, b)| (a.min(k), b.max(k))))) else { return };
        let (lo, hi) = (lo - 1, hi + 4);
        let n = hi - lo + 1;
        if (n.x as i64) * (n.y as i64) * (n.z as i64) > 64 << 20 {
            return;
        }
        let mut marked = vec![false; (n.x * n.y * n.z) as usize];
        for k in keys {
            let b = IVec3::from(*k) * 4 - lo;
            for z in b.z - 1..=b.z + 4 {
                for y in b.y - 1..=b.y + 4 {
                    let row = (b.x - 1 + n.x * (y + n.y * z)) as usize;
                    marked[row..row + 6].fill(true);
                }
            }
        }
        let mut want: Vec<[i32; 3]> = Vec::new();
        {
            let memo = self.lattice.read().unwrap_or_else(|e| e.into_inner());
            for (i, _) in marked.iter().enumerate().filter(|(_, m)| **m) {
                let i = i as i32;
                let key = [lo.x + i % n.x, lo.y + i / n.x % n.y, lo.z + i / (n.x * n.y)];
                if !memo.contains_key(&key) {
                    want.push(key);
                }
            }
        }
        want.sort_unstable();
        if want.is_empty() {
            return;
        }
        let kinds: Vec<([i32; 3], Option<f32>)> = want.iter().copied().zip(self.constants(&want.iter().map(|k| IVec3::from(*k)).collect::<Vec<_>>())).collect();
        let wanted: Vec<[i32; 3]> = kinds.iter().filter(|(_, c)| c.is_none()).map(|(k, _)| *k).collect();
        let bases = self.cache.bricks_of(&self.field, &wanted);
        let mut made: Vec<([i32; 3], Option<f32>, Option<Arc<Brick>>)> = kinds.into_iter().map(|(k, c)| (k, c, None)).collect();
        par_chunks(&mut made, 64, |_, part| {
            for (k, c, out) in part {
                *out = Some(Arc::new(match c {
                    Some(v) => Brick::Const(*v),
                    None => self.data(IVec3::from(*k), &bases[k]),
                }));
            }
        });
        let mut memo = self.lattice.write().unwrap_or_else(|e| e.into_inner());
        memo.reserve(made.len());
        for (k, _, b) in made {
            if memo.len() >= MAX_LATTICE {
                break;
            }
            memo.insert(k, b.expect("made"));
        }
    }

    /// The meshes of `keys`, built in parallel (empty ones left out).
    pub fn mesh_chunks(&self, keys: &[ChunkKey]) -> Vec<(ChunkKey, Option<Arc<ChunkMesh>>)> {
        self.prefill(keys);
        let mut out: Vec<(ChunkKey, Option<Arc<ChunkMesh>>)> = keys.iter().map(|k| (*k, None)).collect();
        par_chunks(&mut out, 1, |_, part| {
            for (k, m) in part {
                let mesh = self.mesh_chunk(*k);
                *m = (!mesh.indices.is_empty()).then(|| Arc::new(mesh));
            }
        });
        out
    }

    /// The monolithic anchored reference: the same kernel over one range
    /// covering the whole clay.
    #[cfg(test)]
    pub fn mesh_whole(&self) -> ChunkMesh {
        self.mesh_range(self.cells.0, self.cells.1)
    }

}

/// `adaptive::pieces` for a cell whose inside corners are `mask` (bit k:
/// corner k): from a table where no face is crossed four times (the
/// pieces then follow from the signs alone), else worked out.
fn pieces_of(mask: usize, f: &[f32; 8]) -> [u8; 12] {
    static TABLE: std::sync::OnceLock<Vec<Option<[u8; 12]>>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..256usize)
            .map(|m| {
                let inside = |c: usize| (m >> c) & 1 == 1;
                // A face crossed four times: its corners alternate.
                let ambiguous = (0..3).any(|axis| (0..2).any(|side| {
                    let on: Vec<usize> = (0..8).filter(|&c| (c >> axis) & 1 == side).collect();
                    let crossed = (0..12).filter(|&e| {
                        let (a, b) = cube_edge(e);
                        on.contains(&a) && on.contains(&b) && inside(a) != inside(b)
                    }).count();
                    crossed == 4
                }));
                (!ambiguous).then(|| pieces(&std::array::from_fn(|c| if inside(c) { -1.0 } else { 1.0 })))
            })
            .collect()
    });
    table[mask].unwrap_or_else(|| pieces(f))
}

/// The chunks a clay's surface is drawn with, by key.
#[derive(Clone, Default)]
pub struct ChunkSet {
    pub chunks: BTreeMap<ChunkKey, Arc<ChunkMesh>>,
}

impl ChunkSet {
    /// Every chunk of the clay.
    pub fn build(mesher: &Mesher) -> ChunkSet {
        let keys = mesher.live_chunks();
        ChunkSet { chunks: mesher.mesh_chunks(&keys).into_iter().filter_map(|(k, m)| Some((k, m?))).collect() }
    }

    /// This set with `dirty` chunks built again (the others shared).
    pub fn rebuilt(&self, mesher: &Mesher, dirty: &[ChunkKey]) -> ChunkSet {
        let mut chunks = self.chunks.clone();
        for (k, m) in mesher.mesh_chunks(dirty) {
            match m {
                Some(m) => {
                    chunks.insert(k, m);
                }
                None => {
                    chunks.remove(&k);
                }
            }
        }
        ChunkSet { chunks }
    }

    #[cfg(test)]
    pub fn triangles(&self) -> usize {
        self.chunks.values().map(|c| c.indices.len() / 3).sum()
    }

    /// The chunks as one mesh (seam vertices welded: equal positions are
    /// one vertex), in the regular vertex format with `paint` over the blob
    /// colors.
    #[cfg(test)]
    pub fn assemble(&self, palette: &[Material], paint: Option<&Paint>) -> Mesh {
        let mut ids: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
        let mut mesh = Mesh { materials: palette.to_vec(), ..Mesh::default() };
        for c in self.chunks.values() {
            let map: Vec<u32> = (0..c.positions.len())
                .map(|i| {
                    *ids.entry(c.positions[i].to_array().map(f32::to_bits)).or_insert_with(|| {
                        mesh.vertices.push(vertex(c, i, palette, paint));
                        (mesh.vertices.len() - 1) as u32
                    })
                })
                .collect();
            mesh.indices.extend(c.indices.iter().map(|&i| map[i as usize]));
        }
        mesh
    }
}

/// Chunk mesh `c`'s vertex `i` in the regular format.
#[cfg(test)]
pub fn vertex(c: &ChunkMesh, i: usize, palette: &[Material], paint: Option<&Paint>) -> Vertex {
    let p = c.positions[i];
    let m = palette.get(c.mats[i] as usize);
    let color = m.map_or(c.base[i], |m| m.painted_color(c.base[i], paint, p));
    Vertex {
        pos: p.to_array(),
        normal: c.normals[i].to_array(),
        color: color.to_array(),
        surface: m.map_or(crate::mesh::OBJECT_SURFACE, Material::surface),
        material: c.mats[i] as u32,
        uv: [0.0; 2],
        tangent: [0.0; 4],
        light: m.map_or([0.0; 2], Material::light),
        base: [1.0, 1.0, 1.0, -1.0],
        wear: [0.0; 3],
        painted: [0; 4],
    }
}

/// The chunks whose meshes can differ between grids `old` and `new` (the
/// composed detail grids of the same clay otherwise): the changed bricks'
/// points, grown by the reach of a cell's vertex and normal. None when
/// the layouts differ (every chunk then).
pub fn dirty_detail(old: Option<&Detail>, new: Option<&Detail>, voxel: f32) -> Option<Vec<ChunkKey>> {
    let mut boxes: Vec<(IVec3, IVec3)> = Vec::new();
    match (old, new) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            if a == b {
                return Some(Vec::new());
            }
            let (ga, gb) = (a.grid(), b.grid());
            if !ga.same_layout(gb) || ga.voxel != voxel {
                return None;
            }
            changed(ga, gb, |k| boxes.push((IVec3::from(k) * 8, IVec3::from(k) * 8 + 7)));
            for (la, lb) in ga.levels().iter().zip(gb.levels()) {
                let s = lb.grid.voxel * 8.0;
                changed(&la.grid, &lb.grid, |k| {
                    let lo = Vec3::from(k.map(|x| x as f32)) * s;
                    boxes.push((((lo - lb.grid.voxel) / voxel).floor().as_ivec3(), ((lo + s + lb.grid.voxel) / voxel).ceil().as_ivec3()));
                });
            }
        }
        (None, Some(g)) | (Some(g), None) => {
            if g.voxel() != voxel || !g.grid().levels().is_empty() {
                return None;
            }
            for k in g.grid().brick_keys() {
                boxes.push((IVec3::from(*k) * 8, IVec3::from(*k) * 8 + 7));
            }
        }
    }
    // A point's value reaches the vertices of the cells around it and
    // their normals (two cells), so the edges two points away.
    Some(chunks_of(&boxes, 2))
}

/// Calls `f` with each brick key whose brick differs (by identity).
fn changed(a: &super::DetailGrid, b: &super::DetailGrid, mut f: impl FnMut([i32; 3])) {
    for k in a.brick_keys() {
        if !b.brick(k).is_some_and(|x| Arc::ptr_eq(x, a.brick(k).expect("listed"))) {
            f(*k);
        }
    }
    for k in b.brick_keys() {
        if a.brick(k).is_none() {
            f(*k);
        }
    }
}

/// The chunks owning an edge within `margin` points of the lattice boxes.
pub fn chunks_of(boxes: &[(IVec3, IVec3)], margin: i32) -> Vec<ChunkKey> {
    let mut out: Vec<ChunkKey> = Vec::new();
    for (lo, hi) in boxes {
        let (a, b) = ((*lo - margin).div_euclid(IVec3::splat(CHUNK)), (*hi + margin).div_euclid(IVec3::splat(CHUNK)));
        for z in a.z..=b.z {
            for y in a.y..=b.y {
                for x in a.x..=b.x {
                    out.push([x, y, z]);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
#[path = "chunks_tests.rs"]
mod tests;
