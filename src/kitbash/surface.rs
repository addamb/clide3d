//! A target's surface in world rest space, for placing parts on it:
//! ray snapping (parts project onto the surface: no floating, no
//! sinking), the paint under a point, the bone that moves it, flat
//! regions (where panels and greebles fit) and hard edges (where bolts
//! go), and per-region masks of where there is room.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat3, Mat4, Vec2, Vec3};

use crate::film::bvh::Bvh;
use crate::mesh::Mesh;
use crate::rig::Skin;

/// One object whose mesh is part of the surface.
#[derive(Clone)]
pub struct Owner {
    pub id: u64,
    pub mesh: Arc<Mesh>,
    /// Rest-pose skin and bone names, for rigged clay.
    pub skin: Option<(Arc<Skin>, Vec<String>)>,
    /// Object -> world (rest pose).
    pub world: Mat4,
    /// World-space decals and other things to keep clear.
    pub keep_clear: Vec<Keep>,
}

/// Something on a surface to keep clear of: a footprint (base frame with
/// unit axes, +Y out; half extents along X and Z).
#[derive(Clone, Copy, Debug)]
pub struct Keep {
    pub frame: Mat4,
    pub hx: f32,
    pub hz: f32,
}

impl Keep {
    /// A round thing (a decal): a square around it, any way up.
    pub fn disc(c: Vec3, r: f32) -> Keep {
        Keep { frame: Mat4::from_translation(c), hx: r, hz: r }
    }

    /// A footprint from a (possibly scaled) placement frame.
    pub fn footprint(frame: Mat4, hx: f32, hz: f32) -> Keep {
        let k = frame.transform_vector3(Vec3::X).length().max(1e-9);
        let x = frame.transform_vector3(Vec3::X).normalize_or(Vec3::X);
        let y = frame.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        let z = x.cross(y);
        Keep { frame: Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), frame.w_axis), hx: hx * k, hz: hz * k }
    }

    pub fn center(&self) -> Vec3 {
        self.frame.w_axis.truncate()
    }

    pub fn radius(&self) -> f32 {
        self.hx.hypot(self.hz)
    }

    /// Whether a world point lies over the footprint (within `depth` of
    /// its base plane).
    pub fn covers(&self, p: Vec3, depth: f32) -> bool {
        let q = self.frame.inverse().transform_point3(p);
        q.x.abs() <= self.hx && q.z.abs() <= self.hz && q.y.abs() <= depth
    }
}

/// Where a ray met the surface.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub p: Vec3,
    /// Shading normal, facing back toward where the ray came from.
    pub n: Vec3,
    /// Geometric (face) normal, same side as `n`.
    pub face: Vec3,
    pub tri: u32,
    pub bary: [f32; 3],
}

pub struct Surface {
    pub owners: Vec<Owner>,
    pub tris: Vec<[Vec3; 3]>,
    /// (owner, triangle index in its mesh).
    pub tri_owner: Vec<(u32, u32)>,
    pub face_n: Vec<Vec3>,
    bvh: Bvh,
    /// Size of the whole surface (bounding box diagonal).
    pub diag: f32,
}

