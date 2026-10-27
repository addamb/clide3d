//! Render-time subdivision and displacement of polygon objects (docs/
//! QUALITY_PLAN.md Stage 4a): a Poly renders as its Catmull-Clark surface
//! with its projected displacement, diced about a pixel fine where the
//! cameras see it.
//!
//! Each cage face gets a level from the cameras' footprint (`dice::Eye`)
//! where the face comes nearest each camera: the smallest level whose
//! edges, the longest cage edge halved once a level, are at most `rate`
//! pixels long, from 1 to `max_level`. Faces wholly outside a camera's
//! frustum (its width and height both, with a margin), or turned away
//! from every camera, count pixels 8 times larger (only shadows and
//! bounces see them); a turned-away face beside one that is not takes
//! that face's level, so outline edges are diced as the front sees them.
//! The finest level M any face wants is evaluated
//! once for the whole Poly, exactly as `subdiv::evaluate` does (stored
//! displacements up to M, none finer), so every point the dicing uses is
//! a point of that one surface. Each face's level-1 quads (one per
//! corner, so n-gons too) are then written at the face's own level: a
//! point keeps its index from level to level (`Topology::refine`), so a
//! level-l quad's corners, read at level l, are level-M points, and each
//! quad is fanned from its first corner as `Mesh::poly` fans it. Where a
//! cage edge's two faces want different levels, the finer side's points
//! along it snap to the coarser side's (the edge takes the lower level):
//! both sides then share the same points along the edge and the surface
//! stays watertight; the snapped triangles that collapse are dropped.
//!
//! Sizes are bounded from counts before anything is allocated: the
//! triangles the levels would make (`Options::max_triangles`, the sum
//! over all Polys of a render; every face one level coarser until they
//! fit) and the faces of level M (`MAX_EVALUATED`). A Poly that cannot
//! fit (level 1 past the evaluation bound, or past what the budget leaves
//! with every face at level 1) renders at its shown level, as before
//! Stage 4a, with a note in its report; the render is never refused. Both tracers then trace the
//! same triangles (`gpu.rs` is unchanged). Each output is a pure function
//! of the Poly, the placement and the cameras, made in face order, so the
//! mesh is the same on any thread count.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::dice::Eye;
use super::{Camera, Item};
use crate::mesh::Mesh;
use crate::poly::Poly;
use crate::poly::subdiv::Refiner;

/// Finest level a render dices to by default (the Poly bound's levels).
pub const DEFAULT_MAX_LEVEL: u8 = crate::poly::MAX_LEVELS;
/// Default edge length on screen, in pixels.
pub const DEFAULT_RATE: f32 = 1.0;
pub const MIN_RATE: f32 = 0.5;
pub const MAX_RATE: f32 = 16.0;
/// Default and largest triangle budget for all Polys of one render.
pub const DEFAULT_MAX_TRIANGLES: usize = 24_000_000;
pub const MIN_TRIANGLES: usize = 10_000;
/// Most faces of the level a Poly is evaluated at (its points, normals
/// and the levels' topology are held while it is diced).
pub const MAX_EVALUATED: u64 = 8_000_000;
/// A face whose normal turns more than this cosine away from every
/// camera is diced as an off-screen one.
const BACK_COS: f32 = 0.3;
/// Cage faces per parallel chunk.
const PAR: usize = 256;

/// `render_film`'s subdivision arguments.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// 0: Polys render as drawn (their shown level), as before Stage 4a.
    pub max_level: u8,
    /// false: no stored displacement (the smooth limit of the cage).
    pub displacement: bool,
    /// Target edge length on screen, in pixels.
    pub rate: f32,
    /// Most triangles all Polys of the render may make.
    pub max_triangles: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { max_level: DEFAULT_MAX_LEVEL, displacement: true, rate: DEFAULT_RATE, max_triangles: DEFAULT_MAX_TRIANGLES }
    }
}

