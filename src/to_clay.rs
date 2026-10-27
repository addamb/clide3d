//! Objects as clay (`to_clay`, InsertMesh): primitives become analytic
//! blobs, every other mesh a `Form::Mesh` (see `mesh_asset.rs`); and the
//! volume remesh (DynaMesh): the clay meshed at a resolution and made a
//! mesh blob again, with blur (a Gaussian over the field) and polish
//! (curvature flow) kept as sculpt offsets.

use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec3};

use crate::clay::{Blob, Clay, Detail, Field, Form, Op};
use crate::clay::Corner;
use crate::mesh::{Mesh, MeshLib};
use crate::mesh_asset::{self, MAX_FIELD_TRIANGLES, MeshAsset, Sign};
use crate::scene::{Scene, Shape};

/// Most points of the sources kept for the Hausdorff estimate.
const MAX_SAMPLES: usize = 4096;

/// An object turned into blobs (in the space `rel` maps its own into),
/// with the mesh assets they name and points on its surface there.
pub struct Converted {
    pub blobs: Vec<Blob>,
    pub assets: Vec<MeshAsset>,
    pub samples: Vec<Vec3>,
    /// Some(why) when a primitive had to become a mesh.
    pub note: Option<String>,
}

/// Rotation in degrees, XYZ (as `Form` takes it).
fn degrees(q: Quat) -> Vec3 {
    let (z, y, x) = q.to_euler(EulerRot::ZYX);
    Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI)
}

/// `m` as translation, rotation and positive scale, when it is exactly
/// that (no shear, no mirror).
fn similar(m: Mat4) -> Option<(Vec3, Quat, Vec3)> {
    if m.determinant() <= 0.0 {
        return None;
    }
    let (s, r, t) = m.to_scale_rotation_translation();
    let back = Mat4::from_scale_rotation_translation(s, r, t);
    let scale = s.max_element().max(1e-6);
    (0..4).all(|c| (m.col(c) - back.col(c)).abs().max_element() <= 1e-5 * scale.max(1.0)).then_some((t, r, s))
}

pub(crate) fn uniform(s: Vec3) -> bool {
    (s.max_element() - s.min_element()) <= 1e-5 * s.max_element()
}

/// A primitive's analytic form under `rel` (None: not representable).
fn primitive(shape: Shape, rel: Mat4) -> Option<Form> {
    let (t, r, s) = similar(rel)?;
    let rotate = degrees(r);
    Some(match shape {
        Shape::Cube => Form::Box { center: t, size: s, rotate, round: 0.0 },
        Shape::Sphere if uniform(s) => Form::Sphere { center: t, radius: 0.5 * s.x },
        Shape::Sphere => Form::Ellipsoid { center: t, radii: 0.5 * s, rotate },
        Shape::Cylinder | Shape::Cone if (s.x - s.z).abs() <= 1e-5 * s.x => {
            let (rad, h) = (0.5 * s.x, 0.5 * s.y);
            let pt = |u: f32, v: f32| Corner::Point(glam::Vec2::new(u, v));
            let profile = if shape == Shape::Cylinder { vec![pt(0.0, -h), pt(rad, -h), pt(rad, h), pt(0.0, h)] } else { vec![pt(0.0, -h), pt(rad, -h), pt(0.0, h)] };
            Form::Lathe { profile, center: t, axis: r * Vec3::Y }
        }
        Shape::Torus if uniform(s) => Form::Torus { center: t, radius: 0.35 * s.x, thickness: 0.15 * s.x, rotate },
        // A plane has no inside: a slab 1% of its size thick.
        Shape::Plane => Form::Box { center: t, size: Vec3::new(s.x, 0.01 * s.x.max(s.z), s.z), rotate, round: 0.0 },
        _ => return None,
    })
}

/// A triangle mesh (object space) as a mesh blob under `rel`: decimated
/// to `MAX_FIELD_TRIANGLES`, signed by the winding number unless closed.
pub fn mesh_blob(points: &[Vec3], tris: &[[u32; 3]], rel: Mat4, op: Op, color: [f32; 3]) -> Result<(Blob, MeshAsset, Vec<Vec3>), String> {
    let (points, tris) = mesh_asset::weld(points, tris);
    let mut points: Vec<Vec3> = points.iter().map(|p| rel.transform_point3(*p)).collect();
    let mut tris = tris;
    if rel.determinant() < 0.0 {
        // A mirroring placement turns the winding inside out.
        tris.iter_mut().for_each(|t| t.swap(1, 2));
    }
    let samples = every(&points, MAX_SAMPLES);
    let asset = MeshAsset::new(std::mem::take(&mut points), tris)?.decimated(MAX_FIELD_TRIANGLES)?;
    let sign = if mesh_asset::watertight(asset.points(), asset.tris()) { Sign::Closed } else { Sign::Winding };
    let blob = Blob::of(Form::Mesh { asset: asset.hash().to_string(), sign, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 }, op, color);
    Ok((blob, asset, samples))
}

