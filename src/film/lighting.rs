//! Equirectangular lighting, sampled by luminance and exact texel solid angle.
use std::f32::consts::{PI, TAU};
use std::io::{BufRead, Cursor, Read};

use glam::{Vec2, Vec3};
use serde_json::Value;

use super::{Rng, shade};

pub(super) struct GpuEnvironment<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [Vec3],
    pub cdf: Vec<f32>,
    /// Whether the map has any light at all.
    pub lit: bool,
    pub rotate: f32,
    pub sun: Option<(Vec3, f32, Vec3)>,
    pub sun_chance: f32,
}

pub struct Environment {
    width: usize,
    height: usize,
    pixels: Vec<Vec3>,
    cdf: Vec<f64>,
    total: f64,
    rotate: f32,
    sun: Option<(Vec3, f32, Vec3)>,
    sun_chance: f32,
}

fn luminance(c: Vec3) -> f32 { c.dot(Vec3::new(0.2126, 0.7152, 0.0722)) }

impl Environment {
    pub fn map(width: usize, height: usize, pixels: Vec<Vec3>, rotate: f32) -> Self {
        let mut cdf = vec![0.0];
        let mut total = 0.0;
        for y in 0..height {
            let solid = std::f64::consts::TAU / width as f64 * ((std::f64::consts::PI * y as f64 / height as f64).cos() - (std::f64::consts::PI * (y + 1) as f64 / height as f64).cos());
            for x in 0..width {
                // Bilinear footprints reach neighboring cells; keep the
                // sampling distribution positive everywhere they emit.
                let mut energy = 0.0;
                for (dy, wy) in [(-1, 0.125), (0, 0.75), (1, 0.125)] {
                    let yy = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                    for (dx, wx) in [(-1, 0.125), (0, 0.75), (1, 0.125)] {
                        let xx = (x as isize + dx).rem_euclid(width as isize) as usize;
                        energy += luminance(pixels[yy * width + xx]).max(0.0) * (wx * wy);
                    }
                }
                total += energy as f64 * solid;
                cdf.push(total);
            }
        }
        Self { width, height, pixels, cdf, total, rotate: rotate.to_radians(), sun: None, sun_chance: 0.0 }
    }

