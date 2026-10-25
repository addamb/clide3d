//! Stage 1a acceptance (docs/SCULPT_PLAN.md 6): StrokeSpec, the resolver
//! and the engine, measured against fixtures and analytic shapes.

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::brush::resolve::ResolveCtx;
use crate::clay::{Field, with_threads};
use crate::harness::geom::brick_hash;

/// Hash of the legacy fixture's detail grid, taken with the sculpt code
/// before the refactor (commit 11090c8): legacy strokes must stay bit-equal.
const LEGACY_SCULPT_HASH: u64 = 0x5ce8_4c61_7277_2869;
/// The same for legacy paint strokes (FNV-1a of the paint JSON in the
/// file form of the time, before bricks were deflated).
const LEGACY_PAINT_HASH: u64 = 0xb6b5_840b_9985_247c;

fn legacy_clay() -> Clay {
    serde_json::from_value(json!({"blobs":[
        {"type":"box", "name":"slab", "center":[0,-0.3,0], "size":[1.2,0.6,1.2], "blend":0},
        {"type":"sphere", "name":"knob", "center":[0.35,0.0,0.35], "radius":0.12, "blend":0.03}
    ], "resolution":64, "detail_cell":0.01}))
    .unwrap()
}

/// Every brush of today's schema, with `point`, `mirror` and masks.
fn legacy_strokes() -> Vec<Value> {
    vec![
        json!({"brush":"inflate","point":[0.2,0.0,0.1],"radius":0.15,"strength":0.6,"mirror":true}),
        json!({"brush":"add","points":[[-0.3,0.0,-0.2],[0.3,0.02,-0.2]],"radius":0.05}),
        json!({"brush":"carve","points":[[-0.2,0.0,0.3],[0.2,0.0,0.3]],"radius":0.04,"strength":0.8}),
        json!({"brush":"smooth","point":[0.0,0.0,-0.2],"radius":0.15,"strength":0.5}),
        json!({"brush":"flatten","point":[0.2,0.02,0.1],"radius":0.12,"strength":0.7,"mirror":true}),
        json!({"brush":"grab","point":[0.4,0.0,-0.4],"radius":0.12,"delta":[0,0.05,0]}),
        json!({"brush":"crease","points":[[-0.4,0,0.0],[-0.1,0,0.05]],"radius":0.06,"strength":0.02,"mirror":true}),
        json!({"brush":"layer","point":[0.0,0.0,0.4],"radius":0.1,"strength":0.015}),
        json!({"brush":"pinch","points":[[-0.3,0.02,-0.2],[0.3,0.02,-0.2]],"radius":0.08,"strength":0.4}),
        json!({"brush":"stretch","point":[-0.4,0.0,0.4],"radius":0.08,"delta":[0,0.1,0.02]}),
        json!({"brush":"noise","point":[0.3,0.0,0.35],"radius":0.12,"strength":0.5,"scale":0.04,"pattern":"bumps"}),
        json!({"brush":"deflate","point":[-0.3,0.0,-0.4],"radius":0.1,"strength":0.5,"mask":{"above":-0.05}}),
        json!({"brush":"inflate","points":[[0.1,0,0],[0.15,0,0.05]],"radius":0.1,"invert":true,"mask":{"blobs":["slab"]}}),
    ]
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// The sculpt tool's path for strokes in object space (`space: local`).
fn sculpt_call(clay: &mut Clay, strokes: &[Value]) -> Vec<StrokeReport> {
    let ctx = ResolveCtx::local();
    prepare(strokes, None, &ctx).unwrap().iter().map(|(_, r)| run(clay, r).unwrap()).collect()
}

#[test]
fn legacy_sculpt_json_is_bit_equal_to_the_pre_refactor_fixture() {
    let mut c = legacy_clay();
    sculpt_call(&mut c, &legacy_strokes());
    assert_eq!(brick_hash(c.detail.as_ref().unwrap().grid()), LEGACY_SCULPT_HASH);
    // The older `Stroke` API (mirror left to the caller) gives the same.
    let mut c = legacy_clay();
    for raw in legacy_strokes() {
        let mut raw = raw.clone();
        if let Some(p) = raw.get("point").cloned() {
            raw["points"] = json!([p]);
        }
        let s: Stroke = serde_json::from_value(raw).unwrap();
        apply(&mut c, &s).unwrap();
        if s.mirror {
            apply(&mut c, &s.mirrored()).unwrap();
        }
        // Stroke is Serialize now, and reads back the same.
        let back: Stroke = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&s).unwrap());
        assert_eq!(s.to_spec(), serde_json::from_value::<StrokeSpec>(serde_json::to_value(&s).unwrap()).unwrap());
    }
    assert_eq!(brick_hash(c.detail.as_ref().unwrap().grid()), LEGACY_SCULPT_HASH);
}

