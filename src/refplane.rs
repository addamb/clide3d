//! Reference image planes (Quality Stage 5a): one view of a turnaround
//! sheet, standing in the scene behind the model as a textured plane at
//! the sheet's shared scale (`set_reference {sheet}`).
//!
//! A plane is a `Shape::Plane` object with `Object::reference` set. It
//! carries its view's image (the sheet cut to the view, at most
//! `MAX_SIDE` a side), the shared scale and the ground line, so saving
//! and opening a scene brings the per-view reference slots back
//! (`app/sheet.rs`). Planes are references, not geometry: exports, print,
//! film (unless `show_references`), silhouettes, close-ups and picking
//! leave them out (`Object::is_reference`).

use std::sync::Arc;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::material::Material;
use crate::mesh::{Mesh, Textures, Vertex};

/// The off-screen renders' and the viewport's background (sRGB): a plane
/// fades toward it by its opacity.
pub const BACKGROUND: [u8; 3] = [96, 100, 106];

/// The longest side of a plane's image (as a single reference image).
pub const MAX_SIDE: u32 = 1024;

/// An sRGB RGBA image, shared (cloning copies a pointer). Equal when the
/// sizes and pixels are (by content hash). Saved as a PNG in base64.
#[derive(Clone)]
pub struct Image(Arc<Data>);

struct Data {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    hash: u128,
}

impl Image {
    /// `rgba` (w × h × 4 bytes), or None when the sizes do not fit.
    pub fn new(w: u32, h: u32, rgba: Vec<u8>) -> Option<Image> {
        ((1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h) && rgba.len() == w as usize * h as usize * 4).then(|| {
            let hash = xxhash_rust::xxh3::xxh3_128(&rgba) ^ ((w as u128) << 64 | h as u128);
            Image(Arc::new(Data { w, h, rgba, hash }))
        })
    }

    pub fn size(&self) -> [usize; 2] {
        [self.0.w as usize, self.0.h as usize]
    }

    pub fn rgba(&self) -> &[u8] {
        &self.0.rgba
    }

    pub fn color_image(&self) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied(self.size(), &self.0.rgba)
    }

    fn png(&self) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out).write_image(&self.0.rgba, self.0.w, self.0.h, image::ExtendedColorType::Rgba8).expect("an in-memory PNG encodes");
        out
    }
}

impl PartialEq for Image {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.0, &o.0) || (self.0.w == o.0.w && self.0.h == o.0.h && self.0.hash == o.0.hash)
    }
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({} x {}, {:032x})", self.0.w, self.0.h, self.0.hash)
    }
}

#[derive(Serialize, Deserialize)]
struct ImageFile {
    w: u32,
    h: u32,
    png: String,
}

impl Serialize for Image {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine;
        ImageFile { w: self.0.w, h: self.0.h, png: base64::engine::general_purpose::STANDARD.encode(self.png()) }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine;
        use serde::de::Error;
        let f = ImageFile::deserialize(d)?;
        // Sizes first, before anything is decoded or allocated.
        if !((1..=MAX_SIDE).contains(&f.w) && (1..=MAX_SIDE).contains(&f.h)) {
            return Err(D::Error::custom(format!("a reference plane's image is 1 to {MAX_SIDE} pixels a side")));
        }
        let raw = f.w as usize * f.h as usize * 4;
        if f.png.len() > (raw + raw / 2 + 65_536) * 4 / 3 {
            return Err(D::Error::custom("a reference plane's PNG is far larger than its size"));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(f.png.as_bytes()).map_err(D::Error::custom)?;
        let reader = || image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png);
        let (w, h) = reader().into_dimensions().map_err(D::Error::custom)?;
        if (w, h) != (f.w, f.h) {
            return Err(D::Error::custom("a reference plane's PNG is not the size it says"));
        }
        let mut r = reader();
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(64 * 1024 * 1024);
        r.limits(limits);
        let img = r.decode().map_err(D::Error::custom)?.to_rgba8();
        Image::new(w, h, img.into_raw()).ok_or_else(|| D::Error::custom("a reference plane's image does not fit"))
    }
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// One view of a turnaround sheet, as a plane (see the module doc).
/// Pixel positions are in `image`'s pixels; rows grow downward, and an
/// edge at `n` lies between pixels `n - 1` and `n`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct RefPlane {
    /// The view slot it fills (front, back, left, right, three_quarter).
    pub slot: String,
    pub image: Image,
    /// Image pixels per world unit: the sheet's shared scale.
    pub px_per_unit: f32,
    /// The row edge of the view's ground line (its subject's lowest edge).
    pub ground_row: f32,
    /// The column at the subject box's middle: the plane's origin.
    pub center_col: f32,
    /// The view's box in the image (min x, min y, max x, max y, pixels,
    /// inclusive): the subject is the image's subject inside it.
    pub subject_box: [u32; 4],
    /// The background difference that counts as subject (`set_reference`'s
    /// threshold).
    pub threshold: u8,
    /// The world height of the ground line.
    pub ground_y: f32,
    /// How strongly the image shows over the viewport's background, 0..1.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub opacity: f32,
    /// The sheet it was cut from (for the reply; never read again).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

