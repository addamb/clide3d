//! Alphas (docs/SCULPT_PLAN.md 3.7): 16-bit grayscale images that shape a
//! dab. A stroke names one (`StrokeSpec::alpha`, an `AlphaRef`); the
//! resolver binds it by content hash (`AlphaParams`), and the engine
//! samples it bilinearly in the dab's tangent frame (`Stamp`):
//! - `use: height` (standard): the dab raises the surface by `depth` x
//!   (alpha - `mid`), so white stands out and, with mid 0.5, black sinks;
//! - `use: mask` (any dab brush): the alpha scales the dab's weight.
//!
//! Modes: `stamp` keeps the image upright on the object (turned by
//! `rotate`), `drag` turns it with the stroke's direction, and `tile`
//! repeats it across the surface, anchored to the object rather than the
//! dab.
//!
//! The scene keeps a library by name (`Scene.alphas`); each clay keeps the
//! images its records use (`AlphaStore`, in its sculpt stack) by hash, so
//! its strokes replay without the library, and the name each was last
//! bound to: re-importing a name with other pixels rebinds it, and the
//! records made with the old image report `stale_context` until a rebase.
//!
//! Images are read from PNG (8 or 16 bit) or JPEG, at most `MAX_SIDE`
//! pixels a side; sizes are checked from the header before anything is
//! decoded. Files keep them as 16-bit grayscale PNG in base64.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use glam::Vec3;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_128;

use super::vdm::Vdm;
use crate::ledger::Hash128;

/// Largest alpha side, in pixels.
pub const MAX_SIDE: u32 = 1024;
/// Most alphas in a scene's library, and in one clay's store.
pub const MAX_ALPHAS: usize = 64;
/// Largest image file `alpha import` reads.
pub const MAX_FILE_BYTES: u64 = 32 << 20;
/// Longest alpha name.
pub const MAX_NAME: usize = 64;

/// A 16-bit grayscale image, shared (cloning copies a pointer). Two are
/// equal when their pixels are (by content hash).
#[derive(Clone)]
pub struct Alpha(Arc<Image>);

struct Image {
    w: u32,
    h: u32,
    data: Vec<u16>,
    hash: Hash128,
    /// A VDM stamp (`brush::vdm`): the image is then its height from above.
    vdm: Option<Vdm>,
}

impl PartialEq for Alpha {
    fn eq(&self, other: &Self) -> bool {
        self.0.hash == other.0.hash
    }
}

impl std::fmt::Debug for Alpha {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}({}x{}, {})", if self.0.vdm.is_some() { "Vdm" } else { "Alpha" }, self.0.w, self.0.h, self.0.hash)
    }
}

fn check(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.into()) }
}

impl Alpha {
    /// An alpha of `w` x `h` pixels, rows top to bottom.
    pub fn new(w: u32, h: u32, data: Vec<u16>) -> Result<Alpha, String> {
        check((1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h), &format!("an alpha is 1 to {MAX_SIDE} pixels a side"))?;
        check(data.len() == (w * h) as usize, "alpha pixels do not match its size")?;
        let mut bytes = Vec::with_capacity(13 + data.len() * 2);
        bytes.extend(b"alpha");
        bytes.extend(w.to_le_bytes());
        bytes.extend(h.to_le_bytes());
        data.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
        let hash = Hash128(xxh3_128(&bytes));
        Ok(Alpha(Arc::new(Image { w, h, data, hash, vdm: None })))
    }

    /// A VDM stamp (`alpha create_vdm`), shown as its height from above;
    /// hashed by its image and its volume.
    pub fn from_vdm(vdm: Vdm) -> Result<Alpha, String> {
        vdm.validate()?;
        let (w, h, data) = vdm.height_image();
        check((1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h), "a VDM's image is too large")?;
        let mut bytes = Vec::with_capacity(13 + data.len() * 2);
        bytes.extend(b"alpha");
        bytes.extend(w.to_le_bytes());
        bytes.extend(h.to_le_bytes());
        data.iter().for_each(|v| bytes.extend(v.to_le_bytes()));
        vdm.hash_bytes(&mut bytes);
        let hash = Hash128(xxh3_128(&bytes));
        Ok(Alpha(Arc::new(Image { w, h, data, hash, vdm: Some(vdm) })))
    }

