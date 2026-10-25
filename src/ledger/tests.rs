//! Stage 1b acceptance (docs/SCULPT_PLAN.md 6): records, replay from
//! checkpoints, save and load, Adjust Last, engine versions, the packed
//! file form and the scene clone guard.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::resolve::ResolveCtx;
use crate::brush::rng::Pcg32;
use crate::harness::{image, perf};

/// A unit sphere with a detail grid of `cell`.
fn sphere(cell: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"sphere","name":"ball","center":[0,0,0],"radius":1.0,"blend":0}], "resolution":48, "detail_cell":cell})).unwrap()
}

/// Mixed strokes over the sphere's upper half, seeded.
fn mixed_strokes(n: usize, seed: u64) -> Vec<Value> {
    let mut rng = Pcg32::new(seed, 11);
    let mut out = Vec::new();
    for i in 0..n {
        let mut on = || {
            let (a, b) = (rng.next_f32() * std::f32::consts::TAU, rng.next_f32() * 1.2);
            [b.sin() * a.cos(), b.cos(), b.sin() * a.sin()]
        };
        let (p, q) = (on(), on());
        let mid = [(p[0] + q[0]) * 0.5, (p[1] + q[1]) * 0.5, (p[2] + q[2]) * 0.5];
        let s = match i % 9 {
            0 => json!({"brush":"inflate","point":p,"radius":0.15,"strength":0.4}),
            1 => json!({"brush":"add","points":[p, mid],"radius":0.05}),
            2 => json!({"brush":"carve","points":[p, mid],"radius":0.04,"strength":0.6}),
            3 => json!({"brush":"smooth","point":p,"radius":0.15,"strength":0.5}),
            4 => json!({"brush":"standard","points":[p, mid],"radius":0.06,"strength":0.5}),
            5 => json!({"brush":"crease","points":[p, mid],"radius":0.05,"strength":0.01,"taper":[0.2,0.2]}),
            6 => json!({"brush":"flatten","point":p,"radius":0.12,"strength":0.5,"falloff":"flat"}),
            7 => json!({"brush":"grab","point":p,"radius":0.1,"delta":[0,0.02,0]}),
            _ => json!({"brush":"layer","point":p,"radius":0.1,"strength":0.01,"symmetry":{"axes":["x"]}}),
        };
        out.push(s);
    }
    out
}

/// Commits `strokes` (object space) one by one to the clay's base layer,
/// as the sculpt tool does; returns the committed records.
fn sculpt(clay: &mut Clay, strokes: &[Value], cache: Option<&ReplayCache>) -> Vec<Committed<Detail>> {
    let meta = Meta::agent("sculpt", None);
    let mut out = Vec::new();
    for (spec, r) in crate::sculpt::prepare(strokes, None, &ResolveCtx::local()).unwrap() {
        let c = commit::<Detail>(clay, &ledger(clay), &meta, &spec, &r, false, cache).unwrap();
        install(clay, c.ledger.clone(), c.grid.clone());
        out.push(c);
    }
    out
}

/// Bit-equal grids, or with `CLIDE_XPLAT=1` within 1/256 voxel.
fn assert_same(a: Option<&Detail>, b: Option<&Detail>, what: &str) {
    if std::env::var("CLIDE_XPLAT").is_ok_and(|v| v == "1") {
        let voxel = a.or(b).map_or(1.0, Detail::voxel);
        let mut worst = 0.0f32;
        detail_changes(a, b, |_, _, u, v| worst = worst.max((u - v).abs()));
        assert!(worst <= voxel / 256.0, "{what}: grids differ by {worst} (> 1/256 voxel)");
    } else {
        assert_eq!(Detail::hash(a), Detail::hash(b), "{what}");
    }
}

#[test]
fn replaying_two_hundred_strokes_gives_the_live_grid_and_selective_undo_matches_a_fresh_replay() {
    let mut clay = sphere(0.02);
    let cache = ReplayCache::default();
    let strokes = mixed_strokes(200, 3);
    let committed = sculpt(&mut clay, &strokes, Some(&cache));
    let l = ledger::<Detail>(&clay);
    assert_eq!(l.len(), 200);
    assert!(l.in_sync(clay.detail.as_ref()));
    assert_eq!(cache.checkpoints(), 6, "a checkpoint every 32 records");
    // From empty: the last result hash and the live grid.
    let fresh = replay(&l, &clay, l.len(), None, |_, _| {}).unwrap();
    assert_same(fresh.as_ref(), clay.detail.as_ref(), "replay from empty vs live");
    assert_eq!(Detail::hash(fresh.as_ref()), committed.last().unwrap().record.result_hash);
    // Every record's own hash replays too.
    let check = edit::<Detail>(&clay, &l, Edit::Replay(0), None).unwrap();
    assert!(check.mismatched.is_empty(), "mismatched {:?}", check.mismatched);
    assert_eq!(check.ledger.verified(), Some(true));

    // Disable record 57 (seq 57): replay from the checkpoint at 32 ...
    let id57 = l.records()[56].id;
    let t = std::time::Instant::now();
    let e = edit::<Detail>(&clay, &l, Edit::Enable(vec![id57], false), Some(&cache)).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!((e.from, e.replayed), (56, 200 - 32), "from the checkpoint at 32");
    assert!(!e.ledger.records()[56].enabled);
    // ... equals a fresh replay of the same records without 57.
    let mut without = l.data().clone();
    without.records.remove(56);
    without.rechain();
    let fresh = replay(&Ledger(Arc::new(without)), &clay, 199, None, |_, _| {}).unwrap();
    assert_same(e.grid.as_ref(), fresh.as_ref(), "disable 57 vs fresh replay without 57");
    assert_eq!(e.ledger.records().last().unwrap().result_hash, Detail::hash(e.grid.as_ref()));
    perf::log("ledger: disable record 57 of 200 and replay (2 cm cell)", ms, json!({ "replayed": e.replayed }));
    // Enabling it again gives the original grid back.
    let mut c2 = clay.clone();
    install(&mut c2, e.ledger.clone(), e.grid.clone());
    let back = edit::<Detail>(&c2, &e.ledger, Edit::Enable(vec![id57], true), Some(&cache)).unwrap();
    assert_same(back.grid.as_ref(), clay.detail.as_ref(), "re-enabled");
}

