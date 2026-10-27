//! A Poly level as a bake's simple mesh (docs/QUALITY_PLAN.md Stage 3c,
//! `bake lowpoly: {poly, level}`; the Stage 3b leftover).
//!
//! The simple mesh is level `level` of the cage subdivided with no
//! displacement (what another program's Catmull-Clark makes of the cage),
//! in the clay's space, with the Poly's UVs (`poly uv`) carried down the
//! levels, split where UVs differ, and tangents along +u. Its vertices
//! take the materials of the nearest rest vertex of the clay.
//!
//! Each texel's target is the Poly's projected surface at the same place:
//! the texel's triangle and weights give a point of its level face (a
//! quad's two triangles cover its parameter square), which descends
//! through the child quads (`Topology::refine`'s corner order) to the
//! finest displaced level, read bilinearly there. A displacement map
//! therefore holds the projection, sideways moves and overhangs included,
//! and the bake then snaps the point onto the exact clay along that
//! surface's normal. Without projected detail (or in an n-gon at level 0)
//! the target is searched for from the texel (`project::find`).

use glam::{Mat3, Mat4, Vec2, Vec3};

use super::subdiv::{Surface, evaluate};
use super::{Level, Poly};
use crate::mesh::{Mesh, Vertex};

/// The simple mesh and what finds each texel's target.
pub struct Low {
    pub mesh: Mesh,
    /// Per triangle: its level face and fan index (corners 0, j, j + 1;
    /// written 0, j + 1, j when `flipped`).
    tri: Vec<(u32, u32)>,
    /// The placement reflects: triangles are wound the other way.
    flipped: bool,
    /// The level's faces (offsets).
    face_start: Vec<u32>,
    level: u8,
    /// The displaced surface to descend to, in the clay's space (shared
    /// by a UDIM layout's tiles, `tile`).
    detail: Option<std::sync::Arc<Detail>>,
    /// Mean cage edge, in the clay's space (searches' unit).
    pub edge: f32,
    /// Per vertex of `mesh`: the nearest rest vertex of the clay (its
    /// materials, its split-rig part and its skin weights).
    pub rest_vertex: Vec<usize>,
}

struct Detail {
    level: u8,
    surface: Surface,
}

/// Levels carrying a displacement (the finest, 0 without).
fn displaced_level(p: &Poly) -> u8 {
    p.levels.iter().rposition(|l| l.displacement.is_some()).map_or(0, |k| k as u8 + 1)
}

impl Low {
    /// Level `level` of `poly` as a simple mesh in the clay's space
    /// (`to_clay`), UVs from `poly.uv` or, without them, an unwrap of the
    /// cage packed for `size` pixels (the bake's reply says which). `rest`
    /// gives the materials.
    pub fn new(poly: &Poly, level: u8, to_clay: Mat4, rest: &Mesh, size: u32, progress: &crate::progress::Progress) -> Result<Low, String> {
        if poly.uv.as_ref().is_some_and(|u| super::uv::spans_tiles(&u.to_vec())) {
            return Err("the Poly's UVs are on UDIM tiles: one map or texture cannot hold them; send udim: true".into());
        }
        Self::build(poly, level, to_clay, rest, size, progress)
    }

    /// `new` for a Poly whose UVs span UDIM tiles: the whole level, each
    /// tile then taken by `tile`.
    pub fn new_udim(poly: &Poly, level: u8, to_clay: Mat4, rest: &Mesh, progress: &crate::progress::Progress) -> Result<Low, String> {
        if poly.uv.is_none() {
            return Err("UDIM maps need UVs on the Poly: run poly uv udim: true first".into());
        }
        Self::build(poly, level, to_clay, rest, 2048, progress)
    }

    /// The tiles `tile` finds triangles on (by each triangle's UV middle),
    /// ascending.
    pub fn tiles(&self) -> Vec<u16> {
        let mut out: std::collections::BTreeSet<u16> = Default::default();
        for t in self.mesh.indices.chunks_exact(3) {
            let uv = [t[0], t[1], t[2]].map(|i| self.mesh.vertices[i as usize].uv);
            let mid = [(uv[0][0] + uv[1][0] + uv[2][0]) / 3.0, (uv[0][1] + uv[1][1] + uv[2][1]) / 3.0];
            out.extend(crate::poly::texture::tile_of(mid));
        }
        out.into_iter().collect()
    }

