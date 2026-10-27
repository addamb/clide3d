//! Skeletons, skin weights and keyframe animation for clay characters.
//!
//! A rig is a list of bones in the clay object's own space, in the rest pose
//! (the pose the clay was modeled in). A bone starts at `head` and points at
//! `tail`; a child's head usually sits on its parent's tail. Bones have no
//! rest rotation, so a key's `rotate` turns a bone (and everything under it)
//! around its head, in the object's axes. This maps straight onto glTF joints.
//!
//! Skin weights (how much each bone moves each vertex) are made from the mesh
//! automatically, see `skin`. Clay is built from blobs, and blobs are a good
//! guess at body parts: a vertex follows the bones that live in its own blob
//! (so a raised arm does not drag the belly), then the closest of those, and
//! only bones it can "see" through the solid clay (an arm vertex does not
//! follow a nearby leg bone across empty space). The weights are then
//! smoothed along the surface so joints bend softly.
//!
//! Clips hold keys per bone. The rig's `active` clip plays on the time
//! slider; frames are time-slider frames.

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::clay::{Clay, Field, par_chunks, rotation};
use crate::material::Material;
use crate::mesh::{Mesh, Vertex};

/// Limits that keep a hostile file or call from stalling the editor.
pub const MAX_BONES: usize = 128;
pub const MAX_CLIPS: usize = 64;
pub const MAX_KEYS: usize = 20_000;
pub const MAX_NAME: usize = 64;
/// Bones a vertex can follow (glTF's 4).
const INFLUENCES: usize = 4;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Rig {
    #[serde(default)]
    pub bones: Vec<Bone>,
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// The clip that plays on the time slider; None shows the rest pose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Build each moving part (body, head, each limb) as its own closed
    /// shell, so a limb that swings away does not leave a dent where it
    /// was melted into the body. Joints then look sewn on, not melted.
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    pub split: bool,
    /// Ordered brushes in rest object space, reapplied after automatic weights.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<WeightStroke>,
}

impl Default for Rig {
    fn default() -> Self {
        Self { bones: Vec::new(), clips: Vec::new(), active: None, split: true, weights: Vec::new() }
    }
}

