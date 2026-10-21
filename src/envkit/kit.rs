//! Modular dungeon pieces, rocks and props, each built from real geometry:
//! every stone of a wall is its own chipped block (see `stone.rs`), so
//! light catches the joints, edges and worn faces.
//!
//! Pieces are sized in whole cells of `GRID` and sit on y = 0:
//! - wall: runs along local X from -length/2 to length/2, thickness along
//!   Z, centered; courses of stones in running bond over a dark mortar core.
//! - corner: a quoin column centered on its grid point, covering the ends
//!   of two walls that meet there; long stones alternate toward +X and +Z.
//! - doorway: a wall with an opening and a round, pointed or flat arch.
//! - pillar: plinth, drums (round) or blocks (square), capital.
//! - stairs: steps climbing along +Z from the origin.
//! - floor: flagstones, bricks or cobbles from -length/2..length/2 (X) by
//!   -depth/2..depth/2 (Z), their tops at y = 0, on a dark grout bed.
//! - vault: a barrel vault along Z springing at `height`.
//! - rock, rubble; torch, candle, brazier (with flames and a light),
//!   bones (clay), chain.

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use super::stone::{self, Corners, Style, aabb, turn};
use super::{Builder, GRID, Rand, fbm, noise};
use crate::material::Material;
use crate::mesh::Mesh;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Piece {
    Wall,
    Corner,
    Doorway,
    Pillar,
    Stairs,
    Floor,
    Vault,
    Rock,
    Rubble,
    Torch,
    Candle,
    Brazier,
    Log,
    Bones,
    Chain,
    Sarcophagus,
}

impl Piece {
    pub const ALL: [Piece; 16] = [Piece::Wall, Piece::Corner, Piece::Doorway, Piece::Pillar, Piece::Stairs, Piece::Floor, Piece::Vault, Piece::Rock, Piece::Rubble, Piece::Torch, Piece::Candle, Piece::Brazier, Piece::Log, Piece::Bones, Piece::Chain, Piece::Sarcophagus];

    pub fn name(self) -> &'static str {
        match self {
            Piece::Wall => "wall",
            Piece::Corner => "corner",
            Piece::Doorway => "doorway",
            Piece::Pillar => "pillar",
            Piece::Stairs => "stairs",
            Piece::Floor => "floor",
            Piece::Vault => "vault",
            Piece::Rock => "rock",
            Piece::Rubble => "rubble",
            Piece::Torch => "torch",
            Piece::Candle => "candle",
            Piece::Brazier => "brazier",
            Piece::Log => "log",
            Piece::Bones => "bones",
            Piece::Chain => "chain",
            Piece::Sarcophagus => "sarcophagus",
        }
    }

    /// Architecture snaps its lengths to whole grid cells.
    pub fn modular(self) -> bool {
        matches!(self, Piece::Wall | Piece::Doorway | Piece::Floor | Piece::Vault)
    }

    /// The light a lit prop carries by default.
    pub fn light(self) -> Option<super::light::LightSource> {
        let (preset, offset) = match self {
            Piece::Torch => ("torch", Vec3::new(0.0, 0.255, 0.175)),
            Piece::Candle => ("candle", Vec3::new(0.0, 0.185, 0.0)),
            Piece::Brazier => ("brazier", Vec3::new(0.0, 0.79, 0.0)),
            _ => return None,
        };
        super::light::LightSource::preset(preset).map(|l| super::light::LightSource { offset, ..l })
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Round,
    Pointed,
    Flat,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    Flagstone,
    Bricks,
    Cobble,
}

/// A kit piece and its settings. Unset values take the piece's defaults
/// (see `MCP` docs and `defaults`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Kit {
    pub piece: Piece,
    /// Along X: walls, doorways, floors; along Z for vaults and chains (hang).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// Pillar thickness, stairs width, vault span, rubble pile diameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    /// Floor size along Z.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    /// Wall, corner and vault thickness; floor stone thickness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f32>,
    /// Mean stone length (floor: stone size; chain: link length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<f32>,
    /// Mean course height of walls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course: Option<f32>,
    /// Joint width between stones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f32>,
    /// How rough stone faces are, 0..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    /// 0..1: chipped edges, cracked and missing stones, crumbling tops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage: Option<f32>,
    /// Floors: how uneven the stones sit (meters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unevenness: Option<f32>,
    /// Mesh density, 0.25..3 (1 = default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<f32>,
    #[serde(default)]
    pub seed: u64,
    /// sRGB stone (or wax, bone, iron) color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// Doorway: opening width, spring height (where the arch starts),
    /// arch shape and voussoir depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<Arch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<f32>,
    /// Stairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rise: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<f32>,
    /// Floor pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<Pattern>,
    /// Pillar: "round" or "square"; torch: "wall" or "stand"; bones:
    /// "femur", "skull", "ribs", "pile".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Rock size (x, y, z).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 3]>,
    /// Rock layering 0..1, and how many flat broken faces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strata: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facets: Option<u32>,
    /// Candles in a cluster; pieces in a rubble pile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Lit props: false leaves the flame out (an unlit torch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lit: Option<bool>,
}

impl Kit {
    pub fn new(piece: Piece) -> Kit {
        Kit {
            piece, length: None, height: None, width: None, depth: None, thickness: None, block: None, course: None, gap: None, roughness: None, damage: None,
            unevenness: None, detail: None, seed: 0, color: None, opening: None, opening_height: None, arch: None, ring: None, steps: None, rise: None, run: None,
            pattern: None, kind: None, size: None, strata: None, facets: None, count: None, lit: None,
        }
    }

    /// Lengths of modular pieces in whole grid cells (at least one).
    pub fn snap(&mut self) {
        if self.piece.modular() {
            for l in [&mut self.length, &mut self.depth].into_iter().flatten() {
                *l = ((*l / GRID).round() * GRID).max(GRID);
            }
            if self.piece == Piece::Vault && let Some(w) = &mut self.width {
                *w = ((*w / GRID).round() * GRID).max(GRID);
            }
        }
    }

    /// Clamps settings so no call can ask for an endless mesh.
    pub fn sanitize(&mut self) {
        let fix = |v: &mut Option<f32>, lo: f32, hi: f32| {
            if let Some(x) = v {
                *x = if x.is_finite() { x.clamp(lo, hi) } else { lo };
            }
        };
        fix(&mut self.length, 0.05, 40.0);
        fix(&mut self.height, 0.02, 20.0);
        fix(&mut self.width, 0.02, 40.0);
        fix(&mut self.depth, 0.05, 40.0);
        fix(&mut self.thickness, 0.01, 4.0);
        fix(&mut self.block, 0.03, 4.0);
        fix(&mut self.course, 0.05, 2.0);
        fix(&mut self.gap, 0.0, 0.2);
        fix(&mut self.roughness, 0.0, 1.0);
        fix(&mut self.damage, 0.0, 1.0);
        fix(&mut self.unevenness, 0.0, 0.2);
        fix(&mut self.detail, 0.25, 3.0);
        fix(&mut self.opening, 0.2, 20.0);
        fix(&mut self.opening_height, 0.2, 20.0);
        fix(&mut self.ring, 0.05, 2.0);
        fix(&mut self.rise, 0.03, 1.0);
        fix(&mut self.run, 0.05, 2.0);
        fix(&mut self.strata, 0.0, 1.0);
        self.steps = self.steps.map(|s| s.clamp(1, 60));
        self.facets = self.facets.map(|s| s.min(24));
        self.count = self.count.map(|s| s.clamp(1, 400));
        if let Some(s) = &mut self.size {
            *s = s.map(|v| if v.is_finite() { v.clamp(0.01, 20.0) } else { 0.5 });
        }
        if let Some(c) = &mut self.color {
            *c = c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.5 });
        }
        if let Some(k) = &self.kind {
            self.kind = Some(k.chars().take(16).collect());
        }
        // Stones under 3 cm across a 40 m floor would be millions.
        let area = self.length.unwrap_or(2.0) * self.depth.unwrap_or(2.0);
        if self.piece == Piece::Floor {
            let min_block = (area / 40_000.0).sqrt().max(0.03);
            if let Some(b) = &mut self.block {
                *b = b.max(min_block);
            }
        }
    }

    fn f(v: Option<f32>, d: f32) -> f32 {
        v.unwrap_or(d)
    }

    pub fn kind_name(&self) -> &str {
        self.kind.as_deref().unwrap_or(match self.piece {
            Piece::Pillar => "round",
            Piece::Torch => "wall",
            Piece::Bones => "femur",
            _ => "",
        })
    }

    /// Checks names that are free text.
    pub fn validate(&self) -> Result<(), String> {
        let kinds: &[&str] = match self.piece {
            Piece::Pillar => &["round", "square"],
            Piece::Torch => &["wall", "stand"],
            Piece::Bones => &["femur", "skull", "ribs", "pile"],
            _ => &[],
        };
        match &self.kind {
            Some(k) if !kinds.contains(&k.as_str()) => Err(if kinds.is_empty() { format!("{} takes no kind", self.piece.name()) } else { format!("{} kind must be one of {kinds:?}", self.piece.name()) }),
            _ => Ok(()),
        }
    }

    /// The finished mesh. `library` overrides the built-in materials by
    /// name (stone, mortar, iron, wood, wax, flame, bone); `main` replaces
    /// the piece's main material (the object's own material).
    pub fn build(&self, library: &[Material], main: Option<&Material>) -> Mesh {
        if self.piece == Piece::Log {
            let mut mesh = crate::fire::charred_log(self.seed as u32);
            if let Some(material) = main.or_else(|| library.iter().find(|m| m.name == "charred_wood" || m.name == "wood")) {
                mesh.materials[0] = material.clone();
                for v in &mut mesh.vertices { v.surface = material.surface(); v.light = material.light(); }
            }
            if self.lit == Some(false) { std::sync::Arc::make_mut(mesh.imported.as_mut().unwrap()).emissive = [0.0;3]; }
            return mesh;
        }
        let mats = Mats::new(library, main, self.piece);
        let detail = Self::f(self.detail, 1.0);
        let mut ctx = Ctx { b: Builder::new(Vec::new()), cell: 0.09 / detail, seed: self.seed, mats };
        ctx.b.mesh.materials.clear();
        match self.piece {
            Piece::Wall => wall(&mut ctx, self, None),
            Piece::Doorway => {
                let opening = Opening {
                    half: Self::f(self.opening, 1.3) * 0.5,
                    spring: Self::f(self.opening_height, 2.0),
                    arch: self.arch.unwrap_or(Arch::Round),
                    ring: Self::f(self.ring, 0.3),
                };
                wall(&mut ctx, self, Some(opening))
            }
            Piece::Corner => corner(&mut ctx, self),
            Piece::Pillar => pillar(&mut ctx, self),
            Piece::Stairs => stairs(&mut ctx, self),
            Piece::Floor => floor(&mut ctx, self),
            Piece::Vault => vault(&mut ctx, self),
            Piece::Rock => {
                let s = self.size.map_or(Vec3::new(0.6, 0.42, 0.5), Vec3::from);
                let style = RockStyle { rough: Self::f(self.roughness, 0.5), strata: Self::f(self.strata, 0.25), facets: self.facets.unwrap_or(7), res: (s.max_element() / 0.035 * detail).clamp(5.0, 40.0) as usize };
                let color = super::color(self.color, [0.33, 0.31, 0.29]);
                let m = ctx.stone();
                rock(&mut ctx.b, Vec3::ZERO, s, Quat::IDENTITY, &style, self.seed, color, m);
            }
            Piece::Rubble => rubble(&mut ctx, self),
            Piece::Torch => torch(&mut ctx, self),
            Piece::Candle => candles(&mut ctx, self),
            Piece::Brazier => brazier(&mut ctx, self),
            Piece::Log => unreachable!("log mesh is built above"),
            Piece::Bones => bones(&mut ctx, self),
            Piece::Chain => chain(&mut ctx, self),
            Piece::Sarcophagus => sarcophagus(&mut ctx, self),
        }
        if matches!(self.piece, Piece::Wall | Piece::Doorway | Piece::Corner | Piece::Pillar | Piece::Stairs | Piece::Floor | Piece::Vault | Piece::Sarcophagus) {
            grime(&mut ctx.b, self.piece == Piece::Floor, self.seed);
        }
        // Weathering bands about a mesh cell or two wide; on small rocks a
        // share of their size, or every bit of them would count as edge.
        let width = match self.piece {
            Piece::Rock => (self.size.map_or(0.5, |s| s.iter().copied().fold(0.0, f32::max)) * 0.12).min(ctx.cell * 1.6),
            Piece::Rubble => ctx.cell * 0.8,
            _ => ctx.cell * 1.6,
        };
        ctx.b.finish(width)
    }
}

