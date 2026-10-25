//! Sculpt and paint layers, and morph targets (docs/SCULPT_PLAN.md 3.5,
//! Stage 4).
//!
//! A clay's layers, bottom first: "base" (its own sculpt grid,
//! `Clay::detail`, with the base ledger), "paint" (`Clay::paint`), then
//! up to `MAX_LAYERS` - 2 more (`StackData::layers`), each a sculpt or a
//! paint layer with an amount, a switch and a ledger of its own. What the
//! clay shows is the composite: base + sum(amount x grid) over the
//! enabled sculpt layers (`composed_detail`), and the paint layers laid
//! over "paint" by coverage x amount (`composed_paint`). Every field and
//! mesh build reads the composite (`Field::new`, the mesh builds' key),
//! so meshes, bakes, films and exports take layers up with no special
//! case. Base and paint keep amount 1.
//!
//! A stroke on a layer reads every enabled layer (WYSIWYG, decision Q1)
//! and writes its own: the engine runs on the composite and the change,
//! divided by the layer's amount, goes into the layer's grid, so the
//! surface moves as the brush says (strokes are refused below
//! `MIN_AMOUNT`). A stroke's context covers the other layers (amounts
//! and contents) and its own layer's amount, so changing another layer
//! marks its records stale_context. Paint layers keep paint as brushed.
//!
//! Records on an extra layer use the same ledger code as base and paint:
//! the layer is seen through a view (`view`): the clay with the layer's
//! grid and ledger in base's (or paint's) place, carrying what its
//! strokes read of the other layers (`View`).
//!
//! Morph targets (`Morph`) are snapshots of the sculpt layers (grids,
//! ledgers, amounts, switches), all shared, so storing one costs nothing
//! until the clay is sculpted away from it. `switch` swaps the clay's
//! sculpt layers with a morph's; `blend` moves them toward it (an outside
//! edit: recorded as opaque records).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clay::{Clay, Detail};
use crate::groups::Groups;
use crate::ledger::{self, Grid, Hash128, Ledger, StackData};
use crate::mask::SculptMask;
use crate::paint::Paint;

/// Most layers on a clay, base and paint included.
pub const MAX_LAYERS: usize = 16;
/// Most morph targets on a clay.
pub const MAX_MORPHS: usize = 32;
/// Ledgers of baked or merged layers kept for reading (oldest dropped).
pub const MAX_ARCHIVED: usize = 8;
/// Strokes are refused on a sculpt layer whose amount is nearer 0 than
/// this (they are stored divided by the amount).
pub const MIN_AMOUNT: f32 = 0.05;
/// Sculpt layer amounts are -2..2; paint layer amounts (opacity) 0..1.
pub const MAX_AMOUNT: f32 = 2.0;
/// Bytes of sculpt, paint, mask and group bricks a clay may hold over its
/// layers and morphs (bricks they share count once): today's
/// `MAX_TOTAL_BRICKS` f32 bricks.
pub const MAX_GRID_BYTES: u64 = crate::clay::MAX_TOTAL_BRICKS as u64 * F32_BRICK;
/// Bytes a clay with layers or morphs may inflate to in a file: every
/// grid it saves, each time it is saved (a file has no sharing: a morph
/// writes its grids and ledgers again), a quarter of the scene read's
/// `INFLATE_BUDGET`, so a clay that may be kept is one that loads.
pub const MAX_SAVED_BYTES: u64 = crate::clay::INFLATE_BUDGET / 4;
/// Longest layer or morph name.
pub const MAX_NAME: usize = 64;
pub const BASE: &str = "base";
pub const PAINT: &str = "paint";
const F32_BRICK: u64 = crate::clay::BRICK_VALUES as u64 * 4;

/// What a layer holds.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Sculpt,
    Paint,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

fn one() -> f32 {
    1.0
}

/// One layer above base and paint.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub kind: Kind,
    #[serde(default = "one")]
    pub amount: f32,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// A sculpt layer's offsets (what its strokes wrote divided by its
    /// amount) and their ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub strokes: Ledger<Detail>,
    /// A paint layer's paint and its ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint: Option<Paint>,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub paint_strokes: Ledger<Paint>,
}

impl Layer {
    pub fn new(name: &str, kind: Kind, amount: f32) -> Layer {
        Layer { name: name.into(), kind, amount, enabled: true, detail: None, strokes: Ledger::default(), paint: None, paint_strokes: Ledger::default() }
    }

    /// Whether it adds anything to the composite.
    pub fn contributes(&self) -> bool {
        self.enabled && self.amount != 0.0 && match self.kind {
            Kind::Sculpt => self.detail.is_some(),
            Kind::Paint => self.paint.is_some(),
        }
    }

    fn points(&self) -> usize {
        self.strokes.points() + self.paint_strokes.points()
    }

    fn is_sane(&self) -> bool {
        let amount_ok = self.amount.is_finite() && match self.kind {
            Kind::Sculpt => self.amount.abs() <= MAX_AMOUNT,
            Kind::Paint => (0.0..=1.0).contains(&self.amount),
        };
        let grids_ok = match self.kind {
            Kind::Sculpt => self.paint.is_none() && self.paint_strokes.is_empty() && self.detail.as_ref().is_none_or(Detail::sane),
            Kind::Paint => self.detail.is_none() && self.strokes.is_empty() && self.paint.as_ref().is_none_or(Paint::is_sane),
        };
        valid_name(&self.name).is_ok() && amount_ok && grids_ok && self.strokes.is_sane() && self.paint_strokes.is_sane()
    }
}

/// One sculpt layer in a morph target.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MorphLayer {
    pub name: String,
    pub amount: f32,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub strokes: Ledger<Detail>,
}

/// A morph target: the sculpt layers as they were (shared, not copied).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Morph {
    pub name: String,
    /// The blobs and settings it was stored on (`ledger::clay_context`).
    pub context: Hash128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Detail>,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub base_strokes: Ledger<Detail>,
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "capped_layers")]
    pub layers: Vec<MorphLayer>,
}

impl Morph {
    fn is_sane(&self) -> bool {
        valid_name(&self.name).is_ok()
            && self.layers.len() < MAX_LAYERS
            && self.base.as_ref().is_none_or(Detail::sane)
            && self.base_strokes.is_sane()
            && self.layers.iter().all(|l| valid_name(&l.name).is_ok() && l.amount.is_finite() && l.amount.abs() <= MAX_AMOUNT && l.detail.as_ref().is_none_or(Detail::sane) && l.strokes.is_sane())
    }

    /// Its composite (base + sum(amount x grid) over its enabled layers).
    pub fn composed(&self) -> Option<Detail> {
        let mut parts: Vec<(&Detail, f32)> = self.base.iter().map(|b| (b, 1.0)).collect();
        for l in self.layers.iter().filter(|l| l.enabled && l.amount != 0.0) {
            if let Some(d) = &l.detail
                && parts.first().is_none_or(|(f, _)| f.same_layout(d))
            {
                parts.push((d, l.amount));
            }
        }
        if parts.is_empty() { None } else { Detail::compose(&parts).ok() }
    }
}

/// A baked or merged layer's ledger, kept for reading (`stroke_log`
/// layer "archived:NAME").
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Archived {
    pub name: String,
    pub kind: Kind,
    /// The layer it was folded into.
    pub into: String,
    pub time: u64,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub strokes: Ledger<Detail>,
    #[serde(default, skip_serializing_if = "Ledger::is_empty")]
    pub paint_strokes: Ledger<Paint>,
}

/// Lists read from a file stop at `N` + 1 entries (the rest skipped
/// unread): past `N` the stack is not sane and `Clay::sanitize` drops it.
fn capped<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>, const N: usize>(d: D) -> Result<Vec<T>, D::Error> {
    struct Visit<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Visit<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a list of at most {N} entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(v) = seq.next_element::<T>()? {
                out.push(v);
                if out.len() > N {
                    while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                    break;
                }
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Visit::<T, N>(std::marker::PhantomData))
}

pub(crate) fn capped_layers<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    capped::<D, T, MAX_LAYERS>(d)
}

pub(crate) fn capped_morphs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Morph>, D::Error> {
    capped::<D, Morph, MAX_MORPHS>(d)
}

