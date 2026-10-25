//! Baking: turns clay into a game-ready mesh, the way studios make
//! characters.
//!
//! 1. Simplify the clay mesh to a triangle budget (meshoptimizer). Kept
//!    vertices are a subset of the clay's, so colors and skin weights carry
//!    over unchanged.
//! 2. Lay it flat into one texture atlas (xatlas UV unwrap).
//! 3. For every texel, find the matching point on the exact clay surface
//!    (blobs + sculpt detail, which are finer than any mesh) and record:
//!    color (blobs + paint), the surface normal relative to the simple
//!    mesh (a tangent-space normal map), and ambient occlusion (how hidden
//!    the point is, from the clay's distance field), roughness and metallic
//!    (from the materials).
//! 4. Grow every island a few texels so mipmaps and filtering do not pull
//!    in the empty background.
//!
//! Split rigs (`rig::build_parts`) bake each part against its own blobs, so
//! a body surface under an arm bakes the body, not the arm.

use std::sync::Arc;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::clay::{Clay, Field, par_chunks};
use crate::material::Material;
use crate::mesh::{Mesh, Textures, Vertex};
use crate::rig::Skin;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// Triangle budget for the simplified mesh.
    pub triangles: u32,
    /// Texture width and height in pixels.
    pub texture: u32,
    /// How much more texture the clay's detail regions get, 0..1: at 1 a
    /// region's texels shrink with its cell (a 4x finer cell gets 4x the
    /// texels across, capped at 4); 0 spreads texels evenly.
    pub region_density: f32,
    /// Read every texel from the exact clay field (slow) instead of the
    /// fine mesh; for comparisons.
    pub exact: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { triangles: 30_000, texture: 2048, region_density: 0.5, exact: false }
    }
}

pub const MIN_TRIANGLES: u32 = 500;
pub const MAX_TRIANGLES: u32 = 300_000;
pub const TEXTURE_SIZES: [u32; 5] = [512, 1024, 2048, 4096, 8192];

impl Settings {
    pub fn sanitize(&mut self) {
        self.triangles = self.triangles.clamp(MIN_TRIANGLES, MAX_TRIANGLES);
        if !TEXTURE_SIZES.contains(&self.texture) {
            self.texture = Settings::default().texture;
        }
        self.region_density = if self.region_density.is_finite() { self.region_density.clamp(0.0, 1.0) } else { 0.5 };
    }
}

/// What a bake reads.
pub struct Source<'a> {
    pub clay: &'a Clay,
    /// The clay's rest mesh (split into parts for a split rig).
    pub rest: &'a Mesh,
    pub skin: Option<&'a Skin>,
    /// Split rigs: (first vertex in `rest`, prims that make that part).
    pub parts: &'a [(u32, Vec<bool>)],
    pub library: &'a [Material],
    pub fallback: &'a str,
    /// The clay's fine mesh, if one is already built (else the bake makes it).
    pub fine: Option<&'a Mesh>,
}

/// Texels kept clear around each island when packing.
const PADDING: u32 = 4;

