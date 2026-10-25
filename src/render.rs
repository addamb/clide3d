//! wgpu renderer for the 3D viewport.
//!
//! GPU objects live in egui-wgpu's `CallbackResources` (created once in
//! `init`). Every frame the app builds a `ViewportCallback` with the camera
//! and one instance per visible object; egui calls it inside its render pass.
//! `render_image` draws the same callback into an off-screen texture so
//! agents can get pictures from any camera without touching the window.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui_wgpu::wgpu;
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::mesh::{Mesh, MeshKey, SculptChunk, SculptVertex, Textures, Vertex};
use crate::view_light::{self, EnvProbe, ViewLighting};

/// Must match `NativeOptions::multisampling`.
pub const MSAA: u32 = 4;
/// Must match `NativeOptions::depth_buffer = 24`.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    /// Orients the XZ ground grid for this view (XY for front, YZ for side).
    grid_model: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    /// x = 1 when the target is not sRGB and the shader must gamma-encode.
    /// y = 1 for flat, unlit fills in the instance color (silhouette masks).
    /// z = how many scene lights, w = studio light strength.
    params: [f32; 4],
    /// Scene lights: (position, radius), (linear color x intensity, 0).
    lights: [[f32; 4]; MAX_VIEW_LIGHTS * 2],
    /// World to shadow map clip space (an orthographic view along the key).
    light_vp: [[f32; 4]; 4],
    /// xyz = toward the key light (the shader normalises it),
    /// w = shadow strength.
    key_dir: [f32; 4],
    /// x = 1 when the key casts shadows, y = one map texel in uv,
    /// z = filter radius in texels, w = 1 for soft (PCSS) shadows.
    shadow: [f32; 4],
    /// x = normal offset (world units), y = the map's depth range (world
    /// units), z = tan of the key's angular radius, w = one texel in world
    /// units.
    shadow2: [f32; 4],
    /// The shadow catcher on the grid plane y = 0 as (min x, min z,
    /// max x, max z); all zero when it is off.
    ground: [f32; 4],
    /// The inverse of `view_proj` (ambient occlusion rebuilds positions).
    inv_view_proj: [[f32; 4]; 4],
    /// The environment's irradiance as SH9 (`view_light::sh_basis`).
    sh: [[f32; 4]; 9],
    /// Directional lights: toward each, and its irradiance; the first is
    /// the shadowed key.
    sun_dir: [[f32; 4]; view_light::MAX_SUNS],
    sun_col: [[f32; 4]; view_light::MAX_SUNS],
    /// x = 1 for environment lighting (0 for the classic studio), y = how
    /// many suns, z = the reflection map's last mip, w = its rotation.
    env: [f32; 4],
    /// x = catchlight irradiance, y = the scale on the environment and
    /// suns (the studio dims when objects carry lights).
    env2: [f32; 4],
    /// x = 1 when ambient occlusion is on, y = its radius (world units),
    /// z = strength.
    ao: [f32; 4],
    /// The occlusion targets' size in texels (w, h, 1 / w, 1 / h).
    ao_size: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<Globals>() == 1200, "Globals must match the WGSL struct");

/// Real-time shadows from the studio key light (viewport and screenshots;
/// render_film traces its own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadows {
    pub enabled: bool,
    /// Contact-hardening (PCSS) penumbrae instead of a fixed soft edge.
    pub soft: bool,
    pub quality: ShadowQuality,
    /// 0..1: how much of the key light a shadow takes away.
    pub strength: f32,
    /// The key's angular radius in radians (soft shadows).
    pub softness: f32,
    /// Catch shadows on the grid plane where there is no floor.
    pub ground: bool,
    /// Toward the key light; also moves its highlight and reflection.
    pub key_dir: Vec3,
}

impl Default for Shadows {
    fn default() -> Self {
        Shadows {
            enabled: true,
            soft: false,
            quality: ShadowQuality::Normal,
            strength: 1.0,
            softness: crate::film::KEY_RADIUS,
            ground: true,
            key_dir: crate::film::KEY_DIR,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowQuality {
    Normal,
    High,
}

impl ShadowQuality {
    /// Shadow map side in texels.
    pub fn size(self) -> u32 {
        match self {
            ShadowQuality::Normal => 2048,
            ShadowQuality::High => 4096,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ShadowQuality::Normal => "normal",
            ShadowQuality::High => "high",
        }
    }
}

/// Above this many fill triangles a frame draws without its extra passes
/// (the shadow map and the ambient occlusion pre-pass): each would draw
/// the whole scene again. One count and one gate serve both.
pub const EXTRA_PASS_TRIANGLE_BUDGET: u64 = 8_000_000;
pub const SHADOW_TRIANGLE_BUDGET: u64 = EXTRA_PASS_TRIANGLE_BUDGET;

/// Whether a frame of `triangles` fill triangles may draw extra passes.
fn extra_passes_fit(triangles: u64) -> bool {
    triangles <= EXTRA_PASS_TRIANGLE_BUDGET
}

/// Whether the window's last frame drew shadows, and why not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShadowStatus {
    pub active: bool,
    /// Fill triangles the shadow pass draws (or would draw).
    pub triangles: u64,
    pub reason: Option<&'static str>,
}

/// Lights from objects (torches, candles) the viewport shades with.
pub const MAX_VIEW_LIGHTS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    model: [[f32; 4]; 4],
    /// The object material's emission and transmission
    /// (`Material::light`), then its detail rotation (cos, sin).
    extra: [f32; 4],
    /// The object material's detail texture (`detail::shader_params`).
    detail: [f32; 4],
    /// sRGB color (fill color, or line color for wireframes).
    color: [f32; 4],
    /// The object's material (`Material::surface`), for meshes whose
    /// vertices do not carry one (primitives).
    surface: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<Instance>() == 128, "Instance must match the vertex attributes");

impl Instance {
    pub fn new(model: Mat4, color: [f32; 3]) -> Self {
        Self {
            model: model.to_cols_array_2d(),
            extra: [0.0, 0.0, 1.0, 0.0],
            detail: [0.0; 4],
            color: [color[0], color[1], color[2], 1.0],
            surface: crate::material::Material::default().surface(),
        }
    }

    /// The object's material, for meshes that defer to it.
    pub fn with_material(mut self, m: &crate::material::Material) -> Self {
        self.surface = m.surface();
        let [e, t] = m.light();
        let [detail, [c, s, _, _]] = crate::detail::shader_params(m);
        self.extra = [e, t, c, s];
        self.detail = detail;
        self
    }

    pub fn with_color(mut self, color: [f32; 3]) -> Self {
        self.color = [color[0], color[1], color[2], 1.0];
        self
    }

    /// The mask and group overlay a sculpt chunk shows (0 none, 1 mask,
    /// 2 groups and mask), in the color's alpha (unused by other fills).
    pub fn with_overlay(mut self, overlay: crate::overlay::Overlay) -> Self {
        self.color[3] = match overlay {
            crate::overlay::Overlay::None => 0.0,
            crate::overlay::Overlay::Mask => 1.0,
            crate::overlay::Overlay::Groups => 2.0,
        };
        self
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 4],
}

/// The brush ring on a sculpted surface (the brush tools' cursor): a
/// ring of `radius` around `center` across the surface's `normal`, a ring
/// where the falloff is at half (`focal`, a fraction of the radius) and a
/// tick along the normal. Depth-tested like the grid, so the surface hides
/// the parts behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub center: Vec3,
    pub normal: Vec3,
    pub radius: f32,
    pub focal: f32,
    /// sRGB, 0..1.
    pub color: [f32; 4],
}

/// Segments of each cursor ring.
const CURSOR_SEGMENTS: usize = 64;
/// Line vertices the cursor takes at most (two rings and the tick).
const CURSOR_VERTICES: usize = 4 * CURSOR_SEGMENTS + 2;

impl Cursor {
    /// The cursor as world-space line segments.
    fn lines(&self) -> Vec<LineVertex> {
        let n = self.normal.try_normalize().unwrap_or(Vec3::Y);
        let (u, v) = n.any_orthonormal_pair();
        // Lifted a little off the surface so the ring is not lost in it.
        let lift = n * self.radius * 0.02;
        let mut out = Vec::with_capacity(CURSOR_VERTICES);
        for (r, alpha) in [(self.radius, 1.0), (self.radius * self.focal.clamp(0.05, 1.0), 0.6)] {
            let at = |k: usize| {
                let a = k as f32 / CURSOR_SEGMENTS as f32 * std::f32::consts::TAU;
                self.center + lift + (u * a.cos() + v * a.sin()) * r
            };
            for k in 0..CURSOR_SEGMENTS {
                let color = [self.color[0], self.color[1], self.color[2], self.color[3] * alpha];
                out.push(LineVertex { pos: at(k).into(), color });
                out.push(LineVertex { pos: at(k + 1).into(), color });
            }
        }
        out.push(LineVertex { pos: (self.center + lift).into(), color: self.color });
        out.push(LineVertex { pos: (self.center + n * self.radius * 0.5).into(), color: self.color });
        out
    }
}

struct GpuMesh {
    /// The CPU mesh this was uploaded from; a new Arc means re-upload.
    source: Arc<Mesh>,
    /// Baked textures and detail settings (group 1); None uses
    /// `Resources::no_textures`.
    textures: Option<wgpu::BindGroup>,
    /// The baked images, kept to rebuild group 1 when materials change.
    images: Option<Arc<[wgpu::TextureView; 3]>>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    edges: wgpu::Buffer,
    edge_count: u32,
    /// Local bounds of `source` (None when empty), for fitting shadows.
    bounds: Option<(Vec3, Vec3)>,
    /// Unique per upload, so a cached shadow map sees mesh edits.
    generation: u64,
}

/// Group 3 of the mesh pipeline: lighting resources shared by every mesh.
/// The mesh pipelines use all four default bind groups, so later lighting
/// (environment, AO) adds bindings here rather than a new group:
///   0 shadow map (texture_depth_2d)
///   1 shadow comparison sampler (linear, LessEqual: 2x2 PCF per tap)
///   2 shadow point sampler (non-filtering)
///   3 environment reflections (Rgba16Float equirect, mips by roughness)
///   4 environment sampler (repeat u, clamp v, trilinear)
///   5 BRDF lookup table (Rg16Float split-sum scale and bias)
///   6 clamp sampler (linear; the BRDF table)
///   7 ambient occlusion (Rg16Float occlusion and distance, textureLoad)
///   8 the matcap (Rgba8UnormSrgb, sampled with the clamp sampler)
mod lighting_bindings {
    pub const SHADOW_MAP: u32 = 0;
    pub const SHADOW_COMPARE: u32 = 1;
    pub const SHADOW_POINT: u32 = 2;
    pub const ENV_MAP: u32 = 3;
    pub const ENV_SAMPLER: u32 = 4;
    pub const BRDF: u32 = 5;
    pub const CLAMP: u32 = 6;
    pub const AO: u32 = 7;
    pub const MATCAP: u32 = 8;
}

/// A unique id for GPU resources that bind groups are keyed by.
fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A key-light shadow map.
struct ShadowMap {
    view: wgpu::TextureView,
    /// Unique per map, for the lighting groups that read it.
    id: u64,
    size: u32,
    /// What the map holds (light matrix, casters, mesh uploads); a pass
    /// whose key matches is skipped.
    key: Option<u64>,
    /// Whether the last frame drawn with it had shadows, and why not.
    status: ShadowStatus,
}

const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

struct Slot {
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    /// Ambient occlusion targets at half the pane's size (None when off).
    ao: Option<AoTargets>,
    /// Group 3 of the mesh pipelines, and what it binds (shadow map,
    /// environment, occlusion and matcap ids).
    lighting: Option<([u64; 4], wgpu::BindGroup)>,
    /// The brush cursor's lines (`Cursor`), and how many are drawn.
    cursor: wgpu::Buffer,
    cursor_count: u32,
}

/// Ambient occlusion: the pre-pass's depth and normals, the raw
/// occlusion and the blurred (occlusion, distance) the fills read.
struct AoTargets {
    size: [u32; 2],
    id: u64,
    depth: wgpu::TextureView,
    normal: wgpu::TextureView,
    raw: wgpu::TextureView,
    blurred: wgpu::TextureView,
    /// Group 1 of the AO pass (depth, normals) and of the blur (depth, raw).
    ao_group: wgpu::BindGroup,
    blur_group: wgpu::BindGroup,
}

/// Screen-space ambient occlusion settings (viewport and screenshots;
/// render_film traces its own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ao {
    pub enabled: bool,
    /// How far around a point occluders count, in world units.
    pub radius: f32,
    /// 0..2: how dark full occlusion gets (1 = black ambient).
    pub strength: f32,
}

impl Default for Ao {
    fn default() -> Self {
        Ao { enabled: true, radius: 0.5, strength: 1.0 }
    }
}

/// Whether a frame drew ambient occlusion, and why not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AoStatus {
    pub active: bool,
    pub reason: Option<&'static str>,
}

/// An environment's reflections on the GPU (written once, then only read,
/// so panes and screenshots with different lighting never disturb each
/// other).
struct GpuEnv {
    probe: Arc<EnvProbe>,
    view: wgpu::TextureView,
    id: u64,
    /// When it was last bound (`Resources::env_clock`).
    used: u64,
}

/// Environments kept on the GPU at once (the least recently used goes).
const GPU_ENVS: usize = 8;

/// Number of panes that can draw in one frame (Maya's four view).
pub const SLOTS: usize = 4;
/// Extra slot used by `render_image`.
const OFFSCREEN_SLOT: usize = SLOTS;

