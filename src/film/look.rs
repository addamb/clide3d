//! Camera optics and scene-linear finishing, before display conversion.
use std::sync::Arc;

use glam::{Mat3, Vec2, Vec3};
use serde_json::Value;

use super::{Camera, lighting::{Environment, Light, number, vector}};

pub struct Look {
    pub environment: Arc<Environment>,
    pub lights: Vec<Light>,
    pub background: bool,
    /// Mist in the air (see `lighting::Fog`).
    pub fog: Option<super::lighting::Fog>,
    pub volumes: Vec<crate::fire::Volume>,
    pub camera: CameraOptions,
    /// Where to trace: the GPU when it can (auto), or only one of them.
    pub device: Device,
    bloom: f32,
    pub post: super::post::Post,
    bloom_threshold: f32,
    vignette: f32,
    balance: Vec3,
    tonemap: ToneMap,
}

#[derive(Clone, Copy)]
pub struct CameraOptions {
    pub fov: Option<f32>,
    pub f_stop: Option<f32>,
    pub focus_distance: Option<f32>,
    pub focus_point: Option<Vec3>,
    pub eye: Option<Vec3>,
    pub target: Option<Vec3>,
    pub sensor_height_mm: f32,
    pub blades: u32,
    pub blade_rotation: f32,
    pub exposure: f32,
}

impl Default for CameraOptions {
    fn default() -> Self { Self { fov: None, f_stop: None, focus_distance: None, focus_point: None, eye: None, target: None, sensor_height_mm: 24.0, blades: 0, blade_rotation: 0.0, exposure: 1.0 } }
}

/// Resolve presets first; explicit values (including false/null) override them.
pub fn resolved_args(args: &Value) -> Result<Value, String> {
    let name = args["look"].as_str().unwrap_or("neutral");
    let mut base = match name {
        "neutral" => serde_json::json!({}),
        "trailer" | "modern_warfare" => serde_json::json!({"environment":{"preset":"storm","intensity":0.65},"lights":[{"type":"sun","direction":[-0.5,0.4,-0.8],"color":[1,0.75,0.43],"intensity":3,"size":1},{"type":"sun","direction":[0.4,0.8,0.8],"color":[0.48,0.65,1],"intensity":0.5,"size":15}],"background":"environment","camera":{"focal_length_mm":45,"aperture":4,"dof":{"blades":7}},"grade":{"look":"modern_warfare","contrast":1.16},"tonemap":"agx","bloom":{"strength":0.08,"threshold":1.5,"streaks":0.15,"dirt":0.12},"vignette":0.22,"grain":0.012,"sharpen":0.12,"fog":{"density":0.025,"height":0.25,"falloff":1.2,"anisotropy":0.55,"distance":50,"noise":0.7,"noise_scale":0.35}}),
        "desert_heat" => serde_json::json!({"environment":"golden_hour","lights":"none","background":"environment","grade":{"look":"desert_heat"},"camera":{"focal_length_mm":65,"aperture":5.6},"tonemap":"agx","bloom":0.06,"grain":0.008,"vignette":0.15,"fog":{"density":0.018,"color":[0.95,0.85,0.65],"noise":0.5,"falloff":4}}),
        "night_ops" => serde_json::json!({"environment":"night","lights":[{"type":"sun","direction":[-0.6,0.5,-0.4],"color":[0.25,0.55,0.9],"intensity":1.5,"size":2}],"background":"environment","camera":{"focal_length_mm":50,"aperture":2},"grade":{"look":"night_ops"},"tonemap":"agx","bloom":{"strength":0.12,"streaks":0.3},"grain":0.015,"vignette":0.3,"fog":{"density":0.02,"height":0.2,"noise":0.6,"falloff":1.2,"anisotropy":0.5}}),
        "cinematic_teal_orange" => serde_json::json!({"grade":{"look":"cinematic_teal_orange"},"tonemap":"agx","bloom":0.06,"vignette":0.15,"grain":0.008,"camera":{"focal_length_mm":50,"aperture":2.8}}),
        _ => return Err("look must be neutral, trailer, modern_warfare, desert_heat, night_ops or cinematic_teal_orange".into()),
    };
    if !args.is_object() { return Err("render_film options must be an object".into()); }
    if !args["look"].is_null() && !args["look"].is_string() { return Err("look must be a preset name".into()); }
    fn merge(a: &mut Value, b: &Value) {
        if let (Some(dst), Some(src)) = (a.as_object_mut(), b.as_object()) {
            for (k, v) in src { merge(dst.entry(k).or_insert(Value::Null), v); }
        } else { *a = b.clone(); }
    }
    merge(&mut base, args);
    Ok(base)
}