pub fn bake(src: &Source, settings: Settings, progress: &crate::progress::Progress) -> Result<(Mesh, Option<Skin>), String> {
    progress.stage("simplifying the mesh", 0.0);
    let mut settings = settings;
    settings.sanitize();
    let rest = src.rest;
    if rest.indices.is_empty() {
        return Err("nothing to bake: the clay is empty".into());
    }
    let cell = src.clay.cell_size().ok_or("nothing to bake: the clay is empty")?;
    let timer = std::time::Instant::now();
    let mut times: Vec<(&str, f32)> = Vec::new();
    let lap = |name: &'static str, times: &mut Vec<(&str, f32)>| times.push((name, timer.elapsed().as_secs_f32()));

    // 1. Simplify. The error limit is loose so the triangle budget decides.
    // Detail regions look bigger to the simplifier (space spread out from
    // each region's middle, smoothly), so they keep more triangles.
    let warped = warp_regions(rest, src.clay, settings.region_density);
    let bytes: &[u8] = match &warped {
        Some(w) => bytemuck::cast_slice(w),
        None => bytemuck::cast_slice(&rest.vertices),
    };
    let stride = if warped.is_some() { 12 } else { std::mem::size_of::<Vertex>() };
    let adapter = meshopt::VertexDataAdapter::new(bytes, stride, 0).map_err(|e| e.to_string())?;
    let target = (settings.triangles as usize * 3).min(rest.indices.len());
    let mut indices = meshopt::simplify(&rest.indices, &adapter, target, 1.0, meshopt::SimplifyOptions::None, None);
    if indices.is_empty() {
        return Err("simplifying removed every triangle".into());
    }
    // Keep only the vertices used, remembering where each came from.
    let mut remap = vec![u32::MAX; rest.vertices.len()];
    let mut origin: Vec<u32> = Vec::new();
    for &i in &indices {
        if remap[i as usize] == u32::MAX {
            remap[i as usize] = origin.len() as u32;
            origin.push(i);
        }
    }
    for i in &mut indices {
        *i = remap[*i as usize];
    }
    lap("simplify", &mut times);

    // 2. UV unwrap.
    progress.check()?;
    progress.stage("laying the mesh flat (UV unwrap)", 0.05);
    let size = settings.texture;
    let positions: Vec<[f32; 3]> = origin.iter().map(|&o| rest.vertices[o as usize].pos).collect();
    let normals: Vec<[f32; 3]> = origin.iter().map(|&o| rest.vertices[o as usize].normal).collect();
    let (uvs, out_indices, xref) = unwrap(&positions, &normals, &indices, size, src.clay, settings.region_density)?;
    let mut mesh = Mesh { materials: rest.materials.clone(), ..Mesh::default() };
    let mut source = Vec::with_capacity(uvs.len());
    for (uv, x) in uvs.iter().zip(&xref) {
        let o = origin[*x as usize] as usize;
        let mut vert = rest.vertices[o];
        vert.uv = *uv;
        vert.color = [1.0; 3];
        mesh.vertices.push(vert);
        source.push(o);
    }
    mesh.indices = out_indices;
    tangents(&mut mesh);
    mesh.edges = edges(&mesh.indices);
    let skin = src.skin.filter(|s| s.joints.len() == rest.vertices.len()).map(|s| Skin {
        joints: source.iter().map(|&o| s.joints[o]).collect(),
        weights: source.iter().map(|&o| s.weights[o]).collect(),
    });
    lap("unwrap", &mut times);

    // 3. The detailed surface to read: the fine mesh (built here unless
    // given) with its ambient occlusion per vertex; texels it misses read
    // the exact field. Split rigs read the field, part by part.
    let whole = Field::new(src.clay, cell);
    let (lo, hi) = whole.bounds().unwrap_or((Vec3::ZERO, Vec3::ONE));
    let ao_step = (hi - lo).length() * 0.004;
    let high = if settings.exact || src.parts.len() > 1 {
        None
    } else {
        progress.stage("building the detailed surface", 0.1);
        progress.check()?;
        Some(())
    };
    // Without a fine mesh already made, refine the rest mesh where there is
    // detail (much cheaper than building a fine mesh just for this).
    let built = high.and_then(|_| src.fine.is_none().then(|| src.clay.refine_rest(rest, crate::clay::MAX_FINE_TRIANGLES)));
    let high = high.map(|_| High::new(src.fine.unwrap_or_else(|| built.as_ref().expect("built")), rest, &whole, ao_step, cell));
    lap("detailed surface", &mut times);

    // 4. Bake every texel.
    progress.stage("baking textures", 0.2);
    let chunks_done = std::sync::atomic::AtomicUsize::new(0);
    let part_of = |o: usize| src.parts.iter().rposition(|(start, _)| *start as usize <= o).unwrap_or(0);
    let vertex_part: Vec<usize> = source.iter().map(|&o| part_of(o)).collect();
    // Weathering: each texel takes the nearest rest vertex's edges and
    // creases (see `weather.rs`), so the bake matches the clay.
    let weathered = palette_has_weather(src);
    let wear = weathered.then(|| crate::weather::surface_wear(src.rest, src.clay, &Field::new(src.clay, cell)));
    // Smart layers and decals read the rest mesh's mask inputs the same way.
    let smart = src.clay.palette(src.library, src.fallback).iter().any(crate::smart::layered) || !src.clay.decals.is_empty();
    let near_rest = (weathered || smart).then(|| crate::rig::NearestVertex::new(src.rest, cell * 2.0));
    let channels: Vec<&crate::paint::Paint> = crate::smart::channel_names(src.clay).iter().filter_map(|c| src.clay.masks.get(c)).collect();
    let decals: Vec<crate::smart::Decal> = src.clay.decals.iter().take(crate::smart::MAX_DECALS).cloned().collect();
    let wear_at = |p: Vec3| -> Option<crate::weather::Wear> { Some(wear.as_ref()?[near_rest.as_ref()?.index(p)?]) };
    let fields: Vec<Field> = src.parts.iter().map(|(_, keep)| Field::new(src.clay, cell).keep_only(keep.clone())).collect();
    let mut palette = src.clay.palette(src.library, src.fallback);
    let names = crate::smart::channel_names(src.clay);
    for m in &mut palette {
        crate::smart::resolve_slots(m, &names);
    }
    let weather = Weather { at: &wear_at, scaled: palette.iter().map(|m| crate::weather::with_scale(m, src.clay)).collect(), width: src.clay.wear_width(), face: src.clay.face(), texel: texel_size(&mesh, size) };
    let covered = raster_ids(&mesh, size);
    lap("raster", &mut times);
    let n = (size * size) as usize;
    let detail = src.clay.detail.as_ref().map(|d| d.finest());
    // Gradient step: finer than the mesh, but not below the sculpt grid.
    let e = detail.map_or(cell * 0.25, |d| (cell * 0.25).min(d * 0.5)).max(1e-5);
    // World size of one texel, so detail maps are read at a matching mip.
    let texel = texel_size(&mesh, size);
    let mut color = vec![0u8; n * 4];
    let mut normal = vec![0u8; n * 4];
    let mut orm = vec![0u8; n * 4];
    // Work in rows of texels, written straight into the three images.
    const ROWS: usize = 4096;
    let mut work: Vec<(&mut [u8], &mut [u8], &mut [u8])> = color.chunks_mut(ROWS * 4).zip(normal.chunks_mut(ROWS * 4)).zip(orm.chunks_mut(ROWS * 4)).map(|((c, n), o)| (c, n, o)).collect();
    let chunk_count = work.len().max(1);
    let missed = std::sync::atomic::AtomicUsize::new(0);
    par_chunks(&mut work, 1, |chunk, out| {
        if progress.cancelled() {
            return;
        }
        let finished = chunks_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        progress.set(0.2 + 0.72 * finished as f32 / chunk_count as f32);
        let (color, normal, orm) = &mut out[0];
        for k in 0..color.len() / 4 {
            let t = chunk * ROWS + k;
            let tri = covered[t];
            if tri == u32::MAX {
                continue;
            }
            let corner = |c: usize| &mesh.vertices[mesh.indices[tri as usize * 3 + c] as usize];
            let (a, b, c) = (corner(0), corner(1), corner(2));
            let Some(bary) = barycentric(a.uv, b.uv, c.uv, t, size) else { continue };
            let w = [1.0 - bary.x - bary.y, bary.x, bary.y];
            let lerp3 = |f: fn(&Vertex) -> Vec3| f(a) * w[0] + f(b) * w[1] + f(c) * w[2];
            let pos = lerp3(|v| Vec3::from(v.pos));
            let nrm = lerp3(|v| Vec3::from(v.normal)).normalize_or(Vec3::Y);
            let tan = lerp3(|v| Vec3::from_slice(&v.tangent));
            let tan = (tan - nrm * nrm.dot(tan)).normalize_or(nrm.any_orthonormal_vector());
            let bit = nrm.cross(tan) * a.tangent[3].signum();
            let part = vertex_part[mesh.indices[tri as usize * 3] as usize];
            let field = fields.get(part).unwrap_or(&whole);
            let found = high.as_ref().and_then(|h| h.find(pos, nrm));
            let mut s = match found {
                Some((p, hn, ao)) => shade(src.clay, field, &palette, p, hn, ao, cell, &weather),
                None => {
                    if high.is_some() {
                        missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    sample(src.clay, field, &whole, &palette, pos, nrm, cell, e, ao_step, &weather)
                }
            };
            let smooth = s.normal;
            // Library detail (weave, grain, pores), at the texel's size.
            // None where a face shades the skin.
            let skin_face = weather.face.as_ref().filter(|_| palette.get(s.material as usize).is_some_and(|m| m.subsurface > 0.0 && m.transmission <= 0.0));
            let share = skin_face.map_or(1.0, |f| 1.0 - f.keep(s.at));
            if let Some(mut d) = palette.get(s.material as usize).and_then(crate::detail::Params::of) {
                d.strength *= share;
                d.color *= share;
                (s.normal, s.color, s.rough) = d.apply(s.at, s.normal, s.color, s.rough, texel);
            }
            // Smart layers and decals, as the film tracer lays them.
            if let Some(m) = palette.get(s.material as usize).filter(|m| crate::smart::layered(m) || !decals.is_empty()) {
                let near = near_rest.as_ref().and_then(|n| n.vertex(s.at));
                let [edge, cavity, _] = near.map_or([0.0; 3], |v| v.wear);
                let mut painted = [0.0; 4];
                for (slot, mask) in channels.iter().enumerate() {
                    painted[slot] = mask.grid().coverage(s.at);
                }
                let occlusion = ((1.0 - s.ao) * crate::smart::OCCLUSION).clamp(0.0, 1.0);
                let at = crate::smart::At { p: s.at, n: smooth, edge, cavity, occlusion, painted, footprint: texel };
                let look = crate::smart::Look { metal: s.metal, ..crate::smart::Look::of(m, s.color, s.rough, s.normal) };
                let look = crate::smart::layers(m, &at, look);
                let look = if decals.is_empty() { look } else { crate::smart::decals(&decals, &at, look) };
                (s.color, s.rough, s.metal, s.normal) = (look.color, look.rough, look.metal, look.n);
            }
            // Normal relative to the simple mesh (tangent space, +Y = bitangent).
            let hn = s.normal;
            let tn = Vec3::new(hn.dot(tan), hn.dot(bit), hn.dot(nrm)).normalize_or(Vec3::Z);
            let cc = s.color.clamp(Vec3::ZERO, Vec3::ONE) * 255.0;
            color[k * 4..k * 4 + 4].copy_from_slice(&[cc.x as u8, cc.y as u8, cc.z as u8, 255]);
            let nn = (tn * 0.5 + 0.5) * 255.0;
            normal[k * 4..k * 4 + 4].copy_from_slice(&[nn.x as u8, nn.y as u8, nn.z as u8, 255]);
            let o = [s.ao, s.rough, s.metal].map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8);
            // Alpha 255 marks a baked texel until the gaps are filled.
            orm[k * 4..k * 4 + 4].copy_from_slice(&[o[0], o[1], o[2], 255]);
        }
    });
    progress.check()?;
    drop(work);
    lap("texels", &mut times);
    let mut filled: Vec<bool> = orm.chunks_exact(4).map(|t| t[3] == 255).collect();

    // 5. Fill every gap from the nearest island, so mipmaps of any level
    // and filtering never pull in background across a seam.
    progress.stage("filling texture gaps", 0.94);
    fill(&mut [&mut color, &mut normal, &mut orm], &mut filled, size as usize);
    // Background that is never sampled (an empty texture): flat normal,
    // open, the clay default.
    for t in (0..n).filter(|&t| !filled[t]) {
        normal[t * 4..t * 4 + 4].copy_from_slice(&[128, 128, 255, 255]);
        orm[t * 4..t * 4 + 4].copy_from_slice(&[255, 140, 0, 255]);
        color[t * 4 + 3] = 255;
    }
    lap("fill", &mut times);
    if std::env::var("BAKE_TIMES").is_ok() {
        let mut last = 0.0;
        let parts: Vec<String> = times.iter().map(|(k, t)| { let d = t - last; last = *t; format!("{k} {d:.2}s") }).collect();
        eprintln!("bake {size}: {} ({} texels from the field)", parts.join(", "), missed.load(std::sync::atomic::Ordering::Relaxed));
    }
    mesh.textures = Some(Arc::new(Textures { size, color, normal, orm, alpha: false }));
    Ok((mesh, skin))
}

/// Displacement maps a bake also makes (per call: they are files, not
/// saved on the object or shown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Maps {
    /// Height along the simple mesh's normal.
    pub displacement: bool,
    /// The whole offset in the simple mesh's tangent frame (VDM).
    pub vector: bool,
    /// Pixels across (over the bake's UVs).
    pub size: u32,
}

/// Largest displacement map: a vector map at 4096 is 192 MB of floats.
pub const MAX_MAP_SIZE: u32 = 4096;

