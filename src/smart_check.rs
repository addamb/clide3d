//! Picture checks for smart materials (ignored tests, run by hand; the GPU
//! traces them when there is one):
//!
//! - `OUT=dir cargo test --release smart_sheet -- --ignored`: every smart
//!   preset on a test piece with edges and cavities (a rounded plate with
//!   grooves and a rim, and a studded ball), one cell each.
//! - `SCENE=dragon.json OUT=dir cargo test --release smart_dragon -- --ignored`:
//!   the dragon in demon_skin.
//! - `SCENE=elf.json OUT=dir cargo test --release smart_elf -- --ignored`:
//!   the elf in old_leather boots and grimy_cloth.
//!
//! SAMPLES sets samples per pixel (default 96), ONLY picks presets.

use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Vec3};

use crate::film::{Camera, Item, Look, Settings, look::Device};
use crate::material::Material;

fn save(dir: &str, name: &str, w: usize, h: usize, px: &[[u8; 3]]) {
    std::fs::create_dir_all(dir).unwrap();
    image::save_buffer(format!("{dir}/{name}.png"), &px.iter().flatten().copied().collect::<Vec<_>>(), w as u32, h as u32, image::ColorType::Rgb8).unwrap();
}

fn orbit(target: Vec3, yaw: f32, pitch: f32, distance: f32, fov: f32) -> Camera {
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
    Camera { eye: target + dir * distance, target, up: Vec3::Y, fov_deg: fov, ortho: None }
}

fn samples() -> u32 {
    std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(96)
}

fn auto() -> Arc<Look> {
    let mut look = Look::default();
    look.device = Device::Auto;
    Arc::new(look)
}

/// The test piece: a rounded plate (pauldron-sized) with grooves, a raised
/// rim and rivets, and a ball with studs and a carved ring.
pub(crate) fn test_piece(material: &str) -> crate::clay::Clay {
    let mut blobs = vec![
        serde_json::json!({ "type": "box", "center": [-0.2, 0.2, 0], "size": [0.3, 0.36, 0.12], "round": 0.025, "blend": 0.0, "material": material }),
        serde_json::json!({ "type": "box", "center": [-0.2, 0.2, 0.055], "size": [0.26, 0.32, 0.03], "op": "carve", "blend": 0.004 }),
        serde_json::json!({ "type": "box", "center": [-0.2, 0.2, 0.045], "size": [0.22, 0.28, 0.03], "round": 0.01, "blend": 0.0, "material": material }),
        serde_json::json!({ "type": "box", "center": [-0.2, 0.2, 0.065], "size": [0.24, 0.018, 0.02], "op": "carve", "blend": 0.003 }),
        serde_json::json!({ "type": "box", "center": [-0.2, 0.28, 0.065], "size": [0.24, 0.012, 0.02], "op": "carve", "blend": 0.003 }),
        serde_json::json!({ "type": "box", "center": [-0.2, 0.12, 0.065], "size": [0.24, 0.012, 0.02], "op": "carve", "blend": 0.003 }),
        serde_json::json!({ "type": "sphere", "center": [0.17, 0.17, 0], "radius": 0.13, "blend": 0.0, "material": material }),
        serde_json::json!({ "type": "torus", "center": [0.17, 0.17, 0], "radius": 0.13, "thickness": 0.012, "rotate": [90, 0, 0], "op": "carve", "blend": 0.003 }),
    ];
    for k in 0..4 {
        let (x, y) = ([-0.33, -0.07][k % 2], [0.04, 0.36][k / 2]);
        blobs.push(serde_json::json!({ "type": "sphere", "center": [x, y, 0.06], "radius": 0.014, "blend": 0.002, "material": material }));
    }
    for k in 0..10 {
        let a = k as f32 * 2.399;
        let up = 0.2 + 0.75 * (k as f32 / 9.0);
        let d = Vec3::new(a.cos() * (1.0 - up * up).sqrt(), up, a.sin() * (1.0 - up * up).sqrt());
        let d = Vec3::new(d.x, d.y * 0.6 - 0.3, d.z.abs() * 0.8 + 0.4).normalize();
        let c = Vec3::new(0.17, 0.17, 0.0) + d * 0.125;
        blobs.push(serde_json::json!({ "type": "sphere", "center": [c.x, c.y, c.z], "radius": 0.02, "blend": 0.01, "material": material }));
    }
    serde_json::from_value(serde_json::json!({ "blobs": blobs, "resolution": 192 })).unwrap()
}

