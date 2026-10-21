//! Native close-up acceptance: no external renderer, assets or dependencies.
use crate::{
    clay::Clay,
    damage, film,
    material::Material,
    scene::Scene,
    smart::{At, Look},
};
use glam::{Mat4, Vec2, Vec3};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
fn output() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("closeup_check");
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn plate() -> Clay {
    serde_json::from_value(json!({"resolution":128,"sharp":true,"blobs":[
        {"type":"box","center":[0,0,0],"size":[0.32,0.40,0.045],"round":0.008,"blend":0,"color":[0.46,0.435,0.355]}
    ]})).unwrap()
}
fn render(
    name: &str,
    items: Vec<film::Item>,
    camera: film::Camera,
    w: usize,
    h: usize,
    aperture: Option<f32>,
) {
    let samples = std::env::var("CLOSEUP_SAMPLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    let focal_mm = 12.0 / (camera.fov_deg.to_radians() * 0.5).tan();
    let mut args = json!({"environment":{"preset":"overcast","intensity":0.65},"lights":[{"type":"sun","direction":[-0.65,0.5,0.45],"color":[1.0,0.93,0.82],"intensity":3.0,"size":3}],"camera":{"position":camera.eye.to_array(),"target":camera.target.to_array(),"focal_length_mm":focal_mm,"fov":camera.fov_deg,"sensor_size_mm":[36,24],"aperture":aperture,"dof":{"focus_point":camera.target.to_array()}},"tonemap":"agx","grain":0.002,"sharpen":0.08});
    args["size"] = json!(w);
    args["aspect"] = json!(w as f32 / h as f32);
    args["samples"] = json!(samples);
    args["floor"] = json!(false);
    let img = film::render_views_with_look(
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
        &img.pixels.iter().flatten().copied().collect::<Vec<_>>(),
        w as u32,
        h as u32,
        image::ColorType::Rgb8,
    )
    .unwrap();
    std::fs::write(output().join(format!("{name}.json")),serde_json::to_vec_pretty(&json!({"render_film":args,"samples":img.samples,"seconds":img.seconds,"width":w,"height":h,"fov":camera.fov_deg})).unwrap()).unwrap();
    eprintln!("{name}: {} spp {:.1}s", img.samples, img.seconds);
}
fn item(c: &Clay, m: Material, world: Mat4) -> film::Item {
    film::Item {
        mesh: Arc::new(c.build_mesh_with(std::slice::from_ref(&m), &m.name)),
        world,
        tint: [1.0; 3],
        surface: m.surface(),
        material: m,
    }
}
#[test]
fn closeup_stamp_structure_and_determinism() {
    for code in 5..=15 {
        for seed in [0, 31, 82] {
            for y in -24..=24 {
                for x in -24..=24 {
                    let uv = Vec2::new(x as f32, y as f32) / 50.0;
                    let a = damage::sample(code, seed, uv);
                    let b = damage::sample(code, seed, uv);
                    assert_eq!(
                        (a.coverage, a.height, a.color),
                        (b.coverage, b.height, b.color)
                    );
                    assert!(
                        a.color.is_finite()
                            && a.height.is_finite()
                            && (0.0..=1.0).contains(&a.coverage)
                    );
                }
            }
        }
    }
    let hole = damage::sample(5, 31, Vec2::ZERO);
    let rim = damage::sample(5, 31, Vec2::new(0.18, 0.0));
    let primer = damage::sample(5, 31, Vec2::new(0.31, 0.0));
    assert!(hole.height < -1.0 && hole.color.max_element() < 0.04 && hole.metal < 0.1);
    assert!(rim.color.min_element() > 0.3 && rim.metal > 0.9);
    assert!(primer.metal < rim.metal && primer.color.x > primer.color.z);
    let burn = damage::sample(7, 31, Vec2::ZERO);
    assert!(burn.metal > 0.18 && burn.metal < 0.30 && burn.color.x < 0.1 && burn.rough > 0.7);
    let soot = damage::sample(7, 31, Vec2::new(0.0, 0.35));
    assert!(soot.coverage > damage::sample(7, 31, Vec2::new(0.0, -0.35)).coverage);
    for code in [9, 10] {
        let g = damage::sample(code, 31, Vec2::ZERO);
        assert!(g.height < 0.0 && g.color.min_element() > 0.5 && g.metal > 0.9);
    }
    // A downhill stain spreads laterally, then fades at its tail.
    let width = |t: f32| {
        (-50..=50)
            .map(|x| damage::sample(11, 31, Vec2::new(x as f32 / 100.0, 0.5 - t)).coverage)
            .filter(|a| *a > 0.05)
            .count()
    };
    assert!(width(0.55) > width(0.15));
    assert!(damage::sample(11, 31, Vec2::new(0.0, -0.49)).coverage < 0.01);
    let oil = damage::sample(12, 31, Vec2::new(0.0, 0.3));
    assert!(oil.rough < 0.15 && oil.color.x > oil.color.z * 2.0 && oil.coverage < 0.85);
}
#[test]
fn closeup_preset_substrate_preserves_paint() {
    for name in ["dusty_khaki", "olive_drab", "gunmetal", "worn_steel"] {
        let m = Material::preset(name).unwrap();
        let mut c = plate();
        let stroke = serde_json::from_value(
            json!({"points":[[0,0,0.023]],"radius":0.04,"color":[0.9,0.05,0.02],"hardness":1}),
        )
        .unwrap();
        crate::paint::stroke(&mut c, &stroke).unwrap();
        let p = Vec3::new(0.0, 0.0, 0.023);
        let a = m.painted_color(Vec3::new(0.8, 0.6, 0.3), c.paint.as_ref(), p);
        assert!(a.x > 0.8 && a.y < 0.1, "{name}: {a}");
        let b = m.painted_color(Vec3::ONE, None, p);
        assert_eq!(b, Vec3::ONE);
        assert!(m.base_color.is_none());
        let off = Material {
            keep_paint: false,
            ..m.clone()
        };
        assert_eq!(off.painted_color(Vec3::ONE, c.paint.as_ref(), p), b);
        let mesh = c.build_mesh_with(std::slice::from_ref(&m), name);
        assert!(mesh.vertices.iter().any(|v| v.color[0] > v.color[1] * 2.0));
        let back: Material = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert_eq!(back, off);
    }
}
#[test]
fn closeup_authored_color_and_explicit_override() {
    let tan = Vec3::new(0.72, 0.57, 0.40);
    for name in ["military_paint", "battle_worn", "desert_dusty", "dusty_khaki", "olive_drab", "gunmetal", "worn_steel"] {
        let m = Material::resolve(&[], name);
        assert_eq!(m.painted_color(tan, None, Vec3::ZERO), tan, "{name}");
        let explicit = Material {name:name.into(), base_color:Some([0.21,0.24,0.17]), ..m};
        let resolved = Material::resolve(&[explicit], name);
        assert_eq!(resolved.painted_color(tan, None, Vec3::ZERO), Vec3::new(0.21,0.24,0.17));
    }
}

#[test]
fn closeup_soot_is_continuous_and_groove_has_no_gaps() {
    for seed in [0, 31, 82] {
        // Sample at submillimetre steps on a half-metre stamp. A smooth
        // cross section must not jump by a visible texel or blister cell.
        for y in [0.19, 0.27, 0.36] {
            let mut previous = damage::sample(7, seed, Vec2::new(-0.45,y)).coverage;
            for x in -449..=450 {
                let a = damage::sample(7, seed, Vec2::new(x as f32 * 0.001,y)).coverage;
                assert!((a - previous).abs() < 0.015, "soot opacity step");
                previous = a;
            }
        }
        for y in -40..=40 {
            let g = damage::sample(9, seed, Vec2::new(0.0, y as f32 * 0.01));
            assert!(g.coverage > 0.99 && g.metal > 0.99 && g.color.min_element() > 0.60);
        }
        let width = |t:f32| (-100..=100).filter(|x| {
            damage::sample(12,seed,Vec2::new(*x as f32 * 0.001,0.5-t)).coverage > 0.5
        }).count();
        assert!(width(0.65) > width(0.12), "oil must spread downstream");
    }
}

/// Sample the actual continuous curvature of a metre box with a 40 mm
/// rounded bevel, then evaluate the same per-hit masks used by film/bakes.
/// Width is measured across the *surface arc*, not its foreshortened image.
#[test]
fn closeup_bevel_wear_width_and_ragged_boundaries() {
    let clay: Clay = serde_json::from_value(json!({"sharp":true,"resolution":150,
        "blobs":[{"type":"box","center":[0,0,0],"size":[1,1,1],"round":0.04,"blend":0}]})).unwrap();
    let field = crate::clay::Field::new(&clay, clay.cell_size().unwrap());
    let step = 0.0005;
    let rows = 127;
    let columns = 801;
    let mut mesh = crate::mesh::Mesh::default();
    for x in 0..columns {
        for y in 0..rows {
            let a = y as f32 * step / 0.04;
            let n = Vec3::new(0.0, a.sin(), a.cos());
            let p = Vec3::new(-0.4 + x as f32 * step, 0.46, 0.46) + n * 0.04;
            let mut v: crate::mesh::Vertex = bytemuck::Zeroable::zeroed();
            v.pos = p.to_array();
            v.normal = n.to_array();
            mesh.vertices.push(v);
        }
    }
    let wear = crate::weather::clay_wear_maps(&mesh, &field, clay.wear_width());
    let mut reports = Vec::new();
    for amount in [0.65, 1.0] {
        let m = Material { layers: damage::wear_layers("battle_worn", amount).unwrap(), ..Default::default() };
        let primer = m.layers.iter().find(|l| l.name == "edge chips" && l.metallic == Some(0.0)).unwrap();
        let steel = m.layers.iter().find(|l| l.name == "edge chips" && l.metallic == Some(1.0)).unwrap();
        let mut widths = Vec::new();
        let mut max_run = 0usize;
        let mut rim = 0;
        let mut gap_samples = 0;
        let mut hidden = 0.0;
        let mut exposed = 0.0;
        for x in 0..columns {
            let mut covered = 0;
            let mut run = 0;
            for y in 0..rows {
                let i = x * rows + y;
                let v = &mesh.vertices[i];
                let at = At { p: Vec3::from(v.pos), n: Vec3::from(v.normal),
                    edge: wear[i][0], cavity: wear[i][1], ..Default::default() };
                let p = crate::smart::mask(&m, primer, &at);
                let s = crate::smart::mask(&m, steel, &at);
                assert!(p >= s, "steel must stay inside primer");
                rim += usize::from(p > 0.5 && s < 0.1);
                if y == rows / 2 { gap_samples += usize::from(s <= 0.5); }
                if s > 0.5 { covered += 1; run += 1; } else { run = 0; }
                max_run = max_run.max(run);
                exposed += s;
                hidden += crate::smart::mask(&m, steel, &At { occlusion: 0.9, ..at });
            }
            widths.push(covered as f32 * step);
        }
        let mean = widths.iter().sum::<f32>() / columns as f32;
        let deviation = (widths.iter().map(|w| (w-mean).powi(2)).sum::<f32>() / columns as f32).sqrt();
        // Gaps along the apex trace; separate flakes elsewhere across the
        // bevel should not count as a continuous stripe along this edge.
        let gaps = gap_samples as f32 / columns as f32;
        eprintln!("bevel wear amount={amount}: mean={:.2} mm, max connected width={:.2} mm, width stddev={:.2} mm, gaps={:.1}%", mean*1000.0, max_run as f32*step*1000.0, deviation*1000.0, gaps*100.0);
        assert!(mean > 0.0001 && mean < 0.010, "equivalent coverage width {mean}");
        assert!(max_run as f32 * step <= 0.010, "oversized connected flake");
        assert!(deviation > 0.0004, "straight/uniform boundary");
        assert!(gaps > 0.15 && gaps < 0.98, "long edges need visible interruptions");
        assert!(hidden < exposed * 0.1 && rim > 50, "shelter and primer margin");
        reports.push(json!({"amount":amount,"mean_width_mm":mean*1000.0,
            "max_connected_width_mm":max_run as f32*step*1000.0,
            "width_stddev_mm":deviation*1000.0,"gap_fraction":gaps}));
    }
    // A flat panel may never chip, even at a noise maximum.
    let m = Material { layers: damage::wear_layers("battle_worn", 1.0).unwrap(), ..Default::default() };
    for l in m.layers.iter().filter(|l| l.name == "edge chips") {
        assert_eq!(crate::smart::mask(&m, l, &At { n: Vec3::Y, ..Default::default() }), 0.0);
    }
    if std::env::var_os("CLOSEUP_RECORD_METRICS").is_some() {
        std::fs::write(output().join("edge_wear_metrics.json"), serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
}

#[test]
fn closeup_grime_is_relative_to_each_part() {
    let mut m = Material {
        layers: damage::wear_layers("battle_worn", 1.0).unwrap(),
        ..Default::default()
    };
    damage::part_grime(&mut m, Vec3::new(-1.0, 5.0, 0.0), Vec3::new(1.0, 6.0, 1.0));
    let layer = m.layers.last().unwrap();
    let low = At {
        p: Vec3::new(0.0, 5.05, 0.3),
        n: Vec3::Z,
        ..Default::default()
    };
    let high = At {
        p: Vec3::new(0.0, 5.95, 0.3),
        ..low
    };
    assert!(crate::smart::mask(&m, layer, &low) > crate::smart::mask(&m, layer, &high));
    let base = Look::of(&m, Vec3::splat(0.4), 0.7, Vec3::Z);
    assert!(
        crate::smart::layers(&m, &low, base).color.length()
            < crate::smart::layers(&m, &high, base).color.length()
    );
}
#[test]
#[ignore]
fn closeup_sheets() {
    let kinds = [
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
        "ricochet",
    ];

    for kind in &kinds {
        let mut c = plate();
        let base = c.build_mesh();
        let mut args = json!({"kind":if *kind=="ricochet" {"bullet_hit"}else{kind},"point":[0,0,0.024],"normal":[0,0,1],"size":0.19,"seed":31});
        if *kind == "bullet_hit" {
            args["caliber_mm"] = json!(25);
            args["size"] = json!(0.10);
        }
        if *kind == "ricochet" {
            args["size"] = json!(0.09);
            args["impact_angle"] = json!(70);
            args["rotate"] = json!(65);
        }
        if ["rust_streak", "oil_leak", "water_stain"].contains(kind) {
            args["point"] = json!([0, 0.15, 0.024]);
            args["size"] = json!(0.12);
            args["length"] = json!(0.30);
        }
        if ["gouge", "scrape"].contains(kind) {
            args["points"] = json!([[-0.06, -0.11, 0.024], [0.06, 0.12, 0.024]]);
            args.as_object_mut().unwrap().remove("point");
            args["size"] = json!(0.05);
        }
        if ["bend", "tear"].contains(kind) {
            args["hinge"] = json!([[0, -0.18, 0.024], [0, 0.18, 0.024]]);
            args["angle"] = json!(25);
            args["size"] = json!(0.4);
        }
        c.decals = damage::stamps(
            &base,
            Mat4::IDENTITY,
            true,
            &serde_json::from_value(args).unwrap(),
        )
        .unwrap();
        let m = Material {
            name: "panel".into(),
            layers: damage::wear_layers("military_paint", 1.0).unwrap(),
            ..Default::default()
        };
        render(
            &format!("damage_{kind}"),
            vec![item(&c, m, Mat4::IDENTITY)],
            film::Camera {
                eye: Vec3::new(0.14, 0.1, 0.95),
                target: Vec3::ZERO,
                up: Vec3::Y,
                fov_deg: 27.0,
                ortho: None,
            },
            640,
            640,
            None,
        );
    }
    contact_sheet("damage_sheet", &kinds.map(|n| format!("damage_{n}")), 4);
    closeup_paint_sheet();
}
#[test]
#[ignore]
fn closeup_paint_sheet() {
    let mut paint_names = Vec::new();
    for name in [
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
    ]
    .iter()
    {
        let m = Material::preset(name).unwrap();
        let mut c = plate();
        crate::paint::stroke(&mut c,&serde_json::from_value(json!({"points":[[-0.08,0.1,0.023],[0.08,0.1,0.023]],"radius":0.008,"color":[0.82,0.80,0.72],"hardness":1})).unwrap()).unwrap();
        let label = format!("paint_{name}");
        render(
            &label,
            vec![item(&c, m, Mat4::IDENTITY)],
            film::Camera {
                eye: Vec3::new(0.14, 0.1, 0.95),
                target: Vec3::ZERO,
                up: Vec3::Y,
                fov_deg: 27.0,
                ortho: None,
            },
            640,
            640,
            None,
        );
        paint_names.push(label);
    }
    contact_sheet("paint_sheet", &paint_names, 5);
}
#[test]
#[ignore]
fn closeup_mech() {
    let source = PathBuf::from(std::env::var("CLOSEUP_SCENE").expect("CLOSEUP_SCENE"));
    let bytes = std::fs::read(source).unwrap();
    std::fs::write(output().join("mech_before.json"), &bytes).unwrap();
    let mut scene: Scene = serde_json::from_slice(&bytes).unwrap();
    scene.frame = Some(0.0);
    let before_scene = scene.clone();
    // Reconnect legacy damage material copies before rebuilding shared finishes.
    let names: Vec<_> = scene
        .materials
        .iter()
        .filter(|m| !m.name.starts_with("damage_"))
        .map(|m| m.name.clone())
        .collect();
    let restore = |name: &str| {
        names
            .iter()
            .filter(|n| name.ends_with(n.as_str()))
            .max_by_key(|n| n.len())
            .cloned()
            .unwrap_or_else(|| name.into())
    };
    for o in &mut scene.objects {
        o.bake = None;
        if let Some(m) = &mut o.material {
            *m = restore(m);
        }
        if let Some(c) = &mut o.clay {
            for b in &mut c.blobs {
                b.material = restore(&b.material);
            }
            c.decals.retain(|d| d.wear_preset.is_empty());
            // The authored soot stamps were offset below nearby craters.
            // Reapply those at the impact; unrelated burns keep their position.
            let impacts:Vec<_> = c.decals.iter().filter(|d| d.shape == crate::smart::Shape::BulletHit).cloned().collect();
            for d in &mut c.decals {
                if d.shape == crate::smart::Shape::Scorch {
                    let p = Vec3::from(d.center);
                    if let Some(hit) = impacts.iter().min_by(|a,b|
                        Vec3::from(a.center).distance_squared(p).total_cmp(&Vec3::from(b.center).distance_squared(p)))
                        && Vec3::from(hit.center).distance(p) < d.size * 0.4 {
                        d.center = hit.center;
                        d.normal = hit.normal;
                    }
                }
            }
            // Deposit soot first, then retain the torn metal/hole above it.
            c.decals.sort_by_key(|d| u8::from(d.shape != crate::smart::Shape::Scorch));
            for d in &mut c.decals {
                if (11..=13).contains(&d.shape.code()) {
                    d.opacity = 1.0;
                    if d.shape == crate::smart::Shape::RustStreak {
                        d.size *= 1.8;
                        d.aspect = (d.aspect / 1.8).max(0.1);
                    }
                }
            }
        }
    }
    scene.materials.retain(|m| !m.name.starts_with("damage_"));
    for m in &mut scene.materials {
        if matches!(m.name.as_str(), "tan_paint" | "armor_khaki") {
            *m = Material {
                name: m.name.clone(),
                base_color: None,
                roughness: 0.76,
                layers: damage::wear_layers("military_paint", 1.0).unwrap(),
                ..Default::default()
            };
        }
        if m.name == "gunmetal" {
            *m = Material::preset("gunmetal").unwrap();
        }
        if m.name == "polished_metal" {
            *m = Material::preset("polished_metal").unwrap();
        }
    }
    let ids: Vec<_> = scene
        .objects
        .iter()
        .filter(|o| {
            o.clay.is_some()
                && !o.name.starts_with("Soldier")
                && o.name != "Helmet"
                && o.name != "Rifle"
        })
        .map(|o| o.id)
        .collect();
    // One representative per shared material suffices; avoid regenerating runoff
    // across the original's already dense damage segment lists.
    let mut materials = std::collections::HashSet::new();
    for id in ids {
        let o = scene.get(id).unwrap();
        let name = o.material.clone().unwrap_or_default();
        if materials.insert(name) {
            damage::apply_wear(&mut scene, id, "battle_worn", 0.65).unwrap();
            damage::apply_wear(&mut scene, id, "desert_dusty", 0.45).unwrap();
        }
    }
    std::fs::write(
        output().join("mech_after.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    // The soldier is outside both crops; avoid rebuilding his high-resolution
    // human skin weights just to inspect the vehicle's paint.
    let mut render_scene = scene.clone();
    let mut excluded = std::collections::HashSet::new();
    for o in &scene.objects {
        if o.name.starts_with("Soldier") || o.name == "Helmet" || o.name == "Rifle" {
            excluded.insert(o.id);
            excluded.extend(scene.descendants(o.id));
        }
    }
    render_scene.objects.retain(|o| !excluded.contains(&o.id));
    let mut before_render = before_scene;
    before_render.objects.retain(|o| !excluded.contains(&o.id));
    for o in &mut before_render.objects { o.bake = None; }
    for (stage, source) in [("after", &render_scene), ("before", &before_render)] {
        let items = crate::paint_checks::scene_items(source);
        let meshes = |items: &[film::Item]| {
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
        };
        render(
            &format!("mech_{stage}_closeup"),
            meshes(&items),
            film::Camera {
                eye: Vec3::new(0.7, 6.5, 3.5),
                target: Vec3::new(0.23, 5.75, 0.9),
                up: Vec3::Y,
                fov_deg: 27.0,
                ortho: None,
            },
            1500,
            1000,
            Some(2.8),
        );
        render(
            &format!("mech_{stage}_detail"),
            meshes(&items),
            film::Camera {
                eye: Vec3::new(6.4, 7.6, 9.0),
                target: Vec3::new(1.9, 6.1, 0.4),
                up: Vec3::Y,
                fov_deg: 19.46,
                ortho: None,
            },
            1500,
            1000,
            Some(4.0),
        );
    }
}

#[test]
fn closeup_controls_roundtrip_and_reject_invalid_input() {
    let c = plate();
    let mesh = c.build_mesh();
    let request:damage::Stamp=serde_json::from_value(json!({"kind":"bullet_hit","point":[0,0,0.024],"impact_angle":70,"rotate":23,"caliber_mm":25})).unwrap();
    let hit = damage::stamps(&mesh, Mat4::IDENTITY, true, &request).unwrap();
    assert!(hit[0].aspect > 2.8 && hit[0].rotate == 23.0);
    let request:damage::Stamp=serde_json::from_value(json!({"kind":"rust_streak","point":[0,0.15,0.024],"width":0.1,"softness":1.7,"color":[0.3,0.24,0.12]})).unwrap();
    let paths = damage::stamps(&mesh, Mat4::IDENTITY, true, &request).unwrap();
    assert!(
        paths
            .iter()
            .all(|d| d.softness == 1.7 && d.color == Some([0.3, 0.24, 0.12]))
    );
    for pair in paths.windows(2) {
        let a = damage::sample_decal(&pair[0], Vec2::new(0.0, -0.5)).coverage * pair[0].opacity;
        let b = damage::sample_decal(&pair[1], Vec2::new(0.0, 0.5)).coverage * pair[1].opacity;
        assert!(
            (a - b).abs() < 1e-5,
            "runoff opacity jumps at a segment seam: {a} / {b}"
        );
    }
    let json = serde_json::to_string(&paths).unwrap();
    let back: Vec<crate::smart::Decal> = serde_json::from_str(&json).unwrap();
    assert_eq!(paths, back);
    let mut invalid = request;
    invalid.softness = 0.0;
    assert!(damage::stamps(&mesh, Mat4::IDENTITY, true, &invalid).is_err());
}

#[test]
fn closeup_substrate_and_markings_survive_fast_exact_bake_and_glb() {
    let mut scene = Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    let mut c = plate();
    c.resolution = 48;
    crate::paint::stroke(
        &mut c,
        &serde_json::from_value(
            json!({"points":[[0,0,0.023]],"radius":0.055,"color":[0.9,0.05,0.02],"hardness":1}),
        )
        .unwrap(),
    )
    .unwrap();
    let m = Material::preset("olive_drab").unwrap();
    scene.materials.push(m);
    let o = scene.get_mut(id).unwrap();
    o.clay = Some(c);
    o.material = Some("olive_drab".into());
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    for exact in [false, true] {
        let settings = crate::bake::Settings {
            triangles: 500,
            texture: 512,
            exact,
            ..Default::default()
        };
        scene.get_mut(id).unwrap().bake = Some(settings);
        meshes.bake(&scene, id, settings).unwrap();
        let tx = meshes.baked(id).unwrap().0.textures.as_ref().unwrap();
        assert!(
            tx.color.chunks_exact(4).any(|c| c[0] > 180 && c[1] < 60),
            "red stencil, exact={exact}"
        );
        assert!(
            tx.color.chunks_exact(4).any(|c| c[0] > 100 && c[0] > c[1] && c[1] > c[2] + 10),
            "authored tan substrate, exact={exact}"
        );
        let out = crate::gltf::export(
            &scene,
            &meshes,
            &crate::gltf::Options {
                roots: vec![],
                animations: false,
                fps: 24.0,
                file_stem: String::new(),
                lods: false,
                atlas: false,
                texel_density: None,
            },
        )
        .unwrap();
        assert!(out.bytes.windows(4).any(|w| w == b"\x89PNG"));
    }
}

fn contact_sheet(file: &str, names: &[String], columns: usize) {
    fn glyph(c: char) -> [u8; 7] {
        match c {
            'A' => [14, 17, 17, 31, 17, 17, 17],
            'B' => [30, 17, 17, 30, 17, 17, 30],
            'C' => [14, 17, 16, 16, 16, 17, 14],
            'D' => [30, 17, 17, 17, 17, 17, 30],
            'E' => [31, 16, 16, 30, 16, 16, 31],
            'F' => [31, 16, 16, 30, 16, 16, 16],
            'G' => [14, 17, 16, 23, 17, 17, 14],
            'H' => [17, 17, 17, 31, 17, 17, 17],
            'I' => [14, 4, 4, 4, 4, 4, 14],
            'J' => [7, 2, 2, 2, 18, 18, 12],
            'K' => [17, 18, 20, 24, 20, 18, 17],
            'L' => [16, 16, 16, 16, 16, 16, 31],
            'M' => [17, 27, 21, 21, 17, 17, 17],
            'N' => [17, 25, 21, 19, 17, 17, 17],
            'O' => [14, 17, 17, 17, 17, 17, 14],
            'P' => [30, 17, 17, 30, 16, 16, 16],
            'Q' => [14, 17, 17, 17, 21, 18, 13],
            'R' => [30, 17, 17, 30, 20, 18, 17],
            'S' => [15, 16, 16, 14, 1, 1, 30],
            'T' => [31, 4, 4, 4, 4, 4, 4],
            'U' => [17, 17, 17, 17, 17, 17, 14],
            'V' => [17, 17, 17, 17, 17, 10, 4],
            'W' => [17, 17, 17, 21, 21, 21, 10],
            'X' => [17, 17, 10, 4, 10, 17, 17],
            'Y' => [17, 17, 10, 4, 4, 4, 4],
            'Z' => [31, 1, 2, 4, 8, 16, 31],
            _ => [0; 7],
        }
    }

    let images: Vec<_> = names
        .iter()
        .map(|n| {
            image::open(output().join(format!("{n}.png")))
                .unwrap()
                .to_rgb8()
        })
        .collect();
    let (w, h) = (images[0].width() as usize, images[0].height() as usize);
    let mut sheet = image::RgbImage::from_pixel(
        (w * columns) as u32,
        ((h + 28) * names.len().div_ceil(columns)) as u32,
        image::Rgb([20, 23, 26]),
    );
    for (i, (name, img)) in names.iter().zip(images).enumerate() {
        let (ox, oy) = (i % columns * w, i / columns * (h + 28));
        image::imageops::replace(&mut sheet, &img, ox as i64, oy as i64);
        for (j, c) in name.replace('_', " ").to_uppercase().chars().enumerate() {
            for (y, row) in glyph(c).iter().enumerate() {
                for x in 0..5 {
                    if row & (1 << (4 - x)) != 0 {
                        for dy in 0..2 {
                            for dx in 0..2 {
                                sheet.put_pixel(
                                    (ox + 10 + j * 12 + x * 2 + dx) as u32,
                                    (oy + h + 7 + y * 2 + dy) as u32,
                                    image::Rgb([210, 219, 224]),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    sheet.save(output().join(format!("{file}.png"))).unwrap();
}
