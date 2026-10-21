//! Picture and timing checks for adaptive detail (ignored tests, run by
//! hand; the GPU traces them when there is one). Pictures go to OUT
//! (default `detail_check/`), timings to stderr.
//!
//! - `cargo test --release detail_human -- --ignored --nocapture`: a whole
//!   `create_human` body in ONE clay with 0.25 mm face and hand regions and
//!   1 mm elsewhere: pores and wrinkles on the face, knuckle creases. The
//!   "before" pictures use one 1.1 mm grid (the old limit for a body).
//! - `SCENE=dragon.json cargo test --release detail_dragon -- --ignored --nocapture`:
//!   the dragon with finer scales over the whole body.
//! - `cargo test --release detail_wall -- --ignored --nocapture`: a kit
//!   stone wall close-up, shading relief only vs film displacement.
//!
//! SAMPLES sets samples per pixel (default 96).

use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4, Vec3};
use serde_json::json;

use crate::clay::Clay;
use crate::film::{Camera, Item, Look, Settings, look::Device};
use crate::material::Material;
use crate::sculpt::{self, Stroke};

fn out() -> String {
    std::env::var("OUT").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/detail_check").into())
}

fn save(name: &str, w: usize, h: usize, px: &[[u8; 3]]) {
    let dir = out();
    std::fs::create_dir_all(&dir).unwrap();
    image::save_buffer(format!("{dir}/{name}.png"), &px.iter().flatten().copied().collect::<Vec<_>>(), w as u32, h as u32, image::ColorType::Rgb8).unwrap();
}

fn samples() -> u32 {
    std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(96)
}

fn look() -> Arc<Look> {
    let mut look = Look::default();
    look.device = Device::Auto;
    Arc::new(look)
}

fn orbit(target: Vec3, yaw: f32, pitch: f32, distance: f32, fov: f32) -> Camera {
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let dir = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos());
    Camera { eye: target + dir * distance, target, up: Vec3::Y, fov_deg: fov, ortho: None }
}

/// Renders each view (diced for its own camera) and saves it; returns
/// seconds per view.
fn render(items: &[Item], views: &[(String, Camera, usize)]) -> Vec<f32> {
    let mut times = Vec::new();
    for (name, cam, size) in views {
        let t = Instant::now();
        let mut items: Vec<Item> = items.iter().map(|i| Item { mesh: i.mesh.clone(), world: i.world, tint: i.tint, surface: i.surface, material: i.material.clone() }).collect();
        let diced = crate::film::dice::displace(&mut items, &[*cam], *size);
        let meshes = crate::film::Meshes::new(items);
        let s = Settings { width: *size, height: *size, samples: samples(), bounces: 4, time_limit: Duration::from_secs(900), floor: true, exposure: 1.0, denoise: true };
        let img = crate::film::render_views_with_look(&meshes, &[], &[*cam], s, look(), &Default::default()).pop().unwrap();
        save(name, img.width, img.height, &img.pixels);
        let secs = t.elapsed().as_secs_f32();
        eprintln!("{name}: {} samples, {diced} diced triangles, {secs:.1}s", img.samples);
        times.push(secs);
    }
    times
}

fn stroke(v: serde_json::Value) -> Stroke {
    serde_json::from_value(v).unwrap()
}

/// The human, its sculpt strokes and where to look: (clay, face center,
/// hand centers, body center).
fn human(regions: bool) -> (Clay, Vec3, [Vec3; 2]) {
    let h = crate::human::Human::default();
    let body = h.build().unwrap();
    let blob = json!({ "type": "human", "name": "body", "part": "whole", "human": h, "color": [0.82, 0.64, 0.54], "blend": 0.0, "material": "skin" });
    let mut clay: Clay = serde_json::from_value(json!({ "blobs": [blob], "resolution": 384 })).unwrap();
    let detail = body.detail_regions(Some(0.00025), Some(0.00025));
    if regions {
        clay.set_detail_cell(0.001).unwrap();
        clay.set_detail_regions(&detail).unwrap();
    } else {
        // The finest one grid a whole body allowed before regions.
        clay.set_detail_cell(0.0011).unwrap();
    }
    let face = detail[0].center;
    let hands = [detail[1].center, detail[2].center];
    (clay, face, hands)
}

