//! The part builders: one function per kind, each in the part's own
//! space (base on y = 0, +Y out of the surface, +X along the part).
//! Sizes come from `Part::dims` [X, Y, Z] or, for parts between points,
//! `points` and `tube_radius`.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Mat4, Quat, Vec2, Vec3};

use super::geo::{Geo, Role, along, circle, rect};
use super::{Kind, Part};
use crate::envkit::Rand;

pub fn build(g: &mut Geo, p: &Part) {
    let s = p.dims();
    let style = p.style();
    match p.kind {
        Kind::Bolt => bolt(g, s, style),
        Kind::Nut => nut(g, s, style),
        Kind::Washer => washer(g, s.x * 0.5, s.y, Role::Metal),
        Kind::Hinge => hinge(g, s, p.count, style),
        Kind::Latch => latch(g, s, style),
        Kind::Clamp => clamp(g, s, style),
        Kind::Handle => handle(g, s, style),
        Kind::Vent => vent(g, s, p.count, style),
        Kind::Hatch => hatch(g, s, style, p.seed),
        Kind::Piston => piston(g, p),
        Kind::Cable => cable(g, p),
        Kind::Hose => hose(g, p),
        Kind::CorrugatedTube => corrugated(g, p),
        Kind::Light => light(g, s, style),
        Kind::Antenna => antenna(g, s, style),
        Kind::Sensor => sensor(g, s, style),
        Kind::Exhaust => exhaust(g, s, style),
        Kind::ArmorPlate => armor_plate(g, s, p.count, style),
        Kind::TrackLink => track_links(g, s, p.count.unwrap_or(1), style),
        Kind::Gear => gear(g, s, p.count, style),
        Kind::Spring => spring(g, p),
        Kind::Pipe => pipe(g, p),
        Kind::Connector => connector(g, s, style),
        Kind::AmmoBox => ammo_box(g, s, style),
        Kind::JerryCan => jerry_can(g, s),
        Kind::Strap => strap(g, p),
        Kind::Net => net(g, s, p.count),
        Kind::Stencil => stencil(g, s, p.text.as_deref().unwrap_or("07"), style),
        Kind::Seam => seam(g, s, p.tube_radius(), style),
        Kind::Greeble => greeble(g, s, style, p.seed),
        Kind::Clip => clip(g, s, style),
    }
}

fn t(x: f32, y: f32, z: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(x, y, z))
}

fn v2(r: f32, y: f32) -> Vec2 {
    Vec2::new(r, y)
}

/// A hexagon of circumradius `r` (flats facing ±Z).
fn hex(r: f32) -> Vec<Vec2> {
    (0..6).map(|i| { let a = i as f32 / 6.0 * TAU; Vec2::new(a.cos(), a.sin()) * r }).collect()
}

/// A disc (or ring when `inner` > 0) from y0 to y1, top edge chamfered.
#[allow(clippy::too_many_arguments)]
fn disc(g: &mut Geo, r: f32, inner: f32, y0: f32, y1: f32, bevel: f32, segs: usize, role: Role) {
    let b = bevel.min((y1 - y0) * 0.45).min((r - inner) * 0.45).max(0.0);
    let prof = if inner > 0.0 {
        vec![v2(inner, y0), v2(r, y0), v2(r, y1 - b), v2(r - b, y1), v2(inner, y1), v2(inner, y0)]
    } else {
        vec![v2(0.0, y0), v2(r, y0), v2(r, y1 - b), v2(r - b, y1), v2(0.0, y1)]
    };
    g.lathe(&prof, segs, role, 30.0);
}

// ---------- fasteners ----------

fn bolt(g: &mut Geo, s: Vec3, style: &str) {
    let (r, h) = (s.x * 0.5, s.y);
    let segs = g.segs(16);
    match style {
        "socket" => {
            g.lathe(&[v2(0.0, 0.0), v2(r, 0.0), v2(r, h * 0.82), v2(r * 0.86, h), v2(r * 0.52, h), v2(r * 0.52, h * 0.62), v2(0.0, h * 0.62)], segs, Role::Paint, 30.0);
            g.prism(&hex(r * 0.44), h * 0.62, h * 0.66, 0.0, 0.0, Role::Seam, false);
        }
        "rivet" => {
            let mut prof = vec![v2(0.0, 0.0), v2(r, 0.0), v2(r, h * 0.12)];
            for i in 1..=8 {
                let a = i as f32 / 8.0 * FRAC_PI_2;
                prof.push(v2(r * a.cos(), h * 0.12 + h * 0.88 * a.sin()));
            }
            g.lathe(&prof, segs, Role::Paint, 40.0);
        }
        "countersunk" => {
            let hh = h * 0.3;
            g.lathe(&[v2(0.0, 0.0), v2(r, 0.0), v2(r, hh * 0.4), v2(r * 0.88, hh), v2(0.0, hh)], segs, Role::Paint, 30.0);
            g.boxy(Vec3::new(0.0, hh + h * 0.01, 0.0), Vec3::new(r * 1.5, h * 0.04, r * 0.2), 0.0, Role::Seam);
        }
        "carriage" => {
            let (r, h) = (r * 1.15, h * 0.55);
            let mut prof = vec![v2(0.0, 0.0), v2(r, 0.0)];
            for i in 1..=7 {
                let a = i as f32 / 7.0 * FRAC_PI_2;
                prof.push(v2(r * a.cos(), h * a.sin()));
            }
            g.lathe(&prof, segs, Role::Paint, 40.0);
        }
        _ => {
            // Hex head on a washer, the thread end showing.
            disc(g, r * 1.22, 0.0, 0.0, h * 0.14, h * 0.04, segs, Role::Dark);
            g.prism(&hex(r), h * 0.14, h * 0.8, r * 0.16, r * 0.04, Role::Paint, false);
            g.lathe(&[v2(0.0, h * 0.8), v2(r * 0.42, h * 0.8), v2(r * 0.42, h * 0.93), v2(r * 0.34, h), v2(0.0, h)], g.segs(10), Role::Metal, 30.0);
        }
    }
}

fn nut(g: &mut Geo, s: Vec3, style: &str) {
    let (r, h) = (s.x * 0.5, s.y);
    let segs = g.segs(16);
    disc(g, r * 1.25, 0.0, 0.0, h * 0.12, h * 0.03, segs, Role::Dark);
    match style {
        "flange" => {
            disc(g, r * 1.12, 0.0, h * 0.12, h * 0.26, h * 0.04, segs, Role::Metal);
            g.prism(&hex(r), h * 0.26, h * 0.72, r * 0.14, r * 0.06, Role::Metal, false);
        }
        "wing" => {
            g.lathe(&[v2(0.0, h * 0.12), v2(r * 0.7, h * 0.12), v2(r * 0.55, h * 0.72), v2(0.0, h * 0.72)], segs, Role::Metal, 30.0);
            for side in [-1.0, 1.0] {
                g.boxy(Vec3::new(side * r * 0.95, h * 0.5, 0.0), Vec3::new(r * 0.9, h * 0.55, r * 0.18), r * 0.05, Role::Metal);
            }
        }
        _ => g.prism(&hex(r), h * 0.12, h * 0.72, r * 0.14, r * 0.1, Role::Metal, false),
    }
    g.lathe(&[v2(0.0, h * 0.7), v2(r * 0.5, h * 0.7), v2(r * 0.5, h * 0.92), v2(r * 0.4, h), v2(0.0, h)], g.segs(10), Role::Metal, 30.0);
}

fn washer(g: &mut Geo, r: f32, h: f32, role: Role) {
    let ri = r * 0.48;
    g.lathe(&[v2(ri, 0.0), v2(r, 0.0), v2(r, h * 0.7), v2(r - h * 0.3, h), v2(ri, h), v2(ri, 0.0)], g.segs(18), role, 30.0);
}

/// A bolt head of diameter `d` standing on `at` (part space, on a face
/// whose normal is +Y).
fn bolt_at(g: &mut Geo, at: Vec3, d: f32, style: &str) {
    g.with(Mat4::from_translation(at), |g| bolt(g, Vec3::new(d, d * 0.5, d), style));
}

// ---------- hinges, latches, clamps, handles ----------

fn hinge(g: &mut Geo, s: Vec3, count: Option<u32>, style: &str) {
    let (len, dia, depth) = (s.x, s.y, s.z);
    let r = dia * 0.5;
    let n = count.unwrap_or(if style == "piano" { (len / (dia * 1.6)).round().clamp(3.0, 64.0) as u32 } else { 3 }).clamp(1, 64) as usize;
    let gap = len * 0.012;
    let seg = len / n as f32;
    let leaf_t = dia * 0.16;
    for side in [-1.0f32, 1.0] {
        let zc = side * (r * 0.4 + (depth * 0.5 - r * 0.4) * 0.5);
        g.boxy(Vec3::new(0.0, leaf_t * 0.5, zc), Vec3::new(len * 0.98, leaf_t, depth * 0.5 - r * 0.4), leaf_t * 0.3, Role::Metal);
        let bolts = if style == "piano" { (len / (depth * 0.6)).round().clamp(2.0, 8.0) as usize } else { 2 };
        for k in 0..bolts {
            let x = (k as f32 + 0.5) / bolts as f32 * len * 0.85 - len * 0.425;
            bolt_at(g, Vec3::new(x, leaf_t, zc + side * depth * 0.06), (depth * 0.16).min(dia * 0.5), "countersunk");
        }
    }
    let segs = g.segs(14);
    for i in 0..n {
        let x0 = -len * 0.5 + i as f32 * seg + gap * 0.5;
        let x1 = x0 + seg - gap;
        g.rod(Vec3::new(x0, r, 0.0), Vec3::new(x1, r, 0.0), r, r * 0.12, segs, Role::Metal);
    }
    for side in [-1.0f32, 1.0] {
        let x = side * len * 0.5;
        g.rod(Vec3::new(x, r, 0.0), Vec3::new(x + side * dia * 0.12, r, 0.0), r * 0.55, r * 0.1, segs, Role::Dark);
    }
}

