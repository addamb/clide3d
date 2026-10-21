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

use crate::mesh::{Mesh, MeshKey, Textures, Vertex};

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
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 4],
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
}

struct Slot {
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: usize,
}

/// Number of panes that can draw in one frame (Maya's four view).
pub const SLOTS: usize = 4;
/// Extra slot used by `render_image`.
const OFFSCREEN_SLOT: usize = SLOTS;

struct Resources {
    mesh_pipeline: wgpu::RenderPipeline,
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
    grid: wgpu::Buffer,
    grid_count: u32,
    gamma_encode: bool,
}

const SHADER: &str = r#"
struct Globals { view_proj: mat4x4<f32>, grid_model: mat4x4<f32>, cam_pos: vec4<f32>, params: vec4<f32>, lights: array<vec4<f32>, 32> };
@group(0) @binding(0) var<uniform> g: Globals;

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

const PI: f32 = 3.14159265;

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

@fragment
fn fs_mesh(in: MeshOut) -> @location(0) vec4<f32> {
    if (g.params.y > 0.5) { return vec4(encode(to_linear(in.color.rgb)), 1.0); }
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
    let coat = clamp(in.surface.z, 0.0, 1.0);
    let sheen = clamp(in.surface.w, 0.0, 1.0);
    let f0 = mix(vec3(0.04), albedo, metal);
    let nv = max(dot(n, v), 0.0);
    let fresnel = f0 + (max(vec3(1.0 - rough), f0) - f0) * pow(1.0 - nv, 5.0);
    let coat_f = 0.04 + 0.96 * pow(1.0 - nv, 5.0);

    let key = normalize(vec3(0.4, 0.8, 0.5));
    let fill = normalize(vec3(-0.6, 0.2, -0.4));
    // A weak light just above and right of the camera, like a photographer's
    // ring light: it puts a catchlight in every eye from any view.
    let to_cam = normalize(g.cam_pos.xyz - in.world_pos);
    let right = normalize(cross(vec3(0.0, 1.0, 0.0), to_cam) + vec3(1e-4, 0.0, 0.0));
    let front = normalize(to_cam + vec3(0.0, 0.3, 0.0) + right * 0.2);
    let lights = array<vec3<f32>, 3>(key, fill, front);
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
        c += (diffuse * (1.0 - coat * coat_f) + spec * (1.0 - coat * coat_f) + clear + rim) * radiance * mix(1.0, ao, 0.35);
    }
    // Lights carried by objects: warm pools that fall off with distance
    // (no shadows here; render_film has them).
    for (var i = 0; i < i32(g.params.z); i++) {
        let lp = g.lights[i * 2];
        let delta = lp.xyz - in.world_pos;
        let d2 = max(dot(delta, delta), lp.w * lp.w);
        let l = normalize(delta);
        let nl = max(dot(n, l), 0.0);
        let e = g.lights[i * 2 + 1].rgb / d2;
        c += (albedo * (1.0 - metal) / PI + ggx(n, v, l, rough, f0)) * nl * e;
    }
    // Ambient from the studio: diffuse sky light, then reflections.
    let hemi = mix(vec3(0.1, 0.09, 0.08), vec3(0.17, 0.18, 0.2), n.y * 0.5 + 0.5) * studio_k;
    c += albedo * (1.0 - metal) * hemi * ao;
    let r = reflect(-v, n);
    c += studio(r, key, rough) * fresnel * (1.0 - coat * coat_f) * (1.0 - 0.5 * rough) * ao * studio_k;
    c += studio(r, key, 0.06) * coat_f * coat * ao * studio_k;
    c += sheen * albedo * pow(1.0 - nv, 4.0) * hemi * 2.0 * ao;
    // Glass: mostly see-through, with a strong reflection at the edges.
    let glass = clamp(in.vcolor.w, 0.0, 1.0);
    let edge = 0.04 + 0.96 * pow(1.0 - nv, 5.0);
    c = mix(c, studio(r, key, rough) * edge + albedo * 0.08, glass);
    let alpha = 1.0 - glass * (1.0 - edge) * 0.8;
    // Emission: the surface's own light, in its color.
    c += albedo * max(in.color.a, 0.0);
    return vec4(encode(c), alpha);
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
"#;

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
            Slot { globals, bind_group, instances: instance_buffer(device, 64), instance_capacity: 64 }
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
    let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mesh layout"),
        bind_group_layouts: &[&bind_layout, &texture_layout, &detail_layout],
        push_constant_ranges: &[],
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

    let pipeline = |label, vs, fs, buffers: &[wgpu::VertexBufferLayout], topology, depth_write, depth_compare, bias, layout: &wgpu::PipelineLayout| {
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
                    format: rs.target_format,
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
    let mesh_pipeline = pipeline(
        "mesh pipeline",
        "vs_mesh",
        "fs_mesh",
        &mesh_buffers,
        wgpu::PrimitiveTopology::TriangleList,
        true,
        wgpu::CompareFunction::Less,
        // Push faces back a little (polygon offset) so wireframes drawn on
        // top of their own faces win the depth test without poking through.
        wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 },
        &mesh_layout,
    );
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
    );
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
    );

    let gpu_meshes = meshes.map(|(key, m)| (key, gpu_mesh(device, &rs.queue, &texture_layout, &sampler, &blank, m, None))).collect();

    let grid_lines = grid_lines();
    let grid = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("grid"),
        contents: bytemuck::cast_slice(&grid_lines),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let resources = Resources {
        mesh_pipeline,
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
        grid,
        grid_count: grid_lines.len() as u32,
        gamma_encode: !rs.target_format.is_srgb(),
    };
    rs.renderer.write().callback_resources.insert(resources);
}

