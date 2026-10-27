//! Stage 1c acceptance (docs/QUALITY_PLAN.md): ClayPolish (defect 4) and
//! projected strokes (defect 5), on synthetic clays.

use glam::{IVec3, Vec3};
use serde_json::{Value, json};

use super::flow::{Polish, Region, passes_for};
use crate::brush::resolve::{ResolveCtx, Resolved, resolve};
use crate::brush::spec::StrokeSpec;
use crate::clay::{Clay, Detail, Field};
use crate::ledger::{Grid, SculptAction};

/// The detail cell of the fixtures.
const CELL: f32 = 0.01;

fn clay(blobs: Value) -> Clay {
    let mut c: Clay = serde_json::from_value(json!({ "resolution": 64, "blobs": blobs })).unwrap();
    c.detail_cell = Some(CELL);
    c
}

fn resolved(spec: Value) -> Resolved {
    let spec: StrokeSpec = serde_json::from_value(spec).unwrap();
    resolve(&spec, &ResolveCtx::local()).unwrap()
}

fn stroke(c: &mut Clay, spec: Value) {
    super::run(c, &resolved(spec)).unwrap();
}

/// Smooth value noise in -1..1 with features about 1 unit apart (integer
/// hash, smoothstep blend: the same on every platform).
fn value_noise(p: Vec3) -> f32 {
    let h = |i: IVec3| {
        let mut h = (i.x as u32).wrapping_mul(0x8da6_b343) ^ (i.y as u32).wrapping_mul(0xd816_3841) ^ (i.z as u32).wrapping_mul(0xcb1a_b31f);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        (h & 0xffff) as f32 / 32767.5 - 1.0
    };
    let f = p.floor();
    let (i, t) = (f.as_ivec3(), p - f);
    let s = t * t * (Vec3::splat(3.0) - 2.0 * t);
    let l = |a: f32, b: f32, w: f32| a + (b - a) * w;
    let c = |x: i32, y: i32, z: i32| h(i + IVec3::new(x, y, z));
    l(l(l(c(0, 0, 0), c(1, 0, 0), s.x), l(c(0, 1, 0), c(1, 1, 0), s.x), s.y), l(l(c(0, 0, 1), c(1, 0, 1), s.x), l(c(0, 1, 1), c(1, 1, 1), s.x), s.y), s.z)
}

/// Lumps `amp` high, about `size` across, displaced into the clay's
/// surface, faded out by `keep(p)` (0 where the surface stays as it is).
fn lumps(c: &mut Clay, amp: f32, size: f32, keep: &(dyn Fn(Vec3) -> f32 + Sync)) {
    let field = Field::new(c, CELL);
    let (lo, hi) = field.bounds().unwrap();
    // Normalised so the lumps' peaks are about `amp`.
    super::engine::displace(c, lo, hi, amp, &|_, _| true, &|p| amp * 1.6 * value_noise(p / size) * keep(p)).unwrap();
}

/// The composed field of a clay (blobs and sculpt).
struct Probe(Field);

impl Probe {
    fn new(c: &Clay) -> Probe {
        Probe(Field::new(c, CELL))
    }

    fn at(&self, p: Vec3) -> f32 {
        let e = Vec3::splat(CELL * 2.0);
        self.0.sample(p, &self.0.near(p - e - self.0.limit, p + e + self.0.limit)).0
    }

    /// Where the surface crosses the segment a..b (bisection; a outside).
    fn cross(&self, a: Vec3, b: Vec3) -> Vec3 {
        let (mut a, mut b) = (a, b);
        assert!(self.at(a) > 0.0 && self.at(b) < 0.0, "no crossing between {a} and {b}");
        for _ in 0..40 {
            let m = (a + b) * 0.5;
            if self.at(m) > 0.0 { a = m } else { b = m }
        }
        (a + b) * 0.5
    }

    fn normal(&self, p: Vec3) -> Vec3 {
        let e = CELL * 0.5;
        let d = |v: Vec3| self.at(p + v * e) - self.at(p - v * e);
        Vec3::new(d(Vec3::X), d(Vec3::Y), d(Vec3::Z)).normalize()
    }
}

/// RMS distance of points from their least-squares plane y = a + b x + c z
/// (or the same with the axes given).
fn plane_rms(pts: &[(f32, f32, f32)]) -> f32 {
    // Normal equations for h = a + b u + c v.
    let n = pts.len() as f64;
    let (mut su, mut sv, mut sh, mut suu, mut svv, mut suv, mut suh, mut svh) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &(u, v, h) in pts {
        let (u, v, h) = (u as f64, v as f64, h as f64);
        (su, sv, sh, suu, svv, suv, suh, svh) = (su + u, sv + v, sh + h, suu + u * u, svv + v * v, suv + u * v, suh + u * h, svh + v * h);
    }
    let m = glam::DMat3::from_cols_array(&[n, su, sv, su, suu, suv, sv, suv, svv]);
    let x = m.inverse() * glam::DVec3::new(sh, suh, svh);
    let rms = pts.iter().map(|&(u, v, h)| (h as f64 - x.x - x.y * u as f64 - x.z * v as f64).powi(2)).sum::<f64>() / n;
    rms.sqrt() as f32
}

/// The box fixture of defect 4: a 1 x 0.6 x 0.6 box (sharp edges), a
/// crease groove 2 cells deep along its top at z = -0.1, and lumps 0.3
/// cells high on its faces (faded out within a few cells of the top-front
/// edge and of the groove, so those start clean).
fn lumpy_box() -> Clay {
    let mut c = clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [1.0, 0.6, 0.6], "blend": 0 }]));
    stroke(&mut c, json!({ "brush": "crease", "points": [[-0.4, 0.3, -0.1], [0.4, 0.3, -0.1]], "radius": 4.0 * CELL, "strength": 2.0 * CELL, "normal": [0, 1, 0] }));
    let fade = |d: f32| super::noise::smooth01((d / CELL - 4.0) / 4.0);
    lumps(&mut c, 0.3 * CELL, 2.0 * CELL, &|p: Vec3| {
        let edge = Vec3::new(0.0, p.y - 0.3, p.z - 0.3).length();
        let groove = Vec3::new(0.0, p.y - 0.3, p.z + 0.1).length();
        fade(edge) * fade(groove)
    });
    c
}

/// What defect 4's checks measure on the box fixture.
#[derive(Debug, Clone, Copy)]
struct BoxShape {
    /// RMS of the top and front faces about their planes, in cells.
    rms: f32,
    /// The top face's alone.
    top: f32,
    /// The top-front edge's angle, from the normals 1.5 cells either side.
    dihedral: f32,
    /// The groove's depth and width at half depth, in cells.
    depth: f32,
    width: f32,
}

