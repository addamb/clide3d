//! Facial hair from a human's face layout (`face.rs`): brows, lashes and
//! peach fuzz as ordinary `Hair` (strands in film, cards in games).
//!
//! - Brows: one guide per hair over a brow shape (a fuller head by the nose,
//!   an arch, a thin tail), denser in the body and sparse at the edges.
//!   Hairs grow the way real brows do: upright at the head, then outward
//!   along the brow, the upper rows angled down and the lower rows up so
//!   they meet along the middle, the tail sweeping out and down. They lie
//!   on the skin.
//! - Lashes: from the lid margins (`eyeball::Lids`), in two staggered rows
//!   on the upper lid and a sparse one below; they leave the lid forward and
//!   curl up (or down below), longest in the middle and fanning outward at
//!   the outer corner.
//! - Peach fuzz: short, very thin, pale vellus hair over the face, lying
//!   down and outward; it shows mostly against a rim light.
//!
//! Guides are made for the left (+X) side and mirrored.

use glam::{Vec2, Vec3};

use crate::face::Face;
use crate::hair::{Guide, Hair, facial_guide};

/// Nearest surface point and normal of the head, in the hair's space.
pub type Snap<'a> = dyn Fn(Vec3) -> Option<(Vec3, Vec3)> + 'a;

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 16_777_216.0
    }
}

fn ss(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The head surface under a face-plane point: from in front of the face,
/// the nearest surface along the way in.
fn place(face: &Face, snap: &Snap, q: Vec2) -> Option<(Vec3, Vec3)> {
    let mut p = face.origin + face.right * q.x + face.up * q.y + face.forward * (face.eye_r * 4.0);
    for _ in 0..40 {
        let (s, n) = snap(p)?;
        let d = (p - s).dot(n);
        if d.abs() < 1e-5 {
            return Some((s, n));
        }
        // Move in along the face's forward axis, by the distance to the skin.
        p -= face.forward * d.max(1e-5).min(face.eye_r);
    }
    snap(p)
}

/// Brows (`density` 0..2 scales the hair count).
pub fn brows(face: &Face, snap: &Snap, color: [f32; 3], density: f32, seed: u32) -> Hair {
    let (ex, er) = (face.eye.x, face.eye_r);
    let base = face.brow_y - er * 0.65;
    let head = Vec2::new(ex * 0.36, base - er * 0.12);
    let peak = Vec2::new(ex + er * 0.55, base + er * 0.2);
    let tail = Vec2::new(ex + er * 1.7, base - er * 0.2);
    let ctrl = peak * 2.0 - (head + tail) * 0.5;
    let at = |s: f32| head * (1.0 - s) * (1.0 - s) + ctrl * 2.0 * s * (1.0 - s) + tail * s * s;
    let tangent = |s: f32| ((ctrl - head) * 2.0 * (1.0 - s) + (tail - ctrl) * 2.0 * s).normalize_or(Vec2::X);
    let thick = |s: f32| er * (0.72 * (1.0 - s) + 0.2 * s) * (0.8 + 0.2 * ss(0.0, 0.08, s));
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761).max(1));
    // Guides map the flow; strands fill in between them.
    let target = 170usize;
    let mut guides = Vec::new();
    let mut tries = 0;
    while guides.len() < target.min(crate::hair::MAX_GUIDES / 2) && tries < target * 20 {
        tries += 1;
        let (s, v) = (rng.next(), rng.next() * 2.0 - 1.0);
        let keep = (0.45 + 0.55 * ss(0.02, 0.2, s) * (1.0 - 0.45 * ss(0.65, 1.0, s))) * (1.0 - v.abs().powi(3) * 0.85);
        if rng.next() > keep {
            continue;
        }
        let t = tangent(s);
        let up = Vec2::new(-t.y, t.x);
        let q = at(s) + up * (v * thick(s) * 0.5);
        // Upright at the head, along the brow after; rows meet in the middle.
        let along = (t - up * (v * 0.45)).normalize_or(t);
        let upright = Vec2::new(0.25, 1.0).normalize();
        let mut dir = upright.lerp(along, ss(0.05, 0.22, s)).normalize_or(t);
        if s > 0.75 {
            dir = (dir + Vec2::new(0.0, -0.35) * ss(0.75, 1.0, s)).normalize_or(dir);
        }
        let Some((root, n)) = place(face, snap, q) else { continue };
        let comb = face.right * dir.x + face.up * dir.y;
        let length = (0.0058 + 0.0032 * ss(0.05, 0.3, s) - 0.0015 * ss(0.75, 1.0, s)) * (0.8 + 0.4 * rng.next());
        guides.push(facial_guide(root, n, comb, length, 0.00012, false, snap));
    }
    let mut h = facial(guides, color, [color[0] * 1.5, color[1] * 1.45, color[2] * 1.4], 0.00008, seed);
    h.interpolate = true;
    h.strands = (3200.0 * density.clamp(0.0, 2.0)) as u32;
    h.rise = 0.04;
    h.frizz = 0.00025;
    h.length_variation = 0.35;
    h.center = Some(face.origin - face.forward * face.eye_r * 5.0);
    h
}