#[test]
fn save_and_load_keep_the_ledger_and_the_grid() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(24, 5), None);
    // An MRGB stroke records on both layers.
    let meta = Meta::agent("sculpt", Some("brow".into()));
    let (spec, r) = crate::sculpt::prepare(&[json!({"brush":"inflate","point":[0,1,0],"radius":0.2,"paint":{"color":[0.8,0.1,0.1]}})], None, &ResolveCtx::local()).unwrap().pop().unwrap();
    let c = commit::<Detail>(&clay, &ledger(&clay), &meta, &spec, &r, true, None).unwrap();
    assert!(c.record.stats.volume_delta.unwrap() > 0.0, "inflate adds volume");
    install(&mut clay, c.ledger, c.grid);
    let p = commit::<Paint>(&clay, &ledger(&clay), &meta, &spec, &r, false, None).unwrap();
    assert!(p.record.stats.changed_points > 0);
    install(&mut clay, p.ledger, p.grid);

    let text = serde_json::to_string(&clay).unwrap();
    let mut back: Clay = serde_json::from_str(&text).unwrap();
    // The file rounds offsets to 1/256 voxel.
    let voxel = clay.detail.as_ref().unwrap().voxel();
    let mut worst = 0.0f32;
    detail_changes(clay.detail.as_ref(), back.detail.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= voxel / 256.0, "saved offsets off by {worst}");
    after_load(&mut back);
    for (a, b) in [(ledger::<Detail>(&clay).records().to_vec(), ledger::<Detail>(&back).records().to_vec())] {
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| **x == **y), "base records differ after a round trip");
    }
    let (pa, pb) = (ledger::<Paint>(&clay), ledger::<Paint>(&back));
    assert!(pa.len() == 1 && pa.records().iter().zip(pb.records()).all(|(x, y)| **x == **y));
    assert_eq!(ledger::<Detail>(&back).head(), ledger::<Detail>(&clay).head());
    // The replay on load matched, and put the exact grid back.
    assert_eq!((ledger::<Detail>(&back).verified(), ledger::<Paint>(&back).verified()), (Some(true), Some(true)));
    assert_same(back.detail.as_ref(), clay.detail.as_ref(), "after load");
    assert_eq!(Paint::hash(back.paint.as_ref()), Paint::hash(clay.paint.as_ref()));
    assert!(ledger::<Detail>(&back).in_sync(back.detail.as_ref()));
}

#[test]
fn adjust_last_scales_the_newest_stroke_and_goes_stale_after_undo() {
    let mut clay = sphere(0.01);
    sculpt(&mut clay, &[json!({"brush":"inflate","point":[0.3,0.9,0.2],"radius":0.2,"strength":0.6})], None);
    let c = sculpt(&mut clay, &[json!({"brush":"standard","point":[0,1,0],"radius":0.15,"strength":0.8})], None).pop().unwrap();
    let last = LastDelta { layer: "base".into(), head: c.ledger.head(), record: c.record.id, source: Source::Agent, before: c.before.clone(), after: c.grid.clone() };
    let (before, after) = (c.before.clone(), c.grid.clone());
    let original = c.record.stats.max_disp;
    let scaled = |clay: &Clay, f: f32, tag: &LastDelta| {
        let (l, g, r, tag) = adjust_last(clay, tag, f).unwrap();
        let mut out = clay.clone();
        install(&mut out, l, g);
        (out, r, tag)
    };
    // Factor 0: the grid before the stroke.
    let (zero, _, tag) = scaled(&clay, 0.0, &last);
    let mut worst = 0.0f32;
    detail_changes(zero.detail.as_ref(), before.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= 1e-6, "factor 0 is off by {worst}");
    // Factor 1 (from the adjusted state: the tag follows each adjust).
    let (one, _, tag) = scaled(&zero, 1.0, &tag);
    assert_same(one.detail.as_ref(), after.as_ref(), "factor 1");
    // Factor 0.5: the midpoint, and half the displacement.
    let (half, r, _) = scaled(&one, 0.5, &tag);
    let mut worst = 0.0f32;
    let empty = Detail::new(0.01);
    let b = before.as_ref().unwrap_or(&empty).grid();
    detail_changes(before.as_ref(), half.detail.as_ref(), |p, _, _, v| {
        let mid = (b.offset(p) + after.as_ref().unwrap().grid().offset(p)) * 0.5;
        worst = worst.max((v - mid).abs());
    });
    assert!(worst <= 1e-4, "factor 0.5 is {worst} off the midpoint");
    assert!((r.stats.max_disp / original - 0.5).abs() <= 0.03, "max_disp {} vs {original}", r.stats.max_disp);
    assert_eq!(r.factor, 0.5);
    // The record replays at its factor.
    let l = ledger::<Detail>(&half);
    let fresh = replay(&l, &half, l.len(), None, |_, _| {}).unwrap();
    assert_same(fresh.as_ref(), half.detail.as_ref(), "replay of a scaled record");

    // After an undo (the scene before the stroke) the tag is stale, and
    // the grid is left alone.
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    let mut undone = clay.clone();
    install(&mut undone, c.ledger.clone(), c.grid.clone());
    let mut h = crate::app::history::History::default();
    let mut before_stroke = clay.clone();
    let l0 = edit::<Detail>(&clay, &ledger(&clay), Edit::Delete(vec![c.record.id]), None).unwrap();
    install(&mut before_stroke, l0.ledger, l0.grid);
    scene.get_mut(id).unwrap().clay = Some(before_stroke.clone());
    let snapshot = scene.clone();
    scene.get_mut(id).unwrap().clay = Some(undone);
    h.push(snapshot, &scene, crate::app::history::StepInfo::new("sculpt standard on clay1 [agent]", "agent", vec!["sculpt".into()]));
    assert!(h.undo(&mut scene));
    let now = scene.get(id).unwrap().clay.clone().unwrap();
    let err = adjust_last(&now, &last, 0.5).unwrap_err();
    assert!(err.contains("stale last"), "{err}");
    assert!(scene.get(id).unwrap().clay.as_ref() == Some(&now), "the grid is untouched");
}

#[test]
fn engine_stale_records_keep_their_saved_grid_until_rebased() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(6, 9), None);
    let mut file = serde_json::to_value(&clay).unwrap();
    file["sculpt"]["base"]["records"][2]["engine"] = json!(ENGINE_VERSION - 1);
    let mut back: Clay = serde_json::from_value(file).unwrap();
    let saved = back.detail.clone();
    after_load(&mut back);
    let l = ledger::<Detail>(&back);
    assert!(back.detail == saved, "the saved grid is kept");
    assert_eq!(l.verified(), Some(false));
    assert!(l.records()[2].engine_stale());
    assert_eq!(layer_json::<Detail>(&back)["engine_stale_records"], 1);
    let e = edit::<Detail>(&back, &l, Edit::Rebase, None).unwrap();
    assert!(e.ledger.records().iter().all(|r| !r.engine_stale() && r.engine == ENGINE_VERSION));
    assert_eq!(e.ledger.verified(), Some(true));
    install(&mut back, e.ledger, e.grid);
    assert_eq!(layer_json::<Detail>(&back)["engine_stale_records"], 0);
    // Rebased, the replay is the live grid again (bit for bit, not the
    // rounded saved one).
    assert_same(back.detail.as_ref(), clay.detail.as_ref(), "rebased");
}

/// One fixture stroke's result hash per brush kernel version: a kernel
/// change that moves it must raise `ENGINE_VERSION` and add a row.
/// Version 2 changed only strokes on refined levels and strokes at a
/// level, so this fixture (no regions) keeps its hash; `sculpt::stage8::LEVEL_FIXTURES`
/// pins one that does change.
const ENGINE_FIXTURES: [(u16, u128); 2] = [(1, 0x18ee_5308_0bd0_df6e_7790_f277_b71c_5193), (2, 0x18ee_5308_0bd0_df6e_7790_f277_b71c_5193)];

