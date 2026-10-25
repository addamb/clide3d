//! Stage 8a acceptance (docs/SCULPT_PLAN.md 6): detail levels and detail
//! transport, on clays directly.

use glam::Vec3;
use serde_json::{Value, json};

use crate::brush::resolve::{ResolveCtx, Resolved, resolve};
use crate::brush::spec::StrokeSpec;
use crate::clay::{Clay, Region};
use crate::ledger;

/// A ball of radius 0.5 with 2 cm detail cells and a 5 mm level over its top.
fn ball() -> Clay {
    let mut c: Clay = serde_json::from_value(json!({ "resolution": 64, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }] })).unwrap();
    c.detail_cell = Some(0.02);
    c.set_detail_regions(&[Region { name: "top".into(), center: Vec3::new(0.0, 0.5, 0.0), radii: Vec3::splat(0.35), cell: 0.005 }]).unwrap();
    c
}

/// The ball with levels 0.02 (its own cell), 0.01 and 0.005.
fn leveled() -> Clay {
    let mut c = ball();
    ledger::stack_mut(&mut c).levels = vec![0.01, 0.005];
    assert_eq!(crate::levels::cells(&c), vec![0.02, 0.01, 0.005]);
    c
}

fn resolved(c: &Clay, spec: &Value) -> Resolved {
    let cells = crate::levels::cells(c);
    let spec: StrokeSpec = serde_json::from_value(spec.clone()).unwrap();
    resolve(&spec, &ResolveCtx { levels: Some(&cells), ..ResolveCtx::local() }).unwrap()
}

fn stroke(c: &mut Clay, spec: Value) {
    let r = resolved(c, &spec);
    super::run(c, &r).unwrap();
}

/// `f` with detail transport off.
fn uncarried<T>(f: impl FnOnce() -> T) -> T {
    super::engine::NO_CARRY.with(|c| c.set(true));
    let out = f();
    super::engine::NO_CARRY.with(|c| c.set(false));
    out
}

fn fine(c: &Clay, p: Vec3) -> f32 {
    c.detail.as_ref().unwrap().grid().levels()[0].grid.level_offset(p)
}

fn corr(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().map(|v| *v as f64).sum::<f64>() / n, b.iter().map(|v| *v as f64).sum::<f64>() / n);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - ma) * (*y as f64 - mb)).sum();
    let va: f64 = a.iter().map(|x| (*x as f64 - ma).powi(2)).sum();
    let vb: f64 = b.iter().map(|y| (*y as f64 - mb).powi(2)).sum();
    cov / (va * vb).sqrt().max(1e-30)
}

/// Pores at level 2, then a level-0 grab of half its radius across the
/// top: in the moved frame (each point against where the grab's map says
/// its material came from) the pore pattern correlates >= 0.85 with
/// transport, and <= 0.3 left where it was (the limit without it).
#[test]
fn a_coarse_grab_carries_finer_pores_along() {
    let mut c = leveled();
    stroke(&mut c, json!({ "brush": "noise", "points": [[0, 0.5, 0]], "radius": 0.3, "strength": 1.0, "scale": 0.015, "level": 2 }));
    let before = c.clone();
    let grab = json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.2, "strength": 1.0, "delta": [0.1, 0, 0], "level": 0 });
    let r = resolved(&c, &grab);
    assert_eq!(r.params.level, Some(0.02));
    let t = std::time::Instant::now();
    let mut with = before.clone();
    super::run(&mut with, &r).unwrap();
    let took = t.elapsed();
    let mut without = before.clone();
    uncarried(|| super::run(&mut without, &r).unwrap());
    // The level-0 grab wrote the coarse grid only: the same either way.
    assert!(with.detail.as_ref().unwrap().grid().bricks().eq(without.detail.as_ref().unwrap().grid().bricks()));
    let map = super::engine::transport(&r.params, &r.dabs[0], 0.2, 1.0).unwrap();
    let (mut was, mut carried, mut left) = (Vec::new(), Vec::new(), Vec::new());
    for i in -10..=10 {
        for k in -10..=10 {
            let (x, z) = (0.05 + i as f32 * 0.003, k as f32 * 0.003);
            let p = Vec3::new(x, (0.25 - x * x - z * z).sqrt(), z);
            was.push(fine(&before, map.source(p)));
            carried.push(fine(&with, p));
            left.push(fine(&without, p));
        }
    }
    let (yes, no) = (corr(&was, &carried), corr(&was, &left));
    eprintln!("pore correlation in the moved frame: {yes:.3} carried, {no:.3} left in place; grab {took:?}");
    assert!(yes >= 0.85, "{yes}");
    assert!(no <= 0.3, "{no}");
}

