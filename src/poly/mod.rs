//! Polygon objects (docs/QUALITY_PLAN.md Stage 3a, D2): a quad-dominant
//! cage with Catmull-Clark subdivision levels and semi-sharp creases
//! (`subdiv.rs`, D16), stored as `Object::poly` on a `Shape::Poly`.
//!
//! The cage is kept in `Chunked` storage: chunks of 4,096 values behind
//! Arcs, so a scene clone shares them and a compare checks pointers first
//! (the per-frame clone and compare stays cheap with millions of faces),
//! and an edit copies only the chunks it touches (the undo cost). Faces
//! are CSR: `face_start` (faces + 1 offsets) into `corners`.
//!
//! Levels are not stored as positions: level k is a pure function of the
//! cage, the creases and the tangent-space displacements of levels 1..k
//! (D17, zero until Stage 3c projects detail), rebuilt on demand
//! (`subdiv::evaluate`). A saved file keeps the cage, the creases, the
//! level count and any displacement, so it stays the cage's size.
//! `made_by` keeps the arguments that made the cage (D3), so
//! `poly rebuild` makes it again.

pub mod export;
pub mod from_mesh;
pub mod low;
pub mod project;
pub mod retopo;
pub mod subdiv;
pub mod texture;
pub mod topology;
pub mod uv;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine;
use glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Values per chunk of `Chunked` storage.
pub const CHUNK: usize = 4096;
/// Most faces a cage may have.
pub const MAX_CAGE_FACES: usize = 4_000_000;
/// Most corners (face sides, summed) a cage may have.
pub const MAX_CAGE_CORNERS: usize = 16_000_000;
/// Most points a cage may have.
pub const MAX_CAGE_POINTS: usize = crate::mesh_asset::MAX_POINTS;
/// Most sides one face may have.
pub const MAX_SIDES: usize = 64;
/// Most subdivision levels kept.
pub const MAX_LEVELS: u8 = 6;
/// Most faces the finest level kept may have (§6: 16M, about 0.9 GB).
pub const MAX_FACES: u64 = 16_000_000;
/// Most faces the shown level may have: it is drawn, picked and exported
/// as triangles (twice as many), the regular viewport's budget.
pub const MAX_SHOWN_FACES: u64 = 4_000_000;
/// Crease sharpness at and above which an edge stays sharp at every level.
pub const INFINITE_SHARPNESS: f32 = 10.0;
/// Most crease edges one call may set, and a cage may keep.
pub const MAX_CREASES: usize = 65_536;

/// Values in chunks of `CHUNK`, shared by Arcs; `==` compares chunk
/// pointers first, then the contents of chunks that are not shared.
#[derive(Clone)]
pub struct Chunked<T> {
    len: usize,
    chunks: Arc<Vec<Arc<[T]>>>,
}

impl<T> std::fmt::Debug for Chunked<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunked({} values in {} chunks)", self.len, self.chunks.len())
    }
}

impl<T: Copy> Chunked<T> {
    pub fn new(values: &[T]) -> Self {
        Chunked { len: values.len(), chunks: Arc::new(values.chunks(CHUNK).map(Arc::from).collect()) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn get(&self, i: usize) -> T {
        self.chunks[i / CHUNK][i % CHUNK]
    }

    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.chunks.iter().flat_map(|c| c.iter().copied())
    }

    pub fn to_vec(&self) -> Vec<T> {
        let mut out = Vec::with_capacity(self.len);
        for c in self.chunks.iter() {
            out.extend_from_slice(c);
        }
        out
    }

    /// Sets value `i`, copying only its chunk (the others stay shared).
    #[cfg_attr(not(test), expect(dead_code, reason = "no tool edits cage points yet; the multires test moves them"))]
    pub fn set(&mut self, i: usize, value: T) {
        let chunks = Arc::make_mut(&mut self.chunks);
        let mut owned = chunks[i / CHUNK].to_vec();
        owned[i % CHUNK] = value;
        chunks[i / CHUNK] = Arc::from(owned);
    }

    pub fn chunks(&self) -> &[Arc<[T]>] {
        &self.chunks
    }
}

impl<T: PartialEq> PartialEq for Chunked<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && (Arc::ptr_eq(&self.chunks, &other.chunks) || self.chunks.iter().zip(other.chunks.iter()).all(|(a, b)| Arc::ptr_eq(a, b) || a[..] == b[..]))
    }
}

