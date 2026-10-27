//! Stage 4d (docs/QUALITY_PLAN.md): stylised skin and eye presets in the
//! film renderer. Small CPU renders, deterministic, with measured checks;
//! the GPU comparisons live with the other GPU tests (`gpu.rs`).

use super::*;
use glam::Quat;
use std::hash::Hasher;

/// Pixel bits of a render.
pub(super) fn pixel_hash(img: &Image) -> u64 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for p in &img.pixels {
        h.write(p);
    }
    h.finish()
}

/// A skin ball (radius 0.05, close up) under the default studio, CPU.
pub(super) fn skin_ball(m: Material, samples: u32) -> Image {
    let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
        "blobs": [{ "type": "sphere", "center": [0, 0.05, 0], "radius": 0.05, "color": [0.9, 0.68, 0.58] }], "resolution": 48
    }))
    .unwrap();
    let mesh = Arc::new(clay.build_mesh());
    let cam = Camera { eye: Vec3::new(0.0, 0.06, 0.4), target: Vec3::new(0.0, 0.05, 0.0), up: Vec3::Y, fov_deg: 20.0, ortho: None };
    let s = Settings { width: 48, height: 48, samples, bounces: 3, time_limit: Duration::from_secs(600), floor: false, exposure: 1.0, denoise: false };
    let items = vec![Item { mesh, world: Mat4::IDENTITY, tint: [1.0; 3], surface: [-1.0, 0.0, 0.0, 0.0], material: m }];
    let img = render(items, &[], &cam, s, &Default::default());
    assert_eq!(img.samples, samples, "the render took every sample");
    img
}