#[test]
fn legacy_paint_json_is_bit_equal_to_the_pre_refactor_fixture() {
    let strokes = [
        json!({"points":[[0.2,0.0,0.1],[0.3,0.0,0.2]],"radius":0.08,"color":[0.9,0.2,0.1],"mirror":true}),
        json!({"points":[[0.0,0.0,0.3]],"radius":0.1,"color":[0.1,0.2,0.9],"hardness":0.9,"strength":0.7}),
        json!({"points":[[0.0,0.0,0.3]],"radius":0.05,"erase":true,"strength":0.6}),
        json!({"points":[[-0.3,0.0,-0.3],[0.3,0.0,-0.3]],"radius":0.06,"color":[0.2,0.8,0.2],"hardness":0.1,"mask":{"above":-0.02}}),
    ];
    let mut old = legacy_clay();
    for raw in &strokes {
        crate::paint::stroke(&mut old, &serde_json::from_value(raw.clone()).unwrap()).unwrap();
    }
    assert_eq!(fnv(old.paint.as_ref().unwrap().grid().raw_file_json().as_bytes()), LEGACY_PAINT_HASH);
    // The paint tool's path: each stroke a StrokeSpec with brush paint.
    let mut new = legacy_clay();
    for (_, r) in prepare(&strokes, Some(Brush::Paint), &ResolveCtx::local()).unwrap() {
        run(&mut new, &r).unwrap();
    }
    assert_eq!(fnv(new.paint.as_ref().unwrap().grid().raw_file_json().as_bytes()), LEGACY_PAINT_HASH);
    assert!(new.detail.is_none(), "paint strokes do not sculpt");
}