    /// The VDM stamp, when this is one.
    pub fn vdm(&self) -> Option<&Vdm> {
        self.0.vdm.as_ref()
    }

    /// Reads a PNG (8 or 16 bit, gray or color: color becomes its
    /// luminance), JPEG or OpenEXR (`decode_exr`). The size is read from the header first; images
    /// over `MAX_SIDE` are refused before they are decoded.
    pub fn decode(bytes: &[u8]) -> Result<Alpha, String> {
        if bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
            return Alpha::decode_exr(bytes);
        }
        let reader = || image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string());
        let (w, h) = reader()?.into_dimensions().map_err(|e| format!("not a PNG or JPEG image: {e}"))?;
        check((1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h), &format!("the image is {w}x{h}; alphas are at most {MAX_SIDE} pixels a side"))?;
        let mut r = reader()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(64 << 20);
        r.limits(limits);
        let img = r.decode().map_err(|e| format!("could not read the image: {e}"))?.into_luma16();
        check(img.width() == w && img.height() == h, "the image's size changed while it was read")?;
        Alpha::new(w, h, img.into_raw())
    }

    /// Reads an OpenEXR (Q6, sculpt Stage 8b): its Y channel (else R,
    /// else the first), values 0..1 (clamped; not finite = 0) to 16 bits.
    /// Every layer's size is read from the header first; files with a
    /// layer over `MAX_SIDE` are refused before any pixels are read.
    fn decode_exr(bytes: &[u8]) -> Result<Alpha, String> {
        use exr::prelude::*;
        let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false).map_err(|e| format!("not an EXR image: {e}"))?;
        check(!meta.headers.is_empty() && meta.headers.len() <= 16, "an EXR alpha has 1 to 16 layers")?;
        for h in meta.headers.iter() {
            let (w, ht) = (h.layer_size.0, h.layer_size.1);
            check((1..=MAX_SIDE as usize).contains(&w) && (1..=MAX_SIDE as usize).contains(&ht), &format!("the image is {w}x{ht}; alphas are at most {MAX_SIDE} pixels a side"))?;
            check(!h.deep && h.channels.list.len() <= 16, "deep EXR images and more than 16 channels are not read")?;
        }
        let image = read().no_deep_data().largest_resolution_level().all_channels().first_valid_layer().all_attributes().from_buffered(Cursor::new(bytes)).map_err(|e| format!("could not read the EXR image: {e}"))?;
        let layer = &image.layer_data;
        let (w, h) = (layer.size.0, layer.size.1);
        check((1..=MAX_SIDE as usize).contains(&w) && (1..=MAX_SIDE as usize).contains(&h), "the image's size changed while it was read")?;
        let list = &layer.channel_data.list;
        let pick = ["Y", "R"].iter().find_map(|n| list.iter().find(|c| c.name.eq(*n))).or(list.first()).ok_or("the EXR image has no channels")?;
        let value = |i: usize| -> f32 {
            match &pick.sample_data {
                FlatSamples::F16(v) => v[i].to_f32(),
                FlatSamples::F32(v) => v[i],
                FlatSamples::U32(v) => v[i] as f32 / u32::MAX as f32,
            }
        };
        check(pick.sample_data.len() == w * h, "the EXR channel does not match its size")?;
        let data = (0..w * h).map(|i| { let v = value(i); if v.is_finite() { (v.clamp(0.0, 1.0) * 65535.0).round() as u16 } else { 0 } }).collect();
        Alpha::new(w as u32, h as u32, data)
    }

    /// The pixels as a 16-bit grayscale PNG.
    pub fn png(&self) -> Vec<u8> {
        let img = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(self.0.w, self.0.h, self.0.data.clone()).expect("sized at construction");
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).expect("PNG into memory");
        out
    }

    /// An 8-bit grayscale PNG at most `side` pixels across (for previews).
    pub fn preview_png(&self, side: u32) -> Vec<u8> {
        let img = image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_raw(self.0.w, self.0.h, self.0.data.clone()).expect("sized at construction");
        let gray = image::DynamicImage::ImageLuma16(img).into_luma8();
        let k = (side as f32 / self.0.w.max(self.0.h) as f32).min(1.0);
        let (w, h) = (((self.0.w as f32 * k).round() as u32).max(1), ((self.0.h as f32 * k).round() as u32).max(1));
        let small = if (w, h) == (self.0.w, self.0.h) { gray } else { image::imageops::resize(&gray, w, h, image::imageops::FilterType::Triangle) };
        let mut out = Vec::new();
        small.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).expect("PNG into memory");
        out
    }

    pub fn hash(&self) -> Hash128 {
        self.0.hash
    }

    pub fn size(&self) -> (u32, u32) {
        (self.0.w, self.0.h)
    }

    /// The value (0..1) at texture coordinates `s` (left to right) and `t`
    /// (top to bottom), bilinear between pixel centers. Off the image:
    /// None, or with `tile` the image repeats.
    pub fn sample(&self, s: f32, t: f32, tile: bool) -> Option<f32> {
        let (w, h) = (self.0.w as i64, self.0.h as i64);
        if !tile && !((0.0..=1.0).contains(&s) && (0.0..=1.0).contains(&t)) {
            return None;
        }
        let (x, y) = (s * w as f32 - 0.5, t * h as f32 - 0.5);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let (fx, fy) = (x.floor(), y.floor());
        let (ax, ay) = (x - fx, y - fy);
        let at = |i: i64, j: i64| {
            let (i, j) = if tile { (i.rem_euclid(w), j.rem_euclid(h)) } else { (i.clamp(0, w - 1), j.clamp(0, h - 1)) };
            self.0.data[(i + j * w) as usize] as f32 / 65535.0
        };
        let (i, j) = (fx as i64, fy as i64);
        let top = at(i, j) + (at(i + 1, j) - at(i, j)) * ax;
        let bottom = at(i, j + 1) + (at(i + 1, j + 1) - at(i, j + 1)) * ax;
        Some(top + (bottom - top) * ay)
    }
}

