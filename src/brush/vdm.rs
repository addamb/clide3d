//! VDM stamps (vector displacement, Alpha 3D; docs/SCULPT_PLAN.md 3.7):
//! a piece of a clay's surface captured as a sparse volume, which the
//! `vdm` brush stamps onto another surface, overhangs and all.
//!
//! `alpha create_vdm` captures a box standing on a clay's surface: `radius`
//! across (each way) and from `below` under the surface to `above` over
//! it, in a frame whose up is the surface normal at the base point and
//! whose across is the object's X laid on the surface. On a lattice of
//! `cell` in that frame it keeps D = C - P, where C is the clay's field
//! (clamped to `limit`, as the field is) and P the field of the flat base
//! plane (its height, clamped alike). Where the clay is flat D is zero and
//! no brick is kept, so the volume is sparse: what stands out of (or sinks
//! into) the plane. It is kept as a detail grid (8³ bricks, the file's
//! 1/256-cell steps; quantized at capture, so a saved stamp loads bit for
//! bit) and hashed by content.
//!
//! A stamp (`Stamp::field`) puts the base plane on the dab's surface and
//! adds D, scaled by the dab's radius over the stamp's: on a flat surface
//! that is exactly the captured field, so a horn bent over itself comes
//! back with its overhang; on a curved one it displaces the surface by the
//! same amounts. The clamps are reconciled (`Stamp::field`), so a stamp
//! taller than either field's clamp band still keeps its tip.
//!
//! VDM stamps live in the scene's alpha library and the clays' alpha
//! stores, as alpha images do (an `Alpha` with a volume; its image is the
//! stamp's height seen from above, for `alpha preview` and the pickers).

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::clay::{Clay, Detail, Field, par_chunks};

/// Most lattice points a capture may sample.
pub const MAX_POINTS: f64 = (1 << 21) as f64;
/// Most cells across a capture's half-width (`radius / cell`), and along
/// its height (`(above + below) / cell`): the preview image is at most
/// 2 x 256 + 1 pixels a side.
pub const MAX_HALF_CELLS: f32 = 256.0;
pub const MAX_HEIGHT_CELLS: f32 = 1024.0;
/// Most bricks a stamp keeps (8 MiB of values).
pub const MAX_BRICKS: usize = 4096;
/// D smaller than this many cells is the flat plane (float noise).
const FLAT: f32 = 1.0 / 128.0;
/// The detail file's steps hold at most 127 cells.
const MAX_D_CELLS: f32 = 126.0;

/// A captured stamp (see the module doc), in its own frame and units.
#[derive(Clone, Debug, PartialEq)]
pub struct Vdm {
    /// Half its width across, each way.
    pub radius: f32,
    pub above: f32,
    pub below: f32,
    /// The clamp of the field it was captured from.
    pub limit: f32,
    /// D on the lattice (its voxel is the capture's cell): x across (u),
    /// y across (v), z up.
    pub grid: Detail,
}

