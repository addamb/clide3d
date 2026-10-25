//! Shared plumbing for viewport GPU tests: one windowless device per
//! process, a render state per target format, per-test validation scopes,
//! image metrics and a small fixture scene.
//!
//! The window draws into a non-sRGB target (`LINEAR`), where the shader
//! gamma-encodes by hand and blends in gamma space; `SRGB` lets the hardware
//! encode. Viewport tests should run on both (see `FORMATS`).

use std::cell::Cell;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use egui_wgpu::wgpu;
use glam::{Mat4, Vec3};

use super::{Resources, ViewportCallback};
use crate::film::gpu::block_on;
use crate::mesh::MeshLib;
use crate::scene::{Scene, Shape};

/// The hardware-encoded target the old headless tests used.
pub const SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
/// The window's kind of target: the shader encodes gamma itself.
pub const LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
pub const FORMATS: [wgpu::TextureFormat; 2] = [SRGB, LINEAR];

/// The viewport's background (the agent screenshot clear colour).
pub const BACKGROUND: [u8; 3] = crate::app::BACKGROUND;

type Gpu = (wgpu::Adapter, wgpu::Device, wgpu::Queue);

fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        Some((adapter, device, queue))
    })
    .as_ref()
}

/// A render state on the sRGB target (None where there is no GPU).
pub fn headless() -> Option<egui_wgpu::RenderState> {
    headless_with(SRGB)
}

/// A render state drawing into `format` (one of `FORMATS`), sharing the
/// process's device and initialised with `render::init`. None where there
/// is no GPU; a validation error in `init` panics rather than skipping.
pub fn headless_with(format: wgpu::TextureFormat) -> Option<egui_wgpu::RenderState> {
    static STATES: [OnceLock<Option<egui_wgpu::RenderState>>; FORMATS.len()] = [const { OnceLock::new() }; FORMATS.len()];
    let i = FORMATS.iter().position(|f| *f == format).unwrap_or_else(|| panic!("no headless state for {format:?}"));
    STATES[i]
        .get_or_init(|| {
            let (adapter, device, queue) = gpu()?.clone();
            let renderer = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
            let rs = egui_wgpu::RenderState {
                adapter,
                available_adapters: Vec::new(),
                device,
                queue,
                target_format: format,
                renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
            };
            checked(&rs, || super::init(&rs, MeshLib::new().prims())).unwrap_or_else(|e| panic!("render::init on {format:?}: {e}"));
            Some(rs)
        })
        .clone()
}

thread_local! {
    static HOLDING: Cell<bool> = const { Cell::new(false) };
}

