//! Placements become scene objects.
//!
//! A placement is a part and where it goes (a world rest-pose frame: +Y
//! out of the surface, base on it) on an owner object. Writing them:
//! - Parts that repeat are instances: one hidden source `Part` object per
//!   distinct part (kept under the hidden `kitbash_library` group) and a
//!   `Scatter` holding the copies, so a thousand bolts cost one bolt mesh
//!   in the viewport, film and exports (glTF nodes share one mesh).
//! - Parts between points (cables, pistons) are their own `Part` objects.
//! - Everything hangs under a group per attachment: the owner's bone when
//!   it rides one (or the bone that moves the hit point most, on rigged
//!   clay), else the owner itself. So parts move with the rig, and glTF /
//!   FBX export them under the joint.
//! - Every object a call adds carries its group tag (`Object::kitbash`):
//!   `remove` takes them all out again (and unused library sources).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::{Mat3, Mat4, Quat, Vec3};

use super::surface::{Hit, Keep, Owner, Surface};
use super::{Kind, Part, REFERENCE_SIZE};
use crate::envkit::scatter::{Instance, Scatter};
use crate::mesh::MeshLib;
use crate::scene::{Object, Scene, Shape};

/// The hidden group holding instance sources.
pub const LIBRARY: &str = "kitbash_library";
/// The tag library sources carry.
pub const LIBRARY_TAG: &str = "kitbash:library";

/// A part and where it goes.
#[derive(Clone, Debug)]
pub struct Placement {
    pub part: Part,
    /// Part space -> world, rest pose (may mirror).
    pub frame: Mat4,
    /// The object it sits on.
    pub owner: u64,
    /// For rigged owners: the bone that moves the spot most.
    pub bone: Option<String>,
    /// How far it was sunk so no corner floats.
    pub sink: f32,
    /// Lower goes first when a budget is tight.
    pub priority: u8,
}

impl Placement {
    pub fn origin(&self) -> Vec3 {
        self.frame.transform_point3(Vec3::ZERO)
    }

    /// Footprint radius (world), for spacing.
    pub fn radius(&self) -> f32 {
        let k = self.frame.transform_vector3(Vec3::X).length();
        self.part.footprint().map_or(self.part.tube_radius() * 2.0, |(hx, hz, _)| hx.hypot(hz)) * k
    }
}

/// A frame on the surface: origin `p`, +Y along `n`, +X along `along`
/// (projected into the surface plane), scaled by `k`.
pub fn frame(p: Vec3, n: Vec3, along: Vec3, k: f32) -> Mat4 {
    let n = n.normalize_or(Vec3::Y);
    let mut x = along - n * along.dot(n);
    if x.length_squared() < 1e-10 {
        x = n.any_orthonormal_vector();
    }
    let x = x.normalize();
    let z = x.cross(n);
    Mat4::from_cols((x * k).extend(0.0), (n * k).extend(0.0), (z * k).extend(0.0), p.extend(1.0))
}

/// A default direction along a surface: world X when the surface allows,
/// else world Z.
pub fn default_along(n: Vec3) -> Vec3 {
    let x = Vec3::X - n * n.x;
    if x.length() > 0.3 { x } else { Vec3::Z - n * n.z }
}

/// Puts a part on the surface at a hit: aligned to `along`, sunk so no
/// footprint corner floats. None when the footprint hangs off the
/// surface or the surface curves away more than `max_sink` of its size
/// (the error says which).
pub fn on_surface(s: &Surface, hit: &Hit, part: Part, along: Vec3, max_sink: f32) -> Result<Placement, &'static str> {
    let mut f = frame(hit.p, hit.n, along, 1.0);
    let mut sink = 0.0;
    if let Some((hx, hz, h)) = part.footprint() {
        let (gap, _, _) = s.embed(f, hx, hz).ok_or("footprint hangs off the surface or over a deep dent")?;
        if gap > max_sink * h.max(hx.min(hz)) {
            return Err("surface curves away under it (raise max_sink)");
        }
        sink = gap;
        f = Mat4::from_translation(-hit.n * sink) * f;
    }
    let o = s.owner_of(hit.tri);
    Ok(Placement { part, frame: f, owner: o.id, bone: s.bone_at(hit), sink, priority: 1 })
}

