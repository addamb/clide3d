//! Field-aligned quad retopology (docs/QUALITY_PLAN.md Stage 3b, D15):
//! Instant Meshes (Jakob, Tarini, Panozzo and Sorkine-Hornung 2015), made
//! deterministic, in-house.
//!
//! 1. The input (a clay's welded rest mesh) is made a well-shaped
//!    triangle mesh of about 8 vertices per output face (`remesh`).
//! 2. A multigrid hierarchy of its vertex graph (`hierarchy`).
//! 3. An orientation field (`field`), then a position field (`position`),
//!    each solved coarse to fine by multicolour Gauss-Seidel.
//! 4. Quads where the lattices meet (`extract`), cleaned and fitted to the
//!    surface (`cleanup`).

pub mod cleanup;
pub mod extract;
pub mod features;
pub mod field;
pub mod hierarchy;
pub mod position;
pub mod remesh;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::BTreeMap;

use glam::Vec3;
use serde::Serialize;

use crate::progress::Progress;

/// The surface a retopology fits: a signed field (negative inside) near
/// a point, exact within `reach` of it.
pub trait Surface: Sync {
    fn near(&self, p: Vec3, reach: f32) -> Box<dyn Fn(Vec3) -> f32 + '_>;
    /// The spacing of the samples the field is reconstructed from at `p`
    /// (a sculpt grid's cell), when it has one.
    fn cell(&self, _p: Vec3) -> Option<f32> {
        None
    }
}

impl Surface for crate::quality::Probe {
    fn near(&self, p: Vec3, reach: f32) -> Box<dyn Fn(Vec3) -> f32 + '_> {
        Box::new(crate::quality::Probe::near(self, p, reach))
    }
    fn cell(&self, p: Vec3) -> Option<f32> {
        Some(crate::quality::Probe::cell(self, p))
    }
}

/// Smoothing sweeps per level, for each field.
const ITERATIONS: usize = 8;
/// Remeshing rounds of the input.
const REMESH_ROUNDS: usize = 3;
/// Input points per output face (the plan's "about 8x the target vertex
/// count").
const INPUT_PER_FACE: usize = 8;
/// Most input points (the plan's 2M bound, checked from the arguments).
pub const MAX_INPUT: usize = 2_000_000;
/// The curvature pull's most weight, against the neighbours' sum.
const PULL: f32 = 0.5;
/// Output points within this share of a lattice step of a line go on it.
const SNAP: f32 = 0.35;
/// Input points within this share of a lattice step of the symmetry
/// plane are held to it (a guide's or border's band is at most this, a
/// fifth of its extent, and at least 1.5 input edges).
const BAND: f32 = 0.6;
/// Longest strip of quads two odd faces are paired across.
const STRIP: usize = 8;
/// Fit rounds of relaxation.
const FIT_ROUNDS: usize = 3;
/// Most points distances are measured at.
const DISTANCE_SAMPLES: usize = 20_000;

/// Target quads, adaptivity, densities and the rest (see `retopo`'s
/// schema in mcp.rs); bounded by the caller.
#[derive(Clone, Debug)]
pub struct Args {
    /// Target quads (after the final split when `pure_quads`).
    pub faces: usize,
    /// 0 uniform, 1 strongly curvature-adaptive.
    pub adaptive: f32,
    pub symmetry: Symmetry,
    /// Polylines the edge flow follows (in the surface's space).
    pub guides: Vec<Vec<Vec3>>,
    pub density: Vec<Density>,
    /// Dihedral angle (degrees) above which an input edge is a crease the
    /// edge flow follows (180: none).
    pub keep_creases: f32,
    pub keep_group_borders: bool,
    pub pure_quads: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args { faces: 8000, adaptive: 0.3, symmetry: Symmetry::Auto, guides: Vec::new(), density: Vec::new(), keep_creases: 40.0, keep_group_borders: true, pure_quads: true }
    }
}

/// Mirror symmetry in x = 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symmetry {
    /// When the surface is X-symmetric within a cell.
    Auto,
    X,
    Off,
}

