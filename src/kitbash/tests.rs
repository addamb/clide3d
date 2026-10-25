//! Kitbash tests: every part builds and is deterministic; parts sit on
//! the surface; mirroring; bone attachment survives export; passes keep
//! their density and budgets; removing a group removes all it added.

use glam::{Mat4, Vec3};
use serde_json::{Value, json};

use super::place::{self, LIBRARY_TAG};
use super::surface::Surface;
use super::tool::{detail_pass, kitbash};
use super::{CATALOG, Kind};
use crate::mesh::MeshLib;
use crate::scene::{Scene, Shape};

/// A box target: a cube primitive scaled to `size`, at `at`.
fn boxy(scene: &mut Scene, name: &str, at: Vec3, size: Vec3) -> u64 {
    let id = scene.add(Shape::Cube);
    let o = scene.get_mut(id).unwrap();
    o.name = name.into();
    o.translation = at;
    o.scale = size;
    id
}

/// A rigged clay block (two bones up its middle), sharp edges.
fn rigged(scene: &mut Scene) -> u64 {
    let id = scene.add(Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.name = "Body".into();
    o.clay = Some(serde_json::from_value(json!({ "blobs": [{ "type": "box", "center": [0, 1, 0], "size": [0.8, 2.0, 0.5], "blend": 0 }], "resolution": 48 })).unwrap());
    o.rig = Some(serde_json::from_value(json!({ "bones": [
        { "name": "base", "head": [0, 0, 0], "tail": [0, 1, 0] },
        { "name": "top", "parent": "base", "head": [0, 1, 0], "tail": [0, 2, 0] },
    ], "split": false })).unwrap());
    id
}

fn instances(scene: &Scene, tag: &str) -> Vec<(Mat4, u64)> {
    let eval = scene.evaluate();
    let mut out = Vec::new();
    for o in scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag)) {
        let Some(sc) = &o.scatter else { continue };
        let w = eval[&o.id].0;
        for i in &sc.instances {
            out.push((w * i.matrix(), sc.sources[i.source as usize]));
        }
    }
    out
}

#[test]
fn every_part_and_style_builds_deterministically() {
    for info in CATALOG.iter() {
        for style in info.styles {
            let p = super::check::sample(info.kind, Some(style));
            p.validate().unwrap();
            let a = p.build(&[]);
            let b = p.build(&[]);
            assert!(!a.indices.is_empty(), "{} {style} is empty", info.name);
            assert_eq!(a.indices, b.indices, "{} {style}", info.name);
            assert!(a.vertices.iter().zip(&b.vertices).all(|(x, y)| x.pos == y.pos && x.normal == y.normal && x.uv == y.uv));
            assert!(a.vertices.iter().all(|v| Vec3::from(v.pos).is_finite() && Vec3::from(v.normal).is_normalized()), "{} {style}", info.name);
            // Game-ready budgets: small parts stay small.
            let tris = a.indices.len() / 3;
            let limit = match info.kind { Kind::Bolt | Kind::Nut | Kind::Washer | Kind::Clip => 1200, Kind::Net | Kind::Hose | Kind::CorrugatedTube | Kind::Spring | Kind::Cable => 30_000, _ => 12_000 };
            assert!(tris <= limit, "{} {style}: {tris} triangles", info.name);
            // UVs are set (meters / size), not all zero.
            assert!(a.vertices.iter().any(|v| v.uv != [0.0, 0.0]), "{} {style} has no UVs", info.name);
            // Most faces turn outward: from the part's center, normals point away more often than not.
            if !info.between_points && !matches!(info.kind, Kind::Net | Kind::Seam) {
                let (lo, hi) = a.bounds();
                let c = (lo + hi) * 0.5;
                let out = a.vertices.iter().filter(|v| (Vec3::from(v.pos) - c).dot(Vec3::from(v.normal)) > 0.0).count();
                assert!(out * 2 >= a.vertices.len(), "{} {style}: {out}/{} normals face out", info.name, a.vertices.len());
            }
            // Sits on y = 0 (the surface), nothing far below it.
            if !info.between_points {
                let (lo, _) = a.bounds();
                assert!(lo.y > -info.size[1].max(0.02) * 0.6, "{} {style} hangs below its base: {}", info.name, lo.y);
            }
        }
    }
}

