//! The GPU chunk pool (docs/SCULPT_PLAN.md Stage 6): a sculpt session's
//! chunks (`mesh::SculptChunk`, slim 24 B vertices) live in a few large
//! paged buffers (VERTEX | INDEX | COPY_DST), each chunk in a slot of
//! `UNIT` x 2^k bytes, its vertices first, then its indices. A chunk is
//! drawn with its page bound once and `base_vertex` / `first_index`
//! pointing into its slot, so thousands of chunks need no buffer each.
//!
//! The set the pool holds is diffed by Arc pointer against the chunks
//! listed each frame, so a stroke, an undo, a redo or a history jump
//! uploads only the chunks that changed. A page more than 30% free (freed
//! slots no chunk took again) is compacted: its chunks are written again,
//! packed, into a new buffer.

use std::collections::HashMap;
use std::sync::Arc;

use egui_wgpu::wgpu;
use glam::Vec3;

use crate::mesh::{MeshKey, SculptChunk, SculptVertex};

const VERTEX: u64 = std::mem::size_of::<SculptVertex>() as u64;
/// Smallest slot: 64 vertices' worth (a multiple of 24, 4 and 256).
pub const UNIT: u64 = VERTEX * 64;
/// Bytes per page (smaller when the device allows less).
pub const PAGE: u64 = 64 << 20;
/// Free share above which a page is compacted.
pub const COMPACT: f64 = 0.3;

/// Slot bookkeeping, apart from the GPU (so it is testable on its own).
#[derive(Default, Debug)]
pub struct Allocator {
    pub pages: Vec<PageUse>,
}

#[derive(Default, Debug, Clone)]
pub struct PageUse {
    pub size: u64,
    /// The bump pointer: bytes ever handed out from the start.
    pub top: u64,
    /// Bytes in live slots.
    pub live: u64,
    /// Freed slots by class.
    free: Vec<Vec<u64>>,
}

/// Where a slot is: page, byte offset, size class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub page: usize,
    pub offset: u64,
    pub class: u32,
}

/// The size class holding `bytes` (slot size `UNIT << class`).
pub fn class_of(bytes: u64) -> u32 {
    bytes.max(1).div_ceil(UNIT).next_power_of_two().trailing_zeros()
}

impl PageUse {
    /// Share of handed-out bytes no live slot uses.
    pub fn fragmentation(&self) -> f64 {
        if self.top == 0 { 0.0 } else { (self.top - self.live) as f64 / self.top as f64 }
    }
}

impl Allocator {
    /// A slot of at least `bytes`; None when it cannot fit a page of
    /// `page` bytes (then a new page is added and asked again).
    pub fn alloc(&mut self, bytes: u64, page: u64) -> Option<Slot> {
        let class = class_of(bytes);
        let size = UNIT << class;
        if size > page {
            return None;
        }
        for (i, p) in self.pages.iter_mut().enumerate() {
            if let Some(offset) = p.free.get_mut(class as usize).and_then(Vec::pop) {
                p.live += size;
                return Some(Slot { page: i, offset, class });
            }
            if p.top + size <= p.size {
                let offset = p.top;
                p.top += size;
                p.live += size;
                return Some(Slot { page: i, offset, class });
            }
        }
        self.pages.push(PageUse { size: page, ..Default::default() });
        self.alloc(bytes, page)
    }

    pub fn free(&mut self, s: Slot) {
        let p = &mut self.pages[s.page];
        p.live -= UNIT << s.class;
        if p.free.len() <= s.class as usize {
            p.free.resize(s.class as usize + 1, Vec::new());
        }
        p.free[s.class as usize].push(s.offset);
    }

    /// Pages to compact.
    pub fn fragmented(&self) -> Vec<usize> {
        self.pages.iter().enumerate().filter(|(_, p)| p.fragmentation() > COMPACT && p.top > p.size / 8).map(|(i, _)| i).collect()
    }

    /// Empties page `i` (its slots are handed out again by the caller).
    pub fn reset(&mut self, i: usize) {
        let size = self.pages[i].size;
        self.pages[i] = PageUse { size, ..Default::default() };
    }
}

/// One chunk on the GPU.
pub struct Entry {
    pub source: Arc<SculptChunk>,
    pub slot: Slot,
    pub first_index: u32,
    pub index_count: u32,
    pub base_vertex: i32,
    pub bounds: (Vec3, Vec3),
}

#[derive(Default)]
pub struct ChunkPool {
    alloc: Allocator,
    buffers: Vec<wgpu::Buffer>,
    entries: HashMap<MeshKey, Entry>,
    /// What the last `sync` wrote: bytes, chunks and milliseconds (the
    /// partial-upload cost of a stroke).
    pub last_upload: (u64, usize, f64),
}

