//! Picture check for the environment kit (ignored test, run by hand):
//! `OUT=env_check cargo test --release env_check -- --ignored --nocapture`
//! builds an 8 x 8 m crypt room from kit pieces, scatters rocks, bones and
//! candles over its floor, and renders it from an iso camera with torches,
//! a brazier and mist. SAMPLES (default 128) and SIZE (960) set quality;
//! the scene is saved next to the picture as `crypt.json`.

use std::sync::Arc;
use std::time::Duration;

use glam::{Quat, Vec2, Vec3};
use serde_json::json;

use super::kit::{Kit, Piece};
use super::scatter::{self, Ground, Scatter, Settings};
use crate::film::{Camera, Item, Look, Meshes};
use crate::mesh::MeshLib;
use crate::scene::{Scene, Shape};

fn piece(scene: &mut Scene, name: &str, kit: Kit, at: Vec3, yaw: f32) -> u64 {
    let id = scene.add(Shape::Kit);
    let o = scene.get_mut(id).unwrap();
    o.name = name.to_string();
    o.light = kit.piece.light();
    o.kit = Some(kit);
    o.translation = at;
    o.rotation = Quat::from_rotation_y(yaw.to_radians());
    id
}

/// The crypt: floor, walls (low and broken toward the camera), an arched
/// doorway, corners, two pillars, rubble, torches, a brazier; then rocks,
/// bones and candles scattered over the floor.
pub fn crypt() -> Scene {
    let mut s = Scene::default();
    let k = |p: Piece| Kit::new(p);
    piece(&mut s, "floor", Kit { length: Some(8.0), depth: Some(8.0), seed: 1, damage: Some(0.35), ..k(Piece::Floor) }, Vec3::ZERO, 0.0);
    // Back walls, full height: north (along X at z = -4) and west.
    piece(&mut s, "wall_n1", Kit { length: Some(2.0), seed: 11, ..k(Piece::Wall) }, Vec3::new(-3.0, 0.0, -4.0), 0.0);
    piece(&mut s, "doorway", Kit { length: Some(4.0), opening: Some(1.6), opening_height: Some(1.7), seed: 12, ..k(Piece::Doorway) }, Vec3::new(0.0, 0.0, -4.0), 0.0);
    piece(&mut s, "wall_n2", Kit { length: Some(2.0), seed: 13, ..k(Piece::Wall) }, Vec3::new(3.0, 0.0, -4.0), 0.0);
    for (i, z) in [-3.0, -1.0, 1.0, 3.0].into_iter().enumerate() {
        piece(&mut s, &format!("wall_w{i}"), Kit { length: Some(2.0), seed: 20 + i as u64, damage: Some(0.2 + 0.1 * i as f32), ..k(Piece::Wall) }, Vec3::new(-4.0, 0.0, z), 90.0);
    }
    // Near walls, broken low so the camera sees in.
    for (i, x) in [-3.0, -1.0, 1.0, 3.0].into_iter().enumerate() {
        piece(&mut s, &format!("wall_s{i}"), Kit { length: Some(2.0), height: Some(0.5 + 0.35 * ((i * 7) % 3) as f32), damage: Some(0.8), seed: 30 + i as u64, ..k(Piece::Wall) }, Vec3::new(x, 0.0, 4.0), 0.0);
        piece(&mut s, &format!("wall_e{i}"), Kit { length: Some(2.0), height: Some(0.4 + 0.3 * ((i * 5) % 3) as f32), damage: Some(0.8), seed: 40 + i as u64, ..k(Piece::Wall) }, Vec3::new(4.0, 0.0, -x), 90.0);
    }
    // A corridor beyond the doorway, lit from far down.
    piece(&mut s, "corridor_floor", Kit { length: Some(2.0), depth: Some(3.0), seed: 14, ..k(Piece::Floor) }, Vec3::new(0.0, 0.0, -5.5), 0.0);
    piece(&mut s, "corridor_l", Kit { length: Some(3.0), seed: 15, ..k(Piece::Wall) }, Vec3::new(-1.25, 0.0, -5.75), 90.0);
    piece(&mut s, "corridor_r", Kit { length: Some(3.0), seed: 16, ..k(Piece::Wall) }, Vec3::new(1.25, 0.0, -5.75), 90.0);
    piece(&mut s, "corridor_torch", Kit { seed: 17, ..k(Piece::Torch) }, Vec3::new(-1.0, 1.8, -6.3), 90.0);
    piece(&mut s, "corner_nw", Kit { seed: 50, ..k(Piece::Corner) }, Vec3::new(-4.0, 0.0, -4.0), 0.0);
    piece(&mut s, "corner_ne", Kit { seed: 51, ..k(Piece::Corner) }, Vec3::new(4.0, 0.0, -4.0), -90.0);
    piece(&mut s, "corner_sw", Kit { seed: 52, height: Some(1.6), damage: Some(0.6), ..k(Piece::Corner) }, Vec3::new(-4.0, 0.0, 4.0), 90.0);
    piece(&mut s, "corner_se", Kit { seed: 53, height: Some(0.9), damage: Some(0.7), ..k(Piece::Corner) }, Vec3::new(4.0, 0.0, 4.0), 180.0);
    piece(&mut s, "pillar_l", Kit { seed: 60, ..k(Piece::Pillar) }, Vec3::new(-1.8, 0.0, -1.2), 0.0);
    piece(&mut s, "pillar_r", Kit { seed: 61, damage: Some(0.45), ..k(Piece::Pillar) }, Vec3::new(1.8, 0.0, -1.2), 0.0);
    piece(&mut s, "rubble", Kit { seed: 70, width: Some(1.8), count: Some(45), ..k(Piece::Rubble) }, Vec3::new(-2.9, 0.0, -2.7), 0.0);
    piece(&mut s, "rubble_small", Kit { seed: 71, width: Some(1.1), height: Some(0.3), count: Some(22), ..k(Piece::Rubble) }, Vec3::new(2.7, 0.0, 2.6), 0.0);
    piece(&mut s, "torch_l", Kit { seed: 80, ..k(Piece::Torch) }, Vec3::new(-1.9, 1.9, -3.75), 0.0);
    piece(&mut s, "torch_r", Kit { seed: 81, ..k(Piece::Torch) }, Vec3::new(1.9, 1.9, -3.75), 0.0);
    piece(&mut s, "torch_w", Kit { seed: 82, ..k(Piece::Torch) }, Vec3::new(-3.75, 1.9, 0.0), 90.0);
    piece(&mut s, "brazier", Kit { seed: 90, ..k(Piece::Brazier) }, Vec3::new(0.3, 0.0, 0.9), 0.0);
    // Sources for scattering, hidden (their copies show).
    let sources = |s: &mut Scene, name: &str, kit: Kit| {
        let id = piece(s, name, kit, Vec3::new(0.0, -20.0, 0.0), 0.0);
        s.get_mut(id).unwrap().visible = false;
        id
    };
    let rock_a = sources(&mut s, "src_rock_a", Kit { seed: 100, size: Some([0.32, 0.2, 0.26]), ..k(Piece::Rock) });
    let rock_b = sources(&mut s, "src_rock_b", Kit { seed: 101, size: Some([0.2, 0.14, 0.18]), facets: Some(9), ..k(Piece::Rock) });
    let femur = sources(&mut s, "src_femur", Kit { seed: 102, ..k(Piece::Bones) });
    let skull = sources(&mut s, "src_skull", Kit { seed: 103, kind: Some("skull".into()), ..k(Piece::Bones) });
    let ribs = sources(&mut s, "src_ribs", Kit { seed: 104, kind: Some("ribs".into()), ..k(Piece::Bones) });
    let candle = sources(&mut s, "src_candle", Kit { seed: 105, ..k(Piece::Candle) });
    let candles = sources(&mut s, "src_candles", Kit { seed: 106, count: Some(3), ..k(Piece::Candle) });
    // Drop copies onto what is built so far.
    let mut lib = MeshLib::new();
    lib.sync(&s);
    let eval = s.evaluate();
    let mut tris = Vec::new();
    for d in scatter::draws(&s, &eval) {
        if !matches!(d.object.kit.as_ref().map(|k| k.piece), Some(Piece::Floor | Piece::Rubble)) {
            continue;
        }
        let Some(m) = lib.get(d.object) else { continue };
        for t in m.indices.chunks_exact(3) {
            tris.push([0, 1, 2].map(|i| d.world.transform_point3(Vec3::from(m.vertices[t[i] as usize].pos))));
        }
    }
    let ground = Ground::new(tris);
    let scatter = |s: &mut Scene, name: &str, src: Vec<u64>, set: Settings| {
        let placed = scatter::place(&set, src.len(), &|p: Vec2| ground.top(p));
        let id = s.add(Shape::Scatter);
        let o = s.get_mut(id).unwrap();
        o.name = name.into();
        o.scatter = Some(Scatter { sources: src, instances: placed.instances, settings: set });
    };
    let clear = vec![[0.3, 0.9, 1.2], [0.0, -3.0, 1.0], [-1.8, -1.2, 0.5], [1.8, -1.2, 0.5]];
    scatter(&mut s, "rocks", vec![rock_a, rock_b], Settings { size: Some([7.4, 7.4]), density: 1.6, min_distance: 0.28, scale: [0.5, 1.4], align: 0.6, sink: 0.02, avoid: clear.clone(), seed: 1, ..Settings::default() });
    scatter(&mut s, "bones", vec![femur, skull, ribs], Settings { center: [-0.5, 0.8], size: Some([6.5, 5.5]), density: 0.9, min_distance: 0.32, scale: [0.85, 1.15], tilt: 8.0, align: 0.8, weights: vec![3.0, 1.0, 1.0], avoid: clear.clone(), seed: 2, ..Settings::default() });
    scatter(&mut s, "candles", vec![candle, candles], Settings { center: [-0.4, -0.4], size: Some([6.8, 6.8]), count: Some(18), density: 2.0, min_distance: 0.6, scale: [0.8, 1.3], avoid: vec![[0.3, 0.9, 1.4], [-1.8, -1.2, 0.5], [1.8, -1.2, 0.5], [0.0, 0.0, 2.2]], seed: 3, ..Settings::default() });
    s
}

