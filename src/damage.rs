//! Saved procedural damage: one profile drives displacement, color and PBR.
//! Stamps use local surface frames; leak paths are integrated in world gravity.
use crate::{
    material::Material,
    mesh::Mesh,
    smart::{Decal, Layer, Op, Shape, Source, Term},
};
use glam::{Mat4, Vec2, Vec3};

#[derive(Clone, Copy)]
pub struct Hit {
    pub coverage: f32,
    pub height: f32,
    pub color: Vec3,
    pub rough: f32,
    pub metal: f32,
}
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Dimensionless profile. Keep `damage_hit` in trace.wgsl identical.
pub fn sample(code: u32, seed: u32, uv: Vec2) -> Hit {
    let r = uv.length();
    let theta = uv.y.atan2(uv.x);
    let phase = crate::smart::rand(seed, 0) * std::f32::consts::TAU;
    let rag = (crate::material::noise(Vec3::new(theta.cos() * 5.0, theta.sin() * 5.0, phase)) - 0.5) * 0.065
        + (theta * 19.0 + phase).sin() * 0.004;
    let mut h = Hit {
        coverage: 1.0 - smooth(0.38, 0.5, r),
        height: 0.0,
        color: Vec3::new(0.52, 0.54, 0.53),
        rough: 0.38,
        metal: 1.0,
    };
    match code {
        5 | 8 => {
            let edge = r - rag * (1.0 + 0.5 * (theta * 3.0 + phase).sin());
            let core = 1.0 - smooth(0.065, 0.145, edge);
            let lip = (1.0 - smooth(0.012, 0.052, (edge - 0.17).abs()))
                * (0.35 + 0.65 * (theta * 5.0 + phase).cos().abs());
            h.height = -core * 1.7 + lip * 0.32;
            let steel = 1.0 - smooth(0.22, 0.29, edge);
            let primer = 1.0 - smooth(0.29, 0.345, edge);
            let abrasion = (theta * 31.0 + phase + r * 98.0).sin() * 0.5 + 0.5;
            h.color = Vec3::new(0.12, 0.105, 0.075)
                .lerp(Vec3::new(0.34, 0.29, 0.20), primer)
                .lerp(Vec3::new(0.48, 0.50, 0.49) * (0.75 + 0.25 * abrasion), steel)
                .lerp(Vec3::new(0.016, 0.018, 0.017), core);
            h.metal = steel * (1.0 - core * 0.94);
            h.rough = 0.72 - steel * 0.27 + core * 0.38;
            h.coverage = 1.0 - smooth(0.32, 0.43, edge);
            let fan = (1.0 - smooth(0.15, 0.48, r))
                * smooth(-0.4, 0.8, (theta - phase).cos());
            h.color = h.color.lerp(Vec3::new(0.055, 0.05, 0.04), fan * (1.0 - primer) * 0.65);
            for i in 0..9 {
                let a = crate::smart::rand(seed, i + 1) * std::f32::consts::TAU;
                let c = Vec2::new(a.cos(), a.sin()) * (0.25 + crate::smart::rand(seed, i + 20) * 0.18);
                let pit = 1.0 - smooth(0.004, 0.012 + crate::smart::rand(seed, i + 40) * 0.013, (uv - c).length());
                h.height -= pit * 0.32;
                h.color = h.color.lerp(Vec3::new(0.12, 0.13, 0.12), pit);
                h.coverage = h.coverage.max(pit * 0.9);
            }
            if code == 8 {
                let char = 1.0 - smooth(0.14, 0.40, edge);
                h.color = h.color.lerp(Vec3::new(0.035, 0.03, 0.025), char * 0.75);
                h.metal *= 1.0 - char * 0.75;
                h.rough = h.rough.max(0.8);
            }
        }
        6 => {
            let bowl = 1.0 - smooth(0.0, 0.45, r);
            h.height = -bowl * bowl;
            let crack = 1.0 - smooth(0.009, 0.023, (r - 0.29 - rag * 2.0).abs());
            h.coverage = crack * smooth(0.38, 0.68, crate::material::noise(Vec3::new(uv.x * 33.0, uv.y * 33.0, phase)));
            h.color = Vec3::new(0.38, 0.40, 0.38);
            h.rough = 0.46;
        }
        7 => {
            // Fine blister cells affect only the burnt paint, never the soot
            // opacity. High-frequency opacity used to expose a square noise
            // lattice (size / 49: 14 mm on the saved torso burn).
            let cells = crate::material::noise(Vec3::new(uv.x * 49.0, uv.y * 49.0, phase));
            let drift = crate::material::noise(Vec3::new(uv.x * 4.1 + 7.2, uv.y * 3.7, phase));
            let edge = r + (drift - 0.5) * 0.025;
            let bare = 1.0 - smooth(0.020, 0.045, edge);
            let burnt = 1.0 - smooth(0.035, 0.085, edge);
            let heat = smooth(0.015, 0.08, r);
            h.color = Vec3::new(0.055, 0.041, 0.028)
                .lerp(Vec3::new(0.085, 0.070, 0.045).lerp(Vec3::new(0.055, 0.065, 0.080), heat), bare);
            let ash = smooth(0.64, 0.82, cells) * burnt * (1.0 - bare);
            h.color = h.color.lerp(Vec3::new(0.28, 0.265, 0.235), ash * 0.35);
            // A continuous fan widens and diffuses uphill. Analytic falloff
            // removes texel-sized blotches without blurring the peeled center.
            let rise = smooth(-0.12, 0.48, uv.y);
            let width = 0.075 + rise * 0.19;
            let wind = 0.035 * rise * rise;
            let cross = (uv.x - wind) / width;
            let plume = (-cross * cross * 2.0).exp()
                * smooth(-0.18, -0.02, uv.y) * (1.0 - smooth(0.08, 0.49, uv.y));
            let halo = (-r * r * 65.0).exp() * 0.25;
            h.coverage = (burnt * 0.88).max(plume * 0.78).max(halo);
            h.rough = 0.91 - bare * (0.15 + cells * 0.10);
            h.metal = bare * (0.18 + cells * 0.12);
            h.height = smooth(0.48, 0.80, cells) * burnt * (1.0 - bare) * 0.08 - bare * 0.025;
        }
        9 | 10 => {
            let end = 1.0 - smooth(0.42, 0.5, uv.y.abs());
            let width = 0.065 + 0.025 * (uv.y * 9.0 + phase).sin()
                + 0.008 * (uv.y * 27.0 + phase).sin();
            let x = uv.x.abs();
            let groove = 1.0 - smooth(width * 0.65, width, x);
            let burr = (1.0 - smooth(0.006, 0.020, (x - width * 1.15).abs())) * (1.0 - groove);
            let chip = 1.0 - smooth(width + 0.028, width + 0.065, x + rag * 0.15);
            let fine = (1.0 - smooth(0.12, 0.22, x))
                * smooth(0.94, 0.995, (uv.x * 330.0 + phase + uv.y * 2.0).sin());
            h.coverage = chip.max(fine * 0.35) * end;
            h.height = (-groove * 0.18 + burr * 0.045 - fine * 0.008) * end * if code == 10 { 0.2 } else { 1.0 };
            h.color = Vec3::new(0.30, 0.255, 0.18)
                .lerp(Vec3::new(0.65, 0.67, 0.65), groove.max(fine))
                .lerp(Vec3::new(0.10, 0.085, 0.065), burr * 0.85);
            h.metal = groove.max(burr).max(fine);
            h.rough = 0.63 - groove * 0.31;
        }
        11..=13 => {
            // +Y is upstream: segment flow remaps this to global path progress.
            let t = (0.5 - uv.y).clamp(0.0, 1.0);
            let center = (t * 14.0 + phase).sin() * 0.018;
            let width = 0.095 + 0.23 * t;
            let x = (uv.x - center).abs();
            let breakup = crate::material::noise(Vec3::new(uv.x * 28.0, t * 19.0, phase));
            let stain = (1.0 - smooth(width * 0.15, width, x)) * (1.0 - smooth(0.68, 1.0, t));
            let source = 1.0 - smooth(0.015, 0.055, Vec2::new(uv.x, (t - 0.07) * 0.8).length());
            h.coverage = (stain * (0.45 + breakup * 0.45) * smooth(0.0, 0.06, t)).max(source);
            h.color = Vec3::new(0.29, 0.19, 0.105)
                .lerp(Vec3::new(0.49, 0.34, 0.19), smooth(0.0, 0.65, t))
                .lerp(Vec3::new(0.16, 0.105, 0.055), source * 0.8);
            h.rough = 0.86;
            h.metal = 0.0;
            if code == 12 {
                let wet_width = 0.012 + 0.060 * t;
                let wet = 1.0 - smooth(wet_width * 0.35, wet_width, x);
                let tail = 1.0 - smooth(0.80, 0.98, t);
                let drip = 1.0 - smooth(0.012, 0.027, Vec2::new(uv.x - center, (t - 0.86) * 0.7).length());
                let film = (wet * smooth(0.0, 0.035, t) * tail).max(drip * 0.8);
                let dust = (1.0 - wet) * stain * (0.35 + breakup * 0.3);
                h.coverage = (film * (0.72 + breakup * 0.08)).max(dust * 0.42);
                h.color = Vec3::new(0.27, 0.20, 0.12).lerp(Vec3::new(0.065, 0.035, 0.014), film);
                h.rough = 0.76 - film * 0.65;
            }
            if code == 13 {
                h.color = Vec3::new(0.34, 0.32, 0.26);
                h.rough = 0.80;
                h.coverage *= 0.55;
            }
        }
        14 | 15 => {
            let edge = 1.0 - smooth(0.35, 0.5, uv.y.abs());
            h.height = uv.x.max(0.0) * edge * 4.0;
            let jitter = if code == 14 {
                0.004 * ((uv.y * 123.0 + phase).sin() + 0.6 * (uv.y * 71.0).sin())
            } else {
                0.014 * (uv.y * 91.0 + phase).sin() + 0.009 * (uv.y * 157.0 - phase).sin()
            };
            let seam = 1.0
                - smooth(
                    if code == 14 { 0.003 } else { 0.010 },
                    if code == 14 { 0.010 } else { 0.030 },
                    (uv.x - jitter).abs(),
                );
            h.coverage = seam * edge;
            h.color = Vec3::new(0.55, 0.55, 0.50);
            if code == 15 {
                h.height -= seam * edge * 2.0;
            }
        }
        _ => {
            h.coverage = 0.0;
        }
    }
    h
}