fn latch(g: &mut Geo, s: Vec3, style: &str) {
    let (w, h, d) = (s.x, s.y, s.z);
    // Base with the pivot block (-Z), keeper (+Z).
    g.boxy(Vec3::new(0.0, h * 0.1, -d * 0.28), Vec3::new(w, h * 0.2, d * 0.42), h * 0.05, Role::Metal);
    g.boxy(Vec3::new(0.0, h * 0.32, -d * 0.36), Vec3::new(w * 0.7, h * 0.3, d * 0.14), h * 0.04, Role::Metal);
    g.rod(Vec3::new(-w * 0.42, h * 0.38, -d * 0.36), Vec3::new(w * 0.42, h * 0.38, -d * 0.36), h * 0.09, h * 0.02, g.segs(10), Role::Chrome);
    let tilt = match style { "toggle" => -0.45, "cam" => 0.05, _ => 0.12 };
    g.with(Mat4::from_translation(Vec3::new(0.0, h * 0.42, -d * 0.36)) * Mat4::from_rotation_x(tilt), |g| {
        g.boxy(Vec3::new(0.0, h * 0.12, d * 0.25), Vec3::new(w * 0.82, h * 0.2, d * 0.55), h * 0.06, Role::Paint);
        if style == "cam" {
            g.rod(Vec3::new(0.0, h * 0.2, d * 0.5), Vec3::new(0.0, h * 0.45, d * 0.5), w * 0.16, w * 0.04, g.segs(12), Role::Rubber);
        }
    });
    let hook = [Vec3::new(-w * 0.3, h * 0.5, -d * 0.2), Vec3::new(-w * 0.3, h * 0.42, d * 0.36), Vec3::new(w * 0.3, h * 0.42, d * 0.36), Vec3::new(w * 0.3, h * 0.5, -d * 0.2)];
    let path = fillet(&hook, w * 0.12, 4);
    g.tube(&path, &|_| w * 0.055, g.segs(8), Role::Chrome, true);
    g.boxy(Vec3::new(0.0, h * 0.14, d * 0.38), Vec3::new(w * 0.9, h * 0.28, d * 0.2), h * 0.05, Role::Metal);
    g.rod(Vec3::new(-w * 0.36, h * 0.3, d * 0.42), Vec3::new(w * 0.36, h * 0.3, d * 0.42), h * 0.07, h * 0.02, g.segs(10), Role::Metal);
    for (x, z) in [(-0.32, -0.42), (0.32, -0.42), (-0.32, 0.43), (0.32, 0.43)] {
        let y = if z < 0.0 { h * 0.2 } else { h * 0.28 };
        bolt_at(g, Vec3::new(w * x, y, d * z), w * 0.14, "socket");
    }
}

fn clamp(g: &mut Geo, s: Vec3, style: &str) {
    let (w, pipe, span) = (s.x, s.y, s.z);
    let pr = pipe * 0.5;
    let th = (pipe * 0.07).max(1e-4);
    if style == "saddle" {
        g.boxy(Vec3::new(0.0, pr * 0.25, 0.0), Vec3::new(w * 1.1, pr * 0.5, pipe * 1.1), pr * 0.05, Role::Dark);
    }
    let mut path = vec![Vec3::new(0.0, 0.0, -span * 0.5), Vec3::new(0.0, 0.0, -pr - th)];
    let mut ups = vec![Vec3::Y, Vec3::new(0.0, 1.0, -1.0).normalize()];
    path.push(Vec3::new(0.0, pr, -pr - th));
    ups.push(Vec3::NEG_Z);
    let n = g.segs(14);
    for i in 1..n {
        let a = PI - i as f32 / n as f32 * PI;
        let dir = Vec3::new(0.0, a.sin(), a.cos());
        path.push(Vec3::new(0.0, pr, 0.0) + dir * (pr + th));
        ups.push(dir);
    }
    path.extend([Vec3::new(0.0, pr, pr + th), Vec3::new(0.0, 0.0, pr + th), Vec3::new(0.0, 0.0, span * 0.5)]);
    ups.extend([Vec3::Z, Vec3::new(0.0, 1.0, 1.0).normalize(), Vec3::Y]);
    // The band's underside is the path; it lies outside the pipe.
    let path: Vec<Vec3> = path.iter().zip(&ups).map(|(p, u)| if u.y > 0.99 { *p } else { *p - *u * th }).collect();
    g.strip(&path, &ups, w, th, Role::Metal, true);
    for side in [-1.0f32, 1.0] {
        let z = side * (pr + th + (span * 0.5 - pr - th) * 0.5);
        bolt_at(g, Vec3::new(0.0, th, z), (w * 0.6).min(span * 0.2), "hex");
    }
}

fn handle(g: &mut Geo, s: Vec3, style: &str) {
    let (len, hh, dia) = (s.x, s.y, s.z);
    let r = dia * 0.5;
    let l = (len - dia * 2.0).max(dia);
    let segs = g.segs(12);
    if style == "recessed" {
        let (pw, pd) = (len, (len * 0.45).max(dia * 4.0));
        let f = pd * 0.12;
        g.frame(&rect(pw, pd, f), &rect(pw - 2.0 * f, pd - 2.0 * f, f * 0.5), hh * 0.35, hh * 0.1, 0.0, Role::Paint, Role::Dark);
        g.rod(Vec3::new(-l * 0.45, hh * 0.18, 0.0), Vec3::new(l * 0.45, hh * 0.18, 0.0), r, r * 0.2, segs, Role::Metal);
        for side in [-1.0f32, 1.0] {
            g.boxy(Vec3::new(side * l * 0.46, hh * 0.09, 0.0), Vec3::new(dia, hh * 0.18, dia * 1.6), dia * 0.1, Role::Metal);
        }
        return;
    }
    let corner = (hh * 0.45).min(r * 4.0);
    let pts = [Vec3::new(-l * 0.5, -r * 0.3, 0.0), Vec3::new(-l * 0.5, hh - r, 0.0), Vec3::new(l * 0.5, hh - r, 0.0), Vec3::new(l * 0.5, -r * 0.3, 0.0)];
    let path = fillet(&pts, corner, 6);
    g.tube(&path, &|_| r, segs, if style == "loop" { Role::Paint } else { Role::Metal }, true);
    for side in [-1.0f32, 1.0] {
        let x = side * l * 0.5;
        if style == "loop" {
            g.with(t(x, 0.0, 0.0), |g| disc(g, r * 2.0, 0.0, 0.0, r * 0.5, r * 0.15, segs, Role::Dark));
        } else {
            g.boxy(Vec3::new(x, r * 0.35, 0.0), Vec3::new(dia * 2.4, r * 0.7, dia * 2.4), r * 0.15, Role::Dark);
            g.rod(Vec3::new(x, r * 0.6, 0.0), Vec3::new(x, hh * 0.45, 0.0), r * 1.35, r * 0.2, segs, Role::Dark);
            for k in [-1.0f32, 1.0] {
                bolt_at(g, Vec3::new(x + k * dia * 0.85, r * 0.7, 0.0), dia * 0.5, "socket");
            }
        }
    }
}

// ---------- vents, hatches, plates ----------

fn vent(g: &mut Geo, s: Vec3, count: Option<u32>, style: &str) {
    let (w, h, d) = (s.x, s.y, s.z);
    if style == "fan" {
        let r = w.min(d) * 0.5;
        let n = g.segs(32);
        g.frame(&circle(r, n), &circle(r * 0.86, n), h, h * 0.2, h * 0.1, Role::Paint, Role::Dark);
        g.lathe(&[v2(0.0, h * 0.1), v2(r * 0.2, h * 0.1), v2(r * 0.2, h * 0.6), v2(r * 0.14, h * 0.75), v2(0.0, h * 0.75)], g.segs(16), Role::Metal, 30.0);
        let blades = count.unwrap_or(7).clamp(3, 16) as usize;
        for i in 0..blades {
            let a = i as f32 / blades as f32 * TAU;
            g.with(Mat4::from_rotation_y(a) * t(r * 0.52, h * 0.42, 0.0) * Mat4::from_rotation_x(0.6), |g| {
                g.boxy(Vec3::ZERO, Vec3::new(r * 0.62, h * 0.06, r * 0.3), 0.0, Role::Dark);
            });
        }
        for i in 0..4 {
            let a = i as f32 / 4.0 * TAU + 0.4;
            g.with(Mat4::from_rotation_y(-a), |g| g.boxy(Vec3::new(r * 0.55, h * 0.85, 0.0), Vec3::new(r * 0.8, h * 0.1, h * 0.12), 0.0, Role::Metal));
        }
        return;
    }
    let f = w.min(d) * 0.1;
    let (iw, id) = (w - 2.0 * f, d - 2.0 * f);
    g.frame(&rect(w, d, f * 0.6), &rect(iw, id, f * 0.3), h, h * 0.22, h * 0.12, Role::Paint, Role::Dark);
    let floor = h * 0.12;
    match style {
        "grille" | "mesh" => {
            let k = if style == "mesh" { 3.0 } else { 1.0 };
            let nx = ((iw / (h * 0.9)) * k).round().clamp(2.0, 40.0) as usize;
            let nz = ((id / (h * 0.9)) * k).round().clamp(2.0, 40.0) as usize;
            let rr = h * 0.06 / k.sqrt();
            let segs = if style == "mesh" { 4 } else { g.segs(6) };
            for i in 1..nx {
                let x = -iw * 0.5 + i as f32 / nx as f32 * iw;
                g.rod(Vec3::new(x, h * 0.72, -id * 0.5), Vec3::new(x, h * 0.72, id * 0.5), rr, 0.0, segs, Role::Dark);
            }
            for j in 1..nz {
                let z = -id * 0.5 + j as f32 / nz as f32 * id;
                g.rod(Vec3::new(-iw * 0.5, h * 0.72 + rr * 1.6, z), Vec3::new(iw * 0.5, h * 0.72 + rr * 1.6, z), rr, 0.0, segs, Role::Dark);
            }
        }
        "slots" => {
            let n = count.unwrap_or((id / (h * 0.9)).round().clamp(2.0, 24.0) as u32).max(2) as usize;
            let pitch = id / n as f32;
            for i in 1..n {
                let z = -id * 0.5 + i as f32 * pitch;
                g.boxy(Vec3::new(0.0, floor + (h * 0.92 - floor) * 0.5, z), Vec3::new(iw, h * 0.92 - floor, pitch * 0.45), pitch * 0.06, Role::Paint);
            }
        }
        _ => {
            let n = count.unwrap_or((id / (h * 0.75)).round().clamp(3.0, 24.0) as u32).max(2) as usize;
            let pitch = id / n as f32;
            for i in 0..n {
                let z = -id * 0.5 + (i as f32 + 0.5) * pitch;
                g.with(t(0.0, floor + (h - floor) * 0.5, z) * Mat4::from_rotation_x(-0.75), |g| {
                    g.boxy(Vec3::ZERO, Vec3::new(iw, h * 0.07, pitch * 1.2), h * 0.015, Role::Paint);
                });
            }
        }
    }
}