#[test]
fn the_engine_version_pins_a_fixture_hash() {
    let mut clay = sphere(0.02);
    let c = sculpt(&mut clay, &[json!({"brush":"standard","points":[[0,1,0],[0.2,0.95,0.1]],"radius":0.08,"strength":0.5}), json!({"brush":"crease","points":[[-0.2,0.95,0],[0.2,0.95,0]],"radius":0.05,"strength":0.01})], None);
    let hash = c.last().unwrap().record.result_hash;
    let pinned = ENGINE_FIXTURES.iter().find(|(v, _)| *v == ENGINE_VERSION).expect("a fixture for this engine version").1;
    assert_eq!(hash, Hash128(pinned), "the brush kernel changed its output: raise ENGINE_VERSION and pin {hash}");
}

#[test]
fn records_keep_resolved_defaults_whatever_the_editor_selects() {
    // A sculpt without layer or cell writes the base layer at the clay's
    // own cell: the record stores both, and the same JSON gives the same
    // result hash on any copy (the resolver reads no editor state).
    let strokes = [json!({"brush":"inflate","point":[0,1,0],"radius":0.2})];
    let (mut a, mut b) = (sphere(0.02), sphere(0.02));
    let ra = sculpt(&mut a, &strokes, None).pop().unwrap().record;
    let rb = sculpt(&mut b, &strokes, Some(&ReplayCache::default())).pop().unwrap().record;
    assert_eq!((ra.layer.as_str(), ra.cell), ("base", Some(0.02)));
    assert_eq!(ra.result_hash, rb.result_hash);
    assert_eq!(ra.context_hash, rb.context_hash);
}

#[test]
fn outside_edits_become_opaque_records_and_replays_still_end_at_the_clay() {
    let mut clay = sphere(0.02);
    // A clay sculpted before ledgers: its grid is the first record.
    let mut old = clay.clone();
    crate::sculpt::run(&mut old, &crate::sculpt::prepare(&[json!({"brush":"inflate","point":[0,1,0],"radius":0.3})], None, &ResolveCtx::local()).unwrap()[0].1).unwrap();
    clay.detail = old.detail.clone();
    let c = sculpt(&mut clay, &[json!({"brush":"carve","points":[[0,1,0],[0.2,1,0]],"radius":0.05})], None);
    let l = ledger::<Detail>(&clay);
    assert_eq!(l.len(), 2);
    assert!(matches!(&l.records()[0].op, RecordOp::Opaque { delta: false, .. }) && l.records()[0].source == Source::Import);
    assert!(c[0].synced.is_some());
    // A new detail cell resamples the grid: an opaque record of it.
    clay.set_detail_cell(0.015).unwrap();
    assert!(sync_clay(&mut clay).unwrap());
    // An outside edit on the same layout is kept as a change (delta).
    let mut d = clay.detail.clone().unwrap();
    d.grid_mut().set([0, 66, 0], 0.003);
    clay.detail = Some(d);
    assert!(sync_clay(&mut clay).unwrap());
    let l = ledger::<Detail>(&clay);
    assert_eq!(l.len(), 4);
    assert!(matches!(&l.records()[3].op, RecordOp::Opaque { delta: true, .. }));
    let fresh = replay(&l, &clay, l.len(), None, |_, _| {}).unwrap();
    assert_same(fresh.as_ref(), clay.detail.as_ref(), "replay through opaque records");
    // Squashing records keeps the result.
    let ids: Vec<u64> = l.records()[1..3].iter().map(|r| r.id).collect();
    let s = edit::<Detail>(&clay, &l, Edit::Squash(ids), None).unwrap();
    assert_eq!(s.ledger.len(), 3);
    let mut worst = 0.0f32;
    detail_changes(s.grid.as_ref(), clay.detail.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    // The squashed change is kept as a file reads it back (1/256 voxel steps).
    assert!(worst <= 0.015 / 512.0 + 1e-7, "squash moved the grid by {worst}");
    assert!(!sync_clay(&mut clay).unwrap(), "nothing left to record");
}

#[test]
fn opaque_records_replay_the_same_after_a_save_and_load() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(5, 21), None);
    // A new detail cell (a whole-grid record), then an outside edit on the
    // same layout (a delta record) with a value off the file's steps.
    clay.set_detail_cell(0.015).unwrap();
    assert!(sync_clay(&mut clay).unwrap());
    // A refined region, sculpted, then a second region: the whole grid,
    // its level's bricks included, becomes a record.
    let region = |name: &str, x: f32| crate::clay::Region { name: name.into(), center: Vec3::new(x, 1.0, 0.0), radii: Vec3::splat(0.25), cell: 0.0075 };
    clay.set_detail_regions(&[region("a", 0.0)]).unwrap();
    assert!(sync_clay(&mut clay).unwrap());
    sculpt(&mut clay, &[json!({"brush":"add","points":[[0,1,0],[0.1,0.99,0]],"radius":0.04})], None);
    clay.set_detail_regions(&[region("a", 0.0), region("b", 0.6)]).unwrap();
    assert!(sync_clay(&mut clay).unwrap());
    assert!(clay.detail.as_ref().unwrap().grid().levels()[0].grid.total_bricks() > 0, "the level has bricks");
    let mut d = clay.detail.clone().unwrap();
    d.grid_mut().set([0, 66, 0], 0.00123);
    clay.detail = Some(d);
    assert!(sync_clay(&mut clay).unwrap());
    sculpt(&mut clay, &mixed_strokes(3, 22), None);
    let l = ledger::<Detail>(&clay);
    let opaque = |i: usize, delta: bool| matches!(&l.records()[i].op, RecordOp::Opaque { delta: d, .. } if *d == delta);
    assert!(opaque(5, false) && opaque(6, false) && opaque(8, false) && opaque(9, true));
    let mut back: Clay = serde_json::from_str(&serde_json::to_string(&clay).unwrap()).unwrap();
    after_load(&mut back);
    assert_eq!(ledger::<Detail>(&back).verified(), Some(true));
    assert_eq!(ledger::<Detail>(&back).head(), l.head());
    assert_same(back.detail.as_ref(), clay.detail.as_ref(), "the exact grid after load");
}

#[test]
fn checkpoints_never_cross_records_that_share_an_id() {
    let cache = ReplayCache::default();
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(31, 7), Some(&cache));
    let undone = clay.clone();
    // Stroke 32 plants the checkpoint at 32 ...
    let mut x = clay.clone();
    sculpt(&mut x, &[json!({"brush":"inflate","point":[0,1,0],"radius":0.3,"strength":0.8})], Some(&cache));
    assert_eq!(cache.checkpoints(), 1);
    // ... then is undone, and an outside edit takes id 32 (no checkpoint).
    let mut y = undone.clone();
    let mut d = y.detail.clone().unwrap();
    d.grid_mut().set([0, 50, 0], 0.004);
    y.detail = Some(d);
    assert!(sync_clay(&mut y).unwrap());
    let ly = ledger::<Detail>(&y);
    assert_eq!(ly.records()[31].id, ledger::<Detail>(&x).records()[31].id, "the same id at the same place");
    let cached = replay(&ly, &y, 32, Some(&cache), |_, _| {}).unwrap();
    let fresh = replay(&ly, &y, 32, None, |_, _| {}).unwrap();
    assert_same(cached.as_ref(), fresh.as_ref(), "replay past an undone stroke's checkpoint");
    // Copies of one clay (same ledger uid) sculpted apart keep their own.
    let (mut a, mut b) = (undone.clone(), undone);
    sculpt(&mut a, &[json!({"brush":"add","points":[[0,1,0],[0.2,0.95,0]],"radius":0.05})], Some(&cache));
    sculpt(&mut b, &[json!({"brush":"carve","points":[[0,1,0],[0.2,0.95,0]],"radius":0.05})], Some(&cache));
    let la = ledger::<Detail>(&a);
    let e = edit::<Detail>(&a, &la, Edit::Enable(vec![la.records()[31].id], true), Some(&cache)).unwrap();
    assert_same(e.grid.as_ref(), a.detail.as_ref(), "copy a replays its own stroke 32");
}

