//! Primitive meshes (all fit a unit box centered on the origin) and ray picking.
//!
//! Triangles wind counter-clockwise seen from outside (`orient_faces`).
//! The renderer draws both sides and turns normals toward the camera, so the
//! viewport would hide a backward triangle; exported files would not.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use glam::{Mat4, Vec3};

use crate::bake;
use crate::clay::Clay;
use crate::material::Material;
use crate::rig::{self, Bone, Rig, Skin};
use crate::eyeball::Eye;
use crate::hair::Hair;
use crate::scene::{Object, Scene, Shape};

/// Which GPU mesh an object draws with: a shared primitive, or its own
/// mesh (clay, eye).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum MeshKey {
    Prim(Shape),
    Own(u64),
}

/// What an eye's mesh is built from: its settings, its glow and (with
/// lids) its placement.
type EyeKey = (Eye, f32, Mat4);
type SolidKey = (Shape, Material, Vec<crate::smart::Decal>);

/// Every mesh the scene needs. Clay meshes are rebuilt when their blobs
/// change; rigged clay is skinned and posed for `frame`.
#[derive(Clone)]
pub struct MeshLib {
    #[cfg(test)]
    pub geometry_builds: usize,
    #[cfg(test)]
    pub shading_updates: usize,
    prims: HashMap<Shape, Arc<Mesh>>,
    solids: HashMap<u64, (SolidKey, Arc<Mesh>)>,
    clay: HashMap<u64, ClayEntry>,
    /// Eye meshes by object, with the glow (emission) they were built with.
    eyes: HashMap<u64, (EyeKey, Arc<Mesh>)>,
    hairs: HashMap<u64, (Hair, Arc<Mesh>)>,
    fires: HashMap<u64, (crate::fire::Settings, usize, usize, Arc<Mesh>)>,
    /// Kit pieces and terrains, by object, with what they were built from.
    kits: HashMap<u64, (EnvKey, Arc<Mesh>)>,
    /// Local bounds of scatter objects (their copies' meshes).
    scatter_bounds: HashMap<u64, (Vec3, Vec3)>,
    /// Time-slider frame to pose rigs at; None shows every rig at rest.
    pub frame: Option<f32>,
    /// Draw baked game meshes (where a fresh bake exists) instead of clay.
    pub show_baked: bool,
    /// Draw fine meshes (clay with a `detail_cell`) instead of clay.
    pub show_fine: bool,
    /// Clay meshes and fine meshes being built on other threads.
    pending: Pending,
}

/// What a kit piece or terrain mesh was built from.
#[derive(Clone, PartialEq)]
enum EnvKey {
    Kit(crate::envkit::kit::Kit, Option<Material>, Vec<Material>, Vec<crate::smart::Decal>),
    Terrain(crate::envkit::terrain::Terrain, Option<Material>, Vec<Material>),
    Part(crate::kitbash::Part, Vec<Material>),
}

/// Background builds by object id. Not copied with the library (a copy is
/// a snapshot for an export, which needs no builds in flight).
#[derive(Default)]
struct Pending {
    /// (what it builds for, object name, start, the thread).
    clay: HashMap<u64, Build<BuildKey, Built>>,
    /// Fine meshes, by the rest mesh they are made from.
    fine: HashMap<u64, Build<Arc<Mesh>, Result<Baked, String>>>,
}

type Build<K, T> = (K, String, std::time::Instant, std::thread::JoinHandle<T>);

impl Clone for Pending {
    fn clone(&self) -> Self {
        Pending::default()
    }
}

/// Everything a clay entry's meshes are built from.
#[derive(Clone, PartialEq)]
struct BuildKey {
    clay: Clay,
    cloth: Option<crate::cloth::Cloth>,
    palette: Vec<Material>,
    library: Vec<Material>,
    fallback: String,
    /// The bones and split mode, for rigged clay.
    rig: Option<(Vec<Bone>, bool, Vec<rig::WeightStroke>)>,
}

/// A clay entry's built meshes.
struct Built {
    fine: Option<Baked>,
    whole: Arc<Mesh>,
    rest: Arc<Mesh>,
    parts: Vec<(u32, Vec<bool>)>,
    skin: Option<(Vec<Bone>, bool, Arc<Skin>)>,
}

impl BuildKey {
    fn same_geometry(&self, other: &Self) -> bool {
        if self.cloth != other.cloth { return false; }
        let mut a = self.clay.clone();
        let mut b = other.clay.clone();
        for c in [&mut a, &mut b] {
            c.paint = None;
            c.masks.clear();
            for blob in &mut c.blobs { blob.material.clear(); blob.color = [0.0; 3]; }
        }
        a == b
    }

    /// Builds the meshes; `whole` reuses an existing shell when only the
    /// rig changed.
    fn build(&self, whole: Option<Arc<Mesh>>) -> Built {
        let whole = whole.unwrap_or_else(|| Arc::new(self.clay.build_mesh_with(&self.library, &self.fallback)));
        let Some((bones, split, weights)) = &self.rig else { return Built { fine: None, rest: whole.clone(), whole, parts: Vec::new(), skin: None } };
        let rig = Rig { bones: bones.clone(), clips: Vec::new(), active: None, split: *split, weights: weights.clone() };
        if *split {
            // Split rigs replace the rest mesh with one shell per part.
            let (mesh, skin, parts) = rig::build_parts(&self.clay, &rig, &whole, &self.library, &self.fallback);
            Built { fine: None, rest: Arc::new(mesh), whole, parts, skin: Some((bones.clone(), true, Arc::new(skin))) }
        } else {
            let skin = rig::skin(&self.clay, &whole, &rig);
            Built { fine: None, rest: whole.clone(), whole, parts: Vec::new(), skin: Some((bones.clone(), false, Arc::new(skin))) }
        }
    }
}

#[derive(Clone)]
struct ClayEntry {
    /// What the meshes below were built from.
    key: BuildKey,
    rest: Arc<Mesh>,
    /// The clay as one shell (`rest` unless a split rig replaced it).
    whole: Arc<Mesh>,
    /// The bones and split mode `skin` (and a split `rest`) were made for.
    skin: Option<(Vec<Bone>, bool, Arc<Skin>)>,
    /// What is drawn: `rest`, or `rest` posed for `posed_for`.
    shown: Arc<Mesh>,
    posed_for: Option<(Rig, Option<f32>)>,
    /// Split rigs: each part's first vertex in `rest` and its prims.
    parts: Vec<(u32, Vec<bool>)>,
    baked: Option<Baked>,
    /// Whether `baked.shown` is drawn instead of `shown`.
    use_baked: bool,
    /// The clay at its `detail_cell` (see `Clay::build_fine`), made when shown.
    fine: Option<Baked>,
    /// Whether `fine.shown` is drawn instead of `shown`.
    use_fine: bool,
    /// Why the fine mesh for this rest mesh could not be built (it is not
    /// tried again until the clay changes).
    fine_error: Option<(Arc<Mesh>, String)>,
}

