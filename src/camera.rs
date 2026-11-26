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
    /// Centre of the scene bounds; first-person mode sizes the far plane from it.
    pub scene_center: Vec3,
    /// First-person mode: the camera turns around the eye and keeps a short near plane.
    pub fly: bool,
    anim: Option<ViewAnim>,
}

/// Smooth turn to a new view direction (gizmo clicks).
#[derive(Clone, Copy, Debug)]
struct ViewAnim {
    from: (f32, f32),
    to: (f32, f32),
    t: f32,
}

const ANIM_SECONDS: f32 = 0.25;

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
            scene_center: Vec3::ZERO,
            fly: false,
            anim: None,
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
        if self.fly {
            // Walking through a location: geometry right in front of the eye must not be
            // clipped, and the whole scene must stay inside the far plane.
            let far = (self.eye() - self.scene_center).length() + r * 2.5;
            let near = (r * 2e-4).max(far * 1e-6);
            return glam::camera::rh::proj::directx::perspective(self.fov_y, aspect, near, far);
        }
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
        self.anim = None;
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

    /// First-person look: turn the view around the eye.
    pub fn look(&mut self, dx: f32, dy: f32) {
        let eye = self.eye();
        self.orbit(dx, dy);
        self.target = eye - self.back_dir() * self.distance;
    }

    /// First-person move in world units: `forward` runs along the ground in the view
    /// direction, `up` along the world up axis.
    pub fn fly_move(&mut self, right: f32, up: f32, forward: f32) {
        let (r, _, _) = self.basis();
        let wup = self.up();
        let fwd_ground = wup.cross(r).normalize_or(r.cross(Vec3::X));
        self.target += r * right + wup * up + fwd_ground * forward;
    }

    /// Yaw/pitch that put the eye on the `dir` side of the target (world space).
    pub fn angles_for_back_dir(&self, dir: Vec3) -> (f32, f32) {
        let d = dir.normalize_or(Vec3::Z);
        // Inverse of `to_world`.
        let c = if self.z_up { Vec3::new(d.x, d.z, -d.y) } else { d };
        let limit = 89.9f32.to_radians();
        let pitch = c.y.clamp(-1.0, 1.0).asin().clamp(-limit, limit);
        let yaw = if c.x.abs() + c.z.abs() < 1e-4 { 0.0 } else { c.x.atan2(c.z) };
        (yaw, pitch)
    }

    /// Turn smoothly to the given angles.
    pub fn animate_to(&mut self, yaw: f32, pitch: f32) {
        let tau = std::f32::consts::TAU;
        let dy = (yaw - self.yaw).rem_euclid(tau);
        let dy = if dy > std::f32::consts::PI { dy - tau } else { dy };
        self.anim = Some(ViewAnim { from: (self.yaw, self.pitch), to: (self.yaw + dy, pitch), t: 0.0 });
    }

    /// Advances a running view animation; returns true while it is running.
    pub fn update(&mut self, dt: f32) -> bool {
        let Some(mut a) = self.anim else { return false };
        a.t = (a.t + dt / ANIM_SECONDS).min(1.0);
        let s = a.t * a.t * (3.0 - 2.0 * a.t);
        let eye = self.eye();
        self.yaw = a.from.0 + (a.to.0 - a.from.0) * s;
        self.pitch = a.from.1 + (a.to.1 - a.from.1) * s;
        if self.fly {
            self.target = eye - self.back_dir() * self.distance;
        }
        self.anim = (a.t < 1.0).then_some(a);
        true
    }

    pub fn frame(&mut self, b: &Aabb) {
        self.scene_center = b.center();
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
        self.anim = None;
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
    fn axis_angles_match_presets() {
        for z_up in [false, true] {
            let mut c = OrbitCamera { z_up, ..Default::default() };
            for (dir, preset) in [(Vec3::X, ViewPreset::Right), (-Vec3::X, ViewPreset::Left)] {
                let (yaw, pitch) = c.angles_for_back_dir(dir);
                c.yaw = yaw;
                c.pitch = pitch;
                assert!((c.back_dir() - dir).length() < 1e-4, "{dir} z_up={z_up}");
                let mut p = c.clone();
                p.set_preset(preset);
                assert!((p.back_dir() - c.back_dir()).length() < 1e-4);
            }
            let up = c.up();
            let (yaw, pitch) = c.angles_for_back_dir(up);
            c.yaw = yaw;
            c.pitch = pitch;
            assert!(c.back_dir().dot(up) > 0.999);
        }
    }

    #[test]
    fn look_keeps_the_eye_and_fly_moves_on_the_ground() {
        let mut c = OrbitCamera { pitch: 0.4, ..Default::default() };
        let eye = c.eye();
        c.look(40.0, -25.0);
        assert!((c.eye() - eye).length() < 1e-4);
        let before = c.eye();
        c.fly_move(0.0, 0.0, 2.0);
        let moved = c.eye() - before;
        assert!(moved.y.abs() < 1e-5 && (moved.length() - 2.0).abs() < 1e-4);
    }

    #[test]
    fn animation_ends_on_target_by_the_short_way() {
        let mut c = OrbitCamera { yaw: 3.0, ..Default::default() };
        c.animate_to(-3.0, 0.2);
        let mut steps = 0;
        while c.update(0.05) {
            assert!(c.yaw >= 3.0 - 1e-4, "went the long way: {}", c.yaw);
            steps += 1;
            assert!(steps < 100);
        }
        assert!(
            ((c.yaw - (-3.0)).rem_euclid(std::f32::consts::TAU))
                .min(std::f32::consts::TAU - (c.yaw - (-3.0)).rem_euclid(std::f32::consts::TAU))
                < 1e-4
        );
        assert!((c.pitch - 0.2).abs() < 1e-5);
    }

    #[test]
    fn fly_projection_is_finite_inside_a_big_scene() {
        let mut c = OrbitCamera { fly: true, scene_radius: 800.0, distance: 2000.0, ..Default::default() };
        c.target = Vec3::new(10.0, 2.0, 10.0);
        assert!(c.view_proj(1.7).is_finite());
    }

    #[test]
    fn empty_bounds_do_not_nan() {
        let mut c = OrbitCamera::default();
        c.frame(&Aabb::empty());
        assert!(c.view_proj(1.0).is_finite());
    }
}