/// A unit sphere with a fine detail grid.
fn sphere(cell: f32) -> Clay {
    serde_json::from_value(json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":1.0,"blend":0}], "resolution":48, "detail_cell":cell})).unwrap()
}

/// The clay's field value at points (blobs plus detail).
struct Probe(Field);

impl Probe {
    fn new(c: &Clay) -> Probe {
        Probe(Field::new(c, c.detail_cell.unwrap()))
    }

    fn at(&self, p: Vec3) -> f32 {
        self.0.sample(p, &self.0.near(p - 0.2, p + 0.2)).0
    }

    /// Where the surface crosses the segment from `a` (inside) to `b`
    /// (outside), as a fraction of the way.
    fn crossing(&self, a: Vec3, b: Vec3) -> f32 {
        assert!(self.at(a) < 0.0 && self.at(b) > 0.0, "no crossing between {a} and {b}");
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..40 {
            let m = (lo + hi) * 0.5;
            if self.at(a.lerp(b, m)) < 0.0 { lo = m } else { hi = m }
        }
        (lo + hi) * 0.5
    }

    /// How far the surface of the unit sphere moved along +Z at `lateral`
    /// [x, y] from the top (where the dab is).
    fn lift(&self, x: f32, y: f32) -> f32 {
        let z0 = (1.0 - x * x - y * y).sqrt();
        let (a, b) = (Vec3::new(x, y, z0 - 0.05), Vec3::new(x, y, z0 + 0.05));
        a.z + (b.z - a.z) * self.crossing(a, b) - z0
    }

    /// Radius of the surface along a direction from the origin.
    fn radius(&self, dir: Vec3) -> f32 {
        0.9 + 0.2 * self.crossing(dir * 0.9, dir * 1.1)
    }
}

/// A standard dab at the top of the unit sphere (+Z).
fn dab_on_top(radius: f32, falloff: Value, cell: f32) -> Clay {
    let mut c = sphere(cell);
    let mut s = json!({"brush":"standard","point":[0,0,1],"radius":radius,"strength":0.5});
    if !falloff.is_null() {
        s["falloff"] = falloff;
    }
    let r = &sculpt_call(&mut c, &[s])[0];
    assert_eq!(r.dabs, 1);
    c
}

#[test]
fn a_standard_dab_has_the_analytic_peak_and_profile() {
    let (r, cell) = (0.05, 0.004);
    let height = 0.5 * r * 0.5;
    for (name, curve) in [("smooth", crate::brush::curve::Preset::Smooth), ("linear", crate::brush::curve::Preset::Linear), ("sharp", crate::brush::curve::Preset::Sharp)] {
        let c = dab_on_top(r, json!(name), cell);
        let probe = Probe::new(&c);
        let peak = probe.lift(0.0, 0.0);
        assert!((peak - height).abs() <= 0.5 * cell, "{name}: peak {peak}, analytic {height}");
        // The radial profile, along several directions.
        let want = crate::brush::curve::Falloff { preset: curve, ..Default::default() }.resolve();
        let mut sq = 0.0;
        let mut n = 0;
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::FRAC_PI_4 + 0.1;
            for i in 0..=20 {
                let rho = r * i as f32 / 20.0;
                let d = probe.lift(rho * a.cos(), rho * a.sin()) - height * want.eval(i as f32 / 20.0);
                sq += d * d;
                n += 1;
            }
        }
        let rms = (sq / n as f32).sqrt() / height;
        eprintln!("standard {name}: peak {peak:.5} (analytic {height:.5}), profile rms {:.2}%", rms * 100.0);
        assert!(rms <= 0.05, "{name}: profile rms {rms}");
        // Nothing changes beyond the radius: no grid point there moved,
        // and the surface beyond 1.05 r is where it was.
        for (key, values) in c.detail.as_ref().unwrap().grid().bricks() {
            for (at, v) in values.iter().enumerate() {
                if *v != 0.0 {
                    let g = Vec3::new((key[0] * 8 + at as i32 % 8) as f32, (key[1] * 8 + at as i32 / 8 % 8) as f32, (key[2] * 8 + at as i32 / 64) as f32) * cell;
                    let lateral = Vec3::new(g.x, g.y, 0.0).length();
                    assert!(lateral < r, "{name}: grid point {g} at {lateral} changed");
                }
            }
        }
        for k in 0..16 {
            let a = k as f32 * std::f32::consts::TAU / 16.0;
            for f in [1.05, 1.1, 1.3] {
                let lift = probe.lift(r * f * a.cos(), r * f * a.sin());
                assert!(lift.abs() <= 1e-4, "{name}: lift {lift} at {f} r");
            }
        }
    }
}

#[test]
fn standard_dab_and_stroke_timings() {
    // One dab of 25 cells at a 2 mm cell, and a 10-point path of them,
    // on a unit (meter) sphere: baselines for Stage 6.
    let (_, dab) = crate::harness::perf::time(5, || dab_on_top(0.05, Value::Null, 0.002));
    let path: Vec<[f32; 3]> = (0..10).map(|i| Vec3::new(i as f32 * 0.02 - 0.09, 0.0, 1.0).normalize().to_array()).collect();
    let (dabs, stroke) = crate::harness::perf::time(3, || {
        let mut c = sphere(0.002);
        sculpt_call(&mut c, &[json!({"brush":"standard","points":path,"radius":0.05})])[0].dabs
    });
    eprintln!("standard: one dab {dab:.1} ms; a {dabs}-dab stroke {stroke:.1} ms");
    crate::harness::perf::log("sculpt standard dab r=25 cells at 2 mm", dab, json!({}));
    crate::harness::perf::log("sculpt standard stroke 0.18 long, r=25 cells at 2 mm", stroke, json!({ "dabs": dabs }));
}

