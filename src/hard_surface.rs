//! Polygon fields and bounded Hermite QEF placement. No external geometry library.
use super::{Form, Geo};
use glam::{Mat3, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Join {
    #[default]
    Round,
    Sharp,
    Chamfer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Corner {
    Point(Vec2),
    Rounded { point: Vec2, radius: f32 },
    Smooth { point: Vec2, smooth: bool },
}
impl Corner {
    fn point(&self) -> Vec2 {
        match *self {
            Self::Point(p) | Self::Rounded { point: p, .. } | Self::Smooth { point: p, .. } => p,
        }
    }
}
/// How array copies along `points` turn: `fixed` (as authored) or
/// `path` (with the path: each copy turns by how far the path has turned
/// since its first point, so links wrap around a track loop).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Orient {
    #[default]
    Fixed,
    Path,
}

pub fn is_fixed(o: &Orient) -> bool {
    *o == Orient::Fixed
}

/// How far each segment of a path has turned from the first one, carried
/// segment to segment (parallel transport), so a closed loop turns all
/// the way around without flipping.
pub fn path_turns(points: &[Vec3]) -> Vec<Quat> {
    let mut turns = vec![Quat::IDENTITY; points.len().saturating_sub(1)];
    for j in 1..turns.len() {
        let (t0, t1) = ((points[j] - points[j - 1]).normalize_or_zero(), (points[j + 1] - points[j]).normalize_or_zero());
        let step = if t0 == Vec3::ZERO || t1 == Vec3::ZERO { Quat::IDENTITY } else { Quat::from_rotation_arc(t0, t1) };
        turns[j] = (step * turns[j - 1]).normalize();
    }
    turns
}

pub fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    let n = usize::deserialize(d)?;
    if !(1..=512).contains(&n) {
        return Err(serde::de::Error::custom("array count must be 1-512"));
    }
    Ok(n)
}
pub fn profile<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Corner>, D::Error> {
    let p = Vec::<Corner>::deserialize(d)?;
    if !(3..=128).contains(&p.len()) || p.iter().any(|c| {
        !c.point().is_finite()
            || matches!(c, Corner::Rounded { radius, .. } if !radius.is_finite() || *radius < 0.0)
    }) {
        return Err(serde::de::Error::custom(
            "profile needs 3-128 finite [u,v] points or {point:[u,v],radius:nonnegative}",
        ));
    }
    let area: f32 = p
        .iter()
        .enumerate()
        .map(|(i, c)| c.point().perp_dot(p[(i + 1) % p.len()].point()))
        .sum();
    if area.abs() < 1e-10
        || p.iter()
            .enumerate()
            .any(|(i, c)| c.point().distance_squared(p[(i + 1) % p.len()].point()) < 1e-12)
    {
        return Err(serde::de::Error::custom(
            "profile must have area and distinct adjacent points",
        ));
    }
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            if j == i + 1 || (i == 0 && j == p.len() - 1) {
                continue;
            }
            let (a, b, c, e) = (
                p[i].point(),
                p[(i + 1) % p.len()].point(),
                p[j].point(),
                p[(j + 1) % p.len()].point(),
            );
            if (b - a).perp_dot(c - a) * (b - a).perp_dot(e - a) < 0.0
                && (e - c).perp_dot(a - c) * (e - c).perp_dot(b - c) < 0.0
            {
                return Err(serde::de::Error::custom(
                    "profile must be simple (no crossing edges)",
                ));
            }
        }
    }
    Ok(p)
}
fn polygon(corners: &[Corner]) -> Vec<Vec2> {
    if corners
        .iter()
        .any(|c| matches!(c, Corner::Smooth { smooth: true, .. }))
        && corners.iter().any(|c| matches!(c, Corner::Rounded { .. }))
    {
        let filleted: Vec<_> = corners
            .iter()
            .map(|c| {
                if matches!(c, Corner::Smooth { .. }) {
                    Corner::Point(c.point())
                } else {
                    c.clone()
                }
            })
            .collect();
        let expanded: Vec<_> = polygon(&filleted)
            .into_iter()
            .map(|p| {
                if corners
                    .iter()
                    .any(|c| matches!(c, Corner::Smooth { smooth: true, .. }) && c.point() == p)
                {
                    Corner::Smooth {
                        point: p,
                        smooth: true,
                    }
                } else {
                    Corner::Point(p)
                }
            })
            .collect();
        return polygon(&expanded);
    }
    if corners
        .iter()
        .any(|c| matches!(c, Corner::Smooth { smooth: true, .. }))
    {
        let mut result = Vec::new();
        for i in 0..corners.len() {
            let j = (i + 1) % corners.len();
            let a = corners[i].point();
            let b = corners[j].point();
            let tangent = |k: usize| {
                if matches!(corners[k], Corner::Smooth { smooth: true, .. }) {
                    (corners[(k + 1) % corners.len()].point()
                        - corners[(k + corners.len() - 1) % corners.len()].point())
                        * 0.5
                } else {
                    b - a
                }
            };
            let (u, v) = (tangent(i), tangent(j));
            let steps = if matches!(corners[i], Corner::Smooth { smooth: true, .. })
                || matches!(corners[j], Corner::Smooth { smooth: true, .. })
            {
                24
            } else {
                1
            };
            for k in 0..steps {
                let t = k as f32 / steps as f32;
                result.push(hermite2(a, b, u, v, t));
            }
        }
        return result;
    }
    let mut p = Vec::new();
    for (i, c) in corners.iter().enumerate() {
        let b = c.point();
        if let Corner::Rounded { radius, .. } = c {
            if *radius == 0.0 {
                p.push(b);
                continue;
            }
            let a = corners[(i + corners.len() - 1) % corners.len()].point();
            let e = corners[(i + 1) % corners.len()].point();
            let u = (a - b).normalize();
            let v = (e - b).normalize();
            let angle = u.dot(v).clamp(-1.0, 1.0).acos();
            if angle < 1e-4 || std::f32::consts::PI - angle < 1e-4 {
                p.push(b);
                continue;
            }
            let distance = (radius / (angle * 0.5).tan().max(1e-5))
                .min(a.distance(b) * 0.49)
                .min(e.distance(b) * 0.49);
            let actual = distance * (angle * 0.5).tan();
            let center = b + (u + v).normalize_or_zero() * (actual / (angle * 0.5).sin().max(1e-5));
            let start = b + u * distance - center;
            let end = b + v * distance - center;
            let sweep = start.perp_dot(end).atan2(start.dot(end));
            for k in 0..=8 {
                let t = sweep * k as f32 / 8.0;
                p.push(
                    center
                        + Vec2::new(
                            start.x * t.cos() - start.y * t.sin(),
                            start.x * t.sin() + start.y * t.cos(),
                        ),
                );
            }
        } else {
            p.push(b);
        }
    }
    p
}
fn polygon_sdf(p: Vec2, points: &[Vec2]) -> f32 {
    polygon_sdf_mode(p, points, false)
}
fn polygon_sdf_mode(p: Vec2, points: &[Vec2], lathe: bool) -> f32 {
    let mut distance = f32::MAX;
    let mut inside = false;
    for (i, &a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        let e = b - a;
        // A profile edge on the revolution axis is not an external surface.
        // Including it creates a zero-distance tube through solid bosses.
        if !lathe || a.x != 0.0 || b.x != 0.0 {
            distance = distance.min(p.distance(
                a + e * ((p - a).dot(e) / e.length_squared().max(1e-20)).clamp(0.0, 1.0),
            ));
        }
        let x = if lathe { p.x.max(1e-7) } else { p.x };
        if (a.y > p.y) != (b.y > p.y) && x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    if inside { -distance } else { distance }
}
/// Exact primitive features within a dual cell. Straight rims should not
/// inherit errors from nonlinear distance interpolation outside the solid.
pub(super) struct FeatureProject {
    p: Vec3,
    lo: Vec3,
    hi: Vec3,
    corner: Option<(f32, Vec3)>,
    edge: Option<(f32, Vec3)>,
}
impl FeatureProject {
    pub fn new(p: Vec3, lo: Vec3, hi: Vec3) -> Self {
        Self {
            p,
            lo,
            hi,
            corner: None,
            edge: None,
        }
    }
    pub fn corner(&mut self, q: Vec3) {
        if q.cmpge(self.lo).all() && q.cmple(self.hi).all() {
            let d = q.distance_squared(self.p);
            if self.corner.is_none_or(|(best, _)| d < best) {
                self.corner = Some((d, q));
            }
        }
    }
    pub fn edge(&mut self, a: Vec3, b: Vec3) {
        let v = b - a;
        let (mut left, mut right) = (0.0f32, 1.0f32);
        for i in 0..3 {
            if v[i].abs() < 1e-12 {
                if a[i] < self.lo[i] || a[i] > self.hi[i] {
                    return;
                }
            } else {
                let x = (self.lo[i] - a[i]) / v[i];
                let y = (self.hi[i] - a[i]) / v[i];
                left = left.max(x.min(y));
                right = right.min(x.max(y));
            }
        }
        if left > right {
            return;
        }
        let t = ((self.p - a).dot(v) / v.length_squared().max(1e-20)).clamp(left, right);
        let q = a + v * t;
        let d = q.distance_squared(self.p);
        if self.edge.is_none_or(|(best, _)| d < best) {
            self.edge = Some((d, q));
        }
    }
    pub fn point(self) -> Option<Vec3> {
        self.corner.or(self.edge).map(|(_, p)| p)
    }
}

fn frame(axis: Vec3) -> Quat {
    Quat::from_rotation_arc(Vec3::Z, axis.normalize_or(Vec3::Z))
}
fn intersection(a: f32, b: f32) -> f32 {
    Vec2::new(a, b).max(Vec2::ZERO).length() + a.max(b).min(0.0)
}

/// Exact planar primitives, tessellated to the requested density for rigs,
/// paint/bake sampling and weathering. CSG/sculpted objects use the field mesh.
pub(super) fn planar_mesh(form: &Form, voxel: f32) -> Option<crate::mesh::Mesh> {
    use crate::mesh::{Mesh, Vertex};
    let (mut positions, mut indices) = match form {
        Form::Box {
            center,
            size,
            rotate,
            round,
        } if *round == 0.0 => {
            let half = (size.abs() * 0.5).max(Vec3::splat(1e-4));
            let rot = super::rotation(*rotate);
            let p: Vec<_> = super::CORNERS
                .iter()
                .map(|&(x, y, z)| {
                    *center
                        + rot
                            * (half
                                * Vec3::new(
                                    if x == 0 { -1.0 } else { 1.0 },
                                    if y == 0 { -1.0 } else { 1.0 },
                                    if z == 0 { -1.0 } else { 1.0 },
                                ))
                })
                .collect();
            let mut indices = Vec::new();
            for q in [
                [0, 2, 3, 1],
                [4, 5, 7, 6],
                [0, 1, 5, 4],
                [2, 6, 7, 3],
                [0, 4, 6, 2],
                [1, 3, 7, 5],
            ] {
                indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
            }
            (p, indices)
        }
        Form::Extrude {
            profile,
            center,
            axis,
            depth,
            bevel,
        } if *bevel == 0.0 => {
            let mut p = polygon(profile);
            let area: f32 = p
                .iter()
                .enumerate()
                .map(|(i, v)| v.perp_dot(p[(i + 1) % p.len()]))
                .sum();
            if area < 0.0 {
                p.reverse();
            }
            let mut remaining: Vec<usize> = (0..p.len()).collect();
            let mut caps = Vec::new();
            while remaining.len() > 3 {
                let mut found = false;
                for i in 0..remaining.len() {
                    let a = remaining[(i + remaining.len() - 1) % remaining.len()];
                    let b = remaining[i];
                    let c = remaining[(i + 1) % remaining.len()];
                    if (p[b] - p[a]).perp_dot(p[c] - p[b]) <= 1e-12 {
                        continue;
                    }
                    if remaining.iter().any(|&j| {
                        j != a
                            && j != b
                            && j != c
                            && (p[b] - p[a]).perp_dot(p[j] - p[a]) >= 0.0
                            && (p[c] - p[b]).perp_dot(p[j] - p[b]) >= 0.0
                            && (p[a] - p[c]).perp_dot(p[j] - p[c]) >= 0.0
                    }) {
                        continue;
                    }
                    caps.push([a as u32, b as u32, c as u32]);
                    remaining.remove(i);
                    found = true;
                    break;
                }
                if !found {
                    return None;
                }
            }
            caps.push([
                remaining[0] as u32,
                remaining[1] as u32,
                remaining[2] as u32,
            ]);
            let n = p.len() as u32;
            let rot = frame(*axis);
            let positions: Vec<_> = [-depth.abs().max(1e-4) * 0.5, depth.abs().max(1e-4) * 0.5]
                .into_iter()
                .flat_map(|z| {
                    p.iter()
                        .map(move |p| *center + rot * Vec3::new(p.x, p.y, z))
                })
                .collect();
            let mut indices = Vec::new();
            for [a, b, c] in caps {
                indices.extend([c, b, a, a + n, b + n, c + n]);
            }
            for i in 0..n {
                let j = (i + 1) % n;
                indices.extend([i, j, j + n, i, j + n, i + n]);
            }
            (positions, indices)
        }
        _ => return None,
    };
    let limit = (voxel * 1.7).powi(2);
    loop {
        let mut split = std::collections::HashMap::new();
        for t in indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if positions[a as usize].distance_squared(positions[b as usize]) > limit {
                    let key = (a.min(b), a.max(b));
                    split.entry(key).or_insert_with(|| {
                        let i = positions.len() as u32;
                        positions.push((positions[a as usize] + positions[b as usize]) * 0.5);
                        i
                    });
                }
            }
        }
        if split.is_empty() {
            break;
        }
        let mut next = Vec::with_capacity(indices.len() * 2);
        for t in indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]];
            let mid = |x: u32, y: u32| split.get(&(x.min(y), x.max(y))).copied();
            match (mid(a, b), mid(b, c), mid(c, a)) {
                (None, None, None) => next.extend([a, b, c]),
                (Some(m), None, None) => next.extend([a, m, c, m, b, c]),
                (None, Some(m), None) => next.extend([b, m, a, m, c, a]),
                (None, None, Some(m)) => next.extend([c, m, b, m, a, b]),
                (Some(m), Some(n), None) => next.extend([a, m, c, m, n, c, m, b, n]),
                (None, Some(m), Some(n)) => next.extend([b, m, a, m, n, a, m, c, n]),
                (Some(n), None, Some(m)) => next.extend([c, m, b, m, n, b, m, a, n]),
                (Some(m), Some(n), Some(o)) => next.extend([a, m, o, b, n, m, c, o, n, m, n, o]),
            }
        }
        indices = next;
    }
    let vertices = positions
        .into_iter()
        .map(|p| Vertex {
            pos: p.to_array(),
            normal: [0.0, 1.0, 0.0],
            color: [1.0; 3],
            surface: [0.0; 4],
            material: 0,
            uv: [0.0; 2],
            tangent: [0.0; 4],
            light: [0.0; 2],
            base: [1.0, 1.0, 1.0, -1.0],
            wear: [0.0; 3],
            painted: [0; 4],
        })
        .collect();
    let mut edges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if seen.insert((a.min(b), a.max(b))) {
                edges.extend([a, b]);
            }
        }
    }
    Some(Mesh {
        vertices,
        indices,
        edges,
        ..Mesh::default()
    })
}