/// A local edge-length multiplier: `scale` at `center`, fading to 1 at
/// `radius`.
#[derive(Clone, Copy, Debug)]
pub struct Density {
    pub center: Vec3,
    pub radius: f32,
    pub scale: f32,
}

/// The surface a retopology starts from: a closed, welded triangle mesh
/// on the field, the unit distances are reported in, and the polygroup
/// at a point (for `keep_group_borders`).
pub struct Input<'a> {
    pub points: &'a [Vec3],
    pub tris: &'a [[u32; 3]],
    pub surface: &'a dyn Surface,
    pub cell: f32,
    pub groups: Option<&'a (dyn Fn(Vec3) -> u16 + Sync)>,
}

/// What a retopology made, and how it measures.
pub struct Outcome {
    pub poly: crate::poly::Poly,
    /// The cleaned cage before the final split (`pure_quads`), and after.
    pub before_split: Shape,
    pub shape: Shape,
    /// The largest and mean distance from the cage (its points, edge
    /// middles and face middles) to the surface, in cells, over `samples`.
    pub hausdorff_cells: f32,
    pub mean_distance_cells: f32,
    /// The largest distance from the input surface's points to the cage.
    pub coverage_cells: f32,
    pub samples: usize,
    pub symmetric: bool,
    /// The largest distance between a point mirrored and its mirror point.
    pub symmetric_error: f32,
    /// The input after remeshing.
    pub input_points: usize,
    pub input_triangles: usize,
    pub crease_edges: usize,
    pub levels: usize,
    /// Orientation-field singularities on the input.
    pub singularities: usize,
    /// The lattice step the quads were extracted at (mean).
    pub spacing: f32,
    pub notes: Vec<String>,
    /// Seconds per step.
    pub timings: Vec<(&'static str, f32)>,
}

/// A part smaller than this many output faces' area is a crumb.
const CRUMB_FACES: f32 = 4.0;

/// The mesh without its parts of area under `least` (points compacted in
/// order), and (parts dropped, their area). The largest part always stays.
fn drop_crumbs(points: &[Vec3], tris: &[[u32; 3]], least: f32) -> (Vec<Vec3>, Vec<[u32; 3]>, (usize, f32)) {
    let roots = part_roots(points.len(), tris);
    let mut part_area: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
    for t in tris {
        let [a, b, c] = t.map(|i| points[i as usize]);
        *part_area.entry(roots[t[0] as usize]).or_default() += 0.5 * (b - a).cross(c - a).length() as f64;
    }
    let largest = part_area.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(r, _)| *r);
    let drop: std::collections::BTreeSet<u32> = part_area.iter().filter(|(r, a)| Some(**r) != largest && (**a as f32) < least).map(|(r, _)| *r).collect();
    if drop.is_empty() {
        return (points.to_vec(), tris.to_vec(), (0, 0.0));
    }
    let dropped = drop.iter().map(|r| part_area[r]).sum::<f64>() as f32;
    let keep: Vec<[u32; 3]> = tris.iter().filter(|t| !drop.contains(&roots[t[0] as usize])).copied().collect();
    let mut map = vec![u32::MAX; points.len()];
    let mut out = Vec::new();
    let tris = keep
        .iter()
        .map(|t| {
            t.map(|i| {
                if map[i as usize] == u32::MAX {
                    map[i as usize] = out.len() as u32;
                    out.push(points[i as usize]);
                }
                map[i as usize]
            })
        })
        .collect();
    (out, tris, (drop.len(), dropped))
}

/// Each point's part (the least point index in it).
fn part_roots(n: usize, tris: &[[u32; 3]]) -> Vec<u32> {
    let mut root: Vec<u32> = (0..n as u32).collect();
    fn find(r: &mut [u32], mut x: u32) -> u32 {
        while r[x as usize] != x {
            r[x as usize] = r[r[x as usize] as usize];
            x = r[x as usize];
        }
        x
    }
    for t in tris {
        for k in 1..3 {
            let (a, b) = (find(&mut root, t[0]), find(&mut root, t[k]));
            if a != b {
                root[a.max(b) as usize] = a.min(b);
            }
        }
    }
    (0..n as u32).map(|v| find(&mut root, v)).collect()
}