#[test]
fn flat_curves_are_flat_and_focal_shift_moves_the_half_height() {
    let (r, cell) = (0.05, 0.003);
    let c = dab_on_top(r, json!("flat"), cell);
    let probe = Probe::new(&c);
    let mut lifts = Vec::new();
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        for i in 0..=16 {
            let rho = 0.8 * r * i as f32 / 16.0;
            lifts.push(probe.lift(rho * a.cos(), rho * a.sin()));
        }
    }
    let mean = lifts.iter().sum::<f32>() / lifts.len() as f32;
    let sd = (lifts.iter().map(|h| (h - mean).powi(2)).sum::<f32>() / lifts.len() as f32).sqrt();
    eprintln!("flat: mean lift {mean:.5}, sd {:.3} voxel", sd / cell);
    assert!(sd <= 0.1 * cell, "flat sd {} voxel", sd / cell);

    // Focal shift +0.5 maps t to sqrt t: (1 - t)^2, half height at
    // t = 1 - 1/sqrt 2 of the radius.
    let (r, cell) = (0.1, 0.004);
    let c = dab_on_top(r, json!({"preset":"smooth","focal_shift":0.5}), cell);
    let probe = Probe::new(&c);
    let peak = probe.lift(0.0, 0.0);
    let analytic = r * (1.0 - 0.5f32.sqrt());
    for k in 0..4 {
        let a = k as f32 * std::f32::consts::FRAC_PI_2 + 0.3;
        let (mut lo, mut hi) = (0.0f32, r);
        for _ in 0..30 {
            let m = (lo + hi) * 0.5;
            if probe.lift(m * a.cos(), m * a.sin()) > peak * 0.5 { lo = m } else { hi = m }
        }
        let half = (lo + hi) * 0.5;
        eprintln!("focal +0.5: half height at {half:.5} (analytic {analytic:.5})");
        assert!((half - analytic).abs() <= 0.1 * analytic, "half height radius {half}, analytic {analytic}");
    }
}

#[test]
fn radial_and_mirror_symmetry_copy_the_stroke_exactly() {
    let cell = 0.004;
    let c0 = Vec3::new(0.6, 0.0, 0.8);
    let mut c = sphere(cell);
    let r = &sculpt_call(&mut c, &[json!({"brush":"standard","point":c0.to_array(),"radius":0.06,"strength":0.6,"symmetry":{"radial":{"axis":"y","count":6}}})])[0];
    assert_eq!((r.dabs, r.instances), (6, 6));
    let probe = Probe::new(&c);
    // Six bumps around the equator, 60 degrees apart.
    let base = c0.z.atan2(c0.x).to_degrees();
    let mut peaks = Vec::new();
    for k in 0..6 {
        let around = |deg: f32| {
            let a = deg.to_radians();
            probe.radius(Vec3::new(a.cos(), 0.0, a.sin()))
        };
        let center = base - 60.0 * k as f32;
        let (mut best, mut at) = (f32::MIN, center);
        for i in -200..=200 {
            let deg = center + i as f32 * 0.01;
            let h = around(deg);
            if h > best {
                (best, at) = (h, deg);
            }
        }
        assert!(best > 1.01, "bump {k} is {best}");
        peaks.push(at);
    }
    for w in peaks.windows(2) {
        let gap = (w[0] - w[1]).rem_euclid(360.0);
        assert!((gap - 60.0).abs() <= 0.5, "bumps {gap} degrees apart: {peaks:?}");
    }
    // The field is the same turned by 60 degrees, at points within a few
    // cells of the surface (farther out the field is clamped to a band and
    // carries no shape), most of them on the bumps.
    let turn = glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_3);
    let mut rng = crate::brush::rng::Pcg32::new(3, 1);
    let mut worst = 0.0f32;
    for i in 0..2000 {
        let near = if i % 4 != 0 { glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_3 * (i % 6) as f32) * c0 } else { Vec3::new(rng.next_f32() - 0.5, rng.next_f32() - 0.5, rng.next_f32() - 0.5).normalize() };
        let dir = (near + Vec3::new(rng.next_f32() - 0.5, rng.next_f32() - 0.5, rng.next_f32() - 0.5) * 0.15).normalize();
        let p = dir * (probe.radius(dir) + (rng.next_f32() - 0.5) * 3.0 * cell);
        worst = worst.max((probe.at(p) - probe.at(turn * p)).abs());
    }
    eprintln!("radial 6: bumps at {peaks:?}; worst field difference under a 60 degree turn {:.3} voxel", worst / cell);
    assert!(worst <= 0.5 * cell, "turned field differs by {} voxel", worst / cell);

    // X mirror: the field is the same across X = 0.
    let mut c = sphere(cell);
    let p0 = Vec3::new(0.45, 0.3, 0.84).normalize();
    let p1 = Vec3::new(0.3, 0.5, 0.81).normalize();
    sculpt_call(&mut c, &[
        json!({"brush":"standard","points":[p0.to_array(),p1.to_array()],"radius":0.05,"strength":0.5,"mirror":true}),
        json!({"brush":"crease","points":[p1.to_array(),(p1 + Vec3::new(0.1,-0.1,0.0)).normalize().to_array()],"radius":0.04,"strength":0.01,"symmetry":{"axes":["x"]}}),
    ]);
    let probe = Probe::new(&c);
    let mut worst = 0.0f32;
    for i in 0..2000 {
        let p = (p0.lerp(p1, rng.next_f32()) + Vec3::new(rng.next_f32() - 0.5, rng.next_f32() - 0.5, rng.next_f32() - 0.5) * 0.15).normalize() * (0.98 + 0.04 * rng.next_f32());
        let _ = i;
        worst = worst.max((probe.at(p) - probe.at(p * Vec3::new(-1.0, 1.0, 1.0))).abs());
    }
    eprintln!("x mirror: worst |T(p) - T(mirror p)| {worst:e}");
    assert!(worst <= 1e-4, "mirror differs by {worst}");
}