struct Resources {
    mesh_pipeline: wgpu::RenderPipeline,
    /// The fills and the shadow catcher into render::post's HDR target.
    mesh_hdr_pipeline: wgpu::RenderPipeline,
    ground_hdr_pipeline: wgpu::RenderPipeline,
    post: post::PostPipelines,
    /// Each window pane's HDR targets (None on the direct path), and the
    /// frame it last drew; a pane gone for a few frames gives them back.
    post_targets: Vec<Option<post::PostTargets>>,
    post_seen: [u64; SLOTS],
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Placeholder textures for meshes without a bake (never read: their
    /// tangent w is 0).
    no_textures: wgpu::BindGroup,
    blank: Arc<[wgpu::TextureView; 3]>,
    /// Library detail textures (group 2), one layer each, uploaded the
    /// first time a material uses one.
    detail_group: wgpu::BindGroup,
    detail_texture: wgpu::Texture,
    detail_loaded: Vec<bool>,
    wire_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    /// One slot per view pane: every pane's `prepare` runs before any
    /// `paint`, so panes cannot share uniform or instance buffers.
    slots: Vec<Slot>,
    meshes: HashMap<MeshKey, GpuMesh>,
    /// Next `GpuMesh::generation`.
    mesh_generation: u64,
    grid: wgpu::Buffer,
    grid_count: u32,
    gamma_encode: bool,
    target_format: wgpu::TextureFormat,
    shadow_pipeline: wgpu::RenderPipeline,
    ground_pipeline: wgpu::RenderPipeline,
    lighting_layout: wgpu::BindGroupLayout,
    shadow_compare: wgpu::Sampler,
    shadow_point: wgpu::Sampler,
    /// One map for every window pane: panes share the light fit (the
    /// persp camera's focus) and the fills, so the first pane's `prepare`
    /// renders it and the others find its key unchanged.
    window_shadow: ShadowMap,
    /// `render_image`'s own map, so screenshots never disturb the window's.
    offscreen_shadow: ShadowMap,
    /// Whether the window's and `render_image`'s last frames drew ambient
    /// occlusion.
    window_ao: AoStatus,
    offscreen_ao: AoStatus,
    prepass_pipeline: wgpu::RenderPipeline,
    ao_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    ao_layout: wgpu::BindGroupLayout,
    env_sampler: wgpu::Sampler,
    clamp_sampler: wgpu::Sampler,
    brdf: wgpu::TextureView,
    /// Bound where there is no occlusion map (occlusion 1) or no
    /// environment (classic lighting).
    white_ao: wgpu::TextureView,
    black_env: GpuEnv,
    envs: Vec<GpuEnv>,
    env_clock: u64,
    /// Sculpt chunks (slim vertices) and their pipelines: fills, HDR
    /// fills, shadow and occlusion pre-pass.
    pool: pool::ChunkPool,
    sculpt_pipeline: wgpu::RenderPipeline,
    sculpt_hdr_pipeline: wgpu::RenderPipeline,
    sculpt_shadow_pipeline: wgpu::RenderPipeline,
    sculpt_prepass_pipeline: wgpu::RenderPipeline,
    /// The matcap uploaded (its id, 0 for none: a white texel) and its view.
    matcap: (u64, wgpu::TextureView),
}

/// The WGSL side of `Globals`, shared by the viewport and AO shaders.
macro_rules! globals_wgsl {
    () => {
        r#"
struct Globals {
    view_proj: mat4x4<f32>, grid_model: mat4x4<f32>, cam_pos: vec4<f32>, params: vec4<f32>, lights: array<vec4<f32>, 32>,
    light_vp: mat4x4<f32>, key_dir: vec4<f32>, shadow: vec4<f32>, shadow2: vec4<f32>, ground: vec4<f32>,
    inv_view_proj: mat4x4<f32>, sh: array<vec4<f32>, 9>, sun_dir: array<vec4<f32>, 4>, sun_col: array<vec4<f32>, 4>,
    env: vec4<f32>, env2: vec4<f32>, ao: vec4<f32>, ao_size: vec4<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;
"#
    };
}

const SHADER: &str = concat!(
    globals_wgsl!(),
    r#"

fn to_linear(c: vec3<f32>) -> vec3<f32> { return pow(c, vec3(2.2)); }
fn encode(c: vec3<f32>) -> vec3<f32> {
    if (g.params.x > 0.5) { return pow(c, vec3(1.0 / 2.2)); }
    return c;
}

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // a = emission
    @location(2) color: vec4<f32>,
    // w = transmission
    @location(3) vcolor: vec4<f32>,
    @location(4) surface: vec4<f32>,
    // zw = detail rotation (cos, sin)
    @location(5) uv: vec4<f32>,
    @location(6) tangent: vec4<f32>,
    @location(7) local_pos: vec3<f32>,
    @location(8) local_normal: vec3<f32>,
    @location(9) @interpolate(flat) detail: vec4<f32>,
    @location(10) @interpolate(flat) nm0: vec3<f32>,
    @location(11) @interpolate(flat) nm1: vec3<f32>,
    @location(12) @interpolate(flat) nm2: vec3<f32>,
};

@group(1) @binding(0) var t_color: texture_2d<f32>;
@group(1) @binding(1) var t_normal: texture_2d<f32>;
@group(1) @binding(2) var t_orm: texture_2d<f32>;
@group(1) @binding(3) var t_sampler: sampler;
// Per material: (layer + 1, tiles per unit, strength, color), (cos, sin, 0, 0).
@group(1) @binding(4) var<uniform> details: array<vec4<f32>, 128>;
// Library detail maps: normal slope x, y (0.5 = flat), brightness / 2,
// roughness offset + 0.5.
@group(2) @binding(0) var t_detail: texture_2d_array<f32>;
@group(2) @binding(1) var s_detail: sampler;
// Lighting (see `lighting_bindings`): the key light's shadow map.
@group(3) @binding(0) var t_shadow: texture_depth_2d;
@group(3) @binding(1) var s_shadow: sampler_comparison;
// Unfiltered reads use textureLoad; the point sampler is for passes that
// need sampling without filtering.
@group(3) @binding(2) var s_shadow_point: sampler;
// The environment's reflections (mip k blurred for roughness k / 5).
@group(3) @binding(3) var t_env: texture_2d<f32>;
@group(3) @binding(4) var s_env: sampler;
// Split-sum BRDF (scale, bias) by (n.v, roughness).
@group(3) @binding(5) var t_brdf: texture_2d<f32>;
@group(3) @binding(6) var s_clamp: sampler;
// Ambient occlusion at half size: (occlusion, distance from the camera).
@group(3) @binding(7) var t_ao: texture_2d<f32>;
// MatCap shading (`render::matcap`), on when env2.z is 1.
@group(3) @binding(8) var t_matcap: texture_2d<f32>;

@vertex
fn vs_mesh(
    @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>, @location(7) idetail: vec4<f32>, @location(8) material: u32,
    @location(9) color: vec4<f32>, @location(10) vcolor: vec3<f32>,
    @location(11) vsurface: vec4<f32>, @location(12) isurface: vec4<f32>,
    @location(13) uv: vec2<f32>, @location(14) tangent: vec4<f32>,
    @location(15) vlight: vec2<f32>,
) -> MeshOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4(pos, 1.0);
    // The normal matrix (inverse transpose) up to scale: the cofactors.
    let flip = sign(dot(m0.xyz, cross(m1.xyz, m2.xyz)));
    let nm = mat3x3<f32>(cross(m1.xyz, m2.xyz), cross(m2.xyz, m0.xyz), cross(m0.xyz, m1.xyz)) * flip;
    var out: MeshOut;
    out.clip = g.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = nm * normal;
    // Primitives share one mesh, so their vertices defer to the object.
    let object = vsurface.x < 0.0;
    out.surface = select(vsurface, isurface, object);
    let light = select(vlight, extra.xy, object);
    out.color = vec4(color.rgb, light.x);
    out.vcolor = vec4(vcolor, light.y);
    let slot = min(material, 63u) * 2u;
    out.detail = select(details[slot], idetail, object);
    out.uv = vec4(uv, select(details[slot + 1u].xy, extra.zw, object));
    out.tangent = vec4(mat3x3<f32>(m0.xyz, m1.xyz, m2.xyz) * tangent.xyz, tangent.w);
    out.local_pos = pos;
    out.local_normal = normal;
    out.nm0 = nm[0];
    out.nm1 = nm[1];
    out.nm2 = nm[2];
    return out;
}

// The slim sculpt vertex (`mesh::SculptVertex`): an octahedral normal.
fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var n = vec3(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return normalize(n);
}

// A polygroup's tint (`overlay::group_color`).
fn group_tint(id: u32) -> vec3<f32> {
    let h = fract(f32(id) * 0.618034) * 6.0;
    let x = 1.0 - abs(h % 2.0 - 1.0);
    var c = vec3(1.0, 0.0, x);
    switch (u32(h)) {
        case 0u: { c = vec3(1.0, x, 0.0); }
        case 1u: { c = vec3(x, 1.0, 0.0); }
        case 2u: { c = vec3(0.0, 1.0, x); }
        case 3u: { c = vec3(0.0, x, 1.0); }
        case 4u: { c = vec3(x, 0.0, 1.0); }
        default: {}
    }
    return c * 0.75 + 0.2;
}

// A sculpt chunk's vertex: the object's material (as primitives), its own
// color with the mask and group overlay (instance color.a: 0 none, 1 mask,
// 2 groups), the same fragment shading as every fill.
@vertex
fn vs_sculpt(
    @location(0) pos: vec3<f32>, @location(1) enc: vec2<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>, @location(7) idetail: vec4<f32>,
    @location(9) color: vec4<f32>, @location(10) vcolor: vec4<f32>,
    @location(12) isurface: vec4<f32>, @location(13) attrs: vec4<u32>,
) -> MeshOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4(pos, 1.0);
    let flip = sign(dot(m0.xyz, cross(m1.xyz, m2.xyz)));
    let nm = mat3x3<f32>(cross(m1.xyz, m2.xyz), cross(m2.xyz, m0.xyz), cross(m0.xyz, m1.xyz)) * flip;
    let normal = oct_decode(enc);
    var c = vcolor.rgb;
    let group = attrs.y + attrs.z * 256u;
    if (color.a > 1.5 && group != 0u) { c = mix(c, group_tint(group), 0.6); }
    let mask = f32(attrs.x) / 255.0;
    if (color.a > 0.5 && mask > 0.0) { c = c * (1.0 - 0.6 * mask); }
    // MatCaps darken creases (the lattice's cavity).
    if (g.env2.z > 0.5) { c = c * (1.0 - 0.5 * f32(attrs.w) / 255.0); }
    var out: MeshOut;
    out.clip = g.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = nm * normal;
    out.surface = isurface;
    out.color = vec4(color.rgb, extra.x);
    out.vcolor = vec4(c, extra.y);
    out.detail = idetail;
    out.uv = vec4(0.0, 0.0, extra.zw);
    out.tangent = vec4(0.0);
    out.local_pos = pos;
    out.local_normal = normal;
    out.nm0 = nm[0];
    out.nm1 = nm[1];
    out.nm2 = nm[2];
    return out;
}

struct Plane { bend: vec3<f32>, tex: vec2<f32> };

// One projection of a triplanar detail lookup: u along `tu`, v along `tv`.
fn detail_plane(p: vec3<f32>, dx: vec3<f32>, dy: vec3<f32>, tu: vec3<f32>, tv: vec3<f32>, freq: f32, turn: vec2<f32>, layer: i32) -> Plane {
    let rot = mat2x2<f32>(turn.x, turn.y, -turn.y, turn.x);
    let uv = rot * (vec2(dot(p, tu), dot(p, tv)) * freq);
    let gx = rot * (vec2(dot(dx, tu), dot(dx, tv)) * freq);
    let gy = rot * (vec2(dot(dy, tu), dot(dy, tv)) * freq);
    // Image rows run down; v runs up.
    let t = textureSampleGrad(t_detail, s_detail, vec2(uv.x, -uv.y), layer, vec2(gx.x, -gx.y), vec2(gy.x, -gy.y));
    let slope = (t.xy - 0.5) * 2.0;
    let ru = tu * turn.x - tv * turn.y;
    let rv = tu * turn.y + tv * turn.x;
    var out: Plane;
    out.bend = ru * slope.x + rv * slope.y;
    out.tex = vec2(t.z * 2.0, t.w - 0.5);
    return out;
}

// GGX microfacet highlight for one light (D * G * F / (4 NL NV), times NL).
fn ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, rough: f32, f0: vec3<f32>) -> vec3<f32> {
    let h = normalize(l + v);
    let nl = max(dot(n, l), 0.0);
    let nv = max(dot(n, v), 1e-3);
    let nh = max(dot(n, h), 0.0);
    let a = rough * rough;
    let a2 = a * a;
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (PI * dd * dd);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    let gv = nv / (nv * (1.0 - k) + k);
    let gl = nl / (nl * (1.0 - k) + k);
    let f = f0 + (1.0 - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    return d * gv * gl * f / (4.0 * nv) ;
}

// A soft studio: bright sky above, warm floor below, brighter toward the key.
fn studio(r: vec3<f32>, key: vec3<f32>, rough: f32) -> vec3<f32> {
    let sky = mix(vec3(0.18, 0.17, 0.16), vec3(0.62, 0.66, 0.72), smoothstep(-0.3, 0.6, r.y));
    let spot = pow(max(dot(r, key), 0.0), mix(64.0, 2.0, rough)) * mix(1.6, 0.25, rough);
    return sky + vec3(spot);
}

// Interleaved gradient noise: a fixed per-pixel rotation for the filter
// taps, so their pattern does not band.
fn pixel_noise(frag: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(frag, vec2(0.06711056, 0.00583715))));
}

// Tap i of n on a unit disk (Vogel's spiral), turned by `turn`.
fn vogel(i: i32, n: f32, turn: f32) -> vec2<f32> {
    let r = sqrt((f32(i) + 0.5) / n);
    let a = f32(i) * 2.39996323 + turn;
    return r * vec2(cos(a), sin(a));
}

// How much of the key light reaches `world` (1 = lit, 0 = in shadow).
// `n` is the geometric normal; the sample point moves along it a little so
// a surface does not shadow itself. 1 outside the map.
fn shadow_vis(world: vec3<f32>, n: vec3<f32>, frag: vec2<f32>) -> f32 {
    let key = normalize(g.key_dir.xyz);
    let toward = select(n, -n, dot(n, key) < 0.0);
    let p = g.light_vp * vec4(world + toward * g.shadow2.x, 1.0);
    let uv = vec2(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    let depth = p.z;
    if (any(uv < vec2(0.0)) || any(uv > vec2(1.0)) || depth > 1.0) { return 1.0; }
    let turn = pixel_noise(frag) * 6.2831853;
    var radius = g.shadow.z;
    if (g.shadow.w > 0.5) {
        // Soft: the penumbra widens with the gap between the receiver and
        // the average blocker, as under a key of the film's angular size.
        let size = i32(round(1.0 / g.shadow.y));
        let search = clamp(depth * g.shadow2.y * g.shadow2.z / g.shadow2.w, 2.0, 24.0);
        var blockers = 0.0;
        var sum = 0.0;
        for (var i = 0; i < 12; i++) {
            let at = (uv + vogel(i, 12.0, turn) * search * g.shadow.y) * f32(size);
            let texel = clamp(vec2<i32>(floor(at)), vec2(0), vec2(size - 1));
            let d = textureLoad(t_shadow, texel, 0);
            if (d < depth) {
                blockers += 1.0;
                sum += d;
            }
        }
        if (blockers < 0.5) { return 1.0; }
        let gap = (depth - sum / blockers) * g.shadow2.y;
        radius = clamp(gap * g.shadow2.z / g.shadow2.w, 1.5, 24.0);
    }
    var lit = 0.0;
    for (var i = 0; i < 16; i++) {
        lit += textureSampleCompareLevel(t_shadow, s_shadow, uv + vogel(i, 16.0, turn) * radius * g.shadow.y, depth);
    }
    return lit / 16.0;
}

// A fill's linear radiance, and its alpha (glass is partly clear).
fn shade(in: MeshOut) -> vec4<f32> {
    // Winding is not consistent across meshes, so face the normal toward the viewer.
    let v = normalize(g.cam_pos.xyz - in.world_pos);
    var n = normalize(in.normal);
    var albedo = to_linear(in.color.rgb) * to_linear(in.vcolor.rgb);
    var rough = clamp(in.surface.x, 0.04, 1.0);
    // Library detail, triplanar in object space (bakes already carry it).
    let lp = in.local_pos;
    let ln = normalize(in.local_normal);
    let dx = dpdx(lp);
    let dy = dpdy(lp);
    let layer = max(i32(in.detail.x + 0.5) - 1, 0);
    var w = pow(abs(ln), vec3(4.0));
    w = w / max(w.x + w.y + w.z, 1e-6);
    let s = select(vec3(1.0), vec3(-1.0), ln < vec3(0.0));
    let px = detail_plane(lp, dx, dy, vec3(0.0, 0.0, -s.x), vec3(0.0, 1.0, 0.0), in.detail.y, in.uv.zw, layer);
    let py = detail_plane(lp, dx, dy, vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, -s.y), in.detail.y, in.uv.zw, layer);
    let pz = detail_plane(lp, dx, dy, vec3(s.z, 0.0, 0.0), vec3(0.0, 1.0, 0.0), in.detail.y, in.uv.zw, layer);
    if (in.detail.x > 0.5 && in.tangent.w == 0.0) {
        var bend = px.bend * w.x + py.bend * w.y + pz.bend * w.z;
        bend = bend - ln * dot(ln, bend);
        let tex = px.tex * w.x + py.tex * w.y + pz.tex * w.z;
        let nm = mat3x3<f32>(in.nm0, in.nm1, in.nm2);
        n = normalize(nm * (ln + bend * in.detail.z));
        albedo *= mix(1.0, tex.x, in.detail.w);
        rough = clamp(rough + tex.y * 0.6 * min(in.detail.z, 1.5), 0.04, 1.0);
    }
    if (dot(normalize(in.normal), v) < 0.0) { n = -n; }
    var metal = clamp(in.surface.y, 0.0, 1.0);
    var ao = 1.0;
    // Baked meshes (tangent w = +-1): color, normal map, and AO/rough/metal
    // from textures. Sample always; branching on a varying cannot sample.
    let tex_color = textureSample(t_color, t_sampler, in.uv.xy);
    let tex_normal = textureSample(t_normal, t_sampler, in.uv.xy).xyz * 2.0 - 1.0;
    let tex_orm = textureSample(t_orm, t_sampler, in.uv.xy);
    if (in.tangent.w != 0.0) {
        // Cut-out textures (hair cards) are clear where alpha is low;
        // opaque bakes have alpha 1 everywhere.
        if (tex_color.a < 0.5) { discard; }
        albedo *= tex_color.rgb;
        let t = normalize(in.tangent.xyz - n * dot(n, in.tangent.xyz));
        let b = cross(n, t) * in.tangent.w;
        n = normalize(t * tex_normal.x + b * tex_normal.y + n * tex_normal.z);
        ao = tex_orm.r;
        rough = clamp(tex_orm.g, 0.04, 1.0);
        metal = tex_orm.b;
    }
    // MatCap: the image's sphere at the normal as the camera sees it
    // (`render::matcap::uv`), in the surface's colors.
    if (g.env2.z > 0.5) {
        var right = cross(vec3(0.0, 1.0, 0.0), v);
        if (dot(right, right) < 1e-8) { right = vec3(1.0, 0.0, 0.0); }
        right = normalize(right);
        let up = cross(v, right);
        let muv = vec2(0.5 + 0.5 * dot(n, right), 0.5 - 0.5 * dot(n, up));
        return vec4(textureSampleLevel(t_matcap, s_clamp, muv, 0.0).rgb * albedo, 1.0);
    }
    let surf = Surf(n, v, albedo, rough, metal, ao, clamp(in.surface.z, 0.0, 1.0), clamp(in.surface.w, 0.0, 1.0), clamp(in.vcolor.w, 0.0, 1.0));
    var c: vec3<f32>;
    if (g.env.x > 0.5) {
        c = light_environment(surf, in);
    } else {
        c = light_classic(surf, in);
    }
    // Glass: mostly see-through, with a strong reflection at the edges.
    let edge = 0.04 + 0.96 * pow(1.0 - max(dot(n, v), 0.0), 5.0);
    let alpha = 1.0 - surf.glass * (1.0 - edge) * 0.8;
    // Emission: the surface's own light, in its color.
    c += albedo * max(in.color.a, 0.0);
    return vec4(c, alpha);
}