/// Serialises error-scoped sections: wgpu 27 keeps one scope stack per
/// device, so parallel tests would pop each other's scopes. Re-entrant on
/// one thread, so `checked` may nest. Get render states before entering
/// `checked`: a first `headless_with` waits for this lock.
struct GpuLock(Option<MutexGuard<'static, ()>>);

impl GpuLock {
    fn take() -> Self {
        static LOCK: Mutex<()> = Mutex::new(());
        if HOLDING.get() {
            return GpuLock(None);
        }
        // A test that panicked while holding it must not fail every later one.
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        HOLDING.set(true);
        GpuLock(Some(guard))
    }
}

impl Drop for GpuLock {
    fn drop(&mut self) {
        if self.0.is_some() {
            HOLDING.set(false);
        }
    }
}

/// Holds the GPU lock until dropped (as `checked` does, without error
/// scopes): renders by other tests wait, so a sequence of frames sees no
/// one else's uploads.
pub fn hold(rs: &egui_wgpu::RenderState) -> impl Drop {
    let _ = rs;
    GpuLock::take()
}

/// Runs `f` inside validation and out-of-memory error scopes on `rs`'s
/// device; Err carries the first error raised. GPU work that `f` spawns on
/// other threads is covered too, since the scopes are device-wide.
pub fn checked<T>(rs: &egui_wgpu::RenderState, f: impl FnOnce() -> T) -> Result<T, String> {
    let _lock = GpuLock::take();
    rs.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    rs.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let validation = block_on(rs.device.pop_error_scope());
    let memory = block_on(rs.device.pop_error_scope());
    let out = out.unwrap_or_else(|p| std::panic::resume_unwind(p));
    match validation.or(memory) {
        Some(e) => Err(e.to_string()),
        None => Ok(out),
    }
}

/// Drops every sculpt chunk on the GPU, so the next frame uploads them all
/// (the full re-upload a partial one must equal).
pub fn clear_chunk_pool(rs: &egui_wgpu::RenderState) {
    if let Some(r) = rs.renderer.write().callback_resources.get_mut::<Resources>() {
        r.pool = Default::default();
    }
}

/// What the last frame uploaded of the sculpt chunks: (bytes, chunks, ms)
/// and how many the pool holds.
pub fn chunk_upload(rs: &egui_wgpu::RenderState) -> ((u64, usize, f64), usize) {
    rs.renderer.read().callback_resources.get::<Resources>().map_or(((0, 0, 0.0), 0), |r| (r.pool.last_upload, r.pool.len()))
}

/// `render_image` inside `checked`, with any error turned into Err.
pub fn render(rs: &egui_wgpu::RenderState, cb: &ViewportCallback, width: u32, height: u32, clear: [u8; 3]) -> Result<egui::ColorImage, String> {
    checked(rs, || super::render_image(rs, cb, width, height, clear))?
}

/// Draws `cb` the way the window does: `prepare` (with egui's encoder),
/// then `paint` inside an MSAA pass over a `screen`-sized target cleared to
/// `clear`, with the viewport egui would set for `pane` (in points; pixels
/// per point 1). Returns the whole screen, checked.
pub fn paint_window(rs: &egui_wgpu::RenderState, cb: &ViewportCallback, pane: egui::Rect, screen: [u32; 2], clear: [u8; 3]) -> Result<egui::ColorImage, String> {
    use egui_wgpu::CallbackTrait;
    let device = &rs.device;
    let format = rs.target_format;
    let size = wgpu::Extent3d { width: screen[0], height: screen[1], depth_or_array_layers: 1 };
    let texture = |format, samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor { label: Some("test window"), size, mip_level_count: 1, sample_count: samples, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] })
    };
    let msaa = texture(format, super::MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let resolved = texture(format, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
    let depth = texture(super::DEPTH_FORMAT, super::MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let bytes_per_row = (screen[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("test window"), size: (bytes_per_row * screen[1]) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let c = clear.map(|v| v as f64 / 255.0);
    let c = if format.is_srgb() { c.map(|v| v.powf(2.2)) } else { c };
    checked(rs, || {
        let mut renderer = rs.renderer.write();
        let resources = &mut renderer.callback_resources;
        let mut encoder = device.create_command_encoder(&Default::default());
        let user = cb.prepare(device, &rs.queue, &egui_wgpu::ScreenDescriptor { size_in_pixels: screen, pixels_per_point: 1.0 }, &mut encoder, resources);
        {
            let view = |t: &wgpu::Texture| t.create_view(&Default::default());
            let (msaa, resolved, depth) = (view(&msaa), view(&resolved), view(&depth));
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_render"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa,
                        depth_slice: None,
                        resolve_target: Some(&resolved),
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0], g: c[1], b: c[2], a: 1.0 }), store: wgpu::StoreOp::Discard },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &depth, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            let info = egui::PaintCallbackInfo { viewport: pane, clip_rect: pane, pixels_per_point: 1.0, screen_size_px: screen };
            let v = info.viewport_in_pixels();
            pass.set_viewport(v.left_px as f32, v.top_px as f32, v.width_px as f32, v.height_px as f32, 0.0, 1.0);
            cb.paint(info, &mut pass, resources);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &resolved, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(screen[1]) } },
            size,
        );
        rs.queue.submit(user.into_iter().chain([encoder.finish()]));
        drop(renderer);
        super::read_image(device, &readback, format, screen, bytes_per_row)
    })?
}