/// One subdivision level's own data (level k ≥ 1).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Level {
    /// Tangent-space displacement per vertex of this level (D17): x along
    /// the frame's tangent, y its bitangent, z its normal. None is zero.
    pub displacement: Option<Chunked<Vec3>>,
}

/// A polygon cage with subdivision levels (see the module doc).
#[derive(Clone, Debug)]
pub struct Poly {
    pub cage: Chunked<Vec3>,
    /// Faces + 1 offsets into `corners`.
    pub face_start: Chunked<u32>,
    pub corners: Chunked<u32>,
    /// Crease sharpness by cage edge (lower vertex index first), in
    /// (0, 10]; 10 is infinitely sharp (`INFINITE_SHARPNESS`).
    pub creases: Arc<BTreeMap<(u32, u32), f32>>,
    /// Levels 1..=n kept.
    pub levels: Vec<Level>,
    /// The level shown, rendered and exported (0 is the cage).
    pub level: u8,
    /// The call that made the cage (`poly rebuild` runs it again), shared:
    /// a retopology's holds its guides, and the scene is cloned and
    /// compared every frame.
    pub made_by: Option<Arc<Value>>,
    /// The `poly project` call that wrote the levels' displacements
    /// (source clay, levels, mode, cone, reach), for `info` and for
    /// `project` again without a source.
    pub projected: Option<Arc<Value>>,
    /// A UV per cage corner (`poly uv`; face-varying, one tile or UDIM
    /// tiles 1001-1100), None before an unwrap.
    pub uv: Option<Chunked<[f32; 2]>>,
    /// Texture tiles and the paint strokes on them (Stage 4b), shared.
    pub paint: Option<Arc<texture::paint::PolyPaint>>,
}

impl PartialEq for Poly {
    /// Pointers first: the per-frame scene compare and the drawn mesh's
    /// key stop at shared storage, creases included.
    fn eq(&self, other: &Self) -> bool {
        self.level == other.level
            && self.cage == other.cage
            && self.face_start == other.face_start
            && self.corners == other.corners
            && (Arc::ptr_eq(&self.creases, &other.creases) || self.creases == other.creases)
            && self.levels == other.levels
            && same_value(&self.made_by, &other.made_by)
            && same_value(&self.projected, &other.projected)
            && self.uv == other.uv
            && match (&self.paint, &other.paint) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                (a, b) => a.is_none() && b.is_none(),
            }
    }
}

/// Shared values compare by pointer first.
fn same_value(a: &Option<Arc<Value>>, b: &Option<Arc<Value>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// Vertex, edge, face and corner counts of one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub vertices: u64,
    pub edges: u64,
    pub faces: u64,
    pub corners: u64,
}

impl Counts {
    /// The next level's counts: every face splits into one quad per
    /// corner, every edge into two, and each corner adds an inner edge.
    pub fn next(self) -> Counts {
        Counts { vertices: self.vertices + self.faces + self.edges, edges: 2 * self.edges + self.corners, faces: self.corners, corners: 4 * self.corners }
    }
}

/// Faces at level `levels` of a cage of `faces` faces and `corners`
/// corners, from the counts alone (saturating).
pub fn faces_at(faces: u64, corners: u64, levels: u8) -> u64 {
    if levels == 0 { faces } else { corners.saturating_mul(4u64.saturating_pow(levels as u32 - 1)) }
}