/// At most `n` of `points`, evenly picked.
fn every(points: &[Vec3], n: usize) -> Vec<Vec3> {
    let step = points.len().div_ceil(n).max(1);
    points.iter().step_by(step).copied().collect()
}

fn mesh_parts(m: &Mesh) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    (m.vertices.iter().map(|v| Vec3::from(v.pos)).collect(), m.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect())
}

/// The blob color that shows `source`'s color on an object of color
/// `onto` (clay draws blob color times object color).
pub fn tint(source: [f32; 3], onto: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| if onto[i] > 1e-3 { (source[i] / onto[i]).clamp(0.0, 1.0) } else { source[i] })
}

/// What an object's clay is made from, taken on the editor's thread
/// (cheap: shared meshes) and made blobs on a job's (`Source::convert`).
#[derive(Clone)]
pub enum Body {
    Primitive(Shape),
    Mesh(Arc<Mesh>),
    Asset(MeshAsset),
}

#[derive(Clone)]
pub struct Source {
    pub name: String,
    pub body: Body,
}

/// Object `id`'s surface to make clay from. Meshes must be synced (clay
/// is read at its composed rest mesh).
pub fn source(scene: &Scene, meshes: &MeshLib, id: u64) -> Result<Source, String> {
    let o = scene.get(id).ok_or_else(|| format!("no object {id}"))?;
    let body = match o.shape {
        Shape::Cube | Shape::Sphere | Shape::Cylinder | Shape::Cone | Shape::Plane | Shape::Torus => Body::Primitive(o.shape),
        Shape::Clay => Body::Mesh(meshes.rest(id).map(|m| m.0.clone()).ok_or_else(|| format!("{} has no mesh yet", o.name))?),
        Shape::Mesh => Body::Asset(o.mesh.clone().ok_or_else(|| format!("{} holds no mesh", o.name))?),
        Shape::Group | Shape::Scatter | Shape::Fire => return Err(format!("{} ({}) has no surface of its own to make clay from", o.name, o.shape.label())),
        _ => Body::Mesh(meshes.get(o).cloned().ok_or_else(|| format!("{} has no mesh yet", o.name))?),
    };
    Ok(Source { name: o.name.clone(), body })
}

/// `m` as a placement a mesh blob takes (center, rotate, one scale).
fn placement(m: Mat4) -> Option<(Vec3, Vec3, f32)> {
    let (t, r, s) = similar(m)?;
    uniform(s).then(|| (t, degrees(r), s.x))
}

impl Source {
    /// The object as blobs with `op`, one set per placement in `places`
    /// (each maps the object's own space, after `inner`, into the clay's),
    /// colored `color`. A mesh is welded, decimated and hashed once: one
    /// asset in the space `inner` maps into, placed by each blob's center,
    /// rotate and scale. A placement that is not a rotation and one scale
    /// bakes into its own asset, so it is taken alone only. The samples
    /// are on the first placement's surface.
    pub fn convert(&self, inner: Mat4, places: &[Mat4], op: Op, color: [f32; 3]) -> Result<Converted, String> {
        let Some(&first) = places.first() else { return Ok(Converted { blobs: Vec::new(), assets: Vec::new(), samples: Vec::new(), note: None }) };
        let name = &self.name;
        let (p, t) = match &self.body {
            Body::Primitive(shape) => {
                // Analytic where the placement allows; else its own small
                // mesh, baked (a primitive's mesh is a few hundred points).
                let prim = Mesh::build(*shape);
                let (p, t) = mesh_parts(&prim);
                let rel = first * inner;
                let samples = every(&p.iter().map(|p| rel.transform_point3(*p)).collect::<Vec<_>>(), MAX_SAMPLES);
                let mut out = Converted { blobs: Vec::new(), assets: Vec::new(), samples, note: None };
                for &m in places {
                    match primitive(*shape, m * inner) {
                        Some(form) => out.blobs.push(Blob::of(form, op, color)),
                        None => {
                            let (blob, asset, _) = mesh_blob(&p, &t, m * inner, op, color)?;
                            out.blobs.push(blob);
                            out.assets.push(asset);
                            out.note = Some(format!("{name} is sheared or unevenly scaled: it became a mesh blob"));
                        }
                    }
                }
                return Ok(out);
            }
            Body::Mesh(m) => mesh_parts(m),
            Body::Asset(a) => (a.points().to_vec(), a.tris().to_vec()),
        };
        if t.is_empty() {
            return Err(format!("{name} has no triangles to make clay from"));
        }
        let placed: Vec<Option<(Vec3, Vec3, f32)>> = places.iter().map(|&m| placement(m)).collect();
        if placed.iter().all(Option::is_some) {
            let (mut blob, asset, samples) = mesh_blob(&p, &t, inner, op, color)?;
            let samples = samples.iter().map(|q| first.transform_point3(*q)).collect();
            let blobs = placed
                .into_iter()
                .flatten()
                .map(|(c, r, k)| {
                    if let Form::Mesh { center, rotate, scale, .. } = &mut blob.form {
                        (*center, *rotate, *scale) = (c, r, k);
                    }
                    blob.clone()
                })
                .collect();
            return Ok(Converted { blobs, assets: vec![asset], samples, note: None });
        }
        if places.len() > 1 {
            return Err(format!("{name} would be placed sheared, mirrored or unevenly scaled (is the target scaled unevenly?): it goes in one place at a time"));
        }
        let (blob, asset, samples) = mesh_blob(&p, &t, first * inner, op, color)?;
        Ok(Converted { blobs: vec![blob], assets: vec![asset], samples, note: None })
    }
}