/// Paints an egui frame's output (`ctx.run`'s) into a `screen`-sized
/// image with a renderer of its own (pixels per point 1), as the window
/// draws its panels. Returns the image, checked.
pub fn paint_ui(rs: &egui_wgpu::RenderState, ctx: &egui::Context, out: egui::FullOutput, screen: [u32; 2]) -> Result<egui::ColorImage, String> {
    let device = &rs.device;
    let format = rs.target_format;
    let size = wgpu::Extent3d { width: screen[0], height: screen[1], depth_or_array_layers: 1 };
    let target = device.create_texture(&wgpu::TextureDescriptor { label: Some("test ui"), size, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let bytes_per_row = (screen[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("test ui"), size: (bytes_per_row * screen[1]) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    checked(rs, || {
        let mut renderer = egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
        let jobs = ctx.tessellate(out.shapes, 1.0);
        let screen_desc = egui_wgpu::ScreenDescriptor { size_in_pixels: screen, pixels_per_point: 1.0 };
        for (id, delta) in &out.textures_delta.set {
            renderer.update_texture(device, &rs.queue, *id, delta);
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        let user = renderer.update_buffers(device, &rs.queue, &mut encoder, &jobs, &screen_desc);
        {
            let view = target.create_view(&Default::default());
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("test ui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, &jobs, &screen_desc);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(screen[1]) } },
            size,
        );
        rs.queue.submit(user.into_iter().chain([encoder.finish()]));
        super::read_image(device, &readback, format, screen, bytes_per_row)
    })?
}

/// Whether `render_image`'s last call drew shadows, and why not.
pub fn offscreen_shadow_status(rs: &egui_wgpu::RenderState) -> super::ShadowStatus {
    rs.renderer.read().callback_resources.get::<Resources>().expect("render::init was not called").offscreen_shadow.status
}

/// Whether `render_image`'s last call drew ambient occlusion, and why not.
pub fn offscreen_ao_status(rs: &egui_wgpu::RenderState) -> super::AoStatus {
    rs.renderer.read().callback_resources.get::<Resources>().expect("render::init was not called").offscreen_ao
}

/// How many environments are on the GPU.
pub fn gpu_environments(rs: &egui_wgpu::RenderState) -> usize {
    rs.renderer.read().callback_resources.get::<Resources>().expect("render::init was not called").envs.len()
}

/// Whether `rs`'s shader takes the manual gamma-encode branch.
pub fn gamma_encodes(rs: &egui_wgpu::RenderState) -> bool {
    rs.renderer.read().callback_resources.get::<Resources>().expect("render::init was not called").gamma_encode
}

/// The test scene: a ground plane, a cube, a sphere, a cylinder and a
/// clay capsule, lit by the studio light (no light objects), seen by a
/// perspective camera from the front right.
pub struct Fixture {
    pub scene: Scene,
    pub meshes: MeshLib,
    pub eye: Vec3,
    pub target: Vec3,
    /// Vertical field of view in degrees.
    pub fov: f32,
}

impl Fixture {
    pub fn new() -> Self {
        let mut scene = Scene::default();
        let mut add = |shape, at: [f32; 3], color: [f32; 3]| {
            let id = scene.add(shape);
            let o = scene.get_mut(id).unwrap();
            o.translation = at.into();
            o.color = color;
            id
        };
        add(Shape::Plane, [0.0, 0.0, 0.0], [0.62, 0.62, 0.6]);
        add(Shape::Cube, [-1.1, 0.5, -0.3], [0.8, 0.32, 0.25]);
        add(Shape::Sphere, [0.2, 0.5, 0.6], [0.3, 0.5, 0.82]);
        add(Shape::Cylinder, [1.2, 0.5, -0.6], [0.4, 0.72, 0.36]);
        let clay = add(Shape::Clay, [0.0, 0.0, -1.2], [0.85, 0.72, 0.55]);
        scene.get_mut(clay).unwrap().clay = Some(
            serde_json::from_value(serde_json::json!({
                "blobs": [
                    { "type": "capsule", "a": [0, 0.2, 0], "b": [0, 1.1, 0], "radius": 0.22 },
                    { "type": "sphere", "center": [0, 1.35, 0], "radius": 0.24 },
                ],
                "resolution": 24,
            }))
            .unwrap(),
        );
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);
        Fixture { scene, meshes, eye: Vec3::new(3.2, 2.6, 4.4), target: Vec3::new(0.0, 0.5, -0.2), fov: 35.0 }
    }

    pub fn view_proj(&self, width: u32, height: u32) -> Mat4 {
        let far = (self.eye - self.target).length() * 3.0 + 10.0;
        Mat4::perspective_rh(self.fov.to_radians(), width as f32 / height as f32, 0.05, far) * Mat4::look_at_rh(self.eye, self.target, Vec3::Y)
    }

    /// The same view for the film renderer.
    #[allow(dead_code, reason = "for viewport-vs-film comparisons")]
    pub fn film_camera(&self) -> crate::film::Camera {
        crate::film::Camera { eye: self.eye, target: self.target, up: Vec3::Y, fov_deg: self.fov, ortho: None }
    }

    /// The viewport callback an agent screenshot of this view would use;
    /// `mask` draws flat white fills.
    pub fn callback(&self, width: u32, height: u32, mask: bool) -> ViewportCallback {
        crate::app::scene_callback_of(&self.scene, &self.meshes, self.view_proj(width, height), self.eye, mask, !mask)
    }

    /// Renders the shaded view on the background colour, checked.
    pub fn render(&self, rs: &egui_wgpu::RenderState, width: u32, height: u32) -> Result<egui::ColorImage, String> {
        render(rs, &self.callback(width, height, false), width, height, BACKGROUND)
    }
}

/// A pixel rectangle, `[x0, y0, x1, y1)`.
pub type Rect = [usize; 4];

/// The whole of `img`.
pub fn full(img: &egui::ColorImage) -> Rect {
    [0, 0, img.size[0], img.size[1]]
}

/// An image from tightly packed 8-bit RGB (the film tests' buffers).
pub fn from_rgb(width: usize, height: usize, rgb: &[u8]) -> egui::ColorImage {
    assert_eq!(rgb.len(), width * height * 3);
    egui::ColorImage::new([width, height], rgb.chunks_exact(3).map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect())
}

/// Per-channel RGB differences (0..255) of two same-sized images.
fn diffs<'a>(a: &'a egui::ColorImage, b: &'a egui::ColorImage) -> impl Iterator<Item = u8> + 'a {
    assert_eq!(a.size, b.size, "images differ in size");
    a.pixels.iter().zip(&b.pixels).flat_map(|(p, q)| [p.r().abs_diff(q.r()), p.g().abs_diff(q.g()), p.b().abs_diff(q.b())])
}

/// Mean absolute RGB difference, in 0..255 steps.
pub fn mean_abs_diff(a: &egui::ColorImage, b: &egui::ColorImage) -> f64 {
    let n = (a.pixels.len() * 3).max(1) as f64;
    diffs(a, b).map(f64::from).sum::<f64>() / n
}

/// Largest RGB difference of any channel of any pixel, in 0..255 steps.
pub fn max_abs_diff(a: &egui::ColorImage, b: &egui::ColorImage) -> u8 {
    diffs(a, b).max().unwrap_or(0)
}

/// Peak signal-to-noise ratio over RGB in dB; infinite for equal images.
pub fn psnr(a: &egui::ColorImage, b: &egui::ColorImage) -> f64 {
    let n = (a.pixels.len() * 3).max(1) as f64;
    let mse = diffs(a, b).map(|d| f64::from(d).powi(2)).sum::<f64>() / n;
    if mse == 0.0 { f64::INFINITY } else { 20.0 * 255f64.log10() - 10.0 * mse.log10() }
}

fn region(img: &egui::ColorImage, [x0, y0, x1, y1]: Rect) -> impl Iterator<Item = egui::Color32> + '_ {
    let [w, h] = img.size;
    assert!(x0 < x1 && x1 <= w && y0 < y1 && y1 <= h, "rect {:?} is outside a {w}x{h} image", [x0, y0, x1, y1]);
    (y0..y1).flat_map(move |y| img.pixels[y * w + x0..y * w + x1].iter().copied())
}

/// Mean RGB of the pixels in `rect`, in 0..255.
pub fn region_mean(img: &egui::ColorImage, rect: Rect) -> [f64; 3] {
    let (mut sum, mut n) = ([0.0; 3], 0.0);
    for p in region(img, rect) {
        sum[0] += f64::from(p.r());
        sum[1] += f64::from(p.g());
        sum[2] += f64::from(p.b());
        n += 1.0;
    }
    sum.map(|s| s / n)
}

/// Standard deviation of the grey level (mean of RGB) in `rect`, in 0..255.
pub fn std_dev(img: &egui::ColorImage, rect: Rect) -> f64 {
    let grey: Vec<f64> = region(img, rect).map(|p| (f64::from(p.r()) + f64::from(p.g()) + f64::from(p.b())) / 3.0).collect();
    let mean = grey.iter().sum::<f64>() / grey.len() as f64;
    (grey.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / grey.len() as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 96;
    const H: u32 = 72;

    fn solid(w: usize, h: usize, c: [u8; 3]) -> egui::ColorImage {
        egui::ColorImage::new([w, h], vec![egui::Color32::from_rgb(c[0], c[1], c[2]); w * h])
    }

    #[test]
    fn metrics_measure_known_differences() {
        let a = solid(8, 4, [100, 100, 100]);
        assert_eq!(mean_abs_diff(&a, &a), 0.0);
        assert_eq!(max_abs_diff(&a, &a), 0);
        assert!(psnr(&a, &a).is_infinite());
        assert_eq!(std_dev(&a, full(&a)), 0.0);

        // Right half brighter by 10 in red only.
        let mut b = a.clone();
        for y in 0..4 {
            for x in 4..8 {
                b.pixels[y * 8 + x] = egui::Color32::from_rgb(110, 100, 100);
            }
        }
        assert_eq!(max_abs_diff(&a, &b), 10);
        assert!((mean_abs_diff(&a, &b) - 10.0 / 6.0).abs() < 1e-9);
        // mse = 100 / 6.
        assert!((psnr(&a, &b) - (20.0 * 255f64.log10() - 10.0 * (100.0f64 / 6.0).log10())).abs() < 1e-9);
        assert_eq!(region_mean(&b, [0, 0, 4, 4]), [100.0; 3]);
        assert_eq!(region_mean(&b, [4, 0, 8, 4]), [110.0, 100.0, 100.0]);
        assert_eq!(region_mean(&b, full(&b)), [105.0, 100.0, 100.0]);
        // Grey levels 100 and 103.33 in equal parts: half the gap.
        assert!((std_dev(&b, full(&b)) - 10.0 / 6.0).abs() < 1e-9);

        let c = from_rgb(2, 1, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(c.pixels[1], egui::Color32::from_rgb(4, 5, 6));
    }

    #[test]
    fn checked_reports_validation_errors() {
        let Some(rs) = headless() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let err = checked(&rs, || {
            let _ = rs.device.create_buffer(&wgpu::BufferDescriptor { label: Some("bad"), size: 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE, mapped_at_creation: false });
        });
        assert!(err.is_err(), "a MAP_READ | MAP_WRITE buffer should fail validation");
        assert!(checked(&rs, || ()).is_ok());
    }

    #[test]
    fn fixture_renders_on_both_formats() {
        let mut images = Vec::new();
        for format in FORMATS {
            let Some(rs) = headless_with(format) else {
                eprintln!("no GPU adapter: skipped");
                return;
            };
            assert_eq!(gamma_encodes(&rs), !format.is_srgb(), "{format:?}");
            let img = Fixture::new().render(&rs, W, H).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(img.size, [W as usize, H as usize]);
            // The sky above the scene is the clear colour (an sRGB target
            // round-trips it through pow 2.2 and the sRGB curve).
            let top = region_mean(&img, [0, 0, W as usize, 3]);
            for (got, want) in top.iter().zip(BACKGROUND) {
                assert!((got - f64::from(want)).abs() <= 2.0, "{format:?}: sky {top:?} vs {BACKGROUND:?}");
            }
            // The objects in the middle are lit and varied, not background.
            let middle = [W as usize / 4, H as usize / 4, W as usize * 3 / 4, H as usize * 3 / 4];
            let mean = region_mean(&img, middle);
            assert!(mean.iter().zip(BACKGROUND).any(|(m, b)| (m - f64::from(b)).abs() > 8.0), "{format:?}: middle {mean:?} looks empty");
            assert!(std_dev(&img, middle) > 8.0, "{format:?}: middle is flat");
            images.push(img);
        }
        // pow(1/2.2) and the sRGB curve differ in the darks, and the window
        // blends in gamma space, so the two only roughly agree.
        let (mad, db) = (mean_abs_diff(&images[0], &images[1]), psnr(&images[0], &images[1]));
        eprintln!("srgb vs linear target: mad {mad:.2}, psnr {db:.1} dB");
        assert!(mad < 4.0 && db > 32.0, "srgb vs linear target: mad {mad:.2}, psnr {db:.1} dB");
    }

    #[test]
    fn fixture_renders_deterministically() {
        for format in FORMATS {
            let Some(rs) = headless_with(format) else {
                eprintln!("no GPU adapter: skipped");
                return;
            };
            let fixture = Fixture::new();
            let a = fixture.render(&rs, W, H).unwrap();
            let b = fixture.render(&rs, W, H).unwrap();
            assert!(max_abs_diff(&a, &b) <= 1, "{format:?}: two renders differ by {}", max_abs_diff(&a, &b));
        }
    }

    #[test]
    fn concurrent_render_images_keep_their_own_globals() {
        // Every thread draws the mask in its own colour on its own clear;
        // before render_image held the renderer through the submit, one
        // thread's draw could pick up another's instances.
        const THREADS: usize = 4;
        for format in FORMATS {
            let Some(rs) = headless_with(format) else {
                eprintln!("no GPU adapter: skipped");
                return;
            };
            let fixture = Fixture::new();
            let jobs: Vec<(ViewportCallback, [u8; 3])> = (0..THREADS)
                .map(|t| {
                    let mut cb = fixture.callback(W, H, true);
                    let c = [[0.9, 0.1, 0.1], [0.1, 0.9, 0.1], [0.1, 0.1, 0.9], [0.9, 0.9, 0.1]][t];
                    cb.fills.iter_mut().for_each(|(_, i)| *i = i.with_color(c));
                    (cb, [t as u8 * 40, 0, 255 - t as u8 * 40])
                })
                .collect();
            let alone: Vec<_> = jobs.iter().map(|(cb, clear)| render(&rs, cb, W, H, *clear).unwrap()).collect();
            let together = checked(&rs, || {
                std::thread::scope(|s| {
                    let handles: Vec<_> = jobs.iter().map(|(cb, clear)| s.spawn(|| (0..6).map(|_| super::super::render_image(&rs, cb, W, H, *clear).unwrap()).collect::<Vec<_>>())).collect();
                    handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
                })
            })
            .unwrap();
            for (t, runs) in together.iter().enumerate() {
                for img in runs {
                    assert!(max_abs_diff(img, &alone[t]) <= 1, "{format:?}: thread {t} drew something else (max diff {})", max_abs_diff(img, &alone[t]));
                }
            }
        }
    }
}
