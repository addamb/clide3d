//! Fur over a clay's body (character follow-ups, feature 3): a Hair whose
//! guides are short hairs spread evenly over the surface (`grow: {fur:
//! true}`, `app/fur.rs`), combed along a flow field. The scalp model the
//! other hair uses (a patch of directions from one centre) cannot hold a
//! body with limbs, so fur grows its film strands differently: each
//! strand belongs to the guide nearest its root (the guide's Voronoi cell
//! on the surface, so the roots cover the body evenly with no bald
//! gaps), is placed on the clay's surface by its field, takes the clay's
//! colour there, copies its guide's shape turned onto its own normal,
//! and is kept out of the clay as other strands are. Excluded parts
//! (blobs by name, spheres, the eyes) grow none, and no strand enters an
//! eye. The viewport shows one short card per guide lying on the
//! surface, coloured as the clay there.
//!
//! Every strand is a pure function of its index (its own random stream),
//! so the strands are the same on any thread count.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::{Cells, Guide, Hair, KEEP_OUT_CHUNK, Rng, Strand};
use crate::mesh::{Mesh, Vertex};

/// Most names or spheres in each of fur's lists.
pub const MAX_LIST: usize = 64;
/// Most film strands one guide grows (the spiral of candidates is four
/// times this at most): the budget over the guides, bounded.
const MAX_PER_GUIDE: usize = 1 << 16;

/// What makes a hair fur, and how its strands are grown (stored; `grow`
/// fills it).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Fur {
    /// The guides' mean root spacing (the hair's units), measured at
    /// grow: a strand's root is at most this far from its guide's.
    pub spacing: f32,
    /// Multiplies the clay's colour at each root (linear).
    pub tint: [f32; 3],
    /// The tips' colour (sRGB) mixed in by `tip_mix`; None: the root's
    /// colour, a little lighter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_color: Option<[f32; 3]>,
    pub tip_mix: f32,
    /// Blobs whose surface grows no fur (the blob making the surface at
    /// the root, as masks see it).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// When not empty, only these blobs' surface grows fur.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Spheres [x, y, z, radius] (the hair's space) with no roots in them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bare: Vec<[f32; 4]>,
    /// Spheres no strand may enter (the eyes): a strand that would is
    /// left out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<[f32; 4]>,
    /// Each strand turns from its guide's direction by up to this many
    /// degrees about the surface normal.
    pub jitter: f32,
    /// Long fur that hangs (`grow.drape`, 0..1): the guides were hung
    /// under gravity over the body (`drape_guide`). 0: short fur, its
    /// strands made as before.
    #[serde(skip_serializing_if = "is_zero")]
    pub drape: f32,
    /// The ground under the clay (the hair's space): a plane [nx, ny, nz,
    /// offset], strands kept on its side (`n·p >= offset`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor: Option<[f32; 4]>,
    /// Long fur's locks (`drape` above 0), 0..1: how far each strand
    /// gathers toward its lock's guide toward the tip (0: strands blend
    /// their neighbouring guides evenly).
    #[serde(skip_serializing_if = "is_zero")]
    pub clump: f32,
    /// A lock's width in guide spacings (1..8): neighbouring guides are
    /// gathered into locks this wide, each guide a smaller lock inside
    /// its lock.
    #[serde(skip_serializing_if = "is_default_clump_size")]
    pub clump_size: f32,
    /// A parting (the hair's space): a plane [nx, ny, nz, offset]; long
    /// strands blend, and gather into locks with, only guides on their
    /// own side of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<[f32; 4]>,
    /// Long fur's wave: how far each lock swings to and fro across
    /// itself (the hair's units; 0: straight). The whole lock waves
    /// together, each lock in its own phase.
    #[serde(skip_serializing_if = "is_zero")]
    pub wave: f32,
    /// Waves along a lock's length.
    #[serde(skip_serializing_if = "is_default_waves")]
    pub waves: f32,
    /// Long fur's ragged ends, 0..1: each lock (and each guide's smaller
    /// lock in it) stops up to this share short of its length.
    #[serde(skip_serializing_if = "is_zero")]
    pub ragged: f32,
    /// Eyelids that grow fur (`grow.lid_length`): very short fur lying on
    /// each lid's cap toward its margin and stopping short of it, so the
    /// lid reads as part of the furred face and its edge stays a clean
    /// line. Their roots are exempt from their own eye's bare sphere.
    /// Empty: lids are bare (fur grown before this, or `lid_length: 0`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lids: Vec<FurLid>,
    /// `grow.lengths` and `grow.direction` regions, applied to each short
    /// strand by its own root (not only to the guides rooted in them):
    /// a strand rooted in one takes its length and lies its way, so a
    /// small part such as a brow, which few guides land on, grows its own
    /// fur. Empty: strands copy their guide (fur grown before this).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<FurRegion>,
    /// With length regions: the length (the hair's units) of a short
    /// strand no region holds, whatever its guide's (a guide rooted in a
    /// region lends its length to no strand outside it). 0: unset.
    #[serde(skip_serializing_if = "is_zero")]
    pub length: f32,
}

/// A region of `Fur::regions` (the hair's space): blobs by name (the
/// blob making the surface at the root) or a sphere [x, y, z, radius],
/// with a length or a direction (or both: the first region holding a
/// root for each wins).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct FurRegion {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blobs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sphere: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f32; 3]>,
}

impl FurRegion {
    fn holds(&self, q: Vec3, head: Option<&dyn crate::groom::Collider>) -> bool {
        match self.sphere {
            Some(s) => q.distance(Vec3::new(s[0], s[1], s[2])) < s[3],
            None => head.is_some_and(|h| h.owned_by(q, &self.blobs)),
        }
    }
}

/// Most lids fur keeps (`Fur::lids`).
pub const MAX_LIDS: usize = 16;
/// Most strands a lid root grows (`FurLid::copies`).
pub const MAX_LID_COPIES: u32 = 4;
/// A lid's fur length against the body's when not asked (`grow.lid_length`),
pub const LID_LENGTH: f32 = 0.35;
/// ... but at most this much of the lid's radius (long coats).
pub const LID_SIZE: f32 = 0.06;

/// A lid's cap as fur sees it (the hair's space): the lid blob's sphere
/// (`clay::lid_geometry`), its axis (the cap is on the axis' side, its
/// margin where `(p - center)·axis = 0`), its middle radius and half
/// thickness, how far out its surface reaches (the blend into the face)
/// and its fur.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct FurLid {
    pub center: [f32; 3],
    pub axis: [f32; 3],
    pub radius: f32,
    pub half: f32,
    pub reach: f32,
    /// The fur's length on it.
    pub length: f32,
    /// Strands each root grows: short fur covers less skin than the
    /// body's, so a lid root grows a few, side by side.
    pub copies: u32,
}

impl FurLid {
    fn c(&self) -> Vec3 {
        Vec3::from(self.center)
    }

    fn n(&self) -> Vec3 {
        Vec3::from(self.axis)
    }

    /// Whether a root at `q` grows this lid's fur: on the outside of its
    /// cap (the axis' side, at or past the shell's middle: never the
    /// hollow over the eye nor the margin's bead), within its reach.
    pub fn holds(&self, q: Vec3) -> bool {
        let d = q - self.c();
        let r = d.length();
        d.dot(self.n()) >= 0.0 && r >= self.radius && r <= self.reach
    }

    /// Whether a bare sphere [x, y, z, r] is this lid's eye's (centred
    /// on the lid): the lid's own roots are not bared by it.
    fn bares_its_eye(&self, s: &[f32; 4]) -> bool {
        Vec3::new(s[0], s[1], s[2]).distance(self.c()) < 0.5 * self.radius
    }

