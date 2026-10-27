//! Triangle meshes kept by value: imported files (`import_mesh`), the
//! shapes `to_clay` and InsertMesh turn into clay (`Form::Mesh`), and
//! `Shape::Mesh` objects.
//!
//! A `MeshAsset` is shared (an Arc) and compares by content hash, so
//! scene clones and compares stay cheap. Clay blobs name an asset by its
//! hash and the clay keeps the asset beside its blobs (`Clay::assets`),
//! so a clay replays, saves and loads on its own; its geometry reads
//! only those (`Geo::new`). Every asset made in this process is also
//! listed (weakly) by hash, so `Clay::adopt_assets` can take a blob added
//! by hash from this session, and the same triangles are one asset.
//!
//! The distance field (`MeshField`) is built once per asset, on first
//! use: a box tree for the nearest triangle (signed by angle-weighted
//! pseudo normals on closed meshes) and a hierarchical generalized
//! winding number (Barill et al. 2018: exact solid angles near, a dipole
//! per node far) that signs open meshes and points beyond the search.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use base64::Engine;
use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Most triangles a clay's mesh asset may hold: imports are decimated
/// to this before the field is built (see `decimated`).
pub const MAX_FIELD_TRIANGLES: usize = 200_000;
/// Most triangles any asset (a `Shape::Mesh` object) may hold.
pub const MAX_TRIANGLES: usize = 4_000_000;
/// Most points any asset may hold.
pub const MAX_POINTS: usize = 6_000_000;
/// Coordinates beyond this (in file units, before scaling) are refused.
pub const MAX_COORD: f32 = 1.0e6;
/// Most points a clay's mesh asset may hold (three per triangle: an
/// unwelded soup of `MAX_FIELD_TRIANGLES`).
pub const MAX_FIELD_POINTS: usize = 3 * MAX_FIELD_TRIANGLES;
/// Most assets one clay keeps.
pub const MAX_ASSETS: usize = 64;
/// Most triangles all of one clay's assets hold together. A field costs
/// several times its triangles' bytes (corners, pseudo normals, the box
/// tree, the edge map while it is built): five full assets, a few
/// hundred MB, not 64 of them.
pub const MAX_CLAY_TRIANGLES: usize = 5 * MAX_FIELD_TRIANGLES;

pub struct MeshData {
    pub points: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
    hash: String,
    field: OnceLock<Arc<MeshField>>,
}

/// A triangle mesh, shared; `==` compares content hashes.
#[derive(Clone)]
pub struct MeshAsset(Arc<MeshData>);

impl PartialEq for MeshAsset {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.hash == other.0.hash
    }
}

impl std::fmt::Debug for MeshAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MeshAsset({}, {} points, {} triangles)", self.0.hash, self.0.points.len(), self.0.tris.len())
    }
}

type Registry = Mutex<HashMap<String, Weak<MeshData>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The content hash of a mesh: xxh3-128 over its points and triangles.
fn hash_of(points: &[Vec3], tris: &[[u32; 3]]) -> String {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(&(points.len() as u64).to_le_bytes());
    h.update(bytemuck::cast_slice(points));
    h.update(&(tris.len() as u64).to_le_bytes());
    h.update(bytemuck::cast_slice(tris));
    format!("{:032x}", h.digest128())
}

impl MeshAsset {
    /// Checks and keeps a mesh: indices in range, coordinates finite and
    /// within `MAX_COORD`, counts within the caps.
    pub fn new(points: Vec<Vec3>, tris: Vec<[u32; 3]>) -> Result<MeshAsset, String> {
        if points.len() > MAX_POINTS || tris.len() > MAX_TRIANGLES {
            return Err(format!("a mesh may have at most {MAX_POINTS} points and {MAX_TRIANGLES} triangles (this one: {} and {})", points.len(), tris.len()));
        }
        if tris.is_empty() {
            return Err("the mesh has no triangles".into());
        }
        if let Some(p) = points.iter().find(|p| !p.is_finite() || p.abs().max_element() > MAX_COORD) {
            return Err(format!("a point is not finite or beyond {MAX_COORD} ({p})"));
        }
        let n = points.len() as u32;
        if tris.iter().any(|t| t.iter().any(|&i| i >= n)) {
            return Err("a triangle points past the vertex list".into());
        }
        let hash = hash_of(&points, &tris);
        let mut r = registry().lock().expect("no panics while locked");
        if let Some(have) = r.get(&hash).and_then(Weak::upgrade) {
            return Ok(MeshAsset(have));
        }
        let data = Arc::new(MeshData { points, tris, hash: hash.clone(), field: OnceLock::new() });
        if r.len() > 256 {
            r.retain(|_, w| w.strong_count() > 0);
        }
        r.insert(hash, Arc::downgrade(&data));
        Ok(MeshAsset(data))
    }

