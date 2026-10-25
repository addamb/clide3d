//! Detail levels (docs/SCULPT_PLAN.md Stage 8a): the cells a clay's
//! sculpting moves between, coarse to fine, as ZBrush moves between
//! subdivision levels. Level 0 is the clay's own detail cell; the finer
//! ones are kept on its sculpt stack (`StackData::levels`) with the
//! active one, so `sculpt` calls without `level` work at the same level
//! whatever the editor shows.
//!
//! A level is a cell, not a grid: a stroke at level k > 0 grows a detail
//! region of that cell over itself (Sculptris Pro, `clay::sculptris`),
//! and the stroke writes the levels at least that coarse. Finer detail
//! under it on the layer it writes is carried along with the change
//! (`sculpt::engine`, detail transport), so a coarse edit keeps the pores
//! made at a finer level. Other sculpt layers keep their own detail where
//! it is (a stroke writes one layer); their records then read a changed
//! base, so they show stale_context and `stroke_edit rebase` replays them.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::clay::{Clay, MIN_DETAIL_CELL};

/// Most levels a clay keeps besides its own cell.
pub const MAX_LEVELS: usize = 8;

/// Two cells this close are one level.
fn same(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * a.abs().max(b.abs())
}

/// The clay's own cell (level 0): its detail grid's, else the one its
/// first stroke takes.
pub fn coarse(clay: &Clay) -> Option<f32> {
    clay.detail.as_ref().map(crate::clay::Detail::voxel).or(clay.detail_cell).or_else(|| clay.cell_size()).filter(|c| c.is_finite() && *c > 0.0)
}

/// The finer cells kept on the stack, finest last (as saved).
fn kept(clay: &Clay) -> &[f32] {
    clay.sculpt.as_ref().map_or(&[], |s| s.data().levels.as_slice())
}

/// Every level's cell, coarse first: the clay's own cell, then the kept
/// cells finer than it (a coarser `detail_cell` set since hides those it
/// passed). Empty when the clay has no cell yet.
pub fn cells(clay: &Clay) -> Vec<f32> {
    let Some(c) = coarse(clay) else { return Vec::new() };
    std::iter::once(c).chain(kept(clay).iter().copied().filter(|l| *l < c * 0.99)).collect()
}

/// The active level: the one whose cell the stack keeps as active (0
/// when it keeps none, or one no longer listed).
pub fn active(clay: &Clay) -> usize {
    let cells = cells(clay);
    let want = clay.sculpt.as_ref().and_then(|s| s.data().level);
    want.and_then(|w| cells.iter().skip(1).position(|c| same(*c, w)).map(|k| k + 1)).unwrap_or(0)
}

/// Fills in a sculpt stroke's level (JSON, before it is read): as given,
/// else the call's, else the clay's active one when it has levels and the
/// stroke names no `cell` (Sculptris Pro on its own). A level above 0 sets
/// the stroke's `cell` to its own, so the stroke grows a region of it
/// first. Records keep both.
pub fn fill(clay: &Clay, raw: &mut Value, call: Option<u8>) -> Result<(), String> {
    let cells = cells(clay);
    let level = match raw.get("level").filter(|v| !v.is_null()) {
        Some(v) => Some(v.as_u64().filter(|l| *l <= u8::MAX as u64).ok_or("level must be a whole number from 0 (detail_level lists them)")? as u8),
        None => None,
    };
    let cell = raw.get("cell").filter(|v| !v.is_null()).map(|v| v.as_f64().map(|c| c as f32).ok_or("cell must be a number")).transpose()?;
    let Some(level) = level.or(call).or_else(|| (cells.len() > 1 && cell.is_none()).then(|| active(clay) as u8)) else { return Ok(()) };
    let at = *cells.get(level as usize).ok_or_else(|| format!("no detail level {level}: the clay has levels 0 to {} (detail_level)", cells.len().saturating_sub(1)))?;
    let o = raw.as_object_mut().ok_or("a stroke is an object")?;
    o.insert("level".into(), json!(level));
    match (level, cell) {
        (0, Some(_)) => return Err("level 0 is the clay's own detail cell: leave cell out (or name a finer level)".into()),
        (0, None) => {}
        (_, None) => {
            o.insert("cell".into(), json!(at));
        }
        (_, Some(c)) if !same(c, at) => return Err(format!("cell {c} is not level {level}'s cell {at}: give one of level or cell")),
        _ => {}
    }
    Ok(())
}

/// A call's `level` (`sculpt`, `stroke_edit replace`): the default for its
/// strokes without one.
pub fn call_level(args: &Value) -> Result<Option<u8>, String> {
    match args.get("level").filter(|l| !l.is_null()) {
        None => Ok(None),
        Some(l) => Ok(Some(l.as_u64().filter(|l| *l <= u8::MAX as u64).ok_or("level must be a whole number from 0 (detail_level lists them)")? as u8)),
    }
}

/// Whether a stroke whose `cell` is `cell` grows the clay's detail regions
/// (Sculptris Pro, as `sculpt` does before applying it): a preview, which
/// cannot relay the clay, then shows it only roughly.
pub fn grows_regions(clay: &Clay, cell: Option<f32>, stroke: &crate::brush::resolve::Resolved) -> bool {
    let Some(cell) = cell else { return false };
    let (lo, hi) = stroke.dabs.iter().flat_map(|d| d.points.iter().enumerate().map(move |(k, p)| (*p, d.radii.as_ref().and_then(|r| r.get(k).copied()).unwrap_or(d.radius)))).fold((glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN)), |(a, b), (p, r)| (a.min(p - r), b.max(p + r)));
    let now = clay.detail_regions();
    crate::clay::sculptris::after_stroke(&now, lo, hi, cell) != now
}

/// Reads a stack's kept cells: at most `MAX_LEVELS` finite cells of at
/// least `MIN_DETAIL_CELL`, coarse first, no two the same (a file's
/// others are dropped, not refused). The list is read element by element.
pub(crate) fn read_kept<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<f32>, D::Error> {
    Ok(tidy(CappedCells::deserialize(d)?.0))
}

/// At most `MAX_LEVELS * 4` cells read; the rest skipped unread.
struct CappedCells(Vec<f32>);

impl<'de> Deserialize<'de> for CappedCells {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = CappedCells;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a list of detail cells")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element::<Option<f32>>()? {
                    if let Some(v) = v {
                        out.push(v);
                    }
                    if out.len() >= MAX_LEVELS * 4 {
                        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                        break;
                    }
                }
                Ok(CappedCells(out))
            }
        }
        d.deserialize_seq(Visit)
    }
}

/// Cells as a stack keeps them: valid ones, coarse first, distinct, at
/// most `MAX_LEVELS`.
pub(crate) fn tidy(mut cells: Vec<f32>) -> Vec<f32> {
    cells.retain(|c| c.is_finite() && *c >= MIN_DETAIL_CELL);
    cells.sort_by(|a, b| b.total_cmp(a));
    cells.dedup_by(|a, b| same(*a, *b));
    cells.truncate(MAX_LEVELS);
    cells
}

/// A level for the reply: its index, cell and how many detail regions
/// refine to it.
pub fn json(clay: &Clay) -> Value {
    let cells = cells(clay);
    let active = active(clay);
    let regions = clay.detail_regions();
    json!({
        "levels": cells.iter().enumerate().map(|(k, c)| json!({ "level": k, "cell": c, "regions": if k == 0 { 0 } else { regions.iter().filter(|r| same(r.cell, *c)).count() }, "active": k == active })).collect::<Vec<_>>(),
        "level": active,
        "active_cell": cells.get(active),
    })
}