#[derive(Clone, Copy)]
enum ToneMap { Aces, Agx, Filmic }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device { Auto, Gpu, Cpu }

impl Default for Look {
    fn default() -> Self {
        Self { environment: Arc::new(Environment::preset("studio").unwrap()),
            lights: super::LIGHTS.into_iter().map(|(direction, size, power)| Light::Sun { direction: direction.normalize(), size, power }).collect(),
            background: false, fog: None, volumes: Vec::new(), camera: CameraOptions::default(), device: Device::Auto, post: super::post::Post::default(), bloom: 0.0, bloom_threshold: 1.0,
            vignette: 0.0, balance: Vec3::ONE, tonemap: ToneMap::Aces }
    }
}

impl Look {
    pub fn parse(args: &Value) -> Result<Self, String> {
        let resolved = resolved_args(args)?;
        let args = &resolved;
        let mut look = Self { environment: Arc::new(Environment::parse(&args["environment"])?), ..Self::default() };
        look.background = match args.get("background") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(v)) => *v,
            Some(Value::String(v)) if v == "environment" => true,
            Some(Value::String(v)) if v == "studio" => false,
            _ => return Err("background must be environment, studio or a boolean".into()),
        };
        match args.get("lights") {
            None | Some(Value::Null) => {},
            Some(Value::String(v)) if v == "studio" => {},
            Some(Value::String(v)) if v == "none" => look.lights.clear(),
            Some(Value::Array(v)) if v.len() <= 64 => look.lights = v.iter().map(Light::parse).collect::<Result<_, _>>()?,
            _ => return Err("lights must be studio, none or an array of at most 64 lights".into()),
        }
        if let Some(v) = args.get("extra_lights") {
            let v = v.as_array().filter(|a| a.len() <= 64).ok_or("extra_lights must be an array of at most 64 lights")?;
            look.lights.extend(v.iter().map(Light::parse).collect::<Result<Vec<_>, _>>()?);
        }
        look.fog = super::lighting::Fog::parse(&args["fog"])?;
        look.camera = CameraOptions::parse(&args["camera"])?;
        look.device = match args["device"].as_str() {
            None if args["device"].is_null() => Device::Auto,
            Some("auto") => Device::Auto,
            Some("gpu") => Device::Gpu,
            Some("cpu") => Device::Cpu,
            _ => return Err("device must be auto, gpu or cpu".into()),
        };
        look.post = super::post::Post::parse(args)?;
        look.bloom = match &args["bloom"] {
            Value::Bool(true) => 0.08,
            Value::Bool(false) | Value::Null => 0.0,
            Value::Object(_) => number(&args["bloom"], "strength", 0.08, 0.0, 1.0)?,
            _ => number(args, "bloom", 0.0, 0.0, 1.0)?,
        };
        look.bloom_threshold = number(&args["bloom"], "threshold", 1.0, 0.0, 100.0)?;
        look.vignette = number(args, "vignette", 0.0, 0.0, 1.0)?;
        look.balance = match &args["white_balance"] {
            Value::Null => Vec3::ONE,
            Value::Array(_) => {
                let v = vector(args, "white_balance", None)?;
                if v.min_element() <= 0.0 || v.max_element() > 10.0 { return Err("white_balance gains must be in (0,10]".into()); }
                v
            }
            _ => {
                let k = number(args, "white_balance", 6500.0, 2000.0, 12000.0)?;
                // Planckian RGB approximation, normalized to daylight.
                temperature(6500.0) / temperature(k).max(Vec3::splat(0.05))
            }
        };
        look.tonemap = match args["tonemap"].as_str() {
            None if args["tonemap"].is_null() => ToneMap::Aces,
            Some("aces") => ToneMap::Aces,
            Some("agx") => ToneMap::Agx,
            Some("filmic") => ToneMap::Filmic,
            _ => return Err("tonemap must be aces, agx or filmic".into()),
        };
        Ok(look)
    }

    pub fn finish(&self, mut color: Vec<Vec3>, w: usize, h: usize, exposure: f32) -> Vec<[u8; 3]> {
        for c in &mut color { *c = c.max(Vec3::ZERO) * self.balance * exposure * self.camera.exposure; }
        color = self.post.optics(&color, w, h);
        if self.bloom > 0.0 {
            let bright: Vec<_> = color.iter().map(|c| {
                let l = c.max_element();
                *c * ((l - self.bloom_threshold).max(0.0) / l.max(1e-6))
            }).collect();
            color = self.post.glare(color, &bright, w, h, self.bloom, (self.camera.blades, self.camera.blade_rotation));
        }
        let mut mapped: Vec<Vec3> = color.iter().enumerate().map(|(i, c)| {
            let x = ((i % w) as f32 + 0.5) / w as f32 * 2.0 - 1.0;
            let y = ((i / w) as f32 + 0.5) / h as f32 * 2.0 - 1.0;
            let c = (*c * (1.0 - self.vignette * (0.5 * (x * x + y * y)).powi(2))).max(Vec3::ZERO);
            let mapped = match self.tonemap {
                ToneMap::Aces => {
                    let x = c * 0.6;
                    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(Vec3::ZERO, Vec3::ONE)
                }
                ToneMap::Filmic => {
                    // One shoulder for all channels keeps saturated light colored.
                    let peak = c.max_element();
                    c / (1.0 + peak)
                }
                ToneMap::Agx => agx(c),
            };
            self.post.grade(mapped.clamp(Vec3::ZERO, Vec3::ONE))
        }).collect();
        self.post.texture(&mut mapped, w, h);
        mapped.into_iter().map(|c| {
            let m = c.clamp(Vec3::ZERO, Vec3::ONE).powf(1.0 / 2.2) * 255.0;
            [m.x as u8, m.y as u8, m.z as u8]
        }).collect()
    }
}