/// The reflection across the plane x = `x0`.
pub fn mirror_x(x0: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(x0, 0.0, 0.0)) * Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)) * Mat4::from_translation(Vec3::new(-x0, 0.0, 0.0))
}

/// Mirrored copies of `list` across x = `x0`, each landing on whatever
/// surface is there (another part of the asset: the other leg).
/// Placements straddling the plane are kept once; passes also keep off the
/// other side's own markings (`avoid_markings`). Returns the copies and
/// how many found no surface.
pub fn mirrored(list: &[Placement], s: &Surface, x0: f32, avoid_markings: bool) -> (Vec<Placement>, usize) {
    let m = mirror_x(x0);
    let mut out = Vec::new();
    let mut missed = 0;
    for p in list {
        let o = p.origin();
        if (o.x - x0).abs() < p.radius() * 0.5 && p.part.points.is_empty() {
            continue;
        }
        let mut q = p.clone();
        if !p.part.points.is_empty() {
            q.part.points = p.part.points.iter().map(|v| m.transform_point3(Vec3::from(*v)).to_array()).collect();
            if let Some(sp) = &mut q.part.span {
                sp.bones = sp.bones.clone().map(|b| swap_side(&b));
            }
            // A point part's frame is identity; its owner is where its first point sits.
            let first = Vec3::from(q.part.points[0]);
            let Some(h) = s.nearest(first) else { missed += 1; continue };
            q.owner = s.owner_of(h.tri).id;
            q.bone = s.bone_at(&h).or(p.bone.clone().map(|b| swap_side(&b)));
            out.push(q);
            continue;
        }
        let f = m * p.frame;
        let origin = f.transform_point3(Vec3::ZERO);
        let n = f.transform_vector3(Vec3::Y).normalize_or(Vec3::Y);
        let reach = (p.radius() + p.sink) * 2.0 + s.diag * 0.002;
        let Some(h) = s.snap(origin + n * p.sink, n, reach) else { missed += 1; continue };
        // Keep the mirrored orientation; move onto the surface found.
        q.frame = Mat4::from_translation(h.p - n * p.sink - origin) * f;
        // The other side may carry its own markings or decals: keep off them.
        if let Some((hx, hz, _)) = p.part.footprint().filter(|_| avoid_markings)
            && s.covers_markings(q.frame, hx, hz)
        {
            missed += 1;
            continue;
        }
        q.owner = s.owner_of(h.tri).id;
        q.bone = s.bone_at(&h).or(p.bone.clone().map(|b| swap_side(&b)));
        out.push(q);
    }
    (out, missed)
}

/// `thigh.L` <-> `thigh.R` (and _L/_R, Left/Right).
pub fn swap_side(name: &str) -> String {
    for (a, b) in [(".L", ".R"), ("_L", "_R"), (".l", ".r"), ("_l", "_r"), ("Left", "Right")] {
        if let Some(stem) = name.strip_suffix(a) { return format!("{stem}{b}"); }
        if let Some(stem) = name.strip_suffix(b) { return format!("{stem}{a}"); }
    }
    name.to_string()
}

/// Where an owner's parts hang: (parent, bone) and that space's world
/// matrix at rest.
fn attach(scene: &Scene, eval: &HashMap<u64, (Mat4, bool)>, owner: u64, bone: Option<&str>) -> (Option<u64>, Option<String>, Mat4) {
    let world = |id: u64| eval.get(&id).map_or(Mat4::IDENTITY, |e| e.0);
    let Some(o) = scene.get(owner) else { return (None, None, Mat4::IDENTITY) };
    // Rigged clay: the bone moving the spot.
    if let Some(rig) = o.rig.as_ref().filter(|r| !r.bones.is_empty()) {
        let b = bone.filter(|b| rig.bone_index(b).is_some()).map(str::to_string).or_else(|| rig.bones.iter().find(|b| b.parent.is_none()).map(|b| b.name.clone()));
        return (Some(o.id), b, world(o.id));
    }
    // An object riding a bone: the same bone, in the rig's space.
    if let (Some(b), Some(p)) = (&o.bone, o.parent)
        && scene.get(p).and_then(|p| p.rig.as_ref()).is_some_and(|r| r.bone_index(b).is_some())
    {
        return (Some(p), Some(b.clone()), world(p));
    }
    // Else the owner itself, or the nearest ancestor scaled evenly (a
    // copy's scale is one number, so it cannot follow a squashed parent).
    let mut cur = Some(o.id);
    while let Some(id) = cur {
        if even_scale(world(id)) {
            return (Some(id), None, world(id));
        }
        cur = scene.get(id).and_then(|x| x.parent);
    }
    (None, None, Mat4::IDENTITY)
}

