//! Render passes for compositing (sculpt Stage 8b), like ZBrush's BPR
//! passes: depth, normal, albedo, ambient occlusion, shadow, mask and
//! emission, each from the camera's first hit of the same meshes, hair,
//! floor and lens the film picture uses.
//!
//! They come from their own pass on the CPU (a few anti-aliasing samples
//! per pixel, seeded per pixel so they are the same every time), whichever
//! tracer made the picture: both tracers trace the same triangles, and
//! the passes need no light transport, only first hits and short
//! visibility rays. Surfaces are shaded as the tracer shades them
//! (`Scene::surface`: normal maps, detail, smart layers), so the albedo and
//! normal passes match the picture.

use std::sync::Arc;

use glam::Vec3;

use super::{Camera, Fiber, HAIR, Look, Meshes, Rng, Scene, Settings, View};

/// One pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pass {
    /// Distance from the camera along its view axis.
    Depth,
    /// The shading normal (world or camera space).
    Normal,
    /// Surface color without light (linear); the floor's gray too.
    Albedo,
    /// How open each point is: 1 = nothing near above it.
    Ao,
    /// How much of the lights' light is blocked: 1 = in full shadow.
    Shadow,
    /// Where objects cover the picture (their silhouette, anti-aliased),
    /// as ZBrush's BPR mask: 1 = object, 0 = floor or background.
    Mask,
    /// Light the surfaces give off (linear).
    Emission,
}

impl Pass {
    pub const ALL: [Pass; 7] = [Pass::Depth, Pass::Normal, Pass::Albedo, Pass::Ao, Pass::Shadow, Pass::Mask, Pass::Emission];

    pub fn name(self) -> &'static str {
        match self {
            Pass::Depth => "depth",
            Pass::Normal => "normal",
            Pass::Albedo => "albedo",
            Pass::Ao => "ao",
            Pass::Shadow => "shadow",
            Pass::Mask => "mask",
            Pass::Emission => "emission",
        }
    }

    pub fn parse(name: &str) -> Option<Pass> {
        Pass::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// How the passes are sampled.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Anti-aliasing samples per pixel (1..64).
    pub samples: u32,
    /// Occlusion rays per sample.
    pub ao_rays: u32,
    /// How far occlusion looks (world units).
    pub ao_distance: f32,
    /// Normals in camera space (x right, y up, z toward the camera)
    /// instead of world space.
    pub camera_normals: bool,
}

/// Depth written where nothing was hit (as Blender writes).
pub const NO_DEPTH: f32 = 1e10;

/// One view's passes, row by row from the top. Coverage-weighted values
/// (albedo, emission, mask) are premultiplied by how much of the pixel
/// hits something; the rest average the samples that hit (depth: the
/// nearest hit; background NO_DEPTH, normal zero, ao 1, shadow 0).
pub struct Passes {
    pub width: usize,
    pub height: usize,
    pub depth: Vec<f32>,
    pub normal: Vec<Vec3>,
    pub albedo: Vec<Vec3>,
    pub ao: Vec<f32>,
    pub shadow: Vec<f32>,
    pub mask: Vec<f32>,
    pub emission: Vec<Vec3>,
}

/// One pixel's sums.
#[derive(Clone, Copy, Default)]
struct Pixel {
    depth: f32,
    normal: Vec3,
    albedo: Vec3,
    ao: f32,
    shadow: f32,
    mask: f32,
    emission: Vec3,
}

