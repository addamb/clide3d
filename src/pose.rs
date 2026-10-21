//! Mirroring, aligning and posing objects, and things that follow other
//! things.
//!
//! - `mirror`: a copy reflected across a plane. Its geometry is reflected
//!   (clay blobs flip and sculpt, paint and decals mirror; symmetric
//!   primitives just turn), it keeps a pivot of its own (the reflected
//!   one), swaps `.L`/`.R` names and bones, keeps the materials, and takes
//!   its children and the kitbash parts sitting on it along. A *linked*
//!   copy carries a `Link` and `sync` keeps it the reflection of its
//!   source: blob edits, materials, moves and new detail passes.
//! - `Follow`: a kitbash group follows the object its parts sit on, so
//!   moving, aligning, re-posing or mirroring that object takes its parts.
//! - `axes_rotation`, `bone_frame`, `object_frame`: orienting by two
//!   vectors, or in the frame of a bone or object (`align`, `grip`).
//! - `pose_rest`: moves a bone's rest pose and carries what is attached.
//!
//! Everything works in the rest pose (bones at rest), where attached
//! objects are placed; clips then move them with the rig.

use std::collections::{HashMap, HashSet};

use glam::{EulerRot, Mat3, Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::clay::{Clay, Form};
use crate::rig::{Rig, mirror_name};
use crate::scene::{Object, Scene, Shape};

/// A kitbash group follows the object its parts sit on.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Follow {
    pub object: u64,
    /// The group's rest world matrix in that object's rest space.
    pub offset: Mat4,
}

/// A linked mirror copy: the reflection of `source`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Link {
    pub source: u64,
    /// The mirror plane (unit normal, n . p = d) in the source's root
    /// space (its topmost ancestor at rest; world for top-level objects),
    /// so a mirrored mech half stays mirrored when the whole mech moves.
    pub normal: Vec3,
    pub d: f32,
    /// The copy's own axis its geometry is reflected in (0 = X); None: it
    /// keeps the source's geometry and its matrix reflects (negative scale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<u8>,
    /// The source clay the copy's clay was made from (not saved), so a
    /// big sculpted clay is mirrored again only when its source changes.
    #[serde(skip)]
    pub from: Option<Box<Clay>>,
}

impl Link {
    pub fn valid(&self) -> bool {
        self.normal.is_finite() && (self.normal.length() - 1.0).abs() < 1e-3 && self.d.is_finite() && self.axis.is_none_or(|a| a < 3)
    }
}

/// The reflection across the plane n . p = d (n unit).
pub fn reflection(n: Vec3, d: f32) -> Mat4 {
    let m = Mat3::IDENTITY - Mat3::from_cols(n * n.x, n * n.y, n * n.z) * 2.0;
    Mat4::from_cols(m.x_axis.extend(0.0), m.y_axis.extend(0.0), m.z_axis.extend(0.0), (n * (2.0 * d)).extend(1.0))
}

/// A reflection of one local axis (None: identity).
pub fn axis_flip(axis: Option<u8>) -> Mat4 {
    match axis {
        Some(a) => {
            let mut s = Vec3::ONE;
            s[a as usize] = -1.0;
            Mat4::from_scale(s)
        }
        None => Mat4::IDENTITY,
    }
}

fn close(a: Mat4, b: Mat4) -> bool {
    let scale = a.to_cols_array().iter().chain(b.to_cols_array().iter()).fold(1.0f32, |m, v| m.max(v.abs()));
    a.to_cols_array().iter().zip(b.to_cols_array().iter()).all(|(x, y)| (x - y).abs() <= 1e-5 * scale)
}

/// Mirrors a decal across its object's x = 0.
pub fn mirror_decal_x(d: &mut crate::smart::Decal) {
    d.center[0] = -d.center[0];
    d.normal[0] = -d.normal[0];
    if let Some(h) = &mut d.hinge {
        h[0][0] = -h[0][0];
        h[1][0] = -h[1][0];
    }
    d.rotate = -d.rotate;
}

/// The other side's name: `arm.L` -> `arm.R` (and `_L`/`_R`, Left/Right,
/// also inside a name: `bolts_arm.L_copies`), or `name_mirror`.
pub fn side_name(name: &str) -> String {
    swap_side_token(name).unwrap_or_else(|| format!("{name}_mirror"))
}

/// Swaps the last side token in a name, if it has one.
pub fn swap_side_token(name: &str) -> Option<String> {
    if let Some(m) = mirror_name(name) {
        return Some(m);
    }
    const PAIRS: [(&str, &str); 6] = [(".L", ".R"), ("_L", "_R"), (".l", ".r"), ("_l", "_r"), ("Left", "Right"), ("left", "right")];
    let mut best: Option<(usize, &str, &str)> = None;
    for (a, b) in PAIRS {
        for (from, to) in [(a, b), (b, a)] {
            for (i, _) in name.match_indices(from) {
                let next = name[i + from.len()..].chars().next();
                if next.is_none_or(|c| !c.is_ascii_alphanumeric()) && best.is_none_or(|(j, f, _)| i > j || (i == j && from.len() > f.len())) {
                    best = Some((i, from, to));
                }
            }
        }
    }
    let (i, from, to) = best?;
    Some(format!("{}{}{}", &name[..i], to, &name[i + from.len()..]))
}

