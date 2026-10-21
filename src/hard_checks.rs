//! Reproducible forged-prop film comparisons through the render_film backend.
use crate::{
    clay::Clay,
    film,
    material::Material,
    rig::Rig,
    scene::{Scene, Shape},
};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn clay(blobs: Value, cell: f32) -> Clay {
    let c: Clay =
        serde_json::from_value(json!({"blobs":blobs,"resolution":192,"detail_cell":cell})).unwrap();
    for b in &c.blobs {
        b.validate().unwrap();
    }
    c
}
fn profile(radius: f32, n: usize) -> Value {
    json!(
        (0..n)
            .map(|i| {
                let a = std::f32::consts::TAU * i as f32 / n as f32;
                [radius * a.cos(), radius * a.sin()]
            })
            .collect::<Vec<_>>()
    )
}
#[derive(serde::Deserialize)]
struct View {
    name: String,
    center: Vec3,
    radius: f32,
    yaw: f32,
}
#[derive(serde::Deserialize)]
struct Showcase {
    name: String,
    blobs: Value,
    cell: f32,
    center: Vec3,
    radius: f32,
    yaw: f32,
    macros: Vec<View>,
}

fn materials() -> Vec<Material> {
    [("forged_steel", 0.36), ("aged_brass", 0.40)]
        .iter()
        .map(|&(name, roughness)| {
            let mut m = Material::preset("metal").unwrap();
            m.name = name.into();
            m.roughness = roughness;
            m.edge_wear = 0.28;
            m.cavity = 0.35;
            m.dirt = 0.12;
            m.variation = 0.13;
            m.gloss_variation = 0.18;
            m.variation_scale = 0.025;
            m.detail = Some("metal_scratched".into());
            m.detail_scale = 0.035;
            m.detail_strength = 0.22;
            m.detail_color = 0.10;
            m
        })
        .collect()
}
// Local replicas retain the showcase's exact section/geometry, resolving
// tiny features several times finer without refining a meter-long prop.
fn macro_clay(name: &str, c: &Clay) -> Option<Clay> {
    let mut local = c.clone();
    match name {
        "sword_edge" => {
            local.blobs.truncate(1);
            if let crate::clay::Form::Extrude { depth, center, .. } = &mut local.blobs[0].form {
                *depth = 0.12;
                center.y = 0.39;
            }
            local.detail_cell = Some(0.0001);
        }
        "crossguard_corner" => {
            local.blobs = vec![c.blobs[2].clone()];
            local.detail_cell = Some(0.00025);
        }
        "rivet" => {
            let crate::clay::Form::Array { source, .. } = &c.blobs[2].form else {
                unreachable!()
            };
            let mut rivet = c.blobs[2].clone();
            rivet.form = *source.clone();
            if let crate::clay::Form::Lathe { center, .. } = &mut rivet.form {
                center.y += 0.266;
                center.z = 0.0065;
            }
            let backing=serde_json::from_value(json!({"type":"box","center":[0,0.266,0],"size":[0.08,0.08,0.011],"blend":0,"material":"forged_steel","color":[0.36,0.40,0.46]})).unwrap();
            local.blobs = vec![backing, rivet];
            local.detail_cell = Some(0.0001);
        }
        "engraved_groove" => {
            let backing=serde_json::from_value(json!({"type":"box","center":[0.02,0.21,0],"size":[0.20,0.20,0.011],"blend":0,"material":"forged_steel","color":[0.36,0.40,0.46]})).unwrap();
            local.blobs = vec![backing, c.blobs[4].clone(), c.blobs[5].clone()];
            local.detail_cell = Some(0.00015);
        }
        "helm_trim" => {
            local.detail_cell = Some(0.0002);
        }
        "plate_rim" => {
            local.detail_cell = Some(0.0003);
        }
        _ => return None,
    }
    Some(local)
}

