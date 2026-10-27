//! UVs of a Poly (docs/QUALITY_PLAN.md Stage 3c; the critic notes moved
//! `poly uv` here from 4b): one tile, per cage corner (face-varying, so
//! seams are corners of one point with different UVs).
//!
//! The cage's faces go to xatlas as polygons (`face_vertex_count`), so a
//! face is never split between charts: chart borders run along cage
//! edges. Levels' UVs are face-varying linear: a child quad's corners
//! take its parent corner's UV, the middles of the parent's two sides at
//! that corner and the mean of the parent's corners (the corner order of
//! `Topology::refine`), so a seam stays a seam and UVs on either side of
//! a shared edge stay equal.

use std::collections::BTreeMap;

use glam::Vec2;

use super::Poly;

/// Most cage faces `poly uv` unwraps (xatlas's time grows faster than
/// the faces).
pub const MAX_UV_FACES: usize = 1_000_000;
/// Atlas sizes (pixels across) the packing aims at.
pub const SIZES: [u32; 5] = [512, 1024, 2048, 4096, 8192];
pub const DEFAULT_SIZE: u32 = 2048;
/// Most pixels kept clear around each chart.
pub const MAX_PADDING: u32 = 32;
pub const DEFAULT_PADDING: u32 = 4;
/// UVs a file may hold lie within this of the tile.
pub const MAX_UV: f32 = 16.0;

/// An unwrap: a UV per cage corner, the chart count, the share of the
/// tile the charts cover and the faces made whole.
pub struct Unwrapped {
    pub uv: Vec<[f32; 2]>,
    pub charts: usize,
    pub utilization: f32,
    /// Faces whose triangles xatlas put in different charts, made whole.
    pub repaired: usize,
    /// UDIM: the tiles used (1001 first) and the texels per unit the
    /// charts were packed at.
    pub tiles: Vec<u16>,
    pub texel_density: Option<f32>,
    /// Each cage face's chart.
    pub face_chart: Vec<i32>,
}

/// Most UDIM tiles a layout may use (1001-1100).
pub const MAX_TILES: u32 = crate::poly::texture::MAX_TILES;
/// Share of the tiles' texels a default UDIM density aims to fill.
const FILL: f32 = 0.55;
/// A chart packed below this share of the density asked for was shrunk
/// to fit a tile: refused.
const DENSITY_KEPT: f32 = 0.95;
/// Tries at a lower default density when the charts take more tiles.
const DENSITY_TRIES: usize = 12;

/// A UDIM layout's request: at most `tiles` tiles, each `size` texels
/// across, at `texel_density` texels per unit (None: the density that
/// fills about `tiles` tiles).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Udim {
    pub tiles: u32,
    pub texel_density: Option<f32>,
}

/// Each chart's texel density (texels per unit: the square root of its
/// UV area in texels of a `size` tile over its 3D area) and its tiles
/// (a chart on one tile has one), by chart index.
pub fn chart_stats(poly: &Poly, uv: &[[f32; 2]], face_chart: &[i32], size: u32) -> BTreeMap<i32, (f32, Vec<u16>)> {
    let p = poly.cage.to_vec();
    let (starts, corners) = poly.cage_faces();
    let mut acc: BTreeMap<i32, (f64, f64, Vec<u16>)> = BTreeMap::new();
    for (f, w) in starts.windows(2).enumerate() {
        let (a, b) = (w[0] as usize, w[1] as usize);
        let area3 = super::subdiv::newell(&p, &corners[a..b]).length() as f64 * 0.5;
        let mut area2 = 0.0f64;
        for j in 1..b - a - 1 {
            let (u0, u1, u2) = (Vec2::from(uv[a]), Vec2::from(uv[a + j]), Vec2::from(uv[a + j + 1]));
            area2 += ((u1 - u0).perp_dot(u2 - u0) * 0.5).abs() as f64;
        }
        let e = acc.entry(face_chart.get(f).copied().unwrap_or(-1)).or_default();
        e.0 += area2 * (size as f64).powi(2);
        e.1 += area3;
        for c in a..b {
            if let Some(t) = crate::poly::texture::tile_of(uv[c]) {
                if !e.2.contains(&t) {
                    e.2.push(t);
                }
            }
        }
    }
    acc.into_iter().map(|(k, (t, a, tiles))| (k, (if a > 0.0 { (t / a).sqrt() as f32 } else { 0.0 }, tiles))).collect()
}