pub(crate) fn capped_archive<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Archived>, D::Error> {
    capped::<D, Archived, MAX_ARCHIVED>(d)
}

/// Layer and morph names: 1 to `MAX_NAME` characters, letters, digits,
/// spaces and `_-.`, not one of the fixed layers' names.
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > MAX_NAME || name.trim() != name {
        return Err(format!("names are 1 to {MAX_NAME} characters with no spaces at the ends"));
    }
    if !name.chars().all(|c| c.is_alphanumeric() || " _-.".contains(c)) {
        return Err("names take letters, digits, spaces and _ - . only".into());
    }
    Ok(())
}

/// The clay's own layers' names (and the archive's): never an extra layer's.
pub fn fixed(name: &str) -> bool {
    matches!(name, BASE | PAINT | "mask" | "groups") || name.starts_with("archived")
}

/// The stack's derived composites, kept beside it (never saved, never
/// compared): each with the parts it was made from, held so their
/// addresses cannot be reused while it is kept.
#[derive(Default, Debug)]
pub struct Cache {
    detail: Mutex<Option<(Vec<(Option<Detail>, u32)>, Option<Detail>)>>,
    extras: Mutex<Option<(Vec<(Option<Detail>, u32)>, Option<Detail>)>>,
    paint: Mutex<Option<(Vec<(Option<Paint>, u32)>, Option<Paint>)>>,
}

/// A copy keeps the entries (each is checked against the parts it was
/// made from on use), so a stack copied to change something else (a
/// paint stroke, a mask action, an undo snapshot's copy-on-write) keeps
/// its composite, and with it the mesh.
impl Clone for Cache {
    fn clone(&self) -> Self {
        fn copy<T: Clone>(m: &Mutex<T>) -> Mutex<T> {
            Mutex::new(m.lock().unwrap_or_else(|e| e.into_inner()).clone())
        }
        Cache { detail: copy(&self.detail), extras: copy(&self.extras), paint: copy(&self.paint) }
    }
}

/// What a stroke on an extra layer reads besides its own grid, carried by
/// the view of that layer (`view`).
#[derive(Debug)]
pub struct View {
    pub name: String,
    pub kind: Kind,
    pub amount: f32,
    /// Sculpt views: the other sculpt layers (base first) at their
    /// amounts, composed on first use (`others`).
    parts: Vec<(Detail, f32)>,
    others: OnceLock<Result<Option<Detail>, String>>,
    /// Paint views: the composed sculpted surface paint lies on.
    pub surface: Option<Detail>,
    /// The layer's context (`layer_context`).
    pub ctx: Hash128,
    /// The clay's own base and paint contexts, for the view's other
    /// ledgers (a paint view's base, a sculpt view's paint).
    pub base_ctx: Hash128,
    pub paint_ctx: Hash128,
    /// Dab points on the clay's other ledgers (the clay's point cap).
    pub points: usize,
}

impl View {
    /// The other sculpt layers composed (None: nothing else is sculpted).
    pub fn others(&self) -> Result<Option<Detail>, String> {
        self.others
            .get_or_init(|| {
                if self.parts.is_empty() {
                    return Ok(None);
                }
                let parts: Vec<(&Detail, f32)> = self.parts.iter().map(|(d, a)| (d, *a)).collect();
                Detail::compose(&parts).map(Some)
            })
            .clone()
    }
}

/// A grid kind that layers can hold (sculpt or paint), or one only the
/// clay itself has (mask, groups).
pub trait Layered: Grid {
    /// The kind of extra layer that holds this grid (None: none does).
    const KIND: Option<Kind>;
    fn slot(_l: &mut Layer) -> Option<(&mut Option<Self>, &mut Ledger<Self>)> {
        None
    }
}

impl Layered for Detail {
    const KIND: Option<Kind> = Some(Kind::Sculpt);
    fn slot(l: &mut Layer) -> Option<(&mut Option<Self>, &mut Ledger<Self>)> {
        (l.kind == Kind::Sculpt).then_some((&mut l.detail, &mut l.strokes))
    }
}

impl Layered for Paint {
    const KIND: Option<Kind> = Some(Kind::Paint);
    fn slot(l: &mut Layer) -> Option<(&mut Option<Self>, &mut Ledger<Self>)> {
        (l.kind == Kind::Paint).then_some((&mut l.paint, &mut l.paint_strokes))
    }
}

impl Layered for SculptMask {
    const KIND: Option<Kind> = None;
}

impl Layered for Groups {
    const KIND: Option<Kind> = None;
}

fn stack(clay: &Clay) -> Option<&StackData> {
    clay.sculpt.as_ref().map(|s| s.data())
}

/// The view a clay is, if it is one.
pub fn view_of(clay: &Clay) -> Option<&View> {
    stack(clay).and_then(|s| s.view.as_deref())
}

/// The extra layers (none on a view).
pub fn extra(clay: &Clay) -> &[Layer] {
    stack(clay).map_or(&[], |s| s.layers.as_slice())
}

pub fn layer<'a>(clay: &'a Clay, name: &str) -> Option<&'a Layer> {
    extra(clay).iter().find(|l| l.name == name)
}

/// What kind a layer of the clay is, by name ("base" and "paint" too).
pub fn kind_of(clay: &Clay, name: &str) -> Result<Kind, String> {
    match name {
        BASE => Ok(Kind::Sculpt),
        PAINT => Ok(Kind::Paint),
        _ => layer(clay, name).map(|l| l.kind).ok_or_else(|| no_layer(clay, name)),
    }
}

fn no_layer(clay: &Clay, name: &str) -> String {
    let names: Vec<&str> = [BASE, PAINT].into_iter().chain(extra(clay).iter().map(|l| l.name.as_str())).collect();
    format!("no layer {name}; the clay has {} (sculpt_layer add makes one)", names.join(", "))
}

/// The contributing extra sculpt layers that line up with `reference`
/// (the first one's layout when None), with their amounts.
fn sculpt_parts<'a>(s: &'a StackData, reference: Option<&Detail>, skip: Option<&str>) -> Vec<(&'a Detail, f32)> {
    let mut out: Vec<(&Detail, f32)> = Vec::new();
    for l in s.layers.iter().filter(|l| l.kind == Kind::Sculpt && l.contributes() && Some(l.name.as_str()) != skip) {
        let d = l.detail.as_ref().expect("contributes");
        if reference.or(out.first().map(|p| p.0)).is_none_or(|r| r.same_layout(d)) {
            out.push((d, l.amount));
        }
    }
    out
}

fn key_of(parts: &[(Option<&Detail>, f32)]) -> Vec<(Option<Detail>, u32)> {
    parts.iter().map(|(d, a)| (d.cloned(), a.to_bits())).collect()
}

/// What the clay's fields read: base plus every enabled sculpt layer at
/// its amount. Without contributing extra layers (none, all off, at 0 or
/// empty) it is `Clay::detail` itself. Views read their own grid (their
/// strokes compose what they read: `sculpt_input`).
pub fn composed_detail(clay: &Clay) -> Option<Detail> {
    let Some(s) = stack(clay).filter(|s| s.view.is_none()) else { return clay.detail.clone() };
    let extras = sculpt_parts(s, clay.detail.as_ref(), None);
    if extras.is_empty() {
        return clay.detail.clone();
    }
    let mut parts: Vec<(Option<&Detail>, f32)> = vec![(clay.detail.as_ref(), 1.0)];
    parts.extend(extras.iter().map(|(d, a)| (Some(*d), *a)));
    let key = key_of(&parts);
    let mut cache = s.cache.detail.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, d)) = cache.as_ref()
        && *k == key
    {
        return d.clone();
    }
    let list: Vec<(&Detail, f32)> = parts.iter().filter_map(|(d, a)| d.map(|d| (d, *a))).collect();
    let out = Detail::compose(&list).ok();
    *cache = Some((key, out.clone()));
    out
}