fn check(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.into()) }
}

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Vdm {
    pub fn cell(&self) -> f32 {
        self.grid.voxel()
    }

    /// Sizes, cell, bricks and their keys within the box: what a stamp
    /// read from a file must hold before anything samples it.
    pub fn validate(&self) -> Result<(), String> {
        let cell = self.cell();
        let fin = [self.radius, self.above, self.below, self.limit, cell].iter().all(|v| v.is_finite());
        check(fin && self.radius > 0.0 && self.above >= 0.0 && self.below >= 0.0 && self.limit > 0.0 && cell > 0.0, "a VDM needs a finite radius, above, below, limit and cell")?;
        check(self.radius <= 1e4 && self.above <= 8.0 * self.radius && self.below <= 8.0 * self.radius, "a VDM's above and below are at most 8 radii")?;
        check(self.radius / cell <= MAX_HALF_CELLS && (self.above + self.below) / cell <= MAX_HEIGHT_CELLS, "a VDM's lattice is too fine for its size")?;
        // The lattice capture would sample (`height_image` reads each
        // point), in f64: a file may not ask for more than a capture makes.
        check(self.lattice_points() <= MAX_POINTS, &format!("a VDM's lattice holds at most {MAX_POINTS} points"))?;
        let g = self.grid.grid();
        check(g.levels().is_empty() && g.brick_count() <= MAX_BRICKS, &format!("a VDM keeps at most {MAX_BRICKS} bricks"))?;
        // In i64: a file's keys may be anything i32 holds (k * 8 would
        // overflow, and i32::MIN has no abs).
        let (na, nh) = ((self.radius / cell).ceil() as i64 + 8, ((self.above.max(self.below)) / cell).ceil() as i64 + 8);
        let inside = |k: &[i32; 3]| (k[0] as i64 * 8).abs() <= na && (k[1] as i64 * 8).abs() <= na && (k[2] as i64 * 8).abs() <= nh;
        check(g.brick_keys().all(inside), "a VDM brick lies outside its box")
    }

    /// Lattice points in the stamp's box (as `capture` counts them).
    pub fn lattice_points(&self) -> f64 {
        let cell = self.cell();
        let a = (self.radius / cell).ceil() as f64 * 2.0 + 1.0;
        a * a * (((self.above / cell).ceil() + (self.below / cell).ceil()) as f64 + 1.0)
    }

    /// The farthest a stamp reaches from its base point, in its own units.
    pub fn reach(&self) -> f32 {
        (2.0 * self.radius * self.radius + self.above.max(self.below).powi(2)).sqrt()
    }

    /// D at `q` (the stamp's frame and units), faded over the outer 15% of
    /// its width; 0 outside its box.
    pub fn delta(&self, q: Vec3) -> f32 {
        let across = q.x.abs().max(q.y.abs());
        if !(across <= self.radius && q.z >= -self.below && q.z <= self.above) {
            return 0.0;
        }
        let d = self.grid.grid().level_offset(q);
        if d == 0.0 {
            return 0.0;
        }
        d * (1.0 - smooth01((across / self.radius - 0.85) / 0.15))
    }

    /// The stamp's height seen from above, 16-bit (`below` black, `above`
    /// white; columns with nothing solid black), one pixel per lattice
    /// column: the alpha image a VDM shows as.
    pub fn height_image(&self) -> (u32, u32, Vec<u16>) {
        let cell = self.cell();
        let na = (self.radius / cell).ceil() as i32;
        let (h0, h1) = (-(self.below / cell).ceil() as i32, (self.above / cell).ceil() as i32);
        let side = (2 * na + 1) as u32;
        let g = self.grid.grid();
        let span = (self.above + self.below).max(cell);
        let mut data = Vec::with_capacity((side * side) as usize);
        for ib in (-na..=na).rev() {
            for ia in -na..=na {
                let top = (h0..=h1).rev().find(|&ih| {
                    let h = ih as f32 * cell;
                    h.clamp(-self.limit, self.limit) + g.get([ia, ib, ih]) <= 0.0
                });
                data.push(top.map_or(0, |ih| (((ih as f32 * cell + self.below) / span).clamp(0.0, 1.0) * 65535.0).round() as u16));
            }
        }
        (side, side, data)
    }

    /// Bytes for the stamp's content hash: sizes, then the bricks in key
    /// order.
    pub fn hash_bytes(&self, out: &mut Vec<u8>) {
        out.extend(b"vdm");
        for v in [self.radius, self.above, self.below, self.limit, self.cell()] {
            out.extend(v.to_bits().to_le_bytes());
        }
        let g = self.grid.grid();
        let mut keys: Vec<&[i32; 3]> = g.brick_keys().collect();
        keys.sort_unstable();
        out.extend((keys.len() as u64).to_le_bytes());
        for k in keys {
            k.iter().for_each(|c| out.extend(c.to_le_bytes()));
            g.brick(k).expect("listed").iter().for_each(|v| out.extend(v.to_bits().to_le_bytes()));
        }
    }
}

/// What `Vdm` reads from and writes to a file.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VdmFile {
    radius: f32,
    above: f32,
    below: f32,
    limit: f32,
    grid: Detail,
}

impl Serialize for Vdm {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        VdmFile { radius: self.radius, above: self.above, below: self.below, limit: self.limit, grid: self.grid.clone() }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Vdm {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let f = VdmFile::deserialize(d)?;
        let v = Vdm { radius: f.radius, above: f.above, below: f.below, limit: f.limit, grid: f.grid };
        v.validate().map_err(serde::de::Error::custom)?;
        Ok(v)
    }
}