pub const MAX_WEIGHT_STROKES: usize = 1024;
pub const MAX_WEIGHT_POINTS: usize = 256;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct WeightStroke {
    pub bone: String,
    pub points: Vec<Vec3>,
    pub radius: f32,
    pub strength: f32,
    pub mode: WeightMode,
    /// Rest object vectors to brush space; preserves a world-space radius
    /// on objects with nonuniform scale.
    #[serde(default = "identity")]
    pub space: Mat4,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WeightMode { Add, Subtract, Replace, Smooth }

fn identity() -> Mat4 { Mat4::IDENTITY }

impl WeightStroke {
    pub fn valid(&self) -> bool {
        !self.points.is_empty() && self.points.len() <= MAX_WEIGHT_POINTS
            && self.points.iter().all(|p| p.is_finite()) && self.space.is_finite()
            && self.radius.is_finite() && self.radius > 0.0
            && self.strength.is_finite() && (0.0..=1.0).contains(&self.strength)
    }

    fn falloff(&self, p: Vec3) -> f32 {
        let d = if self.points.len() == 1 {
            self.space.transform_vector3(p - self.points[0]).length()
        } else {
            let p = self.space.transform_vector3(p);
            self.points.windows(2).map(|ab| {
                let (a, b) = (self.space.transform_vector3(ab[0]), self.space.transform_vector3(ab[1]));
                p.distance(closest_on_segment(p, a, b))
            }).fold(f32::MAX, f32::min)
        };
        // Full effect within half the radius (so replace really reaches its
        // value there), fading smoothly to nothing at the radius.
        let t = (2.0 * (1.0 - d / self.radius)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Bone {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub head: Vec3,
    pub tail: Vec3,
    /// False for helper bones (a root, a control) that move children but no skin.
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    pub deform: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Clip {
    pub name: String,
    /// Wrap around between the first and last key frame.
    #[serde(default = "yes", rename = "loop")]
    pub looped: bool,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Track {
    pub bone: String,
    /// Sorted by frame, one key per frame.
    pub keys: Vec<Key>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Key {
    pub frame: f32,
    /// Degrees, XYZ order (like object and blob rotation).
    #[serde(default)]
    pub rotate: Vec3,
    /// Offset from the rest position, in the parent's axes.
    #[serde(default)]
    pub translate: Vec3,
    #[serde(default = "one")]
    pub scale: Vec3,
    /// How the value moves from this key to the next.
    #[serde(default)]
    pub interp: Interp,
}

fn one() -> Vec3 {
    Vec3::ONE
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Interp {
    /// Eases out of and into each key.
    #[default]
    Smooth,
    Linear,
    /// Holds until the next key.
    Step,
}

/// One bone's pose: rotation, offset and scale relative to the rest pose.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pose {
    pub rotate: Quat,
    pub translate: Vec3,
    pub scale: Vec3,
}

impl Default for Pose {
    fn default() -> Self {
        Self { rotate: Quat::IDENTITY, translate: Vec3::ZERO, scale: Vec3::ONE }
    }
}

impl Key {
    pub fn new(frame: f32) -> Self {
        Self { frame, rotate: Vec3::ZERO, translate: Vec3::ZERO, scale: Vec3::ONE, interp: Interp::Smooth }
    }

    fn pose(&self) -> Pose {
        Pose { rotate: rotation(self.rotate), translate: self.translate, scale: self.scale }
    }
}

impl Track {
    pub fn sample(&self, frame: f32) -> Pose {
        let keys = &self.keys;
        let Some(first) = keys.first() else { return Pose::default() };
        let next = keys.partition_point(|k| k.frame <= frame);
        if next == 0 {
            return first.pose();
        }
        let k0 = &keys[next - 1];
        let Some(k1) = keys.get(next) else { return k0.pose() };
        let t = ((frame - k0.frame) / (k1.frame - k0.frame).max(1e-6)).clamp(0.0, 1.0);
        let t = match k0.interp {
            Interp::Step => 0.0,
            Interp::Linear => t,
            Interp::Smooth => t * t * (3.0 - 2.0 * t),
        };
        let (a, b) = (k0.pose(), k1.pose());
        Pose { rotate: a.rotate.slerp(b.rotate, t), translate: a.translate.lerp(b.translate, t), scale: a.scale.lerp(b.scale, t) }
    }

    /// The raw channel values at `frame`, blended straight between the
    /// neighboring keys (a starting point for a new key).
    pub fn sample_key(&self, frame: f32) -> Key {
        let keys = &self.keys;
        let next = keys.partition_point(|k| k.frame <= frame);
        let (k0, k1) = match (next.checked_sub(1).map(|i| keys[i]), keys.get(next).copied()) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => return a,
            (None, Some(b)) => return b,
            (None, None) => return Key::new(frame),
        };
        let t = ((frame - k0.frame) / (k1.frame - k0.frame).max(1e-6)).clamp(0.0, 1.0);
        Key {
            frame,
            rotate: k0.rotate.lerp(k1.rotate, t),
            translate: k0.translate.lerp(k1.translate, t),
            scale: k0.scale.lerp(k1.scale, t),
            interp: k0.interp,
        }
    }

    /// Adds or replaces the key at `key.frame`.
    pub fn insert(&mut self, key: Key) {
        match self.keys.binary_search_by(|k| k.frame.total_cmp(&key.frame)) {
            Ok(i) => self.keys[i] = key,
            Err(i) => self.keys.insert(i, key),
        }
    }

    /// The key at `frame`, if there is one.
    pub fn key_at(&self, frame: f32) -> Option<&Key> {
        self.keys.iter().find(|k| (k.frame - frame).abs() < 1e-4)
    }
}

impl Clip {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), looped: true, tracks: Vec::new() }
    }

    /// First and last key frame.
    pub fn range(&self) -> Option<(f32, f32)> {
        let frames = self.tracks.iter().flat_map(|t| t.keys.iter().map(|k| k.frame));
        frames.fold(None, |r, f| Some(r.map_or((f, f), |(a, b): (f32, f32)| (a.min(f), b.max(f)))))
    }

    /// Every key frame, sorted and unique.
    pub fn frames(&self) -> Vec<f32> {
        let mut f: Vec<f32> = self.tracks.iter().flat_map(|t| t.keys.iter().map(|k| k.frame)).collect();
        f.sort_by(f32::total_cmp);
        f.dedup();
        f
    }

    pub fn key_count(&self) -> usize {
        self.tracks.iter().map(|t| t.keys.len()).sum()
    }

    /// Clip-local frame for a time-slider frame (wraps when looping).
    pub fn local_frame(&self, frame: f32) -> f32 {
        match self.range() {
            Some((a, b)) if self.looped && b - a > 1e-3 => a + (frame - a).rem_euclid(b - a),
            _ => frame,
        }
    }

    pub fn track(&self, bone: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.bone == bone)
    }

    pub fn track_mut(&mut self, bone: &str) -> &mut Track {
        if let Some(i) = self.tracks.iter().position(|t| t.bone == bone) {
            return &mut self.tracks[i];
        }
        self.tracks.push(Track { bone: bone.into(), keys: Vec::new() });
        self.tracks.last_mut().expect("just pushed")
    }
}

impl Rig {
    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }

    pub fn clip(&self, name: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.name == name)
    }

    pub fn active_clip(&self) -> Option<&Clip> {
        self.active.as_deref().and_then(|n| self.clip(n))
    }

    /// True when `frame` shows anything but the rest pose.
    pub fn is_posed(&self, frame: Option<f32>) -> bool {
        frame.is_some() && self.active_clip().is_some_and(|c| c.key_count() > 0)
    }

    /// Each bone's pose at `frame` in the active clip (None = rest pose).
    pub fn poses(&self, frame: Option<f32>) -> Vec<Pose> {
        let clip = frame.and(self.active_clip());
        self.bones
            .iter()
            .map(|b| match (clip, frame) {
                (Some(c), Some(f)) => c.track(&b.name).map_or(Pose::default(), |t| t.sample(c.local_frame(f))),
                _ => Pose::default(),
            })
            .collect()
    }

    /// Each bone's pose in `clip` at a clip frame (exports sample every clip).
    pub fn clip_poses(&self, clip: &Clip, frame: f32) -> Vec<Pose> {
        self.bones.iter().map(|b| clip.track(&b.name).map_or(Pose::default(), |t| t.sample(frame))).collect()
    }

    /// Bone-to-object matrices for the given poses. Bones are stored parents
    /// first (see `sanitize`), so one pass is enough.
    pub fn globals(&self, poses: &[Pose]) -> Vec<Mat4> {
        let mut out: Vec<Mat4> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let parent = b.parent.as_deref().and_then(|p| self.bones[..i].iter().position(|x| x.name == p));
            let base = parent.map_or(Vec3::ZERO, |p| self.bones[p].head);
            let pose = poses.get(i).copied().unwrap_or_default();
            let local = Mat4::from_scale_rotation_translation(pose.scale, pose.rotate, b.head - base + pose.translate);
            out.push(parent.map_or(local, |p| out[p] * local));
        }
        out
    }

    /// Skinning matrices: rest-pose object space to posed object space.
    pub fn skin_matrices(&self, frame: Option<f32>) -> Vec<Mat4> {
        let g = self.globals(&self.poses(frame));
        g.iter().zip(&self.bones).map(|(m, b)| *m * Mat4::from_translation(-b.head)).collect()
    }

    /// Posed (head, tail) of every bone, in object space.
    pub fn joints(&self, frame: Option<f32>) -> Vec<(Vec3, Vec3)> {
        let g = self.globals(&self.poses(frame));
        g.iter().zip(&self.bones).map(|(m, b)| (m.transform_point3(Vec3::ZERO), m.transform_point3(b.tail - b.head))).collect()
    }

    /// Fixes what a broken or hostile file could contain: bad numbers, too
    /// much data, unknown or looping parents, keys for missing bones.
    pub fn sanitize(&mut self) {
        let finite = |v: Vec3| if v.is_finite() { v } else { Vec3::ZERO };
        self.bones.truncate(MAX_BONES);
        let mut seen = std::collections::HashSet::new();
        self.bones.retain(|b| !b.name.is_empty() && b.name.len() <= MAX_NAME && seen.insert(b.name.clone()));
        for b in &mut self.bones {
            b.head = finite(b.head);
            b.tail = finite(b.tail);
            if b.parent.as_ref().is_some_and(|p| !seen.contains(p) || *p == b.name) {
                b.parent = None;
            }
        }
        // Parents first; a parent loop is cut where it closes.
        let mut placed: Vec<Bone> = Vec::with_capacity(self.bones.len());
        let mut left = std::mem::take(&mut self.bones);
        while !left.is_empty() {
            let before = left.len();
            let mut i = 0;
            while i < left.len() {
                let ready = left[i].parent.as_ref().is_none_or(|p| placed.iter().any(|b| b.name == *p));
                if ready {
                    placed.push(left.remove(i));
                } else {
                    i += 1;
                }
            }
            if left.len() == before {
                left[0].parent = None;
            }
        }
        self.bones = placed;

        self.weights.truncate(MAX_WEIGHT_STROKES);
        self.weights.retain(|s| s.valid() && self.bones.iter().any(|b| b.name == s.bone));
        self.clips.truncate(MAX_CLIPS);
        let mut names = std::collections::HashSet::new();
        self.clips.retain(|c| !c.name.is_empty() && c.name.len() <= MAX_NAME && names.insert(c.name.clone()));
        let bones: std::collections::HashSet<&str> = self.bones.iter().map(|b| b.name.as_str()).collect();
        for c in &mut self.clips {
            let mut budget = MAX_KEYS;
            let mut tracks_seen = std::collections::HashSet::new();
            c.tracks.retain(|t| bones.contains(t.bone.as_str()) && tracks_seen.insert(t.bone.clone()));
            for t in &mut c.tracks {
                t.keys.retain(|k| k.frame.is_finite() && k.frame.abs() < 1e6);
                t.keys.truncate(budget);
                budget -= t.keys.len();
                for k in &mut t.keys {
                    k.rotate = finite(k.rotate);
                    k.translate = finite(k.translate);
                    k.scale = if k.scale.is_finite() { k.scale.clamp(Vec3::splat(0.01), Vec3::splat(100.0)) } else { Vec3::ONE };
                }
                t.keys.sort_by(|a, b| a.frame.total_cmp(&b.frame));
                t.keys.dedup_by(|a, b| (a.frame - b.frame).abs() < 1e-4);
            }
        }
        if self.active.as_ref().is_some_and(|a| self.clip(a).is_none()) {
            self.active = None;
        }
    }
}