#[test]
#[ignore]
fn env_check() {
    let out = std::env::var("OUT").unwrap_or_else(|_| "env_check".into());
    std::fs::create_dir_all(&out).unwrap();
    let samples = std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(128);
    let size = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(960);
    let t0 = std::time::Instant::now();
    let scene = crypt();
    std::fs::write(format!("{out}/crypt.json"), serde_json::to_string(&scene).unwrap()).unwrap();
    let mut lib = MeshLib::new();
    lib.sync(&scene);
    eprintln!("built in {:.1}s", t0.elapsed().as_secs_f32());
    if std::env::var("COUNT").is_ok() {
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Kit) {
            eprintln!("{:>14} {:>8} tris", o.name, lib.get(o).map_or(0, |m| m.indices.len() / 3));
        }
    }
    let eval = scene.evaluate();
    let draws = scatter::draws(&scene, &eval);
    let items: Vec<Item> = draws.iter().filter(|d| d.object.shape.is_mesh()).filter_map(|d| {
        Some(Item { mesh: lib.get(d.object)?.clone(), world: d.world, tint: d.object.color, surface: scene.surface_of(d.object), material: scene.material_of(d.object) })
    }).collect();
    let tris: usize = items.iter().map(|i| i.mesh.indices.len() / 3).sum();
    let args = std::env::var("LOOK").ok().map(|s| serde_json::from_str(&s).unwrap()).unwrap_or_else(|| json!({
        "environment": "night", "lights": "none", "background": "environment", "tonemap": "filmic", "bloom": {"strength": 0.12, "threshold": 1.5},
        "vignette": 0.35, "exposure": 1.6, "fog": {"density": 0.09, "height": 0.4, "falloff": 1.2, "anisotropy": 0.45, "color": [0.85, 0.85, 0.9]},
    }));
    let mut look = Look::parse(&args).unwrap();
    for d in &draws {
        if let Some(l) = &d.object.light {
            let (position, power, radius) = l.emit(d.world, d.copy, 0.0);
            look.lights.push(crate::film::lighting::Light::Sphere { position, radius, power });
        }
    }
    eprintln!("{} items, {tris} triangles, {} lights", items.len(), look.lights.len());
    let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d);
    let target = Vec3::new(env("TX", 0.0), env("TY", 0.5), env("TZ", -0.4));
    let (yaw, pitch, fov) = (env("YAW", 45.0).to_radians(), env("PITCH", 50.0).to_radians(), env("FOV", 26.0));
    let dir = Vec3::new(pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos());
    let distance = env("RADIUS", 5.0) / (fov * 0.5).to_radians().sin();
    let cam = Camera { eye: target + dir * distance, target, up: Vec3::Y, fov_deg: fov, ortho: None };
    let settings = crate::film::Settings { width: size, height: size * 9 / 16, samples, bounces: 4, time_limit: Duration::from_secs(1200), floor: false, exposure: 1.0, denoise: true };
    let meshes = Meshes::new(items);
    let t = std::time::Instant::now();
    let img = crate::film::render_views_with_look(&meshes, &[], &[cam], settings, Arc::new(look), &Default::default()).pop().unwrap();
    eprintln!("{} samples in {:.1}s", img.samples, t.elapsed().as_secs_f32());
    let name = std::env::var("NAME").unwrap_or_else(|_| "crypt".into());
    image::save_buffer(format!("{out}/{name}.png"), &img.pixels.iter().flatten().copied().collect::<Vec<_>>(), img.width as u32, img.height as u32, image::ColorType::Rgb8).unwrap();
}
