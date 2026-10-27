//! Export of a Poly's subdivision cage and maps (docs/QUALITY_PLAN.md
//! Stage 3d, `export_poly`): the files a studio pipeline takes, so another
//! renderer's Catmull-Clark plus displacement reproduces the sculpt.
//!
//! - `Cage`: the geometry written, a level of the Poly with no
//!   displacement (level 0 is the cage itself). A finer level is the
//!   cage subdivided (`Refiner`), its creases those of that level (each
//!   cage crease less one per level, infinite ones kept), its UVs the
//!   face-varying linear ones (`uv::at_level`): Catmull-Clark is closed
//!   under this, so subdividing the written level further gives what
//!   subdividing the cage would.
//! - OBJ (`obj`, read back by `mesh_import::obj::read_cage`): `v` per
//!   point (every cage index, so crease indices stay valid), `vt` per
//!   corner (the UVs are face-varying), `vn` per point, polygon `f`
//!   lines (quads kept), and creases as OpenSubdiv's OBJ tags
//!   `t crease 2/1/0 i j sharpness` with 0-based point indices. Floats are
//!   written in Rust's shortest round-trip form, so points come back bit
//!   for bit; `vt` has v up (OBJ's bottom-left origin: 1 - v of the
//!   glTF-style top-left UVs maps and bakes use, written and read in f64
//!   so the flip is exact).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use std::path::{Path, PathBuf};

use glam::{Mat3, Mat4, Vec3};

use super::subdiv::Refiner;
use super::{INFINITE_SHARPNESS, Level, MAX_CREASES, MAX_SHOWN_FACES, Poly};

/// A level of a Poly as written: points, CSR faces, a UV per corner
/// (top-left origin, as `Poly::uv`) and creases (lower point first).
#[derive(Clone, Debug, PartialEq)]
pub struct Cage {
    pub points: Vec<Vec3>,
    pub face_start: Vec<u32>,
    pub corners: Vec<u32>,
    pub uv: Option<Vec<[f32; 2]>>,
    pub creases: Vec<(u32, u32, f32)>,
}

impl Cage {
    /// Level `level` of `poly` with no displacement (bounded by
    /// `MAX_SHOWN_FACES` from the counts, before anything is built).
    pub fn of(poly: &Poly, level: u8) -> Result<Cage, String> {
        if poly.faces_at(level) > MAX_SHOWN_FACES {
            return Err(format!("level {level} has {} faces; export_poly writes at most {MAX_SHOWN_FACES} (a lower level)", poly.faces_at(level)));
        }
        if level == 0 {
            let (face_start, corners) = poly.cage_faces();
            return Ok(Cage {
                points: poly.cage.to_vec(),
                face_start,
                corners,
                uv: poly.uv.as_ref().map(|u| u.to_vec()),
                creases: poly.creases.iter().map(|(&(a, b), &s)| (a, b, s)).collect(),
            });
        }
        let mut bare = poly.clone();
        bare.set_levels(bare.level_count().max(level))?;
        for l in &mut bare.levels {
            *l = Level::default();
        }
        let r = Refiner::new(&bare, level)?;
        let points = r.points(bare.cage.to_vec(), &bare.levels);
        let mut creases: Vec<(u32, u32, f32)> = r.top.edges.iter().zip(&r.top.sharpness).filter(|(_, s)| **s > 0.0).map(|(e, &s)| (e[0].min(e[1]), e[0].max(e[1]), if s.is_finite() { s.min(INFINITE_SHARPNESS) } else { INFINITE_SHARPNESS })).collect();
        creases.sort_by_key(|c| (c.0, c.1));
        if creases.len() > MAX_CREASES {
            return Err(format!("level {level} has {} crease edges; a file may hold at most {MAX_CREASES} (write a coarser level: the cage's creases decay as the renderer subdivides)", creases.len()));
        }
        Ok(Cage { points, face_start: r.top.face_start.clone(), corners: r.top.corners.clone(), uv: super::uv::at_level(&bare, level), creases })
    }

    pub fn faces(&self) -> usize {
        self.face_start.len() - 1
    }

    /// A Poly of this cage (checked by `Poly::new`; creases checked as a
    /// saved file's are, and as edges of the cage).
    pub fn to_poly(&self) -> Result<Poly, String> {
        let mut p = Poly::new(&self.points, &self.face_start, &self.corners)?;
        if self.creases.len() > MAX_CREASES {
            return Err(format!("more than {MAX_CREASES} creases"));
        }
        let t = p.topology();
        let mut creases = BTreeMap::new();
        for &(a, b, s) in &self.creases {
            if a >= b || !(s > 0.0 && s <= INFINITE_SHARPNESS) {
                return Err(format!("a crease ({a}, {b}, {s}) is not a pair of points in order with sharpness in (0, {INFINITE_SHARPNESS}]"));
            }
            if t.edge_between(a, b).is_none() {
                return Err(format!("a crease ({a}, {b}) is not an edge of the cage"));
            }
            if creases.insert((a, b), s).is_some() {
                return Err(format!("an edge is creased twice ({a}, {b})"));
            }
        }
        p.creases = Arc::new(creases);
        if let Some(uv) = &self.uv {
            if uv.len() != self.corners.len() {
                return Err(format!("{} UVs for {} corners", uv.len(), self.corners.len()));
            }
            if uv.iter().flatten().any(|v| !v.is_finite() || v.abs() > super::uv::MAX_UV) {
                return Err(format!("a UV is not finite or beyond {}", super::uv::MAX_UV));
            }
            p.uv = Some(super::Chunked::new(uv));
        }
        Ok(p)
    }

    /// Area-weighted point normals (OBJ `vn`; renderers subdividing the
    /// cage ignore them).
    fn normals(&self) -> Vec<Vec3> {
        super::subdiv::frames(&self.points, &self.face_start, &self.corners).into_iter().map(|f| f[2]).collect()
    }
}

/// The first line of every OBJ `export_poly` writes (an existing file
/// starting so may be replaced).
pub const OBJ_HEADER: &str = "# Clide3D export_poly";

/// The cage as OBJ text (see the module doc). `notes` are extra comment
/// lines after the header.
pub fn obj(cage: &Cage, name: &str, notes: &[String]) -> Vec<u8> {
    let corners = cage.corners.len();
    let mut out = String::with_capacity(cage.points.len() * 72 + corners * 40 + 256);
    let _ = writeln!(out, "{OBJ_HEADER}: {} points, {} faces, Catmull-Clark subdivision cage", cage.points.len(), cage.faces());
    out.push_str("# creases: t crease 2/1/0 i j sharpness (0-based point indices, OpenSubdiv's OBJ tags; sharpness 10 is infinitely sharp)\n");
    out.push_str("# vt: v up (bottom-left origin); vn: area-weighted point normals of this level\n");
    for n in notes {
        for line in comment_lines(n) {
            let _ = writeln!(out, "# {line}");
        }
    }
    let _ = writeln!(out, "o {}", name.chars().filter(|c| !c.is_whitespace() && !c.is_control() && *c != '#').collect::<String>());
    for p in &cage.points {
        let _ = writeln!(out, "v {} {} {}", p.x, p.y, p.z);
    }
    if let Some(uv) = &cage.uv {
        // UDIM layouts flip v tile by tile (`uv::v_out`).
        let udim = super::uv::spans_tiles(uv);
        if udim {
            let _ = writeln!(out, "# {} (v flipped tile by tile: v in tile row t is written 2t + 1 - v)", super::uv::UDIM_MARK);
        }
        for t in uv {
            let _ = writeln!(out, "vt {} {}", t[0], super::uv::v_out(t[1], udim));
        }
    }
    for n in cage.normals() {
        let _ = writeln!(out, "vn {} {} {}", n.x, n.y, n.z);
    }
    for f in 0..cage.faces() {
        out.push('f');
        for c in cage.face_start[f] as usize..cage.face_start[f + 1] as usize {
            let v = cage.corners[c] + 1;
            if cage.uv.is_some() {
                let _ = write!(out, " {v}/{}/{v}", c + 1);
            } else {
                let _ = write!(out, " {v}//{v}");
            }
        }
        out.push('\n');
    }
    // OpenSubdiv's OBJ tags count their ints, floats and strings
    // (`nint/nfloat/nstring`); the boundary rule is RenderMan's (1 is
    // edge-and-corner, 2 edge-only).
    out.push_str("t interpolateboundary 1/0/0 1\n");
    for &(a, b, s) in &cage.creases {
        let _ = writeln!(out, "t crease 2/1/0 {a} {b} {s}");
    }
    out.into_bytes()
}

