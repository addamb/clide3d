//! Reproducible renders use the real film tracer, with a separate GPU comparison.
use crate::{
    film::{self, Camera, Item, Look, Meshes, Settings, lighting::Environment, look::Device},
    fire::{self, Fire, Settings as FireSettings},
    material::Material,
    mesh::Mesh,
    progress::Progress,
    scene::{Scene, Shape},
};
use glam::{Mat4, Vec3};
use std::{sync::Arc, time::Duration};
fn source(preset: &str) -> Fire {
    fire::simulate(
        FireSettings {
            resolution: 32,
            frames: 12,
            ..FireSettings::preset(preset).unwrap()
        },
        &Progress::default(),
    )
    .unwrap()
}
fn render(
    fire: &Fire,
    frame: usize,
    device: Device,
    size: usize,
    mut items: Vec<Item>,
) -> film::Image {
    let material = Material::default();
    items.push(Item {
        mesh: Arc::new(fire.ember_mesh(Some(frame as f32))),
        world: Mat4::IDENTITY,
        tint: [1.0; 3],
        surface: material.surface(),
        material,
    });
    let r = fire.settings.radius;
    let v = fire.volume(Mat4::IDENTITY, Some(frame as f32)).unwrap();
    let mut look = Look::parse(
        &serde_json::json!({"environment":"night","background":true,"lights":"none","bloom":0.008,"tonemap":"aces"}),
    )
    .unwrap();
    look.device = device;
    look.lights = v.lights();
    look.volumes = vec![v];
    if fire.settings.preset == "smoke_column" {
        look.lights.push(film::lighting::Light::Sun {
            direction: Vec3::new(-0.4, 0.7, 0.6).normalize(),
            size: 0.08,
            power: Vec3::splat(8.0),
        });
    }
    let smoke = fire.settings.preset == "smoke_column";
    let center = Vec3::Y * r * if smoke { 3.0 } else { 1.2 };
    let cam = Camera {
        eye: Vec3::new(
            r * if smoke { 9.0 } else { 6.0 },
            r * if smoke { 5.0 } else { 3.0 },
            r * if smoke { 18.0 } else { 12.0 },
        ),
        target: center,
        up: Vec3::Y,
        fov_deg: 35.0,
        ortho: None,
    };
    film::render_views_with_look(
        &Meshes::new(items),
        &[],
        &[cam],
        Settings {
            width: size,
            height: size,
            samples: if size >= 512 { 64 } else { 8 },
            bounces: 2,
            time_limit: Duration::from_secs(600),
            floor: false,
            exposure: 0.6,
            denoise: size >= 512,
        },
        Arc::new(look),
        &Progress::default(),
    )
    .pop()
    .unwrap()
}
#[test]
fn gpu_matches_cpu_fire_smoke() {
    if film::gpu::get().is_none() {
        eprintln!("no GPU: skipped fire comparison");
        return;
    }
    for preset in ["torch", "campfire", "brazier", "candle", "smoke_column"] {
        let f = source(preset);
        let cpu = render(&f, 5, Device::Cpu, 32, Vec::new());
        let gpu = render(&f, 5, Device::Gpu, 32, Vec::new());
        gpu.traced.assert_honours(Device::Gpu);
        assert_eq!(gpu.samples, 8);
        assert_eq!(cpu.samples, 8);
        let error = cpu
            .pixels
            .iter()
            .zip(&gpu.pixels)
            .flat_map(|(a, b)| (0..3).map(move |i| (a[i] as f32 - b[i] as f32).abs()))
            .sum::<f32>()
            / (32.0 * 32.0 * 3.0);
        std::fs::create_dir_all("fire_check").unwrap();
        for (tag, img) in [("cpu", &cpu), ("gpu", &gpu)] {
            image::save_buffer(
                format!("fire_check/{preset}_{tag}.png"),
                &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
                32,
                32,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        assert!(error < 2.5, "{preset} CPU/GPU mean byte error {error}");
    }
}
#[test]
fn fire_illuminates_nearby_surface_and_smoke_shadows() {
    let f = source("torch");
    let v = f.volume(Mat4::IDENTITY, None).unwrap();
    let m = Material::default();
    let items = vec![Item {
        mesh: Arc::new(Mesh::build(Shape::Sphere)),
        world: Mat4::from_scale_rotation_translation(
            Vec3::splat(0.09),
            glam::Quat::IDENTITY,
            Vec3::new(0.15, 0.12, 0.0),
        ),
        tint: [0.7; 3],
        surface: m.surface(),
        material: m,
    }];
    let mut look = Look::default();
    look.environment = Arc::new(Environment::map(1, 1, vec![Vec3::ZERO], 0.0));
    look.lights = v.lights();
    look.background = true;
    look.device = Device::Cpu;
    let camera = Camera {
        eye: Vec3::new(0.15, 0.12, 0.6),
        target: Vec3::new(0.15, 0.12, 0.0),
        up: Vec3::Y,
        fov_deg: 30.0,
        ortho: None,
    };
    let meshes = Meshes::new(items);
    let settings = Settings {
        width: 32,
        height: 32,
        samples: 32,
        bounces: 1,
        time_limit: Duration::from_secs(60),
        floor: false,
        exposure: 1.0,
        denoise: false,
    };
    let lit = film::render_views_with_look(
        &meshes,
        &[],
        &[camera],
        settings,
        Arc::new(look),
        &Progress::default(),
    )
    .pop()
    .unwrap();
    look = Look::default();
    look.environment = Arc::new(Environment::map(1, 1, vec![Vec3::ZERO], 0.0));
    look.lights.clear();
    look.background = true;
    look.device = Device::Cpu;
    let dark = film::render_views_with_look(
        &meshes,
        &[],
        &[camera],
        settings,
        Arc::new(look),
        &Progress::default(),
    )
    .pop()
    .unwrap();
    let energy = |im: &film::Image| {
        im.pixels
            .iter()
            .map(|p| p[0] as usize + p[1] as usize + p[2] as usize)
            .sum::<usize>()
    };
    assert!(
        energy(&lit) > energy(&dark) + 1000,
        "fire light did not reach surface"
    );
    assert!(fire::transmittance(&[v], Vec3::new(0.0, 0.12, -1.0), Vec3::Z, 2.0) < 1.0);
}
fn contact(paths: &[String], columns: usize, output: &str) {
    let mut sheet = image::RgbImage::new(
        (columns * 512) as u32,
        (paths.len().div_ceil(columns) * 512) as u32,
    );
    for (i, path) in paths.iter().enumerate() {
        let im = image::open(path).unwrap().to_rgb8();
        let im = image::DynamicImage::ImageRgb8(im)
            .resize(512, 512, image::imageops::FilterType::Lanczos3)
            .to_rgb8();
        image::imageops::replace(
            &mut sheet,
            &im,
            ((i % columns) * 512 + (512 - im.width() as usize) / 2) as i64,
            ((i / columns) * 512 + (512 - im.height() as usize) / 2) as i64,
        );
    }
    sheet.save(output).unwrap();
}

fn torch_pairs() {
    for frame in [0, 3, 6, 9] {
        contact(
            &[
                format!("fire_check/round2/torch_{frame:02}.png"),
                format!("fire_check/torch_{frame:02}.png"),
            ],
            2,
            &format!("fire_check/torch_{frame:02}_before_after.png"),
        );
    }
}

#[test]
#[ignore = "writes fire_check image sequence and editable scene; run explicitly"]
fn fire_film_checks() {
    if std::env::var("FIRE_CHECK_CONTACT_ONLY").is_ok() {
        torch_pairs();
        return;
    }
    assert!(
        film::gpu::get().is_some(),
        "visual checks require a GPU adapter"
    );
    std::fs::create_dir_all("fire_check").unwrap();
    for preset in ["torch", "brazier", "campfire", "candle", "smoke_column"] {
        if std::env::var("FIRE_CHECK_PRESET").is_ok_and(|p| p != preset) {
            continue;
        }
        let started = std::time::Instant::now();
        let f = fire::simulate(
            FireSettings {
                resolution: 64,
                frames: 12,
                ..FireSettings::preset(preset).unwrap()
            },
            &Progress::default(),
        )
        .unwrap();
        eprintln!(
            "{preset}: simulated in {:.1}s",
            started.elapsed().as_secs_f32()
        );
        let size = std::env::var("FIRE_CHECK_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512);
        let mut items = Vec::new();
        let piece = match preset {
            "torch" => Some(crate::envkit::kit::Piece::Torch),
            "brazier" => Some(crate::envkit::kit::Piece::Brazier),
            "candle" => Some(crate::envkit::kit::Piece::Candle),
            _ => None,
        };
        if let Some(piece) = piece {
            let mut kit = crate::envkit::kit::Kit::new(piece);
            kit.lit = Some(false);
            let mesh = kit.build(&[], None);
            let offset = piece.light().unwrap().offset;
            let m = Material::default();
            items.push(Item {
                mesh: Arc::new(mesh),
                world: Mat4::from_translation(-offset),
                tint: [1.0; 3],
                surface: m.surface(),
                material: m,
            });
        }
        if preset == "campfire" {
            for k in 0..4 {
                let material = Material::preset("wood").unwrap();
                let angle = k as f32 * 0.9;
                items.push(Item {
                    mesh: Arc::new(
                        crate::envkit::kit::Kit::new(crate::envkit::kit::Piece::Log)
                            .build(&[], None),
                    ),
                    world: Mat4::from_scale_rotation_translation(
                        Vec3::new(0.13, 0.8, 0.13),
                        glam::Quat::from_rotation_y(angle)
                            * glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                        Vec3::new((k as f32 - 1.5) * 0.06, -0.05, 0.0),
                    ),
                    tint: [1.0; 3],
                    surface: material.surface(),
                    material,
                });
            }
        }
        if matches!(preset, "campfire" | "brazier") {
            let r = f.settings.radius;
            for k in 0..32 {
                let a = k as f32 * 2.399;
                let d = r * (k as f32 / 32.0).sqrt();
                let m = Material {
                    emission: 2.5 + (k % 4) as f32,
                    roughness: 1.0,
                    ..Default::default()
                };
                items.push(Item {
                    mesh: Arc::new(Mesh::build(Shape::Sphere)),
                    world: Mat4::from_scale_rotation_translation(
                        Vec3::new(r * 0.18, r * 0.09, r * 0.15),
                        glam::Quat::from_rotation_y(a),
                        Vec3::new(a.cos() * d, -r * 0.08, a.sin() * d),
                    ),
                    tint: [0.8, 0.16, 0.018],
                    surface: m.surface(),
                    material: m,
                });
            }
        }
        let frames = if std::env::var("FIRE_CHECK_PREVIEW").is_ok() {
            vec![3]
        } else if preset == "torch" {
            (0..12).collect()
        } else {
            vec![5]
        };
        for frame in frames {
            let image = render(
                &f,
                frame,
                Device::Gpu,
                size,
                items
                    .iter()
                    .map(|i| Item {
                        mesh: i.mesh.clone(),
                        world: i.world,
                        tint: i.tint,
                        surface: i.surface,
                        material: i.material.clone(),
                    })
                    .collect(),
            );
            assert_eq!(
                image.samples,
                if size >= 512 { 64 } else { 8 },
                "render ended before requested sample count"
            );
            let path = format!("fire_check/{preset}_{frame:02}.png");
            let pixels: Vec<u8> = image.pixels.iter().flatten().copied().collect();
            image::save_buffer(
                &path,
                &pixels,
                size as u32,
                size as u32,
                image::ColorType::Rgb8,
            )
            .unwrap();
            eprintln!("{path}: {} samples in {:.1}s", image.samples, image.seconds);
        }
        if std::env::var("FIRE_CHECK_PREVIEW").is_ok() {
            continue;
        }
        if size == 512 {
            if preset == "torch" {
                contact(
                    &(0..12)
                        .map(|f| format!("fire_check/torch_{f:02}.png"))
                        .collect::<Vec<_>>(),
                    4,
                    "fire_check/torch_sequence.png",
                );
                torch_pairs();
            }
            let frame = if preset == "torch" { 3 } else { 5 };
            let before = format!("fire_check/round2/{preset}_{frame:02}.png");
            if std::path::Path::new(&before).exists() {
                contact(
                    &[before, format!("fire_check/{preset}_{frame:02}.png")],
                    2,
                    &format!("fire_check/{preset}_before_after.png"),
                );
            }
        }

        let mut scene = Scene::default();
        let id = scene.add(Shape::Fire);
        scene.get_mut(id).unwrap().fire = Some(f.clone());
        scene.get_mut(id).unwrap().translation = Vec3::ZERO;
        if let Some(piece) = piece {
            let id = scene.add(Shape::Kit);
            let o = scene.get_mut(id).unwrap();
            let mut kit = crate::envkit::kit::Kit::new(piece);
            kit.lit = Some(false);
            o.kit = Some(kit);
            o.translation = -piece.light().unwrap().offset;
            o.light = None;
        }
        if preset == "campfire" {
            for k in 0..4 {
                let id = scene.add(Shape::Kit);
                let o = scene.get_mut(id).unwrap();
                o.kit = Some(crate::envkit::kit::Kit::new(crate::envkit::kit::Piece::Log));
                o.set_local_matrix(Mat4::from_scale_rotation_translation(
                    Vec3::new(0.13, 0.8, 0.13),
                    glam::Quat::from_rotation_y(k as f32 * 0.9)
                        * glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                    Vec3::new((k as f32 - 1.5) * 0.06, -0.05, 0.0),
                ));
            }
        }
        std::fs::write(
            format!("fire_check/{preset}.json"),
            serde_json::to_vec(&scene).unwrap(),
        )
        .unwrap();
        let (size, pixels) = fire::atlas(&f, 64, 2, 12);
        image::save_buffer(
            format!("fire_check/{preset}_atlas.png"),
            &pixels,
            size as u32,
            size as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.sync(&scene);
        let export = crate::gltf::export(
            &scene,
            &meshes,
            &crate::gltf::Options {
                roots: vec![],
                animations: true,
                fps: 24.0,
                file_stem: preset.into(),
                lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new(),
            },
        )
        .unwrap();
        std::fs::write(format!("fire_check/{preset}.glb"), export.bytes).unwrap();
    }
}

#[test]
#[ignore = "renders the MCP-built crypt with kit flames converted to volumes"]
fn fire_crypt_check() {
    let mut scene: Scene =
        serde_json::from_slice(&std::fs::read("env_check/crypt_mcp.json").unwrap()).unwrap();
    let eval = scene.evaluate();
    let emitters: Vec<_> = crate::envkit::scatter::draws(&scene, &eval)
        .iter()
        .filter_map(|d| {
            let kit = d.object.kit.as_ref()?;
            d.object.light.as_ref()?;
            // Saved round-one lights contain the old, floating offsets.
            let l = kit.piece.light()?;
            Some((
                kit.piece.name().to_string(),
                d.world * Mat4::from_translation(l.offset),
                d.copy,
            ))
        })
        .collect();
    // At this wide framing 64 cells are sufficient for the figures; keep
    // the lighting QA quick without changing the original MCP scene.
    for o in &mut scene.objects {
        if let Some(c) = &mut o.clay {
            c.resolution = c.resolution.min(64);
        }
        if let Some(k) = &mut o.kit
            && k.piece.light().is_some()
        {
            k.lit = Some(false);
            o.light = None;
        }
    }
    let close_world = emitters
        .iter()
        .find(|(preset, world, _)| preset == "torch" && world.w_axis.x < -1.5)
        .expect("crypt wall torch")
        .1;
    let mut templates = std::collections::HashMap::new();
    for (preset, world, seed) in emitters {
        let f = templates
            .entry(preset.clone())
            .or_insert_with(|| {
                fire::simulate(
                    FireSettings {
                        resolution: 12,
                        frames: 1,
                        warmup: 2.0,
                        ..FireSettings::preset(&preset).unwrap()
                    },
                    &Progress::default(),
                )
                .unwrap()
            })
            .clone();
        let id = scene.add(Shape::Fire);
        let o = scene.get_mut(id).unwrap();
        o.name = format!("{preset}_volume_{seed}");
        o.set_local_matrix(world);
        o.fire = Some(if world == close_world {
            fire::simulate(
                FireSettings {
                    resolution: 64,
                    frames: 1,
                    warmup: 1.5,
                    ..FireSettings::preset("torch").unwrap()
                },
                &Progress::default(),
            )
            .unwrap()
        } else {
            f
        });
    }
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    let eval = scene.evaluate();
    let draws = crate::envkit::scatter::draws(&scene, &eval);
    let items: Vec<_> = draws
        .iter()
        .filter(|d| d.object.shape.is_mesh() && d.object.shape != Shape::Fire)
        .filter_map(|d| {
            Some(Item {
                mesh: meshes.get(d.object)?.clone(),
                world: d.world,
                tint: d.object.color,
                surface: scene.surface_of(d.object),
                material: scene.material_of(d.object),
            })
        })
        .collect();
    let mut look = Look::parse(
        &serde_json::json!({"environment":"night","lights":"none","background":true,"bloom":0.02}),
    )
    .unwrap();
    for d in draws {
        if let Some(v) = d.object.fire.as_ref().and_then(|f| f.volume(d.world, None)) {
            look.lights.extend(v.lights());
            look.volumes.push(v);
        }
    }
    look.device = Device::Gpu;
    let camera = Camera {
        eye: Vec3::new(9.0, 7.0, 10.0),
        target: Vec3::new(0.0, 1.0, -0.5),
        up: Vec3::Y,
        fov_deg: 37.0,
        ortho: None,
    };
    let meshes = Meshes::new(items);
    let mut look = Arc::new(look);
    let root = close_world.transform_point3(Vec3::ZERO);
    let close_camera = Camera {
        eye: root + Vec3::new(0.38, 0.17, 1.15),
        target: root + Vec3::new(0.0, 0.07, -0.03),
        up: Vec3::Y,
        fov_deg: 34.0,
        ortho: None,
    };
    for (name, camera, exposure) in [
        ("crypt", camera, 2.0),
        ("crypt_torch_closeup", close_camera, 0.6),
    ] {
        if name == "crypt_torch_closeup" {
            // The wall fills the view: distant volumes cannot enter its rays.
            // Keep the hero and its illumination without marching every fire
            // elsewhere in the crypt for each close-up ray.
            let close = Arc::get_mut(&mut look).unwrap();
            close.volumes.retain(|v| {
                v.inverse
                    .inverse()
                    .transform_point3(Vec3::ZERO)
                    .distance(root)
                    < 1.0
            });
            close.lights = close.volumes.iter().flat_map(|v| v.lights()).collect();
        }
        let image = film::render_views_with_look(
            &meshes,
            &[],
            &[camera],
            Settings {
                width: 512,
                height: 512,
                samples: 128,
                bounces: 3,
                time_limit: Duration::from_secs(600),
                floor: false,
                exposure,
                denoise: true,
            },
            look.clone(),
            &Progress::default(),
        )
        .pop()
        .unwrap();
        std::fs::create_dir_all("fire_check").unwrap();
        image::save_buffer(
            format!("fire_check/{name}.png"),
            &image.pixels.iter().flatten().copied().collect::<Vec<_>>(),
            512,
            512,
            image::ColorType::Rgb8,
        )
        .unwrap();
        assert_eq!(image.samples, 128);
        eprintln!("{name}: {} samples in {:.1}s", image.samples, image.seconds);
    }
    contact(
        &[
            "fire_check/round2/crypt.png".into(),
            "fire_check/crypt.png".into(),
        ],
        2,
        "fire_check/crypt_before_after.png",
    );
    std::fs::write("fire_check/crypt.json", serde_json::to_vec(&scene).unwrap()).unwrap();
}
