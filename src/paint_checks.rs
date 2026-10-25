//! Reproducible paint acceptance scenes through render_film's native pipeline.
use crate::{
    clay::Clay,
    damage, film,
    material::Material,
    scene::{Scene, Shape},
};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
fn output() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("paint_check");
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn render(name: &str, items: Vec<film::Item>, camera: film::Camera, w: usize, h: usize) {
    let mut args = json!({"environment":"overcast","lights":[{"type":"sun","direction":[0.5,0.8,0.7],"intensity":2.0,"size":4}],"camera":{"position":camera.eye.to_array(),"target":camera.target.to_array(),"fov":camera.fov_deg},"tonemap":"agx","grain":0.003,"sharpen":0.15,"size":w,"aspect":w as f32/h as f32});
    let samples = std::env::var("PAINT_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128);
    let image = film::render_views_with_look(
        &film::Meshes::new(items),
        &[],
        &[camera],
        film::Settings {
            width: w,
            height: h,
            samples,
            bounces: 4,
            time_limit: Duration::from_secs(600),
            floor: false,
            exposure: 1.0,
            denoise: true,
        },
        Arc::new(film::Look::parse(&args).unwrap()),
        &Default::default(),
    )
    .pop()
    .unwrap();
    image::save_buffer(
        output().join(format!("{name}.png")),
        &image.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        w as u32,
        h as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
    args["samples"] = json!(image.samples);
    args["bounces"] = json!(4);
    args["frame"] = json!(0);
    args["floor"] = json!(false);
    args["baked"] = json!(false);
    args["fine"] = json!(true);
    args["exposure"] = json!(1.0);
    args["denoise"] = json!(true);
    args["time_limit"] = json!(600);
    std::fs::write(
        output().join(format!("{name}.json")),
        serde_json::to_vec_pretty(
            &json!({"render_film":args,"samples":image.samples,"seconds":image.seconds}),
        )
        .unwrap(),
    )
    .unwrap();
    eprintln!("{name}: {} spp {:.1}s", image.samples, image.seconds);
}
#[test]
#[ignore]
fn paint_material_sheet() {
    for preset in [
        "factory_new",
        "military_paint",
        "battle_worn",
        "desert_dusty",
        "muddy",
        "burnt_wreck",
        "dusty_khaki",
        "olive_drab",
        "gunmetal",
        "worn_steel",
    ] {
        let mut items = Vec::new();
        for (column, amount) in [0.0, 0.3, 0.7, 1.0].into_iter().enumerate() {
            let material = Material {
                name: format!("{preset}_{amount}"),
                roughness: if preset == "muddy" { 0.85 } else { 0.72 },
                layers: damage::wear_layers(preset, amount).unwrap_or_else(|| {
                    let mut layers = Material::preset(preset).unwrap().layers;
                    for layer in &mut layers {
                        layer.opacity *= amount;
                    }
                    layers
                }),
                ..Default::default()
            };
            for (row,mut blob) in [json!({"type":"sphere","radius":0.3,"center":[0,0.33,0]}),json!({"type":"box","size":[0.52,0.52,0.52],"round":0.025,"center":[0,0.3,0],"blend":0})].into_iter().enumerate() {
                blob["color"] = if preset == "muddy" {json!([0.055,0.052,0.047])} else {json!([0.46,0.435,0.355])};
                let clay:Clay=serde_json::from_value(json!({"resolution":72,"blobs":[blob]})).unwrap();
                let mesh=clay.build_mesh_with(std::slice::from_ref(&material),&material.name);
                items.push(film::Item{mesh:Arc::new(mesh),world:Mat4::from_translation(Vec3::new(column as f32*0.8-1.2,0.0,row as f32*0.85)),tint:[1.0;3],surface:material.surface(),material:material.clone()});
            }
        }
        render(
            &format!("sheet_{preset}"),
            items,
            film::Camera {
                eye: Vec3::new(0.5, 2.5, 3.6),
                target: Vec3::new(0.0, 0.25, 0.4),
                up: Vec3::Y,
                fov_deg: 30.0,
                ortho: None,
            },
            1200,
            640,
        );
    }
}
pub(crate) fn scene_items(scene: &Scene) -> Vec<film::Item> {
    let mut lib = crate::mesh::MeshLib::new();
    lib.show_fine = true;
    lib.frame = scene.frame;
    lib.sync(scene);
    assert!(lib.fine_errors().is_empty(), "{:?}", lib.fine_errors());
    let eval = scene.evaluate();
    crate::envkit::scatter::draws(scene, &eval)
        .iter()
        .filter(|d| d.object.shape.is_mesh())
        .filter_map(|d| {
            Some(film::Item {
                mesh: lib.get(d.object)?.clone(),
                world: d.world,
                tint: d.object.color,
                surface: scene.surface_of(d.object),
                material: scene.material_of(d.object),
            })
        })
        .collect()
}
#[test]
#[ignore]
fn paint_mech_films() {
    let source = std::env::var("PAINT_SCENE").expect("PAINT_SCENE");
    let bytes = std::fs::read(source).unwrap();
    std::fs::write(output().join("mech_before.json"), &bytes).unwrap();
    let mut scene: Scene = serde_json::from_slice(&bytes).unwrap();
    scene.frame = Some(0.0);
    let front = film::Camera {
        eye: Vec3::new(3.0, 6.3, 24.0),
        target: Vec3::new(0.0, 3.4, 0.0),
        up: Vec3::Y,
        fov_deg: 25.0,
        ortho: None,
    };
    let detail = film::Camera {
        eye: Vec3::new(4.8, 7.6, 8.0),
        target: Vec3::new(1.2, 6.1, 0.4),
        up: Vec3::Y,
        fov_deg: 19.5,
        ortho: None,
    };
    for stage in ["before", "after"] {
        if stage == "after" {
            let originals: Vec<_> = scene
                .materials
                .iter()
                .filter(|m| !m.name.starts_with("damage_"))
                .map(|m| m.name.clone())
                .collect();
            let restore = |name: &str| {
                originals
                    .iter()
                    .filter(|n| name.ends_with(n.as_str()))
                    .max_by_key(|n| n.len())
                    .cloned()
                    .unwrap_or_else(|| name.into())
            };
            for o in &mut scene.objects {
                if let Some(name) = &mut o.material {
                    *name = restore(name);
                }
                o.bake = None;
                if let Some(c) = &mut o.clay {
                    for b in &mut c.blobs {
                        b.material = restore(&b.material);
                    }
                    c.decals.retain(|d| d.wear_preset.is_empty());
                }
            }
            scene.materials.retain(|m| !m.name.starts_with("damage_"));
            if let Some(m) = scene.materials.iter_mut().find(|m| m.name == "tan_paint") {
                *m = Material {
                    name: "tan_paint".into(),
                    roughness: 0.78,
                    ..Default::default()
                };
                let mut paint = damage::wear_layers("military_paint", 1.0).unwrap();
                paint[0].tint = Some([0.82, 0.94, 1.14]);
                m.layers = paint;
            }
            let ids: Vec<_> = scene
                .objects
                .iter()
                .filter(|o| {
                    o.shape == Shape::Clay
                        && !o.name.starts_with("Soldier")
                        && o.name != "Helmet"
                        && o.name != "Rifle"
                })
                .map(|o| o.id)
                .collect();
            for id in ids {
                damage::apply_wear(&mut scene, id, "battle_worn", 0.7).unwrap();
                damage::apply_wear(&mut scene, id, "desert_dusty", 0.7).unwrap();
                if scene.get(id).unwrap().name.starts_with("Track") {
                    damage::apply_wear(&mut scene, id, "muddy", 0.65).unwrap();
                }
            }
            std::fs::write(
                output().join("mech_after.json"),
                serde_json::to_vec(&scene).unwrap(),
            )
            .unwrap();
        }
        if stage == "before" && std::env::var("PAINT_AFTER_ONLY").is_ok() {
            continue;
        }
        let items = scene_items(&scene);
        for (view, camera) in [("front", front), ("detail", detail)] {
            let items = items
                .iter()
                .map(|i| film::Item {
                    mesh: i.mesh.clone(),
                    world: i.world,
                    tint: i.tint,
                    surface: i.surface,
                    material: i.material.clone(),
                })
                .collect();
            let (w, h) = if view == "front" {
                (990, 1100)
            } else {
                (1200, 800)
            };
            render(&format!("mech_{stage}_{view}"), items, camera, w, h);
        }
    }
}

#[test]
fn paint_microstructure_bakes_and_exports() {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    scene.get_mut(id).unwrap().clay=Some(serde_json::from_value(json!({"resolution":48,"blobs":[{"type":"box","center":[0,0,0],"size":[0.25,0.25,0.08],"round":0.008,"blend":0}]})).unwrap());
    damage::apply_wear(&mut scene, id, "military_paint", 1.0).unwrap();
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    let settings = crate::bake::Settings {
        triangles: 800,
        texture: 256,
        ..Default::default()
    };
    scene.get_mut(id).unwrap().bake = Some(settings);
    meshes.bake(&scene, id, settings).unwrap();
    let textures = meshes.baked(id).unwrap().0.textures.as_ref().unwrap();
    assert!(
        textures
            .normal
            .chunks_exact(4)
            .filter(|c| c[0].abs_diff(128) > 2 || c[1].abs_diff(128) > 2)
            .count()
            > 100,
        "paint grain in normals"
    );
    let range = |channel: usize| {
        let values: Vec<_> = textures.orm.chunks_exact(4).map(|c| c[channel]).collect();
        *values.iter().max().unwrap() - *values.iter().min().unwrap()
    };
    assert!(range(1) > 8, "roughness variation survives bake");
    let exported = crate::gltf::export(
        &scene,
        &meshes,
        &crate::gltf::Options {
            roots: vec![],
            animations: false,
            fps: 24.0,
            file_stem: String::new(),
            lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new(),
        },
    )
    .unwrap();
    let loaded = crate::glb_read::from_bytes(&exported.bytes, None, None).unwrap();
    let decoded = loaded.items[0].mesh.textures.as_ref().unwrap();
    assert_eq!(decoded.color, textures.color);
    assert_eq!(decoded.orm, textures.orm);
    assert!(
        decoded
            .normal
            .chunks_exact(4)
            .zip(textures.normal.chunks_exact(4))
            .all(|(a, b)| (0..3).all(|i| a[i].abs_diff(b[i]) <= 1))
    );
}

#[test]
#[ignore]
fn paint_comparison_sheets() {
    for view in ["front", "detail"] {
        let before = image::open(output().join(format!("mech_before_{view}.png")))
            .unwrap()
            .to_rgb8();
        let after = image::open(output().join(format!("mech_after_{view}.png")))
            .unwrap()
            .to_rgb8();
        assert_eq!(before.dimensions(), after.dimensions());
        let (w, h) = before.dimensions();
        let mut pair = image::RgbImage::new(w * 2, h);
        image::imageops::replace(&mut pair, &before, 0, 0);
        image::imageops::replace(&mut pair, &after, w as i64, 0);
        pair.save(output().join(format!("comparison_{view}.png")))
            .unwrap();
    }
}

// Same world-offset blob construction and 150-cell resolution as MissilePod.
#[test]
fn sharp_mech_box_chip_coverage() {
    let clay: Clay = serde_json::from_value(json!({"sharp":true,"resolution":150,
        "blobs":[{"type":"box","center":[1.78,6.78,-0.05],
        "size":[1.2,0.82,1.2],"round":0.04,"blend":0}]}))
    .unwrap();
    let material = Material {
        layers: damage::wear_layers("battle_worn", 1.0).unwrap(),
        ..Default::default()
    };
    let mut mesh = clay.build_mesh_with(std::slice::from_ref(&material), "clay");
    // Sub-cell QEF position noise must not turn flat paint into chips.
    for (i, v) in mesh.vertices.iter_mut().enumerate() {
        v.pos = (Vec3::from(v.pos) + Vec3::from(v.normal) * (i as f32 * 1.618).sin() * 0.003)
            .to_array();
    }
    let field = crate::clay::Field::new(&clay, clay.cell_size().unwrap());
    let wear = crate::weather::clay_wear_maps(&mesh, &field, clay.wear_width());
    for (v, w) in mesh.vertices.iter_mut().zip(wear) {
        v.wear[0] = w[0];
        v.wear[1] = w[1];
    }
    let mut center = (0.0, 0usize);
    let mut edge = (0.0, 0usize);
    for v in &mesh.vertices {
        let p = Vec3::from(v.pos);
        let q = (p - Vec3::new(1.78, 6.78, -0.05)).abs();
        let at = crate::smart::At {
            p,
            n: Vec3::from(v.normal),
            edge: v.wear[0],
            cavity: v.wear[1],
            occlusion: 0.0,
            painted: [0.0; 4],
            footprint: 0.0,
        };
        let chip = material
            .layers
            .iter()
            .filter(|l| l.name == "edge chips")
            .map(|l| crate::smart::mask(&material, l, &at))
            .fold(0.0, f32::max);
        if q.z > 0.59 && q.x < 0.4 && q.y < 0.22 {
            center.0 += chip;
            center.1 += 1;
        }
        if q.z > 0.55 && q.x > 0.55 {
            edge.0 += chip;
            edge.1 += 1;
        }
    }
    let c = center.0 / center.1 as f32;
    let e = edge.0 / edge.1 as f32;
    eprintln!("mech box chip coverage: face={c:.5}, edge={e:.5}");
    assert!(center.1 > 100 && edge.1 > 100);
    assert!(c < 0.005, "flat face chip coverage {c}");
    assert!(e > c + 0.03, "edge coverage {e}, center {c}");
}

#[test]
#[ignore]
fn paint_missile_chip_probe() {
    let source = std::env::var("PAINT_SCENE").expect("PAINT_SCENE");
    let scene: Scene = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let mut clay = scene
        .objects
        .iter()
        .find(|o| o.name == "MissilePod")
        .unwrap()
        .clay
        .clone()
        .unwrap();
    clay.decals.clear();
    clay.paint = None;
    for b in &mut clay.blobs {
        b.material = "probe".into();
        b.color = [0.0; 3];
    }
    let mut layers = damage::wear_layers("battle_worn", 1.0).unwrap();
    layers.retain(|l| l.name == "edge chips");
    for l in &mut layers {
        l.color = Some([1.0; 3]);
        l.metallic = Some(0.0);
        l.emission = Some(1.0);
    }
    let material = Material {
        name: "probe".into(),
        roughness: 1.0,
        layers,
        ..Default::default()
    };
    let mesh = clay.build_mesh_with(std::slice::from_ref(&material), "probe");
    let label = std::env::var("PAINT_PROBE_LABEL").unwrap_or("after".into());
    render(
        &format!("missile_chip_{label}"),
        vec![film::Item {
            mesh: Arc::new(mesh),
            world: Mat4::IDENTITY,
            tint: [1.0; 3],
            surface: material.surface(),
            material,
        }],
        film::Camera {
            eye: Vec3::new(3.6, 8.0, 3.0),
            target: Vec3::new(1.78, 6.78, -0.05),
            up: Vec3::Y,
            fov_deg: 32.0,
            ortho: None,
        },
        800,
        800,
    );
}

#[test]
fn missile_pod_flat_face_has_no_chips() {
    let mut clay: Clay =
        serde_json::from_str(include_str!("../paint_check/missile_pod_fixture.json")).unwrap();
    for b in &mut clay.blobs {
        b.material = "pod".into();
    }
    let material = Material {
        name: "pod".into(),
        layers: damage::wear_layers("battle_worn", 1.0).unwrap(),
        ..Default::default()
    };
    let mesh = clay.build_mesh_with(std::slice::from_ref(&material), "pod");
    let mut center = (0.0, 0usize);
    for v in &mesh.vertices {
        let p = Vec3::from(v.pos);
        // Broad side of the pod, away from perimeter and attached rails.
        if (p.x - 2.43).abs() < 0.002 && (p.y - 6.78).abs() < 0.2 && (p.z + 0.05).abs() < 0.3 {
            let at = crate::smart::At {
                p,
                n: Vec3::from(v.normal),
                edge: v.wear[0],
                cavity: v.wear[1],
                occlusion: 0.0,
                painted: [0.0; 4],
                footprint: 0.0,
            };
            center.0 += material
                .layers
                .iter()
                .filter(|l| l.name == "edge chips")
                .map(|l| crate::smart::mask(&material, l, &at))
                .fold(0.0, f32::max);
            center.1 += 1;
        }
    }
    let coverage = center.0 / center.1 as f32;
    eprintln!(
        "actual pod side: {} vertices, chip coverage {coverage}",
        center.1
    );
    assert!(center.1 > 100);
    assert!(coverage < 0.005, "pod face chip coverage {coverage}");
}

#[test]
fn paint_detail_survives_mech_closeup_footprint() {
    let material = Material {
        layers: damage::wear_layers("military_paint", 1.0)
            .unwrap()
            .into_iter()
            .chain(damage::wear_layers("battle_worn", 0.7).unwrap())
            .collect(),
        ..Default::default()
    };
    let mut slopes = 0.0;
    let mut rough = (1.0f32, 0.0f32);
    for y in 0..32 {
        for x in 0..32 {
            let at = crate::smart::At {
                p: Vec3::new(1.4 + x as f32 * 0.008, 6.6 + y as f32 * 0.008, 0.55),
                n: Vec3::Z,
                edge: 0.0,
                cavity: 0.0,
                occlusion: 0.0,
                painted: [0.0; 4],
                footprint: 0.008,
            };
            let look = crate::smart::layers(
                &material,
                &at,
                crate::smart::Look::of(&material, Vec3::splat(0.5), 0.78, Vec3::Z),
            );
            slopes += look.n.truncate().length();
            rough.0 = rough.0.min(look.rough);
            rough.1 = rough.1.max(look.rough);
        }
    }
    eprintln!("closeup slope {}, roughness {:?}", slopes / 1024.0, rough);
    assert!(
        slopes / 1024.0 < 0.025,
        "paint normals are too coarse at film footprint"
    );
    assert!(rough.1 - rough.0 > 0.08, "uniform film roughness");
}