    /// What a UDIM bake will bake: each tile the level's triangles lie on
    /// (`tiles`), its simple mesh (`tile`) and the pages under its
    /// triangles (`margin` texels around them, at `resolution`), with the
    /// pages it would hold (`channels` each) counted from `start` against
    /// `max` before any bake: Err(Ok(pages)) past it, Err(Err(e)) when
    /// cancelled.
    #[allow(clippy::type_complexity)]
    pub fn bake_plan(&self, resolution: u32, margin: f32, channels: usize, start: usize, max: usize, progress: &crate::progress::Progress) -> Result<Vec<(u16, Low, std::collections::BTreeSet<usize>)>, Result<usize, String>> {
        let mut plan = Vec::new();
        let mut planned = start;
        for t in self.tiles() {
            progress.check().map_err(Err)?;
            let Some(lt) = self.tile(t) else { continue };
            let under = crate::poly::texture::Textures::pages_under(lt.mesh.indices.chunks_exact(3).map(|k| [k[0], k[1], k[2]].map(|v| lt.mesh.vertices[v as usize].uv)), resolution, margin);
            planned += under.len() * channels;
            if planned > max {
                return Err(Ok(planned));
            }
            plan.push((t, lt, under));
        }
        Ok(plan)
    }

    /// The triangles whose UVs lie in `tile`, their UVs moved into 0-1 (a
    /// chart lies on one tile, so its vertices do too): a bake's simple
    /// mesh for that tile, finding texels' targets as the whole does.
    /// None when no triangle is there.
    pub fn tile(&self, tile: u16) -> Option<Low> {
        // Only the tile's triangles and the vertices they use (renumbered
        // in first-use order), so nothing of another tile is diced or
        // baked with this tile's map.
        let mut mesh = Mesh { materials: self.mesh.materials.clone(), ..Mesh::default() };
        let mut map = vec![u32::MAX; self.mesh.vertices.len()];
        let mut rest_vertex = Vec::new();
        let mut tri = Vec::new();
        let o = crate::poly::texture::tile_origin(tile);
        for (k, t) in self.mesh.indices.chunks_exact(3).enumerate() {
            let uv = [t[0], t[1], t[2]].map(|i| self.mesh.vertices[i as usize].uv);
            let mid = [(uv[0][0] + uv[1][0] + uv[2][0]) / 3.0, (uv[0][1] + uv[1][1] + uv[2][1]) / 3.0];
            if crate::poly::texture::tile_of(mid) != Some(tile) {
                continue;
            }
            for &i in t {
                if map[i as usize] == u32::MAX {
                    map[i as usize] = mesh.vertices.len() as u32;
                    let mut v = self.mesh.vertices[i as usize];
                    v.uv = [v.uv[0] - o[0], v.uv[1] - o[1]];
                    mesh.vertices.push(v);
                    rest_vertex.push(self.rest_vertex[i as usize]);
                }
                mesh.indices.push(map[i as usize]);
            }
            tri.push(self.tri[k]);
        }
        if tri.is_empty() {
            return None;
        }
        mesh.edges = crate::bake::edges(&mesh.indices);
        Some(Low { mesh, tri, flipped: self.flipped, face_start: self.face_start.clone(), level: self.level, detail: self.detail.clone(), edge: self.edge, rest_vertex })
    }