    /// An asset made in this process that some clay or object still holds.
    pub fn lookup(hash: &str) -> Option<MeshAsset> {
        registry().lock().expect("no panics while locked").get(hash).and_then(Weak::upgrade).map(MeshAsset)
    }

    pub fn hash(&self) -> &str {
        &self.0.hash
    }

    pub fn points(&self) -> &[Vec3] {
        &self.0.points
    }

    pub fn tris(&self) -> &[[u32; 3]] {
        &self.0.tris
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.0.points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
    }

    /// The distance field, built on first use.
    pub fn field(&self) -> Arc<MeshField> {
        self.0.field.get_or_init(|| Arc::new(MeshField::new(&self.0.points, &self.0.tris))).clone()
    }

    /// Bytes held (points and triangles).
    pub fn bytes(&self) -> usize {
        self.0.points.len() * 12 + self.0.tris.len() * 12
    }

    /// The mesh with at most `max` triangles: meshopt's simplifier over
    /// the welded positions (the borders of open meshes stay put); a mesh
    /// already small enough is itself, less any points no triangle uses
    /// (so the result is always within `3 * max` points).
    pub fn decimated(&self, max: usize) -> Result<MeshAsset, String> {
        if self.0.tris.len() <= max {
            if self.0.points.len() <= 3 * self.0.tris.len() && all_used(self.0.points.len(), &self.0.tris) {
                return Ok(self.clone());
            }
            return compact(self.0.points.clone(), self.0.tris.clone());
        }
        let (points, tris) = weld(&self.0.points, &self.0.tris);
        let indices: Vec<u32> = tris.iter().flatten().copied().collect();
        let adapter = meshopt::VertexDataAdapter::new(bytemuck::cast_slice(&points), 12, 0).map_err(|e| e.to_string())?;
        let mut out = meshopt::simplify(&indices, &adapter, max * 3, 1.0, meshopt::SimplifyOptions::LockBorder, None);
        if out.len() > max * 3 {
            out = meshopt::simplify(&indices, &adapter, max * 3, 1.0, meshopt::SimplifyOptions::None, None);
        }
        if out.len() > max * 3 || out.is_empty() {
            return Err(format!("the mesh could not be decimated to {max} triangles"));
        }
        let tris: Vec<[u32; 3]> = out.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
        compact(points, tris)
    }
}

/// Whether every one of `points` points is used by a triangle.
fn all_used(points: usize, tris: &[[u32; 3]]) -> bool {
    let mut used = vec![false; points];
    tris.iter().flatten().for_each(|&i| used[i as usize] = true);
    used.into_iter().all(|u| u)
}

/// Drops unused points and renumbers.
fn compact(points: Vec<Vec3>, tris: Vec<[u32; 3]>) -> Result<MeshAsset, String> {
    let (kept, tris) = compacted(&points, tris);
    MeshAsset::new(kept, tris)
}

/// `points` less those no triangle uses (as they are when all are used,
/// else in first-use order), and the triangles renumbered to them.
pub fn without_unused(points: Vec<Vec3>, tris: Vec<[u32; 3]>) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    if all_used(points.len(), &tris) { (points, tris) } else { compacted(&points, tris) }
}

/// `points` less those no triangle uses, in first-use order, and the
/// triangles renumbered to them.
fn compacted(points: &[Vec3], tris: Vec<[u32; 3]>) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mut remap = vec![u32::MAX; points.len()];
    let mut kept = Vec::new();
    let tris = tris
        .into_iter()
        .map(|t| {
            t.map(|i| {
                let slot = &mut remap[i as usize];
                if *slot == u32::MAX {
                    *slot = kept.len() as u32;
                    kept.push(points[i as usize]);
                }
                *slot
            })
        })
        .collect();
    (kept, tris)
}

/// Points merged where they are bit-equal (STL keeps none shared), and
/// triangles renumbered to them; triangles are all kept.
pub fn weld(points: &[Vec3], tris: &[[u32; 3]]) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let mut at: HashMap<[u32; 3], u32> = HashMap::with_capacity(points.len());
    let mut out = Vec::with_capacity(points.len());
    let remap: Vec<u32> = points
        .iter()
        .map(|p| {
            // -0.0 and 0.0 are one point.
            let key = p.to_array().map(|c| if c == 0.0 { 0 } else { c.to_bits() });
            *at.entry(key).or_insert_with(|| {
                out.push(*p);
                out.len() as u32 - 1
            })
        })
        .collect();
    (out, tris.iter().map(|t| t.map(|i| remap[i as usize])).collect())
}

