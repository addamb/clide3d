//! Binary FBX (7.x), Geometry nodes only: `Objects/Geometry/Vertices`
//! (doubles or floats) and `PolygonVertexIndex` (a negative index, bit
//! flipped, ends each polygon), scaled to meters by GlobalSettings'
//! UnitScaleFactor (centimeters per unit, 1 when missing). Model
//! transforms are not applied: each geometry stays in its own space.
//! ASCII FBX is refused.

use std::io::Read;

use glam::Vec3;

use super::{Cursor, Soup};
use crate::mesh_asset::MAX_TRIANGLES;

const MAGIC: &[u8] = b"Kaydara FBX Binary  \x00";
/// Nodes nest at most this deep.
const MAX_DEPTH: usize = 32;
/// Most bytes one property array may inflate to.
const MAX_ARRAY_BYTES: usize = 512 << 20;

/// A property value, as far as geometry needs it.
enum Prop {
    Number(f64),
    Text(String),
    Floats(Vec<f64>),
    Ints(Vec<i64>),
    Other,
}

struct Node {
    name: String,
    props: Vec<Prop>,
    children: Vec<Node>,
}

pub fn read(bytes: &[u8]) -> Result<Soup, String> {
    if !bytes.starts_with(MAGIC) {
        if bytes.starts_with(b"; FBX") {
            return Err("ASCII FBX is not read; export binary FBX".into());
        }
        return Err("not a binary FBX file".into());
    }
    let mut c = Cursor::new(bytes);
    c.take(23)?;
    let version = c.u32()?;
    let wide = version >= 7500;
    let mut top = Vec::new();
    let mut budget = 0usize;
    while let Some(n) = node(&mut c, wide, 0, bytes.len(), &mut budget)? {
        top.push(n);
    }
    // GlobalSettings / Properties70 / P "UnitScaleFactor" ... value.
    let unit = top.iter().filter(|n| n.name == "GlobalSettings").flat_map(|n| &n.children).filter(|n| n.name == "Properties70").flat_map(|n| &n.children)
        .find(|p| matches!(p.props.first(), Some(Prop::Text(t)) if t == "UnitScaleFactor"))
        .and_then(|p| p.props.iter().rev().find_map(|v| if let Prop::Number(x) = v { Some(*x) } else { None }))
        .filter(|u| u.is_finite() && *u > 0.0)
        .unwrap_or(1.0);
    let scale = (unit / 100.0) as f32;
    let mut soup = Soup::default();
    let mut geometries = 0;
    for g in top.iter().filter(|n| n.name == "Objects").flat_map(|n| &n.children).filter(|n| n.name == "Geometry") {
        let child = |name: &str| g.children.iter().find(|c| c.name == name).and_then(|c| c.props.first());
        let (Some(Prop::Floats(v)), Some(Prop::Ints(index))) = (child("Vertices"), child("PolygonVertexIndex")) else { continue };
        if v.len() % 3 != 0 {
            return Err("a geometry's vertex list is not in threes".into());
        }
        geometries += 1;
        let base = soup.points.len() as i64;
        for p in v.chunks_exact(3) {
            soup.point(Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) * scale)?;
        }
        let count = (v.len() / 3) as i64;
        let mut corners = Vec::new();
        for &i in index {
            let (i, last) = if i < 0 { (!i, true) } else { (i, false) };
            if i >= count {
                return Err(format!("a polygon index {i} is past the geometry's {count} vertices"));
            }
            corners.push((base + i) as u32);
            if corners.len() > 1 << 16 {
                return Err("a polygon with more than 65536 corners".into());
            }
            if last {
                soup.polygon(&corners)?;
                corners.clear();
            }
        }
    }
    if geometries == 0 {
        return Err("the FBX holds no Geometry with Vertices and PolygonVertexIndex".into());
    }
    Ok(soup)
}