/// Text for comment lines: split at every line break (a lone `\r` too)
/// and with other control characters dropped, so a name or note can
/// never start a line of its own in any format.
pub fn comment_lines(text: &str) -> Vec<String> {
    text.split(['\n', '\r']).filter(|l| !l.is_empty()).map(|l| l.chars().filter(|c| !c.is_control()).collect()).collect()
}


/// The triangles a .glb holds for a cage level: one vertex per (point,
/// UV), each face fanned from its first corner (as `low::Low` and the
/// bake fan it), the level's area-weighted point normals, and tangents
/// from the UVs (`bake::tangents`, as the bake's normal map assumes).
pub fn cage_mesh(cage: &Cage) -> crate::mesh::Mesh {
    use crate::mesh::{Mesh, Vertex};
    let normals = cage.normals();
    let zero = [0.0f32; 2];
    let uv_of = |c: usize| cage.uv.as_ref().map_or(zero, |u| u[c]);
    let mut keys: Vec<(u32, [u32; 2], u32)> = (0..cage.corners.len() as u32).map(|c| (cage.corners[c as usize], uv_of(c as usize).map(f32::to_bits), c)).collect();
    keys.sort_unstable();
    let mut vert_of = vec![0u32; cage.corners.len()];
    let mut mesh = Mesh::default();
    for (i, k) in keys.iter().enumerate() {
        if i == 0 || (k.0, k.1) != (keys[i - 1].0, keys[i - 1].1) {
            let n = normals[k.0 as usize].normalize_or(Vec3::Y);
            mesh.vertices.push(Vertex { pos: cage.points[k.0 as usize].to_array(), normal: n.to_array(), color: [1.0; 3], surface: crate::mesh::OBJECT_SURFACE, material: 0, uv: uv_of(k.2 as usize), tangent: [0.0; 4], light: [0.0; 2], base: [0.0; 4], wear: [0.0; 3], painted: [0; 4] });
        }
        vert_of[k.2 as usize] = mesh.vertices.len() as u32 - 1;
    }
    for w in cage.face_start.windows(2) {
        let a = w[0] as usize;
        for j in 1..(w[1] - w[0]) as usize - 1 {
            mesh.indices.extend_from_slice(&[vert_of[a], vert_of[a + j], vert_of[a + j + 1]]);
        }
    }
    if cage.uv.is_some() {
        crate::bake::tangents(&mut mesh);
    }
    mesh
}

/// What `check_glb` found: counts, and the cage in `extras.clide3d_cage`.
pub struct GlbCheck {
    pub summary: serde_json::Value,
    pub cage: Option<Cage>,
}