/// The piece's materials: built-in, unless the scene library has one of
/// the same name.
struct Mats {
    library: Vec<Material>,
    main: Option<Material>,
    piece: Piece,
}

impl Mats {
    fn new(library: &[Material], main: Option<&Material>, piece: Piece) -> Mats {
        Mats { library: library.to_vec(), main: main.cloned(), piece }
    }

    fn get(&self, name: &str) -> Material {
        if let Some(m) = self.library.iter().find(|m| m.name == name) {
            return m.clone();
        }
        builtin(name)
    }

    /// The main material: the object's own when it names one.
    fn main_or(&self, name: &str) -> Material {
        let _ = self.piece;
        self.main.clone().unwrap_or_else(|| self.get(name))
    }
}

/// The kit's own materials (scene materials of the same name win).
pub fn builtin(name: &str) -> Material {
    let base = Material { name: name.to_string(), ..Material::default() };
    match name {
        "stone" => Material {
            roughness: 0.84, detail: Some("stone".into()), detail_scale: 0.7, detail_strength: 1.6, detail_color: 0.7,
            cavity: 0.65, edge_wear: 0.35, dirt: 0.45, dirt_color: [0.14, 0.12, 0.1], variation: 0.4, gloss_variation: 0.3, ..base
        },
        "mortar" => Material { roughness: 0.95, detail: Some("stone".into()), detail_scale: 0.5, detail_strength: 0.6, detail_color: 0.3, ..base },
        "iron" => Material { roughness: 0.5, metallic: 0.85, detail: Some("metal_scratched".into()), detail_strength: 0.7, detail_color: 0.4, cavity: 0.5, edge_wear: 0.4, ..base },
        "wood" => Material { roughness: 0.75, detail: Some("wood".into()), detail_strength: 1.0, detail_color: 0.6, cavity: 0.3, ..base },
        "wax" => Material { roughness: 0.42, subsurface: 0.75, scatter: [0.012, 0.008, 0.005], ..base },
        // Seen by the camera only; the light it stands for is the object's
        // light (see `film`: "flame" surfaces let shadow rays through).
        "flame" => Material { roughness: 1.0, emission: 4.5, ..base },
        "ember" => Material { roughness: 0.9, emission: 1.2, ..base },
        "coal" => Material { roughness: 0.92, detail: Some("stone".into()), detail_strength: 0.8, detail_color: 0.3, ..base },
        "bone" => Material { roughness: 0.6, detail: Some("stone".into()), detail_scale: 0.25, detail_strength: 0.5, detail_color: 0.25, cavity: 0.6, dirt: 0.5, dirt_color: [0.3, 0.25, 0.18], ..base },
        "cloth" => Material { roughness: 0.92, ..Material::preset("cloth").unwrap_or(base) },
        "grout" => Material { roughness: 1.0, detail: Some("stone".into()), detail_scale: 0.4, detail_strength: 0.8, detail_color: 0.4, ..base },
        _ => base,
    }
}


struct Ctx {
    b: Builder,
    /// Grid spacing of stone faces.
    cell: f32,
    seed: u64,
    mats: Mats,
}

impl Ctx {
    fn mat(&mut self, name: &str) -> u32 {
        let m = self.mats.get(name);
        self.b.material(m)
    }

    fn stone(&mut self) -> u32 {
        let m = self.mats.main_or("stone");
        self.b.material(Material { name: if self.mats.main.is_some() { m.name.clone() } else { "stone".into() }, ..m })
    }

    fn style(&self, seed: u64, color: Vec3, material: u32, kit: &Kit) -> Style {
        let damage = Kit::f(kit.damage, 0.15);
        let rough = Kit::f(kit.roughness, 0.5);
        Style {
            cell: self.cell,
            round: 0.018 + 0.012 * rough,
            rough: 0.004 + 0.014 * rough,
            chips: 1 + (damage * 5.0) as u32,
            chip_depth: 0.08 + 0.32 * damage,
            round_section: false,
            seed,
            color,
            material,
        }
    }
}

/// Large damp stains and blotches across a whole piece, and rising damp
/// darkening the foot of walls (stone surfaces only).
fn grime(b: &mut Builder, floor: bool, seed: u64) {
    let stone = b.mesh.materials.iter().position(|m| m.name != "mortar" && m.name != "grout").unwrap_or(0) as u32;
    let seed = seed as u32 ^ 0x6E1;
    for v in b.mesh.vertices.iter_mut().filter(|v| v.material == stone) {
        let p = Vec3::from(v.pos);
        let blot = super::smoothstep(0.05, 0.6, fbm(p * 0.55, 3, seed));
        let streak = if floor { 0.0 } else { super::smoothstep(0.35, 0.9, noise(Vec3::new(p.x * 5.0, p.y * 0.35, p.z * 5.0), seed ^ 3)) };
        let damp = if floor { 1.0 } else { 0.62 + 0.38 * super::smoothstep(0.0, 1.4, p.y) };
        let k = (1.0 - 0.38 * blot) * (1.0 - 0.3 * streak) * damp;
        v.color = (Vec3::from(v.color) * k).to_array();
    }
}

// ------------------------------------------------------------------ walls

/// A wall opening: half width, spring height, arch shape, voussoir depth.
#[derive(Clone, Copy)]
struct Opening {
    half: f32,
    spring: f32,
    arch: Arch,
    ring: f32,
}

impl Opening {
    /// (intrados radius, center offset from the middle) of each arch side.
    fn arc(&self) -> (f32, f32) {
        match self.arch {
            Arch::Round | Arch::Flat => (self.half, 0.0),
            // Two arcs of radius 1.7 x half width meeting at a point.
            Arch::Pointed => {
                let r = self.half * 1.7;
                (r, r - self.half)
            }
        }
    }

    /// Half-width the wall's stones must leave free at height `y`.
    fn free(&self, y: f32) -> f32 {
        if y < self.spring {
            return self.half;
        }
        if self.arch == Arch::Flat {
            return if y < self.spring + self.ring { self.half + self.ring } else { 0.0 };
        }
        let (r, off) = self.arc();
        let ro = r + self.ring;
        let dy = y - self.spring;
        if dy >= ro {
            return 0.0;
        }
        ((ro * ro - dy * dy).sqrt() - off).max(0.0)
    }

    /// Top of the arch stones.
    fn top(&self) -> f32 {
        if self.arch == Arch::Flat {
            return self.spring + self.ring;
        }
        let (r, off) = self.arc();
        let ro = r + self.ring;
        self.spring + (ro * ro - off * off).max(0.0).sqrt()
    }
}