    pub fn preset(name: &str) -> Result<Self, String> {
        let (sky, ground, sun): (Vec3, Vec3, Option<(Vec3, f32, Vec3)>) = match name {
            "studio" => (Vec3::new(0.32, 0.34, 0.38), Vec3::new(0.06, 0.055, 0.05), None),
            "overcast" => (Vec3::new(0.9, 1.0, 1.15), Vec3::new(0.16, 0.17, 0.18), None),
            "sunset" => (Vec3::new(0.18, 0.25, 0.48), Vec3::new(0.055, 0.035, 0.025), Some((Vec3::new(-0.8, 0.08, 0.5), 0.009, Vec3::new(4.0, 0.9, 0.25)))),
            "golden_hour" => (Vec3::new(0.42, 0.57, 0.85), Vec3::new(0.12, 0.09, 0.055), Some((Vec3::new(0.65, 0.28, 0.65), 0.009, Vec3::new(4.5, 2.8, 1.3)))),
            "forest" => (Vec3::new(0.25, 0.45, 0.22), Vec3::new(0.07, 0.055, 0.025), Some((Vec3::new(0.5, 0.8, -0.3), 0.012, Vec3::new(2.8, 2.6, 1.8)))),
            "storm" => (Vec3::new(0.10, 0.16, 0.24), Vec3::new(0.025, 0.025, 0.03), None),
            "night" => (Vec3::new(0.008, 0.015, 0.04), Vec3::new(0.002, 0.003, 0.006), Some((Vec3::new(-0.6, 0.5, 0.5), 0.006, Vec3::new(0.035, 0.05, 0.09)))),
            _ => return Err("environment must be studio, overcast, sunset, forest, golden_hour, night, storm or {preset or hdr, rotate, intensity}".into()),
        };
        let (w, h) = if name=="storm" {(512,256)} else {(256,128)};
        let cloud_field=Fog {density:0.5,falloff:0.0,noise:1.0,noise_scale:1.0,seed:71,..Default::default()};
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let d = Self::direction((x as f32 + 0.5) / w as f32, (PI * (y as f32 + 0.5) / h as f32).cos() as f64);
                let t = super::smoothstep(-0.3, 0.6, d.y);
                let mut c = ground.lerp(sky, t);
                if name == "overcast" { c *= 0.65 + 0.35 * d.y.max(0.0); }
                if name=="storm" {
                    let n=0.57*cloud_field.density_at_point(d*6.0)+0.28*cloud_field.density_at_point(d*14.0+Vec3::splat(3.7))+0.15*cloud_field.density_at_point(d*32.0+Vec3::splat(8.1));
                    let cloud=super::smoothstep(0.35,0.65,n);
                    let sky_color=sky.lerp(Vec3::new(0.42,0.47,0.52),cloud)*(0.65+0.35*d.y.max(0.0));
                    c=ground.lerp(sky_color,super::smoothstep(-0.15,0.05,d.y));
                    c+=Vec3::new(0.16,0.12,0.07)*(-d.y.abs()*8.0).exp();
                }
                if matches!(name, "sunset" | "golden_hour") {
                    c += Vec3::new(0.7, 0.25, 0.06) * (-d.y.abs() * 8.0).exp();
                }
                if name == "forest" && d.y > 0.0 {
                    let gaps = ((d.x * 29.0).sin() * (d.z * 37.0).sin() * (d.y * 41.0).sin()).max(0.0).powi(8);
                    c = c * (0.3 + d.y * 0.5) + Vec3::new(1.3, 1.5, 1.8) * gaps;
                }
                pixels.push(c);
            }
        }
        let mut env = Self::map(w, h, pixels, 0.0);
        env.sun = sun.map(|(dir, size, power)| (dir.normalize(), size, power / (TAU * (1.0 - size.cos()))));
        // A separate disc distribution resolves even sub-texel suns.
        if let Some((_, size, c)) = env.sun {
            let flux = luminance(c) as f64 * (TAU * (1.0 - size.cos())) as f64;
            env.sun_chance = (flux / (flux + env.total)).clamp(0.05, 0.95) as f32;
        }
        Ok(env)
    }

    pub fn parse(v: &Value) -> Result<Self, String> {
        if v.is_null() { return Self::preset("studio"); }
        if let Some(s) = v.as_str() { return Self::preset(s); }
        if let Some(name)=v["preset"].as_str() {
            if !v["hdr"].is_null() {return Err("use environment.preset or hdr, not both".into());}
            let intensity=number(v,"intensity",1.0,0.0,10000.0)?;
            let rotate=number(v,"rotate",0.0,-36000.0,36000.0)?.to_radians();
            let mut env=Self::preset(name)?;
            for c in &mut env.pixels {*c*=intensity;}
            for c in &mut env.cdf {*c*=intensity as f64;}
            env.total*=intensity as f64;
            env.rotate=rotate;
            env.sun=env.sun.map(|(d,size,c)| {
                let (sin,cos)=rotate.sin_cos();
                (Vec3::new(d.x*cos-d.z*sin,d.y,d.x*sin+d.z*cos),size,c*intensity)
            });
            if intensity==0.0 {env.sun_chance=0.0;}
            return Ok(env);
        }
        let path = v["hdr"].as_str().ok_or("environment needs a preset or hdr path")?;
        let path = std::path::Path::new(path);
        if !path.is_absolute() || !path.extension().is_some_and(|s| s.eq_ignore_ascii_case("hdr")) { return Err("hdr must be an absolute .hdr path".into()); }
        let rotate = number(v, "rotate", 0.0, -36000.0, 36000.0)?;
        let intensity = number(v, "intensity", 1.0, 0.0, 10000.0)?;
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (w, h, mut pixels) = read_hdr(&bytes)?;
        pixels.iter_mut().for_each(|p| *p *= intensity);
        if pixels.iter().any(|p| !p.is_finite()) { return Err("HDR intensity exceeds float range".into()); }
        Ok(Self::map(w, h, pixels, rotate))
    }

    /// About how much light the environment sheds on a point (for
    /// choosing between it and lights in fog).
    pub fn irradiance(&self) -> f32 {
        let sun = self.sun.map_or(0.0, |(_, size, c)| luminance(c) * TAU * (1.0 - size.cos()));
        (self.total as f32 + sun) * 0.5
    }

    /// What the GPU tracer needs (the cdf scaled to 0..1).
    pub(super) fn gpu(&self) -> GpuEnvironment<'_> {
        let total = self.total.max(1e-300);
        GpuEnvironment {
            width: self.width,
            height: self.height,
            pixels: &self.pixels,
            cdf: self.cdf.iter().map(|c| (c / total) as f32).collect(),
            lit: self.total > 0.0,
            rotate: self.rotate,
            sun: self.sun,
            sun_chance: self.sun_chance,
        }
    }

    fn direction(u: f32, cos: f64) -> Vec3 {
        let phi = TAU * u;
        let r = (1.0 - cos * cos).max(0.0).sqrt() as f32;
        Vec3::new(r * phi.cos(), cos as f32, r * phi.sin())
    }

    fn coords(&self, d: Vec3) -> (usize, usize) {
        let u = ((d.z.atan2(d.x) - self.rotate) / TAU).rem_euclid(1.0);
        let v = Vec2::new(d.x, d.z).length().atan2(d.y) / PI;
        ((u * self.width as f32) as usize % self.width, ((v * self.height as f32) as usize).min(self.height - 1))
    }

    pub fn at(&self, d: Vec3) -> Vec3 {
        let u = ((d.z.atan2(d.x) - self.rotate) / TAU).rem_euclid(1.0);
        let v = Vec2::new(d.x, d.z).length().atan2(d.y) / PI;
        let x = u * self.width as f32 - 0.5;
        let y = (v * self.height as f32 - 0.5).clamp(0.0, self.height as f32 - 1.0);
        let (x0, y0) = (x.floor() as isize, y.floor() as usize);
        let at = |xx: isize, yy: usize| self.pixels[yy * self.width + xx.rem_euclid(self.width as isize) as usize];
        let top = at(x0, y0).lerp(at(x0 + 1, y0), x - x.floor());
        let bottom_y = (y0 + 1).min(self.height - 1);
        let bottom = at(x0, bottom_y).lerp(at(x0 + 1, bottom_y), x - x.floor());
        let mut c = top.lerp(bottom, y - y.floor());
        if let Some((dir, size, power)) = self.sun && d.dot(dir) >= size.cos() { c += power; }
        c
    }

    pub fn pdf(&self, d: Vec3) -> f32 {
        let (x, y) = self.coords(d);
        let i = y * self.width + x;
        let solid = std::f64::consts::TAU / self.width as f64 * ((std::f64::consts::PI * y as f64 / self.height as f64).cos() - (std::f64::consts::PI * (y + 1) as f64 / self.height as f64).cos());
        let map = if self.total > 0.0 { ((self.cdf[i + 1] - self.cdf[i]) / (self.total * solid)) as f32 } else { 1.0 / (4.0 * PI) };
        let sun = self.sun.map_or(0.0, |(dir, size, _)| if d.dot(dir) >= size.cos() { 1.0 / (TAU * (1.0 - size.cos())) } else { 0.0 });
        (1.0 - self.sun_chance) * map + self.sun_chance * sun
    }

    pub(super) fn sample(&self, rng: &mut Rng) -> (Vec3, Vec3, f32) {
        let d = if rng.f() < self.sun_chance {
            let (dir, size, _) = self.sun.unwrap();
            shade::cone(dir, size, rng.v2())
        } else if self.total <= 0.0 { shade::sphere(rng.v2()) } else {
            let pick = rng.f() as f64 * self.total;
            let i = self.cdf.partition_point(|&v| v <= pick).saturating_sub(1).min(self.pixels.len() - 1);
            let (x, y) = (i % self.width, i / self.width);
            let u = (x as f32 + rng.f()) / self.width as f32 + self.rotate / TAU;
            let a = (std::f64::consts::PI * y as f64 / self.height as f64).cos();
            let b = (std::f64::consts::PI * (y + 1) as f64 / self.height as f64).cos();
            Self::direction(u, a + (b - a) * rng.f() as f64)
        };
        (d, self.at(d), self.pdf(d))
    }
}