/// The paint the clay shows: "paint" with every enabled paint layer laid
/// over it (`Clay::paint` itself without any).
pub fn composed_paint(clay: &Clay) -> Option<Paint> {
    let Some(s) = stack(clay).filter(|s| s.view.is_none()) else { return clay.paint.clone() };
    let layers: Vec<(&Paint, f32)> = s.layers.iter().filter(|l| l.kind == Kind::Paint && l.contributes()).map(|l| (l.paint.as_ref().expect("contributes"), l.amount)).collect();
    if layers.is_empty() {
        return clay.paint.clone();
    }
    let mut parts: Vec<(Option<&Paint>, f32)> = vec![(clay.paint.as_ref(), 1.0)];
    parts.extend(layers.iter().map(|(p, a)| (Some(*p), *a)));
    let key: Vec<(Option<Paint>, u32)> = parts.iter().map(|(p, a)| (p.cloned(), a.to_bits())).collect();
    let mut cache = s.cache.paint.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, p)) = cache.as_ref()
        && *k == key
    {
        return p.clone();
    }
    let list: Vec<(&Paint, f32)> = parts.iter().filter_map(|(p, a)| p.map(|p| (p, *a))).collect();
    let out = Paint::composite(&list);
    *cache = Some((key, out.clone()));
    out
}

/// The clay with its composite in place of its own grids and no stack:
/// what its meshes are built from.
pub fn composed_clay(clay: &Clay) -> Clay {
    Clay { detail: composed_detail(clay), paint: composed_paint(clay), sculpt: None, ..clay.clone() }
}

/// The contributing extra sculpt layers composed (base strokes read them
/// with the base grid), cached like the composite.
fn extras_composed(clay: &Clay) -> Result<Option<Detail>, String> {
    let Some(s) = stack(clay) else { return Ok(None) };
    let extras = sculpt_parts(s, clay.detail.as_ref(), None);
    if extras.is_empty() {
        return Ok(None);
    }
    let parts: Vec<(Option<&Detail>, f32)> = extras.iter().map(|(d, a)| (Some(*d), *a)).collect();
    let key = key_of(&parts);
    let mut cache = s.cache.extras.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, d)) = cache.as_ref()
        && *k == key
    {
        return Ok(d.clone());
    }
    let out = Detail::compose(&extras)?;
    *cache = Some((key, Some(out.clone())));
    Ok(Some(out))
}

/// What a sculpt stroke on the clay (a view, or base) reads besides the
/// grid it writes, and the amount it writes at; None when it reads
/// nothing else at amount 1 (the plain path, bit for bit as before
/// layers).
pub fn sculpt_input(clay: &Clay) -> Result<Option<(Option<Detail>, f32)>, String> {
    if let Some(v) = view_of(clay) {
        if v.kind != Kind::Sculpt {
            return Ok(None);
        }
        let others = v.others()?;
        return Ok(if others.is_none() && v.amount == 1.0 { None } else { Some((others, v.amount)) });
    }
    Ok(extras_composed(clay)?.map(|e| (Some(e), 1.0)))
}

/// The sculpted surface paint, mask and groups read: a paint view's, else
/// the composite.
pub fn surface_detail(clay: &Clay) -> Option<Detail> {
    match view_of(clay) {
        Some(v) if v.kind == Kind::Paint => v.surface.clone(),
        _ => composed_detail(clay),
    }
}

/// What a layer's grid is, cheaply: its ledger's head while the ledger
/// ends at it (as `ledger::surface_hash` reads base), else its hash.
fn state(l: &Ledger<Detail>, grid: Option<&Detail>) -> Hash128 {
    ledger::grid_state(l, grid)
}

/// The contributing extra sculpt layers' amounts and contents, hashed
/// after `h` (none: `h` itself, so clays without layers hash as before).
fn fold_extras(clay: &Clay, h: Hash128, tag: &str, skip: Option<&str>) -> Hash128 {
    let Some(s) = stack(clay) else { return h };
    let ls: Vec<&Layer> = s.layers.iter().filter(|l| l.kind == Kind::Sculpt && l.contributes() && Some(l.name.as_str()) != skip).collect();
    if ls.is_empty() {
        return h;
    }
    let desc: Vec<(u32, Hash128)> = ls.iter().map(|l| (l.amount.to_bits(), state(&l.strokes, l.detail.as_ref()))).collect();
    ledger::hash_with(tag, h, &desc)
}

/// Base strokes' context: the blobs (`ctx`), and the other sculpt layers
/// they read.
pub fn base_context(clay: &Clay, ctx: Hash128) -> Hash128 {
    match view_of(clay) {
        Some(v) if v.kind == Kind::Sculpt => v.ctx,
        Some(v) => v.base_ctx,
        None => fold_extras(clay, ctx, "base layers", None),
    }
}

/// The surface's hash (`ledger::surface_hash`), with the sculpt layers.
pub fn surface_context(clay: &Clay, h: Hash128) -> Hash128 {
    fold_extras(clay, h, "surface layers", None)
}

/// The context of strokes on extra sculpt layer `name`: the blobs, its
/// own amount (strokes are stored divided by it) and the other sculpt
/// layers, base included.
fn layer_context(clay: &Clay, name: &str, amount: f32) -> Hash128 {
    let base = state(&ledger::ledger::<Detail>(clay), clay.detail.as_ref());
    let h = ledger::hash_with("layer context", ledger::clay_context(clay), &(amount.to_bits(), base));
    fold_extras(clay, h, "layer others", Some(name))
}

/// Clay `clay` seen as its extra layer `name`: the layer's grid and
/// ledger in base's (sculpt) or paint's place, no other layers, and what
/// its strokes read of them (`View`). Changes made to the view go back
/// with `install`.
pub fn view(clay: &Clay, name: &str) -> Result<Clay, String> {
    let s = stack(clay).ok_or_else(|| no_layer(clay, name))?;
    if s.view.is_some() {
        return Err("a layer view has no layers".into());
    }
    let k = s.layers.iter().position(|l| l.name == name).ok_or_else(|| no_layer(clay, name))?;
    let l = &s.layers[k];
    // The clay's other ledgers: the other extra layers', and the clay's
    // own ledger the layer's takes the place of (base for a sculpt view,
    // paint for a paint view), so a stroke on the layer sees the clay's
    // whole point cap.
    let others = s.layers.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, l)| l.points()).sum::<usize>();
    let points = others + if l.kind == Kind::Sculpt { s.base.points() } else { s.paint.points() };
    let mut data = StackData { layers: Vec::new(), morphs: Vec::new(), archived: Vec::new(), cache: Cache::default(), view: None, ..s.clone() };
    let mut v = clay.clone();
    let (base_ctx, paint_ctx) = (<Detail as Grid>::context(clay), <Paint as Grid>::context(clay));
    let view = match l.kind {
        Kind::Sculpt => {
            data.base = l.strokes.clone();
            v.detail = l.detail.clone();
            let mut parts: Vec<(Detail, f32)> = clay.detail.iter().map(|d| (d.clone(), 1.0)).collect();
            let reference = clay.detail.as_ref().or(l.detail.as_ref());
            parts.extend(sculpt_parts(s, reference, Some(name)).into_iter().map(|(d, a)| (d.clone(), a)));
            View { name: name.into(), kind: Kind::Sculpt, amount: l.amount, parts, others: OnceLock::new(), surface: None, ctx: layer_context(clay, name, l.amount), base_ctx, paint_ctx, points }
        }
        Kind::Paint => {
            data.paint = l.paint_strokes.clone();
            v.paint = l.paint.clone();
            View { name: name.into(), kind: Kind::Paint, amount: l.amount, parts: Vec::new(), others: OnceLock::new(), surface: composed_detail(clay), ctx: paint_ctx, base_ctx, paint_ctx, points }
        }
    };
    data.view = Some(Arc::new(view));
    v.sculpt = Some(ledger::SculptStack::new(data));
    Ok(v)
}

/// The clay as layer `name` of kind G sees it: itself for G's own layer
/// (base, paint, mask, groups), else a view of the extra layer.
pub fn view_for<'a, G: Layered>(clay: &'a Clay, name: &str) -> Result<Cow<'a, Clay>, String> {
    if name == G::LAYER {
        return Ok(Cow::Borrowed(clay));
    }
    match (G::KIND, kind_of(clay, name)?) {
        (Some(k), kind) if k == kind && !fixed(name) => Ok(Cow::Owned(view(clay, name)?)),
        (_, kind) => Err(format!("layer {name} is a {} layer; it takes {} strokes", match kind { Kind::Sculpt => "sculpt", Kind::Paint => "paint" }, if kind == Kind::Sculpt { "sculpt" } else { "paint" })),
    }
}