fn wall(ctx: &mut Ctx, kit: &Kit, opening: Option<Opening>) {
    let length = Kit::f(kit.length, if opening.is_some() { 3.0 } else { 2.0 });
    let height = Kit::f(kit.height, if opening.is_some() { 3.5 } else { 3.0 });
    let thick = Kit::f(kit.thickness, 0.5);
    let block = Kit::f(kit.block, 0.55);
    let course = Kit::f(kit.course, 0.32);
    let gap = Kit::f(kit.gap, 0.012);
    let damage = Kit::f(kit.damage, 0.15);
    let color = super::color(kit.color, [0.4, 0.38, 0.35]);
    let stone_m = ctx.stone();
    let mortar = ctx.mat("mortar");
    let half = length * 0.5;
    let mut rng = Rand::new(ctx.seed ^ 0x5741_4c4c);
    // Course lines, with one at the spring line of an opening.
    let mut lines = vec![0.0];
    let mut y = 0.0;
    while y < height - 1e-4 {
        let mut h = course * rng.range(0.78, 1.25);
        if let Some(o) = opening
            && y < o.spring - 0.1
            && y + h > o.spring - 0.1
        {
            h = o.spring - y;
        }
        if height - (y + h) < course * 0.45 {
            h = height - y;
        }
        y += h;
        lines.push(y.min(height));
    }
    // A crumbling top: the last courses lose stones, more with damage.
    let mut k = 0u64;
    for c in 0..lines.len() - 1 {
        let (y0, y1) = (lines[c], lines[c + 1]);
        // Stones must leave [-free, free] open at both of their heights.
        let (f0, f1) = opening.map_or((0.0, 0.0), |o| (o.free(y0 + 1e-3), o.free(y1 - 1e-3)));
        let mut spans: Vec<[f32; 4]> = Vec::new();
        if f0 <= 0.0 && f1 <= 0.0 {
            spans.push([-half, -half, half, half]);
        } else {
            spans.push([-half, -half, -f0, -f1]);
            spans.push([f0, f1, half, half]);
        }
        let top_course = c + 2 >= lines.len();
        for [l0, l1, r0, r1] in spans {
            let narrow = (r0 - l0).min(r1 - l1);
            if narrow < 0.06 {
                continue;
            }
            // Cut points along the course: random stone lengths, offset
            // every other course (running bond).
            let mut cuts = vec![];
            let lo = l0.max(l1);
            let hi = r0.min(r1);
            let mut x = lo + if c % 2 == 0 { 0.0 } else { -block * rng.range(0.35, 0.6) };
            loop {
                x += block * rng.range(0.7, 1.35);
                if x > hi - block * 0.35 {
                    break;
                }
                if x > lo + block * 0.25 {
                    cuts.push(x);
                }
            }
            let mut edges = vec![(l0, l1)];
            edges.extend(cuts.iter().map(|&x| (x, x)));
            edges.push((r0, r1));
            for e in edges.windows(2) {
                k += 1;
                let mut r = Rand::of(ctx.seed, k * 7919 + c as u64);
                let ruin = damage * (0.04 + 0.5 * ((y1 / height).powi(3)) * if top_course { 1.0 } else { 0.4 });
                if c > 0 && r.chance(ruin) {
                    continue;
                }
                let (a, z) = (e[0], e[1]);
                let g = gap * 0.5;
                // The top course's stones may sit lower when the wall is damaged.
                let drop = if top_course && damage > 0.0 { r.f() * damage * (y1 - y0) * 0.6 } else { 0.0 };
                let (yb, yt) = (y0 + g, y1 - g - drop);
                if yt - yb < 0.04 {
                    continue;
                }
                let front = thick * 0.5 + r.range(-0.6, 1.0) * 0.012;
                let back = -thick * 0.5 - r.range(-0.6, 1.0) * 0.01;
                let cs: Corners = [
                    Vec3::new(a.0 + g, yb, back), Vec3::new(z.0 - g, yb, back),
                    Vec3::new(a.1 + g, yt, back), Vec3::new(z.1 - g, yt, back),
                    Vec3::new(a.0 + g, yb, front), Vec3::new(z.0 - g, yb, front),
                    Vec3::new(a.1 + g, yt, front), Vec3::new(z.1 - g, yt, front),
                ];
                let q = Quat::from_euler(glam::EulerRot::XYZ, r.signed() * 0.012, r.signed() * 0.015, r.signed() * 0.012);
                let style = ctx.style(r.next(), color, stone_m, kit);
                stone::block(&mut ctx.b, turn(cs, q), &style);
            }
        }
    }
    // The mortar core shows in the joints; it leaves the opening open.
    let core = thick * 0.5 - 0.035;
    // Below the top course, so stones a damaged top lost do not bare it.
    let top = lines[lines.len().saturating_sub(2)].max(height * 0.5).min(height - 0.02);
    match opening {
        None => stone::plain(&mut ctx.b, Vec3::new(-half + 0.004, 0.0, -core), Vec3::new(half - 0.004, top, core), color * 0.45, mortar),
        Some(o) => {
            let side = (o.half + if o.arch == Arch::Flat { o.ring } else { 0.0 }).min(half);
            if side < half - 0.01 {
                stone::plain(&mut ctx.b, Vec3::new(-half + 0.004, 0.0, -core), Vec3::new(-side, top, core), color * 0.45, mortar);
                stone::plain(&mut ctx.b, Vec3::new(side, 0.0, -core), Vec3::new(half - 0.004, top, core), color * 0.45, mortar);
            }
            if o.top() < top {
                stone::plain(&mut ctx.b, Vec3::new(-side, o.top(), -core), Vec3::new(side, top, core), color * 0.45, mortar);
            }
            arch(ctx, kit, o, thick, color, stone_m);
        }
    }
}

/// The arch stones (voussoirs) of an opening, with a keystone.
fn arch(ctx: &mut Ctx, kit: &Kit, o: Opening, thick: f32, color: Vec3, material: u32) {
    let gap = Kit::f(kit.gap, 0.012) * 0.5;
    let front = thick * 0.5 + 0.02;
    let back = -thick * 0.5 - 0.015;
    if o.arch == Arch::Flat {
        // One lintel across the top.
        let mut r = Rand::of(ctx.seed, 991);
        let style = ctx.style(r.next(), color * 1.05, material, kit);
        stone::block(&mut ctx.b, aabb(Vec3::new(-o.half - o.ring + gap, o.spring + gap, back), Vec3::new(o.half + o.ring - gap, o.spring + o.ring - gap, front)), &style);
        return;
    }
    let (radius, off) = o.arc();
    let outer = radius + o.ring;
    // Each side runs from its springer up to the crown.
    // Angle from the springer (0) up to the crown, on each side's arc.
    let span = (off / radius).clamp(-1.0, 1.0).acos();
    let per_side = ((span * (radius + o.ring * 0.5)) / 0.32).round().max(2.0) as usize;
    let mut stones: Vec<[Vec3; 4]> = Vec::new();
    for side in [-1.0f32, 1.0] {
        let center = Vec3::new(side * -off, o.spring, 0.0);
        let at = |a: f32, r: f32| center + Vec3::new(side * a.cos() * r, a.sin() * r, 0.0);
        for i in 0..per_side {
            let a0 = span * i as f32 / per_side as f32;
            let a1 = span * (i + 1) as f32 / per_side as f32;
            stones.push([at(a0, radius), at(a1, radius), at(a0, outer), at(a1, outer)]);
        }
    }
    let round = o.arch == Arch::Round;
    for (i, s) in stones.iter().enumerate() {
        let mut r = Rand::of(ctx.seed, 7000 + i as u64);
        let [i0, i1, o0, o1] = *s;
        // Joints: pull each side in along the arc.
        let along0 = (i1 - i0).normalize_or(Vec3::X) * gap;
        let along1 = (o1 - o0).normalize_or(Vec3::X) * gap;
        // The two stones at a round arch's crown stand proud as keystones.
        let keystone = round && (i == per_side - 1 || i == 2 * per_side - 1);
        let lift = if keystone { 0.07 } else { r.range(0.0, 0.03) };
        let out0 = o0 + (o0 - i0).normalize_or(Vec3::Y) * lift;
        let out1 = o1 + (o1 - i1).normalize_or(Vec3::Y) * lift;
        let (f, b) = (front + if keystone { 0.025 } else { r.range(-0.005, 0.01) }, back);
        let p = |v: Vec3, z: f32| Vec3::new(v.x, v.y, z);
        let cs: Corners = [
            p(i0 + along0, b), p(i1 - along0, b), p(out0 + along1, b), p(out1 - along1, b),
            p(i0 + along0, f), p(i1 - along0, f), p(out0 + along1, f), p(out1 - along1, f),
        ];
        let style = ctx.style(r.next(), color * r.range(0.98, 1.1), material, kit);
        stone::block(&mut ctx.b, cs, &style);
    }
}

fn corner(ctx: &mut Ctx, kit: &Kit) {
    let height = Kit::f(kit.height, 3.0);
    let thick = Kit::f(kit.thickness, 0.5);
    let course = Kit::f(kit.course, 0.32);
    let gap = Kit::f(kit.gap, 0.012) * 0.5;
    let color = super::color(kit.color, [0.4, 0.38, 0.35]);
    let m = ctx.stone();
    let mortar = ctx.mat("mortar");
    let c = thick * 0.56;
    let mut rng = Rand::new(ctx.seed ^ 0xC0);
    let mut y = 0.0;
    let mut k = 0;
    while y < height - 1e-3 {
        let mut h = course * rng.range(0.85, 1.2);
        if height - (y + h) < course * 0.45 {
            h = height - y;
        }
        let long = rng.range(0.25, 0.5);
        let (ex, ez) = if k % 2 == 0 { (long, 0.0) } else { (0.0, long) };
        let lo = Vec3::new(-c + gap, y + gap, -c + gap);
        let hi = Vec3::new(c + ex - gap, y + h - gap, c + ez - gap);
        let mut r = Rand::of(ctx.seed, 300 + k);
        let q = Quat::from_rotation_y(r.signed() * 0.015);
        let style = Style { round: ctx.style(0, color, m, kit).round * 1.3, ..ctx.style(r.next(), color * 1.03, m, kit) };
        stone::block(&mut ctx.b, turn(aabb(lo, hi), q), &style);
        y += h;
        k += 1;
    }
    stone::plain(&mut ctx.b, Vec3::new(-c * 0.9, 0.0, -c * 0.9), Vec3::new(c * 0.9, height - 0.02, c * 0.9), color * 0.45, mortar);
}

