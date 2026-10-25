//! The persistent sculpt mask (docs/SCULPT_PLAN.md 3.5 `sculpt_mask`):
//! a sparse grid of u8 values on the clay's surface band, 0 free to 255
//! frozen. Every brush moves a point by 1 - mask there (see
//! `sculpt::engine::Gate`); the transform brush moves what is unmasked.
//!
//! The `mask` tool's actions are records on the clay's mask ledger
//! (`ledger.rs`, layer "mask"): painting (`mask_paint` strokes),
//! selections (lasso and rect through a stored camera, by cavity,
//! curvature, normal, height, blobs, polygroups, connected surface),
//! morphology on the band (grow, shrink, blur, sharpen), invert, clear
//! and all. Selections set, add to or take from the mask (`mode`).
//!
//! Stored like the paint layer: Arc-shared bricks compared by identity,
//! deflated in files. It is named `sculpt_mask` in code and files:
//! `Clay::masks` are the smart materials' painted channels
//! (`from_channel` / `to_channel` copy between the two).

use std::collections::HashMap;
use std::sync::Arc;

use glam::{IVec3, Mat4, Vec3, Vec4};
use serde::{Deserialize, Serialize};

use crate::band::Band;
use crate::brush::spec::Camera;
use crate::clay::{BRICK_VALUES, Clay, Field, MAX_BRICKS, split};
use crate::ledger::Hash128;

type Brick = Arc<[u8; BRICK_VALUES]>;

/// Shared copy-on-write and compared by identity, like `clay::Detail`.
#[derive(Clone, Debug)]
pub struct SculptMask(Arc<MaskGrid>);

#[derive(Clone, Debug, Default)]
pub struct MaskGrid {
    pub voxel: f32,
    bricks: HashMap<[i32; 3], Brick>,
}

impl PartialEq for SculptMask {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl SculptMask {
    pub fn new(voxel: f32) -> Self {
        SculptMask(Arc::new(MaskGrid { voxel, bricks: HashMap::new() }))
    }

    /// The grid's identity, as `==` compares it (cheap to hash).
    pub fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub fn grid(&self) -> &MaskGrid {
        &self.0
    }

    pub fn grid_mut(&mut self) -> &mut MaskGrid {
        Arc::make_mut(&mut self.0)
    }

    /// The same mask mirrored across x = 0.
    pub fn mirrored_x(&self) -> SculptMask {
        let mut out = MaskGrid { voxel: self.0.voxel, bricks: HashMap::new() };
        for (key, brick) in &self.0.bricks {
            for (at, &v) in brick.iter().enumerate().filter(|(_, v)| **v != 0) {
                let l = [at as i32 % 8, at as i32 / 8 % 8, at as i32 / 64];
                out.set([-(key[0] * 8 + l[0]), key[1] * 8 + l[1], key[2] * 8 + l[2]], v);
            }
        }
        SculptMask(Arc::new(out))
    }

    pub fn is_sane(&self) -> bool {
        let g = &self.0;
        g.voxel.is_finite() && (1e-5..=1e3).contains(&g.voxel) && g.bricks.len() <= MAX_BRICKS && g.bricks.keys().all(|k| k.iter().all(|c| c.abs() < 1 << 20))
    }
}

impl MaskGrid {
    pub fn get(&self, i: [i32; 3]) -> u8 {
        let (key, at) = split(i);
        self.bricks.get(&key).map_or(0, |b| b[at])
    }

    pub fn set(&mut self, i: [i32; 3], v: u8) {
        let (key, at) = split(i);
        if v == 0 && !self.bricks.contains_key(&key) {
            return;
        }
        Arc::make_mut(self.bricks.entry(key).or_insert_with(|| Arc::new([0; BRICK_VALUES])))[at] = v;
    }

