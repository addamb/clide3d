//! The mask and polygroup ledgers' grids (`mask.rs`, `groups.rs`): how
//! they hash, apply strokes and actions, and measure changes.

use std::sync::Mutex;

use glam::Vec3;
use xxhash_rust::xxh3::xxh3_128;

use super::{Applied, Checkpoints, DiffStats, Grid, Hash128, Hasher, Ledger, ReplayCache, StackData, Tally, clay_context, surface_hash};
use crate::brush::resolve::Resolved;
use crate::brush::spec::Brush;
use crate::clay::{BRICK_VALUES, Clay};
use crate::groups::{GroupOp, Groups};
use crate::mask::{MaskOp, Select, SculptMask};

/// Changed grid points between two brick maps: `f(point)` per change;
/// returns the bricks that differ.
fn changes<T: PartialEq + Default + Copy>(voxel: f32, keys: impl Iterator<Item = [i32; 3]>, before: impl Fn(&[i32; 3]) -> Option<[T; BRICK_VALUES]>, after: impl Fn(&[i32; 3]) -> Option<[T; BRICK_VALUES]>, mut f: impl FnMut(Vec3)) -> usize {
    let keys: std::collections::BTreeSet<[i32; 3]> = keys.collect();
    let mut bricks = 0;
    for key in keys {
        let (x, y) = (before(&key), after(&key));
        let mut any = false;
        for i in 0..BRICK_VALUES {
            let (u, v) = (x.map_or(T::default(), |x| x[i]), y.map_or(T::default(), |y| y[i]));
            if u != v {
                any = true;
                let l = [i as i32 % 8, i as i32 / 8 % 8, i as i32 / 64];
                f(Vec3::new((key[0] * 8 + l[0]) as f32, (key[1] * 8 + l[1]) as f32, (key[2] * 8 + l[2]) as f32) * voxel);
            }
        }
        bricks += any as usize;
    }
    bricks
}

/// What a mask or group record reads besides its grid: the blobs and the
/// sculpted surface (its band).
fn band_context(tag: &str, clay: &Clay) -> Hash128 {
    Hasher::new(tag).hash(clay_context(clay)).hash(surface_hash(clay)).finish()
}

impl Grid for SculptMask {
    const LAYER: &'static str = "mask";
    type Action = MaskOp;

    fn act(clay: &Clay, grid: Option<&Self>, action: &MaskOp) -> Result<Option<Self>, String> {
        crate::mask::act(clay, grid, action, &|h| super::frozen_groups(clay, h))
    }

    fn mirror_action(action: &MaskOp) -> MaskOp {
        action.mirrored_x()
    }

    fn remap_action(action: &MaskOp, remap: &dyn Fn(Hash128) -> Hash128) -> MaskOp {
        match action {
            MaskOp::Select { select: Select::Groups { ids, groups }, mode } => MaskOp::Select { select: Select::Groups { ids: ids.clone(), groups: remap(*groups) }, mode: *mode },
            a => a.clone(),
        }
    }

    fn action_reads(action: &MaskOp) -> Vec<Hash128> {
        match action {
            MaskOp::Select { select: Select::Groups { groups, .. }, .. } => vec![*groups],
            _ => Vec::new(),
        }
    }

    fn sum(grid: Option<&Self>) -> u128 {
        let Some(m) = grid else { return 0 };
        m.grid().bricks().fold(0u128, |s, (key, values)| {
            let mut buf = Vec::with_capacity(12 + BRICK_VALUES);
            key.iter().for_each(|c| buf.extend(c.to_le_bytes()));
            buf.extend(values.iter());
            s.wrapping_add(xxh3_128(&buf))
        })
    }

    fn sum_change(_: Option<&Self>, _: Option<&Self>) -> Option<u128> {
        None
    }

    fn seal(grid: Option<&Self>, sum: u128) -> Hash128 {
        let Some(m) = grid else { return Hasher::new("mask:none").finish() };
        Hasher::new("mask").bytes(&m.grid().voxel.to_bits().to_le_bytes()).bytes(&sum.to_le_bytes()).finish()
    }

    fn current(clay: &Clay) -> Option<&Self> {
        clay.sculpt_mask.as_ref()
    }

    fn put(clay: &mut Clay, grid: Option<Self>) {
        clay.sculpt_mask = grid;
    }

    fn context(clay: &Clay) -> Hash128 {
        band_context("mask context", clay)
    }

    fn apply(clay: &Clay, grid: Option<&Self>, stroke: &Resolved) -> Result<Applied<Self>, String> {
        if stroke.params.brush != Brush::MaskPaint {
            return Err("the mask layer takes mask_paint strokes".into());
        }
        Ok(Applied { grid: crate::mask::paint(clay, grid, stroke)?, radius: stroke.dabs.first().map_or(0.0, |d| d.radius) })
    }

    fn blend(_: Option<&Self>, _: Option<&Self>, t: f32) -> Result<Option<Self>, String> {
        Err(format!("mask records cannot be scaled (factor {t}); disable or delete them instead"))
    }

    fn delta(_: Option<&Self>, _: &Self) -> Option<Self> {
        None
    }

    fn add(_: Option<&Self>, _: &Self) -> Result<Option<Self>, String> {
        Err("the mask keeps outside edits whole".into())
    }

