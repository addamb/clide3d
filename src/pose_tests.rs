//! Tests for mirroring, aligning, posing and things that follow (see
//! `pose.rs`), two-bone kitbash parts (`kitbash/span.rs`) and path arrays.

use glam::{Mat3, Mat4, Quat, Vec3};
use serde_json::json;

use crate::mesh::MeshLib;
use crate::pose::{self, MirrorOptions};
use crate::scene::{Scene, Shape};

fn clay(scene: &mut Scene, name: &str, blobs: serde_json::Value, res: u32) -> u64 {
    let id = scene.add(Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.name = name.into();
    o.clay = Some(serde_json::from_value(json!({ "blobs": blobs, "resolution": res })).unwrap());
    id
}

/// A rigged body with arms on both sides.
fn body(scene: &mut Scene) -> u64 {
    let id = clay(scene, "Body", json!([{ "type": "box", "center": [0, 1, 0], "size": [0.6, 0.8, 0.4], "blend": 0 }]), 24);
    scene.get_mut(id).unwrap().rig = Some(serde_json::from_value(json!({ "bones": [
        { "name": "spine", "head": [0, 0.6, 0], "tail": [0, 1.4, 0] },
        { "name": "arm.L", "parent": "spine", "head": [0.4, 1.3, 0], "tail": [1.0, 1.3, 0] },
        { "name": "arm.R", "parent": "spine", "head": [-0.4, 1.3, 0], "tail": [-1.0, 1.3, 0] },
    ], "split": false })).unwrap());
    id
}

fn mirror_x() -> MirrorOptions {
    MirrorOptions { normal: Vec3::X, d: 0.0, linked: false, name: None, kitbash: true }
}

fn reflect(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

fn world_vertices(scene: &Scene, meshes: &MeshLib, id: u64) -> Vec<(Vec3, Vec3)> {
    let o = scene.get(id).unwrap();
    let w = scene.world_matrix(id);
    let n = Mat3::from_mat4(w).inverse().transpose();
    meshes.get(o).unwrap().vertices.iter().map(|v| (w.transform_point3(Vec3::from(v.pos)), (n * Vec3::from(v.normal)).normalize())).collect()
}

fn nearest(points: &[(Vec3, Vec3)], p: Vec3) -> f32 {
    points.iter().map(|q| q.0.distance(p)).fold(f32::MAX, f32::min)
}

/// Share of vertices whose normal points away from the mesh's middle.
fn outward(points: &[(Vec3, Vec3)]) -> f32 {
    let c = points.iter().map(|p| p.0).sum::<Vec3>() / points.len() as f32;
    points.iter().filter(|(p, n)| n.dot(*p - c) > 0.0).count() as f32 / points.len() as f32
}

#[test]
fn mirrored_clay_is_a_true_mirror_with_bones_swapped() {
    let mut scene = Scene::default();
    let b = body(&mut scene);
    let arm = clay(&mut scene, "Gauntlet.L", json!([
        { "type": "sphere", "center": [0.25, 0, 0], "radius": 0.22, "bone": "arm.L" },
        { "type": "ellipsoid", "center": [-0.1, 0.2, 0.1], "radii": [0.25, 0.1, 0.15], "rotate": [20, 35, 0] },
        { "type": "capsule", "a": [0, 0, 0], "b": [0.1, -0.3, 0.2], "radius": 0.08 },
    ]), 40);
    {
        let o = scene.get_mut(arm).unwrap();
        o.parent = Some(b);
        o.bone = Some("arm.L".into());
        o.translation = Vec3::new(1.1, 1.25, 0.2);
        o.set_euler_degrees(Vec3::new(10.0, 25.0, -15.0));
        o.scale = Vec3::new(1.2, 0.8, 1.1);
    }
    scene.set_pivot_world(arm, Vec3::new(0.9, 1.3, 0.1));
    let made = pose::mirror(&mut scene, arm, &mirror_x()).unwrap();
    let copy = made.root;
    let c = scene.get(copy).unwrap();
    assert_eq!(c.name, "Gauntlet.R");
    assert_eq!(c.bone.as_deref(), Some("arm.R"));
    assert_eq!(c.clay.as_ref().unwrap().blobs[0].bone, "arm.R");
    assert!(c.link.is_none(), "unlinked by default");
    assert!(scene.world_matrix(copy).determinant() > 0.0, "a real mirror, not a negative scale");
    // Its own pivot is the reflection of the source's.
    assert!(scene.pivot_world(copy).distance(reflect(scene.pivot_world(arm))) < 1e-4);
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    let src = world_vertices(&scene, &meshes, arm);
    let dst = world_vertices(&scene, &meshes, copy);
    let cell = scene.get(arm).unwrap().clay.as_ref().unwrap().cell_size().unwrap() * 1.2;
    let worst = dst.iter().map(|(p, _)| nearest(&src, reflect(*p))).fold(0.0, f32::max);
    assert!(worst < cell * 1.5, "copy vertex {worst} from the reflected source (cell {cell})");
    let worst = src.iter().map(|(p, _)| nearest(&dst, reflect(*p))).fold(0.0, f32::max);
    assert!(worst < cell * 1.5, "source vertex {worst} from the reflected copy");
    assert!(outward(&dst) > 0.9 && outward(&dst) >= outward(&src) - 0.05, "normals face out: {} vs {}", outward(&dst), outward(&src));
}

#[test]
fn mirroring_meshes_with_negative_and_uneven_scale() {
    let mut scene = Scene::default();
    // A primitive with negative, non-uniform scale and a turn.
    let cube = scene.add(Shape::Cone);
    {
        let o = scene.get_mut(cube).unwrap();
        o.name = "Horn_L".into();
        o.translation = Vec3::new(1.5, 0.4, -0.3);
        o.scale = Vec3::new(-0.5, 1.4, 0.8);
        o.set_euler_degrees(Vec3::new(0.0, 30.0, 40.0));
    }
    // A kit piece (not symmetric: mirrored by its matrix).
    let rock = scene.add(Shape::Kit);
    {
        let o = scene.get_mut(rock).unwrap();
        o.name = "Rock".into();
        o.kit = Some(serde_json::from_value(json!({ "piece": "rock", "seed": 3 })).unwrap());
        o.translation = Vec3::new(2.0, 0.0, 1.0);
        o.scale = Vec3::new(1.0, 0.6, 1.4);
        o.set_euler_degrees(Vec3::new(0.0, 50.0, 0.0));
    }
    let a = pose::mirror(&mut scene, cube, &mirror_x()).unwrap();
    let b = pose::mirror(&mut scene, rock, &mirror_x()).unwrap();
    assert_eq!(scene.get(a.root).unwrap().name, "Horn_R");
    assert!(b.negative.contains(&b.root));
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    for (s, d) in [(cube, a.root), (rock, b.root)] {
        let src = world_vertices(&scene, &meshes, s);
        let dst = world_vertices(&scene, &meshes, d);
        let worst = dst.iter().map(|(p, _)| nearest(&src, reflect(*p))).fold(0.0, f32::max);
        assert!(worst < 1e-3, "{}: {worst}", scene.get(d).unwrap().name);
    }
    // The negative cone comes back with a normal (positive) matrix.
    assert!(scene.world_matrix(a.root).determinant() > 0.0);
}

#[test]
fn linked_mirror_follows_edits_until_unlinked() {
    let mut scene = Scene::default();
    let pod = clay(&mut scene, "Pod.L", json!([{ "type": "sphere", "center": [0.1, 0, 0], "radius": 0.3 }]), 24);
    scene.get_mut(pod).unwrap().translation = Vec3::new(1.0, 2.0, 0.0);
    let made = pose::mirror(&mut scene, pod, &MirrorOptions { linked: true, ..mirror_x() }).unwrap();
    let copy = made.root;
    // Edit the source: a blob, a material, a move.
    {
        let o = scene.get_mut(pod).unwrap();
        o.clay.as_mut().unwrap().blobs.push(serde_json::from_value(json!({ "type": "box", "center": [0.3, 0.2, 0], "size": [0.2, 0.2, 0.2], "bone": "x.L" })).unwrap());
        o.material = Some("metal".into());
        o.translation = Vec3::new(1.4, 2.5, 0.3);
        o.set_euler_degrees(Vec3::new(0.0, 0.0, 20.0));
    }
    assert!(pose::sync(&mut scene));
    let c = scene.get(copy).unwrap();
    assert_eq!(c.clay.as_ref().unwrap().blobs.len(), 2);
    assert!(c.clay.as_ref().unwrap().blobs[1].flip);
    assert_eq!(c.clay.as_ref().unwrap().blobs[1].bone, "x.R");
    assert_eq!(c.material.as_deref(), Some("metal"));
    let want = pose::reflection(Vec3::X, 0.0) * scene.world_matrix(pod) * Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    assert!(scene.world_matrix(copy).abs_diff_eq(want, 1e-4));
    // Nothing changed: nothing to do (no churn every frame).
    assert!(!pose::sync(&mut scene));
    // A detail pass on the source shows up on the copy.
    let mut meshes = MeshLib::new();
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "part": "bolt", "target": "Pod.L", "at": [1.4, 2.85, 0.3], "normal": [0, 1, 0] })).unwrap();
    pose::sync(&mut scene);
    let tag = r["group"].as_str().unwrap();
    let groups: Vec<_> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag) && o.shape == Shape::Group).collect();
    assert_eq!(groups.len(), 2, "the source's group and its mirror copy");
    assert!(groups.iter().any(|g| g.parent == Some(copy) && g.link.is_some()), "the mirrored group rides the copy");
    // Unlinked: the copy keeps what it has.
    pose::unlink(&mut scene, copy);
    scene.get_mut(pod).unwrap().clay.as_mut().unwrap().blobs.pop();
    pose::sync(&mut scene);
    assert_eq!(scene.get(copy).unwrap().clay.as_ref().unwrap().blobs.len(), 2);
}