/// Puts a ledger and its grid into layer `name` (G's own layer: the clay).
pub fn install<G: Layered>(clay: &mut Clay, name: &str, l: Ledger<G>, grid: Option<G>) {
    if name == G::LAYER {
        return ledger::install(clay, l, grid);
    }
    let s = ledger::stack_mut(clay);
    if let Some((g, led)) = s.layers.iter_mut().find(|x| x.name == name).and_then(G::slot) {
        *g = grid;
        *led = l;
    }
}

/// Layer `name`'s ledger.
pub fn ledger_of<G: Layered>(clay: &Clay, name: &str) -> Result<Ledger<G>, String> {
    Ok(ledger::ledger::<G>(&*view_for::<G>(clay, name)?))
}

/// Layer `name`'s grid.
pub fn grid_of<G: Layered>(clay: &Clay, name: &str) -> Result<Option<G>, String> {
    Ok(G::current(&*view_for::<G>(clay, name)?).cloned())
}

/// Layer `name`'s stroke context.
pub fn context_of<G: Layered>(clay: &Clay, name: &str) -> Result<Hash128, String> {
    Ok(G::context(&*view_for::<G>(clay, name)?))
}

/// The layer name a record made on this clay (or view) carries.
pub fn record_layer<G: Grid>(clay: &Clay) -> String {
    match view_of(clay) {
        Some(v) if (v.kind == Kind::Sculpt && G::LAYER == BASE) || (v.kind == Kind::Paint && G::LAYER == PAINT) => v.name.clone(),
        _ => G::LAYER.into(),
    }
}

/// Dab points on the clay's ledgers beyond its base, paint, mask and
/// group ledgers: the extra layers' and (for a view) the rest of the
/// clay's. Morphs' ledgers are left out (mostly the same records; each is
/// held to the caps of one ledger), so storing a morph never takes a clay
/// past the cap.
pub fn extra_points(s: &StackData) -> usize {
    s.layers.iter().map(Layer::points).sum::<usize>() + s.view.as_ref().map_or(0, |v| v.points)
}

/// The stack's layers, morphs and archive within their caps.
pub fn is_sane(s: &StackData) -> bool {
    let mut names = HashSet::new();
    s.layers.len() <= MAX_LAYERS - 2
        && s.morphs.len() <= MAX_MORPHS
        && s.archived.len() <= MAX_ARCHIVED
        && s.layers.iter().all(|l| l.is_sane() && !fixed(&l.name) && names.insert(l.name.as_str()))
        && s.morphs.iter().all(Morph::is_sane)
        && s.archived.iter().all(|a| a.strokes.is_sane() && a.paint_strokes.is_sane())
        && s.record_layer.as_deref().is_none_or(|r| r == BASE || s.layers.iter().any(|l| l.name == r))
}

/// Runs `f` on a view of every extra layer in turn (base and paint
/// first: callers do those), putting back what each changes. Sculpt
/// layers before paint ones, so paint reads the sculpt as it ends up.
pub fn each_layer(clay: &mut Clay, f: &mut dyn FnMut(&mut Clay, Kind) -> Result<(), String>) -> Result<(), String> {
    let names: Vec<(String, Kind)> = extra(clay).iter().map(|l| (l.name.clone(), l.kind)).collect();
    for want in [Kind::Sculpt, Kind::Paint] {
        for (name, kind) in names.iter().filter(|(_, k)| *k == want) {
            let mut v = view(clay, name)?;
            f(&mut v, *kind)?;
            match kind {
                Kind::Sculpt => {
                    let (l, g) = (ledger::ledger::<Detail>(&v), v.detail.clone());
                    if layer(clay, name).is_some_and(|x| x.strokes != l || x.detail != g) {
                        install::<Detail>(clay, name, l, g);
                    }
                }
                Kind::Paint => {
                    let (l, g) = (ledger::ledger::<Paint>(&v), v.paint.clone());
                    if layer(clay, name).is_some_and(|x| x.paint_strokes != l || x.paint != g) {
                        install::<Paint>(clay, name, l, g);
                    }
                }
            }
            // A view may have kept alphas or frozen grids for its strokes.
            let vs = v.sculpt.as_ref().expect("a view has a stack").data();
            if clay.sculpt.as_ref().is_some_and(|s| s.data().alphas != vs.alphas) {
                ledger::set_alphas(clay, vs.alphas.clone());
            }
        }
    }
    Ok(())
}

/// Every ledger of sculpt grids the clay keeps besides base: extra
/// layers', morphs' (for what their records use).
pub fn detail_ledgers(clay: &Clay) -> Vec<Ledger<Detail>> {
    let Some(s) = stack(clay) else { return Vec::new() };
    let mut out: Vec<Ledger<Detail>> = s.layers.iter().filter(|l| l.kind == Kind::Sculpt).map(|l| l.strokes.clone()).collect();
    for m in &s.morphs {
        out.push(m.base_strokes.clone());
        out.extend(m.layers.iter().map(|l| l.strokes.clone()));
    }
    out
}

/// Every extra paint layer's ledger.
pub fn paint_ledgers(clay: &Clay) -> Vec<Ledger<Paint>> {
    extra(clay).iter().filter(|l| l.kind == Kind::Paint).map(|l| l.paint_strokes.clone()).collect()
}

/// Bytes of grid bricks the clay holds over its layers, morphs, mask and
/// groups, each shared brick once (`MAX_GRID_BYTES`).
pub fn grid_bytes(clay: &Clay) -> u64 {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut bytes = 0u64;
    let mut detail = |d: &Detail, bytes: &mut u64| {
        let g = d.grid();
        for level in std::iter::once(g).chain(g.levels().iter().map(|l| &l.grid)) {
            for key in level.brick_keys() {
                let b = level.brick(key).expect("listed");
                if seen.insert(Arc::as_ptr(b) as *const f32 as usize) {
                    *bytes += F32_BRICK;
                }
            }
        }
    };
    let mut paints: HashSet<usize> = HashSet::new();
    let mut paint = |p: &Paint, bytes: &mut u64| {
        if paints.insert(p.addr()) {
            *bytes += p.grid().brick_count() as u64 * F32_BRICK;
        }
    };
    if let Some(d) = &clay.detail {
        detail(d, &mut bytes);
    }
    if let Some(p) = &clay.paint {
        paint(p, &mut bytes);
    }
    if let Some(s) = stack(clay) {
        for l in &s.layers {
            l.detail.iter().for_each(|d| detail(d, &mut bytes));
            l.paint.iter().for_each(|p| paint(p, &mut bytes));
        }
        for m in &s.morphs {
            m.base.iter().for_each(|d| detail(d, &mut bytes));
            m.layers.iter().flat_map(|l| l.detail.iter()).for_each(|d| detail(d, &mut bytes));
        }
    }
    if let Some(m) = &clay.sculpt_mask {
        bytes += m.grid().brick_count() as u64 * crate::clay::BRICK_VALUES as u64;
    }
    if let Some(g) = &clay.groups {
        bytes += g.data().bricks().count() as u64 * crate::clay::BRICK_VALUES as u64 * 2;
    }
    bytes
}