fn pair(spec: &Showcase, c: Clay, rig: Option<Rig>, timings: &mut String) {
    let name = &spec.name;
    if std::env::var("HARD_CHECK").is_ok_and(|only| only != *name) {
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hard_check");
    std::fs::create_dir_all(&dir).unwrap();
    let library = materials();
    let material = library[0].clone();
    let mut views = vec![View {
        name: name.clone(),
        center: spec.center,
        radius: spec.radius,
        yaw: spec.yaw,
    }];
    views.extend(spec.macros.iter().map(|v| View {
        name: v.name.clone(),
        center: v.center,
        radius: v.radius,
        yaw: v.yaw,
    }));
    let cameras: Vec<_> = views
        .iter()
        .map(|v| {
            let angle = v.yaw.to_radians();
            film::Camera {
                eye: v.center
                    + Vec3::new(angle.sin(), 0.15, angle.cos()).normalize() * v.radius * 3.8,
                target: v.center,
                up: Vec3::Y,
                fov_deg: 30.0,
                ortho: None,
            }
        })
        .collect();
    let size = 768;
    let settings = film::Settings {
        width: size,
        height: size,
        samples: 64,
        bounces: 4,
        time_limit: Duration::from_secs(180),
        floor: false,
        exposure: 1.20,
        denoise: true,
    };
    let look = Arc::new(
        film::Look::parse(&json!({"device":"auto","tonemap":"agx","vignette":0.12})).unwrap(),
    );
    let mut images = Vec::new();
    for (label, sharp) in [("before", false), ("after", true)] {
        let mut c = c.clone();
        c.sharp = sharp;
        let mut scene = Scene {
            materials: library.clone(),
            ..Scene::default()
        };
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.name = name.clone();
        o.material = Some("forged_steel".into());
        o.clay = Some(c.clone());
        o.rig = rig.clone();
        if std::env::var("HARD_PHASE").is_ok_and(|phase| phase != label) {
            let saved: Scene = serde_json::from_slice(
                &std::fs::read(dir.join(format!("{name}_{label}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(saved.get(id).unwrap().clay.as_ref()).unwrap(),
                serde_json::to_value(Some(&c)).unwrap(),
                "cached phase differs; run without HARD_PHASE"
            );
            images.push(
                views
                    .iter()
                    .map(|v| {
                        let img = image::open(dir.join(format!("{}_{label}.png", v.name)))
                            .unwrap()
                            .to_rgb8();
                        assert_eq!(img.dimensions(), (size as u32, size as u32));
                        film::Image {
                linear: Vec::new(),
                            width: size,
                            height: size,
                            pixels: img.pixels().map(|p| p.0).collect(),
                            samples: 64,
                            seconds: 0.0,
                        }
                    })
                    .collect::<Vec<_>>(),
            );
            continue;
        }
        std::fs::write(
            dir.join(format!("{name}_{label}.json")),
            serde_json::to_vec(&scene).unwrap(),
        )
        .unwrap();
        let start = Instant::now();
        let mesh = if name == "breastplate" {
            c.build_fine_region(
                Vec3::new(-0.70, 0.72, -0.5),
                Vec3::new(0.70, 1.80, 0.5),
                &library,
                "forged_steel",
            )
            .unwrap()
        } else {
            c.build_fine(&library, "forged_steel").unwrap()
        };
        let seconds = start.elapsed().as_secs_f32();
        timings.push_str(&format!(
            "| {name} | {label} | {seconds:.3} | {} | {} |\n",
            mesh.vertices.len(),
            mesh.indices.len() / 3
        ));
        eprintln!(
            "{name} {label}: mesh {seconds:.3}s, {} triangles",
            mesh.indices.len() / 3
        );
        let meshes = film::Meshes::new(vec![film::Item {
            mesh: Arc::new(mesh),
            world: Mat4::IDENTITY,
            tint: [1.0; 3],
            surface: material.surface(),
            material: material.clone(),
        }]);
        let broad: Vec<_> = views
            .iter()
            .enumerate()
            .filter(|(i, v)| *i == 0 || macro_clay(&v.name, &c).is_none())
            .map(|(i, _)| i)
            .collect();
        let mut rendered: Vec<_> = (0..views.len())
            .map(|_| film::Image {
                linear: Vec::new(),
                width: size,
                height: size,
                pixels: Vec::new(),
                samples: 0,
                seconds: 0.0,
            })
            .collect();
        let broad_cameras: Vec<_> = broad.iter().map(|&i| cameras[i]).collect();
        for (i, img) in broad.into_iter().zip(film::render_views_with_look(
            &meshes,
            &[],
            &broad_cameras,
            settings,
            look.clone(),
            &Default::default(),
        )) {
            rendered[i] = img;
        }
        for (i, v) in views.iter().enumerate().skip(1) {
            if let Some(local) = macro_clay(&v.name, &c) {
                let start = Instant::now();
                let mesh = if v.name == "plate_rim" || v.name == "helm_trim" {
                    let pad = Vec3::splat(v.radius * 2.5);
                    local
                        .build_fine_region(v.center - pad, v.center + pad, &library, "forged_steel")
                        .unwrap()
                } else {
                    local.build_fine(&library, "forged_steel").unwrap()
                };
                let seconds = start.elapsed().as_secs_f32();
                timings.push_str(&format!(
                    "| {} | {label} | {seconds:.3} | {} | {} |\n",
                    v.name,
                    mesh.vertices.len(),
                    mesh.indices.len() / 3
                ));
                let meshes = film::Meshes::new(vec![film::Item {
                    mesh: Arc::new(mesh),
                    world: Mat4::IDENTITY,
                    tint: [1.0; 3],
                    surface: material.surface(),
                    material: material.clone(),
                }]);
                rendered[i] = film::render_views_with_look(
                    &meshes,
                    &[],
                    &[cameras[i]],
                    settings,
                    look.clone(),
                    &Default::default(),
                )
                .pop()
                .unwrap();
                let mut local_scene = scene.clone();
                local_scene.get_mut(id).unwrap().clay = Some(local);
                std::fs::write(
                    dir.join(format!("{}_{label}.json", v.name)),
                    serde_json::to_vec(&local_scene).unwrap(),
                )
                .unwrap();
            }
        }
        for (v, img) in views.iter().zip(&rendered) {
            assert!(img.samples > 0);
            eprintln!(
                "{} {label}: {} samples, {:.2}s",
                v.name, img.samples, img.seconds
            );
            let pixels: Vec<u8> = img.pixels.iter().flatten().copied().collect();
            image::save_buffer(
                dir.join(format!("{}_{label}.png", v.name)),
                &pixels,
                size as u32,
                size as u32,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
        images.push(rendered);
    }
    for (i, v) in views.iter().enumerate() {
        let mut pixels = Vec::new();
        for y in 0..size {
            for imgs in &images {
                for rgb in &imgs[i].pixels[y * size..(y + 1) * size] {
                    pixels.extend(rgb);
                }
            }
        }
        image::save_buffer(
            dir.join(format!("{}.png", v.name)),
            &pixels,
            (size * 2) as u32,
            size as u32,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
}

fn decorate_pauldron(c: &mut Clay, attached: bool) {
    use crate::clay::Field;
    let base = if attached {
        c.blobs[2].clone()
    } else {
        c.blobs[0].clone()
    };
    let center = if attached {
        Vec3::new(0.205, 1.30, 0.02)
    } else {
        Vec3::ZERO
    };
    let dome = Clay {
        blobs: vec![base],
        ..c.clone()
    };
    let field = Field::new(&dome, 0.001);
    let all = field.near(center - 1.0, center + 1.0);
    let front = |x: f32, y: f32, sign: f32| {
        let mut z = 0.30;
        while z > 0.0 && field.sample(center + Vec3::new(x, y, z), &all).0 > 0.0 {
            z -= 0.0005;
        }
        [center.x + x, center.y + y, center.z + sign * z]
    };
    let mut crest = vec![
        front(0.0, if attached { 0.064 } else { 0.003 }, 1.0),
        front(0.0, 0.085, 1.0),
        front(0.0, 0.102, 1.0),
    ];
    crest.push((center + Vec3::Y * 0.106).to_array());
    crest.push(front(0.0, 0.102, -1.0));
    crest.push(front(0.0, 0.08, -1.0));
    let sweeps: Vec<_> = c
        .blobs
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.form, crate::clay::Form::Sweep { .. }))
        .map(|(i, _)| i)
        .collect();
    for (j, i) in sweeps.into_iter().enumerate() {
        if let crate::clay::Form::Sweep { points, .. } = &mut c.blobs[i].form {
            *points = if j == 0 {
                crest.iter().map(|p| Vec3::from(*p)).collect()
            } else {
                let sign = if j == 1 { -1.0 } else { 1.0 };
                [
                    [sign * 0.028, 0.02],
                    [sign * 0.07, 0.036],
                    [sign * 0.085, 0.066],
                ]
                .iter()
                .map(|p| Vec3::from(front(p[0], p[1], 1.0)))
                .collect()
            };
        }
    }
}

/// Follow the fitted forging with actual raised trim and engraved ornament.
fn decorate_breastplate(c: &mut Clay) {
    use crate::clay::Field;
    let fitting = Clay {
        blobs: c.blobs[..2].to_vec(),
        ..c.clone()
    };
    let field = Field::new(&fitting, 0.002);
    let all = field.near(Vec3::splat(-10.0), Vec3::splat(10.0));
    let surface = |x: f32, y: f32| {
        let mut z = 0.40;
        while z > -0.1 && field.sample(Vec3::new(x, y, z), &all).0 > 0.0 {
            z -= 0.001;
        }
        let (mut lo, mut hi) = (z, z + 0.001);
        for _ in 0..12 {
            let mid = (lo + hi) * 0.5;
            if field.sample(Vec3::new(x, y, mid), &all).0 < 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        [x, y, (lo + hi) * 0.5]
    };
    let paths = [
        vec![
            [-0.13, 0.966],
            [-0.07, 0.96],
            [0.0, 0.96],
            [0.07, 0.96],
            [0.13, 0.966],
        ],
        vec![
            [-0.15, 1.395],
            [-0.07, 1.378],
            [0.0, 1.353],
            [0.07, 1.378],
            [0.15, 1.395],
        ],
        vec![
            [0.0, 0.985],
            [0.0, 1.08],
            [0.0, 1.18],
            [0.0, 1.29],
            [0.0, 1.34],
        ],
    ];
    for path in paths {
        let points: Vec<_> = path.iter().map(|p| surface(p[0], p[1])).collect();
        let b=serde_json::from_value(json!({"type":"sweep","profile":profile(0.003,16),"points":points,"blend":0,"bone":"spine","material":"aged_brass","color":[0.55,0.38,0.18]})).unwrap();
        c.blobs.push(b);
    }
    for side in [-1.0, 1.0] {
        for y in [1.07, 1.15, 1.23] {
            let points: Vec<_> = [
                [side * 0.035, y + 0.035],
                [side * 0.10, y],
                [side * 0.15, y + 0.035],
            ]
            .iter()
            .map(|p| surface(p[0], p[1]))
            .collect();
            c.blobs.push(serde_json::from_value(json!({"type":"sweep","profile":profile(0.00325,16),"points":points,"material":"aged_brass","color":[0.55,0.38,0.18],"bone":"spine","blend":0})).unwrap());
        }
        for y in [1.02, 1.10, 1.18, 1.26, 1.34] {
            let center = surface(side * 0.145, y);
            c.blobs.push(serde_json::from_value(json!({"type":"sphere","center":center,"radius":0.0035,"blend":0,"bone":"spine","material":"aged_brass","color":[0.55,0.38,0.18]})).unwrap());
        }
    }
}
#[test]
#[ignore]
fn hard_film_checks() {
    let mut timings = String::from(
        "# Mesh timings\n\nSame fine grid: surface nets before, feature placement and crease normals after. Film device auto; 768 px, up to 64 samples.\n\n| Scene | Placement | Seconds | Vertices | Triangles |\n| --- | --- | ---: | ---: | ---: |\n",
    );
    let specs: Vec<Showcase> = serde_json::from_str(include_str!("hard_showcase.json")).unwrap();
    let phase = std::env::var("HARD_PHASE").ok();
    if let Some(phase) = &phase {
        assert!(phase == "before" || phase == "after");
    }
    if std::env::var("HARD_CHECK").is_ok() || phase.is_some() {
        let names: Vec<_> = specs
            .iter()
            .filter(|s| std::env::var("HARD_CHECK").map_or(true, |only| only == s.name))
            .flat_map(|s| {
                std::iter::once(s.name.as_str()).chain(s.macros.iter().map(|v| v.name.as_str()))
            })
            .collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hard_check/TIMINGS.md");
        if let Ok(previous) = std::fs::read_to_string(path) {
            timings = previous
                .lines()
                .filter(|line| {
                    !names.iter().any(|name| {
                        line.starts_with(&format!("| {name} |"))
                            && phase.as_ref().is_none_or(|phase| {
                                line.starts_with(&format!("| {name} | {phase} |"))
                            })
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
        }
    }
    for mut spec in specs {
        if std::env::var("HARD_CHECK").is_ok_and(|only| only != spec.name) {
            continue;
        }
        let rig = if spec.name == "breastplate" {
            let human = crate::human::Human::default();
            let body = human.build().unwrap();
            spec.blobs[0]["human"] = serde_json::to_value(human).unwrap();
            Some(Rig {
                bones: body.rig(),
                split: false,
                ..Default::default()
            })
        } else {
            None
        };
        let mut c = clay(spec.blobs.clone(), spec.cell);
        if spec.name == "pauldron" {
            decorate_pauldron(&mut c, false);
        }
        if spec.name == "breastplate" {
            decorate_pauldron(&mut c, true);
            decorate_breastplate(&mut c);
        }
        pair(&spec, c, rig, &mut timings);
    }
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hard_check/TIMINGS.md"),
        timings,
    )
    .unwrap();
}