/// A consistent tetrahedral fallback for cells whose saddle faces cannot be
/// represented by one dual vertex. Shared grid-edge intersections weld the
/// entire result; this is only used if dual connectivity has a four-face edge.
#[derive(Default)]
pub(super) struct TetraMesh {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    crossings: std::collections::HashMap<(usize, usize), u32>,
}
impl TetraMesh {
    pub fn cell(&mut self, ids: [usize; 8], points: [Vec3; 8], values: [f32; 8]) {
        for tetra in [
            [0, 1, 3, 7],
            [0, 3, 2, 7],
            [0, 2, 6, 7],
            [0, 6, 4, 7],
            [0, 4, 5, 7],
            [0, 5, 1, 7],
        ] {
            let inside: Vec<usize> = tetra.iter().copied().filter(|&i| values[i] < 0.0).collect();
            let outside: Vec<usize> = tetra
                .iter()
                .copied()
                .filter(|&i| values[i] >= 0.0)
                .collect();
            if inside.is_empty() || outside.is_empty() {
                continue;
            }
            let direction = outside.iter().map(|&i| points[i]).sum::<Vec3>() / outside.len() as f32
                - inside.iter().map(|&i| points[i]).sum::<Vec3>() / inside.len() as f32;
            let mut edge = |a: usize, b: usize| {
                let key = (ids[a].min(ids[b]), ids[a].max(ids[b]));
                *self.crossings.entry(key).or_insert_with(|| {
                    let t = (values[a] / (values[a] - values[b])).clamp(0.001, 0.999);
                    let index = self.positions.len() as u32;
                    self.positions.push(points[a].lerp(points[b], t));
                    index
                })
            };
            let triangles = if inside.len() == 1 {
                vec![[
                    edge(inside[0], outside[0]),
                    edge(inside[0], outside[1]),
                    edge(inside[0], outside[2]),
                ]]
            } else if outside.len() == 1 {
                vec![[
                    edge(inside[0], outside[0]),
                    edge(inside[1], outside[0]),
                    edge(inside[2], outside[0]),
                ]]
            } else {
                let q = [
                    edge(inside[0], outside[0]),
                    edge(inside[0], outside[1]),
                    edge(inside[1], outside[1]),
                    edge(inside[1], outside[0]),
                ];
                vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]]
            };
            for mut tri in triangles {
                let p = tri.map(|i| self.positions[i as usize]);
                if (p[1] - p[0]).cross(p[2] - p[0]).dot(direction) < 0.0 {
                    tri.swap(1, 2);
                }
                self.indices.extend(tri);
            }
        }
    }
}

fn hermite2(a: Vec2, b: Vec2, u: Vec2, v: Vec2, t: f32) -> Vec2 {
    a * (2.0 * t * t * t - 3.0 * t * t + 1.0)
        + b * (-2.0 * t * t * t + 3.0 * t * t)
        + u * (t * t * t - 2.0 * t * t + t)
        + v * (t * t * t - t * t)
}
fn spline_path(points: &[Vec3]) -> Vec<Vec3> {
    let mut result = Vec::new();
    for i in 0..points.len() - 1 {
        let a = points[i];
        let b = points[i + 1];
        // Distance-weighted Catmull-Rom tangents avoid loops when adjacent
        // control intervals differ greatly in length.
        let prior = points[i.saturating_sub(1)];
        let next = points[(i + 2).min(points.len() - 1)];
        let len = a.distance(b);
        let u = (b - prior) * (len / (a.distance(prior) + len).max(1e-9));
        let v = (next - a) * (len / (b.distance(next) + len).max(1e-9));
        let steps = (len / 0.002).ceil().clamp(8.0, 96.0) as usize;
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            result.push(
                a * (2.0 * t * t * t - 3.0 * t * t + 1.0)
                    + b * (-2.0 * t * t * t + 3.0 * t * t)
                    + u * (t * t * t - 2.0 * t * t + t)
                    + v * (t * t * t - t * t),
            );
        }
    }
    result.push(*points.last().unwrap());
    result
}

/// A coarse fitting filter with tricubic reconstruction. Independent of
/// render cells, so fine meshes and split rig parts fit the same shell.
pub(super) struct Envelope {
    lo: Vec3,
    step: f32,
    dims: [usize; 3],
    values: Vec<f32>,
}
impl Envelope {
    pub(super) fn cached(key: &[u8], build: impl FnOnce() -> Self) -> std::sync::Arc<Self> {
        use std::{
            hash::{Hash, Hasher},
            sync::{Arc, Mutex, OnceLock},
        };
        type FittingCache = Mutex<Vec<(u64, Arc<Envelope>)>>;
        static CACHE: OnceLock<FittingCache> = OnceLock::new();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let key = hasher.finish();
        let mut cache = CACHE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap();
        if let Some((_, envelope)) = cache.iter().find(|(k, _)| *k == key) {
            return envelope.clone();
        }
        let result = Arc::new(build());
        if cache.len() == 16 {
            cache.remove(0);
        }
        cache.push((key, result.clone()));
        result
    }
    pub(super) fn new(lo: Vec3, hi: Vec3, radius: f32, sample: impl Fn(Vec3) -> f32) -> Self {
        let step = (radius / 2.0)
            .max((hi - lo).max_element() / 48.0)
            .max(0.001);
        let lo = lo - step * 2.0;
        let d = ((hi - lo) / step).ceil().as_uvec3() + 4;
        let dims = [d.x as usize, d.y as usize, d.z as usize];
        let mut values = Vec::with_capacity(dims.iter().product());
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    values.push(sample(lo + Vec3::new(x as f32, y as f32, z as f32) * step));
                }
            }
        }
        Self {
            lo,
            step,
            dims,
            values,
        }
    }
    /// Inflate the filtered shell as a whole, preserving its broad curvature
    /// while covering sampled body protrusions. Never copy local anatomy.
    pub(super) fn fit_outside(
        &mut self,
        body: impl Fn(Vec3) -> f32,
        inside: impl Fn(Vec3) -> bool,
    ) {
        let step = self.step * 0.25;
        let mut inflation = 0.0f32;
        for z in 0..(self.dims[2] - 1) * 4 {
            for y in 0..(self.dims[1] - 1) * 4 {
                for x in 0..(self.dims[0] - 1) * 4 {
                    let mut p = self.lo + Vec3::new(x as f32, y as f32, z as f32) * step;
                    if !inside(p) || body(p).abs() > self.step {
                        continue;
                    }
                    for _ in 0..2 {
                        let e = self.step * 0.03;
                        let g = |a: Vec3| body(p + a * e) - body(p - a * e);
                        let normal =
                            Vec3::new(g(Vec3::X), g(Vec3::Y), g(Vec3::Z)).normalize_or_zero();
                        p -= normal * body(p);
                    }
                    if inside(p) && body(p).abs() < self.step * 0.1 {
                        inflation = inflation.max(self.sample(p));
                    }
                }
            }
        }
        let inflation = inflation.max(0.0) + step * 0.5;
        for value in &mut self.values {
            *value -= inflation;
        }
    }
    pub(super) fn sample(&self, p: Vec3) -> f32 {
        let q = (p - self.lo) / self.step;
        let base = q.floor().as_ivec3();
        let t = q - q.floor();
        let weights = |t: f32| {
            [
                -0.5 * t + t * t - 0.5 * t * t * t,
                1.0 - 2.5 * t * t + 1.5 * t * t * t,
                0.5 * t + 2.0 * t * t - 1.5 * t * t * t,
                -0.5 * t * t + 0.5 * t * t * t,
            ]
        };
        let (wx, wy, wz) = (weights(t.x), weights(t.y), weights(t.z));
        let mut value = 0.0;
        for (z, &wz) in wz.iter().enumerate() {
            for (y, &wy) in wy.iter().enumerate() {
                for (x, &wx) in wx.iter().enumerate() {
                    let x = (base.x + x as i32 - 1).clamp(0, self.dims[0] as i32 - 1) as usize;
                    let y = (base.y + y as i32 - 1).clamp(0, self.dims[1] as i32 - 1) as usize;
                    let z = (base.z + z as i32 - 1).clamp(0, self.dims[2] as i32 - 1) as usize;
                    value += wx * wy * wz * self.values[x + self.dims[0] * (y + self.dims[1] * z)];
                }
            }
        }
        value
    }
}