impl Options {
    /// `subdiv_max_level` (0..=6), `displacement`, `dice_rate` (pixels,
    /// 0.5..=16) and `max_diced` (triangles, 10,000..=24,000,000), each
    /// refused outside its range.
    pub fn parse(args: &Value) -> Result<Options, String> {
        let mut o = Options::default();
        if let Some(v) = args.get("subdiv_max_level").filter(|v| !v.is_null()) {
            o.max_level = v.as_u64().filter(|l| *l <= crate::poly::MAX_LEVELS as u64).ok_or(format!("subdiv_max_level must be an integer 0..={} (0 renders Polys at their shown level)", crate::poly::MAX_LEVELS))? as u8;
        }
        if let Some(v) = args.get("displacement").filter(|v| !v.is_null()) {
            o.displacement = v.as_bool().ok_or("displacement must be true or false")?;
        }
        if let Some(v) = args.get("dice_rate").filter(|v| !v.is_null()) {
            o.rate = v.as_f64().filter(|r| (MIN_RATE as f64..=MAX_RATE as f64).contains(r)).ok_or(format!("dice_rate must be {MIN_RATE}..={MAX_RATE} pixels"))? as f32;
        }
        if let Some(v) = args.get("max_diced").filter(|v| !v.is_null()) {
            o.max_triangles = v.as_u64().filter(|n| (MIN_TRIANGLES as u64..=DEFAULT_MAX_TRIANGLES as u64).contains(n)).ok_or(format!("max_diced must be an integer {MIN_TRIANGLES}..={DEFAULT_MAX_TRIANGLES}"))? as usize;
        }
        Ok(o)
    }
}

/// A Poly item of a render: which item draws it, its object's name and
/// the Poly with every stored level.
pub struct Source {
    pub item: usize,
    pub name: String,
    pub poly: Arc<Poly>,
}

/// One Poly diced: the mesh and what was done.
pub struct Diced {
    pub mesh: Mesh,
    /// Levels the cage faces were diced at (least, most) and the level
    /// evaluated.
    pub levels: (u8, u8),
    pub evaluated: u8,
    pub triangles: usize,
}

/// Every Poly item diced for these cameras. Returns the triangles added
/// (over the drawn meshes they replace) and a report per item; a Poly
/// that cannot fit (the evaluation bound, or the budget even at level 1)
/// keeps its shown level, with a note naming the arguments that would
/// let it dice. Only cancelling fails.
pub fn subdivide(items: &mut [Item], sources: &[Source], cams: &[Camera], width: usize, height: usize, opts: &Options, progress: &crate::progress::Progress) -> Result<(isize, Vec<Value>), String> {
    let eyes: Vec<Eye> = cams.iter().map(|c| Eye::new(c, width, height)).collect();
    let mut reports = Vec::new();
    if opts.max_level == 0 || eyes.is_empty() {
        return Ok((0, reports));
    }
    // Every Poly's face levels first: the budget is shared, so all are
    // made coarser together (not the last ones refused).
    let mut planned: Vec<(&Source, Vec<u8>)> = Vec::new();
    for s in sources {
        progress.check()?;
        let Some(item) = items.get(s.item) else { continue };
        // A baked game mesh shown in its place renders as baked.
        if item.mesh.textures.is_some() {
            reports.push(json!({ "object": s.name, "note": "a baked mesh is shown: rendered as baked, not subdivided" }));
            continue;
        }
        let mut levels = face_levels(&s.poly, item.world, &eyes, opts);
        match fit_evaluation(&s.poly, &mut levels) {
            Ok(()) => planned.push((s, levels)),
            Err(e) => reports.push(shown_level(s, &e)),
        }
    }
    let mut lists: Vec<(&Poly, &mut Vec<u8>)> = planned.iter_mut().map(|(s, l)| (&*s.poly, l)).collect();
    let (coarser, dropped) = fit_budget(&mut lists, opts.max_triangles);
    let mut added = 0isize;
    for (i, (s, levels)) in planned.iter().enumerate() {
        progress.check()?;
        if let Some(e) = dropped.iter().find(|d| d.0 == i) {
            reports.push(shown_level(s, &e.1));
            continue;
        }
        let item = &mut items[s.item];
        let d = match build(&s.poly, levels, opts) {
            Ok(d) => d,
            Err(e) => {
                reports.push(shown_level(s, &e));
                continue;
            }
        };
        added += d.triangles as isize - (item.mesh.indices.len() / 3) as isize;
        let displaced = opts.displacement && s.poly.levels.iter().take(d.evaluated as usize).any(|l| l.displacement.is_some());
        let mut r = json!({
            "object": s.name,
            "cage_faces": s.poly.faces(),
            "levels": [d.levels.0, d.levels.1],
            "evaluated_level": d.evaluated,
            "triangles": d.triangles,
            "displaced": displaced,
        });
        let fewer = coarser.max(levels_cut(&s.poly, levels, opts, &eyes, item.world));
        if fewer > 0 {
            r["note"] = json!(format!("some faces diced up to {fewer} level{} coarser than a pixel asks, to fit max_diced or the evaluation bound ({MAX_EVALUATED} faces)", if fewer == 1 { "" } else { "s" }));
        }
        reports.push(r);
        item.mesh = Arc::new(d.mesh);
    }
    Ok((added, reports))
}