/// Refuses `levels` levels of this cage when the finest would pass
/// `MAX_FACES` (from the counts, before anything is allocated), saying how
/// many fit.
pub fn check_levels(faces: u64, corners: u64, levels: u8) -> Result<(), String> {
    if levels > MAX_LEVELS {
        return Err(format!("at most {MAX_LEVELS} levels"));
    }
    check_faces(faces, corners, levels)
}

/// `check_levels`' face bound alone (tests subdivide a cube 8 times).
pub fn check_faces(faces: u64, corners: u64, levels: u8) -> Result<(), String> {
    let top = faces_at(faces, corners, levels);
    if top > MAX_FACES {
        let fit = (0..=MAX_LEVELS).take_while(|&l| faces_at(faces, corners, l) <= MAX_FACES).last().unwrap_or(0);
        return Err(format!("{levels} levels of this cage ({faces} faces) would make {top} faces, over the {MAX_FACES} bound; at most {fit} levels fit"));
    }
    Ok(())
}

impl Poly {
    /// Checks and keeps a cage: counts within the caps, coordinates
    /// finite and within `mesh_asset::MAX_COORD`, faces of 3 to `MAX_SIDES`
    /// distinct points in range.
    pub fn new(points: &[Vec3], face_start: &[u32], corners: &[u32]) -> Result<Poly, String> {
        let faces = face_start.len().saturating_sub(1);
        if points.len() > MAX_CAGE_POINTS || faces > MAX_CAGE_FACES || corners.len() > MAX_CAGE_CORNERS {
            return Err(format!("a cage may have at most {MAX_CAGE_POINTS} points, {MAX_CAGE_FACES} faces and {MAX_CAGE_CORNERS} corners (this one: {}, {faces} and {})", points.len(), corners.len()));
        }
        if faces == 0 {
            return Err("the cage has no faces".into());
        }
        if let Some(p) = points.iter().find(|p| !p.is_finite() || p.abs().max_element() > crate::mesh_asset::MAX_COORD) {
            return Err(format!("a point is not finite or beyond {} ({p})", crate::mesh_asset::MAX_COORD));
        }
        if face_start[0] != 0 || face_start[faces] as usize != corners.len() {
            return Err("face offsets must start at 0 and end at the corner count".into());
        }
        // Every offset is checked before any face is sliced: a file's
        // offsets may run past the corners and come back (0, 10, 6).
        if let Some(f) = (0..faces).find(|&f| face_start[f] > face_start[f + 1] || face_start[f + 1] as usize > corners.len()) {
            return Err(format!("face offsets must not decrease or pass the {} corners (face {f})", corners.len()));
        }
        let n = points.len() as u32;
        for f in 0..faces {
            let (a, b) = (face_start[f] as usize, face_start[f + 1] as usize);
            if b < a + 3 || b - a > MAX_SIDES {
                return Err(format!("face {f} has {} sides (3 to {MAX_SIDES})", b.saturating_sub(a)));
            }
            let face = &corners[a..b];
            if face.iter().any(|&i| i >= n) {
                return Err(format!("face {f} points past the point list"));
            }
            if (1..face.len()).any(|i| face[..i].contains(&face[i])) {
                return Err(format!("face {f} uses a point twice"));
            }
        }
        Ok(Poly { cage: Chunked::new(points), face_start: Chunked::new(face_start), corners: Chunked::new(corners), creases: Arc::default(), levels: Vec::new(), level: 0, made_by: None, projected: None, uv: None, paint: None })
    }

    /// `new` from a list of faces.
    pub fn from_faces(points: &[Vec3], faces: &[Vec<u32>]) -> Result<Poly, String> {
        let mut face_start = vec![0u32];
        let mut corners = Vec::new();
        for f in faces {
            corners.extend_from_slice(f);
            face_start.push(corners.len() as u32);
        }
        Poly::new(points, &face_start, &corners)
    }

    pub fn faces(&self) -> usize {
        self.face_start.len() - 1
    }

    /// The cage's faces and corners (`face_start`, `corners`) as vectors.
    pub fn cage_faces(&self) -> (Vec<u32>, Vec<u32>) {
        (self.face_start.to_vec(), self.corners.to_vec())
    }