/// Duplicate coincident render vertices at dihedral creases. Positions and
/// winding stay unchanged; smooth regions keep their field normals.
pub(super) fn split_normals(mesh: &mut crate::mesh::Mesh, normal: impl Fn(Vec3) -> Vec3 + Sync) {
    let original = mesh.vertices.len();
    let mut incident = vec![Vec::new(); original];
    let faces: Vec<Vec3> = mesh
        .indices
        .chunks_exact(3)
        .enumerate()
        .map(|(i, t)| {
            for &v in t {
                incident[v as usize].push(i);
            }
            let p: Vec<Vec3> = t
                .iter()
                .map(|&v| Vec3::from(mesh.vertices[v as usize].pos))
                .collect();
            (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero()
        })
        .collect();
    // The shading groups around each crease vertex (the field normal
    // samples) on any thread; splitting copies a vertex with its position,
    // so the groups do not depend on the splits made before them.
    let (vertices, indices) = (&mesh.vertices, &mesh.indices);
    let mut groups: Vec<Vec<(Vec3, Vec<usize>)>> = vec![Vec::new(); original];
    super::par_chunks(&mut groups, 256, |chunk, out| {
        for (j, out) in out.iter_mut().enumerate() {
            let v = chunk * 256 + j;
            let triangles = &incident[v];
            if !triangles
                .iter()
                .any(|&a| triangles.iter().any(|&b| faces[a].dot(faces[b]) < 0.6))
            {
                continue;
            }
            let position = Vec3::from(vertices[v].pos);
            for &f in triangles {
                let center = indices[f * 3..f * 3 + 3]
                    .iter()
                    .map(|&j| Vec3::from(vertices[j as usize].pos))
                    .sum::<Vec3>()
                    / 3.0;
                let n = normal(position.lerp(center, 0.25));
                if let Some(g) = out
                    .iter_mut()
                    .find(|g| g.0.normalize_or_zero().dot(n) > 0.707)
                {
                    g.0 += n;
                    g.1.push(f);
                } else {
                    out.push((n, vec![f]));
                }
            }
        }
    });
    for (v, groups) in groups.into_iter().enumerate() {
        if groups.len() < 2 {
            continue;
        }
        for (i, (n, ts)) in groups.into_iter().enumerate() {
            let mut vertex = mesh.vertices[v];
            vertex.normal = n.normalize_or_zero().to_array();
            let index = if i == 0 {
                mesh.vertices[v] = vertex;
                v as u32
            } else {
                let j = mesh.vertices.len() as u32;
                mesh.vertices.push(vertex);
                j
            };
            for f in ts {
                for j in 0..3 {
                    let slot = &mut mesh.indices[f * 3 + j];
                    if *slot == v as u32 {
                        *slot = index;
                    }
                }
            }
        }
    }
}

/// The eight corners of a box.
fn box_corners(a: Vec3, b: Vec3) -> [Vec3; 8] {
    super::CORNERS.map(|(x, y, z)| Vec3::new(if x == 0 { a.x } else { b.x }, if y == 0 { a.y } else { b.y }, if z == 0 { a.z } else { b.z }))
}

/// Whether a form's distance is exact, so never less than the distance to
/// its box (or to any shape holding it). Arrays of such forms are too.
/// A mesh's is: the nearest triangle within the bound, exactly, and
/// outside its box positive (with no winding pass; see
/// `MeshField::distance_within`), so at least the box's distance. A
/// missing asset is a speck, a sphere.
/// Whether a form is or copies a mesh.
fn holds_mesh(form: &Form) -> bool {
    match form {
        Form::Mesh { .. } => true,
        Form::Array { source, .. } | Form::Shell { source, .. } => holds_mesh(source),
        _ => false,
    }
}

fn exact(form: &Form) -> bool {
    match form {
        Form::Sphere { .. } | Form::Box { .. } | Form::Extrude { .. } | Form::Lathe { .. } | Form::Torus { .. } | Form::Mesh { .. } => true,
        Form::Array { source, .. } => exact(source),
        _ => false,
    }
}

/// How an array finds the copies near a point.
pub(super) enum Copies {
    /// Every copy (one copy, or a source whose distance is not exact).
    All,
    /// Copy i at `step * i`, unrotated; `span` is the source's extent
    /// along `unit`.
    Line { step: Vec3, unit: Vec3, span: (f32, f32) },
    /// Copy i turned by TAU * i / count about the axis through `center`
    /// (`radial` turns toward `side`).
    Circle { center: Vec3, radial: Vec3, side: Vec3 },
    /// Copies along a path, which can come back near itself.
    Tree(CopyTree),
}

/// One array lookup: the best distance so far and the least lower bound
/// of the copies skipped.
struct Walk {
    q: Vec3,
    bound: f32,
    best: f32,
    floor: f32,
    /// More than rounding can move a bound, so it never skips a copy
    /// whose distance comes out (a few ulps) lower.
    slack: f32,
}

impl Walk {
    /// Whether copies at least `gap` away cannot change the result (then
    /// `gap` is remembered as skipped).
    fn skips(&mut self, gap: f32) -> bool {
        let gap = gap - self.slack;
        let skip = gap > self.best.min(self.bound).max(0.0);
        if skip {
            self.floor = self.floor.min(gap);
        }
        skip
    }

    /// Visits `start`, then the copies of both sides outward in turn. Along
    /// a side `lower` (a lower bound of a copy's distance, told which side)
    /// never decreases, so a side ends at its first copy out of reach.
    fn sides(&mut self, start: usize, down: impl Iterator<Item = usize>, up: impl Iterator<Item = usize>, lower: impl Fn(usize, bool) -> f32, mut visit: impl FnMut(&mut Walk, usize)) {
        visit(self, start);
        let (mut down, mut up) = (Some(down), Some(up));
        while down.is_some() || up.is_some() {
            self.side(&mut down, false, &lower, &mut visit);
            self.side(&mut up, true, &lower, &mut visit);
        }
    }

    fn side(&mut self, side: &mut Option<impl Iterator<Item = usize>>, up: bool, lower: &impl Fn(usize, bool) -> f32, visit: &mut impl FnMut(&mut Walk, usize)) {
        let Some(i) = side.as_mut().and_then(Iterator::next) else {
            *side = None;
            return;
        };
        if self.skips(lower(i, up)) {
            *side = None;
        } else {
            visit(self, i);
        }
    }
}

/// Copy boxes in a tree (median splits, up to four copies a leaf), walked
/// nearest box first.
pub(super) struct CopyTree {
    nodes: Vec<CopyNode>,
    order: Vec<u32>,
}

/// A leaf holds `order[first..first + count]`; an inner node (count 0) has
/// its children right after it and at `second`.
struct CopyNode {
    lo: Vec3,
    hi: Vec3,
    first: u32,
    count: u32,
    second: u32,
}

impl CopyTree {
    fn new(boxes: &[(Vec3, Vec3)]) -> Self {
        let mut tree = Self { nodes: Vec::new(), order: (0..boxes.len() as u32).collect() };
        tree.build(boxes, 0, boxes.len());
        tree
    }

    fn build(&mut self, boxes: &[(Vec3, Vec3)], first: usize, end: usize) -> u32 {
        let ids = &mut self.order[first..end];
        let (lo, hi) = ids.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), &i| (lo.min(boxes[i as usize].0), hi.max(boxes[i as usize].1)));
        let node = self.nodes.len();
        self.nodes.push(CopyNode { lo, hi, first: first as u32, count: (end - first) as u32, second: 0 });
        if end - first > 4 {
            let center = |i: u32| boxes[i as usize].0 + boxes[i as usize].1;
            let (clo, chi) = ids.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), &i| (lo.min(center(i)), hi.max(center(i))));
            let axis = (chi - clo).max_position();
            let mid = (first + end) / 2;
            ids.select_nth_unstable_by(mid - first, |&a, &b| center(a)[axis].total_cmp(&center(b)[axis]));
            self.nodes[node].count = 0;
            self.build(boxes, first, mid);
            self.nodes[node].second = self.build(boxes, mid, end);
        }
        node as u32
    }

    fn walk(&self, walk: &mut Walk, mut visit: impl FnMut(&mut Walk, u32)) {
        let q = walk.q;
        let gap = |n: u32| {
            let n = &self.nodes[n as usize];
            (n.lo - q).max(q - n.hi).max(Vec3::ZERO).length()
        };
        // Balanced: at most 512 copies make 8 levels.
        let mut stack = [(0u32, 0.0f32); 32];
        stack[0] = (0, gap(0));
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let (n, g) = stack[top];
            if walk.skips(g) {
                continue;
            }
            let node = &self.nodes[n as usize];
            if node.count > 0 {
                for &i in &self.order[node.first as usize..(node.first + node.count) as usize] {
                    visit(walk, i);
                }
            } else {
                let (a, b) = ((n + 1, gap(n + 1)), (node.second, gap(node.second)));
                let (near, far) = if a.1 <= b.1 { (a, b) } else { (b, a) };
                stack[top] = far;
                stack[top + 1] = near;
                top += 2;
            }
        }
    }
}