/// The report of a Poly left at its shown level, and why.
fn shown_level(s: &Source, why: &str) -> Value {
    json!({ "object": s.name, "cage_faces": s.poly.faces(), "levels": [], "triangles": 0, "shown_level": s.poly.level, "note": format!("rendered at its shown level {} (not subdivided): {why}", s.poly.level) })
}

/// How many levels the evaluation bound took off this Poly's finest face.
fn levels_cut(poly: &Poly, levels: &[u8], opts: &Options, eyes: &[Eye], world: Mat4) -> u8 {
    let asked = face_levels(poly, world, eyes, opts).into_iter().max().unwrap_or(1);
    asked.saturating_sub(levels.iter().copied().max().unwrap_or(1))
}

/// Caps the levels so the finest one evaluated fits `MAX_EVALUATED`;
/// refused when not even level 1 does.
fn fit_evaluation(poly: &Poly, levels: &mut [u8]) -> Result<(), String> {
    let top = levels.iter().copied().max().unwrap_or(1);
    let fit = (1..=top).rev().find(|&l| poly.faces_at(l) <= MAX_EVALUATED);
    match fit {
        Some(l) => {
            levels.iter_mut().for_each(|x| *x = (*x).min(l));
            Ok(())
        }
        None => Err(format!("its {} cage faces make {} faces at level 1, over the {MAX_EVALUATED}-face evaluation bound; send subdiv_max_level: 0 to render it at its shown level", poly.faces(), poly.faces_at(1))),
    }
}

/// Makes every face of every Poly one level coarser (to 1) until all
/// their triangles fit `budget`; returns how many times, and the Polys
/// (by index, with why) left out because level 1 everywhere did not fit:
/// the one with the most triangles at level 1 first (the earlier on a
/// tie), until the rest fit.
fn fit_budget(polys: &mut [(&Poly, &mut Vec<u8>)], budget: usize) -> (u8, Vec<(usize, String)>) {
    let mut coarser = 0u8;
    let mut dropped: Vec<(usize, String)> = Vec::new();
    loop {
        let kept = |i: &usize| !dropped.iter().any(|d| d.0 == *i);
        let total: u64 = (0..polys.len()).filter(kept).map(|i| triangles_at(polys[i].0, polys[i].1)).sum();
        if total <= budget as u64 {
            return (coarser, dropped);
        }
        if (0..polys.len()).filter(kept).all(|i| polys[i].1.iter().all(|&x| x == 1)) {
            let worst = (0..polys.len()).filter(kept).max_by(|&a, &b| triangles_at(polys[a].0, polys[a].1).cmp(&triangles_at(polys[b].0, polys[b].1)).then(b.cmp(&a))).expect("a Poly is over");
            let own = triangles_at(polys[worst].0, polys[worst].1);
            dropped.push((worst, format!("{own} triangles at level 1 ({} cage faces) with the others do not fit max_diced ({budget}, the sum over all Polys of the render); raise max_diced to dice it", polys[worst].0.faces())));
            continue;
        }
        for (_, l) in polys.iter_mut() {
            l.iter_mut().for_each(|x| *x = x.saturating_sub(1).max(1));
        }
        coarser += 1;
    }
}