/// Runs `f` with every rig at rest.
pub fn at_rest<T>(scene: &mut Scene, f: impl FnOnce(&mut Scene) -> T) -> T {
    let keep = scene.frame.take();
    let out = f(scene);
    scene.frame = keep;
    out
}

/// The topmost ancestor of `id` (not `id` itself).
fn root_of(scene: &Scene, id: u64) -> Option<u64> {
    scene.ancestors(id).last().copied()
}

/// The world reflection of a link (rest pose).
fn link_world(scene: &Scene, source: u64, l: &Link) -> Mat4 {
    let r = root_of(scene, source).map_or(Mat4::IDENTITY, |r| scene.world_matrix(r));
    r * reflection(l.normal, l.d) * r.inverse()
}

/// A world plane in `id`'s root space.
fn plane_in_root(scene: &Scene, id: u64, n: Vec3, d: f32) -> (Vec3, f32) {
    let Some(r) = root_of(scene, id).map(|r| scene.world_matrix(r)) else { return (n, d) };
    let a = Mat3::from_mat4(r);
    let t = r.w_axis.truncate();
    let nl = a.transpose() * n;
    let k = nl.length().max(1e-12);
    (nl / k, (d - n.dot(t)) / k)
}

/// The axis an object's geometry can be reflected in exactly, if its
/// world matrix keeps a positive determinant that way.
fn choose_axis(o: &Object, world: Mat4) -> Option<u8> {
    if world.determinant() < 0.0 {
        return None;
    }
    match o.shape {
        Shape::Clay | Shape::Group | Shape::Scatter | Shape::Cube | Shape::Sphere | Shape::Cylinder | Shape::Cone | Shape::Plane | Shape::Torus | Shape::Eye => Some(0),
        Shape::Part if o.part.as_ref().is_some_and(|p| !p.points.is_empty()) => Some(0),
        _ => None,
    }
}

/// A rig mirrored across its object's x = 0: bones, names, clips, weights.
pub fn mirror_rig(r: &Rig) -> Rig {
    let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
    let swap = |n: &str| mirror_name(n).unwrap_or_else(|| n.to_string());
    let mut out = r.clone();
    for b in &mut out.bones {
        b.name = swap(&b.name);
        b.parent = b.parent.as_deref().map(swap);
        b.head = flip(b.head);
        b.tail = flip(b.tail);
    }
    for c in &mut out.clips {
        for t in &mut c.tracks {
            t.bone = swap(&t.bone);
            for k in &mut t.keys {
                *k = crate::rig::mirror_key(k);
            }
        }
    }
    let f = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    for w in &mut out.weights {
        w.bone = swap(&w.bone);
        w.points = w.points.iter().map(|p| flip(*p)).collect();
        w.space = f * w.space * f;
    }
    out
}

fn flip_axis(v: Vec3, axis: Option<u8>) -> Vec3 {
    match axis {
        Some(a) => {
            let mut v = v;
            v[a as usize] = -v[a as usize];
            v
        }
        None => v,
    }
}

/// Makes `x` the reflection of `s` (content, not placement).
fn mirror_content(scene: &Scene, s: &Object, x: &mut Object, axis: Option<u8>) {
    x.shape = s.shape;
    x.color = s.color;
    x.visible = s.visible;
    x.material = s.material.clone();
    x.kitbash = s.kitbash.clone();
    x.eye = s.eye.clone();
    x.hair = s.hair.clone();
    x.kit = s.kit.clone();
    x.terrain = s.terrain.clone();
    x.bake = s.bake;
    x.pivot = flip_axis(s.pivot, axis);
    x.damage = s.damage.clone();
    if axis == Some(0) {
        x.damage.iter_mut().for_each(mirror_decal_x);
    }
    x.light = s.light.clone().map(|mut l| {
        l.offset = flip_axis(l.offset, axis);
        l
    });
    x.rig = s.rig.as_ref().map(|r| if axis == Some(0) { mirror_rig(r) } else { r.clone() });
    x.part = s.part.clone().map(|mut p| {
        p.points = p.points.iter().map(|q| flip_axis(Vec3::from(*q), axis).to_array()).collect();
        if let Some(sp) = &mut p.span {
            sp.bones = sp.bones.clone().map(|b| side_name_or_same(&b));
        }
        p
    });
    x.scatter = s.scatter.clone().map(|mut sc| {
        let f = axis_flip(axis);
        // Lettering stays readable: its copy turns instead of reflecting.
        let text: Vec<bool> = sc.sources.iter().map(|&id| scene.get(id).and_then(|o| o.part.as_ref()).is_some_and(|p| p.text.is_some())).collect();
        for inst in &mut sc.instances {
            let keep = if text.get(inst.source as usize).copied().unwrap_or(false) { Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)) } else { Mat4::IDENTITY };
            let (r, k, t) = crate::kitbash::place::decompose(f * inst.matrix() * keep);
            *inst = crate::envkit::scatter::Instance { source: inst.source, position: t, rotation: r, scale: k };
        }
        sc
    });
    // Clay: mirrored again only when its source changed.
    let from_ok = x.link.as_ref().and_then(|l| l.from.as_deref()) == s.clay.as_ref() && x.clay.is_some() == s.clay.is_some();
    if !from_ok {
        x.clay = s.clay.as_ref().map(|c| if axis == Some(0) { c.mirrored_x() } else { c.clone() });
        if let Some(l) = &mut x.link {
            l.from = s.clay.clone().map(Box::new);
        }
    }
    // The bone on the other side, when the rig has one.
    x.bone = s.bone.as_ref().map(|b| {
        let other = mirror_name(b).filter(|m| x.parent.and_then(|p| scene.get(p)).and_then(|p| p.rig.as_ref()).is_some_and(|r| r.bone_index(m).is_some()));
        other.unwrap_or_else(|| b.clone())
    });
}