fn hatch(g: &mut Geo, s: Vec3, style: &str, seed: u64) {
    let (w, th, d) = (s.x, s.y, s.z);
    let m = w.min(d);
    let fw = m * 0.06;
    let gap = m * 0.014;
    g.frame(&rect(w, d, fw), &rect(w - 2.0 * fw, d - 2.0 * fw, fw * 0.5), th * 0.8, th * 0.25, th * 0.1, Role::Paint, Role::Seam);
    let (dw, dd) = (w - 2.0 * fw - 2.0 * gap, d - 2.0 * fw - 2.0 * gap);
    g.prism(&rect(dw, dd, fw * 0.45), 0.0, th, th * 0.3, 0.0, Role::Paint, false);
    // Hinges on -Z, handle or latch on +Z, bolts in the corners.
    let hr = th * 0.45;
    for side in [-1.0f32, 1.0] {
        let x = side * dw * 0.28;
        let z = -dd * 0.5 - gap * 0.5;
        g.boxy(Vec3::new(x, th * 0.95, z + dd * 0.06), Vec3::new(w * 0.14, th * 0.2, dd * 0.12), th * 0.05, Role::Metal);
        g.rod(Vec3::new(x - w * 0.07, th * 0.9 + hr * 0.5, z), Vec3::new(x + w * 0.07, th * 0.9 + hr * 0.5, z), hr, hr * 0.15, g.segs(10), Role::Metal);
    }
    match style {
        "latch" => g.with(t(0.0, th, dd * 0.5 - dd * 0.12) , |g| latch(g, Vec3::new(m * 0.16, m * 0.06, m * 0.3), "draw")),
        "plain" => {}
        "vented" => g.with(t(0.0, th, 0.0), |g| vent(g, Vec3::new(dw * 0.5, th * 0.8, dd * 0.4), None, "louver")),
        _ => g.with(t(0.0, th, dd * 0.5 - m * 0.1), |g| handle(g, Vec3::new(m * 0.35, m * 0.07, m * 0.035), "bar")),
    }
    let bd = (m * 0.05).max(th * 0.8);
    let mut rng = Rand::new(seed ^ 0x4A7C);
    let style_b = if rng.chance(0.5) { "countersunk" } else { "socket" };
    for (sx, sz) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        bolt_at(g, Vec3::new(sx * (dw * 0.5 - bd * 1.1), th, sz * (dd * 0.5 - bd * 1.1)), bd, style_b);
    }
}

fn armor_plate(g: &mut Geo, s: Vec3, count: Option<u32>, style: &str) {
    let (w, th, d) = (s.x, s.y, s.z);
    let m = w.min(d);
    let c = m * 0.07;
    g.prism(&rect(w, d, c), 0.0, th, th * 0.4, 0.0, Role::Paint, false);
    let mut top = th;
    if style == "layered" {
        g.with(t(-w * 0.08, th, d * 0.06), |g| g.prism(&rect(w * 0.62, d * 0.58, c * 0.8), 0.0, th * 0.8, th * 0.35, 0.0, Role::Paint, false));
        top = th;
    }
    if style == "plain" {
        return;
    }
    let bd = (th * 1.4).clamp(m * 0.025, m * 0.09);
    let e = bd * 1.25 + c * 0.3;
    let long = w.max(d);
    let n = count.map_or_else(|| ((long / (bd * 7.0)).round() as usize).clamp(2, 12), |n| n.max(2) as usize);
    let along_x = w >= d;
    let (lx, lz) = (w * 0.5 - e, d * 0.5 - e);
    let mut spots = Vec::new();
    let short_n = ((n as f32 * w.min(d) / long).round() as usize).max(2);
    let (nx, nz) = if along_x { (n, short_n) } else { (short_n, n) };
    for i in 0..nx {
        let x = -lx + 2.0 * lx * i as f32 / (nx - 1) as f32;
        spots.push(Vec2::new(x, -lz));
        spots.push(Vec2::new(x, lz));
    }
    for j in 1..nz - 1 {
        let z = -lz + 2.0 * lz * j as f32 / (nz - 1) as f32;
        spots.push(Vec2::new(-lx, z));
        spots.push(Vec2::new(lx, z));
    }
    for p in spots {
        bolt_at(g, Vec3::new(p.x, top, p.y), bd, "hex");
    }
}

fn track_links(g: &mut Geo, s: Vec3, count: u32, style: &str) {
    let (pitch, h, w) = (s.x, s.y, s.z);
    let n = count.clamp(1, 64) as usize;
    for i in 0..n {
        let x = (i as f32 - (n as f32 - 1.0) * 0.5) * pitch;
        g.with(t(x, 0.0, 0.0), |g| {
            g.boxy(Vec3::new(0.0, h * 0.2, 0.0), Vec3::new(pitch * 0.9, h * 0.4, w), h * 0.06, Role::Metal);
            // Pin bosses at the link ends, the end connectors over them.
            for side in [-1.0f32, 1.0] {
                g.rod(Vec3::new(side * pitch * 0.42, h * 0.22, -w * 0.5), Vec3::new(side * pitch * 0.42, h * 0.22, w * 0.5), h * 0.2, h * 0.04, g.segs(10), Role::Dark);
                for zs in [-1.0f32, 1.0] {
                    g.boxy(Vec3::new(side * pitch * 0.42, h * 0.24, zs * w * 0.45), Vec3::new(pitch * 0.2, h * 0.48, w * 0.1), h * 0.04, Role::Metal);
                }
            }
            if style == "rubber_pad" {
                g.boxy(Vec3::new(0.0, h * 0.55, 0.0), Vec3::new(pitch * 0.6, h * 0.3, w * 0.7), h * 0.08, Role::Rubber);
            } else {
                for gx in [-0.2f32, 0.2] {
                    let x0 = gx * pitch;
                    g.prism(&[Vec2::new(x0 - pitch * 0.08, -w * 0.4), Vec2::new(x0 + pitch * 0.08, -w * 0.4), Vec2::new(x0 + pitch * 0.08, w * 0.4), Vec2::new(x0 - pitch * 0.08, w * 0.4)], h * 0.4, h * 0.85, h * 0.06, 0.0, Role::Metal, false);
                }
            }
            g.boxy(Vec3::new(0.0, h * 0.5, 0.0), Vec3::new(pitch * 0.3, h * 0.25, w * 0.14), h * 0.05, Role::Dark);
        });
    }
}

fn gear(g: &mut Geo, s: Vec3, count: Option<u32>, style: &str) {
    let (d, th) = (s.x, s.y);
    let r = d * 0.5;
    let n = count.unwrap_or(18).clamp(6, 64) as usize;
    let root = r * if style == "sprocket" { 0.8 } else { 0.86 };
    let (tip_w, root_w) = if style == "sprocket" { (0.12, 0.55) } else { (0.28, 0.5) };
    let mut prof = Vec::with_capacity(n * 4);
    for i in 0..n {
        let a = i as f32 / n as f32 * TAU;
        let step = TAU / n as f32;
        let at = |ang: f32, rad: f32| Vec2::new(ang.cos(), ang.sin()) * rad;
        prof.push(at(a - step * root_w * 0.5, root));
        prof.push(at(a - step * tip_w * 0.5, r));
        prof.push(at(a + step * tip_w * 0.5, r));
        prof.push(at(a + step * root_w * 0.5, root));
    }
    g.prism(&prof, 0.0, th, th * 0.12, 0.0, Role::Metal, false);
    g.lathe(&[v2(0.0, th), v2(r * 0.32, th), v2(r * 0.32, th * 1.5), v2(r * 0.26, th * 1.6), v2(r * 0.14, th * 1.6), v2(r * 0.14, th * 1.3), v2(0.0, th * 1.3)], g.segs(20), Role::Metal, 30.0);
    for k in 0..5 {
        let a = k as f32 / 5.0 * TAU;
        let c = Vec2::new(a.cos(), a.sin()) * r * 0.58;
        g.with(t(c.x, th, c.y), |g| g.lathe(&[v2(0.0, 0.0), v2(r * 0.12, 0.0), v2(r * 0.12, th * 0.03), v2(0.0, th * 0.03)], g.segs(12), Role::Seam, 30.0));
    }
}

// ---------- between points: pistons, cables, hoses, pipes, straps ----------

fn points(p: &Part) -> Vec<Vec3> {
    p.points.iter().map(|q| Vec3::from(*q)).collect()
}