/// How the edges of a welded mesh are used: (edges, edges used by other
/// than exactly two triangles, edges two triangles use the same way round).
pub fn edge_use(tris: &[[u32; 3]]) -> (usize, usize, usize) {
    let (edges, kinds, flipped) = edge_report(tris);
    (edges, kinds.open + kinds.over_used + kinds.pinched, flipped)
}

/// Why edges of a welded mesh are not used exactly twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EdgeKinds {
    /// Used by one triangle: a hole or a crack.
    pub open: usize,
    /// Used by an odd number of triangles above two.
    pub over_used: usize,
    /// Used by four, six, ... triangles: sheets pinched together at one
    /// pair of positions (each sheet would be closed on its own).
    pub pinched: usize,
}

/// `edge_use`'s non-manifold edges by kind.
#[cfg(test)]
pub fn edge_use_kinds(tris: &[[u32; 3]]) -> EdgeKinds {
    edge_report(tris).1
}

/// (edges, the ones not used twice by kind, edges two triangles use the
/// same way round), in one pass; degenerate triangles are skipped.
pub fn edge_report(tris: &[[u32; 3]]) -> (usize, EdgeKinds, usize) {
    let mut uses: HashMap<(u32, u32), (u32, i32)> = HashMap::with_capacity(tris.len() * 2);
    for t in tris {
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let e = uses.entry((a.min(b), a.max(b))).or_default();
            e.0 += 1;
            e.1 += if a < b { 1 } else { -1 };
        }
    }
    let mut kinds = EdgeKinds::default();
    let mut flipped = 0;
    for &(n, way) in uses.values() {
        match n {
            2 => flipped += (way != 0) as usize,
            1 => kinds.open += 1,
            n if n % 2 == 0 => kinds.pinched += 1,
            _ => kinds.over_used += 1,
        }
    }
    (uses.len(), kinds, flipped)
}

/// Whether a mesh is closed and consistently wound once welded: every
/// edge used by exactly two triangles, in opposite directions.
pub fn watertight(points: &[Vec3], tris: &[[u32; 3]]) -> bool {
    let (_, tris) = weld(points, tris);
    let (edges, bad, flipped) = edge_use(&tris);
    edges > 0 && bad == 0 && flipped == 0
}

/// Enclosed volume (divergence theorem; the sign follows the winding).
pub fn volume(points: &[Vec3], tris: &[[u32; 3]]) -> f64 {
    tris.iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| points[i as usize].as_dvec3());
            a.dot(b.cross(c)) / 6.0
        })
        .sum()
}

#[derive(Serialize, Deserialize)]
struct AssetFile {
    points: usize,
    triangles: usize,
    /// Points (f32 x 3) then triangles (u32 x 3), little-endian, deflated, base64.
    data: String,
}

impl Serialize for MeshAsset {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use std::io::Write;
        let mut raw = Vec::with_capacity(self.bytes());
        for p in &self.0.points {
            for c in p.to_array() {
                raw.extend(c.to_le_bytes());
            }
        }
        for t in &self.0.tris {
            for i in t {
                raw.extend(i.to_le_bytes());
            }
        }
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&raw).map_err(serde::ser::Error::custom)?;
        let data = base64::engine::general_purpose::STANDARD.encode(z.finish().map_err(serde::ser::Error::custom)?);
        AssetFile { points: self.0.points.len(), triangles: self.0.tris.len(), data }.serialize(s)
    }
}

impl AssetFile {
    /// The asset, when its counts are within `max_points` and `max_tris`
    /// (checked before anything is inflated) and within the read's budget.
    fn read(self, max_points: usize, max_tris: usize) -> Result<MeshAsset, String> {
        use std::io::Read;
        if self.points > max_points || self.triangles > max_tris {
            return Err(format!("a mesh asset is larger than allowed (at most {max_points} points and {max_tris} triangles here)"));
        }
        let want = (self.points * 12 + self.triangles * 12) as u64;
        crate::clay::charge_inflate(want)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&self.data).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(&bytes[..]).take(want + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
        if raw.len() as u64 != want {
            return Err("a mesh asset has the wrong size".into());
        }
        let f = |c: &[u8]| f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let u = |c: &[u8]| u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        let (pts, tri) = raw.split_at(self.points * 12);
        let points = pts.chunks_exact(12).map(|c| Vec3::new(f(&c[0..4]), f(&c[4..8]), f(&c[8..12]))).collect();
        let tris = tri.chunks_exact(12).map(|c| [u(&c[0..4]), u(&c[4..8]), u(&c[8..12])]).collect();
        MeshAsset::new(points, tris)
    }
}