/// The name of the same bone on the other side ("arm.L" <-> "arm.R").
pub fn mirror_name(name: &str) -> Option<String> {
    const PAIRS: [(&str, &str); 6] = [(".L", ".R"), ("_L", "_R"), (".l", ".r"), ("_l", "_r"), ("Left", "Right"), ("left", "right")];
    for (a, b) in PAIRS {
        if let Some(stem) = name.strip_suffix(a) {
            return Some(format!("{stem}{b}"));
        }
        if let Some(stem) = name.strip_suffix(b) {
            return Some(format!("{stem}{a}"));
        }
    }
    None
}

/// A key as seen from the other side of X = 0.
pub fn mirror_key(k: &Key) -> Key {
    Key {
        rotate: Vec3::new(k.rotate.x, -k.rotate.y, -k.rotate.z),
        translate: Vec3::new(-k.translate.x, k.translate.y, k.translate.z),
        ..*k
    }
}

/// Up to 4 (bone, weight) pairs per vertex; bones are rig bone indices.
#[derive(Clone, Debug, Default)]
pub struct Skin {
    pub joints: Vec<[u16; INFLUENCES]>,
    pub weights: Vec<[f32; INFLUENCES]>,
}

impl Skin {
    /// Vertices each bone moves the most (weight at least 0.5).
    pub fn counts(&self, bones: usize) -> Vec<usize> {
        let mut c = vec![0; bones];
        for (j, w) in self.joints.iter().zip(&self.weights) {
            for i in 0..INFLUENCES {
                if w[i] >= 0.5 && (j[i] as usize) < bones {
                    c[j[i] as usize] += 1;
                }
            }
        }
        c
    }
}

fn closest_on_segment(p: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
    a + ab * t
}

/// Automatic skin weights for a clay mesh (see the module docs).
pub fn skin(clay: &Clay, mesh: &Mesh, rig: &Rig) -> Skin {
    skin_with(clay, mesh, rig, None)
}

/// Skin weights using only the bones in `only` (one part of a split rig).
fn skin_with(clay: &Clay, mesh: &Mesh, rig: &Rig, only: Option<&[usize]>) -> Skin {
    // Coincident crease vertices must receive identical skin weights, or a
    // rigid corner opens when animated. Smooth/paint on geometric adjacency.
    if let Some((geometry,remap))=mesh.welded_geometry() {
        let skin=skin_with(clay,&geometry,rig,only);
        return Skin {joints:remap.iter().map(|&i|skin.joints[i]).collect(),weights:remap.iter().map(|&i|skin.weights[i]).collect()};
    }
    let n = mesh.vertices.len();
    let deform: Vec<usize> = (0..rig.bones.len()).filter(|&i| rig.bones[i].deform).collect();
    let deform = match (deform.is_empty(), only) {
        (false, _) => deform,
        (true, Some(o)) if !o.is_empty() => o.to_vec(),
        _ => (0..rig.bones.len()).collect(),
    };
    if n == 0 || deform.is_empty() {
        return Skin { joints: vec![[0; INFLUENCES]; n], weights: vec![[1.0, 0.0, 0.0, 0.0]; n] };
    }
    let cell = clay.cell_size().unwrap_or(0.02);
    let field = Field::new(clay, cell);
    let list = field.bounds().map(|(lo, hi)| field.near(lo, hi)).unwrap_or_default();
    let solid = |p: Vec3| field.sample(p, &list).0 <= 0.25 * cell;
    let nd = deform.len();
    // A part already knows its bones; the whole mesh asks each blob.
    let allowed = blob_bones(&field, rig, &deform);
    const CANDIDATES: usize = 6;
    const AXIS_WEIGHT: f32 = 0.35;
    // Blend width at joints: bones closer than the nearest one by more than a
    // few sigmas get almost nothing, so thick bodies do not smear across bones.
    let mut lengths: Vec<f32> = deform.iter().map(|&b| rig.bones[b].head.distance(rig.bones[b].tail)).collect();
    lengths.sort_by(f32::total_cmp);
    let sigma = (lengths[lengths.len() / 2] * 0.12).max(cell * 1.5);

    // Raw weights, one row of `nd` per vertex.
    let mut w = vec![0.0f32; n * nd];
    par_chunks(&mut w, nd * 512, |chunk, rows| {
        for (r, row) in rows.chunks_exact_mut(nd).enumerate() {
            let v = &mesh.vertices[chunk * 512 + r];
            // Start just inside the surface, so the sight line runs through clay.
            let p = Vec3::from(v.pos) - Vec3::from(v.normal) * cell;
            // Distance past the bone's ends counts fully, distance out from
            // its axis only partly: on a thick body every spine bone is about
            // as far away, but only one runs past the vertex.
            let owner = owner_blob(&field, p);
            if let Some(slot) = owner.and_then(|i| rig.bone_index(field.blob_bone(i))).and_then(|b| deform.iter().position(|&d| d == b)) {
                row[slot] = 1.0;
                continue;
            }
            let home = owner.and_then(|i| allowed[i].as_ref());
            let mut near: Vec<(usize, f32, Vec3)> = deform
                .iter()
                .enumerate()
                .filter(|(slot, b)| only.map_or_else(|| home.is_none_or(|h| h.contains(slot)), |o| o.contains(b)))
                .map(|(slot, &b)| {
                    let (a, e) = (rig.bones[b].head, rig.bones[b].tail);
                    let q = closest_on_segment(p, a, e);
                    let len = a.distance(e).max(1e-6);
                    let t = (p - a).dot(e - a) / len;
                    let past = (-t).max(t - len).max(0.0);
                    let out = (p.distance(q).powi(2) - past * past).max(0.0).sqrt();
                    (slot, past + AXIS_WEIGHT * out, q)
                })
                .collect();
            near.sort_by(|a, b| a.1.total_cmp(&b.1));
            near.truncate(CANDIDATES);
            let seen: Vec<bool> = near
                .iter()
                .map(|&(_, _, q)| {
                    let steps = (p.distance(q) / cell).ceil().clamp(1.0, 24.0) as usize;
                    (1..=steps).all(|s| solid(p.lerp(q, s as f32 / steps as f32)))
                })
                .collect();
            let any_seen = seen.iter().any(|s| *s);
            let usable = |s: bool| s || !any_seen;
            let dmin = near.iter().zip(&seen).filter(|(_, s)| usable(**s)).map(|(n, _)| n.1).fold(f32::MAX, f32::min);
            for (&(slot, d, _), &s) in near.iter().zip(&seen) {
                if usable(s) {
                    let x = (d - dmin) / sigma;
                    row[slot] = (-x * x).exp();
                }
            }
            let sum: f32 = row.iter().sum();
            if sum > 0.0 {
                row.iter_mut().for_each(|x| *x /= sum);
            }
        }
    });

    // Smooth along the surface so joints bend softly.
    if n * nd <= 16_000_000 {
        let mut nbrs: Vec<Vec<u32>> = vec![Vec::new(); n];
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                nbrs[a as usize].push(b);
                nbrs[b as usize].push(a);
            }
        }
        for _ in 0..8 {
            let src = w.clone();
            par_chunks(&mut w, nd * 512, |chunk, rows| {
                for (r, row) in rows.chunks_exact_mut(nd).enumerate() {
                    let vi = chunk * 512 + r;
                    let nb = &nbrs[vi];
                    if nb.is_empty() {
                        continue;
                    }
                    let inv = 0.5 / nb.len() as f32;
                    for (k, x) in row.iter_mut().enumerate() {
                        let avg: f32 = nb.iter().map(|&j| src[j as usize * nd + k]).sum();
                        *x = 0.5 * *x + avg * inv;
                    }
                }
            });
        }
    }

    if clay.blobs.iter().any(|b| b.op == crate::clay::Op::Plate && !b.bone.is_empty()) {
        for (vertex, row) in mesh.vertices.iter().zip(w.chunks_exact_mut(nd)) {
            if let Some(slot) = field.plate_bone(Vec3::from(vertex.pos)).and_then(|name| rig.bone_index(name)).and_then(|b| deform.iter().position(|&d| d == b)) {
                row.fill(0.0);
                row[slot] = 1.0;
            }
        }
    }
    paint_weights(mesh, rig, &deform, &mut w);

    // Keep the 4 strongest and normalize.
    let mut joints = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for row in w.chunks_exact(nd) {
        let mut best: Vec<(usize, f32)> = row.iter().copied().enumerate().filter(|(_, x)| *x > 1e-4).collect();
        best.sort_by(|a, b| b.1.total_cmp(&a.1));
        best.truncate(INFLUENCES);
        let sum: f32 = best.iter().map(|b| b.1).sum();
        // Unused slots point at bone 0 with weight 0, as glTF asks.
        let mut j = [0u16; INFLUENCES];
        let mut wt = [0.0; INFLUENCES];
        if sum <= 0.0 {
            j[0] = deform[0] as u16;
            wt[0] = 1.0;
        }
        for (i, (slot, x)) in best.into_iter().enumerate() {
            j[i] = deform[slot] as u16;
            wt[i] = if sum > 0.0 { x / sum } else { 0.0 };
        }
        joints.push(j);
        weights.push(wt);
    }
    Skin { joints, weights }
}