#[test]
fn ledgers_refuse_records_and_points_past_their_limits() {
    let mut clay = sphere(0.02);
    let c = sculpt(&mut clay, &mixed_strokes(2, 4), None);
    let record = c.last().unwrap().record.clone();
    let (spec, r) = crate::sculpt::prepare(&[json!({"brush":"inflate","point":[0,1,0],"radius":0.2})], None, &ResolveCtx::local()).unwrap().pop().unwrap();
    let meta = Meta::agent("sculpt", None);
    // A full ledger: the next stroke says to squash.
    let mut full = clay.clone();
    let mut data = ledger::<Detail>(&full).data().clone();
    // Small records (a no-op change each), so the fixture stays light.
    let light = Record { op: RecordOp::Opaque { why: "test".into(), delta: true, grid: None }, digest: Digest::default(), ..(*record).clone() };
    data.records = (0..MAX_RECORDS as u64).map(|i| Arc::new(Record { id: i + 1, ..light.clone() })).collect();
    data.next_id = MAX_RECORDS as u64 + 1;
    data.rechain();
    install(&mut full, Ledger(Arc::new(data)), clay.detail.clone());
    let err = commit::<Detail>(&full, &ledger(&full), &meta, &spec, &r, false, None).err().unwrap();
    assert!(err.contains("full") && err.contains("squash"), "{err}");
    // Two records holding all the dab points the clay may have.
    let mut heavy = clay.clone();
    let mut data = ledger::<Detail>(&heavy).data().clone();
    let mut big = (*record).clone();
    if let RecordOp::Stroke { stroke, .. } = &mut big.op {
        stroke.dabs = vec![crate::brush::resolve::Dab::path(vec![Vec3::Y; MAX_POINTS / 2], 0.05)];
        stroke.paint_dabs = None;
    }
    big.digest = Digest::default();
    data.records = vec![Arc::new(Record { id: 1, ..big.clone() }), Arc::new(Record { id: 2, ..big })];
    data.next_id = 3;
    data.rechain();
    install(&mut heavy, Ledger(Arc::new(data)), clay.detail.clone());
    assert!(heavy.sculpt.as_ref().unwrap().is_sane(), "at the limit is fine");
    let err = commit::<Detail>(&heavy, &ledger(&heavy), &meta, &spec, &r, false, None).err().unwrap();
    assert!(err.contains("dab points") && err.contains("squash"), "{err}");
    // So an edit's sanitize never drops the history mid-session.
    let mut kept = heavy.clone();
    kept.sanitize();
    assert!(kept.sculpt.is_some());
}

#[test]
fn malformed_records_fail_to_load_instead_of_panicking() {
    use crate::brush::resolve::Dab;
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &[json!({"brush":"add","points":[[0,1,0],[0.2,0.95,0]],"radius":0.05})], None);
    let file = serde_json::to_value(&clay).unwrap();
    let dab = file["sculpt"]["base"]["records"][0]["op"]["stroke"]["stroke"]["dabs"][0].clone();
    assert!(dab["points"].as_array().unwrap().len() >= 2);
    let bad = |edit: &dyn Fn(&mut Value)| {
        let mut f = file.clone();
        edit(&mut f["sculpt"]["base"]["records"][0]["op"]["stroke"]["stroke"]["dabs"][0]);
        serde_json::from_value::<Clay>(f).is_err()
    };
    assert!(bad(&|d| d["radii"] = json!([0.05])), "radii shorter than the points");
    assert!(bad(&|d| d["gains"] = json!(vec![1.0; 200])), "gains longer than the points");
    assert!(bad(&|d| d["points"] = json!([])), "no points");
    assert!(!bad(&|_| {}), "the file itself loads");
    // The engine refuses such a dab built in memory too.
    let mut probe = clay.clone();
    let (_, mut r) = crate::sculpt::prepare(&[json!({"brush":"add","points":[[0,1,0],[0.2,0.95,0]],"radius":0.05})], None, &ResolveCtx::local()).unwrap().pop().unwrap();
    r.dabs[0].radii = Some(vec![0.05]);
    assert!(crate::sculpt::run(&mut probe, &r).is_err());
    assert!(Dab::path(vec![Vec3::Y, Vec3::X], 0.1).check().is_ok());
}

#[test]
fn checking_ledgers_on_load_stops_at_the_deadline() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(6, 8), None);
    let mut back: Clay = serde_json::from_str(&serde_json::to_string(&clay).unwrap()).unwrap();
    let saved = back.detail.clone();
    after_load_by(&mut back, std::time::Instant::now());
    let l = ledger::<Detail>(&back);
    assert_eq!(l.verified(), None, "out of time: unchecked");
    assert!(back.detail == saved, "the saved grid is kept");
    // The saved grid is the file's rounding, not what the records give.
    assert!(!l.in_sync(back.detail.as_ref()));
    // The next stroke replays the records first and builds on the exact
    // grid (no outside-edit record): the same ledger and grid as when the
    // check finished, so it replays from scratch.
    let more = [json!({"brush":"inflate","point":[0,1,0],"radius":0.2,"strength":0.3})];
    let mut slow = back.clone();
    sculpt(&mut slow, &more, None);
    sculpt(&mut clay, &more, None);
    let l = ledger::<Detail>(&slow);
    let fast = ledger::<Detail>(&clay);
    assert_eq!(l.len(), fast.len(), "no outside-edit record");
    assert_eq!(l.records().last().unwrap().result_hash, fast.records().last().unwrap().result_hash, "the same result as without the load");
    assert_eq!(l.verified(), Some(true));
    assert_same(slow.detail.as_ref(), clay.detail.as_ref(), "the stroke after an unchecked load");
    let check = edit::<Detail>(&slow, &l, Edit::Replay(0), None).unwrap();
    assert!(check.mismatched.is_empty(), "records after an unchecked load replay: {:?}", check.mismatched);
    let mut again: Clay = serde_json::from_str(&serde_json::to_string(&slow).unwrap()).unwrap();
    after_load(&mut again);
    assert_eq!(ledger::<Detail>(&again).verified(), Some(true));
    // An outside edit of an unchecked load is recorded from the exact grid.
    let mut edited = back.clone();
    let mut d = edited.detail.clone().unwrap();
    d.grid_mut().set([0, 66, 0], 0.003);
    edited.detail = Some(d);
    assert!(sync_clay(&mut edited).unwrap());
    let l = ledger::<Detail>(&edited);
    assert!(matches!(&l.records().last().unwrap().op, RecordOp::Opaque { delta: true, .. }));
    assert!(edit::<Detail>(&edited, &l, Edit::Replay(0), None).unwrap().mismatched.is_empty());
}