fn piston(g: &mut Geo, p: &Part) {
    let pts = points(p);
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let len = a.distance(b);
    let r = p.tube_radius();
    if len < r * 3.0 {
        return;
    }
    // A two-bone piston keeps its rest lengths: the cylinder from `a` and
    // the rod from `b` slide over each other as the joint bends. `half`
    // builds only the cylinder (0) or the rod (1), for export.
    let span = p.span.as_ref();
    let l0 = span.and_then(|s| s.rest_length).filter(|l| *l > r * 3.0).unwrap_or(len);
    let half = span.and_then(|s| s.half);
    let (cyl, rod) = (half != Some(1), half != Some(0));
    let segs = g.segs(18);
    let shock = p.style() == "shock";
    let eye = |g: &mut Geo, y: f32| {
        g.rod(Vec3::new(-r * 0.75, y, 0.0), Vec3::new(r * 0.75, y, 0.0), r * 0.55, r * 0.1, g.segs(14), Role::Metal);
        g.rod(Vec3::new(-r * 0.95, y, 0.0), Vec3::new(r * 0.95, y, 0.0), r * 0.25, r * 0.05, g.segs(10), Role::Chrome);
    };
    // Orientation: local Y runs a -> b; local X is level when it can be.
    let dir = (b - a) / len;
    let side = dir.cross(Vec3::Y).normalize_or(dir.any_orthonormal_vector());
    let up = side.cross(dir);
    let frame = Mat4::from_cols(side.extend(0.0), dir.extend(0.0), up.extend(0.0), a.extend(1.0));
    g.with(frame, |g| {
        let y0 = r * 0.55;
        let y1 = y0 + (l0 - r * 1.2) * 0.52;
        let y2 = len - r * 0.55;
        if cyl {
            eye(g, 0.0);
            g.rod(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, y0 + r * 0.2, 0.0), r * 0.5, r * 0.08, segs, Role::Metal);
            g.lathe(&[v2(0.0, y0), v2(r * 1.08, y0), v2(r * 1.08, y0 + r * 0.3), v2(r, y0 + r * 0.36), v2(r, y1 - r * 0.36), v2(r * 1.1, y1 - r * 0.3), v2(r * 1.1, y1), v2(r * 0.5, y1), v2(r * 0.5, y1 + r * 0.06), v2(0.0, y1 + r * 0.06)], segs, Role::Dark, 30.0);
            // Hydraulic ports near both ends of the body.
            for y in [y0 + r * 0.6, y1 - r * 0.6] {
                g.with(t(0.0, y, 0.0) * Mat4::from_rotation_x(FRAC_PI_2), |g| {
                    g.lathe(&[v2(0.0, 0.0), v2(r * 0.22, 0.0), v2(r * 0.22, r * 1.25), v2(r * 0.3, r * 1.25), v2(r * 0.3, r * 1.45), v2(0.0, r * 1.45)], g.segs(8), Role::Brass, 30.0);
                });
            }
        }
        if rod {
            // At rest the rod starts at the cylinder's top; a two-bone rod
            // reaches on into the cylinder so pulling it out leaves no gap.
            let start = if span.is_some() { (len - (l0 - y1) * 1.6).max(y0 + r * 0.4).min(y1) } else { y1 };
            g.rod(Vec3::new(0.0, start, 0.0), Vec3::new(0.0, y2, 0.0), r * 0.42, r * 0.05, segs, Role::Chrome);
        }
        if shock && cyl {
            let top = if span.is_some() { y1 } else { y2 };
            let coils = 7.0;
            let pitch = (top - y0) / coils;
            let path: Vec<Vec3> = (0..=(coils as usize * 20)).map(|i| { let tt = i as f32 / 20.0; let a = tt * TAU; Vec3::new(a.cos() * r * 1.35, y0 + r * 0.4 + tt * pitch * 0.92, a.sin() * r * 1.35) }).collect();
            g.tube(&path, &|_| r * 0.14, g.segs(8), Role::Paint, true);
        } else if !shock && cyl {
            let boot = (l0 - r * 0.55 - y1) * 0.4;
            let pitch = r * 0.35;
            let path: Vec<Vec3> = (0..=((boot / (pitch / 6.0)).ceil() as usize).max(2)).map(|i| Vec3::new(0.0, y1 + i as f32 * pitch / 6.0, 0.0)).filter(|q| q.y <= y1 + boot).collect();
            g.tube(&path, &|s| r * (0.66 + 0.07 * (s / pitch * TAU).sin()), segs, Role::Rubber, true);
        }
        if rod {
            g.rod(Vec3::new(0.0, y2 - r * 0.25, 0.0), Vec3::new(0.0, len, 0.0), r * 0.48, r * 0.08, segs, Role::Metal);
            eye(g, len);
        }
    });
}

/// A smooth route through `pts` (Catmull-Rom), hanging by `sag` of each
/// span, sampled about every `step`.
pub fn route(pts: &[Vec3], sag: f32, step: f32) -> Vec<Vec3> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let n = pts.len();
    let at = |i: isize| pts[i.clamp(0, n as isize - 1) as usize];
    let mut out = vec![pts[0]];
    for i in 0..n - 1 {
        let (p0, p1, p2, p3) = (at(i as isize - 1), at(i as isize), at(i as isize + 1), at(i as isize + 2));
        let span = p1.distance(p2);
        let k = ((span / step.max(1e-4)).ceil() as usize).clamp(2, 400);
        for j in 1..=k {
            let u = j as f32 / k as f32;
            let (u2, u3) = (u * u, u * u * u);
            let c = 0.5 * ((2.0 * p1) + (-p0 + p2) * u + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u2 + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * u3);
            out.push(c + Vec3::NEG_Y * (sag * span * 4.0 * u * (1.0 - u)));
        }
    }
    out
}

/// Points evenly every `step` along a polyline.
fn resample(path: &[Vec3], step: f32) -> Vec<Vec3> {
    let total: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    let n = ((total / step.max(1e-5)).ceil() as usize).clamp(1, 4000);
    let mut out = Vec::with_capacity(n + 1);
    let (mut seg, mut acc) = (0usize, 0.0f32);
    for i in 0..=n {
        let target = total * i as f32 / n as f32;
        while seg + 1 < path.len() - 1 && acc + path[seg].distance(path[seg + 1]) < target {
            acc += path[seg].distance(path[seg + 1]);
            seg += 1;
        }
        let l = path[seg].distance(path[seg + 1]).max(1e-9);
        out.push(path[seg].lerp(path[seg + 1], ((target - acc) / l).clamp(0.0, 1.0)));
    }
    out
}