/// Displacement maps over a baked mesh's UVs, in the clay's own (object)
/// units: where the clay surface is, seen from each texel's point of the
/// simple mesh. Height runs along the simple mesh's interpolated normal;
/// the vector is (along +u tangent, along the bitangent (glTF: toward -v),
/// along the normal). The surface is looked for only along that normal
/// (`displacement`), so the vector's first two parts are about zero: it
/// holds the height's offset in vector form, no sideways reach. Texels
/// outside every island hold their nearest island's values.
pub struct Displacement {
    pub size: u32,
    pub height: Option<Vec<f32>>,
    pub vector: Option<Vec<[f32; 3]>>,
    /// Texels whose surface was found on the clay field (not the fine mesh),
    /// and those where neither met a surface (left at 0).
    pub from_field: usize,
    pub missed: usize,
}

impl Displacement {
    /// The largest |value| over the maps (each vector component counts).
    pub fn max(&self) -> f32 {
        let h = self.height.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        self.vector.iter().flatten().flatten().fold(h, |m, v| m.max(v.abs()))
    }

    /// One code range for every map: value = (code / 65535 - 0.5) *
    /// scale (the tool gives each file its own).
    #[cfg(test)]
    pub fn scale(&self) -> f32 {
        2.0 * self.max()
    }

    /// 16-bit codes of `values` (mid grey = 0) at `scale` (see `scale`).
    pub fn encode16(values: impl Iterator<Item = f32>, scale: f32) -> Vec<u16> {
        values.map(|v| if scale > 0.0 { ((v / scale + 0.5).clamp(0.0, 1.0) * 65535.0).round() as u16 } else { 32768 }).collect()
    }

    /// One value back from its 16-bit code.
    pub fn decode16(code: u16, scale: f32) -> f32 {
        (code as f32 / 65535.0 - 0.5) * scale
    }

    /// Bilinear height at `uv` (0 without a height map).
    pub fn height_at(&self, uv: glam::Vec2) -> f32 {
        self.height.as_ref().map_or(0.0, |h| self.bilinear(uv, |i| Vec3::splat(h[i])).x)
    }

    /// Bilinear vector at `uv` (zero without a vector map).
    pub fn vector_at(&self, uv: glam::Vec2) -> Vec3 {
        self.vector.as_ref().map_or(Vec3::ZERO, |v| self.bilinear(uv, |i| Vec3::from(v[i])))
    }

    fn bilinear(&self, uv: glam::Vec2, at: impl Fn(usize) -> Vec3) -> Vec3 {
        let s = self.size as usize;
        let p = uv * self.size as f32 - glam::Vec2::splat(0.5);
        let (x0, y0) = (p.x.floor(), p.y.floor());
        let (fx, fy) = (p.x - x0, p.y - y0);
        let clamp = |v: f32| (v.max(0.0) as usize).min(s - 1);
        let (xa, xb, ya, yb) = (clamp(x0), clamp(x0 + 1.0), clamp(y0), clamp(y0 + 1.0));
        let top = at(ya * s + xa).lerp(at(ya * s + xb), fx);
        let bottom = at(yb * s + xa).lerp(at(yb * s + xb), fx);
        top.lerp(bottom, fy)
    }
}

/// Bakes displacement maps for `mesh` (a bake of `src`, with its UVs and
/// tangents): each texel's point on the simple mesh looks along its
/// normal, both ways, for the clay's fine surface (as the normal map
/// does, but reaching further: simplifying can move the surface several
/// cells), else for the clay field's surface on that line.
pub fn displacement(src: &Source, mesh: &Mesh, maps: Maps, progress: &crate::progress::Progress) -> Result<Displacement, String> {
    if !maps.displacement && !maps.vector {
        return Err("no displacement map asked for".into());
    }
    if !(TEXTURE_SIZES.contains(&maps.size) && maps.size <= MAX_MAP_SIZE) {
        return Err(format!("map_size must be one of {:?}", TEXTURE_SIZES.iter().filter(|s| **s <= MAX_MAP_SIZE).collect::<Vec<_>>()));
    }
    if mesh.vertices.iter().any(|v| v.tangent[3] == 0.0) {
        return Err("the baked mesh has no tangents".into());
    }
    let cell = src.clay.cell_size().ok_or("nothing to bake: the clay is empty")?;
    let whole = Field::new(src.clay, cell);
    let (lo, hi) = whole.bounds().unwrap_or((Vec3::ZERO, Vec3::ONE));
    let reach = (cell * 8.0).max((hi - lo).length() * 0.05);
    progress.stage("building the detailed surface", 0.0);
    let built = src.fine.is_none().then(|| src.clay.refine_rest(src.rest, crate::clay::MAX_FINE_TRIANGLES));
    let high = High::bare(src.fine.unwrap_or_else(|| built.as_ref().expect("built")), reach);
    progress.check()?;
    progress.stage("baking displacement", 0.1);
    let size = maps.size;
    let covered = grown_ids(&raster_ids(mesh, size), size as usize, EXTRAPOLATE);
    let n = (size * size) as usize;
    // Per texel: Some(offset in the tangent frame) once baked.
    let mut texels: Vec<Option<Vec3>> = vec![None; n];
    let all: Vec<u32> = (0..whole.prim_count() as u32).collect();
    let from_field = std::sync::atomic::AtomicUsize::new(0);
    let missed = std::sync::atomic::AtomicUsize::new(0);
    const ROWS: usize = 4096;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let chunk_count = n.div_ceil(ROWS).max(1);
    par_chunks(&mut texels, ROWS, |chunk, out| {
        if progress.cancelled() {
            return;
        }
        let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        progress.set(0.1 + 0.85 * finished as f32 / chunk_count as f32);
        for (k, texel) in out.iter_mut().enumerate() {
            let t = chunk * ROWS + k;
            let tri = covered[t];
            if tri == u32::MAX {
                continue;
            }
            let corner = |c: usize| &mesh.vertices[mesh.indices[tri as usize * 3 + c] as usize];
            let (a, b, c) = (corner(0), corner(1), corner(2));
            let Some(bary) = barycentric(a.uv, b.uv, c.uv, t, size) else { continue };
            let w = [1.0 - bary.x - bary.y, bary.x, bary.y];
            let lerp3 = |f: fn(&Vertex) -> Vec3| f(a) * w[0] + f(b) * w[1] + f(c) * w[2];
            let pos = lerp3(|v| Vec3::from(v.pos));
            let nrm = lerp3(|v| Vec3::from(v.normal)).normalize_or(Vec3::Y);
            let tan = lerp3(|v| Vec3::from_slice(&v.tangent));
            let tan = (tan - nrm * nrm.dot(tan)).normalize_or(nrm.any_orthonormal_vector());
            let bit = nrm.cross(tan) * a.tangent[3].signum();
            let p = match high.find(pos, nrm) {
                Some((p, _, _)) => p,
                None => {
                    let near = whole.near(pos - Vec3::splat(reach), pos + Vec3::splat(reach));
                    let list = if near.is_empty() { &all } else { &near };
                    let f = |q: Vec3| whole.sample(q, list).0;
                    let p = crate::adaptive::along(&f, pos, nrm, cell * 0.05, reach);
                    if p == pos && f(pos).abs() > cell * 0.05 {
                        missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        from_field.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    p
                }
            };
            let d = p - pos;
            *texel = Some(Vec3::new(d.dot(tan), d.dot(bit), d.dot(nrm)));
        }
    });
    progress.check()?;
    // Fill the gaps between islands from the nearest island (as the
    // textures are), so filtering never pulls in zero across a seam.
    progress.stage("filling map gaps", 0.96);
    let mut filled: Vec<bool> = texels.iter().map(Option::is_some).collect();
    let mut values: Vec<Vec3> = texels.into_iter().map(|t| t.unwrap_or(Vec3::ZERO)).collect();
    fill_values(&mut values, &mut filled, size as usize);
    Ok(Displacement {
        size,
        height: maps.displacement.then(|| values.iter().map(|v| v.z).collect()),
        vector: maps.vector.then(|| values.iter().map(|v| v.to_array()).collect()),
        from_field: from_field.into_inner(),
        missed: missed.into_inner(),
    })
}

/// Texels around each island baked from its own triangles, extended
/// past their edges: a renderer's bilinear read at an island's rim then
/// blends values of the same surface, not the gap fill's averages.
const EXTRAPOLATE: usize = 2;

/// `ids` (a triangle per texel, `u32::MAX`: none) grown `rings` texels
/// outward, each new texel taking a covered neighbor's triangle.
fn grown_ids(ids: &[u32], size: usize, rings: usize) -> Vec<u32> {
    let mut out = ids.to_vec();
    for _ in 0..rings {
        let before = out.clone();
        for t in (0..out.len()).filter(|&t| before[t] == u32::MAX) {
            let (x, y) = ((t % size) as i32, (t / size) as i32);
            let near = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)].into_iter().find_map(|(dx, dy)| {
                let (nx, ny) = (x + dx, y + dy);
                (nx >= 0 && ny >= 0 && (nx as usize) < size && (ny as usize) < size).then(|| before[ny as usize * size + nx as usize]).filter(|&id| id != u32::MAX)
            });
            if let Some(id) = near {
                out[t] = id;
            }
        }
    }
    out
}

