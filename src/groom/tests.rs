//! Stage 4c's kernel tests: each brush against a measurable answer, the
//! collider, the X mirror, thread counts and bounds (docs/QUALITY_PLAN.md).

use glam::Vec3;

use super::*;
use crate::brush::curve::{Falloff, Preset};
use crate::hair::{Guide, Hair};

/// A sphere head at the origin.
struct Ball {
    radius: f32,
    lift: f32,
}

impl Collider for Ball {
    fn push(&self, p: Vec3) -> Option<Vec3> {
        let want = self.radius + self.lift;
        (p.length() < want).then(|| p.normalize_or(Vec3::Y) * want)
    }
    fn normal(&self, p: Vec3) -> Option<Vec3> {
        Some(p.normalize_or(Vec3::Y))
    }
    fn inside(&self, p: Vec3) -> bool {
        p.length() < self.radius
    }
    fn step(&self) -> f32 {
        0.002
    }
}

const HEAD: Ball = Ball { radius: 0.1, lift: 0.0015 };

fn flat() -> Falloff {
    Falloff { preset: Preset::Flat, ..Falloff::default() }
}

fn lengths(h: &Hair) -> Vec<f32> {
    h.guides.iter().map(|g| length(&g.points)).collect()
}

/// `count` guides on a ring of the head's upper part (y = 0.08), each a
/// straight line of `segments` leaving the scalp at 60° from -Y.
fn ring(count: usize, segments: usize, len: f32) -> Hair {
    let r0 = HEAD.radius + HEAD.lift;
    let guides = (0..count).map(|i| {
        let a = i as f32 / count as f32 * std::f32::consts::TAU;
        let out = Vec3::new(a.cos(), 0.0, a.sin());
        let root = (out * 0.6 + Vec3::Y * 0.8) * r0;
        let d = (out * 60f32.to_radians().sin() + Vec3::NEG_Y * 60f32.to_radians().cos()).normalize();
        Guide { points: (0..=segments).map(|k| root + d * len * k as f32 / segments as f32).collect(), width: None }
    }).collect();
    Hair { guides, center: Some(Vec3::ZERO), ..Hair::default() }
}

/// A lock: `count` straight guides hanging along -Y around the line
/// x = 0.05, z = 0.15 (outside the head), `spread` from it.
fn lock(count: usize, spread: f32, at_x: f32) -> Hair {
    let guides = (0..count).map(|i| {
        let a = i as f32 / count as f32 * std::f32::consts::TAU;
        let root = Vec3::new(at_x + spread * a.cos(), 0.0, 0.15 + spread * a.sin());
        Guide { points: (0..=12).map(|k| root + Vec3::NEG_Y * 0.02 * k as f32).collect(), width: None }
    }).collect();
    Hair { guides, ..Hair::default() }
}

fn run(h: &mut Hair, s: Stroke, c: Option<&dyn Collider>) -> Report {
    apply(h, &[(s, c)]).expect("groom")
}

fn tip_angle_to(h: &Hair, d: Vec3) -> f32 {
    let sum: f32 = h.guides.iter().map(|g| {
        let n = g.points.len();
        (g.points[n - 1] - g.points[n - 2]).normalize().angle_between(d).to_degrees()
    }).sum();
    sum / h.guides.len() as f32
}

/// The plan's `comb_aligns`: 40 guides combed toward -Y. Tip segments
/// turn from 60° to within 10° of -Y and every guide keeps its length
/// within 1%, with and without the head.
#[test]
fn comb_aligns() {
    for collide in [false, true] {
        let mut h = ring(40, 12, 0.3);
        let before = tip_angle_to(&h, Vec3::NEG_Y);
        let l0 = lengths(&h);
        let mut s = Stroke::new(Kind::Comb, vec![Vec3::ZERO], 1.0);
        s.direction = Some(Vec3::NEG_Y);
        s.falloff = flat();
        let c: Option<&dyn Collider> = if collide { Some(&HEAD) } else { None };
        let r = run(&mut h, s, c);
        let after = tip_angle_to(&h, Vec3::NEG_Y);
        let worst = lengths(&h).iter().zip(&l0).map(|(a, b)| (a / b - 1.0).abs()).fold(0.0f32, f32::max);
        eprintln!("comb (collide {collide}): tip angle {before:.2}° -> {after:.3}°, worst length change {:.4}%, {} guides, {} pushes", worst * 100.0, r.guides_changed, r.collisions_resolved);
        assert!((before - 60.0).abs() < 0.01);
        assert!(after <= 10.0, "{after}");
        assert!(worst <= 0.01, "{worst}");
        assert_eq!(r.guides_changed, 40);
        if collide {
            assert!(h.guides.iter().flat_map(|g| g.points.iter().skip(1)).all(|p| p.length() >= HEAD.radius + HEAD.lift * 0.5));
        } else {
            assert!(after < 1e-3, "a full comb with no head lines the tips up: {after}");
        }
    }
}

/// Comb along a path with no direction combs the way the path runs.
#[test]
fn comb_follows_the_path() {
    let mut h = lock(8, 0.01, 0.05);
    let s = Stroke { falloff: flat(), affect: Affect::All, ..Stroke::new(Kind::Comb, vec![Vec3::new(0.05, -0.1, 0.0), Vec3::new(0.05, -0.1, 0.3)], 0.2) };
    run(&mut h, s, None);
    let a = tip_angle_to(&h, Vec3::Z);
    assert!(a < 1e-3, "{a}");
}