impl<'de> Deserialize<'de> for MeshAsset {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The counts bound what is inflated, before anything is.
        AssetFile::deserialize(d)?.read(MAX_POINTS, MAX_TRIANGLES).map_err(serde::de::Error::custom)
    }
}

/// Whether `assets` may all be one clay's: each within the field caps,
/// all together within `MAX_CLAY_TRIANGLES`.
pub fn check_clay_assets<'a>(assets: impl IntoIterator<Item = &'a MeshAsset>) -> Result<(), String> {
    let mut total = 0usize;
    for a in assets {
        if a.tris().len() > MAX_FIELD_TRIANGLES || a.points().len() > MAX_FIELD_POINTS {
            return Err(format!(
                "a clay's mesh asset may have at most {MAX_FIELD_TRIANGLES} triangles and {MAX_FIELD_POINTS} points (this one: {} and {}); to_clay decimates a mesh to that",
                a.tris().len(),
                a.points().len()
            ));
        }
        total += a.tris().len();
    }
    if total > MAX_CLAY_TRIANGLES {
        return Err(format!("a clay's mesh assets may hold at most {MAX_CLAY_TRIANGLES} triangles together (these: {total})"));
    }
    Ok(())
}

/// `Clay::assets` from a file: at most `MAX_ASSETS`, each within the
/// field caps and all within `MAX_CLAY_TRIANGLES` (counts checked before
/// any is inflated), each keyed by its own hash (others are dropped),
/// within the read's inflate budget.
pub fn library<'de, D: serde::Deserializer<'de>>(d: D) -> Result<std::collections::BTreeMap<String, MeshAsset>, D::Error> {
    use serde::de::Error;
    crate::clay::in_inflate_budget_with(d, |d| {
        let map = std::collections::BTreeMap::<String, AssetFile>::deserialize(d)?;
        if map.len() > MAX_ASSETS {
            return Err(D::Error::custom(format!("a clay may keep at most {MAX_ASSETS} mesh assets")));
        }
        if map.values().any(|f| f.points > MAX_FIELD_POINTS || f.triangles > MAX_FIELD_TRIANGLES) {
            return Err(D::Error::custom(format!("a clay's mesh asset may have at most {MAX_FIELD_POINTS} points and {MAX_FIELD_TRIANGLES} triangles")));
        }
        if map.values().map(|f| f.triangles).sum::<usize>() > MAX_CLAY_TRIANGLES {
            return Err(D::Error::custom(format!("a clay's mesh assets may hold at most {MAX_CLAY_TRIANGLES} triangles together")));
        }
        let mut out = std::collections::BTreeMap::new();
        for (k, f) in map {
            let a = f.read(MAX_FIELD_POINTS, MAX_FIELD_TRIANGLES).map_err(D::Error::custom)?;
            if k == a.hash() {
                out.insert(k, a);
            }
        }
        Ok(out)
    })
}

/// How a mesh field tells inside from outside.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Sign {
    /// A closed mesh: the nearest triangle's pseudo normal says.
    #[default]
    Closed,
    /// Any mesh (open, with holes or flipped parts): the generalized
    /// winding number, inside where it passes one half. Holes close.
    Winding,
}

struct Node {
    lo: Vec3,
    hi: Vec3,
    /// Leaf: first index into `order` and count; inner: children.
    a: u32,
    b: u32,
    leaf: bool,
    /// Sum of the triangles' area vectors (half the cross products) and
    /// their area-weighted center, for the far-field winding number.
    n: Vec3,
    c: Vec3,
    /// Radius around `c` that holds the node's box.
    r: f32,
}

/// Signed distance to a triangle mesh (negative inside).
pub struct MeshField {
    tris: Vec<[Vec3; 3]>,
    /// Pseudo normals per triangle: face, edges (ab, bc, ca), corners (a, b, c).
    normals: Vec<[Vec3; 7]>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    pub lo: Vec3,
    pub hi: Vec3,
}

/// A far node counts as one dipole once it is this many of its radii away.
const BETA: f32 = 2.0;