/// The line (an OBJ comment, a USDA primvar's `doc`) that marks a file's
/// UVs as a UDIM layout flipped tile by tile (`v_out`). Only a file that
/// says so (or an import that asks, `udim: true`) is read that way: an
/// ordinary file's UVs outside 0-1 (tiling) are plain 1 - v.
pub const UDIM_MARK: &str = "clide3d uv: udim";

/// UVs that leave the first tile (any coordinate outside 0-1): a UDIM
/// layout, whose files flip v tile by tile (`v_out`).
pub fn spans_tiles(uv: &[[f32; 2]]) -> bool {
    uv.iter().any(|t| t.iter().any(|x| !(0.0..=1.0).contains(x)))
}

/// A UV's v as files write it, v up (OBJ's vt, USD's st), in f64 so it
/// reads back bit for bit (`v_in`). One tile: 1 - v. A UDIM layout flips
/// each tile in place, so tile 1011 stays above 1001: v in [t, t + 1)
/// (top-left within its tile) is written as 2t + 1 - v, in (t, t + 1].
pub fn v_out(v: f32, udim: bool) -> f64 {
    if udim {
        2.0 * v.floor() as f64 + 1.0 - v as f64
    } else {
        1.0 - v as f64
    }
}

/// `v_out` undone (`udim`: the file is marked as a UDIM layout).
pub fn v_in(v: f64, udim: bool) -> f32 {
    if udim {
        // v in (t, t + 1] is tile row t, below the first row too (an
        // exact inverse of `v_out` for every row).
        let t = v.ceil() - 1.0;
        (2.0 * t + 1.0 - v) as f32
    } else {
        (1.0 - v) as f32
    }
}

/// Total 3D area of the cage's faces.
pub fn cage_area(poly: &Poly) -> f64 {
    let p = poly.cage.to_vec();
    let (starts, corners) = poly.cage_faces();
    starts.windows(2).map(|w| super::subdiv::newell(&p, &corners[w[0] as usize..w[1] as usize]).length() as f64 * 0.5).sum()
}

/// `unwrap` spread over UDIM tiles (see `Udim`): xatlas packs the charts
/// at the density into atlases of `size` texels, atlas i becoming tile
/// 1001 + i (u = i % 10, v = i / 10), so each chart stays on one tile.
/// Refused when they take more than `tiles` tiles at a given density; a
/// default density is lowered until they fit.
pub fn unwrap_udim(poly: &Poly, size: u32, padding: u32, udim: Udim) -> Result<Unwrapped, String> {
    if udim.tiles == 0 || udim.tiles > MAX_TILES {
        return Err(format!("tiles must be 1 to {MAX_TILES}"));
    }
    let area = cage_area(poly);
    if !(area > 0.0) {
        return Err("the cage has no area to unwrap".into());
    }
    // A layout keeps its density on every chart (xatlas shrinks a chart
    // larger than a tile instead of refusing it) and its tile count.
    let check = |made: &Unwrapped, d: f32| -> Result<(), String> {
        if made.tiles.len() > udim.tiles as usize {
            return Err(format!("at {d} texels per unit the charts take {} tiles of {size}, more than tiles ({}); lower texel_density or allow more tiles", made.tiles.len(), udim.tiles));
        }
        let small = chart_stats(poly, &made.uv, &made.face_chart, size).values().map(|s| s.0).fold(f32::MAX, f32::min);
        if small < DENSITY_KEPT * d {
            return Err(format!("at {d} texels per unit a chart is larger than one {size}-texel tile (it was packed at {small:.1}); lower texel_density or a larger size"));
        }
        Ok(())
    };
    match udim.texel_density {
        Some(d) => {
            let made = unwrap_at(poly, size, padding, Some(d))?;
            check(&made, d)?;
            Ok(made)
        }
        None => {
            let mut d = ((udim.tiles as f64 * (size as f64).powi(2) * FILL as f64 / area).sqrt()) as f32;
            for _ in 0..DENSITY_TRIES {
                let made = unwrap_at(poly, size, padding, Some(d))?;
                if check(&made, d).is_ok() {
                    return Ok(made);
                }
                d *= 0.9;
            }
            Err(format!("the charts did not fit {} tiles of {size}; allow more tiles or a larger size", udim.tiles))
        }
    }
}

