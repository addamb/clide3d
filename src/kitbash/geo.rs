//! Hard-surface geometry for kitbash parts: chamfered prisms (boxes,
//! hex heads, plates, any polygon), lathes (round parts) and tubes swept
//! along paths (cables, hoses, pipes), each with flat faces where edges
//! are hard, smooth faces where they are round, and UVs in meters times
//! one over the part's size (so a part spans about 0..1).
//!
//! Parts are built in their own space: +Y out of the surface they sit on,
//! the base at y = 0, +X along their length.

use glam::{Mat3, Mat4, Vec2, Vec3};

use crate::envkit::Builder;
use crate::material::Material;

/// What a surface of a part is made of. Each role resolves to a material
/// and a color (see `mod.rs`): paint takes the surface the part sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Paint,
    Metal,
    Dark,
    Rubber,
    Chrome,
    Glow,
    Glass,
    Brass,
    Seam,
    Marking,
    Strap,
}

impl Role {
    pub const ALL: [Role; 11] = [Role::Paint, Role::Metal, Role::Dark, Role::Rubber, Role::Chrome, Role::Glow, Role::Glass, Role::Brass, Role::Seam, Role::Marking, Role::Strap];

    pub fn name(self) -> &'static str {
        match self {
            Role::Paint => "paint",
            Role::Metal => "metal",
            Role::Dark => "dark",
            Role::Rubber => "rubber",
            Role::Chrome => "chrome",
            Role::Glow => "glow",
            Role::Glass => "glass",
            Role::Brass => "brass",
            Role::Seam => "seam",
            Role::Marking => "marking",
            Role::Strap => "strap",
        }
    }

    fn index(self) -> usize {
        Role::ALL.iter().position(|r| *r == self).expect("listed")
    }
}

/// A mesh being built: materials per role, a transform stack and the
/// detail level (segment counts scale with it).
pub struct Geo {
    pub b: Builder,
    mats: [Option<u32>; 11],
    resolved: Vec<(Material, [f32; 3])>,
    pub m: Mat4,
    pub detail: f32,
    uv: f32,
    /// Width of the edge wear band (big faces get a ring this far in).
    pub band_width: f32,
}

impl Geo {
    /// `roles` gives each role's material and sRGB color; `size` is the
    /// part's size (UVs are meters / size).
    pub fn new(roles: Vec<(Material, [f32; 3])>, size: f32, detail: f32) -> Geo {
        Geo { b: Builder::new(Vec::new()), mats: [None; 11], resolved: roles, m: Mat4::IDENTITY, detail: detail.clamp(0.25, 3.0), uv: 1.0 / size.max(1e-4), band_width: 0.0 }
    }

    /// Segment count for a round thing: `n` at detail 1.
    pub fn segs(&self, n: usize) -> usize {
        ((n as f32 * self.detail).round() as usize).clamp(3, 256)
    }

    /// Runs `f` with `m` applied on top of the current transform.
    pub fn with(&mut self, m: Mat4, f: impl FnOnce(&mut Geo)) {
        let keep = self.m;
        self.m = keep * m;
        f(self);
        self.m = keep;
    }

    fn material(&mut self, role: Role) -> u32 {
        let i = role.index();
        if let Some(m) = self.mats[i] {
            return m;
        }
        let m = self.b.material(self.resolved[i].0.clone());
        self.mats[i] = Some(m);
        m
    }

    /// A vertex in part space (the current transform applied).
    fn vert(&mut self, p: Vec3, n: Vec3, uv: Vec2, role: Role) -> u32 {
        let material = self.material(role);
        let color = Vec3::from(self.resolved[role.index()].1);
        let normal = (Mat3::from_mat4(self.m).inverse().transpose() * n).normalize_or(Vec3::Y);
        let i = self.b.vertex(self.m.transform_point3(p), normal, color, material);
        self.b.mesh.vertices[i as usize].uv = (uv * self.uv).to_array();
        i
    }

