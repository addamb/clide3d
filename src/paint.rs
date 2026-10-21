//! Color layer for clay: paint on top of the blob colors.
//!
//! Paint is stored like the sculpt detail layer: a sparse grid of RGBA values
//! (sRGB color plus coverage) in the clay object's space. A mesh vertex takes
//! the blob color, then the paint at its position laid over it by coverage.
//! Paint stays in place in space, like sculpt detail.
//!
//! Ways to paint:
//! - `stroke`: a soft brush along a path (or `erase` to remove paint),
//! - `splat`: color given per surface point (used to fill and to project the
//!   reference image, see `app/eyes.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use glam::{IVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::clay::{BRICK_VALUES, Clay, MAX_BRICKS, split};

type Rgba = [u8; 4];

/// Shared copy-on-write and compared by identity, like `clay::Detail`.
#[derive(Clone, Debug)]
pub struct Paint(Arc<PaintGrid>);

#[derive(Clone, Debug, Default)]
pub struct PaintGrid {
    pub voxel: f32,
    bricks: HashMap<[i32; 3], Box<[Rgba; BRICK_VALUES]>>,
}

impl PartialEq for Paint {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Paint {
    pub fn new(voxel: f32) -> Self {
        Paint(Arc::new(PaintGrid { voxel, bricks: HashMap::new() }))
    }

    pub fn grid(&self) -> &PaintGrid {
        &self.0
    }

    pub fn grid_mut(&mut self) -> &mut PaintGrid {
        Arc::make_mut(&mut self.0)
    }

    /// The same paint on a grid of `voxel` spacing.
    pub fn resampled(&self, voxel: f32) -> Result<Paint, String> {
        let old = &self.0;
        let mut out = PaintGrid { voxel, bricks: HashMap::new() };
        let side = 8.0 * old.voxel;
        for key in old.bricks.keys() {
            let lo = Vec3::new(key[0] as f32, key[1] as f32, key[2] as f32) * side - old.voxel;
            let hi = lo + Vec3::splat(side + 2.0 * old.voxel);
            let (i0, i1) = ((lo / voxel).floor().as_ivec3(), (hi / voxel).ceil().as_ivec3());
            for z in i0.z..=i1.z {
                for y in i0.y..=i1.y {
                    for x in i0.x..=i1.x {
                        let p = Vec3::new(x as f32, y as f32, z as f32) * voxel;
                        if let Some((rgb, a)) = old.sample(p) {
                            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                            out.set([x, y, z], [q(rgb.x), q(rgb.y), q(rgb.z), q(a)]);
                        }
                    }
                }
            }
            if out.bricks.len() > MAX_BRICKS {
                return Err(format!("detail_cell {voxel} needs more than {MAX_BRICKS} paint blocks; use a larger cell"));
            }
        }
        Ok(Paint(Arc::new(out)))
    }

    /// The same paint mirrored across x = 0 (grid point x -> -x).
    pub fn mirrored_x(&self) -> Paint {
        let mut out = PaintGrid { voxel: self.0.voxel, bricks: HashMap::new() };
        let db = 8;
        for (key, brick) in &self.0.bricks {
            for (at, &v) in brick.iter().enumerate().filter(|(_, v)| v[3] > 0) {
                let l = [at as i32 % db, at as i32 / db % db, at as i32 / (db * db)];
                out.set([-(key[0] * db + l[0]), key[1] * db + l[1], key[2] * db + l[2]], v);
            }
        }
        Paint(Arc::new(out))
    }

    pub fn is_sane(&self) -> bool {
        let g = &self.0;
        g.voxel.is_finite()
            && (1e-5..=1e3).contains(&g.voxel)
            && g.bricks.len() <= MAX_BRICKS
            && g.bricks.keys().all(|k| k.iter().all(|c| c.abs() < 1 << 20))
    }
}

impl PaintGrid {
    pub fn get(&self, i: [i32; 3]) -> Rgba {
        let (key, at) = split(i);
        self.bricks.get(&key).map_or([0; 4], |b| b[at])
    }

