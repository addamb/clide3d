//! Weathering: color and roughness that follow the shape, so surfaces stop
//! looking like one even plastic. A material's `cavity`, `edge_wear`,
//! `dirt`, `variation` and `gloss_variation` (see `material.rs`) say how
//! much of each; this module measures the shape and applies them.
//!
//! The shape is read from the mesh itself: each vertex's curvature from how
//! its neighbors' normals turn (positive on ridges and edges, negative in
//! creases), spread over about `width` so a crease darkens a band, not one
//! row of vertices. Clay meshes get it in their vertex colors and
//! roughness; bakes apply the same to every texel from the nearest vertex.

use glam::Vec3;

use crate::material::Material;
use crate::mesh::Mesh;
use crate::sculpt::noise;

/// Per vertex: (edge 0..1, cavity 0..1).
pub type Wear = [f32; 2];

/// Measures edges and creases of `mesh`, about `width` wide (scene units).
pub fn wear_maps(mesh: &Mesh, width: f32) -> Vec<Wear> {
    if let Some((geometry,remap))=mesh.welded_geometry() {
        let wear=wear_maps(&geometry,width);
        return remap.iter().map(|&i|wear[i]).collect();
    }
    let n = mesh.vertices.len();
    let mut nbrs: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in mesh.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            nbrs[a as usize].push(b);
            nbrs[b as usize].push(a);
        }
    }
    let pos = |i: usize| Vec3::from(mesh.vertices[i].pos);
    // Use area-weighted geometric normals. Sharp clay's shading normals are
    // field samples at displaced QEF vertices; they can turn across a planar
    // face and are not a curvature measurement.
    let mut normals = vec![Vec3::ZERO; n];
    for t in mesh.indices.chunks_exact(3) {
        let face = (pos(t[1] as usize) - pos(t[0] as usize))
            .cross(pos(t[2] as usize) - pos(t[0] as usize));
        for &i in t { normals[i as usize] += face; }
    }
    let nrm = |i: usize| normals[i].normalize_or(Vec3::Y);
    // Curvature (1 / length): how fast the normal turns along each edge.
    let mut curve: Vec<f32> = (0..n)
        .map(|i| {
            let nb = &nbrs[i];
            if nb.is_empty() {
                return 0.0;
            }
            nb.iter()
                .map(|&j| {
                    let d = pos(j as usize) - pos(i);
                    let len2 = d.length_squared().max(1e-12);
                    (nrm(j as usize) - nrm(i)).dot(d) / len2
                })
                .sum::<f32>()
                / nb.len() as f32
        })
        .collect();
    // Spread it over about `width`: a few averaging passes per edge length.
    let edge_len = mean_edge(mesh, &nbrs).max(1e-6);
    let passes = ((width / edge_len).powi(2) * 0.5).clamp(1.0, 40.0) as usize;
    for _ in 0..passes {
        let src = curve.clone();
        for (i, c) in curve.iter_mut().enumerate() {
            let nb = &nbrs[i];
            if !nb.is_empty() {
                let avg = nb.iter().map(|&j| src[j as usize]).sum::<f32>() / nb.len() as f32;
                *c = 0.5 * *c + 0.5 * avg;
            }
        }
    }
    // Dimensionless: how much the surface bends over `width`.
    curve.iter().map(|c| [smooth01(c * width / 0.35), smooth01(-c * width / 0.15)]).collect()
}

/// Sharp hard-surface clay needs field curvature; organic sculpted surfaces
/// keep mesh curvature so sculpted folds still contribute. Analytic human
/// covers also avoid repeated, expensive body-distance evaluations.
pub(crate) fn surface_wear(mesh: &Mesh, clay: &crate::clay::Clay, field: &crate::clay::Field) -> Vec<Wear> {
    if !clay.sharp || clay.blobs.iter().any(|b| matches!(b.form, crate::clay::Form::Human { .. })) {
        wear_maps(mesh, clay.wear_width())
    } else {
        clay_wear_maps(mesh, field, clay.wear_width())
    }
}

