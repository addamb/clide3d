//! Orbit camera: it circles a target point, like Maya's perspective view.

use glam::{Mat4, Vec2, Vec3};

/// The closest the orbit camera gets to its target (1 mm).
const MIN_DISTANCE: f32 = 0.001;

pub struct OrbitCamera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: 0.75,
            pitch: 0.45,
            distance: 12.0,
            fov_y: 45f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let dir = Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        );
        self.target + dir * self.distance
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// wgpu uses 0..1 depth, which is what `perspective_rh` produces.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        // The near plane follows the distance, so close-ups do not clip.
        let near = (self.distance * 0.002).clamp(0.001, 0.1);
        Mat4::perspective_rh(self.fov_y, aspect, near, 2000.0f32.max(self.distance * 50.0))
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.55, 1.55);
    }

    /// Moves the target so the scene follows the mouse 1:1 at the target depth.
    pub fn pan(&mut self, delta: Vec2, viewport_height: f32) {
        let world_per_px = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height;
        let view = self.view();
        let right = view.row(0).truncate();
        let up = view.row(1).truncate();
        self.target += (-right * delta.x + up * delta.y) * world_per_px;
    }

    /// Positive moves closer.
    pub fn dolly(&mut self, amount: f32) {
        self.distance = (self.distance * (-amount).exp()).clamp(MIN_DISTANCE, 1000.0);
    }

    /// Zooms toward `point` (what is under the mouse): the eye moves along
    /// the line to it and `point` stays at the same place on screen, so
    /// zooming never gets stuck at the old target. Positive moves closer.
    pub fn dolly_toward(&mut self, point: Vec3, amount: f32) {
        let scale = (-amount).exp();
        let distance = self.distance * scale;
        // At the limits it stops, so the point never slides off the mouse.
        if !(MIN_DISTANCE..=1000.0).contains(&distance) {
            return;
        }
        self.target = point + (self.target - point) * scale;
        self.distance = distance;
    }

    /// Turns to look at `point` from where the camera is now, so orbiting
    /// then circles it.
    pub fn look_at(&mut self, point: Vec3) {
        let offset = self.eye() - point;
        let distance = offset.length();
        if distance < 1e-4 {
            return;
        }
        let dir = offset / distance;
        self.target = point;
        self.distance = distance.clamp(MIN_DISTANCE, 1000.0);
        self.pitch = dir.y.clamp(-1.0, 1.0).asin().clamp(-1.55, 1.55);
        self.yaw = dir.x.atan2(dir.z);
    }

    pub fn frame(&mut self, center: Vec3, radius: f32) {
        self.target = center;
        self.distance = (radius / (self.fov_y * 0.5).sin()).max(MIN_DISTANCE * 10.0);
    }
}

/// The four standard Maya views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewKind {
    Persp,
    Top,
    Front,
    Side,
}

impl ViewKind {
    /// Maya's four-view order: top-left, top-right, bottom-left, bottom-right.
    pub const FOUR: [ViewKind; 4] = [ViewKind::Top, ViewKind::Persp, ViewKind::Front, ViewKind::Side];

    pub fn name(self) -> &'static str {
        match self {
            ViewKind::Persp => "persp",
            ViewKind::Top => "top",
            ViewKind::Front => "front",
            ViewKind::Side => "side",
        }
    }

    pub fn from_name(name: &str) -> Option<ViewKind> {
        Self::FOUR.into_iter().find(|v| v.name() == name)
    }

    /// GPU buffer slot (see `render::SLOTS`).
    pub fn slot(self) -> usize {
        self as usize
    }

    /// Direction from the target toward an orthographic camera, and its up vector.
    fn ortho_axes(self) -> (Vec3, Vec3) {
        match self {
            ViewKind::Top => (Vec3::Y, Vec3::NEG_Z),
            ViewKind::Front => (Vec3::Z, Vec3::Y),
            // Looking down -X puts -Z on the right, as in Maya's side view.
            ViewKind::Side | ViewKind::Persp => (Vec3::X, Vec3::Y),
        }
    }

    /// Lays the XZ ground grid in this view's plane.
    pub fn grid_model(self) -> Mat4 {
        match self {
            ViewKind::Persp | ViewKind::Top => Mat4::IDENTITY,
            ViewKind::Front => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
            ViewKind::Side => Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
        }
    }
}

/// Orthographic camera: no tumbling, only pan and zoom.
pub struct OrthoCamera {
    pub center: Vec3,
    /// Half the visible height in world units (zoom).
    pub half_height: f32,
}

impl Default for OrthoCamera {
    fn default() -> Self {
        Self { center: Vec3::ZERO, half_height: 6.0 }
    }
}

const ORTHO_DISTANCE: f32 = 1000.0;

/// Every view's camera. Persp orbits; top/front/side are orthographic.
#[derive(Default)]
pub struct Cameras {
    pub persp: OrbitCamera,
    pub top: OrthoCamera,
    pub front: OrthoCamera,
    pub side: OrthoCamera,
}

impl Cameras {
    fn ortho(&self, kind: ViewKind) -> &OrthoCamera {
        match kind {
            ViewKind::Top => &self.top,
            ViewKind::Front => &self.front,
            _ => &self.side,
        }
    }

    fn ortho_mut(&mut self, kind: ViewKind) -> &mut OrthoCamera {
        match kind {
            ViewKind::Top => &mut self.top,
            ViewKind::Front => &mut self.front,
            _ => &mut self.side,
        }
    }

