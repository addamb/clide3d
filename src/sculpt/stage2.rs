//! Stage 2 acceptance (docs/SCULPT_PLAN.md 6): alphas, the clay-family
//! brushes, trim, polish, snakehook and presets, measured on fixtures.
//! Posable symmetry and the tools' side (alpha import, brush_preset) are
//! checked through MCP in `app/alphas/tests.rs`.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::alpha::{Alpha, AlphaRef, AlphaStore};
use crate::brush::resolve::ResolveCtx;
use crate::clay::{Detail, Field, with_threads};
use crate::harness::geom::brick_hash;
use crate::ledger::{self, Hash128, Meta};

/// A checkerboard of `n` x `n` pixels in squares of `cell` pixels.
fn checker(n: u32, cell: u32) -> Alpha {
    Alpha::new(n, n, (0..n * n).map(|i| if (i % n / cell + i / n / cell) % 2 == 0 { 65535 } else { 0 }).collect()).unwrap()
}

/// A clay with `alpha` in its store under `name`.
fn with_alpha(mut c: Clay, name: &str, alpha: &Alpha) -> Clay {
    let mut store = AlphaStore::default();
    store.keep(Some(name), alpha).unwrap();
    ledger::set_alphas(&mut c, store);
    c
}

/// Resolves strokes in object space, binding alphas from the clay's store.
fn resolved(c: &Clay, strokes: &[Value]) -> Vec<(StrokeSpec, Resolved)> {
    let store = c.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default();
    let lookup = |a: &AlphaRef| -> Result<Hash128, String> { a.hash.or_else(|| a.name.as_ref().and_then(|n| store.names.get(n).copied())).ok_or("no such alpha".into()) };
    let ctx = ResolveCtx { alpha: Some(&lookup), ..ResolveCtx::local() };
    prepare(strokes, None, &ctx).unwrap()
}

/// Applies strokes as the sculpt tool does, without a ledger.
fn sculpt(c: &mut Clay, strokes: &[Value]) {
    for (_, r) in resolved(c, strokes) {
        run(c, &r).unwrap();
    }
}

/// Surface height (y) above (x, z), searched from `lo` (inside) to `hi`.
fn top(f: &Field, x: f32, z: f32, lo: f32, hi: f32) -> f32 {
    let at = |y: f32| {
        let p = Vec3::new(x, y, z);
        f.sample(p, &f.near(p - 0.05, p + 0.05)).0
    };
    let (mut a, mut b) = (lo, hi);
    assert!(at(a) < 0.0 && at(b) > 0.0, "no surface over ({x}, {z}) between {lo} and {hi}");
    for _ in 0..40 {
        let m = (a + b) * 0.5;
        if at(m) < 0.0 { a = m } else { b = m }
    }
    (a + b) * 0.5
}

fn field(c: &Clay) -> Field {
    Field::new(c, c.detail_cell.unwrap())
}

/// A flat slab whose top is y = 0.
fn slab(cell: f32, half: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"box","name":"slab","center":[0,-half * 0.5,0],"size":[2.0 * half, half, 2.0 * half],"blend":0}], "resolution":64, "detail_cell":cell})).unwrap()
}

/// The slab with a groove `width` wide and `depth` deep along z at x = 0.
fn grooved(cell: f32, width: f32, depth: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[
        {"type":"box","name":"slab","center":[0,-0.02,0],"size":[0.08,0.04,0.08],"blend":0},
        {"type":"box","name":"groove","op":"carve","center":[0,0,0],"size":[width, 2.0 * depth, 0.1],"blend":0}
    ], "resolution":96, "detail_cell":cell})).unwrap()
}

