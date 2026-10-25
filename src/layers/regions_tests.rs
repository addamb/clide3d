//! Sculptris Pro relayouts as `Regions` records (docs/SCULPT_PLAN.md
//! Stage 6 follow-up): each region change a stroke's `cell` makes is kept
//! as the regions, not the grid; it replays bit for bit on any thread
//! count and through a save, and a long drag on a large clay stays far
//! inside the saved budget.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::resolve::ResolveCtx;
use crate::clay::sculptris::after_stroke;
use crate::ledger::{Edit, Meta, RecordOp};

fn sphere(cell: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"sphere","name":"ball","center":[0,0,0],"radius":1.0,"blend":0}], "resolution":48, "detail_cell":cell})).unwrap()
}

/// Commits object-space strokes to `layer` as the sculpt tool does.
fn sculpt_on(clay: &mut Clay, layer: &str, strokes: &[Value]) {
    let meta = Meta::agent("sculpt", None);
    for (spec, r) in crate::sculpt::prepare(strokes, None, &ResolveCtx::local()).unwrap() {
        let c = {
            let v = view_for::<Detail>(clay, layer).unwrap();
            ledger::commit::<Detail>(&v, &ledger::ledger(&v), &meta, &spec, &r, false, None).unwrap()
        };
        install::<Detail>(clay, layer, c.ledger, c.grid);
    }
}

/// One Sculptris stroke as the sculpt tool runs it: the regions grow over
/// the stroke's box (recorded), then the stroke lands on `layer`.
/// Returns whether the regions changed.
fn sculptris(clay: &mut Clay, layer: &str, p: Vec3, r: f32, cell: f32) -> Result<bool, String> {
    let now = clay.detail_regions();
    let next = after_stroke(&now, p - r, p + r, cell);
    let grew = next != now;
    if grew {
        set_regions_recorded(clay, &next, &Meta::agent("sculpt", None), None)?;
        clay.fine_check()?;
        sync_within_budget(clay)?;
    }
    sculpt_on(clay, layer, &[json!({"brush":"inflate","point":p.to_array(),"radius":r,"strength":0.4})]);
    check_budget(clay)?;
    Ok(grew)
}

/// A drag over the sphere's top, each stroke half a radius past the last
/// (so each grows the region), alternating layers when there is "L".
fn drag(clay: &mut Clay, n: usize, layered: bool) -> usize {
    let (r, cell) = (0.06f32, 0.008);
    let mut grew = 0;
    for k in 0..n {
        let x = -0.4 + k as f32 * r * 0.5;
        let p = Vec3::new(x, (1.0 - x * x).sqrt(), 0.1 * (k as f32).sin());
        let layer = if layered && k % 2 == 1 { "L" } else { BASE };
        grew += sculptris(clay, layer, p, r, cell).unwrap() as usize;
    }
    grew
}

fn kinds<G: Grid>(l: &Ledger<G>) -> (usize, usize) {
    let regions = l.records().iter().filter(|r| matches!(r.op, RecordOp::Regions { .. })).count();
    let opaque = l.records().iter().filter(|r| matches!(r.op, RecordOp::Opaque { .. })).count();
    (regions, opaque)
}

/// A plain clay (base only) a Sculptris drag grew, on `threads` threads.
fn grown(threads: usize) -> Clay {
    crate::clay::with_threads(threads, || {
        let mut clay = sphere(0.02);
        sculpt_on(&mut clay, BASE, &[json!({"brush":"standard","points":[[0.0,1.0,0.0],[0.2,0.97,0.1]],"radius":0.15,"strength":0.4})]);
        assert!(drag(&mut clay, 12, false) >= 6, "the drag hardly grew its region");
        // A drag at a second cell (its own region), then more strokes.
        for k in 0..4 {
            let p = Vec3::new(0.5, 0.86, -0.1 + 0.03 * k as f32);
            sculptris(&mut clay, BASE, p, 0.04, 0.005).unwrap();
        }
        sculpt_on(&mut clay, BASE, &[json!({"brush":"smooth","point":[0.0,1.0,0.0],"radius":0.3,"strength":0.5})]);
        clay
    })
}