pub(super) enum Geometry {
    Extrude {
        p: Vec<Vec2>,
        center: Vec3,
        inv: Quat,
        half: f32,
        bevel: f32,
        lo: Vec3,
        hi: Vec3,
    },
    Lathe {
        p: Vec<Vec2>,
        center: Vec3,
        axis: Vec3,
        lo: Vec3,
        hi: Vec3,
    },
    Sweep {
        p: Vec<Vec2>,
        points: Vec<Vec3>,
        frames: Vec<Quat>,
        taper: f32,
        twist: f32,
        radius: f32,
        lengths: Vec<f32>,
        smooth: bool,
    },
    Shell {
        source: Box<Geo>,
        lo: Vec3,
        hi: Vec3,
        thickness: f32,
    },
    Array {
        source: Box<Geo>,
        transforms: Vec<(Vec3, Quat)>,
        /// The source's box, when its distance is exact (so at least the
        /// distance to that box); None evaluates every copy.
        source_bounds: Option<(Vec3, Vec3)>,
        /// How copies near a point are found (with `source_bounds`).
        copies: Copies,
        /// The source holds a mesh, whose bounded lookup deep inside
        /// gives the bound and not its depth: copies are asked with the
        /// caller's bound, not the best so far, so the deepest one counts.
        mesh: bool,
        /// Radius of a ball about each copy's origin holding the copy.
        reach: f32,
        lo: Vec3,
        hi: Vec3,
    },
    Band {
        points: Vec<Vec3>,
        normal: Vec3,
        half: f32,
        depth: f32,
    },
}
impl Geometry {
    pub(super) fn new(form: &Form, assets: &super::Assets) -> Self {
        match form {
            Form::Extrude {
                profile,
                depth,
                center,
                axis,
                bevel,
            } => {
                let p = polygon(profile);
                let half = depth.abs().max(1e-4) * 0.5;
                let rot = frame(*axis);
                let (lo, hi) = p
                    .iter()
                    .flat_map(|p| [-half, half].map(|z| *center + rot * Vec3::new(p.x, p.y, z)))
                    .fold(
                        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                        |(lo, hi), q| (lo.min(q), hi.max(q)),
                    );
                Self::Extrude {
                    p,
                    center: *center,
                    inv: frame(*axis).inverse(),
                    half,
                    bevel: bevel.max(0.0).min(half),
                    lo,
                    hi,
                }
            }
            Form::Lathe {
                profile,
                center,
                axis,
            } => {
                let p = polygon(profile);
                let axis = axis.normalize_or(Vec3::Y);
                let radius = p.iter().map(|p| p.x).fold(0.0, f32::max);
                let lower = p.iter().map(|p| p.y).fold(f32::MAX, f32::min);
                let upper = p.iter().map(|p| p.y).fold(f32::MIN, f32::max);
                let radial = Vec3::from_array(
                    (Vec3::ONE - axis * axis)
                        .max(Vec3::ZERO)
                        .to_array()
                        .map(f32::sqrt),
                ) * radius;
                let (lo, hi) = (
                    *center + (axis * lower).min(axis * upper) - radial,
                    *center + (axis * lower).max(axis * upper) + radial,
                );
                Self::Lathe {
                    p,
                    center: *center,
                    axis,
                    lo,
                    hi,
                }
            }
            Form::Sweep {
                profile,
                points,
                taper,
                twist,
                smooth,
            } => {
                let p = polygon(profile);
                let taper = taper.clamp(0.01, 10.0);
                let radius = p.iter().map(|p| p.length()).fold(0.0, f32::max) * taper.max(1.0);
                let points = if *smooth {
                    spline_path(points)
                } else {
                    points.clone()
                };
                let mut lengths = vec![0.0];
                let mut frames = Vec::new();
                let mut prior = Quat::IDENTITY;
                let mut previous = Vec3::Z;
                for i in 0..points.len() {
                    let tangent = (points[(i + 1).min(points.len() - 1)]
                        - points[i.saturating_sub(1)])
                    .normalize_or(previous);
                    prior = Quat::from_rotation_arc(previous, tangent) * prior;
                    previous = tangent;
                    frames.push(prior);
                    if i > 0 {
                        lengths.push(lengths.last().unwrap() + points[i - 1].distance(points[i]));
                    }
                }
                Self::Sweep {
                    p,
                    points: points.clone(),
                    frames,
                    taper,
                    twist: twist.to_radians(),
                    radius,
                    lengths,
                    smooth: *smooth,
                }
            }
            Form::Shell { source, thickness } => {
                let (g, lo, hi) = Geo::new(source, assets);
                Self::Shell {
                    source: Box::new(g),
                    lo,
                    hi,
                    thickness: thickness.abs().max(1e-4),
                }
            }
            Form::Array {
                source,
                count,
                step,
                radius,
                center,
                axis,
                points,
                ..
            } => {
                let (g, a, b) = Geo::new(source, assets);
                let mut transforms = Vec::new();
                let mut boxes = Vec::new();
                let mut lo = Vec3::splat(f32::MAX);
                let mut hi = -lo;
                let axis = axis.normalize_or(Vec3::Y);
                let radial = axis.any_orthonormal_vector();
                for (translation, rot) in placements(form) {
                    let (mut copy_lo, mut copy_hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                    for c in box_corners(a, b) {
                        let c = translation + rot * c;
                        copy_lo = copy_lo.min(c);
                        copy_hi = copy_hi.max(c);
                    }
                    lo = lo.min(copy_lo);
                    hi = hi.max(copy_hi);
                    boxes.push((copy_lo, copy_hi));
                    transforms.push((translation, rot.inverse()));
                }
                let source_bounds = exact(source).then_some((a, b));
                let copies = if source_bounds.is_none() || *count < 2 {
                    Copies::All
                } else if points.len() >= 2 {
                    Copies::Tree(CopyTree::new(&boxes))
                } else if *radius > 0.0 {
                    Copies::Circle { center: *center, radial, side: axis.cross(radial) }
                } else if step.length_squared() > 1e-12 {
                    let unit = step.normalize();
                    let span = box_corners(a, b).iter().fold((f32::MAX, f32::MIN), |(l, h), c| (l.min(c.dot(unit)), h.max(c.dot(unit))));
                    Copies::Line { step: *step, unit, span }
                } else {
                    // Every copy in one place.
                    Copies::Tree(CopyTree::new(&boxes))
                };
                Self::Array {
                    source: Box::new(g),
                    transforms,
                    source_bounds,
                    copies,
                    mesh: holds_mesh(source),
                    reach: box_corners(a, b).iter().map(|c| c.length()).fold(0.0, f32::max),
                    lo,
                    hi,
                }
            }
            Form::Band {
                points,
                width,
                normal,
            } => Self::Band {
                points: points.clone(),
                normal: normal.normalize_or(Vec3::Z),
                half: width.abs().max(1e-4) * 0.5,
                depth: 0.01,
            },
            _ => unreachable!(),
        }
    }
    pub(super) fn project(&self, q: Vec3, lo: Vec3, hi: Vec3) -> Option<Vec3> {
        let Self::Extrude {
            p,
            center,
            inv,
            half,
            bevel,
            ..
        } = self
        else {
            return None;
        };
        if *bevel > 0.0 {
            return None;
        }
        let rot = inv.inverse();
        let world = |p: Vec2, z: f32| *center + rot * Vec3::new(p.x, p.y, z);
        let mut features = FeatureProject::new(q, lo, hi);
        for (i, &v) in p.iter().enumerate() {
            let prior = p[(i + p.len() - 1) % p.len()];
            let next = p[(i + 1) % p.len()];
            if (v - prior)
                .normalize_or_zero()
                .dot((next - v).normalize_or_zero())
                < 0.75
            {
                features.corner(world(v, -half));
                features.corner(world(v, *half));
                features.edge(world(v, -half), world(v, *half));
            }
            for z in [-half, *half] {
                features.edge(world(v, z), world(next, z));
            }
        }
        if let Some(p) = features.point() {
            return Some(p);
        }
        let local = *inv * (q - *center);
        let uv = local.truncate();
        let mut nearest = (f32::MAX, uv);
        for (i, &a) in p.iter().enumerate() {
            let v = p[(i + 1) % p.len()] - a;
            let p = a + v * ((uv - a).dot(v) / v.length_squared().max(1e-20)).clamp(0.0, 1.0);
            let d = p.distance_squared(uv);
            if d < nearest.0 {
                nearest = (d, p);
            }
        }
        let side = polygon_sdf(uv, p);
        let cap = local.z.abs() - half;
        let xy = if side > 0.0 || (cap <= 0.0 && side > cap) {
            nearest.1
        } else {
            uv
        };
        let z = if cap > 0.0 || (side <= 0.0 && cap >= side) {
            half.copysign(local.z)
        } else {
            local.z
        };
        Some(world(xy, z))
    }

    pub(super) fn band_depth(&mut self, wall: f32) {
        if let Self::Band { depth, .. } = self {
            *depth = wall * 1.5;
        }
    }
    pub(super) fn bounds(&self) -> (Vec3, Vec3) {
        match self {
            Self::Extrude { lo, hi, .. } | Self::Lathe { lo, hi, .. } => (*lo, *hi),
            Self::Shell { lo, hi, .. } | Self::Array { lo, hi, .. } => (*lo, *hi),
            Self::Sweep { points, radius, .. } => points.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), p| (a.min(*p - *radius), b.max(*p + *radius)),
            ),
            // Surface bands extend a bounded distance from their path, so
            // an engraving cannot accidentally cut the opposite body side.
            Self::Band {
                points,
                half,
                depth,
                ..
            } => points.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), p| (a.min(*p - half - depth), b.max(*p + half + depth)),
            ),
        }
    }
    pub(super) fn sdf(&self, q: Vec3) -> f32 {
        self.sdf_within(q, f32::MAX)
    }
    /// `sdf` where it is below `bound`; elsewhere some value from `bound`
    /// up to it (still a lower bound of the distance). Arrays use this to
    /// skip copies that cannot come within the bound; other forms ignore it.
    pub(super) fn sdf_within(&self, q: Vec3, bound: f32) -> f32 {
        match self {
            Self::Extrude {
                p,
                center,
                inv,
                half,
                bevel,
                ..
            } => {
                let q = *inv * (q - *center);
                intersection(
                    polygon_sdf(q.truncate(), p) + bevel,
                    q.z.abs() - half + bevel,
                ) - bevel
            }
            Self::Lathe {
                p, center, axis, ..
            } => {
                let q = q - *center;
                let height = q.dot(*axis);
                polygon_sdf_mode(Vec2::new((q - *axis * height).length(), height), p, true)
            }
            Self::Shell {
                source, thickness, ..
            } => (source.sdf(q) + thickness * 0.5).abs() - thickness * 0.5,
            Self::Array {
                source,
                transforms,
                source_bounds,
                copies,
                mesh,
                reach,
                lo,
                hi,
            } => {
                #[cfg(test)]
                if array_tests::BRUTE.get() {
                    return transforms.iter().map(|(t, r)| source.sdf(*r * (q - *t))).fold(f32::MAX, f32::min);
                }
                let Some((a, b)) = *source_bounds else {
                    return transforms.iter().fold(f32::MAX, |d, (t, r)| d.min(source.sdf_within(*r * (q - *t), d.min(bound))));
                };
                // Copies are visited nearest first. One is skipped when a
                // lower bound of its distance (its box, its ball, the slab
                // of a line array) exceeds the best so far (or `bound`): the
                // source is exact, so the min over copies is unchanged.
                // `floor` keeps the least bound skipped, which is returned
                // when nothing came within `bound`.
                let size = q.abs().max(lo.abs()).max(hi.abs()).max_element() + reach;
                let mut walk = Walk { q, bound, best: f32::MAX, floor: f32::MAX, slack: size * 1e-6 };
                let visit = |walk: &mut Walk, i: usize| {
                    let (t, r) = &transforms[i];
                    let local = *r * (q - *t);
                    let gap = (a - local).max(local - b).max(Vec3::ZERO).length();
                    if walk.skips(gap) {
                        return;
                    }
                    let d = source.sdf_within(local, if *mesh { bound } else { walk.best.min(bound) });
                    walk.best = walk.best.min(d);
                };
                // A ball about a copy's origin bounds it, rotated or not; the
                // bound grows monotonically with the index away from the
                // start, so a side stops at its first copy out of reach.
                let ball = |i: usize| (q - transforms[i].0).length() - reach;
                let n = transforms.len();
                match copies {
                    Copies::All => (0..n).for_each(|i| visit(&mut walk, i)),
                    Copies::Line { step, unit, span } => {
                        // Line copies are never rotated: each lies in a slab
                        // across the line, as well as in its ball.
                        // Below the start a copy's slab ends farther behind
                        // the point at each step; above it, starts farther ahead.
                        let slab = |i: usize, up: bool| {
                            let s = (q - transforms[i].0).dot(*unit);
                            ball(i).max(if up { span.0 - s } else { s - span.1 })
                        };
                        let start = (q.dot(*step) / step.length_squared()).round().clamp(0.0, (n - 1) as f32) as usize;
                        walk.sides(start, (0..start).rev(), start + 1..n, slab, visit);
                    }
                    Copies::Circle { center, radial, side } => {
                        // The distance to a copy's origin grows with its angle
                        // from the point's, up to half a turn either way.
                        let v = q - *center;
                        let angle = v.dot(*side).atan2(v.dot(*radial)).rem_euclid(std::f32::consts::TAU);
                        let start = (angle * n as f32 / std::f32::consts::TAU).round() as usize % n;
                        let up = n / 2;
                        walk.sides(start, (1..n - up).map(|k| (start + n - k) % n), (1..=up).map(|k| (start + k) % n), |i, _| ball(i), visit);
                    }
                    Copies::Tree(tree) => tree.walk(&mut walk, |walk, i| visit(walk, i as usize)),
                }
                walk.best.min(walk.floor)
            }
            Self::Band {
                points,
                normal,
                half,
                depth,
            } => points
                .windows(2)
                .map(|w| {
                    let v = w[1] - w[0];
                    let v = v - *normal * v.dot(*normal);
                    let d = q - w[0];
                    let d = d - *normal * d.dot(*normal);
                    let t = (d.dot(v) / v.length_squared().max(1e-12)).clamp(0.0, 1.0);
                    intersection(
                        (d - v * t).length() - half,
                        (q - w[0] - (w[1] - w[0]) * t).dot(*normal).abs() - depth,
                    )
                })
                .fold(f32::MAX, f32::min),
            Self::Sweep {
                p,
                points,
                frames,
                taper,
                twist,
                lengths,
                smooth,
                ..
            } => {
                if *smooth {
                    let mut nearest = (f32::MAX, 0, 0.0);
                    for (i, w) in points.windows(2).enumerate() {
                        let v = w[1] - w[0];
                        let t = ((q - w[0]).dot(v) / v.length_squared().max(1e-12)).clamp(0.0, 1.0);
                        let d = q.distance_squared(w[0] + v * t);
                        if d < nearest.0 {
                            nearest = (d, i, t);
                        }
                    }
                    let (_, mut i, mut t) = nearest;
                    let curve = |i: usize, t: f32| {
                        let a = points[i];
                        let b = points[i + 1];
                        let u = if i == 0 {
                            b - a
                        } else {
                            (b - points[i - 1]) * 0.5
                        };
                        let v = if i + 2 == points.len() {
                            b - a
                        } else {
                            (points[i + 2] - a) * 0.5
                        };
                        let center = a * (2.0 * t * t * t - 3.0 * t * t + 1.0)
                            + b * (-2.0 * t * t * t + 3.0 * t * t)
                            + u * (t * t * t - 2.0 * t * t + t)
                            + v * (t * t * t - t * t);
                        let tangent = a * (6.0 * t * t - 6.0 * t)
                            + b * (-6.0 * t * t + 6.0 * t)
                            + u * (3.0 * t * t - 4.0 * t + 1.0)
                            + v * (3.0 * t * t - 2.0 * t);
                        let second = a * (12.0 * t - 6.0)
                            + b * (-12.0 * t + 6.0)
                            + u * (6.0 * t - 4.0)
                            + v * (6.0 * t - 2.0);
                        (center, tangent, second)
                    };
                    // Solve the perpendicular section of the continuous curve,
                    // rather than switching between discrete segment slabs.
                    for _ in 0..8 {
                        let (center, tangent, second) = curve(i, t);
                        let d = q - center;
                        t += d.dot(tangent) / (tangent.length_squared() - d.dot(second)).max(1e-12);
                        if t > 1.0 && i + 2 < points.len() {
                            i += 1;
                            t -= 1.0;
                        } else if t < 0.0 && i > 0 {
                            i -= 1;
                            t += 1.0;
                        }
                        t = t.clamp(0.0, 1.0);
                    }
                    let (center, tangent, _) = curve(i, t);
                    let tangent = tangent.normalize_or(Vec3::Z);
                    let frame = frames[i].slerp(frames[i + 1], t);
                    let frame = Quat::from_rotation_arc(frame * Vec3::Z, tangent) * frame;
                    let local = frame.inverse() * (q - center);
                    let fraction = (lengths[i] + (lengths[i + 1] - lengths[i]) * t)
                        / lengths.last().unwrap().max(1e-9);
                    let scale = 1.0 + (taper - 1.0) * fraction;
                    let angle = -twist * fraction;
                    let uv = Vec2::new(
                        local.x * angle.cos() - local.y * angle.sin(),
                        local.x * angle.sin() + local.y * angle.cos(),
                    );
                    let cap = if i == 0 && t == 0.0 {
                        -local.z
                    } else if i + 2 == points.len() && t == 1.0 {
                        local.z
                    } else {
                        -f32::MAX
                    };
                    return intersection(polygon_sdf(uv / scale, p) * scale, cap);
                }
                // Overlapping capped sections keep the field continuous even
                // at section corners. Dense spline samples and interpolated
                // frames make internal joins sub-pixel at film resolution.
                let section_radius = p.iter().map(|v| v.length()).fold(0.0, f32::max);
                let mut distance = f32::MAX;
                for (i, w) in points.windows(2).enumerate() {
                    let v = w[1] - w[0];
                    let len = v.length();
                    let tangent = v / len.max(1e-9);
                    let axial = (q - w[0]).dot(tangent);
                    let t = axial.clamp(0.0, len);
                    let center = w[0] + tangent * t;
                    let fraction = (lengths[i] + t) / lengths.last().unwrap().max(1e-9);
                    let scale = 1.0 + (taper - 1.0) * fraction;
                    let bound = section_radius * scale;
                    if q.distance(center) - bound > distance.max(0.0) {
                        continue;
                    }
                    let local =
                        frames[i].slerp(frames[i + 1], t / len.max(1e-9)).inverse() * (q - center);
                    let angle = -twist * fraction;
                    let uv = Vec2::new(
                        local.x * angle.cos() - local.y * angle.sin(),
                        local.x * angle.sin() + local.y * angle.cos(),
                    );
                    let overlap = len * 0.5;
                    let cap = (-axial - if i > 0 { overlap } else { 0.0 })
                        .max(axial - len - if i + 2 < points.len() { overlap } else { 0.0 });
                    let d = intersection(polygon_sdf(uv / scale, p) * scale, cap);
                    distance = distance.min(d);
                }
                distance
            }
        }
    }
}

