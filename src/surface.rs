//! Finding points on a clay surface, for agents that place strokes and
//! objects: a guessed point a little inside or outside the clay snaps to
//! the surface (`Surface::snap`).
//!
//! It starts at the nearest vertex of the clay's mesh (works at any
//! distance) and then follows the field to the exact surface, which can be
//! finer than the mesh.

use glam::Vec3;

use crate::clay::{Clay, Field};
use crate::mesh::Mesh;
use crate::rig::NearestVertex;

pub struct Surface<'a> {
    mesh: &'a Mesh,
    nearest: NearestVertex<'a>,
    field: Field,
    cell: f32,
}

impl<'a> Surface<'a> {
    /// `mesh` is the clay as one shell (`MeshLib::whole`), in clay space.
    pub fn new(clay: &Clay, mesh: &'a Mesh) -> Option<Surface<'a>> {
        let cell = clay.cell_size()?;
        Some(Surface { mesh, nearest: NearestVertex::new(mesh, cell * 2.0), field: Field::new(clay, cell), cell })
    }

    /// The surface point nearest `p`, and the surface normal there.
    pub fn snap(&self, p: Vec3) -> Option<(Vec3, Vec3)> {
        // The grid search gives up far from the mesh; then check every vertex.
        let v = self.nearest.vertex(p).or_else(|| self.mesh.vertices.iter().min_by(|a, b| Vec3::from(a.pos).distance_squared(p).total_cmp(&Vec3::from(b.pos).distance_squared(p))))?;
        let mut q = Vec3::from(v.pos);
        let r = Vec3::splat(self.cell * 3.0);
        let list = self.field.near(q - r, q + r);
        let d = |x: Vec3| self.field.sample(x, &list).0;
        let e = self.cell * 0.25;
        let grad = |x: Vec3| Vec3::new(d(x + Vec3::X * e) - d(x - Vec3::X * e), d(x + Vec3::Y * e) - d(x - Vec3::Y * e), d(x + Vec3::Z * e) - d(x - Vec3::Z * e));
        // Preserve sub-cell tangential motion instead of pinning repeated
        // short hair steps to the same nearest mesh vertex.
        let delta = p - q;
        if delta.length() < self.cell * 3.0 {
            let n = Vec3::from(v.normal).normalize_or(Vec3::Y);
            q += delta - n * delta.dot(n);
        }
        let start = q;
        for _ in 0..4 {
            let g = grad(q).normalize_or(Vec3::from(v.normal));
            q -= g * d(q);
        }
        if (q - start).length() > self.cell * 2.0 {
            q = start;
        }
        Some((q, grad(q).normalize_or(Vec3::from(v.normal))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_preserves_subcell_motion_along_skin() {
        let clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0,0,0], "radius": 0.5 }], "resolution": 48
        })).unwrap();
        let mesh = clay.build_mesh();
        let s = Surface::new(&clay, &mesh).unwrap();
        let (p, n) = s.snap(Vec3::Z * 0.5).unwrap();
        let step = n.any_orthonormal_vector() * 0.0005;
        let (q, _) = s.snap(p + step).unwrap();
        assert!((q - p).dot(step.normalize()) > 0.0004);
    }

    #[test]
    fn far_and_inside_points_snap_onto_a_sphere() {
        let clay: Clay = serde_json::from_value(serde_json::json!({
            "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }], "resolution": 48
        }))
        .unwrap();
        let mesh = clay.build_mesh();
        let s = Surface::new(&clay, &mesh).unwrap();
        for p in [Vec3::new(0.0, 0.0, 3.0), Vec3::new(0.1, 0.2, 0.05), Vec3::new(-0.4, 0.4, 0.0)] {
            let (q, n) = s.snap(p).unwrap();
            assert!((q.length() - 0.5).abs() < 0.004, "{q}");
            assert!(n.dot(q.normalize()) > 0.99);
            assert!(q.normalize().dot(p.normalize()) > 0.97, "{p} -> {q}");
        }
    }
}