impl Surface {
    pub fn new(owners: Vec<Owner>) -> Surface {
        let mut tris = Vec::new();
        let mut tri_owner = Vec::new();
        let mut face_n = Vec::new();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for (k, o) in owners.iter().enumerate() {
            let m = o.world;
            for (ti, t) in o.mesh.indices.chunks_exact(3).enumerate() {
                let c = [0, 1, 2].map(|j| m.transform_point3(Vec3::from(o.mesh.vertices[t[j] as usize].pos)));
                let n = (c[1] - c[0]).cross(c[2] - c[0]);
                if n.length_squared() < 1e-20 {
                    continue;
                }
                for p in c {
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                tris.push(c);
                tri_owner.push((k as u32, ti as u32));
                face_n.push(n.normalize());
            }
        }
        let bvh = Bvh::new(&tris);
        let diag = if tris.is_empty() { 0.0 } else { (hi - lo).length() };
        Surface { owners, tris, tri_owner, face_n, bvh, diag }
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Nearest hit along a ray within `max`.
    pub fn ray(&self, origin: Vec3, dir: Vec3, max: f32) -> Option<Hit> {
        let dir = dir.normalize_or(Vec3::NEG_Y);
        let h = self.bvh.hit(origin, dir, 0.0, max, &|_| false)?;
        let tri = h.tri;
        let (oi, ti) = self.tri_owner[tri as usize];
        let o = &self.owners[oi as usize];
        let t = &o.mesh.indices[ti as usize * 3..ti as usize * 3 + 3];
        let bary = [1.0 - h.u - h.v, h.u, h.v];
        let nm = Mat3::from_mat4(o.world).inverse().transpose();
        let smooth: Vec3 = (0..3).map(|j| Vec3::from(o.mesh.vertices[t[j] as usize].normal) * bary[j]).sum();
        let mut face = self.face_n[tri as usize];
        if face.dot(dir) > 0.0 {
            face = -face;
        }
        let mut n = (nm * smooth).normalize_or(face);
        if n.dot(face) < 0.0 {
            n = -n;
        }
        Some(Hit { p: origin + dir * h.t, n, face, tri, bary })
    }

    /// The surface near `p`: a ray down `-n` from `reach` above it, or up
    /// from below when that misses (the nearer of the two).
    pub fn snap(&self, p: Vec3, n: Vec3, reach: f32) -> Option<Hit> {
        let n = n.normalize_or(Vec3::Y);
        let down = self.ray(p + n * reach, -n, reach * 2.0);
        let up = self.ray(p - n * reach, n, reach * 2.0);
        match (down, up) {
            (Some(a), Some(b)) => Some(if a.p.distance(p) <= b.p.distance(p) { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    /// The nearest surface point to `p`: rays cast in from outside along
    /// the axes and diagonals, back through the point (points on, just off
    /// or just inside the surface all work). Normals face out.
    pub fn nearest(&self, p: Vec3) -> Option<Hit> {
        let reach = self.diag.max(1e-3);
        let dirs = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z, Vec3::new(1.0, 1.0, 1.0), Vec3::new(-1.0, 1.0, 1.0), Vec3::new(1.0, -1.0, 1.0), Vec3::new(1.0, 1.0, -1.0), Vec3::new(-1.0, -1.0, 1.0), Vec3::new(-1.0, 1.0, -1.0), Vec3::new(1.0, -1.0, -1.0), Vec3::new(-1.0, -1.0, -1.0)];
        let mut best: Option<(f32, Hit)> = None;
        for d in dirs {
            let d = d.normalize();
            for r in [reach * 0.02, reach * 0.25] {
                if let Some(h) = self.ray(p + d * r, -d, r * 2.0) {
                    let dist = h.p.distance(p);
                    if best.as_ref().is_none_or(|b| dist < b.0) {
                        best = Some((dist, h));
                    }
                }
            }
        }
        best.map(|(_, mut h)| {
            let out = self.face_n[h.tri as usize];
            if h.face.dot(out) < 0.0 {
                h.face = -h.face;
                h.n = -h.n;
            }
            h
        })
    }

    pub fn owner_of(&self, tri: u32) -> &Owner {
        &self.owners[self.tri_owner[tri as usize].0 as usize]
    }

    fn corners(&self, tri: u32) -> [usize; 3] {
        let (oi, ti) = self.tri_owner[tri as usize];
        let t = &self.owners[oi as usize].mesh.indices[ti as usize * 3..ti as usize * 3 + 3];
        [t[0] as usize, t[1] as usize, t[2] as usize]
    }

    /// The material and color (sRGB, before smart layers) at a hit.
    pub fn paint_at(&self, h: &Hit) -> (String, [f32; 3]) {
        let o = self.owner_of(h.tri);
        let c = self.corners(h.tri);
        // The corner with the most weight decides the material.
        let k = (0..3).max_by(|&a, &b| h.bary[a].total_cmp(&h.bary[b])).unwrap_or(0);
        let v = &o.mesh.vertices[c[k]];
        let name = o.mesh.materials.get(v.material as usize).map(|m| m.name.clone()).unwrap_or_default();
        (name, self.color_of(h.tri))
    }

    /// A triangle's color before layers (the mean of its corners).
    pub fn color_of(&self, tri: u32) -> [f32; 3] {
        let o = self.owner_of(tri);
        let c = self.corners(tri);
        let base = |i: usize| { let v = &o.mesh.vertices[i]; if v.base[3] >= -1.0 && v.base[0] >= 0.0 { Vec3::new(v.base[0], v.base[1], v.base[2]) } else { Vec3::from(v.color) } };
        ((base(c[0]) + base(c[1]) + base(c[2])) / 3.0).to_array()
    }

    /// Whether a footprint (base plane `frame`, half extents) lies over
    /// paint that differs from the paint at its center (stencils, flags)
    /// or over a decal kept clear.
    pub fn covers_markings(&self, frame: Mat4, hx: f32, hz: f32) -> bool {
        let n = frame.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        let reach = hx.max(hz).max(1e-3);
        let center = frame.transform_point3(Vec3::ZERO);
        let Some(c) = self.ray(center + n * reach, -n, reach * 2.0) else { return false };
        let base = Vec3::from(self.color_of(c.tri));
        for o in &self.owners {
            if o.keep_clear.iter().any(|k| { let d = k.center() - center; (d - n * d.dot(n)).length() < k.radius() + hx.max(hz) && d.dot(n).abs() < reach }) {
                return true;
            }
        }
        for j in 0..5 {
            for i in 0..5 {
                let q = frame.transform_point3(Vec3::new((i as f32 / 2.0 - 1.0) * hx, 0.0, (j as f32 / 2.0 - 1.0) * hz));
                if let Some(h) = self.ray(q + n * reach, -n, reach * 2.0)
                    && Vec3::from(self.color_of(h.tri)).distance(base) > 0.4
                {
                    return true;
                }
            }
        }
        false
    }

    /// The bone that moves a hit point most (rigged owners only).
    pub fn bone_at(&self, h: &Hit) -> Option<String> {
        let o = self.owner_of(h.tri);
        let (skin, names) = o.skin.as_ref()?;
        let c = self.corners(h.tri);
        let mut weight: HashMap<u16, f32> = HashMap::new();
        for (&ci, &b) in c.iter().zip(&h.bary) {
            let (j, w) = (skin.joints.get(ci)?, skin.weights.get(ci)?);
            for (&ji, &wi) in j.iter().zip(w) {
                *weight.entry(ji).or_default() += wi * b;
            }
        }
        let best = weight.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))?;
        names.get(best.0 as usize).cloned()
    }

    /// How far a part's base must sink so no corner of its footprint
    /// floats: the base plane is `frame` (y = 0, +Y out), the footprint
    /// half extents `hx`, `hz`. Returns (sink, worst float before sinking,
    /// how far the surface rises through the base) or None where the
    /// footprint hangs off the surface.
    pub fn embed(&self, frame: Mat4, hx: f32, hz: f32) -> Option<(f32, f32, f32)> {
        let n = frame.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        let reach = (hx.max(hz) * 1.5).max(1e-3);
        let (mut gap, mut rise) = (0.0f32, 0.0f32);
        for (x, z) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)] {
            let q = frame.transform_point3(Vec3::new(x * hx, 0.0, z * hz));
            let h = self.ray(q + n * reach, -n, reach * 2.0)?;
            // Positive: the surface is below the base there (a gap).
            let d = (q - h.p).dot(n);
            gap = gap.max(d);
            rise = rise.max(-d);
        }
        Some((gap.max(0.0), gap, rise))
    }

    /// Flat regions and hard edges of the owners listed in `which` (all
    /// when empty).
    pub fn analyze(&self, which: &[u32], flat_deg: f32, hard_deg: f32) -> Analysis {
        analyze(self, which, flat_deg, hard_deg)
    }
}

/// A flat piece of surface.
#[derive(Clone, Debug)]
pub struct Region {
    pub owner: u32,
    pub tris: Vec<u32>,
    pub normal: Vec3,
    /// A point on the plane (area-weighted centroid).
    pub center: Vec3,
    pub area: f32,
    /// In-plane axes: `u` follows the region's main edge direction.
    pub u: Vec3,
    pub v: Vec3,
    /// Bounds in (u, v) relative to `center`.
    pub lo: Vec2,
    pub hi: Vec2,
    /// The most common color (sRGB) on it.
    pub color: [f32; 3],
}

impl Region {
    pub fn to_uv(&self, p: Vec3) -> Vec2 {
        let d = p - self.center;
        Vec2::new(d.dot(self.u), d.dot(self.v))
    }