    /// A triangle wound counter-clockwise around its normals.
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        let v = &self.b.mesh.vertices;
        let p = |i: u32| Vec3::from(v[i as usize].pos);
        let face = (p(b) - p(a)).cross(p(c) - p(a));
        let n: Vec3 = [a, b, c].iter().map(|&i| Vec3::from(v[i as usize].normal)).sum();
        if face.dot(n) >= 0.0 { self.b.tri(a, b, c) } else { self.b.tri(a, c, b) }
    }

    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.tri(a, b, c);
        self.tri(a, c, d);
        self.b.edge(a, b);
        self.b.edge(b, c);
    }

    /// A flat polygon (counter-clockwise seen along `n`), ear clipped.
    pub fn cap(&mut self, pts: &[Vec3], n: Vec3, role: Role) {
        if pts.len() < 3 {
            return;
        }
        let (u, v) = plane_axes(n);
        let flat: Vec<Vec2> = pts.iter().map(|p| Vec2::new(p.dot(u), p.dot(v))).collect();
        let ids: Vec<u32> = pts.iter().zip(&flat).map(|(p, f)| self.vert(*p, n, *f, role)).collect();
        for i in 0..ids.len() {
            self.b.edge(ids[i], ids[(i + 1) % ids.len()]);
        }
        // A big convex face gets an inner ring a wear band in from its
        // edge, so edge wear (measured per vertex) fades out there instead
        // of spreading over the whole face.
        let band = self.band_width;
        let ccw = signed_area(&flat) > 0.0;
        let k = flat.len();
        let convex = (0..k).all(|i| { let (a, b, c) = (flat[i], flat[(i + 1) % k], flat[(i + 2) % k]); ((b - a).perp_dot(c - b) >= -1e-9) == ccw || (b - a).perp_dot(c - b).abs() < 1e-9 });
        let poly: Vec<Vec2> = if ccw { flat.clone() } else { flat.iter().rev().copied().collect() };
        let inner_ok = |inner: &[Vec2]| signed_area(inner) > signed_area(&poly) * 0.25 && inner.iter().all(|q| poly.iter().enumerate().all(|(i, a)| (poly[(i + 1) % poly.len()] - *a).perp_dot(*q - *a) > 0.0));
        if band > 0.0 && convex && inner_ok(&offset(&poly, -band)) {
            let mut inner = offset(&poly, -band);
            if !ccw {
                inner.reverse();
            }
            let to3 = |q: Vec2| pts[0] + u * (q - flat[0]).x + v * (q - flat[0]).y;
            let inner_ids: Vec<u32> = inner.iter().map(|q| { let p = to3(*q); self.vert(p, n, *q, role) }).collect();
            for i in 0..k {
                let j = (i + 1) % k;
                self.tri(ids[i], ids[j], inner_ids[j]);
                self.tri(ids[i], inner_ids[j], inner_ids[i]);
            }
            for [a, b, c] in ear_clip(&inner) {
                self.tri(inner_ids[a], inner_ids[b], inner_ids[c]);
            }
            return;
        }
        for [a, b, c] in ear_clip(&flat) {
            self.tri(ids[a], ids[b], ids[c]);
        }
    }

    /// A polygon `profile` (XZ, any winding) extruded from y0 to y1, its
    /// top edge chamfered by `top` and its bottom edge by `bottom`. Sides
    /// are flat per edge (`smooth` false) or shaded round (cylinders).
    #[allow(clippy::too_many_arguments)]
    pub fn prism(&mut self, profile: &[Vec2], y0: f32, y1: f32, top: f32, bottom: f32, role: Role, smooth: bool) {
        if profile.len() < 3 || y1 <= y0 {
            return;
        }
        let mut prof: Vec<Vec2> = profile.to_vec();
        if signed_area(&prof) < 0.0 {
            prof.reverse();
        }
        let h = y1 - y0;
        let (top, bottom) = (top.clamp(0.0, h * 0.45), bottom.clamp(0.0, h * 0.45));
        let limit = min_edge(&prof) * 0.45;
        let (top, bottom) = (top.min(limit), bottom.min(limit));
        let ring = |inset: f32, y: f32| -> Vec<Vec3> { offset(&prof, -inset).into_iter().map(|p| Vec3::new(p.x, y, p.y)).collect() };
        let mut rings = Vec::new();
        if bottom > 0.0 {
            rings.push(ring(bottom, y0));
        }
        rings.push(ring(0.0, y0 + bottom));
        rings.push(ring(0.0, y1 - top));
        if top > 0.0 {
            rings.push(ring(top, y1));
        }
        let mut v_acc = 0.0;
        for w in rings.windows(2) {
            let rise = (w[1][0] - w[0][0]).length();
            self.band(&w[0], &w[1], v_acc, role, smooth, false);
            v_acc += rise;
        }
        let top_ring = rings.last().expect("rings").clone();
        self.cap(&top_ring, Vec3::Y, role);
        let mut bottom_ring = rings[0].clone();
        bottom_ring.reverse();
        self.cap(&bottom_ring, Vec3::NEG_Y, role);
    }

    /// Quads between two closed rings of the same length, both running
    /// counter-clockwise seen from +Y (`b` above `a`): normals face out
    /// (or in, for the walls of an opening).
    fn band(&mut self, a: &[Vec3], b: &[Vec3], v0: f32, role: Role, smooth: bool, inward: bool) {
        let flip = if inward { -1.0 } else { 1.0 };
        let n = a.len();
        let mut u = 0.0;
        if smooth {
            // Round sides: one normal per column (around tangent x rise).
            let normals: Vec<Vec3> = (0..n).map(|i| {
                let t = a[(i + 1) % n] - a[(i + n - 1) % n];
                let d = b[i] - a[i];
                d.cross(t).normalize_or(Vec3::X) * flip
            }).collect();
            let mut ids = Vec::with_capacity(n + 1);
            for i in 0..=n {
                let k = i % n;
                let rise = (b[k] - a[k]).length();
                let va = self.vert(a[k], normals[k], Vec2::new(u, v0), role);
                let vb = self.vert(b[k], normals[k], Vec2::new(u, v0 + rise), role);
                ids.push((va, vb));
                u += (a[(k + 1) % n] - a[k]).length();
            }
            for i in 0..n {
                let (a0, b0) = ids[i];
                let (a1, b1) = ids[i + 1];
                self.quad(a0, a1, b1, b0);
            }
            return;
        }
        for i in 0..n {
            let j = (i + 1) % n;
            let rise = if (b[i] - a[i]).length_squared() > 1e-14 { b[i] - a[i] } else { b[j] - a[j] };
            let nn = rise.cross(a[j] - a[i]).normalize_or(Vec3::X) * flip;
            let w = (a[j] - a[i]).length();
            let rise = (b[i] - a[i]).length();
            let q = [
                self.vert(a[i], nn, Vec2::new(u, v0), role),
                self.vert(a[j], nn, Vec2::new(u + w, v0), role),
                self.vert(b[j], nn, Vec2::new(u + w, v0 + rise), role),
                self.vert(b[i], nn, Vec2::new(u, v0 + rise), role),
            ];
            self.quad(q[0], q[1], q[2], q[3]);
            u += w;
        }
    }

    /// A box centered at `c` with full size `s`, every edge chamfered by
    /// `bevel` (the 12 edges and 8 corners all cut).
    pub fn boxy(&mut self, c: Vec3, s: Vec3, bevel: f32, role: Role) {
        let h = s * 0.5;
        let b = bevel.clamp(0.0, s.min_element() * 0.45);
        let prof = if b > 0.0 {
            vec![
                Vec2::new(-h.x + b, -h.z), Vec2::new(h.x - b, -h.z), Vec2::new(h.x, -h.z + b), Vec2::new(h.x, h.z - b),
                Vec2::new(h.x - b, h.z), Vec2::new(-h.x + b, h.z), Vec2::new(-h.x, h.z - b), Vec2::new(-h.x, -h.z + b),
            ]
        } else {
            vec![Vec2::new(-h.x, -h.z), Vec2::new(h.x, -h.z), Vec2::new(h.x, h.z), Vec2::new(-h.x, h.z)]
        };
        let prof: Vec<Vec2> = prof.into_iter().map(|p| p + Vec2::new(c.x, c.z)).collect();
        self.prism(&prof, c.y - h.y, c.y + h.y, b, b, role, false);
    }

    /// A surface of revolution about +Y: `profile` points are (radius,
    /// height), bottom first. Profile corners sharper than `crease`
    /// degrees stay hard; a radius of 0 closes the end.
    pub fn lathe(&mut self, profile: &[Vec2], segs: usize, role: Role, crease: f32) {
        if profile.len() < 2 {
            return;
        }
        let segs = segs.max(3);
        let cos_crease = crease.to_radians().cos();
        // 2D outward normal of each profile segment (radius out, height up).
        let seg_n: Vec<Vec2> = profile.windows(2).map(|w| {
            let d = w[1] - w[0];
            Vec2::new(d.y, -d.x).normalize_or(Vec2::X)
        }).collect();
        let mut v_acc = 0.0;
        for (k, w) in profile.windows(2).enumerate() {
            let len = (w[1] - w[0]).length();
            if len < 1e-7 {
                continue;
            }
            // Normals at the two ends: shared with the neighbor across a
            // soft corner, the segment's own across a hard one.
            let end_n = |at: usize, other: Option<usize>| -> Vec2 {
                let own = seg_n[at];
                match other {
                    Some(o) if seg_n[o].dot(own) > cos_crease => (own + seg_n[o]).normalize_or(own),
                    _ => own,
                }
            };
            let n0 = end_n(k, k.checked_sub(1));
            let n1 = end_n(k, (k + 1 < seg_n.len()).then_some(k + 1));
            let mut ids = Vec::with_capacity((segs + 1) * 2);
            let circ = std::f32::consts::TAU * profile.iter().map(|p| p.x).fold(0.0, f32::max);
            for s in 0..=segs {
                let a = s as f32 / segs as f32 * std::f32::consts::TAU;
                let (sin, cos) = a.sin_cos();
                let radial = Vec3::new(cos, 0.0, sin);
                let pos = |p: Vec2| radial * p.x + Vec3::Y * p.y;
                let nrm = |n: Vec2| (radial * n.x + Vec3::Y * n.y).normalize_or(Vec3::Y);
                let u = s as f32 / segs as f32 * circ;
                let a0 = self.vert(pos(w[0]), nrm(n0), Vec2::new(u, v_acc), role);
                let a1 = self.vert(pos(w[1]), nrm(n1), Vec2::new(u, v_acc + len), role);
                ids.push((a0, a1));
            }
            for s in 0..segs {
                let (a0, a1) = ids[s];
                let (b0, b1) = ids[s + 1];
                if w[0].x < 1e-6 {
                    self.tri(a0, b1, a1);
                } else if w[1].x < 1e-6 {
                    self.tri(a0, b0, a1);
                } else {
                    self.quad(a0, b0, b1, a1);
                }
            }
            v_acc += len;
        }
    }

    /// A tube along `path` with radius `r(t)` (t = arc length), `sides`
    /// around; ends capped flat when `caps`. Frames are carried along the
    /// path (no twist flips).
    pub fn tube(&mut self, path: &[Vec3], r: &dyn Fn(f32) -> f32, sides: usize, role: Role, caps: bool) {
        let path: Vec<Vec3> = path.iter().copied().fold(Vec::new(), |mut acc: Vec<Vec3>, p| {
            if acc.last().is_none_or(|q| q.distance(p) > 1e-6) { acc.push(p); }
            acc
        });
        if path.len() < 2 {
            return;
        }
        let sides = sides.max(3);
        let n = path.len();
        let tangents: Vec<Vec3> = (0..n).map(|i| {
            let a = path[i.saturating_sub(1)];
            let b = path[(i + 1).min(n - 1)];
            (b - a).normalize_or(Vec3::Y)
        }).collect();
        let mut normal = tangents[0].any_orthonormal_vector();
        let mut frames = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                // Parallel transport: rotate the previous normal onto this tangent.
                let q = glam::Quat::from_rotation_arc(tangents[i - 1], tangents[i]);
                normal = (q * normal).normalize_or(tangents[i].any_orthonormal_vector());
                normal = (normal - tangents[i] * normal.dot(tangents[i])).normalize_or(tangents[i].any_orthonormal_vector());
            }
            frames.push((normal, tangents[i].cross(normal)));
        }
        let mut arc = vec![0.0f32; n];
        for i in 1..n {
            arc[i] = arc[i - 1] + path[i].distance(path[i - 1]);
        }
        let mut rings = Vec::with_capacity(n);
        for i in 0..n {
            let rr = r(arc[i]).max(1e-5);
            // Slope of the radius along the path tilts the normal (corrugation).
            let h = 1e-3 * arc[n - 1].max(1e-3);
            let dr = (r((arc[i] + h).min(arc[n - 1])) - r((arc[i] - h).max(0.0))) / (2.0 * h);
            let (nn, bn) = frames[i];
            let mut ring = Vec::with_capacity(sides + 1);
            for s in 0..=sides {
                let a = s as f32 / sides as f32 * std::f32::consts::TAU;
                let radial = nn * a.cos() + bn * a.sin();
                let normal = (radial - tangents[i] * dr).normalize_or(radial);
                let uv = Vec2::new(a * rr, arc[i]);
                ring.push(self.vert(path[i] + radial * rr, normal, uv, role));
            }
            rings.push(ring);
        }
        for i in 0..n - 1 {
            for s in 0..sides {
                let (a0, a1, b0, b1) = (rings[i][s], rings[i][s + 1], rings[i + 1][s], rings[i + 1][s + 1]);
                self.tri(a0, a1, b1);
                self.tri(a0, b1, b0);
            }
            self.b.edge(rings[i][0], rings[i + 1][0]);
        }
        if caps {
            for (i, sign) in [(0usize, -1.0f32), (n - 1, 1.0)] {
                let rr = r(arc[i]).max(1e-5);
                let (nn, bn) = frames[i];
                let pts: Vec<Vec3> = (0..sides).map(|s| {
                    let a = s as f32 / sides as f32 * std::f32::consts::TAU;
                    path[i] + (nn * a.cos() + bn * a.sin()) * rr
                }).collect();
                let pts: Vec<Vec3> = if sign > 0.0 { pts } else { pts.into_iter().rev().collect() };
                self.cap(&pts, tangents[i] * sign, role);
            }
        }
    }

    /// A flat band `width` wide and `thick` thick swept along `path`: its
    /// underside on the path, its thickness toward `up` (one per point).
    pub fn strip(&mut self, path: &[Vec3], up: &[Vec3], width: f32, thick: f32, role: Role, caps: bool) {
        let n = path.len();
        if n < 2 || up.len() != n {
            return;
        }
        let frames: Vec<(Vec3, Vec3, Vec3)> = (0..n).map(|i| {
            let t = (path[(i + 1).min(n - 1)] - path[i.saturating_sub(1)]).normalize_or(Vec3::X);
            let u = (up[i] - t * up[i].dot(t)).normalize_or(t.any_orthonormal_vector());
            (t, u, t.cross(u))
        }).collect();
        let hw = width * 0.5;
        let corner = |i: usize, k: usize| -> Vec3 {
            let (_, u, s) = frames[i];
            match k { 0 => path[i] - s * hw, 1 => path[i] + s * hw, 2 => path[i] + s * hw + u * thick, _ => path[i] - s * hw + u * thick }
        };
        let mut arc = 0.0;
        let mut arcs = vec![0.0f32; n];
        for i in 1..n {
            arc += path[i].distance(path[i - 1]);
            arcs[i] = arc;
        }
        for face in 0..4 {
            let (k0, k1) = (face, (face + 1) % 4);
            let normal = |i: usize| -> Vec3 {
                let (_, u, s) = frames[i];
                match face { 0 => -u, 1 => s, 2 => u, _ => -s }
            };
            let across = if face % 2 == 0 { width } else { thick };
            let mut prev: Option<(u32, u32)> = None;
            for (i, &arc) in arcs.iter().enumerate() {
                let a = self.vert(corner(i, k0), normal(i), Vec2::new(0.0, arc), role);
                let b = self.vert(corner(i, k1), normal(i), Vec2::new(across, arc), role);
                if let Some((pa, pb)) = prev {
                    self.quad(pa, pb, b, a);
                }
                prev = Some((a, b));
            }
        }
        if caps {
            for (i, sign) in [(0usize, -1.0f32), (n - 1, 1.0)] {
                let pts: Vec<Vec3> = (0..4).map(|k| corner(i, k)).collect();
                let pts = if sign > 0.0 { pts } else { pts.into_iter().rev().collect() };
                // Corners run counter-clockwise seen from behind the tangent.
                let t = frames[i].0 * sign;
                let ordered = if (pts[1] - pts[0]).cross(pts[2] - pts[0]).dot(t) >= 0.0 { pts } else { pts.into_iter().rev().collect() };
                self.cap(&ordered, t, role);
            }
        }
    }

    /// A raised frame: walls outside `outer` (XZ) up to `h` (top edge
    /// chamfered by `bevel`), a flat top between `outer` and `inner`, walls
    /// inside `inner` down to `floor` and a floor there in `floor_role`.
    /// Both outlines need the same number of points.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(&mut self, outer: &[Vec2], inner: &[Vec2], h: f32, bevel: f32, floor: f32, role: Role, floor_role: Role) {
        if outer.len() != inner.len() || outer.len() < 3 {
            return;
        }
        let ccw = |p: &[Vec2]| { let mut v = p.to_vec(); if signed_area(&v) < 0.0 { v.reverse(); } v };
        let (outer, inner) = (ccw(outer), ccw(inner));
        let b = bevel.clamp(0.0, h * 0.45);
        let at = |p: &[Vec2], y: f32| -> Vec<Vec3> { p.iter().map(|q| Vec3::new(q.x, y, q.y)).collect() };
        let o0 = at(&outer, 0.0);
        let o1 = at(&outer, h - b);
        let o2 = at(&offset(&outer, -b), h);
        let i_top = at(&inner, h);
        let i_floor = at(&inner, floor.clamp(0.0, h));
        self.band(&o0, &o1, 0.0, role, false, false);
        if b > 0.0 {
            self.band(&o1, &o2, h - b, role, false, false);
        }
        // Top: quads between the outer and inner outlines.
        let n = outer.len();
        for i in 0..n {
            let j = (i + 1) % n;
            let q = [o2[i], o2[j], i_top[j], i_top[i]];
            let ids: Vec<u32> = q.iter().map(|p| self.vert(*p, Vec3::Y, Vec2::new(p.x, p.z), role)).collect();
            self.quad(ids[0], ids[1], ids[2], ids[3]);
        }
        // Inner walls face in, toward the opening.
        self.band(&i_floor, &i_top, 0.0, role, false, true);
        self.cap(&i_floor, Vec3::Y, floor_role);
        let mut bottom = o0;
        bottom.reverse();
        self.cap(&bottom, Vec3::NEG_Y, role);
    }

    /// A round rod from `a` to `b` of radius `r`, its ends chamfered by
    /// `bevel`.
    pub fn rod(&mut self, a: Vec3, b: Vec3, r: f32, bevel: f32, segs: usize, role: Role) {
        let len = a.distance(b);
        if len < 1e-6 {
            return;
        }
        let bv = bevel.clamp(0.0, (len * 0.4).min(r * 0.8));
        let prof = [Vec2::new(0.0, 0.0), Vec2::new(r - bv, 0.0), Vec2::new(r, bv), Vec2::new(r, len - bv), Vec2::new(r - bv, len), Vec2::new(0.0, len)];
        self.with(along(a, b), |g| g.lathe(&prof, segs, role, 30.0));
    }

    /// The finished mesh: smart-material masks and weathering measured
    /// over bands about `wear` wide.
    pub fn finish(self, wear: f32) -> crate::mesh::Mesh {
        self.b.finish(wear.max(1e-4))
    }
}