fn even_scale(m: Mat4) -> bool {
    let c = Mat3::from_mat4(m);
    let l = [c.x_axis.length(), c.y_axis.length(), c.z_axis.length()];
    let mx = l.iter().copied().fold(0.0, f32::max).max(1e-9);
    l.iter().all(|v| (v - l[0]).abs() < mx * 1e-3) && c.x_axis.normalize_or_zero().dot(c.y_axis.normalize_or_zero()).abs() < 1e-3 && c.y_axis.normalize_or_zero().dot(c.z_axis.normalize_or_zero()).abs() < 1e-3
}

/// A similarity matrix as (rotation, signed uniform scale, translation).
pub fn decompose(m: Mat4) -> (Quat, f32, Vec3) {
    let c = Mat3::from_mat4(m);
    let det = c.determinant();
    let s = det.abs().cbrt() * if det < 0.0 { -1.0 } else { 1.0 };
    let r = if s.abs() > 1e-12 { c * (1.0 / s) } else { Mat3::IDENTITY };
    // Orthonormalize (rounding, slight shear).
    let x = r.x_axis.normalize_or(Vec3::X);
    let y = (r.y_axis - x * r.y_axis.dot(x)).normalize_or(Vec3::Y);
    let z = x.cross(y);
    (Quat::from_mat3(&Mat3::from_cols(x, y, z)).normalize(), s, m.w_axis.truncate())
}

/// Surface owners for objects (their meshes as drawn at rest, rigged
/// clay with its skin, clay decals to keep clear).
pub fn owners(scene: &Scene, meshes: &MeshLib, ids: &[u64]) -> Vec<Owner> {
    let eval = scene.evaluate();
    let mut out = Vec::new();
    for &id in ids {
        let Some(o) = scene.get(id) else { continue };
        // Kitbash copies are surfaces too (bolts on a plate): one owner per
        // copy, all named by the scatter, so parts on them hang under it.
        if let Some(sc) = o.scatter.as_ref().filter(|_| o.shape == Shape::Scatter) {
            let w = eval.get(&id).map_or(Mat4::IDENTITY, |e| e.0);
            for inst in &sc.instances {
                let Some(src) = sc.sources.get(inst.source as usize).and_then(|&s| scene.get(s)) else { continue };
                let Some(mesh) = meshes.get(src).filter(|m| !m.indices.is_empty()) else { continue };
                let world = w * inst.matrix() * crate::envkit::scatter::source_shape(src);
                out.push(Owner { id, mesh: mesh.clone(), skin: None, world, keep_clear: Vec::new() });
            }
            continue;
        }
        if !o.shape.is_mesh() || o.kitbash.as_deref() == Some(LIBRARY_TAG) {
            continue;
        }
        let Some(mesh) = meshes.get(o).filter(|m| !m.indices.is_empty()) else { continue };
        let world = eval.get(&id).map_or(Mat4::IDENTITY, |e| e.0);
        let skin = o.rig.as_ref().filter(|r| !r.bones.is_empty()).and_then(|r| {
            let (rest, skin) = meshes.rest(id)?;
            (Arc::ptr_eq(rest, mesh) || rest.vertices.len() == mesh.vertices.len()).then_some(())?;
            Some((skin?.clone(), r.bones.iter().map(|b| b.name.clone()).collect()))
        });
        // Markings stay visible (emblems, images, runes); damage (scratches,
        // splats) may be covered.
        let keep_clear = o.clay.as_ref().map(|c| c.decals.iter().filter(|d| matches!(d.shape, crate::smart::Shape::Emblem | crate::smart::Shape::Image | crate::smart::Shape::Rune)).map(|d| Keep::disc(world.transform_point3(Vec3::from(d.center)), d.size * 0.6 * world.transform_vector3(Vec3::X).length())).collect()).unwrap_or_default();
        out.push(Owner { id, mesh: mesh.clone(), skin, world, keep_clear });
    }
    out
}

