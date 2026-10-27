//! Surface materials: how a surface reflects light (its color comes from the
//! blobs, paint, or object color).
//!
//! Blobs and objects name a material. The name is looked up in the scene's
//! own library first, then in the built-in presets (`PRESETS`). Unknown or
//! empty names use `clay`. Meshes keep a resolved copy (`Mesh::materials`)
//! and each vertex keeps an index into it plus the values the shader needs,
//! so exports can split a mesh by material and the viewport needs no lookup.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// More scene materials than this are refused (and cut from loaded files).
pub const MAX_MATERIALS: usize = 256;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Material {
    pub name: String,
    /// Optional sRGB substrate color, independent of blob/object colors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color: Option<[f32; 3]>,
    /// Keep painted flags and markings over the substrate (default true).
    #[serde(default = "keep_paint_default")]
    pub keep_paint: bool,
    /// 0 = mirror-sharp highlights, 1 = fully matte.
    #[serde(default = "default_roughness")]
    pub roughness: f32,
    /// 0 = paint, skin, cloth; 1 = bare metal (tints reflections by the color).
    #[serde(default)]
    pub metallic: f32,
    /// A thin glossy layer on top: wet eyes, varnish, car paint.
    #[serde(default)]
    pub clearcoat: f32,
    /// A soft glow at grazing angles: velvet, fabric fuzz, peach skin.
    #[serde(default)]
    pub sheen: f32,
    /// Light that enters and spreads under the surface before coming back
    /// out (skin, wax, marble). Film renders only; 0 = none, 1 = all of
    /// the matte part.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subsurface: f32,
    /// How far red, green and blue light travel under the surface, in
    /// scene units (meters). Red goes furthest in skin.
    #[serde(default = "default_scatter")]
    pub scatter: [f32; 3],
    /// Scales every `scatter` distance (Stage 4d): above 1 the
    /// Christensen-Burley profile spreads wider and the falloff at shadow
    /// edges is softer (the stylised skin presets use 1.6). Missing in
    /// files saved before it: 1, as they rendered.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub subsurface_radius: f32,
    /// Procedural skin bump and roughness variation (film and bake).
    #[serde(default)]
    pub pores: f32,
    /// Subtle skin pigment variation (film and bake).
    #[serde(default)]
    pub mottling: f32,
    /// Blend a broad skin reflection with the main GGX lobe in film.
    #[serde(default)]
    pub skin_specular: f32,
    /// Light the surface gives off, in its own color: 1 = as bright as a
    /// white surface in the key light; glowing eyes, lava, screens, magic.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub emission: f32,
    /// How much light passes through, bending at the surface (glass,
    /// water, crystal, gems); the color tints what passes. 0 = opaque.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transmission: f32,
    /// Index of refraction for `transmission`: 1.33 water, 1.5 glass,
    /// 1.77 sapphire, 2.42 diamond.
    #[serde(default = "default_ior")]
    pub ior: f32,
    /// Weathering that follows the shape (see `weather.rs`), each 0..1:
    /// creases darker, edges rubbed lighter, dirt in low places, color
    /// blotches and patchy shine `variation_scale` apart.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cavity: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub edge_wear: f32,
    /// Negative = unset: no weathering dirt, and smart layers with the
    /// dirt role at their preset amount.
    #[serde(default = "unset", skip_serializing_if = "is_unset")]
    pub dirt: f32,
    #[serde(default = "default_dirt_color")]
    pub dirt_color: [f32; 3],
    #[serde(default, skip_serializing_if = "is_zero")]
    pub variation: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gloss_variation: f32,
    /// Size of the blotches (scene units); 0 = from the object's size.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub variation_scale: f32,
    /// A detail texture from the built-in library (`detail.rs`): cloth
    /// weave, leather grain, pores, scratches. None = take the preset's of
    /// the same name (if any); "" or "none" = off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Tile size in scene units; 0 = the texture's real size.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub detail_scale: f32,
    /// Relief (normal and roughness) strength, 0..4.
    #[serde(default = "default_detail_strength")]
    pub detail_strength: f32,
    /// How much the texture's own color pattern modulates the color, 0..1.
    #[serde(default = "default_detail_color")]
    pub detail_color: f32,
    /// Turns the texture, in degrees.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub detail_rotate: f32,
    /// Film renders move the surface by the detail map's height, this many
    /// scene units from its lowest to highest (real geometry: chips and
    /// scales in silhouette and shadow). 0 = shading relief only.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub detail_displace: f32,
    /// Smart material layers over the look above, bottom first (see
    /// `smart.rs`). A scene material named like a smart preset that has
    /// none keeps the preset's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<crate::smart::Layer>,
    /// How much of the layers with role wear and rust show, 0..1 (unset:
    /// the preset's, 0.5); `dirt` drives the dirt role the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wear: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<f32>,
}