    pub fn set(&mut self, i: [i32; 3], v: Rgba) {
        let (key, at) = split(i);
        if v[3] == 0 && !self.bricks.contains_key(&key) {
            return;
        }
        self.bricks.entry(key).or_insert_with(|| Box::new([[0; 4]; BRICK_VALUES]))[at] = v;
    }

    pub fn brick_count(&self) -> usize {
        self.bricks.len()
    }

    fn prune(&mut self) {
        self.bricks.retain(|_, b| b.iter().any(|v| v[3] > 0));
    }

    /// Whether any paint is stored within a cell of `p`.
    pub fn near(&self, p: Vec3) -> bool {
        if self.bricks.is_empty() {
            return false;
        }
        let side = 8.0 * self.voxel;
        let (a, b) = (((p - self.voxel) / side).floor().as_ivec3(), ((p + self.voxel) / side).floor().as_ivec3());
        (a.z..=b.z).any(|z| (a.y..=b.y).any(|y| (a.x..=b.x).any(|x| self.bricks.contains_key(&[x, y, z]))))
    }

    /// Lays the paint at `p` over `base` (sRGB 0..1).
    pub fn apply(&self, p: Vec3, base: Vec3) -> Vec3 {
        match self.sample(p) {
            Some((rgb, a)) => base.lerp(rgb, a.min(1.0)),
            None => base,
        }
    }

    /// Coverage at `p`, 0..1 (a painted mask channel's value).
    pub fn coverage(&self, p: Vec3) -> f32 {
        self.sample(p).map_or(0.0, |(_, a)| a.min(1.0))
    }

    /// Paint color and coverage at `p` (None where unpainted).
    fn sample(&self, p: Vec3) -> Option<(Vec3, f32)> {
        if self.bricks.is_empty() {
            return None;
        }
        let g = p / self.voxel;
        let f = g.floor();
        let t = g - f;
        let i = f.as_ivec3();
        // Interpolate color weighted by coverage, so unpainted corners do
        // not darken the edge of a painted area.
        let (mut rgb, mut a) = (Vec3::ZERO, 0.0f32);
        for n in 0..8 {
            let (dx, dy, dz) = (n & 1, (n >> 1) & 1, (n >> 2) & 1);
            let w = (if dx == 1 { t.x } else { 1.0 - t.x }) * (if dy == 1 { t.y } else { 1.0 - t.y }) * (if dz == 1 { t.z } else { 1.0 - t.z });
            let c = self.get([i.x + dx, i.y + dy, i.z + dz]);
            let ca = c[3] as f32 / 255.0 * w;
            rgb += Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0 * ca;
            a += ca;
        }
        (a > 1e-6).then(|| (rgb / a, a))
    }

    /// Lays `color` over grid point `i` with coverage `w` (0..1).
    fn over(&mut self, i: [i32; 3], color: Vec3, w: f32) {
        if w <= 0.0 {
            return;
        }
        let old = self.get(i);
        let a = old[3] as f32 / 255.0;
        let rgb = Vec3::new(old[0] as f32, old[1] as f32, old[2] as f32) / 255.0;
        // Unpainted points take the new color outright; painted ones blend.
        let mixed = if a <= 0.0 { color } else { rgb.lerp(color, w) };
        let na = a + (1.0 - a) * w;
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        self.set(i, [q(mixed.x), q(mixed.y), q(mixed.z), q(na)]);
    }

