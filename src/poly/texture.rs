//! UDIM texture tiles of a Poly (docs/QUALITY_PLAN.md Stage 4b, D19).
//!
//! A Poly's UVs may span tiles 1001-1100: tile `1001 + u + 10 v` holds
//! the UVs whose whole parts are (u, v), u and v in 0..10, with v counted
//! down the image as the Poly's UVs are (top-left origin; the OBJ and
//! USDA writers flip each tile, see `uv::v_out`). Each channel (`color`,
//! `roughness`, `mask` that strokes paint, `normal` and `ao` that a bake
//! writes) has one resolution for all its tiles and keeps each tile as
//! 256 x 256 pages of 16-bit RGBA texels, made on the first write
//! (sparse) and shared by `Arc` (an undo snapshot or a scene clone shares
//! every page an edit did not touch).
//!
//! Texels are straight alpha: RGB the value (sRGB for `color`, linear 0-1
//! for the others, a scalar repeated in R, G and B), A how much of it
//! covers the material underneath (paint's coverage; 1 where a bake
//! wrote). Every size is bounded before anything is allocated: at most
//! 100 tiles, a resolution of 256-8192, and `MAX_PAGES` pages (4 GiB)
//! over all channels.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod paint;

/// Texels per page side.
pub const PAGE: u32 = 256;
/// Texels per page.
pub const PAGE_TEXELS: usize = (PAGE * PAGE) as usize;
/// Bytes per page (RGBA, 16 bits each).
pub const PAGE_BYTES: u64 = PAGE_TEXELS as u64 * 8;
/// The first tile and how many there may be (1001-1100, 10 a row).
pub const FIRST_TILE: u16 = 1001;
pub const MAX_TILES: u32 = 100;
/// Tile resolutions (texels per side): page multiples, powers of two.
pub const RESOLUTIONS: [u32; 6] = [256, 512, 1024, 2048, 4096, 8192];
pub const DEFAULT_RESOLUTION: u32 = 2048;
/// The most texel memory a Poly may hold, over all its channels (§6).
pub const MAX_BYTES: u64 = 4 << 30;
/// `MAX_BYTES` in pages.
pub const MAX_PAGES: usize = (MAX_BYTES / PAGE_BYTES) as usize;

/// What a channel holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Color,
    Roughness,
    Mask,
    Normal,
    Ao,
}

impl Channel {
    pub const ALL: [Channel; 5] = [Channel::Color, Channel::Roughness, Channel::Mask, Channel::Normal, Channel::Ao];

    pub fn name(self) -> &'static str {
        match self {
            Channel::Color => "color",
            Channel::Roughness => "roughness",
            Channel::Mask => "mask",
            Channel::Normal => "normal",
            Channel::Ao => "ao",
        }
    }

    pub fn parse(s: &str) -> Option<Channel> {
        Channel::ALL.into_iter().find(|c| c.name() == s)
    }

    /// Strokes paint it (the others are written by `bake udim`).
    pub fn paintable(self) -> bool {
        matches!(self, Channel::Color | Channel::Roughness | Channel::Mask)
    }

    /// One value per texel (in R, G and B alike).
    pub fn scalar(self) -> bool {
        matches!(self, Channel::Roughness | Channel::Mask | Channel::Ao)
    }
}

/// The tile a UV lies in (its whole parts, u and v in 0..10), None
/// outside tiles 1001-1100.
pub fn tile_of(uv: [f32; 2]) -> Option<u16> {
    let (u, v) = (uv[0].floor(), uv[1].floor());
    ((0.0..10.0).contains(&u) && (0.0..10.0).contains(&v)).then(|| FIRST_TILE + u as u16 + 10 * v as u16)
}

/// A tile's UV origin (its whole parts).
pub fn tile_origin(tile: u16) -> [f32; 2] {
    let i = tile.saturating_sub(FIRST_TILE);
    [(i % 10) as f32, (i / 10) as f32]
}