fn measure_box(c: &Clay) -> BoxShape {
    let p = Probe::new(c);
    let xs: Vec<f32> = (0..9).map(|i| -0.3 + 0.075 * i as f32).collect();
    // Faces: the top between the groove and the edge region, and the front.
    let mut top = Vec::new();
    let mut front = Vec::new();
    for &x in &xs {
        for k in 0..12 {
            let z = -0.27 + 0.016 * k as f32;
            if (z + 0.1).abs() < 0.07 {
                continue;
            }
            let q = p.cross(Vec3::new(x, 0.3 + 3.0 * CELL, z), Vec3::new(x, 0.3 - 3.0 * CELL, z));
            top.push((x, z, q.y));
            let y = -0.25 + 0.03 * k as f32;
            let q = p.cross(Vec3::new(x, y, 0.3 + 3.0 * CELL), Vec3::new(x, y, 0.3 - 3.0 * CELL));
            front.push((x, y, q.z));
        }
    }
    let rms = ((plane_rms(&top).powi(2) + plane_rms(&front).powi(2)) * 0.5).sqrt() / CELL;
    // The edge.
    let mut dihedral = 0.0;
    for &x in &xs {
        let at = 0.3 - 1.5 * CELL;
        // (A rounded edge has sunk there: the segments reach well in.)
        let a = p.cross(Vec3::new(x, 0.3 + 3.0 * CELL, at), Vec3::new(x, 0.3 - 20.0 * CELL, at));
        let b = p.cross(Vec3::new(x, at, 0.3 + 3.0 * CELL), Vec3::new(x, at, 0.3 - 20.0 * CELL));
        dihedral += 180.0 - p.normal(a).angle_between(p.normal(b)).to_degrees();
    }
    dihedral /= xs.len() as f32;
    // The groove: the top's height across it, against the top's level.
    let (mut depth, mut width) = (0.0, 0.0);
    for &x in &xs {
        let h = |z: f32| p.cross(Vec3::new(x, 0.3 + 3.0 * CELL, z), Vec3::new(x, 0.3 - 6.0 * CELL, z)).y;
        let level = (h(-0.1 - 0.065) + h(-0.1 + 0.065)) * 0.5;
        let zs: Vec<f32> = (0..=120).map(|i| -0.16 + 0.12 * i as f32 / 120.0).collect();
        let hs: Vec<f32> = zs.iter().map(|z| h(*z)).collect();
        let bottom = hs.iter().copied().fold(f32::MAX, f32::min);
        let d = level - bottom;
        let half = level - d * 0.5;
        let inside: Vec<f32> = zs.iter().zip(&hs).filter(|(_, h)| **h < half).map(|(z, _)| *z).collect();
        depth += d / CELL;
        width += (inside.last().unwrap() - inside.first().unwrap()) / CELL;
    }
    BoxShape { rms, top: plane_rms(&top) / CELL, dihedral, depth: depth / xs.len() as f32, width: width / xs.len() as f32 }
}

fn polish(c: &Clay, p: Polish) -> Clay {
    let after = Detail::act(c, c.detail.as_ref(), &SculptAction::Polish(p)).unwrap();
    Clay { detail: after, ..c.clone() }
}

fn whole(amount: f32, passes: Option<u32>) -> Polish {
    Polish { amount, angle: 30.0, region: None, passes: passes.unwrap_or_else(|| passes_for(amount)), keep_volume: true, cell: CELL, frozen: None }
}

/// Defect 4: polish takes the lumps off the faces and keeps the box's
/// edge and the crease groove; plain smooth at the same passes does not.
#[test]
fn polish_removes_lumps_keeps_creases() {
    let c = lumpy_box();
    let before = measure_box(&c);
    let t = std::time::Instant::now();
    let polished = polish(&c, whole(0.6, None));
    let took = t.elapsed();
    let after = measure_box(&polished);
    // Plain smooth with at least as many passes: strokes of 8 (strength 1).
    let mut smoothed = c.clone();
    let strokes = passes_for(0.6).div_ceil(8);
    for _ in 0..strokes {
        stroke(&mut smoothed, json!({ "brush": "smooth", "point": [0, 0.3, 0.1], "radius": 0.55, "strength": 1.0, "falloff": "flat" }));
    }
    let smooth = measure_box(&smoothed);
    eprintln!("defect 4 box: before {before:?}\n  polish 0.6 ({} passes, {took:?}): {after:?}\n  smooth {} passes: {smooth:?}", passes_for(0.6), strokes * 8);
    assert!(after.rms <= 0.25 * before.rms, "lumps {:.3} -> {:.3} cells", before.rms, after.rms);
    assert!((after.dihedral - 90.0).abs() <= 3.0, "edge {:.1} degrees", after.dihedral);
    assert!(after.depth >= 0.85 * before.depth && (after.width / before.width - 1.0).abs() <= 0.1, "groove {:.2} x {:.2} -> {:.2} x {:.2} cells", before.depth, before.width, after.depth, after.width);
    // Why polish exists: smooth rounds the edge and fills the groove.
    assert!((smooth.dihedral - 90.0).abs() > 3.0 && smooth.depth < 0.85 * before.depth, "smooth kept the edge ({:.1}) and groove ({:.2})", smooth.dihedral, smooth.depth);
}

/// The defect 5 fixture: a skull cap (ellipsoid) with a back panel (a box
/// sloping down and back 20 degrees, blend 0.01) whose top meets the cap's
/// back at 110 degrees, meshed at about the cell.
fn cap_and_panel() -> Clay {
    let (s, c) = (20f32.to_radians().sin(), 20f32.to_radians().cos());
    // The panel's top passes through (0, 0, -0.2), running back and down.
    let center = Vec3::new(0.0, 0.0, -0.2) + Vec3::new(0.0, -s, -c) * 0.13 - Vec3::new(0.0, c, -s) * 0.02;
    clay(json!([
        { "name": "cap", "type": "ellipsoid", "center": [0, 0, 0], "radii": [0.25, 0.2, 0.25] },
        { "name": "panel", "type": "box", "center": center.to_array(), "size": [0.4, 0.04, 0.3], "rotate": [-20, 0, 0], "blend": 0.01 }
    ]))
}

/// The plane the strand runs in (x = STRAND_X), off the clay's mirror plane
/// so the cap's normal leans out of it.
const STRAND_X: f32 = 0.1;