/// What `Alpha` reads from and writes to a file.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlphaFile {
    w: u32,
    h: u32,
    /// A 16-bit grayscale PNG, base64.
    png: String,
    /// A VDM stamp's volume (the PNG is then its height image, made again
    /// from the volume when read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vdm: Option<Vdm>,
}

impl Serialize for Alpha {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AlphaFile { w: self.0.w, h: self.0.h, png: base64::engine::general_purpose::STANDARD.encode(self.png()), vdm: self.0.vdm.clone() }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Alpha {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let f = AlphaFile::deserialize(d)?;
        if !((1..=MAX_SIDE).contains(&f.w) && (1..=MAX_SIDE).contains(&f.h)) {
            return Err(D::Error::custom(format!("an alpha is 1 to {MAX_SIDE} pixels a side")));
        }
        // A PNG of w x h 16-bit pixels is about that size deflated; far
        // more text than that is not an alpha of this size.
        let raw = f.w as usize * f.h as usize * 2;
        if f.png.len() > (raw * 2 + 65_536) * 4 / 3 {
            return Err(D::Error::custom("an alpha's PNG is far larger than its size"));
        }
        // Decoded pixels count toward the read's inflate budget, like
        // packed grids.
        crate::clay::charge_inflate(raw as u64).map_err(D::Error::custom)?;
        if let Some(v) = f.vdm {
            // The image is the volume's: made again rather than trusted.
            let a = Alpha::from_vdm(v).map_err(D::Error::custom)?;
            if a.size() != (f.w, f.h) {
                return Err(D::Error::custom("a VDM's image does not match its size"));
            }
            return Ok(a);
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&f.png).map_err(D::Error::custom)?;
        let a = Alpha::decode(&bytes).map_err(D::Error::custom)?;
        if a.size() != (f.w, f.h) {
            return Err(D::Error::custom("an alpha's PNG does not match its size"));
        }
        Ok(a)
    }
}

