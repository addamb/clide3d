//! Smart materials: layered looks driven by masks, like Substance Painter's
//! smart materials, plus decals.
//!
//! A material can carry `layers`: an ordered stack, each a full look
//! (color, roughness, metallic, clearcoat, sheen, glow, a detail map)
//! laid over the ones below where its **mask** says. A mask folds a list
//! of terms (`Term`) with multiply / max / min / add / subtract / screen,
//! each term read from the shape or space at the point:
//!
//! - `edges` and `cavity`: convex and concave curvature (`weather.rs`),
//! - `ao`: how hidden the point is (sampled from the clay's distance field),
//! - `up`, `down`, `direction`: which way the surface faces (object space),
//! - `height`: the object's own y between two values (water lines, mud),
//! - `noise`: fractal value noise of a size and octave count,
//! - `grunge`: a library detail map's height (`detail.rs`) as a pattern,
//! - `painted`: a mask channel painted with the `paint` tool,
//! - `constant`.
//!
//! Each term, and the whole mask, can be inverted and put through levels
//! (`threshold`, `contrast`). Layers with a `role` (wear, rust, dirt) follow
//! the material's `wear`, `rust` and `dirt` amounts, which move the mask's
//! threshold, so one number spreads or shrinks rust everywhere.
//!
//! Smart presets (`PRESETS`: worn_iron, gold_trim, old_leather, demon_skin,
//! dungeon_stone...) are whole stacks under one name; a layer `{"smart":
//! "blood_splatter"}` splices one preset's layers into another stack.
//!
//! Decals (`Decal`) are stamped onto clay at a point with a size and turn:
//! procedural runes, scratches, blood splats and emblems, or a PNG image;
//! they change color, roughness, metal, glow and relief.
//!
//! The same code runs everywhere (the GPU tracer has a copy in
//! `film/trace.wgsl`, `smart_*` functions, kept in step): clay meshes get
//! a per-vertex result in their colors and surface values (the viewport
//! and plain exports), the film tracer evaluates it per pixel from the
//! vertex's mask inputs (`Vertex::base`, `wear`, `painted`), and bakes per
//! texel.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex, OnceLock};

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::material::{Material, noise};

/// Most layers in one material (after splicing smart references).
pub const MAX_LAYERS: usize = 16;
/// Most terms in one mask.
pub const MAX_TERMS: usize = 8;
/// Most decals on one clay object.
pub const MAX_DECALS: usize = 64;
/// Most painted mask channels on one clay object.
pub const MAX_CHANNELS: usize = 4;
/// `Term::slot` for a painted channel the clay does not have.
pub const NO_SLOT: u8 = 255;

/// One layer of a material: a look and where it shows.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Layer {
    /// Splice in this smart preset's layers here (the other fields except
    /// `opacity` are then ignored; opacity scales them all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// "wear", "rust" or "dirt": follows the material's amount of that.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// sRGB; None keeps the color below.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// Multiplies the layer's color (or the color below).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clearcoat: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheen: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emission: Option<f32>,
    /// Brightness blotches in the layer's color, 0..1, `variation_scale` apart.
    #[serde(skip_serializing_if = "is_zero")]
    pub variation: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub variation_scale: f32,
    /// A library detail map for this layer (see `Material::detail`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub detail_scale: f32,
    pub detail_strength: f32,
    pub detail_color: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub detail_rotate: f32,
    /// Terms folded in order; empty = everywhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mask: Vec<Term>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    #[serde(skip_serializing_if = "is_false")]
    pub invert: bool,
    pub opacity: f32,
}

impl Default for Layer {
    fn default() -> Self {
        Layer {
            smart: None,
            name: String::new(),
            role: String::new(),
            color: None,
            tint: None,
            roughness: None,
            metallic: None,
            clearcoat: None,
            sheen: None,
            emission: None,
            variation: 0.0,
            variation_scale: 0.0,
            detail: None,
            detail_scale: 0.0,
            detail_strength: 1.0,
            detail_color: 0.35,
            detail_rotate: 0.0,
            mask: Vec::new(),
            threshold: None,
            contrast: None,
            invert: false,
            opacity: 1.0,
        }
    }
}