/// Bytes the clay's sculpt, paint, mask and group grids inflate to when
/// a file is read back (`clay::unpack`'s form): the clay's own, its
/// layers' and morphs', and the grids of every Opaque record in their
/// ledgers (the mask's and groups' too), each as many times as it is
/// saved (`MAX_SAVED_BYTES`). `Regions` records keep no grid. The masks
/// and groups kept for records to read (`ledger::Frozen`, at most
/// `MAX_FROZEN` of each) are not counted.
pub fn saved_bytes(clay: &Clay) -> u64 {
    const KEYS: u64 = 4;
    const DETAIL_BRICK: u64 = 12 + crate::clay::BRICK_VALUES as u64 * 2;
    const PAINT_BRICK: u64 = 12 + crate::clay::BRICK_VALUES as u64 * 4;
    const MASK_BRICK: u64 = 12 + crate::clay::BRICK_VALUES as u64;
    const GROUP_BRICK: u64 = 12 + crate::clay::BRICK_VALUES as u64 * 2;
    fn detail(d: &Detail) -> u64 {
        let g = d.grid();
        std::iter::once(g).chain(g.levels().iter().map(|l| &l.grid)).map(|l| KEYS + l.brick_count() as u64 * DETAIL_BRICK).sum()
    }
    fn paint(p: &Paint) -> u64 {
        KEYS + p.grid().brick_count() as u64 * PAINT_BRICK
    }
    // Ledgers copied into morphs share their records: each is added up
    // once, and counted every time it is saved.
    fn opaque<G: Grid>(l: &Ledger<G>, size: fn(&G) -> u64, seen: &mut HashMap<usize, u64>) -> u64 {
        *seen.entry(l.records().as_ptr() as usize).or_insert_with(|| {
            l.records()
                .iter()
                .map(|r| match &r.op {
                    ledger::RecordOp::Opaque { grid: Some(g), .. } => size(g),
                    _ => 0,
                })
                .sum()
        })
    }
    fn mask(m: &SculptMask) -> u64 {
        KEYS + m.grid().brick_count() as u64 * MASK_BRICK
    }
    fn groups(g: &Groups) -> u64 {
        KEYS + g.data().bricks().count() as u64 * GROUP_BRICK
    }
    let (mut ds, mut ps, mut ms, mut gs) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let mut bytes = clay.detail.as_ref().map_or(0, detail) + clay.paint.as_ref().map_or(0, paint);
    bytes += clay.sculpt_mask.as_ref().map_or(0, mask) + clay.groups.as_ref().map_or(0, groups);
    let Some(s) = stack(clay) else { return bytes };
    bytes += opaque(&s.base, detail, &mut ds) + opaque(&s.paint, paint, &mut ps);
    // The mask's and polygroups' outside edits (a mask from a smart
    // material's channel, a squash) keep whole grids too.
    bytes += opaque(&s.mask, mask, &mut ms) + opaque(&s.groups, groups, &mut gs);
    for l in &s.layers {
        bytes += l.detail.as_ref().map_or(0, detail) + l.paint.as_ref().map_or(0, paint);
        bytes += opaque(&l.strokes, detail, &mut ds) + opaque(&l.paint_strokes, paint, &mut ps);
    }
    for m in &s.morphs {
        bytes += m.base.as_ref().map_or(0, detail) + opaque(&m.base_strokes, detail, &mut ds);
        for l in &m.layers {
            bytes += l.detail.as_ref().map_or(0, detail) + opaque(&l.strokes, detail, &mut ds);
        }
    }
    for a in &s.archived {
        bytes += opaque(&a.strokes, detail, &mut ds) + opaque(&a.paint_strokes, paint, &mut ps);
    }
    bytes
}

/// Refuses a clay with layers or morphs whose grids grew past
/// `MAX_GRID_BYTES` in memory or `MAX_SAVED_BYTES` in a file (a clay
/// without either keeps the per-grid caps it always had).
pub fn check_budget(clay: &Clay) -> Result<(), String> {
    if stack(clay).is_none_or(|s| s.layers.is_empty() && s.morphs.is_empty()) {
        return Ok(());
    }
    check_budget_within(clay, MAX_GRID_BYTES)?;
    check_saved_within(clay, MAX_SAVED_BYTES)
}

/// Records the clay's outside edits (`ledger::sync_clay`) and checks the
/// budget with what that recorded: a relayout's whole-grid records and a
/// blend's, bake's or split's delta records count. The grids are checked
/// first too, so the deltas sync builds are bounded by grids that fit.
/// Callers run it on their copy of the clay and put that back only on
/// `Ok`, so a refused call leaves the scene as it was.
pub fn sync_within_budget(clay: &mut Clay) -> Result<bool, String> {
    sync_within(clay, MAX_GRID_BYTES, MAX_SAVED_BYTES)
}

fn sync_within(clay: &mut Clay, grid_max: u64, saved_max: u64) -> Result<bool, String> {
    let layered = |clay: &Clay| stack(clay).is_some_and(|s| !s.layers.is_empty() || !s.morphs.is_empty());
    let checked = |clay: &Clay| -> Result<(), String> {
        if !layered(clay) {
            return Ok(());
        }
        check_budget_within(clay, grid_max)?;
        check_saved_within(clay, saved_max)
    };
    checked(clay)?;
    let added = ledger::sync_clay(clay)?;
    checked(clay)?;
    // A clay without layers or morphs keeps its per-grid caps, but what
    // sync just recorded (a relayout's or outside edit's whole grids, one
    // per Sculptris stroke that grows the regions) piles up in its
    // ledgers: those count against the saved budget too, so the file
    // still loads and memory stays bounded.
    if added && !layered(clay) {
        check_saved_within(clay, saved_max)?;
    }
    Ok(added)
}

const BUDGET_HELP: &str = "bake or merge layers down (sculpt_layer bake, merge_down), delete layers or morphs you no longer need, squash old strokes (stroke_edit squash), or use a coarser detail_cell";

fn check_budget_within(clay: &Clay, max: u64) -> Result<(), String> {
    let bytes = grid_bytes(clay);
    if bytes > max {
        return Err(format!("the clay's sculpt, paint, mask and group grids would take {} MB over its layers and morphs (at most {} MB): {BUDGET_HELP}", bytes >> 20, max >> 20));
    }
    Ok(())
}

fn check_saved_within(clay: &Clay, max: u64) -> Result<(), String> {
    let bytes = saved_bytes(clay);
    if bytes > max {
        return Err(format!("the clay's sculpt, paint, mask and group grids would take {} MB in a saved file, over its layers, its ledgers' outside-edit and squash grids, and its morphs (each writes its grids and ledgers again) (at most {} MB): {BUDGET_HELP}", bytes >> 20, max >> 20));
    }
    Ok(())
}

/// Makes every extra sculpt layer and morph grid follow the base grid's
/// layout (after `edit_clay detail_cell` or `detail_regions`), and every
/// extra paint layer the paint cell. Their ledgers record the change as
/// an outside edit (`ledger::sync_clay`). `old_base` is the base grid
/// before the change: grids that shared it take the new one.
pub fn relayout(clay: &mut Clay, paint_cell: Option<f32>, old_base: Option<Detail>) -> Result<(), String> {
    let seeds = match (old_base, clay.detail.clone()) {
        (Some(old), Some(new)) => vec![(old, new)],
        _ => Vec::new(),
    };
    relayout_seeded(clay, paint_cell, seeds)
}

/// `relayout`, with grids already relaid: `(old, new)` pairs, so what
/// shared an old grid (a morph's with the layer it was stored from)
/// takes the same new one.
fn relayout_seeded(clay: &mut Clay, paint_cell: Option<f32>, seeds: Vec<(Detail, Detail)>) -> Result<(), String> {
    if clay.sculpt.is_none() {
        return Ok(());
    }
    // Without a base grid the layers take the new cell (no regions).
    let Some(base) = clay.detail.clone().or_else(|| paint_cell.map(Detail::new)) else { return Ok(()) };
    // Grids shared before (a morph's with the layers it was stored from)
    // stay shared: each is relaid once.
    // Each source is held while it is keyed, so no address is reused.
    let mut done: HashMap<usize, (Detail, Detail)> = HashMap::new();
    // The clay's own grid (and any given) was relaid already: what shares
    // it shares that.
    for (old, new) in seeds {
        done.insert(old.addr(), (old, new));
    }
    let mut relaid = |d: &Detail| -> Result<Detail, String> {
        if let Some((_, r)) = done.get(&d.addr()) {
            return Ok(r.clone());
        }
        let r = d.relaid_like(&base)?;
        done.insert(d.addr(), (d.clone(), r.clone()));
        Ok(r)
    };
    let s = ledger::stack_mut(clay);
    for l in &mut s.layers {
        if let Some(d) = &l.detail {
            l.detail = Some(relaid(d)?);
        }
        if let (Some(p), Some(c)) = (&l.paint, paint_cell)
            && p.grid().voxel != c
        {
            l.paint = Some(p.resampled(c)?);
        }
    }
    for m in &mut s.morphs {
        // A morph's grids follow too (its ledgers keep their records:
        // their next replay records the change).
        if let Some(d) = &m.base {
            m.base = Some(relaid(d)?);
        }
        for l in &mut m.layers {
            if let Some(d) = &l.detail {
                l.detail = Some(relaid(d)?);
            }
        }
    }
    Ok(())
}

