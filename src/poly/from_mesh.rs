//! A cage from a clay's surface-nets mesh (`poly from_clay`): the quick,
//! dense, unclean starting point (Stage 3b's retopology is the clean one).
//!
//! Surface nets make one quad per grid edge the surface crosses and the
//! mesher writes each as two triangles in a row, keeping the quad's four
//! sides (not its diagonal) as the wireframe. The triangles are paired
//! back into those quads, welded by position, and made manifold where the
//! nets pinch two sheets together (`adaptive::separate_pinches`, as print
//! export does): each sheet gets its own copy of the shared corners, moved
//! a little toward its own side.

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use super::{MAX_CAGE_FACES, Poly};

/// What `from_triangles` made.
pub struct FromMesh {
    pub poly: Poly,
    pub quads: usize,
    pub triangles: usize,
    /// Faces left out: collapsed to fewer than three points by the weld.
    pub dropped: usize,
    /// Corners copied to separate pinched sheets.
    pub separated: usize,
    /// Points copied where faces meet only at a point (two fans).
    pub split_points: usize,
}

/// The quad `t0` and `t1` make when they share an edge, wound opposite
/// ways (t0's a → b is t1's b → a): a, t1's third point, b, t0's third.
fn pair(t0: [u32; 3], t1: [u32; 3], wire: Option<&HashSet<(u32, u32)>>) -> Option<[u32; 4]> {
    for k in 0..3 {
        let (a, b, c) = (t0[k], t0[(k + 1) % 3], t0[(k + 2) % 3]);
        for m in 0..3 {
            if t1[m] == b && t1[(m + 1) % 3] == a {
                let d = t1[(m + 2) % 3];
                if d == a || d == b || d == c || wire.is_some_and(|w| w.contains(&(a.min(b), a.max(b)))) {
                    return None;
                }
                return Some([a, d, b, c]);
            }
        }
    }
    None
}

/// A cage from triangles written as quads two in a row, `wire` the
/// mesh's wireframe pairs (the quads' sides; when it holds every triangle
/// edge, as after marching tetrahedra or adaptive nets, any two triangles
/// in a row that share an edge are paired).
/// `min_offset` is the least a pinched copy moves.
pub fn from_triangles(points: &[Vec3], indices: &[u32], wire: &[u32], min_offset: f32) -> Result<FromMesh, String> {
    let tris: Vec<[u32; 3]> = indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    if tris.is_empty() {
        return Err("the mesh has no triangles".into());
    }
    if tris.len() > 2 * MAX_CAGE_FACES {
        return Err(format!("the mesh has {} triangles; a cage is made from at most {}", tris.len(), 2 * MAX_CAGE_FACES));
    }
    // The wireframe leaves the quads' diagonals out when it holds fewer
    // edges than the triangles have.
    let mut tri_edges: Vec<u64> = tris.iter().flat_map(|t| (0..3).map(move |k| { let (a, b) = (t[k], t[(k + 1) % 3]); (a.min(b) as u64) << 32 | a.max(b) as u64 })).collect();
    tri_edges.sort_unstable();
    tri_edges.dedup();
    let sides: Option<HashSet<(u32, u32)>> = Some(wire.chunks_exact(2).map(|e| (e[0].min(e[1]), e[0].max(e[1]))).collect::<HashSet<_>>()).filter(|w| w.len() < tri_edges.len());
    let mut faces: Vec<Vec<u32>> = Vec::with_capacity(tris.len() / 2 + 1);
    let mut i = 0;
    while i < tris.len() {
        match tris.get(i + 1).and_then(|&t1| pair(tris[i], t1, sides.as_ref())) {
            Some(q) => {
                faces.push(q.to_vec());
                i += 2;
            }
            None => {
                faces.push(tris[i].to_vec());
                i += 1;
            }
        }
    }
    // Weld by position (bit-equal; -0 and 0 are one point).
    let mut at: HashMap<[u32; 3], u32> = HashMap::with_capacity(points.len());
    let mut welded: Vec<Vec3> = Vec::with_capacity(points.len());
    let remap: Vec<u32> = points
        .iter()
        .map(|p| {
            let key = p.to_array().map(|c| if c == 0.0 { 0 } else { c.to_bits() });
            *at.entry(key).or_insert_with(|| {
                welded.push(*p);
                welded.len() as u32 - 1
            })
        })
        .collect();
    let mut dropped = 0;
    let mut kept: Vec<Vec<u32>> = Vec::with_capacity(faces.len());
    for f in faces {
        let mut g: Vec<u32> = f.iter().map(|&v| remap[v as usize]).collect();
        g.dedup();
        while g.len() > 1 && g.first() == g.last() {
            g.pop();
        }
        if g.len() < 3 || (1..g.len()).any(|i| g[..i].contains(&g[i])) {
            dropped += 1;
            continue;
        }
        kept.push(g);
    }
    // Pinches apart, on the faces' fans (two triangles per quad, in order).
    let mut fan: Vec<[u32; 3]> = Vec::with_capacity(kept.len() * 2);
    for f in &kept {
        for j in 1..f.len() - 1 {
            fan.push([f[0], f[j], f[j + 1]]);
        }
    }
    let separated = crate::adaptive::separate_pinches(&mut welded, &mut fan, min_offset, 0.02).copies;
    let mut out: Vec<Vec<u32>> = Vec::with_capacity(kept.len());
    let mut k = 0;
    for f in &kept {
        if f.len() == 4 {
            let (t0, t1) = (fan[k], fan[k + 1]);
            // Both halves in one sheet: the quad again; else its halves.
            if t1[0] == t0[0] && t1[1] == t0[2] {
                out.push(vec![t0[0], t0[1], t0[2], t1[2]]);
            } else {
                out.push(t0.to_vec());
                out.push(t1.to_vec());
            }
            k += 2;
        } else {
            out.push(fan[k].to_vec());
            k += 1;
        }
    }
    let split_points = split_fans(&mut welded, &mut out);
    // Points no face uses (dropped faces', welded copies') are left out.
    let mut slot = vec![u32::MAX; welded.len()];
    let mut cage = Vec::new();
    for f in &mut out {
        for v in f.iter_mut() {
            if slot[*v as usize] == u32::MAX {
                slot[*v as usize] = cage.len() as u32;
                cage.push(welded[*v as usize]);
            }
            *v = slot[*v as usize];
        }
    }
    let quads = out.iter().filter(|f| f.len() == 4).count();
    let triangles = out.len() - quads;
    let poly = Poly::from_faces(&cage, &out)?;
    Ok(FromMesh { poly, quads, triangles, dropped, separated, split_points })
}

