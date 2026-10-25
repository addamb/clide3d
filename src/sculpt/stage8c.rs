//! Stage 8c acceptance (docs/SCULPT_PLAN.md 6): VDM stamps captured from
//! clay and stamped back (`brush::vdm`), and curve strokes, on clays
//! directly. Capture, stamping and MoveCurve through MCP are checked in
//! `app/alphas/tests.rs`.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::alpha::{Alpha, AlphaRef, AlphaStore};
use crate::brush::resolve::{MAX_POINTS, ResolveCtx};
use crate::brush::vdm::{Capture, capture};
use crate::clay::{Detail, Field, with_threads};
use crate::ledger::{self, Grid, Hash128};

const CELL: f32 = 0.004;

/// A flat slab whose top is y = 0, with 4 mm detail cells.
fn slab() -> Clay {
    serde_json::from_value(json!({ "blobs": [{ "type": "box", "center": [0, -0.15, 0], "size": [0.6, 0.3, 0.6], "blend": 0 }], "resolution": 64, "detail_cell": CELL })).unwrap()
}

/// The slab with a hooked horn at the origin: up 6 cm, then over +x
/// 4.5 cm, 12 mm thick, so its arm overhangs the slab.
fn horned() -> Clay {
    serde_json::from_value(json!({ "blobs": [
        { "type": "box", "center": [0, -0.15, 0], "size": [0.6, 0.3, 0.6], "blend": 0 },
        { "type": "capsule", "a": [0, -0.01, 0], "b": [0, 0.06, 0], "radius": 0.012, "blend": 0 },
        { "type": "capsule", "a": [0, 0.06, 0], "b": [0.045, 0.06, 0], "radius": 0.012, "blend": 0 },
    ], "resolution": 64, "detail_cell": CELL })).unwrap()
}

/// The horn's stamp: 8 cm across each way, 10 cm up, 2 cm down.
fn horn_stamp() -> Alpha {
    let got = capture(&horned(), &Capture { center: Vec3::ZERO, normal: None, tangent: None, radius: 0.08, above: 0.1, below: 0.02, cell: None }).unwrap();
    assert!((got.base.y).abs() < 1e-4 && (got.normal - Vec3::Y).length() < 1e-3, "{} {}", got.base, got.normal);
    Alpha::from_vdm(got.vdm).unwrap()
}

fn with_alpha(mut c: Clay, name: &str, alpha: &Alpha) -> Clay {
    let mut store = AlphaStore::default();
    store.keep(Some(name), alpha).unwrap();
    ledger::set_alphas(&mut c, store);
    c
}

fn resolved(c: &Clay, spec: &Value) -> Resolved {
    let store = c.sculpt.as_ref().map(|s| s.data().alphas.clone()).unwrap_or_default();
    let lookup = |a: &AlphaRef| -> Result<Hash128, String> { a.hash.or_else(|| a.name.as_ref().and_then(|n| store.names.get(n).copied())).ok_or("no such alpha".into()) };
    let ctx = ResolveCtx { alpha: Some(&lookup), ..ResolveCtx::local() };
    let spec: StrokeSpec = serde_json::from_value(spec.clone()).unwrap();
    crate::brush::resolve::resolve(&spec, &ctx).unwrap()
}

fn field(c: &Clay) -> Field {
    Field::new(c, CELL)
}

fn at(f: &Field, p: Vec3) -> f32 {
    f.sample(p, &f.near(p - 0.05, p + 0.05)).0
}

fn hash(c: &Clay) -> Hash128 {
    <Detail as Grid>::hash(c.detail.as_ref())
}