#[test]
fn a_checkerboard_alpha_stamps_its_heights() {
    let (cell, r, depth) = (0.003, 0.1, 0.01);
    let alpha = checker(64, 16);
    let mut c = with_alpha(slab(cell, 0.3), "checker", &alpha);
    sculpt(&mut c, &[json!({"brush":"standard","point":[0,0,0],"radius":r,"falloff":"flat","alpha":{"name":"checker","depth":depth}})]);
    let f = field(&c);
    // Samples every 2 cells inside 0.8 r: the height against the alpha
    // under it (stamp mode on a +Y face: u = +X, v = -Z).
    let (mut hs, mut als) = (Vec::new(), Vec::new());
    let step = 2.0 * cell;
    let n = (0.8 * r / step) as i32;
    for i in -n..=n {
        for j in -n..=n {
            // Off the squares' edges by half a step.
            let (x, z) = ((i as f32 + 0.5) * step, (j as f32 + 0.5) * step);
            if x.hypot(z) > 0.8 * r {
                continue;
            }
            hs.push(top(&f, x, z, -0.02, 0.03));
            als.push(alpha.sample(x / (2.0 * r) + 0.5, z / (2.0 * r) + 0.5, false).unwrap());
        }
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (mh, ma) = (mean(&hs), mean(&als));
    let cov: f32 = hs.iter().zip(&als).map(|(h, a)| (h - mh) * (a - ma)).sum();
    let (vh, va): (f32, f32) = (hs.iter().map(|h| (h - mh).powi(2)).sum(), als.iter().map(|a| (a - ma).powi(2)).sum());
    let pearson = cov / (vh * va).sqrt();
    let peak = hs.iter().copied().fold(f32::MIN, f32::max);
    // IoU of where it rose (over half the depth) and where the alpha is white.
    let (raised, white): (Vec<bool>, Vec<bool>) = (hs.iter().map(|h| *h > depth * 0.5).collect(), als.iter().map(|a| *a > 0.5).collect());
    let inter = raised.iter().zip(&white).filter(|(a, b)| **a && **b).count() as f32;
    let union = raised.iter().zip(&white).filter(|(a, b)| **a || **b).count() as f32;
    eprintln!("alpha stamp: pearson {pearson:.4}, peak {peak:.5} (depth {depth}, cell {cell}), IoU {:.3} over {} samples", inter / union, hs.len());
    assert!(pearson >= 0.95, "pearson {pearson}");
    assert!((peak - depth).abs() <= 0.5 * cell, "peak {peak}");
    assert!(inter / union >= 0.85, "IoU {}", inter / union);
    // Mask use: the dab rises only where the alpha is white.
    let mut m = with_alpha(slab(cell, 0.3), "checker", &alpha);
    sculpt(&mut m, &[json!({"brush":"standard","point":[0,0,0],"radius":r,"falloff":"flat","alpha":{"name":"checker","use":"mask"}})]);
    let f = field(&m);
    let (white, black) = (top(&f, -0.03, -0.03, -0.02, 0.06), top(&f, 0.03, -0.03, -0.02, 0.06));
    assert!(white > 0.02 && black.abs() < 1e-4, "mask use: white {white}, black {black}");
}

#[test]
fn alphas_hash_by_content_and_rebinding_a_name_makes_records_stale() {
    let alpha = checker(64, 16);
    assert_eq!(Alpha::decode(&alpha.png()).unwrap().hash(), alpha.hash(), "re-importing the same PNG gives the same hash");
    let mut c = with_alpha(slab(0.004, 0.3), "checker", &alpha);
    for (spec, r) in resolved(&c, &[json!({"brush":"standard","point":[0,0,0],"radius":0.08,"alpha":{"name":"checker","depth":0.006}}), json!({"brush":"inflate","point":[0.1,0,0.1],"radius":0.05})]) {
        let k = ledger::commit::<Detail>(&c, &ledger::ledger(&c), &Meta::agent("sculpt", None), &spec, &r, false, None).unwrap();
        ledger::install(&mut c, k.ledger, k.grid);
    }
    let stale = |c: &Clay| {
        let ctx = <Detail as ledger::Grid>::context(c);
        ledger::ledger::<Detail>(c).records().iter().map(|r| r.stale(ctx, c)).collect::<Vec<_>>()
    };
    assert_eq!(stale(&c), [false, false]);
    // The stroke replays from the clay alone, through a save and load.
    let text = serde_json::to_string(&c).unwrap();
    let mut back: Clay = serde_json::from_str(&text).unwrap();
    ledger::after_load(&mut back);
    assert_eq!(ledger::ledger::<Detail>(&back).verified(), Some(true));
    assert_eq!(back.sculpt.as_ref().unwrap().data().alphas, c.sculpt.as_ref().unwrap().data().alphas);
    // The same name bound to other pixels: the alpha stroke is stale, the
    // other is not; rebase takes the new image and clears it.
    let other = checker(64, 8);
    let mut store = c.sculpt.as_ref().unwrap().data().alphas.clone();
    store.keep(Some("checker"), &other).unwrap();
    ledger::set_alphas(&mut c, store);
    assert_eq!(stale(&c), [true, false]);
    let before = brick_hash(c.detail.as_ref().unwrap().grid());
    let e = ledger::edit::<Detail>(&c, &ledger::ledger(&c), ledger::Edit::Rebase, None).unwrap();
    ledger::install(&mut c, e.ledger, e.grid);
    assert_eq!(stale(&c), [false, false]);
    assert_ne!(brick_hash(c.detail.as_ref().unwrap().grid()), before, "the rebased stamp uses the new image");
    let (_, r) = ledger::ledger::<Detail>(&c).records()[0].stroke().map(|(s, r)| (s.clone(), r.clone())).unwrap();
    assert_eq!(r.params.alpha.unwrap().hash, other.hash());
}

#[test]
fn clay_buildup_fills_a_groove_first() {
    let (cell, r) = (0.0005, 0.008);
    let mut c = grooved(cell, 0.005, 0.003);
    let f0 = field(&c);
    let (bottom0, rim0) = (top(&f0, 0.0, 0.0, -0.006, 0.004), top(&f0, 0.0035, 0.0, -0.006, 0.004));
    for _ in 0..3 {
        sculpt(&mut c, &[json!({"brush":"clay_buildup","points":[[0,0,-0.02],[0,0,0.02]],"radius":r,"strength":0.5})]);
    }
    let f = field(&c);
    let (centre, rim) = (top(&f, 0.0, 0.0, -0.006, 0.006) - bottom0, top(&f, 0.0035, 0.0, -0.006, 0.006) - rim0);
    eprintln!("clay_buildup, 3 passes: groove centre up {centre:.5}, rim up {rim:.5}");
    assert!(centre > 0.0 && centre >= 3.0 * rim.max(0.0), "centre {centre}, rim {rim}");
    // More passes build over the plane too (it is a buildup brush).
    for _ in 0..6 {
        sculpt(&mut c, &[json!({"brush":"clay_buildup","points":[[0,0,-0.02],[0,0,0.02]],"radius":r,"strength":0.5})]);
    }
    assert!(top(&field(&c), 0.0035, 0.0, -0.006, 0.008) > rim0 + cell, "buildup builds up");
}

#[test]
fn clay_fills_a_groove_and_leaves_the_plane() {
    let (cell, r) = (0.0005, 0.008);
    let mut c = grooved(cell, 0.005, 0.003);
    let f0 = field(&c);
    let depth0 = top(&f0, 0.006, 0.0, -0.006, 0.004) - top(&f0, 0.0, 0.0, -0.006, 0.004);
    sculpt(&mut c, &[json!({"brush":"clay","points":[[0,0,-0.02],[0,0,0.02]],"radius":r,"strength":0.8})]);
    let f = field(&c);
    let depth = top(&f, 0.006, 0.0, -0.006, 0.004) - top(&f, 0.0, 0.0, -0.006, 0.004);
    let moved = [0.0035f32, 0.005, 0.0065, -0.004, -0.0065].iter().map(|x| (top(&f, *x, 0.003, -0.006, 0.004) - top(&f0, *x, 0.003, -0.006, 0.004)).abs()).fold(0.0, f32::max);
    eprintln!("clay: groove depth {depth0:.5} -> {depth:.5} ({:.0}% less); plane points moved at most {:.3} cell", (1.0 - depth / depth0) * 100.0, moved / cell);
    assert!(depth <= 0.5 * depth0, "depth {depth0} -> {depth}");
    assert!(moved <= 0.5 * cell, "plane moved {moved}");
}

#[test]
fn clay_tubes_lay_an_even_ridge() {
    let (cell, r) = (0.0005, 0.006);
    let mut c = slab(cell, 0.03);
    let path: Vec<[f32; 3]> = (0..10).map(|i| { let t = i as f32 / 9.0; [-0.018 + 0.036 * t, 0.0, 0.006 * (t * 3.0).sin()] }).collect();
    sculpt(&mut c, &[json!({"brush":"clay_tubes","points":path,"radius":r,"strength":0.5})]);
    let f = field(&c);
    // Ridge heights along the path, away from its ends.
    let heights: Vec<f32> = (0..40).map(|k| {
        let t = 0.2 + 0.6 * k as f32 / 39.0;
        let x = -0.018 + 0.036 * t;
        top(&f, x, 0.006 * (t * 3.0).sin(), -0.005, 0.006)
    }).collect();
    let mean = heights.iter().sum::<f32>() / heights.len() as f32;
    let sd = (heights.iter().map(|h| (h - mean).powi(2)).sum::<f32>() / heights.len() as f32).sqrt();
    eprintln!("clay_tubes: ridge {mean:.5} (strength x radius / 2 = {:.5}), sd {:.1}%", 0.5 * r * 0.5, sd / mean * 100.0);
    assert!(mean > 0.5 * 0.5 * r * 0.5, "ridge {mean}");
    assert!(sd <= 0.15 * mean, "sd {sd} of {mean}");
}

/// A flat-topped measure: RMS distance of points from their best plane
/// y = a x + b z + c.
fn plane_rms(pts: &[Vec3]) -> f32 {
    let n = pts.len() as f64;
    let (mut sx, mut sz, mut sy, mut sxx, mut szz, mut sxz, mut sxy, mut szy) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in pts {
        let (x, y, z) = (p.x as f64, p.y as f64, p.z as f64);
        (sx, sz, sy, sxx, szz, sxz, sxy, szy) = (sx + x, sz + z, sy + y, sxx + x * x, szz + z * z, sxz + x * z, sxy + x * y, szy + z * y);
    }
    let m = glam::DMat3::from_cols(glam::DVec3::new(sxx, sxz, sx), glam::DVec3::new(sxz, szz, sz), glam::DVec3::new(sx, sz, n));
    let k = m.inverse() * glam::DVec3::new(sxy, szy, sy);
    (pts.iter().map(|p| (p.y as f64 - (k.x * p.x as f64 + k.y * p.z as f64 + k.z)).powi(2)).sum::<f64>() / n).sqrt() as f32
}

#[test]
fn trim_dynamic_only_cuts_to_a_flat_plane() {
    let (cell, r) = (0.004, 0.15);
    let mut c: Clay = serde_json::from_value(json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":0.5,"blend":0}], "resolution":64, "detail_cell":cell})).unwrap();
    let spec = json!({"brush":"trim_dynamic","point":[0,0.5,0],"radius":r,"strength":1.0});
    let (s, res) = resolved(&c, &[spec]).pop().unwrap();
    let k = ledger::commit::<Detail>(&c, &ledger::ledger(&c), &Meta::agent("sculpt", None), &s, &res, true, None).unwrap();
    let volume = k.record.stats.volume_delta.unwrap();
    ledger::install(&mut c, k.ledger, k.grid);
    let f = field(&c);
    let mut pts = Vec::new();
    let step = 2.0 * cell;
    let n = (0.7 * r / step) as i32;
    for i in -n..=n {
        for j in -n..=n {
            let (x, z) = (i as f32 * step, j as f32 * step);
            if x.hypot(z) <= 0.7 * r {
                pts.push(Vec3::new(x, top(&f, x, z, 0.3, 0.6), z));
            }
        }
    }
    let rms = plane_rms(&pts);
    let cut = 0.5 - pts.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    eprintln!("trim_dynamic: volume_delta {volume:.3e}, planar RMS {:.3} cell over {} points, cut {cut:.4}", rms / cell, pts.len());
    assert!(volume <= 0.0, "volume {volume}");
    assert!(cut > 2.0 * cell, "it cut {cut}");
    assert!(rms <= 0.3 * cell, "rms {rms}");
}

#[test]
fn polish_presses_onto_the_area_plane() {
    let mut c = slab(0.004, 0.3);
    sculpt(&mut c, &[json!({"brush":"noise","point":[0,0,0],"radius":0.2,"strength":1.0,"scale":0.04,"pattern":"bumps"})]);
    let rough = |c: &Clay| {
        let f = field(c);
        let pts: Vec<Vec3> = (-6..=6).flat_map(|i| (-6..=6).map(move |j| (i as f32 * 0.008, j as f32 * 0.008))).map(|(x, z)| Vec3::new(x, top(&f, x, z, -0.05, 0.05), z)).collect();
        plane_rms(&pts)
    };
    let before = rough(&c);
    sculpt(&mut c, &[json!({"brush":"polish","point":[0,0,0],"radius":0.1,"strength":1.0})]);
    let after = rough(&c);
    eprintln!("polish: roughness {before:.5} -> {after:.5}");
    assert!(after < 0.3 * before, "{before} -> {after}");
}

#[test]
fn snakehook_curls_its_tip() {
    let (cell, r) = (0.002, 0.02);
    let mut c = slab(cell, 0.15);
    let spec = json!({"brush":"snakehook","points":[[0,0,0],[0,0.08,0]],"radius":r,"curl_deg":90,"normal":[0,0,1]});
    let path = resolved(&c, std::slice::from_ref(&spec))[0].1.dabs[0].points.clone();
    sculpt(&mut c, &[spec]);
    let f = field(&c);
    // The horn's material near the path's last stretch: centroids of the
    // inside grid points around its 85% and 100% points.
    let centroid = |q: Vec3| {
        let (mut sum, mut n) = (Vec3::ZERO, 0);
        let k = (r / cell) as i32;
        for x in -k..=k {
            for y in -k..=k {
                for z in -k..=k {
                    let p = q + Vec3::new(x as f32, y as f32, z as f32) * cell;
                    if p.distance(q) <= r && p.y > 0.01 && f.sample(p, &f.near(p - 0.01, p + 0.01)).0 < 0.0 {
                        sum += p;
                        n += 1;
                    }
                }
            }
        }
        assert!(n > 0, "no horn near {q}");
        sum / n as f32
    };
    let last = path.len() - 1;
    let (a, b) = (centroid(path[last * 85 / 100]), centroid(path[last]));
    let tip = (b - a).normalize();
    let angle = tip.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees();
    let height = path.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    eprintln!("snakehook curl 90: tip turned {angle:.1} degrees ({tip}), path rises to {height:.3}");
    assert!((angle - 90.0).abs() <= 15.0, "tip at {angle} degrees");
    // Without curl it goes straight up.
    let mut s = slab(cell, 0.15);
    sculpt(&mut s, &[json!({"brush":"snakehook","points":[[0,0,0],[0,0.06,0]],"radius":r})]);
    let fs = field(&s);
    assert!(fs.sample(Vec3::new(0.0, 0.045, 0.0), &fs.near(Vec3::splat(-0.1), Vec3::splat(0.1))).0 < 0.0, "a straight horn");
}

#[test]
fn presets_merge_under_strokes_like_the_spelled_out_spec() {
    let scene = std::collections::BTreeMap::new();
    let merged = crate::brush::preset::merge(&scene, None, &json!({"preset":"DamStandard","points":[[0,0,0],[0.1,0,0]],"radius":0.03,"strength":0.003})).unwrap();
    let spelled = json!({"brush":"crease","taper":[0.2,0.2],"points":[[0,0,0],[0.1,0,0]],"radius":0.03,"strength":0.003});
    let (a, b) = (slab(0.004, 0.3), slab(0.004, 0.3));
    let ra = resolved(&a, &[merged]).pop().unwrap();
    let rb = resolved(&b, &[spelled]).pop().unwrap();
    assert_eq!(ra, rb);
    let commit = |c: &Clay, (s, r): &(StrokeSpec, Resolved)| ledger::commit::<Detail>(c, &ledger::ledger(c), &Meta::agent("sculpt", None), s, r, false, None).unwrap().record.result_hash;
    assert_eq!(commit(&a, &ra), commit(&b, &rb));
    // A preset saved in a scene reads back the same.
    let mut sc = crate::scene::Scene::default();
    sc.brushes.insert("Mine".into(), crate::brush::preset::Preset::try_from(json!({"brush":"clay_tubes","strength":0.4,"falloff":"sharp"})).unwrap());
    let back: crate::scene::Scene = serde_json::from_str(&serde_json::to_string(&sc).unwrap()).unwrap();
    assert_eq!(back.brushes, sc.brushes);
    assert!(back == sc);
}

/// Strokes of the Stage 2 brushes (and alphas) over a slab, from a seed.
fn stage2_strokes(n: usize) -> Vec<Value> {
    let mut rng = crate::brush::rng::Pcg32::new(2027, 3);
    let brushes = ["clay", "clay_buildup", "clay_tubes", "trim_dynamic", "polish", "snakehook", "standard"];
    (0..n)
        .map(|_| {
            let mut f = || rng.next_f32();
            let brush = brushes[(f() * brushes.len() as f32) as usize % brushes.len()];
            let p0 = [0.4 * (f() - 0.5), 0.0, 0.4 * (f() - 0.5)];
            let mut s = json!({"brush":brush,"points":[p0,[p0[0] + 0.05 * (f() - 0.5), 0.0, p0[2] + 0.05 * (f() - 0.5)]],"radius":0.03 + 0.03 * f(),"strength":0.2 + 0.6 * f()});
            match brush {
                "snakehook" => s["points"] = json!([p0, [p0[0], 0.05, p0[2]]]),
                "standard" | "clay" => s["alpha"] = json!({"name":"checker","rotate": 90.0 * f(),"mode": if f() < 0.5 { "drag" } else { "stamp" }}),
                _ => {}
            }
            if f() < 0.3 {
                s["mirror"] = json!(true);
            }
            s
        })
        .collect()
}

#[test]
fn stage2_brushes_are_bit_identical_on_any_thread_count() {
    let strokes = stage2_strokes(60);
    let alpha = checker(32, 8);
    let run_on = |threads: usize| {
        with_threads(threads, || {
            let mut c = with_alpha(slab(0.008, 0.3), "checker", &alpha);
            sculpt(&mut c, &strokes);
            brick_hash(c.detail.as_ref().unwrap().grid())
        })
    };
    let t = std::time::Instant::now();
    let one = run_on(1);
    let one_s = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let twelve = run_on(12);
    eprintln!("60 Stage 2 strokes: {one_s:.2} s on 1 thread, {:.2} s on 12", t.elapsed().as_secs_f64());
    assert_eq!(one, twelve);
}

#[test]
fn mirrored_alpha_stamps_are_mirror_images() {
    // An asymmetric alpha stamped with X symmetry: the copy is its mirror.
    let alpha = Alpha::new(4, 4, (0..16).map(|i| if i % 4 == 0 || i < 4 { 65535 } else { 0 }).collect()).unwrap();
    let mut c = with_alpha(slab(0.004, 0.3), "ell", &alpha);
    sculpt(&mut c, &[json!({"brush":"standard","point":[0.12,0,0.02],"radius":0.06,"falloff":"flat","alpha":{"name":"ell","depth":0.01,"rotate":20},"mirror":true})]);
    let f = field(&c);
    let mut worst = 0.0f32;
    for i in -10..=10 {
        for j in -10..=10 {
            let (x, z) = (0.12 + i as f32 * 0.005, 0.02 + j as f32 * 0.005);
            worst = worst.max((top(&f, x, z, -0.02, 0.03) - top(&f, -x, z, -0.02, 0.03)).abs());
        }
    }
    assert!(worst < 1e-4, "mirror differs by {worst}");
}

#[test]
fn stamp_and_tile_stay_upright_while_drag_turns_with_the_stroke() {
    // Two dots along z (the stroke's direction is +Z): stamp and tile land
    // as the same dots sent one at a time (no direction: object X); drag
    // turns the image a quarter turn, so it lands differently.
    let alpha = Alpha::new(4, 4, (0..16).map(|i| if i % 4 == 0 || i < 4 { 65535 } else { 0 }).collect()).unwrap();
    let (a, b) = ([0.0, 0.0, -0.08], [0.0, 0.0, 0.08]);
    let heights = |strokes: &[Value]| {
        let mut c = with_alpha(slab(0.004, 0.3), "ell", &alpha);
        sculpt(&mut c, strokes);
        let f = field(&c);
        let mut hs = Vec::new();
        for i in -10..=10 {
            for j in -26..=26 {
                hs.push(top(&f, i as f32 * 0.005, j as f32 * 0.005, -0.02, 0.03));
            }
        }
        hs
    };
    let differ = |p: &[f32], q: &[f32]| p.iter().zip(q).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    for mode in ["stamp", "tile", "drag"] {
        let dab = |pts: Value| json!({"brush":"standard","kind":"dots","points":pts,"radius":0.06,"falloff":"flat","alpha":{"name":"ell","depth":0.01,"mode":mode}});
        let path = heights(&[dab(json!([a, b]))]);
        let apart = heights(&[dab(json!([a])), dab(json!([b]))]);
        let d = differ(&path, &apart);
        eprintln!("{mode}: a two-dot stroke along z and the dots one at a time differ by {d:.5}");
        if mode == "drag" {
            assert!(d > 0.004, "drag should turn with the stroke: {d}");
        } else {
            assert!(d < 1e-4, "{mode} should stay upright on object X: {d}");
        }
    }
}

/// Renders a clay as the goldens do (front view, warm clay color).
fn render(c: &Clay) -> egui::ColorImage {
    use crate::harness::image;
    let mut scene = crate::scene::Scene::default();
    let id = scene.add(crate::scene::Shape::Clay);
    let o = scene.get_mut(id).unwrap();
    o.color = [0.85, 0.72, 0.55];
    o.clay = Some(c.clone());
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    image::render(&scene, &meshes, image::View::FRONT).unwrap_or_else(|e| panic!("CLIDE_GPU=1 but: {e}"))
}

/// Clay tubes wound around a ball, replayed from the ledger and rendered.
/// `CLIDE_GPU=1` only.
#[test]
fn golden_clay_tubes() {
    use crate::harness::image;
    if !image::gpu_enabled() {
        eprintln!("golden_clay_tubes: set CLIDE_GPU=1 to run");
        return;
    }
    let mut c: Clay = serde_json::from_value(json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":0.5}], "resolution":128, "detail_cell":0.004})).unwrap();
    let ring = |y: f32, phase: f32| {
        let r = (0.25f32 - y * y).max(0.0).sqrt();
        let pts: Vec<[f32; 3]> = (0..12).map(|i| { let a = phase + i as f32 * 0.2; [r * a.sin(), y, r * a.cos()] }).collect();
        json!({"brush":"clay_tubes","points":pts,"radius":0.04,"strength":0.35})
    };
    sculpt(&mut c, &[ring(0.25, -1.0), ring(0.05, -0.6), ring(-0.15, -1.2)]);
    image::assert_golden("clay_tubes", &render(&c), 2.0, 8);
}

/// Alpha stamps (height and mask, stamp, rotated and tiled) on a slab.
/// `CLIDE_GPU=1` only.
#[test]
fn golden_alpha_stamps() {
    use crate::harness::image;
    if !image::gpu_enabled() {
        eprintln!("golden_alpha_stamps: set CLIDE_GPU=1 to run");
        return;
    }
    let ring = Alpha::new(32, 32, (0..32 * 32).map(|i| { let (x, y) = ((i % 32) as f32 - 15.5, (i / 32) as f32 - 15.5); let d = x.hypot(y); if (8.0..13.0).contains(&d) { 65535 } else { 0 } }).collect()).unwrap();
    let mut c = with_alpha(serde_json::from_value(json!({"blobs":[{"type":"box","center":[0,-0.1,0],"size":[1.2,0.2,0.8],"blend":0}], "resolution":128, "detail_cell":0.003})).unwrap(), "checker", &checker(64, 16));
    let mut store = c.sculpt.as_ref().unwrap().data().alphas.clone();
    store.keep(Some("ring"), &ring).unwrap();
    ledger::set_alphas(&mut c, store);
    sculpt(&mut c, &[
        json!({"brush":"standard","point":[-0.35,0,0.1],"radius":0.15,"falloff":"flat","alpha":{"name":"checker","depth":0.02}}),
        json!({"brush":"standard","point":[0.0,0,0.1],"radius":0.15,"falloff":"flat","alpha":{"name":"ring","depth":0.025,"mid":0.3}}),
        json!({"brush":"standard","point":[0.35,0,0.1],"radius":0.15,"falloff":"flat","alpha":{"name":"checker","depth":0.02,"rotate":45,"scale":0.5,"mode":"tile"}}),
        json!({"brush":"clay_buildup","points":[[-0.4,0,-0.2],[0.4,0,-0.2]],"radius":0.08,"strength":0.8,"alpha":{"name":"ring","mode":"drag"}}),
    ]);
    image::assert_golden("alpha_stamps", &render(&c), 2.0, 8);
}

#[test]
fn over_full_libraries_open_with_their_first_entries() {
    let one = serde_json::to_value(Alpha::new(1, 1, vec![7]).unwrap()).unwrap();
    let alphas: serde_json::Map<String, Value> = (0..crate::brush::alpha::MAX_ALPHAS + 5).map(|k| (format!("a{k:03}"), one.clone())).collect();
    let brushes: serde_json::Map<String, Value> = (0..crate::brush::preset::MAX_PRESETS + 5).map(|k| (format!("p{k:03}"), json!({ "brush": "clay" }))).collect();
    let scene: crate::scene::Scene = serde_json::from_value(json!({ "objects": [], "next_id": 1, "alphas": alphas, "brushes": brushes })).unwrap();
    assert_eq!((scene.alphas.len(), scene.brushes.len()), (crate::brush::alpha::MAX_ALPHAS, crate::brush::preset::MAX_PRESETS));
    assert!(scene.alphas.contains_key("a000") && !scene.alphas.contains_key("a068"));
}

#[test]
fn unused_alphas_leave_the_store() {
    let (a, b) = (checker(16, 4), checker(16, 8));
    let mut c = with_alpha(slab(0.008, 0.3), "a", &a);
    let mut store = c.sculpt.as_ref().unwrap().data().alphas.clone();
    store.keep(Some("b"), &b).unwrap();
    ledger::set_alphas(&mut c, store);
    for (spec, r) in resolved(&c, &[json!({"brush":"standard","point":[0,0,0],"radius":0.08,"alpha":{"name":"a"}})]) {
        let k = ledger::commit::<Detail>(&c, &ledger::ledger(&c), &Meta::agent("sculpt", None), &spec, &r, false, None).unwrap();
        ledger::install(&mut c, k.ledger, k.grid);
    }
    let mut store = c.sculpt.as_ref().unwrap().data().alphas.clone();
    let (names, hashes) = ledger::alphas_in_use(&c);
    store.prune(&names, &hashes);
    assert_eq!(store.images.keys().copied().collect::<Vec<_>>(), vec![a.hash()]);
    assert_eq!(store.names.keys().cloned().collect::<Vec<_>>(), vec!["a".to_string()]);
}