/// The passes of one camera's view of `meshes` (and `fibers`), at the
/// size and with the floor of `s`. Stops early (leaving the rest empty)
/// when `progress` is cancelled.
pub fn render(meshes: &Meshes, fibers: &[Fiber], cam: &Camera, s: Settings, look: Arc<Look>, want: &[Pass], o: Options, progress: &crate::progress::Progress) -> Passes {
    let scene = Scene::new(meshes, fibers, cam.eye, s.floor, look.clone());
    let view = View::new(scene, cam, s);
    let (w, h) = (view.w, view.h);
    let samples = o.samples.clamp(1, 64);
    let needs = |p: Pass| want.contains(&p);
    let (ao, shadow) = (needs(Pass::Ao), needs(Pass::Shadow));
    // Lights for the shadow pass: the look's, else the studio key.
    let fallback = [super::lighting::Light::Sun { direction: super::KEY_DIR.normalize(), size: super::KEY_RADIUS, power: Vec3::ONE }];
    let lights: &[super::lighting::Light] = if look.lights.is_empty() { &fallback } else { &look.lights };
    let mut rows: Vec<Vec<Pixel>> = vec![Vec::new(); h];
    let view = &view;
    crate::clay::par_chunks(&mut rows, 1, |y, row| {
        if progress.cancelled() {
            return;
        }
        let row = &mut row[0];
        *row = vec![Pixel::default(); w];
        for (x, px) in row.iter_mut().enumerate() {
            let (mut hits, mut near) = (0u32, f32::MAX);
            for k in 0..samples {
                let mut rng = Rng::new((y * w + x) as u64 + 1, k as u64 + 101);
                // Stratified over the pixel.
                let n = (samples as f32).sqrt().ceil() as u32;
                let (sx, sy) = ((k % n) as f32 + rng.f(), ((k / n) % n) as f32 + rng.f());
                let (orig, dir) = view.ray(x as f32 + sx / n as f32, y as f32 + sy / n as f32, rng.v2());
                let Some((t, hit)) = view.scene.trace(orig, dir, true) else { continue };
                let p = orig + dir * t;
                let (ng, n, object) = match hit {
                    None => {
                        px.albedo += Vec3::splat(super::FLOOR_ALBEDO);
                        (Vec3::Y, Vec3::Y, false)
                    }
                    Some(hh) if hh.tri & HAIR != 0 => {
                        let seg = view.scene.hair_at(&hh).expect("a hair hit");
                        let facing = (-dir - seg.tangent * seg.tangent.dot(-dir)).normalize_or(-dir);
                        px.albedo += seg.color;
                        (facing, facing, true)
                    }
                    Some(hh) => {
                        let (_, ng, n, surf, _, optics) = view.scene.surface(orig, dir, t, &hh);
                        px.albedo += surf.albedo;
                        px.emission += surf.albedo * optics.emission + optics.emitted;
                        (ng, n, true)
                    }
                };
                hits += 1;
                if object {
                    px.mask += 1.0;
                }
                near = near.min((p - view.eye).dot(view.forward));
                px.normal += n;
                let from = p + ng * 2e-4;
                if ao {
                    let frame = super::shade::Frame::new(ng);
                    let open = (0..o.ao_rays).filter(|_| !view.scene.blocked(from, frame.to_world(super::shade::cosine(rng.v2())), o.ao_distance)).count();
                    px.ao += open as f32 / o.ao_rays.max(1) as f32;
                }
                if shadow {
                    let (mut blocked, mut total) = (0.0, 0.0);
                    for light in lights {
                        let (l, power, far) = light.sample(p, rng.v2());
                        let weight = power.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-6);
                        total += weight;
                        if l.dot(n) <= 0.0 || l.dot(ng) <= 0.0 || view.scene.blocked(from, l, far) {
                            blocked += weight;
                        }
                    }
                    px.shadow += blocked / total;
                }
            }
            let (hit_share, per_hit) = (1.0 / samples as f32, 1.0 / hits.max(1) as f32);
            px.depth = if hits > 0 { near } else { NO_DEPTH };
            px.normal = px.normal.normalize_or_zero();
            if o.camera_normals {
                px.normal = Vec3::new(px.normal.dot(view.right), px.normal.dot(view.up), -px.normal.dot(view.forward));
            }
            px.ao = if hits > 0 { px.ao * per_hit } else { 1.0 };
            px.shadow *= per_hit;
            px.mask *= hit_share;
            px.albedo *= hit_share;
            px.emission *= hit_share;
        }
    });
    let all: Vec<Pixel> = rows.into_iter().flat_map(|r| if r.is_empty() { vec![Pixel::default(); w] } else { r }).collect();
    Passes {
        width: w,
        height: h,
        depth: all.iter().map(|p| p.depth).collect(),
        normal: all.iter().map(|p| p.normal).collect(),
        albedo: all.iter().map(|p| p.albedo).collect(),
        ao: all.iter().map(|p| p.ao).collect(),
        shadow: all.iter().map(|p| p.shadow).collect(),
        mask: all.iter().map(|p| p.mask).collect(),
        emission: all.iter().map(|p| p.emission).collect(),
    }
}

impl Passes {
    /// The nearest and farthest depth hit (None: nothing was hit).
    pub fn depth_range(&self) -> Option<(f32, f32)> {
        let hits = self.depth.iter().filter(|d| **d < NO_DEPTH);
        let (lo, hi) = hits.fold((f32::MAX, f32::MIN), |(a, b), d| (a.min(*d), b.max(*d)));
        (lo <= hi).then_some((lo, hi))
    }