/// Checks a .glb's structure the way a validator would, so a broken file
/// is caught before a pipeline reads it: the header and chunk lengths;
/// every buffer view inside the buffer; every accessor's elements inside
/// its view, its `min`/`max` (where given) the data's bounds; each
/// primitive's attributes of one count, POSITION with bounds, indices
/// whole triangles within the vertices; images, textures, materials,
/// nodes and scenes pointing at what exists. Then reads the cage in the
/// first mesh's `extras.clide3d_cage` (counts checked against its views
/// before anything is kept for them).
pub fn check_glb(bytes: &[u8], progress: &crate::progress::Progress) -> Result<GlbCheck, String> {
    use serde_json::{Value, json};
    progress.check()?;
    let u32_at = |i: usize| -> Result<usize, String> { bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| "the file is cut short".to_string()) };
    if !bytes.starts_with(b"glTF") || u32_at(4)? != 2 {
        return Err("not a glTF 2.0 binary".into());
    }
    if u32_at(8)? != bytes.len() {
        return Err(format!("the header says {} bytes, the file has {}", u32_at(8)?, bytes.len()));
    }
    let jlen = u32_at(12)?;
    if bytes.get(16..20) != Some(b"JSON") || 20 + jlen > bytes.len() || !jlen.is_multiple_of(4) {
        return Err("the JSON chunk is missing, misaligned or too long".into());
    }
    let doc: Value = serde_json::from_slice(&bytes[20..20 + jlen]).map_err(|e| format!("the JSON chunk does not parse: {e}"))?;
    let b0 = 20 + jlen;
    let blen = u32_at(b0)?;
    if bytes.get(b0 + 4..b0 + 8) != Some(b"BIN\0") || b0 + 8 + blen != bytes.len() || !blen.is_multiple_of(4) {
        return Err("the BIN chunk is missing, misaligned or not the rest of the file".into());
    }
    let bin = &bytes[b0 + 8..];
    let list = |k: &str| doc[k].as_array().cloned().unwrap_or_default();
    let (buffers, views, accessors, meshes) = (list("buffers"), list("bufferViews"), list("accessors"), list("meshes"));
    let (images, textures, materials, nodes, samplers) = (list("images"), list("textures"), list("materials"), list("nodes"), list("samplers"));
    // A crafted file could point thousands of accessors and primitives at
    // one large view: the counts are bounded, and the bytes the bounds
    // and index checks read add up to at most twice the BIN chunk (what
    // export_poly writes reads each byte at most once).
    if accessors.len() > MAX_GLB_ITEMS || views.len() > MAX_GLB_ITEMS || meshes.len() > MAX_GLB_ITEMS {
        return Err(format!("more than {MAX_GLB_ITEMS} accessors, buffer views or meshes"));
    }
    let primitives: usize = meshes.iter().map(|m| m["primitives"].as_array().map_or(0, Vec::len)).sum();
    if primitives > MAX_GLB_ITEMS {
        return Err(format!("more than {MAX_GLB_ITEMS} primitives"));
    }
    let budget = std::cell::Cell::new(2 * bin.len() + (1 << 20));
    let charge = |n: usize| -> Result<(), String> {
        progress.check()?;
        let left = budget.get().checked_sub(n).ok_or("the accessors read the same bytes over and over (more than twice the file's data); refused")?;
        budget.set(left);
        Ok(())
    };
    let index = |v: &Value, n: usize, what: &str| -> Result<usize, String> { v.as_u64().map(|i| i as usize).filter(|&i| i < n).ok_or_else(|| format!("{what} {v} does not exist (of {n})")) };
    let buffer_len = buffers.first().and_then(|b| b["byteLength"].as_u64()).ok_or("no buffer 0 with byteLength")? as usize;
    if buffers.len() != 1 || buffer_len > bin.len() || bin.len() - buffer_len >= 4 {
        return Err(format!("buffer 0 is {buffer_len} bytes against a BIN chunk of {}", bin.len()));
    }
    let mut spans = Vec::with_capacity(views.len());
    for (i, v) in views.iter().enumerate() {
        index(&v["buffer"], 1, "buffer")?;
        let (off, len) = (v["byteOffset"].as_u64().unwrap_or(0) as usize, v["byteLength"].as_u64().ok_or(format!("bufferView {i} has no byteLength"))? as usize);
        if off.checked_add(len).is_none_or(|e| e > buffer_len) {
            return Err(format!("bufferView {i} ({off} + {len}) passes the buffer's {buffer_len} bytes"));
        }
        spans.push((off, len));
    }
    let size_of = |c: u64| match c {
        5120 | 5121 => Some(1usize),
        5122 | 5123 => Some(2),
        5125 | 5126 => Some(4),
        _ => None,
    };
    let width_of = |t: &str| match t {
        "SCALAR" => Some(1usize),
        "VEC2" => Some(2),
        "VEC3" => Some(3),
        "VEC4" => Some(4),
        "MAT4" => Some(16),
        _ => None,
    };
    // Each accessor's element bytes, checked within its view.
    let mut acc: Vec<(usize, usize, u64, usize)> = Vec::with_capacity(accessors.len());
    for (i, a) in accessors.iter().enumerate() {
        let view = index(&a["bufferView"], spans.len(), "bufferView")?;
        let comp = a["componentType"].as_u64().unwrap_or(0);
        let size = size_of(comp).ok_or(format!("accessor {i} has componentType {comp}"))?;
        let width = a["type"].as_str().and_then(width_of).ok_or(format!("accessor {i} has type {}", a["type"]))?;
        let count = a["count"].as_u64().filter(|&c| c >= 1).ok_or(format!("accessor {i} needs a count of at least 1"))? as usize;
        let off = a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let (voff, vlen) = spans[view];
        if views[view].get("byteStride").is_some() {
            return Err(format!("accessor {i}: strided views are not written here"));
        }
        if count.checked_mul(width * size).and_then(|n| n.checked_add(off)).is_none_or(|e| e > vlen) {
            return Err(format!("accessor {i} ({count} x {width} x {size} bytes from {off}) passes bufferView {view}'s {vlen} bytes"));
        }
        let start = voff + off;
        acc.push((start, count, comp, width));
        if comp == 5126 && (a.get("min").is_some() || a.get("max").is_some()) {
            charge(count * width * 4)?;
            let floats: Vec<f32> = bin[start..start + count * width * 4].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            let (lo, hi) = (a["min"].as_array().cloned().unwrap_or_default(), a["max"].as_array().cloned().unwrap_or_default());
            if lo.len() != width || hi.len() != width {
                return Err(format!("accessor {i}: min and max need {width} values"));
            }
            for c in 0..width {
                let (mut l, mut h) = (f32::MAX, f32::MIN);
                for e in floats.iter().skip(c).step_by(width) {
                    (l, h) = (l.min(*e), h.max(*e));
                }
                // Written as f32's shortest decimal: compared as f32.
                if lo[c].as_f64().map(|x| x as f32) != Some(l) || hi[c].as_f64().map(|x| x as f32) != Some(h) {
                    return Err(format!("accessor {i}: min/max {} {} are not the data's bounds ({l}, {h}) in component {c}", lo[c], hi[c]));
                }
            }
        }
    }
    let (mut vertices, mut triangles) = (0usize, 0usize);
    for (mi, m) in meshes.iter().enumerate() {
        for (pi, p) in m["primitives"].as_array().cloned().unwrap_or_default().iter().enumerate() {
            let attrs = p["attributes"].as_object().ok_or(format!("mesh {mi} primitive {pi} has no attributes"))?;
            let pos = attrs.get("POSITION").ok_or(format!("mesh {mi} primitive {pi} has no POSITION"))?;
            let pa = index(pos, acc.len(), "accessor")?;
            if accessors[pa].get("min").is_none() {
                return Err(format!("mesh {mi}: POSITION needs min and max"));
            }
            let n = acc[pa].1;
            for (k, v) in attrs {
                let a = index(v, acc.len(), "accessor")?;
                if acc[a].1 != n {
                    return Err(format!("mesh {mi} primitive {pi}: {k} has {} elements, POSITION {n}", acc[a].1));
                }
            }
            if let Some(ix) = p.get("indices") {
                let a = index(ix, acc.len(), "accessor")?;
                let (start, count, comp, width) = acc[a];
                if width != 1 || !count.is_multiple_of(3) || !matches!(comp, 5121 | 5123 | 5125) {
                    return Err(format!("mesh {mi} primitive {pi}: indices are not whole triangles of unsigned integers"));
                }
                let size = size_of(comp).expect("checked");
                charge(count * size)?;
                let read = |e: usize| -> usize {
                    let b = &bin[start + e * size..start + (e + 1) * size];
                    match size {
                        1 => b[0] as usize,
                        2 => u16::from_le_bytes([b[0], b[1]]) as usize,
                        _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize,
                    }
                };
                if let Some(e) = (0..count).find(|&e| read(e) >= n) {
                    return Err(format!("mesh {mi} primitive {pi}: index {} at {e} is past the {n} vertices", read(e)));
                }
                triangles += count / 3;
            }
            if let Some(mat) = p.get("material") {
                index(mat, materials.len(), "material")?;
            }
            vertices = vertices.max(n);
        }
    }
    for (i, im) in images.iter().enumerate() {
        let v = index(&im["bufferView"], spans.len(), "bufferView")?;
        let (off, _) = spans[v];
        if im["mimeType"] != "image/png" || !bin[off..].starts_with(b"\x89PNG") {
            return Err(format!("image {i} is not a PNG in its view"));
        }
    }
    for t in &textures {
        index(&t["source"], images.len(), "image")?;
        if let Some(s) = t.get("sampler") {
            index(s, samplers.len(), "sampler")?;
        }
    }
    for m in &materials {
        let pbr = &m["pbrMetallicRoughness"];
        for t in [&pbr["baseColorTexture"], &pbr["metallicRoughnessTexture"], &m["normalTexture"], &m["occlusionTexture"]] {
            if !t.is_null() {
                index(&t["index"], textures.len(), "texture")?;
            }
        }
    }
    for n in &nodes {
        if let Some(m) = n.get("mesh") {
            index(m, meshes.len(), "mesh")?;
        }
    }
    for sc in list("scenes") {
        for n in sc["nodes"].as_array().cloned().unwrap_or_default() {
            index(&n, nodes.len(), "node")?;
        }
    }
    // The cage.
    let x = &meshes.first().map(|m| m["extras"]["clide3d_cage"].clone()).unwrap_or(Value::Null);
    let cage = if x.is_null() {
        None
    } else {
        let floats = |a: &Value, width: usize| -> Result<Vec<f32>, String> {
            let i = index(a, acc.len(), "accessor")?;
            let (start, count, comp, w) = acc[i];
            if comp != 5126 || w != width {
                return Err("a cage accessor has the wrong type".into());
            }
            Ok(bin[start..start + count * width * 4].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
        };
        let words = |v: &Value, per: usize| -> Result<&[u8], String> {
            let view = index(&v["bufferView"], spans.len(), "bufferView")?;
            let count = v["count"].as_u64().ok_or("a cage view has no count")? as usize;
            let (off, len) = spans[view];
            if count.checked_mul(per).is_none_or(|n| n != len) {
                return Err("a cage view's count does not fit its bytes".into());
            }
            Ok(&bin[off..off + len])
        };
        let u32s = |b: &[u8]| -> Vec<u32> { b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect() };
        let points: Vec<Vec3> = floats(&x["points"], 3)?.chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
        let sizes = u32s(words(&x["face_sizes"], 4)?);
        let corners = u32s(words(&x["corners"], 4)?);
        if sizes.len() > super::MAX_CAGE_FACES || corners.len() > super::MAX_CAGE_CORNERS || points.len() > super::MAX_CAGE_POINTS {
            return Err("the cage is past the Poly bounds".into());
        }
        let mut face_start = Vec::with_capacity(sizes.len() + 1);
        face_start.push(0u32);
        let mut at = 0u64;
        for s in &sizes {
            at += *s as u64;
            face_start.push(at.min(u32::MAX as u64) as u32);
        }
        if at != corners.len() as u64 {
            return Err("the cage's face sizes do not add up to its corners".into());
        }
        let uv = match x.get("uv") {
            Some(a) => Some(floats(a, 2)?.chunks_exact(2).map(|c| [c[0], c[1]]).collect()),
            None => None,
        };
        let creases = match x.get("creases") {
            Some(v) => words(v, 12)?.chunks_exact(12).map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), u32::from_le_bytes([c[4], c[5], c[6], c[7]]), f32::from_le_bytes([c[8], c[9], c[10], c[11]]))).collect(),
            None => Vec::new(),
        };
        Some(Cage { points, face_start, corners, uv, creases })
    };
    Ok(GlbCheck { summary: json!({ "bytes": bytes.len(), "meshes": meshes.len(), "vertices": vertices, "triangles": triangles, "accessors": accessors.len(), "buffer_views": views.len(), "images": images.len(), "materials": materials.len() }), cage })
}

/// Most accessors, buffer views, meshes or primitives `check_glb` reads
/// (export_poly writes about a dozen).
pub const MAX_GLB_ITEMS: usize = 4096;

/// The geometry file's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geometry {
    Obj,
    Glb,
    Usda,
}

impl Geometry {
    pub fn of(path: &Path) -> Result<Geometry, String> {
        match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("obj") => Ok(Geometry::Obj),
            Some("glb") => Ok(Geometry::Glb),
            Some("usda") => Ok(Geometry::Usda),
            _ => Err("path must be an absolute file path ending in .obj, .glb or .usda".into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Geometry::Obj => "obj",
            Geometry::Glb => "glb",
            Geometry::Usda => "usda",
        }
    }

    /// Whether `head` (a file's first bytes) is a file this writer made
    /// (or could have): only those are replaced.
    fn is(self, head: &[u8]) -> bool {
        match self {
            Geometry::Obj => head.starts_with(OBJ_HEADER.as_bytes()),
            // Only a file this writer made: its maker named in the JSON
            // chunk (a .glb) or the layer's doc (a .usda), as the OBJ's
            // header line.
            Geometry::Glb => head.starts_with(b"glTF") && contains(head, GENERATOR.as_bytes()),
            Geometry::Usda => head.starts_with(usda::HEADER.as_bytes()) && contains(head, format!("doc = \"{GENERATOR}.").as_bytes()),
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// A map `export_poly` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MapKind {
    VectorDisplacement,
    Displacement,
    Normal,
    Color,
    Ao,
}

impl MapKind {
    pub const ALL: [MapKind; 5] = [MapKind::VectorDisplacement, MapKind::Displacement, MapKind::Normal, MapKind::Color, MapKind::Ao];

    pub fn name(self) -> &'static str {
        match self {
            MapKind::VectorDisplacement => "vector_displacement",
            MapKind::Displacement => "displacement",
            MapKind::Normal => "normal",
            MapKind::Color => "color",
            MapKind::Ao => "ao",
        }
    }

    pub fn parse(s: &str) -> Option<MapKind> {
        MapKind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// Made by the texture bake (8-bit PNG), not the displacement bake.
    fn textured(self) -> bool {
        matches!(self, MapKind::Normal | MapKind::Color | MapKind::Ao)
    }
}

/// The vector map's space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Tangent,
    Object,
}

impl Space {
    pub fn name(self) -> &'static str {
        match self {
            Space::Tangent => "tangent",
            Space::Object => "object",
        }
    }
}