#[test]
fn parts_sit_on_flat_and_curved_surfaces() {
    let mut scene = Scene::default();
    let b = boxy(&mut scene, "Block", Vec3::new(0.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 2.0));
    let s = scene.add(Shape::Sphere);
    {
        let o = scene.get_mut(s).unwrap();
        o.name = "Ball".into();
        o.translation = Vec3::new(4.0, 1.0, 0.0);
        o.scale = Vec3::splat(2.0);
    }
    let mut meshes = MeshLib::new();
    for (target, at, part) in [
        (b, [0.3, 2.2, 0.2], "vent"),
        (b, [0.0, 1.2, 1.3], "light"),
        (s, [4.0, 2.2, 0.0], "bolt"),
        (s, [4.3, 1.7, 0.5], "sensor"),
        (s, [3.2, 1.2, 0.4], "greeble"),
    ] {
        let r = kitbash(&mut scene, &mut meshes, &json!({ "part": part, "target": target, "at": at })).unwrap();
        let tag = r["group"].as_str().unwrap().to_string();
        meshes.sync(&scene);
        let surf = Surface::new(place::owners(&scene, &meshes, &[target]));
        let copies = instances(&scene, &tag);
        assert_eq!(copies.len(), 1, "{r}");
        for (m, src) in copies {
            let part = scene.get(src).unwrap().part.clone().unwrap();
            let (hx, hz, h) = part.footprint().unwrap();
            let n = m.transform_vector3(Vec3::Y).normalize();
            let tol = 1e-3 + h * 0.01;
            // No footprint corner floats.
            for (x, z) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (0.0, 0.0)] {
                let q = m.transform_point3(Vec3::new(x * hx, 0.0, z * hz));
                let hit = surf.ray(q + n * h * 3.0, -n, h * 6.0).expect("surface under the part");
                let gap = (q - hit.p).dot(n);
                assert!(gap <= tol, "{part:?} floats {gap} at ({x},{z})");
            }
            // The base center is on the surface or sunk only as reported.
            let q = m.transform_point3(Vec3::ZERO);
            let hit = surf.ray(q + n * h * 3.0, -n, h * 6.0).unwrap();
            let sink = (hit.p - q).dot(n);
            let reported = r["max_sink"].as_f64().unwrap() as f32;
            assert!(sink >= -tol && sink <= reported + tol, "{} sinks {sink} (reported {reported})", part.kind.name());
            if target == b {
                assert!(sink.abs() < tol, "flat faces need no sinking: {sink}");
            }
        }
    }
}

#[test]
fn mirror_lands_on_the_other_side() {
    let mut scene = Scene::default();
    let l = boxy(&mut scene, "Leg.L", Vec3::new(1.0, 1.0, 0.0), Vec3::splat(0.8));
    let r = boxy(&mut scene, "Leg.R", Vec3::new(-1.0, 1.0, 0.0), Vec3::splat(0.8));
    let mut meshes = MeshLib::new();
    let out = kitbash(&mut scene, &mut meshes, &json!({ "part": "hatch", "target": "Leg.L", "at": [1.1, 1.1, 0.45], "mirror": true, "size": [0.3, 0.02, 0.4] })).unwrap();
    assert_eq!(out["mirrored"], 1, "{out}");
    let tag = out["group"].as_str().unwrap();
    let copies = instances(&scene, tag);
    assert_eq!(copies.len(), 2);
    let (a, b) = (copies[0].0, copies[1].0);
    let (pa, pb) = (a.transform_point3(Vec3::ZERO), b.transform_point3(Vec3::ZERO));
    assert!((pa.x + pb.x).abs() < 1e-3 && (pa.y - pb.y).abs() < 1e-3 && (pa.z - pb.z).abs() < 1e-3, "{pa} {pb}");
    // A true mirror image, on the other leg.
    assert!(a.determinant() * b.determinant() < 0.0);
    let groups: Vec<&crate::scene::Object> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag) && o.shape == Shape::Group).collect();
    let parents: Vec<Option<u64>> = groups.iter().map(|g| g.parent).collect();
    assert!(parents.contains(&Some(l)) && parents.contains(&Some(r)), "{parents:?}");
    // Point parts mirror their points.
    let out = kitbash(&mut scene, &mut meshes, &json!({ "part": "hose", "target": "Leg.L", "from": [1.0, 1.3, 0.42], "to": [1.0, 0.7, 0.42], "mirror": true })).unwrap();
    assert_eq!(out["mirrored"], 1, "{out}");
    let hoses: Vec<_> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == out["group"].as_str()).filter_map(|o| o.part.as_ref()).collect();
    assert_eq!(hoses.len(), 2);
}