impl CameraOptions {
    fn parse(v: &Value) -> Result<Self, String> {
        if !v.is_null() && !v.is_object() { return Err("camera must be an object".into()); }
        let dof = &v["dof"];
        if !dof.is_null() && !dof.is_object() { return Err("camera.dof must be an object".into()); }
        let sensor_height_mm = if !v["sensor_size_mm"].is_null() {
            let size: [f32;2] = serde_json::from_value(v["sensor_size_mm"].clone()).map_err(|_|"sensor_size_mm must be [width,height]")?;
            if size.iter().any(|x| !x.is_finite() || !(1.0..=100.0).contains(x)) { return Err("sensor dimensions must be 1..100 mm".into()); }
            if !v["sensor_height_mm"].is_null() { return Err("use sensor_size_mm or sensor_height_mm".into()); }
            size[1]
        } else { number(v,"sensor_height_mm",24.0,1.0,100.0)? };
        let fov = if !v["fov"].is_null() { Some(number(v, "fov", 30.0, 1.0, 150.0)?) }
            else if !v["focal_length_mm"].is_null() { Some((sensor_height_mm / (2.0 * number(v, "focal_length_mm", 50.0, 8.0, 1000.0)?)).atan().to_degrees() * 2.0) }
            else { None };
        let f_stop = if !dof["f_stop"].is_null() { Some(number(dof, "f_stop", 2.8, 0.7, 64.0)?) }
            else if !v["aperture"].is_null() { Some(number(v, "aperture", 2.8, 0.7, 64.0)?) } else { None };
        let focus_distance = if !dof["focus_distance"].is_null() { Some(number(dof, "focus_distance", 1.0, 0.0001, 100000.0)?) } else { None };
        let focus_point = if !dof["focus_point"].is_null() { Some(vector(dof, "focus_point", None)?) } else { None };
        if focus_distance.is_some() && focus_point.is_some() { return Err("use focus_distance or focus_point, not both".into()); }
        let eye = if !v["position"].is_null() { Some(vector(v, "position", None)?) } else { None };
        let target = if !v["target"].is_null() { Some(vector(v, "target", None)?) } else { None };
        let blades = number(dof, "blades", 0.0, 0.0, 16.0)?;
        if blades.fract() != 0.0 || (blades > 0.0 && blades < 3.0) { return Err("blades must be 0 (round) or an integer 3..16".into()); }
        let blade_rotation = number(dof, "blade_rotation", 0.0, -360.0, 360.0)?.to_radians();
        if !v["ev"].is_null() && (!v["iso"].is_null() || !v["shutter_seconds"].is_null()) { return Err("use ev or iso/shutter_seconds".into()); }
        let exposure = if !v["ev"].is_null() { 2.0f32.powf(12.0 - number(v, "ev", 12.0, -10.0, 30.0)?) }
            else if !v["iso"].is_null() || !v["shutter_seconds"].is_null() {
                let iso = number(v, "iso", 100.0, 1.0, 102400.0)?;
                let shutter = number(v, "shutter_seconds", 1.0 / 60.0, 0.00001, 30.0)?;
                let f = f_stop.unwrap_or(8.0);
                (iso / 100.0) * shutter * 4096.0 / (f * f)
            } else { 1.0 };
        Ok(Self { fov, f_stop, focus_distance, focus_point, eye, target, sensor_height_mm, blades: blades as u32, blade_rotation, exposure })
    }