/// Upper and lower lashes of one eye's lids (in the hair's space, left eye).
pub fn lashes(face: &Face, lids: &crate::eyeball::Lids, eye_center: Vec3, color: [f32; 3], density: f32, seed: u32) -> Hair {
    let mut rng = Rng(seed.wrapping_mul(1_597_334_677).max(3));
    let mut guides = Vec::new();
    for (upper, margin) in [(true, &lids.upper), (false, &lids.lower)] {
        let pts: Vec<Vec3> = margin.iter().map(|p| Vec3::from(*p)).collect();
        if pts.len() < 2 {
            continue;
        }
        let rows = if upper { 2 } else { 1 };
        // Men's lashes are a little shorter and sparser.
        let male = face.sex;
        let count = ((if upper { 62.0 } else { 18.0 }) * (1.0 - 0.2 * male) * density.clamp(0.0, 2.0)) as usize;
        let first = guides.len();
        for i in 0..count {
            for row in 0..rows {
                if row == 1 && rng.next() < 0.35 {
                    continue;
                }
                // Along the margin, skipping the inner corner (caruncle).
                let s = 0.1 + 0.88 * (i as f32 + 0.5 * row as f32 + 0.3 * rng.next()) / count as f32;
                let x = s * (pts.len() - 1) as f32;
                let k = (x as usize).min(pts.len() - 2);
                let m = pts[k].lerp(pts[k + 1], x - k as f32);
                let out = (m - eye_center).normalize_or(face.forward);
                let vertical = if upper { face.up } else { -face.up };
                // Roots on the front edge of the lid, rows staggered.
                let root = m + out * (0.0007 + 0.0004 * row as f32) + vertical * 0.0003;
                let middle = (std::f32::consts::PI * s).sin();
                let length = (1.0 - 0.25 * male) * if upper { (0.0038 + 0.0030 * middle.powf(0.7)) * (0.85 + 0.3 * rng.next()) } else { (0.0014 + 0.0014 * middle) * (0.7 + 0.5 * rng.next()) };
                // Leave forward and a little up (down below), fan outward
                // toward the outer corner, curl toward `comb`.
                let fan = face.right * (0.55 * ss(0.55, 1.0, s) - 0.2 * (1.0 - ss(0.0, 0.3, s)));
                let leave = (face.forward * 0.9 + vertical * if upper { 0.25 } else { 0.5 } + out * 0.15 + fan * 0.5).normalize_or(face.forward);
                let comb = (vertical + fan * 0.6).normalize_or(vertical);
                let mut g = facial_guide(root, leave, comb, length, 0.0, true, |_: Vec3| None::<(Vec3, Vec3)>);
                // A little irregular, like real lashes.
                let wobble = face.right * (rng.next() - 0.5) * 0.0006 + vertical * (rng.next() - 0.5) * 0.0004;
                let n = g.points.len();
                for (j, p) in g.points.iter_mut().enumerate() {
                    *p += wobble * (j as f32 / (n - 1) as f32).powi(2);
                }
                guides.push(g);
            }
        }
        // Lashes gather in small clumps toward their tips.
        let mine = &mut guides[first..];
        for group in mine.chunks_mut(3) {
            let n = group[0].points.len();
            let tip = group.iter().map(|g| g.points[n - 1]).sum::<Vec3>() / group.len() as f32;
            for g in group.iter_mut() {
                let pull = tip - g.points[n - 1];
                for (j, p) in g.points.iter_mut().enumerate() {
                    *p += pull * 0.45 * (j as f32 / (n - 1) as f32).powi(2);
                }
            }
        }
    }
    guides.truncate(crate::hair::MAX_GUIDES / 2);
    facial(guides, color, color, 0.00011, seed)
}

