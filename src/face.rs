//! Face-aware skin: regional pores, expression wrinkles, oil, color and lips
//! for a human head, driven by the human's own landmarks.
//!
//! `create_human` heads know where their eyes, nose, mouth, chin and ears
//! are (MakeHuman markers and a few fixed base-mesh vertices, which follow
//! every shape target). From them `Face` lays out, in a face plane through
//! the eyes:
//!
//! - region weights: forehead, nose, cheeks, under the eyes, chin, ears,
//!   lips, the oily T-zone, the beard area and sun-exposed skin;
//! - pores in three sizes (fine on the forehead, medium on the cheeks,
//!   large on the nose), pits with darker, rougher insides;
//! - wrinkle lines along real expression lines (forehead lines, frown
//!   lines, crow's feet, under-eye creases, nasolabial folds, marionette
//!   lines, upper-lip lines), deeper with `age` and `wrinkles`;
//! - a fine cross-hatch of skin micro-lines over everything;
//! - color: redness on nose, cheeks, chin and ears, a cool tint under the
//!   eyes, freckles, moles, age spots and stubble when asked;
//! - lips: a shaped vermilion with a soft border, vertical lip lines, a
//!   darker meeting line and a moist, glossy surface;
//! - shine: lower roughness and a thin oily coat on the T-zone.
//!
//! Everything is procedural in the object's space, so the film tracers
//! (CPU here, GPU in `face_shade` of `film/trace.wgsl`, from `Face::pack`)
//! and texture bakes read the same skin at any resolution, and
//! `fold_height` gives the broad part of the folds to sculpt as geometry.

use glam::{IVec3, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::material::noise;

/// Skin settings of a human (`set_human skin`). All amounts 0..1 unless
/// said otherwise.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Skin {
    /// Face detail on (default true).
    pub enabled: bool,
    /// Skin age in years for wrinkles (default: the human's age).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<f32>,
    /// Pore depth and visibility, 0..2.
    pub pores: f32,
    /// Wrinkle depth on top of what age gives, 0..2.
    pub wrinkles: f32,
    pub freckles: f32,
    /// How many moles (0..1 gives 0..8).
    pub moles: f32,
    /// Redness of nose, cheeks, chin and ears.
    pub redness: f32,
    /// Cool, darker skin under the eyes.
    pub under_eye: f32,
    /// Oily shine of the T-zone (forehead, nose, chin).
    pub oil: f32,
    /// Short beard shadow (dots of cut hair over the beard area).
    pub stubble: f32,
    /// Peach fuzz shown as soft sheen in film (real fuzz strands come from
    /// `hair grow style fuzz`).
    pub fuzz: f32,
    /// Lip color (sRGB); unset = a rose from the skin color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lip_color: Option<[f32; 3]>,
    /// How strongly lips are drawn (color, lines, gloss).
    pub lips: f32,
    /// Changes freckles, moles and line breaks.
    pub seed: u32,
}

impl Default for Skin {
    fn default() -> Self {
        Skin { enabled: true, age: None, pores: 1.0, wrinkles: 0.6, freckles: 0.0, moles: 0.0, redness: 0.35, under_eye: 0.35, oil: 0.5, stubble: 0.0, fuzz: 0.5, lip_color: None, lips: 1.0, seed: 0 }
    }
}

impl Skin {
    pub fn sanitize(&mut self) {
        let fix = |v: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(0.0, hi) } else { d };
        let d = Skin::default();
        self.pores = fix(self.pores, 2.0, d.pores);
        self.wrinkles = fix(self.wrinkles, 2.0, d.wrinkles);
        for (v, dv) in [(&mut self.freckles, 0.0), (&mut self.moles, 0.0), (&mut self.redness, d.redness), (&mut self.under_eye, d.under_eye), (&mut self.oil, d.oil), (&mut self.stubble, 0.0), (&mut self.fuzz, d.fuzz), (&mut self.lips, d.lips)] {
            *v = fix(*v, 1.0, dv);
        }
        self.age = self.age.filter(|a| a.is_finite()).map(|a| a.clamp(1.0, 100.0));
        self.lip_color = self.lip_color.map(|c| c.map(|v| fix(v, 1.0, 0.5)));
    }
}

/// Fixed base-mesh vertices (MakeHuman CC0 topology; left side, +X) used as
/// landmarks: they move with every shape target.
const NOSE_TIP: usize = 297;
const NASION: usize = 132;
const SUBNASALE: usize = 343;
const ALA_L: usize = 7099;
const MOUTH_L: usize = 7192;
const MOUTH_R: usize = 432;
const STOMION: usize = 474;
const LIP_TOP: usize = 362;
const LIP_BOTTOM: usize = 492;
const CHIN: usize = 733;
const EAR_L: usize = 12038;

/// Where the parts of a face are, in the human's space (meters).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Landmarks {
    /// Eyeball centers (left = +X first) and the eye socket radius.
    pub eyes: [Vec3; 2],
    pub eye_radius: f32,
    pub nose_tip: Vec3,
    pub nasion: Vec3,
    pub subnasale: Vec3,
    pub ala: Vec3,
    pub mouth: [Vec3; 2],
    pub stomion: Vec3,
    pub lip_top: Vec3,
    pub lip_bottom: Vec3,
    pub chin: Vec3,
    pub ear: Vec3,
    /// Height of the cut between a head clay and a body clay (they
    /// overlap just above it).
    pub neck_cut: f32,
}

impl Landmarks {
    /// From a morphed human's points (base-mesh numbering, as `Body::points`
    /// or its subdivision, which keeps the first indices).
    pub fn of(points: &[Vec3], body: &crate::human::Body) -> Option<Landmarks> {
        let p = |i: usize| points.get(i).copied();
        Some(Landmarks {
            eyes: [body.marker("helper-l-eye")?, body.marker("helper-r-eye")?],
            eye_radius: body.eye_radius("l")?,
            nose_tip: p(NOSE_TIP)?,
            nasion: p(NASION)?,
            subnasale: p(SUBNASALE)?,
            ala: p(ALA_L)?,
            mouth: [p(MOUTH_L)?, p(MOUTH_R)?],
            stomion: p(STOMION)?,
            lip_top: p(LIP_TOP)?,
            lip_bottom: p(LIP_BOTTOM)?,
            chin: p(CHIN)?,
            ear: p(EAR_L)?,
            neck_cut: {
                let (neck, head) = (body.marker("joint-neck")?, body.marker("joint-head")?);
                neck.y + 0.3 * (head.y - neck.y)
            },
        })
    }
}