fn side_name_or_same(n: &str) -> String {
    mirror_name(n).unwrap_or_else(|| n.to_string())
}

/// Brings one linked copy up to date. Returns whether anything changed.
fn sync_one(scene: &mut Scene, id: u64) -> bool {
    let Some(x) = scene.get(id).cloned() else { return false };
    let Some(link) = x.link.clone() else { return false };
    let Some(s) = scene.get(link.source).cloned() else { return false };
    let mut want = x.clone();
    mirror_content(scene, &s, &mut want, link.axis);
    let mut changed = false;
    if want != x {
        *scene.get_mut(id).expect("exists") = want;
        changed = true;
    }
    let desired = link_world(scene, s.id, &link) * scene.world_matrix(s.id) * axis_flip(link.axis);
    if !close(desired, scene.world_matrix(id)) {
        scene.set_world_matrix(id, desired);
        changed = true;
    }
    changed
}

fn unique(scene: &Scene, base: &str) -> String {
    if !scene.objects.iter().any(|o| o.name == base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}{n}")).find(|n| !scene.objects.iter().any(|o| &o.name == n)).expect("unbounded")
}

/// Options of `mirror`.
#[derive(Clone, Debug)]
pub struct MirrorOptions {
    /// World plane: unit normal and offset (n . p = d).
    pub normal: Vec3,
    pub d: f32,
    pub linked: bool,
    /// The copy's name (default: the other side's name).
    pub name: Option<String>,
    /// Also mirror the kitbash parts that sit on the object (and on its children).
    pub kitbash: bool,
}

/// What `mirror` made.
#[derive(Debug, Default)]
pub struct Mirrored {
    pub root: u64,
    /// Every object made (the copy, its children, kitbash groups and their contents).
    pub objects: Vec<u64>,
    /// Kitbash groups mirrored.
    pub groups: Vec<u64>,
    /// Copies drawn through a mirrored (negative) matrix: their geometry
    /// cannot be reflected itself (kit pieces, hair, terrain).
    pub negative: Vec<u64>,
    /// Objects left out (cloth, fire), by name.
    pub skipped: Vec<String>,
}

/// A mirror copy of `src` (with its children and the kitbash parts on
/// them) across a world plane. See the module docs.
pub fn mirror(scene: &mut Scene, src: u64, opt: &MirrorOptions) -> Result<Mirrored, String> {
    let o = scene.get(src).ok_or("no such object")?;
    if matches!(o.shape, Shape::Cloth | Shape::Fire) {
        return Err(format!("{} is a {}: mirror its parent and drape or simulate again", o.name, o.shape.label()));
    }
    if o.kitbash.as_deref() == Some(crate::kitbash::place::LIBRARY_TAG) {
        return Err("kitbash library sources cannot be mirrored".into());
    }
    let n = opt.normal.normalize_or_zero();
    if n == Vec3::ZERO || !opt.d.is_finite() {
        return Err("the mirror plane needs a nonzero normal".into());
    }
    at_rest(scene, |scene| {
        let (nl, dl) = plane_in_root(scene, src, n, opt.d);
        let mut out = Mirrored::default();
        // Copies of the object and its subtree, parents first.
        let mut made: HashMap<u64, u64> = HashMap::new();
        let first = copy_tree(scene, src, None, (nl, dl), opt.name.clone(), &mut made, &mut out);
        out.root = first.ok_or("nothing to mirror")?;
        // Kitbash groups sitting on the object or its children.
        if opt.kitbash {
            let owners: Vec<(u64, u64)> = made.iter().map(|(a, b)| (*a, *b)).collect();
            for (s, x) in owners {
                for g in following(scene, s) {
                    if made.contains_key(&g) {
                        continue;
                    }
                    if let Some(gx) = mirror_group(scene, g, x, (nl, dl), &mut made, &mut out) {
                        out.groups.push(gx);
                    }
                }
            }
        }
        if !opt.linked {
            for &id in &out.objects {
                if let Some(o) = scene.get_mut(id) {
                    o.link = None;
                }
            }
        }
        Ok(out)
    })
}

