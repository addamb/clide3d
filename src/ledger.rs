//! The stroke ledger (docs/SCULPT_PLAN.md 3.4): every stroke on a clay as
//! an ordered, replayable record, kept beside the grid it built.
//!
//! A `Ledger` belongs to one grid: the base sculpt layer (`Clay::detail`)
//! or the paint layer (`Clay::paint`); `SculptStack` (`Clay::sculpt`)
//! holds both. A record stores the stroke as authored (`StrokeSpec`) and
//! as resolved (`Resolved`: object-space dabs, curve, mask), so a replay
//! applies exactly the same dabs with the same engine, with no snapping,
//! camera or UI state, and gives the same grid bit for bit. Records also
//! keep:
//! - `context_hash`: what the stroke read besides its grid (the blobs, the
//!   cell; paint also reads the sculpted surface), so a record whose
//!   context changed since is reported `stale_context`;
//! - `result_hash`: the grid after it (xxh3-128 over its bricks), checked
//!   by replays and on load;
//! - `stats` (`DiffStats`) and `engine`, the kernel version that made it.
//!   A record from an older kernel is `engine_stale`: its saved grid is
//!   kept on load and `stroke_edit rebase` re-applies it.
//!
//! Grids changed outside the ledger (`edit_clay` detail_cell or
//! clear_detail, paint fill, a clay sculpted before ledgers existed)
//! become `Opaque` records: the change as a delta when the grid layout
//! is the same, else the whole grid. So replaying a ledger always ends
//! at the grid the clay shows.
//!
//! Ledgers are Arcs compared by identity, like the grids, so the per-frame
//! scene compare and undo snapshots stay cheap. Replays restart from
//! checkpoints (every `CHECKPOINT_EVERY` records) kept in a `ReplayCache`
//! outside the scene, keyed by the record chain and the context. The
//! chain hashes what each record does (`Record::digest`), so a record
//! with a reused id (after an undo) or a copied ledger sculpted apart
//! never picks up another one's checkpoint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::{Mat4, Vec3};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::brush::alpha::AlphaStore;
use crate::brush::resolve::{FrozenRef, Resolved};
use crate::brush::spec::{Brush, StrokeSpec};
use crate::clay::{Clay, Detail, DetailGrid, Field, MAX_REGIONS, Region};
use crate::groups::Groups;
use crate::mask::SculptMask;
use crate::paint::Paint;
use crate::sculpt::engine::ENGINE_VERSION;

/// A replay restarts from a checkpoint at most this many records back.
pub const CHECKPOINT_EVERY: usize = 32;
/// Checkpoints a `ReplayCache` keeps per grid kind (oldest use dropped).
const CACHE_CHECKPOINTS: usize = 96;
/// Most records in one ledger: `commit` and `sync` refuse more, and a
/// file with more loses its ledgers on load (`Clay::sanitize`; the grids
/// are kept).
pub const MAX_RECORDS: usize = 100_000;
/// Most records in one ledger that hold a grid (Opaque records: outside
/// edits and squashes), refused like `MAX_RECORDS`. A file with more is
/// refused while it is read, before their grids are.
pub const MAX_OPAQUE_GRIDS: usize = 4_096;
/// Most stroke points over a clay's ledgers (dab points, all records),
/// enforced the same way.
pub const MAX_POINTS: usize = 2_000_000;
/// Ledgers with more dabs than this are not replayed to check them on
/// load (their saved grids are kept as they are; `stroke_edit replay`
/// checks them).
const VERIFY_DABS: usize = 2_000;
/// How long checking ledgers may hold up opening a file, over all its
/// clays; ledgers it does not reach stay unchecked.
pub const VERIFY_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// A 128-bit content hash (xxh3-128): stable across runs, Rust releases and
/// platforms, so it can be saved. Written as 32 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, PartialOrd, Ord)]
pub struct Hash128(pub u128);

impl std::fmt::Display for Hash128 {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl Serialize for Hash128 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Hash128 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        u128::from_str_radix(&text, 16).map(Hash128).map_err(|_| serde::de::Error::custom("a hash is 32 hex digits"))
    }
}

/// Streaming xxh3-128.
struct Hasher(Xxh3);

impl Hasher {
    fn new(tag: &str) -> Hasher {
        let mut h = Hasher(Xxh3::new());
        h.bytes(tag.as_bytes());
        h
    }

    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.update(b);
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    fn hash(&mut self, v: Hash128) -> &mut Self {
        self.bytes(&v.0.to_le_bytes())
    }

    fn json(&mut self, v: &impl Serialize) -> &mut Self {
        let text = serde_json::to_vec(v).unwrap_or_default();
        self.bytes(&text)
    }

    fn finish(&self) -> Hash128 {
        Hash128(self.0.digest128())
    }
}

/// Who made a record.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// An MCP tool call (`Record::tool` names it).
    Agent,
    /// The editor's own controls (they send the same tool calls).
    Ui,
    /// Made from other records: mirror copies, squashes.
    Replay,
    /// A grid from outside the ledger: an old file's detail, an outside edit.
    Import,
}

/// Where a UI stroke's pen pressure came from (kept on its records; the
/// pressure itself is in the stroke's samples).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PressureSource {
    /// A touch's force (tablets and touch screens that report one).
    TouchForce,
    /// The macOS tablet monitor (`app::pen_macos`).
    Nsevent,
    /// The pointer's speed (faster presses less), when asked for.
    Velocity,
}

/// What a record changed, measured on its grid.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct DiffStats {
    pub changed_points: u64,
    pub bricks_touched: u64,
    /// Object-space box around the changed grid points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[[f32; 3]; 2]>,
    /// Largest and mean change of a field offset (scene units; about how
    /// far the surface moved there).
    pub max_disp: f32,
    pub mean_disp: f32,
    /// Volume added (negative: removed), with `measure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_delta: Option<f32>,
}

/// A grid a ledger folds strokes into: the sculpt detail, the paint, the
/// persistent mask or the polygroups.
pub trait Grid: Clone + PartialEq + std::fmt::Debug + Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The layer name agents use (`stroke_log {layer}`).
    const LAYER: &'static str;
    /// What its non-stroke records do (mask and group actions).
    type Action: Clone + PartialEq + std::fmt::Debug + Serialize + DeserializeOwned + Send + Sync + 'static;
    /// Applies an action record to `grid`.
    fn act(_clay: &Clay, _grid: Option<&Self>, _action: &Self::Action) -> Result<Option<Self>, String> {
        Err(format!("the {} layer has no actions", Self::LAYER))
    }
    /// The action on the clay mirrored across x = 0.
    fn mirror_action(action: &Self::Action) -> Self::Action {
        action.clone()
    }
    /// The action with the frozen grids it names renamed.
    fn remap_action(action: &Self::Action, _remap: &dyn Fn(Hash128) -> Hash128) -> Self::Action {
        action.clone()
    }
    /// The frozen grids an action names.
    fn action_reads(_action: &Self::Action) -> Vec<Hash128> {
        Vec::new()
    }
    /// The content hash: `seal(grid, sum(grid))`.
    fn hash(grid: Option<&Self>) -> Hash128 {
        Self::seal(grid, Self::sum(grid))
    }
    /// The bricks' hashes added up (wrapping), so a change of a few bricks
    /// updates it without hashing the rest (`sum_change`).
    fn sum(grid: Option<&Self>) -> u128;
    /// `sum(after) - sum(before)` from the bricks they do not share; None
    /// when that cannot be told apart from a whole new grid.
    fn sum_change(before: Option<&Self>, after: Option<&Self>) -> Option<u128>;
    /// The hash of a grid whose bricks add up to `sum`.
    fn seal(grid: Option<&Self>, sum: u128) -> Hash128;
    /// The clay's grid of this kind.
    fn current(clay: &Clay) -> Option<&Self>;
    fn put(clay: &mut Clay, grid: Option<Self>);
    /// What a stroke on this grid reads besides the grid.
    fn context(clay: &Clay) -> Hash128;
    /// Applies a resolved stroke's part for this grid to `grid`, reading
    /// the clay's blobs (and, for paint, its sculpted surface).
    fn apply(clay: &Clay, grid: Option<&Self>, stroke: &Resolved) -> Result<Applied<Self>, String>;
    /// `before + t x (after - before)`.
    fn blend(before: Option<&Self>, after: Option<&Self>, t: f32) -> Result<Option<Self>, String>;
    /// `after - before`, when it can be kept as a change.
    fn delta(before: Option<&Self>, after: &Self) -> Option<Self>;
    fn add(base: Option<&Self>, delta: &Self) -> Result<Option<Self>, String>;
    fn stats(clay: &Clay, before: Option<&Self>, after: Option<&Self>, measure: bool) -> DiffStats;
    /// The grid's cell size.
    fn cell(grid: Option<&Self>) -> Option<f32>;
    /// The grid laid out with `regions` as its refined detail regions
    /// (a `Regions` record: Sculptris Pro growing them); `cell` is the
    /// coarse cell a grid made from nothing takes. Only sculpt grids
    /// have regions.
    fn relayout(_grid: Option<&Self>, _regions: &[Region], _cell: f32) -> Result<Option<Self>, String> {
        Err(format!("the {} layer has no detail regions", Self::LAYER))
    }
    /// The grid as a saved file reads it back, so an opaque record of it
    /// replays the same after a save and load (its `result_hash` holds).
    fn quantized(&self) -> Self {
        self.clone()
    }
    fn mirror(&self) -> Self;
    fn ledger(stack: &StackData) -> &Ledger<Self>;
    fn ledger_mut(stack: &mut StackData) -> &mut Ledger<Self>;
    fn cache(cache: &ReplayCache) -> &Mutex<Checkpoints<Self>>;
}

/// A stroke applied to a grid.
pub struct Applied<G> {
    pub grid: Option<G>,
    /// The radius the engine used (dabs are at least 1.5 cells).
    pub radius: f32,
}

/// What a record does to its grid.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase", bound = "")]
pub enum RecordOp<G: Grid> {
    /// A stroke: as authored, and resolved into the dabs that apply.
    Stroke { spec: Box<StrokeSpec>, stroke: Box<Resolved> },
    /// A grid without stroke history: `grid` replaces the grid, or with
    /// `delta` is added to it (an outside edit, a squashed range).
    Opaque {
        why: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        delta: bool,
        grid: Option<G>,
    },
    /// A mask or group action (grow, lasso, from_blobs, hide ...): as
    /// authored (`spec`, the tool's JSON) and resolved (`action`).
    Action { spec: Box<Value>, action: Box<G::Action> },
    /// The sculpt grid relaid with these refined detail regions (all of
    /// them, in order: `DetailGrid::set_regions`), as Sculptris Pro grows
    /// them before a stroke. It replays from the grid before it, so it
    /// keeps a few hundred bytes where an Opaque record would keep the
    /// whole grid. `cell` is the coarse cell a clay without a grid yet
    /// takes.
    Regions {
        #[serde(deserialize_with = "regions_in")]
        regions: Vec<Region>,
        cell: f32,
    },
}

/// A `Regions` record's regions: at most `MAX_REGIONS`, read no further
/// (a file cannot make it allocate more), each finite with radii and cell
/// above zero (`DetailGrid::set_regions` checks the rest when it replays).
fn regions_in<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Region>, D::Error> {
    struct Regions;
    impl<'de> serde::de::Visitor<'de> for Regions {
        type Value = Vec<Region>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a list of at most {MAX_REGIONS} detail regions")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(r) = seq.next_element::<Region>()? {
                if out.len() == MAX_REGIONS {
                    return Err(serde::de::Error::custom(format!("a regions record holds at most {MAX_REGIONS} detail regions")));
                }
                if !region_sane(&r) {
                    return Err(serde::de::Error::custom("a detail region needs a finite center and radii and cell > 0"));
                }
                out.push(r);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Regions)
}

/// What a `Regions` record may hold (before `Region::validate` against the
/// grid it replays on).
fn region_sane(r: &Region) -> bool {
    r.name.len() <= 256 && r.center.is_finite() && r.radii.is_finite() && r.radii.min_element() > 0.0 && r.cell.is_finite() && r.cell > 0.0
}

/// A `Regions` record's cell and regions are within bounds.
fn regions_sane(regions: &[Region], cell: f32) -> bool {
    regions.len() <= MAX_REGIONS && cell.is_finite() && (1e-5..=1e3).contains(&cell) && regions.iter().all(region_sane)
}

/// The sculpt and paint layers have no actions.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum NoAction {}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// One entry of a ledger.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(bound = "")]
pub struct Record<G: Grid> {
    /// Unique in its ledger; edits that change what a record does give it
    /// a new id.
    pub id: u64,
    pub source: Source,
    /// The tool call that made it (agent and UI records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// How much of the stroke applies (Adjust Last, `stroke_edit scale`):
    /// the grid before + factor x (after - before).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub factor: f32,
    /// The layer it was written to, resolved when it was made.
    pub layer: String,
    /// The grid's cell size when it was made (resolved, never re-derived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<f32>,
    pub op: RecordOp<G>,
    pub context_hash: Hash128,
    pub result_hash: Hash128,
    #[serde(default)]
    pub stats: DiffStats,
    /// The brush kernel version that made `result_hash`.
    pub engine: u16,
    /// Unix seconds.
    #[serde(default)]
    pub time: u64,
    /// Where a UI stroke's pressure came from (the call's
    /// `pressure_source`); not part of what it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_source: Option<PressureSource>,
    /// `digest()`'s cache (not saved).
    #[serde(skip)]
    pub digest: Digest,
}

/// A record's content hash, worked out once. Records compare equal
/// whether or not it has been (it follows from `op`); whoever changes a
/// record's `op` resets it.
#[derive(Clone, Debug, Default)]
pub struct Digest(std::sync::OnceLock<Hash128>);

