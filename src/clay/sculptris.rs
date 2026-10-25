//! Sculptris Pro (docs/SCULPT_PLAN.md Stage 6): a stroke with a `cell`
//! makes the clay's detail finer where it lands, by creating or growing a
//! detail region (`Region`) over the stroke's box (its dabs grown by their
//! radius). The detail grid already refines inside regions, so this is
//! all it takes: the stroke then writes the finer level there.
//!
//! Merges work on boxes: a region made here stands for its inner box (the
//! box whose corners lie on its ellipsoid, the one it was made around), so
//! merging grows a region to the union of the boxes it was given and never
//! compounds the ellipsoid's margin. Every box a stroke was given stays
//! inside some region's inner box. A region set by hand stands for the box
//! around its whole ellipsoid, so what grows from it still holds all of it.
//! The detail grid carries a replaced region's finer detail into the region
//! that takes its place (`DetailGrid::set_regions`), so growing and merging
//! keep every earlier stroke at its cell.
//!
//! - A region of the same cell whose box holds the stroke's (a hand-set
//!   one: whose ellipsoid does) is kept as it is (no relayout).
//! - A same-cell region the box overlaps grows to hold both (the ellipsoid
//!   around the two boxes); regions that then overlap merge the same way.
//! - Past `MAX_REGIONS`, the two regions of one cell whose union box is
//!   smallest merge (of two cells, at the finer, only when no two regions
//!   share a cell).

use glam::Vec3;

use super::{MAX_REGIONS, Region};

/// Half sizes of the ellipsoid that holds a box of half sizes `half`
/// (the box's corners on it).
fn around(half: Vec3) -> Vec3 {
    half.max(Vec3::splat(1e-6)) * 3f32.sqrt()
}

/// The box a region stands for: the one its ellipsoid was made around
/// (`around` undone).
fn inner(r: &Region) -> (Vec3, Vec3) {
    let half = r.radii / 3f32.sqrt();
    (r.center - half, r.center + half)
}

/// The box a region's ellipsoid fits in.
fn bounds(r: &Region) -> (Vec3, Vec3) {
    (r.center - r.radii, r.center + r.radii)
}

/// Whether this module made the region (named `sculptrisN`).
fn made_here(r: &Region) -> bool {
    r.name.strip_prefix("sculptris").is_some_and(|n| n.parse::<u32>().is_ok())
}

/// The box a region stands for in merges: its inner box when made here,
/// the box around its whole ellipsoid when set by hand.
fn span(r: &Region) -> (Vec3, Vec3) {
    if made_here(r) { inner(r) } else { bounds(r) }
}

fn free_name(regions: &[Region]) -> String {
    let n = (1..).find(|n| !regions.iter().any(|r| r.name == format!("sculptris{n}"))).expect("a free name");
    format!("sculptris{n}")
}

fn region(lo: Vec3, hi: Vec3, cell: f32, name: String) -> Region {
    Region { name, center: (lo + hi) * 0.5, radii: around((hi - lo) * 0.5), cell }
}

/// Whether region `r` holds the box `lo..hi` (to rounding): its inner box
/// does when made here, its ellipsoid when set by hand.
fn holds(r: &Region, lo: Vec3, hi: Vec3) -> bool {
    if !made_here(r) {
        let corner = |c: usize| Vec3::new(if c & 1 == 0 { lo.x } else { hi.x }, if c & 2 == 0 { lo.y } else { hi.y }, if c & 4 == 0 { lo.z } else { hi.z });
        return (0..8).all(|c| ((corner(c) - r.center) / r.radii).length() <= 1.0 + 1e-5);
    }
    let (a, b) = inner(r);
    let slack = (b - a).max_element() * 1e-5;
    (a - slack).cmple(lo).all() && hi.cmple(b + slack).all()
}

fn overlap(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    a.0.cmple(b.1).all() && b.0.cmple(a.1).all()
}

fn same(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs())
}