    fn erase(&mut self, i: [i32; 3], w: f32) {
        let mut old = self.get(i);
        if old[3] > 0 {
            old[3] = (old[3] as f32 * (1.0 - w.clamp(0.0, 1.0))).round() as u8;
            self.set(i, old);
        }
    }
}

/// File form: RGBA bytes per brick, base64.
#[derive(Serialize, Deserialize)]
struct PaintFile {
    voxel: f32,
    bricks: Vec<BrickFile>,
}

#[derive(Serialize, Deserialize)]
struct BrickFile {
    at: [i32; 3],
    data: String,
}

impl Serialize for Paint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let g = &self.0;
        let mut keys: Vec<&[i32; 3]> = g.bricks.keys().collect();
        keys.sort();
        let b64 = base64::engine::general_purpose::STANDARD;
        let bricks = keys
            .into_iter()
            .map(|k| BrickFile { at: *k, data: b64.encode(g.bricks[k].iter().flatten().copied().collect::<Vec<u8>>()) })
            .collect();
        PaintFile { voxel: g.voxel, bricks }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Paint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let file = PaintFile::deserialize(d)?;
        if file.bricks.len() > MAX_BRICKS {
            return Err(serde::de::Error::custom("too many paint bricks"));
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut bricks = HashMap::new();
        for b in file.bricks {
            let bytes = b64.decode(&b.data).map_err(serde::de::Error::custom)?;
            if bytes.len() != BRICK_VALUES * 4 {
                return Err(serde::de::Error::custom("bad paint brick size"));
            }
            let mut values = Box::new([[0u8; 4]; BRICK_VALUES]);
            for (v, c) in values.iter_mut().zip(bytes.chunks_exact(4)) {
                *v = [c[0], c[1], c[2], c[3]];
            }
            bricks.insert(b.at, values);
        }
        Ok(Paint(Arc::new(PaintGrid { voxel: file.voxel, bricks })))
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct PaintStroke {
    /// One point (a dab) or a path, in the clay object's space.
    pub points: Vec<Vec3>,
    pub radius: f32,
    /// sRGB 0..1. Not needed to erase.
    #[serde(default)]
    pub color: Option<[f32; 3]>,
    /// Opacity at the center, 0..1 (default 1).
    #[serde(default = "one")]
    pub strength: f32,
    /// 0 = soft edge, 1 = hard edge (default 0.5).
    #[serde(default = "half")]
    pub hardness: f32,
    #[serde(default)]
    pub erase: bool,
    #[serde(default)]
    pub mirror: bool,
    #[serde(default)]
    pub mask: Option<crate::sculpt::Mask>,
}

fn one() -> f32 {
    1.0
}

fn half() -> f32 {
    0.5
}

const MAX_POINTS: usize = 512;
/// Most grid points one stroke may visit (near the surface).
const MAX_REGION: f64 = 40_000_000.0;
/// Largest box (in grid points) a stroke may span.
const MAX_BOX: f64 = 4.0e9;
/// Tile side (grid points); tiles away from the surface are skipped.
const TILE: i32 = 8;

/// The paint layer, created with the clay's cell size on first use.
fn layer(clay: &Clay) -> Result<Paint, String> {
    if let Some(p) = &clay.paint {
        return Ok(p.clone());
    }
    let voxel = clay.detail_cell.or_else(|| clay.detail.as_ref().map(|d| d.voxel())).or_else(|| clay.cell_size()).ok_or("add blobs before painting")?;
    Ok(Paint::new(voxel))
}

fn path_distance(p: Vec3, points: &[Vec3]) -> f32 {
    if points.len() == 1 {
        return (p - points[0]).length();
    }
    points
        .windows(2)
        .map(|w| {
            let ab = w[1] - w[0];
            let t = if ab.length_squared() < 1e-12 { 0.0 } else { ((p - w[0]).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
            (p - (w[0] + ab * t)).length()
        })
        .fold(f32::MAX, f32::min)
}

/// Brush falloff with adjustable hardness: 1 inside `hardness * radius`,
/// easing to 0 at the radius.
fn falloff(d: f32, radius: f32, hardness: f32) -> f32 {
    let inner = radius * hardness.clamp(0.0, 0.95);
    if d <= inner {
        return 1.0;
    }
    let t = ((d - inner) / (radius - inner)).clamp(0.0, 1.0);
    let s = 1.0 - t;
    s * s * (3.0 - 2.0 * s)
}

/// Applies one paint stroke (and its mirror) to the clay.
pub fn stroke(clay: &mut Clay, s: &PaintStroke) -> Result<usize, String> {
    if s.points.is_empty() || s.points.len() > MAX_POINTS {
        return Err(format!("a stroke needs 1 to {MAX_POINTS} points"));
    }
    if !s.points.iter().all(|p| p.is_finite()) || !s.radius.is_finite() || s.radius <= 0.0 {
        return Err("points and radius must be finite, radius > 0".into());
    }
    let color = match (s.erase, s.color) {
        (true, _) => Vec3::ZERO,
        (false, Some(c)) => Vec3::from(c).clamp(Vec3::ZERO, Vec3::ONE),
        (false, None) => return Err("a paint stroke needs color [r, g, b] (or erase: true)".into()),
    };
    let mut paint = layer(clay)?;
    let voxel = paint.grid().voxel;
    let radius = s.radius.max(voxel);
    let strength = if s.strength.is_finite() { s.strength.clamp(0.0, 1.0) } else { 1.0 };
    let mut paths = vec![s.points.clone()];
    if s.mirror {
        paths.push(s.points.iter().map(|p| Vec3::new(-p.x, p.y, p.z)).collect());
    }
    // Paint only matters at the surface: visit tiles near it, so a wide
    // brush on a fine grid stays cheap.
    let field = crate::clay::Field::new(clay, voxel);
    let carved = clay.detail.as_ref().map_or(0.0, |d| d.grid().max_abs());
    // Mesh vertices sit up to about half a mesh cell off the true surface.
    let band = 0.5 * clay.cell_size().unwrap_or(voxel) + 2.0 * voxel + carved;
    let mut touched = 0;
    let mut visited = 0usize;
    let g = paint.grid_mut();
    for path in &paths {
        let (lo, hi) = path.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        let cells = ((hi - lo + 2.0 * radius) / voxel + 2.0).as_dvec3();
        if !cells.is_finite() || cells.x * cells.y * cells.z > MAX_BOX {
            let most = (MAX_BOX.cbrt() as f32 - 2.0) * voxel / 4.0;
            return Err(format!("stroke is too big for the paint grid (cell size {voxel:.4}): keep radius under about {most:.3}"));
        }
        let i0 = ((lo - radius) / voxel).floor().as_ivec3();
        let i1 = ((hi + radius) / voxel).ceil().as_ivec3();
        for tz in (i0.z..=i1.z).step_by(TILE as usize) {
            for ty in (i0.y..=i1.y).step_by(TILE as usize) {
                for tx in (i0.x..=i1.x).step_by(TILE as usize) {
                    let c0 = IVec3::new(tx, ty, tz);
                    let c1 = (c0 + TILE - 1).min(i1);
                    let (blo, bhi) = (c0.as_vec3() * voxel, c1.as_vec3() * voxel);
                    let half = (bhi - blo).length() * 0.5;
                    let center = (blo + bhi) * 0.5;
                    if path_distance(center, path) > radius + half {
                        continue;
                    }
                    let list = field.near(blo - band, bhi + band);
                    if list.is_empty() || field.far_side(center, &list, half + band).is_some() {
                        continue;
                    }
                    visited += ((c1 - c0 + 1).element_product()) as usize;
                    if visited as f64 > MAX_REGION {
                        return Err(format!("stroke covers too much surface for the paint grid (cell size {voxel:.4}); split it into smaller strokes, or use fill"));
                    }
                    for z in c0.z..=c1.z {
                        for y in c0.y..=c1.y {
                            for x in c0.x..=c1.x {
                                let p = IVec3::new(x, y, z).as_vec3() * voxel;
                                let w = falloff(path_distance(p, path), radius, s.hardness) * strength;
                                if w <= 0.0 {
                                    continue;
                                }
                                if s.mask.as_ref().is_some_and(|m| m.above.is_some_and(|y|p.y<y) || m.below.is_some_and(|y|p.y>y) || m.blobs.as_ref().is_some_and(|names| !field.nearest_named(p,names))) { continue; }
                                touched += 1;
                                if s.erase {
                                    g.erase([x, y, z], w);
                                } else {
                                    g.over([x, y, z], color, w);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    finish(clay, paint)?;
    Ok(touched)
}

/// Paints given surface points: (position in object space, color, coverage).
/// Each sample spreads over the 8 grid points around it.
pub fn splat(clay: &mut Clay, samples: &[(Vec3, Vec3, f32)]) -> Result<usize, String> {
    let mut paint = layer(clay)?;
    let voxel = paint.grid().voxel;
    // Accumulate first so overlapping samples average instead of stacking.
    let mut acc: HashMap<[i32; 3], (Vec3, f32, f32)> = HashMap::new();
    for &(p, color, w) in samples {
        if !p.is_finite() || w <= 0.0 {
            continue;
        }
        let g = p / voxel;
        let f = g.floor();
        let t = g - f;
        let i = f.as_ivec3();
        for n in 0..8 {
            let (dx, dy, dz) = (n & 1, (n >> 1) & 1, (n >> 2) & 1);
            let tw = (if dx == 1 { t.x } else { 1.0 - t.x }) * (if dy == 1 { t.y } else { 1.0 - t.y }) * (if dz == 1 { t.z } else { 1.0 - t.z });
            if tw <= 1e-4 {
                continue;
            }
            let e = acc.entry([i.x + dx, i.y + dy, i.z + dz]).or_insert((Vec3::ZERO, 0.0, 0.0));
            e.0 += color * (tw * w);
            e.1 += tw * w;
            e.2 += tw;
        }
    }
    let count = acc.len();
    let g = paint.grid_mut();
    for (i, (rgb, ww, tw)) in acc {
        if ww > 0.0 {
            g.over(i, rgb / ww, (ww / tw).clamp(0.0, 1.0));
        }
    }
    finish(clay, paint)?;
    Ok(count)
}

fn finish(clay: &mut Clay, mut paint: Paint) -> Result<(), String> {
    let g = paint.grid_mut();
    g.prune();
    if g.brick_count() > MAX_BRICKS {
        return Err(format!("too much paint (over {MAX_BRICKS} bricks); use clear_paint or paint less area"));
    }
    clay.paint = (g.brick_count() > 0).then_some(paint);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ball() -> Clay {
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 1.0, "color": [0.5, 0.5, 0.5] })).unwrap();
        Clay { blobs: vec![blob], resolution: 40, ..Default::default() }
    }

    fn red_stroke(points: &[[f32; 3]], radius: f32) -> PaintStroke {
        PaintStroke {
            points: points.iter().map(|p| Vec3::from(*p)).collect(),
            radius,
            color: Some([1.0, 0.0, 0.0]),
            strength: 1.0,
            hardness: 0.5,
            erase: false,
            mirror: false,
            mask: None,
        }
    }

    fn color_near(c: &Clay, p: Vec3) -> [f32; 3] {
        let mesh = c.build_mesh();
        let v = mesh.vertices.iter().min_by(|a, b| (Vec3::from(a.pos) - p).length().total_cmp(&(Vec3::from(b.pos) - p).length())).unwrap();
        v.color
    }

    #[test]
    fn a_single_soft_dab_matches_a_degenerate_path_and_respects_blob_mask() {
        let mut dab=ball();let mut path=dab.clone();
        let s=red_stroke(&[[0.0,1.0,0.0]],0.35);stroke(&mut dab,&s).unwrap();
        let mut two=s.clone();two.points.push(two.points[0]);stroke(&mut path,&two).unwrap();
        assert_eq!(serde_json::to_string(&dab.paint).unwrap(),serde_json::to_string(&path.paint).unwrap());
        assert!(color_near(&dab,Vec3::Y)[0]>0.9);
        let mut c:Clay=serde_json::from_value(serde_json::json!({"resolution":64,"blobs":[
            {"type":"sphere","name":"armor","center":[-0.3,0,0],"radius":0.2,"blend":0,"color":[0.5,0.5,0.5]},
            {"type":"sphere","name":"roller","center":[0.3,0,0],"radius":0.2,"blend":0,"color":[0.5,0.5,0.5]}
        ]})).unwrap();
        let mut s=red_stroke(&[[0.0,0.0,0.15]],0.8);s.mask=Some(crate::sculpt::Mask{blobs:Some(vec!["armor".into()]),..Default::default()});
        stroke(&mut c,&s).unwrap();assert!(color_near(&c,Vec3::new(-0.3,0.0,0.2))[0]>0.9);assert!(color_near(&c,Vec3::new(0.3,0.0,0.2))[0]<0.55);
    }

    #[test]
    fn a_wide_dab_paints_a_fine_head_sized_clay() {
        // An 8.5 cm dab on a 0.5 mm grid only paints near the surface
        // (elf6 needed 47 small dabs).
        let blob = serde_json::from_value(serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.1, "blend": 0.0, "color": [0.5, 0.5, 0.5] })).unwrap();
        let mut c = Clay { blobs: vec![blob], resolution: 128, ..Default::default() };
        c.set_detail_cell(0.0005).unwrap();
        let n = stroke(&mut c, &red_stroke(&[[0.0, 0.1, 0.0]], 0.085)).unwrap();
        assert!(n > 10_000, "{n}");
        let bricks = c.paint.as_ref().unwrap().grid().brick_count();
        assert!(bricks < MAX_BRICKS / 4, "{bricks} bricks, {n} touched");
        assert!(color_near(&c, Vec3::new(0.0, 0.1, 0.0))[0] > 0.9);
    }

    #[test]
    fn stroke_paints_only_where_it_touches() {
        let mut c = ball();
        stroke(&mut c, &red_stroke(&[[0.0, 1.0, 0.0]], 0.3)).unwrap();
        let top = color_near(&c, Vec3::Y);
        assert!(top[0] > 0.9 && top[1] < 0.1, "top {top:?}");
        let bottom = color_near(&c, -Vec3::Y);
        assert!((bottom[0] - 0.5).abs() < 0.02, "bottom {bottom:?}");
    }

    #[test]
    fn mirror_erase_and_round_trip() {
        let mut c = ball();
        let mut s = red_stroke(&[[0.9, 0.3, 0.0]], 0.3);
        s.mirror = true;
        stroke(&mut c, &s).unwrap();
        assert!(color_near(&c, Vec3::new(-0.95, 0.3, 0.0))[0] > 0.9, "mirrored side painted");

        let text = serde_json::to_string(&c).unwrap();
        let back: Clay = serde_json::from_str(&text).unwrap();
        assert!(color_near(&back, Vec3::new(0.95, 0.3, 0.0))[0] > 0.9, "paint survives JSON");

        let mut e = red_stroke(&[[0.9, 0.3, 0.0]], 0.8);
        e.erase = true;
        e.color = None;
        e.mirror = true;
        stroke(&mut c, &e).unwrap();
        assert!((color_near(&c, Vec3::new(0.95, 0.3, 0.0))[0] - 0.5).abs() < 0.05, "erased");
        assert!(c.paint.is_none(), "empty paint layer is dropped");
    }

    #[test]
    fn splat_colors_points_and_bad_input_is_refused() {
        let mut c = ball();
        let samples: Vec<(Vec3, Vec3, f32)> =
            c.build_mesh().vertices.iter().map(|v| (Vec3::from(v.pos), Vec3::new(0.0, 0.0, 1.0), 1.0)).collect();
        splat(&mut c, &samples).unwrap();
        let side = color_near(&c, Vec3::X);
        assert!(side[2] > 0.9 && side[0] < 0.1, "{side:?}");
        assert!(stroke(&mut c, &red_stroke(&[], 0.2)).is_err());
        let mut no_color = red_stroke(&[[0.0, 0.0, 0.0]], 0.2);
        no_color.color = None;
        assert!(stroke(&mut c, &no_color).is_err());
        assert!(stroke(&mut c, &red_stroke(&[[0.0, 0.0, 0.0]], 1e6)).is_err());
        assert!(stroke(&mut Clay::default(), &red_stroke(&[[0.0, 0.0, 0.0]], 0.2)).is_err(), "nothing to paint");
    }
}