#[test]
fn align_points_the_requested_axes() {
    let mut scene = Scene::default();
    let id = scene.add(Shape::Cube);
    {
        let o = scene.get_mut(id).unwrap();
        o.translation = Vec3::new(1.0, 2.0, 3.0);
        o.scale = Vec3::new(-1.0, 2.0, 0.5);
        o.set_euler_degrees(Vec3::new(12.0, 40.0, 7.0));
    }
    let pivot = scene.pivot_world(id);
    let x = Vec3::new(1.0, -1.0, 0.0).normalize();
    let z = Vec3::new(0.3, 0.3, 1.0);
    pose::align_axes(&mut scene, id, &[(Vec3::X, x), (Vec3::Z, z)]).unwrap();
    let w = scene.world_matrix(id);
    assert!(w.transform_vector3(Vec3::X).normalize().abs_diff_eq(x, 1e-4), "x exact (even with a negative scale)");
    let zw = w.transform_vector3(Vec3::Z).normalize();
    assert!(zw.dot(x).abs() < 1e-4 && zw.dot(z.normalize()) > 0.95, "z as close as it can");
    assert!(scene.pivot_world(id).distance(pivot) < 1e-4, "turned about its pivot");
    let s = w.to_scale_rotation_translation().0.abs();
    assert!(s.abs_diff_eq(Vec3::new(1.0, 2.0, 0.5), 1e-4), "scale kept: {s}");
    // In a bone's frame by a named point, riding the bone.
    let b = body(&mut scene);
    let gun = clay(&mut scene, "Gun", json!([{ "type": "box", "center": [0, 0, 0.3], "size": [0.1, 0.1, 0.8] }, { "name": "grip", "type": "box", "center": [0, -0.12, 0.1], "size": [0.06, 0.2, 0.06] }]), 24);
    let anchor = pose::named_point(scene.get(gun).unwrap(), "grip").unwrap();
    pose::frame_place(&mut scene, gun, &pose::FramePlace { of: pose::FrameOf::Bone { rig: b, bone: "arm.L".into(), at: 1.0 }, offset: Vec3::ZERO, rotate: Quat::IDENTITY, roll: 90.0, anchor, attach: true }).unwrap();
    let g = scene.get(gun).unwrap();
    assert_eq!((g.parent, g.bone.as_deref()), (Some(b), Some("arm.L")));
    let w = scene.world_matrix(gun);
    assert!(w.transform_point3(Vec3::new(0.0, -0.12, 0.1)).distance(Vec3::new(1.0, 1.3, 0.0)) < 1e-4, "grip in the hand");
    // Bone frame: Y along the bone (+X here), Z forward, X = Y x Z (down);
    // rolled 90 degrees about Y, the gun's Z takes the frame's X.
    assert!(w.transform_vector3(Vec3::Y).normalize().abs_diff_eq(Vec3::X, 1e-4));
    assert!(w.transform_vector3(Vec3::Z).normalize().abs_diff_eq(-Vec3::Y, 1e-4), "{}", w.transform_vector3(Vec3::Z));
}