    fn build(poly: &Poly, level: u8, to_clay: Mat4, rest: &Mesh, size: u32, progress: &crate::progress::Progress) -> Result<Low, String> {
        if poly.faces_at(level) > super::MAX_SHOWN_FACES {
            return Err(format!("level {level} has {} faces; a bake's simple mesh may have at most {} (a lower level)", poly.faces_at(level), super::MAX_SHOWN_FACES));
        }
        let mut bare = poly.clone();
        bare.set_levels(bare.level_count().max(level))?;
        if bare.uv.is_none() {
            let u = super::uv::unwrap(&bare, size, 4)?;
            bare.uv = Some(super::Chunked::new(&u.uv));
        }
        let uv = super::uv::at_level(&bare, level).expect("set above");
        let detail_level = displaced_level(&bare);
        let detail = if detail_level > 0 {
            let mut d = bare.clone();
            let at = detail_level.max(level);
            d.set_levels(d.level_count().max(at))?;
            let mut s = evaluate(&d, at)?;
            for p in &mut s.points {
                *p = to_clay.transform_point3(*p);
            }
            Some(std::sync::Arc::new(Detail { level: at, surface: s }))
        } else {
            None
        };
        progress.check()?;
        for l in &mut bare.levels {
            *l = Level::default();
        }
        let s = evaluate(&bare, level)?;
        let normals = s.normals();
        let normal_map = Mat3::from_mat4(to_clay).inverse().transpose();
        // One vertex per (level point, UV).
        let mut keys: Vec<(u32, [u32; 2], u32)> = (0..s.corners.len() as u32).map(|c| (s.corners[c as usize], [uv[c as usize][0].to_bits(), uv[c as usize][1].to_bits()], c)).collect();
        keys.sort_unstable();
        let mut vert_of = vec![0u32; s.corners.len()];
        let mut mesh = Mesh { materials: rest.materials.clone(), ..Mesh::default() };
        // The first key of each (point, UV): one vertex each.
        let firsts: Vec<usize> = (0..keys.len()).filter(|&i| i == 0 || (keys[i].0, keys[i].1) != (keys[i - 1].0, keys[i - 1].1)).collect();
        let place: Vec<Vec3> = firsts.iter().map(|&i| to_clay.transform_point3(s.points[keys[i].0 as usize])).collect();
        // Each vertex's nearest rest vertex, in parallel (each a pure
        // function of its point: the same on any thread count).
        let near = crate::rig::NearestVertex::new(rest, edge_of(poly) * crate::poly::project::stretch(Mat3::from_mat4(to_clay)) * 2.0);
        let mut rest_vertex = vec![0usize; firsts.len()];
        crate::clay::par_chunks(&mut rest_vertex, 4096, |c, out| {
            for (k, r) in out.iter_mut().enumerate() {
                *r = near.index(place[c * 4096 + k]).filter(|&r| r < rest.vertices.len()).unwrap_or(0);
            }
        });
        progress.check()?;
        mesh.vertices.reserve(firsts.len());
        let mut v = 0usize;
        for (i, k) in keys.iter().enumerate() {
            if firsts.get(v) == Some(&i) {
                let n = (normal_map * normals[k.0 as usize]).normalize_or(Vec3::Y);
                let from = rest.vertices.get(rest_vertex[v]).copied().unwrap_or_else(blank);
                mesh.vertices.push(Vertex { pos: place[v].to_array(), normal: n.to_array(), color: [1.0; 3], uv: uv[k.2 as usize], ..from });
                v += 1;
            }
            vert_of[k.2 as usize] = mesh.vertices.len() as u32 - 1;
        }
        // A placement that reflects (a linked mirror copy reflects by its
        // matrix) turns the faces inside out: wind them the other way so
        // they face out with their normals (`surface_point` swaps back).
        let flipped = to_clay.determinant() < 0.0;
        let mut tri = Vec::with_capacity(s.corners.len());
        for (f, w) in s.face_start.windows(2).enumerate() {
            let a = w[0] as usize;
            for j in 1..(w[1] - w[0]) as usize - 1 {
                let (b, c) = if flipped { (a + j + 1, a + j) } else { (a + j, a + j + 1) };
                mesh.indices.extend_from_slice(&[vert_of[a], vert_of[b], vert_of[c]]);
                tri.push((f as u32, j as u32));
            }
        }
        crate::bake::tangents(&mut mesh);
        mesh.edges = crate::bake::edges(&mesh.indices);
        Ok(Low { mesh, tri, flipped, rest_vertex, face_start: s.face_start, level, detail, edge: edge_of(poly) * crate::poly::project::stretch(Mat3::from_mat4(to_clay)) })
    }

