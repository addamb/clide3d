//! The viewport's HDR finishing, a live preview of render_film's: meshes
//! draw linear radiance into a half-float MSAA target (pass A), grid and
//! wires into their own target (pass B), bright light spreads into an
//! energy-conserving bloom (a mip chain down and back up), and a composite
//! applies exposure, white balance, vignette, tonemap and grade per sample
//! before it puts the overlay on top, inside egui's pass.
//!
//! The tonemaps and the grade are ports of `film::look` and `film::post`;
//! a GPU probe test keeps them in step. This module owns exposure and
//! tonemapping for the viewport: `fs_mesh` never tonemaps.

use egui_wgpu::wgpu;
use glam::Vec3;
use wgpu::util::DeviceExt;

/// Format of the scene and bloom targets.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The most bloom mip levels (the widest film halo at 4096 px).
const MAX_LEVELS: usize = 8;

/// render_film's grade (`film::post::Post::grade`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grade {
    pub shadow: Vec3,
    pub highlight: Vec3,
    pub lift: Vec3,
    pub gamma: Vec3,
    pub gain: Vec3,
    pub saturation: f32,
    pub contrast: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Grade { shadow: Vec3::ONE, highlight: Vec3::ONE, lift: Vec3::ZERO, gamma: Vec3::ONE, gain: Vec3::ONE, saturation: 1.0, contrast: 1.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Clip at 1, like the viewport before HDR.
    None,
    Aces,
    Agx,
    Filmic,
}

impl Tone {
    pub fn name(self) -> &'static str {
        match self {
            Tone::None => "none",
            Tone::Aces => "aces",
            Tone::Agx => "agx",
            Tone::Filmic => "filmic",
        }
    }

    fn id(self) -> f32 {
        match self {
            Tone::None => 0.0,
            Tone::Aces => 1.0,
            Tone::Agx => 2.0,
            Tone::Filmic => 3.0,
        }
    }
}

/// How the viewport finishes its picture (`set_display.post`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewLook {
    /// Off draws straight into the pane, as before HDR.
    pub enabled: bool,
    /// Brightness multiplier, as render_film's camera exposure (EV 12 = 1).
    pub exposure: f32,
    /// White balance gains.
    pub balance: Vec3,
    pub tone: Tone,
    /// Share of the light above `bloom_threshold` that spreads into a halo.
    pub bloom: f32,
    pub bloom_threshold: f32,
    pub vignette: f32,
    pub grade: Grade,
}

impl Default for ViewLook {
    /// render_film's `bloom: true` and its default tonemap (ACES).
    fn default() -> Self {
        ViewLook { enabled: true, exposure: 1.0, balance: Vec3::ONE, tone: Tone::Aces, bloom: 0.08, bloom_threshold: 1.0, vignette: 0.0, grade: Grade::default() }
    }
}

impl ViewLook {
    /// HDR finishing that leaves the picture as the direct path draws it
    /// (no tonemap, bloom, vignette or grade), for comparisons.
    #[cfg(test)]
    pub fn plain() -> Self {
        ViewLook { tone: Tone::None, bloom: 0.0, ..Default::default() }
    }
}

/// The finishing uniform (`Post` in the WGSL).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniform {
    /// rgb = exposure x white balance, w = tonemap (`Tone::id`).
    eb: [f32; 4],
    /// Bloom strength, threshold, vignette, 1 when the target is not sRGB
    /// and the composite must gamma-encode.
    bloom: [f32; 4],
    /// Grade: shadow tint (w = saturation), highlight tint (w = contrast),
    /// lift, gamma, gain.
    shadow: [f32; 4],
    highlight: [f32; 4],
    lift: [f32; 4],
    gamma: [f32; 4],
    gain: [f32; 4],
    /// Target width, height and their inverses.
    size: [f32; 4],
    /// The background under the pane at its top and bottom edges, as the
    /// target stores it (rgb, 0..1): the halo over uncovered background is
    /// added in linear light on top of it.
    back_top: [f32; 4],
    back_bottom: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<PostUniform>() == 160, "PostUniform must match the WGSL struct");

