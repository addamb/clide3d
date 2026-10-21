//! Triangulated wing sheets. Distance to the sheet minus half thickness
//! gives a safe field with rounded edges, including on nonplanar polygons.

use glam::{Vec2, Vec3};
use crate::human::MeshSdf;

pub(super) fn sheet(points: &[Vec3], sag: f32, scallop: f32, edges: &[usize]) -> MeshSdf {
    if !(3..=20).contains(&points.len()) || points.iter().any(|p| !p.is_finite()) {
        return MeshSdf::new(&[], &[]);
    }
    let normal = points.iter().enumerate().map(|(i, p)| p.cross(points[(i + 1) % points.len()])).sum::<Vec3>().normalize_or_zero();
    if normal == Vec3::ZERO {
        return MeshSdf::new(&[], &[]);
    }
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let project = |p: Vec3| Vec2::new(p.dot(u), p.dot(v));
    let mut boundary = Vec::new();
    for (i, &a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        boundary.push(a);
        if scallop > 0.0 && edges.contains(&i) {
            let inward = normal.cross(b - a).normalize_or_zero();
            for j in 1..8 {
                let t = j as f32 / 8.0;
                boundary.push(a.lerp(b, t) + inward * (scallop * a.distance(b) * 4.0 * t * (1.0 - t)));
            }
        }
    }
    let flat: Vec<Vec2> = boundary.iter().map(|&p| project(p)).collect();
    let cross = |a: Vec2, b: Vec2, c: Vec2| (b - a).perp_dot(c - a);
    let mut ring: Vec<usize> = (0..boundary.len()).collect();
    let mut triangles = Vec::new();
    // Ear clipping supports concave outlines and scalloped trailing edges.
    while ring.len() > 2 {
        let ear = (0..ring.len()).find(|&i| {
            let (a, b, c) = (ring[(i + ring.len() - 1) % ring.len()], ring[i], ring[(i + 1) % ring.len()]);
            cross(flat[a], flat[b], flat[c]) > 1e-10 && !ring.iter().any(|&k| {
                k != a && k != b && k != c && cross(flat[a], flat[b], flat[k]) >= -1e-10
                    && cross(flat[b], flat[c], flat[k]) >= -1e-10 && cross(flat[c], flat[a], flat[k]) >= -1e-10
            })
        });
        let Some(i) = ear else { return MeshSdf::new(&[], &[]) };
        triangles.push([ring[(i + ring.len() - 1) % ring.len()] as u32, ring[i] as u32, ring[(i + 1) % ring.len()] as u32]);
        ring.remove(i);
    }
    if sag == 0.0 {
        return MeshSdf::new(&boundary, &triangles);
    }
    let edge_distance = |p: Vec3| {
        let p = project(p);
        let distances = flat.iter().enumerate().map(|(i, &a)| {
            let ab = flat[(i + 1) % flat.len()] - a;
            p.distance(a + ab * ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0))
        });
        // A smooth minimum avoids a crease where two boundary edges are
        // equally close. Its zero stays on the boundary.
        let mut sum = 0.0;
        for d in distances {
            if d < 1e-6 { return 0.0; }
            sum += d.powi(-4);
        }
        sum.powf(-0.25)
    };
    // Shared edge midpoints keep the sheet continuous as it bows inward.
    for _ in 0..4 {
        let mut midpoints = std::collections::HashMap::new();
        let mut next = Vec::new();
        for [a, b, c] in triangles {
            let mut midpoint = |i: u32, j: u32| *midpoints.entry((i.min(j), i.max(j))).or_insert_with(|| {
                let at = boundary.len() as u32;
                boundary.push((boundary[i as usize] + boundary[j as usize]) * 0.5);
                at
            });
            let (ab, bc, ca) = (midpoint(a, b), midpoint(b, c), midpoint(c, a));
            next.extend([[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]);
        }
        triangles = next;
    }
    let depth = boundary.iter().map(|&p| edge_distance(p)).fold(0.0, f32::max).max(1e-6);
    for p in &mut boundary {
        let t = (edge_distance(*p) / depth).clamp(0.0, 1.0);
        *p += normal * (sag * t * (2.0 - t));
    }
    MeshSdf::new(&boundary, &triangles)
}