/// Pores, wrinkles and knuckle creases on the face and hands, and soft
/// skin bumps on the body.
fn sculpt_human(clay: &mut Clay, face: Vec3, hands: [Vec3; 2], body: &crate::human::Body) -> f32 {
    let t = Instant::now();
    let mut strokes = vec![
        // Body: soft unevenness at 1 mm.
        stroke(json!({ "brush": "noise", "points": [[0, 1.0, 0], [0, 1.35, 0]], "radius": 0.3, "strength": 0.25, "scale": 0.008, "pattern": "bumps" })),
        // Face: pores everywhere, forehead lines, crow's feet, smile lines.
        stroke(json!({ "brush": "noise", "points": [face.to_array()], "radius": 0.11, "strength": 0.35, "scale": 0.0009, "pattern": "pores" })),
        stroke(json!({ "brush": "noise", "points": [(face + Vec3::new(0.0, 0.055, 0.02)).to_array()], "radius": 0.04, "strength": 0.5, "scale": 0.005, "pattern": "wrinkles", "normal": [1, 0, 0] })),
    ];
    let [(l, _), (r, _)] = body.eyes();
    for eye in [l, r] {
        let side = eye.x.signum();
        strokes.push(stroke(json!({ "brush": "noise", "points": [(eye + Vec3::new(side * 0.03, -0.005, -0.005)).to_array()], "radius": 0.016, "strength": 0.5, "scale": 0.0025, "pattern": "wrinkles", "normal": [0, 0.3, 1] })));
    }
    for (side, hand) in ["l", "r"].iter().zip(hands) {
        strokes.push(stroke(json!({ "brush": "noise", "points": [hand.to_array()], "radius": 0.09, "strength": 0.3, "scale": 0.0008, "pattern": "pores" })));
        // Creases across each finger at its knuckles: wrinkles running
        // across the finger's direction, only around the joints.
        for f in 2..=5 {
            for k in 1..=3 {
                let (Some(a), Some(b)) = (body.marker(&format!("joint-{side}-finger-{f}-{k}")), body.marker(&format!("joint-{side}-finger-{f}-{}", k + 1))) else { continue };
                let along = (b - a).normalize_or(Vec3::NEG_Y);
                strokes.push(stroke(json!({ "brush": "noise", "points": [a.to_array()], "radius": 0.012, "strength": 0.7, "scale": 0.0016, "pattern": "wrinkles", "normal": along.cross(Vec3::Z).normalize_or(Vec3::X).to_array() })));
            }
        }
    }
    for s in &strokes {
        sculpt::apply(clay, s).unwrap();
    }
    t.elapsed().as_secs_f32()
}

#[test]
#[ignore]
fn detail_human() {
    let body = crate::human::Human::default().build().unwrap();
    let mut notes = Vec::new();
    let mut film = Vec::new();
    let only = std::env::var("ONLY").ok();
    for (label, regions) in [("before", false), ("after", true)] {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
        let (mut clay, face, hands) = human(regions);
        let sculpt_s = sculpt_human(&mut clay, face, hands, &body);
        let t = Instant::now();
        let mesh = clay.build_fine(&[], "skin").unwrap();
        let mesh_s = t.elapsed().as_secs_f32();
        let tris = mesh.indices.len() / 3;
        let line = format!("{label}: detail_cell {:?}, regions {}, bricks {}, sculpt {sculpt_s:.1}s, fine mesh {tris} triangles in {mesh_s:.1}s",
            clay.detail_cell, clay.detail_regions().len(), clay.detail.as_ref().map_or(0, |d| d.grid().total_bricks()));
        eprintln!("{line}");
        notes.push(line);
        let m = Material::resolve(&[], "skin");
        let items = vec![Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }];
        let hand = hands[0];
        let views = vec![
            (format!("human_{label}_full"), orbit(Vec3::new(0.0, 0.9, 0.0), 25.0, 5.0, 3.0, 40.0), 900),
            (format!("human_{label}_face"), orbit(face + Vec3::new(0.0, 0.01, 0.0), 30.0, 5.0, 0.32, 30.0), 900),
            (format!("human_{label}_hand"), orbit(hand, 70.0, 10.0, 0.22, 35.0), 900),
        ];
        film.push((label, render(&items, &views)));
    }
    for (label, t) in film {
        notes.push(format!("{label} film seconds (full, face, hand): {t:?}"));
    }
    let path = format!("{}/human_timings{}.txt", out(), only.map_or(String::new(), |o| format!("_{o}")));
    std::fs::write(path, notes.join("\n") + "\n").unwrap();
}