impl PostUniform {
    fn new(look: &ViewLook, size: [u32; 2], gamma_encode: bool, background: [egui::Color32; 2]) -> Self {
        let g = &look.grade;
        let e = look.balance * look.exposure;
        let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
        PostUniform {
            eb: [e.x, e.y, e.z, look.tone.id()],
            bloom: [look.bloom.clamp(0.0, 1.0), look.bloom_threshold.max(0.0), look.vignette.clamp(0.0, 1.0), if gamma_encode { 1.0 } else { 0.0 }],
            shadow: g.shadow.extend(g.saturation).into(),
            highlight: g.highlight.extend(g.contrast).into(),
            lift: g.lift.extend(0.0).into(),
            gamma: g.gamma.extend(0.0).into(),
            gain: g.gain.extend(0.0).into(),
            size: [w, h, 1.0 / w, 1.0 / h],
            back_top: rgba(background[0]),
            back_bottom: rgba(background[1]),
        }
    }
}

fn rgba(c: egui::Color32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
}

/// Per pass of the bloom chain: x = 1 to apply exposure and the threshold
/// (the first step down), y = this level's share of the halo, z = 1 when
/// there is a coarser level to add, w unused.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LevelUniform([f32; 4]);

/// How many bloom levels (the first at half size) an image whose shorter
/// side is `min_dim` pixels gets: enough to reach film's widest halo (5% of
/// that side), at least one, never smaller than a pixel.
pub fn bloom_levels(min_dim: u32) -> usize {
    let wanted = (0.05 * min_dim as f32).max(1.0).log2().round() as usize;
    let fits = (32 - min_dim.max(2).leading_zeros() - 1) as usize;
    wanted.clamp(1, MAX_LEVELS).min(fits.max(1))
}

/// Each level's share of the halo. Film blurs the bright light with
/// Gaussians of 0.6%, 1.8% and 5% of the shorter side, weighted 0.5, 0.3
/// and 0.2; level k blurs over about 2^(k+1) pixels, so each radius lands
/// between two levels and its weight is split between them. Sums to 1.
pub fn bloom_weights(min_dim: u32) -> [f32; MAX_LEVELS] {
    let n = bloom_levels(min_dim);
    let mut w = [0.0; MAX_LEVELS];
    for (frac, weight) in [(0.006, 0.5), (0.018, 0.3), (0.05, 0.2)] {
        let level = ((frac * min_dim as f32).max(1.0).log2() - 1.0).clamp(0.0, (n - 1) as f32);
        let (lo, t) = (level.floor() as usize, level.fract());
        w[lo] += weight * (1.0 - t);
        w[(lo + 1).min(n - 1)] += weight * t;
    }
    let total: f32 = w.iter().sum();
    w.map(|x| x / total)
}

/// The tonemaps and grade, shared by the composite and the parity probe.
pub(crate) const POST_COMMON: &str = r#"
struct Post {
    eb: vec4<f32>, bloom: vec4<f32>, shadow: vec4<f32>, highlight: vec4<f32>,
    lift: vec4<f32>, gamma: vec4<f32>, gain: vec4<f32>, size: vec4<f32>,
    back_top: vec4<f32>, back_bottom: vec4<f32>,
};

// Non-finite values (an overflowing emitter) become black.
fn finite(c: vec3<f32>) -> vec3<f32> {
    return select(vec3(0.0), c, abs(c) < vec3(1e30));
}

fn lum(c: vec3<f32>) -> f32 { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

fn tone_aces(c: vec3<f32>) -> vec3<f32> {
    let x = c * 0.6;
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3(0.0), vec3(1.0));
}

// One shoulder for all channels keeps saturated light colored.
fn tone_filmic(c: vec3<f32>) -> vec3<f32> {
    return c / (1.0 + max(c.x, max(c.y, c.z)));
}

fn tone_agx(c: vec3<f32>) -> vec3<f32> {
    let inset = mat3x3<f32>(0.84247905, 0.042328242, 0.042375654, 0.0784336, 0.87846863, 0.0784336, 0.079223745, 0.07916613, 0.87914294);
    let outset = mat3x3<f32>(1.196879, -0.05289685, -0.052971635, -0.09802088, 1.1519032, -0.09804345, -0.09902974, -0.098961175, 1.1510737);
    let v = max(inset * c, vec3(1e-10));
    let x = (clamp(log2(v), vec3(-12.47393), vec3(4.026069)) + 12.47393) / 16.5;
    let x2 = x * x;
    let x4 = x2 * x2;
    let curved = 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
    return outset * pow(max(curved, vec3(0.0)), vec3(2.2));
}