/// Kitbash groups following `owner` that are originals (not mirror copies).
fn following(scene: &Scene, owner: u64) -> Vec<u64> {
    scene.objects.iter().filter(|g| g.follow.as_ref().is_some_and(|f| f.object == owner) && g.link.is_none()).map(|g| g.id).collect()
}

/// Copies `s` and its subtree as linked mirror copies under `parent`
/// (None: next to `s`). Returns the copy of `s`.
fn copy_tree(scene: &mut Scene, s: u64, parent: Option<u64>, plane: (Vec3, f32), name: Option<String>, made: &mut HashMap<u64, u64>, out: &mut Mirrored) -> Option<u64> {
    let src = scene.get(s)?.clone();
    if matches!(src.shape, Shape::Cloth | Shape::Fire) {
        out.skipped.push(src.name.clone());
        return None;
    }
    let world = scene.world_matrix(s);
    let axis = choose_axis(&src, world);
    let mut x = src.clone();
    x.id = 0;
    x.name = unique(scene, &name.unwrap_or_else(|| side_name(&src.name)));
    x.parent = parent.or(src.parent);
    x.follow = None;
    x.link = Some(Link { source: s, normal: plane.0, d: plane.1, axis, from: None });
    let id = add_object(scene, x);
    made.insert(s, id);
    out.objects.push(id);
    if axis.is_none() && !matches!(src.shape, Shape::Group | Shape::Scatter) {
        out.negative.push(id);
    }
    sync_one(scene, id);
    for c in scene.children(Some(s)) {
        let skip = scene.get(c).is_some_and(|o| o.link.is_some() || o.kitbash.as_deref() == Some(crate::kitbash::place::LIBRARY_TAG));
        if !skip {
            copy_tree(scene, c, Some(id), plane, None, made, out);
        }
    }
    Some(id)
}

/// A mirrored copy of kitbash group `g` that follows `owner_copy`.
fn mirror_group(scene: &mut Scene, g: u64, owner_copy: u64, plane: (Vec3, f32), made: &mut HashMap<u64, u64>, out: &mut Mirrored) -> Option<u64> {
    let gx = copy_tree(scene, g, None, plane, None, made, out)?;
    let offset = scene.world_matrix(owner_copy).inverse() * scene.world_matrix(gx);
    if let Some(o) = scene.get_mut(gx) {
        o.follow = Some(Follow { object: owner_copy, offset });
    }
    Some(gx)
}

/// Adds an object with a fresh id; returns it.
fn add_object(scene: &mut Scene, mut o: Object) -> u64 {
    let id = scene.add(Shape::Group);
    o.id = id;
    *scene.get_mut(id).expect("added") = o;
    id
}

/// Keeps kitbash groups on what they sit on and linked mirror copies the
/// reflection of their sources. Cheap when nothing changed; returns
/// whether anything did. The editor runs it after every edit.
pub fn sync(scene: &mut Scene) -> bool {
    if !scene.objects.iter().any(|o| o.follow.is_some() || o.link.is_some()) {
        return false;
    }
    at_rest(scene, |scene| {
        let mut changed = false;
        let ids: HashSet<u64> = scene.objects.iter().map(|o| o.id).collect();
        // Copies whose source is gone: kitbash copies go, others unlink.
        let orphans: Vec<(u64, bool)> = scene.objects.iter().filter(|o| o.link.as_ref().is_some_and(|l| !ids.contains(&l.source))).map(|o| (o.id, o.kitbash.is_some())).collect();
        for (id, kb) in orphans {
            changed = true;
            if kb {
                scene.remove_tree(id);
            } else if let Some(o) = scene.get_mut(id) {
                o.link = None;
            }
        }
        for o in &mut scene.objects {
            if o.follow.as_ref().is_some_and(|f| !ids.contains(&f.object)) {
                o.follow = None;
                changed = true;
            }
        }
        changed |= grow_links(scene);
        changed |= follow_all(scene);
        // Linked copies, parents first.
        let mut linked: Vec<(usize, u64)> = scene.objects.iter().filter(|o| o.link.is_some()).map(|o| (scene.ancestors(o.id).len(), o.id)).collect();
        linked.sort_unstable();
        for (_, id) in linked {
            changed |= sync_one(scene, id);
        }
        changed |= follow_all(scene);
        changed
    })
}