impl PartialEq for Digest {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

fn dabs_digest(h: &mut Hasher, dabs: &[crate::brush::resolve::Dab]) {
    let f32s = |h: &mut Hasher, v: &[f32]| {
        h.u64(v.len() as u64);
        v.iter().for_each(|x| {
            h.bytes(&x.to_bits().to_le_bytes());
        });
    };
    let vec = |h: &mut Hasher, v: Option<Vec3>| f32s(h, &v.map_or(Vec::new(), |v| v.to_array().to_vec()));
    h.u64(dabs.len() as u64);
    for d in dabs {
        f32s(h, &d.points.iter().flat_map(|p| p.to_array()).collect::<Vec<f32>>());
        f32s(h, &[d.radius, d.gain]);
        h.bytes(&[d.radii.is_some() as u8, d.gains.is_some() as u8]);
        f32s(h, d.radii.as_deref().unwrap_or(&[]));
        f32s(h, d.gains.as_deref().unwrap_or(&[]));
        vec(h, d.delta);
        vec(h, d.normal);
        vec(h, d.tangent);
        // Only upright alphas and mirrored copies say so: older records
        // hash as they did.
        if let Some(u) = d.upright {
            h.bytes(b"upright");
            vec(h, Some(u));
        }
        if d.flip {
            h.bytes(b"flip");
        }
    }
}

impl<G: Grid> Record<G> {
    /// An Opaque record with a grid.
    pub fn holds_grid(&self) -> bool {
        matches!(self.op, RecordOp::Opaque { grid: Some(_), .. })
    }

    /// What the record does to its grid, hashed: the resolved stroke (or
    /// the opaque grid), so two records that apply differently never
    /// share a chain (and so a checkpoint) whatever their ids.
    pub fn digest(&self) -> Hash128 {
        *self.digest.0.get_or_init(|| match &self.op {
            RecordOp::Stroke { stroke, .. } => {
                let mut h = Hasher::new("stroke");
                h.json(&(&stroke.params, &stroke.mask, stroke.instances, &stroke.paint));
                dabs_digest(&mut h, &stroke.dabs);
                h.bytes(&[stroke.paint_dabs.is_some() as u8]);
                dabs_digest(&mut h, stroke.paint_dabs.as_deref().unwrap_or(&[]));
                // What it read of the mask and polygroups and the group it
                // paints change what it does; hashed only when present so
                // older records keep their digests.
                if let Some(f) = &stroke.frozen {
                    h.bytes(b"frozen").json(f);
                }
                if let Some(g) = stroke.group {
                    h.bytes(b"group").bytes(&g.to_le_bytes());
                }
                h.finish()
            }
            RecordOp::Opaque { delta, grid, .. } => Hasher::new("opaque").bytes(&[*delta as u8]).hash(G::hash(grid.as_ref())).finish(),
            RecordOp::Action { action, .. } => Hasher::new("action").json(action).finish(),
            RecordOp::Regions { regions, cell } => {
                // Bit for bit: a region that rounds alike in JSON but is
                // not the same float relays differently.
                let mut h = Hasher::new("regions");
                h.bytes(&cell.to_bits().to_le_bytes()).u64(regions.len() as u64);
                for r in regions {
                    h.u64(r.name.len() as u64).bytes(r.name.as_bytes());
                    for v in r.center.to_array().into_iter().chain(r.radii.to_array()).chain([r.cell]) {
                        h.bytes(&v.to_bits().to_le_bytes());
                    }
                }
                h.finish()
            }
        })
    }

    pub fn engine_stale(&self) -> bool {
        self.engine < ENGINE_VERSION
    }

    pub fn stroke(&self) -> Option<(&StrokeSpec, &Resolved)> {
        match &self.op {
            RecordOp::Stroke { spec, stroke } => Some((spec, stroke)),
            RecordOp::Opaque { .. } | RecordOp::Action { .. } | RecordOp::Regions { .. } => None,
        }
    }

    /// The record's context in a clay whose layer context is `ctx`: that,
    /// and for a stroke with a named alpha the image the name is bound to
    /// now (see `alpha_context`).
    pub fn context_in(&self, ctx: Hash128, clay: &Clay) -> Hash128 {
        alpha_context(ctx, self.stroke().map(|(_, r)| r), clay)
    }

    /// Whether what the record read changed since it was made.
    pub fn stale(&self, ctx: Hash128, clay: &Clay) -> bool {
        self.context_hash != self.context_in(ctx, clay)
    }

    fn points(&self) -> usize {
        self.stroke().map_or(0, |(_, r)| stroke_points(r))
    }
}

/// Who and why, for a new record.
#[derive(Clone, Debug)]
pub struct Meta {
    pub source: Source,
    pub tool: Option<String>,
    pub label: Option<String>,
    pub pressure_source: Option<PressureSource>,
}

impl Meta {
    #[cfg(test)]
    pub fn agent(tool: &str, label: Option<String>) -> Meta {
        Meta { source: Source::Agent, tool: Some(tool.into()), label, pressure_source: None }
    }

    /// A record made by `tool` for `source` (an agent call or the UI's
    /// request to the same tool).
    pub fn by(source: Source, tool: &str, label: Option<String>) -> Meta {
        Meta { source, tool: Some(tool.into()), label, pressure_source: None }
    }

    /// The meta with the call's `pressure_source` (a UI stroke's).
    pub fn pressed(mut self, args: &Value) -> Result<Meta, String> {
        self.pressure_source = match args.get("pressure_source").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(serde_json::from_value(v.clone()).map_err(|_| "pressure_source must be touch_force, nsevent or velocity")?),
        };
        Ok(self)
    }
}

/// The records of one grid. Compared by identity (see the module doc).
#[derive(Clone, Debug)]
pub struct Ledger<G: Grid>(Arc<LedgerData<G>>);

#[derive(Clone, Debug)]
pub struct LedgerData<G: Grid> {
    records: Vec<Arc<Record<G>>>,
    /// Seeds the chain: ledgers made apart never share checkpoints.
    uid: u64,
    next_id: u64,
    /// `chains[i]`: the hash of records 0..=i's ids, contents, switches
    /// and factors, which names the grid after record i (with the context).
    chains: Vec<Hash128>,
    /// Dab points over the records (`MAX_POINTS`).
    points: usize,
    /// The grid the records last produced, when known (not saved): a
    /// clay whose grid is another one was edited outside the ledger.
    tip: Option<Option<G>>,
    /// `G::sum` of the tip, when known.
    tip_sum: Option<u128>,
    /// What the replay on load found: Some(true) it matched the saved
    /// grid, Some(false) it did not (the saved grid is kept), None not
    /// checked.
    verified: Option<bool>,
}

impl<G: Grid> PartialEq for Ledger<G> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<G: Grid> Default for Ledger<G> {
    fn default() -> Self {
        Ledger::with_uid(new_uid())
    }
}

fn new_uid() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let h = xxh3_128(&[now.to_le_bytes(), n.to_le_bytes()].concat());
    h as u64
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl<G: Grid> Ledger<G> {
    fn with_uid(uid: u64) -> Ledger<G> {
        Ledger(Arc::new(LedgerData { records: Vec::new(), uid, next_id: 1, chains: Vec::new(), points: 0, tip: None, tip_sum: None, verified: None }))
    }

    fn from_data(mut data: LedgerData<G>) -> Ledger<G> {
        data.rechain();
        Ledger(Arc::new(data))
    }

    pub fn records(&self) -> &[Arc<Record<G>>] {
        &self.0.records
    }

    pub fn is_empty(&self) -> bool {
        self.0.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.records.len()
    }

    /// The ledger's identity as its content: changes with every edit.
    pub fn head(&self) -> Hash128 {
        self.0.chains.last().copied().unwrap_or_else(|| self.0.seed())
    }

    pub fn verified(&self) -> Option<bool> {
        self.0.verified
    }

    /// Whether `grid` is what the records produce (None: not known).
    pub fn in_sync(&self, grid: Option<&G>) -> bool {
        match &self.0.tip {
            Some(t) => t.as_ref() == grid,
            None => G::hash(grid) == self.result(),
        }
    }

    /// The grid hash the records end at.
    fn result(&self) -> Hash128 {
        self.0.records.last().map_or_else(|| G::hash(None), |r| r.result_hash)
    }

    /// Dab points over its records.
    pub fn points(&self) -> usize {
        self.0.points
    }

    /// Within the caps a file's ledgers are held to.
    pub(crate) fn is_sane(&self) -> bool {
        self.len() <= MAX_RECORDS && self.0.grids() <= MAX_OPAQUE_GRIDS && self.0.points <= MAX_POINTS && self.0.ops_sane()
    }

    /// Whether the grid the records end at is known (checked on load, or
    /// built this session).
    pub fn tip_known(&self) -> bool {
        self.0.tip.is_some()
    }

    /// The index of record `id`.
    pub fn position(&self, id: u64) -> Option<usize> {
        self.0.records.iter().position(|r| r.id == id)
    }

    fn data(&self) -> &LedgerData<G> {
        &self.0
    }
}

impl<G: Grid> LedgerData<G> {
    fn seed(&self) -> Hash128 {
        Hasher::new("ledger").u64(self.uid).finish()
    }

    fn rechain(&mut self) {
        self.points = self.records.iter().map(|r| r.points()).sum();
        let mut prev = self.seed();
        self.chains = self
            .records
            .iter()
            .map(|r| {
                prev = Self::link(prev, r);
                prev
            })
            .collect();
    }

    fn link(prev: Hash128, r: &Record<G>) -> Hash128 {
        Hasher::new("record").hash(prev).u64(r.id).hash(r.digest()).bytes(&[r.enabled as u8]).bytes(&r.factor.to_bits().to_le_bytes()).finish()
    }

    /// Every `Regions` record is within bounds and on a sculpt ledger.
    fn ops_sane(&self) -> bool {
        self.records.iter().all(|r| match &r.op {
            RecordOp::Regions { regions, cell } => G::LAYER == "base" && regions_sane(regions, *cell),
            _ => true,
        })
    }

    /// Records that hold a grid.
    fn grids(&self) -> usize {
        self.records.iter().filter(|r| r.holds_grid()).count()
    }

    /// Refused past `MAX_OPAQUE_GRIDS`.
    fn check_grids(&self) -> Result<(), String> {
        if self.grids() > MAX_OPAQUE_GRIDS {
            return Err(format!("the {} layer's ledger would hold more than {MAX_OPAQUE_GRIDS} grids from outside edits and squashes: squash old records into one or delete some (stroke_edit {{action: squash|delete, ids}}) first", G::LAYER));
        }
        Ok(())
    }

    /// Adds a record; refused past `MAX_RECORDS` (or `MAX_OPAQUE_GRIDS`).
    fn push(&mut self, record: Record<G>) -> Result<Arc<Record<G>>, String> {
        if record.holds_grid() && self.grids() >= MAX_OPAQUE_GRIDS {
            return Err(format!("the {} layer's ledger holds {MAX_OPAQUE_GRIDS} grids from outside edits and squashes: squash old records into one or delete some (stroke_edit {{action: squash|delete, ids}}) first", G::LAYER));
        }
        self.push_any(record)
    }

    /// `push` without the grid cap: `edit` checks it on the edited list,
    /// so a squash or delete can always bring a full ledger back under.
    fn push_any(&mut self, mut record: Record<G>) -> Result<Arc<Record<G>>, String> {
        if self.records.len() >= MAX_RECORDS {
            return Err(format!("the {} layer's ledger is full ({MAX_RECORDS} records): squash old records into one (stroke_edit {{action: squash, ids}}) first", G::LAYER));
        }
        record.id = self.next_id;
        self.next_id += 1;
        self.points += record.points();
        let r = Arc::new(record);
        self.records.push(r.clone());
        if self.chains.len() + 1 == self.records.len() {
            let prev = self.chains.last().copied().unwrap_or_else(|| self.seed());
            self.chains.push(Self::link(prev, &r));
        } else {
            self.rechain();
        }
        Ok(r)
    }
}

/// The saved form of a ledger.
#[derive(Serialize)]
#[serde(bound = "")]
struct LedgerOut<'a, G: Grid> {
    uid: u64,
    next_id: u64,
    records: Vec<&'a Record<G>>,
}

#[derive(Deserialize)]
#[serde(bound = "")]
struct LedgerIn<G: Grid> {
    uid: u64,
    next_id: u64,
    #[serde(deserialize_with = "records_in")]
    records: Vec<Record<G>>,
}

/// A file's records. Once there are more than `MAX_RECORDS`, or more than
/// `MAX_OPAQUE_GRIDS` hold a grid, the rest are skipped unread (no grid
/// of theirs is allocated); the list kept is over a cap, so
/// `SculptStack::is_sane` fails and `Clay::sanitize` drops the ledgers,
/// keeping the clay and its grids.
fn records_in<'de, D: serde::Deserializer<'de>, G: Grid>(d: D) -> Result<Vec<Record<G>>, D::Error> {
    struct Records<G>(std::marker::PhantomData<G>);
    impl<'de, G: Grid> serde::de::Visitor<'de> for Records<G> {
        type Value = Vec<Record<G>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a list of at most {MAX_RECORDS} ledger records")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1024));
            let mut grids = 0;
            while let Some(r) = seq.next_element::<Record<G>>()? {
                grids += r.holds_grid() as usize;
                out.push(r);
                if out.len() > MAX_RECORDS || grids > MAX_OPAQUE_GRIDS {
                    while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                    break;
                }
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Records(std::marker::PhantomData))
}

impl<G: Grid> Serialize for Ledger<G> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = &self.0;
        LedgerOut { uid: d.uid, next_id: d.next_id, records: d.records.iter().map(|r| &**r).collect() }.serialize(s)
    }
}

