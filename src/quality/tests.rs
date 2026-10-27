//! Stage 2 acceptance for the field measures (docs/QUALITY_PLAN.md), on
//! synthetic geometry with known answers.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::alpha::{Alpha, AlphaRef, AlphaStore};
use crate::brush::resolve::ResolveCtx;
use crate::clay::Detail;
use crate::ledger::{self, Hash128};

/// The detail cell of the clay fixtures.
const CELL: f32 = 0.01;
/// The fixtures' lumps are this many cells long (lumps 1.5 cells across:
/// the measure reads waves up to about σ in full, longer ones less).
const WAVE: f32 = 3.0;

fn clay(blobs: Value, resolution: u32) -> Clay {
    let mut c: Clay = serde_json::from_value(json!({ "resolution": resolution, "blobs": blobs })).unwrap();
    c.detail_cell = Some(CELL);
    c
}

fn ball(resolution: u32) -> Clay {
    clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3, "blend": 0 }]), resolution)
}

/// Band-limited noise of RMS 1: eight plane waves `wave` long in seeded
/// directions and phases (so its amplitude and size are known).
fn waves(p: Vec3, wave: f32) -> f32 {
    let mut rng = Rng(0x00c0_ffee);
    let k = std::f32::consts::TAU / wave;
    let mut sum = 0.0;
    for _ in 0..8 {
        let d = loop {
            let v = Vec3::new(rng.unit() as f32, rng.unit() as f32, rng.unit() as f32) * 2.0 - 1.0;
            if (0.01..=1.0).contains(&v.length_squared()) {
                break v.normalize();
            }
        };
        sum += (k * d.dot(p) + rng.unit() as f32 * std::f32::consts::TAU).sin();
    }
    // Eight waves of RMS 1/sqrt 2 each.
    sum * 0.5
}

/// Lumps of RMS height `amp` (world), `wave` long, displaced into the
/// clay's surface where `keep(p)` is 1.
fn lumps(c: &mut Clay, amp: f32, wave: f32, keep: &(dyn Fn(Vec3) -> f32 + Sync)) {
    let field = Field::new(c, CELL);
    let (lo, hi) = field.bounds().unwrap();
    crate::sculpt::engine::displace(c, lo, hi, 4.0 * amp, &|_, _| true, &|p| amp * waves(p, wave) * keep(p)).unwrap();
}

fn lumpy_ball(amp_cells: f32, resolution: u32) -> Clay {
    let mut c = ball(resolution);
    if amp_cells > 0.0 {
        lumps(&mut c, amp_cells * CELL, WAVE * CELL, &|_| 1.0);
    }
    c
}

/// Lumpiness of a clay over `samples` points of its regular mesh.
fn score(c: &Clay, samples: usize) -> (Lumpiness, Vec<Point>) {
    let mesh = c.build_mesh();
    let (_, points, _) = sample_clay(c, &mesh, samples).unwrap();
    (lumpiness(&points, &[]), points)
}

/// An analytic sphere with bumps of a known RMS: the score reads about
/// that RMS, and the clean sphere about nothing.
#[test]
fn lumpiness_reads_known_bumps() {
    let (r, cell) = (0.3f32, 0.01f32);
    let wave = 4.0 * cell;
    let k = std::f32::consts::TAU / wave;
    let seeds: Vec<(Vec3, Vec3)> = crate::harness::geom::sphere_points(Vec3::ZERO, r, 4000).into_iter().map(|p| (p, p.normalize())).collect();
    let mut last = -1.0;
    for amp in [0.0f32, 0.05, 0.1, 0.2] {
        let f = |p: Vec3| p.length() - r - amp * cell * (k * p.x).sin() * (k * p.y).sin() * (k * p.z).sin();
        let points: Vec<Point> = seeds.iter().filter_map(|&(p, n)| measure(&f, p, n, cell)).collect();
        let l = lumpiness(&points, &[]);
        // sin x sin y sin z has RMS 1/(2 sqrt 2) over space.
        let truth = amp / (2.0 * 2f32.sqrt());
        let got = l.overall.unwrap();
        println!("bumps {amp} cells (RMS {truth:.4}): lumpiness {got:.4} over {} of {} points, {} lines", l.counted, points.len(), l.lines);
        assert!(points.len() >= 3900, "{} points measured", points.len());
        if amp == 0.0 {
            assert!(got <= 0.002, "a clean sphere scores {got}");
        } else {
            assert!(got >= 0.75 * truth && got <= 1.3 * truth, "bumps of RMS {truth} score {got}");
        }
        assert!(got > last);
        last = got;
    }
}

/// Seeded noise of RMS 0, 0.05, 0.1, 0.2 and 0.4 cells on a sculpted
/// ball: the scores rise with it, the clean ball scores at most 0.01 and
/// 0.4-cell noise at least 0.25.
#[test]
fn lumpiness_ranks_noise() {
    let mut scores = Vec::new();
    for amp in [0.0f32, 0.05, 0.1, 0.2, 0.4] {
        let (l, points) = score(&lumpy_ball(amp, 64), 8000);
        let s = l.overall.unwrap();
        // The surface's own RMS off the sphere (the grid's trilinear
        // reconstruction keeps less than the noise asked for).
        let truth = rms(points.iter().map(|p| (p.p.length() - 0.3) / CELL)).unwrap();
        println!("noise {amp} cells (surface RMS {truth:.4}): lumpiness {s:.4} (raw {:.4}) over {} counted of {}, {} lines, {} intended", l.raw.unwrap(), l.counted, points.len(), l.lines, l.intended);
        if amp > 0.0 {
            assert!(s >= 0.8 * truth && s <= 1.2 * truth, "noise of surface RMS {truth} scores {s}");
        }
        scores.push(s);
    }
    assert!(scores.windows(2).all(|w| w[1] > w[0]), "not monotone: {scores:?}");
    assert!(scores[0] <= 0.01, "{scores:?}");
    assert!(scores[4] >= 0.25, "{scores:?}");
}