/// A skin material saved before Stage 4d (no `subsurface_radius`) loads
/// and renders as it did: the pixels are pinned from before the field
/// existed (the plan's `presets_round_trip` golden), and the CPU tracer
/// is bit-deterministic.
#[test]
fn presets_round_trip() {
    let old: Material = serde_json::from_str(r#"{"name":"face","roughness":0.48,"sheen":0.04,"subsurface":0.8,"pores":0.65,"mottling":0.4,"skin_specular":0.8}"#).unwrap();
    let a = skin_ball(old.clone(), 32);
    let b = skin_ball(old, 32);
    assert_eq!(pixel_hash(&a), pixel_hash(&b), "the same render twice");
    eprintln!("pre-4d skin ball: {:#x}", pixel_hash(&a));
    assert_eq!(pixel_hash(&a), 0x53b7f7853ba6ae3d, "pinned before Stage 4d");
}

/// Display luma (0..1) of an sRGB pixel.
fn luma(p: [u8; 3]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// A black world lit by one small sun from `toward` (CPU).
fn sun_look(toward: [f32; 3], intensity: f32) -> Look {
    let mut look = Look::parse(&serde_json::json!({ "environment": { "preset": "studio", "intensity": 0 }, "lights": [{ "type": "sun", "direction": toward, "size": 0.5, "intensity": intensity }] })).unwrap();
    look.device = look::Device::Cpu;
    look
}

fn trace(items: Vec<Item>, cam: Camera, size: usize, samples: u32, look: Look) -> Image {
    let asked = look.device;
    let s = Settings { width: size, height: size, samples, bounces: 3, time_limit: Duration::from_secs(600), floor: false, exposure: 1.0, denoise: false };
    let meshes = Meshes::new(items);
    let img = render_views_with_look(&meshes, &[], &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
    img.traced.assert_honours(asked);
    assert_eq!(img.samples, samples, "the render took every sample");
    img
}

fn clay_item(blobs: serde_json::Value, resolution: u32, m: &Material) -> Item {
    let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({ "blobs": blobs, "resolution": resolution })).unwrap();
    let mut mesh = clay.build_mesh();
    mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
    Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m.clone() }
}

/// The 10%-90% width (millimetres) of the shadow edge a plate casts on a
/// skin ball (radius 0.5, so the patch seen is nearly flat and lit
/// evenly), along the middle columns of a close-up 40 mm across: the
/// plan's `stylized_skin_softer` measure. A sun from above and in front;
/// the plate's edge puts the shadow's line across the patch's middle.
fn shadow_edge_width(m: &Material) -> f32 {
    let ball = clay_item(serde_json::json!([{ "type": "sphere", "center": [0, 0, -0.5], "radius": 0.5, "color": [0.9, 0.68, 0.58] }]), 64, m);
    // Rays to the sun at 45 degrees: the plate's front edge at y 0.1,
    // z 0.1 shades the patch below y = 0.
    let plate = Item { mesh: Arc::new(crate::mesh::Mesh::build(crate::scene::Shape::Cube)), world: Mat4::from_scale_rotation_translation(Vec3::new(1.0, 0.002, 0.2), Quat::IDENTITY, Vec3::new(0.0, 0.1, 0.2)), tint: [1.0; 3], surface: Material::default().surface(), material: Material::default() };
    let (size, across) = (96, 0.08);
    let distance = 0.25;
    let cam = Camera { eye: Vec3::new(0.0, 0.0, distance), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 2.0 * (across * 0.5 / distance).atan().to_degrees(), ortho: None };
    let img = trace(vec![ball, plate], cam, size, 384, sun_look([0.0, 1.0, 1.0], 3.0));
    // Rows down the middle 16 columns, top (lit) to bottom (shadow).
    let profile: Vec<f32> = (0..size).map(|y| (size / 2 - 8..size / 2 + 8).map(|x| luma(img.pixels[y * size + x])).sum::<f32>() / 16.0).collect();
    let (top, bottom) = (&profile[4..16], &profile[80..92]);
    let hi = top.iter().sum::<f32>() / top.len() as f32;
    let lo = bottom.iter().sum::<f32>() / bottom.len() as f32;
    let at = |f: f32| {
        let level = lo + (hi - lo) * f;
        let k = (10..90).find(|&y| profile[y] < level).expect("the edge");
        let (a, b) = (profile[k - 1], profile[k]);
        (k - 1) as f32 + (a - level) / (a - b).max(1e-6)
    };
    (at(0.1) - at(0.9)) * across / size as f32 * 1000.0
}

/// Stylised skin's shadow edge is softer than realistic skin's: the
/// width at least 1.4x (the plan's `stylized_skin_softer`), and an opaque
/// material's is the sharpest.
#[test]
fn stylized_skin_softer() {
    let real = shadow_edge_width(&Material::preset("skin").unwrap());
    let soft = shadow_edge_width(&Material::preset("skin_stylized").unwrap());
    let opaque = shadow_edge_width(&Material { subsurface: 0.0, ..Material::preset("skin_stylized").unwrap() });
    eprintln!("shadow edge 10-90%: opaque {opaque:.2} mm, skin {real:.2} mm, skin_stylized {soft:.2} mm ({:.2}x)", soft / real);
    assert!(soft >= 1.4 * real, "skin_stylized {soft} against skin {real}");
    assert!(real > opaque);
}

/// A head (a ball) with a thin ear, lit from behind: the light that
/// comes through the ear to the camera, for a material and the same
/// material made opaque.
fn ear_glow(m: &Material) -> (f32, f32, u64) {
    let blobs = serde_json::json!([
        { "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "color": [0.9, 0.68, 0.58] },
        { "type": "ellipsoid", "center": [0.125, 0, 0], "radii": [0.035, 0.045, 0.003], "color": [0.9, 0.68, 0.58], "blend": 0.004 }
    ]);
    let cam = Camera { eye: Vec3::new(0.125, 0.0, 0.5), target: Vec3::new(0.125, 0.0, 0.0), up: Vec3::Y, fov_deg: 14.0, ortho: None };
    let shot = |m: &Material| trace(vec![clay_item(blobs.clone(), 160, m)], cam, 64, 256, sun_look([0.25, 0.2, -1.0], 3.0));
    // The ear's face, away from the head (right of the frame's middle).
    let mean = |img: &Image| {
        let px: Vec<f32> = (20..44).flat_map(|y| (34..50).map(move |x| (y, x))).map(|(y, x)| luma(img.pixels[y * 64 + x])).collect();
        px.iter().sum::<f32>() / px.len() as f32
    };
    let lit = shot(m);
    let dark = shot(&Material { subsurface: 0.0, ..m.clone() });
    (mean(&lit), mean(&dark), pixel_hash(&lit))
}

/// Light bleeds through a thin ear with each stylised skin preset far
/// more than through the same material made opaque, as it does with
/// realistic skin; each render is the same bits twice. fabric_soft has no
/// subsurface (cloth), so it is not part of this check.
#[test]
fn stylized_skin_glows_through_thin_parts() {
    for name in ["skin", "skin_stylized", "lips_stylized", "cheek_blush"] {
        let m = Material::preset(name).unwrap();
        let (glow, opaque, hash) = ear_glow(&m);
        let (_, _, again) = ear_glow(&m);
        eprintln!("{name}: backlit ear {glow:.4} against opaque {opaque:.4} ({:.1}x)", glow / opaque.max(1e-4));
        assert_eq!(hash, again, "{name}: the same render twice");
        assert!(glow > opaque * 2.0 + 0.01, "{name}: ear {glow} opaque {opaque}");
    }
    assert_eq!(Material::preset("fabric_soft").unwrap().subsurface, 0.0);
}


/// A stylised eye (24 mm across, looking down +Z at the origin) as a film
/// item.
pub(super) fn eye_item(catchlight: bool) -> Item {
    let mut eye = crate::eyeball::Eye { iris_color: [0.25, 0.45, 0.7], ..Default::default() };
    eye.stylize(&|k| k == "catchlight");
    eye.catchlight = catchlight;
    let mesh = Arc::new(eye.build_mesh_in(None));
    let m = Material::default();
    Item { mesh, world: Mat4::from_scale(Vec3::splat(0.024)), tint: [1.0; 3], surface: m.surface(), material: m }
}

/// The eye from the front under the studio lights (CPU): the image and
/// the pixels inside the iris.
fn eye_front(items: Vec<Item>) -> (Image, Vec<usize>) {
    let size = 96;
    let cam = Camera { eye: Vec3::new(0.0, 0.0, 0.15), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 12.0, ortho: None };
    let mut look = Look::default();
    look.device = look::Device::Cpu;
    let img = trace(items, cam, size, 256, look);
    // The iris, as seen: its width over the frame's at its depth.
    let mut eye = crate::eyeball::Eye::default();
    eye.stylize(&|_| false);
    let r = 0.5 * eye.iris_width() * 0.024;
    let half = (0.15 - 0.011) * (6f32).to_radians().tan();
    let r_px = r / half * size as f32 * 0.5 * 0.95;
    let inside = (0..size * size).filter(|&i| {
        let (x, y) = ((i % size) as f32 + 0.5 - size as f32 * 0.5, (i / size) as f32 + 0.5 - size as f32 * 0.5);
        x * x + y * y < r_px * r_px
    }).collect();
    (img, inside)
}

/// The plan's `stylized_eye_catchlight`: a front render of a stylised
/// eye has a highlight of luminance at least 0.9 inside the iris; the
/// catch-light is what puts it there, and it gives a skin ball beside the
/// eye no light that the render can measure.
#[test]
fn stylized_eye_catchlight() {
    let (with, iris) = eye_front(vec![eye_item(true)]);
    let (without, _) = eye_front(vec![eye_item(false)]);
    let peak = |img: &Image| iris.iter().map(|&i| luma(img.pixels[i])).fold(0.0f32, f32::max);
    let mean = |img: &Image| iris.iter().map(|&i| luma(img.pixels[i])).sum::<f32>() / iris.len() as f32;
    eprintln!("stylised eye, {} iris pixels: peak luminance {:.3} with the catch-light, {:.3} without; mean {:.4} and {:.4}", iris.len(), peak(&with), peak(&without), mean(&with), mean(&without));
    assert!(iris.len() > 500);
    assert!(peak(&with) >= 0.9, "{}", peak(&with));
    assert!(mean(&with) > mean(&without));
    assert_eq!(pixel_hash(&with), pixel_hash(&eye_front(vec![eye_item(true)]).0), "the same render twice");
    // A skin ball a little in front of the eye and to the side, seen
    // wider: its pixels are those the ball changes.
    let ball = || clay_item(serde_json::json!([{ "type": "sphere", "center": [0.022, -0.004, 0.012], "radius": 0.008, "color": [0.9, 0.68, 0.58] }]), 48, &Material::preset("skin_stylized").unwrap());
    let size = 96;
    let wide = |items: Vec<Item>| {
        let mut look = Look::default();
        look.device = look::Device::Cpu;
        trace(items, Camera { eye: Vec3::new(0.01, 0.0, 0.15), target: Vec3::new(0.01, 0.0, 0.0), up: Vec3::Y, fov_deg: 24.0, ortho: None }, size, 256, look)
    };
    let alone = wide(vec![eye_item(false)]);
    let a = wide(vec![eye_item(true), ball()]);
    let b = wide(vec![eye_item(false), ball()]);
    let region: Vec<usize> = (0..size * size).filter(|&i| (luma(b.pixels[i]) - luma(alone.pixels[i])).abs() > 0.05).collect();
    let mean_in = |img: &Image| region.iter().map(|&i| luma(img.pixels[i])).sum::<f32>() / region.len() as f32;
    let diff = (mean_in(&a) - mean_in(&b)).abs();
    eprintln!("skin ball beside the eye ({} px): mean luminance {:.4} with the catch-light, {:.4} without (difference {diff:.5})", region.len(), mean_in(&a), mean_in(&b));
    assert!(region.len() > 100);
    assert!(diff < 0.01 * mean_in(&b), "the catch-light lights the scene: {diff}");
}

/// The test head (a ball with a thin ear), backlit and lit from the
/// front-right by the studio key too, in each stylised preset, and the
/// stylised eye: the GPU tracer gives the CPU's picture (mean within 3 of
/// 255 per channel, mean pixel difference under 3). Skips without a GPU.
#[test]
fn gpu_matches_cpu_on_stylized_presets() {
    if gpu::get().is_none() {
        eprintln!("no GPU here; skipped");
        return;
    }
    let blobs = serde_json::json!([
        { "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "color": [0.9, 0.68, 0.58] },
        { "type": "ellipsoid", "center": [0.125, 0, 0], "radii": [0.035, 0.045, 0.003], "color": [0.9, 0.68, 0.58], "blend": 0.004 }
    ]);
    let mean = |img: &Image| img.pixels.iter().map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)).sum::<Vec3>() / img.pixels.len() as f32;
    let mad = |a: &Image, b: &Image| a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| (*p as f32 - *q as f32).abs()).sum::<f32>()).sum::<f32>() / (a.pixels.len() * 3) as f32;
    let head_look = |device: look::Device| {
        let mut look = Look::parse(&serde_json::json!({ "environment": { "preset": "studio", "intensity": 0.5 }, "lights": [{ "type": "sun", "direction": [0.25, 0.2, -1.0], "size": 0.5, "intensity": 3.0 }, { "type": "sun", "direction": [0.6, 0.5, 0.6], "size": 2.0, "intensity": 1.0 }] })).unwrap();
        look.device = device;
        look
    };
    let cam = Camera { eye: Vec3::new(0.06, 0.02, 0.45), target: Vec3::new(0.06, 0.0, 0.0), up: Vec3::Y, fov_deg: 34.0, ortho: None };
    for name in ["skin_stylized", "lips_stylized", "cheek_blush", "fabric_soft"] {
        let m = Material::preset(name).unwrap();
        let shot = |device| trace(vec![clay_item(blobs.clone(), 160, &m)], cam, 64, 1024, head_look(device));
        let (cpu, gpu) = (shot(look::Device::Cpu), shot(look::Device::Gpu));
        let (a, b, d) = (mean(&cpu), mean(&gpu), mad(&cpu, &gpu));
        eprintln!("{name}: cpu {a} gpu {b}, mean pixel difference {d:.2}");
        assert!(a.max_element() > 20.0, "{name}: lit");
        assert!((a - b).abs().max_element() < 3.0, "{name}: cpu {a} gpu {b}");
        assert!(d < 3.0, "{name}: pixels differ by {d:.2}");
    }
    let eye = |device| {
        let mut look = Look::default();
        look.device = device;
        trace(vec![eye_item(true)], Camera { eye: Vec3::new(0.0, 0.0, 0.15), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 12.0, ortho: None }, 64, 1024, look)
    };
    let (cpu, gpu) = (eye(look::Device::Cpu), eye(look::Device::Gpu));
    let peak = |img: &Image| img.pixels.iter().map(|p| luma(*p)).fold(0.0f32, f32::max);
    let (a, b, d) = (mean(&cpu), mean(&gpu), mad(&cpu, &gpu));
    eprintln!("stylised eye: cpu {a} gpu {b}, mean pixel difference {d:.2}, peak luminance cpu {:.3} gpu {:.3}", peak(&cpu), peak(&gpu));
    assert!((a - b).abs().max_element() < 3.0 && d < 3.0, "eye: cpu {a} gpu {b}, difference {d}");
    assert!(peak(&gpu) >= 0.9, "the GPU shows the catch-light too: {}", peak(&gpu));
}
