//! Painting a Poly's tiles (docs/QUALITY_PLAN.md Stage 4b): strokes
//! resolved as clay paint strokes are (`sculpt::prepare`: falloff,
//! pressure, symmetry, frames), applied in UV space.
//!
//! Strokes are spatial. A texel is painted when its point on the Poly's
//! shown level lies under a dab (the dab's sweep and falloff, as clay
//! paint weighs grid points) and its face turns the same way as the
//! surface under the dab (so a thin part's far side is left alone). Each
//! dab finds the shown level's faces near it, and each face's fan
//! triangles are rasterized in UV space at the channel's resolution, the
//! texel's 3D point interpolated over the triangle. Texels up to `GUTTER`
//! texels outside a face's UV triangle are painted too (their point
//! extrapolated over its plane), so bilinear reads at a chart's border
//! find paint and not the empty gap; a texel inside a face never gives
//! way to one that is only in another's gutter. Both sides of a UV seam
//! see the same 3D dab, so they are painted alike.
//!
//! Records keep the resolved dabs, the channel, the resolution, the value,
//! the shown level and the UV layout they were painted on, hashed into
//! their digest (§0.2 rule 3). `replay` applies them again on the Poly as
//! it is (after a new unwrap, the same strokes land in the new layout).
//! Everything runs in dab, face and texel order on one thread: the same
//! texels on any thread count.

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use super::{Channel, MAX_PAGES, Textures, TexturesFile, check_resolution, tile_of, tile_origin};
use crate::brush::curve::Curve;
use crate::brush::resolve::{Dab, Resolved};
use crate::poly::Poly;

/// Texels painted outside a face's UV triangle (bilinear reads at a
/// chart's border then find paint).
pub const GUTTER: f32 = 1.5;
/// A page's size, for messages.
const PAGE_KIB: u64 = super::PAGE_BYTES >> 10;
/// Most records a Poly keeps.
pub const MAX_RECORDS: usize = 16_384;
/// Most texels one call may visit (rasterized, painted or not).
pub const MAX_VISITED: u64 = 64_000_000;
/// Grid cells a face may be listed in before it is kept aside (tested by
/// every dab instead).
const MAX_FACE_CELLS: usize = 512;

/// The Poly's shown level, ready to paint: its points, faces, UVs per
/// corner and face normals, with a grid of faces by place.
pub struct Surface {
    points: Vec<Vec3>,
    face_start: Vec<u32>,
    corners: Vec<u32>,
    uv: Vec<[f32; 2]>,
    normals: Vec<Vec3>,
    lo: Vec<Vec3>,
    hi: Vec<Vec3>,
    cell: f32,
    grid: HashMap<[i32; 3], Vec<u32>>,
    /// Faces too big for the grid: every dab tests them.
    big: Vec<u32>,
    /// The surface's box (queries are clamped to it).
    bounds: (Vec3, Vec3),
    /// Every fan triangle (its first corner and its offset in the face),
    /// by tile and UV cell: which face owns a texel (`owned`).
    uv_grid: HashMap<(u16, i32, i32), Vec<u32>>,
    /// Triangles too big for the UV grid, in a coarse grid of
    /// `COARSE` x `COARSE` cells a tile (their boxes clamped to the tile).
    uv_big: HashMap<(u16, i32, i32), Vec<u32>>,
    /// Each fan triangle's corners and tile (`u16::MAX`: off the tiles),
    /// face by face in fan order; a face's first is at `tri_start`.
    tris: Vec<([u32; 3], u16)>,
    tri_start: Vec<u32>,
    uv_cell: f32,
    pub level: u8,
}