    /// A strand (or guide) from root `q`: it lies on the cap, along the
    /// great circle down toward the margin turned `turn` radians about
    /// the root's radius, `scale` of the lid's length but stopping a half
    /// thickness above the margin's plane (the bead stays bare: a clean
    /// line), rising a little off the skin in the middle; `lift` off the
    /// surface. None where there is no room (by the margin: under a
    /// quarter of the length).
    pub fn strand(&self, q: Vec3, turn: f32, scale: f32, segments: usize, lift: f32) -> Option<Vec<Vec3>> {
        let (c, n) = (self.c(), self.n());
        let d = q - c;
        let r = d.length();
        if r < 1e-9 {
            return None;
        }
        let u = d / r;
        let room = r * ((d.dot(n) - self.half) / r).clamp(-1.0, 1.0).asin();
        let l = (self.length * scale).min(0.9 * room);
        if l < 0.25 * self.length {
            return None;
        }
        let down = -(n - u * u.dot(n));
        let t = Quat::from_axis_angle(u, turn) * down.normalize_or(u.any_orthonormal_vector());
        let segments = segments.max(2);
        Some(
            (0..=segments)
                .map(|k| {
                    let s = k as f32 / segments as f32;
                    let a = s * l / r;
                    let h = lift + 0.1 * l * (std::f32::consts::PI * s).sin();
                    c + (u * a.cos() + t * a.sin()) * (r + h)
                })
                .collect(),
        )
    }
}

/// `Fur::waves`' default.
pub const WAVES: f32 = 2.5;

fn is_default_waves(v: &f32) -> bool {
    *v == WAVES
}

/// `Fur::clump_size`'s default.
pub const CLUMP_SIZE: f32 = 3.0;

fn is_default_clump_size(v: &f32) -> bool {
    *v == CLUMP_SIZE
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Default for Fur {
    fn default() -> Self {
        Fur { spacing: 0.01, tint: [1.0; 3], tip_color: None, tip_mix: 0.5, exclude: Vec::new(), include: Vec::new(), bare: Vec::new(), avoid: Vec::new(), jitter: 12.0, drape: 0.0, floor: None, clump: 0.0, clump_size: CLUMP_SIZE, part: None, wave: 0.0, waves: WAVES, ragged: 0.0, lids: Vec::new(), regions: Vec::new(), length: 0.0 }
    }
}

impl Fur {
    /// Bounds what a file or a patch could set.
    pub fn sanitize(&mut self) {
        let fix = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Fur::default();
        self.spacing = fix(self.spacing, 1e-5, 10.0, d.spacing);
        self.tint = self.tint.map(|v| fix(v, 0.0, 4.0, 1.0));
        self.tip_color = self.tip_color.map(|c| c.map(|v| fix(v, 0.0, 1.0, 0.5)));
        self.tip_mix = fix(self.tip_mix, 0.0, 1.0, d.tip_mix);
        self.jitter = fix(self.jitter, 0.0, 90.0, d.jitter);
        self.drape = fix(self.drape, 0.0, 1.0, 0.0);
        self.clump = fix(self.clump, 0.0, 1.0, 0.0);
        self.clump_size = fix(self.clump_size, 1.0, 8.0, CLUMP_SIZE);
        self.wave = fix(self.wave, 0.0, 1.0, 0.0);
        self.waves = fix(self.waves, 0.0, 16.0, WAVES);
        self.ragged = fix(self.ragged, 0.0, 0.9, 0.0);
        self.part = self.part.filter(|f| f.iter().all(|v| v.is_finite()) && Vec3::new(f[0], f[1], f[2]).length() > 1e-6).map(|f| {
            let n = Vec3::new(f[0], f[1], f[2]);
            let l = n.length();
            [f[0] / l, f[1] / l, f[2] / l, f[3] / l]
        });
        self.floor = self.floor.filter(|f| f.iter().all(|v| v.is_finite()) && Vec3::new(f[0], f[1], f[2]).length() > 1e-6).map(|f| {
            let n = Vec3::new(f[0], f[1], f[2]);
            let l = n.length();
            [f[0] / l, f[1] / l, f[2] / l, f[3] / l]
        });
        for list in [&mut self.exclude, &mut self.include] {
            list.truncate(MAX_LIST);
            list.retain(|n| n.len() <= 256);
        }
        for list in [&mut self.bare, &mut self.avoid] {
            list.truncate(MAX_LIST);
            list.retain(|s| s.iter().all(|v| v.is_finite()) && s[3] > 0.0);
        }
        self.length = fix(self.length, 0.0, 3.0, 0.0);
        self.regions.truncate(2 * MAX_LIST);
        self.regions.retain(|r| {
            r.blobs.len() <= MAX_LIST
                && r.blobs.iter().all(|n| n.len() <= 256)
                && r.sphere.is_none_or(|s| s.iter().all(|v| v.is_finite()) && s[3] > 0.0)
                && (r.sphere.is_some() || !r.blobs.is_empty())
                && r.length.is_none_or(|l| l.is_finite() && l > 0.0 && l <= 3.0)
                && r.direction.is_none_or(|d| d.iter().all(|v| v.is_finite()) && Vec3::from(d).length() > 1e-6)
        });
        self.lids.truncate(MAX_LIDS);
        self.lids.retain(|l| {
            let n = Vec3::from(l.axis);
            l.center.iter().chain(&l.axis).chain(&[l.radius, l.half, l.reach, l.length]).all(|v| v.is_finite()) && n.length() > 1e-6 && l.radius > 0.0 && l.half >= 0.0 && l.length > 0.0
        });
        for l in &mut self.lids {
            l.axis = Vec3::from(l.axis).normalize().into();
            l.reach = l.reach.max(l.radius);
            l.length = l.length.min(3.0);
            l.copies = l.copies.clamp(1, MAX_LID_COPIES);
        }
    }

    fn in_any(spheres: &[[f32; 4]], p: Vec3) -> bool {
        spheres.iter().any(|s| p.distance(Vec3::new(s[0], s[1], s[2])) < s[3])
    }
}

/// A guide's root and normal (from the surface when there is one, else
/// from the guide's own rise off the middle of all roots).
fn frames(guides: &[&Guide], head: Option<&dyn crate::groom::Collider>) -> Vec<(Vec3, Vec3)> {
    let middle = guides.iter().map(|g| g.points[0]).sum::<Vec3>() / guides.len().max(1) as f32;
    guides
        .iter()
        .map(|g| {
            let root = g.points[0];
            let n = head.and_then(|h| h.snap(root).map(|(_, n)| n).or_else(|| h.normal(root))).unwrap_or_else(|| (root - middle).normalize_or(Vec3::Y));
            (root, n)
        })
        .collect()
}

/// A hash of the seed and an index: each strand's own random stream.
fn stream(seed: u32, index: u32) -> Rng {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ index.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    Rng(x.max(1))
}

/// Two directions square to `n` and each other.
fn tangents(n: Vec3) -> (Vec3, Vec3) {
    let a = n.any_orthonormal_vector();
    (a, n.cross(a))
}

/// What fur's strands came to: how many each step left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FurCounts {
    /// Strands asked for (the hair's budget).
    pub asked: usize,
    /// Left out: roots on excluded parts, in bare spheres or off the surface.
    pub excluded: usize,
    /// Left out: strands that would enter an eye (`avoid`).
    pub avoided: usize,
    /// Kept out of the clay (moved).
    pub moved: usize,
    /// Left out: long strands wedged between the body and the ground.
    pub wedged: usize,
}

