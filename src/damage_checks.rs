//! Reproducible film evidence using the same recipes as the MCP damage tool.
use crate::{
    clay::Clay,
    damage, film,
    material::Material,
    scene::{Scene, Shape},
    smart::Decal,
};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::{sync::Arc, time::Duration};
fn output() -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("damage_check");
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn settings(w: usize, h: usize) -> film::Settings {
    film::Settings {
        width: w,
        height: h,
        samples: std::env::var("SAMPLES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64),
        bounces: 4,
        time_limit: Duration::from_secs(600),
        floor: true,
        exposure: 1.0,
        denoise: true,
    }
}
fn render(
    name: &str,
    items: Vec<film::Item>,
    cam: film::Camera,
    w: usize,
    h: usize,
) -> Vec<[u8; 3]> {
    let meshes = film::Meshes::new(items);
    let img = film::render_views_with_look(
        &meshes,
        &[],
        &[cam],
        settings(w, h),
        Arc::new(if name.ends_with("daylight") {
            film::Look::parse(&json!({"environment":"overcast", "lights":[
                {"type":"sun", "direction":[0.6,0.8,0.7], "color":[1.0,0.92,0.81], "intensity":2.6, "size":1.4}
            ]})).unwrap()
        } else { film::Look::default() }),
        &Default::default(),
    )
    .pop()
    .unwrap();
    image::save_buffer(
        output().join(format!("{name}.png")),
        &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        w as u32,
        h as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
    eprintln!("{name}: {} samples in {:.1}s", img.samples, img.seconds);
    assert!(img.samples > 0);
    img.pixels
}
// Columns: original / round two / round three. Keep native resolution so
// exposed corners and the material identities can be inspected at full size.
fn mech_comparison(view: &str) {
    let names = ["before", "round2", "after"];
    let frames: Vec<_> = names.iter().map(|name|
        image::open(output().join(format!("mech_{name}_{view}.png"))).unwrap().to_rgb8()
    ).collect();
    let (w, h) = frames[0].dimensions();
    let mut sheet = image::RgbImage::new(w * 3, h);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.dimensions(), (w, h));
        image::imageops::replace(&mut sheet, frame, i as i64 * w as i64, 0);
    }
    sheet.save(output().join(format!("mech_comparison_{view}.png"))).unwrap();
}
fn copy_items(items: &[film::Item]) -> Vec<film::Item> {
    items
        .iter()
        .map(|i| film::Item {
            mesh: i.mesh.clone(),
            world: i.world,
            tint: i.tint,
            surface: i.surface,
            material: i.material.clone(),
        })
        .collect()
}
fn plate() -> Clay {
    serde_json::from_value(json!({"resolution":144,"blobs":[
    {"type":"box","name":"plate","center":[0,0.25,0],"size":[0.32,0.42,0.04],"round":0.006,"blend":0,"color":[0.28,0.33,0.19]},
    {"type":"sphere","name":"bolt","center":[-0.12,0.41,0.022],"radius":0.012,"blend":0,"color":[0.4,0.4,0.35]}
]})).unwrap()
}
#[test]
#[ignore]
fn damage_panel_films() {
    let names = [
        "bullet_hit",
        "dent",
        "scorch",
        "blast",
        "gouge",
        "scrape",
        "rust_streak",
        "oil_leak",
        "water_stain",
        "bend",
        "tear",
        "battle_worn",
    ];
    let mut sheet = vec![[25u8; 3]; 4 * 320 * 3 * 320];
    for (i, name) in names.iter().enumerate() {
        let mut c = plate();
        let base = c.build_mesh();
        let mut args =
            json!({"kind":name,"point":[0,0.26,0.02],"normal":[0,0,1],"size":0.22,"seed":31});
        if *name == "bullet_hit" {
            args["size"] = json!(0.1);
            args["depth"] = json!(0.008);
        }
        if *name == "dent" {
            args["depth"] = json!(0.012);
        }
        if *name == "blast" {
            args["depth"] = json!(0.010);
        }
        match *name {
            "gouge" | "scrape" => {
                args = json!({"kind":name,"points":[[-0.06,0.15,0.02],[0.035,0.30,0.02],[0.07,0.37,0.02]],"normal":[0,0,1],"size":0.038,"seed":31});
            }
            "rust_streak" | "oil_leak" | "water_stain" => {
                args["point"] = json!([-0.12, 0.40, 0.035]);
                args["size"] = json!(0.07);
                args["length"] = json!(0.29);
            }
            "bend" | "tear" => {
                args["size"] = json!(0.80);
                args["hinge"] = json!([[0, 0.04, 0.02], [0, 0.46, 0.02]]);
                args["angle"] = json!(40);
                if *name == "tear" {
                    args["depth"] = json!(0.024);
                }
            }
            _ => {}
        }
        let mut m = Material {
            name: "paint".into(),
            roughness: 0.70,
            ..Material::default()
        };
        if *name == "battle_worn" {
            m.layers = damage::wear_layers(name, 1.0).unwrap();
        } else {
            let req = serde_json::from_value(args).unwrap();
            c.decals = damage::stamps(&base, Mat4::IDENTITY, true, &req).unwrap();
        }
        let mesh = Arc::new(c.build_mesh_with(std::slice::from_ref(&m), "paint"));
        let items = vec![film::Item {
            mesh,
            world: Mat4::IDENTITY,
            tint: [1.0; 3],
            surface: m.surface(),
            material: m,
        }];
        let cam = film::Camera {
            eye: Vec3::new(0.24, 0.40, 0.93),
            target: Vec3::new(0.0, 0.25, 0.0),
            up: Vec3::Y,
            fov_deg: 32.0,
            ortho: None,
        };
        let px = render(&format!("panel_{name}"), items, cam, 320, 320);
        for y in 0..320 {
            for x in 0..320 {
                sheet[(i / 4 * 320 + y) * 1280 + i % 4 * 320 + x] = px[y * 320 + x];
            }
        }
    }
    image::save_buffer(
        output().join("panel_sheet.png"),
        &sheet.iter().flatten().copied().collect::<Vec<_>>(),
        1280,
        960,
        image::ColorType::Rgb8,
    )
    .unwrap();
}
fn scene_items(scene: &Scene) -> Vec<film::Item> {
    let mut items = Vec::new();
    for o in &scene.objects {
        if !scene.is_visible(o.id)
            || matches!(o.shape, Shape::Terrain | Shape::Scatter | Shape::Kit)
        {
            continue;
        }
        let world = scene.world_matrix(o.id);
        let library = damage::library(scene, o);
        let m = damage::world_material(scene.material_of(o), world);
        let mesh = match o.shape {
            Shape::Clay => {
                let c = o.clay.as_ref().unwrap();
                eprintln!("building {} at {}", o.name, c.resolution);
                c.build_mesh_with(&library, o.material.as_deref().unwrap_or(""))
            }
            Shape::Eye => o.eye.as_ref().unwrap().build_mesh(),
            _ => continue,
        };
        eprintln!("{}: {} triangles", o.name, mesh.indices.len() / 3);
        items.push(film::Item {
            mesh: Arc::new(mesh),
            world,
            tint: o.color,
            surface: m.surface(),
            material: m,
        });
    }
    items
}
fn add_stamps(scene: &mut Scene, id: u64, args: serde_json::Value) {
    let o = scene.get(id).unwrap();
    let mesh = o.clay.as_ref().unwrap().build_mesh();
    let world = scene.world_matrix(id);
    let mut all = Vec::<Decal>::new();
    for raw in args.as_array().unwrap() {
        let req = serde_json::from_value(raw.clone()).unwrap();
        all.extend(damage::stamps(&mesh, world, false, &req).unwrap());
    }
    let c = scene.get_mut(id).unwrap().clay.as_mut().unwrap();
    c.decals.extend(all);
    assert!(c.decals.len() <= 64);
}
#[test]
#[ignore]
fn damage_mech_films() {
    let source=std::env::var("DAMAGE_SCENE").expect("set DAMAGE_SCENE to an absolute mech scene .json path");
    let bytes = std::fs::read(&source).unwrap();
    std::fs::write(output().join("mech_before.json"), &bytes).unwrap();
    let mut scene: Scene = serde_json::from_slice(&bytes).unwrap();
    let front = film::Camera {
        eye: Vec3::new(0.7, 5.25, 22.5),
        target: Vec3::new(0.7, 3.8, 0.0),
        up: Vec3::Y,
        fov_deg: 22.0,
        ortho: None,
    };
    let close = film::Camera {
        eye: Vec3::new(1.1, 6.5, 5.3),
        target: Vec3::new(0.0, 5.75, 1.0),
        up: Vec3::Y,
        fov_deg: 31.0,
        ortho: None,
    };
    let quarter = film::Camera {
        eye: Vec3::new(13.0, 8.0, 18.0),
        target: Vec3::new(0.5, 3.7, 0.0),
        up: Vec3::Y, fov_deg: 26.0, ortho: None,
    };
    if std::env::var("AFTER_ONLY").is_err() {
        let items = scene_items(&scene);
        render("mech_before_front", copy_items(&items), front, 680, 800);
        render("mech_before_damage", copy_items(&items), close, 840, 600);
        render("mech_before_daylight", items, quarter, 800, 800);
    }
    // The scene's authored orange paint and scratched texture predate the
    // damage presets. Use desaturated sand paint with broad tonal variation.
    for m in &mut scene.materials {
        if m.name.starts_with("mech_paint") {
            for l in &mut m.layers {
                if l.name == "paint" {
                    l.tint = Some([0.96, 1.07, 1.18]);
                    l.variation = 0.08;
                    l.variation_scale = 0.85;
                    l.detail = None;
                    l.detail_strength = 0.0;
                    l.detail_color = 0.0;
                }
            }
        }
    }
    // Re-render the saved round-two scene with the same tracer and cameras.
    // Preserve mech_round2.json before the first round-three run.
    if std::env::var("RERENDER_ROUND2").is_ok() {
        let old: Scene = serde_json::from_slice(&std::fs::read(output().join("mech_round2.json")).unwrap()).unwrap();
        let items = scene_items(&old);
        render("mech_round2_front", copy_items(&items), front, 680, 800);
        render("mech_round2_damage", copy_items(&items), close, 840, 600);
        render("mech_round2_daylight", items, quarter, 800, 800);
    }
    // Replace the old workaround splats and whiskers, retaining emblems,
    // stencils and sculpted shape from the user's saved source scene.
    let ids: Vec<u64> = scene
        .objects
        .iter()
        .filter(|o| o.shape == Shape::Clay && !o.name.starts_with("Soldier"))
        .map(|o| o.id)
        .collect();
    for id in ids {
        let o = scene.get_mut(id).unwrap();
        o.bake = None;
        let c = o.clay.as_mut().unwrap();
        c.decals.retain(|d| d.shape == crate::smart::Shape::Emblem);
        // Match the old look's local height dirt with proper world masks.
        damage::apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
        damage::apply_wear(&mut scene, id, "desert_dusty", 1.0).unwrap();
        if scene.get(id).unwrap().name.starts_with("Track")
            || scene.get(id).unwrap().name.starts_with("Shin")
        {
            damage::apply_wear(&mut scene, id, "muddy", 0.85).unwrap();
        }
    }
    add_stamps(
        &mut scene,
        2,
        json!([
            {"kind":"bullet_hit","points":[[-0.35,5.5,1.3],[0.18,5.48,1.3],[0.38,5.25,1.3],[0.52,5.57,1.3],[-0.14,5.2,1.3]],"normal":[0,0,1],"caliber_mm":25,"seed":41},
            {"kind":"blast","point":[-0.72,6.15,1.2],"normal":[0,0.6,0.8],"size":0.60,"depth":0.045,"seed":19},
            {"kind":"dent","point":[0.72,5.35,1.3],"size":0.32,"normal":[0,0,1],"seed":15},
            {"kind":"gouge","points":[[0.57,5.55,1.3],[0.78,5.7,1.3]],"size":0.08,"normal":[0,0,1],"seed":18},
            {"kind":"rust_streak","point":[-0.48,5.91,1.27],"length":0.46,"size":0.075,"seed":14},
            {"kind":"rust_streak","point":[0.35,5.91,1.27],"length":0.38,"size":0.06,"seed":12}
        ]),
    );
    add_stamps(
        &mut scene,
        1,
        json!([{"kind":"oil_leak","point":[0.86,2.97,0.25],"size":0.08,"length":0.45,"seed":11}]),
    );
    add_stamps(
        &mut scene,
        6,
        json!([{"kind":"dent","point":[2.0,5.7,0.65],"size":0.25,"seed":19}]),
    );
    std::fs::write(
        output().join("mech_after.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    let items = scene_items(&scene);
    render("mech_after_front", copy_items(&items), front, 680, 800);
    render("mech_after_damage", copy_items(&items), close, 840, 600);
    render("mech_after_daylight", items, quarter, 800, 800);
    for view in ["front", "damage", "daylight"] {
        mech_comparison(view);
    }
    assert_eq!(
        bytes,
        std::fs::read(source).unwrap(),
        "original scene must remain unchanged"
    );
}