/// A slab with bumps and dents on top, to smooth.
fn lumpy() -> Clay {
    let mut c: Clay = serde_json::from_value(json!({"blobs":[{"type":"box","name":"slab","center":[0,-0.3,0],"size":[1.2,0.6,1.2],"blend":0}],"resolution":64,"detail_cell":0.01})).unwrap();
    sculpt_call(&mut c, &[json!({"brush":"noise","point":[0,0,0],"radius":0.4,"strength":1.0,"scale":0.06,"pattern":"bumps"})]);
    c
}

#[test]
fn smooth_peaks_only_lowers_and_valleys_only_raises() {
    let before = lumpy();
    let old = Probe::new(&before);
    let mut moved = [0usize; 2];
    for (k, mode) in ["peaks", "valleys"].into_iter().enumerate() {
        let mut c = before.clone();
        sculpt_call(&mut c, &[json!({"brush":"smooth","point":[0,0,0],"radius":0.3,"strength":1.0,"smooth_mode":mode})]);
        let new = Probe::new(&c);
        for i in 0..40 {
            for j in 0..40 {
                let (x, z) = (i as f32 * 0.012 - 0.24, j as f32 * 0.012 - 0.24);
                let (a, b) = (Vec3::new(x, -0.2, z), Vec3::new(x, 0.2, z));
                let (h0, h1) = (old.crossing(a, b) * 0.4 - 0.2, new.crossing(a, b) * 0.4 - 0.2);
                match mode {
                    "peaks" => assert!(h1 <= h0 + 1e-5, "peaks raised ({x}, {z}) from {h0} to {h1}"),
                    _ => assert!(h1 >= h0 - 1e-5, "valleys lowered ({x}, {z}) from {h0} to {h1}"),
                }
                if (h1 - h0).abs() > 1e-4 {
                    moved[k] += 1;
                }
            }
        }
    }
    assert!(moved.iter().all(|m| *m > 100), "smoothing did little: {moved:?}");
}

#[test]
fn pressure_scales_dabs() {
    let slab = || -> Clay { serde_json::from_value(json!({"blobs":[{"type":"box","center":[0,-0.3,0],"size":[1.2,0.6,1.2],"blend":0}],"resolution":64,"detail_cell":0.005})).unwrap() };
    // No pressure, no change: with size following pressure the dabs
    // vanish; with size off they have no strength.
    for size in ["linear", "off"] {
        let mut c = slab();
        let r = &sculpt_call(&mut c, &[json!({"brush":"standard","points":[[-0.2,0,0,0.0],[0.2,0,0,0.0]],"radius":0.05,"pressure":{"size":size}})])[0];
        assert_eq!(r.changed_points, 0, "size {size}");
    }
    // A pressure ramp: the bump widens monotonically along it.
    let mut c = slab();
    let pts: Vec<Value> = (0..=10).map(|i| json!([-0.3 + 0.06 * i as f32, 0.0, 0.0, i as f32 / 10.0])).collect();
    sculpt_call(&mut c, &[json!({"brush":"standard","points":pts,"radius":0.06,"strength":0.8})]);
    let probe = Probe::new(&c);
    let width = |x: f32| (0..=60).filter(|j| {
        let z = -0.075 + 0.0025 * *j as f32;
        let (a, b) = (Vec3::new(x, -0.05, z), Vec3::new(x, 0.05, z));
        probe.crossing(a, b) * 0.1 - 0.05 > 2e-4
    }).count();
    let widths: Vec<usize> = (0..11).map(|i| width(-0.27 + 0.05 * i as f32)).collect();
    eprintln!("pressure ramp bump widths (probe steps): {widths:?}");
    assert!(widths.windows(2).all(|w| w[1] + 1 >= w[0]), "widths {widths:?}");
    assert!(widths[10] > widths[1] + 10, "widths {widths:?}");
}