impl<'de, G: Grid> Deserialize<'de> for Ledger<G> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Too many records, grids (see `records_in`) or points:
        // `Clay::sanitize` drops the ledgers.
        let file = crate::clay::inflate_budget(crate::clay::INFLATE_BUDGET, || LedgerIn::<G>::deserialize(d))?;
        let mut ids = std::collections::HashSet::new();
        if !file.records.iter().all(|r| ids.insert(r.id) && r.id < file.next_id) {
            return Err(serde::de::Error::custom("ledger record ids must be unique and below next_id"));
        }
        Ok(Ledger::from_data(LedgerData { records: file.records.into_iter().map(Arc::new).collect(), uid: file.uid, next_id: file.next_id, chains: Vec::new(), points: 0, tip: None, tip_sum: None, verified: None }))
    }
}

/// Checkpoint grids by (record chain, context): the grid after the record
/// the chain ends at, replayed in that context.
pub struct Checkpoints<G> {
    map: HashMap<(Hash128, Hash128), (Option<G>, u64)>,
    tick: u64,
}

impl<G: Clone> Default for Checkpoints<G> {
    fn default() -> Self {
        Checkpoints { map: HashMap::new(), tick: 0 }
    }
}

impl<G: Clone> Checkpoints<G> {
    fn get(&mut self, key: (Hash128, Hash128)) -> Option<Option<G>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(&key).map(|(g, t)| {
            *t = tick;
            g.clone()
        })
    }

    fn put(&mut self, key: (Hash128, Hash128), grid: Option<G>) {
        self.tick += 1;
        self.map.insert(key, (grid, self.tick));
        while self.map.len() > CACHE_CHECKPOINTS {
            let oldest = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k).expect("not empty");
            self.map.remove(&oldest);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Replay checkpoints for every ledger (never saved, never undone; the
/// editor keeps one and jobs share it).
#[derive(Default)]
pub struct ReplayCache {
    detail: Mutex<Checkpoints<Detail>>,
    paint: Mutex<Checkpoints<Paint>>,
    mask: Mutex<Checkpoints<SculptMask>>,
    groups: Mutex<Checkpoints<Groups>>,
}

impl ReplayCache {
    pub fn checkpoints(&self) -> usize {
        self.detail.lock().map_or(0, |c| c.len()) + self.paint.lock().map_or(0, |c| c.len()) + self.mask.lock().map_or(0, |c| c.len()) + self.groups.lock().map_or(0, |c| c.len())
    }
}

/// The clay's ledgers: the base sculpt layer's and the paint layer's.
/// Compared by identity like the grids.
#[derive(Clone, Debug, Default)]
pub struct SculptStack(Arc<StackData>);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StackData {
    /// The base sculpt layer (its grid is `Clay::detail`).
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub base: Ledger<Detail>,
    /// The paint layer (its grid is `Clay::paint`).
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub paint: Ledger<Paint>,
    /// The alpha images the records use, and the names they are bound to.
    #[serde(default, skip_serializing_if = "AlphaStore::is_empty")]
    pub alphas: AlphaStore,
    /// The persistent mask (its grid is `Clay::sculpt_mask`).
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub mask: Ledger<SculptMask>,
    /// The polygroups (their grid is `Clay::groups`).
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub groups: Ledger<Groups>,
    /// The masks and groups the records read, by hash.
    #[serde(default, skip_serializing_if = "Frozen::is_empty")]
    pub frozen: Frozen,
    /// Sculpt and paint layers above base and paint (see `layers.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::layers::capped_layers")]
    pub layers: Vec<crate::layers::Layer>,
    /// Morph targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::layers::capped_morphs")]
    pub morphs: Vec<crate::layers::Morph>,
    /// Ledgers of baked and merged layers, for reading.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::layers::capped_archive")]
    pub archived: Vec<crate::layers::Archived>,
    /// The layer sculpt calls without `layer` write (`sculpt_layer record`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_layer: Option<String>,
    /// Detail levels finer than the clay's own cell, coarse first, and
    /// the active one's cell (None: level 0); see `levels.rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::levels::read_kept")]
    pub levels: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
    /// The composites, kept beside the stack (never saved).
    #[serde(skip)]
    pub cache: crate::layers::Cache,
    /// Set on a view of one extra layer (`layers::view`; never saved).
    #[serde(skip)]
    pub view: Option<Arc<crate::layers::View>>,
}

/// Most masks, and most polygroup grids, a clay keeps for its records.
pub const MAX_FROZEN: usize = 256;

/// The persistent masks and polygroups strokes (and mask and group
/// actions) read, by content hash: a record names the one it read
/// (`Resolved::frozen`), so it replays the same after the mask or the
/// groups change, as an alpha record keeps its image.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Frozen {
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "frozen_map")]
    pub masks: std::collections::BTreeMap<Hash128, SculptMask>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "frozen_map")]
    pub groups: std::collections::BTreeMap<Hash128, Groups>,
}

fn frozen_map<'de, D: serde::Deserializer<'de>, V: Deserialize<'de>>(d: D) -> Result<std::collections::BTreeMap<Hash128, V>, D::Error> {
    crate::clay::in_inflate_budget_with(d, crate::brush::alpha::capped_map::<D, Hash128, V, MAX_FROZEN>)
}

impl Frozen {
    pub fn is_empty(&self) -> bool {
        self.masks.is_empty() && self.groups.is_empty()
    }

    fn is_sane(&self) -> bool {
        self.masks.len() <= MAX_FROZEN && self.groups.len() <= MAX_FROZEN && self.masks.values().all(SculptMask::is_sane) && self.groups.values().all(Groups::is_sane)
    }
}

impl PartialEq for SculptStack {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Serialize for SculptStack {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for SculptStack {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(SculptStack(Arc::new(StackData::deserialize(d)?)))
    }
}

impl SculptStack {
    pub fn data(&self) -> &StackData {
        &self.0
    }

    pub(crate) fn new(data: StackData) -> SculptStack {
        SculptStack(Arc::new(data))
    }

    pub(crate) fn is_sane(&self) -> bool {
        let d = &self.0;
        let lens = [d.base.len(), d.paint.len(), d.mask.len(), d.groups.len()];
        let grids = [d.base.0.grids(), d.paint.0.grids(), d.mask.0.grids(), d.groups.0.grids()];
        let ops = d.base.0.ops_sane() && d.paint.0.ops_sane() && d.mask.0.ops_sane() && d.groups.0.ops_sane();
        lens.iter().all(|n| *n <= MAX_RECORDS) && ops && d.points() <= MAX_POINTS && grids.iter().all(|n| *n <= MAX_OPAQUE_GRIDS) && d.alphas.is_sane() && d.frozen.is_sane() && crate::layers::is_sane(d)
    }