/// A crease path an agent would draw along the strand: `n` points two cells
/// off the surface (along the section's normal, in the plane), down the
/// cap's back and on along the panel's top, across their junction. Each
/// point with the sheet it was drawn over (true: the cap).
fn strand_path(n: usize) -> Vec<(Vec3, bool)> {
    let off = 2.0 * CELL;
    let x = STRAND_X;
    // The cap's section at x: an ellipse of radii (b, c) in (y, z).
    let k = (1.0 - (x / 0.25).powi(2)).sqrt();
    let (b, c) = (0.2 * k, 0.25 * k);
    let cap = |t: f32| {
        let p = Vec3::new(x, b * t.sin(), -c * t.cos());
        let n = Vec3::new(0.0, t.sin() / b, -t.cos() / c).normalize();
        p + n * off
    };
    // The panel's top in the plane: y = tan20 (z + 0.2), normal up and back.
    let (s20, c20) = (20f32.to_radians().sin(), 20f32.to_radians().cos());
    let panel_n = Vec3::new(0.0, c20, -s20);
    let panel = |z: f32| Vec3::new(x, (z + 0.2) * s20 / c20, z) + panel_n * off;
    // Down the cap until its offset meets the panel's.
    let above = |p: Vec3| (p - panel(p.z)).dot(panel_n) > 0.0;
    let mut pts: Vec<(Vec3, bool)> = Vec::new();
    let mut t = 0.9f32;
    while above(cap(t)) && t > -0.6 {
        pts.push((cap(t), true));
        t -= 0.002;
    }
    let z0 = pts.last().unwrap().0.z;
    let mut z = z0;
    while z > -0.4 {
        z -= 0.002;
        pts.push((panel(z), false));
    }
    // n points evenly along it.
    let len: Vec<f32> = std::iter::once(0.0).chain(pts.windows(2).scan(0.0, |a, w| { *a += w[0].0.distance(w[1].0); Some(*a) })).collect();
    let total = *len.last().unwrap();
    (0..n)
        .map(|i| {
            let at = total * i as f32 / (n - 1) as f32;
            let j = len.partition_point(|l| *l < at).min(pts.len() - 1);
            pts[j]
        })
        .collect()
}

/// Defect 5: a crease stroke drawn along a strand across the cap and panel
/// junction. Snapped (frame surface) points leave the strand's plane where
/// the cap leans out of it and step unevenly at the junction; projected
/// across the path in that plane they stay in it, on the sheet under them,
/// about as evenly spaced as drawn.
#[test]
fn projected_crease_follows_junction() {
    let c = cap_and_panel();
    let mesh = c.build_mesh();
    let surface = crate::surface::Surface::new(&c, &mesh).unwrap();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let snap = |p: Vec3| surface.snap(p);
    let cast = |o: Vec3, d: Vec3| {
        let h = bvh.hit(&mesh, o, d)?;
        let v = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
        let ix = &mesh.indices[h.tri * 3..h.tri * 3 + 3];
        let n = (v(ix[1]) - v(ix[0])).cross(v(ix[2]) - v(ix[0])).normalize_or_zero();
        Some((o + d * h.t, if n.dot(d) > 0.0 { -n } else { n }))
    };
    let ctx = ResolveCtx { snap: Some(&snap), cast: Some(&cast), ..ResolveCtx::local() };
    let path = strand_path(40);
    let spacing = path.windows(2).map(|w| w[0].0.distance(w[1].0)).fold(0.0, f32::max);
    // Each sheet's own field, to tell which one a point is on.
    let sheet = |name: &str| {
        let mut one = c.clone();
        one.blobs.retain(|b| b.name == name);
        Probe::new(&one)
    };
    let (cap, panel) = (sheet("cap"), sheet("panel"));
    let whole = Probe::new(&c);
    let run = |frame: Value| {
        let spec = json!({ "brush": "crease", "points": path.iter().map(|p| p.0.to_array()).collect::<Vec<_>>(), "radius": 3.0 * CELL, "strength": CELL, "frame": frame });
        let spec: StrokeSpec = serde_json::from_value(spec).unwrap();
        let r = resolve(&spec, &ctx).unwrap();
        let pts = r.dabs[0].points.clone();
        assert_eq!(pts.len(), path.len());
        // How far out of the strand's plane, how far off the surface, how
        // many left the sheet they were drawn over (more than a cell from it
        // and nearer the other), and the largest step against the drawn one.
        let plane = pts.iter().map(|p| (p.x - STRAND_X).abs()).fold(0.0, f32::max) / CELL;
        let off = pts.iter().map(|p| whole.at(*p).abs()).fold(0.0, f32::max) / CELL;
        let jumped = pts.iter().zip(&path).filter(|(p, (_, on_cap))| {
            let (own, other) = if *on_cap { (cap.at(**p), panel.at(**p)) } else { (panel.at(**p), cap.at(**p)) };
            own.abs() > CELL && other.abs() < own.abs()
        }).count();
        let step = pts.windows(2).map(|w| w[0].distance(w[1])).fold(0.0, f32::max) / spacing;
        (plane, off, jumped, step)
    };
    let snapped = run(json!({ "surface": {} }));
    let projected = run(json!({ "project": { "along": "plane", "plane": { "normal": [1, 0, 0] } } }));
    eprintln!("defect 5 junction (cells; step against the drawn spacing): surface: off-plane {:.3}, off-surface {:.3}, jumped {}, step {:.2}; projected: off-plane {:.4}, off-surface {:.3}, jumped {}, step {:.2}", snapped.0, snapped.1, snapped.2, snapped.3, projected.0, projected.1, projected.2, projected.3);
    assert!(projected.0 <= 0.25 && projected.2 == 0 && projected.3 <= 1.5, "projected {projected:?}");
    assert!(projected.1 <= 0.5, "projected points are on the surface: {projected:?}");
    // What snapping does here (the nearest surface point): the cap leans
    // out of the plane, so its points leave it, and at the junction they
    // bunch up and step.
    assert!(snapped.0 > 0.25 && snapped.3 > 1.5, "surface snapping kept to the plane and the spacing: {snapped:?}");
}

/// A ball of radius 0.3 (30 cells) with lumps 0.3 cells high all over.
fn lumpy_ball() -> Clay {
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 }]));
    lumps(&mut c, 0.3 * CELL, 2.0 * CELL, &|_| 1.0);
    c
}

/// The ball's lumps: the RMS of its surface radius about 0.3 over a
/// latitude-longitude net, in cells.
fn ball_rms(c: &Clay) -> f32 {
    let p = Probe::new(c);
    let mut sum = 0.0f64;
    let mut n = 0;
    for i in 1..12 {
        for j in 0..24 {
            let (t, f) = (std::f32::consts::PI * i as f32 / 12.0, std::f32::consts::TAU * j as f32 / 24.0);
            let d = Vec3::new(t.sin() * f.cos(), t.cos(), t.sin() * f.sin());
            let r = p.cross(d * (0.3 + 3.0 * CELL), d * (0.3 - 3.0 * CELL)).length();
            sum += ((r - 0.3) as f64).powi(2);
            n += 1;
        }
    }
    (sum / n as f64).sqrt() as f32 / CELL
}