/// A mesh made from a clay entry's `rest`: a baked game mesh (see
/// `bake.rs`) or a fine mesh.
#[derive(Clone)]
struct Baked {
    settings: bake::Settings,
    /// The rest mesh it was baked from; a new one makes it stale.
    from: Arc<Mesh>,
    mesh: Arc<Mesh>,
    skin: Option<Arc<Skin>>,
    shown: Arc<Mesh>,
    posed_for: Option<(Rig, Option<f32>)>,
}

impl MeshLib {
    pub fn new() -> Self {
        Self { #[cfg(test)] geometry_builds:0, #[cfg(test)] shading_updates:0, solids: HashMap::new(), prims: Mesh::all().into_iter().map(|(s, m)| (s, Arc::new(m))).collect(), clay: HashMap::new(), eyes: HashMap::new(), hairs: HashMap::new(), fires: HashMap::new(), kits: HashMap::new(), scatter_bounds: HashMap::new(), frame: None, show_baked: false, show_fine: false, pending: Pending::default() }
    }

    pub fn prims(&self) -> impl Iterator<Item = (MeshKey, Arc<Mesh>)> + '_ {
        self.prims.iter().map(|(s, m)| (MeshKey::Prim(*s), m.clone()))
    }

    /// Brings every mesh up to date with the scene, waiting for any build
    /// (what tools use: they read the meshes right after).
    pub fn sync(&mut self, scene: &Scene) {
        self.sync_with(scene, true);
    }

    /// Like `sync`, but slow builds (clay meshes, skin weights, fine meshes)
    /// run on other threads; until they finish the old meshes stay shown.
    /// What the window calls every frame.
    pub fn sync_background(&mut self, scene: &Scene) {
        self.sync_with(scene, false);
    }

    /// Whether builds are running (see `sync_background`).
    pub fn busy(&self) -> bool {
        !self.pending.clay.is_empty() || !self.pending.fine.is_empty()
    }

    /// Running builds: (what, seconds so far).
    pub fn building(&self) -> Vec<(String, f32)> {
        let clay = self.pending.clay.values().map(|(_, n, t, _)| (format!("Building mesh {n}"), t.elapsed().as_secs_f32()));
        clay.chain(self.pending.fine.values().map(|(_, n, t, _)| (format!("Building fine mesh {n}"), t.elapsed().as_secs_f32()))).collect()
    }