    pub fn apply(&self, mut cam: Camera) -> Result<Camera, String> {
        if let Some(fov) = self.fov {
            // Preserve framing as the focal length changes (a dolly).
            cam.eye = cam.target + (cam.eye - cam.target) * ((cam.fov_deg * 0.5).to_radians().sin() / (fov * 0.5).to_radians().sin());
            cam.fov_deg = fov;
        }
        if let Some(eye) = self.eye { cam.eye = eye; }
        if let Some(target) = self.target { cam.target = target; }
        if cam.eye.distance_squared(cam.target) < 1e-10 { return Err("camera position and target must differ".into()); }
        if self.f_stop.is_some() && cam.ortho.is_some() { return Err("depth of field needs a perspective view".into()); }
        if self.focus_point.is_some_and(|p| (p - cam.eye).dot((cam.target - cam.eye).normalize()) <= 0.0) { return Err("focus_point must be in front of the camera".into()); }
        Ok(cam)
    }

    pub fn lens(&self, cam: &Camera) -> Lens {
        let focus = self.focus_distance.unwrap_or_else(|| self.focus_point.map_or(cam.eye.distance(cam.target), |p| (p - cam.eye).dot((cam.target - cam.eye).normalize())));
        // Full-frame 24 mm sensor height; scene units are meters.
        let focal = self.sensor_height_mm * 0.001 / (2.0 * (cam.fov_deg * 0.5).to_radians().tan());
        Lens { radius: self.f_stop.map_or(0.0, |f| focal / (2.0 * f)), focus, blades: self.blades, rotation: self.blade_rotation }
    }
}

#[derive(Clone, Copy)]
pub struct Lens { pub radius: f32, pub focus: f32, pub blades: u32, pub rotation: f32 }

impl Lens {
    /// Uniform area sampling of a disk or equal-area triangles of an aperture.
    pub fn sample(&self, u: Vec2) -> Vec2 {
        let phi = std::f32::consts::TAU * u.y;
        if self.blades == 0 { return Vec2::new(phi.cos(), phi.sin()) * u.x.sqrt(); }
        let sector = u.y * self.blades as f32;
        let angle = sector.floor() * std::f32::consts::TAU / self.blades as f32 + self.rotation;
        let next = angle + std::f32::consts::TAU / self.blades as f32;
        Vec2::new(angle.cos(), angle.sin()).lerp(Vec2::new(next.cos(), next.sin()), sector.fract()) * u.x.sqrt()
    }

    pub fn ray(&self, eye: Vec3, d: Vec3, forward: Vec3, right: Vec3, up: Vec3, u: Vec2) -> (Vec3, Vec3) {
        let aperture = self.sample(u);
        let o = eye + self.radius * (right * aperture.x + up * aperture.y);
        let point = eye + d * (self.focus / d.dot(forward).max(1e-6));
        (o, (point - o).normalize())
    }
}