#[test]
fn spacing_spray_and_threads_replay_exactly() {
    // Spacing 0.25: ceil(length / (0.25 r)) dabs, give or take one.
    let mut c = sphere(0.01);
    let r = &sculpt_call(&mut c, &[json!({"brush":"standard","points":[[0,0,1],[0.3,0,0.954]],"radius":0.05,"spacing":0.25})])[0];
    let len = Vec3::new(0.0, 0.0, 1.0).distance(Vec3::new(0.3, 0.0, 0.954));
    assert!((r.dabs as f32 - (len / (0.25 * 0.05)).ceil()).abs() <= 1.0, "{} dabs", r.dabs);

    // Spray with a seed replays bit for bit; another seed differs.
    let spray = |seed: u64| {
        let mut c = sphere(0.01);
        sculpt_call(&mut c, &[json!({"brush":"standard","points":[[0,0,1],[0.3,0,0.954]],"radius":0.05,"kind":{"spray":{"count":3,"jitter":0.7,"seed":seed}},"snap":false})]);
        brick_hash(c.detail.as_ref().unwrap().grid())
    };
    assert_eq!(spray(7), spray(7));
    assert_ne!(spray(7), spray(8));

    // 200 random strokes: the same grid on 1 thread and on 12.
    let strokes = random_strokes(200);
    let run_on = |threads: usize| {
        with_threads(threads, || {
            let mut c = sphere(0.01);
            sculpt_call(&mut c, &strokes);
            brick_hash(c.detail.as_ref().unwrap().grid())
        })
    };
    let t = std::time::Instant::now();
    let one = run_on(1);
    let one_s = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let twelve = run_on(12);
    let twelve_s = t.elapsed().as_secs_f64();
    eprintln!("200 random strokes: {one_s:.2} s on 1 thread, {twelve_s:.2} s on 12; hash {one:#018x}");
    crate::harness::perf::log("sculpt 200 random strokes, 1 thread", one_s * 1000.0, json!({ "hash": format!("{one:#018x}") }));
    crate::harness::perf::log("sculpt 200 random strokes, 12 threads", twelve_s * 1000.0, json!({ "hash": format!("{twelve:#018x}") }));
    assert_eq!(one, twelve);
}

/// Mixed strokes over the upper half of the unit sphere, from a fixed seed.
fn random_strokes(n: usize) -> Vec<Value> {
    let mut rng = crate::brush::rng::Pcg32::new(2026, 10);
    let brushes = ["standard", "inflate", "deflate", "add", "carve", "smooth", "flatten", "crease", "layer", "pinch", "grab", "noise", "standard"];
    (0..n)
        .map(|_| {
            let mut f = || rng.next_f32();
            let on = |a: f32, b: f32| {
                let (t, p) = (a * std::f32::consts::TAU, b * 1.2);
                [p.sin() * t.cos(), p.cos(), p.sin() * t.sin()]
            };
            let brush = brushes[(f() * brushes.len() as f32) as usize % brushes.len()];
            let (a, b) = (f(), f());
            let p0 = on(a, b);
            let p1 = on(a + 0.02 * (f() - 0.5), b + 0.05 * (f() - 0.5));
            let mut s = json!({"brush":brush,"points":[p0,p1],"radius":0.04 + 0.06 * f(),"strength":0.2 + 0.6 * f()});
            match brush {
                "crease" | "layer" => s["strength"] = json!(0.004 + 0.01 * f()),
                "grab" => s["delta"] = json!([0.0, 0.02, 0.0]),
                "noise" => s["scale"] = json!(0.03),
                "standard" if f() < 0.5 => s["falloff"] = json!({"preset":"sphere","focal_shift":f() - 0.5}),
                _ => {}
            }
            if f() < 0.2 {
                s["mirror"] = json!(true);
            }
            s
        })
        .collect()
}