/// Tile numbers 1001-1100.
pub fn valid_tile(tile: u16) -> bool {
    (FIRST_TILE..FIRST_TILE + MAX_TILES as u16).contains(&tile)
}

/// A page's texels (row-major within the page).
type PageData = Arc<Vec<[u16; 4]>>;

/// One tile of a channel: `side x side` page slots, row-major.
#[derive(Clone, Debug, Default)]
pub struct Tile {
    pages: Vec<Option<PageData>>,
}

impl Tile {
    fn new(resolution: u32) -> Tile {
        let side = (resolution / PAGE) as usize;
        Tile { pages: vec![None; side * side] }
    }

    /// Pages holding texels.
    pub fn page_count(&self) -> usize {
        self.pages.iter().filter(|p| p.is_some()).count()
    }
}

/// One channel: its resolution and its tiles.
#[derive(Clone, Debug)]
pub struct Layer {
    pub resolution: u32,
    tiles: BTreeMap<u16, Tile>,
}

/// A Poly's texture channels (see the module doc). `==` compares pages
/// by pointer first.
#[derive(Clone, Debug, Default)]
pub struct Textures {
    layers: BTreeMap<Channel, Layer>,
}

impl PartialEq for Textures {
    fn eq(&self, other: &Self) -> bool {
        self.layers.len() == other.layers.len()
            && self.layers.iter().zip(&other.layers).all(|((ca, a), (cb, b))| {
                ca == cb
                    && a.resolution == b.resolution
                    && a.tiles.len() == b.tiles.len()
                    && a.tiles.iter().zip(&b.tiles).all(|((ta, x), (tb, y))| {
                        ta == tb
                            && x.pages.len() == y.pages.len()
                            && x.pages.iter().zip(&y.pages).all(|(p, q)| match (p, q) {
                                (Some(p), Some(q)) => Arc::ptr_eq(p, q) || p == q,
                                (None, None) => true,
                                _ => false,
                            })
                    })
            })
    }
}

/// Checks a requested resolution.
pub fn check_resolution(resolution: u32) -> Result<(), String> {
    if RESOLUTIONS.contains(&resolution) { Ok(()) } else { Err(format!("resolution must be one of {RESOLUTIONS:?} texels per tile side")) }
}

impl Textures {
    pub fn is_empty(&self) -> bool {
        self.layers.values().all(|l| l.tiles.values().all(|t| t.page_count() == 0))
    }

    /// The channels that hold texels, in order.
    pub fn channels(&self) -> Vec<Channel> {
        self.layers.iter().filter(|(_, l)| l.tiles.values().any(|t| t.page_count() > 0)).map(|(c, _)| *c).collect()
    }

    pub fn resolution(&self, channel: Channel) -> Option<u32> {
        self.layers.get(&channel).map(|l| l.resolution)
    }

    /// The tiles of a channel that hold texels.
    pub fn tiles(&self, channel: Channel) -> Vec<u16> {
        self.layers.get(&channel).map(|l| l.tiles.iter().filter(|(_, t)| t.page_count() > 0).map(|(k, _)| *k).collect()).unwrap_or_default()
    }

    /// Pages held, over all channels.
    pub fn pages(&self) -> usize {
        self.layers.values().flat_map(|l| l.tiles.values()).map(Tile::page_count).sum()
    }

    /// Bytes of texels held.
    pub fn bytes(&self) -> u64 {
        self.pages() as u64 * PAGE_BYTES
    }

    /// Bytes of pages `other` does not share (by pointer): what an undo
    /// snapshot holding only this costs.
    pub fn unshared_bytes(&self, other: Option<&Textures>) -> usize {
        let mut total = 0;
        for (c, l) in &self.layers {
            for (k, t) in &l.tiles {
                let theirs = other.and_then(|o| o.layers.get(c)).filter(|o| o.resolution == l.resolution).and_then(|o| o.tiles.get(k));
                for (i, p) in t.pages.iter().enumerate() {
                    let Some(p) = p else { continue };
                    let shared = theirs.and_then(|t| t.pages.get(i)).and_then(|q| q.as_ref()).is_some_and(|q| Arc::ptr_eq(p, q));
                    if !shared {
                        total += PAGE_BYTES as usize;
                    }
                }
            }
        }
        total
    }