/// Polish keeps a lumpy ball's volume (keep_volume); without it the ball
/// shrinks as curvature flow shrinks every closed surface.
#[test]
fn polish_volume_kept() {
    let c = lumpy_ball();
    let volume = 4.0 / 3.0 * std::f32::consts::PI * 0.3f32.powi(3);
    let kept = polish(&c, whole(0.6, None));
    let free = polish(&c, Polish { keep_volume: false, ..whole(0.6, None) });
    let change = |after: &Clay| crate::ledger::volume_delta(&c, c.detail.as_ref(), after.detail.as_ref()) / volume;
    let (k, f) = (change(&kept), change(&free));
    eprintln!("lumpy ball: lumps {:.3} -> {:.3} cells; volume {:+.4}% kept, {:+.4}% without keep_volume", ball_rms(&c), ball_rms(&kept), k * 100.0, f * 100.0);
    assert!(k.abs() <= 0.001, "volume changed {:+.4}%", k * 100.0);
    assert!(f < -0.002, "the free flow shrinks the ball ({:+.4}%)", f * 100.0);
    assert!(ball_rms(&kept) <= 0.4 * ball_rms(&c));
}

/// `edit_clay polish` and `clay_polish` dabs: the same grid on 1 and 12
/// threads, replayed from the ledger, and after a save and load.
#[test]
fn polish_is_deterministic() {
    let base = lumpy_box();
    let region = Polish { region: Some(Region { center: Vec3::new(0.1, 0.3, 0.0), radii: Vec3::new(0.25, 0.15, 0.25) }), ..whole(0.5, None) };
    let dab = json!({ "brush": "clay_polish", "points": [[-0.3, 0.3, 0.2], [0.3, 0.3, 0.2]], "radius": 0.06, "strength": 0.7, "angle": 25 });
    let run = |threads: usize| {
        crate::clay::with_threads(threads, || {
            let mut c = base.clone();
            let meta = crate::ledger::Meta::agent("edit_clay", None);
            let done = crate::ledger::commit_action::<Detail>(&c, &crate::ledger::ledger(&c), &meta, json!({ "polish": {} }), SculptAction::Polish(region), None).unwrap();
            crate::ledger::install(&mut c, done.ledger, done.grid);
            let (spec, r) = super::prepare(&[dab.clone()], None, &ResolveCtx::local()).unwrap().remove(0);
            let done = crate::ledger::commit::<Detail>(&c, &crate::ledger::ledger(&c), &crate::ledger::Meta::agent("sculpt", None), &spec, &r, false, None).unwrap();
            crate::ledger::install(&mut c, done.ledger, done.grid);
            c
        })
    };
    let (one, many) = (run(1), run(12));
    assert_ne!(Detail::hash(one.detail.as_ref()), Detail::hash(base.detail.as_ref()), "polish changed the grid");
    assert_eq!(Detail::hash(one.detail.as_ref()), Detail::hash(many.detail.as_ref()), "1 and 12 threads");
    let l = crate::ledger::ledger::<Detail>(&one);
    assert_eq!(l.len(), 3, "the lumpy box's own grid, the polish and the dab");
    let again = crate::ledger::replay(&l, &one, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(again.as_ref()), Detail::hash(one.detail.as_ref()), "replayed from the ledger");
    let mut back: Clay = serde_json::from_str(&serde_json::to_string(&one).unwrap()).unwrap();
    crate::ledger::after_load(&mut back);
    let l = crate::ledger::ledger::<Detail>(&back);
    assert_eq!(l.verified(), Some(true), "the saved ledger replays to its grid");
    let replayed = crate::ledger::replay(&l, &back, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(replayed.as_ref()), Detail::hash(one.detail.as_ref()), "after a save and load");
}

/// Every field of a polish record changes its digest.
#[test]
fn polish_digest_covers_fields() {
    let c = lumpy_ball();
    let p = Polish { region: Some(Region { center: Vec3::new(0.0, 0.3, 0.0), radii: Vec3::splat(0.1) }), ..whole(0.5, None) };
    let meta = crate::ledger::Meta::agent("edit_clay", None);
    let done = crate::ledger::commit_action::<Detail>(&c, &crate::ledger::ledger(&c), &meta, json!({ "polish": {} }), SculptAction::Polish(p), None).unwrap();
    let record = (*done.record).clone();
    let digest = |q: Polish| {
        let mut r = record.clone();
        r.op = crate::ledger::RecordOp::Action { spec: Box::new(json!({ "polish": {} })), action: Box::new(SculptAction::Polish(q)) };
        r.digest = Default::default();
        r.digest()
    };
    assert_eq!(digest(p), record.digest());
    let r = p.region.unwrap();
    let variants = [
        Polish { amount: 0.6, ..p },
        Polish { angle: 40.0, ..p },
        Polish { region: None, ..p },
        Polish { region: Some(Region { center: r.center + Vec3::X * 1e-3, ..r }), ..p },
        Polish { region: Some(Region { radii: r.radii * 1.01, ..r }), ..p },
        Polish { passes: p.passes + 1, ..p },
        Polish { keep_volume: false, ..p },
        Polish { cell: CELL * 0.5, ..p },
        Polish { frozen: Some(crate::brush::resolve::FrozenRef { mask: Some(Default::default()), groups: None }), ..p },
    ];
    for (i, q) in variants.into_iter().enumerate() {
        assert_ne!(digest(q), record.digest(), "variant {i}: {q:?}");
    }
    // A mirrored polish moves its region across x = 0.
    let m = <Detail as Grid>::mirror_action(&SculptAction::Polish(Polish { region: Some(Region { center: Vec3::new(0.2, 0.1, 0.0), ..r }), ..p }));
    let SculptAction::Polish(m) = m;
    assert_eq!(m.region.unwrap().center, Vec3::new(-0.2, 0.1, 0.0));
}

/// A projected stroke's record keeps the points where they landed: it
/// replays with nothing to cast at, and its digest follows those points.
#[test]
fn projected_records_replay_without_rays() {
    let c = cap_and_panel();
    let mesh = c.build_mesh();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let cast = |o: Vec3, d: Vec3| bvh.hit(&mesh, o, d).map(|h| (o + d * h.t, -d));
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let points: Vec<[f32; 3]> = strand_path(12).iter().map(|p| p.0.to_array()).collect();
    let spec = |offset: f32| json!({ "brush": "crease", "points": points, "radius": 3.0 * CELL, "strength": CELL, "frame": { "project": { "along": "plane", "plane": { "normal": [1, 0, 0] }, "offset": offset } } });
    let (s, r) = super::prepare(&[spec(0.0)], None, &ctx).unwrap().remove(0);
    let meta = crate::ledger::Meta::agent("sculpt", None);
    let mut sculpted = c.clone();
    let done = crate::ledger::commit::<Detail>(&sculpted, &crate::ledger::ledger(&sculpted), &meta, &s, &r, false, None).unwrap();
    crate::ledger::install(&mut sculpted, done.ledger, done.grid);
    // Resolving again needs rays; replaying does not.
    assert!(super::prepare(&[spec(0.0)], None, &ResolveCtx::local()).unwrap_err().contains("clay surface to hit"));
    let l = crate::ledger::ledger::<Detail>(&sculpted);
    let again = crate::ledger::replay(&l, &sculpted, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(again.as_ref()), Detail::hash(sculpted.detail.as_ref()));
    // The stored spec still says project, so a resend projects again.
    let stored: StrokeSpec = serde_json::from_value(serde_json::to_value(l.records()[0].stroke().unwrap().0).unwrap()).unwrap();
    assert!(matches!(stored.frame, Some(crate::brush::spec::Frame::Project(_))));
    // Another offset lands elsewhere: another digest.
    let (s2, r2) = super::prepare(&[spec(CELL)], None, &ctx).unwrap().remove(0);
    let other = crate::ledger::commit::<Detail>(&c, &crate::ledger::ledger(&c), &meta, &s2, &r2, false, None).unwrap();
    assert_ne!(other.record.digest(), done.record.digest());
    // Points that hit nothing are refused, by index, before any record.
    let far = json!({ "brush": "crease", "points": [[0.1, 0.2, -0.16], [0.1, 3.0, 3.0], [0.1, 3.1, 3.0]], "radius": 3.0 * CELL, "strength": CELL, "frame": { "project": { "along": [0, -1, 0] } } });
    let err = super::prepare(&[far], None, &ctx).unwrap_err();
    assert!(err.contains("projected points 1, 2 (of 3) hit no clay"), "{err}");
}