    /// Faces at level `l`.
    pub fn faces_at(&self, l: u8) -> u64 {
        faces_at(self.faces() as u64, self.corners.len() as u64, l)
    }

    /// Levels kept (not counting the cage).
    pub fn level_count(&self) -> u8 {
        self.levels.len() as u8
    }

    /// Keeps `n` levels (dropping finer ones, adding zero-displacement
    /// ones), within `check_levels`; the shown level stays within them.
    pub fn set_levels(&mut self, n: u8) -> Result<(), String> {
        check_levels(self.faces() as u64, self.corners.len() as u64, n)?;
        self.levels.resize(n as usize, Level::default());
        self.level = self.level.min(n);
        Ok(())
    }

    /// The finest kept level whose faces fit `MAX_SHOWN_FACES`.
    pub fn finest_shown(&self) -> u8 {
        (0..=self.level_count()).rev().find(|&l| self.faces_at(l) <= MAX_SHOWN_FACES).unwrap_or(0)
    }


    /// The object reflected in its own axis `axis` (0 = X), for a linked
    /// mirror copy that reflects its own geometry. No copy of a Poly does
    /// today: `pose::choose_axis` gives a Poly no axis, so a Poly's linked
    /// mirror copy shares the source's Poly and reflects by its matrix (a
    /// negative determinant, which `low::Low` and exports handle by winding
    /// faces the other way). This is the exact image when one does: every
    /// face wound the other
    /// way round from the same first corner, so it still faces out and
    /// keeps its edges, creases and level counts. Each level's tangent
    /// displacement is carried over (`subdiv::mirror_displacements`: the
    /// reversed faces renumber points from level 2 on, so each level's
    /// points are matched and the offsets turned into the image's
    /// frames), so every level is the source's reflected. That evaluates
    /// the displaced levels of both, once per change of the source.
    pub fn mirrored(&self, axis: usize) -> Poly {
        let points: Vec<Vec3> = self
            .cage
            .iter()
            .map(|mut p| {
                p[axis] = -p[axis];
                p
            })
            .collect();
        let (starts, corners) = self.cage_faces();
        let mut flipped = Vec::with_capacity(corners.len());
        for f in starts.windows(2) {
            let face = &corners[f[0] as usize..f[1] as usize];
            flipped.push(face[0]);
            flipped.extend(face[1..].iter().rev());
        }
        let mut out = Poly {
            cage: Chunked::new(&points),
            face_start: self.face_start.clone(),
            corners: Chunked::new(&flipped),
            creases: self.creases.clone(),
            levels: vec![Level::default(); self.levels.len()],
            level: self.level,
            made_by: self.made_by.clone(),
            projected: self.projected.clone(),
            uv: self.uv.as_ref().map(|u| Chunked::new(&uv::mirrored(&self.face_start.to_vec(), &u.to_vec()))),
            paint: None,
        };
        // The same texels (the UVs are the same per face), the strokes
        // reflected.
        out.paint = self.paint.as_ref().map(|p| Arc::new(texture::paint::PolyPaint { textures: p.textures.clone(), records: texture::paint::mirrored(&p.records, axis, &texture::paint::layout_hash(&out)), imported: p.imported }));
        // The counts are the source's, so this cannot fail; if it did the
        // copy would keep its levels undisplaced.
        if let Ok(d) = subdiv::mirror_displacements(self, &out, axis) {
            for (l, d) in out.levels.iter_mut().zip(d) {
                l.displacement = d;
            }
        }
        out
    }

    /// The cage's topology at level 0, with its creases.
    pub fn topology(&self) -> topology::Topology {
        let (s, c) = self.cage_faces();
        let mut t = topology::Topology::build(self.cage.len(), s, c);
        t.set_creases(&self.creases);
        t
    }