impl Hair {
    /// Fur's film strands (see the module): each root in its guide's
    /// cell, on the surface, coloured from it, the guide's shape turned
    /// onto it, kept out of `head`. Returns the strands and the counts.
    pub fn fur_strands(&self, fur: &Fur, head: Option<&dyn crate::groom::Collider>) -> (Vec<Strand>, FurCounts) {
        let guides: Vec<&Guide> = self.guides.iter().filter(|g| g.points.len() >= 2 && g.points.iter().all(|p| p.is_finite())).collect();
        let n = self.strands as usize;
        let mut counts = FurCounts { asked: n, ..Default::default() };
        if guides.is_empty() || n == 0 {
            return (Vec::new(), counts);
        }
        let frames = frames(&guides, head);
        let spacing = fur.spacing.max(1e-5);
        let index = Cells::new(spacing, frames.iter().map(|f| f.0).enumerate());
        let srgb = |c: Vec3| c.clamp(Vec3::ZERO, Vec3::ONE).powf(2.2);
        let tint = Vec3::from(fur.tint);
        let (lift, lv, frizz) = (self.lift, self.length_variation, self.frizz);
        let jitter = fur.jitter.to_radians();
        // Roots: per guide, a sunflower spiral (golden-angle steps, even
        // by area) over a disc a spacing wide round its root, turned its
        // own way, keeping the points whose nearest root is this guide's:
        // exactly its cell, so the strands cover the surface evenly (each
        // cell gets strands by its area) with no gap between cells.
        let per = n as f32 / guides.len() as f32;
        // A cell is about 0.87 spacing² (hexagonal packing) of the disc's
        // pi spacing²: the spiral holds a little more than it needs.
        let spiral = ((per / 0.25).ceil() as usize).clamp(1, 4 * MAX_PER_GUIDE);
        let disc = spacing;
        let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
        let mut cand: Vec<Option<Vec3>> = vec![None; guides.len() * spiral];
        crate::clay::par_chunks(&mut cand, spiral * 8, |c, chunk| {
            for (k, slot) in chunk.iter_mut().enumerate() {
                let i = c * spiral * 8 + k;
                let (g, j) = (i / spiral, i % spiral);
                let (root, normal) = frames[g];
                let (t1, t2) = tangents(normal);
                let turn = stream(self.seed ^ 0xA5A5_5A5A, g as u32).next() * std::f32::consts::TAU;
                let r = disc * ((j as f32 + 0.5) / spiral as f32).sqrt();
                let a = j as f32 * golden + turn;
                let q = root + (t1 * a.cos() + t2 * a.sin()) * r;
                let mine = q.distance_squared(root);
                if index.each(q, mine.sqrt(), |o| o == g || frames[o].0.distance_squared(q) >= mine) {
                    *slot = Some(q);
                }
            }
        });
        let mut roots: Vec<(usize, Vec3)> = cand.iter().enumerate().filter_map(|(i, q)| q.map(|q| (i / spiral, q))).collect();
        // More than the budget: an even share of every guide's, dropped
        // evenly through the list (never whole guides).
        if roots.len() > n {
            let m = roots.len();
            roots = roots.into_iter().enumerate().filter(|(j, _)| (j + 1) * n / m != j * n / m).map(|(_, r)| r).collect();
        }
        let locks = (fur.drape > 0.0).then(|| Locks::new(&guides, &frames, spacing, fur, self.seed, self.melanin_variation));
        // The strands' own spacing: a lid root's strands stand this far
        // apart round it.
        let strand_gap = spacing / per.max(1.0).sqrt();
        let colors = |q: Vec3| {
            let color = head.and_then(|h| h.color(q)).unwrap_or(Vec3::from(self.root_color));
            let root_c = (srgb(color) * tint).min(Vec3::ONE);
            let tip_c = match fur.tip_color {
                Some(t) => root_c.lerp(srgb(Vec3::from(t)), fur.tip_mix),
                None => (root_c * 1.12).min(Vec3::ONE),
            };
            (root_c, tip_c)
        };
        let make = |i: usize| -> Result<Vec<Strand>, bool> {
            let mut rng = stream(self.seed, i as u32);
            let (g, p) = roots[i];
            let (_, normal) = frames[g];
            let (t1, t2) = tangents(normal);
            let (q, nq) = match head {
                Some(h) => h.snap(p).ok_or(false)?,
                None => (p, normal),
            };
            // A root on a furred lid's cap is not bared by its own eye.
            let lid = fur.lids.iter().find(|l| l.holds(q));
            if q.distance(p) > spacing || fur.bare.iter().any(|s| Fur::in_any(std::slice::from_ref(s), q) && !lid.is_some_and(|l| l.bares_its_eye(s))) {
                return Err(false);
            }
            if let Some(h) = head
                && (h.owned_by(q, &fur.exclude) || (!fur.include.is_empty() && !h.owned_by(q, &fur.include)))
            {
                return Err(false);
            }
            if let Some(lid) = lid {
                // Short fur lying on the lid toward its margin, a few
                // strands side by side round the root.
                let segments = guides[g].points.len().saturating_sub(1).clamp(2, 8);
                let mut out = Vec::new();
                for k in 0..lid.copies {
                    let at = if k == 0 {
                        q
                    } else {
                        let a = rng.next() * std::f32::consts::TAU;
                        let off = q + (t1 * a.cos() + t2 * a.sin()) * (strand_gap * rng.next().sqrt());
                        match head {
                            Some(h) => match h.snap(off) {
                                Some((x, _)) if lid.holds(x) => x,
                                _ => continue,
                            },
                            None => off,
                        }
                    };
                    let turn = (rng.next() - 0.5) * 2.0 * jitter;
                    let scale = 1.0 - lv * rng.next().powf(1.5);
                    let Some(points) = lid.strand(at, turn, scale, segments, lift * 1.25) else { continue };
                    let (root_c, tip_c) = colors(at);
                    let width = self.strand_width * (0.7 + 0.6 * rng.next());
                    out.push(Strand { points, root_width: width, tip_width: width * 0.15, root_color: root_c, tip_color: tip_c });
                }
                return if out.is_empty() { Err(false) } else { Ok(out) };
            }
            let (root_c, tip_c) = colors(q);
            if let Some(locks) = &locks {
                // Long fur: the neighbouring guides' shapes blended and
                // moved onto this root (never turned: a long lever would
                // swing the tip), gathered into its locks.
                let start = q + nq * (lift * 1.25);
                let (points, shade) = locks.strand(g, start, nq, &index, &mut rng, lv, frizz);
                let width = self.strand_width * (0.7 + 0.6 * rng.next());
                return Ok(vec![Strand { points, root_width: width, tip_width: width * 0.3, root_color: (root_c * shade).min(Vec3::ONE), tip_color: (tip_c * shade).min(Vec3::ONE) }]);
            }
            // The guide's shape, turned onto this root's normal and a
            // little about it, a little shorter or not.
            let mut turn = Quat::from_axis_angle(nq, (rng.next() - 0.5) * 2.0 * jitter) * Quat::from_rotation_arc(normal, nq);
            let mut scale = 1.0 - lv * rng.next().powf(1.5);
            let gp = &guides[g].points;
            // A region holding this root: its length (the guide's shape
            // scaled to it) and its way (the shape turned about the
            // normal to lie along it), whatever guide the root is near.
            if !fur.regions.is_empty() {
                let want = fur.regions.iter().find(|r| r.length.is_some() && r.holds(q, head)).and_then(|r| r.length).or((fur.length > 0.0).then_some(fur.length));
                if let Some(l) = want {
                    let own: f32 = gp.windows(2).map(|w| w[0].distance(w[1])).sum();
                    scale *= l / own.max(1e-9);
                }
                if let Some(d) = fur.regions.iter().find(|r| r.direction.is_some() && r.holds(q, head)).and_then(|r| r.direction) {
                    let flat = |v: Vec3| (v - nq * nq.dot(v)).try_normalize();
                    if let (Some(from), Some(to)) = (flat(turn * (gp[gp.len() - 1] - gp[0])), flat(Vec3::from(d))) {
                        turn = Quat::from_rotation_arc(from, to) * turn;
                    }
                }
            }
            // A little past the lift, so the root is not pushed out again.
            let start = q + nq * (lift * 1.25);
            let (phase, side) = (rng.next() * std::f32::consts::TAU, (t1 * rng.next() + t2 * rng.next() - (t1 + t2) * 0.5).normalize_or(t1));
            let last = (gp.len() - 1) as f32;
            let points: Vec<Vec3> = gp
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let t = k as f32 / last;
                    let bend = side * (frizz * t * t * (t * 3.0 + phase).sin());
                    start + turn * ((*p - gp[0]) * scale) + turn * bend
                })
                .collect();
            let width = self.strand_width * (0.7 + 0.6 * rng.next());
            Ok(vec![Strand { points, root_width: width, tip_width: width * 0.15, root_color: root_c, tip_color: tip_c }])
        };
        counts.asked = n;
        let mut made: Vec<Option<Result<Vec<Strand>, bool>>> = (0..roots.len()).map(|_| None).collect();
        crate::clay::par_chunks(&mut made, KEEP_OUT_CHUNK, |c, chunk| {
            for (k, slot) in chunk.iter_mut().enumerate() {
                *slot = Some(make(c * KEEP_OUT_CHUNK + k));
            }
        });
        let mut out: Vec<Strand> = Vec::with_capacity(roots.len());
        for m in made.into_iter().flatten() {
            match m {
                Ok(s) => out.extend(s),
                Err(_) => counts.excluded += 1,
            }
        }
        // Lid roots' extra strands past the film budget: dropped evenly.
        let cap = super::MAX_STRANDS as usize;
        if out.len() > cap {
            let m = out.len();
            let mut j = 0;
            out.retain(|_| {
                j += 1;
                j * cap / m != (j - 1) * cap / m
            });
        }
        // The ground (long fur): a point under it is raised onto it, a
        // lift over it; whether any was. A point it raises into the body
        // (hair wedged under a paw resting on the ground) is reported.
        let floor = fur.floor.map(|f| (Vec3::new(f[0], f[1], f[2]), f[3] + lift));
        let ground = |points: &mut [Vec3]| -> Vec<usize> {
            let Some((n, off)) = floor else { return Vec::new() };
            let mut raised = Vec::new();
            for (k, p) in points.iter_mut().enumerate().skip(1) {
                let h = n.dot(*p) - off;
                if h < 0.0 {
                    *p -= n * h;
                    raised.push(k);
                }
            }
            raised
        };
        let slide = fur.drape > 0.0 && !fur.avoid.is_empty();
        let eyes = Spheres { list: &fur.avoid, lift: lift.max(spacing * 0.02) };
        if let Some(head) = head {
            // 0: untouched, 1: moved, 2: wedged between the body and the
            // ground (left out).
            let mut moved = vec![0u8; out.len()];
            let mut both: Vec<(&mut Strand, &mut u8)> = out.iter_mut().zip(moved.iter_mut()).collect();
            crate::clay::par_chunks(&mut both, KEEP_OUT_CHUNK, |_, chunk| {
                for (s, m) in chunk.iter_mut() {
                    // Long fur slides over the eyes (a fringe falls in
                    // front of them) rather than being left out.
                    let slid = slide && crate::groom::keep_out(&mut s.points, &eyes);
                    let raised = !ground(&mut s.points).is_empty();
                    **m = u8::from(crate::groom::keep_out(&mut s.points, head) || raised || slid);
                    let again = ground(&mut s.points);
                    if again.iter().any(|&k| head.inside(s.points[k])) {
                        **m = 2;
                    }
                }
            });
            drop(both);
            counts.moved = moved.iter().filter(|m| **m == 1).count();
            let mut k = 0;
            out.retain(|_| {
                k += 1;
                moved[k - 1] != 2
            });
            counts.wedged = moved.iter().filter(|m| **m == 2).count();
        } else {
            for s in &mut out {
                if slide {
                    crate::groom::keep_out(&mut s.points, &eyes);
                }
                ground(&mut s.points);
            }
        }
        if !fur.avoid.is_empty() {
            let before = out.len();
            out.retain(|s| !s.points.iter().any(|p| Fur::in_any(&fur.avoid, *p)) && !s.points.windows(2).any(|w| Fur::in_any(&fur.avoid, (w[0] + w[1]) * 0.5)));
            counts.avoided = before - out.len();
        }
        (out, counts)
    }

    /// Fur's viewport cards: one short card a guide, lying on the surface
    /// (its face along the root's normal), as wide as the guides' spacing
    /// so neighbours overlap like a coat, coloured as the clay at the root
    /// (white strand texture times the vertex colour), kept out of `head`
    /// like other cards. Returns the mesh and how many cards moved.
    pub fn fur_cards(&self, fur: &Fur, head: Option<&dyn crate::groom::Collider>) -> (Mesh, usize) {
        let mut white = self.clone();
        white.root_color = [1.0; 3];
        white.tip_color = fur.tip_color.map_or([1.0; 3], |t| {
            // The texture carries the tip's shift; the vertex colour the root's.
            let c = Vec3::from(t);
            [0, 1, 2].map(|i| (1.0 - fur.tip_mix + fur.tip_mix * c[i]).clamp(0.0, 1.0))
        });
        let material = crate::material::Material::preset("hair").expect("hair preset");
        let mut mesh = Mesh { materials: vec![material.clone()], textures: Some(super::texture(&white)), ..Mesh::default() };
        let guides: Vec<&Guide> = self.guides.iter().filter(|g| g.points.len() >= 2 && g.points.iter().all(|p| p.is_finite())).collect();
        if guides.is_empty() {
            return (mesh, 0);
        }
        let frames = frames(&guides, head);
        let width = fur.spacing * 1.6;
        let mut middles: Vec<Vec3> = Vec::new();
        let tint = Vec3::from(fur.tint);
        for (g, (root, normal)) in guides.iter().zip(&frames) {
            let color = head.and_then(|h| h.color(*root)).unwrap_or(Vec3::from(self.root_color));
            // sRGB vertex colour of the tinted clay colour.
            let c = (color.clamp(Vec3::ZERO, Vec3::ONE).powf(2.2) * tint).min(Vec3::ONE).powf(1.0 / 2.2);
            let path = &g.points;
            let last = path.len() - 1;
            let base = mesh.vertices.len() as u32;
            let mut variant_rng = stream(self.seed ^ 0x5bd1_e995, middles.len() as u32 + base);
            let variant = (variant_rng.next() * super::VARIANTS as f32).min(super::VARIANTS as f32 - 1.0).floor();
            for (i, &p) in path.iter().enumerate() {
                let t = i as f32 / last as f32;
                let tangent = (path[(i + 1).min(last)] - path[i.saturating_sub(1)]).normalize_or(Vec3::NEG_Y);
                let side = tangent.cross(*normal).normalize_or(tangent.any_orthonormal_vector());
                let up = side.cross(tangent).normalize_or(*normal);
                let half = 0.5 * width * (1.0 - 0.6 * t);
                if head.is_some() {
                    middles.push(p);
                }
                for (k, u) in [-1.0f32, 0.0, 1.0].into_iter().enumerate() {
                    let n = (up + side * u * 0.28).normalize();
                    let across = (side - n * n.dot(side)).normalize_or(side);
                    mesh.vertices.push(Vertex {
                        pos: (p + side * half * u + up * half * 0.1 * (1.0 - u * u)).to_array(),
                        normal: n.to_array(),
                        color: c.to_array(),
                        surface: material.surface(),
                        material: 0,
                        uv: [(variant + k as f32 * 0.5) / super::VARIANTS as f32, t],
                        tangent: [across.x, across.y, across.z, 1.0],
                        light: material.light(),
                        base: [c.x, c.y, c.z, -1.0],
                        wear: [0.0; 3],
                        painted: [0; 4],
                    });
                }
            }
            for i in 0..last as u32 {
                for col in 0..2 {
                    let (a, b, c, d) = (base + 3 * i + col, base + 3 * i + col + 1, base + 3 * (i + 1) + col + 1, base + 3 * (i + 1) + col);
                    mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
                    mesh.edges.extend_from_slice(&[a, d]);
                }
            }
        }
        let Some(head) = head else { return (mesh, 0) };
        // Cards of different guides may have different point counts (a
        // groom lengthens some): kept out one by one.
        let mut moved = 0;
        let (mut v0, mut m0) = (0usize, 0usize);
        for g in &guides {
            let rows = g.points.len();
            let vs = &mut mesh.vertices[v0..v0 + 3 * rows];
            let ms = &mut middles[m0..m0 + rows];
            if super::keep_card_out(vs, ms, head) {
                moved += 1;
            }
            v0 += 3 * rows;
            m0 += rows;
        }
        (mesh, moved)
    }
}