/// Bake timing (printed): the sculpted one-clay human at 4K and 8K, from
/// the fine mesh, and the exact field path at 4K for comparison
/// (EXACT=1). With SCENE=elf.json and BODY=Elf, that scene's body instead.
#[test]
#[ignore]
fn bake_timing() {
    let mut lines = Vec::new();
    let (clay, rest, library, fallback) = match (std::env::var("SCENE"), std::env::var("BODY")) {
        (Ok(path), Ok(body)) => {
            let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let o = scene.objects.iter().find(|o| o.name == body).expect("BODY names a clay object");
            let clay = o.clay.clone().unwrap();
            let fallback = o.material.clone().unwrap_or_default();
            let rest = clay.build_mesh_with(&scene.materials, &fallback);
            (clay, rest, scene.materials.clone(), fallback)
        }
        _ => {
            let body = crate::human::Human::default().build().unwrap();
            let (mut clay, face, hands) = human(true);
            sculpt_human(&mut clay, face, hands, &body);
            let rest = clay.build_mesh_with(&[], "skin");
            (clay, rest, Vec::new(), "skin".to_string())
        }
    };
    lines.push(format!("source: {} rest triangles", rest.indices.len() / 3));
    // Fresh: the bake refines the rest mesh itself. Cached: render_film or
    // a fine view already built the fine mesh, and the bake reads it.
    let cached = std::env::var("CACHED").is_ok().then(|| {
        let t = Instant::now();
        let fine = clay.build_high(&library, &fallback);
        lines.push(format!("fine mesh: {} triangles in {:.1}s (built by render_film or fine views; the bake reuses it)", fine.indices.len() / 3, t.elapsed().as_secs_f32()));
        fine
    });
    let mut runs = vec![(4096, false), (8192, false)];
    if std::env::var("EXACT").is_ok() {
        runs.push((4096, true));
    }
    for (texture, exact) in runs {
        let src = crate::bake::Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &library, fallback: &fallback, fine: cached.as_ref() };
        let settings = crate::bake::Settings { triangles: 60_000, texture, exact, ..Default::default() };
        let t = Instant::now();
        let (mesh, _) = crate::bake::bake(&src, settings, &Default::default()).unwrap();
        let line = format!("bake {texture}{}: {:.1}s, {} triangles", if exact { " exact (every texel from the field)" } else if cached.is_some() { " from the cached fine mesh" } else { " from scratch" }, t.elapsed().as_secs_f32(), mesh.indices.len() / 3);
        eprintln!("{line}");
        lines.push(line);
        if !exact && texture == 4096 && std::env::var("SCENE").is_err() {
            let tex = mesh.textures.as_ref().unwrap();
            save_rgba("bake_human_normal_4k", tex.size, &tex.normal);
            save_rgba("bake_human_color_4k", tex.size, &tex.color);
            // The 60k-triangle game mesh with its maps, at the face.
            let m = Material::resolve(&[], "skin");
            let items = vec![Item { mesh: Arc::new(mesh.clone()), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }];
            let face = crate::human::Human::default().build().unwrap().detail_regions(Some(0.00025), None)[0].center;
            render(&items, &[("bake_human_4k_face".to_string(), orbit(face + Vec3::new(0.0, 0.01, 0.0), 30.0, 5.0, 0.32, 30.0), 900)]);
        }
    }
    eprintln!("{}", lines.join("\n"));
    std::fs::write(format!("{}/bake_timings.txt", out()), lines.join("\n") + "\n").unwrap();
}