#[test]
fn pose_in_place_carries_what_rides_the_bone() {
    let mut scene = Scene::default();
    let b = body(&mut scene);
    scene.get_mut(b).unwrap().clay.as_mut().unwrap().blobs.push(serde_json::from_value(json!({ "type": "capsule", "a": [0.4, 1.3, 0], "b": [1.0, 1.3, 0], "radius": 0.08, "bone": "arm.L" })).unwrap());
    let prop = scene.add(Shape::Cube);
    {
        let o = scene.get_mut(prop).unwrap();
        o.parent = Some(b);
        o.bone = Some("arm.L".into());
        o.translation = Vec3::new(0.9, 1.3, 0.0);
        o.scale = Vec3::splat(0.2);
    }
    let mut meshes = MeshLib::new();
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "part": "bolt", "target": prop, "at": [0.9, 1.41, 0], "normal": [0, 1, 0] })).unwrap();
    let tag = r["group"].as_str().unwrap().to_string();
    let copies = |s: &Scene| -> Vec<Vec3> {
        let eval = s.evaluate();
        s.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag.as_str())).filter_map(|o| Some((o, o.scatter.as_ref()?))).flat_map(|(o, sc)| { let w = eval[&o.id].0; sc.instances.iter().map(move |i| w.transform_point3(i.position)) }).collect()
    };
    let before = copies(&scene);
    let prop_before = scene.world_matrix(prop);
    // Swing the arm down 40 degrees about its head.
    let head = Vec3::new(0.4, 1.3, 0.0);
    let delta = Mat4::from_translation(head) * Mat4::from_rotation_z(-40f32.to_radians()) * Mat4::from_translation(-head);
    let out = pose::pose_rest(&mut scene, b, "arm.L", delta, true, &[]).unwrap();
    pose::sync(&mut scene);
    let rig = scene.get(b).unwrap().rig.as_ref().unwrap();
    let arm = &rig.bones[rig.bone_index("arm.L").unwrap()];
    assert!(arm.tail.abs_diff_eq(delta.transform_point3(Vec3::new(1.0, 1.3, 0.0)), 1e-5));
    assert!(rig.bones[rig.bone_index("arm.R").unwrap()].tail.abs_diff_eq(Vec3::new(-1.0, 1.3, 0.0), 1e-6), "the other arm stays");
    assert!(scene.world_matrix(prop).abs_diff_eq(delta * prop_before, 1e-4), "the prop rides along");
    let after = copies(&scene);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert!(delta.transform_point3(*a).distance(*b) < 1e-4, "kitbash copy left behind");
    }
    assert_eq!(out.blobs.len(), 1);
    let blob = &scene.get(b).unwrap().clay.as_ref().unwrap().blobs[1];
    let crate::clay::Form::Capsule { b: tip, .. } = blob.form else { panic!() };
    assert!(tip.abs_diff_eq(arm.tail, 1e-4), "the arm blob moved with the bone");
}