/// Where and how big `alpha create_vdm` captures, in the clay's space.
pub struct Capture {
    /// On (or near: it moves onto the surface along the normal) the clay.
    pub center: Vec3,
    /// The stamp's up (default: the surface normal there).
    pub normal: Option<Vec3>,
    /// The direction across the stamp (default: object X).
    pub tangent: Option<Vec3>,
    pub radius: f32,
    pub above: f32,
    pub below: f32,
    /// The lattice's cell (default the clay's detail cell, coarsened to fit
    /// `MAX_POINTS`).
    pub cell: Option<f32>,
}

/// What a capture made, with where it stood on the clay.
pub struct Captured {
    pub vdm: Vdm,
    pub base: Vec3,
    pub normal: Vec3,
}

/// Captures a stamp from `clay` (see the module doc). Every size is
/// checked, and the lattice bounded, before anything is sampled.
pub fn capture(clay: &Clay, c: &Capture) -> Result<Captured, String> {
    check(c.center.is_finite() && c.center.abs().max_element() < 1e6, "center must be finite, within 1e6 of the clay's origin")?;
    check(c.radius.is_finite() && c.radius > 0.0 && c.radius <= 1e4, "radius must be above 0 (half the stamp's width)")?;
    check(c.above.is_finite() && c.below.is_finite() && c.above >= 0.0 && c.below >= 0.0 && c.above <= 8.0 * c.radius && c.below <= 8.0 * c.radius, "above and below must be 0 to 8 radii")?;
    check(c.normal.is_none_or(|n| n.is_finite() && n.length() > 1e-6) && c.tangent.is_none_or(|t| t.is_finite()), "normal and tangent must be finite, normal not zero")?;
    let own = clay.detail_cell.or_else(|| clay.cell_size()).ok_or("the clay has no surface to capture")?;
    let mut cell = c.cell.unwrap_or(own);
    check(cell.is_finite() && cell > 0.0, "cell must be above 0")?;
    // Coarsen until the lattice fits (in f64: no size can overflow).
    let count = |cell: f32| {
        let (a, h) = ((c.radius / cell).ceil() as f64 * 2.0 + 1.0, ((c.above / cell).ceil() + (c.below / cell).ceil()) as f64 + 1.0);
        a * a * h
    };
    let fits = |cell: f32| count(cell) <= MAX_POINTS && c.radius / cell <= MAX_HALF_CELLS && (c.above + c.below) / cell <= MAX_HEIGHT_CELLS;
    if !fits(cell) {
        check(c.cell.is_none(), &format!("cell {cell} makes more than {MAX_POINTS} lattice points for that size; use a larger cell or a smaller radius"))?;
        for _ in 0..64 {
            if fits(cell) {
                break;
            }
            cell *= 1.25;
        }
        check(fits(cell), "the capture is too large for its cell")?;
    }
    check(cell >= c.radius / MAX_HALF_CELLS * 0.999 || fits(cell), "cell is too fine for that radius")?;
    let field = Field::new(clay, cell);
    let limit = field.limit;
    check(limit.is_finite() && limit > 0.0, "the clay's field is broken")?;
    let ext = Vec3::splat((2.0 * c.radius * c.radius + c.above.max(c.below).powi(2)).sqrt() + 2.0 * cell);
    let list = field.near(c.center - ext - c.radius, c.center + ext + c.radius);
    // The base plane: the mean surface on two rings around the center
    // (whatever stands on the center itself), each point found along the
    // normal by Newton steps; the normal, unless given, their mean.
    let onto = |q: Vec3, n: Vec3| {
        let mut s = 0.0f32;
        for _ in 0..16 {
            s = (s - field.sample(q + n * s, &list).0).clamp(-c.radius, c.radius);
        }
        let p = q + n * s;
        (field.sample(p, &list).0.abs() <= cell * 0.5).then_some(p)
    };
    let ring = |n: Vec3| -> Vec<Vec3> {
        let (u, v) = super::alpha::frame(n, c.tangent, false, 0.0);
        let mut on = Vec::with_capacity(32);
        for reach in [0.7f32, 0.9] {
            for j in 0..16 {
                let a = std::f32::consts::TAU * j as f32 / 16.0;
                if let Some(p) = onto(c.center + (u * a.cos() + v * a.sin()) * c.radius * reach, n) {
                    on.push(p);
                }
            }
        }
        on
    };
    let mut n = match c.normal {
        Some(n) => n.normalize(),
        None => field.sculpt_normal(c.center, &list, cell),
    };
    let mut on = ring(n);
    if c.normal.is_none() && on.len() >= 8 {
        n = on.iter().fold(Vec3::ZERO, |sum, p| sum + field.sculpt_normal(*p, &list, cell)).normalize_or(n);
        on = ring(n);
    }
    check(on.len() >= 8, "no clay surface around center (along the normal) to stand the stamp on; give a center on the clay and a radius within it")?;
    let base = c.center + n * (on.iter().map(|p| (*p - c.center).dot(n)).sum::<f32>() / on.len() as f32);
    let (u, v) = super::alpha::frame(n, c.tangent, false, 0.0);
    let na = (c.radius / cell).ceil() as i32;
    let (h0, h1) = (-((c.below / cell).ceil() as i32), (c.above / cell).ceil() as i32);
    let side = (2 * na + 1) as usize;
    // D per lattice point, z slice by z slice in parallel.
    let mut slices: Vec<Vec<f32>> = vec![Vec::new(); (h1 - h0 + 1) as usize];
    par_chunks(&mut slices, 1, |k, s| {
        let ih = h0 + k as i32;
        let h = ih as f32 * cell;
        let plane = h.clamp(-limit, limit);
        let out = &mut s[0];
        out.reserve(side * side);
        for ib in -na..=na {
            for ia in -na..=na {
                let p = base + u * (ia as f32 * cell) + v * (ib as f32 * cell) + n * h;
                let d = field.sample(p, &list).0 - plane;
                out.push(if d.abs() < FLAT * cell || !d.is_finite() { 0.0 } else { d.clamp(-MAX_D_CELLS * cell, MAX_D_CELLS * cell) });
            }
        }
    });
    let mut grid = Detail::new(cell);
    {
        let g = grid.grid_mut();
        for (k, slice) in slices.iter().enumerate() {
            let ih = h0 + k as i32;
            for (j, d) in slice.iter().enumerate() {
                if *d != 0.0 {
                    g.set([j as i32 % side as i32 - na, j as i32 / side as i32 - na, ih], *d);
                }
            }
        }
        g.prune();
    }
    let grid = grid.quantized();
    check(grid.grid().brick_count() > 0, "nothing stands out of (or sinks into) the surface there: the capture would be flat")?;
    let vdm = Vdm { radius: c.radius, above: c.above, below: c.below, limit, grid };
    vdm.validate().map_err(|e| format!("{e}; use a smaller region or a larger cell"))?;
    Ok(Captured { vdm, base, normal: n })
}