    /// Makes `channel` take `resolution`: refused when it already holds
    /// texels at another one (no resampling).
    pub fn set_resolution(&mut self, channel: Channel, resolution: u32) -> Result<(), String> {
        check_resolution(resolution)?;
        if let Some(l) = self.layers.get(&channel) {
            if l.resolution == resolution {
                return Ok(());
            }
            if l.tiles.values().any(|t| t.page_count() > 0) {
                return Err(format!("the {} channel is {} texels a tile; use that resolution (or clear the channel first)", channel.name(), l.resolution));
            }
        }
        self.layers.insert(channel, Layer { resolution, tiles: BTreeMap::new() });
        Ok(())
    }

    /// Drops a channel (or every channel).
    pub fn clear(&mut self, channel: Option<Channel>) {
        match channel {
            Some(c) => {
                self.layers.remove(&c);
            }
            None => self.layers.clear(),
        }
    }

    /// The texel at `(x, y)` of a tile (zero where nothing was written).
    #[cfg_attr(not(test), expect(dead_code, reason = "tests read single texels; tools read tiles and samples"))]
    pub fn texel(&self, channel: Channel, tile: u16, x: u32, y: u32) -> [u16; 4] {
        let Some(l) = self.layers.get(&channel) else { return [0; 4] };
        if x >= l.resolution || y >= l.resolution {
            return [0; 4];
        }
        let side = l.resolution / PAGE;
        l.tiles
            .get(&tile)
            .and_then(|t| t.pages[((y / PAGE) * side + x / PAGE) as usize].as_ref())
            .map_or([0; 4], |p| p[((y % PAGE) * PAGE + x % PAGE) as usize])
    }

    /// Whether the page holding texel `(x, y)` of a tile exists.
    pub fn has_page(&self, channel: Channel, tile: u16, x: u32, y: u32) -> bool {
        let Some(l) = self.layers.get(&channel) else { return false };
        let side = l.resolution / PAGE;
        x < l.resolution && y < l.resolution && l.tiles.get(&tile).is_some_and(|t| t.pages[((y / PAGE) * side + x / PAGE) as usize].is_some())
    }

    /// Drops the pages holding these texels that are now all zero (an
    /// erase emptied them), and tiles left with no page.
    pub fn drop_empty(&mut self, channel: Channel, texels: impl Iterator<Item = (u16, u32, u32)>) {
        let Some(l) = self.layers.get_mut(&channel) else { return };
        let side = l.resolution / PAGE;
        let pages: std::collections::BTreeSet<(u16, usize)> = texels.map(|(t, x, y)| (t, ((y / PAGE) * side + x / PAGE) as usize)).collect();
        for (tile, page) in pages {
            let Some(t) = l.tiles.get_mut(&tile) else { continue };
            if let Some(slot) = t.pages.get_mut(page)
                && slot.as_ref().is_some_and(|p| p.iter().all(|v| *v == [0; 4]))
            {
                *slot = None;
            }
            if t.page_count() == 0 {
                l.tiles.remove(&tile);
            }
        }
    }

    /// Pages `writes` (tile, x, y) would make that are not there yet
    /// (counted before allocating).
    pub fn new_pages(&self, channel: Channel, writes: impl Iterator<Item = (u16, u32, u32)>) -> usize {
        let Some(l) = self.layers.get(&channel) else { return 0 };
        let side = l.resolution / PAGE;
        let mut seen = std::collections::BTreeSet::new();
        for (tile, x, y) in writes {
            let page = (y / PAGE) * side + x / PAGE;
            let there = l.tiles.get(&tile).is_some_and(|t| t.pages.get(page as usize).is_some_and(Option::is_some));
            if !there {
                seen.insert((tile, page));
            }
        }
        seen.len()
    }

