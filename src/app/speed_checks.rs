//! Speed task regressions and a reproducible external mech acceptance run.
use crate::{
    material::Material,
    mesh::MeshLib,
    scene::{Scene, Shape},
};
use serde_json::{Value, json};
use std::sync::Arc;

fn clay(scene: &mut Scene, name: &str, material: &str) -> u64 {
    let id = scene.add(Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.name = name.into();
    o.material = Some(material.into());
    o.clay=Some(serde_json::from_value(json!({"resolution":20,"blobs":[{"type":"box","center":[0,0,0],"size":[0.5,0.6,0.4],"blend":0,"color":[0.6,0.4,0.2]}]})).unwrap());
    id
}
#[test]
fn material_edits_preserve_geometry_and_only_refresh_users() {
    let mut s = Scene::default();
    s.materials.push(Material {
        name: "finish".into(),
        ..Default::default()
    });
    let a = clay(&mut s, "A", "finish");
    let b = clay(&mut s, "B", "clay");
    let mut meshes = MeshLib::new();
    meshes.sync(&s);
    let old = meshes.whole(a).unwrap().clone();
    let other = meshes.whole(b).unwrap().clone();
    let count = meshes.geometry_builds;
    s.materials[0].roughness = 0.2;
    s.materials[0].layers = serde_json::from_value(
        json!([{"color":[0.1,0.8,0.3],"mask":[{"source":"constant","value":1}]}]),
    )
    .unwrap();
    meshes.sync(&s);
    assert_eq!(meshes.geometry_builds, count);
    assert_eq!(meshes.shading_updates, 1);
    assert!(Arc::ptr_eq(&other, meshes.whole(b).unwrap()));
    let updated = meshes.whole(a).unwrap();
    assert_eq!(updated.indices, old.indices);
    assert!(
        updated
            .vertices
            .iter()
            .zip(&old.vertices)
            .all(|(a, b)| a.pos == b.pos && a.normal == b.normal)
    );
    assert_ne!(updated.vertices[0].color, old.vertices[0].color);
    s.get_mut(a).unwrap().clay.as_mut().unwrap().blobs[0].material = "metal".into();
    meshes.sync(&s);
    assert_eq!(meshes.geometry_builds, count);
    assert!(
        meshes
            .whole(a)
            .unwrap()
            .materials
            .iter()
            .any(|m| m.metallic > 0.5)
    );
}
#[test]
fn material_replace_rename_and_unused_include_blob_and_part_users() {
    let mut s = Scene::default();
    for name in ["damage_a", "damage_b", "tan_paint", "unused"] {
        s.materials.push(Material {
            name: name.into(),
            ..Default::default()
        });
    }
    let a = clay(&mut s, "A", "damage_a");
    s.get_mut(a).unwrap().clay.as_mut().unwrap().blobs[0].material = "damage_b".into();
    let p = s.add(Shape::Part);
    let mut part = crate::kitbash::Part::new(crate::kitbash::Kind::Bolt);
    part.paint = Some("damage_a".into());
    s.get_mut(p).unwrap().part = Some(part);
    let r = crate::material_management::manage(
        &mut s,
        &json!({"replace":"damage_*","with":"tan_paint"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(r["references_changed"], 3);
    assert_eq!(crate::material_management::users(&s, "tan_paint").len(), 3);
    assert!(!s.materials.iter().any(|m| m.name.starts_with("damage_")));
    crate::material_management::manage(&mut s, &json!({"name":"tan_paint","rename":"sand"}))
        .unwrap();
    assert_eq!(s.get(a).unwrap().material.as_deref(), Some("sand"));
    assert_eq!(crate::material_management::users(&s, "sand").len(), 3);
    crate::material_management::manage(&mut s, &json!({"delete_unused":true})).unwrap();
    assert_eq!(s.materials.len(), 1);
    assert_eq!(s.materials[0].name, "sand");
    let snapshot = s.clone();
    assert!(
        crate::material_management::manage(&mut s, &json!({"replace":"*","with":"missing"}))
            .is_err()
    );
    assert_eq!(s, snapshot);
}
#[test]
fn replies_are_bounded_valid_json_and_details_remain_available() {
    let mut scene = Scene::default();
    for i in 0..300 {
        let id = clay(&mut scene, &format!("Part{i}"), "clay");
        if i > 0 {
            scene.get_mut(id).unwrap().parent = Some(1);
        }
    }
    let summary = crate::replies::scene(&scene, &json!({}));
    assert_eq!(summary["objects"].as_array().unwrap().len(), 1);
    let details = crate::replies::scene(&scene, &json!({"verbose":true}));
    assert_eq!(details["objects"].as_array().unwrap().len(), 300);
    let mut content = super::tools::text(details);
    crate::replies::bound(&mut content, false);
    assert!(content[0]["text"].as_str().unwrap().len() < 8000);
    let _: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    let page = crate::replies::scene(
        &scene,
        &json!({"all":true,"filter":"Part*","offset":25,"limit":10}),
    );
    assert_eq!(page["objects"].as_array().unwrap().len(), 10);
    assert_eq!(page["next_offset"], 35);
}

#[test]
fn linked_mirrors_inherit_detail_once() {
    let mut s = Scene::default();
    let id = clay(&mut s, "Plate.L", "metal");
    s.get_mut(id).unwrap().translation = glam::Vec3::new(0.8, 0.5, 0.0);
    let options = crate::pose::MirrorOptions {
        normal: glam::Vec3::X,
        d: 0.0,
        linked: true,
        name: None,
        kitbash: true,
    };
    let other = crate::pose::mirror(&mut s, id, &options).unwrap().root;
    let mut meshes = MeshLib::new();
    meshes.sync(&s);
    let r=crate::kitbash::tool::detail_pass(&mut s,&mut meshes,&json!({"pass":"bolt","targets":[id,other],"mirror":true,"group":"test_detail","spacing":0.12})).unwrap();
    assert_eq!(r["mirrored"], 0);
    assert_eq!(r["linked_mirror_targets_skipped"], json!([other]));
    crate::pose::sync(&mut s);
    let copies: Vec<_> = s
        .objects
        .iter()
        .filter(|o| {
            o.link.as_ref().is_some_and(|l| {
                s.get(l.source)
                    .is_some_and(|o| o.kitbash.as_deref() == Some("test_detail"))
            })
        })
        .collect();
    assert!(!copies.is_empty());
    let before = s.objects.len();
    crate::pose::sync(&mut s);
    assert_eq!(s.objects.len(), before);
}
#[test]
fn atlas_bake_glb_roundtrip_keeps_uvs_and_part_identity() {
    let mut s = Scene::default();
    let mut ids = Vec::new();
    for i in 0..22 {
        let id = clay(&mut s, &format!("Part{i}"), "clay");
        let o = s.get_mut(id).unwrap();
        o.translation.x = i as f32;
        o.clay.as_mut().unwrap().blobs[0].color = [i as f32 / 22.0, 0.25, 0.8];
        o.bake = Some(crate::bake::Settings {
            triangles: 500,
            texture: 512,
            ..Default::default()
        });
        ids.push(id);
    }
    let mut meshes = MeshLib::new();
    meshes.sync(&s);
    for id in &ids {
        meshes
            .bake(&s, *id, s.get(*id).unwrap().bake.unwrap())
            .unwrap();
    }
    let opts = crate::gltf::Options {
        roots: vec![],
        animations: false,
        fps: 24.0,
        file_stem: String::new(),
        lods: false,
        atlas: true,
        texel_density: Some(128.0),
    };
    let export = crate::gltf::export(&s, &meshes, &opts).unwrap();
    let loaded = crate::glb_read::from_bytes(&export.bytes, None, None).unwrap();
    assert_eq!(loaded.items.len(), 22);
    assert_eq!(loaded.summary["images"].as_array().unwrap().len(), 3);
    for ((id, item), i) in ids.iter().zip(&loaded.items).zip(0..) {
        let original = meshes.baked(*id).unwrap().0;
        assert_eq!(item.mesh.indices.len(), original.indices.len());
        for (v, old) in item.mesh.vertices.iter().zip(&original.vertices) {
            assert!(
                (glam::Vec3::from(v.pos)
                    - s.world_matrix(*id)
                        .transform_point3(glam::Vec3::from(old.pos)))
                .length()
                    < 1e-5
            );
            assert!(v.uv.iter().all(|u| *u > 0.0 && *u < 1.0));
            let texture = item.mesh.textures.as_ref().unwrap();
            let x = (v.uv[0] * texture.size as f32)
                .floor()
                .min(texture.size as f32 - 1.0) as usize;
            let y = (v.uv[1] * texture.size as f32)
                .floor()
                .min(texture.size as f32 - 1.0) as usize;
            let pixel = &texture.color[(y * texture.size as usize + x) * 4..][..3];
            assert!(
                (pixel[0] as f32 / 255.0 - i as f32 / 22.0).abs() < 0.025,
                "part {i}: {pixel:?}"
            );
        }
    }
    let original = crate::gltf::export(
        &s,
        &meshes,
        &crate::gltf::Options {
            atlas: false,
            ..opts
        },
    )
    .unwrap();
    assert_eq!(original.summary["textures"], 66);
    assert!(
        crate::glb_read::from_bytes(&original.bytes, None, None)
            .err()
            .unwrap()
            .contains("64 images")
    );
}

/// Run with `cargo test --release speed_mech_acceptance -- --ignored --nocapture`.
/// Uses only the copied speed_check/mech.json and writes reproducible local artifacts.
#[test]
#[ignore]
fn speed_mech_acceptance() {
    use glam::Vec3;
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("speed_check");
    std::fs::create_dir_all(&out).unwrap();
    let mut scene = super::tools::load_scene(&out.join("mech.json")).unwrap();
    let mut report = json!({"scene_objects":scene.objects.len(),"source":"copied mech3/mech.json"});
    let save = |r: &Value| {
        std::fs::write(
            out.join("measurements.json"),
            serde_json::to_vec_pretty(r).unwrap(),
        )
        .unwrap()
    };
    let legacy = json!({"objects":scene.objects.iter().map(|o|super::tools::object_json_in(&scene,o.id)).collect::<Vec<_>>()});
    report["legacy_scene_reply_bytes"] =
        json!(serde_json::to_string_pretty(&legacy).unwrap().len());
    for (name, args) in [("open_scene", json!({})), ("get_scene", json!({}))] {
        let mut reply = super::tools::text(crate::replies::scene(&scene, &args));
        crate::replies::bound(&mut reply, false);
        report[format!("{name}_reply_bytes")] = json!(reply[0]["text"].as_str().unwrap().len());
    }
    let mut reply = super::tools::text(crate::material_management::listing(
        &scene,
        &json!({"list":true}),
    ));
    crate::replies::bound(&mut reply, false);
    report["material_list_reply_bytes"] = json!(reply[0]["text"].as_str().unwrap().len());
    save(&report);
    let mut meshes = MeshLib::new();
    let start = Instant::now();
    meshes.sync(&scene);
    report["initial_mesh_seconds"] = json!(start.elapsed().as_secs_f64());
    eprintln!("Initial meshes {:.2}s", start.elapsed().as_secs_f64());
    save(&report);
    let index = scene
        .materials
        .iter()
        .position(|m| m.name == "tan_paint")
        .unwrap();
    scene.materials[index].roughness = 0.73;
    meshes.force_legacy_material_rebuild();
    let start = Instant::now();
    meshes.sync(&scene);
    report["legacy_material_edit_seconds"] = json!(start.elapsed().as_secs_f64());
    eprintln!(
        "Legacy material invalidation {:.2}s",
        start.elapsed().as_secs_f64()
    );
    save(&report);
    let geometry = meshes.geometry_builds;
    let shading = meshes.shading_updates;
    scene.materials[index].roughness = 0.72;
    let start = Instant::now();
    meshes.sync(&scene);
    report["material_edit_seconds"] = json!(start.elapsed().as_secs_f64());
    report["material_geometry_rebuilds"] = json!(meshes.geometry_builds - geometry);
    report["material_shading_updates"] = json!(meshes.shading_updates - shading);
    assert_eq!(geometry, meshes.geometry_builds);
    eprintln!(
        "Shading material edit {:.2}s, {} objects",
        start.elapsed().as_secs_f64(),
        meshes.shading_updates - shading
    );
    save(&report);
    let before_geometry = meshes.geometry_builds;
    let id = scene
        .objects
        .iter()
        .find(|o| o.name == "Hand.L")
        .unwrap()
        .id;
    let material = scene.get(id).unwrap().material.clone();
    scene.get_mut(id).unwrap().material = Some("gunmetal".into());
    let start = Instant::now();
    meshes.sync(&scene);
    report["object_material_assignment_seconds"] = json!(start.elapsed().as_secs_f64());
    assert_eq!(before_geometry, meshes.geometry_builds);
    scene.get_mut(id).unwrap().material = material;
    meshes.sync(&scene);
    let baked: Vec<_> = scene
        .objects
        .iter()
        .filter_map(|o| o.bake.map(|s| (o.id, s)))
        .collect();
    let start = Instant::now();
    for (id, s) in &baked {
        let begin = Instant::now();
        meshes.bake(&scene, *id, *s).unwrap();
        eprintln!(
            "Baked {} {:.2}s",
            scene.get(*id).unwrap().name,
            begin.elapsed().as_secs_f64()
        );
    }
    report["bake_seconds"] = json!(start.elapsed().as_secs_f64());
    report["baked_parts"] = json!(baked.len());
    save(&report);
    let frame = scene.objects.iter().find(|o| o.name == "Frame").unwrap().id;
    let mut options = crate::gltf::Options {
        roots: vec![frame],
        animations: true,
        fps: 24.0,
        file_stem: String::new(),
        lods: false,
        atlas: false,
        texel_density: None,
    };
    let mut images = Vec::new();
    let camera = crate::film::Camera {
        eye: Vec3::new(10.0, 6.0, 15.0),
        target: Vec3::new(0.0, 2.4, 0.0),
        up: Vec3::Y,
        fov_deg: 30.0,
        ortho: None,
    };
    for packed in [false, true] {
        let label = if packed { "atlas" } else { "per_part" };
        options.atlas = packed;
        options.texel_density = packed.then_some(512.0);
        let start = Instant::now();
        let export = crate::gltf::export(&scene, &meshes, &options).unwrap();
        std::fs::write(out.join(format!("mech_{label}.glb")), &export.bytes).unwrap();
        report[format!("{label}_export_seconds")] = json!(start.elapsed().as_secs_f64());
        report[format!("{label}_export_bytes")] = json!(export.bytes.len());
        report[format!("{label}_export_summary")] = export.summary.clone();
        save(&report);
        let loaded = crate::glb_read::from_bytes(&export.bytes, Some("fire"), Some(3.0)).unwrap();
        report[format!("{label}_check_export")] = loaded.summary.clone();
        save(&report);
        let imageset = crate::film::render_views(
            &crate::film::Meshes::new(loaded.items),
            &[],
            &[camera],
            crate::film::Settings {
                width: 768,
                height: 768,
                samples: 64,
                bounces: 4,
                time_limit: Duration::from_secs(180),
                floor: true,
                exposure: 1.0,
                denoise: true,
            },
            &Default::default(),
        );
        let image = &imageset[0];
        assert!(image.samples > 0);
        report[format!("{label}_render_seconds")] = json!(image.seconds);
        report[format!("{label}_render_samples")] = json!(image.samples);
        let image = image::RgbImage::from_raw(
            image.width as u32,
            image.height as u32,
            image.pixels.iter().flatten().copied().collect(),
        )
        .unwrap();
        image.save(out.join(format!("mech_{label}.png"))).unwrap();
        images.push(image);
        save(&report);
    }
    let mut sheet = image::RgbImage::new(1536, 768);
    for (i, img) in images.iter().enumerate() {
        image::imageops::replace(&mut sheet, img, i as i64 * 768, 0);
    }
    sheet.save(out.join("atlas_comparison.png")).unwrap();
    let mae = images[0]
        .as_raw()
        .iter()
        .zip(images[1].as_raw())
        .map(|(a, b)| (*a as f32 - *b as f32).abs())
        .sum::<f32>()
        / images[0].as_raw().len() as f32;
    report["render_mean_absolute_channel_difference_255"] = json!(mae);
    save(&report);
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
}

#[test]
fn compact_material_pages_never_skip_unreturned_names() {
    let mut scene = Scene::default();
    for i in 0..30 {
        let name = format!("test_{i:02}_{}", "long".repeat(8));
        scene.materials.push(Material {
            name: name.clone(),
            ..Default::default()
        });
        for j in 0..10 {
            clay(
                &mut scene,
                &format!("object_{j}_{}", "name".repeat(12)),
                &name,
            );
        }
    }
    let mut offset = 0;
    let mut names = std::collections::HashSet::new();
    loop {
        let page = crate::material_management::listing(
            &scene,
            &json!({"filter":"test_*","offset":offset}),
        );
        assert!(serde_json::to_string_pretty(&page).unwrap().len() < 7800);
        for material in page["materials"].as_array().unwrap() {
            assert!(names.insert(material["name"].as_str().unwrap().to_string()));
        }
        if let Some(next) = page["next_offset"].as_u64() {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(names.len(), 30);
}