#[test]
fn sculptris_relayouts_replay_bit_for_bit_on_one_and_twelve_threads() {
    let (one, twelve) = (grown(1), grown(12));
    for clay in [&one, &twelve] {
        let base = ledger::ledger::<Detail>(clay);
        // Relayouts are regions records, never whole grids.
        let (regions, opaque) = kinds(&base);
        assert!(regions >= 7 && opaque == 0, "{regions} regions records, {opaque} opaque");
        let levels = clay.detail.as_ref().unwrap().grid().levels();
        assert_eq!(levels.len(), 2, "one region per cell");
        assert!(levels.iter().all(|l| l.grid.brick_count() > 0), "the fine levels hold the drags");
        // Replayed from empty (no checkpoints), on one and on twelve
        // threads: the live grid bit for bit.
        for threads in [1, 12] {
            crate::clay::with_threads(threads, || {
                let g = ledger::replay(&base, clay, base.len(), None, |_, _| {}).unwrap();
                assert_eq!(Detail::hash(g.as_ref()), Detail::hash(clay.detail.as_ref()), "replay on {threads} threads");
            });
        }
        // Every record's own result hash replays.
        let e = ledger::edit::<Detail>(clay, &base, Edit::Replay(0), None).unwrap();
        assert!(e.mismatched.is_empty(), "{:?}", e.mismatched);
    }
    // The same grids, records and result hashes whatever the thread count.
    assert_eq!(Detail::hash(one.detail.as_ref()), Detail::hash(twelve.detail.as_ref()));
    let digests = |c: &Clay| ledger::ledger::<Detail>(c).records().iter().map(|r| (r.digest(), r.result_hash)).collect::<Vec<_>>();
    assert_eq!(digests(&one), digests(&twelve));
}

#[test]
fn sculptris_relayouts_lay_layers_and_morphs_out_as_before() {
    // A layered clay with a morph: each relayout recorded as regions gives
    // the grids `Clay::set_detail_regions` (the whole-grid path) gives,
    // bit for bit, base, layer and morph, and keeps the morph sharing
    // what it shared.
    let mut clay = sphere(0.02);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"standard","points":[[0.0,1.0,0.0],[0.2,0.97,0.1]],"radius":0.15,"strength":0.4})]);
    ledger::stack_mut(&mut clay).layers.push(Layer::new("L", Kind::Sculpt, 0.7));
    sculpt_on(&mut clay, "L", &[json!({"brush":"inflate","point":[-0.3,0.95,0.0],"radius":0.2,"strength":0.3})]);
    let m = snapshot(&clay, "m");
    ledger::stack_mut(&mut clay).morphs.push(m);
    let (r, cell) = (0.06f32, 0.008);
    let mut grew = 0;
    for k in 0..10 {
        let x = -0.4 + k as f32 * r * 0.5;
        let p = Vec3::new(x, (1.0 - x * x).sqrt(), 0.0);
        let next = after_stroke(&clay.detail_regions(), p - r, p + r, cell);
        if next != clay.detail_regions() {
            let mut before = clay.clone();
            before.set_detail_regions(&next).unwrap();
            let shared = clay.sculpt.as_ref().unwrap().data().morphs[0].base == clay.detail;
            let made = set_regions_recorded(&mut clay, &next, &Meta::agent("sculpt", None), None).unwrap();
            assert_eq!(made.len(), 2, "one record on base, one on the layer");
            sync_within_budget(&mut clay).unwrap();
            let s = clay.sculpt.as_ref().unwrap().data();
            let b = before.sculpt.as_ref().unwrap().data();
            assert_eq!(Detail::hash(clay.detail.as_ref()), Detail::hash(before.detail.as_ref()), "base, relayout {k}");
            assert_eq!(Detail::hash(s.layers[0].detail.as_ref()), Detail::hash(b.layers[0].detail.as_ref()), "layer, relayout {k}");
            assert_eq!(Detail::hash(s.morphs[0].base.as_ref()), Detail::hash(b.morphs[0].base.as_ref()), "morph base, relayout {k}");
            assert_eq!(Detail::hash(s.morphs[0].layers[0].detail.as_ref()), Detail::hash(b.morphs[0].layers[0].detail.as_ref()), "morph layer, relayout {k}");
            assert_eq!(s.morphs[0].base == clay.detail, shared, "the morph shares base as before");
            grew += 1;
        }
        let layer = if k % 2 == 1 { "L" } else { BASE };
        sculpt_on(&mut clay, layer, &[json!({"brush":"inflate","point":p.to_array(),"radius":r,"strength":0.4})]);
    }
    assert!(grew >= 5, "{grew}");
    let s = clay.sculpt.as_ref().unwrap().data();
    assert_eq!(kinds(&s.base), (grew, 0));
    assert_eq!(kinds(&s.layers[0].strokes), (grew, 0));
    check_budget(&clay).unwrap();
}