/// Smallest authored cross-section feature for the default regular grid.
/// Where an array form puts each copy of its source: (translation,
/// rotation), copy = translation + rotation * source. Empty for other
/// forms.
fn placements(form: &Form) -> Vec<(Vec3, Quat)> {
    let Form::Array { count, step, radius, center, axis, points, orient, .. } = form else { return Vec::new() };
    // Path orientation: each segment's turn from the first one,
    // carried along (parallel transport), so a loop turns all
    // the way around without flipping.
    let turns = if *orient == Orient::Path { path_turns(points) } else { vec![Quat::IDENTITY; points.len().saturating_sub(1)] };
    let axis = axis.normalize_or(Vec3::Y);
    let radial = axis.any_orthonormal_vector();
    let lengths: Vec<f32> = std::iter::once(0.0)
        .chain(points.windows(2).scan(0.0, |n, w| {
            *n += w[0].distance(w[1]);
            Some(*n)
        }))
        .collect();
    (0..*count)
        .map(|i| {
            let fraction = i as f32 / (*count - 1).max(1) as f32;
            if points.len() >= 2 {
                let distance = fraction * lengths.last().unwrap();
                let j = lengths
                    .partition_point(|v| *v < distance)
                    .saturating_sub(1)
                    .min(points.len() - 2);
                let t = (distance - lengths[j]) / (lengths[j + 1] - lengths[j]).max(1e-9);
                (points[j].lerp(points[j + 1], t), turns[j])
            } else if *radius > 0.0 {
                let rot = Quat::from_axis_angle(
                    axis,
                    std::f32::consts::TAU * i as f32 / *count as f32,
                );
                (*center + rot * radial * *radius, rot)
            } else {
                (*step * i as f32, Quat::IDENTITY)
            }
        })
        .collect()
}

/// An array form's copies as separate placements of one source form, for
/// drawing them as instances: the source and each copy's (translation,
/// rotation), as the array places them. `leaf` goes through nested
/// arrays to the innermost source, composing the placements. None for
/// other forms.
pub(crate) fn array_copies(form: &Form, leaf: bool) -> Option<(Form, Vec<(Vec3, Quat)>)> {
    let Form::Array { source, .. } = form else { return None };
    let outer = placements(form);
    if leaf && let Some((unit, inner)) = array_copies(source, true) {
        let copies = outer.iter().flat_map(|&(t, r)| inner.iter().map(move |&(ti, ri)| (t + r * ti, r * ri))).collect();
        return Some((unit, copies));
    }
    Some(((**source).clone(), outer))
}

/// A form's bounds (before any blob offsets).
pub(crate) fn form_bounds(form: &Form, assets: &super::Assets) -> (Vec3, Vec3) {
    let (_, lo, hi) = Geo::new(form, assets);
    (lo, hi)
}

pub(super) fn feature_size(form: &Form) -> Option<f32> {
    let section = |p: &[Corner]| {
        let lo = p
            .iter()
            .map(Corner::point)
            .fold(Vec2::splat(f32::MAX), Vec2::min);
        let hi = p
            .iter()
            .map(Corner::point)
            .fold(Vec2::splat(f32::MIN), Vec2::max);
        (hi - lo).min_element()
    };
    match form {
        Form::Extrude {
            profile,
            depth,
            bevel,
            ..
        } => Some(section(profile).min(*depth).min(if *bevel > 0.0 {
            *bevel * 2.0
        } else {
            f32::MAX
        })),
        Form::Sweep { profile, taper, .. } => Some(section(profile) * taper.min(1.0)),
        Form::Lathe { profile, .. } => Some(
            profile
                .iter()
                .enumerate()
                .map(|(i, p)| p.point().distance(profile[(i + 1) % profile.len()].point()))
                .fold(f32::MAX, f32::min),
        ),
        Form::Shell { thickness, .. } => Some(*thickness),
        Form::Array { source, .. } => feature_size(source),
        Form::Band { width, .. } => Some(*width),
        Form::Sphere { radius, .. } => Some(*radius * 2.0),
        Form::Box { size, round, .. } => {
            Some(
                size.abs()
                    .min_element()
                    .min(if *round > 0.0 { *round * 2.0 } else { f32::MAX }),
            )
        }
        _ => None,
    }
}

pub(super) fn quad_diagonal(q: [u32; 4], direction: Vec3, positions: &[Vec3]) -> Option<bool> {
    let [a, b, c, d] = q.map(|i| positions[i as usize]);
    let oriented = |a: Vec3, b: Vec3, c: Vec3| (b - a).cross(c - a).dot(direction) > 0.0;
    if oriented(a, b, c) && oriented(a, c, d) {
        Some(false)
    } else if oriented(a, b, d) && oriented(b, c, d) {
        Some(true)
    } else {
        None
    }
}

pub(super) fn corner_normals(f: &[f32; 8]) -> [(Vec3, Vec3); 8] {
    std::array::from_fn(|i| {
        (
            Vec3::ZERO,
            Vec3::new(
                f[(i & !1) | 1] - f[i & !1],
                f[(i & !2) | 2] - f[i & !2],
                f[(i & !4) | 4] - f[i & !4],
            )
            .normalize_or_zero(),
        )
    })
}
#[cfg(test)]
pub(super) fn cell_normal(f: &[f32; 8], p: Vec3) -> Vec3 {
    let mut gradient = Vec3::ZERO;
    for (i, &(x, y, z)) in super::CORNERS.iter().enumerate() {
        let w = Vec3::new(
            if x == 0 { 1.0 - p.x } else { p.x },
            if y == 0 { 1.0 - p.y } else { p.y },
            if z == 0 { 1.0 - p.z } else { p.z },
        );
        gradient += Vec3::new(
            if x == 0 { -1.0 } else { 1.0 } * w.y * w.z,
            if y == 0 { -1.0 } else { 1.0 } * w.x * w.z,
            if z == 0 { -1.0 } else { 1.0 } * w.x * w.y,
        ) * f[i];
    }
    gradient.normalize_or_zero()
}
pub(super) fn bent(hits: &[(Vec3, Vec3)], cosine: f32) -> bool {
    hits.iter().any(|(_, a)| {
        hits.iter().any(|(_, b)| {
            a.length_squared() > 0.5 && b.length_squared() > 0.5 && a.dot(*b) < cosine
        })
    })
}

/// Minimize plane error + a small pull toward the mass point. Enumerating
/// each coordinate as free/lower/upper solves the box-constrained QEF rather
/// than clamping an unconstrained solution (which loses corner accuracy).
pub(super) fn feature_vertex(hits: &[(Vec3, Vec3)], mean: Vec3) -> Vec3 {
    if !bent(hits, 0.75) {
        return mean;
    }
    let regularization = 1e-5;
    let mut a = Mat3::IDENTITY * regularization;
    let mut b = mean * regularization;
    for &(p, n) in hits {
        a += Mat3::from_cols(n * n.x, n * n.y, n * n.z);
        b += n * n.dot(p);
    }
    let mut best = mean;
    let mut error = f32::MAX;
    // Stay slightly inside the cell: adjacent cells never collapse to one
    // vertex when an exact feature lies on a grid plane.
    for mask in 0..27 {
        let mut mode = mask;
        let mut rows = [[0.0f32; 4]; 3];
        let mut fixed = [None; 3];
        for f in &mut fixed {
            let digit = mode % 3;
            mode /= 3;
            *f = match digit {
                1 => Some(1e-3),
                2 => Some(1.0 - 1e-3),
                _ => None,
            };
        }
        for (i, row) in rows.iter_mut().enumerate() {
            for (j, x) in row[..3].iter_mut().enumerate() {
                *x = a.col(j)[i];
            }
            row[3] = b[i];
        }
        for j in 0..3 {
            if let Some(v) = fixed[j] {
                for (i, row) in rows.iter_mut().enumerate() {
                    if i != j {
                        row[3] -= row[j] * v;
                        row[j] = 0.0;
                    }
                }
                rows[j] = [0.0; 4];
                rows[j][j] = 1.0;
                rows[j][3] = v;
            }
        }
        for j in 0..3 {
            let pivot = (j..3)
                .max_by(|&i, &k| rows[i][j].abs().total_cmp(&rows[k][j].abs()))
                .unwrap();
            rows.swap(j, pivot);
            let d = rows[j][j];
            if d.abs() < 1e-12 {
                continue;
            }
            for x in &mut rows[j][j..4] {
                *x /= d;
            }
            let pivot_row = rows[j];
            for (i, row) in rows.iter_mut().enumerate() {
                if i != j {
                    let d = row[j];
                    for (x, p) in row[j..4].iter_mut().zip(&pivot_row[j..4]) {
                        *x -= d * p;
                    }
                }
            }
        }
        let x = Vec3::new(rows[0][3], rows[1][3], rows[2][3]);
        if !x.is_finite() || x.min_element() < 1e-4 - 1e-6 || x.max_element() > 1.0 - 1e-4 + 1e-6 {
            continue;
        }
        let e = hits.iter().map(|&(p, n)| n.dot(x - p).powi(2)).sum::<f32>()
            + regularization * x.distance_squared(mean);
        if e < error {
            error = e;
            best = x;
        }
    }
    best
}