/// Long fur's locks: each guide's lock centre (a guide picked so the
/// centres are about `clump_size` spacings apart), and each guide's path
/// pulled toward its centre's (a smaller lock inside the lock).
struct Locks<'a> {
    guides: &'a [&'a Guide],
    frames: &'a [(Vec3, Vec3)],
    spacing: f32,
    clump: f32,
    /// Per guide: its lock centre (a guide index).
    centre: Vec<usize>,
    /// Per guide: its path pulled toward its centre's.
    pulled: Vec<Vec<Vec3>>,
    /// Per guide: its lock's shade (pigment factor, about 1).
    shade: Vec<f32>,
    /// The parting, if any.
    part: Option<(Vec3, f32)>,
    /// Per guide: its lock's wave phase (the lock's, and a little of the
    /// guide's own) and how much of the length it keeps (`ragged`).
    phase: Vec<f32>,
    keep: Vec<f32>,
    wave: f32,
    waves: f32,
}

/// Which side of a parting `p` is on (always true without one).
fn on_side(part: Option<(Vec3, f32)>, p: Vec3) -> bool {
    part.is_none_or(|(n, off)| n.dot(p) >= off)
}

/// How strongly the strands gather into the big locks (the share of
/// `clump`); the guide's own small lock takes the rest.
const BIG_LOCK: f32 = 0.6;