/// A record `set_regions_recorded` made, with its seq (1-based) in its
/// ledger.
pub type Made = (Arc<ledger::Record<Detail>>, usize);

/// Lays the clay's detail grid out with `regions` (Sculptris Pro growing
/// them before a stroke) and records it as `Regions` records: on base, and
/// on every extra sculpt layer with a grid (laid out like base, as
/// `relayout` would). Each grid is the record's own replay, so a replay
/// gives it bit for bit, and the records keep the regions, not the grids,
/// so a long drag that keeps growing a region does not pile whole grids
/// into the ledgers. Morph grids follow as `relayout` moves them (their
/// ledgers record it when switched in). Outside edits are recorded first
/// (`sync_within_budget`), so the records apply to what the clay shows.
/// Returns the records made, each with its seq (1-based) in its ledger.
/// The caller checks the budgets afterwards.
pub fn set_regions_recorded(clay: &mut Clay, regions: &[crate::clay::Region], meta: &ledger::Meta, cache: Option<&ledger::ReplayCache>) -> Result<Vec<Made>, String> {
    sync_within_budget(clay)?;
    let cell = match clay.detail.as_ref() {
        Some(d) => d.voxel(),
        None => clay.detail_cell.or_else(|| clay.cell_size()).ok_or("add blobs before detail regions")?,
    };
    let old_base = clay.detail.clone();
    // As `Clay::set_detail_regions`: the cell becomes the clay's
    // detail_cell (before the record, so its context is the one strokes
    // after it read).
    clay.detail_cell = Some(clay.detail_cell.unwrap_or(cell));
    let mut out = Vec::new();
    let made = |c: &ledger::Committed<Detail>| -> Vec<Made> { c.synced.iter().chain([&c.record]).map(|r| (r.clone(), c.ledger.position(r.id).map_or(0, |i| i + 1))).collect() };
    let c = ledger::commit_relayout::<Detail>(clay, &ledger::ledger(clay), meta, regions, cell, cache)?;
    out.extend(made(&c));
    ledger::install(clay, c.ledger, c.grid);
    // The layers take base's regions in its levels' order (`relaid_like`).
    let laid: Vec<crate::clay::Region> = clay.detail.as_ref().map_or_else(Vec::new, |d| d.grid().levels().iter().map(|l| l.region.clone()).collect());
    let mut seeds: Vec<(Detail, Detail)> = old_base.into_iter().zip(clay.detail.clone()).collect();
    let names: Vec<String> = extra(clay).iter().filter(|l| l.kind == Kind::Sculpt && l.detail.as_ref().is_some_and(|d| d.voxel() == cell)).map(|l| l.name.clone()).collect();
    for name in names {
        let old = layer(clay, &name).and_then(|l| l.detail.clone());
        let c = {
            let v = view_for::<Detail>(clay, &name)?;
            ledger::commit_relayout::<Detail>(&v, &ledger::ledger(&v), meta, &laid, cell, cache)?
        };
        out.extend(made(&c));
        seeds.extend(old.into_iter().zip(c.grid.clone()));
        install::<Detail>(clay, &name, c.ledger, c.grid);
    }
    // Morphs (and any layer of another cell, recorded by the caller's
    // sync as before).
    relayout_seeded(clay, None, seeds)?;
    Ok(out)
}

/// A layer's summary (`sculpt_layer list`, `get_clay`).
pub fn layer_json(clay: &Clay, name: &str) -> Result<Value, String> {
    let kind = kind_of(clay, name)?;
    let mut v = match kind {
        Kind::Sculpt => {
            let view = view_for::<Detail>(clay, name)?;
            let mut v = ledger::layer_json::<Detail>(&view);
            v["bricks"] = json!(view.detail.as_ref().map_or(0, |d| d.grid().total_bricks()));
            if name != BASE {
                let lines_up = match (&clay.detail, &view.detail) {
                    (Some(b), Some(d)) => b.same_layout(d),
                    _ => true,
                };
                v["lines_up"] = json!(lines_up);
            }
            v
        }
        Kind::Paint => {
            let view = view_for::<Paint>(clay, name)?;
            let mut v = ledger::layer_json::<Paint>(&view);
            v["bricks"] = json!(view.paint.as_ref().map_or(0, |p| p.grid().brick_count()));
            v
        }
    };
    v["name"] = json!(name);
    if let Some(l) = layer(clay, name) {
        v["amount"] = json!(l.amount);
        v["enabled"] = json!(l.enabled);
        v["contributes"] = json!(l.contributes());
    }
    Ok(v)
}

/// Every layer, bottom first.
pub fn names(clay: &Clay) -> Vec<String> {
    [BASE.to_string(), PAINT.to_string()].into_iter().chain(extra(clay).iter().map(|l| l.name.clone())).collect()
}

/// Every layer's summary, bottom first.
pub fn list_json(clay: &Clay) -> Value {
    let layers: Vec<Value> = names(clay).iter().filter_map(|n| layer_json(clay, n).ok()).collect();
    let s = stack(clay);
    json!({
        "layers": layers,
        "record": s.and_then(|s| s.record_layer.clone()),
        "morphs": s.map_or(Vec::new(), |s| s.morphs.iter().map(|m| m.name.clone()).collect()),
        "archived": s.map_or(Vec::new(), |s| s.archived.iter().map(|a| json!({ "name": a.name, "into": a.into, "records": a.strokes.len() + a.paint_strokes.len() })).collect()),
        "grid_mb": (grid_bytes(clay) as f64 / (1 << 20) as f64 * 10.0).round() / 10.0,
        "max_grid_mb": MAX_GRID_BYTES >> 20,
        "saved_mb": (saved_bytes(clay) as f64 / (1 << 20) as f64 * 10.0).round() / 10.0,
        "max_saved_mb": MAX_SAVED_BYTES >> 20,
    })
}

/// The sculpt layer a call without `layer` writes: the clay's recording
/// layer (`sculpt_layer record`), else base. Never the editor's selection.
pub fn default_sculpt_layer(clay: &Clay) -> String {
    stack(clay).and_then(|s| s.record_layer.clone()).filter(|r| layer(clay, r).is_some_and(|l| l.kind == Kind::Sculpt)).unwrap_or_else(|| BASE.into())
}

/// A sculpt layer's amount as strokes on it need it (refused near 0).
pub fn stroke_amount(clay: &Clay, name: &str) -> Result<f32, String> {
    match name {
        BASE => Ok(1.0),
        PAINT => Err("paint is the clay's paint layer; paint it with the paint tool (sculpt with paint: {color} colors it as it sculpts)".into()),
        _ => {
            let l = layer(clay, name).ok_or_else(|| no_layer(clay, name))?;
            if l.kind != Kind::Sculpt {
                return Err(format!("layer {name} is a paint layer; paint it with the paint tool (layer {name})"));
            }
            if l.amount.abs() < MIN_AMOUNT {
                return Err(format!("layer {name} is at amount {}: strokes are stored divided by the amount, so they are refused below {MIN_AMOUNT}; raise it first (sculpt_layer amount)", l.amount));
            }
            Ok(l.amount)
        }
    }
}

/// The composite with layer `name` at `amount` instead (enabled), sharing
/// every brick that layer does not touch with `old` (the composite before):
/// only that layer's bricks are summed again. Equal, point for point, to
/// composing anew.
pub fn recompose(clay: &Clay, name: &str, amount: f32, old: Option<&Detail>) -> Result<Option<Detail>, String> {
    let s = stack(clay).ok_or_else(|| no_layer(clay, name))?;
    let mut parts: Vec<(&Detail, f32)> = clay.detail.iter().map(|d| (d, 1.0)).collect();
    let mut touched: Option<&Detail> = None;
    for l in s.layers.iter().filter(|l| l.kind == Kind::Sculpt && (l.enabled || l.name == name)) {
        let Some(d) = &l.detail else { continue };
        let a = if l.name == name {
            touched = Some(d);
            amount
        } else {
            l.amount
        };
        if a != 0.0 && parts.first().is_none_or(|(f, _)| f.same_layout(d)) {
            parts.push((d, a));
        }
    }
    if parts.len() <= clay.detail.is_some() as usize {
        return Ok(clay.detail.clone());
    }
    match (touched, old) {
        (Some(t), Some(old)) if old.same_layout(t) => Detail::recompose_keys(&parts, old, t).map(Some),
        _ => Detail::compose(&parts).map(Some),
    }
}