    fn sync_with(&mut self, scene: &Scene, wait: bool) {
        let objects = &scene.objects;
        let live: std::collections::HashSet<u64> = objects.iter().map(|o| o.id).collect();
        self.eyes.retain(|id, _| live.contains(id));
        self.hairs.retain(|id, _| live.contains(id));
        self.fires.retain(|id, _| live.contains(id));
        for o in objects.iter().filter(|o| o.shape == Shape::Fire) {
            let Some(f) = o.fire.as_ref().filter(|f| !f.cache.is_empty()) else { self.fires.remove(&o.id); continue };
            let frame = f.frame_index(scene.frame);
            let unchanged=self.fires.get(&o.id).is_some_and(|(old, _, ptr, _)| old == &f.settings && *ptr == f.cache.as_ptr() as usize);
            if !unchanged {
                self.fires.insert(o.id, (f.settings.clone(), usize::MAX, f.cache.as_ptr() as usize, Arc::new(crate::fire::cards(f, None, true))));
            }
            if let Some((_, previous, _, mesh))=self.fires.get_mut(&o.id) && *previous!=frame {
                let mut preview=(**mesh).clone();
                let frames=f.cache.len().min(16);let tile=frame*(frames-1)/f.cache.len().saturating_sub(1).max(1);
                let step=64.0/preview.textures.as_ref().unwrap().size as f32;
                for (i,v) in preview.vertices.iter_mut().enumerate(){v.uv[0]=(tile as f32+if i%4==1 || i%4==2 {1.0}else{0.0})*step;}
                *mesh=Arc::new(preview);*previous=frame;
            }
        }
        self.kits.retain(|id, _| live.contains(id));
        for o in objects.iter().filter(|o| o.shape == Shape::Hair) {
            let hair = o.hair.clone().unwrap_or_default();
            if !self.hairs.get(&o.id).is_some_and(|(h, _)| *h == hair) {
                let mesh = Arc::new(hair.build_mesh());
                self.hairs.insert(o.id, (hair, mesh));
            }
        }
        for o in objects.iter().filter(|o| o.shape == Shape::Eye) {
            // An eye keeps its own sclera and cornea; a glowing object
            // material (glowing eyes) lights both.
            // Lids sit still in the parent while the eye turns, so the
            // eye's own placement is part of what its mesh depends on.
            let eye = o.eye.clone().unwrap_or_default();
            let place = if eye.lids.is_some() { o.local_matrix() } else { Mat4::IDENTITY };
            let key = (eye, scene.material_of(o).emission, place);
            if !self.eyes.get(&o.id).is_some_and(|(k, _)| *k == key) {
                let mut mesh = key.0.build_mesh_in(key.0.lids.is_some().then(|| place.inverse()));
                if key.1 > 0.0 {
                    mesh.materials.iter_mut().for_each(|m| m.emission = key.1);
                    let lights: Vec<[f32; 2]> = mesh.materials.iter().map(Material::light).collect();
                    mesh.vertices.iter_mut().for_each(|v| v.light = lights[v.material as usize]);
                }
                self.eyes.insert(o.id, (key, Arc::new(mesh)));
            }
        }
        let live: std::collections::HashSet<u64> = objects.iter().filter(|o| matches!(o.shape, Shape::Clay | Shape::Cloth)).map(|o| o.id).collect();
        self.clay.retain(|id, _| live.contains(id));
        self.pending.clay.retain(|id, _| live.contains(id));
        self.pending.fine.retain(|id, _| live.contains(id));
        for o in objects.iter().filter(|o| o.shape == Shape::Clay) {
            let clay = o.clay.clone().unwrap_or_default();
            let fallback = o.material.clone().unwrap_or_default();
            let mut library = crate::damage::library(scene, o);
            let names = clay.material_names();
            library.retain(|m| names.iter().any(|n| m.name.as_str() == if n.is_empty() { fallback.as_str() } else { n.as_str() }));
            let palette = clay.palette(&library, &fallback);
            let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
            let key = BuildKey { clay, cloth: None, palette, library, fallback, rig: rig.map(|r| (r.bones.clone(), r.split, r.weights.clone())) };
            if !self.clay.get(&o.id).is_some_and(|e| e.key == key) {
                if !self.pending.clay.get(&o.id).is_some_and(|p| p.0 == key || (p.0.same_geometry(&key) && p.0.rig==key.rig)) {
                    // Only the rig changed: keep the shell, redo the skin.
                    let reuse = self.clay.get(&o.id).filter(|e| e.key.clay == key.clay && e.key.palette == key.palette && e.key.library == key.library && e.key.fallback == key.fallback).map(|e| e.whole.clone());
                    let shading = self.clay.get(&o.id).filter(|e| e.key.same_geometry(&key) && e.key.rig == key.rig)
                        .map(|e| (e.whole.clone(), e.rest.clone(), e.parts.clone(), e.skin.clone(), e.fine.as_ref().filter(|f|Arc::ptr_eq(&f.from,&e.rest)).cloned()));
                    #[cfg(test)]
                    {if shading.is_some(){self.shading_updates+=1;}else if reuse.is_none(){self.geometry_builds+=1;}}
                    let k = key.clone();
                    let handle = std::thread::Builder::new().name(format!("mesh {}", o.name)).spawn(move || {
                        if let Some((whole, rest, parts, skin, fine)) = shading {
                            let shared = Arc::ptr_eq(&whole, &rest);
                            let whole = Arc::new(crate::mesh_shading::refresh(&k.clay, &k.library, &k.fallback, &whole, &[]));
                            let rest = if shared { whole.clone() } else {
                                Arc::new(crate::mesh_shading::refresh(&k.clay, &k.library, &k.fallback, &rest, &parts))
                            };
                            let fine=fine.map(|mut f| {
                                let refreshed=Arc::new(crate::mesh_shading::refresh(&k.clay,&k.library,&k.fallback,&f.mesh,&[]));
                                f.mesh=refreshed.clone();f.shown=refreshed;f.from=rest.clone();f.posed_for=None;f
                            });
                            Built { fine, whole, rest, parts, skin }
                        } else { k.build(reuse) }
                    }).expect("mesh thread starts");
                    // A build for older settings is dropped (it finishes unseen).
                    self.pending.clay.insert(o.id, (key.clone(), o.name.clone(), std::time::Instant::now(), handle));
                }
                let ready = self.pending.clay.get(&o.id).is_some_and(|p| p.3.is_finished());
                if wait || ready {
                    let (built_key, _, _, handle) = self.pending.clay.remove(&o.id).expect("pending");
                    let built = handle.join().expect("mesh build");
                    let stale = built_key != key;
                    self.install(o.id, built_key, built);
                    if stale && wait { self.sync_with(scene, true); return; }
                }
            }
            let Some(e) = self.clay.get_mut(&o.id) else { continue };
            // Pose only with a skin made for these bones (one may be on its way).
            let Some(rig) = rig.filter(|r| e.skin.as_ref().is_some_and(|(b, _, _)| *b == r.bones)) else {
                if e.posed_for.is_some() || !Arc::ptr_eq(&e.shown, &e.rest) {
                    e.shown = e.rest.clone();
                    e.posed_for = None;
                }
                e.use_baked = self.show_baked && o.bake.is_some() && e.baked.as_ref().is_some_and(|b| Arc::ptr_eq(&b.from, &e.rest));
                if let Some(b) = e.baked.as_mut().filter(|_| e.use_baked) {
                    b.shown = b.mesh.clone();
                    b.posed_for = None;
                }
                continue;
            };
            let frame = if rig.is_posed(self.frame) { self.frame } else { None };
            e.use_baked = self.show_baked && o.bake.is_some() && e.baked.as_ref().is_some_and(|b| Arc::ptr_eq(&b.from, &e.rest));
            if e.use_baked {
                let b = e.baked.as_mut().expect("checked");
                if !b.posed_for.as_ref().is_some_and(|(r, f)| r == rig && *f == frame) {
                    b.shown = match (frame, &b.skin) {
                        (Some(_), Some(skin)) => Arc::new(rig::pose_mesh(&b.mesh, skin, &rig.skin_matrices(frame))),
                        _ => b.mesh.clone(),
                    };
                    b.posed_for = Some((rig.clone(), frame));
                }
                continue;
            }
            if e.posed_for.as_ref().is_some_and(|(r, f)| r == rig && *f == frame) {
                continue;
            }
            e.shown = match frame {
                None => e.rest.clone(),
                Some(_) => {
                    let skin = &e.skin.as_ref().expect("checked").2;
                    Arc::new(rig::pose_mesh(&e.rest, skin, &rig.skin_matrices(frame)))
                }
            };
            e.posed_for = Some((rig.clone(), frame));
        }
        for o in objects.iter().filter(|o| o.shape == Shape::Cloth) {
            let Some(cloth) = o.cloth.as_ref().filter(|c| c.valid()) else { self.clay.remove(&o.id); continue };
            let material = scene.material_of(o);
            let rig = scene.rig_of(o);
            // Compare saved data before cloning a potentially large animation
            // cache. Playback must not allocate another cache every frame.
            let unchanged = self.clay.get(&o.id).is_some_and(|e|
                e.key.cloth.as_ref()==Some(cloth) && e.key.palette.first()==Some(&material)
                && e.key.library==scene.materials && e.key.fallback==o.material.as_deref().unwrap_or_default()
                && e.key.rig.as_ref().map(|r|r.0.as_slice())==rig.map(|r|r.bones.as_slice()));
            if !unchanged {
                let key = BuildKey { clay: Clay::default(), cloth: Some(cloth.clone()), palette: vec![material.clone()], library: scene.materials.clone(), fallback: o.material.clone().unwrap_or_default(), rig: rig.map(|r| (r.bones.clone(), false, Vec::new())) };
                let (mesh, skin) = cloth.mesh(&cloth.positions, &material);
                let mesh = Arc::new(mesh);
                self.install(o.id, key, Built { fine: None, whole: mesh.clone(), rest: mesh, parts: Vec::new(), skin: skin.zip(rig).map(|(s,r)| (r.bones.clone(),false,Arc::new(s))) });
            }
            let e = self.clay.get_mut(&o.id).expect("installed");
            let frame = self.frame;
            let pose_key = rig.map(|r| (r.clone(),frame));
            e.use_baked = self.show_baked && o.bake.is_some() && e.baked.as_ref().is_some_and(|b| Arc::ptr_eq(&b.from,&e.rest));
            if e.posed_for != pose_key {
                e.shown = if frame.is_some() { Arc::new(cloth.mesh(&cloth.at_frame(rig,frame), &material).0) } else { e.rest.clone() };
                e.posed_for = pose_key.clone();
            }
            if let Some(b) = e.baked.as_mut().filter(|_| e.use_baked)
                && b.posed_for != pose_key {
                b.shown = if frame.is_some() && !cloth.cache.is_empty() && cloth.clip.as_ref()==rig.and_then(|r|r.active.as_ref()) {
                    Arc::new(crate::cloth::pose_reduced(&e.rest,&e.shown,&b.mesh))
                } else {match (rig,frame,&b.skin) { (Some(r),Some(_),Some(s)) => Arc::new(rig::pose_mesh(&b.mesh,s,&r.skin_matrices(frame))), _ => b.mesh.clone() }};
                b.posed_for = pose_key;
            }
        }
        self.solids.retain(|id,_|objects.iter().any(|o|o.id==*id));
        for o in objects.iter().filter(|o| !o.shape.own_mesh() && o.shape.is_mesh() && crate::damage::own_primitive(o)) {
            let m=crate::damage::world_material(scene.material_of(o),scene.world_matrix(o.id));
            let key=(o.shape,m.clone(),o.damage.clone());
            if self.solids.get(&o.id).is_some_and(|(k,_)|*k==key) {continue;}
            let Some(base)=self.prims.get(&o.shape) else {continue};
            let mut mesh=(**base).clone();mesh.materials=vec![m.clone()];
            for v in &mut mesh.vertices {v.surface=m.surface();v.light=m.light();}
            crate::damage::displace_mesh(&mut mesh,&o.damage);
            self.solids.insert(o.id,(key,Arc::new(mesh)));
        }
        self.sync_env(scene);
        self.sync_fine(scene, wait);
    }