impl MeshField {
    pub fn new(points: &[Vec3], triangles: &[[u32; 3]]) -> MeshField {
        // Pseudo normals need shared edges: weld first (unindexed STL).
        let (points, triangles) = weld(points, triangles);
        let triangles: Vec<[u32; 3]> = triangles.into_iter().filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect();
        let tris: Vec<[Vec3; 3]> = triangles.iter().map(|t| t.map(|i| points[i as usize])).collect();
        let face: Vec<Vec3> = tris.iter().map(|[a, b, c]| (*b - *a).cross(*c - *a).normalize_or_zero()).collect();
        let mut vertex = vec![Vec3::ZERO; points.len()];
        let mut edge: HashMap<(u32, u32), Vec3> = HashMap::with_capacity(triangles.len() * 2);
        for (t, tri) in triangles.iter().enumerate() {
            for k in 0..3 {
                let (i, j, l) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
                let (p, q, r) = (points[i as usize], points[j as usize], points[l as usize]);
                let angle = (q - p).normalize_or_zero().dot((r - p).normalize_or_zero()).clamp(-1.0, 1.0).acos();
                vertex[i as usize] += face[t] * angle;
                *edge.entry((i.min(j), i.max(j))).or_default() += face[t];
            }
        }
        let normals = triangles
            .iter()
            .enumerate()
            .map(|(t, tri)| {
                let e = |k: usize| edge[&(tri[k].min(tri[(k + 1) % 3]), tri[k].max(tri[(k + 1) % 3]))];
                [face[t], e(0), e(1), e(2), vertex[tri[0] as usize], vertex[tri[1] as usize], vertex[tri[2] as usize]]
            })
            .collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes = Vec::new();
        if !tris.is_empty() {
            build(&mut nodes, &mut order, &tris, 0, tris.len(), 0);
        }
        let (lo, hi) = nodes.first().map_or((Vec3::ZERO, Vec3::ZERO), |n| (n.lo, n.hi));
        MeshField { tris, normals, nodes, order, lo, hi }
    }

    /// The signed distance (exact).
    pub fn distance(&self, p: Vec3, sign: Sign) -> f32 {
        self.distance_within(p, f32::MAX, sign)
    }

    /// The signed distance where it is within `bound`; elsewhere `bound`
    /// with the right sign (the tree skips what is farther). Outside the
    /// box it is positive: at least the box's distance.
    pub fn distance_within(&self, p: Vec3, bound: f32, sign: Sign) -> f32 {
        #[cfg(test)]
        tests::COUNTS.with(|c| c.set((c.get().0 + 1, c.get().1)));
        if self.nodes.is_empty() {
            return bound;
        }
        let bound2 = if bound.is_finite() && bound < 1e18 { bound * bound } else { f32::MAX };
        let mut best = (bound2, None::<(Vec3, Vec3)>);
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if box_dist2(p, node.lo, node.hi) >= best.0 {
                continue;
            }
            if node.leaf {
                for &t in &self.order[node.a as usize..(node.a + node.b) as usize] {
                    let (q, feature) = crate::human::closest(p, &self.tris[t as usize]);
                    let d2 = p.distance_squared(q);
                    if d2 < best.0 {
                        best = (d2, Some((q, self.normals[t as usize][feature])));
                    }
                }
            } else {
                let (l, r) = (&self.nodes[node.a as usize], &self.nodes[node.b as usize]);
                let (first, second) = if box_dist2(p, l.lo, l.hi) < box_dist2(p, r.lo, r.hi) { (node.b, node.a) } else { (node.a, node.b) };
                if sp + 2 <= stack.len() {
                    stack[sp] = first;
                    stack[sp + 1] = second;
                    sp += 2;
                }
            }
        }
        // Outside the box nothing is inside (pseudo normals of a closed
        // mesh say so too, and the winding number of a mesh without
        // overlapping same-facing layers stays under one half there), so
        // it is positive there with no winding pass: at least the box's
        // distance, which arrays of meshes cull copies by.
        let outside = box_dist2(p, self.lo, self.hi) > 0.0;
        match best.1 {
            Some((q, n)) => {
                let d = best.0.sqrt();
                let inside = !outside && match sign {
                    Sign::Closed => (p - q).dot(n) < 0.0,
                    Sign::Winding => self.winding(p) > 0.5,
                };
                if inside { -d } else { d }
            }
            None => {
                if !outside && self.winding(p) > 0.5 { -bound } else { bound }
            }
        }
    }

    /// The generalized winding number at `p`: 1 inside a closed mesh
    /// wound outward, 0 outside, between near holes.
    pub fn winding(&self, p: Vec3) -> f32 {
        #[cfg(test)]
        tests::COUNTS.with(|c| c.set((c.get().0, c.get().1 + 1)));
        if self.nodes.is_empty() {
            return 0.0;
        }
        let mut sum = 0.0f64;
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            let d = node.c - p;
            let len = d.length();
            if !node.leaf && len > BETA * node.r {
                sum += (d.dot(node.n) / (len * len * len)) as f64;
                continue;
            }
            if node.leaf {
                for &t in &self.order[node.a as usize..(node.a + node.b) as usize] {
                    sum += solid_angle(p, &self.tris[t as usize]) as f64;
                }
            } else if sp + 2 <= stack.len() {
                stack[sp] = node.a;
                stack[sp + 1] = node.b;
                sp += 2;
            }
        }
        (sum / (4.0 * PI as f64)) as f32
    }
}