#[test]
fn sculptris_relayouts_save_load_mirror_and_edit() {
    let clay = grown(12);
    let base = ledger::ledger::<Detail>(&clay);
    // Saved and read back: the records, their hashes and the exact grids.
    let text = serde_json::to_string(&clay).unwrap();
    assert!(text.contains("\"regions\":{\"regions\":"), "regions records in the file");
    let mut back: Clay = serde_json::from_str(&text).unwrap();
    back.sanitize();
    assert!(back.sculpt.is_some(), "a sane stack reads back");
    ledger::after_load(&mut back);
    let b = ledger::ledger::<Detail>(&back);
    assert_eq!((b.verified(), b.head()), (Some(true), base.head()));
    assert!(b.records().iter().zip(base.records()).all(|(x, y)| **x == **y && x.digest() == y.digest()));
    assert_eq!(Detail::hash(back.detail.as_ref()), Detail::hash(clay.detail.as_ref()), "the exact grid after load");
    assert!(b.in_sync(back.detail.as_ref()));
    // A file's regions records are bounded while read: more than 16
    // regions, or one with no size, fail to load.
    let v: Value = serde_json::from_str(&text).unwrap();
    let at = v["sculpt"]["base"]["records"].as_array().unwrap().iter().position(|r| r["op"].get("regions").is_some()).unwrap();
    let bad = |edit: &dyn Fn(&mut Value)| {
        let mut f = v.clone();
        edit(&mut f["sculpt"]["base"]["records"][at]["op"]["regions"]);
        serde_json::from_value::<Clay>(f).is_err()
    };
    assert!(!bad(&|_| {}), "the file itself loads");
    assert!(bad(&|op| {
        let one = op["regions"][0].clone();
        op["regions"] = json!(vec![one; MAX_REGIONS_IN_TEST + 1]);
    }), "17 regions");
    assert!(bad(&|op| op["regions"][0]["radii"] = json!([0.1, -1.0, 0.1])), "negative radii");
    assert!(bad(&|op| op["regions"][0]["cell"] = json!(0.0)), "a zero cell");
    // Mirrored: the regions records mirror with the grid.
    let m = clay.mirrored_x();
    let ml = ledger::ledger::<Detail>(&m);
    let regions = |l: &Ledger<Detail>| match &l.records().iter().rev().find(|r| matches!(r.op, RecordOp::Regions { .. })).unwrap().op {
        RecordOp::Regions { regions, .. } => regions.clone(),
        _ => unreachable!(),
    };
    let (a, b) = (regions(&base), regions(&ml));
    assert!(a.iter().zip(&b).all(|(x, y)| x.center.x == -y.center.x && x.center.y == y.center.y && x.radii == y.radii && x.cell == y.cell));
    // Replayed, the mirrored records give the mirrored grid, regions and
    // all (within float noise: the field is read at mirrored points).
    let e = ledger::edit::<Detail>(&m, &ml, Edit::Rebase, None).unwrap();
    let (g, want) = (e.grid.as_ref().unwrap().grid(), m.detail.as_ref().unwrap().grid());
    assert!(g.levels().iter().zip(want.levels()).all(|(a, b)| a.region == b.region) && g.levels().len() == want.levels().len());
    let mut worst = 0.0f32;
    ledger::detail_changes(e.grid.as_ref(), m.detail.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= 1e-5, "mirror replay off by {worst}");
    // Squash over a relayout: one opaque record (the whole grid), and the
    // ledger still ends at the clay.
    let ids: Vec<u64> = base.records().iter().map(|r| r.id).collect();
    let s = ledger::edit::<Detail>(&clay, &base, Edit::Squash(ids[1..5].to_vec()), None).unwrap();
    // (The squashed grid is kept as a file reads it back: 1/256 cell.)
    let mut worst = 0.0f32;
    ledger::detail_changes(s.grid.as_ref(), clay.detail.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= 0.02 / 256.0 + 1e-7, "squash off by {worst}");
    assert!(s.ledger.records().iter().any(|r| r.holds_grid()));
    // Deleting the first relayout: the strokes after it replay on the
    // layout before it until the next one, which lays the grid out again.
    let first = base.records().iter().find(|r| matches!(r.op, RecordOp::Regions { .. })).unwrap().id;
    let d = ledger::edit::<Detail>(&clay, &base, Edit::Delete(vec![first]), None).unwrap();
    assert_eq!(d.ledger.len(), base.len() - 1);
    assert_eq!(d.grid.as_ref().unwrap().grid().levels().len(), clay.detail.as_ref().unwrap().grid().levels().len());
    // Disabled, the same.
    let off = ledger::edit::<Detail>(&clay, &base, Edit::Enable(vec![first], false), None).unwrap();
    assert_eq!(Detail::hash(off.grid.as_ref()), Detail::hash(d.grid.as_ref()));
    // Scaling one is refused (only strokes scale).
    assert!(ledger::edit::<Detail>(&clay, &base, Edit::Scale(vec![first], 0.5), None).is_err());
}