/// The field is the same at every mesh resolution; only the samples'
/// starting points change.
#[test]
fn lumpiness_is_resolution_independent() {
    let base = lumpy_ball(0.2, 96);
    let scores: Vec<f32> = [96, 128, 192]
        .iter()
        .map(|&res| {
            let c = Clay { resolution: res, ..base.clone() };
            score(&c, 8000).0.overall.unwrap()
        })
        .collect();
    println!("resolution 96, 128, 192: {scores:?}");
    let (lo, hi) = scores.iter().fold((f32::MAX, f32::MIN), |(a, b), s| (a.min(*s), b.max(*s)));
    assert!(hi <= 1.1 * lo, "{scores:?}");
}

/// The same points and scores on 1 and 12 threads.
#[test]
fn field_measures_are_thread_independent() {
    let c = lumpy_ball(0.2, 64);
    let mesh = c.build_mesh();
    let run = |threads| crate::clay::with_threads(threads, || sample_clay(&c, &mesh, 4000).unwrap().1);
    let (one, many) = (run(1), run(12));
    assert_eq!(one, many);
    assert_eq!(lumpiness(&one, &[]), lumpiness(&many, &[]));
}


/// Commits strokes to the clay's ledger as the sculpt tool does (alphas
/// bound from its store), so they leave records.
fn commit(c: &mut Clay, strokes: &[Value]) {
    let store = c.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default();
    let lookup = |a: &AlphaRef| -> Result<Hash128, String> { a.hash.or_else(|| a.name.as_ref().and_then(|n| store.names.get(n).copied())).ok_or("no such alpha".into()) };
    let ctx = ResolveCtx { alpha: Some(&lookup), ..ResolveCtx::local() };
    for (spec, r) in crate::sculpt::prepare(strokes, None, &ctx).unwrap() {
        let done = ledger::commit::<Detail>(c, &ledger::ledger(c), &ledger::Meta::agent("sculpt", None), &spec, &r, false, None).unwrap();
        ledger::install(c, done.ledger, done.grid);
    }
}

/// A pore alpha: dots 9 pixels across every 24 pixels (2.5 cells across
/// on the fixture's dabs).
fn pores() -> Alpha {
    let n = 96u32;
    Alpha::new(n, n, (0..n * n).map(|i| { let (x, y) = ((i % n) % 24, (i / n) % 24); if (x as i32 - 12).pow(2) + (y as i32 - 12).pow(2) <= 20 { 65535 } else { 0 } }).collect()).unwrap()
}

/// The ball with stamped pores (a standard brush with an alpha) on its
/// front and a crease groove over its top, and no noise: the texture is
/// intended, so lumpiness stays near a clean ball's.
#[test]
fn lumpiness_ignores_intended_detail() {
    let mut c = ball(64);
    let mut store = AlphaStore::default();
    store.keep(Some("pores"), &pores()).unwrap();
    ledger::set_alphas(&mut c, store);
    let on = |d: Vec3| d.normalize() * 0.3;
    let mut strokes: Vec<Value> = [Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.4, 0.2, 1.0), Vec3::new(-0.4, -0.3, 1.0)].iter().map(|d| json!({ "brush": "standard", "point": on(*d).to_array(), "normal": d.normalize().to_array(), "radius": 0.12, "falloff": "flat", "invert": true, "alpha": { "name": "pores", "depth": 0.6 * CELL } })).collect();
    let arc: Vec<[f32; 3]> = (0..=12).map(|i| on(Vec3::new(-0.5 + i as f32 / 12.0, 1.0, -0.3)).to_array()).collect();
    strokes.push(json!({ "brush": "crease", "points": arc, "radius": 4.0 * CELL, "strength": 2.0 * CELL, "normal": [0, 1, 0] }));
    commit(&mut c, &strokes);
    let (l, points) = score(&c, 8000);
    let textured = points.iter().filter(|p| p.textured).count();
    let texture = rms(points.iter().filter(|p| p.textured).map(|p| p.residual)).unwrap();
    println!("pores and crease: lumpiness {:.4} (raw {:.4}; the textured points' {texture:.4}), {textured} textured, {} intended of {}", l.overall.unwrap(), l.raw.unwrap(), l.intended, points.len());
    assert!(texture >= 0.08, "the texture should be there to ignore ({texture})");
    assert!(l.overall.unwrap() <= 0.015, "{l:?}");
    // The pores' own points are the intended ones.
    let off = points.iter().filter(|p| p.residual.abs() > 0.1 && !p.intended()).count();
    assert!(off == 0, "{off} points with a residual over 0.1 cells are not intended");
}