/// Rounds each corner of a polyline with a quadratic arc of about `r`.
pub fn fillet(pts: &[Vec3], r: f32, segs: usize) -> Vec<Vec3> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    for i in 1..pts.len() - 1 {
        let (a, b, c) = (pts[i - 1], pts[i], pts[i + 1]);
        let rr = r.min(a.distance(b) * 0.49).min(b.distance(c) * 0.49);
        let p = b + (a - b).normalize_or(Vec3::ZERO) * rr;
        let q = b + (c - b).normalize_or(Vec3::ZERO) * rr;
        for k in 0..=segs {
            let u = k as f32 / segs as f32;
            out.push(p * (1.0 - u) * (1.0 - u) + b * 2.0 * u * (1.0 - u) + q * u * u);
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

/// Arc length along a path to each point.
fn arcs(path: &[Vec3]) -> Vec<f32> {
    let mut out = vec![0.0; path.len()];
    for i in 1..path.len() {
        out[i] = out[i - 1] + path[i].distance(path[i - 1]);
    }
    out
}

/// The point and tangent at arc length `s`.
fn at_arc(path: &[Vec3], arc: &[f32], s: f32) -> (Vec3, Vec3) {
    let i = arc.partition_point(|&a| a < s).clamp(1, path.len() - 1);
    let l = (arc[i] - arc[i - 1]).max(1e-9);
    let u = ((s - arc[i - 1]) / l).clamp(0.0, 1.0);
    (path[i - 1].lerp(path[i], u), (path[i] - path[i - 1]).normalize_or(Vec3::X))
}

/// Cuts `cut` off both ends of a path.
fn trim(path: &[Vec3], cut0: f32, cut1: f32) -> Vec<Vec3> {
    let arc = arcs(path);
    let total = *arc.last().unwrap_or(&0.0);
    if total <= cut0 + cut1 + 1e-6 {
        return path.to_vec();
    }
    let (s0, s1) = (cut0, total - cut1);
    let mut out = vec![at_arc(path, &arc, s0).0];
    out.extend(path.iter().zip(&arc).filter(|(_, a)| **a > s0 && **a < s1).map(|(p, _)| *p));
    out.push(at_arc(path, &arc, s1).0);
    out
}

/// Bands (ties, clips) around a path at evenly spaced arc lengths.
fn ties(g: &mut Geo, path: &[Vec3], count: u32, r: f32, len: f32, role: Role) {
    let arc = arcs(path);
    let total = *arc.last().unwrap_or(&0.0);
    for i in 0..count {
        let s = total * (i as f32 + 1.0) / (count as f32 + 1.0);
        let (p, d) = at_arc(path, &arc, s);
        g.rod(p - d * len * 0.5, p + d * len * 0.5, r, r * 0.15, g.segs(12), role);
    }
}

/// A plug at a cable end: `tip` is the end point, `dir` points out of
/// the cable through it.
fn plug(g: &mut Geo, tip: Vec3, dir: Vec3, r: f32) {
    g.with(along(tip - dir * r * 4.0, tip), |g| {
        let segs = g.segs(14);
        g.lathe(&[v2(0.0, 0.0), v2(r * 1.05, 0.0), v2(r * 1.5, r * 1.2), v2(r * 1.5, r * 2.0), v2(r * 0.0, r * 2.0)], segs, Role::Rubber, 30.0);
        let knurl = (0..=18).map(|i| Vec3::new(0.0, r * 2.0 + i as f32 * r * 0.1, 0.0)).collect::<Vec<_>>();
        g.tube(&knurl, &|s| r * (1.8 + 0.08 * (s / (r * 0.3) * TAU).cos()), segs, Role::Metal, true);
        g.lathe(&[v2(0.0, r * 3.8), v2(r * 1.6, r * 3.8), v2(r * 1.6, r * 4.0), v2(0.0, r * 4.0)], segs, Role::Dark, 30.0);
    });
}

fn cable(g: &mut Geo, p: &Part) {
    let r = p.tube_radius();
    let pts = points(p);
    let path = route(&pts, p.sag.unwrap_or(0.04), r * 2.0);
    let ends = p.ends.unwrap_or(true);
    let body = if ends { trim(&path, r * 3.6, r * 3.6) } else { path.clone() };
    let segs = g.segs(10);
    match p.style() {
        "bundle" => {
            let arc = arcs(&body);
            let pitch = r * 14.0;
            for k in 0..3 {
                let phase = k as f32 / 3.0 * TAU;
                let strand: Vec<Vec3> = body.iter().zip(&arc).enumerate().map(|(i, (q, s))| {
                    let d = (body[(i + 1).min(body.len() - 1)] - body[i.saturating_sub(1)]).normalize_or(Vec3::X);
                    let n0 = d.cross(Vec3::Y).normalize_or(d.any_orthonormal_vector());
                    let b0 = d.cross(n0);
                    let a = phase + s / pitch * TAU;
                    *q + (n0 * a.cos() + b0 * a.sin()) * r * 0.5
                }).collect();
                g.tube(&strand, &|_| r * 0.48, g.segs(7), if k == 0 { Role::Rubber } else if k == 1 { Role::Dark } else { Role::Rubber }, true);
            }
            let total = *arc.last().unwrap_or(&0.0);
            ties(g, &body, ((total / (r * 18.0)) as u32).clamp(1, 24), r * 1.12, r * 0.6, Role::Dark);
        }
        "braided" => g.tube(&body, &|s| r * (1.0 + 0.04 * (s / (r * 0.7) * TAU).sin()), segs, Role::Metal, true),
        _ => g.tube(&body, &|_| r, segs, Role::Rubber, true),
    }
    ties(g, &body, p.clips.unwrap_or(0), r * 1.3, r * 1.4, Role::Dark);
    if ends && path.len() >= 2 {
        let n = path.len();
        plug(g, path[0], (path[0] - path[1]).normalize_or(Vec3::X), r);
        plug(g, path[n - 1], (path[n - 1] - path[n - 2]).normalize_or(Vec3::X), r);
    }
}

/// A crimped hose fitting at an end: hex nut at the tip, sleeve behind.
fn fitting(g: &mut Geo, tip: Vec3, dir: Vec3, r: f32) {
    g.with(along(tip - dir * r * 3.4, tip), |g| {
        let segs = g.segs(14);
        g.lathe(&[v2(0.0, 0.0), v2(r * 1.05, 0.0), v2(r * 1.25, r * 0.2), v2(r * 1.25, r * 1.9), v2(r * 1.1, r * 2.0), v2(0.0, r * 2.0)], segs, Role::Chrome, 30.0);
        g.prism(&hex(r * 1.5), r * 2.0, r * 3.0, r * 0.15, r * 0.15, Role::Metal, false);
        g.lathe(&[v2(0.0, r * 3.0), v2(r * 0.8, r * 3.0), v2(r * 0.8, r * 3.4), v2(0.0, r * 3.4)], segs, Role::Brass, 30.0);
    });
}

fn hose(g: &mut Geo, p: &Part) {
    let r = p.tube_radius();
    let pts = points(p);
    let path = route(&pts, p.sag.unwrap_or(0.06), r * 1.5);
    let ends = p.ends.unwrap_or(true);
    let body = if ends { trim(&path, r * 3.2, r * 3.2) } else { path.clone() };
    let segs = g.segs(12);
    match p.style() {
        "smooth" => g.tube(&body, &|_| r, segs, Role::Rubber, true),
        "braided" => {
            let fine = resample(&body, r * 0.15);
            g.tube(&fine, &|s| r * (1.0 + 0.035 * (s / (r * 0.5) * TAU).sin()), segs, Role::Metal, true)
        }
        _ => {
            let pitch = r * 0.8;
            let fine = resample(&body, pitch / 6.0);
            g.tube(&fine, &|s| r * (1.0 + 0.1 * (s / pitch * TAU).sin()), segs, Role::Rubber, true)
        }
    }
    ties(g, &body, p.clips.unwrap_or(0), r * 1.25, r * 1.2, Role::Metal);
    if ends && path.len() >= 2 {
        let n = path.len();
        fitting(g, path[0], (path[0] - path[1]).normalize_or(Vec3::X), r);
        fitting(g, path[n - 1], (path[n - 1] - path[n - 2]).normalize_or(Vec3::X), r);
    }
}

fn corrugated(g: &mut Geo, p: &Part) {
    let r = p.tube_radius();
    let pts = points(p);
    let path = route(&pts, p.sag.unwrap_or(0.03), r * 0.8);
    let ends = p.ends.unwrap_or(true);
    let body = if ends { trim(&path, r * 0.3, r * 0.3) } else { path.clone() };
    let pitch = r * 0.5;
    let fine = resample(&body, pitch / 6.0);
    // Flat-topped ribs.
    g.tube(&fine, &|s| { let w = (s / pitch * TAU).sin(); r * (1.0 + 0.1 * (w * 2.2).clamp(-1.0, 1.0)) }, g.segs(16), Role::Rubber, true);
    if ends && path.len() >= 2 {
        let n = path.len();
        for (tip, back) in [(path[0], path[1]), (path[n - 1], path[n - 2])] {
            let dir = (tip - back).normalize_or(Vec3::X);
            flange(g, tip, dir, r * 1.45, r * 0.32, r * 1.0, 8);
        }
    }
}

/// A bolted ring flange facing `dir` with its face at `tip`.
fn flange(g: &mut Geo, tip: Vec3, dir: Vec3, ro: f32, th: f32, ri: f32, bolts: usize) {
    g.with(along(tip - dir * th, tip), |g| {
        let segs = g.segs(20);
        g.lathe(&[v2(ri * 0.95, 0.0), v2(ro, 0.0), v2(ro, th * 0.75), v2(ro - th * 0.25, th), v2(ri * 0.95, th), v2(ri * 0.95, 0.0)], segs, Role::Metal, 30.0);
        let br = (ro - ri) * 0.32;
        for k in 0..bolts {
            let a = k as f32 / bolts as f32 * TAU;
            let c = Vec3::new(a.cos(), 0.0, a.sin()) * (ri + ro) * 0.5;
            g.with(t(c.x, th, c.z), |g| g.prism(&hex(br), 0.0, br * 0.9, br * 0.18, 0.0, Role::Dark, false));
        }
    });
}

fn pipe(g: &mut Geo, p: &Part) {
    let r = p.tube_radius();
    let pts = points(p);
    let path = fillet(&pts, r * 2.6, g.segs(8));
    g.tube(&path, &|_| r, g.segs(16), Role::Metal, true);
    if p.style() == "flanged" && path.len() >= 2 {
        let n = path.len();
        for (tip, back) in [(path[0], path[1]), (path[n - 1], path[n - 2])] {
            let dir = (tip - back).normalize_or(Vec3::X);
            flange(g, tip, dir, r * 1.8, r * 0.4, r * 0.98, 6);
        }
    }
    ties(g, &path, p.clips.unwrap_or(0), r * 1.2, r * 0.8, Role::Dark);
}

fn strap(g: &mut Geo, p: &Part) {
    let w = p.tube_radius() * 2.0;
    let th = (w * 0.1).max(1e-4);
    let pts = points(p);
    let path = resample(&fillet(&pts, w, 4), w * 0.5);
    let ups: Vec<Vec3> = path.iter().map(|_| Vec3::Y).collect();
    g.strip(&path, &ups, w, th, Role::Strap, true);
    // Buckle in the middle.
    let arc = arcs(&path);
    let (c, d) = at_arc(&path, &arc, arc.last().copied().unwrap_or(0.0) * 0.5);
    let side = d.cross(Vec3::Y).normalize_or(Vec3::Z);
    let up = side.cross(d).normalize_or(Vec3::Y);
    let frame = Mat4::from_cols(d.extend(0.0), up.extend(0.0), side.extend(0.0), (c + up * th).extend(1.0));
    g.with(frame, |g| {
        if p.style() == "ratchet" {
            g.boxy(Vec3::new(0.0, w * 0.2, 0.0), Vec3::new(w * 1.6, w * 0.4, w * 1.15), w * 0.06, Role::Metal);
            g.boxy(Vec3::new(w * 0.2, w * 0.48, 0.0), Vec3::new(w * 1.4, w * 0.12, w * 0.5), w * 0.03, Role::Paint);
        } else {
            let (bw, bd) = (w * 0.9, w * 1.3);
            g.frame(&rect(bw, bd, w * 0.1), &rect(bw * 0.6, bd * 0.75, w * 0.05), w * 0.18, w * 0.04, 0.0, Role::Metal, Role::Strap);
            g.rod(Vec3::new(0.0, w * 0.2, -bd * 0.35), Vec3::new(0.0, w * 0.2, bd * 0.35), w * 0.05, 0.0, g.segs(8), Role::Metal);
        }
    });
}

fn spring(g: &mut Geo, p: &Part) {
    let s = p.dims();
    let (a, b) = if p.points.len() >= 2 { (Vec3::from(p.points[0]), Vec3::from(p.points[p.points.len() - 1])) } else { (Vec3::ZERO, Vec3::Y * s.y) };
    let len = a.distance(b).max(1e-4);
    let rc = s.x * 0.5;
    let wire = (s.x * 0.08).max(1e-4);
    let coils = p.count.unwrap_or(6).max(2) as f32;
    let per = 24;
    let total = coils * per as f32;
    g.with(along(a, b), |g| {
        let path: Vec<Vec3> = (0..=(total as usize)).map(|i| {
            let tt = i as f32 / per as f32;
            // Closed, flat end coils; open pitch in between.
            let e = 0.75;
            let open = (coils - 2.0 * e).max(0.1);
            let y = if tt < e { wire * tt / e } else if tt > coils - e { len - wire * (coils - tt) / e } else { wire + (len - 2.0 * wire) * (tt - e) / open };
            let ang = tt * TAU;
            Vec3::new(ang.cos() * (rc - wire), y.clamp(wire, len - wire), ang.sin() * (rc - wire))
        }).collect();
        g.tube(&path, &|_| wire, g.segs(8), if p.style() == "die" { Role::Paint } else { Role::Metal }, true);
    });
}

// ---------- lights, sensors, antennas, exhausts, connectors ----------

fn light(g: &mut Geo, s: Vec3, style: &str) {
    let (d, hh) = (s.x, s.y);
    let r = d * 0.5;
    let segs = g.segs(20);
    match style {
        "rect" | "strip" => {
            let (w, dd) = if style == "strip" { (s.x, s.z * 0.4) } else { (s.x, s.z) };
            let f = w.min(dd) * 0.12;
            g.frame(&rect(w, dd, f), &rect(w - 2.0 * f, dd - 2.0 * f, f * 0.5), hh, hh * 0.2, hh * 0.45, Role::Dark, Role::Dark);
            g.prism(&rect(w - 2.0 * f, dd - 2.0 * f, f * 0.5), hh * 0.45, hh * 0.82, hh * 0.1, 0.0, Role::Glow, false);
            if style == "rect" {
                for k in 1..4 {
                    let x = -w * 0.5 + f + (w - 2.0 * f) * k as f32 / 4.0;
                    g.rod(Vec3::new(x, hh * 0.92, -dd * 0.5 + f), Vec3::new(x, hh * 0.92, dd * 0.5 - f), f * 0.18, 0.0, g.segs(6), Role::Dark);
                }
            }
        }
        _ => {
            g.lathe(&[v2(0.0, 0.0), v2(r, 0.0), v2(r, hh * 0.14), v2(r * 0.86, hh * 0.24), v2(r * 0.86, hh * 0.7), v2(r * 0.98, hh * 0.76), v2(r * 0.98, hh * 0.86), v2(r * 0.8, hh * 0.86), v2(r * 0.8, hh * 0.78), v2(0.0, hh * 0.78)], segs, Role::Dark, 30.0);
            g.lathe(&[v2(0.0, hh * 0.78), v2(r * 0.8, hh * 0.78), v2(r * 0.78, hh * 0.84), v2(r * 0.6, hh * 0.92), v2(r * 0.32, hh * 0.98), v2(0.0, hh)], segs, Role::Glow, 60.0);
            g.lathe(&[v2(r * 0.8, hh * 0.86), v2(r * 0.98, hh * 0.86), v2(r * 0.94, hh * 0.9), v2(r * 0.8, hh * 0.9), v2(r * 0.8, hh * 0.86)], segs, Role::Chrome, 30.0);
            if style == "cage" {
                for k in 0..3 {
                    let a = k as f32 / 3.0 * PI;
                    let dir = Vec3::new(a.cos(), 0.0, a.sin());
                    let arc: Vec<Vec3> = (0..=12).map(|i| { let u = i as f32 / 12.0 * PI; dir * (u.cos() * r * 0.95) + Vec3::Y * (hh * 0.86 + u.sin() * hh * 0.45) }).collect();
                    g.tube(&arc, &|_| d * 0.025, g.segs(6), Role::Metal, true);
                }
            }
        }
    }
}

fn antenna(g: &mut Geo, s: Vec3, style: &str) {
    let (d, l) = (s.x, s.y);
    let r = d * 0.5;
    let segs = g.segs(14);
    g.lathe(&[v2(0.0, 0.0), v2(r * 1.6, 0.0), v2(r * 1.6, r * 0.3), v2(r * 1.2, r * 0.6), v2(r * 0.9, r * 1.8), v2(0.0, r * 1.8)], segs, Role::Dark, 35.0);
    match style {
        "blade" => {
            let bh = l.min(d * 8.0);
            let prof = [Vec2::new(-d * 1.6, 0.0), Vec2::new(d * 1.6, 0.0), Vec2::new(d * 0.2, bh), Vec2::new(-d * 0.5, bh)];
            g.with(t(0.0, r * 1.6, 0.0) * Mat4::from_rotation_x(-FRAC_PI_2), |g| g.prism(&prof, -d * 0.12, d * 0.12, d * 0.05, d * 0.05, Role::Paint, false));
        }
        "stub" => {
            g.lathe(&[v2(0.0, r * 1.8), v2(r * 0.7, r * 1.8), v2(r * 0.7, l * 0.25), v2(r * 0.55, l * 0.27), v2(r * 0.55, l * 0.3), v2(r * 0.3, l * 0.32), v2(0.0, l * 0.32)], segs, Role::Rubber, 30.0);
        }
        _ => {
            let coil: Vec<Vec3> = (0..=6 * 16).map(|i| { let u = i as f32 / 16.0; let a = u * TAU; Vec3::new(a.cos() * r * 0.45, r * 1.8 + u * r * 0.5, a.sin() * r * 0.45) }).collect();
            g.tube(&coil, &|_| r * 0.14, g.segs(6), Role::Metal, true);
            let y0 = r * 1.8 + 6.0 * r * 0.5;
            g.rod(Vec3::new(0.0, r * 1.7, 0.0), Vec3::new(0.0, y0 + r * 0.2, 0.0), r * 0.3, r * 0.05, g.segs(8), Role::Metal);
            let whip: Vec<Vec3> = (0..=12).map(|i| Vec3::new(0.0, y0 + (l - y0) * i as f32 / 12.0, 0.0)).collect();
            g.tube(&whip, &|s| r * (0.24 - 0.16 * s / (l - y0).max(1e-4)), g.segs(8), Role::Dark, true);
            g.lathe(&circle_profile(r * 0.16, l), g.segs(8), Role::Dark, 60.0);
        }
    }
}

/// A sphere's lathe profile of radius `r` centered at height `y`.
fn circle_profile(r: f32, y: f32) -> Vec<Vec2> {
    (0..=8).map(|i| { let a = -FRAC_PI_2 + i as f32 / 8.0 * PI; v2(r * a.cos(), y + r * a.sin()) }).collect()
}

fn sensor(g: &mut Geo, s: Vec3, style: &str) {
    let (w, hh, d) = (s.x, s.y, s.z);
    let r = w * 0.5;
    let segs = g.segs(20);
    match style {
        "camera" => {
            g.lathe(&[v2(0.0, 0.0), v2(r * 0.6, 0.0), v2(r * 0.6, hh * 0.1), v2(r * 0.3, hh * 0.25), v2(0.0, hh * 0.25)], segs, Role::Dark, 30.0);
            g.boxy(Vec3::new(0.0, hh * 0.6, -d * 0.05), Vec3::new(w * 0.7, hh * 0.7, d * 0.8), hh * 0.08, Role::Paint);
            g.with(t(0.0, hh * 0.6, d * 0.32) * Mat4::from_rotation_x(FRAC_PI_2), |g| {
                let lr = hh * 0.28;
                g.lathe(&[v2(0.0, 0.0), v2(lr, 0.0), v2(lr, d * 0.16), v2(lr * 1.08, d * 0.18), v2(lr * 1.08, d * 0.2), v2(lr * 0.75, d * 0.2), v2(lr * 0.75, d * 0.17), v2(0.0, d * 0.17)], segs, Role::Dark, 30.0);
                g.lathe(&[v2(0.0, d * 0.17), v2(lr * 0.75, d * 0.17), v2(lr * 0.5, d * 0.185), v2(0.0, d * 0.19)], segs, Role::Glass, 60.0);
                g.lathe(&[v2(lr * 0.75, d * 0.172), v2(lr * 0.85, d * 0.172), v2(lr * 0.85, d * 0.176), v2(lr * 0.75, d * 0.176), v2(lr * 0.75, d * 0.172)], segs, Role::Glow, 30.0);
            });
            g.boxy(Vec3::new(0.0, hh * 0.97, d * 0.25), Vec3::new(w * 0.72, hh * 0.05, d * 0.45), 0.0, Role::Paint);
        }
        "lidar" => {
            g.lathe(&[v2(0.0, 0.0), v2(r, 0.0), v2(r, hh * 0.2), v2(r * 0.85, hh * 0.28), v2(0.0, hh * 0.28)], segs, Role::Dark, 30.0);
            g.lathe(&[v2(0.0, hh * 0.28), v2(r * 0.7, hh * 0.28), v2(r * 0.7, hh * 0.5), v2(0.0, hh * 0.5)], segs, Role::Metal, 30.0);
            g.lathe(&[v2(0.0, hh * 0.5), v2(r * 0.7, hh * 0.5), v2(r * 0.7, hh * 0.78), v2(0.0, hh * 0.78)], segs, Role::Glass, 30.0);
            g.lathe(&[v2(0.0, hh * 0.78), v2(r * 0.72, hh * 0.78), v2(r * 0.72, hh * 0.9), v2(r * 0.6, hh), v2(0.0, hh)], segs, Role::Paint, 30.0);
        }
        _ => {
            let base = hh * 0.4;
            g.lathe(&[v2(0.0, 0.0), v2(r, 0.0), v2(r, base * 0.8), v2(r * 0.85, base), v2(0.0, base)], segs, Role::Dark, 30.0);
            let dr = r * 0.8;
            let dh = (hh - base).max(1e-4);
            let mut prof = vec![v2(0.0, base), v2(dr, base)];
            for i in 1..=10 {
                let a = i as f32 / 10.0 * FRAC_PI_2;
                prof.push(v2(dr * a.cos(), base + dh * a.sin()));
            }
            g.lathe(&prof, segs, Role::Glass, 50.0);
            g.lathe(&[v2(dr, base), v2(dr * 1.05, base), v2(dr * 1.05, base + dh * 0.06), v2(dr * 0.98, base + dh * 0.06), v2(dr, base)], segs, Role::Glow, 30.0);
        }
    }
}

fn exhaust(g: &mut Geo, s: Vec3, style: &str) {
    let (d, l) = (s.x, s.y);
    let r = d * 0.5;
    let segs = g.segs(20);
    if style == "port" {
        let (w, dd) = (s.x * 2.0, s.z * 1.2);
        let f = w.min(dd) * 0.12;
        g.frame(&rect(w, dd, f), &rect(w - 2.0 * f, dd - 2.0 * f, f * 0.5), l * 0.15, l * 0.03, 0.0, Role::Dark, Role::Seam);
        let n = 5;
        for i in 0..n {
            let z = -dd * 0.5 + f + (dd - 2.0 * f) * (i as f32 + 0.5) / n as f32;
            g.with(t(0.0, l * 0.07, z) * Mat4::from_rotation_x(-0.7), |g| g.boxy(Vec3::ZERO, Vec3::new(w - 2.0 * f, l * 0.012, (dd - 2.0 * f) / n as f32 * 1.2), 0.0, Role::Dark));
        }
        return;
    }
    flange(g, Vec3::new(0.0, r * 0.18, 0.0), Vec3::Y, r * 1.5, r * 0.18, r * 0.9, 6);
    g.lathe(&[v2(0.0, r * 0.18), v2(r, r * 0.18), v2(r, l * 0.94), v2(r * 1.12, l), v2(r * 0.86, l), v2(r * 0.86, l * 0.8), v2(0.0, l * 0.8)], segs, Role::Dark, 30.0);
    // Perforated heat shield in front (+Z): slats around half the pipe.
    let slats = 9;
    for k in 0..slats {
        let a = -1.2 + 2.4 * (k as f32 + 0.5) / slats as f32;
        let dir = Vec3::new(a.sin(), 0.0, a.cos());
        g.with(Mat4::from_rotation_translation(Quat::from_rotation_y(a), dir * r * 1.25), |g| {
            g.boxy(Vec3::new(0.0, l * 0.5, 0.0), Vec3::new(r * 0.22, l * 0.62, r * 0.05), r * 0.01, Role::Metal);
        });
    }
    for y in [l * 0.22, l * 0.78] {
        let ring: Vec<Vec3> = (0..=24).map(|i| { let a = i as f32 / 24.0 * TAU; Vec3::new(a.cos() * r * 1.12, y, a.sin() * r * 1.12) }).collect();
        g.tube(&ring, &|_| r * 0.07, g.segs(6), Role::Metal, false);
    }
}

fn connector(g: &mut Geo, s: Vec3, style: &str) {
    let (d, l) = (s.x, s.y);
    let r = d * 0.5;
    let segs = g.segs(16);
    g.boxy(Vec3::new(0.0, l * 0.06, 0.0), Vec3::new(d * 1.35, l * 0.12, d * 1.35), l * 0.02, Role::Dark);
    for (x, z) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        bolt_at(g, Vec3::new(x * d * 0.52, l * 0.12, z * d * 0.52), d * 0.16, "socket");
    }
    if style == "rect" {
        g.boxy(Vec3::new(0.0, l * 0.45, 0.0), Vec3::new(d * 1.1, l * 0.66, d * 0.7), d * 0.06, Role::Dark);
        g.rod(Vec3::new(0.0, l * 0.78, 0.0), Vec3::new(0.0, l, 0.0), r * 0.4, 0.0, segs, Role::Rubber);
        return;
    }
    g.lathe(&[v2(0.0, l * 0.12), v2(r * 0.9, l * 0.12), v2(r * 0.9, l * 0.35), v2(0.0, l * 0.35)], segs, Role::Dark, 30.0);
    let knurl: Vec<Vec3> = (0..=24).map(|i| Vec3::new(0.0, l * 0.35 + i as f32 * l * 0.25 / 24.0, 0.0)).collect();
    g.tube(&knurl, &|y| r * (1.0 + 0.05 * (y / (l * 0.03) * TAU).cos()), segs, Role::Metal, true);
    g.lathe(&[v2(0.0, l * 0.6), v2(r * 0.85, l * 0.6), v2(r * 0.6, l * 0.9), v2(r * 0.45, l * 0.9), v2(r * 0.45, l), v2(0.0, l)], segs, Role::Dark, 30.0);
}

// ---------- props ----------

fn ammo_box(g: &mut Geo, s: Vec3, style: &str) {
    let (w, h, d) = (s.x, s.y, s.z);
    if style == "crate" {
        g.boxy(Vec3::new(0.0, h * 0.5, 0.0), Vec3::new(w, h, d), h * 0.04, Role::Paint);
        for k in [-1.0f32, 1.0] {
            g.boxy(Vec3::new(k * w * 0.42, h * 0.5, 0.0), Vec3::new(w * 0.06, h * 1.02, d * 1.03), h * 0.01, Role::Paint);
        }
        g.boxy(Vec3::new(0.0, h * 0.6, d * 0.51), Vec3::new(w * 0.5, h * 0.18, d * 0.02), 0.0, Role::Marking);
        return;
    }
    g.boxy(Vec3::new(0.0, h * 0.4, 0.0), Vec3::new(w, h * 0.8, d), h * 0.03, Role::Paint);
    g.boxy(Vec3::new(0.0, h * 0.89, 0.0), Vec3::new(w * 1.02, h * 0.18, d * 1.04), h * 0.025, Role::Paint);
    for side in [-1.0f32, 1.0] {
        for k in 0..2 {
            let y = h * (0.25 + 0.3 * k as f32);
            g.boxy(Vec3::new(0.0, y, side * d * 0.5), Vec3::new(w * 0.86, h * 0.04, d * 0.03), h * 0.008, Role::Paint);
        }
    }
    let hp = [Vec3::new(-w * 0.25, h * 0.98, 0.0), Vec3::new(-w * 0.25, h * 1.06, 0.0), Vec3::new(w * 0.25, h * 1.06, 0.0), Vec3::new(w * 0.25, h * 0.98, 0.0)];
    g.tube(&fillet(&hp, h * 0.04, 3), &|_| h * 0.018, g.segs(8), Role::Dark, true);
    g.boxy(Vec3::new(w * 0.52, h * 0.8, 0.0), Vec3::new(w * 0.04, h * 0.3, d * 0.4), h * 0.01, Role::Dark);
    g.boxy(Vec3::new(0.0, h * 0.42, d * 0.515), Vec3::new(w * 0.6, h * 0.14, d * 0.01), 0.0, Role::Marking);
}

fn jerry_can(g: &mut Geo, s: Vec3) {
    let (w, h, d) = (s.x, s.y, s.z);
    g.boxy(Vec3::new(0.0, h * 0.45, 0.0), Vec3::new(w, h * 0.9, d), d * 0.12, Role::Paint);
    g.boxy(Vec3::new(0.0, h * 0.45, 0.0), Vec3::new(w * 1.01, h * 0.02, d * 1.04), 0.0, Role::Paint);
    let ang = (h * 0.8).atan2(w * 0.85);
    for side in [-1.0f32, 1.0] {
        for a in [ang, -ang] {
            g.with(t(0.0, h * 0.45, side * d * 0.5) * Mat4::from_rotation_z(a), |g| {
                g.boxy(Vec3::ZERO, Vec3::new((w * 0.85).hypot(h * 0.8) * 0.95, h * 0.05, d * 0.05), d * 0.01, Role::Paint);
            });
        }
    }
    for k in [-1.0f32, 0.0, 1.0] {
        let x = k * w * 0.2;
        let hp = [Vec3::new(x, h * 0.88, -d * 0.3), Vec3::new(x, h * 0.98, -d * 0.3), Vec3::new(x, h * 0.98, d * 0.3), Vec3::new(x, h * 0.88, d * 0.3)];
        g.tube(&fillet(&hp, h * 0.04, 3), &|_| w * 0.022, g.segs(8), Role::Paint, true);
    }
    g.with(t(w * 0.36, h * 0.88, 0.0), |g| {
        g.lathe(&[v2(0.0, 0.0), v2(w * 0.07, 0.0), v2(w * 0.07, h * 0.06), v2(w * 0.06, h * 0.075), v2(0.0, h * 0.075)], g.segs(14), Role::Metal, 30.0);
        g.boxy(Vec3::new(-w * 0.06, h * 0.08, 0.0), Vec3::new(w * 0.12, h * 0.015, w * 0.05), 0.0, Role::Metal);
    });
}

fn net(g: &mut Geo, s: Vec3, count: Option<u32>) {
    let (w, hh, d) = (s.x, s.y, s.z);
    let n = count.unwrap_or(6).clamp(2, 16) as usize;
    let m = ((n as f32 * d / w).round() as usize).clamp(2, 16);
    let r = (w / n as f32).min(d / m as f32) * 0.05;
    let height = |x: f32, z: f32| {
        let (u, v) = ((2.0 * x / w).clamp(-1.0, 1.0), (2.0 * z / d).clamp(-1.0, 1.0));
        r + hh * (1.0 - u * u) * (1.0 - v * v)
    };
    let steps = 24;
    for j in 0..=m {
        let z = -d * 0.5 + d * j as f32 / m as f32;
        let path: Vec<Vec3> = (0..=steps).map(|i| { let x = -w * 0.5 + w * i as f32 / steps as f32; Vec3::new(x, height(x, z), z) }).collect();
        g.tube(&path, &|_| r, g.segs(6), Role::Strap, true);
    }
    for i in 0..=n {
        let x = -w * 0.5 + w * i as f32 / n as f32;
        let path: Vec<Vec3> = (0..=steps).map(|k| { let z = -d * 0.5 + d * k as f32 / steps as f32; Vec3::new(x, height(x, z) + r * 0.9, z) }).collect();
        g.tube(&path, &|_| r, g.segs(6), Role::Strap, true);
    }
    for j in 0..=m {
        for i in 0..=n {
            let (x, z) = (-w * 0.5 + w * i as f32 / n as f32, -d * 0.5 + d * j as f32 / m as f32);
            let y = height(x, z) + r * 0.45;
            g.with(t(x, y, z), |g| g.lathe(&circle_profile(r * 1.7, 0.0), 6, Role::Strap, 70.0));
        }
    }
}

// ---------- markings and seams ----------

/// Strokes of a 3 x 5 stroke font (x 0..2, y 0..4), polylines split by '|'.
fn glyph(c: char) -> &'static str {
    match c {
        '0' => "00 20 24 04 00|00 24",
        '1' => "03 14 10|00 20",
        '2' => "04 24 22 02 00 20",
        '3' => "04 24 20 00|02 22",
        '4' => "04 02 22|24 20",
        '5' => "24 04 02 22 20 00",
        '6' => "24 04 00 20 22 02",
        '7' => "04 24 10",
        '8' => "00 20 24 04 00|02 22",
        '9' => "20 24 04 02 22",
        'A' => "00 03 14 23 20|02 22",
        'B' => "00 04 14 23 12 02|12 21 10 00",
        'C' => "24 04 00 20",
        'D' => "00 04 14 23 21 10 00",
        'E' => "24 04 00 20|02 12",
        'F' => "24 04 00|02 12",
        'G' => "24 04 00 20 22 12",
        'H' => "04 00|24 20|02 22",
        'I' => "04 24|14 10|00 20",
        'J' => "24 21 10 00 01",
        'K' => "04 00|24 02 20",
        'L' => "04 00 20",
        'M' => "00 04 12 24 20",
        'N' => "00 04 20 24",
        'O' => "00 20 24 04 00",
        'P' => "00 04 24 22 02",
        'Q' => "00 20 24 04 00|11 20",
        'R' => "00 04 24 22 02 20",
        'S' => "24 04 02 22 20 00",
        'T' => "04 24|14 10",
        'U' => "04 00 20 24",
        'V' => "04 10 24",
        'W' => "04 00 12 20 24",
        'X' => "04 20|00 24",
        'Y' => "04 12 24|12 10",
        'Z' => "04 24 00 20",
        '-' => "02 22",
        '/' => "00 24",
        '.' => "10",
        ':' => "11|13",
        _ => "",
    }
}

