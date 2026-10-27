//! Render-time displacement: materials with `detail_displace` move their
//! surface by their detail map's height, as real geometry, so stone chips,
//! scales and grain stand out of silhouettes and cast their own shadows.
//!
//! Before the ray tree is built, each such mesh is diced: edges are split
//! (crack-free, see `adaptive.rs`) until they are about a pixel long where
//! the cameras see them, never finer than the map's texels, and each
//! vertex then moves along its (unsplit) normal by the map's height read
//! at a matching mip level. Both tracers trace the same triangles, so CPU
//! and GPU agree. The shading normal stays the undisplaced one: the map's
//! own relief (`detail_strength`) still shades the fine slopes, the way a
//! matching normal map does over a displaced mesh.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Vec3, Vec4};

use super::{Camera, Item};
use crate::mesh::{Mesh, Vertex};

/// Most triangles dicing may add over a whole render.
pub const MAX_DICED: usize = 6_000_000;
/// Smallest displacement worth dicing for, in pixels at the surface.
const MIN_PIXELS: f32 = 0.35;

/// Frustum slack: a point counts as on screen within this factor of the
/// frustum's half-angle tangents (subdivision and displacement bulge past
/// the cage's and the mesh's points).
const MARGIN: f32 = 1.15;

/// What one camera sees: its place and axes, the world size of a pixel
/// per unit of distance (perspective) or in all (orthographic), and the
/// frustum's half extents (tangents with `MARGIN`, or ortho half sizes),
/// from the frame's width and height both.
pub(super) struct Eye {
    eye: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    pixel: f32,
    ortho: bool,
    /// Half extents: tangents (perspective) or half sizes (ortho).
    half_x: f32,
    half_y: f32,
}

impl Eye {
    pub(super) fn new(c: &Camera, width: usize, height: usize) -> Eye {
        let forward = (c.target - c.eye).normalize_or(Vec3::NEG_Z);
        let right = forward.cross(c.up).normalize_or(forward.any_orthonormal_vector());
        let up = right.cross(forward);
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let half = (c.fov_deg.to_radians() * 0.5).clamp(0.01, 1.5);
        match c.ortho {
            Some(h) => Eye { eye: c.eye, forward, right, up, pixel: 2.0 * h / height.max(1) as f32, ortho: true, half_x: h * aspect * MARGIN, half_y: h * MARGIN },
            None => Eye { eye: c.eye, forward, right, up, pixel: 2.0 * half.tan() / height.max(1) as f32, ortho: false, half_x: half.tan() * aspect * MARGIN, half_y: half.tan() * MARGIN },
        }
    }

    /// The cosine between `n` (unit) and the direction from `p` toward
    /// this camera: below 0 the camera sees the back of a surface there.
    pub(super) fn facing(&self, p: Vec3, n: Vec3) -> f32 {
        let toward = if self.ortho { -self.forward } else { (self.eye - p).normalize_or(-self.forward) };
        n.dot(toward)
    }

    /// `p` in the camera's axes (right, up, forward), from the eye.
    fn local(&self, p: Vec3) -> Vec3 {
        let d = p - self.eye;
        Vec3::new(d.dot(self.right), d.dot(self.up), d.dot(self.forward))
    }

    /// Which of the frustum's side (and, in perspective, near) planes `q`
    /// (camera axes) is outside of, as bits.
    fn outside(&self, q: Vec3) -> u8 {
        let (sx, sy) = if self.ortho { (self.half_x, self.half_y) } else { (self.half_x * q.z, self.half_y * q.z) };
        (q.x > sx) as u8 | ((q.x < -sx) as u8) << 1 | ((q.y > sy) as u8) << 2 | ((q.y < -sy) as u8) << 3 | ((!self.ortho && q.z <= 0.0) as u8) << 4
    }

    /// World size of a pixel at `p`; off screen it is taken 8 times larger
    /// (only shadows and reflections see it there).
    pub(super) fn footprint(&self, p: Vec3) -> f32 {
        if self.ortho {
            return self.pixel;
        }
        let dist = (p - self.eye).length().max(1e-4);
        let seen = self.outside(self.local(p)) == 0;
        self.pixel * dist * if seen { 1.0 } else { 8.0 }
    }

