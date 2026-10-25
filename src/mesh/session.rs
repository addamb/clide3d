//! Sculpt sessions (docs/SCULPT_PLAN.md Stage 6): a clay under sculpt (a
//! stroke in the last `SESSION`, or a sculpt tool active on it) is drawn
//! in chunks (`clay::chunks`) that rebuild only where a stroke changed
//! the grid, on a thread of their own, newest state first. Paint, the
//! sculpt mask and polygroups only change chunk attributes (the slim
//! vertex's color, mask and group), never the field.
//!
//! What the viewport draws is the only thing that changes: every other
//! reader (picking fallbacks, snapping, film, bakes, exports) keeps the
//! clay's regular meshes, so no result depends on whether a session runs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use glam::{IVec3, Vec3};

use super::{BuildKey, MeshLib};
use crate::clay::chunks::{self, ChunkKey, ChunkMesh, ChunkSet, Mesher};
use crate::clay::{Clay, Detail};
use crate::groups::Groups;
use crate::mask::SculptMask;
use crate::material::Material;
use crate::paint::Paint;
use crate::scene::Object;

/// How long after its last stroke a clay stays in its sculpt session.
pub const SESSION: Duration = Duration::from_secs(60);

/// How long strokes must pause before the regular mesh of a clay a sculpt
/// tool is on builds in the background.
pub const PAUSE: Duration = Duration::from_millis(300);

/// The slim sculpt vertex (24 B): position, octahedral normal, sRGB
/// color, then mask (0..255), group id (low, high byte) and cavity.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SculptVertex {
    pub pos: [f32; 3],
    pub normal: [i16; 2],
    pub color: [u8; 4],
    pub attrs: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<SculptVertex>() == 24);

/// One chunk ready to draw: slim vertices and triangles (hidden groups'
/// left out), its box, and a number unique to this copy.
#[derive(Debug)]
pub struct SculptChunk {
    pub vertices: Vec<SculptVertex>,
    pub indices: Vec<u32>,
    pub bounds: (Vec3, Vec3),
    pub generation: u64,
}

/// A unit normal as two snorm16 (octahedral).
pub fn oct_encode(n: Vec3) -> [i16; 2] {
    let n = n / (n.x.abs() + n.y.abs() + n.z.abs()).max(1e-12);
    let (x, y) = if n.z >= 0.0 { (n.x, n.y) } else { ((1.0 - n.y.abs()) * n.x.signum(), (1.0 - n.x.abs()) * n.y.signum()) };
    [(x.clamp(-1.0, 1.0) * 32767.0).round() as i16, (y.clamp(-1.0, 1.0) * 32767.0).round() as i16]
}

/// The normal `oct_encode` packed (as the shader unpacks it).
#[cfg(test)]
pub fn oct_decode(e: [i16; 2]) -> Vec3 {
    let (x, y) = ((e[0] as f32 / 32767.0).max(-1.0), (e[1] as f32 / 32767.0).max(-1.0));
    let mut n = Vec3::new(x, y, 1.0 - x.abs() - y.abs());
    let t = (-n.z).max(0.0);
    n.x += if n.x >= 0.0 { -t } else { t };
    n.y += if n.y >= 0.0 { -t } else { t };
    n.normalize_or(Vec3::Z)
}

/// What a chunk's attributes come from.
#[derive(Clone, Default, PartialEq)]
struct Attrs {
    paint: Option<Paint>,
    mask: Option<SculptMask>,
    groups: Option<Groups>,
    hidden: Vec<u16>,
}

/// A clay drawn in chunks.
#[derive(Clone)]
pub(super) struct Chunked {
    /// The composed clay (no mask or groups, as `clay_key`) and palette
    /// the chunk meshes were built from.
    clay: Clay,
    palette: Vec<Material>,
    mesher: Arc<Mesher>,
    set: ChunkSet,
    /// Draw-ready chunks, the mesh each was made from, and the attributes.
    slim: BTreeMap<ChunkKey, (Arc<ChunkMesh>, Arc<SculptChunk>)>,
    attrs: Attrs,
}

type Built = Result<(Arc<Mesher>, ChunkSet, Clay, Vec<Material>, usize, f64), String>;