fn contrast(v: f32, k: f32) -> f32 {
    if (v <= 0.18) { return 0.18 * pow(max(v, 0.0) / 0.18, k); }
    return 1.0 - 0.82 * pow(max(1.0 - v, 0.0) / 0.82, k);
}

fn grade(c0: vec3<f32>, p: Post) -> vec3<f32> {
    let y = clamp(lum(c0), 0.0, 1.0);
    var c = c0 * mix(p.shadow.rgb, p.highlight.rgb, y);
    c = mix(vec3(lum(c)), c, p.shadow.w);
    let k = p.highlight.w;
    c = vec3(contrast(c.x, k), contrast(c.y, k), contrast(c.z, k));
    c = max(c + p.lift.rgb, vec3(0.0));
    return pow(c, 1.0 / p.gamma.rgb) * p.gain.rgb;
}

// Exposed scene light to linear display light (0..1): vignette, tonemap
// and grade. `uv` is the pixel's place in the image (0..1).
fn finish(c: vec3<f32>, uv: vec2<f32>, p: Post) -> vec3<f32> {
    let q = uv * 2.0 - 1.0;
    let r = 0.5 * dot(q, q);
    var m = max(c * (1.0 - p.bloom.z * r * r), vec3(0.0));
    let tone = i32(p.eb.w + 0.5);
    if (tone == 1) { m = tone_aces(m); }
    else if (tone == 2) { m = tone_agx(m); }
    else if (tone == 3) { m = tone_filmic(m); }
    return clamp(grade(clamp(m, vec3(0.0), vec3(1.0)), p), vec3(0.0), vec3(1.0));
}

// `finish`, then gamma when the target does not encode (as film's 2.2).
fn display(c: vec3<f32>, uv: vec2<f32>, p: Post) -> vec3<f32> {
    let m = finish(c, uv, p);
    if (p.bloom.w > 0.5) { return pow(m, vec3(1.0 / 2.2)); }
    return m;
}

// The part of film's glare source above the threshold.
fn bright(c: vec3<f32>, threshold: f32) -> vec3<f32> {
    let l = max(c.x, max(c.y, c.z));
    return c * (max(l - threshold, 0.0) / max(l, 1e-6));
}
"#;

const POST_PASSES: &str = r#"
@group(0) @binding(0) var<uniform> u: Post;