/// A map read with at most `MAX` entries: those past it are skipped
/// unread (not decoded), so an over-full file still opens with the first
/// `MAX`.
pub fn capped_map<'de, D, K, V, const MAX: usize>(d: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct Visit<K, V, const MAX: usize>(std::marker::PhantomData<(K, V)>);
    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de> for Visit<K, V, MAX> {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a map of at most {MAX} entries")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(k) = map.next_key::<K>()? {
                if out.len() >= MAX {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    continue;
                }
                let v = map.next_value::<V>()?;
                out.insert(k, v);
            }
            Ok(out)
        }
    }
    d.deserialize_map(Visit::<K, V, MAX>(std::marker::PhantomData))
}

fn alpha_map<'de, D: serde::Deserializer<'de>, K: Deserialize<'de> + Ord>(d: D) -> Result<BTreeMap<K, Alpha>, D::Error> {
    crate::clay::in_inflate_budget_with(d, capped_map::<D, K, Alpha, MAX_ALPHAS>)
}

fn name_map<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Hash128>, D::Error> {
    capped_map::<D, String, Hash128, MAX_ALPHAS>(d)
}

/// The scene's alpha library (`Scene.alphas`), read within the inflate
/// budget and at most `MAX_ALPHAS` long.
pub fn library<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Alpha>, D::Error> {
    let lib = alpha_map::<D, String>(d)?;
    if lib.keys().any(|n| !name_ok(n)) {
        return Err(serde::de::Error::custom(format!("alpha names are 1 to {MAX_NAME} characters")));
    }
    Ok(lib)
}

/// Whether `name` can name an alpha.
pub fn name_ok(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= MAX_NAME
}

/// The images a clay's records use, by hash, and the name each alpha was
/// last bound to (see the module doc).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AlphaStore {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "alpha_map")]
    pub images: BTreeMap<Hash128, Alpha>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "name_map")]
    pub names: BTreeMap<String, Hash128>,
}

impl AlphaStore {
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.names.is_empty()
    }

    /// Within its limits, every image under its own hash and every name
    /// bound to an image it has.
    pub fn is_sane(&self) -> bool {
        self.images.len() <= MAX_ALPHAS
            && self.names.len() <= MAX_ALPHAS
            && self.images.iter().all(|(h, a)| *h == a.hash())
            && self.names.iter().all(|(n, h)| name_ok(n) && self.images.contains_key(h))
    }

    /// Drops what no record needs: names no record uses, and images no
    /// record uses or kept name binds. `names_used` and `hashes_used` are
    /// what the clay's records name.
    pub fn prune(&mut self, names_used: &std::collections::HashSet<String>, hashes_used: &std::collections::HashSet<Hash128>) {
        self.names.retain(|n, _| names_used.contains(n));
        let bound: std::collections::HashSet<Hash128> = self.names.values().copied().collect();
        self.images.retain(|h, _| hashes_used.contains(h) || bound.contains(h));
    }

    /// Keeps `alpha` (bound to `name`, if any) for the records.
    pub fn keep(&mut self, name: Option<&str>, alpha: &Alpha) -> Result<(), String> {
        if !self.images.contains_key(&alpha.hash()) && self.images.len() >= MAX_ALPHAS {
            return Err(format!("a clay keeps at most {MAX_ALPHAS} alphas for its strokes; squash or delete strokes that use old ones"));
        }
        self.images.insert(alpha.hash(), alpha.clone());
        if let Some(n) = name {
            self.names.insert(n.to_string(), alpha.hash());
        }
        Ok(())
    }
}