/// Sessions: who is in one, and chunk builds running.
#[derive(Default)]
pub(super) struct Sessions {
    /// Last stroke (any sculpt, paint, mask or group edit) by clay.
    strokes: HashMap<u64, Instant>,
    /// The grids each clay had last sync (a change is a stroke).
    grids: HashMap<u64, [usize; 5]>,
    /// Chunk builds running: (the clay and palette, the thread).
    pending: HashMap<u64, (Clay, Vec<Material>, std::thread::JoinHandle<Built>)>,
    /// The clay and palette whose chunk build failed (too many cells or
    /// chunks at its cell, ...), by clay: no build starts again until
    /// they change.
    failed: HashMap<u64, (Clay, Vec<Material>)>,
}

/// A copy of the library (an export's snapshot) is in no session: its
/// syncs never start chunk builds nobody draws.
impl Clone for Sessions {
    fn clone(&self) -> Self {
        Sessions::default()
    }
}

/// The clays a sculpt tool is active on (`MeshLib::set_sculpt_tool`); a
/// copy of the library has none.
#[derive(Default, Debug)]
pub(super) struct ToolOn(pub Vec<u64>);

impl Clone for ToolOn {
    fn clone(&self) -> Self {
        ToolOn::default()
    }
}

/// Clays the viewport draws as a stroke being drawn leaves them (the
/// editor's live preview, `app::brush`), each with the number of its
/// preview, and the newest preview each clay's chunks draw. Only chunks
/// show a preview: every other reader keeps the scene's clay. A copy of
/// the library has none.
#[derive(Default, Debug)]
pub(super) struct Previews {
    pub shown: HashMap<u64, (u64, Clay)>,
    pub drawn: HashMap<u64, u64>,
}

impl Clone for Previews {
    fn clone(&self) -> Self {
        Previews::default()
    }
}

/// Identities of a clay's grids.
fn grid_ids(c: &Clay) -> [usize; 5] {
    [
        c.detail.as_ref().map_or(0, Detail::addr),
        c.paint.as_ref().map_or(0, Paint::addr),
        c.sculpt_mask.as_ref().map_or(0, SculptMask::addr),
        c.groups.as_ref().map_or(0, Groups::addr),
        c.sculpt.as_ref().map_or(0, |s| s.data() as *const _ as usize),
    ]
}