/// Object `id` as blobs with `op`, in the space `rel` maps its own into,
/// colored `color`. Meshes must be synced (clay is read at its composed
/// rest mesh).
#[cfg(test)]
pub fn object_blobs(scene: &Scene, meshes: &MeshLib, id: u64, rel: Mat4, op: Op, color: [f32; 3]) -> Result<Converted, String> {
    source(scene, meshes, id)?.convert(Mat4::IDENTITY, &[rel], op, color)
}

/// The largest grid the volume remesh samples for blur and polish.
pub const MAX_VOLUME_POINTS: usize = 320 * 320 * 320;

/// The volume remesh of `clay` at `resolution` (DynaMesh): its whole
/// surface (sculpt included, paint dropped) meshed and made one closed
/// mesh blob in a new clay, then `blur` (Gaussian sigma in cells, 0..4)
/// and `polish` (curvature flow, 0..1) on the field, volume kept, as an
/// offset grid at the mesh cell. Ok(clay, notes).
pub fn remesh(clay: &Clay, resolution: u32, blur: f32, polish: f32, progress: &crate::progress::Progress) -> Result<(Clay, Vec<String>), String> {
    let mut whole = crate::layers::composed_clay(clay);
    (whole.paint, whole.sculpt_mask, whole.groups) = (None, None, None);
    whole.masks.clear();
    whole.resolution = resolution;
    let mesh = whole.build_mesh_with(&[], "");
    progress.check()?;
    let (p, t) = mesh_parts(&mesh);
    if t.is_empty() {
        return Err("the clay has no surface to remesh".into());
    }
    let color = clay.blobs.first().map_or([0.8; 3], |b| b.color);
    let (blob, asset, _) = mesh_blob(&p, &t, Mat4::IDENTITY, Op::Add, color)?;
    let mut notes = Vec::new();
    if let Form::Mesh { sign: Sign::Winding, .. } = blob.form {
        notes.push("the meshed surface was not closed; it is read by its winding number".into());
    }
    let mut out = Clay { blobs: vec![blob], resolution, sharp: clay.sharp, ..Clay::default() };
    out.assets.insert(asset.hash().to_string(), asset);
    progress.set(0.5);
    if blur > 0.0 || polish > 0.0 {
        out.detail = Some(smooth(&out, blur, polish, progress)?);
    }
    Ok((out, notes))
}