/// How the alpha sits on the dab.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Upright on the object (its X axis across), turned by `rotate`.
    #[default]
    Stamp,
    /// Turned with the stroke's direction.
    Drag,
    /// Repeated across the surface, anchored to the object.
    Tile,
}

/// What the alpha's values do.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Use {
    /// The dab's height profile (standard): depth x (alpha - mid).
    #[default]
    Height,
    /// A weight on the dab (any dab brush).
    Mask,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// An alpha as a stroke names it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct AlphaRef {
    /// A name in the scene's library (`alpha import`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Or the image's content hash (what records keep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<Hash128>,
    /// Degrees, turning the image on the surface.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: f32,
    /// The image spans `scale` x the dab's diameter (default 1).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: f32,
    /// The gray level that stays put (height use), 0..1 (default 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mid: f32,
    /// Height of white over `mid` (scene units; default the brush's own
    /// height, strength x radius / 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: Mode,
    /// Default: height on standard, mask on the other brushes.
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub usage: Option<Use>,
}

impl AlphaRef {
    pub fn validate(&self) -> Result<(), String> {
        check(self.name.is_some() || self.hash.is_some(), "an alpha needs name (from alpha import) or hash")?;
        check(self.name.as_deref().is_none_or(name_ok), &format!("alpha names are 1 to {MAX_NAME} characters"))?;
        check(self.rotate.is_finite(), "alpha rotate must be finite (degrees)")?;
        check((0.05..=20.0).contains(&self.scale), "alpha scale must be 0.05 to 20")?;
        check((0.0..=1.0).contains(&self.mid), "alpha mid must be 0 to 1")?;
        check(self.depth.is_none_or(|d| d.is_finite() && d.abs() <= 10.0), "alpha depth must be finite, at most 10 scene units")
    }
}

/// An alpha bound to its image, in object space: what a record keeps.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct AlphaParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub hash: Hash128,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotate: f32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mid: f32,
    /// Object units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mode: Mode,
    #[serde(default, rename = "use", skip_serializing_if = "is_default")]
    pub usage: Use,
}

/// The dab's tangent frame: `u` across and `v` up the image, both
/// perpendicular to `n`. `tangent` gives u's direction (object X without
/// one); `flip` mirrors v, which a mirrored copy of a dab sets, so its
/// stamp is the mirror image.
pub fn frame(n: Vec3, tangent: Option<Vec3>, flip: bool, rotate_deg: f32) -> (Vec3, Vec3) {
    let t = tangent.unwrap_or(Vec3::X);
    let mut u = t - n * n.dot(t);
    if u.length_squared() < 1e-8 {
        u = n.any_orthonormal_vector();
    }
    let u = u.normalize();
    let v = n.cross(u) * if flip { -1.0 } else { 1.0 };
    if rotate_deg == 0.0 {
        return (u, v);
    }
    let (s, c) = rotate_deg.to_radians().sin_cos();
    (u * c + v * s, v * c - u * s)
}

/// An alpha placed on one dab.
pub struct Stamp<'a> {
    pub alpha: &'a Alpha,
    pub params: &'a AlphaParams,
    pub center: Vec3,
    pub u: Vec3,
    pub v: Vec3,
    /// Half the image's side, object units.
    pub half: f32,
}