/// `fill` for float texels.
fn fill_values(values: &mut [Vec3], filled: &mut [bool], size: usize) {
    let neighbors = |t: usize| {
        let (x, y) = ((t % size) as i32, (t / size) as i32);
        [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)].into_iter().filter_map(move |(dx, dy)| {
            let (nx, ny) = (x + dx, y + dy);
            (nx >= 0 && ny >= 0 && (nx as usize) < size && (ny as usize) < size).then(|| ny as usize * size + nx as usize)
        })
    };
    let mut queued = filled.to_vec();
    let mut ring: Vec<usize> = (0..filled.len()).filter(|&t| !filled[t] && neighbors(t).any(|n| filled[n])).collect();
    for &t in &ring {
        queued[t] = true;
    }
    while !ring.is_empty() {
        let next_values: Vec<Vec3> = ring.iter().map(|&t| {
            let (sum, count) = neighbors(t).filter(|&n| filled[n]).fold((Vec3::ZERO, 0), |(s, c), n| (s + values[n], c + 1));
            sum / count.max(1) as f32
        }).collect();
        for (&t, v) in ring.iter().zip(next_values) {
            values[t] = v;
            filled[t] = true;
        }
        let mut next = Vec::new();
        for &t in &ring {
            for n in neighbors(t) {
                if !queued[n] {
                    queued[n] = true;
                    next.push(n);
                }
            }
        }
        ring = next;
    }
}

/// The rest mesh's positions with each detail region spread out from its
/// middle by its texel-density factor (see `Settings::region_density`),
/// fading over the region's band; None without regions.
fn warp_regions(rest: &Mesh, clay: &Clay, density: f32) -> Option<Vec<[f32; 3]>> {
    let grid = clay.detail.as_ref()?.grid();
    if grid.levels().is_empty() || density <= 0.0 {
        return None;
    }
    let spread: Vec<(Vec3, f32, &crate::clay::Level)> = grid.levels().iter().map(|l| (l.region.center, (grid.voxel / l.grid.voxel).clamp(1.0, 4.0).powf(density), l)).collect();
    Some(rest.vertices.iter().map(|v| {
        let p = Vec3::from(v.pos);
        let mut q = p;
        for (c, k, l) in &spread {
            let w = l.weight(p);
            if w > 0.0 {
                q += (p - *c) * (k - 1.0) * w;
            }
        }
        q.to_array()
    }).collect())
}

/// Lower levels of detail of a game mesh, for engines that swap meshes
/// with distance: LOD1, LOD2 and LOD3 at about a half, a quarter and an
/// eighth of its triangles. Each keeps a subset of its vertices (so UVs,
/// tangents, textures and skin weights carry over) and its UV seams and
/// open borders stay in place, so the textures still fit.
pub fn lods(mesh: &Mesh, skin: Option<&Skin>) -> Vec<(Mesh, Option<Skin>)> {
    let bytes: &[u8] = bytemuck::cast_slice(&mesh.vertices);
    let Ok(adapter) = meshopt::VertexDataAdapter::new(bytes, std::mem::size_of::<Vertex>(), 0) else { return Vec::new() };
    let full = mesh.indices.len() / 3;
    let mut out = Vec::new();
    let mut from = mesh.indices.clone();
    for level in 1..=3u32 {
        let target = ((full >> level).max(MIN_TRIANGLES as usize / 4)) * 3;
        if target >= from.len() {
            break;
        }
        // Seams first kept exactly; if that stalls far above the goal,
        // let border vertices slide too (a small crack beats no LOD).
        let mut indices = meshopt::simplify(&from, &adapter, target, 1.0, meshopt::SimplifyOptions::LockBorder, None);
        if indices.len() > target * 3 / 2 {
            indices = meshopt::simplify(&from, &adapter, target, 1.0, meshopt::SimplifyOptions::None, None);
        }
        if indices.is_empty() || indices.len() >= from.len() {
            break;
        }
        from = indices.clone();
        let mut remap = vec![u32::MAX; mesh.vertices.len()];
        let mut kept: Vec<u32> = Vec::new();
        for i in &mut indices {
            if remap[*i as usize] == u32::MAX {
                remap[*i as usize] = kept.len() as u32;
                kept.push(*i);
            }
            *i = remap[*i as usize];
        }
        let lod = Mesh {
            vertices: kept.iter().map(|&k| mesh.vertices[k as usize]).collect(),
            edges: edges(&indices),
            indices,
            materials: mesh.materials.clone(),
            textures: mesh.textures.clone(),
            imported: mesh.imported.clone(),
            decals: mesh.decals.clone(),
            face: mesh.face.clone(), unshaded: None,
        };
        let skin = skin.filter(|s| s.joints.len() == mesh.vertices.len()).map(|s| Skin {
            joints: kept.iter().map(|&k| s.joints[k as usize]).collect(),
            weights: kept.iter().map(|&k| s.weights[k as usize]).collect(),
        });
        out.push((lod, skin));
    }
    out
}

/// An unwrap: (uv per output vertex, triangles, source vertex of each).
type Unwrapped = (Vec<[f32; 2]>, Vec<u32>, Vec<u32>);

/// UV unwrap (xatlas) into one atlas of `size`: (uv per output vertex,
/// output triangles, source vertex of each output vertex). With
/// `density` > 0, charts in the clay's detail regions are grown before
/// packing so they get more texels.
fn unwrap(positions: &[[f32; 3]], normals: &[[f32; 3]], indices: &[u32], size: u32, clay: &Clay, density: f32) -> Result<Unwrapped, String> {
    let flat_pos: Vec<f32> = positions.iter().flatten().copied().collect();
    let flat_nrm: Vec<f32> = normals.iter().flatten().copied().collect();
    let decl = xatlas_rs_v2::MeshDecl {
        vertex_position_data: xatlas_rs_v2::MeshData::Contiguous(&flat_pos),
        vertex_normal_data: Some(xatlas_rs_v2::MeshData::Contiguous(&flat_nrm)),
        index_data: Some(xatlas_rs_v2::IndexData::U32(indices)),
        face_count: (indices.len() / 3) as u32,
        ..Default::default()
    };
    let pack = xatlas_rs_v2::PackOptions { resolution: size, padding: PADDING, bilinear: true, ..Default::default() };
    let mut atlas = xatlas_rs_v2::Xatlas::new();
    atlas.add_mesh(&decl).map_err(|e| format!("UV unwrap failed: {e:?}"))?;
    let grid = clay.detail.as_ref().map(|d| d.grid()).filter(|g| !g.levels().is_empty() && density > 0.0);
    let Some(grid) = grid else {
        atlas.generate(&xatlas_rs_v2::ChartOptions::default(), &pack);
        return finish_atlas(&atlas);
    };
    // Charts first, then each chart scaled by its detail and packed again.
    // (Only the charts' shapes are kept from this pass: pack it small.)
    let t = std::time::Instant::now();
    atlas.generate(&xatlas_rs_v2::ChartOptions::default(), &xatlas_rs_v2::PackOptions { resolution: 1024, padding: 2, ..pack });
    if std::env::var("BAKE_TIMES").is_ok() { eprintln!("bake first unwrap: {:.2}s", t.elapsed().as_secs_f32()); }
    let (uvs, out_indices, xref) = finish_atlas(&atlas)?;
    let meshes = atlas.meshes();
    let m = meshes.first().ok_or("UV unwrap failed")?;
    let mut chart_of = vec![u32::MAX; uvs.len()];
    let mut scale: Vec<(f32, f32)> = vec![(0.0, 0.0); m.chart_array.len()];
    for (c, chart) in m.chart_array.iter().enumerate() {
        for &f in chart.face_array {
            let t = &out_indices[f as usize * 3..f as usize * 3 + 3];
            let center = t.iter().map(|&i| Vec3::from(positions[xref[i as usize] as usize])).sum::<Vec3>() / 3.0;
            let ratio = (grid.voxel / grid.cell_at(center)).clamp(1.0, 4.0);
            scale[c].0 += ratio.powf(density);
            scale[c].1 += 1.0;
            for &i in t {
                chart_of[i as usize] = c as u32;
            }
        }
    }
    // Pack again with the same charts (the first unwrap's UVs), each
    // chart's surface grown about its middle: xatlas sizes charts by their
    // surface area, so a chart grown k times gets k times the texels across.
    let mut middle = vec![(Vec3::ZERO, 0.0f32); scale.len()];
    for (i, &x) in xref.iter().enumerate() {
        if let Some(m) = middle.get_mut(chart_of[i] as usize) {
            m.0 += Vec3::from(positions[x as usize]);
            m.1 += 1.0;
        }
    }
    let grown: Vec<f32> = xref.iter().enumerate().flat_map(|(i, &x)| {
        let c = chart_of[i] as usize;
        let p = Vec3::from(positions[x as usize]);
        let p = match (scale.get(c), middle.get(c)) {
            (Some(&(sum, count)), Some(&(m, mc))) if count > 0.0 => { let mid = m / mc.max(1.0); mid + (p - mid) * (sum / count) }
            _ => p,
        };
        [p.x, p.y, p.z]
    }).collect();
    let flat_nrm: Vec<f32> = xref.iter().flat_map(|&x| normals[x as usize]).collect();
    let flat_uv: Vec<f32> = uvs.iter().flatten().copied().collect();
    let decl = xatlas_rs_v2::MeshDecl {
        vertex_position_data: xatlas_rs_v2::MeshData::Contiguous(&grown),
        vertex_normal_data: Some(xatlas_rs_v2::MeshData::Contiguous(&flat_nrm)),
        vertex_uv_data: Some(xatlas_rs_v2::MeshData::WithStride { data: bytemuck::cast_slice(&flat_uv), stride: 8 }),
        index_data: Some(xatlas_rs_v2::IndexData::U32(&out_indices)),
        face_count: (out_indices.len() / 3) as u32,
        ..Default::default()
    };
    let mut again = xatlas_rs_v2::Xatlas::new();
    again.add_mesh(&decl).map_err(|e| format!("UV repack failed: {e:?}"))?;
    again.generate(&xatlas_rs_v2::ChartOptions { use_input_mesh_uvs: true, ..Default::default() }, &pack);
    let (uvs2, indices2, xref2) = finish_atlas(&again)?;
    // The repack's vertices refer to the first unwrap's.
    Ok((uvs2, indices2, xref2.iter().map(|&x| xref[x as usize]).collect()))
}