    /// Builds kit pieces and terrains whose settings changed, and measures
    /// scatter objects.
    fn sync_env(&mut self, scene: &Scene) {
        let own_material = |o: &Object| o.material.as_ref().map(|_| crate::damage::world_material(scene.material_of(o),scene.world_matrix(o.id)));
        for o in &scene.objects {
            let key = match (o.shape, &o.kit, &o.terrain) {
                (Shape::Kit, Some(k), _) => EnvKey::Kit(k.clone(), own_material(o), crate::damage::library(scene,o), o.damage.clone()),
                (Shape::Terrain, _, Some(t)) => EnvKey::Terrain(t.clone(), own_material(o), scene.materials.clone()),
                // Two-bone parts are solved again for the pose shown.
                (Shape::Part, _, _) if o.part.is_some() => {
                    let p=crate::kitbash::span::posed(scene,o,self.frame).unwrap_or_else(||o.part.clone().expect("checked"));
                    let lib=p.roles(&scene.materials).into_iter().map(|(m,_)|m).collect();
                    EnvKey::Part(p,lib)
                },
                _ => continue,
            };
            if self.kits.get(&o.id).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            if let Some((old, source))=self.kits.get(&o.id)
                && env_geometry(old,&key) && source.unshaded.is_some() && o.damage.is_empty() {
                    let palette=env_palette(old,&key,source);
                    if palette==source.materials { self.kits.insert(o.id,(key,source.clone())); continue; }
                    let mesh=crate::mesh_shading::refresh_env(source,palette);
                    self.kits.insert(o.id,(key,Arc::new(mesh)));
                    continue;
            }
            let mut mesh = match &key {
                EnvKey::Kit(k, m, lib, _) => k.build(lib, m.as_ref()),
                EnvKey::Terrain(t, m, lib) => t.build(lib, m.as_ref()),
                EnvKey::Part(p, lib) => p.build(lib),
            };
            if !o.damage.is_empty() {crate::damage::displace_mesh(&mut mesh,&o.damage);}
            self.kits.insert(o.id, (key, Arc::new(mesh)));
        }
        self.scatter_bounds.clear();
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Scatter) {
            let Some(sc) = &o.scatter else { continue };
            let shapes: Vec<Option<(Mat4, (Vec3, Vec3))>> = sc.sources.iter().map(|&id| {
                let src = scene.get(id)?;
                let b = self.get(src).filter(|m| !m.vertices.is_empty())?.bounds();
                Some((crate::envkit::scatter::source_shape(src), b))
            }).collect();
            let mut acc: Option<(Vec3, Vec3)> = None;
            for inst in &sc.instances {
                let Some(Some((shape, (lo, hi)))) = shapes.get(inst.source as usize) else { continue };
                let m = inst.matrix() * *shape;
                for i in 0..8 {
                    let c = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
                    let p = m.transform_point3(c);
                    acc = Some(acc.map_or((p, p), |(a, b)| (a.min(p), b.max(p))));
                }
            }
            if let Some(b) = acc {
                self.scatter_bounds.insert(o.id, b);
            }
        }
    }

    /// Puts freshly built meshes in place (a new rest mesh makes bakes and
    /// fine meshes stale).
    fn install(&mut self, id: u64, key: BuildKey, b: Built) {
        match self.clay.get_mut(&id) {
            Some(e) => {
                (e.key, e.whole, e.rest, e.parts, e.skin) = (key, b.whole, b.rest, b.parts, b.skin);
                e.fine=b.fine; e.fine_error=None;
                e.shown = e.rest.clone();
                e.posed_for = None;
            }
            None => {
                let entry = ClayEntry { key, shown: b.rest.clone(), whole: b.whole, rest: b.rest, skin: b.skin, posed_for: None, parts: b.parts, baked: None, use_baked: false, fine: b.fine, use_fine: false, fine_error: None };
                self.clay.insert(id, entry);
            }
        }
    }

    /// Builds (when shown and stale) and poses fine meshes. They are skinned
    /// from the rest mesh's nearest vertex, so they move like it.
    fn sync_fine(&mut self, scene: &Scene, wait: bool) {
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Clay) {
            let Some(e) = self.clay.get_mut(&o.id) else { continue };
            let fresh = e.fine.as_ref().is_some_and(|f| Arc::ptr_eq(&f.from, &e.rest)) || e.fine_error.as_ref().is_some_and(|f| Arc::ptr_eq(&f.0, &e.rest));
            if self.show_fine && !e.use_baked && e.key.clay.detail_cell.is_some() && !fresh {
                if !self.pending.fine.get(&o.id).is_some_and(|p| Arc::ptr_eq(&p.0, &e.rest)) {
                    let (key, rest, skin) = (e.key.clone(), e.rest.clone(), e.skin.as_ref().map(|s| s.2.clone()));
                    let from = rest.clone();
                    let handle = std::thread::Builder::new()
                        .name(format!("fine mesh {}", o.name))
                        .spawn(move || {
                            let mesh = key.clay.build_fine(&key.library, &key.fallback)?;
                            let skin = skin.map(|s| {
                                let near = rig::NearestVertex::new(&rest, key.clay.cell_size().unwrap_or(0.01) * 2.0);
                                let pick: Vec<usize> = mesh.vertices.iter().map(|v| near.index(Vec3::from(v.pos)).unwrap_or(0)).collect();
                                Arc::new(Skin { joints: pick.iter().map(|&i| s.joints[i]).collect(), weights: pick.iter().map(|&i| s.weights[i]).collect() })
                            });
                            let mesh = Arc::new(mesh);
                            Ok(Baked { settings: bake::Settings::default(), from: rest, shown: mesh.clone(), mesh, skin, posed_for: None })
                        })
                        .expect("fine mesh thread starts");
                    self.pending.fine.insert(o.id, (from, o.name.clone(), std::time::Instant::now(), handle));
                }
                if wait || self.pending.fine.get(&o.id).is_some_and(|p| p.3.is_finished()) {
                    let (from, _, _, handle) = self.pending.fine.remove(&o.id).expect("pending");
                    match handle.join().expect("fine mesh build") {
                        Ok(f) => (e.fine, e.fine_error) = (Some(f), None),
                        Err(why) => (e.fine, e.fine_error) = (None, Some((from, why))),
                    }
                }
            }
            e.use_fine = self.show_fine && !e.use_baked && e.fine.as_ref().is_some_and(|f| Arc::ptr_eq(&f.from, &e.rest));
            let Some(f) = e.fine.as_mut().filter(|_| e.use_fine) else { continue };
            let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
            let frame = rig.and_then(|r| r.is_posed(self.frame).then_some(self.frame).flatten());
            let key = rig.map(|r| (r.clone(), frame));
            if f.posed_for != key {
                f.shown = match (rig, frame, &f.skin) {
                    (Some(r), Some(_), Some(skin)) => Arc::new(rig::pose_mesh(&f.mesh, skin, &r.skin_matrices(frame))),
                    _ => f.mesh.clone(),
                };
                f.posed_for = key;
            }
        }
    }

    /// Bakes a clay object into a game mesh with textures (slow: seconds),
    /// unless a bake with the same settings is still fresh. Call `sync` first.
    #[cfg(test)]
    pub fn bake(&mut self, scene: &Scene, id: u64, settings: bake::Settings) -> Result<bool, String> {
        let Some(job) = self.bake_job(scene, id, settings)? else { return Ok(false) };
        let (mesh, skin) = job.run(&Default::default())?;
        self.install_bake(&job, mesh, skin);
        Ok(true)
    }

    /// What a bake needs, owned so it can run on another thread; None if a
    /// bake with these settings is still fresh.
    pub fn bake_job(&self, scene: &Scene, id: u64, settings: bake::Settings) -> Result<Option<BakeJob>, String> {
        let o = scene.get(id).ok_or("object vanished")?;
        let e = self.clay.get(&id).ok_or("not a clay object")?;
        if e.baked.as_ref().is_some_and(|b| b.settings == settings && Arc::ptr_eq(&b.from, &e.rest)) {
            return Ok(None);
        }
        Ok(Some(BakeJob {
            id,
            settings,
            clay: e.key.clay.clone(),
            cloth: e.key.cloth.is_some(),
            rest: e.rest.clone(),
            skin: e.skin.as_ref().map(|s| s.2.clone()),
            parts: e.parts.clone(),
            library: scene.materials.clone(),
            fallback: o.material.clone().unwrap_or_default(),
            // A fine mesh already made for this rest mesh saves the bake
            // building it again.
            fine: e.fine.as_ref().filter(|f| Arc::ptr_eq(&f.from, &e.rest)).map(|f| f.mesh.clone()),
        }))
    }

    /// Keeps a finished bake, unless the clay changed while it ran.
    pub fn install_bake(&mut self, job: &BakeJob, mesh: Mesh, skin: Option<Skin>) -> bool {
        let Some(e) = self.clay.get_mut(&job.id).filter(|e| Arc::ptr_eq(&e.rest, &job.rest)) else { return false };
        let mesh = Arc::new(mesh);
        e.baked = Some(Baked { settings: job.settings, from: e.rest.clone(), shown: mesh.clone(), mesh, skin: skin.map(Arc::new), posed_for: None });
        true
    }

    /// Copies fresh bakes made on a copy of this library (an export job).
    pub fn take_bakes(&mut self, other: &MeshLib) {
        for (id, theirs) in &other.clay {
            if let (Some(e), Some(b)) = (self.clay.get_mut(id), &theirs.baked)
                && Arc::ptr_eq(&e.rest, &b.from)
                && !e.baked.as_ref().is_some_and(|mine| Arc::ptr_eq(&mine.mesh, &b.mesh))
            {
                e.baked = Some(b.clone());
            }
        }
    }

    /// The clay as one closed shell at rest (no split-rig parts).
    pub fn whole(&self, id: u64) -> Option<&Arc<Mesh>> {
        self.clay.get(&id).map(|e| &e.whole)
    }

    /// Clay objects whose fine mesh failed for their current shape, and why.
    pub fn fine_errors(&self) -> Vec<(u64, String)> {
        self.clay.iter().filter_map(|(id, e)| e.fine_error.as_ref().filter(|f| Arc::ptr_eq(&f.0, &e.rest)).map(|f| (*id, f.1.clone()))).collect()
    }

    /// The fresh baked game mesh (rest pose) and its skin, if any.
    pub fn baked(&self, id: u64) -> Option<(&Arc<Mesh>, Option<&Arc<Skin>>)> {
        let e = self.clay.get(&id)?;
        e.baked.as_ref().filter(|b| Arc::ptr_eq(&b.from, &e.rest)).map(|b| (&b.mesh, b.skin.as_ref()))
    }

    pub fn key(o: &Object) -> MeshKey {
        if o.shape.own_mesh() || crate::damage::own_primitive(o) { MeshKey::Own(o.id) } else { MeshKey::Prim(o.shape) }
    }

    /// The mesh as drawn (posed if the object's rig is playing a clip).
    pub fn get(&self, o: &Object) -> Option<&Arc<Mesh>> {
        match Self::key(o) {
            MeshKey::Prim(s) => self.prims.get(&s),
            MeshKey::Own(id) => self.solids.get(&id).map(|e|&e.1).or_else(||self.clay.get(&id).map(ClayEntry::drawn)).or_else(|| self.eyes.get(&id).map(|e| &e.1)).or_else(|| self.hairs.get(&id).map(|e| &e.1)).or_else(|| self.kits.get(&id).map(|e| &e.1)).or_else(|| self.fires.get(&id).map(|e| &e.3)),

        }
    }

    /// The clay mesh in the rest pose and its skin, if rigged.
    pub fn rest(&self, id: u64) -> Option<(&Arc<Mesh>, Option<&Arc<Skin>>)> {
        self.clay.get(&id).map(|e| (&e.rest, e.skin.as_ref().map(|s| &s.2)))
    }

    /// Local bounds of an object's mesh (None for groups and empty clay).
    pub fn bounds(&self, o: &Object) -> Option<(Vec3, Vec3)> {
        if o.shape == Shape::Scatter {
            return self.scatter_bounds.get(&o.id).copied();
        }
        self.get(o).filter(|m| !m.vertices.is_empty()).map(|m| m.bounds())
    }

    /// Every per-object mesh (clay as drawn, eyes).
    pub fn own_meshes(&self) -> Vec<(MeshKey, Arc<Mesh>)> {
        let clay = self.clay.iter().map(|(id, e)| (MeshKey::Own(*id), e.drawn().clone()));
        let eyes = self.eyes.iter().map(|(id, e)| (MeshKey::Own(*id), e.1.clone()));
        clay.chain(eyes).chain(self.solids.iter().map(|(id,e)|(MeshKey::Own(*id),e.1.clone()))).chain(self.hairs.iter().map(|(id, e)| (MeshKey::Own(*id), e.1.clone()))).chain(self.kits.iter().map(|(id, e)| (MeshKey::Own(*id), e.1.clone()))).chain(self.fires.iter().map(|(id, e)| (MeshKey::Own(*id), e.3.clone()))).collect()
    }
}