struct FullOut { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

// One triangle over the whole viewport; uv runs down the image like rows.
@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> FullOut {
    let p = vec2(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var out: FullOut;
    out.clip = vec4(p, 0.0, 1.0);
    out.uv = vec2(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    return out;
}

// Bloom passes: see `LevelUniform`.
@group(0) @binding(1) var<uniform> level: vec4<f32>;
@group(0) @binding(2) var t_src: texture_2d<f32>;
@group(0) @binding(3) var s_post: sampler;
@group(0) @binding(4) var t_next: texture_2d<f32>;

fn tap(uv: vec2<f32>, d: vec2<f32>, x: f32, y: f32) -> vec3<f32> {
    return textureSampleLevel(t_src, s_post, uv + d * vec2(x, y), 0.0).rgb;
}

// Halves the image with the 13-tap filter (weights sum to 1, so energy is
// kept). The first step exposes the scene and keeps what is above the
// threshold.
@fragment
fn fs_down(in: FullOut) -> @location(0) vec4<f32> {
    let d = 1.0 / vec2<f32>(textureDimensions(t_src));
    let uv = in.uv;
    var c = tap(uv, d, 0.0, 0.0) * 0.125;
    c += (tap(uv, d, -2.0, -2.0) + tap(uv, d, 2.0, -2.0) + tap(uv, d, -2.0, 2.0) + tap(uv, d, 2.0, 2.0)) * 0.03125;
    c += (tap(uv, d, 0.0, -2.0) + tap(uv, d, -2.0, 0.0) + tap(uv, d, 2.0, 0.0) + tap(uv, d, 0.0, 2.0)) * 0.0625;
    c += (tap(uv, d, -1.0, -1.0) + tap(uv, d, 1.0, -1.0) + tap(uv, d, -1.0, 1.0) + tap(uv, d, 1.0, 1.0)) * 0.125;
    c = finite(c);
    if (level.x > 0.5) { c = bright(c * u.eb.rgb, u.bloom.y); }
    return vec4(c, 1.0);
}

// This level's share of the halo plus the coarser levels, spread with a
// tent filter on the way up.
@fragment
fn fs_up(in: FullOut) -> @location(0) vec4<f32> {
    var c = textureSampleLevel(t_src, s_post, in.uv, 0.0).rgb * level.y;
    if (level.z > 0.5) {
        let d = 1.0 / vec2<f32>(textureDimensions(t_next));
        var s = vec3(0.0);
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let w = f32((2 - abs(x)) * (2 - abs(y)));
                s += textureSampleLevel(t_next, s_post, in.uv + d * vec2(f32(x), f32(y)), 0.0).rgb * w;
            }
        }
        c += s / 16.0;
    }
    return vec4(c, 1.0);
}

// The composite, drawn into the pane.
@group(0) @binding(1) var t_scene: texture_multisampled_2d<f32>;
@group(0) @binding(2) var t_bloom: texture_2d<f32>;
@group(0) @binding(3) var t_overlay: texture_multisampled_2d<f32>;
@group(0) @binding(4) var s_bloom: sampler;

// The grey scene light that finishes as display light `shown`, by
// bisection (`finish` rises with light).
fn scene_light(shown: f32, uv: vec2<f32>) -> f32 {
    var lo = 0.0;
    var hi = 64.0;
    for (var i = 0; i < 18; i++) {
        let mid = 0.5 * (lo + hi);
        if (lum(finish(vec3(mid), uv, u)) < shown) { lo = mid; } else { hi = mid; }
    }
    return 0.5 * (lo + hi);
}

// What the halo adds over uncovered background, as the target stores it.
// The background (known: the pane's gradient or the clear colour) stands
// for the scene light that would show as it; the halo adds to that light,
// as film's glare adds to whatever it falls on. Nothing where there is no
// halo, so such background stays exactly as it was.
fn halo_over_background(glow: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    if (max(glow.x, max(glow.y, glow.z)) <= 0.0) { return vec3(0.0); }
    let back = mix(u.back_top.rgb, u.back_bottom.rgb, uv.y);
    let back_light = pow(back, vec3(2.2));
    let under = vec3(scene_light(lum(back_light), uv));
    let add = max(finish(under + glow, uv, u) - finish(under, uv, u), vec3(0.0));
    // An sRGB target blends in linear light; otherwise blending is in
    // gamma space, so add the step there.
    if (u.bloom.w > 0.5) { return pow(back_light + add, vec3(1.0 / 2.2)) - back; }
    return add;
}

// Per sample: the scene is unpremultiplied, exposed, loses the light that
// went into the halo, gains the halo, and is finished on its own; the grid
// and wires go over it; the results average (tonemap, then resolve, as the
// direct path encodes and blends each sample).
@fragment
fn fs_composite(in: FullOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(u.size.xy);
    let texel = clamp(vec2<i32>(floor(in.uv * u.size.xy)), vec2(0), size - 1);
    let strength = u.bloom.x;
    let glow = strength * finite(textureSampleLevel(t_bloom, s_bloom, in.uv, 0.0).rgb);
    let halo = halo_over_background(glow, in.uv);
    var sum = vec4(0.0);
    for (var i = 0; i < 4; i++) {
        let p = textureLoad(t_scene, texel, i);
        var s = vec4(halo, 0.0);
        if (p.a > 0.0) {
            let c = finite(p.rgb * u.eb.rgb / p.a);
            let core = c - strength * bright(c, u.bloom.y);
            s = vec4(display(core + glow, in.uv, u) * p.a + halo * (1.0 - p.a), p.a);
        }
        let over = textureLoad(t_overlay, texel, i);
        sum += over + s * (1.0 - over.a);
    }
    return sum * 0.25;
}
"#;

/// The post shader module's source.
pub(crate) fn shader_source() -> String {
    format!("{POST_COMMON}{POST_PASSES}")
}

/// Pipelines, layouts and samplers for the finishing passes (built once).
pub(super) struct PostPipelines {
    bloom_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    pub(super) composite: wgpu::RenderPipeline,
    /// Linear, mirrored at the edges like film's reflecting blur.
    sampler: wgpu::Sampler,
    /// Bound as the coarser level where there is none.
    empty: wgpu::TextureView,
}

impl PostPipelines {
    /// `target` is the pane's format; the composite draws into an MSAA
    /// pass with a depth buffer of `depth`, as egui's is.
    pub(super) fn new(device: &wgpu::Device, target: wgpu::TextureFormat, depth: wgpu::TextureFormat, samples: u32) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("post shader"), source: wgpu::ShaderSource::Wgsl(shader_source().into()) });
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let texture = |binding, multisampled: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: !multisampled }, view_dimension: wgpu::TextureViewDimension::D2, multisampled },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None };
        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom layout"),
            entries: &[uniform(0), uniform(1), texture(2, false), sampler_entry(3), texture(4, false)],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite layout"),
            entries: &[uniform(0), texture(1, true), texture(2, false), texture(3, true), sampler_entry(4)],
        });
        let pipeline = |label, fs, layout: &wgpu::BindGroupLayout, format, blend, depth_stencil, samples| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[layout], push_constant_ranges: &[] });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
                primitive: Default::default(),
                depth_stencil,
                multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let down = pipeline("bloom down", "fs_down", &bloom_layout, HDR_FORMAT, None, None, 1);
        let up = pipeline("bloom up", "fs_up", &bloom_layout, HDR_FORMAT, None, None, 1);
        // Premultiplied over what the pane already shows; depth is ignored.
        let composite = pipeline(
            "post composite",
            "fs_composite",
            &composite_layout,
            target,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            Some(wgpu::DepthStencilState { format: depth, depth_write_enabled: false, depth_compare: wgpu::CompareFunction::Always, stencil: Default::default(), bias: Default::default() }),
            samples,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bloom"),
            address_mode_u: wgpu::AddressMode::MirrorRepeat,
            address_mode_v: wgpu::AddressMode::MirrorRepeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty = target_view(device, "bloom empty", [1, 1], HDR_FORMAT, 1, wgpu::TextureUsages::TEXTURE_BINDING);
        PostPipelines { bloom_layout, composite_layout, down, up, composite, sampler, empty }
    }
}