/// RGBE Radiance: modern channel RLE and legacy interleaved/repeated pixels.
pub fn read_hdr(bytes: &[u8]) -> Result<(usize, usize, Vec<Vec3>), String> {
    let mut input = Cursor::new(bytes);
    let mut line = String::new();
    input.read_line(&mut line).map_err(|e| e.to_string())?;
    if !matches!(line.trim(), "#?RADIANCE" | "#?RGBE") { return Err("not a Radiance HDR file".into()); }
    let mut format = false;
    loop {
        line.clear();
        if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 { return Err("truncated HDR header".into()); }
        if line.trim().is_empty() { break; }
        if line.trim() == "FORMAT=32-bit_rle_rgbe" { format = true; }
    }
    if !format { return Err("HDR requires FORMAT=32-bit_rle_rgbe".into()); }
    line.clear();
    input.read_line(&mut line).map_err(|e| e.to_string())?;
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.len() != 4 || !matches!((parts[0], parts[2]), ("-Y" | "+Y", "-X" | "+X") | ("-X" | "+X", "-Y" | "+Y")) { return Err("HDR resolution needs signed X and Y axes".into()); }
    let scan_height = parts[1].parse::<usize>().map_err(|_| "invalid HDR dimension")?;
    let scan_width = parts[3].parse::<usize>().map_err(|_| "invalid HDR dimension")?;
    let y_first = parts[0].ends_with('Y');
    let (w, h) = if y_first { (scan_width, scan_height) } else { (scan_height, scan_width) };
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|n| n > 16_777_216) { return Err("HDR dimensions exceed 16 million pixels".into()); }
    let mut rgbe = Vec::with_capacity(w * h);
    let mut first = [0; 4];
    input.read_exact(&mut first).map_err(|_| "truncated HDR pixels")?;
    if (8..=32767).contains(&scan_width) && first[0..2] == [2, 2] && first[2] & 128 == 0 {
        for y in 0..scan_height {
            let mut header = first;
            if y > 0 { input.read_exact(&mut header).map_err(|_| "truncated HDR scanline")?; }
            if header[0..2] != [2, 2] || ((header[2] as usize) << 8 | header[3] as usize) != scan_width { return Err("invalid HDR scanline width".into()); }
            let mut row = vec![[0; 4]; scan_width];
            for channel in 0..4 {
                let mut x = 0;
                while x < scan_width {
                    let mut count = [0];
                    input.read_exact(&mut count).map_err(|_| "truncated HDR RLE")?;
                    let run = count[0] > 128;
                    let n = if run { count[0] as usize - 128 } else { count[0] as usize };
                    if n == 0 || n > scan_width - x { return Err("invalid HDR RLE length".into()); }
                    if run {
                        let mut value = [0];
                        input.read_exact(&mut value).map_err(|_| "truncated HDR RLE run")?;
                        for p in &mut row[x..x + n] { p[channel] = value[0]; }
                    } else {
                        for p in &mut row[x..x + n] { input.read_exact(&mut p[channel..channel + 1]).map_err(|_| "truncated HDR RLE literal")?; }
                    }
                    x += n;
                }
            }
            rgbe.extend(row);
        }
    } else {
        let mut pixel = first;
        let mut shift = 0;
        loop {
            if pixel[0..3] == [1, 1, 1] {
                if rgbe.is_empty() || shift > 24 { return Err("invalid legacy HDR repeat".into()); }
                let n = (pixel[3] as usize) << shift;
                if n > w * h - rgbe.len() { return Err("HDR repeat exceeds image".into()); }
                rgbe.extend(std::iter::repeat_n(*rgbe.last().unwrap(), n));
                shift += 8;
            } else { rgbe.push(pixel); shift = 0; }
            if rgbe.len() == w * h { break; }
            input.read_exact(&mut pixel).map_err(|_| "truncated HDR pixels")?;
        }
    }
    let mut pixels = vec![Vec3::ZERO; w * h];
    for major in 0..scan_height {
        for minor in 0..scan_width {
            let p = rgbe[major * scan_width + minor];
            let scale = if p[3] == 0 { 0.0 } else { 2.0f32.powi(p[3] as i32 - 136) };
            let color = Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) * scale;
            if !color.is_finite() { return Err("HDR radiance exceeds float range".into()); }
            let (x, y, x_axis, y_axis) = if y_first { (minor, major, parts[2], parts[0]) } else { (major, minor, parts[0], parts[2]) };
            let yy = if y_axis == "-Y" { y } else { h - 1 - y };
            let xx = if x_axis == "+X" { x } else { w - 1 - x };
            pixels[yy * w + xx] = color;
        }
    }
    Ok((w, h, pixels))
}