/// A stamp placed on one dab, in the clay's space.
pub struct Stamp<'a> {
    pub vdm: &'a Vdm,
    /// The base point (on the surface) and the frame: across (u, v), up n.
    pub center: Vec3,
    pub u: Vec3,
    pub v: Vec3,
    pub n: Vec3,
    /// Clay units per stamp unit.
    pub scale: f32,
    /// The target field's clamp.
    pub limit: f32,
    /// Carve the shape in instead (turned over, D negated).
    pub invert: bool,
}

impl Stamp<'_> {
    /// The field at `p` with the stamp on a surface whose field there is
    /// `t`; None where the stamp leaves it alone.
    ///
    /// With L the smaller clamp (the target's, or the stamp's scaled), the
    /// stamped field is clamp(t) + clamp(k (P_s + D)) - clamp(k h): on a
    /// flat surface (t = clamp(k h)) that is the captured field, clamped
    /// at L, so its zero set (the stamp's surface) comes back whole however
    /// far it stands from the plane.
    pub fn field(&self, p: Vec3, t: f32) -> Option<f32> {
        let d = p - self.center;
        let q = Vec3::new(d.dot(self.u), d.dot(self.v), d.dot(self.n)) / self.scale;
        // Inverted, the stamp is turned over through its base plane and
        // its D negated: a horn becomes a hole of its shape.
        let delta = if self.invert { -self.vdm.delta(q * Vec3::new(1.0, 1.0, -1.0)) } else { self.vdm.delta(q) };
        if delta == 0.0 {
            return None;
        }
        let k = self.scale;
        let lim = self.limit.min(k * self.vdm.limit);
        let plane = q.z.clamp(-self.vdm.limit, self.vdm.limit);
        let stamped = (k * (plane + delta)).clamp(-lim, lim);
        Some(t.clamp(-lim, lim) + stamped - (k * q.z).clamp(-lim, lim))
    }
}