#[test]
fn kitbash_copies_follow_a_moved_and_mirrored_target() {
    let mut scene = Scene::default();
    let b = body(&mut scene);
    let plate = clay(&mut scene, "Plate.L", json!([{ "type": "box", "center": [0, 0, 0], "size": [0.4, 0.1, 0.4], "blend": 0 }]), 32);
    {
        let o = scene.get_mut(plate).unwrap();
        o.parent = Some(b);
        o.bone = Some("arm.L".into());
        o.translation = Vec3::new(0.8, 1.45, 0.0);
    }
    let mut meshes = MeshLib::new();
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "part": "bolt", "target": "Plate.L", "at": [0.8, 1.51, 0.1], "normal": [0, 1, 0], "array": { "count": 3, "step": [0.08, 0, 0] } })).unwrap();
    let tag = r["group"].as_str().unwrap().to_string();
    let copies = |s: &Scene| -> Vec<Vec3> {
        let eval = s.evaluate();
        s.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag.as_str())).filter_map(|o| Some((o, o.scatter.as_ref()?))).flat_map(|(o, sc)| { let w = eval[&o.id].0; sc.instances.iter().map(move |i| w.transform_point3(i.position)) }).collect()
    };
    let before = copies(&scene);
    let w0 = scene.world_matrix(plate);
    {
        let o = scene.get_mut(plate).unwrap();
        o.translation += Vec3::new(0.05, 0.1, -0.2);
        o.set_euler_degrees(Vec3::new(0.0, 30.0, 10.0));
    }
    assert!(pose::sync(&mut scene));
    let moved = scene.world_matrix(plate) * w0.inverse();
    for (a, b) in before.iter().zip(copies(&scene)) {
        assert!(moved.transform_point3(*a).distance(b) < 1e-4, "copy left behind");
    }
    // Mirroring the plate mirrors its parts onto the other arm's bone.
    let made = pose::mirror(&mut scene, plate, &mirror_x()).unwrap();
    assert_eq!(made.groups.len(), 1);
    let g = scene.get(made.groups[0]).unwrap();
    assert_eq!(g.bone.as_deref(), Some("arm.R"));
    let now = copies(&scene);
    let n = before.len();
    assert!(n >= 2, "{r}");
    assert_eq!(now.len(), 2 * n);
    for p in &now[..n] {
        assert!(now[n..].iter().any(|q| q.distance(reflect(*p)) < 1e-4), "mirrored copy missing for {p}");
    }
    // Parts on parts: a latch on the bolts' group.
    let group_name = scene.objects.iter().find(|o| o.kitbash.as_deref() == Some(tag.as_str()) && o.shape == Shape::Group).unwrap().name.clone();
    let p = now[0];
    let r = crate::kitbash::tool::kitbash(&mut scene, &mut meshes, &json!({ "part": { "kind": "washer", "size": [0.004, 0.001, 0.004] }, "target": group_name, "at": p + Vec3::Y * 0.012, "normal": [0, 1, 0] })).unwrap();
    assert_eq!(r["placed"], 1, "{r}");
}

