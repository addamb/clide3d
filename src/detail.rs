//! Detail maps: real surface texture from a small built-in library of CC0
//! scans (ambientCG, see `assets/textures/LICENSE.md`): cloth weave, knit,
//! leather grain, skin pores, wrinkles, brushed metal, wood, stone, scales.
//!
//! A material names one (`detail`) and says how big a tile is
//! (`detail_scale`, scene units; 0 = the texture's real size), how strong
//! its relief is (`detail_strength`), how much its color shows through the
//! base color (`detail_color`), and how it is turned (`detail_rotate`).
//! Clay has no UVs, so maps are projected along the three object axes and
//! blended by the normal (triplanar). The relief bends the shading normal,
//! the texture's roughness moves the material's, and its color (divided by
//! its own average, so the base color stays the agent's) and occlusion
//! modulate the color.
//!
//! The images are compiled in; each is decoded the first time a material
//! uses it and kept (with its mip levels) for every object after that.

use std::sync::OnceLock;

use glam::{Vec2, Vec3};

use crate::material::Material;

/// One library texture: (name, ambientCG id, what it is, real tile size in
/// meters, relief multiplier after normalizing).
pub struct Entry {
    pub name: &'static str,
    pub source: &'static str,
    pub about: &'static str,
    pub tile: f32,
    bump: f32,
    color: &'static [u8],
    normal: &'static [u8],
    rough: &'static [u8],
    height: &'static [u8],
    ao: Option<&'static [u8]>,
}

macro_rules! entry {
    ($name:literal, $source:literal, $about:literal, $tile:expr, $bump:expr, ao) => {
        entry!(@ $name, $source, $about, $tile, $bump, Some(include_bytes!(concat!("../assets/textures/", $name, "/ao.jpg"))))
    };
    ($name:literal, $source:literal, $about:literal, $tile:expr, $bump:expr) => {
        entry!(@ $name, $source, $about, $tile, $bump, None)
    };
    (@ $name:literal, $source:literal, $about:literal, $tile:expr, $bump:expr, $ao:expr) => {
        Entry {
            name: $name,
            source: $source,
            about: $about,
            tile: $tile,
            bump: $bump,
            color: include_bytes!(concat!("../assets/textures/", $name, "/color.jpg")),
            normal: include_bytes!(concat!("../assets/textures/", $name, "/normal.jpg")),
            rough: include_bytes!(concat!("../assets/textures/", $name, "/roughness.jpg")),
            height: include_bytes!(concat!("../assets/textures/", $name, "/height.jpg")),
            ao: $ao,
        }
    };
}

pub static LIBRARY: [Entry; 17] = [
    entry!("linen", "Fabric036", "plain homespun linen or cotton weave: shirts, tunics, dresses", 0.12, 1.0, ao),
    entry!("jersey", "Fabric019", "fine knit jersey: t-shirts, leggings, stockings", 0.06, 1.0),
    entry!("knit", "Fabric018", "knitted wool: sweaters, scarves, socks", 0.12, 1.0),
    entry!("felt", "Fabric034", "felt and brushed wool: hats, cloaks", 0.25, 1.0),
    entry!("velvet", "Fabric084", "velvet, microfiber pile", 0.2, 0.8, ao),
    entry!("denim", "Fabric077", "denim twill: jeans, work clothes", 0.12, 1.2),
    entry!("leather_smooth", "Leather038", "fine smooth leather: boots, gloves, belts", 0.3, 1.0),
    entry!("leather_rough", "Leather014", "old scuffed leather with scratches", 0.6, 1.0),
    entry!("leather_grain", "Leather025", "pebbled leather grain: bags, armor, saddles", 0.12, 1.3),
    entry!("suede", "Leather039", "suede and nubuck", 0.15, 1.3),
    entry!("skin_pores", "Leather030", "fine skin relief: pores in a network of tiny creases", 0.05, 0.7),
    entry!("wrinkles", "Paper003", "soft creases: fine wrinkles, crumpled cloth", 0.15, 0.4),
    entry!("metal_brushed", "Metal009", "brushed steel or aluminum", 0.3, 1.0),
    entry!("metal_scratched", "Metal038", "worn steel with scratches and smudges", 0.4, 1.0),
    entry!("wood", "Wood066", "wood grain: furniture, handles, staffs", 0.6, 1.0),
    entry!("stone", "Rock030", "rough stone: rocks, walls, statues", 1.0, 0.8, ao),
    entry!("scales", "Leather008", "reptile scales: crocodile, dragon, lizard skin", 0.35, 0.8, ao),
];