pub(super) fn validate(form: &Form) -> Result<(), String> {
    fn check(form: &Form, depth: usize) -> Result<usize, String> {
        if depth > 8 {
            return Err("form nesting is limited to 8".into());
        }
        let positive = |x: f32| x.is_finite() && x > 0.0;
        let axis_ok = |v: Vec3| v.is_finite() && v.length_squared() > 1e-12;
        match form {
            Form::Extrude {depth,center,axis,bevel,..} if !positive(*depth) || !center.is_finite() || !axis_ok(*axis) || !bevel.is_finite() || *bevel<0.0 => Err("extrude needs positive depth, finite center, nonzero axis and nonnegative bevel".into()),
            Form::Lathe {profile,center,axis} if !center.is_finite() || !axis_ok(*axis) || profile.iter().any(|p|p.point().x<0.0) => Err("lathe needs nonnegative profile radius, finite center and nonzero axis".into()),
            Form::Sweep {points,taper,twist,..} if !positive(*taper) || *taper>10.0 || !twist.is_finite() || points.windows(2).any(|w|w[0].distance_squared(w[1])<1e-12) => Err("sweep needs distinct path points, taper in (0,10], finite twist".into()),
            Form::Shell {source,thickness} => {if !positive(*thickness) {return Err("shell thickness must be positive".into());}check(source,depth+1)},
            Form::Array {source,count,points,step,radius,center,axis,..} => {
                if !(1..=512).contains(count) || points.len()>128 || points.iter().any(|p|!p.is_finite()) || points.len()==1 || !step.is_finite() || !radius.is_finite() || *radius<0.0 || !center.is_finite() || !axis_ok(*axis) {return Err("array needs count 1-512, finite transforms, nonzero axis and 0 or 2-128 path points".into());}
                let n=check(source,depth+1)?.saturating_mul(*count);
                if n>4096 {Err("nested arrays may contain at most 4096 copies".into())}else{Ok(n)}
            },
            Form::Band {width,normal,..} if !positive(*width) || !axis_ok(*normal) => Err("band needs positive width and nonzero normal".into()),
            // Top level or copied by an array/shell alike.
            Form::Mesh {asset,center,rotate,scale,..} if asset.is_empty() || asset.len()>64 || !asset.bytes().all(|c|c.is_ascii_hexdigit()) || !center.is_finite() || !rotate.is_finite() || !scale.is_finite() || !(1e-6..=1e6).contains(&scale.abs()) || center.abs().max_element()>crate::mesh_asset::MAX_COORD => Err("a mesh blob needs an asset hash, a finite center and rotate, and a scale of 1e-6..1e6".into()),
            _=>Ok(1),
        }
    }
    check(form, 0).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clay::{Clay, Field},
        mesh::Mesh,
    };
    use serde_json::{Value, json};
    use std::collections::HashMap;
    fn clay(blobs: Value) -> Clay {
        serde_json::from_value(json!({"blobs":blobs,"resolution":64})).unwrap()
    }
    fn value(c: &Clay, p: Vec3) -> f32 {
        let f = Field::new(c, 0.001);
        let list = f.near(p - 1.0, p + 1.0);
        f.sample(p, &list).0
    }
    fn closed(mesh: &Mesh) {
        assert!(!mesh.indices.is_empty());
        // Crease normals use duplicate render vertices. Weld exact coincident
        // positions before checking the geometric topology.
        let mut positions = HashMap::new();
        let remap: Vec<u32> = mesh
            .vertices
            .iter()
            .map(|v| {
                let next = positions.len() as u32;
                *positions.entry(v.pos.map(f32::to_bits)).or_insert(next)
            })
            .collect();
        let mut edges = HashMap::new();
        for tri in mesh.indices.chunks_exact(3) {
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                let (a, b) = (remap[a as usize], remap[b as usize]);
                let entry = edges.entry((a.min(b), a.max(b))).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += if a < b { 1 } else { -1 };
            }
            let (a, b, c) = (
                Vec3::from(mesh.vertices[tri[0] as usize].pos),
                Vec3::from(mesh.vertices[tri[1] as usize].pos),
                Vec3::from(mesh.vertices[tri[2] as usize].pos),
            );
            assert!(
                (b - a).cross(c - a).length_squared() > 0.0,
                "degenerate triangle"
            );
        }
        assert!(
            edges
                .values()
                .all(|&(count, winding)| count == 2 && winding == 0),
            "open or nonmanifold edge"
        );
    }
    fn components(mesh: &Mesh) -> usize {
        let mut neighbors = vec![Vec::new(); mesh.vertices.len()];
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                neighbors[a as usize].push(b as usize);
                neighbors[b as usize].push(a as usize);
            }
        }
        let mut visited = vec![false; neighbors.len()];
        let mut count = 0;
        for i in 0..neighbors.len() {
            if visited[i] || neighbors[i].is_empty() {
                continue;
            }
            count += 1;
            let mut stack = vec![i];
            visited[i] = true;
            while let Some(j) = stack.pop() {
                for &k in &neighbors[j] {
                    if !visited[k] {
                        visited[k] = true;
                        stack.push(k);
                    }
                }
            }
        }
        count
    }
    #[test]
    fn trilinear_normals_match_planes() {
        let f = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        assert!(cell_normal(&f, Vec3::splat(0.3)).dot(Vec3::X) > 0.999);
        let f = [-0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        let a = cell_normal(&f, Vec3::new(0.5, 0.0, 0.0));
        let b = cell_normal(&f, Vec3::new(0.0, 0.5, 0.0));
        assert!(a.dot(b) < 0.98);
    }
    #[test]
    fn box_qef_preserves_corners_and_closed_winding() {
        let c =
            clay(json!([{"type":"box","center":[0.013,0.017,0.011],"size":[1,0.8,0.6],"blend":0}]));
        let mesh = c.build_mesh();
        closed(&mesh);
        let corner = Vec3::new(0.513, 0.417, 0.311);
        let error = mesh
            .vertices
            .iter()
            .map(|v| Vec3::from(v.pos).distance(corner))
            .fold(f32::MAX, f32::min);
        assert!(
            error < c.cell_size().unwrap() * 0.02,
            "corner error {error}"
        );
        let mut normals = Vec::new();
        let mut face_edges: HashMap<([u32; 3], [u32; 3]), Vec<Vec3>> = HashMap::new();
        for tri in mesh.indices.chunks_exact(3) {
            let a = Vec3::from(mesh.vertices[tri[0] as usize].pos);
            let b = Vec3::from(mesh.vertices[tri[1] as usize].pos);
            let d = Vec3::from(mesh.vertices[tri[2] as usize].pos);
            let normal = (b - a).cross(d - a).normalize();
            assert!(
                normal.dot((a + b + d) / 3.0 - Vec3::new(0.013, 0.017, 0.011)) > 0.0,
                "flipped box face"
            );
            normals.push(normal);
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                face_edges
                    .entry({
                        let a = mesh.vertices[a as usize].pos.map(f32::to_bits);
                        let b = mesh.vertices[b as usize].pos.map(f32::to_bits);
                        (a.min(b), a.max(b))
                    })
                    .or_default()
                    .push(normal);
            }
        }
        assert!(normals.iter().any(|n| n.dot(Vec3::X) > 0.999));
        assert!(normals.iter().any(|n| n.dot(Vec3::Y) > 0.999));
        assert!(
            face_edges
                .values()
                .any(|n| n.len() == 2 && n[0].dot(n[1]).abs() < 0.02),
            "no near-90-degree dihedral"
        );
    }
    #[test]
    fn diamond_blade_keeps_thin_edge() {
        let mut c = clay(
            json!([{"type":"extrude","profile":[[-0.15,0],[0,0.03],[0.15,0],[0,-0.03]],"depth":1.2,"axis":[0,1,0],"blend":0}]),
        );
        c.resolution = 192;
        let mesh = c.build_mesh();
        closed(&mesh);
        let edge = Vec3::X * 0.15;
        let near: Vec<_> = mesh
            .vertices
            .iter()
            .map(|v| Vec3::from(v.pos))
            .filter(|p| p.y.abs() < 0.4 && p.x > 0.149)
            .collect();
        assert!(!near.is_empty(), "blade edge lost");
        assert!(near.iter().all(|p| p.z.abs() < 0.001), "blade edge rounded");
        assert!(mesh.vertices.iter().any(|v| {
            let p = Vec3::from(v.pos);
            p.x > edge.x - 0.001 && p.y.abs() < 0.4
        }));
    }
    #[test]
    fn hard_forms_shells_arrays_and_joins() {
        let ex =
            json!({"type":"extrude","profile":[[-1,-1],[1,-1],[1,1],[-1,1]],"depth":0.4,"blend":0});
        let c = clay(json!([ex]));
        assert!(value(&c, Vec3::ZERO) < 0.0);
        assert!(value(&c, Vec3::Z * 0.3) > 0.0);
        let lathe =
            clay(json!([{"type":"lathe","profile":[[0.8,-1],[1,-1],[1,1],[0.8,1]],"blend":0}]));
        assert!(value(&lathe, Vec3::X * 0.9) < 0.0);
        assert!(value(&lathe, Vec3::ZERO) > 0.0);
        let sweep = clay(
            json!([{"type":"sweep","profile":[[-0.1,-0.1],[0.1,-0.1],[0.1,0.1],[-0.1,0.1]],"points":[[0,0,0],[0,0,1],[0.3,0,2]],"taper":0.5,"twist":90,"blend":0}]),
        );
        assert!(value(&sweep, Vec3::Z * 0.5) < 0.0);
        assert!(value(&sweep, Vec3::new(1.0, 0.0, 0.5)) > 0.0);
        let shell = clay(
            json!([{"type":"shell","source":{"type":"sphere","center":[0,0,0],"radius":1},"thickness":0.1,"blend":0}]),
        );
        assert!(value(&shell, Vec3::X * 0.95) < 0.0);
        assert!(value(&shell, Vec3::X * 0.89) > 0.0);
        assert!(value(&shell, Vec3::X * 1.01) > 0.0);
        closed(&shell.build_mesh());
        let array = clay(
            json!([{"type":"array","source":{"type":"sphere","center":[0,0,0],"radius":0.1},"count":5,"step":[0.5,0,0],"blend":0}]),
        );
        for i in 0..5 {
            assert!(value(&array, Vec3::X * (0.5 * i as f32)) < 0.0);
        }
        assert!(value(&array, Vec3::X * 2.5) > 0.0);
        let mesh = array.build_mesh();
        closed(&mesh);
        assert_eq!(components(&mesh), 5);
        let mut all = c.clone();
        all.shell = Some(0.05);
        assert!(value(&all, Vec3::ZERO) > 0.0);
        assert!(value(&all, Vec3::Z * 0.18) < 0.0);
        assert!(value(&all, Vec3::Z * 0.14) > 0.0);
        let mut joined = clay(
            json!([{"type":"box","center":[-0.5,0,0],"size":[1,2,2],"blend":0},{"type":"box","center":[0,-0.5,0],"size":[2,1,2],"blend":0.2,"join":"sharp"}]),
        );
        assert!(value(&joined, Vec3::new(0.04, 0.04, 0.0)) > 0.0);
        joined.blobs[1].join = Join::Chamfer;
        assert!(value(&joined, Vec3::new(0.04, 0.04, 0.0)) < 0.0);
    }
    #[test]
    fn fitted_plate_and_surface_bands_have_constant_depth() {
        let mut c = clay(
            json!([{"type":"sphere","center":[0,0,0],"radius":1,"blend":0},{"type":"box","center":[0,0,1],"size":[1,1,1],"op":"plate","offset":0.02,"wall":0.04,"blend":0}]),
        );
        assert!(value(&c, Vec3::Z * 1.04) < 0.0);
        assert!(value(&c, Vec3::Z * 1.01) > 0.0);
        assert!(value(&c, Vec3::Z * 1.07) > 0.0);
        // A split armor part still reads its complete body source.
        c.resolution = 192;
        let mesh = c.build_part(Some(vec![false, true]), &[], "metal");
        closed(&mesh);
        assert!(!mesh.vertices.is_empty());
        c = clay(
            json!([{"type":"box","center":[0,0,0],"size":[1,1,1],"blend":0},{"type":"band","points":[[-0.3,0,0.5],[0.3,0,0.5]],"normal":[0,0,1],"width":0.1,"op":"engrave","wall":0.04,"blend":0}]),
        );
        assert!(value(&c, Vec3::new(0.0, 0.0, 0.48)) > 0.0);
        assert!(value(&c, Vec3::new(0.0, 0.0, 0.45)) < 0.0);
        assert!(value(&c, Vec3::new(0.0, 0.1, 0.48)) < 0.0);
        c.blobs[1].op = super::super::Op::Emboss;
        assert!(value(&c, Vec3::new(0.0, 0.0, 0.52)) < 0.0);
        assert!(value(&c, Vec3::new(0.0, 0.0, 0.55)) > 0.0);
    }
    #[test]
    fn plates_follow_explicit_bones_rigidly() {
        use crate::rig::{Bone, Rig};
        let mut c = clay(json!([
            {"type":"sphere","center":[0,0,0],"radius":0.5,"blend":0},
            {"type":"box","center":[0,0,0.5],"size":[0.7,0.7,0.4],"op":"plate","offset":0.01,"wall":0.04,"bone":"armor","blend":0}
        ]));
        c.resolution = 96;
        let bones = vec![
            Bone {
                name: "body".into(),
                parent: None,
                head: Vec3::ZERO,
                tail: Vec3::Y * 0.3,
                deform: true,
            },
            Bone {
                name: "armor".into(),
                parent: Some("body".into()),
                head: Vec3::ZERO,
                tail: Vec3::Z * 0.5,
                deform: true,
            },
        ];
        let rig = Rig {
            bones,
            ..Default::default()
        };
        let mesh = c.build_mesh();
        let skin = crate::rig::skin(&c, &mesh, &rig);
        let field = Field::new(&c, c.cell_size().unwrap());
        let mut count = 0;
        for (i, v) in mesh.vertices.iter().enumerate() {
            if field.plate_bone(Vec3::from(v.pos)).is_some() {
                assert_eq!(skin.joints[i][0], 1);
                assert_eq!(skin.weights[i][0], 1.0);
                count += 1;
            }
        }
        assert!(count > 100);
    }
    #[test]
    fn lathe_sweep_and_array_modes_are_closed_and_located() {
        let lathe = clay(
            json!([{"type":"lathe","profile":[[0.3,-0.3],[0.5,-0.3],[0.5,0.3],[0.3,0.3]],"blend":0}]),
        );
        closed(&lathe.build_mesh());
        let sweep = clay(
            json!([{"type":"sweep","profile":[[-0.1,-0.1],[0.1,-0.1],[0.1,0.1],[-0.1,0.1]],"points":[[0,0,-0.5],[0,0,0],[0.15,0,0.5]],"taper":0.7,"twist":30,"blend":0}]),
        );
        closed(&sweep.build_mesh());
        let source = json!({"type":"sphere","center":[0,0,0],"radius":0.07});
        let circle =
            clay(json!([{"type":"array","source":source,"count":8,"radius":0.5,"blend":0}]));
        let mesh = circle.build_mesh();
        closed(&mesh);
        assert_eq!(components(&mesh), 8);
        let normal = Vec3::Y;
        let radial = normal.any_orthonormal_vector();
        for i in 0..8 {
            assert!(
                value(
                    &circle,
                    Quat::from_axis_angle(normal, std::f32::consts::TAU * i as f32 / 8.0)
                        * radial
                        * 0.5
                ) < 0.0
            );
        }
        let path = clay(
            json!([{"type":"array","source":source,"count":3,"points":[[0,0,0],[1,0,0],[1,1,0]],"blend":0}]),
        );
        for p in [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0)] {
            assert!(value(&path, p) < 0.0);
        }
        let rounded = clay(
            json!([{"type":"extrude","profile":[{"point":[-0.5,-0.5],"radius":0.1},[0.5,-0.5],[0.5,0.5],[-0.5,0.5]],"depth":0.2,"bevel":0.02,"blend":0}]),
        );
        assert!(value(&rounded, Vec3::new(-0.5, -0.5, 0.0)) > 0.0);
        closed(&rounded.build_mesh());
        let mut cut = clay(
            json!([{"type":"box","center":[0,0,0],"size":[1,1,1],"blend":0},{"type":"box","center":[0.5,0.5,0],"size":[0.8,0.8,2],"op":"carve","join":"chamfer","join_size":0.04,"blend":0}]),
        );
        closed(&cut.build_mesh());
        cut.blobs[1].join = Join::Round;
        closed(&cut.build_mesh());
    }
    #[test]
    fn solid_lathe_axes_are_inside_the_volume() {
        let c = clay(json!([{"type":"lathe","profile":[[0,-1],[1,-1],[1,1],[0,1]],"blend":0}]));
        assert!(value(&c, Vec3::ZERO) < -0.001);
        closed(&c.build_mesh());
    }
    #[test]
    fn rotated_box_creases_stay_on_the_forged_edges() {
        let c = clay(
            json!([{"type":"box","center":[0.013,0.017,0.011],"size":[1,0.8,0.6],"rotate":[0,17,23],"blend":0}]),
        );
        let mesh = c.build_mesh();
        closed(&mesh);
        let inv = crate::clay::rotation(Vec3::new(0.0, 17.0, 23.0)).inverse();
        let mut groups: HashMap<[u32; 3], Vec<Vec3>> = HashMap::new();
        for v in &mesh.vertices {
            groups
                .entry(v.pos.map(f32::to_bits))
                .or_default()
                .push(Vec3::from(v.normal));
        }
        let mut count = 0;
        for (p, ns) in groups {
            if !ns.iter().any(|a| ns.iter().any(|b| a.dot(*b) < 0.1)) {
                continue;
            }
            let q =
                inv * (Vec3::from_array(p.map(f32::from_bits)) - Vec3::new(0.013, 0.017, 0.011));
            let mut errors = (q.abs() - Vec3::new(0.5, 0.4, 0.3)).abs().to_array();
            errors.sort_by(f32::total_cmp);
            assert!(
                errors[1] < c.cell_size().unwrap() * 0.05,
                "crease moved off its edge: {errors:?}"
            );
            count += 1;
        }
        assert!(count > 100);
    }
    #[test]
    fn fine_detail_windows_are_closed_and_keep_the_surface() {
        let mut c = clay(json!([{"type":"box","center":[0,0,0],"size":[1,1,1],"blend":0}]));
        c.detail_cell = Some(0.01);
        let mesh = c
            .build_fine_region(Vec3::splat(0.35), Vec3::splat(0.65), &[], "metal")
            .unwrap();
        closed(&mesh);
        assert!(mesh.bounds().0.min_element() > 0.34 && mesh.bounds().1.max_element() < 0.51);
    }
    #[test]
    fn default_grid_targets_small_features_and_honors_other_resolutions() {
        let mut c =
            clay(json!([{"type":"box","center":[0,0,0],"size":[1,1,1],"round":0.001,"blend":0}]));
        c.resolution = 96;
        assert_eq!(c.mesh_resolution(), 512);
        c.resolution = 64;
        assert_eq!(c.mesh_resolution(), 64);
    }
    #[test]
    fn tightly_twisted_polygon_sweep_has_manifold_connectivity() {
        let mut c = clay(
            json!([{"type":"sweep","profile":[[-0.1,-0.1],[0.1,-0.1],[0.1,0.1],[-0.1,0.1]],"points":[[0,0,-0.5],[0,0,0],[0.15,0,0.5]],"taper":0.7,"twist":30,"blend":0}]),
        );
        c.resolution = 192;
        closed(&c.build_mesh());
    }
    #[test]
    fn crease_shading_is_split_without_moving_geometry() {
        let c =
            clay(json!([{"type":"box","center":[0.013,0.017,0.011],"size":[1,0.8,0.6],"blend":0}]));
        let mesh = c.build_mesh();
        closed(&mesh);
        let mut at: HashMap<[u32; 3], Vec<Vec3>> = HashMap::new();
        for v in &mesh.vertices {
            at.entry(v.pos.map(f32::to_bits))
                .or_default()
                .push(Vec3::from(v.normal));
        }
        assert!(
            at.values()
                .any(|ns| ns.len() >= 3
                    && ns.iter().any(|a| ns.iter().any(|b| a.dot(*b).abs() < 0.02))),
            "box corner still shares shading normals"
        );
    }
    #[test]
    fn spline_sweeps_and_mixed_lathe_profiles_are_smooth() {
        let points = [
            Vec3::ZERO,
            Vec3::new(0.1, 0.2, 0.0),
            Vec3::new(0.0, 0.4, 0.0),
            Vec3::new(-0.2, 0.5, 0.0),
        ];
        let path = spline_path(&points);
        assert_eq!(path[0], points[0]);
        assert_eq!(*path.last().unwrap(), points[3]);
        for w in path.windows(3) {
            assert!(
                (w[1] - w[0]).normalize().dot((w[2] - w[1]).normalize()) > 0.98,
                "spline has a tangent discontinuity"
            );
        }
        let form: Form=serde_json::from_value(json!({"type":"lathe","profile":[[0,-0.2],[0.2,-0.2],{"point":[0.3,0],"smooth":true},{"point":[0.2,0.2],"smooth":true},[0,0.3]]})).unwrap();
        validate(&form).unwrap();
        let Form::Lathe { profile, .. } = &form else {
            unreachable!()
        };
        let p = polygon(profile);
        assert!(p.len() > 40);
        assert!(p.contains(&Vec2::new(0.2, -0.2)));
        let c = clay(json!([form]));
        closed(&c.build_mesh());
    }
    #[test]
    fn forged_fit_bridges_body_valleys_and_keeps_thickness() {
        let mut c = clay(json!([
            {"type":"ellipsoid","center":[0,0,0],"radii":[0.3,0.4,0.15],"blend":0},
            {"type":"sphere","center":[-0.10,0.08,0.12],"radius":0.09,"blend":0.02},
            {"type":"sphere","center":[0.10,0.08,0.12],"radius":0.09,"blend":0.02},
            {"type":"box","center":[0,0,0.18],"size":[0.5,0.6,0.4],"op":"plate","smooth":0.16,"wall":0.01,"offset":0.015,"ridge":0.008,"flare":0.006,"blend":0}
        ]));
        let field = Field::new(&c, 0.001);
        let e = field.prims.last().unwrap().envelope.as_ref().unwrap();
        let root = |x: f32| {
            let (mut lo, mut hi) = (0.0, 0.4);
            for _ in 0..24 {
                let z = (lo + hi) * 0.5;
                if e.sample(Vec3::new(x, 0.08, z)) < 0.0 {
                    lo = z;
                } else {
                    hi = z;
                }
            }
            (lo + hi) * 0.5
        };
        for x in [-0.10, 0.10] {
            assert!(
                e.sample(Vec3::new(x, 0.08, 0.21)) < 0.0,
                "body protrudes through the smooth fit"
            );
        }
        let center = root(0.0);
        let side = root(0.10);
        assert!(
            center > side,
            "forging retains the valley between anatomical bulges: center {center}, side {side}"
        );
        c.resolution = 192;
        closed(&c.build_part(Some(vec![false, false, false, true]), &[], "metal"));
    }
    #[test]
    fn malformed_forms_are_rejected_and_qef_is_bounded() {
        let bad: Result<super::super::Blob, _> = serde_json::from_value(
            json!({"type":"extrude","profile":[[0,0],[1,1],[0,1],[1,0]],"depth":1}),
        );
        assert!(bad.is_err());
        let source = json!({"type":"sphere","center":[0,0,0],"radius":0.1});
        let b:super::super::Blob=serde_json::from_value(json!({"type":"array","source":{"type":"array","source":source,"count":512},"count":512})).unwrap();
        assert!(b.validate().is_err());
        let hits = [
            (Vec3::new(2.0, 0.0, 0.0), Vec3::X),
            (Vec3::new(0.0, 2.0, 0.0), Vec3::Y),
            (Vec3::new(0.0, 0.0, 2.0), Vec3::Z),
        ];
        let v = feature_vertex(&hits, Vec3::splat(0.5));
        assert!(v.min_element() >= 0.0 && v.max_element() <= 1.0);
    }
}