/// A VDM captured from a hooked horn and stamped onto a flat slab elsewhere
/// gives the horn back: Hausdorff (both ways, on the fields' surface bands
/// sampled every half cell) at most 1.5 cells, the overhang (empty under
/// the arm, solid in it), bit-equal replay on any number of threads and
/// after the stamp is saved and read back; with symmetry the copy is the
/// mirror image.
#[test]
fn a_vdm_from_a_horn_stamps_its_overhang_onto_a_plane() {
    let t = std::time::Instant::now();
    let stamp = horn_stamp();
    let took = t.elapsed();
    let v = stamp.vdm().unwrap();
    eprintln!("vdm capture: {} bricks at cell {} in {:.1} ms; image {:?}", v.grid.grid().brick_count(), v.cell(), took.as_secs_f64() * 1e3, stamp.size());
    let at_point = Vec3::new(0.1, 0.0, 0.048);
    let spec = json!({ "brush": "vdm", "points": [at_point.to_array()], "radius": 0.08, "alpha": { "name": "horn" } });
    let base = with_alpha(slab(), "horn", &stamp);
    let r = resolved(&base, &spec);
    let mut c = base.clone();
    let t = std::time::Instant::now();
    let report = run(&mut c, &r).unwrap();
    eprintln!("vdm stamp: {} grid points changed in {:.1} ms", report.changed_points, t.elapsed().as_secs_f64() * 1e3);
    assert!(report.changed_points > 1000, "{}", report.changed_points);

    // Hausdorff between the source horn and the stamped one.
    let (src, dst) = (field(&horned()), field(&c));
    let (mut worst_src, mut worst_dst, mut n_src, mut n_dst) = (0.0f32, 0.0f32, 0, 0);
    let step = CELL * 0.5;
    for i in 0..((0.16 / step) as i32) {
        for j in 0..((0.1 / step) as i32) {
            for k in 0..((0.08 / step) as i32) {
                let p = Vec3::new(-0.06 + i as f32 * step, -0.01 + j as f32 * step, -0.04 + k as f32 * step);
                let (a, b) = (at(&src, p), at(&dst, p + at_point));
                if a.abs() < 0.25 * CELL {
                    worst_src = worst_src.max(b.abs());
                    n_src += 1;
                }
                if b.abs() < 0.25 * CELL {
                    worst_dst = worst_dst.max(a.abs());
                    n_dst += 1;
                }
            }
        }
    }
    eprintln!("vdm Hausdorff: source -> stamp {:.2} cells ({n_src} points), stamp -> source {:.2} cells ({n_dst} points)", worst_src / CELL, worst_dst / CELL);
    assert!(n_src > 500 && n_dst > 500);
    assert!(worst_src <= 1.5 * CELL && worst_dst <= 1.5 * CELL, "{worst_src} {worst_dst}");

    // The overhang: under the arm is empty, the arm is solid, the slab under it too.
    let arm = at_point + Vec3::new(0.035, 0.0, 0.0);
    assert!(at(&dst, arm + Vec3::Y * 0.025) > 0.0 && at(&dst, arm + Vec3::Y * 0.06) < 0.0 && at(&dst, arm - Vec3::Y * 0.01) < 0.0, "no overhang");
    assert!(at(&dst, arm + Vec3::Y * 0.08) > 0.0 && at(&dst, at_point + Vec3::Y * 0.03) < 0.0);

    // Replay: bit-equal, on one thread or several, and with the stamp
    // saved and read back.
    let again = |threads: usize, c0: &Clay| {
        with_threads(threads, || {
            let mut a = c0.clone();
            run(&mut a, &r).unwrap();
            hash(&a)
        })
    };
    assert_eq!(again(1, &base), hash(&c));
    assert_eq!(again(3, &base), hash(&c));
    let t = std::time::Instant::now();
    assert_eq!(again(usize::MAX, &base), hash(&c));
    eprintln!("vdm stamp, warm: {:.1} ms (radius 0.08 = 20 cells, 10 cm tall)", t.elapsed().as_secs_f64() * 1e3);
    let text = serde_json::to_string(&stamp).unwrap();
    let back: Alpha = serde_json::from_str(&text).unwrap();
    assert_eq!(back.hash(), stamp.hash(), "a saved stamp loads bit for bit");
    assert_eq!(again(2, &with_alpha(slab(), "horn", &back)), hash(&c));
    eprintln!("vdm stamp file: {} bytes of JSON", text.len());

    // Mirrored: the copy across x hooks the other way.
    let mut sym = spec.clone();
    sym["symmetry"] = json!({ "axes": ["x"] });
    let mut m = base.clone();
    run(&mut m, &resolved(&base, &sym)).unwrap();
    let f = field(&m);
    let mirror = Vec3::new(-arm.x, arm.y, arm.z);
    assert!(at(&f, mirror + Vec3::Y * 0.06) < 0.0 && at(&f, mirror + Vec3::Y * 0.025) > 0.0, "the mirrored hook");
    assert!(at(&f, Vec3::new(-at_point.x + 0.035, 0.06, at_point.z)) > 0.0, "the mirror image, not a copy");

    // Invert carves the horn's shape into the slab instead.
    let mut inv = spec.clone();
    inv["invert"] = json!(true);
    let mut h = base.clone();
    run(&mut h, &resolved(&base, &inv)).unwrap();
    let f = field(&h);
    assert!(at(&f, at_point - Vec3::Y * 0.03) > 0.0 && at(&f, at_point + Vec3::Y * 0.01) > 0.0, "a hole where the horn was");
}