    pub fn brick_count(&self) -> usize {
        self.bricks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    pub(crate) fn bricks(&self) -> impl Iterator<Item = (&[i32; 3], &Brick)> {
        self.bricks.iter()
    }

    pub(crate) fn brick(&self, key: &[i32; 3]) -> Option<&Brick> {
        self.bricks.get(key)
    }

    fn prune(&mut self) {
        self.bricks.retain(|_, b| b.iter().any(|v| *v != 0));
    }

    /// The mask at `p`, 0..1: trilinear between grid points; exactly the
    /// grid value (v / 255) on a grid point, so 255 is exactly 1.
    pub fn value(&self, p: Vec3) -> f32 {
        if self.bricks.is_empty() || !p.is_finite() {
            return 0.0;
        }
        let g = p / self.voxel;
        if g.abs().max_element() > (1 << 28) as f32 {
            return 0.0;
        }
        let r = g.round();
        if (g - r).abs().max_element() < 1e-3 {
            return self.get(r.as_ivec3().to_array()) as f32 / 255.0;
        }
        let f = g.floor();
        let t = g - f;
        let i = f.as_ivec3();
        let mut sum = 0.0f32;
        for n in 0..8 {
            let (dx, dy, dz) = (n & 1, (n >> 1) & 1, (n >> 2) & 1);
            let w = (if dx == 1 { t.x } else { 1.0 - t.x }) * (if dy == 1 { t.y } else { 1.0 - t.y }) * (if dz == 1 { t.z } else { 1.0 - t.z });
            sum += w * self.get([i.x + dx, i.y + dy, i.z + dz]) as f32;
        }
        sum / 255.0
    }

    /// Every nonzero value: (grid point, value).
    pub fn values(&self) -> impl Iterator<Item = ([i32; 3], u8)> + '_ {
        self.bricks.iter().flat_map(|(key, b)| {
            b.iter().enumerate().filter(|(_, v)| **v != 0).map(move |(at, v)| {
                let l = [at as i32 % 8, at as i32 / 8 % 8, at as i32 / 64];
                ([key[0] * 8 + l[0], key[1] * 8 + l[1], key[2] * 8 + l[2]], *v)
            })
        })
    }
}

/// File form: one byte per grid point, every brick deflated together.
#[derive(Serialize, Deserialize)]
struct MaskFile {
    voxel: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packed: Option<String>,
}

impl Serialize for SculptMask {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let g = &self.0;
        let mut keys: Vec<[i32; 3]> = g.bricks.keys().copied().collect();
        keys.sort();
        let values: Vec<u8> = keys.iter().flat_map(|k| g.bricks[k].iter().copied()).collect();
        let packed = (!keys.is_empty()).then(|| crate::clay::pack(&keys, &values, 1, false));
        MaskFile { voxel: g.voxel, packed }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for SculptMask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::clay::in_inflate_budget_with(d, |d| {
            let file = MaskFile::deserialize(d)?;
            let mut bricks = HashMap::new();
            if let Some(text) = file.packed {
                let (keys, values) = crate::clay::unpack(&text, 1, false, MAX_BRICKS).map_err(|e| serde::de::Error::custom(format!("mask bricks: {e}")))?;
                for (at, bytes) in keys.into_iter().zip(values.chunks_exact(BRICK_VALUES)) {
                    bricks.insert(at, Arc::new(<[u8; BRICK_VALUES]>::try_from(bytes).expect("brick sized")));
                }
            }
            Ok(SculptMask(Arc::new(MaskGrid { voxel: file.voxel, bricks })))
        })
    }
}

/// How a selection combines with the mask there.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The selection becomes the mask.
    #[default]
    Set,
    /// The larger of the two.
    Add,
    /// The mask, less the selection.
    Sub,
}

