//! The film path tracer on the GPU (`trace.wgsl`): the same light paths as
//! `radiance`, many times faster. The scene (both ray trees, vertices,
//! materials, textures, environment, lights) goes up once per render; each
//! dispatch adds a few samples to every pixel's sums on the GPU, and the
//! sums come back once at the end for the CPU's denoiser and finishing.
//!
//! The GPU is a headless wgpu device of its own (not the window's), so
//! renders on worker threads and tests use it alike. When there is no GPU,
//! or the scene does not fit its limits, `render` says no and the caller
//! traces on the CPU.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use egui_wgpu::wgpu;
use glam::Vec3;
use wgpu::util::DeviceExt;

use super::{Meshes, View, look::Device};

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    limits: wgpu::Limits,
    pub name: String,
}

const BINDINGS: u32 = 19;
/// The one read-write binding (the running sums).
const ACCUM: u32 = 16;

/// The shared GPU, made on first use, or why there is none usable.
fn shared() -> &'static Result<Gpu, String> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    GPU.get_or_init(|| {
        let made = Gpu::new();
        if let Err(e) = &made {
            eprintln!("film: no GPU tracer ({e}); using the CPU");
        }
        made
    })
}

/// The shared GPU; None when there is none usable.
#[cfg(test)]
pub fn get() -> Option<&'static Gpu> {
    shared().as_ref().ok()
}

impl Gpu {
    fn new() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))
            .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("only a software adapter".into());
        }
        let limits = adapter.limits();
        if limits.max_storage_buffers_per_shader_stage < BINDINGS - 1 {
            return Err(format!("needs {} storage buffers per stage", BINDINGS - 1));
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("film"),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        // Errors outside the first pass's error scope, and a lost device,
        // fail the render they happen in (see `render`) instead of only
        // being printed.
        device.on_uncaptured_error(Arc::new(|e| {
            eprintln!("film GPU error: {e}");
            *FAULT.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
        }));
        device.set_device_lost_callback(|reason, message| {
            eprintln!("film GPU lost ({reason:?}): {message}");
            *LOST.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{reason:?}: {message}"));
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("trace"), source: wgpu::ShaderSource::Wgsl(include_str!("trace.wgsl").into()) });
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            // The settings move per tile (a dynamic offset into one buffer).
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: binding == 0, min_binding_size: None },
            count: None,
        };
        let mut entries = vec![entry(0, wgpu::BufferBindingType::Uniform)];
        entries.extend((1..BINDINGS).map(|b| entry(b, wgpu::BufferBindingType::Storage { read_only: b != ACCUM })));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("trace"), entries: &entries });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("trace"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("trace"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(e) = block_on(device.pop_error_scope()) {
            return Err(format!("trace shader: {e}"));
        }
        Ok(Gpu { device, queue, pipeline, layout, limits, name: format!("{} ({:?})", info.name, info.backend) })
    }

    fn storage(&self, label: &str, bytes: &[u8]) -> Result<wgpu::Buffer, String> {
        // Bindings need room for one element; empty lists get zeros (larger
        // than any element type in trace.wgsl).
        let padded;
        let bytes = if bytes.len() < 512 {
            padded = { let mut v = bytes.to_vec(); v.resize(512, 0); v };
            &padded[..]
        } else {
            bytes
        };
        check_size(label, bytes.len() as u64, binding_limit(&self.limits))?;
        Ok(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytes, usage: wgpu::BufferUsages::STORAGE }))
    }
}

/// Small blocking executor for wgpu's futures (they finish on their own
/// on native backends; this just waits for them).
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Wake(std::thread::Thread);
    impl std::task::Wake for Wake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Item {
    m: [[f32; 4]; 3],
    n: [[f32; 4]; 3],
    tint: [f32; 4],
    surface: [f32; 4],
    info: [u32; 4],
    tex: [u32; 4],
    imp: [u32; 4],
    imp_emissive: [f32; 4],
    imp_cut: [f32; 4],
    /// Smart shading (`smart.rs`): per-vertex data offset into `floats`
    /// (relative to the item's first vertex, 8 per vertex; NONE: none),
    /// first decal (in `floats`), decal count.
    smart: [u32; 4],
    /// Face (`face.rs`): first float of `Face::pack` (NONE: none), line
    /// count, mole count.
    face: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    eye: [f32; 4],
    forward: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    size: [u32; 4],
    opts: [u32; 4],
    env: [f32; 4],
    env_size: [u32; 4],
    sun_dir: [f32; 4],
    sun_power: [f32; 4],
    scalp_lo: [f32; 4],
    scalp_hi: [f32; 4],
    counts: [u32; 4],
    fog: [f32; 4],
    fog_color: [f32; 4],
    light_info: [f32; 4],
    lens_shape: [f32; 4],
    fog_noise: [f32; 4],
}

const NONE: u32 = u32::MAX;

/// Pixels per side of one dispatch, and samples per dispatch: a single
/// GPU job must stay short (macOS stops a long one, and wgpu then never
/// returns), so a dispatch is one sample of one tile, at most a fraction
/// of a second even under dense hair.
const TILE: u32 = 128;
/// Without hair strands a sample is cheap: bigger tiles keep the GPU full.
const TILE_NO_STRANDS: u32 = 256;
/// Samples per pass (each its own dispatches), and dispatches per submit.
const MAX_BATCH: u32 = 16;
const DISPATCHES_PER_SUBMIT: usize = 8;
/// The longest wait for one pass before the GPU counts as stuck.
const STUCK: std::time::Duration = std::time::Duration::from_secs(30);

/// Set when the GPU once got stuck: later renders use the CPU, unless one
/// asks for the GPU by name (which tries it again).
static BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The latest GPU error no error scope caught (cleared per render).
static FAULT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Why the device was lost, once it is: no later render can use it.
static LOST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Takes the uncaught GPU error since the last call, if any.
fn take_fault() -> Option<String> {
    FAULT.lock().unwrap_or_else(|p| p.into_inner()).take()
}

fn lost() -> Option<String> {
    LOST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The traversal stack in `trace.wgsl` (`array<u32, 32>`): a deeper ray
/// tree can drop far boxes on the GPU when the stack fills.
const GPU_STACK: usize = 32;

/// The most bytes one storage binding can hold on this GPU.
fn binding_limit(limits: &wgpu::Limits) -> u64 {
    (limits.max_storage_buffer_binding_size as u64).min(limits.max_buffer_size)
}

fn check_size(label: &str, bytes: u64, limit: u64) -> Result<(), String> {
    if bytes > limit {
        return Err(format!("{label}: {} is more than one GPU buffer holds ({})", show_bytes(bytes), show_bytes(limit)));
    }
    Ok(())
}

/// Bytes for people: "512 KB", "40.0 MB", "5.12 GB".
fn show_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1 << 10)),
    }
}

/// The sizes of the largest GPU buffers of a render, counted from the
/// scene before anything is packed or uploaded, so a scene the GPU cannot
/// hold is refused in milliseconds and the CPU keeps the whole time limit.
/// (Textures, smart data and the per-view lists are checked exactly when
/// packed, still before any buffer is made.)
pub(crate) struct BufferPlan {
    pub entries: Vec<(&'static str, u64)>,
}

impl BufferPlan {
    /// From counts: ray-tree nodes, triangles, vertices (once per distinct
    /// mesh: items sharing one share its vertices), and each view's size.
    pub(crate) fn from_counts(nodes: u64, tris: u64, verts: u64, views: &[(usize, usize)]) -> BufferPlan {
        let mut entries = vec![("ray tree nodes", nodes * 32), ("triangles", tris * 48), ("triangle data", tris * 16), ("vertices", verts * 80)];
        // The running sums: color, albedo and normal per pixel.
        entries.extend(views.iter().map(|&(w, h)| ("picture sums", (w * h * 48) as u64)));
        BufferPlan { entries }
    }

    pub(crate) fn of(meshes: &Meshes, views: &[(usize, usize)]) -> BufferPlan {
        let mut seen = std::collections::HashSet::new();
        let verts = meshes.items.iter().filter(|it| seen.insert(Arc::as_ptr(&it.mesh))).map(|it| it.mesh.vertices.len() as u64).sum();
        Self::from_counts(meshes.bvh.node_count() as u64, meshes.tris.len() as u64, verts, views)
    }