    /// World size of a pixel on the convex polygon `face` where it comes
    /// nearest this camera, and whether any of it may be on screen (a
    /// conservative frustum test: off screen only when every corner is
    /// outside one plane). Off screen it is taken 8 times larger.
    pub(super) fn face_footprint(&self, face: &[Vec3]) -> (f32, bool) {
        let all = face.iter().fold(0x1f_u8, |m, p| m & self.outside(self.local(*p)));
        let seen = all == 0;
        if self.ortho {
            return (self.pixel, seen);
        }
        let mut dist = f32::MAX;
        for k in 1..face.len().saturating_sub(1) {
            dist = dist.min(closest_on_triangle(self.eye, face[0], face[k], face[k + 1]).distance(self.eye));
        }
        if face.len() < 3 {
            dist = face.iter().map(|p| p.distance(self.eye)).fold(dist, f32::min);
        }
        (self.pixel * dist.max(1e-4) * if seen { 1.0 } else { 8.0 }, seen)
    }
}

/// The point of triangle `abc` nearest `p` (Ericson, Real-Time Collision
/// Detection 5.1.5).
pub(crate) fn closest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = va + vb + vc;
    if !(denom.abs() > 0.0) {
        return a;
    }
    a + ab * (vb / denom) + ac * (vc / denom)
}

/// Dices and displaces every item whose materials ask for it. Returns the
/// number of triangles added.
pub fn displace(items: &mut [Item], cams: &[Camera], width: usize, height: usize) -> usize {
    let eyes: Vec<Eye> = cams.iter().map(|c| Eye::new(c, width, height)).collect();
    if eyes.is_empty() {
        return 0;
    }
    let mut added = 0;
    for it in items.iter_mut() {
        if it.mesh.textures.is_some() || it.mesh.imported.is_some() {
            continue;
        }
        let Some(mut budget_scale) = wants(&it.mesh, &it.material) else { continue };
        let left = MAX_DICED.saturating_sub(added);
        let before = it.mesh.indices.len() / 3;
        // Too many triangles at a pixel: dice coarser until it fits.
        for _ in 0..6 {
            match dice(&it.mesh, &it.material, it.world, &eyes, budget_scale, before + left) {
                Ok(Some(mesh)) => {
                    added += mesh.indices.len() / 3 - before;
                    it.mesh = Arc::new(mesh);
                    break;
                }
                Ok(None) => break,
                Err(()) => budget_scale *= 2.0,
            }
        }
    }
    added
}

/// Some(1) when a material of the mesh displaces.
fn wants(mesh: &Mesh, object: &crate::material::Material) -> Option<f32> {
    // Vertices may defer to the object's material (see `material_of`).
    let any = amount(object).is_some() || mesh.materials.iter().any(|m| amount(m).is_some());
    any.then_some(1.0)
}

/// The material's map and displacement height, when it displaces.
fn amount(m: &crate::material::Material) -> Option<(crate::detail::Params, f32)> {
    (m.detail_displace > 0.0).then_some(())?;
    Some((crate::detail::Params::of(m)?, m.detail_displace))
}

fn material_of<'a>(mesh: &'a Mesh, object: &'a crate::material::Material, v: &Vertex) -> &'a crate::material::Material {
    if v.surface[0] < 0.0 || mesh.materials.is_empty() { object } else { mesh.materials.get(v.material as usize).unwrap_or(object) }
}

