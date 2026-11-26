//! Viewport tools: first-person fly, distance and angle measuring, section slicing.

use crate::i18n::tr;
use crate::scene::Aabb;
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    /// Orbit / pan / zoom around the model.
    #[default]
    Orbit,
    /// First-person: WASD + mouse look.
    Fly,
    /// Ruler: two points.
    Distance,
    /// Protractor: three points, the angle is at the middle one.
    Angle,
}

impl Tool {
    /// Points one measurement takes (0 for tools that do not measure).
    pub fn points_needed(self) -> usize {
        match self {
            Tool::Distance => 2,
            Tool::Angle => 3,
            Tool::Orbit | Tool::Fly => 0,
        }
    }

    pub fn measuring(self) -> bool {
        self.points_needed() > 0
    }
}

/// A finished measurement: two points (distance) or three (angle at the middle point).
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    pub points: Vec<Vec3>,
}

impl Measure {
    pub fn is_angle(&self) -> bool {
        self.points.len() == 3
    }

    /// Distance between the two points, or the summed length of both arms of an angle.
    pub fn length(&self) -> f32 {
        self.points.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    pub fn angle_deg(&self) -> f32 {
        match self.points.as_slice() {
            [a, b, c] => angle_deg(*a, *b, *c),
            _ => 0.0,
        }
    }
}

/// Angle at `vertex` between the rays towards `a` and `c`, in degrees.
pub fn angle_deg(a: Vec3, vertex: Vec3, c: Vec3) -> f32 {
    let u = (a - vertex).normalize_or_zero();
    let v = (c - vertex).normalize_or_zero();
    if u == Vec3::ZERO || v == Vec3::ZERO {
        return 0.0;
    }
    u.dot(v).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Section plane across the model bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub enabled: bool,
    /// 0 = X, 1 = Y, 2 = Z.
    pub axis: usize,
    /// Plane position across the bounds: 0 = min side, 1 = max side.
    pub t: f32,
    /// Keep the far (max) side instead of the near one.
    pub flip: bool,
}

impl Default for Slice {
    fn default() -> Self {
        Self { enabled: false, axis: 0, t: 0.5, flip: false }
    }
}

impl Slice {
    pub fn position(&self, b: &Aabb) -> f32 {
        let lo = b.min[self.axis];
        lo + (b.max[self.axis] - lo) * self.t.clamp(0.0, 1.0)
    }

    /// Plane (n, d) for the shader: geometry with dot(p, n) > d is cut away.
    pub fn plane(&self, b: &Aabb) -> Option<[f32; 4]> {
        if !self.enabled || !b.is_valid() {
            return None;
        }
        let mut n = [0.0f32; 3];
        n[self.axis] = 1.0;
        let pos = self.position(b);
        Some(if self.flip { [-n[0], -n[1], -n[2], -pos] } else { [n[0], n[1], n[2], pos] })
    }

    /// Corners of the cut rectangle across the bounds (for drawing its outline).
    pub fn outline(&self, b: &Aabb) -> [Vec3; 4] {
        let (i, j, k) = (self.axis, (self.axis + 1) % 3, (self.axis + 2) % 3);
        let pos = self.position(b);
        let corner = |a: f32, c: f32| {
            let mut v = Vec3::ZERO;
            v[i] = pos;
            v[j] = a;
            v[k] = c;
            v
        };
        [corner(b.min[j], b.min[k]), corner(b.max[j], b.min[k]), corner(b.max[j], b.max[k]), corner(b.min[j], b.max[k])]
    }
}

/// Unit choices offered when the file does not say how big one unit is:
/// (metres per unit, Russian name, English name).
pub const UNIT_CHOICES: [(f64, &str, &str); 5] =
    [(0.001, "мм", "mm"), (0.01, "см", "cm"), (1.0, "м", "m"), (0.0254, "дюйм", "inch"), (0.3048, "фут", "ft")];

/// Name of a file unit for display ("мм", "0.5 m"...).
pub fn unit_name(metres: f64) -> String {
    UNIT_CHOICES
        .iter()
        .find(|(m, _, _)| ((m - metres) / m).abs() < 1e-6)
        .map(|(_, ru, en)| tr(ru, en).to_string())
        .unwrap_or_else(|| format!("{metres} {}", tr("м", "m")))
}

fn sig(v: f64) -> String {
    let a = v.abs();
    if a >= 1000.0 {
        format!("{v:.0}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else if a >= 10.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

/// Formats a length given in model units. With a known unit size the value is shown in
/// mm / cm / m / km, whichever reads best; otherwise in raw model units.
pub fn format_length(v: f32, unit_m: Option<f64>) -> String {
    let v = v as f64;
    match unit_m {
        None => format!("{} {}", sig(v), tr("ед.", "units")),
        Some(u) => {
            let m = v * u;
            let a = m.abs();
            let (val, name) = if a >= 1000.0 {
                (m / 1000.0, tr("км", "km"))
            } else if a >= 1.0 {
                (m, tr("м", "m"))
            } else if a >= 0.01 {
                (m * 100.0, tr("см", "cm"))
            } else {
                (m * 1000.0, tr("мм", "mm"))
            };
            format!("{} {name}", sig(val))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_angle_and_straight_line() {
        assert!((angle_deg(Vec3::X, Vec3::ZERO, Vec3::Y) - 90.0).abs() < 1e-4);
        assert!((angle_deg(Vec3::X, Vec3::ZERO, -Vec3::X) - 180.0).abs() < 1e-3);
        assert_eq!(angle_deg(Vec3::ZERO, Vec3::ZERO, Vec3::Y), 0.0);
        let m = Measure { points: vec![Vec3::new(1.0, 0.0, 0.0), Vec3::ZERO, Vec3::new(1.0, 1.0, 0.0)] };
        assert!(m.is_angle() && (m.angle_deg() - 45.0).abs() < 1e-4);
    }

    #[test]
    fn distance_is_euclidean() {
        let m = Measure { points: vec![Vec3::ZERO, Vec3::new(3.0, 4.0, 0.0)] };
        assert!((m.length() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn slice_plane_keeps_the_low_side() {
        let b = Aabb { min: Vec3::new(-1.0, 0.0, 0.0), max: Vec3::new(3.0, 2.0, 2.0) };
        let s = Slice { enabled: true, axis: 0, t: 0.25, flip: false };
        assert_eq!(s.plane(&b), Some([1.0, 0.0, 0.0, 0.0]));
        let f = Slice { flip: true, ..s };
        assert_eq!(f.plane(&b), Some([-1.0, 0.0, 0.0, -0.0]));
        assert_eq!(Slice::default().plane(&b), None);
        let o = s.outline(&b);
        assert!(o.iter().all(|p| p.x == 0.0));
    }

    #[test]
    fn lengths_pick_readable_units() {
        assert_eq!(format_length(2.5, Some(1.0)), format!("2.500 {}", tr("м", "m")));
        assert_eq!(format_length(12.0, Some(0.001)), format!("1.200 {}", tr("см", "cm")));
        assert_eq!(format_length(0.004, Some(1.0)), format!("4.000 {}", tr("мм", "mm")));
        assert_eq!(format_length(1500.0, Some(1.0)), format!("1.500 {}", tr("км", "km")));
        assert_eq!(format_length(7.0, None), format!("7.000 {}", tr("ед.", "units")));
        assert_eq!(unit_name(0.001), tr("мм", "mm"));
    }
}