/// Where the faces around a point make more than one fan (joined through
/// edges in two faces), as where two parts touch at a point or the weld
/// merged a pinch's copies back, gives each fan after the first its own
/// copy of the point (in place). Returns the copies made.
fn split_fans(points: &mut Vec<Vec3>, faces: &mut [Vec<u32>]) -> usize {
    let mut face_start = vec![0u32];
    let mut corners = Vec::new();
    for f in faces.iter() {
        corners.extend_from_slice(f);
        face_start.push(corners.len() as u32);
    }
    let t = super::topology::Topology::build(points.len(), face_start, corners);
    let mut copies = 0;
    for v in 0..t.vertices {
        let around = t.faces_of(v);
        if around.len() < 2 {
            continue;
        }
        // The faces around v, by the fan each is in.
        let mut fans = t.fans(v);
        let first = fans[0];
        if fans.iter().all(|&r| r == first) {
            continue;
        }
        // Fans after the one holding v's first face get copies, in order.
        let mut copy_of: Vec<(usize, u32)> = Vec::new();
        for (i, r) in fans.iter_mut().enumerate() {
            if *r == first {
                continue;
            }
            let c = match copy_of.iter().find(|(k, _)| k == r) {
                Some(&(_, c)) => c,
                None => {
                    points.push(points[v]);
                    copies += 1;
                    copy_of.push((*r, points.len() as u32 - 1));
                    points.len() as u32 - 1
                }
            };
            for corner in faces[around[i] as usize].iter_mut().filter(|c| **c as usize == v) {
                *corner = c;
            }
        }
    }
    copies
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two quads written as triangle pairs with their sides as the wire,
    /// points duplicated (as an unwelded mesh): two quads, six points.
    #[test]
    fn pairs_and_welds_quads() {
        let p = [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y, Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 0.0)];
        // Quad 0: 0 1 2 3; quad 1 on its own copies of 1 and 2: 6 4 5 7.
        let mut points = p.to_vec();
        points.extend([p[1], p[2]]);
        let indices = [0, 1, 2, 0, 2, 3, 6, 4, 5, 6, 5, 7];
        let wire = [0, 1, 1, 2, 2, 3, 3, 0, 6, 4, 4, 5, 5, 7, 7, 6];
        let m = from_triangles(&points, &indices, &wire, 1e-6).unwrap();
        assert_eq!((m.quads, m.triangles, m.dropped, m.separated, m.split_points), (2, 0, 0, 0, 0));
        assert_eq!((m.poly.faces(), m.poly.cage.len()), (2, 6));
        // Without a quad-side wire, the same pairs.
        let all: Vec<u32> = indices.chunks(3).flat_map(|t| [t[0], t[1], t[1], t[2], t[2], t[0]]).collect();
        assert_eq!(from_triangles(&points, &indices, &all, 1e-6).unwrap().quads, 2);
        // A quad-side wire holding the first quad's diagonal keeps its
        // triangles apart.
        let mut sides = wire.to_vec();
        sides.extend([0, 2]);
        let m = from_triangles(&points, &indices, &sides[..18], 1e-6).unwrap();
        assert_eq!((m.quads, m.triangles), (1, 2));
        // Two squares touching at one corner: the corner splits in two.
        let p = [Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y, Vec3::new(2.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 0.0), Vec3::new(1.0, 2.0, 0.0)];
        let m = from_triangles(&p, &[0, 1, 2, 0, 2, 3, 2, 4, 5, 2, 5, 6], &[], 1e-6).unwrap();
        assert_eq!((m.quads, m.split_points, m.poly.cage.len()), (2, 1, 8));
    }
}