impl Surface {
    /// Level `level` of `poly` (with its displacement) and its UVs.
    pub fn new(poly: &Poly, level: u8) -> Result<Surface, String> {
        let uv = crate::poly::uv::at_level(poly, level).ok_or("painting a Poly needs UVs: run poly uv first (udim: true spreads them over tiles)")?;
        let s = crate::poly::subdiv::evaluate(poly, level)?;
        let faces = s.face_start.len() - 1;
        let mut normals = Vec::with_capacity(faces);
        let (mut lo, mut hi) = (Vec::with_capacity(faces), Vec::with_capacity(faces));
        let mut edge = 0.0f64;
        for w in s.face_start.windows(2) {
            let face = &s.corners[w[0] as usize..w[1] as usize];
            normals.push(crate::poly::subdiv::newell(&s.points, face).normalize_or_zero());
            let (a, b) = face.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), &c| (a.min(s.points[c as usize]), b.max(s.points[c as usize])));
            edge += (b - a).max_element() as f64;
            lo.push(a);
            hi.push(b);
        }
        let cell = ((edge / faces.max(1) as f64) as f32 * 2.0).max(1e-6);
        let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        let mut big = Vec::new();
        for f in 0..faces {
            let (a, b) = ((lo[f] / cell).floor().as_ivec3(), (hi[f] / cell).floor().as_ivec3());
            let n = (b - a + 1).as_i64vec3();
            if n.x * n.y * n.z > MAX_FACE_CELLS as i64 || !lo[f].is_finite() || !hi[f].is_finite() {
                big.push(f as u32);
                continue;
            }
            for z in a.z..=b.z {
                for y in a.y..=b.y {
                    for x in a.x..=b.x {
                        grid.entry([x, y, z]).or_default().push(f as u32);
                    }
                }
            }
        }
        let bounds = lo.iter().zip(&hi).filter(|(a, b)| a.is_finite() && b.is_finite()).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), (l, h)| (a.min(*l), b.max(*h)));
        // The fan triangles in UV space, by tile (as `apply` rasterizes
        // them: a triangle's tile is its UV middle's).
        let mut tris = Vec::new();
        let mut tri_start = Vec::with_capacity(faces);
        let mut extent = 0.0f64;
        for w in s.face_start.windows(2) {
            let (a, e) = (w[0] as usize, w[1] as usize);
            tri_start.push(tris.len() as u32);
            for j in 1..e - a - 1 {
                let ids = [a, a + j, a + j + 1];
                let t = ids.map(|c| uv[c]);
                let mid = [(t[0][0] + t[1][0] + t[2][0]) / 3.0, (t[0][1] + t[1][1] + t[2][1]) / 3.0];
                let Some(tile) = tile_of(mid) else {
                    tris.push((ids.map(|c| c as u32), u16::MAX));
                    continue;
                };
                let (l, h) = uv_box(&t);
                if l[0].is_finite() && h[0].is_finite() && l[1].is_finite() && h[1].is_finite() {
                    extent += (h[0] - l[0]).max(h[1] - l[1]) as f64;
                }
                tris.push((ids.map(|c| c as u32), tile));
            }
        }
        let uv_cell = ((extent / tris.len().max(1) as f64) as f32 * 2.0).clamp(1e-5, 1.0);
        let mut uv_grid: HashMap<(u16, i32, i32), Vec<u32>> = HashMap::new();
        let mut uv_big: HashMap<(u16, i32, i32), Vec<u32>> = HashMap::new();
        let mut coarse_entries = 0usize;
        for (k, (ids, tile)) in tris.iter().enumerate() {
            if *tile == u16::MAX {
                continue;
            }
            let o = tile_origin(*tile);
            let t = ids.map(|c| [uv[c as usize][0] - o[0], uv[c as usize][1] - o[1]]);
            let (l, h) = uv_box(&t);
            let span = |a: f32, b: f32| ((a / uv_cell).floor() as i64, (b / uv_cell).floor() as i64);
            let ((x0, x1), (y0, y1)) = (span(l[0].max(-1.0), h[0].min(2.0)), span(l[1].max(-1.0), h[1].min(2.0)));
            if !(l[0].is_finite() && h[0].is_finite() && l[1].is_finite() && h[1].is_finite()) || (x1 - x0 + 1) * (y1 - y0 + 1) > MAX_FACE_CELLS as i64 {
                // Texels lie in 0-1 of their tile: the coarse cells the box
                // meets there (none for a box off the tile).
                let cells = |a: f32, b: f32| -> Option<(i32, i32)> {
                    let (a, b) = (if a.is_finite() { a.max(0.0) } else { 0.0 }, if b.is_finite() { b.min(1.0) } else { 1.0 });
                    (a <= b).then(|| (((a * COARSE as f32) as i32).min(COARSE - 1), ((b * COARSE as f32) as i32).min(COARSE - 1)))
                };
                let (Some((cx0, cx1)), Some((cy0, cy1))) = (cells(l[0], h[0]), cells(l[1], h[1])) else { continue };
                coarse_entries += ((cx1 - cx0 + 1) * (cy1 - cy0 + 1)) as usize;
                if coarse_entries > MAX_COARSE_ENTRIES {
                    return Err(format!("the UV layout has too many large overlapping triangles to paint ({MAX_COARSE_ENTRIES} coarse cell entries); unwrap it again (poly uv)"));
                }
                for y in cy0..=cy1 {
                    for x in cx0..=cx1 {
                        uv_big.entry((*tile, x, y)).or_default().push(k as u32);
                    }
                }
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    uv_grid.entry((*tile, x as i32, y as i32)).or_default().push(k as u32);
                }
            }
        }
        Ok(Surface { points: s.points, face_start: s.face_start, corners: s.corners, uv, normals, lo, hi, cell, grid, big, bounds, uv_grid, uv_big, tris, tri_start, uv_cell, level })
    }

    /// Whether the texel centre `q` (texels, within `tile` at `r` texels a
    /// side) lies inside a fan triangle other than `own`: a texel inside
    /// any face belongs to that face, never to another's gutter.
    /// Each triangle tested is counted into `work` (the call's visit
    /// budget).
    fn owned(&self, tile: u16, q: Vec2, r: f32, own: u32, work: &mut u64) -> bool {
        let o = tile_origin(tile);
        let uv = [q.x / r, q.y / r];
        let key = (tile, (uv[0] / self.uv_cell).floor() as i32, (uv[1] / self.uv_cell).floor() as i32);
        let coarse = (tile, ((uv[0] * COARSE as f32) as i32).clamp(0, COARSE - 1), ((uv[1] * COARSE as f32) as i32).clamp(0, COARSE - 1));
        let list = self.uv_grid.get(&key).into_iter().flatten().chain(self.uv_big.get(&coarse).into_iter().flatten());
        for &k in list {
            *work += 1;
            // Past the budget: the caller refuses the call (see `apply_within`).
            if *work > MAX_VISITED {
                return true;
            }
            if k == own {
                continue;
            }
            let (ids, t) = self.tris[k as usize];
            if t != tile {
                continue;
            }
            let px = ids.map(|c| Vec2::new((self.uv[c as usize][0] - o[0]) * r, (self.uv[c as usize][1] - o[1]) * r));
            if locate(q, px[0], px[1], px[2]).is_some_and(|(d, _)| d == 0.0) {
                return true;
            }
        }
        false
    }

    /// Faces whose boxes meet the box `lo..hi`, in face order.
    fn faces_near(&self, lo: Vec3, hi: Vec3) -> Vec<u32> {
        // Clamped to the surface's own box first (a far or huge query box,
        // from a dab or a loaded record, costs no more than the surface),
        // and the cell span worked out in i64 (no i32 overflow).
        if !(lo.is_finite() && hi.is_finite()) {
            return Vec::new();
        }
        let (lo, hi) = (lo.max(self.bounds.0), hi.min(self.bounds.1));
        if lo.cmpgt(hi).any() {
            return Vec::new();
        }
        let cell = |v: Vec3| [v.x, v.y, v.z].map(|x| (x as f64 / self.cell as f64).floor() as i64);
        let (a, b) = (cell(lo), cell(hi));
        let span: Option<i64> = (0..3).try_fold(1i64, |n, k| {
            let d = b[k].checked_sub(a[k])?.checked_add(1)?;
            (d > 0).then_some(())?;
            n.checked_mul(d)
        });
        let mut out: Vec<u32> = if span.is_none_or(|n| n > self.grid.len() as i64) {
            (0..self.normals.len() as u32).collect()
        } else {
            let (a, b) = (a.map(|x| x as i32), b.map(|x| x as i32));
            let (a, b) = (glam::IVec3::from(a), glam::IVec3::from(b));
            let mut v = Vec::new();
            for z in a.z..=b.z {
                for y in a.y..=b.y {
                    for x in a.x..=b.x {
                        if let Some(list) = self.grid.get(&[x, y, z]) {
                            v.extend_from_slice(list);
                        }
                    }
                }
            }
            v.extend_from_slice(&self.big);
            v
        };
        out.sort_unstable();
        out.dedup();
        out.retain(|&f| self.lo[f as usize].cmple(hi).all() && self.hi[f as usize].cmpge(lo).all());
        out
    }

    /// The nearest point of the surface to `p` and its face's normal
    /// (searching ever wider boxes around `p`); None for an empty one.
    pub fn snap(&self, p: Vec3) -> Option<(Vec3, Vec3)> {
        if !p.is_finite() || self.normals.is_empty() {
            return None;
        }
        let (lo, hi) = self.bounds;
        let far = (hi - lo).length() + p.distance(lo.max(p.min(hi))) + self.cell;
        let mut reach = self.cell;
        // Doubling from a cell passes any finite `far` well within 64
        // rounds; a point near f32's end makes `far` infinite.
        for _ in 0..64 {
            let mut best: Option<(f32, Vec3, Vec3)> = None;
            for f in self.faces_near(p - reach, p + reach) {
                let (s, e) = (self.face_start[f as usize] as usize, self.face_start[f as usize + 1] as usize);
                for j in 1..e - s - 1 {
                    let [a, b, c] = [s, s + j, s + j + 1].map(|k| self.points[self.corners[k] as usize]);
                    let q = crate::film::dice::closest_on_triangle(p, a, b, c);
                    let d = q.distance(p);
                    if best.is_none_or(|x| d < x.0) {
                        best = Some((d, q, self.normals[f as usize]));
                    }
                }
            }
            if let Some(b) = best.filter(|b| b.0 <= reach) {
                return Some((b.1, b.2));
            }
            if reach > far || !reach.is_finite() {
                return best.map(|b| (b.1, b.2));
            }
            reach *= 2.0;
        }
        // Nearest to the surface's box instead (all of it in reach).
        let q = p.clamp(lo, hi);
        (q != p && q.is_finite()).then(|| self.snap(q)).flatten()
    }
}

