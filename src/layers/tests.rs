//! Stage 4 acceptance (docs/SCULPT_PLAN.md 6): layers compose linearly
//! and write through their amounts, a stroke reads every layer and marks
//! records stale when another changes, bake keeps the surface, morphs
//! switch by pointer and blend linearly, blend shapes follow their target
//! (or are refused), the shared byte budget, and the perf bounds.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::resolve::ResolveCtx;
use crate::clay::Field;
use crate::harness::perf;
use crate::ledger::{Edit, Meta};

/// A unit sphere with a detail grid of `cell`.
fn sphere(cell: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"sphere","name":"ball","center":[0,0,0],"radius":1.0,"blend":0}], "resolution":48, "detail_cell":cell})).unwrap()
}

fn add_layer(clay: &mut Clay, name: &str, amount: f32) {
    ledger::stack_mut(clay).layers.push(Layer::new(name, Kind::Sculpt, amount));
}

fn set_amount(clay: &mut Clay, name: &str, amount: f32) {
    let s = ledger::stack_mut(clay);
    s.layers.iter_mut().find(|l| l.name == name).unwrap().amount = amount;
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

/// The field of the clay as shown (every layer composed) at `p`.
fn field_at(field: &Field, p: Vec3) -> f32 {
    let list = field.near(p - Vec3::splat(0.1), p + Vec3::splat(0.1));
    field.sample(p, &list).0
}

fn field_of(clay: &Clay) -> Field {
    let voxel = clay.detail_cell.unwrap();
    Field::new(clay, voxel)
}

/// How far the surface moved off the unit sphere along `n`: the root of
/// the field on the ray, by bisection within +-0.15.
fn surface_shift(field: &Field, n: Vec3) -> f32 {
    let (mut lo, mut hi) = (-0.15f32, 0.15f32);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if field_at(field, n * (1.0 + mid)) > 0.0 { hi = mid } else { lo = mid }
    }
    0.5 * (lo + hi)
}

/// Points on the unit sphere's upper cap, seeded.
fn cap_points(n: usize) -> Vec<Vec3> {
    let mut rng = crate::brush::rng::Pcg32::new(17, 3);
    (0..n).map(|_| {
        let (a, b) = (rng.next_f32() * std::f32::consts::TAU, rng.next_f32() * 0.5);
        Vec3::new(b.sin() * a.cos(), b.cos(), b.sin() * a.sin())
    }).collect()
}

#[test]
fn a_layer_at_amount_zero_or_off_is_the_stack_without_it_and_amounts_scale_the_surface() {
    let mut clay = sphere(0.02);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","point":[0.3,0.95,0.0],"radius":0.3,"strength":0.3})]);
    let plain = clay.clone();
    add_layer(&mut clay, "detail", 1.0);
    // Bumps of at most two cells on the layer.
    sculpt_on(&mut clay, "detail", &[json!({"brush":"standard","point":[0.0,1.0,0.0],"radius":0.25,"strength":0.15}), json!({"brush":"inflate","point":[-0.2,0.97,0.1],"radius":0.25,"strength":0.1})]);
    let max = layer(&clay, "detail").unwrap().detail.as_ref().unwrap().grid().max_abs();
    assert!(max > 0.25 * 0.02 && max <= 2.0 * 0.02 + 1e-6, "the layer's offsets are within two cells: {max}");
    // Amount 0 and off: the composite is the base grid itself.
    for off in [true, false] {
        let mut c = clay.clone();
        if off {
            ledger::stack_mut(&mut c).layers[0].enabled = false;
        } else {
            set_amount(&mut c, "detail", 0.0);
        }
        assert!(composed_detail(&c) == plain.detail, "the composite is base's own grid");
        assert_eq!(Detail::hash(composed_detail(&c).as_ref()), Detail::hash(plain.detail.as_ref()));
        let (a, b) = (composed_clay(&c).build_mesh_with(&[], ""), composed_clay(&plain).build_mesh_with(&[], ""));
        assert_eq!(a.vertices.len(), b.vertices.len());
        assert!(a.vertices.iter().zip(&b.vertices).all(|(x, y)| x.pos == y.pos), "the meshes are bit-equal");
    }
    // Amount 0.5 gives half the displacement at 50 probes (+-3%).
    let points: Vec<Vec3> = cap_points(2000);
    let (base, full) = (field_of(&plain), field_of(&clay));
    let mut half = clay.clone();
    set_amount(&mut half, "detail", 0.5);
    let halfway = field_of(&half);
    let mut checked = 0;
    let mut worst = 0.0f32;
    for p in points {
        let (b, f, h) = (surface_shift(&base, p), surface_shift(&full, p), surface_shift(&halfway, p));
        if (f - b).abs() < 0.25 * 0.02 {
            continue;
        }
        worst = worst.max(((h - b) / (f - b) - 0.5).abs() / 0.5);
        checked += 1;
        if checked == 50 {
            break;
        }
    }
    assert_eq!(checked, 50, "50 probes where the layer moves the surface");
    assert!(worst <= 0.03, "amount 0.5 gives 0.5x the displacement within 3%: worst {worst:.4}");
    eprintln!("amount 0.5: displacement ratio within {:.2}% of 0.5 at 50 probes (layer max offset {:.2} cells)", worst * 100.0, max / 0.02);
}