// A fill's material at one pixel, after textures and detail.
struct Surf {
    n: vec3<f32>,
    v: vec3<f32>,
    albedo: vec3<f32>,
    rough: f32,
    metal: f32,
    // Baked ambient occlusion.
    ao: f32,
    coat: f32,
    sheen: f32,
    glass: f32,
};

// Where the shadow map lets the key light through (1 without shadows).
fn key_visibility(in: MeshOut) -> f32 {
    if (g.shadow.x > 0.5) { return mix(1.0, shadow_vis(in.world_pos, normalize(in.normal), in.clip.xy), g.key_dir.w); }
    return 1.0;
}

// A weak light just above and right of the camera, like a photographer's
// ring light: it puts a catchlight in every eye from any view.
fn catchlight(world: vec3<f32>) -> vec3<f32> {
    let to_cam = normalize(g.cam_pos.xyz - world);
    let right = normalize(cross(vec3(0.0, 1.0, 0.0), to_cam) + vec3(1e-4, 0.0, 0.0));
    return normalize(to_cam + vec3(0.0, 0.3, 0.0) + right * 0.2);
}

// Lights carried by objects: warm pools that fall off with distance
// (no shadows from these; render_film has them).
fn object_lights(s: Surf, world: vec3<f32>, f0: vec3<f32>) -> vec3<f32> {
    var c = vec3(0.0);
    for (var i = 0; i < i32(g.params.z); i++) {
        let lp = g.lights[i * 2];
        let delta = lp.xyz - world;
        let d2 = max(dot(delta, delta), lp.w * lp.w);
        let l = normalize(delta);
        let nl = max(dot(s.n, l), 0.0);
        let e = g.lights[i * 2 + 1].rgb / d2;
        c += (s.albedo * (1.0 - s.metal) / PI + ggx(s.n, s.v, l, s.rough, f0)) * nl * e;
    }
    return c;
}

// The viewport's lighting before it took render_film's: a key, a fill and
// the catchlight, and a made-up studio for ambient light and reflections.
fn light_classic(s: Surf, in: MeshOut) -> vec3<f32> {
    let n = s.n;
    let v = s.v;
    let albedo = s.albedo;
    let rough = s.rough;
    let metal = s.metal;
    let ao = s.ao;
    let coat = s.coat;
    let sheen = s.sheen;
    let f0 = mix(vec3(0.04), albedo, metal);
    let nv = max(dot(n, v), 0.0);
    let fresnel = f0 + (max(vec3(1.0 - rough), f0) - f0) * pow(1.0 - nv, 5.0);
    let coat_f = 0.04 + 0.96 * pow(1.0 - nv, 5.0);

    let key = normalize(g.key_dir.xyz);
    // Shadows take away the key light only (its diffuse and highlights).
    let occlusion = array<f32, 3>(key_visibility(in), 1.0, 1.0);
    let fill = normalize(vec3(-0.6, 0.2, -0.4));
    let lights = array<vec3<f32>, 3>(key, fill, catchlight(in.world_pos));
    let studio_k = g.params.w;
    let power = array<f32, 3>(0.8 * studio_k, 0.22 * studio_k, 0.12 * studio_k);
    var c = vec3(0.0);
    for (var i = 0; i < 3; i++) {
        let l = lights[i];
        let nl = max(dot(n, l), 0.0);
        // Light is scaled so a matte white surface matches the old Lambert look.
        let radiance = power[i] * PI;
        let diffuse = albedo * (1.0 - metal) / PI * nl;
        let spec = ggx(n, v, l, rough, f0) * nl;
        let clear = ggx(n, v, l, 0.06, vec3(0.04)) * nl * coat;
        // Fuzz takes the surface color (as glTF sheen does in exports), so
        // dark cloth stays dark.
        let fuzz = albedo;
        let rim = sheen * fuzz * pow(1.0 - nv, 4.0) * sqrt(nl) / PI;
        // Occlusion is ambient light's; a little on direct light stands in
        // for the contact shadows a game engine adds.
        c += (diffuse * (1.0 - coat * coat_f) + spec * (1.0 - coat * coat_f) + clear + rim) * radiance * mix(1.0, ao, 0.35) * occlusion[i];
    }
    c += object_lights(s, in.world_pos, f0);
    // Ambient from the studio: diffuse sky light, then reflections.
    let hemi = mix(vec3(0.1, 0.09, 0.08), vec3(0.17, 0.18, 0.2), n.y * 0.5 + 0.5) * studio_k;
    c += albedo * (1.0 - metal) * hemi * ao;
    let r = reflect(-v, n);
    c += studio(r, key, rough) * fresnel * (1.0 - coat * coat_f) * (1.0 - 0.5 * rough) * ao * studio_k;
    c += studio(r, key, 0.06) * coat_f * coat * ao * studio_k;
    c += sheen * albedo * pow(1.0 - nv, 4.0) * hemi * 2.0 * ao;
    let edge = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
    return mix(c, studio(r, key, rough) * edge + albedo * 0.08, s.glass);
}

// Irradiance from the environment's SH9 (see `view_light::sh_basis`).
fn irradiance(n: vec3<f32>) -> vec3<f32> {
    let e = g.sh[0].rgb * 0.282095
        + (g.sh[1].rgb * n.y + g.sh[2].rgb * n.z + g.sh[3].rgb * n.x) * 0.488603
        + (g.sh[4].rgb * (n.x * n.y) + g.sh[5].rgb * (n.y * n.z) + g.sh[7].rgb * (n.x * n.z)) * 1.092548
        + g.sh[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0))
        + g.sh[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y));
    return max(e, vec3(0.0));
}

// The environment seen along `d` through a GGX lobe of `rough`.
fn prefiltered(d: vec3<f32>, rough: f32) -> vec3<f32> {
    let uv = vec2(fract((atan2(d.z, d.x) - g.env.w) / TAU), atan2(length(d.xz), d.y) / PI);
    return textureSampleLevel(t_env, s_env, uv, clamp(rough * 5.0, 0.0, g.env.z)).rgb;
}

// Split-sum BRDF: (scale, bias) on f0.
fn env_brdf(nv: f32, rough: f32) -> vec2<f32> {
    return textureSampleLevel(t_brdf, s_clamp, vec2(nv, rough), 0.0).rg;
}

// Screen-space ambient occlusion at `world`: the half-size map, upsampled
// from the four nearest texels that are about as far from the camera.
fn ssao(world: vec3<f32>) -> f32 {
    if (g.ao.x < 0.5) { return 1.0; }
    let clip = g.view_proj * vec4(world, 1.0);
    let uv = vec2(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5);
    let at = uv * g.ao_size.xy - 0.5;
    let base = vec2<i32>(floor(at));
    let f = at - floor(at);
    let last = vec2<i32>(g.ao_size.xy) - 1;
    let dist = distance(world, g.cam_pos.xyz);
    var sum = 0.0;
    var weight = 0.0;
    var plain = 0.0;
    for (var i = 0; i < 4; i++) {
        let o = vec2(i & 1, i >> 1u);
        let t = textureLoad(t_ao, clamp(base + o, vec2(0), last), 0).rg;
        let bilinear = select(1.0 - f.x, f.x, o.x == 1) * select(1.0 - f.y, f.y, o.y == 1);
        let w = bilinear * exp(-abs(t.y - dist) / (0.05 * dist + 1e-4));
        sum += t.x * w;
        weight += w;
        plain += t.x * bilinear;
    }
    return select(plain, sum / weight, weight > 1e-3);
}

// render_film's lighting: its suns (the first shadowed) and environment,
// ambient light darkened by occlusion. Diffuse gives up what the surface
// reflects, as film's does.
fn light_environment(s: Surf, in: MeshOut) -> vec3<f32> {
    let n = s.n;
    let v = s.v;
    let f0 = mix(vec3(0.04), s.albedo, s.metal);
    let nv = max(dot(n, v), 0.0);
    let fv = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    let kd = s.albedo * (1.0 - s.metal) * (1.0 - fv) / PI;
    let coat_f = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
    let base = 1.0 - s.coat * coat_f;
    let scale = g.env2.y;
    let key_vis = key_visibility(in);
    var c = vec3(0.0);
    for (var i = 0; i < 5; i++) {
        var l: vec3<f32>;
        var e: vec3<f32>;
        if (i < i32(g.env.y)) {
            l = normalize(g.sun_dir[i].xyz);
            e = g.sun_col[i].rgb * select(1.0, key_vis, i == 0);
        } else if (i == 4 && g.env2.x > 0.0) {
            l = catchlight(in.world_pos);
            e = vec3(g.env2.x);
        } else {
            continue;
        }
        let nl = max(dot(n, l), 0.0);
        // ggx() already carries the cosine.
        let spec = ggx(n, v, l, s.rough, f0);
        let clear = ggx(n, v, l, 0.06, vec3(0.04)) * s.coat;
        let rim = s.sheen * s.albedo * pow(1.0 - nv, 4.0) * sqrt(nl) / PI;
        c += ((kd * nl + spec) * base + clear + rim) * e * scale * mix(1.0, s.ao, 0.35);
    }
    c += object_lights(s, in.world_pos, f0);
    // Ambient: occlusion takes the smaller of the baked and screen-space
    // terms, so a baked mesh is not darkened twice.
    let occ = min(s.ao, ssao(in.world_pos));
    let e = irradiance(n);
    let r = reflect(-v, n);
    let ab = env_brdf(nv, s.rough);
    let cb = env_brdf(nv, 0.06);
    let spec = prefiltered(r, s.rough) * (f0 * ab.x + ab.y);
    let clear = prefiltered(r, 0.06) * (0.04 * cb.x + cb.y) * s.coat;
    let fuzz = s.sheen * s.albedo * pow(1.0 - nv, 4.0) * e * 0.5 / PI;
    c += ((kd * e + spec) * base + clear + fuzz) * occ * scale;
    let edge = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
    return mix(c, (prefiltered(r, s.rough) * edge + s.albedo * e * (0.25 / PI)) * scale, s.glass);
}

// The direct path: the pane's colour as it is (no tonemap; render::post
// owns exposure and tonemapping).
@fragment
fn fs_mesh(in: MeshOut) -> @location(0) vec4<f32> {
    if (g.params.y > 0.5) { return vec4(encode(to_linear(in.color.rgb)), 1.0); }
    let s = shade(in);
    return vec4(encode(s.rgb), s.a);
}

// The HDR path: linear radiance (kept inside half-float range) for
// render::post to expose, bloom and tonemap.
@fragment
fn fs_mesh_hdr(in: MeshOut) -> @location(0) vec4<f32> {
    if (g.params.y > 0.5) { return vec4(to_linear(in.color.rgb), 1.0); }
    let s = shade(in);
    return vec4(clamp(s.rgb, vec3(0.0), vec3(60000.0)), s.a);
}

// The shadow pass: depth only, as seen from the key. Glass and bright
// emitters (flames) cast no shadow, as in render_film.
struct ShadowOut { @builtin(position) clip: vec4<f32>, @location(0) @interpolate(flat) casts: f32 };

@vertex
fn vs_shadow(
    @location(0) pos: vec3<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>, @location(11) vsurface: vec4<f32>, @location(15) vlight: vec2<f32>,
) -> ShadowOut {
    var out: ShadowOut;
    out.clip = g.light_vp * (mat4x4<f32>(m0, m1, m2, m3) * vec4(pos, 1.0));
    let light = select(vlight, extra.xy, vsurface.x < 0.0);
    out.casts = select(1.0, 0.0, light.y > 0.3 || light.x > 1.5);
    return out;
}

@vertex
fn vs_sculpt_shadow(
    @location(0) pos: vec3<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>,
) -> ShadowOut {
    var out: ShadowOut;
    out.clip = g.light_vp * (mat4x4<f32>(m0, m1, m2, m3) * vec4(pos, 1.0));
    out.casts = select(1.0, 0.0, extra.y > 0.3 || extra.x > 1.5);
    return out;
}

@fragment
fn fs_shadow(in: ShadowOut) {
    if (in.casts < 0.5) { discard; }
}

// The shadow catcher: a quad on the grid plane that darkens where the key
// is blocked and is clear elsewhere. It sits behind any real floor there.
// Black is black in both paths, so the HDR pass draws it unchanged
// (premultiplied: it only adds coverage).
struct GroundOut { @builtin(position) clip: vec4<f32>, @location(0) world: vec3<f32> };

@vertex
fn vs_ground(@builtin(vertex_index) i: u32) -> GroundOut {
    let corner = vec2(f32(i & 1u), f32(i >> 1u));
    let xz = mix(g.ground.xy, g.ground.zw, corner);
    var out: GroundOut;
    out.world = vec3(xz.x, 0.0, xz.y);
    out.clip = g.view_proj * vec4(out.world, 1.0);
    return out;
}