/// Captures are bounded before they sample, refuse flat ground and bad
/// sizes, and a stamp's file is checked as it is read.
#[test]
fn captures_are_bounded_and_flat_ground_keeps_nothing() {
    let cap = |center: Vec3, radius: f32, cell: Option<f32>| Capture { center, normal: None, tangent: None, radius, above: radius, below: radius * 0.25, cell };
    let flat = capture(&slab(), &cap(Vec3::ZERO, 0.08, None));
    assert!(flat.err().unwrap().contains("flat"));
    for (c, why) in [
        (cap(Vec3::splat(f32::NAN), 0.08, None), "center"),
        (cap(Vec3::splat(1e30), 0.08, None), "center"),
        (cap(Vec3::ZERO, f32::INFINITY, None), "radius"),
        (cap(Vec3::ZERO, 0.08, Some(1e-6)), "lattice points"),
        (Capture { above: 10.0, ..cap(Vec3::ZERO, 0.08, None) }, "above"),
        (cap(Vec3::new(5.0, 0.0, 0.0), 0.08, None), "no clay surface"),
    ] {
        let t = std::time::Instant::now();
        let e = capture(&horned(), &c).err().unwrap();
        assert!(e.contains(why), "{e}");
        assert!(t.elapsed().as_secs_f64() < 1.0, "refused before sampling");
    }
    // A wide capture coarsens its cell to fit.
    let wide = capture(&horned(), &cap(Vec3::ZERO, 0.3, None)).unwrap();
    assert!(wide.vdm.cell() > CELL);
    // Files: sizes and bricks are checked.
    let stamp = horn_stamp();
    let mut file: Value = serde_json::to_value(&stamp).unwrap();
    file["vdm"]["above"] = json!(1e6);
    assert!(serde_json::from_value::<Alpha>(file).is_err());
    // A file asking for a lattice of 513 x 513 x 1001 points (each size
    // within its own bound) is refused before its image is drawn.
    let mut file: Value = serde_json::to_value(&stamp).unwrap();
    (file["vdm"]["radius"], file["vdm"]["above"], file["vdm"]["below"]) = (json!(256.0 * CELL), json!(980.0 * CELL), json!(20.0 * CELL));
    let t = std::time::Instant::now();
    let e = serde_json::from_value::<Alpha>(file).err().unwrap().to_string();
    assert!(e.contains("lattice holds at most") && t.elapsed().as_secs_f64() < 1.0, "{e}");
    // A brick key whose 8x overflows i32 (or i32::MIN, which has no abs)
    // is refused as outside the box, not a panic or a wrap.
    let grid: Value = serde_json::from_str(&stamp.vdm().unwrap().grid.raw_file_json()).unwrap();
    for key in [i32::MAX / 8 + 1, i32::MAX, i32::MIN / 8, i32::MIN] {
        let mut file: Value = serde_json::to_value(&stamp).unwrap();
        let mut g = grid.clone();
        g["bricks"][0]["at"] = json!([key, 0, 0]);
        file["vdm"]["grid"] = g;
        let e = serde_json::from_value::<Alpha>(file).err().unwrap_or_else(|| panic!("key {key} read")).to_string();
        assert!(e.contains("outside its box"), "{key}: {e}");
    }
    // Brushes and stamps do not mix.
    let image = Alpha::new(4, 4, vec![30000; 16]).unwrap();
    let c = with_alpha(slab(), "img", &image);
    let r = resolved(&c, &json!({ "brush": "vdm", "point": [0, 0, 0], "radius": 0.05, "alpha": { "name": "img" } }));
    assert!(run(&mut c.clone(), &r).err().unwrap().contains("VDM"));
    let c = with_alpha(slab(), "horn", &stamp);
    let r = resolved(&c, &json!({ "brush": "standard", "point": [0, 0, 0], "radius": 0.05, "alpha": { "name": "horn" } }));
    assert!(run(&mut c.clone(), &r).err().unwrap().contains("VDM"));
    let bad: StrokeSpec = serde_json::from_value(json!({ "brush": "vdm", "point": [0, 0, 0], "radius": 0.05 })).unwrap();
    assert!(crate::brush::resolve::resolve(&bad, &ResolveCtx::local()).err().unwrap().contains("needs alpha"));
}