    /// The texel at `(x, y)` of a tile, made writable (its page made, or
    /// copied when shared). The caller checked the tile, the place and
    /// the page budget.
    pub fn texel_mut(&mut self, channel: Channel, tile: u16, x: u32, y: u32) -> &mut [u16; 4] {
        let l = self.layers.get_mut(&channel).expect("set_resolution first");
        let side = l.resolution / PAGE;
        let resolution = l.resolution;
        let t = l.tiles.entry(tile).or_insert_with(|| Tile::new(resolution));
        let page = t.pages[((y / PAGE) * side + x / PAGE) as usize].get_or_insert_with(|| Arc::new(vec![[0u16; 4]; PAGE_TEXELS]));
        &mut Arc::make_mut(page)[((y % PAGE) * PAGE + x % PAGE) as usize]
    }

    /// Bilinear sample of a channel at a UV (texel centres at (i + ½) /
    /// resolution, clamped to the UV's own tile), as RGBA 0-1. None off
    /// the tiles or without the channel.
    pub fn sample(&self, channel: Channel, uv: [f32; 2]) -> Option<[f32; 4]> {
        let l = self.layers.get(&channel)?;
        let tile = tile_of(uv)?;
        let t = l.tiles.get(&tile)?;
        let o = tile_origin(tile);
        let r = l.resolution as f32;
        let (fx, fy) = ((uv[0] - o[0]) * r - 0.5, (uv[1] - o[1]) * r - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let side = l.resolution / PAGE;
        let get = |x: f32, y: f32| -> [f32; 4] {
            let (x, y) = (x.clamp(0.0, r - 1.0) as u32, y.clamp(0.0, r - 1.0) as u32);
            t.pages[((y / PAGE) * side + x / PAGE) as usize].as_ref().map_or([0.0; 4], |p| p[((y % PAGE) * PAGE + x % PAGE) as usize].map(|v| v as f32 / 65535.0))
        };
        let (a, b, c, d) = (get(x0, y0), get(x0 + 1.0, y0), get(x0, y0 + 1.0), get(x0 + 1.0, y0 + 1.0));
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
        }
        Some(out)
    }

    /// A whole tile as RGBA rows (`resolution²` texels; zero where no page).
    pub fn tile_texels(&self, channel: Channel, tile: u16) -> Option<Vec<[u16; 4]>> {
        let l = self.layers.get(&channel)?;
        let t = l.tiles.get(&tile)?;
        let r = l.resolution as usize;
        let side = r / PAGE as usize;
        let mut out = vec![[0u16; 4]; r * r];
        for (i, p) in t.pages.iter().enumerate() {
            let Some(p) = p else { continue };
            let (px, py) = ((i % side) * PAGE as usize, (i / side) * PAGE as usize);
            for row in 0..PAGE as usize {
                let at = (py + row) * r + px;
                out[at..at + PAGE as usize].copy_from_slice(&p[row * PAGE as usize..(row + 1) * PAGE as usize]);
            }
        }
        Some(out)
    }

    /// Puts a whole tile from RGBA rows (`resolution²` texels): pages with
    /// any texel not zero are kept, the rest left out. The caller checked
    /// the tile, the size and the budget (`pages_of`).
    pub fn put_tile(&mut self, channel: Channel, tile: u16, texels: &[[u16; 4]]) {
        let l = self.layers.get_mut(&channel).expect("set_resolution first");
        let r = l.resolution as usize;
        let side = r / PAGE as usize;
        let mut t = Tile::new(l.resolution);
        for (i, slot) in t.pages.iter_mut().enumerate() {
            let (px, py) = ((i % side) * PAGE as usize, (i / side) * PAGE as usize);
            let mut page = Vec::with_capacity(PAGE_TEXELS);
            for row in 0..PAGE as usize {
                let at = (py + row) * r + px;
                page.extend_from_slice(&texels[at..at + PAGE as usize]);
            }
            if page.iter().any(|t| *t != [0; 4]) {
                *slot = Some(Arc::new(page));
            }
        }
        l.tiles.insert(tile, t);
    }