/// A UV triangle's box.
fn uv_box(t: &[[f32; 2]; 3]) -> ([f32; 2], [f32; 2]) {
    t.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(a, b), p| ([a[0].min(p[0]), a[1].min(p[1])], [b[0].max(p[0]), b[1].max(p[1])]))
}

/// A dab's path points and segments by place (cells of its largest
/// radius): the segments that can reach a point, and its nearest path
/// point, without scanning the whole path for every texel.
pub(super) struct PathIndex {
    cell: f32,
    segments: HashMap<[i32; 3], Vec<u32>>,
    points: HashMap<[i32; 3], Vec<u32>>,
}

impl PathIndex {
    pub(super) fn new(points: &[Vec3], reach: f32) -> PathIndex {
        let cell = reach.max(1e-6);
        let key = |p: Vec3| (p / cell).floor().as_ivec3().to_array();
        let mut segments: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        let mut at: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (k, p) in points.iter().enumerate() {
            at.entry(key(*p)).or_default().push(k as u32);
        }
        for (k, w) in points.windows(2).enumerate() {
            let (a, b) = (key(w[0].min(w[1]) - reach), key(w[0].max(w[1]) + reach));
            let n = (0..3).map(|i| (b[i] as i64 - a[i] as i64 + 1).max(1)).product::<i64>();
            if n > 4096 {
                // A segment long against the radius: listed under every
                // cell of its box would be too many; keep it everywhere.
                segments.entry([i32::MIN; 3]).or_default().push(k as u32);
                continue;
            }
            for z in a[2]..=b[2] {
                for y in a[1]..=b[1] {
                    for x in a[0]..=b[0] {
                        segments.entry([x, y, z]).or_default().push(k as u32);
                    }
                }
            }
        }
        PathIndex { cell, segments, points: at }
    }