    /// A pass as an 8-bit RGB picture, for looking at (the reply's sheet):
    /// depth near = white, normals as 0.5 + 0.5 n, colors in sRGB.
    pub fn preview(&self, pass: Pass) -> Vec<[u8; 3]> {
        let gray = |v: f32| { let c = (v.clamp(0.0, 1.0) * 255.0).round() as u8; [c, c, c] };
        let color = |c: Vec3| { let s = srgb8(c); [s[0], s[1], s[2]] };
        match pass {
            Pass::Depth => {
                let (lo, hi) = self.depth_range().unwrap_or((0.0, 1.0));
                self.depth.iter().map(|d| if *d >= NO_DEPTH { [0; 3] } else { gray(if hi > lo { (hi - d) / (hi - lo) } else { 1.0 }) }).collect()
            }
            Pass::Normal => self.normal.iter().map(|n| if *n == Vec3::ZERO { [0; 3] } else { ((*n * 0.5 + 0.5) * 255.0).round().to_array().map(|v| v as u8) }).collect(),
            Pass::Albedo => self.albedo.iter().map(|c| color(*c)).collect(),
            Pass::Emission => self.emission.iter().map(|c| color(*c)).collect(),
            Pass::Ao => self.ao.iter().map(|v| gray(*v)).collect(),
            Pass::Shadow => self.shadow.iter().map(|v| gray(*v)).collect(),
            Pass::Mask => self.mask.iter().map(|v| gray(*v)).collect(),
        }
    }
}