fn pillar(ctx: &mut Ctx, kit: &Kit) {
    let height = Kit::f(kit.height, 3.0);
    let w = Kit::f(kit.width, 0.6);
    let color = super::color(kit.color, [0.42, 0.4, 0.37]);
    let m = ctx.stone();
    let round = kit.kind_name() != "square";
    let g = Kit::f(kit.gap, 0.012) * 0.5;
    let mut n = 0u64;
    let mut add = |ctx: &mut Ctx, lo: Vec3, hi: Vec3, disc: bool, flare: f32| {
        n += 1;
        let mut r = Rand::of(ctx.seed, 500 + n);
        let mut cs = aabb(lo + Vec3::Y * g, hi - Vec3::Y * g);
        if flare != 1.0 {
            for (i, c) in cs.iter_mut().enumerate() {
                if i & 2 != 0 {
                    c.x *= flare;
                    c.z *= flare;
                }
            }
        }
        let q = Quat::from_rotation_y(r.f() * std::f32::consts::TAU);
        let style = Style { round_section: disc, round: if disc { 0.025 } else { 0.03 }, ..ctx.style(r.next(), color * r.range(0.95, 1.05), m, kit) };
        stone::block(&mut ctx.b, if disc { turn(cs, q) } else { cs }, &style);
    };
    let plinth = 0.22_f32.min(height * 0.1);
    let base = 0.14_f32.min(height * 0.06);
    let cap = 0.2_f32.min(height * 0.08);
    let abacus = 0.15_f32.min(height * 0.06);
    add(ctx, Vec3::new(-w * 0.66, 0.0, -w * 0.66), Vec3::new(w * 0.66, plinth, w * 0.66), false, 1.0);
    let r0 = w * 0.5;
    add(ctx, Vec3::new(-r0 * 1.16, plinth, -r0 * 1.16), Vec3::new(r0 * 1.16, plinth + base, r0 * 1.16), round, 0.9);
    let shaft0 = plinth + base;
    let shaft1 = height - cap - abacus;
    let drums = ((shaft1 - shaft0) / if round { 0.5 } else { 0.42 }).round().max(1.0) as usize;
    let mut rng = Rand::new(ctx.seed ^ 0x9111);
    for i in 0..drums {
        let y0 = shaft0 + (shaft1 - shaft0) * i as f32 / drums as f32;
        let y1 = shaft0 + (shaft1 - shaft0) * (i + 1) as f32 / drums as f32;
        let rr = r0 * rng.range(0.985, 1.015);
        let shift = if round { Vec3::ZERO } else { Vec3::new(rng.signed() * 0.01, 0.0, rng.signed() * 0.01) };
        add(ctx, Vec3::new(-rr, y0, -rr) + shift, Vec3::new(rr, y1, rr) + shift, round, 1.0);
    }
    add(ctx, Vec3::new(-r0, shaft1, -r0), Vec3::new(r0, shaft1 + cap, r0), round, 1.25);
    add(ctx, Vec3::new(-w * 0.68, height - abacus, -w * 0.68), Vec3::new(w * 0.68, height, w * 0.68), false, 1.0);
}

/// A stone coffin on a plinth: a carved body with sunken panels and a
/// gabled lid that overhangs it (length along X).
fn sarcophagus(ctx: &mut Ctx, kit: &Kit) {
    let length = Kit::f(kit.length, 2.1);
    let width = Kit::f(kit.width, 0.9);
    let height = Kit::f(kit.height, 0.85);
    let damage = Kit::f(kit.damage, 0.25);
    let color = super::color(kit.color, [0.44, 0.42, 0.39]);
    let m = ctx.stone();
    let (hl, hw) = (length * 0.5, width * 0.5);
    let plinth = 0.1_f32.min(height * 0.15);
    let lid = 0.16_f32.min(height * 0.25);
    let body = height - plinth - lid;
    let mut n = 0u64;
    let mut add = |ctx: &mut Ctx, cs: Corners, round: f32, chips: u32| {
        n += 1;
        let mut r = Rand::of(ctx.seed, 800 + n);
        let style = Style { round, chips, chip_depth: 0.08 + 0.25 * damage, ..ctx.style(r.next(), color * r.range(0.92, 1.06), m, kit) };
        stone::block(&mut ctx.b, cs, &style);
    };
    add(ctx, aabb(Vec3::new(-hl - 0.06, 0.0, -hw - 0.06), Vec3::new(hl + 0.06, plinth, hw + 0.06)), 0.02, 2);
    add(ctx, aabb(Vec3::new(-hl, plinth, -hw), Vec3::new(hl, plinth + body, hw)), 0.025, 2 + (damage * 3.0) as u32);
    // Sunken panels along each long side, and a band at the top.
    for side in [-1.0f32, 1.0] {
        for k in 0..3 {
            let x0 = -hl + 0.12 + (length - 0.24) * k as f32 / 3.0 + 0.03;
            let x1 = -hl + 0.12 + (length - 0.24) * (k + 1) as f32 / 3.0 - 0.03;
            let z = side * (hw + 0.012);
            let (za, zb) = if side > 0.0 { (z - 0.03, z) } else { (z, z + 0.03) };
            add(ctx, aabb(Vec3::new(x0, plinth + 0.1, za), Vec3::new(x1, plinth + body - 0.1, zb)), 0.008, 0);
        }
    }
    // The gabled lid: wide at the eaves, a ridge along the middle.
    let y0 = plinth + body;
    let (ox, oz) = (hl + 0.05, hw + 0.05);
    let ridge = y0 + lid;
    let cs: Corners = [
        Vec3::new(-ox, y0, -oz), Vec3::new(ox, y0, -oz), Vec3::new(-ox, y0 + lid * 0.45, -oz), Vec3::new(ox, y0 + lid * 0.45, -oz),
        Vec3::new(-ox, y0, 0.0), Vec3::new(ox, y0, 0.0), Vec3::new(-ox * 0.97, ridge, 0.0), Vec3::new(ox * 0.97, ridge, 0.0),
    ];
    add(ctx, cs, 0.015, 1);
    let cs: Corners = [
        Vec3::new(-ox, y0, 0.0), Vec3::new(ox, y0, 0.0), Vec3::new(-ox * 0.97, ridge, 0.0), Vec3::new(ox * 0.97, ridge, 0.0),
        Vec3::new(-ox, y0, oz), Vec3::new(ox, y0, oz), Vec3::new(-ox, y0 + lid * 0.45, oz), Vec3::new(ox, y0 + lid * 0.45, oz),
    ];
    add(ctx, cs, 0.015, 1);
}

fn stairs(ctx: &mut Ctx, kit: &Kit) {
    let width = Kit::f(kit.width, 2.0);
    let steps = kit.steps.unwrap_or(5);
    let rise = Kit::f(kit.rise, 0.18);
    let run = Kit::f(kit.run, 0.34);
    let gap = Kit::f(kit.gap, 0.01) * 0.5;
    let color = super::color(kit.color, [0.4, 0.38, 0.35]);
    let m = ctx.stone();
    let total = steps as f32 * run;
    for k in 0..steps {
        let y0 = k as f32 * rise;
        let y1 = y0 + rise;
        let z0 = k as f32 * run;
        let mut r = Rand::of(ctx.seed, 900 + k as u64);
        // Each step is one to three long stones across.
        let pieces = ((width / 0.9).round() as usize).clamp(1, 3);
        let mut cuts = vec![-width * 0.5];
        for i in 1..pieces {
            cuts.push(-width * 0.5 + width * (i as f32 + r.signed() * 0.2) / pieces as f32);
        }
        cuts.push(width * 0.5);
        for (i, c) in cuts.windows(2).enumerate() {
            let sag = r.signed() * 0.008;
            let lo = Vec3::new(c[0] + gap, y0 + gap * 0.3, z0 + gap);
            let hi = Vec3::new(c[1] - gap, y1 - gap + sag, total - gap);
            let style = Style { round: 0.025, ..ctx.style(r.next() ^ i as u64, color * r.range(0.92, 1.06), m, kit) };
            stone::block(&mut ctx.b, aabb(lo, hi), &style);
        }
    }
}