pub fn entry(name: &str) -> Option<&'static Entry> {
    LIBRARY.iter().find(|e| e.name == name)
}

pub fn names() -> Vec<&'static str> {
    LIBRARY.iter().map(|e| e.name).collect()
}

/// Typical normal-map slope at strength 1 (after normalizing each map).
const SLOPE: f32 = 0.35;

/// One decoded texture with its mip levels. Each texel packs sRGB color,
/// normal x and y (0..255, 128 = flat), roughness, occlusion, height.
pub struct Map {
    levels: Vec<(usize, Vec<[u8; 8]>)>,
    /// Average sRGB color, roughness and occlusion.
    mean_color: Vec3,
    mean_rough: f32,
    mean_ao: f32,
    /// Normal xy scale that gives a typical slope of `SLOPE`.
    gain: f32,
    /// Height mean and spread, so heights work as masks alike (`pattern`).
    height_mean: f32,
    height_spread: f32,
}

/// What a map adds at one point (already weighted by the material).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Texel {
    /// Color divided by the map's average (sRGB, ~1).
    pub tint: Vec3,
    /// Tangent-space slope (normal x, y) after normalizing.
    pub slope: Vec2,
    /// Roughness minus the map's average.
    pub rough: f32,
    /// Occlusion divided by the map's average.
    pub ao: f32,
    /// Height 0..1.
    pub height: f32,
}

impl Default for Texel {
    fn default() -> Self {
        Texel { tint: Vec3::ONE, slope: Vec2::ZERO, rough: 0.0, ao: 1.0, height: 0.5 }
    }
}

/// The decoded map called `name` (decoded once, then shared).
pub fn map(name: &str) -> Option<&'static Map> {
    static MAPS: [OnceLock<Option<Map>>; 17] = [const { OnceLock::new() }; 17];
    let i = LIBRARY.iter().position(|e| e.name == name)?;
    MAPS[i].get_or_init(|| Map::decode(&LIBRARY[i]).map_err(|e| eprintln!("detail map {name}: {e}")).ok()).as_ref()
}

fn gray(bytes: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_luma8();
    Ok(if img.width() as usize == size { img.into_raw() } else { image::imageops::resize(&img, size as u32, size as u32, image::imageops::FilterType::Triangle).into_raw() })
}

