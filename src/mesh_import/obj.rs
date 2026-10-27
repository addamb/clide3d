//! Wavefront OBJ: `v` points and `f` polygons (`i`, `i/t`, `i//n`,
//! `i/t/n`; negative indices count back from the last point). Everything
//! else (normals, groups, materials, curves) is skipped.

use glam::Vec3;

use super::Soup;

pub fn read(bytes: &[u8]) -> Result<Soup, String> {
    let mut soup = Soup::default();
    let mut corners: Vec<u32> = Vec::new();
    let mut skipped = 0usize;
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let Ok(line) = std::str::from_utf8(line) else {
            skipped += 1;
            continue;
        };
        let line = line.split('#').next().unwrap_or_default().trim();
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("v") => {
                let mut c = [0.0f32; 3];
                for v in &mut c {
                    *v = words.next().and_then(|w| w.parse().ok()).ok_or_else(|| format!("line {}: a vertex needs x y z", n + 1))?;
                }
                soup.point(Vec3::from(c)).map_err(|e| format!("line {}: {e}", n + 1))?;
            }
            Some("f") => {
                corners.clear();
                let count = soup.points.len() as i64;
                for w in words {
                    let first = w.split('/').next().unwrap_or_default();
                    let i: i64 = first.parse().map_err(|_| format!("line {}: bad face index {w}", n + 1))?;
                    let at = if i < 0 { count + i } else { i - 1 };
                    if !(0..count).contains(&at) {
                        return Err(format!("line {}: face index {i} is outside the {count} vertices so far", n + 1));
                    }
                    corners.push(at as u32);
                    if corners.len() > 1 << 16 {
                        return Err(format!("line {}: a face with more than 65536 corners", n + 1));
                    }
                }
                soup.polygon(&corners).map_err(|e| format!("line {}: {e}", n + 1))?;
            }
            _ => {}
        }
    }
    if skipped > 0 {
        soup.warnings.push(format!("{skipped} lines that are not UTF-8 were skipped"));
    }
    Ok(soup)
}

/// Longest `t crease` chain read (points).
pub const MAX_CREASE_CHAIN: usize = 64;

/// An OBJ's polygons as they are (`import_mesh as: poly`, `export_poly`'s
/// verify): every `v` in file order (unused ones kept, so indices stay
/// those of the file), faces with their sides (not fanned), a UV per
/// corner when every face corner names a `vt` (none when none does; a
/// mix is refused), v flipped to the top-left origin Polys use (in f64,
/// exact for what `export_poly` wrote), and creases from OpenSubdiv's
/// `t crease 2/1/0 i j sharpness` tags (0-based; the older `2/1` too,
/// and chains `N/1/0 i j k ... sharpness` of up to `MAX_CREASE_CHAIN`
/// points, each pair an edge). Counts are checked against the Poly
/// bounds before anything is kept for them. Other tags and normals are
/// skipped (named in the warnings).
pub fn read_cage(bytes: &[u8]) -> Result<(crate::poly::export::Cage, Vec<String>), String> {
    read_cage_as(bytes, None)
}

