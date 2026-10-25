//! Geometry measures for meshing and sculpt checks: Hausdorff distance
//! (one-sided and symmetric), error against an analytic SDF, volume and
//! area, normal angular error, and brick-hash equality of detail grids.
//!
//! Distances to a mesh are exact point-to-triangle distances, found
//! through a uniform grid of triangle boxes searched ring by ring, so they
//! stay cheap on 100k+ triangle meshes.

use std::collections::HashMap;

use glam::{IVec3, Vec3};

use crate::clay::DetailGrid;
use crate::mesh::Mesh;

/// The triangles of `mesh` as corner positions.
pub fn triangles(mesh: &Mesh) -> Vec<[Vec3; 3]> {
    mesh.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| Vec3::from(mesh.vertices[t[k] as usize].pos))).collect()
}

/// Enclosed volume (divergence theorem; positive for outward winding).
pub fn volume(mesh: &Mesh) -> f64 {
    triangles(mesh).iter().map(|[a, b, c]| f64::from(a.dot(b.cross(*c)))).sum::<f64>() / 6.0
}

/// Total triangle area.
pub fn area(mesh: &Mesh) -> f64 {
    triangles(mesh).iter().map(|[a, b, c]| f64::from((*b - *a).cross(*c - *a).length())).sum::<f64>() * 0.5
}

/// Closest point to `p` on triangle `abc` (Ericson, Real-Time Collision
/// Detection 5.1.5).
pub fn closest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// Exact distance queries to a triangle mesh.
pub struct Distance {
    tris: Vec<[Vec3; 3]>,
    cell: f32,
    grid: HashMap<IVec3, Vec<u32>>,
    lo: IVec3,
    hi: IVec3,
}

impl Distance {
    pub fn new(mesh: &Mesh) -> Self {
        let tris = triangles(mesh);
        assert!(!tris.is_empty(), "distance to an empty mesh");
        // About two mean edge lengths per cell keeps each cell's list short.
        let edge = tris.iter().map(|[a, b, c]| (a.distance(*b) + b.distance(*c) + c.distance(*a)) / 3.0).sum::<f32>() / tris.len() as f32;
        let cell = (edge * 2.0).max(1e-6);
        let key = |p: Vec3| (p / cell).floor().as_ivec3();
        let mut grid: HashMap<IVec3, Vec<u32>> = HashMap::new();
        let (mut lo, mut hi) = (IVec3::MAX, IVec3::MIN);
        for (i, [a, b, c]) in tris.iter().enumerate() {
            let (k0, k1) = (key(a.min(*b).min(*c)), key(a.max(*b).max(*c)));
            lo = lo.min(k0);
            hi = hi.max(k1);
            for z in k0.z..=k1.z {
                for y in k0.y..=k1.y {
                    for x in k0.x..=k1.x {
                        grid.entry(IVec3::new(x, y, z)).or_default().push(i as u32);
                    }
                }
            }
        }
        Distance { tris, cell, grid, lo, hi }
    }

    /// The closest point on the mesh to `p`.
    pub fn closest(&self, p: Vec3) -> Vec3 {
        let home = (p / self.cell).floor().as_ivec3();
        // Rings beyond the grid's box add nothing; stop once they are all searched.
        let reach = (home - self.lo).abs().max((self.hi - home).abs()).max_element();
        let (mut best, mut best_d2) = (p, f32::INFINITY);
        for r in 0..=reach {
            for z in -r..=r {
                for y in -r..=r {
                    for x in -r..=r {
                        if x.abs().max(y.abs()).max(z.abs()) != r {
                            continue;
                        }
                        let Some(list) = self.grid.get(&(home + IVec3::new(x, y, z))) else { continue };
                        for &t in list {
                            let [a, b, c] = self.tris[t as usize];
                            let q = closest_on_triangle(p, a, b, c);
                            let d2 = q.distance_squared(p);
                            if d2 < best_d2 {
                                (best, best_d2) = (q, d2);
                            }
                        }
                    }
                }
            }
            // Anything in ring r + 1 or later is at least r cells away.
            if best_d2.is_finite() && best_d2.sqrt() <= r as f32 * self.cell {
                break;
            }
        }
        best
    }