/// Clay curvature from the continuous CSG field over a physical footprint.
/// Unlike triangle-normal derivatives, this cannot amplify tiny QEF faces or
/// change when a bake/film refines the mesh. Flat SDF planes have zero Hessian.
/// Sample unclamped distances: the mesher's narrow band may be smaller than
/// the wear footprint, especially in fine meshes.
pub(crate) fn clay_wear_maps(mesh: &Mesh, field: &crate::clay::Field, width: f32) -> Vec<Wear> {
    let radius = (width * 0.5).clamp(0.0025, 0.0125);
    let mut out = vec![[0.0; 2]; mesh.vertices.len()];
    crate::clay::par_chunks(&mut out, 512, |chunk, values| {
        for (k, value) in values.iter_mut().enumerate() {
            let p = Vec3::from(mesh.vertices[chunk * 512 + k].pos);
            let list = field.near(p - radius * 2.0, p + radius * 2.0);
            let d = |q| field.wear_distance(q, &list);
            let center = d(p);
            let laplacian: f32 = [Vec3::X, Vec3::Y, Vec3::Z].iter()
                .map(|axis| (d(p + *axis * radius) - center) + (d(p - *axis * radius) - center)).sum();
            *value = wear_of(laplacian / radius);
        }
    });
    out
}

/// Edge and cavity wear (0..1) from a surface's bend: the field's
/// Laplacian over a footprint, per unit footprint (positive on ridges).
/// The sculpt mask's by_cavity and by_curvature read it too.
pub(crate) fn wear_of(bend: f32) -> Wear {
    [smooth01((bend - 0.015) / 0.6), smooth01((-bend - 0.015) / 0.4)]
}

/// `color` (sRGB) and `rough` at surface point `p` (normal `n`) after
/// the material's weathering, given the point's `wear`.
pub fn apply(m: &Material, p: Vec3, n: Vec3, color: Vec3, rough: f32, wear: Wear, width: f32) -> (Vec3, f32) {
    let [edge, cavity] = wear;
    let mut c = color;
    let mut r = rough;
    // Blotches, a few `variation_scale` apart, in brightness and a little hue.
    let s = m.variation_scale.max(1e-4);
    if m.variation > 0.0 {
        let v = noise(p / s) * 0.65 + noise(p / s * 2.7 + 17.0) * 0.35 - 0.5;
        let hue = noise(p / s * 0.6 + 41.0) - 0.5;
        c *= 1.0 + v * 0.7 * m.variation;
        c += Vec3::new(hue, -hue * 0.3, -hue) * 0.08 * m.variation;
    }
    if m.gloss_variation > 0.0 {
        r += (noise(p / s * 1.7 + 5.0) - 0.5) * 0.5 * m.gloss_variation;
    }
    // Creases: darker and duller (dirt and shadow gather there).
    let crease = cavity * m.cavity;
    c *= 1.0 - 0.65 * crease;
    r += 0.15 * crease;
    // Edges: rubbed lighter and smoother.
    let worn = edge * m.edge_wear;
    let light = c * 1.3 + Vec3::splat(0.06);
    let grey = Vec3::splat(light.dot(Vec3::splat(1.0 / 3.0)));
    c = c.lerp(light.lerp(grey, 0.3), worn * 0.7);
    r -= 0.2 * worn;
    // Dirt: in creases and on faces turned down, broken up by noise.
    if m.dirt > 0.0 {
        let patchy = noise(p / (width * 6.0).max(1e-4) + 3.0);
        let amount = ((cavity * 0.8 + (-n.y).max(0.0) * 0.4) * (0.5 + patchy) * m.dirt).clamp(0.0, 1.0);
        c = c.lerp(Vec3::from(m.dirt_color), amount * 0.85);
        r += 0.25 * amount;
    }
    (c.clamp(Vec3::ZERO, Vec3::ONE), r.clamp(0.03, 1.0))
}

/// `m` with its blotch size filled in from the clay's size when unset.
pub fn with_scale(m: &Material, clay: &crate::clay::Clay) -> Material {
    let mut m = m.clone();
    if m.variation_scale <= 0.0 {
        m.variation_scale = clay.wear_width() * 8.0;
    }
    m
}