    pub fn at_uv(&self, q: Vec2) -> Vec3 {
        self.center + self.u * q.x + self.v * q.y
    }

    pub fn size(&self) -> Vec2 {
        self.hi - self.lo
    }
}

/// A straight run of hard edge between two flat regions.
#[derive(Clone, Debug)]
pub struct Edge {
    pub a: Vec3,
    pub b: Vec3,
    /// The regions on either side (index into `Analysis::regions`).
    pub sides: [usize; 2],
    /// Convex (an outside corner) or concave (an inside one).
    pub convex: bool,
}

pub struct Analysis {
    pub regions: Vec<Region>,
    /// Region of each surface triangle (usize::MAX: none).
    pub region_of: Vec<usize>,
    pub edges: Vec<Edge>,
}

fn analyze(s: &Surface, which: &[u32], flat_deg: f32, hard_deg: f32) -> Analysis {
    let n = s.tris.len();
    let keep = |t: usize| which.is_empty() || which.contains(&s.tri_owner[t].0);
    // Weld corners by position.
    let q = (s.diag * 2e-6).max(1e-7);
    let key = |p: Vec3| ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
    let mut ids: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(n);
    let mut corner = vec![[0u32; 3]; n];
    for (t, c) in corner.iter_mut().enumerate() {
        if !keep(t) {
            continue;
        }
        for (k, slot) in c.iter_mut().enumerate() {
            let next = ids.len() as u32;
            *slot = *ids.entry(key(s.tris[t][k])).or_insert(next);
        }
    }
    let mut edge_tris: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(n * 3 / 2);
    for (t, c) in corner.iter().enumerate() {
        if !keep(t) {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (c[k], c[(k + 1) % 3]);
            if a != b {
                edge_tris.entry((a.min(b), a.max(b))).or_default().push(t as u32);
            }
        }
    }
    let area = |t: usize| { let [a, b, c] = s.tris[t]; (b - a).cross(c - a).length() * 0.5 };
    // Grow regions: neighbors whose normal stays within `flat_deg` of the
    // seed and that stay on its plane.
    let cos_flat = flat_deg.to_radians().cos();
    let mut region_of = vec![usize::MAX; n];
    let mut regions = Vec::new();
    // Seeds in order of size so big flat faces start from a clean triangle.
    let mut order: Vec<usize> = (0..n).filter(|&t| keep(t)).collect();
    order.sort_by(|&a, &b| area(b).total_cmp(&area(a)).then(a.cmp(&b)));
    let neighbors = |t: usize| -> Vec<u32> {
        let mut out = Vec::with_capacity(3);
        for k in 0..3 {
            let (a, b) = (corner[t][k], corner[t][(k + 1) % 3]);
            if let Some(list) = edge_tris.get(&(a.min(b), a.max(b))) {
                out.extend(list.iter().copied().filter(|&o| o as usize != t));
            }
        }
        out
    };
    for &seed in &order {
        if region_of[seed] != usize::MAX {
            continue;
        }
        let id = regions.len();
        let n0 = s.face_n[seed];
        let p0 = s.tris[seed][0];
        let tol = (area(seed).sqrt() * 0.05).max(s.diag * 2e-4);
        let mut stack = vec![seed];
        region_of[seed] = id;
        let mut tris = Vec::new();
        while let Some(t) = stack.pop() {
            tris.push(t as u32);
            for o in neighbors(t) {
                let o = o as usize;
                if region_of[o] != usize::MAX || !keep(o) {
                    continue;
                }
                let c = (s.tris[o][0] + s.tris[o][1] + s.tris[o][2]) / 3.0;
                if s.face_n[o].dot(n0) >= cos_flat && (c - p0).dot(n0).abs() <= tol {
                    region_of[o] = id;
                    stack.push(o);
                }
            }
        }
        let total: f32 = tris.iter().map(|&t| area(t as usize)).sum();
        let normal = tris.iter().map(|&t| s.face_n[t as usize] * area(t as usize)).sum::<Vec3>().normalize_or(n0);
        let center = tris.iter().map(|&t| { let [a, b, c] = s.tris[t as usize]; (a + b + c) / 3.0 * area(t as usize) }).sum::<Vec3>() / total.max(1e-12);
        regions.push(Region { owner: s.tri_owner[seed].0, tris, normal, center, area: total, u: Vec3::X, v: Vec3::Z, lo: Vec2::ZERO, hi: Vec2::ZERO, color: [0.5; 3] });
    }
    // Axes from the boundary: the dominant edge direction (mod 90 deg),
    // then the one closest to world X (or Z) as u.
    let mut boundary: Vec<Vec<(Vec3, Vec3)>> = vec![Vec::new(); regions.len()];
    let mut edges_raw: Vec<(Vec3, Vec3, usize, usize)> = Vec::new();
    let cos_hard = hard_deg.to_radians().cos();
    let mut keys: Vec<(u32, u32)> = edge_tris.keys().copied().collect();
    keys.sort_unstable();
    for (a, b) in keys {
        let list = &edge_tris[&(a, b)];
        let t0 = list[0] as usize;
        let k0 = (0..3).find(|&k| { let (x, y) = (corner[t0][k], corner[t0][(k + 1) % 3]); (x.min(y), x.max(y)) == (a, b) }).unwrap_or(0);
        let (pa, pb) = (s.tris[t0][k0], s.tris[t0][(k0 + 1) % 3]);
        let r0 = region_of[t0];
        let other = list.iter().map(|&t| t as usize).find(|&t| region_of[t] != r0);
        match other {
            None if list.len() == 1 => boundary[r0].push((pa, pb)),
            None => {}
            Some(t1) => {
                let r1 = region_of[t1];
                boundary[r0].push((pa, pb));
                boundary[r1].push((pa, pb));
                if s.face_n[t0].dot(s.face_n[t1]) < cos_hard {
                    edges_raw.push((pa, pb, r0.min(r1), r0.max(r1)));
                }
            }
        }
    }
    for (r, reg) in regions.iter_mut().enumerate() {
        let (bu, bv) = super::geo::plane_axes(reg.normal);
        let mut acc = Vec2::ZERO;
        for (a, b) in &boundary[r] {
            let d = *b - *a;
            let x = Vec2::new(d.dot(bu), d.dot(bv));
            let l = x.length();
            if l < 1e-9 {
                continue;
            }
            let ang = x.y.atan2(x.x) * 4.0;
            acc += Vec2::new(ang.cos(), ang.sin()) * l;
        }
        let theta = acc.y.atan2(acc.x) / 4.0;
        let u0 = bu * theta.cos() + bv * theta.sin();
        let v0 = reg.normal.cross(u0);
        // Of the four in-plane axes, u is the one nearest world X (Z on
        // faces looking along X), so layouts mirror cleanly.
        let reference = if reg.normal.x.abs() > 0.8 { Vec3::Z } else { Vec3::X };
        let cands = [u0, v0, -u0, -v0];
        let u = cands.into_iter().max_by(|a, b| a.dot(reference).total_cmp(&b.dot(reference))).unwrap_or(u0);
        reg.u = (u - reg.normal * u.dot(reg.normal)).normalize_or(bu);
        reg.v = reg.normal.cross(reg.u);
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        let mut colors: HashMap<[u8; 3], f32> = HashMap::new();
        for &t in &reg.tris {
            for p in s.tris[t as usize] {
                let q = reg.to_uv(p);
                lo = lo.min(q);
                hi = hi.max(q);
            }
            let c = s.color_of(t);
            *colors.entry(c.map(|x| (x * 20.0).round() as u8)).or_default() += area(t as usize);
        }
        reg.lo = lo;
        reg.hi = hi;
        if let Some((c, _)) = colors.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0))) {
            reg.color = c.map(|x| x as f32 / 20.0);
        }
    }
    // Hard edges: merge collinear pieces between the same two regions.
    let mut by_pair: HashMap<(usize, usize), Vec<(Vec3, Vec3)>> = HashMap::new();
    for (a, b, r0, r1) in edges_raw {
        by_pair.entry((r0, r1)).or_default().push((a, b));
    }
    let mut pairs: Vec<_> = by_pair.into_iter().collect();
    pairs.sort_by_key(|(k, _)| *k);
    let mut edges = Vec::new();
    for ((r0, r1), segs) in pairs {
        let dir = regions[r0].normal.cross(regions[r1].normal).normalize_or(Vec3::X);
        let origin = segs[0].0;
        let mut spans: Vec<(f32, f32)> = segs.iter().map(|(a, b)| { let (x, y) = ((*a - origin).dot(dir), (*b - origin).dot(dir)); (x.min(y), x.max(y)) }).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let join = s.diag * 1e-3;
        let mut merged: Vec<(f32, f32)> = Vec::new();
        for sp in spans {
            match merged.last_mut() {
                Some(m) if sp.0 <= m.1 + join => m.1 = m.1.max(sp.1),
                _ => merged.push(sp),
            }
        }
        // Convex when each face's normal points away from the other region.
        let mid = |r: usize| regions[r].center;
        let convex = (mid(r1) - mid(r0)).dot(regions[r0].normal) < 0.0;
        // The line itself: average offset of the pieces off `dir`.
        let off = segs.iter().map(|(a, b)| { let m = (*a + *b) * 0.5; m - origin - dir * (m - origin).dot(dir) }).sum::<Vec3>() / segs.len() as f32;
        for (t0, t1) in merged {
            edges.push(Edge { a: origin + off + dir * t0, b: origin + off + dir * t1, sides: [r0, r1], convex });
        }
    }
    Analysis { regions, region_of, edges }
}

