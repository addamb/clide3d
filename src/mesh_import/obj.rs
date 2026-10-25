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