    /// Err naming the first buffer larger than one binding of `limits`.
    pub(crate) fn check(&self, limits: &wgpu::Limits) -> Result<(), String> {
        let limit = binding_limit(limits);
        self.entries.iter().try_for_each(|&(label, bytes)| check_size(label, bytes, limit))
    }
}

fn tile_side(view: &View) -> u32 {
    if view.scene.hair.is_empty() && view.scene.look.volumes.is_empty() { TILE_NO_STRANDS } else { TILE }
}

/// Bytes between tiles' settings in the uniform buffer.
fn stride(gpu: &Gpu) -> u64 {
    (std::mem::size_of::<Params>() as u64).next_multiple_of(gpu.limits.min_uniform_buffer_offset_alignment as u64)
}

/// The meshes, materials and textures of a render, on the GPU.
struct SceneBuffers {
    nodes: wgpu::Buffer,
    tris: wgpu::Buffer,
    tri_info: wgpu::Buffer,
    verts: wgpu::Buffer,
    items: wgpu::Buffer,
    mats: wgpu::Buffer,
    texels: wgpu::Buffer,
    floats: wgpu::Buffer,
    details: wgpu::Buffer,
    node_count: u32,
}

fn wrap_mode(mode: u32) -> u32 {
    match mode {
        10497 => 1,
        33648 => 2,
        _ => 0,
    }
}

fn upload_meshes(gpu: &Gpu, meshes: &Meshes) -> Result<SceneBuffers, String> {
    let (nodes, tris) = meshes.bvh.gpu();
    let mut verts: Vec<[f32; 20]> = Vec::new();
    let mut bases = Vec::with_capacity(meshes.items.len());
    let mut items = Vec::with_capacity(meshes.items.len());
    let mut mats: Vec<[f32; 24]> = Vec::new();
    let mut texels: Vec<u32> = Vec::new();
    // The white-fiber table for hair comes first (see trace.wgsl).
    let mut floats: Vec<f32> = super::shade::white_fiber_table().to_vec();
    // Detail maps in use: (library index, slot); their texels follow the
    // textures in `texels`, all levels, 2 words per texel.
    let mut maps = DetailSlots::default();
    // Textures shared between items go up once.
    let mut uploaded: Vec<(*const u8, u32)> = Vec::new();
    let mut put = |data: &[u8], texels: &mut Vec<u32>| -> u32 {
        if let Some(&(_, at)) = uploaded.iter().find(|(p, _)| *p == data.as_ptr()) {
            return at;
        }
        let at = texels.len() as u32;
        texels.extend(data.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
        uploaded.push((data.as_ptr(), at));
        at
    };
    // A material: (emission, transmission, ior, subsurface), (scatter,
    // skin specular), (pores, mottling, hair, 0), (detail slot or -1,
    // tiles per unit, strength, color), (rotation cos, sin, first smart
    // layer in floats, layer count), (metallic, clearcoat, sheen,
    // roughness).
    let mut material = |m: &crate::material::Material, texels: &mut Vec<u32>, floats: &mut Vec<f32>| -> [f32; 24] {
        let mut d = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        if let Some((layer, freq, turn)) = crate::detail::layout(m)
            && let Some(slot) = maps.slot(layer, texels)
        {
            d = [slot as f32, freq, m.detail_strength, m.detail_color, turn.x, turn.y, 0.0, 0.0];
        }
        if !m.layers.is_empty() {
            d[6] = f32::from_bits(floats.len() as u32);
            d[7] = m.layers.len() as f32;
            for l in &m.layers {
                smart_layer(m, l, &mut maps, texels, floats);
            }
        }
        [m.emission, m.transmission, m.ior, m.subsurface, m.scatter_radius().x, m.scatter_radius().y, m.scatter_radius().z, m.skin_specular, m.pores, m.mottling, if m.name == "hair" { 1.0 } else { 0.0 }, if m.name == "flame" { 1.0 } else { 0.0 },
            d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], m.metallic, m.clearcoat, m.sheen, m.roughness]
    };
    // Vertices are in object space (each item has its own matrix): items
    // sharing a mesh (copies, identical clay) share its vertices. Per-item
    // data indexed by vertex (smart inputs, imported alpha) is written per
    // item, relative to the shared base.
    let mut vert_bases: std::collections::HashMap<*const crate::mesh::Mesh, u32> = std::collections::HashMap::new();
    for it in &meshes.items {
        let base = *vert_bases.entry(Arc::as_ptr(&it.mesh)).or_insert_with(|| {
            let base = verts.len() as u32;
            verts.extend(it.mesh.vertices.iter().map(|v| {
                [
                    v.pos[0], v.pos[1], v.pos[2], v.color[0],
                    v.normal[0], v.normal[1], v.normal[2], v.color[1],
                    v.uv[0], v.uv[1], v.color[2], f32::from_bits(v.material),
                    v.tangent[0], v.tangent[1], v.tangent[2], v.tangent[3],
                    v.surface[0], v.surface[1], v.surface[2], v.surface[3],
                ]
            }));
            base
        });
        bases.push(base);
        let object = mats.len() as u32;
        let face = it.mesh.face.as_ref().filter(|_| it.mesh.textures.is_none());
        let mut push = |m: &crate::material::Material, texels: &mut Vec<u32>, floats: &mut Vec<f32>| {
            let packed = material(m, texels, floats);
            mats.push(packed);
        };
        push(&it.material, &mut texels, &mut floats);
        for m in &it.mesh.materials {
            push(m, &mut texels, &mut floats);
        }
        let mut face_info = [NONE, 0, 0, 0];
        if let Some(f) = face {
            let (lines, moles) = f.counts();
            face_info = [floats.len() as u32, lines, moles, 0];
            floats.extend(f.pack());
        }
        // Smart shading inputs per vertex, and the decals.
        let mut smart = [NONE, 0, 0, 0];
        let decals = &it.mesh.decals;
        if it.mesh.textures.is_none() && (crate::smart::layered(&it.material) || it.mesh.materials.iter().any(crate::smart::layered) || !decals.is_empty()) {
            smart[0] = (floats.len() as u32).wrapping_sub(base.wrapping_mul(8));
            floats.extend(it.mesh.vertices.iter().flat_map(|v| [v.base[0], v.base[1], v.base[2], v.base[3], v.wear[0], v.wear[1], v.wear[2], f32::from_bits(u32::from_le_bytes(v.painted))]));
            smart[1] = floats.len() as u32;
            smart[2] = decals.len() as u32;
            for d in decals.iter() {
                smart_decal(d, &mut texels, &mut floats);
            }
        }
        let mut flags = 0;
        let mut tex = [0; 4];
        if let Some(t) = &it.mesh.textures {
            flags |= 1;
            if t.alpha {
                flags |= 2;
            }
            tex = [t.size, put(&t.color, &mut texels), put(&t.normal, &mut texels), put(&t.orm, &mut texels)];
        }
        let mut imp = [NONE, 0, NONE, 0];
        let mut imp_emissive = [0.0; 4];
        let mut imp_cut = [-1.0; 4];
        if let Some(m) = &it.mesh.imported {
            flags |= 4;
            if let Some(e) = &m.emissive_texture {
                imp[0] = put(e, &mut texels);
            }
            imp[1] = m.size;
            if !m.vertex_alpha.is_empty() {
                imp[2] = (floats.len() as u32).wrapping_sub(base);
                floats.extend((0..it.mesh.vertices.len()).map(|i| m.vertex_alpha.get(i).copied().unwrap_or(1.0)));
            }
            imp[3] = m.wraps.iter().enumerate().fold(0, |acc, (slot, w)| acc | (wrap_mode(w[0]) | wrap_mode(w[1]) << 2) << (slot * 4));
            imp_emissive = [m.emissive[0], m.emissive[1], m.emissive[2], m.base_alpha];
            imp_cut[0] = m.alpha_cutoff.unwrap_or(-1.0);
        }
        let col = |m: glam::Mat3, c: usize| { let v = m.col(c); [v.x, v.y, v.z, 0.0] };
        items.push(Item {
            m: [col(it.model, 0), col(it.model, 1), col(it.model, 2)],
            n: [col(it.normal, 0), col(it.normal, 1), col(it.normal, 2)],
            tint: [it.tint.x, it.tint.y, it.tint.z, 0.0],
            surface: it.surface,
            info: [object, object + 1, it.mesh.materials.len() as u32, flags],
            tex,
            imp,
            imp_emissive,
            imp_cut,
            smart,
            face: face_info,
        });
    }
    let info: Vec<[u32; 4]> = meshes.tris.iter().map(|t| {
        let b = bases[t.item as usize];
        [t.item, b + t.v[0], b + t.v[1], b + t.v[2]]
    }).collect();
    Ok(SceneBuffers {
        node_count: nodes.len() as u32,
        nodes: gpu.storage("nodes", bytemuck::cast_slice(&nodes))?,
        tris: gpu.storage("triangles", bytemuck::cast_slice(&tris))?,
        tri_info: gpu.storage("triangle data", bytemuck::cast_slice(&info))?,
        verts: gpu.storage("vertices", bytemuck::cast_slice(&verts))?,
        items: gpu.storage("items", bytemuck::cast_slice(&items))?,
        mats: gpu.storage("materials", bytemuck::cast_slice(&mats))?,
        texels: gpu.storage("textures", bytemuck::cast_slice(&texels))?,
        floats: gpu.storage("shading data", bytemuck::cast_slice(&floats))?,
        details: gpu.storage("detail maps", bytemuck::cast_slice(&maps.details))?,
    })
}

/// Library detail maps uploaded for one render: which library entry is in
/// which slot, and each slot's header for `trace.wgsl` (`DMap`).
#[derive(Default)]
struct DetailSlots {
    library: Vec<usize>,
    details: Vec<[f32; 12]>,
}

impl DetailSlots {
    /// The slot of library map `layer`, uploading its texels the first time.
    fn slot(&mut self, layer: usize, texels: &mut Vec<u32>) -> Option<usize> {
        if let Some(s) = self.library.iter().position(|&l| l == layer) {
            return Some(s);
        }
        let map = crate::detail::map(crate::detail::LIBRARY.get(layer)?.name)?;
        let (levels, mean, rough, ao, gain) = map.gpu();
        let at = texels.len() as u32;
        for level in &levels {
            texels.extend(level.iter().flat_map(|t| [u32::from_le_bytes([t[0], t[1], t[2], t[3]]), u32::from_le_bytes([t[4], t[5], t[6], t[7]])]));
        }
        let (height_mean, height_spread) = map.height_stats();
        self.details.push([f32::from_bits(at), f32::from_bits(map.size() as u32), f32::from_bits(levels.len() as u32), 0.0, mean.x, mean.y, mean.z, rough, ao, gain, height_mean, height_spread]);
        self.library.push(layer);
        Some(self.library.len() - 1)
    }
}

/// One smart layer for `trace.wgsl` (`smart_layers`): 7 vec4 of settings,
/// then 3 per mask term.
fn smart_layer(m: &crate::material::Material, l: &crate::smart::Layer, maps: &mut DetailSlots, texels: &mut Vec<u32>, floats: &mut Vec<f32>) {
    let [opacity, threshold, contrast, power] = crate::smart::effective(m, l);
    let flag = |on: bool, bit: u32| if on { bit } else { 0 };
    let flags = flag(l.color.is_some(), 1) | flag(l.tint.is_some(), 2) | flag(l.roughness.is_some(), 4) | flag(l.metallic.is_some(), 8) | flag(l.clearcoat.is_some(), 16) | flag(l.sheen.is_some(), 32) | flag(l.emission.is_some(), 64);
    let color = l.color.unwrap_or([0.0; 3]);
    let tint = l.tint.unwrap_or([1.0; 3]);
    let mut detail = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    if let Some(name) = &l.detail
        && let Some((layer, freq, turn)) = crate::detail::place(name, l.detail_scale, l.detail_rotate)
        && let Some(slot) = maps.slot(layer, texels)
    {
        detail = [slot as f32, freq, l.detail_strength, l.detail_color, turn.x, turn.y];
    }
    let var_scale = if l.variation_scale > 0.0 { l.variation_scale } else { 0.08 };
    floats.extend([
        color[0], color[1], color[2], flags as f32,
        tint[0], tint[1], tint[2], opacity,
        l.roughness.unwrap_or(0.0), l.metallic.unwrap_or(0.0), l.clearcoat.unwrap_or(0.0), l.sheen.unwrap_or(0.0),
        l.emission.unwrap_or(0.0), l.variation, threshold, contrast,
        detail[0], detail[1], detail[2], detail[3],
        detail[4], detail[5], l.mask.len() as f32, power,
        1.0 / var_scale, if l.invert { 1.0 } else { 0.0 }, 0.0, 0.0,
    ]);
    for t in &l.mask {
        use crate::smart::Source;
        let (threshold, contrast) = (t.threshold.unwrap_or(0.5), t.contrast.unwrap_or(if t.threshold.is_some() { 1.0 } else { 0.0 }));
        let mut freq = t.freq();
        let params = match t.source {
            Source::Direction => {
                let d = Vec3::from(t.direction).normalize_or(Vec3::Y);
                [d.x, d.y, d.z]
            }
            Source::Height => [t.range[0], t.range[1], 0.0],
            Source::HeightPlane | Source::DirectionMetric | Source::Streaks => t.direction,
            Source::Painted => [t.slot as f32, 0.0, 0.0],
            Source::Constant => [t.value, 0.0, 0.0],
            Source::Grunge => match t.grunge().and_then(|(map, f)| Some((maps.slot(map, texels)?, f))) {
                Some((slot, f)) => {
                    freq = f;
                    [slot as f32, 0.0, 0.0]
                }
                None => [-1.0, 0.0, 0.0],
            },
            _ => [0.0; 3],
        };
        floats.extend([
            t.source as u32 as f32, t.op as u32 as f32, if t.invert { 1.0 } else { 0.0 }, t.amount,
            threshold, contrast, freq, t.octaves.clamp(1, 6) as f32,
            params[0], params[1], params[2], if t.source==Source::HeightPlane {t.value}else{t.seed},
            t.metric[0],t.metric[1],t.metric[2],t.metric[3],
            t.metric[4],t.metric[5],0.0,0.0,
        ]);
    }
}

/// One decal for `trace.wgsl` (`smart_decals`): 7 vec4.
fn smart_decal(d: &crate::smart::Decal, texels: &mut Vec<u32>, floats: &mut Vec<f32>) {
    use crate::smart::Shape;
    let (tu, tv) = d.frame();
    let mut flags = 0u32;
    let mut color = [0.0; 3];
    match (d.color, d.shape) {
        (Some(c), _) => {
            flags |= 1;
            color = c;
        }
        (None, Shape::Image) => flags |= 16,
        (None, Shape::Splat) => {
            flags |= 1;
            color = [0.28, 0.02, 0.02];
        }
        (None, Shape::Scratch) => flags |= 8,
        (None, Shape::Rune) => flags |= 32,
        _ => {}
    }
    let rough = d.roughness.or(match d.shape {
        Shape::Splat => Some(0.2),
        Shape::Scratch => Some(0.25),
        _ => None,
    });
    if rough.is_some() {
        flags |= 2;
    }
    if d.metallic.is_some() {
        flags |= 4;
    }
    let mut image = [0u32, 0];
    if let Some(p) = d.picture() {
        image = [texels.len() as u32, p.size];
        texels.extend(p.rgba.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    }
    let image_data=if (11..=13).contains(&d.shape.code()) {d.flow.unwrap_or([0.0,1.0])}else{[f32::from_bits(image[0]),f32::from_bits(image[1])]};
    floats.extend([
        d.center[0], d.center[1], d.center[2], d.size,
        d.normal[0], d.normal[1], d.normal[2], d.relief(),
        tu.x, tu.y, tu.z, d.shape.code() as f32,
        tv.x, tv.y, tv.z, f32::from_bits(d.seed),
        color[0], color[1], color[2], flags as f32,
        rough.unwrap_or(0.0), d.metallic.unwrap_or(0.0), d.emission, d.opacity,
        image_data[0], image_data[1], if d.melted_lip {1.0}else{0.0}, d.softness,
    ]);
}

/// Scalp cell hash, the same as `cell_hash` in `trace.wgsl`.
fn cell_hash(x: i32, y: i32, z: i32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ (z as u32).wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// One view on the GPU: its hair, settings and running sums.
struct GpuView {
    params: Params,
    uniform: wgpu::Buffer,
    accum: wgpu::Buffer,
    group: wgpu::BindGroup,
}

fn upload_view(gpu: &Gpu, scene: &SceneBuffers, view: &View, bounces: u32) -> Result<GpuView, String> {
    let sc = &view.scene;
    let (hair_nodes, hair_tris) = sc.hair_bvh.gpu();
    let segs: Vec<[f32; 8]> = sc.hair.iter().map(|h| [h.tangent.x, h.tangent.y, h.tangent.z, 0.0, h.color.x, h.color.y, h.color.z, 0.0]).collect();
    // Scalp tint cells in an open-addressing table keyed like the shader.
    let cells = &sc.scalp.grid;
    let size = if cells.is_empty() { 1 } else { (cells.len() * 4).next_power_of_two().max(2) };
    let mask = if cells.is_empty() { 0 } else { size as u32 - 1 };
    let mut table = vec![[0u32; 12]; size];
    let mut longest = 0;
    for (&(x, y, z), &(root, color, count)) in cells {
        let mut slot = cell_hash(x, y, z) & mask;
        let mut probes = 0;
        while table[slot as usize][3] != 0 {
            slot = (slot + 1) & mask;
            probes += 1;
        }
        longest = longest.max(probes);
        table[slot as usize] = [x as u32, y as u32, z as u32, 1, root.x.to_bits(), root.y.to_bits(), root.z.to_bits(), count.to_bits(), color.x.to_bits(), color.y.to_bits(), color.z.to_bits(), 0];
    }
    if std::env::var("FILM_GPU_DEBUG").is_ok() { eprintln!("scalp: {} cells in {size} slots, longest run {longest}", sc.scalp.grid.len()); }
    let look = &sc.look;
    // power.w: how bright the light is for picking (`Light::local`).
    let lights: Vec<[f32; 16]> = look.lights.iter().map(|l| {
        let lum = l.local().map_or(0.0, |(_, _, lum)| lum);
        match *l {
            super::lighting::Light::Sun { direction, size, power } => [0.0, 0.0, 0.0, 0.0, direction.x, direction.y, direction.z, size, 0.0, 0.0, 0.0, 0.0, power.x, power.y, power.z, 0.0],
            super::lighting::Light::Point { position, power } => [f32::from_bits(1), 0.0, 0.0, 0.0, position.x, position.y, position.z, 0.0, 0.0, 0.0, 0.0, 0.0, power.x, power.y, power.z, lum],
            super::lighting::Light::Area { position, direction, size, power } => [f32::from_bits(2), 0.0, 0.0, 0.0, position.x, position.y, position.z, size, direction.x, direction.y, direction.z, 0.0, power.x, power.y, power.z, lum],
            super::lighting::Light::Sphere { position, radius, power } => [f32::from_bits(3), 0.0, 0.0, 0.0, position.x, position.y, position.z, radius, 0.0, 0.0, 0.0, 0.0, power.x, power.y, power.z, lum],
        }
    }).collect();
    let fog = look.fog.map_or(([0.0; 4], [0.0; 4]), |f| ([f.density, f.height, f.falloff, f.anisotropy], [f.albedo.x, f.albedo.y, f.albedo.z, f.distance]));
    let env = look.environment.gpu();
    let (ew, eh, cdf, lit, rotate, sun, sun_chance) = (env.width, env.height, env.cdf, env.lit, env.rotate, env.sun, env.sun_chance);
    let env_pixels: Vec<[f32; 4]> = env.pixels.iter().map(|p| [p.x, p.y, p.z, 0.0]).collect();
    let (sun_dir, sun_power) = match sun {
        Some((d, size, power)) => ([d.x, d.y, d.z, size], [power.x, power.y, power.z, 0.0]),
        None => ([0.0, 1.0, 0.0, 0.0], [0.0; 4]),
    };
    let (lo, hi) = sc.scalp.bounds;
    let params = Params {
        eye: [view.eye.x, view.eye.y, view.eye.z, view.half],
        forward: [view.forward.x, view.forward.y, view.forward.z, view.ortho.unwrap_or(-1.0)],
        right: [view.right.x, view.right.y, view.right.z, view.lens.radius],
        up: [view.up.x, view.up.y, view.up.z, view.lens.focus],
        size: [view.w as u32, view.h as u32, 0, 1],
        opts: [bounces, sc.floor_y.is_some() as u32, look.background as u32, lights.len() as u32],
        env: [sc.floor_y.unwrap_or(0.0), rotate, if lit { 1.0 } else { 0.0 }, sun_chance],
        env_size: [ew as u32, eh as u32, sun.is_some() as u32, mask],
        sun_dir,
        sun_power,
        scalp_lo: [lo.x, lo.y, lo.z, 0.0],
        scalp_hi: [hi.x, hi.y, hi.z, 0.0],
        counts: [scene.node_count, hair_nodes.len() as u32, 0, 0],
        lens_shape: [view.lens.blades as f32, view.lens.rotation, 0.0, 0.0],
        fog_noise: look.fog.map_or([0.0; 4], |f| [f.noise, f.noise_scale, f.seed as f32, 0.0]),
        fog: fog.0,
        fog_color: fog.1,
        light_info: [look.environment.irradiance(), sc.lights.local.len() as f32, 0.0, 0.0],
    };
    let tile = tile_side(view);
    let tiles = (view.w as u32).div_ceil(tile) * (view.h as u32).div_ceil(tile);
    let stride = stride(gpu);
    let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("params"), size: stride * (tiles * MAX_BATCH) as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let accum_size = (view.w * view.h * 3 * 16) as u64;
    check_size("picture sums", accum_size, binding_limit(&gpu.limits))?;
    let accum = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("sums"), size: accum_size, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let volume_data = crate::fire::pack(&look.volumes);
    let own = [
        gpu.storage("hair nodes", bytemuck::cast_slice(&hair_nodes))?,
        gpu.storage("hair triangles", bytemuck::cast_slice(&hair_tris))?,
        gpu.storage("hair segments", bytemuck::cast_slice(&segs))?,
        gpu.storage("environment", bytemuck::cast_slice(&env_pixels))?,
        gpu.storage("environment cdf", bytemuck::cast_slice(&cdf))?,
        gpu.storage("lights", bytemuck::cast_slice(&lights))?,
        gpu.storage("scalp", bytemuck::cast_slice(&table))?,
        gpu.storage("fire volumes", bytemuck::cast_slice(&volume_data))?,
    ];
    let buffers: [&wgpu::Buffer; BINDINGS as usize] = [
        &uniform, &scene.nodes, &scene.tris, &scene.tri_info, &scene.verts, &scene.items, &scene.mats, &scene.texels, &scene.floats,
        &own[0], &own[1], &own[2], &own[3], &own[4], &own[5], &own[6], &accum, &scene.details, &own[7],
    ];
    gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry {
        binding: i as u32,
        resource: if i == 0 {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: b, offset: 0, size: std::num::NonZeroU64::new(std::mem::size_of::<Params>() as u64) })
        } else {
            b.as_entire_binding()
        },
    }).collect();
    let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("trace"), layout: &gpu.layout, entries: &entries });
    if let Some(e) = block_on(gpu.device.pop_error_scope()) {
        return Err(format!("GPU error binding the scene: {e}"));
    }
    Ok(GpuView { params, uniform, accum, group })
}