    /// Puts one page's texels (`PAGE_TEXELS`, row-major) into a tile,
    /// replacing what was there. The caller checked the place and budget.
    pub fn put_page(&mut self, channel: Channel, tile: u16, page: usize, texels: Vec<[u16; 4]>) {
        let l = self.layers.get_mut(&channel).expect("set_resolution first");
        let resolution = l.resolution;
        let t = l.tiles.entry(tile).or_insert_with(|| Tile::new(resolution));
        if let Some(slot) = t.pages.get_mut(page) {
            *slot = Some(Arc::new(texels));
        }
    }

    /// The pages of a `resolution` tile that triangles with these UVs
    /// (0-1 within the tile) reach, `margin` texels around each.
    pub fn pages_under(uv_tris: impl Iterator<Item = [[f32; 2]; 3]>, resolution: u32, margin: f32) -> std::collections::BTreeSet<usize> {
        let side = (resolution / PAGE) as i64;
        let r = resolution as f32;
        let mut out = std::collections::BTreeSet::new();
        for t in uv_tris {
            let (lo, hi) = t.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(a, b), p| ([a[0].min(p[0]), a[1].min(p[1])], [b[0].max(p[0]), b[1].max(p[1])]));
            if !(lo[0].is_finite() && hi[0].is_finite() && lo[1].is_finite() && hi[1].is_finite()) {
                continue;
            }
            let page = |x: f32| (((x * r) / PAGE as f32).floor() as i64).clamp(0, side - 1);
            let (x0, x1) = (page(lo[0] - margin / r), page(hi[0] + margin / r));
            let (y0, y1) = (page(lo[1] - margin / r), page(hi[1] + margin / r));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    out.insert((y * side + x) as usize);
                }
            }
        }
        out
    }

    /// Pages a whole tile of RGBA rows would hold (those not all zero).
    pub fn pages_of(resolution: u32, texels: &[[u16; 4]]) -> usize {
        let r = resolution as usize;
        let side = r / PAGE as usize;
        (0..side * side)
            .filter(|i| {
                let (px, py) = ((i % side) * PAGE as usize, (i / side) * PAGE as usize);
                (0..PAGE as usize).any(|row| texels[(py + row) * r + px..(py + row) * r + px + PAGE as usize].iter().any(|t| *t != [0; 4]))
            })
            .count()
    }

    /// Pages held in `channel`'s `tile`.
    pub fn tile_pages(&self, channel: Channel, tile: u16) -> usize {
        self.layers.get(&channel).and_then(|l| l.tiles.get(&tile)).map_or(0, Tile::page_count)
    }

    /// A content hash (xxh3-128) over every channel, tile and page in
    /// order: the same texels give the same hash however they were made.
    pub fn hash(&self) -> String {
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        self.hash_into(&mut h);
        format!("{:032x}", h.digest128())
    }

    pub fn hash_into(&self, h: &mut xxhash_rust::xxh3::Xxh3) {
        for (c, l) in &self.layers {
            // An all-zero page holds the same texels as none (an import
            // leaves it out): the hash skips it.
            let pages: Vec<(u16, usize, &PageData)> = l.tiles.iter().flat_map(|(k, t)| t.pages.iter().enumerate().filter_map(move |(i, p)| p.as_ref().filter(|p| p.iter().any(|v| *v != [0; 4])).map(|p| (*k, i, p)))).collect();
            if pages.is_empty() {
                continue;
            }
            h.update(c.name().as_bytes());
            h.update(&l.resolution.to_le_bytes());
            for (k, i, p) in pages {
                h.update(&k.to_le_bytes());
                h.update(&(i as u64).to_le_bytes());
                h.update(bytemuck::cast_slice(&p[..]));
            }
        }
    }
}

/// Channels `bake udim` writes into the tiles.
pub const BAKED: [Channel; 4] = [Channel::Color, Channel::Roughness, Channel::Normal, Channel::Ao];