/// Paint each stroke over the previous result; normalize before the next one.
fn paint_weights(mesh: &Mesh, rig: &Rig, deform: &[usize], w: &mut [f32]) {
    let nd = deform.len();
    let mut neighbors = vec![Vec::new(); mesh.vertices.len()];
    if rig.weights.iter().any(|s| s.mode == WeightMode::Smooth) {
        for t in mesh.indices.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                neighbors[a as usize].push(b as usize);
                neighbors[b as usize].push(a as usize);
            }
        }
    }
    for stroke in rig.weights.iter().filter(|s| s.valid()) {
        let Some(slot) = rig.bone_index(&stroke.bone).and_then(|b| deform.iter().position(|&d| d == b)) else { continue };
        let src = (stroke.mode == WeightMode::Smooth).then(|| w.to_vec());
        for (i, row) in w.chunks_exact_mut(nd).enumerate() {
            let falloff = stroke.falloff(Vec3::from(mesh.vertices[i].pos));
            if falloff == 0.0 { continue; }
            let old = row[slot];
            let next = match stroke.mode {
                WeightMode::Add => (old + stroke.strength * falloff).min(1.0),
                WeightMode::Subtract => (old - stroke.strength * falloff).max(0.0),
                WeightMode::Replace => old + (stroke.strength - old) * falloff,
                WeightMode::Smooth => {
                    let nb = &neighbors[i];
                    if nb.is_empty() { continue; }
                    let src = src.as_ref().expect("smooth snapshot");
                    let avg = nb.iter().map(|&j| src[j * nd + slot]).sum::<f32>() / nb.len() as f32;
                    old + (avg - old) * stroke.strength * falloff
                }
            };
            let rest: f32 = row.iter().enumerate().filter(|(k, _)| *k != slot).map(|(_, x)| *x).sum();
            if rest > 1e-8 {
                for (k, x) in row.iter_mut().enumerate() {
                    if k != slot { *x *= (1.0 - next) / rest; }
                }
            } else if next < 1.0 {
                // Removing the sole influence gives the remainder to the
                // nearest other bone rather than leaving an unweighted vertex.
                let p = Vec3::from(mesh.vertices[i].pos);
                let other = deform.iter().enumerate().filter(|(k, _)| *k != slot).min_by(|(_, a), (_, b)| {
                    let distance = |b: usize| p.distance_squared(closest_on_segment(p, rig.bones[b].head, rig.bones[b].tail));
                    distance(**a).total_cmp(&distance(**b))
                }).map(|(k, _)| k);
                if let Some(k) = other { row[k] = 1.0 - next; } else { continue; }
            }
            row[slot] = next;
        }
    }
}