/// A curve stroke runs a smooth curve through its control points (each
/// one on it), closes back to its first, keeps its points in the spec,
/// and stays bounded whatever its coordinates.
#[test]
fn curve_strokes_follow_a_smooth_curve_through_their_points() {
    let pts = json!([[-0.1, 0, 0], [0, 0, 0.08], [0.1, 0, 0], [0.12, 0, -0.06]]);
    let r = resolved(&slab(), &json!({ "brush": "crease", "kind": { "curve": {} }, "points": pts, "radius": 0.02, "strength": 0.002 }));
    let line = &r.dabs[0].points;
    for p in pts.as_array().unwrap() {
        let p = Vec3::new(p[0].as_f64().unwrap() as f32, 0.0, p[2].as_f64().unwrap() as f32);
        assert!(line.iter().any(|q| q.distance(p) < 1e-5), "the curve passes through {p}");
    }
    // Smooth: no turn sharper than 30 degrees between steps (the
    // polyline turns 80 degrees and more at its points).
    let turn = line.windows(3).map(|w| (w[1] - w[0]).normalize().angle_between((w[2] - w[1]).normalize()).to_degrees()).fold(0.0f32, f32::max);
    assert!(turn < 30.0, "{turn}");
    let closed = resolved(&slab(), &json!({ "brush": "crease", "kind": { "curve": { "closed": true } }, "points": pts, "radius": 0.02, "strength": 0.002 }));
    let l = &closed.dabs[0].points;
    assert_eq!(l.first(), l.last());
    assert!(l.len() > line.len());
    // Dab brushes space dabs along it; the spec keeps the control points.
    let spec: StrokeSpec = serde_json::from_value(json!({ "brush": "standard", "kind": { "curve": {} }, "points": pts, "radius": 0.02 })).unwrap();
    assert_eq!(spec.points.len(), 4);
    assert_eq!(serde_json::to_value(&spec).unwrap()["kind"], json!({ "curve": { "closed": false } }));
    let d = crate::brush::resolve::resolve(&spec, &ResolveCtx::local()).unwrap();
    assert!(d.dabs.len() > 20 && d.dabs.iter().all(|d| d.points.len() == 1));
    // Too few points, lazy, and huge coordinates: refused or bounded.
    for (bad, why) in [
        (json!({ "brush": "standard", "kind": { "curve": {} }, "points": [[0, 0, 0]], "radius": 0.02 }), "control points"),
        (json!({ "brush": "standard", "kind": { "curve": { "closed": true } }, "points": [[0, 0, 0], [1, 0, 0]], "radius": 0.02 }), "control points"),
        (json!({ "brush": "standard", "kind": { "curve": {} }, "points": [[0, 0, 0], [1, 0, 0]], "radius": 0.02, "lazy": 0.5 }), "lazy"),
        (json!({ "brush": "standard", "kind": { "curve": {} }, "points": [[0, 0, 0], [1e30, 0, 0], [0, 1e30, 0]], "radius": 0.02 }), "dabs"),
    ] {
        let s: StrokeSpec = serde_json::from_value(bad).unwrap();
        let e = crate::brush::resolve::resolve(&s, &ResolveCtx::local()).err().unwrap();
        assert!(e.contains(why), "{e}");
    }
    // The most control points, far apart: path dabs keep their point
    // bound; spans too long to measure are refused.
    for (far, ok) in [(1e15f32, true), (1e20, false)] {
        let many: Vec<[f32; 3]> = (0..MAX_POINTS).map(|i| [i as f32 * far, (i % 2) as f32 * far, 0.0]).collect();
        let s: StrokeSpec = serde_json::from_value(json!({ "brush": "inflate", "kind": { "curve": { "closed": true } }, "points": many, "radius": 0.02 })).unwrap();
        match crate::brush::resolve::resolve(&s, &ResolveCtx::local()) {
            Ok(r) => assert!(ok && r.dabs[0].points.len() <= MAX_POINTS, "path dabs keep the point bound"),
            Err(e) => assert!(!ok && e.contains("path points"), "{e}"),
        }
    }
}