    pub fn distance(&self, p: Vec3) -> f32 {
        self.closest(p).distance(p)
    }
}

/// Points spread over a mesh: every vertex, and each triangle's centroid
/// and edge midpoints (where flat triangles stray furthest from a curve).
pub fn surface_samples(mesh: &Mesh) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect();
    for [a, b, c] in triangles(mesh) {
        out.extend([(a + b + c) / 3.0, (a + b) * 0.5, (b + c) * 0.5, (c + a) * 0.5]);
    }
    out
}

/// One-sided Hausdorff distance: the furthest any sample of `from` is
/// from the mesh `to`.
pub fn hausdorff_one_sided(from: &[Vec3], to: &Distance) -> f32 {
    from.iter().map(|p| to.distance(*p)).fold(0.0, f32::max)
}

/// Symmetric Hausdorff distance between two meshes.
pub fn hausdorff(a: &Mesh, b: &Mesh) -> f32 {
    hausdorff_one_sided(&surface_samples(a), &Distance::new(b)).max(hausdorff_one_sided(&surface_samples(b), &Distance::new(a)))
}

/// How far a mesh is from an analytic surface.
#[derive(Debug, Clone, Copy)]
pub struct SdfError {
    /// Furthest a mesh sample is from the surface (|sdf|).
    pub mesh_to_surface: f32,
    /// Furthest a point of the surface (from `surface`) is from the mesh.
    pub surface_to_mesh: f32,
    /// Mean |sdf| over the mesh samples.
    pub mean: f32,
}

impl SdfError {
    /// Symmetric Hausdorff distance.
    pub fn hausdorff(&self) -> f32 {
        self.mesh_to_surface.max(self.surface_to_mesh)
    }
}

/// `mesh` against the zero set of `sdf` (an exact distance), with
/// `surface` points on that zero set for the other direction.
pub fn sdf_error(mesh: &Mesh, sdf: impl Fn(Vec3) -> f32, surface: &[Vec3]) -> SdfError {
    let samples = surface_samples(mesh);
    let d: Vec<f32> = samples.iter().map(|p| sdf(*p).abs()).collect();
    let mesh_to_surface = d.iter().copied().fold(0.0, f32::max);
    let mean = d.iter().sum::<f32>() / d.len().max(1) as f32;
    let surface_to_mesh = if surface.is_empty() { 0.0 } else { hausdorff_one_sided(surface, &Distance::new(mesh)) };
    SdfError { mesh_to_surface, surface_to_mesh, mean }
}

/// `n` points evenly over a sphere (a Fibonacci lattice).
pub fn sphere_points(center: Vec3, radius: f32, n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = golden * i as f32;
            center + Vec3::new(r * a.cos(), y, r * a.sin()) * radius
        })
        .collect()
}

/// Angular error of the vertex normals against `truth` (the analytic
/// normal at a point), in degrees.
#[derive(Debug, Clone, Copy)]
pub struct NormalError {
    pub mean_deg: f32,
    pub max_deg: f32,
}

pub fn normal_error(mesh: &Mesh, truth: impl Fn(Vec3) -> Vec3) -> NormalError {
    let angles: Vec<f32> = mesh
        .vertices
        .iter()
        .map(|v| Vec3::from(v.normal).normalize_or_zero().dot(truth(Vec3::from(v.pos)).normalize_or_zero()).clamp(-1.0, 1.0).acos().to_degrees())
        .collect();
    NormalError { mean_deg: angles.iter().sum::<f32>() / angles.len().max(1) as f32, max_deg: angles.iter().copied().fold(0.0, f32::max) }
}

