//! Light sources carried by objects: a torch's flame, a candle, a brazier,
//! a glowing crystal. `Object::light` holds one; the film renderer turns
//! every visible one (scattered copies included) into a soft spherical
//! light, and the viewport adds its warm pool.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct LightSource {
    /// Where the light sits, in the object's own space (a flame's middle).
    #[serde(default)]
    pub offset: Vec3,
    /// Color temperature in Kelvin: 1800 candle, 2000 torch, 2400 brazier,
    /// 3000 lamp, 6500 daylight. Ignored when `color` is set.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Linear RGB color (overrides `temperature`); brightness stays `intensity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// Radiant intensity: a surface 1 m away, facing it, gets about this
    /// much light (a studio key light is about 3).
    #[serde(default = "one")]
    pub intensity: f32,
    /// Radius of the glowing ball in meters: larger lights cast softer shadows.
    #[serde(default = "default_radius")]
    pub radius: f32,
    /// 0..1: how much brightness and warmth wander, different for every
    /// light (so 30 candles are not all alike) and from frame to frame.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flicker: f32,
}

fn default_temperature() -> f32 {
    2000.0
}

fn default_radius() -> f32 {
    0.05
}

fn one() -> f32 {
    1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Default for LightSource {
    fn default() -> Self {
        LightSource { offset: Vec3::ZERO, temperature: default_temperature(), color: None, intensity: 1.0, radius: default_radius(), flicker: 0.0 }
    }
}

/// Named light presets: (name, temperature, intensity, radius, flicker).
pub const PRESETS: [(&str, f32, f32, f32, f32); 6] = [
    ("candle", 1850.0, 0.35, 0.012, 0.35),
    ("torch", 1950.0, 3.0, 0.06, 0.4),
    ("brazier", 2100.0, 6.0, 0.14, 0.35),
    ("lantern", 2600.0, 1.5, 0.05, 0.1),
    ("fire", 2000.0, 10.0, 0.25, 0.4),
    ("magic", 9000.0, 2.0, 0.08, 0.2),
];

impl LightSource {
    pub fn preset(name: &str) -> Option<LightSource> {
        PRESETS.iter().find(|p| p.0 == name).map(|&(_, temperature, intensity, radius, flicker)| LightSource { offset: Vec3::ZERO, temperature, color: None, intensity, radius, flicker })
    }

    pub fn sanitize(&mut self) {
        let fix = |v: f32, d: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        self.temperature = fix(self.temperature, default_temperature(), 1000.0, 15000.0);
        self.intensity = fix(self.intensity, 1.0, 0.0, 100_000.0);
        self.radius = fix(self.radius, default_radius(), 0.001, 10.0);
        self.flicker = fix(self.flicker, 0.0, 0.0, 1.0);
        if !self.offset.is_finite() {
            self.offset = Vec3::ZERO;
        }
        if let Some(c) = &mut self.color {
            *c = c.map(|v| fix(v, 1.0, 0.0, 100.0));
        }
    }

    /// The light placed by `world`: (position, linear RGB power, radius).
    /// `seed` tells copies apart and `time` (seconds) moves the flicker.
    pub fn emit(&self, world: Mat4, seed: u64, time: f32) -> (Vec3, Vec3, f32) {
        let scale = world.to_scale_rotation_translation().0.abs().max_element().max(1e-4);
        let phase = (seed % 10_007) as f32 * 0.731;
        let wobble = |speed: f32, k: u32| super::noise(Vec3::new(time * speed + phase, phase * 1.7, k as f32 * 13.1), 77 + k);
        let f = self.flicker;
        // Each light differs a little (seed), and wanders over time.
        let bright = (1.0 + f * (0.28 * wobble(0.0, 0) + 0.18 * wobble(7.3, 1) + 0.08 * wobble(19.0, 2))).max(0.05);
        let warmth = self.temperature + f * (160.0 * wobble(0.0, 3) + 90.0 * wobble(5.1, 4));
        let color = match self.color {
            Some(c) => Vec3::from(c),
            None => blackbody(warmth),
        };
        let lum = color.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-6);
        (world.transform_point3(self.offset), color / lum * self.intensity * bright, self.radius * scale)
    }
}