    /// The segments (ascending) that may lie within the reach of `p`.
    pub(super) fn segments_near(&self, p: Vec3) -> Vec<u32> {
        let k = (p / self.cell).floor().as_ivec3().to_array();
        let mut out: Vec<u32> = self.segments.get(&k).into_iter().flatten().chain(self.segments.get(&[i32::MIN; 3]).into_iter().flatten()).copied().collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The nearest path point to `p` (the first on a tie), as a full scan
    /// would find it.
    pub(super) fn nearest(&self, p: Vec3, points: &[Vec3]) -> usize {
        let k = (p / self.cell).floor().as_ivec3();
        let mut best: Option<(f32, usize)> = None;
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    for &i in self.points.get(&(k + glam::IVec3::new(dx, dy, dz)).to_array()).into_iter().flatten() {
                        let d = points[i as usize].distance_squared(p);
                        if best.is_none_or(|b| d < b.0 || (d == b.0 && (i as usize) < b.1)) {
                            best = Some((d, i as usize));
                        }
                    }
                }
            }
        }
        // Exact when the best is within a cell (anything nearer is in the
        // cells searched); else the full scan.
        match best {
            Some((d, i)) if d.sqrt() <= self.cell => i,
            _ => points.iter().enumerate().map(|(i, q)| (q.distance_squared(p), i)).min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))).map_or(0, |x| x.1),
        }
    }
}

/// A texel a dab may paint: (tile, x, y) packed, its distance outside
/// its face (texels; 0 inside), its order, and its weight (negative: the
/// face it belongs to does not take this dab).
type Candidate = (u64, f32, u32, f32);

/// A fan triangle a dab rasterizes: its face, its place in the fan, its
/// tile, its corners in texels and its texel box (x0, y0, x1, y1).
type TriBox = (u32, u32, u16, [Vec2; 3], [u32; 4]);

fn pack(tile: u16, x: u32, y: u32) -> u64 {
    (tile as u64) << 32 | (y as u64) << 16 | x as u64
}

fn unpack(k: u64) -> (u16, u32, u32) {
    ((k >> 32) as u16, (k & 0xffff) as u32, ((k >> 16) & 0xffff) as u32)
}

/// What a stroke lays.
#[derive(Clone, Copy, Debug)]
pub struct Ink {
    pub channel: Channel,
    pub resolution: u32,
    /// RGB (sRGB for color; a scalar channel's value in all three).
    pub value: [f32; 3],
    pub erase: bool,
    /// Opacity at the dab's middle, 0-1.
    pub opacity: f32,
}

