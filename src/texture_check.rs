//! Picture checks for surface detail and hair (ignored tests, run by hand):
//! `SCENE=elf.json OUT=dir cargo test --release texture_check -- --ignored --nocapture`
//! saves a full-body view, face close-ups (front and three-quarter) and a
//! hair close-up from behind. SAMPLES sets samples per pixel (64).

use std::time::Duration;

use glam::Vec3;

use crate::film::{Camera, Fiber, Item, Settings};

fn save(dir: &str, name: &str, img: &crate::film::Image) {
    std::fs::create_dir_all(dir).unwrap();
    image::save_buffer(format!("{dir}/{name}.png"), &img.pixels.iter().flatten().copied().collect::<Vec<_>>(), img.width as u32, img.height as u32, image::ColorType::Rgb8).unwrap();
}

fn orbit(target: Vec3, yaw: f32, pitch: f32, distance: f32, fov: f32) -> Camera {
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
    Camera { eye: target + dir * distance, target, up: Vec3::Y, fov_deg: fov, ortho: None }
}

#[test]
#[ignore]
fn texture_check() {
    let path = std::env::var("SCENE").expect("SCENE");
    let out = std::env::var("OUT").expect("OUT");
    let samples = std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(64);
    let only = std::env::var("VIEWS").ok();
    let scale: f32 = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.show_fine = true;
    meshes.sync(&scene);
    let eval = scene.evaluate();
    let items: Vec<Item> = scene
        .objects
        .iter()
        .filter(|o| o.shape.is_mesh())
        .filter_map(|o| {
            let &(world, true) = eval.get(&o.id)? else { return None };
            Some(Item { mesh: meshes.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
        })
        .filter(|i| i.mesh.textures.as_ref().is_none_or(|t| !t.alpha))
        .collect();
    let mut fibers = Vec::new();
    for o in scene.objects.iter().filter(|o| o.shape == crate::scene::Shape::Hair) {
        let (Some(&(world, true)), Some(hair)) = (eval.get(&o.id), o.hair.as_ref()) else { continue };
        for s in hair.strands() {
            fibers.push(Fiber { points: s.points.iter().map(|p| world.transform_point3(*p)).collect(), root_width: s.root_width, tip_width: s.tip_width, root_color: s.root_color, tip_color: s.tip_color });
        }
    }
    let meshes = crate::film::Meshes::new(items);
    let face = Vec3::new(0.0, 1.69, 0.06);
    let views = [
        ("full", orbit(Vec3::new(0.0, 0.92, 0.0), 28.0, 8.0, 4.6, 30.0), 768),
        ("face", orbit(face, 0.0, 0.0, 0.62, 30.0), 640),
        ("face34", orbit(face, 35.0, 4.0, 0.62, 30.0), 640),
        ("hair", orbit(Vec3::new(0.0, 1.52, -0.04), 155.0, 16.0, 1.5, 30.0), 640),
        ("torso", orbit(Vec3::new(0.0, 1.15, 0.05), 20.0, 5.0, 1.1, 30.0), 640),
        ("boots", orbit(Vec3::new(0.0, 0.3, 0.05), 25.0, 10.0, 1.1, 30.0), 640),
        ("skin", orbit(Vec3::new(0.045, 1.66, 0.08), 30.0, 0.0, 0.22, 30.0), 640),
        ("cloth", orbit(Vec3::new(0.06, 1.2, 0.11), 10.0, 0.0, 0.3, 30.0), 640),
    ];
    for (name, cam, size) in views {
        if only.as_ref().is_some_and(|v| !v.split(',').any(|n| n == name)) {
            continue;
        }
        let size = (size as f32 * scale) as usize;
        let s = Settings { width: size, height: size, samples, bounces: 4, time_limit: Duration::from_secs(1200), floor: true, exposure: 1.0, denoise: true };
        let t = std::time::Instant::now();
        let img = crate::film::render_views(&meshes, &fibers, &[cam], s, &Default::default()).pop().unwrap();
        save(&out, name, &img);
        eprintln!("{name}: {} samples, {:.1}s", img.samples, t.elapsed().as_secs_f32());
    }
}

/// A sheet of spheres, one per library detail texture (in `LIBRARY`
/// order, 6 per row), each about two tiles across in a fitting material:
/// `OUT=dir cargo test --release detail_sheet -- --ignored`.
#[test]
#[ignore]
fn detail_sheet() {
    use crate::material::Material;
    let out = std::env::var("OUT").expect("OUT");
    let samples = std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(64);
    let scale: f32 = std::env::var("TILE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
        "blobs": [{ "type": "sphere", "center": [0, 0.2, 0], "radius": 0.2, "blend": 0.0 }], "resolution": 96
    }))
    .unwrap();
    let mut mesh = clay.build_mesh();
    mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
    let mesh = std::sync::Arc::new(mesh);
    let (cols, cell) = (6, 256usize);
    let lib = &crate::detail::LIBRARY;
    let rows = lib.len().div_ceil(cols);
    let mut sheet = vec![[0u8; 3]; cols * cell * rows * cell];
    for (k, e) in lib.iter().enumerate() {
        let preset = match e.name {
            "linen" | "jersey" | "knit" | "felt" | "denim" => "cloth",
            "velvet" => "velvet",
            "leather_smooth" | "leather_rough" | "leather_grain" | "suede" => "leather",
            "skin_pores" | "wrinkles" => "skin",
            "metal_brushed" | "metal_scratched" => "metal",
            "wood" => "wood",
            "scales" => "scales",
            _ => "clay",
        };
        let mut m = Material::preset(preset).unwrap();
        m.detail = Some(e.name.to_string());
        m.detail_scale = if scale > 0.0 { scale } else { 0.2 };
        m.detail_color = 1.0;
        if e.name == "skin_pores" || e.name == "wrinkles" {
            m.detail_strength = 1.0;
        }
        let tint = match preset {
            "cloth" | "velvet" => [0.32, 0.5, 0.28],
            "leather" => [0.45, 0.26, 0.14],
            "skin" => [0.85, 0.66, 0.56],
            "metal" => [0.75, 0.75, 0.76],
            "wood" => [0.55, 0.36, 0.2],
            "scales" => [0.3, 0.36, 0.22],
            _ => [0.55, 0.53, 0.5],
        };
        let items = vec![Item { mesh: mesh.clone(), world: glam::Mat4::IDENTITY, tint, surface: m.surface(), material: m }];
        let cam = orbit(Vec3::new(0.0, 0.2, 0.0), 0.0, 10.0, 1.25, 20.0);
        let s = Settings { width: cell, height: cell, samples, bounces: 3, time_limit: Duration::from_secs(300), floor: false, exposure: 1.0, denoise: true };
        let img = crate::film::render(items, &[], &cam, s, &Default::default());
        let (ox, oy) = ((k % cols) * cell, (k / cols) * cell);
        for y in 0..cell {
            for x in 0..cell {
                sheet[(oy + y) * cols * cell + ox + x] = img.pixels[y * cell + x];
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    image::save_buffer(format!("{out}/detail_sheet.png"), &sheet.iter().flatten().copied().collect::<Vec<_>>(), (cols * cell) as u32, (rows * cell) as u32, image::ColorType::Rgb8).unwrap();
}

/// Saves the card texture (color over gray, by alpha) of each hair object
/// in SCENE: `SCENE=elf.json OUT=dir cargo test --release hair_card_textures -- --ignored`.
#[test]
#[ignore]
fn hair_card_textures() {
    let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(std::env::var("SCENE").expect("SCENE")).unwrap()).unwrap();
    let out = std::env::var("OUT").expect("OUT");
    std::fs::create_dir_all(&out).unwrap();
    for o in scene.objects.iter().filter(|o| o.hair.is_some()) {
        let mesh = o.hair.as_ref().unwrap().build_mesh();
        let tex = mesh.textures.as_ref().unwrap();
        let rgb: Vec<u8> = tex.color.chunks_exact(4).flat_map(|p| {
            let a = p[3] as f32 / 255.0;
            [0, 1, 2].map(|c| (p[c] as f32 * a + 90.0 * (1.0 - a)) as u8)
        }).collect();
        image::save_buffer(format!("{out}/cards_{}.png", o.name), &rgb, tex.size, tex.size, image::ColorType::Rgb8).unwrap();
    }
}

/// Brightness (0..255 per channel) of a slab of `color` hair fibers and of
/// a matte card of the same color, side by side under even light.
pub(crate) fn hair_slab(color: [f32; 3], samples: u32) -> (Vec3, Vec3) {
    use std::sync::Arc;
    use crate::material::Material;
    let width = 0.0008;
    let lin = Vec3::from(color).powf(2.2);
    let mut fibers = Vec::new();
    for layer in 0..24 {
        for i in 0..80 {
            let x = -0.07 + i as f32 * width * 0.9 + if layer % 2 == 1 { width * 0.45 } else { 0.0 };
            let z = -(layer as f32) * width * 0.7;
            fibers.push(Fiber { points: (0..=4).map(|k| Vec3::new(x, 0.06 - k as f32 * 0.03, z)).collect(), root_width: width, tip_width: width, root_color: lin, tip_color: lin });
        }
    }
    let mut card = crate::mesh::Mesh::build(crate::scene::Shape::Plane);
    for v in &mut card.vertices {
        v.color = color;
        v.surface = crate::mesh::OBJECT_SURFACE;
    }
    let m = Material { roughness: 1.0, ..Material::default() };
    // The plane lies in XZ; stand it up facing +Z, to the right of the slab.
    let world = glam::Mat4::from_translation(Vec3::new(0.06, 0.0, 0.0)) * glam::Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2) * glam::Mat4::from_scale(Vec3::splat(0.06));
    let meshes = crate::film::Meshes::new(vec![Item { mesh: Arc::new(card), world, tint: [1.0; 3], surface: m.surface(), material: m }]);
    let mut look = crate::film::Look::default();
    look.environment = Arc::new(crate::film::lighting::Environment::map(8, 4, vec![Vec3::splat(0.6); 32], 0.0));
    look.lights.clear();
    look.background = true;
    let cam = Camera { eye: Vec3::new(0.0, 0.0, 1.0), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 0.0, ortho: Some(0.08) };
    let s = Settings { width: 64, height: 64, samples, bounces: 4, time_limit: Duration::from_secs(120), floor: false, exposure: 1.0, denoise: false };
    let img = crate::film::render_views_with_look(&meshes, &fibers, &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
    let mean = |x0: usize, x1: usize| {
        let px: Vec<Vec3> = (24..40).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| Vec3::from(img.pixels[y * 64 + x].map(f32::from))).collect();
        px.iter().sum::<Vec3>() / px.len() as f32
    };
    // Slab: x -0.06..0 (pixels ~8..30); card: x 0.01..0.11 (pixels ~38..60).
    (mean(10, 26), mean(42, 56))
}

#[test]
#[ignore]
fn hair_color_report() {
    for c in [[1.0, 1.0, 1.0], [0.78, 0.6, 0.38], [0.45, 0.28, 0.15], [0.12, 0.08, 0.06], [0.9, 0.85, 0.7]] {
        let (hair, card) = hair_slab(c, 64);
        eprintln!("{c:?}: hair {hair:.0} card {card:.0}");
    }
}

/// Hair looks the color it is set to: a thick layer of fibers is about as
/// bright, and the same hue, as a matte card of that color (at the 4
/// bounces film renders use).
#[test]
fn hair_renders_its_set_color() {
    for c in [[0.78, 0.6, 0.38], [0.45, 0.28, 0.15]] {
        let (hair, card) = hair_slab(c, 32);
        let luma = |v: Vec3| v.dot(Vec3::new(0.3, 0.59, 0.11));
        assert!((luma(hair) / luma(card) - 1.0).abs() < 0.1, "{c:?}: hair {hair} card {card}");
        let ratio = hair / card;
        assert!(ratio.min_element() > 0.8 && ratio.max_element() < 1.2, "{c:?}: hair {hair} card {card}");
    }
}