/// Every object light in the scene (scattered copies included) as film
/// lights: soft spheres, flickering for `time` seconds.
pub fn film_lights(scene: &crate::scene::Scene, eval: &std::collections::HashMap<u64, (Mat4, bool)>, time: f32) -> Vec<crate::film::lighting::Light> {
    super::scatter::draws(scene, eval)
        .iter()
        .filter_map(|d| d.object.light.as_ref().map(|l| l.emit(d.world, d.copy, time)))
        .filter(|(_, power, _)| power.max_element() > 0.0)
        .map(|(position, power, radius)| crate::film::lighting::Light::Sphere { position, radius, power })
        .collect()
}

/// The lights the viewport shades with: every object light (copies
/// included), the `MAX_VIEW_LIGHTS` brightest near `focus`.
pub fn view_lights(scene: &crate::scene::Scene, eval: &std::collections::HashMap<u64, (Mat4, bool)>, focus: Vec3) -> Vec<(Vec3, f32, Vec3)> {
    let mut lights: Vec<(Vec3, f32, Vec3)> = super::scatter::draws(scene, eval)
        .iter()
        .filter_map(|d| d.object.light.as_ref().map(|l| l.emit(d.world, d.copy, 0.0)))
        .map(|(p, power, r)| (p, r, power))
        .collect();
    for d in super::scatter::draws(scene,eval) {
        if let Some(f)=d.object.fire.as_ref().filter(|f|!f.cache.is_empty()) {
            let l=f.cache[f.frame_index(scene.frame)].light;
            lights.push((d.world.transform_point3(Vec3::new(l[0],l[1],l[2])),f.settings.radius,Vec3::new(l[3],l[4],l[5])*d.world.determinant().abs()*f.settings.light_gain));
        }
    }
    let weight = |l: &(Vec3, f32, Vec3)| l.2.max_element() / (l.0.distance_squared(focus) + 1.0);
    lights.sort_by(|a, b| weight(b).total_cmp(&weight(a)));
    lights.truncate(crate::render::MAX_VIEW_LIGHTS);
    lights
}

/// Linear RGB of a glowing body at `k` Kelvin, at its own brightness
/// (normalize for a fixed luminance). Tanner Helland's fit.
pub fn blackbody(k: f32) -> Vec3 {
    let t = k.clamp(1000.0, 40000.0) / 100.0;
    let r = if t <= 66.0 { 255.0 } else { 329.698_73 * (t - 60.0).powf(-0.133_204_76) };
    let g = if t <= 66.0 { 99.470_8 * t.ln() - 161.119_57 } else { 288.122_16 * (t - 60.0).powf(-0.075_514_85) };
    let b = if t >= 66.0 { 255.0 } else if t <= 19.0 { 0.0 } else { 138.517_73 * (t - 10.0).ln() - 305.044_8 };
    (Vec3::new(r, g, b).clamp(Vec3::ZERO, Vec3::splat(255.0)) / 255.0).powf(2.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candles_are_warm_and_flicker_apart() {
        let c = LightSource::preset("candle").unwrap();
        let (p, power, r) = c.emit(Mat4::from_translation(Vec3::Y), 1, 0.0);
        assert_eq!(p, Vec3::Y);
        assert!(power.x > power.y && power.y > power.z, "warm: {power}");
        assert!((r - c.radius).abs() < 1e-6);
        let (_, other, _) = c.emit(Mat4::IDENTITY, 2, 0.0);
        assert!((other - power).length() > 1e-3, "two candles differ");
        let (_, later, _) = c.emit(Mat4::IDENTITY, 2, 0.5);
        assert!((other - later).length() > 1e-4, "flicker moves over time");
        let steady = LightSource { flicker: 0.0, ..c };
        assert_eq!(steady.emit(Mat4::IDENTITY, 1, 0.0).1, steady.emit(Mat4::IDENTITY, 9, 3.0).1);
        let lum = steady.emit(Mat4::IDENTITY, 1, 0.0).1.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        assert!((lum - steady.intensity).abs() < 1e-4);
    }
}
