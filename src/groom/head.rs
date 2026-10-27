//! A clay head that hair stays out of (`Head`), and the same head seen
//! from a hair object's space (`InHair`, a `groom::Collider`): groom keeps
//! guides out of it, film keeps strands out of it, and the hair's cards
//! are kept out of it (`Hair::build_mesh_kept_out`). Shared by the app's
//! handlers and the mesh library, which builds cards on its own threads.

use glam::{Mat4, Vec3};

use crate::clay::Clay;

/// A clay that hair rests on (see `drape_strand`) or is groomed against
/// (`app/groom.rs`).
pub struct Head {
    pub(crate) to_local: Mat4,
    pub(crate) to_world: Mat4,
    pub(crate) scale: f32,
    pub(crate) field: crate::clay::Field,
    pub(crate) list: Vec<u32>,
    pub(crate) cell: f32,
    /// Middle of its mesh (clay space): the way out from deep inside.
    pub(crate) center: Vec3,
}

impl Head {
    /// `clay` (as drawn: composed) placed at `world`, its drawn mesh's
    /// bounds `lo..hi` (clay space) limiting the blobs looked at.
    pub fn new(clay: &Clay, (lo, hi): (Vec3, Vec3), world: Mat4) -> Result<Head, String> {
        let cell = clay.cell_size().ok_or("clay is empty")?;
        let field = crate::clay::Field::new(clay, cell);
        let list = field.near(lo - 0.1, hi + 0.1);
        let scale = world.to_scale_rotation_translation().0.abs().max_element().max(1e-6);
        Ok(Head { to_local: world.inverse(), to_world: world, scale, field, list, cell, center: (lo + hi) * 0.5 })
    }

    /// `p` (world) moved out to at least `lift` outside the clay, and the
    /// surface normal (world) if it touches the clay.
    pub(crate) fn push_out(&self, p: Vec3, lift: f32) -> (Vec3, Option<Vec3>) {
        let q = self.to_local.transform_point3(p);
        let lift = lift / self.scale;
        let touch = lift * 2.0 + self.cell * 0.5;
        if self.field.far_side(q, &self.list, touch + self.cell) == Some(false) {
            return (p, None);
        }
        let d = |x: Vec3| self.field.sample(x, &self.list).0;
        let dq = d(q);
        if dq >= touch {
            return (p, None);
        }
        let e = self.cell * 0.5;
        let grad = Vec3::new(d(q + Vec3::X * e) - d(q - Vec3::X * e), d(q + Vec3::Y * e) - d(q - Vec3::Y * e), d(q + Vec3::Z * e) - d(q - Vec3::Z * e));
        // Deep inside the field is flat (it is clamped): head outward.
        let out = grad.try_normalize().unwrap_or_else(|| (q - self.center).normalize_or(Vec3::Y));
        let n = self.to_world.transform_vector3(out).normalize_or(Vec3::Y);
        (self.to_world.transform_point3(q + out * (lift - dq).max(0.0)), Some(n))
    }

    /// `p` (world) moved out until it is at least `lift` outside the clay
    /// (steps of up to the field's clamp: it is a distance only near the
    /// surface); None when it already is. The steps run in the clay's
    /// space, so a point that is clear is never moved by the round trip
    /// through the head's matrix; each step aims a hair past `lift`, so a
    /// lift of 0 still ends outside.
    pub fn push_clear(&self, p: Vec3, lift: f32) -> Option<Vec3> {
        let lift = lift / self.scale;
        let start = self.to_local.transform_point3(p);
        let d = |x: Vec3| self.field.sample(x, &self.list).0;
        if self.field.far_side(start, &self.list, lift + self.cell) == Some(false) {
            return None;
        }
        let aim = lift + self.cell * 1e-3;
        let e = self.cell * 0.5;
        let mut q = start;
        for _ in 0..32 {
            let dq = d(q);
            if dq >= lift {
                break;
            }
            let grad = Vec3::new(d(q + Vec3::X * e) - d(q - Vec3::X * e), d(q + Vec3::Y * e) - d(q - Vec3::Y * e), d(q + Vec3::Z * e) - d(q - Vec3::Z * e));
            // Deep inside the field is flat (it is clamped): head outward.
            let out = grad.try_normalize().unwrap_or_else(|| (q - self.center).normalize_or(Vec3::Y));
            q += out * (aim - dq);
        }
        (q != start).then(|| self.to_world.transform_point3(q))
    }

    /// The spacing at which segments are checked against the clay (its
    /// cell), in world units.
    pub fn step(&self) -> f32 {
        self.cell * self.scale
    }

