//! Picture checks for kitbash (ignored tests, run by hand; the GPU
//! traces them when there is one). Pictures go to OUT (default
//! `kitbash_check/`).
//!
//! - `cargo test --release kitbash_catalog -- --ignored --nocapture`: every
//!   part (`catalog.png`) and every style of every part (`styles.png`),
//!   each labeled with a stencil plate made by the stencil part.
//! - `MECH=/path/mech.json cargo test --release kitbash_mech -- --ignored --nocapture`:
//!   the mech before and after the three detail passes and hand
//!   placements, rendered like its `film_front.png` and `film_detail.png`
//!   (see `mech.rs`).
//!
//! SAMPLES sets samples per pixel (default 64).

use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Vec3};

use super::{CATALOG, Kind, Part};
use crate::film::{Camera, Item, Look, Settings, look::Device};
use crate::material::Material;
use crate::mesh::Mesh;

pub fn out() -> String {
    std::env::var("OUT").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/kitbash_check").into())
}

pub fn samples() -> u32 {
    std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(64)
}

fn item(mesh: Mesh, world: Mat4) -> Item {
    let material = Material::default();
    Item { mesh: Arc::new(mesh), world, tint: [1.0; 3], surface: material.surface(), material }
}

pub fn render(items: Vec<Item>, cam: Camera, w: usize, h: usize, samples: u32, floor: bool) -> image::RgbImage {
    let meshes = crate::film::Meshes::new(items);
    let mut look = Look::default();
    look.device = Device::Auto;
    let s = Settings { width: w, height: h, samples, bounces: 4, time_limit: Duration::from_secs(600), floor, exposure: 1.0, denoise: true };
    let img = crate::film::render_views_with_look(&meshes, &[], &[cam], s, Arc::new(look), &Default::default()).pop().expect("one view");
    image::RgbImage::from_raw(img.width as u32, img.height as u32, img.pixels.iter().flatten().copied().collect()).expect("size")
}

/// Points for parts that run between points, in a 1 m cell.
fn sample_points(kind: Kind) -> Vec<[f32; 3]> {
    match kind {
        Kind::Piston => vec![[-0.42, 0.12, 0.0], [0.42, 0.42, 0.0]],
        Kind::Cable => vec![[-0.45, 0.02, -0.1], [-0.1, 0.25, 0.05], [0.2, 0.12, -0.05], [0.45, 0.02, 0.1]],
        Kind::Hose => vec![[-0.45, 0.02, 0.0], [-0.05, 0.4, 0.05], [0.45, 0.02, 0.0]],
        Kind::CorrugatedTube => vec![[-0.42, 0.08, 0.0], [0.0, 0.3, 0.1], [0.42, 0.08, 0.0]],
        Kind::Pipe => vec![[-0.45, 0.05, -0.2], [-0.45, 0.35, -0.2], [0.2, 0.35, -0.2], [0.2, 0.35, 0.25]],
        Kind::Strap => vec![[-0.45, 0.0, 0.0], [0.45, 0.0, 0.05]],
        _ => Vec::new(),
    }
}

/// A part for the catalog: default size (point parts get sample points).
pub fn sample(kind: Kind, style: Option<&str>) -> Part {
    let mut p = Part::new(kind);
    p.style = style.map(str::to_string);
    p.points = sample_points(kind);
    match kind {
        Kind::Cable => { p.radius = Some(0.03); p.clips = Some(2); }
        Kind::Hose => { p.radius = Some(0.045); p.clips = Some(1); }
        Kind::Piston => p.radius = Some(0.07),
        Kind::Strap => p.radius = Some(0.05),
        Kind::Stencil => p.text = Some("LS-M1".into()),
        Kind::Spring => p.count = Some(7),
        _ => {}
    }
    p.color = Some([0.55, 0.47, 0.33]);
    p
}

/// One labeled cell: the part framed three-quarter, a stencil plate with
/// its name in front.
fn cell(part: &Part, label: &str, size: usize, samples: u32) -> image::RgbImage {
    let mesh = part.build(&[]);
    let (lo, hi) = mesh.bounds();
    let center = (lo + hi) * 0.5;
    let radius = ((hi - lo).length() * 0.5).max(1e-3);
    let mut items = vec![item(mesh, Mat4::IDENTITY)];
    let lw = radius * 1.6;
    let mut plate = Part::new(Kind::Stencil);
    plate.size = Some([lw, lw * 0.012, lw * 0.16]);
    plate.text = Some(label.to_uppercase().replace('_', " "));
    plate.color = Some([0.2, 0.2, 0.2]);
    let plate_mesh = plate.build(&[]);
    let at = Vec3::new(center.x, lo.y, hi.z + radius * 0.35);
    items.push(item(plate_mesh, Mat4::from_translation(at)));
    let (yaw, pitch) = (32f32.to_radians(), 30f32.to_radians());
    let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
    let target = center + Vec3::new(0.0, 0.0, radius * 0.2);
    let fov = 30.0f32;
    let dist = radius * 1.45 / (fov * 0.5).to_radians().sin();
    let cam = Camera { eye: target + dir * dist, target, up: Vec3::Y, fov_deg: fov, ortho: None };
    render(items, cam, size, size, samples, true)
}

fn sheet(cells: &[image::RgbImage], cols: usize, size: usize) -> image::RgbImage {
    let rows = cells.len().div_ceil(cols);
    let mut out = image::RgbImage::from_pixel((cols * size) as u32, (rows * size) as u32, image::Rgb([40, 40, 42]));
    for (i, c) in cells.iter().enumerate() {
        image::imageops::replace(&mut out, c, ((i % cols) * size) as i64, ((i / cols) * size) as i64);
    }
    out
}

#[test]
#[ignore]
fn kitbash_catalog() {
    let dir = out();
    std::fs::create_dir_all(&dir).unwrap();
    let t = std::time::Instant::now();
    let size = 360;
    let cells: Vec<image::RgbImage> = CATALOG.iter().map(|i| cell(&sample(i.kind, None), i.name, size, samples())).collect();
    sheet(&cells, 8, size).save(format!("{dir}/catalog.png")).unwrap();
    eprintln!("catalog: {} parts in {:.1}s", cells.len(), t.elapsed().as_secs_f32());
    let small = 256;
    let mut styles = Vec::new();
    for i in CATALOG.iter() {
        for s in i.styles {
            styles.push(cell(&sample(i.kind, Some(s)), &format!("{} {s}", i.name), small, samples() / 2));
        }
    }
    sheet(&styles, 10, small).save(format!("{dir}/styles.png")).unwrap();
    eprintln!("styles: {} cells, {:.1}s total", styles.len(), t.elapsed().as_secs_f32());
}

/// Before | after pictures of the mech from `build_mech.py`'s renders.
#[test]
#[ignore]
fn kitbash_before_after() {
    let dir = out();
    for view in ["front", "detail"] {
        let (Ok(a), Ok(b)) = (image::open(format!("{dir}/mech_{view}_before.png")), image::open(format!("{dir}/mech_{view}_after.png"))) else {
            eprintln!("{view}: run kitbash_check/build_mech.py first");
            continue;
        };
        let (a, b) = (a.to_rgb8(), b.to_rgb8());
        let gap = 8;
        let mut sheet = image::RgbImage::from_pixel(a.width() + gap + b.width(), a.height().max(b.height()), image::Rgb([30, 30, 32]));
        image::imageops::replace(&mut sheet, &a, 0, 0);
        image::imageops::replace(&mut sheet, &b, (a.width() + gap) as i64, 0);
        sheet.save(format!("{dir}/mech_{view}_before_after.png")).unwrap();
    }
}