    /// The stack for a clay mirrored across x = 0 (`Clay::mirrored_x`):
    /// strokes reflected (as replay records), grids mirrored; `detail` and
    /// `paint` are the mirrored clay's grids. No replay happens: result
    /// hashes are unknown (zero) until a replay or rebase.
    pub(crate) fn mirrored_x(&self, clay: &Clay) -> SculptStack {
        // Mirrored masks and groups hash apart: records name the copies.
        let d = &self.0;
        let mut remap = HashMap::new();
        let mut frozen = Frozen::default();
        for m in d.frozen.masks.values() {
            let (old, copy) = (SculptMask::hash(Some(m)), m.mirrored_x());
            let new = SculptMask::hash(Some(&copy));
            remap.insert(old, new);
            frozen.masks.insert(new, copy);
        }
        for g in d.frozen.groups.values() {
            let (old, copy) = (Groups::hash(Some(g)), g.mirrored_x());
            let new = Groups::hash(Some(&copy));
            remap.insert(old, new);
            frozen.groups.insert(new, copy);
        }
        let remap = |h: Hash128| remap.get(&h).copied().unwrap_or(h);
        SculptStack(Arc::new(StackData {
            base: mirror_ledger(&d.base, clay.detail.as_ref(), &remap),
            paint: mirror_ledger(&d.paint, clay.paint.as_ref(), &remap),
            mask: mirror_ledger(&d.mask, clay.sculpt_mask.as_ref(), &remap),
            groups: mirror_ledger(&d.groups, clay.groups.as_ref(), &remap),
            alphas: d.alphas.clone(),
            frozen,
            layers: d
                .layers
                .iter()
                .map(|l| crate::layers::Layer {
                    detail: l.detail.as_ref().map(Detail::mirrored_x),
                    strokes: mirror_ledger(&l.strokes, l.detail.as_ref().map(Detail::mirrored_x).as_ref(), &remap),
                    paint: l.paint.as_ref().map(Paint::mirrored_x),
                    paint_strokes: mirror_ledger(&l.paint_strokes, l.paint.as_ref().map(Paint::mirrored_x).as_ref(), &remap),
                    ..l.clone()
                })
                .collect(),
            morphs: d
                .morphs
                .iter()
                .map(|m| {
                    let base = m.base.as_ref().map(Detail::mirrored_x);
                    let layers = m.layers.iter().map(|l| {
                        let detail = l.detail.as_ref().map(Detail::mirrored_x);
                        crate::layers::MorphLayer { strokes: mirror_ledger(&l.strokes, detail.as_ref(), &remap), detail, ..l.clone() }
                    });
                    crate::layers::Morph { base_strokes: mirror_ledger(&m.base_strokes, base.as_ref(), &remap), base, layers: layers.collect(), ..m.clone() }
                })
                .collect(),
            archived: Vec::new(),
            record_layer: d.record_layer.clone(),
            levels: d.levels.clone(),
            level: d.level,
            cache: Default::default(),
            view: None,
        }))
    }
}

impl StackData {
    /// Dab points over every ledger.
    fn points(&self) -> usize {
        self.base.0.points + self.paint.0.points + self.mask.0.points + self.groups.0.points + crate::layers::extra_points(self)
    }
}

fn mirror_ledger<G: Grid>(l: &Ledger<G>, grid: Option<&G>, remap: &dyn Fn(Hash128) -> Hash128) -> Ledger<G> {
    let d = l.data();
    let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
    let records = d
        .records
        .iter()
        .map(|r| {
            let op = match &r.op {
                RecordOp::Stroke { spec, stroke } => {
                    let mut s = (**stroke).clone();
                    for dab in s.dabs.iter_mut().chain(s.paint_dabs.iter_mut().flatten()) {
                        dab.points.iter_mut().for_each(|p| *p = flip(*p));
                        dab.delta = dab.delta.map(flip);
                        dab.normal = dab.normal.map(flip);
                        // An oriented dab's frame turns left-handed.
                        if dab.tangent.is_some() || dab.upright.is_some() {
                            dab.tangent = dab.tangent.map(flip);
                            dab.upright = dab.upright.map(flip);
                            dab.flip = !dab.flip;
                        }
                    }
                    if let Some(f) = &mut s.frozen {
                        (f.mask, f.groups) = (f.mask.map(remap), f.groups.map(remap));
                    }
                    if let Some(t) = &mut s.params.transform {
                        let m = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
                        *t = m * *t * m;
                    }
                    RecordOp::Stroke { spec: spec.clone(), stroke: Box::new(s) }
                }
                RecordOp::Opaque { why, delta, grid } => RecordOp::Opaque { why: why.clone(), delta: *delta, grid: grid.as_ref().map(G::mirror) },
                RecordOp::Action { spec, action } => RecordOp::Action { spec: spec.clone(), action: Box::new(G::remap_action(&G::mirror_action(action), remap)) },
                // As `DetailGrid::mirrored_x` mirrors a grid's regions.
                RecordOp::Regions { regions, cell } => RecordOp::Regions {
                    regions: regions
                        .iter()
                        .map(|r| Region { name: crate::rig::mirror_name(&r.name).unwrap_or_else(|| r.name.clone()), center: flip(r.center), ..r.clone() })
                        .collect(),
                    cell: *cell,
                },
            };
            Arc::new(Record { op, source: Source::Replay, result_hash: Hash128::default(), stats: DiffStats { bbox: None, ..r.stats.clone() }, digest: Digest::default(), ..(**r).clone() })
        })
        .collect();
    let data = LedgerData { records, uid: d.uid ^ 0x6d69_7272_6f72_0001, next_id: d.next_id, chains: Vec::new(), points: 0, tip: Some(grid.cloned()), tip_sum: None, verified: None };
    Ledger::from_data(data)
}

/// The clay's ledger of grid kind G (empty if it has none yet).
pub fn ledger<G: Grid>(clay: &Clay) -> Ledger<G> {
    clay.sculpt.as_ref().map(|s| G::ledger(&s.0).clone()).unwrap_or_default()
}

/// Puts a ledger and its grid on the clay.
pub fn install<G: Grid>(clay: &mut Clay, l: Ledger<G>, grid: Option<G>) {
    let mut stack = clay.sculpt.take().unwrap_or_default();
    *G::ledger_mut(Arc::make_mut(&mut stack.0)) = l;
    clay.sculpt = Some(stack);
    G::put(clay, grid);
}

/// A stroke's context: the layer's `ctx`, and for a named alpha the image
/// the clay binds that name to (the stroke's own until the name is
/// imported again with other pixels: then the record is stale_context).
/// Strokes without a named alpha keep `ctx` as it is.
pub fn alpha_context(ctx: Hash128, stroke: Option<&Resolved>, clay: &Clay) -> Hash128 {
    let Some(a) = stroke.and_then(|s| s.params.alpha.as_ref()) else { return ctx };
    let Some(name) = &a.name else { return ctx };
    let bound = clay.sculpt.as_ref().and_then(|s| s.0.alphas.names.get(name).copied()).unwrap_or(a.hash);
    Hasher::new("alpha context").hash(ctx).bytes(name.as_bytes()).hash(bound).finish()
}

/// The alpha names and images the clay's records (base and paint) use.
pub fn alphas_in_use(clay: &Clay) -> (std::collections::HashSet<String>, std::collections::HashSet<Hash128>) {
    let (mut names, mut hashes) = (std::collections::HashSet::new(), std::collections::HashSet::new());
    alpha_strokes(clay, |r| {
        let a = r.params.alpha.as_ref().expect("an alpha stroke");
        hashes.insert(a.hash);
        if let Some(n) = &a.name {
            names.insert(n.clone());
        }
    });
    (names, hashes)
}

/// Calls `f` with every resolved stroke of the clay's records (the layers'
/// and morphs' too) that uses an alpha.
pub fn alpha_strokes(clay: &Clay, mut f: impl FnMut(&Resolved)) {
    let mut add = |s: Option<(&StrokeSpec, &Resolved)>| {
        if let Some((_, r)) = s.filter(|(_, r)| r.params.alpha.is_some()) {
            f(r);
        }
    };
    ledger::<Detail>(clay).records().iter().for_each(|r| add(r.stroke()));
    ledger::<Paint>(clay).records().iter().for_each(|r| add(r.stroke()));
    ledger::<SculptMask>(clay).records().iter().for_each(|r| add(r.stroke()));
    ledger::<Groups>(clay).records().iter().for_each(|r| add(r.stroke()));
    crate::layers::detail_ledgers(clay).iter().for_each(|l| l.records().iter().for_each(|r| add(r.stroke())));
    crate::layers::paint_ledgers(clay).iter().for_each(|l| l.records().iter().for_each(|r| add(r.stroke())));
}

/// Calls `f` with every resolved stroke that uses an alpha in the
/// archived layers' records (`sculpt_layer` folds keep them for reading).
/// Not in `alpha_strokes`: the alpha store keeps nothing for them.
pub fn archived_alpha_strokes(clay: &Clay, mut f: impl FnMut(&Resolved)) {
    let Some(s) = clay.sculpt.as_ref() else { return };
    let mut add = |s: Option<(&StrokeSpec, &Resolved)>| {
        if let Some((_, r)) = s.filter(|(_, r)| r.params.alpha.is_some()) {
            f(r);
        }
    };
    for a in &s.data().archived {
        a.strokes.records().iter().for_each(|r| add(r.stroke()));
        a.paint_strokes.records().iter().for_each(|r| add(r.stroke()));
    }
}

/// The frozen masks and groups the clay's records read.
pub fn frozen_in_use(clay: &Clay) -> std::collections::HashSet<Hash128> {
    fn one<G: Grid>(l: &Ledger<G>, out: &mut std::collections::HashSet<Hash128>) {
        for r in l.records() {
            match &r.op {
                RecordOp::Stroke { stroke, .. } => out.extend(stroke.frozen.iter().flat_map(|f| f.mask.into_iter().chain(f.groups))),
                RecordOp::Action { action, .. } => out.extend(G::action_reads(action)),
                RecordOp::Opaque { .. } | RecordOp::Regions { .. } => {}
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    one::<Detail>(&ledger(clay), &mut out);
    one::<Paint>(&ledger(clay), &mut out);
    one::<SculptMask>(&ledger(clay), &mut out);
    one::<Groups>(&ledger(clay), &mut out);
    // The layers' and morphs' records too.
    crate::layers::detail_ledgers(clay).iter().for_each(|l| one::<Detail>(l, &mut out));
    crate::layers::paint_ledgers(clay).iter().for_each(|l| one::<Paint>(l, &mut out));
    out
}

/// What a stroke about to be committed reads of the clay's persistent
/// mask and polygroups, kept on the clay by hash (records no longer
/// using a kept grid let it go first). Strokes that read neither stay
/// as before (None).
pub fn freeze(clay: &mut Clay, stroke: &mut Resolved) -> Result<(), String> {
    let m = stroke.mask.as_ref();
    let mask = clay.sculpt_mask.clone().filter(|_| m.is_none_or(|m| m.channel()));
    let groups = clay.groups.clone().filter(|g| !g.hidden().is_empty() || m.is_some_and(|m| m.groups.is_some()));
    stroke.frozen = (mask.is_some() || groups.is_some()).then(|| FrozenRef { mask: mask.as_ref().map(|g| SculptMask::hash(Some(g))), groups: groups.as_ref().map(|g| Groups::hash(Some(g))) });
    if stroke.frozen.is_some() {
        keep_frozen(clay, mask, groups)?;
    }
    Ok(())
}

/// Keeps `mask` and `groups` in the clay's frozen store (by hash).
pub fn keep_frozen(clay: &mut Clay, mask: Option<SculptMask>, groups: Option<Groups>) -> Result<(), String> {
    let used = frozen_in_use(clay);
    let mut stack = clay.sculpt.take().unwrap_or_default();
    let f = &mut Arc::make_mut(&mut stack.0).frozen;
    f.masks.retain(|h, _| used.contains(h));
    f.groups.retain(|h, _| used.contains(h));
    let mut full = false;
    if let Some(m) = mask {
        let h = SculptMask::hash(Some(&m));
        full |= !f.masks.contains_key(&h) && f.masks.len() >= MAX_FROZEN;
        f.masks.entry(h).or_insert(m);
    }
    if let Some(g) = groups {
        let h = Groups::hash(Some(&g));
        full |= !f.groups.contains_key(&h) && f.groups.len() >= MAX_FROZEN;
        f.groups.entry(h).or_insert(g);
    }
    clay.sculpt = Some(stack);
    if full {
        return Err(format!("the clay keeps at most {MAX_FROZEN} masks and {MAX_FROZEN} group grids for its strokes; squash old strokes (stroke_edit {{action: squash, ids}}) first"));
    }
    Ok(())
}

/// The frozen mask named `h`, if the clay keeps it.
pub fn frozen_mask(clay: &Clay, h: Hash128) -> Option<SculptMask> {
    clay.sculpt.as_ref().and_then(|s| s.0.frozen.masks.get(&h).cloned())
}

/// The frozen groups named `h`, if the clay keeps them.
pub fn frozen_groups(clay: &Clay, h: Hash128) -> Option<Groups> {
    clay.sculpt.as_ref().and_then(|s| s.0.frozen.groups.get(&h).cloned())
}

/// After a job made frozen grids on a copy of the clay (and its records
/// were installed): the copy's grids join the clay's, and grids no record
/// on the clay reads any more go, so the store never outgrows what its
/// strokes need (`MAX_FROZEN` holds for the clay, not only the copy).
pub fn merge_frozen(cur: &mut Clay, from: &Clay) {
    let theirs = from.sculpt.as_ref().map(|s| &s.0.frozen);
    let used = frozen_in_use(cur);
    let mine = cur.sculpt.as_ref().map(|s| &s.0.frozen);
    let lacks = |h: &Hash128, has: &dyn Fn(&Frozen) -> bool| used.contains(h) && mine.is_none_or(|m| !has(m));
    let missing = theirs.is_some_and(|t| t.masks.keys().any(|h| lacks(h, &|m| m.masks.contains_key(h))) || t.groups.keys().any(|h| lacks(h, &|m| m.groups.contains_key(h))));
    let unused = mine.is_some_and(|m| m.masks.keys().chain(m.groups.keys()).any(|h| !used.contains(h)));
    if !missing && !unused {
        return;
    }
    let mut stack = cur.sculpt.take().unwrap_or_default();
    let f = &mut Arc::make_mut(&mut stack.0).frozen;
    if let Some(t) = theirs {
        for (h, m) in t.masks.iter().filter(|(h, _)| used.contains(*h)) {
            f.masks.entry(*h).or_insert_with(|| m.clone());
        }
        for (h, g) in t.groups.iter().filter(|(h, _)| used.contains(*h)) {
            f.groups.entry(*h).or_insert_with(|| g.clone());
        }
    }
    f.masks.retain(|h, _| used.contains(h));
    f.groups.retain(|h, _| used.contains(h));
    cur.sculpt = Some(stack);
}

/// Puts the alphas its strokes use on the clay (see `alpha::AlphaStore`).
pub fn set_alphas(clay: &mut Clay, alphas: AlphaStore) {
    if clay.sculpt.as_ref().map_or(alphas.is_empty(), |s| s.0.alphas == alphas) {
        return;
    }
    let mut stack = clay.sculpt.take().unwrap_or_default();
    Arc::make_mut(&mut stack.0).alphas = alphas;
    clay.sculpt = Some(stack);
}

/// What a sculpt grid is, cheaply: its ledger's head while the records end
/// at it (or the ledger is unchecked: it ends at the saved grid), else
/// the grid's hash.
pub fn grid_state(l: &Ledger<Detail>, grid: Option<&Detail>) -> Hash128 {
    let known = match &l.0.tip {
        Some(_) => l.in_sync(grid),
        None => !l.is_empty(),
    };
    if known { l.head() } else { Detail::hash(grid) }
}

/// `h` and `v` hashed under `tag`.
pub fn hash_with(tag: &str, h: Hash128, v: &impl Serialize) -> Hash128 {
    Hasher::new(tag).hash(h).json(v).finish()
}

/// The clay's stack to change (made if it has none).
pub(crate) fn stack_mut(clay: &mut Clay) -> &mut StackData {
    Arc::make_mut(&mut clay.sculpt.get_or_insert_with(Default::default).0)
}

/// The sculpted surface as the layers on it read it (paint, mask and
/// groups): the base ledger's head, or the grid's hash without one. An
/// unchecked base ledger (tip unknown) ends at the saved grid, the
/// file's rounding of its result: the surface is its records, as when
/// checked.
pub fn surface_hash(clay: &Clay) -> Hash128 {
    let base = ledger::<Detail>(clay);
    let known = match &base.0.tip {
        Some(_) => base.in_sync(clay.detail.as_ref()),
        None => !base.is_empty(),
    };
    let h = if clay.sculpt.is_some() && known { base.head() } else { Detail::hash(clay.detail.as_ref()) };
    // The sculpt layers on top (clays without any hash as before).
    crate::layers::surface_context(clay, h)
}

/// The blobs and settings every stroke reads (the base field).
pub fn clay_context(clay: &Clay) -> Hash128 {
    Hasher::new("clay").json(&(&clay.blobs, clay.resolution, clay.detail_cell, clay.shell, clay.sharp)).finish()
}

type Brick = Arc<[f32; crate::clay::BRICK_VALUES]>;

/// The bricks of one level two grids do not share (by address), sorted by
/// key: (key, before's, after's).
fn differing<'a>(bg: Option<&'a DetailGrid>, ag: Option<&'a DetailGrid>) -> Vec<([i32; 3], Option<&'a Brick>, Option<&'a Brick>)> {
    let mut out = Vec::new();
    if let Some(a) = ag {
        for key in a.brick_keys() {
            let (x, y) = (bg.and_then(|b| b.brick(key)), a.brick(key));
            if !matches!((x, y), (Some(x), Some(y)) if Arc::ptr_eq(x, y)) {
                out.push((*key, x, y));
            }
        }
    }
    if let Some(b) = bg {
        out.extend(b.brick_keys().filter(|k| ag.is_none_or(|a| a.brick(k).is_none())).map(|k| (*k, b.brick(k), None)));
    }
    out.sort_unstable_by_key(|e| e.0);
    out
}

/// One brick's hash on level `k` (0: the coarse grid).
fn brick_hash(k: u64, key: &[i32; 3], values: &[f32; crate::clay::BRICK_VALUES]) -> u128 {
    let mut buf = Vec::with_capacity(20 + values.len() * 4);
    buf.extend(k.to_le_bytes());
    key.iter().for_each(|c| buf.extend(c.to_le_bytes()));
    values.iter().for_each(|v| buf.extend(v.to_bits().to_le_bytes()));
    xxh3_128(&buf)
}

/// Grid points of two detail grids that differ (by level, comparing
/// shared bricks by address first): `f(object-space point, cell, before,
/// after)`. Returns the bricks that differ.
pub fn detail_changes(before: Option<&Detail>, after: Option<&Detail>, mut f: impl FnMut(Vec3, f32, f32, f32)) -> usize {
    let empty = Detail::new(after.or(before).map_or(1.0, Detail::voxel));
    let (b, a) = (before.unwrap_or(&empty).grid(), after.unwrap_or(&empty).grid());
    let mut touched = 0;
    let mut level = |bg: Option<&DetailGrid>, ag: Option<&DetailGrid>| {
        let voxel = ag.or(bg).map_or(1.0, |g| g.voxel);
        for (key, x, y) in differing(bg, ag) {
            let mut any = false;
            for i in 0..crate::clay::BRICK_VALUES {
                let (u, v) = (x.map_or(0.0, |x| x[i]), y.map_or(0.0, |y| y[i]));
                if u != v {
                    any = true;
                    let l = [i as i32 % 8, i as i32 / 8 % 8, i as i32 / 64];
                    let p = Vec3::new((key[0] * 8 + l[0]) as f32, (key[1] * 8 + l[1]) as f32, (key[2] * 8 + l[2]) as f32) * voxel;
                    f(p, voxel, u, v);
                }
            }
            touched += any as usize;
        }
    };
    level(Some(b), Some(a));
    for k in 0..b.levels().len().max(a.levels().len()) {
        level(b.levels().get(k).map(|l| &l.grid), a.levels().get(k).map(|l| &l.grid));
    }
    touched
}

/// Running change statistics.
#[derive(Default)]
struct Tally {
    n: u64,
    max: f32,
    sum: f64,
    sq: f64,
    lo: Vec3,
    hi: Vec3,
}

impl Tally {
    fn add(&mut self, p: Vec3, d: f32) {
        if self.n == 0 {
            (self.lo, self.hi) = (p, p);
        }
        self.n += 1;
        self.max = self.max.max(d);
        self.sum += d as f64;
        self.sq += (d as f64) * (d as f64);
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }

    fn stats(&self, bricks: usize) -> DiffStats {
        DiffStats {
            changed_points: self.n,
            bricks_touched: bricks as u64,
            bbox: (self.n > 0).then(|| [self.lo.to_array(), self.hi.to_array()]),
            max_disp: self.max,
            mean_disp: if self.n > 0 { (self.sum / self.n as f64) as f32 } else { 0.0 },
            volume_delta: None,
        }
    }
}

/// Volume the offsets add between two detail grids (coarse grid only):
/// the change in a soft inside count over the changed points, using the
/// blobs' field there.
fn volume_delta(clay: &Clay, before: Option<&Detail>, after: Option<&Detail>) -> f32 {
    let voxel = after.or(before).map_or(1.0, Detail::voxel);
    let base = Clay { detail: None, paint: None, sculpt: None, ..clay.clone() };
    let field = Field::new(&base, voxel);
    let occupied = |f: f32| (0.5 - f / voxel).clamp(0.0, 1.0);
    let mut points = Vec::new();
    // The coarse grid's changes only (levels hold refinements of it).
    let only = |d: Option<&Detail>| d.map(Detail::without_levels);
    detail_changes(only(before).as_ref(), only(after).as_ref(), |p, _, u, v| points.push((p, u, v)));
    let mut total = 0.0f64;
    let mut list = Vec::new();
    let mut at = None;
    for (p, u, v) in points {
        let cell = (p / (voxel * 8.0)).floor();
        if at != Some(cell) {
            let lo = cell * voxel * 8.0;
            list = field.near(lo - voxel * 2.0, lo + voxel * 10.0);
            at = Some(cell);
        }
        let b = field.sample_base(p, &list).0;
        total += (occupied(b + v) - occupied(b + u)) as f64;
    }
    (total * (voxel as f64).powi(3)) as f32
}

/// What a sculpt stroke's dabs run on (`Detail::apply`, and the editor's
/// live preview, which runs them a few at a time): the grid the engine
/// reads and writes, and on a clay with sculpt layers the amount of the
/// layer the change folds back into. On such a clay the stroke reads them
/// all and writes its own layer: the engine runs on the composite, and the
/// change over the layer's amount goes into the layer's grid.
pub struct SculptInput {
    /// The engine's grid before the stroke.
    pub grid: Option<Detail>,
    /// The written layer's amount (None: the engine writes the grid itself).
    amount: Option<f32>,
    /// What the other layers add to `grid` (detail transport leaves it
    /// where it is: `engine::Ctx::others`).
    others: Option<Detail>,
}

impl SculptInput {
    /// The input of a stroke on `clay` (or a layer view of it) whose own
    /// grid is `grid`.
    pub fn of(clay: &Clay, grid: Option<&Detail>) -> Result<SculptInput, String> {
        let Some((others, amount)) = crate::layers::sculpt_input(clay)? else {
            return Ok(SculptInput { grid: grid.cloned(), amount: None, others: None });
        };
        if amount.abs() < crate::layers::MIN_AMOUNT {
            return Err(format!("the layer is at amount {amount}: strokes are refused below {} (raise it with sculpt_layer amount)", crate::layers::MIN_AMOUNT));
        }
        let input = match (others.as_ref(), grid) {
            (None, None) => None,
            (None, Some(g)) => Some(Detail::compose(&[(g, amount)])?),
            (Some(o), None) => Some(o.clone()),
            (Some(o), Some(g)) => Some(Detail::compose(&[(o, 1.0), (g, amount)])?),
        };
        Ok(SculptInput { grid: input, amount: Some(amount), others })
    }

    /// Runs `dabs` (the stroke's, or a run of them) from `from`: the input,
    /// or what earlier dabs of the same stroke left. The engine applies
    /// dabs one after another, each from the grid before it (see
    /// `sculpt::engine`), so running a stroke's dabs in pieces, each from
    /// the last, gives the grid running them at once does, bit for bit.
    pub fn run(&self, clay: &Clay, from: Option<&Detail>, stroke: &Resolved, dabs: &[crate::brush::resolve::Dab]) -> Result<crate::sculpt::engine::Diff, String> {
        let alpha = crate::sculpt::engine::alpha_of(clay, &stroke.params)?;
        let gate = crate::sculpt::engine::Gate::of(clay, stroke)?;
        let probe = Clay { detail: from.cloned(), sculpt: None, ..clay.clone() };
        let ctx = crate::sculpt::engine::Ctx { clay: &probe, params: &stroke.params, alpha: alpha.as_ref(), gate: &gate, others: self.others.as_ref() };
        crate::sculpt::engine::apply(&ctx, from, dabs, stroke.mask.as_ref())
    }

    /// The written grid after the whole stroke ran to `diff` (its grid is
    /// the engine's, from this input): `grid` is the layer's own before.
    pub fn finish(&self, grid: Option<&Detail>, diff: crate::sculpt::engine::Diff, stroke: &Resolved) -> Result<Applied<Detail>, String> {
        if diff.dabs == 0 {
            return Ok(Applied { grid: grid.cloned(), radius: stroke.dabs.first().map_or(0.0, |d| d.radius) });
        }
        let Some(amount) = self.amount else {
            return Ok(Applied { grid: Some(diff.detail), radius: diff.radius });
        };
        let before = self.grid.clone().unwrap_or_else(|| diff.detail.empty_like());
        let after = Detail::fold_change(grid, &before, &diff.detail, amount)?;
        Ok(Applied { grid: Some(after), radius: diff.radius })
    }
}

impl Grid for Detail {
    const LAYER: &'static str = "base";
    type Action = NoAction;

    fn sum(grid: Option<&Self>) -> u128 {
        let Some(d) = grid else { return 0 };
        let g = d.grid();
        let level = |k: u64, g: &DetailGrid| g.bricks().fold(0u128, |s, (key, values)| s.wrapping_add(brick_hash(k, key, values)));
        g.levels().iter().enumerate().fold(level(0, g), |s, (k, l)| s.wrapping_add(level(k as u64 + 1, &l.grid)))
    }

    fn sum_change(before: Option<&Self>, after: Option<&Self>) -> Option<u128> {
        let (b, a) = (before?.grid(), after?.grid());
        if !b.same_layout(a) {
            return None;
        }
        let level = |k: u64, bg: &DetailGrid, ag: &DetailGrid| {
            differing(Some(bg), Some(ag)).into_iter().fold(0u128, |s, (key, x, y)| {
                let s = y.map_or(s, |y| s.wrapping_add(brick_hash(k, &key, y)));
                x.map_or(s, |x| s.wrapping_sub(brick_hash(k, &key, x)))
            })
        };
        Some(b.levels().iter().zip(a.levels()).enumerate().fold(level(0, b, a), |s, (k, (lb, la))| s.wrapping_add(level(k as u64 + 1, &lb.grid, &la.grid))))
    }

    fn seal(grid: Option<&Self>, sum: u128) -> Hash128 {
        let Some(d) = grid else { return Hasher::new("detail:none").finish() };
        let g = d.grid();
        let mut head = Hasher::new("detail");
        head.bytes(&g.voxel.to_bits().to_le_bytes()).u64(g.levels().len() as u64);
        for l in g.levels() {
            head.json(&l.region).bytes(&l.band.to_bits().to_le_bytes()).bytes(&l.grid.voxel.to_bits().to_le_bytes());
        }
        head.bytes(&sum.to_le_bytes());
        head.finish()
    }

    fn current(clay: &Clay) -> Option<&Self> {
        clay.detail.as_ref()
    }

    fn put(clay: &mut Clay, grid: Option<Self>) {
        clay.detail = grid;
    }

    fn context(clay: &Clay) -> Hash128 {
        // The other sculpt layers the stroke reads (and a layer's own
        // amount); clays without layers hash as before.
        crate::layers::base_context(clay, clay_context(clay))
    }

    fn apply(clay: &Clay, grid: Option<&Self>, stroke: &Resolved) -> Result<Applied<Self>, String> {
        if stroke.params.brush == Brush::Paint {
            return Ok(Applied { grid: grid.cloned(), radius: 0.0 });
        }
        let input = SculptInput::of(clay, grid)?;
        let diff = input.run(clay, input.grid.as_ref(), stroke, &stroke.dabs)?;
        input.finish(grid, diff, stroke)
    }

    fn blend(before: Option<&Self>, after: Option<&Self>, t: f32) -> Result<Option<Self>, String> {
        match after {
            Some(a) => Detail::blend(before, a, t).map(Some),
            None if t == 1.0 => Ok(None),
            None => Ok(before.cloned()),
        }
    }

    fn delta(before: Option<&Self>, after: &Self) -> Option<Self> {
        Detail::delta(before, after)
    }

    fn add(base: Option<&Self>, delta: &Self) -> Result<Option<Self>, String> {
        match base {
            Some(b) => Detail::add(b, delta).map(Some),
            None => Ok(Some(delta.clone())),
        }
    }

    fn stats(clay: &Clay, before: Option<&Self>, after: Option<&Self>, measure: bool) -> DiffStats {
        let mut t = Tally::default();
        let bricks = detail_changes(before, after, |p, _, u, v| t.add(p, (v - u).abs()));
        let mut s = t.stats(bricks);
        if measure {
            s.volume_delta = Some(volume_delta(clay, before, after));
        }
        s
    }

    fn cell(grid: Option<&Self>) -> Option<f32> {
        grid.map(Detail::voxel)
    }

    fn quantized(&self) -> Self {
        Detail::quantized(self)
    }

    fn relayout(grid: Option<&Self>, regions: &[Region], cell: f32) -> Result<Option<Self>, String> {
        let mut d = match grid {
            Some(g) if g.voxel() != cell => return Err(format!("the detail cell is {} now, not {cell} as when the regions were laid out", g.voxel())),
            Some(g) => g.clone(),
            None if regions.is_empty() => return Ok(None),
            None => Detail::new(cell),
        };
        // Copies the brick map (its bricks stay shared), as any edit does.
        d.grid_mut().set_regions(regions)?;
        Ok(Some(d))
    }

    fn mirror(&self) -> Self {
        self.mirrored_x()
    }

    fn ledger(stack: &StackData) -> &Ledger<Self> {
        &stack.base
    }

    fn ledger_mut(stack: &mut StackData) -> &mut Ledger<Self> {
        &mut stack.base
    }

    fn cache(cache: &ReplayCache) -> &Mutex<Checkpoints<Self>> {
        &cache.detail
    }
}

impl Grid for Paint {
    const LAYER: &'static str = "paint";
    type Action = NoAction;

    fn sum(grid: Option<&Self>) -> u128 {
        let Some(p) = grid else { return 0 };
        p.grid().bricks().fold(0u128, |s, (key, values)| {
            let mut buf = Vec::with_capacity(12 + values.len() * 4);
            key.iter().for_each(|c| buf.extend(c.to_le_bytes()));
            values.iter().for_each(|v| buf.extend(v));
            s.wrapping_add(xxh3_128(&buf))
        })
    }

    fn sum_change(_: Option<&Self>, _: Option<&Self>) -> Option<u128> {
        // Paint bricks are not shared between grids: hash them all.
        None
    }

    fn seal(grid: Option<&Self>, sum: u128) -> Hash128 {
        let Some(p) = grid else { return Hasher::new("paint:none").finish() };
        Hasher::new("paint").bytes(&p.grid().voxel.to_bits().to_le_bytes()).bytes(&sum.to_le_bytes()).finish()
    }

    fn current(clay: &Clay) -> Option<&Self> {
        clay.paint.as_ref()
    }

    fn put(clay: &mut Clay, grid: Option<Self>) {
        clay.paint = grid;
    }

    fn context(clay: &Clay) -> Hash128 {
        // Paint reads the sculpted surface too, and the decals (their
        // depth moves it); files without decals hash as before.
        if let Some(v) = crate::layers::view_of(clay) {
            // A view: the clay's paint context, worked out when the view
            // was made (a paint layer's own, or the paint a sculpt view keeps).
            return v.paint_ctx;
        }
        let h = Hasher::new("paint context").hash(clay_context(clay)).hash(surface_hash(clay)).finish();
        if clay.decals.is_empty() { h } else { Hasher::new("decals").hash(h).json(&clay.decals).finish() }
    }

    fn apply(clay: &Clay, grid: Option<&Self>, stroke: &Resolved) -> Result<Applied<Self>, String> {
        let Some(params) = &stroke.paint else { return Ok(Applied { grid: grid.cloned(), radius: 0.0 }) };
        let gate = crate::sculpt::engine::Gate::of(clay, stroke)?;
        let alpha = crate::sculpt::engine::alpha_of(clay, &stroke.params)?;
        // Paint lies on the composed sculpt (every sculpt layer).
        let mut probe = Clay { paint: grid.cloned(), detail: crate::layers::surface_detail(clay), sculpt: None, ..clay.clone() };
        let dabs = stroke.paint_dabs.as_deref().unwrap_or(&stroke.dabs);
        // MRGB color follows its own dabs, without the sculpt alpha.
        let alpha = alpha.as_ref().filter(|_| stroke.paint_dabs.is_none() && stroke.params.brush == Brush::Paint).map(|a| (a, stroke.params.alpha.as_ref().expect("has an alpha")));
        crate::paint::paint_dabs(&mut probe, params, stroke.mask.as_ref(), dabs, &gate, alpha)?;
        Ok(Applied { grid: probe.paint, radius: dabs.first().map_or(0.0, |d| d.radius) })
    }

    fn blend(_: Option<&Self>, _: Option<&Self>, t: f32) -> Result<Option<Self>, String> {
        Err(format!("paint strokes cannot be scaled (factor {t}); disable or delete them instead"))
    }

    fn delta(_: Option<&Self>, _: &Self) -> Option<Self> {
        None
    }

    fn add(_: Option<&Self>, _: &Self) -> Result<Option<Self>, String> {
        Err("paint keeps outside edits whole".into())
    }

    fn stats(_: &Clay, before: Option<&Self>, after: Option<&Self>, _: bool) -> DiffStats {
        let mut t = Tally::default();
        let mut bricks = 0;
        let voxel = after.or(before).map_or(1.0, |p| p.grid().voxel);
        let (b, a) = (before.map(|p| p.grid()), after.map(|p| p.grid()));
        let keys: std::collections::BTreeSet<[i32; 3]> = a.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k)).chain(b.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k))).collect();
        for key in keys {
            let (x, y) = (b.and_then(|g| g.brick(&key)), a.and_then(|g| g.brick(&key)));
            let mut any = false;
            for i in 0..crate::clay::BRICK_VALUES {
                let (u, v) = (x.map_or([0; 4], |x| x[i]), y.map_or([0; 4], |y| y[i]));
                if u != v {
                    any = true;
                    let l = [i as i32 % 8, i as i32 / 8 % 8, i as i32 / 64];
                    let p = Vec3::new((key[0] * 8 + l[0]) as f32, (key[1] * 8 + l[1]) as f32, (key[2] * 8 + l[2]) as f32) * voxel;
                    t.add(p, 0.0);
                }
            }
            bricks += any as usize;
        }
        t.stats(bricks)
    }

    fn cell(grid: Option<&Self>) -> Option<f32> {
        grid.map(|p| p.grid().voxel)
    }

    fn mirror(&self) -> Self {
        self.mirrored_x()
    }

    fn ledger(stack: &StackData) -> &Ledger<Self> {
        &stack.paint
    }

    fn ledger_mut(stack: &mut StackData) -> &mut Ledger<Self> {
        &mut stack.paint
    }

    fn cache(cache: &ReplayCache) -> &Mutex<Checkpoints<Self>> {
        &cache.paint
    }
}