    /// `poly info`: the cage's faces and points (`points` those faces
    /// use, `cage_points` every index a crease may name), quads,
    /// valences, borders, manifoldness, genus, levels and creases.
    pub fn info(&self) -> Value {
        let t = self.topology();
        let faces = self.faces();
        let sides = |n: usize| (0..faces).filter(|&f| (t.face_start[f + 1] - t.face_start[f]) as usize == n).count();
        let (quads, triangles) = (sides(4), sides(3));
        let open = (0..t.edges.len()).filter(|&e| t.faces_of_edge(e).len() == 1).count();
        let (over, flipped) = t.non_manifold();
        let used: Vec<usize> = (0..t.vertices).filter(|&v| !t.faces_of(v).is_empty()).collect();
        let mut valence: BTreeMap<usize, usize> = BTreeMap::new();
        let (mut interior, mut irregular) = (0usize, 0usize);
        for &v in &used {
            let n = t.edges_of(v).len();
            *valence.entry(n).or_default() += 1;
            if !t.edges_of(v).iter().any(|&e| t.is_border(e as usize)) {
                interior += 1;
                irregular += (n != 4) as usize;
            }
        }
        // A point whose faces make more than one fan (parts meeting only
        // there) counts once per fan, as if split as from_clay splits
        // them: components count parts by shared edges, so the genus is
        // the split surface's.
        let fans = t.fan_counts();
        let pinched = fans.iter().filter(|&&n| n > 1).count();
        let euler = fans.iter().sum::<usize>() as i64 - t.edges.len() as i64 + faces as i64;
        let (components, loops) = (t.components() as i64, t.boundary_loops() as i64);
        let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1e4).round() / 100.0 };
        let fit = (0..=MAX_LEVELS).take_while(|&l| self.faces_at(l) <= MAX_FACES).last().unwrap_or(0);
        serde_json::json!({
            "points": used.len(),
            "cage_points": self.cage.len(),
            "faces": faces,
            "edges": t.edges.len(),
            "quads": quads,
            "triangles": triangles,
            "ngons": faces - quads - triangles,
            "quads_pct": pct(quads, faces),
            "valence": valence.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect::<serde_json::Map<_, _>>(),
            "irregular_pct": pct(irregular, interior),
            "boundary_loops": loops,
            "open_edges": open,
            "non_manifold_edges": over,
            "flipped_edges": flipped,
            "non_manifold_points": pinched,
            "components": components,
            "euler": euler,
            "genus": (over == 0 && flipped == 0).then(|| (2 * components - euler - loops) / 2),
            "levels": self.level_count(),
            "level": self.level,
            "level_faces": (0..=self.level_count()).map(|l| self.faces_at(l)).collect::<Vec<_>>(),
            "max_levels": fit,
            "max_shown_level": (0..=fit).take_while(|&l| self.faces_at(l) <= MAX_SHOWN_FACES).last().unwrap_or(0),
            "creases": { "edges": self.creases.len(), "infinite": self.creases.values().filter(|&&s| s >= INFINITE_SHARPNESS).count() },
            "uv": self.uv.as_ref().map(|u| {
                let u = u.to_vec();
                let mut tiles: Vec<u16> = u.iter().filter_map(|t| texture::tile_of(*t)).collect();
                tiles.sort_unstable();
                tiles.dedup();
                serde_json::json!({ "corners": u.len(), "seams": uv::seams(&t, &u), "udim_tiles": tiles })
            }),
            "paint": self.paint.as_ref().filter(|p| !p.is_empty()).map(|p| serde_json::json!({
                "channels": p.textures.channels().into_iter().map(|c| (c.name().to_string(), serde_json::json!({ "resolution": p.textures.resolution(c), "tiles": p.textures.tiles(c) }))).collect::<serde_json::Map<_, _>>(),
                "records": p.records.len(),
                "pages": p.textures.pages(),
                "texture_mb": (p.textures.bytes() as f64 / 1048576.0 * 10.0).round() / 10.0,
                "from_bake_or_import": p.imported,
            })),
            "displaced_levels": self.levels.iter().enumerate().filter(|(_, l)| l.displacement.is_some()).map(|(k, _)| k + 1).collect::<Vec<_>>(),
            "projected": self.projected.as_deref(),
            "made_by": self.made_by.as_deref(),
            "hash": self.hash(),
        })
    }

    /// Bytes of this object's chunks `other` does not share (chunk for
    /// chunk, by pointer): what an undo snapshot holding only this costs.
    pub fn unshared_bytes(&self, other: Option<&Poly>) -> usize {
        fn part<T: Copy>(a: &Chunked<T>, b: Option<&Chunked<T>>) -> usize {
            a.chunks().iter().enumerate().filter(|(i, c)| !b.and_then(|b| b.chunks().get(*i)).is_some_and(|d| Arc::ptr_eq(c, d))).map(|(_, c)| std::mem::size_of_val::<[T]>(c)).sum()
        }
        let mut total = part(&self.cage, other.map(|o| &o.cage)) + part(&self.face_start, other.map(|o| &o.face_start)) + part(&self.corners, other.map(|o| &o.corners));
        for (k, l) in self.levels.iter().enumerate() {
            if let Some(d) = &l.displacement {
                total += part(d, other.and_then(|o| o.levels.get(k)).and_then(|l| l.displacement.as_ref()));
            }
        }
        if let Some(u) = &self.uv {
            total += part(u, other.and_then(|o| o.uv.as_ref()));
        }
        if let Some(p) = &self.paint {
            let theirs = other.and_then(|o| o.paint.as_ref());
            if !theirs.is_some_and(|t| Arc::ptr_eq(p, t)) {
                total += p.textures.unshared_bytes(theirs.map(|t| &t.textures)) + p.records.len() * std::mem::size_of::<texture::paint::Record>();
            }
        }
        total
    }

    /// Content hash over everything kept (xxh3-128): what save, load and
    /// undo must give back.
    pub fn hash(&self) -> String {
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        let mut put = |tag: &[u8], bytes: &[u8]| {
            h.update(tag);
            h.update(&(bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        };
        for c in self.cage.chunks() {
            put(b"p", bytemuck::cast_slice(c));
        }
        for c in self.face_start.chunks() {
            put(b"s", bytemuck::cast_slice(c));
        }
        for c in self.corners.chunks() {
            put(b"c", bytemuck::cast_slice(c));
        }
        for ((a, b), s) in self.creases.iter() {
            put(b"k", &[a.to_le_bytes(), b.to_le_bytes(), s.to_le_bytes()].concat());
        }
        for (k, l) in self.levels.iter().enumerate() {
            put(b"l", &(k as u64).to_le_bytes());
            for c in l.displacement.iter().flat_map(|d| d.chunks()) {
                put(b"d", bytemuck::cast_slice(c));
            }
        }
        put(b"v", &[self.level]);
        put(b"m", self.made_by.as_ref().map(|v| v.to_string()).unwrap_or_default().as_bytes());
        if let Some(p) = &self.projected {
            put(b"j", p.to_string().as_bytes());
        }
        for c in self.uv.iter().flat_map(|u| u.chunks()) {
            put(b"u", bytemuck::cast_slice(c));
        }
        if let Some(p) = self.paint.as_ref().filter(|p| !p.is_empty()) {
            put(b"t", p.textures.hash().as_bytes());
            put(b"i", &[p.imported as u8]);
            for r in &p.records {
                put(b"r", &[r.id.to_le_bytes().as_slice(), r.digest.as_bytes()].concat());
            }
        }
        format!("{:032x}", h.digest128())
    }
}

/// Packs values (little-endian), deflated, base64.
fn pack(raw: &[u8]) -> Result<String, String> {
    use std::io::Write;
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(raw).map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(z.finish().map_err(|e| e.to_string())?))
}