/// The volume of the box around two boxes.
fn union_volume(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> f64 {
    let s = a.1.max(b.1) - a.0.min(b.0);
    s.x as f64 * s.y as f64 * s.z as f64
}

/// The regions after a stroke at `cell` over the box `lo..hi`.
pub fn after_stroke(regions: &[Region], lo: Vec3, hi: Vec3, cell: f32) -> Vec<Region> {
    if regions.iter().any(|r| same(r.cell, cell) && holds(r, lo, hi)) {
        return regions.to_vec();
    }
    let mut out = regions.to_vec();
    // The stroke's box, grown by the box of every same-cell region it
    // overlaps (and what those then overlap).
    let (mut a, mut b) = (lo, hi);
    loop {
        let Some(k) = out.iter().position(|r| same(r.cell, cell) && overlap(bounds(r), (a, b))) else { break };
        let r = out.remove(k);
        let (c, d) = span(&r);
        (a, b) = (a.min(c), b.max(d));
    }
    let name = free_name(&out);
    out.push(region(a, b, cell, name));
    while out.len() > MAX_REGIONS {
        // The pair whose union box is smallest, of one cell if any two
        // regions share one (ties: the first pair).
        let mut best: Option<(bool, f64, usize, usize)> = None;
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let key = (!same(out[i].cell, out[j].cell), union_volume(span(&out[i]), span(&out[j])));
                if best.is_none_or(|(c, v, _, _)| (key.0, key.1).partial_cmp(&(c, v)) == Some(std::cmp::Ordering::Less)) {
                    best = Some((key.0, key.1, i, j));
                }
            }
        }
        let (_, _, i, j) = best.expect("two regions");
        let second = out.remove(j);
        let first = out.remove(i);
        let ((c, d), (e, f)) = (span(&first), span(&second));
        // A hand-set region merged becomes one made here (its box is now
        // the inner one).
        let name = [&first, &second].into_iter().find(|r| made_here(r)).map_or_else(|| free_name(&out), |r| r.name.clone());
        out.push(region(c.min(e), d.max(f), first.cell.min(second.cell), name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_strokes_stay_within_sixteen_regions_that_cover_them() {
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut regions: Vec<Region> = Vec::new();
        let mut boxes = Vec::new();
        for k in 0..30 {
            let c = Vec3::new(next() * 2.0 - 1.0, next() * 2.0, next() * 2.0 - 1.0);
            let half = Vec3::new(0.02 + next() * 0.05, 0.02 + next() * 0.05, 0.02 + next() * 0.05);
            let cell = [0.002, 0.004, 0.003][k % 3];
            regions = after_stroke(&regions, c - half, c + half, cell);
            boxes.push((c - half, c + half));
            assert!(regions.len() <= MAX_REGIONS);
            for r in &regions {
                r.validate(0.01).unwrap();
            }
            // Every box so far lies in some region.
            for (lo, hi) in &boxes {
                assert!(regions.iter().any(|r| holds(r, *lo, *hi)), "box {lo} {hi} lost after stroke {k}");
            }
        }
        // Full: the last strokes merged regions rather than adding more.
        assert!(regions.len() > MAX_REGIONS / 2, "{}", regions.len());
    }

    #[test]
    fn a_drag_of_overlapping_strokes_grows_its_region_to_their_union_only() {
        // Ten strokes along a crease, each half a radius past the last: the
        // region is the ellipsoid around their union box, not 1.73^10 times
        // bigger.
        let (r, cell) = (0.03f32, 0.002);
        let mut regions = Vec::new();
        for k in 0..10 {
            let c = Vec3::new(k as f32 * r * 0.5, 0.0, 0.0);
            regions = after_stroke(&regions, c - r, c + r, cell);
            assert_eq!(regions.len(), 1);
            regions[0].validate(0.01).unwrap();
        }
        let (lo, hi) = (Vec3::splat(-r), Vec3::new(4.5 * r + r, r, r));
        let want = around((hi - lo) * 0.5);
        assert!(regions[0].radii.cmple(want * (1.0 + 1e-4)).all(), "{} for {want}", regions[0].radii);
        assert!(holds(&regions[0], lo, hi));
    }

    #[test]
    fn past_sixteen_regions_the_closest_pair_of_one_cell_merges() {
        // Seventeen far-apart strokes: one of 0.001 among 0.002s, which
        // never merges into a 0.002 region while two 0.002 regions exist.
        let mut regions = Vec::new();
        for k in 0..17 {
            let c = Vec3::new(k as f32 * 0.5, 0.0, 0.0);
            let cell = if k == 3 { 0.001 } else { 0.002 };
            regions = after_stroke(&regions, c - 0.01, c + 0.01, cell);
        }
        assert_eq!(regions.len(), MAX_REGIONS);
        assert_eq!(regions.iter().filter(|r| r.cell == 0.001).count(), 1);
        let fine = regions.iter().find(|r| r.cell == 0.001).unwrap();
        assert!(holds(fine, Vec3::new(1.49, -0.01, -0.01), Vec3::new(1.51, 0.01, 0.01)) && fine.radii.x < 0.02);
        for r in &regions {
            r.validate(0.01).unwrap();
        }
    }

    #[test]
    fn a_stroke_inside_a_region_changes_nothing_and_neighbours_merge() {
        let r = after_stroke(&[], Vec3::ZERO, Vec3::splat(0.1), 0.002);
        assert_eq!(r.len(), 1);
        assert_eq!(after_stroke(&r, Vec3::splat(0.02), Vec3::splat(0.06), 0.002), r);
        // Overlapping at the same cell: one region holding both.
        let m = after_stroke(&r, Vec3::splat(0.08), Vec3::splat(0.2), 0.002);
        assert_eq!(m.len(), 1);
        assert!(holds(&m[0], Vec3::ZERO, Vec3::splat(0.2)));
        // Another cell: a region of its own.
        assert_eq!(after_stroke(&m, Vec3::splat(0.05), Vec3::splat(0.07), 0.001).len(), 2);
    }
    #[test]
    fn a_hand_set_region_a_stroke_pokes_out_of_grows_to_hold_all_of_it() {
        let face = Region { name: "face".into(), center: Vec3::ZERO, radii: Vec3::ONE, cell: 0.002 };
        // Inside its ellipsoid (though outside its inner box): no change.
        let r = after_stroke(std::slice::from_ref(&face), Vec3::new(0.8, -0.05, -0.05), Vec3::new(0.9, 0.05, 0.05), 0.002);
        assert_eq!(r, vec![face.clone()]);
        // Poking out: the region that takes its place holds its whole
        // ellipsoid and the stroke.
        let r = after_stroke(std::slice::from_ref(&face), Vec3::new(0.9, -0.1, -0.1), Vec3::new(3.0, 0.1, 0.1), 0.002);
        assert_eq!(r.len(), 1);
        assert!(made_here(&r[0]) && holds(&r[0], Vec3::new(0.9, -0.1, -0.1), Vec3::new(3.0, 0.1, 0.1)));
        assert!(holds(&r[0], -Vec3::ONE, Vec3::ONE), "{:?}", r[0]);
    }
}
