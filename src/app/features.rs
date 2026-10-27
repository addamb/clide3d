//! Feature lines for close-up compares (docs/QUALITY_PLAN.md Stage 2):
//! the model's lines (creases, curvature extremes, occluding contours)
//! against the reference's drawn lines, inside both outlines.
//!
//! The model is rasterized on the CPU into the crop (depth, normal and
//! the surface point per pixel), and each clay pixel's mean curvature is
//! read from its field, so the lines do not depend on the tessellation
//! and the result is the same on any machine with the same meshes.

use std::collections::HashSet;

use glam::{Mat3, Vec3};

use super::ClideApp;
use super::outline;

/// Curvature edges (Canny on mean curvature per cell): the hysteresis
/// thresholds, in curvature per cell across one cell.
const CURVATURE_HIGH: f32 = 0.04;
const CURVATURE_LOW: f32 = 0.02;
/// Occluding contours: a depth step of more than this many pixels' width.
const DEPTH_STEP_PX: f32 = 3.0;
/// Creases: the normal turns by more than this between neighbor pixels.
const NORMAL_STEP_DEGREES: f32 = 30.0;
/// Reference lines: Canny on luminance (0..1 per pixel).
const LUMA_HIGH: f32 = 0.08;
const LUMA_LOW: f32 = 0.04;
/// Lines this close to an outline (pixels) are the outline's, which the
/// silhouette measures already cover.
const OUTLINE_PX: f32 = 2.0;

/// An orthographic crop: pixel (i, j) of `size` x `size` has its center
/// at u = u0 - radius + (i + 0.5) per, v = v0 + radius - (j + 0.5) per on
/// the screen axes `right` and `up`; `dir` points toward the camera.
pub(super) struct Crop {
    pub right: Vec3,
    pub up: Vec3,
    pub dir: Vec3,
    pub u0: f32,
    pub v0: f32,
    pub radius: f32,
    pub size: usize,
}

impl Crop {
    fn per(&self) -> f32 {
        2.0 * self.radius / self.size as f32
    }

    /// World point -> (pixel x, pixel y, depth toward the camera).
    fn project(&self, p: Vec3) -> (f32, f32, f32) {
        let per = self.per();
        ((p.dot(self.right) - (self.u0 - self.radius)) / per, ((self.v0 + self.radius) - p.dot(self.up)) / per, p.dot(self.dir))
    }
}

/// What the model shows in a crop, per pixel.
pub(super) struct ModelLines {
    /// The line pixels: curvature edges, depth steps and normal steps,
    /// away from the model's outline.
    pub lines: Vec<bool>,
    /// Where the model is.
    pub mask: Vec<bool>,
}

/// One rasterized pixel: depth, object, surface point and normal (world).
#[derive(Clone, Copy)]
struct Hit {
    depth: f32,
    object: u32,
    p: Vec3,
    n: Vec3,
}