#[test]
fn parts_ride_bones_and_export_under_the_joint() {
    let mut scene = Scene::default();
    let body = rigged(&mut scene);
    let mut meshes = MeshLib::new();
    // Low on the block (base bone) and high (top bone).
    let low = kitbash(&mut scene, &mut meshes, &json!({ "part": "bolt", "target": body, "at": [0.1, 0.4, 0.3], "array": { "count": 3, "step": [0.1, 0, 0] } })).unwrap();
    let high = kitbash(&mut scene, &mut meshes, &json!({ "part": "vent", "target": body, "at": [0.0, 1.7, 0.3] })).unwrap();
    assert_eq!(low["attached"][0]["bone"], "base", "{low}");
    assert_eq!(high["attached"][0]["bone"], "top", "{high}");
    assert_eq!(low["copies"], 3);
    assert_eq!(low["sources"].as_array().unwrap().len(), 1, "three bolts, one mesh");
    // A part on an object that rides a bone rides the same bone.
    let plate = boxy(&mut scene, "Plate", Vec3::new(0.0, 1.5, 0.3), Vec3::new(0.5, 0.4, 0.1));
    {
        let o = scene.get_mut(plate).unwrap();
        o.parent = Some(body);
        o.bone = Some("top".into());
    }
    let on_plate = kitbash(&mut scene, &mut meshes, &json!({ "part": "light", "target": "Plate", "at": [0.0, 1.5, 0.4] })).unwrap();
    assert_eq!(on_plate["attached"][0]["bone"], "top", "{on_plate}");
    meshes.sync(&scene);
    let opts = crate::gltf::Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
    let out = crate::gltf::export(&scene, &meshes, &opts).unwrap();
    let doc = glb_json(&out.bytes);
    let nodes = doc["nodes"].as_array().unwrap();
    let parent_of = |i: usize| nodes.iter().position(|n| n["children"].as_array().is_some_and(|c| c.iter().any(|x| x.as_u64() == Some(i as u64))));
    let under = |i: usize, joint: &str| {
        let mut cur = parent_of(i);
        while let Some(p) = cur {
            if nodes[p]["name"] == joint {
                return true;
            }
            cur = parent_of(p);
        }
        false
    };
    let bolt_nodes: Vec<usize> = nodes.iter().enumerate().filter(|(_, n)| n["mesh"].is_u64() && n["name"].as_str().is_some_and(|s| s.starts_with("kb_bolt"))).map(|(i, _)| i).collect();
    assert_eq!(bolt_nodes.len(), 3, "copies export as nodes");
    assert!(bolt_nodes.iter().all(|&i| under(i, "base")));
    let mesh_ids: std::collections::HashSet<u64> = bolt_nodes.iter().filter_map(|&i| nodes[i]["mesh"].as_u64()).collect();
    assert_eq!(mesh_ids.len(), 1, "instanced: one mesh for every bolt");
    let bolt_mesh = &doc["meshes"][*mesh_ids.iter().next().unwrap() as usize];
    assert!(bolt_mesh["primitives"][0]["attributes"]["TEXCOORD_0"].is_u64(), "parts keep their UVs");
    let vent = nodes.iter().position(|n| n["mesh"].is_u64() && n["name"].as_str().is_some_and(|s| s.starts_with("kb_vent"))).unwrap();
    assert!(under(vent, "top"));
    let light = nodes.iter().position(|n| n["mesh"].is_u64() && n["name"].as_str().is_some_and(|s| s.starts_with("kb_light"))).unwrap();
    assert!(under(light, "top"));
    // FBX writes them too (one geometry per source).
    let fbx = crate::fbx::export(&scene, &meshes, &opts).unwrap();
    assert!(fbx.bytes.len() > 1000);
    // Posed, the bolts follow the base bone and the vent the top bone.
    let r = scene.get_mut(body).unwrap().rig.as_mut().unwrap();
    let mut clip = crate::rig::Clip::new("bend");
    let mut k = crate::rig::Key::new(10.0);
    k.rotate = Vec3::new(0.0, 0.0, 50.0);
    clip.track_mut("top").insert(crate::rig::Key::new(1.0));
    clip.track_mut("top").insert(k);
    r.clips.push(clip);
    r.active = Some("bend".into());
    let before = instances(&scene, high["group"].as_str().unwrap())[0].0.transform_point3(Vec3::ZERO);
    scene.frame = Some(7.0);
    let after = instances(&scene, high["group"].as_str().unwrap())[0].0.transform_point3(Vec3::ZERO);
    let bolts_after = instances(&scene, low["group"].as_str().unwrap())[0].0.transform_point3(Vec3::ZERO);
    scene.frame = None;
    let bolts_before = instances(&scene, low["group"].as_str().unwrap())[0].0.transform_point3(Vec3::ZERO);
    assert!(before.distance(after) > 0.1, "the vent moves with the top bone");
    assert!(bolts_before.distance(bolts_after) < 1e-4, "the bolts stay with the base bone");
}