/// A bake's inputs, owned (see `MeshLib::bake_job`).
pub struct BakeJob {
    pub id: u64,
    pub settings: bake::Settings,
    clay: Clay,
    cloth: bool,
    rest: Arc<Mesh>,
    skin: Option<Arc<Skin>>,
    parts: Vec<(u32, Vec<bool>)>,
    library: Vec<Material>,
    fallback: String,
    fine: Option<Arc<Mesh>>,
}

impl BakeJob {
    /// Rough work, for sharing a progress bar: texels to bake, plus the
    /// mesh to simplify.
    pub fn cost(&self) -> f32 {
        (self.settings.texture as f32).powi(2) + self.rest.indices.len() as f32 * 2.0
    }

    pub fn run(&self, progress: &crate::progress::Progress) -> Result<(Mesh, Option<Skin>), String> {
        if self.cloth { return crate::cloth::bake(&self.rest, self.skin.as_deref(), self.settings, progress); }
        let src = bake::Source { clay: &self.clay, rest: &self.rest, skin: self.skin.as_deref(), parts: &self.parts, library: &self.library, fallback: &self.fallback, fine: self.fine.as_deref() };
        bake::bake(&src, self.settings, progress)
    }
}

impl ClayEntry {
    fn drawn(&self) -> &Arc<Mesh> {
        match (&self.baked, &self.fine) {
            (Some(b), _) if self.use_baked => &b.shown,
            (_, Some(f)) if self.use_fine => &f.shown,
            _ => &self.shown,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    /// sRGB, multiplied with the object color. White for primitives.
    pub color: [f32; 3],
    /// The material's shader values (`Material::surface`); x < 0 means "use
    /// the object's material" (primitives, whose mesh is shared).
    pub surface: [f32; 4],
    /// Index into `Mesh::materials` (unused when that is empty).
    pub material: u32,
    /// Texture coordinates (baked meshes only, see `bake.rs`).
    pub uv: [f32; 2],
    /// Tangent along +u, with the bitangent's handedness in w. w = 0 means
    /// the mesh has no textures (the shader then ignores `uv`).
    pub tangent: [f32; 4],
    /// The material's emission and transmission (`Material::light`);
    /// ignored when `surface` defers to the object.
    pub light: [f32; 2],
    /// Smart materials (`smart.rs`): the sRGB color and roughness before
    /// layers and decals (roughness < 0: the material's), what the masks
    /// read (edge, cavity, occlusion), and the painted mask channels.
    pub base: [f32; 4],
    pub wear: [f32; 3],
    pub painted: [u8; 4],
}

/// Shader values for "take the object's material" (see `Vertex::surface`).
pub const OBJECT_SURFACE: [f32; 4] = [-1.0, 0.0, 0.0, 0.0];

#[derive(Default, Clone)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Wireframe as index pairs. Quads add their 4 sides only (no diagonal) and
    /// caps add only their rim, so wireframes look like Maya's polygon edges.
    pub edges: Vec<u32>,
    /// The materials `Vertex::material` points at. Empty for primitives,
    /// which use their object's material.
    pub materials: Vec<Material>,
    /// Baked images for `Vertex::uv` (see `bake.rs`).
    pub textures: Option<Arc<Textures>>,
    /// File material values that do not occur in baked editor meshes.
    pub imported: Option<Arc<ImportedMaterial>>,
    /// Decals on clay (`smart.rs`), object space.
    pub decals: Arc<Vec<crate::smart::Decal>>,
    /// A human face on this mesh (object space; see `face.rs`): film and
    /// bakes shade its skin with regional detail.
    pub face: Option<Arc<crate::face::Face>>,
    /// Unweathered environment colors and finish width, retained for material-only updates.
    pub unshaded: Option<(Arc<Vec<[f32;3]>>, f32)>,
}

/// Extra glTF shading, kept separate from editor texture sets.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportedMaterial {
    pub emissive: [f32; 3],
    pub emissive_texture: Option<Vec<u8>>,
    pub size: u32,
    pub alpha_cutoff: Option<f32>,
    pub base_alpha: f32,
    pub vertex_alpha: Vec<f32>,
    /// glTF wrapS/wrapT for color, normal, MR, AO and emission.
    pub wraps: [[u32; 2]; 5],
}