/// One wrinkle line in the face plane (left side; the right mirrors it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub points: Vec<Vec2>,
    /// Groove half width and depth (meters) for shading.
    pub width: f32,
    pub depth: f32,
    /// Folds bulge on their +side (the cheek beside a nasolabial fold).
    pub fold: bool,
    lo: Vec2,
    hi: Vec2,
}

impl Line {
    fn new(points: Vec<Vec2>, width: f32, depth: f32, fold: bool) -> Line {
        let (lo, hi) = points.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
        Line { points, width, depth, fold, lo, hi }
    }
}

/// How far fold geometry reaches beyond the shading groove, and how deep.
const FOLD_WIDTH: f32 = 3.0;
const FOLD_DEPTH: f32 = 1.4;
/// Most lines and moles a face packs (the GPU reads fixed records).
pub const MAX_LINES: usize = 64;
pub const MAX_POINTS: usize = 8;
const MAX_MOLES: usize = 8;

/// A face ready to shade: its frame, region layout, lines and settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Face {
    /// Midpoint between the eyes, and the face axes (right = toward the
    /// left eye, +X; up; forward out of the face).
    pub origin: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    /// Face-plane positions (x >= 0 side): eye (x, y), eye radius.
    pub(crate) eye: Vec2,
    pub(crate) eye_r: f32,
    pub(crate) tip: Vec2,
    pub(crate) nasion_y: f32,
    pub(crate) sub_y: f32,
    pub(crate) ala: Vec2,
    pub(crate) mouth: Vec2,
    pub(crate) stomion_y: f32,
    pub(crate) lip_top_y: f32,
    pub(crate) lip_bottom_y: f32,
    pub(crate) chin_y: f32,
    pub(crate) brow_y: f32,
    /// Forward coordinate of the lips (in front of it is lip surface).
    pub(crate) lip_z: f32,
    /// Ear point in face coordinates (x, y, z).
    pub(crate) ear: Vec3,
    /// Settings after age: (pores, freckles, redness, under_eye), (oil,
    /// stubble, fuzz, lips), lip color, age spots, micro-line depth.
    pub(crate) pores: f32,
    pub(crate) freckles: f32,
    pub(crate) redness: f32,
    pub(crate) under_eye: f32,
    pub(crate) oil: f32,
    pub(crate) stubble: f32,
    pub(crate) fuzz: f32,
    pub(crate) lips: f32,
    pub(crate) lip_color: Vec3,
    pub(crate) age_spots: f32,
    pub(crate) micro: f32,
    pub(crate) seed: u32,
    /// Object-space height below which the face fades out (the neck cut).
    pub(crate) low_y: f32,
    /// 0 female .. 1 male: lip tint.
    pub(crate) sex: f32,
    pub lines: Vec<Line>,
    /// (x signed, y, radius, raise).
    pub(crate) moles: Vec<[f32; 4]>,
}

/// What `Face::shade` gives back. `n` is the shading normal (object space),
/// `color` sRGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub n: Vec3,
    pub color: Vec3,
    pub rough: f32,
    /// Extra thin glossy layer (oil, moist lips), 0..1.
    pub coat: f32,
    /// Extra grazing sheen (peach fuzz).
    pub sheen: f32,
}

fn ss(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Gaussian blob in the plane.
fn blob(q: Vec2, c: Vec2, r: Vec2) -> f32 {
    let d = (q - c) / r;
    (-d.length_squared()).exp()
}

/// Integer hash (the same as `face_hash` in trace.wgsl).
pub(crate) fn hash(c: IVec3, seed: u32) -> u32 {
    let mut h = (c.x as u32).wrapping_mul(73856093) ^ (c.y as u32).wrapping_mul(19349663) ^ (c.z as u32).wrapping_mul(83492791) ^ seed.wrapping_mul(0x9e37_79b9);
    h = (h ^ (h >> 16)).wrapping_mul(0x7feb_352d);
    h = (h ^ (h >> 15)).wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

/// Nearest jittered feature point of a cell grid (`cell` apart): the point
/// and a random 0..1 of its cell.
fn worley(p: Vec3, cell: f32, seed: u32) -> (Vec3, f32) {
    let q = p / cell;
    let base = q.floor().as_ivec3();
    let mut best = (f32::MAX, Vec3::ZERO, 0.0);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let c = base + IVec3::new(x, y, z);
                let h = hash(c, seed);
                let j = Vec3::new(unit(h), unit(hash(c, seed ^ 0x51ed)), unit(hash(c, seed ^ 0xa3c5)));
                let f = c.as_vec3() + Vec3::splat(0.1) + j * 0.8;
                let d = (f - q).length_squared();
                if d < best.0 {
                    best = (d, f, unit(hash(c, seed ^ 0x7f4a)));
                }
            }
        }
    }
    (best.1 * cell, best.2)
}

/// `p - f` along the surface (normal `n` removed): spots on skin are
/// round on the surface wherever their feature point sits in depth.
fn along(p: Vec3, f: Vec3, n: Vec3) -> Vec3 {
    let d = p - f;
    d - n * d.dot(n)
}

/// A round pit of radius `r`, depth 1: (height, gradient).
fn pit(p: Vec3, f: Vec3, n: Vec3, r: f32) -> (f32, Vec3) {
    let d = along(p, f, n);
    let t = d.length_squared() / (r * r);
    if t >= 1.0 {
        return (0.0, Vec3::ZERO);
    }
    let s = 1.0 - t;
    // h = -s^2; dh/dp = -2 s * ds/dp = -2 s * (-2 d / r^2).
    (-s * s, d * (4.0 * s / (r * r)))
}