fn glb_json(bytes: &[u8]) -> Value {
    let jlen = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&bytes[20..20 + jlen]).unwrap()
}

#[test]
fn passes_are_deterministic_and_respect_density_and_budget() {
    let build = || {
        let mut scene = Scene::default();
        boxy(&mut scene, "Hull", Vec3::new(0.0, 1.5, 0.0), Vec3::new(4.0, 3.0, 2.5));
        scene
    };
    let mut meshes = MeshLib::new();
    let run = |scene: &mut Scene, meshes: &mut MeshLib, args: Value| detail_pass(scene, meshes, &args).unwrap();
    let (mut a, mut b) = (build(), build());
    for scene in [&mut a, &mut b] {
        run(scene, &mut meshes, json!({ "pass": "panel", "target": "Hull", "seed": 3 }));
        run(scene, &mut meshes, json!({ "pass": "bolt", "target": "Hull", "seed": 3 }));
        run(scene, &mut meshes, json!({ "pass": "greeble", "target": "Hull", "seed": 3, "density": 1.0 }));
    }
    assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap(), "same seed, same parts");
    // Density: greebles per square meter of eligible face (at this asset
    // size), never more than asked.
    let mut c = build();
    let g = run(&mut c, &mut meshes, json!({ "pass": "greeble", "target": "Hull", "seed": 1, "density": 2.0, "facing": [0, 0, 1], "max_angle": 10 }));
    let unit = g["unit"].as_f64().unwrap() as f32;
    let wanted = g["wanted"].as_u64().unwrap() as usize;
    let face = 4.0 * 3.0;
    assert_eq!(wanted, (face * 2.0 / (unit * unit)).round() as usize, "{g}");
    let placed = g["placed"].as_u64().unwrap() as usize;
    assert!(placed <= wanted && placed * 2 >= wanted, "{g}");
    // Only on the front face, none overlapping.
    let copies = instances(&c, g["group"].as_str().unwrap());
    meshes.sync(&c);
    let mut discs = Vec::new();
    for (m, src) in &copies {
        let p = m.transform_point3(Vec3::ZERO);
        assert!((p.z - 1.25).abs() < 1e-3, "{p}");
        let (hx, hz, _) = c.get(*src).unwrap().part.as_ref().unwrap().footprint().unwrap();
        discs.push((p, m.transform_vector3(Vec3::new(hx, 0.0, hz)).length() * 0.7));
    }
    for (i, (p, r)) in discs.iter().enumerate() {
        for (q, s) in &discs[i + 1..] {
            assert!(p.distance(*q) > (r + s) * 0.9, "greebles overlap");
        }
    }
    // Budget: the triangles drawn stay under it.
    let mut d = build();
    let full = run(&mut d, &mut meshes, json!({ "pass": "bolt", "target": "Hull", "seed": 2, "spacing": 0.12 }));
    let all = full["triangles_drawn"].as_u64().unwrap();
    let mut e = build();
    let capped = run(&mut e, &mut meshes, json!({ "pass": "bolt", "target": "Hull", "seed": 2, "spacing": 0.12, "budget": all / 3 }));
    assert!(capped["triangles_drawn"].as_u64().unwrap() <= all / 3, "{capped}");
    assert!(capped["dropped_for_budget"].as_u64().unwrap() > 0);
    // Bolts keep their spacing.
    let pts: Vec<Vec3> = instances(&d, full["group"].as_str().unwrap()).iter().map(|(m, _)| m.transform_point3(Vec3::ZERO)).collect();
    assert!(pts.len() > 20, "{full}");
    for (i, p) in pts.iter().enumerate() {
        for q in &pts[i + 1..] {
            assert!(p.distance(*q) > 0.12 * 0.65, "bolts too close: {p} {q}");
        }
    }
}

