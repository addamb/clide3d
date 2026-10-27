//! Catmull-Clark subdivision with semi-sharp creases (D16; DeRose, Kass
//! and Truong 1998), generalised from `human::subdivide`: n-gons, borders
//! with the "edges and corners" rule, crease sharpness that decays by one
//! per level (fractional sharpness blends the smooth and sharp rules), and
//! limit positions.
//!
//! The rules, for a level's points P:
//! - a face point is the mean of the face's corners;
//! - an edge point is (a + b + f0 + f1) / 4 on a smooth edge and the
//!   midpoint on a sharp one (sharpness ≥ 1, or a border: an edge in other
//!   than two faces); between 0 and 1 it is the blend lerp(smooth, mid, s);
//! - a vertex point is the smooth rule (Q + 2R + (n − 3)v) / n with fewer
//!   than two sharp edges at it, the crease rule (a + 6v + b) / 8 with two,
//!   and stays (a corner) with three or more, or on a border in one face.
//!   Its sharpness is the mean of its sharp edges'; under 1 it blends the
//!   smooth rule into the sharp one by it.
//!
//! Children of an edge keep its sharpness less one; infinite (10 at the
//! tool, `INFINITE_SHARPNESS`) stays. Each output point is a pure function
//! of the level before, summed in the topology's fixed order, and the
//! passes run in parallel by chunks of outputs (`clay::par_chunks`), so a
//! level is bit-identical on any thread count.
//!
//! Level k is `CC(level k − 1) + F_k · D_k` (D17): `F_k` is the frame of
//! the undisplaced level-k mesh at each vertex (its area-weighted normal,
//! a tangent toward the next corner of the vertex's first face, and their
//! cross product) and `D_k` the level's tangent-space displacement. Fine
//! detail therefore turns with the surface when a coarse level moves.
//!
//! Since Stage 3c files hold displacements, so these frames, the point
//! order of `Topology::refine` and the rules above are part of the file
//! format: a change to any of them moves every saved projection. (Stage
//! 3c kept the level mesh's frames rather than limit-surface tangents:
//! any deterministic frame that turns with the surface stores detail
//! exactly, and displacement maps are written in their own UV tangent
//! frames, which other programs can rebuild, not in these.)

use glam::Vec3;

use super::topology::{Core, Topology};
use super::{Chunked, Counts, Level, Poly};
use crate::clay::par_chunks;

/// Outputs per parallel chunk.
const PAR: usize = 4096;

/// Counts of levels 0..=levels.
pub fn counts(t: &Topology, levels: u8) -> Vec<Counts> {
    let mut c = Counts { vertices: t.vertices as u64, edges: t.edges.len() as u64, faces: t.faces() as u64, corners: t.corners.len() as u64 };
    let mut out = vec![c];
    for _ in 0..levels {
        c = c.next();
        out.push(c);
    }
    out
}

/// A vertex's sharp edges: how many, the first two's other points, the
/// mean sharpness, and whether it is a corner.
struct Sharp {
    count: usize,
    ends: [u32; 2],
    mean: f32,
    corner: bool,
}

fn sharp_at(t: &Topology, v: usize) -> Sharp {
    let mut s = Sharp { count: 0, ends: [0; 2], mean: 0.0, corner: false };
    let edges = t.edges_of(v);
    for &e in edges {
        let k = t.sharpness_of(e as usize);
        if k > 0.0 {
            if s.count < 2 {
                s.ends[s.count] = t.other(e as usize, v as u32);
            }
            s.count += 1;
            s.mean += k;
        }
    }
    if s.count > 0 {
        s.mean /= s.count as f32;
    }
    // "Edges and corners": a border point in one face stays.
    s.corner = s.count >= 3 || (edges.len() == 2 && t.faces_of(v).len() == 1);
    s
}