pub fn sample_decal(d: &Decal, uv: Vec2) -> Hit {
    let mut coord = uv;
    if (11..=13).contains(&d.shape.code())
        && let Some([start, end]) = d.flow
    {
        coord.y = 1.0 - end + (uv.y + 0.5) * (end - start) - 0.5;
    }
    let mut h = sample(d.shape.code(), d.seed, coord);
    if (11..=13).contains(&d.shape.code()) {
        h.coverage = h.coverage.powf(1.0 / d.softness);
    }
    if d.melted_lip && d.shape == Shape::Scorch {
        h.height += (1.0 - smooth(0.01, 0.04, (uv.length() - 0.30).abs())) * 0.8;
    }
    h
}

pub fn depth(d: &Decal) -> f32 {
    d.depth.unwrap_or(
        d.size
            * match d.shape {
                Shape::BulletHit => 0.13,
                Shape::Dent => 0.10,
                Shape::Blast => 0.14,
                Shape::Scorch => {
                    if d.melted_lip {
                        0.04
                    } else {
                        0.006
                    }
                }
                Shape::Gouge => 0.055,
                Shape::Scrape => 0.015,
                Shape::Bend | Shape::Tear => 0.15,
                _ => 0.0,
            },
    )
}

pub fn height_at(list: &[Decal], p: Vec3) -> f32 {
    list.iter()
        .filter(|d| d.shape.code() >= 5)
        .map(|d| {
            let rel = p - Vec3::from(d.center);
            if rel.dot(Vec3::from(d.normal)).abs() > d.size * 0.35 {
                return 0.0;
            }
            let (u, v) = d.frame();
            let uv = Vec2::new(rel.dot(u), rel.dot(v));
            if uv.abs().max_element() > 0.5 {
                return 0.0;
            }
            if d.shape == Shape::Bend && d.hinge.is_some() {
                0.0
            } else {
                sample_decal(d, uv).height * depth(d) * d.opacity
            }
        })
        .sum()
}

/// Compile world masks into local coefficients. This also makes bakes use
/// the same world transform as film, including bone-attached rigid parts.
pub fn world_material(mut m: Material, world: Mat4) -> Material {
    // Procedural paint relief is specified in meters, independent of the
    // asset's uniform modeling scale. For nonuniform transforms use mean
    // axis scale, keeping the triplanar grain bounded rather than stretching
    // it to the overall asset bounds.
    let scale = world.to_scale_rotation_translation().0.abs().element_sum() / 3.0;
    let paint_preset = matches!(m.name.as_str(), "military_paint" | "battle_worn" | "desert_dusty" | "muddy" | "burnt_wreck" | "dusty_khaki" | "olive_drab" | "gunmetal" | "worn_steel");
    for l in &mut m.layers {
        if (paint_preset || l.name.starts_with("wear:")) && l.detail_scale > 0.0 {
            l.detail_scale /= scale.max(1e-6);
        }
        for t in &mut l.mask {
            if (l.name.ends_with("edge chips") || l.name.ends_with("rubbed paint halo"))
                && matches!(t.source, Source::Noise) && t.scale > 0.0 {
                t.scale /= scale.max(1e-6);
            }
            if !t.world {
                continue;
            }
            match t.source {
                Source::Up | Source::Down | Source::Direction => {
                    let d = match t.source {
                        Source::Up => Vec3::Y,
                        Source::Down => -Vec3::Y,
                        _ => Vec3::from(t.direction),
                    };
                    let normal = world.inverse().transpose();
                    let [x, y, z] = [
                        normal.x_axis.truncate(),
                        normal.y_axis.truncate(),
                        normal.z_axis.truncate(),
                    ];
                    t.direction = normal
                        .transpose()
                        .transform_vector3(d.normalize_or(Vec3::Y))
                        .to_array();
                    t.metric = [x.dot(x), y.dot(y), z.dot(z), x.dot(y), x.dot(z), y.dot(z)];
                    t.source = Source::DirectionMetric;
                }
                Source::Streaks => {
                    t.direction = world.inverse().transform_vector3(Vec3::Y).normalize_or(Vec3::Y).to_array();
                }
                Source::Height => {
                    let span = t.range[1] - t.range[0];
                    let span = if span.abs() < 1e-6 { 1e-6 } else { span };
                    t.direction = Vec3::new(world.x_axis.y, world.y_axis.y, world.z_axis.y)
                        .to_array()
                        .map(|x| x / span);
                    t.value = (world.w_axis.y - t.range[0]) / span;
                    t.source = Source::HeightPlane;
                }
                _ => {}
            }
            t.world = false;
        }
    }
    m
}

pub fn wear_layers(name: &str, amount: f32) -> Option<Vec<Layer>> {
    let noise = |scale, seed| Term {
        source: Source::Noise,
        scale,
        seed,
        octaves: 3,
        ..Term::default()
    };
    let layer = |name: &str, color, rough, metal, mask| Layer {
        name: name.into(),
        color,
        roughness: Some(rough),
        metallic: metal,
        mask,
        ..Layer::default()
    };
    // Never max a broad handling field over the flakes: that fills the
    // entire constant-curvature bevel, and the final levels cut a straight
    // stripe out of its interpolated edge mask. Millimetre flakes control
    // *every* boundary; handling and exposure only modulate their density.
    let chips = |threshold| {
        let mut handling = noise(0.11, 211.0);
        handling.amount = 0.45;
        let mut l = layer("edge chips", None, 0.46, None, vec![
            handling,
            Term { source: Source::Constant, value: 0.55, op: Op::Add, ..Term::default() },
            noise(0.004, 17.0),
            Term { source: Source::Edges, threshold: Some(0.22), contrast: Some(5.0), ..Term::default() },
            Term { source: Source::Ao, invert: true, ..Term::default() },
            // Soft orientation levels leave vertical handholds eligible.
            Term { source: Source::Up, world: true,
                threshold: Some(-0.5), contrast: Some(0.6), ..Term::default() },
        ]);
        l.threshold = Some(threshold);
        l.contrast = Some(12.0);
        l
    };
    let mut out = match name {
        "factory_new" => vec![],
        "military_paint" => {
            let mut paint = layer("smooth matte paint", None, 0.78, Some(0.0), vec![]);
            paint.variation = 0.35;
            paint.variation_scale = 1.2;
            paint.detail = Some("stone".into());
            paint.detail_scale = 0.004;
            paint.detail_strength = 0.035;
            paint.detail_color = 0.0;
            let mut handled = layer("polished handling", None, 0.57, None, vec![noise(0.18, 29.0)]);
            handled.opacity = 0.65;
            let mut wipes = layer("soft wipe marks", None, 0.48, None, vec![
                Term {source:Source::Streaks, direction:[1.0,0.18,0.0], scale:0.035,
                    seed:61.0, octaves:2, ..Term::default()}, noise(0.35, 91.0)]);
            wipes.opacity = 0.28;
            wipes.tint = Some([0.98, 0.975, 0.965]);
            vec![paint, handled, wipes]
        },
        "battle_worn" => {
            let mut faded = layer("aged paint texture", None, 0.72, None, vec![]);
            faded.tint = Some([1.015, 1.01, 1.0]);
            faded.detail = Some("stone".into());
            faded.detail_scale = 0.004;
            faded.detail_strength = 0.025;
            faded.detail_color = 0.0;
            faded.variation = 0.20;
            faded.variation_scale = 0.42;
            let mut smudges = layer("handled paint and smudges", None, 0.43, None,
                vec![noise(0.32, 29.0)]);
            smudges.tint = Some([0.85, 0.84, 0.82]);
            smudges.opacity = 0.75;
            smudges.mask[0].threshold = Some(0.5);
            smudges.mask[0].contrast = Some(3.5);
            smudges.detail = Some("metal_scratched".into());
            smudges.detail_scale = 0.08;
            smudges.detail_strength = 0.018;
            smudges.detail_color = 0.0;
            let mut halo = chips(0.36);
            halo.name = "rubbed paint halo".into();
            halo.tint = Some([1.09, 1.07, 1.04]);
            halo.roughness = Some(0.59);
            halo.opacity = 0.55;
            let mut primer = chips(0.44);
            primer.color = Some([0.25, 0.16, 0.11]);
            primer.metallic = Some(0.0);
            primer.roughness = Some(0.68);
            let mut metal = chips(0.52);
            metal.color = Some([0.22, 0.215, 0.20]);
            metal.metallic = Some(1.0);
            metal.roughness = Some(0.42);
            let mut grime = layer("cavity grime", Some([0.065, 0.058, 0.046]), 0.86, Some(0.0),
                vec![Term { source: Source::Cavity, amount: 1.4, ..Term::default() },
                     Term { source: Source::Ao, amount: 0.8, op: Op::Max, ..Term::default() }]);
            grime.opacity = 0.55;
            // Runoff is saved as sparse geometry-seeded WaterStain paths by
            // apply_wear, never as a global elongated noise field.
            let mut lower = layer("lower panel grime", Some([0.16,0.145,0.115]), 0.84, Some(0.0),
                vec![Term { source: Source::Height, range:[1.0,0.0], ..Term::default() }, noise(0.24, 83.0)]);
            lower.opacity = 0.38;
            vec![faded, smudges, halo, primer, metal, grime, lower]
        }
        "desert_dusty" => {
            let up = Term { source: Source::Up, world: true, ..Term::default() };
            let mut fade = layer("sun fade", None, 0.65, None, vec![up.clone()]);
            fade.tint = Some([1.045, 1.04, 1.025]);
            fade.opacity = 0.3;
            fade.detail = Some("stone".into());
            fade.detail_scale = 0.004;
            fade.detail_strength = 0.025;
            fade.detail_color = 0.0;
            let mut dust = layer("settled desert dust", Some([0.64, 0.60, 0.49]), 0.88, Some(0.0),
                vec![up.clone(), Term { source: Source::Cavity, amount: 0.5, op: Op::Max, ..Term::default() }]);
            dust.opacity = 0.42;
            dust.variation = 0.06;
            dust.variation_scale = 0.45;
            let mut low = layer("ground dust", Some([0.49, 0.45, 0.36]), 0.90, Some(0.0),
                vec![Term { source: Source::Height, world: true, range: [1.8, 0.08], ..Term::default() },
                    up, Term { amount: 0.8, ..noise(0.38, 21.0) }]);
            low.opacity = 0.16;
            vec![fade, dust, low]
        }
        "muddy" => {
            let height = Term { source: Source::Height, world: true,
                range: [1.2, 0.05], ..Term::default() };
            let mut mud = layer("ground mud", Some([0.32, 0.255, 0.17]), 0.48, Some(0.0),
                vec![Term { source: Source::Cavity, amount: 1.5, ..Term::default() },
                    Term { value: 0.65, op: Op::Max, ..Term::default() }, height.clone()]);
            mud.detail = Some("stone".into());
            mud.detail_scale = 0.085;
            mud.detail_strength = 0.4;
            mud.detail_color = 0.025;
            mud.variation = 0.08;
            mud.variation_scale = 0.12;
            let mut dry = layer("dry tread dust", Some([0.52, 0.46, 0.34]), 0.91, Some(0.0),
                vec![height, Term { source: Source::Up, world: true, ..Term::default() }]);
            dry.opacity = 0.27;
            vec![mud, dry]
        }
        "burnt_wreck" => {
            let mut charred = layer(
                "blistered char",
                Some([0.035, 0.031, 0.026]),
                0.95,
                Some(0.0),
                vec![noise(0.08, 23.0)],
            );
            charred.threshold = Some(0.25);
            charred.contrast = Some(6.0);
            charred.detail = Some("stone".into());
            charred.detail_scale = 0.025;
            charred.detail_strength = 0.15;
            let mut steel = chips(0.22);
            steel.color = Some([0.24, 0.25, 0.23]);
            steel.metallic = Some(1.0);
            vec![charred, steel]
        }
        _ => return None,
    };
    // Dust and mud still expose clean paint between deposits. Give those
    // areas fine relief and texture-driven roughness rather than a flat BRDF.
    if matches!(name, "desert_dusty" | "muddy") {
        out.push(Layer {name:"surface grain".into(), detail:Some("stone".into()),
            detail_scale:0.012, detail_strength:0.045, detail_color:0.0,
            opacity:0.7, ..Layer::default()});
    }
    for l in &mut out {
        if matches!(l.name.as_str(), "edge chips" | "rubbed paint halo") && amount > 0.0 {
            l.threshold = l
                .threshold
                .map(|t| t + (1.0 - amount.clamp(0.0, 1.0)) * 0.15);
        }
        l.opacity *= amount.clamp(0.0, 1.0);
    }
    Some(out)
}