#[test]
fn panels_hatches_and_cables_on_a_rigged_body() {
    let mut scene = Scene::default();
    let body = rigged(&mut scene);
    let mut meshes = MeshLib::new();
    let p = detail_pass(&mut scene, &mut meshes, &json!({ "pass": "panel", "target": body, "seed": 4, "hatches": 1, "scale": 0.4 })).unwrap();
    assert!(p["panels"].as_u64().unwrap() >= 2, "{p}");
    let bones: Vec<String> = p["attached"].as_array().unwrap().iter().map(|a| a["bone"].as_str().unwrap_or("").to_string()).collect();
    assert!(bones.contains(&"base".to_string()) && bones.contains(&"top".to_string()), "{p}");
    // Plates lie flat on the faces (no gap or sink).
    meshes.sync(&scene);
    let surf = Surface::new(place::owners(&scene, &meshes, &[body]));
    for (m, src) in instances(&scene, p["group"].as_str().unwrap()) {
        let part = scene.get(src).unwrap().part.clone().unwrap();
        if part.kind != Kind::ArmorPlate {
            continue;
        }
        let n = m.transform_vector3(Vec3::Y).normalize();
        let (hx, hz, _) = part.footprint().unwrap();
        for (x, z) in [(-1.0, -1.0), (1.0, 1.0), (0.0, 0.0)] {
            let q = m.transform_point3(Vec3::new(x * hx, 0.0, z * hz));
            let h = surf.ray(q + n * 0.05, -n, 0.1).unwrap();
            assert!((q - h.p).dot(n).abs() < 2e-3, "plate off its face by {}", (q - h.p).dot(n));
        }
    }
    // A cable across the joint between two points.
    let c = detail_pass(&mut scene, &mut meshes, &json!({ "pass": "cable", "target": body, "routes": [{ "from": [0.4, 0.6, 0.1], "to": [0.4, 1.5, 0.1], "side": "outer" }], "count": 1 })).unwrap();
    assert_eq!(c["made"].as_array().unwrap().len(), 1, "{c}");
    let cable = scene.objects.iter().find(|o| o.kitbash.as_deref() == c["group"].as_str() && o.part.as_ref().is_some_and(|p| p.kind.info().between_points)).unwrap();
    meshes.sync(&scene);
    assert!(meshes.get(cable).is_some_and(|m| m.indices.len() > 100));
}