    /// The projected surface at triangle `t`'s weights `w` (its corners
    /// 0, j, j + 1): (point, normal), clay space; None without projected
    /// detail or in a face that is not a quad.
    pub fn surface_point(&self, t: u32, w: [f32; 3]) -> Option<(Vec3, Vec3)> {
        let d = self.detail.as_ref()?;
        let (f, j) = *self.tri.get(t as usize)?;
        let w = if self.flipped { [w[0], w[2], w[1]] } else { w };
        let (a, b) = (self.face_start[f as usize] as usize, self.face_start[f as usize + 1] as usize);
        if b - a != 4 {
            return None;
        }
        // The face's parameter square: corners (0,0), (1,0), (1,1), (0,1).
        let mut st = if j == 1 { Vec2::new(w[1] + w[2], w[2]) } else { Vec2::new(w[1], w[1] + w[2]) };
        // Level `level`'s face f: its children at the next level are the
        // quads at its corners (the cage's corners at level 0, 4f + c
        // after).
        let mut face = f as usize;
        let starts_at = |level: u8, face: usize| if level == 0 { self.face_start[face] as usize } else { 4 * face };
        for k in self.level..d.level {
            let c = match (st.x >= 0.5, st.y >= 0.5) {
                (false, false) => 0,
                (true, false) => 1,
                (true, true) => 2,
                (false, true) => 3,
            };
            st = match c {
                0 => Vec2::new(2.0 * st.x, 2.0 * st.y),
                1 => Vec2::new(2.0 * st.y, 2.0 * (1.0 - st.x)),
                2 => Vec2::new(2.0 * (1.0 - st.x), 2.0 * (1.0 - st.y)),
                _ => Vec2::new(2.0 * (1.0 - st.y), 2.0 * st.x),
            }
            .clamp(Vec2::ZERO, Vec2::ONE);
            face = starts_at(k, face) + c;
        }
        let q = &d.surface.corners[4 * face..4 * face + 4];
        let p = |i: usize| d.surface.points[q[i] as usize];
        let (s, t) = (st.x, st.y);
        let point = p(0) * ((1.0 - s) * (1.0 - t)) + p(1) * (s * (1.0 - t)) + p(2) * (s * t) + p(3) * ((1.0 - s) * t);
        let du = (p(1) - p(0)) * (1.0 - t) + (p(2) - p(3)) * t;
        let dv = (p(3) - p(0)) * (1.0 - s) + (p(2) - p(1)) * s;
        // The detail points are placed in the clay's space: a reflection
        // turns the parameter frame's normal inward.
        let n = du.cross(dv).normalize_or_zero();
        Some((point, if self.flipped { -n } else { n }))
    }

    /// The finest displaced level's edge length (the snap's reach unit).
    pub fn fine_edge(&self) -> f32 {
        self.detail.as_ref().map_or(self.edge, |d| self.edge / (1u32 << d.level.min(16)) as f32)
    }
}

/// Mean cage edge in the Poly's space.
fn edge_of(poly: &Poly) -> f32 {
    super::project::mean_edge(poly)
}