impl Map {
    fn decode(e: &Entry) -> Result<Map, String> {
        let color = image::load_from_memory_with_format(e.color, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_rgb8();
        let size = color.width() as usize;
        if size == 0 || color.height() as usize != size || !size.is_power_of_two() {
            return Err("textures must be square powers of two".into());
        }
        let normal = image::load_from_memory_with_format(e.normal, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_rgb8();
        let normal = if normal.width() as usize == size { normal } else { image::imageops::resize(&normal, size as u32, size as u32, image::imageops::FilterType::Triangle) };
        let rough = gray(e.rough, size)?;
        let height = gray(e.height, size)?;
        let ao = match e.ao { Some(b) => gray(b, size)?, None => vec![255; size * size] };
        let mut texels = Vec::with_capacity(size * size);
        for i in 0..size * size {
            let c = color.as_raw();
            let n = normal.as_raw();
            texels.push([c[i * 3], c[i * 3 + 1], c[i * 3 + 2], n[i * 3], n[i * 3 + 1], rough[i], ao[i], height[i]]);
        }
        let count = texels.len() as f32;
        let mean = |k: usize| texels.iter().map(|t| t[k] as f32).sum::<f32>() / count / 255.0;
        let mean_color = Vec3::new(mean(0), mean(1), mean(2)).max(Vec3::splat(0.02));
        // Normalize relief: maps are scanned at very different strengths.
        let (mx, my) = (mean(3), mean(4));
        let rms = (texels.iter().map(|t| (t[3] as f32 / 255.0 - mx).powi(2) + (t[4] as f32 / 255.0 - my).powi(2)).sum::<f32>() / count).sqrt() * 2.0;
        let gain = (SLOPE / rms.max(1e-3)).clamp(0.5, 6.0) * e.bump;
        let (mean_rough, mean_ao) = (mean(5), mean(6).max(0.05));
        let height_mean = mean(7);
        let height_spread = (texels.iter().map(|t| (t[7] as f32 / 255.0 - height_mean).powi(2)).sum::<f32>() / count).sqrt().max(0.01);
        let mut levels = vec![(size, texels)];
        while levels.last().unwrap().0 > 1 {
            let (side, ref data) = *levels.last().unwrap();
            let half = side / 2;
            let mut out = vec![[0u8; 8]; half * half];
            for y in 0..half {
                for x in 0..half {
                    let at = |dx: usize, dy: usize| &data[(2 * y + dy) * side + 2 * x + dx];
                    out[y * half + x] = std::array::from_fn(|k| ((at(0, 0)[k] as u32 + at(1, 0)[k] as u32 + at(0, 1)[k] as u32 + at(1, 1)[k] as u32 + 2) / 4) as u8);
                }
            }
            levels.push((half, out));
        }
        Ok(Map { levels, mean_color, mean_rough, mean_ao, gain, height_mean, height_spread })
    }

    pub fn size(&self) -> usize {
        self.levels[0].0
    }

    /// A height as a mask pattern: 0.5 at the map's mean, 0 and 1 two
    /// spreads below and above.
    pub fn pattern(&self, height: f32) -> f32 {
        (0.5 + (height - self.height_mean) / (4.0 * self.height_spread)).clamp(0.0, 1.0)
    }

    /// For the GPU tracer: the height mean and spread (see `pattern`).
    pub fn height_stats(&self) -> (f32, f32) {
        (self.height_mean, self.height_spread)
    }

    /// For the GPU tracer: every level's texels (largest first, 8 bytes
    /// each as in `levels`), the average sRGB color, roughness and
    /// occlusion, and the slope gain.
    pub fn gpu(&self) -> (Vec<&[[u8; 8]]>, Vec3, f32, f32, f32) {
        (self.levels.iter().map(|(_, d)| &d[..]).collect(), self.mean_color, self.mean_rough, self.mean_ao, self.gain)
    }

    /// Bilinear, wrapping read at `uv` (1 = one tile; v up the image) of
    /// mip level `lod` (0 = full size, fractional blends two levels).
    pub fn sample(&self, uv: Vec2, lod: f32) -> Texel {
        let lod = lod.clamp(0.0, (self.levels.len() - 1) as f32);
        let lo = lod.floor() as usize;
        let raw = if lod > lo as f32 + 1e-3 && lo + 1 < self.levels.len() {
            let f = lod - lo as f32;
            let (a, b) = (self.raw(lo, uv), self.raw(lo + 1, uv));
            std::array::from_fn(|k| a[k] * (1.0 - f) + b[k] * f)
        } else {
            self.raw(lo, uv)
        };
        Texel {
            tint: Vec3::new(raw[0], raw[1], raw[2]) / self.mean_color,
            slope: Vec2::new(raw[3] * 2.0 - 1.0, raw[4] * 2.0 - 1.0) * self.gain,
            rough: raw[5] - self.mean_rough,
            ao: raw[6] / self.mean_ao,
            height: raw[7],
        }
    }

    fn raw(&self, level: usize, uv: Vec2) -> [f32; 8] {
        let (side, data) = (&self.levels[level].0, &self.levels[level].1);
        let s = *side as f32;
        // Image rows run down: v = 1 is one tile up.
        let x = uv.x * s - 0.5;
        let y = -uv.y * s - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let wrap = |v: f32| (v as i64).rem_euclid(*side as i64) as usize;
        let (x0, x1, y0, y1) = (wrap(x0), wrap(x0 + 1.0), wrap(y0), wrap(y0 + 1.0));
        let t = |x: usize, y: usize| &data[y * side + x];
        std::array::from_fn(|k| {
            let top = t(x0, y0)[k] as f32 * (1.0 - fx) + t(x1, y0)[k] as f32 * fx;
            let bottom = t(x0, y1)[k] as f32 * (1.0 - fx) + t(x1, y1)[k] as f32 * fx;
            (top * (1.0 - fy) + bottom * fy) / 255.0
        })
    }

    /// The map's roughness, color and relief packed for the viewport: one
    /// `side` x `side` RGBA8 image: normal x, y, brightness ratio / 2, and
    /// roughness offset + 0.5.
    pub fn viewport_rgba(&self, side: usize) -> Vec<u8> {
        let (s, data) = self.levels.iter().find(|(s, _)| *s <= side).unwrap_or(self.levels.last().unwrap());
        let luma = self.mean_color.dot(Vec3::new(0.3, 0.59, 0.11)).max(0.02);
        let mut out = Vec::with_capacity(s * s * 4);
        for t in data {
            let slope = |v: u8| ((v as f32 / 255.0 * 2.0 - 1.0) * self.gain * 0.5 + 0.5).clamp(0.0, 1.0);
            let bright = (t[0] as f32 * 0.3 + t[1] as f32 * 0.59 + t[2] as f32 * 0.11) / 255.0 / luma * (t[6] as f32 / 255.0 / self.mean_ao).min(1.5);
            let rough = t[5] as f32 / 255.0 - self.mean_rough + 0.5;
            out.extend([slope(t[3]), slope(t[4]), (bright * 0.5).clamp(0.0, 1.0), rough.clamp(0.0, 1.0)].map(|v| (v * 255.0).round() as u8));
        }
        out
    }
}

/// `m`'s detail without decoding it: (library index, tiles per scene unit,
/// cos and sin of the rotation).
pub fn layout(m: &Material) -> Option<(usize, f32, Vec2)> {
    place(m.detail_name()?, m.detail_scale, m.detail_rotate)
}

/// The map called `name` (empty or "none": off) at tile size `scale` (0 =
/// its real size), turned `rotate` degrees: as `layout`.
pub fn place(name: &str, scale: f32, rotate: f32) -> Option<(usize, f32, Vec2)> {
    let layer = LIBRARY.iter().position(|e| e.name == name)?;
    let tile = if scale > 0.0 { scale } else { LIBRARY[layer].tile };
    let a = rotate.to_radians();
    Some((layer, 1.0 / tile, Vec2::new(a.cos(), a.sin())))
}

/// `m`'s detail packed for the viewport shader: (library index + 1 (0 =
/// none), tiles per unit, strength, color), (cos, sin, 0, 0).
/// Layered materials without their own show their first layer's.
pub fn shader_params(m: &Material) -> [[f32; 4]; 2] {
    let own = layout(m).map(|l| (l, m.detail_strength, m.detail_color));
    let layer = || m.layers.first().and_then(|l| Some((place(l.detail.as_deref()?, l.detail_scale, l.detail_rotate)?, l.detail_strength, l.detail_color)));
    match own.or_else(layer) {
        Some(((layer, freq, turn), strength, color)) => [[layer as f32 + 1.0, freq, strength, color], [turn.x, turn.y, 0.0, 0.0]],
        None => [[0.0; 4], [1.0, 0.0, 0.0, 0.0]],
    }
}

/// A material's detail settings resolved for lookups.
#[derive(Clone, Copy)]
pub struct Params {
    pub map: &'static Map,
    /// Tiles per scene unit.
    pub freq: f32,
    pub strength: f32,
    pub color: f32,
    /// cos, sin of the rotation.
    pub turn: Vec2,
    /// Texels per scene unit at mip 0.
    pub density: f32,
}

impl Params {
    pub fn of(m: &Material) -> Option<Params> {
        let (layer, freq, turn) = layout(m)?;
        let map = map(LIBRARY[layer].name)?;
        Some(Params { map, freq, strength: m.detail_strength, color: m.detail_color, turn, density: map.size() as f32 * freq })
    }

    /// The map called `name` with these settings (as a material's).
    pub fn named(name: &str, scale: f32, strength: f32, color: f32, rotate: f32) -> Option<Params> {
        let (layer, freq, turn) = place(name, scale, rotate)?;
        let map = map(LIBRARY[layer].name)?;
        Some(Params { map, freq, strength, color, turn, density: map.size() as f32 * freq })
    }

    /// Mip level for a point seen over `footprint` scene units.
    pub fn lod(&self, footprint: f32) -> f32 {
        (footprint * self.density).max(1.0).log2()
    }

    /// The triplanar lookup at object-space point `p` with normal `n`:
    /// returns the blended texel and the slope as an object-space vector
    /// (to add to the normal).
    pub fn triplanar(&self, p: Vec3, n: Vec3, lod: f32) -> (Texel, Vec3) {
        let w = n.abs().powf(4.0);
        let w = w / w.element_sum().max(1e-12);
        let mut out = Texel { tint: Vec3::ZERO, slope: Vec2::ZERO, rough: 0.0, ao: 0.0, height: 0.0 };
        let mut bend = Vec3::ZERO;
        let (c, s) = (self.turn.x, self.turn.y);
        for axis in 0..3 {
            if w[axis] < 0.003 {
                continue;
            }
            // u along a side axis (flipped on the back so the image is not
            // mirrored), v up (or toward -Z on top and bottom faces).
            let sign = if n[axis] < 0.0 { -1.0 } else { 1.0 };
            let (tu, tv) = match axis {
                0 => (Vec3::Z * -sign, Vec3::Y),
                1 => (Vec3::X, Vec3::Z * -sign),
                _ => (Vec3::X * sign, Vec3::Y),
            };
            let uv = Vec2::new(p.dot(tu), p.dot(tv)) * self.freq;
            let uv = Vec2::new(c * uv.x - s * uv.y, s * uv.x + c * uv.y);
            let t = self.map.sample(uv, lod);
            let (ru, rv) = (tu * c - tv * s, tu * s + tv * c);
            bend += (ru * t.slope.x + rv * t.slope.y) * w[axis];
            out.tint += t.tint * w[axis];
            out.slope += t.slope * w[axis];
            out.rough += t.rough * w[axis];
            out.ao += t.ao * w[axis];
            out.height += t.height * w[axis];
        }
        (out, bend)
    }

    /// Shading at object-space point `p` (normal `n`, sRGB `color`,
    /// roughness `rough`) with this detail, seen over `footprint` scene
    /// units (0 = sharpest).
    pub fn apply(&self, p: Vec3, n: Vec3, color: Vec3, rough: f32, footprint: f32) -> (Vec3, Vec3, f32) {
        let (t, bend) = self.triplanar(p, n, self.lod(footprint));
        let bend = bend - n * n.dot(bend);
        let normal = (n + bend * self.strength).normalize_or(n);
        let tint = Vec3::ONE.lerp(t.tint, self.color);
        let ao = 1.0 + (t.ao.min(1.5) - 1.0) * (0.5 * self.strength).min(1.0);
        let rough = (rough + t.rough * 0.6 * self.strength.min(1.5)).clamp(0.03, 1.0);
        (normal, (color * tint * ao).clamp(Vec3::ZERO, Vec3::ONE), rough)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_map_loads_tiles_and_has_relief() {
        for e in &LIBRARY {
            let m = map(e.name).unwrap_or_else(|| panic!("{} did not load", e.name));
            assert!(m.size() >= 256 && m.size().is_power_of_two(), "{}", e.name);
            assert_eq!(m.levels.last().unwrap().0, 1);
            // Wrapping: a whole tile over is the same texel.
            let uv = Vec2::new(0.137, 0.611);
            let (a, b) = (m.sample(uv, 0.0), m.sample(uv + Vec2::new(3.0, -2.0), 0.0));
            assert!((a.tint - b.tint).abs().max_element() < 1e-4 && (a.slope - b.slope).abs().max_element() < 1e-4, "{}", e.name);
            // Averages are ~1 tint, ~0 slope; the full-size map has relief.
            let mut slope = 0.0;
            let mut tint = Vec3::ZERO;
            for i in 0..400 {
                let t = m.sample(Vec2::new(i as f32 * 0.0731, i as f32 * 0.0419), 0.0);
                slope += t.slope.length() / 400.0;
                tint += t.tint / 400.0;
            }
            assert!(slope > 0.05, "{} is flat: {slope}", e.name);
            assert!((tint - Vec3::ONE).abs().max_element() < 0.25, "{} tint {tint}", e.name);
            // The 1x1 mip is the average: no slope, unit tint.
            let flat = m.sample(uv, 99.0);
            assert!(flat.slope.length() < 0.25 && (flat.tint - Vec3::ONE).abs().max_element() < 0.1, "{} {flat:?}", e.name);
        }
        assert!(entry("nope").is_none() && map("nope").is_none());
    }

    #[test]
    fn triplanar_follows_the_object_axes_and_bends_within_the_surface() {
        let m = Material { detail: Some("linen".into()), detail_strength: 1.0, detail_color: 1.0, ..Material::default() };
        let d = Params::of(&m).unwrap();
        let p = Vec3::new(0.0123, 0.4567, 0.0321);
        // A face looking down +Z reads the XY plane: moving along Z does
        // not change it, moving along X does.
        let (a, bend) = d.triplanar(p, Vec3::Z, 0.0);
        assert_eq!(a, d.triplanar(p + Vec3::Z * 0.3, Vec3::Z, 0.0).0);
        assert_ne!(a, d.triplanar(p + Vec3::X * 0.003, Vec3::Z, 0.0).0);
        assert!(bend.z.abs() < 1e-6 && bend.length() > 0.0);
        // Same for +X faces and the YZ plane.
        let (b, bend) = d.triplanar(p, Vec3::X, 0.0);
        assert_eq!(b, d.triplanar(p + Vec3::X * 0.5, Vec3::X, 0.0).0);
        assert!(bend.x.abs() < 1e-6);
        // Applied: the normal stays near the surface normal, but varies.
        let mut spread = 0.0f32;
        for i in 0..64 {
            let (n, c, r) = d.apply(p + Vec3::X * i as f32 * 0.0007, Vec3::Z, Vec3::splat(0.5), 0.6, 0.0);
            assert!((n.length() - 1.0).abs() < 1e-5 && n.z > 0.6);
            assert!(c.is_finite() && (0.03..=1.0).contains(&r));
            spread = spread.max(1.0 - n.z);
        }
        assert!(spread > 0.01, "no relief: {spread}");
        // Blurry from far away: a big footprint flattens it.
        let (n, _, _) = d.apply(p, Vec3::Z, Vec3::splat(0.5), 0.6, 10.0);
        assert!(n.z > 0.97, "{n}");
        // No detail, or off.
        assert!(Params::of(&Material::default()).is_none());
        assert!(Params::of(&Material { detail: Some("none".into()), ..Material::preset("cloth").unwrap() }).is_none());
        assert!(Params::of(&Material::preset("cloth").unwrap()).is_some());
    }

    #[test]
    fn viewport_images_are_packed_at_the_asked_size() {
        let m = map("scales").unwrap();
        let rgba = m.viewport_rgba(256);
        assert_eq!(rgba.len(), 256 * 256 * 4);
        let mean = |k: usize| rgba.chunks_exact(4).map(|p| p[k] as f32).sum::<f32>() / (256.0 * 256.0);
        assert!((mean(0) - 128.0).abs() < 20.0 && (mean(2) - 128.0).abs() < 30.0, "{} {}", mean(0), mean(2));
    }
}