/// A transform taking +Y (0..1 along it) to the segment from `a` to `b`
/// (lengths unscaled: local y runs 0..|b - a|).
pub fn along(a: Vec3, b: Vec3) -> Mat4 {
    let d = (b - a).normalize_or(Vec3::Y);
    Mat4::from_rotation_translation(glam::Quat::from_rotation_arc(Vec3::Y, d), a)
}

/// Two axes spanning the plane with normal `n`.
pub fn plane_axes(n: Vec3) -> (Vec3, Vec3) {
    let n = n.normalize_or(Vec3::Y);
    let u = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::Y.cross(n).normalize() };
    let u = (u - n * u.dot(n)).normalize_or(n.any_orthonormal_vector());
    (u, n.cross(u))
}

pub fn signed_area(p: &[Vec2]) -> f32 {
    (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a.x * b.y - b.x * a.y }).sum::<f32>() * 0.5
}

fn min_edge(p: &[Vec2]) -> f32 {
    (0..p.len()).map(|i| p[i].distance(p[(i + 1) % p.len()])).fold(f32::MAX, f32::min)
}

/// The polygon (counter-clockwise) moved outward by `d` (inward when
/// negative), with mitered corners (limited so spikes stay short).
pub fn offset(p: &[Vec2], d: f32) -> Vec<Vec2> {
    if d == 0.0 {
        return p.to_vec();
    }
    let n = p.len();
    (0..n).map(|i| {
        let a = p[(i + n - 1) % n];
        let b = p[i];
        let c = p[(i + 1) % n];
        let n1 = { let e = (b - a).normalize_or(Vec2::X); Vec2::new(e.y, -e.x) };
        let n2 = { let e = (c - b).normalize_or(Vec2::X); Vec2::new(e.y, -e.x) };
        let m = (n1 + n2).normalize_or(n1);
        let scale = 1.0 / m.dot(n1).max(0.35);
        b + m * d * scale
    }).collect()
}

