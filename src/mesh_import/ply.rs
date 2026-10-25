//! PLY 1.0: ASCII, binary_little_endian or binary_big_endian. The
//! `vertex` element's x, y, z and the `face` element's vertex_indices
//! (or vertex_index) list; other elements and properties are read past.

use glam::Vec3;

use super::Soup;
use crate::mesh_asset::{MAX_POINTS, MAX_TRIANGLES};

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

#[derive(Clone, Copy)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(name: &str) -> Result<Scalar, String> {
        Ok(match name {
            "char" | "int8" => Scalar::I8,
            "uchar" | "uint8" => Scalar::U8,
            "short" | "int16" => Scalar::I16,
            "ushort" | "uint16" => Scalar::U16,
            "int" | "int32" => Scalar::I32,
            "uint" | "uint32" => Scalar::U32,
            "float" | "float32" => Scalar::F32,
            "double" | "float64" => Scalar::F64,
            _ => return Err(format!("unknown PLY type {name}")),
        })
    }

    fn size(self) -> usize {
        match self {
            Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::F64 => 8,
        }
    }
}

struct Property {
    name: String,
    /// Some(count type) for a list.
    list: Option<Scalar>,
    kind: Scalar,
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

/// Reads values one by one in the file's encoding.
struct Values<'a> {
    bytes: &'a [u8],
    at: usize,
    enc: Encoding,
}

impl Values<'_> {
    fn next(&mut self, kind: Scalar) -> Result<f64, String> {
        if self.enc == Encoding::Ascii {
            while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let start = self.at;
            while self.at < self.bytes.len() && !self.bytes[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let word = std::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| "a PLY value is not text")?;
            return word.parse::<f64>().map_err(|_| if word.is_empty() { "the file is cut short".to_string() } else { format!("bad PLY value {word}") });
        }
        let n = kind.size();
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or("the file is cut short")?;
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(&self.bytes[self.at..end]);
        self.at = end;
        if self.enc == Encoding::Big {
            b[..n].reverse();
        }
        Ok(match kind {
            Scalar::I8 => b[0] as i8 as f64,
            Scalar::U8 => b[0] as f64,
            Scalar::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            Scalar::F64 => f64::from_le_bytes(b),
        })
    }

    fn left(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }
}

pub fn read(bytes: &[u8]) -> Result<Soup, String> {
    let end = find(bytes, b"end_header").ok_or("not a PLY file (no end_header)")?;
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| "the PLY header is not text")?;
    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file".into());
    }
    let mut enc = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in lines {
        let w: Vec<&str> = line.split_ascii_whitespace().collect();
        match w.as_slice() {
            ["format", f, _] => {
                enc = Some(match *f {
                    "ascii" => Encoding::Ascii,
                    "binary_little_endian" => Encoding::Little,
                    "binary_big_endian" => Encoding::Big,
                    _ => return Err(format!("unknown PLY format {f}")),
                })
            }
            ["element", name, count] => {
                if elements.len() >= 64 {
                    return Err("more than 64 PLY elements".into());
                }
                elements.push(Element { name: name.to_string(), count: count.parse().map_err(|_| format!("bad element count {count}"))?, props: Vec::new() });
            }
            ["property", "list", c, k, name] => {
                let e = elements.last_mut().ok_or("a property before any element")?;
                e.props.push(Property { name: name.to_string(), list: Some(Scalar::parse(c)?), kind: Scalar::parse(k)? });
            }
            ["property", k, name] => {
                let e = elements.last_mut().ok_or("a property before any element")?;
                e.props.push(Property { name: name.to_string(), list: None, kind: Scalar::parse(k)? });
            }
            _ => {}
        }
        if elements.last().is_some_and(|e| e.props.len() > 256) {
            return Err("an element with more than 256 properties".into());
        }
    }
    let enc = enc.ok_or("the PLY header names no format")?;
    // The data starts after the end_header line.
    let mut at = end + b"end_header".len();
    while at < bytes.len() && bytes[at] != b'\n' {
        at += 1;
    }
    let mut v = Values { bytes, at: (at + 1).min(bytes.len()), enc };
    let mut soup = Soup::default();
    let mut corners = Vec::new();
    for e in &elements {
        // Every item takes at least a byte (two in ASCII): counts beyond
        // what is left are a broken file, refused before allocating.
        let least = if enc == Encoding::Ascii { 2 * e.props.len() } else { e.props.iter().map(|p| p.list.map_or(p.kind.size(), Scalar::size)).sum::<usize>() }.max(1);
        if e.count.checked_mul(least).is_none_or(|need| need > v.left()) {
            return Err(format!("the file is cut short (element {} says {} items)", e.name, e.count));
        }
        let xyz = ["x", "y", "z"].map(|n| e.props.iter().position(|p| p.name == n && p.list.is_none()));
        let faces = e.props.iter().position(|p| p.list.is_some() && (p.name == "vertex_indices" || p.name == "vertex_index"));
        if e.name == "vertex" {
            if e.count > MAX_POINTS {
                return Err(format!("more than {MAX_POINTS} vertices"));
            }
            if xyz.iter().any(Option::is_none) {
                return Err("the vertex element has no x, y and z".into());
            }
            soup.points.reserve(e.count);
        }
        if e.name == "face" && e.count > MAX_TRIANGLES {
            return Err(format!("more than {MAX_TRIANGLES} faces"));
        }
        for _ in 0..e.count {
            let mut p = [0.0f64; 3];
            corners.clear();
            for (k, prop) in e.props.iter().enumerate() {
                match prop.list {
                    None => {
                        let x = v.next(prop.kind)?;
                        if e.name == "vertex"
                            && let Some(axis) = xyz.iter().position(|&i| i == Some(k))
                        {
                            p[axis] = x;
                        }
                    }
                    Some(count) => {
                        let n = v.next(count)?;
                        if !(0.0..=65536.0).contains(&n) || n.fract() != 0.0 {
                            return Err(format!("bad PLY list length {n}"));
                        }
                        for _ in 0..n as usize {
                            let i = v.next(prop.kind)?;
                            if e.name == "face" && faces == Some(k) {
                                if !(0.0..u32::MAX as f64).contains(&i) || i.fract() != 0.0 {
                                    return Err(format!("bad face index {i}"));
                                }
                                corners.push(i as u32);
                            }
                        }
                    }
                }
            }
            if e.name == "vertex" {
                soup.point(Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32))?;
            } else if e.name == "face" {
                soup.polygon(&corners)?;
            }
        }
    }
    Ok(soup)
}

fn find(bytes: &[u8], what: &[u8]) -> Option<usize> {
    bytes[..bytes.len().min(1 << 16)].windows(what.len()).position(|w| w == what)
}