/// Kitbash parts already in the scene (copies and point parts' ends are
/// left out: only footprints), so new parts keep clear of them.
pub fn existing_parts(scene: &Scene, except: &[u64]) -> Vec<Keep> {
    let eval = scene.evaluate();
    let mut out = Vec::new();
    for o in &scene.objects {
        let Some(&(world, true)) = eval.get(&o.id) else { continue };
        if except.contains(&o.id) {
            continue;
        }
        if let Some(sc) = o.scatter.as_ref().filter(|_| o.shape == Shape::Scatter) {
            for inst in &sc.instances {
                let Some(src) = sc.sources.get(inst.source as usize).and_then(|&s| scene.get(s)).filter(|s| s.shape == Shape::Part) else { continue };
                let Some((hx, hz, _)) = src.part.as_ref().and_then(Part::footprint) else { continue };
                out.push(Keep::footprint(world * inst.matrix() * crate::envkit::scatter::source_shape(src), hx, hz));
            }
        } else if o.shape == Shape::Part && o.kitbash.is_some() && o.kitbash.as_deref() != Some(LIBRARY_TAG) {
            let Some((hx, hz, _)) = o.part.as_ref().and_then(Part::footprint) else { continue };
            out.push(Keep::footprint(world, hx, hz));
        }
    }
    out
}

/// The size of the asset an object belongs to (its root's subtree, world
/// rest bounds: the largest side), and the kit scale: that over the
/// reference size.
pub fn asset_scale(scene: &Scene, meshes: &MeshLib, id: u64) -> (f32, f32) {
    let root = scene.ancestors(id).last().copied().unwrap_or(id);
    let bounds = |o: &Object| if o.kitbash.is_some() { None } else { meshes.bounds(o) };
    let size = scene.subtree_bounds(root, scene.world_matrix(root), &bounds).map_or(1.0, |(lo, hi)| (hi - lo).max_element()).max(1e-3);
    (size, (size / REFERENCE_SIZE).clamp(0.002, 100.0))
}

/// What a write added.
#[derive(Debug, Default)]
pub struct Written {
    pub groups: Vec<u64>,
    pub objects: Vec<u64>,
    pub copies: usize,
    pub sources: Vec<u64>,
    /// Attachments: (group name, parent name, bone).
    pub attached: Vec<(String, String, Option<String>)>,
}

/// Writes placements into the scene under tag `tag` (the rest pose is
/// assumed: `scene.frame` None).
pub fn write(scene: &mut Scene, tag: &str, list: &[Placement]) -> Written {
    let mut out = Written::default();
    if list.is_empty() {
        return out;
    }
    let keep_frame = scene.frame.take();
    let eval = scene.evaluate();
    let library = library_group(scene);
    // Group by attachment, and by the object the parts sit on when the
    // group does not hang under it (so the group can follow it).
    type Key = (Option<u64>, Option<String>, Option<u64>);
    let mut groups: BTreeMap<Key, Vec<(usize, Mat4)>> = BTreeMap::new();
    for (i, p) in list.iter().enumerate() {
        let (parent, bone, w) = attach(scene, &eval, p.owner, p.bone.as_deref());
        let rigged = scene.get(p.owner).is_some_and(|o| o.rig.as_ref().is_some_and(|r| !r.bones.is_empty()));
        let follow = (!rigged && parent != Some(p.owner) && scene.get(p.owner).is_some()).then_some(p.owner);
        groups.entry((parent, bone, follow)).or_default().push((i, w));
    }
    for ((parent, bone, follow), items) in groups {
        let gid = scene.add(Shape::Group);
        let pname = parent.and_then(|p| scene.get(p)).map_or("world".to_string(), |o| o.name.clone());
        let name = unique(scene, &format!("{tag}_{}", bone.clone().unwrap_or_else(|| pname.clone())));
        {
            let g = scene.get_mut(gid).expect("added");
            g.name = name.clone();
            g.parent = parent;
            g.bone = bone.clone();
            g.kitbash = Some(tag.to_string());
        }
        if let Some(owner) = follow {
            let w = items.first().map_or(Mat4::IDENTITY, |it| it.1);
            let offset = eval.get(&owner).map_or(Mat4::IDENTITY, |e| e.0).inverse() * w;
            scene.get_mut(gid).expect("added").follow = Some(crate::pose::Follow { object: owner, offset });
        }
        out.groups.push(gid);
        out.attached.push((name.clone(), pname, bone.clone()));
        let mut sc = Scatter::default();
        let mut src_index: HashMap<u64, u32> = HashMap::new();
        for (i, w) in items {
            let p = &list[i];
            let inv = w.inverse();
            if p.part.kind.info().between_points {
                // Its own object: points into the attachment's space.
                let mut part = p.part.clone();
                part.points = part.points.iter().map(|v| inv.transform_point3(Vec3::from(*v)).to_array()).collect();
                // Copies scale with the space: keep world sizes.
                let k = inv.transform_vector3(Vec3::X).length();
                part.radius = Some(part.tube_radius() * k);
                if let Some(s) = &mut part.size { *s = s.map(|v| v * k); }
                let id = scene.add(Shape::Part);
                let pname = unique(scene, &format!("{name}_{}", part.kind.name()));
                let o = scene.get_mut(id).expect("added");
                o.name = pname;
                o.parent = Some(gid);
                o.part = Some(part);
                o.kitbash = Some(tag.to_string());
                out.objects.push(id);
                continue;
            }
            let src = source(scene, library, &p.part);
            let s = *src_index.entry(src).or_insert_with(|| {
                sc.sources.push(src);
                sc.sources.len() as u32 - 1
            });
            if !out.sources.contains(&src) {
                out.sources.push(src);
            }
            let (r, k, t) = decompose(inv * p.frame);
            sc.instances.push(Instance { source: s, position: t, rotation: r, scale: k });
        }
        if !sc.instances.is_empty() {
            out.copies += sc.instances.len();
            let id = scene.add(Shape::Scatter);
            let sname = unique(scene, &format!("{name}_copies"));
            let o = scene.get_mut(id).expect("added");
            o.name = sname;
            o.parent = Some(gid);
            o.scatter = Some(sc);
            o.kitbash = Some(tag.to_string());
            out.objects.push(id);
        }
    }
    scene.frame = keep_frame;
    out
}