/// New children and new kitbash groups of linked sources get copies.
fn grow_links(scene: &mut Scene) -> bool {
    let mut changed = false;
    let pairs: Vec<(u64, u64, Link)> = scene.objects.iter().filter_map(|o| o.link.as_ref().map(|l| (o.id, l.source, l.clone()))).collect();
    let copied: HashSet<u64> = pairs.iter().map(|p| p.1).collect();
    for (x, s, l) in pairs {
        let plane = (l.normal, l.d);
        let mut out = Mirrored::default();
        let mut made = HashMap::new();
        for c in scene.children(Some(s)) {
            let Some(co) = scene.get(c) else { continue };
            if copied.contains(&c) || co.link.is_some() || co.kitbash.as_deref() == Some(crate::kitbash::place::LIBRARY_TAG) {
                continue;
            }
            copy_tree(scene, c, Some(x), plane, None, &mut made, &mut out);
            changed = true;
        }
        for g in following(scene, s) {
            if copied.contains(&g) || made.contains_key(&g) {
                continue;
            }
            mirror_group(scene, g, x, plane, &mut made, &mut out);
            changed = true;
        }
    }
    changed
}

/// Moves following groups (not linked ones) onto their objects.
fn follow_all(scene: &mut Scene) -> bool {
    let mut changed = false;
    let list: Vec<(u64, Follow)> = scene.objects.iter().filter(|o| o.link.is_none()).filter_map(|o| o.follow.clone().map(|f| (o.id, f))).collect();
    for (g, f) in list {
        let Some(owner) = scene.get(f.object).cloned() else { continue };
        // Parts on a rigged clay hang on its bones already.
        if owner.rig.as_ref().is_some_and(|r| !r.bones.is_empty()) {
            continue;
        }
        // An object on a bone: its groups ride the same bone.
        if let (Some(b), Some(p)) = (&owner.bone, owner.parent)
            && let Some(go) = scene.get(g)
            && (go.parent != Some(p) || go.bone.as_ref() != Some(b))
        {
            let go = scene.get_mut(g).expect("exists");
            go.parent = Some(p);
            go.bone = Some(b.clone());
            changed = true;
        }
        let desired = scene.world_matrix(owner.id) * f.offset;
        if !close(desired, scene.world_matrix(g)) {
            scene.set_world_matrix(g, desired);
            changed = true;
        }
    }
    changed
}

/// Stops a linked copy (with its children and mirrored kitbash groups)
/// following its source. Returns how many objects were unlinked.
pub fn unlink(scene: &mut Scene, id: u64) -> usize {
    let mut ids = scene.descendants(id);
    ids.push(id);
    let groups: Vec<u64> = scene.objects.iter().filter(|o| o.link.is_some() && o.follow.as_ref().is_some_and(|f| ids.contains(&f.object))).map(|o| o.id).collect();
    for g in groups {
        ids.push(g);
        ids.extend(scene.descendants(g));
    }
    let mut n = 0;
    for i in ids {
        if let Some(o) = scene.get_mut(i).filter(|o| o.link.is_some()) {
            o.link = None;
            n += 1;
        }
    }
    n
}

/// After a group is moved on purpose: it stays where it was put relative
/// to its object.
pub fn refollow(scene: &mut Scene, g: u64) {
    at_rest(scene, |scene| {
        let Some(f) = scene.get(g).and_then(|o| o.follow.clone()) else { return };
        let offset = scene.world_matrix(f.object).inverse() * scene.world_matrix(g);
        if let Some(o) = scene.get_mut(g) {
            o.follow = Some(Follow { object: f.object, offset });
        }
    });
}

/// Gives kitbash groups made before groups followed their objects (older
/// scenes) the object they sit on: the only mesh object riding the same
/// bone, or the one whose bounds hold most of the group's parts.
pub fn infer_follow(scene: &mut Scene, meshes: &crate::mesh::MeshLib) -> usize {
    at_rest(scene, |scene| {
        let eval = scene.evaluate();
        let groups: Vec<u64> = scene.objects.iter().filter(|o| o.shape == Shape::Group && o.follow.is_none() && o.link.is_none() && o.kitbash.as_deref().is_some_and(|t| t != crate::kitbash::place::LIBRARY_TAG) && o.parent.is_some()).map(|o| o.id).collect();
        let world_bounds = |o: &Object| -> Option<(Vec3, Vec3)> {
            let (lo, hi) = meshes.bounds(o)?;
            let w = eval.get(&o.id)?.0;
            let mut acc: Option<(Vec3, Vec3)> = None;
            for i in 0..8 {
                let c = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
                let p = w.transform_point3(c);
                acc = Some(acc.map_or((p, p), |(a, b)| (a.min(p), b.max(p))));
            }
            acc
        };
        let mut n = 0;
        for g in groups {
            let go = scene.get(g).expect("listed").clone();
            // The group's parent being the owner itself (rigged clay or a plain object): nothing to infer.
            if go.bone.is_none() || scene.get(go.parent.expect("listed")).is_some_and(|p| p.rig.is_none()) {
                continue;
            }
            let candidates: Vec<&Object> = scene.objects.iter().filter(|o| o.parent == go.parent && o.bone == go.bone && o.kitbash.is_none() && o.shape.is_mesh()).collect();
            let owner = match candidates.len() {
                0 => continue,
                1 => candidates[0].id,
                _ => {
                    // Points of the group's parts, world.
                    let mut pts = Vec::new();
                    for d in scene.descendants(g) {
                        let Some(o) = scene.get(d) else { continue };
                        let w = eval.get(&d).map_or(Mat4::IDENTITY, |e| e.0);
                        if let Some(sc) = &o.scatter {
                            pts.extend(sc.instances.iter().map(|i| w.transform_point3(i.position)));
                        }
                        if let Some(p) = &o.part {
                            pts.extend(p.points.first().map(|q| w.transform_point3(Vec3::from(*q))));
                        }
                    }
                    let score = |o: &Object| -> (usize, i64) {
                        let Some((lo, hi)) = world_bounds(o) else { return (0, 0) };
                        let pad = (hi - lo).length() * 0.02;
                        let inside = pts.iter().filter(|p| p.cmpge(lo - pad).all() && p.cmple(hi + pad).all()).count();
                        (inside, -((hi - lo).length() * 1000.0) as i64)
                    };
                    candidates.iter().max_by_key(|o| score(o)).map(|o| o.id).expect("two or more")
                }
            };
            let offset = scene.world_matrix(owner).inverse() * scene.world_matrix(g);
            scene.get_mut(g).expect("listed").follow = Some(Follow { object: owner, offset });
            n += 1;
        }
        n
    })
}