fn temperature(k: f32) -> Vec3 {
    let t = k / 100.0;
    let r = if t <= 66.0 { 255.0 } else { 329.69873 * (t - 60.0).powf(-0.13320476) };
    let g = if t <= 66.0 { 99.4708 * t.ln() - 161.11957 } else { 288.12216 * (t - 60.0).powf(-0.07551485) };
    let b = if t >= 66.0 { 255.0 } else if t <= 19.0 { 0.0 } else { 138.51773 * (t - 10.0).ln() - 305.0448 };
    (Vec3::new(r, g, b).clamp(Vec3::ZERO, Vec3::splat(255.0)) / 255.0).powf(2.2)
}

fn agx(c: Vec3) -> Vec3 {
    let inset = Mat3::from_cols_array(&[0.84247905, 0.042328242, 0.042375654, 0.0784336, 0.87846863, 0.0784336, 0.079223745, 0.07916613, 0.87914294]);
    let outset = Mat3::from_cols_array(&[1.196879, -0.05289685, -0.052971635, -0.09802088, 1.1519032, -0.09804345, -0.099_029_74, -0.098961175, 1.1510737]);
    let c = (inset * c).max(Vec3::splat(1e-10));
    let x = (Vec3::new(c.x.log2(), c.y.log2(), c.z.log2()).clamp(Vec3::splat(-12.47393), Vec3::splat(4.026069)) + Vec3::splat(12.47393)) / 16.5;
    let x2 = x * x;
    let x4 = x2 * x2;
    let curved = 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - Vec3::splat(0.00232);
    outset * curved.max(Vec3::ZERO).powf(2.2)
}