/// Per cage face, the level a pixel asks for (1..=max_level).
fn face_levels(poly: &Poly, world: Mat4, eyes: &[Eye], opts: &Options) -> Vec<u8> {
    let (fs, corners) = poly.cage_faces();
    let cage = poly.cage.to_vec();
    let flip = if world.determinant() < 0.0 { -1.0 } else { 1.0 };
    // (level, behind) per face.
    let mut out = vec![(1u8, false); poly.faces()];
    crate::clay::par_chunks(&mut out, PAR, |i, chunk| {
        for (k, o) in chunk.iter_mut().enumerate() {
            let f = i * PAR + k;
            let face = &corners[fs[f] as usize..fs[f + 1] as usize];
            let p: Vec<Vec3> = face.iter().map(|&c| world.transform_point3(cage[c as usize])).collect();
            let middle = p.iter().sum::<Vec3>() / p.len() as f32;
            let longest = (0..p.len()).map(|j| p[j].distance(p[(j + 1) % p.len()])).fold(0.0f32, f32::max);
            // Faces every camera sees from behind (only shadows and
            // bounces see them) dice as off-screen ones do, 8 times
            // coarser. (A reflecting placement turns the winding inside
            // out.)
            let normal = (0..p.len()).map(|j| p[j].cross(p[(j + 1) % p.len()])).sum::<Vec3>().normalize_or_zero() * flip;
            let behind = normal != Vec3::ZERO && eyes.iter().all(|e| e.facing(middle, normal) < -BACK_COS);
            // Where the face comes nearest each camera, tested against
            // its whole frustum (not only at sampled points).
            let pixel = eyes.iter().map(|e| e.face_footprint(&p).0).fold(f32::MAX, f32::min) * if behind { 8.0 } else { 1.0 };
            let ratio = longest / (opts.rate * pixel).max(1e-12);
            let want = if ratio > 1.0 { ratio.log2().ceil().min(64.0) as u8 } else { 1 };
            *o = (want.clamp(1, opts.max_level.max(1)), behind);
        }
    });
    // Outline edges belong to the front: a face seen from behind takes the
    // finest level of the faces beside it that are not (edges take the
    // lower level of their faces, so otherwise the front face's outline
    // would snap to the back face's coarse one). Read from the first
    // pass only, in face order: the same on any thread count.
    let first: Vec<(u8, bool)> = out.clone();
    let mut by_edge: std::collections::HashMap<(u32, u32), Vec<u32>> = std::collections::HashMap::new();
    if first.iter().any(|x| x.1) && first.iter().any(|x| !x.1) {
        for f in 0..poly.faces() {
            let face = &corners[fs[f] as usize..fs[f + 1] as usize];
            for j in 0..face.len() {
                let (a, b) = (face[j], face[(j + 1) % face.len()]);
                by_edge.entry((a.min(b), a.max(b))).or_default().push(f as u32);
            }
        }
    }
    let mut levels: Vec<u8> = first.iter().map(|x| x.0).collect();
    for faces in by_edge.values() {
        let front = faces.iter().filter(|&&f| !first[f as usize].1).map(|&f| first[f as usize].0).max();
        if let Some(front) = front {
            for &f in faces.iter().filter(|&&f| first[f as usize].1) {
                levels[f as usize] = levels[f as usize].max(front);
            }
        }
    }
    levels
}

/// Triangles a Poly makes at these face levels (two per level quad).
fn triangles_at(poly: &Poly, levels: &[u8]) -> u64 {
    (0..poly.faces()).map(|f| (poly.face_start.get(f + 1) - poly.face_start.get(f)) as u64 * 2 * 4u64.pow(levels[f] as u32 - 1)).sum()
}

/// One Poly diced for `eyes` within `budget` triangles (see the module
/// doc).
/// One Poly diced alone within `budget` (the tests' entry; `subdivide`
/// plans a render's Polys together). Also returns how many levels the
/// finest face lost to the bounds.
#[cfg(test)]
pub(super) fn dice(poly: &Poly, world: Mat4, eyes: &[Eye], opts: &Options, budget: usize) -> Result<(Diced, u8), String> {
    let mut levels = face_levels(poly, world, eyes, opts);
    let asked = levels.iter().copied().max().unwrap_or(1);
    fit_evaluation(poly, &mut levels)?;
    let (coarser, dropped) = fit_budget(&mut [(poly, &mut levels)], budget);
    if let Some((_, why)) = dropped.into_iter().next() {
        return Err(why);
    }
    let d = build(poly, &levels, opts)?;
    let lost = asked.saturating_sub(d.evaluated).max(coarser);
    Ok((d, lost))
}

