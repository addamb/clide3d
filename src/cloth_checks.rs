//! Reproducible film QA on the task's elf scene, without opening a window.
use crate::{
    cloth::{self, Cloth, Pattern},
    film,
    mesh::MeshLib,
    scene::{Scene, Shape},
};
use glam::Vec3;
use std::time::Duration;
fn trace(items: Vec<film::Item>, camera: &film::Camera, settings: film::Settings) -> film::Image {
    let meshes = film::Meshes::new(items);
    let look = film::Look::parse(&serde_json::json!({"device":"gpu"})).unwrap();
    let image = film::render_views_with_look(
        &meshes,
        &[],
        std::slice::from_ref(camera),
        settings,
        std::sync::Arc::new(look),
        &Default::default(),
    )
    .remove(0);
    assert_eq!(image.samples, 64, "film check did not finish all samples");
    eprintln!("film: {} samples in {:.1}s", image.samples, image.seconds);
    image
}
fn render(scene: &Scene, lib: &MeshLib, path: &std::path::Path, yaw: f32) {
    render_framed(scene, lib, path, yaw, Vec3::new(0.0, 1.0, 0.0), 3.8);
}
fn render_framed(
    scene: &Scene,
    lib: &MeshLib,
    path: &std::path::Path,
    yaw: f32,
    target: Vec3,
    distance: f32,
) {
    let eval = scene.evaluate();
    let items = scene
        .objects
        .iter()
        .filter_map(|o| {
            let &(world, true) = eval.get(&o.id)? else {
                return None;
            };
            Some(film::Item {
                mesh: lib.get(o)?.clone(),
                world,
                tint: o.color,
                surface: scene.surface_of(o),
                material: scene.material_of(o),
            })
        })
        .collect();
    let yaw = yaw.to_radians();
    let camera = film::Camera {
        eye: target + Vec3::new(yaw.sin(), 0.15, yaw.cos()) * distance,
        target,
        up: Vec3::Y,
        fov_deg: 32.0,
        ortho: None,
    };
    let settings = film::Settings {
        width: 768,
        height: 960,
        samples: 64,
        bounces: 3,
        time_limit: Duration::from_secs(600),
        floor: true,
        exposure: 1.0,
        denoise: true,
    };
    let img = trace(items, &camera, settings);
    assert_eq!(img.samples, 64, "film check did not finish all samples");
    image::save_buffer(
        path,
        &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        img.width as u32,
        img.height as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
}
#[test]
#[ignore]
fn elf_cloth_film_check() {
    if let Some(gpu) = film::gpu::get() {
        eprintln!("film check GPU: {}", gpu.name);
    }
    let path = std::env::var("CLOTH_SCENE").expect("CLOTH_SCENE path to elf8/elf.json");
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloth_check");
    std::fs::create_dir_all(&out).unwrap();
    let mut scene: Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    // Moderate body tessellation suffices for film QA; the SDF collision
    // continues to use the exact human mesh, independent of this budget.
    for o in &mut scene.objects {
        if let Some(c) = &mut o.clay {
            c.resolution = c.resolution.min(192);
        }
        o.bake = None;
    }
    scene.frame = None;
    let mut lib = MeshLib::new();
    lib.sync(&scene);
    render(&scene, &lib, &out.join("before.png"), 20.0);
    let parent = scene.objects.iter().find(|o| o.name == "Elf").unwrap().id;
    let belt = scene
        .get(parent)
        .unwrap()
        .clay
        .as_ref()
        .unwrap()
        .blobs
        .iter()
        .find(|b| b.name == "belt")
        .unwrap()
        .clone();
    let body = scene.get_mut(parent).unwrap();
    body.clay.as_mut().unwrap().blobs.retain(|b| {
        ![
            "tunic",
            "skirt_fill",
            "chest_fill",
            "groin_fill",
            "sleeve_upper",
            "sleeve_lower",
            "boot_shaft",
            "belt",
        ]
        .contains(&b.name.as_str())
    });
    body.clay.as_mut().unwrap().detail = None;
    body.clay.as_mut().unwrap().paint = None;
    lib.sync(&scene);
    let body = scene.get(parent).unwrap();
    let clay = body.clay.clone().unwrap();
    let mut rig = body.rig.clone().unwrap();
    let (mesh, skin) = lib.rest(parent).unwrap();
    let (mesh, skin) = (mesh.clone(), skin.cloned());
    let waist = 1.02;
    let garments = [
        (
            "Cloth_tunic",
            Pattern {
                pattern: "tunic".into(),
                length: serde_json::json!("mid_thigh"),
                belt: Some(waist),
                resolution: 56,
                sleeves: true,
                ..Default::default()
            },
            [0.20, 0.36, 0.12],
        ),
        (
            "Cloth_boot.L",
            Pattern {
                pattern: "tube".into(),
                bone: Some("shin.L".into()),
                center: Some(Vec3::new(0.16, 0.43, 0.035)),
                fit: 0.008,
                length: serde_json::json!(0.34),
                flare: 0.0,
                fabric: "leather".into(),
                resolution: 40,
                ..Default::default()
            },
            [0.36, 0.22, 0.12],
        ),
        (
            "Cloth_boot.R",
            Pattern {
                pattern: "tube".into(),
                bone: Some("shin.R".into()),
                center: Some(Vec3::new(-0.16, 0.43, 0.035)),
                fit: 0.008,
                length: serde_json::json!(0.34),
                flare: 0.0,
                fabric: "leather".into(),
                resolution: 40,
                ..Default::default()
            },
            [0.36, 0.22, 0.12],
        ),
    ];
    let mut skirt = None;
    for (name, settings, color) in garments {
        eprintln!("draping {name}");
        let mut c = Cloth::generate(settings, &mesh, Some(&rig)).unwrap();
        cloth::drape(
            &mut c,
            &clay,
            &mesh,
            skin.as_deref(),
            Some(&rig),
            1.5,
            &Default::default(),
        )
        .unwrap();
        let id = scene.add(Shape::Cloth);
        let o = scene.get_mut(id).unwrap();
        o.name = name.into();
        o.parent = Some(parent);
        o.material = Some(
            if name.contains("boot") {
                "leather"
            } else {
                "cloth"
            }
            .into(),
        );
        o.color = color;
        o.cloth = Some(c);
        if name == "Cloth_tunic" {
            skirt = Some(id);
        }
    }
    // Keep the original belt in its original ordering before the buckle.
    let blobs = &mut scene.get_mut(parent).unwrap().clay.as_mut().unwrap().blobs;
    let at = blobs.iter().position(|b| b.name == "buckle").unwrap();
    blobs.insert(at, belt);
    lib.sync(&scene);
    render(&scene, &lib, &out.join("after.png"), 20.0);
    render(&scene, &lib, &out.join("after_side.png"), 90.0);
    render(&scene, &lib, &out.join("after_three_quarter.png"), 45.0);
    render_framed(
        &scene,
        &lib,
        &out.join("shoulder_closeup.png"),
        35.0,
        Vec3::new(0.13, 1.43, 0.015),
        1.15,
    );
    render_framed(
        &scene,
        &lib,
        &out.join("waist_closeup.png"),
        20.0,
        Vec3::new(0.0, 1.01, 0.02),
        1.05,
    );
    if std::env::var_os("CLOTH_PREVIEW").is_some() {
        return;
    }
    rig.active = Some("walk".into());
    scene.get_mut(parent).unwrap().rig.as_mut().unwrap().active = rig.active.clone();
    let skirt = skirt.unwrap();
    let c = scene.get_mut(skirt).unwrap().cloth.as_mut().unwrap();
    cloth::simulate(
        c,
        &clay,
        &mesh,
        skin.as_deref().unwrap(),
        &rig,
        &(0..=16).map(|f| f as f32).collect::<Vec<_>>(),
        24.0,
        &Default::default(),
    )
    .unwrap();
    for frame in [0.0, 4.0, 8.0, 12.0, 16.0] {
        scene.frame = Some(frame);
        lib.frame = Some(frame);
        lib.sync(&scene);
        render(
            &scene,
            &lib,
            &out.join(format!("walk_{frame:02.0}.png")),
            45.0,
        );
    }
    scene.frame = None;
    std::fs::write(
        out.join("elf_cloth.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    lib.frame = None;
    lib.sync(&scene);
    let options = crate::gltf::Options {
        roots: vec![],
        animations: true,
        fps: 24.0,
        file_stem: "elf_cloth".into(),
        lods: false, atlas: true, texel_density: None,
    };
    let exported = crate::gltf::export(&scene, &lib, &options).unwrap();
    std::fs::write(out.join("elf_cloth.glb"), &exported.bytes).unwrap();
    let loaded = crate::glb_read::from_bytes(&exported.bytes, Some("walk"), Some(8.0)).unwrap();
    let target = Vec3::new(0.0, 1.0, 0.0);
    let camera = film::Camera {
        eye: target + Vec3::new(2.5, 0.5, 3.0),
        target,
        up: Vec3::Y,
        fov_deg: 32.0,
        ortho: None,
    };
    let settings = film::Settings {
        width: 768,
        height: 960,
        samples: 64,
        bounces: 3,
        time_limit: Duration::from_secs(600),
        floor: true,
        exposure: 1.0,
        denoise: true,
    };
    let img = trace(loaded.items, &camera, settings);
    image::save_buffer(
        out.join("export_walk_08.png"),
        &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        img.width as u32,
        img.height as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
    std::fs::write(
        out.join("export_check.json"),
        serde_json::to_vec_pretty(&loaded.summary).unwrap(),
    )
    .unwrap();
    // Verify a bounded textured game export as well as the high-resolution
    // cache. The garment normal bake keeps simulated folds in pattern UVs.
    let mut game = scene.clone();
    let mut game_lib = lib.clone();
    for o in &mut game.objects {
        let triangles = match o.shape {
            Shape::Clay => {
                if o.name == "Elf" {
                    70_000
                } else {
                    30_000
                }
            }
            Shape::Cloth => {
                if o.name.contains("tunic") {
                    14_000
                } else if o.name.contains("skirt") {
                    2000
                } else {
                    2500
                }
            }
            _ => continue,
        };
        o.bake = Some(crate::bake::Settings {
            triangles,
            texture: 512,
            ..Default::default()
        });
    }
    game_lib.sync(&game);
    for o in game.objects.iter().filter(|o| o.bake.is_some()) {
        eprintln!("baking {}", o.name);
        game_lib.bake(&game, o.id, o.bake.unwrap()).unwrap();
    }
    let options = crate::gltf::Options {
        roots: vec![],
        animations: true,
        fps: 24.0,
        file_stem: "elf_cloth_game".into(),
        lods: false, atlas: true, texel_density: None,
    };
    let game_glb = crate::gltf::export(&game, &game_lib, &options).unwrap();
    std::fs::write(out.join("elf_cloth_game.glb"), &game_glb.bytes).unwrap();
    let loaded = crate::glb_read::from_bytes(&game_glb.bytes, Some("walk"), Some(8.0)).unwrap();
    assert!(
        loaded.summary["triangles"].as_u64().unwrap() < 200_000,
        "game scene exceeds 200k triangles"
    );
    let img = trace(loaded.items, &camera, settings);
    image::save_buffer(
        out.join("game_export_walk_08.png"),
        &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        img.width as u32,
        img.height as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
    std::fs::write(
        out.join("game_export_check.json"),
        serde_json::to_vec_pretty(&loaded.summary).unwrap(),
    )
    .unwrap();
    let game_fbx = crate::fbx::export(&game, &game_lib, &options).unwrap();
    std::fs::write(out.join("elf_cloth_game.fbx"), &game_fbx.bytes).unwrap();
    for (name, bytes) in game_fbx.files {
        std::fs::write(out.join(name), bytes).unwrap();
    }
}