/// Uploads a mesh; `images` reuses an earlier upload of the same textures.
fn gpu_mesh(device: &wgpu::Device, queue: &wgpu::Queue, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, blank: &Arc<[wgpu::TextureView; 3]>, m: Arc<Mesh>, images: Option<Arc<[wgpu::TextureView; 3]>>) -> GpuMesh {
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
    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, res: &mut Resources) {
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
                res.meshes.insert(*key, gpu_mesh(device, queue, &res.texture_layout, &res.sampler, &res.blank, mesh.clone(), reuse));
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

        let gamma_encode = res.gamma_encode;
        let slot = &mut res.slots[self.slot];
        let globals = Globals {
            view_proj: self.view_proj.to_cols_array_2d(),
            grid_model: self.grid_model.to_cols_array_2d(),
            cam_pos: self.cam_pos.extend(1.0).into(),
            params: [if gamma_encode { 1.0 } else { 0.0 }, if self.flat { 1.0 } else { 0.0 }, self.lights.len().min(MAX_VIEW_LIGHTS) as f32, if self.lights.is_empty() { 1.0 } else { 0.3 }],
            lights: {
                let mut l = [[0.0; 4]; MAX_VIEW_LIGHTS * 2];
                for (i, (p, r, c)) in self.lights.iter().take(MAX_VIEW_LIGHTS).enumerate() {
                    l[i * 2] = [p.x, p.y, p.z, *r];
                    l[i * 2 + 1] = [c.x, c.y, c.z, 0.0];
                }
                l
            },
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
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, res: &Resources) {
        let slot = &res.slots[self.slot];
        pass.set_bind_group(0, &slot.bind_group, &[]);

        pass.set_pipeline(&res.mesh_pipeline);
        pass.set_bind_group(2, &res.detail_group, &[]);
        pass.set_vertex_buffer(1, slot.instances.slice(..));
        for_each_group(&self.fills, 0, |key, range| {
            let Some(mesh) = res.meshes.get(&key).filter(|m| m.index_count > 0) else { return };
            pass.set_bind_group(1, mesh.textures.as_ref().unwrap_or(&res.no_textures), &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, range);
        });

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
    }
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut Resources = resources.get_mut().expect("render::init was not called");
        self.upload(device, queue, res);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &Resources = resources.get().expect("render::init was not called");
        self.draw(pass, res);
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
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("offscreen") });
    {
        let mut renderer = rs.renderer.write();
        let res: &mut Resources = renderer.callback_resources.get_mut().ok_or("renderer not ready")?;
        cb.upload(device, &rs.queue, res);
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
        cb.draw(&mut pass, res);
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

    #[test]
    fn viewport_shader_is_valid_wgsl() {
        let module = wgpu::naga::front::wgsl::parse_str(SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty()).validate(&module).unwrap_or_else(|e| panic!("{e:?}"));
    }
}
