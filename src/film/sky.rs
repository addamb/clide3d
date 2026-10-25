//! A physical sun and sky: single scattering of sunlight through a
//! spherical atmosphere (Nishita et al. 1993), baked into an
//! equirectangular map that `Environment` samples like any other.
//!
//! The planet is 6360 km across to 6420 km of air. Air molecules scatter
//! by Rayleigh (strongly toward blue, 8 km scale height), haze by Mie
//! (grey, forward-peaked, 1.2 km), and ozone only absorbs (a tent around
//! 25 km), which keeps the twilight sky blue rather than grey. Each texel
//! marches its view ray in 32 steps and the sunlight reaching every step
//! in 8 more; the sun disc itself is not painted into the map (it is far
//! smaller than a texel) but given to `Environment` as its sun, so the
//! tracers sample it explicitly.
//!
//! The same sunlight feeds the sky and the disc, so the ratio of sun to
//! sky, and how both redden and dim toward the horizon, come out of the
//! scattering. Their absolute brightness does not: `SOLAR` is a chosen
//! calibration constant (a 30 degree sun lights the ground as the
//! golden_hour preset does), so existing exposures and look presets suit
//! it. It is not a physical irradiance in any unit.
use std::f32::consts::PI;
use std::sync::{Arc, Mutex, OnceLock};

use glam::{DVec3, Vec3};
use serde_json::Value;

use super::lighting::number;

/// The baked map's size (storm's, so the GPU path is a known one).
pub const WIDTH: usize = 512;
pub const HEIGHT: usize = 256;
/// The sun's angular radius (radians), as seen from Earth.
pub const SUN_RADIUS: f32 = 0.00465;
/// Sunlight above the atmosphere: chosen so that with the default
/// turbidity and ground a 30 degree sun gives the ground the same
/// horizontal irradiance (luminance, sun plus sky) as the golden_hour
/// preset (`physical_sky_matches_golden_hour_irradiance_at_30deg`).
pub const SOLAR: f32 = 6.2075;
/// The film key light's azimuth, atan2(0.45, 0.55) in degrees, so a
/// default physical sun stands where render_film's key does.
pub const DEFAULT_AZIMUTH: f32 = 39.29;

/// The preset names that are a physical sky, and their sun elevations.
pub const PRESETS: [(&str, f32); 5] = [("physical", 30.0), ("sky_noon", 60.0), ("sky_afternoon", 30.0), ("sky_golden", 8.0), ("sky_sunset", 1.5)];
/// The settings only a physical sky takes.
pub const KEYS: [&str; 4] = ["sun_elevation", "sun_azimuth", "turbidity", "ground_albedo"];

const GROUND_RADIUS: f64 = 6_360e3;
const TOP_RADIUS: f64 = 6_420e3;
/// The viewer stands this high (metres) above the ground.
const VIEW_HEIGHT: f64 = 1.0;
/// Scattering coefficients per metre at sea level (R, G, B).
const RAYLEIGH: DVec3 = DVec3::new(5.8e-6, 13.5e-6, 33.1e-6);
const RAYLEIGH_HEIGHT: f64 = 8_000.0;
/// Mie scattering at turbidity 2.5; it extincts a tenth more than it
/// scatters.
const MIE: f64 = 21e-6;
const MIE_HEIGHT: f64 = 1_200.0;
const MIE_G: f64 = 0.76;
const OZONE: DVec3 = DVec3::new(0.65e-6, 1.88e-6, 0.085e-6);
const VIEW_STEPS: usize = 32;
const LIGHT_STEPS: usize = 8;

/// A physical sky's settings (angles in degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sky {
    pub elevation: f32,
    pub azimuth: f32,
    /// Haze, 1 (clear air, no Mie) to 10 (thick haze).
    pub turbidity: f32,
    pub ground_albedo: f32,
}

/// A baked sky: the map (`WIDTH` x `HEIGHT`, rows from the zenith down)
/// and the sun's irradiance on a surface facing it, if any reaches the
/// ground.
pub struct Baked {
    pub pixels: Vec<Vec3>,
    pub sun: Option<Vec3>,
}

impl Sky {
    /// A physical sky preset with its default settings.
    pub fn named(name: &str) -> Option<Sky> {
        PRESETS.iter().find(|(n, _)| *n == name).map(|&(_, elevation)| Sky { elevation, azimuth: DEFAULT_AZIMUTH, turbidity: 2.5, ground_albedo: 0.3 })
    }