/// A split rig's mesh and skin: one closed shell per group of blobs that
/// move with the same bones, each weighted only to those bones.
///
/// Surfaces that were buried inside another part (the body under an arm)
/// were never seen or painted, so they take the color of the nearest
/// surface of `whole` (the clay as one shell) that covered them, and its
/// material. `library` and `fallback` are as in `Clay::build_mesh_with`.
/// Also returns each part's first vertex and the prims that make it.
pub fn build_parts(clay: &Clay, rig: &Rig, whole: &Mesh, library: &[Material], fallback: &str) -> (Mesh, Skin, Vec<(u32, Vec<bool>)>) {
    let cell = clay.cell_size().unwrap_or(0.02);
    let field = Field::new(clay, cell);
    let np = field.prim_count();
    let deform: Vec<usize> = (0..rig.bones.len()).filter(|&i| rig.bones[i].deform).collect();
    if np == 0 || deform.is_empty() {
        let mesh = clay.build_mesh_with(library, fallback);
        let skin = skin(clay, &mesh, rig);
        return (mesh, skin, Vec::new());
    }
    // Group blobs by the blob whose bones move them: a bone's home blob
    // leads its own group; a patch on it (a belly) joins the home blob of
    // the bones it holds; a blob with no bone (a nose) joins the nearest
    // home blob.
    let (slots, homes) = blob_homes(&field, rig, &deform);
    let is_home: Vec<bool> = (0..np).map(|i| homes.contains(&Some(i))).collect();
    let leaders: Vec<usize> = (0..np).filter(|&i| is_home[i]).collect();
    let leader = |i: usize| -> Option<usize> {
        if is_home[i] {
            return Some(i);
        }
        if let Some(first) = slots[i].as_ref().and_then(|v| v.first())
            && let Some(h) = homes[*first]
        {
            return Some(h);
        }
        let c = field.blob_center(i);
        leaders.iter().copied().min_by(|&a, &b| field.blob_box(a, c).0.total_cmp(&field.blob_box(b, c).0))
    };
    let bones: Vec<Option<Vec<usize>>> = (0..np)
        .map(|i| if field.is_carve(i) { None } else if let Some(b) = rig.bone_index(field.blob_bone(i)) { Some(vec![b]) } else { leader(i).and_then(|l| slots[l].as_ref()).map(|v| v.iter().map(|&k| deform[k]).collect()) })
        .collect();
    let mut groups: Vec<(Vec<usize>, Vec<bool>)> = Vec::new();
    for i in (0..np).filter(|&i| !field.is_carve(i)) {
        let mut key = bones[i].clone().unwrap_or_default();
        key.sort_unstable();
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1[i] = true,
            None => {
                let mut keep = vec![false; np];
                keep[i] = true;
                groups.push((key, keep));
            }
        }
    }
    let mut mesh = Mesh::default();
    let mut skin = Skin::default();
    let mut parts = Vec::new();
    let list = field.bounds().map(|(lo, hi)| field.near(lo, hi)).unwrap_or_default();
    let nearest = NearestVertex::new(whole, cell * 2.0);
    // Each part's build fills its share of what is left of the bar.
    let (start, count) = (crate::clay::progress_fraction().unwrap_or(0.0), groups.len().max(1) as f32);
    for (n, (key, mut keep)) in groups.into_iter().enumerate() {
        let step = (1.0 - start) / count;
        crate::clay::progress_range(start + step * n as f32, start + step * (n + 1) as f32);
        // Carving cuts every part.
        for i in (0..np).filter(|&i| field.is_carve(i)) {
            keep[i] = true;
        }
        parts.push((mesh.vertices.len() as u32, keep.clone()));
        let mut part = clay.build_part(Some(keep), library, fallback);
        par_chunks(&mut part.vertices, 1024, |_, verts| {
            for v in verts {
                let p = Vec3::from(v.pos);
                if field.sample(p, &list).0 < -0.5 * cell
                    && let Some(near) = nearest.vertex(p)
                {
                    v.color = near.color;
                    v.surface = near.surface;
                    v.material = near.material;
                    v.light = near.light;
                    (v.base, v.wear, v.painted) = (near.base, near.wear, near.painted);
                }
            }
        });
        let s = skin_with(clay, &part, rig, (!key.is_empty()).then_some(key.as_slice()));
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend_from_slice(&part.vertices);
        mesh.indices.extend(part.indices.iter().map(|i| i + base));
        mesh.edges.extend(part.edges.iter().map(|i| i + base));
        // Every part resolves the same palette, so indices agree.
        mesh.materials = part.materials;
        mesh.decals = part.decals;
        mesh.face = part.face;
        skin.joints.extend(s.joints);
        skin.weights.extend(s.weights);
    }
    (mesh, skin, parts)
}

/// Finds the nearest vertex of a mesh, through a hash grid.
pub(crate) struct NearestVertex<'a> {
    mesh: &'a Mesh,
    size: f32,
    grid: std::collections::HashMap<[i32; 3], Vec<u32>>,
}

impl<'a> NearestVertex<'a> {
    pub(crate) fn new(mesh: &'a Mesh, size: f32) -> Self {
        let mut grid: std::collections::HashMap<[i32; 3], Vec<u32>> = std::collections::HashMap::new();
        for (i, v) in mesh.vertices.iter().enumerate() {
            grid.entry(Self::key(Vec3::from(v.pos), size)).or_default().push(i as u32);
        }
        Self { mesh, size, grid }
    }

    fn key(p: Vec3, size: f32) -> [i32; 3] {
        (p / size).floor().as_ivec3().to_array()
    }

    /// The nearest vertex, searching outward ring by ring.
    pub(crate) fn vertex(&self, p: Vec3) -> Option<&'a Vertex> {
        self.index(p).map(|i| &self.mesh.vertices[i])
    }

    /// Index of the nearest vertex.
    pub(crate) fn index(&self, p: Vec3) -> Option<usize> {
        let c = Self::key(p, self.size);
        let mut best: Option<(f32, u32)> = None;
        for r in 0i32..24 {
            for dz in -r..=r {
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != r {
                            continue;
                        }
                        for &i in self.grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]).into_iter().flatten() {
                            let d = Vec3::from(self.mesh.vertices[i as usize].pos).distance_squared(p);
                            if best.is_none_or(|(b, _)| d < b) {
                                best = Some((d, i));
                            }
                        }
                    }
                }
            }
            // Anything in a farther ring is at least r cells away.
            if let Some((d, i)) = best
                && d.sqrt() <= r as f32 * self.size
            {
                return Some(i as usize);
            }
        }
        best.map(|(_, i)| i as usize)
    }
}