/// Connected parts of a triangle mesh (points no triangle uses aside).
fn tri_parts(n: usize, tris: &[[u32; 3]]) -> usize {
    let mut root: Vec<u32> = (0..n as u32).collect();
    fn find(r: &mut [u32], mut x: u32) -> u32 {
        while r[x as usize] != x {
            r[x as usize] = r[r[x as usize] as usize];
            x = r[x as usize];
        }
        x
    }
    for t in tris {
        for k in 1..3 {
            let (a, b) = (find(&mut root, t[0]), find(&mut root, t[k]));
            if a != b {
                root[a.max(b) as usize] = a.min(b);
            }
        }
    }
    let mut used = vec![false; n];
    for t in tris {
        for &v in t {
            used[v as usize] = true;
        }
    }
    (0..n as u32).filter(|&v| used[v as usize] && find(&mut root, v) == v).count()
}

/// Area-weighted vertex normals.
pub fn vertex_normals(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; points.len()];
    for t in tris {
        let c = (points[t[1] as usize] - points[t[0] as usize]).cross(points[t[2] as usize] - points[t[0] as usize]);
        for &v in t {
            n[v as usize] += c;
        }
    }
    n.iter().map(|v| v.normalize_or(Vec3::Z)).collect()
}

/// Each point's share of the area (a third of each triangle's).
fn point_areas(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<f32> {
    let mut a = vec![0.0f32; points.len()];
    for t in tris {
        let x = (points[t[1] as usize] - points[t[0] as usize]).cross(points[t[2] as usize] - points[t[0] as usize]).length() / 6.0;
        for &v in t {
            a[v as usize] += x;
        }
    }
    a
}

/// Total area of a triangle mesh.
pub fn area(points: &[Vec3], tris: &[[u32; 3]]) -> f64 {
    tris.iter().map(|t| (points[t[1] as usize] - points[t[0] as usize]).cross(points[t[2] as usize] - points[t[0] as usize]).length() as f64 * 0.5).sum()
}

/// Whether the surface is X-symmetric within a cell: the field at the
/// mirror image of up to 4,096 of its points, evenly picked, is within a
/// cell of zero.
pub fn is_symmetric(points: &[Vec3], surface: &dyn Surface, cell: f32) -> bool {
    let step = points.len().div_ceil(4096).max(1);
    points.iter().step_by(step).all(|&p| {
        let m = Vec3::new(-p.x, p.y, p.z);
        surface.near(m, 2.0 * cell)(m).abs() <= cell
    })
}

/// The input's edge length for a surface of `area` and `faces` asked for
/// (`INPUT_PER_FACE` points per face, equilateral triangles).
pub fn input_length(area: f32, faces: usize) -> f32 {
    (2.0 * area / (3f32.sqrt() * input_points(faces) as f32)).sqrt()
}

/// Most guide samples (points laid on the surface) one call may make.
pub const MAX_GUIDE_SAMPLES: f64 = 200_000.0;

/// The samples `features::Features::add_guides` would make for `guides`
/// at spacing `step` (as a float: a hostile length cannot overflow it).
pub fn guide_samples(guides: &[Vec<Vec3>], step: f32) -> f64 {
    guides.iter().flat_map(|g| g.windows(2)).map(|w| ((w[0].distance(w[1]) / step) as f64).ceil().max(1.0)).sum::<f64>() + guides.len() as f64
}

/// Whether the polygroups are X-symmetric: at up to 4,096 evenly picked
/// points, the group at the mirror image is the point's own.
pub fn groups_symmetric(points: &[Vec3], groups: &(dyn Fn(Vec3) -> u16 + Sync)) -> bool {
    let step = points.len().div_ceil(4096).max(1);
    points.iter().step_by(step).all(|&p| groups(Vec3::new(-p.x, p.y, p.z)) == groups(p))
}

/// The plan's input size for a request: points the input is remeshed to.
pub fn input_points(faces: usize) -> usize {
    faces.saturating_mul(INPUT_PER_FACE)
}

/// The edge-length multiplier at `p` from the density regions (a smooth
/// fade from `scale` at the middle to 1 at the radius, multiplied).
fn density_at(density: &[Density], p: Vec3) -> f32 {
    density
        .iter()
        .map(|d| {
            let t = (p.distance(d.center) / d.radius.max(1e-9)).min(1.0);
            let w = 1.0 - t * t * (3.0 - 2.0 * t);
            d.scale.powf(w)
        })
        .product()
}

/// Field-aligned quad retopology of `input` (see the module doc).
pub fn retopo(input: &Input, args: &Args, progress: &Progress) -> Result<Outcome, String> {
    let timer = std::time::Instant::now();
    let mut timings: Vec<(&'static str, f32)> = Vec::new();
    let lap = |name: &'static str, timings: &mut Vec<(&'static str, f32)>| timings.push((name, timer.elapsed().as_secs_f32()));
    let mut notes: Vec<String> = Vec::new();
    let (surface, cell) = (input.surface, input.cell);
    if input.tris.is_empty() {
        return Err("the surface has no triangles".into());
    }
    let vertices = input_points(args.faces);
    if vertices > MAX_INPUT {
        return Err(format!("{} faces would need {vertices} input points, over the {MAX_INPUT} bound", args.faces));
    }
    let extract_faces = if args.pure_quads { args.faces as f32 / 4.0 } else { args.faces as f32 };
    // Loose crumbs (parts too small for even a few output faces, as a
    // strong crease stroke can leave) are dropped: a lattice cannot hold
    // them, and they pulled faces across to them (Stage 3c's backlog).
    // The bound is in output faces (`args.faces`, after the final split),
    // not extracted ones (a quarter as many with `pure_quads`).
    let all = area(input.points, input.tris) as f32;
    let (points, tris, crumbs) = drop_crumbs(input.points, input.tris, CRUMB_FACES * all / (args.faces as f32).max(1.0));
    if crumbs.0 > 0 {
        notes.push(format!("dropped {} loose part{} smaller than {CRUMB_FACES} output faces ({:.3}% of the surface)", crumbs.0, if crumbs.0 == 1 { "" } else { "s" }, 100.0 * crumbs.1 / all.max(f32::MIN_POSITIVE)));
    }
    let total = area(&points, &tris) as f32;
    let h_mean = (total / extract_faces.max(1.0)).sqrt();
    let length = input_length(total, args.faces);
    // Guides are laid on the surface every input edge length: their total
    // is bounded before anything is built.
    let samples = guide_samples(&args.guides, length);
    if samples > MAX_GUIDE_SAMPLES {
        return Err(format!("the guides are {:.0} input edges long in all ({:.4} each), over the {MAX_GUIDE_SAMPLES} bound: send shorter guides, or guides on this clay's surface", samples, length));
    }
    progress.stage("remeshing the input", 0.02);
    let m = remesh::prepare(&points, &tris, vertices, length, args.keep_creases, surface, cell, REMESH_ROUNDS, progress)?;
    lap("remesh", &mut timings);
    progress.stage("finding features", 0.25);
    let n = m.points.len();
    let locks = m.locks();
    let normals = vertex_normals(&m.points, &m.tris);
    let symmetric = match args.symmetry {
        Symmetry::Off => false,
        Symmetry::X => true,
        Symmetry::Auto => is_symmetric(&m.points, surface, cell) && (!args.keep_group_borders || input.groups.is_none_or(|g| groups_symmetric(&m.points, g))),
    };
    let mut features = features::Features::default();
    features.add_creases(&m, &locks);
    if symmetric {
        features.add_plane(&m);
        features.add_plane_corners();
    }
    if args.keep_group_borders
        && let Some(groups) = input.groups
    {
        let labels: Vec<u16> = hierarchy::par_map_checked(n, progress, |i| groups(m.points[i]))?;
        features.add_borders(&m, &labels);
    }
    features.add_guides(&args.guides, length, surface, cell, progress)?;
    progress.check()?;
    // Curvature: the pull, and the adaptive spacing.
    let curv: Vec<Option<(f32, f32, Vec3)>> = hierarchy::par_map_checked(n, progress, |i| features::principal(surface, m.points[i], normals[i], 2.0 * cell))?;
    let mut s: Vec<f32> = (0..n)
        .map(|i| {
            let k = curv[i].map_or(0.0, |c| c.0.abs());
            (1.0 + args.adaptive * k * h_mean).powf(-0.5) * density_at(&args.density, m.points[i])
        })
        .collect();
    let mut ring: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in &m.tris {
        for k in 0..3 {
            ring[t[k] as usize].push(t[(k + 1) % 3]);
        }
    }
    for _ in 0..4 {
        s = (0..n).map(|i| (s[i] + ring[i].iter().map(|&j| s[j as usize]).sum::<f32>()) / (1 + ring[i].len()) as f32).collect();
    }
    let areas = point_areas(&m.points, &m.tris);
    let h0 = ((0..n).map(|i| areas[i] as f64 / (s[i] as f64 * s[i] as f64)).sum::<f64>() / extract_faces.max(1.0) as f64).sqrt() as f32;
    let scale: Vec<f32> = s.iter().map(|&x| h0 * x).collect();
    let h_max = scale.iter().copied().fold(0.0, f32::max);
    let band = 1.5 * length;
    let plane_band = band.max(BAND * h0);
    features.index(plane_band.max(SNAP * h_max * 1.2));
    features.add_crossings(0.5 * length, 0.5 * h0, progress)?;
    let pulls = features::pulls(&curv, h0, PULL);
    lap("features", &mut timings);
    progress.stage("solving the fields", 0.35);
    let hierarchy = hierarchy::Hierarchy::build(&m.points, &m.tris, &normals, progress)?;
    let start: Vec<Vec3> = (0..n).map(|i| pulls[i].map_or_else(|| normals[i].any_orthonormal_vector(), |p| p.0)).collect();
    let pulls = field::restrict_pulls(&hierarchy, pulls);
    let mut scales = vec![scale];
    for l in 0..hierarchy.levels.len() - 1 {
        let (fine, coarse) = (&hierarchy.levels[l], &hierarchy.levels[l + 1]);
        let mut sum = vec![(0.0f64, 0.0f64); coarse.len()];
        for i in 0..fine.len() {
            let p = fine.parent[i] as usize;
            sum[p].0 += (scales[l][i] * fine.area[i]) as f64;
            sum[p].1 += fine.area[i] as f64;
        }
        scales.push(sum.iter().map(|&(a, b)| (a / b.max(1e-30)) as f32).collect());
    }
    // Holds: corners stay, points near a line slide along it.
    let classify = |p: Vec3, h: f32| -> (cleanup::Hold, Vec3) {
        if let Some(c) = features.corner(p, SNAP * h) {
            return (cleanup::Hold::Fixed, c);
        }
        match features.nearest(p, SNAP * h, |_| true) {
            Some((k, q)) => (cleanup::Hold::Line(features.segments[k as usize].chain), q),
            None => (cleanup::Hold::Free, p),
        }
    };
    let slide = |k: u32, p: Vec3| features.slide(k, p);
    let on_plane = |k: u32| features.chains[k as usize] == features::Kind::Plane;
    let protected = |a: u32, b: u32, pm: &cleanup::PolyMesh, hold: &[cleanup::Hold], local: &[f32]| -> Option<u32> {
        let (ha, hb) = (hold.get(a as usize)?, hold.get(b as usize)?);
        if *ha == cleanup::Hold::Free || *hb == cleanup::Hold::Free {
            return None;
        }
        let mid = (pm.points[a as usize] + pm.points[b as usize]) * 0.5;
        let h = 0.5 * (local[a as usize] + local[b as usize]);
        features.nearest(mid, SNAP * h, |_| true).map(|(k, _)| features.segments[k as usize].chain)
    };
    let input_parts = tri_parts(n, &m.tris);
    let lines = features.chains.iter().any(|k| matches!(k, features::Kind::Guide | features::Kind::Border));
    // Guides and borders hold a band up to `plane_band` wide; where that
    // tears the lattice (a tight loop on a concave part: the cage comes out
    // in more pieces than the surface), they are solved again holding
    // only the narrow band.
    let mut attempt = 0;
    let (mut pm, mut hold, mut local_h, singularities) = loop {
        let line_band = if attempt == 0 { plane_band } else { band };
        let constraints = features::constraints(&m, &locks, &normals, &features, band, plane_band, line_band);
        let constraints = field::restrict_constraints(&hierarchy, constraints);
        let q = field::solve(&hierarchy, &start, &constraints, &pulls, ITERATIONS, progress)?;
        let singularities = field::singularities(&m.tris, &q[0], &normals);
        lap("orientation", &mut timings);
        progress.set(0.5);
        let o = position::solve(&hierarchy, &q, &scales, &constraints, ITERATIONS, progress)?;
        lap("position", &mut timings);
        progress.stage("extracting quads", 0.65);
        let g = extract::graph(&hierarchy.levels[0], &q[0], &o, &scales[0]);
        let (faces, removed) = extract::faces(&g, progress)?;
        let mut pm = cleanup::PolyMesh { points: g.p.clone(), faces };
        let kept = pm.compact();
        let mut local_h = vec![h0; pm.points.len()];
        for (old, &new) in kept.iter().enumerate() {
            if new != u32::MAX {
                local_h[new as usize] = g.h[old];
            }
        }
        lap("extract", &mut timings);
        progress.stage("cleaning up", 0.75);
        let mut hold: Vec<cleanup::Hold> = Vec::with_capacity(pm.points.len());
        for (p, &h) in pm.points.iter_mut().zip(&local_h) {
            let (held, q) = classify(*p, h);
            hold.push(held);
            *p = q;
        }
        cleanup::fit(&mut pm, &hold, surface, &slide, &on_plane, h0, cell, 0, progress)?;
        cleanup::split_large(&mut pm);
        hold.resize(pm.points.len(), cleanup::Hold::Free);
        local_h.resize(pm.points.len(), h0);
        // First pairs that leave no more irregular points, then those that
        // leave one more (each still removes two odd faces).
        for worsen in 0..=1 {
            for _ in 0..8 {
                let (h2, l2) = (hold.clone(), local_h.clone());
                let snapshot = pm.clone();
                if cleanup::pair_odd(&mut pm, &|a, b| protected(a, b, &snapshot, &h2, &l2).is_some(), STRIP, worsen) == 0 {
                    break;
                }
            }
        }
        // Dissolving renumbers the points: holds and spacings follow.
        let map = cleanup::dissolve_two(&mut pm);
        hold = cleanup::remap(&hold, &map, pm.points.len(), cleanup::Hold::Free);
        local_h = cleanup::remap(&local_h, &map, pm.points.len(), h0);
        let parts = pm.topology().components();
        if parts > input_parts && lines && attempt == 0 {
            notes.push(format!("the cage came out in {parts} pieces where the surface has {input_parts}: guides and borders were solved again holding a narrower band"));
            attempt += 1;
            continue;
        }
        if removed > 0 {
            notes.push(format!("{removed} crossing or dangling lattice edges left out"));
        }
        if parts > input_parts {
            notes.push(format!("the cage has {parts} pieces where the surface has {input_parts}: some small part of it may be left out (coverage_cells says how far)"));
        }
        break (pm, hold, local_h, singularities);
    };
    let before_full = shape(&pm);
    lap("cleanup", &mut timings);
    // Symmetry: the half on x >= 0, mirrored after the fit.
    let mut half = false;
    if symmetric {
        match cleanup::cut_half(&pm, &local_h) {
            Ok((cut, map)) => {
                let mut h2 = vec![cleanup::Hold::Free; cut.points.len()];
                let mut l2 = vec![h0; cut.points.len()];
                for (old, &new) in map.iter().enumerate() {
                    if new != u32::MAX {
                        h2[new as usize] = hold[old];
                        l2[new as usize] = local_h[old];
                    }
                }
                pm = cut;
                hold = h2;
                local_h = l2;
                // Points on the plane's border slide in it (along their
                // own plane loop); corners stay.
                // Points held to the plane inside the half are free.
                let any_plane = features.chains.iter().position(|k| *k == features::Kind::Plane).map(|c| c as u32);
                let t = pm.topology();
                for v in 0..pm.points.len() {
                    let border = pm.points[v].x == 0.0 && t.edges_of(v).iter().any(|&e| t.is_border(e as usize));
                    if border && hold[v] != cleanup::Hold::Fixed {
                        // Its own plane loop, else any: sliding on the plane
                        // only sets x = 0.
                        let loop_of = features.nearest(pm.points[v], features.cell(), |s| s.kind == features::Kind::Plane).map(|(k, _)| features.segments[k as usize].chain);
                        if let Some(c) = loop_of.or(any_plane) {
                            hold[v] = cleanup::Hold::Line(c);
                        }
                    } else if !border && matches!(hold[v], cleanup::Hold::Line(k) if on_plane(k)) {
                        hold[v] = cleanup::Hold::Free;
                    }
                }
                half = true;
            }
            Err(e) => {
                notes.push(format!("not made symmetric: {e}"));
                // Points held to the plane are free again.
                for h in hold.iter_mut() {
                    if matches!(*h, cleanup::Hold::Line(k) if on_plane(k)) {
                        *h = cleanup::Hold::Free;
                    }
                }
            }
        }
    }
    // Measured on the whole, joined as the result is (a plane point on one
    // face of the half dissolves there too).
    let before_split = if half {
        let mut whole = cleanup::mirror(&pm).0;
        cleanup::dissolve_two(&mut whole);
        shape(&whole)
    } else {
        before_full
    };
    if args.pure_quads {
        let t = pm.topology();
        let split = cleanup::split(&pm);
        // Face points are free; edge points of protected edges keep their line.
        let mut h2 = hold.clone();
        h2.extend(std::iter::repeat_n(cleanup::Hold::Free, t.faces()));
        for &[a, b] in &t.edges {
            let on_plane = half && pm.points[a as usize].x == 0.0 && pm.points[b as usize].x == 0.0 && t.edge_between(a, b).is_some_and(|e| t.is_border(e));
            let h = match protected(a, b, &pm, &hold, &local_h) {
                Some(c) => cleanup::Hold::Line(c),
                None if on_plane => hold[a as usize],
                None => cleanup::Hold::Free,
            };
            h2.push(h);
        }
        pm = split;
        hold = h2;
    }
    progress.stage("fitting to the surface", 0.85);
    cleanup::fit(&mut pm, &hold, surface, &slide, &on_plane, h0, cell, FIT_ROUNDS, progress)?;
    let mut symmetric_error = 0.0f32;
    if half {
        // Every border point on the plane (a fixed corner or one the fit
        // could not keep there would leave an open seam).
        cleanup::seal_plane(&mut pm);
        let (full, err) = cleanup::mirror(&pm);
        pm = full;
        symmetric_error = err;
        // A plane point on a single face of the half has two edges in the
        // whole: its two (mirror-image) faces become one, still symmetric.
        cleanup::dissolve_two(&mut pm);
    }
    lap("fit", &mut timings);
    progress.check()?;
    progress.stage("measuring", 0.95);
    let shape_now = shape(&pm);
    let (hausdorff, mean, samples) = distances(&pm, surface, cell, h0);
    let coverage = coverage(&pm, &m.points, cell);
    progress.check()?;
    let poly = crate::poly::Poly::from_faces(&pm.points, &pm.faces)?;
    lap("measure", &mut timings);
    Ok(Outcome {
        poly,
        before_split,
        shape: shape_now,
        hausdorff_cells: hausdorff,
        mean_distance_cells: mean,
        coverage_cells: coverage,
        samples,
        symmetric: half,
        symmetric_error,
        input_points: m.points.len(),
        input_triangles: m.tris.len(),
        crease_edges: m.creases.len(),
        levels: hierarchy.levels.len(),
        singularities,
        spacing: h0,
        notes,
        timings,
    })
}

/// The largest and mean distance from the cage to the surface, in cells:
/// the field at its points, edge middles and face middles (at most
/// `DISTANCE_SAMPLES`, evenly picked).
pub fn distances(m: &cleanup::PolyMesh, surface: &dyn Surface, cell: f32, reach: f32) -> (f32, f32, usize) {
    let t = m.topology();
    let mut at: Vec<Vec3> = m.points.clone();
    at.extend(t.edges.iter().map(|&[a, b]| (m.points[a as usize] + m.points[b as usize]) * 0.5));
    at.extend((0..t.faces()).map(|f| t.face(f).iter().map(|&v| m.points[v as usize]).sum::<Vec3>() / t.face(f).len() as f32));
    let step = at.len().div_ceil(DISTANCE_SAMPLES).max(1);
    let picked: Vec<Vec3> = at.into_iter().step_by(step).collect();
    let d = hierarchy::par_map(picked.len(), |i| surface.near(picked[i], reach)(picked[i]).abs() / cell);
    let max = d.iter().copied().fold(0.0, f32::max);
    let mean = d.iter().map(|&x| x as f64).sum::<f64>() / d.len().max(1) as f64;
    (max, mean as f32, picked.len())
}

/// The largest distance from `points` (the input surface, at most
/// `DISTANCE_SAMPLES` evenly picked) to the cage's triangles, in cells:
/// where the cage leaves a part of the surface out.
pub fn coverage(m: &cleanup::PolyMesh, points: &[Vec3], cell: f32) -> f32 {
    let mut tris = Vec::new();
    for f in &m.faces {
        for k in 1..f.len() - 1 {
            tris.push([f[0], f[k], f[k + 1]]);
        }
    }
    let field = crate::mesh_asset::MeshField::new(&m.points, &tris);
    let step = points.len().div_ceil(DISTANCE_SAMPLES).max(1);
    let picked: Vec<Vec3> = points.iter().step_by(step).copied().collect();
    let d = hierarchy::par_map(picked.len(), |i| field.distance(picked[i], crate::mesh_asset::Sign::Closed).abs() / cell);
    d.iter().copied().fold(0.0, f32::max)
}

/// The shape of a polygon mesh: faces by sides, valences, edge lengths.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Shape {
    pub faces: usize,
    pub quads_pct: f64,
    pub triangles: usize,
    pub pentagons: usize,
    /// Faces of six or more sides.
    pub larger: usize,
    /// Interior points with other than four edges, % of interior points.
    pub irregular_pct: f64,
    /// Points by edge count.
    pub valence: BTreeMap<usize, usize>,
    /// Edge lengths: standard deviation over mean.
    pub edge_cv: f64,
    pub mean_edge: f64,
    pub open_edges: usize,
    pub non_manifold_edges: usize,
    pub flipped_edges: usize,
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1e4).round() / 100.0 }
}