/// Lengthen and shorten change lengths by the requested amount; cut
/// keeps at most the length.
#[test]
fn lengthen_shorten_and_cut_by_the_amount() {
    let mut h = ring(16, 8, 0.2);
    let l0 = lengths(&h);
    let reach = |kind| Stroke { falloff: flat(), length: Some(0.05), ..Stroke::new(kind, vec![Vec3::ZERO], 1.0) };
    run(&mut h, reach(Kind::Lengthen), Some(&HEAD));
    let l1 = lengths(&h);
    let worst = l1.iter().zip(&l0).map(|(a, b)| (a - b - 0.05).abs()).fold(0.0f32, f32::max);
    eprintln!("lengthen 0.05: worst error {worst:e}; points {} -> {}", 9, h.guides[0].points.len());
    assert!(worst < 1e-5, "{worst}");
    // The new points continue at the guide's own spacing.
    assert_eq!(h.guides[0].points.len(), 9 + 2);
    run(&mut h, reach(Kind::Shorten), Some(&HEAD));
    let worst = lengths(&h).iter().zip(&l0).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("shorten 0.05 back: worst error {worst:e}");
    assert!(worst < 1e-5, "{worst}");
    // Half strength: half the amount.
    let mut s = reach(Kind::Lengthen);
    s.strength = 0.5;
    run(&mut h, s, None);
    let worst = lengths(&h).iter().zip(&l0).map(|(a, b)| (a - b - 0.025).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-5, "{worst}");
    let mut s = reach(Kind::Cut);
    s.length = Some(0.12);
    run(&mut h, s, None);
    for l in lengths(&h) {
        assert!((l - 0.12).abs() < 1e-5, "{l}");
    }
    // Shorten never takes a guide away.
    let mut s = reach(Kind::Shorten);
    s.length = Some(10.0);
    run(&mut h, s, None);
    assert!(h.guides.iter().all(|g| g.points.len() >= 2 && length(&g.points) > 0.0));
}

/// The plan's `cut_plane`: nothing past the plane, roots untouched; a
/// guide whose root is past it is left alone and counted.
#[test]
fn cut_plane() {
    let mut h = ring(24, 12, 0.3);
    h.guides.push(Guide { points: vec![Vec3::new(0.0, -0.2, 0.3), Vec3::new(0.0, -0.3, 0.3)], width: None });
    let roots: Vec<Vec3> = h.guides.iter().map(|g| g.points[0]).collect();
    let (c, n) = (Vec3::new(0.0, -0.05, 0.0), Vec3::NEG_Y);
    let s = Stroke { plane: Some((c, n)), ..Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0) };
    let r = run(&mut h, s, Some(&HEAD));
    let worst = h.guides.iter().take(24).flat_map(|g| g.points.iter()).map(|p| (*p - c).dot(n)).fold(f32::MIN, f32::max);
    eprintln!("cut_plane: farthest point past the plane {worst:e}, {} guides cut, {} roots past it", r.guides_changed, r.roots_beyond_plane);
    assert!(worst <= 1e-5, "{worst}");
    assert_eq!(r.roots_beyond_plane, 1);
    assert_eq!(r.guides_changed, 24);
    assert!(h.guides.iter().zip(&roots).all(|(g, r)| g.points[0] == *r));
}

fn tip_spread(h: &Hair) -> f32 {
    let tips: Vec<Vec3> = h.guides.iter().map(|g| *g.points.last().unwrap()).collect();
    let mid = tips.iter().copied().sum::<Vec3>() / tips.len() as f32;
    (tips.iter().map(|t| t.distance_squared(mid)).sum::<f32>() / tips.len() as f32).sqrt()
}

/// Clump gathers the tips of the guides it reaches.
#[test]
fn clump_reduces_spread() {
    let mut h = lock(12, 0.02, 0.05);
    // Tips fan out.
    for g in &mut h.guides {
        let root = g.points[0];
        let out = Vec3::new(root.x - 0.05, 0.0, root.z - 0.15).normalize();
        for (k, p) in g.points.iter_mut().enumerate() {
            *p += out * 0.004 * k as f32;
        }
    }
    let before = tip_spread(&h);
    let l0 = lengths(&h);
    let s = Stroke { falloff: flat(), strength: 0.8, ..Stroke::new(Kind::Clump, vec![Vec3::new(0.05, -0.12, 0.15)], 0.5) };
    run(&mut h, s, Some(&HEAD));
    let after = tip_spread(&h);
    let worst = lengths(&h).iter().zip(&l0).map(|(a, b)| (a / b - 1.0).abs()).fold(0.0f32, f32::max);
    eprintln!("clump 0.8: tip spread {before:.4} -> {after:.4} ({:.1}%), worst length change {:e}", after / before * 100.0, worst);
    assert!(after < before * 0.5, "{before} {after}");
    assert!(worst < 1e-4);
}

/// The angle of `p` about the line through `o` along `axis`, from `ref_`.
fn winding(p: Vec3, o: Vec3, axis: Vec3, ref_: Vec3) -> f32 {
    let v = p - o;
    let v = v - axis * axis.dot(v);
    let x = ref_.normalize();
    let y = axis.cross(x);
    v.dot(y).atan2(v.dot(x)).to_degrees()
}