impl Stamp<'_> {
    /// The alpha under `p` (None off a stamp or drag image).
    pub fn at(&self, p: Vec3) -> Option<f32> {
        let tile = self.params.mode == Mode::Tile;
        let d = if tile { p } else { p - self.center };
        let side = 2.0 * self.half;
        self.alpha.sample(d.dot(self.u) / side + 0.5, 0.5 - d.dot(self.v) / side, tile)
    }

    /// The factor a dab's height takes at `p` (height use): alpha - mid,
    /// 0 off the image.
    pub fn height(&self, p: Vec3) -> f32 {
        self.at(p).map_or(0.0, |a| a - self.params.mid)
    }

    /// The factor a dab's weight takes at `p` (mask use): 0 off the image.
    pub fn mask(&self, p: Vec3) -> f32 {
        self.at(p).unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(n: u32, cell: u32) -> Alpha {
        Alpha::new(n, n, (0..n * n).map(|i| if (i % n / cell + i / n / cell) % 2 == 0 { 65535 } else { 0 }).collect()).unwrap()
    }

    #[test]
    fn png_round_trips_and_hashes_by_content() {
        let a = checker(64, 8);
        let png = a.png();
        let b = Alpha::decode(&png).unwrap();
        assert_eq!(a.hash(), b.hash());
        // 8-bit images widen exactly (v x 257).
        let img = image::GrayImage::from_fn(5, 3, |x, y| image::Luma([(x * 40 + y) as u8]));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let c = Alpha::decode(&bytes).unwrap();
        assert_eq!(c.size(), (5, 3));
        // Pixel (1, 0) is 40: 40 x 257 in 16 bits.
        assert_eq!(c.sample(0.3, 1.0 / 6.0, false), Some(40.0 * 257.0 / 65535.0));
        let text = serde_json::to_string(&a).unwrap();
        let back: Alpha = serde_json::from_str(&text).unwrap();
        assert_eq!(back.hash(), a.hash());
        assert_ne!(checker(64, 4).hash(), a.hash());
    }

    #[test]
    fn oversized_images_are_refused_from_the_header() {
        let img = image::GrayImage::new(MAX_SIDE + 1, 2);
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        assert!(Alpha::decode(&bytes).unwrap_err().contains("at most"));
        assert!(Alpha::decode(b"not an image").is_err());
        let big = serde_json::json!({"w": 2000, "h": 2, "png": ""});
        assert!(serde_json::from_value::<Alpha>(big).is_err());
    }

    /// EXR alphas (Stage 8b): float Y read to 16 bits, clamped; an
    /// oversized one refused from its header.
    #[test]
    fn exr_alphas_read_their_y_channel() {
        let dir = std::env::temp_dir().join(format!("clide-exr-alpha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.exr");
        crate::outputs::write_exr(&path, 4, 2, vec![("Y", vec![0.0, 0.25, 0.5, 1.0, 2.0, -1.0, f32::NAN, 0.75]), ("R", vec![1.0; 8])]).unwrap();
        let a = Alpha::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(a.size(), (4, 2));
        assert_eq!(a.0.data, vec![0, 16384, 32768, 65535, 65535, 0, 0, 49151]);
        crate::outputs::write_exr(&path, MAX_SIDE as usize + 1, 1, vec![("Y", vec![0.5; MAX_SIDE as usize + 1])]).unwrap();
        assert!(Alpha::decode(&std::fs::read(&path).unwrap()).unwrap_err().contains("at most"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sampling_is_bilinear_and_tiles() {
        let a = Alpha::new(2, 1, vec![0, 65535]).unwrap();
        assert_eq!(a.sample(0.25, 0.5, false), Some(0.0));
        assert_eq!(a.sample(0.75, 0.5, false), Some(1.0));
        assert!((a.sample(0.5, 0.5, false).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(a.sample(1.5, 0.5, false), None);
        assert_eq!(a.sample(1.25, 0.5, true), Some(0.0));
    }

    #[test]
    fn mirrored_frames_mirror_the_image() {
        let n = Vec3::new(0.3, 0.9, 0.1).normalize();
        let t = Vec3::new(1.0, 0.2, -0.4);
        let (u, v) = frame(n, Some(t), false, 30.0);
        let m = |p: Vec3| Vec3::new(-p.x, p.y, p.z);
        let (mu, mv) = frame(m(n), Some(m(t)), true, 30.0);
        assert!((mu - m(u)).length() < 1e-5 && (mv - m(v)).length() < 1e-5, "{u} {v} / {mu} {mv}");
        assert!(serde_json::from_value::<AlphaRef>(serde_json::json!({"name": "x", "size": 2})).is_err());
    }
}