#[test]
fn mrgb_sculpts_and_paints_in_one_stroke() {
    let spec = json!({"points":[[0.0,0.0,1.0],[0.2,0.1,0.97]],"radius":0.06,"color":[0.9,0.1,0.1]});
    let mut both = sphere(0.01);
    let mut sculpted = spec.clone();
    sculpted["brush"] = json!("standard");
    sculpted["paint"] = json!({"color": [0.9, 0.1, 0.1]});
    sculpted.as_object_mut().unwrap().remove("color");
    let r = &sculpt_call(&mut both, &[sculpted])[0];
    assert!(r.changed_points > 0 && r.painted.is_some_and(|p| p > 0), "one record changes both grids");
    assert!(both.detail.is_some() && both.paint.is_some());
    let mut painted = sphere(0.01);
    for (_, r) in prepare(&[spec], Some(Brush::Paint), &ResolveCtx::local()).unwrap() {
        run(&mut painted, &r).unwrap();
    }
    // Coverage IoU over the painted grid points.
    let covered = |c: &Clay| -> std::collections::HashSet<[i32; 3]> {
        let g = c.paint.as_ref().unwrap().grid();
        let mut out = std::collections::HashSet::new();
        for x in -40..=40 { for y in -40..=40 { for z in 60..=110 {
            if g.get([x, y, z])[3] > 0 {
                out.insert([x, y, z]);
            }
        } } }
        out
    };
    let (a, b) = (covered(&both), covered(&painted));
    let iou = a.intersection(&b).count() as f32 / a.union(&b).count() as f32;
    eprintln!("MRGB paint coverage IoU {iou:.4} ({} vs {} points)", a.len(), b.len());
    assert!(iou >= 0.95, "IoU {iou}");
}

#[test]
fn flatten_sides_and_crease_taper() {
    // A bump and a dent, flattened with a plane between them.
    let base = lumpy();
    let old = Probe::new(&base);
    for side in ["above", "below"] {
        let mut c = base.clone();
        sculpt_call(&mut c, &[json!({"brush":"flatten","point":[0,0,0],"radius":0.3,"strength":1.0,"normal":[0,1,0],"side":side})]);
        let new = Probe::new(&c);
        for i in 0..30 {
            let x = i as f32 * 0.01 - 0.15;
            let (a, b) = (Vec3::new(x, -0.2, 0.05), Vec3::new(x, 0.2, 0.05));
            let (h0, h1) = (old.crossing(a, b) * 0.4 - 0.2, new.crossing(a, b) * 0.4 - 0.2);
            if side == "above" { assert!(h1 <= h0 + 1e-5, "above filled at {x}") } else { assert!(h1 >= h0 - 1e-5, "below cut at {x}") }
        }
    }
    // Dam Standard: a tapered crease is shallow at its ends, full in the middle.
    let slab = || -> Clay { serde_json::from_value(json!({"blobs":[{"type":"box","center":[0,-0.3,0],"size":[1.2,0.6,1.2],"blend":0}],"resolution":64,"detail_cell":0.005})).unwrap() };
    let mut c = slab();
    sculpt_call(&mut c, &[json!({"brush":"crease","points":[[-0.3,0,0],[0.3,0,0]],"radius":0.04,"strength":0.02,"taper":[0.3,0.3],"normal":[0,1,0]})]);
    let probe = Probe::new(&c);
    let depth = |x: f32| -(probe.crossing(Vec3::new(x, -0.05, 0.0), Vec3::new(x, 0.05, 0.0)) * 0.1 - 0.05);
    let (end, mid) = (depth(-0.29), depth(0.0));
    assert!(mid > 0.017 && end < 0.25 * mid, "taper: end {end}, middle {mid}");
}

#[test]
fn grab_and_stretch_expose_their_backward_map() {
    let params = crate::brush::resolve::Params::new(Brush::Grab, 1.0);
    let mut dab = crate::brush::resolve::Dab::path(vec![Vec3::ZERO], 0.1);
    dab.delta = Some(Vec3::new(0.0, 0.05, 0.0));
    let map = engine::transport(&params, &dab, 0.1, 1.0).unwrap();
    // The anchor's material came from a full delta back; beyond the
    // radius nothing moved.
    assert_eq!(map.source(Vec3::ZERO), Vec3::new(0.0, -0.05, 0.0));
    assert_eq!(map.source(Vec3::new(0.5, 0.0, 0.0)), Vec3::new(0.5, 0.0, 0.0));
    let stretch = crate::brush::resolve::Params::new(Brush::Stretch, 1.0);
    assert!(engine::transport(&stretch, &dab, 0.1, 1.0).is_some());
    assert!(engine::transport(&crate::brush::resolve::Params::new(Brush::Inflate, 1.0), &dab, 0.1, 1.0).is_none());
}