/// What a selection picks, resolved into the clay's object space.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Select {
    /// Surface whose projection through `camera` (object space to clip,
    /// stored when the record was made) falls in the pixel polygon `px`;
    /// with `through` false, only surface facing the camera.
    Lasso { camera: Camera, px: Vec<[f32; 2]>, through: bool },
    /// Cavities (the smart materials' cavity estimate over `radius`).
    Cavity { radius: f32 },
    /// Ridges and convex edges, the same way.
    Curvature { radius: f32 },
    /// Surface whose normal is within `angle` degrees of `dir`.
    Normal { dir: Vec3, angle: f32 },
    /// Surface with min <= along . p + offset <= max.
    Height { along: Vec3, offset: f32, min: f32, max: f32 },
    /// Surface made by these blobs (as stroke masks see ownership).
    Blobs { names: Vec<String> },
    /// Surface in these polygroups (the grid by hash: `ledger::Frozen`).
    Groups { ids: Vec<u16>, groups: Hash128 },
    /// Surface joined to the band point nearest `seed`.
    Connected { seed: Vec3 },
}

/// A mask action as a record keeps it (the `mask` tool's actions but
/// paint, which records strokes).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MaskOp {
    Clear,
    All,
    Invert,
    Grow { cells: u32 },
    Shrink { cells: u32 },
    Blur { cells: u32 },
    Sharpen { amount: f32 },
    Select { select: Select, mode: Mode },
    /// Copies the mask to a smart-material channel (`Clay::masks`); the
    /// mask itself is unchanged.
    ToChannel { name: String },
}

/// Most cells one grow, shrink or blur may span.
pub const MAX_CELLS: u32 = 64;
/// Most pixels in a lasso.
pub const MAX_LASSO: usize = 4096;

impl MaskOp {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |c: bool, m: &str| if c { Ok(()) } else { Err(m.to_string()) };
        match self {
            MaskOp::Grow { cells } | MaskOp::Shrink { cells } | MaskOp::Blur { cells } => ok((1..=MAX_CELLS).contains(cells), &format!("cells must be 1 to {MAX_CELLS}")),
            MaskOp::Sharpen { amount } => ok(amount.is_finite() && (0.0..=4.0).contains(amount), "sharpen amount must be 0 to 4"),
            MaskOp::Select { select, .. } => match select {
                Select::Lasso { camera, px, .. } => {
                    ok((3..=MAX_LASSO).contains(&px.len()) && px.iter().all(|p| p[0].is_finite() && p[1].is_finite()), &format!("a lasso needs 3 to {MAX_LASSO} finite pixels"))?;
                    let [w, h] = camera.viewport;
                    ok(camera.view.is_finite() && camera.proj.is_finite() && w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0, "the camera needs finite matrices and a positive viewport")
                }
                Select::Cavity { radius } | Select::Curvature { radius } => ok(radius.is_finite() && *radius > 0.0, "radius must be positive"),
                Select::Normal { dir, angle } => ok(dir.is_finite() && dir.length() > 1e-6 && (0.0..=180.0).contains(angle), "by_normal needs a direction and angle 0 to 180 degrees"),
                Select::Height { along, offset, min, max } => ok(along.is_finite() && offset.is_finite() && min.is_finite() && max.is_finite() && min <= max, "by_height needs finite min <= max"),
                Select::Blobs { names } => ok(!names.is_empty() && names.len() <= crate::clay::MAX_BLOBS, "by_blobs needs blob names"),
                Select::Groups { ids, .. } => ok(!ids.is_empty() && ids.len() <= 1024, "by_groups needs 1 to 1024 ids"),
                Select::Connected { seed } => ok(seed.is_finite(), "connected needs a finite seed point"),
            },
            MaskOp::ToChannel { name } => ok(!name.trim().is_empty() && name.len() <= 64, "channel names are 1 to 64 characters"),
            _ => Ok(()),
        }
    }

    /// The same action for the clay mirrored across x = 0.
    pub fn mirrored_x(&self) -> MaskOp {
        let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
        match self {
            MaskOp::Select { select, mode } => {
                let select = match select {
                    Select::Lasso { camera, px, through } => Select::Lasso { camera: Camera { view: camera.view * Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)), ..*camera }, px: px.clone(), through: *through },
                    Select::Normal { dir, angle } => Select::Normal { dir: flip(*dir), angle: *angle },
                    Select::Height { along, offset, min, max } => Select::Height { along: flip(*along), offset: *offset, min: *min, max: *max },
                    Select::Connected { seed } => Select::Connected { seed: flip(*seed) },
                    Select::Blobs { names } => Select::Blobs { names: names.clone() },
                    s => s.clone(),
                };
                MaskOp::Select { select, mode: *mode }
            }
            op => op.clone(),
        }
    }
}