fn unique(scene: &Scene, base: &str) -> String {
    if !scene.objects.iter().any(|o| o.name == base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}{n}")).find(|n| !scene.objects.iter().any(|o| &o.name == n)).expect("unbounded")
}

/// The hidden library group (made when missing).
fn library_group(scene: &mut Scene) -> u64 {
    if let Some(o) = scene.objects.iter().find(|o| o.kitbash.as_deref() == Some(LIBRARY_TAG) && o.shape == Shape::Group) {
        return o.id;
    }
    let id = scene.add(Shape::Group);
    let name = unique(scene, LIBRARY);
    let o = scene.get_mut(id).expect("added");
    o.name = name;
    o.visible = false;
    o.kitbash = Some(LIBRARY_TAG.to_string());
    id
}

/// The library source for a part (made when missing).
fn source(scene: &mut Scene, library: u64, part: &Part) -> u64 {
    if let Some(o) = scene.objects.iter().find(|o| o.kitbash.as_deref() == Some(LIBRARY_TAG) && o.part.as_ref() == Some(part)) {
        return o.id;
    }
    let id = scene.add(Shape::Part);
    let size = part.dims();
    let name = unique(scene, &format!("kb_{}_{}_{}", part.kind.name(), part.style(), (size.max_element() * 1000.0).round() as u32));
    let o = scene.get_mut(id).expect("added");
    o.name = name;
    o.parent = Some(library);
    o.visible = false;
    o.part = Some(part.clone());
    o.kitbash = Some(LIBRARY_TAG.to_string());
    id
}

/// Removes every object tagged `tag` (with what is under it) and clay
/// blobs named `tag/...`, then library sources nothing uses. Returns how
/// many objects and blobs went.
pub fn remove(scene: &mut Scene, tag: &str) -> (usize, usize) {
    let doomed: Vec<u64> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(tag)).map(|o| o.id).collect();
    let before = scene.objects.len();
    for id in doomed {
        scene.remove_tree(id);
    }
    let prefix = format!("{tag}/");
    let mut blobs = 0;
    for o in &mut scene.objects {
        if let Some(c) = &mut o.clay {
            let n = c.blobs.len();
            c.blobs.retain(|b| !b.name.starts_with(&prefix));
            blobs += n - c.blobs.len();
            if n != c.blobs.len() {
                // Inserted meshes take their assets with them (the rest
                // stay as they were: removing never needs a new one).
                c.prune_assets();
            }
        }
    }
    collect_garbage(scene);
    (before - scene.objects.len(), blobs)
}

