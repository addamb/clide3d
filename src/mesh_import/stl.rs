//! STL: binary (an 80-byte header, a count, 50 bytes per triangle) when
//! the size says so, else ASCII (`vertex x y z`, three per facet).

use glam::Vec3;

use super::{Cursor, Soup};
use crate::mesh_asset::MAX_TRIANGLES;

pub fn read(bytes: &[u8]) -> Result<Soup, String> {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if n.checked_mul(50).and_then(|b| b.checked_add(84)) == Some(bytes.len()) {
            return binary(bytes, n);
        }
    }
    let head = &bytes[..bytes.len().min(512)];
    if String::from_utf8_lossy(head).trim_start().starts_with("solid") {
        return ascii(bytes);
    }
    Err("not a binary STL (its size does not match its triangle count) nor an ASCII one".into())
}

fn binary(bytes: &[u8], n: usize) -> Result<Soup, String> {
    if n > MAX_TRIANGLES {
        return Err(format!("more than {MAX_TRIANGLES} triangles"));
    }
    let mut soup = Soup { points: Vec::with_capacity(n * 3), tris: Vec::with_capacity(n), warnings: Vec::new() };
    let mut c = Cursor::new(bytes);
    c.take(84)?;
    for _ in 0..n {
        c.take(12)?;
        let mut t = [0u32; 3];
        for i in &mut t {
            *i = soup.point(Vec3::new(c.f32()?, c.f32()?, c.f32()?))?;
        }
        c.take(2)?;
        soup.polygon(&t)?;
    }
    Ok(soup)
}

fn ascii(bytes: &[u8]) -> Result<Soup, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut soup = Soup::default();
    let mut corners = Vec::with_capacity(3);
    let mut words = text.split_ascii_whitespace();
    while let Some(w) = words.next() {
        match w {
            "vertex" => {
                let mut v = [0.0f32; 3];
                for x in &mut v {
                    *x = words.next().and_then(|w| w.parse().ok()).ok_or("a vertex needs x y z")?;
                }
                corners.push(soup.point(Vec3::from(v))?);
            }
            "endloop" => {
                if corners.len() != 3 {
                    return Err(format!("a facet with {} vertices (STL facets have 3)", corners.len()));
                }
                soup.polygon(&corners)?;
                corners.clear();
            }
            _ => {}
        }
    }
    Ok(soup)
}