/// An orthographic camera looking along a named view, in world space,
/// whose pixels are world units across the view (offset by `e`, so all
/// are positive): `at` [h, v] pairs read as they do in screen frames.
pub fn named_camera(view: &str, e: f32) -> Result<(Camera, fn([f32; 2], f32) -> [f32; 2]), String> {
    // (across, up, forward), as `brush::resolve` shoots named-view rays.
    let (r, u, f) = match view {
        "front" => (Vec3::X, Vec3::Y, Vec3::NEG_Z),
        "back" => (Vec3::X, Vec3::Y, Vec3::Z),
        "right" => (Vec3::Z, Vec3::Y, Vec3::NEG_X),
        "left" => (Vec3::Z, Vec3::Y, Vec3::X),
        "top" => (Vec3::X, Vec3::Z, Vec3::NEG_Y),
        "bottom" => (Vec3::X, Vec3::Z, Vec3::Y),
        other => return Err(format!("unknown view {other}; use front, back, right, left, top or bottom, or give camera and px")),
    };
    let view = Mat4::from_cols(Vec4::new(r.x, u.x, -f.x, 0.0), Vec4::new(r.y, u.y, -f.y, 0.0), Vec4::new(r.z, u.z, -f.z, 0.0), Vec4::W);
    let proj = Mat4::orthographic_rh(-e, e, -e, e, -e, e);
    Ok((Camera { view, proj, viewport: [2.0 * e, 2.0 * e] }, |a, e| [a[0] + e, e - a[1]]))
}

/// The mask cell a clay uses (its detail cell, as paint does).
pub fn voxel_of(clay: &Clay) -> Result<f32, String> {
    if let Some(m) = &clay.sculpt_mask {
        return Ok(m.grid().voxel);
    }
    clay.detail_cell.or_else(|| clay.detail.as_ref().map(|d| d.voxel())).or_else(|| clay.cell_size()).ok_or_else(|| "add blobs before masking".into())
}

/// Applies `op` to `mask` (None: empty) on `clay`; `groups` resolves a
/// by_groups snapshot.
pub fn act(clay: &Clay, mask: Option<&SculptMask>, op: &MaskOp, groups: &dyn Fn(Hash128) -> Option<crate::groups::Groups>) -> Result<Option<SculptMask>, String> {
    op.validate()?;
    match op {
        MaskOp::Clear => return Ok(None),
        MaskOp::ToChannel { .. } => return Ok(mask.cloned()),
        _ => {}
    }
    let voxel = voxel_of(clay)?;
    let field = crate::band::field(clay, voxel);
    let band = Band::with(clay, &field, voxel)?;
    let old = mask.cloned().unwrap_or_else(|| SculptMask::new(voxel));
    let cur: Vec<u8> = band.keys.iter().map(|i| sample_u8(old.grid(), voxel, *i)).collect();
    let new: Vec<u8> = match op {
        MaskOp::All => vec![255; band.len()],
        MaskOp::Invert => cur.iter().map(|v| 255 - v).collect(),
        MaskOp::Grow { cells } => morph(&band, cur, *cells, true),
        MaskOp::Shrink { cells } => morph(&band, cur, *cells, false),
        MaskOp::Blur { cells } => quantize(&blur(&band, &floats(&cur), *cells)),
        MaskOp::Sharpen { amount } => {
            let m = floats(&cur);
            let b = blur(&band, &m, 1);
            quantize(&m.iter().zip(&b).map(|(m, b)| m + amount * (m - b)).collect::<Vec<_>>())
        }
        MaskOp::Select { select, mode } => {
            let sel = selection(clay, &field, &band, select, groups)?;
            cur.iter()
                .zip(&sel)
                .map(|(&m, &s)| match mode {
                    Mode::Set => q(s),
                    Mode::Add => m.max(q(s)),
                    Mode::Sub => q(m as f32 / 255.0 * (1.0 - s)),
                })
                .collect()
        }
        MaskOp::Clear | MaskOp::ToChannel { .. } => unreachable!("handled above"),
    };
    Ok(write(old, &band, &new))
}