/// One record applied to the grid before it.
fn step<G: Grid>(clay: &Clay, r: &Record<G>, grid: Option<G>) -> Result<Option<G>, String> {
    if !r.enabled {
        return Ok(grid);
    }
    match &r.op {
        RecordOp::Stroke { stroke, .. } => {
            let after = G::apply(clay, grid.as_ref(), stroke)?.grid;
            if r.factor == 1.0 { Ok(after) } else { G::blend(grid.as_ref(), after.as_ref(), r.factor) }
        }
        RecordOp::Opaque { delta: true, grid: d, .. } => match d {
            Some(d) => G::add(grid.as_ref(), d),
            None => Ok(grid),
        },
        RecordOp::Opaque { grid: g, .. } => Ok(g.clone()),
        RecordOp::Action { action, .. } => G::act(clay, grid.as_ref(), action),
        RecordOp::Regions { regions, cell } => G::relayout(grid.as_ref(), regions, *cell),
    }
}

/// The grid after the first `upto` records, replayed in the clay's current
/// context from the nearest checkpoint. `each` sees every record replayed
/// (its index and the grid after it).
pub fn replay<G: Grid>(l: &Ledger<G>, clay: &Clay, upto: usize, cache: Option<&ReplayCache>, each: impl FnMut(usize, Option<&G>)) -> Result<Option<G>, String> {
    replay_until(l, clay, upto, cache, each, &|| false)
}