@fragment
fn fs_ground(in: GroundOut) -> @location(0) vec4<f32> {
    let vis = shadow_vis(in.world, vec3(0.0, 1.0, 0.0), in.clip.xy);
    // Fade out toward the quad's edges (its margin holds no shadow).
    let margin = max((g.ground.zw - g.ground.xy) * 0.2, vec2(1e-3));
    let inside = min(in.world.xz - g.ground.xy, g.ground.zw - in.world.xz) / margin;
    let fade = smoothstep(0.0, 1.0, min(inside.x, inside.y));
    return vec4(encode(vec3(0.0)), 0.55 * (1.0 - vis) * g.key_dir.w * fade);
}

@vertex
fn vs_wire(
    @location(0) pos: vec3<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(9) color: vec4<f32>,
) -> LineOut {
    var out: LineOut;
    out.clip = g.view_proj * (mat4x4<f32>(m0, m1, m2, m3) * vec4(pos, 1.0));
    out.color = color;
    return out;
}

struct LineOut { @builtin(position) clip: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs_line(@location(0) pos: vec3<f32>, @location(1) color: vec4<f32>) -> LineOut {
    var out: LineOut;
    out.clip = g.view_proj * (g.grid_model * vec4(pos, 1.0));
    out.color = color;
    return out;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    return vec4(encode(to_linear(in.color.rgb)), in.color.a);
}

// The ambient-occlusion pre-pass: world normals (facing the camera) and
// depth of the fills at half size. Glass is left out (it hides nothing),
// and cut-out cards keep their holes.
struct PrepassOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // x = transmission, y = tangent w (baked, so cut-outs apply).
    @location(3) @interpolate(flat) flags: vec2<f32>,
};

@vertex
fn vs_prepass(
    @location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>, @location(11) vsurface: vec4<f32>,
    @location(13) uv: vec2<f32>, @location(14) tangent: vec4<f32>, @location(15) vlight: vec2<f32>,
) -> PrepassOut {
    let world = mat4x4<f32>(m0, m1, m2, m3) * vec4(pos, 1.0);
    let flip = sign(dot(m0.xyz, cross(m1.xyz, m2.xyz)));
    let nm = mat3x3<f32>(cross(m1.xyz, m2.xyz), cross(m2.xyz, m0.xyz), cross(m0.xyz, m1.xyz)) * flip;
    var out: PrepassOut;
    out.clip = g.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = nm * normal;
    out.uv = uv;
    out.flags = vec2(select(vlight, extra.xy, vsurface.x < 0.0).y, tangent.w);
    return out;
}

@vertex
fn vs_sculpt_prepass(
    @location(0) pos: vec3<f32>, @location(1) enc: vec2<f32>,
    @location(2) m0: vec4<f32>, @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>, @location(5) m3: vec4<f32>,
    @location(6) extra: vec4<f32>,
) -> PrepassOut {
    let world = mat4x4<f32>(m0, m1, m2, m3) * vec4(pos, 1.0);
    let flip = sign(dot(m0.xyz, cross(m1.xyz, m2.xyz)));
    let nm = mat3x3<f32>(cross(m1.xyz, m2.xyz), cross(m2.xyz, m0.xyz), cross(m0.xyz, m1.xyz)) * flip;
    var out: PrepassOut;
    out.clip = g.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = nm * oct_decode(enc);
    out.uv = vec2(0.0);
    out.flags = vec2(extra.y, 0.0);
    return out;
}

@fragment
fn fs_prepass(in: PrepassOut) -> @location(0) vec4<f32> {
    let tex = textureSample(t_color, t_sampler, in.uv);
    if (in.flags.x > 0.3 || (in.flags.y != 0.0 && tex.a < 0.5)) { discard; }
    var n = normalize(in.normal);
    if (dot(n, g.cam_pos.xyz - in.world_pos) < 0.0) { n = -n; }
    return vec4(n, 1.0);
}
"#
);

/// Screen-space ambient occlusion from the pre-pass (group 1: its depth,
/// and its normals or the raw occlusion), at half the pane's size: 12
/// taps in the hemisphere over each point, turned by a fixed 4x4 pattern,
/// then a depth-aware 4x4 blur that averages the pattern away. Nothing
/// carries over between frames, so pictures are repeatable.
const AO_SHADER: &str = concat!(
    globals_wgsl!(),
    r#"
@group(1) @binding(0) var t_depth: texture_depth_2d;
@group(1) @binding(1) var t_in: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4(p * 2.0 - 1.0, 0.0, 1.0);
}

// The world position at a texel (w = 0 where nothing was drawn).
fn world_at(texel: vec2<i32>) -> vec4<f32> {
    let d = textureLoad(t_depth, texel, 0);
    let ndc = vec2((f32(texel.x) + 0.5) * g.ao_size.z * 2.0 - 1.0, 1.0 - (f32(texel.y) + 0.5) * g.ao_size.w * 2.0);
    let p = g.inv_view_proj * vec4(ndc, d, 1.0);
    return vec4(p.xyz / p.w, select(1.0, 0.0, d >= 1.0));
}

fn bayer(p: vec2<i32>) -> f32 {
    var m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    return m[(p.y & 3) * 4 + (p.x & 3)];
}

@fragment
fn fs_ao(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(frag.xy);
    let p = world_at(texel);
    if (p.w < 0.5) { return vec4(1.0); }
    let n = normalize(textureLoad(t_in, texel, 0).xyz);
    let up = select(vec3(1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0), abs(n.x) > 0.9);
    let t = normalize(up - n * dot(n, up));
    let b = cross(n, t);
    let turn = bayer(texel) / 16.0 * TAU;
    let radius = g.ao.y;
    let size = vec2<f32>(g.ao_size.xy);
    var occ = 0.0;
    for (var i = 0; i < 12; i++) {
        // Cosine-weighted directions, nearer taps more often.
        let fi = f32(i);
        let h = fract(fi * 0.618034 + 0.31);
        let a = fi * 2.39996323 + turn;
        let dir = vec3(sqrt(h) * cos(a), sqrt(h) * sin(a), max(sqrt(1.0 - h), 0.15));
        let reach = mix(0.15, 1.0, ((fi + 1.0) / 12.0) * ((fi + 1.0) / 12.0));
        let s = p.xyz + (t * dir.x + b * dir.y + n * dir.z) * (radius * reach / length(dir));
        let c = g.view_proj * vec4(s, 1.0);
        if (c.w <= 0.0) { continue; }
        let uv = vec2(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
        if (any(uv < vec2(0.0)) || any(uv >= vec2(1.0))) { continue; }
        let q = world_at(vec2<i32>(uv * size));
        if (q.w < 0.5) { continue; }
        // Something drawn in front of the tap, near enough to shade p.
        let gap = distance(s, g.cam_pos.xyz) - distance(q.xyz, g.cam_pos.xyz);
        let near = 1.0 - smoothstep(radius, 2.0 * radius, distance(q.xyz, p.xyz));
        occ += select(0.0, near, gap > 0.02 * radius);
    }
    return vec4(clamp(1.0 - g.ao.z * occ / 12.0, 0.0, 1.0), 0.0, 0.0, 1.0);
}

// Averages a 4x4 block (every turn of the pattern once), keeping to
// texels at about the same distance; stores (occlusion, distance).
@fragment
fn fs_blur(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(frag.xy);
    let p = world_at(texel);
    if (p.w < 0.5) { return vec4(1.0, 65000.0, 0.0, 1.0); }
    let dc = distance(p.xyz, g.cam_pos.xyz);
    let last = vec2<i32>(g.ao_size.xy) - 1;
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -2; y < 2; y++) {
        for (var x = -2; x < 2; x++) {
            let q = clamp(texel + vec2(x, y), vec2(0), last);
            let wq = world_at(q);
            if (wq.w < 0.5) { continue; }
            let w = exp(-abs(distance(wq.xyz, g.cam_pos.xyz) - dc) / (0.05 * dc + 1e-4));
            sum += textureLoad(t_in, q, 0).r * w;
            weight += w;
        }
    }
    return vec4(sum / max(weight, 1e-6), min(dc, 65000.0), 0.0, 1.0);
}
"#
);

pub fn init(rs: &egui_wgpu::RenderState, meshes: impl Iterator<Item = (MeshKey, Arc<Mesh>)>) {
    let device = &rs.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewport shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });

    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("globals layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let slots = (0..=OFFSCREEN_SLOT)
        .map(|_| {
            let globals = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("globals"),
                size: std::mem::size_of::<Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("globals"),
                layout: &bind_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
            });
            let cursor = device.create_buffer(&wgpu::BufferDescriptor { label: Some("brush cursor"), size: (CURSOR_VERTICES * std::mem::size_of::<LineVertex>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            Slot { globals, bind_group, instances: instance_buffer(device, 64), instance_capacity: 64, ao: None, lighting: None, cursor, cursor_count: 0 }
        })
        .collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport layout"),
        bind_group_layouts: &[&bind_layout],
        push_constant_ranges: &[],
    });
    let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("baked textures layout"),
        entries: &[
            tex_entry(0),
            tex_entry(1),
            tex_entry(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
        ],
    });
    let detail_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("detail layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let lighting_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("lighting layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: lighting_bindings::SHADOW_MAP,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: lighting_bindings::SHADOW_COMPARE,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: lighting_bindings::SHADOW_POINT,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
            tex_entry(lighting_bindings::ENV_MAP),
            wgpu::BindGroupLayoutEntry {
                binding: lighting_bindings::ENV_SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            tex_entry(lighting_bindings::BRDF),
            wgpu::BindGroupLayoutEntry {
                binding: lighting_bindings::CLAMP,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            tex_entry(lighting_bindings::AO),
            tex_entry(lighting_bindings::MATCAP),
        ],
    });
    let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh layout"),
        bind_group_layouts: &[&bind_layout, &texture_layout, &detail_layout, &lighting_layout],
        push_constant_ranges: &[],
    });
    let shadow_compare = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("shadow compare"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        compare: Some(wgpu::CompareFunction::LessEqual),
        ..Default::default()
    });
    let shadow_point = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("shadow point"), ..Default::default() });
    let shadow_size = ShadowQuality::Normal.size().min(device.limits().max_texture_dimension_2d);
    let window_shadow = shadow_map(device, shadow_size);
    let offscreen_shadow = shadow_map(device, shadow_size);
    let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("environment"),
        address_mode_u: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("clamp"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
    let half_texture = |label, format, size: [u32; 2], levels: &[Vec<u16>]| {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_levels(&rs.queue, &texture, levels.iter().enumerate().map(|(k, l)| ((size[0] >> k).max(1), (size[1] >> k).max(1), l.as_slice())));
        texture.create_view(&Default::default())
    };
    let brdf = half_texture("brdf", wgpu::TextureFormat::Rg16Float, [view_light::BRDF_SIZE as u32; 2], &[view_light::brdf_lut().to_vec()]);
    let one = view_light::f16_bits(1.0);
    let white_ao = half_texture("no occlusion", wgpu::TextureFormat::Rg16Float, [1, 1], &[vec![one, view_light::f16_bits(65000.0)]]);
    let black_env = GpuEnv {
        probe: Arc::new(EnvProbe { sh: [Vec3::ZERO; 9], mips: Vec::new(), rotate: 0.0 }),
        view: half_texture("no environment", wgpu::TextureFormat::Rgba16Float, [1, 1], &[vec![0, 0, 0, one]]),
        id: next_id(),
        used: 0,
    };
    let ao_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ambient occlusion layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("baked textures"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        anisotropy_clamp: 8,
        ..Default::default()
    });
    let one = |rgba: [u8; 4]| Arc::new(Textures { size: 1, color: rgba.to_vec(), normal: vec![128, 128, 255, 255], orm: vec![255, 140, 0, 255], alpha: false });
    let blank = Arc::new(texture_views(device, &rs.queue, &one([255; 4])));
    let no_textures = texture_group(device, &texture_layout, &sampler, &blank, &[]);
    let (detail_texture, detail_group) = detail_textures(device, &rs.queue, &detail_layout);

    let pipeline = |label, vs, fs, buffers: &[wgpu::VertexBufferLayout], topology, depth_write, depth_compare, bias, layout: &wgpu::PipelineLayout, format| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers,
            },
            primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: depth_write,
                depth_compare,
                stencil: Default::default(),
                bias,
            }),
            multisample: wgpu::MultisampleState { count: MSAA, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        })
    };

    let at = |location, format, offset| wgpu::VertexAttribute { format, offset: offset as u64, shader_location: location };
    let mesh_attrs = [
        at(0, wgpu::VertexFormat::Float32x3, std::mem::offset_of!(Vertex, pos)),
        at(1, wgpu::VertexFormat::Float32x3, std::mem::offset_of!(Vertex, normal)),
        at(10, wgpu::VertexFormat::Float32x3, std::mem::offset_of!(Vertex, color)),
        at(11, wgpu::VertexFormat::Float32x4, std::mem::offset_of!(Vertex, surface)),
        at(13, wgpu::VertexFormat::Float32x2, std::mem::offset_of!(Vertex, uv)),
        at(14, wgpu::VertexFormat::Float32x4, std::mem::offset_of!(Vertex, tangent)),
        at(15, wgpu::VertexFormat::Float32x2, std::mem::offset_of!(Vertex, light)),
        at(8, wgpu::VertexFormat::Uint32, std::mem::offset_of!(Vertex, material)),
    ];
    let inst_attrs = wgpu::vertex_attr_array![
        2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
        6 => Float32x4, 7 => Float32x4, 9 => Float32x4, 12 => Float32x4
    ];
    let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

    let mesh_buffers = [
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &mesh_attrs,
        },
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &inst_attrs,
        },
    ];
    // Push faces back a little (polygon offset) so wireframes drawn on
    // top of their own faces win the depth test without poking through.
    let face_bias = wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 };
    let mesh_pipeline = pipeline("mesh pipeline", "vs_mesh", "fs_mesh", &mesh_buffers, wgpu::PrimitiveTopology::TriangleList, true, wgpu::CompareFunction::Less, face_bias, &mesh_layout, rs.target_format);
    // The same fills into render::post's HDR target, premultiplied over a
    // clear of nothing.
    let mesh_hdr_pipeline = pipeline("mesh hdr pipeline", "vs_mesh", "fs_mesh_hdr", &mesh_buffers, wgpu::PrimitiveTopology::TriangleList, true, wgpu::CompareFunction::Less, face_bias, &mesh_layout, post::HDR_FORMAT);
    // Sculpt chunks: the slim vertex (`mesh::SculptVertex`), the same
    // instances and fragment shading.
    let sculpt_attrs = [
        at(0, wgpu::VertexFormat::Float32x3, std::mem::offset_of!(SculptVertex, pos)),
        at(1, wgpu::VertexFormat::Snorm16x2, std::mem::offset_of!(SculptVertex, normal)),
        at(10, wgpu::VertexFormat::Unorm8x4, std::mem::offset_of!(SculptVertex, color)),
        at(13, wgpu::VertexFormat::Uint8x4, std::mem::offset_of!(SculptVertex, attrs)),
    ];
    let sculpt_buffers = [
        wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<SculptVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &sculpt_attrs },
        wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Instance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &inst_attrs },
    ];
    let sculpt_pipeline = pipeline("sculpt pipeline", "vs_sculpt", "fs_mesh", &sculpt_buffers, wgpu::PrimitiveTopology::TriangleList, true, wgpu::CompareFunction::Less, face_bias, &mesh_layout, rs.target_format);
    let sculpt_hdr_pipeline = pipeline("sculpt hdr pipeline", "vs_sculpt", "fs_mesh_hdr", &sculpt_buffers, wgpu::PrimitiveTopology::TriangleList, true, wgpu::CompareFunction::Less, face_bias, &mesh_layout, post::HDR_FORMAT);
    let wire_pipeline = pipeline(
        "wire pipeline",
        "vs_wire",
        "fs_line",
        &mesh_buffers,
        wgpu::PrimitiveTopology::LineList,
        false,
        wgpu::CompareFunction::LessEqual,
        Default::default(),
        &layout,
        rs.target_format,
    );
    // The catcher is pushed further back than the mesh pipeline's faces, so
    // a real floor on the grid plane hides it.
    let ground_bias = wgpu::DepthBiasState { constant: 8, slope_scale: 3.0, clamp: 0.0 };
    let ground = |label, format| pipeline(label, "vs_ground", "fs_ground", &[], wgpu::PrimitiveTopology::TriangleStrip, false, wgpu::CompareFunction::LessEqual, ground_bias, &mesh_layout, format);
    let ground_pipeline = ground("ground shadow pipeline", rs.target_format);
    let ground_hdr_pipeline = ground("ground shadow hdr pipeline", post::HDR_FORMAT);
    // Same vertex layouts as the mesh pipeline; the shader reads only the
    // position, the instance matrix and what marks glass and emitters.
    let shadow = |label, vs, buffers: &[wgpu::VertexBufferLayout]| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some(vs), compilation_options: Default::default(), buffers },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: SHADOW_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: Default::default(),
            // Slope bias against acne on surfaces lit at a grazing angle.
            bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_shadow"), compilation_options: Default::default(), targets: &[] }),
        multiview: None,
        cache: None,
    });
    let shadow_pipeline = shadow("shadow pipeline", "vs_shadow", &mesh_buffers);
    let sculpt_shadow_pipeline = shadow("sculpt shadow pipeline", "vs_sculpt_shadow", &sculpt_buffers);
    // The occlusion pre-pass: the mesh pipeline's inputs and group 1 (for
    // cut-outs), one sample, into AO_NORMAL_FORMAT and AO_DEPTH_FORMAT.
    let prepass_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ao pre-pass layout"),
        bind_group_layouts: &[&bind_layout, &texture_layout],
        push_constant_ranges: &[],
    });
    let prepass = |label, vs, buffers: &[wgpu::VertexBufferLayout]| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&prepass_layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some(vs), compilation_options: Default::default(), buffers },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState { format: AO_DEPTH_FORMAT, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::Less, stencil: Default::default(), bias: Default::default() }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_prepass"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: AO_NORMAL_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview: None,
        cache: None,
    });
    let prepass_pipeline = prepass("ao pre-pass pipeline", "vs_prepass", &mesh_buffers);
    let sculpt_prepass_pipeline = prepass("sculpt ao pre-pass pipeline", "vs_sculpt_prepass", &sculpt_buffers);
    let ao_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ambient occlusion shader"), source: wgpu::ShaderSource::Wgsl(AO_SHADER.into()) });
    let ao_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ambient occlusion layout"),
        bind_group_layouts: &[&bind_layout, &ao_layout],
        push_constant_ranges: &[],
    });
    let fullscreen = |label, fs, format| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&ao_pipeline_layout),
            vertex: wgpu::VertexState { module: &ao_shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &ao_shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview: None,
            cache: None,
        })
    };
    let ao_pipeline = fullscreen("ao pipeline", "fs_ao", AO_RAW_FORMAT);
    let blur_pipeline = fullscreen("ao blur pipeline", "fs_blur", AO_FORMAT);
    let line_pipeline = pipeline(
        "grid pipeline",
        "vs_line",
        "fs_line",
        &[wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &line_attrs,
        }],
        wgpu::PrimitiveTopology::LineList,
        false,
        wgpu::CompareFunction::Less,
        Default::default(),
        &layout,
        rs.target_format,
    );

    let mut mesh_generation = 0;
    let gpu_meshes = meshes
        .map(|(key, m)| {
            mesh_generation += 1;
            (key, gpu_mesh(device, &rs.queue, &texture_layout, &sampler, &blank, m, None, mesh_generation))
        })
        .collect();

    let grid_lines = grid_lines();
    let grid = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("grid"),
        contents: bytemuck::cast_slice(&grid_lines),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let resources = Resources {
        mesh_pipeline,
        mesh_hdr_pipeline,
        ground_hdr_pipeline,
        post: post::PostPipelines::new(device, rs.target_format, DEPTH_FORMAT, MSAA),
        post_targets: (0..SLOTS).map(|_| None).collect(),
        post_seen: [0; SLOTS],
        texture_layout,
        sampler,
        no_textures,
        blank,
        detail_group,
        detail_texture,
        detail_loaded: vec![false; crate::detail::LIBRARY.len()],
        wire_pipeline,
        line_pipeline,
        slots,
        meshes: gpu_meshes,
        mesh_generation,
        grid,
        grid_count: grid_lines.len() as u32,
        gamma_encode: !rs.target_format.is_srgb(),
        target_format: rs.target_format,
        shadow_pipeline,
        ground_pipeline,
        lighting_layout,
        shadow_compare,
        shadow_point,
        window_shadow,
        offscreen_shadow,
        window_ao: AoStatus::default(),
        offscreen_ao: AoStatus::default(),
        prepass_pipeline,
        ao_pipeline,
        blur_pipeline,
        ao_layout,
        env_sampler,
        clamp_sampler,
        brdf,
        white_ao,
        black_env,
        envs: Vec::new(),
        env_clock: 0,
        pool: pool::ChunkPool::default(),
        sculpt_pipeline,
        sculpt_hdr_pipeline,
        sculpt_shadow_pipeline,
        sculpt_prepass_pipeline,
        matcap: (0, matcap_view(device, &rs.queue, None)),
    };
    rs.renderer.write().callback_resources.insert(resources);
}