/// A rig whose arm swings in clip `fire`, a two-bone hose and piston on it.
fn two_bone_scene() -> (Scene, u64, u64, u64) {
    let mut scene = Scene::default();
    let rig = clay(&mut scene, "Mech", json!([{ "type": "box", "center": [0, 1.2, 0], "size": [0.3, 0.6, 0.3], "blend": 0 }]), 24);
    scene.get_mut(rig).unwrap().rig = Some(serde_json::from_value(json!({
        "bones": [
            { "name": "chest", "head": [0, 1.0, 0], "tail": [0, 1.6, 0] },
            { "name": "arm", "parent": "chest", "head": [0.3, 1.5, 0], "tail": [0.3, 1.0, 0] },
            { "name": "hand", "parent": "arm", "head": [0.3, 1.0, 0], "tail": [0.3, 0.8, 0] },
        ],
        "clips": [{ "name": "fire", "loop": false, "tracks": [{ "bone": "arm", "keys": [
            { "frame": 0, "rotate": [0, 0, 0], "interp": "linear" },
            { "frame": 10, "rotate": [70, 0, 35], "interp": "linear" },
            { "frame": 20, "rotate": [-20, 0, 10] },
        ] }] }],
        "active": "fire", "split": false,
    })).unwrap());
    let group = scene.add(Shape::Group);
    {
        let o = scene.get_mut(group).unwrap();
        o.parent = Some(rig);
        o.bone = Some("chest".into());
        o.kitbash = Some("hoses".into());
    }
    let mut part = |kind: crate::kitbash::Kind, pts: &[[f32; 3]], r: f32| {
        let id = scene.add(Shape::Part);
        let o = scene.get_mut(id).unwrap();
        o.parent = Some(group);
        o.kitbash = Some("hoses".into());
        let mut p = crate::kitbash::Part::new(kind);
        p.points = pts.to_vec();
        p.radius = Some(r);
        p.span = Some(crate::kitbash::Span { bones: ["chest".into(), "hand".into()], rest_length: None, half: None });
        o.part = Some(p);
        id
    };
    let hose = part(crate::kitbash::Kind::Hose, &[[0.1, 1.4, 0.1], [0.25, 1.1, 0.25], [0.32, 0.9, 0.05]], 0.02);
    let piston = part(crate::kitbash::Kind::Piston, &[[0.15, 1.3, -0.1], [0.3, 0.95, -0.1]], 0.025);
    (scene, rig, hose, piston)
}