/// Distance (texels) from `q` to triangle `abc`, 0 inside, and its
/// barycentric weights (unclamped); None for a triangle with no area.
fn locate(q: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Option<(f32, [f32; 3])> {
    let (v0, v1, v2) = (b - a, c - a, q - a);
    let den = v0.perp_dot(v1);
    if !(den.abs() > 1e-12) {
        return None;
    }
    let w1 = v2.perp_dot(v1) / den;
    let w2 = v0.perp_dot(v2) / den;
    let w = [1.0 - w1 - w2, w1, w2];
    if w.iter().all(|x| *x >= 0.0) {
        return Some((0.0, w));
    }
    let seg = |p: Vec2, s: Vec2, e: Vec2| {
        let d = e - s;
        let t = if d.length_squared() > 0.0 { ((p - s).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
        p.distance(s + d * t)
    };
    Some((seg(q, a, b).min(seg(q, b, c)).min(seg(q, c, a)), w))
}

/// Lays one value over a texel with weight `w` (straight alpha), or
/// takes `w` of its coverage away.
fn lay(t: &mut [u16; 4], ink: &Ink, w: f32) {
    let f = |v: u16| v as f32 / 65535.0;
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let a = f(t[3]);
    if ink.erase {
        t[3] = q(a * (1.0 - w));
        // Wholly erased: no colour left either (so a page can empty).
        if t[3] == 0 {
            *t = [0; 4];
        }
        return;
    }
    let out = w + a * (1.0 - w);
    if out > 0.0 {
        for (v, ink) in t.iter_mut().zip(ink.value) {
            *v = q((ink.clamp(0.0, 1.0) * w + f(*v) * a * (1.0 - w)) / out);
        }
    }
    t[3] = q(out);
}

/// Applies resolved dabs to `textures` (see the module doc). Returns the
/// texels painted; refused (nothing written by that dab) past the page
/// budget or `visited`'s bound.
#[cfg(test)]
pub fn apply(textures: &mut Textures, surface: &Surface, ink: &Ink, curve: &Curve, dabs: &[Dab], visited: &mut u64) -> Result<usize, String> {
    apply_within(textures, surface, ink, curve, dabs, visited, MAX_PAGES, None)
}

/// Coarse UV cells a tile side, for triangles too big for the fine grid.
const COARSE: i32 = 64;
/// Most coarse cell entries a surface keeps (refused past it).
const MAX_COARSE_ENTRIES: usize = 16_000_000;

/// Paths of at most this many points are scanned whole per texel.
const SHORT_PATH: usize = 16;

/// Texels' boxes one dab may rasterize (its faces' UV boxes, counted
/// before anything is allocated).
pub const MAX_DAB_TEXELS: u64 = 16_000_000;

/// `apply` with a page budget of `max_pages` (the tests' smaller one),
/// checking `progress` (cancel) before each dab.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_within(textures: &mut Textures, surface: &Surface, ink: &Ink, curve: &Curve, dabs: &[Dab], visited: &mut u64, max_pages: usize, progress: Option<&crate::progress::Progress>) -> Result<usize, String> {
    check_resolution(ink.resolution)?;
    textures.set_resolution(ink.channel, ink.resolution)?;
    let r = ink.resolution as f32;
    let mut touched = 0usize;
    for dab in dabs {
        if let Some(p) = progress {
            p.check()?;
        }
        dab.check()?;
        if dab.points.is_empty() || !dab.points.iter().all(|p| p.is_finite()) || !(dab.radius.is_finite() && dab.radius > 0.0) {
            return Err("dab points and radius must be finite, radius > 0".into());
        }
        let sweep = crate::sculpt::engine::Sweep { points: &dab.points, radius: dab.radius, radii: dab.radii.clone(), gains: dab.gains.as_deref() };
        let reach = sweep.reach();
        let (lo, hi) = dab.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        let (lo, hi) = (lo - reach, hi + reach);
        let faces = surface.faces_near(lo, hi);
        if faces.is_empty() {
            continue;
        }
        // Each fan triangle's texel box, counted before any allocation:
        // a dab (or the call) that would rasterize too many is refused
        // from the layout alone.
        let mut boxes: Vec<TriBox> = Vec::new();
        let mut estimate = 0u64;
        for &f in &faces {
            let (s, e) = (surface.face_start[f as usize] as usize, surface.face_start[f as usize + 1] as usize);
            for j in 1..e - s - 1 {
                let ids = [s, s + j, s + j + 1];
                let uv = ids.map(|c| surface.uv[c]);
                let mid = [(uv[0][0] + uv[1][0] + uv[2][0]) / 3.0, (uv[0][1] + uv[1][1] + uv[2][1]) / 3.0];
                let Some(tile) = tile_of(mid) else { continue };
                let o = tile_origin(tile);
                let px = uv.map(|t| Vec2::new((t[0] - o[0]) * r, (t[1] - o[1]) * r));
                let (a, b) = (px[0].min(px[1]).min(px[2]) - GUTTER, px[0].max(px[1]).max(px[2]) + GUTTER);
                let (x0, y0) = ((a.x - 0.5).ceil().max(0.0), (a.y - 0.5).ceil().max(0.0));
                let (x1, y1) = ((b.x - 0.5).floor().min(r - 1.0), (b.y - 0.5).floor().min(r - 1.0));
                if !(x0 <= x1 && y0 <= y1) {
                    continue;
                }
                estimate += (x1 - x0 + 1.0) as u64 * (y1 - y0 + 1.0) as u64;
                boxes.push((f, j as u32, tile, px, [x0 as u32, y0 as u32, x1 as u32, y1 as u32]));
            }
        }
        if estimate > MAX_DAB_TEXELS {
            return Err(format!("one dab would rasterize {estimate} texels, more than {MAX_DAB_TEXELS}; a smaller radius, shorter strokes or a lower resolution"));
        }
        *visited += estimate;
        if *visited > MAX_VISITED {
            return Err(format!("the strokes cover more than {MAX_VISITED} texels in one call; split them into smaller calls or a lower resolution"));
        }
        let index = (dab.points.len() > SHORT_PATH).then(|| PathIndex::new(&dab.points, reach));
        // Which way the surface under each point of the path turns: the
        // face of its nearest surface point (a thin part's far side is
        // not nearest).
        let path_normals: Vec<Option<Vec3>> = dab.points.iter().map(|p| surface.snap(*p).map(|s| s.1)).collect();
        // Each texel's point: the face that holds it, else the nearest
        // gutter not inside any other face (first face on a tie).
        let mut cand: Vec<Candidate> = Vec::new();
        let mut order = 0u32;
        for &(f, j, tile, px, [x0, y0, x1, y1]) in &boxes {
            // Cancel between triangles; ownership tests count into the
            // call's visit budget too.
            if let Some(p) = progress {
                p.check()?;
            }
            if *visited > MAX_VISITED {
                return Err(format!("the strokes cover more than {MAX_VISITED} texels in one call; split them into smaller calls or a lower resolution"));
            }
            let s = surface.face_start[f as usize] as usize;
            let j = j as usize;
            let n = surface.normals[f as usize];
            let own = surface.tri_start[f as usize] + j as u32 - 1;
            let p3 = [s, s + j, s + j + 1].map(|c| surface.points[surface.corners[c] as usize]);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let q = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let Some((d, w)) = locate(q, px[0], px[1], px[2]) else { continue };
                    if d > GUTTER {
                        continue;
                    }
                    // A gutter texel inside another face is that face's.
                    if d > 0.0 && surface.owned(tile, q, r, own, visited) {
                        continue;
                    }
                    let p = p3[0] * w[0] + p3[1] * w[1] + p3[2] * w[2];
                    // Faces the way the surface under the nearest path point does.
                    // A short path is scanned whole; a long one through
                    // its index (the same answers, `PathIndex`).
                    let near = match &index {
                        Some(ix) => ix.nearest(p, &dab.points),
                        None => dab.points.iter().enumerate().map(|(i, q)| (q.distance_squared(p), i)).min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))).map_or(0, |x| x.1),
                    };
                    let weight = if path_normals[near].is_some_and(|m| m.dot(n) <= 0.0) {
                        -1.0
                    } else {
                        let w = match &index {
                            Some(ix) => sweep.weight_among(curve, p, &ix.segments_near(p)),
                            None => sweep.weight(curve, p),
                        };
                        (w * ink.opacity * dab.gain).min(1.0)
                    };
                    // A texel inside its face is that face's alone: one
                    // that takes nothing needs no place in the list.
                    if d == 0.0 && weight <= 0.0 {
                        continue;
                    }
                    cand.push((pack(tile, x, y), d, order, weight));
                    order += 1;
                }
            }
        }
        if *visited > MAX_VISITED {
            return Err(format!("the strokes cover more than {MAX_VISITED} texels in one call; split them into smaller calls or a lower resolution"));
        }
        // Per texel the nearest face (inside first), the first on a tie.
        cand.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        cand.dedup_by(|b, a| a.0 == b.0);
        let mut writes: Vec<((u16, u32, u32), f32)> = cand.into_iter().filter(|c| c.3 > 0.0).map(|c| (unpack(c.0), c.3)).collect();
        // Erasing where nothing was painted makes no page.
        if ink.erase {
            writes.retain(|(k, _)| textures.has_page(ink.channel, k.0, k.1, k.2));
        }
        let new = textures.new_pages(ink.channel, writes.iter().map(|w| w.0));
        if textures.pages() + new > max_pages {
            return Err(format!("painting this would hold more than {max_pages} texture pages of {PAGE_KIB} KiB ({:.1} GiB); paint less area or a lower resolution", max_pages as f64 * super::PAGE_BYTES as f64 / (1u64 << 30) as f64));
        }
        for &((tile, x, y), w) in &writes {
            lay(textures.texel_mut(ink.channel, tile, x, y), ink, w);
            touched += 1;
        }
        // Pages an erase emptied are dropped (an empty page and none are
        // the same texels).
        if ink.erase {
            textures.drop_empty(ink.channel, writes.iter().map(|w| w.0));
        }
    }
    Ok(touched)
}