/// Why the GPU did not trace a render: the CPU was asked for, or the
/// reason it could not (no adapter, a buffer over its limit, stuck, ...).
pub(super) enum Refusal {
    CpuAsked,
    Because(String),
}

impl From<String> for Refusal {
    fn from(reason: String) -> Refusal {
        Refusal::Because(reason)
    }
}

impl From<&str> for Refusal {
    fn from(reason: &str) -> Refusal {
        Refusal::Because(reason.into())
    }
}

/// Renders `views` on the GPU into their sums (as `View::pass` would),
/// sharing `time_limit` from `start` like the CPU loop. Ok: the GPU's
/// name; Err (nothing changed in the views): why the CPU traces instead.
pub(super) fn render(views: &mut [View], meshes: &Meshes, s: super::Settings, start: Instant, progress: &crate::progress::Progress) -> Result<String, Refusal> {
    let device = views.first().map_or(Device::Auto, |v| v.scene.look.device);
    if device == Device::Cpu {
        return Err(Refusal::CpuAsked);
    }
    let gpu = shared().as_ref().map_err(|e| format!("no usable GPU ({e})"))?;
    if let Some(why) = lost() {
        return Err(format!("the GPU device was lost earlier in this session ({why}); restart to use it again").into());
    }
    if BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
        // Asking for the GPU by name tries it again (it is marked broken
        // again if it gets stuck again).
        if device != Device::Gpu {
            return Err("the GPU stopped responding earlier in this session (watchdog); use device: gpu to try it again".into());
        }
        BROKEN.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    // Hair strands trace slower here than on the CPU (the GPU has no
    // ray-tracing hardware access through wgpu yet, and the strands' tree
    // is millions of thin ribbons), so auto keeps them on the CPU.
    let strands = views.iter().any(|v| !v.scene.hair.is_empty());
    if strands && device == Device::Auto {
        return Err("hair strands trace faster on the CPU".into());
    }
    // The big buffers from counts alone, before packing anything.
    BufferPlan::of(meshes, &views.iter().map(|v| (v.w, v.h)).collect::<Vec<_>>()).check(&gpu.limits)?;
    take_fault();
    let scene = upload_meshes(gpu, meshes)?;
    let mut gviews = views.iter().map(|v| upload_view(gpu, &scene, v, s.bounces)).collect::<Result<Vec<_>, _>>()?;
    if let Some(e) = take_fault() {
        return Err(format!("GPU error uploading the scene: {e}").into());
    }
    progress.stage(&format!("tracing light on {}", gpu.name), 0.02);
    let total = s.samples as usize * views.len();
    // Samples per dispatch: grown or shrunk to about 0.15 s of GPU work,
    // so a time limit or cancel stops promptly and the OS never sees a
    // hung GPU.
    let mut batch = vec![1u32; views.len()];
    let mut done = vec![0u32; views.len()];
    let mut seconds = vec![0f32; views.len()];
    // The first pass is checked: on any GPU error the CPU renders instead.
    let mut checked = false;
    loop {
        if !checked {
            gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        }
        let mut any = false;
        for (k, gv) in gviews.iter_mut().enumerate() {
            if done[k] >= s.samples {
                continue;
            }
            any = true;
            let n = batch[k].min(s.samples - done[k]);
            let t = Instant::now();
            // Every (sample, tile) is one short dispatch, each with its
            // settings at its own offset of one buffer; one wait per pass.
            let (w, h) = (gv.params.size[0], gv.params.size[1]);
            let stride = stride(gpu) as usize;
            let side = tile_side(&views[k]);
            let tiles: Vec<(u32, u32)> = (0..h).step_by(side as usize).flat_map(|ty| (0..w).step_by(side as usize).map(move |tx| (tx, ty))).collect();
            let jobs: Vec<(u32, (u32, u32))> = (0..n).flat_map(|s| tiles.iter().map(move |&tile| (s, tile))).collect();
            let mut bytes = vec![0u8; stride * jobs.len()];
            for (i, &(sample, (tx, ty))) in jobs.iter().enumerate() {
                let mut p = gv.params;
                p.size[2] = done[k] + sample;
                p.size[3] = 1;
                p.counts[2] = tx;
                p.counts[3] = ty;
                bytes[i * stride..i * stride + std::mem::size_of::<Params>()].copy_from_slice(bytemuck::bytes_of(&p));
            }
            gpu.queue.write_buffer(&gv.uniform, 0, &bytes);
            for chunk in (0..jobs.len()).collect::<Vec<_>>().chunks(DISPATCHES_PER_SUBMIT) {
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&gpu.pipeline);
                    for &i in chunk {
                        let (tx, ty) = jobs[i].1;
                        pass.set_bind_group(0, &gv.group, &[(i * stride) as u32]);
                        pass.dispatch_workgroups(side.min(w - tx).div_ceil(8), side.min(h - ty).div_ceil(8), 1);
                    }
                }
                gpu.queue.submit([encoder.finish()]);
            }
            match gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(STUCK) }) {
                Ok(wgpu::PollStatus::WaitSucceeded | wgpu::PollStatus::QueueEmpty) => {}
                other => {
                    BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Err(format!("the GPU stopped responding: one pass took over {} s ({other:?}); auto renders use the CPU from now on", STUCK.as_secs()).into());
                }
            }
            let took = t.elapsed().as_secs_f32();
            seconds[k] += took;
            done[k] += n;
            if std::env::var("FILM_GPU_DEBUG").is_ok() { eprintln!("view {k}: {n} samples in {took:.3}s"); }
            // Samples per pass: a pass of about 0.3 s keeps time limits and
            // cancel prompt.
            let per = took / n as f32;
            batch[k] = ((0.3 / per.max(1e-4)) as u32).clamp(1, MAX_BATCH);
        }
        if !checked {
            checked = true;
            if let Some(e) = block_on(gpu.device.pop_error_scope()) {
                return Err(format!("GPU error in the first pass: {e}").into());
            }
        }
        if let Some(why) = lost() {
            return Err(format!("the GPU device was lost mid-render ({why})").into());
        }
        if let Some(e) = take_fault() {
            return Err(format!("GPU error mid-render: {e}").into());
        }
        let sum: usize = done.iter().map(|&d| d as usize).sum();
        let by_time = start.elapsed().as_secs_f32() / s.time_limit.as_secs_f32().max(1e-3);
        progress.set(0.02 + 0.93 * (sum as f32 / total.max(1) as f32).max(by_time).min(1.0));
        if !any || start.elapsed() > s.time_limit || progress.cancelled() {
            break;
        }
    }
    // Read the sums back into the views.
    for (k, (view, gv)) in views.iter_mut().zip(&gviews).enumerate() {
        let size = gv.accum.size();
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("read"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&gv.accum, 0, &staging, 0, size);
        gpu.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("GPU error reading the picture back: {e}"))?;
        {
            let data = slice.get_mapped_range();
            let sums: &[[f32; 4]] = bytemuck::cast_slice(&data);
            for i in 0..view.w * view.h {
                let [c, a, n] = [sums[i * 3], sums[i * 3 + 1], sums[i * 3 + 2]];
                view.sum[i] = Vec3::new(c[0], c[1], c[2]);
                view.guides[i].albedo = Vec3::new(a[0], a[1], a[2]);
                view.guides[i].normal = Vec3::new(n[0], n[1], n[2]);
                view.guides[i].depth = n[3];
                view.guides[i].light = a[3];
                view.guides[i].light2 = c[3];
            }
        }
        staging.unmap();
        view.done = done[k];
        view.seconds = seconds[k];
    }
    Ok(gpu.name.clone())
}