/// A clay_polish dab takes lumps off under it and leaves the groove.
#[test]
fn clay_polish_dabs_smooth_lumps_and_keep_the_groove() {
    let c = lumpy_box();
    let mut d = c.clone();
    // Along the top face, over the groove: 3 strokes of dabs 8 passes each.
    for _ in 0..3 {
        stroke(&mut d, json!({ "brush": "clay_polish", "points": [[-0.35, 0.3, -0.12], [0.35, 0.3, -0.12]], "radius": 0.2, "strength": 1.0, "falloff": "flat" }));
    }
    let (before, after) = (measure_box(&c), measure_box(&d));
    eprintln!("clay_polish dabs: {before:?} -> {after:?}");
    assert!(after.top < 0.5 * before.top, "lumps {:.3} -> {:.3}", before.top, after.top);
    assert!(after.depth >= 0.85 * before.depth && (after.dihedral - 90.0).abs() <= 3.0, "{after:?}");
}

/// A region polish changes nothing outside its ellipsoid.
#[test]
fn region_polish_stays_in_its_region() {
    let c = lumpy_ball();
    let r = Region { center: Vec3::new(0.0, 0.3, 0.0), radii: Vec3::splat(0.12) };
    let after = polish(&c, Polish { region: Some(r), ..whole(0.5, None) });
    let mut moved = 0;
    crate::ledger::detail_changes(c.detail.as_ref(), after.detail.as_ref(), |p, _, _, _| {
        assert!(((p - r.center) / r.radii).length() <= 1.0 + 1e-3, "{p} moved");
        moved += 1;
    });
    assert!(moved > 1000, "{moved} points moved");
}

/// A polish that would work on too much surface is refused before the band
/// is read, saying what fits.
#[test]
fn huge_polish_is_refused_before_it_allocates() {
    let mut c = clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [4, 4, 4] }]));
    c.detail_cell = Some(0.0005);
    let t = std::time::Instant::now();
    let err = Detail::act(&c, None, &SculptAction::Polish(Polish { cell: 0.0005, ..whole(0.5, None) })).unwrap_err();
    eprintln!("refused in {:?}: {err}", t.elapsed());
    assert!(err.contains("too big"), "{err}");
    // A box it may cover, but with about 18M surface points: refused once
    // the bricks read pass the cap, not after the band is built.
    let mut ball = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 }]));
    ball.detail_cell = Some(0.0005);
    let t = std::time::Instant::now();
    let err = Detail::act(&ball, None, &SculptAction::Polish(Polish { cell: 0.0005, ..whole(0.5, None) })).unwrap_err();
    eprintln!("refused in {:?}: {err}", t.elapsed());
    assert!(err.contains(&format!("more than {} surface points", super::flow::MAX_BAND)), "{err}");
    let bad = Detail::act(&c, None, &SculptAction::Polish(Polish { angle: 2.0, cell: 0.0005, ..whole(0.5, None) })).unwrap_err();
    assert!(bad.contains("angle 5-90"), "{bad}");
}

/// Stage 1c's performance targets (perf: `--ignored`; CLIDE_PERF=1 holds
/// the bounds, otherwise they get the harness's 3x slack): a whole-clay
/// polish of about 1M band points at 0.5 mm cells, 8 passes, in 2 s; a
/// clay_polish dab of 30 cells in 8 ms a pass; a projected frame in 0.2 ms
/// a point.
#[test]
#[ignore]
fn stage1c_perf() {
    use crate::harness::perf;
    let cell = 0.0005;
    let mut c: Clay = serde_json::from_value(json!({ "resolution": 96, "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.07 }] })).unwrap();
    c.detail_cell = Some(cell);
    let p = Polish { passes: 8, cell, ..whole(0.5, None) };
    let ((diff, worked), ms) = perf::time(3, || super::flow::polish(&c, None, &p).unwrap());
    let band = worked.points;
    eprintln!("polish: {band} band points, 8 passes: {ms:.0} ms ({} points changed), working memory {:.1} MB ({:.1} MB per 1M band points)", diff.changed_points, worked.bytes as f64 / 1e6, worked.bytes as f64 / band as f64);
    assert!(band > 900_000, "{band} band points");
    perf::check("stage1c polish 1M band x 8", ms, 2000.0, json!({ "band_points": band, "changed": diff.changed_points }));
    // A dab of 30 cells on the ball, 8 passes (strength 1).
    c.detail = Some(diff.detail);
    let dab = resolved(json!({ "brush": "clay_polish", "point": [0, 0.07, 0], "radius": 30.0 * cell, "strength": 1.0 }));
    let (_, ms) = perf::time(3, || {
        let mut d = c.clone();
        super::run(&mut d, &dab).unwrap()
    });
    eprintln!("clay_polish dab r = 30 cells: {ms:.1} ms for 8 passes");
    perf::check("stage1c clay_polish dab r30 per pass", ms / 8.0, 8.0, json!({}));
    // Projected against plain: 512 points down onto the ball.
    let mesh = c.build_mesh();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let cast = |o: Vec3, d: Vec3| bvh.hit(&mesh, o, d).map(|h| (o + d * h.t, -d));
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let points: Vec<[f32; 3]> = (0..512).map(|i| { let a = i as f32 / 512.0 * std::f32::consts::TAU; [0.04 * a.cos(), 0.08, 0.04 * a.sin()] }).collect();
    let spec = |frame: Value| serde_json::from_value::<StrokeSpec>(json!({ "brush": "crease", "points": points, "radius": 0.002, "strength": 0.0005, "frame": frame })).unwrap();
    let (plain, projected) = (spec(json!("world")), spec(json!({ "project": { "along": [0, -1, 0], "reach": 0.05 } })));
    let (_, a) = perf::time(5, || resolve(&plain, &ctx).unwrap());
    let (_, b) = perf::time(5, || resolve(&projected, &ctx).unwrap());
    eprintln!("projected frame: {:.4} ms a point ({b:.2} ms against {a:.2} ms for 512)", (b - a) / 512.0);
    perf::check("stage1c projected frame per point", (b - a).max(0.0) / 512.0, 0.2, json!({}));
}

/// A polish reaching a detail region polishes it at its own cell, the
/// same on 1 and 12 threads.
#[test]
fn polish_reaches_detail_regions() {
    let mut c = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 }]));
    c.set_detail_regions(&[crate::clay::Region { name: "top".into(), center: Vec3::new(0.0, 0.3, 0.0), radii: Vec3::splat(0.1), cell: 0.005 }]).unwrap();
    lumps(&mut c, 0.3 * CELL, 2.0 * CELL, &|_| 1.0);
    let p = Polish { region: Some(Region { center: Vec3::new(0.0, 0.3, 0.0), radii: Vec3::splat(0.15) }), ..whole(0.3, None) };
    let run = |n: usize| crate::clay::with_threads(n, || polish(&c, p));
    let (one, many) = (run(1), run(12));
    assert_eq!(Detail::hash(one.detail.as_ref()), Detail::hash(many.detail.as_ref()));
    let level = |x: &Clay| {
        let mut b: Vec<([i32; 3], Vec<u32>)> = x.detail.as_ref().unwrap().grid().levels()[0].grid.bricks().map(|(k, v)| (*k, v.iter().map(|f| f.to_bits()).collect())).collect();
        b.sort();
        b
    };
    assert_ne!(level(&one), level(&c), "the region's own grid was polished");
}