/// Region weights at one point.
#[derive(Clone, Copy, Debug, Default)]
pub struct Regions {
    pub forehead: f32,
    pub nose: f32,
    pub cheek: f32,
    pub under_eye: f32,
    pub chin: f32,
    pub ear: f32,
    pub lips: f32,
    /// 0 at the lip border .. 1 at the lip center line, across each lip.
    pub lip_depth: f32,
    /// Distance from the meeting line of the lips, as a part of the lip.
    pub lip_r: f32,
    pub tzone: f32,
    pub beard: f32,
    pub sun: f32,
    /// How much this is the face at all (front of the head).
    pub face: f32,
}

impl Face {
    pub fn new(l: &Landmarks, skin: &Skin, human_age: f32, sex: f32) -> Face {
        let origin = (l.eyes[0] + l.eyes[1]) * 0.5;
        let right = (l.eyes[0] - l.eyes[1]).normalize_or(Vec3::X);
        let up = (Vec3::Y - right * right.y).normalize_or(Vec3::Y);
        let forward = right.cross(up);
        let to2 = |p: Vec3| Vec2::new((p - origin).dot(right).abs(), (p - origin).dot(up));
        let mut s = skin.clone();
        s.sanitize();
        let age = s.age.unwrap_or(human_age).clamp(1.0, 100.0);
        let eye = to2(l.eyes[0]);
        let eye_r = l.eye_radius;
        let mouth = (to2(l.mouth[0]) + to2(l.mouth[1])) * 0.5;
        let lip_z = (l.stomion - origin).dot(forward);
        let ear_d = l.ear - origin;
        let mut f = Face {
            origin,
            right,
            up,
            forward,
            eye,
            eye_r,
            tip: to2(l.nose_tip),
            nasion_y: to2(l.nasion).y,
            sub_y: to2(l.subnasale).y,
            ala: to2(l.ala),
            mouth,
            stomion_y: to2(l.stomion).y,
            lip_top_y: to2(l.lip_top).y,
            lip_bottom_y: to2(l.lip_bottom).y,
            chin_y: to2(l.chin).y,
            brow_y: eye.y + eye_r * 1.75,
            lip_z,
            ear: Vec3::new(ear_d.dot(right).abs(), ear_d.dot(up), ear_d.dot(forward)),
            pores: s.pores,
            freckles: s.freckles,
            redness: s.redness,
            under_eye: s.under_eye,
            oil: s.oil,
            stubble: s.stubble,
            fuzz: s.fuzz,
            lips: s.lips,
            lip_color: Vec3::ZERO,
            age_spots: ss(50.0, 80.0, age),
            micro: 1.0 + 0.7 * ss(30.0, 80.0, age),
            seed: s.seed,
            low_y: l.neck_cut,
            sex: sex.clamp(0.0, 1.0),
            lines: Vec::new(),
            moles: Vec::new(),
        };
        // Zero = take a rose from the skin color when shading.
        f.lip_color = s.lip_color.map_or(Vec3::ZERO, Vec3::from);
        f.lines = f.layout_lines(age, s.wrinkles);
        f.moles = f.place_moles(s.moles);
        f
    }

    /// The expression lines, aged: (shading groove width, depth) each.
    fn layout_lines(&self, age: f32, wrinkles: f32) -> Vec<Line> {
        let mut out = Vec::new();
        let any = wrinkles * (0.25 + 0.75 * ss(18.0, 70.0, age));
        let old = wrinkles * ss(35.0, 75.0, age);
        let rand = |k: u32| unit(hash(IVec3::new(k as i32, 7, 3), self.seed)) * 2.0 - 1.0;
        let (ex, er) = (self.eye.x, self.eye_r);
        // Forehead lines: roughly parallel to the brows, a little lower in
        // the middle, wavy; more and deeper with age.
        let top = self.brow_y + er * 0.9;
        for k in 0..4 {
            let y0 = top + k as f32 * er * 0.62 + rand(k) * er * 0.08;
            let reach = ex * (1.35 + 0.15 * rand(k + 10)) - k as f32 * ex * 0.12;
            let pts = (0..MAX_POINTS)
                .map(|i| {
                    let x = reach * i as f32 / (MAX_POINTS - 1) as f32;
                    let t = x / reach;
                    Vec2::new(x, y0 - er * 0.18 * (1.0 - t * t) + er * 0.05 * (t * 9.0 + k as f32 * 2.0).sin())
                })
                .collect();
            let depth = 0.00022 * any * (1.0 - 0.15 * k as f32);
            out.push(Line::new(pts, 0.00045, depth, false));
        }
        // Frown lines between the brows.
        for (k, x) in [(0, 0.2), (1, 0.33)] {
            let x = ex * x;
            let a = Vec2::new(x, self.brow_y - er * 0.35);
            let b = Vec2::new(x + ex * 0.06, self.brow_y + er * 0.9 + rand(20 + k) * er * 0.1);
            out.push(Line::new(vec![a, a.lerp(b, 0.5) + Vec2::new(ex * 0.02, 0.0), b], 0.0004, 0.00028 * old * (1.0 - 0.3 * k as f32), false));
        }
        // Crow's feet: a fan from the outer eye corner.
        let canthus = Vec2::new(ex + er * 1.0, self.eye.y - er * 0.08);
        for (k, angle) in [-38.0f32, -14.0, 8.0, 30.0].iter().enumerate() {
            let a = (angle + rand(30 + k as u32) * 5.0).to_radians();
            let dir = Vec2::new(a.cos(), a.sin());
            let bend = Vec2::new(0.0, -er * 0.12);
            let (r0, r1) = (er * 0.25, er * (0.95 + 0.25 * rand(40 + k as u32)));
            let pts = (0..5).map(|i| {
                let t = i as f32 / 4.0;
                canthus + dir * (r0 + (r1 - r0) * t) + bend * t * t
            });
            out.push(Line::new(pts.collect(), 0.00035, 0.00016 * any, false));
        }
        // Creases under the lower lid (the inner one is the tear trough).
        for (k, r) in [1.28f32, 1.55].iter().enumerate() {
            let c = self.eye + Vec2::new(0.0, er * 0.1);
            let pts = (0..MAX_POINTS)
                .map(|i| {
                    let a = (205.0 + 130.0 * i as f32 / (MAX_POINTS - 1) as f32).to_radians();
                    c + Vec2::new(a.cos() * er * r * 1.05, a.sin() * er * r * 0.72)
                })
                .collect();
            out.push(Line::new(pts, 0.0005, 0.00013 * any * (1.0 - 0.3 * k as f32), false));
        }
        // Nasolabial folds: from the nose wing around the mouth corner; a
        // soft line even when young.
        let fold = wrinkles * (0.15 + 0.85 * ss(20.0, 70.0, age));
        let a = self.ala + Vec2::new(er * 0.25, er * 0.15);
        let end = Vec2::new(self.mouth.x + er * 0.6, self.mouth.y - er * 0.85);
        let ctrl = Vec2::new(self.mouth.x + er * 0.75, (a.y + self.mouth.y) * 0.5 + er * 0.2);
        let bez = |t: f32| a * (1.0 - t) * (1.0 - t) + ctrl * 2.0 * t * (1.0 - t) + end * t * t;
        out.push(Line::new((0..MAX_POINTS).map(|i| bez(i as f32 / (MAX_POINTS - 1) as f32)).collect(), 0.0008, 0.00034 * fold, true));
        // Marionette lines down from the mouth corners.
        let m0 = self.mouth + Vec2::new(er * 0.12, -er * 0.25);
        let m1 = Vec2::new(self.mouth.x + er * 0.32, self.chin_y + er * 0.3);
        out.push(Line::new(vec![m0, m0.lerp(m1, 0.5) + Vec2::new(er * 0.06, 0.0), m1], 0.0006, 0.00026 * old, true));
        // Upper lip lines (older skin).
        for k in 0..5 {
            let x = er * (0.18 + 0.27 * k as f32) + rand(50 + k) * er * 0.05;
            let y0 = self.lip_top_y + er * 0.05;
            out.push(Line::new(vec![Vec2::new(x, y0), Vec2::new(x + er * 0.04, y0 + er * (0.35 + 0.1 * rand(60 + k)))], 0.00022, 0.00008 * old, false));
        }
        out.retain(|l| l.depth > 1e-7);
        out.truncate(MAX_LINES);
        out
    }