/// A note when a ray tree is deeper than the GPU's traversal stack: the
/// GPU may then miss boxes the CPU would visit.
pub(super) fn stack_warning(views: &[View], meshes: &Meshes) -> Option<String> {
    let depth = views.iter().map(|v| v.scene.hair_bvh.depth()).fold(meshes.bvh.depth(), usize::max);
    // A walk keeps at most one far box per inner node above its leaf.
    (depth.saturating_sub(1) > GPU_STACK).then(|| format!("the ray tree is {depth} levels deep but the GPU's traversal stack holds {GPU_STACK} boxes; the GPU may miss some surfaces"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use glam::{Mat4, Vec3};

    use super::super::{Camera, Fiber, Item, Look, Meshes, Settings, lighting::Environment, look::Device, render_views_with_look};
    use crate::material::Material;

    fn look(environment: Option<&str>, device: Device) -> Look {
        let mut look = Look::default();
        if let Some(name) = environment {
            look.environment = Arc::new(Environment::preset(name).unwrap());
            look.background = true;
        }
        look.device = device;
        look
    }

    /// A render's pixels and mean (0..255 per channel).
    fn render(items: Vec<Item>, look: Look) -> (Vec<[u8; 3]>, Vec3) {
        render_with(items, &[], 4.0, 256, look)
    }

    /// From `distance` away (close enough to see detail maps), with strands.
    fn render_with(items: Vec<Item>, fibers: &[Fiber], distance: f32, samples: u32, look: Look) -> (Vec<[u8; 3]>, Vec3) {
        let cam = Camera { eye: Vec3::new(0.0, 0.5 + 0.7 * distance / 4.0, distance), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 30.0, ortho: None };
        let s = Settings { width: 64, height: 48, samples, bounces: 4, time_limit: Duration::from_secs(300), floor: true, exposure: 1.0, denoise: false };
        let meshes = Meshes::new(items);
        let asked = look.device;
        let img = render_views_with_look(&meshes, fibers, &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
        img.traced.assert_honours(asked);
        assert_eq!(img.samples, samples);
        let mean = img.pixels.iter().map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)).sum::<Vec3>() / img.pixels.len() as f32;
        (img.pixels, mean)
    }

    fn ball(color: [f32; 3], m: &Material) -> Item {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": color, "blend": 0.0 }], "resolution": 32
        })).unwrap();
        let mut mesh = clay.build_mesh();
        mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
        Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m.clone() }
    }

    /// The denoiser's noise measure (each pixel's sum and square sum of
    /// demodulated lighting) comes out the same from both tracers: the
    /// mean lighting and the per-sample spread agree over the picture.
    #[test]
    fn gpu_matches_cpu_lighting_moments() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let items = || vec![ball([0.8, 0.4, 0.3], &Material::preset("skin").unwrap()), ball([0.3, 0.5, 0.8], &Material { roughness: 0.3, ..Material::default() })];
        let cam = Camera { eye: Vec3::new(1.5, 1.6, 2.6), target: Vec3::new(0.0, 0.4, 0.0), up: Vec3::Y, fov_deg: 40.0, ortho: None };
        let s = Settings { width: 64, height: 48, samples: 256, bounces: 4, time_limit: Duration::from_secs(300), floor: true, exposure: 1.0, denoise: true };
        let stats = |device: Device| {
            let meshes = Meshes::new({ let mut v = items(); v[1].world = Mat4::from_translation(Vec3::new(-1.1, 0.0, -0.4)); v });
            let (views, traced) = super::super::trace_views(&meshes, &[], std::slice::from_ref(&cam), s, Arc::new(look(Some("golden_hour"), device)), &Default::default());
            traced.assert_honours(device);
            let v = &views[0];
            assert_eq!(v.done, s.samples);
            let n = v.done as f32;
            let hit: Vec<_> = v.guides.iter().filter(|g| g.normal != Vec3::ZERO).collect();
            let count = hit.len() as f32;
            let mean = hit.iter().map(|g| g.light / n).sum::<f32>() / count;
            let spread = hit.iter().map(|g| (g.light2 / n - (g.light / n).powi(2)).max(0.0).sqrt()).sum::<f32>() / count;
            (mean, spread, hit.len())
        };
        let (cpu, gpu) = (stats(Device::Cpu), stats(Device::Gpu));
        eprintln!("lighting moments: cpu mean {:.4} spread {:.4} ({} px), gpu mean {:.4} spread {:.4} ({} px)", cpu.0, cpu.1, cpu.2, gpu.0, gpu.1, gpu.2);
        assert!(cpu.1 > 0.01 && gpu.1 > 0.01, "no noise measured: cpu {cpu:?} gpu {gpu:?}");
        assert!((cpu.0 - gpu.0).abs() < 0.02 * cpu.0, "mean lighting: cpu {} gpu {}", cpu.0, gpu.0);
        assert!((cpu.1 - gpu.1).abs() < 0.03 * cpu.1, "per-sample spread: cpu {} gpu {}", cpu.1, gpu.1);
        assert!(cpu.2.abs_diff(gpu.2) < cpu.2 / 50, "hit pixels: cpu {} gpu {}", cpu.2, gpu.2);
    }

    /// The GPU traces the same light as the CPU: plain, glossy, skin,
    /// glass and glowing balls under studio and sun light look alike.
    #[test]
    fn gpu_matches_cpu() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let cases = [
            ("clay", Material::default(), None),
            ("storm", Material::default(), Some("storm")),
            ("metal", Material { roughness: 0.3, metallic: 1.0, ..Material::default() }, None),
            ("skin", Material::preset("skin").unwrap(), Some("golden_hour")),
            ("glass", Material::preset("glass").unwrap(), None),
            ("glow", Material::preset("glow").unwrap(), Some("golden_hour")),
        ];
        for (name, m, env) in cases {
            let (cpu_px, cpu) = render(vec![ball([0.8, 0.4, 0.3], &m)], look(env, Device::Cpu));
            let (gpu_px, gpu) = render(vec![ball([0.8, 0.4, 0.3], &m)], look(env, Device::Gpu));
            // Per pixel: most pixels within noise of each other.
            let close = cpu_px.iter().zip(&gpu_px).filter(|(a, b)| a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 24)).count();
            eprintln!("{name}: cpu {cpu} gpu {gpu}, {close} of {} pixels close", cpu_px.len());
            assert!((cpu - gpu).abs().max_element() < 4.0, "{name}: cpu {cpu} gpu {gpu}");
            assert!(close as f32 > cpu_px.len() as f32 * 0.9, "{name}: only {close} pixels close");
        }
    }

    /// Object lights (more than four, so both pick by importance), a
    /// flame that only the camera sees, and height fog with light shafts
    /// trace the same on both.
    #[test]
    fn gpu_matches_cpu_with_point_lights_flames_and_fog() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let flame_mat = crate::envkit::kit::builtin("flame");
        let mut flame = ball([1.0, 0.6, 0.2], &flame_mat);
        flame.world = Mat4::from_scale_rotation_translation(Vec3::splat(0.15), glam::Quat::IDENTITY, Vec3::new(0.6, 0.9, 0.3));
        let mut lights: Vec<super::super::lighting::Light> = (0..7).map(|i| {
            let a = i as f32 * 0.9;
            super::super::lighting::Light::Sphere { position: Vec3::new(a.cos() * 1.1, 0.4 + 0.1 * i as f32, a.sin() * 1.1), radius: 0.04, power: Vec3::new(1.0, 0.6, 0.3) * (0.3 + 0.1 * i as f32) }
        }).collect();
        // The flame's own light sits inside it: shadow rays must pass the flame.
        lights.push(super::super::lighting::Light::Sphere { position: Vec3::new(0.6, 0.9, 0.3), radius: 0.03, power: Vec3::new(2.0, 1.2, 0.5) });
        for (name, fog) in [("lights", None), ("fog", Some(super::super::lighting::Fog { density: 0.35, albedo: Vec3::splat(0.85), height: 0.3, falloff: 0.8, anisotropy: 0.4, distance: 30.0, ..Default::default() }))] {
            let make = |device| {
                let mut l = look(Some("night"), device);
                l.lights = lights.iter().map(|x| match *x { super::super::lighting::Light::Sphere { position, radius, power } => super::super::lighting::Light::Sphere { position, radius, power }, _ => unreachable!() }).collect();
                l.fog = fog;
                l
            };
            let items = || vec![ball([0.8, 0.8, 0.8], &Material::default()), { let mut f = ball([1.0, 0.6, 0.2], &flame_mat); f.world = flame.world; f }];
            let (cpu_px, cpu) = render_with(items(), &[], 4.0, 1024, make(Device::Cpu));
            let (gpu_px, gpu) = render_with(items(), &[], 4.0, 1024, make(Device::Gpu));
            let mad = cpu_px.iter().zip(&gpu_px).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu_px.len() * 3) as f32;
            eprintln!("{name}: cpu {cpu} gpu {gpu}, mean pixel difference {mad:.2}");
            assert!(cpu.max_element() > 20.0, "{name}: the lights light the scene ({cpu})");
            assert!((cpu - gpu).abs().max_element() < 4.0, "{name}: cpu {cpu} gpu {gpu}");
            assert!(mad < MATCH * 1.5, "{name}: pixels differ by {mad:.2} on average");
        }
    }

    /// A torch and scattered candles in a scene reach both tracers as
    /// lights (the path render_film takes), and light the floor alike.
    #[test]
    fn object_lights_reach_both_tracers() {
        use crate::envkit::kit::{Kit, Piece};
        use crate::envkit::scatter::{Instance, Scatter};
        use crate::scene::Shape;
        let mut scene = crate::scene::Scene::default();
        let floor = scene.add(Shape::Kit);
        scene.get_mut(floor).unwrap().kit = Some(Kit { length: Some(4.0), depth: Some(4.0), ..Kit::new(Piece::Floor) });
        let torch = scene.add(Shape::Kit);
        let o = scene.get_mut(torch).unwrap();
        o.kit = Some(Kit { kind: Some("stand".into()), ..Kit::new(Piece::Torch) });
        o.light = Piece::Torch.light().map(|l| crate::envkit::light::LightSource { offset: glam::Vec3::new(0.0, 1.6, 0.0), ..l });
        o.translation = glam::Vec3::new(-1.0, 0.0, 0.0);
        let candle = scene.add(Shape::Kit);
        let o = scene.get_mut(candle).unwrap();
        o.kit = Some(Kit::new(Piece::Candle));
        o.light = Piece::Candle.light();
        o.visible = false;
        let sc = scene.add(Shape::Scatter);
        let instances = (0..6).map(|i| Instance { source: 0, position: Vec3::new(i as f32 * 0.5 - 1.0, 0.0, 1.0), rotation: glam::Quat::IDENTITY, scale: 1.0 }).collect();
        scene.get_mut(sc).unwrap().scatter = Some(Scatter { sources: vec![candle], instances, settings: Default::default(), placed_by: None });
        let mut lib = crate::mesh::MeshLib::new();
        lib.sync(&scene);
        let eval = scene.evaluate();
        let lights = crate::envkit::light::film_lights(&scene, &eval, 0.0);
        assert_eq!(lights.len(), 7, "the torch and six candle copies");
        let items = || -> Vec<Item> {
            crate::envkit::scatter::draws(&scene, &eval).iter().filter(|d| d.object.shape.is_mesh()).map(|d| {
                Item { mesh: lib.get(d.object).unwrap().clone(), world: d.world, tint: d.object.color, surface: scene.surface_of(d.object), material: scene.material_of(d.object) }
            }).collect()
        };
        if super::get().is_none() {
            eprintln!("no GPU here; skipped the GPU half");
            return;
        }
        let make = |device| {
            let mut l = look(Some("night"), device);
            l.lights = crate::envkit::light::film_lights(&scene, &eval, 0.0);
            l
        };
        let (cpu_px, cpu) = render_with(items(), &[], 4.0, 512, make(Device::Cpu));
        let (gpu_px, gpu) = render_with(items(), &[], 4.0, 512, make(Device::Gpu));
        let mad = cpu_px.iter().zip(&gpu_px).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu_px.len() * 3) as f32;
        eprintln!("object lights: cpu {cpu} gpu {gpu}, mean pixel difference {mad:.2}");
        assert!(cpu.x > 15.0 && cpu.x > cpu.z * 1.5, "warm light reaches the floor: {cpu}");
        assert!((cpu - gpu).abs().max_element() < 4.0, "cpu {cpu} gpu {gpu}");
        assert!(mad < MATCH * 1.5, "pixels differ by {mad:.2} on average");
    }

    /// Every image says which tracer made it: auto with hair strands
    /// falls back to the CPU and says why; asking for the CPU gives no
    /// reason.
    #[test]
    fn auto_fallback_reports_reason() {
        let fibers = vec![Fiber { points: vec![Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 1.2, 0.1)], root_width: 0.004, tip_width: 0.001, root_color: Vec3::splat(0.4), tip_color: Vec3::splat(0.6) }];
        let cam = Camera { eye: Vec3::new(0.0, 0.8, 3.0), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 30.0, ortho: None };
        let s = Settings { width: 16, height: 12, samples: 2, bounces: 1, time_limit: Duration::from_secs(60), floor: false, exposure: 1.0, denoise: false };
        let meshes = Meshes::new(vec![ball([0.8, 0.4, 0.3], &Material::default())]);
        let traced = |device| render_views_with_look(&meshes, &fibers, &[cam], s, Arc::new(look(None, device)), &Default::default()).pop().unwrap().traced;
        let auto = traced(Device::Auto);
        let super::super::Traced::Cpu { reason: Some(reason) } = &auto else { panic!("auto with strands: {auto:?}") };
        let expected = if super::get().is_some() { "hair strands" } else { "no usable GPU" };
        assert!(reason.contains(expected), "{reason}");
        assert_eq!(auto.json()["used"], "cpu");
        assert_eq!(auto.json()["fallback_reason"], reason.as_str());
        assert_eq!(traced(Device::Cpu), super::super::Traced::Cpu { reason: None });
    }

    /// The pre-flight names the buffer that does not fit, with its size
    /// and the limit, from counts alone (no GPU needed).
    #[test]
    fn preflight_names_the_buffer() {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "blend": 0.0 }], "resolution": 64
        })).unwrap();
        let m = Material::default();
        let meshes = Meshes::new(vec![Item { mesh: Arc::new(clay.build_mesh()), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }]);
        let tris = meshes.tris.len() as u64;
        assert!(tris * 48 > 1 << 20 && meshes.bvh.node_count() as u64 * 32 < 1 << 20, "{tris} triangles");
        let plan = super::BufferPlan::of(&meshes, &[(64, 48)]);
        let small = egui_wgpu::wgpu::Limits { max_storage_buffer_binding_size: 1 << 20, max_buffer_size: 1 << 30, ..Default::default() };
        let err = plan.check(&small).unwrap_err();
        assert_eq!(err, format!("triangles: {} is more than one GPU buffer holds (1.0 MB)", super::show_bytes(tris * 48)));
        assert!(plan.check(&egui_wgpu::wgpu::Limits::default()).is_ok());
        // A picture over the limit is named too.
        let err = super::BufferPlan::from_counts(1, 1, 3, &[(4096, 4096)]).check(&small).unwrap_err();
        assert!(err.starts_with("picture sums: 768.0 MB"), "{err}");
        assert!(plan.entries.iter().map(|e| e.1).sum::<u64>() > tris * 64);
    }

    /// Mean per-pixel difference (0..255) allowed between the two tracers
    /// (their random numbers differ, so some noise remains).
    const MATCH: f32 = 2.0;

    /// Library detail maps (weave, leather, scales) and hair strands
    /// trace the same on both, close up.
    #[test]
    fn gpu_matches_cpu_on_detail_and_hair() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let mut cases: Vec<(&str, Material, Vec<Fiber>)> = ["cloth", "leather", "scales"].iter().map(|name| {
            let mut m = Material::preset(name).unwrap();
            if m.detail_name().is_none() {
                m.detail = Some("scales".into());
            }
            m.detail_scale = 0.15;
            m.detail_strength = 2.0;
            m.detail_color = 1.0;
            (*name, m, Vec::new())
        }).collect();
        // A cap of strands over the ball's top.
        let fibers: Vec<Fiber> = (0..2000).map(|i| {
            let a = i as f32 * 2.399_963;
            let r = 0.45 * ((i as f32 + 0.5) / 2000.0).sqrt();
            let root = Vec3::new(r * a.cos(), 0.5 + (0.25 - r * r).max(0.0).sqrt(), r * a.sin());
            let out = (root - Vec3::new(0.0, 0.5, 0.0)).normalize();
            let points = (0..8).map(|k| root + out * 0.01 + (out * 0.3 + Vec3::NEG_Y * 0.4) * (k as f32 / 7.0) * 0.5).collect();
            Fiber { points, root_width: 0.004, tip_width: 0.001, root_color: Vec3::new(0.5, 0.3, 0.1), tip_color: Vec3::new(0.7, 0.5, 0.25) }
        }).collect();
        cases.push(("strands", Material::default(), fibers));
        for (name, m, fibers) in cases {
            // Close: the ball fills the picture. Two CPU renders with other
            // random numbers give the noise floor the GPU must stay near.
            let (cpu_px, cpu) = render_with(vec![ball([0.6, 0.45, 0.35], &m)], &fibers, 1.4, 1024, look(Some("golden_hour"), Device::Cpu));
            let (gpu_px, gpu) = render_with(vec![ball([0.6, 0.45, 0.35], &m)], &fibers, 1.4, 1024, look(Some("golden_hour"), Device::Gpu));
            let mad = cpu_px.iter().zip(&gpu_px).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu_px.len() * 3) as f32;
            eprintln!("{name}: cpu {cpu} gpu {gpu}, mean pixel difference {mad:.2}");
            assert!((cpu - gpu).abs().max_element() < 4.0, "{name}: cpu {cpu} gpu {gpu}");
            assert!(mad < MATCH, "{name}: pixels differ by {mad:.2} on average");
        }
    }

    /// A physical sky traces the same on both: one CPU bake reaches both
    /// tracers as a map and a sun disc, so only noise may differ.
    #[test]
    fn gpu_matches_cpu_under_a_physical_sky() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let paint = Material { roughness: 0.4, ..Material::default() };
        for env in [serde_json::json!({ "preset": "physical", "sun_elevation": 60 }), serde_json::json!({ "preset": "physical", "sun_elevation": 20, "sun_azimuth": 120, "turbidity": 4 })] {
            let lit = |device: Device| {
                let mut l = look(None, device);
                l.environment = Arc::new(Environment::parse(&env).unwrap());
                l.background = true;
                l
            };
            let (cpu_px, cpu) = render_with(vec![ball([0.8, 0.4, 0.3], &paint)], &[], 4.0, 1024, lit(Device::Cpu));
            let (gpu_px, gpu) = render_with(vec![ball([0.8, 0.4, 0.3], &paint)], &[], 4.0, 1024, lit(Device::Gpu));
            let mad = cpu_px.iter().zip(&gpu_px).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu_px.len() * 3) as f32;
            eprintln!("{env}: cpu {cpu} gpu {gpu}, mean pixel difference {mad:.2}");
            assert!((cpu - gpu).abs().max_element() < 4.0, "{env}: cpu {cpu} gpu {gpu}");
            assert!(mad < MATCH, "{env}: pixels differ by {mad:.2} on average");
        }
    }
}