/// The blur and polish offsets for a clay (a dense grid at its mesh cell).
fn smooth(clay: &Clay, blur: f32, polish: f32, progress: &crate::progress::Progress) -> Result<Detail, String> {
    let cell = clay.cell_size().ok_or("the clay has no size")?;
    // A wide clamp, so the blur reads a true distance near the surface.
    let field = Field::new(clay, cell * 4.0);
    let (lo, hi) = field.bounds().ok_or("the clay has no size")?;
    let pad = (3.0 * blur).ceil() + 6.0;
    let i0 = (lo / cell).floor() - pad;
    let i1 = (hi / cell).ceil() + pad;
    let n = (i1 - i0 + 1.0).max(Vec3::ONE);
    let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
    if nx.saturating_mul(ny).saturating_mul(nz) > MAX_VOLUME_POINTS {
        return Err(format!("blur and polish sample at most {MAX_VOLUME_POINTS} points (a resolution of about 300); lower the resolution"));
    }
    let at = |x: usize, y: usize, z: usize| (x + nx * (y + ny * z)) as usize;
    let pos = |x: usize, y: usize, z: usize| (i0 + Vec3::new(x as f32, y as f32, z as f32)) * cell;
    // The field in cells, one z slice per task.
    let mut f0 = vec![0.0f32; nx * ny * nz];
    crate::clay::par_chunks(&mut f0, nx * ny, |z, slice| {
        let (a, b) = (pos(0, 0, z), pos(nx - 1, ny - 1, z));
        let list = field.near(a, b);
        for y in 0..ny {
            for x in 0..nx {
                slice[x + nx * y] = field.sample(pos(x, y, z), &list).0 / cell;
            }
        }
    });
    progress.check()?;
    let mut phi = f0.clone();
    if blur > 0.0 {
        let r = (3.0 * blur).ceil() as isize;
        let kernel: Vec<f32> = (-r..=r).map(|k| (-(k * k) as f32 / (2.0 * blur * blur)).exp()).collect();
        let sum: f32 = kernel.iter().sum();
        let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();
        for axis in 0..3 {
            let (len, stride) = [(nx, 1), (ny, nx), (nz, nx * ny)][axis];
            let src = phi.clone();
            crate::clay::par_chunks(&mut phi, nx * ny, |z, slice| {
                for y in 0..ny {
                    for x in 0..nx {
                        let i = at(x, y, z);
                        let c = [x, y, z][axis] as isize;
                        let mut v = 0.0;
                        for (k, w) in kernel.iter().enumerate() {
                            let j = (c + k as isize - r).clamp(0, len as isize - 1);
                            v += w * src[(i as isize + (j - c) * stride as isize) as usize];
                        }
                        slice[x + nx * y] = v;
                    }
                }
            });
        }
    }
    progress.set(0.7);
    // Mean curvature flow on the band (explicit, stable below dt 1/6).
    let steps = (polish.clamp(0.0, 1.0) * 20.0).round() as usize;
    for _ in 0..steps {
        progress.check()?;
        let src = phi.clone();
        crate::clay::par_chunks(&mut phi, nx * ny, |z, slice| {
            if z == 0 || z + 1 >= nz {
                return;
            }
            for y in 1..ny.saturating_sub(1) {
                for x in 1..nx.saturating_sub(1) {
                    let i = at(x, y, z);
                    let v = src[i];
                    if v.abs() > 4.0 {
                        continue;
                    }
                    let (sy, sz) = (nx as isize, (nx * ny) as isize);
                    let Some(flow) = crate::sculpt::flow::curvature(|a, b, c| src[(i as isize + a + b * sy + c * sz) as usize]) else { continue };
                    slice[x + nx * y] = v + 0.15 * flow;
                }
            }
        });
    }
    // Keep the volume: shift the result so it encloses what the clay did.
    let band: Vec<usize> = (0..f0.len()).filter(|&i| f0[i].abs() < 6.0 || phi[i].abs() < 6.0).collect();
    let fill = |f: &dyn Fn(usize) -> f32| -> f64 { band.iter().map(|&i| (0.5 - f(i) as f64).clamp(0.0, 1.0)).sum() };
    let target = fill(&|i| f0[i]);
    let (mut a, mut b) = (-3.0f32, 3.0f32);
    for _ in 0..40 {
        let m = 0.5 * (a + b);
        // The field minus m holds more the larger m is.
        if fill(&|i| phi[i] - m) > target { b = m } else { a = m }
    }
    let shift = 0.5 * (a + b);
    progress.set(0.9);
    let limit = crate::clay::MAX_OFFSET_VOXELS;
    let mut changes = Vec::new();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = at(x, y, z);
                if f0[i].abs() >= 3.5 {
                    continue;
                }
                let off = (phi[i] - shift - f0[i]).clamp(-limit, limit);
                if off.abs() > 1e-6 {
                    let g = (i0 + Vec3::new(x as f32, y as f32, z as f32)).as_ivec3();
                    changes.push(([g.x, g.y, g.z], off * cell));
                }
            }
        }
    }
    let mut detail = Detail::new(cell);
    detail.grid_mut().write(&changes, false);
    if detail.grid().brick_count() > crate::clay::MAX_BRICKS {
        return Err("the blur and polish offsets need more bricks than a clay holds; lower the resolution".into());
    }
    Ok(detail)
}

/// One-sided Hausdorff estimate: the farthest any of `samples` (points
/// on the sources' surfaces) is from the clay's mesh at its resolution,
/// in scene units (the clay is meshed once more for it).
pub fn hausdorff_est(clay: &Clay, samples: &[Vec3]) -> Option<f32> {
    let mesh = clay.build_mesh_with(&[], "");
    let (p, t) = mesh_parts(&mesh);
    if t.is_empty() {
        return None;
    }
    let field = mesh_asset::MeshField::new(&p, &t);
    Some(samples.iter().map(|&q| field.distance(q, Sign::Closed).abs()).fold(0.0, f32::max))
}

#[cfg(test)]
mod tests;
