//! Image measures for `compare_reference` and `quality_report`
//! (docs/QUALITY_PLAN.md Stage 2): the distance between two outlines, from
//! exact Euclidean distance transforms (Felzenszwalb and Huttenlocher's
//! lower envelope of parabolas, one pass per axis), and feature lines
//! (Canny edges) compared the same way. IoU hides a thin missing part (an
//! ear, an antenna); the contour distance does not.

/// "No seed" in a squared distance transform.
const FAR: f32 = 1e20;

/// The squared distance transform of one row or column `f` (0 at seeds,
/// `FAR` elsewhere, or a previous pass's values) into `d`.
fn edt_1d(f: &[f32], d: &mut [f32], v: &mut Vec<usize>, z: &mut Vec<f32>) {
    let n = f.len();
    v.clear();
    z.clear();
    // The lower envelope of the parabolas rooted at the finite values.
    for q in 0..n {
        if f[q] >= FAR {
            continue;
        }
        loop {
            let Some(&p) = v.last() else {
                v.push(q);
                z.push(f32::NEG_INFINITY);
                break;
            };
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= *z.last().expect("one per parabola") {
                v.pop();
                z.pop();
                continue;
            }
            v.push(q);
            z.push(s);
            break;
        }
    }
    if v.is_empty() {
        d.fill(FAR);
        return;
    }
    let mut k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while k + 1 < v.len() && z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        *out = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// Squared distance (pixels) from every pixel to the nearest seed; `FAR`
/// everywhere without one.
pub(super) fn distance_transform(seeds: &[bool], w: usize, h: usize) -> Vec<f32> {
    let mut g: Vec<f32> = seeds.iter().map(|&s| if s { 0.0 } else { FAR }).collect();
    let (mut v, mut z) = (Vec::new(), Vec::new());
    let mut col = vec![0.0; h];
    let mut out = vec![0.0; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = g[y * w + x];
        }
        edt_1d(&col, &mut out, &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = out[y];
        }
    }
    let mut row = vec![0.0; w];
    for y in 0..h {
        edt_1d(&g[y * w..][..w], &mut row, &mut v, &mut z);
        g[y * w..][..w].copy_from_slice(&row);
    }
    g
}

/// The outline of a mask: its pixels with a 4-neighbor outside it (the
/// image's edge counts as outside).
pub(super) fn boundary(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let at = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && mask[y as usize * w + x as usize];
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            mask[i] && !(at(x - 1, y) && at(x + 1, y) && at(x, y - 1) && at(x, y + 1))
        })
        .collect()
}

/// How far two outlines are apart, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Contour {
    /// The mean distance from each outline's pixels to the other outline,
    /// averaged over both ways.
    pub mean: f32,
    /// The larger of the two ways' 95th percentiles: a part one outline
    /// has and the other lacks (a thin ear) shows here.
    pub p95: f32,
}

/// The distances from the outline pixels of `from` to the outline whose
/// squared distance transform is `to`.
fn directed(from: &[bool], to: &[f32]) -> Vec<f32> {
    from.iter().zip(to).filter(|(b, _)| **b).map(|(_, d)| d.sqrt()).collect()
}

fn p95(mut d: Vec<f32>) -> f32 {
    d.sort_by(f32::total_cmp);
    d[((d.len() - 1) as f32 * 0.95).round() as usize]
}

/// The contour distance between a reference and a model mask of `w` x
/// `h` pixels; None when either is empty.
pub(super) fn contour(reference: &[bool], model: &[bool], w: usize, h: usize) -> Option<Contour> {
    let (a, b) = (boundary(reference, w, h), boundary(model, w, h));
    let (da, db) = (distance_transform(&a, w, h), distance_transform(&b, w, h));
    let (ab, ba) = (directed(&a, &db), directed(&b, &da));
    if ab.is_empty() || ba.is_empty() {
        return None;
    }
    let mean = |d: &[f32]| d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64;
    Some(Contour { mean: ((mean(&ab) + mean(&ba)) * 0.5) as f32, p95: p95(ab).max(p95(ba)) })
}

/// A mask shrunk by `r` pixels (pixels at least `r` from outside it).
pub(super) fn erode(mask: &[bool], w: usize, h: usize, r: f32) -> Vec<bool> {
    let outside: Vec<bool> = mask.iter().map(|m| !m).collect();
    let d = distance_transform(&outside, w, h);
    // With no pixel outside, the image's edge is the outside.
    let edge = |i: usize| ((i % w).min(w - 1 - i % w).min(i / w).min(h - 1 - i / w) + 1) as f32;
    (0..w * h).map(|i| mask[i] && d[i].min(edge(i).powi(2)) > r * r).collect()
}