#[cfg(test)]
mod detail_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use glam::{Mat4, Vec3};

    use super::super::{Camera, Item, Look, Meshes, Settings, lighting::Environment, look::Device, render_views_with_look};
    use crate::material::Material;

    fn camera() -> Camera {
        Camera { eye: Vec3::new(0.0, 0.85, 1.3), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 30.0, ortho: None }
    }

    fn render(items: Vec<Item>, device: Device) -> Vec<[u8; 3]> {
        let mut look = Look::default();
        look.environment = Arc::new(Environment::preset("golden_hour").unwrap());
        look.background = true;
        look.device = device;
        let s = Settings { width: 64, height: 48, samples: 1024, bounces: 4, time_limit: Duration::from_secs(300), floor: true, exposure: 1.0, denoise: false };
        let img = render_views_with_look(&Meshes::new(items), &[], &[camera()], s, Arc::new(look), &Default::default()).pop().unwrap();
        img.traced.assert_honours(device);
        img.pixels
    }

    fn difference(a: &[[u8; 3]], b: &[[u8; 3]]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| (*p as f32 - *q as f32).abs()).sum::<f32>()).sum::<f32>() / (a.len() * 3) as f32
    }

    /// Render-time displacement (diced geometry) and an adaptive fine mesh
    /// with fine sculpted pores trace the same on both tracers; the
    /// displacement visibly changes the picture.
    #[test]
    fn gpu_matches_cpu_on_displacement_and_adaptive_detail() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": [0.6, 0.5, 0.4], "blend": 0.0 }], "resolution": 48
        })).unwrap();
        let plain = Material { detail: Some("stone".into()), detail_scale: 0.3, ..Material::default() };
        let displaced = Material { detail_displace: 0.05, ..plain.clone() };
        let item = |m: &Material, mesh: &crate::mesh::Mesh| {
            let mut mesh = mesh.clone();
            mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
            Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m.clone() }
        };
        let base = clay.build_mesh();
        let mut items = vec![item(&displaced, &base)];
        let diced = super::super::dice::displace(&mut items, &[camera()], 48, 48);
        assert!(diced > 1000, "{diced}");
        let flat = render(vec![item(&plain, &base)], Device::Cpu);
        let mesh = items[0].mesh.clone();
        let again = || vec![Item { mesh: mesh.clone(), world: Mat4::IDENTITY, tint: [1.0; 3], surface: displaced.surface(), material: displaced.clone() }];
        let (cpu, gpu) = (render(again(), Device::Cpu), render(again(), Device::Gpu));
        let (mad, changed) = (difference(&cpu, &gpu), difference(&cpu, &flat));
        eprintln!("displaced: cpu vs gpu {mad:.2}, displaced vs flat {changed:.2}");
        assert!(mad < super::tests_match(), "displaced pixels differ by {mad:.2}");
        assert!(changed > mad * 2.0, "displacement did not show: {changed:.2}");
        // Adaptive detail: a pore region on the front.
        let mut fine = clay.clone();
        fine.set_detail_cell(0.004).unwrap();
        fine.set_detail_regions(&[crate::clay::Region { name: "front".into(), center: Vec3::new(0.0, 0.6, 0.45), radii: Vec3::splat(0.25), cell: 0.001 }]).unwrap();
        let pores: crate::sculpt::Stroke = serde_json::from_value(serde_json::json!({ "brush": "noise", "points": [[0, 0.6, 0.5]], "radius": 0.25, "strength": 0.9, "scale": 0.006, "pattern": "pores" })).unwrap();
        crate::sculpt::apply(&mut fine, &pores).unwrap();
        let mesh = fine.build_fine(&[], "").unwrap();
        let skin = Material::preset("skin").unwrap();
        let (cpu, gpu) = (render(vec![item(&skin, &mesh)], Device::Cpu), render(vec![item(&skin, &mesh)], Device::Gpu));
        let mad = difference(&cpu, &gpu);
        eprintln!("adaptive pores: cpu vs gpu {mad:.2} ({} triangles)", mesh.indices.len() / 3);
        assert!(mad < super::tests_match(), "adaptive pores differ by {mad:.2}");
    }
}