/// The next level's points (in `Topology::refine`'s order: this level's
/// points moved, then face points, then edge points).
pub fn subdivide_points(t: &Topology, p: &[Vec3]) -> Vec<Vec3> {
    let (nv, nf) = (t.vertices, t.faces());
    let mut out = vec![Vec3::ZERO; nv + nf + t.edges.len()];
    let (verts, rest) = out.split_at_mut(nv);
    let (faces, edges) = rest.split_at_mut(nf);
    par_chunks(faces, PAR, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let face = t.face(i * PAR + k);
            *o = face.iter().map(|&c| p[c as usize]).sum::<Vec3>() / face.len() as f32;
        }
    });
    let fp: &[Vec3] = faces;
    par_chunks(edges, PAR, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let e = i * PAR + k;
            let [a, b] = t.edges[e];
            let (a, b) = (p[a as usize], p[b as usize]);
            let mid = (a + b) * 0.5;
            let s = t.sharpness_of(e);
            *o = if s >= 1.0 {
                mid
            } else {
                let fs = t.faces_of_edge(e);
                let smooth = (a + b + fp[fs[0] as usize] + fp[fs[1] as usize]) * 0.25;
                if s <= 0.0 { smooth } else { smooth.lerp(mid, s) }
            };
        }
    });
    par_chunks(verts, PAR, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            *o = vertex_point(t, p, fp, i * PAR + k);
        }
    });
    out
}

fn vertex_point(t: &Topology, p: &[Vec3], fp: &[Vec3], v: usize) -> Vec3 {
    let pv = p[v];
    let edges = t.edges_of(v);
    if edges.is_empty() {
        return pv;
    }
    let smooth = || {
        let faces = t.faces_of(v);
        let n = edges.len() as f32;
        let q = faces.iter().map(|&f| fp[f as usize]).sum::<Vec3>() / faces.len() as f32;
        let r = edges.iter().map(|&e| (pv + p[t.other(e as usize, v as u32) as usize]) * 0.5).sum::<Vec3>() / n;
        (q + 2.0 * r + (n - 3.0) * pv) / n
    };
    let s = sharp_at(t, v);
    if s.count < 2 && !s.corner {
        return smooth();
    }
    let sharp = if s.corner { pv } else { (p[s.ends[0] as usize] + 6.0 * pv + p[s.ends[1] as usize]) / 8.0 };
    if s.mean >= 1.0 { sharp } else { smooth().lerp(sharp, s.mean) }
}

/// Limit positions of a level whose faces are all quads (every level
/// past the cage): Halstead et al. 1993's mask (n²v + 4Σe + Σd) / (n(n + 5))
/// for smooth points (e the edge neighbours, d the diagonal corners), the
/// cubic B-spline's (a + 4v + b) / 6 on creases and borders, and the point
/// itself at corners. Exact where no finite sharpness is left at the
/// level; where some is, the crease's limit is used from sharpness 1 and a
/// blend below it (the smooth and sharp rules, as the vertex rule blends).
pub fn limit_points(t: &Topology, p: &[Vec3]) -> Result<Vec<Vec3>, String> {
    if t.corners.len() != 4 * t.faces() {
        return Err("limit positions need an all-quad level (level 1 or finer)".into());
    }
    let mut out = vec![Vec3::ZERO; t.vertices];
    par_chunks(&mut out, PAR, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let v = i * PAR + k;
            let pv = p[v];
            let edges = t.edges_of(v);
            if edges.is_empty() {
                *o = pv;
                continue;
            }
            let smooth = || {
                let n = edges.len() as f32;
                let e: Vec3 = edges.iter().map(|&e| p[t.other(e as usize, v as u32) as usize]).sum();
                let d: Vec3 = t
                    .faces_of(v)
                    .iter()
                    .map(|&f| {
                        let face = t.face(f as usize);
                        let j = face.iter().position(|&c| c as usize == v).expect("the face holds the vertex");
                        p[face[(j + 2) % 4] as usize]
                    })
                    .sum();
                (n * n * pv + 4.0 * e + d) / (n * (n + 5.0))
            };
            let s = sharp_at(t, v);
            *o = if s.count < 2 && !s.corner {
                smooth()
            } else {
                let sharp = if s.corner { pv } else { (p[s.ends[0] as usize] + 4.0 * pv + p[s.ends[1] as usize]) / 6.0 };
                if s.mean >= 1.0 { sharp } else { smooth().lerp(sharp, s.mean) }
            };
        }
    });
    Ok(out)
}