    pub fn eye(&self, kind: ViewKind) -> Vec3 {
        match kind {
            ViewKind::Persp => self.persp.eye(),
            _ => self.ortho(kind).center + kind.ortho_axes().0 * ORTHO_DISTANCE,
        }
    }

    /// The point a view looks at.
    pub fn target(&self, kind: ViewKind) -> Vec3 {
        match kind {
            ViewKind::Persp => self.persp.target,
            _ => self.ortho(kind).center,
        }
    }

    pub fn view(&self, kind: ViewKind) -> Mat4 {
        match kind {
            ViewKind::Persp => self.persp.view(),
            _ => {
                let c = self.ortho(kind).center;
                Mat4::look_at_rh(self.eye(kind), c, kind.ortho_axes().1)
            }
        }
    }

    pub fn proj(&self, kind: ViewKind, aspect: f32) -> Mat4 {
        match kind {
            ViewKind::Persp => self.persp.proj(aspect),
            _ => {
                let h = self.ortho(kind).half_height;
                Mat4::orthographic_rh(-h * aspect, h * aspect, -h, h, 1.0, ORTHO_DISTANCE * 2.0)
            }
        }
    }

    /// Tumble; orthographic views ignore it (Maya locks them by default).
    pub fn orbit(&mut self, kind: ViewKind, delta: Vec2) {
        if kind == ViewKind::Persp {
            self.persp.orbit(delta);
        }
    }

    pub fn pan(&mut self, kind: ViewKind, delta: Vec2, viewport_height: f32) {
        if kind == ViewKind::Persp {
            return self.persp.pan(delta, viewport_height);
        }
        let view = self.view(kind);
        let cam = self.ortho_mut(kind);
        let world_per_px = 2.0 * cam.half_height / viewport_height;
        cam.center += (-view.row(0).truncate() * delta.x + view.row(1).truncate() * delta.y) * world_per_px;
    }

    /// Positive moves closer / zooms in.
    pub fn dolly(&mut self, kind: ViewKind, amount: f32) {
        if kind == ViewKind::Persp {
            return self.persp.dolly(amount);
        }
        let cam = self.ortho_mut(kind);
        cam.half_height = (cam.half_height * (-amount).exp()).clamp(0.001, 1000.0);
    }

    /// Zooms toward `point`, keeping it under the mouse. Positive zooms in.
    pub fn dolly_toward(&mut self, kind: ViewKind, point: Vec3, amount: f32) {
        if kind == ViewKind::Persp {
            return self.persp.dolly_toward(point, amount);
        }
        let scale = (-amount).exp();
        let cam = self.ortho_mut(kind);
        let half = cam.half_height * scale;
        if !(0.001..=1000.0).contains(&half) {
            return;
        }
        let (axis, _) = kind.ortho_axes();
        // Only the in-plane part of the point matters for an orthographic view.
        let along = |v: Vec3| v - axis * v.dot(axis);
        cam.center = along(point) + along(cam.center - point) * scale + axis * cam.center.dot(axis);
        cam.half_height = half;
    }

    /// Persp turns to look at `point`; orthographic views center on it.
    pub fn look_at(&mut self, kind: ViewKind, point: Vec3) {
        if kind == ViewKind::Persp {
            return self.persp.look_at(point);
        }
        let (axis, _) = kind.ortho_axes();
        let cam = self.ortho_mut(kind);
        cam.center = point - axis * point.dot(axis) + axis * cam.center.dot(axis);
    }

    pub fn frame(&mut self, kind: ViewKind, center: Vec3, radius: f32) {
        if kind == ViewKind::Persp {
            return self.persp.frame(center, radius);
        }
        let cam = self.ortho_mut(kind);
        cam.center = center;
        cam.half_height = (radius * 1.2).max(0.01);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cam: &OrbitCamera, p: Vec3) -> Vec3 {
        (cam.proj(1.5) * cam.view()).project_point3(p)
    }

    #[test]
    fn zoom_toward_a_point_keeps_it_on_screen_and_gets_close() {
        let mut cam = OrbitCamera::default();
        let point = Vec3::new(2.0, 1.0, -0.5);
        let before = screen(&cam, point);
        for _ in 0..150 {
            cam.dolly_toward(point, 0.05);
        }
        let after = screen(&cam, point);
        assert!((before.truncate() - after.truncate()).length() < 1e-3, "{before} -> {after}");
        // It reaches the point instead of stalling at the old target.
        assert!(cam.eye().distance(point) < 0.05, "{}", cam.eye().distance(point));
    }

    #[test]
    fn look_at_keeps_the_eye_and_centers_the_point() {
        let mut cam = OrbitCamera::default();
        let eye = cam.eye();
        let point = Vec3::new(-1.0, 0.5, 2.0);
        cam.look_at(point);
        assert!(cam.eye().distance(eye) < 1e-3);
        assert!(screen(&cam, point).truncate().length() < 1e-3);
    }

    #[test]
    fn ortho_zoom_keeps_the_point_in_place() {
        let mut cams = Cameras::default();
        let point = Vec3::new(1.0, 2.0, 0.0);
        let at = |c: &Cameras| (c.proj(ViewKind::Front, 1.0) * c.view(ViewKind::Front)).project_point3(point).truncate();
        let before = at(&cams);
        cams.dolly_toward(ViewKind::Front, point, 0.7);
        assert!((at(&cams) - before).length() < 1e-4);
        assert!(cams.front.half_height < 6.0);
    }
}