    fn place_moles(&self, amount: f32) -> Vec<[f32; 4]> {
        let n = ((amount * MAX_MOLES as f32).round() as usize).min(MAX_MOLES);
        let mut out = Vec::new();
        let mut k = 0u32;
        while out.len() < n && k < 200 {
            let at = k;
            let r = |j: u32| unit(hash(IVec3::new(at as i32, j as i32, 91), self.seed));
            k += 1;
            let x = (r(0) * 2.0 - 1.0) * self.eye.x * 2.2;
            let y = self.chin_y + (self.brow_y + self.eye_r * 2.5 - self.chin_y) * r(1);
            let q = Vec2::new(x.abs(), y);
            // Not on the eyes or the lips.
            if q.distance(self.eye) < self.eye_r * 1.6 || self.lip_mask(q).0 > 0.05 {
                continue;
            }
            out.push([x, y, 0.0006 + 0.0009 * r(2), r(3)]);
        }
        out
    }

    /// Face-plane coordinates: (|x|, y), sign of x, forward z.
    fn plane(&self, p: Vec3) -> (Vec2, f32, f32) {
        let d = p - self.origin;
        let x = d.dot(self.right);
        (Vec2::new(x.abs(), d.dot(self.up)), if x < 0.0 { -1.0 } else { 1.0 }, d.dot(self.forward))
    }

    /// Lip mask, how deep into the lip (0 border .. 1 middle), and the
    /// position across the lip (0 meeting line .. 1 border).
    pub(crate) fn lip_mask(&self, q: Vec2) -> (f32, f32, f32) {
        let u = q.x / self.mouth.x.max(1e-4);
        let slit = self.stomion_y + (self.mouth.y - self.stomion_y) * u * u;
        let v = q.y - slit;
        let w = (1.0 - u * u).max(0.0);
        let bow = 1.0 + 0.12 * (-((u - 0.3) / 0.16).powi(2)).exp() - 0.12 * (-(u / 0.09).powi(2)).exp();
        let hu = (self.lip_top_y - self.stomion_y).max(1e-4) * 0.72 * w.powf(0.45) * bow;
        let hl = (self.stomion_y - self.lip_bottom_y).max(1e-4) * w.powf(0.6);
        let r = if v > 0.0 { v / hu.max(1e-5) } else { -v / hl.max(1e-5) };
        let mask = (1.0 - ss(0.9, 1.02, r)) * (1.0 - ss(0.94, 1.04, u));
        (mask, 1.0 - (r * 2.0 - 1.0).abs(), r)
    }