/// A separable Gaussian blur (`sigma` pixels; edges clamp).
fn blur(img: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return img.to_vec();
    }
    let r = (3.0 * sigma).ceil() as isize;
    let k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / sum).collect();
    let mut tmp = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            tmp[y * w + x] = (-r..=r).map(|i| k[(i + r) as usize] * img[y * w + (x as isize + i).clamp(0, w as isize - 1) as usize]).sum();
        }
    }
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = (-r..=r).map(|i| k[(i + r) as usize] * tmp[(y as isize + i).clamp(0, h as isize - 1) as usize * w + x]).sum();
        }
    }
    out
}

/// Canny edges of a scalar image: a Gaussian of `sigma` pixels, Sobel
/// gradients (per pixel), non-maximum suppression across the edge, then
/// hysteresis: edges at least `high`, and those at least `low` connected
/// to them. Sequential, so always the same.
pub(super) fn canny(img: &[f32], w: usize, h: usize, sigma: f32, low: f32, high: f32) -> Vec<bool> {
    let g = blur(img, w, h, sigma);
    let at = |x: isize, y: isize| g[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let mut mag = vec![0.0f32; w * h];
    let mut dir = vec![0u8; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1)) / 8.0;
            let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1)) / 8.0;
            let i = y as usize * w + x as usize;
            mag[i] = (gx * gx + gy * gy).sqrt();
            // The gradient's direction in four bins (0, 45, 90, 135 degrees).
            let a = gy.atan2(gx).to_degrees().rem_euclid(180.0);
            dir[i] = (((a + 22.5) / 45.0) as u8) % 4;
        }
    }
    let m = |x: isize, y: isize| if x < 0 || y < 0 || x >= w as isize || y >= h as isize { 0.0 } else { mag[y as usize * w + x as usize] };
    let mut thin = vec![0.0f32; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            let (dx, dy) = [(1, 0), (1, 1), (0, 1), (-1, 1)][dir[i] as usize];
            if mag[i] >= m(x + dx, y + dy) && mag[i] >= m(x - dx, y - dy) {
                thin[i] = mag[i];
            }
        }
    }
    let mut out = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| thin[i] >= high && thin[i] > 0.0).collect();
    for &i in &stack {
        out[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if !out[j] && thin[j] >= low && thin[j] > 0.0 {
                out[j] = true;
                stack.push(j);
            }
        }
    }
    out
}

/// Feature lines (two line maps of one crop) compared, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Features {
    /// The mean distance from each map's line pixels to the other's
    /// nearest, averaged over both ways.
    pub chamfer: f32,
    /// The share of reference line pixels within `NEAR` of a model line.
    pub recall: f32,
    /// The share of model line pixels within `NEAR` of a reference line.
    pub precision: f32,
    pub reference_px: usize,
    pub model_px: usize,
}

/// Lines this close (pixels) count as matched.
pub(super) const NEAR: f32 = 2.0;