/// Per vertex: tangent, bitangent and normal of a level's mesh (see the
/// module doc). Faces are visited in order, so the frames do not depend
/// on the thread count.
pub fn frames(p: &[Vec3], face_start: &[u32], corners: &[u32]) -> Vec<[Vec3; 3]> {
    let mut normal = vec![Vec3::ZERO; p.len()];
    let mut toward = vec![Vec3::ZERO; p.len()];
    for f in 0..face_start.len() - 1 {
        let face = &corners[face_start[f] as usize..face_start[f + 1] as usize];
        let n = newell(p, face);
        for (j, &c) in face.iter().enumerate() {
            normal[c as usize] += n;
            if toward[c as usize] == Vec3::ZERO {
                toward[c as usize] = p[face[(j + 1) % face.len()] as usize] - p[c as usize];
            }
        }
    }
    normal
        .iter()
        .zip(&toward)
        .map(|(n, d)| {
            let n = n.normalize_or(Vec3::Z);
            let t = (*d - n * d.dot(n)).try_normalize().unwrap_or_else(|| n.any_orthonormal_vector());
            [t, n.cross(t), n]
        })
        .collect()
}

/// A polygon's area vector (Newell's method): twice its area along its
/// normal.
pub fn newell(p: &[Vec3], face: &[u32]) -> Vec3 {
    let mut n = Vec3::ZERO;
    for j in 0..face.len() {
        let (a, b) = (p[face[j] as usize], p[face[(j + 1) % face.len()] as usize]);
        n += a.cross(b);
    }
    n
}

/// A level's points, faces and edges.
pub struct Surface {
    pub points: Vec<Vec3>,
    pub face_start: Vec<u32>,
    pub corners: Vec<u32>,
    pub edges: Vec<[u32; 2]>,
}

impl Surface {
    /// Area-weighted vertex normals.
    pub fn normals(&self) -> Vec<Vec3> {
        frames(&self.points, &self.face_start, &self.corners).into_iter().map(|f| f[2]).collect()
    }

    /// Triangles: each face as a fan from its first corner.
    pub fn triangles(&self) -> Vec<[u32; 3]> {
        let mut out = Vec::with_capacity(self.corners.len().saturating_sub(2 * (self.face_start.len() - 1)));
        for f in 0..self.face_start.len() - 1 {
            let face = &self.corners[self.face_start[f] as usize..self.face_start[f + 1] as usize];
            for j in 1..face.len() - 1 {
                out.push([face[0], face[j], face[j + 1]]);
            }
        }
        out
    }
}

/// The topology of a polygon object's levels, made once for its cage
/// faces and creases: re-evaluating after the cage's points move (or its
/// displacements change) runs only the point passes.
pub struct Refiner {
    /// Levels 0..level (with adjacency).
    pub topology: Vec<Topology>,
    /// The finest level's faces and edges.
    pub top: Core,
}

impl Refiner {
    /// Levels up to `level` of `poly`'s cage (refused past `MAX_FACES`).
    pub fn new(poly: &Poly, level: u8) -> Result<Refiner, String> {
        super::check_faces(poly.faces() as u64, poly.corners.len() as u64, level)?;
        let (s, c) = poly.cage_faces();
        let mut t0 = Topology::build(poly.cage.len(), s, c);
        t0.set_creases(&poly.creases);
        let mut topology = vec![t0];
        for _ in 1..level {
            let next = topology.last().expect("level 0").refine();
            topology.push(next);
        }
        let top = if level == 0 { topology[0].core() } else { topology.last().expect("level 0").refine_core() };
        if level == 0 {
            topology.clear();
        }
        Ok(Refiner { topology, top })
    }

    pub fn level(&self) -> usize {
        self.topology.len()
    }

    /// Faces of level `k` (`face_start`, `corners`).
    pub fn faces_of_level(&self, k: usize) -> (&[u32], &[u32]) {
        if k == self.level() { (&self.top.face_start, &self.top.corners) } else { (&self.topology[k].face_start, &self.topology[k].corners) }
    }