/// Map sizes `export_poly` bakes (`bake::MAX_MAP_SIZE` bounds a vector
/// map's floats: 4096² of them is 192 MB, plus the bake's working set).
pub const MAP_SIZES: [u32; 4] = [512, 1024, 2048, 4096];

/// What the sidecar says made it (an existing sidecar is replaced only
/// when it says so).
pub const GENERATOR: &str = "Clide3D export_poly";
/// How every sidecar starts (keys are sorted, so `generator` is first):
/// an existing file is replaced only when it starts so.
const SIDECAR_HEAD: &str = "{\n  \"generator\": \"Clide3D export_poly\"";

/// A checked `export_poly` call.
#[derive(Clone, Debug)]
pub struct Request {
    pub path: PathBuf,
    pub geometry: Geometry,
    /// The level written as the geometry (0 is the cage).
    pub level: u8,
    pub maps: Vec<MapKind>,
    /// The level the maps sit on (undisplaced): a renderer subdivides the
    /// written level `map_level - level` more times and adds the maps.
    pub map_level: u8,
    pub map_size: u32,
    pub vector_format: crate::outputs::Format,
    pub height_format: crate::outputs::Format,
    pub space: Space,
    pub verify: bool,
    /// `udim: true`: the UV layout's tiles (a map file per tile) and the
    /// texture channels written per tile.
    pub udim: Option<UdimFiles>,
}

/// What a UDIM export writes besides the geometry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UdimFiles {
    /// The tiles the written UVs use (maps are baked per tile).
    pub tiles: Vec<u16>,
    /// The Poly's texture channels: (channel, resolution, tiles with
    /// texels).
    pub textures: Vec<(crate::poly::texture::Channel, u32, Vec<u16>)>,
}

impl Request {
    fn stem(&self) -> String {
        self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("poly").to_string()
    }

    /// The sidecar: the geometry's path with `.json`.
    pub fn sidecar(&self) -> PathBuf {
        self.path.with_extension("json")
    }

    fn map_format(&self, k: MapKind) -> crate::outputs::Format {
        match k {
            MapKind::VectorDisplacement => self.vector_format,
            MapKind::Displacement => self.height_format,
            _ => crate::outputs::Format::Png,
        }
    }

    /// Map `k`'s file: `<stem>_<map>.<ext>` beside the geometry.
    pub fn map_path(&self, k: MapKind) -> PathBuf {
        self.path.with_file_name(format!("{}_{}.{}", self.stem(), k.name(), self.map_format(k).ext()))
    }

    /// A UDIM map's file for `tile`: `<stem>.<map>.<UDIM>.<ext>`.
    pub fn map_tile_path(&self, k: MapKind, tile: u16) -> PathBuf {
        self.path.with_file_name(format!("{}.{}.{tile}.{}", self.stem(), k.name(), self.map_format(k).ext()))
    }

    /// A texture channel's file for `tile`: `<stem>.<channel>.<UDIM>.png`.
    pub fn texture_path(&self, c: crate::poly::texture::Channel, tile: u16) -> PathBuf {
        texture_file(&self.path, c, tile)
    }