fn floor(ctx: &mut Ctx, kit: &Kit) {
    let lx = Kit::f(kit.length, 2.0);
    let lz = Kit::f(kit.depth, lx);
    let pattern = kit.pattern.unwrap_or(Pattern::Flagstone);
    let size = Kit::f(kit.block, match pattern {
        Pattern::Flagstone => 0.62,
        Pattern::Bricks => 0.3,
        Pattern::Cobble => 0.2,
    });
    let gap = Kit::f(kit.gap, if pattern == Pattern::Cobble { 0.02 } else { 0.016 });
    let uneven = Kit::f(kit.unevenness, if pattern == Pattern::Cobble { 0.02 } else { 0.012 });
    let thick = Kit::f(kit.thickness, if pattern == Pattern::Cobble { 0.12 } else { 0.1 });
    let damage = Kit::f(kit.damage, 0.2);
    let color = super::color(kit.color, [0.38, 0.36, 0.33]);
    let m = ctx.stone();
    let grout = ctx.mat("grout");
    let mut rng = Rand::new(ctx.seed ^ 0xF1);
    // Rectangles in the XZ plane: (x0, z0, x1, z1).
    let mut rects: Vec<[f32; 4]> = Vec::new();
    match pattern {
        Pattern::Flagstone => {
            let mut todo = vec![[-lx * 0.5, -lz * 0.5, lx * 0.5, lz * 0.5]];
            while let Some(r) = todo.pop() {
                let (w, d) = (r[2] - r[0], r[3] - r[1]);
                let big = size * 1.7;
                let split = w.max(d) > big || (w.max(d) > size * 0.95 && rng.chance(0.6));
                if !split || w.max(d) < size * 0.5 {
                    rects.push(r);
                    continue;
                }
                let t = rng.range(0.27, 0.73);
                if w > d {
                    let x = r[0] + w * t;
                    todo.push([r[0], r[1], x, r[3]]);
                    todo.push([x, r[1], r[2], r[3]]);
                } else {
                    let z = r[1] + d * t;
                    todo.push([r[0], r[1], r[2], z]);
                    todo.push([r[0], z, r[2], r[3]]);
                }
            }
        }
        Pattern::Bricks => {
            let (bl, bw) = (size, size * 0.5);
            let rows = (lz / bw).round().max(1.0) as usize;
            for row in 0..rows {
                let z0 = -lz * 0.5 + lz * row as f32 / rows as f32;
                let z1 = -lz * 0.5 + lz * (row + 1) as f32 / rows as f32;
                let mut x = -lx * 0.5 - if row % 2 == 1 { bl * 0.5 } else { 0.0 };
                while x < lx * 0.5 - 1e-4 {
                    let x1 = (x + bl * rng.range(0.92, 1.08)).min(lx * 0.5);
                    let x0 = x.max(-lx * 0.5);
                    if x1 - x0 > bl * 0.2 {
                        rects.push([x0, z0, x1, z1]);
                    } else if let Some(last) = rects.last_mut() {
                        last[2] = x1;
                    }
                    x = x1;
                }
            }
        }
        Pattern::Cobble => {
            let nx = (lx / size).round().max(1.0) as usize;
            let nz = (lz / size).round().max(1.0) as usize;
            for j in 0..nz {
                for i in 0..nx {
                    let shift = if j % 2 == 1 { 0.5 } else { 0.0 };
                    let x0 = (-lx * 0.5 + lx * (i as f32 + shift) / nx as f32).max(-lx * 0.5);
                    let x1 = (-lx * 0.5 + lx * (i as f32 + 1.0 + shift) / nx as f32).min(lx * 0.5);
                    if x1 - x0 < size * 0.3 {
                        continue;
                    }
                    rects.push([x0, -lz * 0.5 + lz * j as f32 / nz as f32, x1, -lz * 0.5 + lz * (j + 1) as f32 / nz as f32]);
                    if j % 2 == 1 && i == 0 {
                        rects.push([-lx * 0.5, -lz * 0.5 + lz * j as f32 / nz as f32, x0, -lz * 0.5 + lz * (j + 1) as f32 / nz as f32]);
                    }
                }
            }
        }
    }
    let g = gap * 0.5;
    for (i, rect) in rects.iter().enumerate() {
        let mut r = Rand::of(ctx.seed, 20_000 + i as u64);
        if r.chance(damage * 0.04) {
            // A missing stone: the dirt bed and a few broken bits.
            let style = RockStyle { rough: 0.7, strata: 0.0, facets: 4, res: 5 };
            for k in 0..(2 + (r.f() * 4.0) as usize) {
                let p = Vec3::new(r.range(rect[0], rect[2]), -0.035, r.range(rect[1], rect[3]));
                let size = r.range(0.04, 0.11);
                rock(&mut ctx.b, p, Vec3::new(size * 1.3, size * 0.7, size), Quat::from_rotation_y(r.f() * 6.3), &style, r.next() ^ k as u64, color * r.range(0.7, 1.0), m);
            }
            continue;
        }
        let jitter = if pattern == Pattern::Cobble { size * 0.08 } else { 0.0 };
        let mut parts = vec![*rect];
        // Cracked stones: two halves with a fine crack between.
        if pattern == Pattern::Flagstone && r.chance(damage * 0.35) {
            let t = r.range(0.35, 0.65);
            parts = if rect[2] - rect[0] > rect[3] - rect[1] {
                let x = rect[0] + (rect[2] - rect[0]) * t;
                vec![[rect[0], rect[1], x - 0.004, rect[3]], [x + 0.004, rect[1], rect[2], rect[3]]]
            } else {
                let z = rect[1] + (rect[3] - rect[1]) * t;
                vec![[rect[0], rect[1], rect[2], z - 0.004], [rect[0], z + 0.004, rect[2], rect[3]]]
            };
        }
        let tone = color * r.range(0.74, 1.15);
        for (k, p) in parts.into_iter().enumerate() {
            let top = r.signed() * uneven - if k > 0 { r.f() * uneven } else { 0.0 };
            let (tx, tz) = (r.signed() * uneven * 0.8, r.signed() * uneven * 0.8);
            let (x0, z0, x1, z1) = (p[0] + g + r.signed() * jitter, p[1] + g + r.signed() * jitter, p[2] - g + r.signed() * jitter, p[3] - g + r.signed() * jitter);
            if x1 - x0 < 0.02 || z1 - z0 < 0.02 {
                continue;
            }
            let lift = |x: f32, z: f32| top + tx * (x - (x0 + x1) * 0.5) / (x1 - x0).max(1e-3) + tz * (z - (z0 + z1) * 0.5) / (z1 - z0).max(1e-3);
            let bottom = -thick;
            let cs: Corners = [
                Vec3::new(x0, bottom, z0), Vec3::new(x1, bottom, z0),
                Vec3::new(x0, lift(x0, z0), z0), Vec3::new(x1, lift(x1, z0), z0),
                Vec3::new(x0, bottom, z1), Vec3::new(x1, bottom, z1),
                Vec3::new(x0, lift(x0, z1), z1), Vec3::new(x1, lift(x1, z1), z1),
            ];
            let mut style = ctx.style(r.next(), tone, m, kit);
            if pattern == Pattern::Cobble {
                style.round = size * 0.28;
                style.rough *= 1.5;
            } else {
                style.round = style.round.max(0.015);
            }
            stone::block(&mut ctx.b, cs, &style);
        }
    }
    // The grout bed: dark dirt in the joints.
    stone::plain(&mut ctx.b, Vec3::new(-lx * 0.5, -thick * 0.9, -lz * 0.5), Vec3::new(lx * 0.5, -0.03, lz * 0.5), Vec3::new(0.2, 0.17, 0.13), grout);
}

fn vault(ctx: &mut Ctx, kit: &Kit) {
    let length = Kit::f(kit.length, 2.0);
    let span = Kit::f(kit.width, 4.0);
    let spring = Kit::f(kit.height, 3.0);
    let thick = Kit::f(kit.thickness, 0.35);
    let gap = Kit::f(kit.gap, 0.012) * 0.5;
    let color = super::color(kit.color, [0.36, 0.34, 0.31]);
    let m = ctx.stone();
    let radius = span * 0.5;
    let rings = ((length / 0.45).round() as usize).max(1);
    let per_ring = ((std::f32::consts::PI * (radius + thick * 0.5)) / 0.42).round().max(3.0) as usize;
    for ring in 0..rings {
        let z0 = -length * 0.5 + length * ring as f32 / rings as f32;
        let z1 = -length * 0.5 + length * (ring + 1) as f32 / rings as f32;
        let shift = if ring % 2 == 1 { 0.5 } else { 0.0 };
        let mut cuts: Vec<f32> = (0..=per_ring).map(|i| ((i as f32 + shift) / per_ring as f32).min(1.0)).collect();
        cuts.insert(0, 0.0);
        cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        for (i, w) in cuts.windows(2).enumerate() {
            let mut r = Rand::of(ctx.seed, 40_000 + (ring * 1000 + i) as u64);
            let (a0, a1) = (std::f32::consts::PI * (1.0 - w[0]), std::f32::consts::PI * (1.0 - w[1]));
            let at = |a: f32, rad: f32, z: f32| Vec3::new(a.cos() * rad, spring + a.sin() * rad, z);
            let da = gap / radius;
            let (a0, a1) = (a0 - da, a1 + da);
            let (ri, ro) = (radius, radius + thick + r.range(-0.02, 0.02));
            let cs: Corners = [
                at(a0, ri, z0 + gap), at(a1, ri, z0 + gap), at(a0, ro, z0 + gap), at(a1, ro, z0 + gap),
                at(a0, ri, z1 - gap), at(a1, ri, z1 - gap), at(a0, ro, z1 - gap), at(a1, ro, z1 - gap),
            ];
            let style = ctx.style(r.next(), color * r.range(0.9, 1.08), m, kit);
            stone::block(&mut ctx.b, cs, &style);
        }
    }
}

// ------------------------------------------------------------------ rocks

pub struct RockStyle {
    pub rough: f32,
    pub strata: f32,
    pub facets: u32,
    /// Grid cells per cube face side.
    pub res: usize,
}

/// A rock: a sphere-like grid cut by flat broken faces, layered and
/// roughened, resting on its flattened base at `base`.
#[allow(clippy::too_many_arguments)]
pub fn rock(b: &mut Builder, base: Vec3, size: Vec3, q: Quat, s: &RockStyle, seed: u64, color: Vec3, material: u32) {
    let start = b.mesh.vertices.len();
    let n = s.res.max(2);
    let mut rng = Rand::new(seed ^ 0x7c0c);
    let half = size * 0.5;
    let planes: Vec<(Vec3, f32)> = (0..s.facets).map(|_| {
        let d = (rng.unit() + Vec3::Y * 0.3).normalize();
        (d, rng.range(0.55, 0.85))
    }).collect();
    let axis = (Vec3::Y + rng.unit() * 0.35).normalize();
    let strata_freq = rng.range(5.0, 9.0) / size.max_element().max(0.05);
    let tone = color * rng.range(0.85, 1.1);
    let seed32 = seed as u32;
    let mut welded: std::collections::HashMap<(i32, i32, i32), u32> = std::collections::HashMap::new();
    let mut point = |b: &mut Builder, key: (i32, i32, i32)| -> u32 {
        if let Some(&v) = welded.get(&key) {
            return v;
        }
        let c = Vec3::new(key.0 as f32, key.1 as f32, key.2 as f32) / n as f32 * 2.0 - Vec3::ONE;
        let dir = c.normalize_or(Vec3::Y);
        // Unit sphere, cut flat by the broken faces.
        let mut p = dir;
        for &(pn, d) in &planes {
            let over = p.dot(pn) - d;
            if over > 0.0 {
                p -= pn * over;
            }
        }
        // Broad lumps, sharp cracks, and fine grit.
        let rough = s.rough * (0.55 * fbm(p * 2.2, 4, seed32) + 0.35 * super::ridge(p * 5.0, seed32 ^ 3) + 0.2 * fbm(p * 11.0, 2, seed32 ^ 4) - 0.25);
        let layer = (p.dot(axis) * strata_freq * size.max_element() * 0.5).fract();
        let terrace = s.strata * (layer - 0.5).abs() * 0.12;
        p *= 1.0 + 0.11 * rough - terrace;
        // Flattened, slightly buried base.
        p.y = p.y.max(-0.55 + 0.03 * noise(p * 6.0, seed32 ^ 9));
        let local = p * half;
        let pos = base + q * (local + Vec3::Y * half.y * 0.55);
        let band = 1.0 + s.strata * 0.12 * ((p.dot(axis) * strata_freq * size.max_element() * 0.5 * std::f32::consts::TAU).sin());
        let ground = 0.75 + 0.25 * super::smoothstep(-0.55, -0.1, p.y);
        let mottle = 1.0 + 0.15 * fbm(p * 4.0, 2, seed32 ^ 5);
        let v = b.vertex(pos, q * dir, tone * band * ground * mottle, material);
        welded.insert(key, v);
        v
    };
    let ni = n as i32;
    for axis_i in 0..3 {
        let (u, w) = ((axis_i + 1) % 3, (axis_i + 2) % 3);
        for side in [0, ni] {
            for a in 0..ni {
                for c in 0..ni {
                    let at = |da: i32, dc: i32| {
                        let mut k = [0i32; 3];
                        k[axis_i] = side;
                        k[u] = a + da;
                        k[w] = c + dc;
                        (k[0], k[1], k[2])
                    };
                    let (p0, p1, p2, p3) = (point(b, at(0, 0)), point(b, at(1, 0)), point(b, at(1, 1)), point(b, at(0, 1)));
                    if side != 0 { b.quad(p0, p1, p2, p3) } else { b.quad(p0, p3, p2, p1) }
                }
            }
        }
    }
    b.smooth_normals(start);
    // Outline: a few loops around the rock.
    let _ = start;
}