    /// The finest level's points from cage points and per-level
    /// displacements (`levels[k - 1]` for level k).
    pub fn points(&self, cage: Vec<Vec3>, levels: &[Level]) -> Vec<Vec3> {
        let mut p = cage;
        for k in 0..self.level() {
            p = subdivide_points(&self.topology[k], &p);
            if let Some(d) = levels.get(k).and_then(|l| l.displacement.as_ref()) {
                let (s, c) = self.faces_of_level(k + 1);
                let frames = frames(&p, s, c);
                for (i, (q, f)) in p.iter_mut().zip(&frames).enumerate().take(d.len()) {
                    let d = d.get(i);
                    *q += f[0] * d.x + f[1] * d.y + f[2] * d.z;
                }
            }
        }
        p
    }

    /// The finest level as a surface.
    pub fn surface(&self, poly: &Poly) -> Surface {
        Surface { points: self.points(poly.cage.to_vec(), &poly.levels), face_start: self.top.face_start.clone(), corners: self.top.corners.clone(), edges: self.top.edges.clone() }
    }

    /// The finest level's topology with adjacency.
    pub fn top_topology(&self) -> Topology {
        Topology::from_core(self.top.clone())
    }
}

/// Level `level` of `poly` (at most its levels kept).
pub fn evaluate(poly: &Poly, level: u8) -> Result<Surface, String> {
    if level > poly.level_count() {
        return Err(format!("level {level} is past the {} levels kept", poly.level_count()));
    }
    Ok(Refiner::new(poly, level)?.surface(poly))
}

/// Limit positions of level `level`'s points (level 0: the cage's points'
/// limits, read at level 1, where every face is a quad).
#[cfg_attr(not(test), expect(dead_code, reason = "Stage 3c computes frames inline; kept for Stage 4a's limit evaluation, the tests check it"))]
pub fn limit(poly: &Poly, level: u8) -> Result<Vec<Vec3>, String> {
    let at = level.max(1);
    let mut levels = poly.levels.clone();
    levels.resize(at as usize, Level::default());
    let r = Refiner::new(poly, at)?;
    let p = r.points(poly.cage.to_vec(), &levels);
    let mut out = limit_points(&r.top_topology(), &p)?;
    if level == 0 {
        out.truncate(poly.cage.len());
    }
    Ok(out)
}

/// Displacements for level `level` that move its points by `world`
/// (each in that point's frame): what Stage 3c's projection stores.
#[cfg_attr(not(test), expect(dead_code, reason = "Stage 3c computes frames inline; kept for Stage 4a's limit evaluation, the tests check it"))]
pub fn to_tangent(poly: &Poly, level: u8, world: &[Vec3]) -> Result<Vec<Vec3>, String> {
    if level == 0 || level > poly.level_count() {
        return Err(format!("displacement is per level 1..={}", poly.level_count()));
    }
    let r = Refiner::new(poly, level)?;
    // The level undisplaced: coarser levels' displacements stay.
    let mut levels = poly.levels.clone();
    levels[level as usize - 1].displacement = None;
    let p = r.points(poly.cage.to_vec(), &levels);
    if world.len() != p.len() {
        return Err(format!("level {level} has {} points, not {}", p.len(), world.len()));
    }
    let frames = frames(&p, &r.top.face_start, &r.top.corners);
    Ok(world.iter().zip(&frames).map(|(w, f)| Vec3::new(w.dot(f[0]), w.dot(f[1]), w.dot(f[2]))).collect())
}