/// A painted Poly and what each stroke did.
pub struct Painted {
    pub poly: Poly,
    /// Texels each stroke painted, and its record's id.
    pub touched: Vec<usize>,
    pub ids: Vec<u64>,
}

/// Paints resolved strokes on `poly` (its shown level as `surface`) into
/// `channel` at `resolution` (`clear` first drops the channel's texels
/// and records), each stroke's colour or, on a scalar channel, `value`;
/// records them. Refused before the Poly changes (it is returned new).
#[cfg(test)]
pub fn paint_poly(poly: &Poly, surface: &Surface, channel: Channel, resolution: u32, value: Option<f32>, clear: bool, strokes: Vec<Resolved>) -> Result<Painted, String> {
    paint_poly_with(poly, surface, channel, resolution, value, clear, strokes, None)
}

/// `paint_poly`, cancelled through `progress` between dabs (a job's).
#[allow(clippy::too_many_arguments)]
pub fn paint_poly_with(poly: &Poly, surface: &Surface, channel: Channel, resolution: u32, value: Option<f32>, clear: bool, strokes: Vec<Resolved>, progress: Option<&crate::progress::Progress>) -> Result<Painted, String> {
    if !channel.paintable() {
        return Err(format!("strokes paint color, roughness or mask, not {}", channel.name()));
    }
    check_resolution(resolution)?;
    let mut paint = poly.paint.as_deref().cloned().unwrap_or_default();
    if clear {
        paint.textures.clear(Some(channel));
        paint.records.retain(|r| r.channel != channel);
    }
    let layout = layout_hash(poly);
    let (mut touched, mut ids) = (Vec::new(), Vec::new());
    let mut visited = 0u64;
    for (n, r) in strokes.into_iter().enumerate() {
        let p = r.paint.clone().ok_or_else(|| format!("stroke {n}: a paint stroke needs color (or erase: true)"))?;
        if paint.records.len() >= MAX_RECORDS {
            return Err(format!("the Poly keeps at most {MAX_RECORDS} paint strokes; clear a channel first"));
        }
        let lays = match value {
            Some(v) if channel.scalar() => [v.clamp(0.0, 1.0); 3],
            _ => p.color.clamp(Vec3::ZERO, Vec3::ONE).to_array(),
        };
        let mut record = Record { id: paint.next_id(), channel, resolution, value: lays, level: surface.level, layout: layout.clone(), stroke: r, digest: String::new() };
        record.digest = record.digest();
        let ink = Ink { channel, resolution, value: lays, erase: p.erase, opacity: p.opacity };
        touched.push(apply_within(&mut paint.textures, surface, &ink, &p.curve, &record.stroke.dabs, &mut visited, MAX_PAGES, progress).map_err(|e| if progress.is_some_and(|p| p.cancelled()) { e } else { format!("stroke {n}: {e}") })?);
        ids.push(record.id);
        paint.records.push(record);
    }
    let mut out = poly.clone();
    out.paint = Some(std::sync::Arc::new(paint)).filter(|p| !p.is_empty());
    Ok(Painted { poly: out, touched, ids })
}