fn rubble(ctx: &mut Ctx, kit: &Kit) {
    let diameter = Kit::f(kit.width, 1.6);
    let height = Kit::f(kit.height, 0.45);
    let count = kit.count.unwrap_or(40) as usize;
    let color = super::color(kit.color, [0.36, 0.34, 0.31]);
    let damage = Kit::f(kit.damage, 0.6);
    let m = ctx.stone();
    let mortar = ctx.mat("grout");
    let radius = diameter * 0.5;
    // The mound under the pieces: a low dome of dirt and grit.
    {
        let mound = RockStyle { rough: 0.9, strata: 0.0, facets: 0, res: 10 };
        rock(&mut ctx.b, Vec3::new(0.0, -height * 0.45, 0.0), Vec3::new(diameter * 0.95, height * 1.3, diameter * 0.9), Quat::IDENTITY, &mound, ctx.seed ^ 0x33, color * 0.55, mortar);
    }
    for i in 0..count {
        let mut r = Rand::of(ctx.seed, 60_000 + i as u64);
        // Bigger pieces lie lower and further out.
        let u = r.f();
        let d = radius * u.sqrt() * 0.9;
        let a = r.f() * std::f32::consts::TAU;
        let top = height * (1.0 - (d / radius).powi(2)).max(0.0);
        let s = (0.06 + 0.22 * r.f().powf(1.5)) * (0.6 + 0.6 * (d / radius)) * (diameter / 1.6).sqrt();
        let p = Vec3::new(a.cos() * d, top - s * 0.35, a.sin() * d);
        let q = Quat::from_euler(glam::EulerRot::YXZ, r.f() * std::f32::consts::TAU, r.signed() * 0.6, r.signed() * 0.6);
        let tone = color * r.range(0.8, 1.1);
        if r.chance(0.55) {
            // A broken building stone.
            let dims = Vec3::new(s * r.range(1.2, 2.2), s * r.range(0.6, 1.0), s * r.range(0.8, 1.4));
            let cs = aabb(p - dims * 0.5, p + dims * 0.5).map(|c| p + q * (c - p));
            let style = Style { chips: 3 + (damage * 4.0) as u32, chip_depth: 0.2 + 0.3 * damage, cell: ctx.cell * 1.3, ..ctx.style(r.next(), tone, m, kit) };
            stone::block(&mut ctx.b, cs, &style);
        } else {
            let style = RockStyle { rough: 0.6, strata: 0.1, facets: 5, res: 5 };
            rock(&mut ctx.b, p - Vec3::Y * s * 0.2, Vec3::new(s * 1.3, s, s * 1.1), q, &style, r.next(), tone, m);
        }
    }
}

// ------------------------------------------------------------------ props

/// A surface of revolution around `axis` (from `base`): `profile` is
/// (radius, height) from bottom to top; `color(t)` by height share.
#[allow(clippy::too_many_arguments)]
fn lathe(b: &mut Builder, base: Vec3, axis: Vec3, profile: &[(f32, f32)], segments: usize, material: u32, color: &dyn Fn(f32, f32) -> Vec3, wobble: &dyn Fn(f32, f32) -> f32) {
    let start = b.mesh.vertices.len() as u32;
    let frame = crate::film::shade::Frame::new(axis.normalize_or(Vec3::Y));
    let rows = profile.len();
    let top = profile.last().map_or(1.0, |p| p.1).max(1e-6);
    for (i, &(r, h)) in profile.iter().enumerate() {
        let _ = i;
        for s in 0..segments {
            let a = s as f32 / segments as f32 * std::f32::consts::TAU;
            let rr = r * wobble(a, h / top);
            let p = base + frame.t * (a.cos() * rr) + frame.b * (a.sin() * rr) + frame.n * h;
            let out = frame.t * a.cos() + frame.b * a.sin();
            b.vertex(p, out, color(h / top, a), material);
        }
    }
    for i in 0..rows - 1 {
        for s in 0..segments {
            let s1 = (s + 1) % segments;
            let a = start + (i * segments + s) as u32;
            let bb = start + (i * segments + s1) as u32;
            let c = start + ((i + 1) * segments + s1) as u32;
            let d = start + ((i + 1) * segments + s) as u32;
            b.quad(a, bb, c, d);
        }
    }
    // Caps where the profile does not close.
    for (row, up) in [(0usize, false), (rows - 1, true)] {
        if profile[row].0 > 1e-5 {
            let h = profile[row].1;
            let center = b.vertex(base + frame.n * h, if up { frame.n } else { -frame.n }, color(h / top, 0.0), material);
            for s in 0..segments {
                let s1 = (s + 1) % segments;
                let (p, q) = (start + (row * segments + s) as u32, start + (row * segments + s1) as u32);
                if up { b.tri(center, p, q) } else { b.tri(center, q, p) }
            }
        }
    }
    b.smooth_normals(start as usize);
    b.edge(start, start + (segments / 2) as u32);
}

/// A flame: a glowing teardrop leaning a little, white-yellow at the
/// heart and orange at the tip.
fn flame(ctx: &mut Ctx, base: Vec3, height: f32, width: f32, lean: Vec3, seed: u32) {
    let m = ctx.mat("flame");
    let rows = 9;
    let profile: Vec<(f32, f32)> = (0..=rows).map(|i| {
        let t = i as f32 / rows as f32;
        let r = width * 0.5 * (std::f32::consts::PI * t.powf(0.7)).sin().max(0.0).powf(0.8) * (1.0 - 0.35 * t);
        (if i == rows { 0.0 } else { r.max(width * 0.06) }, t * height)
    }).collect();
    let axis = (Vec3::Y + lean).normalize();
    // A hot yellow heart at the root fading to deep orange at the tip.
    let color = |t: f32, _a: f32| Vec3::new(1.0, 0.78 - 0.48 * t, 0.36 - 0.33 * t);
    let wob = move |a: f32, t: f32| 1.0 + 0.25 * t * noise(Vec3::new(a.cos() * 2.0, t * 3.0, a.sin() * 2.0), seed);
    lathe(&mut ctx.b, base, axis, &profile, 10, m, &color, &wob);
}

/// A straight round bar from `a` to `b`.
fn bar(ctx: &mut Ctx, a: Vec3, b: Vec3, r0: f32, r1: f32, material: u32, color: Vec3) {
    let len = a.distance(b);
    let profile = [(r0, 0.0), (r1, len)];
    lathe(&mut ctx.b, a, b - a, &profile, 8, material, &|_, _| color, &|_, _| 1.0);
}

fn torch(ctx: &mut Ctx, kit: &Kit) {
    let iron = ctx.mat("iron");
    let wood = ctx.mat("wood");
    let cloth = ctx.mat("coal");
    let iron_c = super::color(kit.color, [0.2, 0.19, 0.18]);
    let stand = kit.kind_name() == "stand";
    let lit = kit.lit.unwrap_or(true);
    // The torch's own frame: wall torches stick out along +Z from z = 0.
    let (base, tilt) = if stand { (Vec3::new(0.0, 1.25, 0.0), Vec3::ZERO) } else { (Vec3::new(0.0, -0.05, 0.12), Vec3::new(0.0, 0.0, 0.18)) };
    if stand {
        bar(ctx, Vec3::ZERO + Vec3::Y * 0.05, base, 0.018, 0.015, iron, iron_c);
        for k in 0..3 {
            let a = k as f32 / 3.0 * std::f32::consts::TAU;
            bar(ctx, Vec3::new(a.cos() * 0.28, 0.0, a.sin() * 0.28), Vec3::Y * 0.35, 0.012, 0.012, iron, iron_c);
        }
        lathe(&mut ctx.b, base - Vec3::Y * 0.02, Vec3::Y, &[(0.02, 0.0), (0.075, 0.08), (0.08, 0.1)], 12, iron, &|_, _| iron_c, &|_, _| 1.0);
    } else {
        stone::plain(&mut ctx.b, Vec3::new(-0.045, -0.1, 0.0), Vec3::new(0.045, 0.08, 0.018), iron_c, iron);
        bar(ctx, Vec3::new(0.0, -0.06, 0.012), Vec3::new(0.0, -0.06, 0.13), 0.009, 0.009, iron, iron_c);
        bar(ctx, Vec3::new(0.0, 0.05, 0.012), Vec3::new(0.0, 0.05, 0.15), 0.008, 0.008, iron, iron_c);
        lathe(&mut ctx.b, Vec3::new(0.0, 0.03, 0.15), Vec3::Y + tilt, &[(0.034, 0.0), (0.038, 0.03)], 12, iron, &|_, _| iron_c, &|_, _| 1.0);
    }
    // The handle, then the pitch-soaked head.
    let up = (Vec3::Y + tilt).normalize();
    let handle0 = base - up * if stand { 0.0 } else { 0.18 };
    let head = base + up * 0.22;
    bar(ctx, handle0, head, 0.02, 0.026, wood, Vec3::new(0.38, 0.25, 0.15));
    let head_c = Vec3::new(0.12, 0.09, 0.07);
    lathe(&mut ctx.b, head, up, &[(0.03, 0.0), (0.042, 0.03), (0.044, 0.07), (0.034, 0.095)], 12, cloth, &|t, a| head_c * (0.8 + 0.4 * (a * 5.0 + t * 9.0).sin().abs()), &|a, t| 1.0 + 0.08 * (a * 7.0 + t * 11.0).sin());
    if lit {
        let top = head + up * 0.075;
        let seed = ctx.seed as u32;
        flame(ctx, top, 0.24, 0.085, Vec3::new(0.02, 0.0, 0.03), seed);
        flame(ctx, top + Vec3::new(0.02, 0.0, 0.0), 0.16, 0.05, Vec3::new(0.12, 0.0, -0.05), seed ^ 1);
        flame(ctx, top + Vec3::new(-0.018, 0.0, 0.01), 0.18, 0.05, Vec3::new(-0.1, 0.0, 0.06), seed ^ 2);
        let ember = ctx.mat("ember");
        lathe(&mut ctx.b, top - up * 0.012, up, &[(0.035, 0.0), (0.03, 0.02), (0.0, 0.03)], 10, ember, &|_, _| Vec3::new(1.0, 0.38, 0.08), &|a, _| 1.0 + 0.1 * (a * 6.0).sin());
    }
}