/// `pack`'s bytes back, when they are exactly `want` bytes (charged to the
/// read's inflate budget before anything is inflated).
fn unpack(data: &str, want: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    crate::clay::charge_inflate(want)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[..]).take(want + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() as u64 != want {
        return Err("a polygon object's packed data has the wrong size".into());
    }
    Ok(raw)
}

fn le_f32(c: &[u8]) -> f32 {
    f32::from_le_bytes([c[0], c[1], c[2], c[3]])
}

fn le_u32(c: &[u8]) -> u32 {
    u32::from_le_bytes([c[0], c[1], c[2], c[3]])
}

fn points_bytes<'a>(points: impl Iterator<Item = Vec3> + 'a, out: &mut Vec<u8>) {
    for p in points {
        for c in p.to_array() {
            out.extend(c.to_le_bytes());
        }
    }
}

fn read_points(raw: &[u8]) -> Vec<Vec3> {
    raw.chunks_exact(12).map(|c| Vec3::new(le_f32(&c[0..4]), le_f32(&c[4..8]), le_f32(&c[8..12]))).collect()
}

#[derive(Serialize, Deserialize)]
struct DisplacementFile {
    level: u8,
    vertices: usize,
    /// f32 x 3 per vertex, little-endian, deflated, base64.
    data: String,
}

