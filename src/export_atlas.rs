//! Pack existing baked maps without merging objects, changing skins or rebaking.
//! Square tiles have replicated gutters. Uniform UV scaling preserves tangents.
use crate::mesh::{Mesh, Textures};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
pub struct Tile {
    pub textures: Arc<Textures>,
    pub offset: [f32; 2],
    pub scale: f32,
}
pub type Atlas = HashMap<usize, Tile>;
pub fn key(t: &Arc<Textures>) -> usize {
    Arc::as_ptr(t) as usize
}
const SIDE: u32 = 4096;
const BORDER: u32 = 4;

/// At most two 4096 texture sets: six images and 96 Mi pixels, inside
/// check_export's image and decoded pixel limits. Alpha cards stay separate.
pub fn pack(inputs: Vec<(Arc<Mesh>, f32)>, density: Option<f32>) -> Result<Atlas, String> {
    if density.is_some_and(|d| !d.is_finite() || d <= 0.0) {
        return Err("texel_density must be a positive number (pixels per world meter)".into());
    }
    let mut entries: Vec<(Arc<Textures>, u32)> = Vec::new();
    for (mesh, scale) in inputs {
        let Some(t) = mesh.textures.as_ref().filter(|t| !t.alpha) else {
            continue;
        };
        if mesh.vertices.iter().any(|v| {
            v.uv.iter()
                .any(|u| !u.is_finite() || *u < -1e-5 || *u > 1.00001)
        }) {
            continue;
        }
        let requested = if let Some(d) = density {
            let area: f32 = mesh
                .indices
                .chunks_exact(3)
                .map(|tri| {
                    let [a, b, c] =
                        [0, 1, 2].map(|i| glam::Vec3::from(mesh.vertices[tri[i] as usize].pos));
                    (b - a).cross(c - a).length() * 0.5 * scale * scale
                })
                .sum();
            (area.sqrt() * d).ceil().clamp(16.0, SIDE as f32) as u32
        } else {
            t.size
        };
        let side = requested.next_power_of_two().clamp(32, SIDE);
        if let Some((_, old)) = entries.iter_mut().find(|(s, _)| Arc::ptr_eq(s, t)) {
            *old = (*old).max(side);
        } else {
            entries.push((t.clone(), side));
        }
    }
    if entries.len() < 2 {
        return Ok(HashMap::new());
    }
    if entries.len() > 32768 {
        return Err("too many baked texture sets to pack".into());
    }
    entries.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    let placements = loop {
        let (mut page, mut x, mut y, mut row) = (0usize, 0u32, 0u32, 0u32);
        let mut placed = Vec::new();
        for (_, size) in &entries {
            if x + size > SIDE {
                x = 0;
                y += row;
                row = 0;
            }
            if y + size > SIDE {
                page += 1;
                x = 0;
                y = 0;
                row = 0;
            }
            placed.push((page, x, y, *size));
            x += size;
            row = row.max(*size);
        }
        if page < 2 {
            break placed;
        }
        for (_, s) in &mut entries {
            *s = (*s / 2).max(16);
        }
    };
    let pages = placements.last().map_or(0, |p| p.0 + 1);
    let mut output = Vec::new();
    for _ in 0..pages {
        let bytes = SIDE as usize * SIDE as usize * 4;
        output.push(Textures {
            size: SIDE,
            color: vec![0; bytes],
            normal: vec![0; bytes],
            orm: vec![0; bytes],
            alpha: false,
        });
    }
    for ((source, _), &(page, x, y, size)) in entries.iter().zip(&placements) {
        let inner = size - BORDER * 2;
        for channel in 0..3 {
            let src = match channel {
                0 => &source.color,
                1 => &source.normal,
                _ => &source.orm,
            };
            let image = image::RgbaImage::from_raw(source.size, source.size, src.clone())
                .ok_or("invalid baked image size")?;
            let mut resized = image::imageops::resize(
                &image,
                inner,
                inner,
                image::imageops::FilterType::Triangle,
            );
            if channel == 1 {
                for p in resized.pixels_mut() {
                    let n = glam::Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32) / 127.5
                        - glam::Vec3::ONE;
                    let n = n.normalize_or(glam::Vec3::Z);
                    for i in 0..3 {
                        p[i] = ((n[i] * 0.5 + 0.5) * 255.0).round() as u8;
                    }
                }
            }
            let dst = match channel {
                0 => &mut output[page].color,
                1 => &mut output[page].normal,
                _ => &mut output[page].orm,
            };
            for dy in 0..size {
                for dx in 0..size {
                    let sx = dx.saturating_sub(BORDER).min(inner - 1);
                    let sy = dy.saturating_sub(BORDER).min(inner - 1);
                    let at = (((y + dy) * SIDE + x + dx) * 4) as usize;
                    dst[at..at + 4].copy_from_slice(&resized.get_pixel(sx, sy).0);
                }
            }
        }
    }
    let output: Vec<_> = output.into_iter().map(Arc::new).collect();
    Ok(entries
        .into_iter()
        .zip(placements)
        .map(|((source, _), (page, x, y, size))| {
            (
                key(&source),
                Tile {
                    textures: output[page].clone(),
                    offset: [
                        (x + BORDER) as f32 / SIDE as f32,
                        (y + BORDER) as f32 / SIDE as f32,
                    ],
                    scale: (size - BORDER * 2) as f32 / SIDE as f32,
                },
            )
        })
        .collect())
}