    /// `base` with the settings `v` gives.
    pub fn parse(v: &Value, base: Sky) -> Result<Sky, String> {
        Ok(Sky {
            elevation: number(v, "sun_elevation", base.elevation, -5.0, 90.0)?,
            azimuth: number(v, "sun_azimuth", base.azimuth, -36000.0, 36000.0)?,
            turbidity: number(v, "turbidity", base.turbidity, 1.0, 10.0)?,
            ground_albedo: number(v, "ground_albedo", base.ground_albedo, 0.0, 1.0)?,
        })
    }

    /// Toward the sun: `azimuth` turns from +Z toward +X.
    pub fn sun_direction(&self) -> Vec3 {
        let (e, a) = (self.elevation.to_radians(), self.azimuth.to_radians());
        Vec3::new(e.cos() * a.sin(), e.sin(), e.cos() * a.cos())
    }

    /// The baked sky (cached: a bake takes tens of milliseconds and
    /// lighting is parsed often).
    pub fn bake(&self) -> Arc<Baked> {
        type Cache = Mutex<Vec<([u32; 4], Arc<Baked>)>>;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let key = [self.elevation, self.azimuth, self.turbidity, self.ground_albedo].map(f32::to_bits);
        let cache = CACHE.get_or_init(Default::default);
        if let Some((_, b)) = cache.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
            return b.clone();
        }
        let baked = Arc::new(self.bake_uncached());
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push((key, baked.clone()));
        baked
    }

    fn bake_uncached(&self) -> Baked {
        let air = Air { mie: MIE * f64::from(self.turbidity - 1.0) / 1.5 };
        let sun = self.sun_direction().as_dvec3().normalize();
        let eye = DVec3::new(0.0, GROUND_RADIUS + VIEW_HEIGHT, 0.0);
        let mut pixels = vec![Vec3::ZERO; WIDTH * HEIGHT];
        // Above the horizon the sky; below it, for now, the sky just over
        // the horizon in that azimuth (the ground fades into it).
        crate::clay::par_chunks(&mut pixels, WIDTH, |y, row| {
            let cos = (std::f64::consts::PI * (y as f64 + 0.5) / HEIGHT as f64).cos();
            for (x, p) in row.iter_mut().enumerate() {
                let d = super::lighting::Environment::direction((x as f32 + 0.5) / WIDTH as f32, cos).as_dvec3();
                let d = if d.y < 1e-4 { DVec3::new(d.x, 1e-4, d.z).normalize() } else { d };
                *p = air.inscatter(eye, d, sun).as_vec3() * SOLAR;
            }
        });
        let transmit = air.depth(eye, sun, 64).map_or(DVec3::ZERO, |t| (-air.extinction(t)).exp()).as_vec3();
        let sun_irradiance = transmit * SOLAR;
        // The ground: a diffuse floor lit by the sun and the sky above it.
        let mut sky = Vec3::ZERO;
        for y in 0..HEIGHT {
            let solid = (std::f64::consts::TAU / WIDTH as f64 * ((std::f64::consts::PI * y as f64 / HEIGHT as f64).cos() - (std::f64::consts::PI * (y + 1) as f64 / HEIGHT as f64).cos())) as f32;
            let up = (PI * (y as f32 + 0.5) / HEIGHT as f32).cos();
            if up > 0.0 {
                sky += pixels[y * WIDTH..(y + 1) * WIDTH].iter().sum::<Vec3>() * solid * up;
            }
        }
        let ground = (sky + sun_irradiance * self.sun_direction().y.max(0.0)) * self.ground_albedo / PI;
        for y in HEIGHT / 2..HEIGHT {
            let up = (PI * (y as f32 + 0.5) / HEIGHT as f32).cos();
            let haze = super::smoothstep(-0.08, 0.0, up);
            for p in &mut pixels[y * WIDTH..(y + 1) * WIDTH] {
                *p = ground.lerp(*p, haze);
            }
        }
        let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        Baked { pixels, sun: (lum(sun_irradiance) > 1e-6 * SOLAR).then_some(sun_irradiance) }
    }
}

