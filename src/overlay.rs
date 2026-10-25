//! The viewport's mask and polygroup overlay on clay (docs/SCULPT_PLAN.md
//! Stage 3): until the slim sculpt vertex and shader-side clipping
//! (Stage 6), the drawn mesh is copied with masked surface darkened and,
//! with the groups overlay, each group tinted in its own color, into
//! `Vertex::color`; hidden groups' triangles are left out of the index
//! buffer. Only the viewport draws (and picks) the copy: film, bakes and
//! exports read the clay's own meshes, untinted and whole.

use glam::Vec3;

use crate::groups::Groups;
use crate::mask::SculptMask;
use crate::mesh::Mesh;

/// How dark full mask makes the surface (luminance x this).
pub const MASKED: f32 = 0.4;

/// What the viewport tints clay with. Display state: not saved, not an
/// undo step (`set_display overlay`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Overlay {
    /// Neither (hidden groups are still hidden).
    None,
    /// Masked surface darkened.
    #[default]
    Mask,
    /// Groups tinted, and masked surface darkened.
    Groups,
}

impl Overlay {
    pub fn parse(s: &str) -> Result<Overlay, String> {
        match s {
            "none" => Ok(Overlay::None),
            "mask" => Ok(Overlay::Mask),
            "groups" => Ok(Overlay::Groups),
            other => Err(format!("unknown overlay {other}; use none, mask or groups")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Overlay::None => "none",
            Overlay::Mask => "mask",
            Overlay::Groups => "groups",
        }
    }
}

/// A group's tint: hues a golden angle apart.
pub fn group_color(id: u16) -> Vec3 {
    let h = (id as f32 * 0.618_034).fract() * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Vec3::new(r, g, b) * 0.75 + 0.2
}

/// `drawn` (a clay's mesh as shown, perhaps posed) with the overlay;
/// `source` is the same mesh unposed (its vertex i is drawn's vertex i,
/// in the clay's space, where the mask and groups are).
pub fn tint(drawn: &Mesh, source: &Mesh, mask: Option<&SculptMask>, groups: Option<&Groups>, show_groups: bool) -> Mesh {
    let mut mesh = drawn.clone();
    let n = mesh.vertices.len().min(source.vertices.len());
    let hidden = groups.map(Groups::hidden).unwrap_or_default();
    let mut labels = vec![0u16; if groups.is_some() { n } else { 0 }];
    if groups.is_some() {
        crate::clay::par_chunks(&mut labels, 2048, |chunk, out| {
            for (j, l) in out.iter_mut().enumerate() {
                *l = groups.expect("some").label(Vec3::from(source.vertices[chunk * 2048 + j].pos));
            }
        });
    }
    let labels_ref = &labels;
    crate::clay::par_chunks(&mut mesh.vertices[..n], 2048, |chunk, out| {
        for (j, v) in out.iter_mut().enumerate() {
            let i = chunk * 2048 + j;
            let mut c = Vec3::from(v.color);
            if show_groups && let Some(&id) = labels_ref.get(i).filter(|id| **id != 0) {
                c = c.lerp(group_color(id), 0.6);
            }
            if let Some(m) = mask {
                let k = m.grid().value(Vec3::from(source.vertices[i].pos));
                if k > 0.0 {
                    c *= 1.0 - (1.0 - MASKED) * k;
                }
            }
            v.color = c.to_array();
        }
    });
    if !hidden.is_empty() {
        let off = |i: u32| labels.get(i as usize).is_some_and(|id| *id != 0 && hidden.contains(id));
        // A triangle goes when most of its corners are hidden.
        mesh.indices = mesh.indices.chunks_exact(3).filter(|t| t.iter().filter(|i| off(**i)).count() < 2).flatten().copied().collect();
        mesh.edges = mesh.edges.chunks_exact(2).filter(|e| !off(e[0]) && !off(e[1])).flatten().copied().collect();
    }
    mesh
}
