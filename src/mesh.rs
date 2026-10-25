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
    /// One chunk of a clay drawn in chunks (a sculpt session, see
    /// `session.rs`): the object and the chunk.
    Chunk(u64, [i32; 3]),
}

mod pick;
mod session;
pub use pick::{MeshBvh, RayHit};
pub use session::{SculptChunk, SculptVertex};

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
    /// Clay objects that took another object's meshes instead of building.
    #[cfg(test)]
    pub aliases_installed: usize,
    #[cfg(test)]
    pub fine_builds: usize,
    /// Clay builds `sync` waited for (not finished when it joined them).
    #[cfg(test)]
    pub blocking_joins: usize,
    /// Turns sharing off (every clay object builds and draws its own mesh).
    #[cfg(test)]
    pub no_sharing: bool,
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
    /// The mask and group overlay the viewport draws clay with.
    pub overlay: crate::overlay::Overlay,
    /// How long the last overlay tint took (ms).
    #[cfg(test)]
    pub overlay_ms: f64,
    /// Clay meshes and fine meshes being built on other threads.
    pending: Pending,
    /// Clay objects drawing the same mesh as a lower id: that id, whose
    /// `MeshKey` they draw with (see `key_of`).
    owner_of: HashMap<u64, u64>,
    /// Clays drawn in chunks (sculpt sessions), and the sessions.
    chunked: HashMap<u64, session::Chunked>,
    sessions: session::Sessions,
    /// Clays a sculpt tool is active on (`set_sculpt_tool`).
    sculpt_tool: session::ToolOn,
    /// Clays drawn as a stroke being drawn leaves them (`set_preview`).
    previews: session::Previews,
    /// The session clock (None: now); tests set it.
    pub session_now: Option<std::time::Instant>,
    /// Ray-picking trees of the meshes picked (see `pick.rs`).
    bvhs: pick::BvhCache,
    /// The last chunk build's and draw-ready refresh's times (ms).
    chunk_timing: (f64, f64),
    /// Clays whose regular build waits for their strokes to pause (see
    /// `sculpting_now`); `busy` while any does.
    deferred: std::collections::HashSet<u64>,
    /// Chunks built and made draw-ready.
    #[cfg(test)]
    pub chunk_builds: usize,
    #[cfg(test)]
    pub slim_updates: usize,
}

/// What a kit piece or terrain mesh was built from.
#[derive(Clone, PartialEq)]
enum EnvKey {
    Kit(crate::envkit::kit::Kit, Option<Material>, Vec<Material>, Vec<crate::smart::Decal>),
    Terrain(crate::envkit::terrain::Terrain, Option<Material>, Vec<Material>),
    Part(crate::kitbash::Part, Vec<Material>),
    /// An imported mesh (`Shape::Mesh`), drawn in its object's material.
    Imported(crate::mesh_asset::MeshAsset),
}

/// Background builds by object id. Not copied with the library (a copy is
/// a snapshot for an export, which needs no builds in flight).
#[derive(Default)]
struct Pending {
    /// (what it builds for, object name, start, the thread).
    clay: HashMap<u64, Build<BuildKey, Built>>,
    /// Fine meshes, by `ClayEntry::fine_slot` (the rest mesh and skin they
    /// are made from, held in the build so they stay put): objects sharing
    /// both share its build. Also where each one is handed out (see
    /// `MeshLib::fine_building`).
    fine: HashMap<(usize, usize), (Build<(Arc<Mesh>, Option<Arc<Skin>>), Result<Baked, String>>, FineTicket)>,
    /// Clay objects waiting for another object's build of the same key:
    /// (the object building it, the key).
    alias: HashMap<u64, (u64, BuildKey)>,
    /// `content_hash` of each clay build's key, when it can be shared.
    hashes: HashMap<u64, u64>,
}

/// A fine mesh (with its skin) or why it could not be built.
pub type FineResult = Result<(Arc<Mesh>, Option<Arc<Skin>>), String>;

/// Where a fine mesh build leaves its result for anyone waiting on it
/// (a film render waits on its own thread, not the window's).
#[derive(Clone, Default)]
pub struct FineTicket(Arc<(std::sync::Mutex<Option<FineResult>>, std::sync::Condvar)>);

impl FineTicket {
    fn finish(&self, result: FineResult) {
        let (slot, ready) = &*self.0;
        let mut slot = slot.lock().expect("no panics while locked");
        slot.get_or_insert(result);
        ready.notify_all();
    }

    /// A ticket already holding `result`.
    #[cfg(test)]
    pub fn done(result: FineResult) -> Self {
        let ticket = Self::default();
        ticket.finish(result);
        ticket
    }