/// Where a rest point riding `bone` is at `frame` (world).
fn carried(scene: &Scene, rig: u64, bone: &str, p: Vec3, frame: f32) -> Vec3 {
    let r = scene.get(rig).unwrap().rig.as_ref().unwrap();
    scene.world_matrix(rig).transform_point3(r.skin_matrices(Some(frame))[r.bone_index(bone).unwrap()].transform_point3(p))
}

#[test]
fn two_bone_parts_keep_their_ends_on_their_bones() {
    let (mut scene, rig, hose, piston) = two_bone_scene();
    let mut meshes = MeshLib::new();
    for f in [0.0, 5.0, 10.0, 15.0, 20.0] {
        scene.frame = Some(f);
        meshes.frame = Some(f);
        meshes.sync(&scene);
        for (id, tol) in [(hose, 0.06), (piston, 0.04)] {
            let o = scene.get(id).unwrap();
            let rest: Vec<Vec3> = o.part.as_ref().unwrap().points.iter().map(|p| Vec3::from(*p)).collect();
            let posed = crate::kitbash::span::posed(&scene, o, Some(f)).unwrap();
            let w = scene.world_matrix(id);
            let first = w.transform_point3(Vec3::from(posed.points[0]));
            let last = w.transform_point3(Vec3::from(*posed.points.last().unwrap()));
            assert!(first.distance(carried(&scene, rig, "chest", rest[0], f)) < 1e-4, "frame {f}: first end left the chest");
            assert!(last.distance(carried(&scene, rig, "hand", *rest.last().unwrap(), f)) < 1e-4, "frame {f}: last end left the hand");
            // The mesh drawn (viewport, film, render_animation) is the solved one.
            let verts = world_vertices(&scene, &meshes, id);
            assert!(nearest(&verts, last) < tol, "frame {f}: mesh far from the hand end ({})", nearest(&verts, last));
            assert!(nearest(&verts, first) < tol, "frame {f}: mesh far from the chest end");
        }
    }
    // The piston keeps its cylinder length: the rod slides.
    scene.frame = Some(10.0);
    let p10 = crate::kitbash::span::posed(&scene, scene.get(piston).unwrap(), Some(10.0)).unwrap();
    assert!(p10.span.as_ref().unwrap().rest_length.is_some());
}