pub(crate) fn number(v: &Value, key: &str, default: f32, lo: f32, hi: f32) -> Result<f32, String> {
    match v.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(n) => n.as_f64().filter(|x| x.is_finite() && *x >= lo as f64 && *x <= hi as f64).map(|x| x as f32).ok_or_else(|| format!("{key} must be a number in {lo}..{hi}")),
    }
}

pub(crate) fn vector(v: &Value, key: &str, default: Option<Vec3>) -> Result<Vec3, String> {
    if v[key].is_null() { return default.ok_or_else(|| format!("{key} is required")); }
    let a: [f32; 3] = serde_json::from_value(v[key].clone()).map_err(|_| format!("{key} must be [x,y,z]"))?;
    let p = Vec3::from(a);
    if !p.is_finite() { return Err(format!("{key} must be finite")); }
    Ok(p)
}

pub enum Light {
    Sun { direction: Vec3, size: f32, power: Vec3 },
    Area { position: Vec3, direction: Vec3, size: f32, power: Vec3 },
    Point { position: Vec3, power: Vec3 },
    /// A glowing ball (a flame, a lamp): `power` is its radiant intensity,
    /// like a point light's, spread over a sphere of `radius`, so its
    /// shadows have soft edges.
    Sphere { position: Vec3, radius: f32, power: Vec3 },
}

impl Light {
    pub fn parse(v: &Value) -> Result<Self, String> {
        let color = vector(v, "color", Some(Vec3::ONE))?;
        if color.min_element() < 0.0 { return Err("light color must be nonnegative linear RGB".into()); }
        let power = color * number(v, "intensity", 1.0, 0.0, 100000.0)?;
        match v["type"].as_str() {
            Some("sun") => {
                let dir = vector(v, "direction", None)?;
                if dir.length_squared() < 1e-12 { return Err("sun direction cannot be zero".into()); }
                Ok(Self::Sun { direction: dir.normalize(), size: number(v, "size", 0.53, 0.01, 90.0)?.to_radians() * 0.5, power })
            }
            Some("area") => {
                let dir = vector(v, "direction", Some(Vec3::NEG_Y))?;
                if dir.length_squared() < 1e-12 { return Err("area direction cannot be zero".into()); }
                Ok(Self::Area { position: vector(v, "position", None)?, direction: dir.normalize(), size: number(v, "size", 1.0, 0.0001, 1000.0)? * 0.5, power })
            }
            Some("point") => Ok(Self::Point { position: vector(v, "position", None)?, power }),
            Some("sphere") => Ok(Self::Sphere { position: vector(v, "position", None)?, radius: number(v, "radius", 0.05, 0.0001, 100.0)?, power }),
            _ => Err("light type must be sun, area, point or sphere".into()),
        }
    }

    /// Direction, irradiance weight and distance for a shadow ray.
    pub fn sample(&self, p: Vec3, u: Vec2) -> (Vec3, Vec3, f32) {
        match *self {
            Self::Sun { direction, size, power } => (shade::cone(direction, size, u), power, f32::MAX),
            Self::Point { position, power } => {
                let delta = position - p;
                (delta.normalize_or(Vec3::Y), power / delta.length_squared().max(1e-8), delta.length())
            }
            Self::Sphere { position, radius, power } => {
                let delta = position - p;
                let d2 = delta.length_squared();
                let d = d2.sqrt();
                if d <= radius * 1.0001 {
                    // Inside the ball: as bright as at its surface.
                    return (delta.normalize_or(Vec3::Y), power / (radius * radius), d);
                }
                // Even over the cone the ball fills: radiance x solid angle.
                let axis = delta / d;
                let sin2 = (radius * radius / d2).min(1.0);
                let cos_max = (1.0 - sin2).sqrt();
                let l = shade::cone(axis, cos_max.acos(), u);
                let along = l.dot(delta);
                let far = along - (radius * radius - (d2 - along * along)).max(0.0).sqrt();
                let solid = TAU * sin2 / (1.0 + cos_max);
                (l, power * (solid / (PI * radius * radius)), far.max(0.0))
            }
            Self::Area { position, direction, size, power } => {
                let frame = shade::Frame::new(direction);
                let r = size * u.x.sqrt();
                let phi = TAU * u.y;
                let q = position + frame.t * (r * phi.cos()) + frame.b * (r * phi.sin());
                let delta = q - p;
                let l = delta.normalize_or(Vec3::Y);
                (l, power * (PI * size * size * (-l).dot(direction).max(0.0) / delta.length_squared().max(1e-8)), delta.length())
            }
        }
    }
}

