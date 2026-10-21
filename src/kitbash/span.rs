//! Two-bone parts: hoses, cables and pistons whose ends ride different
//! bones (chest -> hand). The first point rides `span.bones[0]`, the last
//! `span.bones[1]`; points between blend from one to the other along the
//! length. Every pose solves the part again from its moved points: hoses
//! hang and bend anew, pistons aim their cylinder and rod at each other
//! with the rest lengths kept. The viewport, film and animation renders
//! read the solved mesh (`MeshLib`); glTF writes hoses as meshes skinned to
//! both joints and pistons as two rigid nodes animated to aim (`gltf.rs`).

use glam::{Mat4, Vec3};

use super::{Kind, Part};
use crate::scene::{Object, Scene};

/// Where a two-bone part hangs: the rigged object, the bone its own
/// placement rides, and part space -> rig rest space.
#[derive(Clone, Debug)]
pub struct Hang {
    pub rig: u64,
    pub bone: String,
    pub to_rig: Mat4,
}

/// The rig a part hangs on (through the first ancestor riding a bone, or
/// a rigged parent).
pub fn hang(scene: &Scene, o: &Object) -> Option<Hang> {
    let mut m = Mat4::IDENTITY;
    let mut cur = o;
    for _ in 0..crate::scene::MAX_DEPTH {
        m = cur.local_matrix() * m;
        let parent = scene.get(cur.parent?)?;
        if let Some(b) = &cur.bone
            && parent.rig.as_ref().is_some_and(|r| r.bone_index(b).is_some())
        {
            return Some(Hang { rig: parent.id, bone: b.clone(), to_rig: m });
        }
        if parent.rig.as_ref().is_some_and(|r| !r.bones.is_empty()) {
            // Hanging on the rig object itself: its root bone carries it.
            let root = parent.rig.as_ref()?.bones.iter().find(|b| b.parent.is_none())?.name.clone();
            return Some(Hang { rig: parent.id, bone: root, to_rig: m });
        }
        cur = parent;
    }
    None
}

/// How far along the points each one is (0 first, 1 last), eased so the
/// ends stay with their bones and the middle blends.
pub fn blend_weights(points: &[Vec3]) -> Vec<f32> {
    let mut acc = vec![0.0f32; points.len()];
    for i in 1..points.len() {
        acc[i] = acc[i - 1] + points[i].distance(points[i - 1]);
    }
    let total = acc.last().copied().unwrap_or(0.0).max(1e-9);
    acc.iter().map(|a| {
        let t = (a / total).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }).collect()
}

/// The part as it is at `frame` (None = rest): points moved with their
/// bones, in the part's own space, and the rest length kept for pistons.
/// None when it is not a two-bone part or its rig or bones are missing.
pub fn posed(scene: &Scene, o: &Object, frame: Option<f32>) -> Option<Part> {
    let part = o.part.as_ref()?;
    let span = part.span.as_ref()?;
    let h = hang(scene, o)?;
    let rig = scene.get(h.rig)?.rig.as_ref()?;
    let (ia, ib, ih) = (rig.bone_index(&span.bones[0])?, rig.bone_index(&span.bones[1])?, rig.bone_index(&h.bone)?);
    let frame = if rig.is_posed(frame) { frame } else { None };
    let skin = rig.skin_matrices(frame);
    let pts: Vec<Vec3> = part.points.iter().map(|p| h.to_rig.transform_point3(Vec3::from(*p))).collect();
    let w = blend_weights(&pts);
    let back = (skin[ih] * h.to_rig).inverse();
    let mut out = part.clone();
    out.points = pts.iter().zip(&w).map(|(q, w)| {
        let p = skin[ia].transform_point3(*q).lerp(skin[ib].transform_point3(*q), *w);
        back.transform_point3(p).to_array()
    }).collect();
    let ends = |p: &[[f32; 3]]| Vec3::from(p[0]).distance(Vec3::from(p[p.len() - 1]));
    if let Some(s) = out.span.as_mut() {
        s.rest_length = Some(ends(&part.points));
    }
    Some(out)
}

/// Kinds that can run between two bones.
pub fn can_span(kind: Kind) -> bool {
    matches!(kind, Kind::Piston | Kind::Cable | Kind::Hose | Kind::CorrugatedTube | Kind::Pipe | Kind::Strap)
}

/// The bone that moves a rig-space point most like it would be carried:
/// the deform bone whose segment is nearest.
pub fn nearest_bone(rig: &crate::rig::Rig, p: Vec3) -> Option<String> {
    rig.bones.iter().filter(|b| b.deform).min_by(|a, b| seg(p, a.head, a.tail).total_cmp(&seg(p, b.head, b.tail))).map(|b| b.name.clone())
}

fn seg(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let d = b - a;
    let t = ((p - a).dot(d) / d.length_squared().max(1e-12)).clamp(0.0, 1.0);
    p.distance(a + d * t)
}