/// Where in a flat region there is room: a grid over its (u, v) bounds
/// holding, per cell, the distance (meters) to the nearest cell that is
/// off the region, painted (stencils) or kept clear.
pub struct Room {
    pub origin: Vec2,
    pub cell: f32,
    pub nx: usize,
    pub ny: usize,
    pub clear: Vec<f32>,
}

impl Room {
    /// Rasterizes `region` (cells about `cell` wide, at most 320 across).
    /// Triangles whose color is far from the region's (stencils, flags; not
    /// soft grime), and `keep_clear`
    /// discs near the plane, count as taken.
    pub fn new(s: &Surface, region: &Region, cell: f32, keep_clear: &[Keep]) -> Room {
        let size = region.size();
        let cell = cell.max(size.max_element() / 320.0).max(1e-5);
        let nx = ((size.x / cell).ceil() as usize + 2).clamp(1, 400);
        let ny = ((size.y / cell).ceil() as usize + 2).clamp(1, 400);
        let origin = region.lo - Vec2::splat(cell);
        let mut free = vec![false; nx * ny];
        let mut taken = vec![false; nx * ny];
        let base = Vec3::from(region.color);
        for &t in &region.tris {
            let tri = s.tris[t as usize];
            let q = tri.map(|p| (region.to_uv(p) - origin) / cell);
            let painted = Vec3::from(s.color_of(t)).distance(base) > 0.4;
            let lo = q.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(*p)).floor().max(Vec2::ZERO);
            let hi = q.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(*p)).ceil().min(Vec2::new(nx as f32 - 1.0, ny as f32 - 1.0));
            for y in lo.y as usize..=hi.y as usize {
                for x in lo.x as usize..=hi.x as usize {
                    let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    if inside(c, q) {
                        if painted { taken[y * nx + x] = true; } else { free[y * nx + x] = true; }
                    }
                }
            }
        }
        for k in keep_clear {
            let depth = k.hx.max(k.hz).max(cell * 2.0);
            if (k.center() - region.center).dot(region.normal).abs() > depth {
                continue;
            }
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, z)| (region.to_uv(k.frame.transform_point3(Vec3::new(x * k.hx, 0.0, z * k.hz))) - origin) / cell);
            let lo = corners.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(*p)).floor().max(Vec2::ZERO);
            let hi = corners.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(*p)).ceil();
            if hi.x < 0.0 || hi.y < 0.0 || lo.x >= nx as f32 || lo.y >= ny as f32 {
                continue;
            }
            let hi = hi.min(Vec2::new(nx as f32 - 1.0, ny as f32 - 1.0));
            for y in lo.y as usize..=hi.y as usize {
                for x in lo.x as usize..=hi.x as usize {
                    let p = region.at_uv(origin + Vec2::new(x as f32 + 0.5, y as f32 + 0.5) * cell);
                    if k.covers(p, depth) {
                        taken[y * nx + x] = true;
                    }
                }
            }
        }
        // Distance to the nearest unusable cell (two-pass chamfer).
        let big = 1e9f32;
        let mut d: Vec<f32> = (0..nx * ny).map(|i| if free[i] && !taken[i] { big } else { 0.0 }).collect();
        let (a, b) = (1.0f32, std::f32::consts::SQRT_2);
        for y in 0..ny {
            for x in 0..nx {
                let i = y * nx + x;
                if x == 0 || y == 0 || x == nx - 1 || y == ny - 1 {
                    d[i] = d[i].min(if d[i] > 0.0 { 1.0 } else { 0.0 });
                }
                let mut m = d[i];
                if x > 0 { m = m.min(d[i - 1] + a); }
                if y > 0 {
                    m = m.min(d[i - nx] + a);
                    if x > 0 { m = m.min(d[i - nx - 1] + b); }
                    if x + 1 < nx { m = m.min(d[i - nx + 1] + b); }
                }
                d[i] = m;
            }
        }
        for y in (0..ny).rev() {
            for x in (0..nx).rev() {
                let i = y * nx + x;
                let mut m = d[i];
                if x + 1 < nx { m = m.min(d[i + 1] + a); }
                if y + 1 < ny {
                    m = m.min(d[i + nx] + a);
                    if x + 1 < nx { m = m.min(d[i + nx + 1] + b); }
                    if x > 0 { m = m.min(d[i + nx - 1] + b); }
                }
                d[i] = m;
            }
        }
        // Cells to meters, measured from the cell edge.
        let clear = d.into_iter().map(|v| (v - 0.5).max(0.0) * cell).collect();
        Room { origin, cell, nx, ny, clear }
    }

    /// Clearance (meters) at a (u, v) point; 0 outside the grid.
    pub fn at(&self, q: Vec2) -> f32 {
        let c = (q - self.origin) / self.cell;
        if c.x < 0.0 || c.y < 0.0 {
            return 0.0;
        }
        let (x, y) = (c.x as usize, c.y as usize);
        if x >= self.nx || y >= self.ny {
            return 0.0;
        }
        self.clear[y * self.nx + x]
    }

    /// Whether a rectangle (center, half extents along u and v, turned by
    /// `angle`) lies on free cells with `margin` to spare.
    pub fn fits(&self, c: Vec2, half: Vec2, angle: f32, margin: f32) -> bool {
        let (sn, cs) = angle.sin_cos();
        let rot = |p: Vec2| Vec2::new(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
        let steps = |l: f32| ((2.0 * l / self.cell).ceil() as usize).clamp(1, 64);
        let (nx, ny) = (steps(half.x), steps(half.y));
        for j in 0..=ny {
            for i in 0..=nx {
                let p = Vec2::new(-half.x + 2.0 * half.x * i as f32 / nx as f32, -half.y + 2.0 * half.y * j as f32 / ny as f32);
                if self.at(c + rot(p)) < margin {
                    return false;
                }
            }
        }
        true
    }

    /// Takes a rectangle (axis aligned in u, v) out of the room, with a gap.
    pub fn take(&mut self, lo: Vec2, hi: Vec2) {
        for y in 0..self.ny {
            for x in 0..self.nx {
                let c = self.origin + Vec2::new(x as f32 + 0.5, y as f32 + 0.5) * self.cell;
                // Distance from the cell center to the rectangle.
                let d = (lo - c).max(c - hi).max(Vec2::ZERO).length();
                let i = y * self.nx + x;
                self.clear[i] = self.clear[i].min(d);
            }
        }
    }

    /// The largest axis-aligned rectangle of cells with clearance of at
    /// least `margin`: (lo, hi) in (u, v).
    pub fn largest_rect(&self, margin: f32) -> Option<(Vec2, Vec2)> {
        let (nx, ny) = (self.nx, self.ny);
        let mut heights = vec![0usize; nx];
        let mut best = (0usize, (0usize, 0usize, 0usize, 0usize));
        for y in 0..ny {
            for (x, h) in heights.iter_mut().enumerate() {
                *h = if self.clear[y * nx + x] >= margin { *h + 1 } else { 0 };
            }
            // Largest rectangle in the histogram.
            let mut stack: Vec<usize> = Vec::new();
            for x in 0..=nx {
                let h = if x < nx { heights[x] } else { 0 };
                while let Some(&top) = stack.last() {
                    if heights[top] <= h {
                        break;
                    }
                    stack.pop();
                    let left = stack.last().map_or(0, |&l| l + 1);
                    let area = heights[top] * (x - left);
                    if area > best.0 {
                        best = (area, (left, x, y + 1 - heights[top], y + 1));
                    }
                }
                stack.push(x);
            }
        }
        if best.0 == 0 {
            return None;
        }
        let (x0, x1, y0, y1) = best.1;
        Some((self.origin + Vec2::new(x0 as f32, y0 as f32) * self.cell, self.origin + Vec2::new(x1 as f32, y1 as f32) * self.cell))
    }
}

/// Whether point `p` is inside triangle `t` (2D).
fn inside(p: Vec2, t: [Vec2; 3]) -> bool {
    let s = |a: Vec2, b: Vec2| (b - a).perp_dot(p - a);
    let (d0, d1, d2) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
    let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
    let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
    !(neg && pos)
}