/// Unwraps the cage into one tile of `size` pixels with `padding` pixels
/// around each chart (bounded by the caller).
///
/// The bundled xatlas's polygon input (`face_vertex_count`) leaves most
/// of its output index array unwritten, so faces go in as fans of
/// triangles (its well-trodden path, as `bake` uses). A face whose
/// triangles land in different charts is then made whole: its later
/// triangles are unfolded, across the side they share, into its first
/// triangle's chart, keeping their shape; and the corrected UVs are
/// packed again as input UVs (`use_input_mesh_uvs`, as `bake` repacks),
/// so the grown charts never overlap. Faces with a short side or a thin
/// triangle are refused first (xatlas drops those).
pub fn unwrap(poly: &Poly, size: u32, padding: u32) -> Result<Unwrapped, String> {
    unwrap_at(poly, size, padding, None)
}

/// `unwrap` (density None: one tile) or a UDIM layout at `density`
/// texels per unit (atlases of `size` become tiles).
fn unwrap_at(poly: &Poly, size: u32, padding: u32, density: Option<f32>) -> Result<Unwrapped, String> {
    if let Some(d) = density
        && !(d.is_finite() && d > 0.0)
    {
        return Err("texel_density must be a positive number of texels per unit".into());
    }
    if poly.faces() > MAX_UV_FACES {
        return Err(format!("the cage has {} faces; poly uv unwraps at most {MAX_UV_FACES}", poly.faces()));
    }
    let (starts, corners) = poly.cage_faces();
    let points = poly.cage.to_vec();
    let scale = points.iter().fold(0.0f32, |m, p| m.max(p.abs().max_element())).max(1e-6);
    let tiny = (scale * 1e-6).powi(2).max(1e-12);
    // Fans from each face's first corner: triangle t is (face, j) with
    // corners (0, j, j + 1) of its face.
    let mut tris: Vec<u32> = Vec::with_capacity(3 * corners.len());
    for (f, w) in starts.windows(2).enumerate() {
        let face = &corners[w[0] as usize..w[1] as usize];
        for j in 1..face.len() - 1 {
            let (a, b, c) = (points[face[0] as usize], points[face[j] as usize], points[face[j + 1] as usize]);
            if (b - a).cross(c - a).length() * 0.5 <= tiny || a.distance_squared(b) <= tiny || b.distance_squared(c) <= tiny {
                return Err(format!("face {f} is degenerate (corners in one place or in a line); poly uv needs faces with area"));
            }
            tris.extend_from_slice(&[face[0], face[j], face[j + 1]]);
        }
    }
    let flat: Vec<f32> = points.iter().flat_map(|p| p.to_array()).collect();
    let pack = xatlas_rs_v2::PackOptions { resolution: size, padding, bilinear: true, texels_per_unit: density.unwrap_or(0.0), ..Default::default() };
    let decl = xatlas_rs_v2::MeshDecl {
        vertex_position_data: xatlas_rs_v2::MeshData::Contiguous(&flat),
        index_data: Some(xatlas_rs_v2::IndexData::U32(&tris)),
        face_count: (tris.len() / 3) as u32,
        ..Default::default()
    };
    let mut atlas = xatlas_rs_v2::Xatlas::new();
    atlas.add_mesh(&decl).map_err(|e| format!("UV unwrap failed: {e:?}"))?;
    atlas.generate(&xatlas_rs_v2::ChartOptions::default(), &pack);
    // Per triangle corner: (chart, atlas, uv in the atlas's pixels).
    let first = triangle_uvs(&atlas, &tris)?;
    // Each face in its first triangle's chart (and atlas), unfolding the
    // others.
    let mut uv = vec![Vec2::ZERO; corners.len()];
    let mut atlas_of = vec![0i32; corners.len()];
    let mut chart_of = vec![0i32; corners.len()];
    let mut repaired = 0usize;
    let mut t = 0usize;
    for w in starts.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        let (chart, at) = (first[3 * t].0, first[3 * t].1);
        atlas_of[a..b].fill(at);
        chart_of[a..b].fill(chart);
        let mut split = false;
        for j in 1..b - a - 1 {
            let k = 3 * (t + j - 1);
            if j == 1 {
                (uv[a], uv[a + 1], uv[a + 2]) = (first[k].2, first[k + 1].2, first[k + 2].2);
                continue;
            }
            if first[k].0 == chart && first[k].2 == uv[a] && first[k + 1].2 == uv[a + j] {
                uv[a + j + 1] = first[k + 2].2;
                continue;
            }
            split = true;
            // Unfold corner j + 1 across the side (0, j), on the side the
            // face's first triangle turns.
            let (p0, pj, pq) = (points[corners[a] as usize], points[corners[a + j] as usize], points[corners[a + j + 1] as usize]);
            let e = pj - p0;
            let along = (pq - p0).dot(e) / e.length_squared();
            let h = ((pq - p0) - e * along).length() / e.length();
            let turn = (uv[a + 1] - uv[a]).perp_dot(uv[a + 2] - uv[a]).signum();
            let d = uv[a + j] - uv[a];
            uv[a + j + 1] = uv[a] + d * along + d.perp() * (h * turn);
        }
        repaired += split as usize;
        t += b - a - 2;
    }
    let (count, px, utilization, charts) = if repaired == 0 {
        stats(&atlas)
    } else {
        // Packed again with these UVs as the charts (each face's corners
        // keyed by its atlas too: two atlases' pixels are not one place).
        let mut keys: Vec<(u32, i32, [u32; 2], usize)> = (0..corners.len()).map(|c| (corners[c], atlas_of[c], [uv[c].x.to_bits(), uv[c].y.to_bits()], c)).collect();
        keys.sort_unstable();
        let mut vert_of = vec![0u32; corners.len()];
        let (mut pos, mut uvs) = (Vec::new(), Vec::new());
        for (i, k) in keys.iter().enumerate() {
            if i == 0 || (k.0, k.1, k.2) != (keys[i - 1].0, keys[i - 1].1, keys[i - 1].2) {
                pos.extend(points[k.0 as usize].to_array());
                // Atlases apart in the input UVs so their charts stay apart.
                uvs.extend([uv[k.3].x + 2.0 * size as f32 * atlas_of[k.3] as f32, uv[k.3].y]);
            }
            vert_of[k.3] = (pos.len() / 3 - 1) as u32;
        }
        let mut tri2 = Vec::with_capacity(tris.len());
        for w in starts.windows(2) {
            let a = w[0] as usize;
            for j in 1..(w[1] - w[0]) as usize - 1 {
                tri2.extend_from_slice(&[vert_of[a], vert_of[a + j], vert_of[a + j + 1]]);
            }
        }
        let decl = xatlas_rs_v2::MeshDecl {
            vertex_position_data: xatlas_rs_v2::MeshData::Contiguous(&pos),
            vertex_uv_data: Some(xatlas_rs_v2::MeshData::WithStride { data: bytemuck::cast_slice(&uvs), stride: 8 }),
            index_data: Some(xatlas_rs_v2::IndexData::U32(&tri2)),
            face_count: (tri2.len() / 3) as u32,
            ..Default::default()
        };
        let mut again_atlas = xatlas_rs_v2::Xatlas::new();
        again_atlas.add_mesh(&decl).map_err(|e| format!("UV repack failed: {e:?}"))?;
        again_atlas.generate(&xatlas_rs_v2::ChartOptions { use_input_mesh_uvs: true, ..Default::default() }, &pack);
        let again = triangle_uvs(&again_atlas, &tri2)?;
        let mut t = 0usize;
        for w in starts.windows(2) {
            let a = w[0] as usize;
            for j in 1..(w[1] - w[0]) as usize - 1 {
                let k = 3 * (t + j - 1);
                (uv[a], uv[a + j], uv[a + j + 1]) = (again[k].2, again[k + 1].2, again[k + 2].2);
                atlas_of[a..w[1] as usize].fill(again[k].1);
                chart_of[a..w[1] as usize].fill(again[k].0);
            }
            t += (w[1] - w[0]) as usize - 2;
        }
        stats(&again_atlas)
    };
    let (out, tiles) = match density {
        None => {
            if count != 1 {
                return Err(format!("the charts did not fit one {size}-pixel tile; use a larger size or a smaller padding"));
            }
            (uv.iter().map(|u| [u.x / px, u.y / px]).collect::<Vec<[f32; 2]>>(), Vec::new())
        }
        Some(_) => {
            if count == 0 || count > MAX_TILES {
                return Err(format!("the charts took {count} tiles; at most {MAX_TILES} (lower texel_density)"));
            }
            // Atlas i is tile 1001 + i: u by i % 10, v by i / 10.
            let out: Vec<[f32; 2]> = uv.iter().zip(&atlas_of).map(|(u, &i)| [u.x / px + (i % 10) as f32, u.y / px + (i / 10) as f32]).collect();
            let mut tiles: Vec<u16> = atlas_of.iter().map(|&i| crate::poly::texture::FIRST_TILE + i as u16).collect();
            tiles.sort_unstable();
            tiles.dedup();
            (out, tiles)
        }
    };
    if out.iter().flatten().any(|x| !x.is_finite()) {
        return Err("UV unwrap gave a corner no UV".into());
    }
    let face_chart = starts.windows(2).map(|w| chart_of[w[0] as usize]).collect();
    Ok(Unwrapped { uv: out, charts, utilization, repaired, tiles, texel_density: density, face_chart })
}