impl Resources {
    /// Indices a fill's mesh or chunk draws (None when not uploaded).
    fn index_count(&self, key: &MeshKey) -> Option<u32> {
        match key {
            MeshKey::Chunk(..) => self.pool.get(key).map(|e| e.index_count),
            _ => self.meshes.get(key).map(|m| m.index_count),
        }
    }

    /// Whether a slot's last frame drew ambient occlusion.
    fn ao_status_mut(&mut self, slot: usize) -> &mut AoStatus {
        if slot == OFFSCREEN_SLOT { &mut self.offscreen_ao } else { &mut self.window_ao }
    }

    /// The reflections texture for `lighting` (uploaded the first time),
    /// and its id.
    fn environment(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, lighting: &ViewLighting) -> (u64, wgpu::TextureView) {
        let Some(probe) = &lighting.probe else { return (self.black_env.id, self.black_env.view.clone()) };
        self.env_clock += 1;
        let clock = self.env_clock;
        if let Some(e) = self.envs.iter_mut().find(|e| Arc::ptr_eq(&e.probe, probe)) {
            e.used = clock;
            return (e.id, e.view.clone());
        }
        if self.envs.len() >= GPU_ENVS
            && let Some(oldest) = self.envs.iter().enumerate().min_by_key(|(_, e)| e.used).map(|(i, _)| i)
        {
            self.envs.swap_remove(oldest);
        }
        let (w, h, _) = probe.mips[0];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("environment"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: probe.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_levels(queue, &texture, probe.mips.iter().map(|(w, h, t)| (*w, *h, t.as_slice())));
        let e = GpuEnv { probe: probe.clone(), view: texture.create_view(&Default::default()), id: next_id(), used: clock };
        let out = (e.id, e.view.clone());
        self.envs.push(e);
        out
    }

    /// The shadow map a slot draws with.
    fn shadow_map(&self, slot: usize) -> &ShadowMap {
        if slot == OFFSCREEN_SLOT { &self.offscreen_shadow } else { &self.window_shadow }
    }

    fn shadow_map_mut(&mut self, slot: usize) -> &mut ShadowMap {
        if slot == OFFSCREEN_SLOT { &mut self.offscreen_shadow } else { &mut self.window_shadow }
    }
}

/// Uploads a mesh; `images` reuses an earlier upload of the same textures.
#[allow(clippy::too_many_arguments)]
fn gpu_mesh(device: &wgpu::Device, queue: &wgpu::Queue, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, blank: &Arc<[wgpu::TextureView; 3]>, m: Arc<Mesh>, images: Option<Arc<[wgpu::TextureView; 3]>>, generation: u64) -> GpuMesh {
    // wgpu cannot bind empty buffers, so empty meshes get a tiny one (never drawn).
    let buffer = |bytes: &[u8], usage| {
        let pad = [0u8; 16];
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh"),
            contents: if bytes.is_empty() { &pad } else { bytes },
            usage,
        })
    };
    // A mesh too big for one GPU buffer (a very fine clay) is drawn as a
    // lighter copy; `source` keeps the full mesh for picking and film.
    let full = m.clone();
    let bounds = (!m.vertices.is_empty()).then(|| m.bounds());
    let m = fit_buffer(m, device.limits().max_buffer_size);
    let images = images.or_else(|| m.textures.as_ref().map(|t| Arc::new(texture_views(device, queue, t))));
    let details: Vec<[[f32; 4]; 2]> = m.materials.iter().take(64).map(crate::detail::shader_params).collect();
    // Meshes without a bake or detail draw with the shared placeholder group.
    let textures = (images.is_some() || details.iter().any(|d| d[0][0] > 0.0)).then(|| texture_group(device, layout, sampler, images.as_ref().unwrap_or(blank), &details));
    GpuMesh {
        textures,
        images,
        vertices: buffer(bytemuck::cast_slice(&m.vertices), wgpu::BufferUsages::VERTEX),
        indices: buffer(bytemuck::cast_slice(&m.indices), wgpu::BufferUsages::INDEX),
        index_count: m.indices.len() as u32,
        edges: buffer(bytemuck::cast_slice(&m.edges), wgpu::BufferUsages::INDEX),
        edge_count: m.edges.len() as u32,
        source: full,
        bounds,
        generation,
    }
}

/// A shadow map of `size` texels a side.
fn shadow_map(device: &wgpu::Device, size: u32) -> ShadowMap {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow map"),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SHADOW_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    ShadowMap { view: texture.create_view(&Default::default()), id: next_id(), size, key: None, status: ShadowStatus::default() }
}

/// Writes mip levels (width, height, half-float texels) of a 16-bit-per-
/// channel texture.
fn write_levels<'a>(queue: &wgpu::Queue, texture: &wgpu::Texture, levels: impl Iterator<Item = (u32, u32, &'a [u16])>) {
    for (mip, (w, h, texels)) in levels.enumerate() {
        let channels = texels.len() as u32 / (w * h);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture, mip_level: mip as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * channels * 2), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }
}

/// Formats of the ambient occlusion targets.
/// A matcap's texture (None: one white texel, never read).
fn matcap_view(device: &wgpu::Device, queue: &wgpu::Queue, m: Option<&matcap::Matcap>) -> wgpu::TextureView {
    let (side, white) = (m.map_or(1, |m| m.side), [255u8; 4]);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("matcap"),
        size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        m.map_or(&white[..], |m| &m.rgba),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
        wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
    );
    texture.create_view(&Default::default())
}

const AO_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const AO_NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const AO_RAW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

impl AoTargets {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, size: [u32; 2]) -> AoTargets {
        let target = |label, format| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let (depth, normal, raw, blurred) = (target("ao depth", AO_DEPTH_FORMAT), target("ao normals", AO_NORMAL_FORMAT), target("ao raw", AO_RAW_FORMAT), target("ao", AO_FORMAT));
        let group = |second: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ambient occlusion"),
                layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&depth) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(second) }],
            })
        };
        let (ao_group, blur_group) = (group(&normal), group(&raw));
        AoTargets { size, id: next_id(), depth, normal, raw, blurred, ao_group, blur_group }
    }
}

/// `m`, or a vertex-clustered copy whose vertex and index buffers each fit
/// in `max_bytes`. Each grid cell keeps its first vertex; triangles that
/// collapse are dropped. Halves the grid until it fits.
fn fit_buffer(m: Arc<Mesh>, max_bytes: u64) -> Arc<Mesh> {
    let fits = |m: &Mesh| {
        let v = (m.vertices.len() * std::mem::size_of::<Vertex>()) as u64;
        let i = (m.indices.len().max(m.edges.len()) * 4) as u64;
        v <= max_bytes && i <= max_bytes
    };
    if fits(&m) || m.vertices.is_empty() {
        return m;
    }
    let (lo, hi) = m.vertices.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), v| (a.min(v.pos.into()), b.max(v.pos.into())));
    let extent = (hi - lo).max(Vec3::splat(1e-6));
    // Start near the vertex count the budget allows (with a safety margin).
    let budget = (max_bytes / std::mem::size_of::<Vertex>() as u64) as f32 * 0.5;
    let mut cells = (budget / (extent.x * extent.y * extent.z / extent.max_element().powi(3))).cbrt().clamp(8.0, 65536.0);
    loop {
        let step = extent.max_element() / cells;
        let mut slot: HashMap<(i32, i32, i32), u32> = HashMap::new();
        let mut vertices = Vec::new();
        let remap: Vec<u32> = m
            .vertices
            .iter()
            .map(|v| {
                let q = ((Vec3::from(v.pos) - lo) / step).floor();
                *slot.entry((q.x as i32, q.y as i32, q.z as i32)).or_insert_with(|| {
                    vertices.push(*v);
                    vertices.len() as u32 - 1
                })
            })
            .collect();
        let indices: Vec<u32> = m
            .indices
            .chunks_exact(3)
            .map(|t| [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]])
            .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
            .flatten()
            .collect();
        let edges: Vec<u32> = m.edges.chunks_exact(2).map(|e| [remap[e[0] as usize], remap[e[1] as usize]]).filter(|e| e[0] != e[1]).flatten().collect();
        let lighter = Mesh {
            vertices,
            indices,
            edges,
            materials: m.materials.clone(),
            textures: m.textures.clone(),
            imported: m.imported.clone(),
            decals: m.decals.clone(),
            face: m.face.clone(), unshaded: None,
        };
        if fits(&lighter) || cells <= 8.0 {
            return Arc::new(lighter);
        }
        cells /= 2.0;
    }
}

/// Uploads baked textures with their mip chains.
fn texture_views(device: &wgpu::Device, queue: &wgpu::Queue, t: &Textures) -> [wgpu::TextureView; 3] {
    let upload = |data: &[u8], format: wgpu::TextureFormat| {
        let levels = 32 - t.size.max(1).leading_zeros();
        let size = wgpu::Extent3d { width: t.size, height: t.size, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("baked"),
            size,
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut level = data.to_vec();
        let mut side = t.size;
        for mip in 0..levels {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &level,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
                wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
            );
            if side > 1 {
                level = half(&level, side as usize);
                side /= 2;
            }
        }
        texture.create_view(&Default::default())
    };
    let color = upload(&t.color, wgpu::TextureFormat::Rgba8UnormSrgb);
    let normal = upload(&t.normal, wgpu::TextureFormat::Rgba8Unorm);
    let orm = upload(&t.orm, wgpu::TextureFormat::Rgba8Unorm);
    [color, normal, orm]
}

/// A mesh's group 1: baked textures (or placeholders) and its materials'
/// detail settings.
fn texture_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, images: &[wgpu::TextureView; 3], details: &[[[f32; 4]; 2]]) -> wgpu::BindGroup {
    let mut packed = vec![[0.0f32; 4]; 128];
    for (i, d) in details.iter().take(64).enumerate() {
        packed[2 * i] = d[0];
        packed[2 * i + 1] = d[1];
    }
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("details"), contents: bytemuck::cast_slice(&packed), usage: wgpu::BufferUsages::UNIFORM });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("baked textures"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&images[0]) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&images[1]) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&images[2]) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: 4, resource: buffer.as_entire_binding() },
        ],
    })
}