/// Twist turns each guide's tip about the lock's axis by the angle.
#[test]
fn twist_turns_by_the_angle() {
    for angle in [90.0f32, -45.0, 30.0] {
        let mut h = lock(10, 0.02, 0.05);
        let axis_o = Vec3::new(0.05, -0.24, 0.15);
        let before: Vec<f32> = h.guides.iter().map(|g| winding(*g.points.last().unwrap(), axis_o, Vec3::NEG_Y, Vec3::X)).collect();
        let s = Stroke { falloff: flat(), angle: angle.to_radians(), ..Stroke::new(Kind::Twist, vec![Vec3::new(0.05, -0.12, 0.15)], 0.5) };
        run(&mut h, s, None);
        let worst = h.guides.iter().zip(&before).map(|(g, b)| {
            let a = winding(*g.points.last().unwrap(), axis_o, Vec3::NEG_Y, Vec3::X);
            // The change, in -180..180.
            ((a - b - angle + 540.0).rem_euclid(360.0) - 180.0).abs()
        }).fold(0.0f32, f32::max);
        eprintln!("twist {angle}°: worst tip winding error {worst:.3}°");
        assert!(worst < 1.5, "{angle}: {worst}");
    }
}

fn turning(h: &Hair) -> f32 {
    h.guides.iter().map(|g| g.points.windows(3).map(|w| (w[1] - w[0]).angle_between(w[2] - w[1])).sum::<f32>()).sum()
}

/// Smooth takes kinks out; lengths stay.
#[test]
fn smooth_straightens() {
    let mut h = lock(6, 0.02, 0.05);
    for g in &mut h.guides {
        for (k, p) in g.points.iter_mut().enumerate() {
            p.x += if k % 2 == 1 { 0.006 } else { 0.0 };
        }
    }
    let (before, l0) = (turning(&h), lengths(&h));
    let s = Stroke { falloff: flat(), iterations: 8, ..Stroke::new(Kind::Smooth, vec![Vec3::new(0.05, -0.12, 0.15)], 0.5) };
    run(&mut h, s, None);
    let after = turning(&h);
    let worst = lengths(&h).iter().zip(&l0).map(|(a, b)| (a / b - 1.0).abs()).fold(0.0f32, f32::max);
    eprintln!("smooth: turning {before:.3} -> {after:.3} rad, worst length change {worst:e}");
    assert!(after < before * 0.3);
    assert!(worst < 1e-4);
}

/// Puff lifts the root-side segments off the scalp.
#[test]
fn puff_lifts_roots() {
    let mut h = ring(16, 12, 0.3);
    // Flatten them onto the head first with a comb, so they lie low.
    let mut c = Stroke { direction: Some(Vec3::NEG_Y), falloff: flat(), affect: Affect::All, ..Stroke::new(Kind::Comb, vec![Vec3::ZERO], 1.0) };
    c.strength = 1.0;
    run(&mut h, c, Some(&HEAD));
    let lift = |h: &Hair| h.guides.iter().map(|g| (g.points[2] - g.points[0]).normalize().dot(g.points[0].normalize())).sum::<f32>() / h.guides.len() as f32;
    let before = lift(&h);
    let s = Stroke { falloff: flat(), strength: 0.6, ..Stroke::new(Kind::Puff, vec![Vec3::ZERO], 1.0) };
    run(&mut h, s, Some(&HEAD));
    let after = lift(&h);
    eprintln!("puff: root segments' cosine to the scalp normal {before:.3} -> {after:.3}");
    assert!(after > before + 0.2, "{before} {after}");
}

fn neighbour_spread(h: &Hair) -> f32 {
    // Spread of each tip's distance to its nearest other tip.
    let tips: Vec<Vec3> = h.guides.iter().map(|g| *g.points.last().unwrap()).collect();
    let d: Vec<f32> = tips.iter().enumerate().map(|(i, a)| tips.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, b)| a.distance(*b)).fold(f32::MAX, f32::min)).collect();
    let m = d.iter().sum::<f32>() / d.len() as f32;
    (d.iter().map(|x| (x - m).powi(2)).sum::<f32>() / d.len() as f32).sqrt() / m
}

/// Relax evens out the spacing between neighbouring guides.
#[test]
fn relax_evens_spacing() {
    // A row of evenly spaced roots whose tips bunch up unevenly.
    let guides = (0..10).map(|i| {
        let root = Vec3::new(i as f32 * 0.01, 0.0, 0.2);
        let tip_x = (i as f32 * 0.01).powf(1.6) * 4.0;
        Guide { points: (0..=10).map(|k| { let t = k as f32 / 10.0; Vec3::new(root.x + (tip_x - root.x) * t, -0.2 * t, 0.2) }).collect(), width: None }
    }).collect();
    let mut h = Hair { guides, ..Hair::default() };
    let before = neighbour_spread(&h);
    let s = Stroke { falloff: flat(), iterations: 16, ..Stroke::new(Kind::Relax, vec![Vec3::new(0.05, -0.1, 0.2)], 1.0) };
    run(&mut h, s, None);
    let after = neighbour_spread(&h);
    eprintln!("relax: tip neighbour spacing variation {before:.3} -> {after:.3}");
    assert!(after < before * 0.5, "{before} {after}");
}