fn blank() -> Vertex {
    Vertex { pos: [0.0; 3], normal: [0.0, 1.0, 0.0], color: [1.0; 3], surface: crate::mesh::OBJECT_SURFACE, material: 0, uv: [0.0; 2], tangent: [0.0; 4], light: [0.0; 2], base: [0.0; 4], wear: [0.0; 3], painted: [0; 4] }
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3};

    use super::*;
    use crate::bake::{Displacement, Maps, Source, displacement};
    use crate::clay::Clay;
    use crate::poly::project::tests::{cube_sphere, lip, noise_sphere, retopo_cage, run};
    use crate::poly::project::{Args, Mode, REACH_EDGES, mean_edge};

    /// How far maps reapplied as geometry (`film::dice::apply_baked`,
    /// diced to half a cell) lie from the clay, in cells: (largest, mean,
    /// share of points over a cell, texels missed). Vector or scalar,
    /// 32-bit floats (EXR) or 16-bit codes (PNG, as the bake tool writes
    /// them).
    pub(crate) fn reapply(c: &Clay, p: &Poly, level: u8, size: u32, vector: bool, png: bool) -> (f32, f32, f32, usize) {
        let composed = crate::layers::composed_clay(c);
        let rest = composed.build_mesh();
        let progress = crate::progress::Progress::default();
        let low = Low::new(p, level, Mat4::IDENTITY, &rest, size, &progress).unwrap();
        let src = Source { clay: &composed, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None, low: Some(&low) };
        let mut d = displacement(&src, &low.mesh, Maps { displacement: !vector, vector, size, object: false }, &progress).unwrap();
        if png {
            let quantize = |v: &[f32]| {
                let scale = 2.0 * v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                Displacement::encode16(v.iter().copied(), scale).iter().map(|&c| Displacement::decode16(c, scale)).collect::<Vec<f32>>()
            };
            if let Some(h) = &mut d.height {
                *h = quantize(h);
            }
            if let Some(v) = &mut d.vector {
                let flat: Vec<f32> = v.iter().flatten().copied().collect();
                *v = quantize(&flat).chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
        }
        let cell = c.cell_size().unwrap();
        let diced = crate::film::dice::apply_baked(&low.mesh, &d, cell * 0.5, 8_000_000).unwrap();
        let probe = crate::quality::Probe::new(&composed).unwrap();
        let pts: Vec<Vec3> = diced.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
        let mut e = vec![0.0f32; pts.len()];
        crate::clay::par_chunks(&mut e, 4096, |i, chunk| {
            for (k, o) in chunk.iter_mut().enumerate() {
                let q = pts[i * 4096 + k];
                *o = probe.near(q, 4.0 * cell)(q).abs() / cell;
            }
        });
        let max = e.iter().copied().fold(0.0, f32::max);
        let mean = e.iter().sum::<f32>() / e.len().max(1) as f32;
        let over = e.iter().filter(|&&x| x > 1.0).count() as f32 / e.len().max(1) as f32;
        (max, mean, over, d.missed)
    }

    fn projected(c: &Clay, p: &Poly, levels: u8, reach: f32) -> Poly {
        run(p, c, Args { levels, mode: Mode::Vector, cone: 45.0, reach }).0
    }

    /// The plan's `maps_reapply`: maps over the cage's UVs, baked on the
    /// undisplaced level and reapplied there as geometry, come back to
    /// the clay: on the noise sphere the vector map (32-bit) within 0.75
    /// cell and the scalar map (16-bit PNG codes) within a cell (a height
    /// field: its detail is along the normal).
    #[test]
    fn maps_reapply() {
        let c = noise_sphere();
        let mut p = cube_sphere(12, 0.5);
        p = projected(&c, &p, 3, REACH_EDGES * mean_edge(&p));
        p.uv = Some(crate::poly::Chunked::new(&crate::poly::uv::unwrap(&p, 2048, 4).unwrap().uv));
        let (vmax, vmean, _, vmiss) = reapply(&c, &p, 3, 2048, true, false);
        let (smax, smean, _, smiss) = reapply(&c, &p, 3, 2048, false, true);
        eprintln!("noise sphere at level 3: vector EXR {vmax:.3} cells (mean {vmean:.4}, {vmiss} missed), scalar PNG {smax:.3} (mean {smean:.4}, {smiss} missed)");
        assert!(vmax <= 0.75 && smax <= 1.0, "{vmax} {smax}");
    }

    /// Vector displacement holds an overhang that scalar displacement
    /// cannot: the lipped cylinder projected onto a plain cylinder's cage,
    /// baked on its undisplaced level 3 at 4096. The vector map reapplied
    /// lies on the clay but for a strip: the projection takes a band of
    /// the wall about a texel tall up the lip's whole underside (0.1, 22
    /// cells), more than any texel can hold, so the largest error is
    /// reported, not bounded. The scalar map (height along the normal
    /// only) leaves the lip's sideways move out: 60 times the mean error,
    /// and over a cell on 200 times as many points.
    #[test]
    fn vector_maps_hold_an_overhang() {
        let plain = lip(false);
        let lipped = lip(true);
        let mut p = retopo_cage(&plain, 2000);
        p = projected(&lipped, &p, 3, 0.15);
        p.uv = Some(crate::poly::Chunked::new(&crate::poly::uv::unwrap(&p, 4096, 4).unwrap().uv));
        let (vmax, vmean, vover, _) = reapply(&lipped, &p, 3, 4096, true, false);
        let (smax, smean, sover, _) = reapply(&lipped, &p, 3, 4096, false, false);
        eprintln!("lip at level 3: vector largest {vmax:.3} cells, mean {vmean:.4}, {:.3}% over a cell; scalar largest {smax:.3}, mean {smean:.4}, {:.3}% over a cell", vover * 100.0, sover * 100.0);
        assert!(vmean <= 0.005 && vover <= 0.001, "vector {vmean} {vover}");
        assert!(smean >= 10.0 * vmean && sover >= 0.01, "scalar {smean} {sover}");
    }

    /// Review fix (Stage 3c): a placement that reflects (a linked mirror
    /// copy reflects by its matrix) winds the simple mesh's faces the
    /// other way, so they face out with their normals: the mesh encloses
    /// a positive volume and each triangle's winding agrees with its
    /// vertex normals; the Poly's projected surface keeps its normal
    /// outward too.
    #[test]
    fn reflected_placements_face_out() {
        let c = noise_sphere();
        let p = projected(&c, &cube_sphere(6, 0.5), 1, REACH_EDGES * mean_edge(&cube_sphere(6, 0.5)));
        let rest = crate::layers::composed_clay(&c).build_mesh();
        let progress = crate::progress::Progress::default();
        for to_clay in [Mat4::IDENTITY, Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)), Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * Mat4::from_translation(Vec3::X * 0.1)] {
            let low = Low::new(&p, 1, to_clay, &rest, 512, &progress).unwrap();
            let pts: Vec<Vec3> = low.mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
            let tris: Vec<[u32; 3]> = low.mesh.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
            let volume = crate::mesh_asset::volume(&pts, &tris);
            assert!(volume > 0.3, "{to_clay}: volume {volume}");
            let agree = tris.iter().filter(|t| {
                let n = (pts[t[1] as usize] - pts[t[0] as usize]).cross(pts[t[2] as usize] - pts[t[0] as usize]);
                n.dot(Vec3::from(low.mesh.vertices[t[0] as usize].normal)) > 0.0
            }).count();
            assert_eq!(agree, tris.len(), "{to_clay}: every triangle winds with its normals");
            let (q, n) = low.surface_point(0, [0.2, 0.3, 0.5]).unwrap();
            assert!(n.dot(q - to_clay.transform_point3(Vec3::ZERO)) > 0.0, "{to_clay}: the projected surface's normal points out");
        }
    }

    /// Review fix (Stage 3c): each vertex of the simple mesh stands for its
    /// nearest rest vertex, so a split rig's texels read their own part's
    /// field (not part 0's) and a rigged clay's game mesh carries skin
    /// weights (it had none).
    #[test]
    fn poly_bakes_keep_parts_and_skin() {
        let c = noise_sphere();
        let composed = crate::layers::composed_clay(&c);
        let rest = composed.build_mesh();
        let p = cube_sphere(6, 0.5);
        let progress = crate::progress::Progress::default();
        let low = Low::new(&p, 1, Mat4::IDENTITY, &rest, 512, &progress).unwrap();
        assert_eq!(low.rest_vertex.len(), low.mesh.vertices.len());
        for (v, &r) in low.mesh.vertices.iter().zip(&low.rest_vertex).step_by(37) {
            let q = Vec3::from(v.pos);
            let best = rest.vertices.iter().map(|w| Vec3::from(w.pos).distance(q)).fold(f32::MAX, f32::min);
            assert!((Vec3::from(rest.vertices[r].pos).distance(q) - best).abs() < 1e-6, "the nearest rest vertex");
        }
        // A skin whose joint is the rest vertex's half (x < 0 or not).
        let joints: Vec<[u16; 4]> = rest.vertices.iter().map(|v| { let mut j = [0u16; 4]; j[0] = (v.pos[0] < 0.0) as u16; j }).collect();
        let weights = vec![{ let mut w = [0.0f32; 4]; w[0] = 1.0; w }; rest.vertices.len()];
        let skin = crate::rig::Skin { joints: joints.clone(), weights };
        let src = Source { clay: &composed, rest: &rest, skin: Some(&skin), parts: &[], library: &[], fallback: "", fine: None, low: Some(&low) };
        let settings = crate::bake::Settings { texture: 512, lowpoly: Some(crate::bake::LowPoly { poly: 1, level: 1 }), ..Default::default() };
        let (mesh, baked_skin) = crate::bake::bake(&src, settings, &progress).unwrap();
        let baked_skin = baked_skin.expect("a rigged clay's Poly bake is skinned");
        assert_eq!(baked_skin.joints.len(), mesh.vertices.len());
        for (i, j) in baked_skin.joints.iter().enumerate() {
            assert_eq!(*j, joints[low.rest_vertex[i]]);
        }
        let left = baked_skin.joints.iter().filter(|j| j[0] == 1).count();
        assert!(left > 0 && left < mesh.vertices.len(), "both halves");
    }
}