/// Side of a library detail layer in the viewport.
const DETAIL_SIDE: u32 = 512;

/// The library's detail layers, all neutral until `load_detail` fills one.
fn detail_textures(device: &wgpu::Device, queue: &wgpu::Queue, layout: &wgpu::BindGroupLayout) -> (wgpu::Texture, wgpu::BindGroup) {
    let layers = crate::detail::LIBRARY.len() as u32;
    let levels = 32 - DETAIL_SIDE.leading_zeros();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("detail library"),
        size: wgpu::Extent3d { width: DETAIL_SIDE, height: DETAIL_SIDE, depth_or_array_layers: layers },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for mip in 0..levels {
        let side = DETAIL_SIDE >> mip;
        let neutral = vec![128u8; (side * side * 4 * layers) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: mip, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &neutral,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
            wgpu::Extent3d { width: side, height: side, depth_or_array_layers: layers },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("detail"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        anisotropy_clamp: 8,
        ..Default::default()
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("detail library"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });
    (texture, group)
}

/// Decodes library map `layer` (once) and uploads it with its mips.
fn load_detail(queue: &wgpu::Queue, res: &mut Resources, layer: usize) {
    if res.detail_loaded.get(layer).is_none_or(|&done| done) {
        return;
    }
    res.detail_loaded[layer] = true;
    let Some(map) = crate::detail::map(crate::detail::LIBRARY[layer].name) else { return };
    let mut level = map.viewport_rgba(DETAIL_SIDE as usize);
    let mut side = (level.len() / 4).isqrt() as u32;
    let mut mip = DETAIL_SIDE.trailing_zeros() - side.trailing_zeros();
    loop {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &res.detail_texture, mip_level: mip, origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 }, aspect: wgpu::TextureAspect::All },
            &level,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
            wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        );
        if side == 1 {
            break;
        }
        level = half(&level, side as usize);
        side /= 2;
        mip += 1;
    }
}

/// The next mip level down: each texel averages a 2x2 block.
fn half(data: &[u8], side: usize) -> Vec<u8> {
    let h = side / 2;
    let mut out = vec![0u8; h * h * 4];
    for y in 0..h {
        for x in 0..h {
            for c in 0..4 {
                let at = |dx: usize, dy: usize| data[((2 * y + dy) * side + 2 * x + dx) * 4 + c] as u32;
                out[(y * h + x) * 4 + c] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    out
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (capacity * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Maya-style ground grid on XZ: 24x24 units, dark center axes.
fn grid_lines() -> Vec<LineVertex> {
    let half = 12;
    let mut v = Vec::new();
    for i in -half..=half {
        let f = i as f32;
        let h = half as f32;
        let color = if i == 0 { [0.16, 0.16, 0.16, 1.0] } else { [0.52, 0.52, 0.52, 0.85] };
        for (a, b) in [(Vec3::new(f, 0.0, -h), Vec3::new(f, 0.0, h)), (Vec3::new(-h, 0.0, f), Vec3::new(h, 0.0, f))] {
            v.push(LineVertex { pos: a.into(), color });
            v.push(LineVertex { pos: b.into(), color });
        }
    }
    v
}

pub struct ViewportCallback {
    /// Which buffer slot this pane uses, 0..SLOTS.
    pub slot: usize,
    pub view_proj: Mat4,
    pub grid_model: Mat4,
    pub cam_pos: Vec3,
    /// Shaded objects. Must be grouped by mesh so each mesh is one draw call.
    pub fills: Vec<(MeshKey, Instance)>,
    /// Wireframes (color = line color), grouped by mesh like `fills`.
    pub wires: Vec<(MeshKey, Instance)>,
    /// Every per-object (clay) mesh in the scene; uploaded when new or
    /// changed, and GPU copies of meshes not listed are dropped.
    pub dynamic: Vec<(MeshKey, Arc<Mesh>)>,
    pub show_grid: bool,
    /// Unlit fills in the instance color (for silhouette masks).
    pub flat: bool,
    /// Lights from objects: (position, radius, linear power); the studio
    /// light dims when there are any.
    pub lights: Vec<(Vec3, f32, Vec3)>,
    pub shadows: Shadows,
    /// What lights the fills: render_film's environment and lights (or the
    /// classic studio).
    pub lighting: Arc<ViewLighting>,
    pub ao: Ao,
    /// Where shadows matter: (camera target, camera distance). The map
    /// covers the casters near it rather than the whole scene, so a large
    /// scene keeps detail; None covers everything.
    pub shadow_focus: Option<(Vec3, f32)>,
    /// HDR finishing (bloom, exposure, tonemap). Masks (`flat`) and
    /// `enabled: false` draw straight into the pane instead.
    pub post: post::ViewLook,
    /// A window pane's rect in points, as given to egui: `prepare` finds
    /// the pixels egui's viewport keeps (it is clamped to the window) and
    /// fits the view and the HDR targets to them. None for `render_image`.
    pub pane: Option<egui::Rect>,
    /// The window frame (`cumulative_frame_nr`), to give back the HDR
    /// targets of panes that stopped drawing.
    pub frame: u64,
    /// What the pane shows behind the picture, at its top and bottom (a
    /// vertical gradient); `render_image` uses its clear colour.
    pub background: [egui::Color32; 2],
    /// Every chunk of the clays in a sculpt session (`mesh::SculptChunk`),
    /// kept uploaded in the chunk pool (only new or changed ones go up);
    /// fills name them by `MeshKey::Chunk`.
    pub chunks: Vec<(MeshKey, Arc<SculptChunk>)>,
    /// MatCap shading for every fill (`set_display shading: matcap`).
    pub matcap: Option<Arc<matcap::Matcap>>,
    /// Keeps the shadow map as it is while a sculpt drag runs (refreshed
    /// on release).
    pub freeze_shadows: bool,
    /// The brush tools' ring on the surface under the mouse.
    pub cursor: Option<Cursor>,
}

/// Fits a pane to the viewport egui gives its callback. That viewport is the
/// pane's rect in pixels, clamped to the window, so a pane partly off
/// screen gets a smaller one; the returned matrix (applied after the
/// projection) keeps the picture where the full pane would put it rather
/// than squashing it in. Also returns the viewport's size; None when it is
/// empty.
fn crop(rect: egui::Rect, pixels_per_point: f32, screen: [u32; 2]) -> Option<(Mat4, [u32; 2])> {
    let kept = egui::epaint::ViewportInPixels::from_points(&rect, pixels_per_point, screen);
    if kept.width_px <= 0 || kept.height_px <= 0 {
        return None;
    }
    // The same rounding as egui, without the clamp.
    let px = |v: f32| (pixels_per_point * v).round();
    let (l, t, r, b) = (px(rect.min.x), px(rect.min.y), px(rect.max.x), px(rect.max.y));
    let (kl, kt, kw, kh) = (kept.left_px as f32, kept.top_px as f32, kept.width_px as f32, kept.height_px as f32);
    let (sx, sy) = ((r - l) / kw, (b - t) / kh);
    let tx = (2.0 * (l - kl) + (r - l)) / kw - 1.0;
    let ty = 1.0 - (2.0 * (t - kt) + (b - t)) / kh;
    let m = Mat4::from_cols(glam::Vec4::new(sx, 0.0, 0.0, 0.0), glam::Vec4::new(0.0, sy, 0.0, 0.0), glam::Vec4::Z, glam::Vec4::new(tx, ty, 0.0, 1.0));
    Some((m, [kept.width_px as u32, kept.height_px as u32]))
}

/// Where the key light's shadow map sits.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowFit {
    light_vp: Mat4,
    /// One texel in world units.
    texel: f32,
    /// World distance from the map's near plane to its far plane.
    depth_range: f32,
    /// The catcher quad on y = 0 (min x, min z, max x, max z).
    ground: Option<[f32; 4]>,
}

/// `x` rounded up to a power of sqrt 2, so small changes keep the size.
fn quantise(x: f32) -> f32 {
    let n = (x.max(1e-3).ln() / std::f32::consts::SQRT_2.ln()).ceil() as i32;
    std::f32::consts::SQRT_2.powi(n)
}

/// Fits an orthographic shadow map of `res` texels along `key` (toward the
/// light) around the world boxes of the casters. The light's orientation is
/// fixed and the map's extent quantised and its origin snapped to whole
/// texels, so orbiting never changes it and panning never makes edges
/// crawl. `focus` (target, camera distance) limits its width to the casters
/// near the target; depth always spans every caster, so tall things outside
/// still shadow it. The map also covers the catcher on y = 0 when nothing
/// reaches below it, whether the catcher is drawn or not, so turning it on
/// leaves the other shadows as they were.
fn fit_shadow(boxes: &[(Vec3, Vec3)], key: Vec3, focus: Option<(Vec3, f32)>, res: u32) -> Option<ShadowFit> {
    let key = key.try_normalize().filter(|k| k.y > 0.05)?;
    let (lo, hi) = boxes.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), (l, h)| (a.min(*l), b.max(*h)));
    if lo.cmpgt(hi).any() {
        return None;
    }
    let corners = |lo: Vec3, hi: Vec3| (0..8).map(move |i| Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][i >> 2]));
    // Shadows fall along -key: where the box's corners land on y = 0, with
    // a margin the catcher fades out over.
    let ground = (lo.y > -0.05).then(|| {
        let (a, b) = corners(lo, hi).map(|p| p - key * (p.y.max(0.0) / key.y)).chain(corners(lo, hi).map(|p| p.with_y(0.0))).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)));
        let pad = (b - a) * 0.35 + 0.2;
        [a.x - pad.x, a.z - pad.z, b.x + pad.x, b.z + pad.z]
    });
    let (mut wlo, mut whi) = (lo, hi);
    if let Some(g) = ground {
        wlo = wlo.min(Vec3::new(g[0], 0.0, g[1]));
        whi = whi.max(Vec3::new(g[2], 0.0, g[3]));
    }
    if let Some((target, distance)) = focus {
        let r = quantise((distance * 4.0).max(2.0));
        wlo = wlo.max(target - r);
        whi = whi.min(target + r);
        if wlo.cmpgt(whi).any() {
            return None;
        }
    }
    let up = if key.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(Vec3::ZERO, -key, up);
    let span = |pts: &mut dyn Iterator<Item = Vec3>| pts.map(|p| view.transform_point3(p)).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(p), b.max(p)));
    let (xlo, xhi) = span(&mut corners(wlo, whi));
    // Depth from every caster and the ground, focus or not.
    let (zlo, zhi) = span(&mut corners(lo.min(wlo), hi.max(whi)));
    let side = quantise((xhi.x - xlo.x).max(xhi.y - xlo.y) * (1.0 + 4.0 / res as f32));
    let texel = side / res as f32;
    let snap = |lo: f32, hi: f32| (((lo + hi) * 0.5 - side * 0.5) / texel).floor() * texel;
    let (x0, y0) = (snap(xlo.x, xhi.x), snap(xlo.y, xhi.y));
    // View space looks down -z; pad depth so nothing touches the planes.
    let pad = (zhi.z - zlo.z) * 0.01 + 0.01;
    let (near, far) = (-zhi.z - pad, -zlo.z + pad);
    let light_vp = Mat4::orthographic_rh(x0, x0 + side, y0, y0 + side, near, far) * view;
    Some(ShadowFit { light_vp, texel, depth_range: far - near, ground })
}

/// Calls `draw(mesh, instance_range)` once per run of equal meshes.
fn for_each_group(items: &[(MeshKey, Instance)], base: usize, mut draw: impl FnMut(MeshKey, std::ops::Range<u32>)) {
    let mut start = 0;
    while start < items.len() {
        let key = items[start].0;
        let end = start + items[start..].iter().take_while(|(k, _)| *k == key).count();
        draw(key, (base + start) as u32..(base + end) as u32);
        start = end;
    }
}