fn mean_edge(mesh: &Mesh, nbrs: &[Vec<u32>]) -> f32 {
    let (mut sum, mut count) = (0.0, 0usize);
    for (i, nb) in nbrs.iter().enumerate().step_by(7) {
        for &j in nb {
            sum += Vec3::from(mesh.vertices[i].pos).distance(Vec3::from(mesh.vertices[j as usize].pos));
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f32 }
}

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box with a groove: edges read as edges, the groove as a crease.
    #[test]
    fn sharp_clay_faces_ignore_shading_normal_artifacts() {
        let clay:crate::clay::Clay=serde_json::from_value(serde_json::json!({
            "sharp":true,"resolution":64,"blobs":[
                {"type":"box","center":[0,0,0],"size":[1,1,1],"round":0.01,"blend":0},
                {"type":"box","center":[0,-0.4,0],"size":[0.2,0.1,0.1],"blend":0}
            ]})).unwrap();
        let mut mesh=clay.build_mesh();
        let expected=wear_maps(&mesh,0.025);
        for (i,v) in mesh.vertices.iter_mut().enumerate() {
            v.normal=Vec3::new((i as f32).sin(),1.0,(i as f32).cos()).normalize().to_array();
        }
        assert_eq!(expected,wear_maps(&mesh,0.025));
        let mut flat=0;
        let mut edge=0;
        for (v,w) in mesh.vertices.iter().zip(expected) {
            let p=Vec3::from(v.pos).abs();
            if p.z>0.48 && p.x<0.3 && p.y<0.3 {assert!(w[0]<0.05,"flat {p:?}: {w:?}");flat+=1;}
            if p.z>0.48 && p.x>0.48 && w[0]>0.1 {edge+=1;}
        }
        assert!(flat>100 && edge>0,"flat {flat} edge {edge}");
    }

    #[test]
    fn edges_and_creases_are_found_where_they_are() {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [
                { "type": "box", "center": [0, 0, 0], "size": [1, 1, 1], "blend": 0.0 },
                { "type": "box", "op": "carve", "center": [0, 0.5, 0], "size": [0.12, 0.2, 2], "blend": 0.0 }
            ],
            "resolution": 64
        }))
        .unwrap();
        let mesh = clay.build_mesh();
        let wear = wear_maps(&mesh, 0.05);
        let at = |p: Vec3| {
            let i = (0..mesh.vertices.len()).min_by(|&a, &b| Vec3::from(mesh.vertices[a].pos).distance(p).total_cmp(&Vec3::from(mesh.vertices[b].pos).distance(p))).unwrap();
            wear[i]
        };
        let corner = at(Vec3::new(0.5, -0.5, 0.0));
        // The groove's inner corner, where its floor meets its wall.
        let groove = at(Vec3::new(0.06, 0.4, 0.0));
        let flat = at(Vec3::new(0.0, 0.0, 0.5));
        assert!(corner[0] > 0.5, "corner {corner:?}");
        assert!(groove[1] > 0.3, "groove {groove:?}");
        assert!(flat[0] < 0.1 && flat[1] < 0.1, "flat {flat:?}");
    }

    #[test]
    fn weathering_darkens_creases_and_lightens_edges() {
        let m = Material { cavity: 1.0, edge_wear: 1.0, ..Material::default() };
        let c = Vec3::splat(0.5);
        let (crease, _) = apply(&m, Vec3::ZERO, Vec3::Y, c, 0.5, [0.0, 1.0], 0.01);
        let (edge, r) = apply(&m, Vec3::ZERO, Vec3::Y, c, 0.5, [1.0, 0.0], 0.01);
        let (plain, pr) = apply(&Material::default(), Vec3::ZERO, Vec3::Y, c, 0.5, [1.0, 1.0], 0.01);
        assert!(crease.x < 0.3 && edge.x > 0.55 && r < 0.5);
        assert_eq!((plain, pr), (c, 0.5));
    }
}