    /// The outward normal (world) near `p`, where the field is a distance
    /// (None far from the clay).
    pub fn normal_at(&self, p: Vec3) -> Option<Vec3> {
        let q = self.to_local.transform_point3(p);
        if self.field.far_side(q, &self.list, self.cell * 4.0).is_some() {
            return None;
        }
        let d = |x: Vec3| self.field.sample(x, &self.list).0;
        let e = self.cell * 0.5;
        let grad = Vec3::new(d(q + Vec3::X * e) - d(q - Vec3::X * e), d(q + Vec3::Y * e) - d(q - Vec3::Y * e), d(q + Vec3::Z * e) - d(q - Vec3::Z * e));
        let out = grad.try_normalize()?;
        Some(self.to_world.inverse().transpose().transform_vector3(out).normalize_or(out))
    }

    /// `p` (world) moved onto the clay's surface along the field's
    /// gradient (a few Newton steps) and the outward normal there (world);
    /// None far from the clay (past four cells) or where the field is flat.
    pub fn snap(&self, p: Vec3) -> Option<(Vec3, Vec3)> {
        let mut q = self.to_local.transform_point3(p);
        let d = |x: Vec3| self.field.sample(x, &self.list).0;
        let e = self.cell * 0.5;
        let grad = |x: Vec3| Vec3::new(d(x + Vec3::X * e) - d(x - Vec3::X * e), d(x + Vec3::Y * e) - d(x - Vec3::Y * e), d(x + Vec3::Z * e) - d(x - Vec3::Z * e));
        let mut n = Vec3::ZERO;
        for _ in 0..4 {
            let dq = d(q);
            if dq.abs() > self.cell * 4.0 {
                return None;
            }
            n = grad(q).try_normalize()?;
            q -= n * dq;
        }
        (d(q).abs() < self.cell * 0.25).then(|| (self.to_world.transform_point3(q), self.to_world.inverse().transpose().transform_vector3(n).normalize_or(n)))
    }

    /// The clay's colour at `p` (world): its blobs' sRGB colour there.
    pub fn color(&self, p: Vec3) -> Vec3 {
        self.field.sample(self.to_local.transform_point3(p), &self.list).1
    }

    /// Whether the clay's surface near `p` (world) is made by a blob
    /// named in `names` (ownership as masks see it, `Field::nearest_named`).
    pub fn owned_by(&self, p: Vec3, names: &[String]) -> bool {
        !names.is_empty() && self.field.nearest_named(self.to_local.transform_point3(p), names)
    }

    /// The bone of the blob that makes the surface near `p` (world).
    pub fn owner_bone(&self, p: Vec3) -> Option<String> {
        self.field.owner_bone(self.to_local.transform_point3(p))
    }

    /// The name of the blob that makes the surface near `p` (world).
    pub fn owner_name(&self, p: Vec3) -> Option<String> {
        self.field.owner_name(self.to_local.transform_point3(p))
    }

    /// Whether `p` (world) is inside the clay.
    pub fn inside(&self, p: Vec3) -> bool {
        let q = self.to_local.transform_point3(p);
        match self.field.far_side(q, &self.list, self.cell) {
            Some(inside) => inside,
            None => self.field.sample(q, &self.list).0 < 0.0,
        }
    }
}

/// A head clay in the hair's space.
pub struct InHair<'a> {
    head: &'a Head,
    /// Hair space to world and back.
    to_world: Mat4,
    to_hair: Mat4,
    /// The hair's lift in world units.
    lift: f32,
    /// The head's cell in the hair's units.
    step: f32,
}

impl<'a> InHair<'a> {
    /// `head` seen from a hair placed at `to_world`, kept `lift` (world
    /// units) out.
    pub fn new(head: &'a Head, to_world: Mat4, lift: f32) -> Self {
        let to_hair = to_world.inverse();
        let unit = to_hair.to_scale_rotation_translation().0.abs().max_element().max(1e-9);
        InHair { head, to_world, to_hair, lift, step: head.step() * unit }
    }
}

impl crate::groom::Collider for InHair<'_> {
    fn push(&self, p: Vec3) -> Option<Vec3> {
        let q = self.head.push_clear(self.to_world.transform_point3(p), self.lift)?;
        Some(self.to_hair.transform_point3(q))
    }
    fn normal(&self, p: Vec3) -> Option<Vec3> {
        let n = self.head.normal_at(self.to_world.transform_point3(p))?;
        Some(self.to_world.transpose().transform_vector3(n).normalize_or_zero()).filter(|n| *n != Vec3::ZERO)
    }
    fn inside(&self, p: Vec3) -> bool {
        self.head.inside(self.to_world.transform_point3(p))
    }
    fn step(&self) -> f32 {
        self.step
    }
    fn snap(&self, p: Vec3) -> Option<(Vec3, Vec3)> {
        let (q, n) = self.head.snap(self.to_world.transform_point3(p))?;
        let n = self.to_world.transpose().transform_vector3(n).normalize_or_zero();
        (n != Vec3::ZERO).then(|| (self.to_hair.transform_point3(q), n))
    }
    fn color(&self, p: Vec3) -> Option<Vec3> {
        Some(self.head.color(self.to_world.transform_point3(p)))
    }
    fn owned_by(&self, p: Vec3, names: &[String]) -> bool {
        self.head.owned_by(self.to_world.transform_point3(p), names)
    }
}