/// What the resolver refuses for the new stroke fields.
#[test]
fn clay_polish_and_project_refusals() {
    let cast = |o: Vec3, d: Vec3| Some((o + d * 0.1, -d));
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let fails = |spec: Value, why: &str| {
        let err = super::prepare(&[spec.clone()], None, &ctx).unwrap_err();
        assert!(err.contains(why), "{spec}: {err}");
    };
    fails(json!({ "brush": "crease", "point": [0, 0, 0], "radius": 0.1, "angle": 20 }), "angle and keep_volume are for brush clay_polish");
    fails(json!({ "brush": "clay_polish", "point": [0, 0, 0], "radius": 0.1, "angle": 95 }), "angle must be 5 to 90");
    fails(json!({ "brush": "clay_polish", "point": [0, 0, 0], "radius": 0.1, "alpha": { "name": "a" } }), "clay_polish takes no alpha");
    fails(json!({ "brush": "crease", "points": [[0, 0, 0], [1, 0, 0]], "radius": 0.1, "snap": true, "frame": { "project": { "along": [0, -1, 0] } } }), "leave snap out");
    fails(json!({ "brush": "crease", "point": [0, 0, 0], "radius": 0.1, "frame": { "project": { "along": "plane", "plane": { "normal": [1, 0, 0] } } } }), "2 or more points");
    fails(json!({ "brush": "crease", "point": [0, 0, 0], "radius": 0.1, "frame": { "project": { "along": [0, -1, 0], "plane": { "normal": [1, 0, 0] } } } }), "goes with along");
    fails(json!({ "brush": "crease", "point": [0, 0, 0], "radius": 0.1, "frame": { "project": { "along": "view:side" } } }), "view:front|back");
    fails(json!({ "brush": "crease", "point": [0, 0, 0], "radius": 0.1, "frame": { "project": { "along": [0, 0, 0] } } }), "nonzero, finite direction");
    fails(json!({ "brush": "crease", "points": [[0, 0, 0], [0, 1, 0]], "radius": 0.1, "frame": { "project": { "along": "plane", "plane": { "normal": [0, 1, 0] } } } }), "runs along the plane's normal");
    // Resolved defaults are stored on clay_polish strokes only.
    let r = super::prepare(&[json!({ "brush": "clay_polish", "point": [0, 0, 0], "radius": 0.1 })], None, &ctx).unwrap().remove(0).1;
    assert_eq!((r.params.angle, r.params.keep_volume), (Some(30.0), Some(true)));
    let r = super::prepare(&[json!({ "brush": "smooth", "point": [0, 0, 0], "radius": 0.1 })], None, &ctx).unwrap().remove(0).1;
    assert!(r.params.angle.is_none() && !serde_json::to_string(&r.params).unwrap().contains("angle"), "other brushes' params read as before");
}

/// The ray caster of a clay's mesh, as the editor's (`app::probe::cast`).
fn caster(mesh: &crate::mesh::Mesh, bvh: &crate::mesh::MeshBvh, o: Vec3, d: Vec3) -> Option<(Vec3, Vec3)> {
    bvh.hit(mesh, o, d).map(|h| (o + d * h.t, -d))
}

/// A projected curve is cast again after its smooth path is made, from
/// points already on the surface: those hit the sheet they are on, not the
/// far side of a thin sheet (nor nothing, on a thick one).
#[test]
fn projected_curves_stay_on_the_near_sheet() {
    // A thick slab, down the top view: before, the landed points' rays
    // passed through the top and the bottom was out of reach (refused).
    let slab = clay(json!([{ "type": "box", "center": [0, 0, 0], "size": [0.8, 0.6, 0.6], "blend": 0 }]));
    let mesh = slab.build_mesh();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let cast = |o: Vec3, d: Vec3| caster(&mesh, &bvh, o, d);
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let spec = json!({ "brush": "crease", "points": [[-0.2, 0.33, -0.1], [0.0, 0.34, 0.0], [0.2, 0.33, 0.1]], "kind": { "curve": {} }, "radius": 0.03, "strength": 0.002, "frame": { "project": { "along": "view:top" } } });
    let (_, r) = super::prepare(&[spec], None, &ctx).unwrap().remove(0);
    let pts = &r.dabs[0].points;
    assert!(pts.len() > 10 && pts.iter().all(|p| (p.y - 0.3).abs() < 0.002), "on the top: {pts:?}");
    // A thin sheet (4 cm, within reach) with plane rays both ways: before,
    // the curve landed on its underside.
    let mut sheet: Clay = serde_json::from_value(json!({ "resolution": 128, "blobs": [{ "type": "box", "center": [0, 0, 0], "size": [0.4, 0.04, 0.4], "blend": 0 }] })).unwrap();
    sheet.detail_cell = Some(CELL);
    let mesh = sheet.build_mesh();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let cast = |o: Vec3, d: Vec3| caster(&mesh, &bvh, o, d);
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let spec = json!({ "brush": "crease", "points": [[-0.15, 0.03, 0.0], [0.0, 0.035, 0.0], [0.15, 0.03, 0.0]], "kind": { "curve": {} }, "radius": 0.03, "strength": 0.002, "frame": { "project": { "along": "plane", "plane": { "normal": [0, 0, 1] } } } });
    let (_, r) = super::prepare(&[spec], None, &ctx).unwrap().remove(0);
    let pts = &r.dabs[0].points;
    assert!(pts.len() > 10 && pts.iter().all(|p| (p.y - 0.02).abs() < 0.002), "on the near side: {pts:?}");
}