/// Drops library sources no scatter uses (and the library when empty).
pub fn collect_garbage(scene: &mut Scene) {
    let used: std::collections::HashSet<u64> = scene.objects.iter().filter_map(|o| o.scatter.as_ref()).flat_map(|s| s.sources.iter().copied()).collect();
    scene.objects.retain(|o| !(o.kitbash.as_deref() == Some(LIBRARY_TAG) && o.shape == Shape::Part && !used.contains(&o.id)));
    let lib: Vec<u64> = scene.objects.iter().filter(|o| o.kitbash.as_deref() == Some(LIBRARY_TAG) && o.shape == Shape::Group).map(|o| o.id).collect();
    for id in lib {
        if scene.children(Some(id)).is_empty() {
            scene.objects.retain(|o| o.id != id);
        }
    }
}

/// Kitbash groups in the scene: tag -> (objects, copies).
pub fn groups(scene: &Scene) -> BTreeMap<String, (usize, usize)> {
    let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in &scene.objects {
        let Some(tag) = o.kitbash.as_deref().filter(|t| *t != LIBRARY_TAG) else { continue };
        let e = out.entry(tag.to_string()).or_default();
        e.0 += 1;
        e.1 += o.scatter.as_ref().map_or(0, |s| s.instances.len());
    }
    out
}

/// A tag not used yet: `base_1`, `base_2`...
pub fn new_tag(scene: &Scene, base: &str) -> String {
    (1..).map(|n| format!("{base}_{n}")).find(|t| !scene.objects.iter().any(|o| o.kitbash.as_deref() == Some(t.as_str())) && !scene.objects.iter().any(|o| o.clay.as_ref().is_some_and(|c| c.blobs.iter().any(|b| b.name.starts_with(&format!("{t}/")))))).expect("unbounded")
}

/// Triangles a set of placements draws, and the unique triangles of the
/// meshes they need (each distinct part built once).
pub fn triangles(list: &[Placement], library: &[crate::material::Material], cache: &mut HashMap<String, usize>) -> (usize, usize) {
    let (mut drawn, mut unique) = (0, 0);
    for p in list {
        let key = serde_json::to_string(&p.part).unwrap_or_default();
        let n = match cache.get(&key) {
            Some(&n) => n,
            None => {
                let n = p.part.build(library).indices.len() / 3;
                cache.insert(key, n);
                unique += n;
                n
            }
        };
        drawn += n;
    }
    (drawn, unique)
}

/// Keeps placements under a triangle budget: lowest priority first, then
/// a seeded shuffle within a priority. Returns how many were dropped.
pub fn fit_budget(list: &mut Vec<Placement>, budget: usize, library: &[crate::material::Material], seed: u64) -> usize {
    let mut cache = HashMap::new();
    let (total, _) = triangles(list, library, &mut cache);
    if total <= budget {
        return 0;
    }
    let mut order: Vec<usize> = (0..list.len()).collect();
    let mut rng = crate::envkit::Rand::new(seed ^ 0xB0D6);
    let keys: Vec<(u8, u64)> = order.iter().map(|_| (0, rng.next())).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(list[i].priority), keys[i].1));
    let mut used = 0;
    let mut keep = vec![false; list.len()];
    for &i in &order {
        let n = cache.get(&serde_json::to_string(&list[i].part).unwrap_or_default()).copied().unwrap_or(0);
        if used + n <= budget {
            used += n;
            keep[i] = true;
        }
    }
    let before = list.len();
    let mut k = keep.into_iter();
    list.retain(|_| k.next().unwrap_or(false));
    before - list.len()
}

/// Parts between points need at least a span; others a footprint.
pub fn check_part(part: &Part) -> Result<(), String> {
    part.validate()?;
    if part.kind.info().between_points && part.kind != Kind::Spring {
        let pts: Vec<Vec3> = part.points.iter().map(|p| Vec3::from(*p)).collect();
        let span: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
        if span < part.tube_radius() * 2.0 {
            return Err(format!("{} is shorter than its width ({span:.4} m)", part.kind.name()));
        }
    }
    Ok(())
}