/// Triangles of a simple polygon (either winding) by ear clipping.
pub fn ear_clip(p: &[Vec2]) -> Vec<[usize; 3]> {
    let n = p.len();
    let mut idx: Vec<usize> = (0..n).collect();
    if signed_area(p) < 0.0 {
        idx.reverse();
    }
    let mut out = Vec::with_capacity(n.saturating_sub(2));
    let cross = |a: Vec2, b: Vec2, c: Vec2| (b - a).perp_dot(c - a);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            if cross(a, b, c) <= 1e-12 {
                continue;
            }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic { return false; }
                let q = p[j];
                cross(a, b, q) > 0.0 && cross(b, c, q) > 0.0 && cross(c, a, q) > 0.0
            });
            if inside {
                continue;
            }
            out.push([ia, ib, ic]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            // Degenerate leftovers: fan the rest.
            break;
        }
    }
    for k in 1..idx.len().saturating_sub(1) {
        out.push([idx[0], idx[k], idx[k + 1]]);
    }
    out
}

/// A circle of `n` points (counter-clockwise from +X) in XZ.
pub fn circle(r: f32, n: usize) -> Vec<Vec2> {
    (0..n).map(|i| { let a = i as f32 / n as f32 * std::f32::consts::TAU; Vec2::new(a.cos(), a.sin()) * r }).collect()
}