fn stencil(g: &mut Geo, s: Vec3, text: &str, style: &str) {
    let (w, th, d) = (s.x, s.y, s.z);
    let raised = style == "raised";
    let base = if raised { 0.0 } else {
        g.boxy(Vec3::new(0.0, th * 0.5, 0.0), Vec3::new(w, th, d), th * 0.3, Role::Dark);
        let rr = d * 0.06;
        for x in [-w * 0.5 + d * 0.15, w * 0.5 - d * 0.15] {
            g.with(t(x, th, 0.0), |g| g.lathe(&[v2(0.0, 0.0), v2(rr, 0.0), v2(rr * 0.7, rr * 0.6), v2(0.0, rr * 0.7)], 8, Role::Metal, 40.0));
        }
        th
    };
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    let usable = if raised { w } else { w - d * 0.5 };
    let mut ch = d * if raised { 0.9 } else { 0.62 };
    let mut cw = ch * 0.62;
    let gap = ch * 0.2;
    let total = chars.len() as f32 * (cw + gap) - gap;
    if total > usable {
        let k = usable / total;
        ch *= k;
        cw *= k;
    }
    let gap = ch * 0.2;
    let total = chars.len() as f32 * (cw + gap) - gap;
    let sw = ch * 0.14;
    let lift = (th * 0.5).max(ch * 0.03);
    for (i, c) in chars.iter().enumerate() {
        let x0 = -total * 0.5 + i as f32 * (cw + gap);
        let pt = |code: &[u8]| -> Vec2 { Vec2::new(x0 + (code[0] - b'0') as f32 * 0.5 * cw, ch * 0.5 - (code[1] - b'0') as f32 * 0.25 * ch) };
        for stroke in glyph(*c).split('|').filter(|s| !s.is_empty()) {
            let pts: Vec<Vec2> = stroke.split(' ').filter(|p| p.len() == 2).map(|p| pt(p.as_bytes())).collect();
            if pts.len() == 1 {
                g.boxy(Vec3::new(pts[0].x, base + lift * 0.5, pts[0].y), Vec3::new(sw, lift, sw), 0.0, Role::Marking);
            }
            for pair in pts.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let mid = (a + b) * 0.5;
                let len = a.distance(b) + sw;
                let ang = (b - a).y.atan2((b - a).x);
                g.with(t(mid.x, base + lift * 0.5, mid.y) * Mat4::from_rotation_y(-ang), |g| g.boxy(Vec3::ZERO, Vec3::new(len, lift, sw), 0.0, Role::Marking));
            }
        }
    }
}