#[test]
fn unchecked_ledgers_settle_before_the_clay_changes() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(6, 8), None);
    let meta = Meta::agent("paint", None);
    let strokes = [json!({"points":[[0,1,0],[0.3,0.95,0]],"radius":0.1,"color":[0.1,0.5,0.9]})];
    for (spec, r) in crate::sculpt::prepare(&strokes, Some(Brush::Paint), &ResolveCtx::local()).unwrap() {
        let c = commit::<Paint>(&clay, &ledger(&clay), &meta, &spec, &r, false, None).unwrap();
        install(&mut clay, c.ledger, c.grid);
    }
    let text = serde_json::to_string(&clay).unwrap();
    let load = |finish: bool| {
        let mut c: Clay = serde_json::from_str(&text).unwrap();
        after_load_by(&mut c, if finish { std::time::Instant::now() + VERIFY_BUDGET } else { std::time::Instant::now() });
        c
    };
    let (checked, unchecked) = (load(true), load(false));
    assert_eq!(ledger::<Detail>(&unchecked).verified(), None);
    // Read before anything settles: paint records are not stale.
    for c in [&checked, &unchecked] {
        assert_eq!(layer_json::<Paint>(c)["stale_records"], json!(0), "paint records read as made on the sculpted surface");
    }
    // A tool settles, then changes the blobs and a detail cell (as
    // edit_clay does): the same ledgers and grids either way.
    let change = |mut c: Clay, cell: bool| {
        settle_clay(&mut c, None).unwrap();
        c.blobs = serde_json::from_value(json!([{"type":"sphere","name":"ball","center":[0,0,0],"radius":0.97,"blend":0}])).unwrap();
        if cell {
            c.set_detail_cell(0.025).unwrap();
        }
        sync_clay(&mut c).unwrap();
        c
    };
    for cell in [false, true] {
        let (a, b) = (change(checked.clone(), cell), change(unchecked.clone(), cell));
        let (la, lb) = (ledger::<Detail>(&a), ledger::<Detail>(&b));
        assert_eq!(la.len(), lb.len(), "the same records (cell change {cell})");
        assert_eq!((la.verified(), lb.verified()), (Some(true), Some(true)));
        assert_eq!(la.records().last().unwrap().result_hash, lb.records().last().unwrap().result_hash);
        assert_same(a.detail.as_ref(), b.detail.as_ref(), "after a blob change");
        // (Records replayed on new blobs may differ: alike either way.)
        assert_eq!(edit::<Detail>(&a, &la, Edit::Replay(0), None).unwrap().mismatched, edit::<Detail>(&b, &lb, Edit::Replay(0), None).unwrap().mismatched);
        assert_eq!(ledger::<Paint>(&a).len(), ledger::<Paint>(&b).len());
    }
    // A paint stroke and a paint edit after the load build on the exact
    // surface, so their records match the checked load's.
    let (spec, r) = crate::sculpt::prepare(&[json!({"points":[[0.2,0.97,0]],"radius":0.08,"color":[0.9,0.2,0.1]})], Some(Brush::Paint), &ResolveCtx::local()).unwrap().pop().unwrap();
    let paint = |mut c: Clay| {
        settle_clay(&mut c, None).unwrap();
        let p = commit::<Paint>(&c, &ledger(&c), &meta, &spec, &r, false, None).unwrap();
        assert!(p.synced.is_none(), "no outside-edit record");
        install(&mut c, p.ledger, p.grid);
        sync_clay(&mut c).unwrap();
        c
    };
    let (a, b) = (paint(checked.clone()), paint(unchecked.clone()));
    let (pa, pb) = (ledger::<Paint>(&a), ledger::<Paint>(&b));
    let (ra, rb) = (pa.records().last().unwrap(), pb.records().last().unwrap());
    assert_eq!((ra.context_hash, ra.result_hash), (rb.context_hash, rb.result_hash), "the paint record is the same either way");
    assert_eq!(rb.context_hash, Paint::context(&b), "and not stale");
    let first = pb.records()[0].id;
    let (ea, eb) = (edit::<Paint>(&a, &pa, Edit::Enable(vec![first], false), None).unwrap(), edit::<Paint>(&b, &pb, Edit::Enable(vec![first], false), None).unwrap());
    assert_eq!(ea.ledger.records().last().unwrap().result_hash, eb.ledger.records().last().unwrap().result_hash);
    // A job settles a copy: the clay takes the settled ledgers it still
    // holds unchecked.
    let mut cur = unchecked.clone();
    let mut settled = unchecked.clone();
    settle_clay(&mut settled, None).unwrap();
    adopt_settled(&mut cur, &unchecked, &settled);
    // (Paint, saved exactly, settles by its hash without a replay.)
    assert_eq!(ledger::<Detail>(&cur).verified(), Some(true));
    assert!(ledger::<Paint>(&cur).in_sync(cur.paint.as_ref()));
    assert!(cur.detail == settled.detail && cur.paint == settled.paint);
}

#[test]
fn records_made_on_other_blobs_load_the_same_checked_or_not() {
    // Blobs changed after the strokes (no outside edit of the grid): the
    // replay on load runs on the new blobs and need not match the saved
    // grid, and settling later says what the check would have.
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &mixed_strokes(6, 8), None);
    clay.blobs = serde_json::from_value(json!([{"type":"sphere","name":"ball","center":[0,0,0],"radius":0.97,"blend":0}])).unwrap();
    assert!(!sync_clay(&mut clay).unwrap());
    let text = serde_json::to_string(&clay).unwrap();
    let mut checked: Clay = serde_json::from_str(&text).unwrap();
    after_load(&mut checked);
    let mut unchecked: Clay = serde_json::from_str(&text).unwrap();
    after_load_by(&mut unchecked, std::time::Instant::now());
    settle_clay(&mut unchecked, None).unwrap();
    let (a, b) = (ledger::<Detail>(&checked), ledger::<Detail>(&unchecked));
    assert!(a.verified().is_some());
    assert_eq!(a.verified(), b.verified());
    assert_eq!(a.len(), b.len());
    assert!(a.in_sync(checked.detail.as_ref()) && b.in_sync(unchecked.detail.as_ref()));
    assert_same(checked.detail.as_ref(), unchecked.detail.as_ref(), "the grid either way");
}