/// A rectangle w x d in XZ with corners chamfered by `c`.
pub fn rect(w: f32, d: f32, c: f32) -> Vec<Vec2> {
    let (x, z) = (w * 0.5, d * 0.5);
    let c = c.clamp(0.0, x.min(z) * 0.9);
    if c <= 0.0 {
        return vec![Vec2::new(-x, -z), Vec2::new(x, -z), Vec2::new(x, z), Vec2::new(-x, z)];
    }
    vec![Vec2::new(-x + c, -z), Vec2::new(x - c, -z), Vec2::new(x, -z + c), Vec2::new(x, z - c), Vec2::new(x - c, z), Vec2::new(-x + c, z), Vec2::new(-x, z - c), Vec2::new(-x, -z + c)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo() -> Geo {
        Geo::new(Role::ALL.iter().map(|r| (Material { name: r.name().into(), ..Material::default() }, [0.5; 3])).collect(), 1.0, 1.0)
    }

    /// Every triangle winds counter-clockwise around its vertex normals and
    /// the shell is closed (each edge, by position, used an even number of times).
    fn closed_and_outward(mesh: &crate::mesh::Mesh) {
        let key = |p: [f32; 3]| p.map(|x| (x * 1e5).round() as i64);
        let mut edges = std::collections::HashMap::new();
        for t in mesh.indices.chunks_exact(3) {
            let p: Vec<Vec3> = t.iter().map(|&i| Vec3::from(mesh.vertices[i as usize].pos)).collect();
            let n: Vec3 = t.iter().map(|&i| Vec3::from(mesh.vertices[i as usize].normal)).sum();
            let face = (p[1] - p[0]).cross(p[2] - p[0]);
            assert!(face.dot(n) >= -1e-9, "backward triangle");
            for k in 0..3 {
                let (a, b) = (key(mesh.vertices[t[k] as usize].pos), key(mesh.vertices[t[(k + 1) % 3] as usize].pos));
                if a == b { continue; }
                *edges.entry(if a < b { (a, b) } else { (b, a) }).or_insert(0) += 1;
            }
        }
        assert!(edges.values().all(|c| c % 2 == 0), "open edges");
    }

    #[test]
    fn chamfered_box_and_lathe_are_closed_and_face_out() {
        let mut g = geo();
        g.boxy(Vec3::new(0.0, 0.5, 0.0), Vec3::new(1.0, 1.0, 2.0), 0.1, Role::Metal);
        let mesh = g.finish(0.05);
        closed_and_outward(&mesh);
        let (lo, hi) = mesh.bounds();
        assert!(lo.distance(Vec3::new(-0.5, 0.0, -1.0)) < 1e-5 && hi.distance(Vec3::new(0.5, 1.0, 1.0)) < 1e-5);
        let mut g = geo();
        g.lathe(&[Vec2::new(0.0, 0.0), Vec2::new(0.5, 0.0), Vec2::new(0.5, 0.3), Vec2::new(0.2, 0.5), Vec2::new(0.0, 0.5)], 24, Role::Metal, 30.0);
        closed_and_outward(&g.finish(0.05));
        let mut g = geo();
        let path: Vec<Vec3> = (0..20).map(|i| Vec3::new(i as f32 * 0.1, (i as f32 * 0.3).sin() * 0.2, 0.0)).collect();
        g.tube(&path, &|t| 0.05 + 0.01 * (t * 40.0).sin(), 12, Role::Rubber, true);
        closed_and_outward(&g.finish(0.05));
    }

    #[test]
    fn ear_clipping_handles_concave_polygons() {
        let l = [Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0), Vec2::new(2.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 2.0), Vec2::new(0.0, 2.0)];
        let tris = ear_clip(&l);
        assert_eq!(tris.len(), 4);
        let area: f32 = tris.iter().map(|t| signed_area(&[l[t[0]], l[t[1]], l[t[2]]])).sum();
        assert!((area - 3.0).abs() < 1e-5);
    }
}