/// Renders one preset on the test piece.
fn piece_with(library: &[Material], m: &str, w: usize, h: usize, samples: u32) -> Vec<[u8; 3]> {
    let clay = test_piece(m);
    let mesh = Arc::new(clay.build_mesh_with(library, m));
    let mat = Material::resolve(library, m);
    let items = vec![Item { mesh, world: Mat4::IDENTITY, tint: [1.0; 3], surface: mat.surface(), material: mat }];
    let meshes = crate::film::Meshes::new(items);
    let cam = orbit(Vec3::new(-0.01, 0.19, 0.0), 18.0, 14.0, 0.95, 33.0);
    let s = Settings { width: w, height: h, samples, bounces: 4, time_limit: Duration::from_secs(600), floor: true, exposure: 1.0, denoise: true };
    crate::film::render_views_with_look(&meshes, &[], &[cam], s, auto(), &Default::default()).pop().unwrap().pixels
}

#[test]
#[ignore]
fn smart_sheet() {
    let out = std::env::var("OUT").expect("OUT");
    let only = std::env::var("ONLY").ok();
    let (cols, w, h) = (5, 480, 330);
    let names: Vec<&str> = crate::smart::PRESETS.iter().map(|p| p.0).filter(|n| only.as_ref().is_none_or(|o| o.split(',').any(|x| x == *n))).collect();
    let rows = names.len().div_ceil(cols);
    let mut sheet = vec![[20u8; 3]; cols * w * rows * h];
    for (k, name) in names.iter().enumerate() {
        let t = std::time::Instant::now();
        // The blood layer is shown over worn iron.
        let lib: Vec<Material> = if *name == "blood_splatter" {
            vec![serde_json::from_value(serde_json::json!({ "name": name, "layers": [{ "smart": "worn_iron" }, { "smart": "blood_splatter" }] })).unwrap()]
        } else {
            Vec::new()
        };
        let px = piece_with(&lib, name, w, h, samples());
        save(&out, &format!("piece_{name}"), w, h, &px);
        let (ox, oy) = ((k % cols) * w, (k / cols) * h);
        for y in 0..h {
            for x in 0..w {
                sheet[(oy + y) * cols * w + ox + x] = px[y * w + x];
            }
        }
        eprintln!("{name}: {:.1}s", t.elapsed().as_secs_f32());
    }
    if only.is_none() {
        save(&out, "material_sheet", cols * w, rows * h, &sheet);
    }
}

/// The raw mask inputs on the test piece, white over black: edges,
/// cavity, ao, and a noise and a grunge term.
#[test]
#[ignore]
fn smart_masks() {
    let out = std::env::var("OUT").expect("OUT");
    let mut masks = vec![
        ("edges", serde_json::json!(["edges"])),
        ("cavity", serde_json::json!(["cavity"])),
        ("ao", serde_json::json!(["ao"])),
        ("noise", serde_json::json!([{ "source": "noise", "scale": 0.03, "octaves": 4 }])),
        ("grunge", serde_json::json!([{ "source": "grunge", "map": "stone", "scale": 0.15 }])),
    ];
    if std::env::var("MAPS").is_ok() {
        masks.clear();
        for e in &crate::detail::LIBRARY {
            masks.push((e.name, serde_json::json!([{ "source": "grunge", "map": e.name, "scale": 0.15, "contrast": 2.0 }])));
        }
    }
    for (name, mask) in masks {
        let m: Material = serde_json::from_value(serde_json::json!({ "name": name, "roughness": 1.0, "layers": [
            { "color": [0.02, 0.02, 0.02], "roughness": 1.0 }, { "color": [0.9, 0.9, 0.9], "mask": mask }
        ] })).unwrap();
        let px = piece_with(&[m], name, 400, 280, 32);
        save(&out, &format!("mask_{name}"), 400, 280, &px);
    }
}

/// Loads a scene with `materials` swapped in: (blob material, new).
fn scene_items(path: &str, swap: &dyn Fn(&mut crate::scene::Scene)) -> (Vec<Item>, crate::scene::Scene) {
    let mut scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    swap(&mut scene);
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.show_fine = std::env::var("FINE").is_ok();
    meshes.sync(&scene);
    let eval = scene.evaluate();
    let items = scene
        .objects
        .iter()
        .filter(|o| o.shape.is_mesh())
        .filter_map(|o| {
            let &(world, true) = eval.get(&o.id)? else { return None };
            Some(Item { mesh: meshes.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
        })
        .filter(|i| i.mesh.textures.as_ref().is_none_or(|t| !t.alpha))
        .collect();
    (items, scene)
}

fn render(items: Vec<Item>, views: &[(&str, Camera, usize)], out: &str) {
    let meshes = crate::film::Meshes::new(items);
    for (name, cam, size) in views {
        let t = std::time::Instant::now();
        let s = Settings { width: *size, height: *size, samples: samples(), bounces: 4, time_limit: Duration::from_secs(1200), floor: true, exposure: 1.0, denoise: true };
        let img = crate::film::render_views_with_look(&meshes, &[], &[*cam], s, auto(), &Default::default()).pop().unwrap();
        save(out, name, img.width, img.height, &img.pixels);
        eprintln!("{name}: {} samples, {:.1}s", img.samples, t.elapsed().as_secs_f32());
    }
}

fn bounds(items: &[Item]) -> (Vec3, Vec3) {
    items.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), it| {
        it.mesh.vertices.iter().fold((lo, hi), |(lo, hi), v| {
            let p = it.world.transform_point3(Vec3::from(v.pos));
            (lo.min(p), hi.max(p))
        })
    })
}