fn bytes_of(c: &SculptChunk) -> u64 {
    c.vertices.len() as u64 * VERTEX + c.indices.len() as u64 * 4
}

impl ChunkPool {
    pub fn get(&self, key: &MeshKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn buffer(&self, page: usize) -> &wgpu::Buffer {
        &self.buffers[page]
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn write(queue: &wgpu::Queue, buffer: &wgpu::Buffer, slot: Slot, c: &SculptChunk) {
        if !c.vertices.is_empty() {
            queue.write_buffer(buffer, slot.offset, bytemuck::cast_slice(&c.vertices));
        }
        if !c.indices.is_empty() {
            queue.write_buffer(buffer, slot.offset + c.vertices.len() as u64 * VERTEX, bytemuck::cast_slice(&c.indices));
        }
    }

    fn entry(source: Arc<SculptChunk>, slot: Slot) -> Entry {
        let v = source.vertices.len() as u64;
        Entry { first_index: ((slot.offset + v * VERTEX) / 4) as u32, index_count: source.indices.len() as u32, base_vertex: (slot.offset / VERTEX) as i32, bounds: source.bounds, slot, source }
    }

    /// Holds exactly `chunks`: drops the others, uploads new or changed
    /// ones (by pointer), and compacts fragmented pages.
    pub fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, chunks: &[(MeshKey, Arc<SculptChunk>)]) {
        let t = std::time::Instant::now();
        let listed: HashMap<MeshKey, &Arc<SculptChunk>> = chunks.iter().map(|(k, c)| (*k, c)).collect();
        let gone: Vec<MeshKey> = self.entries.iter().filter(|(k, e)| !listed.get(k).is_some_and(|c| Arc::ptr_eq(c, &e.source))).map(|(k, _)| *k).collect();
        for k in gone {
            let e = self.entries.remove(&k).expect("listed");
            self.alloc.free(e.slot);
        }
        let page = PAGE.min(device.limits().max_buffer_size);
        let (mut bytes, mut count) = (0u64, 0usize);
        for (k, c) in chunks {
            if self.entries.contains_key(k) {
                continue;
            }
            let Some(slot) = self.alloc.alloc(bytes_of(c), page) else { continue };
            while self.buffers.len() < self.alloc.pages.len() {
                self.buffers.push(device.create_buffer(&wgpu::BufferDescriptor { label: Some("sculpt chunks"), size: page, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
            }
            Self::write(queue, &self.buffers[slot.page], slot, c);
            bytes += bytes_of(c);
            count += 1;
            self.entries.insert(*k, Self::entry(c.clone(), slot));
        }
        for i in self.alloc.fragmented() {
            // Every chunk on the page, packed again into a new buffer.
            self.alloc.reset(i);
            self.buffers[i] = device.create_buffer(&wgpu::BufferDescriptor { label: Some("sculpt chunks"), size: page, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            let mut moved: Vec<(MeshKey, Arc<SculptChunk>)> = self.entries.iter().filter(|(_, e)| e.slot.page == i).map(|(k, e)| (*k, e.source.clone())).collect();
            moved.sort_by_key(|(k, _)| *k);
            for (k, c) in moved {
                let p = &mut self.alloc.pages[i];
                let size = UNIT << class_of(bytes_of(&c));
                let slot = Slot { page: i, offset: p.top, class: class_of(bytes_of(&c)) };
                p.top += size;
                p.live += size;
                Self::write(queue, &self.buffers[i], slot, &c);
                bytes += bytes_of(&c);
                self.entries.insert(k, Self::entry(c, slot));
            }
        }
        self.last_upload = (bytes, count, t.elapsed().as_secs_f64() * 1000.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_aligned_reused_and_compacted() {
        let mut a = Allocator::default();
        let page = 1 << 20;
        let slots: Vec<Slot> = (0..40).map(|k| a.alloc(1000 + k * 997, page).unwrap()).collect();
        for s in &slots {
            assert_eq!(s.offset % VERTEX, 0);
            assert_eq!(s.offset % 4, 0);
            assert!(s.offset + (UNIT << s.class) <= page);
        }
        // No two live slots overlap.
        for (i, x) in slots.iter().enumerate() {
            for y in &slots[i + 1..] {
                if x.page == y.page {
                    assert!(x.offset + (UNIT << x.class) <= y.offset || y.offset + (UNIT << y.class) <= x.offset);
                }
            }
        }
        // Freed slots are taken again by the same class.
        let s = slots[3];
        a.free(s);
        assert_eq!(a.alloc(UNIT << s.class, page), Some(s));
        // Freeing most of a page marks it for compaction.
        for s in &slots[..30] {
            a.free(*s);
        }
        assert!(!a.fragmented().is_empty());
        assert_eq!(class_of(1), 0);
        assert_eq!(class_of(UNIT + 1), 1);
    }
}