/// An atlas's single mesh as (uv, triangles, source vertex).
fn finish_atlas(atlas: &xatlas_rs_v2::Xatlas) -> Result<Unwrapped, String> {
    if atlas.atlas_count() != 1 {
        return Err("UV unwrap needed more than one texture; use a bigger texture or fewer triangles".into());
    }
    let (aw, ah) = (atlas.width().max(1) as f32, atlas.height().max(1) as f32);
    let meshes = atlas.meshes();
    let m = meshes.first().ok_or("UV unwrap failed")?;
    // xatlas gives texels in an atlas that can come out smaller than
    // asked; stretch it (keeping its shape) to fill the texture.
    let uvs = m.vertex_array.iter().map(|v| [v.uv[0] / aw.max(ah), v.uv[1] / aw.max(ah)]).collect();
    let xref = m.vertex_array.iter().map(|v| v.xref).collect();
    Ok((uvs, m.index_array.to_vec(), xref))
}

/// The fine surface a bake reads: its triangles in a ray tree, and ambient
/// occlusion per vertex (it changes slowly, so vertices are enough).
struct High<'a> {
    mesh: &'a Mesh,
    bvh: crate::film::bvh::Bvh,
    ao: Vec<f32>,
    /// How far from the simple mesh to look for it.
    reach: f32,
}

impl<'a> High<'a> {
    fn new(mesh: &'a Mesh, rest: &Mesh, whole: &Field, ao_step: f32, cell: f32) -> High<'a> {
        let corners: Vec<[Vec3; 3]> = mesh.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| Vec3::from(mesh.vertices[t[k] as usize].pos))).collect();
        let t = std::time::Instant::now();
        let bvh = crate::film::bvh::Bvh::new(&corners);
        if std::env::var("BAKE_TIMES").is_ok() { eprintln!("bake ray tree: {:.2}s", t.elapsed().as_secs_f32()); }
        // Occlusion changes slowly: read it at the rest mesh's vertices and
        // give each fine vertex its nearest one's.
        let mut rest_ao: Vec<f32> = vec![0.0; rest.vertices.len()];
        par_chunks(&mut rest_ao, 2048, |chunk, out| {
            for (k, a) in out.iter_mut().enumerate() {
                let v = &rest.vertices[chunk * 2048 + k];
                *a = occlusion(whole, Vec3::from(v.pos), Vec3::from(v.normal), ao_step);
            }
        });
        let nearest = crate::rig::NearestVertex::new(rest, cell * 2.0);
        let mut ao: Vec<f32> = vec![1.0; mesh.vertices.len()];
        par_chunks(&mut ao, 4096, |chunk, out| {
            for (k, a) in out.iter_mut().enumerate() {
                let p = Vec3::from(mesh.vertices[chunk * 4096 + k].pos);
                if let Some(i) = nearest.index(p) {
                    *a = rest_ao[i];
                }
            }
        });
        High { mesh, bvh, ao, reach: cell * 6.0 }
    }

    /// Without ambient occlusion (displacement maps), reaching `reach`.
    fn bare(mesh: &'a Mesh, reach: f32) -> High<'a> {
        let corners: Vec<[Vec3; 3]> = mesh.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| Vec3::from(mesh.vertices[t[k] as usize].pos))).collect();
        High { mesh, bvh: crate::film::bvh::Bvh::new(&corners), ao: Vec::new(), reach }
    }

    /// The fine surface nearest `pos` along the simple mesh's normal (out
    /// or in): (point, normal, ambient occlusion).
    fn find(&self, pos: Vec3, nrm: Vec3) -> Option<(Vec3, Vec3, f32)> {
        let none = |_: &crate::film::bvh::Hit| false;
        let out = self.bvh.hit(pos, nrm, 0.0, self.reach, &none);
        let inside = self.bvh.hit(pos, -nrm, 0.0, out.map_or(self.reach, |h| h.t), &none);
        let h = match (out, inside) {
            (Some(a), Some(b)) => if b.t < a.t { b } else { a },
            (a, b) => a.or(b)?,
        };
        let t = &self.mesh.indices[h.tri as usize * 3..h.tri as usize * 3 + 3];
        let w = [1.0 - h.u - h.v, h.u, h.v];
        let v = |k: usize| &self.mesh.vertices[t[k] as usize];
        let p: Vec3 = (0..3).map(|k| Vec3::from(v(k).pos) * w[k]).sum();
        let n: Vec3 = (0..3).map(|k| Vec3::from(v(k).normal) * w[k]).sum();
        let ao: f32 = (0..3).map(|k| self.ao.get(t[k] as usize).copied().unwrap_or(1.0) * w[k]).sum();
        let n = n.normalize_or(nrm);
        // The far side of a thin part faces away: not this surface.
        (n.dot(nrm) > 0.0).then_some((p, n, ao))
    }
}

/// Ambient occlusion: how much clay (any part) sits close above a point.
fn occlusion(whole: &Field, p: Vec3, n: Vec3, ao_step: f32) -> f32 {
    let wl = whole.near(p - Vec3::splat(ao_step * 6.0), p + Vec3::splat(ao_step * 6.0));
    let mut occ = 0.0;
    let mut weight = 0.5;
    for i in 1..=5 {
        let h = ao_step * i as f32;
        occ += weight * (h - whole.sample(p + n * h, &wl).0).max(0.0) / h;
        weight *= 0.5;
    }
    (1.0 - occ).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Default)]
struct Sample {
    color: Vec3,
    normal: Vec3,
    ao: f32,
    rough: f32,
    metal: f32,
    /// Where on the clay surface, and its palette entry.
    at: Vec3,
    material: u32,
}

/// The clay surface near `pos` (moved onto it along the field gradient) and
/// what it looks like there.
#[allow(clippy::too_many_arguments)]
fn sample(clay: &Clay, field: &Field, whole: &Field, palette: &[Material], pos: Vec3, nrm: Vec3, cell: f32, e: f32, ao_step: f32, wear: &Weather) -> Sample {
    let reach = Vec3::splat(cell * 3.0);
    let list = field.near(pos - reach, pos + reach);
    let d = |p: Vec3| field.sample(p, &list).0;
    let grad = |p: Vec3| Vec3::new(d(p + Vec3::X * e) - d(p - Vec3::X * e), d(p + Vec3::Y * e) - d(p - Vec3::Y * e), d(p + Vec3::Z * e) - d(p - Vec3::Z * e));
    let mut p = pos;
    for _ in 0..4 {
        let g = grad(p).normalize_or(nrm);
        let dist = d(p);
        if dist.abs() < 1e-6 {
            break;
        }
        p -= g * dist;
    }
    // A wild jump means the field was flat or another surface was nearer.
    if (p - pos).length() > cell * 2.0 {
        p = pos;
    }
    let hn = grad(p).normalize_or(nrm);
    let hn = if hn.dot(nrm) < 0.0 { nrm } else { hn };
    shade(clay, field, palette, p, hn, occlusion(whole, p, hn, ao_step), cell, wear)
}