/// `replay`, giving up (Err) when `stop` says so between records.
fn replay_until<G: Grid>(l: &Ledger<G>, clay: &Clay, upto: usize, cache: Option<&ReplayCache>, mut each: impl FnMut(usize, Option<&G>), stop: &dyn Fn() -> bool) -> Result<Option<G>, String> {
    let d = l.data();
    let upto = upto.min(d.records.len());
    let ctx = G::context(clay);
    let mut start = (0, None);
    if let Some(c) = cache {
        let mut map = G::cache(c).lock().unwrap_or_else(|e| e.into_inner());
        let mut k = upto - upto % CHECKPOINT_EVERY;
        while k > 0 {
            if let Some(g) = map.get((d.chains[k - 1], ctx)) {
                start = (k, g);
                break;
            }
            k -= CHECKPOINT_EVERY;
        }
    }
    let (from, mut grid) = start;
    for i in from..upto {
        if stop() {
            return Err("stopped".into());
        }
        let r = &d.records[i];
        grid = step(clay, r, grid).map_err(|e| format!("record {} (seq {}): {e}", r.id, i + 1))?;
        each(i, grid.as_ref());
        if (i + 1) % CHECKPOINT_EVERY == 0
            && let Some(c) = cache
        {
            G::cache(c).lock().unwrap_or_else(|e| e.into_inner()).put((d.chains[i], ctx), grid.clone());
        }
    }
    Ok(grid)
}

/// A ledger whose records end at the clay's grid: when the grid was
/// changed outside it, an Opaque record of the change first. Returns the
/// ledger, the grid to build on and the record added.
pub fn sync<G: Grid>(l: &Ledger<G>, clay: &Clay, cache: Option<&ReplayCache>) -> Result<(Ledger<G>, Option<G>, Option<Arc<Record<G>>>), String> {
    sync_capped(l, clay, cache, true)
}

/// `sync`; without `cap` its record may take the ledger past
/// `MAX_OPAQUE_GRIDS` (the caller checks the result).
fn sync_capped<G: Grid>(l: &Ledger<G>, clay: &Clay, cache: Option<&ReplayCache>, cap: bool) -> Result<(Ledger<G>, Option<G>, Option<Arc<Record<G>>>), String> {
    let live = G::current(clay);
    let mut l = l.clone();
    if l.0.tip.is_none() {
        // Not known yet (a ledger read from a file and not checked on
        // load): in sync when the grid hashes to where the records end.
        let sum = G::sum(live);
        if G::seal(live, sum) == l.result() {
            let mut data = l.data().clone();
            (data.tip, data.tip_sum) = (Some(live.cloned()), Some(sum));
            return Ok((Ledger(Arc::new(data)), live.cloned(), None));
        }
        // Otherwise the records are replayed before anything builds on
        // them, so what follows never depends on whether the check on
        // load finished: the file's rounding of the replayed grid gives
        // way to the exact one, and an outside edit is recorded from it.
        if !l.is_empty() {
            let mut data = l.data().clone();
            match replay(&l, clay, l.len(), cache, |_, _| {}) {
                Ok(g) if G::hash(g.as_ref()) == l.result() => {
                    data.verified = Some(true);
                    // (by content: grids compare by identity)
                    let unchanged = G::hash(g.as_ref().map(G::quantized).as_ref()) == G::hash(live);
                    (data.tip, data.tip_sum) = (Some(g.clone()), Some(G::sum(g.as_ref())));
                    if unchanged {
                        return Ok((Ledger(Arc::new(data)), g, None));
                    }
                }
                // The records do not give the grid (as a mismatch on load):
                // the clay's own grid is kept and built on. (Tools settle
                // ledgers before they change the clay: `settle_clay`.)
                _ => {
                    data.verified = Some(false);
                    (data.tip, data.tip_sum) = (Some(live.cloned()), Some(sum));
                    return Ok((Ledger(Arc::new(data)), live.cloned(), None));
                }
            }
            l = Ledger(Arc::new(data));
        }
    } else if l.in_sync(live) {
        return Ok((l, live.cloned(), None));
    }
    let d = l.data();
    let name = crate::layers::record_layer::<G>(clay);
    let why = if d.records.is_empty() { format!("{name} grid made before the ledger") } else { format!("{name} grid edited outside the ledger") };
    let tip = d.tip.clone().flatten();
    // Opaque grids are kept as a file reads them back (`Grid::quantized`),
    // and the grid built on moves to match (within the file's rounding).
    let (op, grid) = match (&d.tip, live) {
        (Some(Some(t)), Some(now)) => match G::delta(Some(t), now) {
            Some(change) => {
                let change = change.quantized();
                let grid = G::add(Some(t), &change)?;
                (RecordOp::Opaque { why, delta: true, grid: Some(change) }, grid)
            }
            None => {
                let now = now.quantized();
                (RecordOp::Opaque { why, delta: false, grid: Some(now.clone()) }, Some(now))
            }
        },
        _ => {
            let now = live.map(G::quantized);
            (RecordOp::Opaque { why, delta: false, grid: now.clone() }, now)
        }
    };
    let stats = G::stats(clay, tip.as_ref(), grid.as_ref(), false);
    let sum = G::sum(grid.as_ref());
    let record = Record { id: 0, source: Source::Import, tool: None, label: None, enabled: true, factor: 1.0, layer: crate::layers::record_layer::<G>(clay), cell: G::cell(grid.as_ref()), op, context_hash: G::context(clay), result_hash: G::seal(grid.as_ref(), sum), stats, engine: ENGINE_VERSION, time: now_secs(), pressure_source: None, digest: Digest::default() };
    let mut data = d.clone();
    let r = if cap { data.push(record)? } else { data.push_any(record)? };
    (data.tip, data.tip_sum) = (Some(grid.clone()), Some(sum));
    Ok((Ledger(Arc::new(data)), grid, Some(r)))
}

/// A stroke committed to a ledger.
pub struct Committed<G: Grid> {
    pub ledger: Ledger<G>,
    pub grid: Option<G>,
    /// The grid it was applied to (after any outside edit was recorded).
    pub before: Option<G>,
    pub record: Arc<Record<G>>,
    /// The record of an outside edit made first, if any.
    pub synced: Option<Arc<Record<G>>>,
    pub radius: f32,
}

/// Applies a resolved stroke to the clay's grid of kind G and records it:
/// `ledger::commit`, shared by every tool that strokes (agent and UI).
/// The clay is not changed; `install` the result.
pub fn commit<G: Grid>(clay: &Clay, l: &Ledger<G>, meta: &Meta, spec: &StrokeSpec, stroke: &Resolved, measure: bool, cache: Option<&ReplayCache>) -> Result<Committed<G>, String> {
    let (l, before, synced) = sync(l, clay, cache)?;
    if l.len() >= MAX_RECORDS {
        return Err(format!("the {} layer's ledger is full ({MAX_RECORDS} records): squash old records into one (stroke_edit {{action: squash, ids}}) first", G::LAYER));
    }
    check_points(clay, &l, stroke_points(stroke))?;
    let applied = G::apply(clay, before.as_ref(), stroke)?;
    let stats = G::stats(clay, before.as_ref(), applied.grid.as_ref(), measure);
    let ctx = G::context(clay);
    let before_sum = l.0.tip_sum.unwrap_or_else(|| G::sum(before.as_ref()));
    let sum = G::sum_change(before.as_ref(), applied.grid.as_ref()).map_or_else(|| G::sum(applied.grid.as_ref()), |d| before_sum.wrapping_add(d));
    let record = Record {
        id: 0,
        source: meta.source,
        tool: meta.tool.clone(),
        label: meta.label.clone(),
        enabled: true,
        factor: 1.0,
        layer: crate::layers::record_layer::<G>(clay),
        cell: G::cell(applied.grid.as_ref()),
        op: RecordOp::Stroke { spec: Box::new(spec.clone()), stroke: Box::new(stroke.clone()) },
        context_hash: alpha_context(ctx, Some(stroke), clay),
        result_hash: G::seal(applied.grid.as_ref(), sum),
        stats,
        engine: ENGINE_VERSION,
        time: now_secs(),
        pressure_source: meta.pressure_source,
        digest: Digest::default(),
    };
    let mut data = l.data().clone();
    let r = data.push(record)?;
    (data.tip, data.tip_sum) = (Some(applied.grid.clone()), Some(sum));
    if data.records.len() % CHECKPOINT_EVERY == 0
        && let Some(c) = cache
    {
        G::cache(c).lock().unwrap_or_else(|e| e.into_inner()).put((*data.chains.last().expect("pushed"), ctx), applied.grid.clone());
    }
    Ok(Committed { ledger: Ledger(Arc::new(data)), grid: applied.grid, before, record: r, synced, radius: applied.radius })
}

/// Applies an action (mask or group) to the clay's grid of kind G and
/// records it, as `commit` does a stroke; `spec` is the action as the
/// tool was given it.
pub fn commit_action<G: Grid>(clay: &Clay, l: &Ledger<G>, meta: &Meta, spec: Value, action: G::Action, cache: Option<&ReplayCache>) -> Result<Committed<G>, String> {
    let (l, before, synced) = sync(l, clay, cache)?;
    if l.len() >= MAX_RECORDS {
        return Err(format!("the {} layer's ledger is full ({MAX_RECORDS} records): squash old records into one (stroke_edit {{action: squash, ids}}) first", G::LAYER));
    }
    let after = G::act(clay, before.as_ref(), &action)?;
    let stats = G::stats(clay, before.as_ref(), after.as_ref(), false);
    let ctx = G::context(clay);
    let sum = G::sum(after.as_ref());
    let record = Record { id: 0, source: meta.source, tool: meta.tool.clone(), label: meta.label.clone(), enabled: true, factor: 1.0, layer: crate::layers::record_layer::<G>(clay), cell: G::cell(after.as_ref()), op: RecordOp::Action { spec: Box::new(spec), action: Box::new(action) }, context_hash: ctx, result_hash: G::seal(after.as_ref(), sum), stats, engine: ENGINE_VERSION, time: now_secs(), pressure_source: None, digest: Digest::default() };
    let mut data = l.data().clone();
    let r = data.push(record)?;
    (data.tip, data.tip_sum) = (Some(after.clone()), Some(sum));
    if data.records.len() % CHECKPOINT_EVERY == 0
        && let Some(c) = cache
    {
        G::cache(c).lock().unwrap_or_else(|e| e.into_inner()).put((*data.chains.last().expect("pushed"), ctx), after.clone());
    }
    Ok(Committed { ledger: Ledger(Arc::new(data)), grid: after, before, record: r, synced, radius: 0.0 })
}