/// A paint stroke on a Poly, as it applies.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    /// Unique among the Poly's records.
    pub id: u64,
    pub channel: Channel,
    pub resolution: u32,
    /// What it lays: RGB (a scalar channel's value in all three).
    pub value: [f32; 3],
    /// The shown level it was painted on and the hash of the UVs it was
    /// painted in (`layout_hash`).
    pub level: u8,
    pub layout: String,
    pub stroke: Resolved,
    /// `digest()` when it was made (checked on load).
    pub digest: String,
}

impl Record {
    /// Everything that changes what it writes, hashed: the resolved
    /// stroke (dabs, falloff, opacity), the channel, the resolution, the
    /// value, the level and the UV layout.
    pub fn digest(&self) -> String {
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        h.update(b"poly_paint");
        h.update(&crate::ledger::stroke_digest(&self.stroke).0.to_le_bytes());
        h.update(self.channel.name().as_bytes());
        h.update(&self.resolution.to_le_bytes());
        for v in self.value {
            h.update(&v.to_bits().to_le_bytes());
        }
        h.update(&[self.level]);
        h.update(self.layout.as_bytes());
        format!("{:032x}", h.digest128())
    }

    pub fn ink(&self) -> Result<(Ink, Curve), String> {
        let p = self.stroke.paint.as_ref().ok_or("a Poly paint record needs paint parameters")?;
        Ok((Ink { channel: self.channel, resolution: self.resolution, value: self.value, erase: p.erase, opacity: p.opacity }, p.curve.clone()))
    }
}

/// The hash of a Poly's UVs (what a record was painted in).
pub fn layout_hash(poly: &Poly) -> String {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for c in poly.uv.iter().flat_map(|u| u.chunks()) {
        h.update(bytemuck::cast_slice(c));
    }
    format!("{:032x}", h.digest128())
}

/// A Poly's paint: its texture channels and the strokes that painted
/// them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolyPaint {
    pub textures: Textures,
    pub records: Vec<Record>,
    /// Some texels came from a bake or an import, not from the records
    /// (a new unwrap drops them).
    pub imported: bool,
}

impl PolyPaint {
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty() && self.records.is_empty()
    }

    pub fn next_id(&self) -> u64 {
        self.records.iter().map(|r| r.id).max().map_or(1, |m| m + 1)
    }
}

/// Every record applied again, in order, to empty textures on `poly` as
/// it is (its UVs and points), each on the level it was painted on (the
/// finest kept, if that level is gone).
#[cfg(test)]
pub fn replay(poly: &Poly, records: &[Record]) -> Result<Textures, String> {
    let mut out = Textures::default();
    let mut surfaces: std::collections::BTreeMap<u8, Surface> = std::collections::BTreeMap::new();
    for r in records {
        let level = r.level.min(poly.level_count());
        if let std::collections::btree_map::Entry::Vacant(e) = surfaces.entry(level) {
            e.insert(Surface::new(poly, level)?);
        }
        let (ink, curve) = r.ink()?;
        // Each record is bounded on its own, as when it was painted.
        let mut visited = 0u64;
        apply(&mut out, &surfaces[&level], &ink, &curve, &r.stroke.dabs, &mut visited)?;
    }
    Ok(out)
}

