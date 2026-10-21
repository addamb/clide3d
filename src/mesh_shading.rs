//! Refresh clay shading on the existing topology; never run a mesher or redo skin weights.
use crate::{
    clay::{Clay, Field},
    material::Material,
    mesh::Mesh,
};
use glam::Vec3;

pub fn refresh(
    clay: &Clay,
    library: &[Material],
    fallback: &str,
    source: &Mesh,
    parts: &[(u32, Vec<bool>)],
) -> Mesh {
    let cell = clay.cell_size().unwrap_or(0.01);
    let field = Field::new(clay, cell);
    let mut mesh = source.clone();
    mesh.materials = clay.palette(library, fallback);
    let materials = mesh.materials.clone();
    // Split rig shells use their own kept primitives at internal joints.
    let fields: Vec<_> = parts
        .iter()
        .map(|(_, keep)| Field::new(clay, cell).keep_only(keep.clone()))
        .collect();
    crate::clay::par_chunks(&mut mesh.vertices, 1024, |chunk, vertices| {
        for (j, v) in vertices.iter_mut().enumerate() {
            let index = chunk * 1024 + j;
            let f = parts
                .iter()
                .rposition(|(start, _)| *start as usize <= index)
                .map_or(&field, |i| &fields[i]);
            let p = Vec3::from(v.pos);
            let near = f.near(p - cell, p + cell);
            let source = crate::damage::source_point(&clay.decals, p);
            let (_, color, mat) = f.base_full(source, &near, true);
            v.color = clay
                .paint
                .as_ref()
                .map_or(color, |paint| paint.grid().apply(p, color))
                .to_array();
            v.material = (mat as usize).min(materials.len() - 1) as u32;
            let m = &materials[v.material as usize];
            v.surface = m.surface();
            v.light = m.light();
        }
    });
    crate::smart::finish(clay, &field, &mut mesh);
    mesh
}

/// Re-evaluate environment finish from original colors on unchanged geometry.
pub fn refresh_env(source: &Mesh, materials: Vec<Material>) -> Mesh {
    let mut mesh = source.clone();
    mesh.materials = materials;
    let (colors, width) = source.unshaded.as_ref().expect("environment source colors");
    let wear = crate::weather::wear_maps(source, *width);
    for ((v, color), wear) in mesh.vertices.iter_mut().zip(colors.iter()).zip(wear) {
        let mut m = mesh.materials[v.material as usize].clone();
        if m.variation_scale <= 0.0 {
            m.variation_scale = *width * 8.0;
        }
        v.color = *color;
        v.surface = m.surface();
        v.light = m.light();
        if m.weathered() {
            let (c, r) = crate::weather::apply(
                &m,
                Vec3::from(v.pos),
                Vec3::from(v.normal),
                Vec3::from(*color),
                v.surface[0],
                wear,
                *width,
            );
            v.color = c.to_array();
            v.surface[0] = r;
        }
    }
    crate::smart::finish_mesh(&mut mesh, *width);
    mesh
}