#[test]
fn removing_a_group_removes_everything_it_added() {
    let mut scene = Scene::default();
    let body = rigged(&mut scene);
    boxy(&mut scene, "Crate", Vec3::new(2.0, 0.5, 0.0), Vec3::new(1.0, 1.0, 1.0));
    let mut meshes = MeshLib::new();
    let before = serde_json::to_value(&scene).unwrap()["objects"].clone();
    let g = detail_pass(&mut scene, &mut meshes, &json!({ "pass": "greeble", "targets": ["Crate"], "density": 3.0, "seed": 9 })).unwrap();
    let e = detail_pass(&mut scene, &mut meshes, &json!({ "pass": "panel", "target": body, "style": "engrave", "scale": 0.4 })).unwrap();
    assert!(e["engraved_seams"].as_u64().unwrap() > 0, "{e}");
    let k = kitbash(&mut scene, &mut meshes, &json!({ "part": "antenna", "target": "Crate", "at": [2.2, 1.0, 0.1] })).unwrap();
    assert!(scene.objects.iter().any(|o| o.kitbash.as_deref() == Some(LIBRARY_TAG)));
    let listed = kitbash(&mut scene, &mut meshes, &json!({ "action": "list" })).unwrap();
    assert_eq!(listed["groups"].as_object().unwrap().len(), 3, "{listed}");
    for r in [&g, &e, &k] {
        kitbash(&mut scene, &mut meshes, &json!({ "action": "remove", "group": r["group"] })).unwrap();
    }
    assert_eq!(serde_json::to_value(&scene).unwrap()["objects"], before, "the scene is back as it was (no parts, seams or library left)");
    assert!(kitbash(&mut scene, &mut meshes, &json!({ "action": "remove", "group": "nope" })).is_err());
}

#[test]
fn sizes_follow_the_asset() {
    let mut meshes = MeshLib::new();
    let mut bolt_size = |height: f32| {
        let mut scene = Scene::default();
        boxy(&mut scene, "Thing", Vec3::new(0.0, height * 0.5, 0.0), Vec3::new(height * 0.3, height, height * 0.3));
        let r = kitbash(&mut scene, &mut meshes, &json!({ "part": "bolt", "target": "Thing", "at": [0.0, height * 0.5, height * 0.15] })).unwrap();
        r["part"]["size"][0].as_f64().unwrap()
    };
    let mech = bolt_size(6.5);
    let rifle = bolt_size(1.0);
    assert!((mech - 0.045).abs() < 1e-4, "{mech}");
    assert!((rifle / mech - 1.0 / 6.5).abs() < 1e-3, "{rifle}");
}



#[test]
fn absurd_settings_stay_bounded() {
    let mut scene = Scene::default();
    boxy(&mut scene, "Hull", Vec3::new(0.0, 1.5, 0.0), Vec3::new(4.0, 3.0, 2.5));
    let mut meshes = MeshLib::new();
    let t = std::time::Instant::now();
    let r = detail_pass(&mut scene, &mut meshes, &json!({ "pass": "bolt", "target": "Hull", "spacing": 1e-9, "size": [1e-9] })).unwrap();
    assert!(r["placed"].as_u64().unwrap() as usize <= super::passes::MAX_PLACEMENTS, "{r}");
    assert!(detail_pass(&mut scene, &mut meshes, &json!({ "pass": "greeble", "target": "Hull", "density": f32::MAX })).is_ok());
    assert!(detail_pass(&mut scene, &mut meshes, &json!({ "pass": "greeble", "target": "Hull", "area": { "center": [0, 0, 0], "radius": -1 } })).is_err());
    assert!(t.elapsed().as_secs() < 60);
    let mut p = super::Part::new(Kind::Hinge);
    p.style = Some("piano".into());
    p.size = Some([50.0, 0.0005, 0.1]);
    assert!(p.build(&[]).indices.len() / 3 < 200_000);
}