pub(super) fn blur(src: &[Vec3], w: usize, h: usize, sigma: f32) -> Vec<Vec3> {
    let radius = (sigma * 3.0).ceil() as i32;
    let weights: Vec<f32> = (-radius..=radius).map(|i| (-0.5 * (i as f32 / sigma).powi(2)).exp()).collect();
    let total: f32 = weights.iter().sum();
    let mut temp = vec![Vec3::ZERO; src.len()];
    let mut out = temp.clone();
    for y in 0..h {
        for x in 0..w {
            for (i, k) in (-radius..=radius).zip(&weights) {
                let xx = super::post::reflect(x as i32 + i, w);
                temp[y * w + x] += src[y * w + xx] * (k / total);
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            for (i, k) in (-radius..=radius).zip(&weights) {
                let yy = super::post::reflect(y as i32 + i, h);
                out[y * w + x] += temp[yy * w + x] * (k / total);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thin_lens_converges_and_blur_grows_away_from_focus() {
        let lens = Lens { radius: 0.02, focus: 2.0, blades: 0, rotation: 0.0 };
        let width = |depth: f32| {
            let points: Vec<_> = [0.0, 0.25, 0.5, 0.75].into_iter().map(|phi| {
                let (o, d) = lens.ray(Vec3::ZERO, Vec3::Z, Vec3::Z, Vec3::X, Vec3::Y, Vec2::new(1.0, phi));
                o + d * ((depth - o.z) / d.z)
            }).collect();
            points[0].distance(points[2])
        };
        assert!(width(2.0) < 1e-6);
        assert!(width(1.0) > 0.01);
        assert!(width(4.0) > width(3.0) * 1.9);
        let cam = Camera { eye: Vec3::Z * 3.0, target: Vec3::ZERO, up: Vec3::Y, fov_deg: 30.0, ortho: None };
        let a = Look::parse(&json!({"camera":{"focal_length_mm":85,"dof":{"f_stop":2.8,"focus_point":[0,0,0]}}})).unwrap();
        let b = Look::parse(&json!({"camera":{"focal_length_mm":85,"aperture":5.6}})).unwrap();
        assert!((a.camera.lens(&a.camera.apply(cam).unwrap()).radius / b.camera.lens(&b.camera.apply(cam).unwrap()).radius - 2.0).abs() < 1e-6);
    }

    #[test]
    fn finishing_spreads_glow_and_preserves_saturated_filmic_highlights() {
        let look = Look::parse(&json!({"bloom":true,"tonemap":"filmic"})).unwrap();
        let mut c = vec![Vec3::ZERO; 32 * 32];
        c[16 * 32 + 16] = Vec3::new(20.0, 0.1, 0.05);
        let pixels = look.finish(c, 32, 32, 1.0);
        assert!(pixels[16 * 32 + 17][0] > 0);
        assert!(pixels[16 * 32 + 16][0] > pixels[16 * 32 + 16][1] * 3);
        for tone in ["aces", "agx", "filmic"] {
            let look = Look::parse(&json!({"tonemap":tone})).unwrap();
            let p = look.finish(vec![Vec3::ZERO, Vec3::ONE, Vec3::splat(100.0)], 3, 1, 1.0);
            let ramp = look.finish((0..1001).map(|i|Vec3::splat(i as f32*0.1)).collect(),1001,1,1.0);
            assert!(ramp.windows(2).all(|w|w[1][0]>=w[0][0]),"monotonic tone curve: {tone}");
            assert!(p[0][0] < 4);
            assert!(p[1][0] > p[0][0] && p[2][0] >= p[1][0]);
            assert!(p[1].iter().max().unwrap() - p[1].iter().min().unwrap() <= 1, "neutral white: {tone} {:?}", p[1]);
        }
    }

    #[test]
    fn sensor_exposure_and_preset_overrides() {
        let a = Look::parse(&json!({"camera":{"focal_length_mm":50,"sensor_size_mm":[36,24],"aperture":2,"iso":100,"shutter_seconds":0.01}})).unwrap();
        let b = Look::parse(&json!({"camera":{"focal_length_mm":50,"sensor_size_mm":[18,12],"aperture":2,"iso":200,"shutter_seconds":0.01}})).unwrap();
        let cam = Camera { eye:Vec3::Z*3.0,target:Vec3::ZERO,up:Vec3::Y,fov_deg:30.0,ortho:None };
        assert!((a.camera.lens(&a.camera.apply(cam).unwrap()).radius-0.0125).abs()<1e-6);
        assert!((b.camera.lens(&b.camera.apply(cam).unwrap()).radius-0.0125).abs()<1e-6);
        assert!(a.camera.fov.unwrap()>b.camera.fov.unwrap());
        assert!((b.camera.exposure/a.camera.exposure-2.0).abs()<1e-6);
        let e = Look::parse(&json!({"camera":{"ev":11}})).unwrap();
        assert_eq!(e.camera.exposure,2.0);
        let p = Look::parse(&json!({"look":"trailer","fog":false,"bloom":false,"grain":0,"camera":{"aperture":null}})).unwrap();
        assert!(p.fog.is_none() && p.bloom==0.0 && p.camera.f_stop.is_none());
        for blades in [3,5,6,7,16] {
            let lens=Lens {radius:0.03,focus:2.0,blades,rotation:0.2};
            for i in 0..1000 {
                let u=Vec2::new((i as f32+0.5)/1000.0,((i*619%1000) as f32+0.5)/1000.0);
                assert!(lens.sample(u).length()<=1.00001);
                let (o,d)=lens.ray(Vec3::ZERO,Vec3::Z,Vec3::Z,Vec3::X,Vec3::Y,u);
                assert!((o+d*(2.0/d.z)-Vec3::Z*2.0).length()<1e-6);
            }
        }
    }

    #[test]
    fn invalid_options_return_errors() {
        for args in [json!({"camera":{"dof":{"blades":2}}}), json!({"camera":{"sensor_size_mm":[36,0]}}), json!({"camera":{"ev":12,"iso":100}}), json!({"grade":{"gamma":[1,0,1]}}), json!({"look":"unknown"}), json!({"camera":{"dof":{"f_stop":0}}}), json!({"environment":"unknown"}), json!({"white_balance":0}), json!({"lights":[{"type":"sun","direction":[0,0,0]}]}), json!({"camera":{"fov":"wide"}}), json!({"tonemap":"unknown"})] {
            assert!(Look::parse(&args).is_err(), "{args}");
        }
    }
}