/// Transport on a clay with sculpt layers moves only the written layer's
/// finer detail: the engine runs on the composite of two layers, each
/// with pores of its own at level 2, and a level-0 grab on either one
/// gives the other layer's pores where they were plus the written
/// layer's pores moved as a grab on that layer alone moves them. (A
/// carry of the composite would merge the other layer's moved pores into
/// the written one: hiding that layer would then leave them showing.)
#[test]
fn transport_moves_only_the_written_layers_detail() {
    use super::engine::{Ctx, Gate, apply};
    use crate::clay::Detail;
    let c = leveled();
    let layer = |scale: f32, at: [f32; 3]| {
        let mut p = c.clone();
        stroke(&mut p, json!({ "brush": "noise", "points": [at], "radius": 0.3, "strength": 1.0, "scale": scale, "level": 2 }));
        p.detail.unwrap()
    };
    let (base, pores) = (layer(0.02, [0.03, 0.5, 0.0]), layer(0.015, [0.0, 0.5, 0.0]));
    let composite = Detail::compose(&[(&base, 1.0), (&pores, 1.0)]).unwrap();
    let r = resolved(&c, &json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.2, "strength": 1.0, "delta": [0.1, 0, 0], "level": 0 }));
    let run = |grid: &Detail, others: Option<&Detail>| apply(&Ctx { clay: &c, params: &r.params, alpha: None, gate: &Gate::default(), others }, Some(grid), &r.dabs, None).unwrap().detail;
    let at = |d: &Detail, p: Vec3| d.grid().levels()[0].grid.level_offset(p);
    for (what, written, other) in [("base", &base, &pores), ("pores", &pores, &base)] {
        let layered = run(&composite, Some(other));
        let alone = run(written, None);
        let (mut worst, mut moved) = (0.0f32, 0.0f32);
        for i in -10..=10 {
            for k in -10..=10 {
                let (x, z) = (0.05 + i as f32 * 0.003, k as f32 * 0.003);
                let p = Vec3::new(x, (0.25 - x * x - z * z).sqrt(), z);
                worst = worst.max((at(&layered, p) - at(other, p) - at(&alone, p)).abs());
                moved = moved.max((at(&alone, p) - at(written, p)).abs());
            }
        }
        assert!(moved > 1e-3, "the grab on {what} moves its pores: {moved}");
        assert!(worst <= 1e-5, "on {what}: the other layer's pores stay, the written layer's move as alone: off by {worst}");
    }
    // Brushes that move the surface carry along its normal: a level-0
    // inflate on base, which holds no finer detail, leaves the pores'
    // layer's finer level exactly as it was.
    let bare = c.detail.clone().unwrap();
    let composite = Detail::compose(&[(&pores, 1.0), (&bare, 1.0)]).unwrap();
    let r = resolved(&c, &json!({ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.15, "strength": 0.5, "level": 0 }));
    let out = apply(&Ctx { clay: &c, params: &r.params, alpha: None, gate: &Gate::default(), others: Some(&pores) }, Some(&composite), &r.dabs, None).unwrap().detail;
    assert_ne!(out.grid().bricks().count(), 0);
    assert!(out.grid().levels()[0].grid.bricks().eq(composite.grid().levels()[0].grid.bricks()), "the inflate moved no pores into base");
}

/// The coarse surface's point along `dir` (the blobs and the coarse grid,
/// without the finer level).
fn coarse_surface(c: &Clay, dir: Vec3) -> Vec3 {
    let g = c.detail.as_ref().unwrap().grid();
    let f = |r: f32| r - 0.5 + g.level_offset(dir * r);
    let (mut a, mut b) = (0.35f32, 0.7f32);
    assert!(f(a) < 0.0 && f(b) > 0.0);
    for _ in 0..40 {
        let m = 0.5 * (a + b);
        if f(m) < 0.0 { a = m } else { b = m }
    }
    dir * (0.5 * (a + b))
}

/// RMS of the finer level (the high-pass) on the coarse surface over the
/// top cap.
fn high_pass_rms(c: &Clay) -> f64 {
    let mut sum = 0.0;
    let mut n = 0;
    for i in -12..=12 {
        for k in -12..=12 {
            let dir = Vec3::new(i as f32 * 0.006, 1.0, k as f32 * 0.006).normalize();
            sum += (fine(c, coarse_surface(c, dir)) as f64).powi(2);
            n += 1;
        }
    }
    (sum / n as f64).sqrt()
}