/// Array evaluation against the plain loop over every copy, and pins for
/// meshes of nested arrays (a small tower: bands, posts around each floor,
/// a ring of columns).
#[cfg(test)]
pub(crate) mod array_tests {
    use super::*;
    use crate::clay::Clay;
    use crate::mesh::Mesh;
    use serde_json::{Value, json};

    thread_local! {
        /// Evaluate arrays copy by copy (this thread only; with one mesher
        /// thread, see `clay::with_threads`).
        pub(super) static BRUTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The reference: every copy at every level, no culling.
    fn brute(g: &Geo, q: Vec3) -> f32 {
        match g {
            Geo::Hard(Geometry::Array { source, transforms, .. }) => transforms.iter().map(|(t, r)| brute(source, *r * (q - *t))).fold(f32::MAX, f32::min),
            g => g.sdf(q),
        }
    }

    fn form(v: Value) -> Form {
        serde_json::from_value(v).unwrap()
    }

    /// Seeded points: some anywhere in (padded) bounds, some near copies.
    fn points(g: &Geo, lo: Vec3, hi: Vec3, n: usize, seed: u64) -> Vec<Vec3> {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        let pad = (hi - lo).max_element() * 0.25 + 0.2;
        let (lo, hi) = (lo - pad, hi + pad);
        let origins: Vec<Vec3> = match g {
            Geo::Hard(Geometry::Array { transforms, .. }) => transforms.iter().map(|t| t.0).collect(),
            _ => vec![(lo + hi) * 0.5],
        };
        (0..n)
            .map(|i| {
                let r = Vec3::new(next(), next(), next());
                if i % 5 < 3 {
                    lo + (hi - lo) * r
                } else {
                    let o = origins[(next() * origins.len() as f32) as usize % origins.len()];
                    o + (r - 0.5) * pad
                }
            })
            .collect()
    }

    /// Line, overlapping, circle (a ring much smaller than the query
    /// distance, queries inside the ring, rotated copies), a zig-zag path
    /// that comes back near itself, nested arrays, and an ellipsoid leaf.
    fn cases() -> Vec<(&'static str, Form)> {
        let post = json!({"type":"box","center":[0,0.1,0],"size":[0.04,0.2,0.04]});
        let square = json!([[-0.6,0.4,-0.6],[0.6,0.4,-0.6],[0.6,0.4,0.6],[-0.6,0.4,0.6],[-0.6,0.4,-0.6]]);
        let posts = json!({"type":"array","source":post,"count":24,"points":square,"orient":"path"});
        vec![
            ("line", form(json!({"type":"array","source":{"type":"box","center":[0,0,0],"size":[0.3,0.2,0.2]},"count":20,"step":[0.5,0,0]}))),
            ("overlapping line", form(json!({"type":"array","source":{"type":"extrude","profile":[[-0.4,-0.1],[0.4,-0.1],[0.4,0.1],[-0.4,0.1]],"depth":0.3,"axis":[0.2,0.1,1]},"count":30,"step":[0.1,0.05,0.02]}))),
            ("small ring", form(json!({"type":"array","source":{"type":"box","center":[0.02,0,0],"size":[0.3,0.1,0.05],"rotate":[0,20,0]},"count":24,"radius":0.05}))),
            ("rotated ring", form(json!({"type":"array","source":{"type":"box","center":[0.3,0.1,0],"size":[0.4,0.1,0.2],"rotate":[0,30,10]},"count":16,"radius":1.0,"axis":[0.3,1,0.2],"center":[0.1,0.2,0]}))),
            ("lathe ring", form(json!({"type":"array","source":{"type":"lathe","profile":[[0.05,-0.1],[0.15,-0.1],[0.15,0.1],[0.05,0.1]],"center":[0.2,0,0]},"count":7,"radius":0.4,"axis":[1,0,0]}))),
            ("zig-zag path", form(json!({"type":"array","source":{"type":"box","center":[0,0,0],"size":[0.1,0.05,0.3],"rotate":[0,0,25]},"count":60,"points":[[0,0,0],[2,0,0],[0,0.2,0],[2,0.4,0],[0,0.6,0]],"orient":"path"}))),
            ("fixed path", form(json!({"type":"array","source":{"type":"sphere","center":[0,0,0],"radius":0.08},"count":40,"points":[[0,0,0],[1,0,0],[1,0,1],[0,0,1],[0,0,0.05]]}))),
            ("posts per floor", form(json!({"type":"array","source":posts,"count":12,"step":[0,0.3,0]}))),
            ("floors of rings of lines", form(json!({"type":"array","source":{"type":"array","source":{"type":"array","source":{"type":"torus","center":[0,0,0],"radius":0.05,"thickness":0.015},"count":4,"step":[0.12,0,0]},"count":6,"radius":0.5},"count":5,"step":[0,0.25,0]}))),
            ("ellipsoid leaf", form(json!({"type":"array","source":{"type":"array","source":{"type":"ellipsoid","center":[0,0,0],"radii":[0.2,0.05,0.1]},"count":6,"step":[0.3,0,0]},"count":4,"step":[0,0.2,0.05]}))),
            ("single copy", form(json!({"type":"array","source":{"type":"box","center":[0,0,0],"size":[0.3,0.2,0.2]},"count":1,"step":[0.5,0,0]}))),
            ("coincident copies", form(json!({"type":"array","source":{"type":"sphere","center":[0.1,0,0],"radius":0.2},"count":9}))),
        ]
    }

    #[test]
    fn array_sdf_matches_brute_force() {
        for (name, f) in cases() {
            let (g, lo, hi) = Geo::new(&f, &Default::default());
            let (mut worst, mut exact) = (0.0f32, 0usize);
            let samples = points(&g, lo, hi, 10_000, 7);
            for &q in &samples {
                let (a, b) = (g.sdf(q), brute(&g, q));
                worst = worst.max((a - b).abs());
                exact += usize::from(a.to_bits() == b.to_bits());
            }
            eprintln!("{name}: {exact}/{} bit-identical, worst {worst:e}", samples.len());
            assert!(worst <= 1e-6, "{name}: differs from brute force by {worst}");
        }
    }

    /// Bounded lookups: the distance where it is below the bound, else a
    /// value from the bound up to the distance (never more: callers such
    /// as far_side read it as a lower bound).
    #[test]
    fn bounded_array_sdf_keeps_its_contract() {
        for (name, f) in cases() {
            let (g, lo, hi) = Geo::new(&f, &Default::default());
            for (i, &q) in points(&g, lo, hi, 4000, 11).iter().enumerate() {
                let exact = brute(&g, q);
                let bound = [-0.05, 0.0, 0.02, 0.1, 0.4][i % 5];
                let d = g.sdf_within(q, bound);
                if exact < bound {
                    assert!((d - exact).abs() <= 1e-6, "{name}: {d} for {exact} below {bound}");
                } else {
                    assert!(d >= bound - 1e-6 && d <= exact + 1e-6, "{name}: {d} for {exact} at bound {bound}");
                }
            }
        }
    }

    /// Meshes of every case come out the same, copy by copy or nearest
    /// first (one mesher thread, so the brute switch reaches every sample).
    #[test]
    fn array_meshes_match_brute_force() {
        for (name, f) in cases() {
            let mut blob = serde_json::to_value(&f).unwrap();
            blob["blend"] = json!(0);
            let c: Clay = serde_json::from_value(json!({"blobs":[blob],"resolution":48})).unwrap();
            let fast = crate::clay::with_threads(1, || c.build_mesh());
            BRUTE.set(true);
            let slow = crate::clay::with_threads(1, || c.build_mesh());
            BRUTE.set(false);
            assert!(!fast.indices.is_empty(), "{name}");
            assert_eq!(hash(&fast), hash(&slow), "{name}");
        }
    }

    /// The mesher's output does not depend on how many threads it had.
    #[test]
    fn meshes_do_not_depend_on_thread_count() {
        let c = tower(3, 12, 0.02);
        let one = crate::clay::with_threads(1, || (c.build_mesh(), c.build_fine(&[], "").unwrap()));
        let three = crate::clay::with_threads(3, || (c.build_mesh(), c.build_fine(&[], "").unwrap()));
        let all = (c.build_mesh(), c.build_fine(&[], "").unwrap());
        assert_eq!((hash(&one.0), hash(&one.1)), (hash(&all.0), hash(&all.1)));
        assert_eq!((hash(&three.0), hash(&three.1)), (hash(&all.0), hash(&all.1)));
    }

    pub(crate) fn hash(m: &Mesh) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |x: u32| { h ^= x as u64; h = h.wrapping_mul(0x0100_0000_01b3); };
        for v in &m.vertices {
            v.pos.iter().chain(&v.normal).chain(&v.color).for_each(|f| eat(f.to_bits()));
        }
        m.indices.iter().for_each(|&i| eat(i));
        h
    }