#[test]
fn amount_two_doubles_a_standard_dab_both_ways() {
    let dab = [json!({"brush":"standard","point":[0.0,1.0,0.0],"radius":0.1,"strength":0.5})];
    let top = Vec3::Y;
    // Doubled, the dab stays within the field's clamp (4 cells): offsets
    // add linearly there.
    let base = surface_shift(&field_of(&sphere(0.02)), top);
    // A dab on a layer at 1, then the layer at 2.
    let mut clay = sphere(0.02);
    add_layer(&mut clay, "L", 1.0);
    sculpt_on(&mut clay, "L", &dab);
    let one = surface_shift(&field_of(&clay), top) - base;
    set_amount(&mut clay, "L", 2.0);
    let two = surface_shift(&field_of(&clay), top) - base;
    assert!((two / one - 2.0).abs() <= 0.1, "amount 2 doubles the dab: {one} -> {two}");
    // A dab made at amount 2 is what the brush does (stored at half), as
    // on the base layer.
    let mut plain = sphere(0.02);
    sculpt_on(&mut plain, BASE, &dab);
    let on_base = surface_shift(&field_of(&plain), top) - base;
    let mut at_two = sphere(0.02);
    add_layer(&mut at_two, "L", 2.0);
    sculpt_on(&mut at_two, "L", &dab);
    let shown = surface_shift(&field_of(&at_two), top) - base;
    let stored = layer(&at_two, "L").unwrap().detail.as_ref().unwrap().grid().max_abs();
    assert!((shown / on_base - 1.0).abs() <= 0.05, "a dab at amount 2 moves the surface as on base: {on_base} vs {shown}");
    assert!(stored < 0.6 * plain.detail.as_ref().unwrap().grid().max_abs(), "stored divided by the amount");
    eprintln!("standard dab peak {on_base:.5}; layer at 1 -> 2: {one:.5} -> {two:.5} ({:.3}x); committed at 2: {shown:.5}", two / one);
    // Strokes are refused near amount 0.
    set_amount(&mut at_two, "L", 0.01);
    let v = view_for::<Detail>(&at_two, "L").unwrap();
    let (spec, r) = crate::sculpt::prepare(&dab, None, &ResolveCtx::local()).unwrap().remove(0);
    let e = ledger::commit::<Detail>(&v, &ledger::ledger(&v), &Meta::agent("sculpt", None), &spec, &r, false, None).err().unwrap();
    assert!(e.contains("amount"), "{e}");
}

