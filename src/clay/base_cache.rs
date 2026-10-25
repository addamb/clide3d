//! `BaseCache`: a clay's blob field (`Field::sample_base`) memoized on a
//! lattice (docs/SCULPT_PLAN.md section 3.6, Stage 6).
//!
//! Lattice point `i` sits at `i * voxel` in object space, as the detail
//! grid's points do, so a sculpt stroke's region and a chunk of the
//! anchored mesher (`chunks.rs`) read the same values. Values are kept in
//! 8 x 8 x 8 bricks, filled when first asked for (in parallel) and only
//! where they are asked for: near the surface, where strokes and the
//! mesher read. Each value is exactly what `sample_base` gives at that
//! point (the blob field is the same whichever prims a caller lists, see
//! `Field::limit`), so a cache changes no result anywhere, warm or cold:
//! it only saves the prim fold.
//!
//! Caches are keyed by everything the blob field reads (the blobs with
//! their mesh assets' hashes, the shell, the decals, the field's clamp and
//! the lattice cell), so a blob, resolution, damage or decal edit simply
//! asks for another key. They live in one registry, least recently used
//! first out past `CAP_BYTES`; holders (the sculpt engine for a stroke,
//! `MeshLib` for clays in a sculpt session) keep theirs alive meanwhile.
//! Never saved, never in undo.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use glam::IVec3;

use super::{Clay, Field, par_chunks};

/// Lattice points per brick side.
pub const SIDE: i32 = 8;
const VALUES: usize = (SIDE * SIDE * SIDE) as usize;

/// Memory every cache together may hold before the least recently used
/// go (holders keep theirs until they let go).
pub const CAP_BYTES: usize = 256 << 20;

/// One cache may hold at most this; past it values are still exact but
/// worked out each time.
pub const MAX_BYTES: usize = 512 << 20;

const BRICK_BYTES: usize = VALUES * 4 + 64;

type Brick = Arc<[f32; VALUES]>;

/// A quick hash for lattice keys (three small ints; the default SipHash
/// is most of a lookup's cost here), seeded per process and mixed to the
/// last bit, so keys read from a file cannot be chosen to collide.
#[derive(Clone, Copy)]
pub struct KeyHasher(u64);

impl Default for KeyHasher {
    fn default() -> Self {
        static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        KeyHasher(*SEED.get_or_init(|| {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(0x9e37_79b9_7f4a_7c15);
            h.finish()
        }))
    }
}

impl std::hash::Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes.chunks(4) {
            let mut w = [0u8; 4];
            w[..b.len()].copy_from_slice(b);
            self.0 = (self.0 ^ u32::from_le_bytes(w) as u64).wrapping_mul(0x517c_c1b7_2722_0a95).rotate_left(23);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = (self.0 ^ n as u64).wrapping_mul(0x517c_c1b7_2722_0a95).rotate_left(23);
    }

    fn finish(&self) -> u64 {
        // SplitMix64's finalizer: every input bit reaches the low bits the
        // table indexes by.
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// A map by lattice key.
pub type KeyMap<V> = HashMap<[i32; 3], V, std::hash::BuildHasherDefault<KeyHasher>>;

/// The blob field of one clay on one lattice.
pub struct BaseCache {
    pub voxel: f32,
    shards: [RwLock<KeyMap<Brick>>; 16],
    /// Per brick: whether the blob surface is surely far from it, and on
    /// which side (`Field::far_side` at its middle; the chunk mesher's
    /// one-value bricks), worked out once.
    sides: RwLock<KeyMap<Option<bool>>>,
    bytes: AtomicUsize,
    /// Bricks worked out (for tests and `get_clay`'s session_cache).
    filled: AtomicUsize,
}

/// What a cache is for: a hash of the blob field's inputs and the cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub field: u128,
    pub voxel: u32,
}

/// The brick holding lattice point `i`, and `i`'s slot in it.
pub fn split(i: IVec3) -> ([i32; 3], usize) {
    // SIDE is 8: an arithmetic shift floors, as div_euclid does.
    const _: () = assert!(SIDE == 8);
    let key = IVec3::new(i.x >> 3, i.y >> 3, i.z >> 3);
    let l = i - key * SIDE;
    (key.to_array(), (l.x + SIDE * (l.y + SIDE * l.z)) as usize)
}

