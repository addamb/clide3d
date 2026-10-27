//! The brushes: one dab's new field values on a box of grid points.

use glam::{IVec3, Vec3};

use super::engine::{BUILDUP, Gate, Sweep, allowed, closest, closest_normal, hook_source, stretch_source};
use super::flow;
use super::noise::smooth01;
use crate::brush::alpha::{Stamp, Use};
use crate::brush::resolve::Params;
use crate::brush::spec::{Brush, Side, SmoothMode, StrokeMask};
use crate::clay::{Field, LevelView, MAX_OFFSET_VOXELS, par_chunks};

/// One dab's brush, applied region by region.
pub(super) struct Job<'a> {
    pub params: &'a Params,
    pub sweep: &'a Sweep<'a>,
    pub mask: Option<&'a StrokeMask>,
    /// The persistent mask, polygroups and auto-masking.
    pub gate: &'a Gate,
    /// The dab's own normal, when it gives one (auto-masking's reference).
    pub dab_normal: Option<Vec3>,
    pub field: &'a Field,
    /// The detail offsets before the dab.
    pub grid: &'a LevelView<'a>,
    pub voxel: f32,
    pub radius: f32,
    pub strength: f32,
    pub delta: Vec3,
    pub smooth_passes: usize,
    /// flatten, crease, layer, pinch: the surface normal; standard: the
    /// area normal.
    pub normal: Vec3,
    /// standard: the signed height at the dab's center; clay tubes: the
    /// strip's height.
    pub height: f32,
    /// Dab brushes: the dab's frame across the surface (u, v).
    pub frame: (Vec3, Vec3),
    /// The dab's alpha, placed.
    pub stamp: Option<Stamp<'a>>,
    /// vdm: the dab's VDM stamp, placed on the surface.
    pub vdm: Option<crate::brush::vdm::Stamp<'a>>,
    /// The plane brushes: the area's mean surface, along `normal` from the
    /// dab's center.
    pub plane: f32,
    /// Clay tubes: the surface normal at each path point.
    pub normals: Option<&'a [Vec3]>,
}