#[test]
fn two_bone_parts_stay_attached_in_the_exported_gltf() {
    let (mut scene, rig, hose, piston) = two_bone_scene();
    scene.frame = None;
    let mut meshes = MeshLib::new();
    meshes.sync(&scene);
    let opts = crate::gltf::Options { roots: Vec::new(), animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
    let out = crate::gltf::export(&scene, &meshes, &opts).unwrap();
    assert_eq!(out.summary["skins"], 2, "{}", out.summary);
    let hose_verts = scene.get(hose).unwrap().part.as_ref().unwrap().build(&scene.materials).vertices.len();
    let pist = scene.get(piston).unwrap().part.clone().unwrap();
    let ends: Vec<Vec3> = pist.points.iter().map(|p| Vec3::from(*p)).collect();
    let hose_pts: Vec<Vec3> = scene.get(hose).unwrap().part.as_ref().unwrap().points.iter().map(|p| Vec3::from(*p)).collect();
    for f in [0.0, 6.0, 10.0, 16.0, 20.0] {
        let loaded = crate::glb_read::from_bytes(&out.bytes, Some("fire"), Some(f)).unwrap();
        let verts = |n: usize| -> Vec<(Vec3, Vec3)> { loaded.items.iter().filter(|i| i.mesh.vertices.len() == n).flat_map(|i| i.mesh.vertices.iter().map(|v| (i.world.transform_point3(Vec3::from(v.pos)), Vec3::ZERO))).collect() };
        let h = verts(hose_verts);
        assert!(!h.is_empty(), "the hose is in the file");
        let a = carried(&scene, rig, "chest", hose_pts[0], f);
        let b = carried(&scene, rig, "hand", *hose_pts.last().unwrap(), f);
        assert!(nearest(&h, a) < 0.06 && nearest(&h, b) < 0.06, "frame {f}: hose ends {} {}", nearest(&h, a), nearest(&h, b));
        // Piston halves: on their bones and aimed at each other.
        let (pa, pb) = (carried(&scene, rig, "chest", ends[0], f), carried(&scene, rig, "hand", ends[1], f));
        let axis = (pb - pa).normalize();
        let off_axis = |p: Vec3| (p - pa - axis * (p - pa).dot(axis)).length();
        let mut found = 0;
        for i in &loaded.items {
            let pts: Vec<Vec3> = i.mesh.vertices.iter().map(|v| i.world.transform_point3(Vec3::from(v.pos))).collect();
            let near_a = pts.iter().map(|p| p.distance(pa)).fold(f32::MAX, f32::min);
            let near_b = pts.iter().map(|p| p.distance(pb)).fold(f32::MAX, f32::min);
            if (near_a < 0.04 || near_b < 0.04) && pts.len() < 20_000 && i.mesh.vertices.len() != hose_verts && pts.iter().all(|p| off_axis(*p) < 0.025 * 1.8) {
                found += 1;
            }
        }
        assert!(found >= 2, "frame {f}: cylinder and rod on the line between the piston's ends ({found})");
    }
}

#[test]
fn path_arrays_turn_with_the_path() {
    // A loop in the XY plane, around and back.
    let pts: Vec<Vec3> = (0..=24).map(|i| { let a = i as f32 / 24.0 * std::f32::consts::TAU; Vec3::new(a.cos(), a.sin(), 0.0) }).collect();
    let turns = crate::clay::path_turns(&pts);
    let t0 = (pts[1] - pts[0]).normalize();
    for j in 0..turns.len() {
        let t = (pts[j + 1] - pts[j]).normalize();
        assert!((turns[j] * t0).abs_diff_eq(t, 1e-3), "segment {j}");
        // No flip: the loop's normal stays put.
        assert!((turns[j] * Vec3::Z).abs_diff_eq(Vec3::Z, 1e-3), "segment {j} flipped");
    }
    // And the clay mesh: a long thin link turned along a vertical loop
    // stands up at the loop's sides.
    let fixed = json!([{ "type": "array", "count": 8, "points": [[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,0]], "source": { "type": "box", "center": [0,0,0], "size": [0.2, 0.04, 0.3] }, "blend": 0 }]);
    let mut turned = fixed.clone();
    turned[0]["orient"] = json!("path");
    let bounds = |v: serde_json::Value| { let c: crate::clay::Clay = serde_json::from_value(json!({ "blobs": v, "resolution": 64 })).unwrap(); c.build_mesh_with(&[], "").bounds() };
    let (lo_f, hi_f) = bounds(fixed);
    let (lo_t, hi_t) = bounds(turned);
    // Turned links on the vertical runs reach out sideways in x.
    assert!(hi_t.x - lo_t.x > hi_f.x - lo_f.x + 0.1 - 1e-3 || hi_t.y - lo_t.y > hi_f.y - lo_f.y + 0.05, "{lo_f} {hi_f} vs {lo_t} {hi_t}");
}

#[test]
fn side_names_swap_inside_names() {
    assert_eq!(pose::side_name("arm.L"), "arm.R");
    assert_eq!(pose::side_name("bolts_arm.L_copies"), "bolts_arm.R_copies");
    assert_eq!(pose::side_name("Shin_R2"), "Shin_R2_mirror");
    assert_eq!(pose::side_name("Torso"), "Torso_mirror");
}

/// A linked mirror copy of a Poly follows its source: its levels and
/// creases, a copy that reflects its own geometry (mirrored once per
/// source change, no churn), and a source made clay in place (the copy
/// keeps no stale cage).
#[test]
fn linked_mirror_follows_a_poly() {
    let mut scene = Scene::default();
    let src = scene.add(Shape::Poly);
    {
        let o = scene.get_mut(src).unwrap();
        o.name = "Plate.L".into();
        o.translation = Vec3::new(1.5, 0.0, 0.0);
        o.poly = Some(crate::poly::tests::cube());
    }
    let copy = pose::mirror(&mut scene, src, &MirrorOptions { linked: true, ..mirror_x() }).unwrap().root;
    let poly = |scene: &Scene, id: u64| scene.get(id).unwrap().poly.clone();
    assert!(poly(&scene, copy) == poly(&scene, src));
    // Levels and creases on the source show on the copy.
    {
        let p = scene.get_mut(src).unwrap().poly.as_mut().unwrap();
        p.set_levels(2).unwrap();
        p.level = 1;
        p.creases = std::sync::Arc::new([((0, 4), 2.0)].into_iter().collect());
    }
    assert!(pose::sync(&mut scene));
    assert!(poly(&scene, copy) == poly(&scene, src), "the copy has the source's levels and creases");
    assert!(!pose::sync(&mut scene));
    // A copy reflecting its own geometry gets the mirror image, once.
    scene.get_mut(copy).unwrap().link.as_mut().unwrap().axis = Some(0);
    assert!(pose::sync(&mut scene));
    assert!(poly(&scene, copy) == poly(&scene, src).map(|p| p.mirrored(0)));
    assert!(!pose::sync(&mut scene), "mirrored again only when the source changes");
    scene.get_mut(copy).unwrap().link.as_mut().unwrap().axis = None;
    pose::sync(&mut scene);
    // The source made clay in place (as to_clay does): the copy is clay, with no cage.
    {
        let o = scene.get_mut(src).unwrap();
        o.shape = Shape::Clay;
        o.poly = None;
        o.clay = Some(serde_json::from_value(json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }], "resolution": 24 })).unwrap());
    }
    assert!(pose::sync(&mut scene));
    let c = scene.get(copy).unwrap();
    assert!(c.shape == Shape::Clay && c.poly.is_none() && c.clay.is_some());
}