// ---------- align ----------

/// The rotation turning local axis `a.0` exactly onto world `a.1`, and
/// local `b.0` as close to world `b.1` as it can (the rest of it is the
/// part of `b.1` along `a.1`).
pub fn axes_rotation(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> Result<Quat, String> {
    let frame = |p: Vec3, q: Vec3| -> Option<Mat3> {
        let x = p.normalize_or_zero();
        let y = q - x * q.dot(x);
        if x == Vec3::ZERO || y.length() < 1e-4 * q.length().max(1e-9) {
            return None;
        }
        let y = y.normalize();
        Some(Mat3::from_cols(x, y, x.cross(y)))
    };
    let local = frame(a.0, b.0).ok_or("the two local axes must differ")?;
    let world = frame(a.1, b.1).ok_or("the two directions must be nonzero and not parallel")?;
    Ok(Quat::from_mat3(&(world * local.transpose())).normalize())
}

/// A bone's frame at rest in its rig's space: origin `at` of the way from
/// head to tail, +Y along the bone, +Z toward the rig's front (+Z) as far
/// as the bone allows (else up), +X = Y x Z.
pub fn bone_frame(rig: &Rig, bone: &str, at: f32) -> Option<Mat4> {
    let b = rig.bones.iter().find(|b| b.name == bone)?;
    let y = (b.tail - b.head).normalize_or(Vec3::Y);
    let mut z = Vec3::Z - y * y.z;
    if z.length() < 0.2 {
        z = Vec3::Y - y * y.y;
    }
    let z = z.normalize();
    let x = y.cross(z);
    Some(Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), b.head.lerp(b.tail, at).extend(1.0)))
}

/// An object's frame (world, rest): its pivot and its unit axes.
pub fn object_frame(scene: &Scene, id: u64) -> Mat4 {
    let w = scene.world_matrix(id);
    let (_, r, _) = w.to_scale_rotation_translation();
    Mat4::from_rotation_translation(r, scene.pivot_world(id))
}

/// A named point on an object (local space): a blob's center, or its middle.
pub fn named_point(o: &Object, name: &str) -> Option<Vec3> {
    let clay = o.clay.as_ref()?;
    let b = clay.blobs.iter().find(|b| b.name == name)?;
    let p = match &b.form {
        Form::Sphere { center, .. } | Form::Ellipsoid { center, .. } | Form::Box { center, .. } | Form::Torus { center, .. } | Form::Extrude { center, .. } | Form::Lathe { center, .. } => *center,
        Form::Capsule { a, b, .. } => (*a + *b) * 0.5,
        Form::Sweep { points, .. } | Form::Chain { points, .. } | Form::Membrane { points, .. } | Form::Band { points, .. } => points.iter().copied().sum::<Vec3>() / points.len().max(1) as f32,
        _ => return None,
    };
    Some(if b.flip { Vec3::new(-p.x, p.y, p.z) } else { p })
}

/// Places an object (rest pose) so its world rotation is `rot` and the
/// local point `anchor` lands on world `at`; scale is kept.
pub fn place(scene: &mut Scene, id: u64, rot: Quat, anchor: Vec3, at: Vec3) {
    at_rest(scene, |scene| {
        let (s, _, _) = scene.world_matrix(id).to_scale_rotation_translation();
        let m = Mat4::from_scale_rotation_translation(s, rot, Vec3::ZERO);
        let t = at - m.transform_point3(anchor);
        scene.set_world_matrix(id, Mat4::from_translation(t) * m);
    });
}