fn seam(g: &mut Geo, s: Vec3, width: f32, style: &str) {
    let (w, th, d) = (s.x, s.y.max(1e-4), s.z);
    if style == "line" {
        g.boxy(Vec3::new(0.0, th * 0.5, 0.0), Vec3::new(w, th, width), 0.0, Role::Seam);
        return;
    }
    let (x, z) = (w * 0.5, d * 0.5);
    for (c, sz) in [(Vec3::new(0.0, th * 0.5, -z), Vec3::new(w + width, th, width)), (Vec3::new(0.0, th * 0.5, z), Vec3::new(w + width, th, width)), (Vec3::new(-x, th * 0.5, 0.0), Vec3::new(width, th, d - width)), (Vec3::new(x, th * 0.5, 0.0), Vec3::new(width, th, d - width))] {
        g.boxy(c, sz, 0.0, Role::Seam);
    }
}

// ---------- small machinery ----------

fn greeble(g: &mut Geo, s: Vec3, style: &str, seed: u64) {
    let (w, h, d) = (s.x, s.y, s.z);
    let mut rng = Rand::new(seed ^ 0x6EEB);
    let segs = g.segs(14);
    match style {
        "canister" | "tank" => {
            let r = (h * 0.5).min(d * 0.5);
            let l = w;
            let tank = style == "tank";
            g.with(t(0.0, r * 1.05, 0.0) * Mat4::from_rotation_z(-FRAC_PI_2) * t(0.0, -l * 0.5, 0.0), |g| {
                let mut prof = vec![v2(0.0, 0.0)];
                if tank {
                    for i in 0..=6 { let a = -FRAC_PI_2 + i as f32 / 6.0 * FRAC_PI_2; prof.push(v2(r * a.cos(), r + r * a.sin())); }
                    for i in 0..=6 { let a = i as f32 / 6.0 * FRAC_PI_2; prof.push(v2(r * a.cos(), l - r + r * a.sin())); }
                } else {
                    prof.extend([v2(r * 0.9, 0.0), v2(r, r * 0.1), v2(r, l - r * 0.1), v2(r * 0.9, l), v2(r * 0.3, l), v2(r * 0.3, l + r * 0.2), v2(0.0, l + r * 0.2)]);
                }
                if tank { prof.push(v2(0.0, l)); }
                g.lathe(&prof, segs, Role::Dark, 40.0);
                for y in [l * 0.22, l * 0.78] {
                    g.lathe(&[v2(r * 0.98, y - r * 0.08), v2(r * 1.08, y - r * 0.06), v2(r * 1.08, y + r * 0.06), v2(r * 0.98, y + r * 0.08)], segs, Role::Metal, 30.0);
                }
            });
            for x in [-l * 0.3, l * 0.3] {
                g.boxy(Vec3::new(x, r * 0.3, 0.0), Vec3::new(r * 0.4, r * 0.6, r * 1.6), r * 0.05, Role::Metal);
            }
        }
        "pipes" => {
            let r = (h * 0.18).min(d / 8.0);
            for k in 0..3 {
                let z = -d * 0.3 + d * 0.3 * k as f32;
                let y = h * (0.45 + 0.12 * k as f32);
                let pts = [Vec3::new(-w * 0.5, -r, z), Vec3::new(-w * 0.5, y, z), Vec3::new(w * 0.5, y, z), Vec3::new(w * 0.5, -r, z)];
                g.tube(&fillet(&pts, r * 2.0, 5), &|_| r, g.segs(10), if k == 1 { Role::Brass } else { Role::Metal }, true);
            }
            for x in [-w * 0.15, w * 0.2] {
                g.boxy(Vec3::new(x, h * 0.35, 0.0), Vec3::new(r * 1.6, h * 0.7, d * 0.8), r * 0.2, Role::Dark);
            }
        }
        "junction" => {
            g.boxy(Vec3::new(0.0, h * 0.42, 0.0), Vec3::new(w, h * 0.84, d), h * 0.05, Role::Paint);
            g.boxy(Vec3::new(0.0, h * 0.92, 0.0), Vec3::new(w * 0.96, h * 0.16, d * 0.96), h * 0.04, Role::Paint);
            for (x, z) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                bolt_at(g, Vec3::new(x * w * 0.4, h, z * d * 0.4), w.min(d) * 0.08, "socket");
            }
            let n = 2 + (rng.f() * 2.0) as usize;
            for k in 0..n {
                let z = -d * 0.3 + d * 0.6 * (k as f32 + 0.5) / n as f32;
                g.with(t(w * 0.5, h * 0.45, z) * Mat4::from_rotation_z(-FRAC_PI_2), |g| {
                    let r = d * 0.08;
                    g.prism(&hex(r * 1.3), 0.0, r * 0.8, r * 0.15, 0.0, Role::Brass, false);
                    g.lathe(&[v2(0.0, r * 0.8), v2(r, r * 0.8), v2(r * 0.8, r * 2.0), v2(0.0, r * 2.0)], g.segs(10), Role::Rubber, 30.0);
                });
            }
        }
        _ => {
            g.boxy(Vec3::new(0.0, h * 0.38, 0.0), Vec3::new(w, h * 0.76, d), h * 0.05, Role::Paint);
            g.boxy(Vec3::new(0.0, h * 0.84, 0.0), Vec3::new(w * 1.04, h * 0.16, d * 1.04), h * 0.04, Role::Paint);
            let k = 1 + (rng.f() * 2.5) as usize;
            for i in 0..k {
                let x = -w * 0.3 + w * 0.6 * (i as f32 + 0.5) / k as f32;
                g.rod(Vec3::new(x, h * 0.92, d * 0.15), Vec3::new(x, h, d * 0.15), d * 0.09, d * 0.02, g.segs(10), Role::Dark);
            }
            g.boxy(Vec3::new(-w * 0.2, h * 0.94, -d * 0.2), Vec3::new(w * 0.3, h * 0.06, d * 0.3), 0.0, Role::Seam);
        }
    }
}