    pub fn regions(&self, p: Vec3) -> Regions {
        let (q, _, z) = self.plane(p);
        let er = self.eye_r;
        // The face is the front of the head: ahead of the ears.
        let mut g = Regions { face: ss(self.ear.z + er * 0.8, self.ear.z + er * 3.0, z), ..Regions::default() };
        let front = g.face;
        g.forehead = ss(self.brow_y, self.brow_y + er * 0.9, q.y) * (1.0 - ss(self.eye.x * 1.35, self.eye.x * 1.9, q.x)) * (1.0 - ss(self.brow_y + er * 5.5, self.brow_y + er * 7.5, q.y)) * front;
        let half = self.ala.x * (0.35 + 0.65 * ss(self.nasion_y, self.tip.y, q.y));
        g.nose = (1.0 - ss(half * 0.85, half * 1.2, q.x)) * ss(self.sub_y - er * 0.25, self.sub_y + er * 0.1, q.y) * (1.0 - ss(self.nasion_y, self.nasion_y + er * 0.6, q.y)) * ss(self.lip_z - er * 0.5, self.lip_z + er * 0.4, z);
        g.cheek = blob(q, Vec2::new(self.eye.x + er * 0.45, (self.tip.y + self.eye.y) * 0.5 - er * 0.4), Vec2::new(er * 1.7, er * 1.5)) * front * (1.0 - g.nose);
        g.under_eye = blob(q, Vec2::new(self.eye.x - er * 0.1, self.eye.y - er * 1.15), Vec2::new(er * 1.1, er * 0.5)) * front;
        g.chin = blob(q, Vec2::new(0.0, self.chin_y + er * 0.35), Vec2::new(er * 1.2, er * 0.9)) * front;
        let ear_q = Vec3::new(q.x, q.y, z);
        g.ear = (-(ear_q - self.ear).length_squared() / (er * er * 3.6)).exp() * ss(self.ear.x - er * 1.4, self.ear.x - er * 0.8, q.x);
        let (lip, depth, r) = self.lip_mask(q);
        g.lips = lip * ss(self.lip_z - er * 0.8, self.lip_z - er * 0.35, z) * self.lips;
        g.lip_depth = depth;
        g.lip_r = r;
        g.tzone = (g.forehead * (1.0 - ss(self.eye.x * 0.5, self.eye.x * 1.2, q.x)) + g.nose + g.chin * 0.7).min(1.0);
        // The beard: below a line from the ears to under the cheekbones,
        // down onto the upper neck, not on the lips.
        // The beard line: up to the nose over the lip, down to the mouth
        // corner, then rising across the cheek to the sideburn.
        let t = (q.x / self.mouth.x.max(1e-4)).min(1.0);
        let line = self.sub_y - er * 0.1 + (self.mouth.y + er * 0.9 - self.sub_y + er * 0.1) * t * t + (q.x - self.mouth.x).max(0.0) * 0.75;
        g.beard = (1.0 - ss(line - er * 0.7, line + er * 0.3, q.y)) * ss(self.chin_y - er * 3.0, self.chin_y - er * 1.5, q.y) * (1.0 - ss(self.ear.x - er * 1.6, self.ear.x - er * 0.9, q.x)) * (1.0 - lip) * ss(self.ear.z + er * 0.2, self.ear.z + er * 1.5, z) * (1.0 - g.nose);
        g.sun = ((g.nose + g.cheek + g.forehead * 0.7) * front).min(1.0);
        g
    }