/// Puts a composite made for the clay as it is now (`recompose`) into its
/// stack's cache, so the next mesh build takes it without composing.
pub fn plant(clay: &Clay, composite: Option<Detail>) {
    let Some(s) = stack(clay) else { return };
    let extras = sculpt_parts(s, clay.detail.as_ref(), None);
    if extras.is_empty() {
        return;
    }
    let mut parts: Vec<(Option<&Detail>, f32)> = vec![(clay.detail.as_ref(), 1.0)];
    parts.extend(extras.iter().map(|(d, a)| (Some(*d), *a)));
    *s.cache.detail.lock().unwrap_or_else(|e| e.into_inner()) = Some((key_of(&parts), composite));
}

/// Folds extra layer `name` into `into` at its current contribution (the
/// composite stays as it was), keeps its ledger for reading and removes
/// it. `into`'s ledger records the change (an outside edit).
pub fn fold(clay: &mut Clay, name: &str, into: &str) -> Result<(), String> {
    let k = extra(clay).iter().position(|l| l.name == name).ok_or_else(|| no_layer(clay, name))?;
    let l = extra(clay)[k].clone();
    let a = if l.enabled { l.amount } else { 0.0 };
    match l.kind {
        Kind::Sculpt => {
            let to = stroke_amount(clay, into).map_err(|e| format!("cannot fold into {into}: {e}"))?;
            if let Some(g) = l.detail.as_ref().filter(|_| a != 0.0) {
                let target = grid_of::<Detail>(clay, into)?;
                let parts: Vec<(&Detail, f32)> = target.iter().map(|t| (t, 1.0)).chain(std::iter::once((g, a / to))).collect();
                let folded = Detail::compose(&parts).map_err(|e| format!("cannot fold {name} into {into}: {e}"))?;
                set_grid::<Detail>(clay, into, Some(folded));
            }
        }
        Kind::Paint => {
            if let Some(p) = l.paint.as_ref().filter(|_| a > 0.0) {
                let target = grid_of::<Paint>(clay, into)?;
                let folded = match &target {
                    Some(t) => Paint::composite(&[(t, 1.0), (p, a)]),
                    None => Paint::composite(&[(p, a)]),
                };
                set_grid::<Paint>(clay, into, folded);
            }
        }
    }
    let s = ledger::stack_mut(clay);
    let l = s.layers.remove(k);
    s.archived.push(Archived { name: l.name, kind: l.kind, into: into.into(), time: ledger::now_secs(), strokes: l.strokes, paint_strokes: l.paint_strokes });
    while s.archived.len() > MAX_ARCHIVED {
        s.archived.remove(0);
    }
    if s.record_layer.as_deref() == Some(name) {
        s.record_layer = None;
    }
    Ok(())
}

/// Sets layer `name`'s grid, leaving its ledger (the next sync records
/// the change as an outside edit).
pub fn set_grid<G: Layered>(clay: &mut Clay, name: &str, grid: Option<G>) {
    let l = ledger_of::<G>(clay, name).unwrap_or_default();
    if name == G::LAYER {
        G::put(clay, grid);
    } else {
        install::<G>(clay, name, l, grid);
    }
}

/// The sculpt layers as a morph target named `name`.
pub fn snapshot(clay: &Clay, name: &str) -> Morph {
    let layers = extra(clay).iter().filter(|l| l.kind == Kind::Sculpt).map(|l| MorphLayer { name: l.name.clone(), amount: l.amount, enabled: l.enabled, detail: l.detail.clone(), strokes: l.strokes.clone() }).collect();
    Morph { name: name.into(), context: ledger::clay_context(clay), base: clay.detail.clone(), base_strokes: ledger::ledger::<Detail>(clay), layers }
}

/// The names of the clay's sculpt layers (base aside), in order.
fn sculpt_names(clay: &Clay) -> Vec<String> {
    extra(clay).iter().filter(|l| l.kind == Kind::Sculpt).map(|l| l.name.clone()).collect()
}

fn same_layers(clay: &Clay, m: &Morph) -> Result<(), String> {
    let mine = sculpt_names(clay);
    let theirs: Vec<String> = m.layers.iter().map(|l| l.name.clone()).collect();
    if mine != theirs {
        return Err(format!("morph {} was stored with sculpt layers {:?}, the clay has {:?}: store it again, or add or delete layers to match", m.name, theirs, mine));
    }
    Ok(())
}

/// Puts a morph's sculpt layers on the clay (grids, ledgers, amounts and
/// switches, shared), returning what they replace as a morph.
pub fn apply_morph(clay: &mut Clay, m: &Morph) -> Result<Morph, String> {
    same_layers(clay, m)?;
    let old = snapshot(clay, &m.name);
    ledger::install(clay, m.base_strokes.clone(), m.base.clone());
    let s = ledger::stack_mut(clay);
    for ml in &m.layers {
        if let Some(l) = s.layers.iter_mut().find(|l| l.name == ml.name) {
            (l.amount, l.enabled, l.detail, l.strokes) = (ml.amount, ml.enabled, ml.detail.clone(), ml.strokes.clone());
        }
    }
    Ok(old)
}

/// Moves the clay's sculpt layers toward morphs: each grid becomes
/// grid + sum(w x (morph's - grid)), amounts likewise. The grids change
/// outside their ledgers (`ledger::sync_clay` records them).
pub fn blend(clay: &mut Clay, morphs: &[(&Morph, f32)]) -> Result<(), String> {
    for (m, w) in morphs {
        same_layers(clay, m)?;
        if !w.is_finite() || !(0.0..=1.0).contains(w) {
            return Err("morph weights are 0 to 1".into());
        }
    }
    let total: f32 = morphs.iter().map(|(_, w)| *w).sum();
    if total > 1.0 + 1e-6 {
        return Err("morph weights add up to at most 1".into());
    }
    let mix = |cur: Option<&Detail>, theirs: &[Option<&Detail>]| -> Result<Option<Detail>, String> {
        let reference = cur.or_else(|| theirs.iter().flatten().next().copied());
        let Some(reference) = reference else { return Ok(None) };
        let empty = reference.empty_like();
        let mut parts: Vec<(&Detail, f32)> = vec![(cur.unwrap_or(&empty), 1.0 - total)];
        for ((_, w), t) in morphs.iter().zip(theirs) {
            parts.push((t.unwrap_or(&empty), *w));
        }
        if parts.iter().any(|(d, _)| !d.same_layout(reference)) {
            return Err("a morph's grids no longer line up with the clay's (its detail cell or regions changed); store it again".into());
        }
        Detail::compose(&parts).map(Some)
    };
    let base = mix(clay.detail.as_ref(), &morphs.iter().map(|(m, _)| m.base.as_ref()).collect::<Vec<_>>())?;
    let mut layers = Vec::new();
    for name in sculpt_names(clay) {
        let l = layer(clay, &name).expect("listed");
        let theirs: Vec<&MorphLayer> = morphs.iter().map(|(m, _)| m.layers.iter().find(|x| x.name == name).expect("same layers")).collect();
        let grid = mix(l.detail.as_ref(), &theirs.iter().map(|t| t.detail.as_ref()).collect::<Vec<_>>())?;
        let amount = l.amount * (1.0 - total) + morphs.iter().zip(&theirs).map(|((_, w), t)| w * t.amount).sum::<f32>();
        layers.push((name, grid, amount));
    }
    clay.detail = base;
    let s = ledger::stack_mut(clay);
    for (name, grid, amount) in layers {
        if let Some(l) = s.layers.iter_mut().find(|l| l.name == name) {
            (l.detail, l.amount) = (grid, amount);
        }
    }
    Ok(())
}