fn clip(g: &mut Geo, s: Vec3, style: &str) {
    let (w, hh, d) = (s.x, s.y, s.z);
    let r = hh * 0.5;
    let th = (hh * 0.08).max(1e-4);
    if style == "saddle" {
        clamp(g, Vec3::new(w, hh, d), "band");
        return;
    }
    // A loop around a cable lying along X, its tab running out to +Z.
    let mut path = vec![Vec3::new(0.0, 0.0, d * 0.75), Vec3::new(0.0, 0.0, 0.0)];
    let mut ups = vec![Vec3::Y, Vec3::Y];
    let n = 16;
    for i in 1..=n {
        let a = -FRAC_PI_2 - i as f32 / n as f32 * TAU * 0.92;
        let dir = Vec3::new(0.0, a.sin(), a.cos());
        path.push(Vec3::new(0.0, r + th, 0.0) + dir * (r + th));
        ups.push(dir);
    }
    g.strip(&path, &ups, w, th, Role::Metal, true);
    bolt_at(g, Vec3::new(0.0, th, d * 0.5), (w * 0.8).min(d * 0.4), "hex");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fillet_and_route_keep_their_ends() {
        let pts = [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0)];
        let f = fillet(&pts, 0.2, 4);
        assert_eq!(f[0], pts[0]);
        assert_eq!(*f.last().unwrap(), pts[2]);
        let r = route(&pts, 0.1, 0.05);
        assert!(r[0].distance(pts[0]) < 1e-6 && r.last().unwrap().distance(pts[2]) < 1e-6);
        // Sag pulls the middle of a span down.
        let flat = route(&[Vec3::ZERO, Vec3::X * 2.0], 0.1, 0.05);
        let low = flat.iter().map(|p| p.y).fold(0.0, f32::min);
        assert!((low + 0.2).abs() < 0.01, "{low}");
    }

    #[test]
    fn every_glyph_parses() {
        for c in "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-/.:".chars() {
            for stroke in glyph(c).split('|') {
                for p in stroke.split(' ') {
                    let b = p.as_bytes();
                    assert!(b.len() == 2 && b[0] <= b'2' && b[1] <= b'4', "{c}: {p}");
                }
            }
        }
    }
}