/// Whether two clays agree on all but their detail and paint: what else
/// shapes their chunks (field by field, nothing copied: every frame asks).
fn same_shape(a: &Clay, b: &Clay) -> bool {
    let Clay { blobs, resolution, detail: _, paint: _, masks, decals, detail_cell, shell, sharp, sculpt, sculpt_mask, groups, assets } = a;
    *blobs == b.blobs && *resolution == b.resolution && *masks == b.masks && *decals == b.decals && *detail_cell == b.detail_cell && *shell == b.shell && *sharp == b.sharp && *sculpt == b.sculpt && *sculpt_mask == b.sculpt_mask && *groups == b.groups && *assets == b.assets
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Chunk mesh `m` with its attributes, ready to draw.
fn slim(m: &ChunkMesh, palette: &[Material], a: &Attrs) -> SculptChunk {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut labels = Vec::with_capacity(m.positions.len());
    let vertices: Vec<SculptVertex> = (0..m.positions.len())
        .map(|i| {
            let p = m.positions[i];
            let mat = palette.get(m.mats[i] as usize);
            let c = mat.map_or(m.base[i], |mat| mat.painted_color(m.base[i], a.paint.as_ref(), p));
            let mask = a.mask.as_ref().map_or(0.0, |s| s.grid().value(p));
            let group = a.groups.as_ref().map_or(0, |g| g.label(p));
            labels.push(group);
            SculptVertex { pos: p.to_array(), normal: oct_encode(m.normals[i]), color: [q(c.x), q(c.y), q(c.z), 255], attrs: [q(mask), (group & 0xff) as u8, (group >> 8) as u8, m.cavity.get(i).copied().unwrap_or(0)] }
        })
        .collect();
    let indices = if a.hidden.is_empty() {
        m.indices.clone()
    } else {
        // A triangle goes when most of its corners are hidden (as the
        // regular overlay's).
        let off = |i: u32| labels.get(i as usize).is_some_and(|id| *id != 0 && a.hidden.contains(id));
        m.indices.chunks_exact(3).filter(|t| t.iter().filter(|i| off(**i)).count() < 2).flatten().copied().collect()
    };
    let (lo, hi) = m.positions.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    SculptChunk { vertices, indices, bounds: (lo, hi), generation: GENERATION.fetch_add(1, Ordering::Relaxed) }
}

/// Lattice boxes of a grid's bricks of cell `cell` (keys `k`: points
/// 8k..8k+7) on a lattice of `voxel`.
fn lattice_box(k: [i32; 3], cell: f32, voxel: f32) -> (IVec3, IVec3) {
    let lo = Vec3::from(k.map(|x| x as f32)) * 8.0 * cell;
    (((lo - cell) / voxel).floor().as_ivec3(), ((lo + Vec3::splat(9.0 * cell)) / voxel).ceil().as_ivec3())
}

/// Bricks that differ between two grids, by key (`brick` reads one).
fn brick_diff<B>(a: &[([i32; 3], *const B)], b: &[([i32; 3], *const B)]) -> Vec<[i32; 3]> {
    let ma: HashMap<[i32; 3], *const B> = a.iter().copied().collect();
    let mb: HashMap<[i32; 3], *const B> = b.iter().copied().collect();
    let mut out: Vec<[i32; 3]> = a.iter().filter(|(k, p)| mb.get(k) != Some(p)).map(|(k, _)| *k).collect();
    out.extend(b.iter().filter(|(k, _)| !ma.contains_key(k)).map(|(k, _)| *k));
    out
}

impl Attrs {
    /// The chunks whose attributes differ from `self` to `new` (None: all).
    fn dirty(&self, new: &Attrs, voxel: f32) -> Option<Vec<ChunkKey>> {
        if self.hidden != new.hidden {
            return None;
        }
        let mut boxes: Vec<(IVec3, IVec3)> = Vec::new();
        if self.paint != new.paint {
            match (&self.paint, &new.paint) {
                (Some(a), Some(b)) if a.grid().voxel == b.grid().voxel => boxes.extend(a.grid().changed_bricks(b.grid()).into_iter().map(|k| lattice_box(k, a.grid().voxel, voxel))),
                _ => return None,
            }
        }
        if self.mask != new.mask {
            match (&self.mask, &new.mask) {
                (Some(a), Some(b)) if a.grid().voxel == b.grid().voxel => {
                    let ids = |m: &SculptMask| m.grid().bricks().map(|(k, b)| (*k, Arc::as_ptr(b))).collect::<Vec<_>>();
                    boxes.extend(brick_diff(&ids(a), &ids(b)).into_iter().map(|k| lattice_box(k, a.grid().voxel, voxel)));
                }
                (Some(m), None) | (None, Some(m)) => boxes.extend(m.grid().bricks().map(|(k, _)| lattice_box(*k, m.grid().voxel, voxel))),
                _ => return None,
            }
        }
        if self.groups != new.groups {
            match (&self.groups, &new.groups) {
                (Some(a), Some(b)) if a.data().voxel == b.data().voxel => {
                    let ids = |g: &Groups| g.data().bricks().map(|(k, b)| (*k, Arc::as_ptr(b))).collect::<Vec<_>>();
                    boxes.extend(brick_diff(&ids(a), &ids(b)).into_iter().map(|k| lattice_box(k, a.data().voxel, voxel)));
                }
                (Some(g), None) | (None, Some(g)) => boxes.extend(g.data().bricks().map(|(k, _)| lattice_box(*k, g.data().voxel, voxel))),
                _ => return None,
            }
        }
        Some(chunks::chunks_of(&boxes, 1))
    }
}

impl Chunked {
    /// Makes the draw-ready chunks match the chunk meshes and `attrs`:
    /// only chunks whose mesh or attributes changed are made again.
    fn refresh(&mut self, attrs: Attrs) -> usize {
        let dirty = self.attrs.dirty(&attrs, self.mesher.voxel);
        let all = dirty.is_none();
        let marked: HashSet<ChunkKey> = dirty.unwrap_or_default().into_iter().collect();
        self.slim.retain(|k, _| self.set.chunks.contains_key(k));
        let mut work: Vec<(ChunkKey, Arc<ChunkMesh>, Option<Arc<SculptChunk>>)> = self
            .set
            .chunks
            .iter()
            .filter(|(k, m)| all || marked.contains(*k) || !self.slim.get(*k).is_some_and(|(from, _)| Arc::ptr_eq(from, m)))
            .map(|(k, m)| (*k, m.clone(), None))
            .collect();
        let (palette, a) = (&self.palette, &attrs);
        crate::clay::par_chunks(&mut work, 4, |_, part| {
            for (_, m, out) in part {
                *out = Some(Arc::new(slim(m, palette, a)));
            }
        });
        let n = work.len();
        for (k, m, s) in work {
            self.slim.insert(k, (m, s.expect("made")));
        }
        self.attrs = attrs;
        n
    }
}

impl MeshLib {
    /// Marks clay `id` as under sculpt by a tool (each frame the tool is
    /// active on it; a stroke also starts a session).
    pub fn set_sculpt_tool(&mut self, ids: &[u64]) {
        self.sculpt_tool = ToolOn(ids.to_vec());
    }

    /// Draws clay `id` as `clay` (a stroke's live preview, numbered `seq`)
    /// instead of the scene's, in chunks, until it is set to None. Nothing
    /// else reads it: picking fallbacks, snapping, casting, film and
    /// exports keep the scene's clay.
    pub fn set_preview(&mut self, id: u64, preview: Option<(u64, Clay)>) {
        match preview {
            Some(p) => {
                self.previews.shown.insert(id, p);
            }
            None => {
                self.previews.shown.remove(&id);
                self.previews.drawn.remove(&id);
            }
        }
    }

    /// The preview set for clay `id` (its number).
    pub fn preview_shown(&self, id: u64) -> Option<u64> {
        self.previews.shown.get(&id).map(|p| p.0)
    }

    /// The newest preview of clay `id` its chunks are ready to draw.
    pub fn preview_drawn(&self, id: u64) -> Option<u64> {
        self.previews.drawn.get(&id).copied()
    }

    /// The last chunk build's and attribute refresh's times (ms): what a
    /// stroke's remesh took before it could be drawn.
    pub fn chunk_timing(&self) -> (f64, f64) {
        self.chunk_timing
    }

    /// Whether clay `id` is being sculpted in the window right now: a
    /// sculpt tool is on it, and its chunks are building or its last
    /// stroke was under `PAUSE` ago. (Agent strokes alone never hold back
    /// the regular mesh: the agent's next call waits for it.)
    pub(super) fn sculpting_now(&self, id: u64) -> bool {
        let now = self.session_now.unwrap_or_else(Instant::now);
        self.sculpt_tool.0.contains(&id) && (self.chunks_building(id) || (self.chunked.contains_key(&id) && self.sessions.strokes.get(&id).is_some_and(|t| now.saturating_duration_since(*t) < PAUSE)))
    }

    /// Whether any chunk build runs (the window draws again soon to show it).
    pub fn chunks_pending(&self) -> bool {
        !self.sessions.pending.is_empty()
    }

    /// Whether clay `id`'s chunks are being built.
    pub fn chunks_building(&self, id: u64) -> bool {
        self.sessions.pending.contains_key(&id)
    }

    /// Whether clay `id` is drawn in chunks now.
    pub fn chunked(&self, id: u64) -> bool {
        self.chunked.get(&id).is_some_and(|c| !c.slim.is_empty())
    }

    /// The chunk keys clay object `o` is drawn with, when it is drawn in
    /// chunks (else its `key_of`).
    pub fn chunk_keys(&self, o: &Object) -> Option<Vec<super::MeshKey>> {
        let c = self.chunked.get(&o.id).filter(|c| !c.slim.is_empty())?;
        Some(c.slim.iter().filter(|(_, (_, s))| !s.indices.is_empty()).map(|(k, _)| super::MeshKey::Chunk(o.id, *k)).collect())
    }

    /// Every draw-ready chunk, by key.
    pub fn sculpt_chunks(&self) -> Vec<(super::MeshKey, Arc<SculptChunk>)> {
        self.chunked.iter().flat_map(|(id, c)| c.slim.iter().map(move |(k, (_, s))| (super::MeshKey::Chunk(*id, *k), s.clone()))).collect()
    }

    /// Clay `id`'s chunk meshes as one welded mesh (the monolithic mesh,
    /// assembled on demand), with its paint.
    #[cfg(test)]
    pub fn assembled(&self, id: u64) -> Option<super::Mesh> {
        let c = self.chunked.get(&id)?;
        Some(c.set.assemble(&c.palette, c.clay.paint.as_ref()))
    }

    /// `pick_field` on clay `id` while it is in a sculpt session: where a
    /// ray in the clay's space first meets the surface its chunks show
    /// (`t` in units of `dir`), and the normal there. None outside a
    /// session (pick its mesh: `ray_hit`).
    pub fn pick_field(&self, id: u64, origin: Vec3, dir: Vec3) -> Option<Option<(f32, Vec3)>> {
        let c = self.chunked.get(&id)?;
        Some(c.mesher.pick(origin, dir))
    }

    /// The nearest hit of a ray on a mesh (as `Mesh::ray_hit_triangle`,
    /// through a tree kept with the mesh), with barycentrics and normal.
    pub fn ray_hit(&self, mesh: &Arc<super::Mesh>, origin: Vec3, dir: Vec3) -> Option<super::RayHit> {
        self.bvhs.of(mesh).hit(mesh, origin, dir)
    }

    /// The picking trees' cache, to build a tree on another thread (the
    /// stroke preview's), kept for whoever asks next.
    pub fn bvh_cache(&self) -> super::pick::BvhCache {
        self.bvhs.clone()
    }

    /// Builds the picking tree of clay `id`'s mesh in the background, so a
    /// stroke's screen samples cast at once (brush tools call it on the
    /// clays they are on).
    pub fn warm_bvh(&self, id: u64) {
        if let Some(mesh) = self.whole(id) {
            self.bvhs.warm(mesh);
        }
    }

    /// `mesh`'s picking tree.
    pub fn bvh(&self, mesh: &Arc<super::Mesh>) -> Arc<super::MeshBvh> {
        self.bvhs.of(mesh)
    }

    /// The blob-field cache of clay `id`'s session (warm while it runs).
    pub fn session_cache(&self, id: u64) -> Option<Arc<crate::clay::base_cache::BaseCache>> {
        self.chunked.get(&id).map(|c| c.mesher.cache().clone())
    }

    /// Brings sculpt sessions and their chunks up to date; with `wait`,
    /// joins chunk builds (checks that read them right after; tools never
    /// wait for chunks, which only the viewport draws).
    /// (`clays`: the scene's clay objects; `keys`: their build keys.)
    pub(super) fn sync_chunks(&mut self, clays: &[&Object], keys: &[BuildKey], wait: bool) {
        let now = self.session_now.unwrap_or_else(Instant::now);
        let live: HashSet<u64> = clays.iter().map(|o| o.id).collect();
        self.chunked.retain(|id, _| live.contains(id));
        self.sessions.pending.retain(|id, _| live.contains(id));
        self.sessions.strokes.retain(|id, _| live.contains(id));
        self.sessions.grids.retain(|id, _| live.contains(id));
        self.sessions.failed.retain(|id, _| live.contains(id));
        for (o, key) in clays.iter().zip(keys) {
            let Some(c) = o.clay.as_ref() else { continue };
            let ids = grid_ids(c);
            if self.sessions.grids.insert(o.id, ids).is_some_and(|old| old != ids) {
                self.sessions.strokes.insert(o.id, now);
            }
            let active = self.sculpt_tool.0.contains(&o.id) || self.sessions.strokes.get(&o.id).is_some_and(|t| now.saturating_duration_since(*t) < SESSION);
            // Chunks show the clay at rest; posed clay draws its posed mesh.
            let posed = o.rig.as_ref().is_some_and(|r| !r.bones.is_empty() && r.is_posed(self.frame));
            if !active || posed || self.show_baked || self.show_fine {
                self.chunked.remove(&o.id);
                self.sessions.pending.remove(&o.id);
                continue;
            }
            if chunks::eligible(&key.clay, &key.palette).is_err() {
                self.chunked.remove(&o.id);
                self.sessions.pending.remove(&o.id);
                continue;
            }
            // A session from strokes alone (no tool on the clay) builds its
            // first chunks once the clay's regular mesh is built, so they
            // never compete with the build an agent's next call waits for.
            let first_waits = !self.sculpt_tool.0.contains(&o.id) && (self.clay.get(&o.id).is_none_or(|e| e.key != *key) || self.pending.clay.contains_key(&o.id));
            self.update_chunks(o, key.clone(), c, wait, first_waits);
        }
    }

    /// Takes a finished chunk build of clay `id` (with `wait`, waits for
    /// a running one).
    fn take_chunk_build(&mut self, id: u64, wait: bool) {
        if !self.sessions.pending.get(&id).is_some_and(|p| wait || p.2.is_finished()) {
            return;
        }
        let (clay, palette, handle) = self.sessions.pending.remove(&id).expect("pending");
        match handle.join().expect("chunk build") {
            Ok((mesher, set, clay, palette, built, ms)) => {
                self.sessions.failed.remove(&id);
                self.chunk_timing.0 = ms;
                #[cfg(test)]
                {
                    self.chunk_builds += built;
                }
                let _ = built;
                match self.chunked.get_mut(&id) {
                    Some(e) => (e.clay, e.palette, e.mesher, e.set) = (clay, palette, mesher, set),
                    None => {
                        self.chunked.insert(id, Chunked { clay, palette, mesher, set, slim: BTreeMap::new(), attrs: Attrs::default() });
                    }
                }
            }
            Err(_) => {
                self.chunked.remove(&id);
                self.sessions.failed.insert(id, (clay, palette));
            }
        }
    }

    fn update_chunks(&mut self, o: &Object, key: BuildKey, c: &Clay, wait: bool, first_waits: bool) {
        let BuildKey { clay: target, palette, .. } = key;
        // A finished build goes in (when the scene moved on meanwhile, the
        // next build starts from it: the newest state wins).
        self.take_chunk_build(o.id, wait);
        let same = |c: &Clay, p: &[Material]| p == palette.as_slice() && same_shape(c, &target) && c.detail == target.detail;
        // A build that failed for this state is not tried again.
        if self.sessions.failed.get(&o.id).is_some_and(|(c, p)| same(c, p)) {
            return;
        }
        if !self.chunked.get(&o.id).is_some_and(|e| same(&e.clay, &e.palette)) && !self.sessions.pending.contains_key(&o.id) {
            // From the chunks shown when only the grid changed: the dirty
            // ones; else every chunk.
            let base = self.chunked.get(&o.id).filter(|e| e.palette == palette && same_shape(&e.clay, &target)).map(|e| (e.set.clone(), e.clay.detail.clone(), e.mesher.voxel));
            if base.is_none() && first_waits {
                // Chunks of another shape would show the old clay: the
                // regular mesh shows it meanwhile.
                self.chunked.remove(&o.id);
                return;
            }
            let (t, p) = (target.clone(), palette.clone());
            let handle = std::thread::Builder::new()
                .name(format!("chunks {}", o.name))
                .spawn(move || -> Built {
                    let t0 = Instant::now();
                    let mesher = Mesher::new(&t, &p)?;
                    let dirty = base.as_ref().filter(|b| b.2 == mesher.voxel).and_then(|(_, old, _)| chunks::dirty_detail(old.as_ref(), t.detail.as_ref(), mesher.voxel));
                    let (set, built) = match (base, dirty) {
                        (Some((set, _, _)), Some(dirty)) => (set.rebuilt(&mesher, &dirty), dirty.len()),
                        _ => {
                            let set = ChunkSet::build(&mesher);
                            let n = set.chunks.len();
                            (set, n)
                        }
                    };
                    Ok((Arc::new(mesher), set, t, p, built, t0.elapsed().as_secs_f64() * 1000.0))
                })
                .expect("chunk thread starts");
            self.sessions.pending.insert(o.id, (target.clone(), palette.clone(), handle));
            self.take_chunk_build(o.id, wait);
        }
        // A preview is drawn once the chunks show the clay it gave.
        if let Some((seq, _)) = self.previews.shown.get(&o.id)
            && self.chunked.get(&o.id).is_some_and(|e| same(&e.clay, &e.palette))
        {
            self.previews.drawn.insert(o.id, *seq);
        }
        let attrs = Attrs { paint: target.paint.clone(), mask: c.sculpt_mask.clone(), groups: c.groups.clone(), hidden: c.groups.as_ref().map(Groups::hidden).unwrap_or_default() };
        if let Some(e) = self.chunked.get_mut(&o.id) {
            // Paint changes colors only: the chunks keep their meshes.
            if e.clay.paint != target.paint && same_shape(&e.clay, &target) && e.clay.detail == target.detail {
                e.clay.paint = target.paint.clone();
            }
            let t0 = Instant::now();
            let _n = e.refresh(attrs);
            if _n > 0 {
                self.chunk_timing.1 = t0.elapsed().as_secs_f64() * 1000.0;
            }
            #[cfg(test)]
            {
                self.slim_updates += _n;
            }
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
