//! The `detail_level` tool (docs/SCULPT_PLAN.md 4.2, Stage 8a): a clay's
//! detail levels (`levels.rs`). It lists them, adds a finer one and picks
//! the active one, which `sculpt` strokes without `level` work at. The
//! levels and the active one are saved with the clay, so the same call
//! does the same whatever the editor shows. A new level is an edit (an
//! undo step); the active one is selection-like state: never an undo step,
//! and undo and redo keep it. D and Shift+D in the editor send this tool.

use serde_json::{Value, json};

use super::ClideApp;
use super::tools::text;
use crate::clay::MIN_DETAIL_CELL;
use crate::levels::{self, MAX_LEVELS};
use crate::mcp::ToolResult;
use crate::scene::Scene;

fn cell_arg(args: &Value, key: &str) -> Result<Option<f32>, String> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_f64().map(|c| c as f32).filter(|c| c.is_finite() && *c > 0.0).map(Some).ok_or_else(|| format!("{key} must be a cell size > 0")),
    }
}

impl ClideApp {
    /// `detail_level`.
    pub(super) fn detail_level(&mut self, args: &Value) -> ToolResult {
        let (id, _) = self.clay_of(args)?;
        let (add, cell) = (cell_arg(args, "add")?, cell_arg(args, "cell")?);
        let level = match args.get("level").filter(|v| !v.is_null()) {
            None => None,
            Some(v) => Some(v.as_u64().ok_or("level must be a whole number from 0")? as usize),
        };
        if level.is_some() && cell.is_some() {
            return Err("give level or cell, not both".into());
        }
        // A list reads the clay as it is: no settle, no edit.
        if add.is_none() && level.is_none() && cell.is_none() {
            return self.levels_reply(id, false);
        }
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        let coarse = levels::coarse(clay).ok_or("the clay has no detail cell yet: give it blobs (edit_clay)")?;
        let cells = levels::cells(clay);
        // The stack's own list: cells a coarser detail_cell hides stay
        // kept (they come back if it is made finer again).
        let stack = clay.sculpt.as_ref().map(|s| (s.data().levels.clone(), s.data().level));
        let mut kept = stack.as_ref().map_or_else(Vec::new, |s| s.0.clone());
        for c in [add, cell].into_iter().flatten() {
            if cells.iter().any(|l| (l - c).abs() <= 1e-4 * l.max(c)) {
                continue;
            }
            if c < MIN_DETAIL_CELL || c >= coarse * 0.99 {
                return Err(format!("a level's cell is {MIN_DETAIL_CELL} or more and finer than the clay's own cell {coarse} (level 0; edit_clay detail_cell changes that)"));
            }
            if kept.len() >= MAX_LEVELS {
                return Err(format!("the clay keeps {MAX_LEVELS} levels besides its own cell already (counting any a coarser detail_cell hides)"));
            }
            kept.push(c);
        }
        let kept = levels::tidy(kept);
        // The levels as listed (the kept cells finer than level 0).
        let all: Vec<f32> = std::iter::once(coarse).chain(kept.iter().copied().filter(|l| *l < coarse * 0.99)).collect();
        let find = |c: f32| all.iter().position(|l| (l - c).abs() <= 1e-4 * l.max(c)).expect("added");
        let pick = match (level, cell, add) {
            (Some(l), _, _) => Some(if l < all.len() { l } else { return Err(format!("no detail level {l}: the clay has levels 0 to {}", all.len() - 1)) }),
            (None, Some(c), _) | (None, None, Some(c)) => Some(find(c)),
            _ => None,
        };
        let active = match pick {
            Some(0) => None,
            Some(k) => Some(all[k]),
            None => stack.as_ref().and_then(|s| s.1),
        };
        let (was_kept, was_active) = stack.unwrap_or_default();
        if was_kept != kept {
            // A new level is an edit (and an undo step), saved with the
            // clay: the clay's ledgers settle first, as for any edit.
            let mut clay = self.layered_clay(id)?;
            let s = crate::ledger::stack_mut(&mut clay);
            (s.levels, s.level) = (kept, active);
            self.put_layered(id, clay)?;
            return self.levels_reply(id, true);
        } else if was_active != active {
            // Only the active level: selection-like state, saved with the
            // clay but never an undo step (docs/SCULPT_PLAN.md 3). It
            // changes no replay (each record keeps its own level), so
            // nothing settles; the step's base takes it too, so the call
            // records no step.
            let clay = self.scene.get_mut(id).and_then(|o| o.clay.as_mut()).ok_or("not a clay object")?;
            let before = clay.sculpt.clone();
            crate::ledger::stack_mut(clay).level = active;
            let after = clay.sculpt.clone();
            // Stacks compare by identity: the base shares the new one when
            // it had the old one (else it takes the level on its own).
            match self.step_base.get_mut(id).and_then(|o| o.clay.as_mut()) {
                Some(base) if base.sculpt == before => base.sculpt = after,
                _ => set_active_level(&mut self.step_base, id, active),
            }
            self.dirty = true;
        }
        self.levels_reply(id, false)
    }

    /// The reply: the clay's levels as they are now, and the undo step the
    /// call makes (null when it made none: a list or a pick).
    fn levels_reply(&self, id: u64, edit: bool) -> ToolResult {
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("object vanished")?;
        let mut out = levels::json(clay);
        out["target"] = json!(id);
        out["step"] = if edit { json!(self.history.current() + 1) } else { Value::Null };
        out["next"] = json!("sculpt strokes without level work at the active level: they write it and the coarser ones, and carry the finer detail along. A level above 0 grows a detail region of its cell over each stroke (Sculptris Pro).");
        Ok(text(out))
    }
}

/// Every clay's active level cell (`StackData::level`), by object id: what
/// undo and redo keep, as they keep the playing clips.
pub(super) fn active_levels(scene: &Scene) -> Vec<(u64, Option<f32>)> {
    scene.objects.iter().filter_map(|o| Some((o.id, o.clay.as_ref()?.sculpt.as_ref()?.data().level))).collect()
}

/// Puts these active levels back (on clays that still list the level; a
/// clay whose stack is new or gone is left as it is).
pub(super) fn set_active_levels(scene: &mut Scene, active: &[(u64, Option<f32>)]) {
    for (id, level) in active {
        let Some(clay) = scene.get(*id).and_then(|o| o.clay.as_ref()) else { continue };
        let listed = level.is_none_or(|l| levels::cells(clay).iter().skip(1).any(|c| (c - l).abs() <= 1e-4 * c.max(l)));
        if listed {
            set_active_level(scene, *id, *level);
        }
    }
}

/// Sets one clay's active level cell when its stack holds another.
fn set_active_level(scene: &mut Scene, id: u64, level: Option<f32>) {
    if let Some(clay) = scene.get_mut(id).and_then(|o| o.clay.as_mut())
        && clay.sculpt.as_ref().is_some_and(|s| s.data().level != level)
    {
        crate::ledger::stack_mut(clay).level = level;
    }
}

#[cfg(test)]
mod tests;