/// See `tests::MATCH`.
#[cfg(test)]
fn tests_match() -> f32 {
    2.0
}

#[cfg(test)]
mod smart_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use glam::{Mat4, Vec3};

    use super::super::{Camera, Item, Look, Meshes, Settings, look::Device, render_views_with_look};
    use crate::material::Material;

    fn render(meshes: &Meshes, device: Device) -> Vec<[u8; 3]> {
        let mut look = Look::default();
        look.environment = Arc::new(super::super::lighting::Environment::preset("golden_hour").unwrap());
        look.background = true;
        look.device = device;
        let cam = Camera { eye: Vec3::new(0.2, 0.45, 0.75), target: Vec3::new(-0.02, 0.19, 0.0), up: Vec3::Y, fov_deg: 35.0, ortho: None };
        let s = Settings { width: 72, height: 54, samples: 1024, bounces: 4, time_limit: Duration::from_secs(300), floor: true, exposure: 1.0, denoise: false };
        let img = render_views_with_look(meshes, &[], &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
        img.traced.assert_honours(device);
        img.pixels
    }

    /// A wide continuous leak must match, rather than disappearing in a
    /// small region that whole-image averages could overlook.
    #[test]
    fn gpu_matches_cpu_on_continuous_damage_leaks() {
        if super::get().is_none() {eprintln!("no GPU here; skipped");return;}
        let mut clay:crate::clay::Clay=serde_json::from_value(serde_json::json!({"resolution":96,"blobs":[{"type":"box","center":[0,0.2,0],"size":[0.32,0.4,0.06],"round":0.004,"blend":0,"color":[0.65,0.62,0.55]}]})).unwrap();
        let base=clay.build_mesh();
        for (i,kind) in ["rust_streak","oil_leak","water_stain"].iter().enumerate() {
            let request=serde_json::from_value(serde_json::json!({"kind":kind,"point":[-0.10+0.10*i as f32,0.36,0.035],"normal":[0,0,1],"size":0.11,"length":0.30,"seed":17})).unwrap();
            clay.decals.extend(crate::damage::stamps(&base,Mat4::IDENTITY,true,&request).unwrap());
        }
        let mesh=Arc::new(clay.build_mesh());let m=Material::default();
        let meshes=Meshes::new(vec![Item{mesh,world:Mat4::IDENTITY,tint:[1.0;3],surface:m.surface(),material:m}]);
        let (cpu,gpu)=(render(&meshes,Device::Cpu),render(&meshes,Device::Gpu));
        let mad=cpu.iter().zip(&gpu).flat_map(|(a,b)|a.iter().zip(b).map(|(x,y)|(*x as f32-*y as f32).abs())).sum::<f32>()/(cpu.len()*3) as f32;
        let mean=|px:&[[u8;3]]|px.iter().map(|c|Vec3::new(c[0] as f32,c[1] as f32,c[2] as f32)).sum::<Vec3>()/px.len() as f32;
        eprintln!("continuous leaks: CPU {} GPU {}, mean pixel difference {mad:.2}",mean(&cpu),mean(&gpu));
        assert!((mean(&cpu)-mean(&gpu)).abs().max_element()<0.8);assert!(mad<2.0,"leaks differ by {mad:.2}");
    }

    /// Smart layers (every mask source, detail maps, glow), painted mask
    /// channels and decals trace the same on both tracers.
    #[test]
    fn gpu_matches_cpu_on_smart_materials() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let custom: Material = serde_json::from_value(serde_json::json!({ "name": "custom", "layers": [
            { "smart": "grimy_cloth" },
            { "color": [0.1, 0.3, 0.6], "metallic": 1.0, "roughness": 0.2, "mask": [{ "source": "painted", "channel": "blue" }] },
            { "color": [0.9, 0.9, 0.2], "mask": [{ "source": "direction", "direction": [1, 0, 0] }, { "source": "height", "range": [0.1, 0.3], "op": "min" }], "threshold": 0.4, "contrast": 3 },
            { "tint": [0.5, 0.5, 0.5], "mask": [{ "source": "constant", "value": 0.4 }, { "source": "edges", "op": "screen" }, { "source": "ao", "op": "subtract", "invert": true }] },
            { "smart": "blood_splatter", "opacity": 0.8 }
        ] })).unwrap();
        for name in ["worn_iron", "demon_skin", "mossy_stone", "custom", "battle_worn", "desert_dusty", "battle_desert", "muddy", "burnt_wreck", "factory_new"] {
            let mut clay = crate::smart_check::test_piece(name);
            if name == "custom" {
                // A painted channel and a decal of every kind.
                let mut blue = None;
                std::mem::swap(&mut clay.paint, &mut blue);
                let stroke = crate::paint::PaintStroke { points: vec![Vec3::new(0.17, 0.25, 0.12)], radius: 0.08, color: Some([1.0; 3]), strength: 1.0, hardness: 0.5, erase: false, mirror: false, mask: None };
                crate::paint::stroke(&mut clay, &stroke).unwrap();
                clay.masks.insert("blue".into(), clay.paint.take().unwrap());
                for (i, shape) in ["rune", "scratch", "splat", "emblem", "bullet_hit", "dent", "scorch", "blast", "gouge", "scrape", "rust_streak", "oil_leak", "water_stain", "bend", "tear"].iter().enumerate() {
                    let mut d: crate::smart::Decal = serde_json::from_value(serde_json::json!({ "shape": shape, "center": [-0.30 + 0.067 * (i % 4) as f32, 0.09 + 0.067 * (i / 4) as f32, 0.07], "normal": [0, 0, 1], "size": 0.06, "aspect": 1.2, "flow": [0.2,0.7], "melted_lip": *shape=="scorch", "rotate": 20 * i, "seed": i, "emission": if i == 0 { 2.0 } else { 0.0 } })).unwrap();
                    d.sanitize();
                    clay.decals.push(d);
                }
            }
            let mut lib = vec![custom.clone()];
            let world=Mat4::from_rotation_z(0.7);
            let mut m = Material::resolve(&lib, name);
            if name == "battle_desert" {
                m.layers = crate::damage::wear_layers("battle_worn", 1.0).unwrap();
                m.layers.extend(crate::damage::wear_layers("desert_dusty", 1.0).unwrap());
                clay.decals.push(crate::smart::Decal {
                    shape: crate::smart::Shape::WaterStain,
                    wear_preset: "battle_worn".into(),
                    center: [0.0, 0.2, 0.07], normal: [0.0, 0.0, 1.0],
                    size: 0.08, aspect: 2.5, opacity: 0.3, flow: Some([0.0, 1.0]),
                    ..Default::default()
                });
            }
            let m=crate::damage::world_material(m,world);
            lib.retain(|m|m.name!=name);lib.push(Material{name:name.into(),..m});
            let mesh = Arc::new(clay.build_mesh_with(&lib, name));
            let m = Material::resolve(&lib, name);
            let meshes = Meshes::new(vec![Item { mesh, world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m }]);
            let (cpu, gpu) = (render(&meshes, Device::Cpu), render(&meshes, Device::Gpu));
            let mean = |px: &[[u8; 3]]| px.iter().map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)).sum::<Vec3>() / px.len() as f32;
            let mad = cpu.iter().zip(&gpu).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu.len() * 3) as f32;
            eprintln!("{name}: cpu {} gpu {}, mean pixel difference {mad:.2}", mean(&cpu), mean(&gpu));
            assert!((mean(&cpu) - mean(&gpu)).abs().max_element() < 4.0, "{name}");
            assert!(mad < 2.5, "{name}: pixels differ by {mad:.2} on average");
        }
    }
}