impl BaseCache {
    fn new(voxel: f32) -> BaseCache {
        BaseCache { voxel, shards: std::array::from_fn(|_| RwLock::new(KeyMap::default())), sides: RwLock::new(KeyMap::default()), bytes: AtomicUsize::new(0), filled: AtomicUsize::new(0) }
    }

    fn shard_index(&self, key: &[i32; 3]) -> usize {
        let h = (key[0] as u32).wrapping_mul(0x9E37_79B1) ^ (key[1] as u32).wrapping_mul(0x85EB_CA77) ^ (key[2] as u32).wrapping_mul(0xC2B2_AE3D);
        (h >> 28) as usize
    }

    fn shard(&self, key: &[i32; 3]) -> &RwLock<KeyMap<Brick>> {
        &self.shards[self.shard_index(key)]
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Bricks worked out so far.
    pub fn filled(&self) -> usize {
        self.filled.load(Ordering::Relaxed)
    }

    /// Bricks held.
    pub fn bricks(&self) -> usize {
        self.shards.iter().map(|s| s.read().map_or(0, |m| m.len())).sum()
    }

    #[cfg(test)]
    fn get(&self, key: &[i32; 3]) -> Option<Brick> {
        self.shard(key).read().ok()?.get(key).cloned()
    }

    /// Works out one brick of `field` (its blob field, `sample_base`).
    fn compute(&self, field: &Field, key: [i32; 3]) -> [f32; VALUES] {
        let v = self.voxel;
        let k = IVec3::from(key) * SIDE;
        let (lo, hi) = (k.as_vec3() * v, (k + SIDE - 1).as_vec3() * v);
        let near = field.near(lo - v, hi + v);
        let mut out = [0.0f32; VALUES];
        for z in 0..SIDE {
            for y in 0..SIDE {
                for x in 0..SIDE {
                    let p = (k + IVec3::new(x, y, z)).as_vec3() * v;
                    out[(x + SIDE * (y + SIDE * z)) as usize] = field.sample_base(p, &near).0;
                }
            }
        }
        out
    }

    fn keep(&self, key: [i32; 3], brick: [f32; VALUES]) -> Brick {
        let brick = Arc::new(brick);
        self.filled.fetch_add(1, Ordering::Relaxed);
        if self.bytes() + BRICK_BYTES > MAX_BYTES {
            return brick;
        }
        if let Ok(mut m) = self.shard(&key).write() {
            if let Some(old) = m.get(&key) {
                return old.clone();
            }
            m.insert(key, brick.clone());
            self.bytes.fetch_add(BRICK_BYTES, Ordering::Relaxed);
        }
        brick
    }

    /// The bricks `keys` (each once), worked out in parallel where
    /// missing. `field` must be the field the cache was made for.
    pub fn bricks_of(&self, field: &Field, keys: &[[i32; 3]]) -> KeyMap<Brick> {
        let mut out = KeyMap::with_capacity_and_hasher(keys.len(), Default::default());
        let mut missing = Vec::new();
        // Shard by shard: one lock each, not one per key.
        let mut by_shard: Vec<Vec<[i32; 3]>> = vec![Vec::new(); self.shards.len()];
        for k in keys {
            by_shard[self.shard_index(k)].push(*k);
        }
        for (shard, ks) in self.shards.iter().zip(&by_shard) {
            if ks.is_empty() {
                continue;
            }
            let m = shard.read().unwrap_or_else(|e| e.into_inner());
            for k in ks {
                match m.get(k) {
                    Some(b) => {
                        out.insert(*k, b.clone());
                    }
                    None => missing.push(*k),
                }
            }
        }
        missing.sort_unstable();
        missing.dedup();
        let mut work: Vec<([i32; 3], Option<Brick>)> = missing.into_iter().map(|k| (k, None)).collect();
        par_chunks(&mut work, 1, |_, chunk| {
            for (k, b) in chunk {
                *b = Some(self.keep(*k, self.compute(field, *k)));
            }
        });
        out.extend(work.into_iter().map(|(k, b)| (k, b.expect("filled"))));
        out
    }

    /// For each brick of `keys`: whether the blob surface is surely more
    /// than `far` from it (its middle; Some(true): inside), or nothing is
    /// near it at all (Some(false)); None: it may pass through. Memoized
    /// (a function of the key, as the cache is of one field and cell);
    /// the missing ones are worked out in parallel.
    pub fn sides(&self, field: &Field, keys: &[[i32; 3]], far: f32) -> Vec<Option<bool>> {
        let mut out: Vec<(Option<bool>, bool)> = {
            let m = self.sides.read().unwrap_or_else(|e| e.into_inner());
            keys.iter().map(|k| m.get(k).map_or((None, false), |s| (*s, true))).collect()
        };
        let v = self.voxel;
        par_chunks(&mut out, 64, |chunk, part| {
            for (j, (side, known)) in part.iter_mut().enumerate() {
                if *known {
                    continue;
                }
                let k = IVec3::from(keys[chunk * 64 + j]) * SIDE;
                let (lo, hi) = (k.as_vec3() * v - v, (k + SIDE - 1).as_vec3() * v + v);
                let near = field.near(lo, hi);
                *side = if near.is_empty() { Some(false) } else { field.far_side((lo + hi) * 0.5, &near, far) };
            }
        });
        if out.iter().any(|(_, known)| !known) {
            let mut m = self.sides.write().unwrap_or_else(|e| e.into_inner());
            for (k, (side, known)) in keys.iter().zip(&out) {
                if !known && m.len() < MAX_BYTES / BRICK_BYTES {
                    m.insert(*k, *side);
                }
            }
        }
        out.into_iter().map(|(s, _)| s).collect()
    }

    /// The blob field at lattice point `i`.
    #[cfg(test)]
    pub fn value(&self, field: &Field, i: IVec3) -> f32 {
        let (key, at) = split(i);
        match self.get(&key) {
            Some(b) => b[at],
            None => self.keep(key, self.compute(field, key))[at],
        }
    }

    /// The blob field at lattice points `i0 + (x, y, z)` for x < n.x and
    /// so on, x fastest, into `out` (n.x * n.y * n.z values).
    pub fn block(&self, field: &Field, i0: IVec3, n: IVec3, out: &mut [f32]) {
        let (lo, hi) = (i0.div_euclid(IVec3::splat(SIDE)), (i0 + n - 1).div_euclid(IVec3::splat(SIDE)));
        let mut keys = Vec::new();
        for z in lo.z..=hi.z {
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    keys.push([x, y, z]);
                }
            }
        }
        let bricks = self.bricks_of(field, &keys);
        let (nx, ny) = (n.x as usize, n.y as usize);
        par_chunks(out, nx * ny, |z, layer| {
            for y in 0..ny {
                // A row a brick run at a time: one lookup per run.
                let row = &mut layer[nx * y..nx * (y + 1)];
                let (key, at) = split(i0 + IVec3::new(0, y as i32, z as i32));
                let (ky, kz, base) = (key[1], key[2], at - (i0.x.rem_euclid(SIDE)) as usize);
                let mut x = 0;
                while x < nx {
                    let gx = i0.x + x as i32;
                    let (kx, lx) = (gx.div_euclid(SIDE), gx.rem_euclid(SIDE) as usize);
                    let run = (SIDE as usize - lx).min(nx - x);
                    let b = &bricks[&[kx, ky, kz]];
                    row[x..x + run].copy_from_slice(&b[base + lx..base + lx + run]);
                    x += run;
                }
            }
        });
    }

}