/// Turns an object about its pivot (rest pose) so its local axes point
/// along world directions: the first pair exactly, the second as close as
/// it can (one pair: the shortest turn). A mirrored (negative) axis of
/// the object points the other way, so asking for +X still gets +X.
pub fn align_axes(scene: &mut Scene, id: u64, pairs: &[(Vec3, Vec3)]) -> Result<(), String> {
    let (world, pivot_local, pivot_world) = at_rest(scene, |s| (s.world_matrix(id), s.get(id).map_or(Vec3::ZERO, |o| o.pivot), s.pivot_world(id)));
    let (scale, r0, _) = world.to_scale_rotation_translation();
    let signed = |axis: Vec3, v: Vec3| if axis.dot(scale) < 0.0 { -v } else { v };
    let rot = match pairs {
        [] => return Err("align needs two of x_axis, y_axis, z_axis, forward, up (world directions)".into()),
        [(axis, v)] => Quat::from_rotation_arc((r0 * *axis).normalize(), signed(*axis, v.normalize())) * r0,
        [(a1, v1), (a2, v2), ..] => {
            if a1.abs_diff_eq(*a2, 1e-6) {
                return Err("align: give two different axes (forward and z_axis are the same axis)".into());
            }
            axes_rotation((*a1, signed(*a1, *v1)), (*a2, signed(*a2, *v2)))?
        }
    };
    place(scene, id, rot.normalize(), pivot_local, pivot_world);
    Ok(())
}

/// What a frame is taken from.
#[derive(Clone, Debug)]
pub enum FrameOf {
    /// A bone of a rig object, `at` of the way from head to tail.
    Bone { rig: u64, bone: String, at: f32 },
    Object(u64),
}

/// Where `frame_place` puts an object: in a frame, moved by `offset`,
/// turned by `roll` (degrees about the frame's Y) then `rotate`, with the
/// object's local point `anchor` on the frame's origin.
#[derive(Clone, Debug)]
pub struct FramePlace {
    pub of: FrameOf,
    pub offset: Vec3,
    pub rotate: Quat,
    pub roll: f32,
    pub anchor: Vec3,
    /// Hang the object on that bone too.
    pub attach: bool,
}

/// Puts an object in the frame of a bone or another object (rest pose).
pub fn frame_place(scene: &mut Scene, id: u64, f: &FramePlace) -> Result<(), String> {
    at_rest(scene, |scene| {
        let base = match &f.of {
            FrameOf::Bone { rig, bone, at } => {
                let r = scene.get(*rig).and_then(|o| o.rig.as_ref()).ok_or("the rig is gone")?;
                let bf = bone_frame(r, bone, *at).ok_or_else(|| format!("no bone {bone}"))?;
                let (_, q, t) = (scene.world_matrix(*rig) * bf).to_scale_rotation_translation();
                Mat4::from_rotation_translation(q, t)
            }
            FrameOf::Object(p) => {
                if *p == id {
                    return Err("an object cannot be framed in itself".into());
                }
                object_frame(scene, *p)
            }
        };
        let frame = base * Mat4::from_translation(f.offset) * Mat4::from_rotation_y(f.roll.to_radians()) * Mat4::from_quat(f.rotate);
        if f.attach
            && let FrameOf::Bone { rig, bone, .. } = &f.of
        {
            if *rig == id || scene.is_ancestor(id, *rig) {
                return Err("an object cannot ride a bone of its own rig".into());
            }
            scene.reparent(id, Some(*rig))?;
            scene.get_mut(id).ok_or("object vanished")?.bone = Some(bone.clone());
        }
        let (_, rot, origin) = frame.to_scale_rotation_translation();
        place(scene, id, rot, f.anchor, origin);
        Ok(())
    })
}

// ---------- pose in place ----------

/// What `pose_rest` moved.
#[derive(Debug, Default)]
pub struct Posed {
    pub bones: Vec<String>,
    pub objects: Vec<u64>,
    pub blobs: Vec<String>,
    pub strokes: usize,
    /// Blob forms whose roll a turn can only follow approximately.
    pub approximate: Vec<String>,
}