/// Compile the local part's grime gradient independently of its height above ground.
pub fn part_grime(m: &mut Material, lo: Vec3, hi: Vec3) {
    for l in &mut m.layers {
        if l.name.ends_with("lower panel grime") && let Some(t) = l.mask.first_mut() {
            t.source = Source::Height;
            t.world = false;
            t.range = [hi.y,lo.y];
        }
    }
}

/// Nearest point on triangles, preserving sub-vertex placement of small hits.
pub fn snap(mesh: &Mesh, p: Vec3) -> Option<(Vec3, Vec3)> {
    snap_faces(mesh, p, None, mesh.indices.chunks_exact(3))
}
/// Select one surface for a world-space stamp; ties keep target order.
pub fn stamp_owner<'a>(p: Vec3, targets: impl Iterator<Item = (u64, &'a Mesh, Mat4)>) -> Option<u64> {
    targets.filter_map(|(id, mesh, world)| {
        let (q, _) = snap(mesh, world.inverse().transform_point3(p))?;
        Some((id, world.transform_point3(q).distance_squared(p)))
    }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|v| v.0)
}

fn snap_faces<'a>(
    mesh: &Mesh,
    p: Vec3,
    facing: Option<Vec3>,
    triangles: impl Iterator<Item = &'a [u32]>,
) -> Option<(Vec3, Vec3)> {
    let mut best = None;
    let mut distance = f32::INFINITY;
    for t in triangles {
        let a = Vec3::from(mesh.vertices[t[0] as usize].pos);
        let b = Vec3::from(mesh.vertices[t[1] as usize].pos);
        let c = Vec3::from(mesh.vertices[t[2] as usize].pos);
        let n = (b - a).cross(c - a).normalize_or(Vec3::Z);
        if facing.is_some_and(|f| n.dot(f) < -0.05) {
            continue;
        }
        let q = p - n * (p - a).dot(n);
        let inside = [(a, b), (b, c), (c, a)]
            .iter()
            .all(|(x, y)| (*y - *x).cross(q - *x).dot(n) >= -1e-7);
        let q = if inside {
            q
        } else {
            [(a, b), (b, c), (c, a)]
                .iter()
                .map(|(x, y)| {
                    *x + (*y - *x)
                        * ((p - *x).dot(*y - *x) / (*y - *x).length_squared().max(1e-12))
                            .clamp(0.0, 1.0)
                })
                .min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p)))
                .unwrap()
        };
        let d = q.distance_squared(p);
        if d < distance {
            distance = d;
            best = Some((q, n));
        }
    }
    best
}

/// Integrate tangential gravity, then project back onto the surface. A step
/// is accepted only if its world height decreases, so bumps never send oil uphill.
pub fn downhill(mesh: &Mesh, world: Mat4, start: Vec3, length: f32, width: f32) -> Vec<Vec3> {
    let inv = world.inverse();
    let mut p = start;
    let mut out = vec![p];
    let radius = length * inv.to_scale_rotation_translation().0.abs().max_element() + width;
    let triangles: Vec<[u32; 3]> = mesh
        .indices
        .chunks_exact(3)
        .filter(|t| {
            let p = [t[0], t[1], t[2]].map(|i| Vec3::from(mesh.vertices[i as usize].pos));
            let lo = p[0].min(p[1]).min(p[2]);
            let hi = p[0].max(p[1]).max(p[2]);
            (lo - start).max(start - hi).max(Vec3::ZERO).length() <= radius
        })
        .map(|t| [t[0], t[1], t[2]])
        .collect();
    let steps = (length / (width * 0.25).max(0.002)).ceil().clamp(2.0, 96.0) as usize;
    let step = length / steps as f32;
    for _ in 0..steps {
        let Some((q, n)) = snap_faces(mesh, p, None, triangles.iter().map(|t| t.as_slice())) else {
            break;
        };
        let wn = inv.transpose().transform_vector3(n).normalize_or(Vec3::Z);
        let tangent = -Vec3::Y + wn * wn.y;
        let wp = world.transform_point3(q);
        let direction = if tangent.length_squared() < 1e-6 {
            -Vec3::Y
        } else {
            tangent.normalize()
        };
        let guess = inv.transform_point3(wp + direction * step);
        let mut next =
            snap_faces(mesh, guess, Some(n), triangles.iter().map(|t| t.as_slice())).map(|p| p.0);
        if next.is_none_or(|p| world.transform_point3(p).y >= wp.y - step * 0.15) {
            // Drain off a bolt's underside onto the plate below, while a
            // level surface still stops because projection gains no height.
            next = snap_faces(
                mesh,
                inv.transform_point3(wp - Vec3::Y * step),
                Some(n),
                triangles.iter().map(|t| t.as_slice()),
            )
            .map(|p| p.0);
        }
        let Some(next) = next.filter(|p| world.transform_point3(*p).y < wp.y - 1e-6) else {
            break;
        };
        p = next;
        out.push(p);
    }
    out
}

/// Add vertices only where damage requires them. Splits propagate across
/// shared edges to keep the surface watertight. World wear can additionally
/// refine sparse flat panels so curvature masks do not cover entire faces.
pub fn refine_mesh(mesh: &mut Mesh, list: &[Decal], wear_width: Option<f32>) {
    use std::collections::HashMap;
    let geometric: Vec<&Decal> = list.iter().collect();
    let wear = wear_width.is_some();
    let wear_width = wear_width.unwrap_or(0.02);
    let budget = mesh.indices.len() + 600_000;
    for _ in 0..8 {
        if mesh.indices.len() > budget {
            break;
        }
        let mut split = std::collections::HashSet::new();
        for t in mesh.indices.chunks_exact(3) {
            let points = [t[0], t[1], t[2]].map(|i| Vec3::from(mesh.vertices[i as usize].pos));
            let lo = points[0].min(points[1]).min(points[2]);
            let hi = points[0].max(points[1]).max(points[2]);
            if wear {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    if Vec3::from(mesh.vertices[a as usize].pos)
                        .distance(Vec3::from(mesh.vertices[b as usize].pos))
                        > wear_width * 1.5
                    {
                        split.insert((a.min(b), a.max(b)));
                    }
                }
            }
            for d in &geometric {
                let center = Vec3::from(d.center);
                let radius = d.size * d.aspect.max(1.0) * 0.65;
                if (lo - center).max(center - hi).max(Vec3::ZERO).length() > radius {
                    continue;
                }
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    if Vec3::from(mesh.vertices[a as usize].pos)
                        .distance(Vec3::from(mesh.vertices[b as usize].pos))
                        > d.size / 10.0
                    {
                        split.insert((a.min(b), a.max(b)));
                    }
                }
            }
        }
        if split.is_empty() {
            break;
        }
        let growth = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])]
                    .iter()
                    .filter(|(a, b)| split.contains(&((*a).min(*b), (*a).max(*b))))
                    .count()
                    * 3
            })
            .sum::<usize>();
        if mesh.indices.len() + growth > budget {
            break;
        }
        let mut mid = HashMap::new();
        // Sorted keys keep topology and serialization deterministic.
        let mut keys: Vec<_> = split.into_iter().collect();
        keys.sort_unstable();
        for (a, b) in keys {
            let av = mesh.vertices[a as usize];
            let bv = mesh.vertices[b as usize];
            let mut v = av;
            v.pos = ((Vec3::from(av.pos) + Vec3::from(bv.pos)) * 0.5).to_array();
            v.normal = (Vec3::from(av.normal) + Vec3::from(bv.normal))
                .normalize_or(Vec3::Z)
                .to_array();
            v.color = std::array::from_fn(|i| (av.color[i] + bv.color[i]) * 0.5);
            v.base = std::array::from_fn(|i| (av.base[i] + bv.base[i]) * 0.5);
            v.surface = std::array::from_fn(|i| (av.surface[i] + bv.surface[i]) * 0.5);
            mid.insert((a, b), mesh.vertices.len() as u32);
            mesh.vertices.push(v);
        }
        let mut indices = Vec::new();
        for t in mesh.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]];
            let [ab, bc, ca] =
                [(a, b), (b, c), (c, a)].map(|(x, y)| mid.get(&(x.min(y), x.max(y))).copied());
            match (ab, bc, ca) {
                (None, None, None) => indices.extend([a, b, c]),
                (Some(x), None, None) => indices.extend([a, x, c, x, b, c]),
                (None, Some(x), None) => indices.extend([a, b, x, a, x, c]),
                (None, None, Some(x)) => indices.extend([a, b, x, x, b, c]),
                (Some(x), Some(y), None) => indices.extend([a, x, c, x, y, c, x, b, y]),
                (None, Some(y), Some(z)) => indices.extend([a, b, z, z, b, y, z, y, c]),
                (Some(x), None, Some(z)) => indices.extend([a, x, z, x, b, c, x, c, z]),
                (Some(x), Some(y), Some(z)) => indices.extend([a, x, z, x, b, y, z, y, c, x, y, z]),
            }
        }
        mesh.indices = indices;
    }
}