impl Job<'_> {
    /// Reads the field on grid points `i0..=i1`, applies the brush, and
    /// returns the new offsets of changed points within `c0..=c1`.
    pub fn region(&self, i0: IVec3, i1: IVec3, c0: IVec3, c1: IVec3) -> Vec<([i32; 3], f32)> {
        let (params, field, voxel, radius, strength, delta) = (self.params, self.field, self.voxel, self.radius, self.strength, self.delta);
        let (sweep, mask, curve) = (self.sweep, self.mask, &self.params.curve);
        let points = sweep.points;
        let limit = field.limit;
        let n = (i1 - i0 + 1).max(IVec3::ONE);
        let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
        let point = |x: usize, y: usize, z: usize| (i0 + IVec3::new(x as i32, y as i32, z as i32)).as_vec3() * voxel;
        let idx = |x: usize, y: usize, z: usize| x + nx * (y + ny * z);

        // Current blob field and total field on the region.
        let near = field.near(point(0, 0, 0) - voxel - delta.length(), point(nx - 1, ny - 1, nz - 1) + voxel + delta.length());
        let mut base = vec![0.0f32; nx * ny * nz];
        field.base_block(i0, n, voxel, &near, &mut base);
        // Per point: the total field, the brush weight, the distance to the
        // path, and (where they vary along the path) the radius and
        // strength factor there; written z slice by z slice in parallel,
        // each point from its own inputs only.
        let varies = !sweep.uniform();
        let len = base.len();
        let (mut t, mut w, mut dist) = (vec![0.0f32; len], vec![0.0f32; len], vec![0.0f32; len]);
        let (mut rad, mut gain) = if varies { (vec![radius; len], vec![1.0f32; len]) } else { (Vec::new(), Vec::new()) };
        {
            let slice = nx * ny;
            let mut slices: Vec<[&mut [f32]; 5]> = Vec::with_capacity(nz);
            let (mut a, mut b, mut c) = (&mut t[..], &mut w[..], &mut dist[..]);
            let (mut r, mut k) = (&mut rad[..], &mut gain[..]);
            for _ in 0..nz {
                let (a0, a1) = std::mem::take(&mut a).split_at_mut(slice);
                let (b0, b1) = std::mem::take(&mut b).split_at_mut(slice);
                let (c0, c1) = std::mem::take(&mut c).split_at_mut(slice);
                let (r0, r1) = std::mem::take(&mut r).split_at_mut(if varies { slice } else { 0 });
                let (k0, k1) = std::mem::take(&mut k).split_at_mut(if varies { slice } else { 0 });
                slices.push([a0, b0, c0, r0, k0]);
                (a, b, c, r, k) = (a1, b1, c1, r1, k1);
            }
            par_chunks(&mut slices, 1, |z, s| {
                let [ts, ws, ds, rs, ks] = &mut s[0];
                for y in 0..ny {
                    let row = nx * y;
                    // The grid's offsets along the row, a brick at a time.
                    self.grid.row((i0 + IVec3::new(0, y as i32, z as i32)).to_array(), &mut ts[row..row + nx]);
                    for x in 0..nx {
                        let at = row + x;
                        ts[at] = base[idx(x, y, z)] + ts[at];
                        let (d, r, k) = sweep.locate(point(x, y, z));
                        ws[at] = if allowed(mask, field, point(x, y, z)) { curve.weight(d, r) * k } else { 0.0 };
                        ds[at] = d;
                        if varies {
                            (rs[at], ks[at]) = (r, k);
                        }
                    }
                }
            });
        }
        let (rad, gain) = (rad, gain);
        let (t, w, dist) = (t, w, dist);
        let rad_at = |i: usize| if varies { rad[i] } else { radius };
        let strength_at = |i: usize| if varies { strength * gain[i] } else { strength };

        let mut out = t.clone();
        match params.brush {
            Brush::Inflate | Brush::Deflate => {
                let sign = if params.brush == Brush::Inflate { -1.0 } else { 1.0 };
                par_chunks(&mut out, nx * ny, |z, layer| {
                    for (j, o) in layer.iter_mut().enumerate() {
                        let i = z * nx * ny + j;
                        *o += sign * w[i] * strength * rad_at(i) * 0.5;
                    }
                });
            }
            Brush::Add | Brush::Carve => {
                for i in 0..out.len() {
                    let r = rad_at(i);
                    let k = r * 0.5;
                    let tube = dist[i] - r;
                    let d = t[i];
                    let v = if params.brush == Brush::Add {
                        let h = (0.5 + 0.5 * (d - tube) / k).clamp(0.0, 1.0);
                        d * (1.0 - h) + tube * h - k * h * (1.0 - h)
                    } else {
                        let h = (0.5 - 0.5 * (d + tube) / k).clamp(0.0, 1.0);
                        d * (1.0 - h) - tube * h + k * h * (1.0 - h)
                    };
                    out[i] = d + (v - d) * strength_at(i);
                }
            }
            Brush::Crease | Brush::Layer | Brush::Pinch | Brush::Stretch | Brush::Standard | Brush::Snakehook => {
                let c = points[0];
                // Where a point's source may land for the brushes that move
                // it at most a known distance: their prims are looked up in
                // this region's list (the same list, found faster).
                let shift = match params.brush {
                    Brush::Standard => Some(self.height.abs()),
                    Brush::Crease | Brush::Layer => Some(strength.abs()),
                    _ => None,
                };
                let e = Vec3::splat(voxel);
                let hull = shift.map(|h| (point(0, 0, 0) - e - h, point(nx - 1, ny - 1, nz - 1) + e + h));
                let cands = hull.map(|(lo, hi)| field.near(lo, hi)).unwrap_or_default();
                // Each point reads only the pre-dab field: split by z slice.
                par_chunks(&mut out, nx * ny, |z, layer| {
                    let mut list = Vec::with_capacity(cands.len());
                    for y in 0..ny {
                        for x in 0..nx {
                            let i = idx(x, y, z);
                            let p = point(x, y, z);
                            if !allowed(mask, field, p) {
                                continue;
                            }
                            let sign = if params.invert { -1.0 } else { 1.0 };
                            let (r, s) = (rad_at(i), strength_at(i));
                            let source = match params.brush {
                                Brush::Crease => {
                                    let v = p - closest(p, points);
                                    let side = (v - self.normal * v.dot(self.normal)).length();
                                    let band = 1.0 - smooth01((v.dot(self.normal).abs() - s.abs() - voxel * 2.0) / r);
                                    p + self.normal * (sign * s * (1.0 - side / r).max(0.0) * band)
                                }
                                // One target per stroke, rather than accumulating dabs.
                                Brush::Layer => {
                                    let v = p - closest(p, points);
                                    let side = (v - self.normal * v.dot(self.normal)).length();
                                    let band = 1.0 - smooth01((v.dot(self.normal).abs() - s.abs() - voxel * 2.0) / r);
                                    p - self.normal * (sign * s * (1.0 - smooth01((side / r - 0.65) / 0.35)) * band)
                                }
                                Brush::Pinch => {
                                    let q = closest(p, points);
                                    let n = self.normal;
                                    let side = p - q - n * (p - q).dot(n);
                                    p + side * w[i] * strength * 2.0
                                }
                                Brush::Stretch => stretch_source(p, points, delta, radius),
                                Brush::Snakehook => hook_source(p, points, radius),
                                // Moves the surface along the area normal by
                                // the falloff of the distance across it.
                                _ => {
                                    let v = p - c;
                                    let along = v.dot(self.normal);
                                    let across = (v - self.normal * along).length();
                                    let band = 1.0 - smooth01((along.abs() - self.height.abs() - voxel * 2.0) / radius);
                                    let mut h = self.height * curve.weight(across, radius) * band;
                                    if let Some(st) = &self.stamp {
                                        h *= match st.params.usage {
                                            Use::Height => st.height(p),
                                            Use::Mask => st.mask(p),
                                        };
                                    }
                                    if h == 0.0 {
                                        continue;
                                    }
                                    p - self.normal * h
                                }
                            };
                            let (lo, hi) = (source - e, source + e);
                            match hull {
                                Some((a, b)) if lo.cmpge(a).all() && hi.cmple(b).all() => field.near_from(lo, hi, &cands, &mut list),
                                _ => list = field.near(lo, hi),
                            }
                            layer[x + nx * y] = field.sample(source, &list).0;
                        }
                    }
                });
            }
            Brush::Clay | Brush::ClayBuildup | Brush::TrimDynamic | Brush::Polish => {
                // Toward the plane of the area under the dab: clay and
                // buildup only fill (invert: only cut), trim only cuts,
                // polish both ways near the surface.
                let (c, n, (u, v)) = (points[0], self.normal, self.frame);
                // Buildup fills hollows to the plane at once and lays a
                // layer over it: its strength (scaled along a path, as
                // standard's) sets the layer's height.
                let (lerp, lift) = if params.brush == Brush::ClayBuildup { (1.0, radius * BUILDUP * strength) } else { (strength, 0.0) };
                let square = params.brush.square();
                par_chunks(&mut out, nx * ny, |z, layer| {
                    for y in 0..ny {
                        for x in 0..nx {
                            let p = point(x, y, z);
                            let d = p - c;
                            let along = d.dot(n);
                            if along.abs() > radius * 1.5 || !allowed(mask, field, p) {
                                continue;
                            }
                            let (a, b) = (d.dot(u), d.dot(v));
                            let across = if square { a.abs().max(b.abs()) } else { (a * a + b * b).sqrt() };
                            let mut wt = curve.weight(across, radius) * lerp;
                            if let Some(st) = &self.stamp {
                                wt *= st.mask(p);
                            }
                            let t0 = t[idx(x, y, z)];
                            if wt <= 0.0 || (params.brush == Brush::Polish && t0.abs() > radius) {
                                continue;
                            }
                            let above = along - self.plane;
                            let target = match (params.brush, params.invert) {
                                (Brush::Clay | Brush::ClayBuildup, false) => t0.min(above - lift),
                                (Brush::Clay | Brush::ClayBuildup, true) => t0.max(above + lift),
                                (Brush::TrimDynamic, _) => t0.max(above),
                                _ => above,
                            };
                            layer[x + nx * y] = t0 + (target - t0) * wt;
                        }
                    }
                });
            }
            Brush::ClayTubes => {
                // A strip of even height over the path's own surface:
                // overlapping dabs aim at the same height, so it stays even.
                let normals = self.normals.expect("clay tubes have per-point normals");
                let sign = if params.invert { -1.0 } else { 1.0 };
                par_chunks(&mut out, nx * ny, |z, layer| {
                    for y in 0..ny {
                        for x in 0..nx {
                            let i = idx(x, y, z);
                            let p = point(x, y, z);
                            if !allowed(mask, field, p) {
                                continue;
                            }
                            let (q, n) = closest_normal(p, points, normals);
                            let d = p - q;
                            let along = d.dot(n);
                            let (r, h) = (rad_at(i), sign * strength_at(i) * rad_at(i) * 0.5);
                            if along.abs() > r + h.abs() + voxel * 2.0 {
                                continue;
                            }
                            let wt = curve.weight((d - n * along).length(), r);
                            if wt <= 0.0 {
                                continue;
                            }
                            let t0 = t[i];
                            let target = if h >= 0.0 { t0.min(along - h) } else { t0.max(along - h) };
                            layer[x + nx * y] = t0 + (target - t0) * wt;
                        }
                    }
                });
            }
            Brush::Smooth => {
                let gentle = params.smooth_mode == SmoothMode::Gentle;
                for _ in 0..self.smooth_passes {
                    let prev = out.clone();
                    for z in 1..nz.saturating_sub(1) {
                        for y in 1..ny.saturating_sub(1) {
                            for x in 1..nx.saturating_sub(1) {
                                let i = idx(x, y, z);
                                if w[i] <= 0.0 {
                                    continue;
                                }
                                let sum = prev[idx(x - 1, y, z)] + prev[idx(x + 1, y, z)] + prev[idx(x, y - 1, z)] + prev[idx(x, y + 1, z)] + prev[idx(x, y, z - 1)] + prev[idx(x, y, z + 1)];
                                let avg = (sum + prev[i]) / 7.0;
                                let v = if gentle { prev[i] + (avg - prev[i]) * w[i] * 0.5 } else { prev[i] + (avg - prev[i]) * w[i] };
                                // The field's Laplacian sign picks the side:
                                // where the average is above the value the
                                // surface is a dip, below it a bump.
                                out[i] = match params.smooth_mode {
                                    SmoothMode::Peaks => v.max(prev[i]),
                                    SmoothMode::Valleys => v.min(prev[i]),
                                    _ => v,
                                };
                            }
                        }
                    }
                }
            }
            Brush::ClayPolish => {
                // Curvature flow that keeps creases and lines, the dab's
                // weight as its falloff, strength setting the passes.
                let s = flow::Settings { angle: params.angle.unwrap_or(30.0), keep_volume: params.keep_volume.unwrap_or(true), passes: self.smooth_passes };
                out = flow::polish_box(&t, n, &w, voxel, &s);
            }
            Brush::Flatten => {
                let c = points.iter().copied().sum::<Vec3>() / points.len() as f32;
                for z in 0..nz {
                    for y in 0..ny {
                        for x in 0..nx {
                            let i = idx(x, y, z);
                            let plane = (point(x, y, z) - c).dot(self.normal);
                            let v = t[i] + (plane - t[i]) * w[i] * strength;
                            out[i] = match params.side {
                                Side::Both => v,
                                // Only cut down (the field rises) or only fill.
                                Side::Above => v.max(t[i]),
                                Side::Below => v.min(t[i]),
                            };
                        }
                    }
                }
            }
            Brush::Vdm => {
                // The stamp's field where it reaches (`vdm::Stamp::field`),
                // by the dab's strength (pressure, taper): each point from
                // the pre-dab field at that point alone.
                let st = self.vdm.as_ref().expect("vdm dabs have a stamp");
                par_chunks(&mut out, nx * ny, |z, layer| {
                    for y in 0..ny {
                        for x in 0..nx {
                            let i = idx(x, y, z);
                            let p = point(x, y, z);
                            if !allowed(mask, field, p) {
                                continue;
                            }
                            if let Some(v) = st.field(p, t[i]) {
                                let k = strength_at(i).clamp(0.0, 1.0);
                                layer[x + nx * y] = t[i] + (v - t[i]) * k;
                            }
                        }
                    }
                });
            }
            // Handled before the region is read (see `noise_band`); paint
            // strokes do not sculpt.
            Brush::Noise | Brush::Paint | Brush::Transform | Brush::MaskPaint | Brush::GroupPaint => {}
            Brush::Grab => {
                // Each point takes the old value from where the pull came
                // from: from the region when it is there, else the field.
                let sample = |p: Vec3| -> f32 {
                    let g = p / voxel - i0.as_vec3();
                    if g.cmplt(Vec3::ZERO).any() || g.cmpgt((n - 1).as_vec3()).any() {
                        return field.sample(p, &near).0;
                    }
                    let f = g.floor();
                    let (fx, fy, fz) = (f.x as usize, f.y as usize, f.z as usize);
                    let (x1, y1, z1) = ((fx + 1).min(nx - 1), (fy + 1).min(ny - 1), (fz + 1).min(nz - 1));
                    let r = g - f;
                    let l = |a: f32, b: f32, s: f32| a + (b - a) * s;
                    let c00 = l(t[idx(fx, fy, fz)], t[idx(x1, fy, fz)], r.x);
                    let c10 = l(t[idx(fx, y1, fz)], t[idx(x1, y1, fz)], r.x);
                    let c01 = l(t[idx(fx, fy, z1)], t[idx(x1, fy, z1)], r.x);
                    let c11 = l(t[idx(fx, y1, z1)], t[idx(x1, y1, z1)], r.x);
                    l(l(c00, c10, r.y), l(c01, c11, r.y), r.z)
                };
                let anchor = points[0];
                let g0 = sweep.gains.map_or(1.0, |g| g[0]);
                for z in 0..nz {
                    for y in 0..ny {
                        for x in 0..nx {
                            let p = point(x, y, z);
                            let wg = curve.weight((p - anchor).length(), radius);
                            let wg = if varies { wg * g0 } else { wg };
                            if wg > 0.0 {
                                out[idx(x, y, z)] = sample(p - delta * wg);
                            }
                        }
                    }
                }
            }
        }

        let max_off = MAX_OFFSET_VOXELS * voxel;
        // Each point keeps 1 - gate of its change (the persistent mask,
        // groups and auto-masking); without one it is as it was.
        let auto = self.auto(i0, n, &t);
        let gated = self.gate.points() || auto.is_some();
        // Z slice by z slice in parallel, joined in z order: the same list
        // in the same order as one pass.
        let mut slices: Vec<Vec<([i32; 3], f32)>> = vec![Vec::new(); nz];
        par_chunks(&mut slices, 1, |z, s| {
            let changes = &mut s[0];
            for y in 0..ny {
                for x in 0..nx {
                    let gi = i0 + IVec3::new(x as i32, y as i32, z as i32);
                    let i = idx(x, y, z);
                    if gi.cmplt(c0).any() || gi.cmpgt(c1).any() || (out[i] - t[i]).abs() <= 1e-7 || !allowed(mask, field, point(x, y, z)) {
                        continue;
                    }
                    let mut v = out[i];
                    if gated {
                        // Off the surface the gate is read where the surface is.
                        let (xi, yi, zi) = (x as i64, y as i64, z as i64);
                        let at = |dx: i64, dy: i64, dz: i64| t[idx((xi + dx).clamp(0, nx as i64 - 1) as usize, (yi + dy).clamp(0, ny as i64 - 1) as usize, (zi + dz).clamp(0, nz as i64 - 1) as usize)];
                        let normal = Vec3::new(at(1, 0, 0) - at(-1, 0, 0), at(0, 1, 0) - at(0, -1, 0), at(0, 0, 1) - at(0, 0, -1)).try_normalize();
                        let k = self.gate.at_near(field, point(x, y, z), t[i], normal) * auto.as_ref().map_or(1.0, |a| a[i]);
                        if k <= 0.0 {
                            continue;
                        }
                        if k < 1.0 {
                            v = t[i] + (out[i] - t[i]) * k;
                            if (v - t[i]).abs() <= 1e-7 {
                                continue;
                            }
                        }
                    }
                    changes.push((gi.to_array(), (v.clamp(-limit, limit) - base[i]).clamp(-max_off, max_off)));
                }
            }
        });
        let mut changes = Vec::with_capacity(slices.iter().map(Vec::len).sum());
        for s in slices {
            changes.extend(s);
        }
        changes
    }

    /// Auto-masking over a region (None without it): per point, how much
    /// of the change the surface there lets through. Normals and bends
    /// come from the region's own field `t` (central differences), the
    /// reference normal from the dab (or the surface at its center), and
    /// `connected` floods the surface band (within 1.2 cells of the
    /// surface) from the dab's center within 1.5 radii: band points it does
    /// not reach, and points within two cells of them, keep still.
    fn auto(&self, i0: IVec3, n: IVec3, t: &[f32]) -> Option<Vec<f32>> {
        let m = self.gate.auto.as_ref()?;
        let (nx, ny, nz) = (n.x as usize, n.y as usize, n.z as usize);
        let idx = |x: usize, y: usize, z: usize| x + nx * (y + ny * z);
        let voxel = self.voxel;
        let at = |x: i64, y: i64, z: i64| t[idx(x.clamp(0, nx as i64 - 1) as usize, y.clamp(0, ny as i64 - 1) as usize, z.clamp(0, nz as i64 - 1) as usize)];
        let grad = |x: usize, y: usize, z: usize| {
            let (x, y, z) = (x as i64, y as i64, z as i64);
            Vec3::new(at(x + 1, y, z) - at(x - 1, y, z), at(x, y + 1, z) - at(x, y - 1, z), at(x, y, z + 1) - at(x, y, z - 1)).normalize_or_zero()
        };
        let center = self.sweep.points[0];
        let cell = ((center / voxel).round().as_ivec3() - i0).clamp(IVec3::ZERO, n - 1);
        let reference = self.dab_normal.unwrap_or_else(|| grad(cell.x as usize, cell.y as usize, cell.z as usize)).normalize_or(Vec3::Y);
        let mut k = vec![1.0f32; t.len()];
        let cos = m.normal_angle.map(|a| a.to_radians().cos());
        if m.backface || cos.is_some() || m.cavity.is_some() {
            crate::clay::par_chunks(&mut k, nx * ny, |z, layer| {
                for y in 0..ny {
                    for x in 0..nx {
                        let w = &mut layer[x + nx * y];
                        if m.backface || cos.is_some() {
                            let d = grad(x, y, z).dot(reference);
                            if (m.backface && d < 0.0) || cos.is_some_and(|c| d < c) {
                                *w = 0.0;
                                continue;
                            }
                        }
                        if let Some(amount) = m.cavity {
                            let (xi, yi, zi) = (x as i64, y as i64, z as i64);
                            let c = at(xi, yi, zi);
                            let lap = (at(xi + 1, yi, zi) - c) + (at(xi - 1, yi, zi) - c) + (at(xi, yi + 1, zi) - c) + (at(xi, yi - 1, zi) - c) + (at(xi, yi, zi + 1) - c) + (at(xi, yi, zi - 1) - c);
                            let [edge, cavity] = crate::weather::wear_of(lap / voxel);
                            *w *= if amount >= 0.0 { 1.0 - amount * cavity } else { 1.0 + amount * edge };
                        }
                    }
                }
            });
        }
        if m.connected {
            // Thin enough that surfaces a few cells apart stay apart, thick
            // enough that a tilted surface's points still touch.
            let band = 1.2 * voxel;
            let in_band = |i: usize| t[i].abs() <= band;
            let pos = |i: usize| (i0 + IVec3::new((i % nx) as i32, (i / nx % ny) as i32, (i / (nx * ny)) as i32)).as_vec3() * voxel;
            let reach = 1.5 * self.radius;
            // Seed: the band point nearest the dab's center.
            let seed = (0..t.len()).filter(|&i| in_band(i)).min_by(|&a, &b| pos(a).distance_squared(center).total_cmp(&pos(b).distance_squared(center)).then(a.cmp(&b)));
            let mut reached = vec![false; t.len()];
            if let Some(s) = seed {
                reached[s] = true;
                let mut stack = vec![s];
                while let Some(i) = stack.pop() {
                    let (x, y, z) = (i % nx, i / nx % ny, i / (nx * ny));
                    let mut push = |j: usize| {
                        if !reached[j] && in_band(j) && pos(j).distance(center) <= reach {
                            reached[j] = true;
                            stack.push(j);
                        }
                    };
                    if x > 0 { push(i - 1); }
                    if x + 1 < nx { push(i + 1); }
                    if y > 0 { push(i - nx); }
                    if y + 1 < ny { push(i + nx); }
                    if z > 0 { push(i - nx * ny); }
                    if z + 1 < nz { push(i + nx * ny); }
                }
            }
            // Band surface the flood did not reach, grown two cells.
            let mut blocked: Vec<bool> = (0..t.len()).map(|i| in_band(i) && !reached[i]).collect();
            for _ in 0..2 {
                let prev = blocked.clone();
                for (i, b) in blocked.iter_mut().enumerate() {
                    if *b {
                        continue;
                    }
                    let (x, y, z) = (i % nx, i / nx % ny, i / (nx * ny));
                    *b = (x > 0 && prev[i - 1]) || (x + 1 < nx && prev[i + 1]) || (y > 0 && prev[i - nx]) || (y + 1 < ny && prev[i + nx]) || (z > 0 && prev[i - nx * ny]) || (z + 1 < nz && prev[i + nx * ny]);
                }
            }
            for i in 0..t.len() {
                if blocked[i] && !reached[i] {
                    k[i] = 0.0;
                }
            }
        }
        Some(k)
    }
}