/// Records a grid change that has no replayable form (a mask copied from
/// a smart-material channel) as an Opaque record of the whole grid.
pub fn commit_grid<G: Grid>(clay: &Clay, l: &Ledger<G>, meta: &Meta, why: &str, grid: Option<G>, cache: Option<&ReplayCache>) -> Result<Committed<G>, String> {
    let (l, before, synced) = sync(l, clay, cache)?;
    let grid = grid.map(|g| g.quantized());
    let stats = G::stats(clay, before.as_ref(), grid.as_ref(), false);
    let sum = G::sum(grid.as_ref());
    let record = Record { id: 0, source: meta.source, tool: meta.tool.clone(), label: meta.label.clone(), enabled: true, factor: 1.0, layer: crate::layers::record_layer::<G>(clay), cell: G::cell(grid.as_ref()), op: RecordOp::Opaque { why: why.into(), delta: false, grid: grid.clone() }, context_hash: G::context(clay), result_hash: G::seal(grid.as_ref(), sum), stats, engine: ENGINE_VERSION, time: now_secs(), pressure_source: None, digest: Digest::default() };
    let mut data = l.data().clone();
    let r = data.push(record)?;
    (data.tip, data.tip_sum) = (Some(grid.clone()), Some(sum));
    Ok(Committed { ledger: Ledger(Arc::new(data)), grid, before, record: r, synced, radius: 0.0 })
}

/// Lays the clay's grid of kind G out with `regions` (see `Grid::relayout`)
/// and records it as a `Regions` record, which replays from the grid
/// before it: the grid this returns is the replay's own, so a replay
/// gives it bit for bit.
pub fn commit_relayout<G: Grid>(clay: &Clay, l: &Ledger<G>, meta: &Meta, regions: &[Region], cell: f32, cache: Option<&ReplayCache>) -> Result<Committed<G>, String> {
    if !regions_sane(regions, cell) {
        return Err(format!("at most {MAX_REGIONS} detail regions, each with a finite center, radii and cell > 0"));
    }
    let (l, before, synced) = sync(l, clay, cache)?;
    if l.len() >= MAX_RECORDS {
        return Err(format!("the {} layer's ledger is full ({MAX_RECORDS} records): squash old records into one (stroke_edit {{action: squash, ids}}) first", G::LAYER));
    }
    let after = G::relayout(before.as_ref(), regions, cell)?;
    let stats = G::stats(clay, before.as_ref(), after.as_ref(), false);
    let ctx = G::context(clay);
    let sum = G::sum(after.as_ref());
    let op = RecordOp::Regions { regions: regions.to_vec(), cell };
    let record = Record { id: 0, source: meta.source, tool: meta.tool.clone(), label: meta.label.clone(), enabled: true, factor: 1.0, layer: crate::layers::record_layer::<G>(clay), cell: G::cell(after.as_ref()), op, context_hash: ctx, result_hash: G::seal(after.as_ref(), sum), stats, engine: ENGINE_VERSION, time: now_secs(), pressure_source: None, digest: Digest::default() };
    let mut data = l.data().clone();
    let r = data.push(record)?;
    (data.tip, data.tip_sum) = (Some(after.clone()), Some(sum));
    if data.records.len() % CHECKPOINT_EVERY == 0
        && let Some(c) = cache
    {
        G::cache(c).lock().unwrap_or_else(|e| e.into_inner()).put((*data.chains.last().expect("pushed"), ctx), after.clone());
    }
    Ok(Committed { ledger: Ledger(Arc::new(data)), grid: after, before, record: r, synced, radius: 0.0 })
}

/// Dab points in a resolved stroke (sculpt and color dabs).
fn stroke_points(s: &Resolved) -> usize {
    s.dabs.iter().chain(s.paint_dabs.iter().flatten()).map(|d| d.points.len()).sum()
}

/// Refuses `adding` dab points when the clay's ledgers (with `l` in place
/// of its own of kind G) would pass `MAX_POINTS`.
fn check_points<G: Grid>(clay: &Clay, l: &Ledger<G>, adding: usize) -> Result<(), String> {
    let all = clay.sculpt.as_ref().map_or(0, |s| s.0.points());
    let total = all.saturating_sub(ledger::<G>(clay).0.points) + l.0.points + adding;
    if total > MAX_POINTS {
        return Err(format!("the clay's stroke ledgers would hold {total} dab points (at most {MAX_POINTS}, over its layers and morphs): squash old records into one (stroke_edit {{action: squash, ids}}), delete morphs you no longer need, or use fewer or wider-spaced dabs"));
    }
    Ok(())
}

/// Refuses a clay whose ledgers hold more than `MAX_POINTS` dab points
/// in all (after a call swaps whole ledgers in: `morph switch`), which a
/// file would read back without its ledgers.
pub fn check_clay_points(clay: &Clay) -> Result<(), String> {
    let total = clay.sculpt.as_ref().map_or(0, |s| s.0.points());
    if total > MAX_POINTS {
        return Err(format!("the clay's stroke ledgers would hold {total} dab points (at most {MAX_POINTS}, over its layers): squash old records into one (stroke_edit {{action: squash, ids}}) on the layers, or delete the morph and store it again"));
    }
    Ok(())
}

/// Records any outside edit of the clay's grids (see `sync`), so the
/// ledgers end at what the clay shows. Clays without ledgers are left
/// alone (their first stroke records the grid). Returns whether a record
/// was added.
pub fn sync_clay(clay: &mut Clay) -> Result<bool, String> {
    if clay.sculpt.is_none() {
        return Ok(false);
    }
    fn one<G: Grid>(clay: &mut Clay) -> Result<bool, String> {
        let l = ledger::<G>(clay);
        if l.is_empty() && G::current(clay).is_none() {
            return Ok(false);
        }
        let (l, grid, added) = sync(&l, clay, None)?;
        let changed = added.is_some();
        if changed || l != ledger::<G>(clay) {
            install(clay, l, grid);
        }
        Ok(changed)
    }
    let mut changed = one::<Detail>(clay)? | one::<Paint>(clay)?;
    // Each extra layer through its view.
    crate::layers::each_layer(clay, &mut |v, kind| {
        changed |= match kind {
            crate::layers::Kind::Sculpt => one::<Detail>(v)?,
            crate::layers::Kind::Paint => one::<Paint>(v)?,
        };
        Ok(())
    })?;
    Ok(changed | one::<SculptMask>(clay)? | one::<Groups>(clay)?)
}

/// Settles the clay's unchecked ledgers (read from a file and not
/// checked on load: `after_load_by`) as the check would have: replays
/// them against the clay as it is. Every tool that changes a clay calls
/// it first, so what follows never depends on whether the check on load
/// finished: the replay runs in the records' own context (before any
/// blob, cell or grid change), and paint builds on the exact sculpted
/// surface. Base first: paint reads it.
pub fn settle_clay(clay: &mut Clay, cache: Option<&ReplayCache>) -> Result<(), String> {
    if clay.sculpt.is_none() {
        return Ok(());
    }
    fn one<G: Grid>(clay: &mut Clay, cache: Option<&ReplayCache>) -> Result<(), String> {
        let l = ledger::<G>(clay);
        if l.0.tip.is_some() || l.is_empty() {
            return Ok(());
        }
        let (l, grid, _) = sync(&l, clay, cache)?;
        install(clay, l, grid);
        Ok(())
    }
    one::<Detail>(clay, cache)?;
    one::<Paint>(clay, cache)?;
    // Extra layers before mask and groups: those read the composed surface.
    crate::layers::each_layer(clay, &mut |v, kind| match kind {
        crate::layers::Kind::Sculpt => one::<Detail>(v, cache),
        crate::layers::Kind::Paint => one::<Paint>(v, cache),
    })?;
    one::<SculptMask>(clay, cache)?;
    one::<Groups>(clay, cache)
}

/// After a job settled a copy of `before` (`settle_clay`): puts the
/// settled ledgers and grids into `cur` where it still holds `before`'s
/// unchecked ones (layers the job installed itself are left alone).
pub fn adopt_settled(cur: &mut Clay, before: &Clay, settled: &Clay) {
    fn one<G: Grid>(cur: &mut Clay, before: &Clay, settled: &Clay) {
        let (l, s) = (ledger::<G>(cur), ledger::<G>(settled));
        if l.0.tip.is_none() && !l.is_empty() && l == ledger::<G>(before) && G::current(cur) == G::current(before) && s.0.tip.is_some() && s.head() == l.head() {
            install(cur, s, G::current(settled).cloned());
        }
    }
    one::<Detail>(cur, before, settled);
    one::<Paint>(cur, before, settled);
    one::<SculptMask>(cur, before, settled);
    one::<Groups>(cur, before, settled);
    // Extra layers, by name.
    for l in crate::layers::extra(settled).to_vec() {
        let (Some(c), Some(b)) = (crate::layers::layer(cur, &l.name), crate::layers::layer(before, &l.name)) else { continue };
        if l.kind != c.kind {
            continue;
        }
        let same = |x: &crate::layers::Layer| x.strokes == b.strokes && x.detail == b.detail && x.paint_strokes == b.paint_strokes && x.paint == b.paint;
        let unchecked = (!b.strokes.is_empty() && !b.strokes.tip_known()) || (!b.paint_strokes.is_empty() && !b.paint_strokes.tip_known());
        let settled_ok = l.strokes.head() == b.strokes.head() && l.paint_strokes.head() == b.paint_strokes.head() && (l.strokes.is_empty() || l.strokes.tip_known()) && (l.paint_strokes.is_empty() || l.paint_strokes.tip_known());
        if same(c) && unchecked && settled_ok {
            match l.kind {
                crate::layers::Kind::Sculpt => crate::layers::install::<Detail>(cur, &l.name, l.strokes.clone(), l.detail.clone()),
                crate::layers::Kind::Paint => crate::layers::install::<Paint>(cur, &l.name, l.paint_strokes.clone(), l.paint.clone()),
            }
        }
    }
}

/// What `stroke_edit` does.
#[derive(Clone, Debug)]
pub enum Edit {
    Enable(Vec<u64>, bool),
    Delete(Vec<u64>),
    /// A record's stroke, re-resolved (it gets a new id).
    Replace(u64, Box<StrokeSpec>, Box<Resolved>),
    /// Records to before position `to` (1-based seq; past the end: last).
    Move(Vec<u64>, usize),
    Scale(Vec<u64>, f32),
    /// Every record again in the current context and kernel, taking the
    /// results as the records' own.
    Rebase,
    /// Every record again (from `from`, 0-based) to check them.
    Replay(usize),
    /// Records collapsed into one Opaque record.
    Squash(Vec<u64>),
}

/// A ledger after `edit`.
pub struct Edited<G: Grid> {
    pub ledger: Ledger<G>,
    pub grid: Option<G>,
    /// The first record (0-based) whose input changed.
    pub from: usize,
    /// Records applied again (from the checkpoint before `from`).
    pub replayed: usize,
    /// Records whose replay gave another grid than they had (ids).
    pub mismatched: Vec<u64>,
    pub stats: DiffStats,
}