struct Entry {
    key: Key,
    cache: Arc<BaseCache>,
    used: u64,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static CLOCK: AtomicU64 = AtomicU64::new(0);

/// The key of `clay`'s blob field (as `field`, made from it) on a lattice
/// of `voxel`.
pub fn key(clay: &Clay, field: &Field, voxel: f32) -> Key {
    use xxhash_rust::xxh3::Xxh3;
    let mut h = Xxh3::new();
    h.update(b"base field");
    // Mesh assets are named by their content hash in the blobs.
    h.update(&serde_json::to_vec(&(&clay.blobs, clay.shell, &clay.decals)).unwrap_or_default());
    h.update(&field.limit.to_bits().to_le_bytes());
    Key { field: h.digest128(), voxel: voxel.to_bits() }
}

/// The cache for `key`, made if new. Drops the least recently used
/// caches past `CAP_BYTES`.
pub fn get(key: Key) -> Arc<BaseCache> {
    let now = CLOCK.fetch_add(1, Ordering::Relaxed);
    let mut all = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let cache = match all.iter_mut().find(|e| e.key == key) {
        Some(e) => {
            e.used = now;
            e.cache.clone()
        }
        None => {
            let cache = Arc::new(BaseCache::new(f32::from_bits(key.voxel)));
            all.push(Entry { key, cache: cache.clone(), used: now });
            cache
        }
    };
    let mut total: usize = all.iter().map(|e| e.cache.bytes()).sum();
    while total > CAP_BYTES {
        let Some(k) = all.iter().enumerate().filter(|(_, e)| e.key != key).min_by_key(|(_, e)| e.used).map(|(k, _)| k) else { break };
        total -= all[k].cache.bytes();
        all.swap_remove(k);
    }
    cache
}

/// The cache for `clay`'s blob field (`field`) on a lattice of `voxel`.
pub fn for_clay(clay: &Clay, field: &Field, voxel: f32) -> Arc<BaseCache> {
    get(key(clay, field, voxel))
}

/// Forgets every cache (tests of cold costs).
#[cfg(test)]
pub fn clear() {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_values_equal_the_blob_field_whatever_the_prims_listed() {
        let clay = crate::harness::figure(96, true);
        let voxel = 0.004;
        let field = Field::new(&clay, voxel);
        let cache = BaseCache::new(voxel);
        // Points across the torso, arms and feet, through every brick of a
        // block; the direct value takes a tight list as a stroke would.
        let i0 = IVec3::new(-40, 0, -30);
        let n = IVec3::new(80, 120, 60);
        let mut out = vec![0.0; (n.x * n.y * n.z) as usize];
        cache.block(&field, i0 * 2, n, &mut out);
        for z in (0..n.z).step_by(7) {
            for y in (0..n.y).step_by(5) {
                for x in (0..n.x).step_by(3) {
                    let i = i0 * 2 + IVec3::new(x, y, z);
                    let p = i.as_vec3() * voxel;
                    let tight = field.near(p - voxel, p + voxel);
                    let wide = field.near(p - 0.3, p + 0.3);
                    let v = out[(x + n.x * (y + n.y * z)) as usize];
                    assert_eq!(v.to_bits(), field.sample_base(p, &tight).0.to_bits(), "{i}");
                    assert_eq!(v.to_bits(), field.sample_base(p, &wide).0.to_bits(), "{i}");
                    assert_eq!(v.to_bits(), cache.value(&field, i).to_bits());
                }
            }
        }
        assert!(cache.bricks() > 0 && cache.bytes() == cache.bricks() * BRICK_BYTES);
    }

    #[test]
    fn keys_follow_the_blobs_and_the_cell() {
        let a = crate::harness::sphere(0.5, 64);
        let mut b = a.clone();
        b.blobs[0].color = [0.2, 0.3, 0.4];
        let fa = Field::new(&a, 0.01);
        let fb = Field::new(&b, 0.01);
        assert_eq!(key(&a, &fa, 0.01), key(&a.clone(), &fa, 0.01));
        assert_ne!(key(&a, &fa, 0.01), key(&b, &fb, 0.01));
        assert_ne!(key(&a, &fa, 0.01), key(&a, &fa, 0.02));
        assert!(Arc::ptr_eq(&get(key(&a, &fa, 0.01)), &get(key(&a, &fa, 0.01))));
    }
}