fn candles(ctx: &mut Ctx, kit: &Kit) {
    let count = kit.count.unwrap_or(1) as usize;
    let tall = Kit::f(kit.height, 0.18);
    let wax = ctx.mat("wax");
    let wick = ctx.mat("coal");
    let color = super::color(kit.color, [0.86, 0.8, 0.66]);
    let lit = kit.lit.unwrap_or(true);
    let mut rng = Rand::new(ctx.seed ^ 0xCA);
    let spread = if count > 1 { 0.03 + 0.016 * count as f32 } else { 0.0 };
    if count > 1 {
        // A puddle of old wax under the cluster.
        let seed = ctx.seed as u32;
        lathe(&mut ctx.b, Vec3::ZERO, Vec3::Y, &[(spread + 0.03, 0.0), (spread + 0.026, 0.004), (spread + 0.01, 0.008)], 28, wax, &|_, _| color * 0.8, &move |a, _| 1.0 + 0.22 * noise(Vec3::new(a.cos() * 1.6, 0.0, a.sin() * 1.6), seed));
    }
    for i in 0..count {
        let a = i as f32 * 2.399 + rng.f();
        let d = if i == 0 { 0.0 } else { spread * rng.range(0.4, 1.0) };
        let at = Vec3::new(a.cos() * d, if count > 1 { 0.008 } else { 0.0 }, a.sin() * d);
        let h = tall * if i == 0 { 1.0 } else { rng.range(0.35, 0.85) };
        let r = 0.02 + 0.008 * rng.f();
        let melt = rng.range(0.004, 0.012);
        let profile = [(r * 1.08, 0.0), (r, 0.01), (r, h * 0.6), (r * 1.02, h - melt * 2.0), (r * 0.98, h - melt), (r * 0.75, h - melt * 0.6), (r * 0.4, h - melt * 0.9)];
        let seed = rng.next() as u32;
        lathe(&mut ctx.b, at, Vec3::Y, &profile, 14, wax, &|t, _| color * (0.9 + 0.1 * t), &move |ang, t| 1.0 + 0.06 * t * noise(Vec3::new(ang.cos() * 3.0, t * 4.0, ang.sin() * 3.0), seed));
        // Drips down the side.
        for k in 0..2 {
            let da = rng.f() * std::f32::consts::TAU;
            let len = h * rng.range(0.15, 0.4);
            let p = at + Vec3::new(da.cos() * r * 0.97, h - melt - len, da.sin() * r * 0.97);
            let _ = k;
            lathe(&mut ctx.b, p, Vec3::Y, &[(0.0, 0.0), (0.004, 0.004), (0.005, len * 0.6), (0.0, len)], 6, wax, &|_, _| color * 0.97, &|_, _| 1.0);
        }
        let top = at + Vec3::Y * (h - melt * 0.9);
        bar(ctx, top - Vec3::Y * 0.003, top + Vec3::Y * 0.012, 0.0015, 0.001, wick, Vec3::splat(0.05));
        if lit {
            let s = rng.next() as u32;
            flame(ctx, top + Vec3::Y * 0.008, 0.035, 0.014, Vec3::new(rng.signed() * 0.05, 0.0, rng.signed() * 0.05), s);
        }
    }
}

fn brazier(ctx: &mut Ctx, kit: &Kit) {
    let height = Kit::f(kit.height, 0.85);
    let w = Kit::f(kit.width, 0.62);
    let iron = ctx.mat("iron");
    let coal = ctx.mat("coal");
    let ember = ctx.mat("ember");
    let iron_c = super::color(kit.color, [0.2, 0.18, 0.16]);
    let lit = kit.lit.unwrap_or(true);
    let rim = w * 0.5;
    let bowl_bottom = height - 0.22;
    // The bowl: outside, lip, inside.
    let profile = [(0.0, 0.0), (rim * 0.35, 0.0), (rim * 0.7, 0.07), (rim * 0.95, 0.17), (rim, 0.22), (rim * 0.94, 0.225), (rim * 0.88, 0.17), (rim * 0.62, 0.08), (0.0, 0.05)];
    lathe(&mut ctx.b, Vec3::Y * bowl_bottom, Vec3::Y, &profile, 24, iron, &|_, _| iron_c, &|a, t| 1.0 + 0.01 * (a * 9.0 + t).sin());
    // Three legs with feet.
    for k in 0..3 {
        let a = k as f32 / 3.0 * std::f32::consts::TAU + 0.3;
        let top = Vec3::new(a.cos() * rim * 0.6, bowl_bottom + 0.05, a.sin() * rim * 0.6);
        let foot = Vec3::new(a.cos() * rim * 0.95, 0.03, a.sin() * rim * 0.95);
        bar(ctx, top, foot, 0.018, 0.014, iron, iron_c);
        lathe(&mut ctx.b, foot - Vec3::Y * 0.03, Vec3::Y, &[(0.03, 0.0), (0.028, 0.025), (0.0, 0.035)], 8, iron, &|_, _| iron_c, &|_, _| 1.0);
    }
    // A ring tying the legs.
    let ring_y = height * 0.3;
    for k in 0..3 {
        let a0 = k as f32 / 3.0 * std::f32::consts::TAU + 0.3;
        let a1 = (k + 1) as f32 / 3.0 * std::f32::consts::TAU + 0.3;
        let t = (ring_y - 0.03) / (bowl_bottom + 0.02);
        let p = |a: f32| Vec3::new(a.cos(), 0.0, a.sin()) * (rim * (0.95 - 0.35 * t)) + Vec3::Y * ring_y;
        bar(ctx, p(a0), p(a1), 0.008, 0.008, iron, iron_c);
    }
    // Coals heaped in the bowl, glowing between.
    let mut rng = Rand::new(ctx.seed ^ 0xB3);
    // A bed of ash fills the bowl; coals heap on it.
    let ash = RockStyle { rough: 0.5, strata: 0.0, facets: 0, res: 8 };
    rock(&mut ctx.b, Vec3::Y * (bowl_bottom + 0.08), Vec3::new(rim * 1.75, 0.12, rim * 1.75), Quat::IDENTITY, &ash, ctx.seed ^ 0xA5, Vec3::splat(0.05), coal);
    let style = RockStyle { rough: 0.7, strata: 0.0, facets: 4, res: 4 };
    for i in 0..48 {
        let d = rim * 0.8 * rng.f().sqrt();
        let a = rng.f() * std::f32::consts::TAU;
        let s = rng.range(0.045, 0.085);
        let y = bowl_bottom + 0.11 + 0.06 * (1.0 - d / rim) + rng.f() * 0.02;
        let glowing = lit && rng.chance(0.3);
        let c = if glowing { Vec3::new(1.0, 0.32 + rng.f() * 0.15, 0.06) } else { Vec3::splat(0.035 + rng.f() * 0.03) };
        rock(&mut ctx.b, Vec3::new(a.cos() * d, y, a.sin() * d), Vec3::new(s * 1.2, s * 0.8, s), Quat::from_rotation_y(rng.f() * 6.0), &style, i as u64 + ctx.seed, c, if glowing { ember } else { coal });
    }
    if lit {
        let seed = ctx.seed as u32;
        let top = Vec3::Y * (bowl_bottom + 0.16);
        flame(ctx, top, 0.46, 0.12, Vec3::new(0.02, 0.0, 0.0), seed);
        for k in 0..7 {
            let a = k as f32 * 0.9 + rng.f() * 0.4;
            let p = top + Vec3::new(a.cos(), 0.0, a.sin()) * rim * rng.range(0.2, 0.55);
            flame(ctx, p, rng.range(0.16, 0.34), rng.range(0.05, 0.085), Vec3::new(a.cos(), 0.0, a.sin()) * 0.2, seed ^ (k + 3));
        }
    }
}

fn bones(ctx: &mut Ctx, kit: &Kit) {
    let color = super::color(kit.color, [0.78, 0.72, 0.6]);
    let kind = kit.kind_name().to_string();
    let mut rng = Rand::new(ctx.seed ^ 0xB0);
    let mut parts: Vec<(&str, Mat4)> = Vec::new();
    match kind.as_str() {
        "pile" => {
            parts.push(("skull", Mat4::from_rotation_translation(Quat::from_rotation_y(rng.signed()), Vec3::new(0.05, 0.02, 0.0))));
            for i in 0..6 {
                let a = i as f32 * 1.1 + rng.f();
                let q = Quat::from_euler(glam::EulerRot::YXZ, a, rng.signed() * 0.3, rng.signed() * 0.2);
                parts.push((if i % 3 == 2 { "ribs" } else { "femur" }, Mat4::from_rotation_translation(q, Vec3::new(a.cos() * 0.12, 0.01 * i as f32, a.sin() * 0.12))));
            }
        }
        k => parts.push((match k { "skull" => "skull", "ribs" => "ribs", _ => "femur" }, Mat4::IDENTITY)),
    }
    let bone = ctx.mats.get("bone");
    for (part, m) in parts {
        let mesh = bone_mesh(part, color, ctx.seed, &bone);
        let tint = Vec3::splat(rng.range(0.9, 1.05));
        ctx.b.append(&mesh, m, tint);
    }
}