    /// Refuses before any work when a file this call would write holds
    /// something else (a typo must not overwrite a document): the
    /// geometry only over this writer's kind of file, the sidecar only
    /// over a sidecar this writer made, maps only over images of their
    /// kind (`outputs::writable`). Empty files may go.
    pub fn check_paths(&self) -> Result<(), String> {
        if !self.path.is_absolute() || self.path.file_stem().is_none() {
            return Err("path must be an absolute file path ending in .obj, .glb or .usda".into());
        }
        if !self.path.parent().is_some_and(|d| d.is_dir()) {
            return Err(format!("{}: the folder does not exist", self.path.display()));
        }
        if self.path.file_name().and_then(|f| f.to_str()).is_none_or(|f| f.contains('@') || f.chars().any(char::is_control)) {
            return Err("path: a file name with @ (USD's asset quote) or a control character cannot be referenced from every format; rename it".into());
        }
        crate::outputs::overwritable_by(&self.path, &format!("export_poly .{}", self.geometry.name()), 1 << 16, |h| self.geometry.is(h))?;
        crate::outputs::overwritable(&self.sidecar(), "export_poly sidecar", |h| h.starts_with(SIDECAR_HEAD.as_bytes()))?;
        match &self.udim {
            None => {
                for &k in &self.maps {
                    crate::outputs::writable(&self.map_path(k), crate::outputs::Kind::of(self.map_format(k)))?;
                }
            }
            // Every tile's file, before any work (the same fail-closed rule).
            Some(u) => {
                for &k in &self.maps {
                    for &t in &u.tiles {
                        crate::outputs::writable(&self.map_tile_path(k, t), crate::outputs::Kind::of(self.map_format(k)))?;
                    }
                }
                for (c, _, tiles) in &u.textures {
                    for &t in tiles {
                        crate::outputs::writable(&self.texture_path(*c, t), crate::outputs::Kind::Png)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// A texture tile's file beside `geometry`: `<stem>.<channel>.<UDIM>.png`
/// (the UDIM convention: 1001 + u + 10 v).
pub fn texture_file(geometry: &Path, c: crate::poly::texture::Channel, tile: u16) -> PathBuf {
    let stem = geometry.file_stem().and_then(|s| s.to_str()).unwrap_or("poly");
    geometry.with_file_name(format!("{stem}.{}.{tile}.png", c.name()))
}

/// The Poly's placement in its clay's space, when it is a similarity
/// that keeps handedness: `to_clay = translate · scale · rotation`.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub to_clay: Mat4,
    pub scale: f32,
    pub rotation: Mat3,
}

impl Placement {
    /// Refused when the placement shears, scales unevenly or reflects:
    /// maps would then need a frame per texel that no renderer rebuilds.
    pub fn of(to_clay: Mat4) -> Result<Placement, String> {
        let l = Mat3::from_mat4(to_clay);
        let det = l.determinant();
        if !(det.is_finite() && det > 0.0) || !to_clay.is_finite() {
            return Err("maps need the Poly placed like its clay up to a move, a turn and an even scale; this placement reflects or collapses (a mirror copy): export the Poly it mirrors".into());
        }
        let scale = det.cbrt();
        let rotation = l * (1.0 / scale);
        let cols = [rotation.x_axis, rotation.y_axis, rotation.z_axis];
        let ok = cols.iter().all(|c| (c.length() - 1.0).abs() <= 1e-4) && cols[0].dot(cols[1]).abs() <= 1e-4 && cols[1].dot(cols[2]).abs() <= 1e-4 && cols[0].dot(cols[2]).abs() <= 1e-4;
        if !ok {
            return Err("maps need the Poly placed like its clay up to a move, a turn and an even scale; it is scaled unevenly or sheared relative to the clay: scale it evenly".into());
        }
        Ok(Placement { to_clay, scale, rotation })
    }

    /// A vector map value from the clay's units to the Poly's.
    fn to_poly(&self, v: Vec3, space: Space) -> Vec3 {
        match space {
            Space::Tangent => v / self.scale,
            Space::Object => self.rotation.transpose() * v / self.scale,
        }
    }

    /// A vector map value from the Poly's units to the clay's.
    fn to_clay(&self, v: Vec3, space: Space) -> Vec3 {
        match space {
            Space::Tangent => v * self.scale,
            Space::Object => self.rotation * v * self.scale,
        }
    }
}

/// What the maps are baked from: the source clay as a bake reads it (no
/// simple mesh: the export makes the Poly's level), the placement, and
/// the clay's id for the reply.
pub struct MapSource<'a> {
    pub src: crate::bake::Source<'a>,
    pub place: Placement,
}

/// Most triangles `verify` dices the maps' level into (as `bake verify`).
pub const VERIFY_TRIANGLES: usize = 4_000_000;
/// Most diced points `verify` measures against the shown level.
const VERIFY_SHOWN_SAMPLES: usize = 100_000;

fn r4(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// One displacement map as written (Poly units): its values and, for a
/// 16-bit PNG, the code range.
struct Written {
    values: Vec<f32>,
    channels: usize,
    scale: Option<f32>,
}

/// Most bytes of a sidecar `read_textures` reads.
const MAX_SIDECAR_BYTES: u64 = 4 << 20;

/// A texture tile's file, read only when it is a regular file (not a
/// link) no larger than an uncompressed 16-bit RGBA PNG of `res` could be
/// (its pixels plus a row filter byte each and 1 MiB of chunks), through
/// a bounded read.
fn read_tile(path: &Path, res: u32) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let limit = tile_file_limit(res);
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a regular file (a link, folder or device is not read)", path.display()));
    }
    if meta.len() > limit {
        return Err(format!("{}: {} bytes, more than a {res}-texel 16-bit tile can take ({limit})", path.display(), meta.len()));
    }
    // Opened without following a link (one swapped in after the check is
    // refused too).
    let file = crate::outputs::open_regular(path)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{}: grew past {limit} bytes while it was read", path.display()));
    }
    Ok(bytes)
}

/// The most bytes a tile file of `res` texels may have.
pub fn tile_file_limit(res: u32) -> u64 {
    let r = res as u64;
    r * (r * 8 + 1) + (1 << 20)
}

/// The texture tiles an `export_poly udim: true` sidecar beside
/// `geometry` lists (None without a sidecar, or one that lists none),
/// read back exactly: every channel, resolution, tile and file name is
/// checked against what the writer makes (a file beside the geometry,
/// named `<stem>.<channel>.<UDIM>.png`) and each PNG's header against
/// the resolution before its pixels are decoded; the pages kept are
/// bounded by `texture::MAX_PAGES` as they are added.
pub fn read_textures(geometry: &Path, progress: &crate::progress::Progress) -> Result<Option<crate::poly::texture::Textures>, String> {
    use crate::poly::texture::{Channel, MAX_PAGES, MAX_TILES, Textures, check_resolution, valid_tile};
    let side = geometry.with_extension("json");
    let Ok(meta) = std::fs::symlink_metadata(&side) else { return Ok(None) };
    if !meta.is_file() || meta.len() > MAX_SIDECAR_BYTES {
        return Err(format!("{}: the sidecar is not a regular file of at most {} MiB", side.display(), MAX_SIDECAR_BYTES >> 20));
    }
    let text = std::fs::read(&side).map_err(|e| format!("{}: {e}", side.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", side.display()))?;
    if v["generator"].as_str() != Some(GENERATOR) {
        return Ok(None);
    }
    let Some(list) = v["udim"]["textures"].as_object().filter(|l| !l.is_empty()) else { return Ok(None) };
    let mut out = Textures::default();
    let mut pages = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for (name, entry) in list {
        let c = Channel::parse(name).ok_or_else(|| format!("{}: unknown texture channel {name}", side.display()))?;
        let res = entry["resolution"].as_u64().filter(|r| *r <= 8192).ok_or_else(|| format!("{}: {name} has no resolution", side.display()))? as u32;
        check_resolution(res)?;
        let tiles = entry["tiles"].as_array().filter(|t| t.len() <= MAX_TILES as usize).ok_or_else(|| format!("{}: {name} lists no tiles (at most {MAX_TILES})", side.display()))?;
        out.set_resolution(c, res)?;
        for t in tiles {
            progress.check()?;
            let t = t.as_u64().filter(|t| *t <= u16::MAX as u64).map(|t| t as u16).filter(|t| valid_tile(*t)).ok_or_else(|| format!("{}: {name} lists a tile that is not 1001-1100", side.display()))?;
            if !seen.insert((c, t)) {
                return Err(format!("{}: {name} lists tile {t} twice", side.display()));
            }
            let path = texture_file(geometry, c, t);
            let listed = entry["files"][t.to_string()].as_str();
            if listed != path.file_name().and_then(|f| f.to_str()) {
                return Err(format!("{}: {name} tile {t} names {listed:?}, not {}", side.display(), path.display()));
            }
            let bytes = read_tile(&path, res)?;
            let codes = crate::outputs::read_png16_sized(&bytes, res as usize, res as usize, 4).map_err(|e| format!("{}: {e}", path.display()))?;
            let texels: &[[u16; 4]] = bytemuck::cast_slice(&codes);
            pages += Textures::pages_of(res, texels);
            if pages > MAX_PAGES {
                return Err(format!("the texture tiles hold more than {MAX_PAGES} pages ({} GiB)", crate::poly::texture::MAX_BYTES >> 30));
            }
            out.put_tile(c, t, texels);
        }
    }
    Ok(Some(out))
}

/// Writes one float map (Poly units) as `format` at `path` and describes
/// it for the sidecar (a 16-bit PNG's code range is `w.scale`).
fn write_values(path: &Path, format: crate::outputs::Format, size: usize, w: &Written) -> Result<serde_json::Value, String> {
    use crate::outputs::Format;
    match format {
        Format::Exr => {
            let names: &[&str] = if w.channels == 3 { &["R", "G", "B"] } else { &["Y"] };
            let channels = names.iter().enumerate().map(|(c, n)| (*n, w.values.iter().skip(c).step_by(w.channels).copied().collect())).collect();
            crate::outputs::write_exr(path, size, size, channels)?;
        }
        Format::Png => {
            let codes = crate::bake::Displacement::encode16(w.values.iter().copied(), w.scale.unwrap_or(0.0));
            crate::outputs::write_over(path, "Png", |h| crate::outputs::Kind::Png.is(h), &crate::outputs::png16(size, size, w.channels, &codes)?)?;
        }
    }
    let max = w.values.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    Ok(serde_json::json!({
        "file": path.file_name().and_then(|f| f.to_str()).unwrap_or_default(),
        "format": format.ext(),
        "channels": if w.channels == 3 { "R G B" } else { "Y" },
        "bits": if format == Format::Exr { 32 } else { 16 },
        "scale": w.scale,
        "decode": if format == Format::Exr { "float values as they are, in the Poly's units".to_string() } else { "value = (code / 65535 - 0.5) * scale, in the Poly's units; mid grey (32768) is 0".to_string() },
        "max": max,
    }))
}

/// Writes the geometry, the maps and the sidecar for `req` (see the
/// module doc), then checks them from disk (`verify`). `name` names the
/// object in the files; `world` is its world matrix (the sidecar's);
/// `own` its material (a GLB's). Maps need `maps` (and UVs on the Poly:
/// the caller refuses first). The reply lists the files, counts,
/// subdivision conventions, map scales and the verify result.
pub fn export(poly: &Poly, name: &str, world: Mat4, own: &crate::material::Material, req: &Request, maps: Option<&MapSource>, progress: &crate::progress::Progress) -> Result<serde_json::Value, String> {
    use crate::bake::{LowPoly, Maps, Settings};
    use crate::outputs::Format;
    use serde_json::json;
    let timer = std::time::Instant::now();
    let laps = std::cell::RefCell::new(Vec::<(String, f64)>::new());
    let lap = |name: &str| {
        let mut l = laps.borrow_mut();
        let done: f64 = l.iter().map(|x| x.1).sum();
        l.push((name.to_string(), timer.elapsed().as_secs_f64() - done));
    };
    progress.stage("the cage", 0.0);
    let cage = Cage::of(poly, req.level)?;
    progress.check()?;
    let wants = |k: MapKind| req.maps.contains(&k);
    let mut textures: Option<Arc<crate::mesh::Textures>> = None;
    let mut written: BTreeMap<MapKind, Written> = BTreeMap::new();
    let (mut missed, mut from_field) = (0usize, 0usize);
    let mut notes: Vec<String> = Vec::new();
    // UDIM maps: written tile by tile as they are baked (one tile's floats
    // held at a time), entries by map then tile, and each tile's PNG
    // code range for the verify.
    let mut tile_maps: BTreeMap<MapKind, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    let mut tile_scales: BTreeMap<(MapKind, u16), Option<f32>> = BTreeMap::new();
    if let (false, Some(u)) = (req.maps.is_empty(), &req.udim) {
        let m = maps.ok_or("maps need the clay the Poly was made from")?;
        progress.range(0.02, 0.1);
        progress.stage("the maps' level", 0.0);
        let low = super::low::Low::new_udim(poly, req.map_level, m.place.to_clay, m.src.rest, progress)?;
        lap("maps_level");
        let n = u.tiles.len().max(1) as f32;
        for (i, &t) in u.tiles.iter().enumerate() {
            progress.range(0.1 + 0.7 * i as f32 / n, 0.1 + 0.7 * (i + 1) as f32 / n);
            progress.stage(&format!("tile {t}"), 0.0);
            progress.check()?;
            let Some(lt) = low.tile(t) else { continue };
            let src = crate::bake::Source { low: Some(&lt), ..m.src };
            let d = crate::bake::displacement(&src, &lt.mesh, Maps { displacement: wants(MapKind::Displacement), vector: wants(MapKind::VectorDisplacement), size: req.map_size, object: req.space == Space::Object }, progress)?;
            (missed, from_field) = (missed + d.missed, from_field + d.from_field);
            let mut made: Vec<(MapKind, Written)> = Vec::new();
            if let Some(v) = &d.vector {
                made.push((MapKind::VectorDisplacement, Written { values: v.iter().flat_map(|c| m.place.to_poly(Vec3::from(*c), req.space).to_array()).collect(), channels: 3, scale: None }));
            }
            if let Some(h) = &d.height {
                made.push((MapKind::Displacement, Written { values: h.iter().map(|x| x / m.place.scale).collect(), channels: 1, scale: None }));
            }
            for (k, mut w) in made {
                if req.map_format(k) == Format::Png {
                    w.scale = Some(2.0 * w.values.iter().fold(0.0f32, |m, v| m.max(v.abs())));
                }
                let entry = write_values(&req.map_tile_path(k, t), req.map_format(k), req.map_size as usize, &w)?;
                tile_scales.insert((k, t), w.scale);
                tile_maps.entry(k).or_default().insert(t.to_string(), entry);
            }
        }
        lap("displacement_bake");
        if missed > 0 {
            notes.push(format!("{missed} texels found no clay surface and hold no offset (a larger projection reach, or a cage nearer the clay)"));
        }
    } else if !req.maps.is_empty() {
        let m = maps.ok_or("maps need the clay the Poly was made from")?;
        if poly.uv.is_none() {
            return Err("maps need UVs on the Poly: run poly uv first".into());
        }
        progress.range(0.02, 0.1);
        progress.stage("the maps' level", 0.0);
        let low = super::low::Low::new(poly, req.map_level, m.place.to_clay, m.src.rest, req.map_size, progress)?;
        let src = crate::bake::Source { low: Some(&low), ..m.src };
        lap("maps_level");
        if req.maps.iter().any(|k| k.textured()) {
            progress.range(0.1, 0.5);
            let settings = Settings { texture: req.map_size, lowpoly: Some(LowPoly { poly: 0, level: req.map_level }), ..Settings::default() };
            let (mesh, _) = crate::bake::bake(&src, settings, progress)?;
            textures = mesh.textures.clone();
            lap("texture_bake");
        }
        progress.check()?;
        if wants(MapKind::VectorDisplacement) || wants(MapKind::Displacement) {
            progress.range(0.5, 0.8);
            let d = crate::bake::displacement(&src, &low.mesh, Maps { displacement: wants(MapKind::Displacement), vector: wants(MapKind::VectorDisplacement), size: req.map_size, object: req.space == Space::Object }, progress)?;
            (missed, from_field) = (d.missed, d.from_field);
            lap("displacement_bake");
            if let Some(v) = &d.vector {
                let values: Vec<f32> = v.iter().flat_map(|c| m.place.to_poly(Vec3::from(*c), req.space).to_array()).collect();
                written.insert(MapKind::VectorDisplacement, Written { values, channels: 3, scale: None });
            }
            if let Some(h) = &d.height {
                written.insert(MapKind::Displacement, Written { values: h.iter().map(|x| x / m.place.scale).collect(), channels: 1, scale: None });
            }
            if missed > 0 {
                notes.push(format!("{missed} texels found no clay surface and hold no offset (a larger projection reach, or a cage nearer the clay)"));
            }
        }
    }
    progress.range(0.8, 0.9);
    progress.stage("writing", 0.0);
    progress.check()?;
    // Maps first, the geometry next, the sidecar last.
    let size = req.map_size as usize;
    // 16-bit PNG code ranges, each map its own (a height map keeps its
    // precision next to a vector map's larger offsets).
    for (&k, w) in written.iter_mut() {
        if req.map_format(k) == Format::Png {
            w.scale = Some(2.0 * w.values.iter().fold(0.0f32, |m, v| m.max(v.abs())));
        }
    }
    // Each map file is encoded on its own thread (EXR compression is the
    // slow part); the reply lists them in a fixed order.
    let write_map = |k: MapKind| -> Result<serde_json::Value, String> {
        let path = req.map_path(k);
        let file = path.file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
        Ok(match (k, written.get(&k)) {
            (_, Some(w)) => write_values(&path, req.map_format(k), size, w)?,
            (MapKind::Normal | MapKind::Color | MapKind::Ao, None) => {
                let t = textures.as_ref().ok_or("the texture bake made no textures")?;
                let bytes = match k {
                    MapKind::Normal => crate::bake::png(&t.normal, t.size, false)?,
                    MapKind::Color => crate::bake::png(&t.color, t.size, t.alpha)?,
                    _ => crate::outputs::png8(size, size, 1, &t.orm.chunks_exact(4).map(|p| p[0]).collect::<Vec<u8>>())?,
                };
                crate::outputs::write_over(&path, "Png", |h| crate::outputs::Kind::Png.is(h), &bytes)?;
                json!({
                    "file": file,
                    "format": "png",
                    "bits": 8,
                    "channels": match k { MapKind::Normal => "R G B", MapKind::Color => if t.alpha { "R G B A" } else { "R G B" }, _ => "Y" },
                    "decode": match k {
                        MapKind::Normal => "tangent-space normal, OpenGL style (+Y green toward the bitangent; see vector_frame); n = 2 * rgb / 255 - 1",
                        MapKind::Color => "sRGB base color",
                        _ => "ambient occlusion, 0 hidden to 255 open",
                    },
                })
            }
            _ => return Err(format!("the {} map was not made", k.name())),
        })
        .map(|mut v| {
            v["file"] = json!(file);
            v
        })
    };
    let mut map_files = serde_json::Map::new();
    if req.udim.is_some() {
        for (k, tiles) in &tile_maps {
            map_files.insert(k.name().into(), serde_json::Value::Object(tiles.clone()));
        }
    } else {
        let entries: Vec<Result<serde_json::Value, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = req.maps.iter().map(|&k| scope.spawn(move || write_map(k))).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a map writer panicked".into()))).collect()
        });
        for (k, entry) in req.maps.iter().zip(entries) {
            map_files.insert(k.name().into(), entry?);
        }
    }
    // The Poly's texture tiles (UDIM): one 16-bit RGBA PNG per tile with
    // texels, exactly the texels it holds, one tile in memory at a time.
    let mut texture_files = serde_json::Map::new();
    if let (Some(u), Some(paint)) = (&req.udim, poly.paint.as_ref()) {
        for (c, res, tiles) in &u.textures {
            let mut files = serde_json::Map::new();
            for &t in tiles {
                progress.check()?;
                let texels = paint.textures.tile_texels(*c, t).ok_or("a texture tile vanished")?;
                let path = req.texture_path(*c, t);
                crate::outputs::write_over(&path, "Png", |h| crate::outputs::Kind::Png.is(h), &crate::outputs::png16(*res as usize, *res as usize, 4, bytemuck::cast_slice(&texels))?)?;
                files.insert(t.to_string(), json!(path.file_name().and_then(|f| f.to_str())));
            }
            texture_files.insert(c.name().into(), json!({
                "resolution": res,
                "tiles": tiles,
                "files": files,
                "bits": 16,
                "channels": "R G B A",
                "decode": if *c == crate::poly::texture::Channel::Color { "sRGB colour in R G B, straight alpha: A is how much of it covers the material (code / 65535)" } else if *c == crate::poly::texture::Channel::Normal { "tangent-space normal of the tile's bake (OpenGL style, n = 2 * rgb / 65535 - 1), A 1 where baked" } else { "the value (code / 65535) in R, G and B alike, A how much of it covers the material" },
            }));
        }
        lap("textures");
    }
    progress.check()?;
    // A .glb carries the cage (level 0) beside the level it draws.
    let level0 = if req.geometry == Geometry::Glb && req.level > 0 { Some(Cage::of(poly, 0)?) } else { None };
    // The creases the file holds: a .glb's are its cage's (level 0).
    let file_creases = level0.as_ref().unwrap_or(&cage).creases.len();
    let geometry_bytes = match req.geometry {
        Geometry::Obj => obj(&cage, name, &[format!("level {} of {name}; maps and conventions: {}", req.level, req.sidecar().file_name().and_then(|f| f.to_str()).unwrap_or_default())]),
        Geometry::Glb => {
            let mut mesh = cage_mesh(&cage);
            mesh.textures = textures.clone();
            let extras = json!({
                "level_written": req.level,
                "scheme": "catmull-clark",
                "boundary": "edge-and-corner",
                "note": "the mesh is level level_written of this cage with no displacement; the cage (points, face_sizes and corners, per-corner uv with glTF's top-left origin, creases) subdivides to it; conventions and the displacement maps are in the sidecar",
                "sidecar": req.sidecar().file_name().and_then(|f| f.to_str()),
            });
            crate::gltf::poly_glb(name, &mesh, own, world, level0.as_ref().unwrap_or(&cage), extras)?
        }
        Geometry::Usda => {
            let file = |k: MapKind| req.map_path(k).file_name().and_then(|f| f.to_str()).unwrap_or_default().to_string();
            let mut tex = Vec::new();
            for &k in &req.maps {
                let one = |s: f32, b: f32| ([s, s, s, 1.0], [b, b, b, 0.0]);
                let (input, kind, output, raw, (scale, bias)) = match k {
                    MapKind::Displacement => {
                        let s = written.get(&k).and_then(|w| w.scale);
                        ("displacement", "float", "r", true, s.map_or(one(1.0, 0.0), |s| one(s, -0.5 * s)))
                    }
                    MapKind::Normal => ("normal", "normal3f", "rgb", true, one(2.0, -1.0)),
                    MapKind::Color => ("diffuseColor", "color3f", "rgb", false, one(1.0, 0.0)),
                    MapKind::Ao => ("occlusion", "float", "r", true, one(1.0, 0.0)),
                    // UsdPreviewSurface has no vector displacement input:
                    // the file sits beside it (the sidecar names it).
                    MapKind::VectorDisplacement => continue,
                };
                tex.push(usda::Texture { file: file(k), input, output, kind, raw, scale, bias });
            }
            let notes = format!("Level {} of {name}, a Catmull-Clark cage; maps and conventions in {}", req.level, req.sidecar().file_name().and_then(|f| f.to_str()).unwrap_or_default());
            usda::write(&cage, name, world, &tex, &notes)
        }
    };
    crate::outputs::write_over(&req.path, &format!("export_poly .{}", req.geometry.name()), |h| req.geometry.is(h), &geometry_bytes)?;
    let subdivision = json!({
        "scheme": "catmull-clark",
        "boundary": "edge-and-corner (OpenSubdiv's OBJ tag t interpolateboundary 1/0/0 1, RenderMan's 1: border edges sharp, a point in one face a corner)",
        "face_varying_uvs": "linear everywhere (OpenSubdiv's FVAR_LINEAR_ALL): a child corner's UV is its parent face's corner, edge middle or middle UV",
        "creases": file_creases,
        "crease_rule": "semi-sharp (DeRose et al. 1998): an edge of sharpness s stays sharp for s levels, each level's children keep s - 1, fractions blend the smooth and sharp rules; 10 is infinitely sharp",
        "crease_tags": match req.geometry {
            Geometry::Obj => "t crease 2/1/0 i j sharpness in the OBJ, 0-based point indices (OpenSubdiv's OBJ tags)",
            Geometry::Glb => "extras.clide3d_cage.creases of the mesh: (u32 i, u32 j, f32 sharpness) records over the cage's points",
            Geometry::Usda => "creaseIndices, creaseLengths (2 each) and creaseSharpnesses on the Mesh",
        },
        "triangles": "Catmull-Clark's rule for every face size (a triangle splits into 3 quads)",
    });
    let mut sidecar = json!({
        "generator": GENERATOR,
        "version": 1,
        "object": name,
        "geometry": {
            "file": req.path.file_name().and_then(|f| f.to_str()),
            "format": req.geometry.name(),
            "level": req.level,
            "points": cage.points.len(),
            "faces": cage.faces(),
            "corners": cage.corners.len(),
            "units": "the Poly's object units",
            "world_matrix": world.to_cols_array(),
            "axes": "+Y up, +Z front (glTF's)",
            "winding": "counter-clockwise seen from outside",
            "uv": if cage.uv.is_some() { "one per face corner (face-varying); in the OBJ v is up (origin bottom-left), so a map's first row is v = 1" } else { "none" },
        },
        "subdivision": subdivision,
    });
    if !req.maps.is_empty() {
        sidecar["maps"] = json!({
            "level": req.map_level,
            "subdivide_written_by": req.map_level - req.level,
            "size": req.map_size,
            "space": req.space.name(),
            "units": "the Poly's object units",
            "base": "the written level subdivided subdivide_written_by more times with no displacement, its UVs face-varying linear: the maps hold where the sculpt is from each of its surface points",
            "vector_frame": match req.space {
                Space::Tangent => "R along the tangent (toward +u), G along the bitangent cross(normal, tangent) * w (toward +v in the OBJ's bottom-left UVs, up the image; -v in glTF's top-left UVs), B along the normal; normals are the base level's area-weighted point normals interpolated over each triangle, tangents per point from the triangles' UV gradients (two triangles a quad, fanned from its first corner)",
                Space::Object => "R G B along the Poly's object x y z: no tangent frame to match",
            },
            "height": "the displacement map is the offset along the normal",
            "files": map_files,
        });
    }
    if let Some(u) = &req.udim {
        sidecar["udim"] = json!({
            "naming": "<stem>.<channel or map>.<UDIM>.<ext> beside the geometry; UDIM = 1001 + u + 10 v by the UVs' whole parts (v up, as the file's UVs are)",
            "tiles": u.tiles,
            "textures": texture_files,
        });
    }
    let side = serde_json::to_vec_pretty(&sidecar).map_err(|e| e.to_string())?;
    crate::outputs::write_over(&req.sidecar(), "export_poly sidecar", |h| h.starts_with(SIDECAR_HEAD.as_bytes()), &side)?;
    lap("writing");
    progress.range(0.9, 1.0);
    let mut reply = json!({
        "files": {
            "geometry": req.path.display().to_string(),
            "sidecar": req.sidecar().display().to_string(),
            "maps": match &req.udim {
                None => req.maps.iter().map(|k| (k.name().to_string(), json!(req.map_path(*k).display().to_string()))).collect::<serde_json::Map<_, _>>(),
                Some(_) => tile_maps.iter().map(|(k, t)| (k.name().to_string(), json!(t.keys().map(|tile| (tile.clone(), json!(req.map_tile_path(*k, tile.parse().unwrap_or(0)).display().to_string()))).collect::<serde_json::Map<_, _>>()))).collect(),
            },
            "textures": match &req.udim {
                None => serde_json::Map::new(),
                Some(u) => u.textures.iter().map(|(c, _, tiles)| (c.name().to_string(), json!(tiles.iter().map(|t| (t.to_string(), json!(req.texture_path(*c, *t).display().to_string()))).collect::<serde_json::Map<_, _>>()))).collect(),
            },
        },
        "format": req.geometry.name(),
        "level": req.level,
        "faces": cage.faces(),
        "points": cage.points.len(),
        "corners": cage.corners.len(),
        "uv": cage.uv.is_some(),
        "subdivision": { "scheme": "catmull-clark", "boundary": "edge-and-corner", "creases": file_creases },
        "bytes": geometry_bytes.len(),
    });
    if !req.maps.is_empty() {
        reply["maps"] = json!({
            "level": req.map_level,
            "size": req.map_size,
            "space": req.space.name(),
            "scale": written.iter().filter_map(|(k, w)| w.scale.map(|s| (k.name().to_string(), json!(s)))).collect::<serde_json::Map<_, _>>(),
            "max_displacement": written.values().flat_map(|w| w.values.iter()).fold(0.0f32, |m, v| m.max(v.abs())),
            "texels_from_field": from_field,
            "texels_missed": missed,
        });
    }
    if let Some(u) = &req.udim {
        reply["udim"] = json!({ "tiles": u.tiles, "texture_tiles": u.textures.iter().map(|(c, r, t)| (c.name().to_string(), json!({ "resolution": r, "tiles": t }))).collect::<serde_json::Map<_, _>>() });
    }
    if req.verify {
        progress.stage("verifying the files", 0.0);
        let (file_cage, file_level) = match (&level0, req.geometry) {
            (Some(c), _) => (c, 0),
            (None, Geometry::Glb) => (&cage, 0),
            (None, _) => (&cage, req.level),
        };
        reply["verify"] = verify(file_cage, file_level, req, maps, poly, &written, &tile_scales, progress)?;
        lap("verify");
    }
    reply["timings"] = laps.into_inner().into_iter().map(|(k, v)| (k, serde_json::json!((v * 100.0).round() / 100.0))).collect::<serde_json::Map<_, _>>().into();
    reply["notes"] = json!(notes);
    reply["seconds"] = json!((timer.elapsed().as_secs_f32() * 10.0).round() / 10.0);
    Ok(reply)
}

/// Reads the written files back, with no state from the scene: the
/// geometry's cage against the one written, then (with a displacement
/// map) that cage subdivided to the maps' level, the map from disk added
/// as `film::dice::apply_baked` adds it (diced to half a cell), and the
/// result measured against the clay and against the Poly's shown level,
/// in the clay's cells.
#[allow(clippy::too_many_arguments)]
fn verify(cage: &Cage, file_level: u8, req: &Request, maps: Option<&MapSource>, poly: &Poly, written: &BTreeMap<MapKind, Written>, tile_scales: &BTreeMap<(MapKind, u16), Option<f32>>, progress: &crate::progress::Progress) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let bytes = std::fs::read(&req.path).map_err(|e| format!("{}: {e}", req.path.display()))?;
    let mut glb = None;
    let back = match req.geometry {
        Geometry::Obj => crate::mesh_import::read_cage(&bytes)?.0,
        Geometry::Glb => {
            let check = check_glb(&bytes, progress)?;
            glb = Some(check.summary);
            check.cage.ok_or("the .glb holds no cage")?
        }
        Geometry::Usda => usda::read(&bytes)?,
    };
    let same_topology = back.face_start == cage.face_start && back.corners == cage.corners;
    let point_delta = if back.points.len() == cage.points.len() { back.points.iter().zip(&cage.points).map(|(a, b)| (*a - *b).abs().max_element()).fold(0.0f32, f32::max) } else { f32::INFINITY };
    let uv_delta = match (&back.uv, &cage.uv) {
        (Some(a), Some(b)) if a.len() == b.len() => a.iter().zip(b).map(|(x, y)| (x[0] - y[0]).abs().max((x[1] - y[1]).abs())).fold(0.0f32, f32::max),
        (None, None) => 0.0,
        _ => f32::INFINITY,
    };
    let mut out = json!({
        "cage": { "same_topology": same_topology, "max_point_delta": point_delta, "max_uv_delta": uv_delta, "same_creases": back.creases == cage.creases },
    });
    if let Some(g) = glb {
        out["glb"] = g;
    }
    let kind = [MapKind::VectorDisplacement, MapKind::Displacement].into_iter().find(|k| written.contains_key(k) || tile_scales.keys().any(|(t, _)| t == k));
    let (Some(kind), Some(m)) = (kind, maps) else { return Ok(out) };
    // The Poly's shown level (to measure against) is built meanwhile.
    std::thread::scope(|scope| {
        let shown_field = scope.spawn(|| -> Result<crate::mesh_asset::MeshField, String> {
            let shown = super::subdiv::evaluate(poly, poly.level)?;
            let spts: Vec<Vec3> = shown.points.iter().map(|p| m.place.to_clay.transform_point3(*p)).collect();
            Ok(crate::mesh_asset::MeshField::new(&spts, &shown.triangles()))
        });
        progress.check()?;
        // The cage from the file, subdivided to the maps' level: the whole
        // level with one map, each tile's triangles with its tile's map.
        let mut reread = back.to_poly()?;
        let extra = req.map_level - file_level;
        reread.set_levels(extra)?;
        let mut targets: Vec<(super::low::Low, PathBuf, Option<f32>)> = Vec::new();
        if req.udim.is_some() {
            let low = super::low::Low::new_udim(&reread, extra, m.place.to_clay, m.src.rest, progress)?;
            for (&(k, t), scale) in tile_scales {
                if k == kind
                    && let Some(lt) = low.tile(t)
                {
                    targets.push((lt, req.map_tile_path(kind, t), *scale));
                }
            }
        } else {
            let low = super::low::Low::new(&reread, extra, m.place.to_clay, m.src.rest, req.map_size, progress)?;
            targets.push((low, req.map_path(kind), written.get(&kind).and_then(|w| w.scale)));
        }
        let format = req.map_format(kind);
        let size = req.map_size as usize;
        let channels = if kind == MapKind::VectorDisplacement { 3 } else { 1 };
        let cell = m.src.clay.cell_size().ok_or("the clay is empty")?;
        let budget = VERIFY_TRIANGLES / targets.len().max(1);
        let mut pts: Vec<Vec3> = Vec::new();
        let mut triangles = 0usize;
        for (low, path, scale) in &targets {
            // The map from disk, back in the clay's units.
            let values: Vec<f32> = match format {
                crate::outputs::Format::Exr => {
                    let (w, h, list) = crate::outputs::read_exr(path)?;
                    if (w, h) != (size, size) {
                        return Err(format!("{} is {w} x {h}, not {size} x {size}", path.display()));
                    }
                    let names: &[&str] = if channels == 3 { &["R", "G", "B"] } else { &["Y"] };
                    let found: Vec<&Vec<f32>> = names.iter().map(|n| list.iter().find(|(c, _)| c == n).map(|(_, v)| v).ok_or_else(|| format!("{} has no channel {n}", path.display()))).collect::<Result<_, _>>()?;
                    (0..size * size).flat_map(|i| found.iter().map(move |c| c[i])).collect()
                }
                crate::outputs::Format::Png => {
                    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                    let codes = crate::outputs::read_png16_sized(&bytes, size, size, channels).map_err(|e| format!("{}: {e}", path.display()))?;
                    let scale = scale.ok_or("a PNG map has a scale")?;
                    codes.iter().map(|&k| crate::bake::Displacement::decode16(k, scale)).collect()
                }
            };
            let space = if channels == 3 { req.space } else { Space::Tangent };
            let mut disp = crate::bake::Displacement { size: req.map_size, height: None, vector: None, from_field: 0, missed: 0, object: space == Space::Object };
            if channels == 3 {
                disp.vector = Some(values.chunks_exact(3).map(|c| m.place.to_clay(Vec3::new(c[0], c[1], c[2]), space).to_array()).collect());
            } else {
                disp.height = Some(values.iter().map(|h| h * m.place.scale).collect());
            }
            progress.check()?;
            let diced = match crate::film::dice::apply_baked(&low.mesh, &disp, cell * 0.5, budget) {
                Ok(d) => d,
                Err(e) => {
                    out["maps"] = json!({ "skipped": e });
                    return Ok(out);
                }
            };
            triangles += diced.indices.len() / 3;
            pts.extend(diced.vertices.iter().map(|v| Vec3::from(v.pos)));
        }
        let probe = crate::quality::Probe::new(m.src.clay).ok_or("the clay is empty")?;
        let mut e = vec![0.0f32; pts.len()];
        crate::clay::par_chunks(&mut e, 4096, |i, chunk| {
            for (k, o) in chunk.iter_mut().enumerate() {
                let q = pts[i * 4096 + k];
                *o = probe.near(q, 4.0 * cell)(q).abs() / cell;
            }
        });
        let max = e.iter().copied().fold(0.0, f32::max);
        let mean = e.iter().sum::<f32>() / e.len().max(1) as f32;
        // Against the Poly's shown level (its projected detail), in the clay's
        // space: an evenly picked sample of the diced points.
        progress.check()?;
        let field = shown_field.join().map_err(|_| "measuring the shown level panicked".to_string())??;
        let step = pts.len().div_ceil(VERIFY_SHOWN_SAMPLES).max(1);
        let picked: Vec<Vec3> = pts.iter().step_by(step).copied().collect();
        let mut d = vec![0.0f32; picked.len()];
        crate::clay::par_chunks(&mut d, 1024, |i, chunk| {
            for (k, o) in chunk.iter_mut().enumerate() {
                *o = field.distance_within(picked[i * 1024 + k], 16.0 * cell, crate::mesh_asset::Sign::Closed).abs() / cell;
            }
        });
        let smax = d.iter().copied().fold(0.0, f32::max);
        let smean = d.iter().sum::<f32>() / d.len().max(1) as f32;
        out["maps"] = json!({
            "map": kind.name(),
            "from": "the geometry file's cage subdivided to the maps' level, the map read from its file and added as a renderer would (diced to half a cell)",
            "triangles": triangles,
            "tiles": if req.udim.is_some() { json!(targets.len()) } else { serde_json::Value::Null },
            "max_error_cells": r4(max),
            "mean_error_cells": r4(mean),
            "to_shown_level": { "level": poly.level, "max_cells": r4(smax), "mean_cells": r4(smean), "samples": picked.len() },
            "cell": cell,
        });
        Ok(out)
    })
}

pub mod usda;

#[cfg(test)]
pub(crate) mod tests;