/// Peach fuzz over the face (`density` 0..2).
pub fn fuzz(face: &Face, snap: &Snap, color: [f32; 3], density: f32, seed: u32) -> Hair {
    let (ex, er) = (face.eye.x, face.eye_r);
    let step = er * 0.55;
    let mut guides = Vec::new();
    let mut rng = Rng(seed.wrapping_mul(747_796_405).max(5));
    let (y0, y1) = (face.chin_y - er * 0.6, face.brow_y + er * 4.5);
    let mut y = y0;
    while y <= y1 {
        let mut x = 0.0;
        while x <= ex * 2.5 {
            let q = Vec2::new(x + (rng.next() - 0.5) * step * 0.5, y + (rng.next() - 0.5) * step * 0.5);
            x += step;
            let bare = q.distance(face.eye) < er * 1.25 || face.lip_mask(Vec2::new(q.x.abs(), q.y)).0 > 0.2 || (q.y > face.brow_y - er * 0.5 && q.y < face.brow_y + er * 0.6 && q.x > ex * 0.35 && q.x < ex + er * 2.0);
            if bare || q.x < 0.0 {
                continue;
            }
            let Some((root, n)) = place(face, snap, q) else { continue };
            // Hair is shed toward the front of the face only.
            if n.dot(face.forward) < -0.2 {
                continue;
            }
            // Down and out below the brows, up and out on the forehead.
            let dir = if q.y > face.brow_y { Vec2::new(0.6, 0.5) } else if q.x > ex * 1.6 { Vec2::new(0.6, -0.8) } else { Vec2::new(0.35, -1.0) };
            let comb = face.right * dir.x + face.up * dir.y;
            let length = 0.0016 * (0.7 + 0.6 * rng.next());
            guides.push(facial_guide(root, n, comb, length, 0.00008, false, snap));
            if guides.len() >= crate::hair::MAX_GUIDES / 2 {
                break;
            }
        }
        y += step;
    }
    let mut h = facial(guides, [color[0] * 0.8, color[1] * 0.78, color[2] * 0.75], color, 0.000014, seed);
    h.interpolate = true;
    h.strands = (24_000.0 * density.clamp(0.0, 2.0)) as u32;
    h.rise = 0.02;
    h.frizz = 0.0003;
    h.length_variation = 0.6;
    h.center = Some(face.origin - face.forward * face.eye_r * 5.0);
    h
}

/// Common facial hair settings: one strand per guide, mirrored.
fn facial(guides: Vec<Guide>, root: [f32; 3], tip: [f32; 3], width: f32, seed: u32) -> Hair {
    let n = guides.len() as u32;
    Hair {
        guides,
        width: 0.0005,
        taper: 0.05,
        cards: 1,
        spread: 0.00005,
        root_color: root,
        tip_color: tip.map(|c| c.min(1.0)),
        center: None,
        mirror: true,
        seed: seed.max(1),
        strands: n * 2,
        strand_width: width,
        clump: 0.0,
        clump_size: 0.0,
        clump_levels: 1,
        frizz: 0.00004,
        volume: 0.0,
        flyaways: 0.0,
        length_variation: 0.1,
        melanin_variation: 0.35,
        interpolate: false,
        rise: 1.0,
    }
}