/// The plan's `collide`: after every brush, run in turn into the head, no
/// guide point is inside it (each is at least about the lift out).
#[test]
fn collide_after_every_brush() {
    let mut h = ring(24, 12, 0.3);
    let mut strokes = Vec::new();
    // Comb straight into the head, then everything else.
    strokes.push(Stroke { direction: Some(Vec3::new(0.0, -1.0, 0.0)), falloff: flat(), affect: Affect::All, ..Stroke::new(Kind::Comb, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { direction: Some(Vec3::new(-1.0, -0.2, 0.0)), falloff: flat(), affect: Affect::All, ..Stroke::new(Kind::Comb, vec![Vec3::new(0.1, 0.0, 0.0)], 0.3) });
    strokes.push(Stroke { length: Some(0.05), ..Stroke::new(Kind::Lengthen, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { falloff: flat(), ..Stroke::new(Kind::Clump, vec![Vec3::new(0.0, -0.1, 0.1)], 0.2) });
    strokes.push(Stroke { angle: 2.0, ..Stroke::new(Kind::Twist, vec![Vec3::new(0.0, -0.1, -0.1)], 0.2) });
    strokes.push(Stroke { iterations: 6, ..Stroke::new(Kind::Smooth, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { strength: 0.5, ..Stroke::new(Kind::Puff, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { iterations: 6, ..Stroke::new(Kind::Relax, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { length: Some(0.03), ..Stroke::new(Kind::Shorten, vec![Vec3::ZERO], 1.0) });
    strokes.push(Stroke { length: Some(0.2), ..Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0) });
    let mut pushes = 0;
    for s in strokes {
        let kind = s.kind;
        let r = run(&mut h, s, Some(&HEAD));
        pushes += r.collisions_resolved;
        let closest = h.guides.iter().flat_map(|g| g.points.iter().skip(1)).map(|p| p.length()).fold(f32::MAX, f32::min);
        eprintln!("{kind:?}: closest point {:.6} (surface {}), {} pushed, {} inside", closest, HEAD.radius, r.collisions_resolved, r.still_inside);
        assert!(closest >= HEAD.radius + HEAD.lift * 0.5, "{kind:?}: {closest}");
        assert_eq!(r.still_inside, 0);
    }
    assert!(pushes > 0, "the comb into the head needed the collider");
}

fn mirrored(a: &[Vec3], b: &[Vec3]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(p, q)| p.distance(reflect(*q))).fold(0.0f32, f32::max)
}

/// The plan's `mirror`: a stroke on +X with mirror gives mirrored guides
/// on the two sides, for every brush.
#[test]
fn mirror() {
    let base = {
        // Wavy, so smooth has something to do.
        let wavy = || {
            let mut h = lock(8, 0.02, 0.06);
            for (i, g) in h.guides.iter_mut().enumerate() {
                for (k, p) in g.points.iter_mut().enumerate() {
                    p.z += if k % 2 == 1 { 0.004 } else { 0.0 };
                    p.x += 0.002 * k as f32 * (i % 3) as f32;
                }
            }
            h
        };
        let mut h = wavy();
        let other = wavy();
        h.guides.extend(other.guides.iter().map(|g| Guide { points: g.points.iter().map(|p| reflect(*p)).collect(), width: None }));
        h
    };
    let at = vec![Vec3::new(0.06, -0.1, 0.15), Vec3::new(0.08, -0.2, 0.17)];
    let kinds = [
        Stroke { direction: Some(Vec3::new(1.0, -0.3, 0.4)), ..Stroke::new(Kind::Comb, at.clone(), 0.08) },
        Stroke::new(Kind::Comb, at.clone(), 0.08),
        Stroke { length: Some(0.04), ..Stroke::new(Kind::Lengthen, at.clone(), 0.08) },
        Stroke { length: Some(0.04), ..Stroke::new(Kind::Shorten, at.clone(), 0.08) },
        Stroke { plane: Some((Vec3::new(0.06, -0.15, 0.0), Vec3::new(0.3, -1.0, 0.0))), ..Stroke::new(Kind::Cut, at.clone(), 0.08) },
        Stroke { strength: 0.7, ..Stroke::new(Kind::Clump, at.clone(), 0.08) },
        Stroke { angle: 1.2, ..Stroke::new(Kind::Twist, at.clone(), 0.08) },
        Stroke::new(Kind::Smooth, at.clone(), 0.08),
        Stroke::new(Kind::Puff, at.clone(), 0.08),
        Stroke::new(Kind::Relax, at.clone(), 0.08),
    ];
    for s in kinds {
        let kind = s.kind;
        let mut h = base.clone();
        run(&mut h, Stroke { mirror: true, ..s }, Some(&HEAD));
        assert_ne!(h.guides, base.guides, "{kind:?} changed something");
        let worst = (0..8).map(|i| mirrored(&h.guides[i].points, &h.guides[i + 8].points)).fold(0.0f32, f32::max);
        eprintln!("mirror {kind:?}: worst {worst:e}");
        assert!(worst <= 1e-5, "{kind:?}: {worst}");
    }
}

/// The hair's own mirror flag: the stored half answers a stroke on the
/// other side, as the mirrored guides it draws.
#[test]
fn mirror_reaches_the_stored_half() {
    let base = lock(8, 0.02, 0.06);
    let mut h = base.clone();
    let mut s = Stroke { direction: Some(Vec3::NEG_Z), falloff: flat(), ..Stroke::new(Kind::Comb, vec![Vec3::new(-0.06, -0.12, 0.15)], 0.05) };
    run(&mut h, s.clone(), None);
    assert_eq!(h.guides, base.guides, "the stored half is on +X, out of reach");
    s.mirror = true;
    run(&mut h, s, None);
    assert_ne!(h.guides, base.guides);
}

/// Every brush gives the same guides, bit for bit, on 1 and 12 threads.
#[test]
fn same_on_1_and_12_threads() {
    let strokes = || vec![
        Stroke { direction: Some(Vec3::new(0.2, -1.0, 0.1)), mirror: true, ..Stroke::new(Kind::Comb, vec![Vec3::ZERO, Vec3::new(0.1, -0.1, 0.1)], 0.3) },
        Stroke { length: Some(0.02), ..Stroke::new(Kind::Lengthen, vec![Vec3::ZERO], 1.0) },
        Stroke { mirror: true, ..Stroke::new(Kind::Clump, vec![Vec3::new(0.1, -0.1, 0.0)], 0.2) },
        Stroke { angle: 1.0, ..Stroke::new(Kind::Twist, vec![Vec3::new(0.0, -0.1, 0.1)], 0.2) },
        Stroke::new(Kind::Smooth, vec![Vec3::ZERO], 1.0),
        Stroke::new(Kind::Puff, vec![Vec3::ZERO], 1.0),
        Stroke { iterations: 5, ..Stroke::new(Kind::Relax, vec![Vec3::ZERO], 1.0) },
        Stroke { length: Some(0.01), ..Stroke::new(Kind::Shorten, vec![Vec3::ZERO], 1.0) },
        Stroke { plane: Some((Vec3::new(0.0, -0.2, 0.0), Vec3::NEG_Y)), ..Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0) },
    ];
    let go = |n: usize| crate::clay::with_threads(n, || {
        let mut h = ring(300, 24, 0.3);
        let list: Vec<(Stroke, Option<&dyn Collider>)> = strokes().into_iter().map(|s| (s, Some(&HEAD as &dyn Collider))).collect();
        let r = apply(&mut h, &list).expect("groom");
        (h, r)
    });
    let (a, ra) = go(1);
    let (b, rb) = go(12);
    let bits = |h: &Hair| h.guides.iter().flat_map(|g| g.points.iter().flat_map(|p| p.to_array().map(f32::to_bits))).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(ra, rb);
    eprintln!("1 and 12 threads: {} guides changed, {} pushes, identical", ra.guides_changed, ra.collisions_resolved);
}

/// The plan's `groom_bounds`: 65 strokes or 257 points are refused, and
/// so is anything not finite, before any guide changes.
#[test]
fn groom_bounds() {
    let base = ring(4, 4, 0.1);
    let one = || Stroke { direction: Some(Vec3::NEG_Y), ..Stroke::new(Kind::Comb, vec![Vec3::ZERO], 1.0) };
    let mut h = base.clone();
    let many: Vec<(Stroke, Option<&dyn Collider>)> = (0..65).map(|_| (one(), None)).collect();
    assert!(apply(&mut h, &many).unwrap_err().contains("64"));
    let mut long = one();
    long.points = vec![Vec3::ZERO; 257];
    assert!(apply(&mut h, &[(long, None)]).unwrap_err().contains("256"));
    let bad = [
        Stroke { radius: f32::NAN, ..one() },
        Stroke { radius: 0.0, ..one() },
        Stroke { strength: 1.5, ..one() },
        Stroke { points: vec![Vec3::splat(f32::INFINITY)], ..one() },
        Stroke { direction: Some(Vec3::ZERO), ..one() },
        Stroke { direction: None, ..one() },
        Stroke::new(Kind::Lengthen, vec![Vec3::ZERO], 1.0),
        Stroke { length: Some(-1.0), ..Stroke::new(Kind::Shorten, vec![Vec3::ZERO], 1.0) },
        Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0),
        Stroke { length: Some(0.1), plane: Some((Vec3::ZERO, Vec3::Y)), ..Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0) },
        Stroke { plane: Some((Vec3::ZERO, Vec3::ZERO)), ..Stroke::new(Kind::Cut, vec![Vec3::ZERO], 1.0) },
        Stroke { angle: 100.0, ..Stroke::new(Kind::Twist, vec![Vec3::ZERO], 1.0) },
        Stroke { iterations: 0, ..Stroke::new(Kind::Smooth, vec![Vec3::ZERO], 1.0) },
        Stroke { iterations: 33, ..Stroke::new(Kind::Relax, vec![Vec3::ZERO], 1.0) },
    ];
    for s in bad {
        let kind = s.kind;
        // The good stroke first: nothing applies when a later one is bad.
        assert!(apply(&mut h, &[(one(), None), (s, None)]).is_err(), "{kind:?}");
        assert_eq!(h, base);
    }
    let mut big = base.clone();
    big.guides = vec![big.guides[0].clone(); crate::hair::MAX_GUIDES + 1].into();
    assert!(apply(&mut big, &[(one(), None)]).unwrap_err().contains("guides"));
    let mut big = base.clone();
    big.guides[0].points = vec![Vec3::ZERO; crate::hair::MAX_POINTS + 1];
    assert!(apply(&mut big, &[(one(), None)]).unwrap_err().contains("points"));
}

/// Lengthen stops adding points at the per-guide bound and stretches the
/// last segment instead.
#[test]
fn lengthen_keeps_the_point_bound() {
    let mut h = Hair { guides: vec![Guide { points: (0..crate::hair::MAX_POINTS).map(|k| Vec3::new(0.0, -0.001 * k as f32, 0.3)).collect(), width: None }].into(), ..Hair::default() };
    let l0 = lengths(&h)[0];
    let r = run(&mut h, Stroke { falloff: flat(), length: Some(0.1), ..Stroke::new(Kind::Lengthen, vec![Vec3::new(0.0, -0.2, 0.3)], 1.0) }, None);
    assert_eq!(h.guides[0].points.len(), crate::hair::MAX_POINTS);
    assert_eq!(r.points_capped, 1);
    assert!((lengths(&h)[0] - l0 - 0.1).abs() < 1e-4);
}

/// Strokes far from every guide change nothing.
#[test]
fn far_strokes_change_nothing() {
    let base = ring(8, 8, 0.2);
    let mut h = base.clone();
    let r = run(&mut h, Stroke { direction: Some(Vec3::X), ..Stroke::new(Kind::Comb, vec![Vec3::splat(5.0)], 0.1) }, Some(&HEAD));
    assert_eq!(h, base);
    assert_eq!(r, Report::default());
}

/// The plan's performance bound: one stroke over 4,000 guides of 32
/// points within 20 ms (40 ms hard); CLIDE_PERF=1 holds it.
#[test]
#[ignore]
fn groom_perf() {
    let guides: Vec<Guide> = (0..4000).map(|i| {
        let a = i as f32 * 2.399963;
        let y = 1.0 - (i as f32 + 0.5) / 4000.0;
        let r = (1.0 - y * y).sqrt();
        let root = Vec3::new(r * a.cos(), y, r * a.sin()) * (HEAD.radius + HEAD.lift);
        let out = root.normalize();
        Guide { points: (0..32).map(|k| root + (out * 0.3 + Vec3::NEG_Y) .normalize() * 0.01 * k as f32).collect(), width: None }
    }).collect();
    let base = Hair { guides: guides.into(), ..Hair::default() };
    let path: Vec<Vec3> = (0..32).map(|k| Vec3::new(-0.15 + 0.01 * k as f32, 0.05, 0.12)).collect();
    for (kind, c) in [(Kind::Comb, None), (Kind::Comb, Some(&HEAD as &dyn Collider)), (Kind::Clump, Some(&HEAD as &dyn Collider)), (Kind::Relax, Some(&HEAD as &dyn Collider))] {
        let mut times = Vec::new();
        let mut changed = 0;
        for _ in 0..9 {
            let mut h = base.clone();
            let s = Stroke { direction: Some(Vec3::NEG_Y), ..Stroke::new(kind, path.clone(), 0.12) };
            let t = std::time::Instant::now();
            changed = apply(&mut h, &[(s, c)]).unwrap().guides_changed;
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        eprintln!("groom {kind:?} (collider {}): median {:.2} ms over 4,000 guides x 32 points, {changed} changed", c.is_some(), times[4]);
        if std::env::var("CLIDE_PERF").is_ok() && kind == Kind::Comb && c.is_none() {
            assert!(times[4] <= 20.0, "{}", times[4]);
        }
    }
    // The biggest hair, one long mirrored comb with the head: about how
    // long `MAX_WORK`'s units take (printed).
    let guides: Vec<Guide> = (0..crate::hair::MAX_GUIDES).map(|i| {
        let a = i as f32 * 2.399963;
        let y = 1.0 - (i as f32 + 0.5) / crate::hair::MAX_GUIDES as f32;
        let r = (1.0 - y * y).sqrt();
        let root = Vec3::new(r * a.cos(), y, r * a.sin()) * (HEAD.radius + HEAD.lift);
        let out = root.normalize();
        Guide { points: (0..crate::hair::MAX_POINTS).map(|k| root + (out * 0.3 + Vec3::NEG_Y).normalize() * 0.0006 * k as f32).collect(), width: None }
    }).collect();
    let mut big = Hair { guides: guides.into(), ..Hair::default() };
    let long: Vec<Vec3> = (0..MAX_PATH).map(|k| Vec3::new(-0.15 + 0.3 * k as f32 / MAX_PATH as f32, 0.05, 0.12)).collect();
    let s = Stroke { mirror: true, direction: Some(Vec3::NEG_Y), ..Stroke::new(Kind::Comb, long, 0.12) };
    let list = [(s, Some(&HEAD as &dyn Collider))];
    let units = work(&big, &list);
    let t = std::time::Instant::now();
    let r = apply(&mut big, &list).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("biggest hair, one 256-point mirrored comb with the head: {units:.2e} units, {ms:.0} ms ({:.2} ns a unit), {} changed", ms * 1e6 / units as f64, r.guides_changed);
}


/// A damaged hair (from a file: not-finite points, empty and one-point
/// guides, far and huge coordinates, more guides and points than the
/// bounds) loads bounded, and every brush runs over it without a panic,
/// leaving only finite points.
#[test]
fn damaged_hair_loads_bounded_and_grooms() {
    let mut text = serde_json::json!({ "guides": [
        { "points": [[0.0, 0.0, 0.2], ["nan", 0, 0], [0.0, -0.1, 0.2]] },
        { "points": [] },
        { "points": [[0.0, 0.0, 0.2]] },
        { "points": [[1e30, 0.0, 0.0], [-1e30, 1e30, 0.0], [3e38, 3e38, 3e38]] },
        { "points": [[0.01, 0.0, 0.2], [0.01, 0.0, 0.2], [0.01, 0.0, 0.2]] },
    ] });
    text["guides"][0]["points"][1] = serde_json::json!([f32::MAX, 0.0, 0.0]);
    let mut h: Hair = serde_json::from_value(text).unwrap();
    h.guides.push(Guide { points: vec![Vec3::new(f32::NAN, 0.0, 0.0), Vec3::new(0.0, f32::INFINITY, 0.0), Vec3::new(0.02, 0.0, 0.2), Vec3::new(0.02, -0.1, 0.2)], width: None });
    h.guides.extend((0..crate::hair::MAX_GUIDES).map(|_| Guide { points: vec![Vec3::new(0.03, 0.0, 0.2); crate::hair::MAX_POINTS + 50], width: None }));
    h.sanitize();
    assert_eq!(h.guides.len(), crate::hair::MAX_GUIDES);
    assert!(h.guides.iter().all(|g| g.points.len() <= crate::hair::MAX_POINTS && g.points.iter().all(|p| p.is_finite())));
    let all = |kind| Stroke { length: (kind != Kind::Cut).then_some(0.01), plane: (kind == Kind::Cut).then_some((Vec3::new(0.0, -0.05, 0.0), Vec3::NEG_Y)), direction: Some(Vec3::X), angle: 1.0, ..Stroke::new(kind, vec![Vec3::new(0.0, -0.05, 0.2), Vec3::new(0.05, -0.05, 0.2)], 0.3) };
    for kind in [Kind::Comb, Kind::Lengthen, Kind::Shorten, Kind::Cut, Kind::Clump, Kind::Twist, Kind::Smooth, Kind::Puff, Kind::Relax] {
        let mut g = h.clone();
        g.guides.truncate(200);
        apply(&mut g, &[(Stroke { mirror: true, ..all(kind) }, Some(&HEAD as &dyn Collider))]).expect("runs");
        assert!(g.guides.iter().all(|g| g.points.iter().all(|p| p.is_finite())), "{kind:?}");
    }
}

/// A thin plate (5 mm, like an ear) across y = 0, |x|, |z| < 0.05.
struct Plate;

impl Collider for Plate {
    fn push(&self, p: Vec3) -> Option<Vec3> {
        let want = 0.0025 + 0.0015;
        (p.x.abs() < 0.05 && p.z.abs() < 0.05 && p.y.abs() < want).then(|| Vec3::new(p.x, if p.y < 0.0 { -want } else { want }, p.z))
    }
    fn normal(&self, p: Vec3) -> Option<Vec3> {
        Some(if p.y < 0.0 { Vec3::NEG_Y } else { Vec3::Y })
    }
    fn inside(&self, p: Vec3) -> bool {
        p.x.abs() < 0.05 && p.z.abs() < 0.05 && p.y.abs() < 0.0025
    }
    fn step(&self) -> f32 {
        0.002
    }
}

/// Fine samples of a path inside the collider.
fn crossing(points: &[Vec3], c: &dyn Collider) -> usize {
    points.windows(2).map(|w| (0..=100).filter(|&i| c.inside(w[0].lerp(w[1], i as f32 / 100.0))).count()).sum()
}

/// 19 mm segments whose ends are both outside a 5 mm plate cross it:
/// collision checks the segments too (review finding 6), for groom's
/// settle and for film strands, and bends them on the root's side.
#[test]
fn segments_do_not_cross_thin_parts() {
    for x in [0.0, 0.03] {
        let start: Vec<Vec3> = (0..6).map(|k| Vec3::new(x, 0.01 - 0.019 * k as f32, 0.002 * k as f32)).collect();
        assert!(start.iter().all(|p| !Plate.inside(*p)) && crossing(&start, &Plate) > 0, "the fixture crosses the plate");
        let lengths: Vec<f32> = start.windows(2).map(|w| w[0].distance(w[1])).collect();
        let mut p = start.clone();
        settle(&mut p, &lengths, Some(&Plate));
        assert_eq!(crossing(&p, &Plate), 0, "settle at x {x}: {p:?}");
        assert!(p.iter().all(|q| q.y > 0.0), "kept on the root's side: {p:?}");
        let mut s = start.clone();
        assert!(keep_out(&mut s, &Plate));
        assert_eq!(crossing(&s, &Plate), 0, "keep_out at x {x}: {s:?}");
        assert!(s.iter().all(|q| q.y > 0.0), "strand on the root's side: {s:?}");
        // Lengths kept by the strand's turn.
        for (w, l) in s.windows(2).zip(&lengths) {
            assert!((w[0].distance(w[1]) - l).abs() < 0.005, "{} {l}", w[0].distance(w[1]));
        }
    }
    // A path that never reaches the plate is left alone.
    let far: Vec<Vec3> = (0..6).map(|k| Vec3::new(0.2, 0.01 - 0.019 * k as f32, 0.0)).collect();
    let mut f = far.clone();
    assert!(!keep_out(&mut f, &Plate));
    assert_eq!(f, far);
}

/// A lengthen that needs more points than the guide has room for counts
/// as capped even when some room is left (review finding 7).
#[test]
fn lengthen_counts_a_partly_capped_guide() {
    let n = crate::hair::MAX_POINTS - 2;
    let points: Vec<Vec3> = (0..n).map(|k| Vec3::new(0.3, 0.0, 0.0) + Vec3::NEG_Y * 0.001 * k as f32).collect();
    let mut h = Hair { guides: vec![Guide { points, width: None }].into(), ..Hair::default() };
    let before = lengths(&h)[0];
    let s = Stroke { falloff: flat(), length: Some(0.02), ..Stroke::new(Kind::Lengthen, vec![Vec3::new(0.3, -0.25, 0.0)], 1.0) };
    let r = run(&mut h, s, None);
    assert_eq!(h.guides[0].points.len(), crate::hair::MAX_POINTS);
    assert_eq!(r.points_capped, 1, "20 points asked, 2 fit: {r:?}");
    assert!((lengths(&h)[0] - before - 0.02).abs() < 1e-5);
}

/// still_inside checks each guide against the collider it was kept out
/// of, not the last stroke's (review finding 8): a lock combed with the
/// head as collider, and one that sits inside the head combed with no
/// collider, report nothing still inside.
#[test]
fn still_inside_uses_each_guides_own_collider() {
    let mut h = lock(6, 0.01, 0.5);
    let inner = lock(6, 0.01, 0.0);
    // The second lock starts inside the head: roots at z = 0.15 - 0.1.
    h.guides.extend(inner.guides.iter().map(|g| Guide { points: g.points.iter().map(|p| *p - Vec3::Z * 0.1).collect(), width: None }));
    assert!(h.guides[6..].iter().any(|g| g.points.iter().skip(1).any(|p| HEAD.inside(*p))));
    let comb = |x: f32| Stroke { falloff: flat(), direction: Some(Vec3::X), ..Stroke::new(Kind::Comb, vec![Vec3::new(x, -0.12, 0.1)], 0.12) };
    let r = apply(&mut h, &[(comb(0.5), Some(&HEAD)), (comb(0.0), None)]).expect("groom");
    assert_eq!(r.guides_changed, 12, "{r:?}");
    assert_eq!(r.still_inside, 0, "the inner lock had no collider: {r:?}");
}

/// A call's total work is bounded from its sizes before anything runs
/// (review finding 4): the biggest hair, 64 strokes of 256 path points
/// with the head, is refused and the guides are untouched; the perf
/// fixture and a single long stroke over the biggest hair fit.
#[test]
fn groom_work_is_bounded_before_running() {
    let guide = Guide { points: (0..crate::hair::MAX_POINTS).map(|k| Vec3::new(0.3, -0.0005 * k as f32, 0.0)).collect(), width: None };
    let mut big = Hair { guides: vec![guide; crate::hair::MAX_GUIDES].into(), ..Hair::default() };
    let before = big.clone();
    let path: Vec<Vec3> = (0..MAX_PATH).map(|k| Vec3::new(0.3, -0.001 * k as f32, 0.0)).collect();
    let s = Stroke { mirror: true, direction: Some(Vec3::X), ..Stroke::new(Kind::Comb, path.clone(), 0.01) };
    let many: Vec<(Stroke, Option<&dyn Collider>)> = (0..MAX_STROKES).map(|_| (s.clone(), Some(&HEAD as &dyn Collider))).collect();
    let w = work(&big, &many);
    eprintln!("64 long strokes over 4,096 x 512 with the head: {w:.2e} units (bound {MAX_WORK:.0e})");
    let err = apply(&mut big, &many).unwrap_err();
    assert!(err.contains("units of work") && err.contains("split the call"), "{err}");
    assert!(big == before);
    let one = [(s.clone(), Some(&HEAD as &dyn Collider))];
    eprintln!("one long mirrored stroke over it: {:.2e} units", work(&big, &one) as f64);
    assert!(work(&big, &one) <= MAX_WORK, "{:.2e}", work(&big, &one) as f64);
    // The perf fixture's stroke (4,000 x 32, a 32-point path, the head).
    let perf = Hair { guides: vec![Guide { points: vec![Vec3::ZERO; 32], width: None }; 4000].into(), ..Hair::default() };
    let p = Stroke::new(Kind::Relax, path[..32].to_vec(), 0.12);
    assert!(work(&perf, &[(p, Some(&HEAD as &dyn Collider))]) * 10 < MAX_WORK);
}

/// The estimate covers the worst a settle can do (review fix): each point
/// can take a push and three tries of `clear_segment` (15 inner samples,
/// two pushes and a normal each) every round, and a lengthen's own
/// settle runs over the guides it made longer, not the ones it found.
#[test]
fn groom_work_counts_worst_collisions_and_lengthens_settle() {
    let short = Hair { guides: vec![Guide { points: vec![Vec3::ZERO, Vec3::NEG_Y * 0.01], width: None }; 100].into(), ..Hair::default() };
    let point = vec![Vec3::ZERO];
    let comb = Stroke { direction: Some(Vec3::X), ..Stroke::new(Kind::Comb, point.clone(), 0.01) };
    let with = work(&short, &[(comb.clone(), Some(&HEAD as &dyn Collider))]);
    let without = work(&short, &[(comb, None)]);
    let per_point_round = (with - without + 200) / (200 * ROUNDS as u64);
    assert_eq!(per_point_round, COLLIDER_CALLS * COLLIDER_COST);
    assert_eq!(COLLIDER_CALLS, 1 + 3 * (15 + 3));
    let lengthen = Stroke { length: Some(0.05), ..Stroke::new(Kind::Lengthen, point, 0.01) };
    let w = work(&short, &[(lengthen, Some(&HEAD as &dyn Collider))]);
    let after = 100 * crate::hair::MAX_POINTS as u64;
    assert!(w >= after * ROUNDS as u64 * COLLIDER_CALLS * COLLIDER_COST, "{w}: the lengthen's settle is charged at its new size");
}
