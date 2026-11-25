//! Orbit camera: yaw/pitch around a target, perspective or orthographic.

use crate::scene::Aabb;
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewPreset {
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
    Iso,
}

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Radians around the up axis.
    pub yaw: f32,
    /// Radians above the horizon.
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    pub ortho: bool,
    pub z_up: bool,
    /// Scene radius, used for clip planes and speed.
    pub scene_radius: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: 35f32.to_radians(),
            pitch: 25f32.to_radians(),
            distance: 3.0,
            fov_y: 45f32.to_radians(),
            ortho: false,
            z_up: false,
            scene_radius: 1.0,
        }
    }
}

impl OrbitCamera {
    pub fn up(&self) -> Vec3 {
        if self.z_up { Vec3::Z } else { Vec3::Y }
    }

    /// Map a vector expressed in the canonical Y-up frame into world space.
    fn to_world(&self, v: Vec3) -> Vec3 {
        if self.z_up { Vec3::new(v.x, -v.z, v.y) } else { v }
    }

    /// Unit vector from target towards the eye.
    pub fn back_dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.to_world(Vec3::new(sy * cp, sp, cy * cp))
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.back_dir() * self.distance
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye(), self.target, self.up())
    }

    /// Camera basis in world space: (right, up, forward).
    pub fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let fwd = -self.back_dir();
        let right = fwd.cross(self.up()).normalize_or(Vec3::X);
        let up = right.cross(fwd);
        (right, up, fwd)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        let r = self.scene_radius.max(1e-6);
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
        let far = self.distance + r * 4.0;
        if self.ortho {
            let h = self.distance * (self.fov_y * 0.5).tan();
            glam::camera::rh::proj::directx::orthographic(-h * aspect, h * aspect, -h, h, -far, far)
        } else {
            let near = (self.distance - r * 2.0).max(r * 0.001).max(far * 1e-5);
            glam::camera::rh::proj::directx::perspective(self.fov_y, aspect, near, far)
        }
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-89.9f32.to_radians(), 89.9f32.to_radians());
    }

    /// Pan by screen pixels; `viewport_h` in the same pixels.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        let k = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_h.max(1.0);
        let (right, up, _) = self.basis();
        self.target += (-right * dx + up * dy) * k;
    }

    /// Positive delta zooms in.
    pub fn zoom(&mut self, delta: f32) {
        let min = self.scene_radius * 1e-3;
        self.distance = (self.distance * (-delta * 0.0015).exp()).clamp(min, self.scene_radius * 1e3);
    }

    pub fn frame(&mut self, b: &Aabb) {
        self.target = b.center();
        self.scene_radius = b.radius();
        self.distance = self.scene_radius / (self.fov_y * 0.5).sin() * 1.05;
    }

    pub fn set_preset(&mut self, p: ViewPreset) {
        let d = 89.9f32.to_radians();
        let (yaw, pitch) = match p {
            ViewPreset::Front => (0.0, 0.0),
            ViewPreset::Back => (180f32.to_radians(), 0.0),
            ViewPreset::Right => (90f32.to_radians(), 0.0),
            ViewPreset::Left => (-90f32.to_radians(), 0.0),
            ViewPreset::Top => (0.0, d),
            ViewPreset::Bottom => (0.0, -d),
            ViewPreset::Iso => (35f32.to_radians(), 25f32.to_radians()),
        };
        self.yaw = yaw;
        self.pitch = pitch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_targets_center_and_is_finite() {
        let mut c = OrbitCamera::default();
        let mut b = Aabb::empty();
        b.extend(Vec3::new(2.0, 2.0, 2.0)); // zero-size box
        c.frame(&b);
        assert_eq!(c.target, Vec3::new(2.0, 2.0, 2.0));
        let vp = c.view_proj(1.5);
        assert!(vp.is_finite());
        let p = vp.project_point3(c.target);
        assert!(p.x.abs() < 1e-4 && p.y.abs() < 1e-4 && (0.0..=1.0).contains(&p.z));
    }

    #[test]
    fn ortho_and_z_up_are_finite() {
        let mut c = OrbitCamera { ortho: true, z_up: true, ..Default::default() };
        c.set_preset(ViewPreset::Top);
        assert!(c.view_proj(1.0).is_finite());
        assert!(c.back_dir().z > 0.99);
    }

    #[test]
    fn empty_bounds_do_not_nan() {
        let mut c = OrbitCamera::default();
        c.frame(&Aabb::empty());
        assert!(c.view_proj(1.0).is_finite());
    }
}
