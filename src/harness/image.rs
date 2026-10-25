//! Image goldens: offscreen viewport renders (`render::render_image`) at
//! 512 x 512 from a fixed camera, compared with PNGs in `tests/golden/` by
//! mean and 99th-percentile per-channel error.
//!
//! They need a GPU, so they run only with `CLIDE_GPU=1` (and then a
//! missing adapter fails rather than skips); the geometry and hash checks
//! are the gate that always runs. `BLESS=1` rewrites a golden from the
//! current render instead of comparing.

use std::path::PathBuf;

use glam::{Mat4, Vec3};

use crate::mesh::MeshLib;
use crate::render::test_support;
use crate::scene::Scene;

/// Golden size, both ways.
pub const SIZE: u32 = 512;

pub fn gpu_enabled() -> bool {
    std::env::var("CLIDE_GPU").is_ok_and(|v| v == "1")
}

pub fn blessing() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1")
}

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

/// A fixed perspective view of `target` from `eye`.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    pub target: Vec3,
    /// Vertical field of view in degrees.
    pub fov: f32,
}

impl View {
    /// Looking at the origin from the front, a little right and above,
    /// framing about a 1.4-unit object.
    pub const FRONT: View = View { eye: Vec3::new(1.2, 0.9, 2.6), target: Vec3::ZERO, fov: 35.0 };

    fn view_proj(&self) -> Mat4 {
        let far = (self.eye - self.target).length() * 3.0 + 10.0;
        Mat4::perspective_rh(self.fov.to_radians(), 1.0, 0.05, far) * Mat4::look_at_rh(self.eye, self.target, Vec3::Y)
    }
}

/// Renders `scene` (already synced into `meshes`) as an agent screenshot
/// would, on the sRGB target. Err when there is no GPU or wgpu reports a
/// validation error.
pub fn render(scene: &Scene, meshes: &MeshLib, view: View) -> Result<egui::ColorImage, String> {
    let rs = test_support::headless().ok_or("no GPU adapter")?;
    let cb = crate::app::scene_callback_of(scene, meshes, view.view_proj(), view.eye, false, false);
    test_support::render(&rs, &cb, SIZE, SIZE, test_support::BACKGROUND)
}

/// Error between two same-sized images over all RGB channels, in 0..255
/// steps.
#[derive(Debug, Clone, Copy)]
pub struct ImageError {
    pub mean: f64,
    pub p99: u8,
    pub max: u8,
}

pub fn error(a: &egui::ColorImage, b: &egui::ColorImage) -> ImageError {
    assert_eq!(a.size, b.size, "images differ in size");
    let mut counts = [0usize; 256];
    for (p, q) in a.pixels.iter().zip(&b.pixels) {
        for d in [p.r().abs_diff(q.r()), p.g().abs_diff(q.g()), p.b().abs_diff(q.b())] {
            counts[d as usize] += 1;
        }
    }
    let n = counts.iter().sum::<usize>().max(1);
    let mean = counts.iter().enumerate().map(|(d, c)| d as f64 * *c as f64).sum::<f64>() / n as f64;
    // The smallest error that 99% of channels are at or under.
    let (mut seen, mut p99) = (0, 0u8);
    for (d, c) in counts.iter().enumerate() {
        seen += c;
        if seen as f64 >= 0.99 * n as f64 {
            p99 = d as u8;
            break;
        }
    }
    let max = counts.iter().rposition(|c| *c > 0).unwrap_or(0) as u8;
    ImageError { mean, p99, max }
}

fn save(img: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let rgb: Vec<u8> = img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    image::save_buffer(path, &rgb, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgb8).map_err(|e| format!("{}: {e}", path.display()))
}

fn load(path: &std::path::Path) -> Result<egui::ColorImage, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e} (make it with BLESS=1)", path.display()))?.to_rgb8();
    Ok(test_support::from_rgb(img.width() as usize, img.height() as usize, img.as_raw()))
}

/// Compares `img` with `tests/golden/<name>.png` and returns the error;
/// with `BLESS=1` saves `img` as the golden instead (zero error). Every
/// comparison leaves the render in `target/golden/<name>.png` to look at.
pub fn compare(name: &str, img: &egui::ColorImage) -> Result<ImageError, String> {
    let path = golden_dir().join(format!("{name}.png"));
    if blessing() {
        save(img, &path)?;
        eprintln!("blessed {}", path.display());
        return Ok(ImageError { mean: 0.0, p99: 0, max: 0 });
    }
    let golden = load(&path)?;
    if golden.size != img.size {
        return Err(format!("{name}: golden is {:?}, render is {:?}", golden.size, img.size));
    }
    let e = error(&golden, img);
    let _ = save(img, &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("golden").join(format!("{name}.png")));
    Ok(e)
}

/// `compare`, asserting mean <= `mean` and p99 <= `p99` (both in 0..255
/// steps).
pub fn assert_golden(name: &str, img: &egui::ColorImage, mean: f64, p99: u8) {
    let e = compare(name, img).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("golden {name}: {e:?}");
    assert!(e.mean <= mean && e.p99 <= p99, "golden {name}: mean {:.3}/255 (max {mean}), p99 {}/255 (max {p99}); the render is in target/golden/", e.mean, e.p99);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_reports_mean_p99_and_max() {
        let solid = |v: u8| egui::ColorImage::new([10, 10], vec![egui::Color32::from_rgb(v, v, v); 100]);
        let a = solid(100);
        let mut b = solid(100);
        // One pixel of 100 off by 50 in every channel: 1% of channels.
        b.pixels[7] = egui::Color32::from_rgb(150, 150, 150);
        let e = error(&a, &b);
        assert_eq!((e.p99, e.max), (0, 50));
        assert!((e.mean - 0.5).abs() < 1e-9);
        // Two pixels off: past the 99th percentile.
        b.pixels[8] = egui::Color32::from_rgb(110, 110, 110);
        assert_eq!(error(&a, &b).p99, 10);
    }
}