/// `read_cage`, its UVs a UDIM layout when `udim` says so (None: when the
/// file carries the `uv::UDIM_MARK` comment `export_poly` writes; an
/// ordinary file's UVs outside 0-1 tile, and read as plain 1 - v).
pub fn read_cage_as(bytes: &[u8], udim: Option<bool>) -> Result<(crate::poly::export::Cage, Vec<String>), String> {
    use crate::poly::{MAX_CAGE_CORNERS, MAX_CAGE_FACES, MAX_CAGE_POINTS, MAX_CREASES, MAX_SIDES};
    if bytes.len() as u64 > super::MAX_FILE_BYTES {
        return Err("the file is too large".into());
    }
    let mut points: Vec<Vec3> = Vec::new();
    // u, and v as written (flipped once all are read: a UDIM layout flips
    // tile by tile, `uv::v_in`).
    let mut uvs: Vec<(f32, f64)> = Vec::new();
    let mut face_start = vec![0u32];
    let mut corners: Vec<u32> = Vec::new();
    let mut corner_uv: Vec<u32> = Vec::new();
    let (mut with_uv, mut without_uv) = (0usize, 0usize);
    let mut creases: Vec<(u32, u32, f32)> = Vec::new();
    let mut skipped_tags = 0usize;
    let mut boundary_other = false;
    let mut marked = false;
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let at = |e: String| format!("line {}: {e}", n + 1);
        let line = std::str::from_utf8(line).map_err(|_| at("not UTF-8".into()))?;
        if line.trim_start().strip_prefix('#').is_some_and(|c| c.trim_start().starts_with(crate::poly::uv::UDIM_MARK)) {
            marked = true;
        }
        let line = line.split('#').next().unwrap_or_default().trim();
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("v") => {
                if points.len() >= MAX_CAGE_POINTS {
                    return Err(at(format!("more than {MAX_CAGE_POINTS} points")));
                }
                let mut c = [0.0f32; 3];
                for v in &mut c {
                    *v = words.next().and_then(|w| w.parse().ok()).ok_or_else(|| at("a vertex needs x y z".into()))?;
                }
                points.push(Vec3::from(c));
            }
            Some("vt") => {
                if uvs.len() >= MAX_CAGE_CORNERS {
                    return Err(at(format!("more than {MAX_CAGE_CORNERS} texture coordinates")));
                }
                let u: f32 = words.next().and_then(|w| w.parse().ok()).ok_or_else(|| at("a texture coordinate needs u v".into()))?;
                let v: f64 = words.next().and_then(|w| w.parse().ok()).ok_or_else(|| at("a texture coordinate needs u v".into()))?;
                uvs.push((u, v));
            }
            Some("f") => {
                if face_start.len() > MAX_CAGE_FACES {
                    return Err(at(format!("more than {MAX_CAGE_FACES} faces")));
                }
                let (count, tcount) = (points.len() as i64, uvs.len() as i64);
                let resolve = |w: &str, count: i64, what: &str| -> Result<u32, String> {
                    let i: i64 = w.parse().map_err(|_| format!("bad {what} index {w}"))?;
                    let k = if i < 0 { count + i } else { i - 1 };
                    if !(0..count).contains(&k) {
                        return Err(format!("{what} index {i} is outside the {count} so far"));
                    }
                    Ok(k as u32)
                };
                let start = corners.len();
                for w in words {
                    if corners.len() - start >= MAX_SIDES {
                        return Err(at(format!("a face with more than {MAX_SIDES} sides")));
                    }
                    if corners.len() >= MAX_CAGE_CORNERS {
                        return Err(at(format!("more than {MAX_CAGE_CORNERS} face corners")));
                    }
                    let mut parts = w.split('/');
                    corners.push(resolve(parts.next().unwrap_or_default(), count, "vertex").map_err(at)?);
                    match parts.next().filter(|t| !t.is_empty()) {
                        Some(t) => {
                            corner_uv.push(resolve(t, tcount, "texture").map_err(at)?);
                            with_uv += 1;
                        }
                        None => {
                            corner_uv.push(0);
                            without_uv += 1;
                        }
                    }
                }
                if corners.len() - start < 3 {
                    return Err(at("a face needs at least 3 corners".into()));
                }
                face_start.push(corners.len() as u32);
            }
            Some("t") => {
                // A tag and its counts (`ints/floats/strings`, OpenSubdiv's;
                // files before Stage 4a wrote `ints/floats`). Words are
                // taken only as the counts say, so a long line keeps
                // nothing past them; a known tag with more is refused.
                let (tag, counts) = (words.next().unwrap_or_default(), words.next().unwrap_or_default());
                let parts: Vec<&str> = counts.split('/').take(4).collect();
                let old = parts.len() == 2;
                let ints: Option<usize> = parts.first().and_then(|n| n.parse().ok());
                let known = matches!(parts.as_slice(), [_, "1"] | [_, "1", "0"] | [_, "0"] | [_, "0", "0"]);
                match (tag, ints, known) {
                    ("crease", Some(n), true) if parts[1] == "1" => {
                        if !(2..=MAX_CREASE_CHAIN).contains(&n) {
                            return Err(at(format!("a crease chain has 2 to {MAX_CREASE_CHAIN} points, not {n}")));
                        }
                        let w: Vec<&str> = words.by_ref().take(n + 2).collect();
                        if w.len() != n + 1 {
                            return Err(at(format!("t crease {counts} needs {n} points and a sharpness")));
                        }
                        let points: Vec<u32> = w[..n].iter().map(|x| x.parse().map_err(|_| at(format!("bad crease point {x}")))).collect::<Result<_, _>>()?;
                        let s: f32 = w[n].parse().map_err(|_| at(format!("bad crease sharpness {}", w[n])))?;
                        for pair in points.windows(2) {
                            let (a, b) = (pair[0], pair[1]);
                            if creases.len() >= MAX_CREASES {
                                return Err(at(format!("more than {MAX_CREASES} creases")));
                            }
                            if a == b || !(s.is_finite() && s > 0.0) {
                                return Err(at(format!("a crease needs two different points and a positive sharpness ({a} {b} {s})")));
                            }
                            creases.push((a.min(b), a.max(b), s.min(crate::poly::INFINITE_SHARPNESS)));
                        }
                    }
                    ("interpolateboundary", Some(1), true) if parts[1] == "0" => {
                        let w: Vec<&str> = words.by_ref().take(2).collect();
                        if w.len() != 1 {
                            return Err(at("t interpolateboundary needs one value".into()));
                        }
                        // Clide3D's rule is edge-and-corner (RenderMan's 1,
                        // or 2 in the old two-count tag); another is read
                        // as that, with a warning.
                        if w[0] != if old { "2" } else { "1" } {
                            boundary_other = true;
                        }
                    }
                    _ => skipped_tags += 1,
                }
            }
            _ => {}
        }
    }
    if face_start.len() < 2 {
        return Err("the file holds no faces".into());
    }
    if with_uv > 0 && without_uv > 0 {
        return Err(format!("{with_uv} face corners name a texture coordinate and {without_uv} do not; a cage's UVs are all or none"));
    }
    if let Some(&(a, b, _)) = creases.iter().find(|c| c.1 as usize >= points.len()) {
        return Err(format!("a crease ({a}, {b}) names a point past the {} points", points.len()));
    }
    creases.sort_by_key(|c| (c.0, c.1));
    if creases.windows(2).any(|w| (w[0].0, w[0].1) == (w[1].0, w[1].1)) {
        return Err("an edge is creased twice".into());
    }
    let mut warnings = Vec::new();
    if boundary_other {
        warnings.push("the file asks for another boundary rule; Polys subdivide borders edge-and-corner (interpolateboundary 1)".into());
    }
    if skipped_tags > 0 {
        warnings.push(format!("{skipped_tags} tags other than t crease and t interpolateboundary were skipped"));
    }
    let udim = udim.unwrap_or(marked);
    let uv = (with_uv > 0).then(|| corner_uv.iter().map(|&t| uvs[t as usize]).map(|(u, v)| [u, crate::poly::uv::v_in(v, udim)]).collect());
    Ok((crate::poly::export::Cage { points, face_start, corners, uv, creases }, warnings))
}