/// The atmosphere's haze (Mie scattering at sea level, per metre).
struct Air {
    mie: f64,
}

/// Where a ray from `o` along unit `d` leaves a sphere of radius `r`
/// around the planet's centre (o inside it).
fn exit(o: DVec3, d: DVec3, r: f64) -> f64 {
    let b = o.dot(d);
    let c = o.length_squared() - r * r;
    -b + (b * b - c).max(0.0).sqrt()
}

/// Whether a ray from `o` (above the ground) along `d` hits the ground.
fn hits_ground(o: DVec3, d: DVec3) -> bool {
    let b = o.dot(d);
    let disc = b * b - (o.length_squared() - GROUND_RADIUS * GROUND_RADIUS);
    b < 0.0 && disc > 0.0
}

/// Relative density of air, haze and ozone at `h` metres up.
fn density(h: f64) -> DVec3 {
    DVec3::new((-h / RAYLEIGH_HEIGHT).exp(), (-h / MIE_HEIGHT).exp(), (1.0 - (h - 25_000.0).abs() / 15_000.0).max(0.0))
}

impl Air {
    /// Optical depths (air, haze, ozone: density times metres) from `o`
    /// along `d` out of the atmosphere, or None when the ground is in the
    /// way. The steps grow quadratically, as the air thins with height.
    fn depth(&self, o: DVec3, d: DVec3, steps: usize) -> Option<DVec3> {
        if hits_ground(o, d) {
            return None;
        }
        let t = exit(o, d, TOP_RADIUS);
        let mut sum = DVec3::ZERO;
        for i in 0..steps {
            let u = (i as f64 + 0.5) / steps as f64;
            let p = o + d * (t * u * u);
            sum += density(p.length() - GROUND_RADIUS) * (2.0 * t * u / steps as f64);
        }
        Some(sum)
    }

    /// Optical thickness per colour of the given depths.
    fn extinction(&self, depth: DVec3) -> DVec3 {
        RAYLEIGH * depth.x + DVec3::splat(self.mie * 1.1 * depth.y) + OZONE * depth.z
    }