    /// Height of the wrinkle lines at `q` (and its face-plane gradient):
    /// sharp grooves for shading, or (geo) the broad folds for geometry.
    fn lines_height(&self, q: Vec2, p: Vec3, geo: bool) -> (f32, Vec2) {
        let mut h = 0.0;
        let mut grad = Vec2::ZERO;
        for (k, l) in self.lines.iter().enumerate() {
            let (w, depth) = if geo { (l.width * FOLD_WIDTH, l.depth * FOLD_DEPTH) } else { (l.width, l.depth) };
            let reach = w * 3.0;
            if q.x < l.lo.x - reach || q.y < l.lo.y - reach || q.x > l.hi.x + reach || q.y > l.hi.y + reach {
                continue;
            }
            // Nearest point on the polyline, and how far along it is.
            let mut best = (f32::MAX, Vec2::ZERO, 0.0, 1.0);
            let segments = (l.points.len() - 1) as f32;
            for (i, s) in l.points.windows(2).enumerate() {
                let ab = s[1] - s[0];
                let t = ((q - s[0]).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
                let c = s[0] + ab * t;
                let d2 = q.distance_squared(c);
                if d2 < best.0 {
                    let side = if ab.perp_dot(q - s[0]) < 0.0 { 1.0 } else { -1.0 };
                    best = (d2, c, (i as f32 + t) / segments, side);
                }
            }
            let d = best.0.sqrt();
            if d > reach {
                continue;
            }
            let t = best.2;
            let taper = ss(0.0, 0.18, t) * (1.0 - ss(0.75, 1.0, t));
            // Lines break up and vary along their length.
            let breaks = if geo { 1.0 } else { (0.45 + 1.2 * (noise(p / 0.006 + Vec3::splat(k as f32 * 7.3)) - 0.5) + 0.4).clamp(0.0, 1.0) };
            let a = depth * taper * breaks;
            let dir = if d > 1e-9 { (q - best.1) / d } else { Vec2::ZERO };
            let e = (-(d / w).powi(2)).exp();
            h -= a * e;
            grad += dir * (a * e * 2.0 * d / (w * w));
            if l.fold {
                // A soft bulge on the cheek side of a fold.
                let ds = d * best.3 - w * 1.6;
                let wb = w * 1.4;
                let eb = (-(ds / wb).powi(2)).exp();
                h += 0.45 * a * eb;
                grad += dir * best.3 * (0.45 * a * eb * -2.0 * ds / (wb * wb));
            }
        }
        (h, grad)
    }

    /// How much of the face shows at `p`: it fades out down the neck, where
    /// a head clay overlaps the body, into the body's plain skin.
    pub fn keep(&self, p: Vec3) -> f32 {
        ss(self.low_y + 0.012, self.low_y + 0.03, p.y)
    }

    /// Whether `p` (with `pad` around it) is near enough a line for its
    /// fold geometry to reach.
    pub fn near_folds(&self, p: Vec3, pad: f32) -> bool {
        let (q, _, z) = self.plane(p);
        // The face is in front of the ears.
        z > self.ear.z - pad && self.lines.iter().any(|l| {
            let reach = l.width * FOLD_WIDTH * 3.0 + pad;
            q.x >= l.lo.x - reach && q.y >= l.lo.y - reach && q.x <= l.hi.x + reach && q.y <= l.hi.y + reach
        })
    }

    /// Broad fold height for sculpting (meters, negative = in).
    pub fn fold_height(&self, p: Vec3) -> f32 {
        let (q, _, _) = self.plane(p);
        let g = self.regions(p);
        self.lines_height(q, p, true).0 * g.face.max(ss(self.chin_y - self.eye_r * 6.0, self.chin_y - self.eye_r * 1.0, q.y))
    }

    /// The face for `face_shade` in trace.wgsl: a header of 13 vec4 (see
    /// there), the moles (one vec4 each), then each line as 24 floats:
    /// (point count, width, depth, fold), (bounds lo, hi), 8 points.
    pub fn pack(&self) -> Vec<f32> {
        let v3 = |v: Vec3| [v.x, v.y, v.z];
        let mut out = Vec::new();
        out.extend(v3(self.origin));
        out.push(self.eye.x);
        out.extend(v3(self.right));
        out.push(self.eye.y);
        out.extend(v3(self.up));
        out.push(self.eye_r);
        out.extend(v3(self.forward));
        out.push(f32::from_bits(self.seed));
        out.extend([self.tip.x, self.tip.y, self.nasion_y, self.sub_y]);
        out.extend([self.ala.x, self.ala.y, self.mouth.x, self.mouth.y]);
        out.extend([self.stomion_y, self.lip_top_y, self.lip_bottom_y, self.chin_y]);
        out.extend([self.brow_y, self.lip_z, self.age_spots, self.micro]);
        out.extend(v3(self.ear));
        out.push(self.lips);
        out.extend([self.pores, self.freckles, self.redness, self.under_eye]);
        out.extend([self.oil, self.stubble, self.fuzz, 0.0]);
        out.extend(v3(self.lip_color));
        out.push(self.low_y);
        out.extend([self.sex, 0.0, 0.0, 0.0]);
        for m in &self.moles {
            out.extend(m);
        }
        for l in &self.lines {
            out.extend([l.points.len() as f32, l.width, l.depth, if l.fold { 1.0 } else { 0.0 }, l.lo.x, l.lo.y, l.hi.x, l.hi.y]);
            for i in 0..MAX_POINTS {
                let p = l.points.get(i).copied().unwrap_or(Vec2::ZERO);
                out.extend([p.x, p.y]);
            }
        }
        out
    }

    /// Lines and moles in `pack`.
    pub fn counts(&self) -> (u32, u32) {
        (self.lines.len() as u32, self.moles.len() as u32)
    }

    /// Skin at one point (object space `p` and normal `n`, sRGB `color`).
    pub fn shade(&self, p: Vec3, n: Vec3, color: Vec3, rough: f32) -> Look {
        let (q, sx, _) = self.plane(p);
        let g = self.regions(p);
        let mut height_grad = Vec3::ZERO;
        let mut cavity = 0.0;
        // Pores: fine on the forehead, medium on the cheeks, big on the nose.
        let bare = (1.0 - g.lips) * (1.0 - 0.75 * g.under_eye);
        let sizes = [(0.0007, 0.000035, 0.5 + 0.6 * g.forehead + 0.3 * g.chin), (0.00095, 0.00006, 0.35 + 0.8 * g.cheek + 0.3 * g.chin), (0.0012, 0.00008, 1.2 * g.nose + 0.4 * g.cheek)];
        for (k, (cell, depth, w)) in sizes.iter().enumerate() {
            let amount = self.pores * w * bare;
            if amount <= 1e-3 {
                continue;
            }
            let (f, r) = worley(p, *cell, self.seed ^ (0x1000 + k as u32));
            if r > 0.85 {
                continue;
            }
            let radius = cell * (0.17 + 0.16 * r);
            let (h, grad) = pit(p, f, n, radius);
            height_grad += grad * (depth * amount);
            cavity += -h * amount.min(1.0) * (0.06 + 0.08 * k as f32 / 2.0);
        }
        // Expression lines.
        let (lh, lg) = self.lines_height(q, p, false);
        height_grad += self.right * (lg.x * sx) + self.up * lg.y;
        cavity += (-lh / 0.0002).clamp(0.0, 1.0) * 0.6;
        // Micro cross-hatch: two families of fine diagonal lines, broken up.
        let hatch = [(self.right + self.up).normalize(), (self.right - self.up * 0.8).normalize(), (self.up + self.forward * 0.6).normalize()];
        for (k, dir) in hatch.iter().enumerate() {
            let spacing = [0.00045, 0.0006, 0.0005][k];
            let phase = p.dot(*dir) / spacing + 2.5 * noise(p / (spacing * 3.0) + Vec3::splat(k as f32 * 3.1));
            let s = (std::f32::consts::PI * phase).sin();
            let f = (1.0 - s.abs()).powi(4);
            let breaks = ss(0.45, 0.75, noise(p / (spacing * 2.0) + Vec3::splat(11.0 + k as f32)));
            let a = 0.000014 * self.micro * breaks * (1.0 - g.lips);
            // d/dphase of -(1 - |s|)^4.
            let df = 4.0 * (1.0 - s.abs()).powi(3) * s.signum() * (std::f32::consts::PI * phase).cos() * std::f32::consts::PI;
            height_grad += *dir * (a * df / spacing);
            cavity += f * breaks * 0.08 * self.micro;
        }
        // Skin cells: a soft irregular bump under everything.
        {
            let e = 0.00004;
            let b = |p: Vec3| noise(p / 0.00022) * 0.7 + noise(p / 0.00009) * 0.3;
            let grad = Vec3::new(b(p + Vec3::X * e) - b(p - Vec3::X * e), b(p + Vec3::Y * e) - b(p - Vec3::Y * e), b(p + Vec3::Z * e) - b(p - Vec3::Z * e)) / (2.0 * e);
            height_grad += grad * (0.000012 * (1.0 - 0.6 * g.lips) * (0.6 + 0.4 * self.pores));
        }
        // Lip lines: fine, irregular grooves running across each lip,
        // fanning a little from the mouth's middle and breaking up.
        let mut lip_gloss = 0.0;
        if g.lips > 0.01 {
            let spacing = 0.0009 * (0.8 + 0.4 * noise(p / 0.004 + Vec3::splat(5.0)));
            let phase = q.x / spacing + 0.8 * noise(p / 0.0025) + (q.y - self.stomion_y) * q.x / (spacing * 0.04);
            let s = (std::f32::consts::PI * phase).sin();
            let f = (1.0 - s.abs()).powi(10);
            let mid = ss(0.1, 0.35, g.lip_r) * (1.0 - ss(0.6, 0.95, g.lip_r));
            let broken = ss(0.15, 0.45, noise(p / 0.0013 + Vec3::splat(17.0)));
            let a = 0.00008 * g.lips * mid * broken;
            let df = 10.0 * (1.0 - s.abs()).powi(9) * s.signum() * (std::f32::consts::PI * phase).cos() * std::f32::consts::PI;
            height_grad += self.right * (sx * a * df / spacing);
            cavity += f * mid * broken * g.lips * 0.6;
            lip_gloss = g.lips * (0.45 + 0.55 * ss(0.35, 0.75, noise(p / 0.0012)));
        }
        // Moles stand a little proud.
        let mut mole = 0.0f32;
        for m in &self.moles {
            let d = Vec2::new(q.x * sx, q.y).distance(Vec2::new(m[0], m[1]));
            let t = 1.0 - ss(m[2] * 0.7, m[2], d);
            mole = mole.max(t * g.face.max(0.3));
        }
        let bumped = (n - (height_grad - n * height_grad.dot(n))).normalize_or(n);

        // Color.
        let mut c = color;
        let blotch = 0.7 + 0.6 * noise(p / 0.006 + Vec3::splat(self.seed as f32 * 0.37 % 100.0));
        let red = (self.redness * (0.9 * g.cheek + 1.0 * g.nose + 0.5 * g.chin + 0.9 * g.ear) * blotch).min(1.0);
        c *= Vec3::ONE.lerp(Vec3::new(1.03, 0.82, 0.83), red);
        c *= Vec3::ONE.lerp(Vec3::new(0.86, 0.80, 0.90), self.under_eye * g.under_eye);
        let mottle = noise(p / 0.003) * 0.6 + noise(p / 0.011) * 0.4 - 0.5;
        c *= Vec3::ONE + Vec3::new(0.06, -0.10, -0.12) * mottle * 0.6;
        // Freckles and age spots: blotchy brown spots on sunny skin.
        let spots = self.freckles + self.age_spots * 0.5;
        if spots > 0.0 && g.sun > 0.01 {
            for (k, (cell, size)) in [(0.0028, 0.3), (0.0045, 0.38)].iter().enumerate() {
                let (f, r) = worley(p, *cell, self.seed ^ (0x2000 + k as u32));
                let chance = g.sun * if k == 0 { self.freckles * 0.8 } else { self.age_spots * 0.12 + self.freckles * 0.2 };
                if r < chance {
                    let rr = cell * size * (0.35 + 0.4 * (r * 7.31).fract());
                    let edge = rr * (0.75 + 0.5 * noise(p / (rr * 0.8)));
                    let t = 1.0 - ss(edge * 0.6, edge, along(p, f, n).length());
                    let strength = 0.2 + 0.35 * (r / chance.max(1e-3));
                    c *= Vec3::ONE.lerp(Vec3::new(0.82, 0.70, 0.62), t * strength);
                }
            }
        }
        c *= Vec3::ONE.lerp(Vec3::new(0.45, 0.33, 0.28), mole * 0.9);
        // Stubble: cut hairs as dark dots and an overall beard shadow.
        if self.stubble > 0.0 && g.beard > 0.01 {
            let (f, r) = worley(p, 0.0009, self.seed ^ 0x3000);
            let dot = 1.0 - ss(0.00006, 0.00012, along(p, f, n).length());
            let a = self.stubble * g.beard;
            c *= Vec3::ONE.lerp(Vec3::new(0.76, 0.71, 0.76), a * 0.6);
            if r < 0.8 {
                c *= Vec3::ONE.lerp(Vec3::new(0.18, 0.15, 0.14), dot * a);
            }
        }
        // Lips: a rose vermilion, darker and redder deep in, darkest at the
        // meeting line, with a pale rim just outside the border.
        let lip_base = if self.lip_color == Vec3::ZERO { color * Vec3::new(0.95, 0.74, 0.8).lerp(Vec3::new(0.93, 0.78, 0.8), self.sex) } else { self.lip_color };
        let rim = (1.0 - ss(0.0, 0.15, (g.lip_r - 1.04).abs())) * ss(0.2, 0.6, self.lips) * (1.0 - ss(0.9, 1.0, q.x / self.mouth.x.max(1e-4)));
        c *= 1.0 + 0.05 * rim * g.face;
        let lip = lip_base * (0.95 + 0.06 * g.lip_depth) * (0.72 + 0.28 * ss(0.0, 0.3, g.lip_r)) * (1.0 - 0.06 * ss(0.7, 1.0, g.lip_r));
        // Fine vertical streaks of color along the lip lines.
        let streak = 0.9 + 0.2 * noise(Vec3::new(q.x / 0.0007, q.y / 0.004, 3.0));
        c = c.lerp(lip * streak, g.lips);
        // Crevices hold shadow and color.
        c *= 1.0 - 0.22 * cavity.min(1.0);

        // Shine: a slick T-zone, matte cheeks, rougher pores, moist lips.
        let mut r = rough - 0.06 * g.face + 0.03 * g.cheek - 0.12 * self.oil * g.tzone + 0.18 * cavity.min(1.0);
        r = r + (0.34 - r) * lip_gloss;
        let coat = (self.oil * g.tzone * 0.3 * (0.6 + 0.8 * noise(p / 0.004)) + lip_gloss * 0.25).clamp(0.0, 1.0);
        let sheen = self.fuzz * 0.18 * g.face * (1.0 - g.lips);
        // Fade out over the neck where a head clay overlaps the body.
        let keep = self.keep(p);
        Look { n: n.lerp(bumped, keep).normalize_or(n), color: color.lerp(c.clamp(Vec3::ZERO, Vec3::ONE), keep), rough: rough + (r.clamp(0.08, 1.0) - rough) * keep, coat: coat * keep, sheen: sheen * keep }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(age: f32, skin: Skin) -> (Face, Landmarks) {
        let h = crate::human::Human { age, ..Default::default() };
        let body = h.build().unwrap();
        let l = Landmarks::of(&body.points, &body).unwrap();
        (Face::new(&l, &skin, age, 0.0), l)
    }

    #[test]
    fn landmarks_sit_where_faces_have_them() {
        let (_, l) = face(30.0, Skin::default());
        let eye_y = l.eyes[0].y;
        assert!(l.eyes[0].x > 0.02 && l.eyes[1].x < -0.02);
        assert!(l.nose_tip.z > l.eyes[0].z + 0.02, "nose in front of the eyes");
        assert!(l.nose_tip.y < eye_y && l.stomion.y < l.nose_tip.y && l.chin.y < l.stomion.y);
        assert!(l.lip_top.y > l.stomion.y && l.lip_bottom.y < l.stomion.y);
        assert!((l.mouth[0].x + l.mouth[1].x).abs() < 1e-3 && l.mouth[0].x > 0.015);
        assert!(l.ear.x > 0.06 && l.ear.z < l.eyes[0].z);
        assert!(l.ala.x > 0.01 && l.ala.x < l.mouth[0].x);
    }

    #[test]
    fn regions_land_on_the_right_parts() {
        let (f, l) = face(35.0, Skin::default());
        let on = |p: Vec3| f.regions(p);
        let nose = on(l.nose_tip);
        assert!(nose.nose > 0.8 && nose.lips < 0.05 && nose.tzone > 0.8, "{nose:?}");
        let lip = on((l.stomion + l.lip_top) * 0.5);
        assert!(lip.lips > 0.8 && lip.nose < 0.1, "{lip:?}");
        let cheek = on(Vec3::new(l.eyes[0].x + 0.006, (l.eyes[0].y + l.nose_tip.y) * 0.5 - 0.004, l.nose_tip.z - 0.03));
        assert!(cheek.cheek > 0.5 && cheek.lips < 0.01, "{cheek:?}");
        let forehead = on(Vec3::new(0.0, l.eyes[0].y + 0.05, l.nasion.z));
        assert!(forehead.forehead > 0.8 && forehead.tzone > 0.6, "{forehead:?}");
        assert!(on(l.ear).ear > 0.8 && on(l.ear).face < 0.2);
        let under = on(Vec3::new(l.eyes[0].x, l.eyes[0].y - 0.016, l.eyes[0].z + 0.014));
        assert!(under.under_eye > 0.6, "{under:?}");
        // Mirrored.
        let p = Vec3::new(0.03, 1.5, 0.14);
        assert!((on(p).cheek - on(Vec3::new(-0.03, 1.5, 0.14)).cheek).abs() < 1e-4);
        // Beard below the cheeks, not on the forehead or lips.
        assert!(on(l.chin).beard > 0.5 && forehead.beard < 0.01 && lip.beard < 0.05);
    }

    #[test]
    fn wrinkles_deepen_with_age_and_follow_lines() {
        let (young, _) = face(22.0, Skin::default());
        let (old, _) = face(75.0, Skin::default());
        let depth = |f: &Face| f.lines.iter().map(|l| l.depth).sum::<f32>();
        assert!(depth(&old) > depth(&young) * 2.0);
        // Old faces get frown, marionette and upper-lip lines.
        assert!(old.lines.len() > young.lines.len());
        // A forehead line is a groove: lower on it than beside it.
        let l = &old.lines[0];
        let mid = l.points[3];
        let at = |q: Vec2| old.lines_height(q, Vec3::ZERO, false).0;
        assert!(at(mid) < at(mid + Vec2::new(0.0, l.width * 3.0)) - 1e-6);
        // Geometry folds are broader.
        let fold = old.lines.iter().find(|l| l.fold).unwrap();
        let beside = fold.points[3] + Vec2::new(fold.width * 2.0, 0.0);
        assert!(old.lines_height(beside, Vec3::ZERO, true).0.abs() > old.lines_height(beside, Vec3::ZERO, false).0.abs());
        let none = Face::new(&face(75.0, Skin::default()).1, &Skin { wrinkles: 0.0, ..Skin::default() }, 75.0, 0.0);
        assert!(none.lines.is_empty());
    }

    #[test]
    fn shading_is_repeatable_bounded_and_regional() {
        let (f, l) = face(45.0, Skin { freckles: 0.6, moles: 0.5, stubble: 0.5, ..Skin::default() });
        let n = Vec3::Z;
        let c = Vec3::new(0.8, 0.62, 0.52);
        let mut samples = Vec::new();
        for i in 0..400 {
            let p = l.nose_tip + Vec3::new((i % 20) as f32 * 0.0002, (i / 20) as f32 * 0.0002, 0.0);
            let a = f.shade(p, n, c, 0.48);
            assert_eq!(a, f.shade(p, n, c, 0.48));
            assert!((a.n.length() - 1.0).abs() < 1e-4 && a.n.dot(n) > 0.3);
            assert!(a.color.min_element() >= 0.0 && a.color.max_element() <= 1.0 && a.rough >= 0.08 && a.rough <= 1.0);
            samples.push(a);
        }
        // Pores make the nose bumpy.
        assert!(samples.iter().filter(|s| s.n.dot(n) < 0.995).count() > 20);
        // The T-zone is shinier than the cheek.
        let cheek = Vec3::new(l.eyes[0].x + 0.006, (l.eyes[0].y + l.nose_tip.y) * 0.5 - 0.004, l.nose_tip.z - 0.03);
        let avg = |p0: Vec3| {
            let v: Vec<Look> = (0..64).map(|i| f.shade(p0 + Vec3::new((i % 8) as f32 * 0.0003, (i / 8) as f32 * 0.0003, 0.0), n, c, 0.48)).collect();
            (v.iter().map(|s| s.rough).sum::<f32>() / 64.0, v.iter().map(|s| s.coat).sum::<f32>() / 64.0, v.iter().map(|s| s.color).sum::<Vec3>() / 64.0)
        };
        let (nr, nc, ncol) = avg(l.nose_tip);
        let (cr, cc, _) = avg(cheek);
        assert!(nr < cr && nc > cc, "nose {nr} {nc} cheek {cr} {cc}");
        // The nose is redder than plain skin; lips are rose.
        assert!(ncol.x / ncol.y > c.x / c.y);
        let lip = f.shade((l.stomion + l.lip_top) * 0.5, n, c, 0.48);
        assert!(lip.color.x / lip.color.y > c.x / c.y * 1.1 && lip.coat > 0.1, "{lip:?}");
    }
}

#[cfg(test)]
mod clay_tests {
    use crate::clay::Clay;

    #[test]
    fn a_human_head_mesh_carries_its_face() {
        let clay: Clay = serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "human", "part": "head", "human": { "sex": 0.0 }, "blend": 0, "material": "skin" }], "resolution": 96 })).unwrap();
        let mesh = clay.build_part(None, &[], "skin");
        let face = mesh.face.as_ref().expect("face");
        assert!(face.lines.len() > 5);
        let body: Clay = serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "human", "part": "body", "blend": 0 }], "resolution": 64 })).unwrap();
        assert!(body.build_part(None, &[], "skin").face.is_none());
    }
}