/// A projected spray casts every jittered dab onto the clay too, not only
/// the path's centers.
#[test]
fn projected_spray_dabs_land() {
    let ball = clay(json!([{ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 }]));
    let mesh = ball.build_mesh();
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let cast = |o: Vec3, d: Vec3| caster(&mesh, &bvh, o, d);
    let ctx = ResolveCtx { cast: Some(&cast), ..ResolveCtx::local() };
    let spec = json!({ "brush": "inflate", "points": [[0.0, 0.32, 0.0], [0.1, 0.31, 0.0]], "kind": { "spray": { "count": 8, "jitter": 2, "seed": 3 } }, "radius": 0.06, "frame": { "project": { "along": "view:top" } } });
    let (_, r) = super::prepare(&[spec], None, &ctx).unwrap().remove(0);
    assert!(r.dabs.len() >= 8, "{} dabs", r.dabs.len());
    let worst = r.dabs.iter().map(|d| (d.points[0].length() - 0.3).abs()).fold(0.0f32, f32::max);
    assert!(worst < 0.5 * CELL, "a dab {worst} off the ball");
}

/// The flow is the same at any cell: the field and the cell scaled
/// together give the same result scaled, down to the finest detail cell
/// (an absolute gradient cutoff stopped it there).
#[test]
fn polish_flows_at_the_finest_cell() {
    let n = IVec3::splat(40);
    let len = (n.x * n.y * n.z) as usize;
    let field = |voxel: f32| -> Vec<f32> {
        (0..len)
            .map(|i| {
                let p = Vec3::new((i % 40) as f32, (i / 40 % 40) as f32, (i / 1600) as f32);
                let lump = 0.3 * value_noise(p / 2.0);
                ((p - Vec3::splat(19.5)).length() - 14.0 + lump) * voxel
            })
            .collect()
    };
    let s = super::flow::Settings { angle: 30.0, keep_volume: true, passes: 8 };
    let ones = vec![1.0f32; len];
    let moved = |t: &[f32], out: &[f32], voxel: f32| t.iter().zip(out).filter(|(a, b)| (*a - *b).abs() > 1e-4 * voxel).count();
    let unit = field(1.0);
    let at_unit = super::flow::polish_box(&unit, n, &ones, 1.0, &s);
    let v = crate::clay::MIN_DETAIL_CELL;
    let fine = field(v);
    let at_fine = super::flow::polish_box(&fine, n, &ones, v, &s);
    let (a, b) = (moved(&unit, &at_unit, 1.0), moved(&fine, &at_fine, v));
    eprintln!("points moved: {a} at cell 1, {b} at cell {v}");
    assert!(a > 1000 && b as f32 >= 0.95 * a as f32, "{a} at cell 1, {b} at cell {v}");
    let worst = at_unit.iter().zip(&at_fine).map(|(u, f)| (u - f / v).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-3, "scaled results differ by {worst} cells");
}

/// A region larger than the clay is clipped to it: it polishes as the
/// whole clay does, rather than being refused as too big.
#[test]
fn a_region_larger_than_the_clay_polishes_it_all() {
    let c = lumpy_ball();
    let huge = polish(&c, Polish { region: Some(Region { center: Vec3::ZERO, radii: Vec3::splat(100.0) }), ..whole(0.2, None) });
    let all = polish(&c, whole(0.2, None));
    assert_eq!(Detail::hash(huge.detail.as_ref()), Detail::hash(all.detail.as_ref()));
    // One beside the clay works on nothing.
    let beside = polish(&c, Polish { region: Some(Region { center: Vec3::new(5.0, 0.0, 0.0), radii: Vec3::splat(0.5) }), ..whole(0.2, None) });
    assert_eq!(Detail::hash(beside.detail.as_ref()), Detail::hash(c.detail.as_ref()));
}

/// A region polish holds masked points (the persistent mask, frozen in
/// its record) still, as strokes do, and keeps the mask it read alive.
#[test]
fn polish_honours_the_mask() {
    let mut c = lumpy_ball();
    let voxel = crate::mask::voxel_of(&c).unwrap();
    let band = crate::band::Band::of(&c, voxel).unwrap();
    let values: Vec<u8> = band.keys.iter().map(|k| if k[0] as f32 * voxel > 0.0 { 255 } else { 0 }).collect();
    let mask = crate::mask::write(crate::mask::SculptMask::new(voxel), &band, &values).unwrap();
    c.sculpt_mask = Some(mask.clone());
    let frozen = crate::ledger::freeze_with(&mut c, None).unwrap();
    assert!(frozen.is_some_and(|f| f.mask.is_some()));
    let p = Polish { frozen, ..whole(0.4, None) };
    let meta = crate::ledger::Meta::agent("edit_clay", None);
    let done = crate::ledger::commit_action::<Detail>(&c, &crate::ledger::ledger(&c), &meta, json!({ "polish": {} }), SculptAction::Polish(p), None).unwrap();
    // (Against the grid it ran on: the lumps' unrecorded grid is synced
    // into the ledger first, quantized.)
    let ran_on = done.before.clone();
    let mut after = c.clone();
    crate::ledger::install(&mut after, done.ledger, done.grid);
    let (mut masked, mut free) = (0, 0);
    crate::ledger::detail_changes(ran_on.as_ref(), after.detail.as_ref(), |q, _, _, _| {
        // (Off the surface the gate reads the mask where the surface is:
        // near the mask's edge that can be partly masked.)
        if mask.grid().value(q) >= 1.0 && q.x > 3.0 * CELL {
            masked += 1;
        } else if q.x < -2.0 * CELL {
            free += 1;
        }
    });
    assert!(masked == 0 && free > 1000, "{masked} masked points moved, {free} free ones");
    // The record names the mask, so the clay keeps it for replays.
    let h = frozen.unwrap().mask.unwrap();
    assert!(crate::ledger::frozen_in_use(&after).contains(&h));
    let l = crate::ledger::ledger::<Detail>(&after);
    let again = crate::ledger::replay(&l, &after, l.len(), None, |_, _| {}).unwrap();
    assert_eq!(Detail::hash(again.as_ref()), Detail::hash(after.detail.as_ref()));
}