/// An atlas's (atlas count, pixels across, share of the tile used,
/// charts).
fn stats(atlas: &xatlas_rs_v2::Xatlas) -> (u32, f32, f32, usize) {
    let (w, h) = (atlas.width().max(1) as f32, atlas.height().max(1) as f32);
    let px = w.max(h);
    let used = atlas.utilization().and_then(|u| u.first().copied()).unwrap_or(0.0) * (w * h) / (px * px);
    (atlas.atlas_count(), px, used, atlas.chart_count() as usize)
}

/// Per corner of each input triangle (in input order): its output
/// vertex's chart, atlas and UV (that atlas's pixels), checked against
/// the input.
fn triangle_uvs(atlas: &xatlas_rs_v2::Xatlas, tris: &[u32]) -> Result<Vec<(i32, i32, Vec2)>, String> {
    let meshes = atlas.meshes();
    let m = meshes.first().ok_or("UV unwrap failed")?;
    if m.index_array.len() != tris.len() {
        return Err("UV unwrap dropped a triangle".into());
    }
    m.index_array
        .iter()
        .zip(tris)
        .map(|(&i, &want)| {
            let v = m.vertex_array.get(i as usize).ok_or("UV unwrap returned a vertex out of range")?;
            if v.xref != want || v.chart_index < 0 || v.atlas_index < 0 {
                return Err("UV unwrap moved a triangle".to_string());
            }
            Ok((v.chart_index, v.atlas_index, Vec2::from(v.uv)))
        })
        .collect()
}