/// Edits a ledger and replays it from the first changed record (from the
/// nearest checkpoint before it); records after it take their new results.
pub fn edit<G: Grid>(clay: &Clay, l: &Ledger<G>, edit: Edit, cache: Option<&ReplayCache>) -> Result<Edited<G>, String> {
    // An outside edit is recorded even at the grid cap; the cap is checked
    // on the edited list, so squash and delete can always get under it.
    let (l, old, _) = sync_capped(l, clay, cache, false)?;
    let mut data = l.data().clone();
    fn find<G: Grid>(data: &LedgerData<G>, id: u64) -> Result<usize, String> {
        data.records.iter().position(|r| r.id == id).ok_or_else(|| format!("no record {id} on layer {}", G::LAYER))
    }
    fn ids_of<G: Grid>(data: &LedgerData<G>, ids: &[u64]) -> Result<Vec<usize>, String> {
        if ids.is_empty() {
            return Err("pass ids: the records to edit (see stroke_log)".into());
        }
        let mut at: Vec<usize> = ids.iter().map(|id| find(data, *id)).collect::<Result<_, _>>()?;
        at.sort_unstable();
        at.dedup();
        Ok(at)
    }
    let modify = |data: &mut LedgerData<G>, i: usize, f: &dyn Fn(&mut Record<G>)| {
        let mut r = (*data.records[i]).clone();
        f(&mut r);
        data.records[i] = Arc::new(r);
    };
    let n = data.records.len();
    let (from, rebase, check) = match edit {
        Edit::Enable(ids, on) => {
            let at = ids_of(&data, &ids)?;
            for &i in &at {
                modify(&mut data, i, &|r| r.enabled = on);
            }
            (at[0], false, false)
        }
        Edit::Delete(ids) => {
            let at = ids_of(&data, &ids)?;
            for &i in at.iter().rev() {
                data.records.remove(i);
            }
            (at[0], false, false)
        }
        Edit::Replace(id, spec, stroke) => {
            let i = find(&data, id)?;
            if !matches!(data.records[i].op, RecordOp::Stroke { .. }) {
                return Err(format!("record {id} is not a stroke; only strokes can be replaced"));
            }
            let tmp = Ledger(Arc::new(LedgerData { points: data.points - data.records[i].points(), ..data.clone() }));
            check_points(clay, &tmp, stroke_points(&stroke))?;
            let next = data.next_id;
            data.next_id += 1;
            let context = alpha_context(G::context(clay), Some(&stroke), clay);
            modify(&mut data, i, &|r| {
                r.id = next;
                r.op = RecordOp::Stroke { spec: spec.clone(), stroke: stroke.clone() };
                r.context_hash = context;
                r.digest = Digest::default();
                r.time = now_secs();
            });
            (i, false, false)
        }
        Edit::Move(ids, to) => {
            let at = ids_of(&data, &ids)?;
            let to = to.max(1) - 1;
            let moved: Vec<_> = at.iter().map(|&i| data.records[i].clone()).collect();
            for &i in at.iter().rev() {
                data.records.remove(i);
            }
            let before = at.iter().filter(|&&i| i < to).count();
            let dest = (to - before.min(to)).min(data.records.len());
            for (k, r) in moved.into_iter().enumerate() {
                data.records.insert(dest + k, r);
            }
            (at[0].min(dest), false, false)
        }
        Edit::Scale(ids, factor) => {
            if !(0.0..=2.0).contains(&factor) {
                return Err("factor must be 0 to 2".into());
            }
            let at = ids_of(&data, &ids)?;
            if G::LAYER != "base" {
                return Err("only sculpt strokes can be scaled".into());
            }
            for &i in &at {
                if !matches!(data.records[i].op, RecordOp::Stroke { .. }) {
                    return Err(format!("record {} is not a stroke; only strokes scale", data.records[i].id));
                }
                modify(&mut data, i, &|r| r.factor = factor);
            }
            (at[0], false, false)
        }
        Edit::Rebase => {
            // Strokes whose alpha name was bound to other pixels since take
            // the image it names now.
            let names = clay.sculpt.as_ref().map(|s| s.0.alphas.names.clone()).unwrap_or_default();
            for i in 0..data.records.len() {
                let Some((_, s)) = data.records[i].stroke() else { continue };
                let Some(a) = &s.params.alpha else { continue };
                let Some(bound) = a.name.as_ref().and_then(|n| names.get(n)).copied().filter(|h| *h != a.hash) else { continue };
                modify(&mut data, i, &|r| {
                    if let RecordOp::Stroke { stroke, .. } = &mut r.op
                        && let Some(a) = &mut stroke.params.alpha
                    {
                        a.hash = bound;
                    }
                    r.digest = Digest::default();
                });
            }
            (0, true, false)
        }
        Edit::Replay(from) => (from.min(n), false, true),
        Edit::Squash(ids) => {
            let at = ids_of(&data, &ids)?;
            let (a, b) = (at[0], *at.last().expect("not empty"));
            data.rechain();
            let tmp = Ledger(Arc::new(data.clone()));
            let start = replay(&tmp, clay, a, cache, |_, _| {})?;
            let end = replay(&tmp, clay, b + 1, cache, |_, _| {})?;
            // Kept as a file reads it back, like `sync`'s (the replay below
            // takes the result).
            let (delta, grid) = match (&start, &end) {
                (_, None) => (false, None),
                (s, Some(e)) => match G::delta(s.as_ref(), e) {
                    Some(change) => (true, Some(change.quantized())),
                    None => (false, Some(e.quantized())),
                },
            };
            let ids: Vec<String> = data.records[a..=b].iter().map(|r| r.id.to_string()).collect();
            let next = data.next_id;
            data.next_id += 1;
            let stats = G::stats(clay, start.as_ref(), end.as_ref(), false);
            let record = Record { id: next, source: Source::Replay, tool: Some("stroke_edit".into()), label: Some(format!("squash of records {}", ids.join(", "))), enabled: true, factor: 1.0, layer: crate::layers::record_layer::<G>(clay), cell: G::cell(end.as_ref()), op: RecordOp::Opaque { why: "squashed".into(), delta, grid }, context_hash: G::context(clay), result_hash: G::hash(end.as_ref()), stats, engine: ENGINE_VERSION, time: now_secs(), pressure_source: None, digest: Digest::default() };
            data.records.splice(a..=b, [Arc::new(record)]);
            (a, false, false)
        }
    };
    data.check_grids()?;
    data.rechain();
    let mut mismatched = Vec::new();
    let mut replayed = 0;
    let ctx = G::context(clay);
    let tmp = Ledger(Arc::new(data.clone()));
    let mut results: Vec<(usize, Hash128)> = Vec::new();
    let total = data.records.len();
    // Records before the first change keep their inputs, so a checkpoint
    // may skip them; rebase and checks look at every record.
    let cache = if rebase || check { None } else { cache };
    // Hashes follow the replay brick sum by brick sum.
    let mut prev: Option<(Option<G>, u128)> = None;
    let grid = replay(&tmp, clay, total, cache, |i, g| {
        replayed += 1;
        let sum = match &prev {
            Some((p, s)) => G::sum_change(p.as_ref(), g).map_or_else(|| G::sum(g), |d| s.wrapping_add(d)),
            None => G::sum(g),
        };
        if i >= from {
            results.push((i, G::seal(g, sum)));
        }
        prev = Some((g.cloned(), sum));
    })?;
    for (i, h) in results {
        let r = &data.records[i];
        if (check || rebase) && h != r.result_hash {
            mismatched.push(r.id);
        }
        if !check && (h != r.result_hash || rebase) {
            let (h, ctx) = (h, if rebase { r.context_in(ctx, clay) } else { r.context_hash });
            modify(&mut data, i, &|r| {
                r.result_hash = h;
                r.context_hash = ctx;
                if rebase {
                    r.engine = ENGINE_VERSION;
                }
            });
        }
    }
    data.rechain();
    data.tip_sum = prev.as_ref().filter(|(g, _)| *g == grid).map(|(_, s)| *s);
    data.tip = Some(grid.clone());
    if rebase || check {
        data.verified = Some(rebase || mismatched.is_empty());
    }
    let stats = G::stats(clay, old.as_ref(), grid.as_ref(), false);
    Ok(Edited { ledger: Ledger(Arc::new(data)), grid, from, replayed, mismatched, stats })
}

/// After a file loads: replays each ledger to check it against the saved
/// grid. A match puts the replayed grid in place (exactly what was saved
/// before the file rounded it); a mismatch or an engine-stale record
/// keeps the saved grid and is reported (`get_clay`, `stroke_log`).
#[cfg(test)]
pub fn after_load(clay: &mut Clay) {
    after_load_by(clay, std::time::Instant::now() + VERIFY_BUDGET);
}

/// `after_load`, checking until `deadline` (shared by a file's clays so
/// opening it never stalls the editor for long): a ledger whose replay
/// it cuts short stays unchecked (`replay_verified: null`) with its saved
/// grid, as do ledgers past `VERIFY_DABS`; their next stroke or edit
/// replays them first (`sync`).
pub fn after_load_by(clay: &mut Clay, deadline: std::time::Instant) {
    if clay.sculpt.is_none() {
        return;
    }
    fn one<G: Grid>(clay: &mut Clay, deadline: std::time::Instant) {
        let l = ledger::<G>(clay);
        if l.is_empty() {
            return;
        }
        let dabs: usize = l.records().iter().map(|r| r.stroke().map_or(0, |(_, s)| s.dabs.len())).sum();
        let stale = l.records().iter().any(|r| r.engine_stale());
        let saved = G::current(clay).cloned();
        let mut data = l.data().clone();
        // An unchecked ledger leaves the tip unknown: the saved grid is
        // the file's rounding of the records' result, so `sync` replays
        // them before a stroke builds on it.
        (data.tip, data.tip_sum) = (None, None);
        let mut grid = saved.clone();
        let late = || std::time::Instant::now() >= deadline;
        if !stale && dabs <= VERIFY_DABS && !late() {
            match replay_until(&l, clay, l.len(), None, |_, _| {}, &late) {
                Ok(g) if G::hash(g.as_ref()) == l.result() => {
                    data.verified = Some(true);
                    data.tip = Some(g.clone());
                    grid = g;
                }
                // Out of time: left unchecked.
                Err(_) if late() => {}
                _ => (data.verified, data.tip) = (Some(false), Some(saved)),
            }
        } else if stale {
            (data.verified, data.tip) = (Some(false), Some(saved));
        }
        install(clay, Ledger(Arc::new(data)), grid);
    }
    one::<Detail>(clay, deadline);
    one::<Paint>(clay, deadline);
    // A view of each extra layer (one whose view cannot be made, as a
    // hostile file's, is left unchecked).
    let _ = crate::layers::each_layer(clay, &mut |v, kind| {
        match kind {
            crate::layers::Kind::Sculpt => one::<Detail>(v, deadline),
            crate::layers::Kind::Paint => one::<Paint>(v, deadline),
        }
        Ok(())
    });
    one::<SculptMask>(clay, deadline);
    one::<Groups>(clay, deadline);
}

/// What Adjust Last (`stroke_edit scale last`) needs: the newest stroke on
/// a clay's base layer, its grid before and after, tagged with the ledger
/// head right after it. Kept by the editor, outside the scene, so an undo
/// (or any other change of the ledger) makes it stale.
#[derive(Clone, Debug)]
pub struct LastDelta {
    /// The sculpt layer it was made on.
    pub layer: String,
    pub head: Hash128,
    pub record: u64,
    pub source: Source,
    pub before: Option<Detail>,
    pub after: Option<Detail>,
}

/// Scales the newest stroke to `factor` of what it did: the grid before
/// it + factor x (after - before). Returns the ledger (the record's
/// factor and result updated), the grid, the record and the new tag.
pub fn adjust_last(clay: &Clay, last: &LastDelta, factor: f32) -> Result<(Ledger<Detail>, Option<Detail>, Arc<Record<Detail>>, LastDelta), String> {
    if !factor.is_finite() || !(0.0..=2.0).contains(&factor) {
        return Err("factor must be 0 to 2".into());
    }
    let l = ledger::<Detail>(clay);
    if l.head() != last.head || !l.in_sync(clay.detail.as_ref()) {
        return Err("stale last: the base layer changed since that stroke (undo, redo, another stroke or an edit); scale a record by id instead: stroke_edit {action: scale, ids, factor}".into());
    }
    let i = l.position(last.record).filter(|i| i + 1 == l.len()).ok_or("stale last: that stroke is no longer the newest record")?;
    let grid = Detail::blend_opt(last.before.as_ref(), last.after.as_ref(), factor)?;
    let mut data = l.data().clone();
    let mut r = (*data.records[i]).clone();
    r.factor = factor;
    let sum = Detail::sum(grid.as_ref());
    r.result_hash = Detail::seal(grid.as_ref(), sum);
    r.stats = Detail::stats(clay, last.before.as_ref(), grid.as_ref(), false);
    let r = Arc::new(r);
    data.records[i] = r.clone();
    data.rechain();
    (data.tip, data.tip_sum) = (Some(grid.clone()), Some(sum));
    let l = Ledger(Arc::new(data));
    let tag = LastDelta { head: l.head(), ..last.clone() };
    Ok((l, grid, r, tag))
}

impl Detail {
    fn blend_opt(before: Option<&Detail>, after: Option<&Detail>, t: f32) -> Result<Option<Detail>, String> {
        <Detail as Grid>::blend(before, after, t)
    }
}

/// A record as `stroke_log` lists it; `full` adds the spec and the
/// resolved dabs.
pub fn record_json<G: Grid>(r: &Record<G>, seq: usize, stale: bool, full: bool) -> Value {
    let mut v = json!({
        "id": r.id,
        "seq": seq,
        "source": r.source,
        "tool": r.tool,
        "label": r.label,
        "layer": r.layer,
        "enabled": r.enabled,
        "stale_context": stale,
        "engine_stale": r.engine_stale(),
        "stats": r.stats,
        "result_hash": r.result_hash,
    });
    if r.factor != 1.0 {
        v["factor"] = json!(r.factor);
    }
    if let Some(c) = r.cell {
        v["cell"] = json!(c);
    }
    match &r.op {
        RecordOp::Stroke { spec, stroke } => {
            v["kind"] = json!("stroke");
            v["brush"] = json!(spec.brush);
            v["stroke_kind"] = serde_json::to_value(spec.kind).unwrap_or_default();
            v["radius"] = json!(spec.radius);
            v["strength"] = json!(stroke.params.strength);
            v["n_points"] = json!(spec.points.len());
            v["dabs"] = json!(stroke.dabs.len());
            let (lo, hi) = stroke.dabs.iter().flat_map(|d| d.points.iter().map(move |p| (*p - d.radius, *p + d.radius))).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), (l, h)| (a.min(l), b.max(h)));
            if lo.x <= hi.x {
                v["bbox"] = json!([lo.to_array(), hi.to_array()]);
            }
            if full {
                v["spec"] = serde_json::to_value(spec).unwrap_or_default();
                v["resolved"] = serde_json::to_value(stroke).unwrap_or_default();
            }
        }
        RecordOp::Opaque { why, delta, .. } => {
            v["kind"] = json!("opaque");
            v["why"] = json!(why);
            v["delta"] = json!(delta);
        }
        RecordOp::Action { spec, action } => {
            v["kind"] = json!("action");
            v["action"] = (**spec).clone();
            if full {
                v["resolved"] = serde_json::to_value(action).unwrap_or_default();
            }
        }
        RecordOp::Regions { regions, .. } => {
            v["kind"] = json!("regions");
            v["detail_regions"] = json!(regions.len());
            if full {
                v["regions"] = serde_json::to_value(regions).unwrap_or_default();
            }
        }
    }
    v
}

/// A layer's summary for `get_clay` and `stroke_log`.
pub fn layer_json<G: Grid>(clay: &Clay) -> Value {
    let l = ledger::<G>(clay);
    let grid = G::current(clay);
    let legacy = clay.sculpt.is_none() && grid.is_some();
    let ctx = G::context(clay);
    json!({
        "name": crate::layers::record_layer::<G>(clay),
        "kind": match G::LAYER { "base" => "sculpt", k => k },
        "amount": 1.0,
        "enabled": true,
        "records": l.len() + legacy as usize,
        "head": clay.sculpt.is_some().then(|| l.head()),
        "in_sync": legacy || l.in_sync(grid),
        "replay_verified": l.verified(),
        "stale_records": l.records().iter().filter(|r| r.stale(ctx, clay)).count(),
        "engine_stale_records": l.records().iter().filter(|r| r.engine_stale()).count(),
        "note": legacy.then_some("the grid predates the ledger: the next stroke records it as one opaque record"),
    })
}

mod kinds;
#[cfg(test)]
mod tests;