/// `replay` for a new unwrap: a record the new layout cannot hold (past
/// a page or texel bound there) is left out and named, and the rest are
/// kept, so the unwrap still lands; `progress` cancels it between dabs.
/// Returns the texels, the records kept and a note per one left out.
pub fn replay_kept(poly: &Poly, records: &[Record], progress: &crate::progress::Progress) -> Result<(Textures, Vec<Record>, Vec<String>), String> {
    let mut out = Textures::default();
    let mut surfaces: std::collections::BTreeMap<u8, Surface> = std::collections::BTreeMap::new();
    let (mut kept, mut notes) = (Vec::new(), Vec::new());
    for (n, r) in records.iter().enumerate() {
        progress.check()?;
        progress.set(0.8 + 0.2 * n as f32 / records.len().max(1) as f32);
        let level = r.level.min(poly.level_count());
        if let std::collections::btree_map::Entry::Vacant(e) = surfaces.entry(level) {
            e.insert(Surface::new(poly, level)?);
        }
        let (ink, curve) = r.ink()?;
        // Pages are shared: the snapshot costs nothing, and a record that
        // fails part way leaves no trace.
        let before = out.clone();
        let mut visited = 0u64;
        match apply_within(&mut out, &surfaces[&level], &ink, &curve, &r.stroke.dabs, &mut visited, MAX_PAGES, Some(progress)) {
            Ok(_) => kept.push(r.clone()),
            Err(e) if progress.cancelled() => return Err(e),
            Err(e) => {
                out = before;
                notes.push(format!("paint stroke {} was left out: in the new layout {e}", r.id));
            }
        }
    }
    Ok((out, kept, notes))
}

/// Records reflected across `axis` (a mirror copy's), in the copy's
/// layout (`layout`), their digests made again.
pub fn mirrored(records: &[Record], axis: usize, layout: &str) -> Vec<Record> {
    let flip = |v: &mut Vec3| v[axis] = -v[axis];
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            for d in r.stroke.dabs.iter_mut().chain(r.stroke.paint_dabs.iter_mut().flatten()) {
                d.points.iter_mut().for_each(flip);
                for v in [&mut d.delta, &mut d.normal, &mut d.tangent, &mut d.upright].into_iter().flatten() {
                    flip(v);
                }
                d.flip = !d.flip;
            }
            r.layout = layout.to_string();
            r.digest = r.digest();
            r
        })
        .collect()
}

/// Refuses records whose dabs lie far outside `lo..hi` (the Poly's cage
/// box): points more than the box's size outside it, or radii larger
/// than the box (a file's records are applied again on a new unwrap).
pub fn check_records(records: &[Record], lo: Vec3, hi: Vec3) -> Result<(), String> {
    let size = (hi - lo).length().max(1e-6);
    let (a, b) = (lo - size, hi + size);
    for r in records {
        for d in r.stroke.dabs.iter().chain(r.stroke.paint_dabs.iter().flatten()) {
            let inside = d.points.iter().all(|p| p.is_finite() && p.cmpge(a).all() && p.cmple(b).all());
            let radii = d.radius.is_finite() && d.radius <= size && d.radii.iter().flatten().all(|x| x.is_finite() && *x <= size);
            if !inside || !radii {
                return Err(format!("paint record {} has a dab far outside the Poly (points beyond its box by more than its size, or a radius larger than it)", r.id));
            }
        }
    }
    Ok(())
}

/// A Poly's paint as a file holds it.
#[derive(Serialize, Deserialize)]
pub struct PaintFile {
    textures: TexturesFile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    records: Vec<Record>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    imported: bool,
}

impl PolyPaint {
    pub fn to_file(&self) -> Result<PaintFile, String> {
        Ok(PaintFile { textures: self.textures.to_file()?, records: self.records.clone(), imported: self.imported })
    }
}

impl PaintFile {
    /// The paint, its records bounded and their digests checked.
    pub fn read(self) -> Result<PolyPaint, String> {
        if self.records.len() > MAX_RECORDS {
            return Err(format!("a Poly keeps at most {MAX_RECORDS} paint records"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for r in &self.records {
            check_resolution(r.resolution)?;
            if !r.channel.paintable() || !ids.insert(r.id) || !r.value.iter().all(|v| v.is_finite()) {
                return Err(format!("paint record {} is not a stroke on color, roughness or mask, repeats an id or has a value that is not finite", r.id));
            }
            for d in &r.stroke.dabs {
                d.check()?;
            }
            if r.stroke.paint.is_none() || r.digest() != r.digest {
                return Err(format!("paint record {} does not match its digest", r.id));
            }
        }
        let textures = self.textures.read()?;
        // One resolution per channel: the records' and the texels'.
        let mut res: std::collections::BTreeMap<Channel, u32> = std::collections::BTreeMap::new();
        for r in &self.records {
            let want = *res.entry(r.channel).or_insert_with(|| textures.resolution(r.channel).filter(|_| !textures.tiles(r.channel).is_empty()).unwrap_or(r.resolution));
            if want != r.resolution {
                return Err(format!("paint record {} is at {} texels a tile, but its {} channel is at {want}", r.id, r.resolution, r.channel.name()));
            }
        }
        Ok(PolyPaint { textures, records: self.records, imported: self.imported })
    }
}