/// UVs per corner of the next level from a level's (`face_start` its
/// faces' offsets: the cage's at level 0, every 4 after).
pub fn refine(face_start: &[u32], uv: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(uv.len() * 4);
    let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    for w in face_start.windows(2) {
        let face = &uv[w[0] as usize..w[1] as usize];
        let n = face.len();
        let mean = face.iter().fold([0.0f32; 2], |m, u| [m[0] + u[0], m[1] + u[1]]).map(|x| x / n as f32);
        for j in 0..n {
            let (prev, here, next) = (face[(j + n - 1) % n], face[j], face[(j + 1) % n]);
            out.extend_from_slice(&[here, mid(here, next), mean, mid(prev, here)]);
        }
    }
    out
}

/// The Poly's UVs at `level` (per corner of that level), None without UVs.
pub fn at_level(poly: &Poly, level: u8) -> Option<Vec<[f32; 2]>> {
    let mut uv = poly.uv.as_ref()?.to_vec();
    let mut starts = poly.face_start.to_vec();
    for _ in 0..level {
        uv = refine(&starts, &uv);
        starts = (0..=uv.len() as u32 / 4).map(|i| 4 * i).collect();
    }
    Some(uv)
}

/// Edges whose two faces give either end a different UV (UV seams), of
/// a level's topology and corner UVs.
pub fn seams(t: &super::topology::Topology, uv: &[[f32; 2]]) -> usize {
    // Per edge: the UVs (at its first point, at its second) the first
    // face to name it gives; a second face must give the same.
    let mut first: Vec<Option<([f32; 2], [f32; 2])>> = vec![None; t.edges.len()];
    let mut seam = vec![false; t.edges.len()];
    for f in 0..t.faces() {
        let (s, e) = (t.face_start[f] as usize, t.face_start[f + 1] as usize);
        for c in s..e {
            let next = if c + 1 == e { s } else { c + 1 };
            let edge = t.corner_edge[c] as usize;
            let (ua, ub) = if t.edges[edge][0] == t.corners[c] { (uv[c], uv[next]) } else { (uv[next], uv[c]) };
            match first[edge] {
                None => first[edge] = Some((ua, ub)),
                Some(k) => seam[edge] |= k != (ua, ub),
            }
        }
    }
    seam.iter().filter(|&&s| s).count()
}