/// One bone made of clay blobs (smooth knobs and shafts).
fn bone_mesh(part: &str, color: Vec3, seed: u64, material: &Material) -> Mesh {
    let c = color.to_array();
    let sphere = |x: f32, y: f32, z: f32, r: f32| serde_json::json!({"type":"sphere","center":[x,y,z],"radius":r,"color":c,"blend":0.012});
    let blobs = match part {
        "skull" => serde_json::json!([
            {"type":"ellipsoid","center":[0,0.092,-0.012],"radii":[0.07,0.074,0.09],"color":c},
            {"type":"ellipsoid","center":[0,0.06,0.05],"radii":[0.055,0.05,0.045],"color":c,"blend":0.02},
            {"type":"ellipsoid","center":[0.046,0.055,0.045],"radii":[0.016,0.012,0.02],"color":c,"blend":0.012},
            {"type":"ellipsoid","center":[-0.046,0.055,0.045],"radii":[0.016,0.012,0.02],"color":c,"blend":0.012},
            {"type":"box","center":[0,0.024,0.058],"size":[0.06,0.03,0.05],"round":0.012,"color":c,"blend":0.015},
            {"type":"box","center":[0,0.03,0.083],"size":[0.046,0.012,0.008],"round":0.003,"color":[0.86,0.82,0.7],"blend":0.004},
            {"type":"sphere","center":[0.03,0.075,0.088],"radius":0.023,"op":"carve","blend":0.006,"color":[0.06,0.05,0.04]},
            {"type":"sphere","center":[-0.03,0.075,0.088],"radius":0.023,"op":"carve","blend":0.006,"color":[0.06,0.05,0.04]},
            {"type":"ellipsoid","center":[0,0.048,0.094],"radii":[0.01,0.016,0.025],"op":"carve","blend":0.004,"color":[0.06,0.05,0.04]},
            {"type":"ellipsoid","center":[0.05,0.06,-0.02],"radii":[0.012,0.012,0.012],"op":"carve","blend":0.004,"color":[0.12,0.1,0.08]},
        ]),
        "ribs" => {
            // A spine with pairs of ribs arching up and out to both sides.
            let mut v = vec![serde_json::json!({"type":"chain","points":[[-0.16,0.016,0],[0.0,0.02,0],[0.16,0.016,0]],"size":[0.022,0.024,0.024],"color":c,"blend":0.006})];
            for k in 0..5 {
                let x = -0.11 + 0.055 * k as f32;
                let span = 0.11 + 0.02 * (2.0 - (k as f32 - 2.0).abs());
                for side in [-1.0f32, 1.0] {
                    let pts: Vec<[f32; 3]> = (0..6).map(|i| {
                        let t = i as f32 / 5.0;
                        let a = t * 2.3;
                        [x - 0.03 * t, 0.02 + a.sin() * 0.05, side * (1.0 - a.cos()) * span * 0.62]
                    }).collect();
                    v.push(serde_json::json!({"type":"chain","points":pts,"size":[0.008,0.012,0.022],"tilt":0,"color":c,"blend":0.005}));
                }
            }
            serde_json::Value::Array(v)
        }
        _ => serde_json::json!([
            {"type":"capsule","a":[-0.17,0.022,0],"b":[0.17,0.022,0],"radius":0.014,"radius_b":0.016,"color":c},
            sphere(0.205, 0.03, 0.0, 0.026), sphere(0.19, 0.022, 0.022, 0.017),
            sphere(-0.2, 0.024, 0.016, 0.022), sphere(-0.2, 0.024, -0.016, 0.022),
        ]),
    };
    let clay: crate::clay::Clay = match serde_json::from_value(serde_json::json!({"blobs": blobs, "resolution": if part == "skull" { 44 } else { 56 }})) {
        Ok(c) => c,
        Err(_) => return Mesh::default(),
    };
    let mut mesh = clay.build_mesh_with(&[], "");
    let _ = seed;
    mesh.materials = vec![material.clone()];
    for v in &mut mesh.vertices {
        v.material = 0;
        v.surface = material.surface();
        v.light = material.light();
    }
    mesh
}

fn chain(ctx: &mut Ctx, kit: &Kit) {
    let length = Kit::f(kit.length, 1.2);
    let link = Kit::f(kit.block, 0.07);
    let iron = ctx.mat("iron");
    let color = super::color(kit.color, [0.22, 0.2, 0.18]);
    let wire = link * 0.13;
    let pitch = link - wire * 2.2;
    let n = ((length / pitch).round() as usize).clamp(1, 200);
    let mut rng = Rand::new(ctx.seed ^ 0xC4);
    // A wall ring at the top, then links hanging straight down.
    for i in 0..n {
        let center = Vec3::new(0.0, -(i as f32 + 0.5) * pitch, 0.0);
        let turn_y = if i % 2 == 0 { 0.0 } else { std::f32::consts::FRAC_PI_2 } + rng.signed() * 0.15;
        let q = Quat::from_rotation_y(turn_y);
        link_mesh(&mut ctx.b, center, q, link, wire, iron, color * rng.range(0.85, 1.1));
    }
    let shackle = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    link_mesh(&mut ctx.b, Vec3::new(0.0, 0.02, -0.01), shackle, link * 1.1, wire * 1.2, iron, color);
}

/// One oval chain link in its XY plane (long along Y), turned by `q`.
fn link_mesh(b: &mut Builder, center: Vec3, q: Quat, length: f32, wire: f32, material: u32, color: Vec3) {
    let start = b.mesh.vertices.len() as u32;
    let (ring, tube) = (16usize, 6usize);
    let half_straight = (length * 0.5 - length * 0.3).max(0.0);
    let bend = length * 0.3 - wire;
    for i in 0..ring {
        let t = i as f32 / ring as f32;
        // A stadium path: straight sides joined by half circles.
        let a = t * std::f32::consts::TAU;
        let (cx, cy) = (a.cos() * bend, a.sin() * bend + if a.sin() >= 0.0 { half_straight } else { -half_straight });
        let along = Vec3::new(-a.sin(), a.cos(), 0.0);
        let out = Vec3::new(a.cos(), a.sin(), 0.0);
        let _ = along;
        for j in 0..tube {
            let bphi = j as f32 / tube as f32 * std::f32::consts::TAU;
            let n = out * bphi.cos() + Vec3::Z * bphi.sin();
            let p = Vec3::new(cx, cy, 0.0) + n * wire;
            b.vertex(center + q * p, q * n, color, material);
        }
    }
    for i in 0..ring {
        for j in 0..tube {
            let (i1, j1) = ((i + 1) % ring, (j + 1) % tube);
            let at = |i: usize, j: usize| start + (i * tube + j) as u32;
            b.quad(at(i, j), at(i1, j), at(i1, j1), at(i, j1));
        }
    }
    b.edge(start, start + (ring / 2 * tube) as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(kit: Kit) -> Mesh {
        kit.build(&[], None)
    }

    #[test]
    fn pieces_snap_to_the_grid_and_meet() {
        // Lengths snap to whole cells.
        let mut k = Kit { length: Some(2.4), ..Kit::new(Piece::Wall) };
        k.snap();
        assert_eq!(k.length, Some(2.0));
        let mut f = Kit { length: Some(0.2), depth: Some(3.6), ..Kit::new(Piece::Floor) };
        f.snap();
        assert_eq!((f.length, f.depth), (Some(1.0), Some(4.0)));
        // A 2 m wall ends at +-1 m, flush with the grid, 3 m tall, 0.5 m thick.
        let (lo, hi) = build(Kit::new(Piece::Wall)).bounds();
        assert!((lo.x + 1.0).abs() < 0.03 && (hi.x - 1.0).abs() < 0.03, "{lo} {hi}");
        assert!(lo.y > -0.02 && (hi.y - 3.0).abs() < 0.05, "{lo} {hi}");
        assert!((hi.z - 0.25).abs() < 0.04 && (lo.z + 0.25).abs() < 0.04, "{lo} {hi}");
        // A 4 x 2 floor covers its cells with stone tops at y = 0.
        let floor = build(Kit { length: Some(4.0), depth: Some(2.0), ..Kit::new(Piece::Floor) });
        let (lo, hi) = floor.bounds();
        assert!((lo.x + 2.0).abs() < 0.03 && (hi.x - 2.0).abs() < 0.03 && (lo.z + 1.0).abs() < 0.03 && (hi.z - 1.0).abs() < 0.03, "{lo} {hi}");
        assert!(hi.y.abs() < 0.04, "floor top {hi}");
        // A corner covers both wall ends that meet on its grid point.
        let (clo, chi) = build(Kit::new(Piece::Corner)).bounds();
        assert!(clo.x < -0.25 && clo.z < -0.25 && chi.x > 0.25 && chi.z > 0.25, "{clo} {chi}");
        // A doorway is open in the middle, below the spring line.
        let door = build(Kit::new(Piece::Doorway));
        let blocked = door.indices.chunks_exact(3).any(|t| t.iter().all(|&i| {
            let p = Vec3::from(door.vertices[i as usize].pos);
            p.x.abs() < 0.4 && p.y > 0.2 && p.y < 1.8
        }));
        assert!(!blocked, "the doorway's opening is filled");
        let (dlo, dhi) = door.bounds();
        assert!((dlo.x + 1.5).abs() < 0.03 && (dhi.x - 1.5).abs() < 0.03);
    }

    #[test]
    fn every_piece_builds_a_valid_mesh() {
        for piece in Piece::ALL {
            let m = build(Kit::new(piece));
            assert!(!m.indices.is_empty(), "{piece:?} is empty");
            assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
            assert!(m.vertices.iter().all(|v| Vec3::from(v.pos).is_finite() && Vec3::from(v.normal).is_finite()), "{piece:?}");
            assert!(m.vertices.iter().all(|v| (v.material as usize) < m.materials.len()));
            // Same settings, same mesh; another seed, other stones.
            let again = build(Kit::new(piece));
            assert_eq!(again.vertices.len(), m.vertices.len());
            let other = build(Kit { seed: 7, ..Kit::new(piece) });
            if !matches!(piece, Piece::Bones | Piece::Chain | Piece::Torch) {
                assert!(other.vertices.len() != m.vertices.len() || other.vertices.iter().zip(&m.vertices).any(|(a, b)| a.pos != b.pos), "{piece:?} ignores its seed");
            }
        }
        let lit = build(Kit::new(Piece::Torch));
        assert!(lit.materials.iter().any(|m| m.name == "flame"));
        assert!(Piece::Torch.light().is_some() && Piece::Wall.light().is_none());
    }
}