/// Triangles' UVs (within their tile) by tile.
pub type TileTriangles = BTreeMap<u16, Vec<[[f32; 2]; 3]>>;

/// What `bake udim` would hold, from the UV layout alone: each tile's
/// cage triangles (UVs within the tile; a cage face's UVs bound its level
/// faces') and the pages the `BAKED` channels would take at `resolution`
/// within `margin` texels of them.
pub fn bake_estimate(poly: &crate::poly::Poly, resolution: u32, margin: f32) -> (TileTriangles, usize) {
    let mut by_tile: TileTriangles = Default::default();
    let Some(uv) = poly.uv.as_ref().map(|u| u.to_vec()) else { return (by_tile, 0) };
    let (starts, _) = poly.cage_faces();
    for w in starts.windows(2) {
        let face = &uv[w[0] as usize..w[1] as usize];
        // The face's tile by its UV middle, as `Low::tile` takes its
        // triangles (a corner on a tile's edge is not the face's tile).
        let n = face.len() as f32;
        let mid = face.iter().fold([0.0f32; 2], |a, u| [a[0] + u[0] / n, a[1] + u[1] / n]);
        let Some(t) = tile_of(mid) else { continue };
        let o = tile_origin(t);
        let local: Vec<[f32; 2]> = face.iter().map(|u| [u[0] - o[0], u[1] - o[1]]).collect();
        for j in 1..local.len() - 1 {
            by_tile.entry(t).or_default().push([local[0], local[j], local[j + 1]]);
        }
    }
    let pages = by_tile.values().map(|tris| Textures::pages_under(tris.iter().copied(), resolution, margin).len()).sum::<usize>() * BAKED.len();
    (by_tile, pages)
}

/// A bake's textures (one tile's: 8-bit RGBA colour, normal and
/// AO-roughness-metal at the channels' resolution) put into `tile`'s
/// `pages` of the `BAKED` channels, widened to 16 bits, fully covering.
pub fn put_baked(textures: &mut Textures, tile: u16, tex: &crate::mesh::Textures, pages: &std::collections::BTreeSet<usize>) {
    let resolution = tex.size;
    let side = (resolution / PAGE) as usize;
    let r = resolution as usize;
    let wide = |v: u8| v as u16 * 257;
    for &page in pages {
        let (px, py) = ((page % side) * PAGE as usize, (page / side) * PAGE as usize);
        let mut out: [Vec<[u16; 4]>; 4] = std::array::from_fn(|_| Vec::with_capacity(PAGE_TEXELS));
        for row in 0..PAGE as usize {
            for col in 0..PAGE as usize {
                let k = 4 * ((py + row) * r + px + col);
                let (cc, nn, oo) = (&tex.color[k..k + 4], &tex.normal[k..k + 4], &tex.orm[k..k + 4]);
                out[0].push([wide(cc[0]), wide(cc[1]), wide(cc[2]), 65535]);
                out[1].push([wide(oo[1]), wide(oo[1]), wide(oo[1]), 65535]);
                out[2].push([wide(nn[0]), wide(nn[1]), wide(nn[2]), 65535]);
                out[3].push([wide(oo[0]), wide(oo[0]), wide(oo[0]), 65535]);
            }
        }
        for (c, texels) in BAKED.into_iter().zip(out) {
            textures.put_page(c, tile, page, texels);
        }
    }
}

/// Each point's paint colour (sRGB) over white by its coverage, the mean
/// of its corners' samples (`corners` point per corner, `uv` per corner),
/// in corner order: a point on a UV seam reads both charts, painted alike.
/// None without a colour channel.
pub fn point_colors(textures: &Textures, corners: &[u32], uv: &[[f32; 2]], points: usize) -> Option<Vec<[f32; 3]>> {
    textures.layers.get(&Channel::Color).filter(|l| l.tiles.values().any(|t| t.page_count() > 0))?;
    let mut sum = vec![[0.0f32; 4]; points];
    for (c, t) in corners.iter().zip(uv) {
        let Some(s) = sum.get_mut(*c as usize) else { continue };
        let x = textures.sample(Channel::Color, *t).unwrap_or([0.0; 4]);
        for k in 0..3 {
            s[k] += 1.0 + (x[k] - 1.0) * x[3];
        }
        s[3] += 1.0;
    }
    Some(sum.into_iter().map(|s| if s[3] > 0.0 { [s[0] / s[3], s[1] / s[3], s[2] / s[3]] } else { [1.0; 3] }).collect())
}

