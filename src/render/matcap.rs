//! MatCaps (docs/SCULPT_PLAN.md Stage 6): a sphere's shading painted
//! into an image, looked up by the surface normal as the camera sees it,
//! so a sculpt reads its forms the same in any light. Four are built in
//! (`clay`, `red_wax`, `skin`, `metal`); any square PNG or JPEG of a
//! shaded sphere works too (`set_display matcap: path`).
//!
//! The lookup: with `v` toward the camera, `right` = `up_world` x `v` and
//! `up` = `v` x `right`, a normal `n` reads the image at
//! (0.5 + 0.5 n.right, 0.5 - 0.5 n.up): facing the camera is the middle,
//! the silhouette is the rim. `uv` is the CPU side of the shader's.

use std::sync::Arc;

use glam::{Vec2, Vec3};

/// Largest matcap image side.
pub const MAX_SIDE: u32 = 1024;
/// Built-in matcaps' side.
const SIDE: u32 = 256;

/// A matcap image (sRGB RGBA8, square), with an id unique to it.
#[derive(Debug)]
pub struct Matcap {
    pub name: String,
    pub side: u32,
    pub rgba: Vec<u8>,
    pub id: u64,
}

pub const BUILTIN: [&str; 4] = ["clay", "red_wax", "skin", "metal"];

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Where normal `n` reads a matcap, seen along `v` (toward the camera).
#[cfg(test)]
pub fn uv(n: Vec3, v: Vec3) -> Vec2 {
    let mut right = Vec3::Y.cross(v);
    if right.length_squared() < 1e-8 {
        right = Vec3::X;
    }
    let right = right.normalize();
    let up = v.cross(right);
    Vec2::new(0.5 + 0.5 * n.dot(right), 0.5 - 0.5 * n.dot(up))
}

impl Matcap {
    /// A built-in matcap by name.
    pub fn builtin(name: &str) -> Option<Arc<Matcap>> {
        // Base color (linear), key light, specular strength and size, rim.
        let (base, spec, power, rim): (Vec3, f32, f32, Vec3) = match name {
            "clay" => (Vec3::new(0.55, 0.47, 0.42), 0.12, 12.0, Vec3::splat(0.05)),
            "red_wax" => (Vec3::new(0.55, 0.07, 0.05), 0.35, 40.0, Vec3::new(0.25, 0.04, 0.03)),
            "skin" => (Vec3::new(0.72, 0.45, 0.36), 0.18, 20.0, Vec3::new(0.3, 0.12, 0.08)),
            "metal" => (Vec3::new(0.35, 0.36, 0.38), 0.9, 60.0, Vec3::splat(0.25)),
            _ => return None,
        };
        let key = Vec3::new(-0.4, 0.55, 0.73).normalize();
        let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let p = Vec2::new((x as f32 + 0.5) / SIDE as f32 * 2.0 - 1.0, 1.0 - (y as f32 + 0.5) / SIDE as f32 * 2.0);
                // Past the rim: the rim's color (the lookup never reads it).
                let p = if p.length() > 1.0 { p.normalize() } else { p };
                let n = Vec3::new(p.x, p.y, (1.0 - p.length_squared()).max(0.0).sqrt());
                let diffuse = n.dot(key).max(0.0) * 0.8 + 0.2;
                let h = (key + Vec3::Z).normalize();
                let specular = spec * n.dot(h).max(0.0).powf(power);
                let fresnel = (1.0 - n.z).powi(3);
                let c = base * diffuse + Vec3::splat(specular) + rim * fresnel;
                let enc = |v: f32| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
                rgba.extend_from_slice(&[enc(c.x), enc(c.y), enc(c.z), 255]);
            }
        }
        Some(Arc::new(Matcap { name: name.into(), side: SIDE, rgba, id: next_id() }))
    }

    /// A matcap from an image file: square, at most `MAX_SIDE` a side
    /// (checked from the header before decoding).
    pub fn load(path: &std::path::Path) -> Result<Arc<Matcap>, String> {
        let reader = image::ImageReader::open(path).map_err(|e| format!("cannot open matcap {}: {e}", path.display()))?.with_guessed_format().map_err(|e| e.to_string())?;
        let (w, h) = reader.into_dimensions().map_err(|e| format!("matcap {}: {e}", path.display()))?;
        if w != h || w == 0 || w > MAX_SIDE {
            return Err(format!("a matcap must be a square image at most {MAX_SIDE} pixels a side ({w} x {h})"));
        }
        let img = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?.decode().map_err(|e| format!("matcap {}: {e}", path.display()))?;
        let rgba = img.to_rgba8().into_raw();
        Ok(Arc::new(Matcap { name: path.display().to_string(), side: w, rgba, id: next_id() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_matcaps_light_a_sphere_from_the_middle_out() {
        for name in BUILTIN {
            let m = Matcap::builtin(name).unwrap();
            assert_eq!(m.rgba.len(), (m.side * m.side * 4) as usize);
            let v = Vec3::Z;
            // Facing the camera reads the middle; the silhouette the rim.
            assert!((uv(v, v) - Vec2::splat(0.5)).length() < 1e-6);
            assert!(((uv(Vec3::X, v) - Vec2::splat(0.5)).length() - 0.5).abs() < 1e-6);
            assert!(uv(Vec3::Y, v).y < 0.01);
        }
        assert!(Matcap::builtin("chalk").is_none());
    }
}