#[test]
fn squashing_from_the_first_record_replays() {
    let mut clay = sphere(0.02);
    let strokes = [json!({"points":[[0,1,0],[0.3,0.95,0]],"radius":0.1,"color":[0.1,0.5,0.9]}), json!({"points":[[0,1,0]],"radius":0.05,"color":[0.9,0.2,0.1]})];
    let meta = Meta::agent("paint", None);
    for (spec, r) in crate::sculpt::prepare(&strokes, Some(Brush::Paint), &ResolveCtx::local()).unwrap() {
        let c = commit::<Paint>(&clay, &ledger(&clay), &meta, &spec, &r, false, None).unwrap();
        install(&mut clay, c.ledger, c.grid);
    }
    sculpt(&mut clay, &mixed_strokes(3, 5), None);
    let l = ledger::<Paint>(&clay);
    let s = edit::<Paint>(&clay, &l, Edit::Squash(l.records().iter().map(|r| r.id).collect()), None).unwrap();
    assert_eq!(s.ledger.len(), 1);
    assert!(matches!(&s.ledger.records()[0].op, RecordOp::Opaque { delta: false, .. }));
    let fresh = replay(&s.ledger, &clay, 1, None, |_, _| {}).unwrap();
    assert_eq!(Paint::hash(fresh.as_ref()), Paint::hash(s.grid.as_ref()));
    let b = ledger::<Detail>(&clay);
    let s = edit::<Detail>(&clay, &b, Edit::Squash(b.records()[..2].iter().map(|r| r.id).collect()), None).unwrap();
    assert_eq!(s.ledger.len(), 2);
    assert!(s.mismatched.is_empty());
    let fresh = replay(&s.ledger, &clay, 2, None, |_, _| {}).unwrap();
    assert_same(fresh.as_ref(), s.grid.as_ref(), "a sculpt squash from the first record");
}

#[test]
fn paint_strokes_log_to_the_paint_layer() {
    let mut clay = sphere(0.02);
    let strokes = [json!({"points":[[0,1,0],[0.3,0.95,0]],"radius":0.1,"color":[0.1,0.5,0.9]}), json!({"points":[[0,1,0]],"radius":0.05,"erase":true})];
    let meta = Meta::agent("paint", None);
    for (spec, r) in crate::sculpt::prepare(&strokes, Some(Brush::Paint), &ResolveCtx::local()).unwrap() {
        let c = commit::<Paint>(&clay, &ledger(&clay), &meta, &spec, &r, false, None).unwrap();
        install(&mut clay, c.ledger, c.grid);
    }
    let l = ledger::<Paint>(&clay);
    assert_eq!((l.len(), ledger::<Detail>(&clay).len()), (2, 0));
    assert!(clay.detail.is_none());
    let fresh = replay(&l, &clay, 2, None, |_, _| {}).unwrap();
    assert_eq!(Paint::hash(fresh.as_ref()), Paint::hash(clay.paint.as_ref()));
    let e = edit::<Paint>(&clay, &l, Edit::Enable(vec![l.records()[1].id], false), None).unwrap();
    let one = replay(&l, &clay, 1, None, |_, _| {}).unwrap();
    assert_eq!(Paint::hash(e.grid.as_ref()), Paint::hash(one.as_ref()), "the erase undone");
    assert!(edit::<Paint>(&clay, &l, Edit::Scale(vec![l.records()[0].id], 0.5), None).is_err());
}

#[test]
fn mirrored_clays_reflect_their_records() {
    let mut clay = sphere(0.02);
    sculpt(&mut clay, &[json!({"brush":"add","points":[[0.3,0.9,0.1],[0.5,0.8,0.1]],"radius":0.05})], None);
    let m = clay.mirrored_x();
    let l = ledger::<Detail>(&m);
    let (_, r) = l.records()[0].stroke().unwrap();
    assert_eq!(r.dabs[0].points[0], Vec3::new(-0.3, 0.9, 0.1));
    assert_eq!(l.records()[0].source, Source::Replay);
    assert!(l.in_sync(m.detail.as_ref()));
    // Replayed, the reflected records give the mirrored grid (within float
    // noise: the field is evaluated at mirrored points).
    let fresh = replay(&l, &m, 1, None, |_, _| {}).unwrap();
    let mut worst = 0.0f32;
    detail_changes(fresh.as_ref(), m.detail.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= 1e-5, "mirror replay off by {worst}");
}

/// The 6-dab 1 mm fixture of the timing report (harness/timing.rs).
fn six_dabs(cell: f32) -> Clay {
    let mut c = crate::harness::figure(96, true);
    c.set_detail_cell(cell).unwrap();
    let mut strokes = vec![
        json!({"brush":"inflate","points":[[0.0,1.25,0.18]],"radius":0.03}),
        json!({"brush":"inflate","points":[[0.0,1.25,0.18]],"radius":0.08}),
        json!({"brush":"smooth","points":[[0.0,1.25,0.18]],"radius":0.05}),
        json!({"brush":"grab","points":[[0.0,1.25,0.18]],"radius":0.05,"delta":[0,0,0.02]}),
        json!({"brush":"crease","points":[[0.0,1.25,0.18]],"radius":0.02,"strength":0.003}),
    ];
    let path: Vec<[f32; 3]> = (0..12).map(|i| [-0.15 + i as f32 * 0.025, 1.3, 0.17]).collect();
    strokes.push(json!({"brush":"add","points":path,"radius":0.015}));
    sculpt(&mut c, &strokes, None);
    c
}

#[test]
fn deflated_bricks_are_at_least_two_and_a_half_times_smaller() {
    let c = six_dabs(0.001);
    let d = c.detail.as_ref().unwrap();
    let raw = d.raw_file_json().len();
    let packed = serde_json::to_string(d).unwrap().len();
    let ratio = raw as f64 / packed as f64;
    perf::log("saved detail after 6 dabs at 1 mm", 0.0, json!({ "raw_bytes": raw, "packed_bytes": packed, "ratio": (ratio * 100.0).round() / 100.0, "bricks": d.grid().total_bricks() }));
    eprintln!("detail file: raw {raw} B, packed {packed} B, {ratio:.2}x");
    assert!(ratio >= 2.5, "packed {packed} vs raw {raw}: only {ratio:.2}x");
    // Both forms read back to the same offsets.
    let a: Detail = serde_json::from_str(&d.raw_file_json()).unwrap();
    let b: Detail = serde_json::from_str(&serde_json::to_string(d).unwrap()).unwrap();
    assert_eq!(Detail::hash(Some(&a)), Detail::hash(Some(&b)));
    // The whole clay file (ledger included) is smaller too.
    let file = serde_json::to_string(&c).unwrap().len();
    assert!((file as f64) < raw as f64 / 2.0, "clay file {file} vs raw detail {raw}");
    // Paint packs too.
    let mut p = sphere(0.005);
    let (_, r) = crate::sculpt::prepare(&[json!({"points":[[0,1,0],[0.5,0.85,0]],"radius":0.2,"color":[0.7,0.2,0.1]})], Some(Brush::Paint), &ResolveCtx::local()).unwrap().pop().unwrap();
    crate::sculpt::run(&mut p, &r).unwrap();
    let paint = p.paint.as_ref().unwrap();
    let back: Paint = serde_json::from_str(&serde_json::to_string(paint).unwrap()).unwrap();
    assert_eq!(Paint::hash(Some(&back)), Paint::hash(Some(paint)));
    assert!(serde_json::to_string(paint).unwrap().len() * 2 < paint.grid().raw_file_json().len());
}