/// `Shape` of `m`.
pub fn shape(m: &cleanup::PolyMesh) -> Shape {
    let t = m.topology();
    let mut s = Shape { faces: m.faces.len(), ..Default::default() };
    let quads = m.faces.iter().filter(|f| f.len() == 4).count();
    s.quads_pct = pct(quads, s.faces);
    s.triangles = m.faces.iter().filter(|f| f.len() == 3).count();
    s.pentagons = m.faces.iter().filter(|f| f.len() == 5).count();
    s.larger = m.faces.iter().filter(|f| f.len() > 5).count();
    let (mut interior, mut irregular) = (0, 0);
    for v in 0..t.vertices {
        let e = t.edges_of(v);
        if e.is_empty() {
            continue;
        }
        *s.valence.entry(e.len()).or_default() += 1;
        if !e.iter().any(|&e| t.is_border(e as usize)) {
            interior += 1;
            irregular += (e.len() != 4) as usize;
        }
    }
    s.irregular_pct = pct(irregular, interior);
    let lengths: Vec<f64> = t.edges.iter().map(|&[a, b]| m.points[a as usize].distance(m.points[b as usize]) as f64).collect();
    let mean = lengths.iter().sum::<f64>() / lengths.len().max(1) as f64;
    let var = lengths.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / lengths.len().max(1) as f64;
    s.mean_edge = mean;
    s.edge_cv = if mean > 0.0 { (var.sqrt() / mean * 1e4).round() / 1e4 } else { 0.0 };
    s.open_edges = (0..t.edges.len()).filter(|&e| t.faces_of_edge(e).len() == 1).count();
    (s.non_manifold_edges, s.flipped_edges) = t.non_manifold();
    s
}