#[cfg(test)]
mod face_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use glam::{Mat4, Vec3};

    use super::super::{Camera, Item, Look, Meshes, Settings, look::Device, render_views_with_look};
    use crate::material::Material;

    /// A face close-up (regional pores, lines, lips, stubble, freckles,
    /// thin-part light through the nose) and a lidded eye (textured iris
    /// under a refracting cornea, tear line, caruncle) trace the same on
    /// both tracers.
    #[test]
    fn gpu_matches_cpu_on_a_face_close_up() {
        if super::get().is_none() {
            eprintln!("no GPU here; skipped");
            return;
        }
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "human", "part": "head", "blend": 0, "material": "skin",
            "human": { "sex": 1.0, "age": 60, "skin": { "stubble": 0.6, "freckles": 0.5, "moles": 0.5 } } }], "resolution": 160 })).unwrap();
        let skin = Material::resolve(&[], "skin");
        let head = Arc::new(clay.build_mesh_with(&[], "skin"));
        let face = head.face.clone().expect("a face");
        let ring = |from: f32, to: f32, t: f32| (0..12).map(|i| {
            let p = from + (to - from) * i as f32 / 11.0;
            (Vec3::new(t.sin() * p.cos(), t.sin() * p.sin(), t.cos()) * 0.5).to_array()
        }).collect::<Vec<_>>();
        let eye = crate::eyeball::Eye { lids: Some(crate::eyeball::Lids { upper: ring(std::f32::consts::PI, 0.0, 0.7), lower: ring(std::f32::consts::PI, std::f32::consts::TAU, 0.75) }), ..Default::default() };
        let eye_mesh = Arc::new(eye.build_mesh_in(Some(Mat4::IDENTITY)));
        let eye_at = Mat4::from_translation(face.origin + face.forward * 0.06 + face.right * 0.02) * Mat4::from_scale(Vec3::splat(0.02));
        let meshes = Meshes::new(vec![
            Item { mesh: head, world: Mat4::IDENTITY, tint: [1.0; 3], surface: skin.surface(), material: skin.clone() },
            Item { mesh: eye_mesh, world: eye_at, tint: [1.0; 3], surface: [-1.0, 0.0, 0.0, 0.0], material: Material::default() },
        ]);
        let render = |device: Device| {
            let mut look = Look::default();
            look.device = device;
            let target = face.origin - face.up * 0.045 + face.forward * 0.03;
            let cam = Camera { eye: target + face.forward * 0.16 + face.right * 0.05, target, up: Vec3::Y, fov_deg: 40.0, ortho: None };
            let s = Settings { width: 72, height: 54, samples: 1024, bounces: 4, time_limit: Duration::from_secs(300), floor: false, exposure: 1.0, denoise: false };
            let img = render_views_with_look(&meshes, &[], &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
            img.traced.assert_honours(device);
            img.pixels
        };
        let (cpu, gpu) = (render(Device::Cpu), render(Device::Gpu));
        let mean = |px: &[[u8; 3]]| px.iter().map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)).sum::<Vec3>() / px.len() as f32;
        let mad = cpu.iter().zip(&gpu).map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| (*x as f32 - *y as f32).abs()).sum::<f32>()).sum::<f32>() / (cpu.len() * 3) as f32;
        eprintln!("face: cpu {} gpu {}, mean pixel difference {mad:.2}", mean(&cpu), mean(&gpu));
        assert!(mean(&cpu).max_element() > 20.0, "the face is lit");
        assert!((mean(&cpu) - mean(&gpu)).abs().max_element() < 3.0);
        assert!(mad < 3.0, "pixels differ by {mad:.2} on average");
    }
}