/// Linear color to 8-bit sRGB.
pub fn srgb8(c: Vec3) -> [u8; 3] {
    c.to_array().map(|v| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::film::Item;
    use glam::Mat4;
    use std::time::Duration;

    fn settings(size: usize, floor: bool) -> Settings {
        Settings { width: size, height: size, samples: 1, bounces: 1, time_limit: Duration::from_secs(10), floor, exposure: 1.0, denoise: false }
    }

    fn options() -> Options {
        Options { samples: 4, ao_rays: 8, ao_distance: 0.3, camera_normals: false }
    }

    /// A finely tessellated unit sphere (a clay ball, smooth normals).
    fn ball() -> Arc<crate::mesh::Mesh> {
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 1.0 }], "resolution": 96 })).unwrap();
        Arc::new(clay.build_mesh())
    }

    /// The ray through a pixel center of `cam` (pinhole, as `View::ray`).
    fn pixel_ray(cam: &Camera, size: usize, x: usize, y: usize) -> (Vec3, Vec3) {
        let forward = (cam.target - cam.eye).normalize();
        let right = forward.cross(cam.up).normalize();
        let up = right.cross(forward);
        let half = (cam.fov_deg.to_radians() * 0.5).tan();
        let sx = ((x as f32 + 0.5) / size as f32 * 2.0 - 1.0) * half;
        let sy = (1.0 - (y as f32 + 0.5) / size as f32 * 2.0) * half;
        (cam.eye, (forward + right * sx + up * sy).normalize())
    }

    /// Where a ray meets the unit sphere at the origin.
    fn sphere_hit(o: Vec3, d: Vec3) -> Option<Vec3> {
        let b = o.dot(d);
        let disc = b * b - (o.length_squared() - 1.0);
        (disc >= 0.0).then(|| o + d * (-b - disc.sqrt()))
    }

    /// Stage 8b acceptance: on a sphere the normal pass is within 2
    /// degrees of the true normal on average, the mask pass covers the
    /// true silhouette (IoU >= 0.98), and depth grows away from the camera.
    #[test]
    fn passes_match_a_sphere() {
        let item = Item { mesh: ball(), world: Mat4::IDENTITY, tint: [0.8, 0.4, 0.2], surface: crate::material::Material::default().surface(), material: Default::default() };
        let meshes = Meshes::new(vec![item]);
        let cam = Camera { eye: Vec3::new(0.0, 0.0, 4.0), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 40.0, ortho: None };
        let size = 128;
        let mut look = Look::default();
        look.device = crate::film::look::Device::Cpu;
        let t = std::time::Instant::now();
        let p = render(&meshes, &[], &cam, settings(size, false), Arc::new(look), &Pass::ALL, options(), &Default::default());
        eprintln!("passes 128x128, all seven, 4 samples: {:.3} s", t.elapsed().as_secs_f32());
        let (mut angle, mut counted, mut inter, mut union) = (0.0f64, 0, 0, 0);
        for y in 0..size {
            for x in 0..size {
                let i = y * size + x;
                let (o, d) = pixel_ray(&cam, size, x, y);
                let truth = sphere_hit(o, d);
                let covered = p.mask[i] >= 0.5;
                inter += (covered && truth.is_some()) as usize;
                union += (covered || truth.is_some()) as usize;
                // Whole pixels inside the silhouette (not its edge).
                if let Some(hit) = truth.filter(|_| p.mask[i] == 1.0) {
                    let n = p.normal[i];
                    angle += n.dot(hit.normalize()).clamp(-1.0, 1.0).acos().to_degrees() as f64;
                    counted += 1;
                    // Depth: the hit's distance along the view axis (where
                    // it changes slowly across the pixel).
                    if hit.z > 0.8 {
                        assert!((p.depth[i] - (4.0 - hit.z)).abs() < 0.02, "depth {} vs {}", p.depth[i], 4.0 - hit.z);
                    }
                }
            }
        }
        let mean = angle / counted as f64;
        let iou = inter as f64 / union as f64;
        eprintln!("normal pass: mean angle error {mean:.3} deg over {counted} pixels; mask IoU {iou:.4}");
        assert!(counted > 5000 && mean <= 2.0, "mean normal error {mean} deg");
        assert!(iou >= 0.98, "mask IoU {iou}");
        // A lit ball in the studio: its far side is in shadow, nothing
        // near it occludes its front, and it glows nowhere.
        let center = (size / 2) * size + size / 2;
        assert!(p.shadow[center] < 0.5 && p.ao[center] > 0.95, "shadow {} ao {}", p.shadow[center], p.ao[center]);
        assert!(p.emission.iter().all(|e| *e == Vec3::ZERO));
        let albedo = p.albedo[center];
        assert!(albedo.x > albedo.y && albedo.y > albedo.z && albedo.z > 0.0, "{albedo}");
        // Background: no depth, empty mask.
        assert_eq!((p.depth[0], p.mask[0]), (NO_DEPTH, 0.0));
    }

    /// The pixel `p` falls in (pinhole, square picture).
    fn pixel_of(cam: &Camera, size: usize, p: Vec3) -> usize {
        let forward = (cam.target - cam.eye).normalize();
        let right = forward.cross(cam.up).normalize();
        let up = right.cross(forward);
        let half = (cam.fov_deg.to_radians() * 0.5).tan();
        let d = p - cam.eye;
        let (sx, sy) = (d.dot(right) / d.dot(forward) / half, d.dot(up) / d.dot(forward) / half);
        let (x, y) = (((sx + 1.0) * 0.5 * size as f32) as usize, ((1.0 - sy) * 0.5 * size as f32) as usize);
        y * size + x
    }

    /// Depth along a floor seen at a slant rises row by row toward the
    /// horizon; a ball on the floor shades it (ao) and shadows it.
    #[test]
    fn depth_is_monotone_and_the_floor_catches_shadow_and_occlusion() {
        let small = Mat4::from_scale_rotation_translation(Vec3::splat(0.3), glam::Quat::IDENTITY, Vec3::new(0.0, 0.3, 0.0));
        let item = Item { mesh: ball(), world: small, tint: [1.0; 3], surface: crate::material::Material::default().surface(), material: Default::default() };
        let meshes = Meshes::new(vec![item]);
        let cam = Camera { eye: Vec3::new(0.0, 1.2, 3.0), target: Vec3::new(0.0, 0.2, 0.0), up: Vec3::Y, fov_deg: 45.0, ortho: None };
        let size = 96;
        let mut look = Look::default();
        // One sun behind and above: the ball's shadow falls toward the camera.
        look.lights = vec![crate::film::lighting::Light::Sun { direction: Vec3::new(0.0, 1.0, -1.0).normalize(), size: 0.01, power: Vec3::ONE }];
        let p = render(&meshes, &[], &cam, settings(size, true), Arc::new(look), &[Pass::Depth, Pass::Ao, Pass::Shadow, Pass::Mask, Pass::Albedo], Options { ao_distance: 0.6, ..options() }, &Default::default());
        // A column off to the side sees only floor: depth falls as rows go
        // down (nearer the camera).
        let x = 8;
        let column: Vec<f32> = (0..size).map(|y| p.depth[y * size + x]).filter(|d| *d < NO_DEPTH).collect();
        assert!(column.len() > size / 3, "{} floor pixels", column.len());
        assert!(column.windows(2).all(|w| w[1] < w[0]), "depth not monotone: {column:?}");
        // The floor in front of the ball is in its shadow and occluded by
        // it; open floor to the side is neither.
        let (under, open) = (pixel_of(&cam, size, Vec3::new(0.0, 0.0, 0.25)), pixel_of(&cam, size, Vec3::new(0.8, 0.0, 1.0)));
        assert_eq!((p.mask[under], p.mask[open]), (0.0, 0.0));
        // The floor has the traced floor's gray in the albedo pass.
        assert!((p.albedo[open] - Vec3::splat(crate::film::FLOOR_ALBEDO)).abs().max_element() < 1e-5, "floor albedo {}", p.albedo[open]);
        assert!(p.shadow[under] > 0.9 && p.shadow[open] < 0.1, "shadow under {} open {}", p.shadow[under], p.shadow[open]);
        assert!(p.ao[under] < p.ao[open] - 0.1, "ao under {} open {}", p.ao[under], p.ao[open]);
    }
}