/// Moves bone `bone` of `rig_id` (and the bones under it) by the rigid
/// `delta` (rig rest space). With `carry`, the objects riding those bones
/// (props, armor, kitbash groups), the rig clay's blobs assigned to them
/// (plus `extra` blob names) and their weight strokes move along.
pub fn pose_rest(scene: &mut Scene, rig_id: u64, bone: &str, delta: Mat4, carry: bool, extra: &[String]) -> Result<Posed, String> {
    let o = scene.get(rig_id).ok_or("no rig object")?;
    let rig = o.rig.as_ref().ok_or_else(|| format!("{} has no rig", o.name))?;
    if rig.bone_index(bone).is_none() {
        return Err(format!("{} has no bone {bone}; bones: {:?}", o.name, rig.bones.iter().map(|b| &b.name).collect::<Vec<_>>()));
    }
    // The bone and everything under it.
    let mut set: Vec<String> = vec![bone.to_string()];
    loop {
        let more: Vec<String> = rig.bones.iter().filter(|b| b.parent.as_ref().is_some_and(|p| set.contains(p)) && !set.contains(&b.name)).map(|b| b.name.clone()).collect();
        if more.is_empty() {
            break;
        }
        set.extend(more);
    }
    let mut out = Posed { bones: set.clone(), ..Default::default() };
    at_rest(scene, |scene| {
        let o = scene.get_mut(rig_id).expect("checked");
        let rig = o.rig.as_mut().expect("checked");
        for b in rig.bones.iter_mut().filter(|b| set.contains(&b.name)) {
            b.head = delta.transform_point3(b.head);
            b.tail = delta.transform_point3(b.tail);
        }
        if !carry {
            return;
        }
        for w in rig.weights.iter_mut().filter(|w| set.contains(&w.bone)) {
            w.points = w.points.iter().map(|p| delta.transform_point3(*p)).collect();
            out.strokes += 1;
        }
        if let Some(clay) = o.clay.as_mut() {
            for b in clay.blobs.iter_mut().filter(|b| set.contains(&b.bone) || (!b.name.is_empty() && extra.contains(&b.name))) {
                if !transform_form(&mut b.form, delta, b.flip) {
                    out.approximate.push(b.name.clone());
                }
                out.blobs.push(if b.name.is_empty() { b.bone.clone() } else { b.name.clone() });
            }
        }
        // Two-bone parts on this rig: where their ends are now (rig rest
        // space), each end moved when its bone moved, blended between.
        let spans: Vec<(u64, Vec<Vec3>)> = scene.objects.iter().filter_map(|x| {
            let sp = x.part.as_ref()?.span.as_ref()?;
            let h = crate::kitbash::span::hang(scene, x).filter(|h| h.rig == rig_id)?;
            let (ma, mb) = (set.contains(&sp.bones[0]), set.contains(&sp.bones[1]));
            if !ma && !mb && !set.contains(&h.bone) {
                return None;
            }
            let q: Vec<Vec3> = x.part.as_ref()?.points.iter().map(|p| h.to_rig.transform_point3(Vec3::from(*p))).collect();
            let w = crate::kitbash::span::blend_weights(&q);
            let moved = |p: Vec3, on: bool| if on { delta.transform_point3(p) } else { p };
            Some((x.id, q.iter().zip(&w).map(|(p, w)| moved(*p, ma).lerp(moved(*p, mb), *w)).collect()))
        }).collect();
        let riders: Vec<u64> = scene.objects.iter().filter(|x| x.parent == Some(rig_id) && x.bone.as_ref().is_some_and(|b| set.contains(b))).map(|x| x.id).collect();
        for id in riders {
            let x = scene.get_mut(id).expect("listed");
            let m = delta * x.local_matrix();
            x.set_local_matrix(m);
            out.objects.push(id);
        }
        for (id, q) in spans {
            let Some(h) = scene.get(id).and_then(|x| crate::kitbash::span::hang(scene, x)) else { continue };
            let back = h.to_rig.inverse();
            if let Some(p) = scene.get_mut(id).and_then(|x| x.part.as_mut()) {
                p.points = q.iter().map(|v| back.transform_point3(*v).to_array()).collect();
                out.objects.push(id);
            }
        }
        // Groups following carried objects already moved with their bone:
        // keep their offsets.
    });
    Ok(out)
}

/// Moves a blob's form rigidly by `m` (object space). `flip`: the blob is
/// stored mirrored (its form is read at -x). Returns false when only an
/// approximation was possible (the roll of extrude and sweep profiles).
pub fn transform_form(form: &mut Form, m: Mat4, flip: bool) -> bool {
    let f = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    // A flipped blob is stored on the other side: move it by the mirrored motion.
    let m = if flip { f * m * f } else { m };
    let (_, q, _) = m.to_scale_rotation_translation();
    let p = |v: &mut Vec3| *v = m.transform_point3(*v);
    let d = |v: &mut Vec3| *v = m.transform_vector3(*v);
    let turn = |r: &mut Vec3| {
        let (z, y, x) = (q * crate::clay::rotation(*r)).to_euler(EulerRot::ZYX);
        *r = Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI);
    };
    let turned = q.angle_between(Quat::IDENTITY) > 1e-5;
    match form {
        Form::Sphere { center, .. } => p(center),
        Form::Ellipsoid { center, rotate, .. } | Form::Box { center, rotate, .. } | Form::Torus { center, rotate, .. } => {
            p(center);
            turn(rotate);
        }
        Form::Capsule { a, b, .. } => {
            p(a);
            p(b);
        }
        Form::Extrude { center, axis, .. } => {
            p(center);
            d(axis);
            return !turned;
        }
        Form::Lathe { center, axis, .. } => {
            p(center);
            d(axis);
        }
        Form::Sweep { points, .. } => {
            points.iter_mut().for_each(p);
            return !turned;
        }
        Form::Chain { points, .. } | Form::Membrane { points, .. } => points.iter_mut().for_each(p),
        Form::Band { points, normal, .. } => {
            points.iter_mut().for_each(p);
            d(normal);
        }
        Form::Shell { source, .. } => return transform_form(source, m, false),
        Form::Array { source, step, center, axis, points, .. } => {
            let ok = transform_form(source, m, false);
            d(step);
            p(center);
            d(axis);
            points.iter_mut().for_each(p);
            return ok;
        }
        Form::Human { .. } => return false,
    }
    true
}