#[derive(Serialize, Deserialize)]
struct UvFile {
    corners: usize,
    /// f32 x 2 per cage corner, little-endian, deflated, base64.
    data: String,
}

#[derive(Serialize, Deserialize)]
struct PolyFile {
    points: usize,
    faces: usize,
    corners: usize,
    /// Points (f32 x 3), face offsets (u32, faces + 1), then corners
    /// (u32), little-endian, deflated, base64.
    data: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    creases: Vec<(u32, u32, f32)>,
    #[serde(default)]
    levels: u8,
    #[serde(default)]
    level: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    displacement: Vec<DisplacementFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    made_by: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    projected: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uv: Option<UvFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paint: Option<texture::paint::PaintFile>,
}

impl Serialize for Poly {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut raw = Vec::with_capacity(self.cage.len() * 12 + (self.face_start.len() + self.corners.len()) * 4);
        points_bytes(self.cage.iter(), &mut raw);
        for i in self.face_start.iter().chain(self.corners.iter()) {
            raw.extend(i.to_le_bytes());
        }
        let mut displacement = Vec::new();
        for (k, l) in self.levels.iter().enumerate() {
            if let Some(d) = &l.displacement {
                let mut raw = Vec::with_capacity(d.len() * 12);
                points_bytes(d.iter(), &mut raw);
                displacement.push(DisplacementFile { level: k as u8 + 1, vertices: d.len(), data: pack(&raw).map_err(S::Error::custom)? });
            }
        }
        PolyFile {
            points: self.cage.len(),
            faces: self.faces(),
            corners: self.corners.len(),
            data: pack(&raw).map_err(S::Error::custom)?,
            creases: self.creases.iter().map(|(&(a, b), &s)| (a, b, s)).collect(),
            levels: self.level_count(),
            level: self.level,
            displacement,
            made_by: self.made_by.as_deref().cloned(),
            projected: self.projected.as_deref().cloned(),
            uv: match &self.uv {
                Some(u) => {
                    let mut raw = Vec::with_capacity(u.len() * 8);
                    for c in u.iter() {
                        raw.extend(c[0].to_le_bytes());
                        raw.extend(c[1].to_le_bytes());
                    }
                    Some(UvFile { corners: u.len(), data: pack(&raw).map_err(S::Error::custom)? })
                }
                None => None,
            },
            paint: match self.paint.as_ref().filter(|p| !p.is_empty()) {
                Some(p) => Some(p.to_file().map_err(S::Error::custom)?),
                None => None,
            },
        }
        .serialize(s)
    }
}