/// Weathering for a bake: each rest-mesh place's edges and creases, the
/// palette with its blotch scale set, and the band width (all measured
/// once, not per texel).
struct Weather<'a> {
    at: &'a (dyn Fn(Vec3) -> Option<crate::weather::Wear> + Sync),
    scaled: Vec<Material>,
    width: f32,
    /// A human face on the clay (`face.rs`), and one texel's world size.
    face: Option<crate::face::Face>,
    texel: f32,
}

/// What the clay looks like at surface point `p` with normal `hn` and
/// ambient occlusion `ao`.
#[allow(clippy::too_many_arguments)]
fn shade(clay: &Clay, field: &Field, palette: &[Material], p: Vec3, hn: Vec3, ao: f32, cell: f32, wear: &Weather) -> Sample {
    let reach = Vec3::splat(cell * 3.0);
    let list = field.near(p - reach, p + reach);
    // Color and material come from the blob surface under any sculpting.
    let off = clay.detail.as_ref().map_or(0.0, |g| g.grid().offset(p));
    let (_, base, mat) = field.base_full(p + hn * off, &list, true);
    let fallback = Material::default();
    let m = palette.get(mat as usize).unwrap_or(&fallback);
    let col = m.painted_color(base, clay.paint.as_ref(), p);
    let (normal, color, rough) = match wear.face.as_ref().filter(|_| m.subsurface > 0.0 && m.transmission <= 0.0) {
        // Face skin, averaged over the texel so pores do not alias; the
        // oily coat goes into the roughness (game materials have none).
        // Below the face (down the neck) the plain skin detail shows.
        Some(f) => {
            let keep = f.keep(p);
            let (hn, col, rough) = if keep < 1.0 {
                let (sn, sc, sr) = m.skin_detail(p, hn, col, m.roughness);
                (hn.lerp(sn, 1.0 - keep).normalize_or(hn), col.lerp(sc, 1.0 - keep), m.roughness + (sr - m.roughness) * (1.0 - keep))
            } else {
                (hn, col, m.roughness)
            };
            let (t, b) = hn.any_orthonormal_pair();
            let mut acc = (Vec3::ZERO, Vec3::ZERO, 0.0);
            for (du, dv) in [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)] {
                let look = f.shade(p + (t * du + b * dv) * wear.texel, hn, col, rough);
                acc = (acc.0 + look.n, acc.1 + look.color * 0.25, acc.2 + look.rough * (1.0 - 0.4 * look.coat) * 0.25);
            }
            (acc.0.normalize_or(hn), acc.1, acc.2)
        }
        None => m.skin_detail(p, hn, col, m.roughness),
    };
    let (color, rough) = match (wear.at)(p).filter(|_| m.weathered()).zip(wear.scaled.get(mat as usize)) {
        Some((w, scaled)) => crate::weather::apply(scaled, p, hn, color, rough, w, wear.width),
        None => (color, rough),
    };
    Sample { color, normal, ao, rough, metal: m.metallic, at: p, material: mat as u32 }
}

/// Average world size of one texel over the mesh.
fn texel_size(mesh: &Mesh, size: u32) -> f32 {
    let (mut world, mut uv) = (0.0f64, 0.0f64);
    for t in mesh.indices.chunks_exact(3) {
        let v = |i: u32| &mesh.vertices[i as usize];
        let (a, b, c) = (Vec3::from(v(t[0]).pos), Vec3::from(v(t[1]).pos), Vec3::from(v(t[2]).pos));
        world += ((b - a).cross(c - a).length() * 0.5) as f64;
        let (p, q, r) = (Vec2::from(v(t[0]).uv), Vec2::from(v(t[1]).uv), Vec2::from(v(t[2]).uv));
        uv += ((q - p).perp_dot(r - p).abs() * 0.5) as f64;
    }
    ((world / uv.max(1e-12)).sqrt() / size as f64) as f32
}

fn palette_has_weather(src: &Source) -> bool {
    src.clay.palette(src.library, src.fallback).iter().any(Material::weathered)
}

/// Which triangle covers each texel center, and where in it (barycentric
/// weights of corners 1 and 2).
pub(crate) fn raster(mesh: &Mesh, size: u32) -> Vec<Option<(u32, Vec2)>> {
    let s = size as usize;
    let mut out = vec![None; s * s];
    for (tri, t) in mesh.indices.chunks_exact(3).enumerate() {
        let uv = |i: u32| Vec2::from(mesh.vertices[i as usize].uv) * size as f32;
        let (a, b, c) = (uv(t[0]), uv(t[1]), uv(t[2]));
        let area = (b - a).perp_dot(c - a);
        if area.abs() < 1e-12 {
            continue;
        }
        let lo = a.min(b).min(c).floor().max(Vec2::ZERO);
        let hi = a.max(b).max(c).ceil().min(Vec2::splat(size as f32 - 1.0));
        for y in lo.y as usize..=hi.y as usize {
            for x in lo.x as usize..=hi.x as usize {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w1 = (p - a).perp_dot(c - a) / area;
                let w2 = (b - a).perp_dot(p - a) / area;
                if w1 >= -1e-4 && w2 >= -1e-4 && w1 + w2 <= 1.0 + 1e-4 {
                    out[y * s + x] = Some((tri as u32, Vec2::new(w1, w2)));
                }
            }
        }
    }
    out
}

/// Per-vertex tangents along +u, orthogonal to the normal; w = +1 when the
/// bitangent (normal x tangent) points up the image (toward smaller v), as
/// glTF normal maps expect.
pub(crate) fn tangents(mesh: &mut Mesh) {
    let mut tan = vec![Vec3::ZERO; mesh.vertices.len()];
    let mut up = vec![Vec3::ZERO; mesh.vertices.len()];
    for t in mesh.indices.chunks_exact(3) {
        let v = |i: u32| &mesh.vertices[i as usize];
        let (p0, p1, p2) = (Vec3::from(v(t[0]).pos), Vec3::from(v(t[1]).pos), Vec3::from(v(t[2]).pos));
        let (w0, w1, w2) = (Vec2::from(v(t[0]).uv), Vec2::from(v(t[1]).uv), Vec2::from(v(t[2]).uv));
        let (e1, e2) = (p1 - p0, p2 - p0);
        let (d1, d2) = (w1 - w0, w2 - w0);
        let r = d1.perp_dot(d2);
        if r.abs() < 1e-14 {
            continue;
        }
        let du = (e1 * d2.y - e2 * d1.y) / r;
        let dv = (e2 * d1.x - e1 * d2.x) / r;
        for &i in t {
            tan[i as usize] += du;
            up[i as usize] -= dv;
        }
    }
    for (i, v) in mesh.vertices.iter_mut().enumerate() {
        let n = Vec3::from(v.normal);
        let t = (tan[i] - n * n.dot(tan[i])).normalize_or(n.any_orthonormal_vector());
        let w = if n.cross(t).dot(up[i]) < 0.0 { -1.0 } else { 1.0 };
        v.tangent = [t.x, t.y, t.z, w];
    }
}

/// Encodes a `size` x `size` RGBA8 image as PNG (RGB unless `keep_alpha`).
pub fn png(rgba: &[u8], size: u32, keep_alpha: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, size, size);
    enc.set_color(if keep_alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let data: Vec<u8> = if keep_alpha { rgba.to_vec() } else { rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect() };
    w.write_image_data(&data).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

pub(crate) fn edges(indices: &[u32]) -> Vec<u32> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if seen.insert((a.min(b), a.max(b))) {
                out.extend_from_slice(&[a, b]);
            }
        }
    }
    out
}