/// The blob a point belongs to: the `add` blob nearest to (or deepest at) it.
fn owner_blob(field: &Field, p: Vec3) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for i in 0..field.prim_count() {
        if best.is_some_and(|(_, d)| field.blob_box(i, p).0 > d) {
            continue;
        }
        if let Some(d) = field.blob_distance(i, p)
            && best.is_none_or(|(_, b)| d < b)
        {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Which deform bones (as slots in `deform`) may move each blob's vertices.
///
/// A bone's home is the blob that holds most of its length (the biggest blob
/// on a tie, so a spine belongs to the body, not to a belly patch on it).
/// A blob is moved by the bones whose home it is; a blob that is no bone's
/// home (a belly patch) by the bones that lie mostly inside it; and a blob
/// with no bone at all (a nose) is left to plain distance (None).
fn blob_bones(field: &Field, rig: &Rig, deform: &[usize]) -> Vec<Option<Vec<usize>>> {
    blob_homes(field, rig, deform).0
}

/// `blob_bones`, plus each deform slot's home blob.
fn blob_homes(field: &Field, rig: &Rig, deform: &[usize]) -> (Vec<Option<Vec<usize>>>, Vec<Option<usize>>) {
    const SAMPLES: usize = 16;
    let np = field.prim_count();
    // inside[slot][blob] = share of the bone's length inside the blob.
    let inside: Vec<Vec<f32>> = deform
        .iter()
        .map(|&b| {
            let (a, e) = (rig.bones[b].head, rig.bones[b].tail);
            (0..np)
                .map(|i| {
                    let n = (0..=SAMPLES).filter(|s| field.blob_distance(i, a.lerp(e, *s as f32 / SAMPLES as f32)).is_some_and(|d| d < 0.0)).count();
                    n as f32 / (SAMPLES + 1) as f32
                })
                .collect()
        })
        .collect();
    let volume = |i: usize| field.blob_box(i, Vec3::ZERO).1;
    let homes: Vec<Option<usize>> = inside
        .iter()
        .map(|row| {
            let max = row.iter().copied().fold(0.0, f32::max);
            (max > 0.0).then(|| (0..np).filter(|&i| row[i] >= max - 0.05).max_by(|&a, &b| volume(a).total_cmp(&volume(b))).expect("max exists"))
        })
        .collect();
    let allowed = (0..np)
        .map(|i| {
            if let Some(slot) = rig.bone_index(field.blob_bone(i)).and_then(|b| deform.iter().position(|&d| d == b)) {
                return Some(vec![slot]);
            }
            let own: Vec<usize> = (0..deform.len()).filter(|&s| homes[s] == Some(i)).collect();
            if !own.is_empty() {
                return Some(own);
            }
            let within: Vec<usize> = (0..deform.len()).filter(|&s| inside[s][i] >= 0.5).collect();
            (!within.is_empty()).then_some(within)
        })
        .collect();
    (allowed, homes)
}

/// The rest mesh moved by the skinning matrices (linear blend skinning).
pub fn pose_mesh(rest: &Mesh, skin: &Skin, mats: &[Mat4]) -> Mesh {
    let mut vertices = rest.vertices.clone();
    if skin.joints.len() == vertices.len() {
        par_chunks(&mut vertices, 4096, |chunk, verts| {
            for (i, v) in verts.iter_mut().enumerate() {
                let vi = chunk * 4096 + i;
                let (j, w) = (skin.joints[vi], skin.weights[vi]);
                let mut m = Mat4::ZERO;
                for k in 0..INFLUENCES {
                    if w[k] > 0.0 {
                        m += mats.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
                    }
                }
                v.pos = m.transform_point3(Vec3::from(v.pos)).into();
                v.normal = m.transform_vector3(Vec3::from(v.normal)).normalize_or_zero().into();
                if v.tangent[3] != 0.0 {
                    let t = m.transform_vector3(Vec3::from_slice(&v.tangent)).normalize_or_zero();
                    v.tangent = [t.x, t.y, t.z, v.tangent[3]];
                }
            }
        });
    }
    Mesh { vertices, indices: rest.indices.clone(), edges: rest.edges.clone(), materials: rest.materials.clone(), textures: rest.textures.clone(), imported: rest.imported.clone(), decals: rest.decals.clone(), face: rest.face.clone(), unshaded: None }
}

/// Colors a mesh by one bone's weight: blue (none) to red (full).
pub fn weight_colors(mesh: &mut Mesh, skin: &Skin, bone: usize) {
    for (vi, v) in mesh.vertices.iter_mut().enumerate() {
        let (Some(j), Some(w)) = (skin.joints.get(vi), skin.weights.get(vi)) else { continue };
        let x: f32 = (0..INFLUENCES).filter(|&k| j[k] as usize == bone).map(|k| w[k]).sum();
        let c = if x < 0.5 { Vec3::new(0.1, 0.2, 0.9).lerp(Vec3::new(0.1, 0.9, 0.2), x * 2.0) } else { Vec3::new(0.1, 0.9, 0.2).lerp(Vec3::new(0.95, 0.15, 0.1), x * 2.0 - 1.0) };
        v.color = c.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clay::{Blob, Form};

    fn arm() -> (Clay, Rig) {
        let blob = Blob {
            name: String::new(),
            form: Form::Capsule { a: Vec3::ZERO, b: Vec3::new(2.0, 0.0, 0.0), radius: 0.25, radius_b: None },
            op: Default::default(),
            blend: 0.0,
            color: [0.8; 3],
            mirror: false, flip: false,
            material: String::new(),
            bone: String::new(),
            color_blend: None,
            smooth: 0.0,
            offset: 0.004,
            join: Default::default(), join_size: None, wall: 0.004, rim: 0.0, ridge: 0.0, flare: 0.0,
        };
        let clay = Clay { blobs: vec![blob], resolution: 48, ..Default::default() };
        let bone = |name: &str, parent: Option<&str>, a: f32, b: f32| Bone {
            name: name.into(),
            parent: parent.map(Into::into),
            head: Vec3::new(a, 0.0, 0.0),
            tail: Vec3::new(b, 0.0, 0.0),
            deform: true,
        };
        let rig = Rig { bones: vec![bone("upper", None, 0.0, 1.0), bone("lower", Some("upper"), 1.0, 2.0)], ..Default::default() };
        (clay, rig)
    }

    #[test]
    fn weights_follow_the_nearest_bone() {
        let (clay, rig) = arm();
        let mesh = clay.build_mesh();
        let s = skin(&clay, &mesh, &rig);
        let counts = s.counts(2);
        assert!(counts[0] > 50 && counts[1] > 50, "{counts:?}");
        for (v, (j, w)) in mesh.vertices.iter().zip(s.joints.iter().zip(&s.weights)) {
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-3);
            if v.pos[0] > 1.7 {
                assert_eq!(j[0], 1, "tip follows the lower bone");
            }
            if v.pos[0] < 0.3 {
                assert_eq!(j[0], 0, "root follows the upper bone");
            }
        }
    }

    #[test]
    fn bending_the_elbow_moves_the_tip() {
        let (clay, mut rig) = arm();
        let mesh = clay.build_mesh();
        let s = skin(&clay, &mesh, &rig);
        let mut clip = Clip::new("bend");
        let mut k = Key::new(1.0);
        k.rotate = Vec3::new(0.0, 0.0, 90.0);
        clip.track_mut("lower").insert(k);
        rig.clips.push(clip);
        rig.active = Some("bend".into());
        let posed = pose_mesh(&mesh, &s, &rig.skin_matrices(Some(1.0)));
        let (_, hi) = posed.bounds();
        // The lower bone now points up: the tip sits near (1, 1).
        assert!(hi.x < 1.5 && hi.y > 1.0, "{hi:?}");
        let (_, tail) = rig.joints(Some(1.0))[1];
        assert!(tail.distance(Vec3::new(1.0, 1.0, 0.0)) < 1e-4);
        // Rest pose is unchanged.
        let rest = pose_mesh(&mesh, &s, &rig.skin_matrices(None));
        assert_eq!(rest.bounds(), mesh.bounds());
    }

    #[test]
    fn an_embedded_arm_does_not_take_the_body() {
        let blob = |form| Blob { name: String::new(), form, op: Default::default(), blend: 0.05, color: [0.8; 3], mirror: false, flip: false, material: String::new(), bone: String::new(), color_blend: None, join: Default::default(), join_size: None, wall: 0.004, rim: 0.0, ridge: 0.0, flare: 0.0, smooth: 0.0, offset: 0.004 };
        let clay = Clay {
            blobs: vec![
                blob(Form::Sphere { center: Vec3::ZERO, radius: 0.5 }),
                blob(Form::Capsule { a: Vec3::new(0.3, 0.0, 0.0), b: Vec3::new(1.0, 0.0, 0.0), radius: 0.15, radius_b: None }),
            ],
            resolution: 48,
            ..Default::default()
        };
        let bone = |name: &str, parent: Option<&str>, head: Vec3, tail: Vec3| Bone { name: name.into(), parent: parent.map(Into::into), head, tail, deform: true };
        let rig = Rig {
            bones: vec![bone("spine", None, Vec3::new(0.0, -0.4, 0.0), Vec3::new(0.0, 0.4, 0.0)), bone("arm", Some("spine"), Vec3::new(0.35, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0))],
            ..Default::default()
        };
        let mesh = clay.build_mesh();
        let s = skin(&clay, &mesh, &rig);
        for (v, (j, w)) in mesh.vertices.iter().zip(s.joints.iter().zip(&s.weights)) {
            let p = Vec3::from(v.pos);
            let from_arm_axis = Vec3::new(0.0, p.y, p.z).length();
            // Body surface clearly away from where the arm joins.
            if p.x < 0.45 && from_arm_axis > 0.3 {
                assert!(j[0] == 0 && w[0] > 0.6, "body vertex {p:?} follows bone {} ({})", j[0], w[0]);
            }
            if p.x > 0.75 {
                assert_eq!(j[0], 1, "arm vertex {p:?}");
            }
        }
    }

    #[test]
    fn a_split_rig_keeps_the_body_whole_when_the_arm_swings_away() {
        let blob = |form| Blob { name: String::new(), form, op: Default::default(), blend: 0.1, color: [0.8; 3], mirror: false, flip: false, material: String::new(), bone: String::new(), color_blend: None, join: Default::default(), join_size: None, wall: 0.004, rim: 0.0, ridge: 0.0, flare: 0.0, smooth: 0.0, offset: 0.004 };
        let clay = Clay {
            blobs: vec![
                blob(Form::Sphere { center: Vec3::ZERO, radius: 0.5 }),
                blob(Form::Capsule { a: Vec3::new(0.4, 0.2, 0.0), b: Vec3::new(0.5, -0.4, 0.0), radius: 0.15, radius_b: None }),
            ],
            resolution: 48,
            ..Default::default()
        };
        let bone = |name: &str, parent: Option<&str>, head: Vec3, tail: Vec3| Bone { name: name.into(), parent: parent.map(Into::into), head, tail, deform: true };
        let mut clip = Clip::new("raise");
        let mut k = Key::new(1.0);
        k.rotate = Vec3::new(0.0, 0.0, 150.0);
        clip.track_mut("arm").insert(k);
        let mut rig = Rig {
            bones: vec![bone("spine", None, Vec3::new(0.0, -0.4, 0.0), Vec3::new(0.0, 0.4, 0.0)), bone("arm", Some("spine"), Vec3::new(0.4, 0.2, 0.0), Vec3::new(0.5, -0.4, 0.0))],
            clips: vec![clip],
            active: Some("raise".into()),
            split: true,
            weights: Vec::new(),
        };
        // Where the arm was melted in, the body's own surface must still be there.
        let hole = Vec3::new(0.5, -0.1, 0.0);
        let gap = |m: &Mesh| m.vertices.iter().map(|v| Vec3::from(v.pos).distance(hole)).fold(f32::MAX, f32::min);
        let (mesh, s, _) = build_parts(&clay, &rig, &clay.build_mesh(), &[], "");
        assert_eq!(s.joints.len(), mesh.vertices.len());
        let posed = pose_mesh(&mesh, &s, &rig.skin_matrices(Some(1.0)));
        assert!(gap(&posed) < 0.05, "split: nearest surface {}", gap(&posed));
        // One melted shell leaves a dent there.
        rig.split = false;
        let whole = clay.build_mesh();
        let ws = skin(&clay, &whole, &rig);
        let dented = pose_mesh(&whole, &ws, &rig.skin_matrices(Some(1.0)));
        assert!(gap(&dented) > gap(&posed), "whole {} vs split {}", gap(&dented), gap(&posed));
    }

    fn weight(s: &Skin, i: usize, bone: usize) -> f32 {
        s.joints[i].iter().zip(s.weights[i]).filter(|(j, _)| **j as usize == bone).map(|(_, w)| w).sum()
    }

    #[test]
    fn blob_bone_override_owns_the_surface_and_split_part() {
        let (mut clay, mut rig) = arm();
        clay.blobs[0].bone = "upper".into();
        let rest = clay.build_mesh();
        let s = skin(&clay, &rest, &rig);
        assert!(s.weights.iter().all(|w| w[0] == 1.0));
        assert!(s.joints.iter().all(|j| j[0] == 0));
        rig.split = true;
        let (_, split, _) = build_parts(&clay, &rig, &rest, &[], "");
        assert!(split.joints.iter().all(|j| j[0] == 0));
    }

    #[test]
    fn painted_weights_are_local_and_survive_rebuild_and_save() {
        let (mut clay, mut rig) = arm();
        rig.split = false;
        let rest = clay.build_mesh();
        let auto = skin(&clay, &rest, &rig);
        let stroke = WeightStroke { bone: "upper".into(), points: vec![Vec3::new(1.8,0.25,0.0)], radius: 0.3, strength: 0.9, mode: WeightMode::Replace, space: Mat4::IDENTITY };
        rig.weights.push(stroke.clone());
        let painted = skin(&clay, &rest, &rig);
        let mut changed = 0;
        for (i, v) in rest.vertices.iter().enumerate() {
            let p = Vec3::from(v.pos);
            if stroke.falloff(p) == 0.0 {
                assert_eq!(auto.joints[i], painted.joints[i]);
                assert_eq!(auto.weights[i], painted.weights[i]);
            } else if weight(&painted, i, 0) > weight(&auto, i, 0) + 0.1 { changed += 1; }
            assert!((painted.weights[i].iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
        assert!(changed > 5);
        let saved: Rig = serde_json::from_str(&serde_json::to_string(&rig).unwrap()).unwrap();
        assert_eq!(saved, rig);
        assert_eq!(skin(&clay, &rest, &saved).weights, painted.weights);
        clay.resolution = 64;
        let rebuilt = clay.build_mesh();
        let new_auto = skin(&clay, &rebuilt, &Rig { weights: Vec::new(), ..rig.clone() });
        let new_paint = skin(&clay, &rebuilt, &saved);
        assert!(rebuilt.vertices.iter().enumerate().any(|(i, v)| stroke.falloff(Vec3::from(v.pos)) > 0.8 && weight(&new_paint,i,0) > weight(&new_auto,i,0) + 0.5));
    }

    #[test]
    fn weight_modes_use_paths_falloff_and_four_normalized_influences() {
        let (clay, mut rig) = arm();
        let mesh = clay.build_mesh();
        let stroke = WeightStroke { bone: "upper".into(), points: vec![Vec3::new(1.5,0.25,-0.2), Vec3::new(1.5,0.25,0.2)], radius: 0.3, strength: 0.7, mode: WeightMode::Add, space: Mat4::IDENTITY };
        assert!(stroke.falloff(Vec3::new(1.5,0.25,0.0)) > 0.99);
        // The core: full effect out to half the radius, nothing at the radius.
        assert_eq!(stroke.falloff(Vec3::new(1.5 + 0.14,0.25,0.2)), 1.0);
        assert!(stroke.falloff(Vec3::new(1.5 + 0.3,0.25,0.2)) < 1e-6);
        for mode in [WeightMode::Add, WeightMode::Subtract, WeightMode::Replace, WeightMode::Smooth] {
            let mut s = stroke.clone();
            s.mode = mode;
            rig.weights = vec![s];
            let painted = skin(&clay, &mesh, &rig);
            assert!(painted.weights.iter().all(|w| w.iter().all(|x| x.is_finite() && *x >= 0.0) && (w.iter().sum::<f32>() - 1.0).abs() < 1e-6));
        }
        let scaled = WeightStroke { space: Mat4::from_scale(Vec3::new(2.0,1.0,1.0)), ..stroke };
        assert_eq!(scaled.falloff(Vec3::new(1.7,0.25,0.0)), 0.0);
    }

    #[test]
    #[ignore]
    fn wing_check() {
        use crate::film::{self, Camera, Item, Settings};
        use std::sync::Arc;
        let clay: Clay = serde_json::from_value(serde_json::json!({"resolution":128,"blobs":[
            {"type":"capsule","a":[0,-0.6,0],"b":[0,0.8,0],"radius":0.22,"bone":"spine","color":[0.25,0.35,0.3],"blend":0.02},
            {"type":"capsule","a":[0,0.6,0],"b":[1.2,1,0],"radius":0.09,"bone":"arm","color":[0.28,0.4,0.32],"blend":0.02},
            {"type":"capsule","a":[1.2,1,0],"b":[2.6,0.8,0],"radius":0.065,"bone":"finger1","color":[0.28,0.4,0.32],"blend":0.02},
            {"type":"capsule","a":[1.2,1,0],"b":[1.8,-0.65,0],"radius":0.055,"bone":"finger2","color":[0.28,0.4,0.32],"blend":0.02},
            {"type":"membrane","name":"wing","points":[[0,0.6,0],[1.2,1,0],[2.6,0.8,0],[1.8,-0.65,0],[0,-0.5,0]],"thickness":0.065,"sag":0.17,"scallop":0.13,"scallop_edges":[2,3],"color":[0.62,0.29,0.15],"blend":0.025,"bone":"finger2"}
        ]})).unwrap();
        let bone = |name: &str, parent: Option<&str>, head: [f32;3], tail: [f32;3]| Bone { name:name.into(), parent:parent.map(Into::into), head:head.into(),tail:tail.into(),deform:true };
        let mut rig = Rig { bones:vec![bone("spine",None,[0.,-0.6,0.],[0.,0.6,0.]),bone("arm",Some("spine"),[0.,0.6,0.],[1.2,1.,0.]),bone("finger1",Some("arm"),[1.2,1.,0.],[2.6,0.8,0.]),bone("finger2",Some("arm"),[1.2,1.,0.],[1.8,-0.65,0.])],split:false,..Default::default() };
        let rest = clay.build_mesh();
        for (bone, strength, radius) in [("arm",0.55,0.95),("spine",0.75,0.55)] {
            rig.weights.push(WeightStroke {bone:bone.into(),points:vec![Vec3::new(0.1,-0.45,0.),Vec3::new(0.1,0.55,0.)],radius,strength,mode:WeightMode::Replace,space:Mat4::IDENTITY});
        }
        let skin = skin(&clay,&rest,&rig);
        let mut clip = Clip::new("flap");
        let mut key = Key::new(1.0);
        key.rotate = Vec3::new(-30.0,35.0,-15.0);
        clip.track_mut("arm").insert(key);
        rig.clips.push(clip);
        rig.active = Some("flap".into());
        let posed = pose_mesh(&rest,&skin,&rig.skin_matrices(Some(1.0)));
        let mut colored = rest.clone();
        for (i,v) in colored.vertices.iter_mut().enumerate() {
            let w = weight(&skin,i,0);
            v.color = [(2.0*w-1.0).max(0.0),1.0-(2.0*w-1.0).abs(),(1.0-2.0*w).max(0.0)];
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wing_check");
        std::fs::create_dir_all(&dir).unwrap();
        let cam = Camera {eye:Vec3::new(3.,2.,5.),target:Vec3::new(1.,0.2,0.),up:Vec3::Y,fov_deg:38.0,ortho:None};
        for (name,mesh) in [("rest",rest),("posed",posed),("spine_weights",colored)] {
            let mat = Material::default();
            let img = film::render(vec![Item {mesh:Arc::new(mesh),world:Mat4::IDENTITY,tint:[1.;3],surface:mat.surface(),material:mat}],&[],&cam,Settings {width:640,height:480,samples:32,bounces:3,time_limit:std::time::Duration::from_secs(40),floor:false,exposure:1.0,denoise:true},&Default::default());
            image::save_buffer(dir.join(format!("{name}.png")),&img.pixels.iter().flatten().copied().collect::<Vec<_>>(),img.width as u32,img.height as u32,image::ColorType::Rgb8).unwrap();
        }
    }

    #[test]
    fn keys_interpolate_and_loop() {
        let mut clip = Clip::new("wave");
        let mut a = Key::new(1.0);
        a.interp = Interp::Linear;
        let mut b = Key::new(11.0);
        b.translate = Vec3::new(0.0, 1.0, 0.0);
        clip.track_mut("x").insert(a);
        clip.track_mut("x").insert(b);
        let t = clip.track("x").unwrap();
        assert!((t.sample(6.0).translate.y - 0.5).abs() < 1e-5);
        assert_eq!(t.sample(-5.0).translate.y, 0.0);
        assert_eq!(clip.range(), Some((1.0, 11.0)));
        // Frame 16 wraps to 6.
        assert!((clip.local_frame(16.0) - 6.0).abs() < 1e-5);
    }

    #[test]
    fn sanitize_orders_parents_and_drops_junk() {
        let b = |name: &str, parent: Option<&str>| Bone { name: name.into(), parent: parent.map(Into::into), head: Vec3::ZERO, tail: Vec3::Y, deform: true };
        let mut rig = Rig {
            bones: vec![b("hand", Some("arm")), b("arm", Some("hand")), b("arm", None), b("x", Some("nope"))],
            clips: vec![Clip { name: "c".into(), looped: true, tracks: vec![Track { bone: "ghost".into(), keys: vec![Key::new(1.0)] }] }],
            active: Some("missing".into()),
            split: true,
            weights: Vec::new(),
        };
        rig.sanitize();
        assert_eq!(rig.bones.len(), 3);
        // Every parent comes before its child.
        for (i, bone) in rig.bones.iter().enumerate() {
            if let Some(p) = &bone.parent {
                assert!(rig.bones[..i].iter().any(|x| &x.name == p));
            }
        }
        assert!(rig.clips[0].tracks.is_empty());
        assert!(rig.active.is_none());
        assert_eq!(mirror_name("arm.L").as_deref(), Some("arm.R"));
        assert_eq!(mirror_name("legRight").as_deref(), Some("legLeft"));
        assert_eq!(mirror_name("spine"), None);
    }
}

