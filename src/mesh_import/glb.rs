//! GLB through `glb_read` (the reader export checks use): every mesh
//! instance at the rest pose, placed by its node.

use glam::Vec3;

use super::Soup;

pub fn read(bytes: &[u8]) -> Result<Soup, String> {
    let loaded = crate::glb_read::from_bytes(bytes, None, None)?;
    let mut soup = Soup::default();
    for (k, item) in loaded.items.iter().enumerate() {
        let base = soup.points.len() as u32;
        for v in &item.mesh.vertices {
            soup.point(item.world.transform_point3(Vec3::from(v.pos)))?;
        }
        // A mirroring node (baked into the points) or placement leaves
        // the triangles inside out: turn them back.
        let flip = loaded.mirrored.get(k).copied().unwrap_or(false);
        let mirrored = flip != (item.world.determinant() < 0.0);
        for t in item.mesh.indices.chunks_exact(3) {
            let c = [t[0], t[1], t[2]].map(|i| base.saturating_add(i));
            soup.polygon(&if mirrored { [c[0], c[2], c[1]] } else { c })?;
        }
    }
    Ok(soup)
}