/// Per level, the displacements that make `img` (`src` reflected in its
/// axis `axis` by `Poly::mirrored`: the same points reflected, each face
/// wound the other way from its first corner) displace as `src` does,
/// reflected. Levels without a displacement stay None.
///
/// The reversed faces renumber the finer levels (`Topology::refine`
/// numbers child faces by corner, and inner edges by corner too), so the
/// image's points are matched to the source's level by level: the cage's
/// points are the same; face `f`'s corner `j` is the source face's corner
/// `(n - j) % n` (level 0 by construction, and every finer level's quads
/// keep that, their next and previous edges trading places); an edge is
/// the source's edge between the matching corners; face points follow
/// their faces and child faces their corners. Each image point's offset is
/// the source point's world offset, reflected, in the image's own frame.
pub fn mirror_displacements(src: &Poly, img: &Poly, axis: usize) -> Result<Vec<Option<Chunked<Vec3>>>, String> {
    let mut out = vec![None; src.levels.len()];
    let n = src.levels.iter().rposition(|l| l.displacement.is_some()).map_or(0, |k| k + 1);
    if n == 0 {
        return Ok(out);
    }
    let (rs, ri) = (Refiner::new(src, n as u8)?, Refiner::new(img, n as u8)?);
    let mut flip = Vec3::ONE;
    flip[axis.min(2)] = -1.0;
    let (mut ps, mut pi) = (src.cage.to_vec(), img.cage.to_vec());
    // Image face -> source face, image point -> source point (this level).
    let mut fmap: Vec<u32> = (0..src.faces() as u32).collect();
    let mut vmap: Vec<u32> = (0..src.cage.len() as u32).collect();
    for k in 0..n {
        let (ts, ti) = (&rs.topology[k], &ri.topology[k]);
        let (nv, nf, ne) = (ts.vertices, ts.faces(), ts.edges.len());
        if ti.vertices != nv || ti.faces() != nf || ti.edges.len() != ne || ti.corners.len() != ts.corners.len() {
            return Err("the image's level counts differ from the source's".into());
        }
        let mut emap = vec![0u32; ne];
        let mut cmap = vec![0u32; ti.corners.len()];
        for f in 0..nf {
            let g = fmap[f] as usize;
            let (a, b) = (ti.face_start[f] as usize, ti.face_start[f + 1] as usize);
            let sides = b - a;
            let sa = ts.face_start[g] as usize;
            if ts.face_start[g + 1] as usize - sa != sides {
                return Err("the image's faces do not match the source's".into());
            }
            for j in 0..sides {
                let at = (sides - j) % sides;
                cmap[a + j] = (sa + at) as u32;
                // The image's edge from corner j to j + 1 is the source's
                // from its corner `at - 1` to `at`.
                emap[ti.corner_edge[a + j] as usize] = ts.corner_edge[sa + (at + sides - 1) % sides];
            }
        }
        let mut next = Vec::with_capacity(nv + nf + ne);
        next.extend_from_slice(&vmap);
        next.extend(fmap.iter().map(|&g| nv as u32 + g));
        next.extend(emap.iter().map(|&e| (nv + nf) as u32 + e));
        vmap = next;
        fmap = cmap;
        let (bs, bi) = (subdivide_points(ts, &ps), subdivide_points(ti, &pi));
        let ((ss, sc), (is, ic)) = (rs.faces_of_level(k + 1), ri.faces_of_level(k + 1));
        let (fs, fi) = (frames(&bs, ss, sc), frames(&bi, is, ic));
        let d = src.levels[k].displacement.as_ref();
        let world = |m: usize| match d.filter(|d| m < d.len()) {
            Some(d) => {
                let (x, g) = (d.get(m), fs[m]);
                g[0] * x.x + g[1] * x.y + g[2] * x.z
            }
            None => Vec3::ZERO,
        };
        ps = bs.iter().enumerate().map(|(m, b)| *b + world(m)).collect();
        let mut di = Vec::with_capacity(bi.len());
        for (i, f) in fi.iter().enumerate() {
            let w = world(vmap[i] as usize) * flip;
            di.push(Vec3::new(w.dot(f[0]), w.dot(f[1]), w.dot(f[2])));
        }
        pi = bi.iter().zip(&fi).zip(&di).map(|((b, f), x)| *b + (f[0] * x.x + f[1] * x.y + f[2] * x.z)).collect();
        if d.is_some() {
            out[k] = Some(Chunked::new(&di));
        }
    }
    Ok(out)
}