/// Grows every island ring by ring until the whole image is filled: each
/// empty texel next to filled ones takes their average. Neighbors of an
/// island hold its own colors, so mip levels never mix in background.
fn fill(images: &mut [&mut Vec<u8>], filled: &mut [bool], size: usize) {
    let neighbors = |t: usize| {
        let (x, y) = ((t % size) as i32, (t / size) as i32);
        [(-1i32, 0i32), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)].into_iter().filter_map(move |(dx, dy)| {
            let (nx, ny) = (x + dx, y + dy);
            (nx >= 0 && ny >= 0 && (nx as usize) < size && (ny as usize) < size).then(|| ny as usize * size + nx as usize)
        })
    };
    let mut queued = filled.to_vec();
    let mut ring: Vec<usize> = Vec::new();
    for t in 0..filled.len() {
        if !filled[t] && neighbors(t).any(|n| filled[n]) {
            ring.push(t);
            queued[t] = true;
        }
    }
    while !ring.is_empty() {
        for img in images.iter_mut() {
            let values: Vec<[u8; 4]> = ring.iter().map(|&t| {
                let (mut sum, mut count) = ([0u32; 4], 0u32);
                for n in neighbors(t).filter(|&n| filled[n]) {
                    for ch in 0..4 {
                        sum[ch] += img[n * 4 + ch] as u32;
                    }
                    count += 1;
                }
                sum.map(|v| (v / count.max(1)) as u8)
            }).collect();
            for (&t, v) in ring.iter().zip(values) {
                img[t * 4..t * 4 + 4].copy_from_slice(&v);
            }
        }
        for &t in &ring {
            filled[t] = true;
        }
        let mut next = Vec::new();
        for &t in &ring {
            for n in neighbors(t) {
                if !queued[n] {
                    queued[n] = true;
                    next.push(n);
                }
            }
        }
        ring = next;
    }
}

/// Which triangle covers each texel center (`u32::MAX`: none).
fn raster_ids(mesh: &Mesh, size: u32) -> Vec<u32> {
    let s = size as usize;
    let mut out = vec![u32::MAX; s * s];
    for (tri, t) in mesh.indices.chunks_exact(3).enumerate() {
        let uv = |i: u32| Vec2::from(mesh.vertices[i as usize].uv) * size as f32;
        let (a, b, c) = (uv(t[0]), uv(t[1]), uv(t[2]));
        let area = (b - a).perp_dot(c - a);
        if area.abs() < 1e-12 {
            continue;
        }
        let lo = a.min(b).min(c).floor().max(Vec2::ZERO);
        let hi = a.max(b).max(c).ceil().min(Vec2::splat(size as f32 - 1.0));
        for y in lo.y as usize..=hi.y as usize {
            for x in lo.x as usize..=hi.x as usize {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w1 = (p - a).perp_dot(c - a) / area;
                let w2 = (b - a).perp_dot(p - a) / area;
                if w1 >= -1e-4 && w2 >= -1e-4 && w1 + w2 <= 1.0 + 1e-4 {
                    out[y * s + x] = tri as u32;
                }
            }
        }
    }
    out
}