/// Refine only triangles intersecting stamp boxes. Shared edge midpoints
/// avoid cracks. This lets small hits deform even a two-triangle kit panel.
pub fn displace_mesh(mesh: &mut Mesh, list: &[Decal]) {
    // Kit builders have already evaluated their smart stack. Start from
    // its saved base so rebuilding damage does not apply layers twice.
    for v in &mut mesh.vertices {
        if v.base[3] >= 0.0
            && mesh
                .materials
                .get(v.material as usize)
                .is_some_and(crate::smart::layered)
        {
            let m = &mesh.materials[v.material as usize];
            v.color = [v.base[0], v.base[1], v.base[2]];
            v.surface = [v.base[3], m.metallic, m.clearcoat, m.sheen];
        }
    }
    let (lo, hi) = mesh.bounds();
    let wear_width = ((hi - lo).max_element() * 0.02).clamp(0.004, 0.03);
    refine_mesh(
        mesh,
        list,
        mesh.materials
            .iter()
            .any(crate::smart::layered)
            .then_some(wear_width),
    );
    if list.iter().any(|d| d.shape == Shape::Tear) {
        mesh.indices = mesh
            .indices
            .chunks_exact(3)
            .filter(|t| {
                let p = t
                    .iter()
                    .map(|i| Vec3::from(mesh.vertices[*i as usize].pos))
                    .sum::<Vec3>()
                    / 3.0;
                tear_cut(list, p) >= 0.0
            })
            .flatten()
            .copied()
            .collect();
    }
    for v in &mut mesh.vertices {
        let p = Vec3::from(v.pos);
        let mut q = p;
        for d in list {
            let n = Vec3::from(d.normal);
            if Vec3::from(v.normal).dot(n) < 0.15 {
                continue;
            }
            let h = height_at(std::slice::from_ref(d), p);
            if d.hinge.is_some() {
                let old = Vec3::from(v.normal);
                let rotated = (fold_point(d, q + old * 0.001, false) - fold_point(d, q, false))
                    .normalize_or(old);
                v.normal = rotated.to_array();
            }
            q = fold_point(d, q, false);
            q += n * h;
        }
        v.pos = q.to_array();
    }
    // Keep crease normals; add the local displacement gradient only.
    for v in &mut mesh.vertices {
        let p = Vec3::from(v.pos);
        let n = Vec3::from(v.normal);
        let e = 0.0002;
        let g = Vec3::new(
            height_at(list, p + Vec3::X * e) - height_at(list, p - Vec3::X * e),
            height_at(list, p + Vec3::Y * e) - height_at(list, p - Vec3::Y * e),
            height_at(list, p + Vec3::Z * e) - height_at(list, p - Vec3::Z * e),
        ) / (2.0 * e);
        v.normal = (n - g + n * g.dot(n)).normalize_or(n).to_array();
    }
    mesh.edges.clear();
    crate::smart::finish_mesh(mesh, wear_width);
    mesh.decals = std::sync::Arc::new(list.to_vec());
    for v in &mut mesh.vertices {
        let at = crate::smart::At {
            p: Vec3::from(v.pos),
            n: Vec3::from(v.normal),
            ..Default::default()
        };
        let m = &mesh.materials[v.material as usize];
        let look = crate::smart::decals(
            list,
            &at,
            crate::smart::Look {
                metal: v.surface[1],
                coat: v.surface[2],
                sheen: v.surface[3],
                emission: v.light[0],
                ..crate::smart::Look::of(m, Vec3::from(v.color), v.surface[0], at.n)
            },
        );
        v.color = look.color.to_array();
        v.surface = [look.rough, look.metal, look.coat, look.sheen];
    }
}

pub fn library(scene: &crate::scene::Scene, o: &crate::scene::Object) -> Vec<Material> {
    let mut out = scene.materials.clone();
    let mut names = o
        .clay
        .as_ref()
        .map_or_else(Vec::new, |c| c.material_names());
    names.push(o.material.clone().unwrap_or_default());
    for name in names {
        let name = if name.is_empty() {
            o.material.as_deref().unwrap_or("")
        } else {
            &name
        };
        if !out.iter().any(|m| m.name == name) {
            let mut m = Material::resolve(&scene.materials, name);
            m.name = name.into();
            out.push(m);
        }
    }
    out.iter()
        .map(|m| {
            let m = Material::resolve(&out, &m.name);
            world_material(m, scene.world_matrix(o.id))
        })
        .collect()
}

/// A few drains seeded from downward-facing convex lips and concave bolt /
/// hole roots. Spacing is in world metres, independent of tessellation.
fn runoff(mesh: &Mesh, clay: &crate::clay::Clay, field: &crate::clay::Field, world: Mat4, amount: f32, budget: usize) -> Vec<Decal> {
    let wear = crate::weather::surface_wear(mesh, clay, field);
    let mut candidates: Vec<_> = mesh.vertices.iter().enumerate().filter_map(|(i, v)| {
        let n = world.inverse().transpose().transform_vector3(Vec3::from(v.normal)).normalize_or(Vec3::Y);
        let m = mesh.materials.get(v.material as usize)?;
        if m.emission > 0.0 || m.transmission > 0.0 || n.y > -0.08 || n.y < -0.85 {
            return None;
        }
        let strength = wear[i][0].max(wear[i][1]);
        (strength > 0.18).then_some((i, strength))
    }).collect();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut roots = Vec::<Vec3>::new();
    let mut out = Vec::new();
    for (i, _) in candidates {
        let p = Vec3::from(mesh.vertices[i].pos);
        let wp = world.transform_point3(p);
        if roots.iter().any(|q| q.distance(wp) < 0.48) { continue; }
        roots.push(wp);
        if roots.len() > 24 { break; }
        let seed = 3701 + i as u32;
        let req = Stamp {
            kind: Shape::WaterStain, point: Some(wp),
            size: Some(0.045 + crate::smart::rand(seed, 0) * 0.065),
            length: 0.09 + crate::smart::rand(seed, 1) * 0.24,
            seed, ..Stamp::default()
        };
        let Ok(mut path) = stamps(mesh, world, false, &req) else { continue; };
        if out.len() + path.len() > budget { continue; }
        for d in &mut path {
            d.wear_preset = "battle_worn".into();
            d.opacity *= amount * (0.28 + crate::smart::rand(seed, 2) * 0.20);
        }
        out.extend(path);
        if out.len() >= budget || out.iter().map(|d| d.seed).collect::<std::collections::HashSet<_>>().len() >= 3 { break; }
    }
    out
}