impl Layer {
    /// Clamps every value (NaN becomes the default) and caps the mask.
    pub fn sanitize(&mut self) {
        let unit = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
        let opt = |v: Option<f32>| v.map(|v| unit(v, 0.5));
        let text = |s: &mut String| *s = s.trim().chars().take(64).collect();
        self.smart = self.smart.take().map(|mut s| {
            text(&mut s);
            s
        });
        text(&mut self.name);
        text(&mut self.role);
        self.color = self.color.map(|c| c.map(|v| unit(v, 0.5)));
        self.tint = self.tint.map(|c| c.map(|v| if v.is_finite() { v.clamp(0.0, 4.0) } else { 1.0 }));
        (self.roughness, self.metallic, self.clearcoat, self.sheen) = (opt(self.roughness), opt(self.metallic), opt(self.clearcoat), opt(self.sheen));
        self.emission = self.emission.map(|v| if v.is_finite() { v.clamp(0.0, crate::material::MAX_EMISSION) } else { 0.0 });
        self.variation = unit(self.variation, 0.0);
        self.variation_scale = if self.variation_scale.is_finite() { self.variation_scale.clamp(0.0, 100.0) } else { 0.0 };
        self.detail = self.detail.take().map(|mut d| {
            text(&mut d);
            d
        });
        self.detail_scale = if self.detail_scale.is_finite() { self.detail_scale.clamp(0.0, 100.0) } else { 0.0 };
        self.detail_strength = if self.detail_strength.is_finite() { self.detail_strength.clamp(0.0, 4.0) } else { 1.0 };
        self.detail_color = unit(self.detail_color, 0.35);
        self.detail_rotate = if self.detail_rotate.is_finite() { self.detail_rotate.rem_euclid(360.0) } else { 0.0 };
        self.threshold = self.threshold.map(|v| if v.is_finite() { v.clamp(-1.0, 2.0) } else { 0.5 });
        self.contrast = self.contrast.map(|v| if v.is_finite() { v.clamp(0.0, 100.0) } else { 1.0 });
        self.opacity = unit(self.opacity, 1.0);
        self.mask.truncate(MAX_TERMS);
        for t in &mut self.mask {
            t.threshold = t.threshold.map(|v| if v.is_finite() { v.clamp(-1.0, 2.0) } else { 0.5 });
            t.contrast = t.contrast.map(|v| if v.is_finite() { v.clamp(0.0, 100.0) } else { 1.0 });
            t.amount = if t.amount.is_finite() { t.amount.clamp(-4.0, 4.0) } else { 1.0 };
            t.scale = if t.scale.is_finite() { t.scale.clamp(0.0, 100.0) } else { 0.0 };
            t.octaves = t.octaves.clamp(1, 6);
            t.seed = if t.seed.is_finite() { t.seed.clamp(-1000.0, 1000.0) } else { 0.0 };
            t.direction = t.direction.map(|v| if v.is_finite() { v } else { 0.0 });
            t.range = t.range.map(|v| if v.is_finite() { v.clamp(-1e4, 1e4) } else { 0.0 });
            t.value = if t.value.is_finite() { t.value } else { 1.0 };
            text(&mut t.channel);
            text(&mut t.map);
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Constant,
    #[serde(alias = "edge")]
    Edges,
    #[serde(alias = "cavities", alias = "crevices")]
    Cavity,
    #[serde(alias = "occlusion")]
    Ao,
    Up,
    Down,
    #[serde(alias = "dir")]
    Direction,
    Height,
    Noise,
    Grunge,
    #[serde(alias = "paint")]
    Painted,
    /// Compiled world height: dot(position, direction) + value.
    HeightPlane,
    /// Compiled inverse-transpose normal orientation with its scale metric.
    DirectionMetric,
    /// Long gravity-aligned stains; direction is the drainage axis.
    Streaks,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    #[default]
    Multiply,
    Max,
    Min,
    Add,
    Subtract,
    Screen,
}

/// One mask term. In JSON a bare source name ("edges") is a term too.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default, from = "TermIn")]
pub struct Term {
    pub source: Source,
    /// World orientation/height, compiled into local coefficients at mesh build.
    #[serde(default)]
    pub world: bool,
    #[serde(skip)]
    pub metric: [f32;6],
    /// How it folds into the terms before it (the first one starts the mask).
    #[serde(skip_serializing_if = "is_multiply")]
    pub op: Op,
    #[serde(skip_serializing_if = "is_false")]
    pub invert: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    /// Scales the value (before folding).
    pub amount: f32,
    /// noise: feature size; grunge: tile size (scene units; 0 = default).
    #[serde(skip_serializing_if = "is_zero")]
    pub scale: f32,
    /// noise: detail levels (1..6, default 3).
    pub octaves: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub seed: f32,
    /// direction: the way the surface should face.
    pub direction: [f32; 3],
    /// height: object y where the mask is 0 and where it is 1.
    pub range: [f32; 2],
    /// painted: the channel name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// grunge: the library map whose height is the pattern.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub map: String,
    /// constant: the value.
    pub value: f32,
    /// painted: the channel's index in the mesh's `Vertex::painted`
    /// (set when a mesh is built).
    #[serde(skip)]
    pub slot: u8,
}

impl Default for Term {
    fn default() -> Self {
        Term {
            source: Source::Constant,
            world: false,
            metric: [1.0,1.0,1.0,0.0,0.0,0.0],
            op: Op::Multiply,
            invert: false,
            threshold: None,
            contrast: None,
            amount: 1.0,
            scale: 0.0,
            octaves: 3,
            seed: 0.0,
            direction: [0.0, 1.0, 0.0],
            range: [0.0, 1.0],
            channel: String::new(),
            map: String::new(),
            value: 1.0,
            slot: NO_SLOT,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TermIn {
    Name(Source),
    Full(Box<TermFull>),
}

/// `Term` without the shorthand, for deserializing.
#[derive(Deserialize)]
#[serde(default)]
struct TermFull {
    source: Source,
    world: bool,
    op: Op,
    invert: bool,
    threshold: Option<f32>,
    contrast: Option<f32>,
    amount: f32,
    scale: f32,
    octaves: u32,
    seed: f32,
    direction: [f32; 3],
    range: [f32; 2],
    channel: String,
    map: String,
    value: f32,
}

impl Default for TermFull {
    fn default() -> Self {
        let t = Term::default();
        TermFull { source: t.source, world: false, op: t.op, invert: t.invert, threshold: t.threshold, contrast: t.contrast, amount: t.amount, scale: t.scale, octaves: t.octaves, seed: t.seed, direction: t.direction, range: t.range, channel: t.channel, map: t.map, value: t.value }
    }
}

impl From<TermIn> for Term {
    fn from(t: TermIn) -> Term {
        match t {
            TermIn::Name(source) => Term { source, ..Term::default() },
            TermIn::Full(f) => Term {
                source: f.source,
                world: f.world,
                metric: [1.0,1.0,1.0,0.0,0.0,0.0],
                op: f.op,
                invert: f.invert,
                threshold: f.threshold,
                contrast: f.contrast,
                amount: f.amount,
                scale: f.scale,
                octaves: f.octaves,
                seed: f.seed,
                direction: f.direction,
                range: f.range,
                channel: f.channel,
                map: f.map,
                value: f.value,
                slot: NO_SLOT,
            },
        }
    }
}

fn one() -> f32 { 1.0 }

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_multiply(op: &Op) -> bool {
    *op == Op::Multiply
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Levels: `contrast` <= 0 leaves `v`; else a smooth step `1 / contrast`
/// wide around `threshold`.
pub fn levels(v: f32, threshold: f32, contrast: f32) -> f32 {
    if contrast <= 0.0 {
        return v;
    }
    let half = 0.5 / contrast;
    smooth(threshold - half, threshold + half, v)
}

/// (threshold, contrast) of a term or layer: none set = no levels.
fn levels_of(threshold: Option<f32>, contrast: Option<f32>) -> (f32, f32) {
    let c = contrast.unwrap_or(if threshold.is_some() { 1.0 } else { 0.0 });
    (threshold.unwrap_or(0.5), c)
}

/// Fractal value noise, stretched to fill 0..1.
pub fn fbm(p: Vec3, octaves: u32) -> f32 {
    let octaves = octaves.clamp(1, 6);
    let (mut sum, mut amp, mut norm, mut q) = (0.0, 1.0, 0.0, p);
    for _ in 0..octaves {
        sum += amp * noise(q);
        norm += amp;
        amp *= 0.5;
        q = q * 2.03 + Vec3::splat(17.17);
    }
    ((sum / norm - 0.5) * (1.6 + 0.35 * (octaves - 1) as f32) + 0.5).clamp(0.0, 1.0)
}

/// Default noise feature size (scene units).
const NOISE_SCALE: f32 = 0.1;
/// Default variation blotch size.
const VARIATION_SCALE: f32 = 0.08;

impl Term {
    /// Noise frequency (1 / size).
    pub fn freq(&self) -> f32 {
        1.0 / if self.scale > 0.0 { self.scale } else { NOISE_SCALE }
    }

    /// The grunge map (default stone) as (library index, tiles per unit).
    pub fn grunge(&self) -> Option<(usize, f32)> {
        let name = if self.map.is_empty() { "stone" } else { &self.map };
        let i = crate::detail::LIBRARY.iter().position(|e| e.name == name)?;
        let tile = if self.scale > 0.0 { self.scale } else { crate::detail::LIBRARY[i].tile };
        Some((i, 1.0 / tile))
    }

    /// The seed as a noise-space offset.
    pub fn offset(&self) -> Vec3 {
        Vec3::new(1.7, 3.1, 5.3) * self.seed
    }
}

/// What the masks read at one surface point (object space).
#[derive(Clone, Copy, Debug, Default)]
pub struct At {
    pub p: Vec3,
    /// The smooth (unbumped) outward normal.
    pub n: Vec3,
    pub edge: f32,
    pub cavity: f32,
    /// 1 = fully hidden.
    pub occlusion: f32,
    /// Painted channels by `Term::slot`.
    pub painted: [f32; 4],
    /// Scene units one pixel or texel covers (detail map mip).
    pub footprint: f32,
}

/// A look at a point: sRGB color, roughness, metal, clearcoat, sheen,
/// glow, and the shading normal (object space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub color: Vec3,
    pub rough: f32,
    pub metal: f32,
    pub coat: f32,
    pub sheen: f32,
    pub emission: f32,
    pub n: Vec3,
}

impl Look {
    /// `m`'s own values over `color` (sRGB) and normal `n`.
    pub fn of(m: &Material, color: Vec3, rough: f32, n: Vec3) -> Look {
        Look { color, rough, metal: m.metallic, coat: m.clearcoat, sheen: m.sheen, emission: m.emission, n }
    }
}

/// A grunge map's height at `p` (triplanar, unturned), as a pattern
/// around 0.5 (`Map::pattern`).
pub fn grunge_height(map: usize, freq: f32, at: &At) -> f32 {
    let Some(m) = crate::detail::map(crate::detail::LIBRARY[map].name) else { return 0.5 };
    let d = crate::detail::Params { map: m, freq, strength: 1.0, color: 0.0, turn: Vec2::new(1.0, 0.0), density: m.size() as f32 * freq };
    m.pattern(d.triplanar(at.p, at.n, d.lod(at.footprint)).0.height)
}

/// A term's value at `at`, after invert and levels, times `amount`.
pub fn term(t: &Term, at: &At) -> f32 {
    let v = match t.source {
        Source::Constant => t.value,
        Source::HeightPlane => at.p.dot(Vec3::from(t.direction)) + t.value,
        Source::DirectionMetric => {
            let n=at.n;let m=t.metric;
            let length=(m[0]*n.x*n.x+m[1]*n.y*n.y+m[2]*n.z*n.z+2.0*(m[3]*n.x*n.y+m[4]*n.x*n.z+m[5]*n.y*n.z)).max(1e-12).sqrt();
            n.dot(Vec3::from(t.direction))/length
        },
        Source::Edges => at.edge,
        Source::Cavity => at.cavity,
        Source::Ao => at.occlusion,
        Source::Up => at.n.y,
        Source::Down => -at.n.y,
        Source::Direction => at.n.dot(Vec3::from(t.direction).normalize_or(Vec3::Y)),
        Source::Height => {
            let [lo, hi] = t.range;
            if hi == lo { if at.p.y >= lo { 1.0 } else { 0.0 } } else { (at.p.y - lo) / (hi - lo) }
        }
        Source::Streaks => {
            let axis = Vec3::from(t.direction).normalize_or(Vec3::Y);
            let p = at.p - axis * at.p.dot(axis) * 0.96;
            fbm(p * t.freq() + t.offset(), t.octaves)
        },
        Source::Noise => fbm(at.p * t.freq() + t.offset(), t.octaves),
        Source::Grunge => t.grunge().map_or(0.5, |(map, freq)| grunge_height(map, freq, &At { p: at.p + t.offset() * 0.01, ..*at })),
        Source::Painted => at.painted.get(t.slot as usize).copied().unwrap_or(0.0),
    }
    .clamp(0.0, 1.0);
    let v = if t.invert { 1.0 - v } else { v };
    let (th, c) = levels_of(t.threshold, t.contrast);
    levels(v, th, c) * t.amount
}

fn fold(acc: f32, v: f32, op: Op) -> f32 {
    match op {
        Op::Multiply => acc * v,
        Op::Max => acc.max(v),
        Op::Min => acc.min(v),
        Op::Add => acc + v,
        Op::Subtract => acc - v,
        Op::Screen => 1.0 - (1.0 - acc) * (1.0 - v),
    }
}

/// A layer's mask settings after its role's amount: (opacity, threshold,
/// contrast, power for masks without levels).
pub fn effective(m: &Material, l: &Layer) -> [f32; 4] {
    let a = match l.role.as_str() {
        "wear" => m.wear,
        "rust" => m.rust,
        "dirt" => (m.dirt >= 0.0).then_some(m.dirt),
        _ => None,
    }
    .unwrap_or(0.5)
    .clamp(0.0, 1.0);
    let (t, c) = levels_of(l.threshold, l.contrast);
    let k = if c > 0.0 { 1.0 } else { ((1.0 - a) / a.max(0.01)).powf(1.5).clamp(0.15, 8.0) };
    let t = if c > 0.0 { t + (0.5 - a) * 0.6 } else { t };
    [l.opacity.clamp(0.0, 1.0) * (2.0 * a).min(1.0), t, c, k]
}

/// How much of layer `l` shows at `at` (0..1, before opacity).
pub fn mask(m: &Material, l: &Layer, at: &At) -> f32 {
    let [_, t, c, k] = effective(m, l);
    let mut v = 1.0;
    for (i, term_) in l.mask.iter().enumerate() {
        let x = term(term_, at);
        v = if i == 0 { x } else { fold(v, x, term_.op) };
    }
    let mut v = v.clamp(0.0, 1.0);
    if l.invert {
        v = 1.0 - v;
    }
    if c > 0.0 { levels(v, t, c) } else { v.powf(k) }
}

/// Lays `m`'s layers over `look`.
pub fn layers(m: &Material, at: &At, mut look: Look) -> Look {
    for l in &m.layers {
        let w = mask(m, l, at) * effective(m, l)[0];
        if w <= 0.0 {
            continue;
        }
        let mut c = l.color.map_or(look.color, Vec3::from);
        if let Some(t) = l.tint {
            c *= Vec3::from(t);
        }
        if l.variation > 0.0 {
            let s = if l.variation_scale > 0.0 { l.variation_scale } else { VARIATION_SCALE };
            let v = noise(at.p / s + Vec3::splat(31.7)) * 0.65 + noise(at.p / s * 2.7 + Vec3::splat(7.3)) * 0.35 - 0.5;
            c *= 1.0 + v * 0.8 * l.variation;
        }
        let mut rough = l.roughness.unwrap_or(look.rough);
        let mut n = look.n;
        if let Some(d) = layer_detail(l) {
            (n, c, rough) = d.apply(at.p, look.n, c, rough, at.footprint);
        }
        look.color = look.color.lerp(c.clamp(Vec3::ZERO, Vec3::ONE), w);
        look.rough = look.rough + (rough - look.rough) * w;
        look.metal += (l.metallic.unwrap_or(look.metal) - look.metal) * w;
        look.coat += (l.clearcoat.unwrap_or(look.coat) - look.coat) * w;
        look.sheen += (l.sheen.unwrap_or(look.sheen) - look.sheen) * w;
        look.emission += (l.emission.unwrap_or(look.emission) - look.emission) * w;
        look.n = look.n.lerp(n, w).normalize_or(look.n);
    }
    look.color = look.color.clamp(Vec3::ZERO, Vec3::ONE);
    look.rough = look.rough.clamp(0.03, 1.0);
    look
}

/// A layer's detail map, resolved.
pub fn layer_detail(l: &Layer) -> Option<crate::detail::Params> {
    crate::detail::Params::named(l.detail.as_deref()?, l.detail_scale, l.detail_strength, l.detail_color, l.detail_rotate)
}

// ---------------------------------------------------------------- decals

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Rune,
    Scratch,
    #[serde(alias = "blood", alias = "splatter")]
    Splat,
    Emblem,
    Image,
    BulletHit,
    Dent,
    Scorch,
    Blast,
    Gouge,
    Scrape,
    RustStreak,
    OilLeak,
    WaterStain,
    Bend,
    Tear,
}

/// A decal stamped onto clay: projected along `normal` onto the surface
/// around `center` (object space), `size` across, turned by `rotate`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Decal {
    /// Owning wear preset for generated runoff; explicit stamps leave empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wear_preset: String,
    pub shape: Shape,
    /// A PNG file (shape image): its color and alpha.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub center: [f32; 3],
    /// The way the decal faces (out of the surface).
    pub normal: [f32; 3],
    pub size: f32,
    /// Length / width of a damage segment (along v).
    #[serde(default = "one")]
    pub aspect: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hinge: Option<[[f32;3];2]>,
    #[serde(default)]
    pub angle: f32,
    #[serde(default)]
    pub melted_lip: bool,
    /// Runoff edge diffusion, 0.1 (sharp) to 2 (soft).
    pub softness: f32,
    /// Global progress of a leak segment, from its root to its tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<[f32;2]>,
    /// Degrees.
    #[serde(skip_serializing_if = "is_zero")]
    pub rotate: f32,
    /// sRGB. None: splats are blood red, scratches bare bright metal,
    /// runes dark grooves, the rest keep the color below (relief only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    #[serde(skip_serializing_if = "is_zero")]
    pub emission: f32,
    /// Relief: > 0 raised, < 0 cut in (None = the shape's own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    pub opacity: f32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub seed: u32,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

impl Default for Decal {
    fn default() -> Self {
        Decal { wear_preset: String::new(), shape: Shape::Rune, image: None, center: [0.0; 3], normal: [0.0, 0.0, 1.0], size: 0.1, aspect: 1.0, hinge: None, angle: 30.0, melted_lip: false, softness: 1.0, flow: None, rotate: 0.0, color: None, roughness: None, metallic: None, emission: 0.0, depth: None, opacity: 1.0, seed: 0 }
    }
}

impl Shape {
    pub fn code(self) -> u32 {
        self as u32
    }
}

impl Decal {
    pub fn sanitize(&mut self) {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        self.center = self.center.map(|c| fin(c, 0.0));
        let n = Vec3::from(self.normal.map(|c| fin(c, 0.0))).normalize_or(Vec3::Z);
        self.normal = n.to_array();
        self.size = fin(self.size, 0.1).clamp(1e-4, 100.0);
        self.softness=fin(self.softness,1.0).clamp(0.1,2.0);
        self.flow=self.flow.map(|r|r.map(|v|fin(v,0.0).clamp(0.0,1.0)));
        self.angle=fin(self.angle,30.0).clamp(-85.0,85.0);
        self.hinge=self.hinge.filter(|h|h.iter().flatten().all(|v|v.is_finite()) && Vec3::from(h[0]).distance(Vec3::from(h[1]))>1e-5);
        self.aspect = fin(self.aspect, 1.0).clamp(0.1, 100.0);
        self.rotate = fin(self.rotate, 0.0).rem_euclid(360.0);
        self.color = self.color.map(|c| c.map(|v| fin(v, 0.5).clamp(0.0, 1.0)));
        self.roughness = self.roughness.map(|v| fin(v, 0.5).clamp(0.0, 1.0));
        self.metallic = self.metallic.map(|v| fin(v, 0.0).clamp(0.0, 1.0));
        self.emission = fin(self.emission, 0.0).clamp(0.0, crate::material::MAX_EMISSION);
        self.depth = self.depth.map(|v| fin(v, 0.0).clamp(-4.0, 4.0));
        self.opacity = fin(self.opacity, 1.0).clamp(0.0, 1.0);
        self.image = self.image.take().map(|s| s.trim().chars().take(512).collect());
    }

    /// The relief strength (the shape's default when unset).
    pub fn relief(&self) -> f32 {
        if self.shape.code() >= 5 {
            let scale=if self.shape==Shape::Scorch {1.0}else if matches!(self.shape, Shape::Gouge | Shape::Scrape) {0.035}else if self.hinge.is_some(){0.0}else{0.15};
            return crate::damage::depth(self) / self.size / 0.01 * scale;
        }
        self.depth.unwrap_or(match self.shape {
            Shape::Rune => -1.0,
            Shape::Scratch => -0.7,
            Shape::Splat => 0.25,
            Shape::Emblem => 0.9,
            Shape::Image => 0.0,
            _ => 0.0,
        })
    }

    /// Decal space: (along u, along v), each scaled by 1 / size.
    pub fn frame(&self) -> (Vec3, Vec3) {
        let n = Vec3::from(self.normal).normalize_or(Vec3::Z);
        // u runs level (across world up) unless the decal faces up or down.
        let side = if n.y.abs() > 0.95 { Vec3::X } else { Vec3::Y.cross(n).normalize() };
        let up = n.cross(side);
        let a = self.rotate.to_radians();
        let (c, s) = (a.cos(), a.sin());
        ((side * c + up * s) / self.size, (up * c - side * s) / (self.size * self.aspect))
    }

    /// The image (shape image), decoded and cached.
    pub fn picture(&self) -> Option<Arc<Picture>> {
        if self.shape != Shape::Image {
            return None;
        }
        picture(self.image.as_deref()?)
    }
}

/// A decal image: square RGBA8 (`PICTURE` texels across), rows top down.
pub struct Picture {
    pub size: u32,
    pub rgba: Vec<u8>,
}

const PICTURE: u32 = 256;

/// Most decal images kept decoded at once.
const MAX_PICTURES: usize = 64;

/// Loads (once) a PNG or JPEG for decals; None when it cannot be read.
/// Paths come from agents and scene files, so only an absolute path to a
/// regular .png/.jpg/.jpeg file is read, with the same decode limits as
/// reference images.
pub fn picture(path: &str) -> Option<Arc<Picture>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Picture>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(p) = cache.lock().ok()?.get(path) {
        return p.clone();
    }
    let loaded = load_picture(path).map_err(|e| eprintln!("decal image {path}: {e}")).ok().map(Arc::new);
    let mut cache = cache.lock().ok()?;
    if cache.len() >= MAX_PICTURES {
        cache.clear();
    }
    cache.insert(path.to_string(), loaded.clone());
    loaded
}

fn load_picture(path: &str) -> Result<Picture, String> {
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg")) {
        return Err("path must end in .png, .jpg or .jpeg".into());
    }
    if !path.is_file() {
        return Err("not a file".into());
    }
    let mut reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?;
    let img = image::imageops::resize(&img.to_rgba8(), PICTURE, PICTURE, image::imageops::FilterType::Triangle);
    Ok(Picture { size: PICTURE, rgba: img.into_raw() })
}

/// Bilinear clamped read, 0..1 per channel (as `film::sample`).
pub fn picture_sample(p: &Picture, uv: Vec2) -> [f32; 4] {
    let s = p.size as f32;
    let x = (uv.x * s - 0.5).clamp(0.0, s - 1.0);
    let y = (uv.y * s - 0.5).clamp(0.0, s - 1.0);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let size = p.size as usize;
    let (x1, y1) = ((x0 + 1).min(size - 1), (y0 + 1).min(size - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize, c: usize| p.rgba[(y * size + x) * 4 + c] as f32 / 255.0;
    [0, 1, 2, 3].map(|c| {
        let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
        let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
        top * (1.0 - fy) + bottom * fy
    })
}

/// Integer hash shared with `trace.wgsl` (`smart_hash`).
pub fn hash(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// A repeatable random number 0..1 from a seed and an index.
pub fn rand(seed: u32, i: u32) -> f32 {
    (hash(seed.wrapping_mul(0x9E37_79B9) ^ i.wrapping_mul(0x85EB_CA6B).wrapping_add(0x632B_E5AB)) >> 8) as f32 / 16_777_216.0
}

fn seg(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.dot(ab).max(1e-12)).clamp(0.0, 1.0);
    (p - a - ab * t).length()
}

fn rune_point(k: u32) -> Vec2 {
    Vec2::new(((k % 3) as f32 - 1.0) * 0.22, ((k / 3) as f32 - 1.5) * 0.24)
}

/// A procedural shape's coverage at `uv` (decal units, -0.5..0.5).
pub fn shape(code: u32, seed: u32, uv: Vec2) -> f32 {
    match code {
        // Rune: a stave with branches, sometimes in a ring.
        0 => {
            let mut d = seg(uv, Vec2::new(0.0, -0.36), Vec2::new(0.0, 0.36));
            let branches = 2 + (rand(seed, 0) * 3.0) as u32;
            for i in 0..4u32 {
                if i < branches {
                    let row = ((rand(seed, 1 + i) * 4.0) as u32).min(3);
                    let side = if rand(seed, 10 + i) < 0.5 { 0 } else { 2 };
                    let to_row = (row as i32 + (rand(seed, 20 + i) * 3.0) as i32 - 1).clamp(0, 3) as u32;
                    d = d.min(seg(uv, rune_point(1 + 3 * row), rune_point(side + 3 * to_row)));
                }
            }
            if rand(seed, 30) > 0.45 {
                d = d.min((uv.length() - 0.46).abs());
            }
            1.0 - smooth(0.018, 0.032, d)
        }
        // Scratches: thin tapered strokes, mostly along u.
        1 => {
            let mut a = 0.0f32;
            for i in 0..14u32 {
                let c = Vec2::new(rand(seed, i) - 0.5, rand(seed, 20 + i) - 0.5) * 0.75;
                let ang = (rand(seed, 40 + i) - 0.5) * 0.6;
                let half = 0.06 + rand(seed, 60 + i) * 0.3;
                let w = 0.004 + rand(seed, 80 + i) * 0.008;
                let dir = Vec2::new(ang.cos(), ang.sin());
                let (p0, p1) = (c - dir * half, c + dir * half);
                let t = ((uv - p0).dot(dir) / (2.0 * half)).clamp(0.0, 1.0);
                let ww = w * (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0).sqrt();
                let cov = 1.0 - smooth(ww * 0.4, ww.max(1e-4), seg(uv, p0, p1));
                a = a.max(cov * (0.6 + 0.4 * rand(seed, 100 + i)));
            }
            a
        }
        // Splat: a wobbly blot with droplets thrown around it.
        2 => {
            let r = uv.length();
            let th = uv.y.atan2(uv.x);
            let ph = |i: u32| rand(seed, i) * TAU;
            let rim = 0.25 + 0.05 * (3.0 * th + ph(0)).sin() + 0.035 * (5.0 * th + ph(1)).sin() + 0.02 * (9.0 * th + ph(2)).sin() + 0.012 * (15.0 * th + ph(3)).sin();
            let mut a = 1.0 - smooth(rim - 0.012, rim + 0.012, r);
            for i in 0..12u32 {
                let ang = rand(seed, 10 + i) * TAU;
                let dist = 0.29 + rand(seed, 30 + i) * 0.19;
                let rad = (0.008 + rand(seed, 50 + i) * 0.028) * (1.25 - (dist - 0.29) / 0.19 * 0.75);
                let d = (uv - Vec2::new(ang.cos(), ang.sin()) * dist).length();
                a = a.max(1.0 - smooth(rad - 0.005, rad + 0.005, d));
            }
            a
        }
        // Emblem: a ring around a star.
        3 => {
            let r = uv.length();
            let th = uv.y.atan2(uv.x);
            let points = 4 + (rand(seed, 0) * 4.0) as u32;
            let f = (th + rand(seed, 1) * TAU) * points as f32 / TAU;
            let tri = ((f - f.floor()) - 0.5).abs() * 2.0;
            let star_r = 0.14 + 0.2 * tri * tri;
            let star = 1.0 - smooth(star_r - 0.01, star_r + 0.01, r);
            let ring = 1.0 - smooth(0.018, 0.03, (r - 0.43).abs());
            ring.max(star)
        }
        _ => 0.0,
    }
}

/// A decal's (coverage, relief height, color from its image) at `uv`.
fn decal_at(d: &Decal, pic: Option<&Picture>, uv: Vec2) -> (f32, f32, Option<Vec3>) {
    match pic {
        Some(p) => {
            let c = picture_sample(p, Vec2::new(uv.x + 0.5, 0.5 - uv.y));
            let rgb = Vec3::new(c[0], c[1], c[2]);
            (c[3], c[3] * rgb.dot(Vec3::new(0.3, 0.59, 0.11)), Some(rgb))
        }
        None if d.shape == Shape::Image => (0.0, 0.0, None),
        None if d.shape.code() >= 5 => {
            let hit = crate::damage::sample_decal(d, uv);
            (hit.coverage, hit.height, Some(hit.color))
        }
        None => {
            let a = shape(d.shape.code(), d.seed, uv);
            (a, a, None)
        }
    }
}

/// Lays `list` over `look` at `at`.
pub fn decals(list: &[Decal], at: &At, mut look: Look) -> Look {
    for d in list {
        let nrm = Vec3::from(d.normal);
        let rel = at.p - Vec3::from(d.center);
        if rel.dot(nrm).abs() > d.size * 0.5 {
            continue;
        }
        let facing = at.n.dot(nrm);
        if facing < 0.15 {
            continue;
        }
        let (tu, tv) = d.frame();
        let uv = Vec2::new(rel.dot(tu), rel.dot(tv));
        if uv.x.abs() > 0.5 || uv.y.abs() > 0.5 {
            continue;
        }
        let pic = d.picture();
        let fade = smooth(0.15, 0.4, facing) * d.opacity;
        let (a, _, img) = decal_at(d, pic.as_deref(), uv);
        let a = a * fade;
        let relief = d.relief();
        if relief != 0.0 {
            let e = 0.003;
            let h = |o: Vec2| decal_at(d, pic.as_deref(), uv + o).1;
            let hx = (h(Vec2::new(e, 0.0)) - h(Vec2::new(-e, 0.0))) / (2.0 * e);
            let hy = (h(Vec2::new(0.0, e)) - h(Vec2::new(0.0, -e))) / (2.0 * e);
            // Slope per scene unit, along the surface.
            let bend = -(tu * d.size * hx + tv * d.size * hy) * relief * 0.01 * fade;
            let bend = bend - look.n * look.n.dot(bend);
            look.n = (look.n + bend).normalize_or(look.n);
        }
        if a <= 0.0 {
            continue;
        }
        let color = match (d.color, img, d.shape) {
            (Some(c), _, _) => Some(Vec3::from(c)),
            (None, Some(c), _) => Some(c),
            (None, None, Shape::Splat) => Some(Vec3::new(0.28, 0.02, 0.02)),
            (None, None, Shape::Scratch) => Some((look.color * 1.6 + Vec3::splat(0.08)).min(Vec3::ONE)),
            (None, None, Shape::Rune) => Some(look.color * 0.3),
            _ => None,
        };
        if let Some(c) = color {
            look.color = look.color.lerp(c, a);
        }
        let damage = (d.shape.code() >= 5).then(|| crate::damage::sample_decal(d, uv));
        let rough = d.roughness.or(damage.map(|h| h.rough)).or(match d.shape {
            Shape::Splat => Some(0.2),
            Shape::Scratch => Some(0.25),
            _ => None,
        });
        if let Some(r) = rough {
            look.rough += (r - look.rough) * a;
        }
        if let Some(m) = d.metallic.or(damage.map(|h| h.metal)) {
            look.metal += (m - look.metal) * a;
        }
        if d.emission > 0.0 {
            look.emission += (d.emission - look.emission) * a;
        }
    }
    look.color = look.color.clamp(Vec3::ZERO, Vec3::ONE);
    look.rough = look.rough.clamp(0.03, 1.0);
    look
}

// ---------------------------------------------------------------- meshes

/// Whether a material or mesh needs smart shading.
pub fn layered(m: &Material) -> bool {
    !m.layers.is_empty()
}

/// Gives a freshly built clay mesh its mask inputs and per-vertex smart
/// look: weathering (`weather.rs`) first, then each vertex's base color,
/// edge, cavity, occlusion and painted channels, then the layers and
/// decals in its colors and surface values. `field` is the clay's field.
pub fn finish(clay: &crate::clay::Clay, field: &crate::clay::Field, mesh: &mut crate::mesh::Mesh) {
    let width = clay.wear_width();
    let any_layers = mesh.materials.iter().any(layered);
    let any_weather = mesh.materials.iter().any(Material::weathered);
    let wear = (any_layers || any_weather).then(|| crate::weather::surface_wear(mesh, clay, field));
    if any_weather && let Some(wear) = &wear {
        let mats: Vec<Material> = mesh.materials.iter().map(|m| crate::weather::with_scale(m, clay)).collect();
        for (v, w) in mesh.vertices.iter_mut().zip(wear) {
            let m = &mats[v.material as usize];
            if m.weathered() {
                let (c, r) = crate::weather::apply(m, Vec3::from(v.pos), Vec3::from(v.normal), Vec3::from(v.color), v.surface[0], *w, width);
                v.color = c.to_array();
                v.surface[0] = r;
            }
        }
    }
    let channels = channel_names(clay);
    for m in &mut mesh.materials {
        resolve_slots(m, &channels);
    }
    let occlusion = mesh.materials.iter().any(|m| m.layers.iter().any(|l| l.mask.iter().any(|t| t.source == Source::Ao)));
    let ao_step = field.bounds().map_or(0.01, |(lo, hi)| (hi - lo).length() * 0.004);
    let masks: Vec<&crate::paint::Paint> = channels.iter().filter_map(|c| clay.masks.get(c)).collect();
    let decals: Arc<Vec<Decal>> = Arc::new(clay.decals.iter().take(MAX_DECALS).cloned().collect());
    let footprint = clay.cell_size().unwrap_or(0.01);
    let materials = mesh.materials.clone();
    crate::clay::par_chunks(&mut mesh.vertices, 1024, |chunk, verts| {
        for (k, v) in verts.iter_mut().enumerate() {
            let i = chunk * 1024 + k;
            let p = Vec3::from(v.pos);
            let n = Vec3::from(v.normal);
            v.base = [v.color[0], v.color[1], v.color[2], v.surface[0]];
            let [edge, cavity] = wear.as_ref().map_or([0.0; 2], |w| w[i]);
            let occl = if occlusion { occlusion_at(field, p, n, ao_step) } else { 0.0 };
            v.wear = [edge, cavity, occl];
            let mut painted = [0.0f32; 4];
            for (slot, mask) in masks.iter().enumerate() {
                painted[slot] = mask.grid().coverage(p);
                v.painted[slot] = (painted[slot].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            let m = &materials[(v.material as usize).min(materials.len() - 1)];
            if !layered(m) && decals.is_empty() {
                continue;
            }
            let at = At { p, n, edge, cavity, occlusion: occl, painted, footprint };
            let look = Look { metal: v.surface[1], coat: v.surface[2], sheen: v.surface[3], emission: v.light[0], ..Look::of(m, Vec3::from(v.color), v.surface[0], n) };
            let look = decals_over(&decals, &at, layers(m, &at, look));
            v.color = look.color.to_array();
            v.surface = [look.rough, look.metal, look.coat, look.sheen];
            v.light[0] = look.emission;
        }
    });
    mesh.decals = decals;
}

/// Smart-material mask inputs and layers for a mesh that is not clay
/// (cloth garments, kit pieces): edge and cavity from the mesh's own
/// curvature over `width`, no occlusion, paint or decals. Weathering, if
/// any, must already be in the colors.
pub fn finish_mesh(mesh: &mut crate::mesh::Mesh, width: f32) {
    if mesh.vertices.is_empty() || mesh.materials.is_empty() && mesh.vertices.iter().all(|v| v.surface[0] < 0.0) {
        return;
    }
    let lo = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).fold(Vec3::splat(f32::INFINITY), Vec3::min);
    let hi = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
    for m in &mut mesh.materials { crate::damage::part_grime(m, lo, hi); }
    let any_layers = mesh.materials.iter().any(layered);
    let wear = crate::weather::wear_maps(mesh, width);
    let materials = mesh.materials.clone();
    crate::clay::par_chunks(&mut mesh.vertices, 1024, |chunk, verts| {
        for (k, v) in verts.iter_mut().enumerate() {
            let [edge, cavity] = wear[chunk * 1024 + k];
            if let Some(m) = materials.get((v.material as usize).min(materials.len().saturating_sub(1))) { v.color = m.base_color.unwrap_or(v.color); }
            v.base = [v.color[0], v.color[1], v.color[2], v.surface[0]];
            v.wear = [edge, cavity, 0.0];
            if !any_layers || v.surface[0] < 0.0 {
                continue;
            }
            let m = &materials[(v.material as usize).min(materials.len() - 1)];
            if !layered(m) {
                continue;
            }
            let (p, n) = (Vec3::from(v.pos), Vec3::from(v.normal));
            let at = At { p, n, edge, cavity, occlusion: 0.0, painted: [0.0; 4], footprint: width / 2.5 };
            let look = Look { metal: v.surface[1], coat: v.surface[2], sheen: v.surface[3], emission: v.light[0], ..Look::of(m, Vec3::from(v.color), v.surface[0], n) };
            let look = layers(m, &at, look);
            v.color = look.color.to_array();
            v.surface = [look.rough, look.metal, look.coat, look.sheen];
            v.light[0] = look.emission;
        }
    });
}

fn decals_over(list: &[Decal], at: &At, look: Look) -> Look {
    if list.is_empty() { look } else { decals(list, at, look) }
}

/// Scales raw occlusion so a groove floor reads near 1.
pub const OCCLUSION: f32 = 3.0;

/// How hidden `p` is (0 open .. 1 buried), from the field along the normal.
pub fn occlusion_at(field: &crate::clay::Field, p: Vec3, n: Vec3, step: f32) -> f32 {
    let list = field.near(p - Vec3::splat(step * 6.0), p + Vec3::splat(step * 6.0));
    let (mut occ, mut weight) = (0.0, 0.5);
    for i in 1..=5 {
        let h = step * i as f32;
        occ += weight * (h - field.sample(p + n * h, &list).0).max(0.0) / h;
        weight *= 0.5;
    }
    // A flat open surface reads 0; creases and covered parts climb to 1.
    (occ * OCCLUSION).clamp(0.0, 1.0)
}

/// The clay's painted mask channels, by slot (sorted by name).
pub fn channel_names(clay: &crate::clay::Clay) -> Vec<String> {
    clay.masks.keys().take(MAX_CHANNELS).cloned().collect()
}

/// Points `m`'s painted terms at their slots in `channels`.
pub fn resolve_slots(m: &mut Material, channels: &[String]) {
    for l in &mut m.layers {
        for t in &mut l.mask {
            if t.source == Source::Painted {
                t.slot = channels.iter().position(|c| *c == t.channel).map_or(NO_SLOT, |i| i as u8);
            }
        }
    }
}

// ---------------------------------------------------------------- presets

/// Splices `smart` references (one level of nesting) and caps the stack.
pub fn expand(layers: &[Layer], depth: u32) -> Vec<Layer> {
    let mut out = Vec::new();
    for l in layers {
        match &l.smart {
            Some(name) if depth < 3 => {
                if let Some(p) = preset(name) {
                    for mut inner in expand(&p.layers, depth + 1) {
                        inner.opacity *= l.opacity;
                        out.push(inner);
                    }
                }
            }
            Some(_) => {}
            None => out.push(l.clone()),
        }
    }
    out.truncate(MAX_LAYERS);
    out
}

/// (name, what it is for).
pub const PRESETS: [(&str, &str); 25] = [
    ("military_paint", "matte chalky military paint with fine relief and handling variation"),
    ("dusty_khaki", "desaturated sand military paint"),
    ("olive_drab", "matte olive military paint"),
    ("gunmetal", "forged dark metal"),
    ("worn_steel", "scratched steel with polished edges"),
    ("battle_worn", "paint chips, primer, bare metal, cavity grime and polished handling wear"),
    ("desert_dusty", "world-up settled dust, sun-faded paint and roughness variation"),
    ("muddy", "world-height mud accumulation and splatter"),
    ("burnt_wreck", "charred blistered paint and exposed metal"),
    ("factory_new", "no added wear"),
    ("worn_iron", "dark iron armor: polished worn edges, rust and grime in the crevices, scratches"),
    ("rusted_iron", "iron eaten by rust: orange bloom and crust, bare metal only on rubbed edges"),
    ("polished_steel", "bright steel blades and plate: brushed, smudged, dark in the seams"),
    ("gold_trim", "gold trims and ornaments: tarnish in the recesses, bright rubbed edges"),
    ("bronze", "bronze: brown metal, green verdigris in the creases, polished high points"),
    ("old_leather", "old brown leather straps, boots, belts: scuffed lighter edges, dark grime, oiled sheen"),
    ("cracked_leather", "dried cracked leather: a network of cracks, faded raised parts"),
    ("bone", "old bone and horn: yellowed, stained in the pits, polished ridges, fine cracks"),
    ("charred_flesh", "burnt flesh: black crust, glowing ember cracks, ash on top, raw red showing through"),
    ("wet_flesh", "raw wet flesh: deep red, darker veins in the folds, glossy wet film"),
    ("demon_skin", "demon hide: dark red leathery skin, black blotches, hardened ridges, faint inner glow in the deep creases"),
    ("dungeon_stone", "dungeon stone blocks: cold grey, dust on top, soot and damp in the cracks, chipped edges"),
    ("mossy_stone", "old stone with moss growing on the tops and in the cracks"),
    ("grimy_cloth", "worn homespun cloth: grime in the folds, stains, mud at the hem, frayed lighter edges"),
    ("blood_splatter", "a layer to add: fresh and dried blood spattered over anything ({\"smart\": \"blood_splatter\"} in layers)"),
];

fn t(source: Source) -> Term {
    Term { source, ..Term::default() }
}

fn lv(mut term: Term, threshold: f32, contrast: f32) -> Term {
    term.threshold = Some(threshold);
    term.contrast = Some(contrast);
    term
}

fn with(mut term: Term, op: Op, amount: f32) -> Term {
    term.op = op;
    term.amount = amount;
    term
}

fn noise_t(scale: f32, octaves: u32, seed: f32) -> Term {
    Term { source: Source::Noise, scale, octaves, seed, ..Term::default() }
}

fn grunge_t(map: &str, scale: f32) -> Term {
    Term { source: Source::Grunge, map: map.into(), scale, ..Term::default() }
}

fn height_t(lo: f32, hi: f32) -> Term {
    Term { source: Source::Height, range: [lo, hi], ..Term::default() }
}

/// A full-cover layer: the material's main look.
fn base(name: &str, color: [f32; 3], rough: f32, metal: f32) -> Layer {
    Layer { name: name.into(), color: Some(color), roughness: Some(rough), metallic: Some(metal), clearcoat: Some(0.0), sheen: Some(0.0), ..Layer::default() }
}

fn masked(name: &str, mask: Vec<Term>, threshold: f32, contrast: f32) -> Layer {
    Layer { name: name.into(), mask, threshold: Some(threshold), contrast: Some(contrast), ..Layer::default() }
}

fn detail(mut l: Layer, map: &str, scale: f32, strength: f32, color: f32) -> Layer {
    l.detail = Some(map.into());
    l.detail_scale = scale;
    l.detail_strength = strength;
    l.detail_color = color;
    l
}

fn look(mut l: Layer, color: Option<[f32; 3]>, rough: Option<f32>, metal: Option<f32>) -> Layer {
    l.color = color;
    l.roughness = rough;
    l.metallic = metal;
    l
}

fn role(mut l: Layer, role: &str) -> Layer {
    l.role = role.into();
    l
}

/// Shared pieces. Masks are broken up by grunge maps (`Map::pattern`) so
/// wear, rust and dirt come in ragged, uneven patches, never clean bands.
fn rust_layer(name: &str, color: [f32; 3], seed: f32) -> Layer {
    let l = masked(name, vec![lv(t(Source::Cavity), 0.15, 2.0), with(t(Source::Ao), Op::Max, 1.0), with(seeded(grunge_t("stone", 0.12), seed), Op::Multiply, 1.5), with(noise_t(0.25, 2, seed), Op::Add, 0.15)], 0.45, 4.0);
    let l = Layer { variation: 0.5, variation_scale: 0.03, ..look(l, Some(color), Some(0.92), Some(0.0)) };
    role(detail(l, "stone", 0.15, 1.6, 0.8), "rust")
}

fn grime_layer(color: [f32; 3], seed: f32) -> Layer {
    let l = masked("grime", vec![lv(t(Source::Ao), 0.1, 1.5), with(t(Source::Cavity), Op::Max, 1.0), with(t(Source::Down), Op::Add, 0.2), with(seeded(grunge_t("metal_brushed", 0.2), seed), Op::Multiply, 1.4)], 0.45, 3.0);
    role(Layer { opacity: 0.9, ..look(l, Some(color), Some(0.95), Some(0.0)) }, "dirt")
}

/// Worn edges: the convex edges, broken by `breakup` (a library map).
fn edge_layer(name: &str, color: Option<[f32; 3]>, rough: f32, metal: Option<f32>, breakup: &str, seed: f32) -> Layer {
    let threshold = if metal.is_some() { 0.55 } else { 0.72 };
    let l = masked(name, vec![lv(t(Source::Edges), 0.3, 2.0), with(seeded(grunge_t(breakup, 0.07), seed), Op::Multiply, 1.5)], threshold, 5.0);
    role(look(l, color, Some(rough), metal), "wear")
}

/// Soft patches from a grunge map: color variation over a whole surface.
fn patches(name: &str, map: &str, scale: f32, threshold: f32, seed: f32) -> Layer {
    masked(name, vec![seeded(grunge_t(map, scale), seed), with(noise_t(scale * 1.5, 2, seed), Op::Multiply, 1.4)], threshold, 1.5)
}

/// Thin dark lines: the cracks of a cellular map (wrinkles, scales).
fn cracks(name: &str, map: &str, scale: f32, threshold: f32) -> Layer {
    Layer { invert: true, ..masked(name, vec![grunge_t(map, scale)], 1.0 - threshold, 8.0) }
}

fn seeded(mut term: Term, seed: f32) -> Term {
    term.seed = seed;
    term
}

/// The smart preset called `name`, flattened.
pub fn preset(name: &str) -> Option<Material> {
    if matches!(name, "dusty_khaki" | "olive_drab" | "gunmetal" | "worn_steel") {
        let metal = matches!(name, "gunmetal" | "worn_steel");
        let mut layers = crate::damage::wear_layers("military_paint", 1.0).unwrap();
        // Finish presets retain the authored blob/object substrate. Recoloring
        // requires an explicit material base_color, before sparse markings.
        layers[0].metallic = Some(if metal {1.0} else {0.0});
        if metal {
            layers[0].roughness = Some(if name == "gunmetal" {0.55} else {0.45});
            layers[1].roughness = Some(0.30);
            layers[0].detail = Some("metal_scratched".into());
            layers[0].detail_scale = 0.18;
            layers[0].detail_strength = 0.3;
        }
        if name == "worn_steel" {
            layers.push(edge_layer("burnished steel", Some([0.62,0.64,0.65]), 0.24,
                Some(1.0), "metal_scratched", 17.0));
        }
        return Some(Material {name:name.into(), base_color:None, metallic:if metal {1.0}else{0.0}, layers, ..Material::default()});
    }
    if let Some(mut layers) = crate::damage::wear_layers(name, 1.0) {
        for l in &mut layers {l.name = format!("wear:{name}:{}",l.name);}
        return Some(Material { name: name.into(), layers, ..Material::default() });
    }
    let (layers, rough, metal): (Vec<Layer>, f32, f32) = match name {
        "worn_iron" => (
            vec![
                Layer { variation: 0.2, variation_scale: 0.15, ..detail(base("iron", [0.25, 0.245, 0.24], 0.42, 1.0), "metal_scratched", 0.2, 0.9, 0.6) },
                Layer { opacity: 0.8, ..look(patches("patina", "metal_scratched", 0.35, 0.5, 1.0), Some([0.14, 0.135, 0.13]), Some(0.62), Some(0.9)) },
                look(masked("smudges", vec![grunge_t("metal_brushed", 0.25)], 0.55, 2.0), None, Some(0.27), None),
                rust_layer("rust", [0.22, 0.10, 0.045], 2.0),
                role(look(masked("fresh rust", vec![lv(t(Source::Cavity), 0.25, 2.0), with(seeded(grunge_t("stone", 0.07), 3.0), Op::Multiply, 1.5)], 0.55, 5.0), Some([0.45, 0.19, 0.06]), Some(0.9), Some(0.0)), "rust"),
                grime_layer([0.07, 0.06, 0.05], 4.0),
                detail(edge_layer("worn edges", Some([0.58, 0.57, 0.55]), 0.2, Some(1.0), "metal_scratched", 5.0), "metal_brushed", 0.1, 0.4, 0.2),
            ],
            0.42,
            1.0,
        ),
        "rusted_iron" => (
            vec![
                Layer { variation: 0.2, ..detail(base("iron", [0.22, 0.21, 0.2], 0.5, 1.0), "metal_scratched", 0.25, 1.0, 0.6) },
                Layer { variation: 0.5, variation_scale: 0.05, ..role(detail(look(masked("rust bloom", vec![seeded(grunge_t("stone", 0.25), 6.0), with(noise_t(0.3, 2, 6.0), Op::Add, 0.3), with(t(Source::Edges), Op::Subtract, 0.5)], 0.5, 3.0), Some([0.33, 0.14, 0.05]), Some(0.88), Some(0.0)), "stone", 0.18, 1.5, 0.8), "rust") },
                rust_layer("deep rust", [0.17, 0.07, 0.035], 7.0),
                look(masked("orange crust", vec![seeded(grunge_t("metal_scratched", 0.1), 8.0), with(t(Source::Cavity), Op::Add, 0.3), with(t(Source::Edges), Op::Subtract, 0.4)], 0.62, 4.0), Some([0.55, 0.25, 0.07]), Some(0.95), Some(0.0)),
                look(cracks("pitting", "velvet", 0.05, 0.3), Some([0.08, 0.04, 0.025]), Some(0.95), Some(0.0)),
                grime_layer([0.1, 0.07, 0.05], 9.0),
                edge_layer("rubbed edges", Some([0.45, 0.43, 0.41]), 0.35, Some(1.0), "metal_scratched", 10.0),
            ],
            0.85,
            0.0,
        ),
        "polished_steel" => (
            vec![
                detail(base("steel", [0.68, 0.68, 0.70], 0.16, 1.0), "metal_brushed", 0.3, 0.35, 0.15),
                look(masked("smudges", vec![seeded(grunge_t("metal_brushed", 0.2), 11.0)], 0.55, 2.0), None, Some(0.3), None),
                Layer { opacity: 0.6, ..look(patches("dull patches", "metal_scratched", 0.3, 0.55, 12.0), Some([0.5, 0.5, 0.52]), Some(0.25), None) },
                grime_layer([0.12, 0.12, 0.13], 13.0),
                edge_layer("bright edges", Some([0.85, 0.85, 0.87]), 0.08, Some(1.0), "metal_scratched", 14.0),
            ],
            0.16,
            1.0,
        ),
        "gold_trim" => (
            vec![
                Layer { variation: 0.15, ..detail(base("gold", [0.85, 0.62, 0.25], 0.28, 1.0), "metal_brushed", 0.2, 0.4, 0.2) },
                Layer { opacity: 0.7, ..look(patches("rubbed dull", "metal_scratched", 0.25, 0.5, 15.0), Some([0.62, 0.43, 0.17]), Some(0.42), None) },
                role(look(masked("tarnish", vec![lv(t(Source::Cavity), 0.1, 2.0), with(t(Source::Ao), Op::Max, 1.0), with(seeded(grunge_t("metal_brushed", 0.15), 16.0), Op::Multiply, 1.5)], 0.4, 3.0), Some([0.2, 0.13, 0.05]), Some(0.55), Some(0.5)), "dirt"),
                edge_layer("polished", Some([1.0, 0.82, 0.45]), 0.1, Some(1.0), "metal_scratched", 17.0),
            ],
            0.28,
            1.0,
        ),
        "bronze" => (
            vec![
                Layer { variation: 0.2, ..detail(base("bronze", [0.48, 0.32, 0.17], 0.4, 1.0), "metal_scratched", 0.3, 0.7, 0.5) },
                Layer { opacity: 0.8, ..look(patches("dark patina", "metal_scratched", 0.3, 0.5, 18.0), Some([0.27, 0.18, 0.1]), Some(0.55), None) },
                rust_layer("verdigris", [0.24, 0.43, 0.35], 19.0),
                grime_layer([0.09, 0.07, 0.05], 20.0),
                edge_layer("polished", Some([0.82, 0.58, 0.34]), 0.18, Some(1.0), "metal_scratched", 21.0),
            ],
            0.4,
            1.0,
        ),
        "old_leather" => (
            vec![
                Layer { variation: 0.35, variation_scale: 0.1, ..detail(base("leather", [0.27, 0.16, 0.09], 0.6, 0.0), "leather_rough", 0.3, 1.1, 0.6) },
                Layer { clearcoat: Some(0.12), ..look(patches("oiled", "leather_rough", 0.25, 0.55, 22.0), Some([0.16, 0.085, 0.045]), Some(0.4), None) },
                role(look(masked("dried", vec![lv(t(Source::Up), 0.2, 1.5), with(t(Source::Edges), Op::Max, 0.8), with(seeded(grunge_t("metal_scratched", 0.15), 23.0), Op::Multiply, 1.5)], 0.6, 3.0), Some([0.34, 0.23, 0.15]), Some(0.8), None), "wear"),
                grime_layer([0.07, 0.05, 0.035], 24.0),
                detail(Layer { opacity: 0.8, ..edge_layer("scuffed edges", Some([0.42, 0.3, 0.2]), 0.85, None, "velvet", 25.0) }, "suede", 0.12, 0.6, 0.4),
            ],
            0.6,
            0.0,
        ),
        "cracked_leather" => (
            vec![
                Layer { variation: 0.35, ..detail(base("leather", [0.25, 0.15, 0.085], 0.7, 0.0), "leather_grain", 0.12, 1.1, 0.5) },
                role(look(masked("faded", vec![lv(t(Source::Up), 0.2, 1.5), with(t(Source::Edges), Op::Max, 1.0), with(seeded(grunge_t("metal_scratched", 0.18), 26.0), Op::Multiply, 1.5)], 0.45, 2.5), Some([0.43, 0.31, 0.2]), Some(0.85), None), "wear"),
                detail(look(cracks("cracks", "wrinkles", 0.08, 0.25), Some([0.05, 0.035, 0.025]), Some(0.95), None), "wrinkles", 0.08, 1.8, 0.3),
                grime_layer([0.07, 0.045, 0.03], 27.0),
            ],
            0.7,
            0.0,
        ),
        "bone" => (
            vec![
                Layer { variation: 0.3, variation_scale: 0.05, ..detail(base("bone", [0.72, 0.66, 0.53], 0.55, 0.0), "stone", 0.1, 0.5, 0.35) },
                look(masked("yellowed", vec![seeded(grunge_t("leather_rough", 0.2), 28.0), with(t(Source::Cavity), Op::Add, 0.4)], 0.5, 2.0), Some([0.58, 0.48, 0.3]), Some(0.6), None),
                role(look(masked("stains", vec![lv(t(Source::Cavity), 0.1, 2.0), with(t(Source::Ao), Op::Max, 1.0), with(seeded(grunge_t("metal_scratched", 0.1), 29.0), Op::Multiply, 1.5)], 0.4, 3.0), Some([0.3, 0.21, 0.11]), Some(0.75), None), "dirt"),
                detail(Layer { opacity: 0.8, ..look(cracks("hairline cracks", "wrinkles", 0.07, 0.18), Some([0.22, 0.17, 0.1]), Some(0.8), None) }, "wrinkles", 0.07, 0.8, 0.2),
                edge_layer("polished", Some([0.86, 0.82, 0.72]), 0.35, None, "velvet", 30.0),
            ],
            0.55,
            0.0,
        ),
        "charred_flesh" => (
            vec![
                Layer { variation: 0.4, variation_scale: 0.05, ..detail(base("char", [0.05, 0.04, 0.035], 0.85, 0.0), "stone", 0.15, 1.8, 0.7) },
                detail(look(cracks("raw cracks", "wrinkles", 0.06, 0.35), Some([0.3, 0.05, 0.035]), Some(0.5), None), "wrinkles", 0.06, 1.5, 0.3),
                Layer { emission: Some(3.0), ..look(masked("embers", vec![Term { invert: true, ..grunge_t("wrinkles", 0.06) }, with(lv(t(Source::Cavity), 0.05, 2.0), Op::Multiply, 1.0), with(noise_t(0.15, 2, 31.0), Op::Multiply, 1.6)], 0.5, 8.0), Some([1.0, 0.3, 0.04]), Some(0.9), None) },
                role(look(masked("ash", vec![lv(t(Source::Up), 0.35, 1.5), with(seeded(grunge_t("velvet", 0.05), 32.0), Op::Multiply, 1.5)], 0.5, 3.0), Some([0.33, 0.31, 0.29]), Some(1.0), None), "dirt"),
            ],
            0.85,
            0.0,
        ),
        "wet_flesh" => (
            vec![
                Layer { variation: 0.4, variation_scale: 0.05, ..detail(base("flesh", [0.38, 0.06, 0.05], 0.3, 0.0), "wrinkles", 0.08, 1.2, 0.5) },
                look(cracks("veins", "scales", 0.08, 0.2), Some([0.2, 0.02, 0.03]), Some(0.35), None),
                look(masked("dark folds", vec![lv(t(Source::Cavity), 0.1, 2.0), with(t(Source::Ao), Op::Max, 1.0)], 0.4, 2.5), Some([0.14, 0.02, 0.02]), Some(0.3), None),
                look(masked("pale membrane", vec![lv(t(Source::Edges), 0.2, 1.5), with(seeded(grunge_t("leather_rough", 0.12), 33.0), Op::Multiply, 1.5)], 0.5, 3.0), Some([0.55, 0.3, 0.25]), Some(0.3), None),
                Layer { clearcoat: Some(1.0), ..look(masked("wet film", vec![seeded(grunge_t("metal_brushed", 0.15), 34.0), with(t(Source::Cavity), Op::Add, 0.4)], 0.42, 2.0), None, Some(0.06), None) },
            ],
            0.3,
            0.0,
        ),
        "demon_skin" => (
            vec![
                Layer { variation: 0.4, variation_scale: 0.12, ..detail(base("hide", [0.24, 0.07, 0.055], 0.55, 0.0), "leather_grain", 0.08, 1.3, 0.55) },
                look(patches("black markings", "leather_rough", 0.4, 0.6, 35.0), Some([0.05, 0.02, 0.02]), Some(0.5), None),
                look(masked("creases", vec![lv(t(Source::Cavity), 0.1, 2.0), with(t(Source::Ao), Op::Max, 1.0)], 0.4, 2.5), Some([0.04, 0.012, 0.012]), Some(0.7), None),
                role(detail(look(masked("hardened ridges", vec![lv(t(Source::Edges), 0.25, 1.5), with(seeded(grunge_t("metal_scratched", 0.1), 36.0), Op::Multiply, 1.5)], 0.55, 4.0), Some([0.32, 0.15, 0.1]), Some(0.38), None), "scales", 0.12, 1.0, 0.5), "wear"),
                Layer { emission: Some(2.0), ..look(masked("inner glow", vec![lv(t(Source::Cavity), 0.5, 3.0), with(Term { invert: true, ..grunge_t("wrinkles", 0.08) }, Op::Multiply, 1.0)], 0.68, 8.0), Some([1.0, 0.25, 0.04]), Some(0.6), None) },
                look(masked("sweat", vec![seeded(grunge_t("metal_brushed", 0.15), 37.0)], 0.62, 3.0), None, Some(0.18), None),
            ],
            0.55,
            0.0,
        ),
        "dungeon_stone" | "mossy_stone" => {
            let mut v = vec![
                Layer { variation: 0.4, variation_scale: 0.25, ..detail(base("stone", [0.36, 0.34, 0.31], 0.85, 0.0), "stone", 0.5, 1.4, 0.8) },
                look(patches("cold patches", "stone", 0.6, 0.55, 38.0), Some([0.29, 0.29, 0.29]), None, None),
                Layer { tint: Some([0.55, 0.55, 0.55]), ..look(masked("damp", vec![lv(t(Source::Cavity), 0.1, 2.0), with(t(Source::Ao), Op::Max, 1.0), with(height_t(0.5, 0.0), Op::Add, 0.4)], 0.45, 2.0), None, Some(0.35), None) },
                role(look(masked("soot", vec![lv(t(Source::Ao), 0.1, 1.5), with(t(Source::Down), Op::Max, 0.7), with(seeded(grunge_t("metal_brushed", 0.3), 39.0), Op::Multiply, 1.4)], 0.45, 2.5), Some([0.05, 0.045, 0.04]), Some(0.95), None), "dirt"),
                role(look(masked("dust", vec![lv(t(Source::Up), 0.5, 2.0), with(seeded(grunge_t("velvet", 0.06), 40.0), Op::Multiply, 1.4)], 0.5, 3.0), Some([0.5, 0.47, 0.42]), Some(1.0), None), "dirt"),
                edge_layer("chipped edges", Some([0.44, 0.42, 0.39]), 0.9, None, "stone", 41.0),
            ];
            if name == "mossy_stone" {
                let moss = Layer { sheen: Some(0.5), variation: 0.5, variation_scale: 0.04, ..look(masked("moss", vec![lv(t(Source::Up), 0.25, 1.5), with(t(Source::Cavity), Op::Add, 0.6), with(seeded(grunge_t("stone", 0.15), 42.0), Op::Multiply, 1.6)], 0.6, 4.0), Some([0.15, 0.2, 0.07]), Some(0.95), None) };
                v.push(role(detail(moss, "felt", 0.08, 1.4, 0.5), "rust"));
            }
            (v, 0.85, 0.0)
        }
        "grimy_cloth" => (
            vec![
                Layer { variation: 0.3, variation_scale: 0.1, sheen: Some(0.45), ..detail(base("cloth", [0.36, 0.31, 0.24], 0.9, 0.0), "linen", 0.06, 1.3, 0.5) },
                Layer { opacity: 0.7, ..look(patches("stains", "metal_scratched", 0.12, 0.6, 43.0), Some([0.27, 0.22, 0.15]), None, None) },
                Layer { opacity: 0.7, ..grime_layer([0.13, 0.1, 0.07], 44.0) },
                role(look(masked("mud", vec![height_t(0.5, 0.05), with(seeded(grunge_t("stone", 0.1), 45.0), Op::Multiply, 1.5)], 0.5, 4.0), Some([0.16, 0.11, 0.07]), Some(0.9), None), "dirt"),
                Layer { opacity: 0.6, ..edge_layer("frayed edges", Some([0.46, 0.41, 0.33]), 0.95, None, "velvet", 46.0) },
            ],
            0.9,
            0.0,
        ),
        "blood_splatter" => (
            vec![
                look(masked("dried blood", vec![seeded(grunge_t("velvet", 0.05), 47.0), with(noise_t(0.25, 2, 48.0), Op::Multiply, 1.6), with(seeded(grunge_t("stone", 0.1), 47.0), Op::Max, 0.9)], 0.55, 6.0), Some([0.14, 0.03, 0.02]), Some(0.55), Some(0.0)),
                Layer { clearcoat: Some(0.8), ..look(masked("fresh blood", vec![seeded(grunge_t("velvet", 0.04), 49.0), with(noise_t(0.25, 2, 48.0), Op::Multiply, 1.6), with(seeded(grunge_t("stone", 0.08), 49.0), Op::Max, 0.9)], 0.62, 8.0), Some([0.28, 0.01, 0.01]), Some(0.1), Some(0.0)) },
            ],
            0.55,
            0.0,
        ),
        _ => return None,
    };
    let mut m = Material { name: name.to_string(), roughness: rough, metallic: metal, layers, ..Material::default() };
    m.detail = Some(String::new());
    match name {
        "wet_flesh" => {
            m.subsurface = 0.6;
            m.scatter = [0.006, 0.0018, 0.001];
            m.clearcoat = 0.3;
        }
        "demon_skin" => {
            m.subsurface = 0.3;
            m.scatter = [0.004, 0.0012, 0.0008];
        }
        "rusted_iron" => m.rust = Some(0.75),
        "polished_steel" => m.dirt = 0.3,
        _ => {}
    }
    Some(m)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn at(edge: f32, cavity: f32) -> At {
        At { p: Vec3::new(0.11, 0.37, 0.23), n: Vec3::Z, edge, cavity, occlusion: cavity * 0.8, painted: [0.0; 4], footprint: 0.0 }
    }

    /// Meshes that are not clay (cloth, kit pieces) get edges and
    /// creases measured and their smart layers applied.
    #[test]
    fn plain_meshes_get_masks_and_layers() {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "box", "center": [0, 0, 0], "size": [0.4, 0.4, 0.4], "blend": 0.0, "color": [0.5, 0.5, 0.5] }], "resolution": 40
        })).unwrap();
        let mut mesh = clay.build_mesh();
        let m = Material::preset("worn_iron").unwrap();
        assert!(layered(&m));
        mesh.materials = vec![m.clone()];
        for v in &mut mesh.vertices {
            v.material = 0;
            v.surface = m.surface();
            v.color = [0.5; 3];
            v.wear = [0.0; 3];
        }
        let before: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| v.color).collect();
        finish_mesh(&mut mesh, 0.03);
        assert!(mesh.vertices.iter().any(|v| v.wear[0] > 0.3), "no edges found");
        let changed = mesh.vertices.iter().zip(&before).filter(|(v, b)| v.color != **b).count();
        assert!(changed > mesh.vertices.len() / 2, "layers changed {changed} colors");
    }

    #[test]
    fn terms_parse_from_names_and_objects_and_round_trip() {
        let l: Layer = serde_json::from_value(serde_json::json!({
            "color": [1, 0, 0], "mask": ["edges", {"source": "noise", "scale": 0.05, "op": "max"}, "crevices"], "threshold": 0.4
        }))
        .unwrap();
        assert_eq!(l.mask[0].source, Source::Edges);
        assert_eq!((l.mask[1].source, l.mask[1].op, l.mask[1].scale), (Source::Noise, Op::Max, 0.05));
        assert_eq!(l.mask[2].source, Source::Cavity);
        let back: Layer = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        assert!(serde_json::from_value::<Layer>(serde_json::json!({"mask": ["nope"]})).is_err());
    }

    #[test]
    fn masks_pick_edges_and_cavities() {
        let edge_layer = Layer { mask: vec![t(Source::Edges)], threshold: Some(0.5), contrast: Some(4.0), ..Layer::default() };
        let cavity_layer = Layer { mask: vec![t(Source::Cavity)], threshold: Some(0.5), contrast: Some(4.0), ..Layer::default() };
        let m = Material::default();
        assert!(mask(&m, &edge_layer, &at(0.9, 0.0)) > 0.95 && mask(&m, &edge_layer, &at(0.0, 0.9)) < 0.05);
        assert!(mask(&m, &cavity_layer, &at(0.0, 0.9)) > 0.95 && mask(&m, &cavity_layer, &at(0.9, 0.0)) < 0.05);
        // Inverted, and folded with max.
        let inv = Layer { invert: true, ..edge_layer.clone() };
        assert!(mask(&m, &inv, &at(0.9, 0.0)) < 0.05);
        let both = Layer { mask: vec![t(Source::Edges), with(t(Source::Cavity), Op::Max, 1.0)], ..edge_layer.clone() };
        assert!(mask(&m, &both, &at(0.9, 0.0)) > 0.95 && mask(&m, &both, &at(0.0, 0.9)) > 0.95 && mask(&m, &both, &at(0.0, 0.0)) < 0.05);
        // Direction and height.
        let up = Layer { mask: vec![t(Source::Up)], ..Layer::default() };
        assert_eq!(mask(&m, &up, &At { n: Vec3::Y, ..at(0.0, 0.0) }), 1.0);
        assert_eq!(mask(&m, &up, &At { n: -Vec3::Y, ..at(0.0, 0.0) }), 0.0);
        let low = Layer { mask: vec![height_t(0.5, 0.0)], ..Layer::default() };
        assert_eq!(mask(&m, &low, &At { p: Vec3::new(0.0, 0.1, 0.0), ..at(0.0, 0.0) }), 0.8);
        // Noise is repeatable, varies, and stays in 0..1.
        let nz = noise_t(0.05, 4, 0.0);
        let vals: Vec<f32> = (0..200).map(|i| term(&nz, &At { p: Vec3::new(i as f32 * 0.013, 0.2, 0.1), ..at(0.0, 0.0) })).collect();
        assert!(vals.iter().all(|v| (0.0..=1.0).contains(v)));
        let (lo, hi) = vals.iter().fold((1.0f32, 0.0f32), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(lo < 0.25 && hi > 0.75, "{lo} {hi}");
        // Painted channels read by slot.
        let mut painted = Material { layers: vec![Layer { mask: vec![Term { source: Source::Painted, channel: "blood".into(), ..Term::default() }], ..Layer::default() }], ..Material::default() };
        resolve_slots(&mut painted, &["moss".into(), "blood".into()]);
        assert_eq!(painted.layers[0].mask[0].slot, 1);
        assert_eq!(mask(&painted, &painted.layers[0], &At { painted: [0.0, 0.7, 0.0, 0.0], ..at(0.0, 0.0) }), 0.7);
        resolve_slots(&mut painted, &[]);
        assert_eq!(mask(&painted, &painted.layers[0], &At { painted: [1.0; 4], ..at(0.0, 0.0) }), 0.0);
    }

    #[test]
    fn layers_blend_in_order_and_amounts_move_them() {
        let red = Layer { color: Some([1.0, 0.0, 0.0]), roughness: Some(0.9), ..Layer::default() };
        let blue = Layer { color: Some([0.0, 0.0, 1.0]), metallic: Some(1.0), ..Layer::default() };
        let start = Look::of(&Material::default(), Vec3::splat(0.5), 0.5, Vec3::Z);
        let m = Material { layers: vec![red.clone(), blue.clone()], ..Material::default() };
        let out = layers(&m, &at(0.0, 0.0), start);
        assert_eq!((out.color, out.rough, out.metal), (Vec3::Z, 0.9, 1.0));
        let m = Material { layers: vec![blue, Layer { opacity: 0.5, ..red }], ..Material::default() };
        let out = layers(&m, &at(0.0, 0.0), start);
        assert_eq!(out.color, Vec3::new(0.5, 0.0, 0.5));
        // Wear spreads a worn-edge layer: more amount, more surface.
        let worn = Layer { role: "wear".into(), color: Some([1.0; 3]), mask: vec![t(Source::Edges)], threshold: Some(0.5), contrast: Some(3.0), ..Layer::default() };
        let cover = |wear: Option<f32>| {
            let m = Material { wear, layers: vec![worn.clone()], ..Material::default() };
            (0..=10).map(|i| layers(&m, &at(i as f32 / 10.0, 0.0), start).color.x).sum::<f32>()
        };
        assert!(cover(Some(0.0)) < cover(Some(0.2)) && cover(Some(0.2)) < cover(None) && cover(None) < cover(Some(0.9)), "{} {} {} {}", cover(Some(0.0)), cover(Some(0.2)), cover(None), cover(Some(0.9)));
        assert_eq!(cover(Some(0.0)), 0.5 * 11.0);
    }

    #[test]
    fn every_preset_loads_and_splices() {
        for (name, _) in PRESETS {
            let m = Material::find(&[], name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(m.layers.is_empty(), name=="factory_new", "{name}");
            assert!(m.layers.iter().all(|l| l.smart.is_none()));
            for l in &m.layers {
                if let Some(d) = &l.detail {
                    assert!(crate::detail::entry(d).is_some(), "{name}: {d}");
                }
                for t in &l.mask {
                    if t.source == Source::Grunge {
                        assert!(t.grunge().is_some(), "{name}: {}", t.map);
                    }
                }
            }
            let out = layers(&m, &at(0.5, 0.5), Look::of(&m, Vec3::splat(0.5), 0.5, Vec3::Z));
            assert!(out.color.is_finite() && out.n.is_normalized(), "{name}");
        }
        assert!(Material::find(&[], "nope_iron").is_none());
        // A stack that adds blood over iron.
        let armor: Material = serde_json::from_value(serde_json::json!({ "name": "armor", "layers": [{ "smart": "worn_iron" }, { "smart": "blood_splatter", "opacity": 0.5 }] })).unwrap();
        let found = Material::find(&[armor], "armor").unwrap();
        let iron = preset("worn_iron").unwrap().layers.len();
        assert_eq!(found.layers.len(), iron + 2);
        assert_eq!(found.layers[iron].opacity, 0.5);
        // A scene material named like a preset keeps the stack, with its amounts.
        let tuned: Material = serde_json::from_value(serde_json::json!({ "name": "worn_iron", "rust": 0.9 })).unwrap();
        let found = Material::find(&[tuned], "worn_iron").unwrap();
        assert_eq!((found.layers.len(), found.rust), (iron, Some(0.9)));
    }

    #[test]
    fn decal_images_only_read_absolute_image_files() {
        for bad in ["relative.png", "/etc/passwd", "/tmp", "/no/such/file.png", "../../secret.jpg"] {
            assert!(load_picture(bad).is_err(), "{bad}");
            assert!(picture(bad).is_none(), "{bad}");
        }
        let dir = std::env::temp_dir().join(format!("smart_decal_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("stamp.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])).save(&file).unwrap();
        let p = picture(file.to_str().unwrap()).unwrap();
        assert_eq!((p.size, &p.rgba[..4]), (PICTURE, &[255u8, 0, 0, 255][..]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decals_stamp_where_they_are_put() {
        let d = Decal { shape: Shape::Splat, center: [0.0, 0.0, 0.5], normal: [0.0, 0.0, 1.0], size: 0.2, seed: 3, ..Decal::default() };
        let start = Look::of(&Material::default(), Vec3::splat(0.6), 0.6, Vec3::Z);
        let on = |p: Vec3| decals(std::slice::from_ref(&d), &At { p, n: Vec3::Z, ..At::default() }, start);
        let hit = on(Vec3::new(0.0, 0.0, 0.5));
        assert!(hit.color.x < 0.35 && hit.color.y < 0.1, "{hit:?}");
        assert_eq!(on(Vec3::new(0.3, 0.0, 0.5)), start);
        // The back of the object is not stamped.
        let back = decals(std::slice::from_ref(&d), &At { p: Vec3::new(0.0, 0.0, 0.45), n: -Vec3::Z, ..At::default() }, start);
        assert_eq!(back, start);
        // Every shape covers something near its middle and nothing at its corners.
        for code in 0..4 {
            let inside = (0..400).filter(|i| shape(code, 7, Vec2::new((i % 20) as f32 / 20.0 - 0.475, (i / 20) as f32 / 20.0 - 0.475)) > 0.5).count();
            assert!(inside > 10 && inside < 300, "shape {code}: {inside}");
            assert_eq!(shape(code, 7, Vec2::new(0.499, 0.499)), 0.0, "shape {code}");
        }
        // Runes are cut in: the normal bends at a stroke's side.
        let rune = Decal { shape: Shape::Rune, center: [0.0, 0.0, 0.5], size: 0.2, ..Decal::default() };
        let bent = (0..40).any(|i| decals(std::slice::from_ref(&rune), &At { p: Vec3::new(-0.01 + i as f32 * 0.0005, 0.0, 0.5), n: Vec3::Z, ..At::default() }, start).n.z < 0.999);
        assert!(bent);
    }
}