    /// The sky's radiance from `o` along unit `d` under unit sunlight from
    /// `sun`: light scattered once toward the viewer at every step.
    fn inscatter(&self, o: DVec3, d: DVec3, sun: DVec3) -> DVec3 {
        let t = exit(o, d, TOP_RADIUS);
        let (mut air, mut haze, mut along) = (DVec3::ZERO, DVec3::ZERO, DVec3::ZERO);
        for i in 0..VIEW_STEPS {
            let u = (i as f64 + 0.5) / VIEW_STEPS as f64;
            let p = o + d * (t * u * u);
            let step = density(p.length() - GROUND_RADIUS) * (2.0 * t * u / VIEW_STEPS as f64);
            // Up to the middle of this step.
            let to = along + step * 0.5;
            along += step;
            let Some(light) = self.depth(p, sun, LIGHT_STEPS) else { continue };
            let reach = (-self.extinction(to + light)).exp();
            air += reach * step.x;
            haze += reach * step.y;
        }
        let mu = d.dot(sun);
        let rayleigh = 3.0 / (16.0 * std::f64::consts::PI) * (1.0 + mu * mu);
        let g = MIE_G;
        // Cornette-Shanks.
        let mie = 3.0 / (8.0 * std::f64::consts::PI) * (1.0 - g * g) * (1.0 + mu * mu) / ((2.0 + g * g) * (1.0 + g * g - 2.0 * g * mu).powf(1.5));
        air * RAYLEIGH * rayleigh + haze * (self.mie * mie)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_presets_and_parsing() {
        let s = Sky::named("sky_golden").unwrap();
        assert_eq!((s.elevation, s.azimuth, s.turbidity, s.ground_albedo), (8.0, DEFAULT_AZIMUTH, 2.5, 0.3));
        assert!(Sky::named("golden_hour").is_none());
        let p = Sky::parse(&serde_json::json!({ "sun_elevation": 12, "turbidity": 4 }), s).unwrap();
        assert_eq!((p.elevation, p.azimuth, p.turbidity), (12.0, DEFAULT_AZIMUTH, 4.0));
        for (bad, field) in [(serde_json::json!({ "sun_elevation": 95 }), "sun_elevation must be a number in -5..90"), (serde_json::json!({ "turbidity": 0.5 }), "turbidity must be a number in 1..10"), (serde_json::json!({ "ground_albedo": "x" }), "ground_albedo must be a number in 0..1")] {
            assert_eq!(Sky::parse(&bad, s).unwrap_err(), field);
        }
        // The default sun stands where render_film's key does.
        let key = Vec3::new(0.45, 0.75, 0.55).normalize();
        let d = Sky::named("physical").unwrap().sun_direction();
        assert!((d.x.atan2(d.z) - key.x.atan2(key.z)).abs() < 1e-3, "{d} vs {key}");
        assert!((d.y - 30f32.to_radians().sin()).abs() < 1e-6 && (d.length() - 1.0).abs() < 1e-6);
        let d = Sky { azimuth: 90.0, elevation: 0.0, ..s }.sun_direction();
        assert!((d - Vec3::X).length() < 1e-6, "{d}");
    }

    /// The sun's colour along the way down: redder (less blue for its
    /// red) and dimmer as it nears the horizon; gone below it.
    #[test]
    fn physical_sky_sun_warms_and_dims_toward_horizon() {
        let mut last: Option<Vec3> = None;
        for elevation in [60.0, 30.0, 10.0, 2.0] {
            let sun = Sky { elevation, ..Sky::named("physical").unwrap() }.bake().sun.unwrap();
            eprintln!("sun at {elevation} degrees: {sun}");
            if let Some(prev) = last {
                assert!(sun.z / sun.x < prev.z / prev.x, "{elevation}: {sun} not warmer than {prev}");
                assert!(sun.x + sun.y + sun.z < prev.x + prev.y + prev.z, "{elevation}: {sun} not dimmer than {prev}");
            }
            last = Some(sun);
        }
        assert!(Sky { elevation: -2.0, ..Sky::named("physical").unwrap() }.bake().sun.is_none());
    }

    /// The sky is blue overhead by day, and the sky low toward the sun
    /// warms as it sets; the ground is the albedo's share of the light.
    #[test]
    fn physical_sky_warms_toward_a_low_sun() {
        let at = |b: &Baked, d: Vec3| {
            let u = (d.z.atan2(d.x) / std::f32::consts::TAU).rem_euclid(1.0);
            let v = glam::Vec2::new(d.x, d.z).length().atan2(d.y) / PI;
            b.pixels[((v * HEIGHT as f32) as usize).min(HEIGHT - 1) * WIDTH + (u * WIDTH as f32) as usize % WIDTH]
        };
        let mut last: Option<Vec3> = None;
        for elevation in [60.0, 30.0, 10.0, 2.0] {
            let sky = Sky { elevation, ..Sky::named("physical").unwrap() };
            let b = sky.bake();
            let zenith = at(&b, Vec3::Y);
            assert!(zenith.z > zenith.x * 1.5, "{elevation}: zenith {zenith} is not blue");
            // 5 degrees up, toward the sun (well outside its disc).
            let s = sky.sun_direction();
            let low = Vec3::new(s.x, 0.0, s.z).normalize() * 5f32.to_radians().cos() + Vec3::Y * 5f32.to_radians().sin();
            let c = at(&b, low);
            eprintln!("{elevation} degrees: zenith {zenith}, low toward the sun {c}");
            if let Some(prev) = last {
                assert!(c.z / c.x < prev.z / prev.x, "{elevation}: low sky {c} not warmer than {prev}");
            }
            last = Some(c);
            assert!(b.pixels.iter().all(|p| p.is_finite() && p.min_element() >= 0.0));
        }
        // No ground light at all with a black ground.
        let black = Sky { ground_albedo: 0.0, ..Sky::named("physical").unwrap() }.bake();
        assert_eq!(at(&black, Vec3::NEG_Y), Vec3::ZERO);
    }

    /// Bakes repeat exactly (rows are independent, so the thread count
    /// does not matter), and the cache hands back the same sky.
    #[test]
    fn bakes_repeat_exactly() {
        let a = Sky::named("sky_noon").unwrap();
        let (fresh, cached) = (a.bake_uncached(), a.bake());
        assert!(fresh.pixels == cached.pixels && fresh.sun == cached.sun);
        assert!(fresh.pixels != Sky { turbidity: 3.0, ..a }.bake().pixels);
    }
}