/// A page in a file: its slot in the tile and its texels (RGBA 16-bit
/// little-endian, deflated, base64).
#[derive(Serialize, Deserialize)]
struct PageFile {
    at: u32,
    data: String,
}

#[derive(Serialize, Deserialize)]
struct TileFile {
    udim: u16,
    pages: Vec<PageFile>,
}

#[derive(Serialize, Deserialize)]
struct LayerFile {
    channel: Channel,
    resolution: u32,
    tiles: Vec<TileFile>,
}

/// Textures as a file holds them.
#[derive(Serialize, Deserialize, Default)]
pub struct TexturesFile {
    layers: Vec<LayerFile>,
}

impl Textures {
    pub fn to_file(&self) -> Result<TexturesFile, String> {
        let mut layers = Vec::new();
        for (c, l) in &self.layers {
            let mut tiles = Vec::new();
            for (k, t) in &l.tiles {
                let mut pages = Vec::new();
                for (i, p) in t.pages.iter().enumerate() {
                    if let Some(p) = p {
                        let raw: Vec<u8> = p.iter().flat_map(|t| t.iter().flat_map(|v| v.to_le_bytes())).collect();
                        pages.push(PageFile { at: i as u32, data: super::pack(&raw)? });
                    }
                }
                if !pages.is_empty() {
                    tiles.push(TileFile { udim: *k, pages });
                }
            }
            if !tiles.is_empty() {
                layers.push(LayerFile { channel: *c, resolution: l.resolution, tiles });
            }
        }
        Ok(TexturesFile { layers })
    }
}

impl TexturesFile {
    /// The textures, every count and place checked before a page is
    /// inflated (each page charged to the read's inflate budget).
    pub fn read(self) -> Result<Textures, String> {
        let mut out = Textures::default();
        let mut total = 0usize;
        for l in self.layers {
            check_resolution(l.resolution)?;
            if out.layers.contains_key(&l.channel) {
                return Err(format!("the {} channel is listed twice", l.channel.name()));
            }
            if l.tiles.len() > MAX_TILES as usize {
                return Err(format!("a channel may have at most {MAX_TILES} tiles"));
            }
            let side = l.resolution / PAGE;
            let mut layer = Layer { resolution: l.resolution, tiles: BTreeMap::new() };
            for t in l.tiles {
                if !valid_tile(t.udim) || layer.tiles.contains_key(&t.udim) {
                    return Err(format!("tile {} is not one of 1001-1100, or is listed twice", t.udim));
                }
                total += t.pages.len();
                if total > MAX_PAGES {
                    return Err(format!("the textures hold more than {MAX_PAGES} pages ({} GiB)", MAX_BYTES >> 30));
                }
                let mut tile = Tile::new(l.resolution);
                for p in t.pages {
                    if p.at >= side * side || tile.pages[p.at as usize].is_some() {
                        return Err(format!("page {} of tile {} is outside the tile or listed twice", p.at, t.udim));
                    }
                    let raw = super::unpack(&p.data, PAGE_BYTES)?;
                    let texels: Vec<[u16; 4]> = raw.chunks_exact(8).map(|c| [0, 2, 4, 6].map(|k| u16::from_le_bytes([c[k], c[k + 1]]))).collect();
                    tile.pages[p.at as usize] = Some(Arc::new(texels));
                }
                layer.tiles.insert(t.udim, tile);
            }
            out.layers.insert(l.channel, layer);
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod review_tests;