const MAX_REGIONS_IN_TEST: usize = crate::clay::MAX_REGIONS;

#[test]
fn a_long_sculptris_drag_on_a_large_clay_stays_inside_the_saved_budget() {
    // A plain clay (no layers or morphs) sculpted over much of its
    // surface: 60,000 coarse bricks, about 62 MB in a file.
    let voxel = 0.02;
    let mut clay = sphere(voxel);
    let mut d = Detail::new(voxel);
    let g = d.grid_mut();
    let side = (60_000f32).sqrt().ceil() as i32;
    for k in 0..60_000i32 {
        let (x, z) = (k % side - side / 2, k / side - side / 2);
        for i in 0..512 {
            let l = [i % 8, i / 8 % 8, i / 64];
            g.set([x * 8 + l[0], -40 * 8 + l[1], z * 8 + l[2]], voxel * 0.01 * ((i as f32 * 0.37).sin() + 1.5));
        }
    }
    clay.detail = Some(d);
    ledger::stack_mut(&mut clay);
    assert!(sync_within_budget(&mut clay).unwrap(), "the grid is recorded once");
    let start = saved_bytes(&clay);
    let grid = start / 2;
    // Before, every stroke that grew the region recorded the whole grid:
    // this drag would have passed MAX_SAVED_BYTES some 17 strokes in.
    let n = 40;
    assert!(grid * n as u64 > MAX_SAVED_BYTES, "{grid} x {n}");
    let grew = drag(&mut clay, n, false);
    assert!(grew >= 20, "{grew} of {n} strokes grew the region");
    let (regions, opaque) = kinds(&ledger::ledger::<Detail>(&clay));
    assert_eq!((regions, opaque), (grew, 1), "the relayouts keep their regions only");
    let end = saved_bytes(&clay);
    eprintln!("{n}-stroke Sculptris drag on a 60,000-brick clay: {grew} relayouts, saved {} MB -> {} MB", start >> 20, end >> 20);
    // What grew is the strokes' own bricks (the fine region's and the
    // coarse ones they touch), not a grid per relayout.
    assert!(end < start + (8 << 20), "{start} -> {end}");
    check_saved_within(&clay, MAX_SAVED_BYTES).unwrap();
    // And it replays to the live grid.
    let base = ledger::ledger::<Detail>(&clay);
    let g = ledger::replay(&base, &clay, base.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(g.as_ref()), Detail::hash(clay.detail.as_ref()));
}

#[test]
fn the_saved_budget_counts_the_mask_and_groups_and_their_outside_edits() {
    let mut clay = sphere(0.02);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","point":[0.0,1.0,0.0],"radius":0.2,"strength":0.3})]);
    let start = saved_bytes(&clay);
    // 10 mask bricks and 6 group bricks, each kept whole by an outside
    // edit (as a mask from a smart material's channel is).
    let mut mask = SculptMask::new(0.02);
    for k in 0..10 {
        mask.grid_mut().set([k * 8, 50, 0], 200);
    }
    let mut groups = Groups::new(0.02);
    for k in 0..6 {
        groups.data_mut().set([k * 8, 50, 0], 3);
    }
    let meta = Meta::agent("mask", None);
    let c = ledger::commit_grid::<SculptMask>(&clay, &ledger::ledger(&clay), &meta, "from a channel", Some(mask), None).unwrap();
    ledger::install(&mut clay, c.ledger, c.grid);
    let c = ledger::commit_grid::<Groups>(&clay, &ledger::ledger(&clay), &meta, "from a channel", Some(groups), None).unwrap();
    ledger::install(&mut clay, c.ledger, c.grid);
    // Each grid twice: the clay's own and its record's (as a file holds
    // them), at 4 key bytes per grid and 12 + 512 (mask) or 12 + 1,024
    // (groups) per brick.
    let (m, g) = (4 + 10 * (12 + 512), 4 + 6 * (12 + 1024));
    assert_eq!(saved_bytes(&clay), start + 2 * (m + g));
}