/// One mesh diced and displaced; Err when it would pass `max_triangles`,
/// None when nothing needs it (all too small to see).
fn dice(mesh: &Mesh, object: &crate::material::Material, world: glam::Mat4, eyes: &[Eye], coarser: f32, max_triangles: usize) -> Result<Option<Mesh>, ()> {
    let scale = world.to_scale_rotation_translation().0.abs().max_element().max(1e-6);
    let footprint = |local: Vec3| {
        let p = world.transform_point3(local);
        eyes.iter().map(|e| e.footprint(p)).fold(f32::MAX, f32::min) / scale * coarser
    };
    // Per vertex: (params, height) of its material, or None.
    let settings: Vec<Option<(crate::detail::Params, f32)>> = mesh.vertices.iter().map(|v| amount(material_of(mesh, object, v))).collect();
    if settings.iter().all(|s| s.is_none()) {
        return Ok(None);
    }
    // How long an edge should be at a point: a pixel, at least a texel,
    // and only where the displacement would show (else leave it).
    // The displacing materials' (texel size, height), each once. Edges
    // are sized for the strongest of them (the mesh does not say which
    // material a new point will be).
    let mut kinds: Vec<(f32, f32)> = Vec::new();
    for (params, height) in settings.iter().flatten() {
        let k = (1.0 / params.density, *height);
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    let size = |p: Vec3| {
        let pixel = footprint(p);
        kinds.iter().filter(|(_, h)| h / pixel >= MIN_PIXELS).map(|(texel, _)| pixel.max(*texel)).fold(f32::INFINITY, f32::min)
    };
    let mut positions: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let mut indices = mesh.indices.clone();
    let refined = crate::adaptive::refine(&mut positions, &mut indices, &size, &|p, _, _| p, max_triangles).map_err(|_| ())?;
    if refined.parents.is_empty() {
        return Ok(None);
    }
    let (mut vertices, normals) = interpolate(mesh, &refined.parents);
    // Displace: every vertex of a displacing material, by the map's height
    // filtered to its edge size (coarse far away, sharp up close).
    let mut edge_len = vec![0.0f32; vertices.len()];
    for t in indices.chunks_exact(3) {
        for e in 0..3 {
            let (a, b) = (t[e] as usize, t[(e + 1) % 3] as usize);
            let l = positions[a].distance(positions[b]);
            edge_len[a] = edge_len[a].max(l);
            edge_len[b] = edge_len[b].max(l);
        }
    }
    for (i, v) in vertices.iter_mut().enumerate() {
        let Some((params, height)) = amount(material_of(mesh, object, v)) else { continue };
        // Displace from the undisplaced spot, so shared places agree.
        let p = Vec3::from(v.pos);
        let n = normals[i];
        let (t, _) = params.triplanar(p, n, params.lod(edge_len[i]));
        let h = params.map.pattern(t.height) - 0.5;
        v.pos = (p + n * (h * height)).to_array();
    }
    let edges = crate::bake::edges(&indices);
    Ok(Some(Mesh { vertices, indices, edges, materials: mesh.materials.clone(), textures: None, imported: None, decals: mesh.decals.clone(), face: mesh.face.clone(), unshaded: None }))
}

/// A refined mesh's vertices: the old ones, then each new one as the
/// average of its edge's ends (see `adaptive::Refined`); and one smooth
/// normal per vertex, the same for vertices sharing a place (split
/// normals, UV seams), so they move together.
fn interpolate(mesh: &Mesh, parents: &[(u32, u32)]) -> (Vec<Vertex>, Vec<Vec3>) {
    let key = |p: [f32; 3]| p.map(f32::to_bits);
    let mut smooth: HashMap<[u32; 3], Vec3> = HashMap::new();
    for v in &mesh.vertices {
        *smooth.entry(key(v.pos)).or_default() += Vec3::from(v.normal);
    }
    let mut vertices = mesh.vertices.clone();
    let mut normals: Vec<Vec3> = mesh.vertices.iter().map(|v| smooth[&key(v.pos)].normalize_or(Vec3::from(v.normal))).collect();
    for &(a, b) in parents {
        let (va, vb) = (&vertices[a as usize], &vertices[b as usize]);
        let avg3 = |x: [f32; 3], y: [f32; 3]| ((Vec3::from(x) + Vec3::from(y)) * 0.5).to_array();
        let avg4 = |x: [f32; 4], y: [f32; 4]| ((Vec4::from(x) + Vec4::from(y)) * 0.5).to_array();
        let n = (Vec3::from(va.normal) + Vec3::from(vb.normal)).normalize_or(Vec3::from(va.normal));
        let tangent = { let t = avg4(va.tangent, vb.tangent); [t[0], t[1], t[2], va.tangent[3]] };
        let v = Vertex {
            pos: avg3(va.pos, vb.pos),
            normal: n.to_array(),
            color: avg3(va.color, vb.color),
            uv: [(va.uv[0] + vb.uv[0]) * 0.5, (va.uv[1] + vb.uv[1]) * 0.5],
            tangent,
            base: avg4(va.base, vb.base),
            wear: avg3(va.wear, vb.wear),
            painted: std::array::from_fn(|k| ((va.painted[k] as u16 + vb.painted[k] as u16) / 2) as u8),
            ..*va
        };
        normals.push((normals[a as usize] + normals[b as usize]).normalize_or(n));
        vertices.push(v);
    }
    (vertices, normals)
}

/// A baked displacement map (`bake::displacement`) read back as geometry:
/// `mesh` (the bake's simple mesh) diced until its edges are at most about
/// `edge` long, then every vertex moved by the map at its UV, along its
/// smooth normal (height) or in its tangent frame (vector, when there is
/// one). Shading normals stay the undisplaced ones, for the baked normal
/// map. Vertices split at UV seams (or by normals) each read their own
/// chart's texels, and every vertex at one place then moves by the mean
/// of their offsets, summed in vertex order, so a seam never opens
/// (Stage 3c's plan; done in Stage 4a).
pub fn apply_baked(mesh: &Mesh, map: &crate::bake::Displacement, edge: f32, max_triangles: usize) -> Result<Mesh, String> {
    if !(edge.is_finite() && edge > 0.0) {
        return Err("edge must be a positive length".into());
    }
    let mut positions: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    let mut indices = mesh.indices.clone();
    let refined = crate::adaptive::refine(&mut positions, &mut indices, &|_| edge, &|p, _, _| p, max_triangles)?;
    let (mut vertices, normals) = interpolate(mesh, &refined.parents);
    let mut offsets = Vec::with_capacity(vertices.len());
    for (v, n) in vertices.iter().zip(&normals) {
        let uv = glam::Vec2::from(v.uv);
        let offset = if map.vector.is_some() && map.object {
            map.vector_at(uv)
        } else if map.vector.is_some() {
            let t = Vec3::from_slice(&v.tangent);
            let t = (t - *n * n.dot(t)).normalize_or(n.any_orthonormal_vector());
            let b = n.cross(t) * v.tangent[3].signum();
            let d = map.vector_at(uv);
            t * d.x + b * d.y + *n * d.z
        } else {
            *n * map.height_at(uv)
        };
        offsets.push(offset);
    }
    // Seams: one offset per place, the mean of its vertices'.
    let key = |p: [f32; 3]| p.map(|x| if x == 0.0 { 0 } else { x.to_bits() });
    let mut at: HashMap<[u32; 3], (Vec3, u32)> = HashMap::with_capacity(vertices.len());
    for (v, o) in vertices.iter().zip(&offsets) {
        let e = at.entry(key(v.pos)).or_default();
        e.0 += *o;
        e.1 += 1;
    }
    for v in &mut vertices {
        let (sum, n) = at[&key(v.pos)];
        v.pos = (Vec3::from(v.pos) + sum / n as f32).to_array();
    }
    let edges = crate::bake::edges(&indices);
    Ok(Mesh { vertices, indices, edges, materials: mesh.materials.clone(), textures: None, imported: None, decals: mesh.decals.clone(), face: mesh.face.clone(), unshaded: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stone-displaced plane up close gains many triangles and real
    /// height; seen from far away it stays as it was.
    #[test]
    fn displacement_dices_near_the_camera_only() {
        let mut m = crate::material::Material { name: "rock".into(), detail: Some("stone".into()), detail_scale: 0.5, detail_displace: 0.03, ..crate::material::Material::default() };
        m.sanitize();
        let plane = Arc::new(crate::mesh::Mesh::build(crate::scene::Shape::Plane));
        let item = || Item { mesh: plane.clone(), world: glam::Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m.clone() };
        let cam = |d: f32| Camera { eye: Vec3::new(0.0, d, d), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 40.0, ortho: None };
        let mut near = vec![item()];
        let added = displace(&mut near, &[cam(1.0)], 400, 400);
        assert!(added > 5000, "{added}");
        let ys: Vec<f32> = near[0].mesh.vertices.iter().map(|v| v.pos[1]).collect();
        let spread = ys.iter().fold(f32::MIN, |a, b| a.max(*b)) - ys.iter().fold(f32::MAX, |a, b| a.min(*b));
        assert!(spread > 0.01 && spread <= 0.031, "{spread}");
        // Crack-free: an edge without a twin is on the plane's rim.
        let mesh = &near[0].mesh;
        let half = plane.vertices.iter().map(|v| v.pos[0].abs()).fold(0.0, f32::max);
        let mut seen = std::collections::HashSet::new();
        for t in mesh.indices.chunks_exact(3) {
            for e in 0..3 { seen.insert((t[e], t[(e + 1) % 3])); }
        }
        let rim = |i: u32| { let p = mesh.vertices[i as usize].pos; (p[0].abs() - half).abs() < 1e-4 || (p[2].abs() - half).abs() < 1e-4 };
        assert!(seen.iter().all(|&(a, b)| seen.contains(&(b, a)) || (rim(a) && rim(b))), "a crack inside the plane");
        let mut far = vec![item()];
        assert_eq!(displace(&mut far, &[cam(400.0)], 400, 400), 0);
    }

    /// Stage 3c's seam mitigation (done in 4a): a quad whose two
    /// triangles sit in different charts (the diagonal's vertices split,
    /// each side reading other texels) stays closed when a vector map is
    /// added: every place moves once, by the mean of its vertices'.
    #[test]
    fn baked_maps_keep_seams_closed() {
        // Chart A: the left triangle at u < 0.5; chart B: the right one at u > 0.5.
        let mut mesh = Mesh::default();
        for (p, uv) in [([0.0, 0.0, 0.0], [0.05, 0.05]), ([1.0, 0.0, 1.0], [0.45, 0.45]), ([0.0, 0.0, 1.0], [0.05, 0.45]), ([0.0, 0.0, 0.0], [0.55, 0.05]), ([1.0, 0.0, 0.0], [0.95, 0.05]), ([1.0, 0.0, 1.0], [0.95, 0.45])] {
            let i = mesh.vert(Vec3::from(p), Vec3::Y) as usize;
            mesh.vertices[i].uv = uv;
            mesh.vertices[i].tangent = [1.0, 0.0, 0.0, 1.0];
        }
        mesh.indices = vec![0, 2, 1, 3, 5, 4];
        let size = 8u32;
        let vector: Vec<[f32; 3]> = (0..size * size).map(|i| { let x = (i % size) as f32; [0.01 * x, 0.02 * (i / size) as f32, 0.05 + 0.03 * x] }).collect();
        let map = crate::bake::Displacement { size, height: None, vector: Some(vector), from_field: 0, missed: 0, object: false };
        let plain = apply_baked(&mesh, &crate::bake::Displacement { size: 1, height: None, vector: Some(vec![[0.0; 3]]), from_field: 0, missed: 0, object: false }, 0.1, 100_000).unwrap();
        let moved = apply_baked(&mesh, &map, 0.1, 100_000).unwrap();
        let places = |m: &Mesh| m.vertices.iter().map(|v| v.pos.map(f32::to_bits)).collect::<std::collections::HashSet<_>>().len();
        // Before the fix the diagonal's points split in two (a crack).
        assert_eq!(places(&moved), places(&plain), "every place moved once");
        let lifted = moved.vertices.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
        assert!(lifted > 0.04, "the map moved the points ({lifted})");
    }
}