#[test]
#[ignore]
fn smart_dragon() {
    let out = std::env::var("OUT").expect("OUT");
    let (items, _) = scene_items(&std::env::var("SCENE").expect("SCENE"), &|scene| {
        // The body and head default to demon_skin; wings (membrane) and
        // the mouth (gloss) stay.
        for o in &mut scene.objects {
            if o.clay.is_some() {
                o.material = Some("demon_skin".into());
            }
        }
    });
    let (lo, hi) = bounds(&items);
    let c = (lo + hi) * 0.5;
    let size = (hi - lo).max_element();
    eprintln!("dragon bounds {lo} {hi}");
    let views = [
        ("dragon_full", orbit(c, 35.0, 12.0, size * 1.6, 35.0), 900),
        ("dragon_close", orbit(Vec3::new(c.x, c.y + size * 0.1, c.z), 60.0, 10.0, size * 0.55, 35.0), 800),
    ];
    render(items, &views, &out);
}

#[test]
#[ignore]
fn smart_elf() {
    let out = std::env::var("OUT").expect("OUT");
    let (items, _) = scene_items(&std::env::var("SCENE").expect("SCENE"), &|scene| {
        for o in &mut scene.objects {
            if let Some(c) = &mut o.clay {
                for b in &mut c.blobs {
                    match b.material.as_str() {
                        "leather" => b.material = "old_leather".into(),
                        "cloth" => b.material = "grimy_cloth".into(),
                        "metal" => b.material = "worn_iron".into(),
                        _ => {}
                    }
                }
            }
        }
    });
    let views = [
        ("elf_full", orbit(Vec3::new(0.0, 0.92, 0.0), 28.0, 8.0, 4.6, 30.0), 800),
        ("elf_boots", orbit(Vec3::new(0.0, 0.3, 0.05), 25.0, 10.0, 1.1, 30.0), 700),
        ("elf_torso", orbit(Vec3::new(0.0, 1.15, 0.05), 20.0, 5.0, 1.1, 30.0), 700),
    ];
    render(items, &views, &out);
}

/// Decals on worn iron: a glowing rune, a carved rune, scratches, a blood
/// splat and an emblem (`OUT=dir cargo test --release smart_decals -- --ignored`).
#[test]
#[ignore]
fn smart_decals() {
    let out = std::env::var("OUT").expect("OUT");
    let mut clay = test_piece("worn_iron");
    let decals = serde_json::json!([
        { "shape": "rune", "center": [-0.2, 0.27, 0.062], "size": 0.09, "seed": 3, "color": [1.0, 0.4, 0.08], "emission": 4.0 },
        { "shape": "rune", "center": [-0.2, 0.135, 0.062], "size": 0.1, "seed": 8 },
        { "shape": "scratch", "center": [0.17, 0.25, 0.1], "normal": [0, 0.45, 1], "size": 0.14, "rotate": 30, "seed": 2 },
        { "shape": "splat", "center": [0.2, 0.12, 0.11], "normal": [0.2, -0.2, 1], "size": 0.16, "seed": 5 },
        { "shape": "emblem", "center": [-0.2, 0.2, -0.06], "normal": [0, 0, -1], "size": 0.14, "seed": 1, "color": [0.85, 0.62, 0.25], "metallic": 1.0, "roughness": 0.25 }
    ]);
    clay.decals = serde_json::from_value(decals).unwrap();
    clay.decals.iter_mut().for_each(crate::smart::Decal::sanitize);
    let mesh = Arc::new(clay.build_mesh_with(&[], "worn_iron"));
    let mat = Material::resolve(&[], "worn_iron");
    let meshes = crate::film::Meshes::new(vec![Item { mesh, world: Mat4::IDENTITY, tint: [1.0; 3], surface: mat.surface(), material: mat }]);
    for (name, cam) in [("decals", orbit(Vec3::new(-0.01, 0.19, 0.0), 12.0, 8.0, 0.8, 33.0)), ("decals_back", orbit(Vec3::new(-0.2, 0.2, 0.0), 180.0, 5.0, 0.5, 33.0))] {
        let s = Settings { width: 640, height: 440, samples: samples(), bounces: 4, time_limit: Duration::from_secs(600), floor: true, exposure: 1.0, denoise: true };
        let img = crate::film::render_views_with_look(&meshes, &[], &[cam], s, auto(), &Default::default()).pop().unwrap();
        save(&out, name, img.width, img.height, &img.pixels);
    }
}