    /// A tower: a core, `floors` slabs, posts around each floor's edge
    /// (an array of a path array) and a ring of columns, with a dome
    /// blended on top.
    pub(crate) fn tower(floors: usize, posts: usize, detail_cell: f32) -> Clay {
        let height = floors as f32 * 0.3;
        let post = json!({"type":"box","center":[0,0.1,0],"size":[0.04,0.2,0.04]});
        let square = json!([[-0.6,0.4,-0.6],[0.6,0.4,-0.6],[0.6,0.4,0.6],[-0.6,0.4,0.6],[-0.6,0.4,-0.6]]);
        serde_json::from_value(json!({"blobs":[
            {"type":"box","center":[0,height*0.5,0],"size":[0.6,height,0.6],"blend":0,"name":"core"},
            {"type":"array","source":{"type":"extrude","profile":[[-0.65,-0.65],[0.65,-0.65],[0.65,0.65],[-0.65,0.65]],"depth":0.06,"axis":[0,1,0],"center":[0,0.25,0]},"count":floors,"step":[0,0.3,0],"blend":0,"name":"floors"},
            {"type":"array","source":{"type":"array","source":post,"count":posts,"points":square,"orient":"path"},"count":floors,"step":[0,0.3,0],"blend":0,"name":"posts"},
            {"type":"array","source":{"type":"box","center":[0,height*0.5,0],"size":[0.06,height,0.06]},"count":8,"radius":0.75,"blend":0,"name":"columns"},
            {"type":"sphere","center":[0,height,0],"radius":0.35,"blend":0.05,"name":"dome"}
        ],"detail_cell":detail_cell})).unwrap()
    }

    /// A closed icosphere of radius 0.2 and the same with a cap cut off
    /// its top (an open cup, read by its winding number).
    pub(crate) fn ball_assets() -> (crate::mesh_asset::MeshAsset, crate::mesh_asset::MeshAsset) {
        let (p, t) = crate::mesh_asset::tests::icosphere(2, 0.2);
        let cup: Vec<[u32; 3]> = t.iter().copied().filter(|f| f.iter().map(|&i| p[i as usize].y).sum::<f32>() / 3.0 < 0.15).collect();
        (crate::mesh_asset::MeshAsset::new(p.clone(), t).unwrap(), crate::mesh_asset::MeshAsset::new(p, cup).unwrap())
    }

    /// Arrays of mesh blobs: a line, a ring of turned and scaled copies,
    /// a shell of an array, nested arrays, closed and winding signs.
    pub(crate) fn mesh_cases() -> Vec<(&'static str, Clay)> {
        let (ball, cup) = ball_assets();
        let mesh = |a: &crate::mesh_asset::MeshAsset, sign: &str| json!({"type":"mesh","asset":a.hash(),"sign":sign,"center":[0.01,0.02,0],"rotate":[10,20,30],"scale":0.9});
        let forms = [
            ("closed line", json!({"type":"array","source":mesh(&ball,"closed"),"count":6,"step":[0.45,0,0]})),
            ("winding line", json!({"type":"array","source":mesh(&cup,"winding"),"count":6,"step":[0.45,0,0.1]})),
            ("winding ring", json!({"type":"array","source":mesh(&cup,"winding"),"count":10,"radius":0.6,"axis":[0.2,1,0.1]})),
            ("shell of a winding path", json!({"type":"shell","thickness":0.04,"source":{"type":"array","source":mesh(&cup,"winding"),"count":8,"points":[[0,0,0],[1,0,0],[1,0,1],[0,0.2,1]],"orient":"path"}})),
            ("nested closed", json!({"type":"array","source":{"type":"array","source":mesh(&ball,"closed"),"count":3,"step":[0.5,0,0]},"count":3,"step":[0,0.5,0.1]})),
        ];
        forms.into_iter().map(|(name, mut blob)| {
            blob["blend"] = json!(0);
            let mut c: Clay = serde_json::from_value(json!({"blobs":[blob],"resolution":48})).unwrap();
            for a in [&ball, &cup] {
                c.assets.insert(a.hash().to_string(), a.clone());
            }
            (name, c)
        }).collect()
    }

    /// Meshes of arrays of meshes, recorded before such arrays looked only
    /// at the copies near a point (every copy, a winding pass each). The
    /// shell's is brute force's: before, a point deep inside one copy and
    /// nearer another's surface read that distance as its depth (the old
    /// mesh was 0x3f63_d5fb_e932_0296, 36,708 tris).
    #[test]
    fn mesh_array_meshes_are_pinned() {
        let pinned = [
            ("closed line", 0x6eb5_7ecb_95f1_bda0, 1936),
            ("winding line", 0xfa41_b648_deb9_b4e6, 5952),
            ("winding ring", 0xbcba_5004_9f39_632e, 21964),
            ("shell of a winding path", 0x6903_8f17_bbe9_8ce9, 36720),
            ("nested closed", 0x04f9_de89_c4a5_ed44, 9796),
        ];
        for ((name, c), (pinned_name, h, tris)) in mesh_cases().into_iter().zip(pinned) {
            assert_eq!(name, pinned_name);
            let m = c.build_mesh();
            eprintln!("{name}: {:#x} ({} tris)", hash(&m), m.indices.len() / 3);
            assert_eq!((hash(&m), m.indices.len() / 3), (h, tris), "{name}");
        }
    }

    /// Arrays of meshes come out the same copy by copy (one mesher
    /// thread, so the brute switch reaches every sample).
    #[test]
    fn mesh_array_meshes_match_brute_force() {
        for (name, c) in mesh_cases() {
            let fast = crate::clay::with_threads(1, || c.build_mesh());
            BRUTE.set(true);
            let slow = crate::clay::with_threads(1, || c.build_mesh());
            BRUTE.set(false);
            assert!(!fast.indices.is_empty(), "{name}");
            assert_eq!(hash(&fast), hash(&slow), "{name}");
        }
    }

    /// 512 copies of a cup read by its winding number (a line, and 8 x 8 x
    /// 8 nested), sampled around the copies: each sample looks at a few
    /// copies (every copy before, 512 lookups and 512 winding passes) and
    /// runs the winding pass only inside a copy's box, bit for bit as
    /// every copy would give.
    #[test]
    fn mesh_arrays_look_at_the_copies_near_a_point() {
        for (name, f, assets) in mesh_arrays_of_512() {
            let (g, _, _) = Geo::new(&f, &assets);
            let (_, leaves) = array_copies(&f, true).unwrap();
            assert_eq!(leaves.len(), 512);
            let mut state = 9u64;
            let mut next = move || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            };
            let samples: Vec<Vec3> = (0..4000).map(|i| leaves[i * 7919 % 512].0 + Vec3::new(next(), next(), next()) * 0.6).collect();
            let reset = || crate::mesh_asset::tests::COUNTS.with(|c| c.replace((0, 0)));
            reset();
            let t = std::time::Instant::now();
            let fast: Vec<f32> = samples.iter().map(|&q| g.sdf(q)).collect();
            let fast_time = t.elapsed();
            let (lookups, windings) = reset();
            let t = std::time::Instant::now();
            let slow: Vec<f32> = samples.iter().map(|&q| brute(&g, q)).collect();
            let slow_time = t.elapsed();
            let (brute_lookups, brute_windings) = reset();
            let n = samples.len() as f64;
            let same = fast.iter().zip(&slow).filter(|(a, b)| a.to_bits() == b.to_bits()).count();
            eprintln!("{name}: {:.2} lookups and {:.2} winding passes per sample ({fast_time:.1?} for {n}); every copy: {:.0} and {:.2} ({slow_time:.1?}); {same}/{n} bit-identical",
                lookups as f64 / n, windings as f64 / n, brute_lookups as f64 / n, brute_windings as f64 / n);
            assert_eq!(same, samples.len(), "{name}");
            assert_eq!(brute_lookups, 512 * samples.len() as u64);
            assert!(lookups as f64 / n < 16.0, "{name}: {lookups} lookups");
            // A winding pass only where a copy's box holds the sample.
            assert!(windings > 0 && windings == brute_windings, "{name}: {windings} winding passes, {brute_windings} for every copy");
        }
    }

    /// The 512-copy arrays above, with their cup.
    fn mesh_arrays_of_512() -> Vec<(&'static str, Form, super::super::Assets)> {
        let (_, cup) = ball_assets();
        let mesh = json!({"type":"mesh","asset":cup.hash(),"sign":"winding","rotate":[10,20,30]});
        let assets = super::super::Assets::from([(cup.hash().to_string(), cup.clone())]);
        vec![
            ("line of 512", form(json!({"type":"array","source":mesh,"count":512,"step":[0.45,0,0]})), assets.clone()),
            ("8 x 8 x 8", form(json!({"type":"array","source":{"type":"array","source":{"type":"array","source":mesh,"count":8,"step":[0.45,0,0]},"count":8,"step":[0,0.45,0]},"count":8,"step":[0,0,0.45]})), assets),
        ]
    }

    /// Mesh build times of the 512-copy arrays:
    /// `cargo test --release time_mesh_arrays -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn time_mesh_arrays() {
        for (name, f, assets) in mesh_arrays_of_512() {
            for res in [48u32, 256] {
                let mut blob = serde_json::to_value(&f).unwrap();
                blob["blend"] = json!(0);
                let mut c: Clay = serde_json::from_value(json!({"blobs":[blob],"resolution":res})).unwrap();
                c.assets = assets.clone();
                let t = std::time::Instant::now();
                let m = c.build_mesh();
                eprintln!("{name}: res {res} mesh in {:.2}s, {} tris, {:#x}", t.elapsed().as_secs_f32(), m.indices.len() / 3, hash(&m));
            }
        }
    }

    /// Meshes of a small tower, recorded before array evaluation was
    /// accelerated and the mesher parallelized; neither may change them.
    #[test]
    fn nested_array_meshes_are_pinned() {
        let c = tower(6, 16, 0.0125);
        let regular = c.build_mesh();
        let fine = c.build_fine(&[], "").unwrap();
        eprintln!("regular {:#x} ({} tris) fine {:#x} ({} tris)", hash(&regular), regular.indices.len() / 3, hash(&fine), fine.indices.len() / 3);
        assert_eq!((hash(&regular), hash(&fine)), (TOWER_REGULAR, TOWER_FINE));
    }
    const TOWER_REGULAR: u64 = 0xe999_41d6_b669_9bb3;
    const TOWER_FINE: u64 = 0xbd86_3c41_7e93_ffa7;

    /// Regular and fine build times of a 48-floor tower:
    /// `cargo test --release time_tower -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn time_tower() {
        let floors = std::env::var("FLOORS").ok().and_then(|f| f.parse().ok()).unwrap_or(48);
        let c = tower(floors, 40, 0.02);
        let t = std::time::Instant::now();
        let regular = c.build_mesh();
        eprintln!("regular {:.2}s, {} tris, {:#x}", t.elapsed().as_secs_f32(), regular.indices.len() / 3, hash(&regular));
        let t = std::time::Instant::now();
        let fine = c.build_fine(&[], "").unwrap();
        eprintln!("fine {:.2}s, {} tris, {:#x}", t.elapsed().as_secs_f32(), fine.indices.len() / 3, hash(&fine));
    }
}