#[test]
fn scene_clone_and_compare_stay_cheap_with_heavy_sculpts() {
    // The heavy fixture: 8 sculpted clays (standing in for 8 layers), one
    // with paint and 4 mask channels, and a 1,000-record ledger.
    let mut scene = crate::scene::Scene::default();
    let mut base = sphere(0.02);
    let c = sculpt(&mut base, &mixed_strokes(4, 1), None);
    let record = c.last().unwrap().record.clone();
    let mut data = ledger::<Detail>(&base).data().clone();
    data.records = (0..1000).map(|i| Arc::new(Record { id: i + 1, ..(*record).clone() })).collect();
    data.next_id = 1001;
    data.rechain();
    let grid = base.detail.clone();
    install(&mut base, Ledger(Arc::new(data)), grid);
    let paint = crate::paint::Paint::new(0.02);
    base.paint = Some(paint.clone());
    for k in 0..4 {
        base.masks.insert(format!("mask{k}"), paint.clone());
    }
    // A full alpha library and preset list (Stage 2), and alphas kept on
    // the clays.
    let mut store = crate::brush::alpha::AlphaStore::default();
    for k in 0..crate::brush::alpha::MAX_ALPHAS as u32 {
        let a = crate::brush::alpha::Alpha::new(64, 64, (0..64 * 64).map(|i| (i * 7 + k) as u16).collect()).unwrap();
        store.keep(Some(&format!("alpha{k}")), &a).unwrap();
        scene.alphas.insert(format!("alpha{k}"), a);
    }
    set_alphas(&mut base, store);
    for k in 0..crate::brush::preset::MAX_PRESETS {
        scene.brushes.insert(format!("preset{k}"), crate::brush::preset::Preset::try_from(json!({ "brush": "clay_tubes", "strength": 0.4, "falloff": [[0, 1], [0.5, 0.8], [1, 0]], "alpha": { "name": format!("alpha{}", k % 64), "rotate": k } })).unwrap());
    }
    for _ in 0..8 {
        let id = scene.add(crate::scene::Shape::Clay);
        scene.get_mut(id).unwrap().clay = Some(base.clone());
    }
    let (eq, ms) = perf::time(201, || {
        let copy = scene.clone();
        copy == scene
    });
    assert!(eq);
    perf::check("scene clone + compare, heavy sculpt fixture", ms, 0.05, json!({ "objects": 8, "records": 1000, "alphas": 64, "presets": 256 }));
}

#[test]
fn commit_overhead_and_replay_speed() {
    // 64 strokes x 64 points: resolve + commit against apply alone.
    let mut rng = Pcg32::new(17, 3);
    let strokes: Vec<Value> = (0..64)
        .map(|k| {
            let (a, y) = (rng.next_f32() * std::f32::consts::TAU, 0.3 + rng.next_f32() * 0.6);
            let pts: Vec<[f32; 3]> = (0..64).map(|i| {
                let t = a + i as f32 * 0.004;
                let r = (1.0 - y * y).sqrt();
                [r * t.cos(), y, r * t.sin()]
            }).collect();
            json!({"brush": if k % 2 == 0 { "add" } else { "inflate" }, "points": pts, "radius": 0.03, "strength": 0.4})
        })
        .collect();
    let clay0 = sphere(0.01);
    let resolved = crate::sculpt::prepare(&strokes, None, &ResolveCtx::local()).unwrap();
    // Interleaved runs, the median of each, so a loaded machine slows both.
    let (mut totals, mut applies) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        totals.push(perf::time(1, || {
            let mut c = clay0.clone();
            sculpt(&mut c, &strokes, Some(&ReplayCache::default()));
        }).1);
        applies.push(perf::time(1, || {
            let mut c = clay0.clone();
            for (_, r) in &resolved {
                crate::sculpt::run(&mut c, r).unwrap();
            }
        }).1);
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (total, apply) = (median(&mut totals), median(&mut applies));
    let overhead = (total - apply).max(0.0);
    eprintln!("64 x 64-point strokes: commit path {total:.1} ms, apply alone {apply:.1} ms");
    // A loaded machine (the whole suite running) slows both paths alike:
    // the bound follows apply's slowdown over its quiet time (about 45 ms
    // on an M2 Max), which is 1 when the test runs alone.
    let load = (apply / 45.0).max(1.0);
    perf::check("resolve + commit beyond apply, 64 strokes x 64 points", overhead, 20.0 * load, json!({ "total_ms": total, "apply_ms": apply, "load": load }));

    // Replaying 200 dabs at a 2 mm cell.
    let mut c = sphere(0.002);
    c.blobs[0] = serde_json::from_value(json!({"type":"sphere","name":"ball","center":[0,0,0],"radius":0.3,"blend":0})).unwrap();
    let dabs: Vec<Value> = (0..22).map(|k| {
        let a = k as f32 * 0.3;
        json!({"brush":"standard","points":[[0.3 * a.cos(), 0.0, 0.3 * a.sin()], [0.3 * (a + 0.2).cos(), 0.05, 0.3 * (a + 0.2).sin()]],"radius":0.02,"strength":0.4,"spacing":0.5})
    }).collect();
    sculpt(&mut c, &dabs, None);
    let l = ledger::<Detail>(&c);
    let n: usize = l.records().iter().map(|r| r.stroke().unwrap().1.dabs.len()).sum();
    assert!((160..=240).contains(&n), "{n} dabs");
    let (g, ms) = perf::time(1, || replay(&l, &c, l.len(), None, |_, _| {}).unwrap());
    assert_same(g.as_ref(), c.detail.as_ref(), "replay of 200 dabs");
    perf::check("replay 200 dabs at 2 mm", ms, 2000.0, json!({ "dabs": n }));
}

/// A brow ridge sculpted live, replayed from its ledger and rendered: the
/// replay looks like the sculpt. `CLIDE_GPU=1` only.
#[test]
fn golden_brow_ridge_replay() {
    if !image::gpu_enabled() {
        eprintln!("golden_brow_ridge_replay: set CLIDE_GPU=1 to run");
        return;
    }
    let mut clay: Clay = serde_json::from_value(json!({"blobs":[{"type":"sphere","name":"head","center":[0,0,0],"radius":0.5}], "resolution":128, "detail_cell":0.004})).unwrap();
    let brow = |x: f32| json!({"brush":"add","points":[[x * 0.06, 0.12, 0.49], [x * 0.16, 0.14, 0.46], [x * 0.24, 0.11, 0.42]],"radius":0.035});
    sculpt(&mut clay, &[brow(1.0), brow(-1.0), json!({"brush":"standard","points":[[-0.2,0.1,0.47],[0.2,0.1,0.47]],"radius":0.05,"strength":0.4}), json!({"brush":"smooth","point":[0,0.13,0.48],"radius":0.12,"strength":0.4})], None);
    let l = ledger::<Detail>(&clay);
    let mut replayed = clay.clone();
    replayed.detail = replay(&l, &clay, l.len(), None, |_, _| {}).unwrap();
    let render = |c: &Clay| {
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.color = [0.85, 0.72, 0.55];
        o.clay = Some(c.clone());
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.sync(&scene);
        image::render(&scene, &meshes, image::View::FRONT).unwrap_or_else(|e| panic!("CLIDE_GPU=1 but: {e}"))
    };
    // The golden is the live sculpt (BLESS=1); the replay must match it.
    let shot = if image::blessing() { render(&clay) } else { render(&replayed) };
    image::assert_golden("brow_ridge", &shot, 2.0, 8);
}