/// The feature compare of two line maps; None when the reference has no
/// line (nothing to match). A model with no line where the reference has
/// some matches none of them: recall and precision 0, and the chamfer the
/// crop's diagonal (farther than any line in it).
pub(super) fn features(reference: &[bool], model: &[bool], w: usize, h: usize) -> Option<Features> {
    let (dr, dm) = (distance_transform(reference, w, h), distance_transform(model, w, h));
    let (rm, mr) = (directed(reference, &dm), directed(model, &dr));
    if rm.is_empty() {
        return None;
    }
    if mr.is_empty() {
        return Some(Features { chamfer: (w as f32).hypot(h as f32), recall: 0.0, precision: 0.0, reference_px: rm.len(), model_px: 0 });
    }
    let mean = |d: &[f32]| d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64;
    let share = |d: &[f32]| d.iter().filter(|x| **x <= NEAR).count() as f32 / d.len() as f32;
    Some(Features { chamfer: ((mean(&rm) + mean(&mr)) * 0.5) as f32, recall: share(&rm), precision: share(&mr), reference_px: rm.len(), model_px: mr.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
        (0..w * h).map(|i| ((i % w) as f32 + 0.5 - cx).powi(2) + ((i / w) as f32 + 0.5 - cy).powi(2) <= r * r).collect()
    }

    /// The transform is exact: against brute force on scattered seeds.
    #[test]
    fn distance_transform_is_exact() {
        let (w, h) = (37, 23);
        let seeds: Vec<bool> = (0..w * h).map(|i| (i * 7919) % 97 == 0).collect();
        let d = distance_transform(&seeds, w, h);
        for i in 0..w * h {
            let brute = (0..w * h).filter(|&j| seeds[j]).map(|j| ((i % w) as f32 - (j % w) as f32).powi(2) + ((i / w) as f32 - (j / w) as f32).powi(2)).fold(f32::MAX, f32::min);
            assert_eq!(d[i], brute, "pixel {i}");
        }
        assert!(distance_transform(&vec![false; 12], 4, 3).iter().all(|d| *d >= FAR));
    }

    /// A known offset gives a known distance: two discs 6 pixels apart are
    /// 2 x 6 / pi apart on average and about 6 at worst.
    #[test]
    fn contour_reads_a_known_offset() {
        let (w, h) = (256, 256);
        let a = disc(w, h, 128.0, 128.0, 80.0);
        let b = disc(w, h, 134.0, 128.0, 80.0);
        let c = contour(&a, &b, w, h).unwrap();
        let mean = 2.0 * 6.0 / std::f32::consts::PI;
        println!("offset 6 px: mean {:.3} (expected {mean:.3}), p95 {:.3}", c.mean, c.p95);
        assert!((c.mean - mean).abs() < 0.4, "{c:?}");
        assert!(c.p95 > 5.0 && c.p95 <= 6.5, "{c:?}");
        let same = contour(&a, &a, w, h).unwrap();
        assert_eq!((same.mean, same.p95), (0.0, 0.0));
        assert!(contour(&a, &vec![false; w * h], w, h).is_none());
    }

    /// Canny finds a step's edge once, where it is, and not in flat
    /// regions; erosion keeps what is far from the outside.
    #[test]
    fn canny_finds_a_step_once() {
        let (w, h) = (64, 48);
        let img: Vec<f32> = (0..w * h).map(|i| if i % w >= 30 { 1.0 } else { 0.0 }).collect();
        let e = canny(&img, w, h, 1.0, 0.04, 0.08);
        for y in 4..h - 4 {
            let xs: Vec<usize> = (0..w).filter(|&x| e[y * w + x]).collect();
            assert!(!xs.is_empty() && xs.iter().all(|&x| (29..=30).contains(&x)), "row {y}: {xs:?}");
        }
        assert!(canny(&vec![0.5; w * h], w, h, 1.0, 0.04, 0.08).iter().all(|e| !e));
        let m = erode(&vec![true; w * h], w, h, 2.0);
        assert!(!m[w + 1] && m[2 * w + 2]);
        let lines: Vec<bool> = (0..w * h).map(|i| i % w == 10).collect();
        let shifted: Vec<bool> = (0..w * h).map(|i| i % w == 13).collect();
        let f = features(&lines, &shifted, w, h).unwrap();
        assert_eq!((f.chamfer, f.recall, f.precision), (3.0, 0.0, 0.0));
        let f = features(&lines, &lines, w, h).unwrap();
        assert_eq!((f.chamfer, f.recall, f.precision), (0.0, 1.0, 1.0));
        // A model without lines matches none of the reference's; without
        // reference lines there is nothing to compare.
        let none = vec![false; w * h];
        let f = features(&lines, &none, w, h).unwrap();
        assert_eq!((f.recall, f.precision, f.model_px, f.reference_px), (0.0, 0.0, 0, h), "{f:?}");
        assert!(f.chamfer >= w as f32, "{f:?}");
        assert!(features(&none, &lines, w, h).is_none());
    }

    /// A thin antenna the model lacks barely moves the IoU, but the
    /// contour's p95 rises by at least half its length.
    #[test]
    fn contour_distance_catches_thin_parts() {
        let (w, h) = (256, 256);
        let body = disc(w, h, 128.0, 150.0, 70.0);
        let length = 60usize;
        let mut reference = body.clone();
        // A 2-pixel antenna straight up from the top of the disc.
        for y in 80 - length..80 {
            for x in 127..129 {
                reference[y * w + x] = true;
            }
        }
        let iou = |a: &[bool], b: &[bool]| {
            let both = a.iter().zip(b).filter(|(x, y)| **x && **y).count() as f32;
            both / a.iter().zip(b).filter(|(x, y)| **x || **y).count() as f32
        };
        let with = contour(&reference, &body, w, h).unwrap();
        let without = contour(&body, &body, w, h).unwrap();
        println!("antenna {length} px: IoU {:.4}, contour mean {:.3} p95 {:.3} (matching: {:?})", iou(&reference, &body), with.mean, with.p95, without);
        assert!(1.0 - iou(&reference, &body) < 0.01);
        assert!(with.p95 - without.p95 >= 0.5 * length as f32, "{with:?}");
    }
}