impl<'a> Locks<'a> {
    fn new(guides: &'a [&'a Guide], frames: &'a [(Vec3, Vec3)], spacing: f32, fur: &Fur, seed: u32, melanin: f32) -> Self {
        // Centres: in guide order (a random order over the body: the
        // guides come out of sample elimination), each guide farther than
        // the lock's radius from every centre so far starts a lock
        // (greedy Poisson discs: even locks).
        let part = fur.part.map(|f| (Vec3::new(f[0], f[1], f[2]), f[3]));
        let reach = fur.clump_size * spacing;
        let mut grid: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = std::collections::HashMap::new();
        let mut centres: Vec<usize> = Vec::new();
        let key = |p: Vec3| super::cell_of(p, reach);
        let near = |grid: &std::collections::HashMap<(i32, i32, i32), Vec<usize>>, p: Vec3, n: Vec3, r: f32| -> Option<(f32, usize)> {
            let (x, y, z) = key(p);
            let mut best: Option<(f32, usize)> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &c in grid.get(&(x + dx, y + dy, z + dz)).into_iter().flatten() {
                            let d = frames[c].0.distance(p);
                            // A centre on the far side of a leg or the
                            // body (facing away) is not this lock's.
                            if d < r && frames[c].1.dot(n) > 0.3 && on_side(part, frames[c].0) == on_side(part, p) && best.is_none_or(|b| (d, c) < b) {
                                best = Some((d, c));
                            }
                        }
                    }
                }
            }
            best
        };
        for (i, (p, n)) in frames.iter().enumerate() {
            if near(&grid, *p, *n, reach * 0.5).is_none() {
                grid.entry(key(*p)).or_default().push(i);
                centres.push(i);
            }
        }
        let centre: Vec<usize> = frames.iter().enumerate().map(|(i, (p, n))| near(&grid, *p, *n, reach).map_or(i, |b| b.1)).collect();
        let strength = fur.clump * BIG_LOCK;
        let pulled = (0..guides.len()).map(|i| {
            let (own, c) = (&guides[i].points, &guides[centre[i]].points);
            if own.len() != c.len() || centre[i] == i {
                return own.clone();
            }
            let last = (own.len() - 1) as f32;
            own.iter().zip(c).enumerate().map(|(k, (p, q))| p.lerp(*q, strength * (k as f32 / last).powf(0.6))).collect()
        }).collect();
        let shade = (0..guides.len()).map(|i| {
            let mut r = stream(seed ^ 0x3C6E_F372, centre[i] as u32);
            let gauss = r.next() + r.next() + r.next() - 1.5;
            (gauss * 0.5 * melanin).exp()
        }).collect();
        let lock = |i: usize, salt: u32| stream(seed ^ salt, i as u32).next();
        let phase = (0..guides.len()).map(|i| (lock(centre[i], 0x1B87_3593) + 0.15 * (lock(i, 0x1B87_3593) - 0.5)) * std::f32::consts::TAU).collect();
        let keep = (0..guides.len()).map(|i| 1.0 - fur.ragged * (0.7 * lock(centre[i], 0xCC9E_2D51) + 0.3 * lock(i, 0xCC9E_2D51))).collect();
        Locks { guides, frames, spacing, clump: fur.clump, centre, pulled, shade, part, phase, keep, wave: fur.wave, waves: fur.waves }
    }

    /// A strand of guide `g`'s cell starting at `start` (on the surface,
    /// normal `n`): the shapes of the guides within 1.5 spacings that face
    /// its way, weighed by nearness, moved onto it; a little shorter or
    /// not (`lv`); pulled into its lock and its guide's small lock toward
    /// the tip; a gentle wave (`frizz`). Returns the points and the shade.
    fn strand(&self, g: usize, start: Vec3, n: Vec3, index: &Cells, rng: &mut Rng, lv: f32, frizz: f32) -> (Vec<Vec3>, f32) {
        let own = &self.guides[g].points;
        let count = own.len();
        let reach = 1.5 * self.spacing;
        let mut sum = vec![Vec3::ZERO; count];
        let mut total = 0.0;
        let add = |i: usize, w: f32, sum: &mut Vec<Vec3>, total: &mut f32| {
            let p = &self.guides[i].points;
            for (s, q) in sum.iter_mut().zip(p) {
                *s += (*q - p[0]) * w;
            }
            *total += w;
        };
        let mine = on_side(self.part, start);
        for i in index.within(start, reach) {
            let (r, rn) = self.frames[i];
            let d = r.distance(start);
            if i == g || d >= reach || rn.dot(n) < 0.5 || self.guides[i].points.len() != count || on_side(self.part, r) != mine {
                continue;
            }
            let w = (1.0 - d / reach).powi(2);
            add(i, w, &mut sum, &mut total);
        }
        // The cell's own guide always counts, at least as much as its
        // nearest neighbour would at the root (unless it is across the
        // parting and another guide is not).
        let d = self.frames[g].0.distance(start);
        if on_side(self.part, self.frames[g].0) == mine || total == 0.0 {
            add(g, (1.0 - d / reach).max(0.25).powi(2), &mut sum, &mut total);
        }
        let shape: Vec<Vec3> = sum.iter().map(|s| start + *s / total).collect();
        // Shorter: the same path, stopped early (segments stay even).
        let keep = (1.0 - lv * rng.next().powf(1.5)) * self.keep[g];
        let last = (count - 1) as f32;
        let at = |path: &[Vec3], f: f32| {
            let x = (f * last).clamp(0.0, last);
            let lo = (x.floor() as usize).min(count - 1);
            let hi = (lo + 1).min(count - 1);
            path[lo].lerp(path[hi], x - lo as f32)
        };
        let small = self.clump * (1.0 - BIG_LOCK);
        let big = self.clump * BIG_LOCK;
        let centre = &self.guides[self.centre[g]].points;
        let pulled = &self.pulled[g];
        let (phase, freq) = (rng.next() * std::f32::consts::TAU, 2.0 + 2.0 * rng.next());
        let (t1, t2) = tangents(n);
        let side = (t1 * (rng.next() - 0.5) + t2 * (rng.next() - 0.5)).normalize_or(t1);
        // The lock's wave: across its centre's path (square to the path
        // and the centre's skin normal), in the lock's phase, coming in
        // past the root.
        let lock_n = self.frames[self.centre[g]].1;
        let wave_phase = self.phase[g];
        let points = (0..count).map(|k| {
            let t = k as f32 / last;
            let f = t * keep;
            let mut p = at(&shape, f);
            let ramp = f.powf(0.6);
            if centre.len() == count {
                p = p.lerp(at(centre, f), big * ramp);
            }
            p = p.lerp(at(pulled, f), small * ramp);
            if self.wave > 0.0 {
                let path = if centre.len() == count { centre } else { own };
                let along = (at(path, (f + 0.5 / last).min(1.0)) - at(path, (f - 0.5 / last).max(0.0))).normalize_or(Vec3::NEG_Y);
                let across = along.cross(lock_n).normalize_or(t1);
                let come_in = (f / 0.2).min(1.0);
                p += across * (self.wave * come_in * ((f * self.waves * std::f32::consts::TAU + wave_phase).sin() - wave_phase.sin() * (1.0 - come_in)));
            }
            p + side * (frizz * f * (f * freq * std::f32::consts::TAU + phase).sin())
        }).collect();
        (points, self.shade[g])
    }
}