fn keep_paint_default() -> bool { true }

fn unset() -> f32 {
    -1.0
}

fn is_unset(v: &f32) -> bool {
    *v < 0.0
}

fn default_detail_strength() -> f32 {
    1.0
}

fn default_detail_color() -> f32 {
    0.35
}

fn default_dirt_color() -> [f32; 3] {
    [0.24, 0.19, 0.14]
}

fn default_ior() -> f32 {
    1.5
}

/// Most `emission` (exports call it emissive strength).
pub const MAX_EMISSION: f32 = 100.0;

fn default_roughness() -> f32 {
    0.55
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

/// Bounds of `subsurface_radius`.
pub const SUBSURFACE_RADIUS: (f32, f32) = (0.1, 4.0);

/// Human skin's mean free path (Jensen et al. 2001), in meters: right for
/// a character about 1.7 units tall.
fn default_scatter() -> [f32; 3] {
    [0.0037, 0.0014, 0.0007]
}

/// (name, roughness, metallic, clearcoat, sheen, use).
pub const PRESETS: [(&str, f32, f32, f32, f32, &str); 22] = [
    ("clay", 0.55, 0.0, 0.0, 0.0, "default; soft satin"),
    ("skin", 0.48, 0.0, 0.0, 0.04, "skin"),
    ("lips", 0.36, 0.0, 0.08, 0.0, "soft moist lips; paint rose [0.70, 0.43, 0.42]"),
    ("cloth", 0.9, 0.0, 0.0, 0.6, "fabric, wool, felt"),
    ("velvet", 0.8, 0.0, 0.0, 1.0, "velvet, fur-like cloth"),
    ("leather", 0.5, 0.0, 0.15, 0.0, "leather, boots, belts"),
    ("scales", 0.42, 0.0, 0.1, 0.0, "reptile or dragon skin with scales"),
    ("hair", 0.45, 0.0, 0.0, 0.35, "hair, fur"),
    ("wood", 0.7, 0.0, 0.0, 0.0, "wood, bark"),
    ("plastic", 0.3, 0.0, 0.0, 0.0, "plastic, toys"),
    ("rubber", 0.9, 0.0, 0.0, 0.0, "rubber, matte paint"),
    ("metal", 0.32, 1.0, 0.0, 0.0, "steel, iron, worn gold"),
    ("polished_metal", 0.1, 1.0, 0.0, 0.0, "chrome, jewelry"),
    ("gloss", 0.05, 0.0, 1.0, 0.0, "wet or glassy: eyes, gems, nails"),
    ("glow", 0.5, 0.0, 0.0, 0.0, "gives off light in its color: glowing eyes, lava, runes, screens"),
    ("glass", 0.02, 0.0, 0.0, 0.0, "clear see-through glass (color tints it): windows, bottles, lenses"),
    ("water", 0.02, 0.0, 0.0, 0.0, "see-through water, ice, tears"),
    ("crystal", 0.05, 0.0, 0.0, 0.0, "see-through gems and crystal that sparkle (strong bending)"),
    // Stage 4d: the soft, saturated, forgiving look of stylised feature
    // animation (see `Material::preset` for their other values).
    ("skin_stylized", 0.45, 0.0, 0.0, 0.15, "stylised character skin: soft wide subsurface, saturated, no pores, soft rim"),
    ("lips_stylized", 0.34, 0.0, 0.12, 0.05, "stylised lips: soft and a little moist; paint rose [0.80, 0.45, 0.45]"),
    ("cheek_blush", 0.5, 0.0, 0.0, 0.18, "stylised skin for cheeks, nose tip and ears: redder glow; paint a soft pink [0.92, 0.62, 0.58]"),
    ("fabric_soft", 0.85, 0.0, 0.0, 0.5, "stylised cloth: soft sheen, no weave texture"),
];

/// The stylised skin presets' scatter distances (before
/// `subsurface_radius`): every color reaching further than in realistic
/// skin, red furthest ahead, so the light that comes back out is more
/// saturated (red/green 2.8 and red/blue 8, against 2.6 and 5.3 for
/// `skin`). With the plan's subsurface 0.6 (less than skin's 0.8, so more
/// of the light keeps a sharp edge) and radius 1.6, this is what widens
/// the shadow edge to 1.5x realistic skin's (`stylized_skin_softer`).
pub const STYLIZED_SCATTER: [f32; 3] = [0.0056, 0.0020, 0.0007];

impl Default for Material {
    fn default() -> Self {
        Material::preset("clay").expect("clay is a preset")
    }
}

impl Material {
    pub fn preset(name: &str) -> Option<Material> {
        let Some(p) = PRESETS.iter().find(|p| p.0 == name) else { return crate::smart::preset(name) };
        Some(p).map(|&(name, roughness, metallic, clearcoat, sheen, _)| Material {
            name: name.to_string(),
            base_color: None,
            keep_paint: true,
            roughness,
            metallic,
            clearcoat,
            sheen,
            subsurface: match name {
                "skin" => 0.8,
                "lips" | "lips_stylized" => 0.5,
                "skin_stylized" => 0.6,
                "cheek_blush" => 0.7,
                _ => 0.0,
            },
            pores: if name == "skin" { 0.65 } else { 0.0 },
            mottling: match name {
                "skin" => 0.4,
                "skin_stylized" | "cheek_blush" => 0.1,
                _ => 0.0,
            },
            skin_specular: match name {
                "skin" | "lips" => 0.8,
                "skin_stylized" | "cheek_blush" => 0.4,
                "lips_stylized" => 0.5,
                _ => 0.0,
            },
            scatter: match name {
                "lips" | "lips_stylized" => [0.0012, 0.0006, 0.0003],
                "skin_stylized" => STYLIZED_SCATTER,
                // Redder still: blush is blood close under the skin.
                "cheek_blush" => [0.0064, 0.0020, 0.0007],
                _ => default_scatter(),
            },
            subsurface_radius: if matches!(name, "skin_stylized" | "lips_stylized" | "cheek_blush") { 1.6 } else { 1.0 },
            emission: if name == "glow" { 2.5 } else { 0.0 },
            transmission: if matches!(name, "glass" | "water" | "crystal") { 1.0 } else { 0.0 },
            ior: match name {
                "water" => 1.33,
                "crystal" => 2.0,
                _ => default_ior(),
            },
            cavity: 0.0,
            edge_wear: 0.0,
            dirt: unset(),
            dirt_color: default_dirt_color(),
            variation: 0.0,
            gloss_variation: 0.0,
            variation_scale: 0.0,
            detail: preset_detail(name).map(|d| d.0.to_string()),
            detail_scale: 0.0,
            detail_strength: preset_detail(name).map_or(default_detail_strength(), |d| d.1),
            detail_color: preset_detail(name).map_or(default_detail_color(), |d| d.2),
            detail_rotate: 0.0,
            detail_displace: 0.0,
            layers: Vec::new(),
            wear: None,
            rust: None,
        })
    }

    /// Resolve the substrate before applying sparse painted markings. Shared
    /// by meshing and exact baking; smart wear and damage are applied later.
    pub fn painted_color(&self, base: Vec3, paint: Option<&crate::paint::Paint>, p: Vec3) -> Vec3 {
        let base = self.base_color.map_or(base, Vec3::from);
        if self.keep_paint { paint.map_or(base, |g| g.grid().apply(p, base)) } else { base }
    }

    /// The detail texture to use, if any (see `detail`).
    pub fn detail_name(&self) -> Option<&str> {
        self.detail.as_deref().filter(|d| !d.is_empty() && *d != "none")
    }

    /// The scene's material called `name`, else the preset, else None.
    /// A scene material named like a preset that does not say `detail`
    /// keeps the preset's detail texture.
    /// Smart references in `layers` are spliced in.
    pub fn find(library: &[Material], name: &str) -> Option<Material> {
        let mut m = match library.iter().find(|m| m.name == name).cloned() {
            Some(mut m) => {
                if let Some(p) = Material::preset(name) {
                    if m.detail.is_none() {
                        (m.detail, m.detail_scale, m.detail_strength, m.detail_color, m.detail_rotate) = (p.detail, p.detail_scale, p.detail_strength, p.detail_color, p.detail_rotate);
                    }
                    // Paint overrides must remain empty after factory_new;
                    // retain historical fallback for other smart presets.
                    let paint = crate::damage::wear_layers(name, 0.0).is_some()
                        || matches!(name, "dusty_khaki" | "olive_drab" | "gunmetal" | "worn_steel");
                    if m.layers.is_empty() && !paint { m.layers = p.layers; }
                }
                m
            }
            None => Material::preset(name)?,
        };
        if m.layers.iter().any(|l| l.smart.is_some()) {
            m.layers = crate::smart::expand(&m.layers, 0);
        }
        Some(m)
    }

    /// Like `find`, but an empty or unknown name gives `clay`.
    pub fn resolve(library: &[Material], name: &str) -> Material {
        Material::find(library, name).unwrap_or_default()
    }

    /// The per-color scatter distances the renderer uses: `scatter`
    /// times `subsurface_radius` (CPU and GPU both read this).
    pub fn scatter_radius(&self) -> Vec3 {
        Vec3::from(self.scatter) * self.subsurface_radius
    }

    /// Values packed for the shader: roughness, metallic, clearcoat, sheen.
    pub fn surface(&self) -> [f32; 4] {
        [self.roughness, self.metallic, self.clearcoat, self.sheen]
    }

    /// Whether any weathering is set (see `weather.rs`).
    /// Layered materials do not weather: their layers do it.
    pub fn weathered(&self) -> bool {
        self.layers.is_empty() && self.cavity > 0.0 || self.edge_wear > 0.0 || self.dirt > 0.0 || self.variation > 0.0 || self.gloss_variation > 0.0
    }

    /// More values for the shader: emission, transmission.
    pub fn light(&self) -> [f32; 2] {
        [self.emission, self.transmission]
    }

    /// Object-space detail in meters, shared by film shading and texture
    /// baking (sRGB color). Pigment is separate from the sub-millimeter surface relief.
    pub fn skin_detail(&self, p: Vec3, n: Vec3, color: Vec3, rough: f32) -> (Vec3, Vec3, f32) {
        if self.subsurface <= 0.0 || (self.pores == 0.0 && self.mottling == 0.0) {
            return (n, color, rough);
        }
        let q = p / 0.00025;
        let height = |q: Vec3| -noise(q).powi(3);
        let e = 0.08;
        let gradient = Vec3::new(height(q + Vec3::X * e) - height(q - Vec3::X * e), height(q + Vec3::Y * e) - height(q - Vec3::Y * e), height(q + Vec3::Z * e) - height(q - Vec3::Z * e)) * (0.14 * self.pores / (2.0 * e));
        let bumped = (n - (gradient - n * gradient.dot(n))).normalize_or(n);
        let pigment = noise(p / 0.003) * 0.6 + noise(p / 0.011) * 0.4 - 0.5;
        let tint = Vec3::ONE + Vec3::new(0.08, -0.16, -0.20) * pigment * self.mottling;
        let rough = (rough + (noise(p / 0.0008) - 0.5) * 0.18 * self.pores).clamp(0.03, 1.0);
        (bumped, color * tint, rough)
    }

    /// Clamps every value into 0..1 (NaN becomes the default).
    pub fn sanitize(&mut self) {
        let fix = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
        self.base_color = self.base_color.map(|c| c.map(|v| fix(v, 0.5)));
        self.roughness = fix(self.roughness, default_roughness());
        self.metallic = fix(self.metallic, 0.0);
        self.clearcoat = fix(self.clearcoat, 0.0);
        self.sheen = fix(self.sheen, 0.0);
        self.subsurface = fix(self.subsurface, 0.0);
        self.pores = fix(self.pores, 0.0);
        self.mottling = fix(self.mottling, 0.0);
        self.skin_specular = fix(self.skin_specular, 0.0);
        self.emission = if self.emission.is_finite() { self.emission.clamp(0.0, MAX_EMISSION) } else { 0.0 };
        self.transmission = fix(self.transmission, 0.0);
        self.ior = if self.ior.is_finite() { self.ior.clamp(1.0, 3.0) } else { default_ior() };
        for v in [&mut self.cavity, &mut self.edge_wear, &mut self.variation, &mut self.gloss_variation] {
            *v = fix(*v, 0.0);
        }
        self.dirt = if self.dirt.is_finite() && self.dirt >= 0.0 { self.dirt.min(1.0) } else { unset() };
        self.wear = self.wear.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        self.rust = self.rust.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        self.layers.truncate(crate::smart::MAX_LAYERS);
        for l in &mut self.layers {
            l.sanitize();
        }
        self.variation_scale = if self.variation_scale.is_finite() { self.variation_scale.clamp(0.0, 100.0) } else { 0.0 };
        self.dirt_color = self.dirt_color.map(|c| fix(c, 0.2));
        self.scatter = self.scatter.map(|v| if v.is_finite() { v.clamp(1e-5, 1.0) } else { 0.002 });
        self.subsurface_radius = if self.subsurface_radius.is_finite() { self.subsurface_radius.clamp(SUBSURFACE_RADIUS.0, SUBSURFACE_RADIUS.1) } else { 1.0 };
        self.name = self.name.trim().chars().take(64).collect();
        self.detail = self.detail.take().map(|d| d.trim().chars().take(64).collect());
        self.detail_scale = if self.detail_scale.is_finite() { self.detail_scale.clamp(0.0, 100.0) } else { 0.0 };
        self.detail_strength = if self.detail_strength.is_finite() { self.detail_strength.clamp(0.0, 4.0) } else { default_detail_strength() };
        self.detail_color = fix(self.detail_color, default_detail_color());
        self.detail_rotate = if self.detail_rotate.is_finite() { self.detail_rotate.rem_euclid(360.0) } else { 0.0 };
        self.detail_displace = if self.detail_displace.is_finite() { self.detail_displace.clamp(0.0, 1.0) } else { 0.0 };
    }
}

/// The detail texture each preset starts with: (texture, strength, color).
fn preset_detail(name: &str) -> Option<(&'static str, f32, f32)> {
    Some(match name {
        "skin" => ("skin_pores", 0.6, 0.1),
        "lips" => ("wrinkles", 0.3, 0.0),
        "cloth" => ("linen", 1.3, 0.35),
        "velvet" => ("velvet", 0.8, 0.3),
        "leather" => ("leather_rough", 0.8, 0.5),
        "scales" => ("scales", 1.0, 0.5),
        "wood" => ("wood", 1.0, 0.6),
        "metal" => ("metal_brushed", 0.8, 0.3),
        "polished_metal" => ("metal_brushed", 0.15, 0.0),
        _ => return None,
    })
}

/// Smooth deterministic value noise, without a texture or a voxel grid
/// (`mat_noise` in `film/trace.wgsl`).
pub(crate) fn noise(p: Vec3) -> f32 {
    let cell = p.floor().as_ivec3();
    let f = p - p.floor();
    let f = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let mut value = 0.0;
    for z in 0..=1 {
        for y in 0..=1 {
            for x in 0..=1 {
                let c = cell + glam::IVec3::new(x, y, z);
                let mut h = (c.x as u32).wrapping_mul(73856093) ^ (c.y as u32).wrapping_mul(19349663) ^ (c.z as u32).wrapping_mul(83492791);
                h = (h ^ (h >> 16)).wrapping_mul(0x7feb352d);
                h = (h ^ (h >> 15)).wrapping_mul(0x846ca68b);
                h ^= h >> 16;
                let w = |a: f32, k: i32| if k == 0 { 1.0 - a } else { a };
                value += h as f32 / u32::MAX as f32 * w(f.x, x) * w(f.y, y) * w(f.z, z);
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micro_controls_sanitize_and_do_not_change_non_skin() {
        let mut m = Material { pores: f32::NAN, mottling: 3.0, skin_specular: -1.0, ..Material::default() };
        m.sanitize();
        assert_eq!((m.pores, m.mottling, m.skin_specular), (0.0, 1.0, 0.0));
        assert_eq!(m.skin_detail(Vec3::ONE, Vec3::Y, Vec3::ONE, 0.5), (Vec3::Y, Vec3::ONE, 0.5));
        let loaded: Material = serde_json::from_str(&serde_json::to_string(&Material::preset("skin").unwrap()).unwrap()).unwrap();
        assert_eq!(loaded.pores, 0.65);
    }

    #[test]
    fn skin_detail_is_repeatable_bounded_and_switchable() {
        let mut m = Material::preset("skin").unwrap();
        let p = Vec3::new(0.0123, 1.7024, 0.0927);
        let a = m.skin_detail(p, Vec3::Z, Vec3::splat(0.5), m.roughness);
        assert_eq!(a, m.skin_detail(p, Vec3::Z, Vec3::splat(0.5), m.roughness));
        assert!(a.0.dot(Vec3::Z) > 0.98 && (a.0.length() - 1.0).abs() < 1e-6);
        assert!(a.1.min_element() > 0.45 && a.1.max_element() < 0.55);
        assert_ne!(a, m.skin_detail(p + Vec3::X * 0.0002, Vec3::Z, Vec3::splat(0.5), m.roughness));
        m.pores = 0.0;
        m.mottling = 0.0;
        assert_eq!(m.skin_detail(p, Vec3::Z, Vec3::ONE, 0.5), (Vec3::Z, Vec3::ONE, 0.5));
        let old: Material = serde_json::from_str(r#"{"name":"custom"}"#).unwrap();
        assert_eq!(old.pores, 0.0);
        assert!(Material::preset("lips").unwrap().subsurface > 0.0);
    }

    #[test]
    fn library_overrides_presets_and_unknown_is_clay() {
        let lib = vec![Material { name: "skin".into(), roughness: 0.2, metallic: 0.0, clearcoat: 0.0, sheen: 0.0, ..Material::default() }];
        assert_eq!(Material::resolve(&lib, "skin").roughness, 0.2);
        assert_eq!(Material::resolve(&[], "skin").roughness, 0.48);
        assert_eq!(Material::resolve(&[], "nope").name, "clay");
        assert_eq!(Material::resolve(&[], "").name, "clay");
        assert!(Material::find(&[], "nope").is_none());
    }

    #[test]
    fn detail_textures_come_with_presets_and_scene_overrides_keep_them() {
        assert_eq!(Material::preset("cloth").unwrap().detail_name(), Some("linen"));
        assert_eq!(Material::preset("scales").unwrap().detail_name(), Some("scales"));
        assert!(Material::preset("plastic").unwrap().detail_name().is_none());
        // Every preset's detail exists in the library.
        for p in PRESETS {
            if let Some(d) = Material::preset(p.0).unwrap().detail_name() {
                assert!(crate::detail::entry(d).is_some(), "{} names {d}", p.0);
            }
        }
        // A scene "cloth" that predates detail keys keeps the preset's.
        let old: Material = serde_json::from_str(r#"{"name":"cloth","roughness":0.9,"sheen":0.5}"#).unwrap();
        let found = Material::find(&[old], "cloth").unwrap();
        assert_eq!((found.detail_name(), found.roughness), (Some("linen"), 0.9));
        // Its own choice wins, including off.
        let off = Material { name: "cloth".into(), detail: Some("none".into()), ..Material::default() };
        assert!(Material::find(&[off], "cloth").unwrap().detail_name().is_none());
        let custom = Material { name: "cloth".into(), detail: Some("denim".into()), ..Material::default() };
        assert_eq!(Material::find(&[custom], "cloth").unwrap().detail_name(), Some("denim"));
        // Saved and loaded unchanged; values are clamped.
        let mut m = Material { detail: Some(" knit ".into()), detail_scale: -1.0, detail_strength: 9.0, detail_color: f32::NAN, detail_rotate: 450.0, ..Material::default() };
        m.sanitize();
        assert_eq!((m.detail_name(), m.detail_scale, m.detail_strength, m.detail_color, m.detail_rotate), (Some("knit"), 0.0, 4.0, 0.35, 90.0));
        let back: Material = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    /// Stage 4d: the stylised presets carry the plan's values, survive a
    /// save and load (subsurface_radius stored), and a material saved
    /// before the field loads at 1.
    #[test]
    fn stylized_presets_have_the_plan_values() {
        let s = Material::preset("skin_stylized").unwrap();
        assert_eq!((s.subsurface, s.subsurface_radius, s.pores, s.mottling, s.roughness, s.skin_specular, s.sheen), (0.6, 1.6, 0.0, 0.1, 0.45, 0.4, 0.15));
        assert!(s.detail_name().is_none(), "no pore texture");
        assert_eq!(s.scatter, STYLIZED_SCATTER);
        let real = Material::preset("skin").unwrap();
        let ratio = |m: &Material| (m.scatter[0] / m.scatter[1], m.scatter[0] / m.scatter[2]);
        assert!(ratio(&s).0 > ratio(&real).0 && ratio(&s).1 > ratio(&real).1, "more saturated: {:?} against {:?}", ratio(&s), ratio(&real));
        assert!(s.scatter_radius().cmpge(real.scatter_radius() * 1.6 - Vec3::splat(1e-7)).all(), "wider: {} against {}", s.scatter_radius(), real.scatter_radius());
        for name in ["skin_stylized", "lips_stylized", "cheek_blush", "fabric_soft"] {
            let m = Material::preset(name).unwrap();
            let mut checked = m.clone();
            checked.sanitize();
            assert_eq!(checked, m, "{name} is within its bounds");
            let back: Material = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
            assert_eq!(back, m, "{name} round-trips");
        }
        assert!(serde_json::to_string(&s).unwrap().contains("\"subsurface_radius\":1.6"));
        assert!(!serde_json::to_string(&real).unwrap().contains("subsurface_radius"));
        let old: Material = serde_json::from_str(r#"{"name":"face","subsurface":0.8}"#).unwrap();
        assert_eq!(old.subsurface_radius, 1.0);
        assert_eq!(old.scatter_radius(), Vec3::from(old.scatter));
        let mut wild = Material { subsurface_radius: f32::NAN, ..Material::default() };
        wild.sanitize();
        assert_eq!(wild.subsurface_radius, 1.0);
        wild.subsurface_radius = 99.0;
        wild.sanitize();
        assert_eq!(wild.subsurface_radius, SUBSURFACE_RADIUS.1);
        let lips = Material::preset("lips_stylized").unwrap();
        assert!(lips.subsurface > 0.0 && lips.pores == 0.0 && lips.subsurface_radius == 1.6);
        let fabric = Material::preset("fabric_soft").unwrap();
        assert!(fabric.subsurface == 0.0 && fabric.sheen >= 0.5 && fabric.detail_name().is_none());
    }
}