impl ClideApp {
    /// The model's feature lines in `crop`, drawing `only` (every visible
    /// mesh with None).
    pub(super) fn model_lines(&self, crop: &Crop, only: Option<&HashSet<u64>>) -> ModelLines {
        let size = crop.size;
        let eval = self.scene.evaluate();
        let objects: Vec<&crate::scene::Object> = self.scene.objects.iter().filter(|o| o.shape.is_mesh() && !o.is_reference() && only.is_none_or(|ids| ids.contains(&o.id)) && eval.get(&o.id).is_some_and(|e| e.1)).collect();
        let mut hits: Vec<Option<Hit>> = vec![None; size * size];
        for (k, o) in objects.iter().enumerate() {
            let Some(mesh) = self.meshes.get(o) else { continue };
            let world = eval[&o.id].0;
            let normal_m = Mat3::from_mat4(world).inverse().transpose();
            let pts: Vec<(Vec3, Vec3, (f32, f32, f32))> = mesh
                .vertices
                .iter()
                .map(|v| {
                    let p = world.transform_point3(Vec3::from(v.pos));
                    (p, (normal_m * Vec3::from(v.normal)).normalize_or_zero(), crop.project(p))
                })
                .collect();
            for t in mesh.indices.chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| pts.get(i as usize).copied());
                let (Some(a), Some(b), Some(c)) = (a, b, c) else { continue };
                raster(&mut hits, size, [a, b, c], k as u32);
            }
        }
        let mask: Vec<bool> = hits.iter().map(Option::is_some).collect();
        // Mean curvature per cell from each clay's field, at its pixels.
        let mut curvature = vec![0.0f32; size * size];
        let mut cell_px = f32::INFINITY;
        for (k, o) in objects.iter().enumerate() {
            let Some(probe) = o.clay.as_ref().and_then(crate::quality::Probe::new) else { continue };
            let to_local = eval[&o.id].0.inverse();
            let scale = eval[&o.id].0.to_scale_rotation_translation().0.max_element();
            let pixels: Vec<usize> = (0..size * size).filter(|&i| hits[i].is_some_and(|h| h.object == k as u32)).collect();
            if pixels.is_empty() {
                continue;
            }
            let local: Vec<Vec3> = pixels.iter().map(|&i| to_local.transform_point3(hits[i].expect("hit").p)).collect();
            // Concave only: drawn lines mark creases and grooves (dark), and
            // a groove's convex shoulders would add a line either side.
            for (&i, h) in pixels.iter().zip(probe.curvatures(&local)) {
                curvature[i] = h.min(0.0);
            }
            cell_px = cell_px.min(probe.cell(local[local.len() / 2]) * scale / crop.per());
        }
        let cell_px = if cell_px.is_finite() { cell_px.max(0.5) } else { 1.0 };
        // Canny on curvature with thresholds per cell, as pixels see them.
        let mut lines = outline::canny(&curvature, size, size, 1.0, CURVATURE_LOW / cell_px, CURVATURE_HIGH / cell_px);
        // Depth and normal steps between neighbors, on the nearer pixel.
        let depth_step = DEPTH_STEP_PX * crop.per();
        let normal_step = NORMAL_STEP_DEGREES.to_radians().cos();
        for y in 0..size {
            for x in 0..size {
                let Some(a) = hits[y * size + x] else { continue };
                for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                    if nx >= size || ny >= size {
                        continue;
                    }
                    let Some(b) = hits[ny * size + nx] else { continue };
                    if (a.depth - b.depth).abs() > depth_step || a.n.dot(b.n) < normal_step {
                        let near = if a.depth >= b.depth { y * size + x } else { ny * size + nx };
                        lines[near] = true;
                    }
                }
            }
        }
        let inside = outline::erode(&mask, size, size, OUTLINE_PX);
        for (l, i) in lines.iter_mut().zip(&inside) {
            *l &= *i;
        }
        ModelLines { lines, mask }
    }
}

/// The reference's drawn lines in a crop: Canny on luminance, inside its
/// subject mask away from the outline.
pub(super) fn reference_lines(crop: &egui::ColorImage, mask: &[bool]) -> Vec<bool> {
    let [w, h] = crop.size;
    let luma: Vec<f32> = crop.pixels.iter().map(|p| (0.2126 * p.r() as f32 + 0.7152 * p.g() as f32 + 0.0722 * p.b() as f32) / 255.0).collect();
    let mut lines = outline::canny(&luma, w, h, 1.0, LUMA_LOW, LUMA_HIGH);
    let inside = outline::erode(mask, w, h, OUTLINE_PX);
    for (l, i) in lines.iter_mut().zip(&inside) {
        *l &= *i;
    }
    lines
}

/// The two line maps as a panel: reference lines red, the model's cyan,
/// both white, over the model's mask in dark gray.
pub(super) fn lines_panel(reference: &[bool], model: &[bool], mask: &[bool], size: usize) -> egui::ColorImage {
    let pixels = (0..size * size)
        .map(|i| match (reference[i], model[i]) {
            (true, true) => egui::Color32::WHITE,
            (true, false) => egui::Color32::from_rgb(240, 70, 60),
            (false, true) => egui::Color32::from_rgb(60, 210, 240),
            _ if mask[i] => egui::Color32::from_gray(48),
            _ => egui::Color32::from_gray(20),
        })
        .collect();
    egui::ColorImage::new([size, size], pixels)
}

/// Rasterizes one triangle's points (world point, normal, projected) into
/// `hits`, keeping the nearest (largest depth).
fn raster(hits: &mut [Option<Hit>], size: usize, v: [(Vec3, Vec3, (f32, f32, f32)); 3], object: u32) {
    let [(pa, na, a), (pb, nb, b), (pc, nc, c)] = v;
    let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    if area.abs() < 1e-12 || !area.is_finite() {
        return;
    }
    let x0 = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
    let y0 = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
    let x1 = (a.0.max(b.0).max(c.0).ceil().max(0.0) as usize).min(size);
    let y1 = (a.1.max(b.1).max(c.1).ceil().max(0.0) as usize).min(size);
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let w0 = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
            let w1 = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let depth = a.2 * w0 + b.2 * w1 + c.2 * w2;
            let slot = &mut hits[y * size + x];
            if slot.is_none_or(|h| depth > h.depth) {
                *slot = Some(Hit { depth, object, p: pa * w0 + pb * w1 + pc * w2, n: (na * w0 + nb * w1 + nc * w2).normalize_or_zero() });
            }
        }
    }
}