/// Spheres [x, y, z, radius] as something hair is kept out of (the
/// eyes, for long fur), kept `lift` outside them.
struct Spheres<'a> {
    list: &'a [[f32; 4]],
    lift: f32,
}

impl Spheres<'_> {
    /// The sphere `p` is deepest in (by how far inside, then order).
    fn deepest(&self, p: Vec3, margin: f32) -> Option<(Vec3, f32)> {
        self.list.iter().map(|s| (Vec3::new(s[0], s[1], s[2]), s[3] + margin)).filter(|(c, r)| p.distance(*c) < *r).max_by(|a, b| (a.1 - p.distance(a.0)).total_cmp(&(b.1 - p.distance(b.0))))
    }
}

impl crate::groom::Collider for Spheres<'_> {
    fn push(&self, p: Vec3) -> Option<Vec3> {
        let (c, r) = self.deepest(p, self.lift)?;
        Some(c + (p - c).normalize_or(Vec3::Z) * (r + r * 1e-4))
    }
    fn normal(&self, p: Vec3) -> Option<Vec3> {
        let (c, _) = self.deepest(p, self.lift * 4.0)?;
        Some((p - c).normalize_or(Vec3::Z))
    }
    fn inside(&self, p: Vec3) -> bool {
        self.deepest(p, 0.0).is_some()
    }
    fn step(&self) -> f32 {
        self.list.iter().map(|s| s[3] * 0.1).fold(f32::MAX, f32::min).max(1e-6)
    }
}

/// How a long guide hangs (`drape_guide`).
pub struct Drape<'a> {
    /// 0 (no weight: the guide runs on along its first direction) to 1
    /// (it hangs: turned toward `down` within about a fifth of its
    /// length past its stiff root).
    pub weight: f32,
    /// The way gravity pulls (unit).
    pub down: Vec3,
    /// The ground: points stay on the side of the plane (unit normal,
    /// offset: `n·p >= offset`) a lift above it, sliding along it.
    pub floor: Option<(Vec3, f32)>,
    /// What the guide lies on (the clay), kept `lift` out by the collider.
    pub body: Option<&'a dyn crate::groom::Collider>,
    /// Spheres [x, y, z, radius] it slides over (the eyes).
    pub over: &'a [[f32; 4]],
    /// Kept this far above the floor and outside `over`.
    pub lift: f32,
}

/// How far the hanging turns a guide a unit of its length, at weight 1
/// (radians-ish: the step toward `down` is this times a segment's share).
const HANG: f32 = 6.0;
/// The stiff root: the share of the length over which the hanging comes
/// in (the hair leaves the skin before it falls).
const ROOT: f32 = 0.12;

/// A long guide hung from `root` like a chain marched from the root: it
/// leaves along `start` (unit), and each segment turns toward the
/// drape's `down` by its weight (less in the stiff root), then is kept
/// out of the body (`groom::clear_segment` turns it about the point
/// before it, round the surface: hair that meets the body slides down
/// along it), over the `over` spheres and above the floor, keeping its
/// length: every segment is `length / segments` long, but for the
/// body's last push out of the surface (`clear_segment` moves its end a
/// lift out), so the guide keeps its length within a few lifts.
/// Deterministic; `segments + 1` points.
pub fn drape_guide(root: Vec3, start: Vec3, length: f32, segments: usize, drape: &Drape) -> Vec<Vec3> {
    let segments = segments.max(1);
    let l = length / segments as f32;
    let mut d = start.normalize_or(drape.down);
    let mut points = Vec::with_capacity(segments + 1);
    points.push(root);
    for k in 1..=segments {
        let t = (k as f32 - 0.5) / segments as f32;
        let w = drape.weight * HANG / segments as f32 * (t / ROOT).min(1.0);
        let want = (d + drape.down * w).normalize_or(d);
        let a = points[k - 1];
        let mut b = a + want * l;
        // Held so: along `want`, then out of the spheres and the body,
        // then off the floor (last: the ground holds), each keeping the
        // segment's length from `a`.
        let keep = |b: Vec3| a + (b - a).normalize_or(want) * l;
        for s in drape.over {
            let c = Vec3::new(s[0], s[1], s[2]);
            let r = s[3] + drape.lift;
            if b.distance(c) < r {
                b = keep(c + (b - c).normalize_or(drape.down * -1.0) * r);
            }
        }
        if let Some(body) = drape.body {
            if let Some(q) = body.push(b) {
                b = keep(q);
            }
            crate::groom::clear_segment(a, &mut b, body);
        }
        if let Some((n, off)) = drape.floor {
            let h = n.dot(b) - off - drape.lift;
            if h < 0.0 {
                let flat = (want - n * n.dot(want)).normalize_or(n.any_orthonormal_vector());
                let up = (n.dot(a) - off - drape.lift).max(0.0);
                // Down to the floor, then along it the rest of the way:
                // the way it was going, or (where that runs under the
                // body: a paw resting on the ground) back, or across.
                let along = (l * l - up * up).max(0.0).sqrt();
                let across = n.cross(flat);
                let at = |f: Vec3| a - n * up + f * along;
                let clear = |q: Vec3| drape.body.is_none_or(|body| !body.inside(q) && !body.inside((a + q) * 0.5));
                b = [flat, -flat, across, -across].into_iter().map(at).find(|q| clear(*q)).unwrap_or_else(|| at(flat));
            }
        }
        d = (b - a).normalize_or(want);
        points.push(b);
    }
    points
}