/// One node record (None at the empty record that ends a list).
/// A node ending at or before `limit` (its parent's end, or the file's).
fn node(c: &mut Cursor, wide: bool, depth: usize, limit: usize, budget: &mut usize) -> Result<Option<Node>, String> {
    if depth > MAX_DEPTH {
        return Err(format!("nodes nest deeper than {MAX_DEPTH}"));
    }
    if c.left() == 0 {
        return Ok(None);
    }
    let start = c.at;
    let word = |c: &mut Cursor| -> Result<u64, String> { if wide { c.u64() } else { c.u32().map(u64::from) } };
    let end = word(c)?;
    let count = word(c)?;
    let _list_len = word(c)?;
    let name_len = c.u8()? as usize;
    if end == 0 {
        return Ok(None);
    }
    let end = usize::try_from(end).map_err(|_| "a node ends past the file")?;
    if end <= start || end > limit {
        return Err("a node ends outside its parent or the file".into());
    }
    let name = String::from_utf8_lossy(c.take(name_len)?).into_owned();
    // Each property takes at least two bytes.
    if count > end.saturating_sub(c.at) as u64 / 2 + 1 {
        return Err("a node has more properties than bytes".into());
    }
    let mut props = Vec::new();
    for _ in 0..count {
        props.push(prop(c, budget)?);
    }
    let mut children = Vec::new();
    while c.at < end {
        match node(c, wide, depth + 1, end, budget)? {
            Some(n) => children.push(n),
            None => break,
        }
    }
    // Never rewinds: no byte is parsed twice.
    if c.at > end {
        return Err("a node's contents run past its end".into());
    }
    c.at = end;
    Ok(Some(Node { name, props, children }))
}

fn prop(c: &mut Cursor, budget: &mut usize) -> Result<Prop, String> {
    let kind = c.u8()?;
    Ok(match kind {
        b'Y' => Prop::Number(i16::from_le_bytes(c.array()?) as f64),
        b'C' => Prop::Number(c.u8()? as f64),
        b'I' => Prop::Number(i32::from_le_bytes(c.array()?) as f64),
        b'F' => Prop::Number(c.f32()? as f64),
        b'D' => Prop::Number(f64::from_le_bytes(c.array()?)),
        b'L' => Prop::Number(i64::from_le_bytes(c.array()?) as f64),
        b'S' | b'R' => {
            let n = c.u32()? as usize;
            let s = c.take(n)?;
            if kind == b'S' { Prop::Text(String::from_utf8_lossy(s).into_owned()) } else { Prop::Other }
        }
        b'f' | b'd' | b'l' | b'i' | b'b' => {
            let len = c.u32()? as usize;
            let encoding = c.u32()?;
            let packed = c.u32()? as usize;
            let width = match kind { b'd' | b'l' => 8, b'f' | b'i' => 4, _ => 1 };
            let want = len.checked_mul(width).filter(|&w| w <= MAX_ARRAY_BYTES).ok_or("an FBX array is too large")?;
            // Arrays inflate within one budget over the file.
            *budget = budget.checked_add(want).filter(|&b| b <= 3 * MAX_ARRAY_BYTES).ok_or("the FBX arrays inflate past the import limit")?;
            if width == 4 && len / 3 > 4 * MAX_TRIANGLES {
                return Err("an FBX array is too large".into());
            }
            let data = c.take(packed)?;
            let raw = match encoding {
                0 => data.to_vec(),
                1 => {
                    let mut out = Vec::with_capacity(want.min(64 << 20));
                    flate2::read::ZlibDecoder::new(data).take(want as u64 + 1).read_to_end(&mut out).map_err(|e| format!("an FBX array does not inflate: {e}"))?;
                    out
                }
                _ => return Err(format!("unknown FBX array encoding {encoding}")),
            };
            if raw.len() != want {
                return Err("an FBX array has the wrong size".into());
            }
            match kind {
                b'd' => Prop::Floats(raw.chunks_exact(8).map(|b| f64::from_le_bytes(b.try_into().expect("8"))).collect()),
                b'f' => Prop::Floats(raw.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().expect("4")) as f64).collect()),
                b'i' => Prop::Ints(raw.chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().expect("4")) as i64).collect()),
                b'l' => Prop::Ints(raw.chunks_exact(8).map(|b| i64::from_le_bytes(b.try_into().expect("8"))).collect()),
                _ => Prop::Other,
            }
        }
        other => return Err(format!("unknown FBX property type {:?}", other as char)),
    })
}