/// The grid with the band's points set to `values`; None when it is empty.
/// Points off the band keep what they had.
pub(crate) fn write(mut mask: SculptMask, band: &Band, values: &[u8]) -> Option<SculptMask> {
    let same_cell = mask.grid().voxel == band.voxel;
    let g = mask.grid_mut();
    if !same_cell {
        // A mask kept at another cell is resampled onto this band.
        *g = MaskGrid { voxel: band.voxel, bricks: HashMap::new() };
    }
    for (i, v) in band.keys.iter().zip(values) {
        if g.get(*i) != *v {
            g.set(*i, *v);
        }
    }
    g.prune();
    (!g.is_empty()).then_some(mask)
}

/// The mask's value at band point `i` (a point of its own grid, or
/// sampled from a mask kept at another cell).
fn sample_u8(g: &MaskGrid, voxel: f32, i: [i32; 3]) -> u8 {
    if g.voxel == voxel {
        g.get(i)
    } else {
        q(g.value(IVec3::from(i).as_vec3() * voxel))
    }
}

pub(crate) fn q(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn floats(v: &[u8]) -> Vec<f32> {
    v.iter().map(|v| *v as f32 / 255.0).collect()
}

fn quantize(v: &[f32]) -> Vec<u8> {
    v.iter().map(|v| q(*v)).collect()
}

/// Grow (max) or shrink (min) by `cells` steps through band neighbours.
fn morph(band: &Band, mut m: Vec<u8>, cells: u32, grow: bool) -> Vec<u8> {
    for _ in 0..cells {
        let prev = m.clone();
        for (k, out) in m.iter_mut().enumerate() {
            for n in band.neighbours(k).into_iter().flatten() {
                *out = if grow { (*out).max(prev[n]) } else { (*out).min(prev[n]) };
            }
        }
    }
    m
}

/// `cells` passes of diffusion along each axis between band neighbours:
/// what one point gives the next it takes from it, so the mask's sum is
/// kept (before rounding).
fn blur(band: &Band, m: &[f32], cells: u32) -> Vec<f32> {
    let mut m = m.to_vec();
    for _ in 0..cells {
        for axis in 0..3 {
            let prev = m.clone();
            for (k, out) in m.iter_mut().enumerate() {
                let nb = band.neighbours(k);
                for n in [nb[axis * 2], nb[axis * 2 + 1]].into_iter().flatten() {
                    *out += (prev[n] - prev[k]) / 3.0;
                }
            }
        }
    }
    m
}

/// What a selection picks at each band point, 0..1.
fn selection(clay: &Clay, field: &Field, band: &Band, select: &Select, groups: &dyn Fn(Hash128) -> Option<crate::groups::Groups>) -> Result<Vec<f32>, String> {
    let n = band.len();
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    Ok(match select {
        Select::Lasso { camera, px, through } => {
            let normals = (!through).then(|| band.normals(field));
            let clip = camera.proj * camera.view;
            let ortho = camera.proj.w_axis.w == 1.0;
            let eye = camera.view.inverse().transform_point3(Vec3::ZERO);
            let forward = camera.view.inverse().transform_vector3(Vec3::NEG_Z).normalize_or_zero();
            let [w, h] = camera.viewport;
            (0..n)
                .map(|k| {
                    let p = band.point(k);
                    let c = clip * p.extend(1.0);
                    if c.w <= 1e-9 {
                        return 0.0;
                    }
                    let (x, y) = ((c.x / c.w + 1.0) * 0.5 * w, (1.0 - c.y / c.w) * 0.5 * h);
                    if !inside(px, x, y) {
                        return 0.0;
                    }
                    let facing = normals.as_ref().is_none_or(|ns| if ortho { ns[k].dot(-forward) > 0.0 } else { ns[k].dot(eye - p) > 0.0 });
                    flag(facing)
                })
                .collect()
        }
        Select::Cavity { radius } => band.bends(field, *radius).into_iter().map(|b| crate::weather::wear_of(b)[1]).collect(),
        Select::Curvature { radius } => band.bends(field, *radius).into_iter().map(|b| crate::weather::wear_of(b)[0]).collect(),
        Select::Normal { dir, angle } => {
            let (d, cos) = (dir.normalize(), angle.to_radians().cos());
            band.normals(field).into_iter().map(|nm| flag(nm.dot(d) >= cos - 1e-6)).collect()
        }
        Select::Height { along, offset, min, max } => (0..n).map(|k| flag((*min..=*max).contains(&(along.dot(band.point(k)) + offset)))).collect(),
        Select::Blobs { names } => {
            for name in names {
                if !clay.blobs.iter().any(|b| &b.name == name) {
                    return Err(format!("unknown blob {name}"));
                }
            }
            band.per_point(field, |p, _| flag(field.nearest_named(p, names)))
        }
        Select::Groups { ids, groups: hash } => {
            let g = groups(*hash).ok_or("the polygroups this selection read are gone from the clay; select again")?;
            (0..n).map(|k| flag(ids.contains(&g.label(band.point(k))))).collect()
        }
        Select::Connected { seed } => {
            let s = band.nearest(*seed, band.width * 2.0 + band.voxel * 4.0).ok_or("connected: the seed point is not on the clay's surface")?;
            band.flood(&[s], None).into_iter().map(flag).collect()
        }
    })
}

/// Even-odd point in polygon.
fn inside(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

/// Mask painting: raises the mask toward the stroke's strength by its
/// falloff (invert lowers it), on band points only.
pub fn paint(clay: &Clay, mask: Option<&SculptMask>, stroke: &crate::brush::resolve::Resolved) -> Result<Option<SculptMask>, String> {
    let voxel = voxel_of(clay)?;
    let field = crate::band::field(clay, voxel);
    let width = crate::band::width(clay, voxel);
    let carved = crate::layers::surface_detail(clay).map_or(0.0, |d| d.grid().max_abs());
    let mut m = mask.cloned().unwrap_or_else(|| SculptMask::new(voxel));
    if m.grid().voxel != voxel {
        return Err("the mask's cell changed; clear the mask first".into());
    }
    let alpha = crate::sculpt::engine::alpha_of(clay, &stroke.params)?;
    let params = &stroke.params;
    let target = params.strength.clamp(0.0, 1.0);
    for dab in &stroke.dabs {
        dab.check()?;
        let changes = marks(clay, &field, voxel, width + carved, width, dab, params, alpha.as_ref(), stroke.mask.as_ref())?;
        let g = m.grid_mut();
        for (i, w) in changes {
            let old = g.get(i) as f32 / 255.0;
            let v = if params.invert { old.min(1.0 - w * target).min(old) } else { old.max(w * target) };
            g.set(i, q(v));
        }
    }
    let g = m.grid_mut();
    g.prune();
    if g.brick_count() > MAX_BRICKS {
        return Err(format!("the mask is too large (over {MAX_BRICKS} bricks)"));
    }
    Ok((!g.is_empty()).then_some(m))
}

/// Grid points of the surface band under one mask or group dab, with
/// the brush weight there (falloff x gain, alpha), in a fixed order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn marks(clay: &Clay, field: &Field, voxel: f32, reach: f32, width: f32, dab: &crate::brush::resolve::Dab, params: &crate::brush::resolve::Params, alpha: Option<&crate::brush::alpha::Alpha>, limits: Option<&crate::brush::spec::StrokeMask>) -> Result<Vec<([i32; 3], f32)>, String> {
    use crate::sculpt::engine::{Sweep, allowed};
    let radius = dab.radius.max(voxel);
    if !radius.is_finite() {
        return Err("radius must be finite".into());
    }
    let sweep = Sweep { points: &dab.points, radius, radii: dab.radii.as_ref().map(|r| r.iter().map(|r| r.max(voxel)).collect()), gains: dab.gains.as_deref() };
    let (lo, hi) = dab.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
    let cells = ((hi - lo + 2.0 * radius) / voxel + 2.0).as_dvec3();
    if !cells.is_finite() || cells.x * cells.y * cells.z > 4.0e9 {
        return Err(format!("stroke is too big for the mask grid (cell size {voxel:.4})"));
    }
    let stamp = match (alpha, &params.alpha) {
        (Some(image), Some(a)) => {
            let c = dab.points[0];
            let n = dab.normal.filter(|n| n.length() > 1e-6).map(|n| n.normalize()).unwrap_or_else(|| field.sculpt_normal(c, &field.near(c - radius, c + radius), voxel));
            let (u, v) = if a.mode == crate::brush::alpha::Mode::Drag { crate::brush::alpha::frame(n, dab.tangent, dab.flip, a.rotate) } else { crate::brush::alpha::frame(n, Some(dab.upright.unwrap_or(Vec3::X)), dab.flip, a.rotate) };
            Some(crate::brush::alpha::Stamp { alpha: image, params: a, center: c, u, v, half: radius * a.scale })
        }
        _ => None,
    };
    let i0 = ((lo - radius) / voxel).floor().as_ivec3();
    let i1 = ((hi + radius) / voxel).ceil().as_ivec3();
    let mut out = Vec::new();
    let mut visited = 0usize;
    for tz in (i0.z..=i1.z).step_by(8) {
        for ty in (i0.y..=i1.y).step_by(8) {
            for tx in (i0.x..=i1.x).step_by(8) {
                let c0 = IVec3::new(tx, ty, tz);
                let c1 = (c0 + 7).min(i1);
                let (blo, bhi) = (c0.as_vec3() * voxel, c1.as_vec3() * voxel);
                let (center, half) = ((blo + bhi) * 0.5, (bhi - blo).length() * 0.5);
                if sweep.distance(center) > radius + half {
                    continue;
                }
                let list = field.near(blo - reach, bhi + reach);
                if list.is_empty() || field.far_side(center, &list, half + reach).is_some() {
                    continue;
                }
                visited += 512;
                if visited > 40_000_000 {
                    return Err(format!("stroke covers too much surface for the mask grid (cell size {voxel:.4}); split it"));
                }
                for z in c0.z..=c1.z {
                    for y in c0.y..=c1.y {
                        for x in c0.x..=c1.x {
                            let p = IVec3::new(x, y, z).as_vec3() * voxel;
                            let mut w = (sweep.weight(&params.curve, p) * dab.gain).clamp(0.0, 1.0);
                            if let Some(s) = &stamp {
                                w *= s.mask(p);
                            }
                            if w <= 0.0 || field.sample(p, &list).0.abs() > width || !allowed(limits, field, p) {
                                continue;
                            }
                            out.push(([x, y, z], w));
                        }
                    }
                }
            }
        }
    }
    let _ = clay;
    Ok(out)
}

/// Mask coverage of a mesh: the area-weighted mean mask over its
/// triangles (vertex values averaged), 0..1.
pub fn coverage(mask: Option<&SculptMask>, mesh: &crate::mesh::Mesh) -> f32 {
    let Some(m) = mask else { return 0.0 };
    let v: Vec<f32> = mesh.vertices.iter().map(|v| m.grid().value(Vec3::from(v.pos))).collect();
    let (mut sum, mut area) = (0.0f64, 0.0f64);
    for t in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize);
        let (pa, pb, pc) = (Vec3::from(mesh.vertices[a].pos), Vec3::from(mesh.vertices[b].pos), Vec3::from(mesh.vertices[c].pos));
        let s = ((pb - pa).cross(pc - pa).length() * 0.5) as f64;
        sum += s * ((v[a] + v[b] + v[c]) / 3.0) as f64;
        area += s;
    }
    if area > 0.0 { (sum / area) as f32 } else { 0.0 }
}