/// Saves a baked map at most 2048 across (a preview; the bake keeps its size).
fn save_rgba(name: &str, size: u32, rgba: &[u8]) {
    let dir = out();
    std::fs::create_dir_all(&dir).unwrap();
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let img = image::RgbImage::from_raw(size, size, rgb).unwrap();
    let side = size.min(2048);
    image::imageops::resize(&img, side, side, image::imageops::FilterType::Triangle).save(format!("{dir}/{name}.png")).unwrap();
}

/// The dragon (SCENE=dragon.json) before and after: detail regions over
/// its body, neck, tail and legs at a 2 mm cell (the body clay's grid is
/// 4.6 mm), with finer overlapping scales sculpted over them. Wings stay at
/// the coarse cell. Full view and a flank close-up each.
#[test]
#[ignore]
fn detail_dragon() {
    let path = std::env::var("SCENE").expect("SCENE=dragon.json");
    let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut notes = Vec::new();
    let only = std::env::var("ONLY").ok();
    for label in ["before", "after"] {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
        let mut scene = scene.clone();
        if label == "after" {
            let o = scene.objects.iter_mut().find(|o| o.name == "Dragon").expect("a Dragon clay");
            let clay = o.clay.as_mut().unwrap();
            let cell = 0.002;
            let region = |name: &str, c: [f32; 3], r: [f32; 3]| crate::clay::Region { name: name.into(), center: Vec3::from(c), radii: Vec3::from(r), cell };
            let mirror = |r: crate::clay::Region| vec![crate::clay::Region { name: format!("{}.R", r.name), center: Vec3::new(-r.center.x, r.center.y, r.center.z), ..r.clone() }, r];
            let mut regions = vec![
                region("body", [0.0, 1.35, -0.05], [0.62, 0.62, 1.3]),
                region("neck", [0.0, 2.0, 1.3], [0.36, 0.55, 0.62]),
                region("tail", [0.2, 0.95, -2.45], [0.5, 0.6, 1.55]),
            ];
            regions.extend(mirror(region("frontleg", [0.5, 0.62, 0.78], [0.26, 0.66, 0.32])));
            regions.extend(mirror(region("hindleg", [0.48, 0.72, -0.8], [0.32, 0.66, 0.42])));
            let t = Instant::now();
            clay.set_detail_regions(&regions).unwrap();
            let regions_s = t.elapsed().as_secs_f32();
            // Finer overlapping scales along the spine, down the flanks and
            // limbs, pointing toward the tail and down the legs.
            let strokes = [
                json!({ "brush": "noise", "points": [[0, 2.35, 1.7], [0, 2.0, 1.3], [0, 1.7, 0.6], [0, 1.6, -0.4], [0, 1.4, -1.3], [0, 1.0, -2.2], [0.25, 0.6, -3.1]], "radius": 0.75, "strength": 0.55, "scale": 0.014, "pattern": "scales", "normal": [0, -0.3, -1] }),
                json!({ "brush": "noise", "points": [[0.5, 1.2, 0.65], [0.5, 0.5, 0.78], [0.5, 0.1, 0.8]], "radius": 0.28, "strength": 0.55, "scale": 0.01, "pattern": "scales", "normal": [0, -1, 0], "mirror": true }),
                json!({ "brush": "noise", "points": [[0.45, 1.2, -0.7], [0.5, 0.6, -0.9], [0.5, 0.1, -0.75]], "radius": 0.34, "strength": 0.55, "scale": 0.01, "pattern": "scales", "normal": [0, -1, 0], "mirror": true }),
            ];
            let t = Instant::now();
            for s in strokes {
                let s = stroke(s);
                sculpt::apply(clay, &s).unwrap();
                if s.mirror {
                    sculpt::apply(clay, &s.mirrored()).unwrap();
                }
            }
            notes.push(format!("after: {} regions at {cell} in {regions_s:.1}s; scales sculpted in {:.1}s; {} detail bricks", clay.detail_regions().len(), t.elapsed().as_secs_f32(), clay.detail.as_ref().map_or(0, |d| d.grid().total_bricks())));
        }
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.show_fine = true;
        let t = Instant::now();
        meshes.sync(&scene);
        let eval = scene.evaluate();
        let items: Vec<Item> = scene.objects.iter().filter(|o| o.shape.is_mesh()).filter_map(|o| {
            let &(world, true) = eval.get(&o.id)? else { return None };
            Some(Item { mesh: meshes.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
        }).collect();
        let tris: usize = items.iter().map(|i| i.mesh.indices.len() / 3).sum();
        notes.push(format!("{label}: meshes {tris} triangles in {:.1}s; fine mesh errors {:?}", t.elapsed().as_secs_f32(), meshes.fine_errors()));
        let views = vec![
            (format!("dragon_{label}_full"), orbit(Vec3::new(0.0, 1.3, -0.3), 55.0, 18.0, 9.5, 40.0), 1000),
            (format!("dragon_{label}_flank"), orbit(Vec3::new(0.45, 1.25, -0.2), 80.0, 15.0, 0.9, 35.0), 900),
            (format!("dragon_{label}_leg"), orbit(Vec3::new(0.55, 0.45, 0.8), 60.0, 10.0, 0.75, 35.0), 900),
        ];
        let t = render(&items, &views);
        notes.push(format!("{label} film seconds (full, flank, leg): {t:?}"));
        // The sculpted shape alone: the same close-ups with the materials'
        // detail maps (their painted-on scale pattern) off.
        let plain = |m: &Material| Material { detail: Some("none".into()), ..m.clone() };
        let shapes: Vec<Item> = items.into_iter().map(|i| {
            let mesh = if i.mesh.materials.is_empty() { i.mesh } else { Arc::new(crate::mesh::Mesh { materials: i.mesh.materials.iter().map(plain).collect(), ..(*i.mesh).clone() }) };
            Item { mesh, material: plain(&i.material), ..i }
        }).collect();
        let views = vec![
            (format!("dragon_{label}_flank_shape"), orbit(Vec3::new(0.45, 1.25, -0.2), 80.0, 15.0, 0.9, 35.0), 900),
            (format!("dragon_{label}_leg_shape"), orbit(Vec3::new(0.55, 0.45, 0.8), 60.0, 10.0, 0.75, 35.0), 900),
        ];
        render(&shapes, &views);
        eprintln!("{}", notes.join("\n"));
    }
    std::fs::write(format!("{}/dragon_timings{}.txt", out(), only.map_or(String::new(), |o| format!("_{o}"))), notes.join("\n") + "\n").unwrap();
}

/// A kit stone wall close-up with chips: the stone's detail map as shading
/// relief only (before), then with `detail_displace` (after): real chips
/// and pits in the silhouette and shadows, diced for this camera.
#[test]
#[ignore]
fn detail_wall() {
    use crate::envkit::kit::{Kit, Piece, builtin};
    let kit = Kit { length: Some(2.4), damage: Some(0.6), seed: 7, ..Kit::new(Piece::Wall) };
    let mut notes = Vec::new();
    for (label, displace) in [("before", 0.0), ("after", 0.018)] {
        let library = vec![Material { detail_displace: displace, ..builtin("stone") }, Material { detail_displace: displace * 0.5, ..builtin("mortar") }];
        let mesh = Arc::new(kit.build(&library, None));
        let (lo, hi) = mesh.bounds();
        let m = Material::default();
        let items = vec![Item { mesh: mesh.clone(), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }];
        // Low and close along the face, toward the top corner, so the
        // stones' outline shows against the sky.
        let target = Vec3::new(hi.x - 0.35, hi.y - 0.3, hi.z);
        let views = vec![
            (format!("wall_{label}_closeup"), orbit(target, 35.0, 12.0, 0.9, 40.0), 900),
            (format!("wall_{label}_grazing"), orbit(Vec3::new(0.2, (lo.y + hi.y) * 0.5, hi.z), 78.0, 4.0, 1.2, 40.0), 900),
        ];
        let t = render(&items, &views);
        notes.push(format!("{label} (detail_displace {displace}): wall {} triangles; film seconds {t:?}", mesh.indices.len() / 3));
    }
    eprintln!("{}", notes.join("\n"));
    std::fs::write(format!("{}/wall_timings.txt", out()), notes.join("\n") + "\n").unwrap();
}