/// How far two composites differ: (changed points, largest and mean
/// change, rms).
pub fn distance(a: Option<&Detail>, b: Option<&Detail>) -> Value {
    let (mut n, mut max, mut sum, mut sq) = (0u64, 0.0f32, 0.0f64, 0.0f64);
    ledger::detail_changes(a, b, |_, _, u, v| {
        let d = (v - u).abs();
        n += 1;
        max = max.max(d);
        sum += d as f64;
        sq += (d as f64).powi(2);
    });
    json!({ "changed_points": n, "max": max, "mean": if n > 0 { sum / n as f64 } else { 0.0 }, "rms": if n > 0 { (sq / n as f64).sqrt() } else { 0.0 } })
}

/// The shape a blend shape names: "layer:NAME" (the composite with that
/// sculpt layer fully on) or "morph:NAME" (the morph's composite), as a
/// clay to project the exported vertices onto. Err for an unknown or
/// unusable name; Ok(Err(why)) for a shape that would not move anything.
pub fn shape_target(clay: &Clay, shape: &str) -> Result<Result<Clay, String>, String> {
    let (kind, name) = shape.split_once(':').ok_or_else(|| format!("blend shape {shape}: name it layer:NAME or morph:NAME"))?;
    let detail = match kind {
        "layer" => {
            let l = layer(clay, name).ok_or_else(|| format!("blend shape {shape}: {}", no_layer(clay, name)))?;
            if l.kind != Kind::Sculpt {
                return Err(format!("blend shape {shape}: {name} is a paint layer"));
            }
            if l.enabled && l.amount == 1.0 {
                return Ok(Err(format!("layer {name} is already fully on in the exported mesh; disable it (sculpt_layer disable) or set its amount to 0 to export it as a blend shape")));
            }
            let Some(d) = &l.detail else { return Ok(Err(format!("layer {name} holds no sculpting"))) };
            let s = stack(clay).expect("has the layer");
            let mut parts: Vec<(&Detail, f32)> = clay.detail.iter().map(|d| (d, 1.0)).collect();
            for x in s.layers.iter().filter(|x| x.kind == Kind::Sculpt) {
                let (a, on) = if x.name == name { (1.0, true) } else { (x.amount, x.enabled) };
                if let Some(g) = x.detail.as_ref().filter(|_| on && a != 0.0)
                    && parts.first().is_none_or(|(f, _)| f.same_layout(g))
                {
                    parts.push((g, a));
                }
            }
            if !parts.iter().any(|(g, _)| *g == d) {
                return Err(format!("blend shape {shape}: the layer does not line up with base"));
            }
            Some(Detail::compose(&parts)?)
        }
        "morph" => stack(clay).and_then(|s| s.morphs.iter().find(|m| m.name == name)).ok_or_else(|| format!("blend shape {shape}: no morph {name} (morph list)"))?.composed(),
        _ => return Err(format!("blend shape {shape}: name it layer:NAME or morph:NAME")),
    };
    Ok(Ok(Clay { detail, paint: None, sculpt: None, ..clay.clone() }))
}

/// A blend shape fitted to an exported mesh (`project_shape`).
pub struct Fitted {
    /// How far each vertex moves at weight 1.
    pub deltas: Vec<[f32; 3]>,
    /// Largest |field| of the target at a moved vertex.
    pub vertex_residual: f32,
    /// Largest distance from the target's surface (its mesh's vertices) to
    /// the moved mesh: where the target stretched past the vertices.
    pub coverage_residual: f32,
    /// The mesh cell, the unit residuals are judged in.
    pub cell: f32,
}

impl Fitted {
    pub fn residual(&self) -> f32 {
        self.vertex_residual.max(self.coverage_residual)
    }
}

/// Distance from `p` to triangle `abc`.
fn triangle_distance(p: glam::Vec3, a: glam::Vec3, b: glam::Vec3, c: glam::Vec3) -> f32 {
    // Ericson, Real-Time Collision Detection 5.1.5.
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return p.distance(a);
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return p.distance(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return p.distance(a + ab * (d1 / (d1 - d3)));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return p.distance(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return p.distance(a + ac * (d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return p.distance(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denom = 1.0 / (va + vb + vc);
    p.distance(a + ab * (vb * denom) + ac * (vc * denom))
}

/// Moves every vertex of an exported mesh onto a blend shape's target
/// surface (`shape_target`), so the shape keeps the mesh's vertices: two
/// or three Newton steps on the target's field, p -= f grad f / |grad f|^2.
/// Then measures how well that follows the target: the field left at the
/// moved vertices, and how far the target's own surface (a mesh of it)
/// lies from the moved triangles, which catches pulls that stretch the
/// surface past the vertices (a long grab leaves a gap the vertices
/// cannot fill).
pub fn project_shape(target: &Clay, positions: &[[f32; 3]], indices: &[u32]) -> Fitted {
    use glam::{IVec3, Vec3};
    let cell = target.cell_size().unwrap_or(0.01);
    let voxel = target.detail.as_ref().map_or(cell, Detail::voxel).min(cell);
    let field = crate::clay::Field::new(target, voxel);
    let e = 0.5 * voxel;
    let at = |q: Vec3| {
        let list = field.near(q - Vec3::splat(4.0 * voxel), q + Vec3::splat(4.0 * voxel));
        field.sample(q, &list).0
    };
    let mut moved: Vec<([f32; 3], f32)> = positions.iter().map(|p| (*p, 0.0)).collect();
    crate::clay::par_chunks(&mut moved, 512, |_, chunk| {
        for (p, r) in chunk.iter_mut() {
            let mut q = Vec3::from(*p);
            for _ in 0..3 {
                let f = at(q);
                let g = Vec3::new(at(q + Vec3::X * e) - at(q - Vec3::X * e), at(q + Vec3::Y * e) - at(q - Vec3::Y * e), at(q + Vec3::Z * e) - at(q - Vec3::Z * e)) / (2.0 * e);
                let g2 = g.length_squared();
                if f == 0.0 || g2 < 1e-8 || !f.is_finite() {
                    break;
                }
                q -= g * (f / g2);
            }
            *r = at(q).abs();
            *p = q.to_array();
        }
    });
    let vertex_residual = moved.iter().map(|m| m.1).fold(0.0f32, f32::max);
    // The target's surface against the moved triangles, on a hash of
    // cells two mesh cells wide (a target point with no triangle within
    // that counts as that far).
    let h = 2.0 * cell;
    let key = |p: Vec3| (p / h).floor().as_ivec3();
    let mut grid: std::collections::HashMap<IVec3, Vec<u32>> = std::collections::HashMap::new();
    let pos = |i: u32| Vec3::from(moved[i as usize].0);
    for (t, tri) in indices.chunks_exact(3).enumerate() {
        let (a, b, c) = (pos(tri[0]), pos(tri[1]), pos(tri[2]));
        let (lo, hi) = (key(a.min(b).min(c)), key(a.max(b).max(c)));
        let span = (hi - lo + IVec3::ONE).as_i64vec3();
        // A triangle stretched over many cells is left out: it cannot
        // stand for the surface it spans.
        if span.x * span.y * span.z > 512 {
            continue;
        }
        for z in lo.z..=hi.z {
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    grid.entry(IVec3::new(x, y, z)).or_default().push(t as u32);
                }
            }
        }
    }
    let surface = target.build_mesh_with(&[], "");
    let mut gaps: Vec<f32> = vec![0.0; surface.vertices.len()];
    let points: Vec<Vec3> = surface.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    crate::clay::par_chunks(&mut gaps, 1024, |n, chunk| {
        for (j, g) in chunk.iter_mut().enumerate() {
            let p = points[n * 1024 + j];
            let k = key(p);
            let mut best = h;
            for z in -1..=1 {
                for y in -1..=1 {
                    for x in -1..=1 {
                        for &t in grid.get(&(k + IVec3::new(x, y, z))).into_iter().flatten() {
                            let tri = &indices[t as usize * 3..t as usize * 3 + 3];
                            best = best.min(triangle_distance(p, pos(tri[0]), pos(tri[1]), pos(tri[2])));
                        }
                    }
                }
            }
            *g = best;
        }
    });
    let coverage_residual = gaps.iter().copied().fold(0.0f32, f32::max);
    let deltas = moved.iter().zip(positions).map(|(m, p)| [m.0[0] - p[0], m.0[1] - p[1], m.0[2] - p[2]]).collect();
    Fitted { deltas, vertex_residual, coverage_residual, cell }
}

#[cfg(test)]
mod regions_tests;
#[cfg(test)]
mod tests;