fn target_view(device: &wgpu::Device, label: &str, size: [u32; 2], format: wgpu::TextureFormat, samples: u32, usage: wgpu::TextureUsages) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// One pane's (or one screenshot's) offscreen targets, sized to its pixels.
/// About 80 bytes a pixel; recreated only when the size changes.
pub(super) struct PostTargets {
    pub(super) size: [u32; 2],
    /// Linear radiance, premultiplied, alpha = coverage (pass A).
    pub(super) scene_ms: wgpu::TextureView,
    /// Its resolve, the bloom's source.
    pub(super) scene: wgpu::TextureView,
    /// Pass A's depth, which pass B tests the grid and wires against.
    pub(super) depth_ms: wgpu::TextureView,
    /// Grid and wires in the pane's format (pass B), composited per sample.
    pub(super) overlay_ms: wgpu::TextureView,
    /// Bloom levels, each half the last: the way down and the way up.
    down: Vec<wgpu::TextureView>,
    up: Vec<wgpu::TextureView>,
    uniform: wgpu::Buffer,
    down_groups: Vec<wgpu::BindGroup>,
    up_groups: Vec<wgpu::BindGroup>,
    composite_group: wgpu::BindGroup,
}

impl PostTargets {
    pub(super) fn new(device: &wgpu::Device, pipes: &PostPipelines, format: wgpu::TextureFormat, depth: wgpu::TextureFormat, samples: u32, size: [u32; 2]) -> Self {
        let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let read = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let scene_ms = target_view(device, "hdr scene msaa", size, HDR_FORMAT, samples, read);
        let scene = target_view(device, "hdr scene", size, HDR_FORMAT, 1, read);
        let depth_ms = target_view(device, "hdr depth", size, depth, samples, attach);
        let overlay_ms = target_view(device, "overlay msaa", size, format, samples, read);
        let min_dim = size[0].min(size[1]);
        let n = bloom_levels(min_dim);
        let weights = bloom_weights(min_dim);
        let level_size = |k: usize| [(size[0] >> (k + 1)).max(1), (size[1] >> (k + 1)).max(1)];
        let down: Vec<_> = (0..n).map(|k| target_view(device, "bloom down", level_size(k), HDR_FORMAT, 1, read)).collect();
        let up: Vec<_> = (0..n).map(|k| target_view(device, "bloom up", level_size(k), HDR_FORMAT, 1, read)).collect();
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bloom_group = |level: LevelUniform, src: &wgpu::TextureView, next: &wgpu::TextureView| {
            let level = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("bloom level"), contents: bytemuck::bytes_of(&level), usage: wgpu::BufferUsages::UNIFORM });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom"),
                layout: &pipes.bloom_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: level.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&pipes.sampler) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(next) },
                ],
            })
        };
        let down_groups = (0..n).map(|k| bloom_group(LevelUniform([if k == 0 { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]), if k == 0 { &scene } else { &down[k - 1] }, &pipes.empty)).collect();
        let up_groups = (0..n).map(|k| bloom_group(LevelUniform([0.0, weights[k], if k + 1 < n { 1.0 } else { 0.0 }, 0.0]), &down[k], up.get(k + 1).unwrap_or(&pipes.empty))).collect();
        let composite_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post composite"),
            layout: &pipes.composite_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&scene_ms) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&up[0]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&overlay_ms) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&pipes.sampler) },
            ],
        });
        PostTargets { size, scene_ms, scene, depth_ms, overlay_ms, down, up, uniform, down_groups, up_groups, composite_group }
    }

    /// Writes this frame's finishing settings; `background` is what the
    /// pane shows under the picture (top and bottom).
    pub(super) fn write(&self, queue: &wgpu::Queue, look: &ViewLook, gamma_encode: bool, background: [egui::Color32; 2]) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&PostUniform::new(look, self.size, gamma_encode, background)));
    }

    /// Records the bloom chain from `scene` (after pass A); nothing when
    /// `look` has no bloom.
    pub(super) fn bloom(&self, encoder: &mut wgpu::CommandEncoder, pipes: &PostPipelines, look: &ViewLook) {
        if look.bloom <= 0.0 {
            return;
        }
        let mut pass = |label, pipeline: &wgpu::RenderPipeline, target: &wgpu::TextureView, group: &wgpu::BindGroup| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        };
        for (target, group) in self.down.iter().zip(&self.down_groups) {
            pass("bloom down", &pipes.down, target, group);
        }
        for (target, group) in self.up.iter().zip(&self.up_groups).rev() {
            pass("bloom up", &pipes.up, target, group);
        }
    }

    /// Draws the finished picture into `pass` (the pane's viewport).
    pub(super) fn composite(&self, pass: &mut wgpu::RenderPass<'_>, pipes: &PostPipelines) {
        pass.set_pipeline(&pipes.composite);
        pass.set_bind_group(0, &self.composite_group, &[]);
        pass.draw(0..3, 0..1);
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_support;
    use serde_json::json;

    fn validate(src: &str) -> wgpu::naga::Module {
        let module = wgpu::naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty()).validate(&module).unwrap_or_else(|e| panic!("{e:?}"));
        module
    }

    #[test]
    fn post_shader_is_valid_wgsl() {
        let module = validate(&shader_source());
        for entry in ["vs_full", "fs_down", "fs_up", "fs_composite"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "no entry point {entry}");
        }
        let post = module.types.iter().find(|(_, t)| t.name.as_deref() == Some("Post")).expect("Post").1;
        let wgpu::naga::TypeInner::Struct { span, .. } = &post.inner else { panic!("Post is not a struct") };
        assert_eq!(*span as usize, std::mem::size_of::<PostUniform>());
    }

    #[test]
    fn bloom_levels_and_weights_follow_the_image_size() {
        assert_eq!(bloom_levels(16), 1);
        assert_eq!(bloom_levels(17), 1);
        assert!(bloom_levels(768) >= 4 && bloom_levels(768) <= 6);
        assert_eq!(bloom_levels(1 << 20), MAX_LEVELS);
        // Every level is at least a pixel.
        for d in [2, 3, 16, 33, 100, 768, 4096] {
            assert!(d >> bloom_levels(d) >= 1, "{d}");
        }
        // The halo's mean radius (level k spreads over about 2^(k+1)
        // pixels) is a fixed share of the image, like film's.
        let reach = |d: u32| bloom_weights(d).iter().enumerate().map(|(k, w)| w * 2f32.powi(k as i32 + 1)).sum::<f32>() / d as f32;
        for d in [16, 17, 100, 512, 768, 2048] {
            let w = bloom_weights(d);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{d}: {w:?}");
            assert!(w.iter().skip(bloom_levels(d)).all(|&x| x == 0.0), "{d}: weight past the last level");
        }
        let (a, b) = (reach(512), reach(2048));
        assert!((a / b - 1.0).abs() < 0.25, "halo reach {a} at 512 vs {b} at 2048");
    }

    /// Runs the composite's `display` (after exposure and white balance)
    /// on `colors` through a compute probe. `uv.x` runs along the list.
    fn gpu_display(rs: &egui_wgpu::RenderState, look: &ViewLook, colors: &[Vec3]) -> Vec<Vec3> {
        let device = &rs.device;
        let src = format!(
            "{POST_COMMON}
            @group(0) @binding(0) var<uniform> u: Post;
            @group(0) @binding(1) var<storage, read> input: array<vec4<f32>>;
            @group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
            @compute @workgroup_size(64)
            fn probe(@builtin(global_invocation_id) id: vec3<u32>) {{
                let i = id.x;
                if (i >= arrayLength(&input)) {{ return; }}
                let c = input[i];
                output[i] = vec4(display(c.rgb * u.eb.rgb, vec2(c.w, 0.5), u), 1.0);
            }}"
        );
        validate(&src);
        let n = colors.len();
        let input: Vec<[f32; 4]> = colors.iter().enumerate().map(|(i, c)| [c.x, c.y, c.z, (i as f32 + 0.5) / n as f32]).collect();
        test_support::checked(rs, || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("post probe"), source: wgpu::ShaderSource::Wgsl(src.into()) });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("post probe"), layout: None, module: &module, entry_point: Some("probe"), compilation_options: Default::default(), cache: None });
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::bytes_of(&PostUniform::new(look, [n as u32, 1], true, [egui::Color32::BLACK; 2])), usage: wgpu::BufferUsages::UNIFORM });
            let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&input), usage: wgpu::BufferUsages::STORAGE });
            let bytes = (n * 16) as u64;
            let output = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
            let readback = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: input.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                ],
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups((n as u32).div_ceil(64), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, bytes);
            rs.queue.submit([encoder.finish()]);
            let slice = readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let out: Vec<[f32; 4]> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
            out.into_iter().map(|c| Vec3::new(c[0], c[1], c[2])).collect()
        })
        .unwrap()
    }

    #[test]
    fn tonemaps_and_grades_match_render_film() {
        let Some(rs) = test_support::headless() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        // A grey ramp over the HDR range, then saturated colours.
        let mut colors: Vec<Vec3> = (0..1001).map(|i| Vec3::splat(i as f32 * 0.016)).collect();
        for i in 0..64 {
            let hue = i as f32 / 64.0 * std::f32::consts::TAU;
            let c = Vec3::new(hue.cos(), (hue + 2.1).cos(), (hue + 4.2).cos()).max(Vec3::ZERO) + Vec3::splat(0.02);
            colors.push(c * (0.25 + (i % 8) as f32));
        }
        // Without bloom, grain or sharpening, film's finishing is per pixel
        // (vignette aside, which depends only on where the pixel is).
        for args in [
            json!({ "tonemap": "aces" }),
            json!({ "tonemap": "agx" }),
            json!({ "tonemap": "filmic", "white_balance": 4000, "camera": { "ev": 11 } }),
            json!({ "look": "trailer", "bloom": false, "grain": 0, "sharpen": 0 }),
            json!({ "look": "night_ops", "bloom": false, "grain": 0, "vignette": 0.5 }),
            json!({ "grade": { "lift": [0.05, 0, -0.02], "gamma": [1.2, 1, 0.9], "gain": [1, 1.1, 0.9], "contrast": 1.4, "saturation": 1.3 } }),
        ] {
            let look = crate::film::Look::parse(&args).unwrap();
            let film = look.finish(colors.clone(), colors.len(), 1, 1.0);
            let gpu = gpu_display(&rs, &look.view_look(), &colors);
            // Film truncates to bytes; the GPU value lies in [byte, byte + 1).
            let worst = film.iter().zip(&gpu).flat_map(|(f, g)| (0..3).map(move |c| (g[c] * 255.0 - (f[c] as f32 + 0.5)).abs())).fold(0.0f32, f32::max);
            assert!(worst <= 1.0, "{args}: the GPU finish is {worst:.2}/255 from film's");
        }
    }
}