    fn stats(_: &Clay, before: Option<&Self>, after: Option<&Self>, _: bool) -> DiffStats {
        let mut t = Tally::default();
        let voxel = after.or(before).map_or(1.0, |m| m.grid().voxel);
        let (b, a) = (before.map(|m| m.grid()), after.map(|m| m.grid()));
        let keys = a.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k)).chain(b.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k)));
        let bricks = changes(voxel, keys, |k| b.and_then(|g| g.brick(k)).map(|x| **x), |k| a.and_then(|g| g.brick(k)).map(|x| **x), |p| t.add(p, 0.0));
        t.stats(bricks)
    }

    fn cell(grid: Option<&Self>) -> Option<f32> {
        grid.map(|m| m.grid().voxel)
    }

    fn mirror(&self) -> Self {
        self.mirrored_x()
    }

    fn ledger(stack: &StackData) -> &Ledger<Self> {
        &stack.mask
    }

    fn ledger_mut(stack: &mut StackData) -> &mut Ledger<Self> {
        &mut stack.mask
    }

    fn cache(cache: &ReplayCache) -> &Mutex<Checkpoints<Self>> {
        &cache.mask
    }
}

impl Grid for Groups {
    const LAYER: &'static str = "groups";
    type Action = GroupOp;

    fn act(clay: &Clay, grid: Option<&Self>, action: &GroupOp) -> Result<Option<Self>, String> {
        crate::groups::act(clay, grid, action, &|h| super::frozen_mask(clay, h))
    }

    fn remap_action(action: &GroupOp, remap: &dyn Fn(Hash128) -> Hash128) -> GroupOp {
        match action {
            GroupOp::FromMask { threshold, id, mask } => GroupOp::FromMask { threshold: *threshold, id: *id, mask: remap(*mask) },
            a => a.clone(),
        }
    }

    fn action_reads(action: &GroupOp) -> Vec<Hash128> {
        match action {
            GroupOp::FromMask { mask, .. } => vec![*mask],
            _ => Vec::new(),
        }
    }

    fn sum(grid: Option<&Self>) -> u128 {
        let Some(g) = grid else { return 0 };
        g.data().bricks().fold(0u128, |s, (key, values)| {
            let mut buf = Vec::with_capacity(12 + BRICK_VALUES * 2);
            key.iter().for_each(|c| buf.extend(c.to_le_bytes()));
            values.iter().for_each(|v| buf.extend(v.to_le_bytes()));
            s.wrapping_add(xxh3_128(&buf))
        })
    }

    fn sum_change(_: Option<&Self>, _: Option<&Self>) -> Option<u128> {
        None
    }

    fn seal(grid: Option<&Self>, sum: u128) -> Hash128 {
        let Some(g) = grid else { return Hasher::new("groups:none").finish() };
        // Names and visibility are part of the groups (hidden groups stop
        // brushes).
        Hasher::new("groups").bytes(&g.data().voxel.to_bits().to_le_bytes()).bytes(&sum.to_le_bytes()).json(&g.data().info).finish()
    }

    fn current(clay: &Clay) -> Option<&Self> {
        clay.groups.as_ref()
    }

    fn put(clay: &mut Clay, grid: Option<Self>) {
        clay.groups = grid;
    }

    fn context(clay: &Clay) -> Hash128 {
        band_context("groups context", clay)
    }

    fn apply(clay: &Clay, grid: Option<&Self>, stroke: &Resolved) -> Result<Applied<Self>, String> {
        if stroke.params.brush != Brush::GroupPaint {
            return Err("the groups layer takes group_paint strokes".into());
        }
        Ok(Applied { grid: crate::groups::paint(clay, grid, stroke)?, radius: stroke.dabs.first().map_or(0.0, |d| d.radius) })
    }

    fn blend(_: Option<&Self>, _: Option<&Self>, t: f32) -> Result<Option<Self>, String> {
        Err(format!("group records cannot be scaled (factor {t}); disable or delete them instead"))
    }

    fn delta(_: Option<&Self>, _: &Self) -> Option<Self> {
        None
    }

    fn add(_: Option<&Self>, _: &Self) -> Result<Option<Self>, String> {
        Err("groups keep outside edits whole".into())
    }

    fn stats(_: &Clay, before: Option<&Self>, after: Option<&Self>, _: bool) -> DiffStats {
        let mut t = Tally::default();
        let voxel = after.or(before).map_or(1.0, |g| g.data().voxel);
        let (b, a) = (before.map(|g| g.data()), after.map(|g| g.data()));
        let keys = a.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k)).chain(b.into_iter().flat_map(|g| g.bricks().map(|(k, _)| *k)));
        let bricks = changes(voxel, keys, |k| b.and_then(|g| g.brick(k)).map(|x| **x), |k| a.and_then(|g| g.brick(k)).map(|x| **x), |p| t.add(p, 0.0));
        t.stats(bricks)
    }

    fn cell(grid: Option<&Self>) -> Option<f32> {
        grid.map(|g| g.data().voxel)
    }

    fn mirror(&self) -> Self {
        self.mirrored_x()
    }

    fn ledger(stack: &StackData) -> &Ledger<Self> {
        &stack.groups
    }

    fn ledger_mut(stack: &mut StackData) -> &mut Ledger<Self> {
        &mut stack.groups
    }

    fn cache(cache: &ReplayCache) -> &Mutex<Checkpoints<Self>> {
        &cache.groups
    }
}