/// The solid angle a triangle covers seen from `p` (van Oosterom and
/// Strackee), positive when it winds counter-clockwise seen from outside.
fn solid_angle(p: Vec3, [a, b, c]: &[Vec3; 3]) -> f32 {
    let (a, b, c) = (*a - p, *b - p, *c - p);
    let (la, lb, lc) = (a.length(), b.length(), c.length());
    let det = a.dot(b.cross(c));
    let div = la * lb * lc + a.dot(b) * lc + a.dot(c) * lb + b.dot(c) * la;
    2.0 * det.atan2(div)
}

fn box_dist2(p: Vec3, lo: Vec3, hi: Vec3) -> f32 {
    (lo - p).max(p - hi).max(Vec3::ZERO).length_squared()
}

fn build(nodes: &mut Vec<Node>, order: &mut [u32], tris: &[[Vec3; 3]], start: usize, end: usize, depth: usize) -> u32 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut n, mut c, mut area) = (Vec3::ZERO, Vec3::ZERO, 0.0f32);
    for &t in &order[start..end] {
        let [a, b, cc] = tris[t as usize];
        lo = lo.min(a).min(b).min(cc);
        hi = hi.max(a).max(b).max(cc);
        let av = (b - a).cross(cc - a) * 0.5;
        let w = av.length();
        n += av;
        c += (a + b + cc) / 3.0 * w;
        area += w;
    }
    let c = if area > 0.0 { c / area } else { (lo + hi) * 0.5 };
    let r = (lo - c).abs().max((hi - c).abs()).length();
    let me = nodes.len() as u32;
    nodes.push(Node { lo, hi, a: start as u32, b: (end - start) as u32, leaf: true, n, c, r });
    // The traversal stacks hold 64 nodes: depth stays far below that.
    if end - start <= 4 || depth >= 40 {
        return me;
    }
    let axis = (hi - lo).max_position();
    let center = |t: u32| (tris[t as usize][0] + tris[t as usize][1] + tris[t as usize][2])[axis];
    let mid = (start + end) / 2;
    order[start..end].select_nth_unstable_by(mid - start, |&a, &b| center(a).total_cmp(&center(b)).then(a.cmp(&b)));
    let a = build(nodes, order, tris, start, mid, depth + 1);
    let b = build(nodes, order, tris, mid, end, depth + 1);
    let node = &mut nodes[me as usize];
    (node.a, node.b, node.leaf) = (a, b, false);
    me
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    thread_local! {
        /// Distance lookups and winding-number passes on this thread.
        pub(crate) static COUNTS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
    }

    /// An icosphere of `levels` subdivisions, radius `r`, wound outward.
    pub(crate) fn icosphere(levels: u32, r: f32) -> (Vec<Vec3>, Vec<[u32; 3]>) {
        let t = (1.0 + 5f32.sqrt()) / 2.0;
        let mut pts: Vec<Vec3> = [[-1.0, t, 0.0], [1.0, t, 0.0], [-1.0, -t, 0.0], [1.0, -t, 0.0], [0.0, -1.0, t], [0.0, 1.0, t], [0.0, -1.0, -t], [0.0, 1.0, -t], [t, 0.0, -1.0], [t, 0.0, 1.0], [-t, 0.0, -1.0], [-t, 0.0, 1.0]]
            .iter()
            .map(|p| Vec3::from(*p).normalize())
            .collect();
        let mut tris: Vec<[u32; 3]> = vec![[0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11], [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8], [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9], [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1]];
        for _ in 0..levels {
            let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
            let mut next = Vec::with_capacity(tris.len() * 4);
            let mut m = |a: u32, b: u32, pts: &mut Vec<Vec3>| {
                *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
                    pts.push(((pts[a as usize] + pts[b as usize]) * 0.5).normalize());
                    pts.len() as u32 - 1
                })
            };
            for [a, b, c] in tris {
                let (ab, bc, ca) = (m(a, b, &mut pts), m(b, c, &mut pts), m(c, a, &mut pts));
                next.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
            }
            tris = next;
        }
        (pts.into_iter().map(|p| p * r).collect(), tris)
    }

    #[test]
    fn a_sphere_field_is_signed_and_close() {
        let (p, t) = icosphere(4, 0.5);
        assert!(watertight(&p, &t));
        let a = MeshAsset::new(p, t).unwrap();
        let f = a.field();
        for sign in [Sign::Closed, Sign::Winding] {
            assert!(f.distance(Vec3::ZERO, sign) < -0.49);
            assert!((f.distance(Vec3::new(1.0, 0.0, 0.0), sign) - 0.5).abs() < 1e-3);
            assert!((f.distance_within(Vec3::new(0.0, 0.1, 0.0), 0.05, sign) + 0.05).abs() < 1e-6, "inside, beyond the bound");
            assert!((f.distance_within(Vec3::new(0.0, 3.0, 0.0), 0.05, sign) - 0.05).abs() < 1e-6);
        }
        // The far field is a first-order dipole: within a few percent.
        let (inside, outside) = (f.winding(Vec3::new(0.1, 0.2, 0.0)), f.winding(Vec3::new(0.7, 0.0, 0.0)));
        assert!((inside - 1.0).abs() < 0.03 && outside.abs() < 0.03, "{inside} {outside}");
        // The same mesh is one asset (and one field).
        let (p, t) = icosphere(4, 0.5);
        let b = MeshAsset::new(p, t).unwrap();
        assert!(Arc::ptr_eq(&a.0, &b.0));
        assert!(MeshAsset::lookup(a.hash()).is_some());
    }

    #[test]
    fn assets_save_and_load_and_bad_ones_are_refused() {
        let (p, t) = icosphere(2, 1.0);
        let a = MeshAsset::new(p.clone(), t.clone()).unwrap();
        let text = serde_json::to_string(&a).unwrap();
        let back: MeshAsset = serde_json::from_str(&text).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.points(), a.points());
        assert!(MeshAsset::new(p.clone(), vec![[0, 1, 9999]]).is_err());
        assert!(MeshAsset::new(vec![Vec3::NAN; 3], vec![[0, 1, 2]]).is_err());
        assert!(MeshAsset::new(vec![Vec3::splat(1e9); 3], vec![[0, 1, 2]]).is_err());
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["triangles"] = serde_json::json!(MAX_TRIANGLES + 1);
        assert!(serde_json::from_value::<MeshAsset>(v.clone()).is_err());
        v["triangles"] = serde_json::json!(3);
        assert!(serde_json::from_value::<MeshAsset>(v).is_err(), "wrong size");
    }

    /// A clay keeps field-sized assets only: a file's are refused by
    /// their counts before any is inflated, and a blob naming a larger
    /// asset made in this process (a mesh object's) is refused too.
    #[test]
    fn clay_assets_are_held_to_the_field_caps() {
        let fake = |triangles: usize| json!({ "points": 3, "triangles": triangles, "data": "" });
        let read = |assets: serde_json::Value| serde_json::from_value::<crate::clay::Clay>(json!({ "blobs": [], "assets": assets }));
        let e = read(json!({ "a": fake(MAX_FIELD_TRIANGLES + 1) })).unwrap_err().to_string();
        assert!(e.contains("at most"), "{e}");
        let six: serde_json::Map<String, serde_json::Value> = (0..6).map(|i| (format!("{i}"), fake(MAX_FIELD_TRIANGLES))).collect();
        let e = read(serde_json::Value::Object(six)).unwrap_err().to_string();
        assert!(e.contains("together"), "{e}");
        // A real small asset still reads.
        let (p, t) = icosphere(2, 1.0);
        let small = MeshAsset::new(p, t).unwrap();
        let c = read(json!({ small.hash(): small })).unwrap();
        assert_eq!(c.assets.len(), 1);
        // 327,680 triangles: a mesh object may hold it, a clay may not.
        let (p, t) = icosphere(7, 1.0);
        let big = MeshAsset::new(p, t).unwrap();
        assert!(big.tris().len() > MAX_FIELD_TRIANGLES);
        let text = serde_json::to_value(&big).unwrap();
        assert!(serde_json::from_value::<MeshAsset>(text.clone()).is_ok());
        assert!(read(json!({ big.hash(): text })).is_err());
        let mut clay = crate::clay::Clay::default();
        clay.blobs.push(crate::clay::Blob::of(crate::clay::Form::Mesh { asset: big.hash().into(), sign: Sign::Closed, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 }, crate::clay::Op::Add, [0.8; 3]));
        let e = clay.adopt_assets().unwrap_err();
        assert!(e.contains("to_clay"), "{e}");
        // Nor does its field get built for a blob that names it anyway.
        clay.build_mesh();
        assert!(big.0.field.get().is_none());
    }

    /// An array or shell copying a mesh keeps that mesh in the clay: it
    /// is adopted, survives a save and `sanitize`, and is validated.
    #[test]
    fn nested_mesh_sources_keep_their_asset() {
        use crate::clay::{Blob, Clay, Form, Op};
        let (p, t) = icosphere(2, 0.2);
        let a = MeshAsset::new(p, t).unwrap();
        let mesh = |scale: f32| Form::Mesh { asset: a.hash().into(), sign: Sign::Closed, center: Vec3::ZERO, rotate: Vec3::ZERO, scale };
        let array = |scale: f32| Form::Array { source: Box::new(mesh(scale)), count: 3, step: Vec3::X, radius: 0.0, center: Vec3::ZERO, axis: Vec3::Y, points: Vec::new(), orient: Default::default() };
        let mut clay = Clay::default();
        clay.blobs.push(Blob::of(Form::Shell { source: Box::new(array(1.0)), thickness: 0.02 }, Op::Add, [0.8; 3]));
        assert_eq!(clay.blobs[0].mesh_assets(), vec![a.hash()]);
        clay.adopt_assets().unwrap();
        assert!(clay.assets.contains_key(a.hash()));
        let mut back: Clay = serde_json::from_str(&serde_json::to_string(&clay).unwrap()).unwrap();
        back.sanitize();
        assert_eq!(back.blobs.len(), 1);
        assert!(back.assets.contains_key(a.hash()), "sanitize must keep a nested source's asset");
        // Without the asset in the file the blob goes, not a silent speck.
        let mut bare = back.clone();
        bare.assets.clear();
        bare.sanitize();
        assert!(bare.blobs.is_empty());
        // A nested mesh's placement is validated like a top-level one's.
        assert!(Blob::of(array(f32::NAN), Op::Add, [0.8; 3]).validate().is_err());
        assert!(Blob::of(array(1e9), Op::Add, [0.8; 3]).validate().is_err());
    }

    /// A clay's geometry reads its own assets only: an asset this process
    /// still holds (listed by hash) but the clay does not keep is only a
    /// speck, at the top level or nested; `prune_assets` drops only
    /// assets no blob names.
    #[test]
    fn geometry_reads_only_the_clays_own_assets() {
        use crate::clay::{Blob, Clay, Form, Op};
        let (p, t) = icosphere(2, 0.3);
        let a = MeshAsset::new(p, t).unwrap();
        let (p, t) = icosphere(1, 0.1);
        let other = MeshAsset::new(p, t).unwrap();
        assert!(MeshAsset::lookup(a.hash()).is_some(), "listed while held");
        let mesh = Form::Mesh { asset: a.hash().into(), sign: Sign::Closed, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 };
        let nested = Form::Shell { source: Box::new(Form::Array { source: Box::new(mesh.clone()), count: 3, step: Vec3::X, radius: 0.0, center: Vec3::ZERO, axis: Vec3::Y, points: Vec::new(), orient: Default::default() }), thickness: 0.05 };
        for form in [mesh, nested] {
            let mut clay = Clay { blobs: vec![Blob::of(form, Op::Add, [0.8; 3])], resolution: 32, ..Clay::default() };
            // How far the mesh reaches from the copies' origins (x = 0, 1, 2).
            let reach = |m: crate::mesh::Mesh| m.vertices.iter().map(|v| (0..3).map(|i| Vec3::from(v.pos).distance(Vec3::X * i as f32)).fold(f32::MAX, f32::min)).fold(0.0, f32::max);
            let r = reach(clay.build_mesh());
            assert!(r < 0.02, "specks, not the mesh from the process-wide list: {r}");
            clay.assets.insert(a.hash().into(), a.clone());
            clay.assets.insert(other.hash().into(), other.clone());
            assert!(reach(clay.build_mesh()) > 0.28, "the mesh, from the clay's own");
            clay.prune_assets();
            assert_eq!(clay.assets.keys().collect::<Vec<_>>(), vec![a.hash()]);
        }
    }

    #[test]
    fn decimation_keeps_the_shape() {
        let (p, t) = icosphere(5, 1.0);
        let a = MeshAsset::new(p, t).unwrap();
        let d = a.decimated(4000).unwrap();
        assert!(d.tris().len() <= 4000 && d.tris().len() > 1000, "{}", d.tris().len());
        assert!(watertight(d.points(), d.tris()));
        let v = volume(d.points(), d.tris());
        assert!((v / (4.0 / 3.0 * std::f64::consts::PI) - 1.0).abs() < 0.02, "{v}");
    }
}