/// Nearest-neighbour spacing of `points`: mean and coefficient of
/// variation (standard deviation over mean): about 0.1 to 0.2 for an
/// even blue-noise spread, 0.5 for random points.
pub fn spacing_stats(points: &[Vec3]) -> (f32, f32) {
    if points.len() < 2 {
        return (0.0, 0.0);
    }
    let lo = points.iter().fold(Vec3::splat(f32::MAX), |a, p| a.min(*p));
    let hi = points.iter().fold(Vec3::splat(f32::MIN), |a, p| a.max(*p));
    let cell = ((hi - lo).max_element() / (points.len() as f32).sqrt()).max(1e-6);
    let cells = Cells::new(cell, points.iter().copied().enumerate());
    let d: Vec<f32> = (0..points.len()).filter_map(|i| cells.nearest(points[i], points, i, 1).first().map(|&j| points[i].distance(points[j]))).collect();
    let mean = d.iter().sum::<f32>() / d.len() as f32;
    let var = d.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / d.len() as f32;
    (mean, var.sqrt() / mean.max(1e-12))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ball of radius 0.1 at the origin, kept `lift` out.
    struct Ball {
        lift: f32,
    }

    impl crate::groom::Collider for Ball {
        fn push(&self, p: Vec3) -> Option<Vec3> {
            let want = 0.1 + self.lift;
            (p.length() < want).then(|| p.normalize_or(Vec3::Y) * want)
        }
        fn normal(&self, p: Vec3) -> Option<Vec3> {
            Some(p.normalize_or(Vec3::Y))
        }
        fn inside(&self, p: Vec3) -> bool {
            p.length() < 0.1
        }
        fn step(&self) -> f32 {
            0.002
        }
    }

    fn length(p: &[Vec3]) -> f32 {
        p.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    /// `drape_hangs_over_the_body`: with no weight a guide runs straight
    /// along its start; at full weight one leaving a ball's side
    /// sideways ends hanging (its last segment within 10 degrees of
    /// down), one leaving its top slides down over it with every sample
    /// outside, both keep their length to 1e-5, and none goes under the
    /// floor; the same call gives the same bits.
    #[test]
    fn drape_hangs_over_the_body() {
        let ball = Ball { lift: 0.001 };
        let floor = (Vec3::Y, -0.5);
        let mk = |w: f32| Drape { weight: w, down: Vec3::NEG_Y, floor: Some(floor), body: Some(&ball), over: &[], lift: 0.001 };
        let straight = drape_guide(Vec3::new(0.101, 0.0, 0.0), Vec3::X, 0.3, 16, &mk(0.0));
        assert!(straight.iter().all(|p| p.y.abs() < 1e-6 && p.z.abs() < 1e-6));
        assert!((straight[16].x - 0.401).abs() < 1e-5);
        let side = drape_guide(Vec3::new(0.101, 0.0, 0.0), Vec3::X, 0.3, 16, &mk(1.0));
        let tip = (side[16] - side[15]).normalize();
        let angle = tip.angle_between(Vec3::NEG_Y).to_degrees();
        eprintln!("side guide: tip at {:?}, last segment {angle:.2} degrees from down, length {:.6}", side[16], length(&side));
        assert!(angle < 10.0, "{angle}");
        assert!((length(&side) - 0.3).abs() < 1e-5);
        let top = drape_guide(Vec3::new(0.0, 0.101, 0.0), Vec3::Z, 0.4, 24, &mk(1.0));
        let samples = top.windows(2).flat_map(|w| (0..=8).map(move |i| w[0].lerp(w[1], i as f32 / 8.0)));
        let deepest = samples.map(|p| p.length()).fold(f32::MAX, f32::min);
        eprintln!("top guide: tip at {:?}, nearest sample {deepest:.5} from the centre, length {:.6}", top[24], length(&top));
        assert!(deepest > 0.0995, "{deepest}");
        assert!(top[24].y < -0.1 && top[24].z > 0.05, "it slid over the ball and hangs past it: {:?}", top[24]);
        assert!((length(&top) - 0.4).abs() < 1e-5);
        // Long enough to reach the floor: it lies along it.
        let long = drape_guide(Vec3::new(0.101, 0.0, 0.0), Vec3::X, 0.9, 32, &mk(1.0));
        assert!(long.iter().all(|p| p.y >= -0.5 + 0.001 - 1e-5), "{:?}", long.last());
        assert!((long[32].y - (-0.499)).abs() < 1e-4 && (length(&long) - 0.9).abs() < 1e-4, "{:?} {}", long[32], length(&long));
        assert_eq!(drape_guide(Vec3::new(0.0, 0.101, 0.0), Vec3::Z, 0.4, 24, &mk(1.0)), top, "deterministic");
        // A sphere to slide over (an eye) is never entered.
        let eye = [[0.0, -0.05, 0.16, 0.03]];
        let over = Drape { over: &eye, ..mk(1.0) };
        let past = drape_guide(Vec3::new(0.0, 0.101, 0.0), Vec3::Z, 0.4, 24, &over);
        assert!(past.iter().all(|p| p.distance(Vec3::new(0.0, -0.05, 0.16)) >= 0.03), "outside the eye");
    }

    /// A flat skin (y = 0, normal +Y) of guides on a grid `spacing`
    /// apart hanging toward -Z with a sway of their own, as long fur.
    fn hanging(clump: f32) -> Hair {
        hanging_with(clump, 0.06)
    }

    fn hanging_with(clump: f32, sway_size: f32) -> Hair {
        let spacing = 0.02;
        let mut guides = Vec::new();
        let mut r = Rng(9);
        for i in 0..20 {
            for j in 0..20 {
                let root = Vec3::new(i as f32 * spacing + (r.next() - 0.5) * 0.004, 0.0, j as f32 * spacing + (r.next() - 0.5) * 0.004);
                let sway = Vec3::new(r.next() - 0.5, 0.0, 0.0) * sway_size;
                let points = (0..=16).map(|k| {
                    let t = k as f32 / 16.0;
                    root + Vec3::new(0.0, 0.02 * t, -0.2 * t) + sway * t * t
                }).collect();
                guides.push(Guide { points, width: None });
            }
        }
        let fur = Fur { spacing, drape: 1.0, clump, jitter: 0.0, ..Fur::default() };
        Hair { guides: guides.into(), fur: Some(fur), strands: 20_000, frizz: 0.0, length_variation: 0.0, strand_width: 1e-4, ..Hair::default() }
    }

    /// `long_strands_blend_and_gather`: long fur's strands blend their
    /// neighbouring guides (two strands rooted within a fifth of a
    /// spacing end near each other even across a cell edge, where copying
    /// one guide each split them by the guides' sway), keep the strand's
    /// length near its guides', and with `clump` gather toward the
    /// guides: the strands' tips come nearer the nearest guide tip and
    /// the tips bunch (more empty space between locks). The same bits on
    /// 1 and 12 threads.
    #[test]
    fn long_strands_blend_and_gather() {
        let tips = |h: &Hair| -> (Vec<Vec3>, Vec<Vec3>) {
            let s = h.strands();
            (s.iter().map(|s| s.points[0]).collect(), s.iter().map(|s| *s.points.last().unwrap()).collect())
        };
        let loose = hanging(0.0);
        let (roots, ends) = tips(&loose);
        assert!(roots.len() > 15_000, "{}", roots.len());
        // Neighbouring roots: how far apart their tips end.
        let apart = |roots: &[Vec3], ends: &[Vec3]| {
            let cells = Cells::new(0.004, roots.iter().copied().enumerate());
            let (mut worst, mut pairs, mut sum) = (0.0f32, 0, 0.0f32);
            for (i, r) in roots.iter().enumerate().step_by(3) {
                for j in cells.within(*r, 0.004) {
                    if j != i && roots[j].distance(*r) < 0.004 {
                        let d = ends[i].distance(ends[j]);
                        worst = worst.max(d);
                        sum += d;
                        pairs += 1;
                    }
                }
            }
            (pairs, sum / pairs.max(1) as f32, worst)
        };
        let (pairs, mean, worst) = apart(&roots, &ends);
        let copied = Hair { fur: Some(Fur { drape: 0.0, ..loose.fur.clone().unwrap() }), ..loose.clone() };
        let (croots, cends) = tips(&copied);
        let (_, cmean, cworst) = apart(&croots, &cends);
        eprintln!("{pairs} root pairs within 0.2 spacing: tips apart {mean:.5} on average, at most {worst:.5}; copying one guide each: {cmean:.5}, at most {cworst:.5}");
        assert!(pairs > 200 && worst < 0.015 && cworst > 3.0 * worst, "{worst} {cworst}");
        let guide_tips: Vec<Vec3> = loose.guides.iter().map(|g| *g.points.last().unwrap()).collect();
        let to_guide = |ends: &[Vec3]| ends.iter().map(|e| guide_tips.iter().map(|g| g.distance(*e)).fold(f32::MAX, f32::min)).sum::<f32>() / ends.len() as f32;
        let tight = hanging(0.8);
        let (_, tight_ends) = tips(&tight);
        let (a, b) = (to_guide(&ends), to_guide(&tight_ends));
        let (lm, lcv) = spacing_stats(&ends);
        let (tm, tcv) = spacing_stats(&tight_ends);
        eprintln!("tips from the nearest guide tip: {a:.5} loose, {b:.5} with clump 0.8; tips' nearest spacing {lm:.5} (cv {lcv:.3}) loose, {tm:.5} (cv {tcv:.3}) gathered");
        assert!(b < 0.8 * a && tm < 0.6 * lm && tcv > lcv, "{a} {b} {lm} {tm}");
        for s in tight.strands() {
            let l: f32 = s.points.windows(2).map(|w| w[0].distance(w[1])).sum();
            assert!(l > 0.17 && l < 0.26, "{l}");
        }
        let one = crate::clay::with_threads(1, || tight.strands());
        assert!(one.iter().zip(tight.strands()).all(|(a, b)| a.points == b.points && a.root_color == b.root_color));
    }

    /// `locks_wave_together_and_end_raggedly`: straight hanging guides,
    /// with `wave` 0.01 the strands swing off the hanging line by about
    /// the wave (not at all without), two strands of one guide's cell
    /// swing in step (their across offsets correlate above 0.9), and
    /// different locks are out of step; `ragged` 0.5 stops many strands
    /// short (none without it, with no length variation).
    #[test]
    fn locks_wave_together_and_end_raggedly() {
        let with = |wave: f32, ragged: f32| {
            let mut h = hanging_with(0.0, 0.0);
            let f = h.fur.as_mut().unwrap();
            f.wave = wave;
            f.ragged = ragged;
            h
        };
        // Offsets off the hanging line along each strand, from its root
        // (x, then y: no head here, so the lock's normal is the way out
        // from the roots' middle, and the wave is across that).
        let line = Vec3::new(0.0, 0.02, -0.2).normalize();
        let off = |s: &Strand, k: usize| {
            let d = s.points[k] - s.points[0];
            d - line * line.dot(d)
        };
        let offsets = |s: &Strand| -> Vec<f32> { (0..s.points.len()).map(|k| off(s, k).x).chain((0..s.points.len()).map(|k| off(s, k).y)).collect() };
        let swing = |v: &[Strand]| v.iter().map(|s| (0..s.points.len()).map(|k| off(s, k).length()).fold(0.0f32, f32::max)).sum::<f32>() / v.len() as f32;
        let flat = swing(&with(0.0, 0.0).strands());
        let wavy_hair = with(0.01, 0.0);
        let wavy = wavy_hair.strands();
        let swung = swing(&wavy);
        eprintln!("mean largest across offset: straight {flat:.5}, with wave 0.01 {swung:.5}");
        assert!(flat < 0.002 && swung > 0.008 && swung < 0.025, "{flat} {swung}");
        // Strands by their guide (the nearest guide root).
        let roots: Vec<Vec3> = wavy_hair.guides.iter().map(|g| g.points[0]).collect();
        let owner = |p: Vec3| (0..roots.len()).min_by(|&a, &b| roots[a].distance(p).total_cmp(&roots[b].distance(p))).unwrap();
        let corr = |a: &[f32], b: &[f32]| {
            let (ma, mb) = (a.iter().sum::<f32>() / a.len() as f32, b.iter().sum::<f32>() / b.len() as f32);
            let cov: f32 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
            let va: f32 = a.iter().map(|x| (x - ma) * (x - ma)).sum();
            let vb: f32 = b.iter().map(|y| (y - mb) * (y - mb)).sum();
            cov / (va * vb).sqrt().max(1e-12)
        };
        let mut by: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
        for (i, s) in wavy.iter().enumerate().take(4000) {
            by.entry(owner(s.points[0])).or_default().push(i);
        }
        let (mut same, mut n_same, mut other, mut n_other) = (0.0, 0, 0.0, 0);
        let cells: Vec<&Vec<usize>> = by.values().filter(|v| v.len() >= 2).collect();
        for (k, c) in cells.iter().enumerate() {
            same += corr(&offsets(&wavy[c[0]]), &offsets(&wavy[c[1]]));
            n_same += 1;
            let d = cells[(k + 7) % cells.len()];
            other += corr(&offsets(&wavy[c[0]]), &offsets(&wavy[d[0]])).abs();
            n_other += 1;
        }
        let (same, other) = (same / n_same as f32, other / n_other as f32);
        eprintln!("{n_same} cells: same-cell strands' swing correlates {same:.3}; strands of other locks {other:.3} (absolute)");
        assert!(same > 0.9 && other < 0.75, "{same} {other}");
        let length = |s: &Strand| s.points.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
        let even = with(0.0, 0.0).strands();
        let ragged = with(0.0, 0.5).strands();
        let short = |v: &[Strand]| v.iter().filter(|s| length(s) < 0.85 * 0.201).count() as f32 / v.len() as f32;
        eprintln!("strands under 85% of the length: {:.3} even, {:.3} ragged 0.5", short(&even), short(&ragged));
        assert!(short(&even) == 0.0 && short(&ragged) > 0.4);
    }

    /// `long_strands_slide_over_the_eyes`: a sphere (an eye) hanging in
    /// the middle of a long curtain: every strand is kept (short fur
    /// leaves out the ones that would enter it), none has a point or a
    /// segment's middle inside it, and strands that never come near it
    /// are unchanged bit for bit.
    #[test]
    fn long_strands_slide_over_the_eyes() {
        let plain = hanging_with(0.0, 0.0);
        let mut eyed = plain.clone();
        let eye = [0.2, 0.0, -0.1, 0.03];
        eyed.fur.as_mut().unwrap().avoid = vec![eye];
        let (a, b) = (plain.strands(), eyed.strands());
        let c = Vec3::new(eye[0], eye[1], eye[2]);
        let inside = b.iter().filter(|s| s.points.iter().any(|p| p.distance(c) < eye[3]) || s.points.windows(2).any(|w| ((w[0] + w[1]) * 0.5).distance(c) < eye[3])).count();
        let near = a.iter().filter(|s| s.points.iter().any(|p| p.distance(c) < eye[3] * 1.5)).count();
        let same = a.iter().zip(&b).filter(|(x, y)| x.points == y.points).count();
        eprintln!("{} strands without the eye, {} with it ({near} came near it), {inside} inside it, {same} unchanged", a.len(), b.len());
        assert_eq!(a.len(), b.len());
        assert!(near > 100 && inside == 0 && same >= a.len() - near, "{near} {inside} {same}");
        let mut short = eyed.clone();
        short.fur.as_mut().unwrap().drape = 0.0;
        let mut short_plain = plain.clone();
        short_plain.fur.as_mut().unwrap().drape = 0.0;
        assert!(short.strands().len() + 100 < short_plain.strands().len(), "short fur still leaves out what enters an eye");
    }
}