/// A content hash of a detail grid and its levels: the same offsets give
/// the same hash whatever the brick order or sharing. FNV-1a over sorted
/// bricks, bit-exact (a test hash; saved hashes come with the ledger).
pub fn brick_hash(grid: &DetailGrid) -> u64 {
    fn eat(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h = (*h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn level(h: &mut u64, g: &DetailGrid) {
        eat(h, &g.voxel.to_bits().to_le_bytes());
        let mut bricks: Vec<_> = g.bricks().collect();
        bricks.sort_by_key(|(k, _)| **k);
        eat(h, &(bricks.len() as u64).to_le_bytes());
        for (k, values) in bricks {
            for c in k {
                eat(h, &c.to_le_bytes());
            }
            for v in values.iter() {
                eat(h, &v.to_bits().to_le_bytes());
            }
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    level(&mut h, grid);
    for l in grid.levels() {
        level(&mut h, &l.grid);
    }
    h
}

/// Whether two grids hold bit-identical offsets.
pub fn bricks_equal(a: &DetailGrid, b: &DetailGrid) -> bool {
    brick_hash(a) == brick_hash(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Vertex;

    fn mesh(positions: &[Vec3], indices: &[u32]) -> Mesh {
        let vertices = positions.iter().map(|p| Vertex { pos: p.to_array(), normal: p.normalize_or_zero().to_array(), ..bytemuck::Zeroable::zeroed() }).collect();
        Mesh { vertices, indices: indices.to_vec(), ..Mesh::default() }
    }

    /// A unit cube from 0 to `s`, wound outward.
    fn cube(s: f32) -> Mesh {
        let p: Vec<Vec3> = (0..8).map(|i| Vec3::new((i & 1) as f32, (i >> 1 & 1) as f32, (i >> 2 & 1) as f32) * s).collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let idx: Vec<u32> = quads.iter().flat_map(|[a, b, c, d]| [*a, *b, *c, *a, *c, *d]).collect();
        mesh(&p, &idx)
    }

    #[test]
    fn measures_a_cube() {
        let m = cube(2.0);
        assert!((volume(&m) - 8.0).abs() < 1e-5, "{}", volume(&m));
        assert!((area(&m) - 24.0).abs() < 1e-5);
        let d = Distance::new(&m);
        assert!((d.distance(Vec3::new(1.0, 1.0, 5.0)) - 3.0).abs() < 1e-5);
        assert!((d.distance(Vec3::new(1.0, 1.0, 1.0)) - 1.0).abs() < 1e-5);
        assert!((d.distance(Vec3::new(3.0, 3.0, 1.0)) - 2f32.sqrt()).abs() < 1e-5);
        assert!(hausdorff(&m, &m) < 1e-6);
        let mut moved = cube(2.0);
        for v in &mut moved.vertices {
            v.pos[1] += 0.25;
        }
        assert!((hausdorff(&m, &moved) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn measures_against_an_analytic_sphere() {
        let sdf = |p: Vec3| p.length() - 1.0;
        let points = sphere_points(Vec3::ZERO, 1.0, 500);
        assert!(points.iter().all(|p| sdf(*p).abs() < 1e-5));
        // An octahedron inscribed in the unit sphere: its vertices are on it,
        // its face centers 1/sqrt(3) from the center.
        let p = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
        let m = mesh(&p, &[0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5]);
        let e = sdf_error(&m, sdf, &points);
        let gap = 1.0 - 1.0 / 3f32.sqrt();
        assert!((e.mesh_to_surface - gap).abs() < 1e-4, "{e:?}");
        assert!(e.surface_to_mesh <= gap + 1e-4 && e.surface_to_mesh > gap * 0.9, "{e:?}");
        assert!((volume(&m) - 4.0 / 3.0).abs() < 1e-5);
        let n = normal_error(&m, |q| q);
        assert!(n.max_deg < 1e-3, "{n:?}");
    }

    #[test]
    fn brick_hash_follows_content_not_identity() {
        let mut a = crate::clay::Detail::new(0.01);
        a.grid_mut().set([3, -2, 9], 0.5);
        a.grid_mut().set([40, 1, 1], -0.25);
        let mut b = crate::clay::Detail::new(0.01);
        b.grid_mut().set([40, 1, 1], -0.25);
        b.grid_mut().set([3, -2, 9], 0.5);
        assert!(bricks_equal(a.grid(), b.grid()));
        b.grid_mut().set([3, -2, 9], 0.5000001);
        assert!(!bricks_equal(a.grid(), b.grid()));
        assert_ne!(brick_hash(a.grid()), brick_hash(crate::clay::Detail::new(0.02).grid()));
    }
}