#[test]
fn bake_keeps_the_surface_and_other_layers_mark_records_stale_until_rebase() {
    let mut clay = sphere(0.02);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","point":[0.3,0.95,0.0],"radius":0.3,"strength":0.3}), json!({"brush":"smooth","point":[0.2,0.97,0.0],"radius":0.3,"strength":0.5})]);
    add_layer(&mut clay, "A", 1.0);
    add_layer(&mut clay, "B", 0.7);
    sculpt_on(&mut clay, "A", &[json!({"brush":"standard","points":[[0.0,1.0,0.0],[0.3,0.95,0.1]],"radius":0.2,"strength":0.3})]);
    sculpt_on(&mut clay, "B", &[json!({"brush":"flatten","point":[0.1,0.99,0.0],"radius":0.25,"strength":0.5}), json!({"brush":"inflate","point":[-0.2,0.97,0.1],"radius":0.2,"strength":0.2})]);
    // Every stroke reads the others (decision Q1): base's and A's were
    // made before B was sculpted, so B makes them stale; B's are current.
    let stale = |c: &Clay, name: &str| {
        let v = view_for::<Detail>(c, name).unwrap();
        let ctx = <Detail as Grid>::context(&v);
        ledger::ledger::<Detail>(&v).records().iter().map(|r| r.stale(ctx, &v)).collect::<Vec<bool>>()
    };
    assert!(stale(&clay, BASE).iter().all(|s| *s) && stale(&clay, "A").iter().all(|s| *s), "strokes made before B was sculpted read another B");
    assert!(stale(&clay, "B").iter().all(|s| !s), "B's own strokes do not make each other stale");
    set_amount(&mut clay, "A", 0.5);
    assert!(stale(&clay, "B").iter().all(|s| *s), "another layer's amount marks them stale");
    assert!(stale(&clay, "A").iter().all(|s| *s), "the layer's own amount too (its strokes are stored divided by it)");
    // Rebase equals a fresh ordered replay in the layers as they are.
    let e = ledger::edit::<Detail>(&clay, &ledger::ledger(&clay), Edit::Rebase, None).unwrap();
    let fresh = ledger::replay(&e.ledger, &clay, e.ledger.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(e.grid.as_ref()), Detail::hash(fresh.as_ref()));
    ledger::install(&mut clay, e.ledger, e.grid);
    assert!(stale(&clay, BASE).iter().all(|s| !s), "rebase clears stale_context");
    // Bake B (at 0.7) into base: the shown surface stays within 1/256 voxel.
    let before = composed_detail(&clay);
    fold(&mut clay, "B", BASE).unwrap();
    ledger::sync_clay(&mut clay).unwrap();
    let after = composed_detail(&clay);
    let mut worst = 0.0f32;
    ledger::detail_changes(before.as_ref(), after.as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert!(worst <= 0.02 / 256.0, "bake keeps the field within 1/256 voxel: {worst}");
    assert!(layer(&clay, "B").is_none() && clay.sculpt.as_ref().unwrap().data().archived.iter().any(|a| a.name == "B" && a.strokes.len() == 2), "B's records are archived");
    let base = ledger::ledger::<Detail>(&clay);
    assert!(base.records().last().unwrap().holds_grid() && base.in_sync(clay.detail.as_ref()), "base records the bake as an opaque record");
    eprintln!("bake: field kept within {:.2e} ({:.4} voxel)", worst, worst / 0.02);
}

#[test]
fn morphs_switch_by_pointer_and_blend_linearly() {
    let mut clay = sphere(0.01);
    add_layer(&mut clay, "L", 1.0);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","point":[0.3,0.95,0.0],"radius":0.3,"strength":0.3})]);
    sculpt_on(&mut clay, "L", &[json!({"brush":"standard","point":[0.0,1.0,0.0],"radius":0.2,"strength":0.3})]);
    let stored = snapshot(&clay, "m");
    ledger::stack_mut(&mut clay).morphs.push(stored.clone());
    sculpt_on(&mut clay, BASE, &[json!({"brush":"carve","points":[[0.0,0.9,0.4],[0.3,0.9,0.3]],"radius":0.1,"strength":0.5})]);
    sculpt_on(&mut clay, "L", &[json!({"brush":"inflate","point":[-0.3,0.95,0.0],"radius":0.2,"strength":0.3})]);
    let (now_base, now_l) = (clay.detail.clone(), layer(&clay, "L").unwrap().detail.clone());
    // Switch to the morph and back: the very grids, ledgers too.
    let m = clay.sculpt.as_ref().unwrap().data().morphs[0].clone();
    let old = apply_morph(&mut clay, &m).unwrap();
    assert!(clay.detail == stored.base && layer(&clay, "L").unwrap().detail == stored.layers[0].detail);
    assert!(ledger::ledger::<Detail>(&clay) == stored.base_strokes && ledger::ledger::<Detail>(&clay).in_sync(clay.detail.as_ref()));
    let back = apply_morph(&mut clay, &old).unwrap();
    assert!(clay.detail == now_base && layer(&clay, "L").unwrap().detail == now_l, "switching back is pointer-equal");
    assert!(back.base == stored.base);
    assert_eq!(ledger::sync_clay(&mut clay), Ok(false), "switches record nothing: the ledgers come with the grids");
    // Blend 0.5: the field halfway, within 1e-4.
    let (fa, fb) = (field_of(&clay), field_of(&Clay { detail: m.composed(), sculpt: None, ..clay.clone() }));
    blend(&mut clay, &[(&m, 0.5)]).unwrap();
    ledger::sync_clay(&mut clay).unwrap();
    let mid = field_of(&clay);
    let mut worst = 0.0f32;
    for p in cap_points(500) {
        for t in [-0.03f32, 0.0, 0.03] {
            let q = p * (1.0 + t);
            worst = worst.max((field_at(&mid, q) - 0.5 * (field_at(&fa, q) + field_at(&fb, q))).abs());
        }
    }
    assert!(worst <= 1e-4, "blend 0.5 is the field midpoint: {worst}");
    eprintln!("morph blend 0.5: field midpoint within {worst:.2e}");
}

#[test]
fn a_layer_view_counts_the_ledger_it_stands_in_for() {
    // A stroke on a layer is held to the clay's whole point cap: the view
    // counts base's points (which its own ledger takes the place of).
    let mut clay = sphere(0.01);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","points":[[0.3,0.95,0.0],[0.2,0.97,0.1]],"radius":0.3,"strength":0.3})]);
    add_layer(&mut clay, "L", 1.0);
    sculpt_on(&mut clay, "L", &[json!({"brush":"standard","point":[0.0,1.0,0.0],"radius":0.2,"strength":0.3})]);
    ledger::stack_mut(&mut clay).layers.push(Layer::new("P", Kind::Paint, 1.0));
    let (base, l) = (ledger::ledger::<Detail>(&clay).points(), layer(&clay, "L").unwrap().strokes.points());
    assert!(base > 0 && l > 0);
    let v = view(&clay, "L").unwrap();
    assert_eq!(view_of(&v).unwrap().points, base, "a sculpt view: base's points");
    let v = view(&clay, "P").unwrap();
    assert_eq!(view_of(&v).unwrap().points, l + ledger::ledger::<Paint>(&clay).points(), "a paint view: the other layers' and paint's");
}

#[test]
fn morph_grids_stay_shared_through_a_relayout_and_count_per_morph_in_a_file() {
    let voxel = 0.004;
    let mut clay = sphere(voxel);
    clay.detail = Some(bricks(256, voxel, 0.0));
    add_layer(&mut clay, "L", 1.0);
    ledger::stack_mut(&mut clay).layers[0].detail = Some(bricks(256, voxel, 1.0));
    let grids = saved_bytes(&clay);
    // The ledgers record the grids as outside edits: those save too.
    ledger::sync_clay(&mut clay).unwrap();
    let one = saved_bytes(&clay);
    assert_eq!(one, grids * 2, "each ledger's Opaque record holds its whole grid");
    for k in 0..3 {
        let m = snapshot(&clay, &format!("m{k}"));
        ledger::stack_mut(&mut clay).morphs.push(m);
    }
    // In memory a morph is free; in a file it writes its grids again.
    assert_eq!(saved_bytes(&clay), one * 4, "each morph saves base and L, grids and ledgers, again");
    let e = check_saved_within(&clay, one * 4 - 1).unwrap_err();
    assert!(e.contains("saved file") && e.contains("morphs") && e.contains("squash"), "{e}");
    // A new detail cell relays each shared grid once: the morphs still
    // share the clay's.
    clay.set_detail_cell(voxel * 0.5).unwrap();
    let s = clay.sculpt.as_ref().unwrap().data();
    let l = s.layers[0].detail.as_ref().unwrap();
    for m in &s.morphs {
        assert!(m.base.as_ref().unwrap().addr() == clay.detail.as_ref().unwrap().addr() && m.layers[0].detail.as_ref().unwrap().addr() == l.addr());
    }
    let without = Clay { sculpt: Some(ledger::SculptStack::new(StackData { morphs: Vec::new(), ..s.clone() })), ..clay.clone() };
    assert_eq!(grid_bytes(&clay), grid_bytes(&without), "the morphs add nothing in memory after the relayout");
    check_budget(&clay).unwrap();
}

#[test]
fn the_budget_counts_what_sync_records() {
    // Grids that fit, whose ledgers have not recorded them yet: sync adds
    // an Opaque record of each whole grid, which doubles the saved size.
    let voxel = 0.004;
    let mut clay = sphere(voxel);
    clay.detail = Some(bricks(256, voxel, 0.0));
    add_layer(&mut clay, "L", 1.0);
    ledger::stack_mut(&mut clay).layers[0].detail = Some(bricks(256, voxel, 1.0));
    let grids = saved_bytes(&clay);
    check_saved_within(&clay, grids).unwrap();
    // Checked before syncing (the old order) this passed; with what sync
    // records it is refused. Callers hold a copy and put it back only on
    // Ok, so the refusal leaves the scene's clay as it was.
    let mut copy = clay.clone();
    let e = sync_within(&mut copy, MAX_GRID_BYTES, grids).unwrap_err();
    assert!(e.contains("saved file"), "{e}");
    assert!(clay.sculpt.as_ref().unwrap().data().base.is_empty(), "the original is untouched");
    // Room for the records: it syncs and passes.
    let mut copy = clay.clone();
    assert!(sync_within(&mut copy, MAX_GRID_BYTES, grids * 2).unwrap());
    assert_eq!(saved_bytes(&copy), grids * 2);
    assert!(sync_within_budget(&mut clay).unwrap());
}

#[test]
fn a_plain_clay_budgets_what_sync_records() {
    // No layers or morphs: each relayout (a Sculptris stroke growing the
    // regions, an edit_clay detail_regions) records the whole grid, and
    // those records count against the saved budget too.
    let voxel = 0.004;
    let mut clay = sphere(voxel);
    clay.detail = Some(bricks(256, voxel, 0.0));
    // A stack with no layers or morphs (as the first stroke makes).
    ledger::stack_mut(&mut clay);
    assert!(stack(&clay).is_none_or(|s| s.layers.is_empty() && s.morphs.is_empty()));
    let grids = saved_bytes(&clay);
    let mut copy = clay.clone();
    let e = sync_within(&mut copy, MAX_GRID_BYTES, grids).unwrap_err();
    assert!(e.contains("saved file"), "{e}");
    let mut copy = clay.clone();
    assert!(sync_within(&mut copy, MAX_GRID_BYTES, grids * 2).unwrap());
    assert_eq!(saved_bytes(&copy), grids * 2, "the Opaque record holds the whole grid");
    // A second relayout's record does not fit the same budget.
    copy.detail = Some(bricks(256, voxel, 0.5));
    let e = sync_within(&mut copy.clone(), MAX_GRID_BYTES, grids * 2).unwrap_err();
    assert!(e.contains("saved file"), "{e}");
    assert!(sync_within(&mut copy, MAX_GRID_BYTES, grids * 3).unwrap());
}

#[test]
fn layers_replay_through_save_and_load_and_mirror() {
    // Strokes on one layer only: they replay against the same others.
    let mut clay = sphere(0.02);
    add_layer(&mut clay, "L", 0.8);
    sculpt_on(&mut clay, "L", &[json!({"brush":"standard","points":[[0.0,1.0,0.0],[0.3,0.95,0.1]],"radius":0.2,"strength":0.3})]);
    ledger::stack_mut(&mut clay).layers.push(Layer::new("colors", Kind::Paint, 0.6));
    let m = snapshot(&clay, "m");
    ledger::stack_mut(&mut clay).morphs.push(m);
    let text = serde_json::to_string(&clay).unwrap();
    let mut back: Clay = serde_json::from_str(&text).unwrap();
    back.sanitize();
    assert!(back.sculpt.is_some(), "a sane stack reads back");
    ledger::after_load(&mut back);
    let l = layer(&back, "L").unwrap();
    assert_eq!(l.strokes.verified(), Some(true), "the layer's ledger replays to its saved grid");
    assert_eq!(Detail::hash(l.detail.as_ref()), Detail::hash(layer(&clay, "L").unwrap().detail.as_ref()), "and puts the exact grid back");
    assert_eq!(layer(&back, "L").unwrap().amount, 0.8);
    assert_eq!(back.sculpt.as_ref().unwrap().data().morphs.len(), 1);
    // Mirrored: the composite of the mirror is the mirror of the composite.
    let m = clay.mirrored_x();
    let mut worst = 0.0f32;
    ledger::detail_changes(composed_detail(&m).as_ref(), composed_detail(&clay).map(|d| d.mirrored_x()).as_ref(), |_, _, u, v| worst = worst.max((u - v).abs()));
    assert_eq!(worst, 0.0);
    // A file with more layers than a clay may hold loses its stack (the
    // clay and its own grids stay).
    let mut v: Value = serde_json::from_str(&text).unwrap();
    let one = v["sculpt"]["layers"][0].clone();
    v["sculpt"]["layers"] = json!((0..MAX_LAYERS + 4).map(|i| { let mut l = one.clone(); l["name"] = json!(format!("L{i}")); l }).collect::<Vec<_>>());
    let mut hostile: Clay = serde_json::from_value(v).unwrap();
    hostile.sanitize();
    assert!(hostile.sculpt.is_none() && !hostile.blobs.is_empty());
}

#[test]
fn layer_strokes_are_the_same_on_one_and_twelve_threads() {
    let strokes: Vec<Value> = (0..12).map(|i| {
        let a = i as f32 * 0.5;
        let p = [0.4 * a.cos(), 0.9, 0.4 * a.sin()];
        match i % 4 {
            0 => json!({"brush":"standard","point":p,"radius":0.12,"strength":0.4}),
            1 => json!({"brush":"smooth","point":p,"radius":0.15,"strength":0.5}),
            2 => json!({"brush":"add","points":[p,[0.0,1.0,0.0]],"radius":0.05}),
            _ => json!({"brush":"clay_buildup","point":p,"radius":0.1,"strength":0.4}),
        }
    }).collect();
    let run = |threads: usize| {
        crate::clay::with_threads(threads, || {
            let mut clay = sphere(0.02);
            sculpt_on(&mut clay, BASE, &strokes[..3]);
            add_layer(&mut clay, "L", 0.6);
            sculpt_on(&mut clay, "L", &strokes[3..]);
            (Detail::hash(layer(&clay, "L").unwrap().detail.as_ref()), Detail::hash(composed_detail(&clay).as_ref()))
        })
    };
    assert_eq!(run(1), run(12));
}

/// A grid of `n` bricks of small offsets around the unit sphere's cap.
fn bricks(n: usize, voxel: f32, seed: f32) -> Detail {
    let mut d = Detail::new(voxel);
    let g = d.grid_mut();
    let side = (n as f32).sqrt().ceil() as i32;
    let top = (1.0 / voxel / 8.0).floor() as i32;
    for k in 0..n as i32 {
        let (x, z) = (k % side - side / 2, k / side - side / 2);
        for i in 0..512 {
            let l = [i % 8, i / 8 % 8, i / 64];
            g.set([x * 8 + l[0], top * 8 + l[1], z * 8 + l[2]], voxel * 0.1 * ((i as f32 * 0.37 + seed).sin()));
        }
    }
    d
}

#[test]
fn eight_layers_fit_the_budget_and_compose_cheaply() {
    // An 8-layer fixture at the base cell: base and 8 layers of 4,096
    // bricks each (72 MB of the 512 MB budget); a morph of it adds nothing.
    let voxel = 0.002;
    let mut clay = sphere(voxel);
    clay.detail = Some(bricks(4096, voxel, 0.0));
    for k in 0..8 {
        add_layer(&mut clay, &format!("L{k}"), 0.5 + k as f32 * 0.1);
        ledger::stack_mut(&mut clay).layers[k].detail = Some(bricks(4096, voxel, k as f32 + 1.0));
    }
    let bytes = grid_bytes(&clay);
    assert_eq!(bytes, 9 * 4096 * 2048);
    check_budget(&clay).unwrap();
    let m = snapshot(&clay, "m");
    ledger::stack_mut(&mut clay).morphs.push(m);
    assert_eq!(grid_bytes(&clay), bytes, "a morph shares every brick");
    assert!(MAX_GRID_BYTES / (9 * 4096 * 2048) >= 7, "8 such layers fit seven times over");
    // Past the budget the error says what to do.
    let e = check_budget_within(&clay, bytes - 1).unwrap_err();
    assert!(e.contains("bake") && e.contains("coarser detail_cell"), "{e}");
    // Composite update for an amount change on one 10k-brick layer: only
    // its bricks are summed again (remesh not included).
    let mut big = sphere(voxel);
    big.detail = Some(bricks(10_000, voxel, 0.0));
    add_layer(&mut big, "big", 1.0);
    ledger::stack_mut(&mut big).layers[0].detail = Some(bricks(10_000, voxel, 3.0));
    let old = composed_detail(&big);
    let (new, ms) = perf::time(5, || recompose(&big, "big", 0.7, old.as_ref()).unwrap());
    let mut moved = big.clone();
    set_amount(&mut moved, "big", 0.7);
    assert_eq!(Detail::hash(new.as_ref()), Detail::hash(composed_detail(&moved).as_ref()), "the update equals composing anew");
    let (_, fresh_ms) = perf::time(3, || Detail::compose(&[(big.detail.as_ref().unwrap(), 1.0), (layer(&big, "big").unwrap().detail.as_ref().unwrap(), 0.7)]).unwrap());
    eprintln!("composite update for an amount change on a 10k-brick layer: {ms:.2} ms (composing anew {fresh_ms:.2} ms)");
    // Composing anew takes about 4.2 ms on a quiet machine: a slower run
    // says how loaded the machine is (the whole suite runs in parallel).
    // Strict runs (CLIDE_PERF=1, a quiet machine) get no allowance.
    let load = if perf::strict() { 1.0 } else { (fresh_ms / 4.2).max(1.0) };
    perf::check("layers composite update, 10k-brick layer", ms, 10.0 * load, json!({ "bricks": 10_000, "compose_ms": fresh_ms, "load": load }));
}

#[test]
fn composing_eight_layers_adds_little_to_a_remesh() {
    // The same sculpt as one base grid and as base + 8 layers: the mesh
    // build reads the composite (one grid), composed once.
    let mut one = sphere(0.01);
    one.resolution = 96;
    let strokes: Vec<Value> = (0..9).map(|i| {
        let a = i as f32 * 0.7;
        json!({"brush":"inflate","point":[0.6 * a.cos(), 0.75, 0.6 * a.sin()],"radius":0.25,"strength":0.3})
    }).collect();
    sculpt_on(&mut one, BASE, &strokes);
    let mut layered = sphere(0.01);
    layered.resolution = 96;
    sculpt_on(&mut layered, BASE, &strokes[..1]);
    for (k, s) in strokes[1..].iter().enumerate() {
        add_layer(&mut layered, &format!("L{k}"), 1.0);
        sculpt_on(&mut layered, &format!("L{k}"), std::slice::from_ref(s));
    }
    let plain = || composed_clay(&one).build_mesh_with(&[], "");
    let composed = || {
        // A cold composite each time (a copied stack starts with no cache).
        let c = Clay { sculpt: Some(ledger::SculptStack::new(layered.sculpt.as_ref().unwrap().data().clone())), ..layered.clone() };
        composed_clay(&c).build_mesh_with(&[], "")
    };
    // Timed in turns, so load from the rest of the suite falls on both.
    let (mut base, mut lay, mut mesh) = (Vec::new(), Vec::new(), None);
    for _ in 0..5 {
        base.push(perf::time(1, plain).1);
        let (m, ms) = perf::time(1, composed);
        lay.push(ms);
        mesh = Some(m);
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (base_ms, layered_ms, mesh) = (median(base), median(lay), mesh.expect("ran"));
    assert!(!mesh.vertices.is_empty());
    eprintln!("remesh: {base_ms:.1} ms one grid, {layered_ms:.1} ms base + 8 layers composed ({:+.1}%)", (layered_ms / base_ms - 1.0) * 100.0);
    perf::check("layers remesh with 8 layers composed", layered_ms, base_ms * 1.15, json!({ "one_grid_ms": base_ms, "vertices": mesh.vertices.len() }));
}

#[test]
fn blend_shapes_follow_their_morph_and_a_long_grab_is_refused() {
    use crate::scene::{Scene, Shape};
    let mut scene = Scene::default();
    let id = scene.add(Shape::Clay);
    let mut clay = sphere(0.02);
    sculpt_on(&mut clay, BASE, &[json!({"brush":"inflate","point":[0.3,0.95,0.0],"radius":0.3,"strength":0.2})]);
    let rest = clay.clone();
    // A morph of a small change (within a couple of cells).
    sculpt_on(&mut clay, BASE, &[json!({"brush":"standard","points":[[0.0,1.0,0.0],[0.4,0.9,0.0]],"radius":0.25,"strength":0.25}), json!({"brush":"inflate","point":[-0.3,0.93,0.2],"radius":0.3,"strength":0.25})]);
    let smile = snapshot(&clay, "smile");
    // And of a pull three radii long (grab moves at most a radius per
    // stroke; stretch pulls further).
    let mut pulled = rest.clone();
    sculpt_on(&mut pulled, BASE, &[json!({"brush":"stretch","point":[0.0,1.0,0.0],"radius":0.12,"delta":[0.0,0.36,0.0]})]);
    let horn = snapshot(&pulled, "horn");
    let mut shown = rest.clone();
    ledger::stack_mut(&mut shown).morphs = vec![smile, horn];
    scene.get_mut(id).unwrap().clay = Some(shown.clone());
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    let mesh = meshes.rest(id).unwrap().0.clone();
    let positions: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| v.pos).collect();
    let cell = shown.cell_size().unwrap();
    let fit = |name: &str| project_shape(&shape_target(&shown, name).unwrap().unwrap(), &positions, &mesh.indices);
    let (s, h) = (fit("morph:smile"), fit("morph:horn"));
    assert!(s.residual() <= 0.5 * cell, "the small morph is followed: residual {} (cell {cell})", s.residual());
    assert!(h.residual() > 0.5 * cell, "the long grab is refused: residual {} (cell {cell})", h.residual());
    eprintln!("blend shapes: smile residual {:.5} (vertex {:.5}, coverage {:.5}), horn residual {:.5} (vertex {:.5}, coverage {:.5}); half a cell {:.5}", s.residual(), s.vertex_residual, s.coverage_residual, h.residual(), h.vertex_residual, h.coverage_residual, 0.5 * cell);
    // Exported and read back with the target on: same vertex count, every
    // vertex within half a cell of the morph's field.
    let opts = crate::gltf::Options { roots: vec![], animations: false, fps: 24.0, file_stem: String::new(), lods: false, atlas: false, texel_density: None, gpu_instancing: false, blend_shapes: vec![crate::gltf::BlendShape { object: id, name: "morph:smile".into(), deltas: std::sync::Arc::new(s.deltas.clone()) }] };
    let out = crate::gltf::export(&scene, &meshes, &opts).unwrap();
    let plain = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
    let on = crate::glb_read::from_bytes_morphed(&out.bytes, &[("morph:smile".to_string(), 1.0)].into_iter().collect()).unwrap();
    assert_eq!(plain.summary["vertices"], json!(mesh.vertices.len()));
    assert_eq!(on.summary["vertices"], json!(mesh.vertices.len()), "{}", on.summary);
    assert_eq!(on.summary["meshes"][0]["morph_targets"], json!(["morph:smile"]));
    let target = field_of(&Clay { detail: shown.sculpt.as_ref().unwrap().data().morphs[0].composed(), sculpt: None, ..shown.clone() });
    let worst = on.items[0].mesh.vertices.iter().map(|v| field_at(&target, Vec3::from(v.pos)).abs()).fold(0.0f32, f32::max);
    assert!(worst <= 0.5 * cell, "vertices with the target applied lie within half a cell of the morph: {worst}");
    let moved = plain.items[0].mesh.vertices.iter().zip(&on.items[0].mesh.vertices).filter(|(a, b)| a.pos != b.pos).count();
    assert!(moved > 0);
    // The FBX carries a BlendShape deformer with one channel.
    let fbx = crate::fbx::export(&scene, &meshes, &opts).unwrap();
    let find = |needle: &[u8]| fbx.bytes.windows(needle.len()).any(|w| w == needle);
    assert!(find(b"BlendShapeChannel") && find(b"BlendShape") && find(b"FullWeights"));
    eprintln!("glb with morph:smile: {} vertices, worst |field| {worst:.5} after applying it", mesh.vertices.len());
}