impl Light {
    /// Lights everywhere at once (suns) are always sampled; local ones
    /// (point, sphere, area) are picked by how much they could give.
    pub fn local(&self) -> Option<(Vec3, f32, f32)> {
        match *self {
            Self::Sun { .. } => None,
            Self::Point { position, power } => Some((position, 1e-3, luminance(power))),
            Self::Sphere { position, radius, power } => Some((position, radius, luminance(power))),
            // An area light's intensity facing it: radiance x disc area.
            Self::Area { position, size, power, .. } => Some((position, size, luminance(power) * PI * size * size)),
        }
    }
}

/// Lights split for sampling: every sun, and the local lights with what
/// picking needs (position, radius, brightness).
pub struct LightSet {
    pub suns: Vec<u32>,
    pub local: Vec<(u32, Vec3, f32, f32)>,
}

/// Up to this many local lights are all sampled at every point; beyond,
/// `PICKS` are picked by importance, so thirty candles cost two shadow
/// rays, not thirty.
pub const ALL_LOCAL: usize = 4;
pub const PICKS: usize = 2;

impl LightSet {
    pub fn new(lights: &[Light]) -> LightSet {
        let mut set = LightSet { suns: Vec::new(), local: Vec::new() };
        for (i, l) in lights.iter().enumerate() {
            match l.local() {
                None => set.suns.push(i as u32),
                Some((p, r, lum)) => set.local.push((i as u32, p, r, lum)),
            }
        }
        set
    }

    /// How much local light `k` could give at `p` (zero when it is wholly
    /// below the surface's plane `n`).
    pub fn importance(&self, k: usize, p: Vec3, n: Option<Vec3>) -> f32 {
        let (_, c, r, lum) = self.local[k];
        let delta = c - p;
        let d2 = delta.length_squared();
        if let Some(n) = n
            && delta.dot(n) < -r
        {
            return 0.0;
        }
        lum / d2.max(r * r).max(1e-6)
    }

    /// The local lights to sample at `p` (all of them when few, else
    /// `PICKS` chosen by importance), each with one over its chance. Suns
    /// are always sampled too (see `suns`).
    pub(super) fn pick(&self, p: Vec3, n: Option<Vec3>, rng: &mut Rng) -> Picks {
        let mut out = Picks { local: [(0, 0.0); ALL_LOCAL], count: 0 };
        if self.local.len() <= ALL_LOCAL {
            for (k, l) in self.local.iter().enumerate() {
                out.local[k] = (l.0, 1.0);
            }
            out.count = self.local.len();
            return out;
        }
        // Two passes over the weights (no allocation per shading point).
        let total: f32 = (0..self.local.len()).map(|k| self.importance(k, p, n)).sum();
        if total <= 0.0 {
            return out;
        }
        let mut xs = [0.0f32; PICKS];
        for x in &mut xs {
            *x = rng.f() * total;
        }
        let mut acc = 0.0;
        let mut chosen = [usize::MAX; PICKS];
        for k in 0..self.local.len() {
            let w = self.importance(k, p, n);
            for (c, x) in chosen.iter_mut().zip(&xs) {
                if *c == usize::MAX && *x < acc + w && w > 0.0 {
                    *c = k;
                }
            }
            acc += w;
        }
        for c in chosen {
            // Rounding can leave a pick past the last weight: take the last lit one.
            let k = if c == usize::MAX { match (0..self.local.len()).rev().find(|&k| self.importance(k, p, n) > 0.0) { Some(k) => k, None => continue } } else { c };
            let w = self.importance(k, p, n);
            out.local[out.count] = (self.local[k].0, total / (w * PICKS as f32));
            out.count += 1;
        }
        out
    }

    /// Calls `f(light, weight)` for every light to sample at `p`.
    #[cfg(test)]
    pub(super) fn each(&self, p: Vec3, n: Option<Vec3>, rng: &mut Rng, mut f: impl FnMut(u32, f32)) {
        for &s in &self.suns {
            f(s, 1.0);
        }
        for (i, w) in self.pick(p, n, rng).iter() {
            f(i, w);
        }
    }
}

/// Local lights picked at a point (see `LightSet::pick`).
pub struct Picks {
    pub local: [(u32, f32); ALL_LOCAL],
    pub count: usize,
}

impl Picks {
    pub fn iter(&self) -> impl Iterator<Item = (u32, f32)> + '_ {
        self.local[..self.count].iter().copied()
    }
}