/// Coarse grab, then fine noise, then a coarse edit that lifts the
/// surface by more than the noise's band: the finer level rides on the
/// moved surface, its high-pass RMS within 5% (without transport the
/// edit leaves it behind, off the surface).
#[test]
fn a_coarse_edit_keeps_the_finer_detail_on_the_surface() {
    let mut c = leveled();
    stroke(&mut c, json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.2, "strength": 1.0, "delta": [0.03, 0.02, 0], "level": 0 }));
    stroke(&mut c, json!({ "brush": "noise", "points": [[0, 0.5, 0]], "radius": 0.3, "strength": 1.0, "scale": 0.02, "pattern": "bumps", "level": 2 }));
    let rms = high_pass_rms(&c);
    let edit = json!({ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.2, "strength": 0.6, "level": 0 });
    let r = resolved(&c, &edit);
    let mut with = c.clone();
    let t = std::time::Instant::now();
    super::run(&mut with, &r).unwrap();
    let took = t.elapsed();
    let mut without = c.clone();
    uncarried(|| super::run(&mut without, &r).unwrap());
    let lift = (coarse_surface(&with, Vec3::Y) - coarse_surface(&c, Vec3::Y)).length();
    let (a, b) = (high_pass_rms(&with), high_pass_rms(&without));
    eprintln!("high-pass RMS {rms:.5}; after a coarse lift of {lift:.4}: {a:.5} carried ({:.3}), {b:.5} not ({:.3}); edit {took:?}", a / rms, b / rms);
    assert!(lift > 0.03, "the edit lifts the surface past the noise band: {lift}");
    assert!((a / rms - 1.0).abs() <= 0.05, "{a} vs {rms}");
    assert!(b / rms < 0.7, "without transport the detail stays behind: {b} vs {rms}");
}

/// A stroke at a level, dab by dab, gives what it gives at once, and the
/// same on any number of threads (carried levels included).
#[test]
fn a_leveled_stroke_matches_its_dabs_and_any_thread_count() {
    let mut c = leveled();
    stroke(&mut c, json!({ "brush": "noise", "points": [[0, 0.5, 0]], "radius": 0.25, "strength": 1.0, "scale": 0.015, "level": 2 }));
    for spec in [
        json!({ "brush": "standard", "points": [[-0.05, 0.5, 0], [0.05, 0.49, 0.02]], "radius": 0.06, "strength": 0.5, "spacing": 0.3, "level": 0 }),
        json!({ "brush": "inflate", "points": [[-0.05, 0.5, 0], [0.05, 0.49, 0.02]], "radius": 0.06, "strength": 0.5, "level": 0 }),
        json!({ "brush": "clay", "points": [[-0.05, 0.5, 0], [0.05, 0.49, 0.02]], "radius": 0.06, "strength": 0.5, "level": 1 }),
        json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.1, "delta": [0.02, 0.01, 0], "level": 0 }),
        json!({ "brush": "snakehook", "points": [[0, 0.5, 0], [0.01, 0.53, 0], [0.02, 0.56, 0]], "radius": 0.05, "level": 0 }),
    ] {
        let r = resolved(&c, &spec);
        let whole = crate::clay::with_threads(1, || {
            let mut a = c.clone();
            super::run(&mut a, &r).unwrap();
            a
        });
        let many = crate::clay::with_threads(3, || {
            let mut a = c.clone();
            super::run(&mut a, &r).unwrap();
            a
        });
        let timed = |carry: bool| {
            let mut a = c.clone();
            let t = std::time::Instant::now();
            if carry { super::run(&mut a, &r).unwrap(); } else { uncarried(|| super::run(&mut a, &r).unwrap()); }
            t.elapsed().as_secs_f64() * 1000.0 / r.dabs.len() as f64
        };
        eprintln!("{}: {} dabs, {:.2} ms per dab carried, {:.2} ms not", spec["brush"], r.dabs.len(), timed(true), timed(false));
        let mut one = c.clone();
        for dab in &r.dabs {
            let mut d = r.clone();
            d.dabs = vec![dab.clone()];
            super::run(&mut one, &d).unwrap();
        }
        let hash = |c: &Clay| <crate::clay::Detail as ledger::Grid>::hash(c.detail.as_ref());
        assert!(r.dabs.len() > 1 || spec["brush"] != "standard", "{spec}");
        assert_eq!(hash(&whole), hash(&many), "{spec}");
        assert_eq!(hash(&whole), hash(&one), "{spec}");
        assert_ne!(hash(&whole), hash(&c), "{spec}");
    }
}