/// One Poly diced at these face levels (bounded by the caller).
fn build(poly: &Poly, levels: &[u8], opts: &Options) -> Result<Diced, String> {
    let top = levels.iter().copied().max().unwrap_or(1);
    let least = levels.iter().copied().min().unwrap_or(1);
    let mut p = poly.clone();
    if !opts.displacement {
        p.levels.iter_mut().for_each(|l| l.displacement = None);
    }
    let r = Refiner::new(&p, top)?;
    let points = r.points(p.cage.to_vec(), &p.levels);
    let normals: Vec<Vec3> = crate::poly::subdiv::frames(&points, &r.top.face_start, &r.top.corners).into_iter().map(|f| f[2]).collect();
    // Painted colour tiles (Stage 4b): each level-`top` point takes the
    // mean of its corners' samples, so the about-a-pixel triangles carry
    // the texture as vertex colours (both tracers read them; seams agree).
    let colors = p.paint.as_ref().and_then(|paint| {
        let uv = crate::poly::uv::at_level(&p, top)?;
        crate::poly::texture::point_colors(&paint.textures, &r.top.corners, &uv, points.len())
    });
    let t0 = &r.topology[0];
    // Each cage edge at the lower level of its faces (the finer side's
    // points along it snap to the coarser side's: crack-free). Outline
    // edges are at the front face's level: `face_levels` raised the back
    // face beside it.
    let edge_level: Vec<u8> = (0..t0.edges.len()).map(|e| t0.faces_of_edge(e).iter().map(|&f| levels[f as usize]).min().unwrap_or(1)).collect();
    let corners_of = |k: u8| -> &[u32] { r.faces_of_level(k as usize).1 };
    let faces = poly.faces();
    let mut tris: Vec<Vec<[u32; 3]>> = vec![Vec::new(); faces.div_ceil(PAR)];
    crate::clay::par_chunks(&mut tris, 1, |i, out| {
        let out = &mut out[0];
        let mut quads: Vec<(u32, [[u32; 2]; 4])> = Vec::new();
        for (f, &l) in levels.iter().enumerate().take(((i + 1) * PAR).min(faces)).skip(i * PAR) {
            let n = 1u32 << (l - 1);
            for q in t0.face_start[f] as usize..t0.face_start[f + 1] as usize {
                let prev = if q == t0.face_start[f] as usize { t0.face_start[f + 1] as usize - 1 } else { q - 1 };
                let (side0, side3) = (1u32 << (edge_level[t0.corner_edge[q] as usize] - 1), 1u32 << (edge_level[t0.corner_edge[prev] as usize] - 1));
                // The patch's level-l quads with their grid places
                // (corner 0 the cage point, x toward the next edge).
                quads.clear();
                descend(q as u32, [[0, 0], [n, 0], [n, n], [0, n]], l - 1, &mut quads);
                let level = corners_of(l);
                // The points along the two cage-edge sides, by place.
                let mut on0 = vec![u32::MAX; n as usize + 1];
                let mut on3 = vec![u32::MAX; n as usize + 1];
                for (face, at) in &quads {
                    for c in 0..4 {
                        let idx = level[4 * *face as usize + c];
                        let [u, v] = at[c];
                        if v == 0 {
                            on0[u as usize] = idx;
                        }
                        if u == 0 {
                            on3[v as usize] = idx;
                        }
                    }
                }
                let snap = |x: u32, side: u32| -> u32 {
                    let step = n / side;
                    (x + step / 2) / step * step
                };
                for (face, at) in &quads {
                    let mut c = [0u32; 4];
                    for j in 0..4 {
                        let [u, v] = at[j];
                        c[j] = if v == 0 && side0 < n {
                            on0[snap(u, side0) as usize]
                        } else if u == 0 && side3 < n {
                            on3[snap(v, side3) as usize]
                        } else {
                            level[4 * *face as usize + j]
                        };
                    }
                    for t in [[c[0], c[1], c[2]], [c[0], c[2], c[3]]] {
                        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                            out.push(t);
                        }
                    }
                }
            }
        }
    });
    // Points in first-use order.
    let mut map = vec![u32::MAX; points.len()];
    let mut mesh = Mesh::default();
    let count: usize = tris.iter().map(Vec::len).sum();
    mesh.indices.reserve(3 * count);
    for t in tris.iter().flatten() {
        for &i in t {
            if map[i as usize] == u32::MAX {
                map[i as usize] = mesh.vert(points[i as usize], normals[i as usize]);
                if let Some(c) = &colors {
                    mesh.vertices[map[i as usize] as usize].color = c[i as usize];
                }
            }
            mesh.indices.push(map[i as usize]);
        }
    }
    Ok(Diced { mesh, levels: (least, top), evaluated: top, triangles: count })
}

/// A level-k quad (face `face` of its level) descended `down` levels:
/// each child j is face 4·face + j of the next level, at the parent's
/// corner j, its corners [the corner, the middle of the next side, the
/// middle, the middle of the previous side] (`Topology::refine_core`).
fn descend(face: u32, at: [[u32; 2]; 4], down: u8, out: &mut Vec<(u32, [[u32; 2]; 4])>) {
    if down == 0 {
        out.push((face, at));
        return;
    }
    let mid = |a: [u32; 2], b: [u32; 2]| [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
    let centre = mid(at[0], at[2]);
    for j in 0..4 {
        let child = [at[j], mid(at[j], at[(j + 1) % 4]), centre, mid(at[(j + 3) % 4], at[j])];
        descend(4 * face + j as u32, child, down - 1, out);
    }
}

#[cfg(test)]
#[path = "subdiv/tests.rs"]
mod tests;