/// A linked mirror copy of an imported mesh object shows the source's
/// mesh, and follows it when the mesh changes (it had none before).
#[test]
fn linked_mirror_follows_an_imported_mesh() {
    let mut scene = Scene::default();
    let src = scene.add(Shape::Mesh);
    let (points, tris) = crate::mesh_asset::tests::icosphere(1, 0.5);
    {
        let o = scene.get_mut(src).unwrap();
        o.name = "Rock.L".into();
        o.translation = Vec3::new(1.5, 0.0, 0.0);
        o.mesh = Some(crate::mesh_asset::MeshAsset::new(points.clone(), tris.clone()).unwrap());
    }
    let copy = pose::mirror(&mut scene, src, &MirrorOptions { linked: true, ..mirror_x() }).unwrap().root;
    let mesh = |scene: &Scene, id: u64| scene.get(id).unwrap().mesh.clone();
    assert!(mesh(&scene, copy).is_some() && mesh(&scene, copy) == mesh(&scene, src));
    let (p2, t2) = crate::mesh_asset::tests::icosphere(2, 0.5);
    scene.get_mut(src).unwrap().mesh = Some(crate::mesh_asset::MeshAsset::new(p2, t2).unwrap());
    assert!(pose::sync(&mut scene));
    assert!(mesh(&scene, copy) == mesh(&scene, src), "the copy follows a new mesh");
    assert!(!pose::sync(&mut scene));
}