/// On a refined level a brush applies once (version 1 applied inflate,
/// grab and the others that read the grid a second time there, over
/// what the coarse grid had just taken): the same stroke inside a 5 mm
/// region and on the coarse grid alone moves the surface alike (smooth
/// is left out: its passes are counted in cells, so it smooths a finer
/// level less by design).
#[test]
fn a_stroke_on_a_refined_level_applies_once() {
    for brush in ["inflate", "standard", "clay", "grab"] {
        let mut a = ball();
        let mut b = ball();
        b.set_detail_regions(&[]).unwrap();
        // Something to smooth first.
        for c in [&mut a, &mut b] {
            stroke(c, json!({ "brush": "standard", "points": [[0.02, 0.5, 0]], "radius": 0.05, "strength": 0.5 }));
        }
        let s = json!({ "brush": brush, "points": [[0, 0.5, 0]], "radius": 0.1, "strength": 0.5, "delta": [0.03, 0, 0] });
        let (a0, b0) = (a.clone(), b.clone());
        stroke(&mut a, s.clone());
        stroke(&mut b, s);
        for x in [0.0f32, 0.02, 0.05] {
            let p = Vec3::new(x, (0.25f32 - x * x).sqrt(), 0.0);
            let moved = |c: &Clay, c0: &Clay| c.detail.as_ref().unwrap().grid().offset(p) - c0.detail.as_ref().unwrap().grid().offset(p);
            let (fine, coarse) = (moved(&a, &a0), moved(&b, &b0));
            assert!((fine - coarse).abs() <= 0.25 * coarse.abs() + 0.002, "{brush} at x {x}: {fine} on the region, {coarse} on the coarse grid");
        }
    }
}

/// One fixture per kernel version since detail transport (version 2):
/// strokes on a refined level and at levels, pinned by the grid's hash. A
/// kernel change that moves it must raise `ENGINE_VERSION` and add a row.
const LEVEL_FIXTURES: [(u16, u128); 1] = [(2, 0xebbd_2844_a514_bb29_1209_b7e2_e22c_f20e)];

#[test]
fn the_engine_version_pins_a_level_fixture_hash() {
    let mut c = leveled();
    for s in [
        json!({ "brush": "noise", "points": [[0, 0.5, 0]], "radius": 0.15, "strength": 1.0, "scale": 0.015, "level": 2 }),
        json!({ "brush": "inflate", "points": [[0, 0.5, 0]], "radius": 0.1, "strength": 0.4 }),
        json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.1, "delta": [0.03, 0.01, 0], "level": 0 }),
        json!({ "brush": "clay", "points": [[0.03, 0.5, 0]], "radius": 0.06, "strength": 0.5, "level": 0 }),
    ] {
        stroke(&mut c, s);
    }
    let hash = <crate::clay::Detail as ledger::Grid>::hash(c.detail.as_ref());
    let pinned = LEVEL_FIXTURES.iter().find(|(v, _)| *v == super::engine::ENGINE_VERSION).expect("a fixture for this engine version").1;
    assert_eq!(hash, ledger::Hash128(pinned), "the brush kernel changed its output: raise ENGINE_VERSION and pin {hash}");
}

/// Dab timings on a refined level and at levels (p50 / p95 per dab), and
/// the worst case: a big coarse smooth carrying a finer level.
/// `cargo test --release level_timing_report -- --ignored --nocapture`
#[test]
#[ignore]
fn level_timing_report() {
    let mut c = leveled();
    stroke(&mut c, json!({ "brush": "noise", "points": [[0, 0.5, 0]], "radius": 0.3, "strength": 1.0, "scale": 0.015, "level": 2 }));
    let path: Vec<Value> = (0..24).map(|i| json!([-0.12 + i as f32 * 0.01, 0.48, 0.03 * (i as f32 * 0.5).sin()])).collect();
    for (what, spec) in [
        ("standard on the 5 mm region (no level)", json!({ "brush": "standard", "points": path, "radius": 0.04, "strength": 0.5, "spacing": 0.15 })),
        ("standard at level 2", json!({ "brush": "standard", "points": path, "radius": 0.04, "strength": 0.5, "spacing": 0.15, "level": 2 })),
        ("standard at level 0 (normal carry)", json!({ "brush": "standard", "points": path, "radius": 0.04, "strength": 0.5, "spacing": 0.15, "level": 0 })),
        ("clay at level 0 (normal carry)", json!({ "brush": "clay", "points": path, "radius": 0.04, "strength": 0.5, "spacing": 0.15, "level": 0 })),
    ] {
        let r = resolved(&c, &spec);
        let mut a = c.clone();
        let mut times = Vec::new();
        for dab in &r.dabs {
            let mut d = r.clone();
            d.dabs = vec![dab.clone()];
            let t = std::time::Instant::now();
            super::run(&mut a, &d).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
        eprintln!("TIMING {what}: {} dabs, p50 {:.2} ms, p95 {:.2} ms", times.len(), at(0.5), at(0.95));
    }
    for (what, spec) in [
        ("smooth r=0.3 at level 0 (worst case)", json!({ "brush": "smooth", "points": [[0, 0.5, 0]], "radius": 0.3, "strength": 0.5, "level": 0 })),
        ("grab r=0.2 at level 0 (map carry)", json!({ "brush": "grab", "points": [[0, 0.5, 0]], "radius": 0.2, "delta": [0.1, 0, 0], "level": 0 })),
    ] {
        let r = resolved(&c, &spec);
        let mut a = c.clone();
        let t = std::time::Instant::now();
        super::run(&mut a, &r).unwrap();
        eprintln!("TIMING {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
}
