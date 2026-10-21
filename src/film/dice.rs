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

/// What one camera sees: (eye, forward, world size of a pixel per unit
/// of distance (perspective) or in all (orthographic), cos of the half
/// field of view for culling).
struct Eye {
    eye: Vec3,
    forward: Vec3,
    pixel: f32,
    ortho: bool,
    cos_half: f32,
}

impl Eye {
    fn new(c: &Camera, height: usize) -> Eye {
        let forward = (c.target - c.eye).normalize_or(Vec3::NEG_Z);
        let half = (c.fov_deg.to_radians() * 0.5).clamp(0.01, 1.5);
        match c.ortho {
            Some(h) => Eye { eye: c.eye, forward, pixel: 2.0 * h / height.max(1) as f32, ortho: true, cos_half: -1.0 },
            None => Eye { eye: c.eye, forward, pixel: 2.0 * half.tan() / height.max(1) as f32, ortho: false, cos_half: (half * 1.6).min(1.5).cos() },
        }
    }

    /// World size of a pixel at `p`; off screen it is taken 8 times larger
    /// (only shadows and reflections see it there).
    fn footprint(&self, p: Vec3) -> f32 {
        if self.ortho {
            return self.pixel;
        }
        let to = p - self.eye;
        let dist = to.length().max(1e-4);
        let seen = to.dot(self.forward) / dist >= self.cos_half;
        self.pixel * dist * if seen { 1.0 } else { 8.0 }
    }
}

/// Dices and displaces every item whose materials ask for it. Returns the
/// number of triangles added.
pub fn displace(items: &mut [Item], cams: &[Camera], height: usize) -> usize {
    let eyes: Vec<Eye> = cams.iter().map(|c| Eye::new(c, height)).collect();
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
    // Vertices sharing a place (split normals) must move together: one
    // smooth normal per place.
    let key = |p: [f32; 3]| p.map(f32::to_bits);
    let mut smooth: HashMap<[u32; 3], Vec3> = HashMap::new();
    for v in &mesh.vertices {
        *smooth.entry(key(v.pos)).or_default() += Vec3::from(v.normal);
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
    // New vertices: the average of their edge's ends.
    let mut vertices = mesh.vertices.clone();
    let mut normals: Vec<Vec3> = mesh.vertices.iter().map(|v| smooth[&key(v.pos)].normalize_or(Vec3::from(v.normal))).collect();
    for &(a, b) in &refined.parents {
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
        let added = displace(&mut near, &[cam(1.0)], 400);
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
        assert_eq!(displace(&mut far, &[cam(400.0)], 400), 0);
    }
}