/// Add a preset to every material used by an object, preserving its paint.
/// Materials stay shared: later base edits affect every user of the finish.
pub fn apply_wear(
    scene: &mut crate::scene::Scene,
    id: u64,
    preset: &str,
    amount: f32,
) -> Result<(), String> {
    let additions = wear_layers(preset, amount).ok_or("unknown wear preset")?;
    let o = scene.get(id).ok_or("object vanished")?.clone();
    let names = o
        .clay
        .as_ref()
        .map_or_else(|| vec![String::new()], |c| c.material_names());
    let mut replacements = std::collections::HashMap::new();
    for name in names {
        let source = if name.is_empty() {
            o.material.as_deref().unwrap_or("")
        } else {
            &name
        };
        // Reconnect copies saved by earlier versions when their base still
        // exists. The legacy pattern is damage_<object id>_<slot>_<base>.
        let legacy = source.strip_prefix("damage_").and_then(|s| {
            let mut parts=s.splitn(3,'_');
            parts.next()?.parse::<u64>().ok()?;
            parts.next()?.parse::<usize>().ok()?;
            parts.next()
        }).filter(|base| Material::find(&scene.materials,base).is_some());
        let origin = legacy.unwrap_or(source);
        let shared_name = if origin.is_empty() { "clay".to_string() } else { origin.to_string() };
        let mut m = Material::resolve(&scene.materials, origin);
        let optical = m.emission > 0.0 || m.transmission > 0.0;
        let bare = m.metallic >= 0.7
            || m.layers
                .first()
                .is_some_and(|l| l.mask.is_empty() && l.metallic.is_some_and(|v| v >= 0.7));
        let soft = ["rubber", "strap", "cloth", "leather", "skin", "hair", "velvet"]
            .iter().any(|tag| origin.contains(tag));
        if optical {
            let original_name = origin.to_string();
            replacements.insert(name, original_name);
            continue;
        }
        if preset != "factory_new" {
            for l in &mut m.layers {
                for t in &mut l.mask {
                    if matches!(
                        t.source,
                        Source::Height | Source::Up | Source::Down | Source::Direction
                    ) {
                        t.world = true;
                    }
                }
            }
        }
        if preset == "factory_new" {
            m.layers.retain(|l| !l.name.starts_with("wear:"));
        } else {
            m.layers
                .retain(|l| !l.name.starts_with(&format!("wear:{preset}:")));
            // Replace legacy continuous edge bands and local dirt height when
            // converting the look to physical paint chipping.
            if preset == "battle_worn" && !soft && !bare {
                m.edge_wear = 0.0;
                m.layers.retain(|l| {
                    l.name.starts_with("wear:") || !l.mask.iter().any(|t| t.source == Source::Edges)
                });
            }
            if preset == "desert_dusty" || preset == "muddy" {
                // Replace procedural dirt deposits, retaining painted masks,
                // custom finishes and explicit damage stamps.
                m.layers.retain(|l| l.name.starts_with("wear:")
                    || !(l.role == "dirt" && !l.mask.iter().any(|t| t.source == Source::Painted) && l.mask.iter().any(|t|
                        matches!(t.source, Source::Height | Source::Up | Source::Cavity | Source::Noise))));
            }
            if m.layers.len() + additions.len() > crate::smart::MAX_LAYERS {
                return Err("wear stack exceeds 16 layers; simplify the base material".into());
            }
            m.layers.extend(
                additions
                    .iter()
                    .filter(|l| {
                        !bare && !soft
                            || l.name != "edge chips" && !l.name.starts_with("stone chip")
                    })
                    .cloned()
                    .map(|mut l| {
                        if bare || soft {
                            if preset == "desert_dusty" {
                                // Keep dark finishes readable while allowing thicker dust on paint.
                                l.opacity *= if l.name == "settled desert dust" { 0.25 } else { 0.45 };
                            } else if preset == "battle_worn" && l.name != "cavity grime" {
                                l.opacity *= 0.15;
                            }
                        }
                        if matches!(l.name.as_str(), "aged paint texture" | "smooth matte paint" | "surface grain") && (bare || origin.contains("rubber")) {
                            if bare { l.metallic = None; l.roughness = Some(0.55); }
                            l.detail = Some(if bare { "metal_scratched" } else { "stone" }.into());
                            l.detail_scale = if bare {0.16} else {0.006};
                            l.detail_strength = if bare {0.35} else {0.25};
                        }
                        l.name = format!("wear:{preset}:{}", l.name);
                        l
                    }),
            );
        }
        m.name = shared_name.clone();
        if let Some(old) = scene.materials.iter_mut().find(|m| m.name == shared_name) {
            *old = m;
        } else {
            if scene.materials.len() >= crate::material::MAX_MATERIALS {return Err("material library full".into());}
            scene.materials.push(m);
        }
        replacements.insert(name, shared_name);
    }
    let drains = if preset == "battle_worn" && amount > 0.0 {
        o.clay.as_ref().map(|c| {
            let mut c = c.clone();
            c.decals.retain(|d| d.wear_preset.is_empty());
            c.resolution = c.resolution.min(64);
            let budget = 12.min(crate::smart::MAX_DECALS.saturating_sub(c.decals.len()));
            if budget == 0 { return Vec::new(); }
            // Runoff placement uses geometry only: applying a preset again
            // must not change the source mesh's tessellation or seed order.
            let bare_library: Vec<_> = scene.materials.iter().map(|m| Material {
                name: m.name.clone(), emission: m.emission, transmission: m.transmission,
                metallic: m.metallic, ..Default::default()
            }).collect();
            let mesh = c.build_mesh_with(&bare_library, o.material.as_deref().unwrap_or(""));
            runoff(&mesh, &c, &crate::clay::Field::new(&c, c.cell_size().unwrap_or(0.01)), scene.world_matrix(id), amount.clamp(0.0, 1.0), budget)
        }).unwrap_or_default()
    } else { Vec::new() };
    let o = scene.get_mut(id).unwrap();
    if let Some(name) = replacements.get("") { o.material = Some(name.clone()); }
    if let Some(c) = &mut o.clay {
        if preset == "factory_new" || preset == "battle_worn" {
            c.decals.retain(|d| d.wear_preset != "battle_worn");
            c.decals.extend(drains);
        }
        for b in &mut c.blobs {
            if !b.material.is_empty() {
                b.material = replacements[&b.material].clone();
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Stamp {
    #[serde(alias = "shape")]
    pub kind: Shape,
    pub point: Option<Vec3>,
    pub points: Vec<Vec3>,
    pub normal: Option<Vec3>,
    #[serde(alias = "width")]
    pub size: Option<f32>,
    pub caliber_mm: f32,
    /// Obliquity from the surface normal; elongates a projectile gouge.
    pub impact_angle: f32,
    pub rotate: f32,
    pub color: Option<[f32;3]>,
    pub softness: f32,
    pub depth: Option<f32>,
    pub length: f32,
    pub seed: u32,
    pub count: usize,
    pub region: Option<Region>,
    pub hinge: Option<[Vec3; 2]>,
    pub angle: f32,
    pub melted_lip: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub center: Vec3,
    pub radii: Vec3,
}
impl Default for Stamp {
    fn default() -> Self {
        Self {
            kind: Shape::BulletHit,
            point: None,
            points: Vec::new(),
            normal: None,
            size: None,
            caliber_mm: 7.62,
            impact_angle: 0.0, rotate: 0.0, color: None, softness: 1.0,
            depth: None,
            length: 0.25,
            seed: 0,
            count: 1,
            region: None,
            hinge: None,
            angle: 30.0,
            melted_lip: false,
        }
    }
}

/// Resolve a stamp or a deterministic batch. Coordinates default to world
/// rest pose, matching sculpt/paint. Build the whole list before committing.
pub fn stamps(mesh: &Mesh, world: Mat4, local: bool, s: &Stamp) -> Result<Vec<Decal>, String> {
    if s.kind.code() < 5 {
        return Err("damage kind must be bullet_hit/dent/scorch/blast/gouge/scrape/rust_streak/oil_leak/water_stain/bend/tear".into());
    }
    if s.count == 0 || s.count > 64 || s.points.len() > 64 {
        return Err("damage count and points are limited to 64".into());
    }
    if !(5.56..=25.0).contains(&s.caliber_mm)
        || !s.length.is_finite()
        || s.length <= 0.0
        || s.length > 100.0
        || !s.angle.is_finite()
        || s.angle.abs() > 85.0
    {
        return Err("caliber_mm must be 5.56..25, length 0..100 and angle -85..85".into());
    }
    if s.depth.is_some_and(|d| !d.is_finite() || d.abs() > 4.0) {
        return Err("depth must be finite and within four meters".into());
    }
    if s.normal
        .is_some_and(|n| !n.is_finite() || n.length_squared() < 1e-8)
    {
        return Err("normal must be finite and nonzero".into());
    }
    if !s.impact_angle.is_finite() || !(0.0..=80.0).contains(&s.impact_angle)
        || !s.rotate.is_finite() || !s.softness.is_finite() || !(0.1..=2.0).contains(&s.softness)
        || s.color.is_some_and(|c| c.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))) {
        return Err("impact_angle must be 0..80, softness 0.1..2, color RGB 0..1 and rotate finite".into());
    }
    let inv = world.inverse();
    let input = if local { Mat4::IDENTITY } else { inv };
    let scale = if local {
        1.0
    } else {
        inv.to_scale_rotation_translation().0.abs().element_sum() / 3.0
    };
    let size = s.size.unwrap_or(match s.kind {
        Shape::BulletHit => s.caliber_mm / 1000.0 * 4.0,
        Shape::Dent => 0.15,
        Shape::Scorch | Shape::Blast => 0.35,
        Shape::Gouge | Shape::Scrape => 0.035,
        Shape::Bend | Shape::Tear => 0.25,
        _ => 0.06,
    }) * scale;
    if !size.is_finite() || !(0.0001..=100.0).contains(&size) {
        return Err("size must be finite, positive and at most 100 meters".into());
    }
    let mut points = s.points.clone();
    if let Some(p) = s.point {
        points.insert(0, p);
    }
    if let Some(region) = &s.region {
        if !region.center.is_finite()
            || !region.radii.is_finite()
            || region.radii.min_element() <= 0.0
        {
            return Err("region center/radii must be finite with positive radii".into());
        }
        // Sample triangle area, not mesh vertices (avoids dense-feature bias).
        let mut triangles = Vec::new();
        let mut total = 0.0;
        for t in mesh.indices.chunks_exact(3) {
            let p = [t[0], t[1], t[2]].map(|i| {
                let p = Vec3::from(mesh.vertices[i as usize].pos);
                if local { p } else { world.transform_point3(p) }
            });
            let center = (p[0] + p[1] + p[2]) / 3.0;
            if ((center - region.center) / region.radii).length() > 1.0 {
                continue;
            }
            total += (p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5;
            triangles.push((total, p));
        }
        if triangles.is_empty() {
            return Err("region contains no surface triangles".into());
        }
        for i in 0..s.count {
            let pick = crate::smart::rand(s.seed, i as u32 * 3) * total;
            let t = &triangles[triangles
                .partition_point(|(a, _)| *a < pick)
                .min(triangles.len() - 1)]
            .1;
            let a = crate::smart::rand(s.seed, i as u32 * 3 + 1).sqrt();
            let b = crate::smart::rand(s.seed, i as u32 * 3 + 2);
            let mut p = t[0] * (1.0 - a) + t[1] * (a * (1.0 - b)) + t[2] * (a * b);
            if ((p - region.center) / region.radii).length() > 1.0 {
                // Keep the sample on its triangle while clipping it to the
                // requested ellipsoid; its center is already known inside.
                let center = (t[0] + t[1] + t[2]) / 3.0;
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..24 {
                    let m = (lo + hi) * 0.5;
                    if ((center.lerp(p, m) - region.center) / region.radii).length() <= 1.0 {
                        lo = m;
                    } else {
                        hi = m;
                    }
                }
                p = center.lerp(p, lo * 0.999);
            }
            points.push(p);
        }
    } else if s.count > 1 {
        return Err("random count needs a region {center,radii}".into());
    }
    if let Some(hinge) = s.hinge {
        if !hinge.iter().all(|p| p.is_finite()) || hinge[0].distance(hinge[1]) < 0.0001 {
            return Err("hinge must have two distinct finite points".into());
        }
        if points.is_empty() {
            points.push((hinge[0] + hinge[1]) * 0.5);
        }
    }
    if points.is_empty() || points.iter().any(|p| !p.is_finite()) {
        return Err("damage needs finite point/points or region".into());
    }
    let points: Vec<Vec3> = points
        .into_iter()
        .map(|p| input.transform_point3(p))
        .collect();
    let normal = s.normal.map(|n| {
        input
            .inverse()
            .transpose()
            .transform_vector3(n)
            .normalize_or(Vec3::Z)
    });
    let base = |p: Vec3, seed: u32| -> Result<Decal, String> {
        let (q, n) = snap(mesh, p).ok_or("damage target has no triangles")?;
        let mut d = Decal {
            shape: s.kind,
            center: q.to_array(),
            normal: normal.unwrap_or(n).to_array(),
            size,
            seed,
            color: s.color,
            softness: s.softness,
            rotate: s.rotate,
            aspect: if s.kind == Shape::BulletHit { 1.0 / s.impact_angle.to_radians().cos().max(0.17) } else { 1.0 },
            depth: s.depth.map(|d| d * scale),
            melted_lip: s.melted_lip,
            ..Default::default()
        };
        if let Some(hinge) = s.hinge {
            d.hinge = Some(hinge.map(|p| input.transform_point3(p).to_array()));
            d.angle = s.angle;
        } else if matches!(s.kind, Shape::Bend | Shape::Tear) {
            let (_, v) = d.frame();
            let axis = v.normalize();
            d.hinge = Some([
                (q - axis * size * 0.5).to_array(),
                (q + axis * size * 0.5).to_array(),
            ]);
            d.angle = s.angle;
        }
        d.sanitize();
        Ok(d)
    };
    let mut out = Vec::new();
    let segment = |a: Vec3, b: Vec3, seed: u32| -> Result<Decal, String> {
        let mut d = base((a + b) * 0.5, seed)?;
        let n = Vec3::from(d.normal);
        let axis = (b - a).normalize_or(Vec3::Y);
        let side = if n.y.abs() > 0.95 {
            Vec3::X
        } else {
            Vec3::Y.cross(n).normalize()
        };
        let up = n.cross(side);
        // v points uphill for leaks, and follows the drawn path for scrapes.
        d.rotate = (-axis.dot(side)).atan2(axis.dot(up)).to_degrees();
        d.aspect = (a.distance(b) / size).max(0.1);
        d.sanitize();
        Ok(d)
    };
    if matches!(s.kind, Shape::Gouge | Shape::Scrape) && points.len() > 1 {
        for pair in points.windows(2) {
            let a = snap(mesh, pair[0]).unwrap().0;
            let b = snap(mesh, pair[1]).unwrap().0;
            out.push(segment(a, b, s.seed)?);
        }
    } else {
        for (i, p) in points.into_iter().enumerate() {
            let d = base(p, s.seed.wrapping_add(i as u32))?;
            if matches!(
                s.kind,
                Shape::RustStreak | Shape::OilLeak | Shape::WaterStain
            ) {
                let path = downhill(
                    mesh,
                    world,
                    Vec3::from(d.center),
                    s.length
                        * if local {
                            world.to_scale_rotation_translation().0.abs().element_sum() / 3.0
                        } else {
                            1.0
                        },
                    size * 2.0,
                );
                if path.len() < 2 {
                    return Err(
                        "leak start has no downhill surface; choose a vertical/sloping surface"
                            .into(),
                    );
                }
                for (j, pair) in path.windows(2).enumerate() {
                    let mut seg = segment(pair[1], pair[0], d.seed)?;
                    seg.flow = Some([
                        j as f32 / (path.len() - 1) as f32,
                        (j + 1) as f32 / (path.len() - 1) as f32,
                    ]);
                    // The global profile fades continuously; per-segment opacity
                    // steps create horizontal bands at segment boundaries.
                    seg.opacity = 1.0;
                    out.push(seg);
                }
            } else {
                out.push(d);
            }
        }
    }
    if out.len() > crate::smart::MAX_DECALS {
        return Err("damage makes over 64 segments; shorten paths or split the targets".into());
    }
    Ok(out)
}

/// A rigid rotation about the hinge inside the selected plate region. The
/// falloff only blends the boundary; the interior receives the full angle.
pub fn fold_point(d: &Decal, p: Vec3, inverse: bool) -> Vec3 {
    let Some([a, b]) = d.hinge.map(|h| h.map(Vec3::from)) else {
        return p;
    };
    if !matches!(d.shape, Shape::Bend | Shape::Tear) {
        return p;
    }
    let axis = (b - a).normalize_or(Vec3::Y);
    let n = Vec3::from(d.normal);
    let across = n.cross(axis);
    let signed = (p - a).dot(across);
    let near = (p - a).dot(axis);
    let length = a.distance(b);
    let along = 1.0 - smooth(0.0, d.size * 0.2, (-near).max(near - length));
    // Inverse selection includes the folded side of the hinge plane.
    let forward_side = if inverse {
        signed / (d.angle.to_radians().cos().max(0.08))
    } else {
        signed
    };
    let weight = along
        * smooth(0.0, d.size * 0.08, forward_side)
        * (1.0 - smooth(d.size * 0.38, d.size * 0.5, forward_side));
    let sign = if inverse { -1.0 } else { 1.0 };
    a + glam::Quat::from_axis_angle(axis, d.angle.to_radians() * weight * sign) * (p - a)
}
pub fn source_point(list: &[Decal], p: Vec3) -> Vec3 {
    list.iter().rev().fold(p, |p, d| {
        let p = fold_point(d, p, true);
        p - Vec3::from(d.normal) * height_at(std::slice::from_ref(d), p)
    })
}
/// Signed distance of a narrow jagged through-cut at a torn hinge seam.
pub fn tear_cut(list: &[Decal], p: Vec3) -> f32 {
    list.iter()
        .filter(|d| d.shape == Shape::Tear)
        .map(|d| {
            let rel = p - Vec3::from(d.center);
            let (u, v) = d.frame();
            let uv = Vec2::new(rel.dot(u), rel.dot(v));
            let phase = crate::smart::rand(d.seed, 0) * std::f32::consts::TAU;
            let seam =
                (uv.x - 0.014 * (uv.y * 91.0 + phase).sin() - 0.009 * (uv.y * 157.0 - phase).sin())
                    .abs();
            ((seam - 0.012) * d.size)
                .max((uv.y.abs() - 0.4) * d.size * d.aspect)
                .max(rel.dot(Vec3::from(d.normal)).abs() - depth(d).abs() * 2.0)
        })
        .fold(f32::INFINITY, f32::min)
}

pub type ExportGroup = (usize, Vec<u32>, Option<[f32; 2]>);
pub fn export_groups(mesh: &Mesh) -> Option<Vec<ExportGroup>> {
    if mesh.textures.is_some()
        || (mesh.decals.is_empty()
            && !mesh
                .materials
                .iter()
                .any(|m| m.layers.iter().any(|l| l.name.starts_with("wear:"))))
    {
        return None;
    }
    let mut groups = std::collections::BTreeMap::<(usize, u8, u8), Vec<u32>>::new();
    for t in mesh.indices.chunks_exact(3) {
        let material = crate::gltf::triangle_material(mesh, t);
        let rough = t
            .iter()
            .map(|i| mesh.vertices[*i as usize].surface[0])
            .sum::<f32>()
            / 3.0;
        let metal = t
            .iter()
            .map(|i| mesh.vertices[*i as usize].surface[1])
            .sum::<f32>()
            / 3.0;
        let key = (
            material,
            (rough.clamp(0.0, 1.0) * 8.0).round() as u8,
            (metal.clamp(0.0, 1.0) * 4.0).round() as u8,
        );
        groups.entry(key).or_default().extend_from_slice(t);
    }
    Some(
        groups
            .into_iter()
            .map(|((m, r, b), t)| (m, t, Some([r as f32 / 8.0, b as f32 / 4.0])))
            .collect(),
    )
}

pub fn own_primitive(o: &crate::scene::Object) -> bool {
    // Material names no longer encode ownership: shared/custom finishes also
    // need their masks evaluated on a refined mesh rather than the stock cube.
    !o.damage.is_empty() || o.material.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clay::Clay,
        smart::{At, Look},
    };
    fn panel() -> Clay {
        serde_json::from_value(serde_json::json!({"resolution":96,"blobs":[{"type":"box","name":"plate","center":[0,0,0],"size":[0.6,0.6,0.08],"round":0.008,"blend":0,"color":[0.23,0.28,0.14]}]})).unwrap()
    }
    #[test]
    fn fabric_and_leather_never_receive_steel_paint_chips() {
        for name in ["cloth", "grimy_cloth", "leather", "skin", "rubber"] {
            let mut scene=crate::scene::Scene::default();
            let id=scene.add(crate::scene::Shape::Cube);
            scene.get_mut(id).unwrap().material=Some(name.into());
            apply_wear(&mut scene,id,"battle_worn",1.0).unwrap();
            let m=scene.material_of(scene.get(id).unwrap());
            assert!(m.layers.iter().all(|l|!l.name.ends_with("edge chips")),"{name}");
        }
    }

    #[test]
    fn legacy_copies_reconnect_to_their_base_on_reapplication() {
        let mut scene=crate::scene::Scene::default();
        scene.materials.push(Material{name:"paint".into(),roughness:0.8,..Default::default()});
        scene.materials.push(Material{name:"damage_1_0_paint".into(),roughness:0.2,..Default::default()});
        let id=scene.add(crate::scene::Shape::Cube);
        scene.get_mut(id).unwrap().material=Some("damage_1_0_paint".into());
        apply_wear(&mut scene,id,"battle_worn",0.3).unwrap();
        assert_eq!(scene.get(id).unwrap().material.as_deref(),Some("paint"));
        assert_eq!(scene.materials.len(),2);
        assert_eq!(scene.material_of(scene.get(id).unwrap()).roughness,0.8);
    }

    #[test]
    fn named_preset_can_be_replaced_and_cleared() {
        let mut scene=crate::scene::Scene::default();
        let id=scene.add(crate::scene::Shape::Cube);
        scene.get_mut(id).unwrap().material=Some("battle_worn".into());
        apply_wear(&mut scene,id,"battle_worn",0.2).unwrap();
        assert_eq!(scene.material_of(scene.get(id).unwrap()).layers.len(),7);
        apply_wear(&mut scene,id,"factory_new",1.0).unwrap();
        assert!(scene.material_of(scene.get(id).unwrap()).layers.is_empty());
    }

    #[test]
    fn paint_grain_keeps_meter_scale_on_resized_assets() {
        let m=Material{name:"military_paint".into(),layers:wear_layers("military_paint",1.0).unwrap(),..Default::default()};
        let scaled=world_material(m.clone(),Mat4::from_scale(Vec3::splat(6.5)));
        assert!((scaled.layers[0].detail_scale*6.5-m.layers[0].detail_scale).abs()<1e-6);
    }

    #[test]
    fn edge_flakes_keep_millimetre_scale_on_resized_assets() {
        let m = Material { name: "battle_worn".into(),
            layers: wear_layers("battle_worn", 1.0).unwrap(), ..Default::default() };
        let scaled = world_material(m.clone(), Mat4::from_scale(Vec3::splat(6.5)));
        for i in 0..128 {
            let p = Vec3::new(i as f32 * 0.0013, 0.35, 0.12);
            let original = At { p, n: Vec3::Z, edge: 0.9, ..Default::default() };
            let enlarged = At { p: p / 6.5, ..original };
            for (a, b) in m.layers.iter().zip(&scaled.layers)
                .filter(|(a, _)| a.name == "edge chips" || a.name == "rubbed paint halo") {
                let expected = crate::smart::mask(&m, a, &original);
                let actual = crate::smart::mask(&scaled, b, &enlarged);
                assert!((actual - expected).abs() < 0.001,
                    "scaled flake coverage: {actual} != {expected}");
            }
        }
    }

    #[test]
    fn paint_amount_is_monotonic_and_deterministic() {
        for preset in ["military_paint", "battle_worn", "desert_dusty", "muddy", "burnt_wreck"] {
            let mut previous = vec![0.0; wear_layers(preset, 0.0).unwrap().len()];
            for amount in [0.0, 0.08, 0.22, 0.3, 0.55, 0.7, 1.0] {
                let layers = wear_layers(preset, amount).unwrap();
                assert_eq!(layers, wear_layers(preset, amount).unwrap());
                let m = Material {layers:layers.clone(), ..Default::default()};
                for (j, l) in layers.iter().enumerate() {
                    let coverage:f32 = (0..500).map(|i|crate::smart::mask(&m,l,&At {
                        p:Vec3::new(i as f32 * 0.003,0.08,0.12), n:Vec3::Y,
                        edge:0.8,cavity:0.3,..Default::default()
                    }) * l.opacity).sum();
                    assert!(coverage + 0.0001 >= previous[j], "{preset} {amount} {}",l.name);
                    if amount == 0.0 {assert_eq!(coverage,0.0);}
                    previous[j] = coverage;
                }
            }
            assert!(previous.iter().any(|&c|c>0.0));
        }
    }

    #[test]
    fn wear_keeps_shared_material_and_base_edits() {
        let mut scene = crate::scene::Scene::default();
        scene.materials.push(Material {name:"paint".into(),..Default::default()});
        let a=scene.add(crate::scene::Shape::Cube);
        let b=scene.add(crate::scene::Shape::Cube);
        for id in [a,b] {scene.get_mut(id).unwrap().material=Some("paint".into());}
        apply_wear(&mut scene,a,"battle_worn",0.55).unwrap();
        let heavy=scene.materials[0].layers.clone();
        apply_wear(&mut scene,a,"battle_worn",0.08).unwrap();
        assert_eq!(scene.materials.len(),1);
        assert_eq!(heavy.len(),scene.materials[0].layers.len());
        assert_ne!(heavy,scene.materials[0].layers);
        scene.materials[0].roughness=0.91;
        for id in [a,b] {assert_eq!(scene.material_of(scene.get(id).unwrap()).roughness,0.91);}
    }

    #[test]
    fn target_stamps_choose_only_nearest_surface() {
        let mesh=crate::mesh::Mesh::build(crate::scene::Shape::Cube);
        for (x,expected) in [(0.0,1),(3.0,2)] {
            let targets=[(1,&mesh,Mat4::IDENTITY),(2,&mesh,Mat4::from_translation(Vec3::X*3.0))];
            assert_eq!(stamp_owner(Vec3::new(x,0.0,0.51),targets.into_iter()),Some(expected));
        }
    }

    #[test]
    fn damage_is_deterministic_changes_geometry_and_pbr_and_roundtrips() {
        let mut c = panel();
        let before = c.build_mesh();
        let req:Stamp=serde_json::from_value(serde_json::json!({"kind":"bullet_hit","point":[0,0,0.04],"normal":[0,0,1],"size":0.13,"seed":82})).unwrap();
        let a = stamps(&before, Mat4::IDENTITY, true, &req).unwrap();
        let b = stamps(&before, Mat4::IDENTITY, true, &req).unwrap();
        assert_eq!(a, b);
        c.decals = a.clone();
        let mesh = c.build_mesh();
        let center_z = mesh
            .vertices
            .iter()
            .filter(|v| v.pos[0].hypot(v.pos[1]) < 0.014 && v.pos[2] > 0.0)
            .map(|v| v.pos[2])
            .fold(1.0, f32::min);
        assert!(center_z < 0.03, "crater center {center_z}");
        let m = Material::default();
        let at = At {
            p: Vec3::new(0.024, 0.0, 0.035),
            n: Vec3::Z,
            ..Default::default()
        };
        let look = crate::smart::decals(
            &a,
            &at,
            Look::of(&m, Vec3::new(0.23, 0.28, 0.14), 0.85, Vec3::Z),
        );
        assert!(look.metal > 0.8 && look.rough < 0.5 && look.color.min_element() > 0.35, "{look:?}");
        let back: Clay = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.decals, c.decals);
        assert_eq!(mesh.indices, back.build_mesh().indices);
    }
    #[test]
    fn leaks_go_downhill_on_rotated_scaled_world_surface() {
        let mesh = panel().build_mesh();
        let world = Mat4::from_scale_rotation_translation(
            Vec3::new(1.3, 0.7, 1.0),
            glam::Quat::from_rotation_z(0.7),
            Vec3::new(0.0, 2.0, 0.0),
        );
        let points = downhill(&mesh, world, Vec3::new(0.1, 0.2, 0.04), 0.2, 0.02);
        assert!(points.len() > 4);
        for p in points.windows(2) {
            assert!(world.transform_point3(p[1]).y < world.transform_point3(p[0]).y);
            assert!((p[1].z - 0.04).abs() < 0.005);
        }
    }
    #[test]
    fn bolt_leaks_remain_on_the_front_and_reach_down_the_plate() {
        let mut c = panel();
        c.blobs.push(serde_json::from_value(serde_json::json!({"type":"sphere","center":[-0.12,0.22,0.045],"radius":0.016,"blend":0})).unwrap());
        let mesh = c.build_mesh();
        let start = Vec3::new(-0.12, 0.208, 0.06);
        let path = downhill(&mesh, Mat4::IDENTITY, start, 0.29, 0.14);
        assert!(path.last().unwrap().y < start.y - 0.20, "{path:?}");
        assert!(path.iter().all(|p| p.z > 0.035), "must stay on the front");
    }

    #[test]
    fn random_batches_have_deterministic_area_sampling() {
        let mesh = panel().build_mesh();
        let req:Stamp=serde_json::from_value(serde_json::json!({"kind":"dent","count":6,"seed":15,"region":{"center":[0,0,0.04],"radii":[0.2,0.2,0.02]}})).unwrap();
        let a = stamps(&mesh, Mat4::IDENTITY, true, &req).unwrap();
        assert_eq!(a, stamps(&mesh, Mat4::IDENTITY, true, &req).unwrap());
        assert_eq!(a.len(), 6);
        let r = req.region.as_ref().unwrap();
        assert!(
            a.iter()
                .all(|d| ((Vec3::from(d.center) - r.center) / r.radii).length() <= 1.001)
        );
        assert_ne!(a[0].center, a[1].center);
    }
    #[test]
    fn presets_preserve_base_and_world_height_follows_object_transform() {
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Clay);
        scene.get_mut(id).unwrap().clay = Some(panel());
        let clean = scene.get(id).unwrap().clay.as_ref().unwrap().build_mesh();
        assert!(clean.decals.is_empty());
        assert!(clean.vertices.iter().all(|v| v.surface[1] == 0.0));
        for name in ["battle_worn", "desert_dusty", "muddy", "burnt_wreck"] {
            assert!(!wear_layers(name, 0.7).unwrap().is_empty());
        }
        apply_wear(&mut scene, id, "battle_worn", 0.7).unwrap();
        apply_wear(&mut scene, id, "desert_dusty", 0.4).unwrap();
        let m = scene.material_of(scene.get(id).unwrap());
        assert_eq!(m.layers.len(), 11);
        apply_wear(&mut scene, id, "battle_worn", 0.2).unwrap();
        assert_eq!(scene.material_of(scene.get(id).unwrap()).layers.len(), 11);
        apply_wear(&mut scene, id, "factory_new", 1.0).unwrap();
        assert!(scene.material_of(scene.get(id).unwrap()).layers.is_empty());
        let mut m = Material {
            layers: wear_layers("muddy", 1.0).unwrap(),
            ..Material::default()
        };
        let world = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        m = world_material(m, world);
        let term = m.layers[0].mask.iter().find(|t| t.source == Source::HeightPlane).unwrap();
        let low = crate::smart::term(
            term,
            &At {
                p: Vec3::new(0.1, 4.0, 0.0),
                ..Default::default()
            },
        );
        let high = crate::smart::term(
            term,
            &At {
                p: Vec3::new(1.1, 4.0, 0.0),
                ..Default::default()
            },
        );
        assert!(low > high + 0.5);
        let off = wear_layers("battle_worn", 0.0).unwrap();
        assert!(off.iter().all(|l| l.opacity == 0.0));
    }
    #[test]
    fn damage_bakes_color_normal_and_orm_and_survives_glb() {
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Clay);
        let mut c = panel();
        c.resolution = 64;
        c.decals = vec![
            Decal {
                shape: Shape::BulletHit,
                center: [0.0, 0.0, 0.04],
                normal: [0.0, 0.0, 1.0],
                size: 0.15,
                seed: 19,
                ..Default::default()
            },
            Decal {
                shape: Shape::OilLeak,
                center: [0.15, 0.0, 0.04],
                normal: [0.0, 0.0, 1.0],
                size: 0.08,
                aspect: 3.0,
                ..Default::default()
            },
        ];
        scene.get_mut(id).unwrap().clay = Some(c);
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.sync(&scene);
        let settings = crate::bake::Settings {
            triangles: 1000,
            texture: 512,
            ..Default::default()
        };
        scene.get_mut(id).unwrap().bake = Some(settings);
        meshes.bake(&scene, id, settings).unwrap();
        let textures = meshes.baked(id).unwrap().0.textures.as_ref().unwrap();
        assert!(
            textures.orm.chunks_exact(4).filter(|c| c[2] > 200).count() > 100,
            "bare steel in ORM"
        );
        assert!(
            textures.orm.chunks_exact(4).filter(|c| c[1] < 60).count() > 100,
            "glossy oil in ORM"
        );
        assert!(
            textures
                .normal
                .chunks_exact(4)
                .any(|c| c[0].abs_diff(128) > 15 || c[1].abs_diff(128) > 15),
            "crater normal map"
        );
        assert!(
            textures
                .color
                .chunks_exact(4)
                .any(|c| c[0] > 120 && c[1] > 120),
            "steel color"
        );
        let out = crate::gltf::export(
            &scene,
            &meshes,
            &crate::gltf::Options {
                roots: vec![],
                animations: false,
                fps: 24.0,
                file_stem: String::new(),
                lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new(),
            },
        )
        .unwrap();
        let loaded = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
        let decoded = loaded.items[0].mesh.textures.as_ref().unwrap();
        assert_eq!(decoded.color, textures.color);
        assert_eq!(decoded.orm, textures.orm);
        assert!(
            decoded
                .normal
                .chunks_exact(4)
                .zip(textures.normal.chunks_exact(4))
                .all(|(a, b)| (0..3).all(|i| a[i].abs_diff(b[i]) <= 1))
        );
    }

    #[test]
    fn small_caliber_refines_only_its_patch_on_coarse_clay() {
        let mut c = panel();
        c.resolution = 16;
        let before = c.build_mesh();
        let req:Stamp=serde_json::from_value(serde_json::json!({"kind":"bullet_hit","point":[0,0,0.04],"normal":[0,0,1],"caliber_mm":5.56})).unwrap();
        c.decals = stamps(&before, Mat4::IDENTITY, true, &req).unwrap();
        let after = c.build_mesh();
        assert!(after.vertices.len() > before.vertices.len());
        assert!(after.vertices.len() < before.vertices.len() + 10_000);
        assert!(
            after
                .vertices
                .iter()
                .any(|v| v.pos[0].hypot(v.pos[1]) < 0.004 && v.pos[2] > 0.0 && v.pos[2] < 0.038),
            "small physical crater"
        );
    }

    #[test]
    fn world_normals_are_exact_for_scaled_rotated_bone_attachments() {
        use crate::rig::{Bone, Clip, Key, Rig};
        let mut scene = crate::scene::Scene::default();
        let body = scene.add(crate::scene::Shape::Clay);
        let prop = scene.add(crate::scene::Shape::Cube);
        let mut clip = Clip::new("turn");
        let mut key = Key::new(1.0);
        key.rotate = Vec3::new(0.0, 0.0, 70.0);
        clip.track_mut("arm").insert(key);
        scene.get_mut(body).unwrap().rig = Some(Rig {
            bones: vec![Bone {
                name: "arm".into(),
                parent: None,
                head: Vec3::ZERO,
                tail: Vec3::X,
                deform: true,
            }],
            clips: vec![clip],
            active: Some("turn".into()),
            split: true,
            weights: vec![],
        });
        let o = scene.get_mut(prop).unwrap();
        o.parent = Some(body);
        o.bone = Some("arm".into());
        o.scale = Vec3::new(1.7, 0.6, 1.3);
        scene.frame = Some(1.0);
        let world = scene.world_matrix(prop);
        let m = world_material(
            Material {
                layers: wear_layers("desert_dusty", 1.0).unwrap(),
                ..Default::default()
            },
            world,
        );
        let t = &m.layers[1].mask[0];
        let n = Vec3::new(0.3, 0.8, 0.4).normalize();
        let expected = world
            .inverse()
            .transpose()
            .transform_vector3(n)
            .normalize()
            .y
            .max(0.0);
        let actual = crate::smart::term(
            t,
            &At {
                n,
                ..Default::default()
            },
        );
        assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
    }
    #[test]
    fn wear_respects_clean_faces_gravity_and_retains_primer_rims() {
        let m = world_material(Material {
            layers: wear_layers("desert_dusty", 1.0).unwrap(),
            ..Default::default()
        }, Mat4::IDENTITY);
        let look = crate::smart::Look::of(&m, Vec3::new(0.35, 0.29, 0.17), 0.5, Vec3::Z);
        let at = At { p: Vec3::new(0.13, 5.0, 0.31), n: Vec3::Z, ..Default::default() };
        let face = crate::smart::layers(&m, &at, look);
        let top = crate::smart::layers(&m, &At { n: Vec3::Y, ..at }, look);
        let low = crate::smart::layers(&m, &At { p: Vec3::new(0.13, 0.1, 0.31), n: Vec3::Y, ..at }, look);
        assert!((face.color - look.color).length() < 0.015, "vertical paint retains its base between deposits");
        assert!(top.rough > face.rough && low.rough > face.rough);
        let m = Material { layers: wear_layers("battle_worn", 1.0).unwrap(), ..Default::default() };
        let (primer, steel) = (&m.layers[3], &m.layers[4]);
        let mut rim = 0;
        let mut chipped = 0;
        for i in 0..2000 {
            let at = At { p: Vec3::new(i as f32 * 0.001, 0.35, 0.12),
                edge: 0.8, n: Vec3::Z, ..Default::default() };
            let p = crate::smart::mask(&m, primer, &at);
            let s = crate::smart::mask(&m, steel, &at);
            assert!(p >= s);
            rim += usize::from(p > 0.8 && s < 0.2);
            chipped += usize::from(s > 0.8);
        }
        assert!(rim > 0 && chipped > 0 && chipped < 1800);
        let uv = Vec2::new((0.075_f32 * 14.0 + crate::smart::rand(31, 0) * std::f32::consts::TAU).sin() * 0.018, 0.425);
        let oil = sample(12, 31, uv);
        assert!(oil.coverage > 0.7 && oil.coverage < 0.85 && oil.rough < 0.15
            && oil.color.x > oil.color.y && oil.color.y > oil.color.z);
    }

    #[test]
    fn mud_stays_low_and_tread_dust_is_restrained() {
        let m = world_material(Material {
            layers: wear_layers("muddy", 1.0).unwrap(), ..Default::default()
        }, Mat4::IDENTITY);
        let at = At { p: Vec3::new(0.2, 3.0, 0.1), n: Vec3::Z, cavity: 1.0, ..Default::default() };
        let base = crate::smart::Look::of(&m, Vec3::splat(0.12), 0.5, at.n);
        let high = crate::smart::layers(&m, &at, base);
        assert_eq!(crate::smart::mask(&m, &m.layers[0], &at), 0.0, "mud must not reach high cavities");
        assert!((high.color-base.color).length()<0.001, "only fine grain above the mud line");
        let low = crate::smart::layers(&m, &At { p: Vec3::new(0.2, 0.05, 0.1), ..at }, base);
        let top = crate::smart::layers(&m, &At { p: Vec3::new(0.2, 0.05, 0.1), n: Vec3::Y, ..at }, base);
        assert!(low.color.max_element() > 0.25 && low.rough < 0.6);
        assert!(top.color.max_element() > low.color.max_element() && top.rough > low.rough);
    }

    #[test]
    fn desert_wear_preserves_dark_metal_rubber_and_optics() {
        for (name, metallic) in [("gunmetal", 0.85), ("track_rubber", 0.0)] {
            let mut scene = crate::scene::Scene::default();
            scene.materials.push(Material { name: name.into(), metallic, ..Default::default() });
            let id = scene.add(crate::scene::Shape::Cube);
            scene.get_mut(id).unwrap().material = Some(name.into());
            apply_wear(&mut scene, id, "military_paint", 1.0).unwrap();
            apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
            apply_wear(&mut scene, id, "desert_dusty", 1.0).unwrap();
            let m = world_material(scene.material_of(scene.get(id).unwrap()), Mat4::IDENTITY);
            let at = At { p: Vec3::new(0.2, 4.0, 0.1), n: Vec3::Z, ..Default::default() };
            let base = crate::smart::Look::of(&m, Vec3::splat(0.12), 0.5, at.n);
            let face = crate::smart::layers(&m, &at, base);
            let top = crate::smart::layers(&m, &At { n: Vec3::Y, ..at }, base);
            assert!(face.color.max_element() < 0.14, "{name}: {face:?}");
            assert!(top.color.max_element() < 0.21 && top.color.min_element() > face.color.min_element());
            assert!((face.metal - metallic).abs() < 0.001);
            assert!(top.metal >= metallic * 0.85);
            assert!(m.layers.iter().all(|l| l.name != "wear:battle_worn:edge chips"));
        }
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Cube);
        scene.materials.push(Material { name: "optic".into(), transmission: 0.9, ..Default::default() });
        scene.get_mut(id).unwrap().material = Some("optic".into());
        apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
        apply_wear(&mut scene, id, "desert_dusty", 1.0).unwrap();
        assert_eq!(scene.get(id).unwrap().material.as_deref(), Some("optic"));
    }

    #[test]
    fn generated_runoff_is_sparse_repeatable_and_replaceable() {
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Clay);
        let mut c = panel();
        c.blobs.push(serde_json::from_value(serde_json::json!({"type":"sphere","center":[-0.12,0.22,0.045],"radius":0.016,"blend":0})).unwrap());
        c.decals.push(Decal { shape: Shape::Emblem, ..Default::default() });
        scene.get_mut(id).unwrap().clay = Some(c);
        apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
        let a = scene.get(id).unwrap().clay.as_ref().unwrap().decals.clone();
        assert!(a.len() > 1 && a.len() <= 13);
        assert!(a[1..].iter().all(|d| d.shape == Shape::WaterStain && d.opacity <= 0.50));
        apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
        assert_eq!(a, scene.get(id).unwrap().clay.as_ref().unwrap().decals);
        let saved = serde_json::to_vec(&scene).unwrap();
        let restored: crate::scene::Scene = serde_json::from_slice(&saved).unwrap();
        assert_eq!(restored.get(id).unwrap().clay.as_ref().unwrap().decals, a);
        apply_wear(&mut scene, id, "factory_new", 1.0).unwrap();
        let decals = &scene.get(id).unwrap().clay.as_ref().unwrap().decals;
        assert_eq!(decals.len(), 1);
        assert_eq!(decals[0].shape, Shape::Emblem);
    }

    #[test]
    fn unbaked_wear_keeps_flat_panel_centers_and_exports_pbr_groups() {
        let mut scene = crate::scene::Scene::default();
        let id = scene.add(crate::scene::Shape::Cube);
        apply_wear(&mut scene, id, "battle_worn", 1.0).unwrap();
        let mut lib = crate::mesh::MeshLib::new();
        lib.sync(&scene);
        let mesh = lib.get(scene.get(id).unwrap()).unwrap();
        assert!(mesh.vertices.len() > 100);
        assert!(mesh.vertices.iter().any(|v| v.pos[0].abs() < 0.06
            && v.pos[1].abs() < 0.06
            && v.pos[2] > 0.49
            && v.surface[1] < 0.1));
        assert!(
            mesh.vertices.iter().any(|v| v.surface[1] > 0.95),
            "max metal {} edge {}",
            mesh.vertices
                .iter()
                .map(|v| v.surface[1])
                .fold(0.0, f32::max),
            mesh.vertices.iter().map(|v| v.wear[0]).fold(0.0, f32::max)
        );
        let groups = export_groups(mesh).unwrap();
        assert!(groups.iter().any(|g| g.2.unwrap()[1] > 0.75));
        assert!(groups.iter().any(|g| g.2.unwrap()[1] < 0.25));
        let out = crate::gltf::export(
            &scene,
            &lib,
            &crate::gltf::Options {
                roots: vec![],
                animations: false,
                fps: 24.0,
                file_stem: String::new(),
                lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new(),
            },
        )
        .unwrap();
        let loaded = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
        assert!(loaded.items.iter().any(|i| i.material.metallic > 0.75));
    }

    #[test]
    fn fold_rotates_plate_about_hinge_and_mesh_stamps_refine_geometry() {
        let d = Decal {
            shape: Shape::Bend,
            normal: [0.0, 0.0, 1.0],
            size: 1.0,
            hinge: Some([[0.0, -0.5, 0.0], [0.0, 0.5, 0.0]]),
            angle: 45.0,
            ..Default::default()
        };
        // n cross axis = -X: selected half of the plate.
        let p = Vec3::new(-0.2, 0.0, 0.0);
        let q = fold_point(&d, p, false);
        assert!((p.length() - q.length()).abs() < 1e-5);
        assert!(q.z > 0.1);
        assert!(source_point(std::slice::from_ref(&d), q).distance(p) < 1e-5);
        let mut mesh = crate::mesh::Mesh::all()
            .into_iter()
            .find(|(s, _)| *s == crate::scene::Shape::Cube)
            .unwrap()
            .1;
        mesh.materials = vec![Material::default()];
        for v in &mut mesh.vertices {
            v.surface = mesh.materials[0].surface();
        }
        let original = mesh.vertices.len();
        let hit = Decal {
            shape: Shape::BulletHit,
            center: [0.0, 0.0, 0.5],
            normal: [0.0, 0.0, 1.0],
            size: 0.15,
            ..Default::default()
        };
        displace_mesh(&mut mesh, &[hit]);
        assert!(mesh.vertices.len() > original);
        assert!(mesh.vertices.iter().any(|v| v.surface[1] > 0.7));
    }
}