/// Barycentric weights of corners b and c at texel `t`'s center.
fn barycentric(a: [f32; 2], b: [f32; 2], c: [f32; 2], t: usize, size: u32) -> Option<Vec2> {
    let s = size as f32;
    let (a, b, c) = (Vec2::from(a) * s, Vec2::from(b) * s, Vec2::from(c) * s);
    let p = Vec2::new((t % size as usize) as f32 + 0.5, (t / size as usize) as f32 + 0.5);
    let area = (b - a).perp_dot(c - a);
    (area.abs() >= 1e-12).then(|| Vec2::new((p - a).perp_dot(c - a) / area, (b - a).perp_dot(p - a) / area))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skin_bake_samples_the_shared_micro_detail() {
        let clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0,0,0], "radius": 0.1, "material": "skin", "color": [0.8,0.6,0.5] }], "resolution": 48
        })).unwrap();
        let cell = clay.cell_size().unwrap();
        let field = Field::new(&clay, cell);
        let m = Material::preset("skin").unwrap();
        let enabled = vec![m.clone()];
        let disabled = vec![Material { pores: 0.0, mottling: 0.0, ..m }];
        let mut differences = [false; 3];
        for i in 0..16 {
            let p = Vec3::new(i as f32 * 0.0002, 0.0, 0.1);
            let none = Weather { at: &|_| None, scaled: Vec::new(), width: 0.01, face: None, texel: 0.001 };
            let a = sample(&clay, &field, &field, &enabled, p, Vec3::Z, cell, cell * 0.25, cell, &none);
            let b = sample(&clay, &field, &field, &disabled, p, Vec3::Z, cell, cell * 0.25, cell, &none);
            differences[0] |= a.normal.distance(b.normal) > 0.001;
            differences[1] |= a.color.distance(b.color) > 0.001;
            differences[2] |= (a.rough - b.rough).abs() > 0.001;
            assert_eq!(a.metal, b.metal);
            assert_eq!(a.ao, b.ao);
        }
        assert!(differences.iter().all(|v| *v));
    }

    /// Smart layers and decals reach the game textures: worn iron bakes
    /// both bare metal and rust (metal 1 and 0 in the ORM map) with varied
    /// color, plain clay bakes flat; a decal shows in the color map.
    #[test]
    fn bakes_contain_smart_layers_and_decals() {
        let bake_of = |material: &str, decal: bool| {
            let mut clay = crate::smart_check::test_piece(material);
            clay.resolution = 96;
            if decal {
                clay.decals.push(serde_json::from_value(serde_json::json!({ "shape": "splat", "center": [-0.2, 0.2, 0.06], "normal": [0, 0, 1], "size": 0.25 })).unwrap());
            }
            let rest = clay.build_mesh_with(&[], material);
            let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &[], fallback: material, fine: None };
            let (mesh, _) = bake(&src, Settings { triangles: 4000, texture: 512, ..Default::default() }, &Default::default()).unwrap();
            mesh.textures.clone().unwrap()
        };
        let iron = bake_of("worn_iron", false);
        let metal: Vec<u8> = iron.orm.chunks_exact(4).map(|t| t[2]).collect();
        let (bare, rusty) = (metal.iter().filter(|&&m| m > 230).count(), metal.iter().filter(|&&m| m < 25).count());
        assert!(bare > 10_000 && rusty > 500, "bare {bare} rusty {rusty}");
        // Rust is reddish brown: some texels have red well above blue.
        let red = iron.color.chunks_exact(4).filter(|c| c[0] as i32 - c[2] as i32 > 30).count();
        assert!(red > 500, "{red} rusty texels");
        let clay = bake_of("clay", false);
        assert!(clay.orm.chunks_exact(4).all(|t| t[2] == 0));
        // A splat paints blood red.
        let splat = bake_of("clay", true);
        let bloody = |t: &Textures| t.color.chunks_exact(4).filter(|c| c[0] > 50 && c[1] < 20 && c[2] < 20).count();
        assert!(bloody(&splat) > 300 && bloody(&clay) == 0, "{}", bloody(&splat));
    }

    /// A detail texture puts fine relief into the baked normal map, color
    /// and roughness; the same clay without it bakes smooth.
    #[test]
    fn detail_bakes_into_the_game_textures() {
        let clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3, "color": [0.3, 0.5, 0.3], "material": "knitwear" }], "resolution": 48
        })).unwrap();
        let rest = clay.build_mesh();
        let bake_with = |detail: &str| {
            let library = vec![Material { name: "knitwear".into(), detail: Some(detail.into()), detail_scale: 0.08, detail_color: 1.0, ..Material::preset("cloth").unwrap() }];
            let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &library, fallback: "", fine: None };
            let (mesh, _) = bake(&src, Settings { triangles: 2000, texture: 512, ..Default::default() }, &Default::default()).unwrap();
            let tex = mesh.textures.clone().unwrap();
            // Texel-to-texel change inside islands: high-frequency content.
            let covered = raster(&mesh, 512);
            let (mut normal, mut color, mut rough, mut count) = (0.0, 0.0, 0.0, 0);
            for y in 0..511 {
                for x in 0..511 {
                    let (i, r, d) = (y * 512 + x, y * 512 + x + 1, (y + 1) * 512 + x);
                    if covered[i].is_none() || covered[r].is_none() || covered[d].is_none() { continue; }
                    let diff = |img: &[u8], c: usize| (img[i * 4 + c] as f32 - img[r * 4 + c] as f32).abs() + (img[i * 4 + c] as f32 - img[d * 4 + c] as f32).abs();
                    normal += diff(&tex.normal, 0) + diff(&tex.normal, 1);
                    color += diff(&tex.color, 1);
                    rough += diff(&tex.orm, 1);
                    count += 1;
                }
            }
            [normal, color, rough].map(|v| v / count as f32)
        };
        let (plain, knit) = (bake_with("none"), bake_with("knit"));
        assert!(knit[0] > plain[0] * 4.0 + 2.0, "normal {plain:?} {knit:?}");
        assert!(knit[1] > plain[1] * 3.0 + 0.5, "color {plain:?} {knit:?}");
        assert!(knit[2] > plain[2] + 0.3, "roughness {plain:?} {knit:?}");
    }

    /// Detail regions get more of the triangle budget and more texels
    /// (region_density), and the textures leave no unfilled gaps.
    #[test]
    fn regions_get_more_triangles_and_texels() {
        let mut clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 }], "resolution": 64, "detail_cell": 0.004
        })).unwrap();
        let region = crate::clay::Region { name: "front".into(), center: Vec3::new(0.0, 0.0, 0.3), radii: Vec3::splat(0.12), cell: 0.001 };
        clay.set_detail_regions(&[region]).unwrap();
        let rest = clay.build_mesh();
        let share = |density: f32| {
            let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None };
            let (mesh, _) = bake(&src, Settings { triangles: 3000, texture: 1024, region_density: density, ..Default::default() }, &Default::default()).unwrap();
            let (mut tris, mut uv, mut all_uv) = (0, 0.0, 0.0);
            for t in mesh.indices.chunks_exact(3) {
                let v = |i: u32| &mesh.vertices[i as usize];
                let c = (Vec3::from(v(t[0]).pos) + Vec3::from(v(t[1]).pos) + Vec3::from(v(t[2]).pos)) / 3.0;
                let (a, b, d) = (Vec2::from(v(t[0]).uv), Vec2::from(v(t[1]).uv), Vec2::from(v(t[2]).uv));
                let area = (b - a).perp_dot(d - a).abs() * 0.5;
                all_uv += area;
                if c.z > 0.22 {
                    tris += 1;
                    uv += area;
                }
            }
            let tex = mesh.textures.unwrap();
            assert!(tex.orm.chunks_exact(4).all(|t| t[3] == 255));
            (tris as f32 / (mesh.indices.len() / 3) as f32, uv / all_uv)
        };
        let ((t0, u0), (t1, u1)) = (share(0.0), share(1.0));
        println!("region share: triangles {t0:.3} -> {t1:.3}, texels {u0:.3} -> {u1:.3}");
        assert!(t1 > t0 * 1.5, "triangles {t0} -> {t1}");
        assert!(u1 > u0 * 2.0, "texels {u0} -> {u1}");
    }

    #[test]
    fn sphere_bakes_flat_normals_and_its_color() {
        let clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5, "color": [0.8, 0.2, 0.1], "material": "metal" }],
            "resolution": 48
        }))
        .unwrap();
        let rest = clay.build_mesh();
        let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None };
        let (mesh, _) = bake(&src, Settings { triangles: 800, texture: 512, ..Default::default() }, &Default::default()).unwrap();
        assert!(mesh.indices.len() / 3 <= 900, "{}", mesh.indices.len() / 3);
        let tex = mesh.textures.as_ref().unwrap();
        // Texels under triangles: color is the blob's, normal is close to the
        // mesh's (+Z in tangent space), metal is full.
        let mut checked = 0;
        for t in raster(&mesh, 512).iter().enumerate().filter_map(|(i, t)| t.map(|_| i)).step_by(97) {
            let c = &tex.color[t * 4..t * 4 + 3];
            assert!((c[0] as i32 - 204).abs() <= 3 && (c[1] as i32 - 51).abs() <= 3, "{c:?}");
            assert!(tex.normal[t * 4 + 2] > 200, "normal z {}", tex.normal[t * 4 + 2]);
            assert_eq!(tex.orm[t * 4 + 2], 255);
            checked += 1;
        }
        assert!(checked > 100);
        assert!(mesh.vertices.iter().all(|v| v.uv.iter().all(|c| (0.0..=1.0).contains(c)) && v.tangent[3].abs() == 1.0));
    }

    /// Stage 8b acceptance: a bumpy ball baked to 800 triangles, its
    /// displacement map written as a 16-bit PNG, read back and reapplied
    /// as geometry through `film::dice` lands within one clay cell of the
    /// clay both ways (Hausdorff), where the simple mesh alone misses the
    /// bumps by more than a cell; the vector map (EXR floats) does too.
    #[test]
    fn displacement_bake_reapplied_through_dice_matches_the_clay() {
        // 24 bumps spread evenly (a Fibonacci sphere), more than 800
        // triangles follow. (Scalar displacement needs the clay to be a
        // height field over the simple mesh: steep bumps a coarse mesh
        // cuts through are not, and `verify` then reports a larger error.)
        let mut blobs = vec![serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 })];
        for i in 0..24 {
            let y = 1.0 - (i as f32 + 0.5) / 12.0;
            let (r, phi) = ((1.0 - y * y).sqrt(), i as f32 * 2.399_963);
            let d = Vec3::new(r * phi.cos(), y, r * phi.sin()) * 0.3;
            blobs.push(serde_json::json!({ "type": "sphere", "center": d.to_array(), "radius": 0.05, "blend": 0.03 }));
        }
        let clay: Clay = serde_json::from_value(serde_json::json!({ "blobs": blobs, "resolution": 64 })).unwrap();
        let cell = clay.cell_size().unwrap();
        let rest = clay.build_mesh();
        let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None };
        let (mesh, _) = bake(&src, Settings { triangles: 800, texture: 512, ..Default::default() }, &Default::default()).unwrap();
        let t = std::time::Instant::now();
        let disp = displacement(&src, &mesh, Maps { displacement: true, vector: true, size: 512 }, &Default::default()).unwrap();
        eprintln!("displacement maps 512: {:.2} s, {} texels from the field, {} missed", t.elapsed().as_secs_f32(), disp.from_field, disp.missed);
        assert_eq!(disp.missed, 0);
        // Through the file formats an agent gets.
        let scale = disp.scale();
        let codes = Displacement::encode16(disp.height.clone().unwrap().into_iter(), scale);
        let (w, h, channels, back) = crate::outputs::read_png16(&crate::outputs::png16(512, 512, 1, &codes).unwrap()).unwrap();
        assert_eq!((w, h, channels), (512, 512, 1));
        let height = Displacement { size: 512, height: Some(back.iter().map(|c| Displacement::decode16(*c, scale)).collect()), vector: None, from_field: 0, missed: 0 };
        let path = std::env::temp_dir().join(format!("clide-vdm-{}.exr", std::process::id()));
        let v = disp.vector.clone().unwrap();
        crate::outputs::write_exr(&path, 512, 512, vec![("R", v.iter().map(|c| c[0]).collect()), ("G", v.iter().map(|c| c[1]).collect()), ("B", v.iter().map(|c| c[2]).collect())]).unwrap();
        let (_, _, read) = crate::outputs::read_exr(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let get = |n: &str| &read.iter().find(|(k, _)| k == n).unwrap().1;
        let vector = Displacement { size: 512, height: None, vector: Some((0..512 * 512).map(|i| [get("R")[i], get("G")[i], get("B")[i]]).collect()), from_field: 0, missed: 0 };
        let field = Field::new(&clay, cell);
        let all: Vec<u32> = (0..field.prim_count() as u32).collect();
        // Both ways: the mesh's points to the clay (|field|), and the clay's
        // (its rest mesh) to the nearest point of the mesh.
        let hausdorff = |m: &Mesh| {
            let to_clay = m.vertices.iter().map(|v| field.sample(Vec3::from(v.pos), &all).0.abs()).fold(0.0f32, f32::max);
            let near = crate::rig::NearestVertex::new(m, cell);
            let to_mesh = rest.vertices.iter().map(|v| { let p = Vec3::from(v.pos); Vec3::from(near.vertex(p).unwrap().pos).distance(p) }).fold(0.0f32, f32::max);
            (to_clay / cell, to_mesh / cell)
        };
        let plain = crate::film::dice::apply_baked(&mesh, &Displacement { size: 1, height: Some(vec![0.0]), vector: None, from_field: 0, missed: 0 }, cell * 0.5, 4_000_000).unwrap();
        let (a, b) = hausdorff(&plain);
        eprintln!("cell {cell:.4}; simple mesh ({} triangles) alone: {a:.2} / {b:.2} cells (to clay / from clay)", mesh.indices.len() / 3);
        assert!(a.max(b) > 1.4, "the bumps should need the map: {a} {b}");
        for (name, map) in [("height (png16)", &height), ("vector (exr)", &vector)] {
            let diced = crate::film::dice::apply_baked(&mesh, map, cell * 0.5, 4_000_000).unwrap();
            let (a, b) = hausdorff(&diced);
            eprintln!("{name} map reapplied ({} triangles): {a:.2} / {b:.2} cells", diced.indices.len() / 3);
            assert!(a <= 1.0 && b <= 1.0, "{name}: Hausdorff {a} / {b} cells");
        }
    }

}