/// UV area over 3D area per face, as a spread (max / min over faces with
/// area): how evenly the unwrap spends texels.
pub fn stretch_spread(poly: &Poly, uv: &[[f32; 2]]) -> f32 {
    let p = poly.cage.to_vec();
    let (starts, corners) = poly.cage_faces();
    let (mut lo, mut hi) = (f32::MAX, 0.0f32);
    for w in starts.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        let area3 = super::subdiv::newell(&p, &corners[a..b]).length() * 0.5;
        let mut area2 = 0.0f32;
        for j in a..b {
            let (u, v) = (Vec2::from(uv[j]), Vec2::from(uv[if j + 1 == b { a } else { j + 1 }]));
            area2 += u.perp_dot(v);
        }
        if area3 > 1e-12 {
            let r = (area2.abs() * 0.5) / area3;
            lo = lo.min(r);
            hi = hi.max(r);
        }
    }
    if lo > 0.0 && lo < f32::MAX { hi / lo } else { f32::INFINITY }
}

/// A picture of the UV layout: every level-0 face's sides drawn into a
/// `size`² image (white on dark), charts and seams at a glance.
pub fn layout_image(poly: &Poly, uv: &[[f32; 2]], size: usize) -> egui::ColorImage {
    let mut img = egui::ColorImage::new([size, size], vec![egui::Color32::from_gray(24); size * size]);
    let (starts, _) = poly.cage_faces();
    let s = size as f32 - 1.0;
    // UDIM layouts: the tiles in use side by side (tile rows up), their
    // borders dim; one tile fills the picture.
    let (cols, rows) = uv.iter().fold((1.0f32, 1.0f32), |(c, r), t| (c.max(t[0].floor().clamp(0.0, 9.0) + 1.0), r.max(t[1].floor().clamp(0.0, 9.0) + 1.0)));
    let span = cols.max(rows);
    if span > 1.0 {
        for k in 0..=span as usize {
            let at = ((k as f32 / span) * s).round() as usize;
            for i in 0..size {
                img.pixels[at.min(size - 1) * size + i] = egui::Color32::from_gray(70);
                img.pixels[i * size + at.min(size - 1)] = egui::Color32::from_gray(70);
            }
        }
    }
    for w in starts.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        for j in a..b {
            let (u, v) = (uv[j], uv[if j + 1 == b { a } else { j + 1 }]);
            let (p, q) = (Vec2::new(u[0] / span * s, (1.0 - u[1] / span) * s), Vec2::new(v[0] / span * s, (1.0 - v[1] / span) * s));
            let n = (p.distance(q).ceil() as usize).clamp(1, 4 * size);
            for k in 0..=n {
                let r = p.lerp(q, k as f32 / n as f32);
                let (x, y) = (r.x.round() as isize, r.y.round() as isize);
                if (0..size as isize).contains(&x) && (0..size as isize).contains(&y) {
                    img.pixels[y as usize * size + x as usize] = egui::Color32::from_gray(230);
                }
            }
        }
    }
    img
}