impl ViewportCallback {
    /// Uploads meshes, globals and instances, and records the shadow pass
    /// (when the map is stale) and the ambient occlusion passes on
    /// `encoder`, before the main pass. Returns whether it recorded any;
    /// the shadow map's key already claims its pass, so the caller must
    /// submit `encoder`. `view_proj` is the callback's, fitted to the
    /// viewport of `size` pixels (see `crop`).
    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, view_proj: Mat4, size: [u32; 2], encoder: &mut wgpu::CommandEncoder, res: &mut Resources) -> bool {
        let listed: HashSet<MeshKey> = self.dynamic.iter().map(|(k, _)| *k).collect();
        res.meshes.retain(|k, _| matches!(k, MeshKey::Prim(_)) || listed.contains(k));
        for (key, mesh) in &self.dynamic {
            if !res.meshes.get(key).is_some_and(|g| Arc::ptr_eq(&g.source, mesh)) {
                // A posed copy shares its rest mesh's textures: reuse the upload.
                let reuse = res
                    .meshes
                    .remove(key)
                    .filter(|g| matches!((&g.source.textures, &mesh.textures), (Some(a), Some(b)) if Arc::ptr_eq(a, b)))
                    .and_then(|g| g.images);
                res.mesh_generation += 1;
                res.meshes.insert(*key, gpu_mesh(device, queue, &res.texture_layout, &res.sampler, &res.blank, mesh.clone(), reuse, res.mesh_generation));
                for m in &mesh.materials {
                    if let Some((layer, _, _)) = crate::detail::layout(m) {
                        load_detail(queue, res, layer);
                    }
                }
            }
        }
        for (_, i) in &self.fills {
            if i.detail[0] > 0.5 {
                load_detail(queue, res, i.detail[0] as usize - 1);
            }
        }
        // Sculpt chunks: only new or changed ones go up (by pointer).
        res.pool.sync(device, queue, &self.chunks);
        let want = self.matcap.as_ref().map_or(0, |m| m.id);
        if res.matcap.0 != want {
            res.matcap = (want, matcap_view(device, queue, self.matcap.as_deref()));
        }

        // One triangle count gates both extra passes.
        let triangles: u64 = self.fills.iter().filter_map(|(k, _)| res.index_count(k)).map(|n| n as u64 / 3).sum();
        let key = self.key_light();
        let (fit, status) = self.fit_shadow(device, res, triangles, key);
        res.shadow_map_mut(self.slot).status = status;
        let ao = self.ao_status(triangles, size);
        *res.ao_status_mut(self.slot) = ao;
        let ao_size = [size[0].div_ceil(2).max(1), size[1].div_ceil(2).max(1)];
        if !ao.active {
            res.slots[self.slot].ao = None;
        } else if res.slots[self.slot].ao.as_ref().is_none_or(|t| t.size != ao_size) {
            res.slots[self.slot].ao = Some(AoTargets::new(device, &res.ao_layout, ao_size));
        }
        let (env_id, env_view) = res.environment(device, queue, &self.lighting);
        let map_size = res.shadow_map(self.slot).size as f32;
        let sh = &self.shadows;
        let key = key.unwrap_or(sh.key_dir);
        let gamma_encode = res.gamma_encode;
        let l = &*self.lighting;
        // The studio dims when objects carry lights (as before).
        let studio_k = if l.studio && !self.lights.is_empty() { 0.3 } else { 1.0 };
        let lights: Vec<(Vec3, f32, Vec3)> = self.lights.iter().chain(&l.locals).take(MAX_VIEW_LIGHTS).copied().collect();
        let mut sun_dir = [[0.0; 4]; view_light::MAX_SUNS];
        let mut sun_col = [[0.0; 4]; view_light::MAX_SUNS];
        for (i, (d, e)) in l.suns.iter().enumerate().take(view_light::MAX_SUNS) {
            // The first sun is the shadowed key (which shadows can move).
            let d = if i == 0 { key } else { *d };
            sun_dir[i] = [d.x, d.y, d.z, 0.0];
            sun_col[i] = [e.x, e.y, e.z, 0.0];
        }
        let slot = &mut res.slots[self.slot];
        let globals = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            grid_model: self.grid_model.to_cols_array_2d(),
            cam_pos: self.cam_pos.extend(1.0).into(),
            params: [if gamma_encode { 1.0 } else { 0.0 }, if self.flat { 1.0 } else { 0.0 }, lights.len() as f32, studio_k],
            lights: {
                let mut out = [[0.0; 4]; MAX_VIEW_LIGHTS * 2];
                for (i, (p, r, c)) in lights.iter().enumerate() {
                    out[i * 2] = [p.x, p.y, p.z, *r];
                    out[i * 2 + 1] = [c.x, c.y, c.z, 0.0];
                }
                out
            },
            light_vp: fit.map_or(Mat4::IDENTITY, |f| f.light_vp).to_cols_array_2d(),
            key_dir: [key.x, key.y, key.z, sh.strength.clamp(0.0, 1.0)],
            shadow: match fit {
                // A fixed soft edge of a few texels at the normal size.
                Some(_) => [1.0, 1.0 / map_size, 1.5 * map_size / 1024.0, if sh.soft { 1.0 } else { 0.0 }],
                None => [0.0; 4],
            },
            shadow2: match fit {
                Some(f) => [f.texel * 1.5, f.depth_range, sh.softness.clamp(0.0, 0.5).tan(), f.texel],
                None => [0.0; 4],
            },
            ground: fit.and_then(|f| f.ground).filter(|_| sh.ground).unwrap_or([0.0; 4]),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            sh: l.sh.map(|c| [c.x, c.y, c.z, 0.0]),
            sun_dir,
            sun_col,
            env: [if l.probe.is_some() { 1.0 } else { 0.0 }, l.suns.len().min(view_light::MAX_SUNS) as f32, l.probe.as_ref().map_or(0.0, |p| p.max_lod()), l.probe.as_ref().map_or(0.0, |p| p.rotate)],
            env2: [if l.studio { view_light::CATCHLIGHT } else { 0.0 }, studio_k, if self.matcap.is_some() { 1.0 } else { 0.0 }, 0.0],
            ao: if ao.active { [1.0, self.ao.radius, self.ao.strength, 0.0] } else { [0.0; 4] },
            ao_size: [ao_size[0] as f32, ao_size[1] as f32, 1.0 / ao_size[0] as f32, 1.0 / ao_size[1] as f32],
        };
        queue.write_buffer(&slot.globals, 0, bytemuck::bytes_of(&globals));

        // One buffer: fills first, then wires.
        let instances: Vec<Instance> = self.fills.iter().chain(&self.wires).map(|(_, i)| *i).collect();
        if instances.len() > slot.instance_capacity {
            slot.instance_capacity = instances.len().next_power_of_two();
            slot.instances = instance_buffer(device, slot.instance_capacity);
        }
        if !instances.is_empty() {
            queue.write_buffer(&slot.instances, 0, bytemuck::cast_slice(&instances));
        }
        // The brush cursor, through the line shader's grid model.
        slot.cursor_count = 0;
        if let Some(c) = &self.cursor {
            let to_grid = self.grid_model.inverse();
            let lines: Vec<LineVertex> = c.lines().into_iter().map(|l| LineVertex { pos: to_grid.transform_point3(Vec3::from(l.pos)).into(), ..l }).collect();
            if lines.iter().all(|l| l.pos.iter().all(|x| x.is_finite())) {
                queue.write_buffer(&slot.cursor, 0, bytemuck::cast_slice(&lines));
                slot.cursor_count = lines.len() as u32;
            }
        }

        // Group 3: this slot's shadow map, environment and occlusion.
        let ao_view = slot.ao.as_ref().map(|t| (t.id, t.blurred.clone()));
        let map = res.shadow_map(self.slot);
        let ids = [map.id, env_id, ao_view.as_ref().map_or(0, |(id, _)| *id), res.matcap.0];
        if res.slots[self.slot].lighting.as_ref().is_none_or(|(k, _)| *k != ids) {
            let ao_view = ao_view.map_or_else(|| res.white_ao.clone(), |(_, v)| v);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("lighting"),
                layout: &res.lighting_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: lighting_bindings::SHADOW_MAP, resource: wgpu::BindingResource::TextureView(&map.view) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::SHADOW_COMPARE, resource: wgpu::BindingResource::Sampler(&res.shadow_compare) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::SHADOW_POINT, resource: wgpu::BindingResource::Sampler(&res.shadow_point) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::ENV_MAP, resource: wgpu::BindingResource::TextureView(&env_view) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::ENV_SAMPLER, resource: wgpu::BindingResource::Sampler(&res.env_sampler) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::BRDF, resource: wgpu::BindingResource::TextureView(&res.brdf) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::CLAMP, resource: wgpu::BindingResource::Sampler(&res.clamp_sampler) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::AO, resource: wgpu::BindingResource::TextureView(&ao_view) },
                    wgpu::BindGroupEntry { binding: lighting_bindings::MATCAP, resource: wgpu::BindingResource::TextureView(&res.matcap.1) },
                ],
            });
            res.slots[self.slot].lighting = Some((ids, group));
        }
        let shadowed = fit.is_some_and(|fit| self.shadow_pass(encoder, res, &fit));
        if ao.active {
            self.ao_passes(encoder, res);
        }
        shadowed || ao.active
    }

    /// Toward the light that casts shadows: the shadow settings' key for
    /// the studio (and classic) lighting, else the lighting's brightest
    /// sun; None when it has no sun.
    fn key_light(&self) -> Option<Vec3> {
        let l = &self.lighting;
        if l.studio || l.probe.is_none() { Some(self.shadows.key_dir) } else { l.suns.first().map(|(d, _)| *d) }
    }

    /// Whether this frame draws ambient occlusion, and why not.
    fn ao_status(&self, triangles: u64, size: [u32; 2]) -> AoStatus {
        let off = |reason| AoStatus { active: false, reason: Some(reason) };
        if self.flat {
            off("mask render")
        } else if !self.ao.enabled || self.ao.strength <= 0.0 {
            off("turned off")
        } else if self.lighting.probe.is_none() {
            off("classic lighting has no ambient occlusion")
        } else if size[0] == 0 || size[1] == 0 || self.fills.is_empty() {
            off("nothing to draw")
        } else if !extra_passes_fit(triangles) {
            off("over the triangle budget")
        } else {
            AoStatus { active: true, reason: None }
        }
    }

    /// Records the occlusion pre-pass, the occlusion and its blur.
    fn ao_passes(&self, encoder: &mut wgpu::CommandEncoder, res: &Resources) {
        let slot = &res.slots[self.slot];
        let Some(t) = &slot.ao else { return };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ao pre-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &t.normal, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_bind_group(0, &slot.bind_group, &[]);
            pass.set_vertex_buffer(1, slot.instances.slice(..));
            self.draw_meshes(&mut pass, res, &res.prepass_pipeline, &res.sculpt_prepass_pipeline, true);
        }
        for (label, pipeline, group, target) in [("ao", &res.ao_pipeline, &t.ao_group, &t.raw), ("ao blur", &res.blur_pipeline, &t.blur_group, &t.blurred)] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &slot.bind_group, &[]);
            pass.set_bind_group(1, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// The shadow map's fit for this frame (None draws without shadows),
    /// and why there is none. Resizes the map when the quality changed.
    fn fit_shadow(&self, device: &wgpu::Device, res: &mut Resources, triangles: u64, key: Option<Vec3>) -> (Option<ShadowFit>, ShadowStatus) {
        let sh = &self.shadows;
        let off = |reason| (None, ShadowStatus { active: false, triangles: 0, reason: Some(reason) });
        if self.flat {
            return off("mask render");
        }
        if !sh.enabled || sh.strength <= 0.0 {
            return off("turned off");
        }
        let Some(key) = key else { return off("the lighting has no sun") };
        if !extra_passes_fit(triangles) {
            return (None, ShadowStatus { active: false, triangles, reason: Some("over the shadow triangle budget") });
        }
        // World boxes of the casters (not glass, not bright emitters).
        let boxes: Vec<(Vec3, Vec3)> = self
            .fills
            .iter()
            .filter(|(_, i)| i.extra[1] <= 0.3 && i.extra[0] <= 1.5)
            .filter_map(|(k, i)| {
                let (lo, hi) = match k {
                    MeshKey::Chunk(..) => res.pool.get(k)?.bounds,
                    _ => res.meshes.get(k)?.bounds?,
                };
                let m = Mat4::from_cols_array_2d(&i.model);
                let (c, e) = (m.transform_point3((lo + hi) * 0.5), (hi - lo) * 0.5);
                let e = m.x_axis.truncate().abs() * e.x + m.y_axis.truncate().abs() * e.y + m.z_axis.truncate().abs() * e.z;
                Some((c - e, c + e))
            })
            .collect();
        let size = sh.quality.size().min(device.limits().max_texture_dimension_2d);
        if res.shadow_map(self.slot).size != size {
            *res.shadow_map_mut(self.slot) = shadow_map(device, size);
        }
        match fit_shadow(&boxes, key, self.shadow_focus, size) {
            Some(fit) => (Some(fit), ShadowStatus { active: true, triangles, reason: None }),
            None => (None, ShadowStatus { active: false, triangles, reason: Some("nothing casts a shadow") }),
        }
    }

    /// Renders the key's shadow map unless it already holds this light fit,
    /// these casters and these mesh uploads. Returns whether it recorded.
    fn shadow_pass(&self, encoder: &mut wgpu::CommandEncoder, res: &mut Resources, fit: &ShadowFit) -> bool {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        fit.light_vp.to_cols_array().map(f32::to_bits).hash(&mut h);
        for (k, i) in &self.fills {
            k.hash(&mut h);
            match k {
                MeshKey::Chunk(..) => res.pool.get(k).map(|e| e.source.generation).hash(&mut h),
                _ => res.meshes.get(k).map(|m| m.generation).hash(&mut h),
            }
            bytemuck::bytes_of(&i.model).hash(&mut h);
            bytemuck::bytes_of(&i.extra).hash(&mut h);
        }
        let key = h.finish();
        let map = res.shadow_map_mut(self.slot);
        if map.key == Some(key) && std::env::var_os("VIEW_SHADOW_ALWAYS").is_none() {
            return false;
        }
        // A sculpt drag keeps the map it started with.
        if self.freeze_shadows && map.key.is_some() {
            return false;
        }
        map.key = Some(key);
        let res = &*res;
        let map = res.shadow_map(self.slot);
        let slot = &res.slots[self.slot];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &map.view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &slot.bind_group, &[]);
        pass.set_vertex_buffer(1, slot.instances.slice(..));
        self.draw_meshes(&mut pass, res, &res.shadow_pipeline, &res.sculpt_shadow_pipeline, false);
        true
    }

    /// Draws every fill: regular meshes with `regular`, sculpt chunks
    /// (sorted last) with `sculpt`, each page of the chunk pool bound once
    /// and each chunk drawn from its slot. `textures` binds group 1.
    fn draw_meshes(&self, pass: &mut wgpu::RenderPass<'_>, res: &Resources, regular: &wgpu::RenderPipeline, sculpt: &wgpu::RenderPipeline, textures: bool) {
        pass.set_pipeline(regular);
        let (mut on_sculpt, mut page) = (false, usize::MAX);
        for_each_group(&self.fills, 0, |key, range| {
            if let MeshKey::Chunk(..) = key {
                let Some(e) = res.pool.get(&key).filter(|e| e.index_count > 0) else { return };
                if !on_sculpt {
                    pass.set_pipeline(sculpt);
                    if textures {
                        pass.set_bind_group(1, &res.no_textures, &[]);
                    }
                    on_sculpt = true;
                }
                if page != e.slot.page {
                    let b = res.pool.buffer(e.slot.page);
                    pass.set_vertex_buffer(0, b.slice(..));
                    pass.set_index_buffer(b.slice(..), wgpu::IndexFormat::Uint32);
                    page = e.slot.page;
                }
                pass.draw_indexed(e.first_index..e.first_index + e.index_count, e.base_vertex, range);
                return;
            }
            let Some(mesh) = res.meshes.get(&key).filter(|m| m.index_count > 0) else { return };
            if on_sculpt {
                pass.set_pipeline(regular);
                (on_sculpt, page) = (false, usize::MAX);
            }
            if textures {
                pass.set_bind_group(1, mesh.textures.as_ref().unwrap_or(&res.no_textures), &[]);
            }
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, range);
        });
    }

    /// Whether this callback draws through render::post's HDR passes.
    fn finishes(&self) -> bool {
        self.post.enabled && !self.flat
    }

    /// The direct path: everything straight into the pane's pass.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, res: &Resources) {
        self.draw_fills(pass, res, false);
        self.draw_overlay(pass, res);
    }

    /// The shaded fills and the shadow catcher, into the pane (`hdr` false)
    /// or into render::post's scene target.
    fn draw_fills(&self, pass: &mut wgpu::RenderPass<'_>, res: &Resources, hdr: bool) {
        let slot = &res.slots[self.slot];
        pass.set_bind_group(0, &slot.bind_group, &[]);

        pass.set_pipeline(if hdr { &res.mesh_hdr_pipeline } else { &res.mesh_pipeline });
        pass.set_bind_group(2, &res.detail_group, &[]);
        if let Some((_, group)) = &slot.lighting {
            pass.set_bind_group(3, group, &[]);
        }
        pass.set_vertex_buffer(1, slot.instances.slice(..));
        let (regular, sculpt) = if hdr { (&res.mesh_hdr_pipeline, &res.sculpt_hdr_pipeline) } else { (&res.mesh_pipeline, &res.sculpt_pipeline) };
        self.draw_meshes(pass, res, regular, sculpt, true);

        // The shadow catcher, before the grid so grid lines stay on top.
        if !self.flat && self.shadows.enabled && self.shadows.ground {
            pass.set_pipeline(if hdr { &res.ground_hdr_pipeline } else { &res.ground_pipeline });
            pass.set_bind_group(1, &res.no_textures, &[]);
            pass.draw(0..4, 0..1);
        }
    }

    /// The grid and wireframes, in the pane's colours (never tonemapped),
    /// against the fills' depth.
    fn draw_overlay(&self, pass: &mut wgpu::RenderPass<'_>, res: &Resources) {
        let slot = &res.slots[self.slot];
        pass.set_bind_group(0, &slot.bind_group, &[]);

        // Grid after fills so it blends over the background but hides behind objects.
        if self.show_grid {
            pass.set_pipeline(&res.line_pipeline);
            pass.set_vertex_buffer(0, res.grid.slice(..));
            pass.draw(0..res.grid_count, 0..1);
        }

        pass.set_pipeline(&res.wire_pipeline);
        pass.set_vertex_buffer(1, slot.instances.slice(..));
        for_each_group(&self.wires, self.fills.len(), |key, range| {
            let Some(mesh) = res.meshes.get(&key).filter(|m| m.edge_count > 0) else { return };
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.edges.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.edge_count, 0, range);
        });

        // The brush ring, against the fills' depth.
        if self.cursor.is_some() && slot.cursor_count > 0 {
            pass.set_pipeline(&res.line_pipeline);
            pass.set_vertex_buffer(0, slot.cursor.slice(..));
            pass.draw(0..slot.cursor_count, 0..1);
        }
    }

    /// Records the HDR path before the pane's pass: the fills into the
    /// scene target (pass A), the grid and wires into the overlay against
    /// the fills' depth (pass B), then the bloom. `composite` finishes it.
    fn record_post(&self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, res: &Resources, t: &post::PostTargets) {
        t.write(queue, &self.post, res.gamma_encode, self.background);
        // The composite reads every sample of both, so they are kept.
        let color = |view, resolve_target| {
            Some(wgpu::RenderPassColorAttachment { view, depth_slice: None, resolve_target, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hdr scene"),
                color_attachments: &[color(&t.scene_ms, Some(&t.scene))],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth_ms,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.draw_fills(&mut pass, res, true);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[color(&t.overlay_ms, None)],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth_ms,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.draw_overlay(&mut pass, res);
        }
        t.bloom(encoder, &res.post, &self.post);
    }
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut Resources = resources.get_mut().expect("render::init was not called");
        let (view_proj, size) = match self.pane {
            Some(rect) => match crop(rect, screen.pixels_per_point, screen.size_in_pixels) {
                Some((fit, size)) => (fit * self.view_proj, size),
                // egui skips painting an empty viewport.
                None => return Vec::new(),
            },
            None => (self.view_proj, [0, 0]),
        };
        // The shadow and occlusion passes go on their own encoder, submitted
        // here: egui drops
        // its frame encoder (and the buffers returned here) unsubmitted when
        // the surface errors, and the map's key would then claim a pass that
        // never ran. The submit also flushes upload's queue writes first.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shadow") });
        if self.upload(device, queue, view_proj, size, &mut encoder, res) {
            queue.submit([encoder.finish()]);
        }
        if self.slot >= SLOTS {
            return Vec::new();
        }
        // Panes that have not drawn for a few frames (after leaving the
        // four view) give their targets back.
        res.post_seen[self.slot] = self.frame;
        for s in 0..SLOTS {
            if res.post_seen[s].saturating_add(2) < self.frame {
                res.post_targets[s] = None;
                res.slots[s].ao = None;
                res.slots[s].lighting = None;
            }
        }
        if !self.finishes() || size[0] == 0 || size[1] == 0 {
            res.post_targets[self.slot] = None;
            return Vec::new();
        }
        if res.post_targets[self.slot].as_ref().is_none_or(|t| t.size != size) {
            res.post_targets[self.slot] = Some(post::PostTargets::new(device, &res.post, res.target_format, DEPTH_FORMAT, MSAA, size));
        }
        // The HDR passes go on egui's encoder, before its render pass: if
        // that encoder is dropped, nothing remembers them.
        let res = &*res;
        if let Some(t) = &res.post_targets[self.slot] {
            self.record_post(queue, egui_encoder, res, t);
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &Resources = resources.get().expect("render::init was not called");
        let viewport = info.viewport_in_pixels();
        let size = [viewport.width_px.max(0) as u32, viewport.height_px.max(0) as u32];
        match res.post_targets.get(self.slot).and_then(Option::as_ref) {
            Some(t) if self.finishes() && t.size == size => t.composite(pass, &res.post),
            _ => self.draw(pass, res),
        }
    }
}

/// Draws `cb` (its `slot` is ignored) into a `width` x `height` image with a
/// flat sRGB background. Blocks until the GPU is done.
pub fn render_image(rs: &egui_wgpu::RenderState, cb: &ViewportCallback, width: u32, height: u32, clear: [u8; 3]) -> Result<egui::ColorImage, String> {
    let (width, height) = (width.clamp(16, 4096), height.clamp(16, 4096));
    let device = &rs.device;
    let format = rs.target_format;
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let texture = |label, format, samples, usage| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
    };
    let msaa = texture("offscreen msaa", format, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let resolved = texture("offscreen", format, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
    let depth = texture("offscreen depth", DEPTH_FORMAT, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());
    let (msaa_view, resolved_view, depth_view) = (view(&msaa), view(&resolved), view(&depth));

    // An sRGB target encodes on write, so give it the linear clear value.
    let c = clear.map(|v| v as f64 / 255.0);
    let c = if format.is_srgb() { c.map(|v| v.powf(2.2)) } else { c };
    let bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen readback"),
        size: (bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let cb = ViewportCallback {
        slot: OFFSCREEN_SLOT,
        view_proj: cb.view_proj,
        grid_model: cb.grid_model,
        cam_pos: cb.cam_pos,
        fills: cb.fills.clone(),
        wires: cb.wires.clone(),
        dynamic: cb.dynamic.clone(),
        show_grid: cb.show_grid,
        flat: cb.flat,
        lights: cb.lights.clone(),
        shadows: cb.shadows,
        lighting: cb.lighting.clone(),
        ao: cb.ao,
        shadow_focus: cb.shadow_focus,
        post: cb.post,
        pane: None,
        frame: 0,
        background: [egui::Color32::from_rgb(clear[0], clear[1], clear[2]); 2],
        chunks: cb.chunks.clone(),
        matcap: cb.matcap.clone(),
        freeze_shadows: cb.freeze_shadows,
        cursor: cb.cursor,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("offscreen") });
    {
        // Hold the renderer until the submit: upload's queue writes land at
        // the next submit from any thread, so another render_image sharing
        // OFFSCREEN_SLOT must not rewrite its globals or instances before
        // this draw is queued.
        let mut renderer = rs.renderer.write();
        let res: &mut Resources = renderer.callback_resources.get_mut().ok_or("renderer not ready")?;
        cb.upload(device, &rs.queue, cb.view_proj, [width, height], &mut encoder, res);
        let res = &*res;
        // Its own HDR targets, so screenshots never disturb the panes'.
        let targets = cb.finishes().then(|| post::PostTargets::new(device, &res.post, format, DEPTH_FORMAT, MSAA, [width, height]));
        if let Some(t) = &targets {
            cb.record_post(&rs.queue, &mut encoder, res, t);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0], g: c[1], b: c[2], a: 1.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            match &targets {
                Some(t) => t.composite(&mut pass, &res.post),
                None => cb.draw(&mut pass, res),
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &resolved, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(height) },
            },
            size,
        );
        rs.queue.submit([encoder.finish()]);
    }
    read_image(device, &readback, format, [width, height], bytes_per_row)
}