/// A baked mesh's images: `size` x `size` RGBA8, rows from the top (v = 0).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Textures {
    pub size: u32,
    /// sRGB color (times the object color in exports).
    pub color: Vec<u8>,
    /// Tangent-space normal map (OpenGL / glTF convention: +Y up in uv).
    pub normal: Vec<u8>,
    /// R = ambient occlusion, G = roughness, B = metallic (glTF's layout).
    pub orm: Vec<u8>,
    /// The color's alpha is a cut-out mask (hair cards): drawn where >= 0.5.
    pub alpha: bool,
}

const SEGMENTS: u32 = 32;

impl Mesh {
    /// Geometric topology for operations that must cross render-normal seams.
    /// The remap expands a geometric result back to every render vertex.
    pub(crate) fn welded_geometry(&self) -> Option<(Mesh, Vec<usize>)> {
        let mut groups: HashMap<[u32;3],usize> = HashMap::new();
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();
        let mut remap=Vec::with_capacity(self.vertices.len());
        for v in &self.vertices {
            let key=v.pos.map(|x| if x==0.0 {0} else {x.to_bits()});
            let index=*groups.entry(key).or_insert_with(|| { let i=vertices.len();vertices.push(*v);normals.push(Vec3::ZERO);i });
            normals[index]+=Vec3::from(v.normal);remap.push(index);
        }
        if vertices.len()==self.vertices.len() {return None;}
        for (v,n) in vertices.iter_mut().zip(normals) {v.normal=n.normalize_or(Vec3::Y).to_array();}
        let indices=self.indices.iter().map(|&i|remap[i as usize] as u32).collect();
        Some((Mesh {vertices,indices,..Mesh::default()},remap))
    }

    pub fn all() -> HashMap<Shape, Mesh> {
        Shape::ALL.iter().map(|&s| (s, Mesh::build(s))).collect()
    }

    pub fn build(shape: Shape) -> Mesh {
        let mut mesh = match shape {
            Shape::Cube => Mesh::cube(),
            Shape::Sphere => Mesh::sphere(),
            Shape::Cylinder => Mesh::cylinder(),
            Shape::Cone => Mesh::cone(),
            Shape::Plane => Mesh::plane(),
            Shape::Torus => Mesh::torus(),
            Shape::Group | Shape::Clay | Shape::Eye | Shape::Hair | Shape::Cloth | Shape::Fire | Shape::Kit | Shape::Terrain | Shape::Scatter | Shape::Part => Mesh::default(),
        };
        mesh.dedupe_edges();
        mesh.orient_faces();
        mesh
    }

    /// Turns every triangle to wind counter-clockwise seen from the side its
    /// normals point to. The viewport does not care, but exported files do:
    /// other tools light a backward triangle from behind.
    fn orient_faces(&mut self) {
        for t in self.indices.chunks_exact_mut(3) {
            let p = |i: u32| Vec3::from(self.vertices[i as usize].pos);
            let face = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
            let normal: Vec3 = t.iter().map(|&i| Vec3::from(self.vertices[i as usize].normal)).sum();
            if face.dot(normal) < 0.0 {
                t.swap(1, 2);
            }
        }
    }