impl PolyFile {
    /// The polygon object, its counts checked before anything is inflated.
    fn read(self) -> Result<Poly, String> {
        if self.points > MAX_CAGE_POINTS || self.faces > MAX_CAGE_FACES || self.corners > MAX_CAGE_CORNERS {
            return Err(format!("a polygon object may have at most {MAX_CAGE_POINTS} points, {MAX_CAGE_FACES} faces and {MAX_CAGE_CORNERS} corners"));
        }
        if self.creases.len() > MAX_CREASES {
            return Err(format!("a polygon object may keep at most {MAX_CREASES} creases"));
        }
        check_levels(self.faces as u64, self.corners as u64, self.levels)?;
        if self.level > self.levels {
            return Err(format!("shown level {} is past the {} levels kept", self.level, self.levels));
        }
        if faces_at(self.faces as u64, self.corners as u64, self.level) > MAX_SHOWN_FACES {
            return Err(format!("shown level {} would have more than {MAX_SHOWN_FACES} faces", self.level));
        }
        let want = (self.points * 12 + (self.faces + 1 + self.corners) * 4) as u64;
        let raw = unpack(&self.data, want)?;
        let (pts, rest) = raw.split_at(self.points * 12);
        let (starts, corners) = rest.split_at((self.faces + 1) * 4);
        let points = read_points(pts);
        let face_start: Vec<u32> = starts.chunks_exact(4).map(le_u32).collect();
        let corners: Vec<u32> = corners.chunks_exact(4).map(le_u32).collect();
        let mut poly = Poly::new(&points, &face_start, &corners)?;
        let mut creases = BTreeMap::new();
        for (a, b, s) in self.creases {
            if a >= b || b as usize >= self.points || !(s > 0.0 && s <= INFINITE_SHARPNESS) {
                return Err(format!("a crease ({a}, {b}, {s}) is not a pair of points in order with sharpness in (0, {INFINITE_SHARPNESS}]"));
            }
            creases.insert((a, b), s);
        }
        poly.creases = Arc::new(creases);
        poly.levels = vec![Level::default(); self.levels as usize];
        poly.level = self.level;
        if !self.displacement.is_empty() {
            let counts = subdiv::counts(&topology::Topology::build(points.len(), face_start, corners), self.levels);
            for d in self.displacement {
                let k = d.level as usize;
                if k == 0 || k > poly.levels.len() || d.vertices as u64 != counts[k].vertices || poly.levels[k - 1].displacement.is_some() {
                    return Err(format!("a displacement for level {} does not fit the levels kept", d.level));
                }
                let raw = unpack(&d.data, d.vertices as u64 * 12)?;
                let values = read_points(&raw);
                if values.iter().any(|v| !v.is_finite()) {
                    return Err("a displacement is not finite".into());
                }
                poly.levels[k - 1].displacement = Some(Chunked::new(&values));
            }
        }
        poly.made_by = self.made_by.map(Arc::new);
        poly.projected = self.projected.map(Arc::new);
        if let Some(u) = self.uv {
            if u.corners != self.corners {
                return Err(format!("UVs for {} corners on a cage of {}", u.corners, self.corners));
            }
            let raw = unpack(&u.data, u.corners as u64 * 8)?;
            let values: Vec<[f32; 2]> = raw.chunks_exact(8).map(|c| [le_f32(&c[0..4]), le_f32(&c[4..8])]).collect();
            if values.iter().flatten().any(|v| !v.is_finite() || v.abs() > uv::MAX_UV) {
                return Err(format!("a UV is not finite or beyond {}", uv::MAX_UV));
            }
            poly.uv = Some(Chunked::new(&values));
        }
        if let Some(p) = self.paint {
            if poly.uv.is_none() {
                return Err("texture tiles on a Poly without UVs".into());
            }
            let paint = p.read()?;
            let (lo, hi) = points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), q| (a.min(*q), b.max(*q)));
            texture::paint::check_records(&paint.records, lo, hi)?;
            poly.paint = Some(Arc::new(paint));
        }
        Ok(poly)
    }
}

impl<'de> Deserialize<'de> for Poly {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        PolyFile::deserialize(d)?.read().map_err(serde::de::Error::custom)
    }
}