#[cfg(test)]
mod timing {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use glam::Vec3;

    use super::super::{Camera, Fiber, Item, Look, Meshes, Settings, look::Device, render_views_with_look};

    /// CPU against GPU on a saved scene, face and full body, with hair:
    /// `SCENE=path OUT=dir cargo test --release gpu_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn gpu_timing() {
        let path = std::env::var("SCENE").expect("SCENE");
        let scene: crate::scene::Scene = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut lib = crate::mesh::MeshLib::new();
        lib.show_fine = std::env::var("FINE").is_ok();
        lib.sync(&scene);
        let eval = scene.evaluate();
        let items: Vec<Item> = scene.objects.iter().filter(|o| o.shape.is_mesh()).filter_map(|o| {
            let &(world, true) = eval.get(&o.id)? else { return None };
            Some(Item { mesh: lib.get(o)?.clone(), world, tint: o.color, surface: scene.surface_of(o), material: scene.material_of(o) })
        }).collect();
        let mut fibers = Vec::new();
        for o in scene.objects.iter().filter(|o| o.shape == crate::scene::Shape::Hair && std::env::var("NO_HAIR").is_err()) {
            let (Some(&(world, true)), Some(hair)) = (eval.get(&o.id), o.hair.as_ref()) else { continue };
            for s in hair.strands() {
                fibers.push(Fiber { points: s.points.iter().map(|p| world.transform_point3(*p)).collect(), root_width: s.root_width, tip_width: s.tip_width, root_color: s.root_color, tip_color: s.tip_color });
            }
        }
        let tris: usize = items.iter().map(|i| i.mesh.indices.len() / 3).sum();
        eprintln!("{} items, {tris} triangles, {} strands", items.len(), fibers.len());
        let meshes = Meshes::new(items);
        let (lo, hi) = meshes.items.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), it| {
            it.mesh.vertices.iter().fold((lo, hi), |(lo, hi), v| { let p = it.model * Vec3::from(v.pos); (lo.min(p), hi.max(p)) })
        });
        let face = Vec3::new((lo.x + hi.x) * 0.5, hi.y - 0.13, (lo.z + hi.z) * 0.5 + 0.05);
        let body = (lo + hi) * 0.5;
        let cams = [
            ("face", Camera { eye: face + Vec3::new(0.25, 0.05, 0.62), target: face, up: Vec3::Y, fov_deg: 30.0, ortho: None }),
            ("body", Camera { eye: body + Vec3::new(1.6, 0.4, 3.6), target: body, up: Vec3::Y, fov_deg: 35.0, ortho: None }),
        ];
        let samples = std::env::var("SAMPLES").ok().and_then(|s| s.parse().ok()).unwrap_or(64);
        let px = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(512);
        let s = Settings { width: px, height: px, samples, bounces: 4, time_limit: Duration::from_secs(1200), floor: true, exposure: 1.0, denoise: true };
        for (name, cam) in cams {
            for device in [Device::Gpu, Device::Cpu] {
                if device == Device::Cpu && std::env::var("GPU_ONLY").is_ok() { continue; }
                let mut look = Look::default();
                if let Ok(env) = std::env::var("ENV") {
                    look.environment = Arc::new(super::super::lighting::Environment::preset(&env).unwrap());
                    look.background = true;
                }
                look.device = device;
                let t = Instant::now();
                let img = render_views_with_look(&meshes, &fibers, &[cam], s, Arc::new(look), &Default::default()).pop().unwrap();
                eprintln!("{name} {device:?}: {} samples in {:.2}s", img.samples, t.elapsed().as_secs_f32());
                if let Ok(dir) = std::env::var("OUT") {
                    std::fs::create_dir_all(&dir).unwrap();
                    image::save_buffer(format!("{dir}/{name}_{device:?}.png"), &img.pixels.iter().flatten().copied().collect::<Vec<_>>(), px as u32, px as u32, image::ColorType::Rgb8).unwrap();
                }
            }
        }
    }
}

#[cfg(test)]
mod fire_grid_tests {
    use super::*;
    #[test]
    fn gpu_matches_cpu_volume_coefficients() {
        let Some(gpu) = get() else { return };
        let fire = crate::fire::simulate(
            crate::fire::Settings {
                resolution: 64,
                frames: 1,
                seed: u32::MAX,
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
        let world = glam::Mat4::from_scale_rotation_translation(
            Vec3::new(1.3, 0.9, 1.1),
            glam::Quat::from_rotation_y(0.4),
            Vec3::new(0.03, 0.0, 0.0),
        );
        let volume = fire.volume(world, None).unwrap();
        let packed = crate::fire::pack(std::slice::from_ref(&volume));
        let source = include_str!("trace.wgsl");
        let functions = source
            .split("struct FireCell")
            .nth(1)
            .unwrap()
            .split("fn fire_trans")
            .next()
            .unwrap();
        let shader = format!(
            "@group(0) @binding(0) var<storage,read> fire_data:array<vec4f>; @group(0) @binding(1) var<storage,read_write> output:array<vec4f>; struct FireCell{functions}\n@compute @workgroup_size(1) fn main() {{for(var i=0u;i<192u;i++){{let p=vec3f((f32(i%4u)-1.5)*0.019,f32((i/4u)%12u)*0.019,(f32(i/48u)-1.5)*0.017);let c=fire_at(0u,p);output[i*2u]=c.e;output[i*2u+1u]=vec4f(c.scatter,0.0);}}output[384]=vec4f(fire_interval(0u,vec3f(0.0,0.1,1.0),vec3f(0.0,0.0,-1.0),100.0),0.0,0.0);}}"
        );
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fire coefficients"),
                source: wgpu::ShaderSource::Wgsl(shader.into()),
            });
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
        let input = gpu
            .storage("fire coefficients input", bytemuck::cast_slice(&packed))
            .unwrap();
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 385 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 385 * 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = pipeline.get_bind_group_layout(0);
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, 385 * 16);
        gpu.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        {
            let data = slice.get_mapped_range();
            let values: &[[f32; 4]] = bytemuck::cast_slice(&data);
            let mut lit = 0;
            for i in 0..192 {
                let p = Vec3::new(
                    ((i % 4) as f32 - 1.5) * 0.019,
                    ((i / 4) % 12) as f32 * 0.019,
                    ((i / 48) as f32 - 1.5) * 0.017,
                );
                let b = volume.at(p);
                if b[0] > 0.001 {
                    lit += 1;
                }
                for k in 0..7 {
                    let a = values[i * 2 + k / 4][k % 4];
                    assert!(
                        (a - b[k]).abs() < 0.002 + b[k].abs() * 0.0005,
                        "point {i}, channel {k}: GPU {a} CPU {}",
                        b[k]
                    );
                }
            }
            assert!(lit > 5, "parity probes must hit emitting sheets");
            let cpu = volume
                .interval(Vec3::new(0.0, 0.1, 1.0), -Vec3::Z, 100.0)
                .unwrap();
            assert!((values[384][0] - cpu.0).abs() < 1e-5 && (values[384][1] - cpu.1).abs() < 1e-5);
        }
        staging.unmap();
    }
}

#[cfg(test)]
mod lens_tests {
    use super::*;
    use crate::film::{Camera, Item, Look, Meshes, Settings, render_views_with_look, look::Device};
    use crate::material::Material;
    use glam::{Mat4, Vec3};
    use serde_json::json;

    #[test]
    fn gpu_matches_cpu_polygonal_dof_and_heterogeneous_volume() {
        if get().is_none() { eprintln!("no GPU here; skipped"); return; }
        let items = || {
            [(-0.4,0.0), (0.4,-1.3)].into_iter().map(|(x,z)| {
                let mat = Material::default();
                Item { mesh: Arc::new(crate::mesh::Mesh::build(crate::scene::Shape::Sphere)), world: Mat4::from_scale_rotation_translation(Vec3::splat(0.5),glam::Quat::IDENTITY,Vec3::new(x,0.5,z)), tint:[0.7,0.5,0.3],surface:mat.surface(), material:mat }
            }).collect()
        };
        let cam = Camera { eye:Vec3::new(0.0,0.65,2.5),target:Vec3::new(0.0,0.5,0.0),up:Vec3::Y,fov_deg:35.0,ortho:None };
        let settings = Settings { width:48,height:32,samples:512,bounces:3,time_limit:std::time::Duration::from_secs(180),floor:true,exposure:1.0,denoise:false };
        for noise in [0.0,0.85] {
            let mut results = Vec::new();
            for device in [Device::Cpu,Device::Gpu] {
                let mut look = Look::parse(&json!({"camera":{"aperture":0.7,"sensor_height_mm":48,"dof":{"focus_distance":2.5,"blades":5,"blade_rotation":17}},"environment":{"preset":"golden_hour","intensity":0.3,"rotate":120},"lights":"none","background":true,"fog":{"density":0.08,"noise":noise,"noise_scale":0.75,"seed":991,"height":0.4,"falloff":1.5,"distance":8},"grade":{"look":"modern_warfare"},"bloom":{"strength":0.1,"streaks":0.3},"grain":0.01})).unwrap();
                look.device = device;
                let meshes = Meshes::new(items());
                let images = render_views_with_look(&meshes,&[],&[cam],settings,Arc::new(look),&Default::default());
                assert_eq!(images[0].samples,settings.samples);
                images[0].traced.assert_honours(device);
                results.push(images.into_iter().next().unwrap());
            }
            let a=&results[0]; let b=&results[1];
            let mad = a.pixels.iter().zip(&b.pixels).map(|(a,b)| a.iter().zip(b).map(|(x,y)| (*x as f32-*y as f32).abs()).sum::<f32>()).sum::<f32>()/(a.pixels.len()*3) as f32;
            eprintln!("polygon DOF, fog noise {noise}: mean pixel difference {mad:.3}");
            assert!(mad < 5.0);
            let energy = |im: &crate::film::Image| im.linear.iter().copied().sum::<Vec3>()/im.linear.len() as f32;
            assert!((energy(a)-energy(b)).abs().max_element() < 0.08);
        }
    }
}