impl RefPlane {
    /// Whether the plane's numbers can be drawn (finite, in range): a
    /// damaged file's plane is refused on load.
    pub fn is_sane(&self) -> bool {
        let [w, h] = self.image.size();
        let b = self.subject_box;
        self.px_per_unit.is_finite() && self.px_per_unit > 0.0 && self.ground_row.is_finite() && self.center_col.is_finite() && self.ground_y.is_finite()
            && (0.0..=1.0).contains(&self.opacity) && b[0] <= b[2] && b[1] <= b[3] && (b[2] as usize) < w && (b[3] as usize) < h
    }

    /// The plane as a mesh in its object's space: the image as a quad in
    /// the XY plane facing +Z, the ground line at y = 0 and the subject
    /// box's middle at x = 0, at `px_per_unit`; the image faded toward
    /// the viewport's background by `opacity`.
    pub fn mesh(&self) -> Mesh {
        let [w, h] = self.image.size();
        let side = w.max(h);
        let bg = BACKGROUND;
        let mut color = vec![0u8; side * side * 4];
        let o = self.opacity.clamp(0.0, 1.0);
        for y in 0..h {
            for x in 0..w {
                let s = &self.image.rgba()[(y * w + x) * 4..][..4];
                let a = s[3] as f32 / 255.0 * o;
                let d = &mut color[(y * side + x) * 4..][..4];
                for c in 0..3 {
                    d[c] = (s[c] as f32 * a + bg[c] as f32 * (1.0 - a)).round() as u8;
                }
                d[3] = 255;
            }
        }
        let textures = Textures { size: side as u32, color, normal: [128u8, 128, 255, 255].repeat(side * side), orm: [255u8, 255, 0, 255].repeat(side * side), alpha: false };
        let mut material = Material::default();
        material.name = "reference".into();
        material.roughness = 1.0;
        let ppu = self.px_per_unit.max(1e-9);
        let corner = |col: f32, row: f32| Vertex {
            pos: [(col - self.center_col) / ppu, (self.ground_row - row) / ppu, 0.0],
            normal: [0.0, 0.0, 1.0],
            color: [1.0; 3],
            surface: material.surface(),
            material: 0,
            uv: [col / side as f32, row / side as f32],
            // w = 2 marks a reference plane for the viewport's shader:
            // one-sided, unlit, no shadow, no occlusion.
            tangent: [1.0, 0.0, 0.0, 2.0],
            light: material.light(),
            base: [1.0, 1.0, 1.0, -1.0],
            wear: [0.0; 3],
            painted: [0; 4],
        };
        let (wf, hf) = (w as f32, h as f32);
        Mesh {
            vertices: vec![corner(0.0, 0.0), corner(wf, 0.0), corner(wf, hf), corner(0.0, hf)],
            indices: vec![0, 3, 2, 0, 2, 1],
            edges: vec![0, 1, 1, 2, 2, 3, 3, 0],
            materials: vec![material],
            textures: Some(Arc::new(textures)),
            ..Mesh::default()
        }
    }

    /// Where the plane's origin (ground line under the subject's middle)
    /// sits and how it turns, for a view whose camera direction (toward
    /// the camera) is `dir`, standing `back` behind `center` (the model's
    /// middle) on the ground at `ground_y`.
    pub fn placement(dir: Vec3, center: Vec3, ground_y: f32, back: f32) -> (Vec3, glam::Quat) {
        let up = Vec3::Y;
        let right = up.cross(dir).normalize_or(Vec3::X);
        let dir = right.cross(up);
        let origin = Vec3::new(center.x, ground_y, center.z) - dir * back;
        // Columns: the image's right, up and facing directions.
        (origin, glam::Quat::from_mat3(&glam::Mat3::from_cols(right, up, dir)))
    }
}
