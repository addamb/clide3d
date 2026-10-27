//! Triangle meshes from files (`import_mesh`) and to files
//! (`export_print`): OBJ, STL (binary and ASCII), PLY (ASCII and binary
//! either way round), GLB (through `glb_read`) and binary FBX (the
//! Geometry nodes only).
//!
//! Files are untrusted: every count is checked against the bytes left
//! before anything is allocated for it, the file size and the totals are
//! capped, coordinates must be finite and within `MAX_COORD`, and a
//! malformed file is an error, never a panic.

mod fbx;
mod glb;
mod obj;
mod ply;
mod stl;
pub mod write;
pub use obj::{read_cage, read_cage_as};
#[cfg(test)]
mod tests;

use std::path::Path;

use glam::Vec3;

use crate::mesh_asset::{MAX_COORD, MAX_POINTS, MAX_TRIANGLES};

/// Largest file `import_mesh` reads.
pub const MAX_FILE_BYTES: u64 = 1 << 30;

/// The formats read and written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Obj,
    Stl,
    Ply,
    Glb,
    Fbx,
}

impl Format {
    pub fn of(path: &Path) -> Result<Format, String> {
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        Ok(match ext.as_str() {
            "obj" => Format::Obj,
            "stl" => Format::Stl,
            "ply" => Format::Ply,
            "glb" => Format::Glb,
            "fbx" => Format::Fbx,
            _ => return Err(format!("{}: import reads .obj, .stl, .ply, .glb and .fbx", path.display())),
        })
    }
}

/// Triangles as read: points (welded where bit-equal) and every triangle
/// of the file, in its order and winding.
#[derive(Debug, Default)]
pub struct Soup {
    pub points: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
    pub warnings: Vec<String>,
}

impl Soup {
    /// Adds a point, checked.
    fn point(&mut self, p: Vec3) -> Result<u32, String> {
        if !p.is_finite() || p.abs().max_element() > MAX_COORD {
            return Err(format!("a vertex is not finite or beyond {MAX_COORD} ({p})"));
        }
        // Before welding (STL repeats every corner).
        if self.points.len() >= MAX_POINTS.max(3 * MAX_TRIANGLES) {
            return Err(format!("more than {} vertices", MAX_POINTS.max(3 * MAX_TRIANGLES)));
        }
        self.points.push(p);
        Ok(self.points.len() as u32 - 1)
    }

    /// A polygon (fanned into triangles), its corners checked.
    fn polygon(&mut self, corners: &[u32]) -> Result<(), String> {
        if corners.len() < 3 {
            return Ok(());
        }
        if self.tris.len() + corners.len() - 2 > MAX_TRIANGLES {
            return Err(format!("more than {MAX_TRIANGLES} triangles"));
        }
        let n = self.points.len() as u32;
        if corners.iter().any(|&c| c >= n) {
            return Err("a face points past the vertex list".into());
        }
        for k in 1..corners.len() - 1 {
            self.tris.push([corners[0], corners[k], corners[k + 1]]);
        }
        Ok(())
    }

    /// Welds bit-equal points (STL shares none) and checks the result.
    fn finish(mut self) -> Result<Soup, String> {
        if self.tris.is_empty() {
            return Err("the file holds no triangles".into());
        }
        let (points, tris) = crate::mesh_asset::weld(&self.points, &self.tris);
        // Points no face uses (stray `v` lines, a point cloud's) are dropped.
        let (points, tris) = crate::mesh_asset::without_unused(points, tris);
        let degenerate = tris.iter().filter(|t| t[0] == t[1] || t[1] == t[2] || t[0] == t[2]).count();
        if degenerate > 0 {
            self.warnings.push(format!("{degenerate} degenerate triangles (kept; the distance field skips them)"));
        }
        Ok(Soup { points, tris, warnings: self.warnings })
    }
}

/// Reads a mesh file (size checked before reading).
pub fn read(path: &Path) -> Result<Soup, String> {
    let format = Format::of(path)?;
    let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{}: {} MiB is more than the {} MiB import limit", path.display(), len >> 20, MAX_FILE_BYTES >> 20));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes, format).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads mesh bytes of a format.
pub fn parse(bytes: &[u8], format: Format) -> Result<Soup, String> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("the file is too large".into());
    }
    let soup = match format {
        Format::Obj => obj::read(bytes)?,
        Format::Stl => stl::read(bytes)?,
        Format::Ply => ply::read(bytes)?,
        Format::Glb => glb::read(bytes)?,
        Format::Fbx => fbx::read(bytes)?,
    };
    soup.finish()
}

/// Little-endian readers that fail instead of panicking.
pub(crate) struct Cursor<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, at: 0 }
    }

    pub fn left(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or("the file is cut short")?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.array()?))
    }
}