/// Waits for `readback` (a copied `size` image in `format`, rows
/// `bytes_per_row` apart) and returns its pixels.
fn read_image(device: &wgpu::Device, readback: &wgpu::Buffer, format: wgpu::TextureFormat, [width, height]: [u32; 2], bytes_per_row: u32) -> Result<egui::ColorImage, String> {
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let data = slice.get_mapped_range();
    let bgra = matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let row = &data[(y * bytes_per_row) as usize..][..(width * 4) as usize];
        for p in row.chunks_exact(4) {
            let (r, b) = if bgra { (p[2], p[0]) } else { (p[0], p[2]) };
            pixels.push(egui::Color32::from_rgb(r, p[1], b));
        }
    }
    drop(data);
    readback.unmap();
    Ok(egui::ColorImage::new([width as usize, height as usize], pixels))
}

/// Whether the window's last frame drew ambient occlusion (None before
/// the first).
pub fn ao_status(rs: &egui_wgpu::RenderState) -> Option<AoStatus> {
    rs.renderer.read().callback_resources.get::<Resources>().map(|r| r.window_ao)
}

/// Whether the window's last frame drew shadows (None before the first).
pub fn shadow_status(rs: &egui_wgpu::RenderState) -> Option<ShadowStatus> {
    rs.renderer.read().callback_resources.get::<Resources>().map(|r| r.window_shadow.status)
}

pub mod matcap;
mod pool;
pub mod post;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_big_meshes_are_thinned_to_fit() {
        let mut m = Mesh::default();
        let n = 200u32;
        for y in 0..=n {
            for x in 0..=n {
                let mut v: Vertex = bytemuck::Zeroable::zeroed();
                v.pos = [x as f32 / n as f32, y as f32 / n as f32, 0.0];
                m.vertices.push(v);
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = y * (n + 1) + x;
                m.indices.extend_from_slice(&[a, a + 1, a + n + 1, a + 1, a + n + 2, a + n + 1]);
            }
        }
        let m = Arc::new(m);
        assert!(Arc::ptr_eq(&fit_buffer(m.clone(), u64::MAX), &m));
        let max = 64 * 1024;
        let small = fit_buffer(m.clone(), max);
        assert!((small.vertices.len() * std::mem::size_of::<Vertex>()) as u64 <= max);
        assert!((small.indices.len() * 4) as u64 <= max);
        assert!(!small.indices.is_empty() && small.indices.iter().all(|&i| (i as usize) < small.vertices.len()));
    }

    /// Parses and validates WGSL as the default device would (no extra
    /// capabilities).
    fn validate(src: &str) -> wgpu::naga::Module {
        let module = wgpu::naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty()).validate(&module).unwrap_or_else(|e| panic!("{e:?}"));
        module
    }

    #[test]
    fn viewport_shader_is_valid_wgsl() {
        let module = validate(SHADER);
        for entry in ["vs_mesh", "fs_mesh", "fs_mesh_hdr", "vs_shadow", "fs_shadow", "vs_ground", "fs_ground", "vs_wire", "vs_line", "fs_line", "vs_prepass", "fs_prepass"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "no entry point {entry}");
        }
        // The uniform's layout as WGSL sees it is the Rust struct's.
        let globals = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("Globals")).expect("Globals").1;
        let wgpu::naga::TypeInner::Struct { span, .. } = &globals.inner else { panic!("Globals is not a struct") };
        assert_eq!(*span as usize, std::mem::size_of::<Globals>());
    }

    #[test]
    fn ao_shader_is_valid_wgsl() {
        let module = validate(AO_SHADER);
        for entry in ["vs_full", "fs_ao", "fs_blur"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "no entry point {entry}");
        }
        // Both shaders share the Globals text; it must still be the Rust struct.
        let globals = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("Globals")).expect("Globals").1;
        let wgpu::naga::TypeInner::Struct { span, .. } = &globals.inner else { panic!("Globals is not a struct") };
        assert_eq!(*span as usize, std::mem::size_of::<Globals>());
    }

    fn unit_box(at: Vec3) -> (Vec3, Vec3) {
        (at - 0.5, at + 0.5)
    }

    #[test]
    fn shadow_fit_is_stable_and_snapped() {
        let key = crate::film::KEY_DIR;
        let boxes = [unit_box(Vec3::new(0.0, 0.5, 0.0)), unit_box(Vec3::new(2.0, 0.5, -1.0)), (Vec3::new(-3.0, 0.0, -3.0), Vec3::new(3.0, 0.0, 3.0))];
        let fit = fit_shadow(&boxes, key, None, 2048).unwrap();
        // Every caster corner lands inside the map, in depth too.
        for (lo, hi) in boxes {
            for i in 0..8 {
                let p = Vec3::new([lo.x, hi.x][i & 1], [lo.y, hi.y][(i >> 1) & 1], [lo.z, hi.z][i >> 2]);
                let c = fit.light_vp.project_point3(p);
                assert!(c.x.abs() <= 1.0 && c.y.abs() <= 1.0 && (0.0..=1.0).contains(&c.z), "{p} -> {c}");
            }
        }
        // The map's extent is a power of sqrt 2 and its origin a whole
        // number of texels from the light's fixed axes.
        let side = fit.texel * 2048.0;
        let steps = side.ln() / std::f32::consts::SQRT_2.ln();
        assert!((steps - steps.round()).abs() < 1e-3, "side {side}");
        let origin = fit.light_vp.inverse().transform_point3(Vec3::new(-1.0, -1.0, 0.0));
        let view = Mat4::look_at_rh(Vec3::ZERO, -key.normalize(), Vec3::Y);
        let o = view.transform_point3(origin) / fit.texel;
        assert!((o.x - o.x.round()).abs() < 1e-2 && (o.y - o.y.round()).abs() < 1e-2, "origin {o} texels");

        // The camera's orientation is no input, and moving a caster less
        // than a texel keeps the matrix (no crawling edges).
        let mut nudged = boxes;
        nudged[0].0.y += fit.texel * 0.01;
        assert_eq!(fit_shadow(&nudged, key, None, 2048).unwrap().light_vp, fit.light_vp);
        // A focus whose reach covers the scene changes nothing.
        let focused = fit_shadow(&boxes, key, Some((Vec3::ZERO, 10.0)), 2048).unwrap();
        assert_eq!(focused.light_vp, fit.light_vp);
        // A close focus on a huge scene keeps the map small.
        let huge = [(Vec3::new(-500.0, 0.0, -500.0), Vec3::new(500.0, 20.0, 500.0))];
        let near = fit_shadow(&huge, key, Some((Vec3::ZERO, 1.0)), 2048).unwrap();
        assert!(near.texel * 2048.0 < 30.0, "focused map is {} wide", near.texel * 2048.0);

        // The catcher covers where the raised box's shadow lands on y = 0.
        let raised = [unit_box(Vec3::new(0.0, 2.0, 0.0))];
        let g = fit_shadow(&raised, key, None, 2048).unwrap().ground.unwrap();
        let k = key.normalize();
        let lands = Vec3::new(0.0, 2.0, 0.0) - k * (2.0 / k.y);
        assert!(g[0] < lands.x && lands.x < g[2] && g[1] < lands.z && lands.z < g[3], "{g:?} misses {lands}");
        // No catcher when something reaches below the grid.
        let sunk = [unit_box(Vec3::new(0.0, 0.0, 0.0))];
        assert!(fit_shadow(&sunk, key, None, 2048).unwrap().ground.is_none());
        // Nothing to cast, or a key from below the horizon: no shadows.
        assert!(fit_shadow(&[], key, None, 2048).is_none());
        assert!(fit_shadow(&boxes, Vec3::new(0.3, -0.5, 0.2), None, 2048).is_none());
    }

    #[test]
    fn crop_keeps_the_picture_where_the_whole_pane_puts_it() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 70.0));
        // On screen: nothing to fit.
        let (m, size) = crop(rect, 2.0, [400, 300]).unwrap();
        assert_eq!(size, [200, 100]);
        assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-6), "{m}");
        // Hanging off the right and bottom of a 160x110 px screen: egui's
        // viewport keeps [20, 160) x [40, 110). A point at full-pane NDC p
        // lands on pixel x = 20 + (p.x + 1) * 100, y = 40 + (1 - p.y) * 50;
        // in the kept viewport it must land on the same pixel.
        let (m, size) = crop(rect, 2.0, [160, 110]).unwrap();
        assert_eq!(size, [140, 70]);
        for p in [glam::Vec2::new(-1.0, 1.0), glam::Vec2::new(0.25, -0.5), glam::Vec2::new(0.4, 0.8)] {
            let q = m.project_point3(p.extend(0.5));
            let full = (20.0 + (p.x + 1.0) * 100.0, 40.0 + (1.0 - p.y) * 50.0);
            let kept = (20.0 + (q.x + 1.0) * 70.0, 40.0 + (1.0 - q.y) * 35.0);
            assert!((full.0 - kept.0).abs() < 1e-3 && (full.1 - kept.1).abs() < 1e-3, "{p}: {full:?} vs {kept:?}");
            assert!((q.z - 0.5).abs() < 1e-6);
        }
        // Wholly off screen: egui paints nothing.
        assert!(crop(egui::Rect::from_min_max(egui::pos2(200.0, 0.0), egui::pos2(300.0, 50.0)), 1.0, [160, 110]).is_none());
    }

    #[test]
    fn quantise_rounds_up_to_powers_of_root_two() {
        assert_eq!(quantise(1.0), 1.0);
        assert!((quantise(1.2) - std::f32::consts::SQRT_2).abs() < 1e-6);
        assert!((quantise(1.5) - 2.0).abs() < 1e-5);
        assert_eq!(quantise(1.3), quantise(1.4));
    }
}