/// Mist and fog: a medium that dims what is behind it and glows where
/// lights shine through it (single scattering on camera rays), thinning
/// out with height above `height` over `falloff` meters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Extinction per meter at and below `height`.
    pub density: f32,
    /// Share of extinction that scatters (color), the rest is absorbed.
    pub albedo: Vec3,
    pub height: f32,
    /// 0 = the same everywhere.
    pub falloff: f32,
    /// Henyey-Greenstein g: > 0 glows toward lights.
    pub anisotropy: f32,
    /// Rays that leave the scene pass this much fog.
    pub distance: f32,
    pub noise: f32,
    pub noise_scale: f32,
    pub seed: u32,
}

impl Default for Fog {
    fn default() -> Self { Self { density: 0.06, albedo: Vec3::splat(0.9), height: 0.0, falloff: 1.5, anisotropy: 0.35, distance: 40.0, noise: 0.0, noise_scale: 0.5, seed: 1 } }
}
impl Fog {
    /// `fog`: true, a density, or {density, color, height, falloff,
    /// anisotropy, distance}.
    pub fn parse(v: &Value) -> Result<Option<Fog>, String> {
        let mut fog = Fog::default();
        match v {
            Value::Null | Value::Bool(false) => return Ok(None),
            Value::Bool(true) => {}
            Value::Number(_) => fog.density = v.as_f64().filter(|x| x.is_finite() && (0.0..=50.0).contains(x)).ok_or("fog density must be 0..50 per meter")? as f32,
            Value::Object(_) => {
                fog.density = number(v, "density", fog.density, 0.0, 50.0)?;
                fog.albedo = vector(v, "color", Some(fog.albedo))?;
                if fog.albedo.min_element() < 0.0 || fog.albedo.max_element() > 1.0 { return Err("fog color is a scattering albedo, each 0..1".into()); }
                fog.height = number(v, "height", 0.0, -10000.0, 10000.0)?;
                fog.falloff = number(v, "falloff", fog.falloff, 0.0, 10000.0)?;
                fog.anisotropy = number(v, "anisotropy", fog.anisotropy, -0.95, 0.95)?;
                fog.noise = number(v, "noise", 0.0, 0.0, 1.0)?;
                fog.noise_scale = number(v, "noise_scale", 0.5, 0.0001, 100.0)?;
                let seed = number(v, "seed", 1.0, 0.0, 16777215.0)?;
                if seed.fract() != 0.0 { return Err("fog.seed must be an integer".into()); }
                fog.seed = seed as u32;
                fog.distance = number(v, "distance", fog.distance, 0.01, 100000.0)?;
            }
            _ => return Err("fog must be true, a density, or {density, color, height, falloff, anisotropy, distance}".into()),
        }
        Ok((fog.density > 0.0).then_some(fog))
    }

    pub fn density_at(&self, y: f32) -> f32 {
        if self.falloff <= 0.0 || y <= self.height { self.density } else { self.density * (-(y - self.height) / self.falloff).exp() }
    }

    /// Smooth lattice value noise, with identical integer hashing on the GPU.
    pub fn density_at_point(&self, p: Vec3) -> f32 {
        if self.noise == 0.0 { return self.density_at(p.y); }
        fn hash(x: u32) -> f32 {
            let mut x = x; x ^= x >> 16; x = x.wrapping_mul(0x7feb352d); x ^= x >> 15; x = x.wrapping_mul(0x846ca68b); x ^= x >> 16;
            (x & 0xffffff) as f32 / 16777216.0
        }
        let q = p*self.noise_scale;
        let i = q.floor().as_ivec3(); let f = q-q.floor(); let f = f*f*(Vec3::splat(3.0)-2.0*f);
        let mut n = 0.0;
        for z in 0..2 { for y in 0..2 { for x in 0..2 {
            let k = ((i.x+x) as u32).wrapping_mul(73856093) ^ ((i.y+y) as u32).wrapping_mul(19349663) ^ ((i.z+z) as u32).wrapping_mul(83492791) ^ self.seed;
            let weight = |v: f32, bit: i32| if bit == 0 { 1.0-v } else { v };
            n += hash(k)*weight(f.x,x)*weight(f.y,y)*weight(f.z,z);
        } } }
        self.density_at(p.y) * (1.0 + self.noise*(2.0*n-1.0))
    }

    /// Optical depth from `o` along unit `d` for `s` meters.
    pub fn depth(&self, o: Vec3, d: Vec3, s: f32) -> f32 {
        let s = s.min(self.distance).max(0.0);
        if self.noise > 0.0 {
            // Match the GPU's bounded midpoint quadrature; resolve small-scale
            // detail with up to 96 steps, retaining a bounded cost on sky rays.
            let steps = (s*self.noise_scale*4.0).ceil().clamp(24.0, 96.0) as u32;
            return (0..steps).map(|i| self.density_at_point(o+d*(s*(i as f32+0.5)/steps as f32))).sum::<f32>()*s/steps as f32;
        }
        if self.falloff <= 0.0 {
            return self.density * s;
        }
        let y0 = o.y;
        if d.y.abs() < 1e-5 {
            return self.density_at(y0) * s;
        }
        // Split where the ray crosses `height`: dense below, exponential above.
        let cross = ((self.height - y0) / d.y).clamp(0.0, s);
        let (below, above) = if (y0 <= self.height) == (d.y > 0.0) {
            // Starts on the dense side going toward the line, or ...
            if y0 <= self.height { ((0.0, cross), (cross, s)) } else { ((cross, s), (0.0, cross)) }
        } else if y0 <= self.height {
            ((0.0, s), (s, s))
        } else {
            ((s, s), (0.0, s))
        };
        let mut tau = self.density * (below.1 - below.0);
        if above.1 > above.0 {
            let e = |t: f32| (-((y0 + d.y * t) - self.height).max(0.0) / self.falloff).exp();
            tau += self.density * self.falloff / d.y * (e(above.0) - e(above.1));
        }
        tau.max(0.0)
    }