/// The UVs as a mirror image carries them (`Poly::mirrored`): each face
/// read the other way round from its first corner.
pub fn mirrored(face_start: &[u32], uv: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(uv.len());
    for w in face_start.windows(2) {
        let face = &uv[w[0] as usize..w[1] as usize];
        out.push(face[0]);
        out.extend(face[1..].iter().rev());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poly::Chunked;
    use crate::poly::subdiv::Refiner;

    fn unwrapped(p: &Poly) -> Poly {
        let u = unwrap(p, DEFAULT_SIZE, DEFAULT_PADDING).unwrap();
        Poly { uv: Some(Chunked::new(&u.uv)), ..p.clone() }
    }

    /// The cage's faces stay whole in their charts: every corner gets a
    /// UV in the tile, every face's UV polygon has area and turns one way
    /// (no face folded), and the unwrap is the same twice.
    #[test]
    fn unwrap_keeps_faces_whole() {
        let head = crate::poly::project::tests::retopo_cage(&crate::poly::retopo::tests::clay(crate::poly::retopo::tests::head(), 80), 2000);
        for (name, p) in [("sphere", crate::poly::project::tests::cube_sphere(8, 0.5)), ("torus", crate::poly::tests::torus(24, 12)), ("head", head)] {
            let u = unwrap(&p, DEFAULT_SIZE, DEFAULT_PADDING).unwrap();
            assert_eq!(u.uv.len(), p.corners.len());
            assert!(u.uv.iter().flatten().all(|x| (0.0..=1.0).contains(x)), "{name}: UVs in the tile");
            let (starts, _) = p.cage_faces();
            for w in starts.windows(2) {
                let (a, b) = (w[0] as usize, w[1] as usize);
                let turns: Vec<f32> = (a..b).map(|j| {
                    let (x, y, z) = (Vec2::from(u.uv[if j == a { b - 1 } else { j - 1 }]), Vec2::from(u.uv[j]), Vec2::from(u.uv[if j + 1 == b { a } else { j + 1 }]));
                    (y - x).perp_dot(z - y)
                }).collect();
                assert!(turns.iter().all(|t| *t > 0.0) || turns.iter().all(|t| *t < 0.0), "{name}: a face folded in UV: {turns:?}");
            }
            let q = unwrapped(&p);
            let seams = seams(&q.topology(), &u.uv);
            eprintln!("{name}: {} faces, {} charts, {} faces made whole, {seams} seam edges, utilization {:.3}, area spread {:.2}", p.faces(), u.charts, u.repaired, u.utilization, stretch_spread(&p, &u.uv));
            assert!(u.charts >= 1 && seams > 0 && u.utilization > 0.3);
            // No two faces share a texel (texel middles strictly inside).
            let n = 1024usize;
            let mut owner = vec![u32::MAX; n * n];
            let mut shared = 0usize;
            for (f, w) in starts.windows(2).enumerate() {
                let (a, b) = (w[0] as usize, w[1] as usize);
                for j in a + 1..b - 1 {
                    let tri = [u.uv[a], u.uv[j], u.uv[j + 1]].map(|x| Vec2::from(x) * n as f32);
                    let (lo, hi) = (tri[0].min(tri[1]).min(tri[2]), tri[0].max(tri[1]).max(tri[2]));
                    for y in lo.y.floor().max(0.0) as usize..(hi.y.ceil() as usize).min(n) {
                        for x in lo.x.floor().max(0.0) as usize..(hi.x.ceil() as usize).min(n) {
                            let q = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                            let e = |p: Vec2, r: Vec2| (r - p).perp_dot(q - p);
                            let (d0, d1, d2) = (e(tri[0], tri[1]), e(tri[1], tri[2]), e(tri[2], tri[0]));
                            if (d0 > 0.0 && d1 > 0.0 && d2 > 0.0) || (d0 < 0.0 && d1 < 0.0 && d2 < 0.0) {
                                let o = &mut owner[y * n + x];
                                if *o != u32::MAX && *o != f as u32 {
                                    shared += 1;
                                }
                                *o = f as u32;
                            }
                        }
                    }
                }
            }
            let covered = owner.iter().filter(|&&o| o != u32::MAX).count();
            eprintln!("{name}: {shared} of {covered} covered texels claimed by two faces");
            assert!(shared * 1000 <= covered, "{name}: faces overlap in UV ({shared} texels)");
            assert_eq!(unwrap(&p, DEFAULT_SIZE, DEFAULT_PADDING).unwrap().uv, u.uv, "{name}: deterministic");
        }
    }

    /// Levels' UVs are face-varying linear: a seam edge splits into two
    /// seam edges per level and inner edges are never seams, so level k
    /// has 2^k times the cage's seams; a child's middle corner is its
    /// parent's mean.
    #[test]
    fn levels_take_linear_uvs() {
        let p = unwrapped(&crate::poly::project::tests::cube_sphere(6, 0.5));
        let r = Refiner::new(&p, 3).unwrap();
        let s0 = seams(&p.topology(), &at_level(&p, 0).unwrap());
        for k in 1..3u8 {
            let uv = at_level(&p, k).unwrap();
            assert_eq!(uv.len(), r.topology[k as usize].corners.len());
            assert_eq!(seams(&r.topology[k as usize], &uv), s0 << k, "level {k}");
        }
        let (u0, u1) = (at_level(&p, 0).unwrap(), at_level(&p, 1).unwrap());
        let mean = (u0[0][0] + u0[1][0] + u0[2][0] + u0[3][0]) / 4.0;
        assert!((u1[2][0] - mean).abs() < 1e-7 && u1[0] == u0[0]);
    }

    /// A mirror image keeps each face's UVs (read the other way round)
    /// and its seams.
    #[test]
    fn mirrored_keeps_uvs() {
        let p = unwrapped(&crate::poly::project::tests::cube_sphere(5, 0.5));
        let m = p.mirrored(0);
        let (a, b) = (p.uv.as_ref().unwrap().to_vec(), m.uv.as_ref().unwrap().to_vec());
        let (starts, _) = p.cage_faces();
        for w in starts.windows(2) {
            let (x, y) = (w[0] as usize, w[1] as usize);
            let mut fa = a[x..y].to_vec();
            let mut fb = b[x..y].to_vec();
            fa.sort_by(|p, q| p.partial_cmp(q).unwrap());
            fb.sort_by(|p, q| p.partial_cmp(q).unwrap());
            assert_eq!(fa, fb);
        }
        assert_eq!(seams(&m.topology(), &b), seams(&p.topology(), &a));
    }
}