    /// Local-space bounding box.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.vertices.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), v| {
            let p = Vec3::from(v.pos);
            (lo.min(p), hi.max(p))
        })
    }

    fn edge(&mut self, a: u32, b: u32) {
        self.edges.extend_from_slice(&[a, b]);
    }

    /// Drops repeats and edges whose ends sit at the same spot (sphere poles).
    fn dedupe_edges(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let key = |v: &Vertex| v.pos.map(|c| (c * 1e4).round() as i32);
        let mut out = Vec::with_capacity(self.edges.len());
        for pair in self.edges.chunks_exact(2) {
            let (a, b) = (key(&self.vertices[pair[0] as usize]), key(&self.vertices[pair[1] as usize]));
            if a == b {
                continue;
            }
            if seen.insert(if a < b { (a, b) } else { (b, a) }) {
                out.extend_from_slice(pair);
            }
        }
        self.edges = out;
    }

    fn vert(&mut self, pos: Vec3, normal: Vec3) -> u32 {
        self.vertices.push(Vertex {
            pos: pos.into(),
            normal: normal.normalize_or_zero().into(),
            color: [1.0; 3],
            surface: OBJECT_SURFACE,
            material: 0,
            uv: [0.0; 2],
            tangent: [0.0; 4],
            light: [0.0; 2],
            base: [1.0, 1.0, 1.0, -1.0],
            wear: [0.0; 3],
            painted: [0; 4],
        });
        self.vertices.len() as u32 - 1
    }

    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.indices.extend_from_slice(&[a, b, c, a, c, d]);
        for (x, y) in [(a, b), (b, c), (c, d), (d, a)] {
            self.edge(x, y);
        }
    }

    fn cube() -> Mesh {
        let mut m = Mesh::default();
        let faces = [
            (Vec3::X, Vec3::Y, Vec3::Z),
            (Vec3::NEG_X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::Z, Vec3::X),
            (Vec3::NEG_Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (Vec3::NEG_Z, Vec3::Y, Vec3::X),
        ];
        for (n, u, v) in faces {
            let c = n * 0.5;
            let a = m.vert(c - u * 0.5 - v * 0.5, n);
            let b = m.vert(c + u * 0.5 - v * 0.5, n);
            let cc = m.vert(c + u * 0.5 + v * 0.5, n);
            let d = m.vert(c - u * 0.5 + v * 0.5, n);
            m.quad(a, b, cc, d);
        }
        m
    }

    fn sphere() -> Mesh {
        let mut m = Mesh::default();
        let rings = SEGMENTS / 2;
        for r in 0..=rings {
            let theta = PI * r as f32 / rings as f32;
            for s in 0..=SEGMENTS {
                let phi = TAU * s as f32 / SEGMENTS as f32;
                let n = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
                m.vert(n * 0.5, n);
            }
        }
        let row = SEGMENTS + 1;
        for r in 0..rings {
            for s in 0..SEGMENTS {
                let a = r * row + s;
                m.quad(a, a + 1, a + row + 1, a + row);
            }
        }
        m
    }

    /// Flat disc at height `y` facing `normal`.
    fn cap(&mut self, y: f32, normal: Vec3) {
        let center = self.vert(Vec3::new(0.0, y, 0.0), normal);
        let first = self.vertices.len() as u32;
        for s in 0..=SEGMENTS {
            let phi = TAU * s as f32 / SEGMENTS as f32;
            self.vert(Vec3::new(phi.cos() * 0.5, y, phi.sin() * 0.5), normal);
        }
        for s in 0..SEGMENTS {
            self.indices.extend_from_slice(&[center, first + s, first + s + 1]);
            self.edge(first + s, first + s + 1);
        }
    }

    fn cylinder() -> Mesh {
        let mut m = Mesh::default();
        let first = m.vertices.len() as u32;
        for s in 0..=SEGMENTS {
            let phi = TAU * s as f32 / SEGMENTS as f32;
            let n = Vec3::new(phi.cos(), 0.0, phi.sin());
            m.vert(n * 0.5 + Vec3::Y * 0.5, n);
            m.vert(n * 0.5 - Vec3::Y * 0.5, n);
        }
        for s in 0..SEGMENTS {
            let a = first + s * 2;
            m.quad(a, a + 1, a + 3, a + 2);
        }
        m.cap(0.5, Vec3::Y);
        m.cap(-0.5, Vec3::NEG_Y);
        m
    }

    fn cone() -> Mesh {
        let mut m = Mesh::default();
        // Side normal tilts up by radius/height (0.5 / 1.0).
        for s in 0..SEGMENTS {
            let p0 = TAU * s as f32 / SEGMENTS as f32;
            let p1 = TAU * (s + 1) as f32 / SEGMENTS as f32;
            let pm = (p0 + p1) * 0.5;
            let n = |p: f32| Vec3::new(p.cos(), 0.5, p.sin());
            let apex = m.vert(Vec3::Y * 0.5, n(pm));
            let a = m.vert(Vec3::new(p0.cos() * 0.5, -0.5, p0.sin() * 0.5), n(p0));
            let b = m.vert(Vec3::new(p1.cos() * 0.5, -0.5, p1.sin() * 0.5), n(p1));
            m.indices.extend_from_slice(&[apex, a, b]);
            m.edge(apex, a);
            m.edge(a, b);
        }
        m.cap(-0.5, Vec3::NEG_Y);
        m
    }

    fn plane() -> Mesh {
        let mut m = Mesh::default();
        let n = Vec3::Y;
        let a = m.vert(Vec3::new(-0.5, 0.0, -0.5), n);
        let b = m.vert(Vec3::new(0.5, 0.0, -0.5), n);
        let c = m.vert(Vec3::new(0.5, 0.0, 0.5), n);
        let d = m.vert(Vec3::new(-0.5, 0.0, 0.5), n);
        m.quad(a, b, c, d);
        m
    }

    fn torus() -> Mesh {
        let mut m = Mesh::default();
        let (major, minor) = (0.35, 0.15);
        let tube = SEGMENTS / 2;
        for s in 0..=SEGMENTS {
            let u = TAU * s as f32 / SEGMENTS as f32;
            let ring_center = Vec3::new(u.cos(), 0.0, u.sin()) * major;
            for t in 0..=tube {
                let v = TAU * t as f32 / tube as f32;
                let n = Vec3::new(u.cos() * v.cos(), v.sin(), u.sin() * v.cos());
                m.vert(ring_center + n * minor, n);
            }
        }
        let row = tube + 1;
        for s in 0..SEGMENTS {
            for t in 0..tube {
                let a = s * row + t;
                m.quad(a, a + 1, a + row + 1, a + row);
            }
        }
        m
    }

    /// Nearest hit distance along the ray (Möller–Trumbore, both sides).
    /// `dir` does not need to be normalized; the result is in units of `dir`.
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        self.ray_hit_triangle(origin, dir).map(|(t, _)| t)
    }

    /// `ray_hit`, plus which triangle was hit.
    pub fn ray_hit_triangle(&self, origin: Vec3, dir: Vec3) -> Option<(f32, usize)> {
        let mut best: Option<(f32, usize)> = None;
        for (n, tri) in self.indices.chunks_exact(3).enumerate() {
            let p = |i: u32| Vec3::from(self.vertices[i as usize].pos);
            let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
            let e1 = b - a;
            let e2 = c - a;
            let pv = dir.cross(e2);
            let det = e1.dot(pv);
            if det.abs() < 1e-9 {
                continue;
            }
            let inv = 1.0 / det;
            let s = origin - a;
            let u = s.dot(pv) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = dir.dot(q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) * inv;
            if t > 1e-5 && best.is_none_or(|b| t < b.0) {
                best = Some((t, n));
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_invalidates_skin_and_carries_through_baking() {
        use crate::rig::{WeightMode, WeightStroke};
        let mut scene = Scene::default();
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.clay = Some(serde_json::from_value(serde_json::json!({"blobs":[{"type":"capsule","a":[0,0,0],"b":[0,2,0],"radius":0.25,"bone":"base","blend":0}],"resolution":32})).unwrap());
        o.rig = Some(Rig {bones: vec![Bone {name:"base".into(),parent:None,head:Vec3::ZERO,tail:Vec3::Y,deform:true},Bone {name:"tip".into(),parent:Some("base".into()),head:Vec3::Y,tail:Vec3::Y*2.0,deform:true}],split:false,..Default::default()});
        let mut lib = MeshLib::new();
        lib.sync(&scene);
        let old = lib.rest(id).unwrap().1.unwrap().clone();
        scene.get_mut(id).unwrap().rig.as_mut().unwrap().weights.push(WeightStroke {bone:"tip".into(),points:vec![Vec3::Y*2.0],radius:0.6,strength:1.0,mode:WeightMode::Replace,space:glam::Mat4::IDENTITY});
        lib.sync(&scene);
        let (rest, skin) = lib.rest(id).unwrap();
        let skin = skin.unwrap();
        assert_ne!(skin.weights, old.weights);
        let weight = |skin: &Skin, i: usize| skin.joints[i].iter().zip(skin.weights[i]).filter(|(j,_)| **j == 1).map(|(_,w)| w).sum::<f32>();
        assert!(rest.vertices.iter().enumerate().any(|(i,v)| v.pos[1] > 1.9 && weight(skin,i) > 0.5));
        let rest = rest.clone();
        let skin = skin.clone();
        lib.bake(&scene,id,bake::Settings { triangles: 500, texture: 512, ..Default::default() }).unwrap();
        let (baked, bs) = lib.baked(id).unwrap();
        let bs = bs.unwrap();
        assert!(baked.vertices.iter().enumerate().any(|(i,v)| v.pos[1] > 1.9 && weight(bs,i) > 0.5));
        for (i,v) in baked.vertices.iter().enumerate() {
            let j = rest.vertices.iter().position(|r| r.pos == v.pos).unwrap();
            assert_eq!(bs.weights[i],skin.weights[j]);
            assert_eq!(bs.joints[i],skin.joints[j]);
        }
    }

    #[test]
    fn ray_hits_front_face_of_cube() {
        let cube = Mesh::build(Shape::Cube);
        let t = cube.ray_hit(Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z).unwrap();
        assert!((t - 4.5).abs() < 1e-5);
        assert!(cube.ray_hit(Vec3::new(2.0, 0.0, 5.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn cube_wireframe_has_twelve_edges() {
        assert_eq!(Mesh::build(Shape::Cube).edges.len() / 2, 12);
    }

    #[test]
    fn every_shape_winds_outward() {
        for (shape, m) in Mesh::all() {
            for t in m.indices.chunks_exact(3) {
                let p = |i: u32| Vec3::from(m.vertices[i as usize].pos);
                let face = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
                let normal: Vec3 = t.iter().map(|&i| Vec3::from(m.vertices[i as usize].normal)).sum();
                assert!(face.dot(normal) >= 0.0, "{shape:?} {t:?}");
            }
        }
    }

    #[test]
    fn every_shape_builds_valid_indices() {
        for (shape, m) in Mesh::all() {
            assert!(!m.indices.is_empty(), "{shape:?}");
            assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()), "{shape:?}");
            assert!(!m.edges.is_empty() && m.edges.len() % 2 == 0, "{shape:?}");
        }
    }
}


#[cfg(test)]
mod timing {
    use super::*;

    /// Times clay builds of a saved scene: `SCENE=path cargo test --release timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn time_scene_builds() {
        let path = std::env::var("SCENE").expect("SCENE");
        let scene: Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Clay) {
            let clay = o.clay.clone().unwrap_or_default();
            let fallback = o.material.clone().unwrap_or_default();
            let library = crate::damage::library(&scene, o);
            let palette = clay.palette(&library, &fallback);
            let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
            let key = BuildKey { clay, cloth: None, palette, library, fallback, rig: rig.map(|r| (r.bones.clone(), r.split, r.weights.clone())) };
            let t = std::time::Instant::now();
            let whole = Arc::new(key.clay.build_mesh_with(&key.library, &key.fallback));
            let mesh_s = t.elapsed().as_secs_f32();
            let t = std::time::Instant::now();
            let b = key.build(Some(whole.clone()));
            eprintln!("{}: mesh {mesh_s:.1}s ({} tris), skin {:.1}s skinned={}", o.name, whole.indices.len() / 3, t.elapsed().as_secs_f32(), b.skin.is_some());
            if key.clay.detail_cell.is_some() {
                let t = std::time::Instant::now();
                let fine = key.clay.build_fine(&key.library, &key.fallback).map(|m| m.indices.len() / 3).ok();
                eprintln!("  fine {:.1}s {fine:?}", t.elapsed().as_secs_f32());
            }
        }
    }
}

fn env_geometry(a:&EnvKey,b:&EnvKey)->bool {
    match (a,b) {
        (EnvKey::Kit(a,_,_,ad),EnvKey::Kit(b,_,_,bd))=>a==b && ad==bd,
        (EnvKey::Terrain(a,_,_),EnvKey::Terrain(b,_,_))=>a==b,
        (EnvKey::Part(a,_),EnvKey::Part(b,_))=> {
            let mut a=a.clone();let mut b=b.clone();
            for p in [&mut a,&mut b] {p.paint=None;p.color=None;p.materials.clear();}
            a==b
        },
        _=>false,
    }
}
fn env_palette(a:&EnvKey,b:&EnvKey,source:&Mesh)->Vec<Material> {
    source.materials.iter().enumerate().map(|(i,old)|match (a,b) {
        (EnvKey::Part(a,al),EnvKey::Part(b,bl))=> {
            let roles=a.roles(al);let new=b.roles(bl);
            roles.iter().position(|(m,_)|m.name==old.name).map_or_else(||old.clone(),|i|new[i].0.clone())
        },
        (EnvKey::Kit(_,old_own,_,_),EnvKey::Kit(_,own,lib,_))=> {
            if old_own.as_ref().is_some_and(|m|m.name==old.name) || (old_own.is_none() && i==0 && own.is_some()) {
                own.clone().unwrap_or_else(||crate::envkit::kit::builtin("stone"))
            } else {lib.iter().find(|m|m.name==old.name).cloned().unwrap_or_else(||crate::envkit::kit::builtin(&old.name))}
        },
        (EnvKey::Terrain(_,_,_),EnvKey::Terrain(_,own,lib))=>own.clone().or_else(||lib.iter().find(|m|m.name==old.name).cloned()).unwrap_or_else(crate::envkit::terrain::ground_material),
        _=>old.clone(),
    }).collect()
}

#[cfg(test)]
impl MeshLib {
    pub fn force_legacy_material_rebuild(&mut self) {self.clay.clear();self.kits.clear();}
}