/// The working memory at the band cap stays under 150 MB: the bound with
/// every brick the read cap allows holding band points, and the cap's band
/// points all changed; and what a polish reports is that formula.
#[test]
fn polish_memory_fits_the_cap() {
    use super::flow::{MAX_BAND, working_bytes};
    let read = 5 * MAX_BAND / 512;
    let worst = working_bytes(read, read, MAX_BAND, MAX_BAND);
    eprintln!("worst case at the cap ({MAX_BAND} points, {read} bricks read): {:.1} MB", worst as f64 / 1e6);
    assert!(worst as f64 <= 150e6, "{:.0} MB at the cap", worst as f64 / 1e6);
    let c = lumpy_ball();
    let (diff, w) = super::flow::polish(&c, c.detail.as_ref(), &whole(0.1, None)).unwrap();
    eprintln!("lumpy ball: {} band points, {:.2} MB ({:.1} bytes a point)", w.points, w.bytes as f64 / 1e6, w.bytes as f64 / w.points as f64);
    assert!(w.points > 10_000 && w.bytes > w.points * 14 && diff.changed_points > 0, "{w:?}");
}

/// The synthetic face of Stage 1c's exit: an ellipsoid head with a lip
/// groove and two lid grooves (crease strokes in its ledger, 2 cells deep)
/// and lumps 0.3 cells high over it (faded out within a few cells of the
/// grooves, as on the box).
fn lumpy_face() -> (Clay, Vec<[Vec3; 3]>) {
    let mut c = clay(json!([{ "type": "ellipsoid", "center": [0, 0, 0], "radii": [0.25, 0.32, 0.27] }]));
    let on = |x: f32, y: f32| {
        let p = Probe::new(&c);
        let z = 0.27 * (1.0 - (x / 0.25).powi(2) - (y / 0.32).powi(2)).max(0.0).sqrt();
        let d = Vec3::new(x, y, z).normalize();
        p.cross(d * 0.4, d * 0.05)
    };
    // Each groove: its points, and its middle with the points 6 cells
    // above and below it (where its depth is read).
    let lip: Vec<Vec3> = (0..=8).map(|i| on(-0.09 + 0.0225 * i as f32, -0.12 + 0.03 * (1.0 - ((i as f32 - 4.0) / 4.0).powi(2)))).collect();
    let lids: Vec<Vec<Vec3>> = [-1.0f32, 1.0].iter().map(|s| (0..=4).map(|i| on(s * (0.05 + 0.015 * i as f32), 0.09 + 0.012 * (1.0 - ((i as f32 - 2.0) / 2.0).powi(2)))).collect()).collect();
    let mut grooves = Vec::new();
    let mut ledger_strokes = Vec::new();
    for g in std::iter::once(&lip).chain(&lids) {
        let mid = g[g.len() / 2];
        grooves.push([mid, on(mid.x, mid.y + 6.0 * CELL), on(mid.x, mid.y - 6.0 * CELL)]);
        ledger_strokes.push(json!({ "brush": "crease", "points": g.iter().map(|p| p.to_array()).collect::<Vec<_>>(), "radius": 4.0 * CELL, "strength": 2.0 * CELL }));
    }
    for s in ledger_strokes {
        let (spec, r) = super::prepare(&[s], None, &ResolveCtx::local()).unwrap().remove(0);
        let done = crate::ledger::commit::<Detail>(&c, &crate::ledger::ledger(&c), &crate::ledger::Meta::agent("sculpt", None), &spec, &r, false, None).unwrap();
        crate::ledger::install(&mut c, done.ledger, done.grid);
    }
    let paths: Vec<Vec<Vec3>> = std::iter::once(lip).chain(lids).collect();
    let fade = |d: f32| super::noise::smooth01((d / CELL - 4.0) / 4.0);
    lumps(&mut c, 0.3 * CELL, 2.0 * CELL, &move |p: Vec3| {
        let d = paths.iter().flat_map(|g| g.windows(2)).map(|w| {
            let ab = w[1] - w[0];
            p.distance(w[0] + ab * ((p - w[0]).dot(ab) / ab.length_squared()).clamp(0.0, 1.0))
        }).fold(f32::MAX, f32::min);
        fade(d)
    });
    (c, grooves)
}

/// A groove's depth (cells): how far its middle lies under the surface 6
/// cells either side, along the head's radial line.
fn groove_depth(c: &Clay, g: &[Vec3; 3]) -> f32 {
    let p = Probe::new(c);
    let r = |q: Vec3| { let d = q.normalize(); p.cross(d * (q.length() + 4.0 * CELL), d * (q.length() - 6.0 * CELL)).length() };
    ((r(g[1]) + r(g[2])) * 0.5 - r(g[0])) / CELL
}

/// Stage 1c's exit, with Stage 2's measure: one polish call takes the
/// face's lumpiness to at most 0.05 and keeps at least 85% of each
/// groove's depth (the grooves are crease strokes: intended detail). Ten
/// passes do it (amount 0.08): the measure reads lumps up to about 4 cells
/// long, which flow away first. So does the default amount (0.1, 13
/// passes; D-b). The old default (0.5, 64 passes) also rounds these short
/// grooves on the curved head (recorded, not asserted): the guard holds
/// their bottoms but not their flanks.
#[test]
fn polish_gives_a_clean_face() {
    let (c, grooves) = lumpy_face();
    let score = |c: &Clay| {
        let mesh = c.build_mesh();
        let (_, points, _) = crate::quality::sample_clay(c, &mesh, 20_000).unwrap();
        crate::quality::lumpiness(&points, &[])
    };
    let before = score(&c);
    let run = |amount: f32| {
        let meta = crate::ledger::Meta::agent("edit_clay", None);
        let done = crate::ledger::commit_action::<Detail>(&c, &crate::ledger::ledger(&c), &meta, json!({ "polish": { "amount": amount } }), SculptAction::Polish(whole(amount, None)), None).unwrap();
        let mut after = c.clone();
        crate::ledger::install(&mut after, done.ledger, done.grid);
        let kept: Vec<f32> = grooves.iter().map(|g| groove_depth(&after, g) / groove_depth(&c, g)).collect();
        (score(&after), kept)
    };
    let (polished, kept) = run(0.08);
    let (default, default_kept) = run(crate::sculpt::flow::DEFAULT_AMOUNT);
    let (old, old_kept) = run(0.5);
    let depths: Vec<f32> = grooves.iter().map(|g| groove_depth(&c, g)).collect();
    eprintln!("face: lumpiness {:.4} ({} intended points of {}), grooves {depths:?} cells deep; polish amount 0.08: {:.4}, depth kept {kept:?}; default {}: {:.4}, kept {default_kept:?}; amount 0.5: {:.4}, kept {old_kept:?}", before.overall.unwrap(), before.intended, before.counted + before.intended, polished.overall.unwrap(), crate::sculpt::flow::DEFAULT_AMOUNT, default.overall.unwrap(), old.overall.unwrap());
    assert!(before.overall.unwrap() > 0.05, "the face starts lumpy: {before:?}");
    assert!(depths.iter().all(|d| *d >= 1.5), "{depths:?}");
    for (score, kept) in [(polished, kept), (default, default_kept)] {
        assert!(score.overall.unwrap() <= 0.05, "{score:?}");
        assert!(kept.iter().all(|k| *k >= 0.85), "{kept:?}");
    }
}