    pub fn transmittance(&self, o: Vec3, d: Vec3, s: f32) -> f32 {
        (-self.depth(o, d, s)).exp()
    }

    /// Phase function for light going along `l_in` turning into `d_out`
    /// (cosine between the two directions of travel).
    pub fn phase(&self, cos: f32) -> f32 {
        let g = self.anisotropy;
        (1.0 - g * g) / (4.0 * PI * (1.0 + g * g - 2.0 * g * cos).max(1e-6).powf(1.5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_environment_intensity_and_rotation_preserve_sampling() {
        let base=Environment::preset("golden_hour").unwrap();
        let scaled=Environment::parse(&serde_json::json!({"preset":"golden_hour","intensity":0.2,"rotate":90})).unwrap();
        assert!((scaled.irradiance()-base.irradiance()*0.2).abs()<1e-6);
        let d=base.sun.unwrap().0;
        let rotated=Vec3::new(-d.z,d.y,d.x);
        assert!((scaled.at(rotated)-base.at(d)*0.2).abs().max_element()<0.001);
        assert!((scaled.pdf(rotated)-base.pdf(d)).abs()<0.01);
        let black=Environment::parse(&serde_json::json!({"preset":"golden_hour","intensity":0})).unwrap();
        assert_eq!(black.irradiance(),0.0);
        let mut rng=Rng::new(3,7);
        assert_eq!(black.sample(&mut rng).1,Vec3::ZERO);
        assert!(Environment::parse(&serde_json::json!({"preset":"night","hdr":"/tmp/a.hdr"})).is_err());
    }

    #[test]
    fn hdr_raw_rle_legacy_and_orientation() {
        let mut raw = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n+Y 2 -X 2\n".to_vec();
        raw.extend([[128, 64, 32, 129], [64, 128, 32, 129], [32, 64, 128, 129], [0, 0, 0, 0]].concat());
        let (w, h, p) = read_hdr(&raw).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(p[3], Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(p[0], Vec3::ZERO);
        let mut transposed = b"#?RGBE\nFORMAT=32-bit_rle_rgbe\n\n+X 3 -Y 2\n".to_vec();
        transposed.extend((1..=6).flat_map(|x| [x * 16, 0, 0, 129]));
        let (w, h, p) = read_hdr(&transposed).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(p.iter().map(|c| c.x).collect::<Vec<_>>(), vec![0.125, 0.375, 0.625, 0.25, 0.5, 0.75]);
        let mut rle = b"#?RGBE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 8\n".to_vec();
        rle.extend([2, 2, 0, 8, 136, 128, 8, 64, 64, 64, 64, 64, 64, 64, 64, 136, 32, 136, 129]);
        assert!(read_hdr(&rle).unwrap().2.iter().all(|p| *p == Vec3::new(1.0, 0.5, 0.25)));
        rle.pop();
        assert!(read_hdr(&rle).is_err());
        let mut legacy = b"#?RGBE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 4\n".to_vec();
        legacy.extend([128, 128, 128, 129, 1, 1, 1, 3]);
        assert_eq!(read_hdr(&legacy).unwrap().2, vec![Vec3::ONE; 4]);
        assert!(read_hdr(b"#?RGBE\nFORMAT=32-bit_rle_rgbe\n\n-Y 100000 +X 100000\n").is_err());
    }

    #[test]
    fn environment_pdf_and_luminance_distribution() {
        let mut pixels = vec![Vec3::ONE; 32];
        pixels[12] = Vec3::splat(100.0);
        let env = Environment::map(8, 4, pixels, 37.0);
        let mut rng = Rng::new(917, 531);
        let mut integral = 0.0;
        let mut bright = 0;
        for _ in 0..100_000 {
            integral += env.pdf(shade::sphere(rng.v2())) * (4.0 * PI);
            let (d, _, pdf) = env.sample(&mut rng);
            assert!((pdf - env.pdf(d)).abs() < 1e-5);
            let (x, y) = env.coords(d);
            if y * 8 + x == 12 { bright += 1; }
        }
        assert!((integral / 100_000.0 - 1.0).abs() < 0.025, "pdf integral {}", integral / 100_000.0);
        let expected = (env.cdf[13] - env.cdf[12]) / env.total;
        assert!((bright as f64 / 100_000.0 - expected).abs() < 0.01);
        let pole = Environment::map(1, 16384, vec![Vec3::ONE; 16384], 0.0);
        assert!((pole.pdf(Vec3::Y) - 1.0 / (4.0 * PI)).abs() < 1e-6);
        let black = Environment::map(2, 2, vec![Vec3::ZERO; 4], 0.0);
        assert!((black.sample(&mut rng).2 - 1.0 / (4.0 * PI)).abs() < 1e-6);
    }

    #[test]
    fn procedural_sun_is_sampled_at_its_flux_weight() {
        let env = Environment::preset("golden_hour").unwrap();
        let (sun, size, _) = env.sun.unwrap();
        let mut rng = Rng::new(124, 173);
        let mut hits = 0;
        for _ in 0..20_000 {
            let (d, _, pdf) = env.sample(&mut rng);
            assert!(pdf > 0.0 && pdf.is_finite());
            if d.dot(sun) >= size.cos() { hits += 1; }
        }
        assert!((hits as f32 / 20_000.0 - env.sun_chance).abs() < 0.015);
    }

    #[test]
    fn sphere_lights_match_points_far_away_and_soften_near() {
        let s = Light::Sphere { position: Vec3::ZERO, radius: 0.05, power: Vec3::ONE };
        let p = Light::Point { position: Vec3::ZERO, power: Vec3::ONE };
        let far = Vec3::new(0.0, 0.0, 3.0);
        assert!((s.sample(far, Vec2::new(0.3, 0.7)).1.x - p.sample(far, Vec2::ZERO).1.x).abs() < 1e-3);
        let (l, _, t) = s.sample(far, Vec2::new(0.99, 0.2));
        assert!((far + l * t).length() - 0.05 < 1e-3, "shadow rays stop at the ball");
        // Near it, directions spread over the ball.
        let near = Vec3::new(0.0, 0.0, 0.2);
        let spread = s.sample(near, Vec2::new(1.0, 0.0)).0.dot(Vec3::NEG_Z);
        assert!(spread < 0.98);
    }

    #[test]
    fn many_lights_are_picked_by_importance_without_bias() {
        let lights: Vec<Light> = (0..30).map(|i| Light::Sphere { position: Vec3::new(i as f32 * 0.5, 1.0, 0.0), radius: 0.02, power: Vec3::splat(0.5 + (i % 3) as f32) }).collect();
        let set = LightSet::new(&lights);
        let p = Vec3::new(3.0, 0.0, 0.2);
        // The exact sum of every light's irradiance proxy, against picks.
        let exact: f32 = (0..30).map(|k| set.importance(k, p, None)).sum();
        let mut rng = Rng::new(5, 9);
        let mut sum = 0.0;
        let mut rays = 0;
        let n = 20_000;
        for _ in 0..n {
            set.each(p, None, &mut rng, |i, w| {
                rays += 1;
                sum += w * set.importance(set.local.iter().position(|l| l.0 == i).unwrap(), p, None);
            });
        }
        assert_eq!(rays, n * PICKS);
        assert!((sum / n as f32 - exact).abs() < exact * 1e-3, "{} vs {exact}", sum / n as f32);
        // A light under the surface is never picked.
        let below = LightSet::new(&[Light::Point { position: Vec3::NEG_Y, power: Vec3::ONE }, Light::Point { position: Vec3::Y, power: Vec3::ONE }, Light::Point { position: Vec3::Y * 2.0, power: Vec3::ONE }, Light::Point { position: Vec3::Y * 3.0, power: Vec3::ONE }, Light::Point { position: Vec3::Y * 4.0, power: Vec3::ONE }]);
        below.each(Vec3::ZERO, Some(Vec3::Y), &mut rng, |i, _| assert_ne!(i, 0));
    }

    #[test]
    fn fog_depth_integrates_its_density() {
        let fog = Fog { density: 0.3, albedo: Vec3::ONE, height: 1.0, falloff: 0.7, anisotropy: 0.2, distance: 1000.0, ..Default::default() };
        for (o, d, s) in [(Vec3::new(0.0, 0.2, 0.0), Vec3::new(0.3, 0.8, 0.1), 5.0), (Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.2, -0.9, 0.0), 6.0), (Vec3::ZERO, Vec3::X, 4.0), (Vec3::Y * 2.0, Vec3::new(0.6, 0.2, 0.0), 3.0)] {
            let d = d.normalize();
            let steps = 20_000;
            let numeric: f32 = (0..steps).map(|i| fog.density_at((o + d * (s * (i as f32 + 0.5) / steps as f32)).y) * s / steps as f32).sum();
            assert!((fog.depth(o, d, s) - numeric).abs() < 1e-3, "{o} {d}: {} vs {numeric}", fog.depth(o, d, s));
        }
        // The phase function integrates to one.
        let mut rng = Rng::new(3, 4);
        let mean: f32 = (0..50_000).map(|_| fog.phase(shade::sphere(rng.v2()).z) * 4.0 * PI).sum::<f32>() / 50_000.0;
        assert!((mean - 1.0).abs() < 0.02, "{mean}");
    }

    #[test]
    fn local_lights_fall_off_and_area_is_one_sided() {
        let p = Light::Point { position: Vec3::ZERO, power: Vec3::ONE };
        let a = p.sample(Vec3::Z, Vec2::ZERO);
        let b = p.sample(Vec3::Z * 2.0, Vec2::ZERO);
        assert_eq!(a.1, b.1 * 4.0);
        assert_eq!(b.2, 2.0);
        let a = Light::Area { position: Vec3::ZERO, direction: Vec3::Z, size: 1.0, power: Vec3::ONE };
        assert!(a.sample(Vec3::Z, Vec2::ZERO).1.x > 0.0);
        assert_eq!(a.sample(Vec3::NEG_Z, Vec2::ZERO).1, Vec3::ZERO);
    }
}