/// A packed detail level of `n` zero bricks: a few KB that inflate to
/// `4 + n * (12 + BRICK_VALUES * 2)` bytes.
fn zero_bricks(n: usize) -> String {
    let keys: Vec<[i32; 3]> = (0..n as i32).map(|i| [i, 0, 0]).collect();
    crate::clay::pack(&keys, &vec![0u8; n * crate::clay::BRICK_VALUES * 2], 2, true)
}

/// A clay with a detail grid of `n` zero bricks.
fn clay_with_bricks(n: usize) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":1.0,"blend":0}], "resolution":32, "detail": {"voxel": 0.01, "packed": zero_bricks(n)}})).unwrap()
}

/// Packed grids are deflated, so a small file can inflate to a lot. One
/// budget covers a whole scene read: grids spread over many objects are
/// counted together and refused before they are allocated past it.
#[test]
fn packed_grids_inflate_within_one_budget_per_read() {
    let n = 600;
    let one = (4 + n * (12 + crate::clay::BRICK_VALUES * 2)) as u64;
    let mut scene = crate::scene::Scene::default();
    for _ in 0..2 {
        let id = scene.add(crate::scene::Shape::Clay);
        scene.get_mut(id).unwrap().clay = Some(clay_with_bricks(n));
    }
    let text = serde_json::to_string(&scene).unwrap();
    assert!(text.len() < 64 * 1024, "small on disk: {} bytes", text.len());
    // Either grid fits on its own; both do not.
    let read = |budget: u64| crate::clay::inflate_budget(budget, || serde_json::from_str::<crate::scene::Scene>(&text));
    let err = read(one * 3 / 2).expect_err("over the budget").to_string();
    assert!(err.contains("inflate") && err.contains(&(one * 3 / 2).to_string()), "the open budget is named: {err}");
    let back = read(one * 2).expect("within the budget");
    assert_eq!(back.objects.len(), 2);
    assert!(back.objects.iter().all(|o| o.clay.as_ref().and_then(|c| c.detail.as_ref()).is_some_and(|d| d.grid().total_bricks() == n)));
    // Outside any budget (a scene read anywhere), the default applies, and
    // a budget ends with its read.
    assert!(serde_json::from_str::<crate::scene::Scene>(&text).is_ok());
    assert!(read(one * 2).is_ok() && read(one).is_err());
    // A lone grid with a bigger brick count than its file holds is refused
    // by its header, before anything is allocated for it.
    let lie = {
        let mut raw = (MAX_RECORDS as u32 * 4).to_le_bytes().to_vec();
        raw.extend([0u8; 64]);
        use base64::Engine;
        use std::io::Write;
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&raw).unwrap();
        base64::engine::general_purpose::STANDARD.encode(z.finish().unwrap())
    };
    let err = serde_json::from_value::<Detail>(json!({"voxel": 0.01, "packed": lie})).expect_err("too many").to_string();
    assert!(err.contains("too many bricks"), "{err}");
}

/// A ledger's records and the grids they hold are counted while a file
/// is read, so a crafted file cannot make it allocate past the caps.
#[test]
fn ledger_records_and_grids_are_capped_while_read() {
    let clay = clay_with_bricks(1);
    let (l, _, record) = sync::<Detail>(&Ledger::default(), &clay, None).unwrap();
    assert!(record.expect("an outside edit").holds_grid());
    let file = serde_json::to_value(&l).unwrap();
    let template = file["records"][0].clone();
    let ledger_of = |n: u64, grid: bool| {
        let mut r = template.clone();
        if !grid {
            r["op"]["opaque"]["grid"] = Value::Null;
        }
        r["id"] = json!(987_654_321_987u64);
        let one = serde_json::to_string(&r).unwrap();
        let records: Vec<String> = (1..=n).map(|i| one.replace("987654321987", &i.to_string())).collect();
        format!("{{\"uid\":{},\"next_id\":{},\"records\":[{}]}}", file["uid"], n + 1, records.join(","))
    };
    let grids = MAX_OPAQUE_GRIDS as u64;
    let full: Ledger<Detail> = serde_json::from_str(&ledger_of(grids, true)).unwrap();
    assert_eq!((full.len(), full.data().grids()), (MAX_OPAQUE_GRIDS, MAX_OPAQUE_GRIDS));
    // Over a cap, the rest of the list is skipped unread and the ledger is
    // not sane: a clay read with it keeps its grids and drops its ledgers,
    // and the scene still reads.
    let over: Ledger<Detail> = serde_json::from_str(&ledger_of(grids + 3, true)).unwrap();
    assert_eq!(over.len(), MAX_OPAQUE_GRIDS + 1);
    let many: Ledger<Detail> = serde_json::from_str(&ledger_of(MAX_RECORDS as u64 + 3, false)).unwrap();
    assert_eq!(many.len(), MAX_RECORDS + 1);
    for text in [ledger_of(grids + 1, true), ledger_of(MAX_RECORDS as u64 + 1, false)] {
        let mut c = serde_json::to_value(&clay).unwrap();
        c["sculpt"] = json!({ "base": serde_json::from_str::<Value>(&text).unwrap() });
        let mut back: Clay = serde_json::from_value(c).unwrap();
        assert!(back.sculpt.as_ref().is_some_and(|s| !s.is_sane()));
        back.sanitize();
        assert!(back.sculpt.is_none() && back.detail.is_some(), "the ledger goes, the grid stays");
    }

    // At the cap, a grid edited outside the ledger needs one more grid
    // record first. Edits that keep the count refuse; squash and delete,
    // the ways out, take it through and end at or under the cap.
    let moved = clay_with_bricks(2);
    assert!(sync(&full, &moved, None).is_err(), "sync alone keeps the cap");
    let err = edit(&moved, &full, Edit::Enable(vec![1], false), None).err().expect("over the cap");
    assert!(err.contains("squash"), "{err}");
    let squashed = edit(&moved, &full, Edit::Squash(vec![1, 2]), None).expect("squash at the cap");
    assert_eq!(squashed.ledger.data().grids(), MAX_OPAQUE_GRIDS);
    let deleted = edit(&moved, &full, Edit::Delete(vec![1, 2]), None).expect("delete at the cap");
    assert_eq!(deleted.ledger.data().grids(), MAX_OPAQUE_GRIDS - 1);
}

#[test]
fn digests_cover_the_frozen_mask_and_the_painted_group() {
    let mut clay = sphere(0.02);
    let c = sculpt(&mut clay, &mixed_strokes(1, 9), None);
    let plain = (*c.last().unwrap().record).clone();
    let with = |f: &dyn Fn(&mut Resolved)| {
        let mut r = plain.clone();
        if let RecordOp::Stroke { stroke, .. } = &mut r.op {
            f(stroke);
        }
        r.digest = Digest::default();
        r.digest()
    };
    let base = with(&|_| {});
    assert_eq!(base, plain.digest(), "records without them keep their digests");
    let m1 = with(&|s| s.frozen = Some(FrozenRef { mask: Some(Hash128(1)), groups: None }));
    let m2 = with(&|s| s.frozen = Some(FrozenRef { mask: Some(Hash128(2)), groups: None }));
    let g1 = with(&|s| s.group = Some(1));
    let g2 = with(&|s| s.group = Some(2));
    for (a, b) in [(base, m1), (m1, m2), (base, g1), (g1, g2)] {
        assert_ne!(a, b);
    }
}