    /// The result once built; None while still building at `deadline`.
    pub fn wait_until(&self, deadline: std::time::Instant) -> Option<FineResult> {
        let (slot, ready) = &*self.0;
        let mut slot = slot.lock().expect("no panics while locked");
        loop {
            if let Some(r) = slot.as_ref() {
                return Some(r.clone());
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            slot = ready.wait_timeout(slot, left).expect("no panics while locked").0;
        }
    }
}

/// Finishes a ticket even if its build panics.
struct Done(FineTicket);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.finish(Err("the fine mesh build stopped".into()));
    }
}

/// Fine builds that run at once; more wait for a slot. Each holds a grid of
/// up to millions of blocks, and they share the cores anyway.
const FINE_AT_ONCE: usize = 2;

static FINE_RUNNING: (std::sync::Mutex<usize>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// One of the `FINE_AT_ONCE` slots, held while a fine mesh builds.
struct FineSlot;

impl FineSlot {
    fn take() -> Self {
        let (running, free) = &FINE_RUNNING;
        let mut n = running.lock().expect("no panics while locked");
        while *n >= FINE_AT_ONCE {
            n = free.wait(n).expect("no panics while locked");
        }
        *n += 1;
        FineSlot
    }
}

impl Drop for FineSlot {
    fn drop(&mut self) {
        let (running, free) = &FINE_RUNNING;
        *running.lock().expect("no panics while locked") -= 1;
        free.notify_one();
    }
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

/// What a clay object's meshes are built from.
fn clay_key(scene: &Scene, o: &Object) -> BuildKey {
    // Stroke ledgers do not shape the mesh (the grids they built do): a
    // record alone (a label, a disabled stroke replayed to the same grid)
    // rebuilds nothing, and keys never serialize them. The sculpt and
    // paint layers do, through their composites (`layers.rs`), which keep
    // their identity while nothing changes.
    let mut clay = o.clay.as_ref().map(crate::layers::composed_clay).unwrap_or_default();
    // Nor do the mask and groups: the viewport tints and clips the built
    // mesh by them (`overlay`), with no build.
    (clay.sculpt_mask, clay.groups) = (None, None);
    let fallback = o.material.clone().unwrap_or_default();
    let mut library = crate::damage::library(scene, o);
    let names = clay.material_names();
    library.retain(|m| names.iter().any(|n| m.name.as_str() == if n.is_empty() { fallback.as_str() } else { n.as_str() }));
    let palette = clay.palette(&library, &fallback);
    let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
    BuildKey { clay, cloth: None, palette, library, fallback, rig: rig.map(|r| (r.bones.clone(), r.split, r.weights.clone())) }
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
    /// A hash of everything the meshes are built from, for finding other
    /// objects with the same key quickly; None for keys that never share
    /// (rigged clay, whose shown mesh is posed per object, and cloth).
    fn content_hash(&self) -> Option<u64> {
        if self.rig.is_some() || self.cloth.is_some() {
            return None;
        }
        struct Hashing(std::collections::hash_map::DefaultHasher);
        impl std::io::Write for Hashing {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                std::hash::Hasher::write(&mut self.0, bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Sculpt and paint grids are compared by identity (see
        // `clay::Detail`), so their addresses stand in for their bricks:
        // serializing those would encode every brick each stroke.
        let mut clay = self.clay.clone();
        // Mesh assets are named by their hash in the blobs already.
        clay.assets.clear();
        let grids = (clay.detail.take().map(|d| d.addr()), clay.paint.take().map(|p| p.addr()), std::mem::take(&mut clay.masks).into_iter().map(|(n, m)| (n, m.addr())).collect::<Vec<_>>());
        let mut h = Hashing(Default::default());
        serde_json::to_writer(&mut h, &(&clay, grids, &self.palette, &self.library, &self.fallback)).ok()?;
        Some(std::hash::Hasher::finish(&h.0))
    }

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
    /// `key.content_hash()`: entries with the same one may share meshes.
    hash: Option<u64>,
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
    /// The drawn mesh with the mask and group overlay (see `overlay.rs`),
    /// and what it was made from: (drawn mesh, mask, groups, overlay).
    tinted: Option<(TintKey, Arc<Mesh>)>,
}

type TintKey = (usize, Option<usize>, Option<usize>, crate::overlay::Overlay);

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
        Self { #[cfg(test)] geometry_builds:0, #[cfg(test)] shading_updates:0, #[cfg(test)] aliases_installed:0, #[cfg(test)] fine_builds:0, #[cfg(test)] blocking_joins: 0, #[cfg(test)] no_sharing:false, #[cfg(test)] overlay_ms: 0.0, overlay: crate::overlay::Overlay::default(), owner_of: HashMap::new(), solids: HashMap::new(), prims: Mesh::all().into_iter().map(|(s, m)| (s, Arc::new(m))).collect(), clay: HashMap::new(), eyes: HashMap::new(), hairs: HashMap::new(), fires: HashMap::new(), kits: HashMap::new(), scatter_bounds: HashMap::new(), frame: None, show_baked: false, show_fine: false, pending: Pending::default(), chunked: HashMap::new(), sessions: Default::default(), sculpt_tool: Default::default(), previews: Default::default(), session_now: None, bvhs: Default::default(), chunk_timing: (0.0, 0.0), deferred: Default::default(), #[cfg(test)] chunk_builds: 0, #[cfg(test)] slim_updates: 0 }
    }

    pub fn prims(&self) -> impl Iterator<Item = (MeshKey, Arc<Mesh>)> + '_ {
        self.prims.iter().map(|(s, m)| (MeshKey::Prim(*s), m.clone()))
    }

    /// Brings every mesh up to date with the scene, waiting for any build
    /// (what tools use: they read the meshes right after).
    pub fn sync(&mut self, scene: &Scene) {
        self.sync_with(scene, true, true);
    }

    /// Like `sync`, but fine meshes only start building (see
    /// `fine_building`): a film render waits for them on its own thread.
    pub fn sync_shown(&mut self, scene: &Scene) {
        self.sync_with(scene, true, false);
    }

    /// Like `sync`, but slow builds (clay meshes, skin weights, fine meshes)
    /// run on other threads; until they finish the old meshes stay shown.
    /// What the window calls every frame.
    pub fn sync_background(&mut self, scene: &Scene) {
        self.sync_with(scene, false, false);
    }

    /// Whether builds are running (see `sync_background`).
    pub fn busy(&self) -> bool {
        !self.pending.clay.is_empty() || !self.pending.fine.is_empty() || !self.deferred.is_empty()
    }

    /// Whether `sync` would build anything now: a clay's meshes are not
    /// for its shape as it is (its build not started yet, or deferred),
    /// or a fine mesh it shows is not built. `busy` covers builds already
    /// running; the window waits for neither before a call that syncs.
    pub fn stale(&self, scene: &Scene) -> bool {
        scene.objects.iter().filter(|o| o.shape == Shape::Clay).any(|o| match self.clay.get(&o.id) {
            None => true,
            Some(e) => {
                let fine = e.fine.as_ref().is_some_and(|f| Arc::ptr_eq(&f.from, &e.rest)) || e.fine_error.as_ref().is_some_and(|f| Arc::ptr_eq(&f.0, &e.rest));
                e.key != clay_key(scene, o) || (self.show_fine && !e.use_baked && e.key.clay.detail_cell.is_some() && !fine)
            }
        })
    }

    /// `sync`, then joins sculpt sessions' chunk builds (which `sync`
    /// leaves running: tools never read chunks).
    #[cfg(test)]
    pub fn sync_joined(&mut self, scene: &Scene) {
        self.sync(scene);
        let clays: Vec<&Object> = scene.objects.iter().filter(|o| o.shape == Shape::Clay).collect();
        let keys: Vec<BuildKey> = clays.iter().map(|o| clay_key(scene, o)).collect();
        self.sync_chunks(&clays, &keys, true);
    }

    /// Running builds: (what, seconds so far).
    pub fn building(&self) -> Vec<(String, f32)> {
        let clay = self.pending.clay.values().map(|(_, n, t, _)| (format!("Building mesh {n}"), t.elapsed().as_secs_f32()));
        clay.chain(self.pending.fine.values().map(|((_, n, t, _), _)| (format!("Building fine mesh {n}"), t.elapsed().as_secs_f32()))).collect()
    }

    fn sync_with(&mut self, scene: &Scene, wait: bool, wait_fine: bool) {
        self.deferred.clear();
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
        self.pending.alias.retain(|id, _| live.contains(id));
        self.pending.hashes.retain(|id, _| live.contains(id));
        let clays: Vec<&Object> = objects.iter().filter(|o| o.shape == Shape::Clay).collect();
        let keys: Vec<BuildKey> = clays.iter().map(|o| clay_key(scene, o)).collect();
        // Sculpt sessions' chunks first: in the background, the regular
        // mesh of a clay a sculpt tool is on starts building once its
        // chunks are drawn. Nothing waits for chunks here (see
        // `sync_joined`). A clay with a live preview draws the preview.
        self.previews.shown.retain(|id, _| live.contains(id));
        if self.previews.shown.is_empty() {
            self.sync_chunks(&clays, &keys, false);
        } else {
            let shown: Vec<Object> = clays.iter().filter_map(|o| self.previews.shown.get(&o.id).map(|(_, c)| Object { clay: Some(c.clone()), ..(*o).clone() })).collect();
            let chunk_clays: Vec<&Object> = clays.iter().map(|o| shown.iter().find(|s| s.id == o.id).unwrap_or(o)).collect();
            let chunk_keys: Vec<BuildKey> = chunk_clays.iter().zip(&keys).map(|(o, k)| if self.previews.shown.contains_key(&o.id) { clay_key(scene, o) } else { k.clone() }).collect();
            self.sync_chunks(&chunk_clays, &chunk_keys, false);
        }
        // Objects with the same key share one build and one set of meshes
        // (duplicates cost nothing until edited). Hashes of the keys still
        // to build, and who builds each one.
        let sharing = self.sharing();
        // (Hashed once per key: builds running for it keep theirs.)
        let hashes: Vec<Option<u64>> = clays.iter().zip(&keys).map(|(o, key)| {
            if !sharing || self.clay.get(&o.id).is_some_and(|e| e.key == *key) || self.pending.alias.get(&o.id).is_some_and(|a| a.1 == *key) {
                None
            } else if self.pending.clay.get(&o.id).is_some_and(|p| p.0 == *key) {
                self.pending.hashes.get(&o.id).copied()
            } else {
                key.content_hash()
            }
        }).collect();
        let mut building: HashMap<u64, u64> = HashMap::new();
        for ((o, key), h) in clays.iter().zip(&keys).zip(&hashes) {
            if let Some(h) = h && self.pending.clay.get(&o.id).is_some_and(|p| p.0 == *key) {
                building.entry(*h).or_insert(o.id);
            }
        }
        // Every stale build starts first, then they are joined: objects
        // build at once (sharing the cores, see `clay::par_chunks`).
        for ((o, key), h) in clays.iter().zip(&keys).zip(&hashes) {
            if self.clay.get(&o.id).is_some_and(|e| e.key == *key) {
                self.pending.alias.remove(&o.id);
                // Back to the meshes shown (an undo while a build ran): that
                // build is for older settings and is dropped, or `busy`
                // would never clear.
                if self.pending.clay.remove(&o.id).is_some() {
                    self.pending.hashes.remove(&o.id);
                }
                continue;
            }
            // Still waiting for another object's build of this key.
            if self.pending.alias.get(&o.id).is_some_and(|a| a.1 == *key) {
                continue;
            }
            if let Some(h) = *h {
                // Another object has these meshes: take them.
                if let Some(owner) = self.clay.iter().find(|(id, e)| **id != o.id && e.hash == Some(h) && e.key == *key).map(|(id, _)| *id) {
                    self.share(o.id, owner);
                    continue;
                }
                // Another object is building them: wait for that build.
                if let Some(&owner) = building.get(&h).filter(|&&b| b != o.id) {
                    self.pending.clay.remove(&o.id);
                    self.pending.alias.insert(o.id, (owner, key.clone()));
                    continue;
                }
            }
            self.pending.alias.remove(&o.id);
            // A clay being sculpted builds its regular mesh once its strokes
            // pause (its chunks show them meanwhile): each would start a
            // whole remesh, competing with the chunks for the cores.
            if !wait && self.sculpting_now(o.id) {
                self.deferred.insert(o.id);
                continue;
            }
            if !self.pending.clay.get(&o.id).is_some_and(|p| p.0 == *key || (p.0.same_geometry(key) && p.0.rig==key.rig)) {
                // Only the rig changed: keep the shell, redo the skin.
                let reuse = self.clay.get(&o.id).filter(|e| e.key.clay == key.clay && e.key.palette == key.palette && e.key.library == key.library && e.key.fallback == key.fallback).map(|e| e.whole.clone());
                // A shell shared with another object (see `share`) is copied
                // before a rig skins it: the rest mesh it becomes is this
                // object's own, posed and weighted apart from the others.
                let copy = key.rig.is_some() && reuse.as_ref().is_some_and(|w| self.clay.iter().any(|(id, e)| *id != o.id && (Arc::ptr_eq(&e.whole, w) || Arc::ptr_eq(&e.rest, w))));
                let shading = self.clay.get(&o.id).filter(|e| e.key.same_geometry(key) && e.key.rig == key.rig)
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
                    } else { k.build(reuse.map(|w| if copy { Arc::new(Mesh::clone(&w)) } else { w })) }
                }).expect("mesh thread starts");
                // A build for older settings is dropped (it finishes unseen).
                self.pending.clay.insert(o.id, (key.clone(), o.name.clone(), std::time::Instant::now(), handle));
                match h {
                    Some(h) => self.pending.hashes.insert(o.id, *h),
                    None => self.pending.hashes.remove(&o.id),
                };
            }
            if let Some(h) = *h && self.pending.clay.get(&o.id).is_some_and(|p| p.0 == *key) {
                building.entry(h).or_insert(o.id);
            }
        }
        let mut stale = false;
        for (o, key) in clays.iter().zip(&keys) {
            if self.clay.get(&o.id).is_some_and(|e| e.key == *key) || self.pending.alias.contains_key(&o.id) {
                continue;
            }
            let ready = self.pending.clay.get(&o.id).is_some_and(|p| p.3.is_finished());
            if wait || ready {
                #[cfg(test)]
                {self.blocking_joins += usize::from(!ready);}
                let (built_key, _, _, handle) = self.pending.clay.remove(&o.id).expect("pending");
                let built = handle.join().expect("mesh build");
                stale |= built_key != *key;
                self.install(o.id, built_key, built);
            }
        }
        // Objects waiting on another's build take it once it is in; if
        // that build was dropped they build their own next time.
        let waiting: Vec<(u64, u64)> = self.pending.alias.iter().map(|(id, (owner, _))| (*id, *owner)).collect();
        for (id, owner) in waiting {
            let key = &self.pending.alias[&id].1;
            if self.clay.get(&owner).is_some_and(|e| e.key == *key) {
                self.share(id, owner);
            } else if !self.pending.clay.get(&owner).is_some_and(|p| p.0 == *key) {
                self.pending.alias.remove(&id);
                stale = true;
            }
        }
        if stale && wait { self.sync_with(scene, true, wait_fine); return; }
        for o in objects.iter().filter(|o| o.shape == Shape::Clay) {
            let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
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
        self.sync_fine(scene, wait_fine);
        self.sync_overlays(scene);
        // Who draws with whose mesh: the lowest id among objects showing
        // the same one (a tinted mesh is its own).
        self.owner_of.clear();
        let mut first: HashMap<*const Mesh, u64> = HashMap::new();
        let mut ids: Vec<u64> = self.clay.keys().copied().collect();
        ids.sort_unstable();
        // Skinned clay is drawn (posed, weight-painted) on its own.
        for id in ids.into_iter().filter(|id| self.clay[id].skin.is_none()) {
            let owner = *first.entry(Arc::as_ptr(self.clay[&id].display())).or_insert(id);
            if owner != id {
                self.owner_of.insert(id, owner);
            }
        }
    }

    /// Tints the drawn clay meshes with the mask and groups overlay (or
    /// drops the tint): a copy of the drawn mesh, made again only when
    /// the mesh, the mask, the groups or the overlay changed. No mesh is
    /// built.
    fn sync_overlays(&mut self, scene: &Scene) {
        let overlay = self.overlay;
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Clay) {
            let Some(e) = self.clay.get_mut(&o.id) else { continue };
            let clay = o.clay.as_ref();
            let mask = clay.and_then(|c| c.sculpt_mask.as_ref()).filter(|_| overlay != crate::overlay::Overlay::None);
            let groups = clay.and_then(|c| c.groups.as_ref()).filter(|g| overlay == crate::overlay::Overlay::Groups || !g.hidden().is_empty());
            if mask.is_none() && groups.is_none() {
                e.tinted = None;
                continue;
            }
            let key = (Arc::as_ptr(e.drawn()) as usize, mask.map(|m| m.addr()), groups.map(|g| g.addr()), overlay);
            if e.tinted.as_ref().is_some_and(|t| t.0 == key) {
                continue;
            }
            #[cfg(test)]
            let t = std::time::Instant::now();
            let mesh = crate::overlay::tint(e.drawn(), e.unposed(), mask, groups, overlay == crate::overlay::Overlay::Groups);
            #[cfg(test)]
            {
                self.overlay_ms = t.elapsed().as_secs_f64() * 1000.0;
            }
            e.tinted = Some((key, Arc::new(mesh)));
        }
    }

    /// The mesh the viewport draws and picks for an object: as `get`, with
    /// a clay's mask and group overlay (hidden groups left out).
    pub fn display(&self, o: &Object) -> Option<&Arc<Mesh>> {
        match Self::key(o) {
            MeshKey::Own(id) => self.clay.get(&id).map(ClayEntry::display).or_else(|| self.get(o)),
            _ => self.get(o),
        }
    }

    fn sharing(&self) -> bool {
        #[cfg(test)]
        return !self.no_sharing;
        #[cfg(not(test))]
        true
    }

    /// Gives object `id` the meshes of `owner`, built from the same key.
    /// The entry is its own copy (sharing the meshes), so baking, posing or
    /// editing either object later touches only that object.
    fn share(&mut self, id: u64, owner: u64) {
        let mut e = self.clay[&owner].clone();
        e.shown = e.rest.clone();
        e.posed_for = None;
        (e.baked, e.use_baked) = (None, false);
        self.clay.insert(id, e);
        self.pending.clay.remove(&id);
        self.pending.alias.remove(&id);
        self.pending.hashes.remove(&id);
        #[cfg(test)]
        {self.aliases_installed += 1;}
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
                (Shape::Mesh, _, _) if o.mesh.is_some() => EnvKey::Imported(o.mesh.clone().expect("checked")),
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
                EnvKey::Imported(a) => Mesh::imported(a),
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
        // Hashed when the build started (None when sharing is off).
        let hash = self.pending.hashes.remove(&id);
        match self.clay.get_mut(&id) {
            Some(e) => {
                (e.key, e.hash, e.whole, e.rest, e.parts, e.skin) = (key, hash, b.whole, b.rest, b.parts, b.skin);
                e.fine=b.fine; e.fine_error=None;
                e.shown = e.rest.clone();
                e.posed_for = None;
            }
            None => {
                let entry = ClayEntry { key, hash, shown: b.rest.clone(), whole: b.whole, rest: b.rest, skin: b.skin, posed_for: None, parts: b.parts, baked: None, use_baked: false, fine: b.fine, use_fine: false, fine_error: None, tinted: None };
                self.clay.insert(id, entry);
            }
        }
    }

    /// Builds (when shown and stale) and poses fine meshes. They are skinned
    /// from the rest mesh's nearest vertex, so they move like it. Every
    /// stale build starts before any is joined, so objects build at once
    /// (at most `FINE_AT_ONCE` of them: each holds a large grid).
    fn sync_fine(&mut self, scene: &Scene, wait: bool) {
        let show = self.show_fine;
        let stale = |e: &ClayEntry| {
            let fresh = e.fine.as_ref().is_some_and(|f| Arc::ptr_eq(&f.from, &e.rest)) || e.fine_error.as_ref().is_some_and(|f| Arc::ptr_eq(&f.0, &e.rest));
            show && !e.use_baked && e.key.clay.detail_cell.is_some() && !fresh
        };
        // Builds for rest meshes no object shows any more finish unseen.
        let clay = &self.clay;
        self.pending.fine.retain(|slot, _| clay.values().any(|e| e.fine_slot() == *slot));
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Clay) {
            let Some(e) = self.clay.get(&o.id).filter(|e| stale(e)) else { continue };
            // Objects sharing a rest mesh and skin (see `share`) share its
            // fine mesh: one build, taken from whoever has it or is making it.
            let (rest, slot) = (e.rest.clone(), e.fine_slot());
            if let Some((fine, error)) = self.clay.values().find(|x| x.fine_slot() == slot && (x.fine.as_ref().is_some_and(|f| Arc::ptr_eq(&f.from, &rest)) || x.fine_error.as_ref().is_some_and(|f| Arc::ptr_eq(&f.0, &rest)))).map(|x| (x.fine.clone(), x.fine_error.clone())) {
                let e = self.clay.get_mut(&o.id).expect("checked");
                (e.fine, e.fine_error) = (fine, error);
                continue;
            }
            if self.pending.fine.contains_key(&slot) {
                continue;
            }
            #[cfg(test)]
            {self.fine_builds += 1;}
            let (key, rest, skin) = (e.key.clone(), e.rest.clone(), e.skin.as_ref().map(|s| s.2.clone()));
            let from = (rest.clone(), skin.clone());
            let ticket = FineTicket::default();
            let done = ticket.clone();
            let handle = std::thread::Builder::new()
                .name(format!("fine mesh {}", o.name))
                .spawn(move || {
                    let done = Done(done);
                    let built = {
                        let _slot = FineSlot::take();
                        key.clay.build_fine(&key.library, &key.fallback)
                    };
                    let baked = built.map(|mesh| {
                        let skin = skin.map(|s| {
                            let near = rig::NearestVertex::new(&rest, key.clay.cell_size().unwrap_or(0.01) * 2.0);
                            let pick: Vec<usize> = mesh.vertices.iter().map(|v| near.index(Vec3::from(v.pos)).unwrap_or(0)).collect();
                            Arc::new(Skin { joints: pick.iter().map(|&i| s.joints[i]).collect(), weights: pick.iter().map(|&i| s.weights[i]).collect() })
                        });
                        let mesh = Arc::new(mesh);
                        Baked { settings: bake::Settings::default(), from: rest, shown: mesh.clone(), mesh, skin, posed_for: None }
                    });
                    done.0.finish(baked.as_ref().map(|b| (b.mesh.clone(), b.skin.clone())).map_err(Clone::clone));
                    baked
                })
                .expect("fine mesh thread starts");
            self.pending.fine.insert(slot, ((from, o.name.clone(), std::time::Instant::now(), handle), ticket));
        }
        for o in scene.objects.iter().filter(|o| o.shape == Shape::Clay) {
            if let Some(slot) = self.clay.get(&o.id).filter(|e| stale(e)).map(|e| e.fine_slot())
                && let Some(p) = self.pending.fine.get(&slot)
                && (wait || p.0.3.is_finished())
            {
                let (((from, _), _, _, handle), _) = self.pending.fine.remove(&slot).expect("pending");
                let built = handle.join().expect("fine mesh build");
                // Every object showing this rest mesh and skin gets it.
                for e in self.clay.values_mut().filter(|e| e.fine_slot() == slot) {
                    match &built {
                        Ok(f) => (e.fine, e.fine_error) = (Some(f.clone()), None),
                        Err(why) => (e.fine, e.fine_error) = (None, Some((from.clone(), why.clone()))),
                    }
                }
            }
            let Some(e) = self.clay.get_mut(&o.id) else { continue };
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

    /// The fine mesh still being built for an object shown fine (by
    /// `sync_background` or `sync_shown`), to wait for off this thread:
    /// the ticket, seconds so far, and the skin matrices of the pose shown.
    pub fn fine_building(&self, o: &Object) -> Option<(FineTicket, f32, Option<Vec<Mat4>>)> {
        let e = self.clay.get(&o.id).filter(|e| self.show_fine && !e.use_baked && !e.use_fine)?;
        // Objects sharing a rest mesh wait on the same build.
        let ((_, _, started, _), ticket) = self.pending.fine.get(&e.fine_slot())?;
        let rig = o.rig.as_ref().filter(|r| !r.bones.is_empty());
        let frame = rig.and_then(|r| r.is_posed(self.frame).then_some(self.frame).flatten());
        let pose = rig.filter(|_| frame.is_some()).map(|r| r.skin_matrices(frame));
        Some((ticket.clone(), started.elapsed().as_secs_f32(), pose))
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
        scene.get(id).ok_or("object vanished")?;
        let e = self.clay.get(&id).ok_or("not a clay object")?;
        if e.baked.as_ref().is_some_and(|b| b.settings == settings && Arc::ptr_eq(&b.from, &e.rest)) {
            return Ok(None);
        }
        self.bake_inputs(scene, id, settings).map(Some)
    }

    /// What a bake of `id` reads, owned, whether or not a fresh bake exists
    /// (displacement maps of a fresh bake read the same inputs).
    pub fn bake_inputs(&self, scene: &Scene, id: u64, settings: bake::Settings) -> Result<BakeJob, String> {
        let o = scene.get(id).ok_or("object vanished")?;
        let e = self.clay.get(&id).ok_or("not a clay object")?;
        Ok(BakeJob {
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
        })
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

    /// Which mesh an object draws with, ignoring sharing (see `key_of`).
    pub fn key(o: &Object) -> MeshKey {
        if o.shape.own_mesh() || crate::damage::own_primitive(o) { MeshKey::Own(o.id) } else { MeshKey::Prim(o.shape) }
    }

    /// Which GPU mesh an object draws with: clay showing the same mesh as
    /// another object draws with that object's key, so the viewport
    /// uploads it once and draws every copy in one instanced call.
    pub fn key_of(&self, o: &Object) -> MeshKey {
        match Self::key(o) {
            MeshKey::Own(id) => MeshKey::Own(self.owner_of.get(&id).copied().unwrap_or(id)),
            key => key,
        }
    }

    /// The mesh as drawn (posed if the object's rig is playing a clip).
    pub fn get(&self, o: &Object) -> Option<&Arc<Mesh>> {
        match Self::key(o) {
            MeshKey::Prim(s) => self.prims.get(&s),
            MeshKey::Own(id) => self.solids.get(&id).map(|e|&e.1).or_else(||self.clay.get(&id).map(ClayEntry::drawn)).or_else(|| self.eyes.get(&id).map(|e| &e.1)).or_else(|| self.hairs.get(&id).map(|e| &e.1)).or_else(|| self.kits.get(&id).map(|e| &e.1)).or_else(|| self.fires.get(&id).map(|e| &e.3)),
            // `key` never names a chunk (chunks are drawn, never read).
            MeshKey::Chunk(..) => None,
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
        // Objects drawing another's mesh draw with its key (see `key_of`).
        let clay = self.clay.iter().filter(|(id, _)| !self.owner_of.contains_key(id)).map(|(id, e)| (MeshKey::Own(*id), e.display().clone()));
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

    /// Displacement maps of `mesh` (this job's bake) against the clay.
    pub fn displacement(&self, mesh: &Mesh, maps: bake::Maps, progress: &crate::progress::Progress) -> Result<bake::Displacement, String> {
        if self.cloth {
            return Err("cloth bakes have no displacement maps".into());
        }
        let src = bake::Source { clay: &self.clay, rest: &self.rest, skin: self.skin.as_deref(), parts: &self.parts, library: &self.library, fallback: &self.fallback, fine: self.fine.as_deref() };
        bake::displacement(&src, mesh, maps, progress)
    }

    /// The clay's cell (the displacement check's unit).
    pub fn cell(&self) -> Option<f32> {
        self.clay.cell_size()
    }

    /// How far each point is from the clay's surface (|field|).
    pub fn distances(&self, points: &[glam::Vec3]) -> Vec<f32> {
        let Some(cell) = self.clay.cell_size() else { return vec![f32::INFINITY; points.len()] };
        let field = crate::clay::Field::new(&self.clay, cell);
        let all: Vec<u32> = (0..field.prim_count() as u32).collect();
        let mut out: Vec<f32> = vec![0.0; points.len()];
        crate::clay::par_chunks(&mut out, 4096, |chunk, out| {
            for (k, d) in out.iter_mut().enumerate() {
                let p = points[chunk * 4096 + k];
                let reach = glam::Vec3::splat(cell * 4.0);
                let near = field.near(p - reach, p + reach);
                *d = field.sample(p, if near.is_empty() { &all } else { &near }).0.abs();
            }
        });
        out
    }
}

impl ClayEntry {
    /// What a fine mesh is made from, by address: the rest mesh and skin.
    /// Objects with the same slot can share one.
    fn fine_slot(&self) -> (usize, usize) {
        (Arc::as_ptr(&self.rest) as usize, self.skin.as_ref().map_or(0, |s| Arc::as_ptr(&s.2) as usize))
    }

    fn drawn(&self) -> &Arc<Mesh> {
        match (&self.baked, &self.fine) {
            (Some(b), _) if self.use_baked => &b.shown,
            (_, Some(f)) if self.use_fine => &f.shown,
            _ => &self.shown,
        }
    }

    /// `drawn` unposed (the same vertices in the clay's space).
    fn unposed(&self) -> &Arc<Mesh> {
        match (&self.baked, &self.fine) {
            (Some(b), _) if self.use_baked => &b.mesh,
            (_, Some(f)) if self.use_fine => &f.mesh,
            _ => &self.rest,
        }
    }

    /// What the viewport draws: `drawn`, with the overlay if it has one.
    fn display(&self) -> &Arc<Mesh> {
        self.tinted.as_ref().map_or_else(|| self.drawn(), |t| &t.1)
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

    /// An imported mesh as drawn: welded where bit-equal, normals
    /// smooth over shared corners, the object's material (as primitives).
    pub fn imported(asset: &crate::mesh_asset::MeshAsset) -> Mesh {
        let mut m = Mesh::default();
        let (points, tris) = (asset.points(), asset.tris());
        let mut normals = vec![Vec3::ZERO; points.len()];
        for t in tris {
            let [a, b, c] = t.map(|i| points[i as usize]);
            let n = (b - a).cross(c - a);
            for &i in t {
                normals[i as usize] += n;
            }
        }
        for (p, n) in points.iter().zip(&normals) {
            m.vert(*p, *n);
        }
        m.indices = tris.iter().flatten().copied().collect();
        m
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
            Shape::Group | Shape::Clay | Shape::Eye | Shape::Hair | Shape::Cloth | Shape::Fire | Shape::Kit | Shape::Terrain | Shape::Scatter | Shape::Part | Shape::Mesh => Mesh::default(),
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
    #[cfg(test)]
    pub fn ray_hit(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        self.ray_hit_triangle(origin, dir).map(|(t, _)| t)
    }

    /// `ray_hit`, plus which triangle was hit (brute force: see `pick.rs`).
    #[cfg(test)]
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

    /// Fine builds start without blocking the caller, all at once; a film
    /// render's ticket gets the very mesh the library installs later.
    #[test]
    fn fine_meshes_build_behind_tickets() {
        let mut scene = Scene::default();
        let ids: Vec<u64> = (0..3).map(|i| {
            let id = scene.add(Shape::Clay);
            scene.get_mut(id).unwrap().clay = Some(serde_json::from_value(serde_json::json!({"blobs":[{"type":"array","source":{"type":"box","center":[0,0,0],"size":[0.05,0.3,0.05]},"count":24,"step":[0.08,0,0.01 * i as f32],"blend":0}],"detail_cell":0.004})).unwrap());
            id
        }).collect();
        let mut lib = MeshLib::new();
        lib.show_fine = true;
        lib.sync_shown(&scene);
        assert!(lib.busy());
        let tickets: Vec<FineTicket> = ids.iter().map(|&id| lib.fine_building(scene.get(id).unwrap()).expect("building").0).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let meshes: Vec<Arc<Mesh>> = tickets.iter().map(|t| t.wait_until(deadline).expect("built").expect("fine mesh").0).collect();
        lib.sync(&scene);
        for (&id, mesh) in ids.iter().zip(&meshes) {
            let o = scene.get(id).unwrap();
            assert!(lib.fine_building(o).is_none());
            assert!(Arc::ptr_eq(lib.get(o).unwrap(), mesh), "the ticket's mesh is the one shown");
            assert!(mesh.indices.len() > lib.rest(id).unwrap().0.indices.len() / 4);
        }
    }

    /// An undo back to the shown meshes while a build runs drops that
    /// build: the library is not left busy for good.
    #[test]
    fn going_back_to_the_shown_meshes_drops_the_running_build() {
        let mut scene = Scene::default();
        let id = scene.add(Shape::Clay);
        scene.get_mut(id).unwrap().clay = Some(serde_json::from_value(serde_json::json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":0.5,"blend":0}],"resolution":32})).unwrap());
        let mut lib = MeshLib::new();
        lib.sync(&scene);
        let shown = scene.clone();
        scene.get_mut(id).unwrap().clay.as_mut().unwrap().resolution = 40;
        lib.sync_background(&scene);
        assert!(lib.busy(), "the edit builds in the background");
        lib.sync_background(&shown);
        assert!(!lib.busy(), "back to what is shown: nothing to wait for");
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
